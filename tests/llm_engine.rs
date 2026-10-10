//! Engine dispatch (design D1/D2, task 2.2): a review through an `llm_api`
//! engine (fake [`LlmTransport`], no network) produces the same state,
//! history, notifications, and GitHub posts as the same review through the
//! fake agent; failures retry once and fail cleanly on both paths.
//!
//! In-process: the pipeline runs with the fake `gh` binary and the process
//! env (`PRX_DATA_DIR`, `FAKE_GH_*`, `FAKE_AGENT_*`) guarded by ENV_LOCK.

mod common;

use std::path::PathBuf;
use std::sync::Mutex;

use common::*;
use prx::agent::EngineAdapter;
use prx::config::Config;
use prx::effects::EffectsDeps;
use prx::github::GhCli;
use prx::llm::{LlmHttpRequest, LlmHttpResponse, LlmTransport};
use prx::notify::{Notifier, NotifyError, TierBehavior};
use prx::review::{CompletedReview, Pipeline, ReviewOutcome};
use prx::state::{PrState, StateStore};
use serde_json::{json, Value};

static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

const OLD: &str = "2026-09-01T10:00:00Z";
const KEY_VAR: &str = "PRX_IT_LLM_ENGINE_KEY";

/// Canned LLM transport: one response per call (in order), requests kept.
struct FakeTransport {
    responses: Mutex<Vec<Result<LlmHttpResponse, String>>>,
    requests: Mutex<Vec<LlmHttpRequest>>,
}

impl FakeTransport {
    fn new(mut responses: Vec<Result<LlmHttpResponse, String>>) -> Self {
        responses.reverse();
        FakeTransport {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl LlmTransport for FakeTransport {
    async fn post_json(&self, request: LlmHttpRequest) -> Result<LlmHttpResponse, String> {
        self.requests.lock().unwrap().push(request);
        self.responses
            .lock()
            .unwrap()
            .pop()
            .unwrap_or_else(|| Err("no canned response left".to_string()))
    }
}

/// Records every notification as "title | body".
#[derive(Default)]
struct RecordingNotifier(Mutex<Vec<String>>);

impl Notifier for RecordingNotifier {
    async fn notify(
        &self,
        title: &str,
        body: &str,
        _behavior: &TierBehavior,
    ) -> Result<(), NotifyError> {
        self.0.lock().unwrap().push(format!("{title} | {body}"));
        Ok(())
    }
}

enum Engine {
    Agent {
        env: Vec<(&'static str, String)>,
    },
    Llm(Vec<Result<LlmHttpResponse, String>>),
    /// An `llm_api` engine with `provider: openai` (model `gpt-test`).
    LlmOpenai(Vec<Result<LlmHttpResponse, String>>),
    /// An `llm_api` engine whose `api_key_env` variable is unset.
    LlmWithoutKey,
}

/// Everything a review leaves behind, normalized so the two engine paths
/// are comparable (timestamps and the engine name are dropped).
#[derive(Debug, PartialEq)]
struct Observed {
    outcome: String,
    state: PrState,
    last_verdict: Value,
    counts: (u32, u32),
    attempts: u32,
    history: Vec<Value>,
    notifications: Vec<String>,
    writes: Vec<String>,
    review_body: Option<String>,
}

fn blocking_manifest() -> String {
    manifest_json(
        "changes_requested",
        &[
            &finding_json(Some("src/a.rs"), Some(12), "blocking", "fix the injection"),
            &finding_json(Some("src/b.rs"), Some(3), "nit", "rename this"),
            &finding_json(None, None, "nit", "repo-wide: add CI"),
        ],
    )
}

fn anthropic_ok(text: &str) -> Result<LlmHttpResponse, String> {
    Ok(LlmHttpResponse {
        status: 200,
        body: json!({"content": [{"type": "text", "text": text}]}).to_string(),
    })
}

/// An Anthropic messages response reporting `usage` (and the model).
fn anthropic_with_usage(text: &str, usage: Value) -> Result<LlmHttpResponse, String> {
    Ok(LlmHttpResponse {
        status: 200,
        body: json!({
            "id": "msg_01XFDUDYJgAACzvnptvVoYEL",
            "type": "message",
            "role": "assistant",
            "model": "claude-test-20260901",
            "content": [{"type": "text", "text": text}],
            "stop_reason": "end_turn",
            "usage": usage,
        })
        .to_string(),
    })
}

/// An OpenAI chat-completions response reporting `usage` (no model field,
/// as some compatible servers omit it).
fn openai_with_usage(text: &str, usage: Value) -> Result<LlmHttpResponse, String> {
    Ok(LlmHttpResponse {
        status: 200,
        body: json!({
            "id": "chatcmpl-B9MHDbslfkBeAs8l4bebGdFOJ6PeG",
            "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
            "usage": usage,
        })
        .to_string(),
    })
}

fn http_500() -> Result<LlmHttpResponse, String> {
    Ok(LlmHttpResponse {
        status: 500,
        body: "{\"error\": \"overloaded\"}".to_string(),
    })
}

const CADENCE: &str = "  poll_interval_minutes: 15\n  cooldown:\n    before_first_review_minutes: 60\n    between_re_reviews_minutes: 30\n";

fn llm_config() -> String {
    format!(
        "defaults:\n  engine: api\n{CADENCE}engines:\n  api:\n    type: llm_api\n    provider: anthropic\n    model: claude-test\n    api_key_env: {KEY_VAR}\nrepos:\n  - name: org/one\n"
    )
}

/// Run one `review org/one#1` through `engine` in a fresh environment.
/// Returns what it left behind plus the transport (for request asserts).
async fn review_once(engine: Engine) -> (Observed, Option<FakeTransport>) {
    let data = TempDir::new("llm-eng-data");
    let gh_dir = TempDir::new("llm-eng-gh");
    let agent_dir = TempDir::new("llm-eng-agent");
    let gh_path = install_fake_gh(gh_dir.path());
    let agent_path = install_fake_agent(agent_dir.path());
    write_list_fixture(
        gh_dir.path(),
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
    write_file(
        &gh_dir.child("org_one#1.diff"),
        "diff --git a/src/a.rs b/src/a.rs\n+let q = format!(\"{}\", input);\n",
    );

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
    ] {
        std::env::remove_var(var);
    }
    std::env::set_var("PRX_DATA_DIR", data.path());
    std::env::set_var("FAKE_GH_DIR", gh_dir.path());
    std::env::set_var("FAKE_GH_LOG", gh_dir.child("calls.log"));
    match &engine {
        Engine::LlmWithoutKey => std::env::remove_var(KEY_VAR),
        _ => std::env::set_var(KEY_VAR, "sk-test-key"),
    }

    let (yaml, transport) = match engine {
        Engine::Agent { env } => {
            for (k, v) in env {
                std::env::set_var(k, v);
            }
            (config_with_fake_agent(&["org/one"], &agent_path, ""), None)
        }
        Engine::Llm(responses) => (llm_config(), Some(FakeTransport::new(responses))),
        Engine::LlmOpenai(responses) => (
            llm_config()
                .replace("provider: anthropic", "provider: openai")
                .replace("model: claude-test", "model: gpt-test"),
            Some(FakeTransport::new(responses)),
        ),
        Engine::LlmWithoutKey => (llm_config(), Some(FakeTransport::new(Vec::new()))),
    };
    let config_path = data.child("config.yml");
    write_file(&config_path, &yaml);
    let config = Config::load(&config_path).unwrap();
    config.validate().unwrap();
    let repo = config.effective_repos().unwrap().remove(0);

    let gh = GhCli::with_binary(gh_path.to_string_lossy().as_ref());
    let notifier = RecordingNotifier::default();
    let store = StateStore::new(data.child("state.json"));
    let mut state = store.load().unwrap();
    let deps = EffectsDeps::for_repo(&notifier, &config.notification_tiers, &repo);
    prx::status::sync_repo(&gh, &repo, &mut state, chrono::Utc::now(), &deps)
        .await
        .unwrap();
    // Only the review's own effects are compared.
    notifier.0.lock().unwrap().clear();

    let adapter =
        EngineAdapter::with_transport(transport.unwrap_or_else(|| FakeTransport::new(Vec::new())));
    let pipeline = Pipeline {
        gh: &gh,
        writer: &gh,
        adapter: &adapter,
        notifier: &notifier,
        repo: &repo,
        tiers: &config.notification_tiers,
        store: &store,
        dry_run: false,
        fresh_session: false,
    };
    let outcome = pipeline
        .run_and_brief(&mut state, "org/one#1")
        .await
        .unwrap();

    let rec = store.load().unwrap().prs["org/one#1"].clone();
    let history = prx::history::query(&data.child("history.jsonl"), None, 0)
        .unwrap()
        .into_iter()
        .map(|entry| {
            let mut v = serde_json::to_value(entry).unwrap();
            let obj = v.as_object_mut().unwrap();
            obj.remove("ts");
            obj.remove("agent");
            v
        })
        .collect();
    let writes = std::fs::read_to_string(gh_dir.child("calls.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            !["pr list", "pr view", "pr diff", "api user"]
                .iter()
                .any(|read| l.starts_with(read))
        })
        .map(str::to_string)
        .collect();
    let observed = Observed {
        outcome: match outcome {
            // The deferred brief request carries a run timestamp.
            ReviewOutcome::Completed(c) => format!(
                "{:?}",
                ReviewOutcome::Completed(CompletedReview {
                    brief_request: None,
                    ..c
                })
            ),
            ReviewOutcome::Failed { .. } => "failed".to_string(),
            other => format!("{other:?}"),
        },
        state: rec.state,
        last_verdict: serde_json::to_value(rec.last_verdict).unwrap(),
        counts: (rec.blocking_count, rec.nit_count),
        attempts: rec.review_attempts,
        history,
        notifications: notifier.0.lock().unwrap().clone(),
        writes,
        review_body: std::fs::read_to_string(gh_dir.child("org_one#1.review-body.txt")).ok(),
    };
    // The manifest artifact is kept on both paths (A3).
    let manifests: Vec<PathBuf> = std::fs::read_dir(data.child("manifests"))
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    if matches!(observed.state, PrState::ReviewedChangesRequested) {
        assert_eq!(manifests.len(), 1, "one kept manifest: {manifests:?}");
        prx::agent::parse_manifest(&std::fs::read_to_string(&manifests[0]).unwrap()).unwrap();
    }
    let transport = Some(adapter.llm.transport);
    (observed, transport)
}

#[tokio::test]
async fn llm_review_matches_the_fake_agent_review() {
    let _guard = ENV_LOCK.lock().await;
    let (agent, _) = review_once(Engine::Agent {
        env: vec![("FAKE_AGENT_MANIFEST_JSON", blocking_manifest())],
    })
    .await;
    // The model wraps the manifest in prose + a fence; extraction copes.
    let reply = format!("Here is the review:\n```json\n{}\n```", blocking_manifest());
    let (llm, transport) = review_once(Engine::Llm(vec![anthropic_ok(&reply)])).await;

    assert_eq!(agent.state, PrState::ReviewedChangesRequested);
    assert_eq!(agent.counts, (1, 2));
    assert!(!agent.writes.is_empty(), "inline + review posts expected");
    assert!(agent.review_body.is_some());
    assert_eq!(llm, agent);

    // The LLM saw the rendered prompt plus the diff, and the key only in a
    // header.
    let transport = transport.unwrap();
    let requests = transport.requests.lock().unwrap();
    // The review, then the on-demand brief (no canned reply left: it
    // fails best-effort, matching the fake agent's empty reply).
    assert_eq!(requests.len(), 2);
    let prompt = requests[0].body["messages"][0]["content"].as_str().unwrap();
    assert!(
        prompt.contains("pull request\n#1 in repository org/one"),
        "{prompt}"
    );
    assert!(prompt.contains(prx::llm::DIFF_HEADING));
    assert!(prompt.contains("+let q = format!"));
    // Design C1: the output contract stays last, after the inlined diff.
    let contract_at = prompt
        .find("## prx output contract (always added by prx)")
        .expect("contract in prompt");
    assert!(prompt.find(prx::llm::DIFF_HEADING).unwrap() < contract_at);
    assert!(
        prompt.trim_end().ends_with("Do not post to GitHub."),
        "{prompt}"
    );
    assert!(!requests[0].body.to_string().contains("sk-test-key"));
}

#[tokio::test]
async fn llm_failure_retries_once_then_fails_like_the_agent() {
    let _guard = ENV_LOCK.lock().await;
    let (agent, _) = review_once(Engine::Agent {
        env: vec![("FAKE_AGENT_EXIT", "1".to_string())],
    })
    .await;
    let (llm, transport) = review_once(Engine::Llm(vec![http_500(), http_500()])).await;

    assert_eq!(agent.outcome, "failed");
    assert_eq!(agent.state, PrState::Pending);
    assert_eq!(agent.attempts, 1);
    assert!(agent.writes.is_empty());
    assert_eq!(llm, agent);
    // Exactly two calls: the attempt and its one retry.
    assert_eq!(transport.unwrap().requests.lock().unwrap().len(), 2);

    // A missing key fails the same way, naming nothing secret, and makes
    // no HTTP call.
    let (missing, transport) = review_once(Engine::LlmWithoutKey).await;
    assert_eq!(missing, agent);
    assert!(transport.unwrap().requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn llm_retry_succeeds_after_a_truncated_reply() {
    let _guard = ENV_LOCK.lock().await;
    let manifest = blocking_manifest();
    let truncated = &manifest[..manifest.len() / 2];
    let (llm, transport) = review_once(Engine::Llm(vec![
        anthropic_ok(truncated),
        anthropic_ok(&manifest),
    ]))
    .await;
    assert_eq!(llm.state, PrState::ReviewedChangesRequested);
    assert_eq!(llm.attempts, 0);
    assert_eq!(
        llm.history
            .iter()
            .map(|h| h["event"].as_str().unwrap_or_default().to_string())
            .collect::<Vec<_>>(),
        // Newest first; the in-attempt retry emits no second start marker.
        vec!["review_completed", "review_started"]
    );
    // Two review attempts plus the (failing) on-demand brief.
    assert_eq!(transport.unwrap().requests.lock().unwrap().len(), 3);
}

/// Design B2 through the LLM transport: after a review whose manifest has
/// no brief, the on-demand brief prompt (PR metadata plus the diff as
/// untrusted data, no system prompt) goes to the engine and the reply is
/// stored on the review's history entry; the review is unchanged.
#[tokio::test]
async fn llm_engine_generates_the_brief_after_the_review() {
    let _guard = ENV_LOCK.lock().await;
    let (llm, transport) = review_once(Engine::Llm(vec![
        anthropic_ok(&blocking_manifest()),
        anthropic_ok("  The PR adds a query; one blocking injection issue.\n"),
    ]))
    .await;
    assert_eq!(llm.state, PrState::ReviewedChangesRequested);
    assert_eq!(
        llm.history[0]["brief"],
        "The PR adds a query; one blocking injection issue."
    );
    let transport = transport.unwrap();
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let prompt = requests[1].body["messages"][0]["content"].as_str().unwrap();
    assert!(
        prompt.contains("Write a brief of a pull request"),
        "{prompt}"
    );
    assert!(prompt.contains("untrusted data"), "{prompt}");
    assert!(prompt.contains("Pull request: org/one#1"), "{prompt}");
    assert!(prompt.contains("-----BEGIN PR DIFF-----"), "{prompt}");
    assert!(prompt.contains("+let q = format!"), "{prompt}");
    assert!(!prompt.contains("fix the injection"), "{prompt}");
    assert!(requests[1].body.get("system").is_none());
}

/// Task 8.4: Anthropic usage (cache fields included) lands on the review's
/// history entry with the response's model; the summary call's usage is
/// stored apart as `summary_usage` (design D12).
#[tokio::test]
async fn anthropic_usage_is_recorded_for_the_review_and_the_summary() {
    let _guard = ENV_LOCK.lock().await;
    let (llm, _) = review_once(Engine::Llm(vec![
        anthropic_with_usage(
            &blocking_manifest(),
            json!({"input_tokens": 1200, "output_tokens": 340, "cache_read_input_tokens": 800, "cache_creation_input_tokens": 100, "service_tier": "standard"}),
        ),
        anthropic_with_usage(
            "One blocking injection issue.",
            json!({"input_tokens": 300, "output_tokens": 40}),
        ),
    ]))
    .await;
    let completed = &llm.history[0];
    assert_eq!(completed["event"], "review_completed");
    assert_eq!(
        completed["usage"],
        json!({"input_tokens": 1200, "output_tokens": 340, "cache_read_tokens": 800, "cache_write_tokens": 100, "model": "claude-test-20260901"})
    );
    assert_eq!(
        completed["summary_usage"],
        json!({"input_tokens": 300, "output_tokens": 40, "model": "claude-test-20260901"})
    );
}

/// Task 8.4: OpenAI-shaped usage (`prompt_tokens` / `completion_tokens`;
/// the cached prompt tokens are a subset of `prompt_tokens`, so they are
/// stored as cache reads and `input_tokens` is the uncached rest); a response without `model` records
/// the engine's configured model. A reply without usage records none.
#[tokio::test]
async fn openai_usage_is_recorded_with_the_configured_model() {
    let _guard = ENV_LOCK.lock().await;
    let (llm, _) = review_once(Engine::LlmOpenai(vec![
        openai_with_usage(
            &blocking_manifest(),
            json!({"prompt_tokens": 900, "completion_tokens": 120, "total_tokens": 1020, "prompt_tokens_details": {"cached_tokens": 256}}),
        ),
        openai_with_usage("Summary.", json!({"prompt_tokens": "n/a"})),
    ]))
    .await;
    let completed = &llm.history[0];
    assert_eq!(llm.state, PrState::ReviewedChangesRequested);
    assert_eq!(
        completed["usage"],
        json!({"input_tokens": 900 - 256, "output_tokens": 120, "cache_read_tokens": 256, "model": "gpt-test"})
    );
    // Garbled summary usage: the summary is stored, its usage is unknown.
    assert_eq!(completed["brief"], "Summary.");
    assert!(completed.get("summary_usage").is_none(), "{completed}");
}

/// Design D11: a failed run records the usage its attempts reported (the
/// truncated reply consumed tokens; the HTTP 500 retry reported none), and
/// a retry that succeeds counts both attempts.
#[tokio::test]
async fn failed_and_retried_runs_record_partial_usage() {
    let _guard = ENV_LOCK.lock().await;
    let manifest = blocking_manifest();
    let truncated = &manifest[..manifest.len() / 2];
    let partial = json!({"input_tokens": 1000, "output_tokens": 8192});
    let (failed, _) = review_once(Engine::Llm(vec![
        anthropic_with_usage(truncated, partial.clone()),
        http_500(),
    ]))
    .await;
    assert_eq!(failed.outcome, "failed");
    assert_eq!(failed.history[0]["event"], "review_failed");
    assert_eq!(
        failed.history[0]["usage"],
        json!({"input_tokens": 1000, "output_tokens": 8192, "model": "claude-test-20260901"})
    );

    let (retried, _) = review_once(Engine::Llm(vec![
        anthropic_with_usage(truncated, partial),
        anthropic_with_usage(
            &manifest,
            json!({"input_tokens": 1000, "output_tokens": 300}),
        ),
    ]))
    .await;
    assert_eq!(retried.history[0]["event"], "review_completed");
    assert_eq!(retried.history[0]["usage"]["input_tokens"], 2000);
    assert_eq!(retried.history[0]["usage"]["output_tokens"], 8492);
}
