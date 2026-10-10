# IPC API (localhost)

The daemon serves a JSON API on `127.0.0.1:<ephemeral>` — this is the backend of the CLI thin
client and the menu bar shell. Every request must carry `Authorization: Bearer <token>` with the
token from `daemon.token`; anything else gets `401`. Discover the port from `daemon.port`.

| Endpoint | Body / query | Response |
|----------|--------------|----------|
| `GET /status` | — | `{version, pid, uptime_seconds, in_flight, queue_depth, last_poll: {repo: ts}, last_state_poll, review_mode, paused_until?, config_error?, engines, menu, prs: [StatusEntry]}` — served from a read cache, stays responsive during reviews. `review_mode` is the global mode; `paused_until` is present only while a pause is in effect; `config_error` (`{message, at}`) is present while the daemon runs on a last-good config. `engines` lists the resolved engine names (sorted); `menu` is the effective `{sort, group_by}`. |
| `POST /review` | `{key: target \| "all", force: bool, extra_prompt?: string, engine?: string, post?: bool}` | `{enqueued: [keys], note?}` — `key` is any [target form](cli.md) (`org/repo#42`, a PR URL, `repo#42`, `42`, `#42`), resolved against the watched repos; an untracked open PR is tracked on demand first. `force` (an explicit target) bypasses cooldowns but never skip/terminal legality; `--all` enqueues only currently-due PRs. `extra_prompt` / `engine` / `post` shape that run only, never persisted (`post: false` = no GitHub writes). A request with options replaces the options of the PR's queued job; a plain one leaves them. Nothing is enqueued for a repo in mode `off`. Errors: `400` unknown engine (lists the engines) or malformed target (lists the forms), `404` unwatched repo / no such PR, `409` merged or closed PR / ambiguous short target (lists the candidates), `502` gh failure. |
| `POST /brief` | `{key: target}` | `202 {accepted: true, key}` — generate (or regenerate) the PR's [brief](../guides/briefs.md) in the background; `/status` shows `brief_pending` until it lands, or `brief_error` if it fails (the previous brief is kept). Never writes to GitHub. `key` takes the `/review` target forms. Errors: `404` unknown PR / unwatched repo, `409` a brief for that PR is already being written (only one engine run happens), `422` the repo's mode is `off` or `brief.enabled` is false. |
| `POST /approve` | `{key}` | `{ok, changed, note?}` — the human-only approve path. |
| `POST /block` | `{key}` | `{ok, changed, note?}` — post a request-changes review with the effective `block_message`. |
| `POST /unblock` | `{key}` | `{ok, changed, note?}` — dismiss prx's latest request-changes review; the PR returns to pending. |
| `POST /snooze` | `{key, hours? \| until_push?}` | `{ok, changed, note?}` — one of `hours` or `until_push` is required. |
| `POST /unsnooze` | `{key}` | `{ok, changed, note?}` |
| `POST /ignore` | `{key}` | `{ok, changed, note?}` |
| `POST /unignore` | `{key}` | `{ok, changed, note?}` |
| `POST /pause` | `{hours: number} \| {until: RFC 3339}` | `{ok, changed, note?, paused_until}` — exactly one field, ending in the future (else `400`); persisted in `state.json`. |
| `POST /resume` | `{}` | `{ok, changed, paused_until: null, note?}` — lifts the pause (`changed: false` when none was in effect). |
| `POST /review-now` | — | `{ok, changed, note}` — config reload check, then wakes the scheduler for an immediate poll + due-scan. |
| `GET /config` | — | `{path, yaml, raw, effective: {defaults, orgs, repos}, config_error?, shipped_review_prompt, review_contract, shipped_templates}` — the file text, the running config, and per-org/per-repo merged values with each value's source. `shipped_review_prompt` is the built-in guidance; `review_contract` is the output contract always appended to it (with a sample manifest path). |
| `PUT /config` | `{config}` (structured) **or** `{yaml}` (raw text) | `{ok, added, removed, discarded}` — validated in full, written atomically (`0600`), hot-applied. Invalid → `422 {error, field?}` and nothing is written. A structured write re-serializes the YAML (comments dropped); a raw write is saved verbatim. |
| `GET /discover/agents` | — | `{agents: [{name, label, command, args, model_args, models, builtin, path?, on_path}], configured, llm_apis: [{name, provider, model, api_key_env, key_present}]}` — the agent CLIs prx knows and where the daemon found each one, plus the configured `llm_api` engines and whether each key variable is set in the daemon's environment (never the value). |
| `GET /discover/repos` | — | `{repos: [{name, private}]}` — non-archived repositories the gh account can see (most recently pushed first, up to 500). |
| `POST /review-mode` | `{mode: "off" \| "monitor" \| "review comment"}` | `{ok, changed, persisted, note?}` — sets `defaults.review_mode`; `persisted: false` when the file is broken (applied in memory only). |
| `GET /history` | `?repo=org/repo&limit=N&before=RFC3339` | `{entries: [HistoryEntry]}` — newest first; corrupt lines skipped. `before` pages back: only entries strictly older than that timestamp (invalid → `400`). |
| `GET /usage` | `?bucket=day\|week\|month&window=7d\|30d\|90d\|all&group=none\|engine\|repo\|pr\|session&repo=org/repo` | `{bucket, window, group, series: [{group, points: [{bucket_start, totals}]}], groups: [{group, totals}], overall, runs_without_usage}` — [token usage](../guides/token-usage.md) aggregated from history (defaults `day` / `30d` / `none`; invalid values → `400` listing the allowed ones). Local-date buckets (`week` = ISO, Monday start); points cover every bucket in the window. `totals` = the four token fields, `cost_usd` (reported costs only), `runs` (runs **with** usage), `runs_with_cost`, and the `review` / `summary` split; groups appear only when they have usage, and `runs_without_usage` counts the rest. |
| `GET /insights` | `?repo=org/repo&days=N` | The [`Insights`](../guides/insights.md) payload itself, self-describing (no envelope). |

`StatusEntry` carries the key (`org/repo#n`), title, author, head branch, lifecycle state, state
icon, skip reason, the repo's effective `engine`, one-line verdict summary, blocking/nit counts, last review time and its age, the
PR's GitHub creation time (`created_at`), prx's identity login when known (`identity_login`), the
PR's [brief](../guides/briefs.md) (`brief`: `{text, created_at, updated_at, head_sha, source, engine, stale}`,
when one exists and briefs are on), `brief_pending` while a brief is being written, `brief_error`
when the last brief run failed, `brief_available` (false when the repo's mode is `off` or briefs
are disabled for it, so `POST /brief` would answer 422), and the PR's GitHub URL (`html_url`). The
state icon distinguishes a comment-only verdict (💬) from an approved one (✅). CLI commands that mutate
act as thin clients against this API when the daemon is up and fall back to in-process execution
(with a warning) when it is not (all state + GitHub writes flow through the daemon).

## GET /insights payload

```json
{
  "generated_at": "2026-09-13T10:00:00Z",
  "window_days": 30,
  "reviews": {
    "total": 24, "failed": 1, "skipped": 3,
    "last_7_days": 9, "last_30_days": 24,
    "per_day": [{ "date": "2026-09-12", "reviews": 4 }]
  },
  "review_to_merge": { "samples": 7, "average_seconds": 50320 },
  "top_blocking_issues": [{ "text": "missing tenant scope", "count": 6 }],
  "per_author": [{
    "author": "alice", "reviews": 10, "approved": 7, "changes_requested": 3, "comment_only": 0,
    "approval_rate": 0.7, "median_blocking_count": 0.0
  }],
  "per_repo": [{
    "repo": "org/repo", "reviews": 12, "approved": 8, "changes_requested": 4, "comment_only": 0,
    "approval_rate": 0.67, "top_blocking_issues": [{ "text": "no tests", "count": 3 }]
  }],
  "effectiveness": { "changes_requested_reviews": 4, "followed_by_approved": 3, "rate": 0.75 }
}
```

`window_days` is omitted when analyzing all time. Aggregates are deterministic (alphabetical
authors/repos, count-then-text issue ordering), so the same history slice always serializes
identically — usable as data by the menu bar client.
