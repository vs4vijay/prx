//! Phase 4 CLI surfaces: `prx insights` and the polished `prx history`
//! timeline/JSON — in-process fallback and the daemon-backed path — computed
//! from real pipeline runs plus handcrafted history entries (P4 acceptance:
//! "prx history/insights correct math on fixtures").

mod common;

use std::path::PathBuf;
use std::process::Output;
use std::time::Duration;

use chrono::{TimeDelta, Utc};
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

    fn history_path(&self) -> PathBuf {
        self.data_dir.child("history.jsonl")
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

const OLD: &str = "2026-09-01T10:00:00Z"; // long past any cooldown

fn approved_manifest() -> String {
    manifest_json(
        "approved",
        &[&finding_json(Some("src/a.rs"), Some(3), "nit", "tweak")],
    )
}

/// A changes-requested manifest with two blocking findings whose bodies feed
/// the A18 blocking-text history.
fn blocking_manifest() -> String {
    manifest_json(
        "changes_requested",
        &[
            &finding_json(
                Some("src/a.rs"),
                Some(12),
                "blocking",
                "missing tenant scope",
            ),
            &finding_json(
                Some("src/b.rs"),
                Some(4),
                "blocking",
                "no tests for the new path",
            ),
        ],
    )
}

/// Append a handcrafted history entry (used for merge/terminal fixtures).
fn append_entry(env: &Env, entry: &prx::history::HistoryEntry) {
    prx::history::append(&env.history_path(), entry).unwrap();
}

/// A 40-day-old changes-requested review (window-filter fixture).
fn old_review_entry(repo: &str, number: u64, author: &str) -> prx::history::HistoryEntry {
    use prx::machine::HistoryEvent;
    let ts = Utc::now() - TimeDelta::try_days(40).unwrap();
    let rec = prx::state::PrRecord {
        repo: repo.into(),
        number,
        title: "old".into(),
        author: author.into(),
        head_branch: "f/x".into(),
        head_sha: "aaaa".into(),
        is_draft: false,
        gh_created_at: ts,
        state: prx::state::PrState::Pending,
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
        updated_at: ts,
    };
    prx::history::HistoryEntry::from_effect(
        &HistoryEvent::ReviewCompleted {
            verdict: prx::state::Verdict::ChangesRequested,
            blocking_count: 1,
            nit_count: 0,
        },
        &rec,
        Some("fake"),
        None,
        vec!["ancient blocking issue".to_string()],
        ts,
    )
}

fn merged_entry(
    repo: &str,
    number: u64,
    author: &str,
    ts: chrono::DateTime<Utc>,
) -> prx::history::HistoryEntry {
    use prx::machine::HistoryEvent;
    let rec = prx::state::PrRecord {
        repo: repo.into(),
        number,
        title: "t".into(),
        author: author.into(),
        head_branch: "f/x".into(),
        head_sha: "aaaa".into(),
        is_draft: false,
        gh_created_at: ts,
        state: prx::state::PrState::Merged,
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
        updated_at: ts,
    };
    prx::history::HistoryEntry::from_effect(
        &HistoryEvent::PrMerged,
        &rec,
        None,
        None,
        Vec::new(),
        ts,
    )
}

#[test]
fn history_timeline_json_and_insights_in_process() {
    let mut env = Env::new("ins-cli");
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[
            pr_json(1, "Add feature", "alice", "f/x", "aaaa", false, OLD),
            pr_json(2, "Fix bug", "bob", "f/y", "bbbb", false, OLD),
        ],
    );

    // Two real pipeline runs: PR 1 blocked, PR 2 approved.
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let out = env.run(
        &["review", "org/one#2"],
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    // PR 1's issues were fixed (approved re-review) and it merged right
    // after, so review→merge is measured in seconds-to-minutes.
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    append_entry(&env, &merged_entry("org/one", 1, "alice", Utc::now()));

    // An old (40-day-old) review by another author: all-time stats see it,
    // the 30-day window does not.
    append_entry(&env, &old_review_entry("org/two", 9, "carol"));

    // --- history timeline: icons, verdicts, counts, titles; no raw JSON.
    let out = env.run(&["history", "--limit", "10"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("recent review history"), "{stdout}");
    assert!(stdout.contains("org/one#1"), "{stdout}");
    assert!(stdout.contains("changes requested"), "{stdout}");
    assert!(stdout.contains("2 blocking"), "{stdout}");
    assert!(stdout.contains("approved"), "{stdout}");
    assert!(stdout.contains("merged"), "{stdout}");
    assert!(stdout.contains('\u{26a0}'), "CR icon missing: {stdout}");
    assert!(
        stdout.contains('\u{2705}'),
        "approved icon missing: {stdout}"
    );
    assert!(stdout.contains('\u{2714}'), "merged icon missing: {stdout}");
    assert!(
        !stdout.contains('{'),
        "timeline must not print raw JSON: {stdout}"
    );

    // --- history --json: machine-readable, enriched entries round-trip.
    let out = env.run(&["history", "--json", "--limit", "10"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let parsed: Vec<prx::history::HistoryEntry> =
        serde_json::from_str(stdout_of(&out).trim()).expect("history --json must be a JSON array");
    // CR review, approval, re-review (each preceded by its review_started
    // entry, design D6), merge, old fixture.
    assert_eq!(parsed.len(), 8);
    let started = parsed
        .iter()
        .filter(|e| matches!(e, prx::history::HistoryEntry::ReviewStarted { .. }))
        .count();
    assert_eq!(started, 3);
    let blocked = parsed
        .iter()
        .find_map(|e| match e {
            prx::history::HistoryEntry::ReviewCompleted { blocking_texts, .. }
                if blocking_texts.contains(&"missing tenant scope".to_string()) =>
            {
                Some(blocking_texts.clone())
            }
            _ => None,
        })
        .expect("the blocked review is in the JSON output");
    assert_eq!(blocked.len(), 2, "{blocked:?}");

    // --- insights: totals, effectiveness, merge latency, per-author.
    let out = env.run(&["insights"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("prx insights"), "{stdout}");
    assert!(stdout.contains("total: 4"), "{stdout}");
    assert!(stdout.contains("last 7 days: 3"), "{stdout}");
    assert!(stdout.contains("missing tenant scope"), "{stdout}");
    assert!(
        stdout.contains("1 of 2 changes-requested review(s)"),
        "the pipeline CR was fixed by the re-review; carol's old CR was not:\n{stdout}"
    );
    assert!(stdout.contains("1 PR(s)"), "{stdout}");
    assert!(stdout.contains("from last review to merge"), "{stdout}");
    assert!(stdout.contains("alice"), "{stdout}");
    assert!(stdout.contains("bob"), "{stdout}");
    assert!(stdout.contains("carol"), "{stdout}");

    // --- filters: --days narrows the window, --repo narrows the scope.
    let out = env.run(&["insights", "--days", "30"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("last 30 days ·"), "{stdout}");
    assert!(
        stdout.contains("total: 3"),
        "carol's old review is out: {stdout}"
    );
    assert!(!stdout.contains("carol"), "{stdout}");
    let out = env.run(&["insights", "--repo", "org/two"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("total: 1"), "{stdout}");
    assert!(!stdout.contains("alice"), "{stdout}");
    // --days 0 is rejected with a clear error.
    let out = env.run(&["insights", "--days", "0"], &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("--days must be at least 1"));
    env.mark_stopped();
}

#[test]
fn history_and_insights_round_trip_through_the_daemon() {
    let mut env = Env::new("ins-daemon");
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

    let out = env.run(
        &["daemon", "start"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let entries = prx::history::query(&env.history_path(), None, 0).unwrap_or_default();
        // The run's `review_started` entry lands first (design D6).
        if entries
            .iter()
            .any(|e| matches!(e, prx::history::HistoryEntry::ReviewCompleted { .. }))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Daemon-backed: NO in-process fallback warning on stderr.
    let out = env.run(&["history", "--limit", "5"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("daemon not running"),
        "history must be served by the daemon"
    );
    assert!(stdout_of(&out).contains("org/one#1"));

    let out = env.run(&["insights"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("daemon not running"),
        "insights must be served by the daemon"
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("total: 1"), "{stdout}");
    assert!(stdout.contains("missing tenant scope"), "{stdout}");

    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_stopped();
}
