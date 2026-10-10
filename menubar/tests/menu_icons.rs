//! Regression test for the text-only menu: every model item with a symbol
//! must reach its NSMenuItem as an image marked visible (macOS 27 hides
//! menu item images unless `preferredImageVisibility` is `.visible`).
//! Menu creation needs the main thread, so this runs the binary's
//! `--icon-report` mode, which renders through the production path.
#![cfg(target_os = "macos")]

use std::process::Command;

#[test]
fn render_path_sets_visible_images_for_every_symbol() {
    let output = Command::new(env!("CARGO_BIN_EXE_prx-menubar"))
        .arg("--icon-report")
        .output()
        .expect("run prx-menubar --icon-report");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "icon report failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(!stdout.contains("FAIL"), "{stdout}");
    // The offline menu's own symbol rows plus the synthetic submenu's 30.
    let checked = stdout.lines().filter(|line| line.starts_with("ok")).count();
    assert!(checked > 30, "too few items checked:\n{stdout}");
}
