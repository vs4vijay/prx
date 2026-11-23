//! `prx status` end-to-end: two repos polled through the fake gh, state
//! transitions persisted to state.json, exact gh invocations asserted.

mod common;

use std::process::Output;

use chrono::{DateTime, Utc};
use common::*;
use prx::state::{PrRecord, PrState, SkipReason, StateStore, Verdict};

fn ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn setup() -> (TempDir, TempDir, TempDir, std::path::PathBuf) {
    let cfg_dir = TempDir::new("status-cfg");
    let data_dir = TempDir::new("status-data");
    let gh_dir = TempDir::new("status-gh");
    let fixture = install_fake_gh(gh_dir.path());
    let config = cfg_dir.child("config.yml");
    write_file(&config, &config_with(&["org/one", "org/two"], ""));
    (cfg_dir, data_dir, gh_dir, fixture)
}

fn load_state(data_dir: &TempDir) -> prx::state::StateFile {
    StateStore::new(data_dir.child("state.json"))
        .load()
        .expect("state.json should parse")
}

fn save_state(data_dir: &TempDir, state: &prx::state::StateFile) {
    StateStore::new(data_dir.child("state.json"))
        .save(state)
        .expect("state.json should save")
}

fn log_lines(gh_dir: &TempDir) -> Vec<String> {
    std::fs::read_to_string(gh_dir.child("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

fn status(cfg_dir: &TempDir, data_dir: &TempDir, gh_dir: &TempDir, args: &[&str]) -> Output {
    run_cli(
        cfg_dir.child("config.yml").as_path(),
        data_dir.path(),
        args,
        Some(gh_dir.path()),
        &[],
    )
}

#[test]
fn status_syncs_two_repos_through_the_lifecycle() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();

    // org/one: PR 1 (ready, created 3 days ago → cooldown long elapsed),
    // PR 2 (draft). org/two: PR 3 created 5 minutes ago → cooling down.
    let created = "2026-09-09T10:00:00Z";
    let five_min_ago = (Utc::now() - chrono::Duration::minutes(5))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[
            pr_json(
                1,
                "Add feature",
                "alice",
                "feature/x",
                "aaaa",
                false,
                created,
            ),
            pr_json(2, "WIP draft", "bob", "spike/y", "bbbb", true, created),
        ],
    );
    write_list_fixture(
        gh_dir.path(),
        "org/two",
        &[pr_json(
            3,
            "Fresh PR",
            "carol",
            "hotfix/z",
            "cccc",
            false,
            &five_min_ago,
        )],
    );

    // ---- First poll: discovery.
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success(), "status failed: {}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("org/one#1"), "{stdout}");
    assert!(stdout.contains("Add feature"));
    assert!(stdout.contains("org/two#3"));
    assert!(stdout.contains("cooling down: review in"), "{stdout}");
    assert!(stdout.contains("last poll:"));

    let state = load_state(&data_dir);
    assert_eq!(state.prs.len(), 3);
    let pr1 = &state.prs["org/one#1"];
    assert_eq!(pr1.state, PrState::Pending);
    assert_eq!(pr1.review_anchor_at, Some(ts(created))); // A5: anchor = gh creation
    let pr2 = &state.prs["org/one#2"];
    assert_eq!(pr2.state, PrState::Skipped);
    assert_eq!(pr2.skip_reason, Some(SkipReason::Draft));
    let pr3 = &state.prs["org/two#3"];
    assert_eq!(pr3.state, PrState::Pending);

    // Exact gh calls were made for both repos, in order.
    let log = log_lines(&gh_dir);
    assert_eq!(log.len(), 2, "log: {log:?}");
    assert_eq!(
        log[0],
        "pr list -R org/one --json number,title,author,headRefName,headRefOid,isDraft,createdAt,url --state open --limit 200"
    );
    assert!(log[1].contains("pr list -R org/two "));

    // ---- Second poll with no upstream change: states hold.
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    assert_eq!(
        load_state(&data_dir).prs["org/one#1"].state,
        PrState::Pending
    );

    // ---- New commits on PR 1 while first-review pending: sha updates,
    // creation anchor is preserved (A5).
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[
            pr_json(
                1,
                "Add feature",
                "alice",
                "feature/x",
                "aabb",
                false,
                created,
            ),
            pr_json(2, "WIP draft", "bob", "spike/y", "bbbb", true, created),
        ],
    );
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    let state = load_state(&data_dir);
    assert_eq!(state.prs["org/one#1"].head_sha, "aabb");
    assert_eq!(state.prs["org/one#1"].review_anchor_at, Some(ts(created)));

    // ---- PR 3 vanishes from the open list and reports MERGED.
    write_list_fixture(gh_dir.path(), "org/two", &[]);
    write_file(&gh_dir.child("org_two#3.state"), "MERGED");
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    let state = load_state(&data_dir);
    assert_eq!(state.prs["org/two#3"].state, PrState::Merged);
    assert_eq!(state.prs["org/two#3"].skip_reason, None);

    // ---- PR 2 flips draft → ready: skipped(draft) → pending.
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[
            pr_json(
                1,
                "Add feature",
                "alice",
                "feature/x",
                "aabb",
                false,
                created,
            ),
            pr_json(2, "WIP draft", "bob", "spike/y", "bbbb", false, created),
        ],
    );
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    let state = load_state(&data_dir);
    assert_eq!(state.prs["org/one#2"].state, PrState::Pending);
    assert_eq!(state.prs["org/one#2"].skip_reason, None);

    // ---- Re-review: PR 1 has been reviewed (simulate ReviewDone), then a
    // new commit lands → pending with a fresh anchor, prior verdict kept.
    let mut state = load_state(&data_dir);
    {
        let rec: &mut PrRecord = state.prs.get_mut("org/one#1").unwrap();
        rec.state = PrState::ReviewedChangesRequested;
        rec.last_verdict = Some(Verdict::ChangesRequested);
        rec.last_reviewed_sha = Some("aabb".into());
        rec.blocking_count = 2;
        rec.nit_count = 1;
    }
    save_state(&data_dir, &state);

    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[
            pr_json(
                1,
                "Add feature",
                "alice",
                "feature/x",
                "ccdd",
                false,
                created,
            ),
            pr_json(2, "WIP draft", "bob", "spike/y", "bbbb", false, created),
        ],
    );
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    let rec = &load_state(&data_dir).prs["org/one#1"];
    assert_eq!(rec.state, PrState::Pending);
    assert!(rec.is_rereview_pending());
    assert!(rec.last_verdict.is_some());
    assert!(rec.review_anchor_at.unwrap() > rec.gh_created_at); // anchored to detection time

    // ---- JSON output parses and carries the derived fields.
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status", "--json"]);
    assert!(out.status.success());
    let entries: serde_json::Value = serde_json::from_str(&stdout_of(&out)).expect("json output");
    let arr = entries.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    let by_key: std::collections::HashMap<String, &serde_json::Value> = arr
        .iter()
        .map(|e| (e["key"].as_str().unwrap().to_string(), e))
        .collect();
    assert_eq!(by_key["org/one#1"]["state"], "pending");
    assert_eq!(by_key["org/one#2"]["state"], "pending");
    assert_eq!(by_key["org/two#3"]["state"], "merged");
    assert!(by_key["org/two#3"]["summary"]
        .as_str()
        .unwrap()
        .contains("merged"));
}

#[test]
fn status_repo_filter_polls_only_that_repo() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            "2026-09-09T10:00:00Z",
        )],
    );
    write_list_fixture(
        gh_dir.path(),
        "org/two",
        &[pr_json(
            9,
            "Two",
            "bob",
            "f/y",
            "bbbb",
            false,
            "2026-09-09T10:00:00Z",
        )],
    );

    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status", "org/two"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("org/two#9"));
    assert!(!stdout.contains("org/one#1"));

    let log = log_lines(&gh_dir);
    assert_eq!(log.len(), 1, "log: {log:?}");
    assert!(log[0].contains("-R org/two "));

    // Only org/two's PR was tracked.
    let state = load_state(&data_dir);
    assert!(state.prs.contains_key("org/two#9"));
    assert!(!state.prs.contains_key("org/one#1"));

    // Unknown repo errors.
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status", "org/unknown"]);
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("not configured"));
}

#[test]
fn status_failure_paths_are_clean() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();

    // gh not available: status fails with a clear message...
    let out = run_cli_isolated(
        cfg_dir.child("config.yml").as_path(),
        data_dir.path(),
        &["status"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("failed to poll"));

    // ...and once gh works again nothing was corrupted.
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "One",
            "alice",
            "f/x",
            "aaaa",
            false,
            "2026-09-09T10:00:00Z",
        )],
    );
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    assert_eq!(
        load_state(&data_dir).prs["org/one#1"].state,
        PrState::Pending
    );

    // A later gh outage leaves the tracked record untouched.
    let out = run_cli_isolated(
        cfg_dir.child("config.yml").as_path(),
        data_dir.path(),
        &["status"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert_eq!(
        load_state(&data_dir).prs["org/one#1"].state,
        PrState::Pending
    );

    // No config: clear error.
    let empty_cfg = TempDir::new("status-noconf");
    let out = run_cli(
        empty_cfg.child("config.yml").as_path(),
        data_dir.path(),
        &["status"],
        Some(gh_dir.path()),
        &[],
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("prx init"));

    // Config with zero repos: clear error.
    write_file(
        &empty_cfg.child("config.yml"),
        "defaults:\n  agent: pi\nrepos: []\n",
    );
    let out = run_cli(
        empty_cfg.child("config.yml").as_path(),
        data_dir.path(),
        &["status"],
        Some(gh_dir.path()),
        &[],
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("no repositories configured"));
}

#[test]
fn status_marks_closed_when_vanished_pr_reports_closed() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[pr_json(
            7,
            "Gone",
            "alice",
            "f/x",
            "aaaa",
            false,
            "2026-09-09T10:00:00Z",
        )],
    );
    status(&cfg_dir, &data_dir, &gh_dir, &["status"]);

    write_list_fixture(gh_dir.path(), "org/one", &[]);
    write_file(&gh_dir.child("org_one#7.state"), "CLOSED");
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    assert_eq!(
        load_state(&data_dir).prs["org/one#7"].state,
        PrState::Closed
    );
}

#[test]
fn snooze_expiry_restores_previous_state_on_status() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[pr_json(
            4,
            "Snoozed",
            "alice",
            "f/x",
            "aaaa",
            false,
            "2026-09-09T10:00:00Z",
        )],
    );
    status(&cfg_dir, &data_dir, &gh_dir, &["status"]);

    // Snooze the PR for one minute (already expired by the time we re-run).
    let mut state = load_state(&data_dir);
    {
        let rec: &mut PrRecord = state.prs.get_mut("org/one#4").unwrap();
        rec.state = PrState::Skipped;
        rec.skip_reason = Some(SkipReason::Snoozed);
        rec.previous_state = Some(PrState::Pending);
        rec.snooze_until = Some(Utc::now() - chrono::Duration::minutes(1));
    }
    save_state(&data_dir, &state);

    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success());
    let rec = &load_state(&data_dir).prs["org/one#4"];
    assert_eq!(rec.state, PrState::Pending); // A6: previous_state restored
    assert_eq!(rec.skip_reason, None);
    assert_eq!(rec.snooze_until, None);
}

/// `prx status --json` with no daemon: stdout is the entry array and
/// nothing else, in the shape `GET /status` serves under `prs`. Scripts and
/// the app read these fields, so each one is pinned by name.
#[test]
fn status_json_is_the_bare_entry_array() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[
            pr_json(
                7,
                "Add feature",
                "alice",
                "feature/x",
                "aaaa",
                false,
                "2026-09-09T10:00:00Z",
            ),
            pr_json(
                8,
                "WIP draft",
                "bob",
                "spike/y",
                "bbbb",
                true,
                "2026-09-09T11:00:00Z",
            ),
        ],
    );
    write_list_fixture(gh_dir.path(), "org/two", &[]);

    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status", "--json"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let json: serde_json::Value = serde_json::from_str(&stdout).expect("stdout is one JSON value");
    let entries = json.as_array().expect("an array of entries");
    assert_eq!(entries.len(), 2, "{stdout}");

    let ready = &entries[0];
    for (field, value) in [
        ("key", serde_json::json!("org/one#7")),
        ("icon", serde_json::json!("\u{1f195}")),
        ("state", serde_json::json!("pending")),
        ("skip_reason", serde_json::Value::Null),
        ("title", serde_json::json!("Add feature")),
        ("author", serde_json::json!("alice")),
        ("head_branch", serde_json::json!("feature/x")),
        ("is_draft", serde_json::json!(false)),
        ("last_verdict", serde_json::Value::Null),
        ("blocking_count", serde_json::json!(0)),
        ("nit_count", serde_json::json!(0)),
        ("last_review_at", serde_json::Value::Null),
        ("summary", serde_json::json!("ready for review")),
        ("last_review_age", serde_json::json!("never")),
        ("created_at", serde_json::json!("2026-09-09T10:00:00Z")),
        ("brief_pending", serde_json::json!(false)),
        ("brief_available", serde_json::json!(true)),
        (
            "html_url",
            serde_json::json!("https://github.com/org/one/pull/7"),
        ),
        ("engine", serde_json::json!("pi")),
    ] {
        assert_eq!(ready.get(field), Some(&value), "field `{field}`: {ready}");
    }
    let draft = &entries[1];
    assert_eq!(draft["key"], "org/one#8");
    assert_eq!(draft["state"], "skipped");
    assert_eq!(draft["skip_reason"], "draft");
    assert_eq!(draft["is_draft"], true);
    assert_eq!(draft["summary"], "Skipped \u{2014} draft");
    assert_eq!(draft["skip_detail"], "draft");

    // The same payload type the thin client reads from the daemon.
    let typed: Vec<prx::status::StatusEntry> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(typed[1].skip_reason, Some(SkipReason::Draft));
    // --json still syncs: the poll ran and was persisted.
    assert_eq!(load_state(&data_dir).prs.len(), 2);
    assert_eq!(log_lines(&gh_dir).len(), 2);

    // The repo filter applies to the array as it does to the table.
    let out = status(
        &cfg_dir,
        &data_dir,
        &gh_dir,
        &["status", "org/two", "--json"],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "[]\n");
}

/// Watched repos with no open PRs: an empty table is a success, with the
/// poll recorded, not an error.
#[test]
fn status_with_no_open_prs_says_so_and_succeeds() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();
    write_list_fixture(gh_dir.path(), "org/one", &[]);
    write_list_fixture(gh_dir.path(), "org/two", &[]);

    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "No PRs tracked yet.\n");
    let state = load_state(&data_dir);
    assert!(state.prs.is_empty());
    assert!(state.last_poll.is_some(), "the poll itself is recorded");
}

/// One repo failing to poll does not hide the others: the table for what
/// did sync is printed and saved, and the command still exits non-zero
/// naming the repo that failed.
#[test]
fn status_prints_what_synced_when_one_repo_fails_to_poll() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();
    for (repo, number) in [("org/one", 1), ("org/two", 2)] {
        write_list_fixture(
            gh_dir.path(),
            repo,
            &[pr_json(
                number,
                "Add feature",
                "alice",
                "feature/x",
                "aaaa",
                false,
                "2026-09-09T10:00:00Z",
            )],
        );
    }

    // The first `gh pr list` (org/one) fails, the second (org/two) works.
    let out = run_cli(
        cfg_dir.child("config.yml").as_path(),
        data_dir.path(),
        &["status"],
        Some(gh_dir.path()),
        &[("FAKE_GH_LIST_FAIL", "1")],
    );
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("error: failed to poll: org/one"),
        "{}",
        stderr_of(&out)
    );
    assert!(!stderr_of(&out).contains("org/two"), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("org/two#2"), "{stdout}");
    assert!(!stdout.contains("org/one#1"), "{stdout}");
    assert!(stdout.contains("last poll: "), "{stdout}");

    let state = load_state(&data_dir);
    assert!(state.prs.contains_key("org/two#2"));
    assert!(!state.prs.contains_key("org/one#1"));

    // The next poll picks the failed repo up.
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stdout_of(&out).contains("org/one#1"), "{}", stdout_of(&out));
}

/// The review-control lines above the table come from the config and the
/// state file when there is no daemon: mode `off`, and an active pause.
#[test]
fn status_shows_mode_off_and_the_pause_without_a_daemon() {
    let (cfg_dir, data_dir, gh_dir, _fixture) = setup();
    write_file(
        &cfg_dir.child("config.yml"),
        &config_with(&["org/one"], "  review_mode: off\n"),
    );
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[pr_json(
            1,
            "Add feature",
            "alice",
            "feature/x",
            "aaaa",
            false,
            "2026-09-09T10:00:00Z",
        )],
    );
    let state = prx::state::StateFile {
        paused_until: Some(ts("2099-01-01T00:00:00Z")),
        ..Default::default()
    };
    save_state(&data_dir, &state);

    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let stdout = stdout_of(&out);
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines[0].starts_with("reviews paused until 2099-01-01 00:00 UTC ("),
        "{stdout}"
    );
    assert_eq!(lines[1], "review mode: off — polling and reviews stopped");
    assert!(lines[2].trim_start().starts_with("PR "), "{stdout}");
    // The pause survives the status run's own save.
    assert_eq!(
        load_state(&data_dir).paused_until,
        Some(ts("2099-01-01T00:00:00Z"))
    );

    // --json carries the entries only.
    let out = status(&cfg_dir, &data_dir, &gh_dir, &["status", "--json"]);
    assert!(stdout_of(&out).starts_with('['), "{}", stdout_of(&out));
}
