# Quickstart

This walks from an installed `prx` to a first review. If you have not installed it yet, start with
[Install](install.md). You need the GitHub CLI (`gh`) authenticated, and either a coding-agent CLI
(`pi`, `claude`, or `codex` work out of the box) or an [LLM API engine](../guides/engines.md).

The commands below need `prx` on your `PATH`. If you installed PRX.app and `prx` is not found,
choose *Install command-line tool…* under Settings → General in the app; see
[The `prx` command](install.md#the-prx-command). With PRX.app you can also skip the terminal:
see [With PRX.app](#with-prxapp-macos) below.

## Set up and review one PR

```bash
prx init                      # writes ~/.config/prx/config.yml; not needed once PRX.app has run
prx repos add org/repo        # watch a repository (inherits all defaults)
prx doctor                    # verify config, gh binary + auth, engines, prompts, data dir
prx status                    # see your open PRs (polls once; PRs enter "pending" with a cooldown)
prx review org/repo#42 --dry-run   # full pipeline, zero writes: prints what would be posted
prx review org/repo#42        # review one PR now (an explicit target bypasses cooldowns)
prx review --all              # review every currently-due PR across all watched repos
prx daemon start              # background daemon: polling, cooldowns, queue, notifications, IPC
prx status                    # now served by the daemon; insights/history too
```

![A terminal running prx status for every watched repository, prx brief for one pull request, and prx usage by week](../media/screenshots/cli.png)

`prx init` never overwrites a config: if PRX.app (or an earlier `prx init`) already wrote one, it
says `config already exists` and changes nothing. The review prompt is built in, so there is no
prompt file to create.

`--dry-run` is the safe way to see what prx would post: it runs the whole pipeline locally and
writes nothing to GitHub or to prx's own state. See [First review](first-review.md) for what
happens during a real one.

## With PRX.app (macOS)

If you [installed PRX.app](install.md#prxapp-macos) (a menu bar item and a window, with the CLI
and daemon inside), open it instead of running `prx daemon start`:

```bash
open /Applications/PRX.app
```

PRX.app starts the daemon for you (or attaches to one that is already running), opens its window,
and offers *Open at login* and *Install command-line tool…* under Settings → General — see
[PRX.app](../guides/prx-app.md). You do not run `prx init` either: on a first launch with no
config file, PRX.app (v0.1.1 or later) writes the same default config that `prx init` does. The
Dashboard then offers *Add a repo*; repositories can be added on the window's *Repos & Orgs* page
as well as with `prx repos add`.

## Day to day

```bash
prx approve org/repo#42       # post YOUR GitHub approval (the only approve path)
prx snooze org/repo#42 --hours 2 [--until-push]
prx ignore org/repo#42        # permanent skip until `prx unignore`
prx pause --hours 2           # stop automatic reviews for a while (polling continues)
prx resume                    # lift the pause early
prx history                   # timeline of reviews, merges, failures
prx insights                  # analytics: throughput, blockers, effectiveness
prx daemon stop
```

## Next steps

- [Review modes](../guides/review-modes.md): post to GitHub, only monitor, or switch off.
- [Engines](../guides/engines.md): choose the agent or LLM API that reviews.
- [Prompts](../guides/prompts.md): tell the reviewer what matters in your repos.
- [Autostart](../guides/autostart.md): start prx at login.
- [Configuration reference](../reference/configuration.md) and [CLI reference](../reference/cli.md).
