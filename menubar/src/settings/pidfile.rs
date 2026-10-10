//! The app's single-instance pid file (design A2; formerly the settings
//! window's D6 guard, retired with `--settings`). The running PRX UI claims
//! `<data dir>/app.pid` for its lifetime. A later launch reads it and, when
//! that process is a live GUI app, forwards its request there
//! ([`crate::instance`]) instead of starting a second instance. A stale
//! pidfile (dead process) is simply overwritten.

use std::path::{Path, PathBuf};

/// The pidfile path.
pub fn path() -> PathBuf {
    prx::paths::data_dir().join("app.pid")
}

/// What an open request should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Another live instance is running: forward to it.
    ActivateExisting(i32),
    /// No live holder: this process is the instance (claim the pidfile).
    Claim,
}

/// Decide from the pidfile contents. `is_live_app(pid)` answers whether
/// `pid` is a running GUI application (production: `NSRunningApplication`).
pub fn decide(contents: Option<&str>, own_pid: i32, is_live_app: impl Fn(i32) -> bool) -> Decision {
    match contents.and_then(|text| text.trim().parse::<i32>().ok()) {
        Some(pid) if pid > 0 && pid != own_pid && is_live_app(pid) => {
            Decision::ActivateExisting(pid)
        }
        _ => Decision::Claim,
    }
}

/// Holds the claim while the app runs; removes the pidfile on drop
/// (only if it still names this process).
pub struct Claim {
    path: PathBuf,
    pid: i32,
}

impl Claim {
    /// Write `pid` to `path` (temp file + rename; best effort — a failed
    /// claim only weakens single-instance, it never blocks start-up).
    pub fn acquire(path: &Path, pid: i32) -> Claim {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension(format!("pid.{pid}.tmp"));
        let written =
            std::fs::write(&tmp, format!("{pid}\n")).and_then(|()| std::fs::rename(&tmp, path));
        if let Err(error) = written {
            eprintln!(
                "prx-menubar: cannot write app pidfile {}: {error}",
                path.display()
            );
        }
        Claim {
            path: path.to_path_buf(),
            pid,
        }
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let ours = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| text.trim().parse::<i32>().ok())
            == Some(self.pid);
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_live_other_holder_is_activated() {
        assert_eq!(
            decide(Some("4242\n"), 1, |pid| pid == 4242),
            Decision::ActivateExisting(4242)
        );
    }

    #[test]
    fn missing_stale_own_or_garbage_pidfiles_are_claimed() {
        assert_eq!(decide(None, 1, |_| true), Decision::Claim);
        assert_eq!(decide(Some("4242"), 1, |_| false), Decision::Claim);
        assert_eq!(decide(Some("7"), 7, |_| true), Decision::Claim);
        assert_eq!(decide(Some("nope"), 1, |_| true), Decision::Claim);
        assert_eq!(decide(Some("-3"), 1, |_| true), Decision::Claim);
    }

    #[test]
    fn claim_writes_and_releases_only_its_own_pidfile() {
        let dir = std::env::temp_dir().join(format!("prx-app-pid-{}", std::process::id()));
        let path = dir.join("app.pid");
        {
            let _claim = Claim::acquire(&path, 99);
            assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "99");
        }
        assert!(!path.exists(), "released on drop");

        let claim = Claim::acquire(&path, 99);
        // Another process took over (e.g. after a stale-claim race).
        std::fs::write(&path, "100\n").unwrap();
        drop(claim);
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "100");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
