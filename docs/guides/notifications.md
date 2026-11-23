# Notifications

prx raises desktop notifications for lifecycle events. Each event maps to a tier, and each tier
decides whether a banner and a sound are used. The event list, default tiers, and the master
switch are in the [configuration reference](../reference/configuration.md#notification-events-and-tiers).

## Ready and blocked PRs

Two review outcomes need you most. A PR is **ready** when its review verdict is `approved`: it
waits for your approval. A PR is **blocked** when the review found blocking issues. By default
they sound different: a ready PR plays `Glass`, and a blocked PR plays the urgent tier's `Basso`.
To pick other sounds, or no sound, set `notifications.sounds.<event>`
([per-event sounds](../reference/configuration.md#per-event-sounds)):

```bash
prx config set defaults.notifications.sounds.on_review_complete_approved Hero
prx config set defaults.notifications.sounds.on_review_complete_blocking none
prx config set defaults.notifications.sounds.on_review_complete_approved null   # back to Glass
```

PRX.app also keeps a **bell** for them in the menu bar. When a review makes a PR ready or blocked,
the icon shows a bell and the title adds `✓N` (ready) and `✕N` (blocked) after the attention
count. A PR leaves the bell when you open it from PRX (its menu row, its banner, or a link in the
window), or when it is no longer ready or blocked (a new push, your approval, a merge). The bell
is kept in memory: reviews that finished before the app started do not ring, and quitting the app
clears it. Turn it off with `prx config set defaults.notifications.menubar_bell false`.

No notification approves anything. Approving stays your action, from the menu or `prx approve`.

![The Notifications settings page in PRX.app: the Deliver notifications switch, and a list of events (New PR, Review started, Review complete, New commits, Re-review triggered), each set to Urgent, Info or Silent](../media/screenshots/notifications.png)

## From the daemon

Delivery is best-effort (`osascript` on macOS, `notify-send` on Linux) and never fatal.
Per-org and per-repo `notifications.*` overrides decide the tier and the sound of that repo's
events.

## From PRX.app

The shell raises native macOS banners for *new* history events whose tier has
`banner: true` — the same event → tier and sound config the daemon reads (a new PR is a
`new_pr` history entry, so its banner comes from the app as well) (`silent`-tier events, such as
merges and closes by default, never banner; on cold start the existing history is marked seen, not
re-bannered). The daemon also banners these events itself, but duplicates are resolved
automatically: the shell identifies itself on every IPC request
(`X-Prx-Client: menubar/<version>`), and the daemon suppresses its own banners while a menubar has
been seen within the last 90 s (≈ 9 missed 10 s polls), resuming banner delivery by itself once no
menubar answers for that window. Only PRX.app counts: `prx` commands run in a terminal send no such
header and never mute the daemon. `notification_tiers.<tier>.banner: false` in `config.yml` remains
the manual override — it silences the tier on both sides regardless. This applies to
`on_review_started` too: a review start is a `review_started` history entry for the shell and a
daemon notification at the same moment, deduplicated the same way, so if you still see doubled
"review started" banners (e.g. a menubar older than the daemon), set that event's tier to one with
`banner: false` or to `silent`. The shell also honors the global `defaults.notifications.enabled`
master switch: while it is `false` neither side raises anything.

Run as PRX.app, the shell posts its banners through the macOS notification center, so they come
from **PRX** (with its icon) and clicking one opens the PR in the repo's browser. macOS asks once
whether PRX may send notifications; you can change the answer later in System Settings →
Notifications → PRX, and it applies to the next banner. If you choose *Don't Allow*, PRX shows no
banners at all (the daemon stays quiet too while the app runs), but the menu and window still show
every state. Until you answer, or when macOS will not authorize the bundle (an ad-hoc build run
from outside an Applications folder) or you run the bare `prx-menubar` binary, banners use the
older path: [`terminal-notifier`](https://github.com/julienXX/terminal-notifier) when installed
(click opens the PR), else plain `osascript` (no click action).
