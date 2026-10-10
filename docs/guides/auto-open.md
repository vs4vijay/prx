# Auto-open

`auto_open` (defaults / org / repo) lets a finished review pull you in: when a review completes
with a matching verdict, the daemon opens the PR's GitHub URL in the effective `browser`.

| Value | Opens the PR when the review is… |
|---|---|
| `off` *(default)* | never — PRs open only through your own actions |
| `approved` | approved: ready for your human approval |
| `blocking` | changes requested |
| `both` | either of the above |

```bash
prx config set defaults.auto_open blocking
prx config set repos.acme/api.auto_open both
```

Comment-only verdicts never auto-open. It fires in `monitor` mode too (the review happened even
though nothing was posted), never for failed or skipped runs, and only from the daemon — an
in-process `prx review` fallback (daemon down) does not open a browser.
