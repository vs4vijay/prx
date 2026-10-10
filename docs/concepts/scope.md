# Scope and non-goals

**In V1:** multi-repo watching, the full lifecycle state machine (cooldowns anchored to GitHub
creation / commit-detection times, snooze-until-push, ignore, draft tracking), the review pipeline
with the read-only agent contract, diff-size caps, max concurrent reviews, the background daemon
with FIFO queue and jittered polling, desktop notifications with configurable tiers, rotating logs,
90-day history retention, `prx history`/`insights`, `prx doctor`, dry-runs, and the localhost IPC
API.

**Added since V1:** review modes (`off` / `monitor` / `review comment`), cron review schedules,
org-level overrides, live config reload, a configurable browser and review identity, Block/Unblock,
persisted and one-off extra prompts, and the native settings app; then review engines (agents or
LLM APIs) with same-session re-reviews, daemon-wide pause, the `on_review_started` event and the
notifications master switch, inline-comment and review-body template controls, PR briefs
(formerly executive summaries), and verdict-triggered auto-open.

**Still out of scope**: priority ordering beyond oldest-due-first, review depth/delta
modes, file-path filters, exports/digests, CI integration, and team
features.
