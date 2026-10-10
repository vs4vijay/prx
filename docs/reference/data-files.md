# Data files

Everything prx persists lives under `~/.local/share/prx` (override with `PRX_DATA_DIR`):

| Path | Purpose |
|------|---------|
| `state.json` | Current lifecycle state of every tracked PR — including its retained review session and its [brief](../guides/briefs.md) — plus the daemon-wide pause end (`paused_until`) (atomic temp+rename writes; schema versioned, still v1). |
| `history.jsonl` | Append-only event log: one JSON object per review / failure / skip / merge / close. Verified on load: a corrupt line is skipped, not fatal. |
| `daemon.port` | The localhost port the running daemon's IPC API bound (ephemeral). |
| `daemon.token` | Fresh 64-hex-char bearer token, written `0600` on every daemon start. |
| `daemon.pid` | Single-instance guard; also used by `prx daemon stop`/`status`. |
| `logs/prx.log.YYYY-MM-DD` | Daily rotating daemon logs; older files are pruned automatically. `prx daemon logs` tails the newest. |
| `logs/daemon-stderr.log` | Start-up errors of a daemon started by PRX.app. |
| `app.pid` | Single-instance guard for PRX.app (one running app, one window). |
| `manifests/` | The agent-produced review manifests, one file per run (audit trail). |

State-machine facts worth knowing: a review's in-progress transition is applied **in memory only**
until the run completes or fails, so a crash or `kill -9` mid-review can never strand a PR in
`in_review` — the next run picks it up cleanly. History older than 90 days is compacted (atomically)
on daemon start.

The config file lives separately, at `~/.config/prx/config.yml` — see the
[configuration reference](configuration.md).
