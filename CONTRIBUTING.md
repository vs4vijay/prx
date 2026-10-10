# Contributing to prx

Thanks for your interest. Bug reports, documentation fixes, and pull requests are welcome. For
anything larger than a small fix, open an issue first so the approach can be agreed before you
write code.

## Setup

You need a stable Rust toolchain and, for the settings UI tests, Node.js 20 or later. No npm
install is involved. To run prx itself you also need the GitHub CLI (`gh`), signed in.

```bash
git clone https://github.com/vs4vijay/prx
cd prx
cargo build
cargo run -- --help
```

To experiment without touching your own setup, give prx a scratch home:

```bash
PRX_CONFIG=/tmp/prx/config.yml PRX_DATA_DIR=/tmp/prx/data cargo run -- init
```

PRX.app (the `menubar/` crate) builds on macOS only: `cargo run -p prx-menubar`, or
`scripts/bundle-macos.sh --no-install` for an app bundle. Without `--no-install` that script
replaces `~/Applications/PRX.app`. The app needs no `prx init`: started with no config file, it
writes the default config (to `PRX_CONFIG` when set) before it starts its daemon, so set
`PRX_CONFIG` and `PRX_DATA_DIR` as above to keep an experiment out of your own setup.

[Building from source](docs/development.md#building-from-source) has the full build
instructions. The [Install](docs/getting-started/install.md) page is for users and covers the
release downloads only.

## Before you push

CI runs these on every pull request, and all of them must pass.

On Linux or macOS:

```bash
cargo build -p prx --locked
cargo test -p prx --locked
cargo clippy -p prx --all-targets --locked -- -D warnings
cargo fmt --check
node --test menubar/settings-ui-tests/*.test.js
```

On macOS, if you touched `menubar/`:

```bash
cargo build --workspace --all-targets --locked
cargo test -p prx-menubar --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

On Linux, keep the `-p prx`: the menubar crate does not build there.

## Tests

| Tier | Where | What it uses |
|------|-------|--------------|
| Unit | next to the code in `src/` and `menubar/src/` | pure logic: state machine, config merging, insights |
| Integration | `tests/` | the real `prx` binary against fake `gh` and fake agent scripts in `tests/fixtures/`; no network; Unix-only |
| Daemon and hardening | `tests/` | a real detached daemon: IPC, crashes, `SIGKILL`, corrupt files |
| Menubar | `menubar/` | macOS only |
| Settings UI | `menubar/settings-ui-tests/` | `node --test`, no dependencies |

Add a test with every behaviour change. Tests must not reach GitHub, a real agent, or a real LLM
API. More detail is in [docs/development.md](docs/development.md).

## Commit style

One descriptive subject line, prefixed `prx:` (core and repo-wide) or `prx-menubar:` (the app),
with clauses joined by dashes. A body of `-` bullets is optional. This project does not use
Conventional Commits.

```
prx: daemon autostart — launchd plist (macOS) + systemd user unit (Linux) install/uninstall, doctor autostart check with stale-path warning
```

## Rules that do not bend

The review engine never posts to GitHub and never approves; only prx writes, and only a human
approves. The core crate (`src/`) contains no GUI code. These and the other project invariants,
the repository layout, and known pitfalls are in [AGENTS.md](AGENTS.md). Read it before a larger
change, whether you are a person or a coding agent.

## Documentation

User documentation lives in [docs/](docs/README.md). When you change behaviour, update the page
that describes it in the same pull request.

## Security issues

Do not open a public issue for a vulnerability. See [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the [MIT License](LICENSE).
