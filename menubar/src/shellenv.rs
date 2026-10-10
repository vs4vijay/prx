//! The user's login-shell `PATH` (design A3).
//!
//! An app launched from Finder, Spotlight, or launchd gets the bare system
//! `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`), which cannot resolve the
//! homebrew-installed tools a review needs (`gh`, the agent CLIs). This is
//! the same problem the v1.1 launchd fix solved by snapshotting the
//! installing shell's `PATH`; the app instead asks the user's login shell
//! once (`$SHELL -ilc`, bounded by a timeout) and hands the answer to the
//! daemon it spawns. The probe result is cached for the process lifetime.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Markers around the printed PATH, so shell start-up noise (motd, prompts
/// from rc files) never leaks into the value.
const MARK: &str = "__PRX_PATH__";
/// Upper bound for the probe (a slow rc file must not stall the app).
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// Common tool locations appended when the probe fails.
const FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// The login-shell `PATH` (cached), or the process `PATH` plus the common
/// homebrew locations when the probe fails.
pub fn login_path() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let current = std::env::var("PATH").unwrap_or_default();
        match probe() {
            Some(path) => {
                eprintln!(
                    "prx-menubar: login-shell PATH probed ({} entries)",
                    path.split(':').count()
                );
                merge(&path, &current)
            }
            None => {
                eprintln!(
                    "prx-menubar: login-shell PATH probe failed; using PATH plus homebrew dirs"
                );
                merge(&current, &FALLBACK_DIRS.join(":"))
            }
        }
    })
}

/// Ask `$SHELL -ilc` for its PATH (None on any failure or timeout).
fn probe() -> Option<String> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_string());
    let script = format!("printf '%s%s%s' '{MARK}' \"$PATH\" '{MARK}'");
    let mut child = Command::new(shell)
        .args(["-ilc", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    extract(&out)
}

/// The value between the first pair of markers (non-empty).
pub fn extract(output: &str) -> Option<String> {
    let start = output.find(MARK)? + MARK.len();
    let rest = &output[start..];
    let end = rest.find(MARK)?;
    let value = rest[..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// `first` then the entries of `second` it lacks (order kept, no dupes).
pub fn merge(first: &str, second: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for dir in first.split(':').chain(second.split(':')) {
        if !dir.is_empty() && !out.contains(&dir) {
            out.push(dir);
        }
    }
    out.join(":")
}

/// Is `dir` one of the entries of `path`?
pub fn path_contains(path: &str, dir: &std::path::Path) -> bool {
    path.split(':')
        .filter(|entry| !entry.is_empty())
        .any(|entry| std::path::Path::new(entry) == dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_ignores_shell_noise_around_the_markers() {
        let out = format!("Last login: today\n{MARK}/opt/homebrew/bin:/usr/bin{MARK}\nbye");
        assert_eq!(extract(&out).as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
        assert_eq!(extract("no markers"), None);
        assert_eq!(extract(&format!("{MARK}{MARK}")), None);
    }

    #[test]
    fn merge_keeps_order_and_drops_duplicates() {
        assert_eq!(merge("/a:/b", "/b:/c::"), "/a:/b:/c");
        assert_eq!(merge("", "/x"), "/x");
    }

    #[test]
    fn path_contains_matches_whole_entries() {
        let home_bin = std::path::Path::new("/Users/me/.local/bin");
        assert!(path_contains("/usr/bin:/Users/me/.local/bin", home_bin));
        assert!(!path_contains("/usr/bin:/Users/me/.local/bin2", home_bin));
    }
}
