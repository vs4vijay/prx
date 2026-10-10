# CLI reference

```
prx init                          create config + data dirs, write the starter config (never
                                  overwrites; PRX.app does this itself on a first launch)
prx doctor                        self-check: config, gh binary + auth, agents, prompts, data dir
prx repos add <org/repo>          add a repo to watch
prx repos list                    show configured repos (effective agent/poll/skip rules)
prx repos remove <org/repo>       stop watching
prx status [org/repo] [--json]    current state of watched PRs (through the daemon when up)
prx review <target>               review one PR (bypasses cooldowns; daemon-first)
        [--engine NAME] [--no-post] [--prompt TEXT]
                                  per-run options: another engine, no GitHub writes, extra instructions
prx review --all                  review all currently-due PRs (respects cooldowns)
prx review <target> --dry-run     full pipeline, ZERO GitHub writes and zero state changes
prx brief <target> [--regenerate] print the PR's brief and its dates; --regenerate writes a new
                                  one first (never posts to GitHub; daemon-first)
prx approve <org/repo>#<n>        post YOUR GitHub approval (the only approve path)
prx snooze <target> --hours N | --until-push
prx ignore <target> / prx unignore <target>
prx review-now                    ask the daemon to poll + enqueue what's due now
prx pause --hours N | --until T   pause automatic reviews daemon-wide (polling continues)
prx resume                        lift the pause
prx history [--repo org/repo] [--limit N] [--json]
prx insights [--repo org/repo] [--days N]
prx usage [--by day|week|month] [--window 7d|30d|90d|all] [--group none|engine|repo|pr|session]
          [--repo org/repo] [--json]
                                  token usage and cost of review runs, aggregated from history
prx config                        show the fully-merged effective configuration
prx config edit                   open the config file in $VISUAL (or $EDITOR)
prx config set <key> <value>      set a dotted config key, validated before saving
prx daemon start [--foreground]   detached background daemon (single instance)
prx daemon stop                   SIGTERM the daemon and wait for exit
prx daemon status                 is the daemon running (pid + IPC port)?
prx daemon logs                   tail the newest daemon log file
prx daemon install [--app PATH] [--binary-path PATH] [--include-menubar]
                                  register login autostart: the PRX.app login item when the app is
                                  found, else the daemon (launchd/systemd; never starts anything)
prx daemon uninstall              remove the login-autostart registrations (running daemon left alone)
```

Mutating commands (`review`, `approve`, `snooze`, `ignore`, `unignore`, `review-now`, `pause`,
`resume`, plus the
read-only `status`/`history`/`insights`) go through the daemon when one is answering and fall back
to best-effort in-process execution with a warning when it is down. `--dry-run` never touches the
daemon at all — it runs the pipeline locally and prints what *would* have happened.

## `prx review`

`<target>` is a PR in any of these forms:

| Form | Example | Resolves to |
|------|---------|-------------|
| `org/repo#N` | `acme/api#42` | that PR (the repo must be watched) |
| GitHub PR URL | `https://github.com/acme/api/pull/42/files` | the same (trailing path, query, and fragment are ignored) |
| `repo#N` | `api#42` | the watched repo with that short name |
| `N` / `#N` | `42`, `#42` | the one watched repo (mode not `off`) with an open PR #42 — tracked PRs match without a gh call, the rest are looked up concurrently |

The short forms are resolved by the daemon; when several repos match, nothing is enqueued and the
error lists the candidates (use `org/repo#N`). A PR prx doesn't track yet is fetched and tracked on
the spot — exactly as a poll would have found it, skip rules included (an untracked draft is tracked
as skipped and reported not eligible) — and then reviewed; a merged or closed PR is rejected with its
state. `--dry-run`, and the in-process fallback when the daemon is down, accept only `org/repo#N`
and URLs (with a hint saying so).

The per-run options apply to that run only and are never saved:

- `--engine <NAME>` — run any configured engine instead of the repo's. A different engine uses its
  own default model (the repo's `agent_model` is not applied) and runs as a fresh session, leaving
  the PR's retained session for the next default run; an on-demand brief keeps the repo's
  brief engine. Unknown names are rejected with the engine list; history records the engine used.
- `--no-post` — review without posting to GitHub, like `monitor` mode for this run: the verdict,
  state, and history update; the history entry is marked `(not posted)`. (A run can never post more
  than its repo's `review_mode` allows.)
- `--prompt <TEXT>` — extra instructions appended to this run's prompt (see [Prompts](../guides/prompts.md)).

The options conflict with `--all`. Requesting a PR that is already queued replaces the queued job's
options; if a review of it is already running, the request is not queued and the note says so.

## `prx brief`

`prx brief <target>` prints the PR's [brief](../guides/briefs.md), then `created <time> · updated <time>` (local
time), plus `(stale)` when the PR's head has moved past the commit the brief describes. `<target>`
takes the same forms as `prx review` (short forms need the daemon). A PR without a brief exits
non-zero with a hint.

`--regenerate` writes a new brief first: through the daemon it sends `POST /brief` and waits until
the brief lands; without the daemon it runs in-process. It works for any tracked PR, reviewed or
not, in any repo mode except `off`, and never writes to GitHub. Regenerating keeps `created_at`. If
generation fails, the previous brief is kept and the command reports the error.

## `prx usage`

`prx usage` reports the [token usage](../guides/token-usage.md) and reported cost of review runs from
`history.jsonl`: one row per bucket (`--by day|week|month`, weeks are ISO weeks starting Monday,
local time) over `--window 7d|30d|90d|all` (default `day` over `30d`), optionally split by
`--group engine|repo|pr|session` (a session is `engine:session_id`, so resumed re-reviews roll up)
and restricted with `--repo org/repo`. Columns: input (uncached), output, cache read, cache write,
cost (`-` when no run reported one), and runs. Grouped output prints each group's rows and total,
and every report ends with an `overall` row plus the number of runs without usage data (e.g.
`pi` runs, or entries recorded before usage capture). `--json` prints the [`GET /usage`](ipc-api.md)
shape. Like `prx history`, it asks the daemon when it is up and otherwise reads the history file
locally with the same aggregation.

## `prx doctor`

Checks that everything prx needs is in place and prints one line per check: the config file parses
and validates, `gh` is on PATH and authenticated (via `gh auth status`), every referenced engine
resolves — agent binaries on PATH, `llm_api` engines with their key variable set (a warning
otherwise), including `brief.engine` references — which levels use the built-in vs a custom review prompt
(warning when an edited legacy `prompts/default.md` would be lost; an informational hint when a custom
prompt still carries its own manifest schema), and the data directory is
writable. It also reports the daemon's and menubar's login-autostart registrations as
informational lines that never fail the run (see [Autostart](../guides/autostart.md)). Exit code is non-zero on
any other failure — CI-friendly.
