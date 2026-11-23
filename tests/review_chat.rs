//! Review Chat (`prx chat`): the real binary decides between reopening a
//! session and starting one, records the chat, and execs the agent. The
//! agent here is a recording stand-in that writes its argv (NUL-separated),
//! its working directory, and its PRX_* environment, then exits. No real
//! agent, no `gh`, no network.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use common::*;

const RECORDER: &str = "#!/bin/sh\n\
pwd -P > \"$REC_DIR/cwd\"\n\
: > \"$REC_DIR/argv\"\n\
for a in \"$@\"; do printf '%s\\0' \"$a\" >> \"$REC_DIR/argv\"; done\n\
printf '%s#%s' \"$PRX_REPO\" \"$PRX_PR_NUMBER\" > \"$REC_DIR/env\"\n";

const HOSTILE_TITLE: &str =
    "Fix it'; touch \"$REC_DIR/pwned\"; echo \"$(touch $REC_DIR/pwned2)\" `id` {{SESSION_ID}}";

struct Env {
    _root: TempDir,
    config: PathBuf,
    data: PathBuf,
    rec: PathBuf,
    agent: PathBuf,
    review_dir: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Env {
        let root = TempDir::new(tag);
        let bin = root.child("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let agent = bin.join("chat-agent");
        std::fs::write(&agent, RECORDER).unwrap();
        std::fs::set_permissions(&agent, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rec = root.child("rec");
        std::fs::create_dir_all(&rec).unwrap();
        let review_dir = root.child("review-cwd");
        std::fs::create_dir_all(&review_dir).unwrap();
        let config = root.child("config.yml");
        let data = root.child("data");
        std::fs::create_dir_all(&data).unwrap();
        write_file(
            &config,
            &format!(
                "defaults:\n  engine: fake\nengines:\n  fake:\n    type: agent\n    command: {agent}\n    \
                 args: [\"{{{{PROMPT}}}}\"]\n    model_args: [\"--model\", \"{{{{MODEL}}}}\"]\n    chat:\n      \
                 new_args: [\"--sid\", \"{{{{SESSION_ID}}}}\", \"{{{{PROMPT}}}}\"]\n      \
                 resume_args: [\"--resume\", \"{{{{SESSION_ID}}}}\"]\n  \
                 api:\n    type: llm_api\n    provider: anthropic\n    model: m\n    api_key_env: K\n\
                 repos:\n  - name: org/one\n  - name: org/api\n    engine: api\n",
                agent = agent.display()
            ),
        );
        Env {
            _root: root,
            config,
            data,
            rec,
            agent,
            review_dir,
        }
    }

    fn write_state(&self, title: &str, session: Option<(&str, &str)>) {
        let mut pr = serde_json::json!({
            "repo": "org/one", "number": 3, "title": title, "author": "alice",
            "head_branch": "feat/x", "head_sha": "abc123", "is_draft": false,
            "gh_created_at": "2026-09-01T10:00:00Z",
            "state": "reviewed_changes_requested",
            "previous_state": null, "skip_reason": null,
            "snooze_until": null, "snooze_until_push": false,
            "review_anchor_at": "2026-09-01T10:00:00Z",
            "last_reviewed_sha": "abc123", "last_review_at": "2026-09-01T11:00:00Z",
            "last_verdict": "changes_requested", "last_summary": "1 blocker",
            "blocking_count": 1, "nit_count": 0,
            "review_session": null, "review_attempts": 0, "diff_too_large": false,
            "updated_at": "2026-09-01T11:00:00Z"
        });
        if let Some((id, engine)) = session {
            pr["review_session"] = id.into();
            pr["review_session_info"] =
                serde_json::json!({ "engine": engine, "workdir": self.review_dir });
        }
        let mut api = pr.clone();
        api["repo"] = "org/api".into();
        api["review_session"] = serde_json::Value::Null;
        let state = serde_json::json!({
            "schema_version": 1,
            "prs": { "org/one#3": pr, "org/api#5": api },
        });
        write_file(
            &self.data.join("state.json"),
            &serde_json::to_string(&state).unwrap(),
        );
    }

    fn chat(&self, args: &[&str]) -> std::process::Output {
        let rec = self.rec.display().to_string();
        let mut all = vec!["chat"];
        all.extend_from_slice(args);
        run_cli_isolated(&self.config, &self.data, &all, None, &[("REC_DIR", &rec)])
    }

    fn argv(&self) -> Vec<String> {
        let raw = std::fs::read(self.rec.join("argv")).expect("the agent ran");
        raw.split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect()
    }

    fn cwd(&self) -> PathBuf {
        PathBuf::from(
            std::fs::read_to_string(self.rec.join("cwd"))
                .unwrap()
                .trim_end(),
        )
    }

    fn chats(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(self.data.join("chats.json")).unwrap())
            .unwrap()
    }

    fn reset(&self) {
        let _ = std::fs::remove_file(self.rec.join("argv"));
        let _ = std::fs::remove_file(self.rec.join("cwd"));
    }
}

fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

#[test]
fn chat_reopens_the_review_session_then_its_own_chat() {
    let env = Env::new("chat-resume");
    env.write_state("Add cache", Some(("sess-7", "fake")));

    let out = env.chat(&["org/one#3"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("of the last review"),
        "{}",
        stderr_of(&out)
    );
    assert_eq!(env.argv(), ["--resume", "sess-7"]);
    assert_eq!(env.cwd(), canon(&env.review_dir));
    assert_eq!(
        std::fs::read_to_string(env.rec.join("env")).unwrap(),
        "org/one#3"
    );
    let chat = &env.chats()["chats"]["org/one#3"];
    assert_eq!(chat["engine"], "fake");
    assert_eq!(chat["session_id"], "sess-7");
    assert_eq!(chat["origin"], "review");

    // The next chat (here by short form) resumes the recorded chat.
    env.reset();
    let out = env.chat(&["one#3"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("chat session sess-7"));
    assert_eq!(env.argv(), ["--resume", "sess-7"]);
}

#[test]
fn chat_starts_a_new_session_with_the_prompt_as_one_argument() {
    let env = Env::new("chat-new");
    env.write_state(HOSTILE_TITLE, None);

    let out = env.chat(&["org/one#3"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let argv = env.argv();
    assert_eq!(argv.len(), 3, "{argv:?}");
    assert_eq!(argv[0], "--sid");
    let id = argv[1].clone();
    assert_eq!(id.len(), 36, "{id}");
    // The whole prompt is the last argument: rules, then the title verbatim.
    let prompt = &argv[2];
    assert!(
        prompt.contains("Never approve the pull request"),
        "{prompt}"
    );
    assert!(
        prompt.contains(&format!("Title: {HOSTILE_TITLE}\n")),
        "{prompt}"
    );
    assert!(prompt.contains("Verdict: changes requested (1 blocking, 0 nits)"));
    // Nothing in the title ran.
    assert!(!env.rec.join("pwned").exists());
    assert!(!env.rec.join("pwned2").exists());
    // It ran in the PR's own chat directory, which is private.
    let dir = env.data.join("chats/org/one/3");
    assert_eq!(env.cwd(), canon(&dir));
    let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    let chat = &env.chats()["chats"]["org/one#3"];
    assert_eq!(chat["session_id"], id.as_str());
    assert_eq!(chat["origin"], "new");

    // Reopening uses the id prx chose.
    env.reset();
    let out = env.chat(&["org/one#3"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(env.argv(), ["--resume", id.as_str()]);

    // --new starts over with a fresh id.
    env.reset();
    let out = env.chat(&["org/one#3", "--new"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let argv = env.argv();
    assert_eq!(argv[0], "--sid");
    assert_ne!(argv[1], id);
}

#[test]
fn chat_dry_run_prints_the_plan_and_changes_nothing() {
    let env = Env::new("chat-dry");
    env.write_state("Add cache", None);
    let out = env.chat(&["org/one#3", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("starting a new fake session"), "{stdout}");
    assert!(
        stdout.contains(&format!("'{}' '--sid'", env.agent.display())),
        "{stdout}"
    );
    assert!(stdout.trim_end().ends_with("<prompt>"), "{stdout}");
    assert!(!env.rec.join("argv").exists());
    assert!(!env.data.join("chats.json").exists());
}

#[test]
fn chat_with_an_llm_api_engine_asks_for_an_agent_and_dir_starts_new() {
    let env = Env::new("chat-llm");
    env.write_state("Add cache", None);
    let out = env.chat(&["org/api#5"]);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("llm_api engine") && err.contains("--engine"),
        "{err}"
    );
    assert!(!env.data.join("chats.json").exists());

    // --engine picks an agent; --dir runs it in the given directory.
    let clone = env.data.join("my-clone");
    std::fs::create_dir_all(&clone).unwrap();
    let clone_arg = clone.display().to_string();
    let out = env.chat(&["org/api#5", "--engine", "fake", "--dir", &clone_arg]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(env.argv()[0], "--sid");
    assert_eq!(env.cwd(), canon(&clone));

    // A missing --dir is an error, before anything runs.
    env.reset();
    let out = env.chat(&["org/api#5", "--engine", "fake", "--dir", "/nonexistent/x"]);
    assert!(!out.status.success());
    assert!(!env.rec.join("argv").exists());
}

#[test]
fn chat_reports_a_missing_agent_and_keeps_no_record() {
    let env = Env::new("chat-missing");
    env.write_state("Add cache", None);
    std::fs::remove_file(&env.agent).unwrap();
    let out = env.chat(&["org/one#3"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not found"), "{}", stderr_of(&out));
    let chats = env.chats();
    assert!(chats["chats"].get("org/one#3").is_none(), "{chats}");
}
