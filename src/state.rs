//! PR state record types (DEV-PLAN §5) and the `StateStore` with atomic
//! writes (temp file in the same directory + rename).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("failed to read state file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("state file {path} is corrupt: {message}")]
    Corrupt { path: PathBuf, message: String },
    #[error("failed to write state file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Lifecycle states (spec §2.1; DEV-PLAN §5). The spec's "New" and "Cooling
/// Down" are merged into `pending` (A5) and "Re-review Pending" is derived
/// (pending + a prior verdict).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Pending,
    InReview,
    ReviewedApproved,
    ReviewedChangesRequested,
    Skipped,
    Merged,
    Closed,
}

/// Why a PR is in the `skipped` state (audit A6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    Draft,
    Config,
    Snoozed,
    Ignored,
}

/// Review verdict from the agent manifest (DEV-PLAN §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approved,
    ChangesRequested,
    CommentOnly,
}

/// One PR's lifecycle record. Field set is exactly DEV-PLAN §5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrRecord {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub head_branch: String,
    pub head_sha: String,
    pub is_draft: bool,
    pub gh_created_at: DateTime<Utc>,
    pub state: PrState,
    /// State to restore when a snooze expires (audit A6).
    pub previous_state: Option<PrState>,
    pub skip_reason: Option<SkipReason>,
    pub snooze_until: Option<DateTime<Utc>>,
    pub snooze_until_push: bool,
    /// A5: first review anchored to gh_created_at; re-reviews anchored to
    /// new-commit detection time.
    pub review_anchor_at: Option<DateTime<Utc>>,
    pub last_reviewed_sha: Option<String>,
    pub last_review_at: Option<DateTime<Utc>>,
    pub last_verdict: Option<Verdict>,
    pub last_summary: Option<String>,
    pub blocking_count: u32,
    pub nit_count: u32,
    pub review_session: Option<String>,
    pub review_attempts: u32,
    pub diff_too_large: bool,
    /// Legacy executive summary (pre-brief, design D8). Still read so
    /// [`StateStore::load`] can migrate it into [`PrRecord::brief`] (design
    /// B5), and kept as stored so a downgrade still finds it. New code never
    /// writes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_summary: Option<String>,
    /// The PR's current brief (design B3): what the change does, in plain
    /// language. At most one per PR; regenerating keeps `created_at`.
    /// Additive and serde-defaulted (schema stays v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<Brief>,
    /// When a human Approve (`Event::Approve`, menu bar / `prx approve`)
    /// last landed this PR in `reviewed_approved` (design D11, task 8.1):
    /// tells a human-approved PR apart from an agent-approved one still
    /// awaiting the human. Cleared when the next agent review completes,
    /// when a human Block lands, and whenever the PR re-enters `pending`
    /// (new commits, failed review, unblock). Additive and serde-defaulted
    /// (schema stays v1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_approved_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

/// Who produced a brief (design B2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BriefSource {
    /// The review agent's manifest `brief` field (no extra run).
    Review,
    /// A dedicated brief run (`POST /brief`, `prx brief --regenerate`, or a
    /// review whose manifest had no brief).
    OnDemand,
}

/// A PR's brief (design B3). `stale` is derived, never stored: a brief is
/// stale when `head_sha` differs from the PR's current head.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Brief {
    pub text: String,
    /// When the PR first got a brief; kept across regenerations.
    pub created_at: DateTime<Utc>,
    /// When the text was last written.
    pub updated_at: DateTime<Utc>,
    /// The head SHA the brief describes.
    pub head_sha: String,
    pub source: BriefSource,
    /// The engine that produced it; `None` for briefs migrated from a
    /// legacy executive summary (the engine was never recorded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

impl Brief {
    /// Whether the brief describes an earlier commit than `current_head`.
    pub fn is_stale(&self, current_head: &str) -> bool {
        self.head_sha != current_head
    }
}

impl PrRecord {
    /// Design B5: turn a legacy `exec_summary` into a brief when the record
    /// has none yet. Timestamps come from the last review (else the record's
    /// `updated_at`), the head from the last reviewed SHA (else the current
    /// head), and the source is `review`. The legacy field is left in place
    /// so a downgrade keeps it. Returns whether a brief was created.
    pub fn migrate_exec_summary(&mut self) -> bool {
        if self.brief.is_some() {
            return false;
        }
        let Some(text) = self
            .exec_summary
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        else {
            return false;
        };
        let at = self.last_review_at.unwrap_or(self.updated_at);
        self.brief = Some(Brief {
            text: text.to_string(),
            created_at: at,
            updated_at: at,
            head_sha: self
                .last_reviewed_sha
                .clone()
                .unwrap_or_else(|| self.head_sha.clone()),
            source: BriefSource::Review,
            engine: None,
        });
        true
    }

    /// State-file key: `owner/repo#<number>` (DEV-PLAN §5).
    pub fn key_for(repo: &str, number: u64) -> String {
        format!("{repo}#{number}")
    }

    pub fn key(&self) -> String {
        PrRecord::key_for(&self.repo, self.number)
    }

    /// A prior verdict exists, so a `pending` record means "re-review pending".
    pub fn is_rereview_pending(&self) -> bool {
        self.state == PrState::Pending && self.last_verdict.is_some()
    }
}

/// Top-level state file (schema_version 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateFile {
    pub schema_version: u32,
    /// Keyed by `owner/repo#<number>`.
    pub prs: BTreeMap<String, PrRecord>,
    pub last_poll: Option<DateTime<Utc>>,
    /// Design D5: daemon-wide review pause — automatic enqueue stops until
    /// this instant. Additive and serde-defaulted (schema stays v1; older
    /// binaries ignore it). A past instant reads as "not paused".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<DateTime<Utc>>,
}

impl StateFile {
    /// D5: whether reviews are paused at `now` (the pause expires by time).
    pub fn is_paused(&self, now: DateTime<Utc>) -> bool {
        self.active_pause(now).is_some()
    }

    /// D5: the pause end when a pause is in effect at `now`.
    pub fn active_pause(&self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        self.paused_until.filter(|until| now < *until)
    }
}

impl Default for StateFile {
    fn default() -> Self {
        StateFile {
            schema_version: SCHEMA_VERSION,
            prs: BTreeMap::new(),
            last_poll: None,
            paused_until: None,
        }
    }
}

/// Loads and atomically saves `state.json`.
#[derive(Debug, Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    pub fn new(path: PathBuf) -> Self {
        StateStore { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the state file. A missing file yields an empty default state;
    /// a corrupt file is an error (never silently wipe tracked PRs).
    pub fn load(&self) -> Result<StateFile, StateError> {
        if !self.path.exists() {
            return Ok(StateFile::default());
        }
        let raw = std::fs::read_to_string(&self.path).map_err(|source| StateError::Io {
            path: self.path.clone(),
            source,
        })?;
        let mut state: StateFile = serde_json::from_str(&raw).map_err(|e| StateError::Corrupt {
            path: self.path.clone(),
            message: e.to_string(),
        })?;
        // Design B5: legacy executive summaries become briefs on load.
        for rec in state.prs.values_mut() {
            rec.migrate_exec_summary();
        }
        Ok(state)
    }

    /// Save atomically: write a temp file in the same directory, fsync, then
    /// rename over the target so readers never observe a partial write.
    pub fn save(&self, state: &StateFile) -> Result<(), StateError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| StateError::Write {
                path: self.path.clone(),
                source,
            })?;
        }
        let temp = self.temp_path();
        let raw = serde_json::to_string_pretty(state).map_err(|e| StateError::Corrupt {
            path: self.path.clone(),
            message: format!("serialization failed: {e}"),
        })?;
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&temp).map_err(|source| StateError::Write {
                path: temp.clone(),
                source,
            })?;
            f.write_all(raw.as_bytes())
                .and_then(|()| f.sync_all())
                .map_err(|source| StateError::Write {
                    path: temp.clone(),
                    source,
                })?;
        }
        std::fs::rename(&temp, &self.path).map_err(|source| {
            let _ = std::fs::remove_file(&temp);
            StateError::Write {
                path: self.path.clone(),
                source,
            }
        })
    }

    fn temp_path(&self) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let name = self
            .path
            .file_name()
            .map(std::ffi::OsStr::to_string_lossy)
            .unwrap_or_else(|| "state.json".into());
        self.path
            .with_file_name(format!(".{name}.tmp.{}.{}", std::process::id(), nanos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{PrState, StateFile};

    fn sample_record(repo: &str, number: u64) -> PrRecord {
        PrRecord {
            repo: repo.to_string(),
            number,
            title: "t".into(),
            author: "a".into(),
            head_branch: "b".into(),
            head_sha: "sha".into(),
            is_draft: false,
            gh_created_at: Utc::now(),
            state: PrState::Pending,
            previous_state: None,
            skip_reason: None,
            snooze_until: None,
            snooze_until_push: false,
            review_anchor_at: Some(Utc::now()),
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

    #[test]
    fn key_format_matches_dev_plan() {
        assert_eq!(PrRecord::key_for("org/repo", 42), "org/repo#42");
    }

    #[test]
    fn load_missing_file_yields_empty_state() {
        let dir = std::env::temp_dir().join(format!("prx-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let store = StateStore::new(dir.join("missing-state.json"));
        let state = store.load().unwrap();
        assert_eq!(state.schema_version, SCHEMA_VERSION);
        assert!(state.prs.is_empty());
        assert!(state.last_poll.is_none());
    }

    #[test]
    fn save_load_roundtrip_is_atomic_and_deterministic() {
        let dir = std::env::temp_dir().join(format!("prx-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state-rt.json");
        let store = StateStore::new(path.clone());

        let mut state = StateFile::default();
        let rec = sample_record("org/repo", 7);
        state.prs.insert(rec.key(), rec);
        state.last_poll = Some(Utc::now());
        store.save(&state).unwrap();

        // No temp leftovers in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );

        let loaded = store.load().unwrap();
        assert_eq!(loaded, state);

        // Overwrite works (rename replaces the existing file).
        store.save(&StateFile::default()).unwrap();
        assert_eq!(store.load().unwrap(), StateFile::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_state_file_is_an_error_not_empty() {
        let dir = std::env::temp_dir().join(format!("prx-state-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state-corrupt.json");
        std::fs::write(&path, "{not json at all").unwrap();
        let store = StateStore::new(path.clone());
        assert!(store.load().is_err());
        let _ = std::fs::remove_file(&path);
    }

    /// D5: `paused_until` is additive — a v1 file without it loads as not
    /// paused, an unpaused state omits it, and a set value round-trips.
    #[test]
    fn paused_until_is_additive_and_schema_stays_v1() {
        let old: StateFile =
            serde_json::from_str(r#"{"schema_version":1,"prs":{},"last_poll":null}"#).unwrap();
        assert_eq!(old.paused_until, None);
        let json = serde_json::to_string(&StateFile::default()).unwrap();
        assert!(!json.contains("paused_until"), "{json}");

        let until: DateTime<Utc> = "2026-10-07T13:00:00Z".parse().unwrap();
        let paused = StateFile {
            paused_until: Some(until),
            ..StateFile::default()
        };
        let back: StateFile =
            serde_json::from_str(&serde_json::to_string(&paused).unwrap()).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.paused_until, Some(until));
        assert!(back.is_paused("2026-10-07T12:59:00Z".parse().unwrap()));
        assert!(!back.is_paused(until));
    }

    /// Design B5: a stored legacy `exec_summary` loads as a brief stamped
    /// with the last review time and SHA, source `review`; the legacy field
    /// is kept for downgrades and an existing brief is never overwritten.
    #[test]
    fn legacy_exec_summary_migrates_to_brief_on_load() {
        // Own directory: the round-trip test asserts no temp files linger
        // in the shared one while it runs.
        let dir = std::env::temp_dir().join(format!("prx-state-brief-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("state-brief-migrate.json");
        let reviewed: DateTime<Utc> = "2026-10-01T10:00:00Z".parse().unwrap();
        let mut rec = sample_record("org/repo", 9);
        rec.exec_summary = Some("Adds rate limiting.".into());
        rec.last_review_at = Some(reviewed);
        rec.last_reviewed_sha = Some("abc123".into());
        rec.head_sha = "def456".into();
        let mut kept = sample_record("org/repo", 10);
        kept.exec_summary = Some("old text".into());
        let existing = Brief {
            text: "newer brief".into(),
            created_at: reviewed,
            updated_at: reviewed,
            head_sha: "sha".into(),
            source: BriefSource::OnDemand,
            engine: Some("pi".into()),
        };
        kept.brief = Some(existing.clone());
        let mut state = StateFile::default();
        state.prs.insert(rec.key(), rec.clone());
        state.prs.insert(kept.key(), kept.clone());
        let store = StateStore::new(path.clone());
        store.save(&state).unwrap();

        let loaded = store.load().unwrap();
        let brief = loaded.prs[&rec.key()].brief.clone().unwrap();
        assert_eq!(brief.text, "Adds rate limiting.");
        assert_eq!(brief.created_at, reviewed);
        assert_eq!(brief.updated_at, reviewed);
        assert_eq!(brief.head_sha, "abc123");
        assert_eq!(brief.source, BriefSource::Review);
        assert_eq!(brief.engine, None);
        assert!(brief.is_stale("def456"));
        assert!(!brief.is_stale("abc123"));
        assert_eq!(
            loaded.prs[&rec.key()].exec_summary.as_deref(),
            Some("Adds rate limiting.")
        );
        assert_eq!(loaded.prs[&kept.key()].brief, Some(existing));

        // No review time: falls back to the record's updated_at and head.
        let mut bare = sample_record("org/repo", 11);
        bare.exec_summary = Some("x".into());
        assert!(bare.migrate_exec_summary());
        let b = bare.brief.clone().unwrap();
        assert_eq!(b.created_at, bare.updated_at);
        assert_eq!(b.head_sha, bare.head_sha);
        assert!(!bare.migrate_exec_summary());
        let _ = std::fs::remove_file(&path);
    }

    /// A brief round-trips through the state file; a record without one
    /// omits the key.
    #[test]
    fn brief_round_trips_and_is_additive() {
        let at: DateTime<Utc> = "2026-10-02T14:00:00Z".parse().unwrap();
        let mut rec = sample_record("org/repo", 3);
        let json = serde_json::to_string(&rec).unwrap();
        assert!(!json.contains("\"brief\""), "{json}");
        rec.brief = Some(Brief {
            text: "Adds a cache.".into(),
            created_at: at,
            updated_at: at,
            head_sha: "sha".into(),
            source: BriefSource::OnDemand,
            engine: Some("claude".into()),
        });
        let json = serde_json::to_string(&rec).unwrap();
        assert!(json.contains("\"source\":\"on_demand\""), "{json}");
        let back: PrRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec);
    }
}
