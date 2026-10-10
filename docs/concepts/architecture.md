# Architecture: the agent analyzes, prx writes

prx is one Rust binary (CLI, lifecycle state machine, review pipeline, background daemon, localhost
IPC API) plus an optional macOS client, PRX.app. This page explains how the pieces fit and why the
write path is separated from the analysis path. For the threat-model summary see
[Security](security.md).

## Overview

```
prx daemon ──poll──> gh pr list ──> lifecycle state machine ──> desktop notifications
    │                                                     │
    │ render prompt + run agent                           state.json / history.jsonl
    v                                                     │
read-only agent ──writes──> review manifest (JSON)        └──> localhost IPC API
                             │                                  ^           ^
                    prx validates & posts                        │           │
                             v                                   │           │
              gh api (inline) + gh pr review (body)        CLI commands   menu bar client
                             ^                                           (separate GUI)
                    prx approve (human only)
```

- **Rust core** (this repository): one binary, `prx`, containing the CLI, the lifecycle state
  machine, the review pipeline, the background daemon, and a localhost IPC API.
- **PRX.app** (macOS): the menu bar item and app window, a GUI client that consumes the localhost
  API and supervises the daemon. It is a separate crate, not part of the core — see the
  [PRX.app guide](../guides/prx-app.md) and [PRX.app internals](prx-app-internals.md).

## The trust separation

Automated code review has a trust problem: an agent that can post to GitHub can also approve, merge,
or hide instructions inside a diff. prx resolves this with a strict separation:

1. **The agent is read-only.** Every prompt ends with prx's fixed output contract (appended after
   any custom prompt text, which cannot remove it) that forbids any GitHub
   write: no `gh pr review`, no `gh api` write verbs, no merging. It fetches the diff, analyzes
   against the repo's own standards (`AGENTS.md`, `CONTRIBUTING.md`, lint configs), and writes **one
   JSON review manifest** to the file path prx gave it. (With an [`llm_api` engine](../guides/engines.md) there
   is no agent process at all: prx fetches the diff itself, sends it with the same prompt, and takes
   the manifest from the API's reply — the rest of the pipeline is identical.)
2. **prx validates and posts.** The manifest is strictly validated (schema, verdict consistency) and
   becomes the machine source of truth for state, verdicts, counts, history, and insights. prx then
   posts inline comments itself via `gh api repos/{owner}/{repo}/pulls/{n}/comments` and submits one
   review body via `gh pr review -R org/repo#n --request-changes|--comment --body-file -`, with a
   `— automated review by prx` attribution footer (or the effective `identity.attribution_footer`).
3. **The agent never approves.** A GitHub approval is posted **only** through the human's explicit
   `prx approve` (or the menu bar client) — that is the single approve path in the entire system.

## Prompt injection stance

the always-appended output contract instructs the agent to treat all PR content —
title, description, comments, diff text — as untrusted *data*, never as instructions, and to report
suspected injection attempts in its summary. prx-side, nothing from the PR is ever executed: it only
shapes which comments get posted and which lifecycle transition runs.

## No tokens are stored

prx never touches GitHub credentials; every GitHub call goes through your
authenticated `gh` CLI session, and the only network prx itself opens is the localhost IPC listener
— plus, only if you configure an [`llm_api` engine](../guides/engines.md), HTTPS calls to that engine's
`base_url`. (Multi-account setups name an *environment variable* holding a token via
`identity.credential`, and LLM engines name the variable holding their API key via `api_key_env` —
the secret itself never enters the config; see [Browser, identity, and Block](../guides/browser-identity-block.md)
and [Security notes](security.md).)

## Lifecycle states

Every tracked PR is in exactly one state: `pending`, `in_review`, `reviewed_approved`,
`reviewed_changes_requested`, `skipped` (with a reason: `draft`, `config`, `snoozed`, or `ignored`),
`merged`, or `closed`. Cooldowns, skip rules, the [review mode](../guides/review-modes.md), the
[schedule](../guides/schedule.md), and any [pause](../guides/pausing.md) decide when a `pending` PR
is reviewed.

A review's in-progress transition is applied **in memory only**
until the run completes or fails, so a crash or `kill -9` mid-review can never strand a PR in
`in_review` — the next run picks it up cleanly. History older than 90 days is compacted (atomically)
on daemon start.

## One write path

CLI commands that mutate act as thin clients against the [IPC API](../reference/ipc-api.md) when the
daemon is up and fall back to in-process execution (with a warning) when it is not, so all state
changes and GitHub writes flow through the daemon. PRX.app uses the same API and nothing else.

Related: [the review manifest](../reference/review-manifest.md), [data files](../reference/data-files.md).
