# Development

How to build, test, and release prx. Contribution rules are in
[CONTRIBUTING.md](../CONTRIBUTING.md); conventions and invariants for people and coding agents are
in [AGENTS.md](../AGENTS.md).

## Layout

A Cargo workspace with two crates:

- `prx` (the repository root, `src/`): the CLI, daemon, lifecycle state machine, review pipeline,
  and localhost IPC API. It has no GUI code and builds on macOS and Linux.
- `prx-menubar` (`menubar/`): PRX.app, the macOS menu bar item and window. Its settings pages are
  plain JavaScript under `menubar/settings-ui/`, with no npm dependencies and no build step.

## Building from source

Users install prx from a release download; see [Install](getting-started/install.md). Build from
source to work on prx, to run an unreleased version, or on a platform the releases do not cover:
PRX.app on an Intel Mac, or the CLI on a Linux with a glibc older than the release binaries need.

You need a stable Rust toolchain (on macOS, with the Xcode command line tools) and a checkout:

```bash
git clone https://github.com/vs4vijay/prx
cd prx
```

To run prx itself you also need what every user needs: the GitHub CLI (`gh`), signed in, and a
coding-agent CLI or an LLM API key.

### The CLI and daemon

```bash
cargo build                       # a debug build: target/debug/prx
cargo run -- status               # run the CLI from the checkout
cargo install --path .            # a release build, installed as ~/.cargo/bin/prx
```

`cargo install --path .` gives you a `prx` on your `PATH` (Cargo's bin directory,
`~/.cargo/bin`). It is all a headless, CLI-only machine needs, Mac or Linux. If you register
autostart from a build, [Autostart](guides/autostart.md#which-binary-gets-registered) explains
which binary path is recorded.

To try prx without touching your real setup, point it at a scratch home:

```bash
PRX_CONFIG=/tmp/prx/config.yml PRX_DATA_DIR=/tmp/prx/data cargo run -- init
```

### PRX.app (macOS)

```bash
cargo run -p prx-menubar          # run the app UI from the checkout
```

This runs the UI without a bundle; it supervises the `prx` binary next to it in `target/`.

`scripts/bundle-macos.sh` builds the real app bundle. It needs no Apple developer account: it
makes a release build of the workspace, assembles `PRX.app`, and signs it ad hoc.

```bash
scripts/bundle-macos.sh --no-install         # build only: <target>/bundle.noindex/PRX.app
scripts/bundle-macos.sh                      # build, then install to ~/Applications
scripts/bundle-macos.sh --install DIR        # build, then install to DIR (for example /Applications)
open ~/Applications/PRX.app                  # or Spotlight / Finder: "PRX"
```

> **The default form replaces `~/Applications/PRX.app`.** Without `--no-install`, the script
> deletes the `PRX.app` already in the install directory and copies the new build in its place.
> If that is the PRX you use every day, you have just swapped it for your work in progress. Use
> `--no-install` while developing, and quit a running PRX before you install over it.

- `<target>` is Cargo's target directory (`target/`, or `CARGO_TARGET_DIR` when set). The
  `.noindex` suffix keeps Spotlight from listing the build copy next to the installed one.
- An app built on your own machine is not quarantined, so the
  [quarantine step](getting-started/install.md#step-2-allow-it-to-open) that release downloads
  need does not apply. Copied to another Mac, it does.
- The app icon is rendered from `assets/app-icon.svg` with `rsvg-convert` when that is installed
  (`brew install librsvg`); otherwise it comes from the icon set committed in `assets/`.
- The script builds for the machine it runs on. The release zip is Apple silicon only, so this is
  how to get PRX.app on an Intel Mac.
- After installing, Settings → General in the app offers *Install command-line tool…* and *Open
  at login*, as in a release install.

## The gate

CI (`.github/workflows/ci.yml`) runs on every push to `main` and every pull request. Run the same
commands before you push.

On Linux or macOS, the core crate and the settings UI:

```bash
cargo build -p prx --locked
cargo test -p prx --locked
cargo clippy -p prx --all-targets --locked -- -D warnings
cargo fmt --check
node --test menubar/settings-ui-tests/*.test.js
```

On macOS, the whole workspace:

```bash
cargo build --workspace --all-targets --locked
cargo test -p prx-menubar --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Linux uses `-p prx` because the menu bar crate does not build there.

## Tests

- **Pure unit tests** for the heart of the system: the state-machine transition table (every edge in
  the design contract), config merge/override semantics, `config set` key paths, insights math,
  history append/query/compaction.
- **Integration tests** (`tests/`) run the real `prx` binary against a **fake `gh`** shell fixture
  (`tests/fixtures/gh`: canned `pr list`/`pr view`/`pr diff` output, captured review bodies,
  simulated auth expiry) and a **fake agent** (`tests/fixtures/fake-agent`: configurable manifest,
  exit codes, sleeps for timeout testing) — no network, no real GitHub, no real agent. The fakes
  are driven by `FAKE_GH_*` and `FAKE_AGENT_*` environment variables. `PRX_CONFIG` /
  `PRX_DATA_DIR` redirect all state into per-test temp dirs, and `HOME` is redirected too. These
  tests are Unix-only.
- **Daemon tests** spawn the actual detached daemon and exercise IPC round-trips, the
  single-instance guard, graceful stop, notification tiers, and log rotation.
- **Hardening (chaos) tests**: agent crash mid-review (retry once, state stays reviewable), daemon
  `SIGKILL` during a running review (state never stuck in `in_review`, no ghost history), `gh` auth
  expiry followed by recovery on a later poll (daemon keeps running, logs the failure, no spurious
  `on_failure` spam), and corrupt `state.json` / `history.jsonl` (state fails loudly and is never
  overwritten; history degrades gracefully line by line).
- **Menu bar tests** (`cargo test -p prx-menubar`, macOS) cover the menu model, actions, clipboard,
  banners, and the settings bridge.
- **Settings UI tests** (`menubar/settings-ui-tests/*.test.js`) run under `node --test`.

## Working on the settings UI

To work on the UI in a browser against a fake daemon:

```bash
node menubar/settings-ui-tests/preview-server.js
```

## Demo data and media

Everything published under `docs/media/` shows one fictional team: a user `maya`, the
organizations `acme` and `globex`, and made-up pull requests. No screenshot or recording may come
from a real prx setup. The cast is defined once, in `menubar/settings-ui-tests/preview-mock.js`,
and every tool below reads it from there.

| Tool | What it does |
|------|--------------|
| `scripts/demo-sandbox.sh [DIR]` | Builds a throwaway prx setup in `DIR` (default `/tmp/prx-demo`): its own `HOME`, config and data directory, the fake `gh` from `tests/fixtures/`, and stub engines that answer with canned reviews. Nothing in it reads or writes your real prx. It leaves `DIR/prx` (one command inside the sandbox), `DIR/env.sh` (a whole shell inside it), and `DIR/run-app` (the native app inside it). |
| `scripts/demo-seed.js` | Run by `demo-sandbox.sh`: writes the sandbox's `config.yml`, `state.json`, `history.jsonl`, and the fake `gh` and agent fixtures from the cast. Timestamps are relative to the moment it runs. |
| `scripts/capture-screenshots.py` | Regenerates `docs/media/screenshots/*.png` from the settings UI preview in WebKit, with no daemon. `--list` names the shots; `--cli DIR` records the terminal shot inside a sandbox (needs `vhs`); `--import FILE NAME` re-encodes a manual capture without metadata. Needs `node` and the Python packages `playwright` (with WebKit) and `Pillow`. |
| `scripts/demo-video/build.sh` | Rebuilds `docs/media/demo.mp4`, `demo.gif`, and `demo-poster.png`. Needs `node`, `vhs`, `ffmpeg`, and Playwright with Chromium; see [its README](../scripts/demo-video/README.md). Captions and timing are in `scripts/demo-video/stage.js`. The menu bar section plays the recorded takes in `scripts/demo-video/menu-clips/` (see [The menu bar captures](#the-menu-bar-captures)). |
| `assets/render.sh logo` | Renders the README logo and the social preview into `docs/media/` from the SVGs in `assets/` (needs `rsvg-convert`). |

To regenerate everything:

```bash
python3 scripts/capture-screenshots.py                  # the window screenshots
scripts/demo-sandbox.sh /tmp/prx-demo                   # a sandbox for the terminal shot
python3 scripts/capture-screenshots.py --cli /tmp/prx-demo
scripts/demo-video/build.sh                             # video, GIF, poster (about four minutes)
assets/render.sh logo                                   # logo and social preview
```

Rules for the sandbox app:

- Start it only with `DIR/run-app`. A plain launch of the sandbox's app asks your login shell for
  its `PATH`, finds your real `gh` first, and lists your real repositories.
- Never start it twice, and never rebuild a sandbox while its app or daemon is running. Quit it
  from its own menu with *Quit PRX*.
- Look at every new image before you add it. A native capture is a real screen capture: check
  that it shows only the demo data and nothing else from your desktop.

### The menu bar captures

The menu bar menu is the one surface that cannot be scripted from a build: it only exists on a
logged-in desktop. Its media was captured once, from a sandbox app, and is checked in:

- `docs/media/screenshots/menu.png`, `menu-pr-actions.png`, `menu-approved.png`,
  `menu-before-review.png` and `menu-ungrouped.png`: the stills (2x, dark appearance).
- `scripts/demo-video/menu-clips/`: the four trimmed takes the demo video's menu bar section is
  built from, with their geometry in `clips.json`. `build.sh` reads them from there; without them
  (or with `PRX_DEMO_MENU=0`) it builds the video without that section.

How they were made, so they can be made again:

1. `scripts/demo-sandbox.sh DIR && DIR/run-app --background` starts the sandbox app (here a
   debug build, `target/debug/prx-menubar`). Wait for the first poll. The stub engine's review of
   `acme/api#412` lands a little later: `menu-before-review.png` was taken 40 seconds after the
   start, `menu.png` once that review was in. The flat list is the same sandbox with
   `menu.group_by: none`.
2. `scripts/demo-video/menu-capture.swift` does the capturing (its header lists the commands). It
   puts a flat `#1E1E1E` window behind the menu, opens the sandbox app's status item with a
   click that it refuses unless the element under the pointer belongs to the sandbox process,
   reads the open menu through Accessibility to confirm that every row is `acme/…` or
   `globex/…`, and records with ScreenCaptureKit at 30 fps, filtered to two processes: the
   sandbox app and the backdrop. No other window, menu bar item, wallpaper or notification can
   be in a frame. It needs Accessibility and Screen Recording permission.
3. The stills are single frames taken the same way, with and without the backdrop, cut out along
   the menu panel's own alpha, with a soft shadow added on a transparent margin. The takes were
   cropped and trimmed with `ffmpeg` (H.264, 30 fps, 2 pixels per point, metadata stripped).
4. Quit the sandbox app with *Quit PRX* in its menu.

What the captures do not show, and what must not be drawn in to make up for it:

- The status item (the icon in the menu bar strip). macOS draws it in a window the app does not
  own, so a capture filtered to the app cannot see it. In the takes the pointer clicks an empty
  strip and the menu drops from there; the video frames the menu at the top edge of a plain
  panel and says "menu bar" in the caption.
- Light appearance. Everything was captured in dark appearance.
- The app icon in the approve alert. A debug binary is not an app bundle, so the alert shows a
  generic folder icon.

A capture of the status item on its own exists outside the repository; its background is the
system menu bar material, tinted by the desktop behind it, so it is not published.

`python3 scripts/capture-screenshots.py --import FILE NAME` still re-encodes a manual capture
(Cmd-Shift-4, Space, click the menu) without metadata, if a single still ever needs replacing by
hand. Whichever way a capture is made: look at every frame before adding it, and do not add it if
it shows anything but the demo data.

## Releasing

`.github/workflows/release.yml` runs when a tag `vX.Y.Z` is pushed.

1. Set the same version in `Cargo.toml` and `menubar/Cargo.toml`. The release fails when the tag
   does not equal both.
2. `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. The workflow runs the full CI gate on the tagged commit, then builds and publishes a GitHub
   release with generated notes: CLI tarballs for macOS (arm64, x86_64) and Linux (x86_64, arm64),
   `PRX-<tag>-macos-arm64.zip`, and `SHA256SUMS`.

A tag with a pre-release suffix (`v0.2.0-rc.1`) is published as a pre-release. PRX.app in the
release is ad-hoc signed and not notarized. What users do with the assets is described in
[Install](getting-started/install.md).

## Design history

The project is developed through OpenSpec change proposals under `openspec/changes/`. Older design
notes are kept in [docs/internal/](internal/README.md).
