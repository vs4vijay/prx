//! Discovery for the settings app: which review engines this machine can
//! run (agent CLIs it has, plus the configured `llm_api` entries and
//! whether their key variable is set), and which repositories the gh
//! account can see. Served by the daemon (`GET /discover/agents`,
//! `GET /discover/repos`) because the daemon is the process that spawns
//! agents, calls LLM APIs, and runs gh — what it can find is what will run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{builtin_agents, EnginesMap, LlmProvider, MODEL_PLACEHOLDER};

/// Pages of 100 repositories fetched for the repo picker (most recently
/// pushed first).
pub const REPO_PAGES: u32 = 5;

/// One agent CLI prx knows how to drive (an agent-type engine candidate),
/// and whether it was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedAgent {
    pub name: String,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub model_args: Vec<String>,
    /// Model names worth suggesting (never exhaustive; any name is accepted).
    #[serde(default)]
    pub models: Vec<String>,
    /// Built into prx: usable without an `engines:` entry.
    pub builtin: bool,
    /// Where the binary was found, if anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Found on the daemon's own `PATH` (runnable by bare command name);
    /// false with a `path` means it sits in a well-known directory the
    /// daemon's `PATH` does not include.
    #[serde(default)]
    pub on_path: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentsResponse {
    /// Agent-type engine candidates.
    pub agents: Vec<DetectedAgent>,
    /// Resolved binary of each agent-type `engines:` entry in the running
    /// config (`None` = not found), keyed by engine name.
    #[serde(default)]
    pub configured: BTreeMap<String, Option<PathBuf>>,
    /// The configured `llm_api` engines (design D1).
    #[serde(default)]
    pub llm_apis: Vec<LlmApiCandidate>,
}

/// A configured `llm_api` engine as the settings app lists it. The key
/// itself is never read into the response: only whether its variable is
/// set in the daemon's environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmApiCandidate {
    pub name: String,
    pub provider: LlmProvider,
    pub model: String,
    pub api_key_env: String,
    /// `api_key_env` is set (non-empty) in the daemon's environment.
    pub key_present: bool,
}

/// One repository visible to the gh account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSummary {
    /// `owner/repo`.
    pub name: String,
    #[serde(default)]
    pub private: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReposResponse {
    pub repos: Vec<RepoSummary>,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// The agent CLIs prx can set up: the built-ins plus presets for other
/// common ones (added to `agents:` from the settings app).
pub fn known_agents() -> Vec<DetectedAgent> {
    let builtin = builtin_agents();
    let labels = [
        ("pi", "Pi", &[][..]),
        ("claude", "Claude Code", &["opus", "sonnet", "haiku"][..]),
        ("codex", "Codex CLI", &["gpt-5-codex", "gpt-5"][..]),
    ];
    let mut agents: Vec<DetectedAgent> = labels
        .iter()
        .filter_map(|(name, label, models)| {
            let agent = builtin.get(*name)?;
            Some(DetectedAgent {
                name: name.to_string(),
                label: label.to_string(),
                command: agent.command.clone(),
                args: agent.args.clone(),
                model_args: agent.model_args.clone(),
                models: strings(models),
                builtin: true,
                path: None,
                on_path: false,
            })
        })
        .collect();
    // (name, label, args, model args, suggested models)
    type Preset = (
        &'static str,
        &'static str,
        &'static [&'static str],
        &'static [&'static str],
        &'static [&'static str],
    );
    let presets: [Preset; 5] = [
        (
            "gemini",
            "Gemini CLI",
            &["-p", "{{PROMPT}}", "--yolo"],
            &["-m", MODEL_PLACEHOLDER],
            &["gemini-2.5-pro", "gemini-2.5-flash"],
        ),
        (
            "opencode",
            "OpenCode",
            &["run", "{{PROMPT}}"],
            &["--model", MODEL_PLACEHOLDER],
            &[],
        ),
        (
            "cursor-agent",
            "Cursor Agent",
            &["-p", "{{PROMPT}}", "--force"],
            &["--model", MODEL_PLACEHOLDER],
            &[],
        ),
        (
            "copilot",
            "GitHub Copilot CLI",
            &["-p", "{{PROMPT}}", "--allow-all-tools"],
            &["--model", MODEL_PLACEHOLDER],
            &[],
        ),
        (
            "amp",
            "Amp",
            &["-x", "{{PROMPT}}", "--dangerously-allow-all"],
            &[],
            &[],
        ),
    ];
    agents.extend(
        presets
            .iter()
            .map(|(name, label, args, model_args, models)| DetectedAgent {
                name: name.to_string(),
                label: label.to_string(),
                command: name.to_string(),
                args: strings(args),
                model_args: strings(model_args),
                models: strings(models),
                builtin: false,
                path: None,
                on_path: false,
            }),
    );
    agents
}

/// Directories agent CLIs commonly install into that a launchd/systemd
/// daemon's `PATH` often lacks.
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for rel in [
            ".local/bin",
            ".bun/bin",
            ".cargo/bin",
            ".npm-global/bin",
            ".claude/local",
            ".opencode/bin",
            ".volta/bin",
        ] {
            dirs.push(home.join(rel));
        }
    }
    dirs
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Find `command` in `dirs` (an explicit path is checked as-is).
pub fn find_in(command: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    if command.is_empty() {
        return None;
    }
    if command.contains('/') {
        let path = PathBuf::from(command);
        return is_executable(&path).then_some(path);
    }
    dirs.iter()
        .map(|dir| dir.join(command))
        .find(|candidate| is_executable(candidate))
}

/// [`known_agents`] with `path`/`on_path` filled in for this process's
/// environment. Blocking (filesystem probes).
pub fn detect_agents() -> Vec<DetectedAgent> {
    let on_path = path_dirs();
    let extra = extra_dirs();
    known_agents()
        .into_iter()
        .map(|mut agent| {
            if let Some(found) = find_in(&agent.command, &on_path) {
                agent.path = Some(found);
                agent.on_path = true;
            } else {
                agent.path = find_in(&agent.command, &extra);
            }
            agent
        })
        .collect()
}

/// Where each configured agent-type engine's command resolves to
/// (`None` = nowhere).
pub fn resolve_configured(engines: &EnginesMap) -> BTreeMap<String, Option<PathBuf>> {
    let mut dirs = path_dirs();
    dirs.extend(extra_dirs());
    engines
        .iter()
        .filter_map(|(name, engine)| Some((name, engine.as_agent()?)))
        .map(|(name, agent)| (name.clone(), find_in(&agent.command, &dirs)))
        .collect()
}

/// Whether the environment variable `name` is set to a non-empty value.
pub fn env_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// The configured `llm_api` engines, with key-variable presence.
pub fn llm_api_candidates(engines: &EnginesMap) -> Vec<LlmApiCandidate> {
    engines
        .iter()
        .filter_map(|(name, engine)| {
            let llm = engine.as_llm_api()?;
            Some(LlmApiCandidate {
                name: name.clone(),
                provider: llm.provider,
                model: llm.model.clone(),
                api_key_env: llm.api_key_env.clone(),
                key_present: env_present(&llm.api_key_env),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_agents_cover_the_builtins_and_always_take_the_prompt() {
        let known = known_agents();
        for name in builtin_agents().keys() {
            assert!(known.iter().any(|a| &a.name == name && a.builtin), "{name}");
        }
        let names: std::collections::BTreeSet<_> = known.iter().map(|a| &a.name).collect();
        assert_eq!(names.len(), known.len(), "duplicate agent names");
        for agent in &known {
            crate::agent::build_args(&agent.args, "p")
                .unwrap_or_else(|e| panic!("{}: {e}", agent.name));
            assert!(
                agent.model_args.is_empty()
                    || agent
                        .model_args
                        .iter()
                        .any(|a| a.contains(MODEL_PLACEHOLDER)),
                "{}",
                agent.name
            );
        }
    }

    #[test]
    fn configured_engines_split_by_type() {
        let engines: EnginesMap = serde_yaml::from_str(
            "mine:\n  type: agent\n  command: definitely-not-installed-prx\n  args: [\"-\"]\n\
             cheap:\n  type: llm_api\n  provider: anthropic\n  model: claude-haiku\n  \
             api_key_env: PRX_TEST_DISCOVER_UNSET_KEY\n",
        )
        .unwrap();
        let configured = resolve_configured(&engines);
        assert_eq!(configured.len(), 1);
        assert_eq!(configured["mine"], None);
        let llms = llm_api_candidates(&engines);
        assert_eq!(llms.len(), 1);
        assert_eq!(llms[0].name, "cheap");
        assert_eq!(llms[0].provider, LlmProvider::Anthropic);
        assert!(!llms[0].key_present);
    }

    #[test]
    fn find_in_only_accepts_executables() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("prx-discover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("tool");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let dirs = vec![dir.clone()];
        assert_eq!(find_in("tool", &dirs), None);
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(find_in("tool", &dirs), Some(tool.clone()));
        assert_eq!(find_in(tool.to_str().unwrap(), &[]), Some(tool));
        assert_eq!(find_in("missing", &dirs), None);
        assert_eq!(find_in("", &dirs), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// PATH order decides between two installs, a directory named like the
    /// command is not a binary, and a path is taken as written.
    #[test]
    fn find_in_follows_dir_order_and_skips_directories() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("prx-discover-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (first, second, third) = (root.join("first"), root.join("second"), root.join("third"));
        // `first/tool` is a directory; `second` and `third` hold the binary.
        std::fs::create_dir_all(first.join("tool")).unwrap();
        for dir in [&second, &third] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("tool"), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(dir.join("tool"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let missing = root.join("does-not-exist");
        let dirs = vec![missing, first.clone(), second.clone(), third.clone()];
        assert_eq!(find_in("tool", &dirs), Some(second.join("tool")));
        assert_eq!(
            find_in("tool", &[third.clone(), second]),
            Some(third.join("tool"))
        );
        // An explicit path ignores the dirs, and must itself be executable.
        let explicit = third.join("tool");
        assert_eq!(find_in(explicit.to_str().unwrap(), &[]), Some(explicit));
        assert_eq!(find_in(first.join("tool").to_str().unwrap(), &dirs), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Invariant 7: discovery reports whether an `llm_api` key variable is
    /// set (non-empty), and nothing of its value.
    #[test]
    fn llm_candidates_report_key_presence_and_never_the_key() {
        let var = format!("PRX_TEST_DISCOVER_KEY_{}", std::process::id());
        let engines: EnginesMap = serde_yaml::from_str(&format!(
            "api:\n  type: llm_api\n  provider: openai_compatible\n  base_url: http://127.0.0.1:1/v1\n  \
             model: local\n  api_key_env: {var}\n"
        ))
        .unwrap();
        std::env::set_var(&var, "sk-discover-secret");
        assert!(env_present(&var));
        let found = llm_api_candidates(&engines);
        assert_eq!(
            found,
            [LlmApiCandidate {
                name: "api".into(),
                provider: LlmProvider::OpenaiCompatible,
                model: "local".into(),
                api_key_env: var.clone(),
                key_present: true,
            }]
        );
        let response = AgentsResponse {
            llm_apis: found,
            ..Default::default()
        };
        let wire = format!("{} {response:?}", serde_json::to_string(&response).unwrap());
        assert!(
            wire.contains(&var) && !wire.contains("sk-discover-secret"),
            "{wire}"
        );

        // Set but empty counts as not set.
        std::env::set_var(&var, "");
        assert!(!env_present(&var));
        assert!(!llm_api_candidates(&engines)[0].key_present);
        std::env::remove_var(&var);
        assert!(!env_present(&var));
    }

    /// Detection fills in where each known agent is, and never invents a
    /// location: a reported path is an executable file named after the
    /// command, and `on_path` implies a path.
    #[test]
    fn detected_agents_only_report_real_executables() {
        let detected = detect_agents();
        assert_eq!(detected.len(), known_agents().len());
        for agent in &detected {
            match &agent.path {
                Some(path) => {
                    assert!(is_executable(path), "{}: {}", agent.name, path.display());
                    assert_eq!(path.file_name().unwrap().to_str().unwrap(), agent.command);
                }
                None => assert!(!agent.on_path, "{}", agent.name),
            }
        }
    }
}
