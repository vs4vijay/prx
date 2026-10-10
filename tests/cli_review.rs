//! `prx review` against a stub daemon (task 5.1, design D10): the CLI
//! passes the target through unparsed and maps `--engine` / `--no-post` /
//! `--prompt` onto the `/review` request's per-run options.

mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use common::*;

/// A minimal HTTP/1.1 stand-in for the daemon: answers `GET /status` with an
/// empty status payload and `POST /review` with one enqueued key, recording
/// every request line and body.
fn stub_daemon(data_dir: &std::path::Path) -> Arc<Mutex<Vec<(String, String)>>> {
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
            let reply = if request_line.starts_with("GET /status") {
                r#"{"version":"0","pid":1,"uptime_seconds":1,"in_flight":0,"queue_depth":0,"last_poll":{},"last_state_poll":null,"prs":[]}"#
            } else {
                r#"{"enqueued":["org/one#42"]}"#
            };
            log.lock()
                .unwrap()
                .push((request_line, String::from_utf8_lossy(&body).to_string()));
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
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
        .find(|(line, _)| line.starts_with("POST /review"))
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
        .filter(|(line, _)| line.starts_with("POST /review"))
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
