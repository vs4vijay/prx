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

// ------------------------------------------------- hermetic doctor runs
//
// The tests below control everything doctor looks at: a PATH made only of
// the fake-gh directory (so no host `gh`, agent, or browser opener leaks
// in) and a redirected HOME (so the autostart lines never read the real
// `~/Library/LaunchAgents` or `~/.config/systemd`).

struct Sandbox {
    cfg_dir: TempDir,
    data_dir: TempDir,
    home: TempDir,
    gh: TempDir,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        Sandbox {
            cfg_dir: TempDir::new(&format!("doctor-{tag}-cfg")),
            data_dir: TempDir::new(&format!("doctor-{tag}-data")),
            home: TempDir::new(&format!("doctor-{tag}-home")),
            gh: fake_gh_dir(),
        }
    }

    fn config(&self) -> std::path::PathBuf {
        self.cfg_dir.child("config.yml")
    }

    /// A do-nothing executable named `name` in the fake-gh directory.
    fn tool(&self, name: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = self.gh.child(name);
        write_file(&path, "#!/bin/sh\nexit 0\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// Both platform openers, so the browser line is green on any host.
    fn with_system_opener(&self) {
        self.tool("open");
        self.tool("xdg-open");
    }

    fn doctor(&self, with_gh: bool, extra: &[(&str, &str)]) -> std::process::Output {
        let home = self.home.path().to_str().expect("temp home is utf-8");
        let mut env = vec![("HOME", home), ("PRX_APP_SEARCH_DIRS", "")];
        env.extend_from_slice(extra);
        run_cli_isolated(
            &self.config(),
            self.data_dir.path(),
            &["doctor"],
            with_gh.then(|| self.gh.path()),
            &env,
        )
    }
}

/// The report line of the check labelled `name`, as `(marker, detail)`.
#[track_caller]
fn check_line<'a>(stdout: &'a str, name: &str) -> (&'a str, &'a str) {
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.get(9..).is_some_and(|rest| rest.starts_with(name)))
        .collect();
    assert_eq!(lines.len(), 1, "one `{name}` line expected in:\n{stdout}");
    let line = lines[0];
    (&line[3..7], line[9 + name.len()..].trim_start())
}

/// A config whose engine is an absolute path, so the engine line does not
/// depend on PATH.
const SH_ENGINE_CONFIG: &str =
    "defaults:\n  engine: local\nengines:\n  local:\n    type: agent\n    \
                                command: /bin/sh\n    args: [\"-c\", \"true\", \"{{PROMPT}}\"]\n";

/// A machine with nothing installed: the starter config and an empty PATH.
/// Exactly the checks that need a tool fail (gh, gh auth, the engine), the
/// missing browser opener is a warning, and everything that only needs the
/// filesystem passes.
#[test]
fn doctor_with_an_empty_path_fails_only_the_checks_that_need_a_tool() {
    let sb = Sandbox::new("emptypath");
    run_cli(&sb.config(), sb.data_dir.path(), &["init"], None, &[]);

    let out = sb.doctor(false, &[]);
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.starts_with("prx doctor\n"), "{stdout}");

    let (marker, detail) = check_line(&stdout, "config");
    assert_eq!(marker, "ok  ");
    assert_eq!(
        detail,
        format!("{} parsed (0 repos)", sb.config().display())
    );
    assert_eq!(
        check_line(&stdout, "gh binary"),
        ("FAIL", "gh not found on PATH")
    );
    assert_eq!(
        check_line(&stdout, "gh auth"),
        ("FAIL", "skipped (gh not found)")
    );
    assert_eq!(
        check_line(&stdout, "engine (defaults.engine = pi)"),
        ("FAIL", "agent 'pi' command 'pi' not found on PATH")
    );
    let (marker, detail) = check_line(&stdout, "browser");
    assert_eq!(marker, "WARN");
    assert!(
        detail.starts_with("(defaults): `")
            && detail.ends_with("` not found on PATH; prx cannot open PRs in a browser"),
        "{detail}"
    );
    assert_eq!(
        check_line(&stdout, "prompts"),
        ("ok  ", "defaults -> built-in default")
    );
    let (marker, detail) = check_line(&stdout, "data dir");
    assert_eq!(marker, "ok  ");
    assert_eq!(detail, format!("{} writable", sb.data_dir.path().display()));
    assert_eq!(
        check_line(&stdout, "autostart"),
        ("ok  ", "not installed (run `prx daemon install`)")
    );
    assert_eq!(
        check_line(&stdout, "menubar autostart"),
        (
            "ok  ",
            "not installed (run `prx daemon install --include-menubar`)"
        )
    );

    // The failures are counted (the warning is not), on stderr, and there is
    // no success line.
    assert!(
        stderr_of(&out).contains("error: doctor found 3 problem(s)"),
        "{}",
        stderr_of(&out)
    );
    assert!(!stdout.contains("all checks passed"), "{stdout}");
}

/// Everything in place: every line is `ok`, the summary has no warning
/// count, and the exit code is zero. Each warning then shows up in the
/// count without failing the run.
#[test]
fn doctor_summary_counts_warnings_and_only_failures_fail() {
    let sb = Sandbox::new("summary");
    sb.with_system_opener();
    write_file(&sb.config(), SH_ENGINE_CONFIG);

    let out = sb.doctor(true, &[]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "{stdout}\n{}", stderr_of(&out));
    assert!(
        !stdout.contains("[WARN]") && !stdout.contains("[FAIL]"),
        "{stdout}"
    );
    assert!(stdout.ends_with("\n\nall checks passed\n"), "{stdout}");
    assert_eq!(
        check_line(&stdout, "gh auth"),
        ("ok  ", "authenticated as testuser")
    );
    assert_eq!(
        check_line(&stdout, "engine (defaults.engine = local)"),
        ("ok  ", "local -> /bin/sh")
    );
    let (marker, detail) = check_line(&stdout, "browser");
    assert_eq!(marker, "ok  ");
    assert!(
        detail.starts_with("(defaults): system default (via "),
        "{detail}"
    );

    // One warning: a leftover prompt file with the user's own text.
    let legacy = sb.data_dir.child("prompts/default.md");
    write_file(&legacy, "my own review prompt\n");
    let out = sb.doctor(true, &[]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.contains(&format!(
            "[WARN] prompts                      {} is no longer read; paste its text into \
             defaults.review_prompt to keep your edits",
            legacy.display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.ends_with("\n\nall checks passed (1 warning)\n"),
        "{stdout}"
    );

    // Two: an llm_api engine whose key variable is not set here.
    let mut yaml = SH_ENGINE_CONFIG.to_string();
    yaml.push_str(
        "  cloud:\n    type: llm_api\n    provider: anthropic\n    model: claude-haiku\n    \
         api_key_env: PRX_DOCTOR_TEST_UNSET_KEY\nrepos:\n  - name: org/one\n    engine: cloud\n",
    );
    write_file(&sb.config(), &yaml);
    let out = sb.doctor(true, &[]);
    let stdout = stdout_of(&out);
    assert!(out.status.success(), "{stdout}");
    assert!(
        stdout.ends_with("\n\nall checks passed (2 warnings)\n"),
        "{stdout}"
    );

    // A leftover file that is just the shipped default is not worth a word.
    write_file(&legacy, prx::prompt::DEFAULT_PROMPT);
    let out = sb.doctor(true, &[("PRX_DOCTOR_TEST_UNSET_KEY", "set")]);
    let stdout = stdout_of(&out);
    assert!(stdout.ends_with("\n\nall checks passed\n"), "{stdout}");
    assert!(!stdout.contains("no longer read"), "{stdout}");
}

/// Regression: an engine command was always searched on PATH, even when it
/// was a path. With an empty PATH an absolute command (what the docs
/// recommend for launchd) was reported "not found on PATH", and a relative
/// path was "found" whenever some PATH directory happened to contain it,
/// although running it resolves against the working directory.
///
/// Every level that names an engine gets its own line: defaults, an org,
/// a repo, and the brief engines.
#[test]
fn doctor_checks_engine_commands_given_as_paths_without_consulting_path() {
    let sb = Sandbox::new("engine-paths");
    sb.with_system_opener();
    // `<gh dir>/sub/agent` exists and the gh dir is on PATH, but
    // `sub/agent` is not a file relative to where doctor runs.
    sb.tool("sub/agent");
    write_file(
        &sb.config(),
        "defaults:\n  engine: abs\n  brief:\n    engine: rel\n\
         engines:\n  abs:\n    type: agent\n    command: /bin/sh\n    args: [\"-c\", \"true\", \"{{PROMPT}}\"]\n  \
         rel:\n    type: agent\n    command: sub/agent\n    args: [\"{{PROMPT}}\"]\n  \
         ghost:\n    type: agent\n    command: /no/such/agent\n    args: [\"{{PROMPT}}\"]\n  \
         named:\n    type: agent\n    command: gh\n    args: [\"{{PROMPT}}\"]\n\
         orgs:\n  - org: org\n    engine: ghost\n\
         repos:\n  - name: org/one\n    engine: named\n    brief:\n      engine: abs\n",
    );

    let expect = |stdout: &str, gh_line: (&str, String)| {
        assert_eq!(
            check_line(stdout, "engine (defaults.engine = abs)"),
            ("ok  ", "abs -> /bin/sh")
        );
        assert_eq!(
            check_line(stdout, "engine (org/one: brief.engine = abs)"),
            ("ok  ", "abs -> /bin/sh")
        );
        assert_eq!(
            check_line(stdout, "engine (orgs[org]: engine = ghost)"),
            (
                "FAIL",
                "agent 'ghost' command '/no/such/agent' does not exist or is not executable"
            )
        );
        assert_eq!(
            check_line(stdout, "engine (defaults: brief.engine = rel)"),
            (
                "FAIL",
                "agent 'rel' command 'sub/agent' does not exist or is not executable"
            )
        );
        let (marker, detail) = check_line(stdout, "engine (org/one: engine = named)");
        assert_eq!((marker, detail), (gh_line.0, gh_line.1.as_str()));
    };

    // A bare name is still a PATH lookup: found with the gh dir on PATH...
    let out = sb.doctor(true, &[]);
    let stdout = stdout_of(&out);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    expect(
        &stdout,
        ("ok  ", format!("named -> {}", sb.gh.child("gh").display())),
    );
    assert!(
        stderr_of(&out).contains("doctor found 2 problem(s)"),
        "{}",
        stderr_of(&out)
    );

    // ...and not with PATH empty, where the absolute path still resolves.
    let out = sb.doctor(false, &[("PATH", "")]);
    let stdout = stdout_of(&out);
    expect(
        &stdout,
        (
            "FAIL",
            "agent 'named' command 'gh' not found on PATH".to_string(),
        ),
    );
    assert_eq!(
        check_line(&stdout, "gh binary"),
        ("FAIL", "gh not found on PATH")
    );
}

/// Doctor is read-only where the user's setup is concerned: with no config
/// it reports the failure and creates none.
#[test]
fn doctor_never_creates_a_config() {
    let sb = Sandbox::new("no-config");
    let out = sb.doctor(true, &[]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = stdout_of(&out);
    let (marker, detail) = check_line(&stdout, "config");
    assert_eq!(marker, "FAIL");
    assert!(detail.ends_with("run `prx init` first"), "{detail}");
    assert_eq!(
        check_line(&stdout, "engines"),
        ("FAIL", "skipped (config failed)")
    );
    assert_eq!(
        check_line(&stdout, "prompts"),
        ("FAIL", "skipped (config failed)")
    );
    // gh is independent of the config and is still checked.
    assert_eq!(
        check_line(&stdout, "gh auth"),
        ("ok  ", "authenticated as testuser")
    );
    assert!(!sb.config().exists());
}
