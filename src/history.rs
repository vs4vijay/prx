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
    /// prx started tracking an open PR (first seen, or reopened): written
    /// with the `on_new_pr` notification, so PRX.app can raise its banner
    /// while the daemon defers to it.
    NewPr {
        ts: DateTime<Utc>,
        repo: String,
        number: u64,
        title: String,
        author: String,
    },
    /// An entry whose `event` this build does not know (written by a newer
    /// prx, or sent by a newer daemon). It decodes instead of failing the
    /// whole line or payload, carries nothing, and readers skip it:
    /// [`query`] leaves it out and [`compact`] keeps its line untouched.
    /// Never written by this build.
    #[serde(other)]
    Unknown,
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
            HistoryEvent::NewPr => HistoryEntry::NewPr {
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
            | HistoryEntry::NewPr { repo, .. }
            | HistoryEntry::Brief { repo, .. } => repo,
            HistoryEntry::Unknown => "",
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
            | HistoryEntry::NewPr { number, .. }
            | HistoryEntry::Brief { number, .. } => *number,
            HistoryEntry::Unknown => 0,
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
            | HistoryEntry::NewPr { ts, .. }
            | HistoryEntry::Brief { ts, .. } => *ts,
            // Undated: sorts before everything (readers skip it anyway).
            HistoryEntry::Unknown => DateTime::<Utc>::UNIX_EPOCH,
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
            HistoryEntry::NewPr { .. } => "\u{1f195}",       // 🆕
            HistoryEntry::Unknown => "\u{2022}",             // •
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
        .read(true)
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
    // A writer killed mid-line leaves the file without its final newline.
    // Start on a fresh line then, or this entry would be glued onto the torn
    // one and lost with it.
    if ends_mid_line(&mut file) {
        line.insert(0, '\n');
    }
    file.write_all(line.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|source| HistoryError::Write {
            path: path.to_path_buf(),
            source,
        })
}

/// Whether the (non-empty) file's last byte is not a newline. Read errors
/// count as "no": the append itself reports a broken file.
fn ends_mid_line(file: &mut std::fs::File) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// The file's text, with bytes that are not UTF-8 replaced (U+FFFD) instead
/// of failing the read: a line torn inside a multi-byte character is one
/// corrupt line to skip, not a reason to lose the whole history.
fn read_lossy(path: &Path) -> Result<String, HistoryError> {
    let bytes = std::fs::read(path).map_err(|source| HistoryError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
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
    let raw = read_lossy(path)?;
    let mut entries = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(line) {
            // A newer prx's event: nothing this build can show.
            Ok(HistoryEntry::Unknown) => {
                tracing::debug!("skipping history line with an unknown event");
            }
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
    let raw = read_lossy(path)?;
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
    let raw = read_lossy(path)?;
    // A window too large to represent reaches back before any entry.
    let cutoff = i64::try_from(days)
        .ok()
        .and_then(chrono::TimeDelta::try_days)
        .and_then(|window| Utc::now().date_naive().checked_sub_signed(window))
        .unwrap_or(chrono::NaiveDate::MIN);
    let mut kept_lines: Vec<String> = Vec::new();
    let mut removed = 0usize;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryEntry>(line) {
            // A newer prx's event: its date cannot be read here, so its
            // line is kept as written rather than lost.
            Ok(HistoryEntry::Unknown) => kept_lines.push(line.to_string()),
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
    fn new_pr_entry_roundtrips_with_serde_tag() {
        let entry = HistoryEntry::from_effect(
            &HistoryEvent::NewPr,
            &rec(),
            None,
            None,
            Vec::new(),
            Utc::now(),
        );
        assert!(matches!(&entry, HistoryEntry::NewPr { number: 7, .. }));
        let json = serde_json::to_string(&entry).unwrap();
        assert!(json.contains(r#""event":"new_pr""#), "{json}");
        assert_eq!(serde_json::from_str::<HistoryEntry>(&json).unwrap(), entry);
    }

    /// An event this build does not know (a newer prx's) decodes as
    /// `Unknown` instead of failing: alone, and inside a payload list, so
    /// one new variant never breaks `GET /history` for an older client.
    #[test]
    fn unknown_event_decodes_as_unknown() {
        let line = r#"{"event":"pr_teleported","ts":"2026-10-01T00:00:00Z","repo":"o/r","number":3,"extra":{"x":1}}"#;
        assert_eq!(
            serde_json::from_str::<HistoryEntry>(line).unwrap(),
            HistoryEntry::Unknown
        );
        let payload = format!(
            r#"[{line},{{"event":"pr_merged","ts":"2026-10-01T00:00:00Z","repo":"o/r","number":3,"title":"t","author":"a"}}]"#
        );
        let entries: Vec<HistoryEntry> = serde_json::from_str(&payload).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], HistoryEntry::Unknown);
        assert!(matches!(
            entries[1],
            HistoryEntry::PrMerged { number: 3, .. }
        ));
        // A known event with a field it does not know still decodes fully.
        let newer = r#"{"event":"pr_closed","ts":"2026-10-01T00:00:00Z","repo":"o/r","number":4,"title":"t","author":"a","shiny":true}"#;
        assert!(matches!(
            serde_json::from_str::<HistoryEntry>(newer).unwrap(),
            HistoryEntry::PrClosed { number: 4, .. }
        ));
    }

    /// Readers skip an unknown line; compaction keeps it as written (its
    /// age cannot be read here, and a newer prx still wants it).
    #[test]
    fn unknown_lines_are_skipped_by_query_and_kept_by_compaction() {
        let path = dir("unknown-lines").join("history.jsonl");
        let unknown =
            r#"{"event":"pr_teleported","ts":"2001-01-01T00:00:00Z","repo":"o/r","number":3}"#;
        let raw = format!(
            "{unknown}\n{{\"event\":\"pr_merged\",\"ts\":\"2001-01-01T00:00:00Z\",\"repo\":\"o/r\",\"number\":3,\"title\":\"t\",\"author\":\"a\"}}\n"
        );
        std::fs::write(&path, raw).unwrap();
        let entries = query(&path, None, 0).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0], HistoryEntry::PrMerged { .. }));

        let stats = compact(&path, 90).unwrap();
        assert_eq!(
            stats,
            Compaction {
                kept: 1,
                removed: 1
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{unknown}\n")
        );
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

    /// A fresh, empty history file for one test.
    fn fresh(tag: &str) -> std::path::PathBuf {
        let path = dir(tag).join("history.jsonl");
        let _ = std::fs::remove_file(&path);
        path
    }

    fn merged(repo: &str, number: u64, ts: &str) -> HistoryEntry {
        HistoryEntry::PrMerged {
            ts: ts.parse().unwrap(),
            repo: repo.into(),
            number,
            title: format!("PR {number}"),
            author: "alice".into(),
        }
    }

    fn numbers(entries: &[HistoryEntry]) -> Vec<u64> {
        entries.iter().map(HistoryEntry::number).collect()
    }

    /// "Newest first" is append order reversed, whatever the timestamps say;
    /// the repo filter applies before the limit, and a limit beyond the end
    /// (or 0) returns everything.
    #[test]
    fn query_orders_by_append_position_and_filters_before_limiting() {
        let path = fresh("order");
        append(&path, &merged("org/one", 1, "2026-09-03T00:00:00Z")).unwrap();
        append(&path, &merged("org/two", 2, "2026-09-01T00:00:00Z")).unwrap();
        append(&path, &merged("org/one", 3, "2026-09-02T00:00:00Z")).unwrap();
        append(&path, &merged("org/two", 4, "2026-09-04T00:00:00Z")).unwrap();

        assert_eq!(numbers(&query(&path, None, 0).unwrap()), [4, 3, 2, 1]);
        assert_eq!(numbers(&query(&path, None, 3).unwrap()), [4, 3, 2]);
        assert_eq!(numbers(&query(&path, None, 99).unwrap()), [4, 3, 2, 1]);
        // The two newest org/one entries, not "org/one among the two newest".
        assert_eq!(numbers(&query(&path, Some("org/one"), 2).unwrap()), [3, 1]);
        assert_eq!(numbers(&query(&path, Some("org/one"), 1).unwrap()), [3]);
        // The filter is an exact match, not a prefix.
        assert!(query(&path, Some("org/on"), 0).unwrap().is_empty());
        assert!(query(&path, Some("org/one/"), 0).unwrap().is_empty());
    }

    /// Regression: a writer killed mid-line leaves the file without a final
    /// newline. The next append used to be glued onto that torn line, so a
    /// perfectly good entry was lost with it.
    #[test]
    fn append_after_a_torn_last_line_starts_a_fresh_line() {
        let path = fresh("torn");
        append(&path, &merged("org/one", 1, "2026-09-01T00:00:00Z")).unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        write!(f, r#"{{"event":"pr_merged","ts":"2026-09-02T00"#).unwrap();
        drop(f);

        append(&path, &merged("org/one", 3, "2026-09-03T00:00:00Z")).unwrap();
        append(&path, &merged("org/one", 4, "2026-09-04T00:00:00Z")).unwrap();

        assert_eq!(numbers(&query(&path, None, 0).unwrap()), [4, 3, 1]);
        let raw = std::fs::read_to_string(&path).unwrap();
        // The torn line stays on a line of its own; nothing else is padded.
        assert_eq!(raw.lines().count(), 4, "{raw}");
        assert!(raw.ends_with("}\n"), "{raw}");
        assert!(!raw.contains("\n\n"), "{raw}");
    }

    /// Regression: one line torn inside a multi-byte character made the
    /// whole file unreadable (`stream did not contain valid UTF-8`), which
    /// failed `prx history`, `prx insights`, `prx usage` and the daemon's
    /// compaction. It is one corrupt line like any other.
    #[test]
    fn a_line_that_is_not_utf8_is_skipped_like_any_corrupt_line() {
        let path = fresh("not-utf8");
        let since: DateTime<Utc> = "2026-09-01T00:00:00Z".parse().unwrap();
        let recent = Utc::now();
        let review = HistoryEntry::from_effect(
            &HistoryEvent::ReviewCompleted {
                verdict: Verdict::Approved,
                blocking_count: 0,
                nit_count: 0,
            },
            &rec(),
            Some("claude"),
            None,
            Vec::new(),
            recent,
        );
        append(&path, &review).unwrap();
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        // "café" cut after the first byte of the two-byte "é".
        f.write_all(b"{\"event\":\"pr_merged\",\"title\":\"caf\xc3\n")
            .unwrap();
        drop(f);
        let later = HistoryEntry::from_effect(
            &HistoryEvent::NewCommits,
            &rec(),
            None,
            None,
            Vec::new(),
            recent,
        );
        append(&path, &later).unwrap();

        let entries = query(&path, None, 0).unwrap();
        assert_eq!(entries, [later.clone(), review]);

        // The other two readers of the file cope the same way.
        assert!(set_review_brief(&path, "org/one", 7, since, Some("a brief"), None).unwrap());
        assert_eq!(
            compact(&path, 90).unwrap(),
            Compaction {
                kept: 2,
                removed: 1
            }
        );
        let entries = query(&path, None, 0).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(matches!(
            &entries[1],
            HistoryEntry::ReviewCompleted { brief: Some(b), .. } if b == "a brief"
        ));
    }

    /// Each entry is written with one append-mode write, so writers in
    /// different threads (or processes: the CLI fallback next to a daemon)
    /// never interleave bytes and every writer's entries keep their order.
    #[test]
    fn concurrent_appends_keep_every_line_whole() {
        const WRITERS: u64 = 8;
        const PER_WRITER: u64 = 60;
        let path = fresh("concurrent");
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let path = &path;
                scope.spawn(move || {
                    for i in 0..PER_WRITER {
                        let entry = HistoryEntry::ReviewSkipped {
                            ts: "2026-09-01T00:00:00Z".parse().unwrap(),
                            repo: format!("org/w{writer}"),
                            number: i,
                            title: "t".repeat(2_000),
                            author: "alice".into(),
                            agent: None,
                            reason: "diff too large".into(),
                        };
                        append(path, &entry).unwrap();
                    }
                });
            }
        });

        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.lines().count() as u64, WRITERS * PER_WRITER);
        let all = query(&path, None, 0).unwrap();
        assert_eq!(all.len() as u64, WRITERS * PER_WRITER, "no line was torn");
        for writer in 0..WRITERS {
            let mine = query(&path, Some(&format!("org/w{writer}")), 0).unwrap();
            let expected: Vec<u64> = (0..PER_WRITER).rev().collect();
            assert_eq!(numbers(&mine), expected, "writer {writer}");
        }
    }

    /// A log far larger than the retention window normally leaves behind
    /// still pages correctly from the newest end.
    #[test]
    fn large_log_is_queried_from_the_newest_end() {
        let path = fresh("large");
        let mut payload = String::new();
        for n in 0..20_000u64 {
            let repo = if n % 2 == 0 { "org/even" } else { "org/odd" };
            let entry = merged(repo, n, "2026-09-01T00:00:00Z");
            payload.push_str(&serde_json::to_string(&entry).unwrap());
            payload.push('\n');
        }
        std::fs::write(&path, payload).unwrap();

        assert_eq!(query(&path, None, 0).unwrap().len(), 20_000);
        assert_eq!(
            numbers(&query(&path, None, 3).unwrap()),
            [19_999, 19_998, 19_997]
        );
        assert_eq!(
            numbers(&query(&path, Some("org/even"), 2).unwrap()),
            [19_998, 19_996]
        );
        assert_eq!(query(&path, Some("org/odd"), 0).unwrap().len(), 10_000);
    }

    /// Retention is by calendar day (UTC): an entry exactly `days` days old
    /// is kept, one a day older goes. A window of 0 keeps only today's.
    #[test]
    fn compact_keeps_the_boundary_day_and_drops_the_day_before() {
        let path = fresh("compact-boundary");
        // Noon today: whole-day offsets from it cannot straddle a date line.
        let noon = Utc::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc();
        let at = |days: i64| (noon - chrono::TimeDelta::try_days(days).unwrap()).to_rfc3339();
        append(&path, &merged("org/one", 91, &at(91))).unwrap();
        append(&path, &merged("org/one", 90, &at(90))).unwrap();
        append(&path, &merged("org/one", 1, &at(1))).unwrap();
        append(&path, &merged("org/one", 0, &at(0))).unwrap();

        assert_eq!(
            compact(&path, 90).unwrap(),
            Compaction {
                kept: 3,
                removed: 1
            }
        );
        assert_eq!(numbers(&query(&path, None, 0).unwrap()), [0, 1, 90]);

        assert_eq!(
            compact(&path, 0).unwrap(),
            Compaction {
                kept: 1,
                removed: 2
            }
        );
        assert_eq!(numbers(&query(&path, None, 0).unwrap()), [0]);
    }

    /// Kept lines are copied byte for byte (a field written by a newer prx
    /// survives an older prx's compaction), a file with nothing to drop is
    /// not rewritten, and dropping everything leaves an empty file behind.
    #[test]
    fn compact_keeps_lines_verbatim_and_leaves_a_clean_file_alone() {
        let path = fresh("compact-verbatim");
        let today = Utc::now().to_rfc3339();
        let newer = format!(
            r#"{{"event":"pr_closed","ts":"{today}","repo":"org/one","number":5,"title":"t","author":"a","added_later":{{"x":1}}}}"#
        );
        let old = r#"{"event":"pr_closed","ts":"2020-01-01T00:00:00Z","repo":"org/one","number":4,"title":"t","author":"a"}"#;
        std::fs::write(&path, format!("{old}\n\n{newer}\n")).unwrap();

        assert_eq!(
            compact(&path, 90).unwrap(),
            Compaction {
                kept: 1,
                removed: 1
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("{newer}\n")
        );

        // Nothing to remove (blank lines do not count): the bytes stay put.
        let untouched = format!("\n{newer}\n\n");
        std::fs::write(&path, &untouched).unwrap();
        assert_eq!(
            compact(&path, 90).unwrap(),
            Compaction {
                kept: 1,
                removed: 0
            }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), untouched);

        // Everything expired: an empty file, not a lone newline.
        std::fs::write(&path, format!("{old}\n")).unwrap();
        assert_eq!(
            compact(&path, 90).unwrap(),
            Compaction {
                kept: 0,
                removed: 1
            }
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        assert!(query(&path, None, 0).unwrap().is_empty());
    }

    /// A retention window too large for a date (`u64::MAX` days) keeps
    /// everything instead of overflowing.
    #[test]
    fn compact_with_an_unrepresentable_window_keeps_everything() {
        let path = fresh("compact-huge");
        append(&path, &merged("org/one", 1, "1999-01-01T00:00:00Z")).unwrap();
        for days in [u64::MAX, i64::MAX as u64, 400_000_000] {
            assert_eq!(
                compact(&path, days).unwrap(),
                Compaction {
                    kept: 1,
                    removed: 0
                },
                "{days} days"
            );
        }
    }

    /// `set_review_brief` rewrites only its one line: corrupt and blank
    /// lines around it come through untouched, `since` is inclusive, the
    /// newest of several matching reviews is the one updated, and no temp
    /// file is left behind.
    #[test]
    fn set_review_brief_touches_one_line_and_keeps_the_rest_of_the_file() {
        let path = fresh("brief-rest");
        let at = |ts: &str| {
            HistoryEntry::from_effect(
                &HistoryEvent::ReviewCompleted {
                    verdict: Verdict::Approved,
                    blocking_count: 0,
                    nit_count: 0,
                },
                &rec(),
                Some("claude"),
                None,
                Vec::new(),
                ts.parse().unwrap(),
            )
        };
        let first = serde_json::to_string(&at("2026-09-01T10:00:00Z")).unwrap();
        let second = serde_json::to_string(&at("2026-09-01T11:00:00Z")).unwrap();
        std::fs::write(&path, format!("{first}\n{{corrupt\n\n{second}\n")).unwrap();

        // `since` equal to the newest review's timestamp still matches it.
        let since: DateTime<Utc> = "2026-09-01T11:00:00Z".parse().unwrap();
        assert!(set_review_brief(&path, "org/one", 7, since, Some("newest"), None).unwrap());
        let raw = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 4, "{raw}");
        assert_eq!(&lines[..3], [first.as_str(), "{corrupt", ""]);
        assert!(lines[3].contains(r#""brief":"newest""#), "{raw}");

        // A second past the newest review: nothing matches, nothing changes.
        let after = since + chrono::TimeDelta::try_seconds(1).unwrap();
        assert!(!set_review_brief(&path, "org/one", 7, after, Some("x"), None).unwrap());
        // Neither does a call with nothing to set, or a missing file.
        assert!(!set_review_brief(&path, "org/one", 7, since, None, None).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        let missing = path.with_file_name("absent.jsonl");
        assert!(!set_review_brief(&missing, "org/one", 7, since, Some("x"), None).unwrap());
        assert!(!missing.exists());

        let leftovers: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".summary."))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// An event kind this build does not know (written by a newer prx) and a
    /// line missing a required field are skipped; the rest still reads.
    #[test]
    fn unknown_event_kinds_and_incomplete_lines_are_skipped() {
        let path = fresh("unknown-kind");
        let good = serde_json::to_string(&merged("org/one", 1, "2026-09-01T00:00:00Z")).unwrap();
        let unknown = r#"{"event":"pr_reopened","ts":"2026-09-02T00:00:00Z","repo":"org/one","number":2,"title":"t","author":"a"}"#;
        let no_verdict = r#"{"event":"review_completed","ts":"2026-09-03T00:00:00Z","repo":"org/one","number":3,"title":"t","author":"a","blocking_count":0,"nit_count":0}"#;
        let no_ts = r#"{"event":"pr_merged","repo":"org/one","number":4,"title":"t","author":"a"}"#;
        std::fs::write(&path, format!("{unknown}\n{no_verdict}\n{no_ts}\n{good}\n")).unwrap();
        assert_eq!(numbers(&query(&path, None, 0).unwrap()), [1]);
    }

    /// I/O failures name the history file instead of panicking or reading
    /// as "no history".
    #[test]
    fn io_failures_are_errors_that_name_the_file() {
        let base = dir("io-errors");
        // The parent "directory" is a regular file: nothing can be created.
        let blocker = base.join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        let under_file = blocker.join("history.jsonl");
        let err = append(&under_file, &merged("org/one", 1, "2026-09-01T00:00:00Z")).unwrap_err();
        assert!(matches!(err, HistoryError::Write { .. }), "{err}");
        assert!(err.to_string().contains("blocker"), "{err}");

        // The history path is a directory: it exists but cannot be read.
        let a_dir = base.join("history.jsonl.d");
        std::fs::create_dir_all(&a_dir).unwrap();
        for err in [
            query(&a_dir, None, 0).unwrap_err(),
            compact(&a_dir, 90).unwrap_err(),
        ] {
            assert!(matches!(err, HistoryError::Read { .. }), "{err}");
            assert!(err.to_string().contains("history.jsonl.d"), "{err}");
        }
    }
}
