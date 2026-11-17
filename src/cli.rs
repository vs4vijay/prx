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
        /// Review a skipped or oversized PR without asking first (without a
        /// terminal, such a review is refused unless this is given)
        #[arg(short = 'y', long, conflicts_with = "all")]
        yes: bool,
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
    /// Review Chat: talk to a coding agent about a PR in this terminal.
    /// Reopens the PR's session (its last chat, else its last review's) or
    /// starts a new one with the PR's context. Never posts to GitHub.
    Chat {
        /// PR target: org/repo#123, a GitHub PR URL, repo#123, 123, or #123
        /// (the short forms resolve against the tracked PRs)
        target: String,
        /// Start a new session even if one exists
        #[arg(long)]
        new: bool,
        /// Agent engine for a new session (default: the repo's engine);
        /// starts a new session
        #[arg(long, value_name = "NAME")]
        engine: Option<String>,
        /// Run a new session in this directory, e.g. your clone of the repo
        /// (default: the PR's own chat directory); starts a new session
        #[arg(long, value_name = "DIR")]
        dir: Option<std::path::PathBuf>,
        /// Print what would run, change nothing
        #[arg(long)]
        dry_run: bool,
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
        #[arg(long, conflicts_with = "until_push", allow_negative_numbers = true)]
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
        #[arg(long, allow_negative_numbers = true)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Command {
        let argv = std::iter::once("prx").chain(args.iter().copied());
        match Cli::try_parse_from(argv) {
            Ok(cli) => cli.command,
            Err(e) => panic!("`prx {}` should parse: {e}", args.join(" ")),
        }
    }

    fn parse_err(args: &[&str]) -> ErrorKind {
        let argv = std::iter::once("prx").chain(args.iter().copied());
        match Cli::try_parse_from(argv) {
            Ok(cli) => panic!("`prx {}` should be rejected: {cli:?}", args.join(" ")),
            Err(e) => e.kind(),
        }
    }

    /// clap checks the definition itself (duplicate flags, `conflicts_with`
    /// naming a missing argument, …) only when asked; without this a typo
    /// in an attribute is a panic the first time a user runs the command.
    #[test]
    fn command_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    /// The command names are an interface: scripts, the docs, and messages
    /// printed by prx itself all spell them out.
    #[test]
    fn every_documented_command_parses_in_its_simplest_form() {
        for args in [
            &["init"][..],
            &["doctor"],
            &["repos", "add", "org/repo"],
            &["repos", "list"],
            &["repos", "remove", "org/repo"],
            &["status"],
            &["review", "org/repo#1"],
            &["review", "--all"],
            &["brief", "org/repo#1"],
            &["approve", "org/repo#1"],
            &["snooze", "org/repo#1", "--hours", "2"],
            &["ignore", "org/repo#1"],
            &["unignore", "org/repo#1"],
            &["review-now"],
            &["pause", "--hours", "1"],
            &["resume"],
            &["history"],
            &["insights"],
            &["usage"],
            &["config"],
            &["config", "show"],
            &["config", "edit"],
            &["config", "set", "defaults.engine", "claude"],
            &["daemon", "start"],
            &["daemon", "stop"],
            &["daemon", "status"],
            &["daemon", "logs"],
            &["daemon", "install"],
            &["daemon", "uninstall"],
        ] {
            parse(args);
        }
    }

    #[test]
    fn missing_or_unknown_commands_are_usage_errors() {
        assert_eq!(
            parse_err(&[]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert_eq!(parse_err(&["frobnicate"]), ErrorKind::InvalidSubcommand);
        assert_eq!(
            parse_err(&["repos"]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert_eq!(
            parse_err(&["daemon"]),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
        assert_eq!(
            parse_err(&["daemon", "restart"]),
            ErrorKind::InvalidSubcommand
        );
        assert_eq!(
            parse_err(&["status", "--verbose"]),
            ErrorKind::UnknownArgument
        );
        assert_eq!(parse_err(&["init", "extra"]), ErrorKind::UnknownArgument);
    }

    #[test]
    fn version_and_help_are_available() {
        assert_eq!(parse_err(&["--version"]), ErrorKind::DisplayVersion);
        assert_eq!(parse_err(&["--help"]), ErrorKind::DisplayHelp);
        assert_eq!(parse_err(&["review", "--help"]), ErrorKind::DisplayHelp);
        let version = Cli::command().render_version();
        assert_eq!(version.trim(), format!("prx {}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn review_defaults_to_no_target_and_no_flags() {
        let Command::Review {
            target,
            all,
            dry_run,
            engine,
            no_post,
            prompt,
            yes,
        } = parse(&["review"])
        else {
            panic!("not a review command");
        };
        assert_eq!(target, None);
        assert!(!all && !dry_run && !no_post && !yes);
        assert_eq!(engine, None);
        assert_eq!(prompt, None);
    }

    #[test]
    fn review_takes_a_target_and_the_per_run_overrides() {
        let Command::Review {
            target,
            all,
            dry_run,
            engine,
            no_post,
            prompt,
            ..
        } = parse(&[
            "review",
            "org/repo#12",
            "--engine",
            "claude",
            "--no-post",
            "--dry-run",
            "--prompt",
            "focus on auth",
        ])
        else {
            panic!("not a review command");
        };
        assert_eq!(target.as_deref(), Some("org/repo#12"));
        assert!(!all && dry_run && no_post);
        assert_eq!(engine.as_deref(), Some("claude"));
        assert_eq!(prompt.as_deref(), Some("focus on auth"));

        // `#12` must not be swallowed as anything but the target.
        let Command::Review { target, .. } = parse(&["review", "#12"]) else {
            panic!("not a review command");
        };
        assert_eq!(target.as_deref(), Some("#12"));

        for flag in ["-y", "--yes"] {
            let Command::Review { yes, .. } = parse(&["review", "org/repo#12", flag]) else {
                panic!("not a review command");
            };
            assert!(yes, "{flag}");
        }
    }

    /// The per-run overrides apply to one explicit PR; combining them with
    /// `--all` is refused instead of silently applying to every due PR.
    #[test]
    fn review_all_refuses_the_single_pr_overrides() {
        for flags in [
            &["--engine", "claude"][..],
            &["--no-post"],
            &["--prompt", "be brief"],
            &["--yes"],
        ] {
            let mut args = vec!["review", "--all"];
            args.extend_from_slice(flags);
            assert_eq!(parse_err(&args), ErrorKind::ArgumentConflict, "{flags:?}");
        }
        // A dry run of everything is fine.
        let Command::Review { all, dry_run, .. } = parse(&["review", "--all", "--dry-run"]) else {
            panic!("not a review command");
        };
        assert!(all && dry_run);
        assert_eq!(parse_err(&["review", "--engine"]), ErrorKind::InvalidValue);
        assert_eq!(parse_err(&["review", "a", "b"]), ErrorKind::UnknownArgument);
    }

    #[test]
    fn commands_that_act_on_one_pr_require_the_target() {
        for command in ["brief", "approve", "snooze", "ignore", "unignore"] {
            assert_eq!(
                parse_err(&[command]),
                ErrorKind::MissingRequiredArgument,
                "{command}"
            );
        }
        let Command::Brief { target, regenerate } = parse(&["brief", "org/repo#3"]) else {
            panic!("not a brief command");
        };
        assert_eq!(target, "org/repo#3");
        assert!(!regenerate);
        let Command::Brief { regenerate, .. } = parse(&["brief", "org/repo#3", "--regenerate"])
        else {
            panic!("not a brief command");
        };
        assert!(regenerate);
    }

    #[test]
    fn snooze_takes_hours_or_until_push_but_not_both() {
        let Command::Snooze {
            target,
            hours,
            until_push,
        } = parse(&["snooze", "org/repo#1", "--hours", "1.5"])
        else {
            panic!("not a snooze command");
        };
        assert_eq!(target, "org/repo#1");
        assert_eq!(hours, Some(1.5));
        assert!(!until_push);

        let Command::Snooze {
            hours, until_push, ..
        } = parse(&["snooze", "org/repo#1", "--until-push"])
        else {
            panic!("not a snooze command");
        };
        assert_eq!(hours, None);
        assert!(until_push);

        assert_eq!(
            parse_err(&["snooze", "org/repo#1", "--hours", "2", "--until-push"]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            parse_err(&["snooze", "org/repo#1", "--hours", "soon"]),
            ErrorKind::ValueValidation
        );
        // A negative count is a value, not an unknown flag: the command
        // itself reports that it is out of range.
        for negative in [&["--hours", "-1"][..], &["--hours=-1"]] {
            let args = [&["snooze", "org/repo#1"][..], negative].concat();
            let Command::Snooze { hours, .. } = parse(&args) else {
                panic!("not a snooze command");
            };
            assert_eq!(hours, Some(-1.0));
        }
        assert_eq!(
            parse_err(&["snooze", "org/repo#1", "--hours", "-x"]),
            ErrorKind::UnknownArgument
        );
    }

    /// `pause` accepts either form at the parser; "exactly one" is decided
    /// by `pause::resolve_pause_until`, so the daemon API and the CLI give
    /// the same message.
    #[test]
    fn pause_parses_hours_and_until_as_given() {
        let Command::Pause { hours, until } = parse(&["pause", "--hours", "0.5"]) else {
            panic!("not a pause command");
        };
        assert_eq!((hours, until), (Some(0.5), None));
        let Command::Pause { hours, until } = parse(&["pause", "--until", "2026-10-08 09:30"])
        else {
            panic!("not a pause command");
        };
        assert_eq!((hours, until.as_deref()), (None, Some("2026-10-08 09:30")));
        let Command::Pause { hours, until } = parse(&["pause"]) else {
            panic!("not a pause command");
        };
        assert_eq!((hours, until), (None, None));
        let Command::Pause { hours, until } = parse(&["pause", "--hours", "2", "--until", "18:00"])
        else {
            panic!("not a pause command");
        };
        assert_eq!((hours, until.as_deref()), (Some(2.0), Some("18:00")));

        assert_eq!(
            parse_err(&["pause", "--hours", "two"]),
            ErrorKind::ValueValidation
        );
        assert_eq!(parse_err(&["pause", "3"]), ErrorKind::UnknownArgument);
        // A negative count reaches `resolve_pause_until`, which names the
        // problem; only a value of `--hours` may start with a dash.
        let Command::Pause { hours, until } = parse(&["pause", "--hours", "-1.5"]) else {
            panic!("not a pause command");
        };
        assert_eq!((hours, until), (Some(-1.5), None));
        assert_eq!(parse_err(&["pause", "-1"]), ErrorKind::UnknownArgument);
        assert_eq!(
            parse_err(&["pause", "--hours", "--until", "18:00"]),
            ErrorKind::InvalidValue
        );
    }

    #[test]
    fn status_takes_an_optional_repo_and_json() {
        let Command::Status(args) = parse(&["status"]) else {
            panic!("not a status command");
        };
        assert_eq!(args.repo, None);
        assert!(!args.json);
        let Command::Status(args) = parse(&["status", "--json", "org/repo"]) else {
            panic!("not a status command");
        };
        assert_eq!(args.repo.as_deref(), Some("org/repo"));
        assert!(args.json);
    }

    #[test]
    fn history_defaults_and_limit_validation() {
        let Command::History { repo, limit, json } = parse(&["history"]) else {
            panic!("not a history command");
        };
        assert_eq!(repo, None);
        assert_eq!(limit, 20);
        assert!(!json);
        let Command::History { repo, limit, json } =
            parse(&["history", "--repo", "org/repo", "--limit", "0", "--json"])
        else {
            panic!("not a history command");
        };
        assert_eq!(repo.as_deref(), Some("org/repo"));
        assert_eq!(limit, 0);
        assert!(json);
        for bad in ["-1", "many", "1.5"] {
            assert_eq!(
                parse_err(&["history", &format!("--limit={bad}")]),
                ErrorKind::ValueValidation,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn insights_window_is_a_whole_number_of_days() {
        let Command::Insights { repo, days } = parse(&["insights"]) else {
            panic!("not an insights command");
        };
        assert_eq!((repo, days), (None, None));
        let Command::Insights { repo, days } =
            parse(&["insights", "--days", "30", "--repo", "org/repo"])
        else {
            panic!("not an insights command");
        };
        assert_eq!(repo.as_deref(), Some("org/repo"));
        assert_eq!(days, Some(30));
        for bad in ["-7", "7d", "0.5"] {
            assert_eq!(
                parse_err(&["insights", &format!("--days={bad}")]),
                ErrorKind::ValueValidation,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn usage_defaults_to_daily_buckets_over_thirty_days_ungrouped() {
        use crate::usage::{Bucket, Group, Window};
        let Command::Usage {
            bucket,
            window,
            group,
            repo,
            json,
        } = parse(&["usage"])
        else {
            panic!("not a usage command");
        };
        assert_eq!(bucket, Bucket::Day);
        assert_eq!(window, Window::Days30);
        assert_eq!(group, Group::None);
        assert_eq!(repo, None);
        assert!(!json);
    }

    #[test]
    fn usage_accepts_every_documented_bucket_window_and_group() {
        use crate::usage::{Bucket, Group, Window};
        for (token, expected) in [
            ("day", Bucket::Day),
            ("week", Bucket::Week),
            ("month", Bucket::Month),
        ] {
            let Command::Usage { bucket, .. } = parse(&["usage", "--by", token]) else {
                panic!("not a usage command");
            };
            assert_eq!(bucket, expected, "{token}");
        }
        for token in ["7d", "30d", "90d", "all"] {
            let Command::Usage { window, .. } = parse(&["usage", "--window", token]) else {
                panic!("not a usage command");
            };
            // The flag value is the same token the JSON API uses.
            assert_eq!(serde_json::to_value(window).unwrap(), token, "{token}");
        }
        assert!(matches!(
            parse(&["usage", "--window", "7d"]),
            Command::Usage {
                window: Window::Days7,
                ..
            }
        ));
        for (token, expected) in [
            ("none", Group::None),
            ("engine", Group::Engine),
            ("repo", Group::Repo),
            ("pr", Group::Pr),
            ("session", Group::Session),
        ] {
            let Command::Usage { group, .. } = parse(&["usage", "--group", token]) else {
                panic!("not a usage command");
            };
            assert_eq!(group, expected, "{token}");
        }
        for bad in [
            &["usage", "--by", "year"][..],
            &["usage", "--window", "14d"],
            &["usage", "--group", "author"],
            &["usage", "--bucket", "day"],
        ] {
            let kind = parse_err(bad);
            assert!(
                matches!(
                    kind,
                    ErrorKind::ValueValidation
                        | ErrorKind::InvalidValue
                        | ErrorKind::UnknownArgument
                ),
                "{bad:?}: {kind:?}"
            );
        }
    }

    #[test]
    fn config_alone_means_show_and_set_needs_key_and_value() {
        assert!(matches!(parse(&["config"]), Command::Config { cmd: None }));
        assert!(matches!(
            parse(&["config", "show"]),
            Command::Config {
                cmd: Some(ConfigCmd::Show)
            }
        ));
        let Command::Config {
            cmd: Some(ConfigCmd::Set { key, value }),
        } = parse(&["config", "set", "repos.org/repo.engine", "claude"])
        else {
            panic!("not a config set command");
        };
        assert_eq!(key, "repos.org/repo.engine");
        assert_eq!(value, "claude");

        assert_eq!(
            parse_err(&["config", "set"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["config", "set", "defaults.engine"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["config", "set", "a.b", "c", "d"]),
            ErrorKind::UnknownArgument
        );
        assert_eq!(parse_err(&["config", "get"]), ErrorKind::InvalidSubcommand);
    }

    /// A value that starts with a dash (an agent flag, a negative number)
    /// needs `--` so it is not read as an option of prx.
    #[test]
    fn config_set_takes_dash_values_after_a_double_dash() {
        assert_eq!(
            parse_err(&["config", "set", "engines.pi.args[1]", "-p"]),
            ErrorKind::UnknownArgument
        );
        let Command::Config {
            cmd: Some(ConfigCmd::Set { key, value }),
        } = parse(&["config", "set", "engines.pi.args[1]", "--", "-p"])
        else {
            panic!("not a config set command");
        };
        assert_eq!(key, "engines.pi.args[1]");
        assert_eq!(value, "-p");
    }

    #[test]
    fn repos_add_and_remove_require_a_name() {
        let Command::Repos {
            cmd: ReposCmd::Add { repo },
        } = parse(&["repos", "add", "org/repo"])
        else {
            panic!("not a repos add command");
        };
        assert_eq!(repo, "org/repo");
        assert_eq!(
            parse_err(&["repos", "add"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["repos", "remove"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            parse_err(&["repos", "list", "org/repo"]),
            ErrorKind::UnknownArgument
        );
    }

    #[test]
    fn daemon_start_detaches_unless_foreground() {
        assert!(matches!(
            parse(&["daemon", "start"]),
            Command::Daemon {
                cmd: DaemonCmd::Start { foreground: false }
            }
        ));
        assert!(matches!(
            parse(&["daemon", "start", "--foreground"]),
            Command::Daemon {
                cmd: DaemonCmd::Start { foreground: true }
            }
        ));
        assert_eq!(
            parse_err(&["daemon", "stop", "--foreground"]),
            ErrorKind::UnknownArgument
        );
    }

    #[test]
    fn daemon_install_options_default_to_resolving_everything() {
        let Command::Daemon {
            cmd:
                DaemonCmd::Install {
                    binary_path,
                    include_menubar,
                    app,
                },
        } = parse(&["daemon", "install"])
        else {
            panic!("not a daemon install command");
        };
        assert_eq!(binary_path, None);
        assert!(!include_menubar);
        assert_eq!(app, None);

        let Command::Daemon {
            cmd:
                DaemonCmd::Install {
                    binary_path,
                    include_menubar,
                    app,
                },
        } = parse(&[
            "daemon",
            "install",
            "--binary-path",
            "/opt/prx/bin/prx",
            "--include-menubar",
            "--app",
            "/Applications/PRX.app",
        ])
        else {
            panic!("not a daemon install command");
        };
        assert_eq!(binary_path, Some(PathBuf::from("/opt/prx/bin/prx")));
        assert!(include_menubar);
        assert_eq!(app, Some(PathBuf::from("/Applications/PRX.app")));
        assert_eq!(
            parse_err(&["daemon", "install", "--binary-path"]),
            ErrorKind::InvalidValue
        );
    }
}
