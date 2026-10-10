//! YAML config load/validate/merge (DEV-PLAN §4).
//!
//! Global `defaults` are merged with per-org and per-repo overrides
//! (defaults ← matching org ← repo): each layer only overrides the keys it
//! specifies, and org/repo entries share one partial-override struct so both
//! levels merge uniformly (design D4). `~` is expanded in path values, agent
//! commands resolve against a built-in table (pi/claude/codex) that config
//! entries can override, and label skip rules match case-insensitively
//! (audit A19).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::notify::{NotifyEvent, Tier, TierTable};
use crate::paths;
use crate::usage::UsageField;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found at {0}: run `prx init` first")]
    NotFound(PathBuf),
    #[error("failed to read config file {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("failed to write config file {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid configuration: {0}")]
    Validation(String),
}

/// The legacy `agents:` map shape (agent entries only); aliased into
/// [`EnginesMap`] at load (design D1).
pub type AgentsMap = BTreeMap<String, AgentConfig>;

/// The `engines:` map (design D1): every review engine by name.
pub type EnginesMap = BTreeMap<String, EngineConfig>;

/// One agent invocation: the binary plus its argument template.
/// `{{PROMPT}}` is substituted with the rendered prompt, or the agent reads
/// the prompt on stdin when args contains `-` (DEV-PLAN §4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra arguments appended when a model is configured (`agent_model`);
    /// `{{MODEL}}` is substituted with it. Empty = the agent takes no model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_args: Vec<String>,
    /// How a run reports its session reference (design D3); unset keeps the
    /// `prx-session:` stdout convention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_output: Option<SessionOutput>,
    /// Arguments appended to resume a retained session (design D3);
    /// `{{SESSION_ID}}` is substituted. Empty = the engine cannot resume.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume_args: Vec<String>,
    /// Where the agent's JSON output reports token usage (design D11): each
    /// usage field mapped to a (dotted) JSON field path. Set, it replaces
    /// the built-in `claude` / `codex` parsing; unset elsewhere, usage is
    /// unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_output: Option<BTreeMap<UsageField, String>>,
    /// Unknown keys are captured so config rewrites never drop user data.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// Where an agent run reports its session reference (design D3): exactly
/// one of a stdout line prefix (`prx-session:`) or a field of the agent's
/// JSON output (`session_id`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_field: Option<String>,
}

/// Which HTTP API an `llm_api` engine speaks (design D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    Anthropic,
    Openai,
    OpenaiCompatible,
}

/// An LLM API review engine (design D1). The key is never stored: only the
/// name of the environment variable holding it, read at call time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmApiConfig {
    pub provider: LlmProvider,
    /// Endpoint base URL; unset uses the provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: String,
    /// Environment variable holding the API key.
    #[serde(default)]
    pub api_key_env: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// Unknown keys are captured so config rewrites never drop user data.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// One `engines:` entry (design D1), discriminated by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineConfig {
    Agent(AgentConfig),
    LlmApi(LlmApiConfig),
}

impl EngineConfig {
    /// The `type` token (`agent` / `llm_api`).
    pub fn kind(&self) -> &'static str {
        match self {
            EngineConfig::Agent(_) => "agent",
            EngineConfig::LlmApi(_) => "llm_api",
        }
    }

    pub fn as_agent(&self) -> Option<&AgentConfig> {
        match self {
            EngineConfig::Agent(agent) => Some(agent),
            EngineConfig::LlmApi(_) => None,
        }
    }

    pub fn as_llm_api(&self) -> Option<&LlmApiConfig> {
        match self {
            EngineConfig::LlmApi(llm) => Some(llm),
            EngineConfig::Agent(_) => None,
        }
    }

    /// Validate one entry; `field` (`engines.<name>`) names it in errors.
    fn validate(&self, field: &str) -> Result<(), ConfigError> {
        match self {
            EngineConfig::Agent(agent) => {
                if let Some(output) = &agent.session_output {
                    let set = [&output.stdout_prefix, &output.json_field]
                        .iter()
                        .filter(|v| v.as_deref().is_some_and(|s| !s.trim().is_empty()))
                        .count();
                    if set != 1 {
                        return Err(ConfigError::Validation(format!(
                            "{field}.session_output: set exactly one of stdout_prefix or json_field"
                        )));
                    }
                }
                if !agent.resume_args.is_empty()
                    && !agent
                        .resume_args
                        .iter()
                        .any(|a| a.contains(SESSION_ID_PLACEHOLDER))
                {
                    return Err(ConfigError::Validation(format!(
                        "{field}.resume_args must contain the {SESSION_ID_PLACEHOLDER} placeholder"
                    )));
                }
                for (usage_field, path) in agent.usage_output.iter().flatten() {
                    if !crate::usage::valid_json_path(path) {
                        return Err(ConfigError::Validation(format!(
                            "{field}.usage_output.{}: '{path}' is not a JSON field path \
                             (e.g. usage.input_tokens)",
                            serde_json::to_value(usage_field)
                                .ok()
                                .and_then(|v| v.as_str().map(str::to_string))
                                .unwrap_or_default()
                        )));
                    }
                }
                Ok(())
            }
            EngineConfig::LlmApi(llm) => {
                for key in ["resume_args", "session_output"] {
                    if llm.extra.contains_key(key) {
                        return Err(ConfigError::Validation(format!(
                            "{field}.{key} is not supported on llm_api engines: sessions are \
                             an agent-process concept"
                        )));
                    }
                }
                if llm.extra.contains_key("usage_output") {
                    return Err(ConfigError::Validation(format!(
                        "{field}.usage_output is not supported on llm_api engines: prx reads \
                         the API response's usage itself"
                    )));
                }
                for key in INLINE_KEY_FIELDS {
                    if llm.extra.contains_key(*key) {
                        return Err(ConfigError::Validation(format!(
                            "{field}.{key}: API keys must not be written into the config; set \
                             api_key_env to the name of the environment variable holding it"
                        )));
                    }
                }
                if llm.model.trim().is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "{field}.model is required for llm_api engines"
                    )));
                }
                if llm.api_key_env.trim().is_empty() {
                    return Err(ConfigError::Validation(format!(
                        "{field}.api_key_env is required for llm_api engines (the name of the \
                         environment variable holding the API key)"
                    )));
                }
                if !is_env_var_name(&llm.api_key_env) {
                    return Err(ConfigError::Validation(format!(
                        "{field}.api_key_env: '{}' must be an environment variable name \
                         (e.g. ANTHROPIC_API_KEY), not the key itself",
                        llm.api_key_env
                    )));
                }
                if llm.provider == LlmProvider::OpenaiCompatible && llm.base_url.is_none() {
                    return Err(ConfigError::Validation(format!(
                        "{field}.base_url is required for provider openai_compatible"
                    )));
                }
                if let Some(url) = &llm.base_url {
                    if !(url.starts_with("https://") || url.starts_with("http://")) {
                        return Err(ConfigError::Validation(format!(
                            "{field}.base_url: '{url}' must be an http(s) URL"
                        )));
                    }
                }
                if llm.temperature.is_some_and(|t| !t.is_finite() || t < 0.0) {
                    return Err(ConfigError::Validation(format!(
                        "{field}.temperature must be a non-negative number"
                    )));
                }
                if llm.max_tokens == Some(0) {
                    return Err(ConfigError::Validation(format!(
                        "{field}.max_tokens must be > 0"
                    )));
                }
                Ok(())
            }
        }
    }
}

/// Keys that would hold API key material inline on an `llm_api` entry;
/// rejected so keys only ever come from the environment.
const INLINE_KEY_FIELDS: &[&str] = &[
    "api_key",
    "apikey",
    "key",
    "token",
    "secret",
    "password",
    "authorization",
];

/// Whether `name` is a plausible environment variable name (letters,
/// digits, `_`, not starting with a digit) rather than a secret value.
fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// An engine resolved for one repo (design D1): an agent's argv with the
/// effective `agent_model` already applied, or an `llm_api` entry as is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResolvedEngine {
    Agent(AgentInvocation),
    LlmApi(LlmApiConfig),
}

/// A resolved agent-type engine: what `CommandAdapter` spawns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInvocation {
    pub command: String,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_output: Option<SessionOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resume_args: Vec<String>,
    /// Design D11: the agent's explicit usage mapping (see
    /// [`AgentConfig::usage_output`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_output: Option<BTreeMap<UsageField, String>>,
}

impl ResolvedEngine {
    /// The `type` token (`agent` / `llm_api`).
    pub fn kind(&self) -> &'static str {
        match self {
            ResolvedEngine::Agent(_) => "agent",
            ResolvedEngine::LlmApi(_) => "llm_api",
        }
    }

    pub fn as_agent(&self) -> Option<&AgentInvocation> {
        match self {
            ResolvedEngine::Agent(agent) => Some(agent),
            ResolvedEngine::LlmApi(_) => None,
        }
    }

    pub fn as_llm_api(&self) -> Option<&LlmApiConfig> {
        match self {
            ResolvedEngine::LlmApi(llm) => Some(llm),
            ResolvedEngine::Agent(_) => None,
        }
    }

    /// Whether re-reviews resume the PR's retained session (design D3):
    /// only agent engines with `resume_args`.
    pub fn can_resume(&self) -> bool {
        self.as_agent().is_some_and(AgentInvocation::can_resume)
    }
}

impl AgentInvocation {
    /// Whether this agent declares how to resume a session (design D3).
    pub fn can_resume(&self) -> bool {
        !self.resume_args.is_empty()
    }
}

/// Placeholder in an agent's `model_args` replaced with the effective
/// `agent_model`.
pub const MODEL_PLACEHOLDER: &str = "{{MODEL}}";

/// Placeholder in an agent's `resume_args` replaced with the retained
/// session reference (design D3).
pub const SESSION_ID_PLACEHOLDER: &str = "{{SESSION_ID}}";

/// Built-in agent commands; config `agents:` entries override these per key.
pub fn builtin_agents() -> AgentsMap {
    let mut agents = AgentsMap::new();
    agents.insert(
        "pi".to_string(),
        AgentConfig {
            command: "pi".to_string(),
            args: vec!["--yes".into(), "-p".into(), "{{PROMPT}}".into()],
            model_args: vec!["--model".into(), MODEL_PLACEHOLDER.into()],
            session_output: None,
            resume_args: Vec::new(),
            usage_output: None,
            extra: BTreeMap::new(),
        },
    );
    agents.insert(
        "claude".to_string(),
        AgentConfig {
            command: "claude".to_string(),
            args: vec![
                "-p".into(),
                "{{PROMPT}}".into(),
                "--dangerously-skip-permissions".into(),
                "--output-format".into(),
                "json".into(),
            ],
            model_args: vec!["--model".into(), MODEL_PLACEHOLDER.into()],
            // Design D3: `--output-format json` reports `session_id`, and
            // `--resume` continues that session on a re-review.
            session_output: Some(SessionOutput {
                stdout_prefix: None,
                json_field: Some("session_id".into()),
            }),
            resume_args: vec!["--resume".into(), SESSION_ID_PLACEHOLDER.into()],
            usage_output: None,
            extra: BTreeMap::new(),
        },
    );
    agents.insert(
        "codex".to_string(),
        AgentConfig {
            command: "codex".to_string(),
            args: vec!["exec".into(), "--json".into(), "-".into()],
            model_args: vec!["-m".into(), MODEL_PLACEHOLDER.into()],
            // Design D3: `exec --json` emits a `thread.started` event carrying
            // `thread_id`; `exec resume <id>` continues it (the `-` stdin
            // marker is dropped from the argv, so the subcommand appends
            // cleanly and the prompt still arrives on stdin).
            session_output: Some(SessionOutput {
                stdout_prefix: None,
                json_field: Some("thread_id".into()),
            }),
            resume_args: vec!["resume".into(), SESSION_ID_PLACEHOLDER.into()],
            usage_output: None,
            extra: BTreeMap::new(),
        },
    );
    agents
}

/// Built-in engines: the built-in agents as `type: agent` entries (design
/// D1); config `engines:` entries override these per key.
pub fn builtin_engines() -> EnginesMap {
    builtin_agents()
        .into_iter()
        .map(|(name, agent)| (name, EngineConfig::Agent(agent)))
        .collect()
}

fn default_engine() -> String {
    "pi".to_string()
}

fn default_poll_interval() -> u64 {
    15 // audit A7: 15 minutes, not 2h
}

fn default_first_cooldown() -> u64 {
    60
}

fn default_re_cooldown() -> u64 {
    30
}

fn default_review_prompt() -> String {
    "default".to_string()
}

fn default_max_concurrent() -> u32 {
    1
}

fn default_diff_max_files() -> usize {
    50
}

fn default_diff_max_lines() -> usize {
    4000
}

/// Default wall-clock budget for one agent review run (10 minutes).
fn default_agent_timeout_seconds() -> u64 {
    600
}

/// Cooldown windows in minutes (spec §2.3). Anchors are defined in the state
/// machine: first review from GitHub creation time, re-reviews from
/// new-commit detection time (audit A5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Cooldown {
    pub before_first_review_minutes: u64,
    pub between_re_reviews_minutes: u64,
}

impl Default for Cooldown {
    fn default() -> Self {
        Cooldown {
            before_first_review_minutes: default_first_cooldown(),
            between_re_reviews_minutes: default_re_cooldown(),
        }
    }
}

/// Per-repo partial cooldown: only the specified keys override the defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialCooldown {
    pub before_first_review_minutes: Option<u64>,
    pub between_re_reviews_minutes: Option<u64>,
}

impl PartialCooldown {
    /// Field-by-field merge: `self` (the more specific layer) wins.
    fn merged_over(&self, base: &Self) -> Self {
        PartialCooldown {
            before_first_review_minutes: self
                .before_first_review_minutes
                .or(base.before_first_review_minutes),
            between_re_reviews_minutes: self
                .between_re_reviews_minutes
                .or(base.between_re_reviews_minutes),
        }
    }
}

/// Skip rules (DEV-PLAN §4). Labels match case-insensitively and exactly
/// (audit A19); authors match exactly; branches are glob patterns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SkipConfig {
    pub drafts: bool,
    pub authors: Vec<String>,
    pub labels: Vec<String>,
    pub branches: Vec<String>,
}

impl Default for SkipConfig {
    fn default() -> Self {
        SkipConfig {
            drafts: true, // A11: drafts are TRACKED under Skipped(draft), not invisible
            authors: Vec::new(),
            labels: vec!["wip".to_string(), "do-not-review".to_string()],
            branches: Vec::new(),
        }
    }
}

/// Per-repo partial skip config.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialSkip {
    pub drafts: Option<bool>,
    pub authors: Option<Vec<String>>,
    pub labels: Option<Vec<String>>,
    pub branches: Option<Vec<String>>,
}

impl PartialSkip {
    /// Field-by-field merge: `self` (the more specific layer) wins.
    fn merged_over(&self, base: &Self) -> Self {
        PartialSkip {
            drafts: self.drafts.or(base.drafts),
            authors: self.authors.clone().or_else(|| base.authors.clone()),
            labels: self.labels.clone().or_else(|| base.labels.clone()),
            branches: self.branches.clone().or_else(|| base.branches.clone()),
        }
    }
}

/// Event → tier mapping (defaults per DEV-PLAN §4 / spec §3.6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Notifications {
    /// Master switch for notification delivery (design D6). Global: only
    /// `defaults.notifications.enabled` is accepted. `false` stops banner/
    /// sound delivery only (the daemon's effect executor short-circuits);
    /// state and history still update. The menubar client reads this same
    /// key for its own history-driven banners.
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "tier_informational")]
    pub on_new_pr: Tier,
    #[serde(default = "tier_informational")]
    pub on_review_started: Tier,
    #[serde(default = "tier_informational")]
    pub on_review_complete_approved: Tier,
    #[serde(default = "tier_urgent")]
    pub on_review_complete_blocking: Tier,
    #[serde(default = "tier_informational")]
    pub on_new_commits: Tier,
    #[serde(default = "tier_informational")]
    pub on_re_review_triggered: Tier,
    #[serde(default = "tier_silent")]
    pub on_merged: Tier,
    #[serde(default = "tier_silent")]
    pub on_closed: Tier,
    #[serde(default = "tier_urgent")]
    pub on_failure: Tier,
    #[serde(default = "tier_silent")]
    pub on_skipped: Tier,
}

fn default_true() -> bool {
    true
}

fn tier_urgent() -> Tier {
    Tier::Urgent
}
fn tier_informational() -> Tier {
    Tier::Informational
}
fn tier_silent() -> Tier {
    Tier::Silent
}

impl Default for Notifications {
    fn default() -> Self {
        Notifications {
            enabled: true,
            on_new_pr: Tier::Informational,
            on_review_started: Tier::Informational,
            on_review_complete_approved: Tier::Informational,
            on_review_complete_blocking: Tier::Urgent,
            on_new_commits: Tier::Informational,
            on_re_review_triggered: Tier::Informational,
            on_merged: Tier::Silent,
            on_closed: Tier::Silent,
            on_failure: Tier::Urgent,
            on_skipped: Tier::Silent,
        }
    }
}

impl Notifications {
    pub fn tier_for(&self, event: NotifyEvent) -> Tier {
        match event {
            NotifyEvent::NewPr => self.on_new_pr,
            NotifyEvent::ReviewStarted => self.on_review_started,
            NotifyEvent::ReviewCompleteApproved => self.on_review_complete_approved,
            NotifyEvent::ReviewCompleteBlocking => self.on_review_complete_blocking,
            NotifyEvent::NewCommits => self.on_new_commits,
            NotifyEvent::ReReviewTriggered => self.on_re_review_triggered,
            NotifyEvent::Merged => self.on_merged,
            NotifyEvent::Closed => self.on_closed,
            NotifyEvent::Failure => self.on_failure,
            NotifyEvent::Skipped => self.on_skipped,
        }
    }
}

/// Review posture for a repo (design D3). `review comment` is today's
/// behavior: reviews post to GitHub. `monitor` runs the full pipeline but
/// never writes to GitHub; `off` stops polling and reviews entirely.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewMode {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "monitor")]
    Monitor,
    #[serde(rename = "review comment")]
    #[default]
    ReviewComment,
}

/// Per-repo partial notifications: only the specified event tiers override
/// the level below.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialNotifications {
    /// Never valid here: `notifications.enabled` is global (design D6). It
    /// is captured only so validation can reject it instead of ignoring it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    pub on_new_pr: Option<Tier>,
    pub on_review_started: Option<Tier>,
    pub on_review_complete_approved: Option<Tier>,
    pub on_review_complete_blocking: Option<Tier>,
    pub on_new_commits: Option<Tier>,
    pub on_re_review_triggered: Option<Tier>,
    pub on_merged: Option<Tier>,
    pub on_closed: Option<Tier>,
    pub on_failure: Option<Tier>,
    pub on_skipped: Option<Tier>,
}

impl PartialNotifications {
    /// Field-by-field merge: `self` (the more specific layer) wins.
    fn merged_over(&self, base: &Self) -> Self {
        PartialNotifications {
            enabled: self.enabled.or(base.enabled),
            on_new_pr: self.on_new_pr.or(base.on_new_pr),
            on_review_started: self.on_review_started.or(base.on_review_started),
            on_review_complete_approved: self
                .on_review_complete_approved
                .or(base.on_review_complete_approved),
            on_review_complete_blocking: self
                .on_review_complete_blocking
                .or(base.on_review_complete_blocking),
            on_new_commits: self.on_new_commits.or(base.on_new_commits),
            on_re_review_triggered: self.on_re_review_triggered.or(base.on_re_review_triggered),
            on_merged: self.on_merged.or(base.on_merged),
            on_closed: self.on_closed.or(base.on_closed),
            on_failure: self.on_failure.or(base.on_failure),
            on_skipped: self.on_skipped.or(base.on_skipped),
        }
    }
}

/// Which browser opens PR URLs (design D12): the system handler (`system`,
/// the default), a macOS application name or path (`Firefox`,
/// `/Applications/Arc.app`), or an argv template given as a YAML list with a
/// `{{URL}}` placeholder (`["firefox", "--new-tab", "{{URL}}"]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "BrowserRaw", into = "BrowserRaw")]
pub enum Browser {
    #[default]
    System,
    App(String),
    Command(Vec<String>),
}

/// YAML shape of `browser`: a scalar (`system` or an app) or an argv list.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum BrowserRaw {
    Name(String),
    Argv(Vec<String>),
}

impl From<BrowserRaw> for Browser {
    fn from(raw: BrowserRaw) -> Self {
        match raw {
            BrowserRaw::Name(name) if name.trim().eq_ignore_ascii_case("system") => Browser::System,
            BrowserRaw::Name(name) => Browser::App(name),
            BrowserRaw::Argv(argv) => Browser::Command(argv),
        }
    }
}

impl From<Browser> for BrowserRaw {
    fn from(browser: Browser) -> Self {
        match browser {
            Browser::System => BrowserRaw::Name("system".to_string()),
            Browser::App(name) => BrowserRaw::Name(name),
            Browser::Command(argv) => BrowserRaw::Argv(argv),
        }
    }
}

/// Placeholder an argv-template browser substitutes with the PR URL.
pub const BROWSER_URL_PLACEHOLDER: &str = "{{URL}}";

impl Browser {
    /// Validate one `browser` value; `field` names it in the error.
    fn validate(&self, field: &str) -> Result<(), ConfigError> {
        match self {
            Browser::System => Ok(()),
            Browser::App(name) if name.trim().is_empty() => Err(ConfigError::Validation(format!(
                "{field}: app name must not be empty (use `system` for the default browser)"
            ))),
            Browser::App(name) if name.contains(BROWSER_URL_PLACEHOLDER) => {
                Err(ConfigError::Validation(format!(
                    "{field}: '{name}' looks like a command template: write it as a YAML list, \
                     e.g. [\"firefox\", \"--new-tab\", \"{BROWSER_URL_PLACEHOLDER}\"]"
                )))
            }
            Browser::App(_) => Ok(()),
            Browser::Command(argv) => {
                if argv.first().is_none_or(|c| c.trim().is_empty()) {
                    return Err(ConfigError::Validation(format!(
                        "{field}: command template must start with a program name"
                    )));
                }
                if !argv.iter().any(|a| a.contains(BROWSER_URL_PLACEHOLDER)) {
                    return Err(ConfigError::Validation(format!(
                        "{field}: command template {argv:?} must contain the \
                         {BROWSER_URL_PLACEHOLDER} placeholder"
                    )));
                }
                Ok(())
            }
        }
    }
}

/// The GitHub identity prx reviews as (design D12). Every field is optional:
/// unset means today's behavior — the `gh`-authenticated user, the built-in
/// attribution footer, and gh's default credential. The same struct is the
/// partial override at org/repo level (fields merge most-specific-wins).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Identity {
    /// GitHub login prx posts as; used for self-comment detection and prompt
    /// substitution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// Footer appended to posted review comments (replaces the built-in
    /// attribution line).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution_footer: Option<String>,
    /// Name of an environment variable holding the gh token for this
    /// identity (multi-account setups); token values never enter config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

impl Identity {
    /// Field-by-field merge: `self` (the more specific layer) wins.
    fn merged_over(&self, base: &Self) -> Self {
        Identity {
            login: self.login.clone().or_else(|| base.login.clone()),
            attribution_footer: self
                .attribution_footer
                .clone()
                .or_else(|| base.attribution_footer.clone()),
            credential: self.credential.clone().or_else(|| base.credential.clone()),
        }
    }

    /// Validate one `identity` value; `field` names it in the error.
    fn validate(&self, field: &str) -> Result<(), ConfigError> {
        if let Some(login) = &self.login {
            if login.is_empty() || login.split_whitespace().count() != 1 {
                return Err(ConfigError::Validation(format!(
                    "{field}.login: '{login}' must be a single GitHub login (no spaces)"
                )));
            }
        }
        if let Some(var) = &self.credential {
            if !is_env_var_name(var) {
                return Err(ConfigError::Validation(format!(
                    "{field}.credential: '{var}' must be an environment variable name \
                     (e.g. GH_TOKEN_WORK), not a token value"
                )));
            }
        }
        Ok(())
    }
}

/// Default request-changes review body posted by the human Block action.
pub fn default_block_message() -> String {
    "Changes requested — blocked from prx pending a human follow-up.".to_string()
}

fn validate_block_message(message: &str, field: &str) -> Result<(), ConfigError> {
    if message.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "{field} must not be empty: GitHub requires a body on request-changes reviews"
        )));
    }
    Ok(())
}

/// Parse a review `schedule` (design D11): standard 5-field cron
/// (`minute hour day-of-month month day-of-week`, Sunday = 0 or 7) or an
/// `@hourly`/`@daily`/`@weekly`/`@monthly`/`@yearly` shorthand. Evaluation
/// is timezone-free; the scheduler applies it in local time.
///
/// The `cron` crate expects a leading seconds field and numbers weekdays
/// Sunday = 1..Saturday = 7, so the expression is normalized before parsing.
pub fn parse_schedule(expr: &str) -> Result<cron::Schedule, String> {
    let expected = "expected 5-field cron 'minute hour day-of-month month day-of-week' \
                    (e.g. '0 */2 * * *') or @hourly/@daily/@weekly/@monthly/@yearly";
    let trimmed = expr.trim();
    let normalized = if trimmed.starts_with('@') {
        trimmed.to_string()
    } else {
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!("{} fields given, {expected}", fields.len()));
        }
        let dow = normalize_day_of_week(fields[4])?;
        format!(
            "0 {} {} {} {} {dow}",
            fields[0], fields[1], fields[2], fields[3]
        )
    };
    normalized
        .parse::<cron::Schedule>()
        .map_err(|e| format!("{e}; {expected}"))
}

/// Translate a standard-cron day-of-week field (0-7, Sunday = 0 or 7) into
/// the `cron` crate's numbering (1-7, Sunday = 1) by expanding each numeric
/// item to its day set. Named items (`MON-FRI`) and `*`/`?` pass through.
fn normalize_day_of_week(field: &str) -> Result<String, String> {
    if field == "*" || field == "?" {
        return Ok(field.to_string());
    }
    let parse_day = |s: &str| -> Result<u32, String> {
        match s.parse::<u32>() {
            Ok(d) if d <= 7 => Ok(d),
            _ => Err(format!(
                "invalid day-of-week '{s}' (use 0-7, Sunday = 0 or 7, or SUN-SAT)"
            )),
        }
    };
    let mut items = Vec::new();
    for item in field.split(',') {
        if item.chars().any(|c| c.is_ascii_alphabetic()) {
            items.push(item.to_string());
            continue;
        }
        let (range, step) = match item.split_once('/') {
            Some((r, s)) => match s.parse::<u32>() {
                Ok(n) if n > 0 => (r, n),
                _ => return Err(format!("invalid day-of-week step in '{item}'")),
            },
            None => (item, 1),
        };
        let (start, end) = match range {
            "*" => (0, 6),
            _ => match range.split_once('-') {
                Some((a, b)) => (parse_day(a)?, parse_day(b)?),
                // `a/n` runs from `a` to the end of the week, as in vixie cron.
                None if item.contains('/') => (parse_day(range)?, 6),
                None => {
                    let d = parse_day(range)?;
                    (d, d)
                }
            },
        };
        if start > end {
            return Err(format!("day-of-week range '{range}' is reversed"));
        }
        let mut day = start;
        while day <= end {
            items.push(((day % 7) + 1).to_string());
            day += step;
        }
    }
    Ok(items.join(","))
}

fn validate_schedule(expr: &str, field: &str) -> Result<(), ConfigError> {
    parse_schedule(expr)
        .map(|_| ())
        .map_err(|e| ConfigError::Validation(format!("{field}: invalid schedule '{expr}': {e}")))
}

/// The closed placeholder set of `comments.template` (design D7).
pub const COMMENT_TEMPLATE_PLACEHOLDERS: &[&str] = &[
    "VERDICT_LABEL",
    "SUMMARY",
    "BRIEF",
    "EXEC_SUMMARY",
    "BLOCKING_COUNT",
    "NIT_COUNT",
    "FINDINGS_BLOCKING",
    "FINDINGS_NITS",
    "PR_KEY",
    "IDENTITY",
];

/// Reject a `comments.template` with an unknown `{{...}}` placeholder
/// (design D7), naming it, so no review runs with a broken template.
fn validate_comment_template(template: &str, field: &str) -> Result<(), ConfigError> {
    if template.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "{field} must not be empty: remove it to use the built-in template"
        )));
    }
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let name = &after[..end];
        if !COMMENT_TEMPLATE_PLACEHOLDERS.contains(&name) {
            let allowed = COMMENT_TEMPLATE_PLACEHOLDERS
                .iter()
                .map(|p| format!("{{{{{p}}}}}"))
                .collect::<Vec<_>>()
                .join(" ");
            return Err(ConfigError::Validation(format!(
                "{field}: unknown placeholder {{{{{name}}}}} (allowed: {allowed})"
            )));
        }
        rest = &after[end + 2..];
    }
    Ok(())
}

/// What reviews write as comments (design D7). Zero writes is
/// `review_mode: monitor`; these shape what a posting review writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Comments {
    /// Post located findings as inline PR comments; `false` keeps them in
    /// the review body only.
    pub inline: bool,
    /// Review body template (closed placeholder set); unset renders the
    /// built-in body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

impl Default for Comments {
    fn default() -> Self {
        Comments {
            inline: true,
            template: None,
        }
    }
}

/// Per-org/repo partial `comments`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialComments {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

impl PartialComments {
    /// Field-by-field merge: `self` (the more specific layer) wins.
    fn merged_over(&self, base: &Self) -> Self {
        PartialComments {
            inline: self.inline.or(base.inline),
            template: self.template.clone().or_else(|| base.template.clone()),
        }
    }
}

fn default_brief_timeout() -> u64 {
    120
}

/// Brief generation (design B2/B5): the PR's plain-language brief, taken
/// from the review manifest or generated on demand. Formerly the
/// `executive_summary` section, still read as a deprecated alias.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BriefSettings {
    pub enabled: bool,
    /// Engine generating on-demand briefs; unset uses the PR's review engine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub timeout_seconds: u64,
    /// Whether `defaults.brief` was present in the parsed file, so the
    /// deprecated `executive_summary` alias only applies without it
    /// (design B5). Never serialized and ignored by equality.
    #[serde(skip)]
    explicit: bool,
}

impl PartialEq for BriefSettings {
    fn eq(&self, other: &Self) -> bool {
        self.enabled == other.enabled
            && self.engine == other.engine
            && self.timeout_seconds == other.timeout_seconds
    }
}

impl Eq for BriefSettings {}

impl Default for BriefSettings {
    fn default() -> Self {
        BriefSettings {
            enabled: true,
            engine: None,
            timeout_seconds: default_brief_timeout(),
            explicit: false,
        }
    }
}

/// `defaults.brief` as written in the file: marks it explicit (design B5).
fn deserialize_explicit_brief<'de, D>(deserializer: D) -> Result<BriefSettings, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut brief = BriefSettings::deserialize(deserializer)?;
    brief.explicit = true;
    Ok(brief)
}

/// Per-org/repo partial `brief`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialBriefSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl PartialBriefSettings {
    /// Field-by-field merge: `self` (the more specific layer) wins.
    fn merged_over(&self, base: &Self) -> Self {
        PartialBriefSettings {
            enabled: self.enabled.or(base.enabled),
            engine: self.engine.clone().or_else(|| base.engine.clone()),
            timeout_seconds: self.timeout_seconds.or(base.timeout_seconds),
        }
    }

    /// These partial settings over full ones.
    fn applied_to(&self, base: &BriefSettings) -> BriefSettings {
        BriefSettings {
            enabled: self.enabled.unwrap_or(base.enabled),
            engine: self.engine.clone().or_else(|| base.engine.clone()),
            timeout_seconds: self.timeout_seconds.unwrap_or(base.timeout_seconds),
            explicit: base.explicit,
        }
    }
}

/// The deprecated config key of the `brief` section (design B5).
pub const LEGACY_BRIEF_KEY: &str = "executive_summary";

/// The deprecation notice for a level that still uses
/// `executive_summary` (design B5), shared by the load-time log and
/// `prx doctor`.
pub fn legacy_brief_notice(level: &str, both: bool) -> String {
    if both {
        format!(
            "config: {level} sets both `brief` and the deprecated `executive_summary`; \
             using `brief` (remove `executive_summary`)"
        )
    } else {
        format!(
            "config: {level}.executive_summary is deprecated and was renamed to \
             `brief`; it is read as `brief` for now"
        )
    }
}

/// Deprecation notices for every level of raw config YAML that still uses
/// `executive_summary` (design B5), for `prx doctor`: the typed config has
/// already folded the alias into `brief`. Unparseable text yields none.
pub fn legacy_brief_notices(raw: &str) -> Vec<String> {
    let Ok(root) = serde_yaml::from_str::<serde_yaml::Value>(raw) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut probe = |level: String, node: &serde_yaml::Value| {
        if node.get(LEGACY_BRIEF_KEY).is_some() {
            out.push(legacy_brief_notice(&level, node.get("brief").is_some()));
        }
    };
    if let Some(defaults) = root.get("defaults") {
        probe("defaults".to_string(), defaults);
    }
    for (list, name_key) in [("orgs", "org"), ("repos", "name")] {
        for entry in root
            .get(list)
            .and_then(serde_yaml::Value::as_sequence)
            .into_iter()
            .flatten()
        {
            let name = entry
                .get(name_key)
                .and_then(serde_yaml::Value::as_str)
                .unwrap_or("?");
            probe(format!("{list}[{name}]"), entry);
        }
    }
    out
}

/// Pull a legacy `executive_summary` section (captured as unknown) out of
/// one level's pass-through map, parsed as partial brief settings.
fn take_legacy_brief(
    extra: &mut BTreeMap<String, serde_yaml::Value>,
    level: &str,
) -> Result<Option<PartialBriefSettings>, String> {
    match extra.remove(LEGACY_BRIEF_KEY) {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(value) => serde_yaml::from_value(value)
            .map(Some)
            .map_err(|e| format!("{level}.{LEGACY_BRIEF_KEY}: {e}")),
    }
}

/// Which completed-review verdicts open the PR in the browser (design D9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoOpen {
    #[default]
    Off,
    Approved,
    Blocking,
    Both,
}

/// Global defaults applied to every repo unless overridden.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Defaults {
    /// The review engine (an `engines:` entry or a built-in). The legacy
    /// `agent` key is aliased to it at load (design D1); empty only between
    /// parsing and that normalization.
    #[serde(default = "String::new")]
    pub engine: String,
    /// Model passed to an agent-type engine through its `model_args`; unset
    /// leaves the choice to the agent's own default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_model: Option<String>,
    pub poll_interval_minutes: u64,
    pub review_mode: ReviewMode,
    pub cooldown: Cooldown,
    /// The review prompt template TEXT (design F5); `default` = the shipped
    /// prompt. Never a file path: legacy path values fail validation.
    pub review_prompt: String,
    /// Persisted instructions appended to every rendered review prompt
    /// (design F4); unset appends nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_prompt: Option<String>,
    pub max_concurrent_reviews: u32,
    pub diff_max_files: usize,
    pub diff_max_lines: usize,
    /// Wall-clock budget for one agent run; on expiry the process is killed
    /// and the review retries once before being marked failed.
    pub agent_timeout_seconds: u64,
    pub skip: SkipConfig,
    pub notifications: Notifications,
    /// Cron schedule gating review enqueue (design D11); unset keeps the
    /// interval cadence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    pub browser: Browser,
    pub identity: Identity,
    /// Request-changes review body posted by the human Block action.
    pub block_message: String,
    /// Review body posted with the human Approve action; unset approves
    /// without a comment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approve_message: Option<String>,
    /// First line of every posted review (`{{PR}}` and `{{VERDICT}}` are
    /// substituted); unset uses the built-in heading.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_heading: Option<String>,
    pub comments: Comments,
    /// Brief generation (design B5; legacy key `executive_summary`).
    #[serde(deserialize_with = "deserialize_explicit_brief")]
    pub brief: BriefSettings,
    pub auto_open: AutoOpen,
    /// Unknown keys are captured so config rewrites never drop user data.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            engine: default_engine(),
            agent_model: None,
            poll_interval_minutes: default_poll_interval(),
            review_mode: ReviewMode::default(),
            cooldown: Cooldown::default(),
            review_prompt: default_review_prompt(),
            extra_prompt: None,
            max_concurrent_reviews: default_max_concurrent(),
            diff_max_files: default_diff_max_files(),
            diff_max_lines: default_diff_max_lines(),
            agent_timeout_seconds: default_agent_timeout_seconds(),
            skip: SkipConfig::default(),
            notifications: Notifications::default(),
            schedule: None,
            browser: Browser::default(),
            identity: Identity::default(),
            block_message: default_block_message(),
            approve_message: None,
            review_heading: None,
            comments: Comments::default(),
            brief: BriefSettings::default(),
            auto_open: AutoOpen::default(),
            extra: BTreeMap::new(),
        }
    }
}

/// Partial overrides shared by org and repo entries (design D4): only the
/// keys present override the level below (defaults ← org ← repo). It is
/// flattened into both `OrgConfig` and `RepoConfig`, so the YAML shape of
/// existing configs is unchanged and the merge logic is written once.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PartialOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poll_interval_minutes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_mode: Option<ReviewMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_reviews: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_max_files: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_max_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown: Option<PartialCooldown>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip: Option<PartialSkip>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<PartialNotifications>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<Browser>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approve_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_heading: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comments: Option<PartialComments>,
    /// Partial `brief` (design B5; legacy key `executive_summary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<PartialBriefSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_open: Option<AutoOpen>,
    /// Unknown keys are captured so config rewrites never drop user data.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

impl PartialOverrides {
    /// Layer these (more specific) overrides over a less-specific level —
    /// a repo over its org: every present field wins; partial structures
    /// (cooldown, skip, notifications, identity, comments, brief)
    /// merge field by field; unknown-key `extra` maps union with `self`'s
    /// entries winning.
    fn merged_over(&self, base: &PartialOverrides) -> PartialOverrides {
        PartialOverrides {
            engine: self.engine.clone().or_else(|| base.engine.clone()),
            agent_model: self
                .agent_model
                .clone()
                .or_else(|| base.agent_model.clone()),
            poll_interval_minutes: self.poll_interval_minutes.or(base.poll_interval_minutes),
            review_mode: self.review_mode.or(base.review_mode),
            review_prompt: self
                .review_prompt
                .clone()
                .or_else(|| base.review_prompt.clone()),
            extra_prompt: self
                .extra_prompt
                .clone()
                .or_else(|| base.extra_prompt.clone()),
            max_concurrent_reviews: self.max_concurrent_reviews.or(base.max_concurrent_reviews),
            diff_max_files: self.diff_max_files.or(base.diff_max_files),
            diff_max_lines: self.diff_max_lines.or(base.diff_max_lines),
            agent_timeout_seconds: self.agent_timeout_seconds.or(base.agent_timeout_seconds),
            cooldown: match (&self.cooldown, &base.cooldown) {
                (Some(upper), Some(b)) => Some(upper.merged_over(b)),
                (upper, b) => upper.clone().or_else(|| b.clone()),
            },
            skip: match (&self.skip, &base.skip) {
                (Some(upper), Some(b)) => Some(upper.merged_over(b)),
                (upper, b) => upper.clone().or_else(|| b.clone()),
            },
            notifications: match (&self.notifications, &base.notifications) {
                (Some(upper), Some(b)) => Some(upper.merged_over(b)),
                (upper, b) => upper.clone().or_else(|| b.clone()),
            },
            schedule: self.schedule.clone().or_else(|| base.schedule.clone()),
            browser: self.browser.clone().or_else(|| base.browser.clone()),
            identity: match (&self.identity, &base.identity) {
                (Some(upper), Some(b)) => Some(upper.merged_over(b)),
                (upper, b) => upper.clone().or_else(|| b.clone()),
            },
            block_message: self
                .block_message
                .clone()
                .or_else(|| base.block_message.clone()),
            approve_message: self
                .approve_message
                .clone()
                .or_else(|| base.approve_message.clone()),
            review_heading: self
                .review_heading
                .clone()
                .or_else(|| base.review_heading.clone()),
            comments: match (&self.comments, &base.comments) {
                (Some(upper), Some(b)) => Some(upper.merged_over(b)),
                (upper, b) => upper.clone().or_else(|| b.clone()),
            },
            brief: match (&self.brief, &base.brief) {
                (Some(upper), Some(b)) => Some(upper.merged_over(b)),
                (upper, b) => upper.clone().or_else(|| b.clone()),
            },
            auto_open: self.auto_open.or(base.auto_open),
            extra: {
                let mut extra = base.extra.clone();
                extra.extend(self.extra.iter().map(|(k, v)| (k.clone(), v.clone())));
                extra
            },
        }
    }
}

/// One watched repo. Only the keys present override the matching org's
/// entry and the global defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoConfig {
    pub name: String,
    /// Shared partial overrides (design D4), flattened so the override keys
    /// sit directly on the repo entry as they always have.
    #[serde(flatten)]
    pub overrides: PartialOverrides,
}

/// One organization entry (design D4): its partial overrides apply to every
/// watched repo whose name starts with `<org>/`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrgConfig {
    /// The `org/` prefix of the repo names this entry applies to.
    pub org: String,
    /// Shared partial overrides (design D4), flattened like `RepoConfig`'s.
    #[serde(flatten)]
    pub overrides: PartialOverrides,
}

/// How the menu bar orders PR rows within each rendered group (design D7/D8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuSort {
    /// Newest PR creation time first.
    #[default]
    CreatedDesc,
    /// Oldest PR creation time first.
    CreatedAsc,
    /// Most recent review first; never-reviewed PRs last.
    LastReviewDesc,
    /// Ascending `org/repo#N`, PR numbers ordered numerically.
    Key,
}

/// How the menu bar groups PR rows (design D7/D8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuGroupBy {
    /// Today's lifecycle groups, in today's order.
    #[default]
    Lifecycle,
    /// One group per watched repo, ordered by repo name.
    Repo,
    /// A single flat list.
    None,
}

/// The global `menu` section (design D7): menu-bar ordering and grouping.
/// Top-level only; org/repo entries never override it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MenuSettings {
    pub sort: MenuSort,
    pub group_by: MenuGroupBy,
}

/// The whole config file. Deserialization goes through [`ConfigRepr`] so the
/// legacy `agents`/`agent` keys are aliased to `engines`/`engine` on every
/// load path (design D1); serialization always writes the new form.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ConfigRepr")]
pub struct Config {
    pub defaults: Defaults,
    pub notification_tiers: TierTable,
    pub engines: EnginesMap,
    /// Global menu-bar ordering and grouping (design D7).
    #[serde(default)]
    pub menu: MenuSettings,
    /// Org-level overrides (design D4): each applies to every watched repo
    /// whose name starts with `<org>/`.
    pub orgs: Vec<OrgConfig>,
    pub repos: Vec<RepoConfig>,
    /// Unknown top-level keys are captured so rewrites never drop user data.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_yaml::Value>,
}

/// The config file as parsed, before legacy keys are normalized.
#[derive(Deserialize)]
#[serde(default)]
struct ConfigRepr {
    defaults: Defaults,
    notification_tiers: TierTable,
    engines: EnginesMap,
    /// Legacy agent map (design D1), merged into `engines`.
    agents: AgentsMap,
    menu: MenuSettings,
    orgs: Vec<OrgConfig>,
    repos: Vec<RepoConfig>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_yaml::Value>,
}

impl Default for ConfigRepr {
    fn default() -> Self {
        let config = Config::default();
        ConfigRepr {
            defaults: config.defaults,
            notification_tiers: config.notification_tiers,
            engines: config.engines,
            agents: AgentsMap::new(),
            menu: config.menu,
            orgs: config.orgs,
            repos: config.repos,
            extra: config.extra,
        }
    }
}

/// Design B5 at one org/repo level: a legacy `executive_summary` section
/// becomes `brief`; when both are set `brief` wins. Either way a
/// deprecation is logged.
fn normalize_override_brief(overrides: &mut PartialOverrides, level: &str) -> Result<(), String> {
    if let Some(legacy) = take_legacy_brief(&mut overrides.extra, level)? {
        let both = overrides.brief.is_some();
        tracing::warn!("{}", legacy_brief_notice(level, both));
        if !both {
            overrides.brief = Some(legacy);
        }
    }
    Ok(())
}

/// Pull a legacy `agent` key (captured as unknown) out of one level's
/// pass-through map. A non-string value is an error naming the level.
fn take_legacy_agent(
    extra: &mut BTreeMap<String, serde_yaml::Value>,
    level: &str,
) -> Result<Option<String>, String> {
    match extra.remove("agent") {
        None | Some(serde_yaml::Value::Null) => Ok(None),
        Some(serde_yaml::Value::String(name)) => Ok(Some(name)),
        Some(_) => Err(format!(
            "{level}.agent: expected an engine name (the legacy alias of `engine`)"
        )),
    }
}

/// Legacy aliasing at one org/repo level: `agent` becomes `engine`; when
/// both are set the new form wins, with a warning.
fn normalize_override_engine(overrides: &mut PartialOverrides, level: &str) -> Result<(), String> {
    if let Some(legacy) = take_legacy_agent(&mut overrides.extra, level)? {
        if overrides.engine.is_some() {
            tracing::warn!(
                "config: {level} sets both `engine` and the legacy `agent` ({legacy}); using `engine`"
            );
        } else {
            overrides.engine = Some(legacy);
        }
    }
    Ok(())
}

impl TryFrom<ConfigRepr> for Config {
    type Error = String;

    fn try_from(repr: ConfigRepr) -> Result<Self, Self::Error> {
        let ConfigRepr {
            mut defaults,
            notification_tiers,
            mut engines,
            agents,
            menu,
            mut orgs,
            mut repos,
            extra,
        } = repr;
        for (name, agent) in agents {
            match engines.entry(name) {
                std::collections::btree_map::Entry::Occupied(entry) => tracing::warn!(
                    "config: `{}` is defined under both `engines:` and the legacy \
                     `agents:`; using `engines.{}`",
                    entry.key(),
                    entry.key()
                ),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(EngineConfig::Agent(agent));
                }
            }
        }
        let legacy = take_legacy_agent(&mut defaults.extra, "defaults")?;
        if defaults.engine.is_empty() {
            defaults.engine = legacy.unwrap_or_else(default_engine);
        } else if let Some(legacy) = legacy {
            tracing::warn!(
                "config: defaults sets both `engine` and the legacy `agent` ({legacy}); using `engine`"
            );
        }
        if let Some(legacy) = take_legacy_brief(&mut defaults.extra, "defaults")? {
            let both = defaults.brief.explicit;
            tracing::warn!("{}", legacy_brief_notice("defaults", both));
            if !both {
                defaults.brief = legacy.applied_to(&BriefSettings::default());
            }
        }
        for org in &mut orgs {
            let level = format!("orgs[{}]", org.org);
            normalize_override_engine(&mut org.overrides, &level)?;
            normalize_override_brief(&mut org.overrides, &level)?;
        }
        for repo in &mut repos {
            let level = format!("repos[{}]", repo.name);
            normalize_override_engine(&mut repo.overrides, &level)?;
            normalize_override_brief(&mut repo.overrides, &level)?;
        }
        Ok(Config {
            defaults,
            notification_tiers,
            engines,
            menu,
            orgs,
            repos,
            extra,
        })
    }
}

/// Fully-merged effective settings for one repo (defaults ← org ← repo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveRepo {
    pub name: String,
    /// Name of the effective review engine.
    pub engine: String,
    /// The effective model; for an agent-type engine it is already applied
    /// to the resolved args through the agent's `model_args`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model: Option<String>,
    /// The fully resolved review engine (design D1).
    pub resolved_engine: ResolvedEngine,
    pub poll_interval_minutes: u64,
    pub review_mode: ReviewMode,
    pub cooldown: Cooldown,
    /// The effective prompt template text (`default` resolved to the
    /// shipped prompt; `{{PARENT_PROMPT}}` is spliced at render time).
    pub review_prompt: String,
    /// Persisted extra instructions appended after the rendered prompt.
    pub extra_prompt: Option<String>,
    pub max_concurrent_reviews: u32,
    pub diff_max_files: usize,
    pub diff_max_lines: usize,
    pub agent_timeout_seconds: u64,
    pub skip: SkipConfig,
    pub notifications: Notifications,
    /// Effective cron schedule; `None` keeps the interval cadence.
    pub schedule: Option<String>,
    pub browser: Browser,
    pub identity: Identity,
    pub block_message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approve_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_heading: Option<String>,
    pub comments: Comments,
    /// `engine` unset means the review engine (see
    /// [`EffectiveRepo::brief_engine`]).
    pub brief: BriefSettings,
    /// The resolved brief engine (design B2): `brief.engine` resolved like
    /// the review engine (its own default model), else the review engine
    /// itself. `None` only in payloads from older daemons.
    #[serde(
        default,
        alias = "summary_resolved_engine",
        skip_serializing_if = "Option::is_none"
    )]
    pub brief_resolved_engine: Option<ResolvedEngine>,
    pub auto_open: AutoOpen,
}

impl EffectiveRepo {
    /// Name of the engine that generates on-demand briefs (design B2):
    /// `brief.engine`, else the review engine.
    pub fn brief_engine(&self) -> &str {
        self.brief.engine.as_deref().unwrap_or(&self.engine)
    }
}

/// Which config level an effective value came from (design D5: the settings
/// app renders "own override / org / default" next to every field).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValueSource {
    Defaults,
    Org,
    Repo,
}

/// Effective settings plus the source of each overridable value, keyed by
/// field path (`poll_interval_minutes`, `cooldown.before_first_review_minutes`,
/// `notifications.on_merged`, `identity.login`, ...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectiveWithSources {
    #[serde(flatten)]
    pub settings: EffectiveRepo,
    pub sources: BTreeMap<String, ValueSource>,
}

/// Every overridable field path paired with its "is it set at this level"
/// probe. Partial structures are listed per leaf so a repo overriding one
/// cooldown field still inherits the other.
type OverrideProbe = (&'static str, fn(&PartialOverrides) -> bool);

const OVERRIDE_PROBES: &[OverrideProbe] = &[
    ("engine", |o| o.engine.is_some()),
    ("agent_model", |o| o.agent_model.is_some()),
    ("poll_interval_minutes", |o| {
        o.poll_interval_minutes.is_some()
    }),
    ("review_mode", |o| o.review_mode.is_some()),
    ("review_prompt", |o| o.review_prompt.is_some()),
    ("extra_prompt", |o| o.extra_prompt.is_some()),
    ("max_concurrent_reviews", |o| {
        o.max_concurrent_reviews.is_some()
    }),
    ("diff_max_files", |o| o.diff_max_files.is_some()),
    ("diff_max_lines", |o| o.diff_max_lines.is_some()),
    ("agent_timeout_seconds", |o| {
        o.agent_timeout_seconds.is_some()
    }),
    ("cooldown.before_first_review_minutes", |o| {
        o.cooldown
            .as_ref()
            .is_some_and(|c| c.before_first_review_minutes.is_some())
    }),
    ("cooldown.between_re_reviews_minutes", |o| {
        o.cooldown
            .as_ref()
            .is_some_and(|c| c.between_re_reviews_minutes.is_some())
    }),
    ("skip.drafts", |o| {
        o.skip.as_ref().is_some_and(|s| s.drafts.is_some())
    }),
    ("skip.authors", |o| {
        o.skip.as_ref().is_some_and(|s| s.authors.is_some())
    }),
    ("skip.labels", |o| {
        o.skip.as_ref().is_some_and(|s| s.labels.is_some())
    }),
    ("skip.branches", |o| {
        o.skip.as_ref().is_some_and(|s| s.branches.is_some())
    }),
    ("notifications.on_new_pr", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_new_pr.is_some())
    }),
    ("notifications.on_review_started", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_review_started.is_some())
    }),
    ("notifications.on_review_complete_approved", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_review_complete_approved.is_some())
    }),
    ("notifications.on_review_complete_blocking", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_review_complete_blocking.is_some())
    }),
    ("notifications.on_new_commits", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_new_commits.is_some())
    }),
    ("notifications.on_re_review_triggered", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_re_review_triggered.is_some())
    }),
    ("notifications.on_merged", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_merged.is_some())
    }),
    ("notifications.on_closed", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_closed.is_some())
    }),
    ("notifications.on_failure", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_failure.is_some())
    }),
    ("notifications.on_skipped", |o| {
        o.notifications
            .as_ref()
            .is_some_and(|n| n.on_skipped.is_some())
    }),
    ("schedule", |o| o.schedule.is_some()),
    ("browser", |o| o.browser.is_some()),
    ("identity.login", |o| {
        o.identity.as_ref().is_some_and(|i| i.login.is_some())
    }),
    ("identity.attribution_footer", |o| {
        o.identity
            .as_ref()
            .is_some_and(|i| i.attribution_footer.is_some())
    }),
    ("identity.credential", |o| {
        o.identity.as_ref().is_some_and(|i| i.credential.is_some())
    }),
    ("block_message", |o| o.block_message.is_some()),
    ("approve_message", |o| o.approve_message.is_some()),
    ("review_heading", |o| o.review_heading.is_some()),
    ("comments.inline", |o| {
        o.comments.as_ref().is_some_and(|c| c.inline.is_some())
    }),
    ("comments.template", |o| {
        o.comments.as_ref().is_some_and(|c| c.template.is_some())
    }),
    ("brief.enabled", |o| {
        o.brief.as_ref().is_some_and(|e| e.enabled.is_some())
    }),
    ("brief.engine", |o| {
        o.brief.as_ref().is_some_and(|e| e.engine.is_some())
    }),
    ("brief.timeout_seconds", |o| {
        o.brief
            .as_ref()
            .is_some_and(|e| e.timeout_seconds.is_some())
    }),
    ("auto_open", |o| o.auto_open.is_some()),
];

/// Override leaves deliberately without a probe: `notifications.enabled` is
/// global (design D6), so an org/repo value is a validation error, never a
/// source.
#[cfg(test)]
const NON_OVERRIDABLE_LEAVES: &[&str] = &["notifications.enabled"];

/// The field path a validation/parse error names, when it names one
/// (design D5: the settings app renders the error next to that input).
/// Validation messages lead with the path (`defaults.poll_interval_minutes
/// must be > 0`, `repos[o/r].schedule: ...`) and serde_yaml type errors do
/// too (`defaults.poll_interval_minutes: invalid type: ...`); entry-level
/// errors map to their list. `None` when no field can be attributed.
pub fn error_field(error: &ConfigError) -> Option<String> {
    let message = match error {
        ConfigError::Validation(m) => m.as_str(),
        ConfigError::Parse { message, .. } => message.as_str(),
        _ => return None,
    };
    if message.starts_with("duplicate repo entry") || message.starts_with("invalid repo name") {
        return Some("repos".to_string());
    }
    if message.starts_with("duplicate org entry") || message.starts_with("invalid org slug") {
        return Some("orgs".to_string());
    }
    if message.starts_with("invalid branch pattern") {
        return Some("skip.branches".to_string());
    }
    if message.starts_with("unknown engine") {
        return Some("engine".to_string());
    }
    let first = message.split_whitespace().next()?.trim_end_matches(':');
    let rooted = [
        "defaults",
        "repos",
        "orgs",
        "engines",
        "notification_tiers",
        "menu",
    ]
    .iter()
    .any(|root| {
        first == *root
            || first.starts_with(&format!("{root}."))
            || first.starts_with(&format!("{root}["))
    });
    rooted.then(|| first.to_string())
}

/// Write config bytes atomically with owner-only permissions (design D5):
/// a sibling temp file (0600) is written, synced, and renamed over `path`,
/// so readers — the reload watcher included — never see a partial file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let write_err = |source| ConfigError::Write {
        path: path.to_path_buf(),
        source,
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(write_err)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.yml".to_string());
    let tmp = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        // `mode` only applies on creation; a leftover temp file keeps its
        // old bits unless they are reset explicitly.
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.map_err(write_err)
}

/// What `prx config show` prints: the fully-merged effective configuration.
#[derive(Debug, Clone, Serialize)]
pub struct EffectiveView {
    pub config_file: PathBuf,
    pub data_dir: PathBuf,
    pub defaults: EffectiveRepo,
    /// Built-in engines with the config's `engines:` entries layered on.
    pub engines: EnginesMap,
    /// Global menu-bar ordering and grouping (design D7).
    pub menu: MenuSettings,
    pub repos: Vec<EffectiveRepo>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        if !path.exists() {
            return Err(ConfigError::NotFound(path.to_path_buf()));
        }
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Config::parse(&raw, path)
    }

    /// Parse config YAML text (no validation). `path` only labels errors —
    /// shared by [`Config::load`] and the daemon's live-reload path, which
    /// hashes the bytes it read before parsing them.
    pub fn parse(raw: &str, path: &Path) -> Result<Config, ConfigError> {
        serde_yaml::from_str(raw).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: path.to_path_buf(),
                source,
            })?;
        }
        let raw = serde_yaml::to_string(self).map_err(|e| ConfigError::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        std::fs::write(path, raw).map_err(|source| ConfigError::Write {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    }

    /// Resolve an engine key against user config first, then built-ins.
    pub fn resolve_engine(&self, key: &str) -> Option<EngineConfig> {
        self.engines
            .get(key)
            .cloned()
            .or_else(|| builtin_engines().remove(key))
    }

    /// Resolve an engine key into what runs (design D1): an agent's argv
    /// with `agent_model` applied through its `model_args`, or the
    /// `llm_api` entry (which carries its own `model`; `agent_model` does
    /// not apply).
    pub fn resolved_engine(
        &self,
        key: &str,
        agent_model: Option<&str>,
    ) -> Result<ResolvedEngine, ConfigError> {
        match self
            .resolve_engine(key)
            .ok_or_else(|| unknown_engine(key))?
        {
            EngineConfig::Agent(agent) => {
                let mut args = agent.args;
                if let Some(model) = agent_model {
                    args.extend(
                        agent
                            .model_args
                            .iter()
                            .map(|a| a.replace(MODEL_PLACEHOLDER, model)),
                    );
                }
                Ok(ResolvedEngine::Agent(AgentInvocation {
                    command: agent.command,
                    args,
                    session_output: agent.session_output,
                    resume_args: agent.resume_args,
                    usage_output: agent.usage_output,
                }))
            }
            EngineConfig::LlmApi(llm) => Ok(ResolvedEngine::LlmApi(llm)),
        }
    }

    /// Sorted names of every resolvable engine: the built-ins plus the
    /// config's `engines:` entries (design D6).
    pub fn engine_names(&self) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> = builtin_engines().into_keys().collect();
        names.extend(self.engines.keys().cloned());
        names.into_iter().collect()
    }

    pub fn has_repo(&self, name: &str) -> bool {
        self.repos.iter().any(|r| r.name == name)
    }

    /// Validate the config: repo/org name format, duplicate entries, positive
    /// intervals, branch glob patterns, cron schedules, browser templates,
    /// identity fields, block messages, and agent references.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::BTreeSet::new();
        for repo in &self.repos {
            validate_repo_name(&repo.name)?;
            if !seen.insert(repo.name.clone()) {
                return Err(ConfigError::Validation(format!(
                    "duplicate repo entry: {}",
                    repo.name
                )));
            }
        }
        let mut seen_orgs = std::collections::BTreeSet::new();
        for org in &self.orgs {
            validate_org_slug(&org.org)?;
            if !seen_orgs.insert(org.org.clone()) {
                return Err(ConfigError::Validation(format!(
                    "duplicate org entry: {}",
                    org.org
                )));
            }
        }
        if self.defaults.poll_interval_minutes == 0 {
            return Err(ConfigError::Validation(
                "defaults.poll_interval_minutes must be > 0".to_string(),
            ));
        }
        if self.defaults.max_concurrent_reviews == 0 {
            return Err(ConfigError::Validation(
                "defaults.max_concurrent_reviews must be >= 1".to_string(),
            ));
        }
        if self.defaults.agent_timeout_seconds == 0 {
            return Err(ConfigError::Validation(
                "defaults.agent_timeout_seconds must be > 0".to_string(),
            ));
        }
        for pattern in self
            .defaults
            .skip
            .branches
            .iter()
            .chain(
                self.repos
                    .iter()
                    .filter_map(|r| r.overrides.skip.as_ref())
                    .flat_map(|s| s.branches.iter().flatten()),
            )
            .chain(
                self.orgs
                    .iter()
                    .filter_map(|o| o.overrides.skip.as_ref())
                    .flat_map(|s| s.branches.iter().flatten()),
            )
        {
            glob::Pattern::new(pattern).map_err(|e| {
                ConfigError::Validation(format!("invalid branch pattern '{pattern}': {e}"))
            })?;
        }
        // Schedule, browser, identity, block message, and prompt at every level.
        let levels = std::iter::once(("defaults".to_string(), None)).chain(
            self.orgs
                .iter()
                .map(|o| (format!("orgs[{}]", o.org), Some(&o.overrides)))
                .chain(
                    self.repos
                        .iter()
                        .map(|r| (format!("repos[{}]", r.name), Some(&r.overrides))),
                ),
        );
        for (level, overrides) in levels {
            let (schedule, browser, identity, block_message, review_prompt) = match overrides {
                None => (
                    self.defaults.schedule.as_ref(),
                    Some(&self.defaults.browser),
                    Some(&self.defaults.identity),
                    Some(&self.defaults.block_message),
                    Some(&self.defaults.review_prompt),
                ),
                Some(o) => (
                    o.schedule.as_ref(),
                    o.browser.as_ref(),
                    o.identity.as_ref(),
                    o.block_message.as_ref(),
                    o.review_prompt.as_ref(),
                ),
            };
            let (agent_model, review_heading) = match overrides {
                None => (
                    self.defaults.agent_model.as_ref(),
                    self.defaults.review_heading.as_ref(),
                ),
                Some(o) => (o.agent_model.as_ref(), o.review_heading.as_ref()),
            };
            if let Some(model) = agent_model {
                if model.is_empty() || model.split_whitespace().count() != 1 {
                    return Err(ConfigError::Validation(format!(
                        "{level}.agent_model: '{model}' must be a single model name (no spaces)"
                    )));
                }
            }
            if review_heading.is_some_and(|h| h.trim().is_empty()) {
                return Err(ConfigError::Validation(format!(
                    "{level}.review_heading must not be empty: remove it to use the built-in heading"
                )));
            }
            if let Some(p) = review_prompt {
                validate_review_prompt(p, &format!("{level}.review_prompt"))?;
            }
            if let Some(expr) = schedule {
                validate_schedule(expr, &format!("{level}.schedule"))?;
            }
            if let Some(b) = browser {
                b.validate(&format!("{level}.browser"))?;
            }
            if let Some(i) = identity {
                i.validate(&format!("{level}.identity"))?;
            }
            if let Some(m) = block_message {
                validate_block_message(m, &format!("{level}.block_message"))?;
            }
            let (template, summary_engine, summary_timeout) = match overrides {
                None => (
                    self.defaults.comments.template.as_ref(),
                    self.defaults.brief.engine.as_ref(),
                    Some(self.defaults.brief.timeout_seconds),
                ),
                Some(o) => (
                    o.comments.as_ref().and_then(|c| c.template.as_ref()),
                    o.brief.as_ref().and_then(|e| e.engine.as_ref()),
                    o.brief.as_ref().and_then(|e| e.timeout_seconds),
                ),
            };
            if let Some(t) = template {
                validate_comment_template(t, &format!("{level}.comments.template"))?;
            }
            if let Some(key) = summary_engine {
                if self.resolve_engine(key).is_none() {
                    return Err(ConfigError::Validation(format!(
                        "{level}.brief.engine: unknown engine '{key}': define it \
                         under `engines:` in the config"
                    )));
                }
            }
            if summary_timeout == Some(0) {
                return Err(ConfigError::Validation(format!(
                    "{level}.brief.timeout_seconds must be > 0"
                )));
            }
            // `menu` is a global top-level section (design D7).
            let level_extra = match overrides {
                None => &self.defaults.extra,
                Some(o) => &o.extra,
            };
            if level_extra.contains_key("menu") {
                return Err(ConfigError::Validation(format!(
                    "{level}.menu: menu settings are global; set them in the top-level \
                     `menu:` section only"
                )));
            }
            // `notifications.enabled` is global (design D6).
            if overrides
                .and_then(|o| o.notifications.as_ref())
                .is_some_and(|n| n.enabled.is_some())
            {
                return Err(ConfigError::Validation(format!(
                    "{level}.notifications.enabled: the notification master switch is global; \
                     set it under defaults.notifications only"
                )));
            }
        }
        for (name, engine) in &self.engines {
            // Menu action IDs embed engine names between `|` separators (design D9).
            if name.contains('|') {
                return Err(ConfigError::Validation(format!(
                    "engines.{name}: engine names must not contain '|'"
                )));
            }
            engine.validate(&format!("engines.{name}"))?;
        }
        // Every referenced engine must resolve (config entry or built-in).
        let mut engine_keys = vec![self.defaults.engine.clone()];
        engine_keys.extend(self.repos.iter().filter_map(|r| r.overrides.engine.clone()));
        engine_keys.extend(self.orgs.iter().filter_map(|o| o.overrides.engine.clone()));
        for key in engine_keys {
            if self.resolve_engine(&key).is_none() {
                return Err(unknown_engine(&key));
            }
        }
        // `agent_model` only applies to agent-type engines (design D1).
        for repo in self.effective_repos()? {
            if repo.agent_model.is_some() && repo.resolved_engine.as_agent().is_none() {
                tracing::warn!(
                    "config: {}: agent_model is ignored for the llm_api engine '{}' \
                     (it uses the engine's own `model`)",
                    repo.name,
                    repo.engine
                );
            }
        }
        Ok(())
    }

    /// The org entry matching a repo name, if any: the repo's prefix before
    /// the `/` equals the org slug (design D4). Names without a `/` (like
    /// the `(defaults)` pseudo-repo) never match.
    pub fn matching_org(&self, repo_name: &str) -> Option<&OrgConfig> {
        let (org, _) = repo_name.split_once('/')?;
        if org.is_empty() {
            return None;
        }
        self.orgs.iter().find(|o| o.org == org)
    }

    /// Merge global defaults with one repo's overrides, bridged by the
    /// matching org entry when one exists: defaults ← org ← repo, with the
    /// repo level winning field by field and partial structures (cooldown,
    /// skip) merging most-specific-wins per field.
    pub fn effective_repo(&self, repo: &RepoConfig) -> Result<EffectiveRepo, ConfigError> {
        let overrides = match self.matching_org(&repo.name) {
            Some(org) => repo.overrides.merged_over(&org.overrides),
            None => repo.overrides.clone(),
        };
        let engine = overrides
            .engine
            .clone()
            .unwrap_or_else(|| self.defaults.engine.clone());
        let cooldown = match &overrides.cooldown {
            None => self.defaults.cooldown,
            Some(p) => Cooldown {
                before_first_review_minutes: p
                    .before_first_review_minutes
                    .unwrap_or(self.defaults.cooldown.before_first_review_minutes),
                between_re_reviews_minutes: p
                    .between_re_reviews_minutes
                    .unwrap_or(self.defaults.cooldown.between_re_reviews_minutes),
            },
        };
        let skip = merge_skip(&self.defaults.skip, overrides.skip.as_ref());
        let notifications = merge_notifications(
            &self.defaults.notifications,
            overrides.notifications.as_ref(),
        );
        let identity = match &overrides.identity {
            None => self.defaults.identity.clone(),
            Some(p) => p.merged_over(&self.defaults.identity),
        };
        let prompt_raw = overrides
            .review_prompt
            .unwrap_or_else(|| self.defaults.review_prompt.clone());
        let agent_model = overrides
            .agent_model
            .or_else(|| self.defaults.agent_model.clone());
        let resolved_engine = self.resolved_engine(&engine, agent_model.as_deref())?;
        let comments = match &overrides.comments {
            None => self.defaults.comments.clone(),
            Some(p) => Comments {
                inline: p.inline.unwrap_or(self.defaults.comments.inline),
                template: p
                    .template
                    .clone()
                    .or_else(|| self.defaults.comments.template.clone()),
            },
        };
        let brief = match &overrides.brief {
            None => self.defaults.brief.clone(),
            Some(p) => p.applied_to(&self.defaults.brief),
        };
        let brief_resolved_engine = match brief.engine.as_deref() {
            Some(key) if key != engine => self.resolved_engine(key, None)?,
            _ => resolved_engine.clone(),
        };
        Ok(EffectiveRepo {
            name: repo.name.clone(),
            engine,
            agent_model,
            resolved_engine,
            poll_interval_minutes: overrides
                .poll_interval_minutes
                .unwrap_or(self.defaults.poll_interval_minutes),
            review_mode: overrides.review_mode.unwrap_or(self.defaults.review_mode),
            cooldown,
            review_prompt: resolve_review_prompt(&prompt_raw).to_string(),
            extra_prompt: overrides
                .extra_prompt
                .or_else(|| self.defaults.extra_prompt.clone()),
            max_concurrent_reviews: overrides
                .max_concurrent_reviews
                .unwrap_or(self.defaults.max_concurrent_reviews),
            diff_max_files: overrides
                .diff_max_files
                .unwrap_or(self.defaults.diff_max_files),
            diff_max_lines: overrides
                .diff_max_lines
                .unwrap_or(self.defaults.diff_max_lines),
            agent_timeout_seconds: overrides
                .agent_timeout_seconds
                .unwrap_or(self.defaults.agent_timeout_seconds),
            skip,
            notifications,
            schedule: overrides
                .schedule
                .or_else(|| self.defaults.schedule.clone()),
            browser: overrides
                .browser
                .unwrap_or_else(|| self.defaults.browser.clone()),
            identity,
            block_message: overrides
                .block_message
                .unwrap_or_else(|| self.defaults.block_message.clone()),
            approve_message: overrides
                .approve_message
                .or_else(|| self.defaults.approve_message.clone()),
            review_heading: overrides
                .review_heading
                .or_else(|| self.defaults.review_heading.clone()),
            comments,
            brief,
            brief_resolved_engine: Some(brief_resolved_engine),
            auto_open: overrides.auto_open.unwrap_or(self.defaults.auto_open),
        })
    }

    /// Effective settings for every configured repo, in config order.
    pub fn effective_repos(&self) -> Result<Vec<EffectiveRepo>, ConfigError> {
        self.repos.iter().map(|r| self.effective_repo(r)).collect()
    }

    /// The event → tier mapping in effect for `repo_name` (design D13): the
    /// watched repo's merged `notifications`, else defaults merged with a
    /// matching org entry. Falls back to `defaults.notifications` when the
    /// merge fails (an invalid config is never applied anyway).
    pub fn notifications_for(&self, repo_name: &str) -> Notifications {
        let repo = self
            .repos
            .iter()
            .find(|r| r.name == repo_name)
            .cloned()
            .unwrap_or_else(|| RepoConfig {
                name: repo_name.to_string(),
                ..RepoConfig::default()
            });
        self.effective_repo(&repo)
            .map(|r| r.notifications)
            .unwrap_or_else(|_| self.defaults.notifications.clone())
    }

    /// Effective global defaults, rendered as a pseudo-repo for `config show`.
    pub fn effective_defaults(&self) -> Result<EffectiveRepo, ConfigError> {
        let pseudo = RepoConfig {
            name: "(defaults)".to_string(),
            ..RepoConfig::default()
        };
        self.effective_repo(&pseudo)
    }

    /// Build the fully-merged view printed by `prx config show`.
    pub fn effective_view(&self) -> Result<EffectiveView, ConfigError> {
        let mut engines = builtin_engines();
        engines.extend(self.engines.iter().map(|(k, v)| (k.clone(), v.clone())));
        Ok(EffectiveView {
            config_file: paths::config_path(),
            data_dir: paths::data_dir(),
            defaults: self.effective_defaults()?,
            engines,
            menu: self.menu,
            repos: self.effective_repos()?,
        })
    }

    /// Where each overridable value of `repo`'s effective settings comes
    /// from: its own entry, its matching org entry, or the defaults.
    pub fn effective_sources(&self, repo: &RepoConfig) -> BTreeMap<String, ValueSource> {
        let org = self.matching_org(&repo.name).map(|o| &o.overrides);
        OVERRIDE_PROBES
            .iter()
            .map(|(field, is_set)| {
                let source = if is_set(&repo.overrides) {
                    ValueSource::Repo
                } else if org.is_some_and(is_set) {
                    ValueSource::Org
                } else {
                    ValueSource::Defaults
                };
                (field.to_string(), source)
            })
            .collect()
    }

    /// Effective settings + per-field sources for every configured repo.
    pub fn effective_repos_with_sources(&self) -> Result<Vec<EffectiveWithSources>, ConfigError> {
        self.repos
            .iter()
            .map(|repo| {
                Ok(EffectiveWithSources {
                    settings: self.effective_repo(repo)?,
                    sources: self.effective_sources(repo),
                })
            })
            .collect()
    }

    /// Effective settings at org level (defaults ← org) for every org entry,
    /// named by the org slug; sources are `org` or `defaults`.
    pub fn effective_orgs_with_sources(&self) -> Result<Vec<EffectiveWithSources>, ConfigError> {
        self.orgs
            .iter()
            .map(|org| {
                // A member-shaped pseudo repo with no own overrides resolves
                // through the org entry alone.
                let pseudo = RepoConfig {
                    name: format!("{}/*", org.org),
                    ..RepoConfig::default()
                };
                let mut settings = self.effective_repo(&pseudo)?;
                settings.name = org.org.clone();
                Ok(EffectiveWithSources {
                    settings,
                    sources: self.effective_sources(&pseudo),
                })
            })
            .collect()
    }
}

fn unknown_engine(key: &str) -> ConfigError {
    ConfigError::Validation(format!(
        "unknown engine '{key}': define it under `engines:` in the config"
    ))
}

/// Validate an `org/repo` name (exactly one `/`, both halves non-empty,
/// no whitespace).
pub fn validate_repo_name(name: &str) -> Result<(), ConfigError> {
    let invalid = || {
        ConfigError::Validation(format!(
            "invalid repo name '{name}': expected 'org/repo' (one slash, no spaces)"
        ))
    };
    if name.split_whitespace().count() != 1 {
        return Err(invalid());
    }
    let (org, repo) = name.split_once('/').ok_or_else(invalid)?;
    if org.is_empty() || repo.is_empty() || repo.contains('/') {
        return Err(invalid());
    }
    Ok(())
}

/// Validate an org slug (one whitespace-free token, no `/`): the `org` half
/// of the `org/repo` names it applies to.
fn validate_org_slug(slug: &str) -> Result<(), ConfigError> {
    let invalid = || {
        ConfigError::Validation(format!(
            "invalid org slug '{slug}': expected the 'org' half of an 'org/repo' name (one token, no slash)"
        ))
    };
    if slug.is_empty() || slug.split_whitespace().count() != 1 || slug.contains('/') {
        return Err(invalid());
    }
    Ok(())
}

fn merge_notifications(
    base: &Notifications,
    partial: Option<&PartialNotifications>,
) -> Notifications {
    match partial {
        None => base.clone(),
        Some(p) => Notifications {
            // Global only (design D6): never taken from an override.
            enabled: base.enabled,
            on_new_pr: p.on_new_pr.unwrap_or(base.on_new_pr),
            on_review_started: p.on_review_started.unwrap_or(base.on_review_started),
            on_review_complete_approved: p
                .on_review_complete_approved
                .unwrap_or(base.on_review_complete_approved),
            on_review_complete_blocking: p
                .on_review_complete_blocking
                .unwrap_or(base.on_review_complete_blocking),
            on_new_commits: p.on_new_commits.unwrap_or(base.on_new_commits),
            on_re_review_triggered: p
                .on_re_review_triggered
                .unwrap_or(base.on_re_review_triggered),
            on_merged: p.on_merged.unwrap_or(base.on_merged),
            on_closed: p.on_closed.unwrap_or(base.on_closed),
            on_failure: p.on_failure.unwrap_or(base.on_failure),
            on_skipped: p.on_skipped.unwrap_or(base.on_skipped),
        },
    }
}

fn merge_skip(base: &SkipConfig, partial: Option<&PartialSkip>) -> SkipConfig {
    match partial {
        None => base.clone(),
        Some(p) => SkipConfig {
            drafts: p.drafts.unwrap_or(base.drafts),
            authors: p.authors.clone().unwrap_or_else(|| base.authors.clone()),
            labels: p.labels.clone().unwrap_or_else(|| base.labels.clone()),
            branches: p.branches.clone().unwrap_or_else(|| base.branches.clone()),
        },
    }
}

/// Whether a review_prompt value is the `default` keyword (the shipped
/// prompt) rather than custom prompt text.
pub fn is_default_prompt(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("default")
}

/// Resolve a review_prompt value to template text (design F5): `default`
/// → the shipped prompt; anything else IS the prompt text. A verbatim copy
/// of the pre-C1 shipped default (`LEGACY_DEFAULT_PROMPT_V1`, compared
/// trimmed) also resolves to the shipped prompt, so it does not duplicate
/// the always-appended output contract (design C1).
pub fn resolve_review_prompt(value: &str) -> &str {
    if is_default_prompt(value) || crate::prompt::is_legacy_default_prompt(value) {
        crate::prompt::DEFAULT_PROMPT
    } else {
        value
    }
}

/// A legacy (pre-F5) review_prompt file-path value: one line, no template
/// placeholders, and a path shape (`.md`/`.txt` suffix or a `/`, `~`,
/// `./`, `../` prefix). Such values are rejected, never read.
fn looks_like_prompt_path(value: &str) -> bool {
    let v = value.trim();
    if v.contains('\n') || v.contains("{{") {
        return false;
    }
    let lower = v.to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".txt")
        || ["/", "~", "./", "../"].iter().any(|p| v.starts_with(p))
}

fn validate_review_prompt(value: &str, field: &str) -> Result<(), ConfigError> {
    if value.trim().is_empty() {
        return Err(ConfigError::Validation(format!(
            "{field} is empty: remove it (or use `default`) for the shipped prompt"
        )));
    }
    if looks_like_prompt_path(value) {
        return Err(ConfigError::Validation(format!(
            "{field}: review_prompt is now the prompt text; paste the contents of {} \
             instead (or use `default` for the shipped prompt)",
            value.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::{NotifyEvent, Tier};

    fn command_of(eff: &EffectiveRepo) -> &str {
        &eff.resolved_engine
            .as_agent()
            .expect("agent engine")
            .command
    }

    fn args_of(eff: &EffectiveRepo) -> &[String] {
        &eff.resolved_engine.as_agent().expect("agent engine").args
    }

    fn write_config(dir: &std::path::Path, name: &str, content: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn sample() -> &'static str {
        r#"defaults:
  agent: pi
  poll_interval_minutes: 15
  cooldown:
    before_first_review_minutes: 60
    between_re_reviews_minutes: 30
  skip:
    drafts: true
    labels: ["wip", "do-not-review"]
repos:
  - name: org/base
  - name: org/full
    agent: claude
    poll_interval_minutes: 45
    cooldown:
      between_re_reviews_minutes: 5
    skip:
      authors: ["dependabot[bot]"]
      drafts: false
"#
    }

    #[test]
    fn load_merges_repo_overrides_over_defaults() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(&dir, "config.yml", sample());
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();

        let base = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(base.engine, "pi");
        assert_eq!(command_of(&base), "pi"); // built-in table
        assert_eq!(base.poll_interval_minutes, 15);
        assert_eq!(base.cooldown.before_first_review_minutes, 60);
        assert_eq!(base.cooldown.between_re_reviews_minutes, 30);
        assert!(base.skip.drafts);

        let full = cfg.effective_repo(&cfg.repos[1]).unwrap();
        assert_eq!(full.engine, "claude");
        assert_eq!(command_of(&full), "claude"); // built-in table
        assert_eq!(full.poll_interval_minutes, 45);
        // Only the specified cooldown key is overridden.
        assert_eq!(full.cooldown.before_first_review_minutes, 60);
        assert_eq!(full.cooldown.between_re_reviews_minutes, 5);
        // Skip: specified keys override, the rest inherit.
        assert!(!full.skip.drafts);
        assert_eq!(full.skip.authors, vec!["dependabot[bot]".to_string()]);
        assert_eq!(
            full.skip.labels,
            vec!["wip".to_string(), "do-not-review".to_string()]
        );
    }

    #[test]
    fn org_override_reaches_member_repo_without_own_entry() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(
            &dir,
            "org-override.yml",
            r#"defaults:
  agent: pi
  poll_interval_minutes: 15
orgs:
  - org: acme
    agent: claude
    review_mode: monitor
    poll_interval_minutes: 45
repos:
  - name: acme/api
  - name: other/lib
"#,
        );
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();

        let member = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(member.engine, "claude");
        assert_eq!(command_of(&member), "claude");
        assert_eq!(member.poll_interval_minutes, 45);
        assert_eq!(member.review_mode, ReviewMode::Monitor);

        // A repo outside the org keeps the defaults.
        let outsider = cfg.effective_repo(&cfg.repos[1]).unwrap();
        assert_eq!(outsider.engine, "pi");
        assert_eq!(outsider.poll_interval_minutes, 15);
        assert_eq!(outsider.review_mode, ReviewMode::ReviewComment);
    }

    #[test]
    fn repo_override_beats_org() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(
            &dir,
            "repo-beats-org.yml",
            r#"defaults:
  agent: pi
orgs:
  - org: acme
    agent: claude
    poll_interval_minutes: 45
    review_mode: monitor
repos:
  - name: acme/api
    agent: codex
    poll_interval_minutes: 5
    review_mode: off
"#,
        );
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();

        let eff = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(eff.engine, "codex");
        assert_eq!(command_of(&eff), "codex");
        assert_eq!(eff.poll_interval_minutes, 5);
        assert_eq!(eff.review_mode, ReviewMode::Off);
    }

    #[test]
    fn org_and_repo_partials_merge_most_specific_wins() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(
            &dir,
            "org-partials.yml",
            r#"defaults:
  cooldown:
    before_first_review_minutes: 60
    between_re_reviews_minutes: 30
  skip:
    drafts: true
    labels: ["wip", "do-not-review"]
orgs:
  - org: acme
    cooldown:
      before_first_review_minutes: 90
      between_re_reviews_minutes: 15
    skip:
      labels: ["org-blocked"]
repos:
  - name: acme/api
    cooldown:
      between_re_reviews_minutes: 7
    skip:
      drafts: false
  - name: acme/web
"#,
        );
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();

        // Cooldown: the repo's field wins where both set it, the org's field
        // survives where only the org set it, defaults fill the rest.
        let api = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(api.cooldown.before_first_review_minutes, 90); // org
        assert_eq!(api.cooldown.between_re_reviews_minutes, 7); // repo beats org
        assert!(!api.skip.drafts); // repo
        assert_eq!(api.skip.labels, vec!["org-blocked".to_string()]); // org
        assert!(api.skip.authors.is_empty()); // defaults

        // A member repo with no own entry gets the full org layer.
        let web = cfg.effective_repo(&cfg.repos[1]).unwrap();
        assert_eq!(web.cooldown.before_first_review_minutes, 90);
        assert_eq!(web.cooldown.between_re_reviews_minutes, 15);
        assert!(web.skip.drafts);
        assert_eq!(web.skip.labels, vec!["org-blocked".to_string()]);
    }

    #[test]
    fn config_without_orgs_loads_unchanged() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(&dir, "config.yml", sample());
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        assert!(cfg.orgs.is_empty());

        // The defaults ← repo merge is unchanged.
        let full = cfg.effective_repo(&cfg.repos[1]).unwrap();
        assert_eq!(full.engine, "claude");
        assert_eq!(full.poll_interval_minutes, 45);
        assert_eq!(full.cooldown.before_first_review_minutes, 60);
        assert_eq!(full.cooldown.between_re_reviews_minutes, 5);
        assert!(!full.skip.drafts);
        assert_eq!(
            full.skip.labels,
            vec!["wip".to_string(), "do-not-review".to_string()]
        );
    }

    #[test]
    fn agents_config_overrides_builtins_and_validates() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(
            &dir,
            "agents.yml",
            "defaults:\n  agent: pi\nagents:\n  pi:\n    command: my-pi\n    args: [\"-x\"]\nrepos: []\n",
        );
        let cfg = Config::load(&path).unwrap();
        let pi = cfg.resolve_engine("pi").unwrap();
        let pi = pi.as_agent().unwrap();
        assert_eq!(pi.command, "my-pi");
        assert_eq!(pi.args, vec!["-x".to_string()]);
        assert_eq!(
            cfg.resolve_engine("codex")
                .unwrap()
                .as_agent()
                .unwrap()
                .command,
            "codex"
        ); // built-in fallback

        // Unknown agent fails validation.
        let path = write_config(
            &dir,
            "bad-agent.yml",
            "defaults:\n  agent: gemini\nrepos: []\n",
        );
        let cfg = Config::load(&path).unwrap();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("unknown engine 'gemini'"), "{err}");
    }

    #[test]
    fn validate_rejects_bad_repo_names_and_duplicates() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let path = write_config(
            &dir,
            "dup.yml",
            "repos:\n  - name: org/a\n  - name: org/a\n",
        );
        let err = Config::load(&path)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicate repo entry"), "{err}");

        for bad in ["justname", "org/", "/repo", "a/b/c", "a b/c"] {
            assert!(validate_repo_name(bad).is_err(), "{bad} should be invalid");
        }
        assert!(validate_repo_name("org/repo").is_ok());
    }

    #[test]
    fn validate_rejects_zero_poll_interval_and_bad_glob() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let path = write_config(
            &dir,
            "zero.yml",
            "defaults:\n  poll_interval_minutes: 0\nrepos: []\n",
        );
        let err = Config::load(&path)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("poll_interval_minutes"), "{err}");

        let path = write_config(
            &dir,
            "glob.yml",
            "defaults:\n  skip:\n    branches: [\"[unclosed\"]\nrepos: []\n",
        );
        let err = Config::load(&path)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("invalid branch pattern"), "{err}");
    }

    #[test]
    fn review_mode_defaults_to_review_comment_and_repo_overrides() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Absent key: defaults and repo entries inherit `review comment`.
        let path = write_config(&dir, "mode-default.yml", "repos:\n  - name: org/a\n");
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.defaults.review_mode, ReviewMode::ReviewComment);
        assert_eq!(cfg.repos[0].overrides.review_mode, None);
        let eff = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(eff.review_mode, ReviewMode::ReviewComment);

        // Repo override wins over the defaults level.
        let path = write_config(
            &dir,
            "mode-override.yml",
            "defaults:\n  review_mode: monitor\nrepos:\n  - name: org/a\n    review_mode: off\n",
        );
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(eff.review_mode, ReviewMode::Off);
        assert_eq!(
            cfg.effective_defaults().unwrap().review_mode,
            ReviewMode::Monitor
        );
    }

    #[test]
    fn review_mode_valid_tokens_parse() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let cases = [
            ("off", ReviewMode::Off),
            ("monitor", ReviewMode::Monitor),
            // The comment-mode token contains a space and must parse unquoted.
            ("review comment", ReviewMode::ReviewComment),
        ];
        for (token, expected) in cases {
            let path = write_config(
                &dir,
                "mode-token.yml",
                &format!("defaults:\n  review_mode: {token}\nrepos: []\n"),
            );
            let cfg = Config::load(&path).unwrap();
            assert_eq!(cfg.defaults.review_mode, expected, "{token}");
        }

        // The spaced token also parses on a repo entry.
        let path = write_config(
            &dir,
            "mode-repo-token.yml",
            "repos:\n  - name: org/a\n    review_mode: review comment\n",
        );
        let cfg = Config::load(&path).unwrap();
        assert_eq!(
            cfg.repos[0].overrides.review_mode,
            Some(ReviewMode::ReviewComment)
        );
    }

    #[test]
    fn review_mode_rejects_unknown_token_with_actionable_message() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let path = write_config(
            &dir,
            "mode-bad.yml",
            "defaults:\n  review_mode: bogus\nrepos: []\n",
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("unknown variant"), "{err}");
        assert!(err.contains("off"), "{err}");
        assert!(err.contains("monitor"), "{err}");
        assert!(err.contains("review comment"), "{err}");

        let path = write_config(
            &dir,
            "mode-bad-repo.yml",
            "repos:\n  - name: org/a\n    review_mode: silent\n",
        );
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("unknown variant"), "{err}");
        assert!(err.contains("review comment"), "{err}");
    }

    fn utc(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().into()
    }

    fn next_fires(expr: &str, after: &str, n: usize) -> Vec<chrono::DateTime<chrono::Utc>> {
        parse_schedule(expr)
            .unwrap()
            .after(&utc(after))
            .take(n)
            .collect()
    }

    #[test]
    fn schedule_valid_expressions_yield_expected_next_fires() {
        // An every-two-hours cadence: a PR eligible at 09:10 fires at 10:00.
        assert_eq!(
            next_fires("0 */2 * * *", "2026-10-05T09:10:00Z", 2),
            vec![utc("2026-10-05T10:00:00Z"), utc("2026-10-05T12:00:00Z")]
        );
        assert_eq!(
            next_fires("30 9 * * *", "2026-10-05T09:30:00Z", 1),
            vec![utc("2026-10-06T09:30:00Z")]
        );
        // Standard weekday numbering: 1-5 is Mon-Fri. 2026-10-09 is a Friday,
        // so the next fire after Friday 10:00 is Monday 2026-10-12.
        assert_eq!(
            next_fires("0 9 * * 1-5", "2026-10-09T10:00:00Z", 1),
            vec![utc("2026-10-12T09:00:00Z")]
        );
        // Sunday is 0 or 7; names pass through.
        for expr in ["0 9 * * 0", "0 9 * * 7", "0 9 * * SUN"] {
            assert_eq!(
                next_fires(expr, "2026-10-05T00:00:00Z", 1),
                vec![utc("2026-10-11T09:00:00Z")],
                "{expr}"
            );
        }
        // Weekday steps keep standard semantics: */2 = Sun, Tue, Thu, Sat.
        assert_eq!(
            next_fires("0 0 * * */2", "2026-10-05T00:00:00Z", 4),
            vec![
                utc("2026-10-06T00:00:00Z"),
                utc("2026-10-08T00:00:00Z"),
                utc("2026-10-10T00:00:00Z"),
                utc("2026-10-11T00:00:00Z"),
            ]
        );
        assert_eq!(
            next_fires("@hourly", "2026-10-05T09:10:00Z", 1),
            vec![utc("2026-10-05T10:00:00Z")]
        );
    }

    #[test]
    fn schedule_malformed_rejected_with_actionable_message() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        for bad in [
            "every two hours",
            "0 */2 * *",     // too few fields
            "0 0 */2 * * *", // seconds field is not accepted
            "61 * * * *",
            "0 9 * * 8",
            "0 9 * * 5-1",
        ] {
            assert!(parse_schedule(bad).is_err(), "{bad} should be invalid");
        }

        let path = write_config(
            &dir,
            "schedule-bad.yml",
            "defaults:\n  schedule: \"0 */2 * *\"\nrepos: []\n",
        );
        let err = Config::load(&path)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("defaults.schedule"), "{err}");
        assert!(err.contains("0 */2 * *"), "{err}");
        assert!(err.contains("5-field cron"), "{err}");

        // Org and repo levels are validated too, and named in the error.
        let path = write_config(
            &dir,
            "schedule-bad-repo.yml",
            "orgs:\n  - org: acme\n    schedule: \"0 */2 * * *\"\nrepos:\n  - name: acme/api\n    schedule: \"99 * * * *\"\n",
        );
        let err = Config::load(&path)
            .unwrap()
            .validate()
            .unwrap_err()
            .to_string();
        assert!(err.contains("repos[acme/api].schedule"), "{err}");
    }

    #[test]
    fn schedule_unset_everywhere_is_schedule_free_and_overrides_resolve() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let path = write_config(&dir, "schedule-unset.yml", sample());
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        let view = cfg.effective_view().unwrap();
        assert_eq!(view.defaults.schedule, None);
        assert!(view.repos.iter().all(|r| r.schedule.is_none()));
        // Unset is not written back out on save.
        assert!(!serde_yaml::to_string(&cfg).unwrap().contains("schedule"));

        // defaults ← org ← repo precedence.
        let path = write_config(
            &dir,
            "schedule-chain.yml",
            r#"defaults:
  schedule: "0 9 * * *"
orgs:
  - org: acme
    schedule: "0 */2 * * *"
repos:
  - name: acme/api
  - name: acme/web
    schedule: "@daily"
  - name: other/lib
"#,
        );
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        assert_eq!(eff[0].schedule.as_deref(), Some("0 */2 * * *"));
        assert_eq!(eff[1].schedule.as_deref(), Some("@daily"));
        assert_eq!(eff[2].schedule.as_deref(), Some("0 9 * * *"));
    }

    #[test]
    fn browser_and_identity_default_to_system_and_gh_user() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(&dir, "browser-identity-defaults.yml", sample());
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();

        let eff = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(eff.browser, Browser::System);
        // No login/footer/credential: the gh-authenticated user, the built-in
        // footer, and gh's default credential (today's behavior).
        assert_eq!(eff.identity, Identity::default());
        assert_eq!(eff.block_message, default_block_message());
        assert_eq!(eff.notifications, Notifications::default());
    }

    #[test]
    fn browser_identity_and_block_message_overrides_parse() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(
            &dir,
            "browser-identity.yml",
            r#"defaults:
  browser: Firefox
  identity:
    login: prx-bot
    attribution_footer: "Ping @maya"
  block_message: "Blocked: see thread."
orgs:
  - org: work
    browser: ["open", "-a", "Arc", "{{URL}}"]
    identity:
      login: work-bot
      credential: GH_TOKEN_WORK
repos:
  - name: work/api
    identity:
      attribution_footer: "Ping @work-lead"
    block_message: "Needs another pass."
  - name: work/web
    browser: system
  - name: home/app
    browser: /Applications/Safari.app
"#,
        );
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();

        let api = &eff[0];
        assert_eq!(
            api.browser,
            Browser::Command(vec![
                "open".into(),
                "-a".into(),
                "Arc".into(),
                "{{URL}}".into()
            ])
        );
        // Identity merges field by field: repo footer, org login/credential.
        assert_eq!(
            api.identity,
            Identity {
                login: Some("work-bot".into()),
                attribution_footer: Some("Ping @work-lead".into()),
                credential: Some("GH_TOKEN_WORK".into()),
            }
        );
        assert_eq!(api.block_message, "Needs another pass.");

        let web = &eff[1];
        assert_eq!(web.browser, Browser::System);
        assert_eq!(web.block_message, "Blocked: see thread.");
        assert_eq!(
            web.identity.attribution_footer.as_deref(),
            Some("Ping @maya")
        );

        let home = &eff[2];
        assert_eq!(
            home.browser,
            Browser::App("/Applications/Safari.app".into())
        );
        assert_eq!(home.identity.login.as_deref(), Some("prx-bot"));
        assert_eq!(home.identity.credential, None);
        assert_eq!(
            cfg.effective_defaults().unwrap().browser,
            Browser::App("Firefox".into())
        );

        // Round-trip keeps the browser shapes.
        let reparsed: Config = serde_yaml::from_str(&serde_yaml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(reparsed, cfg);
    }

    #[test]
    fn browser_template_requires_url_placeholder() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let validate = |name: &str, yaml: &str| {
            let path = write_config(&dir, name, yaml);
            Config::load(&path).unwrap().validate()
        };

        let err = validate(
            "browser-noplaceholder.yml",
            "defaults:\n  browser: [\"firefox\", \"--new-tab\"]\nrepos: []\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("defaults.browser"), "{err}");
        assert!(err.contains("{{URL}}"), "{err}");

        let err = validate(
            "browser-string-template.yml",
            "repos:\n  - name: org/a\n    browser: \"firefox {{URL}}\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("repos[org/a].browser"), "{err}");
        assert!(err.contains("YAML list"), "{err}");

        let err = validate("browser-empty.yml", "defaults:\n  browser: []\nrepos: []\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("program name"), "{err}");

        validate(
            "browser-ok.yml",
            "defaults:\n  browser: [\"firefox\", \"--new-tab\", \"{{URL}}\"]\nrepos: []\n",
        )
        .unwrap();
    }

    #[test]
    fn identity_and_block_message_validation() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let validate = |name: &str, yaml: &str| {
            let path = write_config(&dir, name, yaml);
            Config::load(&path)
                .unwrap()
                .validate()
                .map_err(|e| e.to_string())
        };

        let err = validate(
            "identity-token.yml",
            "orgs:\n  - org: work\n    identity:\n      credential: ghp_abc-123\nrepos: []\n",
        )
        .unwrap_err();
        assert!(err.contains("orgs[work].identity.credential"), "{err}");
        assert!(err.contains("environment variable name"), "{err}");

        let err = validate(
            "identity-login.yml",
            "defaults:\n  identity:\n    login: \"two words\"\nrepos: []\n",
        )
        .unwrap_err();
        assert!(err.contains("defaults.identity.login"), "{err}");

        let err = validate(
            "block-empty.yml",
            "repos:\n  - name: org/a\n    block_message: \"  \"\n",
        )
        .unwrap_err();
        assert!(err.contains("repos[org/a].block_message"), "{err}");
    }

    #[test]
    fn notifications_partial_merges_through_org_and_repo() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = write_config(
            &dir,
            "notifications.yml",
            r#"defaults:
  notifications:
    on_new_pr: silent
orgs:
  - org: acme
    notifications:
      on_merged: informational
      on_failure: silent
repos:
  - name: acme/api
    notifications:
      on_failure: urgent
      on_new_commits: silent
  - name: other/lib
"#,
        );
        let cfg = Config::load(&path).unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();

        let api = &eff[0].notifications;
        assert_eq!(api.on_new_pr, Tier::Silent); // defaults
        assert_eq!(api.on_merged, Tier::Informational); // org
        assert_eq!(api.on_failure, Tier::Urgent); // repo beats org
        assert_eq!(api.on_new_commits, Tier::Silent); // repo
        assert_eq!(api.on_review_complete_blocking, Tier::Urgent); // built-in

        // Other repos keep the defaults.
        let lib = &eff[1].notifications;
        assert_eq!(lib.on_new_pr, Tier::Silent);
        assert_eq!(lib.on_merged, Tier::Silent);
        assert_eq!(lib.on_failure, Tier::Urgent);
    }

    /// Design D13: every `Defaults` key must have a matching override slot so
    /// org and repo entries accept anything defaults does. Both structs are
    /// built with exhaustive literals (no `..Default::default()`), so adding a
    /// field to either forces this test to be updated.
    #[test]
    fn every_defaults_field_has_an_override_slot() {
        let defaults = Defaults {
            engine: default_engine(),
            agent_model: Some("m".into()),
            poll_interval_minutes: 1,
            review_mode: ReviewMode::Monitor,
            cooldown: Cooldown::default(),
            review_prompt: default_review_prompt(),
            extra_prompt: Some("x".into()),
            max_concurrent_reviews: 1,
            diff_max_files: 1,
            diff_max_lines: 1,
            agent_timeout_seconds: 1,
            skip: SkipConfig::default(),
            notifications: Notifications::default(),
            schedule: Some("@daily".into()),
            browser: Browser::System,
            identity: Identity::default(),
            block_message: default_block_message(),
            approve_message: Some("x".into()),
            review_heading: Some("x".into()),
            comments: Comments::default(),
            brief: BriefSettings::default(),
            auto_open: AutoOpen::Both,
            extra: BTreeMap::new(),
        };
        let overrides = PartialOverrides {
            engine: Some("pi".into()),
            agent_model: Some("m".into()),
            poll_interval_minutes: Some(1),
            review_mode: Some(ReviewMode::Off),
            review_prompt: Some("default".into()),
            extra_prompt: Some("x".into()),
            max_concurrent_reviews: Some(1),
            diff_max_files: Some(1),
            diff_max_lines: Some(1),
            agent_timeout_seconds: Some(1),
            cooldown: Some(PartialCooldown::default()),
            skip: Some(PartialSkip::default()),
            notifications: Some(PartialNotifications::default()),
            schedule: Some("@daily".into()),
            browser: Some(Browser::System),
            identity: Some(Identity::default()),
            block_message: Some("x".into()),
            approve_message: Some("x".into()),
            review_heading: Some("x".into()),
            comments: Some(PartialComments::default()),
            brief: Some(PartialBriefSettings::default()),
            auto_open: Some(AutoOpen::Off),
            extra: BTreeMap::new(),
        };
        let keys = |v: serde_yaml::Value| -> std::collections::BTreeSet<String> {
            v.as_mapping()
                .unwrap()
                .keys()
                .map(|k| k.as_str().unwrap().to_string())
                .collect()
        };
        let defaults_keys = keys(serde_yaml::to_value(&defaults).unwrap());
        let override_keys = keys(serde_yaml::to_value(&overrides).unwrap());
        assert_eq!(defaults_keys, override_keys);

        // The effective merge carries each override through to `EffectiveRepo`.
        let cfg = Config {
            repos: vec![RepoConfig {
                name: "org/a".into(),
                overrides: PartialOverrides {
                    notifications: Some(PartialNotifications {
                        on_merged: Some(Tier::Urgent),
                        ..PartialNotifications::default()
                    }),
                    schedule: Some("0 */2 * * *".into()),
                    browser: Some(Browser::App("Firefox".into())),
                    identity: Some(Identity {
                        login: Some("prx-bot".into()),
                        ..Identity::default()
                    }),
                    block_message: Some("blocked".into()),
                    ..PartialOverrides::default()
                },
            }],
            ..Config::default()
        };
        cfg.validate().unwrap();
        let eff = cfg.effective_repo(&cfg.repos[0]).unwrap();
        assert_eq!(eff.notifications.on_merged, Tier::Urgent);
        assert_eq!(eff.schedule.as_deref(), Some("0 */2 * * *"));
        assert_eq!(eff.browser, Browser::App("Firefox".into()));
        assert_eq!(eff.identity.login.as_deref(), Some("prx-bot"));
        assert_eq!(eff.block_message, "blocked");
    }

    /// `agent_model` reaches the agent through its `model_args`, merges
    /// most-specific-wins, and is ignored by agents that take no model.
    #[test]
    fn agent_model_is_appended_through_model_args() {
        let cfg: Config = serde_yaml::from_str(
            "defaults:\n  agent: claude\n  agent_model: sonnet\nagents:\n  plain:\n    command: plain\n    args: [\"{{PROMPT}}\"]\norgs:\n  - org: acme\n    agent_model: opus\nrepos:\n  - name: acme/a\n  - name: acme/b\n    agent: codex\n    agent_model: gpt-5\n  - name: solo/c\n  - name: solo/d\n    agent: plain\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        assert_eq!(eff[0].agent_model.as_deref(), Some("opus"));
        assert_eq!(
            args_of(&eff[0]),
            [
                "-p",
                "{{PROMPT}}",
                "--dangerously-skip-permissions",
                "--output-format",
                "json",
                "--model",
                "opus"
            ]
        );
        assert_eq!(args_of(&eff[1]), ["exec", "--json", "-", "-m", "gpt-5"]);
        assert_eq!(args_of(&eff[2]).last().map(String::as_str), Some("sonnet"));
        assert_eq!(args_of(&eff[3]), ["{{PROMPT}}"]);
        assert_eq!(
            cfg.effective_sources(&cfg.repos[0])["agent_model"],
            ValueSource::Org
        );

        // Unset: the built-in argument lists are untouched.
        let plain = Config::default().effective_defaults().unwrap();
        assert_eq!(plain.agent_model, None);
        assert_eq!(args_of(&plain), builtin_agents()["pi"].args);
        assert!(!serde_yaml::to_string(&Config::default())
            .unwrap()
            .contains("agent_model"));

        let bad: Config =
            serde_yaml::from_str("defaults:\n  agent_model: \"two words\"\n").unwrap();
        let err = bad.validate().unwrap_err().to_string();
        assert!(err.contains("defaults.agent_model"), "{err}");
        assert_eq!(
            error_field(&bad.validate().unwrap_err()).as_deref(),
            Some("defaults.agent_model")
        );
    }

    #[test]
    fn message_templates_merge_and_validate() {
        let cfg: Config = serde_yaml::from_str(
            "defaults:\n  approve_message: \"LGTM\"\n  review_heading: \"## Review of {{PR}}\"\nrepos:\n  - name: org/a\n    approve_message: \"Ship it\"\n  - name: org/b\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        assert_eq!(eff[0].approve_message.as_deref(), Some("Ship it"));
        assert_eq!(eff[1].approve_message.as_deref(), Some("LGTM"));
        assert_eq!(
            eff[1].review_heading.as_deref(),
            Some("## Review of {{PR}}")
        );
        assert_eq!(
            Config::default()
                .effective_defaults()
                .unwrap()
                .review_heading,
            None
        );

        let bad: Config =
            serde_yaml::from_str("repos:\n  - name: org/a\n    review_heading: \"  \"\n").unwrap();
        let err = bad.validate().unwrap_err().to_string();
        assert!(err.contains("repos[org/a].review_heading"), "{err}");
    }

    #[test]
    fn notification_defaults_match_dev_plan() {
        let n = Notifications::default();
        assert_eq!(n.tier_for(NotifyEvent::NewPr), Tier::Informational);
        assert_eq!(
            n.tier_for(NotifyEvent::ReviewCompleteApproved),
            Tier::Informational
        );
        assert_eq!(
            n.tier_for(NotifyEvent::ReviewCompleteBlocking),
            Tier::Urgent
        );
        assert_eq!(n.tier_for(NotifyEvent::NewCommits), Tier::Informational);
        assert_eq!(
            n.tier_for(NotifyEvent::ReReviewTriggered),
            Tier::Informational
        );
        assert_eq!(n.tier_for(NotifyEvent::Merged), Tier::Silent);
        assert_eq!(n.tier_for(NotifyEvent::Closed), Tier::Silent);
        assert_eq!(n.tier_for(NotifyEvent::Failure), Tier::Urgent);
        assert_eq!(n.tier_for(NotifyEvent::Skipped), Tier::Silent);
        assert_eq!(NotifyEvent::NewPr.config_key(), "on_new_pr");
    }

    #[test]
    fn tier_table_builtin_matches_spec() {
        let t = TierTable::builtin();
        assert!(t.urgent.banner);
        assert_eq!(t.urgent.sound.as_deref(), Some("Basso"));
        assert!(t.informational.banner);
        assert_eq!(t.informational.sound, None);
        assert!(!t.silent.banner);
    }

    fn parse_yaml(yaml: &str) -> Result<Config, ConfigError> {
        Config::parse(yaml, Path::new("t.yml"))
    }

    #[test]
    fn tilde_expansion() {
        assert_eq!(paths::expand_tilde("~"), paths::expand_tilde("~/"));
        assert_eq!(
            paths::expand_tilde("~/x/y.md"),
            paths::expand_tilde("~").join("x/y.md")
        );
        assert_eq!(
            paths::expand_tilde("/abs/path"),
            std::path::PathBuf::from("/abs/path")
        );
    }

    /// Design F5: `default` (or unset) is the shipped prompt text; any other
    /// value IS the prompt text and flows into `EffectiveRepo` verbatim.
    #[test]
    fn review_prompt_is_inline_text_and_default_is_the_shipped_prompt() {
        assert_eq!(
            resolve_review_prompt("default"),
            crate::prompt::DEFAULT_PROMPT
        );
        assert_eq!(
            resolve_review_prompt(" Default "),
            crate::prompt::DEFAULT_PROMPT
        );
        let cfg = parse_yaml(
            "repos:\n  - name: org/a\n  - name: org/b\n    review_prompt: |\n      Review {{REPO}} strictly.\n      Manifest: {{MANIFEST_PATH}}\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        assert_eq!(eff[0].review_prompt, crate::prompt::DEFAULT_PROMPT);
        assert_eq!(
            eff[1].review_prompt,
            "Review {{REPO}} strictly.\nManifest: {{MANIFEST_PATH}}\n"
        );
    }

    /// Design C1: a `review_prompt` that is the pre-C1 shipped default saved
    /// verbatim (whitespace-trimmed) resolves to the new default, so the
    /// manifest schema is not duplicated next to the appended contract.
    #[test]
    fn legacy_default_prompt_text_resolves_to_default() {
        let legacy = crate::prompt::LEGACY_DEFAULT_PROMPT_V1;
        assert_eq!(resolve_review_prompt(legacy), crate::prompt::DEFAULT_PROMPT);
        let padded = format!("\n\n{}\n   \n", legacy.trim());
        assert_eq!(
            resolve_review_prompt(&padded),
            crate::prompt::DEFAULT_PROMPT
        );
        // An edited copy is custom text and stays verbatim.
        let edited = format!("{legacy}\nAlso check migrations.\n");
        assert_eq!(resolve_review_prompt(&edited), edited);

        let indented: String = legacy
            .trim()
            .lines()
            .map(|l| format!("      {l}\n"))
            .collect();
        let cfg = parse_yaml(&format!(
            "repos:\n  - name: org/a\n    review_prompt: |\n{indented}"
        ))
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        assert_eq!(eff[0].review_prompt, crate::prompt::DEFAULT_PROMPT);
    }

    /// Design F5: a legacy file-path value is rejected with an actionable
    /// message at every level, never read.
    #[test]
    fn legacy_review_prompt_paths_are_rejected() {
        for (yaml, field, path) in [
            (
                "defaults:\n  review_prompt: ~/prompts/strict.md\n",
                "defaults.review_prompt",
                "~/prompts/strict.md",
            ),
            (
                "orgs:\n  - org: acme\n    review_prompt: /etc/prx/p.txt\n",
                "orgs[acme].review_prompt",
                "/etc/prx/p.txt",
            ),
            (
                "repos:\n  - name: org/a\n    review_prompt: ./p\n",
                "repos[org/a].review_prompt",
                "./p",
            ),
            (
                "repos:\n  - name: org/a\n    review_prompt: prompts/x.MD\n",
                "repos[org/a].review_prompt",
                "prompts/x.MD",
            ),
        ] {
            let err = parse_yaml(yaml).unwrap().validate().unwrap_err();
            assert_eq!(error_field(&err).as_deref(), Some(field), "{err}");
            let err = err.to_string();
            assert!(
                err.contains(&format!(
                    "{field}: review_prompt is now the prompt text; \
                     paste the contents of {path} instead"
                )),
                "{err}"
            );
        }
        // Prompt-like text that merely mentions a path is fine.
        for ok in [
            "Review {{REPO}} per ./CONTRIBUTING.md",
            "Check the README.md\nand the docs.md",
            "default",
        ] {
            let mut cfg = Config::default();
            cfg.defaults.review_prompt = ok.into();
            cfg.validate().unwrap();
        }
        let mut cfg = Config::default();
        cfg.defaults.review_prompt = "  ".into();
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("defaults.review_prompt is empty"), "{err}");
    }

    /// Design F4: extra_prompt merges most-specific-wins (repo > org >
    /// defaults) and is `None` when unset everywhere; sources track it.
    #[test]
    fn extra_prompt_merges_most_specific_wins() {
        let cfg = parse_yaml(
            "defaults:\n  extra_prompt: D\norgs:\n  - org: acme\n    extra_prompt: O\nrepos:\n  - name: acme/a\n  - name: acme/b\n    extra_prompt: R\n  - name: solo/c\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        let got: Vec<_> = eff.iter().map(|e| e.extra_prompt.as_deref()).collect();
        assert_eq!(got, [Some("O"), Some("R"), Some("D")]);
        let sources: Vec<_> = cfg
            .repos
            .iter()
            .map(|r| cfg.effective_sources(r)["extra_prompt"])
            .collect();
        assert_eq!(
            sources,
            [ValueSource::Org, ValueSource::Repo, ValueSource::Defaults]
        );
        let unset = parse_yaml("repos:\n  - name: org/a\n").unwrap();
        assert_eq!(unset.effective_repos().unwrap()[0].extra_prompt, None);
        // Unset is omitted on save, so existing configs round-trip unchanged.
        let yaml = serde_yaml::to_string(&Config::default()).unwrap();
        assert!(!yaml.contains("extra_prompt"), "{yaml}");
    }

    #[test]
    fn save_roundtrip_preserves_unknown_keys() {
        let dir = std::env::temp_dir().join(format!("prx-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("extra.yml");
        std::fs::write(
            &path,
            "defaults:\n  agent: pi\nfuture_thing:\n  on: true\nrepos:\n  - name: org/a\n    weird: 7\n",
        )
        .unwrap();
        let mut cfg = Config::load(&path).unwrap();
        assert_eq!(
            cfg.extra["future_thing"]["on"],
            serde_yaml::Value::Bool(true)
        );
        assert_eq!(
            cfg.repos[0].overrides.extra["weird"],
            serde_yaml::Value::Number(7.into())
        );

        cfg.repos.push(RepoConfig {
            name: "org/b".into(),
            ..RepoConfig::default()
        });
        cfg.save(&path).unwrap();
        let reloaded = Config::load(&path).unwrap();
        assert_eq!(
            reloaded.extra["future_thing"]["on"],
            serde_yaml::Value::Bool(true)
        );
        assert_eq!(
            reloaded.repos[0].overrides.extra["weird"],
            serde_yaml::Value::Number(7.into())
        );
        assert_eq!(reloaded.repos.len(), 2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_config_is_a_distinct_error() {
        let err = Config::load(std::path::Path::new("/nonexistent/prx/config.yml")).unwrap_err();
        assert!(err.to_string().contains("prx init"));
    }

    // ------------------------------------------- effective sources (D5)

    fn sources_config() -> Config {
        let yaml = r#"defaults:
  agent: pi
orgs:
  - org: acme
    poll_interval_minutes: 30
    cooldown:
      before_first_review_minutes: 120
    review_mode: monitor
repos:
  - name: acme/api
    review_mode: off
    cooldown:
      between_re_reviews_minutes: 5
  - name: acme/web
  - name: other/tool
    identity:
      login: prx-bot
"#;
        Config::parse(yaml, Path::new("t.yml")).unwrap()
    }

    #[test]
    fn effective_sources_name_own_org_or_default_per_field() {
        let cfg = sources_config();
        let api = cfg.effective_sources(&cfg.repos[0]);
        assert_eq!(api["review_mode"], ValueSource::Repo);
        assert_eq!(api["poll_interval_minutes"], ValueSource::Org);
        assert_eq!(
            api["cooldown.before_first_review_minutes"],
            ValueSource::Org
        );
        assert_eq!(
            api["cooldown.between_re_reviews_minutes"],
            ValueSource::Repo
        );
        assert_eq!(api["engine"], ValueSource::Defaults);

        let web = cfg.effective_sources(&cfg.repos[1]);
        assert_eq!(web["review_mode"], ValueSource::Org);
        assert_eq!(
            web["cooldown.between_re_reviews_minutes"],
            ValueSource::Defaults
        );

        let tool = cfg.effective_sources(&cfg.repos[2]);
        assert_eq!(tool["identity.login"], ValueSource::Repo);
        assert_eq!(tool["identity.credential"], ValueSource::Defaults);
        assert_eq!(tool["poll_interval_minutes"], ValueSource::Defaults);

        let views = cfg.effective_repos_with_sources().unwrap();
        assert_eq!(views.len(), 3);
        assert_eq!(views[0].settings.review_mode, ReviewMode::Off);
        assert_eq!(views[1].settings.review_mode, ReviewMode::Monitor);
        assert_eq!(views[1].settings.poll_interval_minutes, 30);

        let orgs = cfg.effective_orgs_with_sources().unwrap();
        assert_eq!(orgs.len(), 1);
        assert_eq!(orgs[0].settings.name, "acme");
        assert_eq!(orgs[0].settings.review_mode, ReviewMode::Monitor);
        assert_eq!(orgs[0].sources["review_mode"], ValueSource::Org);
        assert_eq!(orgs[0].sources["engine"], ValueSource::Defaults);

        // The JSON shape the settings app reads: flattened settings + sources.
        let json = serde_json::to_value(&views[0]).unwrap();
        assert_eq!(json["name"], "acme/api");
        assert_eq!(json["sources"]["review_mode"], "repo");
        let back: EffectiveWithSources = serde_json::from_value(json).unwrap();
        assert_eq!(back, views[0]);
    }

    /// Every leaf of the shared override struct has a source probe, so the
    /// settings app can show provenance for any overridable field.
    #[test]
    fn override_probes_cover_every_override_leaf() {
        let full = PartialOverrides {
            engine: Some("pi".into()),
            agent_model: Some("m".into()),
            approve_message: Some("a".into()),
            review_heading: Some("h".into()),
            poll_interval_minutes: Some(1),
            review_mode: Some(ReviewMode::Monitor),
            review_prompt: Some("p".into()),
            extra_prompt: Some("e".into()),
            max_concurrent_reviews: Some(1),
            diff_max_files: Some(1),
            diff_max_lines: Some(1),
            agent_timeout_seconds: Some(1),
            cooldown: Some(PartialCooldown {
                before_first_review_minutes: Some(1),
                between_re_reviews_minutes: Some(1),
            }),
            skip: Some(PartialSkip {
                drafts: Some(true),
                authors: Some(vec![]),
                labels: Some(vec![]),
                branches: Some(vec![]),
            }),
            notifications: Some(PartialNotifications {
                enabled: Some(false),
                on_new_pr: Some(Tier::Silent),
                on_review_started: Some(Tier::Silent),
                on_review_complete_approved: Some(Tier::Silent),
                on_review_complete_blocking: Some(Tier::Silent),
                on_new_commits: Some(Tier::Silent),
                on_re_review_triggered: Some(Tier::Silent),
                on_merged: Some(Tier::Silent),
                on_closed: Some(Tier::Silent),
                on_failure: Some(Tier::Silent),
                on_skipped: Some(Tier::Silent),
            }),
            schedule: Some("@daily".into()),
            browser: Some(Browser::System),
            identity: Some(Identity {
                login: Some("a".into()),
                attribution_footer: Some("f".into()),
                credential: Some("V".into()),
            }),
            block_message: Some("b".into()),
            comments: Some(PartialComments {
                inline: Some(false),
                template: Some("{{SUMMARY}}".into()),
            }),
            brief: Some(PartialBriefSettings {
                enabled: Some(false),
                engine: Some("pi".into()),
                timeout_seconds: Some(1),
            }),
            auto_open: Some(AutoOpen::Blocking),
            extra: BTreeMap::new(),
        };
        let value = serde_json::to_value(&full).unwrap();
        let mut leaves = std::collections::BTreeSet::new();
        for (key, v) in value.as_object().unwrap() {
            match v.as_object() {
                Some(inner) => leaves.extend(inner.keys().map(|k| format!("{key}.{k}"))),
                None => {
                    leaves.insert(key.clone());
                }
            }
        }
        // Exempt by design: global-only keys a level may not override.
        for exempt in NON_OVERRIDABLE_LEAVES {
            assert!(leaves.remove(*exempt), "{exempt} is not a leaf any more");
        }
        let probed: std::collections::BTreeSet<String> =
            OVERRIDE_PROBES.iter().map(|(f, _)| f.to_string()).collect();
        assert_eq!(leaves, probed);
        assert!(OVERRIDE_PROBES.iter().all(|(_, is_set)| is_set(&full)));
        assert!(OVERRIDE_PROBES
            .iter()
            .all(|(_, is_set)| !is_set(&PartialOverrides::default())));
    }

    #[test]
    fn error_field_points_at_the_offending_input() {
        let field = |yaml: &str| {
            let err = match Config::parse(yaml, Path::new("t.yml")) {
                Ok(cfg) => cfg.validate().unwrap_err(),
                Err(e) => e,
            };
            error_field(&err)
        };
        assert_eq!(
            field("defaults:\n  poll_interval_minutes: 0\n").as_deref(),
            Some("defaults.poll_interval_minutes")
        );
        assert_eq!(
            field("defaults:\n  poll_interval_minutes: soon\n").as_deref(),
            Some("defaults.poll_interval_minutes")
        );
        assert_eq!(
            field("repos:\n  - name: o/r\n    schedule: nope\n").as_deref(),
            Some("repos[o/r].schedule")
        );
        assert_eq!(field("repos:\n  - name: bad\n").as_deref(), Some("repos"));
        assert_eq!(
            field("defaults:\n  agent: missing\n").as_deref(),
            Some("engine")
        );
    }

    // ------------------------------------------------ engines (design D1)

    /// Legacy `agents`/`agent` keys alias to `engines`/`engine` at every
    /// layer, and a rewrite saves the new form.
    #[test]
    fn legacy_agent_keys_alias_to_engines_at_every_layer() {
        let cfg = parse_yaml(
            "defaults:\n  agent: mine\nagents:\n  mine:\n    command: my-agent\n    args: [\"-\"]\norgs:\n  - org: acme\n    agent: claude\nrepos:\n  - name: acme/api\n    agent: codex\n  - name: acme/web\n  - name: solo/lib\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.defaults.engine, "mine");
        assert_eq!(cfg.orgs[0].overrides.engine.as_deref(), Some("claude"));
        assert_eq!(cfg.repos[0].overrides.engine.as_deref(), Some("codex"));
        assert!(cfg.defaults.extra.is_empty() && cfg.repos[0].overrides.extra.is_empty());
        assert_eq!(cfg.engines["mine"].as_agent().unwrap().command, "my-agent");
        let eff = cfg.effective_repos().unwrap();
        let engines: Vec<_> = eff.iter().map(|e| e.engine.as_str()).collect();
        assert_eq!(engines, ["codex", "claude", "mine"]);
        assert_eq!(command_of(&eff[2]), "my-agent");

        let saved = serde_yaml::to_string(&cfg).unwrap();
        assert!(
            saved.contains("engines:") && !saved.contains("agents:"),
            "{saved}"
        );
        assert!(saved.contains("type: agent"), "{saved}");
        assert!(!saved.contains("agent: "), "{saved}");
        let reparsed = parse_yaml(&saved).unwrap();
        assert_eq!(reparsed, cfg);
    }

    /// When both forms are present at one layer the new form wins.
    #[test]
    fn new_engine_keys_win_over_legacy_aliases() {
        let cfg = parse_yaml(
            "defaults:\n  engine: claude\n  agent: codex\nengines:\n  mine:\n    type: agent\n    command: new-agent\nagents:\n  mine:\n    command: old-agent\n  extra:\n    command: extra-agent\nrepos:\n  - name: o/r\n    engine: pi\n    agent: codex\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.defaults.engine, "claude");
        assert_eq!(cfg.repos[0].overrides.engine.as_deref(), Some("pi"));
        assert_eq!(cfg.engines["mine"].as_agent().unwrap().command, "new-agent");
        // Legacy-only entries are still merged in.
        assert_eq!(
            cfg.engines["extra"].as_agent().unwrap().command,
            "extra-agent"
        );
        // A legacy `agent` that is not a name is a load error.
        let err = parse_yaml("repos:\n  - name: o/r\n    agent: [1]\n").unwrap_err();
        assert!(err.to_string().contains("repos[o/r].agent"), "{err}");
    }

    #[test]
    fn engine_resolves_repo_over_org_over_defaults_with_sources() {
        let cfg = parse_yaml(
            "defaults:\n  engine: claude\nengines:\n  cheap:\n    type: llm_api\n    provider: openai\n    model: gpt-5-mini\n    api_key_env: OPENAI_API_KEY\norgs:\n  - org: acme\n    engine: codex\nrepos:\n  - name: acme/api\n    engine: cheap\n  - name: acme/web\n  - name: solo/lib\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        let engines: Vec<_> = eff.iter().map(|e| e.engine.as_str()).collect();
        assert_eq!(engines, ["cheap", "codex", "claude"]);
        let sources: Vec<_> = cfg
            .repos
            .iter()
            .map(|r| cfg.effective_sources(r)["engine"])
            .collect();
        assert_eq!(
            sources,
            [ValueSource::Repo, ValueSource::Org, ValueSource::Defaults]
        );
        match &eff[0].resolved_engine {
            ResolvedEngine::LlmApi(llm) => {
                assert_eq!(llm.provider, LlmProvider::Openai);
                assert_eq!(llm.model, "gpt-5-mini");
                assert_eq!(llm.api_key_env, "OPENAI_API_KEY");
            }
            other => panic!("expected llm_api, got {other:?}"),
        }
        assert_eq!(command_of(&eff[1]), "codex");
        // The effective JSON carries the resolved engine with its type.
        let json = serde_json::to_value(&eff[0]).unwrap();
        assert_eq!(json["resolved_engine"]["type"], "llm_api");
        let orgs = cfg.effective_orgs_with_sources().unwrap();
        assert_eq!(orgs[0].settings.engine, "codex");
        assert_eq!(orgs[0].sources["engine"], ValueSource::Org);
    }

    #[test]
    fn builtin_engines_are_agent_type_and_overridable() {
        let builtins = builtin_engines();
        for name in ["pi", "claude", "codex"] {
            assert_eq!(builtins[name].kind(), "agent", "{name}");
        }
        let cfg = Config::default();
        assert_eq!(
            cfg.resolve_engine("claude").unwrap(),
            builtins["claude"].clone()
        );
        assert!(cfg.resolve_engine("nope").is_none());
        let view = cfg.effective_view().unwrap();
        assert_eq!(view.engines.len(), 3);
        // agent_model reaches an agent through model_args only.
        let resolved = cfg.resolved_engine("codex", Some("gpt-5")).unwrap();
        assert_eq!(
            resolved.as_agent().unwrap().args,
            ["exec", "--json", "-", "-m", "gpt-5"]
        );
        // Design D3: claude and codex resume; pi does not (yet).
        assert!(resolved.can_resume());
        assert!(cfg.resolved_engine("claude", None).unwrap().can_resume());
        assert!(!cfg.resolved_engine("pi", None).unwrap().can_resume());
    }

    #[test]
    fn agent_session_fields_parse_and_validate() {
        let cfg = parse_yaml(
            "engines:\n  resumable:\n    type: agent\n    command: my-agent\n    args: [\"-\"]\n    session_output:\n      json_field: session_id\n    resume_args: [\"--resume\", \"{{SESSION_ID}}\"]\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let agent = cfg.engines["resumable"].as_agent().unwrap();
        assert_eq!(
            agent.session_output.as_ref().unwrap().json_field.as_deref(),
            Some("session_id")
        );
        let resolved = cfg.resolved_engine("resumable", None).unwrap();
        assert_eq!(
            resolved.as_agent().unwrap().resume_args,
            ["--resume", "{{SESSION_ID}}"]
        );

        for (yaml, needle) in [
            (
                "engines:\n  a:\n    type: agent\n    command: a\n    resume_args: [\"--resume\"]\n",
                "engines.a.resume_args must contain the {{SESSION_ID}} placeholder",
            ),
            (
                "engines:\n  a:\n    type: agent\n    command: a\n    session_output:\n      stdout_prefix: \"x:\"\n      json_field: id\n",
                "engines.a.session_output: set exactly one",
            ),
        ] {
            let err = parse_yaml(yaml).unwrap().validate().unwrap_err().to_string();
            assert!(err.contains(needle), "{err}");
        }
    }

    /// Design D11 / task 8.3: `usage_output` maps usage fields to dotted
    /// JSON paths; an unknown field name or a malformed path is rejected,
    /// and `llm_api` entries cannot carry it.
    #[test]
    fn agent_usage_output_parses_and_validates() {
        let agent = "engines:\n  a:\n    type: agent\n    command: a\n    args: [\"-\"]\n";
        let cfg = parse_yaml(&format!(
            "{agent}    usage_output:\n      input_tokens: tokens_in\n      cost_usd: stats.cost\n"
        ))
        .unwrap();
        cfg.validate().unwrap();
        let mapping = cfg
            .resolved_engine("a", None)
            .unwrap()
            .as_agent()
            .unwrap()
            .usage_output
            .clone()
            .unwrap();
        assert_eq!(mapping[&UsageField::InputTokens], "tokens_in");
        assert_eq!(mapping[&UsageField::CostUsd], "stats.cost");
        // Round-trips through a config rewrite.
        let yaml = serde_yaml::to_string(&cfg.engines["a"]).unwrap();
        assert!(
            yaml.contains("usage_output:\n  input_tokens: tokens_in"),
            "{yaml}"
        );

        let err = parse_yaml(&format!("{agent}    usage_output:\n      tokens_in: x\n"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown variant `tokens_in`"), "{err}");
        assert!(
            err.contains("input_tokens"),
            "lists the allowed fields: {err}"
        );

        let err = parse_yaml(&format!(
            "{agent}    usage_output:\n      model: \"a..b\"\n"
        ))
        .unwrap()
        .validate()
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("engines.a.usage_output.model: 'a..b' is not a JSON field path"),
            "{err}"
        );

        let llm = "engines:\n  llm:\n    type: llm_api\n    provider: openai\n    model: m\n    api_key_env: K\n    usage_output:\n      input_tokens: x\n";
        let err = parse_yaml(llm).unwrap().validate().unwrap_err().to_string();
        assert!(
            err.contains("engines.llm.usage_output is not supported on llm_api engines"),
            "{err}"
        );
    }

    /// Design D1/task 1.2: `llm_api` entries need `model` and an
    /// `api_key_env` variable name, never inline key material, and carry no
    /// session configuration.
    #[test]
    fn llm_api_validation_rejections() {
        let base = "engines:\n  llm:\n    type: llm_api\n    provider: anthropic\n";
        let ok = format!("{base}    model: claude-haiku\n    api_key_env: ANTHROPIC_API_KEY\n");
        parse_yaml(&ok).unwrap().validate().unwrap();
        for (extra, needle) in [
            ("    api_key_env: K\n", "engines.llm.model is required"),
            ("    model: m\n", "engines.llm.api_key_env is required"),
            (
                "    model: m\n    api_key_env: sk-ant-abc123\n",
                "engines.llm.api_key_env: 'sk-ant-abc123' must be an environment variable name",
            ),
            (
                "    model: m\n    api_key_env: K\n    api_key: sk-ant-abc123\n",
                "engines.llm.api_key: API keys must not be written into the config",
            ),
            (
                "    model: m\n    api_key_env: K\n    resume_args: [\"{{SESSION_ID}}\"]\n",
                "engines.llm.resume_args is not supported on llm_api engines",
            ),
            (
                "    model: m\n    api_key_env: K\n    session_output:\n      json_field: id\n",
                "engines.llm.session_output is not supported on llm_api engines",
            ),
            (
                "    model: m\n    api_key_env: K\n    base_url: ftp://x\n",
                "engines.llm.base_url",
            ),
        ] {
            let cfg = parse_yaml(&format!("{base}{extra}")).unwrap();
            let err = cfg.validate().unwrap_err();
            assert!(err.to_string().contains(needle), "{err}");
            assert_eq!(
                error_field(&err).map(|f| f.starts_with("engines.llm")),
                Some(true),
                "{err}"
            );
        }
        // openai_compatible has no default endpoint.
        let compat = "engines:\n  llm:\n    type: llm_api\n    provider: openai_compatible\n    model: m\n    api_key_env: K\n";
        let err = parse_yaml(compat).unwrap().validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("engines.llm.base_url is required for provider openai_compatible"),
            "{err}"
        );
        parse_yaml(&format!(
            "{compat}    base_url: http://localhost:11434/v1\n"
        ))
        .unwrap()
        .validate()
        .unwrap();
        // `type` is required; an unknown type is a parse error.
        assert!(parse_yaml("engines:\n  x:\n    command: a\n").is_err());
        assert!(parse_yaml("engines:\n  x:\n    type: robot\n").is_err());
    }

    // ------------------------------------------- new keys (task 1.2)

    #[test]
    fn new_keys_default_and_round_trip() {
        let cfg = Config::default();
        let eff = cfg.effective_defaults().unwrap();
        assert!(eff.comments.inline);
        assert_eq!(eff.comments.template, None);
        assert!(eff.brief.enabled);
        assert_eq!(eff.brief.engine, None);
        assert_eq!(eff.brief.timeout_seconds, 120);
        assert_eq!(eff.brief_engine(), "pi");
        assert_eq!(eff.auto_open, AutoOpen::Off);
        assert!(eff.notifications.enabled);
        // A config without the keys loads with these defaults, and the
        // serialized defaults parse back to the same values.
        let loaded = parse_yaml("repos: []\n").unwrap();
        assert_eq!(loaded.defaults, Defaults::default());
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert_eq!(parse_yaml(&yaml).unwrap(), cfg);
    }

    #[test]
    fn new_keys_merge_repo_over_org_over_defaults() {
        let cfg = parse_yaml(
            "defaults:\n  comments:\n    template: \"{{SUMMARY}}\\n{{IDENTITY}}\"\n  brief:\n    timeout_seconds: 60\n  auto_open: blocking\n  notifications:\n    enabled: false\nengines:\n  cheap:\n    type: llm_api\n    provider: openai_compatible\n    base_url: http://localhost:11434/v1\n    model: llama\n    api_key_env: LOCAL_KEY\norgs:\n  - org: acme\n    comments:\n      inline: false\n    brief:\n      engine: cheap\n    auto_open: both\nrepos:\n  - name: acme/api\n    brief:\n      enabled: false\n    auto_open: off\n  - name: solo/lib\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        let api = &eff[0];
        assert!(!api.comments.inline); // org
        assert_eq!(
            api.comments.template.as_deref(),
            Some("{{SUMMARY}}\n{{IDENTITY}}")
        ); // defaults
        assert!(!api.brief.enabled); // repo
        assert_eq!(api.brief_engine(), "cheap"); // org
        assert_eq!(api.brief.timeout_seconds, 60); // defaults
        assert_eq!(api.auto_open, AutoOpen::Off); // repo beats org
        assert!(!api.notifications.enabled); // global
        let lib = &eff[1];
        assert!(lib.comments.inline);
        assert_eq!(lib.brief_engine(), "pi");
        assert_eq!(lib.auto_open, AutoOpen::Blocking);
        let sources = cfg.effective_sources(&cfg.repos[0]);
        assert_eq!(sources["comments.inline"], ValueSource::Org);
        assert_eq!(sources["brief.enabled"], ValueSource::Repo);
        assert_eq!(sources["brief.engine"], ValueSource::Org);
        assert_eq!(sources["auto_open"], ValueSource::Repo);
        assert!(!sources.contains_key("notifications.enabled"));
    }

    #[test]
    fn comment_template_rejects_unknown_placeholders_by_name() {
        let mut cfg = Config::default();
        cfg.defaults.comments.template = Some(
            "{{VERDICT_LABEL}} {{SUMMARY}} {{BRIEF}} {{EXEC_SUMMARY}} {{BLOCKING_COUNT}} {{NIT_COUNT}} \
             {{FINDINGS_BLOCKING}} {{FINDINGS_NITS}} {{PR_KEY}} {{IDENTITY}} {not one}"
                .into(),
        );
        cfg.validate().unwrap();
        let cfg = parse_yaml(
            "repos:\n  - name: o/r\n    comments:\n      template: \"Score: {{REVIEW_SCORE}}\"\n",
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("repos[o/r].comments.template: unknown placeholder {{REVIEW_SCORE}}"),
            "{message}"
        );
        assert_eq!(
            error_field(&err).as_deref(),
            Some("repos[o/r].comments.template")
        );
        let mut cfg = Config::default();
        cfg.defaults.comments.template = Some(" ".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn legacy_executive_summary_reads_as_brief() {
        // Defaults: the legacy section alone is read as `brief`.
        let cfg = parse_yaml("defaults:\n  executive_summary:\n    enabled: false\n").unwrap();
        assert!(!cfg.defaults.brief.enabled);
        assert_eq!(cfg.defaults.brief.timeout_seconds, 120);
        assert!(!cfg.defaults.extra.contains_key("executive_summary"));
        assert!(!cfg.effective_defaults().unwrap().brief.enabled);
        // Both present: `brief` wins (whole section).
        let cfg = parse_yaml(
            "defaults:\n  executive_summary:\n    enabled: false\n    timeout_seconds: 5\n  \
             brief:\n    timeout_seconds: 30\n",
        )
        .unwrap();
        assert!(cfg.defaults.brief.enabled);
        assert_eq!(cfg.defaults.brief.timeout_seconds, 30);
        // Org/repo partials alias the same way, and `brief` wins there too.
        let cfg = parse_yaml(
            "orgs:\n  - org: acme\n    executive_summary:\n      timeout_seconds: 45\n\
             repos:\n  - name: acme/api\n    executive_summary:\n      enabled: false\n    \
             brief:\n      enabled: true\n  - name: acme/web\n    executive_summary:\n      \
             enabled: false\n",
        )
        .unwrap();
        cfg.validate().unwrap();
        let eff = cfg.effective_repos().unwrap();
        assert!(eff[0].brief.enabled);
        assert_eq!(eff[0].brief.timeout_seconds, 45);
        assert!(!eff[1].brief.enabled);
        assert!(cfg.repos[1].overrides.extra.is_empty());
        // Writes only ever use `brief`.
        let yaml = serde_yaml::to_string(&cfg).unwrap();
        assert!(!yaml.contains("executive_summary"), "{yaml}");
        assert!(yaml.contains("brief:"), "{yaml}");
        assert_eq!(parse_yaml(&yaml).unwrap(), cfg);
        // A malformed legacy section is an error naming it.
        let err = parse_yaml("repos:\n  - name: o/r\n    executive_summary: 3\n").unwrap_err();
        assert!(err.to_string().contains("executive_summary"), "{err}");
    }

    #[test]
    fn legacy_brief_notices_name_every_level() {
        let notices = legacy_brief_notices(
            "defaults:\n  executive_summary:\n    enabled: false\norgs:\n  - org: acme\n    \
             executive_summary: {}\n    brief: {}\nrepos:\n  - name: o/r\n    brief: {}\n",
        );
        assert_eq!(notices.len(), 2, "{notices:?}");
        assert!(notices[0].contains("defaults.executive_summary is deprecated"));
        assert!(notices[0].contains("renamed to `brief`"));
        assert!(notices[1].contains("orgs[acme] sets both"));
        assert!(legacy_brief_notices("defaults:\n  brief: {}\n").is_empty());
        assert!(legacy_brief_notices(": not yaml [").is_empty());
    }

    #[test]
    fn brief_engine_must_resolve() {
        let cfg = parse_yaml("orgs:\n  - org: acme\n    brief:\n      engine: ghost\n").unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string()
                .contains("orgs[acme].brief.engine: unknown engine 'ghost'"),
            "{err}"
        );
        assert_eq!(
            error_field(&err).as_deref(),
            Some("orgs[acme].brief.engine")
        );
        let cfg = parse_yaml("defaults:\n  brief:\n    timeout_seconds: 0\n").unwrap();
        assert!(cfg.validate().is_err());
    }

    /// Design D6: the master switch is global; an org/repo value is an error.
    #[test]
    fn notifications_enabled_is_global_only() {
        parse_yaml("defaults:\n  notifications:\n    enabled: false\n")
            .unwrap()
            .validate()
            .unwrap();
        for (yaml, field) in [
            (
                "orgs:\n  - org: acme\n    notifications:\n      enabled: false\n",
                "orgs[acme].notifications.enabled",
            ),
            (
                "repos:\n  - name: o/r\n    notifications:\n      enabled: true\n",
                "repos[o/r].notifications.enabled",
            ),
        ] {
            let err = parse_yaml(yaml).unwrap().validate().unwrap_err();
            assert!(err.to_string().contains("global"), "{err}");
            assert_eq!(error_field(&err).as_deref(), Some(field));
        }
    }

    /// Design D7: `menu` defaults when absent and round-trips when set.
    #[test]
    fn menu_section_defaults_and_parses() {
        let cfg = parse_yaml("repos: []\n").unwrap();
        assert_eq!(cfg.menu.sort, MenuSort::CreatedDesc);
        assert_eq!(cfg.menu.group_by, MenuGroupBy::Lifecycle);
        cfg.validate().unwrap();

        let cfg = parse_yaml("menu:\n  sort: last_review_desc\n  group_by: none\n").unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.menu.sort, MenuSort::LastReviewDesc);
        assert_eq!(cfg.menu.group_by, MenuGroupBy::None);
        let reparsed = parse_yaml(&serde_yaml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(reparsed.menu, cfg.menu);
        assert!(!reparsed.extra.contains_key("menu"));
    }

    #[test]
    fn menu_rejects_unknown_values_listing_the_allowed_ones() {
        let err = parse_yaml("menu:\n  sort: oldest\n")
            .unwrap_err()
            .to_string();
        for allowed in ["created_desc", "created_asc", "last_review_desc", "key"] {
            assert!(err.contains(allowed), "{allowed} missing from: {err}");
        }
        let err = parse_yaml("menu:\n  group_by: org\n")
            .unwrap_err()
            .to_string();
        for allowed in ["lifecycle", "repo", "none"] {
            assert!(err.contains(allowed), "{allowed} missing from: {err}");
        }
    }

    #[test]
    fn menu_is_global_only() {
        for (yaml, field) in [
            ("defaults:\n  menu:\n    sort: key\n", "defaults.menu"),
            (
                "orgs:\n  - org: acme\n    menu:\n      sort: key\n",
                "orgs[acme].menu",
            ),
            (
                "repos:\n  - name: o/r\n    menu:\n      group_by: none\n",
                "repos[o/r].menu",
            ),
        ] {
            let err = parse_yaml(yaml).unwrap().validate().unwrap_err();
            assert!(err.to_string().contains("global"), "{err}");
            assert_eq!(error_field(&err).as_deref(), Some(field));
        }
    }

    /// Design D9: menu action IDs split on `|`, so engine names exclude it.
    #[test]
    fn engine_names_must_not_contain_a_pipe() {
        let err = parse_yaml("engines:\n  \"my|agent\":\n    type: agent\n    command: x\n")
            .unwrap()
            .validate()
            .unwrap_err();
        assert!(err.to_string().contains("must not contain '|'"), "{err}");
        parse_yaml("engines:\n  my-agent.v2:\n    type: agent\n    command: x\n")
            .unwrap()
            .validate()
            .unwrap();
    }

    #[test]
    fn auto_open_rejects_unknown_values() {
        let err = parse_yaml("defaults:\n  auto_open: always\n").unwrap_err();
        assert!(err.to_string().contains("unknown variant"), "{err}");
    }

    #[test]
    fn write_atomic_replaces_content_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("prx-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.yml");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&path, b"new: true\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new: true\n");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
