//! "Open Review Chat…" (openspec change `review-chat`, design C7): open a
//! terminal window that runs the bundled `prx chat <key>`. The app never
//! runs the agent and never talks to GitHub (A4): `prx chat` does the work
//! in the terminal. The window's command comes from a `.command` file built
//! by [`prx::chat::terminal_script`], which holds only single-quoted paths
//! and the validated key, and is opened with `open -a <terminal> <file>`:
//! argv only, no AppleScript, no shell string with PR text in it.

use std::path::{Path, PathBuf};

/// The terminal app a chat opens in.
pub const DEFAULT_TERMINAL: &str = "Terminal";

/// Opens a `.command` file in a terminal window (injectable, so tests never
/// open one).
pub trait TerminalLauncher: Send + Sync {
    fn open(&self, script: &Path) -> Result<(), String>;
}

/// The real launcher: `/usr/bin/open -a <app> <script>`.
pub struct SystemTerminal {
    pub app: String,
}

impl Default for SystemTerminal {
    fn default() -> Self {
        SystemTerminal {
            app: DEFAULT_TERMINAL.to_string(),
        }
    }
}

impl TerminalLauncher for SystemTerminal {
    fn open(&self, script: &Path) -> Result<(), String> {
        let status = std::process::Command::new("/usr/bin/open")
            .arg("-a")
            .arg(&self.app)
            .arg(script)
            .status()
            .map_err(|e| format!("cannot run open: {e}"))?;
        match status.success() {
            true => Ok(()),
            false => Err(format!("open -a {} exited with {status}", self.app)),
        }
    }
}

/// Write `key`'s `.command` file under `data_dir` and open it. `helper` is
/// the bundled `prx` ([`crate::supervisor::bundled_prx`]). The script
/// exports this app's `PRX_CONFIG` and `PRX_DATA_DIR`, so the chat sees the
/// same config and data as the app (a sandbox stays a sandbox).
pub fn open_review_chat(
    key: &str,
    helper: Option<PathBuf>,
    data_dir: &Path,
    config_path: &Path,
    launcher: &dyn TerminalLauncher,
) -> Result<String, String> {
    prx::review::parse_pr_key(key).map_err(|e| format!("cannot open Review Chat: {e}"))?;
    let helper = helper.ok_or_else(|| {
        "cannot open Review Chat: the bundled prx command-line tool was not found".to_string()
    })?;
    let env = [
        ("PRX_CONFIG", config_path.to_path_buf()),
        ("PRX_DATA_DIR", data_dir.to_path_buf()),
    ];
    let script = prx::chat::write_launch_script(data_dir, &helper, key, &env)
        .map_err(|e| format!("cannot open Review Chat: {e:#}"))?;
    launcher.open(&script)?;
    Ok(format!("opened Review Chat for {key} in a terminal"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct Recording(pub Mutex<Vec<PathBuf>>);

    impl TerminalLauncher for Recording {
        fn open(&self, script: &Path) -> Result<(), String> {
            self.0.lock().unwrap().push(script.to_path_buf());
            Ok(())
        }
    }

    fn temp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("prx-review-chat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_a_quoted_script_and_opens_it() {
        let dir = temp("open");
        let launcher = Recording::default();
        let helper = PathBuf::from("/Apps/My PRX.app/Contents/Helpers/prx");
        let note = open_review_chat(
            "org/repo#13",
            Some(helper),
            &dir,
            &dir.join("it's config.yml"),
            &launcher,
        )
        .unwrap();
        assert!(note.contains("org/repo#13"), "{note}");
        let opened = launcher.0.lock().unwrap().clone();
        assert_eq!(opened, vec![dir.join("chats/launch/org_repo-13.command")]);
        let script = std::fs::read_to_string(&opened[0]).unwrap();
        assert!(script.starts_with("#!/bin/sh\n"), "{script}");
        assert!(script.contains("export PRX_CONFIG='"), "{script}");
        assert!(script.contains("it'\\''s config.yml'"), "{script}");
        assert!(
            script.ends_with("exec '/Apps/My PRX.app/Contents/Helpers/prx' chat 'org/repo#13'\n")
        );
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&opened[0]).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_bad_keys_and_a_missing_helper_without_opening_anything() {
        let dir = temp("refuse");
        let launcher = Recording::default();
        let helper = || Some(PathBuf::from("/x/prx"));
        for key in ["org/repo#1; rm -rf ~", "org/repo", "'$(id)'/x#1"] {
            let err = open_review_chat(key, helper(), &dir, &dir, &launcher).unwrap_err();
            assert!(err.contains("invalid PR target"), "{err}");
        }
        let err = open_review_chat("org/repo#1", None, &dir, &dir, &launcher).unwrap_err();
        assert!(err.contains("bundled prx"), "{err}");
        assert!(launcher.0.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
