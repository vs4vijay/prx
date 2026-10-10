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
