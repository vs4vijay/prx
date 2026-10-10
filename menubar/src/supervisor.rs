//! Daemon supervision in the app (design A3, the Ollama model).
//!
//! On start the app attaches to a daemon that already answers discovery
//! (started by the CLI, launchd, or another session) and never spawns a
//! second one. Otherwise it spawns the bundled `prx daemon start
//! --foreground` (PRX.app's `Contents/Helpers/prx`) with the login-shell
//! `PATH` ([`crate::shellenv`]) and records the owned pid.
//!
//! - Before a spawn the app creates the default config when none exists
//!   ([`ensure_first_run_config`]), so a first launch needs no `prx init`.
//! - An owned daemon exiting non-zero (a crash, `kill -9`) is restarted with
//!   exponential backoff (1 s, 2 s, 4 s, … capped at 60 s). After
//!   [`MAX_FAILURES`] failures inside [`FAILURE_WINDOW`] the supervisor gives
//!   up and reports the daemon stopped (menu: "Start daemon", "Show log").
//! - An owned daemon exiting 0 is a deliberate stop (`prx daemon stop`): no
//!   restart.
//! - A non-zero exit while ANOTHER daemon is alive (the pid-file guard's
//!   loser, A16) is not a crash: the app attaches to the winner.
//! - Quit SIGTERMs only the owned daemon and waits for its graceful exit;
//!   "Quit PRX (keep reviewing)" and an external daemon are left running.
//!
//! The restart policy is pure ([`RestartPolicy`]) so its backoff and cap are
//! unit-tested; the thread around it only spawns, waits, and reports.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// What the menu shows about the daemon's supervision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// Not owned by the app: attached to an external daemon, or none seen
    /// yet.
    Unmanaged,
    /// The app spawned its daemon and is waiting for it to answer.
    Starting,
    /// The app's own daemon is running.
    Running { pid: u32 },
    /// The owned daemon crashed; a restart is scheduled.
    Restarting { delay_secs: u64, failures: usize },
    /// The owned daemon is down and will not be restarted automatically:
    /// a deliberate stop (`gave_up: false`) or the failure cap.
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

enum Cmd {
    /// Manual "Start daemon".
    Start,
    /// The owned child exited.
    Exited { pid: u32, success: bool },
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
        let (tx, rx) = mpsc::channel();
        let owned = Arc::new(Mutex::new(None));
        let worker = Worker {
            prx,
            tx: tx.clone(),
            owned,
            policy: RestartPolicy::default(),
            state: DaemonState::Unmanaged,
            on_state: Box::new(on_state),
            restart_at: None,
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
    state: DaemonState,
    on_state: Box<dyn Fn(DaemonState) + Send>,
    restart_at: Option<Instant>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Cmd>) {
        self.attach_or_spawn();
        loop {
            let timeout = self
                .restart_at
                .map(|at| at.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(3600));
            match rx.recv_timeout(timeout) {
                Ok(Cmd::Start) => {
                    self.policy.reset();
                    self.restart_at = None;
                    if self.owned_pid().is_none() {
                        self.attach_or_spawn();
                    }
                }
                Ok(Cmd::Exited { pid, success }) => self.exited(pid, success),
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
                    if self.restart_at.is_some_and(|at| Instant::now() >= at) {
                        self.restart_at = None;
                        self.attach_or_spawn();
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

    /// A3 attach-first: a daemon that answers (or a live pid-file owner)
    /// is used as is; otherwise spawn the bundled one.
    fn attach_or_spawn(&mut self) {
        if other_daemon_alive(None) {
            eprintln!("prx-menubar: a daemon is already running; attaching (not spawning)");
            self.set_state(DaemonState::Unmanaged);
            return;
        }
        let Some(prx) = self.prx.clone() else {
            eprintln!("prx-menubar: no bundled prx binary found; cannot start a daemon");
            self.set_state(DaemonState::Stopped { gave_up: false });
            return;
        };
        self.set_state(DaemonState::Starting);
        ensure_first_run_config();
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
                        let success = child.wait().map(|s| s.success()).unwrap_or(false);
                        let _ = tx.send(Cmd::Exited { pid, success });
                    })
                    .expect("failed to spawn the daemon waiter thread");
                self.set_state(DaemonState::Running { pid });
            }
            Err(error) => {
                eprintln!("prx-menubar: cannot spawn the daemon: {error}");
                self.failed();
            }
        }
    }

    fn exited(&mut self, pid: u32, success: bool) {
        if self.owned_pid() != Some(pid) {
            return;
        }
        self.set_owned(None);
        if success {
            eprintln!("prx-menubar: daemon pid {pid} exited cleanly; not restarting");
            // The pid-file guard's loser also exits cleanly: attach then.
            if other_daemon_alive(Some(pid)) {
                self.set_state(DaemonState::Unmanaged);
            } else {
                self.set_state(DaemonState::Stopped { gave_up: false });
            }
            return;
        }
        if other_daemon_alive(Some(pid)) {
            eprintln!(
                "prx-menubar: daemon pid {pid} exited; another daemon is running — attaching"
            );
            self.set_state(DaemonState::Unmanaged);
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
                Ok(Cmd::Exited { pid: exited, .. }) if exited == pid => {
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

/// A live daemon other than `except`: the pid file names a live process,
/// or discovery answers.
fn other_daemon_alive(except: Option<u32>) -> bool {
    if let Some(pid) = prx::daemon::read_pid(&prx::paths::daemon_pid_file()) {
        if Some(pid) != except && process_alive(pid) {
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
        .block_on(prx::ipc::DaemonClient::discover())
        .is_some()
}

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
             trap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n",
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
            "#!/bin/sh\ntrap 'exit 0' TERM\nwhile :; do sleep 0.05; done\n",
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
