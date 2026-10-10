//! Client-side native notifications (spec prx/menu-bar-shell: "Native
//! notifications mapped from tiers").
//!
//! The shell raises macOS banners for NEW history events whose configured
//! tier has `banner: true` — reading the SAME config the daemon uses
//! (`prx::config` through `prx::paths`, so the `PRX_CONFIG` override is
//! honored identically). The client never re-derives tiers; if the config
//! cannot be read it falls back to the documented built-in defaults and logs
//! that. `silent`-tier events (merges, closes, skips by default) never
//! banner — the scenario "a merge produces no banner" is decided purely by
//! the tier table.
//!
//! Delta detection is an in-memory watermark over the history timestamps:
//! on cold start the existing backlog is marked seen WITHOUT bannering (the
//! daemon already raised those banners when the events happened), and only
//! strictly-newer entries fire. The watermark is deliberately not persisted:
//! losing it on restart costs nothing because the daemon re-banners its own
//! events. A burst coalesces to at most [`MAX_BANNERS_PER_TICK`] banners —
//! same-PR events join into one summary, and the tail folds into a single
//! "… more events" banner.
//!
//! Delivery (design A7): from PRX.app with notifications allowed, banners
//! are posted through `UNUserNotificationCenter` ([`crate::notify_center`]),
//! so the sender is "PRX" and a click opens the PR; when the user turned PRX
//! notifications off, nothing is shown. Otherwise (bare binary, prompt not
//! answered, or a bundle macOS will not validate) the fallback below applies.
//!
//! The fallback mirrors the daemon's proven `osascript display notification`
//! path, with one honest upgrade: a raw (non-bundle) binary cannot attach a
//! click action to `display notification`, so when `terminal-notifier` is on
//! PATH it is preferred and its click-through opens the PR on GitHub in the
//! repo's configured browser ([`crate::browser`]: `-open <url>` for the
//! system handler, `-execute` with a fully single-quoted command for an app
//! or command template). Without it banners are click-less (documented
//! limitation).
//!
//! GUARD: [`deliver`] spawns subprocesses and must only be called from the
//! poller thread (`poller::poll_loop`) — never from the GUI thread.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use chrono::{DateTime, Utc};

use prx::config::Browser;
use prx::config::Config;
use prx::history::HistoryEntry;
use prx::ipc::EffectiveConfig;
use prx::notify::{NotifyEvent, Tier, TierBehavior};
use prx::paths;
use prx::state::Verdict;

/// At most this many banners are spawned per poll tick; a burst coalesces
/// instead of stacking (task 4.6: ~3 per tick).
pub const MAX_BANNERS_PER_TICK: usize = 3;

/// Cap for the folded burst banner so a backlog burst cannot produce a
/// banner wider than the screen.
const MAX_AGGREGATE_CHARS: usize = 200;

/// One planned banner: pure data, fully decided before any subprocess runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// PR key the event names (`org/repo#13`); empty for the burst
    /// aggregate banner.
    pub key: String,
    pub title: String,
    pub body: String,
    /// Sound name from the tier table, when the tier sets one.
    pub sound: Option<String>,
    /// GitHub web URL for click-through (used by terminal-notifier only).
    pub url: Option<String>,
}

/// The daemon's event → notification wiring (machine.rs), mirrored from the
/// history record the daemon wrote for that event: a `changes_requested`
/// verdict is the blocking event, any other completed review is the approved
/// one (comment-only shares the approved state — there is no separate
/// reviewed_comment_only state, machine.rs §review_completed).
fn notify_event(entry: &HistoryEntry) -> Option<NotifyEvent> {
    Some(match entry {
        HistoryEntry::ReviewCompleted { verdict, .. } => match verdict {
            Verdict::ChangesRequested => NotifyEvent::ReviewCompleteBlocking,
            Verdict::Approved | Verdict::CommentOnly => NotifyEvent::ReviewCompleteApproved,
        },
        HistoryEntry::ReviewStarted { .. } => NotifyEvent::ReviewStarted,
        HistoryEntry::ReviewFailed { .. } => NotifyEvent::Failure,
        HistoryEntry::ReviewSkipped { .. } => NotifyEvent::Skipped,
        HistoryEntry::PrMerged { .. } => NotifyEvent::Merged,
        HistoryEntry::PrClosed { .. } => NotifyEvent::Closed,
        HistoryEntry::NewCommits { .. } => NotifyEvent::NewCommits,
        // An on-demand brief run (design B3) has no notification event.
        HistoryEntry::Brief { .. } => return None,
    })
}

/// The configured tier for one history entry (event mapping + the entry's
/// repo's effective `notifications` mapping — org/repo overrides included —
/// exactly what the daemon's effect executor consults).
fn tier_of(entry: &HistoryEntry, event: NotifyEvent, config: &Config) -> Tier {
    config.notifications_for(entry.repo()).tier_for(event)
}

fn tier_name(tier: Tier) -> &'static str {
    match tier {
        Tier::Urgent => "urgent",
        Tier::Informational => "informational",
        Tier::Silent => "silent",
    }
}

fn pr_key(entry: &HistoryEntry) -> String {
    format!("{}#{}", entry.repo(), entry.number())
}

/// One-line event summary (the banner title carries the PR key, so the body
/// describes the event; wording follows the daemon's notification copy).
fn event_summary(entry: &HistoryEntry) -> String {
    match entry {
        HistoryEntry::ReviewCompleted {
            verdict,
            blocking_count,
            nit_count,
            ..
        } => match verdict {
            Verdict::ChangesRequested => {
                format!(
                    "Changes requested: {}",
                    counted(*blocking_count, "blocking issue")
                )
            }
            Verdict::Approved => "Approved: no blocking issues".to_string(),
            Verdict::CommentOnly => {
                format!(
                    "Commented: {}, no blocking issues",
                    counted(*nit_count, "nit")
                )
            }
        },
        HistoryEntry::ReviewStarted { .. } => "Review started".to_string(),
        HistoryEntry::ReviewFailed { .. } => {
            "Review failed. The details are in PRX, under History.".to_string()
        }
        HistoryEntry::ReviewSkipped { reason, .. } => format!("Review skipped: {reason}"),
        HistoryEntry::PrMerged { .. } => "Merged".to_string(),
        HistoryEntry::PrClosed { .. } => "Closed".to_string(),
        HistoryEntry::NewCommits { .. } => "New commits: re-review pending".to_string(),
        HistoryEntry::Brief { .. } => "Brief written".to_string(),
    }
}

/// "1 nit", "3 nits".
fn counted(n: u32, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// The banner decision for ONE event under the loaded config: `None` when the
/// event's tier does not banner. Logs the decision — the live-run evidence
/// that a silent-tier event was considered and skipped.
pub fn plan(entry: &HistoryEntry, config: &Config) -> Option<Banner> {
    let event = notify_event(entry)?;
    // Master switch (design D6): global, defaults-only, delivery-only — the
    // client raises nothing for ANY event while it is off.
    if !config.defaults.notifications.enabled {
        eprintln!(
            "prx-menubar: notify {} {} -> notifications disabled (defaults.notifications.enabled: false) -> no banner",
            pr_key(entry),
            event.config_key()
        );
        return None;
    }
    let tier = tier_of(entry, event, config);
    let behavior: &TierBehavior = config.notification_tiers.behavior(tier);
    eprintln!(
        "prx-menubar: notify {} {} -> tier {} -> {}",
        pr_key(entry),
        event.config_key(),
        tier_name(tier),
        if behavior.banner {
            "banner"
        } else {
            "no banner"
        }
    );
    if !behavior.banner {
        return None;
    }
    let key = pr_key(entry);
    Some(Banner {
        title: format!("PRX — {key}"),
        body: event_summary(entry),
        sound: behavior.sound.clone(),
        url: crate::actions::github_url(&key).ok(),
        key,
    })
}

/// Delta detector over `GET /history` payloads (newest first). In-memory
/// only: cold start marks the backlog seen without bannering it.
pub struct BannerTracker {
    /// Timestamp of the newest entry already decided; `None` until the
    /// first poll.
    watermark: Option<DateTime<Utc>>,
}

impl Default for BannerTracker {
    fn default() -> Self {
        BannerTracker::new()
    }
}

impl BannerTracker {
    pub fn new() -> BannerTracker {
        BannerTracker { watermark: None }
    }

    /// Feed one poll's history (newest first, as `GET /history` returns) and
    /// get the banners to raise. An empty slice (history fetch failed) never
    /// moves the watermark backwards.
    pub fn update(&mut self, history: &[HistoryEntry], config: &Config) -> Vec<Banner> {
        let newest = history.first().map(HistoryEntry::ts);
        let Some(seen) = self.watermark else {
            // Cold start: the backlog predates this shell run — mark it seen
            // silently (per-entry decisions logged as the live evidence) and
            // banner nothing.
            self.watermark = newest;
            let mut counts = [0usize; 3];
            for entry in history {
                let Some(event) = notify_event(entry) else {
                    continue;
                };
                let tier = tier_of(entry, event, config);
                eprintln!(
                    "prx-menubar: notifications backlog: {} -> {} (marked seen, no banner)",
                    pr_key(entry),
                    tier_name(tier)
                );
                counts[tier as usize] += 1;
            }
            eprintln!(
                "prx-menubar: notifications cold start: {} backlog event(s) marked seen, \
                 0 banners (urgent: {}, informational: {}, silent: {}){}",
                history.len(),
                counts[Tier::Urgent as usize],
                counts[Tier::Informational as usize],
                counts[Tier::Silent as usize],
                newest
                    .map(|ts| format!("; watermark = {ts}"))
                    .unwrap_or_else(|| "; no history yet".to_string())
            );
            return Vec::new();
        };
        // Advance first so a delivery failure below cannot re-fire the same
        // events on the next poll (at-most-once per process lifetime; the
        // daemon re-banners anyway).
        self.watermark = newest.or(Some(seen));
        let fresh: Vec<HistoryEntry> = history
            .iter()
            .filter(|entry| entry.ts() > seen)
            .cloned()
            .collect();
        if fresh.is_empty() {
            return Vec::new();
        }
        let planned: Vec<Banner> = fresh.iter().filter_map(|e| plan(e, config)).collect();
        coalesce(planned)
    }
}

/// Coalesce a tick's banners into at most [`MAX_BANNERS_PER_TICK`]: events
/// for the same PR always join into one banner (summaries joined with " · ",
/// sound from the newest event), and when distinct PRs still exceed the cap
/// the tail folds into one aggregate banner.
fn coalesce(banners: Vec<Banner>) -> Vec<Banner> {
    // `banners` is newest first; keep the first occurrence's sound/url.
    let mut groups: Vec<Banner> = Vec::new();
    for mut banner in banners {
        match groups.iter_mut().find(|group| group.key == banner.key) {
            Some(group) => {
                group.body.push_str(" · ");
                group.body.push_str(&std::mem::take(&mut banner.body));
            }
            None => groups.push(banner),
        }
    }
    if groups.len() <= MAX_BANNERS_PER_TICK {
        return groups;
    }
    let tail = groups.split_off(MAX_BANNERS_PER_TICK - 1);
    let hidden = tail.len();
    let joined = tail
        .iter()
        .map(|banner| banner.body.as_str())
        .collect::<Vec<_>>()
        .join(" · ");
    groups.push(Banner {
        key: String::new(),
        title: "PRX".to_string(),
        body: truncate_chars(
            &format!("and {hidden} more events: {joined}"),
            MAX_AGGREGATE_CHARS,
        ),
        sound: None,
        url: None,
    });
    groups
}

/// Char-boundary-safe truncation with an ellipsis (mirrors the history
/// layer's finding-text cap).
fn truncate_chars(text: &str, max: usize) -> String {
    if max == 0 || text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('\u{2026}');
    out
}

/// Load the same config file the daemon uses (`PRX_CONFIG` honored via
/// `prx::paths`); on any failure fall back to the documented built-in tier
/// defaults and say so — the shell must never refuse to start over config.
fn load_config_from(path: &Path) -> Config {
    match Config::load(path) {
        Ok(config) => {
            eprintln!(
                "prx-menubar: notifications: config loaded from {}",
                path.display()
            );
            config
        }
        Err(error) => {
            eprintln!(
                "prx-menubar: notifications: config {} unreadable ({error}) — \
                 using built-in tier defaults",
                path.display()
            );
            Config::default()
        }
    }
}

/// The notification config as the poller sees it: loaded once, then
/// re-loaded whenever the config file's (mtime, length) changes, so a
/// `defaults.notifications.enabled` / tier edit saved from the settings
/// window (or by hand) applies on the next poll without restarting the
/// shell. Stat-only when unchanged.
pub struct ConfigSource {
    path: PathBuf,
    stamp: Option<(Option<SystemTime>, u64)>,
    config: Config,
}

impl ConfigSource {
    /// The daemon's config path (`PRX_CONFIG` honored via `prx::paths`).
    pub fn new() -> ConfigSource {
        ConfigSource::at(paths::config_path())
    }

    pub fn at(path: PathBuf) -> ConfigSource {
        let stamp = file_stamp(&path);
        let config = load_config_from(&path);
        ConfigSource {
            path,
            stamp,
            config,
        }
    }

    /// The current config, re-read first when the file changed.
    pub fn current(&mut self) -> &Config {
        let stamp = file_stamp(&self.path);
        if stamp != self.stamp {
            eprintln!("prx-menubar: notifications: config file changed — reloading");
            self.stamp = stamp;
            self.config = load_config_from(&self.path);
        }
        &self.config
    }
}

impl Default for ConfigSource {
    fn default() -> Self {
        ConfigSource::new()
    }
}

fn file_stamp(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok(), meta.len()))
}

/// Raise the banners. GUARD: poller thread only (module docs) — this spawns
/// one subprocess per banner and must never run on the GUI thread.
/// `effective` is the daemon's effective config view used to resolve each
/// PR's browser for click-through (`None` → system handler).
pub fn deliver(banners: &[Banner], effective: Option<&EffectiveConfig>) {
    if banners.is_empty() {
        return;
    }
    // Design A7: PRX.app posts its own banners (sender "PRX") when it runs
    // from its bundle and PRX notifications are allowed; a user who turned
    // them off gets none; otherwise the previous path below.
    if cfg!(target_os = "macos") {
        match crate::notify_center::current_route() {
            crate::notify_center::Route::Center => {
                for banner in banners {
                    eprintln!(
                        "prx-menubar: banner via UNUserNotificationCenter: {} — {} (sound: {}, \
                         click-through: {})",
                        banner.title,
                        banner.body,
                        banner.sound.as_deref().unwrap_or("none"),
                        banner.url.as_deref().unwrap_or("none"),
                    );
                    crate::notify_center::post(banner);
                }
                return;
            }
            crate::notify_center::Route::Drop => {
                for banner in banners {
                    eprintln!(
                        "prx-menubar: banner dropped (notifications are off for PRX in System \
                         Settings): {} — {}",
                        banner.title, banner.body
                    );
                }
                return;
            }
            crate::notify_center::Route::Fallback => {}
        }
    }
    // Decided once per tick: prefer click-through when terminal-notifier is
    // installed, else the daemon's plain osascript path.
    let click_through =
        cfg!(target_os = "macos") && paths::find_in_path("terminal-notifier").is_some();
    for banner in banners {
        let via = if cfg!(target_os = "macos") {
            if click_through {
                "terminal-notifier"
            } else {
                "osascript"
            }
        } else {
            "notify-send"
        };
        eprintln!(
            "prx-menubar: banner via {via}: {} — {} (sound: {}, click-through: {})",
            banner.title,
            banner.body,
            banner.sound.as_deref().unwrap_or("none"),
            banner.url.as_deref().unwrap_or("none"),
        );
        let outcome = if cfg!(target_os = "macos") {
            if click_through {
                let browser = match effective {
                    Some(view) => {
                        crate::browser::for_repo(view, crate::browser::repo_of(&banner.key))
                    }
                    None => Browser::System,
                };
                deliver_terminal_notifier(banner, &browser)
            } else {
                deliver_osascript(banner)
            }
        } else {
            deliver_notify_send(banner)
        };
        if let Err(error) = outcome {
            eprintln!("prx-menubar: banner delivery failed: {error}");
        }
    }
}

/// terminal-notifier's click-through args for `url` under `browser`: the
/// system handler uses `-open <url>` (no shell); an app or command template
/// uses `-execute` with the resolver's argv single-quoted for `/bin/sh -c`.
pub fn click_through_args(url: &str, browser: &Browser) -> [String; 2] {
    match browser {
        Browser::System => ["-open".into(), url.into()],
        other => [
            "-execute".into(),
            crate::browser::shell_command(&crate::browser::argv(other, url)),
        ],
    }
}

/// Click-through path: the PR opens in the configured browser when the
/// banner is clicked ([`click_through_args`]). Args are passed as separate
/// argv entries to terminal-notifier itself.
fn deliver_terminal_notifier(banner: &Banner, browser: &Browser) -> Result<(), String> {
    let mut command = Command::new("terminal-notifier");
    command.arg("-title").arg(&banner.title);
    command.arg("-message").arg(&banner.body);
    if let Some(sound) = &banner.sound {
        command.arg("-sound").arg(sound);
    }
    if let Some(url) = &banner.url {
        command.args(click_through_args(url, browser));
    }
    let output = command
        .output()
        .map_err(|e| format!("cannot run terminal-notifier: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "terminal-notifier exited with {status}: {}",
            String::from_utf8_lossy(&output.stderr).trim(),
            status = output.status
        ))
    }
}

/// Fallback (same mechanism as the daemon's `CommandNotifier`): osascript
/// `display notification`, plus the tier sound when set. A raw binary cannot
/// attach a click action here — banners are click-less (documented).
fn deliver_osascript(banner: &Banner) -> Result<(), String> {
    let mut script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape_applescript(&banner.body),
        escape_applescript(&banner.title)
    );
    if let Some(sound) = &banner.sound {
        script.push_str(&format!(" sound name \"{}\"", escape_applescript(sound)));
    }
    run_command("osascript", &["-e", &script])
}

/// Non-macOS fallback mirroring the daemon's `CommandNotifier` (no
/// click-through); a missing binary fails softly like any delivery error.
fn deliver_notify_send(banner: &Banner) -> Result<(), String> {
    run_command(
        "notify-send",
        &[banner.title.as_str(), banner.body.as_str()],
    )
}

fn run_command(program: &str, args: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited with {status}: {}",
            String::from_utf8_lossy(&output.stderr).trim(),
            status = output.status
        ))
    }
}

/// AppleScript string literal escaping (the daemon's notifier passes trusted
/// titles/bodies; here the aggregate can embed skip reasons, so quote
/// backslashes and double quotes).
fn escape_applescript(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic timestamps minutes apart (newest = highest `n`).
    /// 1767225600 = 2026-01-01T00:00:00Z.
    fn ts(n: u64) -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp_secs(1_767_225_600 + (n * 60) as i64).unwrap()
    }

    fn completed(
        n: u64,
        repo: &str,
        number: u64,
        verdict: Verdict,
        blocking: u32,
        nits: u32,
    ) -> HistoryEntry {
        HistoryEntry::ReviewCompleted {
            ts: ts(n),
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
            agent: None,
            verdict,
            blocking_count: blocking,
            nit_count: nits,
            duration_seconds: None,
            blocking_texts: Vec::new(),
            brief: None,
            posted: None,
            usage: None,
            summary_usage: None,
        }
    }

    fn failed(n: u64, repo: &str, number: u64) -> HistoryEntry {
        HistoryEntry::ReviewFailed {
            ts: ts(n),
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
            agent: None,
            usage: None,
        }
    }

    fn skipped(n: u64, repo: &str, number: u64) -> HistoryEntry {
        HistoryEntry::ReviewSkipped {
            ts: ts(n),
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
            agent: None,
            reason: "diff too large".to_string(),
        }
    }

    fn merged(n: u64, repo: &str, number: u64) -> HistoryEntry {
        HistoryEntry::PrMerged {
            ts: ts(n),
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
        }
    }

    fn closed(n: u64, repo: &str, number: u64) -> HistoryEntry {
        HistoryEntry::PrClosed {
            ts: ts(n),
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
        }
    }

    fn new_commits(n: u64, repo: &str, number: u64) -> HistoryEntry {
        HistoryEntry::NewCommits {
            ts: ts(n),
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
        }
    }

    #[test]
    fn blocking_review_maps_to_urgent_banner_with_sound_and_url() {
        let banner = plan(
            &completed(1, "org/repo", 13, Verdict::ChangesRequested, 2, 1),
            &Config::default(),
        )
        .expect("blocking reviews are urgent -> banner");
        assert_eq!(banner.key, "org/repo#13");
        assert_eq!(banner.title, "PRX — org/repo#13");
        assert_eq!(banner.sound.as_deref(), Some("Basso"));
        assert!(banner.body.contains("2 blocking"), "{}", banner.body);
        // Click-through target for the terminal-notifier path.
        assert_eq!(
            banner.url.as_deref(),
            Some("https://github.com/org/repo/pull/13")
        );
    }

    #[test]
    fn failed_review_maps_to_urgent_too() {
        let banner = plan(&failed(1, "org/repo", 7), &Config::default())
            .expect("on_failure defaults to urgent");
        assert_eq!(banner.key, "org/repo#7");
        assert_eq!(banner.sound.as_deref(), Some("Basso"));
        assert!(banner.body.contains("failed"), "{}", banner.body);
    }

    #[test]
    fn informational_reviews_banner_without_sound() {
        for verdict in [Verdict::Approved, Verdict::CommentOnly] {
            let banner = plan(
                &completed(1, "org/repo", 13, verdict, 0, 2),
                &Config::default(),
            )
            .expect("informational -> banner");
            assert_eq!(banner.sound, None, "informational tier has no sound");
            assert_eq!(
                banner.url.as_deref(),
                Some("https://github.com/org/repo/pull/13")
            );
        }
    }

    #[test]
    fn silent_events_never_banner() {
        let config = Config::default();
        for entry in [
            merged(1, "org/repo", 13),
            closed(1, "org/repo", 13),
            skipped(1, "org/repo", 13),
        ] {
            assert!(
                plan(&entry, &config).is_none(),
                "{entry:?} is silent-tier by default and must not banner"
            );
        }
    }

    #[test]
    fn tier_table_overrides_are_honored() {
        // The user silenced informational banners: no banner, even for
        // completed reviews.
        let mut config = Config::default();
        config.notification_tiers.informational.banner = false;
        assert!(plan(
            &completed(1, "org/repo", 13, Verdict::CommentOnly, 0, 1),
            &config
        )
        .is_none());

        // The user re-tiered merges to urgent: a merge banners with sound.
        let mut config = Config::default();
        config.defaults.notifications.on_merged = Tier::Urgent;
        let banner = plan(&merged(1, "org/repo", 13), &config).expect("re-tiered merge banners");
        assert_eq!(banner.sound.as_deref(), Some("Basso"));
        assert_eq!(banner.body, "Merged");
    }

    #[test]
    fn org_and_repo_notification_overrides_pick_the_tier_per_repo() {
        use prx::config::{OrgConfig, PartialNotifications, PartialOverrides, RepoConfig};
        let mut config = Config::default();
        // Org `acme` re-tiers merges to urgent; repo `acme/quiet` pulls them
        // back to informational; `org/repo` keeps the silent default.
        config.orgs.push(OrgConfig {
            org: "acme".into(),
            overrides: PartialOverrides {
                notifications: Some(PartialNotifications {
                    on_merged: Some(Tier::Urgent),
                    ..PartialNotifications::default()
                }),
                ..PartialOverrides::default()
            },
        });
        config.repos.push(RepoConfig {
            name: "acme/quiet".into(),
            overrides: PartialOverrides {
                notifications: Some(PartialNotifications {
                    on_merged: Some(Tier::Informational),
                    ..PartialNotifications::default()
                }),
                ..PartialOverrides::default()
            },
        });
        let urgent = plan(&merged(1, "acme/web", 1), &config).expect("org override banners");
        assert_eq!(urgent.sound.as_deref(), Some("Basso"));
        let info = plan(&merged(1, "acme/quiet", 2), &config).expect("repo override banners");
        assert_eq!(info.sound, None);
        assert!(plan(&merged(1, "org/repo", 3), &config).is_none());
    }

    #[test]
    fn new_commits_resolve_the_on_new_commits_tier() {
        // Default: informational -> banners without a sound.
        let banner = plan(&new_commits(1, "org/repo", 13), &Config::default())
            .expect("on_new_commits defaults to informational -> banner");
        assert_eq!(banner.key, "org/repo#13");
        assert_eq!(banner.sound, None, "informational tier has no sound");
        assert_eq!(banner.body, "New commits: re-review pending");
        assert_eq!(
            banner.url.as_deref(),
            Some("https://github.com/org/repo/pull/13")
        );

        // Re-tiered to urgent: banners with the urgent sound.
        let mut config = Config::default();
        config.defaults.notifications.on_new_commits = Tier::Urgent;
        let banner =
            plan(&new_commits(1, "org/repo", 13), &config).expect("re-tiered new commits banner");
        assert_eq!(banner.sound.as_deref(), Some("Basso"));

        // Re-tiered to silent (banner: false): no banner at all.
        let mut config = Config::default();
        config.defaults.notifications.on_new_commits = Tier::Silent;
        assert!(plan(&new_commits(1, "org/repo", 13), &config).is_none());
    }

    fn started(n: u64, repo: &str, number: u64) -> HistoryEntry {
        HistoryEntry::ReviewStarted {
            ts: ts(n),
            repo: repo.to_string(),
            number,
            title: "t".to_string(),
            author: "a".to_string(),
            agent: None,
        }
    }

    /// 8.4: with `on_review_started` at a banner tier, a review start that
    /// arrives via history polling raises a client banner (the default
    /// `informational` tier banners without sound; urgent adds its sound).
    #[test]
    fn review_started_banners_at_its_configured_tier() {
        let mut tracker = BannerTracker::new();
        let config = Config::default();
        assert!(tracker
            .update(&[merged(1, "org/repo", 9)], &config)
            .is_empty());
        let banners = tracker.update(
            &[started(2, "org/repo", 13), merged(1, "org/repo", 9)],
            &config,
        );
        assert_eq!(banners.len(), 1, "{banners:?}");
        assert_eq!(banners[0].key, "org/repo#13");
        assert_eq!(banners[0].body, "Review started");
        assert_eq!(banners[0].sound, None);

        let mut urgent = Config::default();
        urgent.defaults.notifications.on_review_started = Tier::Urgent;
        let banner = plan(&started(3, "org/repo", 13), &urgent).expect("urgent banners");
        assert_eq!(banner.sound.as_deref(), Some("Basso"));

        let mut silent = Config::default();
        silent.defaults.notifications.on_review_started = Tier::Silent;
        assert!(plan(&started(3, "org/repo", 13), &silent).is_none());
    }

    /// 8.4: the master switch off → no client banner for ANY new event,
    /// urgent ones included; the watermark still advances (history is
    /// untouched, delivery only).
    #[test]
    fn master_switch_off_raises_no_banner_for_any_event() {
        let mut off = Config::default();
        off.defaults.notifications.enabled = false;
        off.defaults.notifications.on_merged = Tier::Urgent;
        let events = [
            started(2, "org/repo", 1),
            completed(3, "org/repo", 2, Verdict::ChangesRequested, 1, 0),
            completed(4, "org/repo", 3, Verdict::Approved, 0, 0),
            failed(5, "org/repo", 4),
            skipped(6, "org/repo", 5),
            merged(7, "org/repo", 6),
            closed(8, "org/repo", 7),
            new_commits(9, "org/repo", 8),
        ];
        for event in &events {
            assert!(plan(event, &off).is_none(), "{event:?}");
        }
        let mut tracker = BannerTracker::new();
        assert!(tracker.update(&[merged(1, "org/repo", 9)], &off).is_empty());
        let mut newest_first = events.to_vec();
        newest_first.reverse();
        assert!(tracker.update(&newest_first, &off).is_empty());
        // Switching back on later only banners events newer than the
        // watermark — nothing replays.
        assert!(tracker.update(&newest_first, &Config::default()).is_empty());
    }

    /// 8.4: the poller's config source picks up a saved master-switch change
    /// on the next poll (no shell restart).
    #[test]
    fn config_source_reloads_when_the_file_changes() {
        let dir =
            std::env::temp_dir().join(format!("prx-menubar-config-source-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yml");
        std::fs::write(&path, "defaults:\n  notifications:\n    enabled: true\n").unwrap();
        let mut source = ConfigSource::at(path.clone());
        assert!(source.current().defaults.notifications.enabled);
        std::fs::write(
            &path,
            "defaults:\n  notifications:\n    enabled: false   # silenced\n",
        )
        .unwrap();
        assert!(!source.current().defaults.notifications.enabled);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cold_start_marks_the_backlog_seen_without_bannering_it() {
        let mut tracker = BannerTracker::new();
        let backlog = vec![
            completed(3, "org/repo", 13, Verdict::ChangesRequested, 2, 0),
            completed(2, "org/repo", 13, Verdict::CommentOnly, 0, 1),
            merged(1, "org/repo", 9),
        ];
        assert!(
            tracker.update(&backlog, &Config::default()).is_empty(),
            "the cold-start backlog never banners"
        );
        // Re-reading the same history stays quiet as well.
        assert!(tracker.update(&backlog, &Config::default()).is_empty());
    }

    #[test]
    fn watermark_advances_and_only_newer_events_fire() {
        let config = Config::default();
        let mut tracker = BannerTracker::new();
        let old = completed(2, "org/repo", 13, Verdict::CommentOnly, 0, 1);
        assert!(tracker
            .update(std::slice::from_ref(&old), &config)
            .is_empty());

        // The next poll: the old entry again (unchanged) plus one new event.
        let fresh = completed(5, "org/repo", 14, Verdict::ChangesRequested, 1, 0);
        let banners = tracker.update(&[fresh.clone(), old], &config);
        assert_eq!(banners.len(), 1, "only the newer entry fires");
        assert_eq!(banners[0].key, "org/repo#14");
        // Feeding the identical payload again dedupes via the watermark.
        assert!(tracker.update(&[fresh], &config).is_empty());
    }

    #[test]
    fn new_silent_event_after_the_watermark_raises_nothing() {
        let config = Config::default();
        let mut tracker = BannerTracker::new();
        let backlog = completed(2, "org/repo", 13, Verdict::CommentOnly, 0, 1);
        assert!(tracker
            .update(std::slice::from_ref(&backlog), &config)
            .is_empty());
        // A merge lands after the shell is up: silent tier -> no banner.
        let banners = tracker.update(&[merged(6, "org/repo", 13)], &config);
        assert!(
            banners.is_empty(),
            "silent events must not banner: {banners:?}"
        );
    }

    #[test]
    fn an_empty_history_poll_does_not_reset_the_watermark() {
        let config = Config::default();
        let mut tracker = BannerTracker::new();
        let backlog = completed(2, "org/repo", 13, Verdict::CommentOnly, 0, 1);
        assert!(tracker
            .update(std::slice::from_ref(&backlog), &config)
            .is_empty());
        // History fetch failed (empty payload): nothing fires, and the next
        // real poll still distinguishes old from new.
        assert!(tracker.update(&[], &config).is_empty());
        let stale = completed(2, "org/repo", 13, Verdict::CommentOnly, 0, 1);
        assert!(tracker
            .update(std::slice::from_ref(&stale), &config)
            .is_empty());
    }

    #[test]
    fn same_pr_events_join_into_one_banner() {
        let config = Config::default();
        let mut tracker = BannerTracker::new();
        assert!(tracker
            .update(
                &[completed(1, "org/repo", 13, Verdict::CommentOnly, 0, 0)],
                &config
            )
            .is_empty());
        // Two new events for the same PR (newest first): one banner whose
        // body carries both summaries, sound from the newest event.
        let banners = tracker.update(
            &[
                completed(4, "org/repo", 13, Verdict::CommentOnly, 0, 3),
                completed(3, "org/repo", 13, Verdict::ChangesRequested, 1, 0),
            ],
            &config,
        );
        assert_eq!(banners.len(), 1, "{banners:?}");
        assert_eq!(banners[0].key, "org/repo#13");
        assert!(banners[0].body.contains(" · "), "{}", banners[0].body);
        assert!(
            banners[0].body.contains("Commented: 3 nits")
                && banners[0]
                    .body
                    .ends_with("Changes requested: 1 blocking issue"),
            "{}",
            banners[0].body
        );
        assert_eq!(banners[0].sound, None, "newest event's tier wins the sound");
    }

    #[test]
    fn a_burst_folds_into_at_most_three_banners() {
        let config = Config::default();
        let mut tracker = BannerTracker::new();
        assert!(tracker
            .update(
                &[completed(1, "org/repo", 1, Verdict::CommentOnly, 0, 0)],
                &config
            )
            .is_empty());
        // Five new blocking reviews on five distinct PRs, newest first
        // (`GET /history` order): 3 banners total, the last the aggregate.
        let burst: Vec<HistoryEntry> = (2..7u64)
            .rev()
            .map(|n| completed(n, "org/repo", n, Verdict::ChangesRequested, 1, 0))
            .collect();
        let banners = tracker.update(&burst, &config);
        assert_eq!(banners.len(), MAX_BANNERS_PER_TICK, "{banners:?}");
        assert_eq!(banners[0].key, "org/repo#6", "newest first");
        assert_eq!(banners[1].key, "org/repo#5");
        let aggregate = &banners[2];
        assert_eq!(aggregate.key, "");
        assert_eq!(aggregate.title, "PRX");
        assert!(
            aggregate.url.is_none(),
            "the aggregate has no single PR to open"
        );
        assert!(
            aggregate.body.starts_with("and 3 more events:"),
            "{}",
            aggregate.body
        );
    }

    #[test]
    fn applescript_escaping_neuters_quotes_and_backslashes() {
        assert_eq!(
            escape_applescript(r#"say "hi" \ now"#),
            r#"say \"hi\" \\ now"#
        );
    }

    #[test]
    fn aggregate_body_is_truncated() {
        assert_eq!(truncate_chars("abcdef", 6), "abcdef");
        assert_eq!(truncate_chars("abcdef", 5), "abcd\u{2026}");
        assert_eq!(truncate_chars("abcdef", 0), "abcdef");
    }

    #[test]
    fn click_through_honors_the_configured_browser() {
        let url = "https://github.com/o/r/pull/1";
        assert_eq!(click_through_args(url, &Browser::System), ["-open", url]);
        assert_eq!(
            click_through_args(url, &Browser::App("Firefox Nightly".into())),
            [
                "-execute".to_string(),
                format!("'open' '-a' 'Firefox Nightly' '{url}'")
            ]
        );
        assert_eq!(
            click_through_args(
                url,
                &Browser::Command(vec!["arc".into(), "--url={{URL}}".into()])
            ),
            ["-execute".to_string(), format!("'arc' '--url={url}'")]
        );
    }
}
