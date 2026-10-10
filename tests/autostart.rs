//! `prx daemon install` / `prx daemon uninstall` (the macOS launchd plist)
//! and the `prx doctor` autostart check.
//!
//! HOME is redirected to a temp dir for every CLI run: `dirs::home_dir()`
//! reads `$HOME` on unix, so the registrations land inside the sandbox.
//!
//! The CLI-level scenarios are compiled on macOS (the plist platform). The
//! Linux unit-format coverage lives in `src/autostart.rs` unit tests, because
//! the enable step needs a live `systemctl --user` session that CI cannot
//! rely on; `prx doctor`'s check itself is file-reading only on both
//! platforms.

#![cfg(target_os = "macos")]

mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::Output;

const PLIST: &str = "Library/LaunchAgents/com.prx.daemon.plist";
const MENUBAR_PLIST: &str = "Library/LaunchAgents/com.prx.menubar.plist";

/// The sandbox. Every `TempDir` is held here for the whole test: dropping
/// one deletes its directory tree.
struct Env {
    _cfg_dir: TempDir,
    config: PathBuf,
    data: TempDir,
    home: TempDir,
    gh: TempDir,
}

fn setup(tag: &str) -> Env {
    let cfg_dir = TempDir::new(&format!("auto-{tag}-cfg"));
    let data = TempDir::new(&format!("auto-{tag}-data"));
    let home = TempDir::new(&format!("auto-{tag}-home"));
    let gh = TempDir::new(&format!("auto-{tag}-gh"));
    install_fake_gh(gh.path());

    // init creates the dirs; the sh-agent config keeps every other
    // doctor check green so the autostart line is the only variable.
    let config = cfg_dir.child("config.yml");
    run_cli(&config, data.path(), &["init"], None, &[]);
    write_file(&config, &config_with_sh_agent(&[]));

    Env {
        _cfg_dir: cfg_dir,
        config,
        data,
        home,
        gh,
    }
}

/// Config that overrides the built-in `pi` agent to use `sh` (always on
/// PATH) so the agent check can pass in tests (same trick as cli_doctor.rs).
fn config_with_sh_agent(repos: &[&str]) -> String {
    let mut out = String::from(
        "defaults:\n  agent: pi\n  review_prompt: default\n  cooldown:\n    before_first_review_minutes: 60\n    between_re_reviews_minutes: 30\nagents:\n  pi:\n    command: sh\n    args: [\"-c\", \"true\"]\n",
    );
    out.push_str("repos:\n");
    for repo in repos {
        out.push_str(&format!("  - name: {repo}\n"));
    }
    out
}

/// Run the prx binary with HOME pointed at the sandbox home.
fn run(env: &Env, args: &[&str]) -> Output {
    let home = env.home.path().to_str().expect("temp home is valid utf-8");
    run_cli(
        &env.config,
        env.data.path(),
        args,
        Some(env.gh.path()),
        // No bundle search outside the sandbox: an installed PRX.app on the
        // host must not turn these headless scenarios into app installs.
        &[("HOME", home), ("PRX_APP_SEARCH_DIRS", "")],
    )
}

fn plist_path(env: &Env) -> PathBuf {
    env.home.path().join(PLIST)
}

fn menubar_plist_path(env: &Env) -> PathBuf {
    env.home.path().join(MENUBAR_PLIST)
}

fn plist_contents(env: &Env) -> String {
    String::from_utf8_lossy(&std::fs::read(plist_path(env)).unwrap()).into_owned()
}

fn menubar_plist_contents(env: &Env) -> String {
    String::from_utf8_lossy(&std::fs::read(menubar_plist_path(env)).unwrap()).into_owned()
}

// ------------------------------------------------------- install / uninstall

#[test]
fn install_writes_plist_with_runatload_keepalive_and_the_real_binary() {
    let env = setup("install");
    let out = run(&env, &["daemon", "install"]);
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));

    let plist = plist_contents(&env);
    assert!(plist.contains("<key>Label</key>"));
    assert!(plist.contains("<string>com.prx.daemon</string>"));
    // RunAtLoad true; D4 supervision: KeepAlive on unsuccessful exits only.
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("<true/>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<key>SuccessfulExit</key>"));
    assert!(plist.contains("<false/>"));
    // launchd's bare PATH can't resolve homebrew tools: the installing
    // shell's PATH must be snapshotted into EnvironmentVariables.
    assert!(plist.contains("<key>EnvironmentVariables</key>"));
    assert!(plist.contains("<key>PATH</key>"));
    // ProgramArguments = [<current exe>, "daemon", "start", "--foreground"].
    assert!(plist.contains(&format!("<string>{}</string>", bin())));
    assert!(plist.contains("<string>daemon</string>"));
    assert!(plist.contains("<string>start</string>"));
    assert!(plist.contains("<string>--foreground</string>"));
    // The promise: effective at next login (nothing is loaded now).
    assert!(stdout_of(&out).contains("next login"));
}

#[test]
fn install_twice_replaces_in_place_exactly_one_registration() {
    let env = setup("twice");
    assert!(run(&env, &["daemon", "install"]).status.success());

    // Second install through the library seam with a different binary: the
    // fixed plist path means the registration is replaced, not duplicated.
    let other = env.home.path().join("other-prx");
    prx::autostart::install_with(&other, env.home.path()).unwrap();

    let agents = plist_path(&env).parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read_dir(&agents).unwrap().count(), 1);
    let plist = plist_contents(&env);
    assert!(plist.contains("other-prx"));
    assert!(!plist.contains(bin()));
    // The flag-free install never writes the menubar agent.
    assert!(!menubar_plist_path(&env).exists());
}

#[test]
fn uninstall_removes_the_registration() {
    let env = setup("uninstall");
    assert!(run(&env, &["daemon", "install"]).status.success());
    assert!(plist_path(&env).exists());

    let out = run(&env, &["daemon", "uninstall"]);
    assert!(
        out.status.success(),
        "uninstall failed:\n{}",
        stdout_of(&out)
    );
    assert!(!plist_path(&env).exists());
    assert!(stdout_of(&out).contains("removed"));
}

#[test]
fn uninstall_without_install_exits_zero_reporting_not_installed() {
    let env = setup("clean-uninstall");
    let out = run(&env, &["daemon", "uninstall"]);
    assert!(out.status.success());
    assert!(stdout_of(&out).contains("autostart was not installed"));
}

#[test]
fn install_never_touches_a_running_daemons_files() {
    let env = setup("undisturbed");
    // A live daemon's data-dir artifacts (A16 single-instance guard + IPC).
    let pid = env.data.path().join("daemon.pid");
    let port = env.data.path().join("daemon.port");
    let token = env.data.path().join("daemon.token");
    write_file(&pid, "424242\n");
    write_file(&port, "127.0.0.1:52525\n");
    write_file(&token, "secret\n");

    let out = run(&env, &["daemon", "install"]);
    assert!(out.status.success());
    assert!(plist_path(&env).exists());
    // The registration is a pure file write: daemon state untouched.
    assert_eq!(std::fs::read_to_string(&pid).unwrap(), "424242\n");
    assert_eq!(std::fs::read_to_string(&port).unwrap(), "127.0.0.1:52525\n");
    assert_eq!(std::fs::read_to_string(&token).unwrap(), "secret\n");
}

// ------------------------------------------------- resolution (spec: stable path)

/// Fake daemon + menubar binaries inside the sandbox home.
fn fake_pair(env: &Env, tag: &str) -> (PathBuf, PathBuf) {
    let bin = env.home.path().join(tag);
    let daemon = bin.join("prx");
    let menubar = bin.join("prx-menubar");
    write_file(&daemon, "");
    write_file(&menubar, "");
    (daemon, menubar)
}

#[test]
fn install_prefers_the_cargo_installed_binary_over_the_target_dir() {
    let env = setup("cargo-pref");
    // The CLI binary itself runs from the repo's target/ dir; with a
    // cargo-installed prx in the (redirected) home, that one is registered.
    let stable = env.home.path().join(".cargo/bin/prx");
    write_file(&stable, "");

    let out = run(&env, &["daemon", "install"]);
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));

    let stdout = stdout_of(&out);
    assert!(
        plist_contents(&env).contains(&format!("<string>{}</string>", stable.display())),
        "{stdout}"
    );
    // ...and install says so (the registered binary path is printed).
    assert!(stdout.contains(stable.to_str().unwrap()), "{stdout}");
    assert!(!plist_contents(&env).contains(bin()));
    // The stable choice prints no build-directory warning.
    assert!(!stderr_of(&out).contains("cargo clean"), "{stdout}");
}

#[test]
fn install_binary_path_override_wins_over_the_cargo_installed_binary() {
    let env = setup("override");
    let stable = env.home.path().join(".cargo/bin/prx");
    write_file(&stable, "");
    let (chosen, _) = fake_pair(&env, "chosen");

    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--binary-path",
            chosen.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        plist_contents(&env).contains(&format!("<string>{}</string>", chosen.display())),
        "{stdout}"
    );
    assert!(stdout.contains(chosen.to_str().unwrap()), "{stdout}");
    assert!(!plist_contents(&env).contains(stable.to_str().unwrap()));
}

#[test]
fn install_rejects_a_relative_binary_path() {
    let env = setup("relative-override");
    let out = run(
        &env,
        &["daemon", "install", "--binary-path", "relative/prx"],
    );
    assert!(!out.status.success(), "relative override must fail");
    assert!(stderr_of(&out).contains("absolute"));
    assert!(!plist_path(&env).exists());
}

#[test]
fn install_from_a_target_dir_prints_the_stability_warning() {
    let env = setup("target-warn");
    // No cargo-installed binary in the sandbox home: the running target/
    // binary is registered, with the risk + remedy spelled out.
    let out = run(&env, &["daemon", "install"]);
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));
    assert!(plist_contents(&env).contains(bin()));
    let stderr = stderr_of(&out);
    assert!(stderr.contains("cargo clean"), "{stderr}");
    assert!(stderr.contains("cargo install --path ."), "{stderr}");
    assert!(stderr.contains("--binary-path"), "{stderr}");
}

// --------------------------------------------------------- include-menubar

#[test]
fn install_include_menubar_writes_both_plists_and_uninstall_removes_both() {
    let env = setup("include-menubar");
    let (daemon, menubar) = fake_pair(&env, "bin");

    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--include-menubar",
            "--binary-path",
            daemon.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));

    // Exactly two registrations, side by side in LaunchAgents.
    let agents = plist_path(&env).parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read_dir(&agents).unwrap().count(), 2);
    assert!(
        plist_contents(&env).contains(&format!("<string>{}</string>", daemon.display())),
        "{}",
        plist_contents(&env)
    );
    let menubar_plist = menubar_plist_contents(&env);
    assert!(menubar_plist.contains("<string>com.prx.menubar</string>"));
    assert!(menubar_plist.contains("<key>RunAtLoad</key>"));
    assert!(menubar_plist.contains("<key>KeepAlive</key>"));
    assert!(menubar_plist.contains("<false/>"));
    // The menubar runs its binary (the daemon's sibling) and nothing else.
    assert!(
        menubar_plist.contains(&format!("<string>{}</string>", menubar.display())),
        "{menubar_plist}"
    );
    // Both registered binary paths are printed.
    let stdout = stdout_of(&out);
    assert!(stdout.contains(daemon.to_str().unwrap()), "{stdout}");
    assert!(stdout.contains(menubar.to_str().unwrap()), "{stdout}");

    // Uninstall removes both agents.
    let out = run(&env, &["daemon", "uninstall"]);
    assert!(
        out.status.success(),
        "uninstall failed:\n{}",
        stdout_of(&out)
    );
    assert!(!plist_path(&env).exists());
    assert!(!menubar_plist_path(&env).exists());
    let stdout = stdout_of(&out);
    assert!(stdout.contains(PLIST), "{stdout}");
    assert!(stdout.contains(MENUBAR_PLIST), "{stdout}");
}

#[test]
fn install_include_menubar_twice_replaces_in_place() {
    let env = setup("menubar-twice");
    let install = |tag: &str| {
        let (daemon, _) = fake_pair(&env, tag);
        let out = run(
            &env,
            &[
                "daemon",
                "install",
                "--include-menubar",
                "--binary-path",
                daemon.to_str().unwrap(),
            ],
        );
        assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));
        daemon
    };
    install("bin1");
    let second = install("bin2");

    // Still exactly two registrations, both pointing at the second install.
    let agents = plist_path(&env).parent().unwrap().to_path_buf();
    assert_eq!(std::fs::read_dir(&agents).unwrap().count(), 2);
    assert!(plist_contents(&env).contains(&format!("<string>{}</string>", second.display())));
    assert!(menubar_plist_contents(&env).contains(&format!(
        "<string>{}</string>",
        second.parent().unwrap().join("prx-menubar").display()
    )));
}

#[test]
fn install_include_menubar_without_menubar_binary_fails_loudly() {
    let env = setup("menubar-missing");
    // A daemon override with no prx-menubar next to it and none in the
    // (redirected) ~/.cargo/bin: nothing can be resolved.
    let daemon = env.home.path().join("bin/prx");
    write_file(&daemon, "");

    // Fresh machine: the loud failure names the expected locations and
    // writes nothing at all.
    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--include-menubar",
            "--binary-path",
            daemon.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success(), "missing menubar must fail loudly");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("prx-menubar"), "{stderr}");
    assert!(
        stderr.contains(
            &env.home
                .path()
                .join("bin/prx-menubar")
                .display()
                .to_string()
        ),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            &env.home
                .path()
                .join(".cargo/bin/prx-menubar")
                .display()
                .to_string()
        ),
        "{stderr}"
    );
    assert!(!plist_path(&env).exists());
    assert!(!menubar_plist_path(&env).exists());

    // An existing flag-free registration is left as the flag-free install
    // would have made it.
    assert!(run(&env, &["daemon", "install"]).status.success());
    let before = plist_contents(&env);
    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--include-menubar",
            "--binary-path",
            daemon.to_str().unwrap(),
        ],
    );
    assert!(!out.status.success());
    assert_eq!(plist_contents(&env), before);
    assert!(!menubar_plist_path(&env).exists());
}

#[test]
fn uninstall_reports_the_absent_menubar_without_failing() {
    let env = setup("daemon-only-uninstall");
    assert!(run(&env, &["daemon", "install"]).status.success());

    let out = run(&env, &["daemon", "uninstall"]);
    assert!(
        out.status.success(),
        "uninstall failed:\n{}",
        stdout_of(&out)
    );
    assert!(!plist_path(&env).exists());
    assert!(!menubar_plist_path(&env).exists());
    let stdout = stdout_of(&out);
    assert!(stdout.contains("removed"), "{stdout}");
    assert!(stdout.contains("no menubar agent"), "{stdout}");
}

// ------------------------------------------------------------------- doctor

#[test]
fn doctor_reports_not_installed_on_a_clean_machine() {
    let env = setup("doctor-clean");
    let out = run(&env, &["doctor"]);
    assert!(out.status.success(), "doctor failed:\n{}", stdout_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("autostart"), "{stdout}");
    assert!(
        stdout.contains("not installed (run `prx daemon install`)"),
        "{stdout}"
    );
    // The menubar agent is reported the same way; its absence is a hint too.
    assert!(stdout.contains("menubar autostart"), "{stdout}");
    assert!(
        stdout.contains("not installed (run `prx daemon install --include-menubar`)"),
        "{stdout}"
    );
    assert!(!stdout.contains("WARN"));
}

#[test]
fn doctor_warns_when_the_cli_installed_binary_is_in_a_build_directory() {
    let env = setup("doctor-installed");
    assert!(run(&env, &["daemon", "install"]).status.success());

    // The cargo-built CLI binary lives under target/, so the registration
    // points into a build directory: doctor warns (never fails) while still
    // reporting the registration and the registered binary path.
    let out = run(&env, &["doctor"]);
    assert!(out.status.success(), "doctor failed:\n{}", stdout_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[WARN] autostart"), "{stdout}");
    assert!(
        stdout.contains(&format!("installed ({})", plist_path(&env).display())),
        "{stdout}"
    );
    assert!(stdout.contains(bin()), "{stdout}");
    assert!(stdout.contains("build directory"), "{stdout}");
    assert!(stdout.contains("cargo install --path ."), "{stdout}");
    assert!(stdout.contains("all checks passed"), "{stdout}");
}

#[test]
fn doctor_warns_on_a_stale_registered_binary_and_still_passes() {
    let env = setup("doctor-stale");
    // Register a binary path that never exists on disk (the install seam
    // takes the exe path explicitly, unlike the CLI which uses current_exe).
    let gone = Path::new("/nonexistent/path/to/prx");
    prx::autostart::install_with(gone, env.home.path()).unwrap();

    let out = run(&env, &["doctor"]);
    assert!(out.status.success(), "stale autostart must not fail doctor");
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[WARN] autostart"), "{stdout}");
    assert!(
        stdout.contains("no longer exists"),
        "expected stale-path warning, got: {stdout}"
    );
    // Exit stays 0 and the run still reports success.
    assert!(stdout.contains("all checks passed"), "{stdout}");
}

#[test]
fn doctor_reports_both_agents_after_include_menubar() {
    let env = setup("doctor-menubar");
    let (daemon, menubar) = fake_pair(&env, "bin");
    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--include-menubar",
            "--binary-path",
            daemon.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));

    // Both registrations are reported with their registered binary paths
    // (--binary-path keeps the sandbox binaries outside any target/ dir, so
    // the ok path is deterministic).
    let out = run(&env, &["doctor"]);
    assert!(out.status.success(), "doctor failed:\n{}", stdout_of(&out));
    let stdout = stdout_of(&out);
    assert!(
        stdout.contains(&format!("installed ({})", plist_path(&env).display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("binary: {}", daemon.display())),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "installed ({})",
            menubar_plist_path(&env).display()
        )),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("binary: {}", menubar.display())),
        "{stdout}"
    );
    assert!(!stdout.contains("WARN"), "{stdout}");
    assert!(stdout.contains("all checks passed"), "{stdout}");
}

#[test]
fn doctor_warns_when_the_menubar_registration_is_stale_and_still_passes() {
    let env = setup("doctor-menubar-stale");
    let (daemon, menubar) = fake_pair(&env, "bin");
    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--include-menubar",
            "--binary-path",
            daemon.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));

    // The registered menubar binary disappears (moved checkout, rebuild).
    std::fs::remove_file(&menubar).unwrap();

    let out = run(&env, &["doctor"]);
    assert!(
        out.status.success(),
        "stale menubar autostart must not fail doctor"
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[WARN] menubar autostart"), "{stdout}");
    assert!(stdout.contains("no longer exists"), "{stdout}");
    assert!(stdout.contains("all checks passed"), "{stdout}");
}

#[test]
fn doctor_warns_when_registrations_point_into_a_build_directory() {
    let env = setup("doctor-target");
    // Both binaries inside a sandbox target/-like path: the override is
    // registered as-is and the menubar resolves as its sibling.
    let bin_dir = env.home.path().join("checkout/target/debug");
    let daemon = bin_dir.join("prx");
    let menubar = bin_dir.join("prx-menubar");
    write_file(&daemon, "");
    write_file(&menubar, "");
    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--include-menubar",
            "--binary-path",
            daemon.to_str().unwrap(),
        ],
    );
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));

    // Doctor flags both registrations with the stable-path remedy and still
    // exits zero.
    let out = run(&env, &["doctor"]);
    assert!(
        out.status.success(),
        "build-directory registrations must not fail doctor:\n{}",
        stdout_of(&out)
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[WARN] autostart"), "{stdout}");
    assert!(stdout.contains("[WARN] menubar autostart"), "{stdout}");
    assert_eq!(stdout.matches("build directory").count(), 2, "{stdout}");
    assert!(stdout.contains("cargo install --path ."), "{stdout}");
    assert!(stdout.contains("all checks passed"), "{stdout}");
}

// ------------------------------------------------------- PRX.app (A4)

const APP_PLIST: &str = "Library/LaunchAgents/com.prx.app.plist";

/// A fake PRX.app under `<home>/<dir>` (existence is all discovery checks).
fn fake_bundle(env: &Env, dir: &str) -> PathBuf {
    let bundle = env.home.path().join(dir).join("PRX.app");
    write_file(&bundle.join("Contents/MacOS/PRX"), "");
    write_file(&bundle.join("Contents/Helpers/prx"), "");
    bundle
}

#[test]
fn install_with_an_app_bundle_migrates_the_legacy_agents() {
    let env = setup("app-migrate");
    let (daemon, _) = fake_pair(&env, "bin");
    let out = run(
        &env,
        &[
            "daemon",
            "install",
            "--include-menubar",
            "--binary-path",
            daemon.to_str().unwrap(),
        ],
    );
    assert!(
        out.status.success(),
        "legacy install failed:\n{}",
        stdout_of(&out)
    );
    assert!(plist_path(&env).exists() && menubar_plist_path(&env).exists());

    let bundle = fake_bundle(&env, "Applications");
    let out = run(
        &env,
        &["daemon", "install", "--app", bundle.to_str().unwrap()],
    );
    assert!(
        out.status.success(),
        "app install failed:\n{}",
        stdout_of(&out)
    );
    let app_plist = env.home.path().join(APP_PLIST);
    let contents = std::fs::read_to_string(&app_plist).unwrap();
    assert!(contents.contains("<string>com.prx.app</string>"));
    assert!(contents.contains(&format!(
        "<string>{}</string>",
        bundle.join("Contents/MacOS/PRX").display()
    )));
    assert!(contents.contains("<string>--background</string>"));
    assert!(!plist_path(&env).exists(), "legacy daemon agent removed");
    assert!(
        !menubar_plist_path(&env).exists(),
        "legacy menubar agent removed"
    );

    let out = run(&env, &["doctor"]);
    assert!(out.status.success(), "doctor failed:\n{}", stdout_of(&out));
    let stdout = stdout_of(&out);
    assert!(stdout.contains("PRX.app login item"), "{stdout}");
    assert!(
        stdout.contains(&format!("app: {}", bundle.display())),
        "{stdout}"
    );
    assert!(!stdout.contains("leftover"), "{stdout}");
}

#[test]
fn install_discovers_an_app_from_the_search_dirs() {
    let env = setup("app-search");
    let bundle = fake_bundle(&env, "Applications");
    let home = env.home.path().to_str().unwrap();
    let search = env.home.path().join("Applications");
    let out = run_cli(
        &env.config,
        env.data.path(),
        &["daemon", "install"],
        Some(env.gh.path()),
        &[
            ("HOME", home),
            ("PRX_APP_SEARCH_DIRS", search.to_str().unwrap()),
        ],
    );
    assert!(out.status.success(), "install failed:\n{}", stdout_of(&out));
    assert!(env.home.path().join(APP_PLIST).exists());
    assert!(
        !plist_path(&env).exists(),
        "no headless agent beside the app"
    );
    assert!(stdout_of(&out).contains(&bundle.display().to_string()));
}

#[test]
fn doctor_warns_when_the_registered_app_is_gone() {
    let env = setup("app-gone");
    let bundle = fake_bundle(&env, "Applications");
    prx::autostart::install_app_with(&bundle, env.home.path()).unwrap();
    std::fs::remove_dir_all(&bundle).unwrap();
    let out = run(&env, &["doctor"]);
    assert!(out.status.success(), "a missing app must not fail doctor");
    let stdout = stdout_of(&out);
    assert!(stdout.contains("[WARN] autostart"), "{stdout}");
    assert!(stdout.contains("no longer exists"), "{stdout}");
}
