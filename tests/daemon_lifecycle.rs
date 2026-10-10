//! Phase 3 daemon lifecycle through the real CLI: detached start (A16 pid
//! guard), stop (SIGTERM), status, logs (A15), and the CLI thin-client
//! round trip through the IPC API (A4) with the in-process fallback when
//! the daemon is absent.
//!
//! These tests spawn real `prx` processes (the detached daemon is a
//! grandchild that survives the CLI via process_group(0)), so every test
//! installs a Drop guard that stops a still-running daemon before the temp
//! dirs vanish.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use chrono::Utc;

use common::*;

/// One test environment: temp config/data/gh dirs + fake agent on PATH.
/// The cfg/agent dir fields are never read but must be kept alive: dropping
/// them deletes the directories the daemons and CLI runs use.
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
    fn new(tag: &str, due_now: bool) -> Self {
        let cfg_dir = TempDir::new(&format!("{tag}-cfg"));
        let data_dir = TempDir::new(&format!("{tag}-data"));
        let gh_dir = TempDir::new(&format!("{tag}-gh"));
        let agent_dir = TempDir::new(&format!("{tag}-agent"));
        install_fake_gh(gh_dir.path());
        let agent_path = install_fake_agent(agent_dir.path());
        // due_now=false keeps the daemon from auto-reviewing the fixture PR:
        // the first-review cooldown is anchored to the gh created_at (A5),
        // so the PR is created "now" and sits inside the default 60-minute
        // cooldown. Tests that want reviews pass true (zero cooldown) or pin
        // their own (old) fixtures.
        let created_at = if due_now {
            OLD.to_string()
        } else {
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
        };
        let cooldown = if due_now {
            "  cooldown:\n    before_first_review_minutes: 0\n    between_re_reviews_minutes: 0\n"
        } else {
            ""
        };
        let config = cfg_dir.path().join("config.yml");
        write_file(
            &config,
            &config_with_fake_agent(&["org/one"], &agent_path, cooldown),
        );
        write_list_fixture(
            gh_dir.path(),
            "org/one",
            &[pr_json(
                1,
                "Add feature",
                "alice",
                "f/x",
                "aaaa",
                false,
                &created_at,
            )],
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

    fn state(&self) -> prx::state::StateFile {
        prx::state::StateStore::new(self.data_dir.child("state.json"))
            .load()
            .expect("state.json should parse")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Insurance: a test that failed mid-way must not leave a detached
        // daemon running against a deleted temp dir.
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

fn wait_for_file(path: &Path, timeout: Duration, want: bool, what: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what} at {}", path.display());
}

/// A canned approved manifest for the fake agent.
fn agent_manifest_env() -> &'static [(&'static str, &'static str)] {
    &[(
        "FAKE_AGENT_MANIFEST_JSON",
        r#"{"manifest_version": 1, "verdict": "approved", "summary": "ok",
  "findings": [{"file": "src/a.rs", "line": 3, "severity": "nit", "body": "tweak"}],
  "duration_seconds": 1}"#,
    )]
}

#[test]
fn daemon_start_status_stop_roundtrip() {
    let mut env = Env::new("lc-roundtrip", false);

    // start (detached): pid + port + token files appear.
    let out = env.run(&["daemon", "start"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("daemon started (pid"), "{stdout}");
    env.mark_running();

    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        true,
        "port file",
    );
    wait_for_file(
        &env.data_dir.child("daemon.token"),
        Duration::from_secs(10),
        true,
        "token file",
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        env.data_dir
            .child("daemon.token")
            .metadata()
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "token file must be 0600 (A4)"
    );

    // status: running, with the IPC endpoint.
    let out = env.run(&["daemon", "status"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("daemon running (pid"),
        "{}",
        stdout_of(&out)
    );

    // The daemon wrote a log file for today (A15) and `daemon logs` reads it.
    let logs = prx::daemon::newest_log_file(&env.data_dir.child("logs"));
    assert!(logs.is_some(), "daemon should create logs/prx.log.<date>");
    let out = env.run(&["daemon", "logs"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    // stop: SIGTERM, clean shutdown, files removed.
    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("daemon stopped"),
        "{}",
        stdout_of(&out)
    );
    env.mark_stopped();
    wait_for_file(
        &env.data_dir.child("daemon.pid"),
        Duration::from_secs(10),
        false,
        "pid file removal",
    );
    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        false,
        "port file removal",
    );

    let out = env.run(&["daemon", "status"], &[]);
    assert!(
        stdout_of(&out).contains("daemon not running"),
        "{}",
        stdout_of(&out)
    );
    // Stopping twice is fine.
    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("daemon not running"));
}

#[test]
fn daemon_start_is_idempotent_and_single_instance() {
    let mut env = Env::new("lc-single", false);
    let out = env.run(&["daemon", "start"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        true,
        "port file",
    );

    // A16: starting again does not spawn a second daemon.
    let out = env.run(&["daemon", "start"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("already running"),
        "{}",
        stdout_of(&out)
    );

    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_stopped();
}

#[test]
fn daemon_start_surfaces_a_bad_config() {
    let mut env = Env::new("lc-badcfg", false);
    // Unknown agent: the child fails config validation before claiming a pid.
    write_file(
        &env.config,
        "defaults:\n  agent: nope\nrepos:\n  - name: org/one\n",
    );
    let out = env.run(&["daemon", "start"], &[]);
    assert!(
        !out.status.success(),
        "start must fail on an invalid config"
    );
    assert!(
        stderr_of(&out).contains("did not claim its pid file"),
        "{}",
        stderr_of(&out)
    );
    env.mark_stopped();
    let out = env.run(&["daemon", "status"], &[]);
    assert!(stdout_of(&out).contains("daemon not running"));
}

#[test]
fn cli_review_falls_back_in_process_without_daemon() {
    let env = Env::new("lc-fallback", false);
    let out = env.run(&["review", "org/one#1"], agent_manifest_env());
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("daemon not running — running in-process"),
        "{}",
        stderr_of(&out)
    );
    // The pipeline really ran in-process.
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, prx::state::PrState::ReviewedApproved);
}

#[test]
fn cli_commands_round_trip_through_the_daemon() {
    let mut env = Env::new("lc-ipc", false);

    // Start the daemon: with the default cooldown the fixture PR is
    // discovered but NOT auto-reviewed, so the CLI commands below drive it.
    let out = env.run(&["daemon", "start"], agent_manifest_env());
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        true,
        "port file",
    );
    wait_for(
        || env.state().prs.contains_key("org/one#1"),
        Duration::from_secs(10),
        "startup poll to discover the PR",
    );

    // status --json goes through the daemon: valid JSON, and NO gh poll was
    // triggered by the CLI itself (the daemon serves its cached state).
    let calls_before = gh_call_count(&env);
    let out = env.run(&["status", "--json"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let parsed: serde_json::Value =
        serde_json::from_str(stdout_of(&out).trim()).expect("status --json must emit a JSON array");
    assert_eq!(parsed[0]["key"], "org/one#1");
    assert_eq!(
        gh_call_count(&env),
        calls_before,
        "thin-client status must not poll gh (A4)"
    );

    // review: the CLI enqueues; the daemon executes it (force bypasses the
    // cooldown for an explicit target, A22).
    let out = env.run(&["review", "org/one#1"], agent_manifest_env());
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("queued org/one#1 for review"),
        "{}",
        stdout_of(&out)
    );
    wait_for(
        || {
            env.state()
                .prs
                .get("org/one#1")
                .is_some_and(|r| r.last_review_at.is_some())
        },
        Duration::from_secs(15),
        "the daemon to run the enqueued review",
    );

    // snooze / ignore / unignore mutate through the daemon.
    let out = env.run(&["snooze", "org/one#1", "--hours", "2"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("snoozed"), "{}", stdout_of(&out));
    assert_eq!(
        env.state().prs["org/one#1"].skip_reason,
        Some(prx::state::SkipReason::Snoozed)
    );

    let out = env.run(&["ignore", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        env.state().prs["org/one#1"].skip_reason,
        Some(prx::state::SkipReason::Ignored)
    );

    let out = env.run(&["unignore", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        env.state().prs["org/one#1"].state,
        prx::state::PrState::Pending
    );

    // approve goes through the daemon (D2: the only GitHub approval path).
    let out = env.run(&["approve", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("approved org/one#1"),
        "{}",
        stdout_of(&out)
    );

    // history is served by the daemon (timeline format with the verdict icon).
    let out = env.run(&["history", "--limit", "10"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("recent review history"), "{stdout}");
    assert!(stdout.contains("org/one#1"), "{stdout}");
    assert!(stdout.contains("approved"), "{stdout}");
    assert!(
        stdout.contains('\u{2705}'),
        "verdict icon missing: {stdout}"
    );

    // review-now triggers a daemon-side poll + due-scan.
    let out = env.run(&["review-now"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("triggered poll + due-scan"),
        "{}",
        stdout_of(&out)
    );

    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_stopped();
}

#[test]
fn daemon_status_and_stop_without_a_daemon() {
    let env = Env::new("lc-none", false);
    let out = env.run(&["daemon", "status"], &[]);
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("daemon not running"));
    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("daemon not running"));
}

/// Hardening: the daemon is SIGKILLed while a review is running. The
/// pipeline persists state only at completion and `ReviewStart` stays
/// in-memory, so state.json must still parse and the record must NOT be
/// stuck in `in_review` — a fresh review must start cleanly afterwards.
#[test]
fn daemon_sigkill_during_review_leaves_state_consistent_and_reviewable() {
    let mut env = Env::new("lc-kill", false);
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
    let prompt_out = env.data_dir.child("agent-prompt.txt");

    // Start the daemon with a very slow agent so the review is in flight.
    let out = env.run(
        &["daemon", "start"],
        &[
            ("FAKE_AGENT_SLEEP", "30"),
            ("FAKE_AGENT_PROMPT_OUT", prompt_out.to_str().unwrap()),
            ("FAKE_AGENT_MANIFEST_JSON", agent_manifest_env()[0].1),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        true,
        "port file",
    );
    wait_for(
        || env.state().prs.contains_key("org/one#1"),
        Duration::from_secs(10),
        "startup poll to discover the PR",
    );

    // Kick off a forced review and wait until the agent is actually running.
    let out = env.run(&["review", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    wait_for_file(&prompt_out, Duration::from_secs(15), true, "agent to start");

    // SIGKILL the daemon mid-review (no graceful shutdown, no cleanup).
    let pid = prx::daemon::read_pid(&env.data_dir.child("daemon.pid"))
        .expect("pid file must name the daemon");
    send_sigkill(pid);
    wait_for(
        || {
            !prx::daemon::read_pid(&env.data_dir.child("daemon.pid"))
                .map(|p| !process_alive_(p))
                .unwrap_or(true)
        },
        Duration::from_secs(10),
        "the daemon process to die",
    );

    // state.json still parses, and the record is NOT stuck in_review:
    // ReviewStart was applied in memory only, and no completion was written.
    let state = env.state();
    let rec = &state.prs["org/one#1"];
    assert_eq!(
        rec.state,
        prx::state::PrState::Pending,
        "a killed review must leave the record reviewable"
    );
    assert_eq!(rec.last_verdict, None);
    // Only the run's review_started marker (design D6) may be there.
    assert!(
        prx::history::query(&env.data_dir.child("history.jsonl"), None, 0)
            .unwrap()
            .iter()
            .all(|e| matches!(e, prx::history::HistoryEntry::ReviewStarted { .. })),
        "no review_completed may be recorded for the killed run"
    );

    // A fresh review (in-process — the daemon is gone) succeeds immediately.
    let out = env.run(&["review", "org/one#1"], agent_manifest_env());
    assert!(
        out.status.success(),
        "the PR must be reviewable after the crash: {}",
        stderr_of(&out)
    );
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, prx::state::PrState::ReviewedApproved);
    env.mark_stopped();
}

#[cfg(unix)]
fn send_sigkill(pid: u32) {
    // SAFETY: plain kill(2) with a constant signal number (test-only).
    let rc = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    assert_eq!(rc, 0, "SIGKILL of pid {pid} failed");
}

#[cfg(unix)]
fn process_alive_(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 performs only the permission check.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Hardening: `gh` auth expiry makes every poll fail. The daemon must keep
/// running (log + retry, no crash), and the next successful poll recovers —
/// here across a restart (the fixture's failure env is process-wide).
///
/// Sync errors deliberately do NOT fire the `on_failure` notification (A10):
/// that event list has no poll-failure key and the urgent tier would spam on
/// transient gh/network hiccups; the failure surfaces in `daemon logs`
/// instead, and polls retry after one interval.
#[test]
fn poll_auth_failures_are_logged_and_recovery_happens_on_a_later_poll() {
    let mut env = Env::new("lc-authfail", false);
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

    // Auth "expires": every `gh pr list` fails while this env is set.
    let out = env.run(&["daemon", "start"], &[("FAKE_GH_LIST_FAIL", "999999")]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        true,
        "port file",
    );
    std::thread::sleep(Duration::from_secs(3));

    // No crash: the daemon answers, the state file is intact and empty.
    let out = env.run(&["daemon", "status"], &[]);
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("daemon running"));
    assert!(
        env.state().prs.is_empty(),
        "failed polls must not fabricate state"
    );

    // The failure is logged for `prx daemon logs`.
    let log = prx::daemon::newest_log_file(&env.data_dir.child("logs"))
        .expect("the daemon must write a log file");
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(text.contains("poll of org/one failed"), "{text}");

    // Auth "returns" (re-login): restart without the failure env — the next
    // successful poll discovers and reviews the PR.
    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_stopped();

    let out = env.run(
        &["daemon", "start"],
        &[("FAKE_AGENT_MANIFEST_JSON", agent_manifest_env()[0].1)],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    wait_for(
        || {
            env.state()
                .prs
                .get("org/one#1")
                .is_some_and(|r| r.last_review_at.is_some())
        },
        Duration::from_secs(15),
        "the recovered poll to review the PR",
    );
    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success());
    env.mark_stopped();
}

fn gh_call_count(env: &Env) -> usize {
    std::fs::read_to_string(env.gh_dir.child("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("pr list"))
        .count()
}

fn wait_for<F: Fn() -> bool>(cond: F, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

/// D5 / task 3.3: `prx pause` / `prx resume` through the daemon (thin
/// client) and in-process once it is stopped, the `--hours` / `--until`
/// forms and their errors, and `prx status` showing the pause.
#[test]
fn cli_pause_and_resume_through_the_daemon_and_in_process() {
    let mut env = Env::new("lc-pause", false);

    // Argument errors need no daemon: both or neither form is rejected.
    let out = env.run(&["pause"], &[]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("specify --hours N or --until"),
        "{}",
        stderr_of(&out)
    );
    let out = env.run(
        &["pause", "--hours", "1", "--until", "2099-01-01T00:00:00Z"],
        &[],
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not both"), "{}", stderr_of(&out));
    let out = env.run(&["pause", "--hours", "0"], &[]);
    assert!(!out.status.success());
    let out = env.run(&["pause", "--until", "not a time"], &[]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("cannot parse --until"),
        "{}",
        stderr_of(&out)
    );
    assert_eq!(env.state().paused_until, None);

    let out = env.run(&["daemon", "start"], agent_manifest_env());
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        true,
        "port file",
    );
    wait_for(
        || env.state().prs.contains_key("org/one#1"),
        Duration::from_secs(10),
        "startup poll to discover the PR",
    );

    // Through the daemon: no in-process fallback warning.
    let out = env.run(&["pause", "--hours", "2"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("in-process"),
        "{}",
        stderr_of(&out)
    );
    assert!(
        stdout_of(&out).contains("reviews paused until"),
        "{}",
        stdout_of(&out)
    );
    let until = env.state().paused_until.expect("pause persisted");
    let left = until - Utc::now();
    assert!(left > chrono::Duration::minutes(118) && left <= chrono::Duration::hours(2));

    // Status shows the pause on its own line (mode is not off).
    let out = env.run(&["status"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("reviews paused until"), "{stdout}");
    assert!(!stdout.contains("review mode: off"), "{stdout}");

    // review --all and review-now are gated; an explicit PR runs with a note.
    let out = env.run(&["review", "--all"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("nothing was enqueued"),
        "{}",
        stdout_of(&out)
    );
    let out = env.run(&["review-now"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("nothing will be enqueued"),
        "{}",
        stdout_of(&out)
    );
    let out = env.run(&["review", "org/one#1"], agent_manifest_env());
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("queued org/one#1 for review"), "{stdout}");
    assert!(stdout.contains("runs anyway"), "{stdout}");

    // The --until form replaces the pause; resume lifts it.
    let out = env.run(&["pause", "--until", "2099-01-01T00:00:00Z"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        env.state().paused_until.map(|t| t.to_rfc3339()),
        Some("2099-01-01T00:00:00+00:00".to_string())
    );
    let out = env.run(&["resume"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("reviews resumed"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(env.state().paused_until, None);
    let out = env.run(&["resume"], &[]);
    assert!(
        stdout_of(&out).contains("not paused"),
        "{}",
        stdout_of(&out)
    );

    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_stopped();

    // In-process fallback: same forms, written straight to state.json.
    let out = env.run(&["pause", "--until", "2099-01-01 09:30"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("daemon not running — running in-process"),
        "{}",
        stderr_of(&out)
    );
    assert!(env.state().paused_until.is_some());
    let out = env.run(&["status"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("reviews paused until"),
        "{}",
        stdout_of(&out)
    );
    let out = env.run(&["review", "--all"], agent_manifest_env());
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("nothing was reviewed"),
        "{}",
        stdout_of(&out)
    );
    let out = env.run(&["resume"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("reviews resumed"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(env.state().paused_until, None);
}

/// Design B2/B4 (task 3.5): `prx brief` through the daemon — no brief yet
/// is an error with a hint, `--regenerate` writes one (never posting to
/// GitHub) and prints it with its dates, a short reference resolves like
/// `prx review`, and a new push marks it "(stale)". In-process once the
/// daemon is stopped, a failed regenerate reports the error and keeps the
/// brief.
#[test]
fn cli_brief_prints_and_regenerates_through_the_daemon_and_in_process() {
    let mut env = Env::new("lc-brief", false);
    let brief_env: &[(&str, &str)] = &[("FAKE_AGENT_SUMMARY", "Adds a feature flag.")];
    let out = env.run(&["daemon", "start"], brief_env);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_running();
    wait_for_file(
        &env.data_dir.child("daemon.port"),
        Duration::from_secs(10),
        true,
        "port file",
    );
    wait_for(
        || env.state().prs.contains_key("org/one#1"),
        Duration::from_secs(10),
        "startup poll to discover the PR",
    );

    let out = env.run(&["brief", "org/one#1"], &[]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out)
            .contains("has no brief yet; write one with `prx brief org/one#1 --regenerate`"),
        "{}",
        stderr_of(&out)
    );

    let out = env.run(&["brief", "org/one#1", "--regenerate"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        !stderr_of(&out).contains("in-process"),
        "{}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "Adds a feature flag.", "{stdout}");
    assert!(
        lines[1].starts_with("created ") && lines[1].contains(" \u{b7} updated "),
        "{stdout}"
    );
    assert!(!stdout.contains("(stale)"), "{stdout}");
    let calls = std::fs::read_to_string(env.gh_dir.child("calls.log")).unwrap();
    assert!(!calls.contains("pr review"), "no GitHub writes: {calls}");
    let brief = env.state().prs["org/one#1"].brief.clone().unwrap();
    assert_eq!(brief.text, "Adds a feature flag.");

    // A short reference prints the same brief.
    let out = env.run(&["brief", "#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).starts_with("Adds a feature flag.\n"));

    // A new push moves the head: the brief is reported stale.
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "Add feature",
            "alice",
            "f/x",
            "bbbb",
            false,
            &Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        )],
    );
    let out = env.run(&["review-now"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    wait_for(
        || env.state().prs["org/one#1"].head_sha == "bbbb",
        Duration::from_secs(10),
        "the poll to see the new head",
    );
    let out = env.run(&["brief", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out)
            .lines()
            .nth(1)
            .unwrap()
            .ends_with(" (stale)"),
        "{}",
        stdout_of(&out)
    );

    let out = env.run(&["daemon", "stop"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    env.mark_stopped();

    // In-process: a failing engine reports the error and keeps the brief.
    let out = env.run(
        &["brief", "org/one#1", "--regenerate"],
        &[("FAKE_AGENT_SUMMARY_EXIT", "3")],
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("daemon not running — running in-process"),
        "{}",
        stderr_of(&out)
    );
    assert!(
        stderr_of(&out).contains("brief generation failed"),
        "{}",
        stderr_of(&out)
    );
    assert_eq!(
        env.state().prs["org/one#1"]
            .brief
            .as_ref()
            .map(|b| b.text.as_str()),
        Some("Adds a feature flag.")
    );
    // In-process regenerate succeeds and keeps created_at.
    let out = env.run(
        &["brief", "org/one#1", "--regenerate"],
        &[("FAKE_AGENT_SUMMARY", "Adds a feature flag, now default on.")],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).starts_with("Adds a feature flag, now default on.\n"));
    assert!(!stdout_of(&out).contains("(stale)"), "{}", stdout_of(&out));
    let after = env.state().prs["org/one#1"].brief.clone().unwrap();
    assert_eq!(after.created_at, brief.created_at);
    assert!(after.updated_at > brief.updated_at);
    assert_eq!(after.head_sha, "bbbb");
    // Short forms need the daemon.
    let out = env.run(&["brief", "#1"], &[]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("needs the daemon"),
        "{}",
        stderr_of(&out)
    );
}
