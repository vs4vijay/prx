# prx — Next Steps Plan

**Date:** 2026-09-13
**Status:** Active
**Prerequisite:** Rust core v0.1 complete (Phases 1–4 implemented, committed through `413a55e`; 207 tests, clippy/fmt clean; see `DEV-PLAN.md` for the design contract and `README.md` for usage).

---

## 0. Current state snapshot

**Update 2026-10-04:** P5–P8 below are done via OpenSpec change `complete-prx-v1` (`openspec/changes/complete-prx-v1`) — live validation, CI + `v0.1` tag, the menu bar shell, and login autostart. The pre-P5 snapshot and tradeoffs below are kept as the v0.1 baseline; P9 stays future.

**Update 2026-10-04 (v1.1 daily-use hardening):** the five daily-use rough edges — menubar autostart, the new-commits history/banner gap, no un-snooze, duplicate daemon + menubar banners, and an unstable unsupervised `target/` autostart registration — are all addressed via OpenSpec change `prx-v1-1-daily-use-hardening` (lives under `openspec/changes/` in the parent `projects` repo): supervised daemon autostart (`daemon start --foreground` + launchd `KeepAlive { SuccessfulExit = false }`, revising the deferred D4 "no KeepAlive" decision — see `spec.md` v0.2.1), a `com.prx.menubar` login agent, stable binary registration (`--binary-path` → `~/.cargo/bin/prx` → running binary), `new_commits` history entries (so the menubar banners them and lists them in History), `POST /unsnooze` + the menubar **Un-snooze** action, and automatic banner dedup via `X-Prx-Client` liveness. P9 below stays future.

Done and verified (fake gh/agent fixtures, no network):
- Config (defaults ← per-repo merge, `config show/edit/set`), atomic `state.json`, pure lifecycle state machine
- Review pipeline: read-only agent → validated JSON manifest → prx posts inline comments (`gh api`) + review body (`gh pr review`); dry-run; retry-once → failure notification; diff-size cap; `history.jsonl`
- Daemon: detached process, pid-file single-instance guard, 15-min ±jitter per-repo polling, FIFO work queue, rotating logs, 90-day compaction
- IPC: localhost axum API (bearer token, port/token files), CLI as thin client with in-process fallback
- CLI: full spec §3.8 surface; `doctor`; `history`/`insights` analytics
- Chaos tests: agent crash, daemon SIGKILL mid-review, gh auth expiry/recovery, corrupt state/history

Known tradeoffs accepted in v0.1 (documented in code, listed here for future work):
- Reviews serialize on one state lock — effective concurrency is 1 even with `max_concurrent_reviews: 2`
- Findings without file+line appear only in the review body (not inline)
- Unverified against real `gh`: the `gh pr review -R org/repo#N` argv (fix is one line in `github.rs::submit_review/pr_approve` if rejected)
- Notifications (osascript/notify-send) are best-effort and untested on real desktops

---

## P5 — Live-repo validation · **done** · S

**Done 2026-10-04 via OpenSpec change `complete-prx-v1` (openspec/changes/complete-prx-v1).** Live-validated against a real repository: real inline comments + review body posted (with the "automated review by prx" footer), state/history/insights reflected it, daemon restart triggered no duplicate review. Two fixes fell out: the `gh pr review -R org/repo#N` argv regression test, and degrading GitHub's rejection of `--request-changes` on a self-authored PR to a COMMENT review instead of failing.

Requires a machine with `gh` authenticated and a throwaway repo (plan §4: "one live smoke review per release").

1. `cargo build --release` (release profile not yet exercised)
2. `prx init` → `prx repos add <org/repo>` → `prx doctor` (with the real agent binary, e.g. `pi`/`claude`)
3. `prx status` → confirm real PRs appear with correct lifecycle states
4. `prx review <org/repo>#<N> --dry-run` → confirm zero GitHub writes
5. `prx review <org/repo>#<N>` → confirm inline blocking/nit comments land via `gh api` and the request-changes body (with "automated review by prx" footer) is posted
6. `prx daemon start` → let it poll/review unattended for a day; check notifications fire per tier; `prx daemon stop`
7. Exercise `prx approve` after the agent's review (the human-approval path, D2)

**Watch-outs:** the `-R org/repo#N` argv above; agent timeout defaults (600 s) vs real agent latency; label/branch skip rules against a real repo's labels.

**Accept:** a real PR has prx-posted inline comments + review body; state/history/insights reflect it; daemon survives restart without duplicate reviews.

## P6 — Quick polish · **done** · S

**Done 2026-10-04 via OpenSpec change `complete-prx-v1` (openspec/changes/complete-prx-v1).** Shipped `.github/workflows/prx-ci.yml` (ubuntu build/test/clippy `-D warnings`/fmt + macos check), tagged `v0.1`, and did the spec v0.2 doc pass (A1–A24 + Rust decisions folded into `spec.md`, `implementation-plan.md` D1 marked superseded).

- **CI workflow** (`.github/workflows/ci.yml`): `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` on push/PR (plan §4: "dry-run in CI").
- **Tag `v0.1`** after P5 passes (plan Phase 5 closes with the tag).
- **Spec v0.2 doc pass:** fold the audit resolutions (A1–A24) + Rust-adapted decisions from `DEV-PLAN.md` back into `spec.md` (still the 2026-08-20 draft) so the docs are self-consistent; mark `implementation-plan.md` D1 as superseded (Rust).

## P7 — Menu bar shell · **done** · M

**Done 2026-10-04 via OpenSpec change `complete-prx-v1` (openspec/changes/complete-prx-v1).** Shipped as the `menubar/` workspace crate (`prx-menubar`, tray-icon/muda/winit): daemon discovery + attention-count icon, grouped PR menu with per-PR action submenus, footer actions, and tier-mapped native notifications — thin client over the IPC API throughout.

Rust-first (`tray-icon`/`muda` + `winit` event loop on macOS; Swift alternative if native polish matters more than the single binary). Consumes the existing IPC API — no core changes expected.

- Icon + needs-attention count (`GET /status` → count of `reviewed_changes_requested` + `in_review`)
- Dropdown grouped by state (A20 collapsing for Reviewed/Skipped once lists grow)
- Per-PR submenu: Open in GitHub, Re-review (`POST /review {key,force:true}`), Approve (`POST /approve`), Snooze 1h/2h/4h/until-push (`POST /snooze`), Ignore/Un-ignore
- Footer: last poll, Review now (`POST /review-now`), Settings (`prx config edit`), History
- Notifications via `UNUserNotificationCenter` mapped from the tier table (Phase-3 `osascript` path remains the CLI/daemon fallback)

**Accept:** full spec §3.4 experience; every menu action round-trips through the daemon and reflects in state.

## P8 — Login autostart · **done** · S–M

**Done 2026-10-04 via OpenSpec change `complete-prx-v1` (openspec/changes/complete-prx-v1).** `prx daemon install`/`uninstall` — launchd plist on macOS (written, not loaded; effective at next login), systemd user unit + enable on Linux — both idempotent, never touching a running daemon — plus the informational `prx doctor` autostart line with the stale-path warning.

- `prx daemon install` (macOS): write launchd plist to `~/Library/LaunchAgents` (RunAtLoad), `prx daemon uninstall` to remove; `doctor` reports install state
- Linux parity (optional): systemd user unit template

**Accept:** daemon starts at login after install; `daemon status/logs` unchanged.

## P9 — V1.1 candidates (spec §8; pick by demand, in suggested order)

1. Cron per-repo schedules (D3's deferred option) replacing/augmenting interval+jitter
2. Active-hours window (don't review 3am)
3. True concurrent reviews: replace the single state mutex with per-PR locking so `max_concurrent_reviews: 2` is real
4. Review depth/delta mode (re-review = diff since last reviewed SHA only)
5. Comment-style options (inline-only / summary-only / both)
6. File-path filters + PR size threshold beyond the existing diff cap
7. Blocking-issue categorization (A18 V2): keyword taxonomy or agent-derived tags in the manifest
8. History export (JSON/CSV) + weekly digest notification
9. Optional hardening (A13): scoped fine-grained PAT for prx's GitHub writes instead of the full user session
