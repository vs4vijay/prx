//! `prx status`: poll watched repos via `gh`, sync state.json through the
//! pure transition table, and render the lifecycle table (spec §3.3).

use std::collections::HashMap;

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use unicode_width::UnicodeWidthStr;

use crate::cli::StatusArgs;
use crate::config::{Config, Cooldown, EffectiveRepo, ReviewMode};
use crate::effects::{execute_poll_effects, EffectsDeps};
use crate::github::{Gh, GhCli, GhError, PrLifecycle, PrSummary};
use crate::machine::{self, Event, MachineCtx, PrSnapshot};
use crate::notify::Notifier;
use crate::paths;
use crate::state::{
    Brief, BriefSource, PrRecord, PrState, SkipReason, StateFile, StateStore, Verdict,
};

/// One renderable row of the status table / `--json` entry. Serialized
/// identically by `prx status --json` and the daemon's `GET /status` (and
/// deserialized by the CLI thin client), so the two paths cannot drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusEntry {
    pub key: String,
    pub icon: String,
    pub state: PrState,
    pub skip_reason: Option<SkipReason>,
    pub title: String,
    pub author: String,
    pub head_branch: String,
    pub is_draft: bool,
    pub last_verdict: Option<Verdict>,
    pub blocking_count: u32,
    pub nit_count: u32,
    pub last_review_at: Option<DateTime<Utc>>,
    /// Verdict summary, snooze note, or "cooling down: review in ..." (A5).
    pub summary: String,
    pub last_review_age: String,
    /// When the PR was opened on GitHub (`PrRecord.gh_created_at`, design
    /// D8: the menu renders the PR's age from it). Absent from older
    /// daemons' payloads.
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// The login prx reviews this PR's repo as (effective `identity.login`,
    /// or the gh account a Block/Unblock resolved; design D12). `None` when
    /// not yet known — the menu then offers Block, not Unblock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_login: Option<String>,
    /// The PR's brief with its derived `stale` flag (design B3); absent when
    /// none exists or briefs are off for the repo. Absent from older
    /// daemons' payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<BriefStatus>,
    /// Whether a brief generation for this PR is running (design B4).
    #[serde(default)]
    pub brief_pending: bool,
    /// Why the last brief generation for this PR failed (the previous
    /// brief, if any, is kept); cleared when the next one starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_error: Option<String>,
    /// Whether a brief can be requested for this PR (`POST /brief` would
    /// not answer 422): the repo is not `off` and has briefs enabled. The
    /// menu and the window offer "Write brief" only then. Older daemons'
    /// payloads lack it and read as available.
    #[serde(default = "default_true")]
    pub brief_available: bool,
    /// The PR on GitHub, `https://github.com/{repo}/pull/{n}` (design D10:
    /// the one URL source for clients and auto-open). Empty from older
    /// daemons' payloads.
    #[serde(default)]
    pub html_url: String,
    /// When a human Approve last landed this PR in `reviewed_approved`
    /// (`PrRecord.human_approved_at`, task 8.1); absent when the human has
    /// not approved since the last agent review. Absent from older daemons'
    /// payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_approved_at: Option<DateTime<Utc>>,
    /// The repo's effective review engine name (design D6: the menu labels
    /// it "(default)"). Absent from older daemons' payloads.
    #[serde(default)]
    pub engine: Option<String>,
}

fn default_true() -> bool {
    true
}

/// A PR's brief as `/status` reports it (design B3): the stored record plus
/// `stale`, derived from the PR's current head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BriefStatus {
    pub text: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub head_sha: String,
    pub source: BriefSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// The brief describes an earlier commit than the PR's head.
    pub stale: bool,
}

impl BriefStatus {
    pub fn from_record(rec: &PrRecord) -> Option<BriefStatus> {
        let brief: &Brief = rec.brief.as_ref()?;
        Some(BriefStatus {
            text: brief.text.clone(),
            created_at: brief.created_at,
            updated_at: brief.updated_at,
            head_sha: brief.head_sha.clone(),
            source: brief.source,
            engine: brief.engine.clone(),
            stale: brief.is_stale(&rec.head_sha),
        })
    }
}

impl StatusEntry {
    /// Whether the menu should offer Unblock instead of Block (design D9):
    /// prx's identity is known and the PR sits in the blocking state — the
    /// state a Block (or a posted request-changes review) leaves behind.
    /// When the identity is unknown Block shows, and an Unblock that finds
    /// nothing reports "nothing to dismiss".
    pub fn offers_unblock(&self) -> bool {
        self.identity_login.is_some() && self.state == PrState::ReviewedChangesRequested
    }
}

pub async fn run_status(args: StatusArgs) -> anyhow::Result<()> {
    let config_path = paths::config_path();
    let config = Config::load(&config_path).context("cannot show status")?;
    config.validate().context("configuration is invalid")?;

    let effective = config
        .effective_repos()
        .context("configuration is invalid")?;
    let repos: Vec<EffectiveRepo> = match args.repo.as_deref() {
        None => effective,
        Some(name) => {
            if !config.has_repo(name) {
                bail!("repo '{name}' is not configured; add it with `prx repos add {name}`");
            }
            effective.into_iter().filter(|r| r.name == name).collect()
        }
    };
    if repos.is_empty() {
        bail!("no repositories configured; add one with `prx repos add <org/repo>`");
    }

    let now = Utc::now();
    let store = StateStore::new(paths::state_file());
    let mut state = store.load().context("failed to load PR state")?;

    let gh = GhCli::new();
    let notifier = crate::notify::CommandNotifier;
    let mut poll_errors: Vec<String> = Vec::new();
    for repo in &repos {
        // Per-repo effective notifications (org/repo overrides, D13).
        let deps = EffectsDeps::for_repo(&notifier, &config.notification_tiers, repo);
        if let Err(e) = sync_repo(&gh, repo, &mut state, now, &deps).await {
            tracing::warn!("failed to poll {}: {e}", repo.name);
            poll_errors.push(repo.name.clone());
        }
    }
    state.last_poll = Some(now);
    store.save(&state).context("failed to save PR state")?;

    let entries = render_entries(&state, &repos, now);
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&entries).context("failed to serialize status JSON")?
        );
    } else {
        // D5: pause and mode `off` are separate lines (different gates).
        for line in crate::pause::status_lines(state.paused_until, config.defaults.review_mode, now)
        {
            println!("{line}");
        }
        print_table(&entries, state.last_poll);
    }
    if !poll_errors.is_empty() {
        bail!("failed to poll: {}", poll_errors.join(", "));
    }
    Ok(())
}

/// Sync one repo against the state file through the transition table,
/// executing the machine's effect intents (notifications + history appends)
/// through the shared effects executor.
pub async fn sync_repo<G: Gh, N: Notifier>(
    gh: &G,
    eff: &EffectiveRepo,
    state: &mut StateFile,
    now: DateTime<Utc>,
    deps: &EffectsDeps<'_, N>,
) -> Result<(), GhError> {
    let repo = eff.name.as_str();
    let summaries = gh.list_open_prs(repo).await?;
    let ctx = MachineCtx {
        now,
        skip: &eff.skip,
        resume_sessions: eff.resolved_engine.can_resume(),
    };
    let open: HashMap<u64, &PrSummary> = summaries.iter().map(|s| (s.number, s)).collect();

    let repo_keys: Vec<String> = state
        .prs
        .keys()
        .filter(|k| k.starts_with(&format!("{repo}#")))
        .cloned()
        .collect();

    // 1. Expire timed snoozes (A6): restore previous_state before polling.
    for key in &repo_keys {
        let expired = state.prs.get(key).is_some_and(|rec| {
            rec.state == PrState::Skipped
                && rec.skip_reason == Some(SkipReason::Snoozed)
                && rec.snooze_until.is_some_and(|u| u <= now)
        });
        if expired {
            apply_and_store(state, key, Event::SnoozeExpired, &ctx, deps).await;
        }
    }

    // 2. Tracked PRs missing from the open list: ask gh whether they merged
    //    or closed. Failures are logged and ignored — a gh error must never
    //    mark a PR terminal.
    for key in &repo_keys {
        let (number, terminal) = match state.prs.get(key) {
            Some(rec) => (
                rec.number,
                matches!(rec.state, PrState::Merged | PrState::Closed),
            ),
            None => continue,
        };
        if terminal || open.contains_key(&number) {
            continue;
        }
        match gh.pr_lifecycle(repo, number).await {
            Ok(PrLifecycle::Merged) => {
                apply_and_store(state, key, Event::PollMerged, &ctx, deps).await
            }
            Ok(PrLifecycle::Closed) => {
                apply_and_store(state, key, Event::PollClosed, &ctx, deps).await
            }
            Ok(PrLifecycle::Open) => {
                // Not in the (capped) list but still open: leave untouched.
                tracing::debug!("{key} not in open list but reports OPEN; leaving as-is");
            }
            Err(e) => tracing::warn!("could not determine lifecycle of {key}: {e}"),
        }
    }

    // 3. Every PR in the open list: discover / change-detect / no-op.
    for summary in &summaries {
        let key = PrRecord::key_for(repo, summary.number);
        let snapshot = PrSnapshot::from_summary(repo, summary);
        let event = match state.prs.get(&key) {
            None => Event::PollNewPr { snapshot },
            Some(rec) => {
                if snapshot.is_draft != rec.is_draft {
                    Event::PollDraftFlipped { snapshot }
                } else if snapshot.head_sha != rec.head_sha {
                    Event::PollShaChanged { snapshot }
                } else {
                    Event::PollUnchanged { snapshot }
                }
            }
        };
        apply_and_store(state, &key, event, &ctx, deps).await;
    }
    Ok(())
}

/// Apply an event to one tracked record and persist the resulting record
/// within `state`, then execute the machine's side-effect intents
/// (notifications + history appends) through the shared effects executor.
/// Shared by poll sync, the review pipeline's callers, and the daemon's
/// manual mutations (snooze/ignore/unignore). Returns nothing: callers
/// diff the record before/after when they need to know whether it changed.
pub async fn apply_and_store<N: Notifier>(
    state: &mut StateFile,
    key: &str,
    event: Event,
    ctx: &MachineCtx<'_>,
    deps: &EffectsDeps<'_, N>,
) {
    let current = state.prs.get(key).cloned();
    let transition = machine::apply(current.as_ref(), event, ctx);
    if let Some(record) = transition.record {
        execute_poll_effects(deps, &record, &transition.effects).await;
        state.prs.insert(key.to_string(), record);
    }
    for effect in &transition.effects {
        tracing::debug!("{key}: effect {effect:?}");
    }
}

/// Render the tracked-PR table rows for `repos` (shared with the daemon's
/// `GET /status` payload so `prx status --json` and IPC output cannot drift).
pub fn render_entries(
    state: &StateFile,
    repos: &[EffectiveRepo],
    now: DateTime<Utc>,
) -> Vec<StatusEntry> {
    let repo_by_name: HashMap<&str, &EffectiveRepo> =
        repos.iter().map(|r| (r.name.as_str(), r)).collect();
    let mut records: Vec<&PrRecord> = state
        .prs
        .values()
        .filter(|rec| repo_by_name.contains_key(rec.repo.as_str()))
        .collect();
    records.sort_by(|a, b| (&a.repo, a.number).cmp(&(&b.repo, b.number)));
    records
        .iter()
        .map(|rec| {
            let fallback = Cooldown::default();
            let repo = repo_by_name.get(rec.repo.as_str()).copied();
            let cooldown = repo.map(|r| &r.cooldown).unwrap_or(&fallback);
            // Design B2: briefs off for the repo means none is shown.
            let brief = repo
                .is_none_or(|r| r.brief.enabled)
                .then(|| BriefStatus::from_record(rec))
                .flatten();
            StatusEntry {
                key: rec.key(),
                icon: icon_for(rec, cooldown, now).to_string(),
                state: rec.state,
                skip_reason: rec.skip_reason,
                title: rec.title.clone(),
                author: rec.author.clone(),
                head_branch: rec.head_branch.clone(),
                is_draft: rec.is_draft,
                last_verdict: rec.last_verdict,
                blocking_count: rec.blocking_count,
                nit_count: rec.nit_count,
                last_review_at: rec.last_review_at,
                summary: summary_line(rec, cooldown, now),
                last_review_age: age_line(rec.last_review_at, now),
                created_at: Some(rec.gh_created_at),
                identity_login: repo.and_then(|r| r.identity.login.clone()),
                brief,
                brief_pending: false,
                brief_error: None,
                brief_available: repo
                    .is_none_or(|r| r.review_mode != ReviewMode::Off && r.brief.enabled),
                html_url: pr_html_url(&rec.repo, rec.number),
                human_approved_at: rec.human_approved_at,
                engine: repo.map(|r| r.engine.clone()),
            }
        })
        .collect()
}

/// The PR's GitHub page: `https://github.com/{repo}/pull/{n}` (design D10).
pub fn pr_html_url(repo: &str, number: u64) -> String {
    format!("https://github.com/{repo}/pull/{number}")
}

/// Icons per spec §2.1. `pending` derives New / Cooling Down / Re-review
/// Pending from the anchor and prior verdict (A5).
/// Comment-only glyph (💬, design D11): a `reviewed_approved` record whose
/// latest agent verdict was `comment_only`.
pub const COMMENT_ONLY_ICON: &str = "\u{1f4ac}";

fn icon_for(rec: &PrRecord, cooldown: &Cooldown, now: DateTime<Utc>) -> &'static str {
    match rec.state {
        PrState::Pending => {
            if rec.last_verdict.is_some() {
                "\u{1f501}" // 🔁 re-review pending
            } else if machine::cooldown_remaining(rec, cooldown, now).is_some() {
                "\u{23f3}" // ⏳ cooling down
            } else {
                "\u{1f195}" // 🆕 new (ready for review)
            }
        }
        PrState::InReview => "\u{1f504}", // 🔄
        // D11: comment-only verdicts share `reviewed_approved` but must not
        // look approved.
        PrState::ReviewedApproved if rec.last_verdict == Some(Verdict::CommentOnly) => {
            COMMENT_ONLY_ICON
        }
        PrState::ReviewedApproved => "\u{2705}", // ✅
        PrState::ReviewedChangesRequested => "\u{26a0}\u{fe0f}", // ⚠️
        PrState::Skipped => "\u{1f6ab}",         // 🚫
        PrState::Merged => "\u{2714}\u{fe0f}",   // ✔️
        PrState::Closed => "\u{1f5d1}\u{fe0f}",  // 🗑️
    }
}

fn summary_line(rec: &PrRecord, cooldown: &Cooldown, now: DateTime<Utc>) -> String {
    match rec.state {
        PrState::Pending => {
            // A14: the cap is the overriding fact — nothing will review this
            // PR until its diff shrinks, so lead with it (the table column is
            // narrow and would truncate a trailing note).
            if rec.diff_too_large {
                return "diff too large \u{2014} review skipped".to_string();
            }
            match machine::cooldown_remaining(rec, cooldown, now) {
                Some(remaining) => {
                    format!("cooling down: review in {}", format_duration(remaining))
                }
                None => "ready for review".to_string(),
            }
        }
        PrState::InReview => "review in progress".to_string(),
        PrState::ReviewedApproved | PrState::ReviewedChangesRequested => {
            let verdict = match rec.last_verdict {
                Some(Verdict::Approved) => "approved",
                Some(Verdict::ChangesRequested) => "changes requested",
                Some(Verdict::CommentOnly) => "commented",
                None => "reviewed",
            };
            let base = rec.last_summary.clone().unwrap_or_else(|| {
                format!(
                    "{verdict}: {} blocking, {} nits",
                    rec.blocking_count, rec.nit_count
                )
            });
            if rec.diff_too_large {
                format!("{base} (a later diff was too large to review)")
            } else {
                base
            }
        }
        PrState::Skipped => match rec.skip_reason {
            Some(SkipReason::Draft) => "skipped: draft".to_string(),
            Some(SkipReason::Config) => "skipped: config".to_string(),
            Some(SkipReason::Snoozed) => match (rec.snooze_until, rec.snooze_until_push) {
                (Some(until), _) => format!("snoozed until {}", until.format("%Y-%m-%d %H:%M UTC")),
                (None, true) => "snoozed until next push".to_string(),
                (None, false) => "snoozed".to_string(),
            },
            Some(SkipReason::Ignored) => "ignored".to_string(),
            None => "skipped".to_string(),
        },
        PrState::Merged => "merged".to_string(),
        PrState::Closed => "closed".to_string(),
    }
}

fn age_line(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    match at {
        None => "never".to_string(),
        Some(at) => {
            let age = now - at;
            if age < chrono::Duration::zero() {
                "just now".to_string()
            } else {
                format!("{} ago", format_duration(age))
            }
        }
    }
}

fn format_duration(d: chrono::Duration) -> String {
    let minutes = d.num_minutes().max(0);
    let hours = minutes / 60;
    let mins = minutes % 60;
    if hours > 0 {
        format!("{hours}h {mins}m")
    } else {
        format!("{mins}m")
    }
}

/// Print the status table (shared with the daemon-backed thin client).
pub fn print_table(entries: &[StatusEntry], last_poll: Option<DateTime<Utc>>) {
    print!("{}", render_table(entries, last_poll));
}

/// Column widths of the status table, in terminal cells.
const ICON_WIDTH: usize = 3;
const KEY_WIDTH: usize = 16;
const TITLE_WIDTH: usize = 40;
const AUTHOR_WIDTH: usize = 14;
const SUMMARY_WIDTH: usize = 36;

/// The status table as text. Columns are padded by terminal display width,
/// not by `char` count: the state icons are emoji that a terminal draws two
/// cells wide, some of them one code point (✅) and some two (⚠️ is U+26A0
/// plus the U+FE0F variation selector), so `{:<3}` gave rows different
/// widths and put the header one cell left of the single-code-point rows.
pub fn render_table(entries: &[StatusEntry], last_poll: Option<DateTime<Utc>>) -> String {
    if entries.is_empty() {
        return "No PRs tracked yet.\n".to_string();
    }
    let mut out = String::new();
    let row = |icon: &str, key: &str, title: &str, author: &str, summary: &str, last: &str| {
        format!(
            "{} {} {} {} {} {}\n",
            pad_display(icon, ICON_WIDTH),
            pad_display(key, KEY_WIDTH),
            pad_display(title, TITLE_WIDTH),
            pad_display(author, AUTHOR_WIDTH),
            pad_display(summary, SUMMARY_WIDTH),
            last,
        )
    };
    out.push_str(&row("", "PR", "TITLE", "AUTHOR", "VERDICT", "LAST REVIEW"));
    for e in entries {
        out.push_str(&row(
            &e.icon,
            &e.key,
            &truncate(&e.title, TITLE_WIDTH - 2),
            &truncate(&e.author, AUTHOR_WIDTH - 2),
            &truncate(&e.summary, SUMMARY_WIDTH - 2),
            &e.last_review_age,
        ));
    }
    match last_poll {
        Some(at) => out.push_str(&format!(
            "\nlast poll: {}\n",
            at.format("%Y-%m-%d %H:%M:%S UTC")
        )),
        None => out.push_str("\nlast poll: never\n"),
    }
    out
}

/// `s` followed by spaces up to `width` terminal cells (never truncates).
fn pad_display(s: &str, width: usize) -> String {
    let pad = width.saturating_sub(UnicodeWidthStr::width(s));
    format!("{s}{}", " ".repeat(pad))
}

/// `s` cut to at most `max` terminal cells, plus an ellipsis when cut.
fn truncate(s: &str, max: usize) -> String {
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    let mut cut = String::new();
    for c in s.chars() {
        cut.push(c);
        if UnicodeWidthStr::width(cut.as_str()) > max {
            cut.pop();
            break;
        }
    }
    format!("{cut}\u{2026}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(repo: &str, number: u64, created: &str, state: PrState) -> PrRecord {
        let created: DateTime<Utc> = created.parse().unwrap();
        PrRecord {
            repo: repo.to_string(),
            number,
            title: "t".into(),
            author: "alice".into(),
            head_branch: "b".into(),
            head_sha: "sha".into(),
            is_draft: false,
            gh_created_at: created,
            state,
            previous_state: None,
            skip_reason: None,
            snooze_until: None,
            snooze_until_push: false,
            review_anchor_at: Some(created),
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
            updated_at: created,
        }
    }

    fn repos(yaml: &str) -> Vec<EffectiveRepo> {
        Config::parse(yaml, std::path::Path::new("t.yml"))
            .unwrap()
            .effective_repos()
            .unwrap()
    }

    /// D8/5.1: entries carry the PR's GitHub creation time from the state
    /// record; D12/4.4: and the effective identity login of its repo.
    #[test]
    fn entries_carry_created_at_and_identity_login() {
        let repos =
            repos("repos:\n  - name: o/a\n    identity:\n      login: prx-bot\n  - name: o/b\n");
        let mut state = StateFile::default();
        for rec in [
            record(
                "o/a",
                1,
                "2026-09-01T10:00:00Z",
                PrState::ReviewedChangesRequested,
            ),
            record(
                "o/b",
                2,
                "2026-09-03T12:30:00Z",
                PrState::ReviewedChangesRequested,
            ),
        ] {
            state.prs.insert(rec.key(), rec);
        }
        let now: DateTime<Utc> = "2026-09-05T00:00:00Z".parse().unwrap();
        let entries = render_entries(&state, &repos, now);
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].created_at,
            Some("2026-09-01T10:00:00Z".parse().unwrap())
        );
        assert_eq!(
            entries[1].created_at,
            Some("2026-09-03T12:30:00Z".parse().unwrap())
        );
        assert_eq!(entries[0].identity_login.as_deref(), Some("prx-bot"));
        assert_eq!(entries[1].identity_login, None);
        // Block vs Unblock: known identity + blocking state offers Unblock;
        // an unknown identity falls back to Block.
        assert!(entries[0].offers_unblock());
        assert!(!entries[1].offers_unblock());

        let json = serde_json::to_value(&entries[0]).unwrap();
        assert_eq!(json["created_at"], "2026-09-01T10:00:00Z");
        assert_eq!(json["identity_login"], "prx-bot");
        // Pre-existing fields are unchanged alongside the new ones.
        assert_eq!(json["key"], "o/a#1");
        assert_eq!(json["author"], "alice");
        assert_eq!(json["state"], "reviewed_changes_requested");
    }

    #[test]
    fn unblock_is_offered_only_in_the_blocking_state() {
        let repos = repos("repos:\n  - name: o/a\n    identity:\n      login: prx-bot\n");
        let now: DateTime<Utc> = "2026-09-05T00:00:00Z".parse().unwrap();
        for (state, unblock) in [
            (PrState::Pending, false),
            (PrState::ReviewedApproved, false),
            (PrState::ReviewedChangesRequested, true),
        ] {
            let mut file = StateFile::default();
            let rec = record("o/a", 1, "2026-09-01T10:00:00Z", state);
            file.prs.insert(rec.key(), rec);
            let entries = render_entries(&file, &repos, now);
            assert_eq!(entries[0].offers_unblock(), unblock, "{state:?}");
        }
    }

    /// D11 / task 8.1: comment-only gets its own glyph (CLI and menu bar
    /// share it), approved keeps ✅, and a human approval is surfaced.
    #[test]
    fn comment_only_glyph_and_human_approval_reach_the_entry() {
        let repos = repos("repos:\n  - name: o/a\n");
        let now: DateTime<Utc> = "2026-09-05T00:00:00Z".parse().unwrap();
        let approved_at: DateTime<Utc> = "2026-09-04T00:00:00Z".parse().unwrap();
        for (verdict, human, icon) in [
            (Verdict::Approved, None, "\u{2705}"),
            (Verdict::Approved, Some(approved_at), "\u{2705}"),
            (Verdict::CommentOnly, None, COMMENT_ONLY_ICON),
        ] {
            let mut file = StateFile::default();
            let mut rec = record("o/a", 1, "2026-09-01T10:00:00Z", PrState::ReviewedApproved);
            rec.last_verdict = Some(verdict);
            rec.human_approved_at = human;
            file.prs.insert(rec.key(), rec);
            let entry = &render_entries(&file, &repos, now)[0];
            assert_eq!(entry.icon, icon, "{verdict:?}");
            assert_eq!(entry.human_approved_at, human);
            let json = serde_json::to_value(entry).unwrap();
            assert_eq!(json.get("human_approved_at").is_some(), human.is_some());
        }
    }

    /// Task 4.1: `brief_available` follows the repo's mode and brief switch
    /// (what `POST /brief` would reject with 422), and the deprecated
    /// `exec_summary` copy is gone from the payload.
    #[test]
    fn brief_available_follows_mode_and_brief_switch() {
        let repos = repos(
            "repos:\n  - name: o/a\n  - name: o/b\n    review_mode: off\n  \
             - name: o/c\n    brief:\n      enabled: false\n",
        );
        let now: DateTime<Utc> = "2026-09-05T00:00:00Z".parse().unwrap();
        let mut file = StateFile::default();
        for repo in ["o/a", "o/b", "o/c"] {
            let rec = record(repo, 1, "2026-09-01T10:00:00Z", PrState::Pending);
            file.prs.insert(rec.key(), rec);
        }
        let entries = render_entries(&file, &repos, now);
        let available: Vec<bool> = entries.iter().map(|e| e.brief_available).collect();
        assert_eq!(available, [true, false, false]);
        let json = serde_json::to_value(&entries[0]).unwrap();
        assert_eq!(json["brief_available"], true);
        assert!(json.get("exec_summary").is_none());
    }

    /// Payloads from daemons predating the new fields still deserialize.
    #[test]
    fn older_payloads_without_new_fields_deserialize() {
        let old = serde_json::json!({
            "key": "o/a#1", "icon": "x", "state": "pending", "skip_reason": null,
            "title": "t", "author": "alice", "head_branch": "b", "is_draft": false,
            "last_verdict": null, "blocking_count": 0, "nit_count": 0,
            "last_review_at": null, "summary": "", "last_review_age": "-"
        });
        let entry: StatusEntry = serde_json::from_value(old).unwrap();
        assert_eq!(entry.created_at, None);
        assert_eq!(entry.identity_login, None);
        assert_eq!(entry.human_approved_at, None);
        assert!(!entry.offers_unblock());
        assert!(entry.brief_available, "older daemons read as available");
    }

    /// The cell at which each column starts, per line of the rendered table,
    /// measured in terminal display width (emoji = two cells), not chars.
    fn column_starts(line: &str, markers: &[&str]) -> Vec<usize> {
        markers
            .iter()
            .map(|m| {
                let at = line
                    .find(m)
                    .unwrap_or_else(|| panic!("{m:?} not in {line:?}"));
                UnicodeWidthStr::width(&line[..at])
            })
            .collect()
    }

    fn entry_with(icon: &str, title: &str, author: &str) -> StatusEntry {
        let mut entry: StatusEntry = serde_json::from_value(serde_json::json!({
            "key": "acme/api#1", "icon": icon, "state": "pending", "skip_reason": null,
            "title": title, "author": author, "head_branch": "b", "is_draft": false,
            "last_verdict": null, "blocking_count": 0, "nit_count": 0,
            "last_review_at": null, "summary": "SUMMARY", "last_review_age": "AGE"
        }))
        .unwrap();
        entry.key = "acme/api#1".into();
        entry
    }

    /// Every icon `icon_for` can produce, as (icon, is it two code points).
    fn all_icons() -> Vec<&'static str> {
        let now: DateTime<Utc> = "2026-09-02T10:00:00Z".parse().unwrap();
        let cooldown = Cooldown::default();
        let mut icons = Vec::new();
        for state in [
            PrState::Pending,
            PrState::InReview,
            PrState::ReviewedApproved,
            PrState::ReviewedChangesRequested,
            PrState::Skipped,
            PrState::Merged,
            PrState::Closed,
        ] {
            let rec = record("o/a", 1, "2026-09-01T10:00:00Z", state);
            icons.push(icon_for(&rec, &cooldown, now));
        }
        let mut again = record("o/a", 1, "2026-09-01T10:00:00Z", PrState::Pending);
        again.last_verdict = Some(Verdict::Approved);
        icons.push(icon_for(&again, &cooldown, now));
        let fresh = record("o/a", 1, "2026-09-02T09:59:00Z", PrState::Pending);
        icons.push(icon_for(&fresh, &cooldown, now));
        icons.push(COMMENT_ONLY_ICON);
        icons
    }

    /// Regression: the icon column was padded with `{:<3}`, which counts
    /// chars. Emoji with a variation selector (two chars, two cells) then
    /// sat one cell left of single-char emoji (one char, two cells), and the
    /// header one cell left of the latter.
    #[test]
    fn table_columns_align_by_display_width_for_every_icon() {
        let icons = all_icons();
        assert!(
            icons.iter().any(|i| i.chars().count() == 2)
                && icons.iter().any(|i| i.chars().count() == 1),
            "the icon set mixes one- and two-code-point emoji: {icons:?}"
        );
        for icon in &icons {
            assert_eq!(UnicodeWidthStr::width(*icon), 2, "{icon:?} is two cells");
        }
        let entries: Vec<StatusEntry> = icons
            .iter()
            .map(|i| entry_with(i, "TITLE", "AUTHOR"))
            .collect();
        let table = render_table(&entries, None);
        let mut lines = table.lines();
        let header = lines.next().unwrap();
        let expected = column_starts(header, &["PR", "TITLE", "AUTHOR", "VERDICT", "LAST REVIEW"]);
        assert_eq!(expected, [4, 21, 62, 77, 114]);
        for (line, icon) in lines.by_ref().take(entries.len()).zip(&icons) {
            assert_eq!(
                column_starts(line, &["acme/api#1", "TITLE", "AUTHOR", "SUMMARY", "AGE"]),
                expected,
                "row with icon {icon:?}: {line:?}"
            );
        }
    }

    /// Wide text in a cell (CJK, emoji) is measured and cut by cells too, so
    /// it cannot push the columns after it out of line.
    #[test]
    fn table_columns_align_with_wide_text_in_cells() {
        let wide_title = "\u{6f22}\u{5b57}".repeat(30); // 60 chars, 120 cells
        let entries = [
            entry_with("\u{2705}", "plain ascii title", "alice"),
            entry_with(
                "\u{26a0}\u{fe0f}",
                &wide_title,
                "\u{1f916}\u{1f916}\u{1f916}\u{1f916}\u{1f916}\u{1f916}\u{1f916}bot",
            ),
            entry_with("\u{1f5d1}\u{fe0f}", "caf\u{e9} \u{26a0}\u{fe0f} fix", "bob"),
        ];
        let table = render_table(&entries, None);
        let starts: Vec<Vec<usize>> = table
            .lines()
            .skip(1)
            .take(3)
            .map(|l| column_starts(l, &["acme/api#1", "SUMMARY", "AGE"]))
            .collect();
        assert_eq!(starts, vec![vec![4, 77, 114]; 3], "{table}");
    }

    #[test]
    fn truncate_and_pad_count_cells_not_chars() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "abc\u{2026}");
        // Four two-cell chars into five cells: two fit, the third would not.
        assert_eq!(
            truncate("\u{6f22}\u{6f22}\u{6f22}\u{6f22}", 5),
            "\u{6f22}\u{6f22}\u{2026}"
        );
        assert_eq!(pad_display("\u{26a0}\u{fe0f}", 3), "\u{26a0}\u{fe0f} ");
        assert_eq!(pad_display("\u{2705}", 3), "\u{2705} ");
        assert_eq!(pad_display("", 3), "   ");
        assert_eq!(pad_display("toolong", 3), "toolong");
    }

    #[test]
    fn empty_table_and_footer() {
        assert_eq!(render_table(&[], None), "No PRs tracked yet.\n");
        let table = render_table(&[entry_with("\u{2705}", "t", "a")], None);
        assert!(table.ends_with("\nlast poll: never\n"), "{table:?}");
    }
}
