//! GitHub I/O via the `gh` CLI (never direct HTTP — auth is delegated to gh).
//!
//! The [`Gh`] trait is the seam for testing and for Phase 2: diff/view and
//! the posting helpers (inline comments via `gh api`, review bodies via
//! `gh pr review`, human approvals) will be added as additional methods
//! without changing the existing surface.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default timeout for a single `gh` invocation.
const GH_TIMEOUT_SECS: u64 = 30;

/// Fields requested from `gh pr list` (DEV-PLAN Phase 1 contract).
pub const LIST_FIELDS: &str = "number,title,author,headRefName,headRefOid,isDraft,createdAt,url";

/// Fields requested from `gh pr view` for one PR's poll snapshot plus its
/// lifecycle (on-demand tracking, design D5): the list fields and `state`.
pub const VIEW_SNAPSHOT_FIELDS: &str =
    "number,title,author,headRefName,headRefOid,isDraft,createdAt,url,state";

/// Fields requested from `gh pr view` for diff stats + head sha.
pub const VIEW_DETAIL_FIELDS: &str = "files,additions,deletions,commits";

/// Fields requested from `gh pr view` for an on-demand brief (design B2).
pub const VIEW_BRIEF_FIELDS: &str = "title,body,author,files";

/// What an on-demand brief describes besides the diff (design B2): the PR's
/// title, description, author, and every changed file path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrBriefView {
    pub title: String,
    pub body: String,
    pub author: String,
    pub files: Vec<String>,
}

#[derive(Debug, Error)]
pub enum GhError {
    #[error("gh binary not found on PATH: {0}")]
    BinaryNotFound(String),
    #[error("gh timed out after {timeout}s running: {command}")]
    Timeout { timeout: u64, command: String },
    #[error("gh command failed ({command}): {stderr}")]
    CommandFailed { command: String, stderr: String },
    #[error("failed to parse gh output: {0}")]
    Parse(String),
    #[error("gh io error: {0}")]
    Io(#[from] std::io::Error),
}

/// One open pull request as returned by `gh pr list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    /// Author login (gh returns an object; we flatten to the login string).
    pub author: String,
    pub head_branch: String,
    pub head_sha: String,
    pub is_draft: bool,
    pub created_at: DateTime<Utc>,
    pub url: String,
}

/// Whether a PR is open, merged, or closed (used when a tracked PR vanishes
/// from the open list).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum PrLifecycle {
    Open,
    Merged,
    Closed,
}

/// Diff stats for one PR (`gh pr view --json files,additions,deletions,commits`),
/// used for the A14 diff-size cap and for the commit the review anchors to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrDetail {
    /// Number of files touched by the diff (length of the `files` array).
    pub files_changed: usize,
    pub additions: u64,
    pub deletions: u64,
    /// Head commit oid (last entry of `commits`), when reported.
    pub head_sha: Option<String>,
}

/// One line-anchored inline comment to post via the REST API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineComment {
    pub body: String,
    /// Head commit the comment anchors to.
    pub commit_id: String,
    pub path: String,
    pub line: u64,
}

/// The review verdict flag for the single review body (D2: never `--approve`
/// from the automated path — approve is the human-only `pr_approve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewFlag {
    RequestChanges,
    Comment,
}

impl ReviewFlag {
    fn gh_arg(self) -> &'static str {
        match self {
            ReviewFlag::RequestChanges => "--request-changes",
            ReviewFlag::Comment => "--comment",
        }
    }
}

/// The GraphQL error GitHub returns when a verdict review (`--request-changes`
/// or `--approve`) targets a pull request authored by the authenticated user.
/// prx degrades those to a COMMENT review instead of failing outright.
fn self_review_error(gh_arg: &str) -> &'static str {
    match gh_arg {
        "--request-changes" => "Can not request changes on your own pull request",
        _ => "Can not approve your own pull request",
    }
}

/// GitHub operations prx needs today. Futures are `+ Send` so the Phase 3
/// daemon can await them across task boundaries.
pub trait Gh: Send + Sync {
    fn list_open_prs(
        &self,
        repo: &str,
    ) -> impl std::future::Future<Output = Result<Vec<PrSummary>, GhError>> + Send;
    fn pr_lifecycle(
        &self,
        repo: &str,
        number: u64,
    ) -> impl std::future::Future<Output = Result<PrLifecycle, GhError>> + Send;
    /// One PR with exactly the fields `list_open_prs` reports, plus its
    /// lifecycle (design D5: tracking a PR on demand builds the same
    /// snapshot a poll would). A PR that does not exist is an error.
    fn pr_snapshot(
        &self,
        repo: &str,
        number: u64,
    ) -> impl std::future::Future<Output = Result<(PrSummary, PrLifecycle), GhError>> + Send;
    /// PR detail with diff stats (A14 cap check) and the current head sha.
    fn pr_view(
        &self,
        repo: &str,
        number: u64,
    ) -> impl std::future::Future<Output = Result<PrDetail, GhError>> + Send;
    /// Title, description, author, and changed file paths for an
    /// on-demand brief (design B2). Read-only.
    fn pr_brief_view(
        &self,
        repo: &str,
        number: u64,
    ) -> impl std::future::Future<Output = Result<PrBriefView, GhError>> + Send;
    /// The full PR diff (read-only; delta reviews and diagnostics).
    fn pr_diff(
        &self,
        repo: &str,
        number: u64,
    ) -> impl std::future::Future<Output = Result<String, GhError>> + Send;
    /// Post ONE line-anchored inline comment (A2: inline comments require the
    /// REST API, `gh pr review` cannot create them). Best-effort per comment.
    fn post_inline_comment(
        &self,
        repo: &str,
        number: u64,
        comment: &InlineComment,
    ) -> impl std::future::Future<Output = Result<(), GhError>> + Send;
    /// Submit the single review body: `--request-changes` iff there are
    /// blocking findings, else `--comment` (never `--approve` here — D2/A1).
    fn submit_review(
        &self,
        repo: &str,
        number: u64,
        flag: ReviewFlag,
        body: &str,
    ) -> impl std::future::Future<Output = Result<(), GhError>> + Send;
    /// Human-only approval path (D2): `prx approve` is the ONLY route to a
    /// GitHub approval.
    fn pr_approve(
        &self,
        repo: &str,
        number: u64,
        body: Option<&str>,
    ) -> impl std::future::Future<Output = Result<(), GhError>> + Send;
    /// Human-only Block (design D9): a request-changes review with `body`.
    /// Unlike [`Gh::submit_review`] there is no comment fallback — a
    /// comment is not a block, so GitHub's refusal surfaces as the error.
    fn pr_request_changes(
        &self,
        repo: &str,
        number: u64,
        body: &str,
    ) -> impl std::future::Future<Output = Result<(), GhError>> + Send;
    /// The login of the account gh is authenticated as (`gh api user`).
    fn current_login(&self) -> impl std::future::Future<Output = Result<String, GhError>> + Send;
    /// Every review on the PR, oldest first (`gh api .../reviews`).
    fn list_reviews(
        &self,
        repo: &str,
        number: u64,
    ) -> impl std::future::Future<Output = Result<Vec<PrReview>, GhError>> + Send;
    /// Dismiss one review (Unblock, design D9).
    fn dismiss_review(
        &self,
        repo: &str,
        number: u64,
        review_id: u64,
        message: &str,
    ) -> impl std::future::Future<Output = Result<(), GhError>> + Send;
}

/// One submitted PR review, as listed by the reviews API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrReview {
    pub id: u64,
    /// `APPROVED` | `CHANGES_REQUESTED` | `COMMENTED` | `DISMISSED` | ...
    pub state: String,
    /// Author login (empty for deleted accounts).
    #[serde(default)]
    pub login: String,
}

/// Production [`Gh`] implementation that shells out to the `gh` binary.
#[derive(Debug, Clone)]
pub struct GhCli {
    binary: String,
    /// Identity credential reference (design D12): the NAME of the
    /// environment variable whose value becomes `GH_TOKEN` for every
    /// invocation; `None` uses gh's own authenticated account. Resolved per
    /// call, so a missing variable surfaces as that call's error.
    credential: Option<String>,
}

impl Default for GhCli {
    fn default() -> Self {
        GhCli::new()
    }
}

impl GhCli {
    /// Use `gh` from PATH.
    pub fn new() -> Self {
        GhCli {
            binary: "gh".to_string(),
            credential: None,
        }
    }

    /// Use an explicit binary (tests inject a fake `gh` script).
    pub fn with_binary(binary: impl Into<String>) -> Self {
        GhCli {
            binary: binary.into(),
            credential: None,
        }
    }

    /// This client acting as an identity's credential (design D12): with a
    /// credential reference, gh runs with `GH_TOKEN` read from that
    /// environment variable; without one, gh's own account is used. Every
    /// GitHub write (review bodies, inline comments, approve, block,
    /// unblock) goes through a client built here.
    pub fn for_credential(&self, credential: Option<&str>) -> GhCli {
        GhCli {
            binary: self.binary.clone(),
            credential: credential.map(str::to_string),
        }
    }

    /// The `GH_TOKEN` value for this client's credential, if any.
    fn token(&self) -> Result<Option<String>, GhError> {
        let Some(var) = &self.credential else {
            return Ok(None);
        };
        std::env::var(var)
            .map(Some)
            .map_err(|_| GhError::CommandFailed {
                command: format!("{} (credential {var})", self.binary),
                stderr: format!(
                    "identity credential environment variable {var} is not set in the daemon's environment"
                ),
            })
    }

    /// Repositories this account can see (owned, collaborator, org member),
    /// most recently pushed first, archived ones left out — up to `pages`
    /// pages of 100. For the settings app's repo picker.
    pub async fn list_accessible_repos(
        &self,
        pages: u32,
    ) -> Result<Vec<crate::discover::RepoSummary>, GhError> {
        let mut repos = Vec::new();
        for page in 1..=pages {
            let endpoint = format!("user/repos?per_page=100&sort=pushed&page={page}");
            let out = self
                .run(&[
                    "api",
                    &endpoint,
                    "--jq",
                    ".[] | select(.archived | not) | [.full_name, .private] | @tsv",
                ])
                .await?;
            // A full page has 100 entries before the archived filter, so
            // the raw count is unknown here: stop on an empty page.
            let before = repos.len();
            repos.extend(out.stdout.lines().filter_map(|line| {
                let (name, private) = line.split_once('\t')?;
                Some(crate::discover::RepoSummary {
                    name: name.trim().to_string(),
                    private: private.trim() == "true",
                })
            }));
            if repos.len() == before {
                break;
            }
        }
        Ok(repos)
    }

    async fn run(&self, args: &[&str]) -> Result<RunOutput, GhError> {
        self.run_with_stdin(args, None).await
    }

    /// Run gh with optional stdin content (used for `--body-file -`).
    async fn run_with_stdin(
        &self,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<RunOutput, GhError> {
        let command = format!("{} {}", self.binary, args.join(" "));
        tracing::debug!("running: {command}");
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(token) = self.token()? {
            cmd.env("GH_TOKEN", token);
        }
        match stdin {
            Some(_) => cmd.stdin(std::process::Stdio::piped()),
            None => cmd.stdin(std::process::Stdio::null()),
        };
        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GhError::BinaryNotFound(self.binary.clone())
            } else {
                GhError::Io(e)
            }
        })?;
        if let Some(content) = stdin {
            use tokio::io::AsyncWriteExt;
            if let Some(mut pipe) = child.stdin.take() {
                pipe.write_all(content.as_bytes())
                    .await
                    .map_err(GhError::Io)?;
                pipe.flush().await.map_err(GhError::Io)?;
            }
        }
        // Dropping the stdin handle closes the pipe (EOF for the reader).
        drop(child.stdin.take());
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(GH_TIMEOUT_SECS),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| GhError::Timeout {
            timeout: GH_TIMEOUT_SECS,
            command: command.clone(),
        })?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                GhError::BinaryNotFound(self.binary.clone())
            } else {
                GhError::Io(e)
            }
        })?;
        if !output.status.success() {
            return Err(GhError::CommandFailed {
                command,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(RunOutput {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        })
    }
}

struct RunOutput {
    stdout: String,
}

impl Gh for GhCli {
    async fn list_open_prs(&self, repo: &str) -> Result<Vec<PrSummary>, GhError> {
        let out = self
            .run(&[
                "pr",
                "list",
                "-R",
                repo,
                "--json",
                LIST_FIELDS,
                "--state",
                "open",
                "--limit",
                "200",
            ])
            .await?;
        let raw: Vec<RawPr> = serde_json::from_str(&out.stdout)
            .map_err(|e| GhError::Parse(format!("pr list for {repo}: {e}")))?;
        Ok(raw.into_iter().map(RawPr::into_summary).collect())
    }

    async fn pr_lifecycle(&self, repo: &str, number: u64) -> Result<PrLifecycle, GhError> {
        let number = number.to_string();
        let out = self
            .run(&["pr", "view", &number, "-R", repo, "--json", "state"])
            .await?;
        let raw: RawState = serde_json::from_str(&out.stdout)
            .map_err(|e| GhError::Parse(format!("pr view {repo}#{number}: {e}")))?;
        PrLifecycle::deserialize_str(&raw.state)
            .ok_or_else(|| GhError::Parse(format!("unknown PR state '{}'", raw.state)))
    }

    async fn pr_snapshot(
        &self,
        repo: &str,
        number: u64,
    ) -> Result<(PrSummary, PrLifecycle), GhError> {
        let number = number.to_string();
        let out = self
            .run(&[
                "pr",
                "view",
                &number,
                "-R",
                repo,
                "--json",
                VIEW_SNAPSHOT_FIELDS,
            ])
            .await?;
        let raw: RawPrWithState = serde_json::from_str(&out.stdout)
            .map_err(|e| GhError::Parse(format!("pr view {repo}#{number}: {e}")))?;
        let lifecycle = PrLifecycle::deserialize_str(&raw.state)
            .ok_or_else(|| GhError::Parse(format!("unknown PR state '{}'", raw.state)))?;
        Ok((raw.pr.into_summary(), lifecycle))
    }

    async fn pr_view(&self, repo: &str, number: u64) -> Result<PrDetail, GhError> {
        let number = number.to_string();
        let out = self
            .run(&[
                "pr",
                "view",
                &number,
                "-R",
                repo,
                "--json",
                VIEW_DETAIL_FIELDS,
            ])
            .await?;
        let raw: RawPrDetail = serde_json::from_str(&out.stdout)
            .map_err(|e| GhError::Parse(format!("pr view {repo}#{number}: {e}")))?;
        Ok(raw.into_detail())
    }

    async fn pr_brief_view(&self, repo: &str, number: u64) -> Result<PrBriefView, GhError> {
        let number = number.to_string();
        let out = self
            .run(&[
                "pr",
                "view",
                &number,
                "-R",
                repo,
                "--json",
                VIEW_BRIEF_FIELDS,
            ])
            .await?;
        let raw: RawPrBriefView = serde_json::from_str(&out.stdout)
            .map_err(|e| GhError::Parse(format!("pr view {repo}#{number}: {e}")))?;
        Ok(PrBriefView {
            title: raw.title,
            body: raw.body.unwrap_or_default(),
            author: raw.author.map(|a| a.login).unwrap_or_default(),
            files: raw
                .files
                .into_iter()
                .map(|f| f.path)
                .filter(|p| !p.is_empty())
                .collect(),
        })
    }

    async fn pr_diff(&self, repo: &str, number: u64) -> Result<String, GhError> {
        let number = number.to_string();
        let out = self.run(&["pr", "diff", &number, "-R", repo]).await?;
        Ok(out.stdout)
    }

    async fn post_inline_comment(
        &self,
        repo: &str,
        number: u64,
        comment: &InlineComment,
    ) -> Result<(), GhError> {
        let endpoint = format!("repos/{repo}/pulls/{number}/comments");
        let body = format!("body={}", comment.body);
        let commit_id = format!("commit_id={}", comment.commit_id);
        let path = format!("path={}", comment.path);
        let line = format!("line={}", comment.line);
        self.run(&[
            "api", &endpoint, "-f", &body, "-F", &commit_id, "-f", &path, "-F", &line,
        ])
        .await
        .map(|_| ())
    }

    async fn submit_review(
        &self,
        repo: &str,
        number: u64,
        flag: ReviewFlag,
        body: &str,
    ) -> Result<(), GhError> {
        // Real gh requires the PR as a positional arg with the repo in -R:
        // `gh pr review <n> -R o/r --flag --body-file -`. (The combined
        // `-R o/r#n` form fails on real gh with "argument required when
        // using the --repo flag" — caught in live-repo validation.)
        let number = number.to_string();
        let base: [&str; 5] = ["pr", "review", &number, "-R", repo];
        let mut first: Vec<&str> = base.to_vec();
        first.push(flag.gh_arg());
        first.push("--body-file");
        first.push("-");
        match self.run_with_stdin(&first, Some(body)).await {
            Err(GhError::CommandFailed { stderr, .. })
                if stderr.contains(self_review_error(flag.gh_arg())) =>
            {
                // GitHub forbids verdict reviews (approve / request-changes)
                // on your own pull request — the common case for a
                // local-first watcher. The review still happened; post the
                // same body as a COMMENT review instead of failing.
                tracing::warn!(
                    "GitHub rejected {} on a self-authored PR; posting the review body as comment",
                    flag.gh_arg()
                );
                let mut retry: Vec<&str> = base.to_vec();
                retry.push(ReviewFlag::Comment.gh_arg());
                retry.push("--body-file");
                retry.push("-");
                self.run_with_stdin(&retry, Some(body)).await.map(|_| ())
            }
            other => other.map(|_| ()),
        }
    }

    async fn pr_approve(&self, repo: &str, number: u64, body: Option<&str>) -> Result<(), GhError> {
        let number = number.to_string();
        let mut args = vec!["pr", "review", &number, "-R", repo, "--approve"];
        // The configured `approve_message`, when there is one.
        let body = body.map(str::trim).filter(|b| !b.is_empty());
        if body.is_some() {
            args.extend(["--body-file", "-"]);
        }
        self.run_with_stdin(&args, body).await.map(|_| ())
    }

    async fn pr_request_changes(&self, repo: &str, number: u64, body: &str) -> Result<(), GhError> {
        let number = number.to_string();
        let args = [
            "pr",
            "review",
            &number,
            "-R",
            repo,
            ReviewFlag::RequestChanges.gh_arg(),
            "--body-file",
            "-",
        ];
        self.run_with_stdin(&args, Some(body)).await.map(|_| ())
    }

    async fn current_login(&self) -> Result<String, GhError> {
        let out = self.run(&["api", "user", "--jq", ".login"]).await?;
        let login = out.stdout.trim().to_string();
        if login.is_empty() {
            return Err(GhError::Parse("gh api user returned no login".to_string()));
        }
        Ok(login)
    }

    async fn list_reviews(&self, repo: &str, number: u64) -> Result<Vec<PrReview>, GhError> {
        // `--jq` flattens every page to one JSON object per line, so the
        // output parses the same however many pages `--paginate` fetched.
        let path = format!("repos/{repo}/pulls/{number}/reviews");
        let out = self
            .run(&[
                "api",
                &path,
                "--paginate",
                "--jq",
                ".[] | {id: .id, state: .state, login: (.user.login // \"\")}",
            ])
            .await?;
        out.stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|e| GhError::Parse(format!("review list entry {line:?}: {e}")))
            })
            .collect()
    }

    async fn dismiss_review(
        &self,
        repo: &str,
        number: u64,
        review_id: u64,
        message: &str,
    ) -> Result<(), GhError> {
        let path = format!("repos/{repo}/pulls/{number}/reviews/{review_id}/dismissals");
        let message = format!("message={message}");
        self.run(&[
            "api",
            "-X",
            "PUT",
            &path,
            "-f",
            &message,
            "-f",
            "event=DISMISS",
        ])
        .await
        .map(|_| ())
    }
}

/// `gh` returns `author` as an object; we only keep the login.
#[derive(Debug, Deserialize)]
struct RawAuthor {
    login: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPr {
    number: u64,
    title: String,
    author: RawAuthor,
    head_ref_name: String,
    head_ref_oid: String,
    is_draft: bool,
    created_at: DateTime<Utc>,
    url: String,
}

impl RawPr {
    fn into_summary(self) -> PrSummary {
        PrSummary {
            number: self.number,
            title: self.title,
            author: self.author.login,
            head_branch: self.head_ref_name,
            head_sha: self.head_ref_oid,
            is_draft: self.is_draft,
            created_at: self.created_at,
            url: self.url,
        }
    }
}

/// `gh pr view --json <list fields>,state` response.
#[derive(Debug, Deserialize)]
struct RawPrWithState {
    #[serde(flatten)]
    pr: RawPr,
    state: String,
}

#[derive(Debug, Deserialize)]
struct RawState {
    state: String,
}

/// `gh pr view --json files,additions,deletions,commits` response.
#[derive(Debug, Deserialize)]
struct RawPrDetail {
    #[serde(default)]
    files: Vec<RawFileStat>,
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    commits: Vec<RawCommit>,
}

/// `gh pr view --json title,body,author,files` response.
#[derive(Debug, Deserialize)]
struct RawPrBriefView {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    author: Option<RawLogin>,
    #[serde(default)]
    files: Vec<RawFileStat>,
}

#[derive(Debug, Deserialize)]
struct RawLogin {
    #[serde(default)]
    login: String,
}

#[derive(Debug, Deserialize)]
struct RawFileStat {
    #[serde(default)]
    path: String,
    #[serde(default)]
    #[allow(dead_code)]
    additions: u64,
    #[serde(default)]
    #[allow(dead_code)]
    deletions: u64,
}

#[derive(Debug, Deserialize)]
struct RawCommit {
    oid: String,
}

impl RawPrDetail {
    fn into_detail(self) -> PrDetail {
        PrDetail {
            files_changed: self.files.len(),
            additions: self.additions,
            deletions: self.deletions,
            head_sha: self.commits.last().map(|c| c.oid.clone()),
        }
    }
}

impl PrLifecycle {
    fn deserialize_str(s: &str) -> Option<PrLifecycle> {
        match s {
            "OPEN" => Some(PrLifecycle::Open),
            "MERGED" => Some(PrLifecycle::Merged),
            "CLOSED" => Some(PrLifecycle::Closed),
            _ => None,
        }
    }
}

/// Result of `gh auth status`, used by `prx doctor`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhAuthStatus {
    pub ok: bool,
    pub user: Option<String>,
    pub detail: String,
}

/// Run `gh auth status` (best-effort parse of the logged-in user).
pub async fn gh_auth_status(binary: &str) -> GhAuthStatus {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.args(["auth", "status"]);
    let output = match cmd.output().await {
        Ok(o) => o,
        Err(e) => {
            return GhAuthStatus {
                ok: false,
                user: None,
                detail: format!("failed to run {binary}: {e}"),
            }
        }
    };
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let detail = combined.trim().to_string();
    let ok = output.status.success();
    // gh prints e.g. "✓ Logged in to github.com as alice (keyring)".
    let user = detail.lines().find_map(|line| {
        line.split(" as ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .map(|u| u.trim_end_matches('(').trim().to_string())
    });
    GhAuthStatus { ok, user, detail }
}
