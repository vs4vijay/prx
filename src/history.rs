//! Review history (`history.jsonl`, spec §3.7 / DEV-PLAN §3): append-only
//! JSONL, one enriched entry per line. The machine emits effect *intents*
//! ([`crate::machine::HistoryEvent`]); [`HistoryEntry`] is the enriched
//! on-disk record (repo/PR/agent/verdict/counts/duration and — from the
//! reviewed manifest — truncated blocking finding texts, added by the
//! executor). Insights computation over these entries lives in
//! [`crate::insights`].

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::machine::HistoryEvent;
use crate::state::{PrRecord, Verdict};
pub use crate::usage::TokenUsage;

#[derive(Debug, Error)]
pub enum HistoryError {
    #[error("failed to write history file {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read history file {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// One enriched history record (the JSONL line). The `event` tag mirrors the
/// machine's `HistoryEvent` variants.
// The completed-review variant carries the usage records; entries are
// short-lived (read, filtered, serialized), so boxing would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HistoryEntry {
    /// A review began executing (design D6): written at the same moment the
    /// `on_review_started` notification fires. Not a review outcome.
    ReviewStarted {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    ReviewCompleted {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        verdict: Verdict,
        blocking_count: u32,
        nit_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_seconds: Option<u64>,
        /// Blocking finding bodies (A18: raw text, no categorization),
        /// truncated by [`truncate_finding_text`]. Entries written before
        /// this field existed (and non-blocking findings) carry nothing —
        /// insights simply see fewer texts than counts for old data.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        blocking_texts: Vec<String>,
        /// The brief recorded with this review (design B3): the manifest's
        /// brief, or the on-demand brief generated right after the review;
        /// absent when none was produced, briefs are off, or the line
        /// predates briefs. Lines written before the rename carry the same
        /// text as `exec_summary`, which still reads here.
        #[serde(
            default,
            alias = "exec_summary",
            skip_serializing_if = "Option::is_none"
        )]
        brief: Option<String>,
        /// Design D4: `false` when the run made no GitHub writes (monitor
        /// mode or a `post: false` run); absent when unknown (older lines).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        posted: Option<bool>,
        /// Design D11: the review run's reported token usage; absent when
        /// the engine reported none (or the line predates the field).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
        /// Design D12/B3: the brief run's usage (the key keeps its old
        /// name), written with the brief ([`set_review_brief`]); absent when
        /// unknown or the brief came from the manifest at no extra run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary_usage: Option<TokenUsage>,
    },
    /// A brief run requested on its own (design B3: `POST /brief`, `prx
    /// brief --regenerate`), not tied to a review entry. Carries the brief
    /// engine's usage, counted as brief usage.
    Brief {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
        /// The brief engine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        /// The generated brief; absent when generation failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        brief: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },
    ReviewFailed {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        /// Design D11: usage the failed attempts reported (partial runs
        /// included); absent when unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TokenUsage>,
    },
    ReviewSkipped {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        reason: String,
    },
    PrMerged {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
    },
    PrClosed {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
    },
    NewCommits {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
    },
}

impl HistoryEntry {
    /// Enrich a machine intent with the record snapshot + run metadata.
    /// `blocking_texts` carries the reviewed manifest's blocking finding
    /// bodies (truncated here — one choke point so no writer can bloat the
    /// JSONL); empty for poll-driven effects and pre-Phase-4 records.
    pub fn from_effect(
        event: &HistoryEvent,
        rec: &PrRecord,
        agent: Option<&str>,
        duration_seconds: Option<u64>,
        blocking_texts: Vec<String>,
        now: DateTime<Utc>,
    ) -> HistoryEntry {
        let repo = rec.repo.clone();
        let number = rec.number;
        let title = rec.title.clone();
        let author = rec.author.clone();
        let agent = agent.map(Into::into);
        match event {
            HistoryEvent::ReviewStarted => HistoryEntry::ReviewStarted {
                ts: now,
                repo,
                number,
                title,
                author,
                agent,
            },
            HistoryEvent::ReviewCompleted {
                verdict,
                blocking_count,
                nit_count,
            } => HistoryEntry::ReviewCompleted {
                ts: now,
                repo,
                number,
                title,
                author,
                agent,
                verdict: *verdict,
                blocking_count: *blocking_count,
                nit_count: *nit_count,
                duration_seconds,
                blocking_texts: blocking_texts
                    .into_iter()
                    .map(|t| truncate_finding_text(&t))
                    .collect(),
                brief: None,
                posted: None,
                usage: None,
                summary_usage: None,
            },
            HistoryEvent::ReviewFailed => HistoryEntry::ReviewFailed {
                ts: now,
                repo,
                number,
                title,
                author,
                agent,
                usage: None,
            },
            HistoryEvent::ReviewSkipped { reason } => HistoryEntry::ReviewSkipped {
                ts: now,
                repo,
                number,
                title,
                author,
                agent,
                reason: reason.clone(),
            },
            HistoryEvent::PrMerged => HistoryEntry::PrMerged {
                ts: now,
                repo,
                number,
                title,
                author,
            },
            HistoryEvent::PrClosed => HistoryEntry::PrClosed {
                ts: now,
                repo,
                number,
                title,
                author,
            },
            HistoryEvent::NewCommits => HistoryEntry::NewCommits {
                ts: now,
                repo,
                number,
                title,
                author,
            },
        }
    }

    /// The repo this entry belongs to (used by `query(repo=...)`).
    pub fn repo(&self) -> &str {
        match self {
            HistoryEntry::ReviewStarted { repo, .. }
            | HistoryEntry::ReviewCompleted { repo, .. }
            | HistoryEntry::ReviewFailed { repo, .. }
            | HistoryEntry::ReviewSkipped { repo, .. }
            | HistoryEntry::PrMerged { repo, .. }
            | HistoryEntry::PrClosed { repo, .. }
            | HistoryEntry::NewCommits { repo, .. }
            | HistoryEntry::Brief { repo, .. } => repo,
        }
    }

    /// The PR number of this entry (CLI timeline rendering).
    pub fn number(&self) -> u64 {
        match self {
            HistoryEntry::ReviewStarted { number, .. }
            | HistoryEntry::ReviewCompleted { number, .. }
            | HistoryEntry::ReviewFailed { number, .. }
            | HistoryEntry::ReviewSkipped { number, .. }
            | HistoryEntry::PrMerged { number, .. }
            | HistoryEntry::PrClosed { number, .. }
            | HistoryEntry::NewCommits { number, .. }
            | HistoryEntry::Brief { number, .. } => *number,
        }
    }

    /// When the event happened (used by retention compaction, A17).
    pub fn ts(&self) -> DateTime<Utc> {
        match self {
            HistoryEntry::ReviewStarted { ts, .. }
            | HistoryEntry::ReviewCompleted { ts, .. }
            | HistoryEntry::ReviewFailed { ts, .. }
            | HistoryEntry::ReviewSkipped { ts, .. }
            | HistoryEntry::PrMerged { ts, .. }
            | HistoryEntry::PrClosed { ts, .. }
            | HistoryEntry::NewCommits { ts, .. }
            | HistoryEntry::Brief { ts, .. } => *ts,
        }
    }

    /// CLI presentation icon per event (spec §2.1 icons where they overlap:
    /// merged/closed; verdicts reuse the reviewed-state icons).
    pub fn icon(&self) -> &'static str {
        match self {
            HistoryEntry::ReviewStarted { .. } => "\u{1f50d}", // 🔍
            HistoryEntry::ReviewCompleted { verdict, .. } => match verdict {
                Verdict::Approved => "\u{2705}",                 // ✅
                Verdict::ChangesRequested => "\u{26a0}\u{fe0f}", // ⚠️
                Verdict::CommentOnly => "\u{1f4ac}",             // 💬
            },
            HistoryEntry::ReviewFailed { .. } => "\u{274c}", // ❌
            HistoryEntry::ReviewSkipped { .. } => "\u{23ed}\u{fe0f}", // ⏭️
            HistoryEntry::PrMerged { .. } => "\u{2714}\u{fe0f}", // ✔️
            HistoryEntry::PrClosed { .. } => "\u{1f5d1}\u{fe0f}", // 🗑️
            HistoryEntry::NewCommits { .. } => "\u{1f501}",  // 🔁
            HistoryEntry::Brief { .. } => "\u{1f4dd}",       // 📝
        }
    }
}

/// Cap a stored finding text at ~120 chars (char-boundary safe) so the
/// append-only JSONL stays small (A18: raw text, not the whole manifest).
fn truncate_finding_text(text: &str) -> String {
    const MAX_CHARS: usize = 120;
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_CHARS {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(MAX_CHARS).collect();
    format!("{cut}\u{2026}")
}

/// Append one entry as a single JSON line. The file is created if missing.
pub fn append(path: &Path, entry: &HistoryEntry) -> Result<(), HistoryError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| HistoryError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| HistoryError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    let mut line = serde_json::to_string(entry).map_err(|e| HistoryError::Write {
        path: path.to_path_buf(),
        source: std::io::Error::other(format!("serialization failed: {e}")),
    })?;
    line.push('\n');
    file.write_all(line.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|source| HistoryError::Write {
            path: path.to_path_buf(),
            source,
        })
}

/// Read history entries, newest first. `repo` filters to one `org/repo`
/// (None = all); `limit` caps the result (0 = unlimited). Corrupt lines are
/// skipped with a warning — one bad line must not hide the whole history.
pub fn query(
    path: &Path,
    repo: Option<&str>,
    limit: usize,
) -> Result<Vec<HistoryEntry>, HistoryError> {
    query_page(path, repo, None, limit)
}

/// [`query`] for paging (the window's History page): only entries strictly
/// older than `before`, when set.
pub fn query_page(
    path: &Path,
    repo: Option<&str>,
    before: Option<DateTime<Utc>>,
    limit: usize,
) -> Result<Vec<HistoryEntry>, HistoryError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path).map_err(|source| HistoryError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut entries = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(line) {
            Ok(entry) => {
                if repo.is_none_or(|r| entry.repo() == r) && before.is_none_or(|b| entry.ts() < b) {
                    entries.push(entry);
                }
            }
            Err(e) => tracing::warn!("skipping corrupt history line: {e}"),
        }
    }
    entries.reverse(); // file is append-order → newest first
    if limit > 0 && entries.len() > limit {
        entries.truncate(limit);
    }
    Ok(entries)
}

/// Attach a brief and/or its run's usage (design D12/B3) to a review's
/// history entry after the fact (an on-demand brief is generated after the
/// review's completion was committed). Targets the newest
/// `review_completed` entry for `repo`#`number` stamped at or after `since`
/// (the review's completion time) and rewrites the file atomically. A
/// `None` leaves that field as it is. Returns whether an entry was updated.
/// Callers serialize this with every other history writer (the daemon holds
/// its state lock, as for appends).
pub fn set_review_brief(
    path: &Path,
    repo: &str,
    number: u64,
    since: DateTime<Utc>,
    summary: Option<&str>,
    usage: Option<&TokenUsage>,
) -> Result<bool, HistoryError> {
    if summary.is_none() && usage.is_none() {
        return Ok(false);
    }
    if !path.exists() {
        return Ok(false);
    }
    let raw = std::fs::read_to_string(path).map_err(|source| HistoryError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut lines: Vec<String> = raw.lines().map(str::to_string).collect();
    let mut updated = false;
    for line in lines.iter_mut().rev() {
        let Ok(mut entry) = serde_json::from_str::<HistoryEntry>(line) else {
            continue;
        };
        let HistoryEntry::ReviewCompleted {
            ts,
            repo: entry_repo,
            number: entry_number,
            brief,
            summary_usage,
            ..
        } = &mut entry
        else {
            continue;
        };
        if entry_repo != repo || *entry_number != number || *ts < since {
            continue;
        }
        if let Some(summary) = summary {
            *brief = Some(summary.to_string());
        }
        if let Some(usage) = usage {
            *summary_usage = Some(usage.clone());
        }
        *line = serde_json::to_string(&entry).map_err(|e| HistoryError::Write {
            path: path.to_path_buf(),
            source: std::io::Error::other(format!("serialization failed: {e}")),
        })?;
        updated = true;
        break;
    }
    if !updated {
        return Ok(false);
    }
    let mut payload = lines.join("\n");
    payload.push('\n');
    let temp = path.with_file_name(format!(
        ".{}.summary.{}",
        path.file_name()
            .map(std::ffi::OsStr::to_string_lossy)
            .unwrap_or_else(|| "history.jsonl".into()),
        std::process::id()
    ));
    std::fs::write(&temp, payload).map_err(|source| HistoryError::Write {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, path).map_err(|source| HistoryError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(true)
}

/// Retention compaction stats (audit A17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compaction {
    pub kept: usize,
    pub removed: usize,
}

/// Drop entries older than `days` and rewrite the file atomically (A17:
/// 90-day retention, run by the daemon on start). Unparseable lines are
/// dropped as well — they are unreadable to `query` anyway. A file with
/// nothing to remove is left untouched (no rewrite churn).
pub fn compact(path: &Path, days: u64) -> Result<Compaction, HistoryError> {
    if !path.exists() {
        return Ok(Compaction {
            kept: 0,
            removed: 0,
        });
    }
    let raw = std::fs::read_to_string(path).map_err(|source| HistoryError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let cutoff = Utc::now().date_naive()
        - chrono::TimeDelta::try_days(days as i64).unwrap_or_else(chrono::TimeDelta::zero);
    let mut kept_lines: Vec<String> = Vec::new();
    let mut removed = 0usize;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(line) {
            Ok(entry) => {
                if entry.ts().date_naive() >= cutoff {
                    kept_lines.push(line.to_string());
                } else {
                    removed += 1;
                }
            }
            Err(_) => removed += 1,
        }
    }
    if removed == 0 {
        return Ok(Compaction {
            kept: kept_lines.len(),
            removed: 0,
        });
    }
    let mut payload = kept_lines.join("\n");
    if !payload.is_empty() {
        payload.push('\n');
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| HistoryError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let temp = path.with_file_name(format!(
        ".{}.compact.{}",
        path.file_name()
            .map(std::ffi::OsStr::to_string_lossy)
            .unwrap_or_else(|| "history.jsonl".into()),
        std::process::id()
    ));
    std::fs::write(&temp, payload).map_err(|source| HistoryError::Write {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, path).map_err(|source| HistoryError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Compaction {
        kept: kept_lines.len(),
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::PrState;

    fn rec() -> PrRecord {
        PrRecord {
            repo: "org/one".into(),
            number: 7,
            title: "Add feature".into(),
            author: "alice".into(),
            head_branch: "f/x".into(),
            head_sha: "aaaa".into(),
            is_draft: false,
            gh_created_at: Utc::now(),
            state: PrState::Pending,
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
            updated_at: Utc::now(),
        }
    }

    fn dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prx-history-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// D8: the deferred summary lands on the newest matching completed
    /// review at/after the completion time; older entries, other PRs, and
    /// other event kinds are untouched.
    #[test]
    fn set_review_brief_updates_only_the_matching_completed_review() {
        let path = dir("set-summary").join("history.jsonl");
        let _ = std::fs::remove_file(&path);
        let r = rec();
        let mut other = rec();
        other.number = 8;
        let completed = |rec: &PrRecord, at: &str| {
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::Approved,
                    blocking_count: 0,
                    nit_count: 0,
                },
                rec,
                None,
                None,
                Vec::new(),
                at.parse().unwrap(),
            )
        };
        let since: DateTime<Utc> = "2026-09-01T10:00:00Z".parse().unwrap();
        assert!(!set_review_brief(&path, "org/one", 7, since, Some("x"), None).unwrap());
        append(&path, &completed(&r, "2026-09-01T09:00:00Z")).unwrap();
        append(&path, &completed(&r, "2026-09-01T10:00:01Z")).unwrap();
        append(&path, &completed(&other, "2026-09-01T10:00:02Z")).unwrap();
        append(
            &path,
            &HistoryEntry::from_effect(
                &HistoryEvent::NewCommits,
                &r,
                None,
                None,
                Vec::new(),
                "2026-09-01T10:00:03Z".parse().unwrap(),
            ),
        )
        .unwrap();
        assert!(set_review_brief(&path, "org/one", 7, since, Some("Fresh."), None).unwrap());
        let summaries: Vec<(u64, Option<String>)> = query(&path, None, 0)
            .unwrap()
            .into_iter()
            .filter_map(|e| match e {
                HistoryEntry::ReviewCompleted { number, brief, .. } => Some((number, brief)),
                _ => None,
            })
            .collect();
        assert_eq!(
            summaries,
            vec![(8, None), (7, Some("Fresh.".into())), (7, None)]
        );
        assert_eq!(query(&path, None, 0).unwrap().len(), 4);
        // No completed review at/after `since` for an unknown PR.
        assert!(!set_review_brief(&path, "org/one", 9, since, Some("x"), None).unwrap());
    }

    #[test]
    fn append_and_query_roundtrip_with_all_event_kinds() {
        let path = dir("roundtrip").join("history.jsonl");
        let r = rec();
        let entries = [
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::ChangesRequested,
                    blocking_count: 2,
                    nit_count: 1,
                },
                &r,
                Some("claude"),
                Some(95),
                vec!["missing tenant scope".to_string(), "no tests".to_string()],
                Utc::now(),
            ),
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewFailed,
                &r,
                Some("claude"),
                None,
                Vec::new(),
                Utc::now(),
            ),
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewSkipped {
                    reason: "diff too large".into(),
                },
                &r,
                Some("claude"),
                None,
                Vec::new(),
                Utc::now(),
            ),
            HistoryEntry::from_effect(
                &HistoryEvent::PrMerged,
                &r,
                None,
                None,
                Vec::new(),
                Utc::now(),
            ),
            HistoryEntry::from_effect(
                &HistoryEvent::PrClosed,
                &r,
                None,
                None,
                Vec::new(),
                Utc::now(),
            ),
            HistoryEntry::from_effect(
                &HistoryEvent::NewCommits,
                &r,
                None,
                None,
                Vec::new(),
                Utc::now(),
            ),
        ];
        for e in &entries {
            append(&path, e).unwrap();
        }

        // One JSON object per line.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count(), 6);

        let all = query(&path, None, 0).unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(all[0], entries[5]); // newest first
        assert_eq!(all[5], entries[0]);

        // Completed entry carries the enrichment.
        match &all[5] {
            HistoryEntry::ReviewCompleted {
                ts: _,
                agent,
                verdict,
                blocking_count,
                nit_count,
                duration_seconds,
                blocking_texts,
                repo,
                number,
                title,
                author,
                brief,
                posted,
                usage,
                summary_usage,
            } => {
                assert_eq!(*posted, None, "unknown unless a run sets it");
                assert_eq!((usage, summary_usage), (&None, &None));
                assert_eq!(agent.as_deref(), Some("claude"));
                assert_eq!(*verdict, Verdict::ChangesRequested);
                assert_eq!(*blocking_count, 2);
                assert_eq!(*nit_count, 1);
                assert_eq!(*duration_seconds, Some(95));
                assert_eq!(
                    blocking_texts,
                    &["missing tenant scope".to_string(), "no tests".to_string()]
                );
                assert_eq!(repo, "org/one");
                assert_eq!(*number, 7);
                assert_eq!(title, "Add feature");
                assert_eq!(author, "alice");
                assert_eq!(brief, &None);
            }
            other => panic!("expected ReviewCompleted, got {other:?}"),
        }

        // Repo filter + limit.
        let other_rec = PrRecord {
            repo: "org/two".into(),
            number: 9,
            ..rec()
        };
        append(
            &path,
            &HistoryEntry::from_effect(
                &HistoryEvent::PrMerged,
                &other_rec,
                None,
                None,
                Vec::new(),
                Utc::now(),
            ),
        )
        .unwrap();
        assert_eq!(query(&path, Some("org/two"), 0).unwrap().len(), 1);
        assert_eq!(query(&path, Some("org/one"), 0).unwrap().len(), 6);
        assert_eq!(query(&path, None, 2).unwrap().len(), 2); // 2 newest
    }

    #[test]
    fn missing_file_queries_empty_and_corrupt_lines_are_skipped() {
        let path = dir("corrupt").join("history.jsonl");
        assert!(query(&path, None, 10).unwrap().is_empty());

        let r = rec();
        append(
            &path,
            &HistoryEntry::from_effect(
                &HistoryEvent::PrMerged,
                &r,
                None,
                None,
                Vec::new(),
                Utc::now(),
            ),
        )
        .unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{{not json").unwrap();
        writeln!(f).unwrap(); // blank line tolerated

        let entries = query(&path, None, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], HistoryEntry::PrMerged { .. }));
    }

    #[test]
    fn compact_drops_entries_older_than_cutoff_and_rewrites_atomically() {
        let path = dir("compact").join("history.jsonl");
        let r = rec();
        let old = Utc::now() - chrono::TimeDelta::try_days(120).unwrap();
        let recent = Utc::now() - chrono::TimeDelta::try_days(2).unwrap();
        append(
            &path,
            &HistoryEntry::from_effect(&HistoryEvent::PrMerged, &r, None, None, Vec::new(), old),
        )
        .unwrap();
        append(
            &path,
            &HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::Approved,
                    blocking_count: 0,
                    nit_count: 1,
                },
                &r,
                None,
                None,
                Vec::new(),
                recent,
            ),
        )
        .unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "{{corrupt").unwrap();
        drop(f);

        let stats = compact(&path, 90).unwrap();
        assert_eq!(stats.kept, 1);
        assert_eq!(stats.removed, 2);

        let entries = query(&path, None, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], HistoryEntry::ReviewCompleted { .. }));

        // No temp files left behind.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".compact."))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        // Idempotent: a second run removes nothing and rewrites nothing.
        let stats2 = compact(&path, 90).unwrap();
        assert_eq!(stats2.removed, 0);
        assert_eq!(query(&path, None, 0).unwrap().len(), 1);

        // Compacting a missing file is a no-op.
        assert_eq!(
            compact(&dir("compact").join("nope.jsonl"), 90).unwrap(),
            Compaction {
                kept: 0,
                removed: 0
            }
        );
    }

    #[test]
    fn ts_accessor_reads_every_variant() {
        let r = rec();
        let now = Utc::now();
        assert_eq!(
            HistoryEntry::from_effect(&HistoryEvent::ReviewFailed, &r, None, None, Vec::new(), now)
                .ts(),
            now
        );
        assert_eq!(
            HistoryEntry::from_effect(&HistoryEvent::PrClosed, &r, None, None, Vec::new(), now)
                .ts(),
            now
        );
    }

    #[test]
    fn blocking_texts_are_truncated_and_collapsed() {
        let r = rec();
        let long = format!("{} end", "word ".repeat(40)); // >120 chars
        let entry = HistoryEntry::from_effect(
            &HistoryEvent::ReviewCompleted {
                verdict: Verdict::ChangesRequested,
                blocking_count: 2,
                nit_count: 0,
            },
            &r,
            None,
            None,
            vec!["  spaced   \t out  \n text ".repeat(3), long],
            Utc::now(),
        );
        match &entry {
            HistoryEntry::ReviewCompleted { blocking_texts, .. } => {
                assert_eq!(blocking_texts.len(), 2);
                assert_eq!(
                    blocking_texts[0],
                    "spaced out text spaced out text spaced out text"
                );
                assert!(
                    blocking_texts[1].chars().count() <= 121,
                    "{:?}",
                    blocking_texts[1]
                );
                assert!(blocking_texts[1].ends_with('\u{2026}'));
            }
            other => panic!("expected ReviewCompleted, got {other:?}"),
        }
    }

    #[test]
    fn old_entries_without_blocking_texts_deserialize() {
        // Pre-Phase-4 record: no blocking_texts key. `serde(default)` keeps
        // history forward-compatible so insights never breaks on old files.
        let raw = format!(
            r#"{{"event":"review_completed","ts":"{}","repo":"org/one","number":1,
                "title":"t","author":"a","verdict":"changes_requested",
                "blocking_count":1,"nit_count":0}}"#,
            Utc::now().to_rfc3339()
        );
        let parsed: HistoryEntry = serde_json::from_str(&raw).unwrap();
        match &parsed {
            HistoryEntry::ReviewCompleted {
                blocking_texts,
                blocking_count,
                ..
            } => {
                assert!(blocking_texts.is_empty());
                assert_eq!(*blocking_count, 1);
            }
            other => panic!("expected ReviewCompleted, got {other:?}"),
        }
    }

    #[test]
    fn new_commits_entry_roundtrips_with_serde_tag() {
        let r = rec();
        let now = Utc::now();
        let entry =
            HistoryEntry::from_effect(&HistoryEvent::NewCommits, &r, None, None, Vec::new(), now);
        match &entry {
            HistoryEntry::NewCommits {
                ts,
                repo,
                number,
                title,
                author,
            } => {
                assert_eq!(*ts, now);
                assert_eq!(repo, "org/one");
                assert_eq!(*number, 7);
                assert_eq!(title, "Add feature");
                assert_eq!(author, "alice");
            }
            other => panic!("expected NewCommits, got {other:?}"),
        }

        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""event":"new_commits""#), "{json}");

        let parsed: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn old_file_without_new_commits_kind_still_parses() {
        // A pre-2.2 history.jsonl carries no `new_commits` tag; per-line
        // tagged deserialization must keep reading it untouched.
        let path = dir("old-file").join("history.jsonl");
        let raw = format!(
            r#"{{"event":"pr_merged","ts":"{ts}","repo":"org/one","number":7,"title":"Add feature","author":"alice"}}
{{"event":"review_completed","ts":"{ts}","repo":"org/one","number":7,"title":"Add feature","author":"alice","verdict":"approved","blocking_count":0,"nit_count":0}}
"#,
            ts = Utc::now().to_rfc3339()
        );
        std::fs::write(&path, raw).unwrap();

        let entries = query(&path, None, 0).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0], HistoryEntry::ReviewCompleted { .. })); // newest first
        assert!(matches!(entries[1], HistoryEntry::PrMerged { .. }));
    }

    #[test]
    fn icon_covers_every_variant() {
        let r = rec();
        let now = Utc::now();
        let entries = [
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::Approved,
                    blocking_count: 0,
                    nit_count: 0,
                },
                &r,
                None,
                None,
                Vec::new(),
                now,
            ),
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::ChangesRequested,
                    blocking_count: 1,
                    nit_count: 0,
                },
                &r,
                None,
                None,
                Vec::new(),
                now,
            ),
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::CommentOnly,
                    blocking_count: 0,
                    nit_count: 0,
                },
                &r,
                None,
                None,
                Vec::new(),
                now,
            ),
            HistoryEntry::from_effect(&HistoryEvent::ReviewFailed, &r, None, None, Vec::new(), now),
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewSkipped { reason: "x".into() },
                &r,
                None,
                None,
                Vec::new(),
                now,
            ),
            HistoryEntry::from_effect(&HistoryEvent::PrMerged, &r, None, None, Vec::new(), now),
            HistoryEntry::from_effect(&HistoryEvent::PrClosed, &r, None, None, Vec::new(), now),
            HistoryEntry::from_effect(&HistoryEvent::NewCommits, &r, None, None, Vec::new(), now),
        ];
        let icons: std::collections::BTreeSet<&str> = entries.iter().map(|e| e.icon()).collect();
        assert_eq!(icons.len(), entries.len(), "each variant gets its own icon");
    }

    /// Design D4: `posted` round-trips, is omitted when unknown, and an old
    /// line without it reads as unknown.
    #[test]
    fn posted_round_trips_and_old_lines_read_as_unknown() {
        let old = r#"{"event":"review_completed","ts":"2026-09-01T10:00:00Z","repo":"org/one","number":1,"title":"t","author":"a","agent":"claude","verdict":"approved","blocking_count":0,"nit_count":0}"#;
        let entry: HistoryEntry = serde_json::from_str(old).unwrap();
        let HistoryEntry::ReviewCompleted { posted, .. } = &entry else {
            panic!("expected ReviewCompleted, got {entry:?}");
        };
        assert_eq!(*posted, None);
        assert!(!serde_json::to_string(&entry).unwrap().contains("posted"));

        let mut entry = entry;
        if let HistoryEntry::ReviewCompleted { posted, .. } = &mut entry {
            *posted = Some(false);
        }
        let line = serde_json::to_string(&entry).unwrap();
        assert!(line.contains(r#""posted":false"#), "{line}");
        assert_eq!(serde_json::from_str::<HistoryEntry>(&line).unwrap(), entry);
    }

    /// Design D11/D12: `usage` / `summary_usage` round-trip with only the
    /// reported fields, an old line reads as unknown, and unknown is never
    /// serialized (no zeros).
    #[test]
    fn usage_round_trips_and_unknown_is_never_written_as_zeros() {
        let old = r#"{"event":"review_failed","ts":"2026-09-01T10:00:00Z","repo":"org/one","number":1,"title":"t","author":"a","agent":"pi"}"#;
        let entry: HistoryEntry = serde_json::from_str(old).unwrap();
        assert!(matches!(
            entry,
            HistoryEntry::ReviewFailed { usage: None, .. }
        ));
        let line = serde_json::to_string(&entry).unwrap();
        assert!(
            !line.contains("usage") && !line.contains("tokens"),
            "{line}"
        );

        let mut entry = HistoryEntry::from_effect(
            &HistoryEvent::ReviewCompleted {
                verdict: Verdict::Approved,
                blocking_count: 0,
                nit_count: 0,
            },
            &rec(),
            Some("claude"),
            None,
            Vec::new(),
            "2026-09-01T10:00:00Z".parse().unwrap(),
        );
        let line = serde_json::to_string(&entry).unwrap();
        assert!(!line.contains("usage"), "{line}");
        if let HistoryEntry::ReviewCompleted {
            usage,
            summary_usage,
            ..
        } = &mut entry
        {
            *usage = Some(TokenUsage {
                input_tokens: Some(1200),
                output_tokens: Some(300),
                cost_usd: Some(0.42),
                model: Some("claude-sonnet-4-5".into()),
                session_id: Some("s-1".into()),
                ..Default::default()
            });
            *summary_usage = Some(TokenUsage {
                output_tokens: Some(80),
                ..Default::default()
            });
        }
        let line = serde_json::to_string(&entry).unwrap();
        assert!(
            line.contains(r#""usage":{"input_tokens":1200,"output_tokens":300,"cost_usd":0.42,"model":"claude-sonnet-4-5","session_id":"s-1"}"#),
            "{line}"
        );
        assert!(
            line.contains(r#""summary_usage":{"output_tokens":80}"#),
            "{line}"
        );
        assert!(
            !line.contains("cache"),
            "unreported fields are omitted: {line}"
        );
        assert_eq!(serde_json::from_str::<HistoryEntry>(&line).unwrap(), entry);
    }

    /// D12: summary usage attaches with (or without) the summary text.
    #[test]
    fn set_review_brief_stores_the_summary_usage() {
        let path = dir("summary-usage").join("history.jsonl");
        let _ = std::fs::remove_file(&path);
        let at: DateTime<Utc> = "2026-09-01T10:00:00Z".parse().unwrap();
        append(
            &path,
            &HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::Approved,
                    blocking_count: 0,
                    nit_count: 0,
                },
                &rec(),
                None,
                None,
                Vec::new(),
                at,
            ),
        )
        .unwrap();
        let usage = TokenUsage {
            input_tokens: Some(10),
            ..Default::default()
        };
        assert!(!set_review_brief(&path, "org/one", 7, at, None, None).unwrap());
        assert!(set_review_brief(&path, "org/one", 7, at, None, Some(&usage)).unwrap());
        assert!(set_review_brief(&path, "org/one", 7, at, Some("S."), None).unwrap());
        match &query(&path, None, 0).unwrap()[0] {
            HistoryEntry::ReviewCompleted {
                brief,
                summary_usage,
                usage: review_usage,
                ..
            } => {
                assert_eq!(brief.as_deref(), Some("S."));
                assert_eq!(summary_usage.as_ref(), Some(&usage));
                assert_eq!(review_usage, &None);
            }
            other => panic!("expected ReviewCompleted, got {other:?}"),
        }
    }

    /// Design B3: lines written before the rename keep their summary as
    /// the review's brief; new lines write `brief` only.
    #[test]
    fn legacy_exec_summary_lines_read_as_brief() {
        let line = r#"{"event":"review_completed","ts":"2026-09-01T10:00:00Z","repo":"org/one","number":7,"title":"t","author":"a","verdict":"approved","blocking_count":0,"nit_count":0,"exec_summary":"Old summary.","summary_usage":{"output_tokens":80}}"#;
        let entry: HistoryEntry = serde_json::from_str(line).unwrap();
        let HistoryEntry::ReviewCompleted {
            brief,
            summary_usage,
            ..
        } = &entry
        else {
            panic!("expected ReviewCompleted, got {entry:?}");
        };
        assert_eq!(brief.as_deref(), Some("Old summary."));
        assert_eq!(summary_usage.as_ref().unwrap().output_tokens, Some(80));
        let written = serde_json::to_string(&entry).unwrap();
        assert!(written.contains(r#""brief":"Old summary.""#), "{written}");
        assert!(!written.contains("exec_summary"), "{written}");
    }

    /// Design B3: an on-demand brief run is its own `brief` event with the
    /// engine's usage; it round-trips and answers the shared accessors.
    #[test]
    fn brief_event_round_trips() {
        let path = dir("brief-event").join("history.jsonl");
        let _ = std::fs::remove_file(&path);
        let ts: DateTime<Utc> = "2026-09-02T08:00:00Z".parse().unwrap();
        let entry = HistoryEntry::Brief {
            ts,
            repo: "org/one".into(),
            number: 7,
            title: "Add feature".into(),
            author: "alice".into(),
            agent: Some("claude".into()),
            brief: Some("Adds a cache.".into()),
            usage: Some(TokenUsage {
                input_tokens: Some(1200),
                output_tokens: Some(90),
                ..Default::default()
            }),
        };
        append(&path, &entry).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with(r#"{"event":"brief","#), "{raw}");
        let back = query(&path, Some("org/one"), 0).unwrap();
        assert_eq!(back, vec![entry.clone()]);
        assert_eq!(entry.repo(), "org/one");
        assert_eq!(entry.number(), 7);
        assert_eq!(entry.ts(), ts);
        assert_eq!(entry.icon(), "\u{1f4dd}");
        // A failed run (no text) omits the brief but keeps the usage.
        let failed = HistoryEntry::Brief {
            ts,
            repo: "org/one".into(),
            number: 7,
            title: "t".into(),
            author: "a".into(),
            agent: None,
            brief: None,
            usage: None,
        };
        let json = serde_json::to_string(&failed).unwrap();
        assert!(!json.contains("\"brief\":"), "{json}");
    }

    /// The window's History page pages with `before` (exclusive).
    #[test]
    fn query_page_returns_entries_strictly_older_than_before() {
        let path = dir("page").join("history.jsonl");
        let _ = std::fs::remove_file(&path);
        let r = rec();
        for minute in 0..5 {
            let ts = format!("2026-09-01T10:0{minute}:00Z").parse().unwrap();
            append(
                &path,
                &HistoryEntry::from_effect(
                    &HistoryEvent::NewCommits,
                    &r,
                    None,
                    None,
                    Vec::new(),
                    ts,
                ),
            )
            .unwrap();
        }
        let before: DateTime<Utc> = "2026-09-01T10:03:00Z".parse().unwrap();
        let page = query_page(&path, Some("org/one"), Some(before), 2).unwrap();
        let stamps: Vec<String> = page.iter().map(|e| e.ts().to_rfc3339()).collect();
        assert_eq!(
            stamps,
            ["2026-09-01T10:02:00+00:00", "2026-09-01T10:01:00+00:00"]
        );
        assert_eq!(query_page(&path, None, Some(before), 0).unwrap().len(), 3);
        assert!(query_page(&path, Some("org/two"), Some(before), 0)
            .unwrap()
            .is_empty());
    }
}
