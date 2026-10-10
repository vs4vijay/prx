//! Clap command types (spec §3.8). Dispatch lives in `main.rs`.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "prx",
    version,
    about = "Local-first PR review lifecycle manager",
    after_help = "Run `prx init` first, then `prx repos add <org/repo>` and `prx status`."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initial setup: create config and data dirs, write the starter config
    Init,
    /// Check that everything prx needs is in place
    Doctor,
    /// Manage watched repositories
    Repos {
        #[command(subcommand)]
        cmd: ReposCmd,
    },
    /// Show the current state of watched PRs (through the daemon when it is up)
    Status(StatusArgs),
    /// Trigger a review (through the daemon when it is up; an explicit
    /// target bypasses cooldowns, --all reviews only due PRs)
    Review {
        /// PR target: org/repo#123, a GitHub PR URL, repo#123, 123, or #123
        /// (the short forms resolve against the daemon's watched repos)
        target: Option<String>,
        /// Review all due PRs across watched repos
        #[arg(long)]
        all: bool,
        /// Run the pipeline without any GitHub writes or state changes
        /// (always local — a dry-run never touches the daemon)
        #[arg(long)]
        dry_run: bool,
        /// Run this configured engine instead of the repo's (this run only)
        #[arg(long, value_name = "NAME", conflicts_with = "all")]
        engine: Option<String>,
        /// Review without posting to GitHub (this run only, like monitor mode)
        #[arg(long, conflicts_with = "all")]
        no_post: bool,
        /// Extra instructions appended to this run's prompt
        #[arg(long, value_name = "TEXT", conflicts_with = "all")]
        prompt: Option<String>,
    },
    /// Print a PR's brief (what it does, in plain language) and its dates
    /// (through the daemon when it is up)
    Brief {
        /// PR target: org/repo#123, a GitHub PR URL, repo#123, 123, or #123
        /// (the short forms resolve against the daemon's watched repos)
        target: String,
        /// Generate a new brief first (never posts to GitHub)
        #[arg(long)]
        regenerate: bool,
    },
    /// Post your GitHub approval on a PR (the only approve path)
    Approve {
        /// PR target like org/repo#123
        target: String,
    },
    /// Snooze a PR: skip reviews until a time or the next push
    Snooze {
        /// PR target like org/repo#123
        target: String,
        /// Snooze for N hours
        #[arg(long, conflicts_with = "until_push")]
        hours: Option<f64>,
        /// Snooze until the next push to the PR
        #[arg(long)]
        until_push: bool,
    },
    /// Ignore a PR until manually un-ignored
    Ignore {
        /// PR target like org/repo#123
        target: String,
    },
    /// Stop ignoring a PR (returns to pending with a fresh cooldown)
    Unignore {
        /// PR target like org/repo#123
        target: String,
    },
    /// Ask the daemon to poll all repos and enqueue what's due now
    ReviewNow,
    /// Pause automatic reviews daemon-wide (polling continues; an explicit
    /// `prx review org/repo#N` still runs). Survives daemon restarts.
    Pause {
        /// Pause for N hours
        #[arg(long)]
        hours: Option<f64>,
        /// Pause until a time: RFC 3339, 'YYYY-MM-DD HH:MM', or 'HH:MM' (local)
        #[arg(long, value_name = "TIME")]
        until: Option<String>,
    },
    /// Lift a pause set by `prx pause`
    Resume,
    /// Recent review history (through the daemon when it is up)
    History {
        /// Filter by repo: org/repo
        #[arg(long)]
        repo: Option<String>,
        /// Maximum entries to show
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Emit machine-readable JSON instead of a timeline
        #[arg(long)]
        json: bool,
    },
    /// Review analytics and patterns computed from review history
    Insights {
        /// Restrict the analysis to one repo: org/repo
        #[arg(long)]
        repo: Option<String>,
        /// Restrict the analysis to the last N days (default: all time)
        #[arg(long, value_name = "N")]
        days: Option<u64>,
    },
    /// Token usage and cost of review runs, aggregated from review history
    /// (through the daemon when it is up)
    Usage {
        /// Bucket size: day, week (ISO, Monday start), or month
        #[arg(long = "by", value_name = "BUCKET", default_value = "day")]
        bucket: crate::usage::Bucket,
        /// Time window ending today: 7d, 30d, 90d, or all
        #[arg(long, default_value = "30d")]
        window: crate::usage::Window,
        /// Split by: none, engine, repo, pr, or session
        #[arg(long, default_value = "none")]
        group: crate::usage::Group,
        /// Restrict to one repo: org/repo
        #[arg(long)]
        repo: Option<String>,
        /// Emit the machine-readable report (the `GET /usage` shape)
        #[arg(long)]
        json: bool,
    },
    /// Show or edit configuration (`prx config` alone shows it)
    Config {
        #[command(subcommand)]
        cmd: Option<ConfigCmd>,
    },
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
}

#[derive(Args, Debug)]
pub struct StatusArgs {
    /// Restrict to one repo: org/repo
    pub repo: Option<String>,
    /// Emit machine-readable JSON instead of a table
    #[arg(long)]
    pub json: bool,
}

#[derive(Subcommand, Debug)]
pub enum ReposCmd {
    /// Add a repo to watch: org/repo
    Add { repo: String },
    /// List configured repos
    List,
    /// Stop watching a repo: org/repo
    Remove { repo: String },
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the fully-merged effective configuration
    Show,
    /// Open the config file in $VISUAL (or $EDITOR)
    Edit,
    /// Set a config value by dotted key path
    /// (e.g. defaults.poll_interval_minutes, repos.org/repo.agent)
    Set { key: String, value: String },
}

#[derive(Subcommand, Debug)]
pub enum DaemonCmd {
    /// Start the daemon (detached by default; --foreground blocks and logs to stderr too)
    Start {
        /// Run in the foreground instead of detaching
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running daemon (SIGTERM, waits for exit)
    Stop,
    /// Check whether the daemon is running
    Status,
    /// Tail the last 100 lines of the newest daemon log
    Logs,
    /// Register the daemon to start at login (launchd plist on macOS, systemd
    /// user unit on Linux). Never starts or stops the daemon. Registers a
    /// stable binary path: the override below, else ~/.cargo/bin/prx when
    /// running from a build directory, else the running binary.
    Install {
        /// Register this exact binary instead of resolving it (must be absolute)
        #[arg(long, value_name = "PATH")]
        binary_path: Option<PathBuf>,
        /// Also register the menubar agent to start at login (macOS only;
        /// on Linux the flag warns and only the daemon is registered)
        #[arg(long)]
        include_menubar: bool,
        /// Register this PRX.app bundle as the login item (macOS). Without
        /// it, the bundle containing this binary, /Applications/PRX.app, or
        /// ~/Applications/PRX.app is used when present; with no bundle the
        /// headless daemon agent is registered.
        #[arg(long, value_name = "PATH")]
        app: Option<PathBuf>,
    },
    /// Remove the login-autostart registration (a running daemon is left alone)
    Uninstall,
}
