//! Insights (spec §3.7, DEV-PLAN §10, A18): pure analytics over history
//! entries. No I/O — callers hand in the entries (from [`crate::history::query`])
//! and get a Serialize struct (the `GET /insights` payload, consumed by the
//! CLI thin client and the future menu bar shell) plus a human-readable
//! renderer for `prx insights`.
//!
//! Determinism: every aggregate is ordered (BTreeMap or explicit
//! count-then-text ordering), so the same history slice always renders and
//! serializes identically.

use std::collections::BTreeMap;

use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::history::HistoryEntry;
use crate::state::Verdict;

/// Cap on the "most common blocking issues" list.
const TOP_ISSUES: usize = 10;
/// Cap on the per-repo "top blocking themes" list.
const TOP_REPO_ISSUES: usize = 5;

/// The full insights report. This is the exact `GET /insights` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Insights {
    pub generated_at: DateTime<Utc>,
    /// Analysis window in days; `None` = all time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_days: Option<u64>,
    pub reviews: ReviewsOverview,
    pub review_to_merge: MergeLatency,
    pub top_blocking_issues: Vec<CountedText>,
    pub per_author: Vec<AuthorPattern>,
    pub per_repo: Vec<RepoPattern>,
    pub effectiveness: Effectiveness,
}

/// Reviews performed (spec §3.7 "daily / weekly / monthly" as the daily
/// series plus the 7-day / 30-day / all-time rollups).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewsOverview {
    /// Completed agent reviews in scope (failures and skips are separate).
    pub total: u32,
    pub failed: u32,
    pub skipped: u32,
    pub last_7_days: u32,
    pub last_30_days: u32,
    /// Completed reviews per day (only days with activity), ascending.
    pub per_day: Vec<DailyCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyCount {
    pub date: NaiveDate,
    pub reviews: u32,
}

/// Average time from the last review of a PR to its merge (only PRs that
/// actually merged after a review count as samples).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeLatency {
    pub samples: u32,
    /// Mean of (pr_merged ts − latest review_completed ts), in seconds.
    /// `None` while there are no samples.
    pub average_seconds: Option<u64>,
}

/// One recurring blocking issue: the raw finding text (A18 — no
/// categorization in V1) and how often it blocked a review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CountedText {
    pub text: String,
    pub count: u32,
}

/// "Author X's PRs always pass": verdict distribution across the author's
/// PR reviews.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuthorPattern {
    pub author: String,
    pub reviews: u32,
    pub approved: u32,
    pub changes_requested: u32,
    pub comment_only: u32,
    /// approved / reviews in 0.0..1.0.
    pub approval_rate: f64,
    /// Median blocking count across the author's reviews.
    pub median_blocking_count: f64,
}

/// Per-repo verdict distribution plus the repo's most frequent blockers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoPattern {
    pub repo: String,
    pub reviews: u32,
    pub approved: u32,
    pub changes_requested: u32,
    pub comment_only: u32,
    /// approved / reviews in 0.0..1.0.
    pub approval_rate: f64,
    pub top_blocking_issues: Vec<CountedText>,
}

/// Review effectiveness (DEV-PLAN §10): the share of changes_requested
/// reviews whose blocking issues were later fixed — operationalized as a
/// later review on the SAME PR coming back approved.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Effectiveness {
    pub changes_requested_reviews: u32,
    pub followed_by_approved: u32,
    /// followed_by_approved / changes_requested_reviews in 0.0..1.0.
    pub rate: f64,
}

/// Compute the report over `entries` (any order; duplicates the caller's
/// slice is not mutated). `window_days` drops entries older than
/// `now − window_days` before aggregating; `None` keeps everything.
pub fn compute(entries: &[HistoryEntry], now: DateTime<Utc>, window_days: Option<u64>) -> Insights {
    // Chronological working copy, optionally windowed. A stable sort keeps
    // same-timestamp entries in file (append) order, which matters for the
    // effectiveness "later review" relation.
    // A window too large to represent (`--days 100000000`) reaches back
    // before any entry: no cutoff, like all time.
    let cutoff = window_days.and_then(|days| {
        i64::try_from(days)
            .ok()
            .and_then(TimeDelta::try_days)
            .and_then(|window| now.checked_sub_signed(window))
    });
    let mut chrono_entries: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|e| cutoff.is_none_or(|cutoff| e.ts() >= cutoff))
        .collect();
    chrono_entries.sort_by_key(|e| e.ts());

    let mut reviews = ReviewsOverview {
        total: 0,
        failed: 0,
        skipped: 0,
        last_7_days: 0,
        last_30_days: 0,
        per_day: Vec::new(),
    };
    let mut per_day: BTreeMap<NaiveDate, u32> = BTreeMap::new();
    let mut issues: BTreeMap<String, (String, u32)> = BTreeMap::new();
    let mut authors: BTreeMap<String, PatternAccum> = BTreeMap::new();
    let mut repos: BTreeMap<String, PatternAccum> = BTreeMap::new();
    // Per-PR review stream, in chronological order, for merge latency and
    // effectiveness.
    let mut per_pr: BTreeMap<(String, u64), Vec<PrEvent>> = BTreeMap::new();

    let window_7 = now - TimeDelta::try_days(7).unwrap_or_else(TimeDelta::zero);
    let window_30 = now - TimeDelta::try_days(30).unwrap_or_else(TimeDelta::zero);

    for entry in &chrono_entries {
        match entry {
            HistoryEntry::ReviewCompleted {
                ts,
                repo,
                number,
                author,
                verdict,
                blocking_count,
                blocking_texts,
                ..
            } => {
                reviews.total += 1;
                if *ts >= window_7 {
                    reviews.last_7_days += 1;
                }
                if *ts >= window_30 {
                    reviews.last_30_days += 1;
                }
                *per_day.entry(ts.date_naive()).or_insert(0) += 1;

                let author_stats = authors.entry(author.clone()).or_default();
                author_stats.record(verdict, *blocking_count);
                let repo_stats = repos.entry(repo.clone()).or_default();
                repo_stats.record(verdict, *blocking_count);
                for text in blocking_texts {
                    let key = normalize_text(text);
                    if key.is_empty() {
                        continue;
                    }
                    let entry = issues.entry(key).or_insert_with(|| (text.clone(), 0));
                    entry.1 += 1;
                    repo_stats.issue(entry.0.clone());
                }
                per_pr
                    .entry((repo.clone(), *number))
                    .or_default()
                    .push(PrEvent::Reviewed {
                        ts: *ts,
                        verdict: *verdict,
                    });
            }
            HistoryEntry::ReviewFailed { .. } => reviews.failed += 1,
            HistoryEntry::ReviewSkipped { .. } => reviews.skipped += 1,
            HistoryEntry::PrMerged {
                ts, repo, number, ..
            } => {
                per_pr
                    .entry((repo.clone(), *number))
                    .or_default()
                    .push(PrEvent::Merged { ts: *ts });
            }
            HistoryEntry::PrClosed { .. } => {}
            // Re-review pending — not a review/merge/skip event.
            HistoryEntry::NewCommits { .. } => {}
            // A review began (design D6) — its outcome is counted when it lands.
            HistoryEntry::ReviewStarted { .. } => {}
            // An on-demand brief run (design B3) is not a review outcome.
            HistoryEntry::Brief { .. } => {}
            // Tracking began; a newer prx's event carries nothing to count.
            HistoryEntry::NewPr { .. } | HistoryEntry::Unknown => {}
        }
    }

    reviews.per_day = per_day
        .into_iter()
        .map(|(date, reviews)| DailyCount { date, reviews })
        .collect();

    let top_blocking_issues = top_texts(&issues, TOP_ISSUES);
    let per_author = authors
        .into_iter()
        .map(|(author, acc)| AuthorPattern {
            author,
            reviews: acc.reviews,
            approved: acc.approved,
            changes_requested: acc.changes_requested,
            comment_only: acc.comment_only,
            approval_rate: rate(acc.approved, acc.reviews),
            median_blocking_count: median(&acc.blocking_counts),
        })
        .collect();
    let per_repo = repos
        .into_iter()
        .map(|(repo, acc)| RepoPattern {
            repo,
            reviews: acc.reviews,
            approved: acc.approved,
            changes_requested: acc.changes_requested,
            comment_only: acc.comment_only,
            approval_rate: rate(acc.approved, acc.reviews),
            top_blocking_issues: top_texts(&acc.issues, TOP_REPO_ISSUES),
        })
        .collect();

    let mut merge_samples: Vec<u64> = Vec::new();
    for events in per_pr.values() {
        // Merge latency: a merge counts when a review happened since the
        // previous merge (or the beginning); the sample is measured from the
        // LATEST review before that merge.
        let mut last_review: Option<DateTime<Utc>> = None;
        for event in events {
            match *event {
                PrEvent::Reviewed { ts, .. } => last_review = Some(ts),
                PrEvent::Merged { ts } => {
                    if let Some(reviewed_at) = last_review {
                        let secs = (ts - reviewed_at).num_seconds().clamp(0, i64::MAX) as u64;
                        merge_samples.push(secs);
                    }
                    last_review = None;
                }
            }
        }
    }
    // Effectiveness pass (per-PR, chronological): a changes_requested review
    // counts as followed when any later review of the same PR is approved.
    // Two CRs followed by one approval count both — the approval is evidence
    // the blocking set was addressed. Comment-only/approved-only streams are
    // not counted in either direction.
    let mut changes_requested_reviews = 0u32;
    let mut followed_by_approved = 0u32;
    for events in per_pr.values() {
        let review_verdicts: Vec<Verdict> = events
            .iter()
            .filter_map(|e| match e {
                PrEvent::Reviewed { verdict, .. } => Some(*verdict),
                PrEvent::Merged { .. } => None,
            })
            .collect();
        for (i, verdict) in review_verdicts.iter().enumerate() {
            if *verdict != Verdict::ChangesRequested {
                continue;
            }
            changes_requested_reviews += 1;
            if review_verdicts[i + 1..].contains(&Verdict::Approved) {
                followed_by_approved += 1;
            }
        }
    }

    let average_seconds = if merge_samples.is_empty() {
        None
    } else {
        Some(merge_samples.iter().sum::<u64>() / merge_samples.len() as u64)
    };

    Insights {
        generated_at: now,
        window_days,
        reviews,
        review_to_merge: MergeLatency {
            samples: merge_samples.len() as u32,
            average_seconds,
        },
        top_blocking_issues,
        per_author,
        per_repo,
        effectiveness: Effectiveness {
            changes_requested_reviews,
            followed_by_approved,
            rate: rate(followed_by_approved, changes_requested_reviews),
        },
    }
}

/// One review-lifecycle event on a single PR, in chronological order.
#[derive(Debug, Clone, Copy)]
enum PrEvent {
    Reviewed { ts: DateTime<Utc>, verdict: Verdict },
    Merged { ts: DateTime<Utc> },
}

/// Verdict/blocking accumulation for one author or repo.
#[derive(Debug, Default)]
struct PatternAccum {
    reviews: u32,
    approved: u32,
    changes_requested: u32,
    comment_only: u32,
    blocking_counts: Vec<u32>,
    /// text → count (keyed by normalized text, display text kept).
    issues: BTreeMap<String, (String, u32)>,
}

impl PatternAccum {
    fn record(&mut self, verdict: &Verdict, blocking_count: u32) {
        self.reviews += 1;
        self.blocking_counts.push(blocking_count);
        match verdict {
            Verdict::Approved => self.approved += 1,
            Verdict::ChangesRequested => self.changes_requested += 1,
            Verdict::CommentOnly => self.comment_only += 1,
        }
    }

    fn issue(&mut self, display: String) {
        let key = normalize_text(&display);
        let entry = self.issues.entry(key).or_insert((display, 0));
        entry.1 += 1;
    }
}

/// Group by normalized (whitespace-collapsed, lowercase) text so
/// "Missing tenant scope" and "missing  tenant scope" aggregate; the first
/// seen spelling is kept for display. Ordered by count desc, then text.
fn top_texts(issues: &BTreeMap<String, (String, u32)>, cap: usize) -> Vec<CountedText> {
    let mut list: Vec<CountedText> = issues
        .values()
        .map(|(text, count)| CountedText {
            text: text.clone(),
            count: *count,
        })
        .collect();
    list.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.text.cmp(&b.text)));
    list.truncate(cap);
    list
}

/// Normalize finding text for grouping (A18 "exact-ish match").
fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn rate(numerator: u32, denominator: u32) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        f64::from(numerator) / f64::from(denominator)
    }
}

/// Median of a multiset (even counts average the two middle values).
fn median(values: &[u32]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<u32> = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        f64::from(sorted[mid])
    } else {
        (f64::from(sorted[mid - 1]) + f64::from(sorted[mid])) / 2.0
    }
}

// ------------------------------------------------------------------ render

/// Human-readable report for `prx insights` (deterministic layout).
pub fn render(insights: &Insights) -> String {
    let mut out = String::new();
    let window = match insights.window_days {
        Some(1) => "last 1 day".to_string(),
        Some(days) => format!("last {days} days"),
        None => "all time".to_string(),
    };
    out.push_str(&format!(
        "prx insights ({} · generated {} UTC)\n\n",
        window,
        insights.generated_at.format("%Y-%m-%d %H:%M")
    ));

    out.push_str("Reviews performed\n");
    let r = &insights.reviews;
    if r.total == 0 && r.failed == 0 && r.skipped == 0 {
        out.push_str("  no reviews recorded yet\n");
    } else {
        out.push_str(&format!(
            "  total: {} (last 7 days: {}, last 30 days: {}) · failed: {} · skipped: {}\n",
            r.total, r.last_7_days, r.last_30_days, r.failed, r.skipped
        ));
        if !r.per_day.is_empty() {
            out.push_str("  per day:\n");
            for day in &r.per_day {
                out.push_str(&format!(
                    "    {} {:>3} {}\n",
                    day.date.format("%Y-%m-%d"),
                    day.reviews,
                    "#".repeat(day.reviews as usize)
                ));
            }
        }
    }

    out.push_str("\nReview to merge\n");
    let m = insights.review_to_merge;
    if m.samples == 0 {
        out.push_str("  no PRs merged after a review yet\n");
    } else {
        out.push_str(&format!(
            "  {} PR(s): average {} from last review to merge\n",
            m.samples,
            format_duration(m.average_seconds.unwrap_or(0))
        ));
    }

    out.push_str("\nMost common blocking issues\n");
    out.push_str("  (from recorded finding texts; reviews recorded before finding texts were stored contribute nothing)\n");
    if insights.top_blocking_issues.is_empty() {
        out.push_str("  no blocking finding texts recorded\n");
    } else {
        for issue in &insights.top_blocking_issues {
            out.push_str(&format!("  {:>3} x {}\n", issue.count, issue.text));
        }
    }

    out.push_str("\nPer author\n");
    if insights.per_author.is_empty() {
        out.push_str("  no data\n");
    } else {
        for a in &insights.per_author {
            out.push_str(&format!(
                "  {:<16} {:>3} reviews · {:>5.1}% approved · median {} blocking\n",
                a.author,
                a.reviews,
                a.approval_rate * 100.0,
                format_median(a.median_blocking_count)
            ));
        }
    }

    out.push_str("\nPer repo\n");
    if insights.per_repo.is_empty() {
        out.push_str("  no data\n");
    } else {
        for repo in &insights.per_repo {
            out.push_str(&format!(
                "  {}: {} reviews · {:.1}% approved\n",
                repo.repo,
                repo.reviews,
                repo.approval_rate * 100.0
            ));
            for issue in &repo.top_blocking_issues {
                out.push_str(&format!("    {:>3} x {}\n", issue.count, issue.text));
            }
        }
    }

    out.push_str("\nReview effectiveness\n");
    let e = insights.effectiveness;
    if e.changes_requested_reviews == 0 {
        out.push_str("  no changes-requested reviews yet\n");
    } else {
        out.push_str(&format!(
            "  {} of {} changes-requested review(s) were followed by an approval on the same PR ({:.0}%)\n",
            e.followed_by_approved,
            e.changes_requested_reviews,
            e.rate * 100.0
        ));
    }
    out
}

fn format_median(median: f64) -> String {
    if median.fract() == 0.0 {
        format!("{}", median as u32)
    } else {
        format!("{median:.1}")
    }
}

/// Seconds as a compact human duration ("2d 3h", "45m", "90s").
fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PrRecord;

    fn rec(repo: &str, number: u64, author: &str) -> PrRecord {
        PrRecord {
            repo: repo.into(),
            number,
            title: "t".into(),
            author: author.into(),
            head_branch: "f/x".into(),
            head_sha: "aaaa".into(),
            is_draft: false,
            gh_created_at: Utc::now(),
            state: crate::state::PrState::Pending,
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
            review_session_info: None,
            last_manifest: None,
            review_attempts: 0,
            diff_too_large: false,
            skip_detail: None,
            human_approved_at: None,
            exec_summary: None,
            brief: None,
            updated_at: Utc::now(),
        }
    }

    #[allow(clippy::too_many_arguments)] // test fixture builder; every field is meaningful
    fn completed(
        repo: &str,
        number: u64,
        author: &str,
        verdict: Verdict,
        blocking: u32,
        nits: u32,
        ts: DateTime<Utc>,
        texts: &[&str],
    ) -> HistoryEntry {
        HistoryEntry::from_effect(
            &crate::machine::HistoryEvent::ReviewCompleted {
                verdict,
                blocking_count: blocking,
                nit_count: nits,
            },
            &rec(repo, number, author),
            Some("fake"),
            None,
            texts.iter().map(|s| s.to_string()).collect(),
            ts,
        )
    }

    fn failed(repo: &str, number: u64, author: &str, ts: DateTime<Utc>) -> HistoryEntry {
        HistoryEntry::from_effect(
            &crate::machine::HistoryEvent::ReviewFailed,
            &rec(repo, number, author),
            None,
            None,
            Vec::new(),
            ts,
        )
    }

    fn terminal(
        event: &crate::machine::HistoryEvent,
        repo: &str,
        number: u64,
        author: &str,
        ts: DateTime<Utc>,
    ) -> HistoryEntry {
        HistoryEntry::from_effect(
            event,
            &rec(repo, number, author),
            None,
            None,
            Vec::new(),
            ts,
        )
    }

    fn days_ago(n: i64) -> DateTime<Utc> {
        Utc::now() - TimeDelta::try_days(n).unwrap()
    }

    #[test]
    fn counts_reviews_per_day_and_windows() {
        let entries = vec![
            completed("o/r", 1, "a", Verdict::Approved, 0, 1, days_ago(1), &[]),
            completed(
                "o/r",
                2,
                "b",
                Verdict::ChangesRequested,
                2,
                0,
                days_ago(2),
                &[],
            ),
            completed("o/r", 3, "a", Verdict::Approved, 0, 0, days_ago(8), &[]),
            completed("o/r", 4, "b", Verdict::CommentOnly, 0, 0, days_ago(40), &[]),
            failed("o/r", 5, "a", days_ago(1)),
            terminal(
                &crate::machine::HistoryEvent::ReviewSkipped {
                    reason: "diff too large".into(),
                },
                "o/r",
                6,
                "a",
                days_ago(2),
            ),
        ];
        let ins = compute(&entries, Utc::now(), None);
        assert_eq!(ins.reviews.total, 4);
        assert_eq!(ins.reviews.failed, 1);
        assert_eq!(ins.reviews.skipped, 1);
        assert_eq!(ins.reviews.last_7_days, 2);
        assert_eq!(ins.reviews.last_30_days, 3);
        // Per-day series is ascending and only lists days with activity.
        let days: Vec<u32> = ins.reviews.per_day.iter().map(|d| d.reviews).collect();
        assert_eq!(days, vec![1, 1, 1, 1]);
        assert!(ins
            .reviews
            .per_day
            .windows(2)
            .all(|w| w[0].date < w[1].date));
    }

    #[test]
    fn merge_latency_uses_latest_review_before_merge_only() {
        let entries = vec![
            // PR 1: two reviews, then merge 1 day after the latest one.
            completed(
                "o/r",
                1,
                "a",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(5),
                &[],
            ),
            completed("o/r", 1, "a", Verdict::Approved, 0, 0, days_ago(1), &[]),
            terminal(
                &crate::machine::HistoryEvent::PrMerged,
                "o/r",
                1,
                "a",
                Utc::now(),
            ),
            // PR 2: merged without any review → no sample.
            terminal(
                &crate::machine::HistoryEvent::PrMerged,
                "o/r",
                2,
                "b",
                days_ago(0),
            ),
            // PR 3: reviewed but never merged → no sample.
            completed("o/r", 3, "b", Verdict::Approved, 0, 0, days_ago(2), &[]),
        ];
        let ins = compute(&entries, Utc::now(), None);
        assert_eq!(ins.review_to_merge.samples, 1);
        let expected = (days_ago(0) - days_ago(1)).num_seconds() as u64;
        let avg = ins.review_to_merge.average_seconds.unwrap();
        assert!(
            avg.abs_diff(expected) < 5,
            "average {avg} vs expected ~{expected}"
        );
    }

    #[test]
    fn blocking_issues_group_by_normalized_text_with_deterministic_order() {
        let entries = vec![
            completed(
                "o/r",
                1,
                "a",
                Verdict::ChangesRequested,
                2,
                0,
                days_ago(2),
                &["Missing tenant scope", "no tests"],
            ),
            completed(
                "o/r",
                2,
                "b",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(1),
                &["missing   tenant scope"],
            ),
            completed(
                "o/r",
                3,
                "b",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(1),
                &["no tests"],
            ),
        ];
        let ins = compute(&entries, Utc::now(), None);
        assert_eq!(ins.top_blocking_issues.len(), 2);
        // Tie on count (2 = 2) breaks alphabetically; higher count first.
        assert_eq!(ins.top_blocking_issues[0].text, "Missing tenant scope");
        assert_eq!(ins.top_blocking_issues[0].count, 2);
        assert_eq!(ins.top_blocking_issues[1].text, "no tests");
        assert_eq!(ins.top_blocking_issues[1].count, 2);
        // The repo rollup saw the same texts.
        assert_eq!(ins.per_repo[0].top_blocking_issues.len(), 2);
    }

    #[test]
    fn per_author_patterns_report_approval_rate_and_median_blocking() {
        let entries = vec![
            // alice: 3 reviews (2 approved, 1 CR), blocking 0,0,2 → median 0
            completed("o/r", 1, "alice", Verdict::Approved, 0, 0, days_ago(3), &[]),
            completed("o/r", 2, "alice", Verdict::Approved, 0, 0, days_ago(2), &[]),
            completed(
                "o/r",
                3,
                "alice",
                Verdict::ChangesRequested,
                2,
                0,
                days_ago(1),
                &[],
            ),
            // bob: 2 reviews (1 approved, 1 CR), blocking 1,3 → median 2.0
            completed("o/r", 4, "bob", Verdict::Approved, 1, 0, days_ago(2), &[]),
            completed(
                "o/r",
                5,
                "bob",
                Verdict::ChangesRequested,
                3,
                0,
                days_ago(1),
                &[],
            ),
        ];
        let ins = compute(&entries, Utc::now(), None);
        let alice = ins.per_author.iter().find(|a| a.author == "alice").unwrap();
        assert_eq!(alice.reviews, 3);
        assert_eq!(alice.approved, 2);
        assert_eq!(alice.changes_requested, 1);
        assert!((alice.approval_rate - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(alice.median_blocking_count, 0.0);
        let bob = ins.per_author.iter().find(|a| a.author == "bob").unwrap();
        assert!((bob.median_blocking_count - 2.0).abs() < 1e-9);
        // Deterministic author order (BTreeMap → alphabetical).
        let names: Vec<&str> = ins.per_author.iter().map(|a| a.author.as_str()).collect();
        assert_eq!(names, vec!["alice", "bob"]);
    }

    #[test]
    fn effectiveness_counts_only_same_pr_later_approvals() {
        let entries = vec![
            // PR 1: CR then approved → followed.
            completed(
                "o/r",
                1,
                "a",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(4),
                &[],
            ),
            completed("o/r", 1, "a", Verdict::Approved, 0, 0, days_ago(3), &[]),
            // PR 2: CR, never re-approved → not followed.
            completed(
                "o/r",
                2,
                "b",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(4),
                &[],
            ),
            // PR 3: CR then comment-only → not followed (only approvals count).
            completed(
                "o/r",
                3,
                "b",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(4),
                &[],
            ),
            completed("o/r", 3, "b", Verdict::CommentOnly, 0, 0, days_ago(3), &[]),
            // PR 4: CR → approved → CR → approved: both CRs followed.
            completed(
                "o/r",
                4,
                "c",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(5),
                &[],
            ),
            completed("o/r", 4, "c", Verdict::Approved, 0, 0, days_ago(4), &[]),
            completed(
                "o/r",
                4,
                "c",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(3),
                &[],
            ),
            completed("o/r", 4, "c", Verdict::Approved, 0, 0, days_ago(2), &[]),
            // PR 5: approved only → not counted at all.
            completed("o/r", 5, "c", Verdict::Approved, 0, 0, days_ago(2), &[]),
            // PR 6: approved BEFORE the CR → does not make the CR "followed"
            // (order in the file decides, not the verdict mix).
            completed("o/r", 6, "d", Verdict::Approved, 0, 0, days_ago(6), &[]),
            completed(
                "o/r",
                6,
                "d",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(5),
                &[],
            ),
        ];
        let ins = compute(&entries, Utc::now(), None);
        assert_eq!(ins.effectiveness.changes_requested_reviews, 6);
        assert_eq!(ins.effectiveness.followed_by_approved, 3);
        assert!((ins.effectiveness.rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn window_days_drops_old_entries() {
        let entries = vec![
            completed("o/r", 1, "a", Verdict::Approved, 0, 0, days_ago(100), &[]),
            completed(
                "o/r",
                2,
                "b",
                Verdict::ChangesRequested,
                1,
                0,
                days_ago(5),
                &["old bug"],
            ),
        ];
        let ins = compute(&entries, Utc::now(), Some(30));
        assert_eq!(ins.window_days, Some(30));
        assert_eq!(ins.reviews.total, 1);
        assert_eq!(ins.reviews.last_7_days, 1);
        assert_eq!(ins.top_blocking_issues.len(), 1);
        // All-time keeps everything.
        let all = compute(&entries, Utc::now(), None);
        assert_eq!(all.reviews.total, 2);
    }

    #[test]
    fn entries_without_texts_contribute_nothing_to_issue_lists() {
        // Old-style entries (counts only) must not fabricate issue texts.
        let entries = vec![completed(
            "o/r",
            1,
            "a",
            Verdict::ChangesRequested,
            3,
            0,
            days_ago(1),
            &[],
        )];
        let ins = compute(&entries, Utc::now(), None);
        assert!(ins.top_blocking_issues.is_empty());
        assert!(ins.per_repo[0].top_blocking_issues.is_empty());
        // …but the counts still feed per-author/median/effectiveness.
        assert_eq!(ins.per_author[0].median_blocking_count, 3.0);
        assert_eq!(ins.effectiveness.changes_requested_reviews, 1);
    }

    #[test]
    fn empty_history_renders_a_usable_report() {
        let ins = compute(&[], Utc::now(), None);
        let text = render(&ins);
        assert!(text.contains("no reviews recorded yet"), "{text}");
        assert!(text.contains("no PRs merged after a review yet"), "{text}");
        assert!(text.contains("no changes-requested reviews yet"), "{text}");
    }

    #[test]
    fn render_includes_all_sections_and_is_deterministic() {
        let entries = vec![
            completed(
                "org/one",
                1,
                "alice",
                Verdict::ChangesRequested,
                2,
                0,
                days_ago(2),
                &["missing tenant scope"],
            ),
            completed(
                "org/one",
                1,
                "alice",
                Verdict::Approved,
                0,
                0,
                days_ago(1),
                &[],
            ),
            terminal(
                &crate::machine::HistoryEvent::PrMerged,
                "org/one",
                1,
                "alice",
                Utc::now(),
            ),
            failed("org/two", 2, "bob", days_ago(1)),
        ];
        let ins = compute(&entries, Utc::now(), None);
        let a = render(&ins);
        let b = render(&ins);
        assert_eq!(a, b, "render must be deterministic");
        for section in [
            "Reviews performed",
            "Review to merge",
            "Most common blocking issues",
            "Per author",
            "Per repo",
            "Review effectiveness",
            "missing tenant scope",
            "alice",
            "org/one",
            "followed by an approval",
        ] {
            assert!(a.contains(section), "missing '{section}' in:\n{a}");
        }
        assert!(a.contains("1 of 1 changes-requested review(s)"), "{a}");
        assert!(a.contains("average"), "{a}");
    }

    #[test]
    fn merge_latency_reset_between_merges_and_duration_format() {
        // A PR that merged, was reopened, reviewed again and re-merged
        // produces exactly one sample per merge-after-review.
        let entries = vec![
            completed("o/r", 1, "a", Verdict::Approved, 0, 0, days_ago(3), &[]),
            terminal(
                &crate::machine::HistoryEvent::PrMerged,
                "o/r",
                1,
                "a",
                days_ago(2),
            ),
            completed("o/r", 1, "a", Verdict::Approved, 0, 0, days_ago(1), &[]),
            terminal(
                &crate::machine::HistoryEvent::PrMerged,
                "o/r",
                1,
                "a",
                days_ago(0),
            ),
        ];
        let ins = compute(&entries, Utc::now(), None);
        assert_eq!(ins.review_to_merge.samples, 2);
        // A review 30 minutes before the merge renders as "30m".
        assert_eq!(format_duration(1800), "30m");
        assert_eq!(format_duration(5), "5s");
        assert_eq!(format_duration(3 * 86_400 + 2 * 3_600), "3d 2h");
        assert_eq!(format_duration(2 * 3_600 + 5 * 60), "2h 5m");
    }

    fn at(ts: &str) -> DateTime<Utc> {
        ts.parse().unwrap()
    }

    fn approved(repo: &str, number: u64, author: &str, ts: DateTime<Utc>) -> HistoryEntry {
        completed(repo, number, author, Verdict::Approved, 0, 0, ts, &[])
    }

    /// One review, nothing else: every ratio with a zero denominator is 0.0
    /// (never NaN, which JSON cannot carry), and the report still serializes.
    #[test]
    fn single_review_reports_whole_rates_and_zero_for_empty_denominators() {
        let now = at("2026-09-30T12:00:00Z");
        let ins = compute(
            &[approved("o/r", 1, "alice", at("2026-09-29T12:00:00Z"))],
            now,
            None,
        );
        assert_eq!(ins.reviews.total, 1);
        assert_eq!((ins.reviews.last_7_days, ins.reviews.last_30_days), (1, 1));
        assert_eq!(ins.per_author.len(), 1);
        assert_eq!(ins.per_author[0].approval_rate, 1.0);
        assert_eq!(ins.per_author[0].median_blocking_count, 0.0);
        assert_eq!(ins.per_repo[0].approval_rate, 1.0);
        assert_eq!(
            ins.review_to_merge,
            MergeLatency {
                samples: 0,
                average_seconds: None
            }
        );
        assert_eq!(ins.effectiveness.changes_requested_reviews, 0);
        assert_eq!(ins.effectiveness.rate, 0.0);

        // Only a failure: no author or repo rows, so no 0/0 anywhere.
        let ins = compute(&[failed("o/r", 1, "alice", now)], now, None);
        assert!(ins.per_author.is_empty() && ins.per_repo.is_empty());
        let json = serde_json::to_value(&ins).unwrap();
        assert_eq!(json["effectiveness"]["rate"], 0.0);
        assert!(json["review_to_merge"]["average_seconds"].is_null());
        let text = render(&ins);
        assert!(
            text.contains("total: 0 (last 7 days: 0, last 30 days: 0) · failed: 1 · skipped: 0"),
            "{text}"
        );
        assert!(!text.contains("per day"), "{text}");
    }

    /// The window cutoff and the 7- and 30-day rollups are all inclusive at
    /// the boundary instant and exclusive one second before it.
    #[test]
    fn window_and_rollup_boundaries_are_inclusive() {
        let now = at("2026-09-30T12:00:00Z");
        let entries = [
            approved("o/r", 1, "a", at("2026-09-23T12:00:00Z")), // exactly 7 days
            approved("o/r", 2, "a", at("2026-09-23T11:59:59Z")),
            approved("o/r", 3, "a", at("2026-08-31T12:00:00Z")), // exactly 30 days
            approved("o/r", 4, "a", at("2026-08-31T11:59:59Z")),
        ];
        let all = compute(&entries, now, None);
        assert_eq!(all.reviews.total, 4);
        assert_eq!(all.reviews.last_7_days, 1);
        assert_eq!(all.reviews.last_30_days, 3);

        let week = compute(&entries, now, Some(7));
        assert_eq!(week.reviews.total, 1);
        assert_eq!(week.window_days, Some(7));
        let month = compute(&entries, now, Some(30));
        assert_eq!(month.reviews.total, 3);
        // A zero-day window is "from now on": only an entry stamped now.
        let none = compute(&entries, now, Some(0));
        assert_eq!(none.reviews.total, 0);
        let instant = compute(&[approved("o/r", 5, "a", now)], now, Some(0));
        assert_eq!(instant.reviews.total, 1);
    }

    /// Regression: `prx insights --days 100000000` panicked (`DateTime -
    /// TimeDelta` overflowed) and `--days 18446744073709551615` wrapped to a
    /// cutoff in the future, reporting an empty history. A window that
    /// reaches back further than time does is all of it.
    #[test]
    fn a_window_too_large_to_represent_covers_everything() {
        let now = at("2026-09-30T12:00:00Z");
        let entries = [
            approved("o/r", 1, "a", at("2026-09-29T12:00:00Z")),
            approved("o/r", 2, "a", at("1999-01-01T00:00:00Z")),
        ];
        for days in [100_000_000, i64::MAX as u64, u64::MAX] {
            let ins = compute(&entries, now, Some(days));
            assert_eq!(ins.reviews.total, 2, "{days} days");
            assert_eq!(ins.window_days, Some(days));
        }
    }

    /// The per-day series buckets by UTC calendar day, whatever the local
    /// time zone: one second either side of midnight is two days.
    #[test]
    fn per_day_series_splits_at_utc_midnight() {
        let now = at("2026-09-30T12:00:00Z");
        let entries = [
            approved("o/r", 1, "a", at("2026-09-28T23:59:59Z")),
            approved("o/r", 2, "a", at("2026-09-29T00:00:00Z")),
            approved("o/r", 3, "a", at("2026-09-29T23:59:59Z")),
        ];
        let ins = compute(&entries, now, None);
        assert_eq!(
            ins.reviews.per_day,
            [
                DailyCount {
                    date: NaiveDate::from_ymd_opt(2026, 9, 28).unwrap(),
                    reviews: 1
                },
                DailyCount {
                    date: NaiveDate::from_ymd_opt(2026, 9, 29).unwrap(),
                    reviews: 2
                },
            ]
        );
        let text = render(&ins);
        assert!(text.contains("    2026-09-28   1 #\n"), "{text}");
        assert!(text.contains("    2026-09-29   2 ##\n"), "{text}");
    }

    /// The caller's order is irrelevant: newest-first (what `history::query`
    /// returns) and oldest-first give the same report, with authors and
    /// repos in name order.
    #[test]
    fn report_does_not_depend_on_input_order() {
        let now = at("2026-09-30T12:00:00Z");
        let mut entries = vec![
            completed(
                "org/b",
                1,
                "zed",
                Verdict::ChangesRequested,
                3,
                0,
                at("2026-09-20T10:00:00Z"),
                &["no tests"],
            ),
            approved("org/b", 1, "zed", at("2026-09-21T10:00:00Z")),
            terminal(
                &crate::machine::HistoryEvent::PrMerged,
                "org/b",
                1,
                "zed",
                at("2026-09-21T12:00:00Z"),
            ),
            completed(
                "org/a",
                9,
                "amy",
                Verdict::CommentOnly,
                0,
                2,
                at("2026-09-22T10:00:00Z"),
                &[],
            ),
        ];
        let forward = compute(&entries, now, None);
        entries.reverse();
        let backward = compute(&entries, now, None);
        assert_eq!(forward, backward);

        let authors: Vec<&str> = forward
            .per_author
            .iter()
            .map(|a| a.author.as_str())
            .collect();
        assert_eq!(authors, ["amy", "zed"]);
        let repos: Vec<&str> = forward.per_repo.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(repos, ["org/a", "org/b"]);
        // Read chronologically even when handed newest-first: the approval
        // follows the changes-requested review, the merge follows both.
        assert_eq!(backward.effectiveness.followed_by_approved, 1);
        assert_eq!(backward.review_to_merge.average_seconds, Some(2 * 3_600));
        assert_eq!(forward.per_author[0].comment_only, 1);
    }

    /// A merge only counts after a review of that PR: a merge with no
    /// review, a merge recorded before the review, and a review on another
    /// PR are not samples. The average is whole seconds, rounded down.
    #[test]
    fn merges_without_a_prior_review_are_not_latency_samples() {
        let now = at("2026-09-30T12:00:00Z");
        let merged = |repo: &str, n: u64, ts: &str| {
            terminal(
                &crate::machine::HistoryEvent::PrMerged,
                repo,
                n,
                "a",
                at(ts),
            )
        };
        let entries = [
            merged("o/r", 1, "2026-09-20T10:00:00Z"),
            merged("o/r", 2, "2026-09-20T10:00:00Z"),
            approved("o/r", 2, "a", at("2026-09-20T11:00:00Z")),
            approved("o/r", 3, "a", at("2026-09-20T10:00:00Z")),
            merged("o/other", 3, "2026-09-20T12:00:00Z"),
        ];
        let ins = compute(&entries, now, None);
        assert_eq!(ins.review_to_merge.samples, 0);
        assert_eq!(ins.review_to_merge.average_seconds, None);

        let entries = [
            approved("o/r", 1, "a", at("2026-09-20T10:00:00Z")),
            merged("o/r", 1, "2026-09-20T10:00:01Z"),
            approved("o/r", 2, "a", at("2026-09-20T10:00:00Z")),
            merged("o/r", 2, "2026-09-20T10:00:02Z"),
            // Merged the instant it was reviewed: a zero-second sample.
            approved("o/r", 3, "a", at("2026-09-20T10:00:00Z")),
            merged("o/r", 3, "2026-09-20T10:00:00Z"),
        ];
        let ins = compute(&entries, now, None);
        assert_eq!(
            ins.review_to_merge,
            MergeLatency {
                samples: 3,
                average_seconds: Some(1)
            }
        );
    }

    /// Closed PRs, new commits, review starts, and on-demand briefs are not
    /// review outcomes: they add nothing to any count.
    #[test]
    fn non_outcome_events_change_nothing() {
        let now = at("2026-09-30T12:00:00Z");
        let base = [approved("o/r", 1, "a", at("2026-09-29T10:00:00Z"))];
        let mut noisy = base.to_vec();
        for event in [
            crate::machine::HistoryEvent::PrClosed,
            crate::machine::HistoryEvent::NewCommits,
            crate::machine::HistoryEvent::ReviewStarted,
        ] {
            noisy.push(terminal(&event, "o/r", 1, "a", at("2026-09-29T11:00:00Z")));
        }
        noisy.push(HistoryEntry::Brief {
            ts: at("2026-09-29T11:30:00Z"),
            repo: "o/r".into(),
            number: 1,
            title: "t".into(),
            author: "a".into(),
            agent: Some("fake".into()),
            brief: Some("what it does".into()),
            usage: None,
        });
        assert_eq!(compute(&noisy, now, None), compute(&base, now, None));
    }

    /// The issue lists are capped (10 overall, 5 per repo), most frequent
    /// first with ties in text order; blank texts are not issues.
    #[test]
    fn issue_lists_are_capped_and_blank_texts_ignored() {
        let now = at("2026-09-30T12:00:00Z");
        let texts: Vec<String> = (0..12).map(|i| format!("issue {i:02}")).collect();
        let mut refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        refs.extend(["", "   ", "\n\t"]);
        let mut entries = vec![completed(
            "o/r",
            1,
            "a",
            Verdict::ChangesRequested,
            12,
            0,
            at("2026-09-29T10:00:00Z"),
            &refs,
        )];
        // "issue 11" blocks a second review: it leads both lists.
        entries.push(completed(
            "o/r",
            2,
            "a",
            Verdict::ChangesRequested,
            1,
            0,
            at("2026-09-29T11:00:00Z"),
            &["Issue   11"],
        ));
        let ins = compute(&entries, now, None);
        let top: Vec<(&str, u32)> = ins
            .top_blocking_issues
            .iter()
            .map(|i| (i.text.as_str(), i.count))
            .collect();
        assert_eq!(top.len(), 10);
        assert_eq!(top[0], ("issue 11", 2));
        assert_eq!(top[1], ("issue 00", 1));
        assert_eq!(top[9], ("issue 08", 1));
        let repo = &ins.per_repo[0].top_blocking_issues;
        assert_eq!(repo.len(), 5);
        assert_eq!((repo[0].text.as_str(), repo[0].count), ("issue 11", 2));
        assert_eq!(repo[4].text, "issue 03");
    }

    #[test]
    fn median_handles_empty_odd_even_and_unsorted_input() {
        assert_eq!(median(&[]), 0.0);
        assert_eq!(median(&[5]), 5.0);
        assert_eq!(median(&[3, 1, 2]), 2.0);
        assert_eq!(median(&[4, 1, 3, 2]), 2.5);
        assert_eq!(median(&[0, 0, 0, 9]), 0.0);
        assert_eq!(median(&[u32::MAX, u32::MAX]), f64::from(u32::MAX));
        assert_eq!(format_median(2.0), "2");
        assert_eq!(format_median(2.5), "2.5");
        assert_eq!(format_median(0.0), "0");
    }

    #[test]
    fn rate_is_zero_for_an_empty_denominator() {
        assert_eq!(rate(0, 0), 0.0);
        assert_eq!(rate(3, 0), 0.0);
        assert_eq!(rate(1, 4), 0.25);
        assert_eq!(rate(4, 4), 1.0);
    }

    #[test]
    fn durations_switch_units_at_the_minute_hour_and_day() {
        for (seconds, text) in [
            (0, "0s"),
            (59, "59s"),
            (60, "1m"),
            (3_599, "59m"),
            (3_600, "1h 0m"),
            (86_399, "23h 59m"),
            (86_400, "1d 0h"),
            (90_000, "1d 1h"),
        ] {
            assert_eq!(format_duration(seconds), text, "{seconds}s");
        }
    }

    /// The header names the window: all time, N days, and the singular.
    #[test]
    fn header_names_the_window() {
        let now = at("2026-09-30T12:34:56Z");
        let header = |days| {
            let text = render(&compute(&[], now, days));
            text.lines().next().unwrap().to_string()
        };
        assert_eq!(
            header(None),
            "prx insights (all time · generated 2026-09-30 12:34 UTC)"
        );
        assert_eq!(
            header(Some(30)),
            "prx insights (last 30 days · generated 2026-09-30 12:34 UTC)"
        );
        assert_eq!(
            header(Some(1)),
            "prx insights (last 1 day · generated 2026-09-30 12:34 UTC)"
        );
    }

    /// The `GET /insights` body: `window_days` is absent for all time, and a
    /// report survives the trip to the CLI thin client unchanged.
    #[test]
    fn payload_omits_an_unset_window_and_round_trips() {
        let now = at("2026-09-30T12:00:00Z");
        let entries = [
            completed(
                "o/r",
                1,
                "a",
                Verdict::ChangesRequested,
                1,
                2,
                at("2026-09-29T10:00:00Z"),
                &["no tests"],
            ),
            approved("o/r", 1, "a", at("2026-09-29T12:00:00Z")),
        ];
        let all = compute(&entries, now, None);
        let json = serde_json::to_value(&all).unwrap();
        assert!(json.get("window_days").is_none(), "{json}");
        assert_eq!(json["generated_at"], "2026-09-30T12:00:00Z");
        assert_eq!(json["reviews"]["per_day"][0]["date"], "2026-09-29");
        assert_eq!(json["effectiveness"]["rate"], 1.0);
        assert_eq!(serde_json::from_value::<Insights>(json).unwrap(), all);

        let windowed = compute(&entries, now, Some(14));
        let json = serde_json::to_value(&windowed).unwrap();
        assert_eq!(json["window_days"], 14);
        assert_eq!(serde_json::from_value::<Insights>(json).unwrap(), windowed);
    }
}
