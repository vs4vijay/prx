# Install

prx is one binary, `prx`, that contains the CLI and the background daemon. On macOS there is also
PRX.app, a menu bar client that bundles the same binary. Pick whichever fits:

| You want | Use |
|----------|-----|
| The CLI and daemon on macOS or Linux | A [release tarball](#cli-from-a-release), or [build from source](#cli-from-source) |
| The menu bar app on macOS | The [PRX.app release zip](#prxapp-from-a-release), or [build it from source](#prxapp-from-source) |

## Requirements

- The [GitHub CLI](https://cli.github.com/) (`gh`), authenticated (`gh auth status` succeeds). prx
  makes every GitHub call through it and stores no tokens of its own.
- Something to do the reviewing: a coding-agent CLI on your machine (`pi`, `claude`, and `codex`
  are built in; others can be [configured](../guides/engines.md)), or an API key for an LLM API
  engine.
- To build from source: a stable Rust toolchain.
- For PRX.app: macOS 12 or later.
- Desktop notifications use `osascript` on macOS and `notify-send` on Linux. They are optional.

## CLI and daemon

### CLI from a release

Tagged versions are published on the
[releases page](https://github.com/vs4vijay/prx/releases). If that page has no release yet, build
from source instead. Each release carries:

| Asset | Contents |
|-------|----------|
| `prx-<tag>-aarch64-apple-darwin.tar.gz` | CLI and daemon, macOS Apple silicon |
| `prx-<tag>-x86_64-apple-darwin.tar.gz` | CLI and daemon, macOS Intel |
| `prx-<tag>-x86_64-unknown-linux-gnu.tar.gz` | CLI and daemon, Linux x86_64 |
| `prx-<tag>-aarch64-unknown-linux-gnu.tar.gz` | CLI and daemon, Linux arm64 |
| `PRX-<tag>-macos-arm64.zip` | PRX.app for Apple silicon (the app plus its bundled CLI) |
| `SHA256SUMS` | Checksums of every asset above |

Each tarball unpacks to a directory of the same name holding `prx`, `README.md`, and `LICENSE`:

```bash
shasum -a 256 -c SHA256SUMS --ignore-missing     # Linux: sha256sum -c SHA256SUMS --ignore-missing
tar -xzf prx-<tag>-<target>.tar.gz
install -m 755 prx-<tag>-<target>/prx ~/.local/bin/prx   # or any directory on your PATH
prx --version
```

### CLI from source

```bash
git clone https://github.com/vs4vijay/prx
cd prx
cargo install --path .        # or: cargo build && use target/debug/prx
```

`cargo install --path .` is also the right choice for headless, CLI-only use on a Mac.

## PRX.app (macOS)

### PRX.app from a release

Download `PRX-<tag>-macos-arm64.zip` from the
[releases page](https://github.com/vs4vijay/prx/releases), unzip it, and move `PRX.app` to
`/Applications` or `~/Applications`.

PRX.app is ad-hoc signed and not notarized, so macOS quarantines a downloaded copy. Clear the flag
once before the first launch:

```bash
xattr -dr com.apple.quarantine /Applications/PRX.app
open /Applications/PRX.app
```

The release zip is built for Apple silicon only. On an Intel Mac, build the app from source.

### PRX.app from source

One script, no Apple developer account:

```bash
scripts/bundle-macos.sh                    # release build, assemble, ad-hoc sign, install to ~/Applications
scripts/bundle-macos.sh --install /Applications   # another install directory
scripts/bundle-macos.sh --no-install       # build only: <target>/bundle.noindex/PRX.app
open ~/Applications/PRX.app                # or Spotlight / Finder: "PRX"
```

An app built on your own machine is not quarantined. The app icon is rendered with `rsvg-convert`
when it is installed (`brew install librsvg`), and otherwise comes from the icon set committed in
the repository.

`cargo run -p prx-menubar` runs the UI straight from a checkout; it supervises the `prx` binary
next to it in `target/`.

### The `prx` command from the app

The app carries the CLI at `PRX.app/Contents/Helpers/prx`. Settings → General → *Install
command-line tool…* links it to `~/.local/bin/prx`, so `prx` in a terminal is the app's own CLI.
See the [PRX.app guide](../guides/prx-app.md#settings).

## Next

Continue with the [Quickstart](quickstart.md). To have prx start at login, see
[Autostart](../guides/autostart.md).
