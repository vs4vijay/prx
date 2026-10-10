# Troubleshooting

Start with `prx doctor`. It prints one line per check and exits non-zero when something prx needs
is missing:

- the config file parses and validates;
- `gh` is on `PATH` and authenticated (`gh auth status`);
- every referenced engine resolves: agent binaries on `PATH`, `llm_api` engines with their key
  variable set (a warning otherwise);
- which levels use the built-in or a custom review prompt;
- the data directory is writable;
- the login-autostart registrations (informational, never a failure).

For anything the daemon did, `prx daemon logs` tails the newest log file under
`~/.local/share/prx/logs/`. The manifests each review produced are kept in
`~/.local/share/prx/manifests/`.

## A PR is not being reviewed

Check these in order; `prx status` shows the state and skip reason of each PR.

| Cause | What to look for |
|-------|------------------|
| Cooldown | A new PR waits `cooldown.before_first_review_minutes` (default 60) after its creation, and `between_re_reviews_minutes` (default 30) after new commits. `prx review org/repo#N` bypasses it. |
| Skip rules | Drafts, and PRs matching `skip.authors`, `skip.labels` (default `wip`, `do-not-review`), or `skip.branches`, are tracked as `skipped`. |
| Schedule | With a `schedule` set, a repo waits for its next cron tick. *Check GitHub now* / `prx review-now` only polls early. See [Review schedule](guides/schedule.md). |
| Pause | `prx status` shows the pause; `prx resume` lifts it. See [Pausing](guides/pausing.md). |
| Mode `off` | No polling and no reviews for that repo; an explicit `prx review` is refused. See [Review modes](guides/review-modes.md). |
| Diff too large | Diffs over `diff_max_files` (50) or `diff_max_lines` (4000) are skipped with a `too_large` note and re-checked on later polls. |
| Snoozed or ignored | `prx unignore`, or *Unsnooze* in the app. |

## A review failed

A manifest that fails validation, an agent crash, or a timeout (`agent_timeout_seconds`, default
600) is retried once. If the retry fails too, the PR returns to reviewable, an `on_failure`
notification fires, and history records the failure. Look at `prx daemon logs`, then run
`prx review org/repo#N --dry-run` to reproduce it without posting.

- **"posting review failed"**: the repo uses `identity.credential` and the named environment
  variable is not set in the daemon's environment.
- **An `llm_api` review fails naming a variable**: the `api_key_env` variable is not set in the
  daemon's environment.
- **A resumed session keeps failing**: prx already retries a failed resume as a fresh session in
  the same attempt, so a broken session alone does not fail a review.

## The daemon cannot find `gh` or my agent after login

launchd and systemd start your session with a minimal `PATH` and do not read your shell profile.
A command configured as a bare name (`command: pi`) works in your terminal and is not found at
login.

- Set `engines.<name>.command` to an absolute path, for example `/Users/you/.bun/bin/pi`.
  `prx doctor` shows what each engine resolves to.
- launchd plists written by `prx daemon install` record the `PATH` of the shell that ran the
  install; re-run it from a shell where `gh` resolves if that changed.
- API key variables (`api_key_env`) and `identity.credential` variables exported only in
  `~/.zshrc` are invisible to an autostarted daemon. `prx doctor`, run from your shell, cannot
  detect that. Export them where the daemon starts.
- PRX.app avoids most of this: a daemon it starts gets your login shell's `PATH`.

See [Autostart](guides/autostart.md).

## Autostart registration problems

- **Nothing starts until the next login.** On macOS the plists are written but not loaded. Load
  the daemon's now with `launchctl load ~/Library/LaunchAgents/com.prx.daemon.plist`.
- **`prx doctor` warns the registered binary no longer exists, or points into a build
  directory.** This happens after `cargo clean` or a move. Run `cargo install --path .`, then
  `prx daemon install` again (or pass `--binary-path`).
- **`autostart leftover` warnings.** Legacy `com.prx.daemon` / `com.prx.menubar` agents sit beside
  the PRX.app login item. Re-run `prx daemon install` to remove them.
- **Starting the daemon manually after an autostart** is harmless: the pid-file guard allows one
  daemon.

## A config change did not apply

The daemon re-reads `config.yml` within about a second. If an edit breaks the file (bad YAML or a
failed validation), the daemon keeps running on the last good config and reports the reason as
`config_error` in `GET /status`, in the app's menu, and in the settings pages. Fixing the file
clears it.

- `max_concurrent_reviews` does not hot-reload. Restart the daemon after changing it.
- A `review_prompt` that is a file path is rejected: it is the prompt text now. Paste the file's
  contents instead. See [Prompts](guides/prompts.md).
- An unknown `{{...}}` placeholder in `comments.template` fails validation. The allowed set is in
  [Commentary](guides/commentary.md).
- An inline API key (`api_key`, `token`, …) or a key pasted into `api_key_env` is rejected: name
  the environment variable instead.
- Saving from the settings form drops YAML comments. Use the *config.yml* page to keep them.
- Any rewrite of a config using the legacy `agents:` / `agent` keys saves it as `engines:` /
  `engine`, which older prx binaries cannot read. Back the file up first if you might roll back;
  see [Engines](guides/engines.md#legacy-agents--agent-keys).

## `prx review` rejects my target

- A short form (`42`, `#42`, `repo#42`) that matches several watched repos enqueues nothing and
  lists the candidates. Use `org/repo#N`.
- Short forms are resolved by the daemon. `--dry-run`, and the in-process fallback when the
  daemon is down, accept only `org/repo#N` and PR URLs.
- A merged or closed PR is rejected with its state. The repo must be watched.

## macOS will not open PRX.app or `prx`

You downloaded PRX.app, opened it, and macOS stopped it with a message like one of these (the
exact wording depends on the macOS version):

- *"PRX" Not Opened. Apple could not verify "PRX" is free of malware that may harm your Mac or
  compromise your privacy.*
- *"PRX" can't be opened because Apple cannot check it for malicious software.*
- *"PRX" cannot be opened because the developer cannot be verified.*
- *"PRX" is damaged and can't be opened. You should move it to the Trash.*

Or you downloaded the CLI, and running `prx` prints only `killed`.

Nothing is damaged. This is Gatekeeper: PRX.app and `prx` are signed ad hoc and not notarized by
Apple, and macOS refuses such a program while it carries the quarantine flag that browsers put on
downloads. Close the dialog (*Done* or *Cancel*, not *Move to Trash*) and clear the flag:

```bash
xattr -dr com.apple.quarantine /Applications/PRX.app     # the app, wherever you put it
xattr -d com.apple.quarantine ~/.local/bin/prx           # a prx from a release tarball
```

Then open it again. For the app there is also System Settings → Privacy & Security → *Open
Anyway*, which appears after a refused open; it is not offered for the "damaged" message. The
step is needed again after each new download. See
[Allow it to open](getting-started/install.md#step-2-allow-it-to-open) in the install guide.

## PRX.app

- **macOS refuses to open a downloaded PRX.app** ("Apple could not verify", "is damaged"). See
  [macOS will not open PRX.app or `prx`](#macos-will-not-open-prxapp-or-prx) above.
- **The menu shows *Daemon restarting*, then *Daemon stopped*, on a new install of v0.1.0.**
  PRX.app v0.1.0 does not create the config file, and the daemon does not start without one;
  `logs/daemon-stderr.log` says ``config file not found … run `prx init` first``. From v0.1.1 the
  app writes a default config on its first launch, so either
  [update](getting-started/install.md#update), or stay on v0.1.0 and run
  `/Applications/PRX.app/Contents/Helpers/prx init` once, then choose *Start daemon* in the menu.
  On v0.1.1 or later the same two menu states mean something else: see the next entry.
- **The menu shows *Daemon stopped*.** The app gave up after 5 daemon failures within 10 minutes.
  Choose *Show daemon log*, and check `logs/daemon-stderr.log` in the data directory for start-up
  errors. *Start daemon* tries again. A config file that does not parse or validate is a common
  cause (`failed to parse config file …`, `configuration is invalid`): PRX.app creates a config
  only when there is none, and never replaces or repairs one that exists, so fix the file (or
  move it away to get a fresh default on the next start).
- **The menu says the daemon and app versions differ.** An older daemon (from the CLI or a login
  item) is still running. Stop it with `prx daemon stop`, then choose *Start daemon* in the menu.
- **No banners.** Check System Settings → Notifications → PRX, and
  `defaults.notifications.enabled`. An ad-hoc build run from outside an Applications folder
  cannot use the notification center; it falls back to `terminal-notifier` when installed, else
  `osascript`.
- **Doubled "review started" banners.** This can happen with a menu bar app older than the
  daemon. Set that event's tier to `silent`. See [Notifications](guides/notifications.md).
- **`prx` is not found after *Install command-line tool…*.** `~/.local/bin` is not on your login
  shell's `PATH`; the settings section shows the line to add. An existing regular file at
  `~/.local/bin/prx` is never overwritten: move it away first.
- **The built app has a generic icon.** Install `rsvg-convert` (`brew install librsvg`) and
  rebuild.

## Usage shows no tokens

Only `claude`, `codex`, and `llm_api` engines report usage out of the box. For any other agent
(including `pi`), map its output with `usage_output`. Runs without usage are counted separately,
never as zeros. See [Token usage](guides/token-usage.md).

## Damaged data files

A corrupt `state.json` makes prx fail loudly and is never overwritten, so you can inspect or
restore it. A corrupt line in `history.jsonl` is skipped and the rest is read normally. See
[Data files](reference/data-files.md).
