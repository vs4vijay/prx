//! Entry point and command dispatch. All logic lives in the `prx` library.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::Parser;

use prx::cli::{Command, ConfigCmd, DaemonCmd, ReposCmd, StatusArgs};
use prx::config::{validate_repo_name, Config, RepoConfig};
use prx::effects::EffectsDeps;
use prx::history::HistoryEntry;
use prx::machine::{Event, MachineCtx};
use prx::notify::CommandNotifier;
use prx::state::{PrRecord, StateStore};
use prx::{
    autostart, config, config_edit, daemon, history, insights, ipc, logging, paths, pause, review,
    status,
};

#[tokio::main]
async fn main() {
    let cli = prx::cli::Cli::parse();
    // The daemon logs to rotating files (A15) + stderr in the foreground;
    // every other command logs to stderr only. The guard must outlive main.
    let _log_guard = match &cli.command {
        Command::Daemon {
            cmd: DaemonCmd::Start { foreground },
        } => {
            let child = std::env::var_os("PRX_DAEMON_CHILD").is_some();
            Some(logging::init_daemon(*foreground && !child))
        }
        _ => {
            logging::init();
            None
        }
    };
    if let Err(e) = run(cli.command).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn run(command: Command) -> Result<()> {
    match command {
        Command::Init => cmd_init(),
        Command::Doctor => cmd_doctor().await,
        Command::Repos { cmd } => match cmd {
            ReposCmd::Add { repo } => cmd_repos_add(&repo),
            ReposCmd::List => cmd_repos_list(),
            ReposCmd::Remove { repo } => cmd_repos_remove(&repo),
        },
        Command::Status(args) => cmd_status(args).await,
        Command::Review {
            target,
            all,
            dry_run,
            engine,
            no_post,
            prompt,
        } => {
            let overrides = review::ReviewOverrides {
                engine,
                post: no_post.then_some(false),
                extra_prompt: prompt,
            }
            .normalized();
            cmd_review(target.as_deref(), all, dry_run, &overrides).await
        }
        Command::Brief { target, regenerate } => review::run_brief_cmd(&target, regenerate).await,
        Command::Approve { target } => cmd_approve(&target).await,
        Command::Snooze {
            target,
            hours,
            until_push,
        } => cmd_snooze(&target, hours, until_push).await,
        Command::Ignore { target } => cmd_ignore(&target).await,
        Command::Unignore { target } => cmd_unignore(&target).await,
        Command::ReviewNow => cmd_review_now().await,
        Command::Pause { hours, until } => cmd_pause(hours, until.as_deref()).await,
        Command::Resume => cmd_resume().await,
        Command::History { repo, limit, json } => cmd_history(repo.as_deref(), limit, json).await,
        Command::Insights { repo, days } => cmd_insights(repo.as_deref(), days).await,
        Command::Usage {
            bucket,
            window,
            group,
            repo,
            json,
        } => {
            let query = prx::usage::Query {
                bucket,
                window,
                group,
                repo: repo.filter(|r| !r.trim().is_empty()),
            };
            cmd_usage(&query, json).await
        }
        Command::Config { cmd } => match cmd {
            None | Some(ConfigCmd::Show) => cmd_config_show(),
            Some(ConfigCmd::Edit) => cmd_config_edit(),
            Some(ConfigCmd::Set { key, value }) => cmd_config_set(&key, &value),
        },
        Command::Daemon { cmd } => match cmd {
            DaemonCmd::Start { foreground } => {
                if foreground {
                    daemon::run_foreground().await
                } else {
                    daemon::start_detached()
                }
            }
            DaemonCmd::Stop => daemon::run_stop().await,
            DaemonCmd::Status => {
                daemon::run_status_cmd();
                Ok(())
            }
            DaemonCmd::Logs => daemon::run_logs_cmd(),
            DaemonCmd::Install {
                binary_path,
                include_menubar,
                app,
            } => autostart::install(binary_path.as_deref(), include_menubar, app.as_deref()),
            DaemonCmd::Uninstall => autostart::uninstall(),
        },
    }
}

// ------------------------------------------------------ daemon-backed CLI
//
// Mutating commands act as thin clients when a live daemon answers (A4:
// ALL state + GitHub writes flow through the daemon) and fall back to
// in-process execution with a warning when it does not.

async fn cmd_review(
    target: Option<&str>,
    all: bool,
    dry_run: bool,
    overrides: &review::ReviewOverrides,
) -> Result<()> {
    // Mirror run_review_cmd's argument validation so the daemon path cannot
    // turn a bare `prx review` into an accidental `--all`.
    match (target, all) {
        (Some(_), true) => bail!("specify either a PR target or --all, not both"),
        (None, false) => bail!("specify a PR target (org/repo#123) or --all"),
        _ => {}
    }
    // Dry-run makes no writes, so it never needs the daemon.
    if !dry_run {
        if let Some(client) = ipc::DaemonClient::discover().await {
            // A22: an explicit target bypasses cooldowns (force); --all
            // reviews only currently-due PRs. D10: the target goes through
            // unparsed — the daemon resolves every reference form.
            let resp = match target {
                Some(target) if !all => client.review_with(target, true, overrides).await?,
                _ => client.review(target, all, target.is_some()).await?,
            };
            if resp.enqueued.is_empty() {
                println!(
                    "{}",
                    resp.note
                        .as_deref()
                        .unwrap_or("nothing was enqueued for review")
                );
            } else {
                for key in &resp.enqueued {
                    println!("queued {key} for review (the daemon will run it)");
                }
                // D5: e.g. "reviews paused …; this explicit review runs anyway".
                if let Some(note) = &resp.note {
                    println!("note: {note}");
                }
            }
            return Ok(());
        }
        eprintln!("daemon not running — running in-process");
        // D5: the in-process path honors the persisted pause too.
        let now = Utc::now();
        if let Some(until) = load_active_pause(now) {
            if all {
                println!(
                    "{}; nothing was reviewed (review a single PR explicitly, or `prx resume`)",
                    pause::describe(until, now)
                );
                return Ok(());
            }
            println!(
                "note: {}; this explicit review runs anyway",
                pause::describe(until, now)
            );
        }
    }
    review::run_review_cmd(target, all, dry_run, overrides).await
}

async fn cmd_approve(target: &str) -> Result<()> {
    if let Some(client) = ipc::DaemonClient::discover().await {
        let resp = client.approve(target).await?;
        if resp.changed {
            println!("approved {target}");
        } else {
            println!(
                "{}",
                resp.note
                    .unwrap_or_else(|| format!("{target}: nothing to do"))
            );
        }
        return Ok(());
    }
    eprintln!("daemon not running — running in-process");
    review::run_approve_cmd(target).await
}

async fn cmd_snooze(target: &str, hours: Option<f64>, until_push: bool) -> Result<()> {
    if let Some(client) = ipc::DaemonClient::discover().await {
        let resp = client.snooze(target, hours, until_push).await?;
        print_mutation(target, &resp);
        return Ok(());
    }
    eprintln!("daemon not running — running in-process");
    snooze_inprocess(target, hours, until_push).await
}

async fn cmd_ignore(target: &str) -> Result<()> {
    if let Some(client) = ipc::DaemonClient::discover().await {
        let resp = client.ignore(target).await?;
        print_mutation(target, &resp);
        return Ok(());
    }
    eprintln!("daemon not running — running in-process");
    manual_event_inprocess(target, Event::Ignore).await
}

async fn cmd_unignore(target: &str) -> Result<()> {
    if let Some(client) = ipc::DaemonClient::discover().await {
        let resp = client.unignore(target).await?;
        print_mutation(target, &resp);
        return Ok(());
    }
    eprintln!("daemon not running — running in-process");
    manual_event_inprocess(target, Event::Unignore).await
}

async fn cmd_review_now() -> Result<()> {
    if let Some(client) = ipc::DaemonClient::discover().await {
        let resp = client.review_now().await?;
        match resp.note.filter(|note| note.contains("paused")) {
            // D5: paused, the daemon polls but enqueues nothing.
            Some(note) => println!("{note}"),
            None => println!("triggered poll + due-scan on the daemon"),
        }
        return Ok(());
    }
    eprintln!("daemon not running — running in-process");
    let now = Utc::now();
    if let Some(until) = load_active_pause(now) {
        println!(
            "{}; nothing was reviewed (`prx resume` to lift it)",
            pause::describe(until, now)
        );
        return Ok(());
    }
    // Same semantics as `prx review --all`: poll everything, review what's due.
    review::run_review_cmd(None, true, false, &review::ReviewOverrides::default()).await
}

/// D5: the pause end in effect at `now` per state.json (in-process paths).
fn load_active_pause(now: chrono::DateTime<Utc>) -> Option<chrono::DateTime<Utc>> {
    StateStore::new(paths::state_file())
        .load()
        .ok()
        .and_then(|state| state.active_pause(now))
}

/// `prx pause` (D5): daemon-first, with an in-process fallback that writes
/// the pause straight into state.json (the next daemon start honors it).
async fn cmd_pause(hours: Option<f64>, until: Option<&str>) -> Result<()> {
    let now = Utc::now();
    let until = until.map(|u| pause::parse_until(u, now)).transpose()?;
    // Validate up front so both paths reject both/neither identically.
    let end = pause::resolve_pause_until(hours, until, now)?;
    if let Some(client) = ipc::DaemonClient::discover().await {
        let resp = client.pause(hours, until).await?;
        let end = resp.paused_until.unwrap_or(end);
        println!(
            "{} — polling continues; `prx resume` to lift it",
            pause::describe(end, Utc::now())
        );
        return Ok(());
    }
    eprintln!("daemon not running — running in-process");
    let store = StateStore::new(paths::state_file());
    let mut state = store.load().context("failed to load PR state")?;
    state.paused_until = Some(end);
    store.save(&state).context("failed to save PR state")?;
    println!(
        "{} — `prx resume` to lift it",
        pause::describe(end, Utc::now())
    );
    Ok(())
}

/// `prx resume` (D5): daemon-first, in-process fallback.
async fn cmd_resume() -> Result<()> {
    if let Some(client) = ipc::DaemonClient::discover().await {
        let resp = client.resume().await?;
        println!(
            "{}",
            if resp.changed {
                "reviews resumed"
            } else {
                "reviews were not paused"
            }
        );
        return Ok(());
    }
    eprintln!("daemon not running — running in-process");
    let store = StateStore::new(paths::state_file());
    let mut state = store.load().context("failed to load PR state")?;
    let was_paused = state.is_paused(Utc::now());
    if state.paused_until.take().is_some() {
        store.save(&state).context("failed to save PR state")?;
    }
    println!(
        "{}",
        if was_paused {
            "reviews resumed"
        } else {
            "reviews were not paused"
        }
    );
    Ok(())
}

async fn cmd_status(args: StatusArgs) -> Result<()> {
    if let Some(client) = ipc::DaemonClient::discover().await {
        let payload = client.get_status().await?;
        let mut prs = payload.prs;
        if let Some(repo) = &args.repo {
            let prefix = format!("{repo}#");
            prs.retain(|e| e.key.starts_with(&prefix));
        }
        if args.json {
            println!(
                "{}",
                serde_json::to_string_pretty(&prs).context("failed to serialize status JSON")?
            );
        } else {
            // D5: pause and mode `off` are separate lines (different gates).
            for line in pause::status_lines(payload.paused_until, payload.review_mode, Utc::now()) {
                println!("{line}");
            }
            status::print_table(&prs, payload.last_state_poll);
        }
        return Ok(());
    }
    status::run_status(args).await
}

async fn cmd_history(repo: Option<&str>, limit: usize, json: bool) -> Result<()> {
    // Daemon-first (A4), with an in-process fallback for daemon-less use.
    let entries = if let Some(client) = ipc::DaemonClient::discover().await {
        client.history(repo, limit).await?.entries
    } else {
        eprintln!("daemon not running — reading history in-process");
        history::query(&paths::history_file(), repo, limit)?
    };
    if entries.is_empty() {
        println!("no history yet");
        return Ok(());
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&entries).context("failed to serialize history JSON")?
        );
    } else {
        print_history_timeline(&entries);
    }
    Ok(())
}

fn print_history_timeline(entries: &[HistoryEntry]) {
    println!("recent review history (newest first):");
    for entry in entries {
        println!("  {}", format_history_entry(entry));
    }
}

/// One timeline line: `<ts> <icon> <key> <what> · <title>`.
fn format_history_entry(entry: &HistoryEntry) -> String {
    use HistoryEntry as H;
    let ts = entry.ts().format("%Y-%m-%d %H:%M");
    let key = format!("{}#{}", entry.repo(), entry.number());
    let title = truncate(&entry_title(entry), 36);
    match entry {
        H::ReviewCompleted {
            verdict,
            blocking_count,
            nit_count,
            agent,
            duration_seconds,
            posted,
            usage,
            summary_usage,
            ..
        } => {
            let verdict_text = match verdict {
                prx::state::Verdict::Approved => "approved",
                prx::state::Verdict::ChangesRequested => "changes requested",
                prx::state::Verdict::CommentOnly => "commented",
            };
            // D4: a run that made no GitHub writes says so.
            let verdict_text = match posted {
                Some(false) => format!("{verdict_text} (not posted)"),
                _ => verdict_text.to_string(),
            };
            let agent = agent
                .as_deref()
                .map(|a| format!(" · {a}"))
                .unwrap_or_default();
            let duration = duration_seconds
                .map(|d| format!(" · {d}s"))
                .unwrap_or_default();
            let tokens = tokens_text(&[usage.as_ref(), summary_usage.as_ref()]);
            let key = format!("{key:<16}");
            format!(
                "{} {} {}  {} — {} blocking, {} nits{}{}{} · {}",
                ts,
                entry.icon(),
                key,
                verdict_text,
                blocking_count,
                nit_count,
                agent,
                duration,
                tokens,
                title
            )
        }
        H::ReviewStarted { .. } => {
            format!(
                "{} {} {:<16} review started · {}",
                ts,
                entry.icon(),
                key,
                title
            )
        }
        H::ReviewFailed { usage, .. } => {
            format!(
                "{} {} {:<16} review failed{} · {}",
                ts,
                entry.icon(),
                key,
                tokens_text(&[usage.as_ref()]),
                title
            )
        }
        H::ReviewSkipped { reason, .. } => {
            format!(
                "{} {} {:<16} review skipped ({reason}) · {}",
                ts,
                entry.icon(),
                key,
                title
            )
        }
        H::PrMerged { .. } => format!("{} {} {:<16} merged · {}", ts, entry.icon(), key, title),
        H::PrClosed { .. } => format!("{} {} {:<16} closed · {}", ts, entry.icon(), key, title),
        H::NewCommits { .. } => {
            format!(
                "{} {} {:<16} new commits · {}",
                ts,
                entry.icon(),
                key,
                title
            )
        }
        H::Brief { brief, usage, .. } => {
            format!(
                "{} {} {:<16} brief {}{} · {}",
                ts,
                entry.icon(),
                key,
                if brief.is_some() { "written" } else { "failed" },
                tokens_text(&[usage.as_ref()]),
                title
            )
        }
    }
}

/// ` · 12.3k in / 1.1k out` (plus cache reads and the cost when reported)
/// for a run's review + summary usage; empty when no usage is known
/// (design D11: unknown is never shown as zeros).
fn tokens_text(usages: &[Option<&prx::usage::TokenUsage>]) -> String {
    let known: Vec<&prx::usage::TokenUsage> = usages
        .iter()
        .flatten()
        .copied()
        .filter(|u| u.has_counts())
        .collect();
    if known.is_empty() {
        return String::new();
    }
    let sum = |f: fn(&prx::usage::TokenUsage) -> Option<u64>| -> Option<u64> {
        known.iter().filter_map(|u| f(u)).reduce(|a, b| a + b)
    };
    let mut parts = Vec::new();
    if let Some(n) = sum(|u| u.input_tokens) {
        parts.push(format!("{} in", prx::usage::format_tokens(n)));
    }
    if let Some(n) = sum(|u| u.output_tokens) {
        parts.push(format!("{} out", prx::usage::format_tokens(n)));
    }
    if let Some(n) = sum(|u| u.cache_read_tokens) {
        parts.push(format!("{} cached", prx::usage::format_tokens(n)));
    }
    let mut text = match parts.is_empty() {
        true => String::new(),
        false => format!(" · {}", parts.join(" / ")),
    };
    if let Some(cost) = known.iter().filter_map(|u| u.cost_usd).reduce(|a, b| a + b) {
        text.push_str(&format!(" · {}", prx::usage::format_cost(cost)));
    }
    text
}

fn entry_title(entry: &HistoryEntry) -> String {
    match entry {
        HistoryEntry::ReviewStarted { title, .. }
        | HistoryEntry::ReviewCompleted { title, .. }
        | HistoryEntry::ReviewFailed { title, .. }
        | HistoryEntry::ReviewSkipped { title, .. }
        | HistoryEntry::PrMerged { title, .. }
        | HistoryEntry::PrClosed { title, .. }
        | HistoryEntry::NewCommits { title, .. }
        | HistoryEntry::Brief { title, .. } => title.clone(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}\u{2026}")
    }
}

async fn cmd_insights(repo: Option<&str>, days: Option<u64>) -> Result<()> {
    if days == Some(0) {
        bail!("--days must be at least 1");
    }
    // Daemon-first: the same math runs against the daemon's history file
    // (A4); the fallback computes directly from the local file.
    let report = if let Some(client) = ipc::DaemonClient::discover().await {
        client.insights(repo, days).await?
    } else {
        eprintln!("daemon not running — computing insights in-process");
        let entries = history::query(&paths::history_file(), repo, 0)?;
        insights::compute(&entries, Utc::now(), days)
    };
    print!("{}", insights::render(&report));
    Ok(())
}

async fn cmd_usage(query: &prx::usage::Query, json: bool) -> Result<()> {
    // Daemon-first like `prx history` (A4); the fallback aggregates the
    // local history file with the same function (design D13).
    let report = if let Some(client) = ipc::DaemonClient::discover().await {
        client.usage(query).await?
    } else {
        eprintln!("daemon not running — reading history in-process");
        let entries = history::query(&paths::history_file(), query.repo.as_deref(), 0)?;
        prx::usage::compute(&entries, Utc::now(), query)
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).context("failed to serialize usage JSON")?
        );
    } else {
        print!("{}", prx::usage::render(&report));
    }
    Ok(())
}

fn print_mutation(target: &str, resp: &ipc::MutationResponse) {
    if resp.changed {
        println!(
            "{}",
            resp.note
                .clone()
                .unwrap_or_else(|| format!("{target}: done"))
        );
    } else {
        println!(
            "{}",
            resp.note
                .clone()
                .unwrap_or_else(|| format!("{target}: nothing to do"))
        );
    }
}

// ------------------------------------------- in-process manual mutations
//
// Fallback path when the daemon is down (best-effort per DEV-PLAN §9): the
// machine events apply directly to state.json.

fn load_config_and_repo(repo_name: &str) -> Result<(Config, prx::config::EffectiveRepo)> {
    let config = Config::load(&paths::config_path()).context("configuration is required")?;
    config.validate().context("configuration is invalid")?;
    let repo = config
        .effective_repos()
        .context("configuration is invalid")?
        .into_iter()
        .find(|r| r.name == repo_name)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "repo '{repo_name}' is not configured; add it with `prx repos add {repo_name}`"
            )
        })?;
    Ok((config, repo))
}

fn require_tracked(store: &StateStore, key: &str) -> Result<PrRecord> {
    let state = store.load().context("failed to load PR state")?;
    state
        .prs
        .get(key)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("{key} is not tracked yet; run `prx status` first"))
}

async fn snooze_inprocess(target: &str, hours: Option<f64>, until_push: bool) -> Result<()> {
    if hours.is_none() && !until_push {
        bail!("specify --hours N or --until-push");
    }
    if hours.is_some_and(|h| h <= 0.0) {
        bail!("--hours must be > 0");
    }
    let (repo_name, number) = review::parse_pr_key(target)?;
    let (config, repo) = load_config_and_repo(&repo_name)?;
    let key = PrRecord::key_for(&repo_name, number);
    let store = StateStore::new(paths::state_file());
    let before = require_tracked(&store, &key)?;

    let until =
        hours.map(|h| Utc::now() + chrono::Duration::milliseconds((h * 3_600_000.0) as i64));
    let changed =
        apply_manual_event_direct(&config, &repo, &key, Event::Snooze { until, until_push })
            .await?;

    let after = require_tracked(&store, &key)?;
    if !changed {
        println!("{key}: nothing to do (current state {:?})", before.state);
        return Ok(());
    }
    match (after.snooze_until, after.snooze_until_push) {
        (Some(until), _) => println!("{key} snoozed until {}", until.format("%Y-%m-%d %H:%M UTC")),
        (None, true) => println!("{key} snoozed until next push"),
        _ => println!("{key} snoozed"),
    }
    Ok(())
}

async fn manual_event_inprocess(target: &str, event: Event) -> Result<()> {
    let (repo_name, number) = review::parse_pr_key(target)?;
    let (config, repo) = load_config_and_repo(&repo_name)?;
    let key = PrRecord::key_for(&repo_name, number);
    let store = StateStore::new(paths::state_file());
    let before = require_tracked(&store, &key)?;
    let label = match &event {
        Event::Ignore => format!("{key} ignored"),
        Event::Unignore => format!("{key} un-ignored (pending, cooldown restarted)"),
        _ => format!("{key}: done"),
    };
    let changed = apply_manual_event_direct(&config, &repo, &key, event).await?;
    if !changed {
        println!("{key}: nothing to do (current state {:?})", before.state);
        return Ok(());
    }
    println!("{label}");
    Ok(())
}

/// Apply one manual event to state.json directly (daemon-down fallback):
/// machine transition + effects (notify/history) + atomic save. Returns
/// whether the record changed.
async fn apply_manual_event_direct(
    config: &Config,
    repo: &prx::config::EffectiveRepo,
    key: &str,
    event: Event,
) -> Result<bool> {
    let store = StateStore::new(paths::state_file());
    let mut state = store.load().context("failed to load PR state")?;
    let before = state.prs.get(key).cloned();
    let notifier = CommandNotifier;
    let deps = EffectsDeps::for_repo(&notifier, &config.notification_tiers, repo);
    let skip = repo.skip.clone();
    let ctx = MachineCtx {
        now: Utc::now(),
        skip: &skip,
        resume_sessions: repo.resolved_engine.can_resume(),
    };
    status::apply_and_store(&mut state, key, event, &ctx, &deps).await;
    let after = state.prs.get(key).cloned();
    store.save(&state).context("failed to save PR state")?;
    Ok(before != after)
}

// ------------------------------------------------------------------ init

fn cmd_init() -> Result<()> {
    let config_path = paths::config_path();
    let config_dir = paths::config_dir();
    let data_dir = paths::data_dir();

    std::fs::create_dir_all(&config_dir)
        .with_context(|| format!("cannot create config dir {}", config_dir.display()))?;
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("cannot create data dir {}", data_dir.display()))?;

    // One definition of the default config, shared with PRX.app's first
    // launch (`prx::config::ensure_starter_config`); never overwrites.
    if prx::config::ensure_starter_config(&config_path)? {
        println!("wrote config: {}", config_path.display());
    } else {
        println!("config already exists: {}", config_path.display());
    }

    // Design F5: the review prompt is config text (`default` = built in),
    // so init writes no prompt file.
    println!("review prompt: built-in default (customize defaults.review_prompt with prompt text)");

    println!("\nnext steps:");
    println!("  prx repos add <org/repo>   # watch a repository");
    println!("  prx doctor                 # verify gh, engines, and config");
    println!("  prx status                 # see your open PRs");
    Ok(())
}

// ---------------------------------------------------------------- doctor

struct Check {
    name: String,
    ok: bool,
    detail: String,
    /// True for informational warnings (rendered WARN, never counted as a
    /// failure — used by checks that must not fail the run by themselves).
    warning: bool,
}

fn check(name: impl Into<String>, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        ok,
        detail: detail.into(),
        warning: false,
    }
}

/// A warning line: never counts toward doctor's failure count.
fn check_warn(name: impl Into<String>, detail: impl Into<String>) -> Check {
    let mut c = check(name, true, detail);
    c.warning = true;
    c
}

/// The PRX.app login-item line (A4): the active registration and whether
/// the registered app still exists. Informational; never fails the run.
fn app_autostart_check(st: &autostart::Status) -> Check {
    let name = "autostart";
    let path = st
        .path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let Some(bundle) = st
        .binary
        .as_deref()
        .and_then(autostart::bundle_of_executable)
    else {
        return check_warn(
            name,
            format!("PRX.app login item ({path}) — registered app could not be parsed"),
        );
    };
    if !autostart::is_app_bundle(&bundle) {
        return check_warn(
            name,
            format!(
                "PRX.app login item ({path}) — registered app {} no longer exists \
                 (reinstall PRX.app, then re-run `prx daemon install`)",
                bundle.display()
            ),
        );
    }
    check(
        name,
        true,
        format!("PRX.app login item ({path}) — app: {}", bundle.display()),
    )
}

/// One autostart registration line (D5) — the daemon and menubar agents are
/// reported identically: installed with the registered binary path, or not
/// installed. A missing registration is a hint (ok); a stale binary or a
/// registration pointing into a build directory is a warning. Malformed
/// registration files are reported, never a crash. None of these fails the
/// run by itself.
fn autostart_check(name: &str, install_hint: &str, st: &autostart::Status) -> Check {
    if !st.installed {
        return check(name, true, format!("not installed ({install_hint})"));
    }
    let path = st
        .path
        .as_deref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    match &st.binary {
        None => check_warn(
            name,
            format!("installed ({path}) — registered binary could not be parsed"),
        ),
        Some(binary) if !binary.exists() => check_warn(
            name,
            format!(
                "installed ({path}) — registered binary {} no longer exists \
                 (stale install; re-run `prx daemon install`)",
                binary.display()
            ),
        ),
        Some(binary) if autostart::is_in_target_dir(binary) => check_warn(
            name,
            format!(
                "installed ({path}) — binary {} lives in a build directory; the \
                 registration breaks on `cargo clean` or a moved checkout — run \
                 `cargo install --path .` and re-run `prx daemon install` (or pass \
                 `--binary-path` with the stable binary)",
                binary.display()
            ),
        ),
        Some(binary) => check(
            name,
            true,
            format!("installed ({path}) — binary: {}", binary.display()),
        ),
    }
}

async fn cmd_doctor() -> Result<()> {
    let mut checks: Vec<Check> = Vec::new();

    // Config file parses and validates.
    let config_path = paths::config_path();
    let config = match Config::load(&config_path) {
        Ok(cfg) => match cfg.validate() {
            Ok(()) => {
                checks.push(check(
                    "config",
                    true,
                    format!(
                        "{} parsed ({} repos)",
                        config_path.display(),
                        cfg.repos.len()
                    ),
                ));
                Some(cfg)
            }
            Err(e) => {
                checks.push(check("config", false, e.to_string()));
                None
            }
        },
        Err(e) => {
            checks.push(check("config", false, e.to_string()));
            None
        }
    };
    // Design B5: the deprecated `executive_summary` section still loads (as
    // `brief`); say so, informationally.
    if let Ok(raw) = std::fs::read_to_string(&config_path) {
        for notice in config::legacy_brief_notices(&raw) {
            let notice = notice.trim_start_matches("config: ");
            checks.push(check("config", true, format!("hint: {notice}")));
        }
    }

    // gh binary + auth.
    match paths::find_in_path("gh") {
        Some(gh_path) => {
            checks.push(check("gh binary", true, gh_path.display().to_string()));
            let auth = prx::github::gh_auth_status(gh_path.to_string_lossy().as_ref()).await;
            let detail = match &auth.user {
                Some(user) => format!("authenticated as {user}"),
                None if auth.ok => "authenticated".to_string(),
                None => first_line(&auth.detail).to_string(),
            };
            checks.push(check("gh auth", auth.ok, detail));
        }
        None => {
            checks.push(check("gh binary", false, "gh not found on PATH"));
            checks.push(check("gh auth", false, "skipped (gh not found)"));
        }
    }

    // Engines referenced by the config (design D1): every level that names
    // one, plus executive-summary engines. Agent engines must resolve to a
    // binary; an llm_api engine whose key variable is unset is a warning
    // (the key is read at call time, possibly from the daemon's own env).
    let mut engine_refs: Vec<(String, String)> = Vec::new();
    if let Some(cfg) = &config {
        engine_refs.push((
            format!("defaults.engine = {}", cfg.defaults.engine),
            cfg.defaults.engine.clone(),
        ));
        let levels = cfg
            .orgs
            .iter()
            .map(|o| (format!("orgs[{}]", o.org), &o.overrides))
            .chain(cfg.repos.iter().map(|r| (r.name.clone(), &r.overrides)));
        for (level, overrides) in levels {
            if let Some(engine) = &overrides.engine {
                engine_refs.push((format!("{level}: engine = {engine}"), engine.clone()));
            }
        }
        let summary_engines =
            std::iter::once(("defaults".to_string(), cfg.defaults.brief.engine.as_ref()))
                .chain(cfg.orgs.iter().map(|o| {
                    (
                        format!("orgs[{}]", o.org),
                        o.overrides.brief.as_ref().and_then(|e| e.engine.as_ref()),
                    )
                }))
                .chain(cfg.repos.iter().map(|r| {
                    (
                        r.name.clone(),
                        r.overrides.brief.as_ref().and_then(|e| e.engine.as_ref()),
                    )
                }));
        for (level, engine) in summary_engines {
            if let Some(engine) = engine {
                engine_refs.push((format!("{level}: brief.engine = {engine}"), engine.clone()));
            }
        }
    }
    if engine_refs.is_empty() {
        checks.push(check("engines", false, "skipped (config failed)"));
    }
    for (source, key) in engine_refs {
        let label = format!("engine ({source})");
        match config.as_ref().and_then(|cfg| cfg.resolve_engine(&key)) {
            Some(config::EngineConfig::Agent(agent)) => match paths::find_in_path(&agent.command) {
                Some(path) => {
                    checks.push(check(label, true, format!("{key} -> {}", path.display())))
                }
                None => checks.push(check(
                    label,
                    false,
                    format!(
                        "agent '{key}' command '{}' not found on PATH",
                        agent.command
                    ),
                )),
            },
            Some(config::EngineConfig::LlmApi(llm)) => {
                let target = format!(
                    "{key} -> llm_api {} model {}",
                    serde_plain_token(&llm.provider),
                    llm.model
                );
                if prx::discover::env_present(&llm.api_key_env) {
                    checks.push(check(
                        label,
                        true,
                        format!("{target} (key from ${})", llm.api_key_env),
                    ));
                } else {
                    checks.push(check_warn(
                        label,
                        format!(
                            "{target}: ${} is not set in this environment; reviews with this \
                             engine fail unless the daemon's environment provides it",
                            llm.api_key_env
                        ),
                    ));
                }
            }
            None => checks.push(check(
                label,
                false,
                format!("engine '{key}' is not defined (built-in or under engines:)"),
            )),
        }
    }

    // Cron schedules (design D4): each repo with an effective schedule and
    // its next fire, in local time as the daemon evaluates it.
    match config.as_ref().map(|cfg| cfg.effective_repos()) {
        Some(Ok(repos)) => {
            let now = Utc::now();
            for repo in repos {
                let Some(expr) = &repo.schedule else { continue };
                match config::parse_schedule(expr) {
                    Ok(_) => match prx::daemon::next_schedule_tick(expr, now) {
                        Some(next) => checks.push(check(
                            "schedule",
                            true,
                            format!(
                                "{}: '{expr}' next fire {}",
                                repo.name,
                                next.with_timezone(&chrono::Local)
                                    .format("%Y-%m-%d %H:%M %Z")
                            ),
                        )),
                        None => checks.push(check_warn(
                            "schedule",
                            format!("{}: '{expr}' never fires again", repo.name),
                        )),
                    },
                    Err(e) => checks.push(check(
                        "schedule",
                        false,
                        format!("{}: invalid schedule '{expr}': {e}", repo.name),
                    )),
                }
            }
        }
        Some(Err(e)) => checks.push(check("schedule", false, e.to_string())),
        None => {}
    }

    // Browser resolvability (design D9): every distinct effective browser.
    // An unlaunchable one is a warning: opens fall back to the system
    // browser.
    if let Some(cfg) = &config {
        let mut browsers: Vec<(String, config::Browser)> = Vec::new();
        let levels = cfg
            .effective_defaults()
            .into_iter()
            .chain(cfg.effective_repos().unwrap_or_default());
        for eff in levels {
            match browsers.iter_mut().find(|(_, b)| *b == eff.browser) {
                Some((names, _)) => {
                    names.push_str(", ");
                    names.push_str(&eff.name);
                }
                None => browsers.push((eff.name.clone(), eff.browser.clone())),
            }
        }
        for (names, browser) in browsers {
            match browser_launchable(&browser) {
                Ok(detail) => checks.push(check("browser", true, format!("{names}: {detail}"))),
                Err(problem) => checks.push(check_warn(
                    "browser",
                    format!("{names}: {problem}; PRs open in the system browser instead"),
                )),
            }
        }
    }

    // Review prompts are inline text (design F5): report which levels use
    // the built-in default vs custom text. Legacy path values already fail
    // config validation above.
    match &config {
        Some(cfg) => {
            let describe = |value: &str| {
                if config::is_default_prompt(value) {
                    "built-in default".to_string()
                } else if prx::prompt::is_legacy_default_prompt(value) {
                    "built-in default (saved copy of the previous default)".to_string()
                } else {
                    format!("custom text ({} lines)", value.lines().count())
                }
            };
            // Design C1: the output contract is appended to every prompt,
            // so a custom prompt that still carries its own manifest schema
            // gets an informational hint (never a failure; reviews run).
            let schema_hint = |level: &str, value: &str| {
                let custom = !config::is_default_prompt(value)
                    && !prx::prompt::is_legacy_default_prompt(value);
                (custom && prx::prompt::has_own_manifest_schema(value)).then(|| {
                    check(
                        "prompts",
                        true,
                        format!(
                            "hint: {level} review_prompt has its own manifest schema; \
                             output instructions are now added automatically and can be \
                             removed from the prompt"
                        ),
                    )
                })
            };
            checks.push(check(
                "prompts",
                true,
                format!("defaults -> {}", describe(&cfg.defaults.review_prompt)),
            ));
            checks.extend(schema_hint("defaults", &cfg.defaults.review_prompt));
            let orgs = cfg
                .orgs
                .iter()
                .filter_map(|o| Some((&o.org, o.overrides.review_prompt.as_ref()?)));
            let repos = cfg
                .repos
                .iter()
                .filter_map(|r| Some((&r.name, r.overrides.review_prompt.as_ref()?)));
            for (level, value) in orgs.chain(repos) {
                checks.push(check(
                    "prompts",
                    true,
                    format!("{level} -> {}", describe(value)),
                ));
                checks.extend(schema_hint(level, value));
            }
        }
        None => checks.push(check("prompts", false, "skipped (config failed)")),
    }
    let legacy = paths::legacy_default_prompt_path();
    if let Ok(text) = std::fs::read_to_string(&legacy) {
        if text != prx::prompt::DEFAULT_PROMPT && !prx::prompt::is_legacy_default_prompt(&text) {
            checks.push(check_warn(
                "prompts",
                format!(
                    "{} is no longer read; paste its text into defaults.review_prompt to keep your edits",
                    legacy.display()
                ),
            ));
        }
    }

    // Data dir writable.
    let data_dir = paths::data_dir();
    let data_ok = std::fs::create_dir_all(&data_dir).is_ok() && probe_write(&data_dir);
    checks.push(check(
        "data dir",
        data_ok,
        format!("{} writable", data_dir.display()),
    ));

    // Autostart registrations (D4/D5): the daemon and menubar agents are
    // reported identically. Informational: a missing registration is a hint,
    // a stale or build-directory one a warning; none of these fails the run.
    // With the PRX.app login item (A4) active, it is the registration line
    // and any legacy agent left beside it is a leftover warning.
    let app_item = autostart::app_status();
    if app_item.installed {
        checks.push(app_autostart_check(&app_item));
        for (label, st) in [
            ("com.prx.daemon", autostart::status()),
            ("com.prx.menubar", autostart::menubar_status()),
        ] {
            if let (true, Some(path)) = (st.installed, st.path.as_deref()) {
                checks.push(check_warn(
                    "autostart leftover",
                    format!(
                        "legacy agent {label} is still registered ({}) beside PRX.app — \
                         re-run `prx daemon install` to remove it",
                        path.display()
                    ),
                ));
            }
        }
    } else {
        checks.push(autostart_check(
            "autostart",
            "run `prx daemon install`",
            &autostart::status(),
        ));
        checks.push(autostart_check(
            "menubar autostart",
            "run `prx daemon install --include-menubar`",
            &autostart::menubar_status(),
        ));
    }

    // Report.
    println!("prx doctor");
    let failures = checks.iter().filter(|c| !c.ok).count();
    let warnings = checks.iter().filter(|c| c.warning).count();
    for c in &checks {
        let marker = if c.warning {
            "WARN"
        } else if c.ok {
            "ok  "
        } else {
            "FAIL"
        };
        println!("  [{marker}] {:<28} {}", c.name, c.detail);
    }
    if failures > 0 {
        bail!("doctor found {failures} problem(s)");
    }
    if warnings > 0 {
        let plural = if warnings == 1 { "" } else { "s" };
        println!("\nall checks passed ({warnings} warning{plural})");
    } else {
        println!("\nall checks passed");
    }
    Ok(())
}

/// The serde token of a unit enum value (`openai_compatible`).
fn serde_plain_token<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Whether `browser` can be launched on this machine (design D9), with a
/// description; `Err` names what is missing. The open itself is the
/// browser resolver's job; this only mirrors its fallback decision.
fn browser_launchable(browser: &config::Browser) -> std::result::Result<String, String> {
    let system_opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    match browser {
        config::Browser::System => match paths::find_in_path(system_opener) {
            Some(_) => Ok(format!("system default (via {system_opener})")),
            None => Err(format!("`{system_opener}` not found on PATH")),
        },
        config::Browser::App(app) => {
            if !cfg!(target_os = "macos") {
                return Err(format!(
                    "application '{app}' needs macOS `open -a`; use a command template here"
                ));
            }
            let found = if app.contains('/') {
                std::path::Path::new(app).exists()
            } else {
                // `open -Ra` resolves the app through Launch Services
                // without launching it.
                std::process::Command::new("open")
                    .args(["-Ra", app])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok_and(|s| s.success())
            };
            if found {
                Ok(format!("application '{app}'"))
            } else {
                Err(format!("application '{app}' not found"))
            }
        }
        config::Browser::Command(argv) => {
            let program = argv.first().map(String::as_str).unwrap_or_default();
            let found = if program.contains('/') {
                std::path::Path::new(program).exists()
            } else {
                paths::find_in_path(program).is_some()
            };
            if found {
                Ok(format!("command `{}`", argv.join(" ")))
            } else {
                Err(format!("command '{program}' not found on PATH"))
            }
        }
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

fn probe_write(dir: &std::path::Path) -> bool {
    let probe = dir.join(format!(".prx-doctor-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"ok") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

// ----------------------------------------------------------------- repos

fn load_config_for_edit() -> Result<(std::path::PathBuf, Config)> {
    let path = paths::config_path();
    let config = Config::load(&path).context("failed to load config (run `prx init` first)")?;
    Ok((path, config))
}

fn cmd_repos_add(repo: &str) -> Result<()> {
    validate_repo_name(repo)?;
    let (path, mut config) = load_config_for_edit()?;
    if config.has_repo(repo) {
        println!("{repo} is already being watched");
        return Ok(());
    }
    config.repos.push(RepoConfig {
        name: repo.to_string(),
        ..RepoConfig::default()
    });
    config.validate().context("resulting config is invalid")?;
    config
        .save(&path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    println!("added {repo}");
    Ok(())
}

fn cmd_repos_list() -> Result<()> {
    let (_, config) = load_config_for_edit()?;
    config.validate().context("configuration is invalid")?;
    if config.repos.is_empty() {
        println!("no repos configured; add one with `prx repos add <org/repo>`");
        return Ok(());
    }
    let effective = config
        .effective_repos()
        .context("configuration is invalid")?;
    println!("{:<24} {:<10} {:<8} SKIP RULES", "REPO", "ENGINE", "POLL");
    for repo in effective {
        let mut parts: Vec<String> = Vec::new();
        if repo.skip.drafts {
            parts.push("drafts".to_string());
        }
        if !repo.skip.authors.is_empty() {
            parts.push(format!("authors:{}", repo.skip.authors.join(",")));
        }
        if !repo.skip.labels.is_empty() {
            parts.push(format!("labels:{}", repo.skip.labels.join(",")));
        }
        if !repo.skip.branches.is_empty() {
            parts.push(format!("branches:{}", repo.skip.branches.join(",")));
        }
        let skip = if parts.is_empty() {
            "-".to_string()
        } else {
            parts.join(" ")
        };
        println!(
            "{:<24} {:<10} {:<4}min {}",
            repo.name, repo.engine, repo.poll_interval_minutes, skip
        );
    }
    Ok(())
}

fn cmd_repos_remove(repo: &str) -> Result<()> {
    let (path, mut config) = load_config_for_edit()?;
    let Some(pos) = config.repos.iter().position(|r| r.name == repo) else {
        bail!("{repo} is not being watched");
    };
    config.repos.remove(pos);
    config.validate().context("resulting config is invalid")?;
    config
        .save(&path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    println!("removed {repo}");
    Ok(())
}

// ---------------------------------------------------------------- config

fn cmd_config_show() -> Result<()> {
    let config = load_config_for_edit()?.1;
    config.validate().context("configuration is invalid")?;
    let view = config
        .effective_view()
        .context("failed to merge effective configuration")?;
    print!(
        "{}",
        serde_yaml::to_string(&view).context("failed to serialize effective configuration")?
    );
    Ok(())
}

fn cmd_config_edit() -> Result<()> {
    let path = paths::config_path();
    // POSIX convention: $VISUAL (full-screen editor) wins over $EDITOR
    // (line-oriented fallback) when both are set.
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .map_err(|_| {
            anyhow::anyhow!(
                "no $EDITOR (or $VISUAL) set; edit {} directly",
                path.display()
            )
        })?;
    config_edit::open_with(&path, &editor).map_err(|e| anyhow::anyhow!("{e}"))
}

fn cmd_config_set(key: &str, value: &str) -> Result<()> {
    let path = paths::config_path();
    let mut config = Config::load(&path).context("failed to load config (run `prx init` first)")?;
    config_edit::set_value(&mut config, key, value).map_err(|e| anyhow::anyhow!("{e}"))?;
    // Validate before writing: a `set` that would produce an invalid config
    // leaves the file untouched.
    config
        .validate()
        .context("resulting configuration is invalid (file not modified)")?;
    config
        .save(&path)
        .with_context(|| format!("cannot write {}", path.display()))?;
    println!("set {key} = {value}");
    Ok(())
}
