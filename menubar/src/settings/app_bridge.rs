//! App-local bridge methods (Settings → General, tasks 6.6/6.7). These never
//! reach the daemon: they manage the PRX.app login item and the `prx`
//! command-line link on this machine, so they work while the daemon is
//! offline. Kept apart from [`super::bridge`]'s daemon routing; the bridge
//! hands every `app.*` message here.
//!
//! ```text
//! JS → Rust   {"id": 1, "method": "app.info",            "params": {}}
//!             {"id": 2, "method": "app.login_item.set",  "params": {"enabled": true}}
//!             {"id": 3, "method": "app.cli.install",     "params": {}}
//! ```
//!
//! `app.info` answers `{bundle, version, login_item: {enabled, app,
//! this_app}, cli: {link, target, state, on_path}}` where `state` is
//! `missing | linked | linked_elsewhere | blocked | unavailable`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::bridge::{BridgeError, Response};

/// Does this JS message name an `app.*` method?
pub fn is_app_message(body: &str) -> bool {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("method").and_then(Value::as_str).map(str::to_string))
        .is_some_and(|method| method.starts_with("app."))
}

/// Handle one `app.*` message (blocking; run it off the main thread).
pub fn handle(body: &str) -> Response {
    let message: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    eprintln!("prx-menubar: settings bridge {method} (id {id})");
    let context = Context::current();
    let result = match method {
        "app.info" => Ok(info(&context)),
        "app.login_item.set" => match params.get("enabled").and_then(Value::as_bool) {
            Some(enabled) => set_login_item(&context, enabled),
            None => Err(app_error(
                "bad_request",
                "app.login_item.set: `enabled` must be a boolean",
            )),
        },
        "app.cli.install" => install_cli(&context),
        other => Err(app_error(
            "bad_request",
            format!("unknown bridge method '{other}'"),
        )),
    };
    match result {
        Ok(value) => Response::success(id, value),
        Err(error) => Response::failure(id, error),
    }
}

fn app_error(kind: &str, message: impl Into<String>) -> BridgeError {
    BridgeError {
        kind: kind.into(),
        message: message.into(),
        field: None,
    }
}

/// Where things live for this process (a seam for tests).
struct Context {
    home: Option<PathBuf>,
    bundle: Option<PathBuf>,
    prx: Option<PathBuf>,
    path: String,
}

impl Context {
    fn current() -> Context {
        let exe = std::env::current_exe().ok();
        Context {
            home: dirs_home(),
            bundle: exe.as_deref().and_then(prx::autostart::enclosing_bundle),
            prx: crate::supervisor::bundled_prx(),
            path: crate::shellenv::login_path().to_string(),
        }
    }

    fn link(&self) -> Option<PathBuf> {
        self.home.as_ref().map(|home| cli_link_path(home))
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// `~/.local/bin/prx` (design A6).
pub fn cli_link_path(home: &Path) -> PathBuf {
    home.join(".local").join("bin").join("prx")
}

fn info(context: &Context) -> Value {
    let login = context.home.as_deref().map(prx::autostart::app_status_with);
    let registered_app = login
        .as_ref()
        .filter(|st| st.installed)
        .and_then(|st| st.binary.as_deref())
        .and_then(prx::autostart::bundle_of_executable);
    let this_app = matches!((&registered_app, &context.bundle), (Some(a), Some(b)) if a == b);
    let cli = match (context.link(), context.prx.as_deref()) {
        (Some(link), Some(target)) => {
            let dir = link.parent().map(Path::to_path_buf).unwrap_or_default();
            json!({
                "link": link,
                "target": target,
                "state": link_state(&link, target).as_str(),
                "on_path": crate::shellenv::path_contains(&context.path, &dir),
            })
        }
        (link, _) => {
            json!({ "link": link, "target": null, "state": "unavailable", "on_path": false })
        }
    };
    json!({
        "bundle": context.bundle,
        "version": env!("CARGO_PKG_VERSION"),
        "login_item": {
            "enabled": registered_app.is_some(),
            "app": registered_app,
            "this_app": this_app,
        },
        "cli": cli,
    })
}

fn set_login_item(context: &Context, enabled: bool) -> Result<Value, BridgeError> {
    let home = context
        .home
        .as_deref()
        .ok_or_else(|| app_error("app", "cannot determine the home directory"))?;
    if enabled {
        let bundle = context.bundle.as_deref().ok_or_else(|| {
            app_error(
                "unsupported",
                "Open at login needs PRX.app — build it with scripts/bundle-macos.sh and open it from Applications",
            )
        })?;
        prx::autostart::install_app_with(bundle, home)
            .map_err(|e| app_error("app", format!("{e:#}")))?;
    } else {
        prx::autostart::uninstall_app_with(home).map_err(|e| app_error("app", format!("{e:#}")))?;
    }
    Ok(info(context))
}

fn install_cli(context: &Context) -> Result<Value, BridgeError> {
    let link = context
        .link()
        .ok_or_else(|| app_error("app", "cannot determine the home directory"))?;
    let target = context
        .prx
        .as_deref()
        .ok_or_else(|| app_error("unsupported", "no bundled prx binary next to this app"))?;
    let outcome = install_cli_link(target, &link).map_err(|message| app_error("app", message))?;
    let dir = link.parent().map(Path::to_path_buf).unwrap_or_default();
    let on_path = crate::shellenv::path_contains(&context.path, &dir);
    let mut value = info(context);
    value["installed"] = json!(outcome.as_str());
    if !on_path {
        value["warning"] = json!(format!(
            "{} is not on your shell's PATH — add it (e.g. export PATH=\"{}:$PATH\" in ~/.zprofile) to run `prx`",
            dir.display(),
            dir.display()
        ));
    }
    Ok(value)
}

/// What is at the link path now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    Missing,
    /// A symlink to `target`.
    Linked,
    /// A symlink to something else (it may be replaced).
    LinkedElsewhere(PathBuf),
    /// A regular file or directory: never overwritten.
    Blocked,
}

impl LinkState {
    fn as_str(&self) -> &'static str {
        match self {
            LinkState::Missing => "missing",
            LinkState::Linked => "linked",
            LinkState::LinkedElsewhere(_) => "linked_elsewhere",
            LinkState::Blocked => "blocked",
        }
    }
}

pub fn link_state(link: &Path, target: &Path) -> LinkState {
    match std::fs::symlink_metadata(link) {
        Err(_) => LinkState::Missing,
        Ok(meta) if meta.file_type().is_symlink() => match std::fs::read_link(link) {
            Ok(points) if points == target => LinkState::Linked,
            Ok(points) => LinkState::LinkedElsewhere(points),
            Err(_) => LinkState::LinkedElsewhere(PathBuf::new()),
        },
        Ok(_) => LinkState::Blocked,
    }
}

/// What [`install_cli_link`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOutcome {
    Created,
    AlreadyLinked,
    /// An older link (e.g. to a previous PRX.app location) was replaced.
    Replaced(PathBuf),
}

impl LinkOutcome {
    fn as_str(&self) -> &'static str {
        match self {
            LinkOutcome::Created => "created",
            LinkOutcome::AlreadyLinked => "already_linked",
            LinkOutcome::Replaced(_) => "replaced",
        }
    }
}

/// Create `link → target` (design A6). Refuses when `link` exists and is not
/// a symlink; an existing symlink is reused or repointed.
pub fn install_cli_link(target: &Path, link: &Path) -> Result<LinkOutcome, String> {
    if let Some(dir) = link.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let outcome = match link_state(link, target) {
        LinkState::Linked => return Ok(LinkOutcome::AlreadyLinked),
        LinkState::Blocked => {
            return Err(format!(
                "{} already exists and is not a symlink — PRX will not overwrite it; \
                 move it away and try again",
                link.display()
            ))
        }
        LinkState::LinkedElsewhere(previous) => {
            std::fs::remove_file(link)
                .map_err(|e| format!("cannot replace {}: {e}", link.display()))?;
            LinkOutcome::Replaced(previous)
        }
        LinkState::Missing => LinkOutcome::Created,
    };
    std::os::unix::fs::symlink(target, link)
        .map_err(|e| format!("cannot create {}: {e}", link.display()))?;
    eprintln!(
        "prx-menubar: command-line tool: {} -> {} ({})",
        link.display(),
        target.display(),
        outcome.as_str()
    );
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prx-cli-link-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_the_link_and_its_directory() {
        let dir = temp("create");
        let target = dir.join("PRX.app/Contents/Helpers/prx");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"").unwrap();
        let link = cli_link_path(&dir.join("home"));
        assert_eq!(link_state(&link, &target), LinkState::Missing);
        assert_eq!(install_cli_link(&target, &link), Ok(LinkOutcome::Created));
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        assert_eq!(link_state(&link, &target), LinkState::Linked);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_to_overwrite_a_regular_file() {
        let dir = temp("refuse");
        let target = dir.join("prx");
        std::fs::write(&target, b"").unwrap();
        let link = cli_link_path(&dir.join("home"));
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(&link, b"#!/bin/sh\necho mine\n").unwrap();
        assert_eq!(link_state(&link, &target), LinkState::Blocked);
        let err = install_cli_link(&target, &link).unwrap_err();
        assert!(err.contains("not a symlink"), "{err}");
        // Untouched.
        assert_eq!(
            std::fs::read_to_string(&link).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_link_is_kept_or_repointed() {
        let dir = temp("existing");
        let target = dir.join("new/prx");
        let old = dir.join("old/prx");
        for path in [&target, &old] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        let link = cli_link_path(&dir.join("home"));
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            install_cli_link(&target, &link),
            Ok(LinkOutcome::AlreadyLinked)
        );

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&old, &link).unwrap();
        assert_eq!(
            link_state(&link, &target),
            LinkState::LinkedElsewhere(old.clone())
        );
        assert_eq!(
            install_cli_link(&target, &link),
            Ok(LinkOutcome::Replaced(old))
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), target);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_app_methods_are_routed_here() {
        assert!(is_app_message(
            r#"{"id":1,"method":"app.info","params":{}}"#
        ));
        assert!(!is_app_message(
            r#"{"id":1,"method":"config.get","params":{}}"#
        ));
        assert!(!is_app_message("garbage"));
    }

    #[test]
    fn info_reports_the_link_and_login_item_state() {
        let dir = temp("info");
        let home = dir.join("home");
        let target = dir.join("prx");
        std::fs::write(&target, b"").unwrap();
        let context = Context {
            home: Some(home.clone()),
            bundle: None,
            prx: Some(target.clone()),
            path: "/usr/bin".into(),
        };
        let value = info(&context);
        assert_eq!(value["cli"]["state"], "missing");
        assert_eq!(value["cli"]["on_path"], false);
        assert_eq!(value["login_item"]["enabled"], false);
        // Without a bundle, Open at login is refused (not silently headless).
        let err = set_login_item(&context, true).unwrap_err();
        assert_eq!(err.kind, "unsupported");
        // Installing warns that ~/.local/bin is not on PATH.
        let installed = install_cli(&context).unwrap();
        assert_eq!(installed["installed"], "created");
        assert_eq!(installed["cli"]["state"], "linked");
        assert!(installed["warning"]
            .as_str()
            .unwrap()
            .contains("not on your shell's PATH"));
        let on_path = Context {
            path: format!("/usr/bin:{}", home.join(".local/bin").display()),
            ..context
        };
        let again = install_cli(&on_path).unwrap();
        assert_eq!(again["installed"], "already_linked");
        assert!(again.get("warning").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
