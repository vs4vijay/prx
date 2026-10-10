//! The pure PR lifecycle transition table (DEV-PLAN §6).
//!
//! This module performs NO I/O and owns NO clock: callers pass `now`, the
//! effective skip config, and an [`Event`]; the machine returns the new
//! [`PrRecord`] plus side-effect intents ([`Effect`]) as data. Callers
//! execute them (notify, history append, post to GitHub).

use chrono::{DateTime, Duration, Utc};
use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::config::{Cooldown, SkipConfig};
use crate::github::PrSummary;
use crate::notify::NotifyEvent;
use crate::state::{PrRecord, PrState, SkipReason, Verdict};

/// What the machine knows about a PR at poll time. `repo` is `org/repo`;
/// `labels` comes from `gh` when available (the list call does not fetch
/// labels, so it is usually empty — the skip machinery still exercises it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSnapshot {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head_branch: String,
    pub head_sha: String,
    pub is_draft: bool,
    #[serde(default)]
    pub labels: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl PrSnapshot {
    pub fn from_summary(repo: &str, s: &PrSummary) -> Self {
        PrSnapshot {
            repo: repo.to_string(),
            number: s.number,
            title: s.title.clone(),
            author: s.author.clone(),
            head_branch: s.head_branch.clone(),
            head_sha: s.head_sha.clone(),
            is_draft: s.is_draft,
            labels: Vec::new(),
            created_at: s.created_at,
        }
    }
}

/// Inputs the machine needs but does not own.
#[derive(Debug, Clone, Copy)]
pub struct MachineCtx<'a> {
    pub now: DateTime<Utc>,
    pub skip: &'a SkipConfig,
    /// The repo's effective engine can resume sessions (design D3): new
    /// commits keep `review_session` so the re-review resumes it. When
    /// false, a new-commit transition clears the stale reference (v0.1).
    pub resume_sessions: bool,
}

/// Side-effect intents returned as data; the caller executes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Notify(NotifyEvent),
    HistoryAppend(HistoryEvent),
}

/// History events persisted to `history.jsonl`. The caller enriches
/// them with the record snapshot (repo, number, title, author, agent...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryEvent {
    /// A review began executing (design D6). Not a review outcome: insights
    /// ignore it; the menubar banners it like `on_review_started`.
    ReviewStarted,
    ReviewCompleted {
        verdict: Verdict,
        blocking_count: u32,
        nit_count: u32,
    },
    ReviewFailed,
    /// Review skipped before the agent ran (e.g. A14 diff-size cap).
    ReviewSkipped {
        reason: String,
    },
    PrMerged,
    PrClosed,
    /// New commits on a reviewed PR (or an until-push snooze wake): the PR is
    /// re-review pending (A5/A9).
    NewCommits,
}

/// Poll and lifecycle events (DEV-PLAN §6).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    PollNewPr {
        snapshot: PrSnapshot,
    },
    PollShaChanged {
        snapshot: PrSnapshot,
    },
    PollUnchanged {
        snapshot: PrSnapshot,
    },
    PollMerged,
    PollClosed,
    PollReopened {
        snapshot: PrSnapshot,
    },
    PollDraftFlipped {
        snapshot: PrSnapshot,
    },
    ReviewStart,
    ReviewDone {
        verdict: Verdict,
        blocking_count: u32,
        nit_count: u32,
        summary: Option<String>,
        /// The head SHA the review actually analyzed.
        reviewed_sha: String,
    },
    ReviewFailed,
    Snooze {
        until: Option<DateTime<Utc>>,
        until_push: bool,
    },
    SnoozeExpired,
    /// User-initiated un-snooze (D1). The restore is exactly snooze expiry's
    /// (A6 `previous_state`, terminal-filtered, default pending); it stays a
    /// distinct event so history/telemetry can tell the two apart, and emits
    /// no effects — the user did it themselves.
    Unsnooze,
    Ignore,
    Unignore,
    /// The human-only approve (A1/D2). The gh approval is posted by the
    /// caller BEFORE this event runs; the machine only records the
    /// attention-state consequence.
    Approve,
    /// The human Block (design D9), mirroring [`Event::Approve`]: the
    /// request-changes review is posted by the caller BEFORE this event
    /// runs; the machine records the blocking state.
    RequestChanges,
    /// The human Unblock (design D9, decision "unblock returns the PR to
    /// pending"): the caller dismissed prx's request-changes review BEFORE
    /// this event runs; a blocked record returns to `pending` (cooldown
    /// anchored now, like any other re-entry). Anything else is a no-op.
    Unblock,
}

/// The outcome of one transition. `record: None` means "nothing changed".
#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub record: Option<PrRecord>,
    pub effects: Vec<Effect>,
}

impl Transition {
    fn unchanged() -> Self {
        Transition {
            record: None,
            effects: Vec::new(),
        }
    }

    fn changed(record: PrRecord, effects: Vec<Effect>) -> Self {
        Transition {
            record: Some(record),
            effects,
        }
    }
}

/// Apply an event to the current record (or `None` for a PR never seen).
pub fn apply(current: Option<&PrRecord>, event: Event, ctx: &MachineCtx) -> Transition {
    match event {
        Event::PollNewPr { snapshot } => poll_new_pr(current, &snapshot, ctx),
        Event::PollShaChanged { snapshot } => poll_sha_changed(current, &snapshot, ctx),
        Event::PollUnchanged { snapshot } => poll_unchanged(current, &snapshot, ctx),
        Event::PollMerged => poll_terminal(current, PrState::Merged, ctx.now),
        Event::PollClosed => poll_terminal(current, PrState::Closed, ctx.now),
        Event::PollReopened { snapshot } => poll_reopened(current, &snapshot, ctx),
        Event::PollDraftFlipped { snapshot } => poll_draft_flipped(current, &snapshot, ctx),
        Event::ReviewStart => review_start(current, ctx.now),
        Event::ReviewDone {
            verdict,
            blocking_count,
            nit_count,
            summary,
            reviewed_sha,
        } => review_done(
            current,
            verdict,
            blocking_count,
            nit_count,
            summary,
            reviewed_sha,
            ctx.now,
        ),
        Event::ReviewFailed => review_failed(current, ctx.now),
        Event::Snooze { until, until_push } => snooze(current, until, until_push, ctx.now),
        Event::SnoozeExpired => snooze_expired(current, ctx.now),
        Event::Unsnooze => unsnooze(current, ctx.now),
        Event::Ignore => ignore(current, ctx.now),
        Event::Unignore => unignore(current, ctx.now),
        Event::Approve => human_verdict(current, PrState::ReviewedApproved, ctx.now),
        Event::RequestChanges => human_verdict(current, PrState::ReviewedChangesRequested, ctx.now),
        Event::Unblock => unblock(current, ctx.now),
    }
}

// ---------------------------------------------------------------- skip rules

/// Evaluate skip rules (DEV-PLAN §4, audits A11/A19). Draft skip wins when
/// several rules match; all non-draft reasons collapse into `Config`.
pub fn evaluate_skip(snapshot: &PrSnapshot, skip: &SkipConfig) -> Option<SkipReason> {
    if skip.drafts && snapshot.is_draft {
        return Some(SkipReason::Draft);
    }
    if skip.authors.iter().any(|a| a == &snapshot.author) {
        return Some(SkipReason::Config);
    }
    // A19: labels are case-insensitive EXACT matches, not prefixes.
    if skip
        .labels
        .iter()
        .any(|l| snapshot.labels.iter().any(|pl| pl.eq_ignore_ascii_case(l)))
    {
        return Some(SkipReason::Config);
    }
    // Branch patterns are globs; invalid patterns were rejected by config
    // validation and are ignored here to keep the machine total.
    if skip.branches.iter().any(|p| {
        Pattern::new(p)
            .map(|p| p.matches(&snapshot.head_branch))
            .unwrap_or(false)
    }) {
        return Some(SkipReason::Config);
    }
    None
}

// ------------------------------------------------------------- poll: new PR

fn poll_new_pr(current: Option<&PrRecord>, snap: &PrSnapshot, ctx: &MachineCtx) -> Transition {
    if let Some(rec) = current {
        // Defensive: the PR is already tracked. Resync fields, keep state.
        let mut rec = rec.clone();
        refresh_from_snapshot(&mut rec, snap, ctx.now);
        return Transition::changed(rec, Vec::new());
    }
    match evaluate_skip(snap, ctx.skip) {
        Some(reason) => {
            let mut rec = record_from_snapshot(snap, PrState::Skipped, ctx.now);
            rec.skip_reason = Some(reason);
            Transition::changed(rec, vec![Effect::Notify(NotifyEvent::Skipped)])
        }
        None => {
            let mut rec = record_from_snapshot(snap, PrState::Pending, ctx.now);
            // A5: first-review cooldown is anchored to GitHub creation time,
            // so pre-existing PRs discovered on the first poll review
            // immediately once their window has passed.
            rec.review_anchor_at = Some(snap.created_at);
            Transition::changed(rec, vec![Effect::Notify(NotifyEvent::NewPr)])
        }
    }
}

// ------------------------------------------------------- poll: sha changed

fn poll_sha_changed(current: Option<&PrRecord>, snap: &PrSnapshot, ctx: &MachineCtx) -> Transition {
    let Some(rec) = current else {
        // Unknown PR with a "changed" sha: treat as a fresh discovery.
        return poll_new_pr(None, snap, ctx);
    };
    let mut rec = rec.clone();
    refresh_from_snapshot(&mut rec, snap, ctx.now);
    match rec.state {
        PrState::Pending => {
            if rec.last_verdict.is_some() {
                // Re-review pending and more commits landed: re-anchor to now
                // so rapid push sessions do not spam reviews (spec §2.3).
                rec.review_anchor_at = Some(ctx.now);
            }
            // First-review pending keeps its creation-time anchor (A5).
            Transition::changed(rec, Vec::new())
        }
        PrState::InReview => {
            // A8: new commits while reviewing — record the pending re-review
            // (head_sha drifts from what the running review analyzed) and let
            // the review finish. Never abort on a push.
            Transition::changed(rec, Vec::new())
        }
        PrState::ReviewedApproved | PrState::ReviewedChangesRequested => {
            // reviewed_* + new commits → re-review pending with the cooldown
            // anchored to commit-detection time (A5).
            enter_pending(&mut rec, ctx.now, ctx.now);
            retain_or_clear_session(&mut rec, ctx);
            Transition::changed(
                rec,
                vec![
                    Effect::HistoryAppend(HistoryEvent::NewCommits),
                    Effect::Notify(NotifyEvent::NewCommits),
                ],
            )
        }
        PrState::Skipped => {
            if rec.skip_reason == Some(SkipReason::Snoozed) && rec.snooze_until_push {
                // A9: "until next push" snooze wakes immediately and is
                // treated as re-review pending.
                enter_pending(&mut rec, ctx.now, ctx.now);
                retain_or_clear_session(&mut rec, ctx);
                Transition::changed(
                    rec,
                    vec![
                        Effect::HistoryAppend(HistoryEvent::NewCommits),
                        Effect::Notify(NotifyEvent::NewCommits),
                    ],
                )
            } else {
                // Timed snooze keeps sleeping; draft/config/ignored stays put.
                Transition::changed(rec, Vec::new())
            }
        }
        // A vanished PR should never appear in the open list; ignore.
        PrState::Merged | PrState::Closed => Transition::unchanged(),
    }
}

/// Design D3: a re-review triggered by new commits resumes the PR's
/// session only when the engine can resume; otherwise the stale reference
/// is dropped so the re-review starts fresh.
fn retain_or_clear_session(rec: &mut PrRecord, ctx: &MachineCtx) {
    if !ctx.resume_sessions {
        rec.review_session = None;
    }
}

// ----------------------------------------------------- poll: no changes

fn poll_unchanged(current: Option<&PrRecord>, snap: &PrSnapshot, ctx: &MachineCtx) -> Transition {
    let Some(rec) = current else {
        // Defensive: caller should have sent PollNewPr.
        return poll_new_pr(None, snap, ctx);
    };
    let mut rec = rec.clone();
    if matches!(rec.state, PrState::Merged | PrState::Closed) {
        return Transition::unchanged();
    }
    refresh_from_snapshot(&mut rec, snap, ctx.now);
    Transition::changed(rec, Vec::new())
}

// ------------------------------------------------------- poll: terminal

fn poll_terminal(current: Option<&PrRecord>, target: PrState, now: DateTime<Utc>) -> Transition {
    let Some(rec) = current else {
        // Never-tracked PR disappearing: nothing to record.
        return Transition::unchanged();
    };
    // Idempotent: already terminal, no duplicate effects on later polls.
    if matches!(rec.state, PrState::Merged | PrState::Closed) {
        return Transition::unchanged();
    }
    let mut rec = rec.clone();
    rec.state = target;
    rec.skip_reason = None;
    rec.snooze_until = None;
    rec.snooze_until_push = false;
    rec.previous_state = None;
    rec.review_session = None;
    rec.updated_at = now;
    let effects = match target {
        PrState::Merged => vec![
            Effect::HistoryAppend(HistoryEvent::PrMerged),
            Effect::Notify(NotifyEvent::Merged),
        ],
        PrState::Closed => vec![
            Effect::HistoryAppend(HistoryEvent::PrClosed),
            Effect::Notify(NotifyEvent::Closed),
        ],
        _ => Vec::new(),
    };
    Transition::changed(rec, effects)
}

// ------------------------------------------------------- poll: reopened

fn poll_reopened(current: Option<&PrRecord>, snap: &PrSnapshot, ctx: &MachineCtx) -> Transition {
    let Some(rec) = current else {
        return poll_new_pr(None, snap, ctx);
    };
    match rec.state {
        PrState::Closed => {
            // A8: reopened after closed → a FRESH pending cycle.
            let mut rec = rec.clone();
            refresh_from_snapshot(&mut rec, snap, ctx.now);
            rec.last_reviewed_sha = None;
            rec.last_review_at = None;
            rec.last_verdict = None;
            rec.last_summary = None;
            rec.blocking_count = 0;
            rec.nit_count = 0;
            rec.review_session = None;
            rec.review_attempts = 0;
            rec.diff_too_large = false;
            match evaluate_skip(snap, ctx.skip) {
                Some(reason) => {
                    rec.state = PrState::Skipped;
                    rec.skip_reason = Some(reason);
                    rec.review_anchor_at = None;
                    Transition::changed(rec, vec![Effect::Notify(NotifyEvent::Skipped)])
                }
                None => {
                    enter_pending(&mut rec, snap.created_at, ctx.now);
                    Transition::changed(rec, vec![Effect::Notify(NotifyEvent::NewPr)])
                }
            }
        }
        // Merged PRs cannot be reopened on GitHub.
        PrState::Merged => Transition::unchanged(),
        // Defensive: not closed, so "reopened" is a plain resync.
        _ => {
            let mut rec = rec.clone();
            refresh_from_snapshot(&mut rec, snap, ctx.now);
            Transition::changed(rec, Vec::new())
        }
    }
}

// ---------------------------------------------------- poll: draft flips

fn poll_draft_flipped(
    current: Option<&PrRecord>,
    snap: &PrSnapshot,
    ctx: &MachineCtx,
) -> Transition {
    let Some(rec) = current else {
        return poll_new_pr(None, snap, ctx);
    };
    let became_draft = snap.is_draft && !rec.is_draft;
    let became_ready = !snap.is_draft && rec.is_draft;
    let mut rec = rec.clone();
    refresh_from_snapshot(&mut rec, snap, ctx.now);

    if became_draft {
        match rec.state {
            // Snoozed/ignored keep their reason; only the draft flag updates.
            PrState::Skipped
                if matches!(
                    rec.skip_reason,
                    Some(SkipReason::Snoozed) | Some(SkipReason::Ignored)
                ) =>
            {
                Transition::changed(rec, Vec::new())
            }
            PrState::Skipped => {
                rec.skip_reason = Some(SkipReason::Draft);
                Transition::changed(rec, Vec::new())
            }
            // §6: `draft: true` from a non-draft → skipped(draft). If a
            // review is running, the caller aborts it when it sees the
            // record left `in_review`.
            PrState::Pending
            | PrState::InReview
            | PrState::ReviewedApproved
            | PrState::ReviewedChangesRequested => {
                if ctx.skip.drafts {
                    rec.state = PrState::Skipped;
                    rec.skip_reason = Some(SkipReason::Draft);
                    rec.snooze_until = None;
                    rec.snooze_until_push = false;
                    rec.previous_state = None;
                }
                Transition::changed(rec, Vec::new())
            }
            PrState::Merged | PrState::Closed => Transition::unchanged(),
        }
    } else if became_ready {
        match rec.state {
            // §6: draft→ready flips Skipped(draft) → pending. Any other skip
            // reason needs its own exit (unignore / snooze expiry).
            PrState::Skipped if rec.skip_reason == Some(SkipReason::Draft) => {
                match evaluate_skip(snap, ctx.skip) {
                    Some(reason) => {
                        rec.skip_reason = Some(reason);
                        Transition::changed(rec, Vec::new())
                    }
                    None => {
                        // First review: creation-time anchor (A5).
                        enter_pending(&mut rec, snap.created_at, ctx.now);
                        Transition::changed(rec, Vec::new())
                    }
                }
            }
            _ => Transition::changed(rec, Vec::new()),
        }
    } else {
        // No actual flip (defensive resync).
        Transition::changed(rec, Vec::new())
    }
}

// ------------------------------------------------------------ review flow

fn review_start(current: Option<&PrRecord>, now: DateTime<Utc>) -> Transition {
    let Some(rec) = current else {
        return Transition::unchanged();
    };
    let mut rec = rec.clone();
    match rec.state {
        // §6: ReviewStart is legal from pending or the reviewed states
        // (force re-review bypasses the cooldown check at the caller).
        PrState::Pending | PrState::ReviewedApproved | PrState::ReviewedChangesRequested => {
            rec.state = PrState::InReview;
            rec.updated_at = now;
            Transition::changed(rec, Vec::new())
        }
        PrState::InReview | PrState::Skipped | PrState::Merged | PrState::Closed => {
            Transition::unchanged()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn review_done(
    current: Option<&PrRecord>,
    verdict: Verdict,
    blocking_count: u32,
    nit_count: u32,
    summary: Option<String>,
    reviewed_sha: String,
    now: DateTime<Utc>,
) -> Transition {
    let Some(rec) = current else {
        return Transition::unchanged();
    };
    let mut rec = rec.clone();
    if rec.state != PrState::InReview {
        return Transition::unchanged();
    }
    // DEV-PLAN §7: any blocking finding forces changes_requested.
    let verdict = if blocking_count > 0 {
        Verdict::ChangesRequested
    } else {
        verdict
    };
    let drifted = rec.head_sha != reviewed_sha;
    rec.last_reviewed_sha = Some(reviewed_sha);
    rec.last_review_at = Some(now);
    rec.last_verdict = Some(verdict);
    rec.last_summary = summary;
    rec.blocking_count = blocking_count;
    rec.nit_count = nit_count;
    rec.review_session = None;
    // A fresh agent verdict supersedes any earlier human approval: the PR
    // is back to "awaiting the human" if the agent approved again.
    rec.human_approved_at = None;
    rec.updated_at = now;

    let (state, mut effects) = match verdict {
        Verdict::ChangesRequested => (
            PrState::ReviewedChangesRequested,
            vec![Effect::Notify(NotifyEvent::ReviewCompleteBlocking)],
        ),
        // comment_only shares the approved state; last_verdict keeps the
        // distinction (there is no separate reviewed_comment_only state).
        Verdict::Approved | Verdict::CommentOnly => (
            PrState::ReviewedApproved,
            vec![Effect::Notify(NotifyEvent::ReviewCompleteApproved)],
        ),
    };
    effects.push(Effect::HistoryAppend(HistoryEvent::ReviewCompleted {
        verdict,
        blocking_count,
        nit_count,
    }));

    if drifted {
        // Commits landed while reviewing (A8): the review is recorded but we
        // collapse straight into re-review pending. Doing it here (instead of
        // waiting for the next poll) prevents a re-anchor loop where every
        // later poll re-detects the drift and resets the cooldown.
        enter_pending(&mut rec, now, now);
    } else {
        rec.state = state;
    }
    Transition::changed(rec, effects)
}

fn review_failed(current: Option<&PrRecord>, now: DateTime<Utc>) -> Transition {
    let Some(rec) = current else {
        return Transition::unchanged();
    };
    let mut rec = rec.clone();
    if rec.state != PrState::InReview {
        return Transition::unchanged();
    }
    // §7 says "state stays put" (no verdict recorded); we return to `pending`
    // with a fresh anchor so the scheduler retries only after a cooldown
    // instead of hot-looping, and track the consecutive failure count.
    rec.state = PrState::Pending;
    rec.review_anchor_at = Some(now);
    rec.review_session = None;
    rec.review_attempts += 1;
    rec.updated_at = now;
    Transition::changed(
        rec,
        vec![
            Effect::Notify(NotifyEvent::Failure),
            Effect::HistoryAppend(HistoryEvent::ReviewFailed),
        ],
    )
}

// -------------------------------------------------------- snooze/ignore

fn snooze(
    current: Option<&PrRecord>,
    until: Option<DateTime<Utc>>,
    until_push: bool,
    now: DateTime<Utc>,
) -> Transition {
    let Some(rec) = current else {
        return Transition::unchanged();
    };
    if until.is_none() && !until_push {
        // A snooze with no schedule does nothing (validated at the boundary).
        return Transition::unchanged();
    }
    let mut rec = rec.clone();
    match rec.state {
        PrState::Pending | PrState::ReviewedApproved | PrState::ReviewedChangesRequested => {
            rec.previous_state = Some(rec.state);
        }
        PrState::InReview => {
            // Snoozing aborts the running review (the caller kills the
            // process when it sees the record leave `in_review`); on expiry
            // we restore to pending with a fresh cooldown.
            rec.previous_state = Some(PrState::Pending);
            rec.review_anchor_at = Some(now);
        }
        // Already skipped (draft/config/ignored/snoozed) or terminal:
        // nothing to snooze, and the original skip reason must survive.
        PrState::Skipped | PrState::Merged | PrState::Closed => return Transition::unchanged(),
    }
    rec.state = PrState::Skipped;
    rec.skip_reason = Some(SkipReason::Snoozed);
    rec.snooze_until = until;
    rec.snooze_until_push = until_push;
    rec.updated_at = now;
    Transition::changed(rec, Vec::new())
}

fn snooze_expired(current: Option<&PrRecord>, now: DateTime<Utc>) -> Transition {
    let Some(rec) = current else {
        return Transition::unchanged();
    };
    let mut rec = rec.clone();
    if rec.state != PrState::Skipped || rec.skip_reason != Some(SkipReason::Snoozed) {
        return Transition::unchanged();
    }
    // A6: restore previous_state. The only reachable stored values are
    // pending and the reviewed states; default to pending defensively.
    rec.state = rec
        .previous_state
        .filter(|s| !matches!(s, PrState::Skipped | PrState::Merged | PrState::Closed))
        .unwrap_or(PrState::Pending);
    rec.skip_reason = None;
    rec.snooze_until = None;
    rec.snooze_until_push = false;
    rec.previous_state = None;
    rec.updated_at = now;
    Transition::changed(rec, Vec::new())
}

/// D1: user-initiated un-snooze. The restore is exactly `snooze_expired`'s
/// (A6 `previous_state` filtered against terminal states, default pending,
/// snooze fields cleared); delegating keeps the two transitions identical by
/// construction. No effects — the user did it themselves.
fn unsnooze(current: Option<&PrRecord>, now: DateTime<Utc>) -> Transition {
    snooze_expired(current, now)
}

fn ignore(current: Option<&PrRecord>, now: DateTime<Utc>) -> Transition {
    let Some(rec) = current else {
        return Transition::unchanged();
    };
    let mut rec = rec.clone();
    // Idempotent; terminal states supersede ignore (an ignored merged PR
    // would be stuck forever since Unignore refuses terminal states).
    if matches!(rec.state, PrState::Merged | PrState::Closed)
        || rec.skip_reason == Some(SkipReason::Ignored)
    {
        return Transition::unchanged();
    }
    rec.state = PrState::Skipped;
    rec.skip_reason = Some(SkipReason::Ignored);
    rec.snooze_until = None;
    rec.snooze_until_push = false;
    rec.previous_state = None;
    rec.updated_at = now;
    Transition::changed(rec, Vec::new())
}

fn unignore(current: Option<&PrRecord>, now: DateTime<Utc>) -> Transition {
    let Some(rec) = current else {
        return Transition::unchanged();
    };
    let mut rec = rec.clone();
    // §6: Unignore → pending (anchor = now), only meaningful from
    // skipped(ignored); everything else is a no-op. Skip rules are
    // re-applied on the next poll event that carries a snapshot.
    if rec.state != PrState::Skipped || rec.skip_reason != Some(SkipReason::Ignored) {
        return Transition::unchanged();
    }
    enter_pending(&mut rec, now, now);
    Transition::changed(rec, Vec::new())
}

/// The human verdict events (menu-bar-shell "Approve from the menu bar",
/// review-lifecycle "Human block posts request changes"): Approve lands a
/// reviewed PR in `reviewed_approved` (out of the attention set); Block
/// (`RequestChanges`) lands it in `reviewed_changes_requested`. No
/// effects — the human IS the event, there is nothing to notify.
fn human_verdict(current: Option<&PrRecord>, target: PrState, now: DateTime<Utc>) -> Transition {
    let Some(rec) = current else {
        // Untracked PR: the gh write already happened; nothing to record.
        return Transition::unchanged();
    };
    let mut rec = rec.clone();
    match rec.state {
        // Legal from pending and both reviewed states. Skip/snooze
        // bookkeeping is cleared the way `unignore`/`enter_pending` clear it
        // so no stale skip state lingers; the agent's review fields
        // (last_verdict, last_reviewed_sha, counts) are NOT touched — they
        // describe the agent's review, not the human decision.
        PrState::Pending | PrState::ReviewedApproved | PrState::ReviewedChangesRequested => {
            rec.state = target;
            // Task 8.1: record the human approval (a Block clears it) so
            // clients can tell it from an agent approval awaiting the human.
            rec.human_approved_at = (target == PrState::ReviewedApproved).then_some(now);
            rec.skip_reason = None;
            rec.snooze_until = None;
            rec.snooze_until_push = false;
            rec.previous_state = None;
            rec.updated_at = now;
            Transition::changed(rec, Vec::new())
        }
        // In review: deciding now would race the in-flight ReviewDone —
        // the agent's verdict owns the next state. Skipped records need an
        // explicit exit (unignore / snooze expiry); merged/closed are
        // terminal.
        PrState::InReview | PrState::Skipped | PrState::Merged | PrState::Closed => {
            Transition::unchanged()
        }
    }
}

/// Unblock: the dismissal already happened on GitHub, so a record in the
/// blocking state (`reviewed_changes_requested`) leaves it for `pending`
/// right away — the menu offers Block again without waiting for a poll.
/// The agent's review fields are untouched (as with the verdict events);
/// every other state, and untracked PRs, are no-ops. No effects.
fn unblock(current: Option<&PrRecord>, now: DateTime<Utc>) -> Transition {
    match current {
        Some(rec) if rec.state == PrState::ReviewedChangesRequested => {
            let mut rec = rec.clone();
            enter_pending(&mut rec, now, now);
            Transition::changed(rec, Vec::new())
        }
        _ => Transition::unchanged(),
    }
}

// ------------------------------------------------------------- helpers

fn refresh_from_snapshot(rec: &mut PrRecord, snap: &PrSnapshot, now: DateTime<Utc>) {
    rec.repo = snap.repo.clone();
    rec.number = snap.number;
    rec.title = snap.title.clone();
    rec.author = snap.author.clone();
    rec.head_branch = snap.head_branch.clone();
    rec.head_sha = snap.head_sha.clone();
    rec.is_draft = snap.is_draft;
    rec.updated_at = now;
}

fn record_from_snapshot(snap: &PrSnapshot, state: PrState, now: DateTime<Utc>) -> PrRecord {
    PrRecord {
        repo: snap.repo.clone(),
        number: snap.number,
        title: snap.title.clone(),
        author: snap.author.clone(),
        head_branch: snap.head_branch.clone(),
        head_sha: snap.head_sha.clone(),
        is_draft: snap.is_draft,
        gh_created_at: snap.created_at,
        state,
        previous_state: None,
        skip_reason: None,
        snooze_until: None,
        snooze_until_push: false,
        review_anchor_at: None,
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

/// Enter `pending` with a specific cooldown anchor, clearing skip/snooze
/// bookkeeping.
fn enter_pending(rec: &mut PrRecord, anchor: DateTime<Utc>, now: DateTime<Utc>) {
    rec.state = PrState::Pending;
    rec.skip_reason = None;
    rec.snooze_until = None;
    rec.snooze_until_push = false;
    rec.previous_state = None;
    // A human approval covers the head it was given on; a PR heading back
    // to review (new commits, failure, unblock) awaits a fresh decision.
    rec.human_approved_at = None;
    rec.review_anchor_at = Some(anchor);
    rec.updated_at = now;
}

// ------------------------------------------------------ cooldown checks

/// Which cooldown applies: first review uses `before_first_review`, anything
/// after a verdict uses `between_re_reviews` (A5).
pub fn applicable_cooldown_minutes(rec: &PrRecord, cooldown: &Cooldown) -> u64 {
    if rec.last_verdict.is_none() {
        cooldown.before_first_review_minutes
    } else {
        cooldown.between_re_reviews_minutes
    }
}

/// True when a `pending` record has waited out its cooldown and is eligible
/// for the review queue. The scheduler decides pickup; the table never does.
pub fn is_review_due(rec: &PrRecord, cooldown: &Cooldown, now: DateTime<Utc>) -> bool {
    if rec.state != PrState::Pending {
        return false;
    }
    match cooldown_ends_at(rec, cooldown) {
        Some(ends) => now >= ends,
        None => true,
    }
}

/// Remaining cooldown for a pending record, if still cooling.
pub fn cooldown_remaining(
    rec: &PrRecord,
    cooldown: &Cooldown,
    now: DateTime<Utc>,
) -> Option<Duration> {
    let ends = cooldown_ends_at(rec, cooldown)?;
    if rec.state != PrState::Pending {
        return None;
    }
    if now >= ends {
        None
    } else {
        Some(ends - now)
    }
}

fn cooldown_ends_at(rec: &PrRecord, cooldown: &Cooldown) -> Option<DateTime<Utc>> {
    let anchor = rec.review_anchor_at?;
    let minutes = applicable_cooldown_minutes(rec, cooldown);
    let duration = Duration::try_minutes(i64::try_from(minutes).ok()?)?;
    anchor.checked_add_signed(duration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Cooldown, SkipConfig};

    const T0: &str = "2026-09-01T10:00:00Z";
    const T0_30M: &str = "2026-09-01T10:30:00Z";
    const T0_61M: &str = "2026-09-01T11:01:00Z";

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn snap(sha: &str) -> PrSnapshot {
        PrSnapshot {
            repo: "org/repo".into(),
            number: 1,
            title: "Add feature".into(),
            author: "alice".into(),
            head_branch: "feat/x".into(),
            head_sha: sha.into(),
            is_draft: false,
            labels: vec![],
            created_at: t(T0),
        }
    }

    fn ctx_at<'a>(now: &str, skip: &'a SkipConfig) -> MachineCtx<'a> {
        MachineCtx {
            now: t(now),
            skip,
            resume_sessions: false,
        }
    }

    /// A record that was discovered by a previous poll (pending, anchor = T0).
    fn tracked() -> PrRecord {
        let skip = SkipConfig::default();
        apply(
            None,
            Event::PollNewPr {
                snapshot: snap("aaaa"),
            },
            &ctx_at(T0, &skip),
        )
        .record
        .unwrap()
    }

    fn record_in(state: PrState, skip_reason: Option<SkipReason>) -> PrRecord {
        let mut rec = tracked();
        rec.state = state;
        rec.skip_reason = skip_reason;
        rec
    }

    fn state_of(tr: &Transition) -> PrState {
        tr.record.as_ref().expect("record expected").state
    }

    fn record_of(tr: &Transition) -> PrRecord {
        tr.record.as_ref().expect("record expected").clone()
    }

    fn notifications(tr: &Transition) -> Vec<NotifyEvent> {
        tr.effects
            .iter()
            .filter_map(|e| match e {
                Effect::Notify(ev) => Some(*ev),
                _ => None,
            })
            .collect()
    }

    fn history(tr: &Transition) -> Vec<HistoryEvent> {
        tr.effects
            .iter()
            .filter_map(|e| match e {
                Effect::HistoryAppend(h) => Some(h.clone()),
                _ => None,
            })
            .collect()
    }

    fn poll(event: Event, rec: &PrRecord, now: &str) -> Transition {
        let skip = SkipConfig::default();
        apply(Some(rec), event, &ctx_at(now, &skip))
    }

    // ------------------------------------------------------- PollNewPr

    #[test]
    fn new_pr_tracks_pending_anchored_to_gh_creation() {
        let skip = SkipConfig::default();
        let tr = apply(
            None,
            Event::PollNewPr {
                snapshot: snap("aaaa"),
            },
            &ctx_at(T0_61M, &skip),
        );
        let rec = record_of(&tr);
        assert_eq!(rec.state, PrState::Pending);
        assert_eq!(rec.review_anchor_at, Some(t(T0))); // A5: gh_created_at, not detection time
        assert_eq!(rec.gh_created_at, t(T0));
        assert_eq!(notifications(&tr), vec![NotifyEvent::NewPr]);
        assert!(history(&tr).is_empty());
    }

    #[test]
    fn new_pr_draft_is_skipped_when_drafts_enabled() {
        let skip = SkipConfig::default();
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = apply(None, Event::PollNewPr { snapshot: s }, &ctx_at(T0, &skip));
        let rec = record_of(&tr);
        assert_eq!(rec.state, PrState::Skipped);
        assert_eq!(rec.skip_reason, Some(SkipReason::Draft));
        assert_eq!(rec.review_anchor_at, None);
        assert_eq!(notifications(&tr), vec![NotifyEvent::Skipped]);
    }

    #[test]
    fn new_pr_draft_is_tracked_when_draft_skip_disabled() {
        let skip = SkipConfig {
            drafts: false,
            ..SkipConfig::default()
        };
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = apply(None, Event::PollNewPr { snapshot: s }, &ctx_at(T0, &skip));
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    #[test]
    fn new_pr_skipped_by_author_exact_match() {
        let skip = SkipConfig {
            authors: vec!["alice".into()],
            ..SkipConfig::default()
        };
        let tr = apply(
            None,
            Event::PollNewPr {
                snapshot: snap("aaaa"),
            },
            &ctx_at(T0, &skip),
        );
        let rec = record_of(&tr);
        assert_eq!(rec.skip_reason, Some(SkipReason::Config));
    }

    #[test]
    fn new_pr_skipped_by_label_case_insensitive() {
        let mut s = snap("aaaa");
        s.labels = vec!["WIP".into()]; // skip list has lowercase "wip" (A19)
        let skip = SkipConfig::default();
        let tr = apply(None, Event::PollNewPr { snapshot: s }, &ctx_at(T0, &skip));
        assert_eq!(record_of(&tr).skip_reason, Some(SkipReason::Config));
    }

    #[test]
    fn new_pr_label_match_is_exact_not_prefix() {
        let mut s = snap("aaaa");
        s.labels = vec!["wipfix".into()]; // prefix of a skipped label but not equal
        let skip = SkipConfig::default();
        let tr = apply(None, Event::PollNewPr { snapshot: s }, &ctx_at(T0, &skip));
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    #[test]
    fn new_pr_skipped_by_branch_glob() {
        let skip = SkipConfig {
            branches: vec!["release/*".into()],
            ..SkipConfig::default()
        };
        let mut s = snap("aaaa");
        s.head_branch = "release/1.2".into();
        let tr = apply(None, Event::PollNewPr { snapshot: s }, &ctx_at(T0, &skip));
        assert_eq!(record_of(&tr).skip_reason, Some(SkipReason::Config));

        let mut other = snap("aaaa");
        other.head_branch = "feature/other".into();
        let tr2 = apply(
            None,
            Event::PollNewPr { snapshot: other },
            &ctx_at(T0, &skip),
        );
        assert_eq!(state_of(&tr2), PrState::Pending);
    }

    #[test]
    fn draft_reason_wins_when_multiple_rules_match() {
        let skip = SkipConfig {
            authors: vec!["alice".into()],
            ..SkipConfig::default()
        };
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = apply(None, Event::PollNewPr { snapshot: s }, &ctx_at(T0, &skip));
        assert_eq!(record_of(&tr).skip_reason, Some(SkipReason::Draft));
    }

    #[test]
    fn poll_new_pr_for_tracked_record_resyncs_without_state_change() {
        let mut rec = record_in(PrState::ReviewedApproved, None);
        rec.title = "Old title".into();
        let tr = poll(
            Event::PollNewPr {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedApproved);
        assert_eq!(out.head_sha, "bbbb");
        assert_eq!(out.title, "Add feature");
        assert!(notifications(&tr).is_empty());
    }

    // -------------------------------------------------- PollShaChanged

    #[test]
    fn sha_change_while_first_pending_keeps_creation_anchor() {
        let rec = tracked(); // anchor = T0
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.head_sha, "bbbb");
        assert_eq!(out.review_anchor_at, Some(t(T0))); // A5: pushes do not re-anchor first review
    }

    #[test]
    fn sha_change_while_rereview_pending_reanchors_to_now() {
        let mut rec = tracked();
        rec.last_verdict = Some(Verdict::Approved);
        rec.review_anchor_at = Some(t(T0));
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.review_anchor_at, Some(t(T0_30M))); // re-review: anchor = detection time
        assert_eq!(out.last_verdict, Some(Verdict::Approved)); // verdict kept for UI
    }

    #[test]
    fn sha_change_while_in_review_records_drift_without_aborting() {
        let mut rec = record_in(PrState::InReview, None);
        rec.head_sha = "aaaa".into();
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::InReview); // let the running review finish (A8)
        assert_eq!(out.head_sha, "bbbb"); // drift recorded for post-review detection
        assert_eq!(out.last_reviewed_sha, None);
        assert!(notifications(&tr).is_empty());
    }

    #[test]
    fn sha_change_from_reviewed_starts_rereview_pending() {
        let mut rec = record_in(PrState::ReviewedChangesRequested, None);
        rec.last_verdict = Some(Verdict::ChangesRequested);
        rec.last_reviewed_sha = Some("aaaa".into());
        rec.review_anchor_at = Some(t(T0));
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert!(out.is_rereview_pending());
        assert_eq!(out.review_anchor_at, Some(t(T0_30M)));
        assert_eq!(notifications(&tr), vec![NotifyEvent::NewCommits]);
    }

    /// Design D3: new commits keep the session only for engines that can
    /// resume; otherwise the stale reference is cleared (v0.1 behavior).
    #[test]
    fn sha_change_retains_session_only_for_resume_capable_engines() {
        let mut rec = record_in(PrState::ReviewedApproved, None);
        rec.last_verdict = Some(Verdict::Approved);
        rec.review_session = Some("sess-1".into());
        let skip = SkipConfig::default();
        let event = || Event::PollShaChanged {
            snapshot: snap("bbbb"),
        };

        let cleared = record_of(&apply(Some(&rec), event(), &ctx_at(T0_30M, &skip)));
        assert_eq!(cleared.state, PrState::Pending);
        assert_eq!(cleared.review_session, None);

        let resuming = MachineCtx {
            resume_sessions: true,
            ..ctx_at(T0_30M, &skip)
        };
        let kept = record_of(&apply(Some(&rec), event(), &resuming));
        assert_eq!(kept.state, PrState::Pending);
        assert_eq!(kept.review_session.as_deref(), Some("sess-1"));
        // The re-review then starts with the retained session in hand.
        let started = record_of(&apply(Some(&kept), Event::ReviewStart, &resuming));
        assert_eq!(started.review_session.as_deref(), Some("sess-1"));

        // An "until next push" snooze waking on new commits follows the
        // same rule.
        let mut snoozed = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        snoozed.snooze_until_push = true;
        snoozed.previous_state = Some(PrState::ReviewedApproved);
        snoozed.review_session = Some("sess-2".into());
        let woke = record_of(&apply(Some(&snoozed), event(), &resuming));
        assert_eq!(woke.state, PrState::Pending);
        assert_eq!(woke.review_session.as_deref(), Some("sess-2"));
        let woke = record_of(&apply(Some(&snoozed), event(), &ctx_at(T0_30M, &skip)));
        assert_eq!(woke.review_session, None);
    }

    #[test]
    fn sha_change_wakes_snooze_until_push() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.snooze_until_push = true;
        rec.snooze_until = None;
        rec.previous_state = Some(PrState::Pending);
        rec.last_verdict = Some(Verdict::Approved);
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending); // A9: wake immediately
        assert!(out.is_rereview_pending());
        assert!(!out.snooze_until_push);
        assert_eq!(out.skip_reason, None);
        assert_eq!(notifications(&tr), vec![NotifyEvent::NewCommits]);
    }

    #[test]
    fn sha_change_from_reviewed_appends_new_commits_history() {
        let mut rec = record_in(PrState::ReviewedApproved, None);
        rec.last_verdict = Some(Verdict::Approved);
        rec.last_reviewed_sha = Some("aaaa".into());
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        // D2: the history entry rides the notification, in one effects list.
        assert_eq!(
            tr.effects,
            vec![
                Effect::HistoryAppend(HistoryEvent::NewCommits),
                Effect::Notify(NotifyEvent::NewCommits)
            ]
        );
    }

    #[test]
    fn sha_change_wakes_until_push_appends_new_commits_history() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.snooze_until_push = true;
        rec.snooze_until = None;
        rec.previous_state = Some(PrState::Pending);
        rec.last_verdict = Some(Verdict::Approved);
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        // D2: the history entry rides the notification, in one effects list.
        assert_eq!(
            tr.effects,
            vec![
                Effect::HistoryAppend(HistoryEvent::NewCommits),
                Effect::Notify(NotifyEvent::NewCommits)
            ]
        );
    }

    #[test]
    fn sha_change_keeps_timed_snooze_asleep() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.snooze_until = Some(t("2026-09-01T12:00:00Z"));
        rec.snooze_until_push = false;
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Skipped);
        assert_eq!(out.skip_reason, Some(SkipReason::Snoozed));
        assert_eq!(out.head_sha, "bbbb"); // fields still refreshed
    }

    #[test]
    fn sha_change_keeps_config_and_ignored_skips() {
        for reason in [SkipReason::Config, SkipReason::Ignored, SkipReason::Draft] {
            let mut rec = record_in(PrState::Skipped, Some(reason));
            rec.head_sha = "aaaa".into();
            let tr = poll(
                Event::PollShaChanged {
                    snapshot: snap("bbbb"),
                },
                &rec,
                T0_30M,
            );
            let out = record_of(&tr);
            assert_eq!(out.state, PrState::Skipped);
            assert_eq!(out.skip_reason, Some(reason));
            assert_eq!(out.head_sha, "bbbb");
        }
    }

    #[test]
    fn sha_change_on_terminal_records_is_ignored() {
        for state in [PrState::Merged, PrState::Closed] {
            let rec = record_in(state, None);
            let tr = poll(
                Event::PollShaChanged {
                    snapshot: snap("bbbb"),
                },
                &rec,
                T0_30M,
            );
            assert_eq!(tr.record, None); // terminal states are frozen
        }
    }

    #[test]
    fn sha_change_for_unknown_pr_discovers_it() {
        let skip = SkipConfig::default();
        let tr = apply(
            None,
            Event::PollShaChanged {
                snapshot: snap("aaaa"),
            },
            &ctx_at(T0, &skip),
        );
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    // --------------------------------------------------- PollUnchanged

    #[test]
    fn poll_unchanged_refreshes_metadata_without_state_change() {
        let mut rec = tracked();
        rec.title = "Old".into();
        let mut s = snap("aaaa");
        s.title = "Renamed".into();
        let tr = poll(Event::PollUnchanged { snapshot: s }, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.title, "Renamed");
        assert_eq!(out.review_anchor_at, Some(t(T0)));
        assert!(tr.effects.is_empty());
    }

    #[test]
    fn poll_unchanged_on_unknown_pr_discovers_it() {
        let skip = SkipConfig::default();
        let tr = apply(
            None,
            Event::PollUnchanged {
                snapshot: snap("aaaa"),
            },
            &ctx_at(T0, &skip),
        );
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    #[test]
    fn poll_unchanged_on_terminal_is_frozen() {
        let rec = record_in(PrState::Merged, None);
        let tr = poll(
            Event::PollUnchanged {
                snapshot: snap("aaaa"),
            },
            &rec,
            T0_30M,
        );
        assert_eq!(tr.record, None);
    }

    // ---------------------------------------------- PollMerged/Closed

    #[test]
    fn merged_from_pending_records_history_and_notify() {
        let rec = tracked();
        let tr = poll(Event::PollMerged, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Merged);
        assert_eq!(out.skip_reason, None);
        assert_eq!(history(&tr), vec![HistoryEvent::PrMerged]);
        assert_eq!(notifications(&tr), vec![NotifyEvent::Merged]);
    }

    #[test]
    fn merged_while_in_review_marks_terminal_for_caller_abort() {
        let rec = record_in(PrState::InReview, None);
        let tr = poll(Event::PollMerged, &rec, T0_30M);
        assert_eq!(state_of(&tr), PrState::Merged); // caller aborts the review process
    }

    #[test]
    fn merged_from_reviewed_and_skipped() {
        let reviewed = record_in(PrState::ReviewedApproved, None);
        let tr = poll(Event::PollMerged, &reviewed, T0_30M);
        assert_eq!(state_of(&tr), PrState::Merged);

        let skipped = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        let tr = poll(Event::PollMerged, &skipped, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Merged);
        assert_eq!(out.snooze_until, None); // snooze bookkeeping cleared
    }

    #[test]
    fn merged_is_idempotent_with_no_duplicate_effects() {
        let rec = record_in(PrState::Merged, None);
        let tr = poll(Event::PollMerged, &rec, T0_30M);
        assert_eq!(tr.record, None);
        assert!(tr.effects.is_empty());
    }

    #[test]
    fn closed_from_pending_records_history_and_notify() {
        let rec = tracked();
        let tr = poll(Event::PollClosed, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Closed);
        assert_eq!(history(&tr), vec![HistoryEvent::PrClosed]);
        assert_eq!(notifications(&tr), vec![NotifyEvent::Closed]);
    }

    #[test]
    fn closed_from_in_review_marks_terminal() {
        let rec = record_in(PrState::InReview, None);
        let tr = poll(Event::PollClosed, &rec, T0_30M);
        assert_eq!(state_of(&tr), PrState::Closed);
    }

    #[test]
    fn poll_merged_for_unknown_record_is_noop() {
        let skip = SkipConfig::default();
        let tr = apply(None, Event::PollMerged, &ctx_at(T0, &skip));
        assert_eq!(tr.record, None);
    }

    // --------------------------------------------------- PollReopened

    #[test]
    fn reopened_after_closed_starts_fresh_pending() {
        let mut rec = record_in(PrState::Closed, None);
        rec.last_verdict = Some(Verdict::ChangesRequested);
        rec.blocking_count = 3;
        rec.last_reviewed_sha = Some("aaaa".into());
        rec.review_attempts = 2;
        let tr = poll(
            Event::PollReopened {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.review_anchor_at, Some(t(T0))); // fresh cycle anchored to creation (A5)
        assert_eq!(out.last_verdict, None); // review fields reset
        assert_eq!(out.last_reviewed_sha, None);
        assert_eq!(out.blocking_count, 0);
        assert_eq!(out.review_attempts, 0);
        assert_eq!(notifications(&tr), vec![NotifyEvent::NewPr]);
    }

    #[test]
    fn reopened_after_closed_respects_skip_rules() {
        let skip = SkipConfig::default(); // drafts skipped
        let mut s = snap("bbbb");
        s.is_draft = true;
        let rec = record_in(PrState::Closed, None);
        let tr = apply(
            Some(&rec),
            Event::PollReopened { snapshot: s },
            &ctx_at(T0, &skip),
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Skipped);
        assert_eq!(out.skip_reason, Some(SkipReason::Draft));
    }

    #[test]
    fn reopened_after_merged_is_ignored() {
        let rec = record_in(PrState::Merged, None);
        let tr = poll(
            Event::PollReopened {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        assert_eq!(tr.record, None); // merged PRs cannot reopen
    }

    #[test]
    fn reopened_from_non_terminal_resyncs() {
        let rec = tracked();
        let tr = poll(
            Event::PollReopened {
                snapshot: snap("aaaa"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
    }

    // ----------------------------------------------- PollDraftFlipped

    #[test]
    fn flip_to_draft_from_pending_skips() {
        let rec = tracked();
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = poll(Event::PollDraftFlipped { snapshot: s }, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Skipped);
        assert_eq!(out.skip_reason, Some(SkipReason::Draft));
        assert!(out.is_draft);
    }

    #[test]
    fn flip_to_draft_from_in_review_skips_and_caller_aborts() {
        let rec = record_in(PrState::InReview, None);
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = poll(Event::PollDraftFlipped { snapshot: s }, &rec, T0_30M);
        assert_eq!(state_of(&tr), PrState::Skipped);
        assert_eq!(record_of(&tr).skip_reason, Some(SkipReason::Draft));
    }

    #[test]
    fn flip_to_draft_from_reviewed_skips() {
        let mut rec = record_in(PrState::ReviewedChangesRequested, None);
        rec.last_verdict = Some(Verdict::ChangesRequested);
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = poll(Event::PollDraftFlipped { snapshot: s }, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Skipped);
        assert_eq!(out.skip_reason, Some(SkipReason::Draft));
        assert_eq!(out.last_verdict, Some(Verdict::ChangesRequested)); // history preserved
    }

    #[test]
    fn flip_to_draft_tracked_when_draft_skip_disabled() {
        let skip = SkipConfig {
            drafts: false,
            ..SkipConfig::default()
        };
        let rec = tracked();
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = apply(
            Some(&rec),
            Event::PollDraftFlipped { snapshot: s },
            &ctx_at(T0_30M, &skip),
        );
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    #[test]
    fn flip_to_ready_from_draft_skip_goes_pending() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Draft));
        rec.is_draft = true;
        rec.gh_created_at = t(T0);
        let tr = poll(
            Event::PollDraftFlipped {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.skip_reason, None);
        assert_eq!(out.review_anchor_at, Some(t(T0))); // first review: creation anchor (A5)
        assert!(!out.is_draft);
    }

    #[test]
    fn flip_to_ready_rechecks_skip_rules() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Draft));
        rec.is_draft = true;
        let mut s = snap("bbbb");
        s.labels = vec!["DO-NOT-REVIEW".into()]; // case-insensitive default label skip
        let tr = poll(Event::PollDraftFlipped { snapshot: s }, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Skipped);
        assert_eq!(out.skip_reason, Some(SkipReason::Config));
    }

    #[test]
    fn draft_flip_while_snoozed_or_ignored_stays_put() {
        let mut snoozed = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        snoozed.snooze_until = Some(t("2026-09-01T12:00:00Z"));
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = poll(
            Event::PollDraftFlipped {
                snapshot: s.clone(),
            },
            &snoozed,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.skip_reason, Some(SkipReason::Snoozed));
        assert_eq!(out.snooze_until, Some(t("2026-09-01T12:00:00Z")));

        let ignored = record_in(PrState::Skipped, Some(SkipReason::Ignored));
        let tr = poll(Event::PollDraftFlipped { snapshot: s }, &ignored, T0_30M);
        assert_eq!(record_of(&tr).skip_reason, Some(SkipReason::Ignored));
    }

    #[test]
    fn flip_to_ready_from_ignored_or_snoozed_is_noop() {
        let ignored = record_in(PrState::Skipped, Some(SkipReason::Ignored));
        let tr = poll(
            Event::PollDraftFlipped {
                snapshot: snap("aaaa"),
            },
            &ignored,
            T0_30M,
        );
        assert_eq!(record_of(&tr).skip_reason, Some(SkipReason::Ignored));
    }

    #[test]
    fn draft_flip_on_terminal_is_frozen() {
        let rec = record_in(PrState::Closed, None);
        let mut s = snap("aaaa");
        s.is_draft = true;
        let tr = poll(Event::PollDraftFlipped { snapshot: s }, &rec, T0_30M);
        assert_eq!(tr.record, None);
    }

    #[test]
    fn draft_flip_without_actual_flip_resyncs() {
        let rec = tracked();
        let tr = poll(
            Event::PollDraftFlipped {
                snapshot: snap("aaaa"),
            },
            &rec,
            T0_30M,
        );
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    // ---------------------------------------------------- ReviewStart

    #[test]
    fn review_start_from_pending_enters_in_review() {
        let rec = tracked();
        let tr = poll(Event::ReviewStart, &rec, T0_61M);
        assert_eq!(state_of(&tr), PrState::InReview);
    }

    #[test]
    fn review_start_from_reviewed_enters_in_review() {
        let mut rec = record_in(PrState::ReviewedApproved, None);
        rec.last_verdict = Some(Verdict::Approved);
        let tr = poll(Event::ReviewStart, &rec, T0_61M);
        assert_eq!(state_of(&tr), PrState::InReview);
    }

    #[test]
    fn review_start_blocked_outside_pending_or_reviewed() {
        let in_review = record_in(PrState::InReview, None);
        let tr = poll(Event::ReviewStart, &in_review, T0_61M);
        assert_eq!(tr.record, None);

        let skipped = record_in(PrState::Skipped, Some(SkipReason::Ignored));
        let tr = poll(Event::ReviewStart, &skipped, T0_61M);
        assert_eq!(tr.record, None);

        let merged = record_in(PrState::Merged, None);
        let tr = poll(Event::ReviewStart, &merged, T0_61M);
        assert_eq!(tr.record, None);
    }

    // ----------------------------------------------------- ReviewDone

    #[test]
    fn review_done_approved_records_verdict_and_history() {
        let rec = record_in(PrState::InReview, None);
        let tr = poll(
            Event::ReviewDone {
                verdict: Verdict::Approved,
                blocking_count: 0,
                nit_count: 3,
                summary: Some("3 nits, no blockers".into()),
                reviewed_sha: "aaaa".into(),
            },
            &rec,
            T0_61M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedApproved);
        assert_eq!(out.last_verdict, Some(Verdict::Approved));
        assert_eq!(out.last_reviewed_sha, Some("aaaa".into()));
        assert_eq!(out.last_review_at, Some(t(T0_61M)));
        assert_eq!(out.nit_count, 3);
        assert_eq!(out.blocking_count, 0);
        assert_eq!(
            notifications(&tr),
            vec![NotifyEvent::ReviewCompleteApproved]
        );
        assert!(matches!(
            history(&tr).as_slice(),
            [HistoryEvent::ReviewCompleted {
                verdict: Verdict::Approved,
                blocking_count: 0,
                nit_count: 3
            }]
        ));
    }

    #[test]
    fn review_done_changes_requested_notifies_blocking() {
        let rec = record_in(PrState::InReview, None);
        let tr = poll(
            Event::ReviewDone {
                verdict: Verdict::ChangesRequested,
                blocking_count: 2,
                nit_count: 1,
                summary: None,
                reviewed_sha: "aaaa".into(),
            },
            &rec,
            T0_61M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedChangesRequested);
        assert_eq!(out.last_summary, None);
        assert_eq!(
            notifications(&tr),
            vec![NotifyEvent::ReviewCompleteBlocking]
        );
    }

    #[test]
    fn review_done_comment_only_shares_approved_state() {
        let rec = record_in(PrState::InReview, None);
        let tr = poll(
            Event::ReviewDone {
                verdict: Verdict::CommentOnly,
                blocking_count: 0,
                nit_count: 0,
                summary: Some("FYI notes".into()),
                reviewed_sha: "aaaa".into(),
            },
            &rec,
            T0_61M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedApproved);
        assert_eq!(out.last_verdict, Some(Verdict::CommentOnly)); // distinction kept on the record
    }

    #[test]
    fn review_done_blocking_findings_force_changes_requested() {
        let rec = record_in(PrState::InReview, None);
        let tr = poll(
            Event::ReviewDone {
                verdict: Verdict::Approved, // manifest inconsistency
                blocking_count: 1,
                nit_count: 0,
                summary: None,
                reviewed_sha: "aaaa".into(),
            },
            &rec,
            T0_61M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedChangesRequested);
        assert_eq!(out.last_verdict, Some(Verdict::ChangesRequested));
    }

    #[test]
    fn review_done_with_head_drift_collapses_to_rereview_pending() {
        let mut rec = record_in(PrState::InReview, None);
        rec.head_sha = "bbbb".into(); // commits landed during the review
        let tr = poll(
            Event::ReviewDone {
                verdict: Verdict::Approved,
                blocking_count: 0,
                nit_count: 0,
                summary: None,
                reviewed_sha: "aaaa".into(), // review analyzed the old head
            },
            &rec,
            T0_61M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert!(out.is_rereview_pending());
        assert_eq!(out.last_reviewed_sha, Some("aaaa".into()));
        assert_eq!(out.review_anchor_at, Some(t(T0_61M))); // re-review cooldown from now
        assert_eq!(out.last_verdict, Some(Verdict::Approved)); // verdict still recorded
    }

    #[test]
    fn review_done_outside_in_review_is_noop() {
        let rec = tracked();
        let tr = poll(
            Event::ReviewDone {
                verdict: Verdict::Approved,
                blocking_count: 0,
                nit_count: 0,
                summary: None,
                reviewed_sha: "aaaa".into(),
            },
            &rec,
            T0_61M,
        );
        assert_eq!(tr.record, None);
    }

    // --------------------------------------------------- ReviewFailed

    #[test]
    fn review_failed_returns_to_pending_with_fresh_anchor() {
        let mut rec = record_in(PrState::InReview, None);
        rec.review_attempts = 0;
        rec.review_session = Some("session-1".into());
        let tr = poll(Event::ReviewFailed, &rec, T0_61M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.review_anchor_at, Some(t(T0_61M))); // no hot-loop retries
        assert_eq!(out.review_attempts, 1);
        assert_eq!(out.review_session, None);
        assert_eq!(out.last_verdict, None);
        assert_eq!(notifications(&tr), vec![NotifyEvent::Failure]);
        assert_eq!(history(&tr), vec![HistoryEvent::ReviewFailed]);
    }

    #[test]
    fn review_failed_attempts_accumulate() {
        let mut rec = record_in(PrState::InReview, None);
        rec.review_attempts = 2;
        let tr = poll(Event::ReviewFailed, &rec, T0_61M);
        assert_eq!(record_of(&tr).review_attempts, 3);
    }

    #[test]
    fn review_failed_outside_in_review_is_noop() {
        let rec = tracked();
        let tr = poll(Event::ReviewFailed, &rec, T0_61M);
        assert_eq!(tr.record, None);
    }

    // -------------------------------------------------------- Snooze

    #[test]
    fn snooze_from_pending_stores_previous_state() {
        let rec = tracked();
        let until = t("2026-09-02T10:00:00Z");
        let tr = poll(
            Event::Snooze {
                until: Some(until),
                until_push: false,
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Skipped);
        assert_eq!(out.skip_reason, Some(SkipReason::Snoozed));
        assert_eq!(out.previous_state, Some(PrState::Pending)); // A6
        assert_eq!(out.snooze_until, Some(until));
        assert!(!out.snooze_until_push);
    }

    #[test]
    fn snooze_until_push_sets_flag() {
        let rec = tracked();
        let tr = poll(
            Event::Snooze {
                until: None,
                until_push: true,
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.snooze_until, None);
        assert!(out.snooze_until_push);
    }

    #[test]
    fn snooze_from_reviewed_restores_reviewed_state_on_expiry() {
        let rec = record_in(PrState::ReviewedChangesRequested, None);
        let tr = poll(
            Event::Snooze {
                until: Some(t("2026-09-02T10:00:00Z")),
                until_push: false,
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.previous_state, Some(PrState::ReviewedChangesRequested));
    }

    #[test]
    fn snooze_from_in_review_restores_pending_with_fresh_cooldown() {
        let mut rec = record_in(PrState::InReview, None);
        rec.review_anchor_at = Some(t(T0));
        let tr = poll(
            Event::Snooze {
                until: Some(t("2026-09-02T10:00:00Z")),
                until_push: false,
            },
            &rec,
            T0_30M,
        );
        let out = record_of(&tr);
        assert_eq!(out.previous_state, Some(PrState::Pending)); // never restore to in_review
        assert_eq!(out.review_anchor_at, Some(t(T0_30M)));
    }

    #[test]
    fn snooze_from_skipped_or_terminal_is_noop() {
        let skipped = record_in(PrState::Skipped, Some(SkipReason::Draft));
        let tr = poll(
            Event::Snooze {
                until: Some(t("2026-09-02T10:00:00Z")),
                until_push: false,
            },
            &skipped,
            T0_30M,
        );
        // Unchanged: the original skip reason survives untouched.
        assert_eq!(tr.record, None);

        let merged = record_in(PrState::Merged, None);
        let tr = poll(
            Event::Snooze {
                until: Some(t("2026-09-02T10:00:00Z")),
                until_push: false,
            },
            &merged,
            T0_30M,
        );
        assert_eq!(tr.record, None);
    }

    #[test]
    fn snooze_without_schedule_is_noop() {
        let rec = tracked();
        let tr = poll(
            Event::Snooze {
                until: None,
                until_push: false,
            },
            &rec,
            T0_30M,
        );
        assert_eq!(tr.record, None); // unchanged: no schedule, no snooze
    }

    // -------------------------------------------------- SnoozeExpired

    #[test]
    fn snooze_expired_restores_pending_with_its_anchor() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.previous_state = Some(PrState::Pending);
        rec.snooze_until = Some(t("2026-09-01T09:00:00Z"));
        rec.review_anchor_at = Some(t(T0));
        let tr = poll(Event::SnoozeExpired, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.review_anchor_at, Some(t(T0))); // anchor preserved from before snooze
        assert_eq!(out.skip_reason, None);
        assert_eq!(out.snooze_until, None);
        assert_eq!(out.previous_state, None);
    }

    #[test]
    fn snooze_expired_restores_reviewed_state() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.previous_state = Some(PrState::ReviewedApproved);
        rec.last_verdict = Some(Verdict::Approved);
        let tr = poll(Event::SnoozeExpired, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedApproved);
        assert_eq!(out.last_verdict, Some(Verdict::Approved));
    }

    #[test]
    fn snooze_expired_without_previous_state_defaults_to_pending() {
        let rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        let tr = poll(Event::SnoozeExpired, &rec, T0_30M);
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    #[test]
    fn snooze_expired_never_restores_invalid_previous() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.previous_state = Some(PrState::Merged); // corrupt data
        let tr = poll(Event::SnoozeExpired, &rec, T0_30M);
        assert_eq!(state_of(&tr), PrState::Pending);
    }

    #[test]
    fn snooze_expired_on_non_snoozed_is_noop() {
        for reason in [SkipReason::Draft, SkipReason::Config, SkipReason::Ignored] {
            let rec = record_in(PrState::Skipped, Some(reason));
            let tr = poll(Event::SnoozeExpired, &rec, T0_30M);
            assert_eq!(tr.record, None); // expiry only applies to snoozed records
        }
        let pending = tracked();
        let tr = poll(Event::SnoozeExpired, &pending, T0_30M);
        assert_eq!(tr.record, None);
    }

    // ------------------------------------------------------ Unsnooze

    #[test]
    fn unsnooze_restores_reviewed_state() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.previous_state = Some(PrState::ReviewedApproved);
        rec.last_verdict = Some(Verdict::Approved);
        rec.snooze_until = Some(t("2026-09-02T10:00:00Z"));
        let tr = poll(Event::Unsnooze, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedApproved); // not pending
        assert_eq!(out.last_verdict, Some(Verdict::Approved));
        assert_eq!(out.skip_reason, None); // snooze schedule cleared
        assert_eq!(out.snooze_until, None);
        assert_eq!(out.previous_state, None);
        assert!(notifications(&tr).is_empty()); // the user did it themselves
        assert!(history(&tr).is_empty());
    }

    #[test]
    fn unsnooze_restores_pending_with_its_anchor() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.previous_state = Some(PrState::Pending);
        rec.snooze_until = Some(t("2026-09-01T09:00:00Z"));
        rec.review_anchor_at = Some(t(T0));
        let tr = poll(Event::Unsnooze, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.review_anchor_at, Some(t(T0))); // anchor preserved from before snooze
        assert_eq!(out.skip_reason, None);
        assert_eq!(out.snooze_until, None);
        assert_eq!(out.previous_state, None);
    }

    #[test]
    fn unsnooze_on_non_snoozed_is_noop() {
        for reason in [SkipReason::Draft, SkipReason::Config, SkipReason::Ignored] {
            let rec = record_in(PrState::Skipped, Some(reason));
            let tr = poll(Event::Unsnooze, &rec, T0_30M);
            assert_eq!(tr.record, None); // un-snooze only applies to snoozed records
        }
        let pending = tracked();
        let tr = poll(Event::Unsnooze, &pending, T0_30M);
        assert_eq!(tr.record, None);
    }

    #[test]
    fn unsnooze_on_terminal_is_noop() {
        for state in [PrState::Merged, PrState::Closed] {
            let rec = record_in(state, None);
            let tr = poll(Event::Unsnooze, &rec, T0_30M);
            assert_eq!(tr.record, None); // terminal states are never resurrected
        }
    }

    // --------------------------------------------- Ignore / Unignore

    #[test]
    fn ignore_from_any_nonterminal_state() {
        for (state, reason) in [
            (PrState::Pending, None),
            (PrState::InReview, None),
            (PrState::ReviewedApproved, None),
        ] {
            let rec = record_in(state, reason);
            let tr = poll(Event::Ignore, &rec, T0_30M);
            let out = record_of(&tr);
            assert_eq!(out.state, PrState::Skipped);
            assert_eq!(out.skip_reason, Some(SkipReason::Ignored));
            assert_eq!(out.previous_state, None);
        }
    }

    #[test]
    fn ignore_clears_any_snooze() {
        let mut rec = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        rec.snooze_until = Some(t("2026-09-02T10:00:00Z"));
        rec.snooze_until_push = true;
        let tr = poll(Event::Ignore, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.skip_reason, Some(SkipReason::Ignored));
        assert_eq!(out.snooze_until, None);
        assert!(!out.snooze_until_push);
    }

    #[test]
    fn ignore_is_idempotent() {
        let ignored = record_in(PrState::Skipped, Some(SkipReason::Ignored));
        let tr = poll(Event::Ignore, &ignored, T0_30M);
        assert_eq!(tr.record, None);
        assert!(tr.effects.is_empty());
    }

    #[test]
    fn ignore_terminal_is_noop() {
        let rec = record_in(PrState::Merged, None);
        let tr = poll(Event::Ignore, &rec, T0_30M);
        assert_eq!(tr.record, None);
    }

    #[test]
    fn unignore_goes_pending_anchored_to_now() {
        let ignored = record_in(PrState::Skipped, Some(SkipReason::Ignored));
        let tr = poll(Event::Unignore, &ignored, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.skip_reason, None);
        assert_eq!(out.review_anchor_at, Some(t(T0_30M))); // §6: anchor = now
    }

    #[test]
    fn unignore_non_ignored_is_noop() {
        let snoozed = record_in(PrState::Skipped, Some(SkipReason::Snoozed));
        let tr = poll(Event::Unignore, &snoozed, T0_30M);
        assert_eq!(tr.record, None); // only ignored records respond to unignore

        let pending = tracked();
        let tr = poll(Event::Unignore, &pending, T0_30M);
        assert_eq!(tr.record, None);

        let closed = record_in(PrState::Closed, None);
        let tr = poll(Event::Unignore, &closed, T0_30M);
        assert_eq!(tr.record, None);
    }

    // ---------------------------------------------------------- Approve

    #[test]
    fn approve_lands_reviewed_approved_from_attention_states() {
        for state in [
            PrState::Pending,
            PrState::ReviewedApproved,
            PrState::ReviewedChangesRequested,
        ] {
            let rec = record_in(state, None);
            let tr = poll(Event::Approve, &rec, T0_30M);
            assert_eq!(state_of(&tr), PrState::ReviewedApproved);
            assert!(notifications(&tr).is_empty()); // the human IS the event
            assert!(history(&tr).is_empty());
        }
    }

    #[test]
    fn approve_clears_stale_skip_and_snooze_bookkeeping() {
        for state in [
            PrState::Pending,
            PrState::ReviewedApproved,
            PrState::ReviewedChangesRequested,
        ] {
            let mut rec = record_in(state, None);
            // Stale bookkeeping a reviewed record should never carry; the
            // transition must not let it linger.
            rec.skip_reason = Some(SkipReason::Snoozed);
            rec.snooze_until = Some(t("2026-09-02T10:00:00Z"));
            rec.snooze_until_push = true;
            rec.previous_state = Some(PrState::Pending);
            let tr = poll(Event::Approve, &rec, T0_30M);
            let out = record_of(&tr);
            assert_eq!(out.state, PrState::ReviewedApproved);
            assert_eq!(out.skip_reason, None);
            assert_eq!(out.snooze_until, None);
            assert!(!out.snooze_until_push);
            assert_eq!(out.previous_state, None);
            assert_eq!(out.updated_at, t(T0_30M));
        }
    }

    #[test]
    fn approve_keeps_the_agent_review_record() {
        let mut rec = record_in(PrState::ReviewedChangesRequested, None);
        rec.last_verdict = Some(Verdict::ChangesRequested);
        rec.last_reviewed_sha = Some("aaaa".into());
        rec.last_review_at = Some(t(T0));
        rec.blocking_count = 3;
        rec.nit_count = 4;
        let tr = poll(Event::Approve, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::ReviewedApproved);
        assert_eq!(out.last_verdict, Some(Verdict::ChangesRequested));
        assert_eq!(out.last_reviewed_sha.as_deref(), Some("aaaa"));
        assert_eq!(out.last_review_at, Some(t(T0)));
        assert_eq!(out.blocking_count, 3);
        assert_eq!(out.nit_count, 4);
    }

    #[test]
    fn approve_is_noop_from_in_review() {
        let rec = record_in(PrState::InReview, None);
        let tr = poll(Event::Approve, &rec, T0_30M);
        assert_eq!(tr.record, None); // would race the in-flight ReviewDone
    }

    #[test]
    fn approve_is_noop_from_skipped_or_terminal() {
        for (state, reason) in [
            (PrState::Skipped, Some(SkipReason::Ignored)),
            (PrState::Skipped, Some(SkipReason::Snoozed)),
            (PrState::Merged, None),
            (PrState::Closed, None),
        ] {
            let rec = record_in(state, reason);
            let tr = poll(Event::Approve, &rec, T0_30M);
            assert_eq!(tr.record, None); // skipped needs unignore; terminal is frozen
        }
    }

    #[test]
    fn approve_for_untracked_record_is_noop() {
        let skip = SkipConfig::default();
        let tr = apply(None, Event::Approve, &ctx_at(T0, &skip));
        assert_eq!(tr.record, None); // the gh write happens at the caller
    }

    // ------------------------------------- human_approved_at (task 8.1)

    fn human_approved(verdict: Verdict) -> PrRecord {
        let mut rec = record_in(PrState::ReviewedApproved, None);
        rec.last_verdict = Some(verdict);
        record_of(&poll(Event::Approve, &rec, T0_30M))
    }

    #[test]
    fn human_approve_sets_human_approved_at_and_keeps_the_agent_verdict() {
        for state in [
            PrState::Pending,
            PrState::ReviewedApproved,
            PrState::ReviewedChangesRequested,
        ] {
            let mut rec = record_in(state, None);
            rec.last_verdict = Some(Verdict::Approved);
            assert_eq!(rec.human_approved_at, None);
            let out = record_of(&poll(Event::Approve, &rec, T0_30M));
            assert_eq!(out.human_approved_at, Some(t(T0_30M)), "{state:?}");
            assert_eq!(out.last_verdict, Some(Verdict::Approved));
        }
    }

    #[test]
    fn human_block_clears_human_approved_at() {
        let rec = human_approved(Verdict::Approved);
        let out = record_of(&poll(Event::RequestChanges, &rec, T0_61M));
        assert_eq!(out.state, PrState::ReviewedChangesRequested);
        assert_eq!(out.human_approved_at, None);
    }

    #[test]
    fn next_agent_review_clears_human_approved_at() {
        for verdict in [
            Verdict::Approved,
            Verdict::CommentOnly,
            Verdict::ChangesRequested,
        ] {
            let mut rec = human_approved(Verdict::Approved);
            rec = record_of(&apply(
                Some(&rec),
                Event::ReviewStart,
                &ctx_at(T0_30M, &SkipConfig::default()),
            ));
            assert_eq!(rec.state, PrState::InReview);
            let out = record_of(&poll(
                Event::ReviewDone {
                    verdict,
                    blocking_count: u32::from(verdict == Verdict::ChangesRequested),
                    nit_count: 0,
                    summary: None,
                    reviewed_sha: rec.head_sha.clone(),
                },
                &rec,
                T0_61M,
            ));
            assert_eq!(out.human_approved_at, None, "{verdict:?}");
        }
    }

    #[test]
    fn re_entering_pending_clears_human_approved_at() {
        // New commits on a human-approved PR.
        let rec = human_approved(Verdict::Approved);
        let out = record_of(&poll(
            Event::PollShaChanged {
                snapshot: snap("ffff"),
            },
            &rec,
            T0_61M,
        ));
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.human_approved_at, None);
    }

    #[test]
    fn unchanged_polls_keep_human_approved_at() {
        let rec = human_approved(Verdict::Approved);
        let out = record_of(&poll(
            Event::PollUnchanged {
                snapshot: snap(&rec.head_sha),
            },
            &rec,
            T0_61M,
        ));
        assert_eq!(out.human_approved_at, Some(t(T0_30M)));
    }

    // ------------------------------------------------- RequestChanges (D9)

    #[test]
    fn request_changes_lands_blocking_from_attention_states() {
        for state in [
            PrState::Pending,
            PrState::ReviewedApproved,
            PrState::ReviewedChangesRequested,
        ] {
            let mut rec = record_in(state, None);
            rec.last_verdict = Some(Verdict::Approved);
            rec.previous_state = Some(PrState::Pending);
            let tr = poll(Event::RequestChanges, &rec, T0_30M);
            let out = record_of(&tr);
            assert_eq!(out.state, PrState::ReviewedChangesRequested);
            assert_eq!(out.previous_state, None);
            assert_eq!(out.updated_at, t(T0_30M));
            // The agent's review record is the agent's, not the human's.
            assert_eq!(out.last_verdict, Some(Verdict::Approved));
            assert!(notifications(&tr).is_empty());
            assert!(history(&tr).is_empty());
        }
    }

    #[test]
    fn request_changes_is_noop_from_in_review_skipped_terminal_and_untracked() {
        for (state, reason) in [
            (PrState::InReview, None),
            (PrState::Skipped, Some(SkipReason::Ignored)),
            (PrState::Merged, None),
            (PrState::Closed, None),
        ] {
            let rec = record_in(state, reason);
            let tr = poll(Event::RequestChanges, &rec, T0_30M);
            assert_eq!(tr.record, None);
        }
        let skip = SkipConfig::default();
        let tr = apply(None, Event::RequestChanges, &ctx_at(T0, &skip));
        assert_eq!(tr.record, None);
    }

    // ---------------------------------------------------------- Unblock

    #[test]
    fn unblock_returns_blocked_record_to_pending() {
        let mut rec = record_in(PrState::ReviewedChangesRequested, None);
        rec.last_verdict = Some(Verdict::ChangesRequested);
        let tr = poll(Event::Unblock, &rec, T0_30M);
        let out = record_of(&tr);
        assert_eq!(out.state, PrState::Pending);
        assert_eq!(out.review_anchor_at, Some(t(T0_30M)));
        assert_eq!(out.updated_at, t(T0_30M));
        assert_eq!(out.last_verdict, Some(Verdict::ChangesRequested));
        assert!(notifications(&tr).is_empty());
        assert!(history(&tr).is_empty());
    }

    #[test]
    fn unblock_is_noop_outside_the_blocking_state_and_untracked() {
        for (state, reason) in [
            (PrState::Pending, None),
            (PrState::ReviewedApproved, None),
            (PrState::InReview, None),
            (PrState::Skipped, Some(SkipReason::Ignored)),
            (PrState::Merged, None),
            (PrState::Closed, None),
        ] {
            let rec = record_in(state, reason);
            let tr = poll(Event::Unblock, &rec, T0_30M);
            assert_eq!(tr.record, None);
        }
        let skip = SkipConfig::default();
        let tr = apply(None, Event::Unblock, &ctx_at(T0, &skip));
        assert_eq!(tr.record, None);
    }

    // ------------------------------------------------------ cooldowns

    #[test]
    fn first_review_uses_before_first_review_cooldown() {
        let cooldown = Cooldown::default(); // 60 / 30
        let rec = tracked(); // anchor T0, no prior verdict
        assert!(!machine_is_due(&rec, &cooldown, t(T0_30M)));
        assert!(machine_is_due(&rec, &cooldown, t(T0_61M)));
    }

    #[test]
    fn rereview_uses_between_re_reviews_cooldown() {
        let cooldown = Cooldown::default();
        let mut rec = tracked();
        rec.last_verdict = Some(Verdict::Approved);
        rec.review_anchor_at = Some(t(T0));
        assert!(!machine_is_due(&rec, &cooldown, t("2026-09-01T10:29:00Z")));
        assert!(machine_is_due(&rec, &cooldown, t("2026-09-01T10:31:00Z")));
    }

    fn machine_is_due(rec: &PrRecord, cooldown: &Cooldown, now: DateTime<Utc>) -> bool {
        is_review_due(rec, cooldown, now)
    }

    #[test]
    fn exactly_at_cooldown_end_is_due() {
        let cooldown = Cooldown::default();
        let rec = tracked(); // 60 min from T0 → due at 11:00
        assert!(machine_is_due(&rec, &cooldown, t("2026-09-01T11:00:00Z")));
    }

    #[test]
    fn non_pending_records_are_never_due() {
        let cooldown = Cooldown::default();
        let rec = record_in(PrState::ReviewedApproved, None);
        assert!(!machine_is_due(&rec, &cooldown, t("2027-01-01T00:00:00Z")));
    }

    #[test]
    fn pending_without_anchor_is_due_immediately() {
        let cooldown = Cooldown::default();
        let mut rec = tracked();
        rec.review_anchor_at = None; // defensive default
        assert!(machine_is_due(&rec, &cooldown, t(T0)));
    }

    #[test]
    fn cooldown_remaining_reports_window() {
        let cooldown = Cooldown::default();
        let rec = tracked();
        let remaining = cooldown_remaining(&rec, &cooldown, t(T0_30M)).unwrap();
        assert_eq!(remaining.num_minutes(), 30);
        assert_eq!(
            format!("cooling {}m", remaining.num_minutes()),
            "cooling 30m"
        );
        assert_eq!(cooldown_remaining(&rec, &cooldown, t(T0_61M)), None);
    }

    // -------------------------------------------------- misc invariants

    #[test]
    fn updated_at_is_set_on_every_transition() {
        let rec = tracked();
        let tr = poll(
            Event::PollShaChanged {
                snapshot: snap("bbbb"),
            },
            &rec,
            T0_61M,
        );
        assert_eq!(record_of(&tr).updated_at, t(T0_61M));
    }

    #[test]
    fn key_format_is_owner_repo_number() {
        assert_eq!(tracked().key(), "org/repo#1");
    }
}
