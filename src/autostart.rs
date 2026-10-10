//! Login autostart registration for the prx daemon (D4) and PRX.app (A4).
//!
//! On macOS, when a PRX.app bundle is found (design A4: an explicit
//! `--app`, the bundle enclosing the running binary, then
//! `/Applications/PRX.app` and `~/Applications/PRX.app`), `prx daemon
//! install` registers ONE login item, `com.prx.app`, launching
//! `PRX.app/Contents/MacOS/PRX --background` (the app supervises its own
//! daemon), and removes the legacy `com.prx.daemon` / `com.prx.menubar`
//! agents. Without a bundle the headless registration below is unchanged.
//!
//! `prx daemon install` writes a per-OS registration that launches the daemon
//! at user login; `prx daemon uninstall` removes it. Neither ever signals a
//! running daemon — they only write or remove registration files.
//!
//! - macOS: a launchd plist at `~/Library/LaunchAgents/com.prx.daemon.plist`
//!   with `RunAtLoad` and `ProgramArguments = [<exe>, "daemon", "start",
//!   "--foreground"]`. launchd supervises the foreground process (D4):
//!   `KeepAlive { SuccessfulExit = false }` relaunches a crashed (non-zero
//!   exit) daemon, while a clean exit 0 — including `prx daemon stop`'s
//!   SIGTERM shutdown — stands the supervisor down. The pid-file guard (A16)
//!   still makes a launchd + manual double start a no-op. The agent is
//!   deliberately NOT loaded at install time: `RunAtLoad` applies at the
//!   next login, and loading immediately would *start* the daemon, which
//!   install does not promise. The menubar agent has its own template
//!   (`com.prx.menubar.plist`: `RunAtLoad`, the same `KeepAlive`, the
//!   `prx-menubar` binary as sole argument) for the opt-in
//!   `--include-menubar` install. Its binary resolves next to the
//!   registered daemon binary (falling back to `~/.cargo/bin/prx-menubar`);
//!   an unresolvable menubar fails the install loudly before anything is
//!   written.
//! - Linux: a systemd user unit at
//!   `~/.config/systemd/user/prx-daemon.service` running
//!   `<exe> daemon start --foreground` (systemd wants the foreground
//!   process; it supervises via `Restart=on-failure`), enabled via
//!   `systemctl --user enable`.
//!
//! Both registration files are written from templates below and parsed back
//! with minimal string extraction for `prx doctor` — they are our own file
//! format, so no XML/INI crate is involved.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// launchd label and plist file stem (macOS).
const LABEL: &str = "com.prx.daemon";
/// launchd label and plist file stem for the menubar agent (macOS).
const MENUBAR_LABEL: &str = "com.prx.menubar";
/// The menubar binary's file name, resolved next to the daemon binary.
const MENUBAR_BINARY_NAME: &str = "prx-menubar";
/// systemd user unit name (Linux).
const UNIT_NAME: &str = "prx-daemon.service";
/// launchd label and plist file stem for the PRX.app login item (macOS, A4).
pub const APP_LABEL: &str = "com.prx.app";
/// The app bundle's directory name.
pub const APP_BUNDLE_NAME: &str = "PRX.app";
/// The UI executable inside the bundle (`Contents/MacOS/PRX`).
pub const APP_EXECUTABLE: &str = "PRX";
/// The bundled CLI/daemon, relative to the bundle: `Contents/Helpers/prx`.
/// Not `Contents/MacOS/prx`: on a case-insensitive volume (the macOS
/// default) that path IS `Contents/MacOS/PRX`.
pub const APP_CLI: &str = "Contents/Helpers/prx";
/// The login item's launch flag: menu bar + daemon, no window.
pub const APP_BACKGROUND_FLAG: &str = "--background";
/// Overrides the bundle search directories (colon-separated; empty = none).
/// A test hook: keeps CLI tests hermetic on machines with PRX.app installed.
pub const APP_SEARCH_DIRS_ENV: &str = "PRX_APP_SEARCH_DIRS";

// --------------------------------------------------------------- entry points

/// Register login autostart for the running prx binary, resolving what to
/// register per D5: the `--binary-path` override (absolute enforced), else
/// `~/.cargo/bin/prx` when the running binary is a `target/` build, else the
/// running binary itself (warning when that lies in a build directory). With
/// `include_menubar`, the menubar agent is resolved and registered too.
///
/// On macOS a PRX.app bundle takes precedence (A4): `app` is an explicit
/// bundle path; otherwise the bundle enclosing the running binary or an
/// installed `PRX.app` is used. An explicit `--binary-path` asks for the
/// headless daemon registration and skips the bundle search.
pub fn install(
    binary_path: Option<&Path>,
    include_menubar: bool,
    app: Option<&Path>,
) -> Result<()> {
    let exe = std::env::current_exe()
        .context("cannot locate the prx binary (current_exe unavailable)")?;
    let home = dirs::home_dir().context("cannot determine the home directory ($HOME)")?;
    let cargo_bin = home.join(".cargo").join("bin");
    let search = app_search_dirs(&home);
    install_discovering_with(
        &exe,
        &home,
        &cargo_bin,
        &search,
        binary_path,
        include_menubar,
        app,
    )
}

/// Test seam for the full A4 flow: `exe` plays `current_exe()`, `search`
/// the bundle search directories. A found bundle registers the app login
/// item; otherwise the headless D5 flow runs unchanged.
pub fn install_discovering_with(
    exe: &Path,
    home: &Path,
    cargo_bin: &Path,
    search: &[PathBuf],
    binary_path: Option<&Path>,
    include_menubar: bool,
    app: Option<&Path>,
) -> Result<()> {
    if cfg!(target_os = "macos") && (app.is_some() || binary_path.is_none()) {
        if let Some(bundle) = find_app_bundle(app, exe, search)? {
            if include_menubar {
                println!("note: --include-menubar is implied: PRX.app runs the menu bar");
            }
            return install_app_with(&bundle, home);
        }
    }
    install_resolved_with(exe, home, cargo_bin, binary_path, include_menubar)
}

// ------------------------------------------------------- PRX.app (A4)

/// Where installed bundles are looked for: `/Applications` then
/// `~/Applications` (or the [`APP_SEARCH_DIRS_ENV`] override).
pub fn app_search_dirs(home: &Path) -> Vec<PathBuf> {
    if let Some(value) = std::env::var_os(APP_SEARCH_DIRS_ENV) {
        return std::env::split_paths(&value)
            .filter(|dir| !dir.as_os_str().is_empty())
            .collect();
    }
    vec![PathBuf::from("/Applications"), home.join("Applications")]
}

/// A directory is a PRX bundle when it holds `Contents/MacOS/PRX`.
pub fn is_app_bundle(path: &Path) -> bool {
    path.join("Contents")
        .join("MacOS")
        .join(APP_EXECUTABLE)
        .is_file()
}

/// The bundle enclosing `exe` (`X.app/Contents/MacOS/PRX` or
/// `X.app/Contents/Helpers/prx`, symlinks resolved), when it is a PRX
/// bundle.
pub fn enclosing_bundle(exe: &Path) -> Option<PathBuf> {
    bundle_around(exe).or_else(|| bundle_around(&std::fs::canonicalize(exe).ok()?))
}

fn bundle_around(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let contents = dir.parent()?;
    let bundle = contents.parent()?;
    let shaped = (dir.file_name()? == "MacOS" || dir.file_name()? == "Helpers")
        && contents.file_name()? == "Contents"
        && bundle.extension().is_some_and(|ext| ext == "app");
    (shaped && is_app_bundle(bundle)).then(|| bundle.to_path_buf())
}

/// A4 discovery order: the explicit bundle (an error when it is not a PRX
/// bundle), the bundle enclosing `exe`, then `<dir>/PRX.app` for each search
/// directory. `None` means the headless registration applies.
pub fn find_app_bundle(
    explicit: Option<&Path>,
    exe: &Path,
    search_dirs: &[PathBuf],
) -> Result<Option<PathBuf>> {
    if let Some(given) = explicit {
        if !given.is_absolute() {
            bail!("the app path must be absolute (got `{}`)", given.display());
        }
        if !is_app_bundle(given) {
            bail!(
                "`{}` is not a PRX.app bundle (no Contents/MacOS/{APP_EXECUTABLE})",
                given.display()
            );
        }
        return Ok(Some(given.to_path_buf()));
    }
    if let Some(bundle) = enclosing_bundle(exe) {
        return Ok(Some(bundle));
    }
    Ok(search_dirs
        .iter()
        .map(|dir| dir.join(APP_BUNDLE_NAME))
        .find(|bundle| is_app_bundle(bundle)))
}

/// The app's login-item plist path.
pub fn app_plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{APP_LABEL}.plist"))
}

/// The login item (A4): `PRX --background` at login; `KeepAlive {
/// SuccessfulExit = false }` relaunches a crashed app while Quit (exit 0)
/// stays quit until the next login.
pub fn app_plist_contents(bundle: &Path) -> String {
    let exe = bundle.join("Contents").join("MacOS").join(APP_EXECUTABLE);
    let exe = xml_escape(&exe.display().to_string());
    let path = xml_escape(&launchd_path());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{APP_LABEL}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>{APP_BACKGROUND_FLAG}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#
    )
}

/// Register the `com.prx.app` login item for `bundle` under `home` and
/// remove the legacy daemon/menubar agents (A4). Never loads anything.
pub fn install_app_with(bundle: &Path, home: &Path) -> Result<()> {
    if !bundle.is_absolute() {
        bail!("the app path must be absolute (got `{}`)", bundle.display());
    }
    let plist = app_plist_path(home);
    let dir = plist
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plist path has no parent: {}", plist.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::write(&plist, app_plist_contents(bundle))
        .with_context(|| format!("cannot write {}", plist.display()))?;
    println!("wrote {}", plist.display());
    println!(
        "  runs: {} {APP_BACKGROUND_FLAG}",
        bundle
            .join("Contents")
            .join("MacOS")
            .join(APP_EXECUTABLE)
            .display()
    );
    for legacy in [plist_path(home), menubar_plist_path(home)] {
        if legacy.exists() {
            std::fs::remove_file(&legacy)
                .with_context(|| format!("cannot remove {}", legacy.display()))?;
            println!("removed legacy agent {}", legacy.display());
        }
    }
    println!("autostart registered — PRX opens in the menu bar at your next login");
    Ok(())
}

/// Remove only the app's login item (the "Open at login" toggle off).
pub fn uninstall_app_with(home: &Path) -> Result<bool> {
    let plist = app_plist_path(home);
    if !plist.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&plist).with_context(|| format!("cannot remove {}", plist.display()))?;
    println!("removed {}", plist.display());
    Ok(true)
}

/// The app login item's registration; `binary` is `Contents/MacOS/PRX`.
pub fn app_status_with(home: &Path) -> Status {
    if cfg!(target_os = "macos") {
        registration_status(&app_plist_path(home), plist_binary)
    } else {
        Status {
            installed: false,
            path: None,
            binary: None,
        }
    }
}

/// [`app_status_with`] for the current user's home.
pub fn app_status() -> Status {
    match dirs::home_dir() {
        Some(home) => app_status_with(&home),
        None => Status {
            installed: false,
            path: None,
            binary: None,
        },
    }
}

/// The bundle a registered `Contents/MacOS/PRX` path belongs to.
pub fn bundle_of_executable(binary: &Path) -> Option<PathBuf> {
    Some(binary.parent()?.parent()?.parent()?.to_path_buf())
}

/// Remove the login-autostart registrations (daemon and, when present, the
/// menubar agent). Idempotent: succeeds with a "not installed" note when
/// there is nothing to remove.
pub fn uninstall() -> Result<()> {
    let home = dirs::home_dir().context("cannot determine the home directory ($HOME)")?;
    uninstall_with(&home)
}

/// Test seam: install the given binary as-is (no resolution, no menubar),
/// like the flag-free CLI install would.
pub fn install_with(exe: &Path, home: &Path) -> Result<()> {
    install_resolved_with(exe, home, &home.join(".cargo").join("bin"), None, false)
}

/// Test seam for the full resolved flow: `exe` plays `current_exe()`,
/// `cargo_bin` plays `~/.cargo/bin`. Everything is resolved before anything
/// is written, so a menubar resolution failure leaves the registration
/// exactly as the flag-free install would have made it.
pub fn install_resolved_with(
    exe: &Path,
    home: &Path,
    cargo_bin: &Path,
    binary_path: Option<&Path>,
    include_menubar: bool,
) -> Result<()> {
    let resolved = resolve_daemon_binary(binary_path, exe, cargo_bin)?;
    if !resolved.is_absolute() {
        bail!(
            "the registered binary path must be absolute (got `{}`)",
            resolved.display()
        );
    }
    // Rule 3's warning: a build-directory registration breaks on `cargo
    // clean` or a moved checkout; name the stable-alternative remedy.
    if is_in_target_dir(&resolved) {
        eprintln!(
            "warning: registering the build-directory binary {}",
            resolved.display()
        );
        eprintln!("  this registration breaks on `cargo clean` or a moved checkout;");
        eprintln!(
            "  run `cargo install --path .` and re-run `prx daemon install` \
             (or pass `--binary-path` with the stable binary)"
        );
    }
    // Menubar resolution happens before any write: the loud-failure scenario
    // promises the daemon registration is left as the flag-free install
    // would have made it.
    let menubar = if !include_menubar {
        None
    } else if cfg!(target_os = "macos") {
        Some(resolve_menubar_binary(&resolved, cargo_bin)?)
    } else {
        if cfg!(target_os = "linux") {
            eprintln!("warning: the menubar agent is macOS-only; skipping its registration");
        }
        None
    };
    if cfg!(target_os = "macos") {
        install_macos(&resolved, home, menubar.as_deref())
    } else if cfg!(target_os = "linux") {
        install_linux(&resolved, home)
    } else {
        bail!("login autostart is not supported on this platform")
    }
}

/// D5 resolution order for the daemon binary: (1) the `--binary-path`
/// override (relative paths are rejected); (2) `~/.cargo/bin/prx` when the
/// running binary sits inside a Cargo build directory (`target/` component)
/// and a cargo-installed prx exists; (3) the running binary itself.
pub fn resolve_daemon_binary(
    binary_path: Option<&Path>,
    exe: &Path,
    cargo_bin: &Path,
) -> Result<PathBuf> {
    if let Some(given) = binary_path {
        if !given.is_absolute() {
            bail!(
                "the registered binary path must be absolute (got `{}`)",
                given.display()
            );
        }
        return Ok(given.to_path_buf());
    }
    let stable = cargo_bin.join("prx");
    if is_in_target_dir(exe) && stable.exists() {
        return Ok(stable);
    }
    Ok(exe.to_path_buf())
}

/// The menubar binary: `prx-menubar` next to the resolved daemon binary,
/// falling back to `~/.cargo/bin/prx-menubar`. Fails naming the expected
/// locations when neither exists.
pub fn resolve_menubar_binary(daemon: &Path, cargo_bin: &Path) -> Result<PathBuf> {
    let sibling = daemon.parent().map(|dir| dir.join(MENUBAR_BINARY_NAME));
    let fallback = cargo_bin.join(MENUBAR_BINARY_NAME);
    let mut candidates: Vec<PathBuf> = sibling.into_iter().collect();
    if !candidates.contains(&fallback) {
        candidates.push(fallback);
    }
    if let Some(found) = candidates.iter().find(|c| c.exists()) {
        return Ok(found.clone());
    }
    let looked = candidates
        .iter()
        .map(|p| format!("`{}`", p.display()))
        .collect::<Vec<_>>()
        .join(" and ");
    bail!(
        "cannot find the {MENUBAR_BINARY_NAME} binary: looked at {looked}. Build it next to \
         the daemon binary (or `cargo install --path menubar`), then re-run \
         `prx daemon install --include-menubar`"
    )
}

/// Is `path` inside a Cargo build directory (any `target` path component)?
/// Public so `prx doctor` can flag a build-directory registration with the
/// same whole-component rule install warns with.
pub fn is_in_target_dir(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == "target")
}

/// Test seam: uninstall from an explicit home directory.
pub fn uninstall_with(home: &Path) -> Result<()> {
    if cfg!(target_os = "macos") {
        uninstall_macos(home)
    } else if cfg!(target_os = "linux") {
        uninstall_linux(home)
    } else {
        bail!("login autostart is not supported on this platform")
    }
}

/// What `prx doctor` needs to know about the current registration.
pub struct Status {
    /// A registration file exists.
    pub installed: bool,
    /// The registration file (plist or unit), when installed.
    pub path: Option<PathBuf>,
    /// The binary path recorded in the registration, when it can be parsed.
    pub binary: Option<PathBuf>,
}

/// Inspect the registration for the current user's home.
pub fn status() -> Status {
    match dirs::home_dir() {
        Some(home) => status_with(&home),
        None => Status {
            installed: false,
            path: None,
            binary: None,
        },
    }
}

/// Test seam: inspect the registration under an explicit home directory.
pub fn status_with(home: &Path) -> Status {
    if cfg!(target_os = "macos") {
        registration_status(&plist_path(home), plist_binary)
    } else {
        registration_status(&unit_path(home), unit_binary)
    }
}

/// Inspect the menubar agent's registration for the current user's home.
/// The menubar is macOS-only, so every other platform reports not installed.
pub fn menubar_status() -> Status {
    match dirs::home_dir() {
        Some(home) => menubar_status_with(&home),
        None => Status {
            installed: false,
            path: None,
            binary: None,
        },
    }
}

/// Test seam: inspect the menubar agent's registration under an explicit
/// home directory.
pub fn menubar_status_with(home: &Path) -> Status {
    if cfg!(target_os = "macos") {
        registration_status(&menubar_plist_path(home), plist_binary)
    } else {
        Status {
            installed: false,
            path: None,
            binary: None,
        }
    }
}

fn registration_status(path: &Path, parse: fn(&str) -> Option<PathBuf>) -> Status {
    match std::fs::read_to_string(path) {
        Ok(contents) => Status {
            installed: true,
            path: Some(path.to_path_buf()),
            binary: parse(&contents),
        },
        Err(_) => Status {
            installed: false,
            path: None,
            binary: None,
        },
    }
}

// ---------------------------------------------------------------------- macOS

fn plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// Where the menubar agent's plist lands: next to the daemon's, same
/// convention. (Written by the opt-in `--include-menubar` install.)
pub fn menubar_plist_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join(format!("{MENUBAR_LABEL}.plist"))
}

/// launchd runs agents with a bare `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`)
/// that cannot resolve homebrew-installed tools (`gh`, the review agent), so
/// the installing shell's `PATH` is snapshotted into `EnvironmentVariables` —
/// the same stable-registration tradeoff as the `--binary-path` override:
/// pinned at install time, refreshed by reinstalling.
fn launchd_path() -> String {
    std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".to_string())
}

/// The launchd plist: `RunAtLoad` starts the daemon at login, in the
/// foreground, so launchd owns the process and can supervise it (D4):
/// `KeepAlive { SuccessfulExit = false }` relaunches a crashed (non-zero
/// exit) daemon, while a clean exit 0 — including `prx daemon stop`'s
/// graceful SIGTERM shutdown — stands the supervisor down, so stop is not
/// fought. The pid-file guard (A16) keeps a launchd + manual double start a
/// no-op.
fn plist_contents(exe: &Path) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let path = xml_escape(&launchd_path());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>start</string>
        <string>--foreground</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
</dict>
</plist>
"#
    )
}

/// The menubar agent's launchd plist (D4): `RunAtLoad` starts the menubar at
/// login; the same `KeepAlive` as the daemon relaunches a crashed shell,
/// while the menubar's own Quit (a clean exit 0) keeps it quit until the
/// next login. The binary is the sole `ProgramArguments` entry.
pub fn menubar_plist_contents(exe: &Path) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let path = xml_escape(&launchd_path());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{MENUBAR_LABEL}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
    </dict>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
</dict>
</plist>
"#
    )
}

fn install_macos(exe: &Path, home: &Path, menubar: Option<&Path>) -> Result<()> {
    let plist = plist_path(home);
    let dir = plist
        .parent()
        .ok_or_else(|| anyhow::anyhow!("plist path has no parent: {}", plist.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    // Overwriting an existing plist is the reinstall path: the same fixed
    // path means a second install replaces in place, never duplicates.
    std::fs::write(&plist, plist_contents(exe))
        .with_context(|| format!("cannot write {}", plist.display()))?;
    println!("wrote {}", plist.display());
    println!("  runs: {} daemon start --foreground", exe.display());
    // Switching back to the headless registration: one login item only.
    let app_plist = app_plist_path(home);
    if app_plist.exists() {
        std::fs::remove_file(&app_plist)
            .with_context(|| format!("cannot remove {}", app_plist.display()))?;
        println!("removed {}", app_plist.display());
    }
    if let Some(menubar_exe) = menubar {
        let menubar_plist = menubar_plist_path(home);
        std::fs::write(&menubar_plist, menubar_plist_contents(menubar_exe))
            .with_context(|| format!("cannot write {}", menubar_plist.display()))?;
        println!("wrote {}", menubar_plist.display());
        println!("  runs: {}", menubar_exe.display());
    }
    println!("autostart registered — the daemon will start at your next login");
    println!(
        "  (to load it now instead: `launchctl load {}`)",
        plist.display()
    );
    Ok(())
}

fn uninstall_macos(home: &Path) -> Result<()> {
    // File removal only: a running app or daemon is never signalled.
    let removed_app = uninstall_app_with(home)?;
    let mut removed_any = removed_app;
    let mut removed_daemon = false;
    let plist = plist_path(home);
    if plist.exists() {
        std::fs::remove_file(&plist)
            .with_context(|| format!("cannot remove {}", plist.display()))?;
        println!("removed {}", plist.display());
        removed_any = true;
        removed_daemon = true;
    }
    let menubar_plist = menubar_plist_path(home);
    if menubar_plist.exists() {
        std::fs::remove_file(&menubar_plist)
            .with_context(|| format!("cannot remove {}", menubar_plist.display()))?;
        println!("removed {}", menubar_plist.display());
        removed_any = true;
    } else if removed_daemon {
        // Daemon-only install: the absent menubar is reported, not an error.
        println!("  (no menubar agent was installed)");
    }
    if !removed_any {
        println!("autostart was not installed");
    }
    Ok(())
}

/// Extract the registered binary from our own plist: the first `<string>`
/// inside the `ProgramArguments` array. Minimal by design — we wrote the
/// file, so the shape is fixed.
fn plist_binary(contents: &str) -> Option<PathBuf> {
    let after = contents.split_once("<key>ProgramArguments</key>")?.1;
    let body = after.split_once("<array>")?.1;
    let body = body.split_once("</array>")?.0;
    let start = body.find("<string>")? + "<string>".len();
    let rest = &body[start..];
    let end = rest.find("</string>")?;
    Some(PathBuf::from(xml_unescape(rest[..end].trim())))
}

/// Escape the XML text entities that can appear in a filesystem path.
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Inverse of [`xml_escape`] (`&amp;` last so entities are not double-folded).
fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

// ---------------------------------------------------------------------- Linux

fn unit_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("systemd")
        .join("user")
        .join(UNIT_NAME)
}

/// The systemd user unit: systemd supervises the process, so the unit runs
/// the foreground daemon (not the detached start). `WantedBy` lives under
/// `[Install]` — that is where `systemctl enable` reads it from.
fn unit_contents(exe: &Path) -> String {
    let exe = exe.display();
    format!(
        r#"[Unit]
Description=prx daemon (local-first PR review lifecycle manager)

[Service]
ExecStart={exe} daemon start --foreground
Restart=on-failure

[Install]
WantedBy=default.target
"#
    )
}

fn write_unit(exe: &Path, home: &Path) -> Result<PathBuf> {
    let unit = unit_path(home);
    let dir = unit
        .parent()
        .ok_or_else(|| anyhow::anyhow!("unit path has no parent: {}", unit.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    std::fs::write(&unit, unit_contents(exe))
        .with_context(|| format!("cannot write {}", unit.display()))?;
    Ok(unit)
}

fn install_linux(exe: &Path, home: &Path) -> Result<()> {
    let unit = write_unit(exe, home)?;
    println!("wrote {}", unit.display());
    println!("  runs: {} daemon start --foreground", exe.display());
    // systemd is unavailable (container, WSL1, no user session): leave the
    // unit file in place and report the failure honestly — install never
    // silently half-succeeds.
    systemctl(&["--user", "daemon-reload"]).context(
        "unit file written, but `systemctl --user daemon-reload` failed (is systemd running?)",
    )?;
    systemctl(&["--user", "enable", UNIT_NAME]).with_context(|| {
        format!("unit file written, but `systemctl --user enable {UNIT_NAME}` failed")
    })?;
    println!("enabled {UNIT_NAME} — starts at login via systemd");
    Ok(())
}

fn uninstall_linux(home: &Path) -> Result<()> {
    let unit = unit_path(home);
    if !unit.exists() {
        println!("autostart was not installed");
        return Ok(());
    }
    // Plain disable — never `--now`, which would stop a running daemon.
    match std::process::Command::new("systemctl")
        .args(["--user", "disable", UNIT_NAME])
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => bail!(
            "`systemctl --user disable {UNIT_NAME}` failed: {}; the unit file was left in place",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        // No systemctl at all: nothing was enabled, so removing the file is
        // the whole job; say so instead of failing.
        Err(_) => {
            eprintln!("note: systemctl unavailable; removing the unit file without disabling")
        }
    }
    std::fs::remove_file(&unit).with_context(|| format!("cannot remove {}", unit.display()))?;
    // Best-effort: make systemd forget the removed unit.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();
    println!("removed {}", unit.display());
    Ok(())
}

/// Run `systemctl` with `args`: Ok only when the binary exists and exits 0.
fn systemctl(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("cannot run systemctl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        bail!(
            "`systemctl {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
    }
}

/// Extract the registered binary from our own unit: the first token of the
/// `ExecStart=` line (written as an unquoted absolute path by `install`).
fn unit_binary(contents: &str) -> Option<PathBuf> {
    contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("ExecStart="))?
        .split_whitespace()
        .next()
        .map(PathBuf::from)
}

// --------------------------------------------------------------- unit tests

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prx-autostart-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn plist_carries_label_runatload_foreground_and_keepalive() {
        let contents = plist_contents(Path::new("/opt/prx/bin/prx"));
        assert!(contents.contains("<string>com.prx.daemon</string>"));
        assert!(contents.contains("<key>RunAtLoad</key>"));
        assert!(contents.contains("<true/>"));
        // D4 supervision: launchd runs the foreground daemon.
        assert!(contents.contains("<string>/opt/prx/bin/prx</string>"));
        assert!(contents.contains("<string>daemon</string>"));
        assert!(contents.contains("<string>start</string>"));
        assert!(contents.contains("<string>--foreground</string>"));
        // KeepAlive restricted to unsuccessful exits: a crash is relaunched,
        // a clean exit (incl. `prx daemon stop`) is not.
        assert!(contents.contains("<key>KeepAlive</key>"));
        assert!(contents.contains("<key>SuccessfulExit</key>"));
        assert!(contents.contains("<false/>"));
    }

    #[test]
    fn menubar_plist_carries_label_runatload_keepalive_and_sole_binary() {
        let contents = menubar_plist_contents(Path::new("/opt/prx/bin/prx-menubar"));
        assert!(contents.contains("<string>com.prx.menubar</string>"));
        assert!(contents.contains("<key>RunAtLoad</key>"));
        assert!(contents.contains("<true/>"));
        // Same supervision as the daemon (D4).
        assert!(contents.contains("<key>KeepAlive</key>"));
        assert!(contents.contains("<key>SuccessfulExit</key>"));
        assert!(contents.contains("<false/>"));
        // The binary is the sole ProgramArguments entry: no subcommand.
        let args = contents
            .split_once("<array>")
            .unwrap()
            .1
            .split_once("</array>")
            .unwrap()
            .0;
        assert!(args.contains("<string>/opt/prx/bin/prx-menubar</string>"));
        assert_eq!(args.matches("<string>").count(), 1);
    }

    #[test]
    fn plist_binary_extracts_first_program_argument() {
        let contents = plist_contents(Path::new("/opt/prx/bin/prx"));
        assert_eq!(
            plist_binary(&contents),
            Some(PathBuf::from("/opt/prx/bin/prx"))
        );
    }

    #[test]
    fn plist_binary_extracts_the_menubar_binary_from_its_plist() {
        let contents = menubar_plist_contents(Path::new("/opt/prx/bin/prx-menubar"));
        assert_eq!(
            plist_binary(&contents),
            Some(PathBuf::from("/opt/prx/bin/prx-menubar"))
        );
    }

    #[test]
    fn plist_binary_unescapes_xml_entities_in_paths() {
        let contents = plist_contents(Path::new("/opt/a&b<c>/prx"));
        assert_eq!(
            plist_binary(&contents),
            Some(PathBuf::from("/opt/a&b<c>/prx"))
        );
    }

    #[test]
    fn plist_binary_is_none_for_garbage() {
        assert_eq!(plist_binary("not a plist"), None);
    }

    #[test]
    fn unit_carries_exec_start_restart_and_wantedby() {
        let contents = unit_contents(Path::new("/usr/local/bin/prx"));
        assert!(contents.contains("ExecStart=/usr/local/bin/prx daemon start --foreground"));
        assert!(contents.contains("Restart=on-failure"));
        assert!(contents.contains("WantedBy=default.target"));
    }

    #[test]
    fn unit_binary_extracts_exec_start_token() {
        let contents = unit_contents(Path::new("/usr/local/bin/prx"));
        assert_eq!(
            unit_binary(&contents),
            Some(PathBuf::from("/usr/local/bin/prx"))
        );
        assert_eq!(unit_binary("no ExecStart here"), None);
    }

    #[test]
    fn status_with_reports_missing_registration() {
        let home = temp_home("missing");
        let st = status_with(&home);
        assert!(!st.installed);
        assert!(st.path.is_none());
        assert!(st.binary.is_none());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn menubar_status_with_reports_missing_registration() {
        let home = temp_home("menubar-missing-status");
        let st = menubar_status_with(&home);
        assert!(!st.installed);
        assert!(st.path.is_none());
        assert!(st.binary.is_none());
        std::fs::remove_dir_all(&home).unwrap();
    }

    // ------------------------------------------- D5 binary resolution (pure)

    /// Create an empty file (existence is all the resolution checks).
    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn is_in_target_dir_matches_any_target_component() {
        assert!(is_in_target_dir(Path::new("/repo/target/debug/prx")));
        assert!(is_in_target_dir(Path::new("/weird/target/prx")));
        assert!(!is_in_target_dir(Path::new("/usr/local/bin/prx")));
        // Whole-component match only: `targets` and `target-layout` are not
        // build directories.
        assert!(!is_in_target_dir(Path::new("/opt/targets/prx")));
        assert!(!is_in_target_dir(Path::new("/opt/target-layout/prx")));
    }

    #[test]
    fn resolve_daemon_prefers_an_absolute_override_over_everything() {
        let home = temp_home("res-override");
        let cargo_bin = home.join(".cargo/bin");
        touch(&cargo_bin.join("prx"));
        let given = Path::new("/opt/prx/bin/prx");
        assert_eq!(
            resolve_daemon_binary(Some(given), Path::new("/repo/target/debug/prx"), &cargo_bin)
                .unwrap(),
            given
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn resolve_daemon_rejects_a_relative_override() {
        let home = temp_home("res-relative");
        let err = resolve_daemon_binary(
            Some(Path::new("relative/prx")),
            Path::new("/repo/target/debug/prx"),
            &home.join(".cargo/bin"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("absolute"), "{err}");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn resolve_daemon_prefers_cargo_bin_over_a_target_dir_exe() {
        let home = temp_home("res-cargo");
        let cargo_bin = home.join(".cargo/bin");
        let stable = cargo_bin.join("prx");
        touch(&stable);
        let built = Path::new("/repo/target/debug/prx");
        assert_eq!(
            resolve_daemon_binary(None, built, &cargo_bin).unwrap(),
            stable
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn resolve_daemon_keeps_the_target_dir_exe_without_a_cargo_install() {
        let home = temp_home("res-no-cargo");
        let built = Path::new("/repo/target/debug/prx");
        assert_eq!(
            resolve_daemon_binary(None, built, &home.join(".cargo/bin")).unwrap(),
            built
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn resolve_daemon_keeps_a_non_target_exe_even_with_a_cargo_install() {
        let home = temp_home("res-stable-exe");
        let cargo_bin = home.join(".cargo/bin");
        touch(&cargo_bin.join("prx"));
        let installed = Path::new("/usr/local/bin/prx");
        assert_eq!(
            resolve_daemon_binary(None, installed, &cargo_bin).unwrap(),
            installed
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn resolve_menubar_prefers_the_sibling_of_the_daemon() {
        let home = temp_home("res-menubar-sibling");
        let cargo_bin = home.join(".cargo/bin");
        touch(&cargo_bin.join("prx-menubar"));
        let bin = home.join("bin");
        let daemon = bin.join("prx");
        touch(&daemon);
        touch(&bin.join("prx-menubar"));
        assert_eq!(
            resolve_menubar_binary(&daemon, &cargo_bin).unwrap(),
            bin.join("prx-menubar")
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn resolve_menubar_falls_back_to_cargo_bin() {
        let home = temp_home("res-menubar-fallback");
        let cargo_bin = home.join(".cargo/bin");
        touch(&cargo_bin.join("prx-menubar"));
        let daemon = home.join("bin/prx");
        touch(&daemon);
        assert_eq!(
            resolve_menubar_binary(&daemon, &cargo_bin).unwrap(),
            cargo_bin.join("prx-menubar")
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn resolve_menubar_fails_naming_the_expected_locations() {
        let home = temp_home("res-menubar-missing");
        let cargo_bin = home.join(".cargo/bin");
        let daemon = home.join("bin/prx");
        touch(&daemon);
        let err = resolve_menubar_binary(&daemon, &cargo_bin).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("prx-menubar"), "{msg}");
        assert!(
            msg.contains(&home.join("bin/prx-menubar").display().to_string()),
            "{msg}"
        );
        assert!(
            msg.contains(&cargo_bin.join("prx-menubar").display().to_string()),
            "{msg}"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }
}

/// macOS-only roundtrips through the real install/uninstall paths (the plist
/// platform — no launchd involvement, pure file writes).
#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prx-autostart-macos-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Fake `prx` + `prx-menubar` binaries inside `bin_dir`.
    fn fake_pair(bin_dir: &Path) -> (PathBuf, PathBuf) {
        std::fs::create_dir_all(bin_dir).unwrap();
        let prx = bin_dir.join("prx");
        let menubar = bin_dir.join("prx-menubar");
        std::fs::write(&prx, b"").unwrap();
        std::fs::write(&menubar, b"").unwrap();
        (prx, menubar)
    }

    #[test]
    fn install_then_status_roundtrip_and_uninstall() {
        let home = temp_home("roundtrip");
        let fake = home.join("fake-prx");
        install_with(&fake, &home).unwrap();

        let st = status_with(&home);
        assert!(st.installed);
        assert_eq!(st.path, Some(plist_path(&home)));
        assert_eq!(st.binary, Some(fake));

        uninstall_with(&home).unwrap();
        assert!(!status_with(&home).installed);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn uninstall_without_install_succeeds() {
        let home = temp_home("clean");
        uninstall_with(&home).unwrap();
        assert!(!status_with(&home).installed);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn install_twice_overwrites_in_place() {
        let home = temp_home("twice");
        let first = home.join("first-prx");
        let second = home.join("second-prx");
        install_with(&first, &home).unwrap();
        install_with(&second, &home).unwrap();

        let plist = plist_path(&home);
        let agents = plist.parent().unwrap();
        assert_eq!(std::fs::read_dir(agents).unwrap().count(), 1);
        assert_eq!(status_with(&home).binary, Some(second));
        // The flag-free install never writes the menubar agent.
        assert!(!menubar_plist_path(&home).exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn install_rejects_relative_binary_path() {
        let home = temp_home("relative");
        assert!(install_with(Path::new("relative/prx"), &home).is_err());
        assert!(!status_with(&home).installed);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn include_menubar_writes_both_plists_and_uninstall_removes_both() {
        let home = temp_home("menubar-roundtrip");
        let (prx, menubar) = fake_pair(&home.join("bin"));
        install_resolved_with(&prx, &home, &home.join(".cargo/bin"), None, true).unwrap();

        let daemon_plist = plist_path(&home);
        let menubar_plist = menubar_plist_path(&home);
        let daemon_contents = std::fs::read_to_string(&daemon_plist).unwrap();
        let menubar_contents = std::fs::read_to_string(&menubar_plist).unwrap();
        assert_eq!(plist_binary(&daemon_contents), Some(prx));
        assert!(menubar_contents.contains("<string>com.prx.menubar</string>"));
        assert!(menubar_contents.contains("<key>RunAtLoad</key>"));
        assert!(menubar_contents.contains("<false/>"));
        assert_eq!(plist_binary(&menubar_contents), Some(menubar));

        uninstall_with(&home).unwrap();
        assert!(!daemon_plist.exists());
        assert!(!menubar_plist.exists());
        assert!(!status_with(&home).installed);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn include_menubar_twice_replaces_in_place_exactly_two_plists() {
        let home = temp_home("menubar-twice");
        let (first, _) = fake_pair(&home.join("bin1"));
        let (second, second_menubar) = fake_pair(&home.join("bin2"));
        install_resolved_with(&first, &home, &home.join(".cargo/bin"), None, true).unwrap();
        install_resolved_with(&second, &home, &home.join(".cargo/bin"), None, true).unwrap();

        let plist = plist_path(&home);
        let agents = plist.parent().unwrap();
        assert_eq!(std::fs::read_dir(agents).unwrap().count(), 2);
        assert_eq!(
            plist_binary(&std::fs::read_to_string(plist_path(&home)).unwrap()),
            Some(second)
        );
        assert_eq!(
            plist_binary(&std::fs::read_to_string(menubar_plist_path(&home)).unwrap()),
            Some(second_menubar)
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn missing_menubar_fails_loudly_leaving_the_daemon_flag_free() {
        let home = temp_home("menubar-missing");
        let cargo_bin = home.join(".cargo/bin");
        // Only the daemon binary exists — no sibling, no cargo-installed menubar.
        let bin = home.join("bin");
        let prx = bin.join("prx");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(&prx, b"").unwrap();

        // Fresh machine: the loud failure writes nothing at all, naming the
        // expected locations.
        let err = install_resolved_with(&prx, &home, &cargo_bin, None, true).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("prx-menubar"), "{msg}");
        assert!(
            msg.contains(&home.join("bin/prx-menubar").display().to_string()),
            "{msg}"
        );
        assert!(
            msg.contains(&cargo_bin.join("prx-menubar").display().to_string()),
            "{msg}"
        );
        assert!(!plist_path(&home).exists());
        assert!(!menubar_plist_path(&home).exists());

        // An existing flag-free registration is left exactly as the
        // flag-free install would have made it.
        install_with(&prx, &home).unwrap();
        let before = std::fs::read_to_string(plist_path(&home)).unwrap();
        install_resolved_with(&prx, &home, &cargo_bin, None, true).unwrap_err();
        assert_eq!(std::fs::read_to_string(plist_path(&home)).unwrap(), before);
        assert!(!menubar_plist_path(&home).exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn uninstall_twice_with_menubar_stays_idempotent() {
        let home = temp_home("menubar-uninstall-twice");
        let (prx, _) = fake_pair(&home.join("bin"));
        install_resolved_with(&prx, &home, &home.join(".cargo/bin"), None, true).unwrap();
        uninstall_with(&home).unwrap();
        // Second uninstall: nothing left, still succeeds.
        uninstall_with(&home).unwrap();
        assert!(!plist_path(&home).exists());
        assert!(!menubar_plist_path(&home).exists());
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// A fake PRX.app (`Contents/MacOS/PRX` + `prx`) under `dir`.
    fn fake_bundle(dir: &Path) -> PathBuf {
        let bundle = dir.join(APP_BUNDLE_NAME);
        let macos = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join(APP_EXECUTABLE), b"").unwrap();
        std::fs::create_dir_all(bundle.join("Contents/Helpers")).unwrap();
        std::fs::write(bundle.join(APP_CLI), b"").unwrap();
        bundle
    }

    #[test]
    fn app_bundle_discovery_follows_the_a4_order() {
        let home = temp_home("bundle-order");
        let system = home.join("SystemApplications");
        let user = home.join("Applications");
        let search = vec![system.clone(), user.clone()];
        let exe = Path::new("/usr/local/bin/prx");
        // Nothing installed: headless.
        assert_eq!(find_app_bundle(None, exe, &search).unwrap(), None);
        // ~/Applications only.
        let user_bundle = fake_bundle(&user);
        assert_eq!(
            find_app_bundle(None, exe, &search).unwrap(),
            Some(user_bundle.clone())
        );
        // /Applications wins over ~/Applications.
        let system_bundle = fake_bundle(&system);
        assert_eq!(
            find_app_bundle(None, exe, &search).unwrap(),
            Some(system_bundle.clone())
        );
        // The bundle enclosing the running binary wins over both.
        let enclosing = fake_bundle(&home.join("build"));
        let inner = enclosing.join(APP_CLI);
        assert_eq!(
            find_app_bundle(None, &inner, &search).unwrap(),
            Some(enclosing.clone())
        );
        // An explicit --app wins over everything; a non-bundle is an error.
        assert_eq!(
            find_app_bundle(Some(&user_bundle), &inner, &search).unwrap(),
            Some(user_bundle)
        );
        let err = find_app_bundle(Some(&home.join("Nope.app")), exe, &search).unwrap_err();
        assert!(err.to_string().contains("not a PRX.app bundle"), "{err}");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn bundle_install_writes_one_app_item_and_removes_legacy_agents() {
        let home = temp_home("bundle-install");
        let apps = home.join("Applications");
        let bundle = fake_bundle(&apps);
        // Legacy daemon + menubar agents from an older install.
        let (prx, _) = fake_pair(&home.join("bin"));
        install_resolved_with(&prx, &home, &home.join(".cargo/bin"), None, true).unwrap();
        assert!(plist_path(&home).exists() && menubar_plist_path(&home).exists());

        install_discovering_with(
            Path::new("/usr/local/bin/prx"),
            &home,
            &home.join(".cargo/bin"),
            std::slice::from_ref(&apps),
            None,
            false,
            None,
        )
        .unwrap();

        let agents = app_plist_path(&home).parent().unwrap().to_path_buf();
        assert_eq!(std::fs::read_dir(&agents).unwrap().count(), 1);
        let contents = std::fs::read_to_string(app_plist_path(&home)).unwrap();
        assert!(contents.contains("<string>com.prx.app</string>"));
        assert!(contents.contains("<string>--background</string>"));
        assert!(contents.contains("<key>RunAtLoad</key>"));
        assert!(contents.contains("<key>SuccessfulExit</key>"));
        let st = app_status_with(&home);
        assert!(st.installed);
        assert_eq!(st.binary, Some(bundle.join("Contents/MacOS/PRX")));
        assert_eq!(bundle_of_executable(&st.binary.unwrap()), Some(bundle));
        assert!(!status_with(&home).installed);
        assert!(!menubar_status_with(&home).installed);

        // Uninstall removes the app item too.
        uninstall_with(&home).unwrap();
        assert!(!app_status_with(&home).installed);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn no_bundle_install_keeps_the_headless_daemon_agent() {
        let home = temp_home("no-bundle-install");
        let (prx, _) = fake_pair(&home.join("bin"));
        install_discovering_with(
            &prx,
            &home,
            &home.join(".cargo/bin"),
            &[home.join("Applications"), home.join("SystemApplications")],
            None,
            false,
            None,
        )
        .unwrap();
        let st = status_with(&home);
        assert!(st.installed);
        assert_eq!(st.binary, Some(prx));
        assert!(!app_status_with(&home).installed);
        assert!(!menubar_status_with(&home).installed);
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn binary_path_override_skips_the_bundle_and_drops_a_stale_app_item() {
        let home = temp_home("bundle-override");
        let apps = home.join("Applications");
        let bundle = fake_bundle(&apps);
        install_app_with(&bundle, &home).unwrap();
        let (prx, _) = fake_pair(&home.join("bin"));
        install_discovering_with(
            Path::new("/usr/local/bin/prx"),
            &home,
            &home.join(".cargo/bin"),
            std::slice::from_ref(&apps),
            Some(&prx),
            false,
            None,
        )
        .unwrap();
        assert!(status_with(&home).installed);
        assert!(!app_status_with(&home).installed, "one login item only");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn toggling_the_app_item_off_leaves_nothing_registered() {
        let home = temp_home("bundle-toggle");
        let bundle = fake_bundle(&home.join("Applications"));
        install_app_with(&bundle, &home).unwrap();
        assert!(uninstall_app_with(&home).unwrap());
        assert!(!uninstall_app_with(&home).unwrap(), "idempotent");
        assert!(!app_status_with(&home).installed);
        std::fs::remove_dir_all(&home).unwrap();
    }
}

/// Linux-only file-format coverage that needs no systemd user session (the
/// enable step itself requires a live `systemctl --user`).
#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "prx-autostart-linux-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_unit_then_status_roundtrip() {
        let home = temp_home("roundtrip");
        let fake = home.join("fake-prx");
        let unit = write_unit(&fake, &home).unwrap();
        assert_eq!(unit, unit_path(&home));

        let st = status_with(&home);
        assert!(st.installed);
        assert_eq!(st.path, Some(unit));
        assert_eq!(st.binary, Some(fake));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
