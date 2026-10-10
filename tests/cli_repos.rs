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
