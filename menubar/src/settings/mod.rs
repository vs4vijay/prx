//! The settings window (tasks 8.1/8.2, design D6): a titled objc2
//! `NSWindow` whose content view hosts a `wry` WKWebView showing the
//! embedded UI ([`assets`], `menubar/settings-ui/`). No tao, no second
//! event loop: the window lives on the shell's own NSApplication run loop,
//! and bridge replies come back through a [`mainloop`] default-mode queue
//! (the same mechanism the tray uses).
//!
//! The PRX app hosts one lazily created window (footer "Open PRX" /
//! "Settings…", a relaunch, or a Dock reopen — design A2); the accessory
//! activation policy is restored on close. The former standalone
//! `prx-menubar --settings` host is retired: the app is single-instance
//! ([`pidfile`], [`crate::instance`]), so a second launch asks the running
//! app to open this window. While the window is open the app runs with the
//! `.regular` activation policy (Dock icon "PRX", normal focus) and the PRX
//! app menu (About PRX, Settings… ⌘,, Hide PRX, Quit PRX) plus an Edit menu
//! so the standard edit shortcuts (⌘C/⌘V/⌘A/⌘Z) and ⌘W reach the window.

pub mod app_bridge;
pub mod assets;
pub mod bridge;
pub mod browser_bridge;
pub mod pidfile;

use std::borrow::Cow;
use std::cell::RefCell;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::Arc;

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use raw_window_handle::{
    AppKitWindowHandle, HandleError, HasWindowHandle, RawWindowHandle, WindowHandle,
};
use wry::http::{header::CONTENT_TYPE, Response as HttpResponse, StatusCode};
use wry::{NewWindowResponse, WebView, WebViewBuilder};

use crate::mainloop::{self, MainThreadSender};
use bridge::{Bridge, DaemonBackend};

/// Initial window size (points).
const WIDTH: f64 = 1040.0;
const HEIGHT: f64 = 700.0;

/// The user-visible app name (window title, app menu, About).
pub const APP_NAME: &str = "PRX";

/// What an [`Settings::open`] request did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    Created,
    Focused,
}

/// Main-thread messages for the window host.
#[derive(Clone)]
enum SettingsMsg {
    /// A bridge reply to hand to the page.
    Reply(bridge::Response),
    /// `browsers.choose` (request id): run the app chooser panel. Deferred
    /// to the run loop so the modal panel never runs inside the webview's
    /// IPC callback.
    ChooseApp(serde_json::Value),
    /// The window is closing (sent from the delegate; applied deferred).
    Closed,
}

/// The settings window host. Main thread only; cheap to keep around (the
/// window itself is created on first open and dropped on close).
pub struct Settings {
    inner: Rc<RefCell<Inner>>,
}

struct Inner {
    mtm: MainThreadMarker,
    sender: MainThreadSender<SettingsMsg>,
    open: Option<OpenWindow>,
}

/// Everything alive while the window is shown. Field order is drop order:
/// the webview goes before its window.
struct OpenWindow {
    webview: WebView,
    window: Retained<NSWindow>,
    _delegate: Retained<WindowDelegate>,
    menu: Menu,
}

impl Settings {
    /// Create the host and attach its reply queue to the main run loop.
    pub fn new(mtm: MainThreadMarker) -> Settings {
        let (sender, pending) = mainloop::channel::<SettingsMsg>();
        let inner = Rc::new(RefCell::new(Inner {
            mtm,
            sender,
            open: None,
        }));
        let weak = Rc::downgrade(&inner);
        mainloop::install_source(pending, move |message| {
            let Some(inner) = weak.upgrade() else { return };
            let message = match message {
                // The modal panel runs its own run loop: keep `inner`
                // unborrowed meanwhile (a Dock reopen may need it).
                SettingsMsg::ChooseApp(id) => {
                    SettingsMsg::Reply(match browser_bridge::choose(mtm) {
                        Ok(value) => bridge::Response::success(id, value),
                        Err(error) => bridge::Response::failure(id, error),
                    })
                }
                other => other,
            };
            inner.borrow_mut().apply(message);
        });
        Settings { inner }
    }

    /// Show the window on `page` (`dashboard`, `general`, …; design D16):
    /// focus it and switch to the page when already open, create it on the
    /// page otherwise.
    pub fn open(&self, page: &str) -> Result<Opened, String> {
        self.inner.borrow_mut().open(page)
    }
}

impl Inner {
    fn apply(&mut self, message: SettingsMsg) {
        match message {
            SettingsMsg::Reply(reply) => {
                let Some(open) = &self.open else { return };
                if let Err(error) = open.webview.evaluate_script(&reply.to_script()) {
                    eprintln!("prx-menubar: settings bridge reply failed: {error}");
                }
            }
            // Answered in `new`'s queue handler, outside this borrow.
            SettingsMsg::ChooseApp(id) => {
                self.apply(SettingsMsg::Reply(bridge::Response::failure(
                    id,
                    bridge::BridgeError::unsupported("app chooser unavailable"),
                )))
            }
            SettingsMsg::Closed => self.closed(),
        }
    }

    fn open(&mut self, page: &str) -> Result<Opened, String> {
        let app = NSApplication::sharedApplication(self.mtm);
        if let Some(open) = &self.open {
            if let Err(error) = open.webview.evaluate_script(&assets::go_script(page)) {
                eprintln!("prx-menubar: cannot switch the window to {page}: {error}");
            }
            open.window.deminiaturize(None);
            open.window.makeKeyAndOrderFront(None);
            activate(&app);
            return Ok(Opened::Focused);
        }

        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        let menu = main_menu().map_err(|e| format!("cannot build menu: {e}"))?;
        menu.init_for_nsapp();

        let window = new_window(self.mtm);
        let delegate = WindowDelegate::new(self.mtm, self.sender.clone());
        window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        let webview = match self.build_webview(&window, page) {
            Ok(webview) => webview,
            Err(error) => {
                window.setDelegate(None);
                window.close();
                menu.remove_for_nsapp();
                self.restore_policy(&app);
                return Err(error);
            }
        };
        window.center();
        window.makeKeyAndOrderFront(None);
        activate(&app);
        self.open = Some(OpenWindow {
            webview,
            window,
            _delegate: delegate,
            menu,
        });
        Ok(Opened::Created)
    }

    fn build_webview(&self, window: &NSWindow, page: &str) -> Result<WebView, String> {
        let content = window
            .contentView()
            .ok_or_else(|| "settings window has no content view".to_string())?;
        let parent = ContentView(NonNull::from(&*content).cast());

        let sender = self.sender.clone();
        let chooser = self.sender.clone();
        let bridge = Bridge::new(
            DaemonBackend,
            Arc::new(move |reply| sender.send(SettingsMsg::Reply(reply))),
        )
        .with_chooser(Arc::new(move |id| chooser.send(SettingsMsg::ChooseApp(id))));
        WebViewBuilder::new()
            .with_custom_protocol(assets::SCHEME.into(), |_id, request| {
                serve_asset(request.uri().path())
            })
            .with_navigation_handler(|url| {
                if assets::is_allowed_navigation(&url) {
                    return true;
                }
                // An external link never replaces the app: it opens in the
                // repo's browser instead.
                if assets::is_external_link(&url) {
                    crate::browser::open_external(url);
                } else {
                    eprintln!("prx-menubar: window blocked navigation to {url}");
                }
                false
            })
            // `target="_blank"` links ("Open in GitHub"): no second webview
            // window — hand them to the browser.
            .with_new_window_req_handler(|url, _features| {
                if assets::is_external_link(&url) {
                    crate::browser::open_external(url);
                } else {
                    eprintln!("prx-menubar: window blocked a new window for {url}");
                }
                NewWindowResponse::Deny
            })
            .with_ipc_handler(move |request| bridge.handle(request.body()))
            .with_url(assets::start_url(page))
            .with_devtools(cfg!(debug_assertions))
            .build(&parent)
            .map_err(|e| format!("cannot create the settings webview: {e}"))
    }

    /// The window closed: drop it, then restore the accessory policy (no
    /// Dock icon while only the menu bar item is showing).
    fn closed(&mut self) {
        let Some(open) = self.open.take() else { return };
        open.menu.remove_for_nsapp();
        open.window.setDelegate(None);
        drop(open);
        let app = NSApplication::sharedApplication(self.mtm);
        eprintln!("prx-menubar: settings window closed");
        self.restore_policy(&app);
    }

    fn restore_policy(&self, app: &NSApplication) {
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}

fn activate(app: &NSApplication) {
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

fn new_window(mtm: MainThreadMarker) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
    // SAFETY: plain designated initializer on a fresh allocation, main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: we own the window through `Retained`; AppKit must not also
    // release it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(APP_NAME));
    window.setMinSize(NSSize::new(640.0, 440.0));
    window
}

/// The conventional macOS shortcut for an app's settings: ⌘,.
pub fn settings_accelerator() -> Accelerator {
    Accelerator::new(Modifiers::META, Code::Comma)
}

/// The PRX app menu (About PRX, Settings… ⌘,, Hide PRX, Close Window,
/// Quit PRX ⌘Q) and the Edit menu: the key equivalents for the webview's
/// text fields live in the main menu. Settings… carries the tray footer's
/// id, so ⌘, takes the same route and brings the window forward; Quit PRX
/// carries the footer Quit's id, so ⌘Q stops an owned daemon the same way.
fn main_menu() -> Result<Menu, muda::Error> {
    let app_menu = Submenu::new(APP_NAME, true);
    let about = AboutMetadata {
        name: Some(APP_NAME.into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        comments: Some("Local-first pull request reviews".into()),
        ..Default::default()
    };
    app_menu.append_items(&[
        &PredefinedMenuItem::about(Some(&format!("About {APP_NAME}")), Some(about)),
        &PredefinedMenuItem::separator(),
        &MenuItem::with_id(
            crate::actions::SETTINGS_ID,
            "Settings…",
            true,
            Some(settings_accelerator()),
        ),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::hide(Some(&format!("Hide {APP_NAME}"))),
        &PredefinedMenuItem::hide_others(None),
        &PredefinedMenuItem::show_all(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::close_window(Some("Close Window")),
        &MenuItem::with_id(
            crate::model::QUIT_ID,
            crate::model::QUIT_LABEL,
            true,
            Some(Accelerator::new(Modifiers::META, Code::KeyQ)),
        ),
    ])?;
    let edit = Submenu::new("Edit", true);
    edit.append_items(&[
        &PredefinedMenuItem::undo(None),
        &PredefinedMenuItem::redo(None),
        &PredefinedMenuItem::separator(),
        &PredefinedMenuItem::cut(None),
        &PredefinedMenuItem::copy(None),
        &PredefinedMenuItem::paste(None),
        &PredefinedMenuItem::select_all(None),
    ])?;
    let menu = Menu::new();
    menu.append_items(&[&app_menu, &edit])?;
    Ok(menu)
}

fn serve_asset(path: &str) -> HttpResponse<Cow<'static, [u8]>> {
    let built = match assets::lookup(path) {
        Some((content_type, bytes)) => HttpResponse::builder()
            .header(CONTENT_TYPE, content_type)
            .body(Cow::Borrowed(bytes)),
        None => HttpResponse::builder()
            .status(StatusCode::NOT_FOUND)
            .header(CONTENT_TYPE, "text/plain")
            .body(Cow::Borrowed(&b"not found"[..])),
    };
    built.unwrap_or_else(|_| HttpResponse::new(Cow::Borrowed(&b""[..])))
}

/// The window's content view as a raw-window-handle parent for wry (which
/// replaces the window's content view with its own container).
struct ContentView(NonNull<c_void>);

impl HasWindowHandle for ContentView {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::AppKit(AppKitWindowHandle::new(self.0));
        // SAFETY: the view outlives the borrow (wry uses it during `build`
        // only, and the window retains it).
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

define_class!(
    /// Forwards `windowWillClose:` to the host's queue (applied after
    /// AppKit finishes closing, never re-entrantly).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PrxSettingsWindowDelegate"]
    #[ivars = MainThreadSender<SettingsMsg>]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            self.ivars().send(SettingsMsg::Closed);
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, sender: MainThreadSender<SettingsMsg>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(sender);
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    }
}
