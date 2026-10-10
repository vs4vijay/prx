// Unit tests for the Overview pages' pure view models (Dashboard, Pull
// requests, History, Usage & stats) and the chart geometry, run against the
// payloads preview-mock.js serves.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const O = require("../settings-ui/overview.js");
const Chart = require("../settings-ui/chart.js");
const D = require("../settings-ui/pages-dashboard.js");
const P = require("../settings-ui/pages-prs.js");
const H = require("../settings-ui/pages-history.js");
const U = require("../settings-ui/pages-usage.js");
const mock = require("./preview-mock.js");

const NOW = mock.NOW;
const status = () => mock.call("status.get");

test("formatting helpers", () => {
  assert.equal(O.formatTokens(950), "950");
  assert.equal(O.formatTokens(12345), "12.3k");
  assert.equal(O.formatTokens(123456), "123k");
  assert.equal(O.formatTokens(4200000), "4.2M");
  assert.equal(O.formatCost(0.004), "<$0.01");
  assert.equal(O.formatCost(1.5), "$1.50");
  assert.equal(O.formatCost(undefined), "");
  assert.equal(O.shortAge(new Date(NOW - 3 * 3600e3).toISOString(), NOW), "3h");
  assert.equal(O.ago(new Date(NOW - 10e3).toISOString(), NOW), "just now");
  // One token definition everywhere: "tokens" is every kind; a breakdown
  // names input (never including cache), cache and output.
  assert.equal(O.usageLine({ input_tokens: 12000, cache_read_tokens: 300, output_tokens: 1100, cost_usd: 0.04 }), "13.4k tokens · $0.04");
  assert.equal(O.usageBreakdown({ input_tokens: 12000, cache_read_tokens: 300, cache_write_tokens: 200, output_tokens: 1100 }), "12k in · 500 cache · 1.1k out");
  assert.equal(O.usageBreakdown({ input_tokens: 900, output_tokens: 10 }), "900 in · 10 out");
  assert.equal(O.usageLine({ model: "x" }), "");
  assert.equal(O.usageLine(undefined), "");
  assert.ok(O.compareKeys("acme/api#9", "acme/api#10") < 0);
});

test("lifecycle groups match the menu bar's", () => {
  const prs = status().prs;
  const group = (key) => O.groupOf(prs.find((p) => p.key === key));
  assert.equal(group("acme/api#412"), "needs_review");
  assert.equal(group("acme/api#409"), "in_review");
  assert.equal(group("acme/web#1288"), "ready");
  // A human already approved it, so it's merely reviewed.
  assert.equal(group("acme/ui-kit#77"), "reviewed");
  // Approved state with a comment-only agent verdict is not "ready".
  assert.equal(group("globex/ingest#58"), "reviewed");
  assert.equal(group("acme/web#1270"), "terminal");
});

test("one state system: every PR maps to a labelled, toned state", () => {
  const prs = status().prs;
  const state = (key) => O.stateOf(prs.find((p) => p.key === key));
  assert.equal(state("acme/api#412"), "needs_review");
  assert.equal(state("acme/api#409"), "in_review");
  assert.equal(state("acme/web#1288"), "ready");
  assert.equal(state("acme/web#1279"), "changes_requested");
  assert.equal(state("acme/ui-kit#77"), "approved");
  assert.equal(state("globex/ingest#58"), "commented");
  assert.equal(state("acme/api#398"), "skipped");
  assert.equal(state("acme/web#1270"), "merged");
  assert.equal(state("acme/ios-app#85"), "closed");
  prs.forEach((pr) => {
    const v = O.stateView(O.stateOf(pr));
    assert.ok(v.label && v.tone && v.glyph, pr.key);
  });
  // Changes requested is the error tone wherever it shows; approvals are ok.
  assert.equal(O.stateView("changes_requested").tone, "error");
  assert.equal(O.stateView(O.VERDICT_STATES.changes_requested).tone, "error");
  assert.equal(O.stateView("ready").tone, "ok");
  assert.equal(O.stateView("approved").tone, "ok");
  // The menu bar's vocabulary: "Needs review", never "Pending".
  assert.equal(O.stateLabel(prs.find((p) => p.key === "acme/api#412")), "Needs review");
  assert.equal(O.stateLabel(prs.find((p) => p.key === "acme/api#398")), "Skipped (draft)");
  assert.equal(O.stateLabel(prs.find((p) => p.key === "acme/infra#231")), "Skipped (skip rule)");
  assert.equal(O.stateLabel(prs.find((p) => p.key === "acme/billing#159")), "Skipped (snoozed)");
  assert.equal(O.groupLabel("ready"), "Ready for approval");
});

test("dashboard model: counts, mode, attention, recent, 7-day usage", () => {
  const s = status();
  const history = mock.call("history.list", D.QUERIES.history);
  const usage = mock.call("usage.get", D.QUERIES.usage);
  const m = D.dashboardModel(s, history, usage, null, NOW);
  assert.deepEqual(m.counts, { tracked: 12, attention: 3, ready: 2, running: 2, inFlight: 1, queued: 1 });
  assert.equal(m.mode.label, "Review and comment");
  assert.equal(m.mode.paused, false);
  // Changes requested first, newest review first.
  assert.deepEqual(m.attention.map((a) => a.key), ["acme/billing#164", "acme/web#1279", "acme/api#409"]);
  assert.equal(m.recent.length, 6);
  assert.equal(m.recent[0].ts, history.entries[0].ts);
  // Recent activity: the latest event per PR only.
  assert.equal(new Set(m.recent.map((r) => r.key)).size, m.recent.length);
  assert.equal(m.configError, null);
  const broken = D.dashboardModel(Object.assign({}, s, { config_error: { message: "bad" } }), history, usage, null, NOW);
  assert.equal(broken.configError.message, "bad");
  assert.ok(m.week.tokens > 0);
  assert.equal(m.week.runs, usage.overall.runs);
  assert.match(m.week.cost, /^\$\d+\.\d\d$/);
  assert.equal(m.week.withoutUsage, usage.runs_without_usage);
  // Paused, and an older daemon without /usage.
  s.paused_until = new Date(NOW + 60e3).toISOString();
  const old = D.dashboardModel(s, history, null, { kind: "unsupported" }, NOW);
  assert.equal(old.mode.paused, true);
  assert.equal(old.week, null);
  assert.equal(old.usageUnsupported, true);
});

test("pull requests: the first sort follows menu.sort; headers toggle", () => {
  const prs = status().prs;
  const keys = (rows) => rows.map((p) => p.key);
  assert.deepEqual(P.initialSort({ sort: "created_desc" }), { column: "created", dir: "desc" });
  assert.deepEqual(P.initialSort(undefined), { column: "created", dir: "desc" });
  assert.deepEqual(P.initialSort({ sort: "key" }), { column: "key", dir: "asc" });
  const newest = keys(P.sortRows(prs, P.initialSort({ sort: "created_desc" })));
  assert.deepEqual(newest.slice(0, 3), ["acme/ios-app#88", "acme/api#412", "acme/billing#164"]);
  assert.deepEqual(keys(P.sortRows(prs, P.initialSort({ sort: "created_asc" })))[0], "acme/ios-app#85");
  // Never-reviewed PRs last, in key order.
  const reviewed = keys(P.sortRows(prs, P.initialSort({ sort: "last_review_desc" })));
  assert.equal(reviewed[0], "acme/billing#164");
  assert.deepEqual(reviewed.slice(-3), ["acme/billing#159", "acme/infra#231", "acme/ios-app#88"]);
  assert.deepEqual(keys(P.sortRows(prs, { column: "key", dir: "asc" })).slice(0, 4), ["acme/api#398", "acme/api#409", "acme/api#412", "acme/billing#159"]);
  assert.deepEqual(P.toggleSort({ column: "key", dir: "asc" }, "key"), { column: "key", dir: "desc" });
  assert.deepEqual(P.toggleSort({ column: "key", dir: "asc" }, "last_review"), { column: "last_review", dir: "desc" });
  assert.deepEqual(P.toggleSort({ column: "key", dir: "asc" }, "author"), { column: "author", dir: "asc" });
});

test("pull requests: filters by repo, state, and text", () => {
  const prs = status().prs;
  const keys = (f) => P.filterRows(prs, Object.assign({ repo: "", state: "", text: "" }, f)).map((p) => p.key).sort();
  assert.equal(keys({}).length, prs.length);
  assert.deepEqual(keys({ state: "ready" }), ["acme/web#1288", "globex/ingest#61"]);
  assert.deepEqual(keys({ state: "attention" }), ["acme/api#409", "acme/billing#164", "acme/web#1279"]);
  assert.deepEqual(keys({ repo: "globex/ingest" }), ["globex/ingest#58", "globex/ingest#61"]);
  assert.deepEqual(keys({ text: "PRIYA" }), ["acme/web#1270", "acme/web#1288"]);
  assert.deepEqual(keys({ text: "#41", repo: "acme/api" }), ["acme/api#412"]);
  assert.deepEqual(keys({ repo: "acme/web", state: "terminal" }), ["acme/web#1270"]);
  assert.deepEqual(P.repoNames(prs), ["acme/api", "acme/billing", "acme/infra", "acme/ios-app", "acme/ui-kit", "acme/web", "globex/ingest"]);
});

test("pull requests: each action's request, engines, and terminal PRs", () => {
  const s = status();
  const pr = s.prs.find((p) => p.key === "acme/api#42") || s.prs.find((p) => p.key === "acme/api#412");
  assert.deepEqual(P.reviewRequest("review", pr), { target: "acme/api#412" });
  assert.deepEqual(P.reviewRequest("engine", pr, { engine: "codex" }), { target: "acme/api#412", engine: "codex" });
  assert.deepEqual(P.reviewRequest("nopost", pr), { target: "acme/api#412", post: false });
  // Only what differs from the defaults: the repo's engine and posting.
  assert.deepEqual(P.reviewRequest("prompt", pr, { prompt: "  Check the migration. ", engine: "claude", post: true }),
    { target: "acme/api#412", extra_prompt: "Check the migration." });
  assert.deepEqual(P.reviewRequest("prompt", pr, { prompt: "", engine: "codex", post: false }),
    { target: "acme/api#412", engine: "codex", post: false });
  assert.deepEqual(P.targetRequest("  42 "), { target: "42" });
  assert.equal(P.targetRequest("   "), null);
  const choices = P.engineChoices(s, pr);
  assert.deepEqual(choices.map((c) => c.label), ["cheap", "claude (default)", "codex", "pi"]);
  assert.deepEqual(P.engineChoices({ prs: [] }, pr), []);
  const merged = s.prs.find((p) => p.state === "merged");
  assert.equal(P.canReview(merged), false);
  assert.equal(P.rowModel(merged, NOW).reviewable, false);
  assert.equal(P.canReview(pr), true);
  // The mock daemon answers like the real one, and the note is shown.
  const result = mock.call("review.request", P.reviewRequest("nopost", pr));
  assert.equal(P.reviewNote(result), "Queued acme/api#412 without posting");
  assert.equal(P.reviewNote({ enqueued: ["a/b#1"] }), "Queued a/b#1.");
  assert.equal(P.reviewNote({ enqueued: [] }), "Nothing was queued.");
  assert.deepEqual(mock.requests[mock.requests.length - 1], { target: "acme/api#412", post: false });
  assert.throws(() => mock.call("review.request", { target: merged.key }), /merged/);
});

test("history: paging with before, polling, and filters", () => {
  assert.deepEqual(H.historyQuery({ repo: "" }, null), { limit: H.PAGE });
  assert.deepEqual(H.historyQuery({ repo: "acme/api" }, "2026-10-01T00:00:00Z"), { limit: H.PAGE, repo: "acme/api", before: "2026-10-01T00:00:00Z" });
  const first = mock.call("history.list", H.historyQuery({}, null)).entries;
  assert.equal(first.length, H.PAGE);
  let merged = { entries: first, done: false };
  let pages = 1;
  while (!merged.done) {
    merged = H.mergeOlder(merged.entries, mock.call("history.list", H.historyQuery({}, H.oldestTs(merged.entries))).entries);
    pages += 1;
  }
  assert.ok(pages >= 3);
  const all = mock.call("history.list", { limit: 1000 }).entries;
  assert.deepEqual(merged.entries.map(H.entryId), all.map(H.entryId));
  // Overlapping pages never duplicate.
  assert.equal(H.mergeOlder(first, first.slice(-3), 3).entries.length, first.length);
  // A poll puts only the new entries on top.
  const polled = H.mergeNewer(all.slice(2), all.slice(0, H.PAGE));
  assert.deepEqual(polled.map(H.entryId), all.map(H.entryId));
  const codex = H.filterEntries(all, { repo: "", event: "", engine: "codex" });
  assert.ok(codex.length > 0 && codex.every((e) => e.agent === "codex"));
  const done = H.filterEntries(all, { repo: "acme/api", event: "review_completed", engine: "" });
  assert.ok(done.length > 0 && done.every((e) => e.repo === "acme/api" && e.event === "review_completed"));
  // Nothing is in the future, and nothing is old enough to be compacted.
  assert.ok(all.every((e) => Date.parse(e.ts) <= NOW && Date.parse(e.ts) > NOW - 90 * 86400e3));
  assert.deepEqual(H.engineNames(all), ["cheap", "claude", "codex", "pi"]);
  assert.ok(H.repoNames(all).includes("globex/ingest"));
});

test("history: rows show engine, tokens and not-posted; details split usage", () => {
  const all = mock.call("history.list", { limit: 1000 }).entries;
  const monitor = all.find((e) => e.event === "review_completed" && e.repo === "acme/ios-app" && e.usage && e.summary_usage);
  const v = H.entryView(monitor, NOW);
  assert.equal(v.notPosted, true);
  assert.equal(v.engine, monitor.agent);
  assert.match(v.usage, /^[\d.]+[kM]? tokens/);
  assert.equal(v.tokens, O.formatTokens(O.tokenTotal(monitor.usage)) + " tokens");
  assert.equal(v.stateId, O.VERDICT_STATES[monitor.verdict]);
  assert.equal(v.hasDetail, true);
  const d = H.detailModel(monitor);
  assert.equal(d.brief, monitor.brief || monitor.exec_summary);
  assert.ok(d.brief);
  assert.deepEqual(d.review.map((r) => r[0]).slice(0, 2), ["Input", "Output"]);
  // The detail's Total is the row's "tokens": the two never disagree.
  assert.equal(d.review.find((r) => r[0] === "Total")[1], O.formatTokens(O.tokenTotal(monitor.usage)));
  assert.ok(d.briefUsage.some((r) => r[0] === "Model" && r[1] === "claude-haiku-4-5"));
  // Lines written before the rename carry `exec_summary`; both read as the brief.
  assert.equal(H.detailModel({ event: "review_completed", exec_summary: "Old text." }).brief, "Old text.");
  assert.equal(H.detailModel({ event: "review_completed", brief: "New.", exec_summary: "Old." }).brief, "New.");
  // An on-demand brief run: its usage is brief usage, not review usage.
  const run = all.find((e) => e.event === "brief" && e.brief);
  const b = H.detailModel(run);
  assert.equal(b.brief, run.brief);
  assert.equal(b.review, null);
  assert.ok(b.briefUsage.some((r) => r[0] === "Input"));
  assert.equal(H.entryView(run, NOW).hasDetail, true);
  assert.equal(H.entryView(run, NOW).eventLabel, "Brief");
  assert.equal(H.entryView(run, NOW).summary, "Brief written");
  assert.equal(O.eventSummary({ event: "brief" }), "Brief failed");
  const pi = all.find((e) => e.event === "review_completed" && e.agent === "pi");
  assert.equal(H.entryView(pi, NOW).usage, "");
  assert.equal(H.detailModel(pi).review, null);
  const merged = all.find((e) => e.event === "pr_merged");
  assert.equal(H.entryView(merged, NOW).hasDetail, false);
  assert.equal(H.entryView(merged, NOW).summary, "Merged");
});

test("usage: toggles map onto the usage.get and insights.get queries", () => {
  assert.deepEqual(U.usageQuery({}), { bucket: "day", window: "30d", group: "none" });
  // The bucket follows the range when the page doesn't pin one.
  assert.deepEqual(U.usageQuery({ window: "90d", group: "engine" }), { bucket: "week", window: "90d", group: "engine" });
  assert.deepEqual(U.usageQuery({ window: "all" }), { bucket: "month", window: "all", group: "none" });
  assert.equal(U.bucketFor("7d"), "day");
  assert.deepEqual(U.usageQuery({ bucket: "month", window: "all", group: "engine" }), { bucket: "month", window: "all", group: "engine" });
  assert.deepEqual(U.usageQuery({ bucket: "week", window: "7d", group: "repo", repo: "acme/api" }), { bucket: "week", window: "7d", group: "repo", repo: "acme/api" });
  assert.deepEqual(U.insightsQuery({ window: "90d" }), { days: 90 });
  assert.deepEqual(U.insightsQuery({ window: "all" }), {});
  assert.deepEqual(U.insightsQuery({}), { days: 30 });
  assert.equal(U.bucketLabel("2026-10-05", "week"), "Oct 5");
  assert.equal(U.bucketLabel("2026-01-01", "month"), "Jan '26");
  assert.equal(U.groupName("claude:c0ffee1-1234", "session"), "claude · c0ffee1-1234");
  assert.equal(U.groupName("pi:none", "session"), "pi · no session");
  assert.equal(U.groupName("all", "none"), "All runs");
});

test("usage: monthly by engine charts one bar per month, stacked by engine", () => {
  const usage = mock.call("usage.get", U.usageQuery({ bucket: "month", window: "90d", group: "engine" }));
  const data = U.chartData(usage);
  assert.equal(data.labels.length, usage.series[0].points.length);
  assert.ok(data.labels.every((l) => /^[A-Z][a-z]{2} '\d\d$/.test(l)));
  // Largest group first; each value splits input (with cache) and output.
  const rows = U.groupRows(usage);
  assert.deepEqual(rows.map((r) => r.name), ["claude", "codex", "pi", "cheap"]);
  assert.deepEqual(data.series.map((s) => s.name), ["claude", "codex", "pi", "cheap"]);
  const claude = usage.groups.find((g) => g.group === "claude").totals;
  const sum = data.series[0].values.reduce((n, v) => n + v.input + v.output, 0);
  assert.equal(sum, O.tokenTotal(claude));
  assert.equal(rows[0].total, O.tokenTotal(claude));
  // pi reports nothing: only its runs' summaries (another engine) count.
  assert.equal(rows[2].total, rows[2].summaryTokens);
  assert.equal(rows[2].cost, "—");
  const overall = U.overallModel(usage);
  assert.equal(overall.total, rows.reduce((n, r) => n + r.total, 0));
  assert.equal(overall.withoutUsage, usage.runs_without_usage);
  assert.match(overall.callout, /runs? without usage data/);
  assert.equal(U.isEmpty(usage), false);
});

test("usage: many groups fold into Other; empty usage says so", () => {
  const usage = mock.call("usage.get", { bucket: "week", window: "all", group: "pr" });
  assert.ok(usage.groups.length > U.MAX_SERIES);
  const data = U.chartData(usage);
  assert.equal(data.series.length, U.MAX_SERIES);
  assert.match(data.series[U.MAX_SERIES - 1].name, /^Other \(\d+\)$/);
  const total = data.series.reduce((n, s) => n + s.values.reduce((m, v) => m + v.input + v.output, 0), 0);
  assert.equal(total, O.tokenTotal(usage.overall));
  const empty = { bucket: "day", window: "7d", group: "none", series: [], groups: [], runs_without_usage: 2,
    overall: { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, cost_usd: 0, runs: 2, runs_with_cost: 0 } };
  assert.equal(U.isEmpty(empty), true);
  assert.match(U.REPORTING, /claude.*codex/);
  assert.equal(U.overallModel(empty).callout, "2 runs without usage data are not in these totals.");
});

test("usage: insights panel model", () => {
  const ins = mock.call("insights.get", U.insightsQuery({ window: "30d" }));
  const m = U.insightsModel(ins);
  assert.equal(m.reviews, ins.reviews.total);
  assert.equal(m.verdicts.reduce((n, v) => n + v.count, 0), ins.per_repo.reduce((n, r) => n + r.reviews, 0));
  assert.ok(Math.abs(m.verdicts.reduce((n, v) => n + v.pct, 0) - 100) <= 2);
  assert.match(m.effectiveness.rate, /^\d+%$/);
  assert.equal(U.insightsModel({ per_repo: [], effectiveness: { changes_requested_reviews: 0 } }).effectiveness, null);
});

test("chart geometry: nice axis, stacked segments, thinned labels", () => {
  assert.equal(Chart.niceMax(0), 1);
  assert.equal(Chart.niceMax(730), 1000);
  assert.equal(Chart.niceMax(180), 200);
  assert.equal(Chart.niceMax(2400), 2500);
  assert.equal(Chart.niceMax(41000), 50000);
  const data = { labels: ["a", "b", "c"], series: [
    { name: "x", values: [{ input: 300, output: 100 }, { input: 0, output: 0 }, { input: 50, output: 0 }] },
    { name: "y", values: [{ input: 200, output: 100 }, { input: 0, output: 0 }, { input: 0, output: 10 }] },
  ] };
  const g = Chart.layout(data, { width: 300, height: 200 });
  assert.equal(g.max, 1000);
  const plotBottom = g.plot.y + g.plot.height;
  const bar = g.bars[0];
  assert.deepEqual(bar.segments.map((s) => [s.series, s.part]), [[0, "input"], [0, "output"], [1, "input"], [1, "output"]]);
  assert.deepEqual(bar.segments.map((s) => s.cls), ["c1", "c1 out", "c2", "c2 out"]);
  // Contiguous from the baseline, heights proportional to the values.
  assert.ok(Math.abs(bar.segments[0].y + bar.segments[0].height - plotBottom) < 1e-9);
  for (let i = 1; i < bar.segments.length; i++) {
    assert.ok(Math.abs(bar.segments[i].y + bar.segments[i].height - bar.segments[i - 1].y) < 1e-9);
  }
  assert.ok(Math.abs(bar.segments.reduce((n, s) => n + s.height, 0) - (700 / 1000) * g.plot.height) < 1e-9);
  assert.equal(g.bars[1].segments.length, 0);
  assert.equal(g.bars[0].total, 700);
  assert.ok(g.bars.every((b, i) => i === 0 || b.x > g.bars[i - 1].x + g.bars[i - 1].width));
  assert.deepEqual(g.ticks.map((t) => t.value), [0, 200, 400, 600, 800, 1000]);
  // 60 buckets in a narrow chart: only some labels, always the last one.
  const many = { labels: Array.from({ length: 60 }, (_, i) => "d" + i), series: [{ name: "all", values: Array.from({ length: 60 }, () => ({ input: 1, output: 1 })) }] };
  const wide = Chart.layout(many, { width: 400, height: 200 });
  const shown = wide.bars.filter((b) => b.showLabel);
  assert.ok(shown.length < 60 && shown.length >= 5);
  assert.equal(wide.bars[59].showLabel, true);
  // An empty series still yields an axis.
  assert.equal(Chart.layout({ labels: [], series: [] }, {}).max, 1);
});

test("the mock serves an older daemon's missing /usage as `unsupported`", () => {
  const reply = mock.answer({ method: "usage.get", params: { bucket: "hour" } });
  assert.equal(reply.ok, false);
  assert.match(reply.error.message, /invalid bucket 'hour'/);
});

test("pull requests: at most five columns, the verdict folded into State", () => {
  // State, Pull request, Age, Last review, plus the actions column.
  assert.deepEqual(P.COLUMNS.map((c) => c[0]), ["state", "key", "created", "last_review"]);
  const pr = status().prs.find((p) => p.key === "acme/web#1279");
  const r = P.rowModel(pr, NOW);
  assert.equal(r.stateId, "changes_requested");
  assert.equal(r.findings, "2 blocking, 3 nits");
  assert.equal(r.engine, "claude");
});

test("dashboard: Resume and the broken-config banner are served by the mock", () => {
  assert.equal(mock.call("pause.resume").changed, true);
  assert.equal(mock.call("status.get").paused_until, undefined);
  assert.ok(mock.call("status.get").next_poll);
});

test("pause: the mock starts, restarts and lifts a pause like the daemon", () => {
  const before = Date.now();
  const r = mock.call("pause.start", { hours: 2 });
  assert.equal(r.ok, true);
  const until = Date.parse(r.paused_until);
  assert.ok(until >= before + 2 * 3600e3 && until <= Date.now() + 2 * 3600e3);
  assert.equal(mock.call("status.get").paused_until, r.paused_until);
  assert.equal(O.pauseState(mock.call("status.get"), Date.now()).paused, true);
  const at = new Date(Date.now() + 3600e3).toISOString();
  assert.equal(Date.parse(mock.call("pause.start", { until: at }).paused_until), Date.parse(at));
  for (const bad of [{}, { hours: 1, until: at }, { hours: 0 }, { hours: "2" }, { until: "soon" }]) {
    assert.throws(() => mock.call("pause.start", bad), (e) => e.kind === "bad_request", JSON.stringify(bad));
  }
  assert.throws(() => mock.call("pause.start", { until: new Date(Date.now() - 1000).toISOString() }),
    (e) => e.kind === "daemon" && /future/.test(e.message));
  assert.equal(mock.call("pause.resume").paused_until, null);
  assert.equal(mock.call("status.get").paused_until, undefined);
});
