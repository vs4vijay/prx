//! Token usage on the CLI (tasks 8.6 / 9.3): `prx history` shows each run's
//! tokens and cost, and `prx usage` aggregates a seeded history file
//! in-process (no daemon) into bucket rows plus an overall row; its
//! `--json` output is the `GET /usage` shape.

mod common;

use std::path::PathBuf;
use std::process::Output;

use common::*;

struct Env {
    _cfg_dir: TempDir,
    data_dir: TempDir,
    config: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let cfg_dir = TempDir::new(&format!("{tag}-cfg"));
        let data_dir = TempDir::new(&format!("{tag}-data"));
        let config = cfg_dir.child("config.yml");
        write_file(&config, &config_with(&["org/one", "org/two"], ""));
        Env {
            _cfg_dir: cfg_dir,
            data_dir,
            config,
        }
    }

    /// Run with UTC as the local time zone so buckets are deterministic.
    fn run(&self, args: &[&str]) -> Output {
        run_cli(
            &self.config,
            self.data_dir.path(),
            args,
            None,
            &[("TZ", "UTC")],
        )
    }

    fn seed(&self, lines: &[String]) {
        write_file(
            &self.data_dir.child("history.jsonl"),
            &(lines.join("\n") + "\n"),
        );
    }
}

fn completed(ts: &str, repo: &str, number: u64, agent: &str, usage: &str) -> String {
    format!(
        r#"{{"event":"review_completed","ts":"{ts}","repo":"{repo}","number":{number},"title":"Change {number}","author":"alice","agent":"{agent}","verdict":"approved","blocking_count":0,"nit_count":1{usage}}}"#
    )
}

fn seeded() -> Vec<String> {
    vec![
        completed(
            "2026-07-30T10:00:00Z",
            "org/one",
            1,
            "claude",
            r#","usage":{"input_tokens":12345,"output_tokens":1100,"cache_read_tokens":40000,"cost_usd":0.41,"session_id":"s-1"},"summary_usage":{"input_tokens":655,"output_tokens":100}"#,
        ),
        completed(
            "2026-08-02T10:00:00Z",
            "org/one",
            1,
            "claude",
            r#","usage":{"input_tokens":3000,"output_tokens":300,"cost_usd":0.09,"session_id":"s-1"}"#,
        ),
        completed(
            "2026-08-20T10:00:00Z",
            "org/two",
            5,
            "codex",
            r#","usage":{"input_tokens":1500,"output_tokens":305,"cache_read_tokens":1024}"#,
        ),
        // Written before usage capture existed.
        completed("2026-08-21T10:00:00Z", "org/two", 6, "pi", ""),
        r#"{"event":"review_failed","ts":"2026-08-22T10:00:00Z","repo":"org/two","number":7,"title":"Change 7","author":"bob","agent":"claude","usage":{"input_tokens":800,"cost_usd":0.02}}"#.to_string(),
    ]
}

/// Task 8.6: tokens (review + summary) and the cost on completed and failed
/// runs; nothing for runs without usage.
#[test]
fn history_timeline_shows_tokens_and_cost() {
    let env = Env::new("usage-history");
    env.seed(&seeded());
    let out = env.run(&["history"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        lines,
        [
            "recent review history (newest first):",
            "  2026-08-22 10:00 \u{274c} org/two#7        review failed · 800 in · $0.02 · Change 7",
            "  2026-08-21 10:00 \u{2705} org/two#6         approved — 0 blocking, 1 nits · pi · Change 6",
            "  2026-08-20 10:00 \u{2705} org/two#5         approved — 0 blocking, 1 nits · codex · 1.5k in / 305 out / 1.0k cached · Change 5",
            "  2026-08-02 10:00 \u{2705} org/one#1         approved — 0 blocking, 1 nits · claude · 3.0k in / 300 out · $0.09 · Change 1",
            "  2026-07-30 10:00 \u{2705} org/one#1         approved — 0 blocking, 1 nits · claude · 13.0k in / 1.2k out / 40.0k cached · $0.41 · Change 1",
        ],
        "{stdout}"
    );
}

/// Task 9.3 / spec "CLI monthly totals": one row per month with tokens,
/// cost, and run counts, then the overall row; old entries are reported as
/// runs without usage.
#[test]
fn usage_prints_month_rows_and_an_overall_row() {
    let env = Env::new("usage-month");
    env.seed(&seeded());
    let out = env.run(&["usage", "--by", "month", "--window", "all"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("daemon not running"),
        "{}",
        stderr_of(&out)
    );
    let stdout = stdout_of(&out);
    let row = |prefix: &str| {
        stdout
            .lines()
            .find(|l| l.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} row: {stdout}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    assert_eq!(
        row("month"),
        "month input output cache read cache write cost runs"
    );
    assert_eq!(row("2026-07"), "2026-07 13.0k 1.2k 40.0k 0 $0.41 1");
    assert_eq!(row("2026-08"), "2026-08 5.3k 605 1.0k 0 $0.11 3");
    assert_eq!(row("overall"), "overall 18.3k 1.8k 41.0k 0 $0.52 4");
    assert!(
        stdout.contains("1 run(s) without usage data are not in these totals"),
        "{stdout}"
    );

    // Grouped by engine: one section per engine with its own total row.
    let out = env.run(&[
        "usage", "--by", "month", "--window", "all", "--group", "engine", "--repo", "org/two",
    ]);
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains("\ncodex\n") && stdout.contains("\nclaude\n"),
        "{stdout}"
    );
    assert!(!stdout.contains("2026-07"), "repo filter: {stdout}");

    // Invalid values are rejected with the allowed ones.
    let out = env.run(&["usage", "--by", "hour"]);
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("expected one of day, week, month"),
        "{}",
        stderr_of(&out)
    );
}

/// Task 9.3: `--json` is the `GET /usage` shape (notes.md).
#[test]
fn usage_json_matches_the_endpoint_shape() {
    let env = Env::new("usage-json");
    env.seed(&seeded());
    let out = env.run(&[
        "usage", "--by", "week", "--window", "all", "--group", "session", "--json",
    ]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["bucket"], "week");
    assert_eq!(v["window"], "all");
    assert_eq!(v["group"], "session");
    assert_eq!(v["runs_without_usage"], 1);
    let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "bucket",
            "group",
            "groups",
            "overall",
            "runs_without_usage",
            "series",
            "window"
        ]
    );
    // The resumed claude session rolls up two runs; review/summary split.
    let session = v["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["group"] == "claude:s-1")
        .unwrap();
    assert_eq!(session["totals"]["runs"], 2);
    assert_eq!(session["totals"]["input_tokens"], 16000);
    assert_eq!(session["totals"]["summary"]["input_tokens"], 655);
    assert_eq!(session["totals"]["runs_with_cost"], 2);
    // The first point is the ISO week (Monday) of the first run.
    assert_eq!(v["series"][0]["points"][0]["bucket_start"], "2026-07-27");
    // Same shape as the typed endpoint payload.
    serde_json::from_value::<prx::usage::Usage>(v).unwrap();
}

/// The uniform token semantics, end to end: `input` is uncached input,
/// cache reads and writes have their own columns, and nothing is folded
/// into anything else. Cost is the sum of the costs runs reported: with
/// none reported the column is `-`, never `$0.00`.
#[test]
fn usage_keeps_input_cache_and_cost_apart() {
    let env = Env::new("usage-semantics");
    env.seed(&[
        completed(
            "2026-08-20T10:00:00Z",
            "org/one",
            1,
            "claude",
            r#","usage":{"input_tokens":100,"output_tokens":10,"cache_read_tokens":2000,"cache_write_tokens":300}"#,
        ),
        completed(
            "2026-08-21T10:00:00Z",
            "org/one",
            2,
            "codex",
            r#","usage":{"input_tokens":50,"cache_read_tokens":500}"#,
        ),
    ]);
    let out = env.run(&["usage", "--by", "month", "--window", "all", "--json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let overall = &v["overall"];
    assert_eq!(overall["input_tokens"], 150);
    assert_eq!(overall["output_tokens"], 10);
    assert_eq!(overall["cache_read_tokens"], 2500);
    assert_eq!(overall["cache_write_tokens"], 300);
    assert_eq!(overall["runs"], 2);
    assert_eq!(overall["runs_with_cost"], 0);
    assert_eq!(overall["cost_usd"], 0.0);

    let out = env.run(&["usage", "--by", "month", "--window", "all"]);
    let stdout = stdout_of(&out);
    let overall: Vec<&str> = stdout
        .lines()
        .find(|l| l.starts_with("overall"))
        .unwrap_or_else(|| panic!("no overall row: {stdout}"))
        .split_whitespace()
        .collect();
    assert_eq!(overall, ["overall", "150", "10", "2.5k", "300", "-", "2"]);
    assert!(!stdout.contains("without usage data"), "{stdout}");
}

/// Aggregation across engines, repos, and PRs from one seeded history:
/// each `--group` partitions the same runs, and the group totals add up to
/// the overall row whichever way they are cut.
#[test]
fn usage_groups_partition_the_same_runs() {
    let env = Env::new("usage-groups");
    env.seed(&seeded());
    let groups = |group: &str| -> Vec<(String, u64, u64)> {
        let out = env.run(&[
            "usage", "--by", "month", "--window", "all", "--group", group, "--json",
        ]);
        assert!(out.status.success(), "{}", stderr_of(&out));
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(v["overall"]["runs"], 4, "--group {group}");
        assert_eq!(v["overall"]["input_tokens"], 18_300, "--group {group}");
        v["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| {
                (
                    g["group"].as_str().unwrap().to_string(),
                    g["totals"]["runs"].as_u64().unwrap(),
                    g["totals"]["input_tokens"].as_u64().unwrap(),
                )
            })
            .collect()
    };
    let row = |name: &str, runs, input| (name.to_string(), runs, input);
    assert_eq!(groups("none"), [row("all", 4, 18_300)]);
    assert_eq!(
        groups("engine"),
        [row("claude", 3, 16_800), row("codex", 1, 1_500)]
    );
    assert_eq!(
        groups("repo"),
        [row("org/one", 2, 16_000), row("org/two", 2, 2_300)]
    );
    assert_eq!(
        groups("pr"),
        [
            row("org/one#1", 2, 16_000),
            row("org/two#5", 1, 1_500),
            row("org/two#7", 1, 800)
        ]
    );
    for group in ["engine", "repo", "pr", "session"] {
        let rows = groups(group);
        assert_eq!(rows.iter().map(|r| r.1).sum::<u64>(), 4, "--group {group}");
        assert_eq!(
            rows.iter().map(|r| r.2).sum::<u64>(),
            18_300,
            "--group {group}"
        );
    }
}

/// The default window is 30 days ending today, so a history that is all
/// older than that reports nothing (and no "without usage" note), while
/// `--window all` reaches it. Invalid flag values are usage errors that
/// name the allowed values.
#[test]
fn usage_default_window_excludes_old_runs() {
    let env = Env::new("usage-window");
    env.seed(&[completed(
        "2020-01-15T10:00:00Z",
        "org/one",
        1,
        "claude",
        r#","usage":{"input_tokens":100}"#,
    )]);
    let out = env.run(&["usage", "--json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        (
            v["bucket"].as_str(),
            v["window"].as_str(),
            v["group"].as_str()
        ),
        (Some("day"), Some("30d"), Some("none"))
    );
    assert_eq!(v["overall"]["runs"], 0);
    assert_eq!(v["runs_without_usage"], 0);
    assert_eq!(v["series"][0]["points"].as_array().unwrap().len(), 30);

    let out = env.run(&["usage", "--window", "all", "--by", "month", "--json"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["overall"]["input_tokens"], 100);
    assert_eq!(v["series"][0]["points"][0]["bucket_start"], "2020-01-01");

    for (flag, value, allowed) in [
        ("--window", "14d", "7d, 30d, 90d, all"),
        ("--group", "author", "none, engine, repo, pr, session"),
        ("--by", "year", "day, week, month"),
    ] {
        let out = env.run(&["usage", flag, value]);
        assert_eq!(out.status.code(), Some(2), "{flag} {value}");
        assert!(
            stderr_of(&out).contains(&format!("expected one of {allowed}")),
            "{}",
            stderr_of(&out)
        );
        assert_eq!(stdout_of(&out), "");
    }
}
