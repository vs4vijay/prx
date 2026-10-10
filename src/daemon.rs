//! The daemon core (DEV-PLAN §9): single-instance guard (A16), detached
//! lifecycle commands (start/stop/status/logs), the scheduler loop with
//! per-repo poll intervals + ±10% jitter (A7), and the global FIFO work
//! queue (D6). All state and GitHub writes flow through here (A4).
//!
//! Concurrency model: `Shared` owns the state-file truth behind one tokio
//! mutex. A review job holds that mutex for the whole pipeline run —
//! serialized, race-free, and the reason a daemon restart cannot duplicate a
//! review (`last_reviewed_sha` plus the PollUnchanged path keep reviewed PRs
//! asleep). `max_concurrent_reviews` (D6) caps concurrently *running* jobs;
//! the state mutex may further serialize their state-touching sections, so
//! the config value is an upper bound, never exceeded.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rand::RngExt;
use thiserror::Error;
use tokio::sync::{watch, Notify, Semaphore};

use crate::agent::EngineAdapter;
use crate::browser::{self, Opener, SystemOpener};
use crate::config::{self, Config, ConfigError, EffectiveRepo, ReviewMode};
use crate::effects::EffectsDeps;
use crate::github::GhCli;
use crate::history;
use crate::ipc::ConfigErrorInfo;
use crate::machine;
use crate::notify::{CommandNotifier, Notifier, NotifyError, TierBehavior, TierTable};
use crate::paths;
use crate::pause::{self, EnqueueOrigin};
use crate::review;
use crate::review::ReviewOverrides;
use crate::state::{StateFile, StateStore, Verdict};
use crate::status;

/// History retention window in days, compacted on daemon start (A17).
const HISTORY_RETENTION_DAYS: u64 = 90;

/// D3 (banner dedup): how long a menubar announcement keeps the daemon's own
/// desktop banners suppressed. The menubar polls every 10 s, so 90 s ≈ 9
/// missed polls — a generous margin over normal polling gaps, while a quit
/// menubar is noticed within about a minute and a half.
const MENUBAR_LIVENESS_WINDOW: Duration = Duration::from_secs(90);

// ------------------------------------------------- banner dedup (D3)

/// The client-liveness cell (D3): when an authenticated IPC request last
/// carried `X-Prx-Client`. `Shared` records into it (auth middleware); the
/// daemon's suppressing notifier reads it before delegating a banner — hence
/// the `Arc`, shared by both owners.
#[derive(Debug, Default)]
struct MenubarLiveness(std::sync::Mutex<Option<Instant>>);

impl MenubarLiveness {
    fn note_seen(&self) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Instant::now());
    }

    fn recently_seen(&self, window: Duration) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|seen| seen.elapsed() <= window)
    }
}

/// Defers the daemon's own desktop banners to a live menubar (D3): the
/// menubar raises the same banner from its `/history` delta, so while a
/// menubar client announced itself within [`MENUBAR_LIVENESS_WINDOW`], the
/// notification is skipped with a debug log. Adds suppression only — the
/// tier table's `banner: false` short-circuit stays authoritative inside the
/// inner notifier, and an absent or stale client delegates unchanged.
pub struct MenubarDeferringNotifier<N> {
    inner: N,
    liveness: Arc<MenubarLiveness>,
}

impl<N> MenubarDeferringNotifier<N> {
    /// Wrap `inner` with a fresh liveness cell. [`Shared::new`] re-reads
    /// that cell, so client announcements recorded through `Shared` are the
    /// same ones this wrapper checks.
    pub fn wrapping(inner: N) -> Self {
        Self {
            inner,
            liveness: Arc::new(MenubarLiveness::default()),
        }
    }
}

impl<N: Notifier> Notifier for MenubarDeferringNotifier<N> {
    async fn notify(
        &self,
        title: &str,
        body: &str,
        behavior: &TierBehavior,
    ) -> Result<(), NotifyError> {
        if self.liveness.recently_seen(MENUBAR_LIVENESS_WINDOW) {
            tracing::debug!("menubar is live; suppressing daemon banner \"{title}: {body}\"");
            return Ok(());
        }
        self.inner.notify(title, body, behavior).await
    }
}

// ------------------------------------------------------------- dependencies

/// Everything the daemon needs to poll, review, and notify — except the
/// config-derived settings, which live behind [`Shared::live`] so the
/// reload path (D1) can swap them. Concrete types: the fixture binaries
/// (`GhCli::with_binary`, the configured agent command) are the test seams —
/// the `Gh`/`AgentAdapter`/`Notifier` traits stay async-fn-in-trait and are
/// not dyn-compatible.
pub struct Deps {
    pub gh: GhCli,
    pub adapter: EngineAdapter,
    /// D3: the real notifier wrapped in menubar-liveness suppression. The
    /// in-process `prx status` path builds its own bare `CommandNotifier`
    /// and never sees this wrapper.
    pub notifier: MenubarDeferringNotifier<CommandNotifier>,
    /// The config file the live-reload task watches (D1). `None` disables
    /// reloading: the startup config stays live for the daemon's lifetime.
    pub config_path: Option<PathBuf>,
    pub store: StateStore,
    pub history_path: PathBuf,
    pub data_dir: PathBuf,
    /// Spawns the browser for verdict-triggered auto-open (design D9).
    /// Production: [`SystemOpener`]; tests inject a spy.
    pub opener: Arc<dyn Opener + Send + Sync>,
}

impl Deps {
    /// Build the production dependency set, watching `config_path`.
    pub fn production(config_path: PathBuf, data_dir: PathBuf) -> Deps {
        Deps {
            gh: GhCli::new(),
            adapter: EngineAdapter::default(),
            notifier: MenubarDeferringNotifier::wrapping(CommandNotifier),
            config_path: Some(config_path),
            store: StateStore::new(paths::state_file()),
            history_path: paths::history_file(),
            data_dir,
            opener: Arc::new(SystemOpener),
        }
    }
}

// ------------------------------------------------------------- live config

/// The config-derived settings the daemon runs on. Immutable once built:
/// a reload (D1) builds a fresh one and swaps the `Arc`, so readers holding
/// the previous snapshot finish their work on a consistent view.
#[derive(Debug, Clone)]
pub struct LiveConfig {
    /// The validated config as parsed (raw view for later config routes).
    pub config: Config,
    /// The watched-repo set, fully merged (defaults ← org ← repo).
    pub repos: Vec<EffectiveRepo>,
    pub tiers: TierTable,
}

impl LiveConfig {
    /// Validate `config` and derive the live view. Errors leave nothing
    /// half-applied: callers only swap on `Ok`.
    pub fn from_config(config: Config) -> Result<LiveConfig, ConfigError> {
        config.validate()?;
        let repos = config.effective_repos()?;
        Ok(LiveConfig {
            tiers: config.notification_tiers.clone(),
            repos,
            config,
        })
    }

    pub fn repo(&self, name: &str) -> Option<&EffectiveRepo> {
        self.repos.iter().find(|r| r.name == name)
    }
}

/// What one applied reload changed in the watched-repo set (D1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReloadSummary {
    /// Repos that entered the poll rotation.
    pub added: Vec<String>,
    /// Repos dropped from scheduling (state records retained, unlisted).
    pub removed: Vec<String>,
    /// Queued (not yet running) review keys discarded with removed repos.
    pub discarded: Vec<String>,
}

impl ReloadSummary {
    pub fn is_noop(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.discarded.is_empty()
    }
}

/// What the reload watcher last saw on disk: content hash, or unreadable.
/// Remembered even when the content was rejected, so a broken file is
/// reported once rather than re-parsed (and re-logged) every second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigFingerprint {
    Content(u64),
    Unreadable,
}

/// Change detection only (not security): exact for any edit, independent of
/// mtime granularity (D1).
fn content_hash(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

/// Why a config write over IPC (D5) did not happen.
#[derive(Debug, Error)]
pub enum ConfigWriteError {
    /// The daemon runs without a config file (tests / embedded use).
    #[error("this daemon has no config file to write")]
    NoConfigFile,
    /// Parse or validation failure: nothing was written or applied.
    #[error("{0}")]
    Invalid(#[source] ConfigError),
    /// Validated, but persisting failed: nothing was applied.
    #[error("{0}")]
    Persist(#[source] ConfigError),
}

// ------------------------------------------------------------ shared state

/// One queued review. `due` is the FIFO key: the queue pops
/// oldest-due-first (D6); ties keep insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub key: String,
    pub repo: String,
    pub due: DateTime<Utc>,
    /// A22: forced jobs bypass the cooldown re-check at execution time.
    pub force: bool,
    /// D1/D10: this run's per-run options (engine, post, extra prompt).
    /// They live in the in-memory queue alone — never in state, history, or
    /// config — so they are ephemeral by construction.
    pub overrides: ReviewOverrides,
}

#[derive(Debug, Default)]
struct Runtime {
    queued: Vec<Job>,
    in_flight: HashSet<String>,
    /// Last successful poll per repo (reported by `GET /status`).
    last_poll: BTreeMap<String, DateTime<Utc>>,
    /// Next scheduled poll per repo (interval ± jitter, A7).
    next_poll: BTreeMap<String, DateTime<Utc>>,
    /// D11: next review-enqueue tick per repo with an effective `schedule`
    /// (repos on the interval cadence have no entry).
    next_review_tick: BTreeMap<String, DateTime<Utc>>,
}

/// Tunables for the scheduler loop. `Default` is the production shape; tests
/// shrink the tick and scale the configured poll intervals down.
#[derive(Debug, Clone, Copy)]
pub struct SchedulerOptions {
    /// Wakeup cadence. The daemon never busy-loops: tokio::select over this
    /// tick, poll requests, and shutdown.
    pub tick: Duration,
    /// A7: apply ±10% jitter to per-repo poll intervals.
    pub jitter: bool,
    /// Poll every repo once immediately on startup (A8: PRs accumulated
    /// while the daemon was down are handled by creation-time cooldowns).
    pub startup_poll: bool,
    /// Test hook: multiplies configured poll intervals (kept ≥ 1s).
    pub interval_scale: f64,
    /// D1: how often the reload task hashes the config file.
    pub config_check: Duration,
    /// The scheduler's clock (poll timestamps, cooldown due-checks, and
    /// D11 schedule ticks). Test hook: shift it to reach a cron tick
    /// without waiting for wall time.
    pub clock: fn() -> DateTime<Utc>,
}

impl Default for SchedulerOptions {
    fn default() -> Self {
        SchedulerOptions {
            tick: Duration::from_secs(20),
            jitter: true,
            startup_poll: true,
            interval_scale: 1.0,
            config_check: Duration::from_secs(1),
            clock: Utc::now,
        }
    }
}

/// The shared daemon state: the state-file truth (A4: every state write
/// flows through this one owner), the work queue, and a read cache so
/// `GET /status` stays responsive while a review holds the state lock.
pub struct Shared {
    pub deps: Deps,
    /// Per-start IPC bearer token (A4).
    pub token: String,
    pub pid: u32,
    pub started_at: Instant,
    pub max_concurrent: usize,
    pub options: SchedulerOptions,
    state: tokio::sync::Mutex<StateFile>,
    state_cache: watch::Sender<StateFile>,
    runtime: tokio::sync::Mutex<Runtime>,
    /// Wakes the drainer when a job is enqueued or a running job finished.
    pub work_available: Notify,
    /// Wakes the scheduler for an immediate poll of all repos (/review-now).
    pub poll_requested: Notify,
    /// D3 (banner dedup): shared with the daemon's suppressing notifier —
    /// the auth middleware records client announcements here, the notifier
    /// reads them (see [`MenubarLiveness`]).
    menubar_liveness: Arc<MenubarLiveness>,
    pub semaphore: Arc<Semaphore>,
    /// The live config (D1): swapped whole by [`Shared::apply_config`],
    /// read via [`Shared::live`]. Never held across an await.
    live: std::sync::RwLock<Arc<LiveConfig>>,
    /// D2: why the config file on disk is not the one running, if it isn't.
    config_error: std::sync::Mutex<Option<ConfigErrorInfo>>,
    /// D1: the last config-file fingerprint the watcher processed. The
    /// async lock also serializes concurrent checks (1 s tick vs review-now).
    config_seen: tokio::sync::Mutex<Option<ConfigFingerprint>>,
    /// Wakes the scheduler after a reload so added repos are polled now
    /// rather than on the next tick.
    config_applied: Notify,
    /// D9/D12: gh-authenticated logins resolved by Block/Unblock, keyed by
    /// the identity's credential reference (`None` = gh's own account).
    /// Fills in the status payload's identity login when config sets none.
    gh_logins: std::sync::Mutex<BTreeMap<Option<String>, String>>,
    /// Design B4: PR keys with a brief generation running (on-demand or
    /// right after a review). A second request for the same key is refused.
    briefs_in_flight: std::sync::Mutex<HashSet<String>>,
    /// The last brief failure per PR key (`/status` `brief_error`), cleared
    /// when the next brief run for that PR starts.
    brief_errors: std::sync::Mutex<BTreeMap<String, String>>,
    /// Design B4: on-demand brief runs share these slots — apart from the
    /// review slots — so a burst of Regenerate clicks cannot fan out agents.
    pub brief_semaphore: Arc<Semaphore>,
}

/// Concurrent on-demand brief runs (design B4).
pub const BRIEF_CONCURRENCY: usize = 2;

/// Why a brief request was refused (design B4; `POST /brief`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BriefRequestError {
    #[error("{0} is not tracked")]
    UnknownPr(String),
    #[error("review mode is off for {0}; briefs are not generated")]
    RepoOff(String),
    #[error("briefs are disabled for {0} (brief.enabled: false)")]
    Disabled(String),
    #[error("a brief for {0} is already being written")]
    InProgress(String),
}

impl Shared {
    fn new(deps: Deps, config: Config, token: String, options: SchedulerOptions) -> Result<Shared> {
        let live = LiveConfig::from_config(config).context("configuration is invalid")?;
        let state = deps.store.load().context("failed to load PR state")?;
        // The notifier wrapper and this Shared must observe the same
        // announcements: one liveness cell, two owners.
        let menubar_liveness = deps.notifier.liveness.clone();
        // The concurrency cap is sized once at startup; a reload that
        // changes max_concurrent_reviews applies on the next daemon start.
        let cap = live
            .config
            .defaults
            .max_concurrent_reviews
            .max(
                live.repos
                    .iter()
                    .map(|r| r.max_concurrent_reviews)
                    .max()
                    .unwrap_or(1),
            )
            .max(1) as usize;
        Ok(Shared {
            semaphore: Arc::new(Semaphore::new(cap)),
            max_concurrent: cap,
            deps,
            token,
            pid: std::process::id(),
            started_at: Instant::now(),
            options,
            state_cache: watch::channel(state.clone()).0,
            state: tokio::sync::Mutex::new(state),
            runtime: tokio::sync::Mutex::new(Runtime::default()),
            work_available: Notify::new(),
            poll_requested: Notify::new(),
            menubar_liveness,
            live: std::sync::RwLock::new(Arc::new(live)),
            config_error: std::sync::Mutex::new(None),
            // Unknown until the first check, which re-applies the file once
            // (a no-op reconcile when it still matches the startup config).
            config_seen: tokio::sync::Mutex::new(None),
            config_applied: Notify::new(),
            gh_logins: std::sync::Mutex::new(BTreeMap::new()),
            briefs_in_flight: std::sync::Mutex::new(HashSet::new()),
            brief_errors: std::sync::Mutex::new(BTreeMap::new()),
            brief_semaphore: Arc::new(Semaphore::new(BRIEF_CONCURRENCY)),
        })
    }

    /// Design B4: mark `key`'s brief as running; `false` when one already
    /// is.
    fn try_begin_brief(&self, key: &str) -> bool {
        let begun = self
            .briefs_in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key.to_string());
        if begun {
            self.brief_errors
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(key);
        }
        begun
    }

    /// End `key`'s brief run, remembering its failure (when it stored no
    /// brief) for `/status`.
    fn end_brief(&self, key: &str, error: Option<String>) {
        if let Some(error) = error {
            self.brief_errors
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(key.to_string(), error);
        }
        self.briefs_in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }

    /// The last brief failure for `key`, until its next brief run starts.
    pub fn brief_error(&self, key: &str) -> Option<String> {
        self.brief_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .cloned()
    }

    /// Design B4: whether a brief for `key` is running (`brief_pending`).
    pub fn brief_pending(&self, key: &str) -> bool {
        self.briefs_in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(key)
    }

    /// `POST /brief` (design B2/B4): validate, mark in flight, and generate
    /// the PR's brief in the background. Never writes to GitHub. Returns
    /// once the run is accepted; the result lands in state (and `/status`)
    /// when it finishes, and a failure keeps the previous brief.
    pub fn request_brief(
        self: &Arc<Self>,
        repo_name: &str,
        number: u64,
    ) -> Result<(), BriefRequestError> {
        let live = self.live();
        let key = crate::state::PrRecord::key_for(repo_name, number);
        let Some(repo) = live.repo(repo_name).cloned() else {
            return Err(BriefRequestError::UnknownPr(key));
        };
        if repo.review_mode == ReviewMode::Off {
            return Err(BriefRequestError::RepoOff(repo.name.clone()));
        }
        if !repo.brief.enabled {
            return Err(BriefRequestError::Disabled(repo.name.clone()));
        }
        let Some(rec) = self.snapshot().prs.get(&key).cloned() else {
            return Err(BriefRequestError::UnknownPr(key));
        };
        let key = key.as_str();
        if !self.try_begin_brief(key) {
            return Err(BriefRequestError::InProgress(key.to_string()));
        }
        let shared = self.clone();
        let key = key.to_string();
        tokio::spawn(async move {
            let error = match shared.brief_semaphore.clone().acquire_owned().await {
                Ok(_permit) => {
                    let request = review::BriefRequest::standalone(&rec, Utc::now());
                    run_brief(&shared, &live, &repo, &request).await
                }
                Err(_) => Some("the daemon is shutting down".to_string()),
            };
            shared.end_brief(&key, error);
        });
        Ok(())
    }

    /// The config currently in force. Cheap (an `Arc` clone); hold the
    /// snapshot for one unit of work so it sees one consistent config.
    pub fn live(&self) -> Arc<LiveConfig> {
        self.live
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// D2: the last config-file reload failure, cleared by the next
    /// successful apply.
    pub fn config_error(&self) -> Option<ConfigErrorInfo> {
        self.config_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_config_error(&self, error: Option<ConfigErrorInfo>) {
        *self
            .config_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = error;
    }

    /// The reload path (D1), shared by the file watcher and future config
    /// writers: validate `config`, swap it in, and reconcile the scheduler.
    /// On error nothing changes — the previous config keeps running.
    ///
    /// Reconcile: repos new to the set enter the rotation (no `next_poll`
    /// yet, so the next scheduler pass polls them; the scheduler is woken
    /// for that); removed repos leave the rotation, their queued reviews are
    /// discarded (in-flight ones finish), and their state records stay in
    /// `state.json` but drop out of status; a changed poll interval
    /// reschedules the repo from the next pass.
    pub async fn apply_config(&self, config: Config) -> Result<ReloadSummary, ConfigError> {
        self.swap_config(config, true).await
    }

    /// Decision "review mode applies in memory when the config file is
    /// broken": apply `config` (the last-good config with one change) live
    /// WITHOUT writing the file. The watcher's fingerprint is untouched, so
    /// the 1 s hash-poll does not re-read the unchanged broken file and
    /// cannot revert the change; `config_error` stays set. Once the file is
    /// fixed (its hash changes) the fixed file is applied and wins — the
    /// in-memory change is lost then, as it is on restart.
    pub async fn apply_config_unpersisted(
        &self,
        config: Config,
    ) -> Result<ReloadSummary, ConfigError> {
        let _seen = self.config_seen.lock().await;
        self.swap_config(config, false).await
    }

    async fn swap_config(
        &self,
        config: Config,
        clear_error: bool,
    ) -> Result<ReloadSummary, ConfigError> {
        let next = Arc::new(LiveConfig::from_config(config)?);
        let mut summary = ReloadSummary::default();
        {
            // Swap under the runtime lock: anything that checks the live
            // repo set while holding it sees either the old set with the
            // old runtime, or the new set with the reconciled runtime.
            let mut rt = self.runtime.lock().await;
            let prev = std::mem::replace(
                &mut *self
                    .live
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
                next.clone(),
            );
            for repo in &next.repos {
                match prev.repo(&repo.name) {
                    None => summary.added.push(repo.name.clone()),
                    Some(old) => {
                        if old.poll_interval_minutes != repo.poll_interval_minutes {
                            rt.next_poll.remove(&repo.name);
                        }
                        // D11: a changed schedule re-derives its next tick.
                        if old.schedule != repo.schedule {
                            rt.next_review_tick.remove(&repo.name);
                        }
                    }
                }
            }
            for repo in &prev.repos {
                if next.repo(&repo.name).is_none() {
                    summary.removed.push(repo.name.clone());
                    rt.next_poll.remove(&repo.name);
                    rt.last_poll.remove(&repo.name);
                    rt.next_review_tick.remove(&repo.name);
                }
            }
            // Queued reviews of removed repos — and of repos now in mode
            // `off` (D3: stop reviewing immediately) — are discarded.
            rt.queued.retain(|job| {
                let keep = next
                    .repo(&job.repo)
                    .is_some_and(|repo| repo.review_mode != ReviewMode::Off);
                if !keep {
                    summary.discarded.push(job.key.clone());
                }
                keep
            });
        }
        if clear_error {
            self.set_config_error(None);
        }
        self.config_applied.notify_one();
        Ok(summary)
    }

    /// D1/D2: re-read the config file if its content changed since the last
    /// check and apply it; a failed read/parse/validate keeps the last-good
    /// config running and records `config_error` until a good read.
    pub async fn check_config_file(&self) {
        let Some(path) = self.deps.config_path.as_deref() else {
            return;
        };
        let mut seen = self.config_seen.lock().await;
        let read = std::fs::read(path);
        let fingerprint = match &read {
            Ok(bytes) => ConfigFingerprint::Content(content_hash(bytes)),
            Err(_) => ConfigFingerprint::Unreadable,
        };
        if *seen == Some(fingerprint) {
            return;
        }
        *seen = Some(fingerprint);

        let parsed = match read {
            Ok(bytes) => String::from_utf8(bytes)
                .map_err(|e| ConfigError::Parse {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })
                .and_then(|raw| Config::parse(&raw, path)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(ConfigError::NotFound(path.to_path_buf()))
            }
            Err(source) => Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            }),
        };
        let applied = match parsed {
            Ok(config) => self.apply_config(config).await,
            Err(e) => Err(e),
        };
        match applied {
            Ok(summary) if summary.is_noop() => tracing::debug!("config re-read; no repo changes"),
            Ok(summary) => tracing::info!(
                "config reloaded: added {:?}, removed {:?}, discarded {} queued review(s)",
                summary.added,
                summary.removed,
                summary.discarded.len()
            ),
            Err(e) => {
                tracing::warn!("config reload failed; keeping the last-good config: {e}");
                self.set_config_error(Some(ConfigErrorInfo {
                    message: e.to_string(),
                    at: Utc::now(),
                }));
            }
        }
    }

    /// D5: the validated config write behind `PUT /config` and
    /// `POST /review-mode`. Validates `config` in full first — on error
    /// nothing is written and nothing changes — then persists atomically
    /// (0600 temp file + rename) and hot-applies through [`apply_config`],
    /// the same path the reload watcher uses. `text` is the exact file
    /// content to write (a raw-YAML submission keeps its comments); `None`
    /// serializes `config`. The watcher's fingerprint is updated under its
    /// lock, so it sees the written file as already applied.
    ///
    /// [`apply_config`]: Shared::apply_config
    pub async fn write_config(
        &self,
        config: Config,
        text: Option<String>,
    ) -> Result<ReloadSummary, ConfigWriteError> {
        let path = self
            .deps
            .config_path
            .as_deref()
            .ok_or(ConfigWriteError::NoConfigFile)?;
        LiveConfig::from_config(config.clone()).map_err(ConfigWriteError::Invalid)?;
        let text = match text {
            Some(text) => text,
            None => serde_yaml::to_string(&config).map_err(|e| {
                ConfigWriteError::Persist(ConfigError::Parse {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })
            })?,
        };
        let mut seen = self.config_seen.lock().await;
        config::write_atomic(path, text.as_bytes()).map_err(ConfigWriteError::Persist)?;
        *seen = Some(ConfigFingerprint::Content(content_hash(text.as_bytes())));
        // Validated above; a failure here would mean validation is not
        // deterministic — surface it rather than pretend it applied.
        let summary = self
            .apply_config(config)
            .await
            .map_err(ConfigWriteError::Invalid)?;
        tracing::info!(
            "config written over IPC: added {:?}, removed {:?}, discarded {} queued review(s)",
            summary.added,
            summary.removed,
            summary.discarded.len()
        );
        Ok(summary)
    }

    /// The config file as it is on disk right now (the base for focused
    /// edits like `POST /review-mode`, so external edits the watcher has
    /// not applied yet are not overwritten by the running config).
    pub fn load_config_file(&self) -> Result<Config, ConfigWriteError> {
        let path = self
            .deps
            .config_path
            .as_deref()
            .ok_or(ConfigWriteError::NoConfigFile)?;
        let config = Config::load(path).map_err(ConfigWriteError::Invalid)?;
        config.validate().map_err(ConfigWriteError::Invalid)?;
        Ok(config)
    }

    /// D12: the gh-authenticated login previously resolved for this
    /// credential reference, if any.
    pub fn cached_gh_login(&self, credential: Option<&str>) -> Option<String> {
        self.gh_logins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&credential.map(str::to_string))
            .cloned()
    }

    pub fn remember_gh_login(&self, credential: Option<&str>, login: &str) {
        self.gh_logins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(credential.map(str::to_string), login.to_string());
    }

    /// Point-in-time copy of the state truth. Readers that must not block on
    /// an in-flight review (only `GET /status`) use this cache; everything
    /// that mutates takes [`Shared::lock_state`].
    pub fn snapshot(&self) -> StateFile {
        self.state_cache.borrow().clone()
    }

    /// Exclusive access to the state truth. Callers must `publish()` before
    /// releasing the guard (the cache is only refreshed at mutation points).
    pub async fn lock_state(&self) -> tokio::sync::MutexGuard<'_, StateFile> {
        self.state.lock().await
    }

    /// Persist the truth to disk and refresh the read cache. Call while
    /// holding the state lock.
    pub(crate) fn publish(&self, state: &StateFile) {
        if let Err(e) = self.deps.store.save(state) {
            tracing::warn!("failed to save PR state: {e}");
        }
        // send_replace (not send): the cache has no receivers to wake, and
        // send would refuse to update a receiver-less channel.
        self.state_cache.send_replace(state.clone());
    }

    /// Insert jobs into the queue (D6), deduplicated by PR key: an
    /// already-queued key keeps its older due time unless the new job is
    /// forced; in-flight keys are never re-queued. Jobs for repos whose
    /// effective review mode is `off` are dropped, forced or not (D3).
    /// Returns the enqueued keys.
    pub async fn enqueue(&self, jobs: Vec<Job>) -> Vec<String> {
        let mut rt = self.runtime.lock().await;
        // Checked under the runtime lock: a job racing a reload that removed
        // its repo is dropped here instead of slipping into the queue.
        let live = self.live();
        let mut enqueued = Vec::new();
        for job in jobs {
            let reviewing = live
                .repo(&job.repo)
                .is_some_and(|repo| repo.review_mode != ReviewMode::Off);
            if rt.in_flight.contains(&job.key) || !reviewing {
                continue;
            }
            if let Some(existing) = rt.queued.iter_mut().find(|j| j.key == job.key) {
                let mut upgraded = false;
                if job.force && !existing.force {
                    existing.due = job.due;
                    existing.force = true;
                    upgraded = true;
                }
                // D1: a request carrying options replaces the queued run's
                // options; a plain request leaves them intact.
                if !job.overrides.is_empty() && existing.overrides != job.overrides {
                    existing.overrides = job.overrides;
                    upgraded = true;
                }
                if upgraded {
                    enqueued.push(job.key);
                }
                continue;
            }
            enqueued.push(job.key.clone());
            rt.queued.push(job);
        }
        if !enqueued.is_empty() {
            self.work_available.notify_one();
        }
        enqueued
    }

    /// D5: set the daemon-wide pause end (persisted in the state file, so
    /// it survives restarts). Returns whether the stored value changed.
    pub async fn pause_until(&self, until: DateTime<Utc>) -> bool {
        let mut state = self.lock_state().await;
        let changed = state.paused_until != Some(until);
        if changed {
            state.paused_until = Some(until);
            self.publish(&state);
        }
        changed
    }

    /// D5: lift the pause. Returns whether a pause was in effect at `now`
    /// (an expired leftover is cleared silently).
    pub async fn resume(&self, now: DateTime<Utc>) -> bool {
        let mut state = self.lock_state().await;
        let was_paused = state.is_paused(now);
        if state.paused_until.take().is_some() {
            self.publish(&state);
        }
        was_paused
    }

    /// Queue depth + in-flight count for `GET /status`.
    /// Whether a review of `key` is running right now.
    pub async fn is_in_flight(&self, key: &str) -> bool {
        self.runtime.lock().await.in_flight.contains(key)
    }

    pub async fn queue_stats(&self) -> (usize, usize) {
        let rt = self.runtime.lock().await;
        (rt.queued.len(), rt.in_flight.len())
    }

    /// Keys of queued (not yet running) reviews, oldest-due first.
    pub async fn queued_keys(&self) -> Vec<String> {
        let rt = self.runtime.lock().await;
        let mut queued: Vec<&Job> = rt.queued.iter().collect();
        queued.sort_by_key(|job| job.due);
        queued.iter().map(|job| job.key.clone()).collect()
    }

    pub async fn last_poll_map(&self) -> BTreeMap<String, DateTime<Utc>> {
        self.runtime.lock().await.last_poll.clone()
    }

    /// The soonest scheduled repo poll (`/status` `next_poll`, the menu's
    /// "next check"); `None` before any poll is planned.
    pub async fn next_poll_at(&self) -> Option<DateTime<Utc>> {
        self.runtime.lock().await.next_poll.values().min().copied()
    }

    /// Record a client announcement: the IPC auth middleware calls this when
    /// an authenticated request carries `X-Prx-Client` (D3).
    pub fn note_menubar_seen(&self) {
        self.menubar_liveness.note_seen();
    }

    /// True when a client announced itself within `window` (D3: the menubar
    /// polls every 10 s, so the daemon uses 90 s ≈ 9 missed polls).
    pub fn menubar_recently_seen(&self, window: Duration) -> bool {
        self.menubar_liveness.recently_seen(window)
    }
}

// ------------------------------------------------------------- pid guard

#[derive(Debug, Error)]
pub enum PidError {
    #[error("daemon already running (pid {0})")]
    AlreadyRunning(u32),
    #[error("failed to write pid file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Single-instance guard (A16): `daemon.pid` is created exclusively; a
/// stale file (owning process gone) is reclaimed. On drop the file is
/// removed only if it still names this process, so a successor's file is
/// never deleted.
#[derive(Debug)]
pub struct PidGuard {
    path: PathBuf,
    pid: u32,
}

impl PidGuard {
    pub fn acquire(data_dir: &Path) -> Result<PidGuard, PidError> {
        let path = data_dir.join("daemon.pid");
        std::fs::create_dir_all(data_dir).map_err(|source| PidError::Write {
            path: path.clone(),
            source,
        })?;
        for _ in 0..3 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let pid = std::process::id();
                    writeln!(file, "{pid}").map_err(|source| PidError::Write {
                        path: path.clone(),
                        source,
                    })?;
                    return Ok(PidGuard { path, pid });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    match read_pid(&path) {
                        // Our own leftover file (crash without drop): steal it.
                        Some(pid) if pid == std::process::id() => {}
                        // Another live process owns it: single instance (A16).
                        Some(pid) if process_alive(pid) => {
                            return Err(PidError::AlreadyRunning(pid));
                        }
                        // Stale or unreadable: reclaim.
                        _ => {}
                    }
                    let _ = std::fs::remove_file(&path);
                }
                Err(source) => return Err(PidError::Write { path, source }),
            }
        }
        Err(PidError::Write {
            path,
            source: std::io::Error::other("pid file could not be created after reclaiming"),
        })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if read_pid(&self.path) == Some(self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Read the pid recorded in a pid file, if it parses.
pub fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// True when a process with this pid exists. `kill(pid, 0)` sends no signal;
/// EPERM means the process is alive but owned by another user.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // SAFETY: kill(2) with signal 0 performs only the permission check.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}

// ------------------------------------------------------------- lifecycle

/// Foreground daemon entry: `prx daemon start --foreground` (user-facing and
/// the detached child's re-exec target). Logging is initialized by `main`.
pub async fn run_foreground() -> Result<()> {
    let config_path = paths::config_path();
    let config = Config::load(&config_path).context("cannot start the daemon")?;
    config.validate().context("configuration is invalid")?;
    let deps = Deps::production(config_path, paths::data_dir());

    let pid_guard = PidGuard::acquire(&deps.data_dir).map_err(|e| anyhow::anyhow!("{e}"))?;
    let handle = spawn_core(deps, config, SchedulerOptions::default(), Some(pid_guard)).await?;
    tracing::info!(
        "prx daemon started (pid {}): watching {} repo(s), IPC on 127.0.0.1:{}",
        handle.shared.pid,
        handle.shared.live().repos.len(),
        handle.port
    );

    wait_for_shutdown_signal().await;
    // Graceful shutdown choice (documented): stop scheduling, let the current
    // tick finish, persist state, and exit. In-flight reviews are abandoned —
    // the pipeline persists state only at completion, so state.json stays
    // consistent, and the agent child is killed via kill_on_drop. Queued but
    // unstarted jobs are discarded; the next start re-discovers everything
    // by polling (A8: creation-time cooldowns make that safe).
    tracing::info!("shutdown signal received; finishing up");
    handle.shutdown().await;
    Ok(())
}

/// Resolve the shutdown future: SIGTERM and SIGINT (unix).
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("cannot install SIGTERM handler");
        let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("cannot install SIGINT handler");
        tokio::select! {
            _ = term.recv() => {},
            _ = int.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// A running daemon instance (production foreground run or in-process test).
pub struct DaemonHandle {
    pub port: u16,
    pub token: String,
    pub shared: Arc<Shared>,
    shutdown_tx: watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    _pid_guard: Option<PidGuard>,
}

impl DaemonHandle {
    /// Stop the daemon: signal shutdown, join the core tasks, and wait
    /// (bounded) for in-flight jobs to wind down.
    pub async fn shutdown(mut self) {
        let _ = self.shutdown_tx.send(true);
        for task in self.tasks.drain(..) {
            let _ = task.await;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let (_, in_flight) = self.shared.queue_stats().await;
            if in_flight == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        drop(self._pid_guard.take());
    }
}

/// Spawn the IPC server + scheduler + drainer + config-reload watcher as
/// tokio tasks, running on `config` (validated here). Shared by
/// `run_foreground` and the in-process test harness.
pub async fn spawn_core(
    deps: Deps,
    config: Config,
    options: SchedulerOptions,
    pid_guard: Option<PidGuard>,
) -> Result<DaemonHandle> {
    // A17: compact history older than the retention window on daemon start.
    if let Err(e) = history::compact(&deps.history_path, HISTORY_RETENTION_DAYS) {
        tracing::warn!("history compaction failed: {e}");
    }

    let token = crate::ipc::generate_token();
    let shared = Arc::new(Shared::new(deps, config, token, options)?);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let (listener, port) = crate::ipc::bind(&shared).await?;
    let server = tokio::spawn(crate::ipc::serve(
        listener,
        shared.clone(),
        shutdown_rx.clone(),
    ));
    let scheduler = tokio::spawn(scheduler_loop(shared.clone(), shutdown_rx.clone()));
    let drainer = tokio::spawn(drain_loop(shared.clone(), shutdown_rx.clone()));
    let reloader = tokio::spawn(config_reload_loop(shared.clone(), shutdown_rx.clone()));

    Ok(DaemonHandle {
        port,
        token: shared.token.clone(),
        shared,
        shutdown_tx,
        tasks: vec![server, scheduler, drainer, reloader],
        _pid_guard: pid_guard,
    })
}

/// `prx daemon start` (detached): re-exec this binary in the background.
/// The child survives the parent via `process_group(0)` and nulled stdio.
pub fn start_detached() -> Result<()> {
    if let Some(pid) = read_pid(&paths::daemon_pid_file()) {
        if process_alive(pid) {
            // A16: starting twice is idempotent.
            println!("daemon already running (pid {pid})");
            return Ok(());
        }
    }
    let exe = std::env::current_exe().context("cannot locate the prx binary")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["daemon", "start", "--foreground"])
        .env("PRX_DAEMON_CHILD", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn().context("failed to spawn the daemon process")?;
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if read_pid(&paths::daemon_pid_file()) == Some(pid) {
            println!("daemon started (pid {pid})");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!(
        "daemon (pid {pid}) did not claim its pid file within 5s — \
         check `prx daemon logs`"
    );
}

/// `prx daemon stop`: SIGTERM, wait for exit, SIGKILL only as last resort.
pub async fn run_stop() -> Result<()> {
    let pid_path = paths::daemon_pid_file();
    let Some(pid) = read_pid(&pid_path) else {
        println!("daemon not running");
        return Ok(());
    };
    if !process_alive(pid) {
        println!("stale pid file (pid {pid} is gone); removed");
        let _ = std::fs::remove_file(&pid_path);
        return Ok(());
    }
    send_signal(pid, libc::SIGTERM);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if !process_alive(pid) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if process_alive(pid) {
        tracing::warn!("daemon pid {pid} ignored SIGTERM; sending SIGKILL");
        send_signal(pid, libc::SIGKILL);
        tokio::time::sleep(Duration::from_secs(2)).await;
        if process_alive(pid) {
            bail!("daemon (pid {pid}) did not exit");
        }
    }
    if read_pid(&pid_path) == Some(pid) {
        let _ = std::fs::remove_file(&pid_path);
    }
    println!("daemon stopped");
    Ok(())
}

#[cfg(unix)]
fn send_signal(pid: u32, signal: i32) {
    // SAFETY: plain kill(2) with a constant signal number.
    let _ = unsafe { libc::kill(pid as i32, signal) };
}

#[cfg(not(unix))]
fn send_signal(_pid: u32, _signal: i32) {}

/// `prx daemon status`: pid file + process liveness (A16).
pub fn run_status_cmd() {
    let Some(pid) = read_pid(&paths::daemon_pid_file()) else {
        println!("daemon not running");
        return;
    };
    if !process_alive(pid) {
        println!("daemon not running (stale pid file: pid {pid})");
        return;
    }
    match std::fs::read_to_string(paths::daemon_port_file()) {
        Ok(port) => println!(
            "daemon running (pid {pid}, IPC on 127.0.0.1:{})",
            port.trim()
        ),
        Err(_) => println!("daemon running (pid {pid})"),
    }
}

/// `prx daemon logs`: tail the last 100 lines of the newest log file (A15).
pub fn run_logs_cmd() -> Result<()> {
    let logs_dir = paths::logs_dir();
    let newest = newest_log_file(&logs_dir)
        .ok_or_else(|| anyhow::anyhow!("no daemon logs yet under {}", logs_dir.display()))?;
    let raw = std::fs::read_to_string(&newest).unwrap_or_default();
    let lines: Vec<&str> = raw.lines().collect();
    let skip = lines.len().saturating_sub(100);
    for line in &lines[skip..] {
        println!("{line}");
    }
    Ok(())
}

/// Newest `prx.log.YYYY-MM-DD` in `dir` (by parsed date; unparseable names
/// like editor temp files are ignored).
pub fn newest_log_file(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<(chrono::NaiveDate, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter_map(|p| {
            let date = p
                .file_name()?
                .to_string_lossy()
                .strip_prefix("prx.log.")?
                .parse::<chrono::NaiveDate>()
                .ok()?;
            Some((date, p))
        })
        .collect();
    files.sort_by_key(|(date, _)| *date);
    files.pop().map(|(_, path)| path)
}

// ------------------------------------------------------------- scheduler

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollMode {
    /// Poll every repo (startup, /review-now).
    All,
    /// Poll only repos whose (jittered) interval has elapsed.
    Due,
}

async fn scheduler_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    let mut ticker = tokio::time::interval(shared.options.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    if shared.options.startup_poll {
        poll_repos(&shared, PollMode::All).await;
    }
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => poll_repos(&shared, PollMode::Due).await,
            _ = shared.poll_requested.notified() => {
                // D1: review-now (menu "Refresh now") implies a reload check,
                // run first so the poll already covers newly added repos.
                shared.check_config_file().await;
                poll_repos(&shared, PollMode::All).await;
            }
            // Added repos have no next_poll yet, so a Due pass polls them.
            _ = shared.config_applied.notified() => poll_repos(&shared, PollMode::Due).await,
        }
    }
}

/// D1: hash-poll the config file every `options.config_check` and apply
/// changes (no-op when the daemon has no config path to watch).
async fn config_reload_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    if shared.deps.config_path.is_none() {
        return;
    }
    let mut ticker = tokio::time::interval(shared.options.config_check);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = ticker.tick() => shared.check_config_file().await,
        }
    }
}

/// Poll repos per `mode`, sync state through the machine, and enqueue every
/// due PR. One failed poll is logged and retried after the normal interval —
/// it must never stall the other repos or the queue.
///
/// Review-mode and schedule gates (D3/D11): a repo in mode `off` is not
/// polled at all (no GitHub calls, nothing enqueued, so no poll-driven
/// notifications; its last-known state keeps serving). A repo with an
/// effective `schedule` keeps polling on `poll_interval`, but its due-scan
/// only runs at schedule ticks — the tick also forces a fresh poll so the
/// scan sees current state; cooldowns and skip rules still decide which PRs
/// are due there.
async fn poll_repos(shared: &Arc<Shared>, mode: PollMode) {
    let now = (shared.options.clock)();
    let mut due_jobs = Vec::new();
    // One config snapshot per pass; a reload mid-pass applies from the next.
    let live = shared.live();
    for repo in &live.repos {
        if repo.review_mode == ReviewMode::Off {
            continue;
        }
        let (cadence, next_poll) = {
            let mut rt = shared.runtime.lock().await;
            let cadence = review_cadence(&mut rt.next_review_tick, repo, now);
            (cadence, rt.next_poll.get(&repo.name).copied())
        };
        if mode == PollMode::Due && cadence != ReviewCadence::Tick && now < next_poll.unwrap_or(now)
        {
            continue;
        }
        let deps = EffectsDeps::for_repo(&shared.deps.notifier, &live.tiers, repo);
        let synced = {
            let mut state = shared.lock_state().await;
            match status::sync_repo(&shared.deps.gh, repo, &mut state, now, &deps).await {
                Ok(()) => {
                    state.last_poll = Some(now);
                    // D5: a pause stops the due-scan but not the poll; a
                    // tick that arrives while paused is still consumed below.
                    let origin = match cadence {
                        ReviewCadence::Tick => EnqueueOrigin::ScheduledTick,
                        _ => EnqueueOrigin::DueScan,
                    };
                    if cadence != ReviewCadence::Waiting
                        && pause::enqueue_allowed(&state, repo.review_mode, origin, now)
                    {
                        for (key, rec) in state.prs.iter() {
                            if rec.repo == repo.name
                                && machine::is_review_due(rec, &repo.cooldown, now)
                            {
                                due_jobs.push(Job {
                                    key: key.clone(),
                                    repo: repo.name.clone(),
                                    due: now,
                                    force: false,
                                    overrides: ReviewOverrides::default(),
                                });
                            }
                        }
                    }
                    shared.publish(&state);
                    true
                }
                Err(e) => {
                    tracing::warn!("poll of {} failed: {e}", repo.name);
                    false
                }
            }
        };
        let mut rt = shared.runtime.lock().await;
        if shared.live().repo(&repo.name).is_none() {
            // Removed by a reload while this pass ran: leave it unscheduled.
            continue;
        }
        if synced {
            rt.last_poll.insert(repo.name.clone(), now);
        }
        // Failed polls retry after the plain interval (no jitter) rather than
        // hammering gh every tick.
        rt.next_poll.insert(
            repo.name.clone(),
            next_poll_after(repo, now, shared.options, synced),
        );
        if cadence == ReviewCadence::Tick {
            // The tick is consumed even when its poll failed (like a cron
            // job that errored): the next attempt is the next tick, not a
            // retry storm against a failing gh.
            match repo
                .schedule
                .as_deref()
                .and_then(|e| next_schedule_tick(e, now))
            {
                Some(tick) => rt.next_review_tick.insert(repo.name.clone(), tick),
                None => rt.next_review_tick.remove(&repo.name),
            };
        }
    }
    if !due_jobs.is_empty() {
        shared.enqueue(due_jobs).await;
    }
}

/// When a repo's due-scan may enqueue reviews (D11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewCadence {
    /// No schedule: every successful poll runs the due-scan (pre-D11).
    Interval,
    /// Scheduled, between ticks: poll for freshness, enqueue nothing.
    Waiting,
    /// Scheduled and a tick has arrived: poll now and run the due-scan.
    Tick,
}

/// Classify `repo` against its schedule tick at `now`. A repo first seen
/// with a schedule (startup, reload, changed expression) gets its next
/// upcoming tick and waits for it — it never enqueues on sight.
fn review_cadence(
    ticks: &mut BTreeMap<String, DateTime<Utc>>,
    repo: &EffectiveRepo,
    now: DateTime<Utc>,
) -> ReviewCadence {
    let Some(expr) = repo.schedule.as_deref() else {
        ticks.remove(&repo.name);
        return ReviewCadence::Interval;
    };
    match ticks.get(&repo.name) {
        Some(tick) if now >= *tick => ReviewCadence::Tick,
        Some(_) => ReviewCadence::Waiting,
        None => {
            if let Some(tick) = next_schedule_tick(expr, now) {
                ticks.insert(repo.name.clone(), tick);
            }
            ReviewCadence::Waiting
        }
    }
}

/// The first tick of cron `expr` strictly after `now`, evaluated in local
/// time (D11). `None` for an expression with no future fire (or one that
/// fails to parse — validation rejects those before they get here).
pub fn next_schedule_tick(expr: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    next_schedule_tick_in(expr, now, &chrono::Local)
}

/// [`next_schedule_tick`] evaluated in time zone `tz`.
///
/// The cron expression is walked over naive wall-clock times (as if UTC, so
/// the `cron` crate never meets a DST transition: in `tz` directly it skips
/// fires in a repeated or skipped hour, and panics in zones whose local
/// midnight does not exist). Each wall-clock candidate is then resolved
/// with [`resolve_wall_clock`], and the first instant strictly after `now`
/// wins — so a wall-clock time that repeats fires once and no instant fires
/// twice.
fn next_schedule_tick_in<Tz: chrono::TimeZone>(
    expr: &str,
    now: DateTime<Utc>,
    tz: &Tz,
) -> Option<DateTime<Utc>> {
    let schedule = config::parse_schedule(expr).ok()?;
    let wall_now = now.with_timezone(tz).naive_local().and_utc();
    // Candidates resolving to <= now only occur inside a repeated hour, so
    // the bound is a hang guard, never reached in practice.
    schedule
        .after(&wall_now)
        .take(10_000)
        .find_map(|wall| resolve_wall_clock(tz, &wall.naive_utc()).filter(|tick| *tick > now))
}

/// The instant a cron wall-clock time `wall` denotes in `tz`: the instant
/// itself; the *first* occurrence of a repeated (fall-back) time; and for a
/// skipped (spring-forward) time, the same wall-clock distance past the
/// gap's start (e.g. 02:30 in a 02:00→03:00 gap fires at 03:30).
fn resolve_wall_clock<Tz: chrono::TimeZone>(
    tz: &Tz,
    wall: &chrono::NaiveDateTime,
) -> Option<DateTime<Utc>> {
    if let Some(tick) = tz.from_local_datetime(wall).earliest() {
        return Some(tick.with_timezone(&Utc));
    }
    // In a gap: step back to the last valid wall-clock time before it and
    // carry the offset in effect there forward (gaps are at most hours).
    (1..=4 * 24).find_map(|quarters| {
        let back = chrono::Duration::minutes(15 * quarters);
        tz.from_local_datetime(&(*wall - back))
            .earliest()
            .map(|before| (before + back).with_timezone(&Utc))
    })
}

/// The next poll time for a repo: configured interval × test scale, ±10%
/// jitter when enabled (A7). `jitter_on=false` also skips jitter (failed
/// polls and tests that need determinism).
fn next_poll_after(
    repo: &EffectiveRepo,
    now: DateTime<Utc>,
    options: SchedulerOptions,
    jitter_on: bool,
) -> DateTime<Utc> {
    let mut secs = repo.poll_interval_minutes as f64 * 60.0 * options.interval_scale;
    if options.jitter && jitter_on {
        secs *= rand::rng().random_range(0.9f64..1.1);
    }
    let millis = (secs * 1000.0).max(1000.0) as i64;
    now + chrono::Duration::milliseconds(millis)
}

/// Drainer: pop the oldest-due queued job while the D6 concurrency cap has a
/// free slot, and run it. Sleeps on `work_available` when there is nothing
/// runnable (empty queue or cap reached — job completion wakes it).
async fn drain_loop(shared: Arc<Shared>, mut shutdown: watch::Receiver<bool>) {
    loop {
        let job = {
            let mut rt = shared.runtime.lock().await;
            let next = rt
                .queued
                .iter()
                .enumerate()
                .min_by_key(|(_, job)| job.due)
                .map(|(i, _)| i);
            match next {
                Some(i) if shared.semaphore.available_permits() > 0 => {
                    let job = rt.queued.remove(i);
                    rt.in_flight.insert(job.key.clone());
                    Some(job)
                }
                _ => None,
            }
        };
        if let Some(job) = job {
            match shared.semaphore.clone().acquire_owned().await {
                Ok(permit) => {
                    tokio::spawn(execute_job(shared.clone(), job, permit));
                }
                Err(_) => break, // semaphore closed: shutting down
            }
        } else {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = shared.work_available.notified() => {}
            }
        }
    }
}

/// Run one queued review. The state lock is held for the whole pipeline run:
/// all state writes flow through this single owner (A4), which is what makes
/// a restart unable to duplicate a review. A failed review never stops the
/// queue — the pipeline already ran the on_failure effects; we log and move
/// on (DEV-PLAN §9).
async fn execute_job(shared: Arc<Shared>, job: Job, _permit: tokio::sync::OwnedSemaphorePermit) {
    // The config in force when the job starts governs the whole run.
    let live = shared.live();
    let Some(repo) = live.repo(&job.repo).cloned() else {
        tracing::warn!(
            "{}: repo no longer configured; dropping queued review",
            job.key
        );
        finish_job(&shared, &job).await;
        return;
    };
    // D3: the mode may have turned `off` between dequeue and here.
    if repo.review_mode == ReviewMode::Off {
        tracing::info!("{}: review mode is off; dropping queued review", job.key);
        finish_job(&shared, &job).await;
        return;
    }

    // Non-force jobs re-check due-ness against fresh state: a snooze or
    // ignore may have landed between enqueue and execution (A22: forced
    // jobs bypass this check).
    if !job.force {
        let now = (shared.options.clock)();
        let due = {
            let state = shared.lock_state().await;
            state
                .prs
                .get(&job.key)
                .is_some_and(|rec| machine::is_review_due(rec, &repo.cooldown, now))
        };
        if !due {
            tracing::debug!("{}: no longer due; dropping queued review", job.key);
            finish_job(&shared, &job).await;
            return;
        }
    }

    // D2: per-run options apply to a clone of the live repo settings. The
    // engine is re-validated here: the config may have reloaded since the
    // enqueue, and a vanished engine fails through the normal failure path.
    let (repo, fresh_session, unusable) =
        match review::apply_overrides(&live.config, &repo, &job.overrides) {
            Ok((run_repo, fresh_session)) => (run_repo, fresh_session, None),
            Err(e) => {
                // Recorded under the requested engine; the retained session
                // is left alone (fresh_session), as for any override run.
                let mut run_repo = repo.clone();
                run_repo.engine = job.overrides.engine.clone().unwrap_or(run_repo.engine);
                (run_repo, true, Some(format!("{e:#}")))
            }
        };

    tracing::info!("reviewing {}", job.key);
    let writer = shared
        .deps
        .gh
        .for_credential(repo.identity.credential.as_deref());
    let pipeline = review::Pipeline {
        gh: &shared.deps.gh,
        writer: &writer,
        adapter: &shared.deps.adapter,
        notifier: &shared.deps.notifier,
        repo: &repo,
        tiers: &live.tiers,
        store: &shared.deps.store,
        dry_run: false,
        fresh_session,
    };
    if let Some(reason) = unusable {
        let outcome = {
            let mut state = shared.lock_state().await;
            let result = pipeline.fail_before_run(&mut state, &job.key, reason).await;
            shared.publish(&state);
            result
        };
        match outcome {
            Ok(outcome) => tracing::warn!("{}: {:?}", job.key, outcome),
            Err(e) => tracing::warn!("{}: review failed: {e:#}", job.key),
        }
        finish_job(&shared, &job).await;
        return;
    }
    // The review proper — through its state transition, history, and
    // completion notifications — commits under the state lock.
    let outcome = {
        let mut state = shared.lock_state().await;
        let result = pipeline
            .run_with_extra_prompt(&mut state, &job.key, job.overrides.extra_prompt.as_deref())
            .await;
        shared.publish(&state);
        result
    };
    match outcome {
        Ok(review::ReviewOutcome::Completed(c)) => {
            tracing::info!(
                "{}: reviewed ({:?}) — {} blocking, {} nits",
                job.key,
                c.verdict,
                c.blocking_count,
                c.nit_count
            );
            auto_open(&shared, &repo, &job, c.verdict).await;
            // Design B2: without a manifest brief, the on-demand brief comes
            // last, generated with the state lock released (it may take up
            // to its timeout) and stored afterwards under a short re-lock.
            // The job stays in flight until then, so no second review of
            // this PR interleaves. A brief already running for this PR
            // (`POST /brief`) stands in for it.
            if let Some(request) = &c.brief_request {
                if shared.try_begin_brief(&job.key) {
                    let run = pipeline.generate_brief(request).await;
                    let stored = {
                        let mut state = shared.lock_state().await;
                        let stored = review::store_brief(&mut state, request, &run, Utc::now());
                        shared.publish(&state);
                        stored
                    };
                    shared.end_brief(&job.key, run.error.filter(|_| !stored));
                } else {
                    tracing::info!(
                        "{}: a brief is already being written; skipping the post-review brief",
                        job.key
                    );
                }
            }
        }
        Ok(other) => tracing::info!("{}: {:?}", job.key, other),
        Err(e) => tracing::warn!("{}: review failed: {e:#}", job.key),
    }
    finish_job(&shared, &job).await;
}

/// Verdict-triggered auto-open (design D9; spec prx/browser-open): after a
/// completed review — monitor mode included, never failed/skipped runs —
/// open the PR's `html_url` in the effective browser when `auto_open`
/// matches the verdict. Best-effort: failures are logged. Runs outside the
/// state lock, on the blocking pool (the opener waits for its process).
async fn auto_open(shared: &Arc<Shared>, repo: &EffectiveRepo, job: &Job, verdict: Verdict) {
    if !browser::auto_open_matches(repo.auto_open, verdict) {
        return;
    }
    let Some((repo_name, number)) = job
        .key
        .rsplit_once('#')
        .and_then(|(r, n)| Some((r.to_string(), n.parse::<u64>().ok()?)))
    else {
        return;
    };
    let url = status::pr_html_url(&repo_name, number);
    let opener = shared.deps.opener.clone();
    let browser_cfg = repo.browser.clone();
    let key = job.key.clone();
    tracing::info!("{key}: auto_open {:?}: opening {url}", repo.auto_open);
    let opened =
        tokio::task::spawn_blocking(move || browser::open_url(opener.as_ref(), &browser_cfg, &url))
            .await;
    match opened {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => tracing::warn!("{key}: auto-open failed: {e}"),
        Err(e) => tracing::warn!("{key}: auto-open task failed: {e}"),
    }
}

/// One on-demand brief run (design B2): generate outside the state lock,
/// then store under a short lock and publish. Failures are logged and
/// returned; the previous brief stays.
async fn run_brief(
    shared: &Arc<Shared>,
    live: &LiveConfig,
    repo: &EffectiveRepo,
    request: &review::BriefRequest,
) -> Option<String> {
    let writer = shared
        .deps
        .gh
        .for_credential(repo.identity.credential.as_deref());
    let pipeline = review::Pipeline {
        gh: &shared.deps.gh,
        writer: &writer,
        adapter: &shared.deps.adapter,
        notifier: &shared.deps.notifier,
        repo,
        tiers: &live.tiers,
        store: &shared.deps.store,
        dry_run: false,
        fresh_session: true,
    };
    tracing::info!("{}: writing brief", request.key);
    let run = pipeline.generate_brief(request).await;
    let mut state = shared.lock_state().await;
    let stored = review::store_brief(&mut state, request, &run, Utc::now());
    shared.publish(&state);
    match (run.error, stored) {
        (_, true) => {
            tracing::info!("{}: brief written", request.key);
            None
        }
        (Some(error), false) => {
            tracing::warn!("{}: brief failed: {error}", request.key);
            Some(error)
        }
        (None, false) => {
            tracing::info!("{}: brief not stored (superseded)", request.key);
            None
        }
    }
}

async fn finish_job(shared: &Arc<Shared>, job: &Job) {
    {
        let mut rt = shared.runtime.lock().await;
        rt.in_flight.remove(&job.key);
    }
    shared.work_available.notify_one();
}

// ------------------------------------------------------------- unit tests

#[cfg(test)]
mod tests {
    use super::*;

    /// Test-only DST time zones (no tz database needed): one standard and
    /// one daylight offset, daylight time in effect for UTC instants in
    /// `[dst_start, dst_end)`.
    mod dst {
        use chrono::{
            FixedOffset, MappedLocalTime, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone,
        };

        #[derive(Debug)]
        pub struct Rules {
            std_secs: i32,
            dst_secs: i32,
            /// (y, m, d, h) UTC of the spring-forward instant.
            dst_start: (i32, u32, u32, u32),
            /// (y, m, d, h) UTC of the fall-back instant.
            dst_end: (i32, u32, u32, u32),
        }

        /// US Eastern 2026: 02:00 EST → 03:00 EDT on Mar 8 (07:00Z),
        /// 02:00 EDT → 01:00 EST on Nov 1 (06:00Z).
        pub static EASTERN_2026: Rules = Rules {
            std_secs: -5 * 3600,
            dst_secs: -4 * 3600,
            dst_start: (2026, 3, 8, 7),
            dst_end: (2026, 11, 1, 6),
        };

        /// A midnight-transition zone (Chile-like, southern hemisphere):
        /// 00:00 -04 → 01:00 -03 on Sep 6 2026 (04:00Z), so local midnight
        /// does not exist that day; 00:00 -03 → 23:00 -04 on Apr 5 2026
        /// (03:00Z), so 23:00–24:00 on Apr 4 repeats.
        pub static MIDNIGHT_2026: Rules = Rules {
            std_secs: -4 * 3600,
            dst_secs: -3 * 3600,
            dst_start: (2026, 9, 6, 4),
            dst_end: (2026, 4, 5, 3),
        };

        #[derive(Debug, Clone, Copy)]
        pub struct Zone(pub &'static Rules);

        #[derive(Debug, Clone, Copy)]
        pub struct ZoneOffset {
            zone: &'static Rules,
            fixed: FixedOffset,
        }

        impl Offset for ZoneOffset {
            fn fix(&self) -> FixedOffset {
                self.fixed
            }
        }

        fn utc((y, m, d, h): (i32, u32, u32, u32)) -> NaiveDateTime {
            NaiveDate::from_ymd_opt(y, m, d)
                .unwrap()
                .and_hms_opt(h, 0, 0)
                .unwrap()
        }

        impl Zone {
            fn offset(&self, secs: i32) -> ZoneOffset {
                ZoneOffset {
                    zone: self.0,
                    fixed: FixedOffset::east_opt(secs).unwrap(),
                }
            }
            fn is_dst(&self, at: &NaiveDateTime) -> bool {
                let (start, end) = (utc(self.0.dst_start), utc(self.0.dst_end));
                if start < end {
                    start <= *at && *at < end
                } else {
                    // Southern hemisphere: DST spans the new year.
                    *at >= start || *at < end
                }
            }
        }

        impl TimeZone for Zone {
            type Offset = ZoneOffset;

            fn from_offset(offset: &ZoneOffset) -> Self {
                Zone(offset.zone)
            }

            fn offset_from_local_date(&self, local: &NaiveDate) -> MappedLocalTime<ZoneOffset> {
                self.offset_from_local_datetime(&local.and_time(NaiveTime::MIN))
            }

            fn offset_from_local_datetime(
                &self,
                local: &NaiveDateTime,
            ) -> MappedLocalTime<ZoneOffset> {
                // Earlier instant first: daylight (larger) offset, then std.
                let valid: Vec<ZoneOffset> = [self.0.dst_secs, self.0.std_secs]
                    .into_iter()
                    .map(|secs| self.offset(secs))
                    .filter(|o| {
                        let instant =
                            *local - chrono::Duration::seconds(o.fixed.local_minus_utc().into());
                        self.offset_from_utc_datetime(&instant).fixed == o.fixed
                    })
                    .collect();
                match valid.as_slice() {
                    [] => MappedLocalTime::None,
                    [one] => MappedLocalTime::Single(*one),
                    [first, second, ..] => MappedLocalTime::Ambiguous(*first, *second),
                }
            }

            fn offset_from_utc_date(&self, utc: &NaiveDate) -> ZoneOffset {
                self.offset_from_utc_datetime(&utc.and_time(NaiveTime::MIN))
            }

            fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> ZoneOffset {
                if self.is_dst(utc) {
                    self.offset(self.0.dst_secs)
                } else {
                    self.offset(self.0.std_secs)
                }
            }
        }
    }

    fn utc_at(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        use chrono::TimeZone;
        Utc.with_ymd_and_hms(y, m, d, h, min, 0).unwrap()
    }

    /// Every tick `expr` produces from `from` until `until`, by repeatedly
    /// asking for the next fire after the previous one (as the daemon does).
    fn ticks_between<Tz: chrono::TimeZone>(
        expr: &str,
        tz: &Tz,
        from: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Vec<DateTime<Utc>> {
        let mut ticks = Vec::new();
        let mut now = from;
        while let Some(tick) = next_schedule_tick_in(expr, now, tz) {
            assert!(tick > now, "tick {tick} not strictly after {now}");
            if tick > until {
                break;
            }
            ticks.push(tick);
            now = tick;
        }
        ticks
    }

    #[test]
    fn schedule_in_the_repeated_fall_back_hour_fires_exactly_once() {
        let tz = dst::Zone(&dst::EASTERN_2026);
        // 01:30 local happens twice on Nov 1 (05:30Z EDT, 06:30Z EST).
        let ticks = ticks_between(
            "30 1 * * *",
            &tz,
            utc_at(2026, 10, 31, 12, 0),
            utc_at(2026, 11, 2, 12, 0),
        );
        assert_eq!(
            ticks,
            [utc_at(2026, 11, 1, 5, 30), utc_at(2026, 11, 2, 6, 30)],
            "one deterministic fire (the first 01:30) on the fall-back day"
        );
        // Asked from inside the repeated hour (second pass, 01:10 EST), the
        // already-fired first 01:30 is not fired again: next is tomorrow.
        assert_eq!(
            next_schedule_tick_in("30 1 * * *", utc_at(2026, 11, 1, 6, 10), &tz),
            Some(utc_at(2026, 11, 2, 6, 30))
        );
        // A sub-hourly schedule across the fold never fires twice for one
        // wall-clock time and stays strictly increasing.
        let ticks = ticks_between(
            "*/30 * * * *",
            &tz,
            utc_at(2026, 11, 1, 4, 0),
            utc_at(2026, 11, 1, 8, 0),
        );
        let walls: Vec<_> = ticks
            .iter()
            .map(|t| t.with_timezone(&tz).naive_local())
            .collect();
        let mut unique = walls.clone();
        unique.dedup();
        assert_eq!(walls, unique, "no wall-clock time fires twice: {walls:?}");
    }

    #[test]
    fn schedule_in_the_skipped_spring_forward_hour_fires_once_after_the_gap() {
        let tz = dst::Zone(&dst::EASTERN_2026);
        // 02:30 local does not exist on Mar 8; it fires once, right after
        // the gap (03:30 EDT = 07:30Z), then normally the next day.
        let ticks = ticks_between(
            "30 2 * * *",
            &tz,
            utc_at(2026, 3, 7, 12, 0),
            utc_at(2026, 3, 9, 12, 0),
        );
        assert_eq!(
            ticks,
            [utc_at(2026, 3, 8, 7, 30), utc_at(2026, 3, 9, 6, 30)],
        );
    }

    #[test]
    fn schedule_survives_a_zone_whose_local_midnight_does_not_exist() {
        let tz = dst::Zone(&dst::MIDNIGHT_2026);
        // Sep 6 has no 00:00; a daily-at-midnight and a noon schedule must
        // not panic and must still fire once per day.
        let ticks = ticks_between(
            "@daily",
            &tz,
            utc_at(2026, 9, 5, 12, 0),
            utc_at(2026, 9, 7, 12, 0),
        );
        assert_eq!(ticks.len(), 2, "{ticks:?}");
        assert_eq!(ticks[0], utc_at(2026, 9, 6, 4, 0), "right after the gap");
        assert_eq!(ticks[1], utc_at(2026, 9, 7, 3, 0));
        let noon = ticks_between(
            "0 12 * * *",
            &tz,
            utc_at(2026, 9, 5, 18, 0),
            utc_at(2026, 9, 6, 18, 0),
        );
        assert_eq!(noon, [utc_at(2026, 9, 6, 15, 0)]);
        // The repeated 23:00 hour on Apr 4 fires once.
        let late = ticks_between(
            "30 23 * * *",
            &tz,
            utc_at(2026, 4, 4, 12, 0),
            utc_at(2026, 4, 5, 12, 0),
        );
        assert_eq!(late, [utc_at(2026, 4, 5, 2, 30)]);
    }

    /// A pid that is guaranteed dead: run `true` and wait for it to exit.
    fn dead_pid() -> u32 {
        let child = std::process::Command::new("true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut child = child;
        child.wait().unwrap();
        pid
    }

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "prx-pidguard-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn acquire_writes_pid_file_and_drop_removes_it() {
        let d = dir("basic");
        let guard = PidGuard::acquire(&d).unwrap();
        assert_eq!(guard.pid(), std::process::id());
        assert_eq!(read_pid(&d.join("daemon.pid")), Some(std::process::id()));
        drop(guard);
        assert!(!d.join("daemon.pid").exists());
    }

    #[test]
    fn acquire_fails_while_another_live_process_owns_the_file() {
        let d = dir("running");
        // A live foreign process: sleep briefly.
        let mut sleeper = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        let pid = sleeper.id();
        std::fs::write(d.join("daemon.pid"), format!("{pid}\n")).unwrap();
        let err = PidGuard::acquire(&d).unwrap_err();
        assert!(
            matches!(err, PidError::AlreadyRunning(p) if p == pid),
            "{err}"
        );
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
    }

    #[test]
    fn stale_pid_file_is_reclaimed() {
        let d = dir("stale");
        let dead = dead_pid();
        std::fs::write(d.join("daemon.pid"), format!("{dead}\n")).unwrap();
        let guard = PidGuard::acquire(&d).unwrap();
        assert_eq!(read_pid(&d.join("daemon.pid")), Some(std::process::id()));
        drop(guard);
    }

    #[test]
    fn unreadable_pid_file_is_reclaimed() {
        let d = dir("junk");
        std::fs::write(d.join("daemon.pid"), "not a pid at all").unwrap();
        let guard = PidGuard::acquire(&d).unwrap();
        assert_eq!(read_pid(&d.join("daemon.pid")), Some(std::process::id()));
        drop(guard);
    }

    #[test]
    fn drop_does_not_remove_a_successors_pid_file() {
        let d = dir("successor");
        let guard = PidGuard::acquire(&d).unwrap();
        // Simulate a successor overwriting the file before our drop.
        let mut sleeper = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        std::fs::write(d.join("daemon.pid"), format!("{}\n", sleeper.id())).unwrap();
        drop(guard);
        assert_eq!(read_pid(&d.join("daemon.pid")), Some(sleeper.id()));
        sleeper.kill().unwrap();
        sleeper.wait().unwrap();
    }

    #[test]
    fn newest_log_file_prefers_lexically_largest_date() {
        let d = dir("logs");
        assert_eq!(newest_log_file(&d), None);
        std::fs::write(d.join("prx.log.2026-09-01"), b"old").unwrap();
        std::fs::write(d.join("prx.log.2026-09-12"), b"new").unwrap();
        std::fs::write(d.join("prx.log.2026-09-12.tmp"), b"junk").unwrap();
        assert_eq!(newest_log_file(&d), Some(d.join("prx.log.2026-09-12")));
    }

    // ---------------------------------------- banner dedup (D3)

    /// `CommandNotifier` shells out, so delivery is only observable through
    /// a recording implementation.
    type Delivered = Arc<std::sync::Mutex<Vec<String>>>;

    #[derive(Default)]
    struct RecordingNotifier(Delivered);

    impl Notifier for RecordingNotifier {
        async fn notify(
            &self,
            title: &str,
            body: &str,
            _behavior: &TierBehavior,
        ) -> Result<(), NotifyError> {
            self.0.lock().unwrap().push(format!("{title}: {body}"));
            Ok(())
        }
    }

    fn deferring_notifier(
        liveness: &Arc<MenubarLiveness>,
        delivered: &Delivered,
    ) -> MenubarDeferringNotifier<RecordingNotifier> {
        MenubarDeferringNotifier {
            inner: RecordingNotifier(delivered.clone()),
            liveness: liveness.clone(),
        }
    }

    fn banner_behavior() -> TierBehavior {
        TierBehavior {
            banner: true,
            sound: None,
        }
    }

    #[tokio::test]
    async fn banner_suppressed_while_menubar_recently_seen() {
        let liveness = Arc::new(MenubarLiveness::default());
        let delivered: Delivered = Default::default();
        let notifier = deferring_notifier(&liveness, &delivered);

        liveness.note_seen();
        notifier
            .notify("prx", "org/repo#1 review passed", &banner_behavior())
            .await
            .unwrap();
        assert!(
            delivered.lock().unwrap().is_empty(),
            "a fresh menubar ping must suppress the daemon banner"
        );
    }

    #[tokio::test]
    async fn banner_delivered_once_the_liveness_window_expires() {
        let liveness = Arc::new(MenubarLiveness::default());
        let delivered: Delivered = Default::default();
        let notifier = deferring_notifier(&liveness, &delivered);

        // Backdate the announcement past MENUBAR_LIVENESS_WINDOW.
        let stale = Instant::now()
            .checked_sub(MENUBAR_LIVENESS_WINDOW + Duration::from_secs(1))
            .unwrap();
        *liveness.0.lock().unwrap() = Some(stale);
        notifier
            .notify("prx", "org/repo#1 review passed", &banner_behavior())
            .await
            .unwrap();
        assert_eq!(
            delivered.lock().unwrap().len(),
            1,
            "an expired menubar ping must not suppress the daemon banner"
        );
    }

    #[tokio::test]
    async fn banner_delivered_when_no_menubar_was_ever_seen() {
        let liveness = Arc::new(MenubarLiveness::default());
        let delivered: Delivered = Default::default();
        let notifier = deferring_notifier(&liveness, &delivered);

        notifier
            .notify("prx", "org/repo#1 review passed", &banner_behavior())
            .await
            .unwrap();
        assert_eq!(
            delivered.lock().unwrap().len(),
            1,
            "without any client announcement the daemon banners as before"
        );
    }
}
