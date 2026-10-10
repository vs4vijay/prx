# prx — Rust Implementation Plan (working notes, binding for all agents)

This document adapts `spec.md` (v0.1 draft) and `implementation-plan.md` to a **Rust** implementation
(user decision: everything in Rust; the D1 "TypeScript + Swift" split is superseded). The audit
findings A1–A24 in `design-audit.md` are **binding design requirements** — implement their
resolutions, especially the A1–A3 architecture change below.

## 0. The core architecture (audit resolution — memorize this)

**"Agent analyzes; prx writes."**

- The coding agent is **read-only**: it fetches the diff / repo standards and writes a **JSON
  review manifest** to a file. It NEVER runs `gh pr review`, never posts to GitHub, never approves.
- prx validates the manifest, posts inline comments itself via `gh api` (REST), and submits one
  review body via `gh pr review` (`--request-changes` if any blocking finding, else `--comment`).
  Approve is posted ONLY via the human's explicit action (`prx approve` / menu).
- The manifest is the machine source of truth for state, verdict, counts, history, insights.

## 1. Stack decisions (Rust)

- Single binary crate `prx`, edition 2021, repo root `/workspace/prx`.
- Deps: `clap` (derive), `tokio` (full), `axum` (localhost IPC API), `serde`+`serde_json`+`serde_yaml` (0.9),
  `chrono` (serde), `thiserror`, `anyhow` (CLI-level errors), `dirs`, `rand` (jitter),
  `tracing` + `tracing-subscriber` + `tracing-appender` (daily rotating logs), `glob` (branch patterns).
- No direct HTTP to GitHub ever — all GitHub I/O goes through the `gh` CLI (auth delegated to gh).
- Notifications: `osascript` on macOS, `notify-send` on Linux; best-effort, never fatal.
- Testability: everything I/O-bound sits behind traits (`Gh`, `AgentAdapter`, `Notifier`); tests use
  fake `gh` / fake agent shell scripts in `tests/fixtures/` and `PRX_DATA_DIR`/`PRX_CONFIG` env overrides.

## 2. Layout (single crate)

```
src/main.rs        entry, dispatch
src/cli.rs         clap command types
src/paths.rs       config/data dir resolution (PRX_CONFIG, PRX_DATA_DIR env overrides)
src/config.rs      load/validate/merge (defaults ← repo), ~ expansion, label/branch skip rules
src/state.rs       state types + StateStore (atomic write: temp+rename), history append helpers
src/machine.rs     PURE transition table (no I/O) — the heart; exhaustive unit tests
src/github.rs      trait Gh + GhCli impl (shells out), PR list/view/diff types, posting helpers
src/agent.rs       AgentAdapter trait, config-driven command runner (timeout+kill), manifest types + strict validation
src/review.rs      pipeline: render prompt → run adapter → validate manifest → post via Gh → state/history/notify
src/notify.rs      event → tier mapping, tier → banner/sound
src/history.rs     history.jsonl append + query + insights computation
src/daemon.rs      scheduler loop (interval+jitter), global FIFO work queue (max N concurrent), single-instance guard
src/ipc.rs         axum server (auth token, port file) + client used by CLI
src/logging.rs     tracing init (rotating file + stderr for foreground)
tests/             integration tests + fixtures (fake gh, fake agent)
```

## 3. File locations

- Config: `$PRX_CONFIG` or `~/.config/prx/config.yml`
- Data dir: `$PRX_DATA_DIR` or `~/.local/share/prx`
  - `state.json` (atomic), `history.jsonl` (append-only), `daemon.port`, `daemon.token` (0600),
    `daemon.pid`, `logs/prx.log.YYYY-MM-DD`, `prompts/default.md` (written by `prx init`)

## 4. Config schema (YAML)

```yaml
defaults:
  agent: pi                       # key into agents map
  poll_interval_minutes: 15       # A7: 15 min, not 2h (cron deferred; D3)
  cooldown:
    before_first_review_minutes: 60    # A5: anchored to GitHub PR created_at
    between_re_reviews_minutes: 30     # A5: anchored to new-commit detection time
  review_prompt: default          # path or "default"; A12: override replaces, {{PARENT_PROMPT}} splices default in
  max_concurrent_reviews: 1       # D6 (up to 2)
  diff_max_files: 50              # A14 diff-size cap → skip + too_large note
  diff_max_lines: 4000
  skip:
    drafts: true                  # A11: drafts are TRACKED under Skipped(draft), not invisible
    authors: []                   # exact match
    labels: ["wip", "do-not-review"]  # A19: case-insensitive exact match
    branches: []                  # glob patterns
  notifications:                  # A10: these keys ARE the event list
    on_new_pr: informational
    on_review_complete_approved: informational
    on_review_complete_blocking: urgent
    on_new_commits: informational
    on_re_review_triggered: informational
    on_merged: silent
    on_closed: silent
    on_failure: urgent
    on_skipped: silent

notification_tiers:
  urgent:       { banner: true,  sound: Basso }
  informational:{ banner: true,  sound: null }
  silent:       { banner: false, sound: null }

agents:                           # optional overrides; built-ins exist for pi, claude, codex
  pi:
    command: pi
    args: ["--yes", "-p", "{{PROMPT}}"]
  claude:
    command: claude
    args: ["-p", "{{PROMPT}}", "--dangerously-skip-permissions"]
  codex:
    command: codex
    args: ["exec", "-"]
# agent reads the prompt on stdin when args contains "-", else {{PROMPT}} is substituted.
# prx appends instructions telling the agent to write the manifest to {{MANIFEST_PATH}}.

repos:                            # per-repo overrides; only specify what differs
  - name: org/repo
    poll_interval_minutes: 15
    agent: claude
    review_prompt: ~/.config/prx/prompts/myrepo.md
    skip: { authors: ["dependabot[bot]"] }
```

## 5. PR state record (state.json, schema_version 1)

Key: `"owner/repo#<number>"`.

```json
{
  "repo": "org/repo", "number": 42, "title": "...", "author": "...",
  "head_branch": "feat/x", "head_sha": "abc123", "is_draft": false,
  "gh_created_at": "2026-09-01T10:00:00Z",
  "state": "pending|in_review|reviewed_approved|reviewed_changes_requested|skipped|merged|closed",
  "previous_state": "reviewed_changes_requested",   // restored on snooze expiry (A6)
  "skip_reason": null,                              // draft|config|snoozed|ignored (A6)
  "snooze_until": null, "snooze_until_push": false, // A9
  "review_anchor_at": "2026-09-01T10:00:00Z",       // A5: first=gh_created_at; re-review=commit-detection time
  "last_reviewed_sha": "abc123", "last_review_at": null,
  "last_verdict": null,                             // approved|changes_requested|comment_only
  "last_summary": null, "blocking_count": 0, "nit_count": 0,
  "review_session": null, "review_attempts": 0,
  "diff_too_large": false, "updated_at": "..."
}
```

Note: the spec's "New" and "Cooling Down" are MERGED into one `pending` state (A5); UI/CLI derive
"cooling down" from `pending` + `review_anchor_at` + cooldown. "Re-review Pending" is `pending` with
a prior verdict (also derivable, not a separate enum value).

## 6. Transition table (machine.rs) — A5, A8, A9 edges are REQUIRED

Input events: `PollNewPr`, `PollShaChanged`, `PollUnchanged`, `PollMerged`, `PollClosed`,
`PollReopened`, `PollDraftFlipped`, `ReviewStart`, `ReviewDone{verdict,blocking,nits}`,
`ReviewFailed`, `Snooze{until?,until_push}`, `SnoozeExpired`, `Ignore`, `Unignore`.

Rules:
- `PollNewPr` → if skip rules match → `skipped(draft|config)`; else `pending` (anchor=gh_created_at).
- `pending` + cooldown elapsed → eligible for review queue (scheduler picks up, not the table).
- `ReviewStart` → `in_review` (only from `pending` or eligible reviewed states).
- New commits while `in_review` (`PollShaChanged`) → record pending re-review; **let the running
  review finish**, then queue re-review (never abort unless terminal).
- `PollMerged`/`PollClosed` while `in_review` → abort review, mark `merged`/`closed` (terminal, but
  `PollReopened` on `closed` → fresh `pending`).
- `ReviewDone` → `reviewed_approved` | `reviewed_changes_requested` (+ reviewed_sha, counts).
- `reviewed_*` + `PollShaChanged` → `pending` with anchor = now (re-review cooldown).
- `Snooze` → `skipped(snoozed)`, keep `previous_state`; `SnoozeExpired` → restore previous_state;
  if `snooze_until_push` and `PollShaChanged` → wake immediately.
- `Ignore` → `skipped(ignored)` (idempotent, works from any state); `Unignore` → `pending`
  (anchor=now) unless terminal.
- `PollDraftFlipped` (draft→ready) → `skipped(draft)` → `pending`. `draft: true` from non-draft →
  `skipped(draft)`.

## 7. Review manifest (contract, versioned)

Agent writes this JSON to the path given as `{{MANIFEST_PATH}}`:

```json
{
  "manifest_version": 1,
  "verdict": "approved|changes_requested|comment_only",
  "summary": "one-line verdict",
  "findings": [{"file": "src/x.rs", "line": 12, "severity": "blocking|nit", "body": "..."}],
  "duration_seconds": 95
}
```

Strict validation; on failure retry the agent once, then `on_failure` notification + state stays
put, history records the failure. `verdict` must be consistent with findings (any `blocking` →
`changes_requested`).

## 8. Posting (review.rs via github.rs)

- Inline: `gh api repos/{o}/{r}/pulls/{n}/comments -f body=... -F commit_id=<sha> -f path=... -F line=<n>`
  (per-comment best-effort; collect failures, don't abort the review).
- Review body: `gh pr review -R o/r#n --request-changes|--comment --body-file -` with verdict,
  findings summary, and footer `— automated review by prx` (A13 attribution).
- Human approve: `gh pr review -R o/r#n --approve` (the ONLY approve path; D2).
- Dry-run: `prx review o/r#n --dry-run` runs the full pipeline (render, agent, validate) but makes
  NO GitHub writes and NO state mutation — prints what would happen.

## 9. Daemon + IPC (D4/D5, A4, A15, A16)

- `prx daemon start` → spawn detached background process (or `--foreground`); pid file guard =
  single instance (A16); `stop` = SIGTERM via pid; `status` = pid alive check; `logs` = tail log file.
- Scheduler: per-repo interval + ±10% jitter; global work queue, FIFO oldest-due-first, max
  `max_concurrent_reviews` (D6); reviews run via `tokio::process` with timeout+kill.
- IPC: axum on 127.0.0.1 (ephemeral port → `daemon.port`), token in `daemon.token` (0600),
  `Authorization: Bearer` required. Endpoints:
  `GET /status`, `POST /review {key:"o/r#n"|"all", force:bool}` (A22: force bypasses cooldown,
  `--all`/non-force respects cooldowns), `POST /approve {key}`, `POST /snooze {key, hours?|until_push?}`,
  `POST /ignore {key}`, `POST /unignore {key}`, `POST /review-now`, `GET /history?repo=`, `GET /insights`.
- CLI commands that mutate (`review/approve/snooze/ignore/unignore/review-now`) act as thin clients
  when the daemon is up (A4: ALL state + GitHub writes flow through the daemon); when it's down they
  execute directly (best-effort, safe for `status` which just reads).
- Notifications on state transitions with the tier mapping; every notification event is
  configurable/disableable (A10).

## 10. Insights (§3.7, A18)

From history.jsonl: reviews/day/week, avg review→merge, per-author patterns, per-repo patterns,
effectiveness (% of changes_requested reviews whose blocking issues were later fixed — i.e. a later
review on same PR approved). V1 keeps raw finding text (no categorization).

## 11. V1 cut line (plan §2)

IN: spec §1–7 + max-concurrent-reviews, diff-size cap, retention compaction (90d on daemon start),
`prx doctor`, dry-run, rotating logs, `daemon logs`.
OUT: cron schedules, active hours, priority ordering, review depth/delta, comment-style options,
file-path filters, exports/digests, web UI, CI integration, team features.
Menu bar shell (Phase 4 of the TS plan) is a **separate GUI deliverable, out of scope for the Rust
core** — the IPC API is its backend. Document this in the README.

## 12. Acceptance per phase

- P1: `prx init/doctor/repos add|list|remove/status` work; status shows real lifecycle states using
  fake-gh fixtures; transition table unit tests cover every edge in §6 (100% branch intent).
- P2: full review pipeline end-to-end under fake gh + fake agent: manifest → inline comments →
  review body (correct flag), dry-run makes zero writes, agent timeout/invalid-manifest → retry →
  failure notification path, history.jsonl appended, state transitions.
- P3: daemon survives restart (no duplicate reviews thanks to last_reviewed_sha), CLI round-trips
  through IPC, single-instance guard works, notifications best-effort, logs rotate.
- P4: `prx history/insights` correct math on fixtures, 90-day compaction, README, clippy clean,
  full `cargo test` green.
