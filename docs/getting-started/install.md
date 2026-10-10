# Install

prx is one program, `prx`, that contains the command-line tool (CLI) and the background daemon.
On macOS there is also PRX.app, a menu bar app that carries its own copy of `prx`. Both are
downloads from the [releases page](https://github.com/vs4vijay/prx/releases); nothing has to be
compiled.

| You have | Install |
|----------|---------|
| A Mac with Apple silicon (M1 or later) | [PRX.app](#prxapp-macos). It includes the CLI and the daemon. |
| An Intel Mac | The [CLI](#cli-and-daemon-macos-and-linux). There is no prebuilt PRX.app for Intel; you can [build it from source](../development.md#building-from-source). |
| Linux | The [CLI](#cli-and-daemon-macos-and-linux). |

Before either, have the [GitHub CLI](https://cli.github.com/) signed in (`gh auth status`) and a
coding-agent CLI or an LLM API key at hand; [Requirements](#requirements) has the details. To
compile prx yourself instead, see [Building from source](../development.md#building-from-source).

## PRX.app (macOS)

Three steps: get the app, allow it to open, open it. This describes v0.1.1 and later.

### Step 1: get PRX.app

There are two ways. They end in the same place: `PRX.app` in your Applications folder.

**Option 1: download it from the Releases page.** No terminal needed.

1. Open the [latest release](https://github.com/vs4vijay/prx/releases/latest) in your browser.
2. Under *Assets*, click `PRX-<version>-macos-arm64.zip` to download it.
3. In Finder, open your Downloads folder and double-click the zip. Safari may already have
   unpacked it for you. You now have **PRX** (`PRX.app`) next to the zip.
4. Drag **PRX** into the Applications folder.

**Option 2: use the command line.**

```bash
cd ~/Downloads
gh release download --repo vs4vijay/prx --pattern 'PRX-*-macos-arm64.zip'
ditto -x -k PRX-*-macos-arm64.zip /Applications    # unpacks PRX.app into Applications
```

`gh release download` without a version fetches the newest release, so these lines stay the same
for later versions; for a particular one, put its tag after `download`
(`gh release download v0.1.1 --repo …`). The `PRX-*` pattern expects one PRX zip in the folder:
delete older ones first. When a `/Applications/PRX.app` is already there, follow
[Update](#update) instead, which removes the old copy first.

The commands use `gh` because the file names carry the version (`PRX-v0.1.1-macos-arm64.zip`),
so there is no fixed download address that always points at the newest file. `gh` finds the
newest release and matches the name by pattern, and prx needs `gh` anyway.

`~/Applications` works as well as `/Applications`. The rest of this page assumes `/Applications`.

### Step 2: allow it to open

This step is the same for both options. Do it once, before you open PRX for the first time.

> [!IMPORTANT]
> Run this in Terminal before the first open:
>
> ```bash
> xattr -dr com.apple.quarantine /Applications/PRX.app
> ```
>
> PRX.app is not yet notarized by Apple, so macOS blocks a downloaded copy. The app is not
> damaged; this command clears the download flag macOS objects to.

It prints nothing when it works, and does no harm if the app was not quarantined (a download made
with `gh` usually is not; one made with a browser always is).

Why it is needed: PRX.app is signed ad hoc and is not notarized by Apple. macOS puts a quarantine
flag on anything a browser downloads, and refuses to open a quarantined app that Apple has not
notarized. The command removes that flag from the app and from the `prx` inside it. A later
release may be signed and notarized; until then this step is needed after every browser download,
including updates.

**If you skip it**, macOS stops the app when you open it. The wording depends on the macOS
version; it is one of these, or close to it:

- *"PRX" Not Opened. Apple could not verify "PRX" is free of malware that may harm your Mac or
  compromise your privacy.*
- *"PRX" can't be opened because Apple cannot check it for malicious software.*
- *"PRX" cannot be opened because the developer cannot be verified.*
- *"PRX" is damaged and can't be opened. You should move it to the Trash.*

The app is not damaged. Choose *Done* or *Cancel* (not *Move to Trash*), run the `xattr` command
above, and open the app again.

**Without a terminal:** after macOS has refused the app once, open System Settings → Privacy &
Security, scroll down to the message about PRX, choose *Open Anyway*, and confirm. This only
allows the app itself. If macOS showed the "damaged" message, or offers no *Open Anyway* button,
or `prx` is later stopped with `killed` in a terminal, use the `xattr` command.

### Step 3: open it

Double-click PRX in the Applications folder, open it from Spotlight, or run:

```bash
open /Applications/PRX.app
```

There is nothing to set up first. What to expect:

- A PRX icon appears in the menu bar, and the PRX window opens on the Dashboard. PRX has a Dock
  icon only while its window is open; closing the window leaves the menu bar icon running.
- On the first launch PRX writes a default config file, `~/.config/prx/config.yml`, and starts
  the daemon (or attaches to one that is already running). You do not run `prx init` or
  `prx daemon start` yourself. A config file that is already there is used as it is and never
  overwritten.
- Nothing is watched yet, so the Dashboard offers *Add a repo*, and the menu says *No PRs tracked
  yet*. From there, continue with the [Quickstart](quickstart.md#with-prxapp-macos).

The default config reviews with the `pi` CLI. If you use `claude`, `codex`, or an LLM API
instead, choose it in Settings; see [Engines](../guides/engines.md).

> [!NOTE]
> **v0.1.0 only:** that version does not create the config file. Its menu shows *Daemon
> restarting* and then *Daemon stopped* until you either update to v0.1.1 or later, or run
> `/Applications/PRX.app/Contents/Helpers/prx init` once and choose *Start daemon* in the menu.

To have PRX start when you log in, switch on *Open at login* under Settings → General. See
[Autostart](../guides/autostart.md).

### The `prx` command

The app carries the CLI at `/Applications/PRX.app/Contents/Helpers/prx`. It does not put `prx` on
your `PATH` by itself, and it does not ask. To get a `prx` command in your terminal:

1. In PRX, open Settings → General.
2. Under *PRX app*, choose *Install command-line tool…*.

This creates the link `~/.local/bin/prx`, pointing at the copy inside the app, so the CLI and the
app are always the same version. If `~/.local/bin` is not on your shell's `PATH`, the page says so
and shows the line to add to `~/.zprofile`. A regular file already at `~/.local/bin/prx` is never
overwritten; move it away first. More in the [PRX.app guide](../guides/prx-app.md#pages).

With PRX.app installed you do not need the CLI download below.

## CLI and daemon (macOS and Linux)

Three steps here too: get the tarball, on macOS allow `prx` to run, put it on your `PATH`.

### Step 1: get the tarball and unpack it

Pick the file for your machine. `<version>` is the release, for example `v0.1.1`:

| Machine | File | `target` |
|---------|------|----------|
| macOS, Apple silicon | `prx-<version>-aarch64-apple-darwin.tar.gz` | `aarch64-apple-darwin` |
| macOS, Intel | `prx-<version>-x86_64-apple-darwin.tar.gz` | `x86_64-apple-darwin` |
| Linux, x86_64 | `prx-<version>-x86_64-unknown-linux-gnu.tar.gz` | `x86_64-unknown-linux-gnu` |
| Linux, arm64 | `prx-<version>-aarch64-unknown-linux-gnu.tar.gz` | `aarch64-unknown-linux-gnu` |

**Option 1: download it from the Releases page.** Open the
[latest release](https://github.com/vs4vijay/prx/releases/latest) in your browser, download the
file under *Assets*, and unpack it: double-click it in Finder on macOS, or run
`tar -xzf prx-*.tar.gz` in the folder it was saved to.

**Option 2: use the command line**, in the folder you want the download in:

```bash
target=aarch64-apple-darwin    # the `target` column for your machine
gh release download --repo vs4vijay/prx --pattern "prx-*-$target.tar.gz"
tar -xzf prx-*-"$target".tar.gz
```

Either way you now have a folder of the same name as the tarball, `prx-<version>-<target>/`,
holding `prx`, `README.md`, and `LICENSE`. Only `prx` is needed. Run the next two steps in the
folder that contains it (`~/Downloads` after a browser download). They expect one unpacked prx
folder there: delete older ones first.

### Step 2 (macOS only): allow it to run

The same for both options, before you run `prx` for the first time:

> [!IMPORTANT]
> ```bash
> xattr -dr com.apple.quarantine prx-v*/
> ```
>
> `prx` is not yet notarized by Apple, so macOS stops a downloaded copy the moment it starts: the
> shell prints `killed` and nothing else. Nothing is damaged; this clears the download flag.

The reason is the same as for [PRX.app](#step-2-allow-it-to-open): `tar` and Finder pass the
browser's quarantine flag on to `prx`. If you already copied `prx` somewhere, clear it there:
`xattr -d com.apple.quarantine ~/.local/bin/prx`. Linux has no such step.

### Step 3: put `prx` on your `PATH` and create the config

```bash
mkdir -p ~/.local/bin
install -m 755 prx-v*/prx ~/.local/bin/prx    # or any directory on your PATH
prx --version
prx init                                      # writes ~/.config/prx/config.yml
```

`prx init` creates the config file and the data directory, and leaves an existing config alone.
Without the app it is how the config gets created: `prx daemon start` does not start without one
and says ``run `prx init` first``.

Notes:

- **Linux:** `tar` prints *Ignoring unknown extended header keyword
  'LIBARCHIVE.xattr.com.apple.provenance'* a few times. The archives are made on a Mac; the
  warning is harmless.
- If `prx --version` answers `command not found`, `~/.local/bin` is not on your `PATH`. Add
  `export PATH="$HOME/.local/bin:$PATH"` to `~/.zprofile` (zsh, the macOS default) or
  `~/.profile` (bash on most Linux systems) and open a new terminal.

Continue with the [Quickstart](quickstart.md). To start the daemon at login, see
[Autostart](../guides/autostart.md).

## Requirements

prx needs two things on your machine to do anything useful:

- **The [GitHub CLI](https://cli.github.com/) (`gh`), signed in.** `gh auth status` must succeed.
  prx makes every GitHub call through `gh` and stores no tokens of its own.
- **Something to do the reviewing.** Either a coding-agent CLI (`pi`, `claude`, and `codex` work
  without configuration; others can be [configured](../guides/engines.md)), or an API key for an
  [LLM API engine](../guides/engines.md).

PRX.app opens and its daemon runs without either; they are needed once there is something to
review.

And the system has to be recent enough:

| | Requirement |
|---|-------------|
| PRX.app | macOS 12 or later, Apple silicon |
| `prx` on macOS | macOS 11 or later, Apple silicon or Intel |
| `prx` on Linux | x86_64 or arm64, glibc 2.39 or later (Ubuntu 24.04, Debian 13, or newer) |

On a Linux with an older glibc, `prx` reports ``version `GLIBC_2.39' not found`` and, on arm64,
does not start; [build from source](../development.md#building-from-source) there.

Desktop notifications use `osascript` on macOS and `notify-send` on Linux. They are optional.

## What a release contains

Every release carries the same six files. `<tag>` is the version, for example `v0.1.1`:

| File | Contents |
|------|----------|
| `PRX-<tag>-macos-arm64.zip` | PRX.app for Apple silicon: the menu bar app with `prx` inside |
| `prx-<tag>-aarch64-apple-darwin.tar.gz` | `prx` for macOS, Apple silicon |
| `prx-<tag>-x86_64-apple-darwin.tar.gz` | `prx` for macOS, Intel |
| `prx-<tag>-x86_64-unknown-linux-gnu.tar.gz` | `prx` for Linux, x86_64 |
| `prx-<tag>-aarch64-unknown-linux-gnu.tar.gz` | `prx` for Linux, arm64 |
| `SHA256SUMS` | Checksums of the five files above |

## Verify a download

`SHA256SUMS` lists the SHA-256 checksum of every other file in the release. Download it from the
release page next to the other files, or add `--pattern SHA256SUMS` to the `gh release download`
line. Then run the check in the folder that holds `SHA256SUMS` and whatever you downloaded:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing     # macOS
sha256sum -c SHA256SUMS --ignore-missing         # Linux
```

Each file you have is reported as `OK`. `--ignore-missing` skips the release files you did not
download; without it they are reported as `FAILED open or read`, which only means they are not
there. A line ending in `FAILED` alone means the file does not match: download it again.

## Update

A newer release replaces the old files. Your configuration (`~/.config/prx`) and data
(`~/.local/share/prx`) are not touched.

**PRX.app**

1. Choose *Quit PRX* in the menu. This also stops the daemon the app started. (A daemon you
   started yourself keeps running: stop it with `prx daemon stop`.)
2. Remove the old app: drag it from Applications to the Trash, or
   `rm -rf /Applications/PRX.app`.
3. Get the new one as in [Step 1](#step-1-get-prxapp), then allow it to open again
   ([Step 2](#step-2-allow-it-to-open)) and open it:

   ```bash
   xattr -dr com.apple.quarantine /Applications/PRX.app
   open /Applications/PRX.app
   ```

As long as the app stays in the same folder, the `~/.local/bin/prx` link and *Open at login* keep
working without changes.

**CLI**

```bash
prx daemon stop
# download and unpack the new tarball as above (on macOS, clear the quarantine flag), then:
install -m 755 prx-v*/prx ~/.local/bin/prx
prx daemon start
```

## Uninstall

1. Remove the login autostart, if you enabled it (*Open at login* in the app, or
   `prx daemon install`), and stop prx:

   ```bash
   prx daemon uninstall     # removes the launchd or systemd registration
   prx daemon stop          # or Quit PRX in the menu
   ```

2. Remove the program:

   ```bash
   rm -rf /Applications/PRX.app     # macOS, if you installed the app
   rm -f ~/.local/bin/prx           # the CLI, or the link to the app's copy
   ```

3. Optionally remove your configuration, review history, and logs:

   ```bash
   rm -rf ~/.config/prx ~/.local/share/prx
   ```

If `prx` is already gone, the autostart registration is one of these files, which you can delete
by hand: on macOS `com.prx.app.plist`, `com.prx.daemon.plist`, or `com.prx.menubar.plist` in
`~/Library/LaunchAgents`; on Linux `~/.config/systemd/user/prx-daemon.service` (run
`systemctl --user disable prx-daemon.service` first).

macOS itself keeps a few small files for the app: `~/Library/Preferences/com.prx.app.plist`,
`~/Library/Caches/com.prx.app`, and `~/Library/WebKit/com.prx.app`. They are safe to delete.

## Next

Continue with the [Quickstart](quickstart.md), then [Your first review](first-review.md). If
something on this page did not go as described, see
[Troubleshooting](../troubleshooting.md#macos-will-not-open-prxapp-or-prx).
