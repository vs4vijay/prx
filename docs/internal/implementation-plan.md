# prx — Implementation Plan

**Date:** 2026-09-03
**Prerequisite:** resolve the blockers in `docs/internal/design-audit.md` (A1–A3) via the spec revision in Phase 0.

> **SUPERSEDED in part (2026-10-04):** decision **D1 (TypeScript core + Swift menu bar shell)** was superseded by the **Rust implementation** — a single Rust crate, one `prx` binary containing the CLI, state machine, review pipeline, daemon, and localhost IPC API, with the menu bar as a separate GUI client over that API. `DEV-PLAN.md` is the binding adaptation of this plan; the shipped code (`src/`, `README.md`) is ground truth. Audit resolutions A1–A24 and decisions D2–D6 carry over unchanged.

---

## 1. Key decisions (with recommendations)

| # | Decision | Recommendation | Alternative |
|---|----------|----------------|-------------|
| D1 | Stack | **TypeScript core** (daemon + CLI) + **thin Swift menu bar shell**. The hard, iteration-heavy logic (state machine, scheduling, agent adapters, insights) lives in TS; the Swift shell only renders the menu and calls the daemon API. | Single native Swift app (most polished menu bar/notifications, slower core iteration); Rust + `tray-icon` (one binary, slower dev); Electron/Tauri (heavier than the problem needs) |
| D2 | Approvals | Agent never approves. Analyze-only agent; prx posts reviews (`request-changes` / `comment-only`); the human's menu action is the only approve path. | `agent_may_approve` per-repo opt-in (future) |
| D3 | Scheduling | V1: per-repo interval in minutes (default 15) + jitter. Cron expressions deferred. | cron parser in V2 |
| D4 | Review results | JSON manifest written by the agent is the machine source of truth; prx validates it and does all GitHub writes via `gh api` (inline comments via REST, review body via `gh pr review`). | agent posts directly + prx scrapes (rejected: fragile, A1/A2/A3) |
| D5 | IPC | Localhost TCP API on a port written to `~/.local/share/prx/daemon.port` + auth token file (0600). Simplest cross-language client (CLI + Swift `URLSession`). | Unix domain socket (cleaner, slightly fiddlier from Swift) |
| D6 | Review queue | Global FIFO work queue in the daemon, max 1 concurrent review (config, up to 2), oldest-due-first. | per-repo parallelism |

**Architecture after D2/D4/D5:**

```
gh CLI ──(read: pr list, pr view, pr diff)──▶ daemon (TS)
                                   │  scheduler + cooldowns + state machine + queue
pi/claude/codex ──(spawn, read-only, JSON manifest)──▶ daemon
daemon ──(write: gh api inline comments, gh pr review body)──▶ GitHub
daemon ──(localhost API)──▶ prx CLI ┘ and ─▶ Swift menu bar shell ──▶ UNUserNotificationCenter
state: state.json (atomic) · history.jsonl (append) · logs/ (rotating)
```

## 2. V1 cut line

In: everything in §1–§7 of the spec plus max-concurrent-reviews (D6), diff-size cap (A14), retention compaction (A17), `prx doctor`, dry-run mode, logging.
Out (advanced §8 / future §10): cron schedules, active hours, priority ordering, review depth/delta mode, comment-style options, file-path filters, export/digests, web UI, CI integration.

## 3. Phases

### Phase 0 — Spec revision (docs only) · S
Apply the audit: merge New/Cooling Down with creation-anchored cooldowns (A5), `skip_reason` + `previous_state` in state schema + Un-ignore action (A6), poll default 15 min (A7), define in-flight/edge transitions (A8), snooze-until-push (A9), align notification events (A10), draft visibility (A11), prompt inheritance contract (A12), rewrite §3.2/§4.3 around the manifest + analyze-only agent (A1–A3), add logging + doctor + force-review semantics (A15, A22), state "V1: macOS" (A21).
**Accept:** spec v0.2 with all blocker/high findings resolved; audit doc linked.

### Phase 1 — Core + CLI skeleton (no daemon, no agent) · M
TS project scaffold; config load/validate/merge (defaults ← per-repo, `~` expansion, A19); state store with atomic writes (temp+rename); GitHub layer over `gh` JSON output (`pr list -R o/r --json …`, `pr view`, `pr diff`); state machine implemented as a pure transition table (incl. A8/A9 edges) with unit tests; `prx init`, `prx doctor` (gh auth, agent binaries on PATH, config + prompt files readable), `prx repos add/list/remove`, `prx status [repo]`.
**Accept:** `prx status` shows real open PRs with correct lifecycle states across two configured repos; transition table reaches 100% branch coverage.

### Phase 2 — Review pipeline · M–L
Agent adapter interface: `spawn(cmd, workdir) → manifest`, timeout + kill, per-agent resume-ref for "open session", first adapter = `pi`, stub for `claude`; manifest JSON-schema validation; **dry-run mode** (`prx review o/r#N --dry-run` — full pipeline, no GitHub writes); posting path: inline comments via `gh api` REST (`POST /repos/{o}/{r}/pulls/{n}/comments`, `commit_id`/`path`/`line`), then `gh pr review -r/-c` with verdict body + "automated review by prx" footer (A13); diff-size cap; failure/timeout → `on_failure` notification + retry with backoff; append `history.jsonl`; verdict → state transition; `prx review <repo>#<n>` (force) and `prx review --all` (cooldown-respecting) semantics.
**Accept:** a real PR gets inline blocking/nit comments + a request-changes review end-to-end; `--dry-run` proves the identical pipeline without writing; agent hang/timeout recovers and notifies.

### Phase 3 — Daemon, scheduler, IPC, notifications · M
Launchd agent (plist in `~/Library/LaunchAgents`, RunAtLoad) via `prx daemon install/start/stop/status`; interval scheduler + jitter; cooldowns from creation/re-commit anchors; single-instance guard; localhost API (D5): `GET /status`, `POST /review`, `POST /approve`, `POST /snooze`, `POST /ignore|unignore`, `POST /review-now`; CLI becomes a thin client over the API; notifications v1 via `osascript` (banner) + `afplay` (sound) with the tier mapping.
**Accept:** PRs reviewed on schedule unattended across daemon restarts (state survives, no duplicate reviews thanks to last-reviewed-SHA); all CLI commands work against the running daemon.

### Phase 4 — Menu bar shell (Swift) · M
SwiftUI menu bar extra: icon + needs-attention count; dropdown grouped by state (A20 collapsing); per-PR submenu — Open in GitHub, Open review session, Re-review, Approve (`gh pr review --approve` through the daemon API, handles "already approved" gracefully), Snooze (1h/2h/4h/until-push), Ignore/Un-ignore; footer — last poll, Review now, Settings (`prx config edit`), History; native `UNUserNotificationCenter` with tier→banner/sound mapping; login-item via SMAppService.
**Accept:** full §3.4 experience; approve/snooze/ignore round-trip through the daemon and reflect in state; notifications honor tiers.

### Phase 5 — Insights, retention, hardening → v0.1 · M
`prx history [--repo]`, `prx insights` (counts, avg review→merge, per-author/repo patterns, effectiveness; raw-text issue list — categorization deferred per A18); 90-day compaction on daemon start; chaos tests: agent crash mid-review, network loss, `gh` auth expiry (notification + recovery), daemon killed during review; README + `prx init` walkthrough; tag `v0.1`.
**Accept:** §11 success criteria demonstrably met (2-week history exercise for the patterns criterion may trail).

## 4. Testing strategy

- State machine: pure table-driven unit tests (every §2.2 edge incl. A8/A9).
- Agent adapter: fake adapter with canned manifests; golden-file tests for manifest validation.
- GitHub layer: recorded `gh` JSON fixtures; posting path behind dry-run.
- End-to-end: one throwaway repo; dry-run in CI, one live smoke review per release.
- Daemon: restart/kill tests asserting state-file integrity.

## 5. Risks

| Risk | Mitigation |
|------|-----------|
| Agent output variance breaks the manifest contract | Strict schema + validation failure → retry once, then `on_failure`; contract is versioned (`manifest_version: 1`) |
| Prompt injection via PR content | Analyze-only agent (D2), PR text treated as data, review footer, documented residual risk (A13) |
| macOS notification reliability without a signed app bundle | UNUserNotificationCenter from the Swift shell; `osascript` fallback in Phase 3 |
| Scope creep (§8 advanced options) | V1 cut line in §2; spec marks the rest as post-V1 |
