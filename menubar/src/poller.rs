//! Background `/status` poller (spec: "Daemon discovery and offline state").
//! A plain OS thread drives a current-thread tokio runtime; every tick it
//! re-runs `DaemonClient::discover()` — which re-reads `daemon.port` /
//! `daemon.token` from the data dir — and then `GET /status` (plus the most
//! recent `GET /history` entries for the History submenu, riding the same
//! tick so no separate timer exists). Because discovery happens per tick, a
//! daemon restart with a fresh token heals automatically on the next poll,
//! and a dead daemon flips the shell to its offline model. Outcomes are
//! handed to a caller callback (the GUI forwards them onto the main thread);
//! refresh nudges come back through a command channel.
//!
//! Config changes (spec "PRX.app refreshes after a config change"): a save
//! from the window ([`PollerHandle::config_changed`]), a change of the
//! config file's content (checked every [`FILE_CHECK`]; the daemon is asked
//! to reload it at once), or a new `config_revision` in a poll each poll now
//! and then every [`FAST_INTERVAL`] for [`FAST_POLLS`] polls ([`Cadence`]),
//! so the menu shows the change, and a newly watched repo's first poll,
//! within seconds.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use prx::history::HistoryEntry;
use prx::ipc::{DaemonClient, StatusResponse};

use crate::banners;

/// Periodic poll cadence (~10 s; `/status` is a cheap cached localhost read).
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// The cadence right after a config change …
pub const FAST_INTERVAL: Duration = Duration::from_secs(2);

/// … for this many polls after the immediate one.
pub const FAST_POLLS: u32 = 5;

/// How often the poller hashes the config file between polls (a small
/// local read, like the daemon's own watcher).
pub const FILE_CHECK: Duration = Duration::from_secs(1);

/// History entries fetched per tick for the footer's History submenu
/// (`GET /history` is a small local-file read; the menu shows the newest).
pub const HISTORY_LIMIT: usize = 10;

/// One successful poll: the status payload plus the recent history entries.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub status: StatusResponse,
    pub history: Vec<HistoryEntry>,
    /// `defaults.notifications.menubar_bell` from the config file at this
    /// poll (the bell is the app's own feature; the daemon does not report it).
    pub menubar_bell: bool,
    /// The daemon's `config_revision` differs from the previous poll's: the
    /// config changed (by any route). The GUI tells an open window.
    pub config_changed: bool,
}

/// One poll outcome, ready for the GUI thread.
#[derive(Debug, Clone)]
pub enum PollOutcome {
    /// Fresh `GET /status` + `GET /history` payloads.
    Up(Box<Snapshot>),
    /// The daemon could not be discovered or did not answer.
    Down(String),
}

/// Commands the GUI thread can send to nudge the poller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollerCommand {
    /// Poll immediately (menu opened / user clicked Refresh / an action
    /// completed).
    Refresh,
    /// The config was just changed through the daemon (a save from the
    /// window): poll now and keep the fast cadence for a few polls.
    ConfigChanged,
}

/// A handle for the GUI thread to nudge the poller.
#[derive(Clone)]
pub struct PollerHandle {
    tx: Sender<PollerCommand>,
}

impl PollerHandle {
    pub fn new(tx: Sender<PollerCommand>) -> PollerHandle {
        PollerHandle { tx }
    }

    /// Ask for an immediate poll (best effort; a closed poller is ignored).
    pub fn refresh(&self) {
        let _ = self.tx.send(PollerCommand::Refresh);
    }

    /// The config changed: poll now, then fast for a few polls.
    pub fn config_changed(&self) {
        let _ = self.tx.send(PollerCommand::ConfigChanged);
    }
}

/// The poll cadence: [`POLL_INTERVAL`], or [`FAST_INTERVAL`] for the
/// [`FAST_POLLS`] polls after a config change.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cadence {
    fast_left: u32,
}

impl Cadence {
    /// A config change: the polls that follow come fast.
    pub fn config_changed(&mut self) {
        self.fast_left = FAST_POLLS;
    }

    /// A poll just ran: how long until the next one.
    pub fn after_poll(&mut self) -> Duration {
        if self.fast_left > 0 {
            self.fast_left -= 1;
            FAST_INTERVAL
        } else {
            POLL_INTERVAL
        }
    }
}

/// Notices a change of the daemon's `config_revision` between polls. The
/// first revision seen, and a daemon too old to report one, are not
/// changes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RevisionWatch {
    last: Option<u64>,
}

impl RevisionWatch {
    /// Record `revision` from a poll; `true` when it differs from the last
    /// one recorded (a daemon restart counts: its config may differ).
    pub fn observe(&mut self, revision: Option<u64>) -> bool {
        let changed = matches!((self.last, revision), (Some(last), Some(now)) if last != now);
        if revision.is_some() {
            self.last = revision;
        }
        changed
    }
}

/// Notices a change of the config file's content (hash; a missing file is
/// a state of its own, so creating or deleting it counts).
#[derive(Debug, Clone)]
pub struct FileWatch {
    path: PathBuf,
    seen: Option<u64>,
}

impl FileWatch {
    pub fn at(path: PathBuf) -> FileWatch {
        let seen = banners::content_fingerprint(&path);
        FileWatch { path, seen }
    }

    /// `true` once per change since the last call.
    pub fn changed(&mut self) -> bool {
        let now = banners::content_fingerprint(&self.path);
        if now == self.seen {
            return false;
        }
        self.seen = now;
        true
    }
}

/// Spawn the poller thread with a plain outcome handler: every
/// [`PollOutcome`] is handed to `on_outcome` (the GUI forwards it onto the
/// main thread; self-test mode prints it). The thread ends when the command
/// channel disconnects (the GUI side was dropped) or the process exits.
pub fn spawn_with_handler<F>(commands: Receiver<PollerCommand>, on_outcome: F)
where
    F: FnMut(PollOutcome) + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name("prx-menubar-poll".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("prx-menubar: cannot start poller runtime: {error}");
                    return;
                }
            };
            runtime.block_on(poll_loop(&commands, on_outcome));
        })
        .expect("failed to spawn the poller thread");
    // The thread terminates on its own (loop closure / channel disconnect);
    // nothing joins it.
    drop(handle);
}

/// Why the loop woke up, decided from one wait on the command channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wake {
    /// Poll now (a nudge or the interval elapsed).
    Poll,
    /// A config change: poll now and go fast.
    ConfigChanged,
    /// Nothing due yet: only look at the config file.
    Idle,
    /// The GUI side is gone.
    Stop,
}

fn wake_from(received: Result<PollerCommand, RecvTimeoutError>, due: bool) -> Wake {
    match received {
        Ok(PollerCommand::Refresh) => Wake::Poll,
        Ok(PollerCommand::ConfigChanged) => Wake::ConfigChanged,
        Err(RecvTimeoutError::Timeout) if due => Wake::Poll,
        Err(RecvTimeoutError::Timeout) => Wake::Idle,
        Err(RecvTimeoutError::Disconnected) => Wake::Stop,
    }
}

async fn poll_loop<F>(commands: &Receiver<PollerCommand>, mut on_outcome: F)
where
    F: FnMut(PollOutcome),
{
    // Task 4.6: the notification config and the seen-events watermark live
    // on this poller thread; banners are decided AND delivered here — never
    // on the GUI thread (see the banners module guard).
    // Re-read when the config file changes (master switch / tier edits
    // apply on the next poll, task 8.4).
    let mut notification_config = banners::ConfigSource::new();
    let mut banner_tracker = banners::BannerTracker::new();
    let mut cadence = Cadence::default();
    let mut revisions = RevisionWatch::default();
    let mut config_file = FileWatch::at(prx::paths::config_path());
    // The first poll runs at once: a daemon that already runs shows in the
    // menu on launch, not one interval later.
    let mut next_poll = Instant::now();
    loop {
        // A nudge or the interval elapse triggers a poll; between polls the
        // loop wakes every FILE_CHECK to look at the config file. The
        // channel closing (event loop gone) ends the thread.
        let wait = next_poll
            .saturating_duration_since(Instant::now())
            .min(FILE_CHECK);
        let wake = wake_from(
            commands.recv_timeout(wait),
            Instant::now() + Duration::from_millis(5) >= next_poll,
        );
        let mut poll = match wake {
            Wake::Stop => break,
            Wake::Poll => true,
            Wake::ConfigChanged => {
                cadence.config_changed();
                true
            }
            Wake::Idle => false,
        };
        // An edit of the file by any route (CLI, editor, the window): ask
        // the daemon to apply it now rather than on its own next look.
        let file_changed = config_file.changed();
        if file_changed {
            eprintln!("prx-menubar: config file changed — asking the daemon to reload it");
            cadence.config_changed();
            poll = true;
        }
        if !poll {
            continue;
        }

        let outcome = poll_once(
            file_changed,
            &mut notification_config,
            &mut banner_tracker,
            &mut revisions,
        )
        .await;
        if let PollOutcome::Up(snapshot) = &outcome {
            if snapshot.config_changed {
                eprintln!(
                    "prx-menubar: daemon config revision now {:?}",
                    snapshot.status.config_state.config_revision
                );
                cadence.config_changed();
            }
        }
        next_poll = Instant::now() + cadence.after_poll();
        if let PollOutcome::Down(error) = &outcome {
            eprintln!("prx-menubar: poll failed: {error}");
        }
        on_outcome(outcome);
    }
}

/// One poll: discover, optionally nudge a reload, `GET /status` and
/// `GET /history`, and raise new banners.
async fn poll_once(
    nudge_reload: bool,
    notification_config: &mut banners::ConfigSource,
    banner_tracker: &mut banners::BannerTracker,
    revisions: &mut RevisionWatch,
) -> PollOutcome {
    // Re-discover on EVERY poll: fresh port/token files are re-read each
    // time, so a restarted daemon (rotated bearer token) needs no user
    // intervention, and a dead daemon yields None (offline model).
    let Some(client) = DaemonClient::discover_as_menubar().await else {
        return PollOutcome::Down(
            "daemon not reachable (discovery files missing or no answer)".to_string(),
        );
    };
    if nudge_reload {
        // Best effort: an older daemon has no route, and its watcher picks
        // the file up within a second or two anyway.
        match client.reload_config().await {
            Ok(reload) => eprintln!(
                "prx-menubar: daemon config reload: {:?} (revision {:?})",
                reload.outcome, reload.config_state.config_revision
            ),
            Err(error) => eprintln!("prx-menubar: POST /config/reload failed: {error:#}"),
        }
    }
    let status = match client.get_status().await {
        Ok(status) => status,
        Err(error) => return PollOutcome::Down(format!("{error:#}")),
    };
    eprintln!(
        "prx-menubar: GET /status ok ({} PR{}, pid {})",
        status.prs.len(),
        if status.prs.len() == 1 { "" } else { "s" },
        status.pid
    );
    // History rides the same tick (task 4.5): a failure only empties the
    // submenu, never the status.
    let history = match client.history(None, HISTORY_LIMIT).await {
        // A newer daemon's event this build cannot name decodes as
        // `Unknown`; it has nothing to show.
        Ok(response) => response
            .entries
            .into_iter()
            .filter(|entry| !matches!(entry, HistoryEntry::Unknown))
            .collect(),
        Err(error) => {
            eprintln!("prx-menubar: GET /history failed: {error:#}");
            Vec::new()
        }
    };
    let config_changed = revisions.observe(status.config_state.config_revision);
    let menubar_bell = notification_config
        .current()
        .defaults
        .notifications
        .menubar_bell;
    let snapshot = Snapshot {
        status,
        history,
        menubar_bell,
        config_changed,
    };
    // Delta-detect NEW history events and raise the banner their configured
    // tier asks for (cold start marks the backlog seen silently; bursts
    // coalesce).
    let planned = banner_tracker.update(&snapshot.history, notification_config.current());
    if !planned.is_empty() {
        // Click-through honors each repo's effective `browser`: fetched
        // only when a banner is raised.
        let config = match client.get_config().await {
            Ok(config) => Some(config),
            Err(error) => {
                eprintln!(
                    "prx-menubar: GET /config failed ({error:#}) — banners open with the system browser"
                );
                None
            }
        };
        banners::deliver(&planned, config.as_ref().map(|c| &c.effective));
    }
    PollOutcome::Up(Box::new(snapshot))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// After a config change the next polls come every 2 s, five times,
    /// then the cadence falls back to 10 s; another change restarts it.
    #[test]
    fn cadence_goes_fast_after_a_config_change() {
        let mut cadence = Cadence::default();
        assert_eq!(cadence.after_poll(), POLL_INTERVAL);
        cadence.config_changed();
        for _ in 0..FAST_POLLS {
            assert_eq!(cadence.after_poll(), FAST_INTERVAL);
        }
        assert_eq!(cadence.after_poll(), POLL_INTERVAL);
        cadence.config_changed();
        assert_eq!(cadence.after_poll(), FAST_INTERVAL);
    }

    /// The first revision is a baseline, a new one is a change, an older
    /// daemon's missing revision changes nothing and keeps the baseline.
    #[test]
    fn revision_watch_reports_changes_after_the_first_poll() {
        let mut watch = RevisionWatch::default();
        assert!(!watch.observe(Some(3)));
        assert!(!watch.observe(Some(3)));
        assert!(watch.observe(Some(4)));
        assert!(!watch.observe(None));
        assert!(!watch.observe(Some(4)));
        // A restarted daemon starts over at 1: still a change.
        assert!(watch.observe(Some(1)));

        let mut old_daemon = RevisionWatch::default();
        assert!(!old_daemon.observe(None));
        assert!(!old_daemon.observe(None));
    }

    /// The file watch fires once per content change, including a same-size
    /// rewrite, creation, and removal; a rewrite with equal content does not.
    #[test]
    fn file_watch_fires_once_per_content_change() {
        let dir =
            std::env::temp_dir().join(format!("prx-menubar-file-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yml");
        let _ = std::fs::remove_file(&path);
        let mut watch = FileWatch::at(path.clone());
        assert!(!watch.changed());
        std::fs::write(&path, "a: 1\n").unwrap();
        assert!(watch.changed());
        assert!(!watch.changed());
        std::fs::write(&path, "a: 1\n").unwrap();
        assert!(!watch.changed(), "same content is no change");
        std::fs::write(&path, "a: 2\n").unwrap();
        assert!(watch.changed());
        std::fs::remove_file(&path).unwrap();
        assert!(watch.changed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Commands map to wakes: a save goes fast, a nudge polls, a timeout
    /// polls only once the interval is due, a closed channel stops.
    #[test]
    fn wakes_follow_commands_and_the_interval() {
        assert_eq!(wake_from(Ok(PollerCommand::Refresh), false), Wake::Poll);
        assert_eq!(
            wake_from(Ok(PollerCommand::ConfigChanged), false),
            Wake::ConfigChanged
        );
        assert_eq!(wake_from(Err(RecvTimeoutError::Timeout), false), Wake::Idle);
        assert_eq!(wake_from(Err(RecvTimeoutError::Timeout), true), Wake::Poll);
        assert_eq!(
            wake_from(Err(RecvTimeoutError::Disconnected), true),
            Wake::Stop
        );
    }

    /// End to end on the channel: a `config_changed` from the GUI side
    /// arrives as the fast-cadence command.
    #[test]
    fn handle_sends_config_changed() {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = PollerHandle::new(tx);
        handle.config_changed();
        handle.refresh();
        assert_eq!(rx.recv().unwrap(), PollerCommand::ConfigChanged);
        assert_eq!(rx.recv().unwrap(), PollerCommand::Refresh);
    }
}
