# PRX — Design Audit

- **Part 0** (2026-10-10): menu bar redesign: the status glyph and the menu's information architecture.
- **Part I** (2026-10-08): the app window (`menubar/settings-ui/`) and the menu bar menu (`menubar/src/model.rs`, `banners.rs`). Task 7.4 of `prx-desktop-app-and-briefs`.
- **Part II** (2026-09-03): the original spec audit.

---

# Part 0 — Menu bar redesign (2026-10-10)

The redesign answers the user's feedback on daily use. It was done with `/impeccable` in Operate mode: scanability, native macOS idiom, and one obvious path to each task.

## Status glyph

- **No colour, no drawn count.** The attention state used to be a 3×5-pixel number inside a 7 pt pill. It was unreadable at 1x, crowded the mark, and older builds drew it in red. Menu-bar peers (Mail-style counters, GitHub PR menu bar apps) use a template glyph and put the count in the status item's **title**, beside the glyph. PRX now does the same. The title uses the menu bar's own font, stays crisp at every scale, tints with the bar, and VoiceOver reads it. Colour stays out of the menu bar: macOS tints template images, and a red mark reads as an error, not as "PRs are waiting".
- **Five shapes, one corner.** The idle state is the full Verdict X. Each other state swaps the counter-stroke's lower segment for one badge in the bottom-right corner, knocked out 1 pt from the mark:
  - **Attention**: a solid dot (the notification-dot idiom).
  - **Reviewing**: three small dots.
  - **Paused/stopped**: pause bars, shown both for a pause and for review mode `off`.
  - **Offline**: the mark at 45% opacity plus a slashed ring.

  Precedence is offline, then paused/stopped, then reviewing, then attention, then idle. The count title is shown whenever the daemon answers and attention > 0, in any glyph state (`99+` cap). It is hidden offline. The tooltip says the state in words. Sources: `assets/menubar-glyph.svg`, `assets/menubar-glyph-states.png` (`assets/render.sh glyph`), and `menubar/src/icon.rs`.

## Menu structure

The sections run top to bottom, most urgent first:

1. **Header (what needs you):** the headline ("2 PRs need attention" / "All clear" / "Reviews off"), "Checked GitHub 2m ago · next in 13m", "⏸ Automatic reviews paused · 1h 12m left" while paused, and the last action message.
2. **Quick actions:** Check GitHub now ⌘R, Review PR… ⌘N, and History ▸ (ending in "Open history in PRX"). These sit above the PR list so they never need a scroll.
3. **PR groups, attention first:** Changes requested, In review, Ready for approval (with inline Approve…), Needs review, Reviewed, Skipped, Merged / closed. "Changes requested" is now its own group. Before, half of the headline's count was buried inside "Reviewed". With `menu.group_by` set to `repo` or `none`, those PRs are also pinned in a "Needs attention (N)" group at the top. The user's own setup has 166 PRs in one flat list, so before this the 4 attention PRs could sit pages deep. PR submenus are split by separators: open · brief · review runs · Approve/Block · Snooze/Ignore.
4. **Automatic reviews:** the pause control and "Review mode: Review and comment ▸" (round 3 merges them into one submenu).
5. **App:** Open PRX ⌘O, Settings… ⌘,, and "PRX 0.1.0 · daemon running ▸" (About).
6. **Quit PRX ⌘Q.** Holding ⌥ shows Quit PRX (keep reviewing).

**Diagnostics moved to About.** The daemon pid, uptime and version used to sit behind ⌥ under the status line. They now live in the About submenu with the PR, in-flight and queue counts, "Show daemon log", and "Open config.yml". The About title flags a daemon whose version differs from the app's. Offline, About is a plain "PRX x.y · daemon not running" line, because Start daemon and Show log sit right above it.

**One refresh item.** There were two refresh-like items. "Check GitHub now" (online, `POST /review-now`) makes the daemon reload its config if it changed and poll every repo now. It also enqueues reviews that are due, unless paused. "Refresh now" (offline) only made the app re-probe the local daemon, which already happens on every menu open and every ~10 s. Its result also could not show until the menu closed. "Refresh now" is removed. "Check GitHub now" is the only refresh, on ⌘R, and its message reads "checking GitHub now" (or names the pause).

**Pause vs mode.** These are now two submenus in one section. Each title states the current value, and each says what it does:

- **Pause** (temporary, daemon-wide): "Pause automatic reviews ▸" offers For 30 minutes / 1 hour / 2 hours / 4 hours under two disabled lines, "Holds automatic reviews; manual reviews still run" and "Resumes by itself when the time is up". While paused, the title becomes "⏸ Automatic reviews paused · 1h 12m left" and "Resume now" leads the submenu.
- **Mode** (lasting, per-repo default): "Review mode: Monitor ▸" opens with "Default for repos without their own mode". The radio rows say what each mode does to GitHub: "Review and comment — reviews and posts to GitHub", "Monitor — reviews, never posts", "Off — no checks, no reviews". "Per-repo modes…" opens the window's Repos page.

**Verbs and keys.** Open opens a place (PRX, GitHub, config.yml, history). Check asks GitHub. Review runs the agent. Pause/Resume and Show are used as before. ⌘R, ⌘N, ⌘O, ⌘, and ⌘Q answer while the menu is open.

**Spec deviation.** This replaces the visual-identity footer order (Open PRX, Review PR…, Settings…, pause, Quit). The spec delta now describes the new order.

## Round 2 (2026-10-10, after daily use)

The user ran the redesigned build and raised five points. Each decision below supersedes the matching part above.

- **Inline Approve names its PR.** `indentationLevel` alone did not read, so the user could not tell which PR an "Approve…" belonged to. The row is now "↳ Approve #13…": an `arrow.turn.down.right` symbol in the image column, directly under the PR row's state symbol, one indent level in, with the PR number in the label. The ⌥-alternate option was rejected because it hides the action and AppKit does not alternate a submenu row cleanly. The confirmation dialog is unchanged.
- **One icon system: SF Symbols.** Before, the menu mixed daemon emoji (🆕 ⏳ 🔁 🔄 ✅ ⚠️ 💬 🚫 ✔️ 🗑️), history emoji (🔍 ❌ ⏭️ 📝), and unicode prefixes (⏸ in the pause title, ✓/⚠ in messages). These glyphs rendered in colour, at mixed sizes, and inside the label text. Now every image is an SF Symbol set on the `NSMenuItem` (`main.rs` `decorate`), and labels carry no glyphs. State symbols follow the app window's state system (`overview.js` `STATES`, `app.css` `.tone-*`). Needs review is `circle` (cooling down `hourglass`, re-review `arrow.clockwise.circle`). In review is `ellipsis.circle`. Ready for approval is `checkmark.circle`, approved `checkmark.circle.fill`, changes requested `exclamationmark.circle.fill`, commented `text.bubble`, skipped `minus.circle`, merged `arrow.triangle.merge`, closed `xmark.circle`, failed `exclamationmark.triangle.fill`, and brief `doc.text`. Ok, warning, error and merged tones take the dynamic system green, orange, red and purple, so they hold up in light and dark. Neutral and in-progress symbols stay template images, because the menu's selection highlight is the accent colour and an accent-tinted symbol would vanish on it. Each top-level command also gets a symbol, so the image column is never ragged. If a symbol fails to load, a monochrome text fallback is prefixed instead.
- **History ▸ follows Settings…** in the app section.
- **A one-line header.** The headline carries its timing: "2 PRs need attention · checked 2m ago · next in 13m". While paused, the countdown takes the "next" slot ("· paused, 1h 12m left"), so the separate ⏸ line is gone. The pause submenu's title still states the countdown. "No PRs tracked yet" becomes the headline. The header symbol follows the menu bar glyph's precedence (pause, reviewing, attention, all clear), so the header explains the glyph. A second line appears only for something extra: the last action's outcome (its ✓/⚠ prefix becomes the symbol) or a daemon health problem. Only a health problem together with a message makes three lines.
- **No About submenu.** A healthy daemon costs no row. The diagnostics (app and daemon version, pid, uptime, PR / reviewing / queued counts) are the headline's ⌥ alternate. "Show daemon log" is the ⌥ alternate of Open PRX (⌥⌘O), and "Open config.yml" the ⌥ alternate of Settings… (⌥⌘,). An unhealthy daemon gets a header line whose click is the fix: "config.yml not loaded: … — Open…" opens the raw config, and "Daemon 0.0.9 is running, app is 0.1.0 — Show log" opens the log. Offline, one line says what is wrong and why (for example "Daemon restarting · failure 3 of 5, retry in 4s"), followed by Start daemon, Show daemon log, Open PRX and Quit.

## Round 3 (2026-10-10, pause under review mode)

The user's view: "pause would come under review mode only". A pause is a temporary hold on the mode, not a peer of it, so the two submenus become one. This supersedes **Pause vs mode** above.

- **One submenu, both states in the title.** "Review mode: Review and comment ▸", or "Review mode: Monitor · paused, 1h 12m left ▸" while paused. The symbol is `slider.horizontal.3` when active and the pause bars (`pause.circle`) while paused or off, matching the menu bar glyph. Off wins the title as it wins the headline, so a pause under Off reads "Review mode: Off".
- **Inside, mode first.** "Default for repos without their own mode", the three radio rows with their one-line meanings, and "Per-repo modes…" are unchanged.
- **Then the pause section,** after a separator: "Pause automatic reviews" (with the pause symbol), "Holds automatic reviews; manual reviews still run; resumes by itself", then For 30 minutes / 1 hour / 2 hours / 4 hours. The two explanation lines became one.
- **While paused,** the section opens with the countdown "Automatic reviews paused · 1h 12m left" and "Resume now". "Pick a time to restart the pause from now" sits above the durations, which stay enabled.
- **In mode Off,** the section stays visible and its durations are disabled under "Nothing to pause while the review mode is Off". Disabled reads clearer than hidden: the controls stay where the user expects them, and the reason says why they are inert. A pause that is still running can be resumed.
- **IDs unchanged.** `pause:30m|1h|2h|4h`, `resume` and `mode:*` dispatch as before. The menu no longer emits `pause` (the old submenu parent); `actions.rs` keeps it in the submenu-hint arm.
- **Header unchanged.** The headline keeps its "· paused, 1h 12m left" suffix.
- **App window not changed.** It still shows Pause as its own section next to the Review mode cards.

---

# Part I — App window and menu (2026-10-08)

**Method.** This audit used the `/impeccable` critique. Two independent assessments ran:

- **Design review.** Every page was rendered through `settings-ui-tests/preview-server.js` with the fake daemon, in light and dark. The states covered were the defaults, `?empty`, `?offline`, `?broken`, `?norepos`, `?paused`, `?nousage`, a dirty form, and expanded rows (86 screenshots at 1000×700).
- **Mechanical evidence.** This combined the `impeccable detect` CLI scan, the in-page detector overlay, computed-style WCAG contrast in both themes, a keyboard tab walk, a dark-mode leak scan, and a hard-coded colour grep.

**Severity.** P0 blocks the core task. P1 is a significant usability or trust problem. P2 is friction or an inconsistency. P3 is polish.

## Summary

| | |
|---|---|
| Nielsen heuristics | **23 / 40** (status 2, real-world match 2, control 2, consistency 2, error prevention 2, recognition 2, efficiency 3, minimalism 3, recovery 2, help 3) |
| Detector (CLI) | 0 findings |
| Detector (in-page) | 8 flags. Mostly false positives (table cells, inset rows, fixed-height chips). Real: 11px axis/detail text on Usage, and the thin-border + wide-shadow picker popover. |
| Contrast | Passes in both themes, with one exception: `.choice-detail` on a selected choice in dark is **4.49:1**. Disabled Save/Revert falls to about 2:1, which is exempt but reads too faint in dark. |
| Focus | A global `:focus-visible` ring (app.css:82) makes every tab stop visible. There is no skip link, so 14 sidebar stops come before the content. |
| Dark mode | No light-surface leaks on any page. |
| Tokens | 62 of 63 colour literals are inside custom-property blocks. The 1 outlier, the `.switch::after` knob `#fff`, is intentional. |

**Verdict on specificity.** The window is a competent, quiet clone of System Settings: grey sidebar, grouped white cards, SF type, one blue accent. The tokens are disciplined and dark mode is clean. What feels authored for PRX is the copy and the domain logic, not the visuals. Examples are the verdict-ratio bar, "7 of 15 changes-requested reviews later approved", and "Top blocking issues". The PR state is the product's core object, yet it has no consistent visual language: dots, pills and plain text disagree across pages. That is the biggest opportunity for task 7.5 and 7.6. Make verdict and state the visual system.

**Strengths to keep.**

1. **Help copy.** It is short, concrete and honest ("Never the token itself.", "History still records each event.").
2. **Override model in Repos & Orgs.** The tree, "Overridden (n) / All settings", the "This repo" badge with Reset, and "Follows the acme org overrides, then the defaults".
3. **Token and theme discipline.** Focus rings and dark mode both work everywhere.

## Cross-cutting

- **P1. State vocabulary and colour drift.**
  - "Changes requested" is a red pill on Dashboard and a grey dot in the PR table.
  - "Approved" is green on one row and grey on another.
  - The menu says "Review comment", the app says "Review and comment".
  - "Ready for your approval" and "Ready for approval" both appear, as do "Pending" and "Needs review".

  *Fix:* one state table (label, colour token, glyph) in `app.css` and `model.rs`, used by the menu, Dashboard, Pull requests and History.
- **P1. Unset optional fields render with nothing selected** (radios and segmented controls on Notifications and Account & Browser), so the effective value is invisible. *Fix:* pre-select the inherited value, marked "Default (Info)".
- **P1. Saving the form strips YAML comments.** The dirty footer warns about this in passing, so a power user loses data silently. *Fix:* write comment-preserving patches, or require a one-time blocking confirm.
- **P2. Brand casing.** "prx" appears in the sidebar header, subtitles and copy. Standardise on "PRX" (spec: visual-identity, Menu IA).
- **P2. No skip-to-content link.** Each page costs 14 Tab presses before reaching its content. *Fix:* a skip link, or ⌘1…⌘9 page shortcuts.
- **P3. Disabled primary button in dark** (about 2.2:1) looks absent rather than disabled. Raise its opacity.

## Dashboard (`pages-dashboard.js`)

- **P1. A broken config is invisible here** (`?broken`). The only signal is a footer line on Settings pages. *Fix:* a red banner above the stat cards, such as "config.yml has an error at repos[acme/api].schedule — Fix", linking to the field.
- **P1. Paused is plain text** ("Paused until 12:29 AM · mode Review and comment") with no Resume control. *Fix:* a banner, "Reviews paused until 12:29 AM", with a Resume button.
- **P2. Empty state (no repos)** shows four zero cards and "Nothing needs you right now". There is no path to add a repo, which the spec requires (visual-identity → *Empty state*). *Fix:* replace the cards with an onboarding card plus an "Add a repo" action that opens Repos.
- **P2. "In review" PRs are listed under "Needs attention"** even though the user can't act on them. Filter them out, or rename the section.
- **P2. Recent activity repeats one PR** as separate "Review started" and "Approved" rows. Collapse to the latest outcome per PR.

## Pull requests (`pages-prs.js`)

- **P0. At the default 1000px window only State, PR and Title are visible.**
  - Author, Age, Last verdict, Last review and Engine sit behind a horizontal scroll.
  - The sticky Actions column covers them.
  - Engine is still clipped ("clau") at 1400px.

  *Fix:* no more than about 5 columns. Merge verdict and findings into the State cell, and move Engine and Last review into the detail view.
- **P1. There is no PR detail view.** The executive summary (soon the brief) exists only as a `title=` tooltip, so the menu's per-PR drill-down is richer than the window. *Fix:* row click or Enter opens an inspector with the brief card (task 4.3), findings, usage and actions.
- **P2. A native `<select>` "More…" is repeated on every row.** It is heavy and not a native action menu. Use one ⋯ button that opens a menu.
- **P2.** The × on the "Queued acme/api#10." note row floats mid-row. Align it with the state cell.
- **P3.** The subtitle "Reviews started here run like the menu bar's" is jargon. Drop it.

## History (`pages-history.js`)

- **P1. Token numbers contradict each other.** The row says "121k in", the expanded detail says "Input 29.8k" (cache folded in on one, excluded on the other). Pick one definition and label cache explicitly.
- **P2. No column headers, and the trailing "verdict · chip · chip · tokens · $" cluster varies in width,** so it scans poorly. Use fixed-width trailing columns.
- **P3.** Titles truncate at a different width on each row.
- **Rename (task 4.3):** "Executive summary" becomes "Brief".

## Usage & stats (`pages-usage.js`, `chart.js`)

- **P1. Totals disagree** in the same way as History. The card says "4M · 3.9M in / 97.1k out", while the Totals table says "Input 1M, Cache read 2.7M". Show in / cache / out the same way everywhere.
- **P1. Three unlabeled segmented controls hold 12 options** (Day/Week/Month, 7/30/90/All, Total/engine/repo/PR/session), more than the five-option limit for one decision point. *Fix:* derive the bucket from the range, and label the breakdown "Group by".
- **P2. The chart legend doesn't match the bars:** grey input/output swatches against blue or per-engine bars, and the output cap is a 2px sliver. Use an opacity pair of the series colour.
- **P2.** 11px axis and `.stat-detail` text (app.css:568, 658). Raise it to 12px or make sure it has high contrast.
- **P3.** The missing-usage warning banner is long, persistent and reads like docs. Shorten it to "6 runs had no usage data — why?"
- **Rename (task 4.3):** "Summary usage" becomes "Brief usage".

## Settings pages

- **General (`pages-defaults.js`).**
  - P2: Save enables only on blur or change, not while typing. Use the `input` event for dirty state.
  - P2: In dark, `.choice-detail` on the selected choice is 4.49:1. Lift `--muted` on `--accent-soft`.
  - P3: "Check GitHub every" has no help text, unlike its neighbours.
  - Add the "Open at login" toggle (task 6.6).
- **Review.**
  - P2: `{{PARENT_PROMPT}}` sits in prose, while Messages offers insertable chips. Reuse the chip pattern.
  - P2: the label column and stacked fields mix two grids on one card.
  - Add the read-only "Always added by PRX" contract section (task 1.5).
- **Messages (`pages-messages.js`).** P2: when unset, "Review body" is an empty textarea. Show the built-in body as a placeholder or preview, so users see what gets posted under their name.
- **Schedule.** P1: nine radio options in one list, with "Custom" below the fold. Group them ("With every poll / Hourly ▾ / Weekdays ▾ / Custom cron") and preview the next run time.
- **Notifications.**
  - P1: unset events (for example "Review started") show no selected tier (see Cross-cutting).
  - P2: dim the Events card when Deliver is off.
  - P2: in dark, the selected segment (`#55555b`, app.css:324) is barely distinguishable.
- **Engines.**
  - P2: the claude default shows `--dangerously-skip-permissions` with no explanation. A trust-sensitive flag needs a one-line reason.
  - P2: Claude Code has only "Customize" while the others have "Use for reviews", so button positions shift. Reserve the slot.
- **Repos & Orgs (`pages-repos.js`).**
  - P1: a repo with a broken config entry isn't flagged in the tree.
  - P2: the `.*` regex glyph and the bare count badges ("2") are unexplained. Use the text "2 overrides" plus tooltips.
  - P2: the picker popover's 1px border + 28px shadow (app.css:439) is heavier than a native menu.
- **Menu bar.** P3: the help text breaks the template the other pages use ("These apply everywhere…"). Acceptable, but it reads as a bug.
- **Account & Browser.** P1: "Open PRs automatically" shows no selection when unset. The browser picker (tasks 5.2 and 5.3) replaces the free-text browser field.
- **config.yml (raw).** P1: "Save file" and the global footer's "Save ⌘S / Revert / Reload from disk" are two save systems on one page. Hide the global footer here.

## Menu bar menu (`model.rs`, `banners.rs`)

- **P1. Footer order and labels miss the spec.**
  - Current (model.rs:558–573): `last poll`, `Pause ▸`, `Review now`, `Review PR…`, `Open prx…`, `Settings…`, `History ▸`, `Quit prx`.
  - Target (design V3): Open PRX, Review PR…, Settings…, pause controls, Quit PRX.
  - Drop the ellipsis from "Open PRX", since it takes no further input.
  - Rename "Review now", which is ambiguous next to "Review PR…", to "Check GitHub now".
  - Move History into the window, or next to Open PRX.
- **P1. Debug data is shown as UI.** "daemon: up · pid 1234 · v1.1" (model.rs:482) and "last poll: 2026-10-08 18:13:00 UTC" (model.rs:555). *Fix:* a status header such as "Checked 2 min ago · next in 13 min" in local time, with pid and version behind an ⌥ alternate.
- **P1. Lowercase brand in headlines and banners:** "prx — all clear", "prx — daemon offline", "prx — reviews stopped". Use "PRX", sentence-case.
- **P1. Mode labels disagree with the app** ("Review off / Monitor / Review comment" vs "Off / Monitor / Review and comment"). Use one vocabulary.
- **P1. Approve and Block post to GitHub as the user from one click, with no confirmation.** The "    ↳ Approve key" row fakes hierarchy with spaces. *Fix:* confirm Block (and Approve on a stale review), and use a real indentation level.
- **P1. The offline menu offers only Refresh and Quit** (model.rs:589–607). Add "Start daemon" and "Open PRX" (task 6.4 also adds "Show log").
- **P2. PR row labels have no length cap.** They run icon · key · author · age — title — summary, and the summary allows up to 200 characters, so the menu gets very wide. Cap the row at about 60 characters and keep the summary/brief in the submenu.
- **P2. Scaling.** Paging (10 per group, then "Show more") lets a 30+ PR menu exceed screen height. Add a "N more in PRX…" item that opens the window.
- **P2. The PR submenu has about 12 items,** four of them Review variants. Group those under "Review ▸".
- **P2. Copy.**
  - Lowercase sentence starts ("no PRs tracked", "all clear").
  - "Un-ignore" / "Un-snooze" should be "Stop Ignoring" / "Unsnooze".
  - "issue(s)" / "nit(s)" in banners (banners.rs:122–126) should use real plurals.
  - "review failed — check agent and logs" doesn't say where the logs are.

## Persona red flags

- **First-timer:**
  - The offline page says "Start it with `prx daemon start`", a CLI command inside a GUI.
  - The empty Dashboard has no setup path.
  - Schedule has nine choices.
  - `--dangerously-skip-permissions` is shown without explanation.
- **Power user:**
  - Saving strips YAML comments.
  - Verdicts sit off-screen in the PR table.
  - The two token totals disagree.
  - There is no keyboard path through PR rows (no ↑/↓ + Enter to open), and no PR inspector.

## Suggested order for 7.5–7.7

1. **Tokens (7.5).** Add a single state/verdict table (colour + glyph + label) as tokens, fix the dark `.choice-detail` and disabled-button contrast, and set a 12px floor.
2. **Pull requests and Dashboard (7.6).** The PR inspector with the brief card, a ≤5-column table, empty/paused/broken banners.
3. **Usage and History (7.6).** One token-accounting definition, labeled controls.
4. **Settings (7.6).** Show effective defaults, the Schedule grouping, one save system on config.yml.
5. **Menu (7.7).** Footer order, the status header in local relative time, PRX casing, the offline actions.

## Audit pass after 7.5–7.7 (2026-10-09)

**Method.** The `/impeccable` audit: the `impeccable detect` scan of `settings-ui/` (0 findings), a computed-style WCAG contrast sweep in WebKit over every page plus the `?broken`, `?paused`, `?empty` and `?offline` states in light and dark (no text below AA, with the system accent active), a colour-literal grep of `app.css` (none outside the `:root` token blocks), a keyboard check of the skip link and the row action menu, and screenshots of every changed page in both themes. **No blocking issues remain.**

| Audit item | Status |
|---|---|
| P0 PR table hides columns behind a horizontal scroll | Fixed: State (pill + findings), Pull request (title, key, author), Age, Last review, actions. Engine and other facts are in the PR detail. |
| P1 State vocabulary and colour drift | Fixed: one state table (`overview.js` STATES, `.tone-*` tokens, shape glyphs) on Dashboard, Pull requests, History and the verdict mix; the menu uses the same labels ("Needs review", "Ready for approval", "Review and comment", "Commented"). |
| P1 Unset optional fields show nothing selected | Fixed: unset fields show the effective default (GET /config `effective`, else the schema's built-in value), marked "Not set, so PRX uses the default". |
| P1 History vs Usage token totals disagree | Fixed (display only): "tokens" is always every kind; breakdowns name input, cache and output separately; History's detail adds a Total that matches its row. |
| P1 Dashboard: broken config invisible, Paused has no Resume | Fixed: banners with "Open config.yml" and "Resume reviews" (new `pause.resume` bridge call). |
| P1 Usage: three unlabeled segmented controls | Fixed: "Range" and "Group by"; the bucket follows the range. |
| P1 Schedule: nine radios | Fixed: With every poll / On a schedule (one menu) / Custom. |
| P1 Repos: broken entry not flagged | Fixed: the tree flags the entry the config error names. |
| P1 config.yml: two save systems | Fixed: the global save bar is hidden on that page. |
| P1 Menu footer order, debug data, casing, mode labels | Fixed (task 7.7): status header with relative check times, pid/version behind ⌥, V3 footer order, sentence case, the app's mode labels. |
| P1 Approve/Block with no confirmation, fake indent | Fixed: both ask first (NSAlert); the inline Approve uses `indentationLevel`. |
| P1 Offline menu offers only Refresh and Quit | Fixed: Start daemon / Show log (task 6.4) plus Open PRX. |
| P1 Saving the form strips YAML comments | Open (out of scope for 7.x; core config writer). |
| P2 fixed | Empty Dashboard onboarding; Recent activity one row per PR; ⋯ row menu; History fixed trailing columns; chart legend colours; 12px text floor; dark `.choice-detail` and disabled-button contrast; Save enables while typing; Deliver off dims Events; trust note on unattended agent flags; reserved action slot on Engines; popover surface; "2 overrides" labels; skip link; brand casing; menu row caps and plurals. |
| P2/P3 deferred | In-review PRs under "Needs attention"; Messages built-in body preview; ↑/↓ through PR rows; menu "N more in PRX…" and a "Review ▸" submenu; Menu bar page help template. |

---

# Part II — Spec audit (2026-09-03)

**Date:** 2026-09-03
**Scope:** `docs/internal/spec.md` (draft, 2026-08-20)
**Verdict:** The philosophy and feature set are sound and well-scoped. Three blockers must be resolved before implementation; they share one common fix (§ "Recommended resolution" below). The rest are gaps that can be folded into a spec revision.

---

## Blockers (must resolve before implementation)

### A1 — Agent approval contradicts the stated human-approval philosophy

**Spec refs:** §1.1 ("Manual approval authority — AI informs, human decides"), §1.2 (non-goal: auto-approval), §3.5 (Approve action), §4.3 (default prompt)

The default review prompt instructs the agent: *"If no blocking issues, approve."* The agent runs under the user's `gh` session, so this posts a GitHub approval **as the user** — which is exactly the auto-approval the spec rules out. It also breaks the menu bar "Approve" action: `gh pr review --approve` fails when the same account has already approved the PR.

**Fix:** the agent must never post approvals. Its verdict ("good to merge") is informational; the human's Approve action is the only path to a GitHub approval. An explicit `agent_may_approve` opt-in can be considered later.

### A2 — The default review prompt is technically infeasible as written

**Spec refs:** §3.2 ("Reviews post inline comments on the PR via `gh pr review`"), §4.3 ("Post inline comments using `gh pr review`")

`gh pr review` can only submit a single review with a body (`--approve | --request-changes | --comment`, plus `--body`). It **cannot create line-anchored inline comments**. Inline comments require the REST API (`POST /repos/{owner}/{repo}/pulls/{n}/comments` with `commit_id`, `path`, `line`) via `gh api`, or the equivalent GraphQL mutation.

**Fix:** either the prompt must teach the agent to use `gh api` REST calls, or — better — the agent should not post to GitHub at all (see A3 / recommended resolution).

### A3 — No machine-readable output contract for reviews

**Spec refs:** §3.3 (one-line verdict, blocking vs nit counts), §3.7 (history fields: verdict, blocking count, nit count, time taken), §5.2 (history log)

Nearly every user-visible feature depends on structured review results — the one-line verdict ("3 nits, no blockers"), the blocking/nit counts, history analytics, and the state machine's Approved-vs-Changes-Requested branch. The spec only says the agent posts a review via `gh`; deriving structure from scraped prose is fragile and won't support insights reliably.

**Fix:** define a review manifest as the machine-facing artifact: the agent writes a JSON file (or stdout sentinel) with `{ verdict, summary, findings: [{ file, line, severity: "blocking"|"nit", body }], duration }`. The posted GitHub review is the human-facing artifact; the manifest is the source of truth for state, menu, and history.

### Recommended resolution for A1–A3 (single architecture change)

**"Agent analyzes; prx writes."**

- The agent is **read-only**: it fetches the diff and repo standards (`gh pr diff`, `gh pr view`, AGENTS.md, …) and produces the manifest. It never touches `gh pr review`, never approves, cannot merge.
- **prx** validates the manifest, posts inline comments via `gh api` (REST), and submits one review with the verdict in the body (`request-changes` when blocking findings exist, `comment-only` otherwise; approve only ever via the human's menu action).

This resolves all three blockers at once: approvals stay human (A1), the `gh pr review` inline limitation is bypassed (A2), and the manifest becomes the single structured source of truth (A3). It also shrinks the prompt-injection blast radius (see A13).

---

## High-priority gaps

### A4 — CLI ↔ daemon interaction is undefined

**Spec refs:** §3.8, §6

`prx review/approve`, and the menu bar's "Review now", need to command a *running* daemon. If the CLI mutates `state.json` directly while the daemon polls, state races and reviews double-fire.

**Fix:** the daemon exposes a small local API (localhost TCP with a port file + auth token, or a Unix domain socket); CLI and menu bar are thin clients; **all** state writes and GitHub writes flow through the daemon.

### A5 — `New` vs `Cooling Down` are redundant; cooldown anchor undefined

**Spec refs:** §2.1, §2.2

Both states mean "waiting out the cooldown", and the transition list implies a PR is detected as `New` then demoted to `Cooling Down` on the next tick. Also undefined: is `before_first_review` measured from PR **creation** or **detection**? For a 3-day-old PR discovered on the first poll, detection-based cooldown would delay a review for no reason.

**Fix:** one pending state (or a crisply defined split), cooldown measured from GitHub's PR creation timestamp (so pre-existing PRs review immediately), cooldown clock for re-reviews starting at new-commit detection.

### A6 — `Skipped` overloads four different reasons; "un-ignore" is missing

**Spec refs:** §2.1, §2.2, §3.5

Skipped conflates: draft, config-excluded, snoozed (temporary, restores previous state), ignored (permanent until reversed). These have different exits, but §5.1's state file has no `skip_reason` or `previous_state`. §3.5's Ignore description says "until manually un-ignored", yet no un-ignore action exists in any menu.

**Fix:** add `skip_reason` + `previous_state` to the state schema; add Un-ignore to the per-PR submenu.

### A7 — Poll interval makes the cooldowns pointless

**Spec refs:** §3.1 (default poll: every 2h), §2.3 (60 min / 30 min cooldowns)

With a 2-hour poll, a 60-minute first-review cooldown almost never elapses "in between" — the poll dominates latency, and "proactive review" becomes "reviewed up to 2h late". One `gh pr list` per repo is cheap.

**Fix:** default poll of 10–15 minutes with jitter; keep cron as the advanced per-repo option.

### A8 — In-flight and edge-case transitions are unspecified

Enumerate and define: new commits pushed **while** the agent is reviewing (let it finish, then queue re-review); PR merged/closed **while** In Review (abort review, mark terminal); re-review triggered while another review is queued (global work queue, max N concurrent — §8.1's concurrency setting should be V1, not advanced); PR **reopened** after Closed (currently terminal); draft → ready-for-review flip; PRs accumulated while the daemon was stopped (handled by creation-time cooldowns from A5).

### A9 — Snooze "until next push" isn't wired into the state machine

The snooze option exists (§3.4), but the transitions (§2.2) don't say what new commits do to a snoozed PR. Intent is clearly "wake up on push": define `Skipped(snoozed, until_push)` + new commits → wake, treat as re-review pending.

---

## Medium issues

- **A10** — §4.2 defines `on_new_commits` notification key; the §3.6 event table has no matching row. Align the two.
- **A11** — Drafts: §3.1 says "filter out" (never seen), §2.1 lists drafts under `Skipped` (tracked). Pick one; recommended: drafts are tracked and shown under Skipped (cheap visibility), with a config flag to hide entirely.
- **A12** — §3.2 says a per-repo prompt "inherits from default, can extend or replace" but inheritance mechanics are undefined. Simplest contract: override replaces, and a `{{PARENT_PROMPT}}` placeholder splices in the default.
- **A13** — Prompt injection & attribution. PR content can contain instructions aimed at the reviewer agent, and the agent operates with the user's `gh` identity. The analyze-only architecture (A1–A3 fix) removes approve/merge capability, but the spec should also: instruct the agent to treat PR text as data, mark the posted review body with an "automated review by prx" footer (teammates currently see reviews as the user), and document the residual risk. Optional hardening: a scoped fine-grained PAT for prx's writes instead of the full user session.
- **A14** — No diff-size guard in V1. §8.3's size threshold is "advanced", but agents have real context limits — V1 needs a hard cap (skip + `too-large` note + informational notification).
- **A15** — Logging isn't specified anywhere. Daemon needs rotating logs (`~/.local/share/prx/logs/`) and a `prx daemon logs` shortcut; "self-healing" (§11) depends on diagnosing failures.
- **A16** — Single-instance daemon guard (pid file / launchd is the source of truth) so `daemon start` is idempotent.
- **A17** — §8.4's 90-day retention needs mechanics: compaction of `history.jsonl` on daemon start.
- **A18** — Insights "most common blocking issues (categorized)" needs a category taxonomy. V1: store raw finding text + counts; categorization (keyword or agent-derived tags in the manifest) deferred to V2.
- **A19** — Config hygiene: `~` expansion in YAML path values; case-insensitive label matching; `wip` as exact match vs prefix — define.
- **A20** — Menu scaling: define collapsing for Reviewed/Skipped groups once tracked PRs grow.

## Low / notes

- **A21** — Menu bar, `Basso` sound, launchd login item make this macOS-only in practice; state "V1: macOS" explicitly in the spec.
- **A22** — Define force semantics: `prx review <repo>#<n>` bypasses cooldown; `prx review --all` respects cooldowns (only due items).
- **A23** — Force-pushes: head-SHA comparison handles them naturally; worth one sentence.
- **A24** — Fork PRs: `gh pr diff/view` work fine; note that checkout-based flows are avoided (diff-only review helps here too).

## What's strong (keep as-is)

Crisp non-goals; the human-decides philosophy; per-repo override model; tiered notifications with per-event mapping; append-only JSONL history; the defaults-with-rationale table; delegating all auth to `gh`; success criteria that are actually testable.
