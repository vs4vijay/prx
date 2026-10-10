# Token usage

Every review run records the tokens it consumed when its engine reports them, in the history entry
(`usage`, plus `summary_usage` for the on-demand [brief](briefs.md) run that followed it). A brief
run requested on its own (`prx brief --regenerate`, the menu, `POST /brief`) is a separate `brief`
history event with its usage; both count as brief usage (the `summary` split). Where the
counts come from:

- **`claude`** (built-in, keyed on the command's basename): the JSON result's
  `usage.input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`,
  `total_cost_usd`, `session_id`, and the model from `modelUsage` (the costliest when several ran).
- **`codex`** (built-in): the `usage` of every `turn.completed` event of `exec --json`, summed, and
  the `thread_id` as the session.
- **`llm_api`**: the provider response's `usage` — Anthropic's `input_tokens` / `output_tokens` /
  cache read and creation fields; OpenAI's `prompt_tokens` / `completion_tokens` /
  `prompt_tokens_details.cached_tokens`, plus `usage.cost` when an OpenAI-compatible gateway
  reports one.
- **Any other agent** (e.g. `pi`): nothing, unless you map it with `usage_output`.

Token semantics are the same for every engine: `input_tokens` is **uncached** input,
`cache_read_tokens` / `cache_write_tokens` are cache reads / writes, so input + output + cache
fields is the run's total with nothing counted twice. Engines that report cached tokens as a
*subset* of their input (codex `cached_input_tokens`, OpenAI `cached_tokens`) are normalized at
capture: `input_tokens = reported input − cached`, `cache_read_tokens = cached`. Cost is recorded
only when the engine reports it — prx never estimates prices.

Usage is best-effort: a missing or malformed value makes that run's usage *unknown* (never zeros),
and the review itself is unaffected. A failed run still records what its attempts reported, and a
retried run counts every attempt. `prx history` shows each run's review + brief usage on its row
(`12.3k in / 1.1k out / 40.0k cached · $0.41`; nothing when unknown), and [`prx usage`](../reference/cli.md) and the window's *Usage & stats* page aggregate
it.

For your own agent CLI, `usage_output` maps the fields to **dotted JSON paths** in its stdout (one
JSON document, or JSON Lines — each field is read from the last line where its path resolves). An
explicit mapping replaces the built-in parsing for that engine; map `input_tokens` to an uncached
count to keep the semantics above:

```yaml
engines:
  omp:
    type: agent
    command: omp
    args: ["--json", "-p", "{{PROMPT}}"]
    usage_output:
      input_tokens: stats.usage.input
      output_tokens: stats.usage.output
      cost_usd: stats.cost
      session_id: session.id
```

An unknown field name or a malformed path (`a..b`) is a validation error, and so is `usage_output`
on an `llm_api` engine (prx reads its usage from the API response).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../media/screenshots/usage-dark.png">
  <img src="../media/screenshots/usage.png" alt="The Usage and stats page in PRX.app: total tokens, reported cost and runs for the last 30 days, a note about runs without usage data, and a bar chart of tokens per day">
</picture>

The CLI side is [`prx usage`](../reference/cli.md#prx-usage); the API side is `GET /usage` in the
[IPC API](../reference/ipc-api.md).
