//! The JS↔Rust bridge of the settings window (task 8.2, design D6).
//!
//! Wire format (JSON strings over wry's IPC handler / `evaluate_script`):
//!
//! ```text
//! JS → Rust   {"id": 7, "method": "config.get",      "params": {}}
//!             {"id": 8, "method": "config.put",      "params": {"config": {…}}}   structured
//!             {"id": 9, "method": "config.put",      "params": {"yaml": "…"}}     raw text
//!             {"id": 10, "method": "review_mode.set", "params": {"mode": "monitor"}}
//!             {"id": 11, "method": "discover.agents", "params": {}}
//!             {"id": 12, "method": "discover.repos",  "params": {}}
//!             {"id": 13, "method": "status.get",      "params": {}}
//!             {"id": 14, "method": "history.list",    "params": {"limit": 50, "repo": "o/r", "before": "<RFC3339>"}}
//!             {"id": 15, "method": "usage.get",       "params": {"bucket": "week", "window": "30d", "group": "engine", "repo": "o/r"}}
//!             {"id": 16, "method": "insights.get",    "params": {"days": 30, "repo": "o/r"}}
//!             {"id": 17, "method": "review.request",  "params": {"target": "42", "engine": "codex", "post": false, "extra_prompt": "…"}}
//!             {"id": 18, "method": "brief.request",   "params": {"key": "acme/api#42"}}
//!             {"id": 19, "method": "pause.start",     "params": {"hours": 2}}   or {"until": "<RFC3339>"}
//!             {"id": 20, "method": "pause.resume",    "params": {}}
//! Rust → JS   {"id": 7, "ok": true,  "result": …}
//!             {"id": 8, "ok": false, "error": {"kind": "rejected", "message": "…", "field": "defaults.poll_interval_minutes"}}
//! ```
//!
//! `app.*` methods (login item, CLI link) are handled by
//! [`super::app_bridge`], and the browser picker's `browsers.list`,
//! `browsers.choose` and `browsers.test` by [`super::browser_bridge`]
//! (design W1); none of them reach the daemon.
//!
//! `id` is chosen by JS and echoed verbatim (correlation; responses may
//! arrive out of order). Error kinds: `bad_request` (malformed message,
//! unknown method, bad params — `id` is null when it could not be read),
//! `offline` (no daemon), `rejected` (HTTP 422 config validation; `field`
//! when attributable), `unsupported` (the daemon is too old for the route:
//! a 404 from `/usage`, design D14), `daemon` (any other daemon/transport
//! failure; for `review.request` and `brief.request` the message is the
//! daemon's own text, e.g. ambiguous candidates or "already running").
//!
//! The Overview reads (`status.get`, `history.list`, `usage.get`,
//! `insights.get`) pass the daemon's JSON through verbatim (a plain
//! authenticated loopback GET, so newer payload fields reach the page
//! without a shell rebuild); `review.request` goes through
//! [`DaemonClient::review_with`] with the per-run overrides (design D1).
//!
//! Every daemon call goes through [`prx::ipc::DaemonClient`] in this
//! process: the token never reaches JS, and the only network traffic is
//! the client's `http://127.0.0.1:<port>` loopback endpoint. Calls run on
//! a worker thread ([`Bridge::handle`]); the reply is handed to `deliver`,
//! which the window host routes back to the main thread for
//! `evaluate_script`. The routing here is webview-free so it unit-tests.

use std::future::Future;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use prx::config::ReviewMode;
use prx::ipc::{ConfigRejection, DaemonClient};
use prx::review::ReviewOverrides;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One JS request as received.
#[derive(Debug, Deserialize)]
struct RawRequest {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

/// A parsed bridge call.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `GET /config`.
    GetConfig,
    /// `PUT /config` with the structured config (JSON in the config's shape).
    PutConfig(Value),
    /// `PUT /config` with the raw YAML text.
    PutConfigYaml(String),
    /// `POST /review-mode`.
    SetReviewMode(ReviewMode),
    /// `GET /discover/agents`: agent CLIs found on this machine.
    DiscoverAgents,
    /// `GET /discover/repos`: repositories the gh account can see.
    DiscoverRepos,
    /// `GET /status` (Overview pages).
    GetStatus,
    /// `GET /history?…` — the path with its query (`limit`, `repo`,
    /// `before` paging).
    ListHistory(String),
    /// `GET /usage?…` — the path with its query.
    GetUsage(String),
    /// `GET /insights?…` — the path with its query (`days`, `repo`).
    GetInsights(String),
    /// `POST /review` (forced) for any target form with per-run options.
    RequestReview {
        target: String,
        overrides: ReviewOverrides,
    },
    /// `POST /brief` (task 4.3): write or regenerate a PR's brief in the
    /// background; the window follows `brief_pending` on `/status`.
    RequestBrief(String),
    /// `POST /pause` (General/Dashboard pause choices): hold automatic
    /// reviews for `hours` or until `until` — exactly one is set.
    Pause {
        hours: Option<f64>,
        until: Option<DateTime<Utc>>,
    },
    /// `POST /resume` (Dashboard "Resume reviews"): end a pause now.
    Resume,
}

impl Call {
    /// The wire method name (for logs; never the payload).
    pub fn method(&self) -> &'static str {
        match self {
            Call::GetConfig => "config.get",
            Call::PutConfig(_) => "config.put (structured)",
            Call::PutConfigYaml(_) => "config.put (yaml)",
            Call::SetReviewMode(_) => "review_mode.set",
            Call::DiscoverAgents => "discover.agents",
            Call::DiscoverRepos => "discover.repos",
            Call::GetStatus => "status.get",
            Call::ListHistory(_) => "history.list",
            Call::GetUsage(_) => "usage.get",
            Call::GetInsights(_) => "insights.get",
            Call::RequestReview { .. } => "review.request",
            Call::RequestBrief(_) => "brief.request",
            Call::Pause { .. } => "pause.start",
            Call::Resume => "pause.resume",
        }
    }
}

/// The bridge's error payload (`error` of a failed response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeError {
    pub kind: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

impl BridgeError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        BridgeError {
            kind: kind.into(),
            message: message.into(),
            field: None,
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        BridgeError::new("bad_request", message)
    }

    /// The daemon predates the route (design D14: "update the daemon").
    pub fn unsupported(message: impl Into<String>) -> Self {
        BridgeError::new("unsupported", message)
    }

    pub fn offline() -> Self {
        BridgeError::new(
            "offline",
            "the PRX daemon is not running — start it from the PRX menu or with `prx daemon start` (nothing was saved)",
        )
    }

    /// Map a daemon-client error: a 422 [`ConfigRejection`] keeps its field.
    pub fn from_daemon(error: anyhow::Error) -> Self {
        match error.downcast_ref::<ConfigRejection>() {
            Some(rejection) => BridgeError {
                kind: "rejected".into(),
                message: rejection.error.clone(),
                field: rejection.field.clone(),
            },
            None => BridgeError::new("daemon", format!("{error:#}")),
        }
    }
}

/// One reply to JS, correlated by the request's `id`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: Value,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<BridgeError>,
}

impl Response {
    pub fn success(id: Value, result: Value) -> Self {
        Response {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Value, error: BridgeError) -> Self {
        Response {
            id,
            ok: false,
            result: None,
            error: Some(error),
        }
    }

    /// The JS statement that hands this reply to the page's bridge client
    /// (`settings-ui/bridge.js`). JSON is a valid JS expression.
    pub fn to_script(&self) -> String {
        let json = serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"id":null,"ok":false,"error":{"kind":"daemon","message":"unencodable reply"}}"#
                .into()
        });
        format!("window.prxBridge && window.prxBridge.__resolve({json});")
    }
}

/// Parse a JS message into `(id, call)`; a malformed one becomes the
/// `bad_request` reply to send back (with the id when it was readable).
pub fn parse(body: &str) -> Result<(Value, Call), Box<Response>> {
    let raw: RawRequest = serde_json::from_str(body).map_err(|e| {
        let id = serde_json::from_str::<Value>(body)
            .ok()
            .and_then(|v| v.get("id").cloned())
            .unwrap_or(Value::Null);
        Box::new(Response::failure(
            id,
            BridgeError::bad_request(format!("malformed bridge message: {e}")),
        ))
    })?;
    let id = raw.id;
    let fail = |message: String| {
        Box::new(Response::failure(
            id.clone(),
            BridgeError::bad_request(message),
        ))
    };
    let call = match raw.method.as_str() {
        "config.get" => Call::GetConfig,
        "config.put" => {
            let config = raw.params.get("config").filter(|v| !v.is_null());
            let yaml = raw.params.get("yaml").filter(|v| !v.is_null());
            match (config, yaml) {
                (Some(config), None) => Call::PutConfig(config.clone()),
                (None, Some(Value::String(yaml))) => Call::PutConfigYaml(yaml.clone()),
                (None, Some(_)) => return Err(fail("config.put: `yaml` must be a string".into())),
                _ => return Err(fail(
                    "config.put: send exactly one of `config` (structured) or `yaml` (raw text)"
                        .into(),
                )),
            }
        }
        "review_mode.set" => {
            let mode = raw.params.get("mode").cloned().unwrap_or(Value::Null);
            match serde_json::from_value::<ReviewMode>(mode) {
                Ok(mode) => Call::SetReviewMode(mode),
                Err(e) => return Err(fail(format!("review_mode.set: invalid `mode`: {e}"))),
            }
        }
        "discover.agents" => Call::DiscoverAgents,
        "discover.repos" => Call::DiscoverRepos,
        "status.get" => Call::GetStatus,
        "history.list" => Call::ListHistory(
            query_path(
                "/history",
                &raw.params,
                &[
                    ("limit", Param::Count),
                    ("repo", Param::Text),
                    ("before", Param::Text),
                ],
            )
            .map_err(|e| fail(format!("history.list: {e}")))?,
        ),
        "usage.get" => Call::GetUsage(
            query_path(
                "/usage",
                &raw.params,
                &[
                    ("bucket", Param::Text),
                    ("window", Param::Text),
                    ("group", Param::Text),
                    ("repo", Param::Text),
                ],
            )
            .map_err(|e| fail(format!("usage.get: {e}")))?,
        ),
        "insights.get" => Call::GetInsights(
            query_path(
                "/insights",
                &raw.params,
                &[("days", Param::Count), ("repo", Param::Text)],
            )
            .map_err(|e| fail(format!("insights.get: {e}")))?,
        ),
        "review.request" => {
            parse_review(&raw.params).map_err(|e| fail(format!("review.request: {e}")))?
        }
        "brief.request" => match raw.params.get("key") {
            Some(Value::String(key)) if !key.trim().is_empty() => {
                Call::RequestBrief(key.trim().to_string())
            }
            _ => return Err(fail("brief.request: `key` must be a PR key".into())),
        },
        "pause.start" => parse_pause(&raw.params).map_err(|e| fail(format!("pause.start: {e}")))?,
        "pause.resume" => Call::Resume,
        other => return Err(fail(format!("unknown bridge method '{other}'"))),
    };
    Ok((id, call))
}

/// The accepted type of one query parameter.
#[derive(Clone, Copy)]
enum Param {
    /// A non-empty string (sent percent-encoded).
    Text,
    /// A positive integer.
    Count,
}

/// `base` plus the given `params` as a query string, in `allowed` order.
/// Absent, null, and empty-string values are skipped (the daemon applies
/// its defaults); a wrongly typed value is an error.
fn query_path(base: &str, params: &Value, allowed: &[(&str, Param)]) -> Result<String, String> {
    let mut pairs = Vec::new();
    for &(name, kind) in allowed {
        let value = match params.get(name) {
            None | Some(Value::Null) => continue,
            Some(value) => value,
        };
        let text = match (kind, value) {
            (Param::Text, Value::String(text)) if text.is_empty() => continue,
            (Param::Text, Value::String(text)) => text.clone(),
            (Param::Count, Value::Number(n)) if n.as_u64().is_some_and(|n| n > 0) => n.to_string(),
            (Param::Text, _) => return Err(format!("`{name}` must be a string")),
            (Param::Count, _) => return Err(format!("`{name}` must be a positive integer")),
        };
        pairs.push(format!("{name}={}", encode_component(&text)));
    }
    Ok(if pairs.is_empty() {
        base.to_string()
    } else {
        format!("{base}?{}", pairs.join("&"))
    })
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass).
fn encode_component(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// `pause.start {hours} | {until}`: exactly one — `hours` a positive
/// number, `until` an RFC 3339 timestamp (the daemon checks it is in the
/// future, as for `prx pause`).
fn parse_pause(params: &Value) -> Result<Call, String> {
    let present = |name: &str| !matches!(params.get(name), None | Some(Value::Null));
    match (present("hours"), present("until")) {
        (true, true) => Err("give `hours` or `until`, not both".into()),
        (false, false) => Err("give `hours` or `until`".into()),
        (true, false) => match params["hours"].as_f64() {
            Some(hours) if hours.is_finite() && hours > 0.0 => Ok(Call::Pause {
                hours: Some(hours),
                until: None,
            }),
            _ => Err("`hours` must be a positive number".into()),
        },
        (false, true) => {
            let until = params["until"]
                .as_str()
                .and_then(|text| DateTime::parse_from_rfc3339(text.trim()).ok())
                .ok_or("`until` must be an RFC 3339 timestamp")?;
            Ok(Call::Pause {
                hours: None,
                until: Some(until.with_timezone(&Utc)),
            })
        }
    }
}

/// `review.request {target, extra_prompt?, engine?, post?}`: the target is
/// required; blank optional text counts as absent.
fn parse_review(params: &Value) -> Result<Call, String> {
    let text = |name: &str| -> Result<Option<String>, String> {
        match params.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => {
                Ok(Some(text.trim().to_string()).filter(|t| !t.is_empty()))
            }
            Some(_) => Err(format!("`{name}` must be a string")),
        }
    };
    let target = text("target")?.ok_or("enter a PR reference (`target`)")?;
    let post = match params.get("post") {
        None | Some(Value::Null) => None,
        Some(Value::Bool(post)) => Some(*post),
        Some(_) => return Err("`post` must be a boolean".into()),
    };
    Ok(Call::RequestReview {
        target,
        overrides: ReviewOverrides {
            engine: text("engine")?,
            post,
            extra_prompt: text("extra_prompt")?,
        },
    })
}

/// Map a daemon HTTP answer for a pass-through GET of `path`: 2xx → its
/// JSON; a 404 from `/usage` → `unsupported` (an older daemon); anything
/// else → `daemon` with the daemon's `{"error"}` text when it sent one.
pub fn http_reply(path: &str, status: u16, body: &[u8]) -> Result<Value, BridgeError> {
    if (200..300).contains(&status) {
        return serde_json::from_slice(body)
            .map_err(|e| BridgeError::new("daemon", format!("invalid daemon response: {e}")));
    }
    if status == 404 && path.starts_with("/usage") {
        return Err(BridgeError::unsupported(
            "this prx daemon does not report token usage — update prx and restart the daemon",
        ));
    }
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| format!("daemon returned HTTP {status}"));
    Err(BridgeError::new("daemon", message))
}

/// What the bridge needs from the daemon. Production is [`DaemonBackend`];
/// tests substitute fakes.
pub trait Backend: Send + Sync + 'static {
    fn execute(&self, call: Call) -> impl Future<Output = Result<Value, BridgeError>> + Send;
}

/// The real backend: re-discovers the daemon per call (fresh port/token
/// files, like the menu actions) and proxies through [`DaemonClient`].
pub struct DaemonBackend;

impl Backend for DaemonBackend {
    async fn execute(&self, call: Call) -> Result<Value, BridgeError> {
        let client = DaemonClient::discover()
            .await
            .ok_or_else(BridgeError::offline)?;
        let encode = |value: Result<Value, serde_json::Error>| {
            value.map_err(|e| BridgeError::new("daemon", format!("cannot encode reply: {e}")))
        };
        match call {
            Call::GetConfig => {
                let config = client
                    .get_config()
                    .await
                    .map_err(BridgeError::from_daemon)?;
                encode(serde_json::to_value(config))
            }
            Call::PutConfig(value) => {
                let written = client
                    .put_config_value(value)
                    .await
                    .map_err(BridgeError::from_daemon)?;
                encode(serde_json::to_value(written))
            }
            Call::PutConfigYaml(yaml) => {
                let written = client
                    .put_config_yaml(&yaml)
                    .await
                    .map_err(BridgeError::from_daemon)?;
                encode(serde_json::to_value(written))
            }
            Call::SetReviewMode(mode) => {
                let response = client
                    .set_review_mode(mode)
                    .await
                    .map_err(BridgeError::from_daemon)?;
                encode(serde_json::to_value(response))
            }
            Call::DiscoverAgents => {
                let found = client
                    .discover_agents()
                    .await
                    .map_err(BridgeError::from_daemon)?;
                encode(serde_json::to_value(found))
            }
            Call::DiscoverRepos => {
                let found = client
                    .discover_repos()
                    .await
                    .map_err(BridgeError::from_daemon)?;
                encode(serde_json::to_value(found))
            }
            Call::GetStatus => loopback_get("/status").await,
            Call::ListHistory(path) | Call::GetUsage(path) | Call::GetInsights(path) => {
                loopback_get(&path).await
            }
            Call::RequestReview { target, overrides } => {
                // Forced, like the menu's review actions (same request).
                let response = client
                    .review_with(&target, true, &overrides)
                    .await
                    .map_err(|e| {
                        BridgeError::new(
                            "daemon",
                            crate::actions::daemon_error_text(&format!("{e:#}")),
                        )
                    })?;
                encode(serde_json::to_value(response))
            }
            Call::RequestBrief(key) => {
                let response = client.brief(&key).await.map_err(|e| {
                    BridgeError::new(
                        "daemon",
                        crate::actions::brief_error_text(&format!("{e:#}")),
                    )
                })?;
                encode(serde_json::to_value(response))
            }
            Call::Pause { hours, until } => {
                // A 400 (e.g. an `until` in the past) keeps the daemon's text.
                let response = client.pause(hours, until).await.map_err(|e| {
                    BridgeError::new(
                        "daemon",
                        crate::actions::daemon_error_text(&format!("{e:#}")),
                    )
                })?;
                encode(serde_json::to_value(response))
            }
            Call::Resume => {
                let response = client.resume().await.map_err(BridgeError::from_daemon)?;
                encode(serde_json::to_value(response))
            }
        }
    }
}

/// An authenticated `GET path` against the discovered daemon, answering its
/// JSON verbatim ([`http_reply`] maps the status). The token is read here
/// and never leaves this process. Blocking I/O is fine: each bridge call
/// runs alone on its own worker thread and runtime.
async fn loopback_get(path: &str) -> Result<Value, BridgeError> {
    use std::io::{Read, Write};
    let read = |file: std::path::PathBuf| {
        std::fs::read_to_string(file)
            .map(|s| s.trim().to_string())
            .map_err(|_| BridgeError::offline())
    };
    let port: u16 = read(prx::paths::daemon_port_file())?
        .parse()
        .map_err(|_| BridgeError::offline())?;
    let token = read(prx::paths::daemon_token_file())?;
    let transport =
        |e: std::io::Error| BridgeError::new("daemon", format!("daemon request failed: {e}"));
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).map_err(transport)?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(transport)?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\n\
         x-prx-client: menubar/{}\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    )
    .map_err(transport)?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(transport)?;
    let (status, body) = parse_http_response(&raw).map_err(|e| BridgeError::new("daemon", e))?;
    http_reply(path, status, &body)
}

/// Split a raw HTTP/1.1 response into its status code and (de-chunked)
/// body.
pub fn parse_http_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("truncated daemon response")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let body = &raw[split + 4..];
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or("malformed daemon status line")?;
    let chunked = head.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.trim().eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
        })
    });
    if !chunked {
        return Ok((status, body.to_vec()));
    }
    let mut out = Vec::new();
    let mut rest = body;
    loop {
        let line_end = rest
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("truncated chunked body")?;
        let size_text = String::from_utf8_lossy(&rest[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "malformed chunk size")?;
        rest = &rest[line_end + 2..];
        if size == 0 {
            return Ok((status, out));
        }
        let chunk = rest.get(..size).ok_or("truncated chunk")?;
        out.extend_from_slice(chunk);
        rest = rest.get(size + 2..).unwrap_or(&[]);
    }
}

/// Routes JS messages to the backend off the calling (main) thread and
/// hands every reply to `deliver` (any thread).
pub struct Bridge<B: Backend> {
    backend: Arc<B>,
    deliver: Arc<dyn Fn(Response) + Send + Sync>,
    /// Main-thread handler for `browsers.choose` (given the request id); it
    /// must answer through the same reply path. `None` answers
    /// `unsupported`.
    chooser: Option<Arc<dyn Fn(Value) + Send + Sync>>,
}

impl<B: Backend> Clone for Bridge<B> {
    fn clone(&self) -> Self {
        Bridge {
            backend: Arc::clone(&self.backend),
            deliver: Arc::clone(&self.deliver),
            chooser: self.chooser.clone(),
        }
    }
}

impl<B: Backend> Bridge<B> {
    pub fn new(backend: B, deliver: Arc<dyn Fn(Response) + Send + Sync>) -> Self {
        Bridge {
            backend: Arc::new(backend),
            deliver,
            chooser: None,
        }
    }

    /// Route `browsers.choose` to `chooser` (called on the thread that
    /// calls [`Bridge::handle`], the main thread for the window host).
    pub fn with_chooser(mut self, chooser: Arc<dyn Fn(Value) + Send + Sync>) -> Self {
        self.chooser = Some(chooser);
        self
    }

    /// Handle one JS message without blocking: malformed messages are
    /// answered immediately, valid calls run on a worker thread with their
    /// own current-thread runtime. Exactly one reply per message.
    pub fn handle(&self, body: &str) {
        // App-local methods (login item, CLI link; tasks 6.6/6.7) never
        // touch the daemon: they have their own module.
        if super::app_bridge::is_app_message(body) {
            let deliver = Arc::clone(&self.deliver);
            let body = body.to_string();
            let _ = std::thread::Builder::new()
                .name("prx-settings-app".into())
                .spawn(move || deliver(super::app_bridge::handle(&body)));
            return;
        }
        // Browser picker (design W1): AppKit discovery and test opens run
        // here; the NSOpenPanel of `browsers.choose` needs the main thread,
        // so the window host's chooser answers it.
        if super::browser_bridge::is_browser_message(body) {
            if super::browser_bridge::method_of(body).as_deref() == Some("browsers.choose") {
                if let Some(chooser) = &self.chooser {
                    eprintln!("prx-menubar: settings bridge browsers.choose");
                    return chooser(super::browser_bridge::id_of(body));
                }
            }
            let deliver = Arc::clone(&self.deliver);
            let body = body.to_string();
            let _ = std::thread::Builder::new()
                .name("prx-settings-browsers".into())
                .spawn(move || deliver(super::browser_bridge::handle(&body)));
            return;
        }
        let (id, call) = match parse(body) {
            Ok(parsed) => parsed,
            Err(reply) => return (self.deliver)(*reply),
        };
        eprintln!("prx-menubar: settings bridge {} (id {id})", call.method());
        let me = self.clone();
        let worker_id = id.clone();
        let spawned = std::thread::Builder::new()
            .name("prx-settings-bridge".into())
            .spawn(move || {
                let reply = me.run(worker_id, call);
                (me.deliver)(reply);
            });
        if let Err(error) = spawned {
            (self.deliver)(Response::failure(
                id,
                BridgeError::new("daemon", format!("cannot start bridge worker: {error}")),
            ));
        }
    }

    /// Execute one parsed call to completion (blocking; worker threads only).
    pub fn run(&self, id: Value, call: Call) -> Response {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                return Response::failure(
                    id,
                    BridgeError::new("daemon", format!("cannot start bridge runtime: {error}")),
                )
            }
        };
        match runtime.block_on(self.backend.execute(call)) {
            Ok(result) => Response::success(id, result),
            Err(error) => Response::failure(id, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Fake daemon: `config.get` is slow, so a later request overtakes it.
    struct Fake;

    impl Backend for Fake {
        async fn execute(&self, call: Call) -> Result<Value, BridgeError> {
            match call {
                Call::GetConfig => {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Ok(json!({ "yaml": "defaults: {}\n" }))
                }
                Call::PutConfigYaml(yaml) if yaml.contains("poll_interval_minutes: 0") => Err(
                    BridgeError::from_daemon(anyhow::Error::new(ConfigRejection {
                        error: "defaults.poll_interval_minutes must be at least 1".into(),
                        field: Some("defaults.poll_interval_minutes".into()),
                    })),
                ),
                Call::PutConfigYaml(_) | Call::PutConfig(_) => {
                    Ok(json!({ "ok": true, "added": [], "removed": [], "discarded": [] }))
                }
                Call::SetReviewMode(_) => Err(BridgeError::offline()),
                Call::DiscoverAgents => {
                    Ok(json!({ "agents": [], "configured": {}, "llm_apis": [] }))
                }
                Call::DiscoverRepos => Ok(json!({ "repos": [] })),
                Call::GetStatus => Ok(json!({ "prs": [] })),
                Call::ListHistory(path) | Call::GetInsights(path) => Ok(json!({ "path": path })),
                Call::GetUsage(path) => http_reply(&path, 404, b""),
                Call::RequestReview { target, overrides } => Ok(json!({
                    "enqueued": [target],
                    "note": serde_json::to_value(&overrides).unwrap(),
                })),
                Call::RequestBrief(key) if key.ends_with("#9") => Err(BridgeError::new(
                    "daemon",
                    format!("a brief for {key} is already being written"),
                )),
                Call::RequestBrief(key) => Ok(json!({ "accepted": true, "key": key })),
                Call::Pause { hours: Some(h), .. } if h > 100.0 => Err(BridgeError::new(
                    "daemon",
                    "pause end must be in the future",
                )),
                Call::Pause { hours, until } => Ok(json!({
                    "paused_until": "2026-10-08T12:00:00Z",
                    "hours": hours,
                    "until": until,
                })),
                Call::Resume => Ok(json!({ "paused_until": null })),
            }
        }
    }

    fn bridge() -> (Bridge<Fake>, mpsc::Receiver<Response>) {
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        let bridge = Bridge::new(
            Fake,
            Arc::new(move |reply| {
                let _ = tx.lock().unwrap().send(reply);
            }),
        );
        (bridge, rx)
    }

    fn recv(rx: &mpsc::Receiver<Response>) -> Response {
        rx.recv_timeout(Duration::from_secs(5)).expect("a reply")
    }

    #[test]
    fn replies_carry_their_request_id_even_out_of_order() {
        let (bridge, rx) = bridge();
        bridge.handle(r#"{"id": 1, "method": "config.get"}"#);
        bridge.handle(r#"{"id": "two", "method": "config.put", "params": {"yaml": "repos: []"}}"#);
        let first = recv(&rx);
        let second = recv(&rx);
        // The fast put overtakes the slow get; ids still match their calls.
        assert_eq!(first.id, json!("two"));
        assert!(first.ok);
        assert_eq!(first.result.unwrap()["ok"], json!(true));
        assert_eq!(second.id, json!(1));
        assert_eq!(second.result.unwrap()["yaml"], json!("defaults: {}\n"));
    }

    #[test]
    fn rejections_keep_the_field_and_daemon_errors_propagate() {
        let (bridge, rx) = bridge();
        bridge.handle(
            r#"{"id": 3, "method": "config.put", "params": {"yaml": "defaults:\n  poll_interval_minutes: 0\n"}}"#,
        );
        let reply = recv(&rx);
        assert_eq!(reply.id, json!(3));
        assert!(!reply.ok);
        let error = reply.error.unwrap();
        assert_eq!(error.kind, "rejected");
        assert_eq!(
            error.field.as_deref(),
            Some("defaults.poll_interval_minutes")
        );

        bridge.handle(r#"{"id": 4, "method": "review_mode.set", "params": {"mode": "monitor"}}"#);
        let reply = recv(&rx);
        assert_eq!(reply.id, json!(4));
        assert_eq!(reply.error.unwrap().kind, "offline");

        let other = BridgeError::from_daemon(anyhow::anyhow!("daemon returned 500: boom"));
        assert_eq!(other.kind, "daemon");
        assert!(other.message.contains("boom"));
        assert_eq!(other.field, None);
    }

    #[test]
    fn malformed_messages_are_answered_without_reaching_the_backend() {
        let (bridge, rx) = bridge();
        for (body, id) in [
            ("not json", Value::Null),
            (r#"{"id": 5, "method": "config.delete"}"#, json!(5)),
            (
                r#"{"id": 6, "method": "config.put", "params": {}}"#,
                json!(6),
            ),
            (
                r#"{"id": 7, "method": "config.put", "params": {"config": {}, "yaml": "x"}}"#,
                json!(7),
            ),
            (
                r#"{"id": 8, "method": "review_mode.set", "params": {"mode": "loud"}}"#,
                json!(8),
            ),
            (r#"{"id": 9}"#, json!(9)),
        ] {
            // Malformed messages are answered synchronously.
            bridge.handle(body);
            let reply = rx.try_recv().expect("an immediate reply");
            assert_eq!(reply.id, id, "{body}");
            assert_eq!(reply.error.unwrap().kind, "bad_request", "{body}");
        }
    }

    #[test]
    fn calls_parse_into_the_daemon_routes() {
        assert_eq!(
            parse(r#"{"id":1,"method":"config.get"}"#).unwrap().1,
            Call::GetConfig
        );
        assert_eq!(
            parse(r#"{"id":1,"method":"config.put","params":{"config":{"repos":[]}}}"#)
                .unwrap()
                .1,
            Call::PutConfig(json!({"repos": []}))
        );
        assert_eq!(
            parse(r#"{"id":1,"method":"config.put","params":{"yaml":"a: 1"}}"#)
                .unwrap()
                .1,
            Call::PutConfigYaml("a: 1".into())
        );
        assert_eq!(
            parse(r#"{"id":1,"method":"review_mode.set","params":{"mode":"off"}}"#)
                .unwrap()
                .1,
            Call::SetReviewMode(ReviewMode::Off)
        );
        assert_eq!(
            parse(r#"{"id":1,"method":"discover.agents"}"#).unwrap().1,
            Call::DiscoverAgents
        );
        assert_eq!(
            parse(r#"{"id":1,"method":"discover.repos","params":{}}"#)
                .unwrap()
                .1,
            Call::DiscoverRepos
        );
    }

    /// 10.1: every Overview method routes to its daemon call with its
    /// query; bad params never reach the backend.
    #[test]
    fn overview_methods_route_to_their_daemon_calls() {
        let call = |body: &str| parse(body).map(|(_, call)| call);
        assert_eq!(
            call(r#"{"id":1,"method":"status.get","params":{}}"#).unwrap(),
            Call::GetStatus
        );
        assert_eq!(
            call(r#"{"id":1,"method":"history.list","params":{"limit":50,"repo":"acme/api","before":"2026-10-05T12:00:00+02:00"}}"#)
                .unwrap(),
            Call::ListHistory(
                "/history?limit=50&repo=acme/api&before=2026-10-05T12%3A00%3A00%2B02%3A00".into()
            )
        );
        assert_eq!(
            call(r#"{"id":1,"method":"history.list","params":{"limit":20}}"#).unwrap(),
            Call::ListHistory("/history?limit=20".into())
        );
        assert_eq!(
            call(r#"{"id":1,"method":"usage.get","params":{"bucket":"week","window":"30d","group":"engine","repo":""}}"#)
                .unwrap(),
            Call::GetUsage("/usage?bucket=week&window=30d&group=engine".into())
        );
        assert_eq!(
            call(r#"{"id":1,"method":"insights.get","params":{"days":30}}"#).unwrap(),
            Call::GetInsights("/insights?days=30".into())
        );
        assert_eq!(
            call(r#"{"id":1,"method":"insights.get","params":{}}"#).unwrap(),
            Call::GetInsights("/insights".into())
        );
        for body in [
            r#"{"id":1,"method":"history.list","params":{"limit":"ten"}}"#,
            r#"{"id":1,"method":"history.list","params":{"limit":0}}"#,
            r#"{"id":1,"method":"usage.get","params":{"bucket":7}}"#,
            r#"{"id":1,"method":"insights.get","params":{"days":-1}}"#,
        ] {
            assert_eq!(
                call(body).unwrap_err().error.unwrap().kind,
                "bad_request",
                "{body}"
            );
        }
    }

    /// 10.1: `review.request` carries the per-run overrides; blank text is
    /// absent and the target is required.
    #[test]
    fn review_request_carries_the_overrides() {
        let call = |body: &str| parse(body).map(|(_, call)| call);
        assert_eq!(
            call(r#"{"id":1,"method":"review.request","params":{"target":" 42 ","engine":"codex","post":false,"extra_prompt":" focus "}}"#)
                .unwrap(),
            Call::RequestReview {
                target: "42".into(),
                overrides: ReviewOverrides {
                    engine: Some("codex".into()),
                    post: Some(false),
                    extra_prompt: Some("focus".into()),
                },
            }
        );
        assert_eq!(
            call(r#"{"id":1,"method":"review.request","params":{"target":"acme/api#42","extra_prompt":"  "}}"#)
                .unwrap(),
            Call::RequestReview {
                target: "acme/api#42".into(),
                overrides: ReviewOverrides::default(),
            }
        );
        for body in [
            r#"{"id":1,"method":"review.request","params":{}}"#,
            r#"{"id":1,"method":"review.request","params":{"target":"  "}}"#,
            r#"{"id":1,"method":"review.request","params":{"target":"42","post":"no"}}"#,
            r#"{"id":1,"method":"review.request","params":{"target":"42","engine":3}}"#,
        ] {
            assert_eq!(
                call(body).unwrap_err().error.unwrap().kind,
                "bad_request",
                "{body}"
            );
        }
        // Through the bridge: the overrides reach the backend.
        let (bridge, rx) = bridge();
        bridge.handle(
            r#"{"id":"r","method":"review.request","params":{"target":"42","post":false}}"#,
        );
        let reply = recv(&rx);
        assert_eq!(reply.id, json!("r"));
        let result = reply.result.unwrap();
        assert_eq!(result["enqueued"], json!(["42"]));
        assert_eq!(result["note"], json!({"post": false}));
    }

    /// Task 4.3: `brief.request {key}` reaches the backend as
    /// `POST /brief`; a missing or blank key is a bad request, and a
    /// daemon refusal keeps its text.
    #[test]
    fn brief_request_posts_the_key() {
        let call = |body: &str| parse(body).map(|(_, call)| call);
        assert_eq!(
            call(r#"{"id":1,"method":"brief.request","params":{"key":" acme/api#42 "}}"#).unwrap(),
            Call::RequestBrief("acme/api#42".into())
        );
        assert_eq!(Call::RequestBrief("k".into()).method(), "brief.request");
        assert_eq!(
            call(r#"{"id":1,"method":"pause.resume"}"#).unwrap(),
            Call::Resume
        );
        assert_eq!(Call::Resume.method(), "pause.resume");
        for body in [
            r#"{"id":1,"method":"brief.request","params":{}}"#,
            r#"{"id":1,"method":"brief.request","params":{"key":"  "}}"#,
            r#"{"id":1,"method":"brief.request","params":{"key":42}}"#,
        ] {
            assert_eq!(
                call(body).unwrap_err().error.unwrap().kind,
                "bad_request",
                "{body}"
            );
        }
        let (bridge, rx) = bridge();
        bridge.handle(r#"{"id":"b","method":"brief.request","params":{"key":"acme/api#42"}}"#);
        let reply = recv(&rx);
        assert_eq!(reply.id, json!("b"));
        assert_eq!(
            reply.result.unwrap(),
            json!({"accepted": true, "key": "acme/api#42"})
        );
        bridge.handle(r#"{"id":"c","method":"brief.request","params":{"key":"acme/api#9"}}"#);
        let error = recv(&rx).error.unwrap();
        assert_eq!(error.kind, "daemon");
        assert_eq!(
            error.message,
            "a brief for acme/api#9 is already being written"
        );
    }

    /// `pause.start` takes exactly one of `hours` (positive) / `until`
    /// (RFC 3339) and reaches the backend as `POST /pause`; a daemon
    /// refusal keeps its text.
    #[test]
    fn pause_start_posts_hours_or_until() {
        let call = |body: &str| parse(body).map(|(_, call)| call);
        assert_eq!(
            call(r#"{"id":1,"method":"pause.start","params":{"hours":0.5}}"#).unwrap(),
            Call::Pause {
                hours: Some(0.5),
                until: None
            }
        );
        assert_eq!(
            call(
                r#"{"id":1,"method":"pause.start","params":{"until":"2026-10-08T18:00:00+02:00"}}"#
            )
            .unwrap(),
            Call::Pause {
                hours: None,
                until: Some("2026-10-08T16:00:00Z".parse().unwrap())
            }
        );
        assert_eq!(
            Call::Pause {
                hours: Some(1.0),
                until: None
            }
            .method(),
            "pause.start"
        );
        for body in [
            r#"{"id":1,"method":"pause.start","params":{}}"#,
            r#"{"id":1,"method":"pause.start"}"#,
            r#"{"id":1,"method":"pause.start","params":{"hours":1,"until":"2026-10-08T18:00:00Z"}}"#,
            r#"{"id":1,"method":"pause.start","params":{"hours":0}}"#,
            r#"{"id":1,"method":"pause.start","params":{"hours":-2}}"#,
            r#"{"id":1,"method":"pause.start","params":{"hours":"2"}}"#,
            r#"{"id":1,"method":"pause.start","params":{"until":"tomorrow"}}"#,
        ] {
            assert_eq!(
                call(body).unwrap_err().error.unwrap().kind,
                "bad_request",
                "{body}"
            );
        }
        let (bridge, rx) = bridge();
        bridge.handle(r#"{"id":"p","method":"pause.start","params":{"hours":2}}"#);
        let reply = recv(&rx);
        assert_eq!(reply.id, json!("p"));
        assert_eq!(
            reply.result.unwrap(),
            json!({"paused_until": "2026-10-08T12:00:00Z", "hours": 2.0, "until": null})
        );
        bridge.handle(r#"{"id":"q","method":"pause.start","params":{"hours":500}}"#);
        let error = recv(&rx).error.unwrap();
        assert_eq!(error.kind, "daemon");
        assert_eq!(error.message, "pause end must be in the future");
    }

    /// D14: a 404 from `/usage` is `unsupported`; other failures keep the
    /// daemon's text.
    #[test]
    fn usage_404_maps_to_unsupported() {
        let (bridge, rx) = bridge();
        bridge.handle(r#"{"id":2,"method":"usage.get","params":{"bucket":"day"}}"#);
        let reply = recv(&rx);
        assert_eq!(reply.id, json!(2));
        assert_eq!(reply.error.unwrap().kind, "unsupported");

        let bad = http_reply(
            "/usage?bucket=hour",
            400,
            br#"{"error":"bucket must be one of day, week, month"}"#,
        )
        .unwrap_err();
        assert_eq!(bad.kind, "daemon");
        assert_eq!(bad.message, "bucket must be one of day, week, month");
        // A 404 elsewhere is not "update the daemon".
        assert_eq!(http_reply("/history", 404, b"").unwrap_err().kind, "daemon");
        assert_eq!(
            http_reply("/status", 200, br#"{"prs":[]}"#).unwrap(),
            json!({"prs": []})
        );
        assert_eq!(
            http_reply("/status", 200, b"<html>").unwrap_err().kind,
            "daemon"
        );
    }

    #[test]
    fn http_responses_parse_plain_and_chunked() {
        let plain = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}";
        assert_eq!(parse_http_response(plain).unwrap(), (200, b"{}".to_vec()));
        let chunked = b"HTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert_eq!(
            parse_http_response(chunked).unwrap(),
            (404, br#"{"a":1}"#.to_vec())
        );
        assert!(parse_http_response(b"HTTP/1.1 200 OK\r\n").is_err());
        assert!(parse_http_response(b"garbage\r\n\r\n").is_err());
    }

    /// The pass-through GET against a loopback "daemon": the token goes in
    /// the Authorization header (never in the reply), the JSON comes back
    /// verbatim, and a 404 from `/usage` is `unsupported`.
    #[test]
    fn loopback_get_authenticates_and_passes_json_through() {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("prx-bridge-{}-{port}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("daemon.port"), port.to_string()).unwrap();
        std::fs::write(dir.join("daemon.token"), "s3cret").unwrap();
        let (tx, seen) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = Vec::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    head.push(line.trim().to_string());
                }
                let (status, body) = if head[0].starts_with("GET /usage") {
                    ("404 Not Found", String::new())
                } else {
                    (
                        "200 OK",
                        r#"{"entries":[{"event":"review_completed"}]}"#.to_string(),
                    )
                };
                let _ = tx.send(head);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (history, usage) = crate::actions::tests::with_data_dir(&dir, || {
            (
                runtime.block_on(loopback_get("/history?limit=5")),
                runtime.block_on(loopback_get("/usage")),
            )
        });
        assert_eq!(
            history.unwrap(),
            json!({"entries": [{"event": "review_completed"}]})
        );
        assert_eq!(usage.unwrap_err().kind, "unsupported");
        let head = seen.recv().unwrap();
        assert_eq!(head[0], "GET /history?limit=5 HTTP/1.1");
        assert!(
            head.iter().any(|h| h == "Authorization: Bearer s3cret"),
            "{head:?}"
        );
        assert!(head.iter().any(|h| h.starts_with("x-prx-client: menubar/")));
    }

    #[test]
    fn replies_render_as_a_resolve_call() {
        let script = Response::success(json!(1), json!({"a": "</script>"})).to_script();
        assert!(script.starts_with("window.prxBridge && window.prxBridge.__resolve({"));
        assert!(script.contains(r#""id":1"#));
        assert!(script.ends_with(");"));
    }
}
