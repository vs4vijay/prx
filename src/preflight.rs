//! Pre-flight check for a manual review: what the human should confirm
//! before prx spends engine tokens on a PR that is skipped or larger than
//! the configured diff caps.
//!
//! [`check`] is pure: callers pass the PR's record (or, for an untracked
//! PR, what the skip rules would decide), the repo's mode and caps, and the
//! diff size when `gh pr view` answered. The daemon serves it as
//! `POST /review/preflight`; the CLI's in-process path calls it directly.
//! A review sent with `confirm: true` runs past exactly these warnings (a
//! `draft` / `config` skip and the diff cap). Nothing confirms past
//! `blocked`.

use serde::{Deserialize, Serialize};

use crate::config::{EffectiveRepo, ReviewMode};
use crate::github::Gh;
use crate::state::{PrRecord, PrState, SkipReason};

/// Lines and files a PR's diff changes (`gh pr view`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSize {
    pub files_changed: u64,
    pub lines: u64,
}

/// A PR's size next to the repo's diff caps (the pre-flight's size
/// indicator).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeReport {
    pub files_changed: u64,
    pub lines: u64,
    pub max_files: u64,
    pub max_lines: u64,
}

impl SizeReport {
    /// `2,140 lines in 62 files`.
    pub fn summary(&self) -> String {
        format!(
            "{} {} in {} {}",
            format_count(self.lines),
            plural(self.lines, "line", "lines"),
            format_count(self.files_changed),
            plural(self.files_changed, "file", "files"),
        )
    }

    /// The size against the caps when it exceeds one, else `None`.
    pub fn over_limit(&self) -> Option<String> {
        size_text(
            self.files_changed,
            self.lines,
            self.max_files,
            self.max_lines,
        )
    }
}

/// What a warning is about. Unknown kinds from a newer daemon decode as
/// `Other` (show the message, confirm as usual).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    /// The PR is skipped as `draft` or `config` (or the skip rules would
    /// skip it).
    Skipped,
    /// The diff is over `diff_max_files` or `diff_max_lines`.
    TooLarge,
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightWarning {
    pub kind: WarningKind,
    /// One short line: `Skipped — draft`, `Large PR — 2,140 lines changed
    /// (limit 1,500)`.
    pub message: String,
}

/// The pre-flight answer (`POST /review/preflight`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preflight {
    /// The resolved `org/repo#N`.
    pub key: String,
    /// What the human confirms before the review runs; empty when nothing
    /// needs confirming.
    #[serde(default)]
    pub warnings: Vec<PreflightWarning>,
    /// Why no confirmation can make this review run (mode `off`, snoozed,
    /// ignored, in review, merged, closed). Clients send the review anyway
    /// and show the daemon's own answer; they never ask first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<String>,
    /// The PR's size, when `gh pr view` answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<SizeReport>,
}

impl Preflight {
    /// Ask the human first: there are warnings and nothing blocks the run.
    pub fn needs_confirmation(&self) -> bool {
        self.blocked.is_none() && !self.warnings.is_empty()
    }

    /// The warning lines, then the size when known and not already said.
    pub fn lines(&self) -> Vec<String> {
        let mut lines: Vec<String> = self.warnings.iter().map(|w| w.message.clone()).collect();
        let size_said = self
            .warnings
            .iter()
            .any(|w| w.kind == WarningKind::TooLarge);
        if let (Some(size), false) = (&self.size, size_said) {
            lines.push(format!("Size — {}", size.summary()));
        }
        lines
    }
}

/// A skip a confirmed manual review runs past: the rule-based ones. A
/// snooze or an ignore is the human's own decision and has its own exit.
pub fn confirmable_skip(reason: Option<SkipReason>) -> bool {
    matches!(reason, Some(SkipReason::Draft) | Some(SkipReason::Config))
}

/// Everything [`check`] reads.
#[derive(Debug, Clone)]
pub struct PreflightInput<'a> {
    pub key: &'a str,
    pub repo: &'a str,
    pub mode: ReviewMode,
    pub max_files: u64,
    pub max_lines: u64,
    /// The tracked record, if any.
    pub record: Option<&'a PrRecord>,
    /// For an untracked PR: what the skip rules would decide
    /// ([`crate::machine::skip_decision`]).
    pub untracked_skip: Option<(SkipReason, Option<String>)>,
    pub size: Option<DiffSize>,
}

impl<'a> PreflightInput<'a> {
    /// The mode and caps of `repo`.
    pub fn for_repo(key: &'a str, repo: &'a EffectiveRepo) -> Self {
        PreflightInput {
            key,
            repo: &repo.name,
            mode: repo.review_mode,
            max_files: repo.diff_max_files as u64,
            max_lines: repo.diff_max_lines as u64,
            record: None,
            untracked_skip: None,
            size: None,
        }
    }
}

/// The pre-flight rules. Blocked (nothing to confirm): mode `off`, a review
/// already running, merged or closed, snoozed, ignored. Warnings: a `draft`
/// or `config` skip with its detail, and a diff over either cap.
pub fn check(input: &PreflightInput) -> Preflight {
    let key = input.key.to_string();
    let size = input.size.map(|s| SizeReport {
        files_changed: s.files_changed,
        lines: s.lines,
        max_files: input.max_files,
        max_lines: input.max_lines,
    });
    let blocked = |reason: String| Preflight {
        key: key.clone(),
        warnings: Vec::new(),
        blocked: Some(reason),
        size,
    };
    if input.mode == ReviewMode::Off {
        return blocked(format!("review mode is off for {}", input.repo));
    }
    let mut warnings = Vec::new();
    match input.record {
        Some(rec) => match rec.state {
            PrState::InReview => return blocked(format!("a review of {key} is already running")),
            PrState::Merged => return blocked(format!("{key} is merged")),
            PrState::Closed => return blocked(format!("{key} is closed")),
            PrState::Skipped if rec.skip_reason == Some(SkipReason::Snoozed) => {
                return blocked(format!("{key} is snoozed; unsnooze it to review it"))
            }
            PrState::Skipped if rec.skip_reason == Some(SkipReason::Ignored) => {
                return blocked(format!("{key} is ignored; stop ignoring it to review it"))
            }
            PrState::Skipped => warnings.push(skipped_warning(
                rec.skip_text().unwrap_or_else(|| "skipped".into()),
            )),
            _ => {}
        },
        None => {
            if let Some((reason, detail)) = &input.untracked_skip {
                let text = match reason {
                    SkipReason::Draft => "draft".to_string(),
                    _ => detail.clone().unwrap_or_else(|| "skip rule".into()),
                };
                warnings.push(skipped_warning(text));
            }
        }
    }
    let over = size.and_then(|s| s.over_limit()).or_else(|| {
        // Size unknown (gh did not answer): the last capped run still says
        // something.
        input
            .record
            .filter(|rec| size.is_none() && rec.diff_too_large)
            .map(|rec| {
                rec.skip_detail
                    .clone()
                    .unwrap_or_else(|| "diff too large".into())
            })
    });
    if let Some(text) = over {
        warnings.push(PreflightWarning {
            kind: WarningKind::TooLarge,
            message: format!("Large PR \u{2014} {text}"),
        });
    }
    Preflight {
        key,
        warnings,
        blocked: None,
        size,
    }
}

fn skipped_warning(text: String) -> PreflightWarning {
    PreflightWarning {
        kind: WarningKind::Skipped,
        message: format!("Skipped \u{2014} {text}"),
    }
}

/// The PR's diff size through `gh pr view` (one read). `None` when gh
/// fails: the pre-flight then goes without a size.
pub async fn measure<G: Gh>(gh: &G, repo: &str, number: u64) -> Option<DiffSize> {
    match gh.pr_view(repo, number).await {
        Ok(detail) => Some(DiffSize {
            files_changed: detail.files_changed as u64,
            lines: detail.additions.saturating_add(detail.deletions),
        }),
        Err(e) => {
            tracing::debug!("{repo}#{number}: no size for the pre-flight ({e})");
            None
        }
    }
}

/// The size against the caps, naming only the exceeded ones:
/// `2,140 lines changed (limit 1,500)`, `62 files changed (limit 50)`, or
/// both. `None` when the diff fits.
pub fn size_text(files: u64, lines: u64, max_files: u64, max_lines: u64) -> Option<String> {
    let lines_part = format!(
        "{} {} changed (limit {})",
        format_count(lines),
        plural(lines, "line", "lines"),
        format_count(max_lines)
    );
    let files_text = |verb: &str| {
        format!(
            "{} {}{verb} (limit {})",
            format_count(files),
            plural(files, "file", "files"),
            format_count(max_files)
        )
    };
    match (lines > max_lines, files > max_files) {
        (true, true) => Some(format!("{lines_part}, {}", files_text(""))),
        (true, false) => Some(lines_part),
        (false, true) => Some(files_text(" changed")),
        (false, false) => None,
    }
}

/// `2140` → `2,140`.
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn plural<'a>(n: u64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn record(state: PrState, reason: Option<SkipReason>) -> PrRecord {
        let now = Utc::now();
        PrRecord {
            repo: "acme/api".into(),
            number: 7,
            title: "t".into(),
            author: "a".into(),
            head_branch: "b".into(),
            head_sha: "s".into(),
            is_draft: false,
            gh_created_at: now,
            state,
            previous_state: None,
            skip_reason: reason,
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
            review_session_info: None,
            last_manifest: None,
            review_attempts: 0,
            diff_too_large: false,
            skip_detail: None,
            exec_summary: None,
            brief: None,
            human_approved_at: None,
            updated_at: now,
        }
    }

    fn input<'a>(record: Option<&'a PrRecord>, size: Option<DiffSize>) -> PreflightInput<'a> {
        PreflightInput {
            key: "acme/api#7",
            repo: "acme/api",
            mode: ReviewMode::ReviewComment,
            max_files: 50,
            max_lines: 1500,
            record,
            untracked_skip: None,
            size,
        }
    }

    fn messages(p: &Preflight) -> Vec<&str> {
        p.warnings.iter().map(|w| w.message.as_str()).collect()
    }

    #[test]
    fn counts_and_size_text_are_concise() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(2140), "2,140");
        assert_eq!(format_count(1234567), "1,234,567");
        assert_eq!(
            size_text(3, 2140, 50, 1500).as_deref(),
            Some("2,140 lines changed (limit 1,500)")
        );
        assert_eq!(
            size_text(62, 10, 50, 1500).as_deref(),
            Some("62 files changed (limit 50)")
        );
        assert_eq!(
            size_text(62, 2140, 50, 1500).as_deref(),
            Some("2,140 lines changed (limit 1,500), 62 files (limit 50)")
        );
        assert_eq!(size_text(50, 1500, 50, 1500), None, "at the cap fits");
    }

    #[test]
    fn a_plain_pending_pr_needs_no_confirmation() {
        let rec = record(PrState::Pending, None);
        let size = DiffSize {
            files_changed: 1,
            lines: 1,
        };
        let p = check(&input(Some(&rec), Some(size)));
        assert!(!p.needs_confirmation());
        assert_eq!(p.blocked, None);
        assert_eq!(p.lines(), vec!["Size \u{2014} 1 line in 1 file"]);
    }

    #[test]
    fn skipped_and_large_prs_warn_with_their_detail() {
        let mut rec = record(PrState::Skipped, Some(SkipReason::Config));
        rec.skip_detail = Some("author dependabot[bot] excluded".into());
        let size = DiffSize {
            files_changed: 3,
            lines: 2140,
        };
        let p = check(&input(Some(&rec), Some(size)));
        assert!(p.needs_confirmation());
        assert_eq!(
            messages(&p),
            vec![
                "Skipped \u{2014} author dependabot[bot] excluded",
                "Large PR \u{2014} 2,140 lines changed (limit 1,500)",
            ]
        );
        assert_eq!(p.warnings[1].kind, WarningKind::TooLarge);

        let draft = record(PrState::Skipped, Some(SkipReason::Draft));
        assert_eq!(
            messages(&check(&input(Some(&draft), None))),
            vec!["Skipped \u{2014} draft"]
        );
    }

    #[test]
    fn a_capped_pr_without_a_fresh_size_still_warns() {
        let mut rec = record(PrState::Pending, None);
        rec.diff_too_large = true;
        rec.skip_detail = Some("62 files changed (limit 50)".into());
        assert_eq!(
            messages(&check(&input(Some(&rec), None))),
            vec!["Large PR \u{2014} 62 files changed (limit 50)"]
        );
        // A fresh size that fits wins over the stale flag.
        let fits = DiffSize {
            files_changed: 2,
            lines: 20,
        };
        assert!(check(&input(Some(&rec), Some(fits))).warnings.is_empty());
    }

    #[test]
    fn untracked_prs_warn_with_what_the_rules_would_decide() {
        let mut i = input(None, None);
        i.untracked_skip = Some((SkipReason::Config, Some("label wip excluded".into())));
        assert_eq!(
            messages(&check(&i)),
            vec!["Skipped \u{2014} label wip excluded"]
        );
    }

    #[test]
    fn blocked_states_never_ask() {
        let off = PreflightInput {
            mode: ReviewMode::Off,
            ..input(None, None)
        };
        assert_eq!(
            check(&off).blocked.as_deref(),
            Some("review mode is off for acme/api")
        );
        for (state, reason, text) in [
            (PrState::InReview, None, "already running"),
            (PrState::Merged, None, "is merged"),
            (PrState::Closed, None, "is closed"),
            (PrState::Skipped, Some(SkipReason::Snoozed), "unsnooze"),
            (PrState::Skipped, Some(SkipReason::Ignored), "stop ignoring"),
        ] {
            let rec = record(state, reason);
            let p = check(&input(Some(&rec), None));
            assert!(p.blocked.as_deref().unwrap().contains(text), "{state:?}");
            assert!(!p.needs_confirmation());
        }
        assert!(confirmable_skip(Some(SkipReason::Draft)));
        assert!(confirmable_skip(Some(SkipReason::Config)));
        assert!(!confirmable_skip(Some(SkipReason::Snoozed)));
        assert!(!confirmable_skip(None));
    }

    #[test]
    fn unknown_warning_kinds_decode() {
        let p: Preflight =
            serde_json::from_str(r#"{"key":"a/b#1","warnings":[{"kind":"quota","message":"m"}]}"#)
                .unwrap();
        assert_eq!(p.warnings[0].kind, WarningKind::Other);
        assert!(p.needs_confirmation());
    }
}
