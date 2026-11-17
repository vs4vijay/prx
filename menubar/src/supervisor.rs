//! Daemon supervision in the app (design A3, the Ollama model).
//!
//! While PRX.app runs, a daemon runs. On start the app attaches to a daemon
//! that already runs (started by the CLI, launchd, or an earlier app) and
//! never spawns a second one. Otherwise it spawns the bundled `prx daemon
//! start --foreground` (PRX.app's `Contents/Helpers/prx`) with the
//! login-shell `PATH` ([`crate::shellenv`]) and records the owned pid.
//!
//! - Before a spawn the app creates the default config when none exists
//!   ([`ensure_first_run_config`]), so a first launch needs no `prx init`.
//! - A spawned daemon is *Starting* until it is ready (its pid file and a
//!   fresh IPC token, or discovery answering with its pid), then *Running*.
//!   The app nudges a poll then, so the menu turns online at once.
//! - Every [`WATCH_INTERVAL`] the app probes the daemon, owned or adopted.
//! - A daemon that goes away, owned (any exit) or adopted (gone on
//!   [`VANISH_PROBES`] probes in a row), is restarted with exponential
//!   backoff (1 s, 2 s, 4 s, … capped at 60 s). After [`MAX_FAILURES`]
//!   failures inside [`FAILURE_WINDOW`] the supervisor gives up and reports
//!   the daemon stopped (menu: "Start daemon", "Show log").
//! - Except a deliberate stop: `prx daemon stop` leaves a stop marker
//!   ([`prx::paths::daemon_stop_marker`]), and then the app shows *Daemon
//!   stopped* and waits for "Start daemon". A daemon that someone starts
//!   meanwhile is adopted.
//! - A non-zero exit while ANOTHER daemon is alive (the pid-file guard's
//!   loser, A16) is not a crash: the app attaches to the winner.
//! - A daemon whose `/status` says `restart_recommended` (its polls have
//!   failed in a way a fresh process can fix, see [`prx::health`]) is
//!   replaced: SIGTERM, then a spawn. [`HealthPolicy`] allows that once per
//!   [`HEALTH_RESTART_GAP`], doubling up to [`HEALTH_RESTART_MAX_GAP`] while
//!   the daemon stays unhealthy. This is what recovers a daemon orphaned by
//!   an earlier app in a stale system session.
//! - Quit SIGTERMs only the owned daemon and waits for its graceful exit;
//!   "Quit PRX (keep reviewing)" and an external daemon are left running.
//!
//! The restart policies are pure ([`RestartPolicy`], [`HealthPolicy`]) so
//! their backoff and caps are unit-tested; the thread around them only
//! probes, spawns, signals, and reports.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Failures that make the supervisor give up when they all fall inside
/// [`FAILURE_WINDOW`].
pub const MAX_FAILURES: usize = 5;
/// The sliding window the failure cap counts in.
pub const FAILURE_WINDOW: Duration = Duration::from_secs(10 * 60);
/// First restart delay; doubles per failure in the window.
pub const BASE_DELAY: Duration = Duration::from_secs(1);
/// Restart delay ceiling.
pub const MAX_DELAY: Duration = Duration::from_secs(60);
/// How long Quit waits for the owned daemon's graceful shutdown (the same
/// window `prx daemon stop` grants before escalating).
pub const STOP_GRACE: Duration = Duration::from_secs(10);
/// How often the daemon is probed while one runs (or should).
pub const WATCH_INTERVAL: Duration = Duration::from_secs(5);
/// How often a starting daemon is checked for readiness.
pub const READY_INTERVAL: Duration = Duration::from_millis(250);
/// Probes in a row that must find no daemon before an adopted one counts
/// as gone (a daemon that is just starting claims its pid file first).
pub const VANISH_PROBES: u32 = 2;
/// The shortest gap between two health replacements.
pub const HEALTH_RESTART_GAP: Duration = Duration::from_secs(30 * 60);
/// The longest gap between two health replacements.
pub const HEALTH_RESTART_MAX_GAP: Duration = Duration::from_secs(4 * 60 * 60);
/// Bound on one discovery probe (a hung daemon must not stall supervision).
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// What the menu shows about the daemon's supervision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// Not owned by the app: attached to an external daemon, or none seen
    /// yet.
    Unmanaged,
    /// The app spawned its daemon and is waiting for it to be ready.
    Starting,
    /// The app's own daemon is running.
    Running { pid: u32 },
    /// The daemon went away; a restart is scheduled.
    Restarting { delay_secs: u64, failures: usize },
    /// No daemon, and none is started automatically: a deliberate stop
    /// (`gave_up: false`) or the failure cap.
    Stopped { gave_up: bool },
}

/// One restart decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    RestartAfter(Duration),
    GiveUp,
}

/// Exponential backoff with a failure cap over a sliding window.
#[derive(Debug, Clone, Default)]
pub struct RestartPolicy {
    failures: VecDeque<Instant>,
}

impl RestartPolicy {
    /// Record an unexpected exit at `now` and decide what to do.
    pub fn on_failure(&mut self, now: Instant) -> Decision {
        while self
            .failures
            .front()
            .is_some_and(|at| now.duration_since(*at) >= FAILURE_WINDOW)
        {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        let count = self.failures.len();
        if count >= MAX_FAILURES {
            return Decision::GiveUp;
        }
        let factor = 1u32 << (count - 1).min(16);
        Decision::RestartAfter(BASE_DELAY.saturating_mul(factor).min(MAX_DELAY))
    }

    /// Failures currently inside the window.
    pub fn failures(&self) -> usize {
        self.failures.len()
    }

    /// A manual "Start daemon" starts a fresh budget.
    pub fn reset(&mut self) {
        self.failures.clear();
    }
}

/// How often an unhealthy daemon may be replaced: once per gap, the gap
/// doubling from [`HEALTH_RESTART_GAP`] to [`HEALTH_RESTART_MAX_GAP`] while
/// the daemon never becomes healthy in between, so a lasting outage (a
/// network that is really down) costs a few restarts a day, not a loop.
#[derive(Debug, Clone)]
pub struct HealthPolicy {
    gap: Duration,
    next_allowed: Option<Instant>,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        HealthPolicy {
            gap: HEALTH_RESTART_GAP,
            next_allowed: None,
        }
    }
}

impl HealthPolicy {
    /// May the daemon be replaced at `now`? A yes uses up this gap.
    pub fn allow(&mut self, now: Instant) -> bool {
        if self.next_allowed.is_some_and(|at| now < at) {
            return false;
        }
        self.next_allowed = Some(now + self.gap);
        self.gap = self.gap.saturating_mul(2).min(HEALTH_RESTART_MAX_GAP);
        true
    }

    /// The daemon synced again (or the user restarted it): fresh budget.
    pub fn reset(&mut self) {
        *self = HealthPolicy::default();
    }
}

/// What one probe of a daemon that answers learned from `/status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub pid: u32,
    /// The daemon recommends a restart (`restart_recommended`).
    pub restart_recommended: bool,
    /// Every polled repo has synced and none is failing.
    pub healthy: bool,
}

impl Answer {
    fn of(status: &prx::ipc::StatusResponse) -> Answer {
        let thresholds = prx::health::HealthThresholds::default();
        Answer {
            pid: status.pid,
            restart_recommended: status.restart_recommended,
            healthy: !status.poll_health.is_empty()
                && status
                    .poll_health
                    .values()
                    .all(|h| h.last_success.is_some() && !h.is_failing(&thresholds)),
        }
    }
}

/// Asks the running daemon's `/status` (None: nothing answers).
type AskFn = dyn Fn() -> Option<Answer> + Send;

enum Cmd {
    /// Manual "Start daemon".
    Start,
    /// Manual "Restart daemon": replace whatever runs with a fresh daemon.
    Restart,
    /// The owned child exited.
    Exited { pid: u32 },
    /// Quit: stop the owned daemon when `stop`, then acknowledge.
    Shutdown { stop: bool, done: Sender<()> },
}

/// Handle to the supervisor thread (cheap to clone).
#[derive(Clone)]
pub struct Supervisor {
    tx: Sender<Cmd>,
}

impl Supervisor {
    /// Start supervising: attach-first, else spawn `prx` (when known).
    /// `on_state` runs on the supervisor thread for every state change.
    pub fn spawn(
        prx: Option<PathBuf>,
        on_state: impl Fn(DaemonState) + Send + 'static,
    ) -> Supervisor {
        Supervisor::spawn_with(prx, Box::new(ask_daemon), on_state)
    }

    /// [`Supervisor::spawn`] with the `/status` probe supplied (tests).
    fn spawn_with(
        prx: Option<PathBuf>,
        ask: Box<AskFn>,
        on_state: impl Fn(DaemonState) + Send + 'static,
    ) -> Supervisor {
        let (tx, rx) = mpsc::channel();
        let worker = Worker {
            prx,
            tx: tx.clone(),
            owned: Arc::new(Mutex::new(None)),
            policy: RestartPolicy::default(),
            health: HealthPolicy::default(),
            state: DaemonState::Unmanaged,
            on_state: Box::new(on_state),
            ask,
            restart_at: None,
            next_watch: Instant::now() + WATCH_INTERVAL,
            ready_wait: None,
            replacing: None,
            missing: 0,
        };
        std::thread::Builder::new()
            .name("prx-menubar-supervisor".into())
            .spawn(move || worker.run(rx))
            .expect("failed to spawn the supervisor thread");
        Supervisor { tx }
    }

    /// Menu "Start daemon": a fresh restart budget and an attach-or-spawn.
    pub fn start_daemon(&self) {
        let _ = self.tx.send(Cmd::Start);
    }

    /// Menu "Restart daemon" (the poll-health line): stop the running
    /// daemon, owned or not, and start a fresh one.
    pub fn restart_daemon(&self) {
        let _ = self.tx.send(Cmd::Restart);
    }

    /// Quit: with `stop_owned`, SIGTERM the owned daemon and wait for it
    /// (bounded by [`STOP_GRACE`] plus a margin). An external daemon is
    /// never touched. Blocks the caller until done.
    pub fn shutdown(&self, stop_owned: bool) {
        let (done, wait) = mpsc::channel();
        if self
            .tx
            .send(Cmd::Shutdown {
                stop: stop_owned,
                done,
            })
            .is_ok()
        {
            let _ = wait.recv_timeout(STOP_GRACE + Duration::from_secs(2));
        }
    }
}

struct Worker {
    prx: Option<PathBuf>,
    tx: Sender<Cmd>,
    owned: Arc<Mutex<Option<u32>>>,
    policy: RestartPolicy,
    health: HealthPolicy,
    state: DaemonState,
    on_state: Box<dyn Fn(DaemonState) + Send>,
    ask: Box<AskFn>,
    restart_at: Option<Instant>,
    next_watch: Instant,
    /// The owned daemon being waited on for readiness, and when it was
    /// spawned.
    ready_wait: Option<(u32, SystemTime)>,
    /// A daemon this supervisor stopped to replace it: its exit or
    /// disappearance spawns a fresh one without counting as a failure.
    replacing: Option<u32>,
    /// Probes in a row that found no daemon (adopted daemons only).
    missing: u32,
}

impl Worker {
    fn run(mut self, rx: Receiver<Cmd>) {
        self.attach_or_spawn();
        loop {
            let now = Instant::now();
            let wake = match self.restart_at {
                Some(at) => at.min(self.next_watch),
                None => self.next_watch,
            };
            match rx.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(Cmd::Start) => {
                    self.policy.reset();
                    self.restart_at = None;
                    if self.owned_pid().is_none() {
                        self.attach_or_spawn();
                    }
                }
                Ok(Cmd::Restart) => self.restart_now(),
                Ok(Cmd::Exited { pid }) => self.exited(pid),
                Ok(Cmd::Shutdown { stop, done }) => {
                    if stop {
                        self.stop_owned(&rx);
                    } else if let Some(pid) = self.owned_pid() {
                        eprintln!("prx-menubar: quitting; daemon pid {pid} keeps running");
                    }
                    let _ = done.send(());
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {
                    let now = Instant::now();
                    if self.restart_at.is_some_and(|at| now >= at) {
                        self.restart_at = None;
                        self.attach_or_spawn();
                    }
                    if now >= self.next_watch {
                        self.watch();
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn owned_pid(&self) -> Option<u32> {
        self.owned.lock().ok().and_then(|o| *o)
    }

    fn set_owned(&self, pid: Option<u32>) {
        if let Ok(mut owned) = self.owned.lock() {
            *owned = pid;
        }
    }

    fn set_state(&mut self, state: DaemonState) {
        if state != self.state {
            eprintln!("prx-menubar: daemon supervision: {state:?}");
            self.state = state.clone();
            (self.on_state)(state);
        }
    }

    fn schedule_watch(&mut self) {
        let every = if self.ready_wait.is_some() {
            READY_INTERVAL
        } else {
            WATCH_INTERVAL
        };
        self.next_watch = Instant::now() + every;
    }

    /// A3 attach-first: a daemon that answers (or a live pid-file owner)
    /// is used as is; otherwise spawn the bundled one.
    fn attach_or_spawn(&mut self) {
        self.missing = 0;
        if other_daemon_alive(None) {
            eprintln!("prx-menubar: a daemon is already running; attaching (not spawning)");
            self.set_state(DaemonState::Unmanaged);
            self.schedule_watch();
            return;
        }
        self.spawn_own();
    }

    fn spawn_own(&mut self) {
        let Some(prx) = self.prx.clone() else {
            eprintln!("prx-menubar: no bundled prx binary found; cannot start a daemon");
            self.set_state(DaemonState::Stopped { gave_up: false });
            return;
        };
        self.set_state(DaemonState::Starting);
        ensure_first_run_config();
        // The app starts this daemon on purpose: an old stop marker must
        // not turn a failed start into a "deliberate" stop.
        let _ = std::fs::remove_file(prx::paths::daemon_stop_marker());
        let spawned_at = SystemTime::now();
        match spawn_daemon(&prx) {
            Ok(child) => {
                let pid = child.id();
                eprintln!("prx-menubar: spawned daemon pid {pid} ({})", prx.display());
                self.set_owned(Some(pid));
                let tx = self.tx.clone();
                std::thread::Builder::new()
                    .name("prx-menubar-daemon-wait".into())
                    .spawn(move || {
                        let mut child = child;
                        let _ = child.wait();
                        let _ = tx.send(Cmd::Exited { pid });
                    })
                    .expect("failed to spawn the daemon waiter thread");
                self.ready_wait = Some((pid, spawned_at));
                self.schedule_watch();
            }
            Err(error) => {
                eprintln!("prx-menubar: cannot spawn the daemon: {error}");
                self.failed();
            }
        }
    }

    /// One probe: readiness of a starting daemon, the disappearance of an
    /// adopted one, a daemon appearing while none is expected, and the
    /// health of whatever answers.
    fn watch(&mut self) {
        let owned = self.owned_pid();
        if let Some((pid, spawned_at)) = self.ready_wait {
            if owned != Some(pid) {
                self.ready_wait = None;
            } else if daemon_ready(pid, spawned_at, || (self.ask)()) {
                eprintln!("prx-menubar: daemon pid {pid} is ready");
                self.ready_wait = None;
                self.set_state(DaemonState::Running { pid });
            }
            self.schedule_watch();
            return;
        }
        let pid_owner = prx::daemon::pid_file_owner(&prx::paths::daemon_pid_file());
        let answer = (self.ask)();
        if pid_owner.is_some() || answer.is_some() {
            self.missing = 0;
            if owned.is_none() {
                if self.restart_at.take().is_some()
                    || matches!(self.state, DaemonState::Stopped { .. })
                {
                    eprintln!("prx-menubar: a daemon started elsewhere; attaching");
                }
                self.set_state(DaemonState::Unmanaged);
            }
            if let Some(answer) = answer {
                self.check_health(answer);
            }
        } else if owned.is_none() && self.state == DaemonState::Unmanaged {
            // The adopted daemon (or the one being replaced) is gone.
            self.missing += 1;
            if self.missing >= VANISH_PROBES {
                self.missing = 0;
                self.vanished();
            }
        }
        self.schedule_watch();
    }

    /// An adopted daemon is gone: replace it unless the user stopped it.
    fn vanished(&mut self) {
        if self.replacing.take().is_some() {
            eprintln!("prx-menubar: the replaced daemon is gone; starting a fresh one");
            self.attach_or_spawn();
        } else if stop_requested() {
            eprintln!("prx-menubar: the daemon was stopped with `prx daemon stop`; not restarting");
            self.set_state(DaemonState::Stopped { gave_up: false });
        } else {
            eprintln!("prx-menubar: the daemon this app attached to is gone; starting one");
            self.failed();
        }
    }

    /// Replace a daemon that recommends it, within [`HealthPolicy`].
    fn check_health(&mut self, answer: Answer) {
        if answer.healthy {
            self.health.reset();
            return;
        }
        if !answer.restart_recommended || self.replacing.is_some() {
            return;
        }
        if !self.health.allow(Instant::now()) {
            return;
        }
        eprintln!(
            "prx-menubar: daemon pid {} keeps failing to sync and recommends a restart; replacing it",
            answer.pid
        );
        self.replace(answer.pid);
    }

    /// SIGTERM `pid` (the owned daemon, or the pid-file owner) so a fresh
    /// daemon replaces it once it is gone.
    fn replace(&mut self, pid: u32) {
        let owned = self.owned_pid() == Some(pid);
        let owner = prx::daemon::pid_file_owner(&prx::paths::daemon_pid_file()) == Some(pid);
        if !owned && !owner {
            eprintln!("prx-menubar: pid {pid} is not the daemon of this data dir; not signalling");
            return;
        }
        self.replacing = Some(pid);
        self.set_state(DaemonState::Unmanaged);
        signal(pid, libc::SIGTERM);
    }

    /// Manual "Restart daemon".
    fn restart_now(&mut self) {
        self.policy.reset();
        self.health.reset();
        self.restart_at = None;
        let running = self
            .owned_pid()
            .or_else(|| prx::daemon::pid_file_owner(&prx::paths::daemon_pid_file()));
        match running {
            Some(pid) => {
                eprintln!("prx-menubar: restart requested; stopping daemon pid {pid}");
                self.replace(pid);
            }
            None => self.attach_or_spawn(),
        }
    }

    fn exited(&mut self, pid: u32) {
        if self.owned_pid() != Some(pid) {
            return;
        }
        self.set_owned(None);
        self.ready_wait = None;
        if self.replacing == Some(pid) {
            self.replacing = None;
            eprintln!("prx-menubar: replaced daemon pid {pid} exited; starting a fresh one");
            self.attach_or_spawn();
            return;
        }
        if other_daemon_alive(Some(pid)) {
            eprintln!(
                "prx-menubar: daemon pid {pid} exited; another daemon is running — attaching"
            );
            self.set_state(DaemonState::Unmanaged);
            self.schedule_watch();
            return;
        }
        if stop_requested() {
            eprintln!(
                "prx-menubar: daemon pid {pid} was stopped with `prx daemon stop`; not restarting"
            );
            self.set_state(DaemonState::Stopped { gave_up: false });
            return;
        }
        eprintln!("prx-menubar: daemon pid {pid} exited unexpectedly");
        self.failed();
    }

    fn failed(&mut self) {
        match self.policy.on_failure(Instant::now()) {
            Decision::RestartAfter(delay) => {
                self.restart_at = Some(Instant::now() + delay);
                self.set_state(DaemonState::Restarting {
                    delay_secs: delay.as_secs(),
                    failures: self.policy.failures(),
                });
            }
            Decision::GiveUp => {
                eprintln!(
                    "prx-menubar: {MAX_FAILURES} daemon failures within {} min; giving up",
                    FAILURE_WINDOW.as_secs() / 60
                );
                self.restart_at = None;
                self.set_state(DaemonState::Stopped { gave_up: true });
            }
        }
    }

    /// SIGTERM the owned daemon and wait for its exit report (bounded).
    fn stop_owned(&mut self, rx: &Receiver<Cmd>) {
        let Some(pid) = self.owned_pid() else {
            eprintln!("prx-menubar: quitting; no owned daemon to stop");
            return;
        };
        eprintln!("prx-menubar: quitting; stopping owned daemon pid {pid}");
        signal(pid, libc::SIGTERM);
        let deadline = Instant::now() + STOP_GRACE;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Cmd::Exited { pid: exited }) if exited == pid => {
                    eprintln!("prx-menubar: owned daemon pid {pid} stopped");
                    self.set_owned(None);
                    return;
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        eprintln!(
            "prx-menubar: daemon pid {pid} still shutting down after {STOP_GRACE:?}; leaving it"
        );
    }
}

/// A spawned daemon is ready once it answers discovery with its pid, or —
/// what `prx daemon start` waits for — its pid file names it and its IPC
/// token was written after the spawn.
fn daemon_ready(pid: u32, spawned_at: SystemTime, ask: impl Fn() -> Option<Answer>) -> bool {
    let claimed = prx::daemon::read_pid(&prx::paths::daemon_pid_file()) == Some(pid);
    let token_fresh = std::fs::metadata(prx::paths::daemon_token_file())
        .and_then(|m| m.modified())
        .is_ok_and(|at| at >= spawned_at - Duration::from_secs(1));
    (claimed && token_fresh) || ask().is_some_and(|answer| answer.pid == pid)
}

/// `prx daemon stop` stopped the last daemon on purpose.
fn stop_requested() -> bool {
    prx::paths::daemon_stop_marker().exists()
}

/// The bundled CLI/daemon: PRX.app's `Contents/Helpers/prx` when running
/// from the bundle, else the `prx` next to this executable (a cargo build or
/// install dir).
pub fn bundled_prx() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if let Some(bundle) = prx::autostart::enclosing_bundle(&exe) {
        let helper = bundle.join(prx::autostart::APP_CLI);
        return helper.is_file().then_some(helper);
    }
    let sibling = exe.parent()?.join("prx");
    sibling.is_file().then_some(sibling)
}

/// First launch: the daemon refuses to start without a config file, and an
/// app user is never asked to run `prx init`. So before every spawn the app
/// makes sure one exists, through the same library call `prx init` uses
/// ([`prx::config::ensure_starter_config`]: one definition of the default
/// config, non-interactive, and it never touches an existing file). An
/// invalid existing config is therefore left alone and the daemon's own
/// error stays visible ("Daemon stopped", "Show log"). A failure to write is
/// logged and the spawn goes ahead, so the daemon reports the real problem.
fn ensure_first_run_config() {
    let path = prx::paths::config_path();
    match prx::config::ensure_starter_config(&path) {
        Ok(true) => eprintln!(
            "prx-menubar: first run: no config found; wrote the default config to {}",
            path.display()
        ),
        Ok(false) => {}
        Err(error) => eprintln!("prx-menubar: cannot create the default config: {error}"),
    }
}

/// Spawn `prx daemon start --foreground` detached from our process group,
/// logging to the daemon's own log files (`PRX_DAEMON_CHILD` keeps it off
/// stderr) with early errors (bad config, panics) captured beside them.
fn spawn_daemon(prx: &Path) -> std::io::Result<std::process::Child> {
    let logs = prx::paths::logs_dir();
    let _ = std::fs::create_dir_all(&logs);
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(stderr_log_path())
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut command = Command::new(prx);
    command
        .args(["daemon", "start", "--foreground"])
        .env("PATH", crate::shellenv::login_path())
        .env("PRX_DAEMON_CHILD", "1")
        // This app restarts the daemon when it exits non-zero, so the
        // daemon may exit for a fresh start (see `prx::health`).
        .env(prx::daemon::SUPERVISED_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr);
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn()
}

/// Where the spawned daemon's stderr goes.
pub fn stderr_log_path() -> PathBuf {
    prx::paths::logs_dir().join("daemon-stderr.log")
}

/// The log "Show log" opens: the newest daemon log, else the stderr log.
pub fn log_to_show() -> PathBuf {
    prx::daemon::newest_log_file(&prx::paths::logs_dir()).unwrap_or_else(stderr_log_path)
}

/// A live daemon other than `except`: the pid file names the live process
/// that wrote it (not one that reused its pid after a reboot), or
/// discovery answers.
fn other_daemon_alive(except: Option<u32>) -> bool {
    if let Some(pid) = prx::daemon::pid_file_owner(&prx::paths::daemon_pid_file()) {
        if Some(pid) != except {
            return true;
        }
    }
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return false;
    };
    runtime
        .block_on(async {
            tokio::time::timeout(PROBE_TIMEOUT, prx::ipc::DaemonClient::discover()).await
        })
        .ok()
        .flatten()
        .is_some()
}

/// The production `/status` probe (bounded by [`PROBE_TIMEOUT`]).
fn ask_daemon() -> Option<Answer> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async {
        tokio::time::timeout(PROBE_TIMEOUT, async {
            let client = prx::ipc::DaemonClient::discover().await?;
            client.get_status().await.ok().map(|s| Answer::of(&s))
        })
        .await
        .ok()
        .flatten()
    })
}

#[cfg(test)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 only probes for existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn signal(pid: u32, sig: i32) {
    // SAFETY: plain kill(2) with a constant signal number.
    let _ = unsafe { libc::kill(pid as i32, sig) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a fake daemon does to look ready, as the real one does once
    /// its IPC API is bound: claim the pid file, then publish a token.
    const FAKE_READY: &str = "echo $$ > \"$PRX_DATA_DIR/daemon.pid\"\n\
                              echo token > \"$PRX_DATA_DIR/daemon.token\"\n";

    #[test]
    fn health_replacements_back_off_from_thirty_minutes_to_four_hours() {
        let mut policy = HealthPolicy::default();
        let t0 = Instant::now();
        assert!(policy.allow(t0));
        assert!(!policy.allow(t0 + Duration::from_secs(29 * 60)));
        assert!(policy.allow(t0 + HEALTH_RESTART_GAP));
        // The next gap is an hour, then two, then four, and stays there.
        let t1 = t0 + HEALTH_RESTART_GAP;
        assert!(!policy.allow(t1 + Duration::from_secs(59 * 60)));
        assert!(policy.allow(t1 + Duration::from_secs(60 * 60)));
        let mut at = t1 + Duration::from_secs(60 * 60);
        for gap_hours in [2, 4, 4, 4] {
            let gap = Duration::from_secs(gap_hours * 3600);
            assert!(!policy.allow(at + gap - Duration::from_secs(1)));
            at += gap;
            assert!(policy.allow(at));
        }
        // Healthy again: the next failure may be handled at once.
        policy.reset();
        assert!(policy.allow(at + Duration::from_secs(1)));
    }

    #[test]
    fn answers_are_healthy_only_when_every_repo_synced() {
        use prx::health::RepoPollHealth;
        let mut status: prx::ipc::StatusResponse = serde_json::from_value(serde_json::json!({
            "version": "0", "pid": 7, "uptime_seconds": 1, "in_flight": 0,
            "queue_depth": 0, "last_poll": {}, "last_state_poll": null, "prs": []
        }))
        .unwrap();
        assert!(!Answer::of(&status).healthy, "nothing polled yet");
        let mut ok = RepoPollHealth::default();
        ok.record_success(chrono::Utc::now());
        status.poll_health.insert("org/a".into(), ok);
        assert!(Answer::of(&status).healthy);
        let mut bad = RepoPollHealth::default();
        for _ in 0..3 {
            bad.record_failure(chrono::Utc::now(), "tls: bad certificate");
        }
        status.poll_health.insert("org/b".into(), bad);
        status.restart_recommended = true;
        let answer = Answer::of(&status);
        assert!(!answer.healthy && answer.restart_recommended);
        assert_eq!(answer.pid, 7);
    }

    /// A fake daemon this test starts itself (as if the CLI or an earlier
    /// app had): it claims the pid file, exits 0 on SIGTERM, and is reaped
    /// by a thread so a dead one never lingers as a zombie.
    fn external_daemon(dir: &Path) -> u32 {
        let fake = dir.join("external-prx");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\n{FAKE_READY}trap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut child = Command::new(&fake).spawn().unwrap();
        let pid = child.id();
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while prx::daemon::pid_file_owner(&prx::paths::daemon_pid_file()) != Some(pid) {
            assert!(
                Instant::now() < deadline,
                "the external daemon claims the pid file"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        pid
    }

    fn fake_prx(dir: &Path) -> PathBuf {
        let fake = dir.join("prx");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\n{FAKE_READY}trap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    fn wait_state(rx: &Receiver<DaemonState>, want: impl Fn(&DaemonState) -> bool) -> DaemonState {
        loop {
            let state = rx
                .recv_timeout(Duration::from_secs(30))
                .expect("state change");
            if want(&state) {
                return state;
            }
        }
    }

    /// The first-launch bug: an app that attached to a daemon (one still
    /// shutting down, or started elsewhere) never looked again, so when it
    /// exited the menu sat at "Daemon offline" until the user clicked Start.
    /// Now the app notices and starts its own.
    #[test]
    fn an_adopted_daemon_that_goes_away_is_replaced() {
        let dir = sandbox("adopted");
        in_sandbox(&dir.clone(), move || {
            let external = external_daemon(&dir);
            let (tx, rx) = mpsc::channel();
            let supervisor = Supervisor::spawn(Some(fake_prx(&dir)), move |s| {
                let _ = tx.send(s);
            });
            std::thread::sleep(Duration::from_millis(300));
            signal(external, libc::SIGKILL);
            let restarting = wait_state(&rx, |s| !matches!(s, DaemonState::Unmanaged));
            assert!(
                matches!(restarting, DaemonState::Restarting { failures: 1, .. }),
                "{restarting:?}"
            );
            let DaemonState::Running { pid } =
                wait_state(&rx, |s| matches!(s, DaemonState::Running { .. }))
            else {
                unreachable!()
            };
            assert_ne!(pid, external);
            supervisor.shutdown(true);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// `prx daemon stop` is respected: no restart, "Daemon stopped".
    #[test]
    fn a_deliberately_stopped_daemon_is_not_restarted() {
        let dir = sandbox("deliberate");
        in_sandbox(&dir.clone(), move || {
            let external = external_daemon(&dir);
            let (tx, rx) = mpsc::channel();
            let supervisor = Supervisor::spawn(Some(fake_prx(&dir)), move |s| {
                let _ = tx.send(s);
            });
            std::thread::sleep(Duration::from_millis(300));
            std::fs::write(prx::paths::daemon_stop_marker(), format!("{external}\n")).unwrap();
            signal(external, libc::SIGTERM);
            assert_eq!(
                wait_state(&rx, |_| true),
                DaemonState::Stopped { gave_up: false }
            );
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(6)).ok(),
                None,
                "nothing is started"
            );
            // "Start daemon" starts one, and clears the marker.
            supervisor.start_daemon();
            wait_state(&rx, |s| matches!(s, DaemonState::Running { .. }));
            assert!(!prx::paths::daemon_stop_marker().exists());
            supervisor.shutdown(true);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// The incident: an orphaned daemon that cannot reach GitHub reports
    /// `restart_recommended`; the app stops it and starts a fresh one, and
    /// (the health gap keeps it from doing so again soon).
    #[test]
    fn a_daemon_that_recommends_a_restart_is_replaced() {
        let dir = sandbox("unhealthy");
        in_sandbox(&dir.clone(), move || {
            let external = external_daemon(&dir);
            let ask = move || {
                process_alive(external).then_some(Answer {
                    pid: external,
                    restart_recommended: true,
                    healthy: false,
                })
            };
            let (tx, rx) = mpsc::channel();
            let supervisor =
                Supervisor::spawn_with(Some(fake_prx(&dir)), Box::new(ask), move |s| {
                    let _ = tx.send(s);
                });
            let DaemonState::Running { pid } =
                wait_state(&rx, |s| matches!(s, DaemonState::Running { .. }))
            else {
                unreachable!()
            };
            assert_ne!(pid, external);
            assert!(!process_alive(external), "the unhealthy daemon was stopped");
            supervisor.shutdown(true);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn backoff_doubles_from_one_second() {
        let mut policy = RestartPolicy::default();
        let t0 = Instant::now();
        assert_eq!(
            policy.on_failure(t0),
            Decision::RestartAfter(Duration::from_secs(1))
        );
        assert_eq!(
            policy.on_failure(t0 + Duration::from_secs(2)),
            Decision::RestartAfter(Duration::from_secs(2))
        );
        assert_eq!(
            policy.on_failure(t0 + Duration::from_secs(5)),
            Decision::RestartAfter(Duration::from_secs(4))
        );
        assert_eq!(
            policy.on_failure(t0 + Duration::from_secs(10)),
            Decision::RestartAfter(Duration::from_secs(8))
        );
    }

    #[test]
    fn five_failures_within_ten_minutes_give_up() {
        let mut policy = RestartPolicy::default();
        let t0 = Instant::now();
        for i in 0..4 {
            assert!(matches!(
                policy.on_failure(t0 + Duration::from_secs(60 * i)),
                Decision::RestartAfter(_)
            ));
        }
        assert_eq!(
            policy.on_failure(t0 + Duration::from_secs(9 * 60)),
            Decision::GiveUp
        );
    }

    #[test]
    fn failures_older_than_the_window_do_not_count() {
        let mut policy = RestartPolicy::default();
        let t0 = Instant::now();
        for i in 0..4 {
            policy.on_failure(t0 + Duration::from_secs(i));
        }
        // Ten minutes after the first four, the budget is fresh again.
        let later = t0 + FAILURE_WINDOW + Duration::from_secs(10);
        assert_eq!(
            policy.on_failure(later),
            Decision::RestartAfter(Duration::from_secs(1))
        );
        assert_eq!(policy.failures(), 1);
    }

    #[test]
    fn the_delay_is_capped_at_sixty_seconds() {
        // The cap only binds when failures spread out enough to stay under
        // the give-up count; check the arithmetic directly.
        let factor = 1u32 << 9;
        assert_eq!(BASE_DELAY.saturating_mul(factor).min(MAX_DELAY), MAX_DELAY);
    }

    /// End to end with a fake `prx` (a shell loop that exits 0 on SIGTERM):
    /// spawn, kill -9 → restart, quit stops it, quit-keep leaves it, and a
    /// live daemon pid file means attach, never spawn. `PRX_DATA_DIR`
    /// points at a sandbox (serialized with the other env-pointing tests).
    #[test]
    fn supervises_a_real_child_process() {
        let dir = std::env::temp_dir().join(format!("prx-supervisor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        in_sandbox(&dir.clone(), move || supervise_in(&dir));
    }

    /// `PRX_DATA_DIR` and `PRX_CONFIG` both pointed into `dir` (the spawn
    /// path now creates a missing config, which must never be the real
    /// one), serialized with the other env-pointing tests.
    fn in_sandbox<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        crate::actions::tests::with_data_dir(dir, || {
            let previous = std::env::var("PRX_CONFIG").ok();
            std::env::set_var("PRX_CONFIG", dir.join("config").join("config.yml"));
            let out = f();
            match previous {
                Some(value) => std::env::set_var("PRX_CONFIG", value),
                None => std::env::remove_var("PRX_CONFIG"),
            }
            out
        })
    }

    fn sandbox(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prx-firstrun-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A fake `prx` with the real daemon's start rule: exit 1 when the
    /// config is missing or does not parse (here: lacks a `repos:` key),
    /// otherwise run until SIGTERM.
    fn config_checking_prx(dir: &Path) -> PathBuf {
        let fake = dir.join("prx");
        std::fs::write(
            &fake,
            "#!/bin/sh\n\
             [ -f \"$PRX_CONFIG\" ] || { echo 'config file not found' >&2; exit 1; }\n\
             grep -q '^repos:' \"$PRX_CONFIG\" || { echo 'failed to parse config' >&2; exit 1; }\n\
             {FAKE_READY}trap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n"
                .replace("{FAKE_READY}", FAKE_READY),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        fake
    }

    /// The v0.1.0 bug: a first launch with no config left the daemon
    /// crash-looping into "Daemon stopped". Now the app writes the default
    /// config before the spawn, and the daemon comes up and stays up.
    #[test]
    fn first_launch_without_a_config_creates_one_and_the_daemon_stays_up() {
        let dir = sandbox("fresh");
        in_sandbox(&dir.clone(), move || {
            let config = prx::paths::config_path();
            assert!(config.starts_with(&dir) && !config.exists());
            let fake = config_checking_prx(&dir);
            let (tx, rx) = mpsc::channel();
            let supervisor = Supervisor::spawn(Some(fake), move |s| {
                let _ = tx.send(s);
            });
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(20)).ok(),
                Some(DaemonState::Starting)
            );
            let Some(DaemonState::Running { pid }) = rx.recv_timeout(Duration::from_secs(20)).ok()
            else {
                panic!("the daemon should be running");
            };
            // No crash follows: the fake exits at once without a config.
            assert_eq!(
                rx.recv_timeout(Duration::from_millis(1500)).ok(),
                None,
                "the daemon must stay up on the config the app created"
            );
            assert!(process_alive(pid));

            // What was written is the `prx init` config, and it is valid.
            assert_eq!(
                std::fs::read_to_string(&config).unwrap(),
                prx::config::STARTER_CONFIG
            );
            let loaded = prx::config::Config::load(&config).unwrap();
            loaded.validate().unwrap();
            assert!(loaded.repos.is_empty(), "first run watches nothing yet");

            supervisor.shutdown(true);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn an_existing_config_is_left_untouched_by_a_launch() {
        let dir = sandbox("existing");
        in_sandbox(&dir.clone(), move || {
            let config = prx::paths::config_path();
            let mine = "# mine\nrepos:\n  - name: org/mine\n";
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(&config, mine).unwrap();
            let (tx, rx) = mpsc::channel();
            let supervisor = Supervisor::spawn(Some(config_checking_prx(&dir)), move |s| {
                let _ = tx.send(s);
            });
            loop {
                let state = rx.recv_timeout(Duration::from_secs(20)).expect("state");
                if matches!(state, DaemonState::Running { .. }) {
                    break;
                }
            }
            assert_eq!(std::fs::read_to_string(&config).unwrap(), mine);
            supervisor.shutdown(true);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// An invalid config is the user's to fix: the app must not replace it
    /// with the default, and the failed start must stay visible.
    #[test]
    fn an_invalid_existing_config_is_not_replaced_and_the_failure_shows() {
        let dir = sandbox("invalid");
        in_sandbox(&dir.clone(), move || {
            let config = prx::paths::config_path();
            let broken = "defaults: [not, a, map\n";
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(&config, broken).unwrap();
            assert!(prx::config::Config::load(&config).is_err());
            let (tx, rx) = mpsc::channel();
            let supervisor = Supervisor::spawn(Some(config_checking_prx(&dir)), move |s| {
                let _ = tx.send(s);
            });
            loop {
                let state = rx.recv_timeout(Duration::from_secs(20)).expect("state");
                if matches!(state, DaemonState::Restarting { .. }) {
                    break;
                }
            }
            // Through the restart that follows, too.
            loop {
                let state = rx.recv_timeout(Duration::from_secs(20)).expect("state");
                if matches!(state, DaemonState::Restarting { failures: 2, .. }) {
                    break;
                }
            }
            assert_eq!(std::fs::read_to_string(&config).unwrap(), broken);
            supervisor.shutdown(true);
            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    fn supervise_in(dir: &Path) {
        let fake = dir.join("prx");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\n{FAKE_READY}trap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (tx, rx) = mpsc::channel();
        let wait_for = |want: &dyn Fn(&DaemonState) -> bool| -> DaemonState {
            loop {
                let state = rx
                    .recv_timeout(Duration::from_secs(20))
                    .expect("state change");
                if want(&state) {
                    return state;
                }
            }
        };
        let tx1 = tx.clone();
        let supervisor = Supervisor::spawn(Some(fake.clone()), move |s| {
            let _ = tx1.send(s);
        });
        let DaemonState::Running { pid: first } =
            wait_for(&|s| matches!(s, DaemonState::Running { .. }))
        else {
            unreachable!()
        };
        assert!(process_alive(first));

        // A crash (kill -9) is restarted after the 1 s backoff.
        signal(first, libc::SIGKILL);
        assert_eq!(
            wait_for(&|s| matches!(s, DaemonState::Restarting { .. })),
            DaemonState::Restarting {
                delay_secs: 1,
                failures: 1
            }
        );
        let DaemonState::Running { pid: second } =
            wait_for(&|s| matches!(s, DaemonState::Running { .. }))
        else {
            unreachable!()
        };
        assert_ne!(first, second);

        // "Quit PRX": SIGTERM, graceful exit awaited.
        supervisor.shutdown(true);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!process_alive(second), "owned daemon stopped on quit");

        // "Quit PRX (keep reviewing)": the owned daemon survives the quit.
        let tx2 = tx.clone();
        let keep = Supervisor::spawn(Some(fake.clone()), move |s| {
            let _ = tx2.send(s);
        });
        let DaemonState::Running { pid: kept } =
            wait_for(&|s| matches!(s, DaemonState::Running { .. }))
        else {
            unreachable!()
        };
        keep.shutdown(false);
        std::thread::sleep(Duration::from_millis(200));
        assert!(
            process_alive(kept),
            "keep reviewing leaves the daemon running"
        );

        // A live daemon (the kept one, as if CLI-started) owns the pid file:
        // a new app session attaches and spawns nothing.
        std::fs::write(dir.join("daemon.pid"), format!("{kept}\n")).unwrap();
        let tx3 = tx.clone();
        let attach = Supervisor::spawn(Some(fake.clone()), move |s| {
            let _ = tx3.send(s);
        });
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).ok(),
            None,
            "attached: no state change, nothing spawned"
        );
        attach.shutdown(true);
        assert!(process_alive(kept), "an external daemon is never stopped");

        signal(kept, libc::SIGKILL);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reset_restores_the_budget() {
        let mut policy = RestartPolicy::default();
        let t0 = Instant::now();
        for i in 0..5 {
            policy.on_failure(t0 + Duration::from_secs(i));
        }
        policy.reset();
        assert_eq!(policy.failures(), 0);
        assert_eq!(
            policy.on_failure(t0 + Duration::from_secs(6)),
            Decision::RestartAfter(BASE_DELAY)
        );
    }
}
