# Your first review

This page follows one pull request through prx so you know what to expect before letting the
daemon run on its own. It assumes you have done the [Quickstart](quickstart.md) setup: a config
file (`prx init`, or a first launch of PRX.app), a watched repository (`prx repos add`, or *Add a
repo* in the app), and a clean `prx doctor`.

## 1. See what prx is tracking

```bash
prx status
```

`prx status` polls your watched repositories once and lists their open PRs. A new PR starts as
`pending` with a cooldown: by default prx waits 60 minutes after the PR was created before its
first review, and 30 minutes after new commits before a re-review. Drafts and PRs matching your
skip rules are listed as `skipped`, not hidden.

## 2. Dry-run it

```bash
prx review acme/api#42 --dry-run
```

A dry run executes the whole pipeline (prompt, engine, manifest validation) and prints what would
be posted. It writes nothing to GitHub and changes none of prx's state, so it is the safe way to
check your engine and prompt. It accepts `org/repo#N` and PR URLs.

## 3. Review it for real

```bash
prx review acme/api#42
```

Naming a PR explicitly bypasses the cooldown. What happens:

1. prx renders the review prompt and runs your engine read-only. The engine's only output is a
   JSON [review manifest](../reference/review-manifest.md): a verdict, a one-line summary, and
   findings marked `blocking` or `nit`.
2. prx validates the manifest. A crash, a timeout, or an invalid manifest is retried once.
3. In the default [review mode](../guides/review-modes.md), prx posts the result itself: one
   review body (ending with an `— automated review by prx` footer) and an inline comment for each
   finding that has a file and line. A review with blocking findings is submitted as *request
   changes*; otherwise it is a comment. prx never submits an approval on its own.
4. The PR moves to `reviewed_approved` or `reviewed_changes_requested`, the run is appended to
   history, and a desktop notification fires.

To try this without posting anything, use `prx review acme/api#42 --no-post`, or set the repo to
`monitor` mode.

## 4. Decide

The review is advice. The approval is yours:

```bash
prx approve acme/api#42       # posts YOUR GitHub approval
```

`prx approve` (or *Approve…* in PRX.app) is the only path in prx that approves a pull request.
Other things you can do with a PR: `prx snooze`, `prx ignore`, Block and Unblock from the app
(see [Browser, identity, and Block](../guides/browser-identity-block.md)), or read its
[brief](../guides/briefs.md) with `prx brief acme/api#42`.

## 5. Let the daemon take over

```bash
prx daemon start
```

The daemon polls on an interval, applies cooldowns, queues reviews, sends notifications, and
serves the CLI and PRX.app. If PRX.app is running, it has already started the daemon and this
command only tells you so. From here:

- [Review schedule](../guides/schedule.md) limits when reviews run; [Pausing](../guides/pausing.md)
  holds them for a while.
- [Prompts](../guides/prompts.md) and [Commentary](../guides/commentary.md) shape what the
  reviewer looks for and what gets posted.
- `prx history`, `prx insights`, and `prx usage` show what happened and what it cost.
- [Autostart](../guides/autostart.md) starts prx at login.

If something does not behave as described, run `prx doctor` and see
[Troubleshooting](../troubleshooting.md).
