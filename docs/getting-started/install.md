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

> **On macOS, one step is easy to miss.** A PRX.app or `prx` downloaded with a browser will not
> open until you clear its quarantine flag, with one command:
> [Clear the quarantine flag](#clear-the-quarantine-flag).

To compile prx yourself instead, see
[Building from source](../development.md#building-from-source).

## Before you start

prx needs two things on your machine to do anything useful:

- **The [GitHub CLI](https://cli.github.com/) (`gh`), signed in.** `gh auth status` must succeed.
  prx makes every GitHub call through `gh` and stores no tokens of its own.
- **Something to do the reviewing.** Either a coding-agent CLI (`pi`, `claude`, and `codex` work
  without configuration; others can be [configured](../guides/engines.md)), or an API key for an
  [LLM API engine](../guides/engines.md).

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

Every release carries the same six files. `<tag>` is the version, for example `v0.1.0`:

| File | Contents |
|------|----------|
| `PRX-<tag>-macos-arm64.zip` | PRX.app for Apple silicon: the menu bar app with `prx` inside |
| `prx-<tag>-aarch64-apple-darwin.tar.gz` | `prx` for macOS, Apple silicon |
| `prx-<tag>-x86_64-apple-darwin.tar.gz` | `prx` for macOS, Intel |
| `prx-<tag>-x86_64-unknown-linux-gnu.tar.gz` | `prx` for Linux, x86_64 |
| `prx-<tag>-aarch64-unknown-linux-gnu.tar.gz` | `prx` for Linux, arm64 |
| `SHA256SUMS` | Checksums of the five files above |

You can download a file in two ways:

- **In a browser**, from the [latest release](https://github.com/vs4vijay/prx/releases/latest).
  On macOS, a browser marks what it downloads as quarantined, and unpacking passes the mark on to
  the app or to `prx`.
- **In a terminal**, with the `gh` you already have. This is what the commands on this page use.
  Without a version it fetches the newest release, so the commands stay the same for later
  versions; to get a particular one, put its tag after `download`
  (`gh release download v0.1.0 --repo …`).

## PRX.app (macOS)

### Download and unpack

```bash
cd ~/Downloads
gh release download --repo vs4vijay/prx --pattern 'PRX-*-macos-arm64.zip' --pattern SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing     # optional; prints "PRX-<tag>-macos-arm64.zip: OK"
ditto -x -k PRX-*-macos-arm64.zip .              # unpacks PRX.app into the current folder
```

If you downloaded the zip in a browser, double-click it in Finder instead of running `ditto`
(Safari may already have unpacked it). Either way you end up with `PRX.app` in `~/Downloads`. The
`PRX-*` pattern expects one PRX zip in the folder; delete older ones first.

### Move it to Applications

Drag `PRX.app` into the Applications folder, or:

```bash
mv PRX.app /Applications/
```

`~/Applications` works too. The rest of this page assumes `/Applications`.

### Clear the quarantine flag

**Do this before you open the app for the first time:**

```bash
xattr -dr com.apple.quarantine /Applications/PRX.app
```

It prints nothing when it works, and does no harm if the app was not quarantined.

Why it is needed: PRX.app is signed ad hoc and is not notarized by Apple. macOS puts a
quarantine flag on anything a browser downloads, and refuses to open a quarantined app that Apple
has not notarized. The command removes that flag from the app and from the `prx` inside it. A
later release may be signed and notarized; until then this step is needed after every browser
download, including updates.

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

### First launch

prx reads its settings from `~/.config/prx/config.yml`, and the daemon does not start without
that file. Create it once, with the `prx` inside the app, then open the app:

```bash
/Applications/PRX.app/Contents/Helpers/prx init
open /Applications/PRX.app
```

You can also double-click PRX in Finder or open it from Spotlight. What to expect:

- A PRX icon appears in the menu bar, and the PRX window opens on the Dashboard. PRX has a Dock
  icon only while its window is open; closing the window leaves the menu bar icon running.
- The app starts the daemon for you, or attaches to one that is already running. You do not run
  `prx daemon start` yourself.
- Nothing is watched yet, so the Dashboard offers *Add a repo*. From there, continue with the
  [Quickstart](quickstart.md).

If you opened the app before running `prx init`, the menu shows *Daemon restarting* and then
*Daemon stopped*. Run the `init` command above, then choose *Start daemon* in the menu.

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

Set `target` to the line for your machine, then run the rest as written:

```bash
target=aarch64-apple-darwin          # macOS, Apple silicon
# target=x86_64-apple-darwin         # macOS, Intel
# target=x86_64-unknown-linux-gnu    # Linux, x86_64
# target=aarch64-unknown-linux-gnu   # Linux, arm64

gh release download --repo vs4vijay/prx --pattern "prx-*-$target.tar.gz" --pattern SHA256SUMS
shasum -a 256 -c SHA256SUMS --ignore-missing     # optional; on Linux: sha256sum -c SHA256SUMS --ignore-missing
tar -xzf prx-*-"$target".tar.gz
xattr -dr com.apple.quarantine prx-*-"$target"   # macOS only
mkdir -p ~/.local/bin
install -m 755 prx-*-"$target"/prx ~/.local/bin/prx
prx --version
```

Notes:

- The tarball unpacks to a folder of the same name (`prx-<tag>-<target>/`) holding `prx`,
  `README.md`, and `LICENSE`. Only `prx` is needed; it can live in any directory on your `PATH`.
- **macOS:** the `xattr` line matters when the tarball came from a browser. `tar` passes the
  quarantine flag on to `prx`, and macOS then stops it the moment it starts: the shell prints
  `killed` and nothing else. The reason is the same as for
  [PRX.app](#clear-the-quarantine-flag). If you already copied `prx` somewhere, clear it there:
  `xattr -d com.apple.quarantine ~/.local/bin/prx`.
- **Linux:** `tar` prints *Ignoring unknown extended header keyword
  'LIBARCHIVE.xattr.com.apple.provenance'* a few times. The archives are made on a Mac; the
  warning is harmless.
- If `prx --version` answers `command not found`, `~/.local/bin` is not on your `PATH`. Add
  `export PATH="$HOME/.local/bin:$PATH"` to `~/.zprofile` (zsh, the macOS default) or
  `~/.profile` (bash on most Linux systems) and open a new terminal.

Continue with the [Quickstart](quickstart.md). To start the daemon at login, see
[Autostart](../guides/autostart.md).

## Verify a download

`SHA256SUMS` lists the SHA-256 checksum of every other file in the release. Run the check in the
folder that holds `SHA256SUMS` and whatever you downloaded:

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
2. Download and unpack the new zip as in [Download and unpack](#download-and-unpack).
3. Replace the app and clear the quarantine flag again:

   ```bash
   rm -rf /Applications/PRX.app
   mv PRX.app /Applications/
   xattr -dr com.apple.quarantine /Applications/PRX.app
   open /Applications/PRX.app
   ```

As long as the app stays in the same folder, the `~/.local/bin/prx` link and *Open at login* keep
working without changes.

**CLI**

```bash
prx daemon stop
# download and unpack the new tarball as above, then:
install -m 755 prx-*-"$target"/prx ~/.local/bin/prx
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
