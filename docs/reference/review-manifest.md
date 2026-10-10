# The review manifest

The agent's only output artifact is a JSON file (versioned, strictly validated):

```json
{
  "manifest_version": 1,
  "verdict": "approved | changes_requested | comment_only",
  "summary": "one-line verdict",
  "findings": [
    { "file": "src/x.rs", "line": 12, "severity": "blocking | nit", "body": "what and why" }
  ],
  "duration_seconds": 95
}
```

Validation rules: `manifest_version` must be `1`; unknown extra fields are tolerated; unknown
`verdict`/`severity` values are rejected; and the **verdict consistency rule** — any finding with
severity `blocking` forces `verdict: "changes_requested"`. A manifest that fails validation, an
agent crash, or a timeout is retried **once**; if the retry fails too, the review fails cleanly
(state returns to reviewable, an `on_failure` notification fires, history records the failure).
Findings without both `file` and `line` are included in the review body but not posted inline.

The manifest may also carry an optional `brief` string — a plain-language description of the PR
that prx stores as the PR's [brief](../guides/briefs.md).

The written manifest files are kept under `<data dir>/manifests/` for auditability — the agent's
artifact is the source of truth.

The schema is part of the output contract prx appends to every prompt, so a custom
[prompt](../guides/prompts.md) never needs to repeat it. Why the agent is limited to this one
artifact is explained in [Architecture](../concepts/architecture.md).
