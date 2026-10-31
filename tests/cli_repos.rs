//! `prx repos add/list/remove` round-trips through the real binary.

mod common;

use std::fs;

use common::*;

#[test]
fn repos_add_list_remove_roundtrip() {
    let cfg_dir = TempDir::new("repos-cfg");
    let data_dir = TempDir::new("repos-data");
    let config = cfg_dir.child("config.yml");

    // prx init creates the starter config so repos commands have a file.
    let out = run_cli(&config, data_dir.path(), &["init"], None, &[]);
    assert!(out.status.success(), "init failed: {}", stderr_of(&out));

    // List starts empty.
    let out = run_cli(&config, data_dir.path(), &["repos", "list"], None, &[]);
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("no repos configured"));

    // Add.
    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "add", "org/one"],
        None,
        &[],
    );
    assert!(out.status.success(), "add failed: {}", stderr_of(&out));
    assert!(stdout_of(&out).contains("added org/one"));

    let on_disk = fs::read_to_string(&config).unwrap();
    assert!(on_disk.contains("org/one"));

    // List shows it.
    let out = run_cli(&config, data_dir.path(), &["repos", "list"], None, &[]);
    assert!(out.status.success());
    let stdout = stdout_of(&out);
    assert!(stdout.contains("org/one"), "list output: {stdout}");
    assert!(stdout.contains("pi")); // effective agent shown

    // Add is idempotent.
    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "add", "org/one"],
        None,
        &[],
    );
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("already being watched"));
    let on_disk = fs::read_to_string(&config).unwrap();
    assert_eq!(on_disk.matches("org/one").count(), 1);

    // Add a second repo, then remove the first.
    run_cli(
        &config,
        data_dir.path(),
        &["repos", "add", "org/two"],
        None,
        &[],
    );
    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "remove", "org/one"],
        None,
        &[],
    );
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("removed org/one"));

    let on_disk = fs::read_to_string(&config).unwrap();
    assert!(!on_disk.contains("org/one"));
    assert!(on_disk.contains("org/two"));

    // Removing a missing repo errors.
    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "remove", "org/one"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert!(
        stdout_of(&out).contains("not being watched")
            || stderr_of(&out).contains("not being watched")
    );
}

#[test]
fn repos_add_rejects_invalid_names() {
    let cfg_dir = TempDir::new("repos-invalid");
    let data_dir = TempDir::new("repos-invalid-data");
    let config = cfg_dir.child("config.yml");
    run_cli(&config, data_dir.path(), &["init"], None, &[]);

    for bad in ["justname", "org/", "/repo", "a/b/c", "has space/x", ""] {
        let out = run_cli(&config, data_dir.path(), &["repos", "add", bad], None, &[]);
        assert!(!out.status.success(), "'{bad}' should be rejected");
        assert!(stderr_of(&out).contains("invalid repo name"), "for '{bad}'");
    }
    // Nothing was added (the starter config's commented example must not
    // count — check the parsed config, not raw text).
    let parsed = prx::config::Config::load(&config).unwrap();
    assert!(parsed.repos.is_empty());
}

#[test]
fn repos_commands_require_existing_config() {
    let cfg_dir = TempDir::new("repos-noconf");
    let data_dir = TempDir::new("repos-noconf-data");
    let config = cfg_dir.child("missing.yml");

    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "add", "org/one"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert!(stderr_of(&out).contains("prx init"));

    let out = run_cli(&config, data_dir.path(), &["repos", "list"], None, &[]);
    assert!(!out.status.success());

    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "remove", "org/one"],
        None,
        &[],
    );
    assert!(!out.status.success());
}

/// Adding and removing a repo leaves every other repo, its overrides, and
/// the defaults as they were — and needs neither `gh` nor an engine: the
/// commands edit the config and call nothing (PATH is empty here).
#[test]
fn repos_add_and_remove_keep_the_rest_of_the_config() {
    let cfg_dir = TempDir::new("repos-keep");
    let data_dir = TempDir::new("repos-keep-data");
    let config = cfg_dir.child("config.yml");
    write_file(
        &config,
        "defaults:\n  agent: claude\n  poll_interval_minutes: 7\n  diff_max_lines: 123\n\
         repos:\n  - name: org/keep\n    review_mode: monitor\n    diff_max_files: 3\n  - name: org/other\n",
    );
    let effective = || {
        let parsed = prx::config::Config::load(&config).unwrap();
        parsed.validate().unwrap();
        parsed.effective_repos().unwrap()
    };
    let before = effective();

    let out = run_cli_isolated(
        &config,
        data_dir.path(),
        &["repos", "add", "org/new"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let added = effective();
    assert_eq!(
        added.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["org/keep", "org/other", "org/new"]
    );
    // The new repo inherits the defaults; the others are untouched.
    assert_eq!(added[2].engine, "claude");
    assert_eq!(added[2].diff_max_lines, 123);
    assert_eq!(added[2].review_mode, prx::config::ReviewMode::ReviewComment);
    assert_eq!(added[0].review_mode, prx::config::ReviewMode::Monitor);
    assert_eq!(added[0].diff_max_files, 3);
    assert_eq!(added[..2], before[..]);

    let out = run_cli_isolated(&config, data_dir.path(), &["repos", "list"], None, &[]);
    assert!(out.status.success(), "{}", stderr_of(&out));
    let listed = stdout_of(&out);
    for name in ["org/keep", "org/other", "org/new"] {
        assert!(listed.contains(name), "{listed}");
    }

    let out = run_cli_isolated(
        &config,
        data_dir.path(),
        &["repos", "remove", "org/other"],
        None,
        &[],
    );
    assert!(out.status.success(), "{}", stderr_of(&out));
    let removed = effective();
    assert_eq!(
        removed.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
        ["org/keep", "org/new"]
    );
    assert_eq!(removed[0], before[0]);
    // Watching a repo tracks nothing by itself: no state, no history.
    assert!(!data_dir.child("state.json").exists());
    assert!(!data_dir.child("history.jsonl").exists());
}

/// A rejected name changes nothing on disk.
#[test]
fn repos_add_rejection_leaves_the_file_untouched() {
    let cfg_dir = TempDir::new("repos-reject");
    let data_dir = TempDir::new("repos-reject-data");
    let config = cfg_dir.child("config.yml");
    let original = "# hand-written\ndefaults:\n  agent: pi\nrepos:\n  - name: org/keep   # mine\n";
    write_file(&config, original);
    for bad in ["../x/y", "org/re po", "org", "a/b/c"] {
        let out = run_cli(
            &config,
            data_dir.path(),
            &["repos", "add", "--", bad],
            None,
            &[],
        );
        assert!(!out.status.success(), "'{bad}' should be rejected");
    }
    let out = run_cli(
        &config,
        data_dir.path(),
        &["repos", "remove", "org/absent"],
        None,
        &[],
    );
    assert!(!out.status.success());
    assert_eq!(fs::read_to_string(&config).unwrap(), original);
}

/// A repo name becomes part of `gh` arguments, REST paths
/// (`repos/<name>/pulls/...`), PR keys (`<name>#<n>`), and file names, so
/// only what GitHub itself allows in `owner/repo` should be accepted.
#[test]
fn repos_add_rejects_names_github_cannot_have() {
    let cfg_dir = TempDir::new("repos-charset");
    let data_dir = TempDir::new("repos-charset-data");
    let config = cfg_dir.child("config.yml");
    write_file(&config, "defaults:\n  agent: pi\nrepos: []\n");
    for bad in [
        "org/repo#1",
        "org/a;b",
        "org/$(id)",
        "org/a?b",
        "-x/y",
        "org/..",
        "org/.",
    ] {
        let out = run_cli(
            &config,
            data_dir.path(),
            &["repos", "add", "--", bad],
            None,
            &[],
        );
        assert!(!out.status.success(), "'{bad}' should be rejected");
    }
    assert!(prx::config::Config::load(&config).unwrap().repos.is_empty());
}
