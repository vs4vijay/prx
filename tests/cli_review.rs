//! `prx review` against a stub daemon (task 5.1, design D10): the CLI
//! passes the target through unparsed and maps `--engine` / `--no-post` /
//! `--prompt` onto the `/review` request's per-run options.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use common::*;

/// A pre-flight answer with nothing to confirm.
const PREFLIGHT_CLEAR: &str = r#"{"key":"org/one#42","warnings":[]}"#;

/// A minimal HTTP/1.1 stand-in for the daemon: answers `GET /status` with an
/// empty status payload, `POST /approve` with a changed mutation,
/// `POST /review/preflight` with nothing to confirm, and `POST /review` with
/// one enqueued key, recording every request line and body.
fn stub_daemon(data_dir: &std::path::Path) -> Arc<Mutex<Vec<(String, String)>>> {
    stub_daemon_with(data_dir, Some(PREFLIGHT_CLEAR))
}

/// [`stub_daemon`] with this `POST /review/preflight` answer; `None` is an
/// older daemon without the route (a bare 404).
fn stub_daemon_with(
    data_dir: &std::path::Path,
    preflight: Option<&'static str>,
) -> Arc<Mutex<Vec<(String, String)>>> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    write_file(&data_dir.join("daemon.port"), &port.to_string());
    write_file(&data_dir.join("daemon.token"), "stub-token");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            let request_line = request_line.trim().to_string();
            let mut status = "200 OK";
            let reply = if request_line.starts_with("POST /review/preflight") {
                preflight.unwrap_or_else(|| {
                    status = "404 Not Found";
                    ""
                })
            } else if request_line.starts_with("GET /status") {
                r#"{"version":"0","pid":1,"uptime_seconds":1,"in_flight":0,"queue_depth":0,"last_poll":{},"last_state_poll":null,"prs":[]}"#
            } else if request_line.starts_with("POST /approve") {
                r#"{"ok":true,"changed":true}"#
            } else {
                r#"{"enqueued":["org/one#42"]}"#
            };
            log.lock()
                .unwrap()
                .push((request_line, String::from_utf8_lossy(&body).to_string()));
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    seen
}

#[test]
fn review_passes_the_target_and_options_to_the_daemon() {
    let cfg = TempDir::new("cli-review-cfg");
    let data = TempDir::new("cli-review-data");
    let config = cfg.child("config.yml");
    write_file(&config, &config_with(&["org/one"], ""));
    let seen = stub_daemon(data.path());

    let out = run_cli(
        &config,
        data.path(),
        &["review", "42", "--no-post", "--engine", "claude"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    assert!(
        stdout_of(&out).contains("queued org/one#42 for review"),
        "{}",
        stdout_of(&out)
    );
    let seen = seen.lock().unwrap().clone();
    let (line, body) = seen
        .iter()
        .find(|(line, _)| line.starts_with("POST /review "))
        .unwrap_or_else(|| panic!("no /review request: {seen:?}"));
    assert!(line.starts_with("POST /review "), "{line}");
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "key": "42", "force": true, "engine": "claude", "post": false })
    );
}

#[test]
fn review_prompt_flag_is_sent_and_plain_requests_carry_no_options() {
    let cfg = TempDir::new("cli-review-prompt-cfg");
    let data = TempDir::new("cli-review-prompt-data");
    let config = cfg.child("config.yml");
    write_file(&config, &config_with(&["org/one"], ""));
    let seen = stub_daemon(data.path());

    for args in [
        &["review", "one#42", "--prompt", " focus on auth "][..],
        &["review", "org/one#42"],
    ] {
        let out = run_cli(&config, data.path(), args, None, &[]);
        assert!(out.status.success(), "{}", stderr_of(&out));
    }
    let bodies: Vec<serde_json::Value> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(line, _)| line.starts_with("POST /review "))
        .map(|(_, body)| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(
        bodies,
        [
            serde_json::json!({ "key": "one#42", "force": true, "extra_prompt": "focus on auth" }),
            serde_json::json!({ "key": "org/one#42", "force": true }),
        ]
    );
}

/// Invariant 6: with the daemon up, `prx review` and `prx approve` are thin
/// clients. The CLI process itself runs no `gh` (so it posts nothing and
/// approves nothing) and no engine; it only asks the daemon.
#[test]
fn with_a_daemon_the_cli_runs_no_gh_and_no_engine_itself() {
    let cfg = TempDir::new("cli-review-thin-cfg");
    let data = TempDir::new("cli-review-thin-data");
    let gh_dir = TempDir::new("cli-review-thin-gh");
    let agent_dir = TempDir::new("cli-review-thin-agent");
    install_fake_gh(gh_dir.path());
    let agent = install_fake_agent(agent_dir.path());
    let config = cfg.child("config.yml");
    write_file(&config, &config_with_fake_agent(&["org/one"], &agent, ""));
    write_list_fixture(
        gh_dir.path(),
        "org/one",
        &[pr_json(
            42,
            "Thin client",
            "alice",
            "f/x",
            "aaaa",
            false,
            "2026-09-01T10:00:00Z",
        )],
    );
    let seen = stub_daemon(data.path());
    let runs_log = agent_dir.child("runs.log");
    let manifest = manifest_json("approved", &[]);
    let env = [
        ("FAKE_AGENT_MANIFEST_JSON", manifest.as_str()),
        ("FAKE_AGENT_RUNS_LOG", runs_log.to_str().unwrap()),
    ];

    for args in [
        &["review", "org/one#42"][..],
        &["review", "--all"],
        &["approve", "org/one#42"],
    ] {
        let out = run_cli(&config, data.path(), args, Some(gh_dir.path()), &env);
        assert!(out.status.success(), "{args:?}: {}", stderr_of(&out));
        assert!(
            !stderr_of(&out).contains("running in-process"),
            "{args:?}: {}",
            stderr_of(&out)
        );
    }
    assert!(
        !gh_dir.child("calls.log").exists(),
        "the CLI ran gh: {}",
        std::fs::read_to_string(gh_dir.child("calls.log")).unwrap_or_default()
    );
    assert!(!runs_log.exists(), "the CLI ran the engine");
    assert!(!data.child("state.json").exists());
    assert!(!data.child("history.jsonl").exists());

    let posts: Vec<(String, serde_json::Value)> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(line, _)| line.starts_with("POST ") && !line.contains("/review/preflight"))
        .map(|(line, body)| {
            let path = line.split(' ').nth(1).unwrap().to_string();
            (path, serde_json::from_str(body).unwrap())
        })
        .collect();
    assert_eq!(posts.len(), 3, "{posts:?}");
    assert_eq!(posts[0].0, "/review");
    assert_eq!(posts[1].0, "/review");
    assert_eq!(posts[0].1["key"], "org/one#42");
    // An approval is requested by the human's command only, for that PR only.
    assert_eq!(
        posts[2],
        (
            "/approve".to_string(),
            serde_json::json!({ "key": "org/one#42" })
        )
    );
}

/// `--dry-run` never needs the daemon and never asks it for anything: a
/// dry run must not be able to enqueue a real review.
#[test]
fn dry_run_never_contacts_the_daemon() {
    let cfg = TempDir::new("cli-review-dry-cfg");
    let data = TempDir::new("cli-review-dry-data");
    let config = cfg.child("config.yml");
    write_file(&config, &config_with(&["org/one"], ""));
    let seen = stub_daemon(data.path());

    let out = run_cli(
        &config,
        data.path(),
        &["review", "org/one#42", "--dry-run"],
        None,
        &[],
    );
    // Nothing is tracked in this sandbox, and a dry run does not poll.
    assert!(!out.status.success());
    assert!(
        stderr_of(&out).contains("org/one#42 is not tracked yet"),
        "{}",
        stderr_of(&out)
    );
    assert!(
        seen.lock().unwrap().is_empty(),
        "{:?}",
        seen.lock().unwrap()
    );
}

/// The target and `--all` are mutually exclusive and one is required; the
/// mistake is reported before anything is asked of the daemon.
#[test]
fn review_argument_errors_do_not_reach_the_daemon() {
    let cfg = TempDir::new("cli-review-args-cfg");
    let data = TempDir::new("cli-review-args-data");
    let config = cfg.child("config.yml");
    write_file(&config, &config_with(&["org/one"], ""));
    let seen = stub_daemon(data.path());

    for (args, message) in [
        (
            &["review"][..],
            "specify a PR target (org/repo#123) or --all",
        ),
        (
            &["review", "org/one#42", "--all"][..],
            "specify either a PR target or --all, not both",
        ),
    ] {
        let out = run_cli(&config, data.path(), args, None, &[]);
        assert!(!out.status.success(), "{args:?}");
        assert!(
            stderr_of(&out).contains(message),
            "{args:?}: {}",
            stderr_of(&out)
        );
    }
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|(line, _)| line.starts_with("POST ")),
        "{:?}",
        seen.lock().unwrap()
    );
}

/// The pre-flight's warnings without a terminal: the CLI refuses, names the
/// warnings and `--yes`, and never sends the review.
#[test]
fn a_risky_review_without_a_terminal_is_refused_and_not_sent() {
    let cfg = TempDir::new("cli-review-risky-cfg");
    let data = TempDir::new("cli-review-risky-data");
    let config = cfg.child("config.yml");
    write_file(&config, &config_with(&["org/one"], ""));
    let seen = stub_daemon_with(
        data.path(),
        Some(
            r#"{"key":"org/one#42","warnings":[{"kind":"skipped","message":"Skipped \u2014 draft"},{"kind":"too_large","message":"Large PR \u2014 2,140 lines changed (limit 1,500)"}],"size":{"files_changed":3,"lines":2140,"max_files":50,"max_lines":1500}}"#,
        ),
    );

    let out = run_cli(&config, data.path(), &["review", "42"], None, &[]);
    assert!(!out.status.success(), "{}", stdout_of(&out));
    let err = stderr_of(&out);
    assert!(err.contains("Skipped \u{2014} draft"), "{err}");
    assert!(
        err.contains("Large PR \u{2014} 2,140 lines changed (limit 1,500)"),
        "{err}"
    );
    assert!(err.contains("--yes"), "{err}");
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .any(|(line, _)| line.starts_with("POST /review/preflight")),
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|(line, _)| line.starts_with("POST /review ")),
        "the review was sent: {seen:?}"
    );
}

/// `--yes` skips the pre-flight and sends the review confirmed; an older
/// daemon without the route is asked nothing more and gets the plain
/// request.
#[test]
fn yes_confirms_and_an_older_daemon_gets_the_plain_request() {
    let cfg = TempDir::new("cli-review-yes-cfg");
    let data = TempDir::new("cli-review-yes-data");
    let config = cfg.child("config.yml");
    write_file(&config, &config_with(&["org/one"], ""));
    let seen = stub_daemon_with(data.path(), None);

    for args in [&["review", "42", "-y"][..], &["review", "42"]] {
        let out = run_cli(&config, data.path(), args, None, &[]);
        assert!(out.status.success(), "{args:?}: {}", stderr_of(&out));
    }
    let seen = seen.lock().unwrap().clone();
    let preflights = seen
        .iter()
        .filter(|(line, _)| line.starts_with("POST /review/preflight"))
        .count();
    assert_eq!(preflights, 1, "only the run without --yes asks: {seen:?}");
    let bodies: Vec<serde_json::Value> = seen
        .iter()
        .filter(|(line, _)| line.starts_with("POST /review "))
        .map(|(_, body)| serde_json::from_str(body).unwrap())
        .collect();
    assert_eq!(
        bodies,
        [
            serde_json::json!({ "key": "42", "force": true, "confirm": true }),
            serde_json::json!({ "key": "42", "force": true }),
        ]
    );
}
