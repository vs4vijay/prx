# prx — Design Specification

**Version:** v0.3 (2026-10-07) — review control, OpenSpec change `prx-v1-1-review-control` (§4.6). Earlier: v0.2.1 (2026-10-04) — revises the deferred D4 "no KeepAlive" autostart decision (recorded with the v0.1 autostart change `complete-prx-v1`): launchd now supervises the daemon (`daemon start --foreground` + `KeepAlive { SuccessfulExit = false }`, §6) via OpenSpec change `prx-v1-1-daily-use-hardening` (v0.2, 2026-10-04: folded the audit resolutions A1–A24 (`design-audit.md`) and the Rust-adapted decisions from `DEV-PLAN.md` into the original draft (v0.1, 2026-08-20))
**Status:** Current
**Author:** Vijay

---

## 1. Overview

prx is a local-first PR review lifecycle manager. It watches configured GitHub repositories for open pull requests, delegates automated code reviews to a configurable coding agent, manages the review lifecycle with scheduling and cooldowns, provides at-a-glance status via a menu bar interface, sends proactive notifications, and tracks full review history with insights.

**Implementation shape (v0.2):** prx is a single Rust binary — CLI, lifecycle state machine, review pipeline, background daemon, and a localhost IPC API in one `prx` executable. This supersedes the TypeScript-core + Swift-menu-bar split contemplated in `implementation-plan.md` (decision D1 there); the menu bar is now a separate GUI client that consumes the IPC API (§6). The audit findings A1–A24 in `design-audit.md` are folded into this revision; the headline architecture rule is **"agent analyzes; prx writes"** (§3.2).

### 1.1 Goals

- Proactive, automated PR reviews without leaving the desktop
- Full visibility into PR review state from the menu bar
- Manual approval authority — AI informs, human decides
- Configurable per-repo review standards, agents, and schedules
- Historical data and pattern insights over time
- Zero cloud dependencies — local machine + `gh` CLI + coding agent

### 1.2 Non-Goals

- Auto-approval or auto-merge (human stays in the loop)
- Any GitHub write performed by the coding agent — it is read-only (§3.2)
- A GUI inside the core binary — the menu bar shell is a separate client over the IPC API
- CI monitoring independent of PR review
- Human comment tracking/threading
- Replacement for GitHub's web UI (complements it)

---

## 2. Core Concepts

### 2.1 PR Lifecycle States

| State | Icon | Meaning |
|-------|------|---------|
| Pending | ⏳ | PR waiting out a review cooldown — covers the first review and re-reviews (v0.2: the draft's `New`, `Cooling Down`, and `Re-review Pending` merged into one stored state, A5). "New" vs "cooling down" vs "re-review pending" is derived for display from `pending` + cooldown anchor + prior verdict. |
| In Review | 🔄 | Agent is actively reviewing |
| Reviewed (Approved) | ✅ | Agent found no blocking issues |
| Reviewed (Changes Requested) | ⚠️ | Agent found blocking issues |
| Skipped | 🚫 | Not reviewable right now, with a recorded reason (A6): draft, excluded by config, snoozed (temporary), or ignored (until un-ignored) |
| Merged | ✔️ | PR merged (terminal state, moves to history) |
| Closed | 🗑️ | PR closed without merge; a re-opened PR returns to Pending (A8) |

### 2.2 State Transitions

```
PR Detected (first poll) → Pending (skip rules checked first: draft / config → Skipped with reason)
Pending + first-review cooldown elapsed → In Review   (cooldown anchored to GitHub creation time, A5)
In Review + agent finishes → Reviewed (Approved | Changes Requested)
Reviewed + new commits detected → Pending (re-review cooldown anchored to commit-detection time, A5)
Pending (re-review) + re-review cooldown elapsed → In Review
New commits while In Review → the running review finishes, then a re-review is queued (A8)
PR merged/closed while In Review → review aborted, terminal state (A8)
PR re-opened after Closed → Pending again (A8)
Draft flipped to ready → Skipped(draft) → Pending (A8)
Reviewed → User Approves (from tool) → posts GitHub approval (the only approve path, A1)
Reviewed / Any → Merged (detected via poll) → moves to history
Reviewed / Any → Closed (detected via poll) → moves to history
Any state → User Snoozes → Skipped(snoozed), previous state kept
Any state → User Ignores → Skipped(ignored) (permanent until un-ignored, A6)
Skipped(snoozed) + snooze expires → returns to previous state
Skipped(snoozed) + user un-snoozes → returns to previous state (v0.2.1)
Skipped(snoozed, "until next push") + new commits → wakes immediately (A9)
PRs that piled up while the daemon was down enter Pending with a creation-time anchor, so pre-existing PRs review on the first due cycle (A5/A8)
```

### 2.3 Cooldowns

| Type | Default | Anchor | Purpose |
|------|---------|--------|---------|
| `before_first_review` | 60 min | PR creation time on GitHub (A5) | Give author time to push fixups before first review |
| `between_re_reviews` | 30 min | New-commit detection time (A5) | Avoid spamming during rapid push sessions |

Both configurable globally and per-repo. Anchoring the first cooldown to GitHub's creation timestamp means a PR that already exists when prx starts watching is reviewed on its first due cycle, not delayed by an arbitrary detection time.

---

## 3. Features

### 3.1 Repository Watching

- Watch multiple GitHub repositories simultaneously
- Poll for open PRs on a per-repo interval (default: every 15 minutes, ±10% jitter — A7; the draft's 2-hour default would have made the cooldowns pointless). A cron `schedule` can additionally gate when reviews enqueue (v1.2, §4.4); state polling stays on the interval.
- Detect new PRs, new commits on existing PRs (head-SHA comparison, which also handles force-pushes naturally — A23), and PR close/merge/re-open events
- Draft PRs are tracked and shown under Skipped (draft) rather than hidden (A11)
- Filter by author (exact match), label (case-insensitive exact match — A19), or branch glob pattern (configurable)
- Diff-size guard: PRs over the configured file/line caps are skipped with a "too large" note and re-checked on later polls (A14)
- Fork PRs work fine — the review is diff-only via `gh pr diff` / `gh pr view`; prx never checks out the code (A24)

### 3.2 Automated Review

Reviews follow one architecture rule — **"agent analyzes; prx writes"** (audit A1–A3):

- The coding agent (pi, claude, codex — configurable via the `agents` map, prompt on argv or stdin) is **read-only**: given the PR number + repo and a rendered review prompt, it fetches the diff and repo standards itself (`gh pr diff`, `gh pr view`, AGENTS.md, CONTRIBUTING.md, …) and writes a **JSON review manifest** to the path prx gives it. It never runs `gh pr review`, never posts to GitHub, never approves, cannot merge.
- **prx validates the manifest** (schema + verdict consistency: any blocking finding forces a changes-requested verdict) and treats it as the machine source of truth for the verdict, blocking/nit counts, menu, history, and insights (A3). Validation failure, agent crash, or timeout → retry once, then a clean failure with an `on_failure` notification.
- **prx posts to GitHub**: inline comments via the REST API through `gh api` (`gh pr review` cannot create line-anchored comments — A2; per-comment best-effort, failures collected), then **one review body** via `gh pr review` — `request changes` when blocking findings exist, otherwise a comment-only review — carrying the verdict summary and an "automated review by prx" attribution footer (A13). On the user's own PRs GitHub rejects request-changes verdict reviews, so prx degrades those to a comment-type review instead of failing.
- Approvals are **human-only** (A1): the agent never approves; `prx approve` / the menu Approve action is the single approve path in the system.

Supporting details:

- Default review prompt ships with prx — guidance-only text with placeholders; prx always appends its fixed output contract (read-only rule, untrusted-data rule, manifest path and schema) last (§4.3)
- Per-repo prompt override **replaces** the default guidance; a `{{PARENT_PROMPT}}` placeholder splices the default guidance back in (A12)
- Agent discovers repo-specific standards on its own (AGENTS.md, CONTRIBUTING.md, etc.)
- Written manifests are kept under the data dir (`manifests/`) as an audit trail

### 3.3 Review Intelligence

All verdict lines, counts, and history below derive from the validated review manifest (§3.2) — not from scraping the posted GitHub review.

**At-a-glance (visible in menu bar per PR):**
- PR title + author + repo
- Status icon (state from §2.1)
- One-line verdict: "3 nits, no blockers" or "2 blocking: missing tenant scope, no tests"
- Time since last review

**On drill-down (expanded view per PR):**
- Full list of review comments posted (blocking vs nit, with file/line)
- Which blocking issues are resolved vs still open (on re-review)
- Diff stats (files changed, additions, deletions)
- Verdict: "Good to merge" / "Needs attention" / "Blocking issues"
- Available actions (see §3.5)

### 3.4 Menu Bar Interface

The menu bar is a **separate GUI client** (macOS — A21) that consumes the daemon's localhost IPC API (§6); the core binary itself ships no GUI.

**Top-level display:**
- Icon + count of PRs needing attention — a colored, appearance-aware, multi-scale (1x/2x/3x) icon with distinct offline, stopped (review mode `off`), reviewing (reviews in flight), attention (count), and idle states (v1.2)
- Dropdown shows all tracked PRs, ordered by the global `menu.sort` — `created_desc` (default, newest PR first), `created_asc`, `last_review_desc` (never-reviewed last), or `key` (repo, then PR number numerically), ties falling back to `key` — and grouped by `menu.group_by`: `lifecycle` (default; the state groups), `repo` (one group per repo, by name), or `none` (a single "Pull requests (N)" list). The inline Approve row follows every approve-ready PR in all modes; a saved change applies on the next refresh (§4.7). Each row shows key, author, and PR age (`2h`, `3d`, `5w`) before title and summary. Every group paginates 10 rows at a time with working "Show more" / "Show less" items, resetting to the first page whenever the group's PR set changes (v1.2 — supersedes A20's static collapse)
- Review-mode selector: an inline radio group *Off / Monitor / Review and comment* setting `defaults.review_mode` (§4.4); while `off` the headline becomes a "Reviews off" banner over the last-known state

**Per-PR submenu:**
- Open in GitHub (browser)
- Open review session (agent session for this PR)
- Review (now, bypassing cooldowns)
- Review with ▸ — one item per configured engine, the repo's own marked "(default)"; hidden when the daemon reports no engines (an older daemon)
- Review without posting (no GitHub writes for that run)
- Review with prompt… — a panel with the prompt, an engine picker, and a "Post to GitHub" checkbox; only options that differ from the defaults are sent (per-run overrides, never persisted — §4.7)
- Approve (posts your GitHub approval — the only approve path, §3.2)
- Block (request changes) / Unblock — Unblock replaces Block when prx's identity holds the PR's request-changes review (v1.2)
- Snooze (configurable duration: 1h, 2h, 4h, until next push)
- Unsnooze (returns a snoozed PR to its pre-snooze state — v0.2.1)
- Mark as ignored (skip permanently for this PR)
- Stop ignoring (reverses Ignore — A6)

**Status header** (top of the menu):
- The headline (attention count, pause countdown, or "Reviews off")
- When PRX last checked GitHub and when it checks next, in relative time; the daemon's version and pid behind ⌥
- "Check GitHub now" — trigger immediate review cycle (also a config reload check)
- History — the newest events, with their age

**Footer items** (in this order):
- Open PRX — opens the PRX window on its Dashboard (§4.5)
- Review PR… — a panel taking any target form (`org/repo#N`, a PR URL, `repo#N`, `N`, `#N` — §4.7) plus the per-run options; the daemon's error or note (e.g. ambiguous candidates) is shown verbatim
- Settings… — opens the same window on General (§4.5), not a text editor
- Pause reviews ▸ — durations and Resume now
- Quit PRX (⌥: Quit PRX (keep reviewing))

Approve and Block ask for confirmation before they post to GitHub.

### 3.5 Quick Actions

Actions available from the menu bar per PR:

| Action | Effect |
|--------|--------|
| Approve | Posts your GitHub approval via `gh pr review` — the only approve path in the system (A1) |
| Review | Queues an immediate review regardless of cooldown (and of any `schedule`) |
| Review with ▸ engine | Same, run by the chosen engine for that run only (a different engine uses its own default model and a fresh session) |
| Review without posting | Same, with zero GitHub writes for that run (like `monitor`); verdict, state, and history still update, history marked not posted |
| Review with prompt… | Same, with one-off instructions appended to that run's prompt, optionally combined with an engine and no posting (never persisted) |
| Review PR… (footer) | Review any PR by target form, tracking an untracked open PR on demand (§4.7) |
| Block | Posts a request-changes review with the effective `block_message` as prx's identity; a tracked PR moves to Reviewed (changes requested). Human action — not gated by review mode |
| Unblock | Dismisses prx's latest request-changes review and returns the PR to Pending (cooldown anchored at the unblock); nothing to dismiss is a clean no-op note |
| Snooze | Moves to Skipped state for configured duration (or until the next push) |
| Unsnooze | Returns a snoozed PR to its pre-snooze state, reviewed state intact (v0.2.1) |
| Ignore | Permanently skip this PR (until manually un-ignored) |
| Stop ignoring | Returns an ignored PR to Pending (A6) |
| Open in GitHub | Opens PR URL in the configured `browser` (system default unless set — §4.4) |
| Open review session | Opens the agent session for this PR (agent-specific: pi uses `--session`, others may open a log file or browser link) |

### 3.6 Notifications

**Notification tiers:**

| Tier | Default behavior |
|------|-----------------|
| Urgent | Banner + sound |
| Informational | Banner, no sound |
| Silent | Menu bar update only, no banner |

**Default event → tier mapping (all configurable):**

| Event | Default tier |
|-------|-------------|
| Blocking issues found | Urgent |
| Script/agent failure | Urgent |
| Review completed (approved) | Informational |
| New PR detected | Informational |
| New commits detected on a tracked PR | Informational (A10) |
| Re-review triggered | Informational |
| PR merged/closed | Silent |
| PR skipped (draft/cooldown) | Silent |
| No new PRs found | Silent |

Users can reassign any event to any tier, or disable specific events entirely. When the menu bar shell is running it banners from `/history` itself, and the daemon suppresses its own duplicate banners while a menubar has been seen recently (automatic, via client self-identification — v0.2.1); the `banner: false` tier setting remains the manual override. Run as PRX.app, the shell posts those banners through the macOS notification center, so the sender is "PRX" and clicking a banner opens the PR in the repo's browser; if the user turns PRX notifications off in System Settings, no banner is shown by either process. A bare binary, or a bundle macOS will not authorize, falls back to `terminal-notifier`/`osascript`.

### 3.7 Review History & Insights

**Data tracked per review (append-only JSONL; one line per review / failure / skip / merge / close / new-commits (v0.2.1) — corrupt lines are skipped on load, and history older than 90 days is compacted on daemon start, A17):**
- Timestamp
- Repo + PR number + PR title + author
- Agent used
- Review verdict (approved / changes-requested / comment-only)
- Number of blocking comments
- Number of nit comments
- Raw blocking finding texts (so insights can surface recurring blockers — A18)
- Time taken
- Whether blocking issues were later resolved
- PR outcome (merged / closed / still open)
- Engine used, whether the review was posted (`posted: false` for no-write runs), and token usage when the engine reports it (`usage` for the review, `summary_usage` for the run that wrote the PR's brief — input/output/cache tokens, cost, model, session; omitted when unknown, never zeros — §4.7)

**Insights surfaced:**
- Reviews performed (daily / weekly / monthly)
- Average time from review to merge
- Most common blocking issues (raw texts in v0.2; categorization deferred — A18)
- Per-author patterns ("author X's PRs always pass", "author Y frequently misses tests")
- Per-repo patterns ("repo A has 80% ACL-related blocks")
- Review effectiveness (% of changes-requested reviews followed by an approved review on the same PR)
- Token usage and reported cost per day / week / month, split by engine, repo, PR, or session (`prx usage`, `GET /usage`, the window's Usage & stats page — §4.7)

### 3.8 CLI Interface

```bash
prx init                        # initial setup, creates config + data dirs (the prompt is built in)
prx doctor                      # self-check: config, gh binary + auth, agents, prompts, data dir
prx repos add <org/repo>        # add a repo to watch
prx repos list                  # show configured repos
prx repos remove <org/repo>     # stop watching
prx status [org/repo] [--json]  # current state of watched PRs
prx review <target>             # review one PR now — explicit target bypasses cooldown (A22)
                                #   target: org/repo#N | PR URL | repo#N | N | #N (§4.7)
prx review <target> [--engine NAME] [--no-post] [--prompt TEXT]
                                # per-run options: another engine, no GitHub writes, extra instructions
prx review --all                # review all currently-due PRs (respects cooldowns; no per-run options)
prx review <target> --dry-run   # full pipeline, zero GitHub writes, zero state changes
prx approve <org/repo>#<number> # post approval for specific PR (the human-only path)
prx snooze <target> --hours N | --until-push
prx ignore <target>             # skip permanently until un-ignored (A6)
prx unignore <target>
prx review-now                  # ask the daemon to poll + enqueue what's due now
prx history [--repo <org/repo>] [--limit N] [--json]
prx insights [--repo <org/repo>] [--days N]
prx usage [--by day|week|month] [--window 7d|30d|90d|all] [--group none|engine|repo|pr|session] [--repo <org/repo>] [--json]
                                # token usage + reported cost from history (daemon-first, local fallback)
prx config                      # show the fully-merged effective config
prx config edit                 # open config in $VISUAL (or $EDITOR)
prx config set <key> <value>    # set a dotted config key, validated before saving
prx daemon start [--foreground] # start background daemon (single instance, A16)
prx daemon stop                 # stop background daemon
prx daemon status               # check if daemon is running
prx daemon logs                 # tail the newest daemon log (A15)
prx daemon install [--include-menubar] [--binary-path <path>]   # register login autostart (supervised — §6, v0.2.1)
prx daemon uninstall            # remove the login-autostart registrations (daemon + menubar)
```

Mutating commands go through the daemon's IPC API when it is up (thin clients — A4) and fall back to best-effort in-process execution with a warning when it is down. `prx review` passes its target to the daemon unparsed; `--dry-run` (always local) and the in-process fallback accept only `org/repo#N` and PR URLs, with a hint. `prx usage` reads `GET /usage` when the daemon is up and aggregates `history.jsonl` locally otherwise, with the same function; `--json` prints the `/usage` shape.

---

## 4. Configuration

### 4.1 Config File Location

`~/.config/prx/config.yml` (default, configurable via `PRX_CONFIG` env var). The data directory (state, history, logs, manifests — §5) defaults to `~/.local/share/prx` and is configurable via `PRX_DATA_DIR`.

### 4.2 Structure

```yaml
# Global defaults — apply to all repos unless overridden
defaults:
  agent: pi                            # key into the agents map (built-ins: pi, claude, codex)
  poll_interval_minutes: 15            # per-repo poll cadence; ±10% jitter added (A7)
  cooldown:
    before_first_review_minutes: 60    # anchored to GitHub PR creation time (A5)
    between_re_reviews_minutes: 30     # anchored to new-commit detection time (A5)
  review_mode: review comment          # off | monitor | review comment (§4.4)
  # schedule: "0 */2 * * *"            # cron gating review enqueue, local time (§4.4); unset = interval cadence
  review_prompt: default               # "default" (built in) or the prompt TEXT itself; override replaces (A12)
  # extra_prompt: |                    # appended to every review prompt (§4.3)
  #   Pay special attention to database migrations.
  max_concurrent_reviews: 1            # global FIFO work-queue cap
  diff_max_files: 50                   # diff-size caps (A14): over → skipped with a "too large" note
  diff_max_lines: 4000
  agent_timeout_seconds: 600           # wall-clock budget per agent run
  skip:
    drafts: true                       # drafts tracked under Skipped(draft), not hidden (A11)
    authors: []                        # exact author logins to skip
    labels: ["wip", "do-not-review"]   # case-insensitive exact match (A19)
    branches: []                       # glob patterns on the head branch
  notifications:                       # these keys ARE the event list (A10)
    on_new_pr: informational
    on_review_complete_approved: informational
    on_review_complete_blocking: urgent
    on_new_commits: informational
    on_re_review_triggered: informational
    on_merged: silent
    on_closed: silent
    on_failure: urgent
    on_skipped: silent
  browser: system                      # system | macOS app name/path | argv list with {{URL}}
  identity: {}                         # login / attribution_footer / credential (env var NAME) — §4.4
  block_message: "Changes requested — blocked from prx pending a human follow-up."

# Notification tier behavior
notification_tiers:
  urgent:        { banner: true,  sound: Basso }
  informational: { banner: true,  sound: null }
  silent:        { banner: false, sound: null }

# Optional agent overrides; built-ins exist for pi, claude, codex.
# A bare "-" arg means the rendered prompt is piped on stdin; otherwise
# {{PROMPT}} is substituted into the argv.
agents:
  pi:
    command: pi
    args: ["--yes", "-p", "{{PROMPT}}"]

# Menu bar ordering and grouping — global only (rejected under orgs/repos; §3.4, §4.7)
menu:
  sort: created_desc                   # created_desc | created_asc | last_review_desc | key
  group_by: lifecycle                  # lifecycle | repo | none

# Per-org overrides: apply to every watched repo named <org>/... (§4.4)
orgs:
  - org: some-org
    review_mode: monitor

# Per-repo overrides (only specify what differs from defaults ← org)
repos:
  - name: acme/api
    schedule: "0 */2 * * *"
    review_prompt: |
      {{PARENT_PROMPT}}

      Also check every tenant-scoped query for a tenant filter.

  - name: some-org/other-repo
    agent: claude
    poll_interval_minutes: 30
    skip:
      authors: ["dependabot[bot]"]
```

Config hygiene (A19): path values expand `~`; label matching is case-insensitive and exact; skip authors match exactly. Unknown keys are preserved across rewrites.

### 4.3 Review Prompt Template

`review_prompt` is the review **guidance** text (v1.2): unset or `default` uses the guidance built into the binary — no prompt file exists on disk. A value that looks like a legacy file path (single line, no `{{` placeholders, `.md`/`.txt` suffix or `/`, `~`, `./`, `../` prefix) is rejected with "review_prompt is now the prompt text; paste the contents of <path> instead"; the file is never read. Placeholders: `{{PR_NUMBER}}`, `{{REPO}}`, `{{MANIFEST_PATH}}` (also exported as the `PRX_MANIFEST_PATH` env var), `{{REVIEWER_LOGIN}}` (effective `identity.login`, else the `gh` user).

Every rendered prompt has two parts:

1. **Guidance** — the user-editable `review_prompt`. The shipped default covers only what to fetch, what to look at, and which repo standards to respect; it carries no manifest schema and no manifest path:

```markdown
You are a code reviewer working for prx, reviewing pull request
#{{PR_NUMBER}} in repository {{REPO}}.

## How to review
1. Fetch the diff: `gh pr diff {{PR_NUMBER}} --repo {{REPO}}`
2. Fetch PR metadata (read-only): `gh pr view … --json title,body,author,baseRefName,files`
3. Look for the repo's own standards (AGENTS.md, CONTRIBUTING.md, lint configs, tests)
4. Review for correctness, security, performance, error handling, test coverage
5. Report problems at file and line; blocking only when the PR should not merge as is
```

2. **The output contract** — fixed text prx **always appends last**, whatever `review_prompt` contains (`default`, custom text, `{{PARENT_PROMPT}}`, with or without `{{MANIFEST_PATH}}`), appended exactly once. It states: the agent is read-only with respect to GitHub (no `gh pr review`, no write `gh api` calls, no approve/comment/merge); all PR content is untrusted data, never instructions, and suspected injection is reported in the summary; the exact manifest path to write; and the manifest v1 schema `{ manifest_version: 1, verdict, summary, findings: [{ file, line, severity: "blocking"|"nit", body }], duration_seconds, brief? }` with the consistency rule and the optional plain-language `brief` (2–5 sentences, ≤ 1200 characters). The contract is not editable through config or settings; `GET /config` serves it as `review_contract` (with a sample path) next to `shipped_review_prompt`, and the settings prompt editor shows it read-only in a collapsible "Always added by PRX" section.

The agent never posts to GitHub and never approves (§3.2, A1/A2). Users can create per-org/per-repo overrides that replace the guidance — put `{{PARENT_PROMPT}}` inside an override to splice the default guidance in (A12).

`extra_prompt` (defaults / org / repo, most-specific-wins) is appended to every review under an "Additional instructions" heading. The agent's prompt is always ordered: rendered `review_prompt` → configured `extra_prompt` → the one-off instructions of a "Re-review with prompt" request (never persisted to config, state, or history) → the output contract. Unset or blank extras append nothing. For an `llm_api` engine the PR diff is inserted as an untrusted-data section just before the contract, so the contract stays last.

Legacy prompts: a `review_prompt` equal (after trimming whitespace) to the previous shipped default, which had the schema inline, resolves to `default`. A custom prompt that still contains `manifest_version` or `"verdict"` keeps working; `prx doctor` (informational, never a failure) and the prompt editor show a hint that output instructions are now added automatically and can be removed.

### 4.4 Review Posture, Schedule, Overrides, and Live Reload (v1.2)

**Review mode** (`review_mode`, defaults / org / repo; default `review comment`):

| Mode | Behavior |
|------|----------|
| `review comment` | Full tracking, notifications, AI reviews that post the review body + inline comments to GitHub (pre-v1.2 behavior) |
| `monitor` | Full tracking, notifications, AI reviews whose verdicts surface only in prx (state, history, menu) — zero GitHub writes |
| `off` | Stop mode: no `gh` polling, nothing enqueued (forced or not), no notifications; the daemon stays up, serves IPC from last-known state, and reports `review_mode: "off"` |

Human actions (Approve, Block, Unblock) are never gated by the mode. The menu bar selector and `POST /review-mode` set `defaults.review_mode`, persisted and applied at once. If the config file is broken at that moment, the mode is applied to the running last-good config only (`persisted: false`, "not saved" note); the file is untouched, and once fixed its own `review_mode` wins.

**Schedule** (`schedule`, defaults / org / repo): a 5-field cron expression (`minute hour day-of-month month day-of-week`, Sunday = 0 or 7) or `@hourly`/`@daily`/`@weekly`/`@monthly`/`@yearly`, evaluated in local time and validated at load. It gates review *enqueueing* only — state polling continues on `poll_interval_minutes` — and cooldowns, skip rules, and review mode still decide what is due at each tick. A repo first seen with a schedule waits for its next tick. Explicit human requests are not deferred: a targeted `prx review` / menu Re-review runs immediately, and `prx review --all` enqueues what is due now; "Check GitHub now" only polls early. Unset everywhere = the interval cadence.

**Browser and identity** (defaults / org / repo): `browser` is `system`, a macOS app name/path (`open -a`), or an argv list containing `{{URL}}`, honored by every PR-opening action (menu Open, banner click-through). `identity.login` is the GitHub login prx acts as (default: the `gh`-authenticated user — used to find prx's own request-changes review on Unblock and exposed in `/status`); `identity.credential` names an environment variable whose token is passed to `gh` as `GH_TOKEN` for **every GitHub write** on that repo (review bodies, inline comments, approve, block, unblock) — reads keep gh's default account, and a missing variable fails that write with a clear error. `identity.attribution_footer` replaces the built-in attribution line at the end of posted review bodies (unset = `— automated review by prx`). Prompts may reference `{{REVIEWER_LOGIN}}` (the effective `identity.login`, else the `gh` user, resolved only when referenced). `block_message` is the request-changes body the Block action posts.

**Overrides:** `orgs:` entries (keyed by `org`) apply to every watched repo named `<org>/…`; every defaults key is overridable at org and repo level. Effective values resolve **defaults ← org ← repo**, most-specific-wins; partial structures (`cooldown`, `skip`, `notifications`, `identity`) merge field by field, lists replace. `notifications.*` overrides pick the tier for that repo's events — daemon notifications and menu bar banners alike.

**Live reload:** the daemon hashes the config file every second (and on Check GitHub now) and applies changes without a restart: added repos are polled at once; removed repos stop being tracked and their queued reviews are discarded (history kept); other values apply to the next poll or review (a running review keeps the config it started with). A file that fails to parse or validate leaves the daemon on its last-good config with `config_error` in `/status` until fixed. `max_concurrent_reviews` is the exception: the queue is sized at daemon start, so changing it needs a restart.

### 4.5 Settings App (v1.2)

The menu bar's Settings… opens a native macOS window (AppKit window + embedded webview, local UI); Launching PRX.app again (Finder, Spotlight, Dock) opens the same window in the running instance. One window at a time (pid-file guard; a second launch focuses the first). Pages cover the whole config — general, review (agent, prompts, limits, cooldowns, skip rules, block message), schedule, notifications + tiers, agents, repos & orgs (add/remove, per-org/per-repo overrides with "inherited from" display and Reset), identity & browser — plus a raw YAML view. The app reaches the daemon only through the token-authenticated loopback IPC (`GET`/`PUT /config`): the daemon validates every save in full (field errors shown inline, nothing written while invalid), writes atomically, and hot-applies. Form saves re-serialize the YAML (comments dropped); raw saves keep the text verbatim.

The same window is the **PRX window** (v1.3): its sidebar has an **Overview** group — *Dashboard* (headline counts, review mode and pause, attention list with each PR's brief card, recent activity, last 7 days' tokens and cost), *Pull requests* (sortable/filterable table, initial order `menu.sort`, row actions Open / Review / Review with / Review without posting / Review with prompt, a Review PR… box, and a per-PR detail with the brief card: full text, created and updated dates, a stale marker, and Regenerate with progress and an inline error), *History* (newest first with engine, tokens, and a not-posted marker; repo/event/engine filters; Load more via `GET /history?before=`; a detail view with the brief and review / brief usage), and *Usage & stats* (stacked-bar chart with bucket/window/group toggles, per-group totals, overall totals with a runs-without-usage callout, the insights panel; "update the daemon" when `/usage` is missing) — and a **Settings** group with the pages above plus *Menu bar* (`menu.sort`, `menu.group_by`). Overview pages refresh about every 10 s while visible and show an offline state when the daemon is down. Entry points: the menu footer's **Open PRX** (opens Dashboard) and **Settings…** / ⌘, (opens General); either focuses an already-open window on its page, and a settings draft survives page switches. New bridge methods: `status.get`, `history.list {repo?, limit?, before?}`, `usage.get {bucket?, window?, group?, repo?}` (a 404 maps to `unsupported`), `insights.get {days?, repo?}`, `review.request {target, extra_prompt?, engine?, post?}`, and `brief.request {key}` (`POST /brief`).

### 4.6 Review Control (v0.3)

OpenSpec change `prx-v1-1-review-control` (capabilities `prx/review-engines`, `prx/scheduling`, `prx/commentary`, `prx/executive-summary`, `prx/browser-open`, `prx/menu-bar-shell`). The README is the configuration reference; these are the design-level notes.

- **Engines** replace the agents map: `engines.<name>` with `type: agent` (command/args/`model_args`, optional `session_output` + `resume_args` with `{{SESSION_ID}}`) or `type: llm_api` (`provider` anthropic | openai | openai_compatible, `base_url`, `model`, `api_key_env`, `temperature`, `max_tokens`). `engine` resolves repo > org > defaults. Legacy `agents`/`agent` are load-time aliases; rewrites save the new form, which older binaries cannot read. For `llm_api`, prx fetches the diff, calls the API, and validates the reply as a manifest exactly like an agent's — the "agent analyzes; prx writes" rule (§3.2) is unchanged.
- **Same-session re-reviews:** resume-capable agent engines (built-ins `claude`, `codex`) keep `review_session` across new-commit transitions and resume it; a failed resume retries as a fresh session inside the same attempt. Other engines clear the session as before (§5.3).
- **Pause:** `prx pause --hours N | --until T` / `prx resume` (`POST /pause`, `POST /resume`) set `paused_until` in the state file (schema stays v1). Automatic enqueue requires *not paused* and `review_mode != off`; pause keeps polling, `off` does not; an explicit single-PR review bypasses the pause. Schedule ticks during a pause are consumed (§4.4).
- **Notifications:** new `on_review_started` event (default `informational`, plus a `review_started` history entry for the menu bar); global master switch `defaults.notifications.enabled` (defaults-only; org/repo values are rejected).
- **Commentary:** `comments.inline` and `comments.template` (closed placeholder set, validated at load; the default renders byte-identical to the pre-template body). Zero writes remains `review_mode: monitor`.
- **Brief** (formerly the executive summary; renamed by `prx-desktop-app-and-briefs`): a 2–5 sentence plain-language description of what the PR does and why, at most 1200 characters. It comes from the review manifest's optional `brief` at no extra run, else from a best-effort on-demand run on `brief.engine` (default: the review engine) after the review, or on request (`POST /brief`, `prx brief --regenerate`, the menu's Write / Regenerate brief, the window's Regenerate). Stored per PR with `created_at` (kept on regeneration), `updated_at`, the head SHA it describes (stale once the head moves), its source, and engine; recorded in history and served in `/status` with `brief_pending` / `brief_error`. A failure never fails the review and keeps the previous brief. Config `brief` (legacy `executive_summary` still read); `{{BRIEF}}` (alias `{{EXEC_SUMMARY}}`) renders this run's manifest brief when the agent wrote one, else the PR's stored brief (an on-demand brief is written after the posts).
- **Browser:** the resolver moves to core with a system fallback; `auto_open` (`off` | `approved` | `blocking` | `both`) opens the PR from the daemon on matching completed reviews, monitor mode included.
- **Menu bar:** 💬 comment-only icon, a *Ready for approval* group with inline Approve, brief drill-down (one line, age and stale marker, Copy / Regenerate / Write brief), pause submenu with countdown.
- **Review targeting (v1.3):** a single-PR target — CLI, `POST /review`, menu, window — is any of `org/repo#N`, a GitHub PR URL (`https://github.com/org/repo/pull/N`, trailing path/query/fragment ignored), `repo#N` (a watched repo's short name), `N`, or `#N`; malformed input is rejected listing the forms. Resolution is against **watched repos only**: `org/repo` must be watched (else 404); `repo#N` must match exactly one watched repo's name (none → 404, several → 409 listing the candidates); a bare number checks every watched repo whose mode is not `off` — a tracked non-terminal key matches without a gh call, the rest are looked up concurrently and only open PRs match (none → 404, several → 409 with candidates). **On-demand tracking:** a resolved PR that prx does not track is fetched from GitHub outside the state lock and must be open (merged/closed → 409 naming the state and it stays untracked; missing → 404; gh failure → 502); it is then tracked through the machine's `PollNewPr` exactly as a poll would (skip rules, notifications, publish) and continues to the normal eligibility check and enqueue — an untracked draft becomes tracked-as-skipped and is reported not eligible. A repo in mode `off` enqueues nothing. Short forms need the daemon: `--dry-run` and the in-process fallback accept only `org/repo#N` and URLs.

### 4.7 Review Options, Menu Layout, and Token Usage (v1.3)

OpenSpec change `review-options-and-menu-layout` (capabilities `review-run-options`, `review-targeting`, `menu-ordering`, `token-usage`, `app-window`). The README is the reference; these are the design-level notes.

- **Per-run overrides:** `ReviewRequest` gains `engine?` and `post?` beside `extra_prompt?` (`prx review --engine/--no-post/--prompt`, the menu's Review with ▸ / Review without posting / Review with prompt…, the window's row actions). Never persisted. An unknown engine is a 400 listing the engines. A different engine runs with its own default model (no `agent_model`) as a fresh session — it neither resumes nor stores `review_session`, so the retained session survives for the next default run — and is recorded as the history `agent`; the brief keeps the repo's brief engine. `post: false` runs as `monitor` (zero GitHub writes; verdict, state, history, and `posted: false` still recorded); `post: true` never escalates past the repo's `review_mode`. Queue coalescing: a request carrying overrides replaces a queued job's options, a plain one leaves them; an in-flight PR gets an "already running" note.
- **Menu layout:** global `menu: {sort, group_by}` (defaults `created_desc`, `lifecycle`; invalid values list the allowed ones; rejected at org/repo level), served in `GET /status` as `menu`, together with `engines` (sorted resolved names) and `StatusEntry.engine` (all `serde(default)`, so old payloads still parse).
- **Token usage:** history `ReviewCompleted`/`ReviewFailed` carry `usage`, `ReviewCompleted` carries `summary_usage` — `TokenUsage {input_tokens?, output_tokens?, cache_read_tokens?, cache_write_tokens?, cost_usd?, model?, session_id?}`, every field omitted when unknown. Built-in capture for `claude` (JSON result `usage` + `total_cost_usd`), `codex` (`turn.completed` usage summed), and `llm_api` (Anthropic and OpenAI response usage); any agent can map fields with `usage_output` (dotted JSON paths into its stdout; overrides the built-in parsing). `input_tokens` is always **uncached** input: engines reporting cached tokens as a subset of input (codex `cached_input_tokens`, OpenAI `prompt_tokens_details.cached_tokens`) are normalized at capture to `input_tokens = reported − cached`, `cache_read_tokens = cached`, so summing input + output + cache fields never double-counts. Malformed usage is unknown and never fails a review; failed and retried runs record what their attempts reported.
- **Aggregation:** `GET /usage?bucket=day|week|month&window=7d|30d|90d|all&group=none|engine|repo|pr|session&repo=` (invalid → 400) and `prx usage` share one function: local-date buckets (ISO weeks), every bucket in the window present, session key `engine:session_id`; `Totals.runs` counts runs **with** usage, `runs_without_usage` (top level) counts the rest, groups appear only when they have usage, and cost sums reported costs only (`runs_with_cost`).
- **History paging:** `GET /history?before=<RFC 3339>` returns entries strictly older than the timestamp (invalid → 400).

---

## 5. Data Storage

### 5.1 State File

`~/.local/share/prx/state.json` (atomic temp+rename writes; schema versioned)

Current state of all tracked PRs, keyed by `owner/repo#<number>`:
- PR number, repo, title, author, head branch, head SHA, draft flag
- GitHub creation timestamp — the first-review cooldown anchor (A5)
- Current lifecycle state, plus `previous_state` (restored on snooze expiry) and `skip_reason` (draft / config / snoozed / ignored — A6)
- Cooldown anchor: GitHub creation time for first reviews, commit-detection time for re-reviews (A5)
- Last reviewed commit SHA
- Last review timestamp
- Last review verdict + summary, blocking/nit counts
- Snooze expiry / until-push flag (A9)
- Ignored flag, agent session reference, review attempt count, diff-too-large flag

### 5.2 History File

`~/.local/share/prx/history.jsonl`

Append-only log — one JSON object per review / failure / skip / merge / close / new-commits (v0.2.1). Verified on load: a corrupt line is skipped, not fatal. History older than 90 days is compacted (atomically) on daemon start (A17).

### 5.3 Agent Sessions & Manifests

Agent sessions are managed by the coding agent itself (e.g., pi's `--session-id` mechanism). prx tracks the session identifier per PR so it can resume/reference. The agent-written review manifests are kept under `~/.local/share/prx/manifests/` as an audit trail — the agent's artifact is the source of truth (A3).

---

## 6. Background Daemon

- Runs as a single-instance background process (pid-file guard makes `daemon start` idempotent — A16)
- Login autostart via `prx daemon install` is **supervised** (v0.2.1): the macOS launchd plist runs `daemon start --foreground` with `KeepAlive { SuccessfulExit = false }` — launchd relaunches a crashed daemon and stands down on a clean exit (`daemon stop` exits 0 after its SIGTERM shutdown), matching Linux systemd's `Restart=on-failure`. `--include-menubar` also registers a `com.prx.menubar` agent (RunAtLoad, same KeepAlive; macOS-only — the flag warns and skips on Linux), and the registered binary resolves stable-first (`--binary-path` → `~/.cargo/bin/prx` when building from `target/` → the running binary, warned when inside `target/`). This revises the deferred D4 decision ("no KeepAlive") from the v0.1 autostart work (`complete-prx-v1`) — OpenSpec change `prx-v1-1-daily-use-hardening`
- Polls repos on their configured intervals (±10% jitter); review enqueueing follows a repo's `schedule` when set, and repos in review mode `off` are not polled (§4.4)
- Hot-reloads `config.yml` (1 s hash poll, last-good fallback — §4.4)
- Manages cooldown timers and a global FIFO work queue (oldest-due-first, at most `max_concurrent_reviews` reviews running)
- Triggers agent reviews when conditions are met
- Updates state file and history; an in-progress review is applied in memory only until it completes or fails, so a crash can never strand a PR in In Review
- Sends best-effort notifications on state transitions
- Serves a localhost IPC API (127.0.0.1, ephemeral port recorded in `daemon.port`, bearer token in `daemon.token`, mode 0600) — the backend for the CLI thin client and the menu bar shell. **All** state writes and GitHub writes flow through the daemon (A4); CLI commands mutate through it when it is up and fall back to best-effort in-process execution when it is down.
- Writes daily rotating logs (`~/.local/share/prx/logs/`); `prx daemon logs` tails the newest (A15)
- Compacts history older than 90 days on start (A17)

---

## 7. Sensible Defaults

| Setting | Default | Rationale |
|---------|---------|-----------|
| Agent | `pi` | Most common local agent |
| Poll interval | Every 15 min ± jitter | Frequent enough for cooldowns to matter between polls (A7) |
| First review cooldown | 60 min from PR creation | Author's "push fixup" window (A5) |
| Re-review cooldown | 30 min from commit detection | Avoids review spam during rapid iteration (A5) |
| Skip drafts | Yes (tracked under Skipped) | Drafts aren't ready for review, but cheap visibility (A11) |
| Max concurrent reviews | 1 | Keeps agent load and GitHub noise low |
| Diff-size cap | 50 files / 4000 lines | Agents have real context limits (A14) |
| Blocking issues notification | Urgent (banner + sound) | Needs human attention |
| Review approved notification | Informational (banner, no sound) | Good to know, not urgent |
| PR merged/closed | Silent | Housekeeping, menu bar update only |
| Agent failure | Urgent | Something broke, needs fixing |

---

## 8. Advanced Options

Several items from the original draft moved into V1 during the audit: max concurrent reviews and the diff-size cap (§3.1/§4.2), rotating logs (§6, A15), 90-day retention compaction (§5.2, A17), plus `prx doctor` and `--dry-run`. What remains advanced/deferred:

### 8.1 Review Scheduling (deferred)

- ~~Cron expressions~~ — shipped in v1.2 as `schedule` (§4.4); an active-hours window can be expressed as a cron range (e.g. `0 9-18 * * MON-FRI`)
- "Active hours" window — only review during 9am-6pm (don't wake up the author at 3am with review comments)
- Priority ordering beyond the V1 oldest-due-first queue

### 8.2 Review Behavior (deferred)

- Review depth — full review vs quick scan / delta-only (V1 always reviews the full diff)
- ~~Comment style~~ — shipped in v0.3 as `comments.inline` + `comments.template` (§4.6)
- Approval authority — fixed by design (A1): the agent never approves and prx's only approve path is the human's explicit action; an explicit `agent_may_approve` opt-in may be considered later
- Max review attempts: V1 retries a failed run once, then fails cleanly (§3.2)

### 8.3 Filtering (deferred)

- File path filters — only review PRs that touch certain paths
- (The size threshold moved into V1 as the hard diff-size cap, §4.2 — A14)

### 8.4 Insights Configuration (deferred)

- Export format (JSON, CSV)
- Digest frequency — "send me a weekly summary email/notification of review stats"
- (The 90-day retention period moved into V1 with compaction mechanics, §5.2 — A17)

---

## 9. Security Considerations

- All actions performed via user's authenticated `gh` CLI session
- No tokens stored by prx itself — delegates auth to `gh`
- The agent runs read-only (§3.2): it cannot approve, merge, or post — which also shrinks the prompt-injection blast radius (A13)
- PR content is treated as data, never as instructions, and every posted review body carries an attribution footer ("automated review by prx" by default, or the configured `identity.attribution_footer`) so teammates see it as automated (A13). Residual risk — an agent acting on injected content during analysis — is documented; optional hardening: a scoped fine-grained PAT for prx's writes instead of the full user session.
- The only network prx itself opens is the localhost IPC listener (127.0.0.1, bearer-token auth); all GitHub traffic goes through `gh`. Exception (v0.3, opt-in): a configured `llm_api` engine is called at its `base_url` only (reqwest + rustls), with the key read from the environment variable named by `api_key_env` — never stored or logged
- Prompts live inline in the config (no prompt files are read); legacy path values are rejected at validation (§4.3)
- Multi-account credentials are referenced by environment-variable name (`identity.credential`), never stored as token values; the settings app never handles credentials
- Review history may contain PR titles/authors — stored locally only, no telemetry

---

## 10. Future Considerations (Not in V1)

- PR / history / insights pages inside the settings app window (the native settings app itself shipped in v1.2, §4.5)
- CI status integration
- Human comment tracking and thread resolution detection
- Team-wide insights (aggregate across multiple users)
- Slack/Discord webhook notifications
- Auto-merge when conditions are met (CI green + clean AI verdict + human approved — approvals stay human-only, §3.2)
- PR creation assistance (not just review)
- Integration with project management tools (Plane, Linear, Jira)

---

## 11. Success Criteria

- Reviewer (user) sees all open PRs and their review state from menu bar without opening GitHub
- Reviews are posted automatically within configured cooldown windows
- User can approve a PR from the menu bar in one click after seeing AI verdict
- Notifications fire reliably for configured events
- Historical data enables identifying patterns after 2+ weeks of usage
- Adding a new repo takes one CLI command and inherits all defaults
- System is self-healing — recovers from agent failures, network issues, auth expiry with clear notifications
