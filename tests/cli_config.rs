//! `prx config show` merge semantics, `prx init` artifacts, and the stubbed
//! commands' stable skeleton behavior.

mod common;

use std::fs;

use common::*;

#[test]
fn init_writes_config_idempotently_and_no_prompt_file() {
    let cfg_dir = TempDir::new("init-cfg");
    let data_dir = TempDir::new("init-data");
    let config = cfg_dir.child("config.yml");

    let out = run_cli(&config, data_dir.path(), &["init"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    let config_on_disk = fs::read_to_string(&config).unwrap();
    assert!(config_on_disk.contains("before_first_review_minutes: 60"));
    assert!(config_on_disk.contains("poll_interval_minutes: 15"));
    assert!(config_on_disk.contains("labels: [\"wip\", \"do-not-review\"]"));
    assert!(config_on_disk.contains("on_review_complete_blocking: urgent"));
    assert!(config_on_disk.contains("repos: []"));
    assert!(config_on_disk.contains("# menu:"));
    assert!(config_on_disk.contains("#   sort: created_desc"));
    assert!(config_on_disk.contains("#   group_by: lifecycle"));

    // Design F5: the prompt is config text, so init writes no prompt file;
    // the starter config uses the built-in default.
    assert!(!data_dir.child("prompts/default.md").exists());
    assert!(config_on_disk.contains("review_prompt: default"));
    prx::config::Config::parse(&config_on_disk, &config)
        .unwrap()
        .validate()
        .unwrap();
    // Design C1: the default prompt is guidance only (diff via gh pr diff);
    // the A1-A3 rules — read-only agent, manifest as the only output, never
    // posts to GitHub — live in the always-appended output contract.
    let prompt_text = prx::prompt::DEFAULT_PROMPT;
    assert!(prompt_text.contains("{{PR_NUMBER}}"));
    assert!(prompt_text.contains("{{REPO}}"));
    assert!(prompt_text.contains("gh pr diff"));
    assert!(!prompt_text.contains("{{MANIFEST_PATH}}"));
    assert!(!prompt_text.contains("manifest_version"));
    let contract = prx::prompt::OUTPUT_CONTRACT;
    assert!(contract.contains("{{MANIFEST_PATH}}"));
    assert!(contract.contains("manifest_version"));
    assert!(contract.contains("READ-ONLY"));
    assert!(contract.contains("NEVER run `gh pr review`"));
    assert!(contract.contains("UNTRUSTED DATA"));

    // Re-running init keeps existing files.
    fs::write(&config, "defaults:\n  agent: pi\nrepos: []\n").unwrap();
    let out = run_cli(&config, data_dir.path(), &["init"], None, &[]);
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("config already exists"));
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        "defaults:\n  agent: pi\nrepos: []\n"
    );
}

#[test]
fn config_show_merges_defaults_with_repo_overrides() {
    let cfg_dir = TempDir::new("show-cfg");
    let data_dir = TempDir::new("show-data");
    let config = cfg_dir.child("config.yml");

    // org/base inherits everything; org/full overrides individual keys only.
    write_file(
        &config,
        r#"defaults:
  agent: pi
  poll_interval_minutes: 15
  cooldown:
    before_first_review_minutes: 60
    between_re_reviews_minutes: 30
  max_concurrent_reviews: 1
  diff_max_files: 50
  diff_max_lines: 4000
  skip:
    drafts: true
    authors: []
    labels: ["wip", "do-not-review"]
    branches: []
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
"#,
    );

    let out = run_cli(&config, data_dir.path(), &["config"], None, &[]); // bare = show
    assert!(out.status.success(), "{}", stderr_of(&out));
    let view: serde_yaml::Value = serde_yaml::from_str(&stdout_of(&out)).unwrap();

    // org/base inherits every default.
    let base = &view["repos"][0];
    assert_eq!(base["name"].as_str().unwrap(), "org/base");
    assert_eq!(base["engine"].as_str().unwrap(), "pi");
    assert_eq!(base["resolved_engine"]["command"].as_str().unwrap(), "pi"); // built-in table
    assert_eq!(base["poll_interval_minutes"].as_i64().unwrap(), 15);
    assert_eq!(
        base["cooldown"]["before_first_review_minutes"]
            .as_i64()
            .unwrap(),
        60
    );
    assert_eq!(
        base["cooldown"]["between_re_reviews_minutes"]
            .as_i64()
            .unwrap(),
        30
    );
    assert_eq!(base["diff_max_files"].as_i64().unwrap(), 50);
    assert!(base["skip"]["drafts"].as_bool().unwrap());
    assert_eq!(base["skip"]["labels"][0].as_str().unwrap(), "wip");

    // org/full overrides only what it specifies; everything else inherits.
    let full = &view["repos"][1];
    assert_eq!(full["name"].as_str().unwrap(), "org/full");
    assert_eq!(full["engine"].as_str().unwrap(), "claude");
    assert_eq!(
        full["resolved_engine"]["command"].as_str().unwrap(),
        "claude"
    );
    assert_eq!(
        full["resolved_engine"]["args"][2].as_str().unwrap(),
        "--dangerously-skip-permissions"
    );
    assert_eq!(full["poll_interval_minutes"].as_i64().unwrap(), 45);
    // Cooldown: only the specified key is overridden.
    assert_eq!(
        full["cooldown"]["before_first_review_minutes"]
            .as_i64()
            .unwrap(),
        60
    );
    assert_eq!(
        full["cooldown"]["between_re_reviews_minutes"]
            .as_i64()
            .unwrap(),
        5
    );
    // Skip: authors/drafts overridden, labels inherited.
    assert_eq!(
        full["skip"]["authors"][0].as_str().unwrap(),
        "dependabot[bot]"
    );
    assert!(!full["skip"]["drafts"].as_bool().unwrap());
    assert_eq!(full["skip"]["labels"][1].as_str().unwrap(), "do-not-review");

    // The effective view also reports locations and the resolved engines table.
    assert_eq!(
        view["config_file"].as_str().unwrap(),
        config.display().to_string()
    );
    assert_eq!(
        view["data_dir"].as_str().unwrap(),
        data_dir.path().display().to_string()
    );
    assert!(view["engines"]["pi"]["command"].as_str().is_some());
    assert_eq!(view["engines"]["pi"]["type"].as_str(), Some("agent"));
    assert!(view["defaults"]["name"]
        .as_str()
        .unwrap()
        .starts_with("(defaults)"));
}

/// Design D1 + task 1.2: a legacy (v0.1/v1.2) config shows in the engines
/// form, a `config set` rewrite saves that form, and the new commentary
/// keys are settable and validated through the CLI.
#[test]
fn legacy_config_shows_engines_and_new_keys_round_trip_through_set() {
    let cfg_dir = TempDir::new("legacy-cfg");
    let data_dir = TempDir::new("legacy-data");
    let config = cfg_dir.child("config.yml");
    write_file(
        &config,
        "defaults:\n  agent: fake\nagents:\n  fake:\n    command: fake-agent\n    args: [\"-\"]\norgs:\n  - org: acme\n    agent: codex\nrepos:\n  - name: acme/api\n  - name: org/one\n    agent: claude\n",
    );
    let show = |config: &std::path::Path| {
        let out = run_cli(config, data_dir.path(), &["config", "show"], None, &[]);
        assert!(out.status.success(), "{}", stderr_of(&out));
        serde_yaml::from_str::<serde_yaml::Value>(&stdout_of(&out)).unwrap()
    };
    let view = show(&config);
    assert!(view.get("agents").is_none());
    assert_eq!(view["engines"]["fake"]["type"].as_str(), Some("agent"));
    assert_eq!(
        view["engines"]["fake"]["command"].as_str(),
        Some("fake-agent")
    );
    assert_eq!(view["defaults"]["engine"].as_str(), Some("fake"));
    assert_eq!(view["repos"][0]["engine"].as_str(), Some("codex"));
    assert_eq!(view["repos"][1]["engine"].as_str(), Some("claude"));
    assert_eq!(view["defaults"]["comments"]["inline"].as_bool(), Some(true));

    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "defaults.comments.inline", "false"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let view = show(&config);
    assert_eq!(
        view["defaults"]["comments"]["inline"].as_bool(),
        Some(false)
    );
    assert_eq!(
        view["repos"][0]["comments"]["inline"].as_bool(),
        Some(false)
    );
    let saved = fs::read_to_string(&config).unwrap();
    assert!(
        saved.contains("engines:") && !saved.contains("agents:"),
        "{saved}"
    );

    let before = saved;
    let out = run_cli(
        &config,
        data_dir.path(),
        &[
            "config",
            "set",
            "defaults.comments.template",
            "{{REVIEW_SCORE}}",
        ],
        None,
        &[],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    assert!(
        err.contains("defaults.comments.template: unknown placeholder {{REVIEW_SCORE}}"),
        "{err}"
    );
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}

/// Design D7 (task 1.2): `menu.*` defaults, sets, and shows through the
/// dotted-path machinery; an unknown value is rejected with the choices.
#[test]
fn menu_settings_set_and_show() {
    let cfg_dir = TempDir::new("menu-cfg");
    let data_dir = TempDir::new("menu-data");
    let config = cfg_dir.child("config.yml");
    write_file(&config, "defaults:\n  engine: pi\nrepos: []\n");
    let show = || {
        let out = run_cli(&config, data_dir.path(), &["config", "show"], None, &[]);
        assert!(out.status.success(), "{}", stderr_of(&out));
        serde_yaml::from_str::<serde_yaml::Value>(&stdout_of(&out)).unwrap()
    };
    let view = show();
    assert_eq!(view["menu"]["sort"].as_str(), Some("created_desc"));
    assert_eq!(view["menu"]["group_by"].as_str(), Some("lifecycle"));

    for (key, value) in [("menu.sort", "last_review_desc"), ("menu.group_by", "none")] {
        let out = run_cli(
            &config,
            data_dir.path(),
            &["config", "set", key, value],
            None,
            &[],
        );
        assert!(out.status.success(), "{}", stderr_of(&out));
    }
    let view = show();
    assert_eq!(view["menu"]["sort"].as_str(), Some("last_review_desc"));
    assert_eq!(view["menu"]["group_by"].as_str(), Some("none"));

    let before = fs::read_to_string(&config).unwrap();
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "menu.sort", "oldest"],
        None,
        &[],
    );
    assert!(!out.status.success());
    let err = stderr_of(&out);
    for allowed in ["created_desc", "created_asc", "last_review_desc", "key"] {
        assert!(err.contains(allowed), "{allowed} missing from: {err}");
    }
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn config_show_reports_validation_errors() {
    let cfg_dir = TempDir::new("show-bad");
    let data_dir = TempDir::new("show-bad-data");
    let config = cfg_dir.child("config.yml");

    write_file(
        &config,
        "defaults:\n  agent: pi\nrepos:\n  - name: no-slash\n",
    );
    let out = run_cli(&config, data_dir.path(), &["config", "show"], None, &[]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("invalid repo name"));
}

#[test]
fn history_insights_and_config_editing_are_implemented() {
    let cfg_dir = TempDir::new("cfg-cmds-cfg");
    let data_dir = TempDir::new("cfg-cmds-data");
    let config = cfg_dir.child("config.yml");
    write_file(&config, "defaults:\n  agent: pi\nrepos: []\n");

    // history/insights fall back in-process without a daemon and succeed
    // (empty data → friendly output, not a stub message).
    let out = run_cli(&config, data_dir.path(), &["history"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out).trim(), "no history yet");
    assert!(stderr_of(&out).contains("daemon not running"));

    let out = run_cli(&config, data_dir.path(), &["insights"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("prx insights"),
        "{}",
        stdout_of(&out)
    );
    assert!(stdout_of(&out).contains("no reviews recorded yet"));

    // config set edits the file through the dotted key path.
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "defaults.agent", "codex"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("set defaults.agent = codex"));
    let saved = std::fs::read_to_string(&config).unwrap();
    assert!(saved.contains("engine: codex"), "{saved}");

    // config edit opens $EDITOR (the `true` binary succeeds on any file).
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "edit"],
        None,
        &[("EDITOR", "true")],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    // An invalid value is rejected and leaves the file untouched.
    let before = std::fs::read_to_string(&config).unwrap();
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "defaults.poll_interval_minutes", "0"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("file not modified"),
        "{}",
        stderr_of(&out)
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

#[test]
fn repos_add_survives_unknown_top_level_keys() {
    // Users may have extra keys in their config; a rewrite must not drop them.
    let cfg_dir = TempDir::new("show-extra");
    let data_dir = TempDir::new("show-extra-data");
    let config = cfg_dir.child("config.yml");
    write_file(
        &config,
        "defaults:\n  agent: pi\nfuture_feature:\n  enabled: true\nrepos: []\n",
    );

    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "add", "org/one"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    let on_disk = fs::read_to_string(&config).unwrap();
    assert!(
        on_disk.contains("future_feature"),
        "unknown keys dropped: {on_disk}"
    );
    assert!(on_disk.contains("org/one"));
}

#[test]
fn config_set_updates_agent_map_in_place_without_duplicates() {
    // Regression: `config set agents.pi.…` used to (a) treat `args[0]` as a
    // literal key named "args[0]" (silently dropped by the schema, leaving
    // empty agent args) and (b) append a SECOND top-level `agents:` block,
    // which made `prx review` fail with "agent args contain neither '-'
    // nor {{PROMPT}}" and reloads fail with "duplicate field `agents`".
    // The set must update the ONE parsed map in place.
    let cfg_dir = TempDir::new("set-agents");
    let data_dir = TempDir::new("set-agents-data");
    let config = cfg_dir.child("config.yml");

    let out = run_cli(&config, data_dir.path(), &["init"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "add", "octocat/hello-world"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "agents.pi.command", "fake-agent"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "agents.pi.args[0]", "{{PROMPT}}"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "agents.pi.args[1]", "--", "-p"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));

    // Exactly ONE top-level `engines:` key in the file (the legacy
    // `agents.…` paths alias to it, design D1).
    let on_disk = fs::read_to_string(&config).unwrap();
    let agents_lines = on_disk
        .lines()
        .filter(|line| line.starts_with("engines:"))
        .count();
    assert_eq!(agents_lines, 1, "duplicate engines map: {on_disk}");

    // The parsed config has a single engines map with both args entries.
    let parsed: prx::config::Config = serde_yaml::from_str(&on_disk).unwrap();
    assert_eq!(parsed.engines.len(), 1, "{on_disk}");
    let pi = parsed.engines["pi"].as_agent().unwrap();
    assert_eq!(pi.command, "fake-agent");
    assert_eq!(pi.args, vec!["{{PROMPT}}".to_string(), "-p".to_string()]);

    // `prx config` shows the merged map, also with exactly one engines block
    // and the update applied.
    let out = run_cli(&config, data_dir.path(), &["config"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let shown = stdout_of(&out);
    assert_eq!(
        shown.lines().filter(|l| l.starts_with("engines:")).count(),
        1,
        "{shown}"
    );
    assert!(shown.contains("fake-agent"), "{shown}");
    assert!(shown.contains("{{PROMPT}}"), "{shown}");

    // Re-setting an existing nested key updates it in place (still one map).
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "agents.pi.command", "fake-agent-2"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let on_disk = fs::read_to_string(&config).unwrap();
    assert_eq!(
        on_disk
            .lines()
            .filter(|line| line.starts_with("engines:"))
            .count(),
        1,
        "{on_disk}"
    );
    assert!(on_disk.contains("fake-agent-2"), "{on_disk}");

    // A sparse index (no args[0]/args[1] yet) is rejected cleanly and the
    // file is left untouched.
    let before = on_disk.clone();
    let out = run_cli(
        &config,
        data_dir.path(),
        &["config", "set", "agents.pi.args[3]", "x"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("out of range"),
        "{}",
        stderr_of(&out)
    );
    assert_eq!(fs::read_to_string(&config).unwrap(), before);
}
