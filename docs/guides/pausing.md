# Pausing reviews

A pause is a daemon-wide, timed hold on **automatic** reviews — for a demo, a release freeze, or a
noisy afternoon:

```bash
prx pause --hours 2                  # fractional hours work too: --hours 0.5
prx pause --until 18:00              # next 18:00 local time (today, or tomorrow if past)
prx pause --until "2026-10-08 09:00" # local date + time; RFC 3339 also accepted
prx resume                           # lift it early
```

Exactly one of `--hours` / `--until` is required and the end must be in the future; pausing again
replaces the current end. The menu bar's *Review mode ▸* submenu offers the same control in its
*Pause automatic reviews* section (30 min / 1 h / 2 h / 4 h, *Resume now*), and so does the PRX window's Dashboard and General page. The pause is stored as `paused_until` in `state.json`, so it **survives daemon
restarts**, and it simply expires at its end time — no resume step needed.

While paused, nothing is enqueued automatically: scheduled ticks are consumed without a review
(missed fires are skipped, never replayed), the interval due-scan enqueues nothing, and
`prx review --all` / `prx review-now` enqueue nothing (with a note). **Polling continues**, so
`prx status` and the menu stay current. An explicit single-PR review — `prx review org/repo#42` or
the menu's *Re-review now* — still runs, with a note that reviews are paused.

How it differs from `review_mode: off`:

| | Pause | `review_mode: off` |
|---|---|---|
| Scope | daemon-wide | defaults / org / repo |
| Duration | timed; expires by itself | until you change the mode |
| Polling + state | **continues** | stops (last-known state served) |
| Explicit `prx review org/repo#n` | runs (with a note) | refused |
| Shown as | `… · paused, 1h 12m left` in the menu header and on the *Review mode* submenu title | *Reviews off* |

The two compose — a review is enqueued automatically only when prx is **not paused and** the repo's
mode is not `off` — and `prx status` prints each state on its own line. Pausing does not touch
cooldowns.
