//! Library-level tests of the `Gh` trait implementation against the fake gh
//! fixture (exact argv, JSON parsing incl. author-object flattening, and
//! error mapping).

#![allow(clippy::await_holding_lock)]

mod common;

use common::*;
use prx::github::{Gh, GhCli, GhError, PrLifecycle};

/// The fake gh reads its fixture dir from the process env, so the tests in
/// this file take turns.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn gh_cli_parses_list_lifecycle_and_maps_errors() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let gh_dir = TempDir::new("gh-trait");
    let fixture = install_fake_gh(gh_dir.path());
    let log = gh_dir.child("calls.log");

    std::env::set_var("FAKE_GH_DIR", gh_dir.path());
    std::env::set_var("FAKE_GH_LOG", &log);

    let gh = GhCli::with_binary(fixture.to_string_lossy().to_string());

    // No fixture file → empty list.
    let empty = gh.list_open_prs("org/none").await.unwrap();
    assert!(empty.is_empty());

    // A list fixture: author object must flatten to the login string.
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
    let prs = gh.list_open_prs("org/one").await.unwrap();
    assert_eq!(prs.len(), 1);
    let pr = &prs[0];
    assert_eq!(pr.number, 1);
    assert_eq!(pr.author, "alice"); // login extracted from the author object
    assert_eq!(pr.title, "Add feature");
    assert_eq!(pr.head_branch, "feature/x");
    assert_eq!(pr.head_sha, "aaaa");
    assert!(!pr.is_draft);
    assert_eq!(pr.url, "https://github.com/org/repo/pull/1");
    assert_eq!(pr.created_at.to_rfc3339(), "2026-09-09T10:00:00+00:00");

    // Lifecycle defaults to OPEN when no state fixture exists.
    assert_eq!(
        gh.pr_lifecycle("org/one", 1).await.unwrap(),
        PrLifecycle::Open
    );
    write_file(&gh_dir.child("org_one#1.state"), "MERGED");
    assert_eq!(
        gh.pr_lifecycle("org/one", 1).await.unwrap(),
        PrLifecycle::Merged
    );
    write_file(&gh_dir.child("org_one#1.state"), "CLOSED");
    assert_eq!(
        gh.pr_lifecycle("org/one", 1).await.unwrap(),
        PrLifecycle::Closed
    );

    // The exact argv is logged by the fixture.
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains(
        "pr list -R org/one --json number,title,author,headRefName,headRefOid,isDraft,createdAt,url --state open --limit 200"
    ));
    assert!(calls.contains("pr view 1 -R org/one --json state"));

    // Missing binary maps to BinaryNotFound.
    let broken = GhCli::with_binary("/nonexistent/prx-fake-gh");
    let err = broken.list_open_prs("org/one").await.unwrap_err();
    assert!(matches!(err, GhError::BinaryNotFound(_)));

    // Auth status helper: ok and failing paths.
    std::env::remove_var("FAKE_GH_AUTH_FAIL");
    let status = prx::github::gh_auth_status(&fixture.to_string_lossy()).await;
    assert!(status.ok);
    assert_eq!(status.user.as_deref(), Some("testuser"));
    std::env::set_var("FAKE_GH_AUTH_FAIL", "1");
    let status = prx::github::gh_auth_status(&fixture.to_string_lossy()).await;
    assert!(!status.ok);
    assert!(status.user.is_none());
    assert!(status.detail.contains("gh auth login"));
    std::env::remove_var("FAKE_GH_AUTH_FAIL");
    std::env::remove_var("FAKE_GH_DIR");
    std::env::remove_var("FAKE_GH_LOG");
}

/// Task 3.2 / design D5: the single-PR fetch builds exactly the snapshot
/// the poll's list call reports for the same PR, plus its lifecycle; a PR
/// that does not exist is an error.
#[tokio::test]
async fn pr_snapshot_matches_the_list_output_for_the_same_pr() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let gh_dir = TempDir::new("gh-snapshot");
    let fixture = install_fake_gh(gh_dir.path());
    let log = gh_dir.child("calls.log");
    let gh = GhCli::with_binary(fixture.to_string_lossy().to_string());
    std::env::set_var("FAKE_GH_DIR", gh_dir.path());
    std::env::set_var("FAKE_GH_LOG", &log);

    let entry = pr_json(
        7,
        "Draft work",
        "bob",
        "feature/y",
        "bbbb",
        true,
        "2026-09-10T08:30:00Z",
    );
    write_list_fixture(gh_dir.path(), "org/one", std::slice::from_ref(&entry));
    write_pr_fixture(gh_dir.path(), "org/one", 7, &entry);

    let listed = gh.list_open_prs("org/one").await.unwrap();
    let (single, lifecycle) = gh.pr_snapshot("org/one", 7).await.unwrap();
    assert_eq!(lifecycle, PrLifecycle::Open);
    assert_eq!(single, listed[0]);
    assert_eq!(
        prx::machine::PrSnapshot::from_summary("org/one", &single),
        prx::machine::PrSnapshot::from_summary("org/one", &listed[0])
    );

    write_file(&gh_dir.child("org_one#7.state"), "MERGED");
    let (_, lifecycle) = gh.pr_snapshot("org/one", 7).await.unwrap();
    assert_eq!(lifecycle, PrLifecycle::Merged);

    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains(
        "pr view 7 -R org/one --json number,title,author,headRefName,headRefOid,isDraft,createdAt,url,state"
    ));

    let err = gh.pr_snapshot("org/one", 8).await.unwrap_err();
    assert!(matches!(err, GhError::CommandFailed { .. }), "{err:?}");
}
