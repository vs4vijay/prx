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
