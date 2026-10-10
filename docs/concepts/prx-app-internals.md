# PRX.app internals

How the macOS app is built and how it relates to the daemon. For using the app, see the
[PRX.app guide](../guides/prx-app.md).

## A thin client

PRX.app is the macOS GUI: a status-bar icon plus a menu that mirrors the daemon's view of your
pull requests, and an [app window](../guides/prx-app.md#the-prx-window). The UI lives in its own workspace crate
(`menubar/`, binary `prx-menubar`) and is a **thin client by construction** — every mutation
(review, approve, snooze, ignore) is a bearer-token call to the daemon's localhost [IPC API](../reference/ipc-api.md), so
the client itself never invokes `gh` and never talks to GitHub. Its only subprocesses are the
daemon it supervises, the configured [`browser`](../guides/browser-identity-block.md) (macOS `open` by
default), and the notification binary.

## Bundle layout

The bundle is `PRX.app/Contents/MacOS/PRX` (the UI; what Activity Monitor, the Dock, and the app
menu show) plus `PRX.app/Contents/Helpers/prx` (the full CLI and daemon — not `MacOS/prx`, which
on the default case-insensitive volume would be the same file as `MacOS/PRX`). `Info.plist` sets
`CFBundleName` *PRX*, `com.prx.app`, `LSUIElement` (no Dock icon unless the window is open), and
macOS 12 as the minimum; the version comes from `cargo metadata`. The icon is rendered from
`assets/app-icon.svg` with `rsvg-convert` (`brew install librsvg`), else a committed
`assets/AppIcon.iconset`, else a placeholder. The app is ad-hoc signed (`codesign -s -`), which is
fine for the machine that built it; a copy downloaded to another Mac is quarantined and trips
Gatekeeper (no notarization) until the flag is
[cleared](../getting-started/install.md#clear-the-quarantine-flag).
`cargo install --path .` keeps working for headless, CLI-only use, and `cargo run -p prx-menubar`
still runs the UI from a checkout (it supervises the `prx` next to it in `target/`); see
[Building from source](../development.md#building-from-source).

## One app, two processes

The daemon stays a separate process (crash isolation: a WebView or
AppKit fault never kills an in-flight review). On launch the app first looks for a running daemon
(the CLI's `prx daemon start`, a launchd agent, another session) and **attaches** to it; only when
none answers does it start `Contents/Helpers/prx daemon start --foreground` itself, with your
login shell's `PATH` (so `gh` and agent CLIs resolve like in Terminal), logging to the usual daemon
logs plus `logs/daemon-stderr.log` for start-up errors. A daemon the app started is supervised:
if it exits non-zero (a crash, `kill -9`) it is restarted after 1 s, 2 s, 4 s, … (capped at 60 s)
while the menu reads *Daemon restarting · …*; after 5 failures within 10 minutes the app stops retrying and
the menu shows *Daemon stopped* with *Start daemon* and *Show daemon log* (the newest daemon log in
Console). A clean exit — `prx daemon stop` — is respected: no restart. The app never starts a
second daemon while one runs (the daemon's pid-file guard settles any race).

## Single instance

PRX is single-instance: a bare second launch posts `com.prx.app.open-window` to the running app
and exits, and the guard file is `app.pid` in the [data directory](../reference/data-files.md).

## Main thread and run loop

The app runs `NSApplication` directly, with background threads (the poller,
actions, the supervisor) feeding a main-thread queue drained through a `CFRunLoopSource`
registered in `kCFRunLoopDefaultMode` only — so a poll cycle can never wake the run loop while a
menu is being tracked, and menus stay open through polls. (This replaced an earlier winit event
loop, whose wake-ups dismissed the menu mid-tracking on every poll.)

## Settings UI

The window's pages are a local web UI hosted in an AppKit window (`menubar/settings-ui`, plain
JavaScript with no build step). It talks only to its host process, which calls the loopback IPC
API; it never touches `config.yml` itself. See [Development](../development.md) for running it in a
browser against a fake daemon.
