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

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use prx::history::HistoryEntry;
use prx::ipc::{DaemonClient, StatusResponse};

use crate::banners;

/// Periodic poll cadence (~10 s; `/status` is a cheap cached localhost read).
pub const POLL_INTERVAL: Duration = Duration::from_secs(10);

/// History entries fetched per tick for the footer's History submenu
/// (`GET /history` is a small local-file read; the menu shows the newest).
pub const HISTORY_LIMIT: usize = 10;

/// One successful poll: the status payload plus the recent history entries.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub status: StatusResponse,
    pub history: Vec<HistoryEntry>,
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
#[derive(Debug, Clone, Copy)]
pub enum PollerCommand {
    /// Poll immediately (menu opened / user clicked Refresh / an action
    /// completed).
    Refresh,
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
    loop {
        // A refresh nudge or the interval elapse triggers a poll; the channel
        // closing (event loop gone) ends the thread.
        if !matches!(
            commands.recv_timeout(POLL_INTERVAL),
            Ok(PollerCommand::Refresh) | Err(RecvTimeoutError::Timeout)
        ) {
            break;
        }

        // Re-discover on EVERY poll: fresh port/token files are re-read each
        // time, so a restarted daemon (rotated bearer token) needs no user
        // intervention, and a dead daemon yields None (offline model).
        let outcome = match DaemonClient::discover().await {
            Some(client) => match client.get_status().await {
                Ok(status) => {
                    eprintln!(
                        "prx-menubar: GET /status ok ({} PR{}, pid {})",
                        status.prs.len(),
                        if status.prs.len() == 1 { "" } else { "s" },
                        status.pid
                    );
                    // History rides the same tick (task 4.5): a failure only
                    // empties the submenu, never the status.
                    let history = match client.history(None, HISTORY_LIMIT).await {
                        Ok(response) => response.entries,
                        Err(error) => {
                            eprintln!("prx-menubar: GET /history failed: {error:#}");
                            Vec::new()
                        }
                    };
                    let snapshot = Snapshot { status, history };
                    // Delta-detect NEW history events and raise the banner
                    // their configured tier asks for (cold start marks the
                    // backlog seen silently; bursts coalesce).
                    let planned =
                        banner_tracker.update(&snapshot.history, notification_config.current());
                    if !planned.is_empty() {
                        // Click-through honors each repo's effective
                        // `browser`: fetched only when a banner is raised.
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
                Err(error) => PollOutcome::Down(format!("{error:#}")),
            },
            None => PollOutcome::Down(
                "daemon not reachable (discovery files missing or no answer)".to_string(),
            ),
        };
        if let PollOutcome::Down(error) = &outcome {
            eprintln!("prx-menubar: poll failed: {error}");
        }
        on_outcome(outcome);
    }
}
