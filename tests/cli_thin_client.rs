//! The CLI as a thin client: with a daemon answering on the port in
//! `daemon.port`, each command sends one request and prints the daemon's
//! answer; it reads no config, polls no repo, and writes no state itself.
//!
//! The daemon here is a scripted HTTP stand-in (no real daemon, no `gh`):
//! each test sets the reply for a route and asserts the request the CLI
//! made, what it printed, and its exit code. Requests are answered by the
//! stand-in's own thread, so nothing is timing-dependent.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Output;
use std::sync::{Arc, Mutex};

use common::*;
use serde_json::{json, Value};

/// One request the stand-in received.
#[derive(Debug, Clone)]
struct Request {
    /// `METHOD /path?query`.
    line: String,
    authorization: String,
    client: String,
    body: String,
}

/// A scripted daemon plus the sandbox the CLI runs in. There is no config
/// file at `config`: a thin client must not need one.
struct Stub {
    _cfg_dir: TempDir,
    data_dir: TempDir,
    home: TempDir,
    config: PathBuf,
    seen: Arc<Mutex<Vec<Request>>>,
    /// `(request-line prefix, HTTP status, body)`; the newest match wins.
    routes: Arc<Mutex<Vec<(String, u16, String)>>>,
}

/// A healthy `GET /status` body with the given extra fields merged in.
fn status_body(extra: Value) -> String {
    let mut body = json!({
        "version": "0.0.0", "pid": 1, "uptime_seconds": 1, "in_flight": 0,
        "queue_depth": 0, "last_poll": {}, "last_state_poll": null, "prs": []
    });
    for (k, v) in extra.as_object().expect("extra is an object") {
        body[k] = v.clone();
    }
    body.to_string()
}

/// One `/status` PR row, as the daemon serializes a `StatusEntry`.
fn pr_row(key: &str, icon: &str, title: &str, summary: &str) -> Value {
    json!({
        "key": key, "icon": icon, "state": "pending", "skip_reason": null,
        "title": title, "author": "alice", "head_branch": "f/x", "is_draft": false,
        "last_verdict": null, "blocking_count": 0, "nit_count": 0,
        "last_review_at": null, "summary": summary, "last_review_age": "never"
    })
}

impl Stub {
    fn start(tag: &str) -> Stub {
        let cfg_dir = TempDir::new(&format!("thin-{tag}-cfg"));
        let data_dir = TempDir::new(&format!("thin-{tag}-data"));
        let home = TempDir::new(&format!("thin-{tag}-home"));
        let config = cfg_dir.child("config.yml");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        write_file(&data_dir.child("daemon.port"), &port.to_string());
        write_file(&data_dir.child("daemon.token"), "stub-token\n");

        let seen = Arc::new(Mutex::new(Vec::new()));
        let routes: Arc<Mutex<Vec<(String, u16, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let (log, table) = (seen.clone(), routes.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut request = Request {
                    line: line.trim().trim_end_matches(" HTTP/1.1").to_string(),
                    authorization: String::new(),
                    client: String::new(),
                    body: String::new(),
                };
                let mut length = 0usize;
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                    let Some((name, value)) = header.split_once(':') else {
                        continue;
                    };
                    let value = value.trim().to_string();
                    match name.to_ascii_lowercase().as_str() {
                        "content-length" => length = value.parse().unwrap_or(0),
                        "authorization" => request.authorization = value,
                        "x-prx-client" => request.client = value,
                        _ => {}
                    }
                }
                let mut body = vec![0u8; length];
                if reader.read_exact(&mut body).is_err() {
                    continue;
                }
                request.body = String::from_utf8_lossy(&body).into_owned();

                let scripted = table
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|(prefix, _, _)| {
                        // A route prefix matches its own path only, with or
                        // without a query (`POST /review` is not
                        // `POST /review/preflight`).
                        request
                            .line
                            .strip_prefix(prefix.as_str())
                            .is_some_and(|rest| {
                                rest.is_empty() || rest.starts_with('?') || rest.starts_with(' ')
                            })
                    })
                    .map(|(_, status, body)| (*status, body.clone()));
                let (status, reply) = match scripted {
                    Some(reply) => reply,
                    None if request.line.starts_with("GET /status") => {
                        (200, status_body(json!({})))
                    }
                    // Unscripted, the pre-flight finds nothing to confirm.
                    None if request.line == "POST /review/preflight" => (
                        200,
                        json!({ "key": "org/one#1", "warnings": [] }).to_string(),
                    ),
                    None => (404, r#"{"error":"no such route in the stub"}"#.to_string()),
                };
                log.lock().unwrap().push(request);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                );
            }
        });
        Stub {
            _cfg_dir: cfg_dir,
            data_dir,
            home,
            config,
            seen,
            routes,
        }
    }

    /// Answer requests whose line starts with `prefix` (`"POST /snooze"`).
    fn reply(&self, prefix: &str, status: u16, body: impl ToString) {
        self.routes
            .lock()
            .unwrap()
            .push((prefix.to_string(), status, body.to_string()));
    }

    /// Run the CLI with an empty PATH: a thin client needs no `gh`.
    fn run(&self, args: &[&str]) -> Output {
        let home = self.home.path().to_str().expect("temp home is utf-8");
        run_cli_isolated(
            &self.config,
            self.data_dir.path(),
            args,
            None,
            &[("HOME", home), ("TZ", "UTC")],
        )
    }

    /// Everything received so far except the discovery probes and the
    /// review pre-flights.
    fn requests(&self) -> Vec<Request> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.line != "GET /status" && r.line != "POST /review/preflight")
            .cloned()
            .collect()
    }

    /// The single non-probe request since the last call, then forget it.
    #[track_caller]
    fn take_one(&self) -> Request {
        let requests = self.requests();
        self.seen.lock().unwrap().clear();
        assert_eq!(requests.len(), 1, "{requests:?}");
        requests.into_iter().next().unwrap()
    }

    fn body_of(&self, request: &Request) -> Value {
        serde_json::from_str(&request.body).unwrap_or_else(|e| panic!("{e}: {:?}", request.body))
    }
}

#[track_caller]
fn ok_stdout(out: &Output) -> String {
    assert!(out.status.success(), "stderr: {}", stderr_of(out));
    assert!(
        !stderr_of(out).contains("daemon not running"),
        "fell back in-process: {}",
        stderr_of(out)
    );
    stdout_of(out)
}

// ------------------------------------------------------------------ status

/// `prx status` prints the daemon's rows as they come, filters by repo on
/// an exact `org/repo#` prefix, and with `--json` emits just the PR array.
/// It needs no config and leaves no state behind.
#[test]
fn status_prints_the_daemons_rows_and_filters_by_repo() {
    let stub = Stub::start("status");
    stub.reply(
        "GET /status",
        200,
        status_body(json!({
            "last_state_poll": "2026-09-05T07:08:09Z",
            "prs": [
                pr_row("org/one#1", "\u{1f195}", "Add feature", "ready for review"),
                pr_row("org/one-extra#3", "\u{2705}", "Other repo", "approved: 0 blocking, 0 nits"),
                pr_row("org/two#2", "\u{26a0}\u{fe0f}", "Fix bug", "changes requested: 1 blocking, 0 nits"),
            ]
        })),
    );

    let stdout = ok_stdout(&stub.run(&["status"]));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "{stdout}");
    assert!(lines[0].trim_start().starts_with("PR "), "{stdout}");
    assert!(lines[1].contains("org/one#1") && lines[1].contains("ready for review"));
    assert!(lines[2].contains("org/one-extra#3"));
    assert!(lines[3].contains("org/two#2") && lines[3].contains("Fix bug"));
    assert_eq!(lines[5], "last poll: 2026-09-05 07:08:09 UTC");

    // "org/one" is not a prefix match for "org/one-extra".
    let stdout = ok_stdout(&stub.run(&["status", "org/one"]));
    assert!(stdout.contains("org/one#1"), "{stdout}");
    assert!(
        !stdout.contains("org/one-extra") && !stdout.contains("org/two"),
        "{stdout}"
    );

    let out = stub.run(&["status", "org/two", "--json"]);
    let entries: Vec<prx::status::StatusEntry> =
        serde_json::from_str(&ok_stdout(&out)).expect("--json is the bare PR array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, "org/two#2");
    assert_eq!(entries[0].summary, "changes requested: 1 blocking, 0 nits");

    // Nothing tracked at all.
    stub.reply("GET /status", 200, status_body(json!({})));
    assert_eq!(ok_stdout(&stub.run(&["status"])), "No PRs tracked yet.\n");
    assert_eq!(ok_stdout(&stub.run(&["status", "--json"])), "[]\n");

    // A thin client: no config was needed and nothing was written locally.
    assert!(!stub.config.exists());
    assert!(!stub.data_dir.child("state.json").exists());
    assert!(!stub.data_dir.child("history.jsonl").exists());
}

/// The review-control header: an active pause and mode `off` are separate
/// lines above the table; an expired pause is not shown; `--json` carries
/// neither.
#[test]
fn status_shows_pause_and_mode_off_above_the_table() {
    let stub = Stub::start("status-header");
    let prs = json!([pr_row(
        "org/one#1",
        "\u{1f195}",
        "Add feature",
        "ready for review"
    )]);
    stub.reply(
        "GET /status",
        200,
        status_body(json!({
            "prs": prs, "paused_until": "2099-01-01T00:00:00Z", "review_mode": "off"
        })),
    );
    let stdout = ok_stdout(&stub.run(&["status"]));
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(
        lines[0].starts_with("reviews paused until 2099-01-01 00:00 UTC (")
            && lines[0].ends_with(" — polling continues; resume with `prx resume`"),
        "{stdout}"
    );
    assert_eq!(lines[1], "review mode: off — polling and reviews stopped");
    assert!(lines[2].trim_start().starts_with("PR "), "{stdout}");

    let json_out = ok_stdout(&stub.run(&["status", "--json"]));
    assert!(json_out.starts_with('['), "{json_out}");
    assert!(!json_out.contains("paused"), "{json_out}");

    stub.reply(
        "GET /status",
        200,
        status_body(json!({ "prs": prs, "paused_until": "2020-01-01T00:00:00Z" })),
    );
    let stdout = ok_stdout(&stub.run(&["status"]));
    assert!(stdout.trim_start().starts_with("PR "), "{stdout}");
}

/// While the config file is broken the daemon runs on its last good config
/// and says so in `/status`. `prx status` passes that on as a warning on
/// stderr, so the table and the `--json` array on stdout are unchanged.
#[test]
fn status_warns_when_the_daemon_reports_a_config_error() {
    let stub = Stub::start("status-config-error");
    let prs = json!([pr_row(
        "org/one#1",
        "\u{1f195}",
        "Add feature",
        "ready for review"
    )]);
    let healthy = status_body(json!({ "prs": prs }));
    stub.reply(
        "GET /status",
        200,
        status_body(json!({
            "prs": prs,
            "config_error": {
                "message": "defaults.poll_interval_minutes must be > 0",
                "at": "2026-09-05T07:00:00Z"
            }
        })),
    );
    let warning = "warning: the config file has errors; the daemon is using its last good \
                   config: defaults.poll_interval_minutes must be > 0";

    let out = stub.run(&["status"]);
    let table = ok_stdout(&out);
    assert!(stderr_of(&out).contains(warning), "{}", stderr_of(&out));
    assert!(!table.contains("warning"), "{table}");

    let out = stub.run(&["status", "--json"]);
    let entries: Vec<prx::status::StatusEntry> = serde_json::from_str(&ok_stdout(&out)).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(stderr_of(&out).contains(warning), "{}", stderr_of(&out));

    // Healthy again: same table, no warning.
    stub.reply("GET /status", 200, healthy);
    let out = stub.run(&["status"]);
    assert_eq!(ok_stdout(&out), table);
    assert!(!stderr_of(&out).contains("warning"), "{}", stderr_of(&out));
}

/// Polls that keep failing make the table stale: `prx status` says so on
/// stderr with the error and the fix, and stdout stays the same.
#[test]
fn status_warns_when_polls_keep_failing() {
    let stub = Stub::start("status-poll-health");
    let prs = json!([pr_row(
        "org/one#1",
        "\u{1f195}",
        "Add feature",
        "ready for review"
    )]);
    stub.reply("GET /status", 200, status_body(json!({ "prs": prs })));
    let out = stub.run(&["status"]);
    let table = ok_stdout(&out);
    assert!(!stderr_of(&out).contains("warning"), "{}", stderr_of(&out));

    stub.reply(
        "GET /status",
        200,
        status_body(json!({
            "prs": prs,
            "poll_health": {
                "org/one": {
                    "consecutive_failures": 1100,
                    "last_error": "tls: failed to verify certificate: x509: OSStatus -26276",
                    "last_error_at": "2026-11-13T10:00:00Z",
                    "failing_since": "2026-11-01T10:00:00Z",
                    "last_success": "2026-11-01T09:45:00Z",
                    "poll_interval_minutes": 15
                }
            },
            "restart_recommended": true
        })),
    );
    let out = stub.run(&["status"]);
    assert_eq!(ok_stdout(&out), table);
    let stderr = stderr_of(&out);
    assert!(
        stderr.contains(
            "warning: GitHub polls are failing for org/one (1100 in a row since 2026-11-01 10:00 UTC; last success"
        ),
        "{stderr}"
    );
    assert!(stderr.contains("x509: OSStatus -26276"), "{stderr}");
    assert!(stderr.contains("prx daemon stop"), "{stderr}");

    let out = stub.run(&["status", "--json"]);
    let entries: Vec<prx::status::StatusEntry> = serde_json::from_str(&ok_stdout(&out)).unwrap();
    assert_eq!(entries.len(), 1);
}

// --------------------------------------------------------------- mutations

/// `approve`, `snooze`, `ignore`, and `unignore` post the target as given
/// and print the daemon's note, or a default line for changed / unchanged.
#[test]
fn mutations_send_the_target_and_print_the_daemons_answer() {
    let stub = Stub::start("mutations");
    let changed = json!({ "ok": true, "changed": true });
    let unchanged = json!({ "ok": true, "changed": false });
    let noted = |changed: bool, note: &str| json!({ "ok": true, "changed": changed, "note": note });

    // approve
    stub.reply("POST /approve", 200, &changed);
    assert_eq!(
        ok_stdout(&stub.run(&["approve", "org/one#1"])),
        "approved org/one#1\n"
    );
    let request = stub.take_one();
    assert_eq!(request.line, "POST /approve");
    assert_eq!(stub.body_of(&request), json!({ "key": "org/one#1" }));
    stub.reply("POST /approve", 200, &unchanged);
    assert_eq!(
        ok_stdout(&stub.run(&["approve", "org/one#1"])),
        "org/one#1: nothing to do\n"
    );
    stub.reply(
        "POST /approve",
        200,
        noted(false, "org/one#1 is already approved"),
    );
    assert_eq!(
        ok_stdout(&stub.run(&["approve", "org/one#1"])),
        "org/one#1 is already approved\n"
    );
    stub.seen.lock().unwrap().clear();

    // ignore / unignore: the same three shapes through one printer.
    for (command, route) in [("ignore", "POST /ignore"), ("unignore", "POST /unignore")] {
        stub.reply(route, 200, &changed);
        assert_eq!(
            ok_stdout(&stub.run(&[command, "org/one#1"])),
            "org/one#1: done\n"
        );
        let request = stub.take_one();
        assert_eq!(request.line, route);
        assert_eq!(stub.body_of(&request), json!({ "key": "org/one#1" }));

        stub.reply(route, 200, &unchanged);
        assert_eq!(
            ok_stdout(&stub.run(&[command, "org/one#1"])),
            "org/one#1: nothing to do\n"
        );
        stub.reply(route, 200, noted(true, "the daemon's own words"));
        assert_eq!(
            ok_stdout(&stub.run(&[command, "org/one#1"])),
            "the daemon's own words\n"
        );
        stub.seen.lock().unwrap().clear();
    }

    // snooze: hours or until-push, exactly as flagged.
    stub.reply("POST /snooze", 200, noted(true, "org/one#1 snoozed"));
    assert_eq!(
        ok_stdout(&stub.run(&["snooze", "org/one#1", "--hours", "1.5"])),
        "org/one#1 snoozed\n"
    );
    assert_eq!(
        stub.body_of(&stub.take_one()),
        json!({ "key": "org/one#1", "hours": 1.5, "until_push": false })
    );
    ok_stdout(&stub.run(&["snooze", "org/one#1", "--until-push"]));
    assert_eq!(
        stub.body_of(&stub.take_one()),
        json!({ "key": "org/one#1", "hours": null, "until_push": true })
    );
}

/// The daemon resolves every reference form, so the client passes the
/// target through untouched instead of parsing it.
#[test]
fn targets_reach_the_daemon_unparsed() {
    let stub = Stub::start("targets");
    stub.reply("POST /ignore", 200, json!({ "ok": true, "changed": true }));
    for target in ["7", "#7", "one#7", "https://github.com/org/one/pull/7"] {
        ok_stdout(&stub.run(&["ignore", target]));
        assert_eq!(stub.body_of(&stub.take_one()), json!({ "key": target }));
    }
}

// ------------------------------------------------------- review / review-now

/// What `prx review` prints for each shape of the daemon's answer, and the
/// difference between a single target (forced) and `--all` (due PRs only).
#[test]
fn review_reports_what_was_queued() {
    let stub = Stub::start("review");
    stub.reply(
        "POST /review",
        200,
        json!({ "enqueued": ["org/one#1", "org/two#2"] }),
    );
    assert_eq!(
        ok_stdout(&stub.run(&["review", "--all"])),
        "queued org/one#1 for review (the daemon will run it)\n\
         queued org/two#2 for review (the daemon will run it)\n"
    );
    assert_eq!(
        stub.body_of(&stub.take_one()),
        json!({ "key": "all", "force": false })
    );

    stub.reply(
        "POST /review",
        200,
        json!({ "enqueued": ["org/one#1"], "note": "reviews paused; this explicit review runs anyway" }),
    );
    assert_eq!(
        ok_stdout(&stub.run(&["review", "org/one#1"])),
        "queued org/one#1 for review (the daemon will run it)\n\
         note: reviews paused; this explicit review runs anyway\n"
    );
    assert_eq!(
        stub.body_of(&stub.take_one()),
        json!({ "key": "org/one#1", "force": true })
    );

    stub.reply(
        "POST /review",
        200,
        json!({ "enqueued": [], "note": "org/one#1 is already queued" }),
    );
    assert_eq!(
        ok_stdout(&stub.run(&["review", "org/one#1"])),
        "org/one#1 is already queued\n"
    );
    stub.reply("POST /review", 200, json!({ "enqueued": [] }));
    assert_eq!(
        ok_stdout(&stub.run(&["review", "--all"])),
        "nothing was enqueued for review\n"
    );
}

/// A single-PR review asks the daemon's pre-flight first. With warnings and
/// no terminal it refuses and sends nothing; `--yes` sends it confirmed
/// without asking; `--all` never asks.
#[test]
fn review_asks_the_preflight_and_confirms_only_with_yes() {
    let stub = Stub::start("review-preflight");
    stub.reply("POST /review", 200, json!({ "enqueued": ["org/one#1"] }));
    stub.reply(
        "POST /review/preflight",
        200,
        json!({
            "key": "org/one#1",
            "warnings": [{ "kind": "skipped", "message": "Skipped \u{2014} author dependabot[bot] excluded" }],
            "size": { "files_changed": 2, "lines": 40, "max_files": 50, "max_lines": 4000 }
        }),
    );

    let out = stub.run(&["review", "org/one#1"]);
    assert_eq!(out.status.code(), Some(1), "{}", stdout_of(&out));
    let err = stderr_of(&out);
    assert!(
        err.contains("org/one#1 needs your confirmation before review"),
        "{err}"
    );
    assert!(
        err.contains("- Skipped \u{2014} author dependabot[bot] excluded"),
        "{err}"
    );
    assert!(err.contains("- Size \u{2014} 40 lines in 2 files"), "{err}");
    assert!(err.contains("rerun with --yes"), "{err}");
    let seen: Vec<String> = stub
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.line.clone())
        .collect();
    assert!(
        seen.contains(&"POST /review/preflight".to_string()),
        "{seen:?}"
    );
    assert!(stub.requests().is_empty(), "the review was sent");
    stub.seen.lock().unwrap().clear();

    assert_eq!(
        ok_stdout(&stub.run(&["review", "org/one#1", "--yes"])),
        "queued org/one#1 for review (the daemon will run it)\n"
    );
    let lines: Vec<String> = stub
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.line.clone())
        .collect();
    assert!(
        !lines.contains(&"POST /review/preflight".to_string()),
        "{lines:?}"
    );
    assert_eq!(
        stub.body_of(&stub.take_one()),
        json!({ "key": "org/one#1", "force": true, "confirm": true })
    );

    ok_stdout(&stub.run(&["review", "--all"]));
    let lines: Vec<String> = stub
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.line.clone())
        .collect();
    assert!(
        !lines.contains(&"POST /review/preflight".to_string()),
        "{lines:?}"
    );
}

/// `prx review-now` confirms the trigger, or relays the daemon's note when
/// reviews are paused (the poll still ran, nothing was enqueued).
#[test]
fn review_now_confirms_or_relays_the_pause() {
    let stub = Stub::start("review-now");
    stub.reply(
        "POST /review-now",
        200,
        json!({ "ok": true, "changed": true }),
    );
    assert_eq!(
        ok_stdout(&stub.run(&["review-now"])),
        "triggered poll + due-scan on the daemon\n"
    );
    let request = stub.take_one();
    assert_eq!(
        (request.line.as_str(), request.body.as_str()),
        ("POST /review-now", "")
    );

    let paused = "reviews paused until 2099-01-01 00:00 UTC; polled, nothing enqueued";
    stub.reply(
        "POST /review-now",
        200,
        json!({ "ok": true, "changed": false, "note": paused }),
    );
    assert_eq!(ok_stdout(&stub.run(&["review-now"])), format!("{paused}\n"));
}

// ---------------------------------------------------------- pause / resume

/// `prx pause` sends the duration or the parsed end time and prints the end
/// the daemon confirmed; `prx resume` says whether there was a pause.
#[test]
fn pause_and_resume_report_the_daemons_view() {
    let stub = Stub::start("pause");
    stub.reply(
        "POST /pause",
        200,
        json!({ "ok": true, "changed": true, "paused_until": "2099-06-01T12:00:00Z" }),
    );
    let stdout = ok_stdout(&stub.run(&["pause", "--hours", "2"]));
    assert!(
        stdout.starts_with("reviews paused until 2099-06-01 12:00 UTC (")
            && stdout.ends_with(" — polling continues; `prx resume` to lift it\n"),
        "{stdout}"
    );
    assert_eq!(stub.body_of(&stub.take_one()), json!({ "hours": 2.0 }));

    ok_stdout(&stub.run(&["pause", "--until", "2099-06-01T12:00:00+02:00"]));
    assert_eq!(
        stub.body_of(&stub.take_one()),
        json!({ "until": "2099-06-01T10:00:00Z" })
    );

    // Invalid arguments never reach the daemon.
    for args in [&["pause"][..], &["pause", "--hours", "0"]] {
        let out = stub.run(args);
        assert_eq!(out.status.code(), Some(1), "{args:?}");
    }
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());

    stub.reply("POST /resume", 200, json!({ "ok": true, "changed": true }));
    assert_eq!(ok_stdout(&stub.run(&["resume"])), "reviews resumed\n");
    assert_eq!(stub.take_one().line, "POST /resume");
    stub.reply("POST /resume", 200, json!({ "ok": true, "changed": false }));
    assert_eq!(
        ok_stdout(&stub.run(&["resume"])),
        "reviews were not paused\n"
    );
    // The pause lives in the daemon: the client wrote no state of its own.
    assert!(!stub.data_dir.child("state.json").exists());
}

// ------------------------------------------------- history / insights / usage

/// `prx history` asks for `--limit` entries (default 20) of `--repo`, and
/// prints the daemon's entries as the timeline or, with `--json`, as given.
#[test]
fn history_queries_the_daemon_with_limit_and_repo() {
    let stub = Stub::start("history");
    let entries = json!({ "entries": [
        { "event": "pr_merged", "ts": "2026-09-02T10:00:00Z", "repo": "org/one",
          "number": 1, "title": "Add feature", "author": "alice" },
        { "event": "review_completed", "ts": "2026-09-01T10:00:00Z", "repo": "org/one",
          "number": 1, "title": "Add feature", "author": "alice", "agent": "claude",
          "verdict": "approved", "blocking_count": 0, "nit_count": 2, "posted": false }
    ]});
    stub.reply("GET /history", 200, &entries);

    let stdout = ok_stdout(&stub.run(&["history"]));
    assert_eq!(
        stdout.lines().collect::<Vec<_>>(),
        [
            "recent review history (newest first):",
            "  2026-09-02 10:00 \u{2714}\u{fe0f} org/one#1        merged · Add feature",
            "  2026-09-01 10:00 \u{2705} org/one#1         approved (not posted) — 0 blocking, 2 nits · claude · Add feature",
        ]
    );
    assert_eq!(stub.take_one().line, "GET /history?limit=20");

    let out = stub.run(&["history", "--repo", "org/one", "--limit", "5", "--json"]);
    let printed: Value = serde_json::from_str(&ok_stdout(&out)).unwrap();
    assert_eq!(printed, entries["entries"]);
    assert_eq!(stub.take_one().line, "GET /history?limit=5&repo=org/one");

    // A repo name is percent-encoded where it has to be.
    ok_stdout(&stub.run(&["history", "--repo", "org/a b&c=d"]));
    assert_eq!(
        stub.take_one().line,
        "GET /history?limit=20&repo=org/a%20b%26c%3Dd"
    );

    stub.reply("GET /history", 200, json!({ "entries": [] }));
    assert_eq!(ok_stdout(&stub.run(&["history"])), "no history yet\n");
}

/// `prx insights` passes `--repo` / `--days` on and renders the daemon's
/// report; `--days 0` is refused before any request.
#[test]
fn insights_passes_filters_and_renders_the_daemons_report() {
    let stub = Stub::start("insights");
    let now = "2026-09-30T12:00:00Z".parse().unwrap();
    let report = prx::insights::compute(&[], now, Some(14));
    stub.reply(
        "GET /insights",
        200,
        serde_json::to_string(&report).unwrap(),
    );

    let stdout = ok_stdout(&stub.run(&["insights", "--repo", "org/one", "--days", "14"]));
    assert_eq!(stdout, prx::insights::render(&report));
    assert!(
        stdout.starts_with("prx insights (last 14 days · generated 2026-09-30 12:00 UTC)\n"),
        "{stdout}"
    );
    assert_eq!(stub.take_one().line, "GET /insights?repo=org/one&days=14");

    ok_stdout(&stub.run(&["insights"]));
    assert_eq!(stub.take_one().line, "GET /insights");
    ok_stdout(&stub.run(&["insights", "--days", "7"]));
    assert_eq!(stub.take_one().line, "GET /insights?days=7");

    let out = stub.run(&["insights", "--days", "0"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).contains("error: --days must be at least 1"),
        "{}",
        stderr_of(&out)
    );
    assert!(stub.requests().is_empty(), "{:?}", stub.requests());
}

/// `prx usage` maps its flags onto the `/usage` query (defaults included)
/// and prints the daemon's report as the table or, with `--json`, as given.
#[test]
fn usage_maps_flags_onto_the_query() {
    let stub = Stub::start("usage");
    let now = "2026-09-30T12:00:00Z".parse().unwrap();
    let query = prx::usage::Query::parse(Some("month"), Some("90d"), Some("repo"), None).unwrap();
    let report = prx::usage::compute_in(&[], now, &query, &chrono::Utc);
    stub.reply("GET /usage", 200, serde_json::to_string(&report).unwrap());

    assert_eq!(
        ok_stdout(&stub.run(&["usage"])),
        prx::usage::render(&report)
    );
    assert_eq!(
        stub.take_one().line,
        "GET /usage?bucket=day&window=30d&group=none"
    );

    let out = stub.run(&[
        "usage", "--by", "month", "--window", "90d", "--group", "repo", "--repo", "org/one",
        "--json",
    ]);
    let printed: prx::usage::Usage = serde_json::from_str(&ok_stdout(&out)).unwrap();
    assert_eq!(printed, report);
    assert_eq!(
        stub.take_one().line,
        "GET /usage?bucket=month&window=90d&group=repo&repo=org/one"
    );

    // A blank --repo is no filter.
    ok_stdout(&stub.run(&["usage", "--repo", "  "]));
    assert_eq!(
        stub.take_one().line,
        "GET /usage?bucket=day&window=30d&group=none"
    );
}

// ------------------------------------------------------- errors / discovery

/// A daemon that refuses a request fails the command with exit code 1 and
/// the daemon's reason; nothing is retried in-process behind its back.
#[test]
fn daemon_errors_fail_the_command_with_the_daemons_reason() {
    let stub = Stub::start("errors");
    stub.reply(
        "POST /snooze",
        422,
        json!({ "error": "org/one#9 is not tracked" }),
    );
    let out = stub.run(&["snooze", "org/one#9", "--hours", "1"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_of(&out), "");
    assert!(
        stderr_of(&out)
            .contains("error: daemon returned 422 Unprocessable Entity: org/one#9 is not tracked"),
        "{}",
        stderr_of(&out)
    );
    assert!(!stderr_of(&out).contains("running in-process"));

    // An error without the JSON envelope still names the status.
    stub.reply("POST /ignore", 500, "boom");
    let out = stub.run(&["ignore", "org/one#1"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).contains("daemon returned 500 Internal Server Error"),
        "{}",
        stderr_of(&out)
    );

    // A success status with a body that is not the expected shape.
    stub.reply("GET /history", 200, r#"{"unexpected":true}"#);
    let out = stub.run(&["history"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).contains("invalid daemon response"),
        "{}",
        stderr_of(&out)
    );
    assert_eq!(stdout_of(&out), "");

    // A route the daemon does not have (an older daemon).
    let out = stub.run(&["usage"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr_of(&out).contains("daemon returned 404 Not Found: no such route in the stub"),
        "{}",
        stderr_of(&out)
    );
}

/// Every request, the discovery probe included, carries the bearer token
/// from `daemon.token` (surrounding whitespace trimmed).
#[test]
fn requests_carry_the_bearer_token_from_the_token_file() {
    let stub = Stub::start("token");
    stub.reply("GET /history", 200, json!({ "entries": [] }));
    ok_stdout(&stub.run(&["history"]));
    let seen = stub.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].line, "GET /status");
    for request in &seen {
        assert_eq!(request.authorization, "Bearer stub-token", "{request:?}");
    }
}

/// The CLI must not present itself as the menu bar app: the daemon holds
/// back its own desktop banners while a menubar client was seen recently,
/// so a `prx status` from a terminal would silence notifications on a
/// machine where no app is running to show them.
#[test]
fn cli_requests_do_not_identify_as_the_menubar() {
    let stub = Stub::start("client-id");
    stub.reply("GET /history", 200, json!({ "entries": [] }));
    ok_stdout(&stub.run(&["history"]));
    for request in stub.seen.lock().unwrap().iter() {
        assert!(
            !request.client.starts_with("menubar"),
            "{} sent X-Prx-Client: {}",
            request.line,
            request.client
        );
    }
}

/// No usable daemon means the in-process path, announced on stderr: a port
/// file nobody listens on (a crashed daemon's leftovers), a port file that
/// is not a number, and a listener that rejects the token.
#[test]
fn an_unreachable_or_unusable_daemon_falls_back_in_process() {
    let fallback = "daemon not running — reading history in-process";

    // Leftover files pointing at a port that was just closed.
    let cfg = TempDir::new("thin-stale-cfg");
    let data = TempDir::new("thin-stale-data");
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    write_file(&data.child("daemon.port"), &closed.to_string());
    write_file(&data.child("daemon.token"), "stale");
    let config = cfg.child("config.yml");
    let out = run_cli_isolated(&config, data.path(), &["history"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains(fallback), "{}", stderr_of(&out));
    assert_eq!(stdout_of(&out), "no history yet\n");

    write_file(&data.child("daemon.port"), "not-a-port");
    let out = run_cli_isolated(&config, data.path(), &["history"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains(fallback), "{}", stderr_of(&out));

    // Something answers, but not as our daemon: 401 on the probe.
    let stub = Stub::start("unauthorized");
    stub.reply("GET /status", 401, json!({ "error": "unauthorized" }));
    let out = stub.run(&["history"]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(stderr_of(&out).contains(fallback), "{}", stderr_of(&out));
    assert!(stub.requests().is_empty(), "only the probe was sent");

    // And a 200 that is not a status payload is not our daemon either.
    stub.reply("GET /status", 200, r#"{"hello":"world"}"#);
    let out = stub.run(&["history"]);
    assert!(stderr_of(&out).contains(fallback), "{}", stderr_of(&out));
}

/// A dry run makes no writes, so it never involves the daemon: with one
/// answering, `prx review --dry-run` does not even probe it and goes down
/// the local path (which here stops at the missing config).
#[test]
fn dry_run_review_never_contacts_the_daemon() {
    let stub = Stub::start("dry-run");
    stub.reply("POST /review", 200, json!({ "enqueued": ["org/one#1"] }));

    let out = stub.run(&["review", "org/one#1", "--dry-run"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr_of(&out));
    assert!(
        stderr_of(&out).contains("run `prx init` first"),
        "{}",
        stderr_of(&out)
    );
    assert!(!stdout_of(&out).contains("queued"), "{}", stdout_of(&out));
    assert!(
        stub.seen.lock().unwrap().is_empty(),
        "{:?}",
        stub.seen.lock().unwrap()
    );

    // The same command without --dry-run is handed to the daemon.
    let stdout = ok_stdout(&stub.run(&["review", "org/one#1"]));
    assert_eq!(
        stdout,
        "queued org/one#1 for review (the daemon will run it)\n"
    );
}
