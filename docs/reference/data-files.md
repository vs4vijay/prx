# Data files

Everything prx persists lives under `~/.local/share/prx` (override with `PRX_DATA_DIR`):

| Path | Purpose |
|------|---------|
| `state.json` | Current lifecycle state of every tracked PR — including its retained review session (with the engine and directory it came from, `review_session_info`), its [brief](../guides/briefs.md), and `skip_detail` (the matched skip rule, or the diff size against the cap, in a few words; optional) — plus the daemon-wide pause end (`paused_until`) (atomic temp+rename writes; schema versioned, still v1). |
| `history.jsonl` | Append-only event log: one JSON object per new PR / review / failure / skip / merge / close (the `event` field names it; a new or reopened PR is `new_pr`). Verified on load: a corrupt line is skipped, not fatal. A line with an `event` this version does not know is skipped when read and kept as it is by compaction, so an older prx can share the file with a newer one. |
| `daemon.port` | The localhost port the running daemon's IPC API bound (ephemeral). |
| `daemon.token` | Fresh 64-hex-char bearer token, written `0600` on every daemon start. |
| `daemon.pid` | Single-instance guard; also used by `prx daemon stop`/`status`. |
| `daemon.stop-requested` | Written by `prx daemon stop`, removed by the next daemon start. While it exists, PRX.app does not start a daemon on its own. |
| `logs/prx.log.YYYY-MM-DD` | Daily rotating daemon logs; older files are pruned automatically. `prx daemon logs` tails the newest. |
| `logs/daemon-stderr.log` | Start-up errors of a daemon started by PRX.app or by `prx daemon start`. |
| `app.pid` | Single-instance guard for PRX.app (one running app, one window). |
| `manifests/` | The agent-produced review manifests, one file per run (audit trail). A PR's record in `state.json` points at its latest (`last_manifest`). |
| `chats.json` | [Review Chat](../guides/review-chat.md) records, by PR key: engine, session id (when known), directory, origin (`new` or `review`), and times. Written only by `prx chat` (atomic, `0600`); a corrupt file is moved to `chats.json.corrupt`. |
| `chats/<org>/<repo>/<n>/` | The directory a new Review Chat runs in (`0700`). |
| `chats/launch/*.command` | The scripts PRX.app opens in Terminal to run `prx chat <key>` (quoted paths and the key only). |

State-machine facts worth knowing: a review's in-progress transition is applied **in memory only**
until the run completes or fails, so a crash or `kill -9` mid-review can never strand a PR in
`in_review` — the next run picks it up cleanly. History older than 90 days is compacted (atomically)
on daemon start.

The config file lives separately, at `~/.config/prx/config.yml` — see the
[configuration reference](configuration.md).
