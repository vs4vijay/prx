# AGENTS.md

Guidance for coding agents and new contributors working in this repository. Keep it current: when
a decision changes how work is done here, add an entry under
[Announcements / recent decisions](#announcements--recent-decisions) and update the section it
affects.

## Announcements / recent decisions

Newest first. One entry per decision, in this format:

```
### YYYY-MM-DD: short title
What changed, why, and what to do differently from now on. Link the files or docs involved.
```

### 2026-10-29: HistoryEntry tolerates unknown variants; new variants are fine

`HistoryEntry` (`src/history.rs`) has a catch-all `#[serde(other)] Unknown` variant. A history
line or a `GET /history` entry whose `event` this build does not know decodes as `Unknown`
instead of failing the whole payload: `history::query` skips it, compaction keeps its line as it
is, and PRX.app's poller drops it. The first new variant that relies on this is `new_pr`, which
the machine writes wherever it raises `on_new_pr`, so PRX.app can banner new PRs while the daemon
defers to it. From now on, adding a history variant is a compatible change: handle it in the
`match`es on `HistoryEntry` (`src/main.rs`, `src/insights.rs`, `menubar/src/banners.rs`,
`menubar/src/model.rs`) and in `overview.js` `EVENTS`, and never match `Unknown` as if it had
data. See `openspec/changes/ready-blocked-alerts/`.

### 2026-10-14: Tests must not raise real desktop banners

The daemon's macOS banners now really fire: `src/notify.rs` runs `osascript` with a fixed
`on run argv` script and passes title, body, and sound as arguments, never spliced into
AppleScript source. So `install_fake_gh` in `tests/common/mod.rs` also puts do-nothing `osascript`
and `notify-send` stand-ins (`install_quiet_notifiers`) into the fake `gh` directory. From now on,
any test that starts a daemon or raises a notification goes through that harness; a test that
wants to observe banners installs its own recording fakes over the stand-ins. Keep notification
text out of the script source.

### 2026-10-14: Only PRX.app identifies itself as the menubar client

`DaemonClient` (`src/ipc.rs`) no longer announces itself by default. PRX.app opts in with
`DaemonClient::discover_as_menubar()` / `.as_menubar()`, and the daemon counts the `X-Prx-Client`
header only when its value is `menubar` or `menubar/<version>`. From now on, CLI code uses the
plain client (`discover()`), or a terminal command would mute the daemon's own banners, and
menubar code that creates a client uses the opt-in constructor.

### 2026-10-14: Settings UI pages are tested by booting the real scripts in a fake DOM

Page suites under `menubar/settings-ui-tests/` load the scripts `index.html` lists into
`fake-dom.js` with a fake daemon behind the bridge; `page-fixtures.js` holds the payloads and the
helpers that drive a page. The fake DOM's `innerHTML`, `outerHTML`, and `insertAdjacentHTML`
throw, so a page that injects markup fails its test. From now on, test a page change this way,
and put new test payloads in `page-fixtures.js` or the test file, not in `preview-mock.js`, which
is the demo dataset. See [Tests](docs/development.md#tests).

### 2026-10-11: PRX.app creates the default config on first launch (v0.1.1); install is three steps

A fresh PRX.app v0.1.0 was dead on arrival: with no config file the daemon refused to start and
the supervisor gave up with *Daemon stopped*, unless the user had run `prx init`. From v0.1.1 the
app's supervisor calls `prx::config::ensure_starter_config` before it spawns the daemon
(`menubar/src/supervisor.rs`, `ensure_first_run_config`). That function and `STARTER_CONFIG` live
in `src/config.rs` and are also what `prx init` uses, so there is one definition of a default
config. From now on:

- Never tell an app user to run `prx init`. `prx init` remains the explicit setup step for
  CLI-only use, and `prx daemon start` without a config still fails with ``run `prx init` first``.
- `ensure_starter_config` only ever creates a missing file. It must not overwrite, repair, or
  rewrite an existing config; an invalid one has to keep surfacing its own error.
- The starter config has to load and validate with no agent CLI and no `gh` installed
  (`tests/daemon_lifecycle.rs` checks this with an empty `PATH`).
- Install docs for the app are three steps: get PRX.app (option 1 the Releases page, option 2 the
  command line), one shared quarantine step in a `> [!IMPORTANT]` block, open it. Release asset
  names carry the version, so there is no version-free `releases/latest/download/…` URL; the
  command-line option uses `gh release download --pattern`.

### 2026-10-10: Install docs are download-first; building from source is a development topic

"Install" in `README.md` and `docs/getting-started/install.md` now means what a user does:
download a release asset, on macOS clear the quarantine flag, run it. Everything about cloning,
`cargo install --path .`, and `scripts/bundle-macos.sh` lives in
[Building from source](docs/development.md#building-from-source). From now on, keep build steps
out of the install pages, and keep the quarantine step prominent wherever a macOS download is
described, for as long as releases are not notarized. If the release assets or their names change
in `.github/workflows/release.yml`, update the install page's asset table and commands with them.

### 2026-10-10: Demo dataset and media tooling

All published screenshots, the demo video, and the GIF under `docs/media/` are generated from one
fictional dataset (user `maya`, orgs `acme` and `globex`), defined in
`menubar/settings-ui-tests/preview-mock.js`. `scripts/demo-sandbox.sh` and `scripts/demo-seed.js`
build a throwaway prx setup from it, `scripts/capture-screenshots.py` takes the screenshots,
`scripts/demo-video/build.sh` builds the video, and `assets/render.sh logo` renders the logo. From
now on, never add media captured from a real prx setup; regenerate it with these tools and look
at the result before adding it. The menu bar menu is the exception: it cannot be scripted, so its
stills (`docs/media/screenshots/menu*.png`) and the takes the video plays
(`scripts/demo-video/menu-clips/`) are checked-in captures of the sandbox app; do not redraw or
edit them. See [Demo data and media](docs/development.md#demo-data-and-media).

### 2026-10-10: Documentation restructured

`README.md` is now a landing page. All reference material lives under `docs/`, organised by what
a reader is trying to do (`getting-started/`, `guides/`, `reference/`, `concepts/`,
`troubleshooting.md`, `development.md`); `docs/README.md` is the index. When behaviour changes,
update the matching page under `docs/`, and touch the README only if the pitch, install steps, or
feature list change.

### 2026-10-10: Internal design notes moved to docs/internal/

`spec.md`, `design-audit.md`, `DEV-PLAN.md`, `implementation-plan.md`, and `next-steps.plan.md`
moved from the repository root to `docs/internal/`. They are historical and partly stale. Code
comments still cite their identifiers (`A<n>`, `DEV-PLAN §n`, `spec §n`); see
`docs/internal/README.md` for the key. Do not put those identifiers in user-facing docs.

## Project overview

prx ("Pull Request eXaminer", tagline "AI informs, human decides") is a local-first manager for
the pull request review lifecycle. It polls watched GitHub repositories through the `gh` CLI, has
a local coding agent or an LLM API review each open PR read-only, posts the result to GitHub
itself, sends desktop notifications, and records history.

- One Rust binary, `prx`: CLI, background daemon, lifecycle state machine, review pipeline, and a
  localhost IPC API.
- PRX.app: a macOS menu bar item and window that is a client of that API.
- User documentation: [docs/README.md](docs/README.md). Start with
  [docs/concepts/architecture.md](docs/concepts/architecture.md).

## Core invariants

Do not weaken these. A change that needs to break one needs an explicit decision from the
maintainer first.

1. **The engine never posts and never approves.** An engine (agent CLI or LLM API) analyses
   read-only and produces one JSON review manifest. prx alone writes to GitHub. The only approve
   path is the human's `prx approve` / `POST /approve`. No automated path may post an approval.
2. **The output contract is always appended last** to every review prompt and cannot be removed
   or edited through config or settings.
3. **PR content is untrusted data.** Titles, descriptions, comments, and diffs are never executed
   and never treated as instructions.
4. **`src/` stays GUI-free.** All AppKit / objc2 code lives in `menubar/`. The core crate must
   keep building on Linux.
5. **`menubar/` talks to the core only through the IPC API and the `prx::` library.** It never
   invokes `gh` and never talks to GitHub.
6. **All state changes and GitHub writes flow through the daemon** when it is running. CLI
   commands are thin clients with an in-process fallback when the daemon is down.
7. **No secrets in config, state, history, manifests, or logs.** Credentials are referenced by
   environment-variable name only. prx makes GitHub calls only through `gh`.
8. **The IPC API binds `127.0.0.1` only** and requires the per-start bearer token.

## Repo layout

```
Cargo.toml            workspace root and the `prx` crate (v0.1.1)
src/                  core crate: CLI, daemon, state machine, review pipeline, IPC API (no GUI)
  main.rs, cli.rs       entry point and clap command types
  daemon.rs, ipc.rs     background daemon; localhost API and the CLI's thin client
  machine.rs, state.rs  pure lifecycle transition table; persisted PR state
  review.rs, prompt.rs  review orchestration and briefs; prompt rendering and the output contract
  agent.rs, llm.rs      agent-CLI engines; LLM API engines
  github.rs             all GitHub I/O, through the `gh` CLI
  config.rs, config_edit.rs   YAML config load/validate/merge and the starter config; `prx config set|edit`
  history.rs, insights.rs, usage.rs   history log, analytics, token usage
  notify.rs, effects.rs, browser.rs, pause.rs, autostart.rs, discover.rs, status.rs,
  paths.rs, logging.rs  supporting modules
tests/                integration tests for the core crate (run the real binary)
  fixtures/gh, fixtures/fake-agent   shell fakes for `gh` and an agent
  common/               shared test harness (fake `gh`, fake agent, quiet notifier stand-ins)
menubar/              `prx-menubar` crate: PRX.app (macOS only)
  src/                  AppKit/objc2 shell: menu model, poller, supervisor, banners, settings window
  settings-ui/          the window's pages: dependency-free vanilla JS, no npm, no build step
  settings-ui-tests/    node:test suites, fake DOM and fixtures, preview server with a fake daemon
  tests/                menubar integration tests
scripts/              bundle-macos.sh (build and install PRX.app), verify-upgrade.sh
  demo-sandbox.sh, demo-seed.js   throwaway prx setup filled with the fictional demo dataset
  capture-screenshots.py          regenerates docs/media/screenshots/
  demo-video/                     build.sh and sources for docs/media/demo.mp4, demo.gif, poster
                                  (menu-clips/: recorded menu bar takes; menu-capture.swift: their capture tool)
assets/               brand assets and the app icon sources; render.sh renders icons and the logo
docs/                 user documentation; docs/internal/ holds historical design notes
  media/                logo, screenshots, demo video and GIF (generated; fictional data only)
openspec/             OpenSpec change proposals (changes/) and specs
.agents/              agent commands and skills; `.claude` is a symlink to it
.github/workflows/    ci.yml (push/PR gate, reusable), release.yml (tag → release assets)
```

## Build, test, lint

The CI gate is `.github/workflows/ci.yml`. Run the matching commands before proposing a change.

Core crate and settings UI (Linux or macOS; this is the Ubuntu CI job):

```bash
cargo build -p prx --locked
cargo test -p prx --locked
cargo clippy -p prx --all-targets --locked -- -D warnings
cargo fmt --check
node --test menubar/settings-ui-tests/*.test.js
```

Whole workspace (macOS only; this is the macOS CI job):

```bash
cargo build --workspace --all-targets --locked
cargo test -p prx-menubar --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

On Linux always pass `-p prx`: the menubar crate does not build there.

Other useful commands:

```bash
cargo run -- <command>                               # run the CLI from a checkout
cargo run -p prx-menubar                             # run the app UI (macOS)
scripts/bundle-macos.sh --no-install                 # build PRX.app, do not install it
node menubar/settings-ui-tests/preview-server.js     # settings UI in a browser, fake daemon
scripts/demo-sandbox.sh /tmp/prx-demo                # throwaway prx with fictional data
/tmp/prx-demo/prx status                             # one command inside that sandbox
```

Without `--no-install`, `scripts/bundle-macos.sh` replaces `~/Applications/PRX.app`, which may be
the copy the user is running. Build instructions live in
[Building from source](docs/development.md#building-from-source);
[Install](docs/getting-started/install.md) is the user page and covers release downloads only.

## Testing approach

- **Unit tests** sit next to the code: the state-machine transition table, config merge and
  override rules, `config set` key paths, insights math, history append / query / compaction.
- **Integration tests** in `tests/` run the real `prx` binary against the shell fakes
  `tests/fixtures/gh` and `tests/fixtures/fake-agent`, controlled with `FAKE_GH_DIR` and other
  `FAKE_GH_*` / `FAKE_AGENT_*` variables. Each test is isolated with `PRX_CONFIG` and
  `PRX_DATA_DIR` in a temp dir and a redirected `HOME`. No network. Unix-only. Install the fake
  `gh` with `install_fake_gh` (`tests/common/mod.rs`): it also installs do-nothing `osascript` /
  `notify-send`, so no test raises a real desktop banner.
- **CLI paths** each have a suite: `tests/cli_thin_client.rs` (the CLI against a scripted stand-in
  daemon), `tests/cli_fallback.rs` (every command with no daemon), `tests/cli_pause_paths.rs`
  (file locations, `prx pause` / `prx resume` without a daemon, argument errors).
- **Invariant tests**: `tests/review_invariants.rs` runs the real pipeline in-process against
  recording `Gh` doubles and shows that no automated path approves, blocks, or dismisses.
- **Daemon and hardening tests** start a real detached daemon: IPC round-trips, single-instance
  guard, graceful stop, agent crash, `SIGKILL` mid-review, `gh` auth expiry, corrupt data files.
- **Menubar tests** (`cargo test -p prx-menubar`) run on macOS.
- **Settings UI tests** use `node --test`. Page suites boot the scripts `index.html` lists in
  `fake-dom.js` (no HTML parser: `innerHTML` and friends throw) with payloads from
  `page-fixtures.js`. Do not add npm dependencies.

Never write a test that calls real GitHub, a real agent, or a real LLM API. New behaviour needs a
test at the lowest tier that can show it.

## Commit style

Not Conventional Commits. One long, descriptive subject line:

- Prefix `prx:` for the core crate and repo-wide changes, `prx-menubar:` for the app.
- Describe the whole change in the subject, joining clauses with dashes (` — `) or commas.
- Optional body of `-` bullets for detail.

Examples from the log:

```
prx: daemon autostart — launchd plist (macOS) + systemd user unit (Linux) install/uninstall, doctor autostart check with stale-path warning, HOME-redirected integration tests
prx-menubar: daemon discovery + status polling, offline state, attention-count icon, grouped PR menu model with collapse (unit-tested)
```

Do not commit or push unless asked.

## Gotchas

- **Helper path.** Inside PRX.app the CLI is `Contents/Helpers/prx`, not `Contents/MacOS/prx`:
  on a case-insensitive APFS volume that would be the same file as `Contents/MacOS/PRX`.
- **Signing.** PRX.app is ad-hoc signed and not notarized. A copy downloaded with a browser is
  quarantined and macOS refuses to open it until `xattr -dr com.apple.quarantine PRX.app`; the
  same goes for a `prx` unpacked from a browser-downloaded tarball. The user-facing steps are in
  [Install](docs/getting-started/install.md#step-2-allow-it-to-open).
- **First launch.** PRX.app writes the default config itself when none exists
  (`ensure_first_run_config` in `menubar/src/supervisor.rs`), so no doc or message may ask an
  app user to run `prx init`. Tests that start a `Supervisor` must point `PRX_CONFIG` as well as
  `PRX_DATA_DIR` into a sandbox, or the spawn path would create a config in the real home.
- **Formatting one file.** `cargo fmt -- <files>` formats the whole workspace, not just those
  files. To format single files use `rustfmt --edition 2021 <file>`.
- **Menubar client header.** A `DaemonClient` is anonymous unless it opts in with
  `discover_as_menubar()` / `as_menubar()`. Only `menubar/` may opt in; a CLI command that did
  would mute the daemon's banners for as long as a menubar client counts as recently seen.
- **`prx daemon start` waits.** It returns only once the daemon has claimed the pid file and
  published its IPC token (10 s limit), and on failure prints the reason the daemon gave, so a
  command run right after it reaches the daemon.
- **launchd PATH.** launchd plists written by `prx daemon install` snapshot the installing
  shell's `PATH`. Engines should be configured with absolute paths.
- **Release tags.** A release tag `vX.Y.Z` must equal the version in both `Cargo.toml` and
  `menubar/Cargo.toml`, or `release.yml` fails.
- **Wire compatibility.** New fields in config, state, history, and IPC payloads are additive and
  carry `#[serde(default)]`, so older files and older peers keep working. Do not rename or remove
  a field without an alias.
- **`.claude` is a symlink to `.agents`.** Edit files under `.agents/`.
- **Workflow.** Changes are planned as OpenSpec proposals under `openspec/changes/` before
  implementation.
- **Hot reload exception.** Config reloads live, except `max_concurrent_reviews`, which is sized
  at daemon start.
- **Legacy names still accepted.** `agents:` / `agent` (now `engines:` / `engine`) and
  `executive_summary` (now `brief`) are read as aliases; rewrites save the new names.
- **Shared files.** `assets/` is brand material with its own README; do not restyle it in passing.
- **Demo sandbox app.** Start the sandbox's app only with `<sandbox>/run-app`, never twice, and
  never with a plain launch: a plain launch takes `PATH` from the login shell, finds the real
  `gh`, and shows real repositories. Quit it from its menu (*Quit PRX*). Do not rebuild a sandbox
  while its app or daemon is running.
- **Published media.** Everything under `docs/media/` must come from the demo dataset. Do not
  commit a capture of a real setup, and do not hand-edit generated images. See
  [Demo data and media](docs/development.md#demo-data-and-media).

## Domain knowledge

Terms and rules that the code assumes. Extend this section as you learn things that are not
obvious from one file.

- **Lifecycle states** (`src/state.rs`): `pending`, `in_review`, `reviewed_approved`,
  `reviewed_changes_requested`, `skipped` (reasons `draft`, `config`, `snoozed`, `ignored`),
  `merged`, `closed`. `in_review` is held in memory only until the run ends, so a crash never
  strands a PR there.
- **Review manifest**: the engine's only output. `manifest_version` 1; `verdict` is `approved`,
  `changes_requested`, or `comment_only`; findings have severity `blocking` or `nit`. Any
  `blocking` finding forces `changes_requested`. Invalid manifests, crashes, and timeouts are
  retried once. A finding with an empty body or `line: 0` makes the manifest invalid (lines are
  1-based; omit `line` instead). See
  [docs/reference/review-manifest.md](docs/reference/review-manifest.md).
- **A verdict of `approved` is not a GitHub approval.** prx posts it as a comment review; the PR
  then waits for the human's approval.
- **Engines**: `type: agent` (a local CLI; built-ins `pi`, `claude`, `codex`) or `type: llm_api`
  (`anthropic`, `openai`, `openai_compatible`). See [docs/guides/engines.md](docs/guides/engines.md).
- **Review modes**: `review comment` (default), `monitor` (review, never post), `off` (no polling,
  no reviews). A **pause** is different: daemon-wide, timed, and polling continues. `off` is a
  stop switch for explicit reviews too: `prx review` declines with or without a daemon.
- **Cooldowns** are anchored to the PR's GitHub creation time (first review) and to the detection
  of new commits (re-review). An explicit single-PR review bypasses them.
- **Config resolution**: defaults ← org ← repo, most specific wins; partial structures merge field
  by field. A broken config file never stops the daemon: it keeps the last good config and
  reports `config_error`.
- **Config validation**: a repo name is `org/repo` with one slash and only `[A-Za-z0-9._-]` on
  each side (`validate_repo_name` in `src/config.rs`; `.` and `..` halves and an org starting
  with `-` are refused too). `poll_interval_minutes`, `max_concurrent_reviews`, and
  `agent_timeout_seconds` must be above zero in org and repo overrides as well as in `defaults`.
- **`prx config set <key> null`** (or `~`) clears an optional key or an override.
- **Brief**: a short plain-language description of a PR, taken from the manifest or generated on
  demand; best-effort and never part of the verdict.
- **Notification tiers**: events map to `urgent`, `informational`, or `silent`. The daemon
  suppresses its own banners while a menubar client has been seen recently. The tier decides the
  banner; the sound is `notifications.sounds.<event>`, else the event's built-in sound (only
  `on_review_complete_approved`: `Glass`), else the tier's (`Notifications::behavior_for`).
- **Token usage** semantics are uniform: `input_tokens` is uncached input; cache reads and writes
  are separate; cost is recorded only when the engine reports it.
- **Data locations**: config at `~/.config/prx/config.yml` (`PRX_CONFIG`), data at
  `~/.local/share/prx` (`PRX_DATA_DIR`). See [docs/reference/data-files.md](docs/reference/data-files.md).
