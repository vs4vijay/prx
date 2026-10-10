//! Clipboard writes for "Copy brief" (task 4.1, design D11).
//!
//! The production clipboard pipes the text to `pbcopy` on stdin (no shell,
//! no argv-length limits, no escaping); the trait keeps the action path
//! testable with an in-memory clipboard.

use std::io::Write;
use std::process::{Command, Stdio};
#[cfg(test)]
use std::sync::Mutex;

/// Somewhere text can be copied to.
pub trait Clipboard: Send + Sync {
    fn copy(&self, text: &str) -> Result<(), String>;
}

/// The system clipboard via `pbcopy` (macOS).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClipboard;

impl Clipboard for SystemClipboard {
    fn copy(&self, text: &str) -> Result<(), String> {
        pipe_to("pbcopy", &[], text)
    }
}

/// Run `program args…` with `text` on its stdin; succeed on exit status 0.
pub fn pipe_to(program: &str, args: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{program}: no stdin"))?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("{program}: write failed: {e}"))?;
        // Dropping stdin closes the pipe so the program sees EOF.
    }
    let status = child
        .wait()
        .map_err(|e| format!("{program}: wait failed: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

/// In-memory clipboard (tests): remembers every copied text.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct MemoryClipboard(pub Mutex<Vec<String>>);

#[cfg(test)]
impl Clipboard for MemoryClipboard {
    fn copy(&self, text: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stdin pipe hands the exact bytes over (`cat` stands in for
    /// `pbcopy`, writing to a file through `sh -c`).
    #[test]
    fn pipe_to_feeds_the_full_text_on_stdin() {
        let out = std::env::temp_dir().join(format!("prx-clip-{}", std::process::id()));
        let text = "line one\nline \"two\" with $HOME and ✓";
        let script = format!("cat > '{}'", out.display());
        pipe_to("sh", &["-c", &script], text).unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), text);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn pipe_to_reports_failures() {
        assert!(pipe_to("sh", &["-c", "cat >/dev/null; exit 3"], "x")
            .unwrap_err()
            .contains("exited"));
        assert!(pipe_to("prx-no-such-clipboard-tool", &[], "x")
            .unwrap_err()
            .contains("cannot run"));
    }
}
