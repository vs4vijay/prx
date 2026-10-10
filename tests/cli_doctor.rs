//! `prx doctor` success and failure paths, all with the fake gh fixture.

mod common;

use common::*;
use std::fs;

fn fake_gh_dir() -> TempDir {
    let dir = TempDir::new("doctor-gh");
    install_fake_gh(dir.path());
    dir
}

/// Config that overrides the built-in `pi` agent to use `sh` (always on
/// PATH) so the agent check can pass in tests.
fn config_with_sh_agent(repos: &[&str]) -> String {
    let mut out = String::from(
        "defaults:\n  agent: pi\n  review_prompt: default\n  cooldown:\n    before_first_review_minutes: 60\n    between_re_reviews_minutes: 30\nagents:\n  pi:\n    command: sh\n    args: [\"-c\", \"true\"]\n",
    );
    out.push_str("repos:\n");
    for repo in repos {
        out.push_str(&format!("  - name: {repo}\n"));
    }
    out
}

#[test]
fn doctor_passes_when_everything_is_in_place() {
    let cfg_dir = TempDir::new("doctor-ok-cfg");
    let data_dir = TempDir::new("doctor-ok-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    // init creates the config + data dirs.
    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    write_file(&config, &config_with_sh_agent(&["org/one"]));

    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    assert!(out.status.success(), "doctor failed:\n{}", stdout_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("all checks passed"));
    assert!(stdout.contains("authenticated as testuser"));
    assert!(stdout.contains("sh")); // agent resolved to the override command
    assert!(!stdout.contains("FAIL"));
}

#[test]
fn doctor_fails_without_gh_on_path() {
    let cfg_dir = TempDir::new("doctor-nogh-cfg");
    let data_dir = TempDir::new("doctor-nogh-data");
    let config = cfg_dir.child("config.yml");
    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    write_file(&config, &config_with_sh_agent(&[]));

    // Fully stubbed PATH: no gh anywhere on it, regardless of what the host
    // has installed.
    let out = run_cli_isolated(&config, data_dir.path(), &["doctor"], None, &[]);
    assert!(!out.status.success());
    let stdout = stdout_of(&out);
    assert!(stdout.contains("gh not found on PATH"));
    assert!(stdout.contains("skipped (gh not found)"));
}

#[test]
fn doctor_fails_on_gh_auth_failure() {
    let cfg_dir = TempDir::new("doctor-auth-cfg");
    let data_dir = TempDir::new("doctor-auth-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();
    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    write_file(&config, &config_with_sh_agent(&[]));

    let out = run_cli(
        &config,
        data_dir.path(),
        &["doctor"],
        Some(gh.path()),
        &[("FAKE_GH_AUTH_FAIL", "1")],
    );
    assert!(!out.status.success());
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[FAIL] gh auth"));
    assert!(stdout.contains("gh auth login"));
}

#[test]
fn doctor_fails_on_unparsable_config() {
    let cfg_dir = TempDir::new("doctor-badcfg");
    let data_dir = TempDir::new("doctor-badcfg-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    write_file(&config, "defaults: [broken\n  yaml: {{{{");
    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    assert!(!out.status.success());
    assert!(stdout_of(&out).contains("[FAIL] config"));
}

#[test]
fn doctor_fails_on_invalid_config_values() {
    let cfg_dir = TempDir::new("doctor-badval");
    let data_dir = TempDir::new("doctor-badval-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    // Unknown engine reference (legacy `agent` key, aliased).
    write_file(&config, "defaults:\n  agent: gemini\nrepos: []\n");
    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    assert!(!out.status.success());
    let stdout = stdout_of(&out);
    assert!(stdout.contains("unknown engine 'gemini'"), "{stdout}");
}

/// Design F5: a legacy file-path `review_prompt` fails the config check
/// with the "paste the contents" instruction; the file is never read.
#[test]
fn doctor_rejects_a_legacy_prompt_path() {
    let cfg_dir = TempDir::new("doctor-prompt");
    let data_dir = TempDir::new("doctor-prompt-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    let legacy = cfg_dir.child("prompts/mine.md");
    write_file(
        &config,
        &config_with_sh_agent(&[]).replace(
            "review_prompt: default",
            &format!("review_prompt: {}", legacy.display()),
        ),
    );
    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    assert!(!out.status.success());
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(&format!(
            "review_prompt is now the prompt text; paste the contents of {} instead",
            legacy.display()
        )),
        "{stdout}"
    );
}

/// Design C1: a custom prompt that still carries its own manifest schema
/// gets an informational hint (the contract is appended automatically);
/// the check does not fail and doctor exits zero.
#[test]
fn doctor_hints_at_a_custom_prompt_with_its_own_manifest_schema() {
    let cfg_dir = TempDir::new("doctor-schema-hint");
    let data_dir = TempDir::new("doctor-schema-hint-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    let mut yaml = config_with_sh_agent(&["org/one"]);
    yaml.push_str(
        "    review_prompt: |\n      Review {{REPO}}.\n      Write {\"manifest_version\": 1} to {{MANIFEST_PATH}}.\n",
    );
    yaml.push_str("  - name: org/two\n    review_prompt: Focus on SQL injection in {{REPO}}.\n");
    write_file(&config, &yaml);

    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "doctor failed:\n{stdout}");
    let hints: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("has its own manifest schema"))
        .collect();
    assert_eq!(hints.len(), 1, "{stdout}");
    assert!(hints[0].contains("[ok  ] prompts"), "{stdout}");
    assert!(hints[0].contains("org/one review_prompt"), "{stdout}");
    assert!(
        hints[0].contains("output instructions are now added automatically and can be removed"),
        "{stdout}"
    );
    assert!(stdout.contains("all checks passed"), "{stdout}");
}

/// Design B5: the deprecated `executive_summary` section still loads, and
/// doctor mentions the rename to `brief` without failing.
#[test]
fn doctor_mentions_the_executive_summary_rename() {
    let cfg_dir = TempDir::new("doctor-brief-rename");
    let data_dir = TempDir::new("doctor-brief-rename-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    let mut yaml = config_with_sh_agent(&["org/one"]);
    yaml.push_str("    executive_summary:\n      enabled: false\n");
    write_file(&config, &yaml);

    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "doctor failed:\n{stdout}");
    let hints: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("renamed to `brief`"))
        .collect();
    assert_eq!(hints.len(), 1, "{stdout}");
    assert!(hints[0].contains("[ok  ] config"), "{stdout}");
    assert!(
        hints[0].contains("repos[org/one].executive_summary is deprecated"),
        "{stdout}"
    );
}

/// Design C1: the previous shipped default saved verbatim is reported as
/// the built-in default, with no schema hint.
#[test]
fn doctor_treats_the_legacy_default_prompt_text_as_default() {
    let cfg_dir = TempDir::new("doctor-legacy-default");
    let data_dir = TempDir::new("doctor-legacy-default-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    let indented: String = prx::prompt::LEGACY_DEFAULT_PROMPT_V1
        .lines()
        .map(|l| format!("      {l}\n"))
        .collect();
    let mut yaml = config_with_sh_agent(&["org/one"]);
    yaml.push_str(&format!("    review_prompt: |\n{indented}"));
    write_file(&config, &yaml);

    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "doctor failed:\n{stdout}");
    assert!(
        stdout.contains("org/one -> built-in default (saved copy of the previous default)"),
        "{stdout}"
    );
    assert!(!stdout.contains("has its own manifest schema"), "{stdout}");
}

#[test]
fn doctor_fails_on_missing_agent_binary() {
    let cfg_dir = TempDir::new("doctor-agent");
    let data_dir = TempDir::new("doctor-agent-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();

    // Starter config: agent pi resolves to built-in command "pi". The isolated
    // PATH keeps this deterministic even when the host has a real `pi`
    // (or other agent) installed.
    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    let out = run_cli_isolated(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    assert!(!out.status.success());
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("'pi' not found on PATH") || stdout.contains("pi' command 'pi' not found"),
        "{stdout}"
    );
}

#[test]
fn doctor_fails_when_data_dir_is_not_writable() {
    let cfg_dir = TempDir::new("doctor-data");
    let gh = fake_gh_dir();
    let config = cfg_dir.child("config.yml");
    run_cli(&config, cfg_dir.path(), &["init"], None, &[]);
    write_file(&config, &config_with_sh_agent(&[]));

    // Point the data dir at an existing regular file: creation must fail.
    let not_a_dir = cfg_dir.child("blocker");
    write_file(&not_a_dir, "not a directory");

    let out = run_cli(&config, &not_a_dir, &["doctor"], Some(gh.path()), &[]);
    assert!(!out.status.success());
    assert!(stdout_of(&out).contains("[FAIL] data dir"));
    let _ = fs::remove_file(&not_a_dir);
}

/// Task 1.3: doctor reports each repo's schedule with its next fire, every
/// referenced engine (an `llm_api` engine whose key variable is unset is a
/// warning), and browser resolvability (an unlaunchable browser warns with
/// the system fallback). Warnings never fail the run.
#[test]
fn doctor_reports_schedule_engine_and_browser_lines() {
    let cfg_dir = TempDir::new("doctor-ext-cfg");
    let data_dir = TempDir::new("doctor-ext-data");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();
    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    let mut yaml = config_with_sh_agent(&["org/one", "org/two"]);
    yaml.push_str(
        "    schedule: \"0 9 * * 1-5\"\n    engine: cheap\n    browser: [\"prx-no-such-browser\", \"{{URL}}\"]\n\
         engines:\n  cheap:\n    type: llm_api\n    provider: anthropic\n    model: claude-haiku\n    \
         api_key_env: PRX_DOCTOR_UNSET_KEY\n",
    );
    write_file(&config, &yaml);

    let out = run_cli(&config, data_dir.path(), &["doctor"], Some(gh.path()), &[]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "doctor failed:\n{stdout}");
    assert!(
        stdout.contains("schedule") && stdout.contains("org/two: '0 9 * * 1-5' next fire"),
        "{stdout}"
    );
    assert!(!stdout.contains("org/one: '"), "{stdout}");
    assert!(
        stdout.contains("[WARN] engine (org/two: engine = cheap)")
            && stdout.contains("$PRX_DOCTOR_UNSET_KEY is not set"),
        "{stdout}"
    );
    assert!(stdout.contains("engine (defaults.engine = pi)"), "{stdout}");
    assert!(
        stdout.contains("[WARN] browser")
            && stdout.contains("org/two: command 'prx-no-such-browser' not found on PATH")
            && stdout.contains("system browser instead"),
        "{stdout}"
    );

    // With the key variable set, the engine line passes.
    let out = run_cli(
        &config,
        data_dir.path(),
        &["doctor"],
        Some(gh.path()),
        &[("PRX_DOCTOR_UNSET_KEY", "x")],
    );
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(
            "cheap -> llm_api anthropic model claude-haiku (key from $PRX_DOCTOR_UNSET_KEY)"
        ),
        "{stdout}"
    );
}

/// Design A4: with the PRX.app login item active, a legacy daemon agent left
/// beside it is reported as a leftover warning (never a failure).
#[cfg(target_os = "macos")]
#[test]
fn doctor_warns_about_a_leftover_legacy_agent_beside_the_app_item() {
    let cfg_dir = TempDir::new("doctor-leftover-cfg");
    let data_dir = TempDir::new("doctor-leftover-data");
    let home = TempDir::new("doctor-leftover-home");
    let config = cfg_dir.child("config.yml");
    let gh = fake_gh_dir();
    run_cli(&config, data_dir.path(), &["init"], None, &[]);
    write_file(&config, &config_with_sh_agent(&[]));

    let bundle = home.path().join("Applications/PRX.app");
    write_file(&bundle.join("Contents/MacOS/PRX"), "");
    prx::autostart::install_app_with(&bundle, home.path()).unwrap();
    // An older install's daemon agent, written after the migration.
    let legacy = home
        .path()
        .join("Library/LaunchAgents/com.prx.daemon.plist");
    write_file(&legacy, "<plist/>");

    let out = run_cli(
        &config,
        data_dir.path(),
        &["doctor"],
        Some(gh.path()),
        &[("HOME", home.path().to_str().unwrap())],
    );
    let stdout = stdout_of(&out);
    assert!(
        out.status.success(),
        "a leftover must not fail doctor:\n{stdout}"
    );
    assert!(stdout.contains("PRX.app login item"), "{stdout}");
    assert!(stdout.contains("[WARN] autostart leftover"), "{stdout}");
    assert!(stdout.contains("legacy agent com.prx.daemon"), "{stdout}");
    assert!(stdout.contains("all checks passed"), "{stdout}");
}
