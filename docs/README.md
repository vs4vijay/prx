# prx documentation

prx watches your GitHub repositories, has a local coding agent or an LLM API review each pull
request read-only, and posts the result itself. Only you approve. New here? Read
[Install](getting-started/install.md), then the [Quickstart](getting-started/quickstart.md).

## Getting started

| Page | What it covers |
|------|----------------|
| [Install](getting-started/install.md) | Download PRX.app or the CLI from a release, the macOS quarantine step, first launch, updating, uninstalling |
| [Quickstart](getting-started/quickstart.md) | From `prx init` to a running daemon |
| [Your first review](getting-started/first-review.md) | What happens to one PR, step by step |

## Guides

| Page | What it covers |
|------|----------------|
| [Review modes](guides/review-modes.md) | `review comment`, `monitor`, and `off` |
| [Review schedule](guides/schedule.md) | Cron expressions that limit when reviews run |
| [Pausing reviews](guides/pausing.md) | A timed, daemon-wide hold on automatic reviews |
| [Prompts](guides/prompts.md) | Review guidance, extra instructions, the fixed output contract |
| [Commentary](guides/commentary.md) | Inline comments and the review body template |
| [Briefs](guides/briefs.md) | Plain-language descriptions of what a PR does |
| [Engines](guides/engines.md) | Agent CLIs and LLM APIs, sessions, validation |
| [Token usage](guides/token-usage.md) | What is recorded, per engine, and how to map your own |
| [Insights](guides/insights.md) | Throughput, recurring blockers, review effectiveness |
| [Notifications](guides/notifications.md) | Banners from the daemon and from PRX.app |
| [Browser, identity, and Block](guides/browser-identity-block.md) | Which browser opens PRs, which account posts, Block / Unblock |
| [Auto-open](guides/auto-open.md) | Open a PR in the browser when its review completes |
| [Autostart](guides/autostart.md) | Start prx at login with launchd or systemd |
| [PRX.app user guide](guides/prx-app.md) | The macOS menu, window, and settings pages |

## Reference

| Page | What it covers |
|------|----------------|
| [Configuration](reference/configuration.md) | Every config key, notification tiers, org and repo overrides, live reload |
| [CLI](reference/cli.md) | Every command, target forms, per-run options |
| [Environment variables](reference/environment.md) | Variables prx reads, names, and sets |
| [IPC API](reference/ipc-api.md) | The daemon's localhost JSON API |
| [Review manifest](reference/review-manifest.md) | The JSON an engine must produce |
| [Data files](reference/data-files.md) | What prx stores and where |

## Concepts

| Page | What it covers |
|------|----------------|
| [Architecture](concepts/architecture.md) | Why the agent analyzes and prx writes; lifecycle states |
| [Security model](concepts/security.md) | Credentials, network footprint, prompt injection, human-only approval |
| [PRX.app internals](concepts/prx-app-internals.md) | Bundle layout, daemon supervision, run loop |
| [Scope and non-goals](concepts/scope.md) | What prx does and does not try to do |

## Help and contributing

- [Troubleshooting](troubleshooting.md)
- [Development](development.md): [building from source](development.md#building-from-source),
  the test tiers, releasing
- [CONTRIBUTING.md](../CONTRIBUTING.md), [AGENTS.md](../AGENTS.md), [SECURITY.md](../SECURITY.md)
- [Internal design notes](internal/README.md): historical, partly stale
