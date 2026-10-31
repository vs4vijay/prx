//! The CLI with no daemon running: every command's in-process path, its
//! output, and its exit code. Covers the manual mutations (`snooze`,
//! `ignore`, `unignore`) applied straight to state.json, the pause gate on
//! `review-now` / `review --all`, what each command does with no config
//! file, `prx init`, and the argument errors `main.rs` raises itself.
//!
//! Every run gets its own config, data dir, and HOME, and a PATH that puts
//! the fake `gh` (and the do-nothing notifier stand-ins) first.

mod common;

use std::path::PathBuf;
use std::process::Output;

use chrono::{Duration, Utc};
use common::*;
use prx::state::{PrState, SkipReason, StateFile, StateStore};

const NOT_RUNNING: &str = "daemon not running — running in-process";

struct Env {
    cfg_dir: TempDir,
    data_dir: TempDir,
    gh_dir: TempDir,
    home: TempDir,
    config: PathBuf,
}

impl Env {
    /// A sandbox with the fake gh installed and no config file yet.
    fn bare(tag: &str) -> Self {
        let cfg_dir = TempDir::new(&format!("fb-{tag}-cfg"));
        let data_dir = TempDir::new(&format!("fb-{tag}-data"));
        let gh_dir = TempDir::new(&format!("fb-{tag}-gh"));
        let home = TempDir::new(&format!("fb-{tag}-home"));
        install_fake_gh(gh_dir.path());
        let config = cfg_dir.child("config.yml");
        Env {
            cfg_dir,
            data_dir,
            gh_dir,
            home,
            config,
        }
    }

    /// A sandbox watching `org/one`, whose PR #1 is open and long past its
    /// cooldown. Nothing is tracked until `prx status` runs.
    fn new(tag: &str) -> Self {
        let env = Env::bare(tag);
        write_file(&env.config, &config_with(&["org/one"], ""));
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
                "2026-09-01T10:00:00Z",
            )],
        );
        env
    }

    /// The same, with PR #1 already tracked (one `prx status`).
    fn tracked(tag: &str) -> Self {
        let env = Env::new(tag);
        let out = env.run(&["status"]);
        assert!(out.status.success(), "{}", stderr_of(&out));
        env
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with(args, &[])
    }

    fn run_with(&self, args: &[&str], extra: &[(&str, &str)]) -> Output {
        let home = self.home.path().to_str().expect("temp home is utf-8");
        let mut env = vec![("HOME", home), ("TZ", "UTC")];
        env.extend_from_slice(extra);
        run_cli(
            &self.config,
            self.data_dir.path(),
            args,
            Some(self.gh_dir.path()),
            &env,
        )
    }

    fn state(&self) -> StateFile {
        StateStore::new(self.data_dir.child("state.json"))
            .load()
            .expect("state.json should parse")
    }

    fn state_bytes(&self) -> Vec<u8> {
        std::fs::read(self.data_dir.child("state.json")).unwrap_or_default()
    }

    /// Every `gh` invocation so far, one line each.
    fn gh_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.gh_dir.child("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

/// Exit code 1 with exactly one `error:` line on stderr containing `needle`.
#[track_caller]
fn assert_fails_with(out: &Output, needle: &str) {
    let stderr = stderr_of(out);
    assert_eq!(out.status.code(), Some(1), "stderr: {stderr}");
    let errors: Vec<&str> = stderr
        .lines()
        .filter(|l| l.starts_with("error: "))
        .collect();
    assert_eq!(errors.len(), 1, "{stderr}");
    assert!(errors[0].contains(needle), "wanted {needle:?} in: {stderr}");
}

// ------------------------------------------------- snooze / ignore / unignore

/// `prx ignore` / `prx unignore` with the daemon down: the transition is
/// applied to state.json, each command says what it did, and repeating one
/// is a reported no-op, not an error.
#[test]
fn ignore_and_unignore_apply_in_process_and_repeat_as_no_ops() {
    let env = Env::tracked("ignore");
    assert_eq!(env.state().prs["org/one#1"].state, PrState::Pending);

    let out = env.run(&["ignore", "org/one#1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "org/one#1 ignored\n");
    assert!(stderr_of(&out).contains(NOT_RUNNING), "{}", stderr_of(&out));
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::Skipped);
    assert_eq!(rec.skip_reason, Some(SkipReason::Ignored));

    let out = env.run(&["ignore", "org/one#1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "org/one#1: nothing to do (current state Skipped)\n"
    );
    assert_eq!(env.state().prs["org/one#1"], rec, "a no-op changes nothing");

    // The table shows it, and a poll does not bring it back.
    let out = env.run(&["status"]);
    assert!(stdout_of(&out).contains("ignored"), "{}", stdout_of(&out));
    assert_eq!(
        env.state().prs["org/one#1"].skip_reason,
        Some(SkipReason::Ignored)
    );

    let out = env.run(&["unignore", "org/one#1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "org/one#1 un-ignored (pending, cooldown restarted)\n"
    );
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::Pending);
    assert_eq!(rec.skip_reason, None);

    let out = env.run(&["unignore", "org/one#1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        stdout_of(&out),
        "org/one#1: nothing to do (current state Pending)\n"
    );

    // None of this is a GitHub write: only the polls reached gh.
    assert!(
        env.gh_calls().iter().all(|c| c.starts_with("pr list ")),
        "{:?}",
        env.gh_calls()
    );
}

/// `prx snooze` in process: `--hours` records the end time it prints,
/// `--until-push` records the flag, and the PR reads as snoozed afterwards.
#[test]
fn snooze_applies_in_process_for_hours_and_until_push() {
    let env = Env::tracked("snooze");
    let earliest = Utc::now() + Duration::hours(2);
    let out = env.run(&["snooze", "org/one#1", "--hours", "2"]);
    let latest = Utc::now() + Duration::hours(2);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains(NOT_RUNNING), "{}", stderr_of(&out));

    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.state, PrState::Skipped);
    assert_eq!(rec.skip_reason, Some(SkipReason::Snoozed));
    assert_eq!(rec.previous_state, Some(PrState::Pending));
    let until = rec.snooze_until.expect("a timed snooze records its end");
    assert!(earliest <= until && until <= latest, "{until}");
    assert!(!rec.snooze_until_push);
    assert_eq!(
        stdout_of(&out),
        format!(
            "org/one#1 snoozed until {}\n",
            until.format("%Y-%m-%d %H:%M UTC")
        )
    );

    // Fractional hours are honoured to the minute.
    let env = Env::tracked("snooze-fraction");
    let earliest = Utc::now() + Duration::minutes(30);
    let out = env.run(&["snooze", "org/one#1", "--hours", "0.5"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let until = env.state().prs["org/one#1"].snooze_until.unwrap();
    assert!(
        earliest <= until && until <= Utc::now() + Duration::minutes(30),
        "{until}"
    );

    let env = Env::tracked("snooze-push");
    let out = env.run(&["snooze", "org/one#1", "--until-push"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "org/one#1 snoozed until next push\n");
    let rec = env.state().prs["org/one#1"].clone();
    assert_eq!(rec.skip_reason, Some(SkipReason::Snoozed));
    assert_eq!(rec.snooze_until, None);
    assert!(rec.snooze_until_push);
    let out = env.run(&["status"]);
    assert!(
        stdout_of(&out).contains("snoozed until next push"),
        "{}",
        stdout_of(&out)
    );
}

/// Regression: with the daemon down `prx snooze --hours 1e15` panicked
/// (`DateTime + TimeDelta` overflowed) and `--hours nan` "snoozed" the PR
/// until the current minute. The fallback now refuses what the daemon's
/// `POST /snooze` refuses, and a refused snooze leaves the state alone.
#[test]
fn snooze_rejects_durations_the_daemon_would_reject() {
    let env = Env::tracked("snooze-bounds");
    let before = env.state_bytes();
    for (hours, message) in [
        ("0", "--hours must be > 0"),
        ("nan", "--hours must be > 0"),
        ("inf", "--hours must be > 0"),
        ("1e15", "--hours is too large (max 87600)"),
        ("87600.5", "--hours is too large (max 87600)"),
    ] {
        let out = env.run(&["snooze", "org/one#1", "--hours", hours]);
        assert_fails_with(&out, message);
        assert_eq!(stdout_of(&out), "", "--hours {hours}");
        assert_eq!(env.state_bytes(), before, "--hours {hours}");
    }
    // Neither flag is an error from prx; both at once is a usage error.
    assert_fails_with(
        &env.run(&["snooze", "org/one#1"]),
        "specify --hours N or --until-push",
    );
    let out = env.run(&["snooze", "org/one#1", "--hours", "2", "--until-push"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr_of(&out));
    assert_eq!(env.state_bytes(), before);

    // The cap itself is accepted.
    let out = env.run(&["snooze", "org/one#1", "--hours", "87600"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
}

/// The manual mutations name what is wrong with their target: not a PR
/// reference, a repo that is not watched, a PR that was never polled.
#[test]
fn manual_mutations_reject_bad_unwatched_and_untracked_targets() {
    let env = Env::new("targets");
    for command in ["ignore", "unignore"] {
        assert_fails_with(
            &env.run(&[command, "nonsense"]),
            "invalid PR target 'nonsense': expected org/repo#123",
        );
        // The short forms need the daemon's watched-repo list.
        assert_fails_with(
            &env.run(&[command, "1"]),
            "invalid PR target '1': expected org/repo#123",
        );
        assert_fails_with(
            &env.run(&[command, "org/zzz#1"]),
            "repo 'org/zzz' is not configured; add it with `prx repos add org/zzz`",
        );
        assert_fails_with(
            &env.run(&[command, "org/one#1"]),
            "org/one#1 is not tracked yet; run `prx status` first",
        );
    }
    assert_fails_with(
        &env.run(&["snooze", "org/zzz#1", "--until-push"]),
        "repo 'org/zzz' is not configured",
    );
    assert_fails_with(
        &env.run(&["snooze", "org/one#1", "--until-push"]),
        "org/one#1 is not tracked yet; run `prx status` first",
    );
    assert!(env.gh_calls().is_empty(), "{:?}", env.gh_calls());
    assert!(
        !env.data_dir.child("state.json").exists(),
        "a refused mutation creates no state"
    );
}

// ------------------------------------------------------------------- pause

/// A pause set with the daemon down gates the automatic in-process paths:
/// `review-now` and `review --all` review nothing (and never reach gh),
/// `status` shows the pause above the table, and `resume` lifts it once.
#[test]
fn pause_gates_review_now_and_review_all_in_process() {
    let env = Env::tracked("pause-gate");
    let polls = env.gh_calls().len();

    let out = env.run(&["pause", "--hours", "1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.starts_with("reviews paused until "), "{stdout}");
    assert!(stdout.ends_with(" — `prx resume` to lift it\n"), "{stdout}");
    let until = env.state().paused_until.expect("the pause is persisted");
    assert!(until > Utc::now() + Duration::minutes(59), "{until}");

    let out = env.run(&["review-now"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.starts_with("reviews paused until ")
            && stdout.ends_with("; nothing was reviewed (`prx resume` to lift it)\n"),
        "{stdout}"
    );
    let out = env.run(&["review", "--all"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out)
            .ends_with("; nothing was reviewed (review a single PR explicitly, or `prx resume`)\n"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(env.gh_calls().len(), polls, "a paused run never polls");

    let out = env.run(&["status"]);
    let stdout = stdout_of(&out);
    let first = stdout.lines().next().unwrap_or_default();
    assert!(
        first.starts_with("reviews paused until ")
            && first.ends_with(" — polling continues; resume with `prx resume`"),
        "{stdout}"
    );
    assert_eq!(env.gh_calls().len(), polls + 1, "status still polls");

    let out = env.run(&["resume"]);
    assert_eq!(stdout_of(&out), "reviews resumed\n");
    assert_eq!(env.state().paused_until, None);
    let out = env.run(&["resume"]);
    assert!(out.status.success());
    assert_eq!(stdout_of(&out), "reviews were not paused\n");
    let out = env.run(&["status"]);
    assert!(!stdout_of(&out).contains("paused"), "{}", stdout_of(&out));
}

/// `prx pause` validates its arguments before anything is written, the
/// same way on both paths.
#[test]
fn pause_argument_errors_write_nothing() {
    let env = Env::new("pause-args");
    assert_fails_with(&env.run(&["pause"]), "specify --hours N or --until <time>");
    assert_fails_with(
        &env.run(&["pause", "--hours", "1", "--until", "2099-01-01T00:00:00Z"]),
        "specify either --hours or --until, not both",
    );
    assert_fails_with(&env.run(&["pause", "--hours", "0"]), "--hours must be > 0");
    assert_fails_with(
        &env.run(&["pause", "--hours", "1e9"]),
        "--hours is too large (max 87600)",
    );
    assert_fails_with(
        &env.run(&["pause", "--until", "yesterday"]),
        "cannot parse --until 'yesterday'",
    );
    assert_fails_with(
        &env.run(&["pause", "--until", "2020-01-01T00:00:00Z"]),
        "the pause end 2020-01-01 00:00 UTC is not in the future",
    );
    assert!(!env.data_dir.child("state.json").exists());

    // An absolute end is stored as given.
    let out = env.run(&["pause", "--until", "2099-01-01T00:00:00Z"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).starts_with("reviews paused until 2099-01-01 00:00 UTC ("),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(
        env.state().paused_until,
        Some("2099-01-01T00:00:00Z".parse().unwrap())
    );
}

// ------------------------------------------------------------------ review

/// `prx review` needs exactly one of a target and `--all`; `main.rs` checks
/// that before it looks for a daemon or a config.
#[test]
fn review_needs_a_target_or_all_but_not_both() {
    let env = Env::bare("review-args");
    let out = env.run(&["review"]);
    assert_fails_with(&out, "specify a PR target (org/repo#123) or --all");
    assert!(!stderr_of(&out).contains("daemon not running"));
    assert_fails_with(
        &env.run(&["review", "org/one#1", "--all"]),
        "specify either a PR target or --all, not both",
    );
    // The per-run options are for one PR: with --all they are usage errors.
    for flags in [
        &["--engine", "pi"][..],
        &["--no-post"],
        &["--prompt", "be brief"],
    ] {
        let mut args = vec!["review", "--all"];
        args.extend_from_slice(flags);
        let out = env.run(&args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", stderr_of(&out));
        assert!(
            stderr_of(&out).contains("cannot be used with"),
            "{}",
            stderr_of(&out)
        );
    }
    assert!(env.gh_calls().is_empty());
}

// --------------------------------------------------------------- no config

/// With no config file, every command that needs one fails with exit code
/// 1 and points at `prx init`; none of them creates the file or reaches gh.
#[test]
fn commands_that_need_a_config_point_at_prx_init() {
    let env = Env::bare("noconfig");
    let commands: &[&[&str]] = &[
        &["status"],
        &["status", "--json"],
        &["status", "org/one"],
        &["review", "org/one#1"],
        &["review", "--all"],
        &["review", "org/one#1", "--dry-run"],
        &["review-now"],
        &["snooze", "org/one#1", "--hours", "1"],
        &["ignore", "org/one#1"],
        &["unignore", "org/one#1"],
        &["brief", "org/one#1"],
        &["repos", "list"],
        &["repos", "add", "org/one"],
        &["repos", "remove", "org/one"],
        &["config"],
        &["config", "show"],
        &["config", "set", "defaults.poll_interval_minutes", "5"],
    ];
    for args in commands {
        let out = env.run(args);
        let stderr = stderr_of(&out);
        assert_eq!(out.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains(&format!(
                "config file not found at {}: run `prx init` first",
                env.config.display()
            )),
            "{args:?}: {stderr}"
        );
        assert_eq!(stdout_of(&out), "", "{args:?}");
    }
    assert!(!env.config.exists(), "nothing wrote a config");
    assert!(env.gh_calls().is_empty(), "{:?}", env.gh_calls());

    // `doctor` reports it as its config check and fails.
    let out = env.run(&["doctor"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("[FAIL] config") && stdout.contains("run `prx init` first"),
        "{stdout}"
    );
    assert!(
        stdout.contains("[FAIL] engines") && stdout.contains("skipped (config failed)"),
        "{stdout}"
    );
    assert!(!env.config.exists());
}

/// A config that exists but does not parse is reported as what it is: the
/// commands that edit the config must not tell the user to run `prx init`
/// over a YAML typo.
#[test]
fn a_config_that_does_not_parse_is_not_answered_with_run_prx_init() {
    let env = Env::bare("badyaml");
    let broken = "defaults: [broken\n  yaml: {{{{";
    write_file(&env.config, broken);
    let commands: &[&[&str]] = &[
        &["repos", "list"],
        &["repos", "add", "org/one"],
        &["repos", "remove", "org/one"],
        &["config", "set", "defaults.poll_interval_minutes", "5"],
    ];
    for args in commands {
        let out = env.run(args);
        let stderr = stderr_of(&out);
        assert_fails_with(
            &out,
            &format!("failed to parse config file {}", env.config.display()),
        );
        assert!(!stderr.contains("prx init"), "{args:?}: {stderr}");
        assert_eq!(std::fs::read_to_string(&env.config).unwrap(), broken);
    }
}

/// The commands that only read history or the pause work on a machine with
/// no config at all, and report "nothing yet" rather than failing.
#[test]
fn history_views_and_the_pause_work_without_a_config() {
    let env = Env::bare("noconfig-ok");
    let out = env.run(&["history"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "no history yet\n");
    assert!(
        stderr_of(&out).contains("daemon not running — reading history in-process"),
        "{}",
        stderr_of(&out)
    );
    // Also with --json: there is no entry to print, so no JSON either.
    let out = env.run(&["history", "--json"]);
    assert_eq!(stdout_of(&out), "no history yet\n");

    let out = env.run(&["insights"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("no reviews recorded yet"),
        "{}",
        stdout_of(&out)
    );
    assert!(
        stderr_of(&out).contains("daemon not running — computing insights in-process"),
        "{}",
        stderr_of(&out)
    );

    let out = env.run(&["usage", "--json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let usage: prx::usage::Usage = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(usage.overall.runs, 0);
    assert_eq!(usage.runs_without_usage, 0);

    let out = env.run(&["resume"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "reviews were not paused\n");

    for args in [&["daemon", "status"][..], &["daemon", "stop"]] {
        let out = env.run(args);
        assert!(out.status.success(), "{args:?}: {}", stderr_of(&out));
        assert_eq!(stdout_of(&out), "daemon not running\n", "{args:?}");
    }
    assert!(!env.config.exists());
}

// -------------------------------------------------------------------- init

/// `prx init` writes a config that loads, says where, and lists the next
/// steps; a second run reports the existing file and changes nothing.
#[test]
fn init_creates_a_loadable_config_once() {
    let env = Env::bare("init");
    let nested = env.cfg_dir.child("a/b/config.yml");
    let data = env.data_dir.child("x/y");
    let home = env.home.path().to_str().unwrap();
    let run = |args: &[&str]| run_cli(&nested, &data, args, None, &[("HOME", home)]);

    let out = run(&["init"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.starts_with(&format!("wrote config: {}\n", nested.display())),
        "{stdout}"
    );
    for step in ["prx repos add <org/repo>", "prx doctor", "prx status"] {
        assert!(stdout.contains(step), "{stdout}");
    }
    assert!(data.is_dir(), "init creates the data dir");
    let written = std::fs::read(&nested).unwrap();
    assert_eq!(written, prx::config::STARTER_CONFIG.as_bytes());

    // It is usable as written: it shows, and a repo can be added to it.
    assert!(run(&["config", "show"]).status.success());
    let out = run(&["repos", "add", "org/one"]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    let edited = std::fs::read(&nested).unwrap();
    let out = run(&["init"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).starts_with(&format!("config already exists: {}\n", nested.display())),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(std::fs::read(&nested).unwrap(), edited, "never overwritten");
}

/// `prx init` never repairs or replaces an existing config, however broken:
/// the file keeps its bytes and its own error keeps surfacing.
#[test]
fn init_leaves_an_invalid_config_untouched() {
    let env = Env::bare("init-invalid");
    for broken in [
        "defaults: [broken\n  yaml: {{{{",
        "",
        "defaults:\n  engine: nope\n",
    ] {
        write_file(&env.config, broken);
        let out = env.run(&["init"]);
        assert!(out.status.success(), "{}", stderr_of(&out));
        assert!(
            stdout_of(&out).contains("config already exists"),
            "{}",
            stdout_of(&out)
        );
        assert_eq!(std::fs::read_to_string(&env.config).unwrap(), broken);
    }
    // The last one still fails where it is used.
    let out = env.run(&["repos", "list"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr_of(&out).contains("nope"), "{}", stderr_of(&out));
}

/// A config or data location that cannot be created is a clean error.
#[test]
fn init_reports_a_location_it_cannot_create() {
    let env = Env::bare("init-blocked");
    let blocker = env.cfg_dir.child("blocker");
    write_file(&blocker, "a file, not a directory");
    let home = env.home.path().to_str().unwrap();

    let out = run_cli(
        &blocker.join("config.yml"),
        env.data_dir.path(),
        &["init"],
        None,
        &[("HOME", home)],
    );
    assert_fails_with(&out, "cannot create config dir");

    let out = run_cli(
        &env.config,
        &blocker.join("data"),
        &["init"],
        None,
        &[("HOME", home)],
    );
    assert_fails_with(&out, "cannot create data dir");
    assert!(!env.config.exists(), "no config without a data dir");
}

// ------------------------------------------------------------- config edit

/// `prx config edit` runs `$VISUAL`, else `$EDITOR`, on the config path,
/// and says what to do when neither is set.
#[test]
fn config_edit_prefers_visual_over_editor_and_needs_one_of_them() {
    let env = Env::new("config-edit");
    // Two "editors" that record which one ran and on what.
    let record = |name: &str| -> String {
        let script = env.cfg_dir.child(name);
        write_file(
            &script,
            &format!(
                "#!/bin/sh\nprintf '{name} %s\\n' \"$1\" >> '{}'\n",
                env.cfg_dir.child("opened.log").display()
            ),
        );
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.display().to_string()
    };
    let (visual, editor) = (record("visual"), record("editor"));
    let opened = || std::fs::read_to_string(env.cfg_dir.child("opened.log")).unwrap_or_default();

    // run_cli inherits the caller's environment: clear both explicitly.
    let run = |vars: &[(&str, &str)]| {
        let mut cmd = std::process::Command::new(bin());
        cmd.args(["config", "edit"])
            .env("PRX_CONFIG", &env.config)
            .env("PRX_DATA_DIR", env.data_dir.path())
            .env("HOME", env.home.path())
            .env_remove("VISUAL")
            .env_remove("EDITOR");
        for (k, v) in vars {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };

    let out = run(&[]);
    assert_fails_with(
        &out,
        &format!(
            "no $EDITOR (or $VISUAL) set; edit {} directly",
            env.config.display()
        ),
    );

    assert!(run(&[("EDITOR", editor.as_str())]).status.success());
    assert_eq!(opened(), format!("editor {}\n", env.config.display()));
    let both = [("EDITOR", editor.as_str()), ("VISUAL", visual.as_str())];
    assert!(run(&both).status.success());
    assert_eq!(
        opened(),
        format!("editor {0}\nvisual {0}\n", env.config.display())
    );

    // An empty or blank $VISUAL is no editor: $EDITOR is used, and with
    // neither usable the command says so.
    std::fs::remove_file(env.cfg_dir.child("opened.log")).unwrap();
    for blank in ["", "  "] {
        let out = run(&[("EDITOR", editor.as_str()), ("VISUAL", blank)]);
        assert!(out.status.success(), "{blank:?}: {}", stderr_of(&out));
    }
    assert_eq!(
        opened(),
        format!("editor {0}\neditor {0}\n", env.config.display())
    );
    let out = run(&[("EDITOR", " "), ("VISUAL", "")]);
    assert_fails_with(&out, "no $EDITOR (or $VISUAL) set");

    // An editor that fails is a failure of the command.
    let out = run(&[("EDITOR", "false")]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
}

// -------------------------------------------------------------- the binary

/// `--version` is the crate version, `--help` lists every subcommand, and a
/// usage error (no command, an unknown one) exits 2, distinct from the 1 of
/// a command that ran and failed.
#[test]
fn version_help_and_usage_errors() {
    let env = Env::bare("binary");
    let out = env.run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out),
        format!("prx {}\n", env!("CARGO_PKG_VERSION"))
    );

    let out = env.run(&["--help"]);
    assert!(out.status.success());
    let help = stdout_of(&out);
    for command in [
        "init",
        "doctor",
        "repos",
        "status",
        "review",
        "brief",
        "approve",
        "snooze",
        "ignore",
        "unignore",
        "review-now",
        "pause",
        "resume",
        "history",
        "insights",
        "usage",
        "config",
        "daemon",
    ] {
        assert!(
            help.lines()
                .any(|l| l.trim_start().starts_with(&format!("{command} "))),
            "`{command}` missing from --help:\n{help}"
        );
    }

    for args in [&[][..], &["nosuch"], &["status", "--nosuch"], &["repos"]] {
        let out = env.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(stdout_of(&out), "", "{args:?}");
        assert!(stderr_of(&out).contains("Usage: prx"), "{args:?}");
    }
    assert!(!env.config.exists());
}
