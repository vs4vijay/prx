# Insights

`prx insights [--repo org/repo] [--days N]` computes, from `history.jsonl`:

- **Reviews performed** — totals, failures, skips, 7-day / 30-day rollups, and a per-day series
  (daily / weekly / monthly view).
- **Average review → merge time** — measured from each PR's *latest* review before a merge; PRs
  that merged without a review are not samples.
- **Most common blocking issues** — the raw finding texts (no categorization in V1), grouped
  by normalized text so casing/whitespace variants aggregate; per-repo top lists included.
- **Per-author patterns** — "author X's PRs usually pass": verdict distribution, approval rate, and
  the median blocking count across the author's reviews.
- **Per-repo patterns** — the same distribution per repository.
- **Review effectiveness** — the share of `changes_requested` reviews that were **followed by an
  `approved` review on the same PR**, i.e. blocking issues were subsequently fixed.

History entries carry the raw blocking finding texts (truncated to ~120 chars each) so insights can
surface recurring blockers; entries written before this field existed simply contribute counts.

The JSON shape is documented under [`GET /insights`](../reference/ipc-api.md#get-insights-payload).
