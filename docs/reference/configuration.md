# Configuration reference

Config lives at `~/.config/prx/config.yml` (override the path with `PRX_CONFIG`). It is YAML.
`prx config show` prints the fully merged effective view; `prx config edit` opens it in `$VISUAL`
(or `$EDITOR`); `prx config set <dotted.key> <value>` edits a single value and validates before
saving. Org entries under `orgs:` and repo entries under `repos:` override individual keys only —
everything else inherits (see [Org and repo overrides](#org-and-repo-overrides)). On macOS the
[settings app](../guides/prx-app.md#settings) edits the same file through the daemon. A running daemon picks up
every change by itself — see [Live reload](#live-reload). Every rewrite saves the current key
names, so a config still using the legacy `agents`/`agent` keys comes back as `engines`/`engine` —
[back it up](../guides/engines.md) first if you may run an older prx binary against it.

Guides for the larger topics: [review modes](../guides/review-modes.md),
[schedule](../guides/schedule.md), [prompts](../guides/prompts.md),
[commentary](../guides/commentary.md), [briefs](../guides/briefs.md),
[engines](../guides/engines.md), [browser, identity, and Block](../guides/browser-identity-block.md),
[auto-open](../guides/auto-open.md), [notifications](../guides/notifications.md).

## Keys

| Key | Default | Meaning |
|-----|---------|---------|
| `defaults.engine` | `pi` | The review engine: a key into the [`engines`](../guides/engines.md) map (built-in agent engines: `pi`, `claude`, `codex`). The legacy key `agent` is still accepted as an alias. |
| `defaults.agent_model` | *(unset)* | Model handed to an **agent** engine through its `model_args` (e.g. `opus`, `gpt-5`). Unset = the agent's own default. Ignored by agents without `model_args`, and ignored (with a warning) for `llm_api` engines, which carry their own `model`. |
| `defaults.poll_interval_minutes` | `15` | Per-repo poll cadence; the daemon adds ±10% jitter per repo. |
| `defaults.review_mode` | `review comment` | `off`, `monitor`, or `review comment` — see [Review mode](../guides/review-modes.md). |
| `defaults.schedule` | *(unset)* | Cron expression gating when reviews enqueue — see [Review schedule](../guides/schedule.md). Unset = review whatever is due on every poll. |
| `defaults.cooldown.before_first_review_minutes` | `60` | Wait after the PR's **GitHub creation time** before the first review. |
| `defaults.cooldown.between_re_reviews_minutes` | `30` | Wait after a **new-commit detection** before a re-review. |
| `defaults.review_prompt` | `default` | The review prompt **text** (`default` = the built-in prompt). Not a file path — see [Prompts](../guides/prompts.md). |
| `defaults.extra_prompt` | *(unset)* | Instructions appended to every review prompt — see [Prompts](../guides/prompts.md). |
| `defaults.max_concurrent_reviews` | `1` | Cap on reviews running simultaneously (global FIFO queue, oldest-due-first). Sized at daemon start: the effective cap is the largest value across defaults and repos. |
| `defaults.diff_max_files` | `50` | Diff-size cap. Larger diffs are skipped with a `too_large` note — no agent run, no GitHub writes — and re-checked on later polls. |
| `defaults.diff_max_lines` | `4000` | Second cap (files changed vs. additions+deletions). |
| `defaults.agent_timeout_seconds` | `600` | Wall-clock budget per agent run; the process is killed on expiry and the run retried once. |
| `defaults.skip.drafts` | `true` | Draft PRs are *tracked* under `skipped (draft)`, not invisible. |
| `defaults.skip.authors` | `[]` | Exact author logins to skip (e.g. `dependabot[bot]`). |
| `defaults.skip.labels` | `["wip", "do-not-review"]` | Case-insensitive exact label match. |
| `defaults.skip.branches` | `[]` | Glob patterns on the head branch. |
| `defaults.notifications.enabled` | `true` | Master switch: `false` stops every banner and sound (daemon and menu bar); state, history, and the menu keep updating. **Global** — rejected under `orgs:`/`repos:`. |
| `defaults.notifications.*` | see below | Event → tier mapping; the other keys ARE the event list. |
| `defaults.browser` | `system` | What opens PR URLs: `system`, a macOS app name or path, or an argv list containing `{{URL}}`. |
| `defaults.identity.login` | *(gh user)* | The GitHub login prx reviews as (used to find its own request-changes review on Unblock, and as `{{REVIEWER_LOGIN}}` in prompts). |
| `defaults.identity.credential` | *(unset)* | Name of an **environment variable** holding the token for every GitHub write. |
| `defaults.identity.attribution_footer` | *(unset)* | Footer line that ends posted review bodies, replacing the built-in `— automated review by prx` (e.g. `Ping @you`). |
| `defaults.block_message` | `Changes requested — blocked from prx pending a human follow-up.` | Body of the request-changes review the human **Block** action posts (must not be blank). |
| `defaults.approve_message` | *(unset)* | Body posted with the human **Approve** action. Unset = a bare approval. |
| `defaults.review_heading` | ``## prx review of `{{PR}}` — {{VERDICT}}`` | First line of every posted review; `{{PR}}` is `org/repo#n`, `{{VERDICT}}` the verdict phrase. Must not be blank when set. |
| `defaults.comments.inline` | `true` | Post located findings as inline PR comments; `false` keeps them in the review body only — see [Commentary](../guides/commentary.md). |
| `defaults.comments.template` | *(built-in body)* | Review body template with a closed placeholder set, validated at load — see [Commentary](../guides/commentary.md). |
| `defaults.brief.enabled` | `true` | Keep a plain-language [brief](../guides/briefs.md) per PR (from the review manifest, else generated on demand). Formerly `executive_summary`, still read as an alias. |
| `defaults.brief.engine` | *(the review engine)* | Engine that writes on-demand briefs; any `engines` entry (point it at a cheap `llm_api`). |
| `defaults.brief.timeout_seconds` | `120` | Budget for an on-demand brief run (separate from `agent_timeout_seconds`). |
| `defaults.auto_open` | `off` | Open the PR in the effective `browser` when a review completes: `off`, `approved`, `blocking`, or `both` — see [Auto-open](../guides/auto-open.md). |
| `notification_tiers.<tier>.banner` | see below | Whether the tier raises a desktop banner. |
| `notification_tiers.<tier>.sound` | see below | Sound name for the tier (`null` = silent). |
| `engines.<name>.type` | — | `agent` (a local CLI) or `llm_api` (prx calls an HTTP API). Entries override the built-ins `pi`, `claude`, `codex` by name. The legacy `agents:` map is still read (as `type: agent`). See [Engines](../guides/engines.md). |
| `engines.<name>.command` | — | *agent:* the binary (use an absolute path for [autostart](../guides/autostart.md)). |
| `engines.<name>.args` | — | *agent:* argv template; `{{PROMPT}}` is substituted, a bare `-` arg means the prompt is piped on **stdin**. |
| `engines.<name>.model_args` | — | *agent:* extra argv appended when `agent_model` is set; `{{MODEL}}` is substituted. Built-ins: `--model {{MODEL}}` (`pi`, `claude`), `-m {{MODEL}}` (`codex`). |
| `engines.<name>.session_output` | `stdout_prefix: "prx-session:"` | *agent:* how a run reports its session id — exactly one of `stdout_prefix` or `json_field`. |
| `engines.<name>.resume_args` | `[]` | *agent:* argv appended to resume the PR's previous session; must contain `{{SESSION_ID}}`. Empty = no resume. |
| `engines.<name>.usage_output` | *(built-in for `claude`/`codex`)* | *agent:* map of token-usage fields (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `cost_usd`, `model`, `session_id`) to dotted JSON paths in the agent's stdout (e.g. `usage.input_tokens`). Overrides the built-in parsing; rejected on `llm_api` engines — see [Token usage](../guides/token-usage.md). |
| `engines.<name>.provider` | — | *llm_api:* `anthropic`, `openai`, or `openai_compatible`. |
| `engines.<name>.base_url` | per provider | *llm_api:* `https://api.anthropic.com` / `https://api.openai.com/v1`; **required** for `openai_compatible`. Must be `http(s)://`. |
| `engines.<name>.model` | — | *llm_api:* model id (required). |
| `engines.<name>.api_key_env` | — | *llm_api:* **name** of the environment variable holding the API key (required). Inline keys (`api_key`, `token`, …) are rejected. |
| `engines.<name>.temperature` / `max_tokens` | *(provider default)* | *llm_api:* optional sampling parameters (Anthropic requires `max_tokens`; prx sends `8192` when unset). |
| `menu.sort` | `created_desc` | Order of PR rows in the menu bar (and the initial sort of the window's *Pull requests* page): `created_desc` (newest PR first), `created_asc` (oldest first), `last_review_desc` (most recently reviewed first; never-reviewed PRs last), or `key` (repo name, then PR number numerically — `#9` before `#10`). Ties fall back to `key`. **Global** — rejected under `orgs:`/`repos:`. See [PRX.app](../guides/prx-app.md). |
| `menu.group_by` | `lifecycle` | How the menu groups PR rows: `lifecycle` (the state groups), `repo` (one group per repo, by name), or `none` (one flat *Pull requests* list). **Global**. |
| `orgs[].org` | *(required)* | Org slug; the entry applies to every watched repo named `<org>/…`. |
| `orgs[].*` | *(inherit)* | Any `defaults` key above (partial: set only what differs). |
| `repos[].name` | *(required)* | `org/repo`. |
| `repos[].*` | *(inherit)* | Any `defaults` key above: `engine` (or legacy `agent`), `agent_model`, `poll_interval_minutes`, `review_mode`, `schedule`, `review_prompt`, `extra_prompt`, `max_concurrent_reviews`, `diff_max_files`, `diff_max_lines`, `agent_timeout_seconds`, `cooldown.*`, `skip.*`, `notifications.*` (tiers only — not `enabled`), `browser`, `identity.*`, `block_message`, `approve_message`, `review_heading`, `comments.*`, `brief.*`, `auto_open`. |

## Notification events and tiers

Default event → tier mapping (every event can be retiered):

| Event key | Default tier |
|-----------|--------------|
| `on_new_pr` | `informational` |
| `on_review_started` | `informational` |
| `on_review_complete_approved` | `informational` |
| `on_review_complete_blocking` | `urgent` |
| `on_new_commits` | `informational` |
| `on_re_review_triggered` | `informational` |
| `on_merged` | `silent` |
| `on_closed` | `silent` |
| `on_failure` | `urgent` |
| `on_skipped` | `silent` |

Default tier behavior: `urgent` = banner + `Basso` sound, `informational` = banner, no sound,
`silent` = menu bar update only. Delivery is best-effort (`osascript` on macOS, `notify-send` on
Linux) and never fatal. `on_review_started` fires whenever a review begins executing (scheduled or
requested) — retier it to `silent` if you only want to hear about results. To mute everything at
once without retiering, set the global master switch:

```bash
prx config set defaults.notifications.enabled false
prx config set defaults.notifications.on_review_started silent   # or just quiet review starts
```

## Org and repo overrides

```yaml
defaults:
  review_mode: review comment
  skip:
    labels: ["wip"]
orgs:
  - org: acme                     # applies to every watched acme/* repo
    review_mode: monitor
    skip:
      authors: ["dependabot[bot]"]
repos:
  - name: acme/api                # inherits monitor + the org's skip.authors
  - name: acme/web
    review_mode: review comment   # repo beats org
    skip:
      labels: ["wip", "do-not-review"]
```

Effective values resolve **defaults ← org ← repo**: the most specific level that sets a key wins,
and partial structures (`cooldown`, `skip`, `notifications`, `identity`, `comments`,
`brief`) merge field by field — in
the example, `acme/web` posts comments, skips `dependabot[bot]` (from the org), and skips both labels
(its own list replaces the inherited one). `prx config show` and the settings app display the
merged result. An org entry only matters for repos listed under `repos:`. `notifications.*`
overrides decide the tier of that repo's events (daemon notifications and menu bar banners alike).
One caveat: the menu bar's mode selector shows and sets the global `defaults.review_mode`.

## Live reload

The daemon re-reads `config.yml` on its own: it hashes the file every second (and again on
*Check GitHub now* / `prx review-now`), so `prx repos add/remove`, `prx config set`, hand edits, and
settings-app saves apply within about a second — no restart. Added repos are polled right away;
removed repos stop being tracked, their queued reviews are discarded, and their history is kept.
Changed intervals, cooldowns, schedules, prompts, skip rules, modes, tiers, engines, and comment /
summary / auto-open settings apply to the next poll or review (a review already running finishes with the config it started with).

If an edit breaks the file (bad YAML or a failed validation), the daemon keeps running on the
**last good** config and reports the reason as `config_error` in `GET /status` and the settings app;
fixing the file clears it automatically. A menu bar mode switch made while the file is broken applies
to the running config only (answered with `persisted: false` and a "not saved" note) — the file
is left alone, and once fixed its own `review_mode` wins. **`max_concurrent_reviews` does not hot-reload**:
the queue is sized at daemon start, so restart the daemon after changing it.
