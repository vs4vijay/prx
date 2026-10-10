//! Localhost IPC API (axum) and the thin client used by the CLI (A4: ALL
//! state + GitHub writes flow through the daemon). The server binds
//! `127.0.0.1:0`, publishes the port to `daemon.port` and a fresh 32-byte
//! token to `daemon.token` (0600) on every start; every route requires
//! `Authorization: Bearer <token>`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, HeaderName, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::TokioExecutor;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;

use crate::config::{
    self, Config, ConfigError, EffectiveRepo, EffectiveWithSources, Identity, MenuSettings,
    RepoConfig, ReviewMode,
};
use crate::daemon::{BriefRequestError, ConfigWriteError, Job, LiveConfig, Shared};
use crate::effects::EffectsDeps;
use crate::github::{Gh, GhCli, GhError, PrLifecycle};
use crate::history::{self, HistoryEntry};
use crate::machine::{self, Event, MachineCtx, PrSnapshot};
use crate::pause::{self, EnqueueOrigin};
use crate::review::{PrRef, ReviewOverrides};
use crate::state::{PrRecord, PrState, SkipReason};
use crate::status::{self, StatusEntry};

/// Client request timeout (reviews are enqueued, not awaited; approve may
/// take a couple of seconds to shell out to gh).
const CLIENT_TIMEOUT: Duration = Duration::from_secs(30);

/// D3 (banner dedup): client self-identification. Every [`DaemonClient`]
/// request carries `X-Prx-Client: menubar/<version>`; the daemon's
/// `auth_middleware` records the last-seen instant so the notifier can defer
/// desktop banners while a menubar client is live (90 s ≈ 9 missed polls).
const CLIENT_HEADER: HeaderName = HeaderName::from_static("x-prx-client");
const CLIENT_ID: &str = concat!("menubar/", env!("CARGO_PKG_VERSION"));

// ------------------------------------------------------------------ server

/// Random 32-byte hex token (64 chars) for the IPC API (A4).
pub fn generate_token() -> String {
    use rand::RngExt;
    let mut buf = [0u8; 32];
    rand::rng().fill(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Bind the IPC listener and publish `daemon.port` + `daemon.token`.
pub async fn bind(shared: &Arc<Shared>) -> Result<(tokio::net::TcpListener, u16)> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("cannot bind the IPC listener")?;
    let port = listener.local_addr()?.port();
    std::fs::write(crate::paths::daemon_port_file(), port.to_string())
        .context("cannot write daemon.port")?;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(crate::paths::daemon_token_file())
        .context("cannot write daemon.token")?;
    file.write_all(shared.token.as_bytes())
        .context("cannot write daemon.token")?;
    Ok((listener, port))
}

/// Serve the API until the shutdown watch fires, then remove the discovery
/// files so clients fall back to in-process execution immediately (A4).
pub async fn serve(
    listener: tokio::net::TcpListener,
    shared: Arc<Shared>,
    shutdown: watch::Receiver<bool>,
) {
    let app = build_router(shared);
    let mut shutdown = shutdown;
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        let _ = shutdown.changed().await;
    });
    if let Err(e) = server.await {
        tracing::error!("IPC server error: {e}");
    }
    let _ = std::fs::remove_file(crate::paths::daemon_port_file());
    let _ = std::fs::remove_file(crate::paths::daemon_token_file());
}

// ------------------------------------------------------------- wire types

/// `GET /status` payload. `prs` uses the exact `prx status --json`
/// serialization (`StatusEntry`) so the two outputs cannot drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub version: String,
    pub pid: u32,
    pub uptime_seconds: u64,
    pub in_flight: usize,
    pub queue_depth: usize,
    /// Last successful poll per repo (scheduler view).
    pub last_poll: BTreeMap<String, DateTime<Utc>>,
    /// Global `state.json` last_poll.
    pub last_state_poll: Option<DateTime<Utc>>,
    /// The soonest scheduled repo poll (the menu's "next check", task 7.7);
    /// absent before the scheduler has planned one (and from older
    /// daemons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_poll: Option<DateTime<Utc>>,
    pub prs: Vec<StatusEntry>,
    /// D2: set while the config file on disk failed to load and the daemon
    /// keeps running on its last-good config; absent when healthy (and from
    /// older daemons).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<ConfigErrorInfo>,
    /// D3/D8: the global review mode (`defaults.review_mode`; per-org/repo
    /// overrides are not reflected here). Older daemons omit the field,
    /// which reads as `review comment` — their only behavior.
    #[serde(default)]
    pub review_mode: ReviewMode,
    /// D5: end of the daemon-wide review pause while one is in effect;
    /// absent when not paused (and from older daemons). Independent of
    /// `review_mode`: a pause keeps polling, mode `off` does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<DateTime<Utc>>,
    /// D6: sorted names of every resolvable engine (built-ins plus
    /// `engines:` entries). Empty from older daemons.
    #[serde(default)]
    pub engines: Vec<String>,
    /// D6/D7: the global menu ordering and grouping. Older daemons omit it,
    /// which reads as the defaults.
    #[serde(default)]
    pub menu: MenuSettings,
}

/// D2: a rejected config reload — the parse/validate error and when the
/// daemon saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigErrorInfo {
    pub message: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResponse {
    pub enqueued: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `POST /review-mode` answer: [`MutationResponse`] plus whether the mode
/// was written to the config file. `persisted: false` means the file has
/// errors, so the mode was applied to the running (last-good) config only
/// and is lost on restart or when the fixed file is picked up (`note` says
/// so for display).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewModeResponse {
    pub ok: bool,
    pub changed: bool,
    #[serde(default = "default_true")]
    pub persisted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutationResponse {
    pub ok: bool,
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `GET /config` payload (design D5): the running config as written
/// (`raw`, for form population), the file's current text (`yaml`, for the
/// raw view — it can differ from `raw` while `config_error` is set), and
/// the merged per-repo/per-org settings with each value's source for
/// inheritance display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigResponse {
    pub path: Option<PathBuf>,
    pub yaml: Option<String>,
    pub raw: Config,
    pub effective: EffectiveConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<ConfigErrorInfo>,
    /// The shipped default review prompt text (design F5), so the form can
    /// show it where `review_prompt` is unset or `default`.
    #[serde(default)]
    pub shipped_review_prompt: String,
    /// The prx output contract (design C1/C2) appended to every review
    /// prompt, with a sample manifest path: the prompt editor previews it
    /// read-only, from the same source agents receive.
    #[serde(default)]
    pub review_contract: String,
    /// The built-in text of the other message templates, keyed by config
    /// field (`review_heading`, `identity.attribution_footer`), so the form
    /// can show what is posted while they are unset.
    #[serde(default)]
    pub shipped_templates: BTreeMap<String, String>,
}

/// The merged view inside [`ConfigResponse`]: `defaults` as every repo
/// sees them before overrides, each org entry at org level (defaults ←
/// org), and each watched repo (defaults ← org ← repo).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveConfig {
    pub defaults: EffectiveRepo,
    pub orgs: Vec<EffectiveWithSources>,
    pub repos: Vec<EffectiveWithSources>,
}

/// `PUT /config` body: exactly one of a structured config (the settings
/// form, JSON in the config's own shape) or the full YAML text (the raw
/// view; written verbatim, comments included).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigWriteRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaml: Option<String>,
}

/// A successful `PUT /config`: persisted and hot-applied; the repo-set
/// reconcile summary says what changed in the watched set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigWriteResponse {
    pub ok: bool,
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub discarded: Vec<String>,
}

/// A rejected config write (HTTP 422): nothing was written or applied.
/// `field` is the offending field path when the error names one (e.g.
/// `defaults.poll_interval_minutes`), so the settings app can render the
/// message next to that input. [`DaemonClient`] returns it as the error
/// value (downcast with `anyhow::Error::downcast_ref::<ConfigRejection>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{error}")]
pub struct ConfigRejection {
    pub error: String,
    #[serde(default)]
    pub field: Option<String>,
}

/// `POST /pause` body (D5): exactly one of `hours` (> 0) or `until`
/// (RFC 3339, in the future). Both or neither is a 400.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PauseRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hours: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
}

/// `POST /pause` / `POST /resume` answer: [`MutationResponse`] plus the
/// pause end now in effect (`None` after a resume).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PauseResponse {
    pub ok: bool,
    pub changed: bool,
    #[serde(default)]
    pub paused_until: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `POST /review-mode` body: the new global (`defaults.review_mode`) mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewModeRequest {
    pub mode: ReviewMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub entries: Vec<HistoryEntry>,
}

/// `GET /insights` query parameters. `repo` and `days` are optional
/// (all repos, all time by default) — DEV-PLAN §9 lists the bare endpoint;
/// the CLI's `--repo`/`--days` flags map onto these.
#[derive(Debug, Deserialize)]
pub struct InsightsQuery {
    pub repo: Option<String>,
    pub days: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    error: String,
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

// ------------------------------------------------------------------ routes

fn build_router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/status", get(status_handler))
        .route("/review", post(review_handler))
        .route("/brief", post(brief_handler))
        .route("/approve", post(approve_handler))
        .route("/snooze", post(snooze_handler))
        .route("/ignore", post(ignore_handler))
        .route("/unignore", post(unignore_handler))
        .route("/unsnooze", post(unsnooze_handler))
        .route("/review-now", post(review_now_handler))
        .route("/pause", post(pause_handler))
        .route("/resume", post(resume_handler))
        .route("/config", get(get_config_handler).put(put_config_handler))
        .route("/review-mode", post(review_mode_handler))
        .route("/block", post(block_handler))
        .route("/unblock", post(unblock_handler))
        .route("/history", get(history_handler))
        .route("/insights", get(insights_handler))
        .route("/usage", get(usage_handler))
        .route("/discover/agents", get(discover_agents_handler))
        .route("/discover/repos", get(discover_repos_handler))
        .layer(middleware::from_fn_with_state(
            shared.clone(),
            auth_middleware,
        ))
        .with_state(shared)
}

async fn auth_middleware(
    State(shared): State<Arc<Shared>>,
    headers: HeaderMap,
    req: Request,
    next: Next,
) -> Response {
    // Localhost-only API; a plain equality check is sufficient.
    let expected = format!("Bearer {}", shared.token);
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if provided == Some(expected.as_str()) {
        // D3: an authenticated request announcing a client (header presence
        // is the whole signal) marks the menubar as alive for banner dedup.
        if headers.contains_key(CLIENT_HEADER) {
            shared.note_menubar_seen();
        }
        next.run(req).await
    } else {
        error_response(StatusCode::UNAUTHORIZED, "missing or invalid bearer token")
    }
}

async fn status_handler(State(shared): State<Arc<Shared>>) -> Response {
    // Served from the read cache: /status must stay responsive while a
    // review holds the state lock.
    let state = shared.snapshot();
    let live = shared.live();
    let mut prs = status::render_entries(&state, &live.repos, Utc::now());
    // D12: without a configured login, the identity is the gh account a
    // Block/Unblock resolved (unknown until then — the menu offers Block).
    for entry in prs.iter_mut().filter(|e| e.identity_login.is_none()) {
        let repo_name = entry.key.rsplit_once('#').map_or("", |(repo, _)| repo);
        let credential = live
            .repo(repo_name)
            .and_then(|r| r.identity.credential.as_deref());
        entry.identity_login = shared.cached_gh_login(credential);
    }
    // Design B4: a running brief and the last failure are runtime-only.
    for entry in prs.iter_mut() {
        entry.brief_pending = shared.brief_pending(&entry.key);
        entry.brief_error = shared.brief_error(&entry.key);
    }
    let (queue_depth, in_flight) = shared.queue_stats().await;
    Json(StatusResponse {
        version: env!("CARGO_PKG_VERSION").to_string(),
        pid: shared.pid,
        uptime_seconds: shared.started_at.elapsed().as_secs(),
        in_flight,
        queue_depth,
        last_poll: shared.last_poll_map().await,
        last_state_poll: state.last_poll,
        next_poll: shared.next_poll_at().await,
        prs,
        config_error: shared.config_error(),
        review_mode: live.config.defaults.review_mode,
        paused_until: state.active_pause(Utc::now()),
        engines: live.config.engine_names(),
        menu: live.config.menu,
    })
    .into_response()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewRequest {
    /// A PR reference (`org/repo#N`, a GitHub PR URL, `repo#N`, `N`, `#N`;
    /// design D5) or `all`.
    pub key: String,
    #[serde(default)]
    pub force: bool,
    /// D10: instructions appended to the rendered prompt for this run only
    /// (never persisted). Blank is the same as absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_prompt: Option<String>,
    /// D1: run this configured engine instead of the repo's, for this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// D1: `false` reviews without GitHub writes (like `monitor`), for this
    /// run; absent lets the repo's review mode decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<bool>,
}

impl ReviewRequest {
    /// The per-run options this request carries (normalized: blank text is
    /// absent).
    pub fn overrides(&self) -> ReviewOverrides {
        ReviewOverrides {
            engine: self.engine.clone(),
            post: self.post,
            extra_prompt: self.extra_prompt.clone(),
        }
        .normalized()
    }
}

/// A22: forced jobs bypass cooldowns (but not skip/terminal legality — the
/// pipeline re-validates on execution); non-force enqueues only due items.
/// D3: nothing is enqueued, forced or not, for a repo in mode `off`.
/// D5: a single target is resolved against the watched repos (every gh
/// call before the state lock), and an untracked open PR is tracked on
/// demand exactly as a poll would, then enqueued like a tracked one.
async fn review_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<ReviewRequest>,
) -> Response {
    let now = Utc::now();
    let overrides = req.overrides();
    let live = shared.live();
    // D2: an unknown engine is rejected before anything is enqueued.
    if let Err(e) = overrides.validate_engine(&live.config) {
        return error_response(StatusCode::BAD_REQUEST, format!("{e:#}"));
    }
    if req.key.trim() == "all" {
        let snapshot = shared.snapshot();
        // D5: `review --all` is an automatic origin — paused, it enqueues
        // nothing (explicit single-PR reviews still run).
        if let Some(until) = snapshot.active_pause(now) {
            return Json(ReviewResponse {
                enqueued: vec![],
                note: Some(format!(
                    "{}; nothing was enqueued (review a single PR explicitly, or `prx resume`)",
                    pause::describe(until, now)
                )),
            })
            .into_response();
        }
        let mut jobs = Vec::new();
        for repo in live.repos.iter().filter(|repo| {
            pause::enqueue_allowed(&snapshot, repo.review_mode, EnqueueOrigin::ReviewAll, now)
        }) {
            for (key, rec) in &snapshot.prs {
                if rec.repo != repo.name {
                    continue;
                }
                let eligible = if req.force {
                    is_reviewable(rec)
                } else {
                    machine::is_review_due(rec, &repo.cooldown, now)
                };
                if eligible {
                    jobs.push(Job {
                        key: key.clone(),
                        repo: repo.name.clone(),
                        due: now,
                        force: req.force,
                        overrides: overrides.clone(),
                    });
                }
            }
        }
        let enqueued = shared.enqueue(jobs).await;
        let note = (enqueued.is_empty())
            .then(|| "nothing to enqueue (not due, or already queued/in flight)".to_string());
        return Json(ReviewResponse { enqueued, note }).into_response();
    }

    let target = match resolve_target(&shared, &live, &req.key).await {
        Ok(target) => target,
        Err((status, message)) => return error_response(status, message),
    };
    let (repo_name, number) = (target.repo, target.number);
    let Some(repo) = live.repo(&repo_name) else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("repo '{repo_name}' is not watched by this daemon"),
        );
    };
    let key = PrRecord::key_for(&repo_name, number);
    if repo.review_mode == ReviewMode::Off {
        return Json(ReviewResponse {
            enqueued: vec![],
            note: Some(format!(
                "review mode is off for {repo_name}; {key} was not enqueued"
            )),
        })
        .into_response();
    }
    if !shared.snapshot().prs.contains_key(&key) {
        // D5: validate against GitHub (outside the state lock), then track.
        let snapshot = match target.fetched {
            Some(snapshot) => snapshot,
            None => match fetch_open_pr(&shared, &repo_name, number).await {
                Ok(snapshot) => snapshot,
                Err((status, message)) => return error_response(status, message),
            },
        };
        track_on_demand(&shared, &live, repo, &key, snapshot).await;
    }
    let snapshot = shared.snapshot();
    let Some(rec) = snapshot.prs.get(&key) else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{key} could not be tracked"),
        );
    };
    let eligible = if req.force {
        is_reviewable(rec)
    } else {
        machine::is_review_due(rec, &repo.cooldown, now)
    };
    if !eligible {
        return Json(ReviewResponse {
            enqueued: vec![],
            note: Some(not_eligible_note(&key, rec)),
        })
        .into_response();
    }
    let enqueued = shared
        .enqueue(vec![Job {
            key: key.clone(),
            repo: repo_name,
            due: now,
            force: req.force,
            overrides,
        }])
        .await;
    let mut notes = Vec::new();
    if enqueued.is_empty() {
        notes.push(if shared.is_in_flight(&key).await {
            format!("a review of {key} is already running; this request was not queued")
        } else {
            format!("{key} is already queued for review")
        });
    }
    // D5: an explicit single-PR job bypasses the pause, with a note.
    if let Some(until) = snapshot.active_pause(now) {
        notes.push(format!(
            "{}; this explicit review runs anyway",
            pause::describe(until, now)
        ));
    }
    let note = (!notes.is_empty()).then(|| notes.join("; "));
    Json(ReviewResponse { note, enqueued }).into_response()
}

/// A resolved single-PR target (design D5). `fetched` carries the PR's
/// open snapshot when resolution already fetched it from GitHub.
struct ResolvedTarget {
    repo: String,
    number: u64,
    fetched: Option<PrSnapshot>,
}

/// Resolve a PR reference against the watched repos (design D5). A bare
/// number checks every watched repo whose mode is not `off`: a tracked,
/// non-terminal key counts without a gh call; the others are fetched
/// concurrently, and only open PRs count. Errors carry the HTTP status.
async fn resolve_target(
    shared: &Arc<Shared>,
    live: &LiveConfig,
    reference: &str,
) -> Result<ResolvedTarget, (StatusCode, String)> {
    let parsed = crate::review::parse_pr_ref(reference)
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let resolved = |repo: &str, number| ResolvedTarget {
        repo: repo.to_string(),
        number,
        fetched: None,
    };
    let ambiguous = |what: String, mut keys: Vec<String>| {
        keys.sort();
        (
            StatusCode::CONFLICT,
            format!(
                "{what} is ambiguous; it matches {} — use org/repo#N",
                keys.join(", ")
            ),
        )
    };
    match parsed {
        PrRef::Full(repo, number) => match live.repo(&repo) {
            Some(_) => Ok(resolved(&repo, number)),
            None => Err((
                StatusCode::NOT_FOUND,
                format!("repo '{repo}' is not watched by this daemon"),
            )),
        },
        PrRef::Short(name, number) => {
            let matches: Vec<&EffectiveRepo> = live
                .repos
                .iter()
                .filter(|r| r.name.rsplit_once('/').is_some_and(|(_, n)| n == name))
                .collect();
            match matches[..] {
                [repo] => Ok(resolved(&repo.name, number)),
                [] => Err((
                    StatusCode::NOT_FOUND,
                    format!("no watched repo is named '{name}'"),
                )),
                _ => Err(ambiguous(
                    format!("{name}#{number}"),
                    matches
                        .iter()
                        .map(|r| PrRecord::key_for(&r.name, number))
                        .collect(),
                )),
            }
        }
        PrRef::Bare(number) => {
            let snapshot = shared.snapshot();
            let mut found: Vec<ResolvedTarget> = Vec::new();
            let mut lookups = tokio::task::JoinSet::new();
            for repo in live
                .repos
                .iter()
                .filter(|r| r.review_mode != ReviewMode::Off)
            {
                let key = PrRecord::key_for(&repo.name, number);
                let tracked = snapshot
                    .prs
                    .get(&key)
                    .is_some_and(|rec| !matches!(rec.state, PrState::Merged | PrState::Closed));
                if tracked {
                    found.push(resolved(&repo.name, number));
                    continue;
                }
                let gh = shared.deps.gh.clone();
                let name = repo.name.clone();
                lookups.spawn(async move {
                    let fetched = gh.pr_snapshot(&name, number).await;
                    (name, fetched)
                });
            }
            while let Some(joined) = lookups.join_next().await {
                let Ok((name, fetched)) = joined else {
                    continue;
                };
                match fetched {
                    Ok((summary, PrLifecycle::Open)) => found.push(ResolvedTarget {
                        fetched: Some(PrSnapshot::from_summary(&name, &summary)),
                        repo: name,
                        number,
                    }),
                    Ok(_) => {}
                    Err(e) => tracing::debug!("{name}#{number}: not found ({e})"),
                }
            }
            match found.len() {
                1 => Ok(found.remove(0)),
                0 => Err((
                    StatusCode::NOT_FOUND,
                    format!("no watched repo has an open PR #{number}"),
                )),
                _ => Err(ambiguous(
                    format!("#{number}"),
                    found
                        .iter()
                        .map(|t| PrRecord::key_for(&t.repo, number))
                        .collect(),
                )),
            }
        }
    }
}

/// Fetch one untracked PR (outside the state lock, design D5) and require
/// it to be open: a missing PR is a 404, a merged/closed one a 409 naming
/// its state.
async fn fetch_open_pr(
    shared: &Arc<Shared>,
    repo: &str,
    number: u64,
) -> Result<PrSnapshot, (StatusCode, String)> {
    let key = PrRecord::key_for(repo, number);
    match shared.deps.gh.pr_snapshot(repo, number).await {
        Ok((summary, PrLifecycle::Open)) => Ok(PrSnapshot::from_summary(repo, &summary)),
        Ok((_, lifecycle)) => {
            let state = match lifecycle {
                PrLifecycle::Merged => "merged",
                _ => "closed",
            };
            Err((
                StatusCode::CONFLICT,
                format!("{key} is {state}; nothing to review"),
            ))
        }
        Err(GhError::CommandFailed { stderr, .. }) if stderr.contains("Could not resolve") => {
            Err((
                StatusCode::NOT_FOUND,
                format!("{key} does not exist on GitHub"),
            ))
        }
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            format!("cannot fetch {key} from GitHub: {e}"),
        )),
    }
}

/// Track a PR on demand (design D5): the poll's `PollNewPr` through the
/// machine under the state lock (skip rules, notifications, publish).
async fn track_on_demand(
    shared: &Arc<Shared>,
    live: &LiveConfig,
    repo: &EffectiveRepo,
    key: &str,
    snapshot: PrSnapshot,
) {
    let deps = EffectsDeps::for_repo(&shared.deps.notifier, &live.tiers, repo);
    let ctx = MachineCtx {
        now: Utc::now(),
        skip: &repo.skip,
        resume_sessions: repo.resolved_engine.can_resume(),
    };
    let mut state = shared.lock_state().await;
    status::apply_and_store(&mut state, key, Event::PollNewPr { snapshot }, &ctx, &deps).await;
    shared.publish(&state);
}

/// Why a tracked PR is not enqueued: its state, plus the skip reason.
fn not_eligible_note(key: &str, rec: &PrRecord) -> String {
    let reason = rec
        .skip_reason
        .map(|reason| {
            let reason = match reason {
                SkipReason::Draft => "draft",
                SkipReason::Config => "config rules",
                SkipReason::Snoozed => "snoozed",
                SkipReason::Ignored => "ignored",
            };
            format!(", reason {reason}")
        })
        .unwrap_or_default();
    format!(
        "{key} is not eligible for review right now (state {:?}{reason})",
        rec.state
    )
}

/// The pipeline's own legality check: pending or already reviewed (a force
/// re-review is allowed; skipped/terminal/in-flight states are not).
fn is_reviewable(rec: &PrRecord) -> bool {
    matches!(
        rec.state,
        PrState::Pending | PrState::ReviewedApproved | PrState::ReviewedChangesRequested
    )
}

#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    pub key: String,
}

/// The human-only approve path (A1/D2), executed on the handler task (a
/// single quick gh call). "Already approved" is a graceful no-op.
///
/// After the gh write succeeds (or is a no-op), the attention state is
/// routed through the machine like the other manual mutations: approving a
/// `reviewed_changes_requested` PR must take it out of the attention set
/// (`reviewed_approved`, menu-bar-shell "Approve from the menu bar").
/// Approving untracked PRs / unwatched repos keeps working: the machine has
/// nothing to record, so the plain gh answer is returned unchanged.
async fn approve_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<ApproveRequest>,
) -> Response {
    let (repo_name, number) = match crate::review::parse_pr_key(&req.key) {
        Ok(parsed) => parsed,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    let live = shared.live();
    let (identity, _) = identity_for(&live, &repo_name);
    let message = approve_message_for(&live, &repo_name);
    let gh = shared
        .deps
        .gh
        .for_credential(identity.credential.as_deref());
    let fresh = match gh.pr_approve(&repo_name, number, message.as_deref()).await {
        Ok(()) => true,
        Err(GhError::CommandFailed { stderr, .. }) if stderr.contains("already approved") => false,
        Err(e) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                format!("approve failed for {repo_name}#{number}: {e}"),
            )
        }
    };
    // What approve answered before the machine existed: ok/changed=true for
    // a fresh gh approval, ok/changed=false + note for the no-op.
    let plain = MutationResponse {
        ok: true,
        changed: fresh,
        note: (!fresh).then(|| "already approved — nothing to do".to_string()),
    };
    let key = PrRecord::key_for(&repo_name, number);
    let applied = match try_apply_manual_event(&shared, &req.key, Event::Approve).await {
        Ok(applied) => applied,
        // Untracked PR / unwatched repo: the gh write already happened, so
        // the plain answer stands.
        Err(_) => return Json(plain).into_response(),
    };
    let (Some(before), Some(after)) = (applied.before, applied.after) else {
        return Json(plain).into_response();
    };
    if before.state == after.state {
        // No attention-state move (already reviewed_approved, or the machine
        // declined in_review/skipped/terminal): keep the plain answer.
        return Json(plain).into_response();
    }
    Json(MutationResponse {
        ok: true,
        changed: true,
        note: Some(format!("{key}: approval posted; state → {:?}", after.state)),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SnoozeRequest {
    pub key: String,
    #[serde(default)]
    pub hours: Option<f64>,
    #[serde(default)]
    pub until_push: bool,
}

async fn snooze_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<SnoozeRequest>,
) -> Response {
    if req.hours.is_some_and(|h| h <= 0.0) {
        return error_response(StatusCode::BAD_REQUEST, "--hours must be > 0");
    }
    if req.hours.is_none() && !req.until_push {
        return error_response(
            StatusCode::BAD_REQUEST,
            "specify hours or until_push (a snooze needs a schedule)",
        );
    }
    let until = req
        .hours
        .map(|h| Utc::now() + chrono::Duration::milliseconds((h * 3_600_000.0) as i64));
    apply_manual_event(
        &shared,
        &req.key,
        Event::Snooze {
            until,
            until_push: req.until_push,
        },
    )
    .await
}

async fn ignore_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<KeyRequest>,
) -> Response {
    apply_manual_event(&shared, &req.key, Event::Ignore).await
}

async fn unignore_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<KeyRequest>,
) -> Response {
    apply_manual_event(&shared, &req.key, Event::Unignore).await
}

async fn unsnooze_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<KeyRequest>,
) -> Response {
    apply_manual_event(&shared, &req.key, Event::Unsnooze).await
}

#[derive(Debug, Deserialize)]
pub struct KeyRequest {
    pub key: String,
}

/// `POST /brief` acceptance (design B4): the brief for `key` is being
/// generated in the background; `/status` shows `brief_pending` until it
/// lands (or `brief_error` when it fails).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BriefResponse {
    pub accepted: bool,
    pub key: String,
}

/// `POST /brief` (design B2/B4): generate or regenerate a PR's brief.
/// `key` takes the same references as `/review`. Replies 202 when
/// accepted, 404 for an unknown PR, 409 while a brief for it is already
/// running, and 422 for a repo whose mode is `off` (or with briefs off).
async fn brief_handler(State(shared): State<Arc<Shared>>, Json(req): Json<KeyRequest>) -> Response {
    let live = shared.live();
    let target = match resolve_target(&shared, &live, &req.key).await {
        Ok(target) => target,
        Err((status, message)) => return error_response(status, message),
    };
    let key = PrRecord::key_for(&target.repo, target.number);
    match shared.request_brief(&target.repo, target.number) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(BriefResponse {
                accepted: true,
                key,
            }),
        )
            .into_response(),
        Err(e) => {
            let status = match e {
                BriefRequestError::UnknownPr(_) => StatusCode::NOT_FOUND,
                BriefRequestError::InProgress(_) => StatusCode::CONFLICT,
                BriefRequestError::RepoOff(_) | BriefRequestError::Disabled(_) => {
                    StatusCode::UNPROCESSABLE_ENTITY
                }
            };
            error_response(status, e.to_string())
        }
    }
}

/// The records around one manual event application: `after` is `None` when
/// the key was not tracked (the caller decides what that means — approve
/// must keep working for untracked PRs, the other mutations 404).
struct ManualApply {
    key: String,
    before: Option<PrRecord>,
    after: Option<PrRecord>,
}

/// Why a manual event could not be applied at all (before any machine
/// transition): the target key is malformed or its repo is not watched.
/// Rendered per-command; approve treats both as "nothing tracked" and keeps
/// the plain gh answer.
enum ManualEventError {
    BadKey(String),
    UnwatchedRepo(String),
}

/// Apply a manual machine event to the state truth, execute effects, and
/// persist (A4: all state writes flow through the daemon's state lock).
async fn try_apply_manual_event(
    shared: &Arc<Shared>,
    target: &str,
    event: Event,
) -> Result<ManualApply, ManualEventError> {
    let (repo_name, number) = crate::review::parse_pr_key(target)
        .map_err(|e| ManualEventError::BadKey(format!("{e:#}")))?;
    let live = shared.live();
    let repo = live
        .repo(&repo_name)
        .ok_or_else(|| ManualEventError::UnwatchedRepo(repo_name.clone()))?;
    let key = PrRecord::key_for(&repo_name, number);
    let deps = EffectsDeps::for_repo(&shared.deps.notifier, &live.tiers, repo);
    let skip = repo.skip.clone();
    let ctx = MachineCtx {
        now: Utc::now(),
        skip: &skip,
        resume_sessions: repo.resolved_engine.can_resume(),
    };
    let (before, after) = {
        let mut state = shared.lock_state().await;
        let before = state.prs.get(&key).cloned();
        status::apply_and_store(&mut state, &key, event, &ctx, &deps).await;
        let after = state.prs.get(&key).cloned();
        shared.publish(&state);
        (before, after)
    };
    Ok(ManualApply { key, before, after })
}

/// Apply a manual machine event (Snooze/Ignore/Unignore/Unsnooze) to the
/// state truth, execute effects, persist, and report whether anything
/// changed. All state writes flow through here (A4).
async fn apply_manual_event(shared: &Arc<Shared>, target: &str, event: Event) -> Response {
    let event_tag = match &event {
        Event::Snooze { .. } => "snooze",
        Event::Ignore => "ignore",
        Event::Unignore => "unignore",
        Event::Unsnooze => "unsnooze",
        _ => "other",
    };
    let applied = match try_apply_manual_event(shared, target, event).await {
        Ok(applied) => applied,
        Err(ManualEventError::BadKey(msg)) => return error_response(StatusCode::BAD_REQUEST, msg),
        Err(ManualEventError::UnwatchedRepo(repo)) => {
            return error_response(
                StatusCode::NOT_FOUND,
                format!("repo '{repo}' is not watched by this daemon"),
            )
        }
    };
    let Some(after) = applied.after else {
        return error_response(
            StatusCode::NOT_FOUND,
            format!("{} is not tracked", applied.key),
        );
    };
    if applied.before == Some(after.clone()) {
        return Json(MutationResponse {
            ok: true,
            changed: false,
            note: Some(format!(
                "{}: no change (current state {:?})",
                applied.key, after.state
            )),
        })
        .into_response();
    }
    let note = match (event_tag, after.skip_reason) {
        ("snooze", Some(SkipReason::Snoozed)) => Some(snooze_note(&after)),
        ("ignore", Some(SkipReason::Ignored)) => Some(format!("{} ignored", applied.key)),
        ("unignore", _) => Some(format!(
            "{} un-ignored (pending, cooldown restarted)",
            applied.key
        )),
        // The unsnooze restore names the state the PR returned to (D1) —
        // pending / reviewed-approved / reviewed-changes-requested.
        ("unsnooze", _) => Some(format!(
            "{} un-snoozed (restored to {:?})",
            applied.key, after.state
        )),
        _ => None,
    };
    Json(MutationResponse {
        ok: true,
        changed: true,
        note,
    })
    .into_response()
}

fn snooze_note(rec: &PrRecord) -> String {
    let key = rec.key();
    match (rec.snooze_until, rec.snooze_until_push) {
        (Some(until), _) => format!("{key} snoozed until {}", until.format("%Y-%m-%d %H:%M UTC")),
        (None, true) => format!("{key} snoozed until next push"),
        _ => format!("{key} snoozed"),
    }
}

// ------------------------------------------------------- config (D5)

/// `GET /config` (design D5).
async fn get_config_handler(State(shared): State<Arc<Shared>>) -> Response {
    let live = shared.live();
    let effective = match effective_config(&live) {
        Ok(effective) => effective,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot merge the running config: {e}"),
            )
        }
    };
    let path = shared.deps.config_path.clone();
    let yaml = path
        .as_deref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    Json(ConfigResponse {
        path,
        yaml,
        raw: live.config.clone(),
        effective,
        config_error: shared.config_error(),
        shipped_review_prompt: crate::prompt::DEFAULT_PROMPT.to_string(),
        review_contract: crate::prompt::contract_preview(),
        shipped_templates: BTreeMap::from([
            (
                "block_message".to_string(),
                crate::config::default_block_message(),
            ),
            (
                "review_heading".to_string(),
                crate::review::DEFAULT_REVIEW_HEADING.to_string(),
            ),
            (
                "identity.attribution_footer".to_string(),
                crate::review::DEFAULT_ATTRIBUTION_FOOTER
                    .trim_end()
                    .to_string(),
            ),
        ]),
    })
    .into_response()
}

fn effective_config(live: &LiveConfig) -> Result<EffectiveConfig, ConfigError> {
    Ok(EffectiveConfig {
        defaults: live.config.effective_defaults()?,
        orgs: live.config.effective_orgs_with_sources()?,
        repos: live.config.effective_repos_with_sources()?,
    })
}

/// 422 with the error and, when attributable, the offending field path.
fn rejection_response(error: &ConfigError) -> Response {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(ConfigRejection {
            error: error.to_string(),
            field: config::error_field(error),
        }),
    )
        .into_response()
}

fn config_write_error_response(error: ConfigWriteError) -> Response {
    match error {
        ConfigWriteError::Invalid(e) => rejection_response(&e),
        ConfigWriteError::NoConfigFile => error_response(
            StatusCode::CONFLICT,
            ConfigWriteError::NoConfigFile.to_string(),
        ),
        ConfigWriteError::Persist(e) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
    }
}

/// `PUT /config` (design D5): parse + validate the whole document first —
/// a rejection leaves the file and the running config untouched — then
/// write atomically (0600) and hot-apply through the reload path.
async fn put_config_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<ConfigWriteRequest>,
) -> Response {
    let label = shared
        .deps
        .config_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("config.yml"));
    let (parsed, text) = match (req.config, req.yaml) {
        (Some(value), None) => {
            // Through YAML so type errors name their field path exactly as
            // they do for the file (`defaults.poll_interval_minutes: ...`).
            let parsed = serde_yaml::to_string(&value)
                .map_err(|e| ConfigError::Parse {
                    path: label.clone(),
                    message: e.to_string(),
                })
                .and_then(|yaml| Config::parse(&yaml, &label));
            (parsed, None)
        }
        (None, Some(yaml)) => (Config::parse(&yaml, &label), Some(yaml)),
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "send exactly one of `config` (structured) or `yaml` (raw text)",
            )
        }
    };
    let config = match parsed {
        Ok(config) => config,
        Err(e) => return rejection_response(&e),
    };
    match shared.write_config(config, text).await {
        Ok(summary) => Json(ConfigWriteResponse {
            ok: true,
            added: summary.added,
            removed: summary.removed,
            discarded: summary.discarded,
        })
        .into_response(),
        Err(e) => config_write_error_response(e),
    }
}

/// `POST /review-mode` (design D5/D3): set `defaults.review_mode`, persist
/// it, and apply it now. The base is the config file as on disk (so an
/// external edit not yet picked up is kept). While that file fails to load
/// the user's broken edit is never overwritten: the mode is applied to the
/// running last-good config only and the answer says `persisted: false`
/// (lost on restart, or when the fixed file is picked up — the file wins).
async fn review_mode_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<ReviewModeRequest>,
) -> Response {
    let previous = shared.live().config.defaults.review_mode;
    let mut config = match shared.load_config_file() {
        Ok(config) => config,
        Err(ConfigWriteError::Invalid(e)) => {
            return review_mode_in_memory(&shared, req.mode, &e).await
        }
        Err(e) => return config_write_error_response(e),
    };
    let on_disk = config.defaults.review_mode;
    if previous == req.mode && on_disk == req.mode {
        return Json(ReviewModeResponse {
            ok: true,
            changed: false,
            persisted: true,
            note: Some(format!("review mode is already {}", mode_label(req.mode))),
        })
        .into_response();
    }
    config.defaults.review_mode = req.mode;
    match shared.write_config(config, None).await {
        Ok(_) => Json(ReviewModeResponse {
            ok: true,
            changed: true,
            persisted: true,
            note: Some(format!("review mode → {}", mode_label(req.mode))),
        })
        .into_response(),
        Err(e) => config_write_error_response(e),
    }
}

/// The broken-file branch of `POST /review-mode`: apply the mode to the
/// running last-good config without touching the file.
async fn review_mode_in_memory(
    shared: &Shared,
    mode: ReviewMode,
    file_error: &ConfigError,
) -> Response {
    let not_saved = format!(
        "not saved: the config file has errors ({file_error}); this mode is lost on restart \
         or when the fixed file is loaded"
    );
    let mut config = shared.live().config.clone();
    if config.defaults.review_mode == mode {
        return Json(ReviewModeResponse {
            ok: true,
            changed: false,
            persisted: false,
            note: Some(format!(
                "review mode is already {} ({not_saved})",
                mode_label(mode)
            )),
        })
        .into_response();
    }
    config.defaults.review_mode = mode;
    match shared.apply_config_unpersisted(config).await {
        Ok(_) => Json(ReviewModeResponse {
            ok: true,
            changed: true,
            persisted: false,
            note: Some(format!("review mode → {} ({not_saved})", mode_label(mode))),
        })
        .into_response(),
        Err(e) => config_write_error_response(ConfigWriteError::Invalid(e)),
    }
}

// ------------------------------------------------------------ discovery

/// `GET /discover/agents`: the engine candidates (design D1) — the known
/// agent CLIs and which of them this daemon can run (it is the process
/// that spawns them), plus the configured `llm_api` entries.
async fn discover_agents_handler(State(shared): State<Arc<Shared>>) -> Response {
    let configured = shared.live().config.engines.clone();
    let response = tokio::task::spawn_blocking(move || crate::discover::AgentsResponse {
        agents: crate::discover::detect_agents(),
        configured: crate::discover::resolve_configured(&configured),
        llm_apis: crate::discover::llm_api_candidates(&configured),
    })
    .await
    .unwrap_or_default();
    Json(response).into_response()
}

/// `GET /discover/repos`: repositories the gh account can see, for the
/// settings app's repo picker.
async fn discover_repos_handler(State(shared): State<Arc<Shared>>) -> Response {
    let credential = shared.live().config.defaults.identity.credential.clone();
    let gh = shared.deps.gh.for_credential(credential.as_deref());
    match gh.list_accessible_repos(crate::discover::REPO_PAGES).await {
        Ok(repos) => Json(crate::discover::ReposResponse { repos }).into_response(),
        Err(e) => error_response(
            StatusCode::BAD_GATEWAY,
            format!("could not list repositories: {e}"),
        ),
    }
}

fn mode_label(mode: ReviewMode) -> &'static str {
    match mode {
        ReviewMode::Off => "off",
        ReviewMode::Monitor => "monitor",
        ReviewMode::ReviewComment => "review comment",
    }
}

// ------------------------------------------------- block / unblock (D9)

/// The identity and block message in effect for `repo_name`: the watched
/// repo's merged settings, else the defaults merged with any matching org
/// (blocking a PR of an unwatched repo still posts as the right identity).
fn identity_for(live: &LiveConfig, repo_name: &str) -> (Identity, String) {
    if let Some(repo) = live.repo(repo_name) {
        return (repo.identity.clone(), repo.block_message.clone());
    }
    let pseudo = RepoConfig {
        name: repo_name.to_string(),
        ..RepoConfig::default()
    };
    match live.config.effective_repo(&pseudo) {
        Ok(repo) => (repo.identity, repo.block_message),
        Err(_) => (
            live.config.defaults.identity.clone(),
            live.config.defaults.block_message.clone(),
        ),
    }
}

/// The approve message in effect for `repo_name` (same resolution as
/// [`identity_for`]); `None` approves without a comment.
fn approve_message_for(live: &LiveConfig, repo_name: &str) -> Option<String> {
    if let Some(repo) = live.repo(repo_name) {
        return repo.approve_message.clone();
    }
    let pseudo = RepoConfig {
        name: repo_name.to_string(),
        ..RepoConfig::default()
    };
    match live.config.effective_repo(&pseudo) {
        Ok(repo) => repo.approve_message,
        Err(_) => live.config.defaults.approve_message.clone(),
    }
}

/// The login prx acts as: the configured `identity.login`, else the gh
/// account behind the identity's credential (resolved once, then cached).
async fn resolve_login(
    shared: &Shared,
    gh: &GhCli,
    identity: &Identity,
) -> Result<String, GhError> {
    if let Some(login) = &identity.login {
        return Ok(login.clone());
    }
    let credential = identity.credential.as_deref();
    if let Some(login) = shared.cached_gh_login(credential) {
        return Ok(login);
    }
    let login = gh.current_login().await?;
    shared.remember_gh_login(credential, &login);
    Ok(login)
}

/// `POST /block` (design D9): post a request-changes review with the
/// effective block message as the effective identity, then route
/// `Event::RequestChanges` through the machine like approve does (tracked
/// PRs → blocking state; untracked PRs post cleanly). A human action: not
/// gated by review mode.
async fn block_handler(State(shared): State<Arc<Shared>>, Json(req): Json<KeyRequest>) -> Response {
    let (repo_name, number) = match crate::review::parse_pr_key(&req.key) {
        Ok(parsed) => parsed,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    let key = PrRecord::key_for(&repo_name, number);
    let (identity, message) = identity_for(&shared.live(), &repo_name);
    let gh = shared
        .deps
        .gh
        .for_credential(identity.credential.as_deref());
    if let Err(e) = gh.pr_request_changes(&repo_name, number, &message).await {
        return error_response(
            StatusCode::BAD_GATEWAY,
            format!("block failed for {key}: {e}"),
        );
    }
    // Best effort: learn the login now so /status can offer Unblock.
    if let Err(e) = resolve_login(&shared, &gh, &identity).await {
        tracing::debug!("{key}: could not resolve the gh login after block: {e}");
    }
    let posted = format!("{key}: request-changes review posted");
    let note = match try_apply_manual_event(&shared, &req.key, Event::RequestChanges).await {
        Ok(ManualApply {
            before: Some(before),
            after: Some(after),
            ..
        }) if before.state != after.state => format!("{posted}; state → {:?}", after.state),
        _ => posted,
    };
    Json(MutationResponse {
        ok: true,
        changed: true,
        note: Some(note),
    })
    .into_response()
}

/// `POST /unblock` (design D9): dismiss the most recent request-changes
/// review authored by prx's identity, then route `Event::Unblock` through
/// the machine (a blocked tracked PR returns to pending right away, so the
/// menu offers Block again; untracked PRs dismiss cleanly). Nothing to
/// dismiss is a clean note, not an error, and leaves state unchanged. A
/// state poll is still requested to re-sync the rest from GitHub.
async fn unblock_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<KeyRequest>,
) -> Response {
    let (repo_name, number) = match crate::review::parse_pr_key(&req.key) {
        Ok(parsed) => parsed,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    let key = PrRecord::key_for(&repo_name, number);
    let failed = |e: GhError| {
        error_response(
            StatusCode::BAD_GATEWAY,
            format!("unblock failed for {key}: {e}"),
        )
    };
    let (identity, _) = identity_for(&shared.live(), &repo_name);
    let gh = shared
        .deps
        .gh
        .for_credential(identity.credential.as_deref());
    let login = match resolve_login(&shared, &gh, &identity).await {
        Ok(login) => login,
        Err(e) => return failed(e),
    };
    let reviews = match gh.list_reviews(&repo_name, number).await {
        Ok(reviews) => reviews,
        Err(e) => return failed(e),
    };
    let Some(review) = reviews
        .iter()
        .rev()
        .find(|r| r.state == "CHANGES_REQUESTED" && r.login.eq_ignore_ascii_case(&login))
    else {
        return Json(MutationResponse {
            ok: true,
            changed: false,
            note: Some(format!(
                "nothing to dismiss: no open request-changes review by @{login} on {key}"
            )),
        })
        .into_response();
    };
    if let Err(e) = gh
        .dismiss_review(&repo_name, number, review.id, "Unblocked from prx.")
        .await
    {
        return failed(e);
    }
    let dismissed = format!("{key}: dismissed @{login}'s request-changes review");
    let note = match try_apply_manual_event(&shared, &req.key, Event::Unblock).await {
        Ok(ManualApply {
            before: Some(before),
            after: Some(after),
            ..
        }) if before.state != after.state => format!("{dismissed}; state → {:?}", after.state),
        _ => dismissed,
    };
    // Re-sync the rest from GitHub on the next state poll (run it now).
    shared.poll_requested.notify_one();
    Json(MutationResponse {
        ok: true,
        changed: true,
        note: Some(note),
    })
    .into_response()
}

/// `POST /review-now`: wake the scheduler for a config reload check (D1)
/// followed by an immediate poll + due-scan of all repos. The work itself
/// runs on the scheduler task, never on the HTTP handler.
async fn review_now_handler(State(shared): State<Arc<Shared>>) -> Response {
    shared.poll_requested.notify_one();
    // D5: paused, the poll still runs but the due-scan enqueues nothing.
    let now = Utc::now();
    let note = match shared.snapshot().active_pause(now) {
        Some(until) => format!(
            "poll triggered; {} so nothing will be enqueued",
            pause::describe(until, now)
        ),
        None => "poll + due-scan triggered".to_string(),
    };
    Json(MutationResponse {
        ok: true,
        changed: true,
        note: Some(note),
    })
    .into_response()
}

/// `POST /pause` (D5): set the daemon-wide pause from `hours` or `until`
/// (exactly one). Persisted in the state file; polling continues.
async fn pause_handler(
    State(shared): State<Arc<Shared>>,
    Json(req): Json<PauseRequest>,
) -> Response {
    let now = Utc::now();
    let until = match pause::resolve_pause_until(req.hours, req.until, now) {
        Ok(until) => until,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    let changed = shared.pause_until(until).await;
    Json(PauseResponse {
        ok: true,
        changed,
        paused_until: Some(until),
        note: Some(pause::describe(until, now)),
    })
    .into_response()
}

/// `POST /resume` (D5): lift the pause; automatic enqueue resumes from the
/// next poll / tick (missed schedule fires are not replayed).
async fn resume_handler(State(shared): State<Arc<Shared>>) -> Response {
    let changed = shared.resume(Utc::now()).await;
    Json(PauseResponse {
        ok: true,
        changed,
        paused_until: None,
        note: Some(if changed {
            "reviews resumed".to_string()
        } else {
            "reviews were not paused".to_string()
        }),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    pub repo: Option<String>,
    pub limit: Option<usize>,
    /// Paging (the window's History page): only entries strictly older
    /// than this RFC 3339 timestamp.
    pub before: Option<String>,
}

async fn history_handler(
    State(shared): State<Arc<Shared>>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    let limit = q.limit.unwrap_or(100);
    // An empty `repo=` parameter means "all repos".
    let repo = q.repo.as_deref().filter(|r| !r.is_empty());
    let before = match q.before.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(raw) => match DateTime::parse_from_rfc3339(raw) {
            Ok(ts) => Some(ts.with_timezone(&Utc)),
            Err(e) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    format!("invalid before '{raw}': expected an RFC 3339 timestamp ({e})"),
                )
            }
        },
        None => None,
    };
    match history::query_page(&shared.deps.history_path, repo, before, limit) {
        Ok(entries) => Json(HistoryResponse { entries }).into_response(),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to read history: {e}"),
        ),
    }
}

/// `GET /insights` (DEV-PLAN §10): pure analytics over the history file.
/// The response body is the [`crate::insights::Insights`] struct itself —
/// self-describing (`generated_at`, `window_days`) so the menu bar client
/// needs no envelope.
async fn insights_handler(
    State(shared): State<Arc<Shared>>,
    Query(q): Query<InsightsQuery>,
) -> Response {
    // An empty `repo=` parameter means "all repos".
    let repo = q.repo.as_deref().filter(|r| !r.is_empty());
    let entries = match history::query(&shared.deps.history_path, repo, 0) {
        Ok(entries) => entries,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read history: {e}"),
            )
        }
    };
    let insights = crate::insights::compute(&entries, Utc::now(), q.days);
    Json(insights).into_response()
}

/// `GET /usage` query parameters (design D13), all optional; parsed by
/// [`crate::usage::Query::parse`] so invalid values answer 400 with the
/// allowed ones.
#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub bucket: Option<String>,
    pub window: Option<String>,
    pub group: Option<String>,
    pub repo: Option<String>,
}

/// `GET /usage` (design D13): token usage aggregated on demand from the
/// history file. The body is [`crate::usage::Usage`] itself.
async fn usage_handler(State(shared): State<Arc<Shared>>, Query(q): Query<UsageQuery>) -> Response {
    let query = match crate::usage::Query::parse(
        q.bucket.as_deref(),
        q.window.as_deref(),
        q.group.as_deref(),
        q.repo.as_deref(),
    ) {
        Ok(query) => query,
        Err(e) => return error_response(StatusCode::BAD_REQUEST, e),
    };
    let entries = match history::query(&shared.deps.history_path, query.repo.as_deref(), 0) {
        Ok(entries) => entries,
        Err(e) => {
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to read history: {e}"),
            )
        }
    };
    Json(crate::usage::compute(&entries, Utc::now(), &query)).into_response()
}

// ------------------------------------------------------------------ client

/// Thin HTTP client for the CLI (A4). Commands that mutate act through this
/// client when the daemon is up; otherwise the CLI falls back to in-process
/// execution.
#[derive(Clone)]
pub struct DaemonClient {
    base: String,
    token: String,
    http: Client<HttpConnector, Full<Bytes>>,
}

impl DaemonClient {
    /// Client pointing at an explicit base URL + token (tests use this to
    /// target an in-process daemon).
    pub fn with_base(base: impl Into<String>, token: impl Into<String>) -> DaemonClient {
        DaemonClient {
            base: base.into(),
            token: token.into(),
            http: Client::builder(TokioExecutor::new()).build_http(),
        }
    }

    /// Discover a live daemon from `daemon.port` + `daemon.token`. Returns
    /// None when the files are missing or the daemon does not answer —
    /// the CLI then falls back to in-process execution. Transport-level
    /// probe failures (e.g. the accept loop momentarily starved under
    /// load) are retried with a small backoff; an HTTP error answer from
    /// a live daemon (e.g. a stale port file) is not retried.
    pub async fn discover() -> Option<DaemonClient> {
        let port = std::fs::read_to_string(crate::paths::daemon_port_file())
            .ok()?
            .trim()
            .parse::<u16>()
            .ok()?;
        let token = std::fs::read_to_string(crate::paths::daemon_token_file())
            .ok()?
            .trim()
            .to_string();
        let client = DaemonClient::with_base(format!("http://127.0.0.1:{port}"), token);
        // `raw` fails only on transport-level errors (connect/timeout/body
        // read); a live daemon that answers — even with an HTTP error —
        // comes back as Ok with the status, so it settles the probe
        // immediately instead of being retried.
        const PROBE_ATTEMPTS: usize = 3;
        const PROBE_BACKOFF: Duration = Duration::from_millis(200);
        for attempt in 0..PROBE_ATTEMPTS {
            if attempt > 0 {
                tokio::time::sleep(PROBE_BACKOFF).await;
            }
            match client.raw("GET", "/status", None).await {
                Ok((status, bytes)) => {
                    if !status.is_success() {
                        return None;
                    }
                    serde_json::from_slice::<StatusResponse>(&bytes).ok()?;
                    return Some(client);
                }
                Err(_) => continue,
            }
        }
        None
    }

    async fn request<R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<R> {
        let (status, bytes) = self
            .raw(method, path, body)
            .await
            .context("daemon request failed")?;
        if !status.is_success() {
            let message = serde_json::from_slice::<ApiError>(&bytes)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("HTTP {status}"));
            anyhow::bail!("daemon returned {status}: {message}");
        }
        serde_json::from_slice(&bytes).map_err(|e| anyhow!("invalid daemon response: {e}"))
    }

    async fn raw(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(StatusCode, Bytes)> {
        let builder = hyper::Request::builder()
            .method(method)
            .uri(format!("{}{}", self.base, path))
            .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
            .header(CLIENT_HEADER, CLIENT_ID);
        let request = match body {
            Some(value) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Full::new(Bytes::from(value.to_string())))
                .map_err(|e| anyhow!("bad request: {e}"))?,
            None => builder
                .body(Full::new(Bytes::new()))
                .map_err(|e| anyhow!("bad request: {e}"))?,
        };
        let response = tokio::time::timeout(CLIENT_TIMEOUT, self.http.request(request))
            .await
            .map_err(|_| anyhow!("daemon request timed out"))?
            .map_err(|e| anyhow!("cannot reach the daemon: {e}"))?;
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|e| anyhow!("failed to read daemon response: {e}"))?
            .to_bytes();
        Ok((status, bytes))
    }

    pub async fn get_status(&self) -> Result<StatusResponse> {
        self.request("GET", "/status", None).await
    }

    pub async fn review(
        &self,
        key: Option<&str>,
        all: bool,
        force: bool,
    ) -> Result<ReviewResponse> {
        let key = if all {
            "all".to_string()
        } else {
            key.unwrap_or("all").to_string()
        };
        self.request(
            "POST",
            "/review",
            Some(json!({ "key": key, "force": force })),
        )
        .await
    }

    /// `POST /review` for one PR with ephemeral extra instructions (D10).
    pub async fn review_with_prompt(
        &self,
        key: &str,
        force: bool,
        extra_prompt: &str,
    ) -> Result<ReviewResponse> {
        let overrides = ReviewOverrides {
            extra_prompt: Some(extra_prompt.to_string()),
            ..ReviewOverrides::default()
        };
        self.review_with(key, force, &overrides).await
    }

    /// `POST /review` for one target in any reference form (design D5) with
    /// per-run options (design D1). Only the options that are set are sent.
    pub async fn review_with(
        &self,
        target: &str,
        force: bool,
        overrides: &ReviewOverrides,
    ) -> Result<ReviewResponse> {
        let body = ReviewRequest {
            key: target.to_string(),
            force,
            extra_prompt: overrides.extra_prompt.clone(),
            engine: overrides.engine.clone(),
            post: overrides.post,
        };
        let body = serde_json::to_value(&body).context("cannot encode the review request")?;
        self.request("POST", "/review", Some(body)).await
    }

    pub async fn get_config(&self) -> Result<ConfigResponse> {
        self.request("GET", "/config", None).await
    }

    /// `PUT /config` with a structured config. A validation failure comes
    /// back as an error whose value is a [`ConfigRejection`].
    pub async fn put_config(&self, config: &Config) -> Result<ConfigWriteResponse> {
        let value = serde_json::to_value(config).context("cannot encode the config")?;
        self.put_config_value(value).await
    }

    /// `PUT /config` with a structured config as untyped JSON (the settings
    /// app's form): the daemon parses it, so type errors come back as a
    /// [`ConfigRejection`] naming the field, exactly as for the file.
    pub async fn put_config_value(&self, config: serde_json::Value) -> Result<ConfigWriteResponse> {
        self.write_config(ConfigWriteRequest {
            config: Some(config),
            yaml: None,
        })
        .await
    }

    /// `PUT /config` with the raw YAML text (written verbatim when valid).
    pub async fn put_config_yaml(&self, yaml: &str) -> Result<ConfigWriteResponse> {
        self.write_config(ConfigWriteRequest {
            config: None,
            yaml: Some(yaml.to_string()),
        })
        .await
    }

    async fn write_config(&self, req: ConfigWriteRequest) -> Result<ConfigWriteResponse> {
        let body = serde_json::to_value(&req).context("cannot encode the config")?;
        let (status, bytes) = self
            .raw("PUT", "/config", Some(body))
            .await
            .context("daemon request failed")?;
        if status == StatusCode::UNPROCESSABLE_ENTITY {
            if let Ok(rejection) = serde_json::from_slice::<ConfigRejection>(&bytes) {
                return Err(anyhow::Error::new(rejection));
            }
        }
        if !status.is_success() {
            let message = serde_json::from_slice::<ApiError>(&bytes)
                .map(|e| e.error)
                .unwrap_or_else(|_| format!("HTTP {status}"));
            anyhow::bail!("daemon returned {status}: {message}");
        }
        serde_json::from_slice(&bytes).map_err(|e| anyhow!("invalid daemon response: {e}"))
    }

    /// `GET /discover/agents`.
    pub async fn discover_agents(&self) -> Result<crate::discover::AgentsResponse> {
        self.request("GET", "/discover/agents", None).await
    }

    /// `GET /discover/repos`.
    pub async fn discover_repos(&self) -> Result<crate::discover::ReposResponse> {
        self.request("GET", "/discover/repos", None).await
    }

    pub async fn set_review_mode(&self, mode: ReviewMode) -> Result<ReviewModeResponse> {
        self.request(
            "POST",
            "/review-mode",
            Some(json!(ReviewModeRequest { mode })),
        )
        .await
    }

    pub async fn block(&self, key: &str) -> Result<MutationResponse> {
        self.request("POST", "/block", Some(json!({ "key": key })))
            .await
    }

    pub async fn unblock(&self, key: &str) -> Result<MutationResponse> {
        self.request("POST", "/unblock", Some(json!({ "key": key })))
            .await
    }

    pub async fn approve(&self, key: &str) -> Result<MutationResponse> {
        self.request("POST", "/approve", Some(json!({ "key": key })))
            .await
    }

    pub async fn snooze(
        &self,
        key: &str,
        hours: Option<f64>,
        until_push: bool,
    ) -> Result<MutationResponse> {
        self.request(
            "POST",
            "/snooze",
            Some(json!({ "key": key, "hours": hours, "until_push": until_push })),
        )
        .await
    }

    /// `POST /brief`: start generating `key`'s brief (design B4).
    pub async fn brief(&self, key: &str) -> Result<BriefResponse> {
        self.request("POST", "/brief", Some(json!({ "key": key })))
            .await
    }

    pub async fn ignore(&self, key: &str) -> Result<MutationResponse> {
        self.request("POST", "/ignore", Some(json!({ "key": key })))
            .await
    }

    pub async fn unignore(&self, key: &str) -> Result<MutationResponse> {
        self.request("POST", "/unignore", Some(json!({ "key": key })))
            .await
    }

    pub async fn unsnooze(&self, key: &str) -> Result<MutationResponse> {
        self.request("POST", "/unsnooze", Some(json!({ "key": key })))
            .await
    }

    pub async fn review_now(&self) -> Result<MutationResponse> {
        self.request("POST", "/review-now", None).await
    }

    /// `POST /pause` (D5): exactly one of `hours` / `until`.
    pub async fn pause(
        &self,
        hours: Option<f64>,
        until: Option<DateTime<Utc>>,
    ) -> Result<PauseResponse> {
        self.request("POST", "/pause", Some(json!(PauseRequest { hours, until })))
            .await
    }

    /// `POST /resume` (D5).
    pub async fn resume(&self) -> Result<PauseResponse> {
        self.request("POST", "/resume", Some(json!({}))).await
    }

    pub async fn history(&self, repo: Option<&str>, limit: usize) -> Result<HistoryResponse> {
        self.history_page(repo, None, limit).await
    }

    /// `GET /history` paged: entries strictly older than `before`.
    pub async fn history_page(
        &self,
        repo: Option<&str>,
        before: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<HistoryResponse> {
        let mut path = format!("/history?limit={limit}");
        if let Some(repo) = repo {
            path.push_str(&format!("&repo={}", encode_uri_component(repo)));
        }
        if let Some(before) = before {
            path.push_str(&format!(
                "&before={}",
                encode_uri_component(&before.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true))
            ));
        }
        self.request("GET", &path, None).await
    }

    /// `GET /usage` (design D13).
    pub async fn usage(&self, query: &crate::usage::Query) -> Result<crate::usage::Usage> {
        let mut path = format!(
            "/usage?bucket={}&window={}&group={}",
            query.bucket, query.window, query.group
        );
        if let Some(repo) = &query.repo {
            path.push_str(&format!("&repo={}", encode_uri_component(repo)));
        }
        self.request("GET", &path, None).await
    }

    pub async fn insights(
        &self,
        repo: Option<&str>,
        days: Option<u64>,
    ) -> Result<crate::insights::Insights> {
        let mut path = String::from("/insights");
        let mut params: Vec<String> = Vec::new();
        if let Some(repo) = repo {
            params.push(format!("repo={}", encode_uri_component(repo)));
        }
        if let Some(days) = days {
            params.push(format!("days={days}"));
        }
        if !params.is_empty() {
            path.push('?');
            path.push_str(&params.join("&"));
        }
        self.request("GET", &path, None).await
    }
}

fn encode_uri_component(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '/' | '-') {
                c.to_string()
            } else {
                format!("%{:02X}", c as u32)
            }
        })
        .collect()
}

/// Raw status probe (tests: auth rejection, endpoint status codes).
pub async fn probe_status(port: u16, path: &str, token: Option<&str>) -> Result<u16> {
    let client = DaemonClient::with_base(format!("http://127.0.0.1:{port}"), "");
    let mut builder = hyper::Request::builder()
        .method("GET")
        .uri(format!("http://127.0.0.1:{port}{path}"));
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let request = builder
        .body(Full::new(Bytes::new()))
        .map_err(|e| anyhow!("bad probe request: {e}"))?;
    let response = tokio::time::timeout(CLIENT_TIMEOUT, client.http.request(request))
        .await
        .map_err(|_| anyhow!("probe timed out"))?
        .map_err(|e| anyhow!("probe failed: {e}"))?;
    Ok(response.status().as_u16())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_64_hex_chars() {
        let token = generate_token();
        assert_eq!(token.len(), 64);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(token, generate_token());
    }

    fn status_payload(config_error: Option<ConfigErrorInfo>, mode: ReviewMode) -> StatusResponse {
        StatusResponse {
            version: "1".into(),
            pid: 1,
            uptime_seconds: 2,
            in_flight: 0,
            queue_depth: 0,
            last_poll: BTreeMap::new(),
            last_state_poll: None,
            next_poll: None,
            prs: vec![],
            config_error,
            review_mode: mode,
            paused_until: None,
            engines: vec![],
            menu: MenuSettings::default(),
        }
    }

    /// 5.1: the payload carries `review_mode` and (only while set)
    /// `config_error` alongside the unchanged existing fields.
    #[test]
    fn status_payload_carries_mode_and_config_error() {
        let healthy = serde_json::to_value(status_payload(None, ReviewMode::Monitor)).unwrap();
        assert_eq!(healthy["review_mode"], "monitor");
        assert!(healthy.get("config_error").is_none());
        for field in [
            "version",
            "pid",
            "uptime_seconds",
            "in_flight",
            "queue_depth",
            "last_poll",
            "last_state_poll",
            "prs",
        ] {
            assert!(healthy.get(field).is_some(), "{field} missing");
        }

        let at: DateTime<Utc> = "2026-09-05T00:00:00Z".parse().unwrap();
        let broken = serde_json::to_value(status_payload(
            Some(ConfigErrorInfo {
                message: "invalid configuration: x".into(),
                at,
            }),
            ReviewMode::Off,
        ))
        .unwrap();
        assert_eq!(broken["review_mode"], "off");
        assert_eq!(
            broken["config_error"]["message"],
            "invalid configuration: x"
        );
        assert_eq!(broken["config_error"]["at"], "2026-09-05T00:00:00Z");

        // An older daemon's payload (no new fields) reads as healthy and
        // `review comment`.
        let mut old = healthy;
        old.as_object_mut().unwrap().remove("review_mode");
        let parsed: StatusResponse = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.review_mode, ReviewMode::ReviewComment);
        assert_eq!(parsed.config_error, None);
    }

    #[test]
    fn rejection_response_names_the_field() {
        let err = ConfigError::Validation("defaults.poll_interval_minutes must be > 0".into());
        let rejection = ConfigRejection {
            error: err.to_string(),
            field: config::error_field(&err),
        };
        assert_eq!(
            rejection.field.as_deref(),
            Some("defaults.poll_interval_minutes")
        );
        let as_anyhow = anyhow::Error::new(rejection.clone());
        assert_eq!(
            as_anyhow.downcast_ref::<ConfigRejection>(),
            Some(&rejection)
        );
    }
}
