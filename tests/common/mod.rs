//! Shared harness for integration tests: temp dirs, CLI invocation with
//! PRX_CONFIG / PRX_DATA_DIR overrides, and the fake `gh` fixture install.

#![allow(dead_code)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A unique temp directory removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("prx-it-{tag}-{}-{nanos}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn child(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_prx")
}

/// Run the prx binary with redirected config/data dirs.
/// `path_prepend` puts a directory first on PATH (for the fake gh) and wires
/// FAKE_GH_DIR / FAKE_GH_LOG to it so the fixture resolves its canned
/// responses and records every invocation to `<dir>/calls.log`.
pub fn run_cli(
    config: &Path,
    data: &Path,
    args: &[&str],
    path_prepend: Option<&Path>,
    extra_env: &[(&str, &str)],
) -> Output {
    let path = path_prepend.map(|dir| match std::env::var("PATH") {
        Ok(old) => format!("{}:{old}", dir.display()),
        Err(_) => dir.display().to_string(),
    });
    spawn_cli(config, data, args, path.as_deref(), path_prepend, extra_env)
}

/// Run the prx binary with a fully stubbed PATH: the only entries are
/// directories this harness controls — the fake gh dir (if any) plus a fresh
/// empty temp dir. Nothing from the host PATH leaks in, so tests that assert
/// the ABSENCE of a binary (missing `gh`, missing agent command) stay
/// deterministic on machines where those binaries are installed. Note the
/// fixtures use `#!/bin/sh` shebangs and shell builtins only on the paths
/// these tests exercise, so they run fine without a host PATH.
pub fn run_cli_isolated(
    config: &Path,
    data: &Path,
    args: &[&str],
    fake_gh_dir: Option<&Path>,
    extra_env: &[(&str, &str)],
) -> Output {
    let empty = TempDir::new("isolated-path");
    let path = match fake_gh_dir {
        Some(dir) => format!("{}:{}", dir.display(), empty.path().display()),
        None => empty.path().display().to_string(),
    };
    spawn_cli(config, data, args, Some(&path), fake_gh_dir, extra_env)
}

fn spawn_cli(
    config: &Path,
    data: &Path,
    args: &[&str],
    path: Option<&str>,
    fake_gh_dir: Option<&Path>,
    extra_env: &[(&str, &str)],
) -> Output {
    let mut cmd = Command::new(bin());
    cmd.args(args)
        .env("PRX_CONFIG", config)
        .env("PRX_DATA_DIR", data);
    if let Some(path) = path {
        cmd.env("PATH", path);
    }
    if let Some(dir) = fake_gh_dir {
        cmd.env("FAKE_GH_DIR", dir);
        cmd.env("FAKE_GH_LOG", dir.join("calls.log"));
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to spawn prx binary")
}

pub fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Copy the fake gh fixture into `dir` and make it executable (the exec bit
/// is not relied on from the source tree).
pub fn install_fake_gh(dir: &Path) -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest)
        .join("tests")
        .join("fixtures")
        .join("gh");
    let dest = dir.join("gh");
    fs::copy(&src, &dest).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    dest
}

/// Copy the fake agent fixture into `dir` (executable); the returned path is
/// what the prx config's `agents:` entry should point at.
pub fn install_fake_agent(dir: &Path) -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let src = Path::new(manifest)
        .join("tests")
        .join("fixtures")
        .join("fake-agent");
    let dest = dir.join("fake-agent");
    fs::copy(&src, &dest).unwrap();
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();
    dest
}

pub fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// A minimal-but-explicit config listing `repos`.
pub fn config_with(repos: &[&str], extra: &str) -> String {
    let mut out = String::from(
        "defaults:\n  agent: pi\n  poll_interval_minutes: 15\n  cooldown:\n    before_first_review_minutes: 60\n    between_re_reviews_minutes: 30\n",
    );
    out.push_str(extra);
    out.push_str("repos:\n");
    for repo in repos {
        out.push_str(&format!("  - name: {repo}\n"));
    }
    out
}

/// A config using the fake agent for every repo. `agent_path` is the
/// fake-agent fixture path; `extra` splices additional keys into the
/// `defaults:` block (e.g. "  agent_timeout_seconds: 1\n").
pub fn config_with_fake_agent(repos: &[&str], agent_path: &Path, extra: &str) -> String {
    let mut out = format!(
        "defaults:\n  agent: fake\n  poll_interval_minutes: 15\n  cooldown:\n    before_first_review_minutes: 60\n    between_re_reviews_minutes: 30\n{extra}agents:\n  fake:\n    command: {}\n    args: [\"{{{{PROMPT}}}}\"]\n",
        agent_path.display()
    );
    out.push_str("repos:\n");
    for repo in repos {
        out.push_str(&format!("  - name: {repo}\n"));
    }
    out
}

/// A canned review manifest with the given verdict and counts.
pub fn manifest_json(verdict: &str, findings: &[&str]) -> String {
    format!(
        r#"{{"manifest_version": 1, "verdict": "{verdict}", "summary": "fixture summary",
  "findings": [{}], "duration_seconds": 42}}"#,
        findings.join(",\n")
    )
}

/// One finding JSON blob for [`manifest_json`].
pub fn finding_json(file: Option<&str>, line: Option<u64>, severity: &str, body: &str) -> String {
    let file_part = match file {
        Some(f) => format!(r#""file": "{f}", "#),
        None => String::new(),
    };
    let line_part = match line {
        Some(l) => format!(r#""line": {l}, "#),
        None => String::new(),
    };
    format!(r#"{{ {file_part}{line_part}"severity": "{severity}", "body": "{body}" }}"#)
}

/// Fixture JSON for `gh pr list`: one PR with the given properties.
#[allow(clippy::too_many_arguments)]
pub fn pr_json(
    number: u64,
    title: &str,
    author: &str,
    branch: &str,
    sha: &str,
    is_draft: bool,
    created_at: &str,
) -> String {
    format!(
        r#"  {{
    "headRefOid": "{sha}",
    "url": "https://github.com/org/repo/pull/{number}",
    "headRefName": "{branch}",
    "number": {number},
    "isDraft": {is_draft},
    "author": {{"login": "{author}"}},
    "createdAt": "{created_at}",
    "title": "{title}"
  }}"#
    )
}

/// Write a `list_<org>_<repo>.json` fixture with the given PR JSON entries.
pub fn write_list_fixture(fake_gh_dir: &Path, repo: &str, entries: &[String]) {
    let (org, name) = repo.split_once('/').expect("repo must be org/name");
    let body = entries.join(",\n");
    let content = if entries.is_empty() {
        "[]".to_string()
    } else {
        format!("[\n{body}\n]")
    };
    write_file(
        &fake_gh_dir.join(format!("list_{org}_{name}.json")),
        &content,
    );
}

/// Write a `<org>_<repo>#<n>.pr.json` fixture: the single-PR view used for
/// on-demand tracking (`gh pr view <n> --json <list fields>,state`). `entry`
/// is a [`pr_json`] object; the fake splices `state` in from `<n>.state`.
pub fn write_pr_fixture(fake_gh_dir: &Path, repo: &str, number: u64, entry: &str) {
    let (org, name) = repo.split_once('/').expect("repo must be org/name");
    write_file(
        &fake_gh_dir.join(format!("{org}_{name}#{number}.pr.json")),
        entry,
    );
}

pub const LIST_ARGS: &str =
    "pr list -R {repo} --json number,title,author,headRefName,headRefOid,isDraft,createdAt,url --state open --limit 200";
