# Briefs

A **brief** says what a PR does, in plain language, for someone who has not opened the diff: 2–5
short sentences (at most 1200 characters) on what changes, why, and anything to notice (risky
areas, migrations, breaking changes). Each tracked PR has at most one, stored in `state.json` with
`created_at` (first brief), `updated_at` (last write), the head SHA it describes, its source
(`review` or `on_demand`), and the engine. `GET /status` reports it with a derived `stale` flag —
true once the PR's head has moved past the brief's SHA. A stale brief stays visible until it is
replaced. The menu bar shows it in the PR's submenu.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../media/screenshots/prs-dark.png">
  <img src="../media/screenshots/prs.png" alt="The Pull requests page in PRX.app with one row open: the brief of a pull request, when it was created and updated, a Regenerate button, and the last verdict below it">
</picture>

Briefs come from two places:

- **The review itself, at no extra cost.** The output contract asks the review agent for an
  optional `brief` field in its manifest. When the agent writes one, prx stores it (source
  `review`) and runs nothing else.
- **On demand.** When the manifest has no brief (and `brief.enabled` is on), prx generates one
  right after the review is committed. `prx brief <target> --regenerate`, the menu, and
  `POST /brief` generate one at any time, also for PRs that were never reviewed (any repo mode
  except `off`). An on-demand run reads the PR read-only (`gh pr view` + `gh pr diff`), sends the
  title, description, file list, and diff (cut at a file boundary past 60,000 characters, with a
  note) to the brief engine as untrusted data, and never writes to GitHub.

```yaml
defaults:
  brief:
    enabled: true          # default; false = no briefs stored, generated, or shown
    engine: haiku          # any engines entry (e.g. the llm_api `haiku` from the Engines guide); unset = the review engine
    timeout_seconds: 60    # default 120
repos:
  - name: org/huge-repo
    brief:
      enabled: false
```

It is strictly best-effort: the verdict never depends on it, and an engine error, timeout, or empty
reply is logged — the review still completes, posts, and transitions — and the previous brief, if
any, stays unchanged. Regenerating keeps `created_at` and moves `updated_at`. A slow run for an old
head never overwrites a brief written since for a newer head.

## Renamed from `executive_summary`

The section was called `executive_summary`. That key is still
read as `brief` (when both are set, `brief` wins); prx logs a deprecation and `prx doctor` mentions
it, and config edits write `brief` only. Stored executive summaries become briefs on load, dated to
the PR's last review.

## Cost note

an on-demand brief is an extra engine run (for an agent, a whole session). Agents
that follow the output contract write the brief in the review itself, so this run is rare; when
it does run, point `brief.engine` at a cheap [`llm_api` engine](engines.md), or disable briefs per
org/repo where you don't read them. Brief runs count as brief usage in `prx usage`.

See also [`prx brief`](../reference/cli.md#prx-brief) and `POST /brief` in the
[IPC API](../reference/ipc-api.md).
