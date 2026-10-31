//! Path resolution for prx. All I/O locations flow through here so tests can
//! redirect everything with the `PRX_CONFIG` / `PRX_DATA_DIR` env overrides.

use std::path::{Path, PathBuf};

/// Full path of the config file: `$PRX_CONFIG` or `~/.config/prx/config.yml`.
pub fn config_path() -> PathBuf {
    if let Ok(p) = std::env::var("PRX_CONFIG") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    home().join(".config").join("prx").join("config.yml")
}

/// Directory containing the config file (parent of `config_path`).
pub fn config_dir() -> PathBuf {
    config_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home().join(".config").join("prx"))
}

/// Data directory: `$PRX_DATA_DIR` or `~/.local/share/prx`.
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("PRX_DATA_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    home().join(".local").join("share").join("prx")
}

/// Persistent PR lifecycle state (`state.json`), written atomically.
pub fn state_file() -> PathBuf {
    data_dir().join("state.json")
}

/// Append-only review history (`history.jsonl`).
pub fn history_file() -> PathBuf {
    data_dir().join("history.jsonl")
}

/// Where `prx init` wrote the default prompt before design F5 made
/// `review_prompt` inline text; no longer read (doctor warns if edited).
pub fn legacy_default_prompt_path() -> PathBuf {
    data_dir().join("prompts").join("default.md")
}

/// Rotating daemon logs (Phase 3).
pub fn logs_dir() -> PathBuf {
    data_dir().join("logs")
}

/// Daemon single-instance guard file (A16).
pub fn daemon_pid_file() -> PathBuf {
    data_dir().join("daemon.pid")
}

/// Ephemeral IPC port chosen by the daemon (A4).
pub fn daemon_port_file() -> PathBuf {
    data_dir().join("daemon.port")
}

/// Per-start bearer token for the IPC API, written 0600 (A4).
pub fn daemon_token_file() -> PathBuf {
    data_dir().join("daemon.token")
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` or `~/` to the user's home directory (audit A19).
pub fn expand_tilde(value: &str) -> PathBuf {
    let value = value.trim();
    if value == "~" {
        return home();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(value)
}

/// Search `PATH` for an executable named `name` (used by `prx doctor`).
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

// The `PRX_CONFIG` / `PRX_DATA_DIR` / `HOME` resolution itself is covered in
// `tests/cli_pause_paths.rs`, which runs the binary with a controlled
// environment: unit tests share one process, so they must not set variables.
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// The on-disk layout is a compatibility surface: an upgrade must find
    /// the files the previous version wrote, under the same names.
    #[test]
    fn data_files_keep_their_names_under_the_data_dir() {
        let data = data_dir();
        assert_eq!(state_file(), data.join("state.json"));
        assert_eq!(history_file(), data.join("history.jsonl"));
        assert_eq!(logs_dir(), data.join("logs"));
        assert_eq!(daemon_pid_file(), data.join("daemon.pid"));
        assert_eq!(daemon_port_file(), data.join("daemon.port"));
        assert_eq!(daemon_token_file(), data.join("daemon.token"));
        assert_eq!(
            legacy_default_prompt_path(),
            data.join("prompts").join("default.md")
        );
        assert_eq!(config_path().parent(), Some(config_dir().as_path()));
    }

    #[test]
    fn tilde_expands_only_as_a_leading_home_reference() {
        let home = expand_tilde("~");
        assert!(home.is_absolute(), "{home:?}");
        assert_eq!(expand_tilde("  ~/bin/agent  "), home.join("bin/agent"));
        // Another user's home, a tilde mid-path, and plain paths are as written.
        for literal in ["~other/bin", "/opt/~/x", "relative/~", "bin/agent", ""] {
            assert_eq!(expand_tilde(literal), PathBuf::from(literal), "{literal:?}");
        }
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prx-paths-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_with_mode(path: &Path, mode: u32) {
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Only a regular file with an execute bit counts: not a plain file,
    /// not a directory (which has execute bits), not a missing path.
    #[test]
    fn executable_means_a_regular_file_with_an_execute_bit() {
        let dir = scratch("exec");
        let tool = dir.join("tool");
        write_with_mode(&tool, 0o755);
        let owner_only = dir.join("owner-only");
        write_with_mode(&owner_only, 0o700);
        let data = dir.join("data");
        write_with_mode(&data, 0o644);
        std::fs::create_dir(dir.join("subdir")).unwrap();

        assert!(is_executable_file(&tool));
        assert!(is_executable_file(&owner_only));
        assert!(!is_executable_file(&data));
        assert!(!is_executable_file(&dir.join("subdir")));
        assert!(!is_executable_file(&dir.join("missing")));
        // A symlink is judged by what it points at.
        std::os::unix::fs::symlink(&tool, dir.join("link")).unwrap();
        std::os::unix::fs::symlink(dir.join("missing"), dir.join("dangling")).unwrap();
        assert!(is_executable_file(&dir.join("link")));
        assert!(!is_executable_file(&dir.join("dangling")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An engine configured with an absolute path is found without being on
    /// `PATH` (the launchd advice in the docs depends on it); a name that
    /// exists nowhere is not found.
    #[test]
    fn find_in_path_accepts_absolute_paths_and_misses_unknown_names() {
        let dir = scratch("find");
        let tool = dir.join("my-agent");
        write_with_mode(&tool, 0o755);
        assert_eq!(find_in_path(tool.to_str().unwrap()), Some(tool.clone()));
        let plain = dir.join("not-executable");
        write_with_mode(&plain, 0o644);
        assert_eq!(find_in_path(plain.to_str().unwrap()), None);
        assert_eq!(find_in_path("prx-no-such-binary-anywhere"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
