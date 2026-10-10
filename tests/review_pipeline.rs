//! `prx review` / `prx approve` end-to-end under the fake gh + fake agent:
//! manifest → inline comments → review body (correct flag), dry-run zero
//! writes, timeout/invalid-manifest → retry → failure path, diff cap skip,
//! A22 force vs cooldown semantics, and A12 prompt inheritance.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::*;

/// One test environment: temp config/data/gh dirs plus the fake agent path.
struct Env {
    /// Held so the config dir lives as long as the env.
    _cfg_dir: TempDir,
    data_dir: TempDir,
    gh_dir: TempDir,
    agent_dir: TempDir,
    agent_path: PathBuf,
    config: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let cfg_dir = TempDir::new(&format!("{tag}-cfg"));
        let data_dir = TempDir::new(&format!("{tag}-data"));
        let gh_dir = TempDir::new(&format!("{tag}-gh"));
        let agent_dir = TempDir::new(&format!("{tag}-agent"));
        install_fake_gh(gh_dir.path());
        let agent_path = install_fake_agent(agent_dir.path());
        let config = cfg_dir.child("config.yml");
        Env {
            _cfg_dir: cfg_dir,
            data_dir,
            gh_dir,
            agent_dir,
            agent_path,
            config,
        }
    }

    /// Write the config using the fake agent; `extra` goes into `defaults:`.
    fn write_config(&self, repos: &[&str], extra: &str) {
        write_file(
            &self.config,
            &config_with_fake_agent(repos, &self.agent_path, extra),
        );
    }

    /// Set `defaults.review_prompt` to inline prompt text (design F5) in
    /// the config written by `write_config` (a JSON string is a valid YAML
    /// double-quoted scalar).
    fn write_default_prompt(&self, content: &str) {
        let config = std::fs::read_to_string(&self.config).unwrap();
        let quoted = serde_json::to_string(content).unwrap();
        let config = config.replacen(
            "defaults:\n",
            &format!("defaults:\n  review_prompt: {quoted}\n"),
            1,
        );
        write_file(&self.config, &config);
    }

    fn run(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
        run_cli(
            &self.config,
            self.data_dir.path(),
            args,
            Some(self.gh_dir.path()),
            extra_env,
        )
    }

    fn state(&self) -> prx::state::StateFile {
        prx::state::StateStore::new(self.data_dir.child("state.json"))
            .load()
            .expect("state.json should parse")
    }

    /// Outcome history: every entry except the per-run `review_started`
    /// markers (design D6) — see [`Env::raw_history`].
    fn history(&self) -> Vec<prx::history::HistoryEntry> {
        self.raw_history()
            .into_iter()
            .filter(|e| !matches!(e, prx::history::HistoryEntry::ReviewStarted { .. }))
            .collect()
    }

    /// Every history entry, `review_started` included (newest first).
    fn raw_history(&self) -> Vec<prx::history::HistoryEntry> {
        prx::history::query(&self.data_dir.child("history.jsonl"), None, 0).unwrap()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.gh_dir.child("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.to_string())
            .collect()
    }
}

const OLD: &str = "2026-09-01T10:00:00Z"; // long past any cooldown
const DEFAULT_PROMPT: &str =
    "BASE-PROMPT: review PR #{{PR_NUMBER}} in {{REPO}}; manifest at {{MANIFEST_PATH}}.\n";

fn blocking_manifest() -> String {
    manifest_json(
        "changes_requested",
        &[
            &finding_json(
                Some("src/a.rs"),
                Some(12),
                "blocking",
                "fix the sql injection",
            ),
            &finding_json(Some("src/b.rs"), Some(3), "nit", "rename this"),
            &finding_json(None, None, "nit", "repo-wide: add CI"),
        ],
    )
}

#[test]
fn review_end_to_end_changes_requested() {
    let env = Env::new("rev-block");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
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

    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str()),
            ("FAKE_AGENT_SESSION", "sess-7"),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("reviewed (changes requested)"), "{stdout}");
    assert!(
        stdout.contains("inline comments posted 2/failed 0"),
        "{stdout}"
    );

    // Inline comments went through the REST API, one per anchored finding;
    // the unanchored (no file) finding is only in the review body.
    let calls = env.calls();
    let api: Vec<&String> = calls
        .iter()
        .filter(|l| l.contains("api repos/org/one/pulls/1/comments"))
        .collect();
    assert_eq!(api.len(), 2, "calls: {calls:?}");
    assert!(api.iter().any(|l| l.contains("path=src/a.rs")
        && l.contains("line=12")
        && l.contains("body=blocking: fix the sql injection")));
    assert!(api.iter().any(|l| l.contains("path=src/b.rs")
        && l.contains("line=3")
        && l.contains("body=nit: rename this")));
    assert!(!calls
        .iter()
        .any(|l| l.contains("api") && l.contains("repo-wide")));
    // All inline comments anchor to the analyzed head commit.
    assert!(api.iter().all(|l| l.contains("commit_id=aaaa")));

    // One review body with --request-changes (blocking findings present, D2:
    // never --approve from the automated path).
    assert!(calls
        .iter()
        .any(|l| l == "pr review 1 -R org/one --request-changes --body-file -"));
    assert!(!calls.iter().any(|l| l.contains("--approve")));
    let body = std::fs::read_to_string(env.gh_dir.child("org_one#1.review-body.txt")).unwrap();
    assert!(body.contains("changes requested"), "{body}");
    assert!(body.contains("fixture summary"));
    assert!(body.contains("### Blocking (1)"));
    assert!(body.contains("`src/a.rs:12`"));
    assert!(body.contains("### Nits (2)"));
    assert!(body.contains("repo-wide: add CI"));
    assert!(body.contains("\u{2014} automated review by prx")); // A13 footer

    // State: blocking verdict + counts + reviewed sha + captured session.
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, prx::state::PrState::ReviewedChangesRequested);
    assert_eq!(
        rec.last_verdict,
        Some(prx::state::Verdict::ChangesRequested)
    );
    assert_eq!(rec.blocking_count, 1);
    assert_eq!(rec.nit_count, 2);
    assert_eq!(rec.last_reviewed_sha.as_deref(), Some("aaaa"));
    assert_eq!(rec.review_session.as_deref(), Some("sess-7"));
    assert!(!rec.diff_too_large);

    // History: one enriched review_completed entry.
    let history = env.history();
    assert_eq!(history.len(), 1, "{history:?}");
    match &history[0] {
        prx::history::HistoryEntry::ReviewCompleted {
            agent,
            verdict,
            blocking_count,
            nit_count,
            duration_seconds,
            repo,
            number,
            ..
        } => {
            assert_eq!(agent.as_deref(), Some("fake"));
            assert_eq!(*verdict, prx::state::Verdict::ChangesRequested);
            assert_eq!(*blocking_count, 1);
            assert_eq!(*nit_count, 2);
            assert_eq!(*duration_seconds, Some(42));
            assert_eq!(repo, "org/one");
            assert_eq!(*number, 1);
        }
        other => panic!("expected ReviewCompleted, got {other:?}"),
    }
}

#[test]
fn review_nit_only_posts_comment_review_and_approves_state() {
    let env = Env::new("rev-nit");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(2, "Small fix", "bob", "f/y", "bbbb", false, OLD)],
    );

    let manifest = manifest_json(
        "approved",
        &[
            &finding_json(Some("src/c.rs"), Some(7), "nit", "add a doc comment"),
            &finding_json(None, None, "nit", "consider a changelog"),
        ],
    );
    let out = env.run(
        &["review", "org/one#2"],
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    // No blocking findings → --comment, never --request-changes/--approve.
    let calls = env.calls();
    assert!(calls
        .iter()
        .any(|l| l == "pr review 2 -R org/one --comment --body-file -"));
    assert!(!calls.iter().any(|l| l.contains("--request-changes")));
    assert!(!calls.iter().any(|l| l.contains("--approve")));

    let rec = env.state().prs["org/one#2"].clone();
    assert_eq!(rec.state, prx::state::PrState::ReviewedApproved);
    assert_eq!(rec.last_verdict, Some(prx::state::Verdict::Approved));
    assert_eq!(rec.blocking_count, 0);
    assert_eq!(rec.nit_count, 2);
}

#[test]
fn review_dry_run_makes_zero_writes_and_zero_state_changes() {
    let env = Env::new("rev-dry");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
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

    // Populate the state file first (dry-run refuses to poll).
    let out = env.run(&["status"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let state_before = std::fs::read_to_string(env.data_dir.child("state.json")).unwrap();

    let manifest = blocking_manifest();
    let out = env.run(
        &["review", "org/one#1", "--dry-run"],
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[dry-run]"), "{stdout}");
    assert!(stdout.contains("no GitHub writes and no state changes"));
    assert!(stdout.contains("2 inline comment(s)"));
    assert!(stdout.contains("--request-changes"));
    assert!(stdout.contains("validated manifest"));
    assert!(stdout.contains("\"verdict\": \"changes_requested\""));

    // Zero gh WRITE calls: only reads (list for status, view for the cap).
    let calls = env.calls();
    assert!(!calls.iter().any(|l| l.contains("api repos")));
    assert!(!calls.iter().any(|l| l.contains("pr review")));
    assert!(calls.iter().any(|l| l.contains("pr list")));
    assert!(calls
        .iter()
        .any(|l| l.contains("pr view 1 -R org/one --json files,additions,deletions,commits")));

    // State file byte-identical; no history; no review body captured.
    assert_eq!(
        std::fs::read_to_string(env.data_dir.child("state.json")).unwrap(),
        state_before
    );
    assert!(!env.data_dir.child("history.jsonl").exists());
    assert!(!env.gh_dir.child("org_one#1.review-body.txt").exists());
}

#[test]
fn review_agent_timeout_kills_process_retries_once_then_fails() {
    let env = Env::new("rev-timeout");
    env.write_config(&["org/one"], "  agent_timeout_seconds: 1\n");
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "Hangs forever",
            "alice",
            "f/x",
            "aaaa",
            false,
            OLD,
        )],
    );

    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_SLEEP", "30")], // killed by prx, twice (one retry)
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("timed out after 1s"),
        "{}",
        stderr_of(&out)
    );
    assert!(stderr_of(&out).contains("retrying once")); // exactly one retry
    assert!(stdout_of(&out).contains("review FAILED"));

    // Machine ReviewFailed: back to pending with a fresh anchor, attempts=1,
    // no verdict recorded, no review posted.
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, prx::state::PrState::Pending);
    assert_eq!(rec.review_attempts, 1);
    assert_eq!(rec.last_verdict, None);
    assert!(!env.calls().iter().any(|l| l.contains("pr review")));
    assert!(!env.calls().iter().any(|l| l.contains("api repos")));

    let history = env.history();
    assert_eq!(history.len(), 1);
    assert!(matches!(
        &history[0],
        prx::history::HistoryEntry::ReviewFailed { agent: Some(a), .. } if a == "fake"
    ));
}

#[test]
fn review_invalid_manifest_retries_once_then_fails() {
    let env = Env::new("rev-badmf");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Bad output", "alice", "f/x", "aaaa", false, OLD)],
    );

    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", "{definitely not json")],
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("retrying once"),
        "{}",
        stderr_of(&out)
    );
    assert!(stdout_of(&out).contains("review FAILED"));

    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, prx::state::PrState::Pending);
    assert_eq!(rec.review_attempts, 1);
    assert_eq!(env.history().len(), 1);

    // A manifest with an unknown verdict is equally invalid.
    let out = env.run(
        &["review", "org/one#1"],
        &[(
            "FAKE_AGENT_MANIFEST_JSON",
            r#"{"manifest_version": 1, "verdict": "lgtm"}"#,
        )],
    );
    assert!(!out.status.success());
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.review_attempts, 2); // accumulated across runs
}

#[test]
fn review_diff_cap_skips_the_agent_and_posts_nothing() {
    let env = Env::new("rev-cap");
    // Files cap 2 (PR 1 has 3 files); lines cap 4000 (PR 2 has 5000 lines).
    env.write_config(
        &["org/one"],
        "  diff_max_files: 2\n  diff_max_lines: 4000\n",
    );
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[
            pr_json(1, "Huge file count", "alice", "f/x", "aaaa", false, OLD),
            pr_json(2, "Huge line count", "bob", "f/y", "bbbb", false, OLD),
        ],
    );
    write_file(
        &env.gh_dir.child("org_one#1.detail.json"),
        r#"{"files":[{"path":"a"},{"path":"b"},{"path":"c"}],"additions":10,"deletions":10,"commits":[{"oid":"aaaa"}]}"#,
    );
    write_file(
        &env.gh_dir.child("org_one#2.detail.json"),
        r#"{"files":[{"path":"a"}],"additions":3000,"deletions":2000,"commits":[{"oid":"bbbb"}]}"#,
    );

    let manifest = blocking_manifest();
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("skipped \u{2014} diff too large"));

    // NO agent ran: no manifest anywhere, no gh writes, state stays pending
    // but flagged (A14), history records the skip.
    let manifests_dir = env.data_dir.child("manifests");
    assert!(!manifests_dir.exists() || std::fs::read_dir(&manifests_dir).unwrap().next().is_none());
    assert!(!env.calls().iter().any(|l| l.contains("api repos")));
    assert!(!env.calls().iter().any(|l| l.contains("pr review")));
    let rec = env.state().prs["org/one#1"].clone();
    assert!(rec.diff_too_large);
    assert_eq!(rec.state, prx::state::PrState::Pending);
    assert_eq!(rec.last_verdict, None);

    // The line-count cap triggers the same skip for PR 2.
    let out = env.run(
        &["review", "org/one#2"],
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("diff too large"));
    assert!(env.state().prs["org/one#2"].diff_too_large);

    // History: two review_skipped entries.
    let history = env.history();
    assert_eq!(history.len(), 2, "{history:?}");
    assert!(history.iter().all(|e| matches!(
        e,
        prx::history::HistoryEntry::ReviewSkipped { reason, .. } if reason == "diff too large"
    )));

    // Status surfaces the note.
    let out = env.run(&["status"], &[]);
    assert!(stdout_of(&out).contains("diff too large"));
}

#[test]
fn review_all_respects_cooldowns_and_reviews_only_due_prs() {
    let env = Env::new("rev-all");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    let five_min_ago = (chrono::Utc::now() - chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[
            pr_json(1, "Old PR", "alice", "f/x", "aaaa", false, OLD),
            pr_json(2, "Fresh PR", "bob", "f/y", "bbbb", false, &five_min_ago),
        ],
    );

    let manifest = manifest_json("approved", &[]);
    let out = env.run(
        &["review", "--all"],
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("org/one#1"), "{stdout}");
    assert!(!stdout.contains("org/one#2 reviewed")); // never picked up

    // A22: --all respects cooldowns — PR 1 reviewed, PR 2 untouched.
    let state = env.state();
    assert_eq!(
        state.prs["org/one#1"].state,
        prx::state::PrState::ReviewedApproved
    );
    let fresh = &state.prs["org/one#2"];
    assert_eq!(fresh.state, prx::state::PrState::Pending);
    assert_eq!(fresh.last_verdict, None);
    assert_eq!(fresh.review_attempts, 0);
    assert!(env.gh_dir.child("org_one#1.review-body.txt").exists());
    assert!(!env.gh_dir.child("org_one#2.review-body.txt").exists());
}

#[test]
fn review_target_bypasses_cooldown_force_semantics() {
    let env = Env::new("rev-force");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    let five_min_ago = (chrono::Utc::now() - chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "Fresh PR",
            "alice",
            "f/x",
            "aaaa",
            false,
            &five_min_ago,
        )],
    );

    // A22: an explicit target runs the pipeline immediately despite the
    // 60-minute first-review cooldown having 55 minutes left.
    let manifest = manifest_json("approved", &[]);
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", manifest.as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        env.state().prs["org/one#1"].state,
        prx::state::PrState::ReviewedApproved
    );
}

#[test]
fn approve_happy_path_and_already_approved_are_graceful() {
    let env = Env::new("approve");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Ready", "alice", "f/x", "aaaa", false, OLD)],
    );

    let out = env.run(&["approve", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(env
        .calls()
        .iter()
        .any(|l| l == "pr review 1 -R org/one --approve"));
    assert!(stdout_of(&out).contains("approved org/one#1"));

    // gh rejects a second approval by the same account: prx reports it
    // gracefully and exits 0.
    write_file(
        &env.gh_dir.child("org_one#1.approve-error"),
        "already approved this pull request",
    );
    let out = env.run(&["approve", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("already approved"));

    // Other gh errors still fail loudly.
    write_file(
        &env.gh_dir.child("org_one#1.approve-error"),
        "network unreachable",
    );
    let out = env.run(&["approve", "org/one#1"], &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("network unreachable"));
}

/// The daemon-less approve fallback updates the tracked state through the
/// same machine event the daemon applies: a reviewed_changes_requested PR
/// leaves the attention set (reviewed_approved) after `prx approve`, and an
/// untracked PR still approves cleanly with no state entry.
#[test]
fn approve_updates_tracked_state_in_process_and_skips_untracked() {
    let env = Env::new("approve-state");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
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

    // Land the agent review first: reviewed_changes_requested (attention).
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        env.state().prs["org/one#1"].state,
        prx::state::PrState::ReviewedChangesRequested
    );

    // The human approves: gh posts the approval AND the record leaves the
    // attention set; the agent's own verdict is untouched.
    let out = env.run(&["approve", "org/one#1"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("approved org/one#1"));
    let rec = &env.state().prs["org/one#1"];
    assert_eq!(rec.state, prx::state::PrState::ReviewedApproved);
    assert_eq!(
        rec.last_verdict,
        Some(prx::state::Verdict::ChangesRequested)
    );

    // Approving a PR that was never polled still succeeds with no entry.
    let out = env.run(&["approve", "org/one#99"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("approved org/one#99"));
    assert!(!env.state().prs.contains_key("org/one#99"));
}

#[test]
fn review_prompt_inheritance_override_replaces_and_splices_parent() {
    let env = Env::new("rev-prompt");
    // Per-repo inline override with A12 inheritance: parent spliced in,
    // then extras (design F5: the override is prompt text, not a path).
    let config_with_override = |override_yaml: &str| {
        format!(
            "defaults:\n  agent: fake\n  cooldown:\n    before_first_review_minutes: 60\n    between_re_reviews_minutes: 30\nagents:\n  fake:\n    command: {}\n    args: [\"{{{{PROMPT}}}}\"]\nrepos:\n  - name: org/one\n    review_prompt: |\n{override_yaml}",
            env.agent_path.display(),
        )
    };
    write_file(
        &env.config,
        &config_with_override(
            "      {{PARENT_PROMPT}}\n      TENANT-RULES for PR {{PR_NUMBER}} in {{REPO}}\n",
        ),
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Inherited", "alice", "f/x", "aaaa", false, OLD)],
    );

    let prompt_out = env.agent_dir.child("prompt1.txt");
    let manifest = manifest_json("approved", &[]);
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
            (
                "FAKE_AGENT_PROMPT_OUT",
                prompt_out.to_str().expect("utf8 temp path"),
            ),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    // A12: the default was spliced via {{PARENT_PROMPT}} AND the override
    // extended it; placeholders are fully substituted.
    let prompt = std::fs::read_to_string(&prompt_out).unwrap();
    // The parent is the shipped default prompt (design F5).
    assert!(
        prompt.starts_with("# prx review prompt (default)"),
        "{prompt}"
    );
    assert!(prompt.contains("pull request\n#1 in repository org/one"));
    assert!(prompt.contains("TENANT-RULES for PR 1 in org/one"));
    assert!(!prompt.contains("{{"), "{prompt}"); // nothing left unsubstituted
                                                 // The manifest path the agent was told to write matches the artifact.
                                                 // (the output contract quotes it in backticks).
    let path = prompt
        .split('`')
        .find(|t| t.ends_with(".json"))
        .expect("manifest path in prompt");
    assert!(Path::new(path).exists(), "manifest artifact {path} missing");

    // Override without {{PARENT_PROMPT}} REPLACES the default entirely, and
    // every prompt ends with the output contract (design C1: read-only +
    // manifest contract), appended exactly once.
    write_file(
        &env.config,
        &config_with_override("      OVERRIDE-ONLY: strict rules.\n"),
    );
    write_list_fixture(env.gh_dir.path(), "org/one", &[]);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Inherited", "alice", "f/x", "cccc", false, OLD)],
    );
    let prompt_out2 = env.agent_dir.child("prompt2.txt");
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
            (
                "FAKE_AGENT_PROMPT_OUT",
                prompt_out2.to_str().expect("utf8 temp path"),
            ),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let prompt2 = std::fs::read_to_string(&prompt_out2).unwrap();
    assert!(
        prompt2.contains("OVERRIDE-ONLY: strict rules."),
        "{prompt2}"
    );
    assert!(!prompt2.contains("prx review prompt (default)")); // replaced, not merged
    let contract_heading = "## prx output contract (always added by prx)";
    assert_eq!(prompt2.matches(contract_heading).count(), 1, "{prompt2}");
    let contract_at = prompt2.find(contract_heading).unwrap();
    assert!(prompt2.find("OVERRIDE-ONLY").unwrap() < contract_at);
    assert!(prompt2.contains("READ-ONLY"));
    assert!(prompt2.contains("Do not post to GitHub."));
    assert!(prompt2.contains("\"manifest_version\": 1"));
}

#[test]
fn review_command_argument_errors_are_clear() {
    let env = Env::new("rev-args");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);

    // Both target and --all.
    let out = env.run(&["review", "org/one#1", "--all"], &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not both"));

    // Neither.
    let out = env.run(&["review"], &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("org/repo#123"));

    // Malformed target.
    let out = env.run(&["review", "just-a-number"], &[]);
    assert!(!out.status.success());
    // D5: the error lists every accepted reference form.
    assert!(
        stderr_of(&out).contains("org/repo#N"),
        "{}",
        stderr_of(&out)
    );
    assert!(stderr_of(&out).contains("repo#N, N, or #N"));

    // Unconfigured repo.
    let out = env.run(&["review", "other/repo#1"], &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not configured"));

    // Untracked PR (dry-run refuses to poll).
    let out = env.run(&["review", "org/one#1", "--dry-run"], &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not tracked"));

    // Config load failure (no config file at all).
    let empty = TempDir::new("rev-args-noconf");
    let out = run_cli(
        &empty.child("config.yml"),
        env.data_dir.path(),
        &["review", "org/one#1"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("prx init"));
}

/// Design D12: the effective `identity.attribution_footer` replaces the
/// built-in attribution line, and `{{REVIEWER_LOGIN}}` in the prompt is the
/// effective `identity.login`, else the gh-authenticated user. Repos without
/// an identity keep the built-in footer.
#[test]
fn review_identity_footer_and_reviewer_login_substitution() {
    let env = Env::new("rev-identity");
    write_file(
        &env.config,
        &format!(
            "defaults:\n  agent: fake\n  review_prompt: \"Review as @{{{{REVIEWER_LOGIN}}}}.\"\n  cooldown:\n    before_first_review_minutes: 60\n    between_re_reviews_minutes: 30\nagents:\n  fake:\n    command: {}\n    args: [\"{{{{PROMPT}}}}\"]\nrepos:\n  - name: org/one\n    identity:\n      login: prx-bot\n      attribution_footer: \"Ping @maya\"\n  - name: org/two\n",
            env.agent_path.display(),
        ),
    );
    for (repo, n) in [("org/one", 1), ("org/two", 2)] {
        write_list_fixture(
            env.gh_dir.path(),
            repo,
            &[pr_json(n, "Identity", "alice", "f/x", "aaaa", false, OLD)],
        );
    }
    let manifest = manifest_json("approved", &[]);
    let mut prompts = Vec::new();
    for key in ["org/one#1", "org/two#2"] {
        let prompt_out = env
            .agent_dir
            .child(&format!("{}.txt", key.replace(['/', '#'], "_")));
        let out = env.run(
            &["review", key],
            &[
                ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
                ("FAKE_AGENT_PROMPT_OUT", prompt_out.to_str().unwrap()),
            ],
        );
        assert!(out.status.success(), "{}", stderr_of(&out));
        prompts.push(std::fs::read_to_string(&prompt_out).unwrap());
    }

    // Configured login wins; unset falls back to gh's user (fixture default).
    assert!(prompts[0].contains("Review as @prx-bot."), "{}", prompts[0]);
    assert!(
        prompts[1].contains("Review as @testuser."),
        "{}",
        prompts[1]
    );

    let one = std::fs::read_to_string(env.gh_dir.child("org_one#1.review-body.txt")).unwrap();
    assert!(one.trim_end().ends_with("Ping @maya"), "{one}");
    assert!(!one.contains("automated review by prx"), "{one}");
    let two = std::fs::read_to_string(env.gh_dir.child("org_two#2.review-body.txt")).unwrap();
    assert!(two.ends_with("\u{2014} automated review by prx\n"), "{two}");
}

/// Design D3 / task 2.3: with `resume_args`, a re-review after new commits
/// resumes the PR's retained session; when the resumed run fails, the same
/// attempt retries fresh and the review completes with a clean history.
/// Without `resume_args`, the stale session is cleared and never resumed.
#[test]
fn rereview_resumes_the_session_and_falls_back_fresh_on_failure() {
    for resumable in [true, false] {
        let env = Env::new(if resumable {
            "rev-resume"
        } else {
            "rev-noresume"
        });
        env.write_config(&["org/one"], "");
        if resumable {
            let config = std::fs::read_to_string(&env.config).unwrap();
            let config = config.replacen(
                "    args: [\"{{PROMPT}}\"]\n",
                "    args: [\"{{PROMPT}}\"]\n    resume_args: [\"--resume\", \"{{SESSION_ID}}\"]\n",
                1,
            );
            write_file(&env.config, &config);
        }
        let runs_log = env.agent_dir.child("runs.log");
        let runs_log = runs_log.to_str().unwrap();
        let manifest = manifest_json("approved", &[]);
        let list = |sha: &str| {
            write_list_fixture(
                env.gh_dir.path(),
                "org/one",
                &[pr_json(3, "Resume me", "carol", "f/r", sha, false, OLD)],
            )
        };

        list("aaaa");
        let out = env.run(
            &["review", "org/one#3"],
            &[
                ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
                ("FAKE_AGENT_SESSION", "sess-1"),
                ("FAKE_AGENT_RUNS_LOG", runs_log),
            ],
        );
        assert!(out.status.success(), "{}", stderr_of(&out));
        assert_eq!(
            env.state().prs["org/one#3"].review_session.as_deref(),
            Some("sess-1")
        );

        // New commits; the resumed run is rejected, the fresh one succeeds.
        list("bbbb");
        let out = env.run(
            &["review", "org/one#3"],
            &[
                ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
                ("FAKE_AGENT_SESSION", "sess-2"),
                ("FAKE_AGENT_RUNS_LOG", runs_log),
                ("FAKE_AGENT_FAIL_RESUME", "1"),
            ],
        );
        assert!(out.status.success(), "{}", stderr_of(&out));

        let runs = std::fs::read_to_string(runs_log).unwrap();
        if resumable {
            assert_eq!(runs, "fresh\nresume sess-1\nfresh\n");
        } else {
            assert_eq!(runs, "fresh\nfresh\n");
        }
        let rec = env.state().prs["org/one#3"].clone();
        assert_eq!(rec.state, prx::state::PrState::ReviewedApproved);
        assert_eq!(rec.last_reviewed_sha.as_deref(), Some("bbbb"));
        assert_eq!(rec.review_session.as_deref(), Some("sess-2"));
        assert_eq!(rec.review_attempts, 0);

        let kinds: Vec<String> = env
            .history()
            .iter()
            .map(|h| {
                serde_json::to_value(h).unwrap()["event"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            kinds.iter().filter(|k| *k == "review_completed").count(),
            2,
            "{kinds:?}"
        );
        assert!(!kinds.iter().any(|k| k == "review_failed"), "{kinds:?}");
        // One start marker per attempt: the transparent fresh retry after a
        // failed resume happens inside the attempt and emits no second one.
        let started = env
            .raw_history()
            .iter()
            .filter(|e| matches!(e, prx::history::HistoryEntry::ReviewStarted { .. }))
            .count();
        assert_eq!(started, 2);
    }
}

// ------------------------------------- commentary (D7) + notifications (D6)

/// Install fake `osascript` / `notify-send` binaries into the PATH-prepended
/// fake gh dir: every delivered notification appends its argv to the
/// returned log (`CommandNotifier` runs one of them only for banner tiers).
fn install_fake_notifier(env: &Env) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let log = env.gh_dir.child("notify.log");
    for name in ["osascript", "notify-send"] {
        let path = env.gh_dir.child(name);
        write_file(
            &path,
            &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    log
}

fn notifications(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn inline_calls(env: &Env) -> Vec<String> {
    env.calls()
        .into_iter()
        .filter(|l| l.starts_with("api ") && l.contains("/comments"))
        .collect()
}

fn github_writes(env: &Env) -> Vec<String> {
    env.calls()
        .into_iter()
        .filter(|l| l.starts_with("pr review") || l.starts_with("api "))
        .collect()
}

/// Design D7 / task 4.2: `comments.inline: false` keeps every finding in the
/// review body and makes no inline posts; a repo-level override re-enables
/// inline comments for that repo only (repo > defaults).
#[test]
fn comments_inline_off_posts_the_body_only_and_repo_override_merges() {
    let env = Env::new("rev-inline-off");
    env.write_config(&["org/one", "org/two"], "  comments:\n    inline: false\n");
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "  - name: org/two\n",
        "  - name: org/two\n    comments:\n      inline: true\n",
        1,
    );
    write_file(&env.config, &config);
    env.write_default_prompt(DEFAULT_PROMPT);
    for (repo, n) in [("org/one", 1), ("org/two", 2)] {
        write_list_fixture(
            env.gh_dir.path(),
            repo,
            &[pr_json(n, "Inline", "alice", "f/x", "aaaa", false, OLD)],
        );
    }

    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("inline comments posted 0/failed 0"),
        "{}",
        stdout_of(&out)
    );
    assert!(inline_calls(&env).is_empty(), "calls: {:?}", env.calls());
    // The body is posted and lists every finding, located or not.
    assert!(env
        .calls()
        .iter()
        .any(|l| l == "pr review 1 -R org/one --request-changes --body-file -"));
    let body = std::fs::read_to_string(env.gh_dir.child("org_one#1.review-body.txt")).unwrap();
    for text in [
        "`src/a.rs:12` \u{2014} fix the sql injection",
        "`src/b.rs:3` \u{2014} rename this",
        "repo-wide: add CI",
    ] {
        assert!(body.contains(text), "{text} missing: {body}");
    }
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.blocking_count, 1);
    assert_eq!(rec.nit_count, 2);

    // org/two overrides inline back on.
    let out = env.run(
        &["review", "org/two#2"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let inline = inline_calls(&env);
    assert_eq!(inline.len(), 2, "calls: {:?}", env.calls());
    assert!(inline.iter().all(|l| l.contains("repos/org/two/pulls/2")));
}

/// Shipped D3 behavior re-verified (task 4.2): `review_mode: monitor` makes
/// zero GitHub writes, while state, history, and notifications still update.
#[test]
fn monitor_mode_makes_zero_github_writes_but_records_and_notifies() {
    let env = Env::new("rev-monitor");
    env.write_config(&["org/one"], "  review_mode: monitor\n");
    env.write_default_prompt(DEFAULT_PROMPT);
    let log = install_fake_notifier(&env);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Monitor", "alice", "f/x", "aaaa", false, OLD)],
    );

    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(github_writes(&env).is_empty(), "calls: {:?}", env.calls());
    assert!(!env.gh_dir.child("org_one#1.review-body.txt").exists());

    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, prx::state::PrState::ReviewedChangesRequested);
    assert_eq!(rec.blocking_count, 1);
    assert!(matches!(
        env.history()[..],
        [prx::history::HistoryEntry::ReviewCompleted { .. }]
    ));
    let delivered = notifications(&log);
    assert!(
        delivered
            .iter()
            .any(|l| l.contains("1 blocking issue(s) found")),
        "{delivered:?}"
    );
}

/// Design D7 / task 4.2: a custom `comments.template` renders exactly, with
/// the effective `identity.attribution_footer` where `{{IDENTITY}}` sits
/// (the built-in footer text when unset).
#[test]
fn custom_template_places_the_effective_attribution_footer() {
    let env = Env::new("rev-template");
    env.write_config(
        &["org/one", "org/two"],
        "  comments:\n    template: \"{{IDENTITY}} says {{VERDICT_LABEL}} on {{PR_KEY}} \
         ({{BLOCKING_COUNT}} blocking, {{NIT_COUNT}} nits)\\n\\n{{EXEC_SUMMARY}}\\n\\n\
         {{FINDINGS_BLOCKING}}\\n\"\n",
    );
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "  - name: org/one\n",
        "  - name: org/one\n    identity:\n      attribution_footer: \"prx-bot (ask @maya)\"\n",
        1,
    );
    write_file(&env.config, &config);
    env.write_default_prompt(DEFAULT_PROMPT);
    for (repo, n) in [("org/one", 1), ("org/two", 2)] {
        write_list_fixture(
            env.gh_dir.path(),
            repo,
            &[pr_json(n, "Template", "alice", "f/x", "aaaa", false, OLD)],
        );
        let out = env.run(
            &["review", &format!("{repo}#{n}")],
            &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
        );
        assert!(out.status.success(), "{}", stderr_of(&out));
    }

    let one = std::fs::read_to_string(env.gh_dir.child("org_one#1.review-body.txt")).unwrap();
    assert_eq!(
        one,
        "prx-bot (ask @maya) says changes requested on org/one#1 (1 blocking, 2 nits)\n\n\
         ### Blocking (1)\n- `src/a.rs:12` \u{2014} fix the sql injection\n"
    );
    let two = std::fs::read_to_string(env.gh_dir.child("org_two#2.review-body.txt")).unwrap();
    assert!(
        two.starts_with("\u{2014} automated review by prx says changes requested on org/two#2"),
        "{two}"
    );
}

/// Design D6 / task 6.1: a review start appends a `review_started` history
/// entry (before the outcome) and delivers `on_review_started` at its
/// default banner tier; retiered to silent it delivers nothing.
#[test]
fn review_started_writes_history_and_notifies_per_its_tier() {
    let env = Env::new("rev-started");
    env.write_config(&["org/one"], "");
    env.write_default_prompt(DEFAULT_PROMPT);
    let log = install_fake_notifier(&env);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Started", "alice", "f/x", "aaaa", false, OLD)],
    );
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    // Newest first: the outcome, then the start marker.
    match &env.raw_history()[..] {
        [prx::history::HistoryEntry::ReviewCompleted { .. }, prx::history::HistoryEntry::ReviewStarted {
            repo,
            number,
            title,
            agent,
            ..
        }] => {
            assert_eq!((repo.as_str(), *number), ("org/one", 1));
            assert_eq!(title, "Started");
            assert_eq!(agent.as_deref(), Some("fake"));
        }
        other => panic!("unexpected history: {other:?}"),
    }
    // Delivered at the default (informational, banner) tier, before the
    // outcome's notification.
    let delivered = notifications(&log);
    let position = |needle: &str| delivered.iter().position(|l| l.contains(needle));
    let started = position("org/one#1: review started").expect("start banner");
    let blocking = position("blocking issue(s) found").expect("outcome banner");
    assert!(started < blocking, "{delivered:?}");

    // Retier to silent: the start still lands in history, no banner.
    std::fs::remove_file(&log).unwrap();
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "defaults:\n",
        "defaults:\n  notifications:\n    on_review_started: silent\n",
        1,
    );
    write_file(&env.config, &config);
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let started = env
        .raw_history()
        .iter()
        .filter(|e| matches!(e, prx::history::HistoryEntry::ReviewStarted { .. }))
        .count();
    assert_eq!(started, 2);
    let delivered = notifications(&log);
    assert!(
        !delivered.iter().any(|l| l.contains("review started")),
        "{delivered:?}"
    );
    assert!(
        delivered
            .iter()
            .any(|l| l.contains("blocking issue(s) found")),
        "{delivered:?}"
    );
}

/// Design D6 / task 6.2: `defaults.notifications.enabled: false` stops every
/// delivery while state and history update; re-enabled, the next events
/// deliver again.
#[test]
fn notifications_master_switch_stops_delivery_only() {
    let env = Env::new("rev-master-switch");
    env.write_config(&["org/one"], "  notifications:\n    enabled: false\n");
    env.write_default_prompt(DEFAULT_PROMPT);
    let log = install_fake_notifier(&env);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Quiet", "alice", "f/x", "aaaa", false, OLD)],
    );
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(notifications(&log).is_empty(), "{:?}", notifications(&log));
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, prx::state::PrState::ReviewedChangesRequested);
    assert_eq!(env.raw_history().len(), 2, "started + completed recorded");

    // Re-enable: the next review's events deliver.
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "    enabled: false\n",
        "    enabled: true\n",
        1,
    );
    write_file(&env.config, &config);
    let out = env.run(
        &["review", "org/one#1"],
        &[("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str())],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let delivered = notifications(&log);
    assert!(
        delivered.iter().any(|l| l.contains("review started")),
        "{delivered:?}"
    );
    assert!(
        delivered
            .iter()
            .any(|l| l.contains("blocking issue(s) found")),
        "{delivered:?}"
    );
}

// ------------------------------------------- per-run review options (D1–D4)

/// Add a second agent engine `other` (same fake binary; its argv starts
/// with the `OTHER-ENGINE` marker) after the `fake` agent entry. With
/// `resume`, both agents get `resume_args`.
fn add_other_engine(env: &Env, resume: bool) {
    let resume_line = if resume {
        "    resume_args: [\"--resume\", \"{{SESSION_ID}}\"]\n"
    } else {
        ""
    };
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "    args: [\"{{PROMPT}}\"]\n",
        &format!(
            "    args: [\"{{{{PROMPT}}}}\"]\n{resume_line}  other:\n    command: {}\n    args: [\"OTHER-ENGINE\", \"{{{{PROMPT}}}}\"]\n{resume_line}",
            env.agent_path.display()
        ),
        1,
    );
    write_file(&env.config, &config);
}

/// Tasks 4.2 / 4.4 / 4.5: `--engine other --no-post --prompt …` runs the
/// override engine's argv with the extra instructions, makes zero GitHub
/// writes, still updates the verdict, state, and history (agent `other`,
/// marked not posted), and `prx history` renders "(not posted)". The next
/// plain run uses the configured engine and posts again.
#[test]
fn review_options_engine_no_post_and_prompt_apply_to_one_run() {
    let env = Env::new("rev-options");
    env.write_config(&["org/one"], "");
    add_other_engine(&env, false);
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Options", "alice", "f/x", "aaaa", false, OLD)],
    );
    let prompt_out = env.agent_dir.child("prompt.txt");
    let manifest = blocking_manifest();
    let agent_env = [
        ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
        ("FAKE_AGENT_PROMPT_OUT", prompt_out.to_str().unwrap()),
    ];

    let out = env.run(
        &[
            "review",
            "https://github.com/org/one/pull/1/files",
            "--engine",
            "other",
            "--no-post",
            "--prompt",
            "EXTRA-XYZ check the migration",
        ],
        &agent_env,
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let prompt = std::fs::read_to_string(&prompt_out).unwrap();
    assert!(prompt.starts_with("OTHER-ENGINE "), "{prompt}");
    assert!(prompt.contains("EXTRA-XYZ check the migration"), "{prompt}");
    assert!(github_writes(&env).is_empty(), "calls: {:?}", env.calls());
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, prx::state::PrState::ReviewedChangesRequested);
    assert_eq!(rec.blocking_count, 1);
    match &env.history()[..] {
        [prx::history::HistoryEntry::ReviewCompleted { agent, posted, .. }] => {
            assert_eq!(agent.as_deref(), Some("other"));
            assert_eq!(*posted, Some(false));
        }
        other => panic!("expected one completed review, got {other:?}"),
    }
    let out = env.run(&["history"], &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let timeline = stdout_of(&out);
    assert!(
        timeline.contains("changes requested (not posted) — 1 blocking, 2 nits · other"),
        "{timeline}"
    );
    // Nothing about the options persisted anywhere.
    for path in [env.data_dir.child("state.json"), env.config.clone()] {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("EXTRA-XYZ"), "{} leaked it", path.display());
    }

    // The next plain run: the configured engine, posted, no extra prompt.
    let out = env.run(&["review", "org/one#1"], &agent_env);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let prompt = std::fs::read_to_string(&prompt_out).unwrap();
    assert!(prompt.starts_with("BASE-PROMPT"), "{prompt}");
    assert!(!prompt.contains("EXTRA-XYZ"), "{prompt}");
    assert!(env
        .calls()
        .iter()
        .any(|l| l == "pr review 1 -R org/one --request-changes --body-file -"));
    match &env.history()[0] {
        prx::history::HistoryEntry::ReviewCompleted { agent, posted, .. } => {
            assert_eq!(agent.as_deref(), Some("fake"));
            assert_eq!(*posted, Some(true));
        }
        other => panic!("expected a completed review, got {other:?}"),
    }
    let timeline = stdout_of(&env.run(&["history"], &[]));
    assert_eq!(timeline.matches("(not posted)").count(), 1, "{timeline}");

    // An unknown engine is rejected before anything runs.
    let out = env.run(&["review", "org/one#1", "--engine", "gpt9"], &agent_env);
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(err.contains("unknown engine 'gpt9'"), "{err}");
    assert!(err.contains("claude, codex, fake, other, pi"), "{err}");
}

/// Task 4.3 / design D3: a retained session survives a one-off run with
/// another engine — the override run starts fresh and does not store its
/// own session — and the next default run resumes the retained one.
#[test]
fn engine_override_run_neither_resumes_nor_replaces_the_retained_session() {
    let env = Env::new("rev-override-session");
    env.write_config(&["org/one"], "");
    add_other_engine(&env, true);
    env.write_default_prompt(DEFAULT_PROMPT);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(3, "Session", "carol", "f/s", "aaaa", false, OLD)],
    );
    let runs_log = env.agent_dir.child("runs.log");
    let runs_log = runs_log.to_str().unwrap();
    let manifest = manifest_json("approved", &[]);
    let run = |args: &[&str], session: &str| {
        let out = env.run(
            args,
            &[
                ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
                ("FAKE_AGENT_SESSION", session),
                ("FAKE_AGENT_RUNS_LOG", runs_log),
            ],
        );
        assert!(out.status.success(), "{}", stderr_of(&out));
    };
    let session = || env.state().prs["org/one#3"].review_session.clone();

    run(&["review", "org/one#3"], "sess-1");
    assert_eq!(session().as_deref(), Some("sess-1"));
    run(&["review", "org/one#3", "--engine", "other"], "sess-other");
    assert_eq!(session().as_deref(), Some("sess-1"), "override kept it");
    run(&["review", "org/one#3"], "sess-1");
    assert_eq!(
        std::fs::read_to_string(runs_log).unwrap(),
        "fresh\nfresh\nresume sess-1\n"
    );
    let agents: Vec<Option<String>> = env
        .history()
        .iter()
        .map(|e| match e {
            prx::history::HistoryEntry::ReviewCompleted { agent, .. } => agent.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    let want = |a: &str| Some(a.to_string());
    assert_eq!(agents, [want("fake"), want("other"), want("fake")]);
}

/// Task 5.1 / design D10: locally (dry-run, or no daemon), only the full
/// and URL forms resolve; the short forms print a hint.
#[test]
fn local_review_needs_a_full_reference_and_hints_otherwise() {
    let env = Env::new("rev-local-hint");
    env.write_config(&["org/one"], "");
    for target in ["42", "#42", "one#42"] {
        let out = env.run(&["review", target, "--dry-run"], &[]);
        assert!(!out.status.success());
        let err = stderr_of(&out);
        assert!(err.contains("needs the daemon"), "{err}");
        assert!(
            err.contains("locally (dry-run) use org/repo#N or a PR URL"),
            "{err}"
        );
    }
    // Without --dry-run and without a daemon: the same hint.
    let out = env.run(&["review", "42"], &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("use org/repo#N or a PR URL"));
    // The flags conflict with --all (clap).
    for flag in [&["--engine", "x"][..], &["--no-post"], &["--prompt", "p"]] {
        let mut args = vec!["review", "--all"];
        args.extend_from_slice(flag);
        let out = env.run(&args, &[]);
        assert!(!out.status.success());
        assert!(
            stderr_of(&out).contains("cannot be used with"),
            "{}",
            stderr_of(&out)
        );
    }
}

/// Map the fake agent's JSON output to usage fields (task 8.3).
fn add_usage_output(env: &Env) {
    let config = std::fs::read_to_string(&env.config).unwrap().replacen(
        "    args: [\"{{PROMPT}}\"]\n",
        "    args: [\"{{PROMPT}}\"]\n    model_args: [\"--model\", \"{{MODEL}}\"]\n    usage_output:\n      input_tokens: tokens_in\n      output_tokens: stats.tokens_out\n      cost_usd: stats.cost\n",
        1,
    );
    write_file(&env.config, &config);
}

/// Tasks 8.3 / 8.5: a fake agent with a `usage_output` mapping records the
/// mapped counts with the engine, the configured model, and the captured
/// session; the on-demand brief's usage is stored apart as
/// `summary_usage`; `prx history` shows the tokens.
#[test]
fn completed_run_records_review_and_summary_usage() {
    let env = Env::new("rev-usage");
    env.write_config(&["org/one"], "  agent_model: fake-model-1\n");
    add_usage_output(&env);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Usage", "alice", "f/x", "aaaa", false, OLD)],
    );
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str()),
            ("FAKE_AGENT_SESSION", "sess-u"),
            (
                "FAKE_AGENT_STDOUT",
                r#"{"tokens_in": 12345, "stats": {"tokens_out": 1100, "cost": 0.25}}"#,
            ),
            (
                "FAKE_AGENT_SUMMARY",
                r#"{"result": "Adds a query; one blocker.", "tokens_in": 900, "stats": {"tokens_out": 120}}"#,
            ),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    let history = env.history();
    let prx::history::HistoryEntry::ReviewCompleted {
        agent,
        usage,
        summary_usage,
        brief,
        ..
    } = &history[0]
    else {
        panic!("expected ReviewCompleted, got {:?}", history[0]);
    };
    assert_eq!(agent.as_deref(), Some("fake"));
    assert_eq!(brief.as_deref(), Some("Adds a query; one blocker."));
    assert_eq!(
        usage.as_ref().unwrap(),
        &prx::usage::TokenUsage {
            input_tokens: Some(12345),
            output_tokens: Some(1100),
            cost_usd: Some(0.25),
            model: Some("fake-model-1".into()),
            session_id: Some("sess-u".into()),
            ..Default::default()
        }
    );
    assert_eq!(
        summary_usage.as_ref().unwrap(),
        &prx::usage::TokenUsage {
            input_tokens: Some(900),
            output_tokens: Some(120),
            model: Some("fake-model-1".into()),
            ..Default::default()
        }
    );

    // Task 8.6: the timeline shows review + summary tokens and the cost.
    let out = env.run(&["history"], &[]);
    let stdout = stdout_of(&out);
    let line = stdout
        .lines()
        .find(|l| l.contains("org/one#1") && l.contains("changes requested"))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(
        line.contains(" · 13.2k in / 1.2k out · $0.25 · Usage"),
        "{line}"
    );
}

/// Task 8.5: a failed run records the usage its attempts reported (both
/// attempts exited non-zero after reporting usage), and an engine without
/// usage records none.
#[test]
fn failed_run_records_partial_usage() {
    let env = Env::new("rev-usage-fail");
    env.write_config(&["org/one"], "");
    add_usage_output(&env);
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Usage", "alice", "f/x", "aaaa", false, OLD)],
    );
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_EXIT", "1"),
            ("FAKE_AGENT_STDOUT", r#"{"tokens_in": 700}"#),
        ],
    );
    assert!(!out.status.success());
    let history = env.history();
    let prx::history::HistoryEntry::ReviewFailed { usage, .. } = &history[0] else {
        panic!("expected ReviewFailed, got {:?}", history[0]);
    };
    assert_eq!(
        usage.as_ref().unwrap(),
        &prx::usage::TokenUsage {
            input_tokens: Some(1400),
            ..Default::default()
        }
    );
    let out = env.run(&["history"], &[]);
    assert!(
        stdout_of(&out).contains("review failed · 1.4k in · Usage"),
        "{}",
        stdout_of(&out)
    );

    // The same failure through an agent that reports nothing: unknown.
    let env = Env::new("rev-usage-none");
    env.write_config(&["org/one"], "");
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Usage", "alice", "f/x", "aaaa", false, OLD)],
    );
    env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_EXIT", "1"),
            ("FAKE_AGENT_STDOUT", r#"{"tokens_in": 700}"#),
        ],
    );
    let raw = std::fs::read_to_string(env.data_dir.child("history.jsonl")).unwrap();
    assert!(raw.contains("review_failed"), "{raw}");
    assert!(!raw.contains("usage"), "{raw}");
}

// ------------------------------------------------------------------ briefs

/// The brief-run log and prompt file the fake agent writes for free-form
/// (on-demand brief) runs.
fn brief_env(env: &Env) -> (PathBuf, PathBuf) {
    (
        env.data_dir.child("brief-runs.log"),
        env.data_dir.child("brief-prompt.txt"),
    )
}

fn brief_runs(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// Design B2 (task 3.1): a manifest `brief` is stored with source `review`
/// and the reviewed head, recorded on the history entry, and rendered by
/// `{{BRIEF}}` in the same run's body — with no extra agent run and no
/// brief fetch from GitHub.
#[test]
fn manifest_brief_is_stored_without_an_extra_run() {
    let env = Env::new("rev-brief-manifest");
    env.write_config(
        &["org/one"],
        "  comments:\n    template: \"{{VERDICT_LABEL}}\\n\\nBrief: {{BRIEF}}\\n\"\n",
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Brief", "alice", "f/x", "aaaa", false, OLD)],
    );
    let (log, prompt) = brief_env(&env);
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str()),
            (
                "FAKE_AGENT_BRIEF",
                "This adds rate limiting to the login endpoint.",
            ),
            ("FAKE_AGENT_SUMMARY", "should never be used"),
            ("FAKE_AGENT_SUMMARY_RUNS_LOG", log.to_str().unwrap()),
            ("FAKE_AGENT_SUMMARY_PROMPT_OUT", prompt.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(brief_runs(&log), 0, "no extra brief run");
    assert!(!prompt.exists());
    assert!(
        !env.calls()
            .iter()
            .any(|c| c.contains("title,body,author,files")),
        "{:?}",
        env.calls()
    );

    let rec = &env.state().prs["org/one#1"];
    let brief = rec.brief.clone().expect("brief stored");
    assert_eq!(brief.text, "This adds rate limiting to the login endpoint.");
    assert_eq!(brief.source, prx::state::BriefSource::Review);
    assert_eq!(brief.head_sha, "aaaa");
    assert_eq!(brief.engine.as_deref(), Some("fake"));
    assert_eq!(Some(brief.created_at), rec.last_review_at);

    let history = env.history();
    let prx::history::HistoryEntry::ReviewCompleted {
        brief: entry_brief,
        summary_usage,
        ..
    } = &history[0]
    else {
        panic!("expected ReviewCompleted, got {:?}", history[0]);
    };
    assert_eq!(
        entry_brief.as_deref(),
        Some("This adds rate limiting to the login endpoint.")
    );
    assert_eq!(summary_usage, &None);

    let body = std::fs::read_to_string(env.gh_dir.child("org_one#1.review-body.txt")).unwrap();
    assert_eq!(
        body,
        "changes requested\n\nBrief: This adds rate limiting to the login endpoint.\n"
    );
}

/// Design B2 (tasks 3.1/3.2): without a manifest brief, exactly one
/// on-demand run follows the review. It reads the PR read-only (`gh pr
/// view` + `gh pr diff`, from the fixture), carries the diff as untrusted
/// data, and is stored with source `on_demand`.
#[test]
fn missing_manifest_brief_runs_one_on_demand_brief() {
    let env = Env::new("rev-brief-ondemand");
    env.write_config(&["org/one"], "");
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Brief", "alice", "f/x", "aaaa", false, OLD)],
    );
    write_file(
        &env.gh_dir.child("org_one#1.brief.json"),
        r#"{"title":"Rate limit logins","body":"Stops brute force.","author":{"login":"alice"},"files":[{"path":"src/login.rs"}]}"#,
    );
    write_file(
        &env.gh_dir.child("org_one#1.diff"),
        "diff --git a/src/login.rs b/src/login.rs\n+    limiter.check(ip)?;\n",
    );
    let (log, prompt) = brief_env(&env);
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str()),
            ("FAKE_AGENT_SUMMARY", "Adds a login rate limiter."),
            ("FAKE_AGENT_SUMMARY_RUNS_LOG", log.to_str().unwrap()),
            ("FAKE_AGENT_SUMMARY_PROMPT_OUT", prompt.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(brief_runs(&log), 1, "one on-demand brief run");
    let prompt = std::fs::read_to_string(&prompt).unwrap();
    for needle in [
        "Write a brief of a pull request",
        "untrusted data",
        "Title: Rate limit logins",
        "- src/login.rs",
        "Stops brute force.",
        "+    limiter.check(ip)?;",
    ] {
        assert!(prompt.contains(needle), "missing {needle:?}:\n{prompt}");
    }
    let calls = env.calls();
    assert!(calls
        .iter()
        .any(|c| c.contains("pr view 1 -R org/one --json title,body,author,files")));
    assert!(calls.iter().any(|c| c.starts_with("pr diff 1 -R org/one")));

    let brief = env.state().prs["org/one#1"].brief.clone().expect("brief");
    assert_eq!(brief.text, "Adds a login rate limiter.");
    assert_eq!(brief.source, prx::state::BriefSource::OnDemand);
    assert_eq!(brief.head_sha, "aaaa");
}

/// Design B2 (task 3.1): `brief.enabled: false` ignores a manifest brief
/// and runs no brief generation.
#[test]
fn disabled_briefs_store_and_run_nothing() {
    let env = Env::new("rev-brief-off");
    env.write_config(
        &["org/one"],
        "  brief:\n    enabled: false\n  comments:\n    template: \"[{{BRIEF}}] {{VERDICT_LABEL}}\"\n",
    );
    write_list_fixture(
        env.gh_dir.path(),
        "org/one",
        &[pr_json(1, "Brief", "alice", "f/x", "aaaa", false, OLD)],
    );
    let (log, _) = brief_env(&env);
    let out = env.run(
        &["review", "org/one#1"],
        &[
            ("FAKE_AGENT_MANIFEST_JSON", blocking_manifest().as_str()),
            ("FAKE_AGENT_BRIEF", "ignored"),
            ("FAKE_AGENT_SUMMARY", "ignored too"),
            ("FAKE_AGENT_SUMMARY_RUNS_LOG", log.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(brief_runs(&log), 0);
    assert_eq!(env.state().prs["org/one#1"].brief, None);
    let history = env.history();
    assert!(matches!(
        &history[0],
        prx::history::HistoryEntry::ReviewCompleted { brief: None, .. }
    ));
    let body = std::fs::read_to_string(env.gh_dir.child("org_one#1.review-body.txt")).unwrap();
    assert_eq!(body, "[] changes requested");
}
