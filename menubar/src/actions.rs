//! Menu-action dispatch (spec: "Per-PR actions round-trip through the
//! daemon" + "Footer actions"). [`ActionController::perform`] is THE
//! production handler: it maps a model item id (`<action>|<arg>`) onto a
//! [`prx::ipc::DaemonClient`] call — never a direct GitHub call, never `gh`
//! (A4, by construction: this module's only subprocesses are the configured
//! browser — see [`crate::browser`] — and the clipboard's `pbcopy`, see
//! [`crate::clipboard`]). Settings… and the other window shortcuts are not
//! actions here: the shell opens the in-process window ([`window_page`],
//! task 8.6).
//!
//! The GUI thread only calls [`ActionController::dispatch`], which runs the
//! action on a background thread (HTTP must never block the event loop), publishes the outcome as a message line for the menu, and
//! then nudges the poller so the menu reflects the new `/status` state.

use std::fmt::Write as _;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use prx::config::ReviewMode;
use prx::ipc::{
    BriefResponse, DaemonClient, MutationResponse, PauseResponse, ReviewModeResponse,
    ReviewResponse, StatusResponse,
};
use prx::review::ReviewOverrides;

use crate::clipboard::{Clipboard, SystemClipboard};

use crate::poller::PollerCommand;

// ------------------------------------------------------------- id vocabulary
// Shared with `model.rs`, which builds the ids these constants describe. An
// id is `<action>|<arg>`; the arg is a PR key (`org/repo#13`) for per-PR
// actions and empty for the footer ones.

pub const PR_ROW_PREFIX: &str = "pr";
pub const OPEN_PREFIX: &str = "open";
/// "Review" (design D9): a forced review with no per-run options.
pub const REVIEW_PREFIX: &str = "review";
/// "Review with ▸ {engine}": `review-engine|{engine}|{key}`. The engine
/// comes first so two `split_once('|')` calls parse it unambiguously (config
/// validation rejects engine names containing `|`; keys never contain one).
pub const REVIEW_ENGINE_PREFIX: &str = "review-engine";
/// The "Review with ▸" submenu parent (navigation only).
pub const REVIEW_WITH_PARENT_ID: &str = "review-with";
/// "Review without posting": `post: false`.
pub const REVIEW_NOPOST_PREFIX: &str = "review-nopost";
/// "Review with prompt…": the shell opens the review panel and submits
/// through [`ActionController::dispatch_review`]; the id itself never
/// reaches the daemon (the prompt is not in the id).
pub const REVIEW_PROMPT_PREFIX: &str = "review-prompt";
/// Footer "Review PR…": the review panel with a PR-reference field.
pub const REVIEW_PR_ID: &str = "review-pr";
/// Footer "Open prx…": the app window on its Dashboard page (design D16).
pub const OPEN_WINDOW_ID: &str = "open-window";
pub const APPROVE_PREFIX: &str = "approve";
pub const BLOCK_PREFIX: &str = "block";
/// "Copy brief" in a PR's drill-down (task 4.1): copies the PR's full
/// brief text (fetched fresh from `/status`) to the clipboard.
pub const COPY_BRIEF_PREFIX: &str = "copy-brief";
/// "Write brief" / "Regenerate brief" (task 4.2) → `POST /brief`. The
/// daemon generates in the background; the menu shows "Writing brief…"
/// from `/status`'s `brief_pending` until the new brief lands.
pub const BRIEF_PREFIX: &str = "brief";
pub const UNBLOCK_PREFIX: &str = "unblock";
/// The "Review mode: …" submenu parent (navigation only).
pub const MODE_PARENT_ID: &str = "mode";
/// Review-mode radio entries (task 7.1) → `POST /review-mode`.
pub const MODE_OFF_ID: &str = "mode:off";
pub const MODE_MONITOR_ID: &str = "mode:monitor";
pub const MODE_COMMENT_ID: &str = "mode:comment";
/// Pause control (task 8.3, design D5/D11) → `POST /pause {hours}` /
/// `POST /resume`, a section of the "Review mode: …" submenu since
/// design-audit Part 0 round 3. `PAUSE_PARENT_ID` was its own submenu's
/// parent; the menu no longer emits it, but a stray click still answers
/// with the submenu hint.
pub const PAUSE_PARENT_ID: &str = "pause";
pub const PAUSE_30M_ID: &str = "pause:30m";
pub const PAUSE_1H_ID: &str = "pause:1h";
pub const PAUSE_2H_ID: &str = "pause:2h";
pub const PAUSE_4H_ID: &str = "pause:4h";
pub const RESUME_ID: &str = "resume";
pub const SNOOZE_1H: &str = "snooze:1";
pub const SNOOZE_2H: &str = "snooze:2";
pub const SNOOZE_4H: &str = "snooze:4";
pub const SNOOZE_PUSH: &str = "snooze:push";
pub const SNOOZE_PARENT_ID: &str = "snooze";
pub const IGNORE_PREFIX: &str = "ignore";
pub const UNIGNORE_PREFIX: &str = "unignore";
pub const UNSNOOZE_PREFIX: &str = "unsnooze";
/// "Check GitHub now" (⌘R) → `POST /review-now`: the daemon reloads its
/// config if changed and polls every repo immediately (enqueueing what is
/// due, unless paused). The menu's only refresh item.
pub const REVIEW_NOW_ID: &str = "review-now";
pub const SETTINGS_ID: &str = "settings";
pub const HISTORY_ID: &str = "history";
/// History ▸ "Open history in PRX": the window's History page.
pub const OPEN_HISTORY_ID: &str = "open-history";
/// About ▸ "Open config.yml": the window's config.yml page.
pub const OPEN_CONFIG_ID: &str = "open-config";
/// Review mode ▸ "Per-repo modes…": the window's Repos & Orgs page.
pub const OPEN_REPOS_ID: &str = "open-repos";
/// Group pagination (`page:more|<group>` / `page:less|<group>`): local to the
/// shell — they mutate its `PaginationState` and rebuild the menu, never
/// round-tripping to the daemon (design D7).
pub const PAGE_MORE_PREFIX: &str = "page:more";
pub const PAGE_LESS_PREFIX: &str = "page:less";

/// A parsed pagination click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAction {
    More(String),
    Less(String),
}

/// Recognize a pagination id; `None` for every other id.
pub fn page_action(id: &str) -> Option<PageAction> {
    match id.split_once('|')? {
        (PAGE_MORE_PREFIX, group) if !group.is_empty() => Some(PageAction::More(group.into())),
        (PAGE_LESS_PREFIX, group) if !group.is_empty() => Some(PageAction::Less(group.into())),
        _ => None,
    }
}

/// The window page an id opens (design D16): "Open PRX" lands on
/// Dashboard, "Settings…" (⌘,) on General, and the submenu shortcuts on
/// History, config.yml and Repos & Orgs. `None` for every other id.
pub fn window_page(id: &str) -> Option<&'static str> {
    match id {
        OPEN_WINDOW_ID => Some("dashboard"),
        SETTINGS_ID => Some("general"),
        OPEN_HISTORY_ID => Some("history"),
        OPEN_CONFIG_ID => Some("raw"),
        OPEN_REPOS_ID => Some("repos"),
        _ => None,
    }
}

/// A parsed review action id (design D9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAction {
    /// `review|{key}`: no options.
    Plain(String),
    /// `review-engine|{engine}|{key}`.
    Engine { engine: String, key: String },
    /// `review-nopost|{key}`.
    NoPost(String),
    /// `review-prompt|{key}`: the shell opens the review panel for `key`.
    Prompt(String),
    /// `review-pr`: the shell opens the review panel with a reference field.
    ReviewPr,
}

impl ReviewAction {
    /// The request a direct (panel-less) review action sends: the target and
    /// its per-run options. `None` for the two panel actions.
    pub fn request(&self) -> Option<(String, ReviewOverrides)> {
        match self {
            ReviewAction::Plain(key) => Some((key.clone(), ReviewOverrides::default())),
            ReviewAction::Engine { engine, key } => Some((
                key.clone(),
                ReviewOverrides {
                    engine: Some(engine.clone()),
                    ..ReviewOverrides::default()
                },
            )),
            ReviewAction::NoPost(key) => Some((
                key.clone(),
                ReviewOverrides {
                    post: Some(false),
                    ..ReviewOverrides::default()
                },
            )),
            ReviewAction::Prompt(_) | ReviewAction::ReviewPr => None,
        }
    }
}

/// Recognize a review action id; `None` for every other id (including the
/// footer's `review-now`, which is a poll trigger, and malformed ids).
pub fn review_action(id: &str) -> Option<ReviewAction> {
    if id == REVIEW_PR_ID {
        return Some(ReviewAction::ReviewPr);
    }
    let (action, rest) = id.split_once('|')?;
    let key = |rest: &str| (!rest.is_empty()).then(|| rest.to_string());
    match action {
        REVIEW_PREFIX => key(rest).map(ReviewAction::Plain),
        REVIEW_NOPOST_PREFIX => key(rest).map(ReviewAction::NoPost),
        REVIEW_PROMPT_PREFIX => key(rest).map(ReviewAction::Prompt),
        REVIEW_ENGINE_PREFIX => {
            let (engine, key) = rest.split_once('|')?;
            (!engine.is_empty() && !key.is_empty()).then(|| ReviewAction::Engine {
                engine: engine.to_string(),
                key: key.to_string(),
            })
        }
        _ => None,
    }
}

/// How long `review-now` waits before its refresh nudge, so the nudged
/// `/status` already shows the daemon's immediate poll (the daemon wakes its
/// scheduler asynchronously; the 10 s poller would also catch it later).
pub const REVIEW_NOW_SETTLE: Duration = Duration::from_secs(2);

/// Runs menu actions and reports their outcome. Cheap to clone.
#[derive(Clone)]
pub struct ActionController {
    /// Refresh nudges (a completed action always triggers an immediate poll).
    refresh: Sender<PollerCommand>,
    /// Outcome sink: the GUI thread renders it as a disabled info line,
    /// self-test mode prints it.
    publish: Arc<dyn Fn(String) + Send + Sync>,
    /// "Copy brief" target (the system clipboard; injectable for tests).
    clipboard: Arc<dyn Clipboard>,
}

impl ActionController {
    pub fn new(refresh: Sender<PollerCommand>, publish: Arc<dyn Fn(String) + Send + Sync>) -> Self {
        ActionController {
            refresh,
            publish,
            clipboard: Arc::new(SystemClipboard),
        }
    }

    /// Replace the clipboard (tests).
    #[cfg(test)]
    pub fn with_clipboard(mut self, clipboard: Arc<dyn Clipboard>) -> Self {
        self.clipboard = clipboard;
        self
    }

    /// Run `id` on a background thread (the GUI entry point used by the menu
    /// click handler). Never blocks, never panics: every failure becomes a
    /// published message line.
    pub fn dispatch(&self, id: &str) {
        let me = self.clone();
        let id = id.to_string();
        let spawned = std::thread::Builder::new()
            .name("prx-menubar-action".into())
            .spawn(move || me.run(&id));
        if let Err(error) = spawned {
            (self.publish)(format!("⚠ cannot start action thread: {error}"));
        }
    }

    /// Submit a review panel's request (task 6.4): a forced review of
    /// `target` (any reference form) with `overrides`, on a background
    /// thread; same outcome/refresh path as [`ActionController::dispatch`].
    pub fn dispatch_review(&self, target: &str, overrides: ReviewOverrides) {
        let me = self.clone();
        let target = target.to_string();
        let spawned = std::thread::Builder::new()
            .name("prx-menubar-action".into())
            .spawn(move || {
                let id = format!("{REVIEW_PROMPT_PREFIX}|{target}");
                let result = block_on(review_target(&target, &overrides));
                me.finish(&id, result);
            });
        if let Err(error) = spawned {
            (self.publish)(format!("⚠ cannot start action thread: {error}"));
        }
    }

    /// The full action path: perform, publish the outcome, refresh the menu.
    /// `dispatch` runs this on a thread; the self-test harness calls it
    /// synchronously — same code either way.
    pub fn run(&self, id: &str) {
        let result = block_on(self.perform(id));
        self.finish(id, result);
    }

    /// Publish an action's outcome and nudge the poller.
    fn finish(&self, id: &str, result: Result<String, String>) {
        let message = match &result {
            Ok(note) => format!("✓ {note}"),
            Err(error) => format!("⚠ {error}"),
        };
        eprintln!("prx-menubar: action {id} -> {message}");
        (self.publish)(message);
        // After EVERY action: immediate refresh so the menu reflects the new
        // state from /status (also refreshes the last-poll line).
        let _ = self.refresh.send(PollerCommand::Refresh);
    }

    /// One menu id mapped onto its daemon call or local operation. Errors are
    /// plain strings: they are shown to the user, never unwrapped.
    pub async fn perform(&self, id: &str) -> Result<String, String> {
        if let Some(review) = review_action(id) {
            return match review.request() {
                Some((target, overrides)) => review_target(&target, &overrides).await,
                None => Err("this review is submitted from the review panel".into()),
            };
        }
        let (action, key) = id.split_once('|').unwrap_or((id, ""));
        match action {
            OPEN_PREFIX => {
                let url = github_url(key)?;
                // The repo's effective `browser` (daemon effective view;
                // system handler when the daemon cannot say).
                let client = DaemonClient::discover().await;
                let browser =
                    crate::browser::resolve(client.as_ref(), crate::browser::repo_of(key)).await;
                crate::browser::open(&browser, &url)?;
                Ok(format!("opened {url}"))
            }
            COPY_BRIEF_PREFIX => {
                let client = daemon().await?;
                let status = client.get_status().await.map_err(format_error)?;
                copy_brief(&status, key, self.clipboard.as_ref())
            }
            BRIEF_PREFIX => request_brief(key).await,
            BLOCK_PREFIX => {
                let client = daemon().await?;
                let response = client.block(key).await.map_err(format_error)?;
                Ok(summarize_mutation(
                    key,
                    "blocked (changes requested)",
                    response,
                ))
            }
            UNBLOCK_PREFIX => {
                let client = daemon().await?;
                let response = client.unblock(key).await.map_err(format_error)?;
                Ok(summarize_mutation(key, "unblocked", response))
            }
            MODE_OFF_ID => set_review_mode(ReviewMode::Off).await,
            MODE_MONITOR_ID => set_review_mode(ReviewMode::Monitor).await,
            MODE_COMMENT_ID => set_review_mode(ReviewMode::ReviewComment).await,
            PAUSE_30M_ID => pause(0.5).await,
            PAUSE_1H_ID => pause(1.0).await,
            PAUSE_2H_ID => pause(2.0).await,
            PAUSE_4H_ID => pause(4.0).await,
            RESUME_ID => {
                let client = daemon().await?;
                let response = client.resume().await.map_err(format_error)?;
                Ok(summarize_pause(None, response))
            }
            PAUSE_PARENT_ID | REVIEW_WITH_PARENT_ID | MODE_PARENT_ID => Err(submenu_hint(action)),
            APPROVE_PREFIX => {
                let client = daemon().await?;
                let response = client.approve(key).await.map_err(format_error)?;
                Ok(summarize_mutation(key, "approval posted", response))
            }
            SNOOZE_1H => snooze(key, Some(1.0), false).await,
            SNOOZE_2H => snooze(key, Some(2.0), false).await,
            SNOOZE_4H => snooze(key, Some(4.0), false).await,
            SNOOZE_PUSH => snooze(key, None, true).await,
            SNOOZE_PARENT_ID => Err(submenu_hint(action)),
            IGNORE_PREFIX => {
                let client = daemon().await?;
                let response = client.ignore(key).await.map_err(format_error)?;
                Ok(summarize_mutation(key, "ignored", response))
            }
            UNIGNORE_PREFIX => {
                let client = daemon().await?;
                let response = client.unignore(key).await.map_err(format_error)?;
                Ok(summarize_mutation(key, "un-ignored", response))
            }
            UNSNOOZE_PREFIX => {
                let client = daemon().await?;
                let response = client.unsnooze(key).await.map_err(format_error)?;
                Ok(summarize_mutation(key, "un-snoozed", response))
            }
            REVIEW_NOW_ID => {
                let client = daemon().await?;
                let response = client.review_now().await.map_err(format_error)?;
                // Give the daemon's scheduler a moment so the refresh nudge
                // that follows already carries the advanced last-poll time.
                tokio::time::sleep(REVIEW_NOW_SETTLE).await;
                Ok(summarize_check(response.note.as_deref()))
            }
            SETTINGS_ID | OPEN_WINDOW_ID | OPEN_HISTORY_ID | OPEN_CONFIG_ID | OPEN_REPOS_ID => {
                Err("the window is opened by the menu shell".into())
            }
            PR_ROW_PREFIX | HISTORY_ID => Err(submenu_hint(action)),
            PAGE_MORE_PREFIX | PAGE_LESS_PREFIX => {
                Err("pagination is handled by the menu shell, not the daemon".into())
            }
            other => Err(format!("no action wired for id '{other}'")),
        }
    }
}

/// Copy `key`'s full brief from a `/status` payload to the clipboard
/// (task 4.1). The menu only shows a truncated line; this is where the
/// full text comes from.
pub fn copy_brief(
    status: &StatusResponse,
    key: &str,
    clipboard: &dyn Clipboard,
) -> Result<String, String> {
    let entry = status
        .prs
        .iter()
        .find(|e| e.key == key)
        .ok_or_else(|| format!("{key} is no longer tracked"))?;
    let text = entry
        .brief
        .as_ref()
        .map(|b| b.text.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} has no brief"))?;
    clipboard.copy(text)?;
    Ok(format!("{key}: brief copied"))
}

/// "Write brief" / "Regenerate brief" (task 4.2): `POST /brief` for `key`.
/// A refusal (unknown PR, already running, repo `off`, briefs disabled)
/// comes back as the daemon's own text, which the menu shows as its error
/// line.
async fn request_brief(key: &str) -> Result<String, String> {
    if key.is_empty() {
        return Err("no PR given — nothing sent".into());
    }
    let client = daemon().await?;
    let response = client
        .brief(key)
        .await
        .map_err(|e| brief_error_text(&format_error(e)))?;
    Ok(summarize_brief(&response))
}

/// A `POST /brief` failure as the user reads it: the daemon's own text
/// (unknown PR, already running, repo `off`, briefs disabled), or an
/// update hint when the daemon predates briefs (a bare 404: no route).
pub fn brief_error_text(error: &str) -> String {
    let text = daemon_error_text(error);
    if text.starts_with("HTTP 404") {
        return "this PRX daemon can't write briefs — update prx and restart the daemon".into();
    }
    text
}

/// The message line for an accepted `POST /brief`.
pub fn summarize_brief(response: &BriefResponse) -> String {
    format!("{}: writing brief\u{2026}", response.key)
}

/// Re-discover the daemon per action (fresh port/token files — heals across
/// daemon restarts exactly like the poller).
async fn daemon() -> Result<DaemonClient, String> {
    DaemonClient::discover()
        .await
        .ok_or_else(|| "daemon offline — action not sent".to_string())
}

/// Run an action future on a fresh current-thread runtime (action threads
/// and the self-test harness have none).
fn block_on(
    future: impl std::future::Future<Output = Result<String, String>>,
) -> Result<String, String> {
    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(future),
        Err(error) => Err(format!("cannot start action runtime: {error}")),
    }
}

/// `POST /review` for `target` (any reference form: `org/repo#N`, URL,
/// `repo#N`, `N`, `#N`) with per-run `overrides`. Force semantics: the
/// explicit menu target bypasses the cooldown; the daemon still holds
/// skip/terminal legality and resolves the target. Its error text (unknown
/// engine, ambiguous candidates, merged PR, …) is returned verbatim, minus
/// the transport's `daemon returned <status>:` prefix.
pub async fn review_target(target: &str, overrides: &ReviewOverrides) -> Result<String, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("no PR reference entered — nothing sent".into());
    }
    let client = daemon().await?;
    let response = client
        .review_with(target, true, overrides)
        .await
        .map_err(|e| daemon_error_text(&format_error(e)))?;
    Ok(summarize_review(target, overrides, response))
}

/// Strip the client's `daemon returned 400 Bad Request: ` prefix so the
/// daemon's own message (shown verbatim to the user) leads.
pub fn daemon_error_text(error: &str) -> String {
    error
        .strip_prefix("daemon returned ")
        .and_then(|rest| rest.split_once(": "))
        .map(|(_, message)| message.to_string())
        .unwrap_or_else(|| error.to_string())
}

/// `POST /review-mode` from the menu's radio group (global default).
async fn set_review_mode(mode: ReviewMode) -> Result<String, String> {
    let client = daemon().await?;
    let response = client.set_review_mode(mode).await.map_err(format_error)?;
    summarize_review_mode(mode, response)
}

fn mode_label(mode: ReviewMode) -> &'static str {
    match mode {
        ReviewMode::Off => "Off",
        ReviewMode::Monitor => "Monitor",
        ReviewMode::ReviewComment => "Review and comment",
    }
}

/// The message line for a mode change. Not persisted (broken config file,
/// design F3) is surfaced as a warning carrying the daemon's note.
fn summarize_review_mode(mode: ReviewMode, response: ReviewModeResponse) -> Result<String, String> {
    let label = mode_label(mode);
    if !response.persisted {
        let note = response
            .note
            .unwrap_or_else(|| "config file has errors".to_string());
        return Err(format!(
            "review mode set to {label} for this session only — not saved: {note}"
        ));
    }
    Ok(match (response.changed, response.note) {
        (_, Some(note)) => note,
        (true, None) => format!("review mode set to {label}"),
        (false, None) => format!("review mode already {label}"),
    })
}

/// `POST /pause {hours}` from the footer submenu (daemon-wide, design D5).
async fn pause(hours: f64) -> Result<String, String> {
    let client = daemon().await?;
    let response = client
        .pause(Some(hours), None)
        .await
        .map_err(format_error)?;
    Ok(summarize_pause(Some(hours), response))
}

/// The message line for a pause/resume (`hours` is `None` for a resume).
/// A daemon note wins over the local wording, as for every mutation.
fn summarize_pause(hours: Option<f64>, response: PauseResponse) -> String {
    if let Some(note) = response.note {
        return note;
    }
    match (hours, response.paused_until) {
        (Some(_), Some(until)) => format!(
            "automatic reviews paused until {}",
            until.with_timezone(&chrono::Local).format("%H:%M")
        ),
        (Some(hours), None) => format!("pause for {hours}h requested"),
        (None, _) if response.changed => "reviews resumed".to_string(),
        (None, _) => "reviews were not paused".to_string(),
    }
}

async fn snooze(key: &str, hours: Option<f64>, until_push: bool) -> Result<String, String> {
    let client = daemon().await?;
    let response = client
        .snooze(key, hours, until_push)
        .await
        .map_err(format_error)?;
    Ok(summarize_mutation(key, "snoozed", response))
}

/// anyhow's alternate formatting (`{:#}`) renders the full cause chain; the
/// error type stays anonymous here (no anyhow dependency in the shell).
fn format_error(error: impl std::fmt::Display) -> String {
    format!("{error:#}")
}

/// The message line for "Check GitHub now": the daemon's note
/// ("poll + due-scan triggered", or "poll triggered; paused until … so
/// nothing will be enqueued") in the menu's words.
pub fn summarize_check(note: Option<&str>) -> String {
    match note.and_then(|n| n.strip_prefix("poll triggered; ")) {
        Some(rest) => format!("checking GitHub \u{2014} {rest}"),
        None => "checking GitHub now".to_string(),
    }
}

fn submenu_hint(action: &str) -> String {
    format!("'{action}' is a submenu row — pick an action inside it")
}

/// Summarize `POST /review` for the message line (the daemon's `note` already
/// explains the nothing-queued case and on-demand tracking). The daemon
/// resolves short targets, so the queued key comes from `enqueued`.
fn summarize_review(target: &str, overrides: &ReviewOverrides, response: ReviewResponse) -> String {
    if let Some(note) = response.note {
        return note;
    }
    let Some(key) = response.enqueued.first() else {
        return format!("{target}: nothing queued");
    };
    let mut options = Vec::new();
    if let Some(engine) = &overrides.engine {
        options.push(engine.clone());
    }
    if overrides.post == Some(false) {
        options.push("not posted".to_string());
    }
    if overrides.extra_prompt.is_some() {
        options.push("with prompt".to_string());
    }
    if options.is_empty() {
        format!("{key}: review queued")
    } else {
        format!("{key}: review queued ({})", options.join(", "))
    }
}

/// Summarize a `MutationResponse`; the daemon's note (e.g. "already approved
/// — nothing to do") wins over any local wording.
fn summarize_mutation(key: &str, verb: &str, response: MutationResponse) -> String {
    if let Some(note) = response.note {
        return note;
    }
    if response.changed {
        format!("{key}: {verb}")
    } else {
        format!("{key}: no change")
    }
}

/// The PR's web URL derived from its state-file key (`org/repo#13` →
/// `https://github.com/org/repo/pull/13`). The status payload does not carry
/// a URL field, and GitHub's web-URL scheme is fixed, so deriving it locally
/// keeps the daemon payload untouched. Also used by the banner click-through
/// (`banners`).
pub(crate) fn github_url(key: &str) -> Result<String, String> {
    let (repo, number) = key
        .split_once('#')
        .ok_or_else(|| format!("malformed PR key '{key}' (want org/repo#13)"))?;
    if repo.is_empty() || repo.contains('#') {
        return Err(format!("malformed PR key '{key}' (want org/repo#13)"));
    }
    let number: u64 = number
        .parse()
        .map_err(|_| format!("malformed PR key '{key}' (number must be numeric)"))?;
    Ok(format!("https://github.com/{repo}/pull/{number}"))
}

/// A GitHub write the menu asks about before sending (audit P1: Approve
/// and Block post a review as the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    /// The alert's title.
    pub title: String,
    /// What happens, in one or two sentences.
    pub body: String,
    /// The confirming button ("Approve", "Block").
    pub button: String,
}

/// The confirmation `id` needs, or `None` when it runs on click. `status`
/// (the last poll) adds the PR title and its last findings.
pub fn confirmation(id: &str, status: Option<&StatusResponse>) -> Option<Confirmation> {
    let (action, key) = id.split_once('|')?;
    if key.is_empty() {
        return None;
    }
    let entry = status.and_then(|s| s.prs.iter().find(|e| e.key == key));
    let title = entry
        .map(|e| format!("{key}: {}", e.title))
        .unwrap_or_else(|| key.to_string());
    match action {
        APPROVE_PREFIX => {
            let mut body =
                format!("PRX posts an approving review on {key} as your GitHub account.");
            if let Some(e) = entry.filter(|e| e.blocking_count > 0) {
                let _ = write!(
                    body,
                    " The last review found {} blocking issue{}.",
                    e.blocking_count,
                    if e.blocking_count == 1 { "" } else { "s" }
                );
            }
            Some(Confirmation {
                title: format!("Approve {title}?"),
                body,
                button: "Approve".into(),
            })
        }
        BLOCK_PREFIX => Some(Confirmation {
            title: format!("Block {title}?"),
            body: format!(
                "PRX posts a request-changes review on {key} as your GitHub account, with \
                 your block message (Settings \u{2192} Messages). Unblock in this menu \
                 dismisses it."
            ),
            button: "Block".into(),
        }),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn approve_and_block_ask_before_posting() {
        let status = status_with_brief(None);
        let approve = confirmation("approve|org/r#5", Some(&status)).unwrap();
        assert_eq!(approve.button, "Approve");
        assert_eq!(approve.title, "Approve org/r#5: t?");
        assert!(approve.body.contains("as your GitHub account"));
        let block = confirmation("block|org/r#5", None).unwrap();
        assert_eq!(block.button, "Block");
        assert_eq!(block.title, "Block org/r#5?");
        // Everything else runs on click, as before.
        for id in [
            "unblock|org/r#5",
            "open|org/r#5",
            "review|org/r#5",
            "ignore|org/r#5",
            "approve|",
            "quit",
        ] {
            assert_eq!(confirmation(id, Some(&status)), None, "{id}");
        }
    }

    #[test]
    fn github_url_derives_the_web_url_from_the_key() {
        assert_eq!(
            github_url("acme/billing#113").unwrap(),
            "https://github.com/acme/billing/pull/113"
        );
        assert_eq!(
            github_url("org/repo#1").unwrap(),
            "https://github.com/org/repo/pull/1"
        );
    }

    #[test]
    fn github_url_rejects_malformed_keys() {
        for key in ["org/repo", "#13", "org/repo#x", ""] {
            assert!(github_url(key).is_err(), "{key} must be rejected");
        }
    }

    #[test]
    fn daemon_notes_win_over_local_wording() {
        let response = MutationResponse {
            ok: true,
            changed: false,
            note: Some("already approved — nothing to do".into()),
        };
        assert_eq!(
            summarize_mutation("org/r#1", "approval posted", response),
            "already approved — nothing to do"
        );
        let changed = MutationResponse {
            ok: true,
            changed: true,
            note: None,
        };
        assert_eq!(
            summarize_mutation("org/r#1", "ignored", changed),
            "org/r#1: ignored"
        );
        let unchanged = MutationResponse {
            ok: true,
            changed: false,
            note: None,
        };
        assert_eq!(
            summarize_mutation("org/r#1", "ignored", unchanged),
            "org/r#1: no change"
        );
    }

    #[test]
    fn review_summary_reports_queue_and_empty_cases() {
        let plain = ReviewOverrides::default();
        let queued = || ReviewResponse {
            enqueued: vec!["org/r#1".into()],
            note: None,
        };
        assert_eq!(
            summarize_review("org/r#1", &plain, queued()),
            "org/r#1: review queued"
        );
        // A bare-number target reports the key the daemon resolved.
        let options = ReviewOverrides {
            engine: Some("codex".into()),
            post: Some(false),
            extra_prompt: Some("x".into()),
        };
        assert_eq!(
            summarize_review("1", &options, queued()),
            "org/r#1: review queued (codex, not posted, with prompt)"
        );
        let noted = ReviewResponse {
            enqueued: vec![],
            note: Some("not eligible".into()),
        };
        assert_eq!(summarize_review("org/r#1", &plain, noted), "not eligible");
        let empty = ReviewResponse {
            enqueued: vec![],
            note: None,
        };
        assert_eq!(summarize_review("42", &plain, empty), "42: nothing queued");
    }

    /// 6.1: every review id parses, engine names with `-`/`.` included;
    /// malformed ids and neighbouring footer ids do not.
    #[test]
    fn review_action_ids_parse() {
        assert_eq!(
            review_action("review|org/r#1"),
            Some(ReviewAction::Plain("org/r#1".into()))
        );
        assert_eq!(
            review_action("review-nopost|org/r#1"),
            Some(ReviewAction::NoPost("org/r#1".into()))
        );
        assert_eq!(
            review_action("review-prompt|org/r#1"),
            Some(ReviewAction::Prompt("org/r#1".into()))
        );
        assert_eq!(review_action("review-pr"), Some(ReviewAction::ReviewPr));
        for engine in ["codex", "my-agent", "gpt-5.1", "a.b-c_d"] {
            assert_eq!(
                review_action(&format!("review-engine|{engine}|my-org/my.repo#10")),
                Some(ReviewAction::Engine {
                    engine: engine.into(),
                    key: "my-org/my.repo#10".into()
                }),
                "{engine}"
            );
        }
        for id in [
            "review-now",
            "review",
            "review|",
            "review-engine|codex",
            "review-engine||org/r#1",
            "review-engine|codex|",
            "rereview|org/r#1",
            "rereview-prompt|org/r#1",
            "review-with",
            "open|org/r#1",
        ] {
            assert_eq!(review_action(id), None, "{id}");
        }
        // Direct actions carry exactly their option; panel actions none.
        let (target, overrides) = review_action("review-engine|codex|o/r#2")
            .unwrap()
            .request()
            .unwrap();
        assert_eq!(target, "o/r#2");
        assert_eq!(overrides.engine.as_deref(), Some("codex"));
        assert_eq!((overrides.post, overrides.extra_prompt), (None, None));
        let (_, overrides) = review_action("review-nopost|o/r#2")
            .unwrap()
            .request()
            .unwrap();
        assert_eq!(overrides.post, Some(false));
        assert_eq!(overrides.engine, None);
        let (_, overrides) = review_action("review|o/r#2").unwrap().request().unwrap();
        assert_eq!(overrides, ReviewOverrides::default());
        assert!(review_action("review-prompt|o/r#2")
            .unwrap()
            .request()
            .is_none());
        assert!(ReviewAction::ReviewPr.request().is_none());
    }

    #[test]
    fn footer_ids_open_the_window_on_their_page() {
        assert_eq!(window_page(OPEN_WINDOW_ID), Some("dashboard"));
        assert_eq!(window_page(SETTINGS_ID), Some("general"));
        assert_eq!(window_page(OPEN_HISTORY_ID), Some("history"));
        assert_eq!(window_page(OPEN_CONFIG_ID), Some("raw"));
        assert_eq!(window_page(OPEN_REPOS_ID), Some("repos"));
        assert_eq!(window_page("review-now"), None);
        assert_eq!(window_page("history"), None);
        assert_eq!(window_page(MODE_PARENT_ID), None);
    }

    #[test]
    fn daemon_error_text_strips_the_transport_prefix() {
        assert_eq!(
            daemon_error_text("daemon returned 400 Bad Request: 42 is ambiguous: a/x#42, b/y#42"),
            "42 is ambiguous: a/x#42, b/y#42"
        );
        assert_eq!(
            daemon_error_text("cannot reach the daemon: refused"),
            "cannot reach the daemon: refused"
        );
    }

    /// Serializes the tests that point `PRX_DATA_DIR` somewhere (process
    /// env is shared across the test threads).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with discovery pointed at `data_dir`, restoring the env.
    /// Shared with the settings bridge tests (same process env).
    pub(crate) fn with_data_dir<T>(data_dir: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var("PRX_DATA_DIR").ok();
        std::env::set_var("PRX_DATA_DIR", data_dir);
        let out = f();
        match previous {
            Some(value) => std::env::set_var("PRX_DATA_DIR", value),
            None => std::env::remove_var("PRX_DATA_DIR"),
        }
        out
    }

    fn block_on_test<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    fn controller() -> ActionController {
        ActionController::new(std::sync::mpsc::channel().0, Arc::new(|_| {}))
    }

    /// Discovery pointed at an empty data dir: only the offline error is
    /// reachable, which proves an id's daemon match arm fired.
    fn perform_offline(id: &str) -> Result<String, String> {
        let dir = std::env::temp_dir().join(format!("prx-menubar-offline-{}", std::process::id()));
        with_data_dir(&dir, || block_on_test(controller().perform(id)))
    }

    const OFFLINE: &str = "daemon offline — action not sent";

    /// The unsnooze id routes to the daemon path (`DaemonClient::unsnooze`),
    /// not the "no action wired" fall-through.
    #[test]
    fn unsnooze_action_id_round_trips_through_the_controller() {
        assert_eq!(
            perform_offline("unsnooze|org/repo#2"),
            Err(OFFLINE.to_string()),
            "unsnooze|key must route to the daemon call, failing only on discovery"
        );
    }

    /// 8.6: Settings… is routed to the shell's in-process window, and the
    /// action path no longer launches an editor for it.
    #[test]
    fn settings_opens_the_window_not_an_editor() {
        assert_eq!(window_page(SETTINGS_ID), Some("general"));
        for id in ["history", "review-now", "page:more|g", "settings|x", ""] {
            assert_eq!(window_page(id), None, "{id}");
        }
        // (The footer item's id is SETTINGS_ID — asserted by the model's
        // footer test.)
        // Were it ever dispatched as an action, nothing is launched: the
        // controller answers immediately with the shell note (no editor
        // process, no daemon call).
        assert_eq!(
            block_on_test(controller().perform(SETTINGS_ID)),
            Err("the window is opened by the menu shell".to_string())
        );
    }

    /// 7.1/7.2: the new ids route to their daemon calls (offline → the
    /// published offline error, never "no action wired").
    #[test]
    fn new_ids_route_to_the_daemon_and_fail_offline() {
        for id in [
            "block|org/repo#2",
            "unblock|org/repo#2",
            "mode:off",
            "mode:monitor",
            "mode:comment",
        ] {
            assert_eq!(perform_offline(id), Err(OFFLINE.to_string()), "{id}");
        }
        // 6.4: every direct review id routes to the daemon.
        for id in [
            "review|org/repo#2",
            "review-engine|codex|org/repo#2",
            "review-nopost|org/repo#2",
        ] {
            assert_eq!(perform_offline(id), Err(OFFLINE.to_string()), "{id}");
        }
        let dir = std::env::temp_dir().join(format!("prx-menubar-offline-{}", std::process::id()));
        let prompted = with_data_dir(&dir, || {
            block_on_test(review_target("org/repo#2", &ReviewOverrides::default()))
        });
        assert_eq!(prompted, Err(OFFLINE.to_string()));
        // An empty reference never reaches discovery.
        assert!(
            block_on_test(review_target("  \n", &ReviewOverrides::default()))
                .unwrap_err()
                .contains("nothing sent")
        );
        // The panel ids themselves are not daemon actions.
        for id in ["review-prompt|org/repo#2", "review-pr"] {
            assert!(
                perform_offline(id).unwrap_err().contains("review panel"),
                "{id}"
            );
        }
        assert!(perform_offline("review-with")
            .unwrap_err()
            .contains("submenu"));
        // The old ids are gone.
        assert!(perform_offline("rereview|org/repo#2")
            .unwrap_err()
            .contains("no action wired"));
    }

    /// A minimal loopback "daemon": answers `GET /status` (discovery probe)
    /// and records every other request, replying with `reply(path)`.
    fn fake_daemon(
        reply: fn(&str) -> String,
    ) -> (std::path::PathBuf, std::sync::mpsc::Receiver<String>) {
        fake_daemon_with_status(
            r#"{"version":"0","pid":1,"uptime_seconds":0,"in_flight":0,"queue_depth":0,"last_poll":{},"last_state_poll":null,"prs":[]}"#.to_string(),
            reply,
        )
    }

    /// [`fake_daemon`] answering `GET /status` with `status`.
    fn fake_daemon_with_status(
        status: String,
        reply: fn(&str) -> String,
    ) -> (std::path::PathBuf, std::sync::mpsc::Receiver<String>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir =
            std::env::temp_dir().join(format!("prx-menubar-fake-{}-{port}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("daemon.port"), port.to_string()).unwrap();
        std::fs::write(dir.join("daemon.token"), "t0k").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap_or("").to_string(),
                    parts.next().unwrap_or("").to_string(),
                );
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let json = if path == "/status" {
                    status.clone()
                } else {
                    let _ = tx.send(format!(
                        "{method} {path} {}",
                        String::from_utf8_lossy(&body)
                    ));
                    reply(&path)
                };
                // A `400:` / `409:` prefix makes the reply that error.
                let (status_line, json) = if let Some(rest) = json.strip_prefix("400:") {
                    ("400 Bad Request", rest.to_string())
                } else if let Some(rest) = json.strip_prefix("409:") {
                    ("409 Conflict", rest.to_string())
                } else {
                    ("200 OK", json)
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{json}",
                    json.len()
                );
            }
        });
        (dir, rx)
    }

    /// "Check GitHub now" reads as what it does, paused or not.
    #[test]
    fn check_github_message_uses_the_menu_words() {
        assert_eq!(
            summarize_check(Some("poll + due-scan triggered")),
            "checking GitHub now"
        );
        assert_eq!(summarize_check(None), "checking GitHub now");
        assert_eq!(
            summarize_check(Some(
                "poll triggered; paused until 18:00 so nothing will be enqueued"
            )),
            "checking GitHub \u{2014} paused until 18:00 so nothing will be enqueued"
        );
    }

    /// 7.1: selecting a mode posts `/review-mode` with that mode.
    #[test]
    fn selecting_a_mode_calls_the_review_mode_endpoint() {
        let (dir, requests) =
            fake_daemon(|_| r#"{"ok":true,"changed":true,"persisted":true}"#.into());
        let result = with_data_dir(&dir, || block_on_test(controller().perform("mode:monitor")));
        assert_eq!(result, Ok("review mode set to Monitor".to_string()));
        let request = requests.try_recv().expect("one recorded request");
        assert!(request.starts_with("POST /review-mode "), "{request}");
        assert!(request.contains(r#""mode":"monitor""#), "{request}");
    }

    /// 7.1 / F3: a not-persisted change surfaces the daemon's "not saved"
    /// note as a warning on the message line.
    #[test]
    fn not_persisted_mode_change_surfaces_the_note() {
        let (dir, _requests) = fake_daemon(|_| {
            r#"{"ok":true,"changed":true,"persisted":false,"note":"not saved: config.yml has errors"}"#.into()
        });
        let result = with_data_dir(&dir, || block_on_test(controller().perform("mode:off")));
        let error = result.expect_err("not persisted is a warning");
        assert!(error.contains("not saved"), "{error}");
        assert!(error.contains("set to Off"), "{error}");
    }

    /// 7.2: block/unblock and the prompted re-review hit their endpoints.
    #[test]
    fn block_unblock_and_prompted_review_hit_their_endpoints() {
        let (dir, requests) = fake_daemon(|path| match path {
            "/review" => r#"{"enqueued":["org/r#5"]}"#.into(),
            _ => r#"{"ok":true,"changed":true}"#.into(),
        });
        let prompt = ReviewOverrides {
            extra_prompt: Some("check the migration".into()),
            ..ReviewOverrides::default()
        };
        let (block, unblock, prompted) = with_data_dir(&dir, || {
            (
                block_on_test(controller().perform("block|org/r#5")),
                block_on_test(controller().perform("unblock|org/r#5")),
                block_on_test(review_target("org/r#5", &prompt)),
            )
        });
        assert_eq!(block, Ok("org/r#5: blocked (changes requested)".into()));
        assert_eq!(unblock, Ok("org/r#5: unblocked".into()));
        assert_eq!(prompted, Ok("org/r#5: review queued (with prompt)".into()));
        let seen: Vec<String> = requests.try_iter().collect();
        assert!(
            seen[0].starts_with("POST /block ") && seen[0].contains("org/r#5"),
            "{seen:?}"
        );
        assert!(seen[1].starts_with("POST /unblock "), "{seen:?}");
        assert!(seen[2].starts_with("POST /review "), "{seen:?}");
        assert!(
            seen[2].contains(r#""extra_prompt":"check the migration""#),
            "{seen:?}"
        );
        assert!(seen[2].contains(r#""force":true"#), "{seen:?}");
    }

    /// 8.3: the footer pause ids post `/pause {hours}` / `/resume`, and the
    /// daemon's note becomes the message line.
    #[test]
    fn pause_and_resume_hit_their_endpoints() {
        let (dir, requests) = fake_daemon(|path| {
            match path {
            "/pause" => r#"{"ok":true,"changed":true,"paused_until":"2026-10-05T12:30:00Z","note":"reviews paused for 30m"}"#.into(),
            _ => r#"{"ok":true,"changed":true,"paused_until":null}"#.into(),
        }
        });
        let results: Vec<Result<String, String>> = with_data_dir(&dir, || {
            ["pause:30m", "pause:1h", "pause:2h", "pause:4h", "resume"]
                .iter()
                .map(|id| block_on_test(controller().perform(id)))
                .collect()
        });
        assert_eq!(results[0], Ok("reviews paused for 30m".into()));
        assert_eq!(results[4], Ok("reviews resumed".into()));
        let seen: Vec<String> = requests.try_iter().collect();
        assert_eq!(seen.len(), 5, "{seen:?}");
        for (request, hours) in seen.iter().zip(["0.5", "1.0", "2.0", "4.0"]) {
            assert!(request.starts_with("POST /pause "), "{request}");
            assert!(
                request.contains(&format!(r#""hours":{hours}"#)),
                "{request}"
            );
        }
        assert!(seen[4].starts_with("POST /resume "), "{seen:?}");
        // Offline: routed to the daemon, never "no action wired".
        assert_eq!(perform_offline("pause:1h"), Err(OFFLINE.to_string()));
        assert!(perform_offline("pause").unwrap_err().contains("submenu"));
    }

    /// 6.4: each review action posts a forced `/review` carrying exactly its
    /// option.
    #[test]
    fn review_actions_post_their_overrides() {
        let (dir, requests) = fake_daemon(|_| r#"{"enqueued":["org/r#5"]}"#.into());
        let results: Vec<Result<String, String>> = with_data_dir(&dir, || {
            [
                "review|org/r#5",
                "review-engine|my-agent|org/r#5",
                "review-nopost|org/r#5",
            ]
            .iter()
            .map(|id| block_on_test(controller().perform(id)))
            .collect()
        });
        assert_eq!(
            results,
            [
                Ok("org/r#5: review queued".to_string()),
                Ok("org/r#5: review queued (my-agent)".to_string()),
                Ok("org/r#5: review queued (not posted)".to_string()),
            ]
        );
        let seen: Vec<serde_json::Value> = requests
            .try_iter()
            .map(|line| {
                assert!(line.starts_with("POST /review "), "{line}");
                serde_json::from_str(line.trim_start_matches("POST /review ")).unwrap()
            })
            .collect();
        assert_eq!(
            seen,
            [
                serde_json::json!({"key": "org/r#5", "force": true}),
                serde_json::json!({"key": "org/r#5", "force": true, "engine": "my-agent"}),
                serde_json::json!({"key": "org/r#5", "force": true, "post": false}),
            ]
        );
    }

    /// 6.4: "Review PR…" passes the typed reference through unparsed, and a
    /// daemon rejection (ambiguous candidates) surfaces as its own text.
    #[test]
    fn review_pr_surfaces_the_daemon_error_text() {
        let (dir, requests) = fake_daemon(|_| {
            r#"400:{"error":"42 matches more than one watched repo: acme/api#42, acme/web#42"}"#
                .into()
        });
        let options = ReviewOverrides {
            engine: Some("codex".into()),
            post: Some(false),
            extra_prompt: Some("focus on auth".into()),
        };
        let result = with_data_dir(&dir, || block_on_test(review_target(" 42 ", &options)));
        assert_eq!(
            result,
            Err("42 matches more than one watched repo: acme/api#42, acme/web#42".to_string())
        );
        let line = requests.try_recv().expect("one request");
        let body: serde_json::Value =
            serde_json::from_str(line.trim_start_matches("POST /review ")).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "key": "42", "force": true, "engine": "codex", "post": false,
                "extra_prompt": "focus on auth"
            })
        );
    }

    #[test]
    fn page_ids_parse_as_local_pagination() {
        assert_eq!(
            page_action("page:more|Needs-review"),
            Some(PageAction::More("Needs-review".into()))
        );
        assert_eq!(
            page_action("page:less|Skipped"),
            Some(PageAction::Less("Skipped".into()))
        );
        assert_eq!(page_action("page:more|"), None);
        assert_eq!(page_action("open|org/repo#1"), None);
        assert_eq!(page_action("settings"), None);
    }

    // ------------------------------------------------ 4.1/4.2 briefs

    fn status_with_brief(text: Option<&str>) -> StatusResponse {
        let mut entry = serde_json::json!({
            "key": "org/r#5", "icon": "✅", "state": "reviewed_approved",
            "skip_reason": null, "title": "t", "author": "alice",
            "head_branch": "b", "is_draft": false, "last_verdict": "approved",
            "blocking_count": 0, "nit_count": 1, "last_review_at": null,
            "summary": "s", "last_review_age": "1m"
        });
        if let Some(text) = text {
            entry["brief"] = serde_json::json!({
                "text": text, "created_at": "2026-10-05T10:00:00Z",
                "updated_at": "2026-10-05T11:00:00Z", "head_sha": "aaaa",
                "source": "review", "stale": false
            });
        }
        serde_json::from_value(serde_json::json!({
            "version": "0", "pid": 1, "uptime_seconds": 0, "in_flight": 0,
            "queue_depth": 0, "last_poll": {}, "last_state_poll": null,
            "prs": [entry]
        }))
        .unwrap()
    }

    /// The full text — not the menu's truncated line — reaches the
    /// clipboard; a missing brief or PR is an error, and copies nothing.
    #[test]
    fn copy_brief_copies_the_full_text() {
        let full = format!("{} end.", "word ".repeat(80));
        let clipboard = crate::clipboard::MemoryClipboard::default();
        let status = status_with_brief(Some(&full));
        assert_eq!(
            copy_brief(&status, "org/r#5", &clipboard),
            Ok("org/r#5: brief copied".to_string())
        );
        assert_eq!(*clipboard.0.lock().unwrap(), vec![full.clone()]);
        assert!(copy_brief(&status, "org/r#6", &clipboard)
            .unwrap_err()
            .contains("no longer tracked"));
        assert!(copy_brief(&status_with_brief(None), "org/r#5", &clipboard)
            .unwrap_err()
            .contains("no brief"));
        assert_eq!(clipboard.0.lock().unwrap().len(), 1);
    }

    /// The drill-down id routes through the controller: `/status` is read
    /// fresh from the daemon and the brief lands on the injected clipboard
    /// (no daemon mutation is sent).
    #[test]
    fn copy_brief_id_reads_status_and_fills_the_clipboard() {
        let full = "Adds a retry loop.\nOne nit about naming.";
        let body = serde_json::to_string(&serde_json::json!({
            "version": "0", "pid": 1, "uptime_seconds": 0, "in_flight": 0,
            "queue_depth": 0, "last_poll": {}, "last_state_poll": null,
            "prs": [serde_json::to_value(&status_with_brief(Some(full)).prs[0]).unwrap()]
        }))
        .unwrap();
        let (dir, requests) = fake_daemon_with_status(body, |_| "{}".into());
        let clipboard = Arc::new(crate::clipboard::MemoryClipboard::default());
        let controller = controller().with_clipboard(clipboard.clone());
        let result = with_data_dir(&dir, || {
            block_on_test(controller.perform(&format!("{COPY_BRIEF_PREFIX}|org/r#5")))
        });
        assert_eq!(result, Ok("org/r#5: brief copied".into()));
        assert_eq!(*clipboard.0.lock().unwrap(), vec![full.to_string()]);
        assert!(requests.try_recv().is_err(), "no daemon mutation");
        // Offline: the daemon is required for the fresh text.
        assert_eq!(
            perform_offline("copy-brief|org/r#5"),
            Err(OFFLINE.to_string())
        );
    }

    /// Task 4.2: "Write brief" / "Regenerate brief" (`brief|{key}`, also the
    /// `--self-test "brief|org/repo#13"` path) posts `/brief` with the key
    /// and reports the acceptance.
    #[test]
    fn brief_id_posts_the_brief_endpoint() {
        let (dir, requests) = fake_daemon(|path| match path {
            "/brief" => r#"{"accepted":true,"key":"org/repo#13"}"#.into(),
            other => panic!("unexpected {other}"),
        });
        let result = with_data_dir(&dir, || {
            block_on_test(controller().perform(&format!("{BRIEF_PREFIX}|org/repo#13")))
        });
        assert_eq!(result, Ok("org/repo#13: writing brief\u{2026}".to_string()));
        let line = requests.try_recv().expect("one request");
        let body: serde_json::Value =
            serde_json::from_str(line.trim_start_matches("POST /brief ")).unwrap();
        assert_eq!(body, serde_json::json!({ "key": "org/repo#13" }));
        assert!(requests.try_recv().is_err(), "exactly one request");
    }

    /// Task 4.2: a refusal (409 already running here; 404/422 alike)
    /// becomes the error line with the daemon's own text, and offline is
    /// the usual "not sent" error.
    #[test]
    fn brief_refusal_surfaces_the_daemon_error_text() {
        let (dir, _requests) = fake_daemon(|_| {
            r#"409:{"error":"a brief for org/repo#13 is already being written"}"#.into()
        });
        let result = with_data_dir(&dir, || {
            block_on_test(controller().perform("brief|org/repo#13"))
        });
        assert_eq!(
            result,
            Err("a brief for org/repo#13 is already being written".to_string())
        );
        assert_eq!(
            perform_offline("brief|org/repo#13"),
            Err(OFFLINE.to_string())
        );
        assert!(perform_offline("brief|").unwrap_err().contains("no PR"));
        // A daemon without the route answers a bare 404.
        assert_eq!(
            brief_error_text("daemon returned 404 Not Found: HTTP 404 Not Found"),
            "this PRX daemon can't write briefs — update prx and restart the daemon"
        );
        assert_eq!(
            brief_error_text("daemon returned 404 Not Found: org/repo#13 is not a tracked PR"),
            "org/repo#13 is not a tracked PR"
        );
    }

    /// The published line for a failed action is the ⚠ error line (the
    /// menu's error banner); success is ✓.
    #[test]
    fn brief_failure_is_published_as_the_error_line() {
        let (dir, _requests) = fake_daemon(|_| r#"409:{"error":"already running"}"#.into());
        let lines = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = lines.clone();
        let (refresh, nudges) = std::sync::mpsc::channel();
        let controller = ActionController::new(
            refresh,
            Arc::new(move |line: String| sink.lock().unwrap().push(line)),
        );
        with_data_dir(&dir, || controller.run("brief|org/repo#13"));
        assert_eq!(
            *lines.lock().unwrap(),
            vec!["⚠ already running".to_string()]
        );
        assert!(
            nudges.try_recv().is_ok(),
            "the menu refreshes after the action"
        );
    }
}
