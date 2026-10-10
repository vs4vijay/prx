//! `prx-menubar` — the PRX macOS app (menu bar item + app window). Bundled
//! as `PRX.app/Contents/MacOS/PRX` (design A1; `scripts/bundle-macos.sh`),
//! next to the `prx` CLI/daemon it supervises ([`supervisor`], design A3).
//! Single instance with reopen ([`instance`], design A2): a launch without
//! `--background` opens the window; `--background` (the login item) shows
//! only the menu bar item.
//!
//! Runs `NSApplication` directly (accessory policy: no Dock icon) with a
//! `tray-icon` status-bar icon (multi-scale, appearance-aware colored images —
//! [`icon`]) and a `muda`
//! menu. A background poller ([`poller`]) re-discovers the daemon and fetches
//! `GET /status` (+ the recent `GET /history` entries) every ~10 s (plus a
//! nudge on tray clicks, manual refreshes, and completed actions); each
//! outcome is turned into a pure [`model::MenuModel`] which is logged,
//! rendered 1:1 into a fresh `muda` menu (`tray.set_menu`), and reflected on
//! the icon (attention count / reviewing / stopped / idle / offline variants).
//! Design D2: rebuild-per-poll, no incremental menu diffing. Group
//! pagination clicks (`page:more|…` / `page:less|…`, design D7) stay local:
//! they update the shell's [`model::PaginationState`] and rebuild the menu
//! from the last snapshot without a daemon round-trip.
//!
//! Cross-thread messages reach the main thread through [`mainloop`]'s
//! `CFRunLoopSource`, registered in `kCFRunLoopDefaultMode` only. That is the
//! fix for the menu that used to close itself: winit's run-loop machinery
//! (proxy source + control-flow observers) lived in `kCFRunLoopCommonModes`,
//! so a poll outcome landing while the NSMenu was open woke the loop inside
//! AppKit's menu tracking and dismissed the menu (rust-windowing/tao#1324 —
//! every click nudges a poll, so the menu died within moments of opening).
//! Now a poll outcome that arrives mid-open waits out the menu and applies
//! right after it closes; the tray click drains the queue synchronously
//! BEFORE tray-icon shows the menu, so every open presents the freshest
//! polled model.
//!
//! Menu clicks are dispatched through [`actions::ActionController`] (task
//! 4.4/4.5: every per-PR and footer action round-trips through the daemon's
//! IPC API; never `gh`, never a direct GitHub call). New history events whose
//! configured tier banners raise native macOS notifications from the poller
//! thread ([`banners`], task 4.6 — the daemon re-banners its own events, so
//! one event can appear twice; see the README). `--self-test <id>` runs the
//! identical poll loop + action handler headlessly for verification.

mod actions;
mod banners;
mod browser;
mod clipboard;
mod icon;
mod instance;
mod mainloop;
mod model;
mod notify_center;
mod poller;
mod review_panel;
mod settings;
mod shellenv;
mod supervisor;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::error::Error;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{
    CheckMenuItem, ContextMenu, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol;
use objc2::{msg_send, sel, MainThreadMarker};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSColor, NSEventModifierFlags, NSImage,
    NSImageSymbolConfiguration, NSMenu, NSMenuItem,
};
use objc2_foundation::NSString;
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use actions::{ActionController, PageAction, ReviewAction};
use icon::{Appearance, IconState};
use mainloop::{MainThreadSender, Pending};
use model::{
    ItemKind, MenuModel, ModelItem, PaginationState, Shortcut, Symbol, Tone, QUIT_ID, QUIT_KEEP_ID,
    SHOW_LOG_ID, START_DAEMON_ID,
};
use poller::{PollOutcome, PollerCommand, PollerHandle, Snapshot};
use supervisor::{DaemonState, Supervisor};

/// Cross-thread messages applied on the main thread.
#[derive(Debug, Clone)]
enum Msg {
    /// A poll finished (success or daemon unreachable).
    Poll(PollOutcome),
    /// An action finished; the payload is the message line for the menu.
    Action(String),
    /// A Show more / Show less click (applied after the menu closes).
    Page(PageAction),
    /// "Review with prompt…" for this PR key (`Some`) or the footer's
    /// "Review PR…" (`None`): open the review panel (after the menu closes)
    /// and submit its request.
    ReviewPanel(Option<String>),
    /// Footer "Open PRX" / "Settings…", a relaunch, or a Dock reopen: show
    /// the window on this page (after the menu closes).
    OpenWindow(String),
    /// The daemon supervisor changed state (drives the offline menu).
    Supervisor(DaemonState),
    /// Approve / Block (audit P1): ask first (after the menu closes), then
    /// dispatch the action id.
    Confirm(String, actions::Confirmation),
}

/// What quitting does to a daemon the app started (task 6.5): "Quit PRX"
/// stops it, "Quit PRX (keep reviewing)" leaves it running. An external
/// daemon is never stopped either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuitMode {
    StopOwned,
    KeepDaemon,
}

/// `Rc<RefCell<_>>` for the muda/tray-icon event handlers. Their
/// `set_event_handler` APIs demand `Send + Sync` (a cross-platform surface),
/// but on macOS both crates deliver every event on the main thread (AppKit
/// target/action from the status item) — so the shared state is only ever
/// touched from the main thread, exactly the invariant `Rc<RefCell<_>>`
/// wants. Nothing here crosses threads despite the bounds.
struct MainCell<T>(Rc<RefCell<T>>);

unsafe impl<T> Send for MainCell<T> {}
unsafe impl<T> Sync for MainCell<T> {}

impl<T> MainCell<T> {
    /// Run `f` with unique access to the inner state (main thread only).
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        f(&mut self.0.borrow_mut())
    }
}

/// Application state driven by the main run loop. Owned by an
/// `Rc<RefCell<_>>` shared with the drain source and the tray/menu event
/// handlers — all of which fire on the main thread, never re-entrantly
/// (the drain source is default-mode-only; menu actions spawn threads).
struct App {
    tray: Option<TrayIcon>,
    /// The currently rendered menu model (last poll outcome).
    model: MenuModel,
    /// The last action outcome, rendered as a disabled info line until the
    /// next action replaces it.
    message: Option<String>,
    /// The icon currently on the tray and the appearance it was rendered
    /// for (regenerate only when either changes).
    icon_state: IconState,
    icon_appearance: Appearance,
    /// The status item's count title and tooltip currently shown (set only
    /// when they change).
    icon_title: Option<String>,
    icon_tooltip: String,
    /// The last successful poll (pagination clicks rebuild from it).
    snapshot: Option<Snapshot>,
    /// Pages shown per lifecycle group (design D7).
    pagination: PaginationState,
    poller: PollerHandle,
    actions: ActionController,
    /// The main-loop queue, also drained synchronously on tray clicks.
    pending: Pending<Msg>,
    /// Enqueues local follow-ups (pagination) onto the main loop so they
    /// apply after the menu closes, like every other rebuild.
    sender: MainThreadSender<Msg>,
    /// The in-process settings window host (task 8.6; window created on
    /// first open).
    settings: settings::Settings,
    /// The daemon supervisor (design A3; `None` in self-test runs).
    supervisor: Option<Supervisor>,
    /// The supervisor's last reported state (shapes the offline menu).
    daemon: DaemonState,
    /// Read by the terminate hook: whether Quit stops an owned daemon.
    quit_mode: Rc<Cell<QuitMode>>,
}

impl App {
    fn new(
        poller: PollerHandle,
        actions: ActionController,
        pending: Pending<Msg>,
        sender: MainThreadSender<Msg>,
        settings: settings::Settings,
        supervisor: Option<Supervisor>,
        quit_mode: Rc<Cell<QuitMode>>,
    ) -> Self {
        Self {
            tray: None,
            // No daemon answer yet: start offline; the first poll (immediate)
            // corrects it within moments.
            model: model::offline_model(),
            message: None,
            icon_state: IconState::Offline,
            icon_appearance: Appearance::Light,
            icon_title: None,
            icon_tooltip: String::new(),
            snapshot: None,
            pagination: PaginationState::default(),
            poller,
            actions,
            pending,
            sender,
            settings,
            supervisor,
            daemon: DaemonState::Unmanaged,
            quit_mode,
        }
    }

    /// The current model with the pending action message applied.
    fn current_model(&self) -> MenuModel {
        match &self.message {
            Some(message) => self.model.clone().with_message(message.clone()),
            None => self.model.clone(),
        }
    }

    /// Swap in a freshly rendered menu (model may have changed without a
    /// poll: action messages rebuild too). Never called while a menu is
    /// open: the drain source is default-mode-only, and the synchronous
    /// drain on a tray click runs before tray-icon opens the menu.
    fn swap_menu(&mut self) {
        let Some(tray) = &self.tray else { return };
        match render_menu(&self.current_model()) {
            Ok(menu) => tray.set_menu(Some(Box::new(menu))),
            Err(error) => eprintln!("prx-menubar: cannot rebuild menu: {error}"),
        }
    }

    /// Builds the status-bar icon and its first menu. Must run on the main
    /// thread after `NSApplication` exists (`sharedApplication` is the setup
    /// the old winit `resumed` hook was waiting for).
    fn build_tray(&mut self) -> Result<(), Box<dyn Error>> {
        let menu = render_menu(&self.current_model())?;
        let tray = TrayIconBuilder::new()
            .with_id("prx-menubar")
            .with_menu(Box::new(menu))
            .with_tooltip(settings::APP_NAME)
            .with_icon(icon::icon(self.icon_state, self.icon_appearance))
            .build()?;
        // Swap the 1x bootstrap raster for the multi-scale image.
        self.icon_appearance = icon::appearance_of(&tray);
        icon::apply(&tray, self.icon_state, self.icon_appearance)?;
        self.tray = Some(tray);
        Ok(())
    }

    /// Apply one main-thread message (poll outcome or action outcome).
    fn apply_msg(&mut self, message: Msg) {
        match message {
            Msg::Poll(outcome) => self.apply_poll_outcome(&outcome),
            Msg::Action(line) => self.apply_action_message(line),
            Msg::Page(page) => self.apply_page(page),
            Msg::ReviewPanel(key) => self.ask_review(key),
            Msg::OpenWindow(page) => self.open_window(&page),
            Msg::Supervisor(state) => self.apply_daemon_state(state),
            Msg::Confirm(id, request) => self.confirm_action(&id, &request),
        }
    }

    /// Apply everything queued so far (main thread only). The drain source
    /// calls this on wake; the tray click calls it synchronously before the
    /// menu opens so the open presents the freshest model.
    fn drain_pending(&mut self) {
        let pending = self.pending.clone();
        pending.drain(|message| self.apply_msg(message));
    }

    /// Apply a poll outcome: rebuild the model, log it, swap the menu and
    /// the icon. This is the single place the shell's polled state changes.
    fn apply_poll_outcome(&mut self, outcome: &PollOutcome) {
        self.snapshot = match outcome {
            PollOutcome::Up(snapshot) => Some((**snapshot).clone()),
            PollOutcome::Down(_) => None,
        };
        self.rebuild_model();
        eprintln!("prx-menubar: {}", self.model.dump().trim_end());
        self.update_icon();
        self.swap_menu();
    }

    /// Rebuild the model from the last snapshot (offline without one).
    fn rebuild_model(&mut self) {
        self.model = match &self.snapshot {
            Some(snapshot) => model::build_model(
                &snapshot.status,
                &snapshot.history,
                &mut self.pagination,
                chrono::Utc::now(),
            ),
            None => model::offline_model_for(&self.daemon),
        };
    }

    /// A supervision change: only the offline menu shows it, so rebuild
    /// when no snapshot is live, and nudge a poll (a (re)started daemon
    /// should flip the menu online promptly).
    fn apply_daemon_state(&mut self, state: DaemonState) {
        self.daemon = state;
        if self.snapshot.is_none() {
            self.rebuild_model();
            self.update_icon();
            self.swap_menu();
        }
        self.poller.refresh();
    }

    /// Quit the app; `mode` decides the fate of an owned daemon (applied by
    /// the terminate hook, which every quit path runs through).
    fn quit(&mut self, mode: QuitMode) {
        eprintln!("prx-menubar: quit requested ({mode:?})");
        self.quit_mode.set(mode);
        let mtm = MainThreadMarker::new().expect("menu events arrive on the main thread");
        NSApplication::sharedApplication(mtm).terminate(None);
    }

    /// Offline "Show log": the newest daemon log in Console.
    fn show_log(&mut self) {
        let path = supervisor::log_to_show();
        if !path.exists() {
            self.apply_action_message(format!("no daemon log yet ({})", path.display()));
            return;
        }
        if let Err(error) = std::process::Command::new("/usr/bin/open")
            .args(["-a", "Console"])
            .arg(&path)
            .spawn()
        {
            self.apply_action_message(format!("⚠ cannot open {}: {error}", path.display()));
        }
    }

    /// Re-render the tray icon when the state or the menu bar's appearance
    /// changed (appearance is re-sampled on every poll, so a light/dark
    /// switch is picked up within one poll interval), and update the count
    /// title and tooltip when they changed.
    fn update_icon(&mut self) {
        let Some(tray) = &self.tray else { return };
        let title = icon::count_title(&self.model);
        let tooltip = icon::tooltip(&self.model);
        if (&title, &tooltip) != (&self.icon_title, &self.icon_tooltip) {
            icon::apply_title(tray, title.as_deref(), &tooltip);
            eprintln!("prx-menubar: status title -> {title:?} ({tooltip})");
            self.icon_title = title;
            self.icon_tooltip = tooltip;
        }
        let wanted = IconState::for_model(&self.model);
        let appearance = icon::appearance_of(tray);
        if (wanted, appearance) == (self.icon_state, self.icon_appearance) {
            return;
        }
        match icon::apply(tray, wanted, appearance) {
            Ok(()) => {
                eprintln!("prx-menubar: icon -> {wanted:?} ({appearance:?})");
                self.icon_state = wanted;
                self.icon_appearance = appearance;
            }
            Err(error) => eprintln!("prx-menubar: cannot update icon: {error}"),
        }
    }

    /// A Show more / Show less click: page the group and rebuild the menu
    /// in place from the last snapshot (no daemon round-trip).
    fn apply_page(&mut self, page: PageAction) {
        match &page {
            PageAction::More(group) => self.pagination.more(group),
            PageAction::Less(group) => self.pagination.less(group),
        }
        eprintln!("prx-menubar: pagination {page:?}");
        self.rebuild_model();
        self.swap_menu();
    }

    /// Run the review panel for `key` (or, with `None`, for a typed PR
    /// reference) and submit a non-cancelled result as a forced review with
    /// the options that differ from the defaults (background thread).
    fn ask_review(&mut self, key: Option<String>) {
        let mtm = MainThreadMarker::new().expect("main-loop messages apply on the main thread");
        let spec = panel_spec(self.snapshot.as_ref(), key);
        let label = spec.target.clone().unwrap_or_else(|| "a PR".into());
        match review_panel::ask(mtm, &spec).and_then(|result| result.into_request()) {
            Some((target, overrides)) => self.actions.dispatch_review(&target, overrides),
            None => eprintln!("prx-menubar: review panel cancelled for {label}"),
        }
    }

    /// Ask before a GitHub write; dispatch it only when confirmed.
    fn confirm_action(&mut self, id: &str, request: &actions::Confirmation) {
        let mtm = MainThreadMarker::new().expect("main-loop messages apply on the main thread");
        if review_panel::confirm(mtm, request) {
            self.actions.dispatch(id);
        } else {
            eprintln!("prx-menubar: {id} cancelled");
        }
    }

    /// Show the window on `page` (focus and switch it if open; activate
    /// another process's window if one holds it). Failures become the
    /// message line.
    fn open_window(&mut self, page: &str) {
        match self.settings.open(page) {
            Ok(opened) => eprintln!("prx-menubar: window {opened:?} on {page}"),
            Err(error) => self.apply_action_message(format!("⚠ {error}")),
        }
    }

    /// Show an action outcome as a disabled info line (the refreshed menu
    /// follows via the poller nudge the action already sent).
    fn apply_action_message(&mut self, line: String) {
        eprintln!("prx-menubar: {line}");
        self.message = Some(line);
        self.swap_menu();
    }

    /// A tray click. Only the left-button press opens the menu: drain queued
    /// messages first (so the menu opens with the freshest polled model),
    /// then nudge a refresh whose outcome applies at the next drain — which,
    /// if the menu stays open, is that menu's next open.
    fn handle_tray_event(&mut self, event: TrayIconEvent) {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Down,
            ..
        } = event
        {
            self.drain_pending();
            self.poller.refresh();
        }
    }

    /// Dispatch a menu click. Quit terminates the app, the supervision items
    /// stay local, window shortcuts open the window, and everything else
    /// goes to the action controller — the same ids the model built.
    fn handle_menu_event(&mut self, event: MenuEvent) {
        let id: &str = &event.id.0;
        if id == QUIT_ID {
            self.quit(QuitMode::StopOwned);
        } else if id == QUIT_KEEP_ID {
            self.quit(QuitMode::KeepDaemon);
        } else if id == START_DAEMON_ID {
            match &self.supervisor {
                Some(supervisor) => supervisor.start_daemon(),
                None => self.apply_action_message("⚠ no daemon supervisor in this run".into()),
            }
        } else if id == SHOW_LOG_ID {
            self.show_log();
        } else if let Some(page) = actions::page_action(id) {
            // Applied from the main-loop queue (after the menu closes), like
            // every other menu rebuild.
            self.sender.send(Msg::Page(page));
        } else if let Some(page) = actions::window_page(id) {
            // In-process window (no editor launch), opened from the
            // main-loop queue once the menu has closed.
            self.sender.send(Msg::OpenWindow(page.to_string()));
        } else if let Some(panel) = match actions::review_action(id) {
            Some(ReviewAction::Prompt(key)) => Some(Some(key)),
            Some(ReviewAction::ReviewPr) => Some(None),
            _ => None,
        } {
            // The panel is modal: open it from the main-loop queue once the
            // menu has closed, never inside menu tracking.
            self.sender.send(Msg::ReviewPanel(panel));
        } else if let Some(request) =
            actions::confirmation(id, self.snapshot.as_ref().map(|s| &s.status))
        {
            // Also modal, so also after the menu closes.
            self.sender.send(Msg::Confirm(id.to_string(), request));
        } else {
            let label = self
                .model
                .items()
                .find(|item| item.id == id)
                .map(|item| item.label.as_str())
                .unwrap_or("<stale id>");
            eprintln!("prx-menubar: action click: id={id} label={label}");
            self.actions.dispatch(id);
        }
    }
}

/// The review panel's inputs from the last snapshot: the daemon's engine
/// list and, for a PR, its effective engine (the picker's default).
fn panel_spec(snapshot: Option<&Snapshot>, key: Option<String>) -> review_panel::PanelSpec {
    let status = snapshot.map(|s| &s.status);
    let default_engine = key.as_ref().and_then(|key| {
        status?
            .prs
            .iter()
            .find(|entry| &entry.key == key)?
            .engine
            .clone()
    });
    review_panel::PanelSpec {
        target: key,
        engines: status.map(|s| s.engines.clone()).unwrap_or_default(),
        default_engine,
    }
}

/// Render the pure model 1:1 into a fresh `muda` menu: sections become
/// separator-delimited blocks, leaf items become plain `MenuItem`s with their
/// model id, and items with children become `Submenu`s (per-PR actions,
/// Snooze, History). Dumb by design; all decisions live in the model.
fn render_menu(model: &MenuModel) -> Result<Menu, muda::Error> {
    let menu = Menu::new();
    for (index, section) in model.sections.iter().enumerate() {
        if index > 0 {
            menu.append(&PredefinedMenuItem::separator())?;
        }
        for item in &section.items {
            menu.append(render_item(item)?.as_item())?;
        }
    }
    apply_native_flags(&menu, model);
    Ok(menu)
}

/// Apply what muda can't express to the NSMenuItems it created (the same
/// NSMenu tray-icon shows), by position: top-level items map 1:1 onto the
/// model's sections, with one separator between sections; a submenu's
/// items map 1:1 onto its children.
fn apply_native_flags(menu: &Menu, model: &MenuModel) {
    for_each_native(menu, model, &mut decorate);
}

/// Walk the NSMenuItems muda created for `model`, pairing each with its
/// model item: top-level items map 1:1 onto the model's sections, with one
/// separator between sections; a submenu's items map 1:1 onto its children.
fn for_each_native(
    menu: &Menu,
    model: &MenuModel,
    visit: &mut dyn FnMut(&NSMenu, isize, &ModelItem),
) {
    let pointer = menu.ns_menu();
    if pointer.is_null() {
        return;
    }
    // SAFETY: muda returns its live NSMenu, owned by `menu` (alive for this
    // borrow); main thread only, like every menu render.
    let ns_menu: &NSMenu = unsafe { &*pointer.cast::<NSMenu>() };
    let mut index: isize = 0;
    for (section_index, section) in model.sections.iter().enumerate() {
        if section_index > 0 {
            index += 1; // the separator
        }
        for item in &section.items {
            visit_native(ns_menu, index, item, visit);
            index += 1;
        }
    }
}

fn visit_native(
    menu: &NSMenu,
    index: isize,
    item: &ModelItem,
    visit: &mut dyn FnMut(&NSMenu, isize, &ModelItem),
) {
    visit(menu, index, item);
    if item.children.is_empty() {
        return;
    }
    if let Some(submenu) = menu.itemAtIndex(index).and_then(|native| native.submenu()) {
        for (child_index, child) in item.children.iter().enumerate() {
            visit_native(&submenu, child_index as isize, child, visit);
        }
    }
}

/// One native item:
/// - `alternate`: the item replaces the one above it while ⌥ is held
///   (Quit's "keep reviewing", the troubleshooting shortcuts, the
///   headline's diagnostics). With key equivalents muda already set both
///   masks; without, the base gets an empty mask and the alternate ⌥.
/// - `indent`: `indentationLevel` (the inline Approve).
/// - `symbol`: the SF Symbol image ([`symbol_image`]), marked always
///   visible ([`show_image`]); if it cannot load, the symbol's monochrome
///   text fallback prefixes the title instead.
fn decorate(menu: &NSMenu, index: isize, item: &ModelItem) {
    let Some(native) = menu.itemAtIndex(index) else {
        return;
    };
    if item.alternate && index > 0 {
        if item.shortcut.is_none() {
            if let Some(base) = menu.itemAtIndex(index - 1) {
                base.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
            }
            native.setKeyEquivalentModifierMask(NSEventModifierFlags::Option);
        }
        native.setAlternate(true);
    }
    if item.indent > 0 {
        native.setIndentationLevel(item.indent as isize);
    }
    if let Some(symbol) = item.symbol {
        match symbol_image(symbol) {
            Some(image) => {
                native.setImage(Some(&image));
                show_image(&native);
            }
            None if !symbol.fallback().is_empty() => native.setTitle(&NSString::from_str(
                &format!("{} {}", symbol.fallback(), item.label),
            )),
            None => {}
        }
    }
}

/// `NSMenuItemImageVisibilityVisible`.
const IMAGE_VISIBILITY_VISIBLE: isize = 1;

/// Opt the item's image into display. From macOS 27, AppKit decides
/// whether menu item images show (`preferredImageVisibility`, default
/// Automatic) and typically hides them, so a plain `setImage:` renders a
/// text-only menu. objc2-app-kit 0.3 predates the property, hence the raw
/// message, guarded for older systems (where images always show).
fn show_image(item: &NSMenuItem) {
    if item.respondsToSelector(sel!(setPreferredImageVisibility:)) {
        // SAFETY: macOS 27 `-[NSMenuItem setPreferredImageVisibility:]`
        // takes an NSInteger enum; presence checked above.
        unsafe {
            let _: () = msg_send![item, setPreferredImageVisibility: IMAGE_VISIBILITY_VISIBLE];
        }
    }
}

/// The item's `preferredImageVisibility`, or `None` before macOS 27.
fn image_visibility(item: &NSMenuItem) -> Option<isize> {
    if !item.respondsToSelector(sel!(preferredImageVisibility)) {
        return None;
    }
    // SAFETY: as in `show_image`; the getter returns the NSInteger enum.
    Some(unsafe { msg_send![item, preferredImageVisibility] })
}

thread_local! {
    /// Loaded symbol images (main thread only; the menu is rebuilt on every
    /// poll, the images never change).
    static SYMBOL_IMAGES: RefCell<HashMap<Symbol, Option<Retained<NSImage>>>> =
        RefCell::new(HashMap::new());
}

/// The SF Symbol image for `symbol`, cached. Neutral and progress tones
/// stay template images (tinted like the menu text, inverted on the
/// selection highlight, which is itself the accent colour); ok / warn /
/// error / merged take the matching dynamic system colour (the app
/// window's `.tone-*`), legible in light and dark.
fn symbol_image(symbol: Symbol) -> Option<Retained<NSImage>> {
    SYMBOL_IMAGES.with(|cache| {
        cache
            .borrow_mut()
            .entry(symbol)
            .or_insert_with(|| {
                let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(symbol.sf_name()),
                    Some(&NSString::from_str(symbol.description())),
                );
                if image.is_none() {
                    eprintln!("prx-menubar: no SF Symbol {:?}", symbol.sf_name());
                }
                let image = image?;
                let color = match symbol.tone() {
                    Tone::Neutral | Tone::Progress => return Some(image),
                    Tone::Ok => NSColor::systemGreenColor(),
                    Tone::Warn => NSColor::systemOrangeColor(),
                    Tone::Error => NSColor::systemRedColor(),
                    Tone::Merged => NSColor::systemPurpleColor(),
                };
                let config = NSImageSymbolConfiguration::configurationWithHierarchicalColor(&color);
                image.imageWithSymbolConfiguration(&config).or(Some(image))
            })
            .clone()
    })
}

/// One rendered row (muda models leaves, separators and submenu parents as
/// distinct types; this keeps the recursion expressible).
enum Rendered {
    Item(MenuItem),
    Check(CheckMenuItem),
    Separator(PredefinedMenuItem),
    Submenu(Submenu),
}

impl Rendered {
    fn as_item(&self) -> &dyn IsMenuItem {
        match self {
            Rendered::Item(item) => item,
            Rendered::Check(item) => item,
            Rendered::Separator(item) => item,
            Rendered::Submenu(submenu) => submenu,
        }
    }
}

/// The muda accelerator for a model shortcut (⌘ plus the key, ⌥ for the
/// alternates). Settings… keeps the app menu's own ⌘, accelerator.
fn accelerator(shortcut: Shortcut) -> Option<Accelerator> {
    let code = match shortcut.key {
        ',' if !shortcut.option => return Some(settings::settings_accelerator()),
        ',' => Code::Comma,
        'n' => Code::KeyN,
        'o' => Code::KeyO,
        'q' => Code::KeyQ,
        'r' => Code::KeyR,
        other => {
            eprintln!("prx-menubar: no key code for shortcut {other:?}");
            return None;
        }
    };
    let modifiers = if shortcut.option {
        Modifiers::META | Modifiers::ALT
    } else {
        Modifiers::META
    };
    Some(Accelerator::new(modifiers, code))
}

fn render_item(item: &ModelItem) -> Result<Rendered, muda::Error> {
    let accelerator = item.shortcut.and_then(accelerator);
    match item.kind {
        ItemKind::Separator => return Ok(Rendered::Separator(PredefinedMenuItem::separator())),
        // Review-mode radio entry: the checkmark comes from the payload.
        ItemKind::Mode => {
            return Ok(Rendered::Check(CheckMenuItem::with_id(
                item.id.clone(),
                item.label.as_str(),
                item.enabled,
                item.checked,
                accelerator,
            )))
        }
        _ => {}
    }
    if item.children.is_empty() {
        return Ok(Rendered::Item(MenuItem::with_id(
            item.id.clone(),
            item.label.as_str(),
            item.enabled,
            accelerator,
        )));
    }
    let children: Vec<Rendered> = item
        .children
        .iter()
        .map(render_item)
        .collect::<Result<_, _>>()?;
    let refs: Vec<&dyn IsMenuItem> = children.iter().map(Rendered::as_item).collect();
    Ok(Rendered::Submenu(Submenu::with_id_and_items(
        item.id.clone(),
        item.label.as_str(),
        item.enabled,
        &refs,
    )?))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(position) = args.iter().position(|arg| arg == "--self-test") {
        return run_self_test(args.get(position + 1).map(String::as_str));
    }
    if args.iter().any(|arg| arg == "--icon-report") {
        return run_icon_report();
    }
    // A2: the login item launches with `--background` (menu bar only); any
    // other launch opens the window. The retired `--settings` flag still
    // opens the window, on the General page.
    let background = args.iter().any(|arg| arg == "--background");
    let open_page: Option<&str> = if args.iter().any(|arg| arg == "--settings") {
        Some("general")
    } else if background {
        None
    } else {
        Some("dashboard")
    };

    let mtm =
        MainThreadMarker::new().expect("prx-menubar must run its event loop on the main thread");
    let application = NSApplication::sharedApplication(mtm);
    // A menu bar extra: no Dock icon until the window opens.
    application.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let claim = match instance::claim_or_forward(open_page) {
        instance::Startup::Primary(claim) => claim,
        instance::Startup::Forwarded(pid) => {
            eprintln!("prx-menubar: PRX is already running (pid {pid}); forwarded the launch");
            return ExitCode::SUCCESS;
        }
    };

    eprintln!(
        "prx-menubar: starting PRX {} (prx data dir: {}, config: {}{})",
        env!("CARGO_PKG_VERSION"),
        prx::paths::data_dir().display(),
        prx::paths::config_path().display(),
        if background { ", background" } else { "" }
    );

    let (sender, pending) = mainloop::channel::<Msg>();
    let (tx, rx) = std::sync::mpsc::channel::<PollerCommand>();
    let poll_sender = sender.clone();
    poller::spawn_with_handler(rx, move |outcome| {
        poll_sender.send(Msg::Poll(outcome));
    });
    let poller = PollerHandle::new(tx.clone());
    let action_sender = sender.clone();
    let actions = ActionController::new(
        tx,
        Arc::new(move |message: String| {
            action_sender.send(Msg::Action(message));
        }),
    );

    // A3: attach to a running daemon, else spawn and supervise the bundled
    // one. State changes reach the menu through the main-loop queue.
    let supervisor_sender = sender.clone();
    let supervisor = Supervisor::spawn(supervisor::bundled_prx(), move |state| {
        supervisor_sender.send(Msg::Supervisor(state));
    });
    let quit_mode = Rc::new(Cell::new(QuitMode::StopOwned));

    // A2 delegate: Dock/Finder reopen and the second-launch notification
    // open the window (queued: applied on the main loop, never re-entrantly);
    // termination (any path) settles the owned daemon and releases the
    // instance pidfile. The terminate hook deliberately touches no App
    // state: it runs inside `terminate:`, possibly under a menu handler.
    let delegate_sender = sender.clone();
    let terminate_supervisor = supervisor.clone();
    let terminate_mode = Rc::clone(&quit_mode);
    let claim = RefCell::new(Some(claim));
    let _delegate = instance::AppDelegate::install(mtm, move |event| match event {
        instance::AppEvent::OpenWindow(page) => delegate_sender.send(Msg::OpenWindow(page)),
        instance::AppEvent::WillTerminate => {
            let stop = terminate_mode.get() == QuitMode::StopOwned;
            terminate_supervisor.shutdown(stop);
            drop(claim.borrow_mut().take());
        }
    });

    // A7: PRX.app posts its own banners (sender "PRX"). Installed before
    // the run loop so a banner click that launched the app is delivered;
    // asks for permission once. None for a bare binary (fallback path).
    let _notification_delegate = notify_center::init();

    let app = Rc::new(RefCell::new(App::new(
        poller,
        actions,
        pending.clone(),
        sender.clone(),
        settings::Settings::new(mtm),
        Some(supervisor),
        quit_mode,
    )));

    // Tray/menu events arrive synchronously on the main thread (AppKit
    // target/action); forward them into the shared App state.
    let menu_app = MainCell(Rc::clone(&app));
    MenuEvent::set_event_handler(Some(move |event| {
        menu_app.with(|app| app.handle_menu_event(event));
    }));
    let tray_app = MainCell(Rc::clone(&app));
    TrayIconEvent::set_event_handler(Some(move |event| {
        tray_app.with(|app| app.handle_tray_event(event));
    }));

    if let Err(error) = app.borrow_mut().build_tray() {
        eprintln!("prx-menubar: failed to create tray icon: {error}");
        return ExitCode::FAILURE;
    }
    eprintln!("prx-menubar: status-bar icon registered (offline until first poll)");

    mainloop::install_source(pending, {
        let app = Rc::clone(&app);
        move |message| app.borrow_mut().apply_msg(message)
    });

    // Kick the first poll immediately instead of waiting one interval.
    app.borrow_mut().poller.refresh();
    if let Some(page) = open_page {
        sender.send(Msg::OpenWindow(page.to_string()));
    }

    application.run();
    ExitCode::SUCCESS
}

/// Headless verification harness: runs the IDENTICAL poll loop (printing
/// each menu model) and the identical action path
/// (`ActionController::run` — the exact code `dispatch` spawns for a real
/// click), then shows the post-action refresh. Example:
/// `prx-menubar --self-test "snooze:2|org/repo#13"`.
/// `--icon-report`: render the offline menu plus a synthetic submenu
/// carrying every [`Symbol`] through the production path ([`render_menu`]),
/// then print, per native item with a symbol, whether it has an image and
/// whether that image is marked visible. Exits non-zero on any miss; the
/// `menu_icons` integration test runs it (menu creation needs the main
/// thread, which libtest's worker threads are not).
fn run_icon_report() -> ExitCode {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("icon-report: not on the main thread");
        return ExitCode::FAILURE;
    };
    let _application = NSApplication::sharedApplication(mtm);
    let mut model = model::offline_model();
    if let Some(section) = model.sections.last_mut() {
        section.items.push(icon_report_submenu());
    }
    let menu = match render_menu(&model) {
        Ok(menu) => menu,
        Err(error) => {
            eprintln!("icon-report: render failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut checked = 0;
    let mut failures = 0;
    for_each_native(&menu, &model, &mut |ns_menu, index, item| {
        let Some(symbol) = item.symbol else { return };
        let native = ns_menu.itemAtIndex(index);
        let image = native.as_ref().and_then(|native| native.image()).is_some();
        let visibility = native.as_ref().and_then(|native| image_visibility(native));
        let ok = image && visibility.is_none_or(|v| v == IMAGE_VISIBILITY_VISIBLE);
        checked += 1;
        if !ok {
            failures += 1;
        }
        println!(
            "{} {:<20} {:<30} image={image} visibility={visibility:?} {:?}",
            if ok { "ok  " } else { "FAIL" },
            format!("{symbol:?}"),
            symbol.sf_name(),
            item.label
        );
    });
    println!("icon-report: {checked} items with symbols, {failures} failing");
    if failures == 0 && checked > 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// A submenu whose children carry every symbol (the report's coverage of
/// nested items and of the whole symbol vocabulary).
fn icon_report_submenu() -> ModelItem {
    let leaf = |symbol: Symbol| ModelItem {
        id: format!("icon-report|{symbol:?}"),
        label: format!("{symbol:?}"),
        enabled: true,
        kind: ItemKind::Action,
        checked: false,
        alternate: false,
        indent: 0,
        symbol: Some(symbol),
        shortcut: None,
        children: Vec::new(),
    };
    ModelItem {
        children: Symbol::ALL.into_iter().map(leaf).collect(),
        ..leaf(Symbol::History)
    }
}

fn run_self_test(id: Option<&str>) -> ExitCode {
    let Some(id) = id else {
        eprintln!("usage: prx-menubar --self-test <action-id>   (e.g. \"snooze:2|org/repo#13\")");
        return ExitCode::FAILURE;
    };

    let (tx, rx) = std::sync::mpsc::channel::<PollerCommand>();
    let (outcome_tx, outcome_rx) = std::sync::mpsc::channel::<PollOutcome>();
    poller::spawn_with_handler(rx, move |outcome| {
        match &outcome {
            PollOutcome::Up(snapshot) => eprintln!(
                "prx-menubar self-test: {}",
                model::build_model(
                    &snapshot.status,
                    &snapshot.history,
                    &mut model::PaginationState::default(),
                    chrono::Utc::now(),
                )
                .dump()
                .trim_end()
            ),
            PollOutcome::Down(error) => {
                eprintln!("prx-menubar self-test: poll failed: {error}");
                eprintln!(
                    "prx-menubar self-test: {}",
                    model::offline_model().dump().trim_end()
                );
            }
        }
        let _ = outcome_tx.send(outcome);
    });

    let actions = ActionController::new(
        tx.clone(),
        Arc::new(|message: String| {
            eprintln!("prx-menubar self-test: message line: {message}");
        }),
    );

    eprintln!("prx-menubar self-test: baseline poll…");
    let _ = tx.send(PollerCommand::Refresh);
    if outcome_rx.recv_timeout(Duration::from_secs(30)).is_err() {
        eprintln!("prx-menubar self-test: no baseline poll within 30 s");
        return ExitCode::FAILURE;
    }

    eprintln!("prx-menubar self-test: dispatching '{id}' through the production handler");
    actions.run(id);

    eprintln!("prx-menubar self-test: waiting for the post-action refresh…");
    if outcome_rx.recv_timeout(Duration::from_secs(90)).is_err() {
        eprintln!("prx-menubar self-test: no post-action poll within 90 s");
        return ExitCode::FAILURE;
    }

    eprintln!("prx-menubar self-test: done");
    ExitCode::SUCCESS
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Every symbol the model can emit names an SF Symbol this macOS has
    /// (`imageWithSystemSymbolName:` returns nil for unknown names, which
    /// would silently drop the row's icon).
    #[test]
    fn every_symbol_resolves_to_an_image() {
        let missing: Vec<&str> = Symbol::ALL
            .into_iter()
            .filter(|symbol| symbol_image(*symbol).is_none())
            .map(Symbol::sf_name)
            .collect();
        assert!(missing.is_empty(), "SF Symbols not found: {missing:?}");
    }

    /// Tinted (hierarchical-colour) variants still carry a drawable size.
    #[test]
    fn symbol_images_are_not_empty() {
        for symbol in Symbol::ALL {
            let size = symbol_image(symbol).expect("symbol image").size();
            assert!(
                size.width > 0.0 && size.height > 0.0,
                "{symbol:?} is empty: {size:?}"
            );
        }
    }

    #[test]
    fn symbol_list_is_complete_and_unique() {
        let names: std::collections::HashSet<Symbol> = Symbol::ALL.into_iter().collect();
        assert_eq!(names.len(), Symbol::ALL.len());
    }
}
