//! Agent adapter layer (DEV-PLAN §0/§7): the coding agent is READ-ONLY — it
//! analyzes and writes a JSON review manifest; it never posts to GitHub.
//! [`CommandAdapter`] spawns the configured agent command ({{PROMPT}}
//! substitution or stdin), enforces the wall-clock timeout with kill, and
//! validates the manifest strictly. [`EngineAdapter`] dispatches each run
//! to the repo's engine type: agent entries to [`CommandAdapter`], `llm_api`
//! entries to [`crate::llm::LlmApiAdapter`] (design D1/D2).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{
    AgentInvocation, EffectiveRepo, ResolvedEngine, SessionOutput, SESSION_ID_PLACEHOLDER,
};
use crate::llm::{LlmApiAdapter, LlmTransport, ReqwestTransport};
use crate::state::{PrRecord, Verdict};
use crate::usage::{self, Mismatch, TokenUsage};

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent binary not found: {0}")]
    BinaryNotFound(String),
    #[error("agent timed out after {timeout}s: {command}")]
    Timeout { timeout: u64, command: String },
    #[error("agent command failed ({command}): {stderr}")]
    CommandFailed { command: String, stderr: String },
    #[error("agent wrote no review manifest at {path}")]
    ManifestMissing { path: PathBuf },
    #[error("invalid review manifest: {0}")]
    Manifest(String),
    #[error("agent configuration is invalid: {0}")]
    Config(String),
    #[error("LLM API key missing: environment variable {0} is not set")]
    ApiKeyMissing(String),
    #[error("LLM API returned HTTP {status}: {detail}")]
    LlmHttp { status: u16, detail: String },
    #[error("LLM API request failed: {0}")]
    LlmTransport(String),
    #[error("LLM API response unusable: {0}")]
    LlmResponse(String),
    #[error("agent io error: {0}")]
    Io(#[from] std::io::Error),
    /// A failure after the engine already reported token usage (design
    /// D11: a failed run still records what it consumed). Displays as the
    /// underlying error.
    #[error("{error}")]
    WithUsage {
        error: Box<AgentError>,
        usage: Box<TokenUsage>,
    },
}

impl AgentError {
    /// Attach the usage the failed run reported; `None` keeps the error as
    /// it is.
    pub fn with_usage(self, usage: Option<TokenUsage>) -> AgentError {
        match (self, usage) {
            (AgentError::WithUsage { error, usage: had }, Some(more)) => AgentError::WithUsage {
                error,
                usage: Box::new(TokenUsage::combine(Some(*had), Some(more)).unwrap_or_default()),
            },
            (error, Some(usage)) => AgentError::WithUsage {
                error: Box::new(error),
                usage: Box::new(usage),
            },
            (error, None) => error,
        }
    }

    /// The usage a failed run reported, when any.
    pub fn usage(&self) -> Option<&TokenUsage> {
        match self {
            AgentError::WithUsage { usage, .. } => Some(usage),
            _ => None,
        }
    }

    /// The underlying error, without attached usage.
    pub fn cause(&self) -> &AgentError {
        match self {
            AgentError::WithUsage { error, .. } => error.cause(),
            other => other,
        }
    }
}

/// Finding severity in the review manifest (DEV-PLAN §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Blocking,
    Nit,
}

/// One finding from the review manifest. `file` may be absent for
/// repo-wide observations (those are not posted as inline comments).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFinding {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    pub severity: Severity,
    pub body: String,
}

/// The machine-readable review manifest (DEV-PLAN §7, audit A3). Extra keys
/// are tolerated; the documented fields are validated strictly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewManifest {
    pub manifest_version: u32,
    pub verdict: Verdict,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<ManifestFinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u64>,
    /// Optional plain-language brief of the PR (design B2), additive in
    /// manifest v1. Lenient: a non-string value is ignored (with a warning)
    /// rather than failing the review, since the verdict never depends on
    /// the brief.
    #[serde(
        default,
        deserialize_with = "deserialize_lenient_brief",
        skip_serializing_if = "Option::is_none"
    )]
    pub brief: Option<String>,
}

/// `brief` as a string, else `None` (null, missing, or any other type).
fn deserialize_lenient_brief<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(text) => Some(text),
        serde_json::Value::Null => None,
        other => {
            tracing::warn!(
                "manifest `brief` is not a string ({}); ignoring it",
                match other {
                    serde_json::Value::Bool(_) => "a boolean",
                    serde_json::Value::Number(_) => "a number",
                    serde_json::Value::Array(_) => "an array",
                    _ => "an object",
                }
            );
            None
        }
    })
}

impl ReviewManifest {
    /// The manifest's brief, trimmed; `None` when absent or blank.
    pub fn brief_text(&self) -> Option<&str> {
        self.brief
            .as_deref()
            .map(str::trim)
            .filter(|b| !b.is_empty())
    }

    pub fn blocking_count(&self) -> u32 {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Blocking)
            .count() as u32
    }

    pub fn nit_count(&self) -> u32 {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Nit)
            .count() as u32
    }
}

/// Parse + STRICT validation. Rules (DEV-PLAN §7):
/// - `manifest_version` must be 1,
/// - `verdict` and finding severities must be known enum values,
/// - a finding's `file` must be non-empty when present,
/// - verdict consistency: any blocking finding forces `changes_requested`
///   (normalized with a warning, not an error).
pub fn parse_manifest(raw: &str) -> Result<ReviewManifest, AgentError> {
    let manifest: ReviewManifest = serde_json::from_str(raw)
        .map_err(|e| AgentError::Manifest(format!("parse failed: {e}")))?;
    if manifest.manifest_version != 1 {
        return Err(AgentError::Manifest(format!(
            "unsupported manifest_version {} (expected 1)",
            manifest.manifest_version
        )));
    }
    for finding in &manifest.findings {
        if finding.file.as_deref().is_some_and(|f| f.trim().is_empty()) {
            return Err(AgentError::Manifest(
                "finding has an empty file path (omit `file` instead)".to_string(),
            ));
        }
    }
    let mut manifest = manifest;
    if manifest.blocking_count() > 0 && manifest.verdict != Verdict::ChangesRequested {
        tracing::warn!(
            "manifest verdict '{:?}' is inconsistent with {} blocking finding(s); \
             normalizing to changes_requested",
            manifest.verdict,
            manifest.blocking_count()
        );
        manifest.verdict = Verdict::ChangesRequested;
    }
    Ok(manifest)
}

/// One successful adapter run: the validated manifest plus an optional
/// adapter-specific session reference captured from the agent's stdout
/// (best-effort; spec §5.3) and the token usage the engine reported
/// (design D11; `None` = unknown).
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRun {
    pub manifest: ReviewManifest,
    pub session: Option<String>,
    pub usage: Option<TokenUsage>,
}

/// A free-form prompt's reply (e.g. the executive summary) and the token
/// usage the engine reported for it (design D12).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromptReply {
    pub text: String,
    pub usage: Option<TokenUsage>,
}

/// Delivery of a review run. Async-fn-in-trait (like [`crate::github::Gh`]):
/// generic-friendly, not dyn-compatible — callers use generics.
pub trait AgentAdapter: Send + Sync {
    fn run_review(
        &self,
        pr: &PrRecord,
        repo: &EffectiveRepo,
        prompt: &str,
        manifest_path: &Path,
    ) -> impl std::future::Future<Output = Result<AgentRun, AgentError>> + Send;

    /// Run a free-form prompt through `engine` and return its text reply
    /// (e.g. the executive summary, design D8). Adapters that only run
    /// reviews keep this default, which reports the prompt as unsupported.
    fn run_prompt(
        &self,
        engine: &ResolvedEngine,
        prompt: &str,
        timeout_secs: u64,
    ) -> impl std::future::Future<Output = Result<PromptReply, AgentError>> + Send {
        let _ = (engine, prompt, timeout_secs);
        async {
            Err(AgentError::Config(
                "this adapter cannot run free-form prompts".to_string(),
            ))
        }
    }
}

/// Build the concrete argv from the configured template. `{{PROMPT}}` is
/// substituted; a bare `-` arg means the prompt is piped on stdin instead
/// (DEV-PLAN §4). An agent that would never receive the prompt is a config
/// error.
pub fn build_args(template: &[String], prompt: &str) -> Result<(Vec<String>, bool), AgentError> {
    let stdin_mode = template.iter().any(|a| a == "-");
    let args: Vec<String> = template
        .iter()
        .filter(|a| a.as_str() != "-")
        .map(|a| a.replace("{{PROMPT}}", prompt))
        .collect();
    let substituted = template.iter().any(|a| a.contains("{{PROMPT}}"));
    if !stdin_mode && !substituted {
        return Err(AgentError::Config(
            "agent args contain neither '-' nor {{PROMPT}}: the agent would never \
             receive the review prompt"
                .to_string(),
        ));
    }
    Ok((args, stdin_mode))
}

/// The default session convention: the agent may print a line
/// `prx-session: <ref>` on stdout; prx stores it for "open review session".
pub const DEFAULT_SESSION_PREFIX: &str = "prx-session:";

/// Capture a session reference from a run's stdout (design D3): a line
/// prefix (default `prx-session:`), or a top-level field of the agent's
/// JSON output — either one JSON document or JSON Lines, first match wins.
pub fn capture_session(stdout: &str, output: Option<&SessionOutput>) -> Option<String> {
    let json_field = output
        .and_then(|o| o.json_field.as_deref())
        .map(str::trim)
        .filter(|f| !f.is_empty());
    if let Some(field) = json_field {
        let from_value = |v: &serde_json::Value| match v.get(field)? {
            serde_json::Value::String(s) => Some(s.trim().to_string()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            _ => None,
        };
        let whole = serde_json::from_str::<serde_json::Value>(stdout.trim())
            .ok()
            .and_then(|v| from_value(&v));
        return whole
            .or_else(|| {
                stdout.lines().find_map(|line| {
                    serde_json::from_str::<serde_json::Value>(line.trim())
                        .ok()
                        .and_then(|v| from_value(&v))
                })
            })
            .filter(|s| !s.is_empty());
    }
    let prefix = output
        .and_then(|o| o.stdout_prefix.as_deref())
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(DEFAULT_SESSION_PREFIX);
    stdout
        .lines()
        .find_map(|l| l.trim_start().strip_prefix(prefix))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The token usage an agent run reported on stdout (design D11): an
/// explicit `usage_output` mapping wins; otherwise the built-in parsing for
/// a `claude` / `codex` command (by basename); otherwise unknown. Never
/// fails: unusable output is unknown usage.
pub fn capture_usage(stdout: &str, invocation: &AgentInvocation) -> Option<TokenUsage> {
    if let Some(mapping) = &invocation.usage_output {
        return usage::parse_mapped(stdout, mapping);
    }
    let basename = Path::new(&invocation.command)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    match basename {
        "claude" => claude_usage(stdout),
        "codex" => codex_usage(stdout),
        _ => None,
    }
}

/// Built-in `claude --output-format json`: the result object's `usage`
/// counts, `total_cost_usd`, `session_id`, and the model from `modelUsage`
/// (the one model, or the costliest when several ran).
pub fn claude_usage(stdout: &str) -> Option<TokenUsage> {
    let values = usage::stdout_json_values(stdout);
    let result = values
        .iter()
        .rev()
        .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("result"))
        .or_else(|| values.last())?;
    let read = || -> Result<TokenUsage, Mismatch> {
        let model = match result.get("modelUsage").and_then(|m| m.as_object()) {
            Some(models) => models
                .iter()
                .max_by(|(_, a), (_, b)| {
                    let cost = |v: &serde_json::Value| {
                        v.get("costUSD").and_then(|c| c.as_f64()).unwrap_or(0.0)
                    };
                    cost(a).total_cmp(&cost(b))
                })
                .map(|(name, _)| name.clone()),
            None => usage::text_at(result, "model")?,
        };
        Ok(TokenUsage {
            input_tokens: usage::count_at(result, "usage.input_tokens")?,
            output_tokens: usage::count_at(result, "usage.output_tokens")?,
            cache_read_tokens: usage::count_at(result, "usage.cache_read_input_tokens")?,
            cache_write_tokens: usage::count_at(result, "usage.cache_creation_input_tokens")?,
            cost_usd: usage::cost_at(result, "total_cost_usd")?,
            model,
            session_id: usage::text_at(result, "session_id")?,
        })
    };
    usage::known_or_log(read(), "claude")
}

/// Built-in `codex exec --json`: the sum of the `usage` of every
/// `turn.completed` event and the `thread_id` of `thread.started`. codex
/// counts `cached_input_tokens` inside `input_tokens`, so each turn is
/// normalized: uncached input → `input_tokens`, cached → cache reads.
pub fn codex_usage(stdout: &str) -> Option<TokenUsage> {
    let read = || -> Result<TokenUsage, Mismatch> {
        let mut total: Option<TokenUsage> = None;
        let mut thread = None;
        for event in usage::stdout_json_values(stdout) {
            match event.get("type").and_then(|t| t.as_str()) {
                Some("thread.started") => thread = usage::text_at(&event, "thread_id")?,
                Some("turn.completed") => {
                    // codex's `input_tokens` includes the cached part.
                    let turn = TokenUsage::from_inclusive_input(
                        usage::count_at(&event, "usage.input_tokens")?,
                        usage::count_at(&event, "usage.cached_input_tokens")?,
                        usage::count_at(&event, "usage.output_tokens")?,
                    );
                    total = TokenUsage::combine(total, Some(turn));
                }
                _ => {}
            }
        }
        Ok(total.unwrap_or_default().fill(None, thread.as_deref()))
    };
    usage::known_or_log(read(), "codex")
}

/// The argv template for one run (design D3): the agent's args, plus its
/// `resume_args` with `{{SESSION_ID}}` substituted when resuming.
pub fn run_template(invocation: &AgentInvocation, resume: Option<&str>) -> Vec<String> {
    let mut template = invocation.args.clone();
    if let Some(session) = resume {
        template.extend(
            invocation
                .resume_args
                .iter()
                .map(|a| a.replace(SESSION_ID_PLACEHOLDER, session)),
        );
    }
    template
}

/// What one spawned agent process produced. A non-zero exit is still an
/// output (its stdout may report usage); [`ProcessOutput::require_success`]
/// turns it into [`AgentError::CommandFailed`].
struct ProcessOutput {
    stdout: String,
    stderr: String,
    success: bool,
    command_label: String,
}

impl ProcessOutput {
    fn require_success(&self) -> Result<(), AgentError> {
        match self.success {
            true => Ok(()),
            false => Err(AgentError::CommandFailed {
                command: self.command_label.clone(),
                stderr: self.stderr.trim().to_string(),
            }),
        }
    }
}

/// Spawn `invocation` with the prompt (argv substitution or stdin), the
/// given extra env, and a wall-clock `timeout_secs` enforced with kill.
async fn run_process(
    invocation: &AgentInvocation,
    template: &[String],
    prompt: &str,
    envs: &[(&str, &std::ffi::OsStr)],
    timeout_secs: u64,
) -> Result<ProcessOutput, AgentError> {
    let (args, stdin_mode) = build_args(template, prompt)?;
    let command_label = format!("{} {}", invocation.command, args.join(" "));
    tracing::debug!(
        "running agent {}: prompt {} bytes (not logged)",
        command_label,
        prompt.len()
    );

    let mut cmd = tokio::process::Command::new(&invocation.command);
    cmd.args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Nothing must outlive a cancelled run (e.g. a dropped future).
        .kill_on_drop(true);
    for (key, value) in envs {
        cmd.env(key, value);
    }

    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AgentError::BinaryNotFound(invocation.command.clone())
        } else {
            AgentError::Io(e)
        }
    })?;

    if stdin_mode {
        use tokio::io::AsyncWriteExt;
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(prompt.as_bytes())
                .await
                .map_err(AgentError::Io)?;
            pipe.flush().await.map_err(AgentError::Io)?;
        }
    }
    // Always close stdin: in substitution mode the agent must not block
    // waiting for input it will never get.
    drop(child.stdin.take());

    // Drain stdout/stderr concurrently so a chatty agent cannot deadlock
    // on full pipes while we wait.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut p) = stdout_pipe {
            use tokio::io::AsyncReadExt;
            let _ = p.read_to_end(&mut buf).await;
        }
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut p) = stderr_pipe {
            use tokio::io::AsyncReadExt;
            let _ = p.read_to_end(&mut buf).await;
        }
        buf
    });

    let status = match tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        child.wait(),
    )
    .await
    {
        Ok(status) => status.map_err(AgentError::Io)?,
        Err(_elapsed) => {
            // DEV-PLAN §9: reviews run with timeout + kill. The reader
            // tasks are aborted rather than awaited: grandchildren that
            // survived the kill can hold the pipes open for a long time,
            // and waiting on them would defeat the timeout.
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(AgentError::Timeout {
                timeout: timeout_secs,
                command: command_label,
            });
        }
    };

    let stdout = String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()).into_owned();

    Ok(ProcessOutput {
        stdout,
        stderr,
        success: status.success(),
        command_label,
    })
}

/// The production adapter: spawns `command args...` via tokio::process with
/// the configured timeout, killing the process on expiry.
///
/// Session continuity (design D3): when the agent declares `resume_args`
/// and the PR carries a retained session, the run resumes it; if that
/// resumed run fails, the same call retries once as a fresh session, so a
/// broken or expired session never fails a review on its own.
#[derive(Debug, Clone, Default)]
pub struct CommandAdapter;

impl CommandAdapter {
    /// One spawn: run the agent, then read + validate the manifest file.
    async fn run_once(
        &self,
        pr: &PrRecord,
        invocation: &AgentInvocation,
        timeout_secs: u64,
        prompt: &str,
        manifest_path: &Path,
        resume: Option<&str>,
    ) -> Result<AgentRun, AgentError> {
        // Stale manifests from previous attempts must not leak into this run.
        let _ = std::fs::remove_file(manifest_path);
        let number = pr.number.to_string();
        // Convenience env vars so agents (and test fixtures) can locate the
        // manifest without scraping the prompt.
        let envs: [(&str, &std::ffi::OsStr); 3] = [
            ("PRX_MANIFEST_PATH", manifest_path.as_os_str()),
            ("PRX_PR_NUMBER", number.as_ref()),
            ("PRX_REPO", pr.repo.as_ref()),
        ];
        let template = run_template(invocation, resume);
        let output = run_process(invocation, &template, prompt, &envs, timeout_secs).await?;
        // Design D11: a run that fails after reporting usage still records it.
        let usage = capture_usage(&output.stdout, invocation);
        let failed = |e: AgentError| e.with_usage(usage.clone());
        output.require_success().map_err(failed)?;

        if !manifest_path.exists() {
            return Err(failed(AgentError::ManifestMissing {
                path: manifest_path.to_path_buf(),
            }));
        }
        let raw = std::fs::read_to_string(manifest_path).map_err(|e| failed(AgentError::Io(e)))?;
        let manifest = parse_manifest(&raw).map_err(failed)?;
        let session = capture_session(&output.stdout, invocation.session_output.as_ref())
            // A resumed run that reports no reference keeps the one it resumed.
            .or_else(|| resume.map(str::to_string));
        Ok(AgentRun {
            manifest,
            session,
            usage,
        })
    }
}

impl AgentAdapter for CommandAdapter {
    async fn run_review(
        &self,
        pr: &PrRecord,
        repo: &EffectiveRepo,
        prompt: &str,
        manifest_path: &Path,
    ) -> Result<AgentRun, AgentError> {
        // The LLM API path is a separate adapter (design D2); only agent-type
        // engines are spawned here.
        let invocation = repo.resolved_engine.as_agent().ok_or_else(|| {
            AgentError::Config(format!(
                "engine '{}' is an {} engine, not a command",
                repo.engine,
                repo.resolved_engine.kind()
            ))
        })?;
        tracing::debug!(
            "agent run for {}: manifest {}",
            pr.key(),
            manifest_path.display()
        );
        if let Some(parent) = manifest_path.parent() {
            std::fs::create_dir_all(parent).map_err(AgentError::Io)?;
        }
        let timeout = repo.agent_timeout_seconds;

        let resume = pr
            .review_session
            .as_deref()
            .filter(|_| invocation.can_resume());
        // Usage a failed resume attempt reported counts toward this run.
        let mut resume_usage = None;
        if let Some(session) = resume {
            match self
                .run_once(
                    pr,
                    invocation,
                    timeout,
                    prompt,
                    manifest_path,
                    Some(session),
                )
                .await
            {
                Ok(run) => return Ok(run),
                Err(e) => {
                    tracing::warn!(
                        "{}: resuming session {session} failed ({e}); retrying with a fresh session",
                        pr.key()
                    );
                    resume_usage = e.usage().cloned();
                }
            }
        }
        match self
            .run_once(pr, invocation, timeout, prompt, manifest_path, None)
            .await
        {
            Ok(run) => Ok(AgentRun {
                usage: TokenUsage::combine(resume_usage, run.usage),
                ..run
            }),
            Err(e) => Err(e.with_usage(resume_usage)),
        }
    }
}

/// Engine dispatch (design D1/D2): agent entries run through
/// [`CommandAdapter`], `llm_api` entries through [`LlmApiAdapter`]. The
/// pipeline's retry-once and failure semantics are identical for both
/// because both return [`AgentRun`] / [`AgentError`]. Generic over the LLM
/// transport so tests inject canned responses.
#[derive(Debug, Clone, Default)]
pub struct EngineAdapter<T = ReqwestTransport> {
    pub command: CommandAdapter,
    pub llm: LlmApiAdapter<T>,
}

impl<T: LlmTransport> EngineAdapter<T> {
    /// Dispatch with an injected LLM transport (test seam).
    pub fn with_transport(transport: T) -> Self {
        EngineAdapter {
            command: CommandAdapter,
            llm: LlmApiAdapter::new(transport),
        }
    }

    /// Run a free-form prompt through `engine` and return its text reply,
    /// without the review-manifest contract (e.g. the executive summary).
    /// `llm_api`: one completion with no system prompt. Agent: one spawn,
    /// fresh session, stdout as the reply — for agents in JSON output mode
    /// (e.g. built-in `claude`), the top-level `result` string is used.
    /// The reply carries the usage the engine reported (design D12).
    pub async fn run_prompt(
        &self,
        engine: &ResolvedEngine,
        prompt: &str,
        timeout_secs: u64,
    ) -> Result<PromptReply, AgentError> {
        match engine {
            ResolvedEngine::LlmApi(cfg) => {
                let (text, usage) = self
                    .llm
                    .complete_with_usage(cfg, None, prompt, timeout_secs)
                    .await?;
                Ok(PromptReply { text, usage })
            }
            ResolvedEngine::Agent(invocation) => {
                let template = run_template(invocation, None);
                let output = run_process(invocation, &template, prompt, &[], timeout_secs).await?;
                let usage = capture_usage(&output.stdout, invocation);
                output
                    .require_success()
                    .map_err(|e| e.with_usage(usage.clone()))?;
                Ok(PromptReply {
                    text: agent_text_reply(&output.stdout),
                    usage,
                })
            }
        }
    }
}

/// An agent's text reply: the `result` field of a single JSON document on
/// stdout (JSON output mode), else stdout itself; trimmed.
fn agent_text_reply(stdout: &str) -> String {
    serde_json::from_str::<serde_json::Value>(stdout.trim())
        .ok()
        .and_then(|v| v.get("result")?.as_str().map(str::to_string))
        .unwrap_or_else(|| stdout.to_string())
        .trim()
        .to_string()
}

impl<T: LlmTransport> AgentAdapter for EngineAdapter<T> {
    async fn run_review(
        &self,
        pr: &PrRecord,
        repo: &EffectiveRepo,
        prompt: &str,
        manifest_path: &Path,
    ) -> Result<AgentRun, AgentError> {
        match &repo.resolved_engine {
            ResolvedEngine::Agent(_) => {
                self.command
                    .run_review(pr, repo, prompt, manifest_path)
                    .await
            }
            ResolvedEngine::LlmApi(_) => self.llm.run_review(pr, repo, prompt, manifest_path).await,
        }
    }

    async fn run_prompt(
        &self,
        engine: &ResolvedEngine,
        prompt: &str,
        timeout_secs: u64,
    ) -> Result<PromptReply, AgentError> {
        EngineAdapter::run_prompt(self, engine, prompt, timeout_secs).await
    }
}

/// A minimal tracked record for unit tests across modules.
#[cfg(test)]
pub(crate) fn test_pr() -> PrRecord {
    use chrono::Utc;
    PrRecord {
        repo: "org/repo".into(),
        number: 7,
        title: "t".into(),
        author: "a".into(),
        head_branch: "b".into(),
        head_sha: "s".into(),
        is_draft: false,
        gh_created_at: Utc::now(),
        state: crate::state::PrState::InReview,
        previous_state: None,
        skip_reason: None,
        snooze_until: None,
        snooze_until_push: false,
        review_anchor_at: None,
        last_reviewed_sha: None,
        last_review_at: None,
        last_verdict: None,
        last_summary: None,
        blocking_count: 0,
        nit_count: 0,
        review_session: None,
        review_attempts: 0,
        diff_too_large: false,
        human_approved_at: None,
        exec_summary: None,
        brief: None,
        updated_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_valid_manifest_and_ignores_extra_keys() {
        let raw = r#"{
            "manifest_version": 1,
            "verdict": "approved",
            "summary": "3 nits, no blockers",
            "findings": [
                {"file": "src/x.rs", "line": 12, "severity": "nit", "body": "rename this"},
                {"severity": "nit", "body": "repo-wide: consider CI"}
            ],
            "duration_seconds": 95,
            "future_field": {"nested": true}
        }"#;
        let m = parse_manifest(raw).unwrap();
        assert_eq!(m.manifest_version, 1);
        assert_eq!(m.verdict, Verdict::Approved);
        assert_eq!(m.summary, "3 nits, no blockers");
        assert_eq!(m.findings.len(), 2);
        assert_eq!(m.findings[0].file.as_deref(), Some("src/x.rs"));
        assert_eq!(m.findings[0].line, Some(12));
        assert_eq!(m.blocking_count(), 0);
        assert_eq!(m.nit_count(), 2);
        assert_eq!(m.duration_seconds, Some(95));
    }

    /// Design B2: `brief` is an optional, additive v1 field.
    #[test]
    fn parse_accepts_manifests_with_and_without_brief() {
        let without = parse_manifest(r#"{"manifest_version": 1, "verdict": "approved"}"#).unwrap();
        assert_eq!(without.brief, None);
        assert_eq!(without.brief_text(), None);
        assert!(!serde_json::to_string(&without).unwrap().contains("brief"));

        let with = parse_manifest(
            r#"{"manifest_version": 1, "verdict": "approved",
                "brief": "  This adds rate limiting to the login endpoint.\n"}"#,
        )
        .unwrap();
        assert_eq!(with.manifest_version, 1);
        assert_eq!(
            with.brief_text(),
            Some("This adds rate limiting to the login endpoint.")
        );

        for raw in [
            r#"{"manifest_version": 1, "verdict": "approved", "brief": "   "}"#,
            r#"{"manifest_version": 1, "verdict": "approved", "brief": null}"#,
            r#"{"manifest_version": 1, "verdict": "approved", "brief": 42}"#,
            r#"{"manifest_version": 1, "verdict": "approved", "brief": ["a"]}"#,
        ] {
            let m = parse_manifest(raw).unwrap();
            assert_eq!(m.brief_text(), None, "{raw}");
        }
    }

    #[test]
    fn parse_rejects_wrong_manifest_version() {
        let err = parse_manifest(r#"{"manifest_version": 2, "verdict": "approved"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unsupported manifest_version 2"), "{err}");
    }

    #[test]
    fn parse_rejects_unknown_verdict_and_severity() {
        let err = parse_manifest(r#"{"manifest_version": 1, "verdict": "ship_it"}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid review manifest"), "{err}");

        let err = parse_manifest(
            r#"{"manifest_version": 1, "verdict": "approved",
                "findings": [{"severity": "meh", "body": "x"}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("invalid review manifest"), "{err}");
    }

    #[test]
    fn parse_rejects_empty_file_path() {
        let err = parse_manifest(
            r#"{"manifest_version": 1, "verdict": "approved",
                "findings": [{"file": "  ", "severity": "nit", "body": "x"}]}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("empty file path"), "{err}");
    }

    #[test]
    fn blocking_findings_normalize_inconsistent_verdict() {
        let raw = r#"{
            "manifest_version": 1,
            "verdict": "approved",
            "summary": "oops",
            "findings": [{"file": "a.rs", "line": 1, "severity": "blocking", "body": "sql injection"}]
        }"#;
        let m = parse_manifest(raw).unwrap();
        assert_eq!(m.verdict, Verdict::ChangesRequested); // normalized + warned
        assert_eq!(m.blocking_count(), 1);
    }

    #[test]
    fn changes_requested_without_blockings_is_kept() {
        // A reviewer may request changes without any blocking finding.
        let raw = r#"{"manifest_version": 1, "verdict": "changes_requested",
                      "findings": [{"severity": "nit", "body": "x"}]}"#;
        let m = parse_manifest(raw).unwrap();
        assert_eq!(m.verdict, Verdict::ChangesRequested);
    }

    #[test]
    fn summary_and_findings_default_to_empty() {
        let m = parse_manifest(r#"{"manifest_version": 1, "verdict": "comment_only"}"#).unwrap();
        assert!(m.summary.is_empty());
        assert!(m.findings.is_empty());
        assert_eq!(m.duration_seconds, None);
    }

    #[test]
    fn build_args_substitutes_prompt_or_uses_stdin() {
        let template = vec![
            "--yes".to_string(),
            "-p".to_string(),
            "{{PROMPT}}".to_string(),
        ];
        let (args, stdin) = build_args(&template, "review PR 1").unwrap();
        assert_eq!(args, vec!["--yes", "-p", "review PR 1"]);
        assert!(!stdin);

        let template = vec!["exec".to_string(), "-".to_string()];
        let (args, stdin) = build_args(&template, "review PR 1").unwrap();
        assert_eq!(args, vec!["exec"]);
        assert!(stdin);

        // Embedded substitution also works.
        let template = vec!["-p {{PROMPT}} --quiet".to_string()];
        let (args, _) = build_args(&template, "hi").unwrap();
        assert_eq!(args, vec!["-p hi --quiet"]);
    }

    #[test]
    fn build_args_rejects_template_without_prompt_input() {
        let template = vec!["--flag".to_string()];
        let err = build_args(&template, "p").unwrap_err();
        assert!(matches!(err, AgentError::Config(_)));
        assert!(err.to_string().contains("neither '-' nor {{PROMPT}}"));
    }

    #[test]
    fn session_ref_is_parsed_from_stdout_prefix() {
        let parse = |out: &str| capture_session(out, None);
        assert_eq!(
            parse("working...\nprx-session:  abc-123 \ndone"),
            Some("abc-123".to_string())
        );
        assert_eq!(parse("no session here"), None);
        assert_eq!(parse("prx-session:   "), None);

        // A custom prefix replaces the default one.
        let custom = SessionOutput {
            stdout_prefix: Some("session=".into()),
            json_field: None,
        };
        assert_eq!(
            capture_session("x\nsession=s-9\nprx-session: no", Some(&custom)),
            Some("s-9".to_string())
        );
    }

    #[test]
    fn session_ref_is_parsed_from_a_json_field() {
        let field = |f: &str| SessionOutput {
            stdout_prefix: None,
            json_field: Some(f.into()),
        };
        // One JSON document (claude --output-format json).
        let doc = r#"{"type":"result","result":"done","session_id":"c-1"}"#;
        assert_eq!(
            capture_session(doc, Some(&field("session_id"))),
            Some("c-1".to_string())
        );
        // JSON Lines (codex exec --json): the first line carrying the field.
        let jsonl = "{\"type\":\"thread.started\",\"thread_id\":\"t-2\"}\nnot json\n{\"thread_id\":\"t-3\"}\n";
        assert_eq!(
            capture_session(jsonl, Some(&field("thread_id"))),
            Some("t-2".to_string())
        );
        // Missing field, non-JSON output, or the prefix line: nothing.
        assert_eq!(capture_session(doc, Some(&field("thread_id"))), None);
        assert_eq!(
            capture_session("prx-session: p-1", Some(&field("session_id"))),
            None
        );
    }

    fn invocation(resume_args: &[&str]) -> AgentInvocation {
        AgentInvocation {
            command: "agent".into(),
            args: vec!["-p".into(), "{{PROMPT}}".into()],
            session_output: None,
            resume_args: resume_args.iter().map(|a| a.to_string()).collect(),
            usage_output: None,
        }
    }

    #[test]
    fn resume_args_are_appended_with_the_session_id() {
        let inv = invocation(&["--resume", "{{SESSION_ID}}"]);
        assert!(inv.can_resume());
        assert_eq!(run_template(&inv, None), ["-p", "{{PROMPT}}"]);
        let template = run_template(&inv, Some("sess-1"));
        assert_eq!(template, ["-p", "{{PROMPT}}", "--resume", "sess-1"]);
        // The full prompt (with any per-run instructions) reaches the
        // resumed run exactly as it reaches a fresh one.
        let (args, stdin) = build_args(&template, "PROMPT + extra").unwrap();
        assert_eq!(args, ["-p", "PROMPT + extra", "--resume", "sess-1"]);
        assert!(!stdin);

        // Built-in codex: the stdin marker drops out, so `exec resume <id>`
        // reads as a subcommand with the prompt still on stdin.
        let codex = crate::config::Config::default()
            .resolved_engine("codex", Some("gpt-5"))
            .unwrap();
        let codex = codex.as_agent().unwrap();
        let (args, stdin) = build_args(&run_template(codex, Some("t-1")), "p").unwrap();
        assert_eq!(args, ["exec", "--json", "-m", "gpt-5", "resume", "t-1"]);
        assert!(stdin);
        let claude = crate::config::Config::default()
            .resolved_engine("claude", None)
            .unwrap();
        let (args, _) =
            build_args(&run_template(claude.as_agent().unwrap(), Some("c-1")), "p").unwrap();
        assert_eq!(
            &args[args.len() - 4..],
            ["--output-format", "json", "--resume", "c-1"]
        );

        // No resume_args: the engine cannot resume.
        assert!(!invocation(&[]).can_resume());
    }

    #[test]
    fn agent_text_reply_prefers_a_json_result_field() {
        assert_eq!(
            agent_text_reply("{\"result\": \" summary \", \"session_id\": \"x\"}\n"),
            "summary"
        );
        assert_eq!(agent_text_reply("  plain text\n"), "plain text");
    }

    /// Write an executable shell script and return its path.
    #[cfg(unix)]
    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(dir).unwrap();
        let path = dir.join("agent.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn repo_for(command: &Path, resume: bool) -> EffectiveRepo {
        let mut config = crate::config::Config::default();
        let yaml = format!(
            "type: agent\ncommand: {}\nargs: [\"{{{{PROMPT}}}}\"]\n{}",
            command.display(),
            if resume {
                "resume_args: [\"--resume\", \"{{SESSION_ID}}\"]\n"
            } else {
                ""
            }
        );
        config
            .engines
            .insert("t".into(), serde_yaml::from_str(&yaml).unwrap());
        config.defaults.engine = "t".into();
        config
            .effective_repo(&crate::config::RepoConfig {
                name: "org/repo".into(),
                ..Default::default()
            })
            .unwrap()
    }

    /// Design D3: a failed resumed run falls back to a fresh session inside
    /// the same call; non-resume engines never see the session.
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_resume_falls_back_to_a_fresh_session() {
        let dir = std::env::temp_dir().join(format!("prx-agent-resume-{}", std::process::id()));
        let log = dir.join("argv.log");
        let manifest = r#"{"manifest_version": 1, "verdict": "approved"}"#;
        let agent = script(
            &dir,
            &format!(
                "echo \"$*\" >> {log}\ncase \"$*\" in *--resume*) exit 3;; esac\n\
                 printf '%s' '{manifest}' > \"$PRX_MANIFEST_PATH\"\necho 'prx-session: fresh-1'\n",
                log = log.display()
            ),
        );
        let mut pr = test_pr();
        pr.review_session = Some("old-1".into());
        let path = dir.join("m.json");

        let run = CommandAdapter
            .run_review(&pr, &repo_for(&agent, true), "PROMPT", &path)
            .await
            .unwrap();
        assert_eq!(run.session.as_deref(), Some("fresh-1"));
        let argv = std::fs::read_to_string(&log).unwrap();
        assert_eq!(argv, "PROMPT --resume old-1\nPROMPT\n");

        // Without resume_args the retained session is ignored.
        std::fs::remove_file(&log).unwrap();
        CommandAdapter
            .run_review(&pr, &repo_for(&agent, false), "PROMPT", &path)
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "PROMPT\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A successful resumed run that reports no new reference keeps the
    /// resumed one.
    #[cfg(unix)]
    #[tokio::test]
    async fn resumed_run_without_a_reported_session_keeps_the_resumed_one() {
        let dir = std::env::temp_dir().join(format!("prx-agent-keep-{}", std::process::id()));
        let manifest = r#"{"manifest_version": 1, "verdict": "approved"}"#;
        let agent = script(
            &dir,
            &format!("printf '%s' '{manifest}' > \"$PRX_MANIFEST_PATH\"\n"),
        );
        let mut pr = test_pr();
        pr.review_session = Some("old-2".into());
        let run = CommandAdapter
            .run_review(&pr, &repo_for(&agent, true), "P", &dir.join("m.json"))
            .await
            .unwrap();
        assert_eq!(run.session.as_deref(), Some("old-2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_prompt_returns_agent_stdout_text() {
        let dir = std::env::temp_dir().join(format!("prx-agent-text-{}", std::process::id()));
        let agent = script(&dir, "echo \"summary of: $1\"\n");
        let repo = repo_for(&agent, false);
        let text = EngineAdapter::<ReqwestTransport>::default()
            .run_prompt(&repo.resolved_engine, "the PR", 10)
            .await
            .unwrap();
        assert_eq!(text.text, "summary of: the PR");
        assert_eq!(text.usage, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    const CLAUDE_RESULT: &str = include_str!("../tests/fixtures/usage/claude-result.json");
    const CODEX_TWO_TURNS: &str = include_str!("../tests/fixtures/usage/codex-two-turns.jsonl");
    const CLAUDE_MALFORMED: &str =
        include_str!("../tests/fixtures/usage/claude-malformed-usage.json");

    /// Design D11: the built-in claude result's usage, cost, session, and
    /// the costliest model of `modelUsage`.
    #[test]
    fn claude_result_fixture_gives_counts_and_cost() {
        let u = claude_usage(CLAUDE_RESULT).unwrap();
        assert_eq!(u.input_tokens, Some(38));
        assert_eq!(u.output_tokens, Some(4211));
        assert_eq!(u.cache_read_tokens, Some(402_118));
        assert_eq!(u.cache_write_tokens, Some(18_234));
        assert_eq!(u.cost_usd, Some(0.4123571));
        assert_eq!(u.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(
            u.session_id.as_deref(),
            Some("8f1c2a4e-5b7d-4c1e-9a3f-2d6e8b0c4f17")
        );
    }

    /// Spec "Codex multi-turn sum": turns reporting 1000 + 500 input
    /// tokens (640 + 384 of them cached) sum to 1500 input tokens, stored
    /// normalized as 476 uncached input + 1024 cache reads.
    #[test]
    fn codex_turns_are_summed() {
        let u = codex_usage(CODEX_TWO_TURNS).unwrap();
        assert_eq!(u.input_tokens, Some((1000 - 640) + (500 - 384)));
        assert_eq!(
            u.input_tokens.unwrap() + u.cache_read_tokens.unwrap(),
            1000 + 500
        );
        assert_eq!(u.output_tokens, Some(305));
        assert_eq!(u.cache_read_tokens, Some(1024));
        assert_eq!(u.cache_write_tokens, None);
        assert_eq!(u.cost_usd, None);
        assert_eq!(
            u.session_id.as_deref(),
            Some("0199a213-81c0-7800-8aa1-bbab2a035a53")
        );
        // A stream without turn.completed reports no usage.
        assert_eq!(
            codex_usage("{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n"),
            None
        );
        // A garbled turn makes the whole usage unknown.
        let garbled = format!(
            "{CODEX_TWO_TURNS}{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":[1]}}}}\n"
        );
        assert_eq!(codex_usage(&garbled), None);
    }

    #[test]
    fn malformed_or_missing_usage_is_unknown() {
        assert_eq!(claude_usage(CLAUDE_MALFORMED), None);
        assert_eq!(
            claude_usage("{\"type\":\"result\",\"usage\":\"n/a\"}"),
            None
        );
        assert_eq!(claude_usage("not json at all"), None);
        assert_eq!(claude_usage(""), None);
    }

    /// Built-in parsing is keyed on the command basename; an explicit
    /// `usage_output` mapping wins over it; other agents report nothing.
    #[test]
    fn capture_usage_dispatches_on_basename_and_mapping() {
        let mut inv = invocation(&[]);
        inv.command = "/opt/homebrew/bin/claude".into();
        assert_eq!(
            capture_usage(CLAUDE_RESULT, &inv).unwrap().output_tokens,
            Some(4211)
        );
        inv.command = "codex".into();
        assert_eq!(
            capture_usage(CODEX_TWO_TURNS, &inv).unwrap().input_tokens,
            Some(476)
        );
        inv.command = "pi".into();
        assert_eq!(capture_usage(CLAUDE_RESULT, &inv), None);

        inv.command = "claude".into();
        inv.usage_output = Some(
            [(
                crate::usage::UsageField::InputTokens,
                "usage.cache_read_input_tokens".to_string(),
            )]
            .into(),
        );
        let u = capture_usage(CLAUDE_RESULT, &inv).unwrap();
        assert_eq!(u.input_tokens, Some(402_118));
        assert_eq!(
            u.output_tokens, None,
            "the mapping replaces the built-in parsing"
        );
    }

    /// A `claude`-named fake agent: garbled usage leaves the review intact
    /// with usage unknown; a failed run still reports what it consumed.
    #[cfg(unix)]
    #[tokio::test]
    async fn usage_never_fails_a_review_and_failed_runs_keep_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("prx-agent-usage-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let claude = dir.join("claude");
        let stdout = dir.join("stdout.json");
        let manifest = r#"{"manifest_version": 1, "verdict": "approved"}"#;
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\nprintf '%s' '{manifest}' > \"$PRX_MANIFEST_PATH\"\ncat {}\nexit \"${{EXIT:-0}}\"\n",
                stdout.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let repo = repo_for(&claude, false);
        let path = dir.join("m.json");

        std::fs::write(&stdout, CLAUDE_MALFORMED).unwrap();
        let run = CommandAdapter
            .run_review(&test_pr(), &repo, "P", &path)
            .await
            .unwrap();
        assert_eq!(run.usage, None);
        assert_eq!(run.manifest.verdict, Verdict::Approved);

        std::fs::write(&stdout, CLAUDE_RESULT).unwrap();
        let run = CommandAdapter
            .run_review(&test_pr(), &repo, "P", &path)
            .await
            .unwrap();
        assert_eq!(run.usage.unwrap().input_tokens, Some(38));

        // A non-zero exit keeps the reported usage on the error.
        std::fs::write(
            &claude,
            format!("#!/bin/sh\ncat {}\nexit 2\n", stdout.display()),
        )
        .unwrap();
        let err = CommandAdapter
            .run_review(&test_pr(), &repo, "P", &path)
            .await
            .unwrap_err();
        assert!(
            matches!(err.cause(), AgentError::CommandFailed { .. }),
            "{err}"
        );
        assert_eq!(err.usage().unwrap().output_tokens, Some(4211));
        assert!(err.to_string().starts_with("agent command failed"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
