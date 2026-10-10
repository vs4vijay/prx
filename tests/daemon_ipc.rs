//! Phase 3 daemon and IPC integration tests, run in-process: the server,
//! scheduler, and drainer run as tokio tasks against the fake gh + fake
//! agent, and the tests exercise every endpoint, the A22 force semantics,
//! restart safety (the P3 acceptance core), and D6 concurrency.
//!
//! In-process tests share one process, so PRX_* env vars (used by the review
//! pipeline's manifest paths and the history appender) are set per test and
//! guarded by ENV_LOCK to keep parallel tests from racing.
//!
//! The guard is deliberately a std::sync::Mutex held across awaits (the env
//! must stay exclusive for the whole async test body, daemons included):

#![allow(clippy::await_holding_lock)]

mod common;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use prx::agent::EngineAdapter;
use prx::config::{Config, ReviewMode};
use prx::daemon::{self, DaemonHandle, Deps, MenubarDeferringNotifier, PidGuard, SchedulerOptions};
use prx::github::GhCli;
use prx::history::HistoryEntry;
use prx::ipc::{self, DaemonClient};
use prx::notify::CommandNotifier;
use prx::review::ReviewOverrides;
use prx::state::{PrState, SkipReason, StateFile, StateStore, Verdict};

/// A config using the fake agent, without a `cooldown:` block so `extra`
/// can supply one (the shared helper always emits the 60/30 defaults).
fn local_config(repos: &[&str], agent_path: &std::path::Path, extra: &str) -> String {
    let mut out = format!(
        "defaults:\n  agent: fake\n  poll_interval_minutes: 15\n  review_prompt: \"{DEFAULT_PROMPT_YAML}\"\n{extra}agents:\n  fake:\n    command: {}\n    args: [\"{{{{PROMPT}}}}\"]\n",
        agent_path.display()
    );
    out.push_str("repos:\n");
    for repo in repos {
        out.push_str(&format!("  - name: {repo}\n"));
    }
    out
}

/// Cooldowns of zero minutes: every pending PR is due immediately.
const COOLDOWN_ZERO: &str =
    "  cooldown:\n    before_first_review_minutes: 0\n    between_re_reviews_minutes: 0\n";

static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn env_guard() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

const OLD: &str = "2026-09-01T10:00:00Z"; // long past any cooldown
/// The test review prompt, inline in `defaults.review_prompt` (design F5)
/// as a YAML double-quoted scalar (interpolated by `local_config`).
const DEFAULT_PROMPT_YAML: &str =
    "BASE-PROMPT: review PR #{{PR_NUMBER}}; manifest {{MANIFEST_PATH}}.\\n";

/// Fast scheduler options: 100ms tick, ~0.9s poll intervals (15 min scaled).
fn fast_opts() -> SchedulerOptions {
    SchedulerOptions {
        tick: Duration::from_millis(100),
        jitter: true,
        startup_poll: true,
        interval_scale: 0.001,
        config_check: Duration::from_millis(100),
        ..SchedulerOptions::default()
    }
}

async fn wait_for<F, Fut>(mut cond: F, timeout: Duration, what: &str)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Records every argv the daemon asks to open (design D9: no real browser
/// ever launches in tests).
#[derive(Default)]
struct SpyOpener(Mutex<Vec<Vec<String>>>);

impl prx::browser::Opener for SpyOpener {
    fn spawn(&self, argv: &[String]) -> Result<(), String> {
        self.0.lock().unwrap().push(argv.to_vec());
        Ok(())
    }
}

/// One test environment: temp config/data/gh/agent dirs, env wiring, and an
/// in-process daemon handle. The cfg/agent dir fields are never read but
/// must be kept alive: dropping them deletes the directories in use.
struct Env {
    #[allow(dead_code)]
    cfg_dir: TempDir,
    data_dir: TempDir,
    gh_dir: TempDir,
    #[allow(dead_code)]
    agent_dir: TempDir,
    config: PathBuf,
    agent_path: PathBuf,
    #[allow(dead_code)]
    gh_path: PathBuf,
    /// The auto-open spy shared by every daemon this env starts.
    opener: std::sync::Arc<SpyOpener>,
}

impl Env {
    fn new(tag: &str) -> Self {
        let cfg_dir = TempDir::new(&format!("{tag}-cfg"));
        let data_dir = TempDir::new(&format!("{tag}-data"));
        let gh_dir = TempDir::new(&format!("{tag}-gh"));
        let agent_dir = TempDir::new(&format!("{tag}-agent"));
        install_fake_gh(gh_dir.path());
        let agent_path = install_fake_agent(agent_dir.path());
        let gh_path = gh_dir.path().join("gh");
        let config = cfg_dir.path().join("config.yml");
        Env {
            cfg_dir,
            data_dir,
            gh_dir,
            agent_dir,
            config,
            agent_path,
            gh_path,
            opener: std::sync::Arc::new(SpyOpener::default()),
        }
    }

    /// Write the config (with its inline prompt) and wire the process env this
    /// daemon (and its gh/agent children) will see. Callers hold ENV_LOCK.
    /// All FAKE_* variables are reset first: the env is process-global and
    /// a previous test's settings (e.g. FAKE_AGENT_MUTEX_DIR pointing at a
    /// deleted temp dir) must not leak into this daemon's children.
    fn configure(&self, repos: &[&str], extra: &str, manifest_env: &[(&str, &str)]) {
        for var in [
            "FAKE_AGENT_MANIFEST",
            "FAKE_AGENT_MANIFEST_JSON",
            "FAKE_AGENT_SESSION",
            "FAKE_AGENT_SLEEP",
            "FAKE_AGENT_EXIT",
            "FAKE_AGENT_PROMPT_OUT",
            "FAKE_AGENT_MUTEX_DIR",
            "FAKE_AGENT_SUMMARY",
            "FAKE_AGENT_SUMMARY_EXIT",
            "FAKE_AGENT_SUMMARY_SLEEP",
            "FAKE_AGENT_SUMMARY_PROMPT_OUT",
            "FAKE_AGENT_SUMMARY_RUNS_LOG",
            "FAKE_AGENT_BRIEF",
            "FAKE_GH_LIST_FAIL",
        ] {
            std::env::remove_var(var);
        }
        write_file(&self.config, &local_config(repos, &self.agent_path, extra));
        std::env::set_var("PRX_CONFIG", self.config.as_os_str());
        std::env::set_var("PRX_DATA_DIR", self.data_dir.path().as_os_str());
        std::env::set_var("FAKE_GH_DIR", self.gh_dir.path().as_os_str());
        std::env::set_var("FAKE_GH_LOG", self.gh_dir.child("calls.log").as_os_str());
        for (k, v) in manifest_env {
            std::env::set_var(k, v);
        }
    }

    fn load_config(&self) -> Config {
        Config::load(&self.config).expect("config should parse")
    }

    fn deps(&self) -> Deps {
        Deps {
            // In-process tests have no PATH setup: point gh at the fixture.
            gh: GhCli::with_binary(self.gh_path.to_string_lossy().as_ref()),
            adapter: EngineAdapter::default(),
            notifier: MenubarDeferringNotifier::wrapping(CommandNotifier),
            config_path: Some(self.config.clone()),
            store: StateStore::new(self.data_dir.path().join("state.json")),
            history_path: self.data_dir.path().join("history.jsonl"),
            data_dir: self.data_dir.path().to_path_buf(),
            opener: self.opener.clone(),
        }
    }

    /// Every argv the daemon opened so far (auto-open).
    fn opened(&self) -> Vec<Vec<String>> {
        self.opener.0.lock().unwrap().clone()
    }

    async fn start(&self, opts: SchedulerOptions) -> DaemonHandle {
        let guard = PidGuard::acquire(self.data_dir.path()).expect("pid guard");
        daemon::spawn_core(self.deps(), self.load_config(), opts, Some(guard))
            .await
            .expect("daemon should spawn")
    }

    fn client(&self, handle: &DaemonHandle) -> DaemonClient {
        DaemonClient::with_base(
            format!("http://127.0.0.1:{}", handle.port),
            handle.token.clone(),
        )
    }

    fn state(&self) -> StateFile {
        StateStore::new(self.data_dir.child("state.json"))
            .load()
            .expect("state.json should parse")
    }

    /// Outcome history: every entry except the `review_started` markers
    /// (design D6) each review run also writes — see [`Env::raw_history`].
    fn history(&self) -> Vec<HistoryEntry> {
        self.raw_history()
            .into_iter()
            .filter(|e| !matches!(e, HistoryEntry::ReviewStarted { .. }))
            .collect()
    }

    /// Every history entry, `review_started` included (newest first).
    fn raw_history(&self) -> Vec<HistoryEntry> {
        prx::history::query(&self.data_dir.child("history.jsonl"), None, 0).unwrap()
    }

    fn review_bodies(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.gh_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().ends_with(".review-body.txt"))
                    .unwrap_or(false)
            })
            .collect()
    }
}

fn approved_manifest() -> String {
    manifest_json(
        "approved",
        &[&finding_json(
            Some("src/a.rs"),
            Some(3),
            "nit",
            "rename this",
        )],
    )
}

// ------------------------------------------------------------------- tests

#[tokio::test]
async fn auth_requires_a_bearer_token() {
    let _env = env_guard();
    let env = Env::new("ipc-auth");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    assert_eq!(
        ipc::probe_status(handle.port, "/status", None)
            .await
            .unwrap(),
        401
    );
    assert_eq!(
        ipc::probe_status(handle.port, "/status", Some("wrong-token"))
            .await
            .unwrap(),
        401
    );
    assert_eq!(
        ipc::probe_status(handle.port, "/status", Some(&handle.token))
            .await
            .unwrap(),
        200
    );
    // The typed client works with the right token.
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs.len(), 0);
    handle.shutdown().await;
}

/// D3: the typed client announces the menubar (`X-Prx-Client`) on every
/// request and the auth middleware records the last-seen instant; a valid
/// request without the header (the raw auth probe) records nothing.
#[tokio::test]
async fn menubar_client_header_records_last_seen_through_ipc() {
    let _env = env_guard();
    let env = Env::new("ipc-menubar-seen");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    // Nothing announced itself yet.
    assert!(!handle.shared.menubar_recently_seen(Duration::from_secs(90)));
    // Authenticated but headerless: no liveness recorded.
    assert_eq!(
        ipc::probe_status(handle.port, "/status", Some(&handle.token))
            .await
            .unwrap(),
        200
    );
    assert!(!handle.shared.menubar_recently_seen(Duration::from_secs(90)));
    // The DaemonClient announces itself on every request.
    client.get_status().await.unwrap();
    assert!(handle.shared.menubar_recently_seen(Duration::from_secs(90)));
    handle.shutdown().await;
}

/// D3: liveness expires — a freshly recorded last-seen counts inside the
/// 90 s window but not inside a zero-length one (Instant cannot be faked,
/// so expiry is asserted through the accessor's window comparison).
#[tokio::test]
async fn menubar_liveness_expires_after_the_window_through_ipc() {
    let _env = env_guard();
    let env = Env::new("ipc-menubar-expiry");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;

    env.client(&handle).get_status().await.unwrap();
    assert!(handle.shared.menubar_recently_seen(Duration::from_secs(90)));
    assert!(!handle.shared.menubar_recently_seen(Duration::ZERO));
    handle.shutdown().await;
}

#[tokio::test]
async fn status_endpoint_reports_daemon_info_and_discovered_prs() {
    let _env = env_guard();
    let env = Env::new("ipc-status");
    env.configure(&["org/one"], "", &[]);
    // A PR created just now: pending and inside its first-review cooldown.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
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
            &now,
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    wait_for(
        || async {
            client
                .get_status()
                .await
                .map(|s| s.prs.len() == 1)
                .unwrap_or(false)
        },
        Duration::from_secs(5),
        "startup poll to discover the PR",
    )
    .await;
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs.len(), 1);
    let pr = &status.prs[0];
    assert_eq!(pr.key, "org/one#1");
    assert_eq!(pr.state, PrState::Pending);
    assert!(pr.summary.contains("cooling down"), "{}", pr.summary);
    assert_eq!(status.in_flight, 0);
    assert_eq!(status.queue_depth, 0);
    assert!(status.uptime_seconds <= 5, "{}", status.uptime_seconds);
    assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(status.pid, std::process::id());
    assert!(status.last_poll.contains_key("org/one"));
    assert!(status.last_state_poll.is_some());
    handle.shutdown().await;
}

/// Design D6/D7 (task 2.1): `/status` carries the sorted engine names, the
/// menu settings, and each PR's effective engine; `/config` reports `menu`.
/// An older daemon's payload without these fields still deserializes.
#[tokio::test]
async fn status_carries_engines_menu_and_pr_engine() {
    let _env = env_guard();
    let env = Env::new("ipc-status-engines");
    env.configure(&["org/one"], "", &[]);
    let mut yaml = std::fs::read_to_string(&env.config).unwrap();
    yaml.push_str(
        "  - name: org/two\n    engine: my-agent\nmenu:\n  sort: key\n  group_by: repo\n\
         engines:\n  my-agent:\n    type: agent\n    command: my-agent\n",
    );
    write_file(&env.config, &yaml);
    let now = fresh_now();
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, &now)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/two",
        &[pr_json(2, "Two", "bob", "f/y", "bbbb", false, &now)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async {
            client
                .get_status()
                .await
                .map(|s| s.prs.len() == 2)
                .unwrap_or(false)
        },
        Duration::from_secs(5),
        "startup poll to discover both PRs",
    )
    .await;

    let status = client.get_status().await.unwrap();
    assert_eq!(
        status.engines,
        ["claude", "codex", "fake", "my-agent", "pi"]
    );
    assert_eq!(status.menu.sort, prx::config::MenuSort::Key);
    assert_eq!(status.menu.group_by, prx::config::MenuGroupBy::Repo);
    let engine_of = |key: &str| {
        status
            .prs
            .iter()
            .find(|p| p.key == key)
            .and_then(|p| p.engine.clone())
    };
    assert_eq!(engine_of("org/one#1").as_deref(), Some("fake"));
    assert_eq!(engine_of("org/two#2").as_deref(), Some("my-agent"));

    let wire = serde_json::to_value(&status).unwrap();
    assert_eq!(wire["menu"]["sort"], "key");
    assert_eq!(wire["menu"]["group_by"], "repo");

    let config = client.get_config().await.unwrap();
    assert_eq!(config.raw.menu, status.menu);
    handle.shutdown().await;

    // An older daemon's payload: no engines, menu, or per-PR engine.
    let old: ipc::StatusResponse = serde_json::from_value(serde_json::json!({
        "version": "0", "pid": 1, "uptime_seconds": 0, "in_flight": 0,
        "queue_depth": 0, "last_poll": {}, "last_state_poll": null,
        "prs": [{
            "key": "org/r#5", "icon": "x", "state": "pending",
            "skip_reason": null, "title": "t", "author": "alice",
            "head_branch": "b", "is_draft": false, "last_verdict": null,
            "blocking_count": 0, "nit_count": 0, "last_review_at": null,
            "summary": "s", "last_review_age": "never"
        }]
    }))
    .unwrap();
    assert!(old.engines.is_empty());
    assert_eq!(old.menu, prx::config::MenuSettings::default());
    assert_eq!(old.prs[0].engine, None);
}

#[tokio::test]
async fn snooze_ignore_unignore_transition_state_through_ipc() {
    let _env = env_guard();
    let env = Env::new("ipc-mutate");
    env.configure(&["org/one"], "", &[]);
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
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async {
            client
                .get_status()
                .await
                .map(|s| s.prs.len() == 1)
                .unwrap_or(false)
        },
        Duration::from_secs(5),
        "PR discovery",
    )
    .await;

    // Snooze for 1 hour: skipped(snoozed) with snooze_until ≈ now + 1h.
    let resp = client.snooze("org/one#1", Some(1.0), false).await.unwrap();
    assert!(resp.changed, "{resp:?}");
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs[0].state, PrState::Skipped);
    assert_eq!(status.prs[0].skip_reason, Some(SkipReason::Snoozed));
    let stored = env.state();
    let until = stored.prs["org/one#1"].snooze_until.unwrap();
    let expected = chrono::Utc::now() + chrono::TimeDelta::try_hours(1).unwrap();
    assert!(
        (until - expected).num_minutes().abs() < 5,
        "snooze_until {until} vs expected {expected}"
    );
    // Persisted to state.json (A4: the write went through the daemon).
    assert_eq!(
        env.state().prs["org/one#1"].skip_reason,
        Some(SkipReason::Snoozed)
    );

    // Snoozing without a schedule is a client-side 400.
    let err = client.snooze("org/one#1", None, false).await.unwrap_err();
    assert!(err.to_string().contains("hours or until_push"), "{err}");

    // Ignore supersedes the snooze (snooze bookkeeping cleared).
    let resp = client.ignore("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.skip_reason, Some(SkipReason::Ignored));
    assert_eq!(rec.snooze_until, None);
    assert!(!rec.snooze_until_push);

    // Unignore → pending with a fresh cooldown anchored to now (§6).
    let resp = client.unignore("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, PrState::Pending);
    assert_eq!(rec.skip_reason, None);
    let anchor = rec.review_anchor_at.unwrap();
    assert!(
        (chrono::Utc::now() - anchor).num_seconds().abs() < 30,
        "anchor {anchor} should be ~now"
    );

    // Idempotence: re-unignore and re-ignore on pending are no-ops.
    let resp = client.unignore("org/one#1").await.unwrap();
    assert!(!resp.changed, "{resp:?}");
    let resp = client.snooze("org/one#1", Some(0.5), false).await.unwrap();
    assert!(resp.changed);
    let resp = client.snooze("org/one#1", Some(0.5), false).await.unwrap();
    assert!(!resp.changed, "snoozing a snoozed PR changes nothing");
    handle.shutdown().await;
}

/// D1: un-snooze round-trips through the daemon — a snoozed PR returns to
/// its pre-snooze state (reviewed_approved here, not pending) with the
/// schedule cleared and the note naming the restore; un-snoozing a
/// non-snoozed PR reports no change and leaves the record untouched.
#[tokio::test]
async fn unsnooze_restores_pre_snooze_state_through_ipc() {
    let _env = env_guard();
    let env = Env::new("ipc-unsnooze");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
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
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async { env.state().prs.contains_key("org/one#1") },
        Duration::from_secs(5),
        "PR discovery",
    )
    .await;

    // Non-snoozed (pending or in flight): un-snooze reports no change.
    let resp = client.unsnooze("org/one#1").await.unwrap();
    assert!(!resp.changed, "{resp:?}");

    // The review lands approved; un-snoozing is still a no-op there.
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "the review to land as approved",
    )
    .await;
    let resp = client.unsnooze("org/one#1").await.unwrap();
    assert!(!resp.changed, "{resp:?}");

    // Snooze the reviewed PR, then un-snooze it: back to reviewed_approved
    // (not pending), snooze bookkeeping cleared, note naming the restore.
    let resp = client.snooze("org/one#1", Some(1.0), false).await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert_eq!(
        env.state().prs["org/one#1"].previous_state,
        Some(PrState::ReviewedApproved)
    );
    let resp = client.unsnooze("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    let note = resp.note.unwrap();
    assert!(note.contains("ReviewedApproved"), "{note}");
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, PrState::ReviewedApproved);
    assert_eq!(rec.skip_reason, None);
    assert_eq!(rec.snooze_until, None);
    assert!(!rec.snooze_until_push);
    assert_eq!(rec.previous_state, None);

    // Idempotence: un-snoozing the restored PR is a no-op again.
    let resp = client.unsnooze("org/one#1").await.unwrap();
    assert!(!resp.changed, "{resp:?}");
    handle.shutdown().await;
}

#[tokio::test]
async fn scheduler_reviews_due_prs_end_to_end() {
    let _env = env_guard();
    let env = Env::new("ipc-review");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
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
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "the queued review to complete",
    )
    .await;

    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.last_verdict, Some(Verdict::Approved));
    assert_eq!(rec.last_reviewed_sha.as_deref(), Some("aaaa"));
    assert_eq!(rec.blocking_count, 0);
    assert_eq!(rec.nit_count, 1);
    assert_eq!(rec.review_attempts, 0);
    assert!(rec.last_review_at.is_some());

    // The review body went through gh with the attribution footer (A13).
    let bodies = env.review_bodies();
    assert_eq!(bodies.len(), 1);
    let body = std::fs::read_to_string(&bodies[0]).unwrap();
    assert!(body.contains("automated review by prx"), "{body}");

    // History got the enriched entry.
    let history = env.history();
    assert_eq!(history.len(), 1);
    match &history[0] {
        HistoryEntry::ReviewCompleted {
            repo,
            number,
            agent,
            ..
        } => {
            assert_eq!(repo, "org/one");
            assert_eq!(*number, 1);
            assert_eq!(agent.as_deref(), Some("fake"));
        }
        other => panic!("expected ReviewCompleted, got {other:?}"),
    }

    // Queue drained (the job ends after its executive-summary step, which
    // follows the committed completion — design D8); /status reflects the
    // new state (from the read cache).
    wait_completed_and_idle(&env, &client, 1, "the review job to finish").await;
    let status = client.get_status().await.unwrap();
    assert_eq!(status.queue_depth, 0);
    assert_eq!(status.in_flight, 0);
    assert_eq!(status.prs[0].state, PrState::ReviewedApproved);
    handle.shutdown().await;
}

#[tokio::test]
async fn review_endpoint_a22_force_vs_nonforce() {
    let _env = env_guard();
    let env = Env::new("ipc-a22");
    env.configure(
        &["org/one"],
        "",
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    // Both PRs were created just now: with the default 60-minute cooldown
    // the startup poll discovers them but nothing is due yet.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[
            pr_json(1, "Fresh A", "alice", "f/x", "aaaa", false, &now),
            pr_json(2, "Fresh B", "bob", "f/y", "bbbb", false, &now),
        ],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    wait_for(
        || async { env.state().prs.len() == 2 },
        Duration::from_secs(5),
        "both PRs to be discovered",
    )
    .await;
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(env.history().len(), 0, "nothing is due yet");

    // Non-force "all" respects cooldowns: nothing is enqueued (A22).
    let resp = client.review(None, true, false).await.unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");
    // Non-force specific key on a cooling-down PR: nothing enqueued either.
    let resp = client
        .review(Some("org/one#2"), false, false)
        .await
        .unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        env.state().prs["org/one#2"].state,
        PrState::Pending,
        "cooling-down PR must not be reviewed non-forced"
    );

    // Force bypasses the cooldown (A22) and executes through the queue.
    let resp = client.review(Some("org/one#2"), false, true).await.unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#2".to_string()]);
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#2").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "forced review to complete",
    )
    .await;
    assert_eq!(env.history().len(), 1);
    assert_eq!(
        env.state().prs["org/one#1"].state,
        PrState::Pending,
        "the untouched PR must stay pending"
    );

    // Unknown keys fail with clear errors.
    let err = client
        .review(Some("org/one#99"), false, true)
        .await
        .unwrap_err();
    // D5: an untracked key is looked up on GitHub before anything else.
    assert!(
        err.to_string().contains("does not exist on GitHub"),
        "{err}"
    );
    let err = client
        .review(Some("org/unknown#1"), false, true)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not watched"), "{err}");
    let err = client.review(Some("bogus"), false, true).await.unwrap_err();
    assert!(err.to_string().contains("invalid PR target"), "{err}");
    handle.shutdown().await;
}

#[tokio::test]
async fn review_now_triggers_poll_and_due_scan() {
    let _env = env_guard();
    let env = Env::new("ipc-now");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    // The daemon starts with an empty repo; the PR appears afterwards.
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
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

    client.review_now().await.unwrap();
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "review-now to poll, discover, and review the PR",
    )
    .await;
    handle.shutdown().await;
}

#[tokio::test]
async fn approve_endpoint_is_graceful_on_already_approved() {
    let _env = env_guard();
    let env = Env::new("ipc-approve");
    env.configure(&["org/one"], "", &[]);
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
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    let resp = client.approve("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");

    // gh rejects a second approval with "already approved" → graceful no-op.
    write_file(
        &env.gh_dir.child("org_one#1.approve-error"),
        "gh: Pull request org/one#1 is already approved by you",
    );
    let resp = client.approve("org/one#1").await.unwrap();
    assert!(!resp.changed, "{resp:?}");
    assert!(resp.note.unwrap().contains("already approved"));

    // Any other gh failure surfaces as an error, not a panic/500 crash.
    write_file(
        &env.gh_dir.child("org_one#1.approve-error"),
        "gh: network unreachable",
    );
    let err = client.approve("org/one#1").await.unwrap_err();
    assert!(err.to_string().contains("approve failed"), "{err}");
    handle.shutdown().await;
}

/// Menu-bar-shell "Approve from the menu bar": approving a reviewed PR posts
/// the GitHub approval AND takes it out of the attention set
/// (reviewed_changes_requested → reviewed_approved). Approving an untracked
/// PR keeps working and writes no state entry.
#[tokio::test]
async fn approve_updates_attention_state_and_untracked_still_works() {
    let _env = env_guard();
    let env = Env::new("ipc-approve-state");
    let changes_requested = manifest_json(
        "changes_requested",
        &["{\"file\": \"src/a.rs\", \"line\": 1, \"severity\": \"blocking\", \"body\": \"missing tenant scope\"}"],
    );
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", changes_requested.as_str())],
    );
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
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    // The agent review lands changes-requested: the PR is in the attention
    // set and /status reflects it.
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedChangesRequested)
            )
        },
        Duration::from_secs(10),
        "the review to land as changes requested",
    )
    .await;
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs[0].state, PrState::ReviewedChangesRequested);

    // The human approves: gh posts the approval (recorded by the fake gh)
    // and the daemon's state moves out of the attention set.
    let resp = client.approve("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert!(
        resp.note
            .as_deref()
            .unwrap_or_default()
            .contains("approval posted"),
        "{resp:?}"
    );
    assert!(
        count_calls(&env.gh_dir, "--approve") >= 1,
        "gh approval posted"
    );
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, PrState::ReviewedApproved);
    assert_eq!(rec.last_verdict, Some(Verdict::ChangesRequested));
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs[0].state, PrState::ReviewedApproved);

    // Approving a PR the daemon does not track still succeeds (the gh write
    // is unconditional) and leaves no state entry behind.
    let resp = client.approve("org/one#99").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert!(count_calls(&env.gh_dir, "--approve") >= 2);
    assert!(
        !env.state().prs.contains_key("org/one#99"),
        "untracked approve must not create a state entry"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn history_endpoint_filters_by_repo_and_limit() {
    let _env = env_guard();
    let env = Env::new("ipc-history");
    env.configure(
        &["org/one", "org/two"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "A", "alice", "f/x", "aaaa", false, OLD)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/two",
        &[pr_json(2, "B", "bob", "f/y", "bbbb", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    wait_for(
        || async { env.history().len() == 2 },
        Duration::from_secs(10),
        "both reviews to land in history",
    )
    .await;

    // Each review wrote a `review_started` entry before its outcome (D6).
    let all = client.history(None, 0).await.unwrap();
    assert_eq!(all.entries.len(), 4);
    let one = client.history(Some("org/one"), 0).await.unwrap();
    assert_eq!(one.entries.len(), 2);
    assert!(one.entries.iter().all(|e| e.repo() == "org/one"));
    assert!(matches!(
        one.entries[0],
        HistoryEntry::ReviewCompleted { .. }
    ));
    assert!(matches!(one.entries[1], HistoryEntry::ReviewStarted { .. }));
    let limited = client.history(None, 1).await.unwrap();
    assert_eq!(limited.entries.len(), 1);
    handle.shutdown().await;
}

#[tokio::test]
async fn insights_endpoint_computes_from_history_with_filters() {
    let _env = env_guard();
    let env = Env::new("ipc-insights");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[(
            "FAKE_AGENT_MANIFEST_JSON",
            manifest_json(
                "changes_requested",
                &[
                    "{\"file\": \"src/a.rs\", \"line\": 1, \"severity\": \"blocking\", \"body\": \"missing tenant scope\"}",
                    "{\"file\": \"src/b.rs\", \"line\": 2, \"severity\": \"nit\", \"body\": \"rename this\"}",
                ],
            )
            .as_str(),
        )],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "A", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async { env.history().len() == 1 },
        Duration::from_secs(10),
        "the review to land in history",
    )
    .await;

    // The bare endpoint returns the insights payload (no 501 stub anymore).
    assert_eq!(
        ipc::probe_status(handle.port, "/insights", Some(&handle.token))
            .await
            .unwrap(),
        200
    );

    let insights = client.insights(None, None).await.unwrap();
    assert_eq!(insights.reviews.total, 1);
    assert_eq!(insights.reviews.failed, 0);
    assert_eq!(insights.per_repo.len(), 1);
    assert_eq!(insights.per_repo[0].repo, "org/one");
    assert_eq!(insights.per_repo[0].approval_rate, 0.0);
    // A18: the blocking finding text flows from manifest → history → insights.
    assert_eq!(insights.top_blocking_issues.len(), 1);
    assert_eq!(insights.top_blocking_issues[0].text, "missing tenant scope");
    assert_eq!(insights.top_blocking_issues[0].count, 1);
    // The nit is not a blocking issue.
    assert!(insights
        .top_blocking_issues
        .iter()
        .all(|i| i.text != "rename this"));

    // Repo and window filters.
    let filtered = client.insights(Some("org/one"), Some(7)).await.unwrap();
    assert_eq!(filtered.window_days, Some(7));
    assert_eq!(filtered.reviews.total, 1);
    let empty = client.insights(Some("org/two"), None).await.unwrap();
    assert_eq!(empty.reviews.total, 0);
    assert!(empty.per_repo.is_empty());
    handle.shutdown().await;
}

#[tokio::test]
async fn daemon_restart_does_not_duplicate_reviews() {
    let _env = env_guard();
    let env = Env::new("ipc-restart");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
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

    // First daemon run: the due PR is discovered and reviewed once.
    let handle = env.start(fast_opts()).await;
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "first daemon run to review the PR",
    )
    .await;
    assert_eq!(env.history().len(), 1);
    handle.shutdown().await;
    assert!(
        !env.data_dir.child("daemon.pid").exists(),
        "pid file must be released on shutdown"
    );

    // Restart against the same data dir: the PR is unchanged, so the
    // PollUnchanged path keeps it asleep — no duplicate review (P3
    // acceptance). Wait across several poll cycles to prove it stays put.
    let handle = env.start(fast_opts()).await;
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        assert_eq!(
            env.history().len(),
            1,
            "restart must not re-review an unchanged PR"
        );
        assert_eq!(env.review_bodies().len(), 1);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, PrState::ReviewedApproved);
    assert_eq!(rec.last_reviewed_sha.as_deref(), Some("aaaa"));
    handle.shutdown().await;
}

#[tokio::test]
async fn max_concurrent_one_serializes_agent_runs() {
    let _env = env_guard();
    let env = Env::new("ipc-concurrency");
    let mutex_dir = TempDir::new("ipc-concurrency-mutex");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_AGENT_SLEEP", "0.2"),
            ("FAKE_AGENT_MUTEX_DIR", mutex_dir.path().to_str().unwrap()),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[
            pr_json(1, "A", "alice", "f/x", "aaaa", false, OLD),
            pr_json(2, "B", "bob", "f/y", "bbbb", false, OLD),
        ],
    );
    let handle = env.start(fast_opts()).await;

    wait_for(
        || async { env.history().len() == 2 },
        Duration::from_secs(15),
        "both due PRs to be reviewed",
    )
    .await;
    assert!(env.state().prs["org/one#1"].last_review_at.is_some());
    assert!(env.state().prs["org/one#2"].last_review_at.is_some());

    // The fake agent records its start/end times under a shared lock dir:
    // with max_concurrent_reviews = 1 the runs must not overlap.
    let timeline = std::fs::read_to_string(mutex_dir.child("timeline.log")).unwrap();
    let mut intervals: Vec<(u128, u128)> = Vec::new();
    let mut start = None;
    for line in timeline.lines() {
        let (kind, ns) = line
            .split_once(' ')
            .expect("timeline lines are 'start|end <nanos>'");
        let ns: u128 = ns.parse().unwrap();
        match kind {
            "start" => start = Some(ns),
            "end" => intervals.push((start.expect("end after start"), ns)),
            other => panic!("unexpected timeline entry: {other}"),
        }
    }
    intervals.sort();
    assert_eq!(intervals.len(), 2, "timeline: {timeline}");
    assert!(
        intervals[0].1 <= intervals[1].0,
        "agent runs overlapped: {intervals:?}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn jittered_intervals_keep_polling_repos() {
    let _env = env_guard();
    let env = Env::new("ipc-jitter");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;

    // With the 15-minute default scaled to ~0.9s (±10% jitter via rand),
    // several polls should land within a few seconds.
    wait_for(
        || async { count_calls(&env.gh_dir, "pr list") >= 3 },
        Duration::from_secs(8),
        "at least 3 jittered polls",
    )
    .await;
    handle.shutdown().await;
}

fn count_calls(gh_dir: &TempDir, needle: &str) -> usize {
    std::fs::read_to_string(gh_dir.child("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains(needle))
        .count()
}

/// Hardening: `gh pr list` fails twice (auth expiry, simulated by the fake
/// gh), then succeeds. The daemon must not crash, must keep answering, and
/// the next successful poll recovers and reviews the PR (same daemon — with
/// the fast scheduler the interval is ~0.9s).
#[tokio::test]
async fn sync_failures_recover_on_a_later_poll_without_crash() {
    let _env = env_guard();
    let env = Env::new("ipc-authfail");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_GH_LIST_FAIL", "2"),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "A", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    // The startup poll runs on its own task after spawn: wait for it rather
    // than racing it. It fails (FAKE_GH_LIST_FAIL=2), so nothing is tracked,
    // but /status keeps answering.
    wait_for(
        || async { count_calls(&env.gh_dir, "pr list") >= 1 },
        Duration::from_secs(5),
        "the failing startup poll to fire",
    )
    .await;
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs.len(), 0);

    // Still failing: a forced poll + due-scan reports and changes nothing.
    client.review_now().await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(env.state().prs.is_empty());

    // Countdown exhausted: the next poll succeeds and the pipeline runs.
    client.review_now().await.unwrap();
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "the recovered poll to discover and review the PR",
    )
    .await;
    assert_eq!(env.history().len(), 1);
    handle.shutdown().await;
}

/// Corrupt history lines are invisible to the API but do not break it:
/// /history serves only the parseable entries and /insights stays 200.
#[tokio::test]
async fn corrupt_history_lines_are_skipped_by_the_endpoints() {
    let _env = env_guard();
    let env = Env::new("ipc-corrupt");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let good = r#"{"event":"review_completed","ts":"2026-09-10T10:00:00Z","repo":"org/one","number":1,"title":"t","author":"alice","verdict":"changes_requested","blocking_count":2,"nit_count":0}"#;
    write_file(
        &env.data_dir.child("history.jsonl"),
        &format!("{good}\n{{truncated\n\n"),
    );

    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    let history = client.history(None, 0).await.unwrap();
    assert_eq!(history.entries.len(), 1);
    let insights = client.insights(None, None).await.unwrap();
    assert_eq!(insights.reviews.total, 1);
    assert_eq!(insights.effectiveness.changes_requested_reviews, 1);
    handle.shutdown().await;
}

/// Tasks 9.2 + window paging: `GET /usage` aggregates the history file
/// (bucket/window/group/repo), invalid values answer 400 listing the allowed
/// ones, and `GET /history?before=` pages strictly older entries.
#[tokio::test]
async fn usage_endpoint_and_history_paging() {
    let _env = env_guard();
    let env = Env::new("ipc-usage");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let at =
        |days: i64| (chrono::Utc::now() - chrono::TimeDelta::try_days(days).unwrap()).to_rfc3339();
    let line = |ts: &str, number: u64, agent: &str, usage: &str| {
        format!(
            r#"{{"event":"review_completed","ts":"{ts}","repo":"org/one","number":{number},"title":"t","author":"alice","agent":"{agent}","verdict":"approved","blocking_count":0,"nit_count":0{usage}}}"#
        )
    };
    let lines = [
        line(
            &at(3),
            1,
            "claude",
            r#","usage":{"input_tokens":1000,"output_tokens":100,"cost_usd":0.5,"session_id":"s-1"},"summary_usage":{"input_tokens":50,"output_tokens":5}"#,
        ),
        line(
            &at(2),
            1,
            "claude",
            r#","usage":{"input_tokens":2000,"output_tokens":200,"session_id":"s-1"}"#,
        ),
        line(
            &at(1),
            2,
            "codex",
            r#","usage":{"input_tokens":1500,"cache_read_tokens":1024}"#,
        ),
        line(&at(1), 3, "pi", ""),
    ];
    write_file(
        &env.data_dir.child("history.jsonl"),
        &(lines.join("\n") + "\n"),
    );

    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    let query = prx::usage::Query {
        bucket: prx::usage::Bucket::Day,
        window: prx::usage::Window::Days7,
        group: prx::usage::Group::Engine,
        repo: None,
    };
    let usage = client.usage(&query).await.unwrap();
    assert_eq!(usage.runs_without_usage, 1);
    assert_eq!(usage.overall.runs, 3);
    assert_eq!(usage.overall.input_tokens, 4550);
    assert_eq!(usage.overall.review.input_tokens, 4500);
    assert_eq!(usage.overall.summary.input_tokens, 50);
    assert_eq!(usage.overall.runs_with_cost, 1);
    let names: Vec<&str> = usage.groups.iter().map(|g| g.group.as_str()).collect();
    assert_eq!(names, ["claude", "codex"]);
    assert!(usage.series.iter().all(|s| s.points.len() == 7));
    let sessions = client
        .usage(&prx::usage::Query {
            group: prx::usage::Group::Session,
            ..query.clone()
        })
        .await
        .unwrap();
    assert_eq!(sessions.groups[0].group, "claude:s-1");
    assert_eq!(sessions.groups[0].totals.runs, 2);

    // Raw wire: defaults, the pinned keys, and 400s that list the values.
    let http = reqwest::Client::new();
    let get = |path: &str| {
        http.get(format!("http://127.0.0.1:{}{path}", handle.port))
            .bearer_auth(&handle.token)
            .send()
    };
    let body: serde_json::Value = get("/usage").await.unwrap().json().await.unwrap();
    assert_eq!(
        (&body["bucket"], &body["window"], &body["group"]),
        (
            &serde_json::json!("day"),
            &serde_json::json!("30d"),
            &serde_json::json!("none")
        )
    );
    assert_eq!(body["series"][0]["group"], "all");
    assert_eq!(body["series"][0]["points"].as_array().unwrap().len(), 30);
    let other: serde_json::Value = get("/usage?repo=org%2Ftwo")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["overall"]["runs"], 0);
    for (path, allowed) in [
        ("/usage?bucket=hour", "day, week, month"),
        ("/usage?window=1y", "7d, 30d, 90d, all"),
        ("/usage?group=author", "none, engine, repo, pr, session"),
    ] {
        let response = get(path).await.unwrap();
        assert_eq!(response.status().as_u16(), 400, "{path}");
        let text = response.text().await.unwrap();
        assert!(text.contains(allowed), "{path}: {text}");
    }

    // History paging: strictly older than `before`, newest first.
    let all = client.history(None, 0).await.unwrap().entries;
    assert_eq!(all.len(), 4);
    let page = client
        .history_page(Some("org/one"), Some(all[1].ts()), 10)
        .await
        .unwrap()
        .entries;
    assert_eq!(page.len(), 2);
    assert!(page.iter().all(|e| e.ts() < all[1].ts()));
    assert_eq!(page[0].ts(), all[2].ts());
    let response = get("/history?before=yesterday").await.unwrap();
    assert_eq!(response.status().as_u16(), 400);
    handle.shutdown().await;
}

// ------------------------------------------- live config reload (D1/D2)

impl Env {
    /// Replace the config file the way editors do (temp file + rename), so
    /// the daemon's watcher never observes a half-written file.
    fn rewrite_config(&self, content: &str) {
        let tmp = self.config.with_extension("yml.tmp");
        write_file(&tmp, content);
        std::fs::rename(&tmp, &self.config).unwrap();
    }

    fn config_for(&self, repos: &[&str], extra: &str) -> String {
        local_config(repos, &self.agent_path, extra)
    }
}

/// True when `/status` lists `key` (the daemon is reachable).
async fn status_lists(client: &DaemonClient, key: &str) -> bool {
    client
        .get_status()
        .await
        .map(|s| s.prs.iter().any(|p| p.key == key))
        .unwrap_or(false)
}

#[tokio::test]
async fn config_reload_adds_and_removes_repos_without_restart() {
    let _env = env_guard();
    let env = Env::new("ipc-reload-repos");
    env.configure(&["org/one"], "", &[]);
    // PRs created just now: inside their cooldowns, so nothing is reviewed.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, &now)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/two",
        &[pr_json(5, "Two", "bob", "f/y", "bbbb", false, &now)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#1"),
        Duration::from_secs(5),
        "startup poll to discover org/one#1",
    )
    .await;
    assert!(!status_lists(&client, "org/two#5").await);

    // Adding a repo: the watcher alone (no review-now) picks it up.
    env.rewrite_config(&env.config_for(&["org/one", "org/two"], ""));
    wait_for(
        || status_lists(&client, "org/two#5"),
        Duration::from_secs(5),
        "the added repo's PR to appear in /status",
    )
    .await;
    let status = client.get_status().await.unwrap();
    assert!(status.last_poll.contains_key("org/two"));
    assert!(status.config_error.is_none());
    assert_eq!(handle.shared.live().repos.len(), 2);

    // Removing it: unlisted from /status, state record retained.
    env.rewrite_config(&env.config_for(&["org/one"], ""));
    wait_for(
        || async { !status_lists(&client, "org/two#5").await },
        Duration::from_secs(5),
        "the removed repo's PR to leave /status",
    )
    .await;
    let status = client.get_status().await.unwrap();
    assert!(status.prs.iter().any(|p| p.key == "org/one#1"));
    assert!(!status.last_poll.contains_key("org/two"));
    assert!(
        env.state().prs.contains_key("org/two#5"),
        "removed repos keep their state records"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn config_reload_discards_removed_repos_queued_reviews() {
    let _env = env_guard();
    let env = Env::new("ipc-reload-queue");
    env.configure(
        &["org/one", "org/two"],
        COOLDOWN_ZERO,
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            // Long enough that the queue behind the one running review
            // (max_concurrent_reviews = 1) is still waiting at reload time.
            ("FAKE_AGENT_SLEEP", "3"),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/two",
        &[
            pr_json(1, "Two-A", "bob", "f/y", "bbbb", false, OLD),
            pr_json(2, "Two-B", "bob", "f/z", "cccc", false, OLD),
        ],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    let shared = handle.shared.clone();

    // One review runs, the rest queue; at least one org/two review waits.
    wait_for(
        || async {
            let (_, in_flight) = shared.queue_stats().await;
            in_flight == 1
                && shared
                    .queued_keys()
                    .await
                    .iter()
                    .any(|k| k.starts_with("org/two#"))
        },
        Duration::from_secs(5),
        "a running review with org/two reviews queued behind it",
    )
    .await;
    let discarded: Vec<String> = shared
        .queued_keys()
        .await
        .into_iter()
        .filter(|k| k.starts_with("org/two#"))
        .collect();

    // Remove org/two, then "Refresh now": review-now implies a reload check.
    env.rewrite_config(&env.config_for(&["org/one"], COOLDOWN_ZERO));
    client.review_now().await.unwrap();
    wait_for(
        || async {
            !shared
                .queued_keys()
                .await
                .iter()
                .any(|k| k.starts_with("org/two#"))
        },
        Duration::from_secs(2),
        "org/two's queued reviews to be discarded",
    )
    .await;
    assert!(!status_lists(&client, "org/two#1").await);

    // Let the running review finish: the discarded ones never run.
    wait_for(
        || async { shared.queue_stats().await == (0, 0) },
        Duration::from_secs(10),
        "the queue to drain",
    )
    .await;
    let state = env.state();
    for key in &discarded {
        let rec = &state.prs[key];
        assert!(rec.last_review_at.is_none(), "{key} was reviewed anyway");
        assert_eq!(rec.state, PrState::Pending, "{key} record retained");
    }
    handle.shutdown().await;
}

#[tokio::test]
async fn invalid_config_keeps_last_good_and_recovers_automatically() {
    let _env = env_guard();
    let env = Env::new("ipc-reload-lastgood");
    env.configure(&["org/one"], "", &[]);
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, &now)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/two",
        &[pr_json(5, "Two", "bob", "f/y", "bbbb", false, &now)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#1"),
        Duration::from_secs(5),
        "startup poll to discover org/one#1",
    )
    .await;
    assert!(client.get_status().await.unwrap().config_error.is_none());

    // Malformed YAML: the old repos keep serving, the error is surfaced.
    env.rewrite_config("defaults: [unclosed\n");
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.config_error.is_some())
        },
        Duration::from_secs(5),
        "the parse failure to surface as config_error",
    )
    .await;
    let status = client.get_status().await.unwrap();
    let error = status.config_error.expect("config_error set");
    assert!(error.message.contains("parse"), "{}", error.message);
    assert!(error.at <= chrono::Utc::now());
    assert!(status.prs.iter().any(|p| p.key == "org/one#1"));

    // Well-formed but failing validation: nothing applies, not even the
    // valid part (org/two), and the error reflects the new reason.
    env.rewrite_config(&env.config_for(&["org/one", "org/two", "not a repo"], ""));
    wait_for(
        || async {
            client.get_status().await.is_ok_and(|s| {
                s.config_error
                    .is_some_and(|e| e.message.contains("invalid configuration"))
            })
        },
        Duration::from_secs(5),
        "the validation failure to surface as config_error",
    )
    .await;
    // Give the scheduler a few ticks to (wrongly) poll org/two.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let status = client.get_status().await.unwrap();
    assert!(status.prs.iter().any(|p| p.key == "org/one#1"));
    assert!(!status.prs.iter().any(|p| p.key == "org/two#5"));
    assert_eq!(handle.shared.live().repos.len(), 1);

    // Fixing the file recovers without a restart and clears the error.
    env.rewrite_config(&env.config_for(&["org/one", "org/two"], ""));
    wait_for(
        || async {
            client.get_status().await.is_ok_and(|s| {
                s.config_error.is_none() && s.prs.iter().any(|p| p.key == "org/two#5")
            })
        },
        Duration::from_secs(5),
        "the fixed config to apply and clear config_error",
    )
    .await;
    handle.shutdown().await;
}

// ------------------------------------------- review-mode + schedule gates

/// GitHub writes the fake gh saw: `gh pr review …` (review bodies and
/// approvals) and `gh api …` (inline comments).
fn github_writes(gh_dir: &TempDir) -> usize {
    std::fs::read_to_string(gh_dir.child("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("pr review") || l.starts_with("api "))
        .count()
}

fn all_calls(gh_dir: &TempDir) -> usize {
    std::fs::read_to_string(gh_dir.child("calls.log"))
        .unwrap_or_default()
        .lines()
        .count()
}

fn completed_reviews(env: &Env, repo_name: &str) -> usize {
    env.history()
        .iter()
        .filter(|e| matches!(e, HistoryEntry::ReviewCompleted { repo, .. } if repo == repo_name))
        .count()
}

#[tokio::test]
async fn review_mode_off_stops_polling_and_reviews_but_serves_status() {
    let _env = env_guard();
    let env = Env::new("ipc-mode-off");
    env.configure(
        &["org/one"],
        "",
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    // Created just now: cooling down, so nothing reviews before the switch.
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, &now)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#1"),
        Duration::from_secs(5),
        "startup poll to discover org/one#1",
    )
    .await;
    assert_eq!(
        client.get_status().await.unwrap().review_mode,
        ReviewMode::ReviewComment
    );

    // Switch to off through the live reload path.
    env.rewrite_config(&env.config_for(&["org/one"], "  review_mode: off\n"));
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.review_mode == ReviewMode::Off)
        },
        Duration::from_secs(5),
        "the off mode to apply",
    )
    .await;
    // Let a poll that raced the swap finish before taking the baseline.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let calls = all_calls(&env.gh_dir);
    let baseline = client.get_status().await.unwrap();
    let state_poll = env.state().last_poll;

    // Forced fires: review-now, a forced single review, a forced "all".
    client.review_now().await.unwrap();
    let resp = client.review(Some("org/one#1"), false, true).await.unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");
    assert!(resp.note.unwrap_or_default().contains("off"));
    let resp = client.review(None, true, true).await.unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");

    // Well past the ~0.9s poll interval: no gh call of any kind, no queue,
    // and the poll timestamps stand still — yet status keeps serving.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(all_calls(&env.gh_dir), calls, "off must not call gh");
    let status = client.get_status().await.unwrap();
    assert_eq!(status.review_mode, ReviewMode::Off);
    assert_eq!(status.last_poll, baseline.last_poll);
    assert_eq!(status.last_state_poll, baseline.last_state_poll);
    assert_eq!(env.state().last_poll, state_poll);
    assert_eq!((status.queue_depth, status.in_flight), (0, 0));
    assert!(status.prs.iter().any(|p| p.key == "org/one#1"));
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);
    assert!(env.history().is_empty());

    // The human Approve is not gated by the mode.
    client.approve("org/one#1").await.unwrap();
    assert_eq!(count_calls(&env.gh_dir, "--approve"), 1);

    // Back to review comment: polling resumes.
    env.rewrite_config(&env.config_for(&["org/one"], ""));
    wait_for(
        || async {
            client.get_status().await.is_ok_and(|s| {
                s.review_mode == ReviewMode::ReviewComment
                    && s.last_poll.get("org/one") > baseline.last_poll.get("org/one")
            })
        },
        Duration::from_secs(5),
        "polling to resume after leaving off",
    )
    .await;
    handle.shutdown().await;
}

#[tokio::test]
async fn review_mode_monitor_records_reviews_without_github_writes() {
    let _env = env_guard();
    let env = Env::new("ipc-mode-monitor");
    env.configure(
        &["org/one"],
        &format!("{COOLDOWN_ZERO}  review_mode: monitor\n"),
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "the monitor-mode review to complete",
    )
    .await;

    // Verdict, counts, and history recorded; nothing written to GitHub
    // (the manifest's anchored nit would otherwise post inline).
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.last_verdict, Some(Verdict::Approved));
    assert_eq!(rec.last_reviewed_sha.as_deref(), Some("aaaa"));
    assert_eq!(rec.nit_count, 1);
    assert_eq!(completed_reviews(&env, "org/one"), 1);
    assert_eq!(github_writes(&env.gh_dir), 0);
    assert!(env.review_bodies().is_empty());
    let status = client.get_status().await.unwrap();
    assert_eq!(status.review_mode, ReviewMode::Monitor);
    assert_eq!(status.prs[0].state, PrState::ReviewedApproved);

    // The human Approve still posts under monitor.
    client.approve("org/one#1").await.unwrap();
    assert_eq!(count_calls(&env.gh_dir, "--approve"), 1);

    // Back to review comment (live reload): a re-review writes again.
    env.rewrite_config(&env.config_for(&["org/one"], COOLDOWN_ZERO));
    wait_for(
        || async { handle.shared.live().repos[0].review_mode == ReviewMode::ReviewComment },
        Duration::from_secs(5),
        "the review comment mode to apply",
    )
    .await;
    let resp = client.review(Some("org/one#1"), false, true).await.unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#1".to_string()]);
    wait_for(
        || async { completed_reviews(&env, "org/one") == 2 },
        Duration::from_secs(10),
        "the review-comment re-review to complete",
    )
    .await;
    assert_eq!(env.review_bodies().len(), 1);
    assert_eq!(count_calls(&env.gh_dir, "api "), 1, "inline comment posted");
    handle.shutdown().await;
}

/// Seconds the schedule test shifts the scheduler clock forward.
static CLOCK_OFFSET: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

fn shifted_clock() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
        + chrono::Duration::seconds(CLOCK_OFFSET.load(std::sync::atomic::Ordering::SeqCst))
}

#[tokio::test]
async fn schedule_defers_review_enqueue_to_the_cron_tick() {
    let _env = env_guard();
    CLOCK_OFFSET.store(0, std::sync::atomic::Ordering::SeqCst);
    let env = Env::new("ipc-schedule");
    env.configure(
        &["org/two"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    // org/one ticks once a year (Jan 1, 00:00 local); org/two has no
    // schedule and keeps the interval cadence.
    const SCHEDULE: &str = "0 0 1 1 *";
    env.rewrite_config(&format!(
        "{}  - name: org/one\n    schedule: \"{SCHEDULE}\"\n",
        env.config_for(&["org/two"], COOLDOWN_ZERO)
    ));
    for repo in ["org/one", "org/two"] {
        write_list_fixture(
            env.gh_dir.path(),
            repo,
            &[pr_json(1, "Due", "alice", "f/x", "aaaa", false, OLD)],
        );
    }
    let handle = env
        .start(SchedulerOptions {
            clock: shifted_clock,
            ..fast_opts()
        })
        .await;
    let client = env.client(&handle);

    // Unscheduled repo: reviewed on its first poll, as before D11.
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/two#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "the unscheduled repo's review",
    )
    .await;

    // Scheduled repo: state polling continues on poll_interval…
    let first = client.get_status().await.unwrap().last_poll["org/one"];
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.last_poll.get("org/one").is_some_and(|t| *t > first))
        },
        Duration::from_secs(5),
        "the scheduled repo to keep polling",
    )
    .await;
    // …and review-now polls too, but the eligible PR defers to the tick.
    client.review_now().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, PrState::Pending);
    assert!(rec.last_review_at.is_none());
    assert!(!handle
        .shared
        .queued_keys()
        .await
        .contains(&"org/one#1".to_string()));
    assert_eq!(completed_reviews(&env, "org/one"), 0);

    // Advance the scheduler clock just past the next tick: it enqueues there.
    let tick = daemon::next_schedule_tick(SCHEDULE, shifted_clock()).expect("a next tick");
    CLOCK_OFFSET.store(
        (tick - chrono::Utc::now()).num_seconds() + 60,
        std::sync::atomic::Ordering::SeqCst,
    );
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "the scheduled review at its tick",
    )
    .await;
    // One review per tick: the next tick is a year out.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(completed_reviews(&env, "org/one"), 1);
    assert_eq!(completed_reviews(&env, "org/two"), 1);
    CLOCK_OFFSET.store(0, std::sync::atomic::Ordering::SeqCst);
    handle.shutdown().await;
}

// ---------------------------------------------- IPC config surface (4.1)

fn fresh_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn file_mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn config_get_and_valid_put_persist_and_apply_live() {
    let _env = env_guard();
    let env = Env::new("ipc-config-put");
    env.configure(&["org/one"], "", &[]);
    let now = fresh_now();
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, &now)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/two",
        &[pr_json(5, "Two", "bob", "f/y", "bbbb", false, &now)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    let got = client.get_config().await.unwrap();
    assert_eq!(got.raw.repos.len(), 1);
    assert!(got.yaml.as_deref().unwrap().contains("org/one"));
    assert_eq!(got.effective.repos[0].settings.name, "org/one");
    assert_eq!(
        got.effective.repos[0].sources["poll_interval_minutes"],
        prx::config::ValueSource::Defaults
    );

    // Structured write: add a repo with its own poll interval.
    let mut cfg = got.raw.clone();
    cfg.repos.push(prx::config::RepoConfig {
        name: "org/two".into(),
        overrides: prx::config::PartialOverrides {
            poll_interval_minutes: Some(30),
            ..Default::default()
        },
    });
    let resp = client.put_config(&cfg).await.unwrap();
    assert!(resp.ok);
    assert_eq!(resp.added, vec!["org/two".to_string()]);
    assert_eq!(file_mode(&env.config), 0o600);
    assert!(env.load_config().has_repo("org/two"));
    wait_for(
        || status_lists(&client, "org/two#5"),
        Duration::from_secs(5),
        "the PUT-added repo to be tracked live",
    )
    .await;
    let got = client.get_config().await.unwrap();
    let two = got
        .effective
        .repos
        .iter()
        .find(|r| r.settings.name == "org/two")
        .unwrap();
    assert_eq!(two.settings.poll_interval_minutes, 30);
    assert_eq!(
        two.sources["poll_interval_minutes"],
        prx::config::ValueSource::Repo
    );

    // Raw YAML write is persisted verbatim (comments survive).
    let yaml = format!("# keep me\n{}", env.config_for(&["org/one"], ""));
    let resp = client.put_config_yaml(&yaml).await.unwrap();
    assert_eq!(resp.removed, vec!["org/two".to_string()]);
    assert_eq!(std::fs::read_to_string(&env.config).unwrap(), yaml);
    assert_eq!(handle.shared.live().repos.len(), 1);
    assert!(client.get_status().await.unwrap().config_error.is_none());
    handle.shutdown().await;
}

/// Design D1: the settings app's structured PUT /config round-trips an
/// `llm_api` engine and an org-level `engine` override, saved in the
/// engines form even though the running config was written with the
/// legacy `agents:`/`agent` keys.
#[tokio::test]
async fn settings_put_round_trips_llm_engine_and_org_engine_override() {
    let _env = env_guard();
    let env = Env::new("ipc-config-engines");
    env.configure(&["org/one"], "", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    let got = client.get_config().await.unwrap();
    // The legacy `agents: {fake: ...}` / `defaults.agent` config loads as
    // engines.
    assert_eq!(got.raw.defaults.engine, "fake");
    assert_eq!(got.raw.engines["fake"].kind(), "agent");

    let mut draft = serde_json::to_value(&got.raw).unwrap();
    draft["engines"]["cheap"] = serde_json::json!({
        "type": "llm_api",
        "provider": "openai_compatible",
        "base_url": "http://localhost:11434/v1",
        "model": "llama3",
        "api_key_env": "PRX_TEST_LLM_KEY",
        "temperature": 0.2,
        "max_tokens": 4096
    });
    draft["orgs"] = serde_json::json!([{ "org": "org", "engine": "cheap" }]);
    let resp = client.put_config_value(draft).await.unwrap();
    assert!(resp.ok);

    let on_disk = std::fs::read_to_string(&env.config).unwrap();
    assert!(on_disk.contains("engines:"), "{on_disk}");
    assert!(!on_disk.contains("agents:"), "{on_disk}");
    assert!(on_disk.contains("type: llm_api"), "{on_disk}");
    assert!(
        on_disk.contains("api_key_env: PRX_TEST_LLM_KEY"),
        "{on_disk}"
    );

    let got = client.get_config().await.unwrap();
    let llm = got.raw.engines["cheap"].as_llm_api().unwrap();
    assert_eq!(llm.provider, prx::config::LlmProvider::OpenaiCompatible);
    assert_eq!(llm.base_url.as_deref(), Some("http://localhost:11434/v1"));
    assert_eq!(llm.temperature, Some(0.2));
    assert_eq!(llm.max_tokens, Some(4096));
    assert_eq!(got.raw.orgs[0].overrides.engine.as_deref(), Some("cheap"));
    let one = &got.effective.repos[0];
    assert_eq!(one.settings.engine, "cheap");
    assert_eq!(one.settings.resolved_engine.kind(), "llm_api");
    assert_eq!(one.sources["engine"], prx::config::ValueSource::Org);
    assert_eq!(got.effective.orgs[0].settings.engine, "cheap");

    // An llm_api entry carrying its key inline is rejected, naming the field.
    let mut draft = serde_json::to_value(&got.raw).unwrap();
    draft["engines"]["cheap"]["api_key"] = "sk-not-allowed".into();
    let err = client.put_config_value(draft).await.unwrap_err();
    let rejection = err
        .downcast_ref::<prx::ipc::ConfigRejection>()
        .expect("a 422 rejection");
    assert_eq!(
        rejection.field.as_deref(),
        Some("engines.cheap.api_key"),
        "{rejection:?}"
    );
    handle.shutdown().await;
}

#[tokio::test]
async fn invalid_config_put_is_rejected_and_changes_nothing() {
    let _env = env_guard();
    let env = Env::new("ipc-config-reject");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            &fresh_now(),
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#1"),
        Duration::from_secs(5),
        "startup poll",
    )
    .await;
    let before = std::fs::read(&env.config).unwrap();

    // Invalid value + a repo removal: neither may land.
    let mut cfg = client.get_config().await.unwrap().raw;
    cfg.defaults.poll_interval_minutes = 0;
    cfg.repos.clear();
    let err = client.put_config(&cfg).await.unwrap_err();
    let rejection = err
        .downcast_ref::<prx::ipc::ConfigRejection>()
        .expect("a structured rejection");
    assert_eq!(
        rejection.field.as_deref(),
        Some("defaults.poll_interval_minutes")
    );

    // Type error through the raw path names the field too.
    let bad = env
        .config_for(&["org/one"], "")
        .replace("poll_interval_minutes: 15", "poll_interval_minutes: soon");
    let err = client.put_config_yaml(&bad).await.unwrap_err();
    let rejection = err.downcast_ref::<prx::ipc::ConfigRejection>().unwrap();
    assert_eq!(
        rejection.field.as_deref(),
        Some("defaults.poll_interval_minutes")
    );

    assert_eq!(std::fs::read(&env.config).unwrap(), before);
    let live = handle.shared.live();
    assert_eq!(live.config.defaults.poll_interval_minutes, 15);
    assert_eq!(live.repos.len(), 1);
    // Give the watcher a few ticks: still the same, still healthy.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let status = client.get_status().await.unwrap();
    assert!(status.config_error.is_none());
    assert!(status.prs.iter().any(|p| p.key == "org/one#1"));
    handle.shutdown().await;
}

#[tokio::test]
async fn review_mode_change_applies_now_and_survives_restart() {
    let _env = env_guard();
    let env = Env::new("ipc-review-mode");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    // Let the startup poll pass finish (a pass already running completes).
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.last_poll.contains_key("org/one"))
        },
        Duration::from_secs(5),
        "the startup poll",
    )
    .await;

    let resp = client.set_review_mode(ReviewMode::Off).await.unwrap();
    assert!(resp.changed && resp.persisted, "{resp:?}");
    assert_eq!(
        client.get_status().await.unwrap().review_mode,
        ReviewMode::Off
    );
    assert_eq!(env.load_config().defaults.review_mode, ReviewMode::Off);
    assert_eq!(file_mode(&env.config), 0o600);
    // Polling stops immediately: no gh calls over several poll intervals.
    let calls = all_calls(&env.gh_dir);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(all_calls(&env.gh_dir), calls);

    let again = client.set_review_mode(ReviewMode::Off).await.unwrap();
    assert!(!again.changed, "{again:?}");
    handle.shutdown().await;

    // Persisted: a restarted daemon comes up in mode off.
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    assert_eq!(
        client.get_status().await.unwrap().review_mode,
        ReviewMode::Off
    );
    let resp = client.set_review_mode(ReviewMode::Monitor).await.unwrap();
    assert!(resp.changed);
    assert_eq!(env.load_config().defaults.review_mode, ReviewMode::Monitor);
    handle.shutdown().await;
}

// ------------------------------------------------- block / unblock (4.2)

async fn tracked_pending(env: &Env, client: &DaemonClient, key: &str) {
    let _ = env;
    wait_for(
        || status_lists(client, key),
        Duration::from_secs(5),
        "the PR to be tracked",
    )
    .await;
}

#[tokio::test]
async fn block_posts_request_changes_with_configured_message_and_transitions() {
    let _env = env_guard();
    let env = Env::new("ipc-block");
    env.configure(
        &["org/one"],
        "  block_message: \"Hold: needs security review\"\n",
        &[],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            &fresh_now(),
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    tracked_pending(&env, &client, "org/one#1").await;

    let resp = client.block("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert!(
        resp.note
            .as_deref()
            .unwrap_or_default()
            .contains("ReviewedChangesRequested"),
        "{resp:?}"
    );
    assert!(count_calls(&env.gh_dir, "--request-changes") >= 1);
    let body = std::fs::read_to_string(env.gh_dir.child("org_one#1.review-body.txt")).unwrap();
    assert_eq!(body, "Hold: needs security review");
    assert_eq!(
        env.state().prs["org/one#1"].state,
        PrState::ReviewedChangesRequested
    );
    // The gh account resolved by the block is the payload's identity, so
    // the menu can now offer Unblock.
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs[0].identity_login.as_deref(), Some("testuser"));
    assert!(status.prs[0].offers_unblock());

    // Untracked PRs post cleanly with no state entry.
    let resp = client.block("org/one#77").await.unwrap();
    assert!(resp.changed);
    assert!(!env.state().prs.contains_key("org/one#77"));
    handle.shutdown().await;
}

#[tokio::test]
async fn unblock_dismisses_latest_own_review_or_reports_nothing() {
    let _env = env_guard();
    let env = Env::new("ipc-unblock");
    env.configure(&["org/one"], "  identity:\n    login: prx-bot\n", &[]);
    let now = fresh_now();
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[
            pr_json(1, "One", "alice", "f/x", "aaaa", false, &now),
            pr_json(2, "Two", "alice", "f/y", "bbbb", false, &now),
        ],
    );
    write_file(
        &env.gh_dir.child("org_one#1.reviews.jsonl"),
        concat!(
            "{\"id\":10,\"state\":\"CHANGES_REQUESTED\",\"login\":\"prx-bot\"}\n",
            "{\"id\":11,\"state\":\"CHANGES_REQUESTED\",\"login\":\"someone\"}\n",
            "{\"id\":12,\"state\":\"CHANGES_REQUESTED\",\"login\":\"PRX-bot\"}\n",
            "{\"id\":13,\"state\":\"COMMENTED\",\"login\":\"prx-bot\"}\n",
        ),
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    tracked_pending(&env, &client, "org/one#1").await;
    let status = client.get_status().await.unwrap();
    assert!(status
        .prs
        .iter()
        .all(|p| p.identity_login.as_deref() == Some("prx-bot")));
    let polled_before = status.last_poll["org/one"];

    let resp = client.unblock("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert!(env.gh_dir.child("org_one#1.dismissed-12").exists());
    assert!(!env.gh_dir.child("org_one#1.dismissed-10").exists());
    assert!(!env.gh_dir.child("org_one#1.dismissed-11").exists());
    // Re-sync: a state poll follows the dismissal.
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.last_poll["org/one"] > polled_before)
        },
        Duration::from_secs(5),
        "a re-sync poll after unblock",
    )
    .await;

    let resp = client.unblock("org/one#2").await.unwrap();
    assert!(!resp.changed, "{resp:?}");
    assert!(
        resp.note
            .as_deref()
            .unwrap_or_default()
            .contains("nothing to dismiss"),
        "{resp:?}"
    );
    assert_eq!(count_calls(&env.gh_dir, "dismissals"), 1);
    handle.shutdown().await;
}

#[tokio::test]
async fn block_and_unblock_work_while_mode_is_off() {
    let _env = env_guard();
    let env = Env::new("ipc-block-off");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            &fresh_now(),
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    tracked_pending(&env, &client, "org/one#1").await;
    client.set_review_mode(ReviewMode::Off).await.unwrap();

    let resp = client.block("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert_eq!(
        env.state().prs["org/one#1"].state,
        PrState::ReviewedChangesRequested
    );
    write_file(
        &env.gh_dir.child("org_one#1.reviews.jsonl"),
        "{\"id\":7,\"state\":\"CHANGES_REQUESTED\",\"login\":\"testuser\"}\n",
    );
    let resp = client.unblock("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert!(env.gh_dir.child("org_one#1.dismissed-7").exists());
    handle.shutdown().await;
}

// ---------------------------------------------- ephemeral prompt (4.3)

#[tokio::test]
async fn extra_prompt_shapes_one_run_and_is_never_persisted() {
    let _env = env_guard();
    let env = Env::new("ipc-extra-prompt");
    let prompt_out = env.gh_dir.child("prompt.txt");
    let manifest = approved_manifest();
    // A persisted extra_prompt (design F4) is configured alongside.
    env.configure(
        &["org/one"],
        "  extra_prompt: CONFIGURED-EXTRA check migrations\n",
        &[
            ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
            ("FAKE_AGENT_PROMPT_OUT", prompt_out.to_str().unwrap()),
        ],
    );
    // Fresh PR: inside its cooldown, so only the forced reviews run.
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            &fresh_now(),
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#1"),
        Duration::from_secs(5),
        "the PR to be tracked",
    )
    .await;

    const MARK: &str = "EPHEMERAL-XYZ focus on tenant isolation";
    let resp = client
        .review_with_prompt("org/one#1", true, MARK)
        .await
        .unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#1".to_string()]);
    // Idle too: the job stays in flight through its executive-summary step
    // (design D8), and an in-flight PR is not re-queued.
    wait_completed_and_idle(&env, &client, 1, "the prompted review").await;
    let prompt = std::fs::read_to_string(&prompt_out).unwrap();
    // Order (design F4): prompt → configured extra → ephemeral extra.
    let base = prompt.find("BASE-PROMPT").expect(&prompt);
    let configured = prompt.find("CONFIGURED-EXTRA").expect(&prompt);
    let ephemeral = prompt.find(MARK).expect(&prompt);
    assert!(base < configured && configured < ephemeral, "{prompt}");

    // Nothing about it reaches state, history, or config.
    let persisted = [
        env.data_dir.child("state.json"),
        env.data_dir.child("history.jsonl"),
        env.config.clone(),
    ];
    for path in &persisted {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            !text.contains("EPHEMERAL-XYZ"),
            "{} leaked it",
            path.display()
        );
    }

    // The next (plain) re-review renders without it.
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_for(
        || async { completed_reviews(&env, "org/one") == 2 },
        Duration::from_secs(10),
        "the plain re-review",
    )
    .await;
    let prompt = std::fs::read_to_string(&prompt_out).unwrap();
    assert!(prompt.contains("BASE-PROMPT"), "{prompt}");
    assert!(prompt.contains("CONFIGURED-EXTRA"), "{prompt}");
    assert!(!prompt.contains("EPHEMERAL-XYZ"), "{prompt}");
    handle.shutdown().await;
}

/// Task 8.3 / design F4–F5: the settings form's PUT payload (GET /config
/// `raw` with edits, as untyped JSON) sets an inline review prompt and an
/// extra prompt; the next review's agent receives both, in order.
#[tokio::test]
async fn form_edited_review_and_extra_prompts_reach_the_agent() {
    let _env = env_guard();
    let env = Env::new("ipc-form-prompts");
    let prompt_out = env.gh_dir.child("prompt.txt");
    let manifest = approved_manifest();
    env.configure(
        &["org/one"],
        "",
        &[
            ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
            ("FAKE_AGENT_PROMPT_OUT", prompt_out.to_str().unwrap()),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            &fresh_now(),
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#1"),
        Duration::from_secs(5),
        "the PR to be tracked",
    )
    .await;

    let got = client.get_config().await.unwrap();
    assert_eq!(
        got.shipped_review_prompt,
        prx::prompt::DEFAULT_PROMPT,
        "GET /config carries the shipped prompt for the form"
    );
    // Design C2: the always-appended output contract, with a sample path,
    // for the editor's read-only preview.
    assert_eq!(got.review_contract, prx::prompt::contract_preview());
    assert!(got.review_contract.contains("\"manifest_version\": 1"));
    assert!(got.review_contract.contains("\"brief\""));
    assert!(got
        .review_contract
        .contains(prx::prompt::SAMPLE_MANIFEST_PATH));
    assert!(!got.review_contract.contains("{{MANIFEST_PATH}}"));
    assert!(!got.shipped_review_prompt.contains("manifest_version"));
    let mut draft = serde_json::to_value(&got.raw).unwrap();
    draft["defaults"]["review_prompt"] =
        "FORM-PROMPT for {{REPO}} #{{PR_NUMBER}}\nWrite {{MANIFEST_PATH}}\n".into();
    draft["repos"][0]["extra_prompt"] = "FORM-EXTRA watch the auth paths".into();
    let resp = client.put_config_value(draft).await.unwrap();
    assert!(resp.ok);
    let on_disk = std::fs::read_to_string(&env.config).unwrap();
    assert!(on_disk.contains("FORM-PROMPT"), "{on_disk}");
    let got = client.get_config().await.unwrap();
    let one = &got.effective.repos[0];
    assert_eq!(
        one.settings.extra_prompt.as_deref(),
        Some("FORM-EXTRA watch the auth paths")
    );
    assert_eq!(one.sources["extra_prompt"], prx::config::ValueSource::Repo);
    assert_eq!(
        one.sources["review_prompt"],
        prx::config::ValueSource::Defaults
    );

    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_for(
        || async { completed_reviews(&env, "org/one") == 1 },
        Duration::from_secs(10),
        "the review after the form save",
    )
    .await;
    let prompt = std::fs::read_to_string(&prompt_out).unwrap();
    let base = prompt.find("FORM-PROMPT for org/one #1").expect(&prompt);
    let extra = prompt
        .find("FORM-EXTRA watch the auth paths")
        .expect(&prompt);
    assert!(base < extra, "{prompt}");
    // Design C1: the output contract follows the configured extra.
    let contract = prompt
        .find("## prx output contract (always added by prx)")
        .expect(&prompt);
    assert!(extra < contract, "{prompt}");
    assert!(!prompt.contains("BASE-PROMPT"), "{prompt}");
    assert!(!prompt.contains("{{"), "{prompt}");

    // A legacy path value is rejected with the actionable message.
    let mut draft = serde_json::to_value(&got.raw).unwrap();
    draft["defaults"]["review_prompt"] = "~/prompts/mine.md".into();
    let err = client
        .put_config_value(draft)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("paste the contents of ~/prompts/mine.md instead"),
        "{err}"
    );
    handle.shutdown().await;
}

// ------------------------------------- follow-up decisions (unblock, credential, mode)

/// `(token, args)` per fake-gh call, from the `calls.log.tokens` sidecar.
fn token_calls(gh_dir: &TempDir) -> Vec<(String, String)> {
    std::fs::read_to_string(gh_dir.child("calls.log.tokens"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(t, a)| (t.to_string(), a.to_string()))
        .collect()
}

/// GitHub writes: review bodies/approvals/blocks (`pr review`), inline
/// comments and review dismissals (`api …/comments`, `api …/dismissals`).
fn is_github_write(args: &str) -> bool {
    args.starts_with("pr review")
        || (args.starts_with("api ")
            && (args.contains("/comments") || args.contains("/dismissals")))
}

#[tokio::test]
async fn unblock_returns_blocked_pr_to_pending_and_reblocks_cleanly() {
    let _env = env_guard();
    let env = Env::new("ipc-unblock-pending");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            &fresh_now(),
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    tracked_pending(&env, &client, "org/one#1").await;

    client.block("org/one#1").await.unwrap();
    let status = client.get_status().await.unwrap();
    assert!(status.prs[0].offers_unblock());

    write_file(
        &env.gh_dir.child("org_one#1.reviews.jsonl"),
        "{\"id\":7,\"state\":\"CHANGES_REQUESTED\",\"login\":\"testuser\"}\n",
    );
    let resp = client.unblock("org/one#1").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert!(
        resp.note.as_deref().unwrap_or_default().contains("Pending"),
        "{resp:?}"
    );
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);
    // Right away (no poll needed): no longer blocked, the menu offers Block.
    let status = client.get_status().await.unwrap();
    assert_eq!(status.prs[0].state, PrState::Pending);
    assert!(!status.prs[0].offers_unblock());

    // GitHub now shows the review dismissed: a second unblock is a note and
    // leaves the state alone.
    write_file(
        &env.gh_dir.child("org_one#1.reviews.jsonl"),
        "{\"id\":7,\"state\":\"DISMISSED\",\"login\":\"testuser\"}\n",
    );
    let resp = client.unblock("org/one#1").await.unwrap();
    assert!(!resp.changed, "{resp:?}");
    assert!(
        resp.note
            .as_deref()
            .unwrap_or_default()
            .contains("nothing to dismiss"),
        "{resp:?}"
    );
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);

    // Untracked PRs still dismiss cleanly with no state entry.
    write_file(
        &env.gh_dir.child("org_one#77.reviews.jsonl"),
        "{\"id\":9,\"state\":\"CHANGES_REQUESTED\",\"login\":\"testuser\"}\n",
    );
    let resp = client.unblock("org/one#77").await.unwrap();
    assert!(resp.changed, "{resp:?}");
    assert!(env.gh_dir.child("org_one#77.dismissed-9").exists());
    assert!(!env.state().prs.contains_key("org/one#77"));

    // No credential configured: writes run exactly like reads (gh's own
    // account, whatever GH_TOKEN the environment has).
    let calls = token_calls(&env.gh_dir);
    let read_token = &calls
        .iter()
        .find(|(_, a)| a.starts_with("pr list"))
        .expect("a poll")
        .0;
    assert!(calls.iter().any(|(_, a)| is_github_write(a)));
    for (token, args) in calls.iter().filter(|(_, a)| is_github_write(a)) {
        assert_eq!(token, read_token, "{args}");
    }
    handle.shutdown().await;
}

#[tokio::test]
async fn identity_credential_is_used_for_every_github_write() {
    let _env = env_guard();
    let env = Env::new("ipc-credential-writes");
    let identity = "  identity:\n    credential: PRX_TEST_CRED_WORK\n";
    let blocking = manifest_json(
        "changes_requested",
        &[&finding_json(
            Some("src/a.rs"),
            Some(3),
            "blocking",
            "fix this",
        )],
    );
    env.configure(
        &["org/one"],
        &format!("{COOLDOWN_ZERO}{identity}"),
        &[("FAKE_AGENT_MANIFEST_JSON", blocking.as_str())],
    );
    std::env::set_var("PRX_TEST_CRED_WORK", "tok-work");
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    // Automated review: inline comment + request-changes review body.
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/one#1").map(|r| r.state),
                Some(PrState::ReviewedChangesRequested)
            )
        },
        Duration::from_secs(10),
        "the automated review to post",
    )
    .await;
    // Human writes: approve, block, unblock.
    client.approve("org/one#1").await.unwrap();
    client.block("org/one#1").await.unwrap();
    write_file(
        &env.gh_dir.child("org_one#1.reviews.jsonl"),
        "{\"id\":5,\"state\":\"CHANGES_REQUESTED\",\"login\":\"testuser\"}\n",
    );
    client.unblock("org/one#1").await.unwrap();
    handle.shutdown().await;
    std::env::remove_var("PRX_TEST_CRED_WORK");

    let calls = token_calls(&env.gh_dir);
    let writes: Vec<&(String, String)> = calls.iter().filter(|(_, a)| is_github_write(a)).collect();
    for needle in ["/comments", "--request-changes", "--approve", "/dismissals"] {
        assert!(
            writes.iter().any(|(_, a)| a.contains(needle)),
            "no {needle} write in {writes:?}"
        );
    }
    for (token, args) in &writes {
        assert_eq!(token, "tok-work", "write without the credential: {args}");
    }
    // Reads keep gh's default account.
    for (token, args) in calls.iter().filter(|(_, a)| a.starts_with("pr list")) {
        assert_ne!(token, "tok-work", "{args}");
    }
}

#[tokio::test]
async fn review_mode_applies_in_memory_while_the_config_file_is_broken() {
    let _env = env_guard();
    let env = Env::new("ipc-review-mode-broken");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.last_poll.contains_key("org/one"))
        },
        Duration::from_secs(5),
        "the startup poll",
    )
    .await;

    let broken = "defaults: [unclosed\n";
    env.rewrite_config(broken);
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.config_error.is_some())
        },
        Duration::from_secs(5),
        "the parse failure to surface",
    )
    .await;

    let resp = client.set_review_mode(ReviewMode::Off).await.unwrap();
    assert!(resp.ok && resp.changed && !resp.persisted, "{resp:?}");
    assert!(
        resp.note
            .as_deref()
            .unwrap_or_default()
            .contains("not saved"),
        "{resp:?}"
    );
    let status = client.get_status().await.unwrap();
    assert_eq!(status.review_mode, ReviewMode::Off);
    assert!(status.config_error.is_some(), "the file is still broken");
    assert_eq!(std::fs::read_to_string(&env.config).unwrap(), broken);

    // The 1 s hash-poll does not revert it while the file stays broken.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let status = client.get_status().await.unwrap();
    assert_eq!(status.review_mode, ReviewMode::Off);
    assert!(status.config_error.is_some());
    let again = client.set_review_mode(ReviewMode::Off).await.unwrap();
    assert!(!again.changed && !again.persisted, "{again:?}");

    // Fixing the file: the fixed file wins (its mode, review comment).
    env.rewrite_config(&env.config_for(&["org/one"], ""));
    wait_for(
        || async {
            client.get_status().await.is_ok_and(|s| {
                s.config_error.is_none() && s.review_mode == ReviewMode::ReviewComment
            })
        },
        Duration::from_secs(5),
        "the fixed file to apply",
    )
    .await;
    handle.shutdown().await;
}

// ------------------------------------------------------ pause (D5, 3.2/3.3)

/// `POST /pause` / `POST /resume` round-trips and their 400s: both or
/// neither of `hours`/`until`, a non-positive `hours`, a past `until`.
#[tokio::test]
async fn pause_and_resume_endpoints_round_trip_and_reject_bad_forms() {
    let _env = env_guard();
    let env = Env::new("ipc-pause-api");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    let status = client.get_status().await.unwrap();
    assert_eq!(status.paused_until, None);
    let json = serde_json::to_value(&status).unwrap();
    assert!(json.get("paused_until").is_none(), "{json}");

    let future = chrono::Utc::now() + chrono::Duration::hours(3);
    for (hours, until, needle) in [
        (None, None, "--hours N or --until"),
        (Some(1.0), Some(future), "not both"),
        (Some(0.0), None, "must be > 0"),
        (
            None,
            Some(chrono::Utc::now() - chrono::Duration::hours(1)),
            "not in the future",
        ),
    ] {
        let err = client.pause(hours, until).await.unwrap_err().to_string();
        assert!(err.contains("400") && err.contains(needle), "{err}");
    }
    assert_eq!(env.state().paused_until, None);

    // hours form.
    let resp = client.pause(Some(2.0), None).await.unwrap();
    assert!(resp.ok && resp.changed, "{resp:?}");
    let until = resp.paused_until.expect("pause end");
    let left = until - chrono::Utc::now();
    assert!(left > chrono::Duration::minutes(119) && left <= chrono::Duration::hours(2));
    assert_eq!(client.get_status().await.unwrap().paused_until, Some(until));
    assert_eq!(env.state().paused_until, Some(until));

    // until form replaces it.
    let resp = client.pause(None, Some(future)).await.unwrap();
    assert!(resp.changed);
    assert_eq!(resp.paused_until, Some(future));
    assert_eq!(
        client.get_status().await.unwrap().paused_until,
        Some(future)
    );

    let resp = client.resume().await.unwrap();
    assert!(
        resp.ok && resp.changed && resp.paused_until.is_none(),
        "{resp:?}"
    );
    assert_eq!(client.get_status().await.unwrap().paused_until, None);
    assert_eq!(env.state().paused_until, None);
    let resp = client.resume().await.unwrap();
    assert!(!resp.changed, "{resp:?}");
    handle.shutdown().await;
}

/// The 3.2 daemon scenario: pause, polls keep refreshing state while every
/// automatic origin (interval due-scan, review-now, review --all, a forced
/// schedule tick) enqueues nothing, an explicit single PR still runs, the
/// pause survives a restart, and resume brings back due-based reviews — but
/// not the schedule tick consumed while paused (no catch-up).
#[tokio::test]
async fn pause_gates_automatic_enqueue_keeps_polling_and_survives_restart() {
    let _env = env_guard();
    CLOCK_OFFSET.store(0, std::sync::atomic::Ordering::SeqCst);
    let env = Env::new("ipc-pause");
    env.configure(
        &["org/two"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    const SCHEDULE: &str = "0 0 1 1 *";
    env.rewrite_config(&format!(
        "{}  - name: org/one\n    schedule: \"{SCHEDULE}\"\n",
        env.config_for(&["org/two"], COOLDOWN_ZERO)
    ));
    for repo in ["org/one", "org/two"] {
        write_list_fixture(env.gh_dir.path(), repo, &[]);
    }
    let opts = SchedulerOptions {
        clock: shifted_clock,
        ..fast_opts()
    };
    let handle = env.start(opts).await;
    let client = env.client(&handle);
    wait_for(
        || async {
            client.get_status().await.is_ok_and(|s| {
                s.last_poll.contains_key("org/one") && s.last_poll.contains_key("org/two")
            })
        },
        Duration::from_secs(5),
        "the startup poll",
    )
    .await;

    // Long enough to outlast the shifted scheduler clock below.
    let resp = client.pause(Some(20_000.0), None).await.unwrap();
    assert!(resp.changed);
    let until = resp.paused_until.unwrap();

    // Due PRs appear: polling continues and tracks them, nothing enqueues.
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Due", "alice", "f/x", "aaaa", false, OLD)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/two",
        &[
            pr_json(1, "Due", "alice", "f/x", "aaaa", false, OLD),
            pr_json(2, "Explicit", "bob", "f/y", "bbbb", false, OLD),
        ],
    );
    wait_for(
        || async {
            status_lists(&client, "org/one#1").await && status_lists(&client, "org/two#1").await
        },
        Duration::from_secs(5),
        "paused polls to refresh state",
    )
    .await;

    // Forced fires: review-now, review --all (plain and forced).
    let resp = client.review_now().await.unwrap();
    assert!(resp.note.unwrap_or_default().contains("paused"));
    for force in [false, true] {
        let resp = client.review(None, true, force).await.unwrap();
        assert!(resp.enqueued.is_empty(), "{resp:?}");
        assert!(resp.note.unwrap_or_default().contains("paused"));
    }
    // A schedule tick arrives while paused: polled, consumed, not enqueued.
    let tick = daemon::next_schedule_tick(SCHEDULE, shifted_clock()).expect("a next tick");
    CLOCK_OFFSET.store(
        (tick - chrono::Utc::now()).num_seconds() + 60,
        std::sync::atomic::Ordering::SeqCst,
    );
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.last_poll.get("org/one").is_some_and(|t| *t >= tick))
        },
        Duration::from_secs(5),
        "a poll at the schedule tick",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(handle.shared.queued_keys().await.is_empty());
    assert_eq!(completed_reviews(&env, "org/one"), 0);
    assert_eq!(completed_reviews(&env, "org/two"), 0);
    for key in ["org/one#1", "org/two#1", "org/two#2"] {
        assert_eq!(env.state().prs[key].state, PrState::Pending, "{key}");
    }

    // An explicit single-PR review bypasses the pause, with a note.
    let resp = client.review(Some("org/two#2"), false, true).await.unwrap();
    assert_eq!(resp.enqueued, vec!["org/two#2".to_string()]);
    assert!(resp.note.unwrap_or_default().contains("runs anyway"));
    wait_for(
        || async { completed_reviews(&env, "org/two") == 1 },
        Duration::from_secs(10),
        "the explicit review to run while paused",
    )
    .await;
    handle.shutdown().await;

    // Restart: the pause survived and the startup poll enqueues nothing.
    assert_eq!(env.state().paused_until, Some(until));
    let handle = env.start(opts).await;
    let client = env.client(&handle);
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.last_poll.contains_key("org/two"))
        },
        Duration::from_secs(5),
        "the post-restart poll",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(client.get_status().await.unwrap().paused_until, Some(until));
    assert_eq!(env.state().prs["org/two#1"].state, PrState::Pending);
    assert_eq!(completed_reviews(&env, "org/two"), 1);

    // Resume: due-based reviews come back on the next poll; the scheduled
    // repo waits for its next tick (the one missed while paused is gone).
    let resp = client.resume().await.unwrap();
    assert!(resp.changed);
    assert_eq!(client.get_status().await.unwrap().paused_until, None);
    wait_for(
        || async {
            matches!(
                env.state().prs.get("org/two#1").map(|r| r.state),
                Some(PrState::ReviewedApproved)
            )
        },
        Duration::from_secs(10),
        "the due review after resume",
    )
    .await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);
    assert_eq!(completed_reviews(&env, "org/one"), 0);
    assert!(!client.resume().await.unwrap().changed);
    CLOCK_OFFSET.store(0, std::sync::atomic::Ordering::SeqCst);
    handle.shutdown().await;
}

// ---------------------------------------------------- brief + auto-open

fn blocking_manifest() -> String {
    manifest_json(
        "changes_requested",
        &[&finding_json(
            Some("src/a.rs"),
            Some(3),
            "blocking",
            "unscoped query",
        )],
    )
}

/// Wait until `n` reviews of org/one completed and the queue is idle (the
/// auto-open hook runs after the pipeline, before the job finishes).
async fn wait_completed_and_idle(env: &Env, client: &DaemonClient, n: usize, what: &str) {
    wait_for(
        || async { completed_reviews(env, "org/one") >= n },
        Duration::from_secs(15),
        what,
    )
    .await;
    wait_for(
        || async {
            let s = client.get_status().await.unwrap();
            s.in_flight == 0 && s.queue_depth == 0
        },
        Duration::from_secs(10),
        "the review queue to go idle",
    )
    .await;
}

/// The PR's stored brief text.
fn brief_text(env: &Env, key: &str) -> Option<String> {
    env.state().prs[key].brief.as_ref().map(|b| b.text.clone())
}

fn last_completed_summary(env: &Env) -> Option<String> {
    env.history()
        .into_iter()
        .find_map(|e| match e {
            HistoryEntry::ReviewCompleted { brief, .. } => Some(brief),
            _ => None,
        })
        .flatten()
}

/// Design B2/B5: without a manifest brief, the on-demand brief is stored
/// on the record and the history entry, surfaced with `html_url` in
/// `/status`, and rendered by a custom `{{EXEC_SUMMARY}}` (legacy alias)
/// template — the posted body carries the PR's stored brief (the new one
/// is generated after the posts).
#[tokio::test]
async fn on_demand_brief_is_stored_surfaced_and_templated() {
    let _env = env_guard();
    let env = Env::new("ipc-exec-summary");
    let prompt_out = env.data_dir.child("summary-prompt.txt");
    std::fs::write(
        env.gh_dir.child("org_one#1.brief.json"),
        r#"{"title":"Add feature","body":"Why: speed.","author":{"login":"alice"},"files":[{"path":"src/q.rs"}]}"#,
    )
    .unwrap();
    std::fs::write(
        env.gh_dir.child("org_one#1.diff"),
        "diff --git a/src/q.rs b/src/q.rs\n+fast path\n",
    )
    .unwrap();
    env.configure(
        &["org/one"],
        &format!(
            "{COOLDOWN_ZERO}  comments:\n    template: \"S=[{{{{EXEC_SUMMARY}}}}] {{{{VERDICT_LABEL}}}}\"\n"
        ),
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_AGENT_SUMMARY", "  First summary.  "),
            (
                "FAKE_AGENT_SUMMARY_PROMPT_OUT",
                prompt_out.to_str().unwrap(),
            ),
        ],
    );
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
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_completed_and_idle(&env, &client, 1, "the first review").await;

    // Stored on the record and the history entry (trimmed).
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(
        rec.brief.as_ref().map(|b| b.text.as_str()),
        Some("First summary.")
    );
    assert_eq!(
        last_completed_summary(&env).as_deref(),
        Some("First summary.")
    );
    // The built-in brief prompt saw the PR metadata and diff, not the
    // review.
    let prompt = std::fs::read_to_string(&prompt_out).unwrap();
    assert!(prompt.contains("org/one#1"), "{prompt}");
    assert!(prompt.contains("Title: Add feature"), "{prompt}");
    assert!(prompt.contains("- src/q.rs"), "{prompt}");
    assert!(prompt.contains("+fast path"), "{prompt}");
    assert!(!prompt.contains("fixture summary"), "{prompt}");
    let brief = env.state().prs["org/one#1"].brief.clone().unwrap();
    assert_eq!(brief.source, prx::state::BriefSource::OnDemand);
    assert_eq!(brief.head_sha, "aaaa");
    assert_eq!(brief.created_at, brief.updated_at);
    // The first posted body had no summary to render yet.
    let body_path = env.gh_dir.child("org_one#1.review-body.txt");
    let body = std::fs::read_to_string(&body_path).unwrap();
    assert_eq!(body, "S=[] approved — no blocking issues");

    // `/status` carries the summary and the PR URL.
    let status = client.get_status().await.unwrap();
    let entry = &status.prs[0];
    assert_eq!(
        entry.brief.as_ref().map(|b| b.text.as_str()),
        Some("First summary.")
    );
    assert_eq!(entry.html_url, "https://github.com/org/one/pull/1");
    let raw = serde_json::to_value(entry).unwrap();
    assert!(raw.get("exec_summary").is_none(), "{raw}");
    assert_eq!(raw["brief"]["text"], "First summary.");
    assert_eq!(raw["brief"]["stale"], false);
    assert_eq!(raw["brief"]["source"], "on_demand");
    assert_eq!(raw["brief_pending"], false);
    assert_eq!(raw["html_url"], "https://github.com/org/one/pull/1");

    // A re-review posts the stored summary and replaces it afterwards.
    std::env::set_var("FAKE_AGENT_SUMMARY", "Second summary.");
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_completed_and_idle(&env, &client, 2, "the re-review").await;
    let body = std::fs::read_to_string(&body_path).unwrap();
    assert_eq!(body, "S=[First summary.] approved — no blocking issues");
    assert_eq!(
        brief_text(&env, "org/one#1").as_deref(),
        Some("Second summary.")
    );
    assert_eq!(
        client.get_status().await.unwrap().prs[0]
            .brief
            .as_ref()
            .map(|b| b.text.as_str()),
        Some("Second summary.")
    );
    // Regenerating keeps created_at and moves updated_at.
    let second = env.state().prs["org/one#1"].brief.clone().unwrap();
    assert_eq!(second.created_at, brief.created_at);
    assert!(second.updated_at > brief.updated_at);
    handle.shutdown().await;
}

/// Design B2/B3 best-effort semantics: a failed review keeps the earlier
/// brief; a completed review whose brief engine errors or times out still
/// completes and posts, records no brief for that run, keeps the previous
/// brief, and reports the failure as `brief_error`.
#[tokio::test]
async fn brief_failures_never_fail_the_review_and_keep_the_old_brief() {
    let _env = env_guard();
    let env = Env::new("ipc-exec-summary-fail");
    env.configure(
        &["org/one"],
        &format!("{COOLDOWN_ZERO}  brief:\n    timeout_seconds: 1\n"),
        &[
            ("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str()),
            ("FAKE_AGENT_SUMMARY", "Kept summary."),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_completed_and_idle(&env, &client, 1, "the first review").await;
    assert_eq!(
        brief_text(&env, "org/one#1").as_deref(),
        Some("Kept summary.")
    );

    // A failed re-review (agent exits non-zero, twice) keeps the summary.
    std::env::set_var("FAKE_AGENT_EXIT", "1");
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_for(
        || async {
            env.history()
                .iter()
                .any(|e| matches!(e, HistoryEntry::ReviewFailed { .. }))
        },
        Duration::from_secs(10),
        "the failed re-review",
    )
    .await;
    wait_completed_and_idle(&env, &client, 1, "the queue after the failure").await;
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::Pending);
    assert_eq!(
        rec.brief.as_ref().map(|b| b.text.as_str()),
        Some("Kept summary.")
    );
    assert_eq!(
        client.get_status().await.unwrap().prs[0]
            .brief
            .as_ref()
            .map(|b| b.text.as_str()),
        Some("Kept summary.")
    );

    // The brief engine errors: the review completes, posts, and
    // transitions; no brief is recorded for that run and the old one stays.
    std::env::set_var("FAKE_AGENT_SUMMARY_EXIT", "2");
    std::env::remove_var("FAKE_AGENT_EXIT");
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_completed_and_idle(&env, &client, 2, "the review with a failing summary").await;
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::ReviewedChangesRequested);
    assert_eq!(
        rec.brief.as_ref().map(|b| b.text.as_str()),
        Some("Kept summary.")
    );
    assert_eq!(last_completed_summary(&env), None);
    assert_eq!(env.review_bodies().len(), 1, "the review body was posted");
    let entry = client.get_status().await.unwrap().prs[0].clone();
    assert_eq!(
        entry.brief.as_ref().map(|b| b.text.as_str()),
        Some("Kept summary.")
    );
    assert!(!entry.brief_pending);
    assert!(entry.brief_error.is_some(), "{entry:?}");

    // A summary engine past `timeout_seconds` fails the same way.
    std::env::remove_var("FAKE_AGENT_SUMMARY_EXIT");
    std::env::set_var("FAKE_AGENT_SUMMARY_SLEEP", "4");
    let started = Instant::now();
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_completed_and_idle(&env, &client, 3, "the review with a slow summary").await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "timeout enforced"
    );
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::ReviewedChangesRequested);
    assert_eq!(
        rec.brief.as_ref().map(|b| b.text.as_str()),
        Some("Kept summary.")
    );
    let error = client.get_status().await.unwrap().prs[0]
        .brief_error
        .clone();
    assert!(
        error.is_some_and(|e| e.contains("timed out")),
        "timeout reported"
    );
    handle.shutdown().await;
}

/// Design B2 ordering: a slow on-demand brief engine delays nothing. The
/// review's completion — state transition, history entry (written after its
/// completion notification, in the same effects pass), and auto-open — is
/// committed while the summary is still generating, the state lock is free
/// meanwhile (a mutation answers at once), and the summary attaches to the
/// record and that same history entry afterwards.
#[tokio::test]
async fn slow_brief_does_not_delay_completion_or_hold_the_state_lock() {
    let _env = env_guard();
    let env = Env::new("ipc-exec-summary-slow");
    env.configure(
        &["org/one"],
        &format!(
            "{COOLDOWN_ZERO}  auto_open: approved\n  browser: [\"mybrowser\", \"{{{{URL}}}}\"]\n  brief:\n    timeout_seconds: 30\n"
        ),
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_AGENT_SUMMARY", "Slow summary."),
            ("FAKE_AGENT_SUMMARY_SLEEP", "4"),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async { completed_reviews(&env, "org/one") >= 1 },
        Duration::from_secs(15),
        "the review's completion entry",
    )
    .await;
    let completed_at = Instant::now();

    // Committed before the summary: transition, history, auto-open.
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::ReviewedApproved);
    assert_eq!(rec.brief, None, "the brief is still generating");
    assert_eq!(last_completed_summary(&env), None);
    assert_eq!(env.opened().len(), 1, "auto-open ran before the summary");
    let status = client.get_status().await.unwrap();
    assert_eq!(status.in_flight, 1, "the brief step is still running");
    assert!(
        status.prs[0].brief_pending,
        "brief_pending while generating"
    );
    assert_eq!(status.prs[0].state, PrState::ReviewedApproved);

    // The state lock is free while the summary engine sleeps.
    let started = Instant::now();
    client.pause(Some(1.0), None).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a state mutation waited {:?} on the summary",
        started.elapsed()
    );
    client.resume().await.unwrap();
    assert!(
        completed_at.elapsed() < Duration::from_secs(3),
        "the checks above ran while the summary was pending"
    );

    // Then the summary attaches to the record and the same history entry.
    wait_completed_and_idle(&env, &client, 1, "the summary step").await;
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(
        rec.brief.as_ref().map(|b| b.text.as_str()),
        Some("Slow summary.")
    );
    assert_eq!(
        last_completed_summary(&env).as_deref(),
        Some("Slow summary.")
    );
    assert_eq!(completed_reviews(&env, "org/one"), 1, "updated in place");
    assert_eq!(
        client.get_status().await.unwrap().prs[0]
            .brief
            .as_ref()
            .map(|b| b.text.as_str()),
        Some("Slow summary.")
    );
    handle.shutdown().await;
}

/// Design B4 (task 3.4): `POST /brief` answers 202 and generates the
/// brief in the background (`brief_pending` meanwhile), never posting to
/// GitHub, even for a never-reviewed PR; a second request while one runs
/// gets 409 and only one engine run happens; an unknown PR is 404 and an
/// `off` repo is 422.
#[tokio::test]
async fn post_brief_accepts_once_and_rejects_unknown_off_and_in_progress() {
    let _env = env_guard();
    let env = Env::new("ipc-post-brief");
    let runs_log = env.data_dir.child("brief-runs.log");
    env.configure(
        &["org/one", "org/off"],
        "",
        &[
            ("FAKE_AGENT_SUMMARY", "Adds a login rate limiter."),
            ("FAKE_AGENT_SUMMARY_SLEEP", "1"),
            ("FAKE_AGENT_SUMMARY_RUNS_LOG", runs_log.to_str().unwrap()),
        ],
    );
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "  - name: org/off\n",
        "  - name: org/off\n    review_mode: off\n",
        1,
    );
    write_file(&env.config, &config);
    // Brand new PRs: the default cooldown keeps them unreviewed.
    let now = chrono::Utc::now().to_rfc3339();
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, &now)],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/off",
        &[pr_json(2, "Off", "alice", "f/y", "bbbb", false, &now)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || async { env.state().prs.contains_key("org/one#1") },
        Duration::from_secs(10),
        "org/one#1 to be tracked",
    )
    .await;
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);

    let http = reqwest::Client::new();
    let post = |key: &str| {
        http.post(format!("http://127.0.0.1:{}/brief", handle.port))
            .bearer_auth(&handle.token)
            .json(&serde_json::json!({ "key": key }))
            .send()
    };
    // Two concurrent requests for the same PR: one accepted, one 409.
    let (a, b) = tokio::join!(post("org/one#1"), post("org/one#1"));
    let mut codes = [a.unwrap().status().as_u16(), b.unwrap().status().as_u16()];
    codes.sort();
    assert_eq!(codes, [202, 409]);
    let status = client.get_status().await.unwrap();
    let entry = status.prs.iter().find(|e| e.key == "org/one#1").unwrap();
    assert!(entry.brief_pending, "pending while generating");
    // The client surfaces the 409 as an error naming the cause.
    let err = client.brief("org/one#1").await.unwrap_err().to_string();
    assert!(err.contains("already being written"), "{err}");

    // Unknown PR (untracked number, unwatched repo) → 404; off repo → 422.
    let resp = post("org/one#99").await.unwrap();
    assert_eq!(resp.status().as_u16(), 404);
    assert_eq!(post("nope/x#1").await.unwrap().status().as_u16(), 404);
    let resp = post("org/off#2").await.unwrap();
    assert_eq!(resp.status().as_u16(), 422);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("off"), "{body}");

    wait_for(
        || async {
            let s = client.get_status().await.unwrap();
            s.prs
                .iter()
                .any(|e| e.key == "org/one#1" && !e.brief_pending && e.brief.is_some())
        },
        Duration::from_secs(10),
        "the brief to land",
    )
    .await;
    let entry = client
        .get_status()
        .await
        .unwrap()
        .prs
        .into_iter()
        .find(|e| e.key == "org/one#1")
        .unwrap();
    let brief = entry.brief.unwrap();
    assert_eq!(brief.text, "Adds a login rate limiter.");
    assert_eq!(brief.source, prx::state::BriefSource::OnDemand);
    assert!(!brief.stale);
    assert_eq!(entry.brief_error, None);
    assert_eq!(
        std::fs::read_to_string(&runs_log).unwrap().lines().count(),
        1,
        "exactly one engine run"
    );
    assert_eq!(github_writes(&env.gh_dir), 0, "nothing posted to GitHub");
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);
    assert!(env
        .history()
        .iter()
        .any(|e| matches!(e, HistoryEntry::Brief { brief: Some(_), .. })));

    // Accepted again once idle (regenerate): the CLI client path.
    let accepted = client.brief("org/one#1").await.unwrap();
    assert!(accepted.accepted);
    assert_eq!(accepted.key, "org/one#1");
    handle.shutdown().await;
}

/// Task 7.2: a matching verdict opens the PR's `html_url` through the
/// effective browser; a non-matching verdict does not.
#[tokio::test]
async fn auto_open_opens_matching_verdicts_in_the_configured_browser() {
    let _env = env_guard();
    let env = Env::new("ipc-auto-open");
    env.configure(
        &["org/one"],
        &format!(
            "{COOLDOWN_ZERO}  auto_open: blocking\n  browser: [\"mybrowser\", \"--new-tab\", \"{{{{URL}}}}\"]\n"
        ),
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_completed_and_idle(&env, &client, 1, "the blocking review").await;
    let url = "https://github.com/org/one/pull/1";
    assert_eq!(
        env.opened(),
        vec![vec![
            "mybrowser".to_string(),
            "--new-tab".to_string(),
            url.to_string()
        ]]
    );
    assert_eq!(client.get_status().await.unwrap().prs[0].html_url, url);

    // An approved verdict does not match `blocking`.
    std::env::set_var("FAKE_AGENT_MANIFEST_JSON", approved_manifest());
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_completed_and_idle(&env, &client, 2, "the approved re-review").await;
    assert_eq!(env.opened().len(), 1);

    // A failed run never opens.
    std::env::set_var("FAKE_AGENT_MANIFEST_JSON", blocking_manifest());
    std::env::set_var("FAKE_AGENT_EXIT", "1");
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_for(
        || async {
            env.history()
                .iter()
                .any(|e| matches!(e, HistoryEntry::ReviewFailed { .. }))
        },
        Duration::from_secs(10),
        "the failed re-review",
    )
    .await;
    wait_completed_and_idle(&env, &client, 2, "the queue after the failure").await;
    assert_eq!(env.opened().len(), 1);
    handle.shutdown().await;
}

/// Task 7.2: `auto_open` unset (off) never opens.
#[tokio::test]
async fn auto_open_off_by_default_opens_nothing() {
    let _env = env_guard();
    let env = Env::new("ipc-auto-open-off");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_completed_and_idle(&env, &client, 1, "the blocking review").await;
    assert_eq!(
        env.state().prs["org/one#1"].state,
        PrState::ReviewedChangesRequested
    );
    assert!(env.opened().is_empty());
    handle.shutdown().await;
}

/// Task 7.2: monitor mode writes nothing to GitHub and still auto-opens;
/// the org-level `auto_open` applies (repo > org > defaults), and the
/// on-demand brief runs too.
#[tokio::test]
async fn auto_open_fires_in_monitor_mode_from_the_org_level() {
    let _env = env_guard();
    let env = Env::new("ipc-auto-open-monitor");
    env.configure(
        &["org/one"],
        &format!(
            "{COOLDOWN_ZERO}  auto_open: off\norgs:\n  - org: org\n    auto_open: both\n    review_mode: monitor\n"
        ),
        &[
            ("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str()),
            ("FAKE_AGENT_SUMMARY", "Monitor summary."),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_completed_and_idle(&env, &client, 1, "the monitor-mode review").await;
    assert_eq!(github_writes(&env.gh_dir), 0);
    assert!(env.review_bodies().is_empty());
    let opened = env.opened();
    assert_eq!(opened.len(), 1);
    assert_eq!(
        opened[0].last().map(String::as_str),
        Some("https://github.com/org/one/pull/1")
    );
    assert_eq!(
        brief_text(&env, "org/one#1").as_deref(),
        Some("Monitor summary.")
    );
    handle.shutdown().await;
}

// ------------------------------------------- review targeting (D5) + options (D1)

/// `pr_json` for an on-demand fixture (`<n>.pr.json`) created now.
fn fresh_pr(number: u64, is_draft: bool) -> String {
    pr_json(
        number,
        &format!("PR {number}"),
        "alice",
        "f/x",
        "aaaa",
        is_draft,
        &fresh_now(),
    )
}

/// Tasks 3.3 / 3.4 / design D5: references resolve against the watched
/// repos only — full, short, and bare forms — with ambiguity and no-match
/// errors; an untracked open PR is tracked on demand and enqueued, a
/// merged one is rejected and stays untracked, and an untracked draft is
/// tracked as skipped and reported not eligible (`draft`).
#[tokio::test]
async fn review_targets_resolve_against_watched_repos_and_track_on_demand() {
    let _env = env_guard();
    let env = Env::new("ipc-targets");
    let manifest = approved_manifest();
    env.configure(
        &["org/one", "org/two", "org/three", "acme/one"],
        "",
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "  - name: org/three\n",
        "  - name: org/three\n    review_mode: off\n",
        1,
    );
    write_file(&env.config, &config);
    for repo in ["org/one", "org/two", "org/three", "acme/one"] {
        write_list_fixture(env.gh_dir.path(), repo, &[]);
    }
    let gh = env.gh_dir.path();
    write_pr_fixture(gh, "org/one", 42, &fresh_pr(42, false));
    write_pr_fixture(gh, "org/one", 43, &fresh_pr(43, false));
    write_pr_fixture(gh, "org/two", 43, &fresh_pr(43, false));
    write_pr_fixture(gh, "org/three", 44, &fresh_pr(44, false));
    write_pr_fixture(gh, "org/one", 12, &fresh_pr(12, false));
    write_file(&env.gh_dir.child("org_one#12.state"), "MERGED");
    write_pr_fixture(gh, "org/two", 50, &fresh_pr(50, true));
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    let plain = ReviewOverrides::default();
    let review = |target: &'static str| {
        let client = &client;
        let plain = &plain;
        async move { client.review_with(target, true, plain).await }
    };

    // Unwatched repo, malformed input, no match, ambiguity: nothing enqueued.
    let err = review("other/repo#5").await.unwrap_err().to_string();
    assert!(err.contains("repo 'other/repo' is not watched"), "{err}");
    let err = review("org/one#abc").await.unwrap_err().to_string();
    assert!(err.contains("org/repo#N, a GitHub PR URL"), "{err}");
    let err = review("99").await.unwrap_err().to_string();
    assert!(err.contains("no watched repo has an open PR #99"), "{err}");
    // org/three#44 exists, but mode `off` repos are not searched.
    let err = review("#44").await.unwrap_err().to_string();
    assert!(err.contains("no watched repo has an open PR #44"), "{err}");
    let err = review("43").await.unwrap_err().to_string();
    assert!(err.contains("org/one#43, org/two#43"), "{err}");
    let err = review("one#42").await.unwrap_err().to_string();
    assert!(err.contains("acme/one#42, org/one#42"), "{err}");
    let err = review("nope#1").await.unwrap_err().to_string();
    assert!(err.contains("no watched repo is named 'nope'"), "{err}");
    let err = review("org/one#12").await.unwrap_err().to_string();
    assert!(err.contains("org/one#12 is merged"), "{err}");
    let err = review("org/one#77").await.unwrap_err().to_string();
    assert!(err.contains("org/one#77 does not exist on GitHub"), "{err}");
    assert!(env.state().prs.is_empty(), "nothing was tracked");
    assert_eq!(client.get_status().await.unwrap().queue_depth, 0);

    // A bare number with a single open match: tracked on demand, enqueued.
    let resp = review("42").await.unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#42".to_string()]);
    assert!(status_lists(&client, "org/one#42").await);
    wait_completed_and_idle(&env, &client, 1, "the on-demand review").await;
    assert_eq!(
        env.state().prs["org/one#42"].state,
        PrState::ReviewedApproved
    );

    // A short name with a single match; the tracked key needs no gh call.
    let calls = || std::fs::read_to_string(env.gh_dir.child("calls.log")).unwrap();
    let resp = review("two#43").await.unwrap();
    assert_eq!(resp.enqueued, vec!["org/two#43".to_string()]);
    let before = calls()
        .matches("pr view 42 -R org/one --json number")
        .count();
    let resp = review("https://github.com/org/one/pull/42/files")
        .await
        .unwrap();
    assert!(
        resp.enqueued == vec!["org/one#42".to_string()] || resp.note.is_some(),
        "{resp:?}"
    );
    assert_eq!(
        calls()
            .matches("pr view 42 -R org/one --json number")
            .count(),
        before,
        "a tracked PR is not fetched again"
    );

    // An untracked draft: tracked as skipped, reported not eligible.
    let resp = review("org/two#50").await.unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");
    let note = resp.note.unwrap_or_default();
    assert!(note.contains("not eligible"), "{note}");
    assert!(note.contains("reason draft"), "{note}");
    let rec = env.state().prs["org/two#50"].clone();
    assert_eq!(rec.state, PrState::Skipped);
    assert_eq!(rec.skip_reason, Some(SkipReason::Draft));
    assert!(!env.state().prs.contains_key("org/one#12"));
    handle.shutdown().await;
}

/// Write `env`'s config with a second engine `other` (the fake agent with
/// an `OTHER-ENGINE` argv marker) unless `with_other` is false.
fn config_with_other_engine(env: &Env, extra: &str, with_other: bool) -> String {
    let config = env.config_for(&["org/one"], extra);
    if !with_other {
        return config;
    }
    config.replacen(
        "    args: [\"{{PROMPT}}\"]\n",
        &format!(
            "    args: [\"{{{{PROMPT}}}}\"]\n  other:\n    command: {}\n    args: [\"OTHER-ENGINE\", \"{{{{PROMPT}}}}\"]\n",
            env.agent_path.display()
        ),
        1,
    )
}

fn completed_agents(env: &Env, key_number: u64) -> Vec<Option<String>> {
    env.history()
        .iter()
        .rev()
        .filter_map(|e| match e {
            HistoryEntry::ReviewCompleted { number, agent, .. } if *number == key_number => {
                Some(agent.clone())
            }
            _ => None,
        })
        .collect()
}

/// Task 4.1 / design D1–D2: an unknown engine is a 400 listing the engines;
/// a queued job's options are replaced by a later request carrying options
/// and left intact by a plain one (one job runs, with the override engine);
/// an in-flight PR gets the "already running" note. An override engine that
/// vanished from the config before execution fails the run normally.
#[tokio::test]
async fn review_options_validate_coalesce_and_fail_when_the_engine_vanishes() {
    let _env = env_guard();
    let env = Env::new("ipc-options");
    let manifest = approved_manifest();
    let extra = "  max_concurrent_reviews: 1\n";
    env.configure(
        &["org/one"],
        extra,
        &[
            ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
            ("FAKE_AGENT_SLEEP", "1"),
        ],
    );
    write_file(&env.config, &config_with_other_engine(&env, extra, true));
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[
            pr_json(1, "One", "alice", "f/x", "aaaa", false, &fresh_now()),
            pr_json(2, "Two", "alice", "f/y", "bbbb", false, &fresh_now()),
        ],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#2"),
        Duration::from_secs(5),
        "both PRs to be tracked",
    )
    .await;
    let with = |engine: Option<&str>, post: Option<bool>| ReviewOverrides {
        engine: engine.map(str::to_string),
        post,
        extra_prompt: None,
    };

    let err = client
        .review_with("org/one#1", true, &with(Some("gpt9"), None))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("400"), "{err}");
    assert!(err.contains("unknown engine 'gpt9'"), "{err}");
    assert!(err.contains("claude, codex, fake, other, pi"), "{err}");
    assert_eq!(client.get_status().await.unwrap().queue_depth, 0);

    // #1 runs (in flight ~1 s); #2 waits in the queue behind it.
    let plain = with(None, None);
    client.review_with("org/one#1", true, &plain).await.unwrap();
    wait_for(
        || async { client.get_status().await.unwrap().in_flight == 1 },
        Duration::from_secs(5),
        "#1 to start",
    )
    .await;
    let resp = client.review_with("org/one#1", true, &plain).await.unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");
    assert!(
        resp.note
            .as_deref()
            .unwrap_or_default()
            .contains("already running"),
        "{resp:?}"
    );
    let resp = client.review_with("org/one#2", true, &plain).await.unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#2".to_string()]);
    let resp = client
        .review_with("org/one#2", true, &with(Some("other"), Some(false)))
        .await
        .unwrap();
    assert_eq!(
        resp.enqueued,
        vec!["org/one#2".to_string()],
        "options replaced"
    );
    let resp = client.review_with("org/one#2", true, &plain).await.unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");
    assert!(
        resp.note
            .as_deref()
            .unwrap_or_default()
            .contains("already queued"),
        "{resp:?}"
    );
    assert_eq!(client.get_status().await.unwrap().queue_depth, 1);
    wait_for(
        || async { completed_agents(&env, 2).len() == 1 },
        Duration::from_secs(15),
        "the coalesced #2 review",
    )
    .await;
    assert_eq!(completed_agents(&env, 2), [Some("other".to_string())]);
    assert_eq!(completed_agents(&env, 1), [Some("fake".to_string())]);
    match env.history().iter().find(|e| e.number() == 2) {
        Some(HistoryEntry::ReviewCompleted { posted, .. }) => assert_eq!(*posted, Some(false)),
        other => panic!("expected #2's completed review, got {other:?}"),
    }
    assert!(!env.gh_dir.child("org_one#2.review-body.txt").exists());
    assert!(env.gh_dir.child("org_one#1.review-body.txt").exists());

    // The override engine disappears between enqueue and execution.
    wait_completed_and_idle(&env, &client, 2, "both reviews").await;
    client.review_with("org/one#1", true, &plain).await.unwrap();
    wait_for(
        || async { client.get_status().await.unwrap().in_flight == 1 },
        Duration::from_secs(5),
        "#1 to start again",
    )
    .await;
    let resp = client
        .review_with("org/one#2", true, &with(Some("other"), None))
        .await
        .unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#2".to_string()]);
    env.rewrite_config(&config_with_other_engine(&env, extra, false));
    wait_for(
        || async {
            env.history().iter().any(|e| {
                matches!(e, HistoryEntry::ReviewFailed { number: 2, agent, .. }
                    if agent.as_deref() == Some("other"))
            })
        },
        Duration::from_secs(15),
        "the vanished-engine run to fail",
    )
    .await;
    let rec = env.state().prs["org/one#2"].clone();
    assert_eq!(rec.state, PrState::Pending, "failed back to pending");
    handle.shutdown().await;
}

/// Task 4.2 / design D2: `post: true` never makes a `monitor` repo post.
#[tokio::test]
async fn post_true_does_not_escalate_a_monitor_repo() {
    let _env = env_guard();
    let env = Env::new("ipc-monitor-post");
    let manifest = approved_manifest();
    env.configure(
        &["org/one"],
        "  review_mode: monitor\n",
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            &fresh_now(),
        )],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    wait_for(
        || status_lists(&client, "org/one#1"),
        Duration::from_secs(5),
        "the PR to be tracked",
    )
    .await;
    let post = ReviewOverrides {
        post: Some(true),
        ..ReviewOverrides::default()
    };
    let resp = client.review_with("org/one#1", true, &post).await.unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#1".to_string()]);
    wait_completed_and_idle(&env, &client, 1, "the monitor review").await;
    let calls = std::fs::read_to_string(env.gh_dir.child("calls.log")).unwrap();
    assert!(!calls.contains("pr review"), "{calls}");
    assert!(!calls.contains("/comments"), "{calls}");
    match &env.history()[..] {
        [HistoryEntry::ReviewCompleted { posted, .. }] => assert_eq!(*posted, Some(false)),
        other => panic!("expected one completed review, got {other:?}"),
    }
    handle.shutdown().await;
}
