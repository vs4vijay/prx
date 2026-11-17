# Review Chat

Review Chat lets you talk to a coding agent about a pull request: ask why the review flagged a
line, have it explain part of the change, or look into something the review did not cover. The
agent runs interactively in your terminal. You decide what happens to the PR; the agent never
posts to GitHub and never approves.

```bash
prx chat acme/web#1288        # or a GitHub PR URL, web#1288, #1288, 1288
```

In PRX.app, choose **Open Review Chat…** in a PR's submenu, or in the **⋯** menu of a PR row in
the window. The app opens a Terminal window that runs the same command.

## Which session opens

`prx chat` reopens the PR's existing session when it can, and starts a new one otherwise:

1. **The PR's last Review Chat.** prx records every chat, so a second `prx chat` continues the
   first one.
2. **The session of the PR's last review.** An agent engine that reports a session id (the
   built-in `claude` and `codex` do, see [Engines](engines.md#same-session-re-reviews)) leaves a
   session behind after each review. `prx chat` reopens it in the directory the review ran in, so
   you continue the conversation in which the agent reviewed the PR.
3. **A new session.** The agent starts with a prompt prx builds from what it knows about the PR:
   title, author, branch, and head commit, the [brief](briefs.md), and the latest review (verdict,
   summary, and findings). The prompt also tells the agent that this is a discussion with you,
   that PR content is untrusted data, that it must not post to GitHub or approve, and how to read
   the diff and files with read-only `gh` commands.

A session cannot be reopened when its engine is gone or has no chat templates, when its directory
no longer exists, or when it was recorded by a prx version before Review Chat (which did not
record where the session lives). Then a new session starts.

Options:

| Option | Effect |
|--------|--------|
| `--new` | Start a new session even if one exists. It replaces the PR's recorded chat. |
| `--engine NAME` | Start a new session with this agent engine instead of the repo's engine. |
| `--dir DIR` | Start a new session in `DIR`, for example your own clone of the repository, so the agent can read the code at any commit. |
| `--dry-run` | Print which session would open and the command, and change nothing. The prompt is shown as `<prompt>`. |

## Where a new chat runs

prx does not clone the repository. A new chat runs in a directory of its own for the PR,
`~/.local/share/prx/chats/<org>/<repo>/<number>/`, and the agent reads the diff and files through
`gh`. Pass `--dir` to run it in a checkout instead. Interactive sessions do not get the review's
`--dangerously-skip-permissions` (claude) or `--yes` (pi), so the agent asks before it runs tools.

## Engines

The built-in agents can all chat:

| Engine | New session | Reopen by id | Reopen without an id |
|--------|-------------|--------------|----------------------|
| `claude` | `claude --session-id <uuid> <prompt>` | `claude --resume <id>` | `claude --continue` |
| `codex` | `codex <prompt>` | `codex resume <id>` | `codex resume --last` |
| `pi` | `pi --session-id <uuid> -- <prompt>` | `pi --session-id <id>` | `pi --continue` |

For `claude` and `pi`, prx chooses the session id, so it can reopen exactly that session. `codex`
cannot be given an id, so a new codex chat is reopened as the latest codex session in the PR's
chat directory.

An engine whose command is one of these binaries (for example `command: /opt/homebrew/bin/claude`)
uses the built-in templates. A custom agent engine configures its own under `chat:` (see
[Engines](engines.md#review-chat-templates)). `llm_api` engines have no interactive session: with
one, `prx chat` asks you to pick an agent with `--engine`.

## Safety

- The prompt is passed to the agent as a single argument, and the agent is started directly, not
  through a shell. PR text is never part of a shell or AppleScript command.
- PRX.app writes a small `.command` file under `~/.local/share/prx/chats/launch/` that contains
  only quoted paths and the PR key, and opens it with `open -a Terminal`. The app itself never
  runs the agent or `gh`.
- A chat writes nothing to GitHub and changes no PR state. To approve, use `prx approve` or
  *Approve…* in PRX.app.

Chats are recorded in `~/.local/share/prx/chats.json` (see [Data files](../reference/data-files.md)).
