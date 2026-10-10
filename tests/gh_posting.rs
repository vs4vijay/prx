//! Library-level tests of the Phase 2 `Gh` additions (pr_view/pr_diff and
//! the posting helpers) against the fake gh fixture: exact argv, JSON
//! parsing, and stdin capture for `--body-file -`. One combined test (the
//! Phase 1 house pattern) because the FAKE_GH_* env vars are process-global.

mod common;

use common::*;
use prx::github::{Gh, GhCli, GhError, InlineComment, ReviewFlag};

fn calls(dir: &TempDir) -> Vec<String> {
    std::fs::read_to_string(dir.child("calls.log"))
        .unwrap_or_default()
        .lines()
        .map(|l| l.to_string())
        .collect()
}

#[tokio::test]
async fn gh_cli_pr_view_diff_and_posting_helpers_match_the_contract() {
    let dir = TempDir::new("gh-posting");
    let fixture = install_fake_gh(dir.path());
    std::env::set_var("FAKE_GH_DIR", dir.path());
    std::env::set_var("FAKE_GH_LOG", dir.child("calls.log"));
    let gh = GhCli::with_binary(fixture.to_string_lossy().to_string());

    // ---- pr view: diff stats + head sha parsing
    write_file(
        &dir.child("org_one#5.detail.json"),
        r#"{
          "files": [
            {"path": "src/a.rs", "additions": 10, "deletions": 2},
            {"path": "src/b.rs", "additions": 1, "deletions": 0},
            {"path": "README.md", "additions": 0, "deletions": 4}
          ],
          "additions": 11,
          "deletions": 6,
          "commits": [{"oid": "1111"}, {"oid": "2222"}]
        }"#,
    );
    let detail = gh.pr_view("org/one", 5).await.unwrap();
    assert_eq!(detail.files_changed, 3);
    assert_eq!(detail.additions, 11);
    assert_eq!(detail.deletions, 6);
    assert_eq!(detail.head_sha.as_deref(), Some("2222")); // last commit = head

    // Missing fixture → empty detail (the A14 cap check passes on it).
    let empty = gh.pr_view("org/none", 1).await.unwrap();
    assert_eq!(empty.files_changed, 0);
    assert_eq!(empty.head_sha, None);
    assert!(calls(&dir)
        .iter()
        .any(|l| l == "pr view 5 -R org/one --json files,additions,deletions,commits"));

    // ---- pr diff
    write_file(
        &dir.child("org_one#9.diff"),
        "diff --git a/x b/x\n+fn main() {}\n",
    );
    let diff = gh.pr_diff("org/one", 9).await.unwrap();
    assert!(diff.contains("+fn main() {}"));
    assert!(calls(&dir).iter().any(|l| l == "pr diff 9 -R org/one"));

    // ---- inline comment: REST API argv shape (A2)
    let comment = InlineComment {
        body: "blocking: sql injection here".into(),
        commit_id: "deadbeef".into(),
        path: "src/db.rs".into(),
        line: 42,
    };
    gh.post_inline_comment("org/one", 7, &comment)
        .await
        .unwrap();
    assert!(calls(&dir).iter().any(|l| {
        l.contains("api repos/org/one/pulls/7/comments")
            && l.contains("-f body=blocking: sql injection here")
            && l.contains("-F commit_id=deadbeef")
            && l.contains("-f path=src/db.rs")
            && l.contains("-F line=42")
    }));

    // ---- review body: stdin piping with the right verdict flag (D2: no approve)
    let body = "## prx review\n\n\u{2014} automated review by prx\n";
    gh.submit_review("org/one", 3, ReviewFlag::RequestChanges, body)
        .await
        .unwrap();
    gh.submit_review("org/one", 4, ReviewFlag::Comment, body)
        .await
        .unwrap();
    let log = calls(&dir);
    // Regression (live-repo validation, 2026-10-04): real gh rejects the
    // combined form `gh pr review -R org/repo#N ...` with a usage error
    // ("argument required when using the --repo flag"). The PR number must
    // be positional: `gh pr review <n> -R org/repo --flag --body-file -`.
    assert!(log
        .iter()
        .any(|l| l == "pr review 3 -R org/one --request-changes --body-file -"));
    assert!(log
        .iter()
        .any(|l| l == "pr review 4 -R org/one --comment --body-file -"));
    assert!(!log.iter().any(|l| l.starts_with("pr review -R ")));
    let captured = std::fs::read_to_string(dir.child("org_one#3.review-body.txt")).unwrap();
    assert_eq!(captured, body);
    assert!(dir.child("org_one#4.review-body.txt").exists());

    // ---- human approve path + "already approved" error injection
    gh.pr_approve("org/one", 2, None).await.unwrap();
    assert!(calls(&dir)
        .iter()
        .any(|l| l == "pr review 2 -R org/one --approve"));
    // A configured approve message rides along as the review body; a blank
    // one approves bare.
    gh.pr_approve("org/one", 2, Some("Looks good, thanks!"))
        .await
        .unwrap();
    assert!(calls(&dir)
        .iter()
        .any(|l| l == "pr review 2 -R org/one --approve --body-file -"));
    gh.pr_approve("org/one", 2, Some("  ")).await.unwrap();
    assert_eq!(
        calls(&dir)
            .iter()
            .filter(|l| l.as_str() == "pr review 2 -R org/one --approve")
            .count(),
        2
    );
    write_file(
        &dir.child("org_one#2.approve-error"),
        "already approved this pull request",
    );
    let err = gh.pr_approve("org/one", 2, None).await.unwrap_err();
    assert!(matches!(err, GhError::CommandFailed { .. }));
    assert!(err.to_string().contains("already approved"));

    // ---- Regression (live-repo validation, 2026-10-04): GitHub rejects
    // verdict reviews on self-authored PRs ("Review Can not request changes
    // on your own pull request"). submit_review must degrade the flag to
    // --comment and post the same body instead of failing.
    let self_body =
        "## prx review\n\nverdict: changes_requested\n\n\u{2014} automated review by prx\n";
    write_file(
        &dir.child("org_one#6.review-error"),
        "failed to create review: GraphQL: Review Can not request changes on your own pull request (addPullRequestReview)",
    );
    gh.submit_review("org/one", 6, ReviewFlag::RequestChanges, self_body)
        .await
        .unwrap();
    let log = calls(&dir);
    assert!(log
        .iter()
        .any(|l| l == "pr review 6 -R org/one --request-changes --body-file -"));
    assert!(log
        .iter()
        .any(|l| l == "pr review 6 -R org/one --comment --body-file -"));
    let degraded = std::fs::read_to_string(dir.child("org_one#6.review-body.txt")).unwrap();
    assert_eq!(degraded, self_body);

    std::env::remove_var("FAKE_GH_DIR");
    std::env::remove_var("FAKE_GH_LOG");
}
