//! Tracing initialization. The CLI logs to stderr only; the daemon (Phase 3,
//! audit A15) additionally writes daily-rotating files
//! `<data dir>/logs/prx.log.YYYY-MM-DD` and prunes files older than
//! [`LOG_RETENTION_DAYS`] on startup.

use std::path::Path;

use chrono::{Local, TimeDelta};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Log retention for the daemon (A15).
pub const LOG_RETENTION_DAYS: i64 = 14;

/// Initialize CLI logging to stderr. Respects `RUST_LOG`; defaults to `warn`
/// so normal CLI stdout stays clean.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Initialize daemon logging: a daily-rotating file appender under
/// `logs/`, plus stderr when `stderr_too` (the foreground invocation; the
/// detached child runs with stdio nulled). Respects `RUST_LOG`, defaulting
/// to `info` — the daemon has no stdout to keep clean. The returned guard
/// must be held for the process lifetime (it flushes the async writer).
pub fn init_daemon(stderr_too: bool) -> WorkerGuard {
    let logs_dir = crate::paths::logs_dir();
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        eprintln!(
            "warning: cannot create logs dir {}: {e}",
            logs_dir.display()
        );
    }
    prune_old_logs(&logs_dir, LOG_RETENTION_DAYS);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let (file_writer, guard) = tracing_appender::non_blocking(rolling::daily(&logs_dir, "prx.log"));
    // Layers are built per branch so each fmt layer's subscriber type is
    // inferred independently (a shared layer value would unify the two
    // branches' different S types).
    if stderr_too {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(file_writer),
            )
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_writer(file_writer),
            )
            .init();
    }
    guard
}

/// Remove `logs/prx.log.YYYY-MM-DD` files older than `keep_days`. Unrelated
/// files and files with unparseable names are left alone (best-effort; never
/// fatal — A15 "self-healing" diagnostics must not be lost to a bad prune).
pub fn prune_old_logs(dir: &Path, keep_days: i64) {
    let cutoff = Local::now().date_naive() - TimeDelta::try_days(keep_days).unwrap_or_default();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(date_str) = name.strip_prefix("prx.log.") else {
            continue;
        };
        let Ok(date) = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_removes_only_old_rotated_files() {
        let dir = std::env::temp_dir().join(format!("prx-logs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join(format!(
            "prx.log.{}",
            (Local::now().date_naive() - TimeDelta::try_days(30).unwrap()).format("%Y-%m-%d")
        ));
        let fresh = dir.join(format!(
            "prx.log.{}",
            Local::now().date_naive().format("%Y-%m-%d")
        ));
        let foreign = dir.join("prx.log.not-a-date");
        let other = dir.join("unrelated.txt");
        for f in [&old, &fresh, &foreign, &other] {
            std::fs::write(f, b"x").unwrap();
        }

        prune_old_logs(&dir, LOG_RETENTION_DAYS);

        assert!(!old.exists(), "expired log must be pruned");
        assert!(fresh.exists(), "today's log must survive");
        assert!(foreign.exists(), "unparseable names must survive");
        assert!(other.exists(), "unrelated files must survive");

        // Missing directory is a no-op.
        prune_old_logs(&dir.join("nope"), LOG_RETENTION_DAYS);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
