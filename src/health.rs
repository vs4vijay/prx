//! Poll health: whether the daemon is still syncing with GitHub.
//!
//! The daemon records one [`RepoPollHealth`] per polled repo (consecutive
//! failed polls, the last error, when the failure streak began, the last
//! success) and reports them in `GET /status` as `poll_health`. Everything
//! that judges those numbers lives here, so the daemon, `prx status`, and
//! PRX.app agree:
//!
//! - a repo is *failing* after [`HealthThresholds::failing_after`]
//!   consecutive failed polls ([`sync_warning`]);
//! - a restart is *recommended* ([`restart_recommended`]) only when every
//!   polled repo is failing with an environment error (TLS or certificate,
//!   network, timeout) for long enough, and the daemon has been up long
//!   enough. That is the shape of a process whose session or network stack
//!   went stale (a daemon orphaned from an earlier PRX.app, a long sleep),
//!   which a fresh process fixes. An auth error never recommends a restart:
//!   a restart cannot fix it, and `gh auth login` is picked up by the next
//!   poll anyway.
//!
//! Pure functions, no I/O.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Longest error text kept per repo (characters).
pub const ERROR_MAX_CHARS: usize = 300;

/// One repo's poll record, as `/status` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPollHealth {
    /// Failed polls since the last successful one.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// The last poll error (shortened), while the repo is failing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When `last_error` happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error_at: Option<DateTime<Utc>>,
    /// The first failed poll of the current streak.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_since: Option<DateTime<Utc>>,
    /// The last successful poll by this daemon (absent: none yet).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_success: Option<DateTime<Utc>>,
    /// The repo's effective poll interval, so a reader can judge staleness
    /// without the config.
    #[serde(default)]
    pub poll_interval_minutes: u64,
}

impl RepoPollHealth {
    /// A successful poll at `now` ends any failure streak.
    pub fn record_success(&mut self, now: DateTime<Utc>) {
        self.consecutive_failures = 0;
        self.last_error = None;
        self.last_error_at = None;
        self.failing_since = None;
        self.last_success = Some(now);
    }

    /// A failed poll at `now` with `error` (any length; kept shortened).
    pub fn record_failure(&mut self, now: DateTime<Utc>, error: &str) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.last_error = Some(summarize_error(error));
        self.last_error_at = Some(now);
        self.failing_since.get_or_insert(now);
    }

    /// Failing under `thresholds`.
    pub fn is_failing(&self, thresholds: &HealthThresholds) -> bool {
        self.consecutive_failures >= thresholds.failing_after.max(1)
    }
}

/// When failing polls become a warning and when a restart is recommended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthThresholds {
    /// Consecutive failed polls that make a repo *failing*.
    pub failing_after: u32,
    /// How long every repo must have been failing before a restart is
    /// recommended (measured from the newest streak start).
    pub restart_after_streak: Duration,
    /// Minimum daemon uptime before it recommends restarting itself, which
    /// also caps self-restarts to one per this period.
    pub restart_min_uptime: Duration,
}

impl Default for HealthThresholds {
    fn default() -> Self {
        HealthThresholds {
            failing_after: 3,
            restart_after_streak: Duration::from_secs(30 * 60),
            restart_min_uptime: Duration::from_secs(30 * 60),
        }
    }
}

/// What kind of failure an error message describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// gh is not logged in or the token expired: the user has to act, and
    /// the next poll after `gh auth login` recovers without a restart.
    Auth,
    /// TLS or certificate, DNS, connection, or timeout: the process's
    /// environment. A fresh process can fix these.
    Environment,
    /// Anything else (a missing repo, a gh usage error, a parse error).
    Other,
}

const AUTH_MARKERS: &[&str] = &[
    "gh auth login",
    "not logged in",
    "bad credentials",
    "authentication",
    "http 401",
    "requires authentication",
    "token has expired",
];

const ENVIRONMENT_MARKERS: &[&str] = &[
    "tls",
    "x509",
    "certificate",
    "osstatus",
    "errsec",
    "ssl",
    "could not resolve host",
    "no such host",
    "dial tcp",
    "connection refused",
    "connection reset",
    "network is unreachable",
    "network is down",
    "no route to host",
    "i/o timeout",
    "timed out",
    "timeout",
    "proxyconnect",
    "error connecting to",
    "unexpected eof",
];

/// Classify a poll error by its text.
pub fn classify(error: &str) -> ErrorClass {
    let lower = error.to_lowercase();
    if AUTH_MARKERS.iter().any(|m| lower.contains(m)) {
        ErrorClass::Auth
    } else if ENVIRONMENT_MARKERS.iter().any(|m| lower.contains(m)) {
        ErrorClass::Environment
    } else {
        ErrorClass::Other
    }
}

/// A poll error shortened for display: the gh command prefix dropped, the
/// first non-empty line, whitespace collapsed, at most
/// [`ERROR_MAX_CHARS`] characters.
pub fn summarize_error(error: &str) -> String {
    let body = error
        .strip_prefix("gh command failed (")
        .and_then(|rest| rest.split_once("): ").map(|(_, stderr)| stderr))
        .unwrap_or(error);
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("unknown error");
    let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, ERROR_MAX_CHARS)
}

/// `s` cut to `max` characters, with an ellipsis when cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

/// The failing repos in one view, for a warning line or banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncWarning {
    /// Failing repos, sorted.
    pub repos: Vec<String>,
    /// Repos with a poll record (failing or not).
    pub polled: usize,
    /// The earliest streak start among the failing repos.
    pub since: Option<DateTime<Utc>>,
    /// The newest successful poll among the failing repos (`None`: none of
    /// them has synced since the daemon started).
    pub last_success: Option<DateTime<Utc>>,
    /// The longest streak.
    pub failures: u32,
    /// The last error of the repo with the longest streak.
    pub error: String,
}

impl SyncWarning {
    /// "org/one" or "org/one and 2 more"; "all 3 repos" when every polled
    /// repo is failing and there is more than one.
    pub fn repos_label(&self) -> String {
        match self.repos.as_slice() {
            [] => String::new(),
            [one] => one.clone(),
            [first, rest @ ..] if self.repos.len() < self.polled => {
                format!("{first} and {} more", rest.len())
            }
            _ => format!("all {} repos", self.repos.len()),
        }
    }

    /// One line: "GitHub sync failing for 2h (org/one): <error>".
    pub fn headline(&self, now: DateTime<Utc>) -> String {
        let since = self
            .since
            .map(|at| format!(" for {}", format_age(now - at)))
            .unwrap_or_default();
        format!(
            "GitHub sync failing{since} ({}): {}",
            self.repos_label(),
            self.error
        )
    }
}

/// The warning for `health`, or `None` when no repo is failing.
pub fn sync_warning(
    health: &BTreeMap<String, RepoPollHealth>,
    thresholds: &HealthThresholds,
) -> Option<SyncWarning> {
    let failing: Vec<(&String, &RepoPollHealth)> = health
        .iter()
        .filter(|(_, h)| h.is_failing(thresholds))
        .collect();
    let (_, worst) = failing
        .iter()
        .max_by_key(|(name, h)| (h.consecutive_failures, std::cmp::Reverse(*name)))?;
    Some(SyncWarning {
        repos: failing.iter().map(|(name, _)| (*name).clone()).collect(),
        polled: health.len(),
        since: failing.iter().filter_map(|(_, h)| h.failing_since).min(),
        last_success: failing.iter().filter_map(|(_, h)| h.last_success).max(),
        failures: worst.consecutive_failures,
        error: worst
            .last_error
            .clone()
            .unwrap_or_else(|| "unknown error".to_string()),
    })
}

/// The warning lines `prx status` prints on stderr for a daemon's poll
/// health (empty when every repo syncs).
pub fn status_warnings(
    health: &BTreeMap<String, RepoPollHealth>,
    restart_recommended: bool,
    now: DateTime<Utc>,
) -> Vec<String> {
    let thresholds = HealthThresholds::default();
    let Some(warning) = sync_warning(health, &thresholds) else {
        return Vec::new();
    };
    let since = warning
        .since
        .map(|at| format!(" since {}", at.format("%Y-%m-%d %H:%M UTC")))
        .unwrap_or_default();
    let success = match warning.last_success {
        Some(at) => format!("last success {} ago", format_age(now - at)),
        None => "no successful poll since the daemon started".to_string(),
    };
    let mut lines = vec![format!(
        "warning: GitHub polls are failing for {} ({} in a row{since}; {success}), so this data may be stale: {}",
        warning.repos_label(),
        warning.failures,
        warning.error
    )];
    if restart_recommended {
        lines.push(
            "warning: a fresh daemon process should fix this: PRX.app restarts it on its own; \
             otherwise run `prx daemon stop` and then `prx daemon start`"
                .to_string(),
        );
    } else if classify(&warning.error) == ErrorClass::Auth {
        lines.push(
            "warning: gh is not authenticated: run `gh auth login`; the next poll picks it up"
                .to_string(),
        );
    }
    lines
}

/// Whether a fresh daemon process is likely to fix what `health` shows:
/// every polled repo failing with an environment error, the newest streak
/// at least `restart_after_streak` old, and `uptime` at least
/// `restart_min_uptime`.
pub fn restart_recommended(
    health: &BTreeMap<String, RepoPollHealth>,
    uptime: Duration,
    now: DateTime<Utc>,
    thresholds: &HealthThresholds,
) -> bool {
    if health.is_empty() || uptime < thresholds.restart_min_uptime {
        return false;
    }
    let all_environment = health.values().all(|h| {
        h.is_failing(thresholds)
            && h.last_error
                .as_deref()
                .is_some_and(|e| classify(e) == ErrorClass::Environment)
    });
    if !all_environment {
        return false;
    }
    let Some(newest_start) = health.values().filter_map(|h| h.failing_since).max() else {
        return false;
    };
    let streak = (now - newest_start).to_std().unwrap_or_default();
    streak >= thresholds.restart_after_streak
}

/// A compact age: "45m", "5h 10m", "36d".
pub fn format_age(age: chrono::Duration) -> String {
    let minutes = age.num_minutes().max(0);
    let hours = minutes / 60;
    if hours == 0 {
        format!("{minutes}m")
    } else if hours < 48 {
        format!("{hours}h {}m", minutes % 60)
    } else {
        format!("{}d", hours / 24)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TLS: &str = "gh command failed (gh pr list -R org/one --json number): \
                       Post \"https://api.github.com/graphql\": tls: failed to verify \
                       certificate: x509: OSStatus -26276\n";
    const AUTH: &str = "gh command failed (gh pr list -R org/one): gh: To get started \
                        with GitHub CLI, please run: gh auth login";

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn failing(n: u32, since: &str, error: &str) -> RepoPollHealth {
        let mut h = RepoPollHealth {
            poll_interval_minutes: 15,
            ..Default::default()
        };
        let start = at(since);
        for i in 0..n {
            h.record_failure(start + chrono::Duration::minutes(15 * i as i64), error);
        }
        h
    }

    #[test]
    fn a_success_ends_the_streak_and_a_failure_starts_one() {
        let mut h = RepoPollHealth::default();
        h.record_failure(at("2026-11-01T10:00:00Z"), TLS);
        h.record_failure(at("2026-11-01T10:15:00Z"), TLS);
        assert_eq!(h.consecutive_failures, 2);
        assert_eq!(h.failing_since, Some(at("2026-11-01T10:00:00Z")));
        assert_eq!(h.last_error_at, Some(at("2026-11-01T10:15:00Z")));
        assert_eq!(
            h.last_error.as_deref(),
            Some("Post \"https://api.github.com/graphql\": tls: failed to verify certificate: x509: OSStatus -26276")
        );
        h.record_success(at("2026-11-01T10:30:00Z"));
        assert_eq!(h.consecutive_failures, 0);
        assert_eq!(h.last_error, None);
        assert_eq!(h.failing_since, None);
        assert_eq!(h.last_success, Some(at("2026-11-01T10:30:00Z")));
    }

    #[test]
    fn errors_are_classified_auth_first() {
        assert_eq!(classify(TLS), ErrorClass::Environment);
        assert_eq!(classify(AUTH), ErrorClass::Auth);
        assert_eq!(
            classify("gh timed out after 120s running: gh pr list"),
            ErrorClass::Environment
        );
        assert_eq!(
            classify("error connecting to api.github.com"),
            ErrorClass::Environment
        );
        assert_eq!(
            classify("GraphQL: Could not resolve to a Repository with the name 'org/gone'."),
            ErrorClass::Other
        );
        assert_eq!(classify("HTTP 401: Bad credentials"), ErrorClass::Auth);
    }

    #[test]
    fn summaries_are_one_short_line() {
        assert_eq!(
            summarize_error("gh command failed (gh x): \n\n  first   line \nsecond"),
            "first line"
        );
        assert_eq!(summarize_error("plain"), "plain");
        assert_eq!(summarize_error(""), "unknown error");
        let long = "x".repeat(1000);
        let cut = summarize_error(&long);
        assert_eq!(cut.chars().count(), ERROR_MAX_CHARS);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn a_warning_needs_three_failures() {
        let t = HealthThresholds::default();
        let mut map = BTreeMap::new();
        map.insert(
            "org/one".to_string(),
            failing(2, "2026-11-01T10:00:00Z", TLS),
        );
        map.insert("org/two".to_string(), RepoPollHealth::default());
        assert_eq!(sync_warning(&map, &t), None);

        map.insert(
            "org/one".to_string(),
            failing(3, "2026-11-01T10:00:00Z", TLS),
        );
        let w = sync_warning(&map, &t).unwrap();
        assert_eq!(w.repos, vec!["org/one".to_string()]);
        assert_eq!(w.polled, 2);
        assert_eq!(w.failures, 3);
        assert_eq!(w.since, Some(at("2026-11-01T10:00:00Z")));
        assert_eq!(w.repos_label(), "org/one");
        assert!(w.error.starts_with("Post "));
        assert_eq!(
            w.headline(at("2026-11-01T12:05:00Z")),
            format!("GitHub sync failing for 2h 5m (org/one): {}", w.error)
        );
    }

    #[test]
    fn labels_name_one_some_or_all_repos() {
        let t = HealthThresholds::default();
        let mut map = BTreeMap::new();
        for repo in ["org/a", "org/b", "org/c"] {
            map.insert(repo.to_string(), failing(4, "2026-11-01T10:00:00Z", TLS));
        }
        assert_eq!(sync_warning(&map, &t).unwrap().repos_label(), "all 3 repos");
        map.insert("org/d".to_string(), RepoPollHealth::default());
        assert_eq!(
            sync_warning(&map, &t).unwrap().repos_label(),
            "org/a and 2 more"
        );
    }

    #[test]
    fn restart_needs_every_repo_failing_long_enough_with_an_environment_error() {
        let t = HealthThresholds::default();
        let now = at("2026-11-01T12:00:00Z");
        let up = Duration::from_secs(3600);
        let mut map = BTreeMap::new();
        assert!(!restart_recommended(&map, up, now, &t), "nothing polled");

        map.insert("org/a".to_string(), failing(3, "2026-11-01T11:00:00Z", TLS));
        map.insert("org/b".to_string(), failing(3, "2026-11-01T11:20:00Z", TLS));
        assert!(restart_recommended(&map, up, now, &t));

        // Too young a daemon, or too young a streak.
        assert!(!restart_recommended(&map, Duration::from_secs(60), now, &t));
        assert!(!restart_recommended(
            &map,
            up,
            at("2026-11-01T11:40:00Z"),
            &t
        ));

        // One repo still syncing: not the process.
        let mut partly = map.clone();
        partly.insert("org/c".to_string(), RepoPollHealth::default());
        assert!(!restart_recommended(&partly, up, now, &t));

        // Auth expiry: a restart cannot fix it.
        let mut auth = map.clone();
        auth.insert(
            "org/b".to_string(),
            failing(5, "2026-11-01T10:00:00Z", AUTH),
        );
        assert!(!restart_recommended(&auth, up, now, &t));

        // Other errors (a deleted repo) neither.
        let mut other = map;
        other.insert(
            "org/b".to_string(),
            failing(
                5,
                "2026-11-01T10:00:00Z",
                "Could not resolve to a Repository",
            ),
        );
        assert!(!restart_recommended(&other, up, now, &t));
    }

    #[test]
    fn status_warnings_name_the_failure_and_the_fix() {
        let now = at("2026-11-13T12:00:00Z");
        let mut map = BTreeMap::new();
        assert!(status_warnings(&map, false, now).is_empty());
        let mut h = failing(4, "2026-11-13T10:00:00Z", TLS);
        h.last_success = Some(at("2026-10-01T00:00:00Z"));
        map.insert("org/one".to_string(), h);
        let lines = status_warnings(&map, true, now);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with(
            "warning: GitHub polls are failing for org/one (4 in a row since 2026-11-13 10:00 UTC; last success 43d ago)"
        ), "{}", lines[0]);
        assert!(lines[0].contains("x509: OSStatus -26276"));
        assert!(lines[1].contains("prx daemon stop"));

        map.insert(
            "org/one".to_string(),
            failing(3, "2026-11-13T10:00:00Z", AUTH),
        );
        let lines = status_warnings(&map, false, now);
        assert!(lines[0].contains("no successful poll since the daemon started"));
        assert!(lines[1].contains("gh auth login"));
    }

    #[test]
    fn ages_are_compact() {
        assert_eq!(format_age(chrono::Duration::minutes(45)), "45m");
        assert_eq!(format_age(chrono::Duration::minutes(125)), "2h 5m");
        assert_eq!(format_age(chrono::Duration::hours(866)), "36d");
        assert_eq!(format_age(chrono::Duration::minutes(-5)), "0m");
    }

    #[test]
    fn older_payloads_decode() {
        let h: RepoPollHealth = serde_json::from_str("{}").unwrap();
        assert_eq!(h, RepoPollHealth::default());
        let json = serde_json::to_value(RepoPollHealth::default()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"consecutive_failures": 0, "poll_interval_minutes": 0})
        );
    }
}
