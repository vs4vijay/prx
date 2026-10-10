# Browser, identity, and Block

```yaml
defaults:
  browser: Firefox                              # or "/Applications/Arc.app", or "system"
  identity:
    login: my-review-bot
  block_message: "Blocked pending the migration plan — see the thread."
orgs:
  - org: work-org
    browser: ["open", "-na", "Google Chrome", "--args", "--profile-directory=Work", "{{URL}}"]
    identity:
      credential: GH_TOKEN_WORK                 # env var NAME, never the token
```

- **`browser`** is honored by every PR-opening action — the menu's *Open in GitHub*, banner
  click-through, and the daemon's [auto-open](auto-open.md), all through one resolver in the core.
  `system` uses `open <url>` (`xdg-open` on Linux), an app name/path uses `open -a <app> <url>`,
  and a YAML list is run as argv (no shell) with `{{URL}}` substituted; a list without `{{URL}}`,
  or a scalar containing it, fails validation. When the configured browser cannot be launched
  (say, an app that isn't installed), prx logs a warning and opens the system default instead.
- **Choosing it in PRX.app**: Settings → *Account & Browser* → *Open PRs with* (and the same
  row in each repo or org editor under *Repos & Orgs*) is one dropdown:
  - *System default (<name>)* saves `system`; the label shows your current default browser.
  - Every installed app that can open `https` links, with its icon (default first, then by name).
    Picking one saves its name, e.g. `browser: Firefox`, or its path when two installed browsers
    share the name.
  - *Other app…* opens a chooser for any application and saves its path (or its name when it is
    one of the listed browsers); the dropdown then shows the app by name.
  - *Custom command…* shows the one-argument-per-line editor; `{{URL}}` must appear exactly once.
  - Repo and org rows also offer *Inherit*, which removes the override (while the row inherits,
    it reads *Inherit (<browser>)*, e.g. *Inherit (Firefox)*).

  A saved app that isn't installed shows as *<value> (not found)* and is kept as-is until you
  pick something else. *Test* opens the prx project page with the current choice and shows any
  failure (such as a missing program) under the row; it saves nothing and never falls back to
  the system browser.
- **`identity.credential`** names an environment variable whose token is passed to `gh` as
  `GH_TOKEN` for **every GitHub write** on that repo: review bodies, inline comments, approve (daemon
  and CLI), Block, and Unblock. PR reads keep `gh`'s default account. The variable is read per call
  — when it is missing, that write fails with a clear error (a review fails as "posting review
  failed"). The daemon must see the variable, so export it in the environment the daemon starts
  from. No credential = the `gh`-authenticated user, as before.
- **`identity.login`** is the login prx acts as; it defaults to the credential's (or `gh`'s) user.
- **Block / Unblock** are human actions in the menu bar's per-PR submenu (`POST /block` /
  `POST /unblock`). Block posts a request-changes review with the effective `block_message` and moves
  a tracked PR to `reviewed_changes_requested`. Unblock dismisses the latest request-changes review
  authored by prx's identity and returns the PR to **pending** (cooldown anchored at the unblock,
  last verdict kept), so the menu offers Block again; with nothing to dismiss it is a clean no-op note.
