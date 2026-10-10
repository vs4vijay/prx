# Autostart (start at login)

`prx daemon install` registers prx to start at login; `prx daemon uninstall` removes the
registration. Neither starts or stops anything right now. If the daemon cannot find `gh` or your
agent after a login start, see [Troubleshooting](../troubleshooting.md).

## With PRX.app (macOS)

When a PRX.app bundle is found, `prx daemon install` registers **one**
login item, `~/Library/LaunchAgents/com.prx.app.plist` (`Label com.prx.app`), which launches
`PRX.app/Contents/MacOS/PRX --background` at login — the menu bar item plus the daemon the app
starts and supervises, no window — with `RunAtLoad` and `KeepAlive { SuccessfulExit = false }`
(a crashed app relaunches; *Quit PRX* stays quit until the next login). It replaces the separate
`com.prx.daemon` and `com.prx.menubar` agents and **removes** them, so re-running
`prx daemon install` after installing the app migrates an older setup. The app's *Open at login*
switch (Settings → General) writes and removes the same plist. The bundle is looked up in this
order: `--app <PATH>` (an absolute path to a PRX.app); the bundle containing the running `prx`
(e.g. `PRX.app/Contents/Helpers/prx`, or the `~/.local/bin/prx` link to it); `/Applications/PRX.app`;
`~/Applications/PRX.app`. An explicit `--binary-path` asks for the headless daemon agent instead
(and drops a `com.prx.app` item, so there is only ever one login registration).

## Headless (no app)

Without a bundle — Linux, or a `cargo install`-only Mac — `prx daemon
install` registers the daemon to start at login exactly as before. On macOS it writes a launchd plist to
`~/Library/LaunchAgents/com.prx.daemon.plist` (`Label com.prx.daemon`, `RunAtLoad`); on Linux it
writes a systemd
user unit to `~/.config/systemd/user/prx-daemon.service` (the foreground daemon under systemd
supervision, `Restart=on-failure`) and enables it with `systemctl --user enable`. Neither command
starts or stops a daemon. The macOS plists are written but **not loaded**, so they take effect at
your next login — to load the daemon's now, run
`launchctl load ~/Library/LaunchAgents/com.prx.daemon.plist` yourself (the menubar's likewise). The
Linux unit is enabled immediately but also only launches at login. The daemon's pid-file
single-instance guard makes a login autostart followed by a manual `prx daemon start` a
harmless no-op.

## Supervision

The daemon plist runs `prx daemon start --foreground` with
`KeepAlive { SuccessfulExit = false }`, so launchd — like systemd already does — relaunches a
crashed (non-zero-exit) daemon; a clean exit is never relaunched, which is why `prx daemon stop`
(whose graceful shutdown exits 0) stays stopped. The opt-in `--include-menubar` flag (headless
installs without PRX.app; with the app it is implied) also writes
`~/Library/LaunchAgents/com.prx.menubar.plist` with `RunAtLoad` and the same `KeepAlive`: a crashed
menubar relaunches, while *Quit PRX* (exit 0) stays quit until your next login. The menubar is
macOS-only — on Linux the flag warns and is skipped.

## Which binary gets registered

Install resolves a stable path rather than blindly registering
what is running, in this order: an explicit `--binary-path <PATH>` (must be absolute); the
cargo-installed `~/.cargo/bin/prx`, when the running binary sits in a Cargo build directory
(any `target/` path component) and that stable binary exists; otherwise the running binary itself,
with a warning when it lives under `target/` (the remedy is printed: `cargo install --path .`, then
re-run `prx daemon install` — or pass `--binary-path`). With `--include-menubar`, the menubar binary
resolves as `prx-menubar` next to the registered daemon binary, falling back to
`~/.cargo/bin/prx-menubar`; the install fails naming the expected location when neither exists.

`prx daemon uninstall` removes every registration (the app login item, daemon, and menubar), each idempotently — it
succeeds with a "not installed" note when there is nothing to remove — and never signals a running
daemon; stop the daemon separately with `prx daemon stop` if that is what you want.

`prx doctor` reports the active registration. With the app login item it prints one `autostart`
line naming the plist and the registered app — a warning when that app no longer exists — plus an
`autostart leftover` warning for each legacy `com.prx.daemon` / `com.prx.menubar` agent still beside
it (re-run `prx daemon install` to remove them). Otherwise it prints one informational line per
agent (`autostart` and `menubar autostart`): not
installed (a hint, with the install command), installed (with the registration path and the binary
it points at), or a warning when the registered binary no longer exists (stale after `cargo clean`
or a move) or points into a build directory — none of these ever fails the run. Re-run
`prx daemon install` after `cargo install --path .` to refresh the recorded path.

## Use absolute agent paths

launchd and systemd run your session with a minimal PATH, so an agent
configured as a bare name (`command: pi`) that works fine in your shell will not be found at login.
Configure `engines.<name>.command` as an **absolute path** — e.g. `/Users/you/.bun/bin/pi` — in
`~/.config/prx/config.yml` (or the daemon won't find your agent at login); `prx doctor` confirms
what each engine resolves to. The same minimal environment applies to `llm_api` key variables
(`api_key_env`) and `identity.credential`: a variable exported only in your shell profile is not
visible to an autostarted daemon.
