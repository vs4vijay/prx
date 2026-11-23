//! What the binary does at its edges, without a daemon: where it looks for
//! its files (`PRX_CONFIG` / `PRX_DATA_DIR` / `HOME`), `prx pause` and
//! `prx resume` falling back to state.json, and argument errors.

mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use chrono::{DateTime, Duration, Utc};
use common::*;

/// Run prx with exactly the given location variables: `HOME` is always
/// redirected, and `PRX_CONFIG` / `PRX_DATA_DIR` are set only when given
/// (the shared harness always sets both, which is what these tests vary).
fn run_at(home: &Path, config: Option<&str>, data: Option<&str>, args: &[&str]) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .env("HOME", home)
        .env_remove("PRX_CONFIG")
        .env_remove("PRX_DATA_DIR")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME");
    if let Some(config) = config {
        cmd.env("PRX_CONFIG", config);
    }
    if let Some(data) = data {
        cmd.env("PRX_DATA_DIR", data);
    }
    cmd.output().expect("failed to spawn prx binary")
}

/// `config_file` and `data_dir` as `prx config show` reports them.
fn shown_locations(out: &Output) -> (String, String) {
    assert!(out.status.success(), "{}", stderr_of(out));
    let view: serde_yaml::Value = serde_yaml::from_str(&stdout_of(out)).unwrap();
    (
        view["config_file"].as_str().unwrap().to_string(),
        view["data_dir"].as_str().unwrap().to_string(),
    )
}

// ------------------------------------------------------------ locations

#[test]
fn without_overrides_everything_lives_under_home() {
    let home = TempDir::new("paths-home");
    let out = run_at(home.path(), None, None, &["init"]);
    assert!(out.status.success(), "{}", stderr_of(&out));

    let config = home.child(".config/prx/config.yml");
    let data = home.child(".local/share/prx");
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        prx::config::STARTER_CONFIG
    );
    assert!(data.is_dir());
    assert!(stdout_of(&out).contains(&format!("wrote config: {}", config.display())));

    let shown = shown_locations(&run_at(home.path(), None, None, &["config", "show"]));
    assert_eq!(
        shown,
        (config.display().to_string(), data.display().to_string())
    );
}

/// An override that is set but empty (`PRX_CONFIG=`) is the same as unset;
/// it must not resolve to the current directory or to an empty path.
#[test]
fn empty_overrides_fall_back_to_the_defaults() {
    let home = TempDir::new("paths-empty");
    let out = run_at(home.path(), Some(""), Some(""), &["init"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(home.child(".config/prx/config.yml").is_file());
    assert!(home.child(".local/share/prx").is_dir());
}

/// The two overrides are independent: each one moves only its own location.
#[test]
fn each_override_moves_only_its_own_location() {
    let home = TempDir::new("paths-split");
    let elsewhere = TempDir::new("paths-split-elsewhere");
    let custom_config = elsewhere.child("deep/nested/my-prx.yml");
    let custom_data = elsewhere.child("data/dir");
    let default_config = home.child(".config/prx/config.yml");
    let default_data = home.child(".local/share/prx");

    // Only the config moves; `init` creates its missing parent directories.
    let cfg = custom_config.to_str().unwrap();
    let out = run_at(home.path(), Some(cfg), None, &["init"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(custom_config.is_file());
    assert!(!default_config.exists());
    assert!(default_data.is_dir());
    let shown = shown_locations(&run_at(home.path(), Some(cfg), None, &["config", "show"]));
    assert_eq!(shown, (cfg.to_string(), default_data.display().to_string()));

    // Only the data dir moves.
    let data = custom_data.to_str().unwrap();
    let out = run_at(home.path(), None, Some(data), &["init"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(default_config.is_file());
    assert!(custom_data.is_dir());
    let shown = shown_locations(&run_at(home.path(), None, Some(data), &["config", "show"]));
    assert_eq!(
        shown,
        (default_config.display().to_string(), data.to_string())
    );
}

/// State written by a command lands in the overridden data dir and nowhere
/// under the home directory.
#[test]
fn data_dir_override_is_where_state_is_written() {
    let home = TempDir::new("paths-state");
    let data = TempDir::new("paths-state-data");
    let out = run_at(
        home.path(),
        None,
        Some(data.path().to_str().unwrap()),
        &["pause", "--hours", "1"],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(data.child("state.json").is_file());
    assert!(!home.child(".local").exists());
}

// ------------------------------------------------------- pause / resume

fn pause_cli(data: &Path, args: &[&str]) -> Output {
    // The config path is never read by pause/resume; point it at nothing.
    run_cli(&data.join("no-config.yml"), data, args, None, &[])
}

fn stored_pause(data: &Path) -> Option<DateTime<Utc>> {
    let raw = fs::read_to_string(data.join("state.json")).unwrap();
    let state: serde_json::Value = serde_json::from_str(&raw).unwrap();
    state
        .get("paused_until")
        .and_then(|v| v.as_str())
        .map(|s| s.parse().unwrap())
}

#[test]
fn pause_and_resume_work_without_a_daemon() {
    let data = TempDir::new("pause-inproc");

    let before = Utc::now();
    let out = pause_cli(data.path(), &["pause", "--hours", "2"]);
    let after = Utc::now();
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains("daemon not running"));
    let stdout = stdout_of(&out);
    assert!(stdout.starts_with("reviews paused until "), "{stdout}");
    assert!(stdout.contains("`prx resume` to lift it"), "{stdout}");
    // The stored end is two hours after the moment the command ran.
    let until = stored_pause(data.path()).expect("pause stored");
    assert!(until >= before + Duration::hours(2), "{until} vs {before}");
    assert!(until <= after + Duration::hours(2), "{until} vs {after}");

    let out = pause_cli(data.path(), &["resume"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out).trim(), "reviews resumed");
    assert_eq!(stored_pause(data.path()), None);

    let out = pause_cli(data.path(), &["resume"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out).trim(), "reviews were not paused");
}

#[test]
fn pause_until_accepts_an_absolute_time() {
    let data = TempDir::new("pause-until");
    let out = pause_cli(data.path(), &["pause", "--until", "2099-01-02T03:04:05Z"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("reviews paused until 2099-01-02 03:04 UTC"),
        "{}",
        stdout_of(&out)
    );
    assert_eq!(
        stored_pause(data.path()),
        Some("2099-01-02T03:04:05Z".parse().unwrap())
    );
    // A later pause replaces the earlier one.
    let out = pause_cli(data.path(), &["pause", "--until", "2098-06-01T00:00:00Z"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(
        stored_pause(data.path()),
        Some("2098-06-01T00:00:00Z".parse().unwrap())
    );
}

/// Every malformed pause request fails with its reason and writes nothing.
#[test]
fn bad_pause_requests_fail_and_write_no_state() {
    let data = TempDir::new("pause-bad");
    for (args, reason) in [
        (&["pause"][..], "specify --hours N or --until <time>"),
        (
            &["pause", "--hours", "1", "--until", "2099-01-01T00:00:00Z"],
            "specify either --hours or --until, not both",
        ),
        (&["pause", "--hours", "0"], "--hours must be > 0"),
        (&["pause", "--hours=-3"], "--hours must be > 0"),
        (&["pause", "--hours", "NaN"], "--hours must be > 0"),
        (&["pause", "--hours", "1e9"], "--hours is too large"),
        (
            &["pause", "--until", "next week"],
            "cannot parse --until 'next week'",
        ),
        (
            &["pause", "--until", "2020-01-01T00:00:00Z"],
            "the pause end 2020-01-01 00:00 UTC is not in the future",
        ),
    ] {
        let out = pause_cli(data.path(), args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = stderr_of(&out);
        assert!(err.contains(reason), "{args:?}: {err}");
        assert!(stdout_of(&out).is_empty(), "{args:?}");
        assert!(!data.child("state.json").exists(), "{args:?} wrote state");
    }
}

const TRACKED_STATE: &str = r#"{
  "schema_version": 1,
  "prs": {
    "org/repo#7": {
      "repo": "org/repo", "number": 7, "title": "Add cache", "author": "alice",
      "head_branch": "feat/cache", "head_sha": "abc", "is_draft": false,
      "gh_created_at": "2026-09-01T10:00:00Z",
      "state": "pending",
      "previous_state": null, "skip_reason": null,
      "snooze_until": null, "snooze_until_push": false,
      "review_anchor_at": "2026-09-01T10:00:00Z",
      "last_reviewed_sha": null, "last_review_at": null,
      "last_verdict": null, "last_summary": null,
      "blocking_count": 0, "nit_count": 0,
      "review_session": null, "review_attempts": 0, "diff_too_large": false,
      "updated_at": "2026-09-01T10:00:00Z"
    }
  },
  "last_poll": "2026-09-01T10:05:00Z"
}"#;

/// Pausing and resuming edit one field: the tracked PRs and the last poll
/// time in state.json are carried through untouched.
#[test]
fn pause_and_resume_keep_the_tracked_prs() {
    let data = TempDir::new("pause-keeps");
    write_file(&data.child("state.json"), TRACKED_STATE);
    let original: serde_json::Value = serde_json::from_str(TRACKED_STATE).unwrap();

    let out = pause_cli(data.path(), &["pause", "--hours", "1"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stored_pause(data.path()).is_some());
    let paused: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(data.child("state.json")).unwrap()).unwrap();
    assert_eq!(paused["prs"], original["prs"]);
    assert_eq!(paused["last_poll"], original["last_poll"]);

    let out = pause_cli(data.path(), &["resume"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let resumed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(data.child("state.json")).unwrap()).unwrap();
    assert_eq!(resumed, original);
}

/// A pause that already ran out is not a pause: `resume` says so, and
/// clears the stale timestamp while it is there.
#[test]
fn resume_reports_an_expired_pause_as_not_paused() {
    let data = TempDir::new("pause-expired");
    write_file(
        &data.child("state.json"),
        r#"{"schema_version":1,"prs":{},"last_poll":null,"paused_until":"2020-01-01T00:00:00Z"}"#,
    );
    let out = pause_cli(data.path(), &["resume"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out).trim(), "reviews were not paused");
    assert_eq!(stored_pause(data.path()), None);
}

/// A damaged state.json is never overwritten by a pause or a resume: the
/// command fails, names the file, and leaves the bytes for recovery.
#[test]
fn pause_and_resume_refuse_to_overwrite_a_corrupt_state_file() {
    let data = TempDir::new("pause-corrupt");
    let damaged = "{\"schema_version\": 1, \"prs\": {\"org/repo#7\": {";
    write_file(&data.child("state.json"), damaged);
    for args in [&["pause", "--hours", "1"][..], &["resume"]] {
        let out = pause_cli(data.path(), args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let err = stderr_of(&out);
        assert!(err.contains("failed to load PR state"), "{err}");
        assert!(err.contains("state.json is corrupt"), "{err}");
        assert_eq!(
            fs::read_to_string(data.child("state.json")).unwrap(),
            damaged
        );
    }
}

// ----------------------------------------------------- argument errors

/// A command line prx cannot parse exits 2 with the reason on stderr and
/// nothing on stdout, before any file is touched.
#[test]
fn usage_errors_exit_2_and_touch_nothing() {
    let data = TempDir::new("usage-errors");
    for args in [
        &["frobnicate"][..],
        &["review", "--all", "--engine", "claude"],
        &["snooze", "org/repo#1", "--hours", "2", "--until-push"],
        &["history", "--limit", "many"],
        &["usage", "--by", "year"],
        &["config", "set", "defaults.engine"],
        &["approve"],
    ] {
        let out = pause_cli(data.path(), args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(stdout_of(&out).is_empty(), "{args:?}");
        let err = stderr_of(&out);
        assert!(err.contains("error:"), "{args:?}: {err}");
        assert!(err.contains("try '--help'"), "{args:?}: {err}");
    }
    assert_eq!(fs::read_dir(data.path()).unwrap().count(), 0);
}

#[test]
fn version_and_help_print_to_stdout_and_succeed() {
    let data = TempDir::new("version-help");
    let out = pause_cli(data.path(), &["--version"]);
    assert!(out.status.success());
    assert_eq!(
        stdout_of(&out).trim(),
        format!("prx {}", env!("CARGO_PKG_VERSION"))
    );

    let out = pause_cli(data.path(), &["--help"]);
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
            "{command} missing from help:\n{help}"
        );
    }
}
