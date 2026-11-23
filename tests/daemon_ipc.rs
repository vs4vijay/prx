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
        // The daemon looks its banner command up on PATH: this env's gh dir
        // comes first, so banners go to its stand-ins (or a test's recording
        // fakes) and never to the desktop of the machine running the tests.
        static HOST_PATH: OnceLock<String> = OnceLock::new();
        let host_path = HOST_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
        std::env::set_var(
            "PATH",
            format!("{}:{host_path}", self.gh_dir.path().display()),
        );
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

    /// Every review history entry, `review_started` included (newest
    /// first). `new_pr` tracking markers are left out: every poll that
    /// finds a PR writes one.
    fn raw_history(&self) -> Vec<HistoryEntry> {
        prx::history::query(&self.data_dir.child("history.jsonl"), None, 0)
            .unwrap()
            .into_iter()
            .filter(|e| !matches!(e, HistoryEntry::NewPr { .. }))
            .collect()
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

/// D3: only the menu bar app counts as a menubar sighting. The auth
/// middleware records the last-seen instant for `X-Prx-Client: menubar/…`,
/// which the typed client sends once it opted in with `as_menubar`. The
/// plain client (the CLI's), a request without the header, and a header
/// naming some other client record nothing.
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
    // The plain client is what the CLI uses: it does not announce.
    client.get_status().await.unwrap();
    assert!(!handle.shared.menubar_recently_seen(Duration::from_secs(90)));
    // A client header that names something other than the menubar (an
    // older CLI sent `menubar/<version>`; anything else is not the app).
    let raw = Raw::new(&handle);
    let bearer = format!("Bearer {}", handle.token);
    for other in ["cli/0.1.1", "", "menubarista/1", "Menubar/1", "x-menubar/1"] {
        let headers = [("authorization", bearer.as_str()), ("x-prx-client", other)];
        assert_eq!(raw.send("GET", "/status", &headers, None).await.status, 200);
        assert!(
            !handle.shared.menubar_recently_seen(Duration::from_secs(90)),
            "{other:?} is not the menubar"
        );
    }
    // PRX.app's client opts in and announces on every request.
    client.clone().as_menubar().get_status().await.unwrap();
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

    let menubar = env.client(&handle).as_menubar();
    menubar.get_status().await.unwrap();
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

    // Each review wrote a `review_started` entry before its outcome (D6),
    // after the `new_pr` entry the poll that found the PR wrote.
    let all = client.history(None, 0).await.unwrap();
    assert_eq!(all.entries.len(), 6);
    let one = client.history(Some("org/one"), 0).await.unwrap();
    assert_eq!(one.entries.len(), 3);
    assert!(one.entries.iter().all(|e| e.repo() == "org/one"));
    assert!(matches!(
        one.entries[0],
        HistoryEntry::ReviewCompleted { .. }
    ));
    assert!(matches!(one.entries[1], HistoryEntry::ReviewStarted { .. }));
    assert!(matches!(
        one.entries[2],
        HistoryEntry::NewPr { number: 1, .. }
    ));
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
    assert!(note.contains("skipped \u{2014} draft"), "{note}");
    assert!(note.contains("confirm"), "{note}");
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

// ------------------------------------- raw requests: auth and bad input

/// What the daemon answered to one raw request.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    /// The message of a JSON error body (`{"error": "..."}`), else empty.
    fn error(&self) -> String {
        serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_default()
    }
}

/// Raw HTTP access to a daemon: unlike [`DaemonClient`], a request carries
/// exactly the headers and bytes a test gives it.
struct Raw {
    http: reqwest::Client,
    port: u16,
    token: String,
}

impl Raw {
    fn new(handle: &DaemonHandle) -> Raw {
        Raw {
            http: reqwest::Client::builder().no_proxy().build().unwrap(),
            port: handle.port,
            token: handle.token.clone(),
        }
    }

    /// Send one request with only the given headers (no token, no content
    /// type, no client announcement unless listed).
    async fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<&str>,
    ) -> Reply {
        let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let mut request = self
            .http
            .request(method, format!("http://127.0.0.1:{}{path}", self.port));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        if let Some(body) = body {
            request = request.body(body.to_string());
        }
        let response = request.send().await.expect("the daemon must answer");
        Reply {
            status: response.status().as_u16(),
            body: response.text().await.unwrap_or_default(),
        }
    }

    /// Send a request the way a well-behaved client does: this start's
    /// bearer token, and a JSON content type with the body.
    async fn authed(&self, method: &str, path: &str, body: Option<&str>) -> Reply {
        let bearer = format!("Bearer {}", self.token);
        self.send(
            method,
            path,
            &[
                ("authorization", &bearer),
                ("content-type", "application/json"),
            ],
            body,
        )
        .await
    }
}

const KEY_BODY: &str = r#"{"key": "org/one#1"}"#;

/// Every route of the IPC API, with a body it accepts. A route added to
/// `ipc::build_router` belongs here too, so the auth tests cover it.
const ROUTES: &[(&str, &str, Option<&str>)] = &[
    ("GET", "/status", None),
    (
        "POST",
        "/review",
        Some(r#"{"key": "org/one#1", "force": true}"#),
    ),
    ("POST", "/brief", Some(KEY_BODY)),
    ("POST", "/approve", Some(KEY_BODY)),
    (
        "POST",
        "/snooze",
        Some(r#"{"key": "org/one#1", "hours": 1}"#),
    ),
    ("POST", "/ignore", Some(KEY_BODY)),
    ("POST", "/unignore", Some(KEY_BODY)),
    ("POST", "/unsnooze", Some(KEY_BODY)),
    ("POST", "/review-now", None),
    ("POST", "/pause", Some(r#"{"hours": 1}"#)),
    ("POST", "/resume", Some("{}")),
    ("GET", "/config", None),
    ("PUT", "/config", Some(r#"{"yaml": "repos: []\n"}"#)),
    ("POST", "/review-mode", Some(r#"{"mode": "off"}"#)),
    ("POST", "/block", Some(KEY_BODY)),
    ("POST", "/unblock", Some(KEY_BODY)),
    ("GET", "/history", None),
    ("GET", "/insights", None),
    ("GET", "/usage", None),
    ("GET", "/discover/agents", None),
    ("GET", "/discover/repos", None),
];

/// A PR created just now: inside the default first-review cooldown, so the
/// daemon tracks it and reviews nothing by itself.
fn cooling_pr(number: u64) -> String {
    pr_json(
        number,
        "Add feature",
        "alice",
        "f/x",
        "aaaa",
        false,
        &fresh_now(),
    )
}

/// Wait (without any IPC request) until the daemon tracks `key`.
async fn tracked_on_disk(env: &Env, key: &str) {
    wait_for(
        || async { env.state().prs.contains_key(key) },
        Duration::from_secs(5),
        "the PR to be tracked",
    )
    .await;
}

/// The tracked PRs as stored, for before/after comparisons (the scheduler
/// keeps moving `last_poll`, so whole-file equality would be meaningless).
fn stored_prs(env: &Env) -> serde_json::Value {
    serde_json::to_value(&env.state().prs).unwrap()
}

/// Core invariant 8: every route answers 401 — before it looks at the body
/// or does anything — unless the request carries this start's token as
/// `Authorization: Bearer <token>`. Near misses (another scheme, a changed
/// character, the token in the URL) are refused the same way, change no
/// state, write nothing to GitHub, and do not count as a menubar sighting.
#[tokio::test]
async fn every_route_rejects_requests_without_the_bearer_token() {
    let _env = env_guard();
    let env = Env::new("ipc-auth-all");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let raw = Raw::new(&handle);
    tracked_on_disk(&env, "org/one#1").await;
    let prs_before = stored_prs(&env);
    let config_before = std::fs::read_to_string(&env.config).unwrap();

    let token = handle.token.clone();
    let mut altered = token.clone().into_bytes();
    altered[63] = if altered[63] == b'0' { b'1' } else { b'0' };
    let altered = String::from_utf8(altered).unwrap();
    let credentials: Vec<(&str, Option<String>)> = vec![
        ("no Authorization header", None),
        ("an empty token", Some("Bearer ".to_string())),
        ("an unrelated token", Some("Bearer wrong-token".to_string())),
        ("one changed character", Some(format!("Bearer {altered}"))),
        (
            "a truncated token",
            Some(format!("Bearer {}", &token[..63])),
        ),
        ("a token with a suffix", Some(format!("Bearer {token}0"))),
        ("the bare token", Some(token.clone())),
        ("a lower-case scheme", Some(format!("bearer {token}"))),
        ("the Basic scheme", Some(format!("Basic {token}"))),
        ("two spaces", Some(format!("Bearer  {token}"))),
    ];
    for (method, path, body) in ROUTES {
        for (what, credential) in &credentials {
            let mut headers = vec![
                ("content-type", "application/json"),
                ("x-prx-client", "menubar/test"),
            ];
            if let Some(credential) = credential {
                headers.push(("authorization", credential.as_str()));
            }
            let reply = raw.send(method, path, &headers, *body).await;
            assert_eq!(reply.status, 401, "{method} {path} with {what}: {reply:?}");
            assert_eq!(
                reply.error(),
                "missing or invalid bearer token",
                "{method} {path} with {what}"
            );
        }
        // The token is a header, never a URL parameter.
        for param in ["token", "access_token", "bearer"] {
            let reply = raw
                .send(method, &format!("{path}?{param}={token}"), &[], *body)
                .await;
            assert_eq!(reply.status, 401, "{method} {path}?{param}=…");
        }
    }

    // Nothing happened: same PRs, same config, no pause, no GitHub write,
    // no history, and no menubar "seen" (which would mute the banners).
    assert_eq!(stored_prs(&env), prs_before);
    assert_eq!(std::fs::read_to_string(&env.config).unwrap(), config_before);
    assert_eq!(env.state().paused_until, None);
    assert_eq!(
        handle.shared.live().config.defaults.review_mode,
        ReviewMode::ReviewComment
    );
    assert_eq!(github_writes(&env.gh_dir), 0);
    assert!(env.raw_history().is_empty());
    assert!(!handle.shared.menubar_recently_seen(Duration::from_secs(90)));

    // The same request with the right header is served.
    assert_eq!(raw.authed("GET", "/status", None).await.status, 200);
    handle.shutdown().await;
}

/// The route table above matches the daemon: with the token, every listed
/// method + path is routed to a handler (never 404 / 405, never 401).
#[tokio::test]
async fn every_listed_route_exists_and_accepts_the_token() {
    let _env = env_guard();
    let env = Env::new("ipc-routes");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let raw = Raw::new(&handle);
    tracked_on_disk(&env, "org/one#1").await;

    for (method, path, body) in ROUTES {
        let reply = raw.authed(method, path, *body).await;
        assert!(
            ![401, 404, 405].contains(&reply.status),
            "{method} {path} must be routed: {reply:?}"
        );
    }
    handle.shutdown().await;
}

/// Core invariant 8: the token is minted per start. After a restart the
/// previous start's token opens nothing, the discovery files describe the
/// new start, and they are gone again once the daemon stops.
#[tokio::test]
async fn a_token_from_a_previous_start_is_rejected_after_a_restart() {
    let _env = env_guard();
    let env = Env::new("ipc-stale-token");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let token_file = env.data_dir.child("daemon.token");
    let port_file = env.data_dir.child("daemon.port");

    let first = env.start(fast_opts()).await;
    let old_token = first.token.clone();
    assert_eq!(std::fs::read_to_string(&token_file).unwrap(), old_token);
    assert_eq!(
        std::fs::read_to_string(&port_file).unwrap(),
        first.port.to_string()
    );
    assert_eq!(file_mode(&token_file), 0o600);
    first.shutdown().await;
    assert!(!token_file.exists(), "daemon.token must go with the daemon");
    assert!(!port_file.exists(), "daemon.port must go with the daemon");

    let second = env.start(fast_opts()).await;
    assert_ne!(second.token, old_token, "every start mints a new token");
    assert_eq!(std::fs::read_to_string(&token_file).unwrap(), second.token);
    tracked_on_disk(&env, "org/one#1").await;

    let raw = Raw::new(&second);
    let stale = format!("Bearer {old_token}");
    for (method, path, body) in ROUTES {
        let reply = raw
            .send(
                method,
                path,
                &[
                    ("authorization", &stale),
                    ("content-type", "application/json"),
                ],
                *body,
            )
            .await;
        assert_eq!(reply.status, 401, "{method} {path} with the old token");
    }
    // The typed client reports it as an error, not as a daemon answer.
    let stale_client =
        DaemonClient::with_base(format!("http://127.0.0.1:{}", second.port), old_token);
    let err = stale_client.approve("org/one#1").await.unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");
    assert_eq!(github_writes(&env.gh_dir), 0);
    second.shutdown().await;
}

/// A `daemon.token` left behind by a killed daemon may have any permissions
/// by now. The next start must not put its token into a readable file.
#[tokio::test]
async fn a_leftover_token_file_is_made_private_again() {
    use std::os::unix::fs::PermissionsExt;
    let _env = env_guard();
    let env = Env::new("ipc-token-perms");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let token_file = env.data_dir.child("daemon.token");
    write_file(&token_file, "a-token-from-a-daemon-that-was-killed");
    std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let handle = env.start(fast_opts()).await;
    assert_eq!(std::fs::read_to_string(&token_file).unwrap(), handle.token);
    assert_eq!(
        file_mode(&token_file),
        0o600,
        "the token file must be readable by its owner only"
    );
    handle.shutdown().await;
}

/// Core invariant 8: the API listens on 127.0.0.1 and nowhere else. A
/// connection to the same port on the IPv6 loopback, or on an address
/// other machines could reach, finds nothing listening.
#[tokio::test]
async fn ipc_api_listens_on_the_ipv4_loopback_only() {
    use std::net::{IpAddr, SocketAddr};
    let _env = env_guard();
    let env = Env::new("ipc-loopback");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;

    async fn connects(addr: SocketAddr) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_secs(2), tokio::net::TcpStream::connect(addr))
                .await,
            Ok(Ok(_))
        )
    }
    let loopback: IpAddr = "127.0.0.1".parse().unwrap();
    assert!(connects(SocketAddr::new(loopback, handle.port)).await);
    let v6: IpAddr = "::1".parse().unwrap();
    assert!(
        !connects(SocketAddr::new(v6, handle.port)).await,
        "the API must not listen on ::1"
    );
    // This machine's outward-facing address, if it has one: a connected
    // UDP socket reveals the route's source address without sending.
    let outward = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|socket| {
            socket.connect("192.0.2.1:9")?;
            socket.local_addr()
        })
        .map(|addr| addr.ip())
        .ok()
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified());
    if let Some(ip) = outward {
        assert!(
            !connects(SocketAddr::new(ip, handle.port)).await,
            "the API must not listen on {ip}"
        );
    }
    handle.shutdown().await;
}

/// Requests the daemon cannot read — broken JSON, a missing or mistyped
/// field, no content type, the wrong method, an unknown path, a bad query
/// value — are refused with the matching 4xx. None of them changes state,
/// reaches GitHub, or disturbs the daemon.
#[tokio::test]
async fn unreadable_requests_are_refused_without_side_effects() {
    let _env = env_guard();
    let env = Env::new("ipc-bad-requests");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let raw = Raw::new(&handle);
    tracked_on_disk(&env, "org/one#1").await;
    let prs_before = stored_prs(&env);

    for (what, path, body, status) in [
        ("truncated JSON", "/ignore", r#"{"key":"#, 400),
        ("not JSON", "/approve", "org/one#1", 400),
        ("an empty body", "/block", "", 400),
        ("a missing field", "/ignore", "{}", 422),
        ("a missing field", "/approve", "{}", 422),
        ("a mistyped field", "/approve", r#"{"key": 1}"#, 422),
        ("an array body", "/snooze", "[]", 422),
        (
            "a mistyped option",
            "/review",
            r#"{"key": "all", "force": "yes"}"#,
            422,
        ),
        (
            "an unknown mode",
            "/review-mode",
            r#"{"mode": "sometimes"}"#,
            422,
        ),
        ("a mistyped pause", "/pause", r#"{"hours": "two"}"#, 422),
    ] {
        let reply = raw.authed("POST", path, Some(body)).await;
        assert_eq!(reply.status, status, "{what} to {path}: {reply:?}");
    }
    // A JSON body must say it is JSON.
    let bearer = format!("Bearer {}", handle.token);
    let reply = raw
        .send(
            "POST",
            "/approve",
            &[("authorization", &bearer)],
            Some(KEY_BODY),
        )
        .await;
    assert_eq!(reply.status, 415, "{reply:?}");

    for (method, path) in [
        ("POST", "/status"),
        ("DELETE", "/status"),
        ("GET", "/approve"),
        ("GET", "/review"),
        ("GET", "/review-now"),
        ("DELETE", "/config"),
        ("POST", "/config"),
        ("PUT", "/review-mode"),
        ("POST", "/history"),
    ] {
        let reply = raw.authed(method, path, Some(KEY_BODY)).await;
        assert_eq!(reply.status, 405, "{method} {path}: {reply:?}");
    }
    for path in ["/", "/nope", "/status/", "/approve/org/one/1", "/v1/status"] {
        let reply = raw.authed("GET", path, None).await;
        assert_eq!(reply.status, 404, "{path}: {reply:?}");
        // Without the token an unknown path is a 401 like any other: the
        // API does not tell strangers which routes exist.
        assert_eq!(raw.send("GET", path, &[], None).await.status, 401, "{path}");
    }
    for path in [
        "/history?limit=many",
        "/history?limit=-1",
        "/history?before=yesterday",
        "/insights?days=-1",
        "/insights?days=soon",
        "/usage?bucket=fortnight",
        "/usage?window=forever",
        "/usage?group=color",
    ] {
        let reply = raw.authed("GET", path, None).await;
        assert_eq!(reply.status, 400, "{path}: {reply:?}");
    }
    // The handlers' own 400s name the problem in the JSON error body.
    let reply = raw.authed("GET", "/history?before=yesterday", None).await;
    assert!(reply.error().contains("RFC 3339"), "{reply:?}");

    assert_eq!(stored_prs(&env), prs_before);
    assert_eq!(github_writes(&env.gh_dir), 0);
    assert!(env.raw_history().is_empty());
    assert_eq!(raw.authed("GET", "/status", None).await.status, 200);
    handle.shutdown().await;
}

/// Well-formed requests that name the wrong thing: a reference that is not
/// a PR, a repo this daemon does not watch, a PR it does not track, or
/// values no handler accepts. Each gets a 4xx with a JSON `error`, and —
/// for approve / block / unblock in particular — nothing is sent to GitHub.
#[tokio::test]
async fn requests_naming_the_wrong_thing_get_a_4xx_and_change_nothing() {
    let _env = env_guard();
    let env = Env::new("ipc-wrong-target");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let raw = Raw::new(&handle);
    tracked_on_disk(&env, "org/one#1").await;
    let prs_before = stored_prs(&env);
    let config_before = std::fs::read_to_string(&env.config).unwrap();

    let key = |key: &str| format!(r#"{{"key": "{key}"}}"#);
    let snooze = |key: &str| format!(r#"{{"key": "{key}", "hours": 1}}"#);
    let cases: Vec<(&str, String, u16, &str)> = vec![
        // Not a PR reference at all.
        ("/approve", key("bogus"), 400, "invalid PR target"),
        ("/approve", key("org/one#x"), 400, "invalid PR target"),
        ("/block", key("bogus"), 400, "invalid PR target"),
        ("/unblock", key("org/one"), 400, "invalid PR target"),
        ("/ignore", key("#"), 400, "invalid PR target"),
        ("/unignore", key(""), 400, "invalid PR target"),
        ("/unsnooze", key("one#1#2"), 400, "invalid PR target"),
        ("/snooze", snooze("bogus"), 400, "invalid PR target"),
        ("/brief", key("not a pr"), 400, ""),
        // A repo this daemon does not watch.
        ("/ignore", key("org/unknown#1"), 404, "not watched"),
        ("/unignore", key("org/unknown#1"), 404, "not watched"),
        ("/unsnooze", key("org/unknown#1"), 404, "not watched"),
        ("/snooze", snooze("org/unknown#1"), 404, "not watched"),
        ("/brief", key("org/unknown#1"), 404, "not watched"),
        // A watched repo, but a PR the daemon does not track.
        ("/ignore", key("org/one#99"), 404, "not tracked"),
        ("/unignore", key("org/one#99"), 404, "not tracked"),
        ("/unsnooze", key("org/one#99"), 404, "not tracked"),
        ("/snooze", snooze("org/one#99"), 404, "not tracked"),
        ("/brief", key("org/one#99"), 404, "not tracked"),
        // Values no handler accepts.
        ("/snooze", key("org/one#1"), 400, "hours or until_push"),
        (
            "/snooze",
            r#"{"key": "org/one#1", "hours": 0}"#.into(),
            400,
            "must be > 0",
        ),
        (
            "/snooze",
            r#"{"key": "org/one#1", "hours": -2, "until_push": true}"#.into(),
            400,
            "must be > 0",
        ),
        (
            "/review",
            r#"{"key": "org/one#1", "force": true, "engine": "no-such-engine"}"#.into(),
            400,
            "no-such-engine",
        ),
        ("/pause", "{}".into(), 400, "--hours N or --until"),
    ];
    for (path, body, status, needle) in &cases {
        let reply = raw.authed("POST", path, Some(body)).await;
        assert_eq!(reply.status, *status, "{path} {body}: {reply:?}");
        assert!(
            !reply.error().is_empty() && reply.error().contains(needle),
            "{path} {body}: {reply:?}"
        );
    }
    for body in ["{}", r#"{"config": {}, "yaml": "repos: []\n"}"#] {
        let reply = raw.authed("PUT", "/config", Some(body)).await;
        assert_eq!(reply.status, 400, "{body}: {reply:?}");
        assert!(reply.error().contains("exactly one"), "{reply:?}");
    }

    assert_eq!(stored_prs(&env), prs_before);
    assert_eq!(std::fs::read_to_string(&env.config).unwrap(), config_before);
    assert_eq!(env.state().paused_until, None);
    assert_eq!(
        github_writes(&env.gh_dir),
        0,
        "a refused request must not reach GitHub"
    );
    let (queued, in_flight) = handle.shared.queue_stats().await;
    assert_eq!((queued, in_flight), (0, 0), "nothing may be enqueued");
    handle.shutdown().await;
}

/// A timed snooze is capped at 87600 hours (~10 years), like a pause. A
/// larger value used to overflow the end time and kill the request: the
/// client saw a dropped connection instead of an answer.
#[tokio::test]
async fn snooze_hours_past_the_cap_are_a_400_not_a_dropped_connection() {
    let _env = env_guard();
    let env = Env::new("ipc-snooze-cap");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    tracked_pending(&env, &client, "org/one#1").await;

    for hours in [87_600.5, 1e9, 1e15, 1e300] {
        let err = client
            .snooze("org/one#1", Some(hours), false)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("400") && err.contains("too large (max 87600)"),
            "{hours} hours: {err}"
        );
    }
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);

    // The cap itself is accepted.
    let resp = client
        .snooze("org/one#1", Some(87_600.0), false)
        .await
        .unwrap();
    assert!(resp.changed, "{resp:?}");
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.skip_reason, Some(SkipReason::Snoozed));
    let left = rec.snooze_until.expect("a timed snooze") - chrono::Utc::now();
    assert!(
        left > chrono::Duration::hours(87_599) && left <= chrono::Duration::hours(87_600),
        "{left}"
    );
    handle.shutdown().await;
}

/// A request body past the 2 MB limit is refused with 413 before anything
/// is parsed or written.
#[tokio::test]
async fn an_oversized_body_is_refused_and_the_config_stays_as_it_was() {
    let _env = env_guard();
    let env = Env::new("ipc-oversized");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let raw = Raw::new(&handle);
    let config_before = std::fs::read_to_string(&env.config).unwrap();

    let padding = "# padding\n".repeat(300_000);
    let body = serde_json::json!({ "yaml": format!("{config_before}{padding}") }).to_string();
    assert!(body.len() > 2 * 1024 * 1024);
    let reply = raw.authed("PUT", "/config", Some(&body)).await;
    assert_eq!(reply.status, 413, "{}", reply.body);

    assert_eq!(std::fs::read_to_string(&env.config).unwrap(), config_before);
    assert_eq!(raw.authed("GET", "/status", None).await.status, 200);
    handle.shutdown().await;
}

// ------------------------------------------------ approve (invariant 1)

/// Core invariant 1: only `POST /approve` posts a GitHub approval. An
/// automated review with verdict `approved` is posted as a comment, and no
/// other route — re-review, block, unblock, snooze, ignore, brief, pause,
/// mode and config changes — ever runs `gh pr review --approve`.
#[tokio::test]
async fn only_the_approve_route_posts_a_github_approval() {
    let _env = env_guard();
    let env = Env::new("ipc-approve-only");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str())],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    // The automated review: verdict approved, posted as a comment review.
    wait_completed_and_idle(&env, &client, 1, "the automated review").await;
    assert_eq!(
        env.state().prs["org/one#1"].last_verdict,
        Some(Verdict::Approved)
    );
    assert_eq!(
        count_calls(&env.gh_dir, "pr review 1 -R org/one --comment"),
        1
    );

    // Everything else a client can ask for.
    let key = "org/one#1";
    client.review(Some(key), false, true).await.unwrap();
    wait_completed_and_idle(&env, &client, 2, "the forced re-review").await;
    client.review(None, true, true).await.unwrap();
    wait_completed_and_idle(&env, &client, 3, "the forced review of all").await;
    client.block(key).await.unwrap();
    client.unblock(key).await.unwrap();
    client.snooze(key, Some(1.0), false).await.unwrap();
    client.unsnooze(key).await.unwrap();
    client.snooze(key, None, true).await.unwrap();
    client.ignore(key).await.unwrap();
    client.unignore(key).await.unwrap();
    client.brief(key).await.unwrap();
    client.review_now().await.unwrap();
    client.pause(Some(1.0), None).await.unwrap();
    client.resume().await.unwrap();
    client.set_review_mode(ReviewMode::Monitor).await.unwrap();
    client
        .set_review_mode(ReviewMode::ReviewComment)
        .await
        .unwrap();
    let yaml = std::fs::read_to_string(&env.config).unwrap();
    client.put_config_yaml(&yaml).await.unwrap();
    client.history(None, 10).await.unwrap();
    client.insights(None, None).await.unwrap();
    wait_for(
        || async {
            let s = client.get_status().await.unwrap();
            s.in_flight == 0 && s.queue_depth == 0 && !s.prs[0].brief_pending
        },
        Duration::from_secs(15),
        "the daemon to go idle",
    )
    .await;
    assert!(
        github_writes(&env.gh_dir) >= 4,
        "reviews and a block were posted"
    );
    assert_eq!(
        count_calls(&env.gh_dir, "--approve"),
        0,
        "no automated or indirect path may approve"
    );

    // The human's approve: exactly one approval, for exactly this PR.
    let resp = client.approve(key).await.unwrap();
    assert!(resp.ok && resp.changed, "{resp:?}");
    assert_eq!(count_calls(&env.gh_dir, "--approve"), 1);
    assert_eq!(
        count_calls(&env.gh_dir, "pr review 1 -R org/one --approve"),
        1
    );
    handle.shutdown().await;
}

// ------------------------------------------------ config reload (D1/D2)

/// D2: a config file that disappears or stops being text is a reload
/// failure like any other — the daemon keeps polling on the last good
/// config, reports `config_error`, and clears it once the file is back.
#[tokio::test]
async fn a_missing_or_unreadable_config_file_keeps_the_last_good_config() {
    let _env = env_guard();
    let env = Env::new("ipc-config-gone");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    tracked_pending(&env, &client, "org/one#1").await;
    let good = std::fs::read_to_string(&env.config).unwrap();

    let error_reported = |want: bool| {
        let client = client.clone();
        move || {
            let client = client.clone();
            async move { client.get_status().await.unwrap().config_error.is_some() == want }
        }
    };
    for breakage in ["removed", "binary"] {
        match breakage {
            "removed" => std::fs::remove_file(&env.config).unwrap(),
            _ => std::fs::write(&env.config, [0xff, 0xfe, 0x00, 0xc3]).unwrap(),
        }
        wait_for(
            error_reported(true),
            Duration::from_secs(5),
            "config_error to be reported",
        )
        .await;
        let status = client.get_status().await.unwrap();
        let error = status.config_error.expect("config_error");
        assert!(!error.message.is_empty(), "{breakage}");
        // Still on the last good config: the repo is watched and polled.
        assert!(
            status.prs.iter().any(|p| p.key == "org/one#1"),
            "{breakage}"
        );
        let polled = status.last_poll["org/one"];
        wait_for(
            || async { client.get_status().await.unwrap().last_poll["org/one"] > polled },
            Duration::from_secs(10),
            "polling to continue on the last good config",
        )
        .await;

        write_file(&env.config, &good);
        wait_for(
            error_reported(false),
            Duration::from_secs(5),
            "config_error to clear once the file is back",
        )
        .await;
    }
    handle.shutdown().await;
}

/// Sample the daemon's in-flight count until `completed` reviews of
/// org/one are in history; returns the largest count seen.
async fn max_in_flight_until(env: &Env, handle: &DaemonHandle, completed: usize) -> usize {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut max = 0;
    while completed_reviews(env, "org/one") < completed {
        assert!(Instant::now() < deadline, "the reviews did not complete");
        max = max.max(handle.shared.queue_stats().await.1);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    max
}

fn due_prs(numbers: std::ops::RangeInclusive<u64>) -> Vec<String> {
    numbers
        .map(|n| {
            pr_json(
                n,
                &format!("PR {n}"),
                "alice",
                &format!("f/{n}"),
                &format!("sha{n}"),
                false,
                OLD,
            )
        })
        .collect()
}

/// `max_concurrent_reviews` is read once, at start: a reload that raises
/// it changes the running config but not the number of review slots, and
/// the new value applies from the next start.
#[tokio::test]
async fn max_concurrent_reviews_is_sized_at_start_and_ignores_reloads() {
    let _env = env_guard();
    let env = Env::new("ipc-cap-reload");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_AGENT_SLEEP", "0.3"),
        ],
    );
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    assert_eq!(handle.shared.max_concurrent, 1);

    let raised = format!("{COOLDOWN_ZERO}  max_concurrent_reviews: 3\n");
    write_file(
        &env.config,
        &local_config(&["org/one"], &env.agent_path, &raised),
    );
    wait_for(
        || async { handle.shared.live().config.defaults.max_concurrent_reviews == 3 },
        Duration::from_secs(5),
        "the reload to apply",
    )
    .await;
    assert_eq!(handle.shared.max_concurrent, 1);
    assert_eq!(handle.shared.semaphore.available_permits(), 1);

    // Three due PRs still go through the one slot sized at start.
    write_list_fixture(env.gh_dir.path(), "org/one", &due_prs(1..=3));
    handle.shared.poll_requested.notify_one();
    assert_eq!(max_in_flight_until(&env, &handle, 3).await, 1);
    handle.shutdown().await;

    let restarted = env.start(fast_opts()).await;
    assert_eq!(restarted.shared.max_concurrent, 3);
    assert_eq!(restarted.shared.semaphore.available_permits(), 3);
    restarted.shutdown().await;
}

/// D6: with `max_concurrent_reviews: 2` and five PRs due at once, two
/// reviews are in flight while the rest wait in the queue — never three —
/// and every PR is reviewed exactly once.
#[tokio::test]
async fn in_flight_reviews_never_exceed_the_concurrency_cap() {
    let _env = env_guard();
    let env = Env::new("ipc-cap-two");
    env.configure(
        &["org/one"],
        &format!("{COOLDOWN_ZERO}  max_concurrent_reviews: 2\n"),
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_AGENT_SLEEP", "0.3"),
        ],
    );
    write_list_fixture(env.gh_dir.path(), "org/one", &due_prs(1..=5));
    let handle = env.start(fast_opts()).await;
    assert_eq!(handle.shared.max_concurrent, 2);

    assert_eq!(max_in_flight_until(&env, &handle, 5).await, 2);
    let client = env.client(&handle);
    wait_completed_and_idle(&env, &client, 5, "all five reviews").await;
    assert_eq!(completed_reviews(&env, "org/one"), 5, "one review per PR");
    for n in 1..=5 {
        assert_eq!(
            env.state().prs[&format!("org/one#{n}")].state,
            PrState::ReviewedApproved
        );
    }
    handle.shutdown().await;
}

/// `in_review` exists in memory only: while the agent is running, the
/// state file still shows the PR as pending (so a daemon killed at this
/// point leaves nothing stranded), and the run's start is already in
/// history.
#[tokio::test]
async fn the_state_file_never_shows_in_review_while_a_review_runs() {
    let _env = env_guard();
    let env = Env::new("ipc-in-review");
    let prompt_out = env.data_dir.child("agent-started.txt");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_AGENT_SLEEP", "1"),
            ("FAKE_AGENT_PROMPT_OUT", prompt_out.to_str().unwrap()),
        ],
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    let handle = env.start(fast_opts()).await;

    wait_for(
        || async { prompt_out.exists() },
        Duration::from_secs(10),
        "the agent to start",
    )
    .await;
    assert!(handle.shared.is_in_flight("org/one#1").await);
    let on_disk = env.state();
    assert_eq!(on_disk.prs["org/one#1"].state, PrState::Pending);
    assert_eq!(on_disk.prs["org/one#1"].last_verdict, None);
    assert!(
        matches!(
            env.raw_history().as_slice(),
            [HistoryEntry::ReviewStarted { .. }]
        ),
        "{:?}",
        env.raw_history()
    );

    let client = env.client(&handle);
    wait_completed_and_idle(&env, &client, 1, "the review to finish").await;
    assert_eq!(
        env.state().prs["org/one#1"].state,
        PrState::ReviewedApproved
    );
    handle.shutdown().await;
}

/// A poll that fails is not reported as a poll: `last_poll` stays empty
/// for the repo and nothing is tracked, while a retry is already planned.
#[tokio::test]
async fn a_failed_poll_is_not_reported_as_a_poll_and_a_retry_is_planned() {
    let _env = env_guard();
    let env = Env::new("ipc-poll-fail");
    env.configure(&["org/one"], "", &[("FAKE_GH_LIST_FAIL", "999999")]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    wait_for(
        || async { count_calls(&env.gh_dir, "pr list") >= 2 },
        Duration::from_secs(10),
        "the failed poll to be retried",
    )
    .await;
    let status = client.get_status().await.unwrap();
    assert!(status.last_poll.is_empty(), "{:?}", status.last_poll);
    assert_eq!(status.last_state_poll, None);
    assert!(status.prs.is_empty());
    let next = status.next_poll.expect("a retry must be planned");
    assert!(next > chrono::Utc::now() - chrono::Duration::seconds(5));
    assert!(env.state().prs.is_empty());
    handle.shutdown().await;
}

/// `next_poll` in `/status` is the soonest planned poll: once a repo has
/// been polled it lies after that poll, about one (scaled) interval away.
#[tokio::test]
async fn status_reports_the_next_planned_poll_after_the_last_one() {
    let _env = env_guard();
    let env = Env::new("ipc-next-poll");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    // One hour: with the 0.001 test scale, 3.6 s — long enough that the
    // poll after the startup one cannot have happened yet.
    write_file(
        &env.config,
        &local_config(&["org/one"], &env.agent_path, "")
            .replace("poll_interval_minutes: 15", "poll_interval_minutes: 60"),
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    wait_for(
        || async { !client.get_status().await.unwrap().last_poll.is_empty() },
        Duration::from_secs(5),
        "the startup poll",
    )
    .await;
    let status = client.get_status().await.unwrap();
    let last = status.last_poll["org/one"];
    let next = status.next_poll.expect("the next poll is planned");
    let gap = (next - last).num_milliseconds();
    // 60 min × 0.001 = 3.6 s, ±10 % jitter.
    assert!((3_240..=3_960).contains(&gap), "{gap} ms");
    handle.shutdown().await;
}

// ------------------------------------------------------- discovery

/// Replace this env's fake gh with one that answers `gh api user/repos…`
/// the way the real one does with `--jq … @tsv` (first page only), or
/// fails when `fail` is set. Everything else is answered with nothing.
fn install_repo_listing_gh(env: &Env, fail: bool) {
    use std::os::unix::fs::PermissionsExt;
    let answer = if fail {
        "echo 'gh: HTTP 401: Bad credentials' >&2; exit 1".to_string()
    } else {
        "case \"$2\" in *page=1) printf 'acme/api\\tfalse\\nacme/vault\\ttrue\\n';; esac"
            .to_string()
    };
    let script = format!(
        "#!/bin/sh\nprintf '%s %s\\n' \"${{GH_TOKEN:-<default>}}\" \"$*\" >> \"$FAKE_GH_LOG.tokens\"\n\
         if [ \"$1\" = api ]; then {answer}; exit 0; fi\necho '[]'\n"
    );
    write_file(&env.gh_path, &script);
    std::fs::set_permissions(&env.gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `GET /discover/repos` lists what the gh account can see, asking gh
/// with the default identity's credential; a gh failure is a 502 with the
/// reason, not an empty list.
#[tokio::test]
async fn discover_repos_lists_through_gh_with_the_identity_credential() {
    let _env = env_guard();
    let env = Env::new("ipc-discover-repos");
    env.configure(
        &["org/one"],
        "  identity:\n    credential: PRX_TEST_CRED_DISCOVER\n",
        &[],
    );
    std::env::set_var("PRX_TEST_CRED_DISCOVER", "tok-discover");
    install_repo_listing_gh(&env, false);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    let found = client.discover_repos().await.unwrap();
    let listed: Vec<(&str, bool)> = found
        .repos
        .iter()
        .map(|r| (r.name.as_str(), r.private))
        .collect();
    assert_eq!(listed, [("acme/api", false), ("acme/vault", true)]);
    let api_calls: Vec<(String, String)> = token_calls(&env.gh_dir)
        .into_iter()
        .filter(|(_, args)| args.starts_with("api user/repos"))
        .collect();
    assert!(!api_calls.is_empty());
    assert!(
        api_calls.iter().all(|(token, _)| token == "tok-discover"),
        "{api_calls:?}"
    );

    install_repo_listing_gh(&env, true);
    let err = client.discover_repos().await.unwrap_err().to_string();
    assert!(
        err.contains("502") && err.contains("could not list repositories"),
        "{err}"
    );
    std::env::remove_var("PRX_TEST_CRED_DISCOVER");
    handle.shutdown().await;
}

/// `GET /discover/agents` reports the built-in agent CLIs, where each
/// configured agent command resolves on this daemon's machine, and the
/// configured LLM API engines — without running any of them.
#[tokio::test]
async fn discover_agents_reports_builtins_and_configured_engines() {
    let _env = env_guard();
    let env = Env::new("ipc-discover-agents");
    env.configure(&["org/one"], "", &[]);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    let found = client.discover_agents().await.unwrap();
    let names: Vec<&str> = found.agents.iter().map(|a| a.name.as_str()).collect();
    for builtin in ["pi", "claude", "codex"] {
        assert!(names.contains(&builtin), "{builtin} missing from {names:?}");
    }
    // The configured fake agent resolves to its absolute path.
    assert_eq!(
        found.configured["fake"].as_deref(),
        Some(env.agent_path.as_path())
    );
    assert!(found.llm_apis.is_empty(), "{:?}", found.llm_apis);
    // Nothing was executed to find that out: no agent run, no gh call.
    assert_eq!(github_writes(&env.gh_dir), 0);
    assert!(env.raw_history().is_empty());
    handle.shutdown().await;
}

// ------------------------------------------------ banners (tiers, D3)

/// Replace this env's stand-in notifiers with recording ones: every banner
/// the daemon raises appends its argv, one argument per line, to the
/// returned log, closed by an `--end--` line.
fn install_recording_notifiers(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let log = env.gh_dir.child("banners.log");
    for name in ["osascript", "notify-send"] {
        let path = env.gh_dir.child(name);
        write_file(
            &path,
            &format!(
                "#!/bin/sh\n{{ for a in \"$@\"; do printf '%s\\n' \"$a\"; done; echo '--end--'; }} >> '{}'\n",
                log.display()
            ),
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    log
}

/// The banners raised so far, each as the arguments that follow the
/// title: `[body]`, or `[body, sound]` when the tier plays one.
fn banners(log: &std::path::Path) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines
        .split(|line| *line == "--end--")
        .filter(|argv| !argv.is_empty())
        .map(|argv| {
            let title = argv
                .iter()
                .rposition(|arg| *arg == "PRX")
                .expect("every banner is titled PRX");
            argv[title + 1..].iter().map(|s| s.to_string()).collect()
        })
        .collect()
}

fn blocking_review_manifest() -> String {
    manifest_json(
        "changes_requested",
        &[&finding_json(
            Some("src/a.rs"),
            Some(3),
            "blocking",
            "fix this",
        )],
    )
}

/// With no menubar client around, the daemon raises its own banners, one
/// per event, as that event's tier says: informational events (new PR,
/// review started) without a sound, an urgent one (blocking review) with
/// the tier's sound on macOS, and silent ones (merged) not at all. A PR
/// title — untrusted text — arrives as a plain argument, quotes and all.
#[tokio::test]
async fn daemon_banners_follow_each_events_tier() {
    let _env = env_guard();
    let env = Env::new("ipc-banners");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[(
            "FAKE_AGENT_MANIFEST_JSON",
            blocking_review_manifest().as_str(),
        )],
    );
    let log = install_recording_notifiers(&env);
    // JSON-escaped in the fixture; the title itself is: Fix "it" & (more)
    let title = r#"Fix \"it\" & (more)"#;
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, title, "alice", "f/x", "aaaa", false, OLD)],
    );
    // No DaemonClient in this test: it would announce a menubar.
    let handle = env.start(fast_opts()).await;

    wait_for(
        || async { banners(&log).len() >= 3 },
        Duration::from_secs(15),
        "the new-PR, review-started and review-complete banners",
    )
    .await;
    let urgent_sound: &[&str] = if cfg!(target_os = "macos") {
        &["Basso"]
    } else {
        &[]
    };
    let expected: Vec<Vec<String>> = [
        (r#"new PR org/one#1: Fix "it" & (more)"#, &[][..]),
        ("org/one#1: review started", &[][..]),
        ("org/one#1 review: 1 blocking issue(s) found", urgent_sound),
    ]
    .iter()
    .map(|(body, sound)| {
        std::iter::once(*body)
            .chain(sound.iter().copied())
            .map(String::from)
            .collect()
    })
    .collect();
    assert_eq!(banners(&log), expected);

    // Merged is a silent-tier event: recorded, never bannered.
    write_file(&env.gh_dir.child("org_one#1.state"), "MERGED");
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    handle.shared.poll_requested.notify_one();
    wait_for(
        || async {
            env.raw_history()
                .iter()
                .any(|e| matches!(e, HistoryEntry::PrMerged { .. }))
        },
        Duration::from_secs(10),
        "the merge to be recorded",
    )
    .await;
    // One more full poll, so a banner for the merge would have been raised.
    let polled = handle.shared.last_poll_map().await["org/one"];
    handle.shared.poll_requested.notify_one();
    wait_for(
        || async { handle.shared.last_poll_map().await["org/one"] > polled },
        Duration::from_secs(10),
        "the next poll",
    )
    .await;
    assert_eq!(
        banners(&log),
        expected,
        "a silent-tier event must not banner"
    );
    handle.shutdown().await;
}

/// D3: while a menubar client has announced itself, the daemon leaves the
/// banners to it — the same events happen and are recorded, but the
/// daemon's notifier is not run.
#[tokio::test]
async fn daemon_banners_are_left_to_a_live_menubar_client() {
    let _env = env_guard();
    let env = Env::new("ipc-banners-deferred");
    env.configure(
        &["org/one"],
        COOLDOWN_ZERO,
        &[(
            "FAKE_AGENT_MANIFEST_JSON",
            blocking_review_manifest().as_str(),
        )],
    );
    let log = install_recording_notifiers(&env);
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle).as_menubar();

    // PRX.app's client announces itself as the menubar on every request.
    client.get_status().await.unwrap();
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "One", "alice", "f/x", "aaaa", false, OLD)],
    );
    client.review_now().await.unwrap();
    wait_completed_and_idle(&env, &client, 1, "the review to complete").await;

    assert_eq!(
        env.state().prs["org/one#1"].state,
        PrState::ReviewedChangesRequested
    );
    assert!(
        env.raw_history()
            .iter()
            .any(|e| matches!(e, HistoryEntry::ReviewStarted { .. })),
        "the events the banners are for did happen"
    );
    assert_eq!(banners(&log), Vec::<Vec<String>>::new());
    handle.shutdown().await;
}

// ------------------------------------------------ agent failures

/// An agent that crashes, and one that hangs past its timeout, fail the
/// review without taking the daemon with them: the PR is pending again
/// (never `in_review`), the failure is in history, the review slot is
/// free, and the next review of the same PR runs normally.
#[tokio::test]
async fn a_crashing_or_hanging_agent_fails_the_review_and_frees_the_queue() {
    let _env = env_guard();
    let env = Env::new("ipc-agent-fail");
    env.configure(
        &["org/one"],
        "  agent_timeout_seconds: 1\n",
        &[
            ("FAKE_AGENT_MANIFEST_JSON", approved_manifest().as_str()),
            ("FAKE_AGENT_EXIT", "1"),
        ],
    );
    write_list_fixture(env.gh_dir.path(), "org/one", &[cooling_pr(1)]);
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);
    tracked_pending(&env, &client, "org/one#1").await;

    let failed = |env: &Env| {
        env.history()
            .iter()
            .filter(|e| matches!(e, HistoryEntry::ReviewFailed { .. }))
            .count()
    };
    let idle_after_failures = |n: usize| {
        let (env, client) = (&env, client.clone());
        move || {
            let client = client.clone();
            async move {
                let s = client.get_status().await.unwrap();
                failed(env) >= n && s.in_flight == 0 && s.queue_depth == 0
            }
        }
    };

    // Crash: exit 1 after writing the manifest (and again on the retry).
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_for(
        idle_after_failures(1),
        Duration::from_secs(15),
        "the crashed review to fail",
    )
    .await;
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::Pending);
    assert_eq!(rec.last_verdict, None);
    assert_eq!(rec.review_attempts, 1);

    // Hang: the agent outlives its 1 s timeout and is killed.
    std::env::remove_var("FAKE_AGENT_EXIT");
    std::env::set_var("FAKE_AGENT_SLEEP", "20");
    let hanging = Instant::now();
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_for(
        idle_after_failures(2),
        Duration::from_secs(15),
        "the hung review to time out",
    )
    .await;
    assert!(
        hanging.elapsed() < Duration::from_secs(15),
        "the timeout, not the agent, must end the run"
    );
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);
    assert_eq!(
        github_writes(&env.gh_dir),
        0,
        "a failed review posts nothing"
    );

    // A healthy agent: the same PR is reviewed as if nothing had happened.
    std::env::remove_var("FAKE_AGENT_SLEEP");
    client.review(Some("org/one#1"), false, true).await.unwrap();
    wait_completed_and_idle(&env, &client, 1, "the healthy review").await;
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::ReviewedApproved);
    assert_eq!(rec.last_verdict, Some(Verdict::Approved));
    assert_eq!(count_calls(&env.gh_dir, "pr review"), 1);
    handle.shutdown().await;
}

// ------------------------------------------------------------ poll health

/// Polls that keep failing show in `/status` per repo, a TLS failure on
/// every repo raises the restart request (thresholds shrunk for the test),
/// a successful poll clears the record, and an auth failure never asks for
/// a restart.
#[tokio::test]
async fn failing_polls_are_reported_and_recommend_a_restart_only_for_environment_errors() {
    let _env = env_guard();
    let env = Env::new("ipc-poll-health");
    env.configure(&["org/one", "org/two"], "", &[]);
    for repo in ["org/one", "org/two"] {
        write_list_fixture(env.gh_dir.path(), repo, &[]);
    }
    let tls = "Post \"https://api.github.com/graphql\": tls: failed to verify certificate: \
               x509: OSStatus -26276";
    write_file(&env.gh_dir.child("list.error"), &format!("{tls}\n"));
    let handle = env
        .start(SchedulerOptions {
            health: prx::health::HealthThresholds {
                failing_after: 3,
                restart_after_streak: Duration::ZERO,
                restart_min_uptime: Duration::ZERO,
            },
            ..fast_opts()
        })
        .await;
    let client = env.client(&handle);

    wait_for(
        || async {
            client.get_status().await.is_ok_and(|s| {
                s.poll_health.len() == 2
                    && s.poll_health.values().all(|h| h.consecutive_failures >= 3)
            })
        },
        Duration::from_secs(20),
        "three failed polls per repo",
    )
    .await;
    let status = client.get_status().await.unwrap();
    let one = &status.poll_health["org/one"];
    assert_eq!(one.last_error.as_deref(), Some(tls));
    assert!(one.failing_since.is_some() && one.last_success.is_none());
    assert_eq!(one.poll_interval_minutes, 15);
    assert!(status.restart_recommended);
    tokio::time::timeout(Duration::from_secs(5), handle.shared.restart_requested())
        .await
        .expect("the restart request is raised");

    // GitHub is reachable again: the next poll clears the record.
    std::fs::remove_file(env.gh_dir.child("list.error")).unwrap();
    wait_for(
        || async {
            client.get_status().await.is_ok_and(|s| {
                s.poll_health
                    .values()
                    .all(|h| h.consecutive_failures == 0 && h.last_success.is_some())
            })
        },
        Duration::from_secs(20),
        "a successful poll",
    )
    .await;
    let status = client.get_status().await.unwrap();
    assert!(!status.restart_recommended);
    assert!(status.poll_health["org/one"].last_error.is_none());

    // Auth expiry: failing, but a restart cannot fix it.
    write_file(
        &env.gh_dir.child("list.error"),
        "gh: To get started with GitHub CLI, please run: gh auth login\n",
    );
    wait_for(
        || async {
            client
                .get_status()
                .await
                .is_ok_and(|s| s.poll_health.values().all(|h| h.consecutive_failures >= 3))
        },
        Duration::from_secs(20),
        "three failed polls per repo",
    )
    .await;
    assert!(!client.get_status().await.unwrap().restart_recommended);
    handle.shutdown().await;
}

/// The review pre-flight (`POST /review/preflight`) and `confirm`: a draft
/// warns with its skip, an oversized PR with its size against the cap, a
/// plain PR needs nothing; the pre-flight tracks nothing. Without `confirm`
/// a draft is not enqueued (an older client's behaviour); confirmed, it is
/// reviewed once and stays skipped. Mode `off` is blocked, not a warning.
#[tokio::test]
async fn preflight_warns_and_a_confirmed_review_runs_past_the_skip() {
    let _env = env_guard();
    let env = Env::new("ipc-preflight");
    let manifest = approved_manifest();
    env.configure(
        &["org/one", "org/three"],
        "  diff_max_lines: 1500\n",
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "  - name: org/three\n",
        "  - name: org/three\n    review_mode: off\n",
        1,
    );
    write_file(&env.config, &config);
    for repo in ["org/one", "org/three"] {
        write_list_fixture(env.gh_dir.path(), repo, &[]);
    }
    let gh = env.gh_dir.path();
    write_pr_fixture(gh, "org/one", 5, &fresh_pr(5, true));
    write_pr_fixture(gh, "org/one", 6, &fresh_pr(6, false));
    write_pr_fixture(gh, "org/one", 7, &fresh_pr(7, false));
    write_pr_fixture(gh, "org/three", 8, &fresh_pr(8, false));
    write_file(
        &env.gh_dir.child("org_one#6.detail.json"),
        r#"{"files":[{"path":"a"}],"additions":2000,"deletions":140,"commits":[{"oid":"aaaa"}]}"#,
    );
    write_file(
        &env.gh_dir.child("org_one#7.detail.json"),
        r#"{"files":[{"path":"a"}],"additions":10,"deletions":2,"commits":[{"oid":"aaaa"}]}"#,
    );
    let handle = env.start(fast_opts()).await;
    let client = env.client(&handle);

    let draft = client.preflight("org/one#5").await.unwrap().unwrap();
    assert!(draft.needs_confirmation(), "{draft:?}");
    assert_eq!(draft.warnings[0].message, "Skipped \u{2014} draft");
    let large = client.preflight("6").await.unwrap().unwrap();
    assert_eq!(large.key, "org/one#6");
    assert_eq!(
        large.warnings[0].message,
        "Large PR \u{2014} 2,140 lines changed (limit 1,500)"
    );
    assert_eq!(large.size.unwrap().lines, 2140);
    let plain = client.preflight("org/one#7").await.unwrap().unwrap();
    assert!(!plain.needs_confirmation(), "{plain:?}");
    assert_eq!(plain.size.unwrap().lines, 12);
    let off = client.preflight("org/three#8").await.unwrap().unwrap();
    assert_eq!(
        off.blocked.as_deref(),
        Some("review mode is off for org/three")
    );
    assert!(!off.needs_confirmation());
    assert!(env.state().prs.is_empty(), "the pre-flight tracks nothing");

    // Unconfirmed (an older client): tracked as skipped, not enqueued.
    let plain_opts = ReviewOverrides::default();
    let resp = client
        .review_with("org/one#5", true, &plain_opts)
        .await
        .unwrap();
    assert!(resp.enqueued.is_empty(), "{resp:?}");
    assert!(
        resp.note
            .unwrap_or_default()
            .contains("skipped \u{2014} draft"),
        "the note names the skip"
    );
    let rec = env.state().prs["org/one#5"].clone();
    assert_eq!(rec.skip_reason, Some(SkipReason::Draft));
    let status = client.get_status().await.unwrap();
    let entry = status.prs.iter().find(|e| e.key == "org/one#5").unwrap();
    assert_eq!(entry.skip_detail.as_deref(), Some("draft"));
    assert_eq!(entry.summary, "Skipped \u{2014} draft");

    // Confirmed: reviewed once, and still skipped as a draft.
    let resp = client
        .review_confirmed("org/one#5", true, &plain_opts, true)
        .await
        .unwrap();
    assert_eq!(resp.enqueued, vec!["org/one#5".to_string()]);
    wait_completed_and_idle(&env, &client, 1, "the confirmed review").await;
    let rec = env.state().prs["org/one#5"].clone();
    assert_eq!(rec.state, PrState::Skipped);
    assert_eq!(rec.skip_reason, Some(SkipReason::Draft));
    assert!(rec.last_verdict.is_some(), "{rec:?}");

    // Mode off declines, confirmed or not.
    let resp = client
        .review_confirmed("org/three#8", true, &plain_opts, true)
        .await
        .unwrap();
    assert!(resp.enqueued.is_empty());
    assert!(resp.note.unwrap_or_default().contains("review mode is off"));
    handle.shutdown().await;
}
