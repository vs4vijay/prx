//! The menu bar bell (`notifications.menubar_bell`): ready and blocked PRs
//! the user has not opened yet.
//!
//! A PR enters when its review entry arrives in `GET /history`: verdict
//! `approved` makes it **ready** (it waits for the human's approval),
//! `changes_requested` makes it **blocked**. It leaves when the user opens
//! it from PRX (a menu row, a banner click, or a link in the window), or
//! when `/status` shows it is no longer in that state (a new push, a human
//! approval, a merge, a later review with the other verdict).
//!
//! Like the banners' watermark ([`crate::banners::BannerTracker`]), the
//! tracking is in memory only: the backlog seen at the first poll is
//! history, not news, and an app restart starts with an empty bell.
//!
//! Opens that happen off the main thread (a banner click arrives on the
//! notification center's thread) are queued with [`acknowledge_url`] and
//! applied by the app at its next poll outcome, which the call also nudges.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, OnceLock};

use chrono::{DateTime, Utc};

use prx::history::HistoryEntry;
use prx::state::{PrState, Verdict};
use prx::status::StatusEntry;

use crate::poller::PollerHandle;

/// Why a PR rings the bell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BellKind {
    /// The review approved it: it waits for the human's approval.
    Ready,
    /// The review found blocking issues.
    Blocked,
}

impl BellKind {
    /// The kind a history entry puts its PR in, if any. Comment-only
    /// reviews are not ready: nothing approved them.
    fn of(entry: &HistoryEntry) -> Option<BellKind> {
        match entry {
            HistoryEntry::ReviewCompleted {
                verdict: Verdict::Approved,
                ..
            } => Some(BellKind::Ready),
            HistoryEntry::ReviewCompleted {
                verdict: Verdict::ChangesRequested,
                ..
            } => Some(BellKind::Blocked),
            _ => None,
        }
    }

    /// Whether the PR is still in this kind's state: ready is the menu's
    /// "Ready for approval" (the agent approved, no human has yet), blocked
    /// is `reviewed_changes_requested`.
    fn holds(self, pr: &StatusEntry) -> bool {
        match self {
            BellKind::Ready => {
                pr.state == PrState::ReviewedApproved
                    && pr.last_verdict == Some(Verdict::Approved)
                    && pr.human_approved_at.is_none()
            }
            BellKind::Blocked => pr.state == PrState::ReviewedChangesRequested,
        }
    }
}

/// The bell's two counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BellCounts {
    pub ready: usize,
    pub blocked: usize,
}

impl BellCounts {
    pub fn total(&self) -> usize {
        self.ready + self.blocked
    }
}

/// Unacknowledged ready / blocked PRs, keyed by `org/repo#N`.
#[derive(Debug, Default)]
pub struct BellTracker {
    /// Whether the first poll has been seen (its backlog is not news).
    primed: bool,
    /// Newest history timestamp already looked at; `None` while no history
    /// has been seen at all.
    watermark: Option<DateTime<Utc>>,
    unacked: BTreeMap<String, BellKind>,
}

impl BellTracker {
    pub fn new() -> BellTracker {
        BellTracker::default()
    }

    /// Feed one poll: its history (newest first) and its PRs. With the bell
    /// off nothing is tracked, but the watermark still moves, so turning it
    /// on later does not ring for old reviews.
    pub fn update(&mut self, history: &[HistoryEntry], prs: &[StatusEntry], enabled: bool) {
        let newest = history.first().map(HistoryEntry::ts);
        if !self.primed {
            // First look: the backlog is not news.
            self.primed = true;
            self.watermark = newest;
            return;
        }
        let seen = self.watermark;
        self.watermark = newest.max(seen);
        if !enabled {
            self.unacked.clear();
            return;
        }
        // Oldest first, so a PR's newest review decides its kind.
        let mut arrived = BTreeSet::new();
        for entry in history.iter().rev().filter(|e| Some(e.ts()) > seen) {
            if let Some(kind) = BellKind::of(entry) {
                let key = format!("{}#{}", entry.repo(), entry.number());
                self.unacked.insert(key.clone(), kind);
                arrived.insert(key);
            }
        }
        // A PR that left its state leaves the bell. One that arrived in
        // this very poll is kept: the status was read before the history
        // and may not show the review yet.
        self.unacked.retain(|key, kind| {
            arrived.contains(key)
                || prs
                    .iter()
                    .find(|pr| &pr.key == key)
                    .is_some_and(|pr| kind.holds(pr))
        });
    }

    /// The user opened `key`: it no longer rings. Returns whether it did.
    pub fn acknowledge(&mut self, key: &str) -> bool {
        self.unacked.remove(key).is_some()
    }

    #[cfg(test)]
    pub fn kind(&self, key: &str) -> Option<BellKind> {
        self.unacked.get(key).copied()
    }

    pub fn counts(&self) -> BellCounts {
        let count = |kind| self.unacked.values().filter(|k| **k == kind).count();
        BellCounts {
            ready: count(BellKind::Ready),
            blocked: count(BellKind::Blocked),
        }
    }
}

/// Opens reported off the main thread, applied at the next poll outcome.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The poller, nudged after a queued open so the bell updates promptly.
static WAKE: OnceLock<PollerHandle> = OnceLock::new();

/// Register the poller to nudge after [`acknowledge_url`] (once, at start).
pub fn set_waker(poller: PollerHandle) {
    let _ = WAKE.set(poller);
}

/// The PR key of a GitHub pull request URL
/// (`https://github.com/org/repo/pull/13/files` → `org/repo#13`).
pub fn key_of_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let rest = rest.split(['?', '#']).next()?;
    let mut parts = rest.split('/');
    let (org, repo, pull, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if org.is_empty() || repo.is_empty() || pull != "pull" {
        return None;
    }
    let number: u64 = number.parse().ok()?;
    Some(format!("{org}/{repo}#{number}"))
}

/// A PR link was opened from PRX (any thread): queue its acknowledgement.
/// Links that are not pull requests are ignored.
pub fn acknowledge_url(url: &str) {
    let Some(key) = key_of_url(url) else {
        return;
    };
    if let Ok(mut pending) = PENDING.lock() {
        pending.push(key);
    }
    if let Some(poller) = WAKE.get() {
        poller.refresh();
    }
}

/// Take the queued acknowledgements (main thread, per poll outcome).
pub fn take_pending() -> Vec<String> {
    PENDING
        .lock()
        .map(|mut pending| std::mem::take(&mut *pending))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(n: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + n, 0).unwrap()
    }

    fn review(n: i64, key: &str, verdict: Verdict) -> HistoryEntry {
        let (repo, number) = key.split_once('#').unwrap();
        HistoryEntry::ReviewCompleted {
            ts: ts(n),
            repo: repo.into(),
            number: number.parse().unwrap(),
            title: "t".into(),
            author: "a".into(),
            agent: None,
            verdict,
            blocking_count: u32::from(verdict == Verdict::ChangesRequested),
            nit_count: 0,
            duration_seconds: None,
            blocking_texts: Vec::new(),
            brief: None,
            posted: None,
            usage: None,
            summary_usage: None,
        }
    }

    fn pr(key: &str, state: PrState, verdict: Option<Verdict>) -> StatusEntry {
        let mut entry: StatusEntry = serde_json::from_value(serde_json::json!({
            "key": key, "icon": "", "state": "pending", "skip_reason": null, "title": "t",
            "author": "a", "head_branch": "b", "is_draft": false, "last_verdict": null,
            "blocking_count": 0, "nit_count": 0, "last_review_at": null, "summary": "",
            "last_review_age": "",
        }))
        .unwrap();
        entry.state = state;
        entry.last_verdict = verdict;
        entry
    }

    fn ready(key: &str) -> StatusEntry {
        pr(key, PrState::ReviewedApproved, Some(Verdict::Approved))
    }

    fn blocked(key: &str) -> StatusEntry {
        pr(
            key,
            PrState::ReviewedChangesRequested,
            Some(Verdict::ChangesRequested),
        )
    }

    /// Newest first, like `GET /history`.
    fn newest_first(mut entries: Vec<HistoryEntry>) -> Vec<HistoryEntry> {
        entries.sort_by_key(|e| std::cmp::Reverse(e.ts()));
        entries
    }

    #[test]
    fn the_backlog_at_the_first_poll_does_not_ring() {
        let mut bell = BellTracker::new();
        let history = vec![review(1, "o/r#1", Verdict::Approved)];
        bell.update(&history, &[ready("o/r#1")], true);
        assert_eq!(bell.counts(), BellCounts::default());
    }

    #[test]
    fn approved_and_blocking_reviews_ring_separately() {
        let mut bell = BellTracker::new();
        let old = vec![review(1, "o/r#9", Verdict::Approved)];
        bell.update(&old, &[], true);
        let history = newest_first(vec![
            review(1, "o/r#9", Verdict::Approved),
            review(2, "o/r#1", Verdict::Approved),
            review(3, "o/r#2", Verdict::ChangesRequested),
            review(4, "o/r#3", Verdict::CommentOnly),
        ]);
        let prs = [ready("o/r#1"), blocked("o/r#2"), ready("o/r#9")];
        bell.update(&history, &prs, true);
        assert_eq!(
            bell.counts(),
            BellCounts {
                ready: 1,
                blocked: 1
            }
        );
        assert_eq!(bell.kind("o/r#1"), Some(BellKind::Ready));
        assert_eq!(bell.kind("o/r#2"), Some(BellKind::Blocked));
        assert_eq!(bell.kind("o/r#3"), None, "comment-only is not ready");
        // The same entries again ring nothing new.
        bell.update(&history, &prs, true);
        assert_eq!(bell.counts().total(), 2);
    }

    #[test]
    fn opening_the_pr_acknowledges_it() {
        let mut bell = BellTracker::new();
        bell.update(&[], &[], true);
        let history = vec![review(1, "o/r#1", Verdict::Approved)];
        bell.update(&history, &[ready("o/r#1")], true);
        assert!(bell.acknowledge("o/r#1"));
        assert!(!bell.acknowledge("o/r#1"));
        bell.update(&history, &[ready("o/r#1")], true);
        assert_eq!(bell.counts(), BellCounts::default());
    }

    #[test]
    fn a_pr_that_leaves_its_state_leaves_the_bell() {
        let mut bell = BellTracker::new();
        bell.update(&[], &[], true);
        let history = newest_first(vec![
            review(1, "o/r#1", Verdict::Approved),
            review(2, "o/r#2", Verdict::ChangesRequested),
        ]);
        // The status may predate the history in the poll that brings it.
        let stale = [
            pr("o/r#1", PrState::InReview, None),
            pr("o/r#2", PrState::InReview, None),
        ];
        bell.update(&history, &stale, true);
        assert_eq!(bell.counts().total(), 2);
        // Next poll: #1 got a push (pending again), #2 is still blocked.
        bell.update(
            &history,
            &[
                pr("o/r#1", PrState::Pending, Some(Verdict::Approved)),
                blocked("o/r#2"),
            ],
            true,
        );
        assert_eq!(
            bell.counts(),
            BellCounts {
                ready: 0,
                blocked: 1
            }
        );
        // #2 merged.
        bell.update(&history, &[pr("o/r#2", PrState::Merged, None)], true);
        assert_eq!(bell.counts().total(), 0);
    }

    #[test]
    fn a_human_approval_ends_ready() {
        let mut bell = BellTracker::new();
        bell.update(&[], &[], true);
        let history = vec![review(1, "o/r#1", Verdict::Approved)];
        bell.update(&history, &[ready("o/r#1")], true);
        let mut approved = ready("o/r#1");
        approved.human_approved_at = Some(ts(5));
        bell.update(&history, &[approved], true);
        assert_eq!(bell.counts().total(), 0);
    }

    #[test]
    fn a_newer_verdict_moves_the_pr_between_kinds() {
        let mut bell = BellTracker::new();
        bell.update(&[], &[], true);
        let first = vec![review(1, "o/r#1", Verdict::ChangesRequested)];
        bell.update(&first, &[blocked("o/r#1")], true);
        let second = newest_first(vec![
            review(1, "o/r#1", Verdict::ChangesRequested),
            review(2, "o/r#1", Verdict::Approved),
        ]);
        bell.update(&second, &[ready("o/r#1")], true);
        assert_eq!(
            bell.counts(),
            BellCounts {
                ready: 1,
                blocked: 0
            }
        );
    }

    #[test]
    fn the_bell_off_tracks_nothing_and_does_not_ring_old_reviews_later() {
        let mut bell = BellTracker::new();
        bell.update(&[], &[], true);
        let history = vec![review(1, "o/r#1", Verdict::Approved)];
        bell.update(&history, &[ready("o/r#1")], false);
        assert_eq!(bell.counts().total(), 0);
        bell.update(&history, &[ready("o/r#1")], true);
        assert_eq!(bell.counts().total(), 0, "already seen while off");
    }

    #[test]
    fn pr_urls_map_to_keys() {
        assert_eq!(
            key_of_url("https://github.com/acme/api/pull/42").as_deref(),
            Some("acme/api#42")
        );
        assert_eq!(
            key_of_url("https://github.com/acme/api/pull/42/files?w=1#diff").as_deref(),
            Some("acme/api#42")
        );
        for url in [
            "https://github.com/acme/api/issues/42",
            "https://github.com/acme/api",
            "https://example.com/acme/api/pull/42",
            "https://github.com/acme/api/pull/x",
        ] {
            assert_eq!(key_of_url(url), None, "{url}");
        }
    }

    #[test]
    fn queued_acknowledgements_are_taken_once() {
        acknowledge_url("https://github.com/acme/api/pull/7");
        acknowledge_url("https://github.com/acme/api/issues/7");
        let taken = take_pending();
        assert!(taken.contains(&"acme/api#7".to_string()), "{taken:?}");
        assert!(!take_pending().contains(&"acme/api#7".to_string()));
    }
}
