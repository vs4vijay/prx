//! Hardening pass (DEV-PLAN §5 "self-healing", spec §11): agent crashes
//! mid-review retry once and leave state consistent; corrupt data files
//! fail loudly (state) or degrade gracefully (history); and 90-day history
//! compaction runs on daemon start (A17).

mod common;

use std::path::PathBuf;
use std::process::Output;

use common::*;

/// One test environment: temp config/data/gh dirs + fake agent on PATH.
struct Env {
    #[allow(dead_code)]
    cfg_dir: TempDir,
    data_dir: TempDir,
    gh_dir: TempDir,
    #[allow(dead_code)]
    agent_dir: TempDir,
    config: PathBuf,
    stopped: bool,
}

impl Env {
    fn new(tag: &str) -> Self {
        let cfg_dir = TempDir::new(&format!("{tag}-cfg"));
        let data_dir = TempDir::new(&format!("{tag}-data"));
        let gh_dir = TempDir::new(&format!("{tag}-gh"));
        let agent_dir = TempDir::new(&format!("{tag}-agent"));
        install_fake_gh(gh_dir.path());
        let agent_path = install_fake_agent(agent_dir.path());
        let config = cfg_dir.path().join("config.yml");
        write_file(
            &config,
            &config_with_fake_agent(&["org/one"], &agent_path, ""),
        );
        Env {
            cfg_dir,
            data_dir,
            gh_dir,
            agent_dir,
            config,
            stopped: true,
        }
    }

    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        run_cli(
            &self.config,
            self.data_dir.path(),
            args,
            Some(self.gh_dir.path()),
            extra_env,
        )
    }

    fn mark_running(&mut self) {
        self.stopped = false;
    }

    fn mark_stopped(&mut self) {
        self.stopped = true;
    }

    /// Loads state.json — a panic here IS the assertion that the file
    /// still parses (no partial/corrupt writes).
    fn state(&self) -> prx::state::StateFile {
        prx::state::StateStore::new(self.data_dir.child("state.json"))
            .load()
            .expect("state.json should parse")
    }

    /// Outcome history: without the per-run `review_started` markers (D6).
    fn history(&self) -> Vec<prx::history::HistoryEntry> {
        prx::history::query(&self.data_dir.child("history.jsonl"), None, 0)
            .unwrap()
            .into_iter()
            .filter(|e| !matches!(e, prx::history::HistoryEntry::ReviewStarted { .. }))
            .collect()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if !self.stopped {
            let _ = run_cli(
                &self.config,
                self.data_dir.path(),
                &["daemon", "stop"],
                None,
                &[],
            );
        }
    }
}

const OLD: &str = "2026-09-01T10:00:00Z";

fn approved_manifest() -> String {
    manifest_json(
        "approved",
        &[&finding_json(Some("src/a.rs"), Some(3), "nit", "tweak")],
    )
}

// ------------------------------------------------- agent crash mid-review

/// The agent writes a manifest but exits non-zero (crash after producing its
/// artifact, or before — prx cannot tell). DEV-PLAN §7: retry once, then the
/// ReviewFailed path. The state file must stay parseable and the record must
/// end reviewable (pending), never stuck in_review.
#[test]
fn agent_exiting_nonzero_retries_once_and_fails_cleanly() {
    let env = Env::new("hd-crash");
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "Add feature",
            "alice",
            "f/x",
            "aaaa",
            false,
            OLD,
        )],
    );
    // Baseline: the PR is tracked first, so the crash happens on a
    // well-formed record.
    let out = env.run(&["status"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    // The agent writes a valid manifest but crashes (exit 1) — twice (retry).
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_EXIT", "1"),
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
        ],
    );
    assert!(!out.status.success(), "a failed review must fail the CLI");
    assert!(
        stderr_of(&out).contains("retrying once"),
        "exactly one retry: {}",
        stderr_of(&out)
    );

    // State consistent: parses, pending (not stuck in_review), one failed
    // run recorded.
    let state = env.state();
    let rec = &state.prs["org/one#1"];
    assert_eq!(rec.state, prx::state::PrState::Pending);
    assert_eq!(rec.review_attempts, 1);
    assert_eq!(rec.last_verdict, None);

    // History recorded the failure.
    let history = env.history();
    assert_eq!(history.len(), 1);
    assert!(matches!(
        history[0],
        prx::history::HistoryEntry::ReviewFailed { .. }
    ));

    // And the PR is immediately reviewable again (no reset needed).
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, prx::state::PrState::ReviewedApproved);
}

// ------------------------------------------------- corrupt data files

/// A corrupt state file must fail loudly — never silently wipe tracked PRs —
/// and stay untouched on disk.
#[test]
fn corrupt_state_file_fails_loudly_and_is_never_wiped() {
    let env = Env::new("hd-state");
    let corrupt = "{\"schema_version\": 1, \"prs\": { \"org/one#1\": { BROKEN";
    write_file(&env.data_dir.child("state.json"), corrupt);

    let out = env.run(&["status"], &[]);
    assert!(!out.status.success(), "status must not swallow corruption");
    assert!(
        stderr_of(&out).contains("failed to load PR state") || stderr_of(&out).contains("corrupt"),
        "{}",
        stderr_of(&out)
    );
    let out = env.run(&["review", "org/one#1"], &[]);
    assert!(!out.status.success());
    // The file is untouched: the user can still recover it by hand.
    assert_eq!(
        std::fs::read_to_string(env.data_dir.child("state.json")).unwrap(),
        corrupt
    );
}

/// A corrupt history line must not hide the rest of the history: the CLI and
/// the daemon's endpoints skip it and serve the good entries.
#[test]
fn corrupt_history_line_is_skipped_by_cli_and_daemon() {
    let mut env = Env::new("hd-history");
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);

    // One good entry, one corrupt line (truncated mid-JSON), one blank line.
    let good = r#"{"event":"review_completed","ts":"2026-09-10T10:00:00Z","repo":"org/one","number":1,"title":"t","author":"alice","verdict":"changes_requested","blocking_count":1,"nit_count":0}"#;
    write_file(
        &env.data_dir.child("history.jsonl"),
        &format!("{good}\n{{\"event\":\"review_comp\n\n"),
    );

    // In-process CLI: only the good entry survives.
    let out = env.run(&["history", "--json", "--limit", "10"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let parsed: Vec<prx::history::HistoryEntry> =
        serde_json::from_str(stdout_of(&out).trim()).expect("valid JSON array");
    assert_eq!(parsed.len(), 1);
    let out = env.run(&["insights"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("total: 1"), "{}", stdout_of(&out));

    // Through the daemon: same graceful behavior on both endpoints.
    let out = env.run(&["daemon", "start"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    let out = env.run(&["history", "--limit", "10"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("org/one#1"));
    let out = env.run(&["insights"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("total: 1"));
    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success());
    env.mark_stopped();
}

// ------------------------------------------------- retention (A17)

/// 90-day compaction runs on daemon start (A17): entries older than the
/// window and unparseable lines are dropped atomically; recent entries and
/// everything around them keep working.
#[test]
fn daemon_start_compacts_history_beyond_90_days() {
    let mut env = Env::new("hd-compact");
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);

    let history_path = env.data_dir.child("history.jsonl");
    let old_ts = "2026-01-01T00:00:00Z"; // >90 days before the test date
    let recent = format!(
        r#"{{"event":"review_completed","ts":"{}","repo":"org/one","number":2,"title":"fresh","author":"bob","verdict":"approved","blocking_count":0,"nit_count":1}}"#,
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    write_file(
        &history_path,
        &format!(
            "{{\"event\":\"pr_merged\",\"ts\":\"{old_ts}\",\"repo\":\"org/one\",\"number\":1,\"title\":\"old\",\"author\":\"alice\"}}\n\
             {{\"event\":\"review_completed\",\"ts\":\"{old_ts}\",\"repo\":\"org/one\",\"number\":3,\"title\":\"old2\",\"author\":\"alice\",\"verdict\":\"approved\",\"blocking_count\":0,\"nit_count\":0}}\n\
             {recent}\n{{CORRUPT-LINE-should-be-dropped\n"
        ),
    );

    // The daemon start performs the compaction synchronously (A17).
    let out = env.run(&["daemon", "start"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();

    let raw = std::fs::read_to_string(&history_path).unwrap();
    assert!(!raw.contains("old"), "old entries must be gone: {raw}");
    assert!(
        !raw.contains("CORRUPT-LINE"),
        "unparseable lines must be dropped: {raw}"
    );
    assert!(raw.contains("fresh"), "recent entry must survive: {raw}");
    let entries = env.history();
    assert_eq!(entries.len(), 1);

    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success());
    env.mark_stopped();
}
