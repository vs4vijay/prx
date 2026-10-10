//! Pure menu model (spec: prx/menu-bar-shell, D2). `build_model` turns a
//! `GET /status` payload plus the recent `GET /history` entries into
//! plain-data [`MenuModel`]s — sections of items with an id, label, enabled
//! flag, kind and (for submenu parents) children — with no GUI types
//! anywhere, so grouping, the attention count, pagination, the per-PR action
//! submenus and the footer are unit-testable hermetically. `main.rs` renders
//! the model 1:1 into a `muda` menu.
//!
//! Item ids encode `<action>|<arg>` (e.g. `open|org/repo#13`,
//! `snooze:2|org/repo#13`) so the click handler can dispatch;
//! [`crate::actions`] owns the vocabulary.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use prx::config::{MenuGroupBy, MenuSort, ReviewMode};
use prx::history::HistoryEntry;
use prx::ipc::StatusResponse;
use prx::state::{PrState, SkipReason};
use prx::status::StatusEntry;

use crate::actions;
use crate::supervisor::DaemonState;

/// PR rows per page in every lifecycle group (design D7). The first page
/// always renders; "Show more" reveals the next page in place.
pub const PAGE_SIZE: usize = 10;

/// Per-group pagination (design D7): how many pages each group shows. Lives
/// in the shell (not the model) so it survives rebuilds; `build_model`
/// reconciles it against each group's current PR key-set and resets a group
/// to page 1 whenever that set changes (a PR appeared, left, or moved), so
/// stale deep pagination can never hide a new arrival.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaginationState {
    /// Group slug → pages shown (absent = 1).
    pages: HashMap<String, usize>,
    /// Group slug → the sorted PR keys last rendered in that group.
    keys: HashMap<String, Vec<String>>,
    /// The grouping mode last rendered; switching it resets every group.
    mode: Option<MenuGroupBy>,
}

impl PaginationState {
    /// Note the grouping mode about to render: a change (the user switched
    /// "Group PRs by") starts every group over on its first page.
    pub fn set_mode(&mut self, mode: MenuGroupBy) {
        if self.mode != Some(mode) {
            self.pages.clear();
            self.keys.clear();
            self.mode = Some(mode);
        }
    }

    /// Pages currently shown for `group` (at least 1).
    pub fn pages(&self, group: &str) -> usize {
        self.pages.get(group).copied().unwrap_or(1).max(1)
    }

    /// "Show more": reveal one more page (clamped at render time).
    pub fn more(&mut self, group: &str) {
        let next = self.pages(group) + 1;
        self.pages.insert(group.to_string(), next);
    }

    /// "Show less": collapse back to the first page.
    pub fn less(&mut self, group: &str) {
        self.pages.insert(group.to_string(), 1);
    }

    /// Reconcile one group's key-set and return the pages to render, clamped
    /// to what the group can fill.
    fn reconcile(&mut self, group: &str, keys: Vec<String>) -> usize {
        if self.keys.get(group) != Some(&keys) {
            self.pages.insert(group.to_string(), 1);
        }
        let max_pages = keys.len().div_ceil(PAGE_SIZE).max(1);
        let pages = self.pages(group).min(max_pages);
        self.pages.insert(group.to_string(), pages);
        self.keys.insert(group.to_string(), keys);
        pages
    }
}

/// Max history entries rendered in the History submenu (most recent first).
pub const HISTORY_LIMIT: usize = 10;

/// The id of the Quit item ("Quit PRX": also stops a daemon the app
/// started, design A3/6.5).
pub const QUIT_ID: &str = "quit";
/// Its label (also the app menu's ⌘Q item).
pub const QUIT_LABEL: &str = "Quit PRX";
/// The ⌥ alternate of Quit: quit the UI, leave the daemon running.
pub const QUIT_KEEP_ID: &str = "quit-keep";
/// Its label (the renderer marks it as the ⌥ alternate of [`QUIT_LABEL`]).
pub const QUIT_KEEP_LABEL: &str = "Quit PRX (keep reviewing)";
/// Offline menu: start the bundled daemon (design A3).
pub const START_DAEMON_ID: &str = "daemon-start";
/// Open the newest daemon log in Console (offline menu; online, the ⌥
/// alternate of Open PRX and the version-mismatch health line).
pub const SHOW_LOG_ID: &str = "daemon-log";

/// The PRX app's own version (the headline's ⌥ diagnostics line; the
/// daemon reports its own).
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A menu key equivalent: ⌘ plus `key`, and ⌥ when `option` (the ⌥ alternate
/// of a ⌘ item). Shown while the menu is open and answered there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub key: char,
    pub option: bool,
}

/// ⌘`key`.
pub const fn cmd(key: char) -> Shortcut {
    Shortcut { key, option: false }
}

/// ⌥⌘`key`.
pub const fn cmd_option(key: char) -> Shortcut {
    Shortcut { key, option: true }
}

/// What a rendered item represents. The renderer treats leaf kinds as plain
/// disabled-or-enabled `MenuItem`s and items with children as `Submenu`s; the
/// kinds exist so tests (and the action dispatcher) can tell actionable rows
/// from decoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    /// Disabled informational line (attention banner, daemon state, last
    /// poll, action-outcome message, history rows).
    Info,
    /// Disabled group header ("Reviewed (12)").
    GroupHeader,
    /// A PR row: a submenu parent whose children are its actions
    /// (Open/Review…/Approve/Snooze/Ignore). The row itself opens the
    /// submenu (macOS gives submenu rows no separate click action).
    Pr,
    /// A wired click action (per-PR actions, Check GitHub now, Review PR…,
    /// Open PRX, Settings…, the review-mode submenu).
    Action,
    /// The History submenu parent (navigation only; must stay enabled so it
    /// can expand).
    History,
    /// A separator line inside a submenu (top-level sections are separated
    /// by the renderer; this groups a submenu's rows).
    Separator,
    /// Quit the app ("Quit PRX" and its ⌥ alternate "Quit PRX (keep
    /// reviewing)"; local, always enabled).
    Quit,
    /// Local daemon supervision while offline ("Start daemon", "Show
    /// log"): handled by the app itself, never an IPC call.
    Supervise,
    /// One entry of the review-mode radio group (`mode:comment` /
    /// `mode:monitor` / `mode:off`, design D8) inside the "Review mode:"
    /// submenu; `checked` marks the daemon's current global mode. Rendered
    /// as a check item.
    Mode,
}

/// One renderable menu row. Items with `children` render as `Submenu`s whose
/// entries are the children (one level of nesting further is used by the
/// per-PR Snooze group).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelItem {
    /// `<action>|<arg>` string handed to muda as the `MenuId`.
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub kind: ItemKind,
    /// Checkmark state (only meaningful for [`ItemKind::Mode`] radio rows).
    pub checked: bool,
    /// Shown instead of the item above it while ⌥ is held (the renderer
    /// marks it as that item's `NSMenuItem` alternate). Used for the Quit
    /// alternate.
    pub alternate: bool,
    /// `NSMenuItem.indentationLevel`: the inline Approve under a ready PR
    /// (paired with its ↳ symbol, so the association reads at a glance).
    pub indent: u8,
    /// The SF Symbol in the item's image column (design-audit Part 0,
    /// round 2): one symbol vocabulary for states, verdicts, health and
    /// the top-level commands.
    pub symbol: Option<Symbol>,
    /// Key equivalent shown at the row's trailing edge.
    pub shortcut: Option<Shortcut>,
    pub children: Vec<ModelItem>,
}

impl ModelItem {
    fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        enabled: bool,
        kind: ItemKind,
    ) -> ModelItem {
        ModelItem {
            id: id.into(),
            label: label.into(),
            enabled,
            kind,
            checked: false,
            alternate: false,
            indent: 0,
            symbol: None,
            shortcut: None,
            children: Vec::new(),
        }
    }

    fn with_children(
        id: impl Into<String>,
        label: impl Into<String>,
        enabled: bool,
        kind: ItemKind,
        children: Vec<ModelItem>,
    ) -> ModelItem {
        ModelItem {
            id: id.into(),
            label: label.into(),
            enabled,
            kind,
            checked: false,
            alternate: false,
            indent: 0,
            symbol: None,
            shortcut: None,
            children,
        }
    }

    fn info(id: impl Into<String>, label: impl Into<String>) -> ModelItem {
        ModelItem::new(id, label, false, ItemKind::Info)
    }

    /// A separator row inside a submenu.
    fn separator() -> ModelItem {
        ModelItem::new("separator", "", false, ItemKind::Separator)
    }

    /// This item as the ⌥ alternate of the item before it.
    fn into_alternate(mut self) -> ModelItem {
        self.alternate = true;
        self
    }

    /// This item with a key equivalent.
    fn with_shortcut(mut self, shortcut: Shortcut) -> ModelItem {
        self.shortcut = Some(shortcut);
        self
    }

    /// This item with a symbol in its image column.
    fn with_symbol(mut self, symbol: Symbol) -> ModelItem {
        self.symbol = Some(symbol);
        self
    }
}

/// The colour role of a [`Symbol`], mirroring the app window's state tones
/// (settings-ui `app.css` `.tone-*`, `overview.js` `STATES`). `Neutral`
/// renders as a template image (tinted like the menu text); the others map
/// to the matching dynamic system colour, so they hold up in light and dark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    Neutral,
    Progress,
    Ok,
    Warn,
    Error,
    Merged,
}

/// Every image the menu shows (design-audit Part 0, round 2): SF Symbols
/// only, no emoji or ad-hoc unicode in labels. PR states use the app
/// window's state system (same labels, same tones); the header's status
/// symbol follows the menu bar glyph's precedence. Each symbol has a
/// monochrome text fallback the renderer prefixes when the symbol cannot
/// be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Symbol {
    // PR and history states (app: STATES).
    NeedsReview,
    CoolingDown,
    ReReview,
    InReview,
    Ready,
    Approved,
    ChangesRequested,
    Commented,
    Skipped,
    Merged,
    Closed,
    Failed,
    Brief,
    // Header status.
    AllClear,
    Attention,
    Paused,
    Warning,
    Info,
    Success,
    // Commands.
    CheckNow,
    ReviewPr,
    ApproveFor,
    Mode,
    OpenWindow,
    Settings,
    OpenConfig,
    History,
    ShowLog,
    StartDaemon,
    Quit,
}

impl Symbol {
    /// Every symbol, for the renderer's availability checks.
    pub const ALL: [Symbol; 30] = [
        Symbol::NeedsReview,
        Symbol::CoolingDown,
        Symbol::ReReview,
        Symbol::InReview,
        Symbol::Ready,
        Symbol::Approved,
        Symbol::ChangesRequested,
        Symbol::Commented,
        Symbol::Skipped,
        Symbol::Merged,
        Symbol::Closed,
        Symbol::Failed,
        Symbol::Brief,
        Symbol::AllClear,
        Symbol::Attention,
        Symbol::Paused,
        Symbol::Warning,
        Symbol::Info,
        Symbol::Success,
        Symbol::CheckNow,
        Symbol::ReviewPr,
        Symbol::ApproveFor,
        Symbol::Mode,
        Symbol::OpenWindow,
        Symbol::Settings,
        Symbol::OpenConfig,
        Symbol::History,
        Symbol::ShowLog,
        Symbol::StartDaemon,
        Symbol::Quit,
    ];

    /// The SF Symbol name.
    pub fn sf_name(self) -> &'static str {
        match self {
            Symbol::NeedsReview => "circle",
            Symbol::CoolingDown => "hourglass",
            Symbol::ReReview => "arrow.clockwise.circle",
            Symbol::InReview => "ellipsis.circle",
            Symbol::Ready => "checkmark.circle",
            Symbol::Approved => "checkmark.circle.fill",
            Symbol::ChangesRequested => "exclamationmark.circle.fill",
            Symbol::Commented => "text.bubble",
            Symbol::Skipped => "minus.circle",
            Symbol::Merged => "arrow.triangle.merge",
            Symbol::Closed => "xmark.circle",
            Symbol::Failed => "exclamationmark.triangle.fill",
            Symbol::Brief => "doc.text",
            Symbol::AllClear => "checkmark.circle.fill",
            Symbol::Attention => "exclamationmark.circle.fill",
            Symbol::Paused => "pause.circle",
            Symbol::Warning => "exclamationmark.triangle.fill",
            Symbol::Info => "info.circle",
            Symbol::Success => "checkmark",
            Symbol::CheckNow => "arrow.clockwise",
            Symbol::ReviewPr => "text.magnifyingglass",
            Symbol::ApproveFor => "arrow.turn.down.right",
            Symbol::Mode => "slider.horizontal.3",
            Symbol::OpenWindow => "macwindow",
            Symbol::Settings => "gearshape",
            Symbol::OpenConfig => "doc.badge.gearshape",
            Symbol::History => "clock.arrow.circlepath",
            Symbol::ShowLog => "doc.text.magnifyingglass",
            Symbol::StartDaemon => "play.circle",
            Symbol::Quit => "power",
        }
    }

    /// Its colour role (the app window's tone for the same state).
    pub fn tone(self) -> Tone {
        match self {
            Symbol::InReview => Tone::Progress,
            Symbol::Ready | Symbol::Approved | Symbol::AllClear | Symbol::Success => Tone::Ok,
            Symbol::ChangesRequested | Symbol::Failed | Symbol::Attention => Tone::Error,
            Symbol::Warning => Tone::Warn,
            Symbol::Merged => Tone::Merged,
            _ => Tone::Neutral,
        }
    }

    /// The accessibility description (VoiceOver reads it with the row, since
    /// the state is no longer text in the label). States use the app
    /// window's labels.
    pub fn description(self) -> &'static str {
        match self {
            Symbol::NeedsReview => "Needs review",
            Symbol::CoolingDown => "Cooling down",
            Symbol::ReReview => "Re-review pending",
            Symbol::InReview => "In review",
            Symbol::Ready => "Ready for approval",
            Symbol::Approved => "Approved",
            Symbol::ChangesRequested => "Changes requested",
            Symbol::Commented => "Commented",
            Symbol::Skipped => "Skipped",
            Symbol::Merged => "Merged",
            Symbol::Closed => "Closed",
            Symbol::Failed => "Failed",
            Symbol::Brief => "Brief",
            Symbol::AllClear => "All clear",
            Symbol::Attention => "Needs attention",
            Symbol::Paused => "Paused",
            Symbol::Warning => "Warning",
            Symbol::Info => "Info",
            Symbol::Success => "Done",
            Symbol::CheckNow => "Check",
            Symbol::ReviewPr => "Review",
            Symbol::ApproveFor => "For the PR above",
            Symbol::Mode => "Mode",
            Symbol::OpenWindow => "Window",
            Symbol::Settings => "Settings",
            Symbol::OpenConfig => "Config",
            Symbol::History => "History",
            Symbol::ShowLog => "Log",
            Symbol::StartDaemon => "Start",
            Symbol::Quit => "Quit",
        }
    }

    /// Monochrome text stand-in, used only when the symbol fails to load.
    pub fn fallback(self) -> &'static str {
        match self {
            Symbol::NeedsReview => "\u{25cb}",                 // ○
            Symbol::CoolingDown => "\u{29d7}",                 // ⧗
            Symbol::ReReview | Symbol::CheckNow => "\u{21bb}", // ↻
            Symbol::InReview => "\u{2026}",                    // …
            Symbol::Ready | Symbol::Approved | Symbol::AllClear | Symbol::Success => "\u{2713}",
            Symbol::ChangesRequested | Symbol::Attention => "!",
            Symbol::Failed | Symbol::Warning => "\u{26a0}\u{fe0e}",
            Symbol::Skipped => "\u{2013}",
            Symbol::Merged => "\u{2192}",
            Symbol::Closed => "\u{00d7}",
            Symbol::Paused => "\u{2016}",
            Symbol::ApproveFor => "\u{21b3}", // ↳
            _ => "",
        }
    }
}

/// A group of items rendered together (separated from other sections).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuSection {
    pub items: Vec<ModelItem>,
}

/// The whole menu, pure data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuModel {
    /// True when the daemon could not be reached; no PR rows are rendered
    /// and every actionable item is absent or disabled.
    pub offline: bool,
    /// PRs in `reviewed_changes_requested` + `in_review` (drives the icon).
    pub attention: usize,
    /// Reviews currently running (`StatusResponse.in_flight`; drives the
    /// icon's in-progress state).
    pub in_flight: usize,
    /// The daemon's global review mode is `off` (drives the icon's paused
    /// variant).
    pub stopped: bool,
    /// A daemon-wide pause is active (also the icon's paused variant).
    pub paused: bool,
    pub sections: Vec<MenuSection>,
}

impl MenuModel {
    /// Flat view of every item, in render order (children first after their
    /// parent).
    pub fn items(&self) -> impl Iterator<Item = &ModelItem> {
        self.sections
            .iter()
            .flat_map(|s| s.items.iter())
            .flat_map(item_and_descendants)
    }

    /// Human-readable dump used for debug logs (verification aid: the menu
    /// model is logged on every rebuild).
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            &mut out,
            "menu model: offline={} attention={} in_flight={} stopped={} paused={}",
            self.offline, self.attention, self.in_flight, self.stopped, self.paused
        );
        for (i, section) in self.sections.iter().enumerate() {
            let _ = writeln!(&mut out, "  section {i}:");
            for item in &section.items {
                dump_item(&mut out, item, 0);
            }
        }
        out
    }

    /// The action-outcome line (errors, daemon notes) as a disabled info item
    /// in the status header, right under the headline (and its ⌥ alternate)
    /// and any health line. The shell calls this before rendering whenever a
    /// message is pending. The publishers' leading `✓ ` / `⚠ ` become the
    /// line's symbol instead of text, so the menu stays on one icon system.
    pub fn with_message(self, message: impl Into<String>) -> MenuModel {
        let mut model = self;
        let message = message.into();
        let (symbol, text) = if let Some(rest) = message.strip_prefix("\u{2713} ") {
            (Symbol::Success, rest.to_string())
        } else if let Some(rest) = message.strip_prefix("\u{26a0} ") {
            (Symbol::Warning, rest.to_string())
        } else {
            (Symbol::Info, message.clone())
        };
        let item = ModelItem::info("info|message", text).with_symbol(symbol);
        if let Some(section) = model.sections.first_mut() {
            let at = section
                .items
                .iter()
                .rposition(|i| {
                    // Headline, its ⌥ alternate, and a health line (whose id
                    // is its fix: open the config or the log).
                    [
                        "info|attention",
                        "info|about",
                        actions::OPEN_CONFIG_ID,
                        SHOW_LOG_ID,
                    ]
                    .contains(&i.id.as_str())
                })
                .map(|p| p + 1)
                .unwrap_or(section.items.len());
            section.items.insert(at, item);
        }
        model
    }
}

fn item_and_descendants(item: &ModelItem) -> Box<dyn Iterator<Item = &ModelItem> + '_> {
    Box::new(std::iter::once(item).chain(item.children.iter().flat_map(item_and_descendants)))
}

fn dump_item(out: &mut String, item: &ModelItem, depth: usize) {
    let shortcut = item
        .shortcut
        .map(|s| format!("  [{}⌘{}]", if s.option { "⌥" } else { "" }, s.key))
        .unwrap_or_default();
    let _ = writeln!(
        out,
        "    {:indent$}[{:3}] {:<11} {:<24} {}{}{}{}{}{}",
        "",
        if item.enabled { "on" } else { "off" },
        format!("{:?}", item.kind),
        item.id,
        if item.alternate { "(⌥) " } else { "" },
        item.symbol
            .map(|s| format!("<{}> ", s.sf_name()))
            .unwrap_or_default(),
        if item.indent > 0 { "  " } else { "" },
        if item.checked { "✓ " } else { "" },
        item.label,
        shortcut,
        indent = depth * 4
    );
    for child in &item.children {
        dump_item(out, child, depth + 1);
    }
}

/// Lifecycle groups in render order (design-audit Part 0): what needs the
/// human first — the attention groups the headline counts (changes
/// requested, in review), then PRs ready for the human's approval (design
/// D11) — then the queue, then everything settled.
const GROUPS: &[(Group, &str)] = &[
    (Group::ChangesRequested, "Changes requested"),
    (Group::InReview, "In review"),
    (Group::ReadyForApproval, "Ready for approval"),
    (Group::NeedsReview, "Needs review"),
    (Group::Reviewed, "Reviewed"),
    (Group::Skipped, "Skipped"),
    (Group::Terminal, "Merged / closed"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    ChangesRequested,
    InReview,
    ReadyForApproval,
    NeedsReview,
    Reviewed,
    Skipped,
    Terminal,
}

/// One rendered group of PR rows (design D8): its pagination slug (also the
/// header id suffix), its header title (the count is appended), and its
/// entries in sort order. Empty groups are never produced.
#[derive(Debug, Clone)]
pub struct RenderGroup<'a> {
    pub slug: String,
    pub title: String,
    pub entries: Vec<&'a StatusEntry>,
}

/// Partition `sorted` (already in `menu.sort` order) into the groups of
/// `group_by`:
/// - `lifecycle`: [`GROUPS`] in their fixed order;
/// - `repo`: one group per repo, by repo name (slug `repo:{org/repo}`);
/// - `none`: a single "Pull requests" list (slug `all`).
pub fn render_groups(sorted: Vec<&StatusEntry>, group_by: MenuGroupBy) -> Vec<RenderGroup<'_>> {
    match group_by {
        MenuGroupBy::Lifecycle => GROUPS
            .iter()
            .map(|&(group, title)| RenderGroup {
                slug: group_slug(group).to_string(),
                title: title.to_string(),
                entries: sorted
                    .iter()
                    .copied()
                    .filter(|e| group_of(e) == group)
                    .collect(),
            })
            .filter(|g| !g.entries.is_empty())
            .collect(),
        MenuGroupBy::Repo => {
            let mut repos: BTreeMap<&str, Vec<&StatusEntry>> = BTreeMap::new();
            for entry in sorted {
                repos.entry(repo_of(&entry.key)).or_default().push(entry);
            }
            repos
                .into_iter()
                .map(|(repo, entries)| RenderGroup {
                    slug: format!("repo:{repo}"),
                    title: repo.to_string(),
                    entries,
                })
                .collect()
        }
        MenuGroupBy::None if sorted.is_empty() => Vec::new(),
        MenuGroupBy::None => vec![RenderGroup {
            slug: "all".to_string(),
            title: "Pull requests".to_string(),
            entries: sorted,
        }],
    }
}

/// The repo part of a PR key (`org/repo#13` → `org/repo`).
fn repo_of(key: &str) -> &str {
    key.split_once('#').map(|(repo, _)| repo).unwrap_or(key)
}

/// The `key` order: repo name, then the PR number numerically (`#9` before
/// `#10`); a malformed key sorts by its text after the well-formed ones of
/// its repo.
fn key_order(a: &StatusEntry, b: &StatusEntry) -> Ordering {
    let split = |key: &str| {
        let (repo, number) = key.split_once('#').unwrap_or((key, ""));
        (
            repo.to_string(),
            number.parse::<u64>().ok(),
            key.to_string(),
        )
    };
    let (ra, na, ka) = split(&a.key);
    let (rb, nb, kb) = split(&b.key);
    ra.cmp(&rb)
        .then_with(|| match (na, nb) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        })
        .then_with(|| ka.cmp(&kb))
}

/// Newest first on an optional timestamp; missing values sort last.
fn newest_first(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => y.cmp(&x),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Sort the PR rows by `menu.sort` (design D8). Ties — and PRs without the
/// sorted timestamp, which go last — fall back to [`key_order`], so the
/// order is total and stable across polls.
pub fn sort_entries(entries: &mut [&StatusEntry], sort: MenuSort) {
    entries.sort_by(|a, b| {
        let primary = match sort {
            MenuSort::CreatedDesc => newest_first(a.created_at, b.created_at),
            MenuSort::CreatedAsc => match (a.created_at, b.created_at) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
            MenuSort::LastReviewDesc => newest_first(a.last_review_at, b.last_review_at),
            MenuSort::Key => Ordering::Equal,
        };
        primary.then_with(|| key_order(a, b))
    });
}

/// Map a PR onto the display groups: the lifecycle state, plus the agent's
/// verdict to pull approve-ready PRs out of the generic reviewed group.
/// Changes-requested PRs get their own group (they are half of the
/// attention count; buried in "Reviewed" the headline pointed nowhere).
fn group_of(entry: &StatusEntry) -> Group {
    match entry.state {
        PrState::Pending => Group::NeedsReview,
        PrState::InReview => Group::InReview,
        PrState::ReviewedApproved if awaiting_human_approval(entry) => Group::ReadyForApproval,
        PrState::ReviewedChangesRequested => Group::ChangesRequested,
        PrState::ReviewedApproved => Group::Reviewed,
        PrState::Skipped => Group::Skipped,
        PrState::Merged | PrState::Closed => Group::Terminal,
    }
}

/// "Ready for approval" (design D11): the record sits in
/// `reviewed_approved` because the AGENT's latest verdict was `approved`,
/// and no human has approved since (task 8.1). A human Approve also lands
/// in `reviewed_approved` but stamps `human_approved_at` (cleared again when
/// the next agent review completes or the PR heads back to `pending`), so a
/// PR the human already approved leaves this group for Reviewed. A human
/// Approve over a changes-requested / comment-only verdict was never here:
/// its agent verdict is not `approved`.
fn awaiting_human_approval(entry: &StatusEntry) -> bool {
    entry.state == PrState::ReviewedApproved
        && entry.last_verdict == Some(Verdict::Approved)
        && entry.human_approved_at.is_none()
}

/// PRs needing attention: changes requested + reviews in flight (spec:
/// "Menu bar icon with needs-attention count").
pub fn attention_count(prs: &[StatusEntry]) -> usize {
    prs.iter()
        .filter(|e| {
            matches!(
                e.state,
                PrState::ReviewedChangesRequested | PrState::InReview
            )
        })
        .count()
}

/// Build the online menu model from a fresh `/status` payload and the most
/// recent `/history` entries (History submenu; empty slice renders a
/// placeholder row). `pagination` is the shell's per-group page state
/// (reconciled here: a group whose PR key-set changed resets to page 1);
/// `now` anchors the PR rows' compact age (injectable for tests).
///
/// Layout (design-audit Part 0, rounds 2–3), one separator between sections:
/// 1. status header — one line: the headline, when PRX checked GitHub and
///    when it checks next (or the pause countdown), with the daemon's
///    diagnostics as its ⌥ alternate; a second line only when there is
///    something extra (a health problem with its fix, the action message);
/// 2. quick actions — Check GitHub now ⌘R, Review PR… ⌘N;
/// 3. the PR groups, attention first;
/// 4. automatic reviews — the review-mode submenu, with the pause inside
///    it, titled with both states;
/// 5. the app — Open PRX ⌘O (⌥: Show daemon log), Settings… ⌘, (⌥: Open
///    config.yml), History ▸;
/// 6. Quit PRX ⌘Q (⌥: keep reviewing).
pub fn build_model(
    status: &StatusResponse,
    history: &[HistoryEntry],
    pagination: &mut PaginationState,
    now: DateTime<Utc>,
) -> MenuModel {
    let attention = attention_count(&status.prs);
    let off = status.review_mode == ReviewMode::Off;
    let pause_left = pause_remaining(status.paused_until, now);

    // The status header: the headline is the attention count — the one
    // thing to act on. Review mode `off` replaces it (design D8): nothing
    // is polled, so the rows below are last-known state and a count would
    // overstate what PRX knows. A pause (design D5) keeps the count live
    // (polling continues) and takes the "next check" slot.
    let headline = if off {
        "Reviews off".to_string()
    } else if status.prs.is_empty() {
        "No PRs tracked yet".to_string()
    } else if attention == 1 {
        "1 PR needs attention".to_string()
    } else if attention > 1 {
        format!("{attention} PRs need attention")
    } else {
        "All clear".to_string()
    };
    // The symbol follows the menu bar glyph's precedence (paused/stopped,
    // reviewing, attention, idle), so the header explains the glyph.
    let status_symbol = if off || pause_left.is_some() {
        Symbol::Paused
    } else if status.in_flight > 0 {
        Symbol::InReview
    } else if attention > 0 {
        Symbol::Attention
    } else {
        Symbol::AllClear
    };
    let mut top = vec![
        ModelItem::info(
            "info|attention",
            format!(
                "{headline} \u{b7} {}",
                check_line(status, pause_left.as_deref(), now)
            ),
        )
        .with_symbol(status_symbol),
        diagnostics_item(status, now),
    ];
    if let Some(health) = health_item(status) {
        top.push(health);
    }

    // Quick actions, above the (possibly long) PR list so they never need
    // a scroll. "Check GitHub now" is the ONE refresh: it asks the daemon
    // to poll GitHub immediately (the menu's own re-read of the daemon
    // happens on every open, so it needs no item).
    let quick = vec![
        ModelItem::new(
            actions::REVIEW_NOW_ID,
            "Check GitHub now",
            true,
            ItemKind::Action,
        )
        .with_shortcut(cmd('r'))
        .with_symbol(Symbol::CheckNow),
        ModelItem::new(
            actions::REVIEW_PR_ID,
            "Review PR\u{2026}",
            true,
            ItemKind::Action,
        )
        .with_shortcut(cmd('n'))
        .with_symbol(Symbol::ReviewPr),
    ];

    // Sort once, then partition into the rendered groups (design D8):
    // partitioning a sorted list keeps every group sorted. A payload
    // without `menu` (older daemon) deserializes to the defaults.
    let menu = status.menu;
    let mut sorted: Vec<&StatusEntry> = status.prs.iter().collect();
    sort_entries(&mut sorted, menu.sort);
    pagination.set_mode(menu.group_by);

    let mut sections = vec![MenuSection { items: top }, MenuSection { items: quick }];
    // Grouped by repo or not at all, the PRs the headline counts would be
    // scattered through the list (possibly pages deep): pin them in a
    // "Needs attention" group first. Lifecycle grouping already leads with
    // them.
    if menu.group_by != MenuGroupBy::Lifecycle {
        let pinned: Vec<&StatusEntry> = sorted
            .iter()
            .copied()
            .filter(|e| {
                matches!(
                    e.state,
                    PrState::ReviewedChangesRequested | PrState::InReview
                )
            })
            .collect();
        if !pinned.is_empty() {
            let mut items = vec![ModelItem::new(
                "info|group|attention",
                format!("Needs attention ({})", pinned.len()),
                false,
                ItemKind::GroupHeader,
            )];
            items.extend(
                pinned
                    .iter()
                    .take(PAGE_SIZE)
                    .map(|entry| pr_item(entry, &status.engines, now)),
            );
            if pinned.len() > PAGE_SIZE {
                items.push(ModelItem::info(
                    "info|attention-more",
                    format!("{} more in the list below", pinned.len() - PAGE_SIZE),
                ));
            }
            sections.push(MenuSection { items });
        }
    }
    for group in render_groups(sorted, menu.group_by) {
        let RenderGroup {
            slug,
            title,
            entries,
        } = group;
        let mut items = vec![ModelItem::new(
            format!("info|group|{slug}"),
            format!("{title} ({})", entries.len()),
            false,
            ItemKind::GroupHeader,
        )];
        let keys = entries.iter().map(|e| e.key.clone()).collect();
        let pages = pagination.reconcile(&slug, keys);
        let shown = (pages * PAGE_SIZE).min(entries.len());
        for entry in &entries[..shown] {
            items.push(pr_item(entry, &status.engines, now));
            if awaiting_human_approval(entry) {
                // Inline Approve (spec "Ready-for-approval grouping"), in
                // every grouping mode: the same id as the submenu's
                // Approve, so one dispatch path.
                // Round 2: "↳ Approve #13…" — the turn-down arrow sits in
                // the image column under the PR's state symbol, indented
                // one level, and the label names the PR's number.
                let mut approve = ModelItem::new(
                    format!("{}|{}", actions::APPROVE_PREFIX, entry.key),
                    format!("Approve #{}\u{2026}", pr_number(&entry.key)),
                    true,
                    ItemKind::Action,
                )
                .with_symbol(Symbol::ApproveFor);
                approve.indent = 1;
                items.push(approve);
            }
        }
        if shown < entries.len() {
            let hidden = entries.len() - shown;
            items.push(ModelItem::new(
                format!("{}|{slug}", actions::PAGE_MORE_PREFIX),
                format!("Show more ({hidden} hidden)"),
                true,
                ItemKind::Action,
            ));
        }
        if pages > 1 {
            items.push(ModelItem::new(
                format!("{}|{slug}", actions::PAGE_LESS_PREFIX),
                "Show less",
                true,
                ItemKind::Action,
            ));
        }
        sections.push(MenuSection { items });
    }

    // Automatic reviews: one submenu for the lasting mode and the
    // temporary hold on it; its title states both.
    sections.push(MenuSection {
        items: vec![mode_item(status.review_mode, pause_left.as_deref())],
    });

    // The app: the window, settings, and the history browser. The
    // troubleshooting shortcuts are their ⌥ alternates (the log behind
    // Open PRX, the raw config behind Settings…), so they never cost a row.
    sections.push(MenuSection {
        items: vec![
            ModelItem::new(actions::OPEN_WINDOW_ID, "Open PRX", true, ItemKind::Action)
                .with_shortcut(cmd('o'))
                .with_symbol(Symbol::OpenWindow),
            ModelItem::new(SHOW_LOG_ID, "Show daemon log", true, ItemKind::Supervise)
                .into_alternate()
                .with_shortcut(cmd_option('o'))
                .with_symbol(Symbol::ShowLog),
            ModelItem::new(
                actions::SETTINGS_ID,
                "Settings\u{2026}",
                true,
                ItemKind::Action,
            )
            .with_shortcut(cmd(','))
            .with_symbol(Symbol::Settings),
            ModelItem::new(
                actions::OPEN_CONFIG_ID,
                "Open config.yml",
                true,
                ItemKind::Action,
            )
            .into_alternate()
            .with_shortcut(cmd_option(','))
            .with_symbol(Symbol::OpenConfig),
            ModelItem::with_children(
                actions::HISTORY_ID,
                "History",
                true,
                ItemKind::History,
                history_items(history, now),
            )
            .with_symbol(Symbol::History),
        ],
    });
    sections.push(quit_section());

    MenuModel {
        offline: false,
        attention,
        in_flight: status.in_flight,
        stopped: off,
        paused: pause_left.is_some(),
        sections,
    }
}

/// Quit PRX ⌘Q and its ⌥ alternate, in their own last section (the macOS
/// convention: a separator above Quit).
fn quit_section() -> MenuSection {
    MenuSection {
        items: vec![
            ModelItem::new(QUIT_ID, QUIT_LABEL, true, ItemKind::Quit)
                .with_shortcut(cmd('q'))
                .with_symbol(Symbol::Quit),
            ModelItem::new(QUIT_KEEP_ID, QUIT_KEEP_LABEL, true, ItemKind::Quit)
                .into_alternate()
                .with_shortcut(cmd_option('q'))
                .with_symbol(Symbol::Quit),
        ],
    }
}

/// The headline's ⌥ alternate (design-audit Part 0, round 2): the daemon
/// diagnostics a user needs only when something is off — versions, pid,
/// uptime and the work counts — on one disabled line, in place of the old
/// About submenu. Healthy, the daemon costs no row at all.
fn diagnostics_item(status: &StatusResponse, now: DateTime<Utc>) -> ModelItem {
    let uptime = compact_age(
        now - chrono::Duration::seconds(status.uptime_seconds as i64),
        now,
    );
    ModelItem::info(
        "info|about",
        format!(
            "PRX {APP_VERSION} \u{b7} daemon {} \u{b7} pid {} \u{b7} up {uptime} \u{b7} {} \u{b7} {} reviewing \u{b7} {} queued",
            status.version,
            status.pid,
            count(status.prs.len(), "PR"),
            status.in_flight,
            status.queue_depth
        ),
    )
    .with_symbol(Symbol::Info)
    .into_alternate()
}

/// Menu length of the config-error message in the health line.
const CONFIG_ERROR_MENU_CHARS: usize = 60;

/// The header's health line, present only when the running daemon is not
/// healthy, and always carrying its fix: a config.yml the daemon could not
/// load (it runs on the last good one) opens the raw config; a daemon whose
/// version differs from the app's (a stale build still running) opens its
/// log.
fn health_item(status: &StatusResponse) -> Option<ModelItem> {
    if let Some(error) = &status.config_error {
        return Some(
            ModelItem::new(
                actions::OPEN_CONFIG_ID,
                format!(
                    "config.yml not loaded: {} \u{2014} Open\u{2026}",
                    truncate_summary(&error.message, CONFIG_ERROR_MENU_CHARS)
                ),
                true,
                ItemKind::Action,
            )
            .with_symbol(Symbol::Warning),
        );
    }
    if status.version != APP_VERSION {
        return Some(
            ModelItem::new(
                SHOW_LOG_ID,
                format!(
                    "Daemon {} is running, app is {APP_VERSION} \u{2014} Show log",
                    status.version
                ),
                true,
                ItemKind::Supervise,
            )
            .with_symbol(Symbol::Warning),
        );
    }
    None
}

/// The offline model (spec scenario "Daemon not running"): a clear offline
/// indicator, no PR rows at all, and nothing daemon-backed (footer actions
/// are absent rather than disabled — the daemon cannot honor them anyway).
/// Equivalent to [`offline_model_for`] with no supervision state.
pub fn offline_model() -> MenuModel {
    offline_model_for(&DaemonState::Unmanaged)
}

/// The offline model under the app's daemon supervision (design A3): the
/// headline and daemon line say whether the app is starting, restarting,
/// or has stopped its daemon; "Start daemon" and "Show log" are offered
/// whenever no start is already underway. No refresh item: the menu
/// re-probes the daemon every time it opens (and every ~10 s), so a manual
/// "Refresh now" could only ever repeat what opening the menu just did.
pub fn offline_model_for(daemon: &DaemonState) -> MenuModel {
    let (headline, symbol, can_start) = match daemon {
        DaemonState::Unmanaged => (
            "Daemon offline \u{b7} PRX reconnects when it starts".to_string(),
            Symbol::Warning,
            true,
        ),
        DaemonState::Starting | DaemonState::Running { .. } => (
            "Starting the daemon\u{2026}".to_string(),
            Symbol::CoolingDown,
            false,
        ),
        DaemonState::Restarting {
            delay_secs,
            failures,
        } => (
            format!(
                "Daemon restarting \u{b7} failure {failures} of {}, retry in {delay_secs}s",
                crate::supervisor::MAX_FAILURES
            ),
            Symbol::CoolingDown,
            false,
        ),
        DaemonState::Stopped { gave_up: true } => (
            format!(
                "Daemon stopped \u{b7} {} crashes in {} min",
                crate::supervisor::MAX_FAILURES,
                crate::supervisor::FAILURE_WINDOW.as_secs() / 60
            ),
            Symbol::Failed,
            true,
        ),
        DaemonState::Stopped { gave_up: false } => {
            ("Daemon stopped".to_string(), Symbol::Failed, true)
        }
    };
    let top = vec![
        ModelItem::info("info|attention", headline).with_symbol(symbol),
        ModelItem::info(
            "info|about",
            format!("PRX {APP_VERSION} \u{b7} daemon not running"),
        )
        .with_symbol(Symbol::Info)
        .into_alternate(),
    ];
    let mut supervise = Vec::new();
    if can_start {
        supervise.push(
            ModelItem::new(START_DAEMON_ID, "Start daemon", true, ItemKind::Supervise)
                .with_symbol(Symbol::StartDaemon),
        );
    }
    supervise.push(
        ModelItem::new(SHOW_LOG_ID, "Show daemon log", true, ItemKind::Supervise)
            .with_symbol(Symbol::ShowLog),
    );
    MenuModel {
        offline: true,
        attention: 0,
        in_flight: 0,
        stopped: false,
        paused: false,
        sections: vec![
            MenuSection { items: top },
            MenuSection { items: supervise },
            // The window works offline (it shows the daemon state); the
            // daemon-backed items are absent.
            MenuSection {
                items: vec![ModelItem::new(
                    actions::OPEN_WINDOW_ID,
                    "Open PRX",
                    true,
                    ItemKind::Action,
                )
                .with_shortcut(cmd('o'))
                .with_symbol(Symbol::OpenWindow)],
            },
            quit_section(),
        ],
    }
}

/// The headline's timing: when PRX last checked GitHub and when it checks
/// next, relative to `now` (so always local, never a raw UTC timestamp).
/// While paused the countdown takes the "next" slot (a check enqueues
/// nothing then); the next check is omitted while reviews are off (no
/// polling) or when the daemon has not planned one.
fn check_line(status: &StatusResponse, pause_left: Option<&str>, now: DateTime<Utc>) -> String {
    let mut line = match status.last_state_poll {
        Some(at) => format!("checked {} ago", compact_age(at, now)),
        None => "not checked yet".to_string(),
    };
    if status.review_mode == ReviewMode::Off {
        return line;
    }
    if let Some(left) = pause_left {
        let _ = write!(line, " \u{b7} paused, {left} left");
    } else if let Some(next) = status.next_poll {
        match pause_remaining(Some(next), now) {
            Some(left) => {
                let _ = write!(line, " \u{b7} next in {left}");
            }
            None => line.push_str(" \u{b7} checking now"),
        }
    }
    line
}

/// Remaining pause time as a compact countdown (`1h 12m`, `45m`, `2d 3h`),
/// or `None` when not paused. Rounded UP to the minute so a live pause never
/// reads `0m`; an end already in the past (payload a poll old) is not a
/// pause.
pub fn pause_remaining(paused_until: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<String> {
    let seconds = (paused_until? - now).num_seconds();
    if seconds <= 0 {
        return None;
    }
    let minutes = (seconds + 59) / 60;
    let (days, hours, mins) = (minutes / (60 * 24), (minutes / 60) % 24, minutes % 60);
    Some(match (days, hours, mins) {
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d {h}h"),
    })
}

/// The app window's mode labels (settings-ui schema.js MODE_OPTIONS).
pub fn mode_name(mode: ReviewMode) -> &'static str {
    match mode {
        ReviewMode::Off => "Off",
        ReviewMode::Monitor => "Monitor",
        ReviewMode::ReviewComment => "Review and comment",
    }
}

/// The review-mode submenu (design-audit Part 0, round 3): the lasting
/// posture (`defaults.review_mode`) and the temporary pause on it, in one
/// place. The title states both — "Review mode: Monitor", or "Review mode:
/// Monitor · paused, 1h 12m left" — and its symbol is the pause bars while
/// paused or off (the menu bar glyph's paused/stopped state).
///
/// Inside, the mode radio group: a header says it is the default for repos
/// without their own mode; each row says what the mode does to GitHub, from
/// most to least active; "Per-repo modes…" opens the window's Repos page.
/// Selecting a mode posts `/review-mode`. Then, after a separator, the
/// pause section ([`pause_rows`]).
fn mode_item(current: ReviewMode, pause_left: Option<&str>) -> ModelItem {
    let off = current == ReviewMode::Off;
    let mut children = vec![
        ModelItem::info(
            "info|mode|scope",
            "Default for repos without their own mode",
        ),
        ModelItem::separator(),
    ];
    for (mode, id, what) in [
        (
            ReviewMode::ReviewComment,
            actions::MODE_COMMENT_ID,
            "reviews and posts to GitHub",
        ),
        (
            ReviewMode::Monitor,
            actions::MODE_MONITOR_ID,
            "reviews, never posts",
        ),
        (
            ReviewMode::Off,
            actions::MODE_OFF_ID,
            "no checks, no reviews",
        ),
    ] {
        let label = format!("{} \u{2014} {what}", mode_name(mode));
        let mut item = ModelItem::new(id, label, true, ItemKind::Mode);
        item.checked = mode == current;
        children.push(item);
    }
    children.push(ModelItem::separator());
    children.push(ModelItem::new(
        actions::OPEN_REPOS_ID,
        "Per-repo modes\u{2026}",
        true,
        ItemKind::Action,
    ));
    children.push(ModelItem::separator());
    children.extend(pause_rows(off, pause_left));

    // Off wins over a pause, as in the headline: nothing runs either way.
    let mut title = format!("Review mode: {}", mode_name(current));
    if let (false, Some(left)) = (off, pause_left) {
        let _ = write!(title, " \u{b7} paused, {left} left");
    }
    ModelItem::with_children(
        actions::MODE_PARENT_ID,
        title,
        true,
        ItemKind::Action,
        children,
    )
    .with_symbol(if off || pause_left.is_some() {
        Symbol::Paused
    } else {
        Symbol::Mode
    })
}

/// The pause section of the review-mode submenu (design D5/D11): a
/// temporary, daemon-wide hold on AUTOMATIC reviews that ends on its own.
/// It opens with its title ("Pause automatic reviews") or, while paused,
/// the countdown ("Automatic reviews paused · 1h 12m left") and "Resume
/// now"; a line says what a pause does; then the durations, which restart
/// the pause from now while one is running → `POST /pause` / `POST
/// /resume`. In mode `off` there is nothing to hold: the durations stay
/// visible but disabled, under the reason (a running pause can still be
/// resumed).
fn pause_rows(off: bool, left: Option<&str>) -> Vec<ModelItem> {
    let mut rows = Vec::new();
    match left {
        Some(left) => {
            rows.push(
                ModelItem::info(
                    "info|pause|status",
                    format!("Automatic reviews paused \u{b7} {left} left"),
                )
                .with_symbol(Symbol::Paused),
            );
            rows.push(ModelItem::new(
                actions::RESUME_ID,
                "Resume now",
                true,
                ItemKind::Action,
            ));
        }
        None => rows.push(
            ModelItem::info("info|pause|title", "Pause automatic reviews")
                .with_symbol(Symbol::Paused),
        ),
    }
    rows.push(ModelItem::info(
        "info|pause|what",
        "Holds automatic reviews; manual reviews still run; resumes by itself",
    ));
    if off {
        rows.push(ModelItem::info(
            "info|pause|off",
            "Nothing to pause while the review mode is Off",
        ));
    } else if left.is_some() {
        rows.push(ModelItem::info(
            "info|pause|when",
            "Pick a time to restart the pause from now",
        ));
    }
    rows.extend(
        [
            (actions::PAUSE_30M_ID, "For 30 minutes"),
            (actions::PAUSE_1H_ID, "For 1 hour"),
            (actions::PAUSE_2H_ID, "For 2 hours"),
            (actions::PAUSE_4H_ID, "For 4 hours"),
        ]
        .into_iter()
        .map(|(id, label)| ModelItem::new(id, label, !off, ItemKind::Action)),
    );
    rows
}

/// Stable slug for a group (used in ids/dumps).
fn group_slug(group: Group) -> &'static str {
    match group {
        Group::ChangesRequested => "Changes-requested",
        Group::NeedsReview => "Needs-review",
        Group::InReview => "In-review",
        Group::ReadyForApproval => "Ready-for-approval",
        Group::Reviewed => "Reviewed",
        Group::Skipped => "Skipped",
        Group::Terminal => "Terminal",
    }
}

/// One PR row: state icon + key + author + age + title + one-line verdict
/// summary (see [`pr_label`]), rendered
/// as a submenu carrying the per-PR actions (spec: "Per-PR actions round-trip
/// through the daemon"). Mutating actions are disabled for terminal PRs
/// (the daemon would reject or no-op them); Open stays available. The row
/// itself opens the submenu — macOS submenu rows have no separate click
/// action — and "Open in GitHub" is its first entry.
fn pr_item(entry: &StatusEntry, engines: &[String], now: DateTime<Utc>) -> ModelItem {
    let key = &entry.key;
    let terminal = matches!(entry.state, PrState::Merged | PrState::Closed);
    let action = |id: String, label: &str| ModelItem::new(id, label, !terminal, ItemKind::Action);
    // Open is a read on GitHub, not a daemon mutation: always available.
    let open = ModelItem::new(
        format!("{}|{key}", actions::OPEN_PREFIX),
        "Open in GitHub",
        true,
        ItemKind::Action,
    );
    let snooze = ModelItem::with_children(
        actions::SNOOZE_PARENT_ID,
        "Snooze",
        !terminal,
        ItemKind::Action,
        vec![
            action(format!("{}|{key}", actions::SNOOZE_1H), "Snooze 1 hour"),
            action(format!("{}|{key}", actions::SNOOZE_2H), "Snooze 2 hours"),
            action(format!("{}|{key}", actions::SNOOZE_4H), "Snooze 4 hours"),
            action(
                format!("{}|{key}", actions::SNOOZE_PUSH),
                "Snooze until next push",
            ),
        ],
    );
    // Show exactly the skip toggle that applies: Un-ignore only for
    // currently-ignored PRs, Un-snooze only for currently-snoozed ones
    // (the daemon's unignore/unsnooze are no-ops otherwise); Ignore for
    // everything else.
    let skipped = entry.state == PrState::Skipped;
    let (skip_id, skip_label) = if skipped && entry.skip_reason == Some(SkipReason::Ignored) {
        (actions::UNIGNORE_PREFIX, "Stop ignoring")
    } else if skipped && entry.skip_reason == Some(SkipReason::Snoozed) {
        (actions::UNSNOOZE_PREFIX, "Unsnooze")
    } else {
        (actions::IGNORE_PREFIX, "Ignore")
    };
    // Unblock replaces Block when prx's identity holds the PR's
    // request-changes review (per the payload; unknown identity → Block).
    let (block_id, block_label) = if entry.offers_unblock() {
        (actions::UNBLOCK_PREFIX, "Unblock")
    } else {
        (actions::BLOCK_PREFIX, "Block (request changes)\u{2026}")
    };
    // Grouped by separators: open · brief · review runs · your verdict on
    // GitHub · hide it for a while.
    let mut children = vec![open];
    let brief = brief_items(entry, now);
    if !brief.is_empty() {
        children.push(ModelItem::separator());
        children.extend(brief);
    }
    children.push(ModelItem::separator());
    // Review actions (design D9), in order: Review → Review with ▸ →
    // Review without posting → Review with prompt…. "Review with" lists the
    // daemon's engines (the repo's own marked "(default)") and is hidden
    // when an older daemon reports none.
    children.push(action(
        format!("{}|{key}", actions::REVIEW_PREFIX),
        "Review",
    ));
    if !engines.is_empty() {
        let engine_items = engines
            .iter()
            .map(|engine| {
                let label = if entry.engine.as_deref() == Some(engine.as_str()) {
                    format!("{engine} (default)")
                } else {
                    engine.clone()
                };
                action(
                    format!("{}|{engine}|{key}", actions::REVIEW_ENGINE_PREFIX),
                    &label,
                )
            })
            .collect();
        children.push(ModelItem::with_children(
            actions::REVIEW_WITH_PARENT_ID,
            "Review with",
            !terminal,
            ItemKind::Action,
            engine_items,
        ));
    }
    children.extend([
        action(
            format!("{}|{key}", actions::REVIEW_NOPOST_PREFIX),
            "Review without posting",
        ),
        action(
            format!("{}|{key}", actions::REVIEW_PROMPT_PREFIX),
            "Review with prompt…",
        ),
        ModelItem::separator(),
        action(
            format!("{}|{key}", actions::APPROVE_PREFIX),
            "Approve\u{2026}",
        ),
        action(format!("{block_id}|{key}"), block_label),
        ModelItem::separator(),
        snooze,
        action(format!("{skip_id}|{key}"), skip_label),
    ]);
    ModelItem::with_children(
        format!("{}|{key}", actions::PR_ROW_PREFIX),
        pr_label(entry, now),
        true,
        ItemKind::Pr,
        children,
    )
    .with_symbol(pr_symbol(entry))
}

/// A PR row's state symbol: the app window's display state (`overview.js`
/// `stateOf`), with "Needs review" split the way the daemon's payload
/// already splits it — new, cooling down, or re-review pending after new
/// commits.
pub fn pr_symbol(entry: &StatusEntry) -> Symbol {
    match entry.state {
        PrState::Pending if entry.last_verdict.is_some() => Symbol::ReReview,
        // The payload's icon is the only cooldown signal the shell gets.
        PrState::Pending if entry.icon.starts_with('\u{23f3}') => Symbol::CoolingDown,
        PrState::Pending => Symbol::NeedsReview,
        PrState::InReview => Symbol::InReview,
        PrState::ReviewedApproved if awaiting_human_approval(entry) => Symbol::Ready,
        PrState::ReviewedApproved if entry.last_verdict == Some(Verdict::CommentOnly) => {
            Symbol::Commented
        }
        PrState::ReviewedApproved => Symbol::Approved,
        PrState::ReviewedChangesRequested => Symbol::ChangesRequested,
        PrState::Skipped => Symbol::Skipped,
        PrState::Merged => Symbol::Merged,
        PrState::Closed => Symbol::Closed,
    }
}

/// The verdict's symbol (the brief block's verdict line, history rows).
fn verdict_symbol(verdict: Option<Verdict>) -> Symbol {
    match verdict {
        Some(Verdict::Approved) => Symbol::Approved,
        Some(Verdict::ChangesRequested) => Symbol::ChangesRequested,
        Some(Verdict::CommentOnly) => Symbol::Commented,
        None => Symbol::NeedsReview,
    }
}

/// The number part of a PR key (`org/repo#13` → `13`).
fn pr_number(key: &str) -> &str {
    key.rsplit_once('#').map(|(_, n)| n).unwrap_or(key)
}

/// Menu length of the drill-down's brief line (design D11: "about 200
/// characters"; the full text is copied on demand).
pub const SUMMARY_MENU_CHARS: usize = 200;

/// Menu length of the drill-down's "Brief failed: …" line.
const BRIEF_ERROR_MENU_CHARS: usize = 80;

/// The drill-down's brief block (task 4.1, spec "Brief in the menu bar"):
/// with a brief, the verdict line, the brief on one line, "updated <age>"
/// (plus "· stale"), "Copy brief" and "Regenerate brief"; without one,
/// "Write brief" when the repo can have briefs. While a brief is being
/// written the action is disabled and reads "Writing brief…"; after a
/// failed run a "Brief failed: …" line explains why. Briefs work for any
/// tracked state (spec "On-demand brief generation"), so nothing here is
/// disabled for merged or closed PRs; Copy is a read and always enabled.
fn brief_items(entry: &StatusEntry, now: DateTime<Utc>) -> Vec<ModelItem> {
    let key = &entry.key;
    let brief = entry.brief.as_ref().filter(|b| !b.text.trim().is_empty());
    let mut items = Vec::new();
    if let Some(brief) = brief {
        items.push(
            ModelItem::info(format!("info|verdict|{key}"), verdict_line(entry))
                .with_symbol(verdict_symbol(entry.last_verdict)),
        );
        items.push(ModelItem::info(
            format!("info|brief|{key}"),
            truncate_summary(&brief.text, SUMMARY_MENU_CHARS),
        ));
        let mut age = format!("updated {} ago", compact_age(brief.updated_at, now));
        if brief.stale {
            age.push_str(" \u{b7} stale");
        }
        items.push(ModelItem::info(format!("info|brief-age|{key}"), age));
        items.push(ModelItem::new(
            format!("{}|{key}", actions::COPY_BRIEF_PREFIX),
            "Copy brief",
            true,
            ItemKind::Action,
        ));
    } else if !entry.brief_available && !entry.brief_pending {
        // Briefs are off for this repo (mode `off` or `brief.enabled:
        // false`): `POST /brief` would refuse, so offer nothing.
        return items;
    }
    if let Some(error) = entry
        .brief_error
        .as_deref()
        .filter(|e| !e.trim().is_empty() && !entry.brief_pending)
    {
        items.push(ModelItem::info(
            format!("info|brief-error|{key}"),
            format!(
                "Brief failed: {}",
                truncate_summary(error, BRIEF_ERROR_MENU_CHARS)
            ),
        ));
    }
    let (label, enabled) = if entry.brief_pending {
        ("Writing brief\u{2026}", false)
    } else if brief.is_some() {
        ("Regenerate brief", entry.brief_available)
    } else {
        ("Write brief", true)
    };
    items.push(ModelItem::new(
        format!("{}|{key}", actions::BRIEF_PREFIX),
        label,
        enabled,
        ItemKind::Action,
    ));
    items
}

/// The brief as one menu line: whitespace runs (newlines included)
/// collapse to single spaces, and text past `max` characters is cut with an
/// ellipsis.
pub fn truncate_summary(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}", cut.trim_end())
}

/// The drill-down's verdict line with its finding counts, shown above the
/// brief (spec "Summary visible before deciding").
fn verdict_line(entry: &StatusEntry) -> String {
    let verdict = match entry.last_verdict {
        Some(Verdict::Approved) => "Approved",
        Some(Verdict::ChangesRequested) => "Changes requested",
        Some(Verdict::CommentOnly) => "Commented",
        None => "Not reviewed",
    };
    format!(
        "{verdict} \u{b7} {} blocking \u{b7} {}",
        entry.blocking_count,
        count(entry.nit_count as usize, "nit")
    )
}

/// `{key} · {author} · {age} — {title} — {summary}` (design D8); the state
/// is the row's symbol ([`pr_symbol`]), not an emoji in the text. The age
/// segment is omitted when the payload carries no `created_at`.
fn pr_label(entry: &StatusEntry, now: DateTime<Utc>) -> String {
    let mut head = format!("{} · {}", entry.key, entry.author);
    if let Some(age) = entry.created_at.map(|at| compact_age(at, now)) {
        let _ = write!(head, " · {age}");
    }
    // Capped so a long title or summary can't widen the whole menu (the
    // full text is in the PR's submenu and the app).
    let mut label = format!(
        "{head} — {}",
        truncate_summary(&entry.title, PR_TITLE_MENU_CHARS)
    );
    if !entry.summary.trim().is_empty() {
        let _ = write!(
            label,
            " — {}",
            truncate_summary(&entry.summary, PR_SUMMARY_MENU_CHARS)
        );
    }
    label
}

/// Menu lengths of a PR row's title and summary segments.
const PR_TITLE_MENU_CHARS: usize = 48;
const PR_SUMMARY_MENU_CHARS: usize = 36;

/// Compact elapsed time since `since`: minutes under an hour (at least
/// `1m`), hours under a day, days under a week, weeks beyond. A `since` in
/// the future (clock skew) counts as just created.
pub fn compact_age(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = (now - since).num_minutes().max(0);
    let (hour, day, week) = (60, 60 * 24, 60 * 24 * 7);
    if minutes < hour {
        format!("{}m", minutes.max(1))
    } else if minutes < day {
        format!("{}h", minutes / hour)
    } else if minutes < week {
        format!("{}d", minutes / day)
    } else {
        format!("{}w", minutes / week)
    }
}

/// The History submenu rows: the most recent [`HISTORY_LIMIT`] entries, each
/// a disabled line `<icon> repo#n — what · age` (decoration; the submenu
/// refreshes with every poll), then "Open history in PRX" for the full,
/// searchable list in the window.
fn history_items(entries: &[HistoryEntry], now: DateTime<Utc>) -> Vec<ModelItem> {
    let mut items: Vec<ModelItem> = if entries.is_empty() {
        vec![ModelItem::info(
            "info|history|empty",
            "No review history yet",
        )]
    } else {
        entries
            .iter()
            .take(HISTORY_LIMIT)
            .enumerate()
            .map(|(n, entry)| {
                ModelItem::info(format!("info|history|{n}"), history_line(entry, now))
                    .with_symbol(history_symbol(entry))
            })
            .collect()
    };
    items.push(ModelItem::separator());
    items.push(ModelItem::new(
        actions::OPEN_HISTORY_ID,
        "Open history in PRX",
        true,
        ItemKind::Action,
    ));
    items
}

/// A history row's symbol: the state the event put the PR in, from the
/// same vocabulary as the PR rows.
fn history_symbol(entry: &HistoryEntry) -> Symbol {
    match entry {
        HistoryEntry::ReviewStarted { .. } => Symbol::InReview,
        HistoryEntry::ReviewCompleted { verdict, .. } => verdict_symbol(Some(*verdict)),
        HistoryEntry::ReviewFailed { .. } => Symbol::Failed,
        HistoryEntry::ReviewSkipped { .. } => Symbol::Skipped,
        HistoryEntry::PrMerged { .. } => Symbol::Merged,
        HistoryEntry::PrClosed { .. } => Symbol::Closed,
        HistoryEntry::NewCommits { .. } => Symbol::ReReview,
        HistoryEntry::Brief { brief: Some(_), .. } => Symbol::Brief,
        HistoryEntry::Brief { brief: None, .. } => Symbol::Failed,
    }
}

/// One history entry as a summary line: PR, what happened, and how long ago
/// (relative, so never a raw UTC timestamp); the event is the row's symbol.
fn history_line(entry: &HistoryEntry, now: DateTime<Utc>) -> String {
    let what = match entry {
        HistoryEntry::ReviewCompleted {
            verdict,
            blocking_count,
            nit_count,
            posted,
            ..
        } => {
            let verdict = match verdict {
                Verdict::Approved => "Approved",
                Verdict::ChangesRequested => "Changes requested",
                Verdict::CommentOnly => "Commented",
            };
            // Design D4: a run that made no GitHub writes says so.
            let not_posted = if *posted == Some(false) {
                " (not posted)"
            } else {
                ""
            };
            format!(
                "{verdict}{not_posted} — {blocking_count} blocking, {}",
                count(*nit_count as usize, "nit")
            )
        }
        HistoryEntry::ReviewStarted { .. } => "Review started".to_string(),
        HistoryEntry::ReviewFailed { .. } => "Review failed".to_string(),
        HistoryEntry::ReviewSkipped { reason, .. } => format!("Review skipped: {reason}"),
        HistoryEntry::PrMerged { .. } => "Merged".to_string(),
        HistoryEntry::PrClosed { .. } => "Closed".to_string(),
        HistoryEntry::NewCommits { .. } => "New commits — re-review pending".to_string(),
        HistoryEntry::Brief { brief: Some(_), .. } => "Brief written".to_string(),
        HistoryEntry::Brief { brief: None, .. } => "Brief failed".to_string(),
    };
    format!(
        "{}#{} — {} \u{b7} {} ago",
        entry.repo(),
        entry.number(),
        what,
        compact_age(entry.ts(), now)
    )
}

use prx::state::Verdict;

/// "1 nit", "3 nits".
fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", plural(n))
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn entry(key: &str, state: PrState, title: &str, summary: &str) -> StatusEntry {
        let (skip_reason, verdict) = match state {
            PrState::Skipped => (Some(SkipReason::Snoozed), None),
            PrState::ReviewedApproved => (None, Some(Verdict::Approved)),
            PrState::ReviewedChangesRequested => (None, Some(Verdict::ChangesRequested)),
            _ => (None, None),
        };
        StatusEntry {
            key: key.to_string(),
            icon: "•".to_string(),
            state,
            skip_reason,
            title: title.to_string(),
            author: "alice".to_string(),
            head_branch: "f/x".to_string(),
            is_draft: false,
            last_verdict: verdict,
            blocking_count: 1,
            nit_count: 2,
            last_review_at: None,
            summary: summary.to_string(),
            last_review_age: "never".to_string(),
            created_at: None,
            identity_login: None,
            brief: None,
            brief_pending: false,
            brief_error: None,
            brief_available: true,
            html_url: String::new(),
            human_approved_at: None,
            engine: None,
        }
    }

    /// Fixed "now" so ages are deterministic.
    fn now() -> DateTime<Utc> {
        "2026-10-05T12:00:00Z".parse().unwrap()
    }

    /// One-shot build with fresh pagination (page 1 everywhere).
    fn build(status: &StatusResponse, history: &[HistoryEntry]) -> MenuModel {
        build_model(status, history, &mut PaginationState::default(), now())
    }

    fn status(prs: Vec<StatusEntry>) -> StatusResponse {
        StatusResponse {
            version: "0.1.0".to_string(),
            pid: 42,
            uptime_seconds: 7,
            in_flight: 0,
            queue_depth: 0,
            last_poll: Default::default(),
            last_state_poll: None,
            next_poll: None,
            prs,
            config_error: None,
            review_mode: Default::default(),
            paused_until: None,
            engines: vec![],
            menu: Default::default(),
        }
    }

    fn history_entry(repo: &str, number: u64) -> HistoryEntry {
        HistoryEntry::from_effect(
            &prx::machine::HistoryEvent::ReviewCompleted {
                verdict: Verdict::Approved,
                blocking_count: 0,
                nit_count: 1,
            },
            &sample_record(repo, number),
            Some("pi"),
            Some(30),
            Vec::new(),
            Utc::now(),
        )
    }

    fn sample_record(repo: &str, number: u64) -> prx::state::PrRecord {
        let now = Utc::now();
        prx::state::PrRecord {
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
            head_branch: "b".to_string(),
            head_sha: "s".to_string(),
            is_draft: false,
            gh_created_at: now,
            state: PrState::Pending,
            previous_state: None,
            skip_reason: None,
            snooze_until: None,
            snooze_until_push: false,
            review_anchor_at: Some(now),
            last_reviewed_sha: None,
            last_review_at: None,
            last_verdict: None,
            last_summary: None,
            blocking_count: 0,
            nit_count: 0,
            review_session: None,
            review_attempts: 0,
            diff_too_large: false,
            human_approved_at: None,
            exec_summary: None,
            brief: None,
            updated_at: now,
        }
    }

    /// PR-row labels under the named group header.
    fn group_labels(model: &MenuModel, group_slug: &str) -> Vec<String> {
        let header_id = format!("info|group|{group_slug}");
        let mut out = Vec::new();
        let mut under_header = false;
        for item in model.items() {
            if item.kind == ItemKind::GroupHeader {
                under_header = item.id == header_id;
                continue;
            }
            if under_header && item.kind == ItemKind::Pr {
                out.push(item.label.clone());
            }
        }
        out
    }

    /// 4.4: the status payload carries prx's identity login per PR, which
    /// is what lets the model choose Unblock over Block for a PR prx holds
    /// a request-changes review on (unknown identity → Block).
    #[test]
    fn payload_identity_distinguishes_block_from_unblock() {
        let mut blocked = entry("o/r#1", PrState::ReviewedChangesRequested, "t", "s");
        assert!(!blocked.offers_unblock(), "unknown identity offers Block");
        blocked.identity_login = Some("prx-bot".to_string());
        assert!(blocked.offers_unblock());
        let mut pending = entry("o/r#2", PrState::Pending, "t", "s");
        pending.identity_login = Some("prx-bot".to_string());
        assert!(!pending.offers_unblock());
        let payload = status(vec![blocked, pending]);
        let offers: Vec<bool> = payload.prs.iter().map(|e| e.offers_unblock()).collect();
        assert_eq!(offers, [true, false]);
    }

    #[test]
    fn every_lifecycle_state_maps_onto_its_group() {
        let model = build(
            &status(vec![
                entry(
                    "org/r#1",
                    PrState::Pending,
                    "pending one",
                    "ready for review",
                ),
                entry(
                    "org/r#2",
                    PrState::InReview,
                    "in review one",
                    "review in progress",
                ),
                entry(
                    "org/r#3",
                    PrState::ReviewedApproved,
                    "approved one",
                    "approved",
                ),
                entry(
                    "org/r#4",
                    PrState::ReviewedChangesRequested,
                    "changes one",
                    "changes requested: 1 blocking",
                ),
                entry("org/r#5", PrState::Skipped, "skipped one", "snoozed"),
                entry("org/r#6", PrState::Merged, "merged one", "merged"),
                entry("org/r#7", PrState::Closed, "closed one", "closed"),
            ]),
            &[],
        );

        let needs = group_labels(&model, "Needs-review");
        let in_review = group_labels(&model, "In-review");
        let changes = group_labels(&model, "Changes-requested");
        let reviewed = group_labels(&model, "Reviewed");
        let skipped = group_labels(&model, "Skipped");
        let terminal = group_labels(&model, "Terminal");

        assert_eq!(needs.len(), 1, "{needs:?}");
        assert!(needs[0].contains("org/r#1"), "{needs:?}");
        assert!(needs[0].contains("pending one"));
        assert!(needs[0].contains("ready for review"));
        assert_eq!(in_review.len(), 1);
        assert!(in_review[0].contains("org/r#2"));
        // 8.1: an agent-approved PR is pulled into the ready group.
        let ready = group_labels(&model, "Ready-for-approval");
        assert_eq!(ready.len(), 1, "{ready:?}");
        assert!(ready[0].contains("org/r#3"));
        // Changes requested gets its own group (half the attention count).
        assert_eq!(changes.len(), 1, "{changes:?}");
        assert!(changes[0].contains("org/r#4"));
        assert!(reviewed.is_empty(), "{reviewed:?}");
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("org/r#5"));
        assert_eq!(terminal.len(), 2, "{terminal:?}");
        assert!(terminal.iter().any(|l| l.contains("org/r#6")));
        assert!(terminal.iter().any(|l| l.contains("org/r#7")));
        assert!(!model.offline);
    }

    #[test]
    fn attention_count_is_changes_requested_plus_in_review() {
        let model = build(
            &status(vec![
                entry("org/r#1", PrState::InReview, "a", "s"),
                entry("org/r#2", PrState::InReview, "b", "s"),
                entry(
                    "org/r#3",
                    PrState::ReviewedChangesRequested,
                    "c",
                    "changes requested",
                ),
                entry("org/r#4", PrState::Pending, "d", "ready"),
                entry("org/r#5", PrState::ReviewedApproved, "e", "ok"),
                entry("org/r#6", PrState::Skipped, "f", "snoozed"),
                entry("org/r#7", PrState::Merged, "g", "merged"),
            ]),
            &[],
        );
        assert_eq!(model.attention, 3);
    }

    #[test]
    fn zero_attention_when_nothing_needs_it() {
        let model = build(
            &status(vec![
                entry("org/r#1", PrState::Pending, "a", "ready"),
                entry("org/r#2", PrState::ReviewedApproved, "b", "ok"),
            ]),
            &[],
        );
        assert_eq!(model.attention, 0);
        let headline = model
            .items()
            .find(|i| i.id == "info|attention")
            .expect("attention banner present");
        assert_eq!(headline.label, "All clear · not checked yet");
        assert_eq!(headline.symbol, Some(Symbol::AllClear));
    }

    #[test]
    fn offline_model_has_no_actionable_prs_and_disables_mutations() {
        let model = offline_model();
        assert!(model.offline);
        assert_eq!(model.attention, 0);
        assert!(
            model.items().all(|i| i.kind != ItemKind::Pr),
            "no PR rows may render while offline"
        );
        let offline_item = model
            .items()
            .find(|i| i.label.contains("Daemon offline"))
            .expect("clear offline indicator");
        assert!(!offline_item.enabled, "offline indicator must be disabled");
        // Every item is either decoration (disabled) or an always-safe
        // non-daemon action (Quit is local, Open PRX shows the window's
        // offline state, the supervision items are the app's own). The
        // daemon-backed actions (Check GitHub now / Review PR… /
        // Settings… / pause / mode / History) are absent entirely: the
        // daemon cannot honor them. No refresh item: opening the menu
        // already re-probes the daemon.
        for item in model.items() {
            match item.kind {
                ItemKind::Info
                | ItemKind::GroupHeader
                | ItemKind::History
                | ItemKind::Separator => {
                    assert!(!item.enabled, "{} must be disabled", item.id);
                }
                ItemKind::Action if item.id == actions::OPEN_WINDOW_ID => {
                    assert!(item.enabled, "Open PRX must stay available offline")
                }
                ItemKind::Pr | ItemKind::Action | ItemKind::Mode => {
                    panic!("no PR/action/mode rows offline: {}", item.id)
                }
                ItemKind::Quit | ItemKind::Supervise => {
                    assert!(item.enabled, "{} must stay available", item.id)
                }
            }
        }
    }

    fn pending(n: u64) -> StatusEntry {
        entry(
            &format!("org/r#{n:03}"),
            PrState::Pending,
            &format!("pr {n}"),
            "ready for review",
        )
    }

    fn find<'a>(model: &'a MenuModel, id: &str) -> Option<&'a ModelItem> {
        model.items().find(|i| i.id == id)
    }

    /// 6.1: author and compact age sit between the key and the title.
    #[test]
    fn pr_row_shows_author_and_age() {
        let mut e = entry("org/r#1", PrState::Pending, "fix bug", "ready");
        e.created_at = Some(now() - chrono::Duration::days(2));
        let model = build(&status(vec![e]), &[]);
        let row = model.items().find(|i| i.kind == ItemKind::Pr).unwrap();
        assert_eq!(row.label, "org/r#1 · alice · 2d — fix bug — ready");
        assert_eq!(row.symbol, Some(Symbol::NeedsReview));
    }

    #[test]
    fn pr_row_without_created_at_omits_the_age_segment() {
        let model = build(
            &status(vec![entry("org/r#1", PrState::Pending, "fix bug", "ready")]),
            &[],
        );
        let row = model.items().find(|i| i.kind == ItemKind::Pr).unwrap();
        assert_eq!(row.label, "org/r#1 · alice — fix bug — ready");
    }

    #[test]
    fn compact_age_units() {
        let ago = |minutes: i64| compact_age(now() - chrono::Duration::minutes(minutes), now());
        assert_eq!(ago(0), "1m");
        assert_eq!(ago(-30), "1m", "future timestamps count as fresh");
        assert_eq!(ago(45), "45m");
        assert_eq!(ago(2 * 60 + 59), "2h");
        assert_eq!(ago(23 * 60), "23h");
        assert_eq!(ago(3 * 24 * 60), "3d");
        assert_eq!(ago(6 * 24 * 60 + 23 * 60), "6d");
        assert_eq!(ago(5 * 7 * 24 * 60 + 60), "5w");
    }

    /// 6.2: every group (not just reviewed/skipped) shows its first page
    /// followed by an enabled Show more; no Show less on page 1.
    #[test]
    fn first_page_renders_with_enabled_show_more() {
        let mut prs: Vec<StatusEntry> = (0..23).map(pending).collect();
        prs.push(entry("org/t#1", PrState::Merged, "m", "merged"));
        let model = build(&status(prs), &[]);
        let needs = group_labels(&model, "Needs-review");
        assert_eq!(needs.len(), PAGE_SIZE);
        assert!(needs[0].contains("org/r#000") && needs[9].contains("org/r#009"));
        let more = find(&model, "page:more|Needs-review").expect("Show more");
        assert!(more.enabled);
        assert_eq!(more.kind, ItemKind::Action);
        assert_eq!(more.label, "Show more (13 hidden)");
        assert!(find(&model, "page:less|Needs-review").is_none());
        // A group within one page gets neither item; the static expander is gone.
        assert_eq!(group_labels(&model, "Terminal").len(), 1);
        assert!(find(&model, "page:more|Terminal").is_none());
        assert!(model.items().all(|i| !i.id.starts_with("info|more")));
    }

    #[test]
    fn show_more_reveals_next_page_and_show_less_collapses() {
        let payload = status((0..23).map(pending).collect());
        let mut pages = PaginationState::default();
        build_model(&payload, &[], &mut pages, now());

        pages.more("Needs-review");
        let model = build_model(&payload, &[], &mut pages, now());
        let needs = group_labels(&model, "Needs-review");
        assert_eq!(needs.len(), 20, "second page appears in place");
        assert!(needs[19].contains("org/r#019"));
        assert_eq!(
            find(&model, "page:more|Needs-review").unwrap().label,
            "Show more (3 hidden)"
        );
        assert!(find(&model, "page:less|Needs-review").unwrap().enabled);

        pages.more("Needs-review");
        let model = build_model(&payload, &[], &mut pages, now());
        assert_eq!(group_labels(&model, "Needs-review").len(), 23);
        assert!(
            find(&model, "page:more|Needs-review").is_none(),
            "all shown"
        );
        assert!(find(&model, "page:less|Needs-review").is_some());

        // Extra clicks past the end clamp instead of accumulating.
        pages.more("Needs-review");
        build_model(&payload, &[], &mut pages, now());
        assert_eq!(pages.pages("Needs-review"), 3);

        pages.less("Needs-review");
        let model = build_model(&payload, &[], &mut pages, now());
        assert_eq!(group_labels(&model, "Needs-review").len(), PAGE_SIZE);
        assert!(find(&model, "page:less|Needs-review").is_none());
    }

    #[test]
    fn arriving_pr_resets_the_group_to_its_first_page() {
        let mut pages = PaginationState::default();
        let before = status((0..23).map(pending).collect());
        build_model(&before, &[], &mut pages, now());
        pages.more("Needs-review");
        pages.more("Needs-review");
        // An unchanged key-set keeps the expanded window.
        let model = build_model(&before, &[], &mut pages, now());
        assert_eq!(group_labels(&model, "Needs-review").len(), 23);

        // A new PR sorting first arrives: the window resets to page 1 and the
        // arrival is on it.
        let mut prs: Vec<StatusEntry> = (0..23).map(pending).collect();
        prs.push(entry("org/a#1", PrState::Pending, "new", "ready"));
        let model = build_model(&status(prs), &[], &mut pages, now());
        let needs = group_labels(&model, "Needs-review");
        assert_eq!(needs.len(), PAGE_SIZE, "reset to the first page");
        assert!(needs[0].contains("org/a#1"), "{needs:?}");
        assert!(find(&model, "page:less|Needs-review").is_none());
    }

    #[test]
    fn item_ids_encode_action_and_pr_key() {
        let model = build(
            &status(vec![entry(
                "org/repo#13",
                PrState::ReviewedChangesRequested,
                "fix bug",
                "changes requested: 2 blocking",
            )]),
            &[],
        );
        let row = model
            .items()
            .find(|i| i.kind == ItemKind::Pr)
            .expect("PR row present");
        assert_eq!(row.id, "pr|org/repo#13");
        assert!(row.enabled);
        let open = row
            .children
            .iter()
            .find(|c| c.id == "open|org/repo#13")
            .expect("Open in GitHub entry");
        assert!(open.enabled);
        assert_eq!(open.label, "Open in GitHub");
    }

    #[test]
    fn every_pr_row_offers_the_full_action_submenu() {
        let model = build(
            &status(vec![entry(
                "org/repo#13",
                PrState::Pending,
                "fix bug",
                "ready for review",
            )]),
            &[],
        );
        let row = model
            .items()
            .find(|i| i.kind == ItemKind::Pr)
            .expect("PR row present");
        let ids: Vec<&str> = row.children.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "open|org/repo#13",
                "separator",
                "brief|org/repo#13",
                "separator",
                "review|org/repo#13",
                "review-nopost|org/repo#13",
                "review-prompt|org/repo#13",
                "separator",
                "approve|org/repo#13",
                "block|org/repo#13",
                "separator",
                "snooze",
                "ignore|org/repo#13",
            ],
            "open / write brief / review / no-post / prompt / approve / block / snooze / \
             ignore (no engine list: no Review with)"
        );
        let snooze = &row.children[11];
        assert_eq!(
            snooze
                .children
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "snooze:1|org/repo#13",
                "snooze:2|org/repo#13",
                "snooze:4|org/repo#13",
                "snooze:push|org/repo#13",
            ],
            "1h / 2h / 4h / until push"
        );
        assert!(row
            .children
            .iter()
            .all(|c| c.enabled || c.kind == ItemKind::Separator));
    }

    #[test]
    fn snoozed_prs_show_unsnooze_and_ignored_prs_show_unignore() {
        let mut ignored = entry("org/repo#1", PrState::Skipped, "ignored one", "ignored");
        ignored.skip_reason = Some(SkipReason::Ignored);
        let snoozed = entry("org/repo#2", PrState::Skipped, "snoozed one", "snoozed");

        let model = build(&status(vec![ignored, snoozed]), &[]);
        let rows: Vec<&ModelItem> = model.items().filter(|i| i.kind == ItemKind::Pr).collect();
        let ignored_row = rows
            .iter()
            .find(|r| r.id == "pr|org/repo#1")
            .expect("ignored row");
        assert!(
            ignored_row
                .children
                .iter()
                .any(|c| c.id == "unignore|org/repo#1"),
            "ignored PR offers Un-ignore"
        );
        assert!(
            !ignored_row
                .children
                .iter()
                .any(|c| c.id == "ignore|org/repo#1"),
            "ignored PR does not offer Ignore again"
        );
        assert!(
            !ignored_row
                .children
                .iter()
                .any(|c| c.id == "unsnooze|org/repo#1"),
            "ignored PR does not offer Un-snooze"
        );
        let snoozed_row = rows
            .iter()
            .find(|r| r.id == "pr|org/repo#2")
            .expect("snoozed row");
        let unsnooze = snoozed_row
            .children
            .iter()
            .find(|c| c.id == "unsnooze|org/repo#2")
            .expect("snoozed PR offers Un-snooze");
        assert_eq!(unsnooze.label, "Unsnooze");
        assert!(unsnooze.enabled, "a snoozed PR is not terminal");
        assert!(
            !snoozed_row
                .children
                .iter()
                .any(|c| c.id == "ignore|org/repo#2"),
            "snoozed PR offers Un-snooze instead of Ignore"
        );
        assert!(
            !snoozed_row
                .children
                .iter()
                .any(|c| c.id == "unignore|org/repo#2"),
            "snoozed PR does not offer Un-ignore"
        );
    }

    #[test]
    fn terminal_prs_disable_mutating_actions_but_keep_open() {
        let model = build(
            &status(vec![entry("org/repo#9", PrState::Merged, "m", "merged")]),
            &[],
        );
        let row = model
            .items()
            .find(|i| i.kind == ItemKind::Pr)
            .expect("merged row present");
        let open = row
            .children
            .iter()
            .find(|c| c.id == "open|org/repo#9")
            .expect("open entry");
        assert!(open.enabled, "Open stays available on a merged PR");
        // Briefs work in any tracked state (task 4.1): Write brief too.
        let write = format!("{}|org/repo#9", actions::BRIEF_PREFIX);
        assert!(row.children.iter().any(|c| c.id == write && c.enabled));
        for child in &row.children {
            if child.id != open.id && child.id != write {
                assert!(!child.enabled, "{} must be disabled on terminal", child.id);
            }
            for grandchild in &child.children {
                assert!(
                    !grandchild.enabled,
                    "{grandchild_id} must be disabled on terminal",
                    grandchild_id = grandchild.id
                );
            }
        }
    }

    fn ids(items: &[ModelItem]) -> Vec<&str> {
        items.iter().map(|i| i.id.as_str()).collect()
    }

    fn shortcut(model: &MenuModel, id: &str) -> Option<Shortcut> {
        find(model, id).and_then(|i| i.shortcut)
    }

    /// Design-audit Part 0: header (attention), quick actions, PR groups,
    /// automatic reviews, the app with About, then Quit — with ⌘ shortcuts
    /// on the everyday items and no diagnostics outside About.
    #[test]
    fn menu_sections_follow_the_redesign() {
        let model = build(&status(vec![pending(1)]), &[]);
        let n = model.sections.len();
        assert_eq!(n, 6, "header, quick, one group, reviews, app, quit");
        // Round 2: one header line (its ⌥ alternate is the diagnostics).
        assert_eq!(
            ids(&model.sections[0].items),
            ["info|attention", "info|about"]
        );
        assert_eq!(
            ids(&model.sections[1].items),
            [actions::REVIEW_NOW_ID, actions::REVIEW_PR_ID]
        );
        assert_eq!(
            model.sections[1]
                .items
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            ["Check GitHub now", "Review PR…"]
        );
        assert_eq!(model.sections[2].items[0].kind, ItemKind::GroupHeader);
        assert_eq!(
            ids(&model.sections[n - 3].items),
            [actions::MODE_PARENT_ID],
            "one submenu for the mode and the pause (round 3)"
        );
        // The app: Open PRX (⌥ Show daemon log), Settings… (⌥ Open
        // config.yml), then History ▸ after Settings….
        let app = &model.sections[n - 2].items;
        assert_eq!(
            ids(app),
            [
                actions::OPEN_WINDOW_ID,
                SHOW_LOG_ID,
                actions::SETTINGS_ID,
                actions::OPEN_CONFIG_ID,
                actions::HISTORY_ID
            ]
        );
        assert_eq!(
            app.iter()
                .map(|i| (i.label.as_str(), i.alternate))
                .collect::<Vec<_>>(),
            [
                ("Open PRX", false),
                ("Show daemon log", true),
                ("Settings…", false),
                ("Open config.yml", true),
                ("History", false)
            ]
        );
        assert_eq!(ids(&model.sections[n - 1].items), [QUIT_ID, QUIT_KEEP_ID]);
        assert!(model
            .sections
            .iter()
            .flat_map(|s| &s.items)
            .all(|i| i.kind == ItemKind::GroupHeader || i.kind == ItemKind::Info || i.enabled));
        // The ⌥ alternates: the diagnostics, the troubleshooting
        // shortcuts, and the keep-reviewing Quit — nothing else.
        assert_eq!(
            model
                .items()
                .filter(|i| i.alternate)
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            [
                "info|about",
                SHOW_LOG_ID,
                actions::OPEN_CONFIG_ID,
                QUIT_KEEP_ID
            ]
        );
        // Shortcuts: one per everyday command, distinct; each alternate
        // is its base's key with ⌥.
        assert_eq!(shortcut(&model, actions::REVIEW_NOW_ID), Some(cmd('r')));
        assert_eq!(shortcut(&model, actions::REVIEW_PR_ID), Some(cmd('n')));
        assert_eq!(shortcut(&model, actions::OPEN_WINDOW_ID), Some(cmd('o')));
        assert_eq!(shortcut(&model, SHOW_LOG_ID), Some(cmd_option('o')));
        assert_eq!(shortcut(&model, actions::SETTINGS_ID), Some(cmd(',')));
        assert_eq!(
            shortcut(&model, actions::OPEN_CONFIG_ID),
            Some(cmd_option(','))
        );
        assert_eq!(shortcut(&model, QUIT_ID), Some(cmd('q')));
        assert_eq!(shortcut(&model, QUIT_KEEP_ID), Some(cmd_option('q')));
        let keys: Vec<(char, bool)> = model
            .items()
            .filter_map(|i| i.shortcut)
            .map(|s| (s.key, s.option))
            .collect();
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len(), "no shortcut is used twice");

        // Offline: the same order for what it can offer.
        let offline = offline_model();
        assert_eq!(offline.sections.len(), 4);
        assert_eq!(
            ids(&offline.sections[0].items),
            ["info|attention", "info|about"]
        );
        assert_eq!(
            offline.sections[0].items[1].label,
            format!("PRX {APP_VERSION} · daemon not running")
        );
        assert_eq!(
            ids(&offline.sections[1].items),
            [START_DAEMON_ID, SHOW_LOG_ID]
        );
        assert_eq!(ids(&offline.sections[2].items), [actions::OPEN_WINDOW_ID]);
        assert_eq!(ids(&offline.sections[3].items), [QUIT_ID, QUIT_KEEP_ID]);
    }

    /// There is exactly one refresh-like item: "Check GitHub now" (the
    /// daemon polls GitHub). The old offline "Refresh now" (re-read the
    /// daemon) is gone — opening the menu already does that.
    #[test]
    fn one_refresh_item_with_a_distinct_meaning() {
        let online = build(&status(vec![pending(1)]), &[]);
        let refreshes: Vec<&str> = online
            .items()
            .filter(|i| {
                let l = i.label.to_lowercase();
                l.contains("refresh") || l.contains("check github") || l.contains("reload")
            })
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(refreshes, [actions::REVIEW_NOW_ID]);
        for model in [offline_model(), online] {
            assert!(model.items().all(|i| !i.label.contains("Refresh")));
        }
    }

    /// Round 2: no About submenu. The daemon's diagnostics are the
    /// headline's ⌥ alternate; a health line appears only when the daemon
    /// is unhealthy, and its click is the fix.
    #[test]
    fn diagnostics_are_the_headlines_option_alternate() {
        let mut payload = status(vec![pending(1)]);
        payload.queue_depth = 2;
        payload.in_flight = 1;
        payload.version = APP_VERSION.to_string();
        let model = build(&payload, &[]);
        let header = &model.sections[0].items;
        // A healthy daemon costs no row: the headline, then (⌥) its
        // diagnostics.
        assert_eq!(ids(header), ["info|attention", "info|about"]);
        for word in ["pid", "UTC", "daemon", "version"] {
            assert!(!header[0].label.contains(word), "{}", header[0].label);
        }
        let about = &header[1];
        assert!(about.alternate && !about.enabled, "{about:?}");
        assert_eq!(
            about.label,
            format!(
                "PRX {APP_VERSION} · daemon {APP_VERSION} · pid 42 · up 1m · 1 PR · 1 reviewing · 2 queued"
            )
        );
        assert!(model.items().all(|i| i.id != "about"), "no About submenu");

        // Unhealthy: a version mismatch adds a line whose click opens the log.
        payload.version = "9.9.9".into();
        let skewed = build(&payload, &[]);
        let health = &skewed.sections[0].items[2];
        assert_eq!(
            (health.id.as_str(), health.enabled, health.symbol),
            (SHOW_LOG_ID, true, Some(Symbol::Warning))
        );
        assert_eq!(
            health.label,
            format!("Daemon 9.9.9 is running, app is {APP_VERSION} — Show log")
        );

        // A config error wins the line and opens config.yml.
        payload.config_error = Some(prx::ipc::ConfigErrorInfo {
            message: "defaults.review_mode: unknown variant `loud`".into(),
            at: now(),
        });
        let broken = build(&payload, &[]);
        let header = ids(&broken.sections[0].items);
        assert_eq!(
            header,
            ["info|attention", "info|about", actions::OPEN_CONFIG_ID]
        );
        assert_eq!(
            broken.sections[0].items[2].label,
            "config.yml not loaded: defaults.review_mode: unknown variant `loud` — Open…"
        );
        // An action message goes under it.
        let both = broken.with_message("⚠ daemon refused");
        assert_eq!(
            ids(&both.sections[0].items),
            [
                "info|attention",
                "info|about",
                actions::OPEN_CONFIG_ID,
                "info|message"
            ]
        );
    }

    /// Task 7.7: the status header says when PRX checked GitHub and checks
    /// next, in relative time (never a raw UTC timestamp or the pid).
    #[test]
    fn status_header_shows_relative_check_times() {
        let entries = [history_entry("org/one", 7), history_entry("org/two", 9)];
        let mut payload = status(vec![]);
        payload.last_state_poll = Some(now() - chrono::Duration::minutes(2));
        payload.next_poll = Some(now() + chrono::Duration::minutes(13));
        let model = build(&payload, &entries);
        let header = &model.sections[0].items;
        // Round 2: the headline carries the timing — one line.
        assert_eq!(ids(header), ["info|attention", "info|about"]);
        assert_eq!(
            header[0].label,
            "No PRs tracked yet · checked 2m ago · next in 13m"
        );
        for item in model.items().filter(|i| i.id != "info|about") {
            assert!(!item.label.contains("pid"), "{}", item.label);
            assert!(!item.label.contains("UTC"), "{}", item.label);
        }
        // Never checked, and nothing planned yet.
        let fresh = build(&status(vec![]), &[]);
        assert_eq!(
            fresh.sections[0].items[0].label,
            "No PRs tracked yet · not checked yet"
        );
        // Reviews off: no "next" (the daemon doesn't poll).
        payload.review_mode = ReviewMode::Off;
        let off = build(&payload, &[]);
        assert_eq!(
            off.sections[0].items[0].label,
            "Reviews off · checked 2m ago"
        );
        // A due check reads as running now.
        payload.review_mode = ReviewMode::ReviewComment;
        payload.next_poll = Some(now() - chrono::Duration::seconds(5));
        let due = build(&payload, &[]);
        assert_eq!(
            due.sections[0].items[0].label,
            "No PRs tracked yet · checked 2m ago · checking now"
        );

        let history = model
            .items()
            .find(|i| i.id == "history")
            .expect("history submenu");
        assert_eq!(
            history.children.len(),
            4,
            "one row per entry, a separator, Open history in PRX"
        );
        for (row, entry) in history.children.iter().zip(entries.iter()) {
            assert!(!row.enabled, "history rows are decoration");
            assert!(row
                .label
                .starts_with(&format!("{}#{}", entry.repo(), entry.number())));
            assert!(row.label.contains("Approved"), "{}", row.label);
            assert!(row.label.ends_with(" ago"), "{}", row.label);
            assert_eq!(row.symbol, Some(Symbol::Approved));
        }
        let open = history.children.last().unwrap();
        assert_eq!(
            (open.id.as_str(), open.label.as_str(), open.enabled),
            (actions::OPEN_HISTORY_ID, "Open history in PRX", true)
        );
    }

    #[test]
    fn history_submenu_renders_new_commits_as_re_review_pending() {
        let entry = HistoryEntry::NewCommits {
            ts: Utc::now(),
            repo: "org/one".to_string(),
            number: 7,
            title: "t".to_string(),
            author: "a".to_string(),
        };
        let model = build(&status(vec![]), &[entry]);
        let row = model
            .items()
            .find(|i| i.id == "info|history|0")
            .expect("history row present");
        assert!(!row.enabled, "history rows are decoration");
        assert!(row.label.contains("org/one#7"), "{}", row.label);
        assert!(
            row.label.contains("New commits — re-review pending"),
            "{}",
            row.label
        );
    }

    #[test]
    fn history_submenu_caps_at_ten_and_handles_empty() {
        let entries: Vec<HistoryEntry> = (0..13u64).map(|n| history_entry("org/one", n)).collect();
        let model = build(&status(vec![]), &entries);
        let history = model.items().find(|i| i.id == "history").unwrap();
        let rows = |h: &ModelItem| {
            h.children
                .iter()
                .filter(|c| c.id.starts_with("info|history"))
                .count()
        };
        assert_eq!(rows(history), HISTORY_LIMIT);

        let model = build(&status(vec![]), &[]);
        let history = model.items().find(|i| i.id == "history").unwrap();
        assert_eq!(rows(history), 1);
        assert_eq!(history.children[0].label, "No review history yet");
        assert_eq!(
            history.children.last().unwrap().id,
            actions::OPEN_HISTORY_ID
        );
    }

    #[test]
    fn empty_status_renders_empty_notice_without_group_headers() {
        let model = build(&status(vec![]), &[]);
        assert!(!model.offline);
        assert_eq!(model.attention, 0);
        assert!(model
            .items()
            .any(|i| i.label.starts_with("No PRs tracked yet · ")));
        assert!(
            model.items().all(|i| i.kind != ItemKind::GroupHeader),
            "no empty group headers"
        );
    }

    #[test]
    fn dump_lists_items_and_flags() {
        let model = offline_model();
        let dump = model.dump();
        assert!(dump.contains("offline=true"), "{dump}");
        assert!(dump.contains("Quit PRX"), "{dump}");
        assert!(dump.contains("info|about"), "{dump}");
        assert!(dump.contains("(⌥) <info.circle> PRX"), "{dump}");
        assert!(dump.contains("<power> Quit PRX"), "{dump}");
    }

    #[test]
    fn dump_indents_submenu_children() {
        let model = build(
            &status(vec![entry("org/r#1", PrState::Pending, "a", "ready")]),
            &[],
        );
        let dump = model.dump();
        assert!(dump.contains("pr|org/r#1"), "{dump}");
        assert!(dump.contains("      snooze:2|org/r#1"), "{dump}");
    }

    #[test]
    fn message_line_inserts_after_the_headline() {
        let model = build(&status(vec![]), &[]).with_message("already approved — nothing to do");
        let ids: Vec<&str> = model.sections[0]
            .items
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        // Right below the headline and its ⌥ alternate: two lines at most.
        assert_eq!(ids, ["info|attention", "info|about", "info|message"]);
        let message = model
            .items()
            .find(|i| i.id == "info|message")
            .expect("message line");
        assert!(!message.enabled, "message is a disabled info item");
        assert_eq!(message.label, "already approved — nothing to do");
        assert_eq!(message.symbol, Some(Symbol::Info));
        // The publishers' ✓ / ⚠ prefixes become the symbol.
        for (raw, label, symbol) in [
            ("✓ snoozed org/r#1", "snoozed org/r#1", Symbol::Success),
            ("⚠ daemon refused", "daemon refused", Symbol::Warning),
        ] {
            let model = build(&status(vec![]), &[]).with_message(raw);
            let message = find(&model, "info|message").unwrap();
            assert_eq!(
                (message.label.as_str(), message.symbol),
                (label, Some(symbol))
            );
        }
    }

    fn mode_rows(model: &MenuModel) -> Vec<(&str, bool)> {
        model
            .items()
            .filter(|i| i.kind == ItemKind::Mode)
            .map(|i| (i.id.as_str(), i.checked))
            .collect()
    }

    /// 7.1 / Part 0: the radio group leads the "Review mode: <current>"
    /// submenu (the pause section follows it), each row says what the mode
    /// does to GitHub, and the checkmark follows the payload's global mode.
    #[test]
    fn mode_checkmark_follows_the_payload() {
        for (mode, checked_id, title) in [
            (ReviewMode::Off, "mode:off", "Review mode: Off"),
            (ReviewMode::Monitor, "mode:monitor", "Review mode: Monitor"),
            (
                ReviewMode::ReviewComment,
                "mode:comment",
                "Review mode: Review and comment",
            ),
        ] {
            let mut payload = status(vec![pending(1)]);
            payload.review_mode = mode;
            let model = build(&payload, &[]);
            let rows = mode_rows(&model);
            assert_eq!(
                rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                ["mode:comment", "mode:monitor", "mode:off"]
            );
            let checked: Vec<&str> = rows.iter().filter(|r| r.1).map(|r| r.0).collect();
            assert_eq!(checked, [checked_id], "{mode:?}");
            assert!(model
                .items()
                .filter(|i| i.kind == ItemKind::Mode)
                .all(|i| i.enabled));
            let parent = find(&model, actions::MODE_PARENT_ID).expect("mode submenu");
            assert_eq!(parent.label, title, "current mode at a glance");
            assert_eq!(
                parent.children[..8]
                    .iter()
                    .map(|c| c.label.as_str())
                    .collect::<Vec<_>>(),
                [
                    "Default for repos without their own mode",
                    "",
                    "Review and comment — reviews and posts to GitHub",
                    "Monitor — reviews, never posts",
                    "Off — no checks, no reviews",
                    "",
                    "Per-repo modes…",
                    "",
                ]
            );
            assert_eq!(parent.children[6].id, actions::OPEN_REPOS_ID);
            let symbol = if mode == ReviewMode::Off {
                Symbol::Paused
            } else {
                Symbol::Mode
            };
            assert_eq!(parent.symbol, Some(symbol), "{mode:?}");
            let n = model.sections.len();
            assert!(model.sections[n - 3]
                .items
                .iter()
                .any(|i| i.id == actions::MODE_PARENT_ID));
            assert!(model.sections[n - 1].items.iter().any(|i| i.id == "quit"));
        }
    }

    /// 7.1: in `off` the stopped banner (with the last-state age) replaces
    /// the attention headline while the PR rows stay rendered.
    #[test]
    fn stopped_banner_replaces_the_headline_in_off() {
        let blocked = entry("org/r#3", PrState::ReviewedChangesRequested, "c", "cr");
        let mut payload = status(vec![blocked]);
        payload.last_state_poll = Some(now() - chrono::Duration::hours(3));
        let headline = |model: &MenuModel| {
            model
                .items()
                .find(|i| i.id == "info|attention")
                .unwrap()
                .label
                .clone()
        };
        let active = build(&payload, &[]);
        assert_eq!(headline(&active), "1 PR needs attention · checked 3h ago");
        assert_eq!(
            find(&active, "info|attention").unwrap().symbol,
            Some(Symbol::Attention)
        );
        assert!(!active.stopped);

        payload.review_mode = ReviewMode::Off;
        let off = build(&payload, &[]);
        assert_eq!(headline(&off), "Reviews off · checked 3h ago");
        assert_eq!(
            find(&off, "info|attention").unwrap().symbol,
            Some(Symbol::Paused)
        );
        assert!(off.stopped);
        assert_eq!(off.items().filter(|i| i.kind == ItemKind::Pr).count(), 1);

        payload.last_state_poll = None;
        let never = build(&payload, &[]);
        assert_eq!(headline(&never), "Reviews off · not checked yet");
    }

    /// 7.2: Unblock replaces Block exactly when the payload says prx's
    /// identity holds the request-changes review.
    #[test]
    fn unblock_replaces_block_per_the_payload() {
        let children = |e: StatusEntry| {
            let key = e.key.clone();
            let model = build(&status(vec![e]), &[]);
            let row = find(&model, &format!("pr|{key}")).unwrap().clone();
            row.children
                .iter()
                .map(|c| (c.id.clone(), c.label.clone()))
                .collect::<Vec<_>>()
        };
        let has = |rows: &[(String, String)], id: &str| rows.iter().any(|(i, _)| i == id);

        let mut blocked = entry("o/r#1", PrState::ReviewedChangesRequested, "t", "s");
        blocked.identity_login = Some("prx-bot".into());
        let rows = children(blocked);
        assert!(has(&rows, "unblock|o/r#1"));
        assert!(!has(&rows, "block|o/r#1"));
        assert!(rows.iter().any(|(_, l)| l == "Unblock"));

        // Unknown identity → Block even in the blocking state.
        let rows = children(entry("o/r#2", PrState::ReviewedChangesRequested, "t", "s"));
        assert!(has(&rows, "block|o/r#2"));
        assert!(!has(&rows, "unblock|o/r#2"));

        let mut pending = entry("o/r#3", PrState::Pending, "t", "s");
        pending.identity_login = Some("prx-bot".into());
        let rows = children(pending);
        assert!(has(&rows, "block|o/r#3"));
        assert!(has(&rows, "review-prompt|o/r#3"));
        assert!(has(&rows, "review|o/r#3"), "plain review stays");
    }

    // ------------------------------------------- 8.1 indicators + ready group

    fn with_verdict(
        key: &str,
        state: PrState,
        verdict: Option<Verdict>,
        icon: &str,
    ) -> StatusEntry {
        let mut e = entry(key, state, "t", "s");
        e.last_verdict = verdict;
        e.icon = icon.to_string();
        e
    }

    /// Group header ids in render order.
    fn group_order(model: &MenuModel) -> Vec<String> {
        model
            .items()
            .filter(|i| i.kind == ItemKind::GroupHeader)
            .map(|i| i.id.trim_start_matches("info|group|").to_string())
            .collect()
    }

    /// Seeded state covering every state, rendered through the daemon's own
    /// `status::render_entries` (so the glyphs are the core `icon_for` ones
    /// the CLI prints too): each row shows its glyph and lands in its group
    /// per the menu-bar-shell scenarios — Ready (agent-approved, awaiting the
    /// human) vs human-approved vs comment-only vs blocking.
    #[test]
    fn every_state_shows_its_glyph_and_group() {
        let now = now();
        let seeded = |number: u64,
                      state: PrState,
                      verdict: Option<Verdict>,
                      human: bool|
         -> prx::state::PrRecord {
            let mut rec = sample_record("o/r", number);
            rec.state = state;
            rec.last_verdict = verdict;
            rec.review_anchor_at = Some(now - chrono::TimeDelta::try_hours(48).unwrap());
            if state == PrState::Skipped {
                rec.skip_reason = Some(SkipReason::Ignored);
            }
            rec.human_approved_at = human.then_some(now);
            rec
        };
        let records = [
            seeded(1, PrState::Pending, None, false),
            seeded(2, PrState::InReview, None, false),
            // Agent approved, human has not: ready for approval.
            seeded(3, PrState::ReviewedApproved, Some(Verdict::Approved), false),
            // Comment-only shares reviewed_approved but not its glyph/group.
            seeded(
                4,
                PrState::ReviewedApproved,
                Some(Verdict::CommentOnly),
                false,
            ),
            // Blocking.
            seeded(
                5,
                PrState::ReviewedChangesRequested,
                Some(Verdict::ChangesRequested),
                false,
            ),
            // Human Approve over a blocking agent verdict.
            seeded(
                6,
                PrState::ReviewedApproved,
                Some(Verdict::ChangesRequested),
                true,
            ),
            seeded(7, PrState::Skipped, None, false),
            seeded(8, PrState::Merged, Some(Verdict::Approved), false),
            seeded(9, PrState::Closed, None, false),
            // Human Approve of an agent-approved PR: no longer awaiting.
            seeded(10, PrState::ReviewedApproved, Some(Verdict::Approved), true),
            // Re-review pending after an earlier verdict.
            seeded(11, PrState::Pending, Some(Verdict::Approved), false),
            // Just opened: still cooling down.
            {
                let mut rec = seeded(12, PrState::Pending, None, false);
                rec.review_anchor_at = Some(now);
                rec
            },
        ];
        let mut state = prx::state::StateFile::default();
        for rec in records {
            state.prs.insert(rec.key(), rec);
        }
        let repos =
            prx::config::Config::parse("repos:\n  - name: o/r\n", std::path::Path::new("t.yml"))
                .unwrap()
                .effective_repos()
                .unwrap();
        let prs = prx::status::render_entries(&state, &repos, now);
        let model = build(&status(prs), &[]);
        // Round 2: the state is the row's SF Symbol (the app window's
        // state system), never an emoji in the label.
        let expect = [
            ("Needs-review", "o/r#1", Symbol::NeedsReview),
            ("In-review", "o/r#2", Symbol::InReview),
            ("Ready-for-approval", "o/r#3", Symbol::Ready),
            ("Reviewed", "o/r#4", Symbol::Commented),
            ("Changes-requested", "o/r#5", Symbol::ChangesRequested),
            ("Reviewed", "o/r#6", Symbol::Approved),
            ("Skipped", "o/r#7", Symbol::Skipped),
            ("Terminal", "o/r#8", Symbol::Merged),
            ("Terminal", "o/r#9", Symbol::Closed),
            ("Reviewed", "o/r#10", Symbol::Approved),
            ("Needs-review", "o/r#11", Symbol::ReReview),
            ("Needs-review", "o/r#12", Symbol::CoolingDown),
        ];
        for (group, key, symbol) in expect {
            let labels = group_labels(&model, group);
            assert!(
                labels.iter().any(|l| l.starts_with(&format!("{key} "))),
                "{key} under {group}: {labels:?}"
            );
            let row = find(&model, &format!("pr|{key}")).unwrap();
            assert_eq!(row.symbol, Some(symbol), "{key}");
            assert!(row.label.starts_with(key), "{}", row.label);
        }
        assert_eq!(
            group_labels(&model, "Ready-for-approval").len(),
            1,
            "only the PR still awaiting the human"
        );
        // What needs the human first: the attention groups, then Ready,
        // then the queue, then everything settled.
        assert_eq!(
            group_order(&model),
            [
                "Changes-requested",
                "In-review",
                "Ready-for-approval",
                "Needs-review",
                "Reviewed",
                "Skipped",
                "Terminal"
            ]
        );
        // Blocking vs ready vs comment-only: three distinct symbols.
        let glyph = |key: &str| find(&model, &format!("pr|{key}")).unwrap().symbol;
        let (ready, comment, blocking) = (glyph("o/r#3"), glyph("o/r#4"), glyph("o/r#5"));
        assert_ne!(ready, comment);
        assert_ne!(ready, blocking);
        assert_ne!(comment, blocking);
        // Only Ready rows carry the inline Approve.
        let inline_approves: Vec<&str> = model
            .sections
            .iter()
            .flat_map(|s| s.items.iter())
            .filter(|i| i.id.starts_with("approve|"))
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(inline_approves, ["approve|o/r#3"]);
        // The attention count is unchanged: blocking + in review only.
        assert_eq!(model.attention, 2);
    }

    /// Task 8.1: the human-approval signal moves an agent-approved PR out of
    /// Ready (and the inline Approve with it) once the human approved, and
    /// back when it is cleared (the next agent review).
    #[test]
    fn human_approval_leaves_the_ready_group() {
        let mut approved = with_verdict(
            "o/r#3",
            PrState::ReviewedApproved,
            Some(Verdict::Approved),
            "✅",
        );
        let ready = build(&status(vec![approved.clone()]), &[]);
        assert_eq!(group_labels(&ready, "Ready-for-approval").len(), 1);
        assert!(find(&ready, "approve|o/r#3").is_some());

        approved.human_approved_at = Some(now());
        let done = build(&status(vec![approved.clone()]), &[]);
        assert!(group_labels(&done, "Ready-for-approval").is_empty());
        assert_eq!(group_labels(&done, "Reviewed").len(), 1);
        assert!(
            done.sections
                .iter()
                .flat_map(|s| s.items.iter())
                .all(|i| i.id != "approve|o/r#3"),
            "no inline Approve once the human approved"
        );
        // The submenu Approve stays available (approving again is harmless).
        assert!(find(&done, "approve|o/r#3").is_some());
        assert_eq!(done.attention, 0);
    }

    fn brief(text: &str, updated: &str, stale: bool) -> prx::status::BriefStatus {
        prx::status::BriefStatus {
            text: text.to_string(),
            created_at: "2026-10-01T09:00:00Z".parse().unwrap(),
            updated_at: updated.parse().unwrap(),
            head_sha: "aaaa".to_string(),
            source: prx::state::BriefSource::Review,
            engine: None,
            stale,
        }
    }

    /// A row's child ids, separators left out (the grouping is pinned by
    /// `every_pr_row_offers_the_full_action_submenu`).
    fn child_ids(model: &MenuModel, row: &str) -> Vec<String> {
        find(model, row)
            .expect("row")
            .children
            .iter()
            .filter(|c| c.kind != ItemKind::Separator)
            .map(|c| c.id.clone())
            .collect()
    }

    /// Task 4.1: with a brief the drill-down shows the verdict line, the
    /// brief truncated to one ~200-char line, "updated <age>", "Copy brief"
    /// and "Regenerate brief" (all enabled, terminal PRs included).
    #[test]
    fn drill_down_shows_the_brief_with_copy_and_regenerate() {
        let long = format!("First line.\n\n{}", "lorem ipsum ".repeat(40));
        let mut with = with_verdict(
            "o/r#3",
            PrState::ReviewedApproved,
            Some(Verdict::Approved),
            "✅",
        );
        with.brief = Some(brief(&long, "2026-10-05T10:00:00Z", false));
        let mut merged = with_verdict("o/r#4", PrState::Merged, Some(Verdict::Approved), "✔️");
        merged.brief = Some(brief("Short brief.", "2026-10-05T11:30:00Z", false));
        let model = build(&status(vec![with, merged]), &[]);

        assert_eq!(
            &child_ids(&model, "pr|o/r#3")[..6],
            [
                "open|o/r#3",
                "info|verdict|o/r#3",
                "info|brief|o/r#3",
                "info|brief-age|o/r#3",
                "copy-brief|o/r#3",
                "brief|o/r#3",
            ]
        );
        let row = find(&model, "pr|o/r#3").unwrap();
        assert_eq!(row.children[1].kind, ItemKind::Separator, "open · brief");
        assert_eq!(row.children[2].label, "Approved · 1 blocking · 2 nits");
        let line = &row.children[3];
        assert_eq!(line.kind, ItemKind::Info);
        assert!(!line.enabled);
        assert_eq!(line.label.chars().count(), SUMMARY_MENU_CHARS);
        assert!(
            line.label.starts_with("First line. lorem ipsum"),
            "{}",
            line.label
        );
        assert!(line.label.ends_with('\u{2026}'));
        assert!(!line.label.contains('\n'));
        assert_eq!(row.children[4].label, "updated 2h ago");
        let copy = &row.children[5];
        assert_eq!(copy.label, "Copy brief");
        assert!(copy.enabled && copy.kind == ItemKind::Action);
        let regenerate = &row.children[6];
        assert_eq!(regenerate.label, "Regenerate brief");
        assert!(regenerate.enabled && regenerate.kind == ItemKind::Action);

        // A short brief shows whole; Copy and Regenerate stay available on
        // terminal PRs (briefs work in any tracked state).
        assert_eq!(
            find(&model, "info|brief|o/r#4").unwrap().label,
            "Short brief."
        );
        assert_eq!(
            find(&model, "info|brief-age|o/r#4").unwrap().label,
            "updated 30m ago"
        );
        assert!(find(&model, "copy-brief|o/r#4").unwrap().enabled);
        assert!(find(&model, "brief|o/r#4").unwrap().enabled);
    }

    /// Task 4.1: a stale brief says so on its age line.
    #[test]
    fn stale_brief_is_marked_on_the_age_line() {
        let mut pr = entry("o/r#3", PrState::Pending, "t", "s");
        pr.brief = Some(brief("Old.", "2026-10-03T12:00:00Z", true));
        let model = build(&status(vec![pr]), &[]);
        assert_eq!(
            find(&model, "info|brief-age|o/r#3").unwrap().label,
            "updated 2d ago · stale"
        );
        assert_eq!(
            find(&model, "brief|o/r#3").unwrap().label,
            "Regenerate brief"
        );
    }

    /// Task 4.1: no brief offers "Write brief" (and no brief lines); a repo
    /// that cannot have briefs (`off`, or briefs disabled) offers nothing.
    #[test]
    fn missing_brief_offers_write_brief_unless_unavailable() {
        let none = with_verdict(
            "o/r#5",
            PrState::ReviewedApproved,
            Some(Verdict::CommentOnly),
            "💬",
        );
        let mut off = entry("o/r#6", PrState::Pending, "t", "s");
        off.brief_available = false;
        let model = build(&status(vec![none, off]), &[]);

        let write = find(&model, "brief|o/r#5").expect("Write brief");
        assert_eq!(write.label, "Write brief");
        assert!(write.enabled && write.kind == ItemKind::Action);
        assert_eq!(
            child_ids(&model, "pr|o/r#5")[..2],
            ["open|o/r#5", "brief|o/r#5"]
        );
        for id in [
            "info|verdict|o/r#5",
            "info|brief|o/r#5",
            "info|brief-age|o/r#5",
            "copy-brief|o/r#5",
        ] {
            assert!(find(&model, id).is_none(), "{id}");
        }
        for id in ["brief|o/r#6", "info|brief-error|o/r#6", "copy-brief|o/r#6"] {
            assert!(find(&model, id).is_none(), "{id}");
        }
    }

    /// Task 4.1: while a brief is being written the action is disabled and
    /// reads "Writing brief…", with or without an existing brief.
    #[test]
    fn pending_brief_disables_the_action() {
        let mut fresh = entry("o/r#3", PrState::Pending, "t", "s");
        fresh.brief_pending = true;
        fresh.brief_error = Some("old failure".into());
        let mut regen = entry("o/r#4", PrState::Pending, "t", "s");
        regen.brief = Some(brief("Kept.", "2026-10-05T10:00:00Z", false));
        regen.brief_pending = true;
        let model = build(&status(vec![fresh, regen]), &[]);
        for key in ["o/r#3", "o/r#4"] {
            let item = find(&model, &format!("brief|{key}")).unwrap();
            assert_eq!(item.label, "Writing brief\u{2026}");
            assert!(!item.enabled, "{key}");
        }
        // The previous brief stays visible (and copyable) meanwhile, and a
        // stale failure is not shown while a new run is under way.
        assert_eq!(find(&model, "info|brief|o/r#4").unwrap().label, "Kept.");
        assert!(find(&model, "copy-brief|o/r#4").unwrap().enabled);
        assert!(find(&model, "info|brief-error|o/r#3").is_none());
    }

    /// Task 4.1/4.2: after a failed run the drill-down says why, above the
    /// action, which stays available for another try.
    #[test]
    fn failed_brief_shows_the_error_line() {
        let mut pr = entry("o/r#3", PrState::Pending, "t", "s");
        pr.brief = Some(brief("Kept.", "2026-10-05T10:00:00Z", false));
        pr.brief_error = Some(format!("engine timed out after 120s {}", "x".repeat(200)));
        let model = build(&status(vec![pr]), &[]);
        let ids = child_ids(&model, "pr|o/r#3");
        let error_at = ids
            .iter()
            .position(|id| id == "info|brief-error|o/r#3")
            .unwrap();
        assert_eq!(ids[error_at + 1], "brief|o/r#3");
        let line = find(&model, "info|brief-error|o/r#3").unwrap();
        assert!(
            line.label.starts_with("Brief failed: engine timed out"),
            "{}",
            line.label
        );
        assert!(line.label.chars().count() <= "Brief failed: ".len() + 80);
        assert!(find(&model, "brief|o/r#3").unwrap().enabled);
    }

    #[test]
    fn truncate_summary_collapses_whitespace_and_caps_length() {
        assert_eq!(truncate_summary("  a\n\n b\tc ", 200), "a b c");
        assert_eq!(truncate_summary("abcdef", 6), "abcdef");
        assert_eq!(truncate_summary("abcdefg", 6), "abcde\u{2026}");
        assert_eq!(truncate_summary("ab   cdefg", 4), "ab\u{2026}");
    }

    /// Spec "Approve-ready PRs are pulled forward": two approved PRs both
    /// sit in the ready group, each with an inline Approve right under its
    /// row (same id as the submenu Approve), and nowhere else.
    #[test]
    fn ready_group_offers_inline_approve_on_each_entry() {
        let model = build(
            &status(vec![
                with_verdict(
                    "o/r#3",
                    PrState::ReviewedApproved,
                    Some(Verdict::Approved),
                    "✅",
                ),
                with_verdict(
                    "o/r#4",
                    PrState::ReviewedApproved,
                    Some(Verdict::Approved),
                    "✅",
                ),
                with_verdict(
                    "o/r#5",
                    PrState::ReviewedApproved,
                    Some(Verdict::CommentOnly),
                    "✅",
                ),
            ]),
            &[],
        );
        let section = model
            .sections
            .iter()
            .find(|s| s.items[0].id == "info|group|Ready-for-approval")
            .expect("ready group section");
        assert_eq!(section.items[0].label, "Ready for approval (2)");
        let ids: Vec<&str> = section.items[1..].iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            ["pr|o/r#3", "approve|o/r#3", "pr|o/r#4", "approve|o/r#4"]
        );
        // Round 2: "↳ Approve #N…" — the ↳ symbol under the row's state
        // symbol, one indent level in, and the PR's number in the label.
        let approves: Vec<(&str, Option<Symbol>, u8, bool)> = section.items[1..]
            .iter()
            .filter(|i| i.kind == ItemKind::Action)
            .map(|i| (i.label.as_str(), i.symbol, i.indent, i.enabled))
            .collect();
        assert_eq!(
            approves,
            [
                ("Approve #3…", Some(Symbol::ApproveFor), 1, true),
                ("Approve #4…", Some(Symbol::ApproveFor), 1, true)
            ]
        );
        assert_eq!(Symbol::ApproveFor.sf_name(), "arrow.turn.down.right");
        assert_eq!(Symbol::ApproveFor.fallback(), "↳");
        // Comment-only stays in Reviewed with no inline Approve row.
        let reviewed = model
            .sections
            .iter()
            .find(|s| s.items[0].id == "info|group|Reviewed")
            .expect("reviewed section");
        assert!(reviewed.items.iter().all(|i| i.kind != ItemKind::Action));
        assert_eq!(model.attention, 0, "approved PRs never raise attention");
    }

    // ------------------------------------------------------- 8.3 pause footer

    /// The headline without its timing (`All clear · checked 2m ago` →
    /// `All clear`).
    fn headline(model: &MenuModel) -> String {
        let label = &find(model, "info|attention")
            .expect("attention banner present")
            .label;
        label.split(" · ").next().unwrap().to_string()
    }

    fn mode_parent(model: &MenuModel) -> &ModelItem {
        find(model, actions::MODE_PARENT_ID).expect("review-mode submenu in the footer")
    }

    /// The pause section: the review-mode submenu's rows after the
    /// separator that follows "Per-repo modes…".
    fn pause_section(model: &MenuModel) -> Vec<(&str, &str, bool)> {
        let children = &mode_parent(model).children;
        let repos = children
            .iter()
            .position(|c| c.id == actions::OPEN_REPOS_ID)
            .expect("Per-repo modes…");
        assert_eq!(children[repos + 1].kind, ItemKind::Separator);
        children[repos + 2..]
            .iter()
            .map(|c| (c.id.as_str(), c.label.as_str(), c.enabled))
            .collect()
    }

    const DURATIONS: [(&str, &str); 4] = [
        ("pause:30m", "For 30 minutes"),
        ("pause:1h", "For 1 hour"),
        ("pause:2h", "For 2 hours"),
        ("pause:4h", "For 4 hours"),
    ];

    fn durations(enabled: bool) -> Vec<(&'static str, &'static str, bool)> {
        DURATIONS
            .iter()
            .map(|&(id, label)| (id, label, enabled))
            .collect()
    }

    /// Round 3: the pause lives inside "Review mode ▸" — no submenu of its
    /// own — and explains itself: it holds automatic reviews only, and ends
    /// on its own. Then the durations.
    #[test]
    fn pause_section_explains_itself_and_offers_durations() {
        let model = build(&status(vec![]), &[]);
        assert!(find(&model, actions::PAUSE_PARENT_ID).is_none());
        let parent = mode_parent(&model);
        assert_eq!(parent.label, "Review mode: Review and comment");
        assert_eq!(parent.symbol, Some(Symbol::Mode));
        let mut expected = vec![
            ("info|pause|title", "Pause automatic reviews", false),
            (
                "info|pause|what",
                "Holds automatic reviews; manual reviews still run; resumes by itself",
                false,
            ),
        ];
        expected.extend(durations(true));
        assert_eq!(pause_section(&model), expected);
        assert_eq!(
            find(&model, "info|pause|title").unwrap().symbol,
            Some(Symbol::Paused)
        );
        assert!(
            find(&model, actions::RESUME_ID).is_none(),
            "nothing to resume"
        );
        assert_eq!(headline(&model), "No PRs tracked yet");
        assert!(!model.paused);
    }

    /// Spec "Pause is not shown as stopped": paused in `review comment` →
    /// the headline stays the attention count with the countdown in its
    /// timing slot (round 2: no separate pause line), the review-mode
    /// title carries it too (round 3), rows keep rendering, not stopped;
    /// the countdown follows `now` on each rebuild.
    #[test]
    fn paused_shows_a_countdown_not_the_stopped_banner() {
        let mut payload = status(vec![entry(
            "o/r#1",
            PrState::ReviewedChangesRequested,
            "t",
            "s",
        )]);
        payload.review_mode = ReviewMode::Monitor;
        payload.paused_until = Some(now() + chrono::Duration::minutes(72));
        payload.last_state_poll = Some(now() - chrono::Duration::minutes(2));
        payload.next_poll = Some(now() + chrono::Duration::minutes(13));
        let model = build(&payload, &[]);
        let line = find(&model, "info|attention").unwrap();
        assert_eq!(
            line.label,
            "1 PR needs attention · checked 2m ago · paused, 1h 12m left"
        );
        assert_eq!(line.symbol, Some(Symbol::Paused));
        assert!(!model.stopped);
        assert!(model.paused);
        let parent = mode_parent(&model);
        assert_eq!(parent.label, "Review mode: Monitor · paused, 1h 12m left");
        assert_eq!(parent.symbol, Some(Symbol::Paused));
        let mut expected = vec![
            (
                "info|pause|status",
                "Automatic reviews paused · 1h 12m left",
                false,
            ),
            ("resume", "Resume now", true),
            (
                "info|pause|what",
                "Holds automatic reviews; manual reviews still run; resumes by itself",
                false,
            ),
            (
                "info|pause|when",
                "Pick a time to restart the pause from now",
                false,
            ),
        ];
        expected.extend(durations(true));
        assert_eq!(
            pause_section(&model),
            expected,
            "countdown and Resume lead; durations restart the pause"
        );
        assert_eq!(
            group_labels(&model, "Changes-requested").len(),
            1,
            "rows keep updating"
        );
        // The message is the only second line.
        let with_message = model.clone().with_message("✓ paused");
        let header = ids(&with_message.sections[0].items);
        assert_eq!(header, ["info|attention", "info|about", "info|message"]);

        // Ten minutes later the countdown has moved.
        let later = build_model(
            &payload,
            &[],
            &mut PaginationState::default(),
            now() + chrono::Duration::minutes(10),
        );
        assert_eq!(
            mode_parent(&later).label,
            "Review mode: Monitor · paused, 1h 2m left"
        );
        assert_eq!(
            find(&later, "info|pause|status").unwrap().label,
            "Automatic reviews paused · 1h 2m left"
        );

        // An expired end (stale payload) is not a pause.
        let expired = build_model(
            &payload,
            &[],
            &mut PaginationState::default(),
            now() + chrono::Duration::hours(2),
        );
        assert_eq!(mode_parent(&expired).label, "Review mode: Monitor");
        assert_eq!(mode_parent(&expired).symbol, Some(Symbol::Mode));
        assert!(find(&expired, actions::RESUME_ID).is_none());
        assert!(!find(&expired, "info|attention")
            .unwrap()
            .label
            .contains("paused"));
        assert!(!expired.paused);
    }

    /// Mode `off` shows stopped, not a countdown, in the header and the
    /// review-mode title (off wins when both apply). The pause section
    /// stays visible but its durations are disabled under the reason; a
    /// running pause can still be resumed.
    #[test]
    fn off_wins_the_headline_over_a_pause() {
        let mut payload = status(vec![]);
        payload.review_mode = ReviewMode::Off;
        let off = build(&payload, &[]);
        assert_eq!(headline(&off), "Reviews off");
        assert_eq!(mode_parent(&off).label, "Review mode: Off");
        assert_eq!(mode_parent(&off).symbol, Some(Symbol::Paused));
        let what = (
            "info|pause|what",
            "Holds automatic reviews; manual reviews still run; resumes by itself",
            false,
        );
        let reason = (
            "info|pause|off",
            "Nothing to pause while the review mode is Off",
            false,
        );
        let mut expected = vec![
            ("info|pause|title", "Pause automatic reviews", false),
            what,
            reason,
        ];
        expected.extend(durations(false));
        assert_eq!(pause_section(&off), expected);

        payload.paused_until = Some(now() + chrono::Duration::minutes(30));
        let both = build(&payload, &[]);
        assert_eq!(headline(&both), "Reviews off");
        assert!(!find(&both, "info|attention")
            .unwrap()
            .label
            .contains("paused"));
        assert!(both.stopped);
        assert_eq!(mode_parent(&both).label, "Review mode: Off");
        let mut expected = vec![
            (
                "info|pause|status",
                "Automatic reviews paused · 30m left",
                false,
            ),
            ("resume", "Resume now", true),
            what,
            reason,
        ];
        expected.extend(durations(false));
        assert_eq!(pause_section(&both), expected);
    }

    #[test]
    fn pause_remaining_formats_compactly() {
        let at = |secs: i64| pause_remaining(Some(now() + chrono::Duration::seconds(secs)), now());
        assert_eq!(at(1), Some("1m".into()));
        assert_eq!(at(30 * 60), Some("30m".into()));
        assert_eq!(at(60 * 60), Some("1h".into()));
        assert_eq!(at(4 * 3600 - 30), Some("4h".into()));
        assert_eq!(at(26 * 3600), Some("1d 2h".into()));
        assert_eq!(at(48 * 3600), Some("2d".into()));
        assert_eq!(at(0), None);
        assert_eq!(pause_remaining(None, now()), None);
    }

    // ------------------------------------------- 6.2 review actions submenu

    fn with_engines(prs: Vec<StatusEntry>, engines: &[&str]) -> StatusResponse {
        let mut payload = status(prs);
        payload.engines = engines.iter().map(|e| e.to_string()).collect();
        payload
    }

    #[test]
    fn review_actions_come_in_order_with_the_default_engine_marked() {
        let mut e = entry("o/r#7", PrState::ReviewedApproved, "t", "s");
        e.engine = Some("codex".into());
        let model = build(
            &with_engines(vec![e], &["claude", "codex", "my-agent"]),
            &[],
        );
        let row = find(&model, "pr|o/r#7").unwrap();
        let ids: Vec<&str> = row
            .children
            .iter()
            .filter(|c| c.kind != ItemKind::Separator)
            .map(|c| c.id.as_str())
            .collect();
        // After Open and the brief action (task 4.1).
        assert_eq!(ids[1], "brief|o/r#7");
        assert_eq!(
            &ids[2..6],
            [
                "review|o/r#7",
                "review-with",
                "review-nopost|o/r#7",
                "review-prompt|o/r#7"
            ]
        );
        let labels: Vec<&str> = row
            .children
            .iter()
            .filter(|c| c.kind != ItemKind::Separator)
            .map(|c| c.label.as_str())
            .collect();
        assert_eq!(
            &labels[2..6],
            [
                "Review",
                "Review with",
                "Review without posting",
                "Review with prompt…"
            ]
        );
        let with = &row.children[5];
        let engines: Vec<(&str, &str)> = with
            .children
            .iter()
            .map(|c| (c.id.as_str(), c.label.as_str()))
            .collect();
        assert_eq!(
            engines,
            [
                ("review-engine|claude|o/r#7", "claude"),
                ("review-engine|codex|o/r#7", "codex (default)"),
                ("review-engine|my-agent|o/r#7", "my-agent"),
            ]
        );
        assert!(with.enabled && with.children.iter().all(|c| c.enabled));
        // No label anywhere says "Re-review".
        assert!(
            model.items().all(|i| !i.label.contains("Re-review")),
            "{}",
            model.dump()
        );
    }

    #[test]
    fn old_daemons_get_no_review_with_submenu() {
        let model = build(&status(vec![pending(1)]), &[]);
        assert!(find(&model, "review-with").is_none());
        assert!(model.items().all(|i| !i.id.starts_with("review-engine|")));
        assert!(find(&model, "review-nopost|org/r#001").is_some());
    }

    #[test]
    fn review_actions_are_disabled_for_terminal_prs() {
        let model = build(
            &with_engines(
                vec![entry("o/r#9", PrState::Closed, "t", "s")],
                &["claude", "codex"],
            ),
            &[],
        );
        let row = find(&model, "pr|o/r#9").unwrap();
        for item in row.children.iter().flat_map(item_and_descendants) {
            if item.id.starts_with("review") {
                assert!(!item.enabled, "{} must be disabled", item.id);
            }
        }
        assert!(find(&model, "review-engine|codex|o/r#9").is_some());
    }

    #[test]
    fn history_marks_runs_that_did_not_post() {
        let mut entry = history_entry("org/one", 7);
        if let HistoryEntry::ReviewCompleted { posted, .. } = &mut entry {
            *posted = Some(false);
        }
        let posted = history_entry("org/one", 8);
        let model = build(&status(vec![]), &[entry, posted]);
        let history = find(&model, "history").unwrap();
        assert!(
            history.children[0]
                .label
                .contains("Approved (not posted) — 0 blocking"),
            "{}",
            history.children[0].label
        );
        assert!(!history.children[1].label.contains("not posted"));
    }

    // ------------------------------------------- 7.x sort and grouping

    fn created(key: &str, state: PrState, hours_ago: Option<i64>) -> StatusEntry {
        let mut e = entry(key, state, "t", "s");
        e.created_at = hours_ago.map(|h| now() - chrono::Duration::hours(h));
        e
    }

    fn with_menu(prs: Vec<StatusEntry>, sort: MenuSort, group_by: MenuGroupBy) -> StatusResponse {
        let mut payload = status(prs);
        payload.menu = prx::config::MenuSettings { sort, group_by };
        payload
    }

    fn pr_keys(model: &MenuModel) -> Vec<String> {
        model
            .items()
            .filter(|i| i.kind == ItemKind::Pr)
            .map(|i| i.id.trim_start_matches("pr|").to_string())
            .collect()
    }

    fn sorted_keys(prs: &[StatusEntry], sort: MenuSort) -> Vec<String> {
        let mut refs: Vec<&StatusEntry> = prs.iter().collect();
        sort_entries(&mut refs, sort);
        refs.iter().map(|e| e.key.clone()).collect()
    }

    #[test]
    fn sort_comparators_follow_the_spec() {
        let mut never = created("a/x#3", PrState::Pending, Some(1));
        never.last_review_at = None;
        let mut old = created("a/x#10", PrState::Pending, Some(24));
        old.last_review_at = Some(now() - chrono::Duration::hours(5));
        let mut new = created("a/x#9", PrState::Pending, Some(2));
        new.last_review_at = Some(now() - chrono::Duration::hours(1));
        let missing = created("a/x#1", PrState::Pending, None);
        let prs = vec![old, missing, never, new];

        // Newest first; no created_at goes last.
        assert_eq!(
            sorted_keys(&prs, MenuSort::CreatedDesc),
            ["a/x#3", "a/x#9", "a/x#10", "a/x#1"]
        );
        assert_eq!(
            sorted_keys(&prs, MenuSort::CreatedAsc),
            ["a/x#10", "a/x#9", "a/x#3", "a/x#1"]
        );
        // Never-reviewed last, then key order among them.
        assert_eq!(
            sorted_keys(&prs, MenuSort::LastReviewDesc),
            ["a/x#9", "a/x#10", "a/x#1", "a/x#3"]
        );
        // Numeric PR numbers: #9 before #10.
        assert_eq!(
            sorted_keys(&prs, MenuSort::Key),
            ["a/x#1", "a/x#3", "a/x#9", "a/x#10"]
        );
    }

    #[test]
    fn ties_fall_back_to_the_key_order() {
        let prs = vec![
            created("b/y#2", PrState::Pending, Some(3)),
            created("a/x#10", PrState::Pending, Some(3)),
            created("a/x#9", PrState::Pending, Some(3)),
        ];
        assert_eq!(
            sorted_keys(&prs, MenuSort::CreatedDesc),
            ["a/x#9", "a/x#10", "b/y#2"]
        );
    }

    /// Spec "Newest first by default": #9 (today) above #10 (yesterday) in
    /// the same group.
    #[test]
    fn the_default_menu_lists_newest_first() {
        let model = build(
            &status(vec![
                created("acme/api#10", PrState::Pending, Some(24)),
                created("acme/api#9", PrState::Pending, Some(1)),
            ]),
            &[],
        );
        assert_eq!(pr_keys(&model), ["acme/api#9", "acme/api#10"]);
    }

    #[test]
    fn group_by_none_renders_one_flat_list() {
        let model = build(
            &with_menu(
                vec![
                    created("o/r#1", PrState::Pending, Some(3)),
                    created("o/r#2", PrState::ReviewedChangesRequested, Some(2)),
                    created("o/r#3", PrState::Merged, Some(1)),
                ],
                MenuSort::CreatedDesc,
                MenuGroupBy::None,
            ),
            &[],
        );
        let headers: Vec<&str> = model
            .items()
            .filter(|i| i.kind == ItemKind::GroupHeader)
            .map(|i| i.label.as_str())
            .collect();
        // The changes-requested PR is pinned above the flat list too.
        assert_eq!(headers, ["Needs attention (1)", "Pull requests (3)"]);
        assert_eq!(pr_keys(&model), ["o/r#2", "o/r#3", "o/r#2", "o/r#1"]);
    }

    #[test]
    fn group_by_repo_orders_headers_by_name() {
        let model = build(
            &with_menu(
                vec![
                    created("acme/web#1", PrState::Pending, Some(1)),
                    created("acme/api#2", PrState::Pending, Some(2)),
                    created("acme/api#3", PrState::Skipped, Some(3)),
                ],
                MenuSort::CreatedDesc,
                MenuGroupBy::Repo,
            ),
            &[],
        );
        let headers: Vec<(&str, &str)> = model
            .items()
            .filter(|i| i.kind == ItemKind::GroupHeader)
            .map(|i| (i.id.as_str(), i.label.as_str()))
            .collect();
        assert_eq!(
            headers,
            [
                ("info|group|repo:acme/api", "acme/api (2)"),
                ("info|group|repo:acme/web", "acme/web (1)"),
            ]
        );
        assert_eq!(pr_keys(&model), ["acme/api#2", "acme/api#3", "acme/web#1"]);
    }

    #[test]
    fn inline_approve_appears_in_every_mode() {
        let ready = || {
            with_verdict(
                "o/r#3",
                PrState::ReviewedApproved,
                Some(Verdict::Approved),
                "✅",
            )
        };
        for group_by in [MenuGroupBy::Lifecycle, MenuGroupBy::Repo, MenuGroupBy::None] {
            let model = build(
                &with_menu(vec![ready(), pending(1)], MenuSort::CreatedDesc, group_by),
                &[],
            );
            let inline: Vec<&str> = model
                .sections
                .iter()
                .flat_map(|s| s.items.iter())
                .filter(|i| i.id.starts_with("approve|"))
                .map(|i| i.id.as_str())
                .collect();
            assert_eq!(inline, ["approve|o/r#3"], "{group_by:?}");
            // Right under its own row.
            let section = model
                .sections
                .iter()
                .find(|s| s.items.iter().any(|i| i.id == "approve|o/r#3"))
                .unwrap();
            let at = section
                .items
                .iter()
                .position(|i| i.id == "approve|o/r#3")
                .unwrap();
            assert_eq!(section.items[at - 1].id, "pr|o/r#3", "{group_by:?}");
        }
    }

    #[test]
    fn pagination_is_per_slug_and_resets_when_the_mode_changes() {
        let prs: Vec<StatusEntry> = (0..23).map(pending).collect();
        let lifecycle = with_menu(prs.clone(), MenuSort::Key, MenuGroupBy::Lifecycle);
        let flat = with_menu(prs, MenuSort::Key, MenuGroupBy::None);
        let mut pages = PaginationState::default();
        build_model(&flat, &[], &mut pages, now());
        pages.more("all");
        let model = build_model(&flat, &[], &mut pages, now());
        assert_eq!(pr_keys(&model).len(), 20);
        assert!(find(&model, "page:less|all").is_some());

        // Switch the mode and back: every group starts over.
        build_model(&lifecycle, &[], &mut pages, now());
        let model = build_model(&flat, &[], &mut pages, now());
        assert_eq!(pr_keys(&model).len(), PAGE_SIZE);
        assert_eq!(
            find(&model, "page:more|all").unwrap().label,
            "Show more (13 hidden)"
        );
    }

    /// Grouped by repo or not at all, the attention PRs are pinned in a
    /// "Needs attention" group first (lifecycle already leads with them).
    #[test]
    fn attention_prs_are_pinned_when_not_grouped_by_lifecycle() {
        let prs = || {
            let mut prs: Vec<StatusEntry> = (1..=12).map(pending).collect();
            prs.push(entry("o/z#1", PrState::ReviewedChangesRequested, "c", "cr"));
            prs.push(entry("o/z#2", PrState::InReview, "r", "ir"));
            prs
        };
        for group_by in [MenuGroupBy::None, MenuGroupBy::Repo] {
            let model = build(&with_menu(prs(), MenuSort::Key, group_by), &[]);
            assert_eq!(group_order(&model)[0], "attention", "{group_by:?}");
            assert_eq!(
                find(&model, "info|group|attention").unwrap().label,
                "Needs attention (2)"
            );
            let pinned: Vec<&str> = model.sections[2]
                .items
                .iter()
                .filter(|i| i.kind == ItemKind::Pr)
                .map(|i| i.id.as_str())
                .collect();
            assert_eq!(pinned, ["pr|o/z#1", "pr|o/z#2"], "{group_by:?}");
        }
        let lifecycle = build(
            &with_menu(prs(), MenuSort::Key, MenuGroupBy::Lifecycle),
            &[],
        );
        assert!(find(&lifecycle, "info|group|attention").is_none());
        let calm = build(
            &with_menu(vec![pending(1)], MenuSort::Key, MenuGroupBy::None),
            &[],
        );
        assert!(find(&calm, "info|group|attention").is_none());
    }

    /// 7.3: the model reads `status.menu` on every build, so a saved
    /// setting applies on the next refresh; an absent `menu` means the
    /// defaults.
    #[test]
    fn menu_settings_apply_per_payload() {
        let prs = || {
            vec![
                created("o/r#10", PrState::Pending, Some(1)),
                created("o/r#9", PrState::ReviewedChangesRequested, Some(2)),
            ]
        };
        let mut pages = PaginationState::default();
        let first = build_model(
            &with_menu(prs(), MenuSort::CreatedDesc, MenuGroupBy::Lifecycle),
            &[],
            &mut pages,
            now(),
        );
        assert_eq!(group_order(&first), ["Changes-requested", "Needs-review"]);
        let second = build_model(
            &with_menu(prs(), MenuSort::Key, MenuGroupBy::None),
            &[],
            &mut pages,
            now(),
        );
        assert_eq!(group_order(&second), ["attention", "all"]);
        assert_eq!(pr_keys(&second), ["o/r#9", "o/r#9", "o/r#10"]);

        // An older daemon's payload has no `menu`: defaults apply.
        let mut value = serde_json::to_value(status(prs())).unwrap();
        value.as_object_mut().unwrap().remove("menu");
        value.as_object_mut().unwrap().remove("engines");
        let old: StatusResponse = serde_json::from_value(value).unwrap();
        let model = build(&old, &[]);
        assert_eq!(group_order(&model), ["Changes-requested", "Needs-review"]);
        assert!(find(&model, "review-with").is_none());
    }
}

/// Quit semantics (task 6.5) and daemon supervision states (task 6.4).
#[cfg(test)]
mod supervision_tests {
    use super::*;

    fn labels(model: &MenuModel) -> Vec<(String, String)> {
        model
            .items()
            .map(|i| (i.id.clone(), i.label.clone()))
            .collect()
    }

    fn online() -> MenuModel {
        let status: StatusResponse = serde_json::from_value(serde_json::json!({
            "pid": 1, "version": "0.0.0", "uptime_seconds": 1, "in_flight": 0,
            "queue_depth": 0, "last_poll": {}, "last_state_poll": null, "prs": [],
        }))
        .unwrap_or_else(|e| panic!("status fixture: {e}"));
        build_model(&status, &[], &mut PaginationState::default(), Utc::now())
    }

    #[test]
    fn quit_is_followed_by_its_keep_reviewing_alternate() {
        for model in [online(), offline_model()] {
            let items = labels(&model);
            let quit = items
                .iter()
                .position(|(id, _)| id == QUIT_ID)
                .expect("Quit PRX");
            assert_eq!(items[quit].1, "Quit PRX");
            // The ⌥ alternate must sit right after its base item (AppKit
            // pairs alternates with the previous item).
            assert_eq!(items[quit + 1].0, QUIT_KEEP_ID);
            assert_eq!(items[quit + 1].1, "Quit PRX (keep reviewing)");
            assert!(items.iter().all(|(_, label)| !label.contains("prx")));
        }
    }

    #[test]
    fn offline_states_follow_the_supervisor() {
        let has = |model: &MenuModel, id: &str| model.items().any(|i| i.id == id);
        // Round 2: one line says what is wrong and why; its symbol says how
        // bad (the menu bar glyph shows offline).
        let headline = |model: &MenuModel| {
            let line = model.items().find(|i| i.id == "info|attention").unwrap();
            (line.label.clone(), line.symbol.unwrap())
        };
        assert!(offline_model().items().all(|i| i.id != "info|daemon"));

        let unmanaged = offline_model_for(&DaemonState::Unmanaged);
        assert_eq!(
            headline(&unmanaged),
            (
                "Daemon offline · PRX reconnects when it starts".to_string(),
                Symbol::Warning
            )
        );
        assert!(has(&unmanaged, START_DAEMON_ID) && has(&unmanaged, SHOW_LOG_ID));

        let starting = offline_model_for(&DaemonState::Running { pid: 9 });
        assert_eq!(
            headline(&starting),
            ("Starting the daemon…".to_string(), Symbol::CoolingDown)
        );
        assert!(!has(&starting, START_DAEMON_ID), "a start is underway");

        let restarting = offline_model_for(&DaemonState::Restarting {
            delay_secs: 4,
            failures: 3,
        });
        let (line, _) = headline(&restarting);
        assert!(
            line.starts_with("Daemon restarting · failure 3 of"),
            "{line}"
        );
        assert!(line.ends_with("retry in 4s"), "{line}");
        assert!(!has(&restarting, START_DAEMON_ID));
        assert!(has(&restarting, SHOW_LOG_ID));

        let gave_up = offline_model_for(&DaemonState::Stopped { gave_up: true });
        assert_eq!(
            headline(&gave_up),
            (
                "Daemon stopped · 5 crashes in 10 min".to_string(),
                Symbol::Failed
            )
        );
        let start = gave_up.items().find(|i| i.id == START_DAEMON_ID).unwrap();
        assert_eq!(start.label, "Start daemon");
        assert!(start.enabled && start.kind == ItemKind::Supervise);
        let log = gave_up.items().find(|i| i.id == SHOW_LOG_ID).unwrap();
        assert_eq!(log.label, "Show daemon log");

        let stopped = offline_model_for(&DaemonState::Stopped { gave_up: false });
        assert_eq!(headline(&stopped).0, "Daemon stopped");
        assert!(has(&stopped, START_DAEMON_ID));
    }
}
