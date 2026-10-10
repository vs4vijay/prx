# Commentary: what prx posts

What a posting review writes is shaped by three layers, each settable at defaults / org / repo:

- **Whether anything is written** — `review_mode: monitor` is the zero-write switch (see
  [Review mode](review-modes.md)); there is no separate comment on/off key.
- **`comments.inline`** (default `true`) — `false` keeps every finding in the review body and posts
  no inline PR comments. Findings without both `file` and `line` are always body-only.
- **`comments.template`** — the review body. Unset renders the built-in body: the effective
  `review_heading` line, the summary, the blocking and nit sections, and the attribution footer —
  byte-for-byte what prx posted before templates existed, including any `review_heading` /
  `identity.attribution_footer` overrides.

A custom template **replaces the whole body** (the `review_heading` line is part of the built-in
body only, so write your own heading). Placeholders:

| Placeholder | Renders |
|---|---|
| `{{PR_KEY}}` | `org/repo#n` |
| `{{VERDICT_LABEL}}` | `approved — no blocking issues`, `changes requested`, or `comment-only review` |
| `{{SUMMARY}}` | the manifest's one-line summary |
| `{{BRIEF}}` | the PR's [brief](briefs.md) — see the note below |
| `{{EXEC_SUMMARY}}` | deprecated alias of `{{BRIEF}}` (renders the same text) |
| `{{BLOCKING_COUNT}}` / `{{NIT_COUNT}}` | finding counts |
| `{{FINDINGS_BLOCKING}}` / `{{FINDINGS_NITS}}` | a `### Blocking (n)` / `### Nits (n)` section with one bullet per finding; empty when there are none |
| `{{IDENTITY}}` | the effective `identity.attribution_footer`, else `— automated review by prx` |

```yaml
defaults:
  comments:
    inline: false
    template: |
      ## {{VERDICT_LABEL}} — {{PR_KEY}}

      {{SUMMARY}}

      {{BRIEF}}

      {{FINDINGS_BLOCKING}}

      {{FINDINGS_NITS}}

      {{IDENTITY}}
```

Any other `{{...}}` (say `{{REVIEW_SCORE}}`) fails validation when the config loads — `prx config
set` refuses to save it and a running daemon keeps its last good config — so no review ever posts a
half-rendered body. A line holding only a placeholder whose value is empty is dropped with one
adjacent blank line, so absent sections leave no gaps.

## Where `{{BRIEF}}` comes from

When the review agent writes a `brief` into its manifest, the
body carries that brief, for the commit just reviewed. Otherwise the on-demand brief is generated
*after* the GitHub posts (so it can never delay them), and the body carries the PR's stored brief
from earlier — or renders empty when the PR has none yet.

The GitHub-side author is always the `gh`-authenticated account (or the `identity.credential`
account); the template and footer are attribution text only.
