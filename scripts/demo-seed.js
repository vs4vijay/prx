#!/usr/bin/env node
// Seed a prx demo sandbox with the fictional team from
// menubar/settings-ui-tests/preview-mock.js, so the CLI and the native app
// tell the same story as the window preview. Run by scripts/demo-sandbox.sh:
//
//   node scripts/demo-seed.js <config.yml> <data dir> <fake gh dir> <agent dir>
//
// Writes config.yml, state.json and history.jsonl (schemas: src/config.rs,
// PrRecord in src/state.rs, HistoryEntry in src/history.rs), the fixture
// files tests/fixtures/gh reads, and the canned manifests the demo agent
// (written by demo-sandbox.sh) answers reviews with. Timestamps are relative
// to now: run it again for a fresh "today".
"use strict";
const fs = require("node:fs");
const path = require("node:path");
const mock = require("../menubar/settings-ui-tests/preview-mock.js");

const [configPath, dataDir, ghDir, agentDir] = process.argv.slice(2);
if (!agentDir) {
  console.error("usage: demo-seed.js <config.yml> <data dir> <fake gh dir> <agent dir>");
  process.exit(2);
}
const seed = mock.seed;
const NOW = mock.NOW;
const MIN = 60e3;
const iso = (ms) => new Date(ms).toISOString().replace(/\.\d{3}Z$/, "Z");
const write = (file, text) => { fs.mkdirSync(path.dirname(file), { recursive: true }); fs.writeFileSync(file, text); };
const json = (value) => JSON.stringify(value, null, 2) + "\n";

// ---- config.yml -------------------------------------------------------------
// The mock's config, with the brief engine kept off the network: its key
// variable is a placeholder the sandbox env sets, and its base URL is a
// closed local port, so nothing in the sandbox can call a real LLM API.
const config = mock.call("config.get").raw;
config.engines.cheap.base_url = "http://127.0.0.1:9";
// "Arc" in the preview; here whatever the machine's default browser is, so
// `prx doctor` has nothing to warn about on a machine without Arc.
config.defaults.browser = "system";
write(configPath, seed.toYaml(config).replace(/^#.*\n/, "# prx configuration (demo sandbox: a fictional team)\n"));

// ---- per-PR files (what `gh pr view --json files` and `gh pr diff` answer) --
const FILES = {
  "acme/api#412": [["src/middleware/rate_limit.rs", 148, 0], ["src/routes/public.rs", 22, 6], ["config/limits.toml", 14, 0], ["tests/rate_limit.rs", 96, 0]],
  "acme/api#409": [["src/session/store.rs", 164, 121], ["src/session/redis.rs", 203, 0], ["migrations/0142_copy_sessions.sql", 18, 0], ["tests/session_store.rs", 88, 12]],
  "acme/api#398": [["src/graphql/gateway.rs", 412, 0], ["src/graphql/schema.graphql", 190, 0]],
  "acme/web#1288": [["app/checkout/summary.tsx", 54, 9], ["lib/billing/estimate-tax.ts", 61, 0], ["app/checkout/summary.test.tsx", 47, 0]],
  "acme/web#1279": [["components/booking/date-field.tsx", 38, 311], ["app/reports/range-filter.tsx", 27, 204], ["lib/calendar/grid.ts", 0, 388]],
  "acme/web#1270": [["app/api/cart/legacy/route.ts", 0, 214], ["lib/cart/legacy-client.ts", 0, 173]],
  "acme/ui-kit#77": [["src/button/button.tsx", 31, 4], ["src/button/button.css", 44, 0], ["src/button/button.stories.tsx", 28, 0]],
  "acme/billing#164": [["internal/webhooks/stripe.go", 97, 18], ["internal/webhooks/retry.go", 142, 0], ["internal/webhooks/retry_test.go", 118, 0]],
  "acme/billing#159": [["internal/invoice/pdf.go", 73, 29], ["internal/invoice/locale.go", 88, 0]],
  "acme/ios-app#88": [["Sources/Push/TokenStore.swift", 41, 17], ["Tests/PushTests/TokenRotationTests.swift", 64, 0]],
  "acme/ios-app#85": [["Sources/Settings/SettingsView.swift", 236, 0]],
  "acme/infra#231": [["terraform/eks/main.tf", 2, 2], ["terraform/eks/.terraform.lock.hcl", 12, 12]],
  "globex/ingest#58": [["jobs/backfill/runner.py", 62, 21], ["jobs/backfill/offsets.py", 49, 0], ["tests/test_backfill_resume.py", 71, 0]],
  "globex/ingest#61": [["migrations/2026_10_partition_events.sql", 84, 0], ["rollups/nightly.sql", 12, 9]],
};
// What the PR author wrote (`gh pr view --json body`), for on-demand briefs.
function body(row) {
  return "## What\n\n" + row[1] + ".\n\n## Why\n\n" + (row[5].brief || "See the linked ticket.") + "\n\n## Testing\n\nUnit tests added; checked by hand on staging.\n";
}
function diff(files) {
  return files.map(([file, added]) => [
    "diff --git a/" + file + " b/" + file,
    "--- a/" + file,
    "+++ b/" + file,
    "@@ -1,3 +1," + (3 + Math.min(added, 2)) + " @@",
    " // " + path.basename(file),
    added ? "+// changed in this pull request" : "-// removed in this pull request",
    added > 1 ? "+// see the PR description for the reasoning" : " ",
    " ",
  ].join("\n")).join("\n") + "\n";
}

// ---- state.json and the gh fixtures -------------------------------------------
const history = mock.call("history.list", { limit: 100000 }).entries;
const sessionOf = (repo, number) => {
  const run = history.find((e) => e.repo === repo && e.number === number && e.usage && e.usage.session_id);
  return run ? run.usage.session_id : null;
};
const prs = {};
const lists = {};
config.repos.forEach((r) => { lists[r.name] = []; });

seed.prs.forEach((row) => {
  const [key, title, author, state, opened, extra] = row;
  const [repo, n] = key.split("#");
  const number = Number(n);
  const created = NOW - opened;
  const head = seed.sha(key);
  const reviewedSha = extra.verdict ? seed.sha(key + (extra.pushed ? "@reviewed" : "")) : null;
  const reviewedAt = extra.verdict ? NOW - extra.last : null;
  // A re-review is anchored to when the new commits were seen.
  const anchor = state === "in_review" ? NOW - 38 * MIN : created;
  const updated = state === "in_review" ? NOW - 3 * MIN : reviewedAt || created + 6 * MIN;
  const record = {
    repo, number, title, author, head_branch: extra.branch, head_sha: head, is_draft: !!extra.draft,
    gh_created_at: iso(created), state,
    previous_state: extra.skip === "snoozed" ? "pending" : null,
    skip_reason: extra.skip || null,
    snooze_until: null, snooze_until_push: extra.skip === "snoozed",
    review_anchor_at: iso(anchor),
    last_reviewed_sha: reviewedSha,
    last_review_at: reviewedAt ? iso(reviewedAt) : null,
    last_verdict: extra.verdict || null,
    last_summary: extra.summary || null,
    blocking_count: extra.blocking || 0, nit_count: extra.nits || 0,
    review_session: extra.verdict ? sessionOf(repo, number) : null,
    review_attempts: extra.verdict ? 1 : 0,
    diff_too_large: false,
    updated_at: iso(updated),
  };
  if (extra.brief) {
    record.brief = { text: extra.brief, created_at: iso(reviewedAt), updated_at: iso(reviewedAt), head_sha: reviewedSha,
      source: "review", engine: seed.repoEngine(repo) };
  }
  if (extra.human) record.human_approved_at = iso(NOW - extra.human);
  prs[key] = record;

  const base = path.join(ghDir, repo.replace("/", "_") + "#" + number);
  const files = FILES[key] || [];
  const summary = {
    number, title, author: { login: author }, headRefName: extra.branch, headRefOid: head, isDraft: !!extra.draft,
    createdAt: iso(created), url: "https://github.com/" + repo + "/pull/" + number,
  };
  const fileStats = files.map(([p, additions, deletions]) => ({ path: p, additions, deletions }));
  const open = state !== "merged" && state !== "closed";
  if (open && lists[repo]) lists[repo].push(summary);
  write(base + ".state", open ? "OPEN" : state.toUpperCase());
  write(base + ".pr.json", json(summary));
  write(base + ".detail.json", json({ files: fileStats, additions: files.reduce((s, f) => s + f[1], 0),
    deletions: files.reduce((s, f) => s + f[2], 0), commits: [{ oid: reviewedSha || head }, { oid: head }] }));
  write(base + ".brief.json", json({ title, body: body(row), author: { login: author }, files: fileStats }));
  write(base + ".diff", diff(files));
});

Object.keys(lists).forEach((repo) => write(path.join(ghDir, "list_" + repo.replace("/", "_") + ".json"), json(lists[repo])));
write(path.join(ghDir, "user"), seed.user + "\n");
// The repo picker (`gh api user/repos`, answered by the sandbox's gh wrapper).
write(path.join(ghDir, "repos.tsv"), seed.discoverable.map((r) => r.name + "\t" + r.private).join("\n") + "\n");

write(path.join(dataDir, "state.json"), json({ schema_version: 1, prs, last_poll: iso(NOW - 4 * MIN) }));
// Append-only on disk: oldest first.
write(path.join(dataDir, "history.jsonl"), history.slice().reverse().map((e) => JSON.stringify(e)).join("\n") + "\n");

// ---- canned reviews for the demo agent ---------------------------------------
// When a daemon runs in the sandbox (the native app starts one), the PRs
// waiting for a review get these manifests instead of a real agent run.
const finding = (file, line, severity, text) => ({ file, line, severity, body: text });
const MANIFESTS = {
  "acme/api#412": {
    manifest_version: 1, verdict: "changes_requested", summary: "Limits reset on every deploy",
    findings: [
      finding("src/middleware/rate_limit.rs", 61, "blocking", "Counters live in process memory, so every deploy and every extra node resets or multiplies the limit. Keep them in Redis like the other shared state."),
      finding("src/routes/public.rs", 18, "nit", "The 429 body differs from the error shape the other routes use."),
      finding("config/limits.toml", 4, "nit", "Document the unit: requests per minute, per API key."),
    ],
    brief: "Adds per-API-key rate limits to the public endpoints, answering 429 with a Retry-After header once a key goes over. The counters are kept in each server's memory, so the real limit grows with the number of servers and resets on deploy.",
  },
  "acme/api#409": {
    manifest_version: 1, verdict: "approved", summary: "TTL now slides on read; ready",
    findings: [finding("src/session/redis.rs", 112, "nit", "The pipeline could batch the GET and the EXPIRE into one round trip.")],
    brief: "Moves login sessions out of Postgres and into Redis, so the API nodes stop contending on one hot table. Sessions get a 30-day sliding expiry that is now extended on every read, and an existing session is copied across the first time it is used, so nobody is signed out during the rollout.",
  },
  "acme/ios-app#88": {
    manifest_version: 1, verdict: "approved", summary: "Fixes the race; test covers it",
    findings: [finding("Sources/Push/TokenStore.swift", 37, "nit", "`rotate()` can be `private` now that the observer calls `refresh()`.")],
    brief: "Fixes a crash when the push notification token changed while the app was open: the old token was freed while a request still held it. The store now swaps tokens under a lock, and a new test rotates the token mid-request.",
  },
};
const FALLBACK = { manifest_version: 1, verdict: "approved", summary: "No blockers found", findings: [],
  brief: "A small, self-contained change with tests for the new behaviour. Nothing in it touches stored data or a public API." };
Object.keys(MANIFESTS).forEach((key) => write(path.join(agentDir, key.replace("/", "_") + ".json"), json(MANIFESTS[key])));
write(path.join(agentDir, "default.json"), json(FALLBACK));

console.log("seeded " + Object.keys(prs).length + " pull requests in " + config.repos.length + " repos, " + history.length + " history entries");
