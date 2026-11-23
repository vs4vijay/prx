// The Usage & stats page end to end (pages-usage.js with the real shell,
// bridge, chart and form helpers) against the fake DOM: totals, the chart,
// the per-group table and the insights panel, the range and grouping
// toggles and the queries they send, the loading, empty, older-daemon,
// offline and failed states, and hostile group names shown as text.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const U = require("../settings-ui/pages-usage.js");
const X = require("./page-fixtures.js");
const { boot, withText, bridgeError } = require("./fake-dom.js");

const section = (w, title) => w.page.querySelectorAll("section").find((s) => s.querySelector("h2") && s.querySelector("h2").textContent === title);
const stats = (w) => w.page.querySelectorAll(".stats .stat").map((s) => s.querySelectorAll("span").map((n) => n.textContent));
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
// Choose a segmented toggle (a radio group) by its label.
function choose(w, group, label) {
  const inputs = w.byId("usage-" + group).querySelectorAll("input");
  const labels = w.byId("usage-" + group).querySelectorAll("label");
  inputs.forEach((input, i) => { input.checked = labels[i].textContent === label; });
  w.fire(inputs.find((i) => i.checked), "change");
}

// ---- view model edge cases ----------------------------------------------------

test("model: queries fall back to the defaults for unknown toggles", () => {
  assert.deepEqual(U.usageQuery({ window: "1y" }), { bucket: "day", window: "1y", group: "none" });
  assert.equal(U.bucketFor("nonsense"), "day");
  assert.deepEqual(U.RANGE_BUCKETS, { "7d": "day", "30d": "day", "90d": "week", all: "month" });
  assert.deepEqual(U.insightsQuery({ window: "7d" }), { days: 7 });
  assert.deepEqual(U.WINDOWS.map((w) => w.value), ["7d", "30d", "90d", "all"]);
  assert.deepEqual(U.GROUPINGS.map((g) => g.value), ["none", "engine", "repo", "pr", "session"]);
  U.WINDOWS.forEach((w) => assert.ok(["day", "week", "month"].includes(U.bucketFor(w.value)), w.value));
});

test("model: bucket and group labels for odd input", () => {
  assert.equal(U.bucketLabel("2026-12-31", "day"), "Dec 31");
  assert.equal(U.bucketLabel("2026-01-01", "week"), "Jan 1");
  assert.equal(U.bucketLabel("1999-02-01", "month"), "Feb '99");
  // Anything that is not a date is shown as sent, never "undefined".
  assert.equal(U.bucketLabel("W41", "week"), "W41");
  assert.equal(U.bucketLabel("", "day"), "");
  assert.equal(U.bucketLabel(undefined, "day"), "");
  assert.equal(U.groupName("claude", "engine"), "claude");
  assert.equal(U.groupName("acme/api#42", "pr"), "acme/api#42");
  assert.equal(U.groupName("all", "engine"), "All runs");
  assert.equal(U.groupName("pi", "session"), "pi · no session");
  assert.equal(U.groupName("pi:", "session"), "pi · no session");
  // Long session ids are shortened; ids containing a colon keep their tail.
  assert.equal(U.groupName("codex:0123456789abcdef", "session"), "codex · 0123456789ab…");
  assert.equal(U.groupName("codex:01234567890123", "session"), "codex · 01234567890123");
  assert.equal(U.groupName("claude:a:b", "session"), "claude · a:b");
});

test("model: chart data counts cache as input and folds the tail into Other", () => {
  const t = (i, o, extra) => X.totals(i, o, 1, extra);
  const usage = { bucket: "day", group: "engine",
    series: [{ group: "a", points: [{ bucket_start: "2026-10-13", totals: t(10, 1, { cache_read_tokens: 5, cache_write_tokens: 4 }) }, { bucket_start: "2026-10-14", totals: t(0, 0) }] }],
    groups: [{ group: "a", totals: t(10, 1) }] };
  assert.deepEqual(U.chartData(usage), { labels: ["Oct 13", "Oct 14"], series: [{ name: "a", color: 0, values: [{ input: 19, output: 1 }, { input: 0, output: 0 }] }] });
  // Exactly MAX_SERIES groups all get their own stack; one more folds two.
  const many = (n) => ({ bucket: "day", group: "repo",
    series: Array.from({ length: n }, (_, i) => ({ group: "g" + i, points: [{ bucket_start: "2026-10-14", totals: t(100 - i, 1) }] })),
    groups: Array.from({ length: n }, (_, i) => ({ group: "g" + i, totals: t(100 - i, 1) })) });
  assert.deepEqual(U.chartData(many(U.MAX_SERIES)).series.map((s) => s.name), ["g0", "g1", "g2", "g3", "g4", "g5"]);
  const folded = U.chartData(many(U.MAX_SERIES + 1));
  assert.deepEqual(folded.series.map((s) => s.name), ["g0", "g1", "g2", "g3", "g4", "Other (2)"]);
  assert.deepEqual(folded.series[5].values, [{ input: 95 + 94, output: 2 }]);
  assert.deepEqual(folded.series.map((s) => s.color), [0, 1, 2, 3, 4, 5]);
  // No series at all is an empty chart, not an error.
  assert.deepEqual(U.chartData({ bucket: "day", group: "none" }), { labels: [], series: [] });
});

test("model: totals, cost notes and the missing-usage callout", () => {
  const o = U.overallModel(X.usage({ group: "none" }));
  assert.deepEqual([o.total, o.input, o.output, o.cacheRead, o.cacheWrite, o.runs, o.runsWithCost, o.cost, o.review, o.summary],
    [3700, 3000, 300, 400, 0, 2, 1, "$1.50", 3300, 550]);
  assert.equal(o.costNote, "Cost covers the 1 run whose engine reported one.");
  assert.equal(o.callout, "1 run without usage data is not in these totals.");
  const all = X.usage({ group: "none" });
  all.overall.runs_with_cost = 2;
  all.runs_without_usage = 0;
  assert.deepEqual([U.overallModel(all).costNote, U.overallModel(all).callout], [null, null]);
  assert.deepEqual(U.overallModel({}).total, 0);
  assert.equal(U.overallModel({}).cost, null);
  // Nothing recorded, or only runs that reported nothing: empty.
  assert.equal(U.isEmpty({}), true);
  assert.equal(U.isEmpty({ overall: X.totals(0, 0, 3) }), true);
  assert.equal(U.isEmpty({ overall: X.totals(0, 0, 1, { runs_with_cost: 1, cost_usd: 0.2 }) }), false, "a cost alone is still usage");
  assert.equal(U.isEmpty({ overall: X.totals(1, 0, 1) }), false);
});

test("model: group rows are sorted by tokens, then by name", () => {
  const usage = { group: "engine", groups: [
    { group: "b", totals: X.totals(100, 0, 1) }, { group: "a", totals: X.totals(100, 0, 2, { runs_with_cost: 1, cost_usd: 0.004 }) }, { group: "c", totals: X.totals(500, 5, 1) }] };
  const rows = U.groupRows(usage);
  assert.deepEqual(rows.map((r) => [r.name, r.total, r.runs, r.cost]), [["c", 505, 1, "—"], ["a", 100, 2, "<$0.01"], ["b", 100, 1, "—"]]);
  assert.deepEqual(U.groupRows({}), []);
});

test("model: insights with no reviews, one verdict, and no merges", () => {
  const empty = U.insightsModel({});
  assert.deepEqual([empty.reviews, empty.failed, empty.skipped, empty.effectiveness, empty.mergeLatency], [0, 0, 0, null, null]);
  assert.deepEqual(empty.verdicts.map((v) => [v.id, v.count, v.pct]), [["approved", 0, 0], ["changes_requested", 0, 0], ["comment_only", 0, 0]]);
  assert.deepEqual(empty.topBlocking, []);
  const m = U.insightsModel(X.insights());
  assert.deepEqual(m.verdicts.map((v) => [v.label, v.count, v.pct]), [["Approved", 6, 50], ["Changes requested", 5, 42], ["Commented", 1, 8]]);
  assert.deepEqual(m.effectiveness, { rate: "60%", detail: "3 of 5 changes-requested reviews later approved" });
  assert.equal(m.mergeLatency, "1.5 h on average (4 PRs)");
  assert.equal(U.insightsModel({ review_to_merge: { samples: 1, average_seconds: 3600 } }).mergeLatency, "1 h on average (1 PR)");
  assert.equal(U.insightsModel({ review_to_merge: { samples: 0, average_seconds: null } }).mergeLatency, null);
  assert.equal(U.insightsModel({ effectiveness: { changes_requested_reviews: 1, followed_by_approved: 0, rate: 0 } }).effectiveness.detail,
    "0 of 1 changes-requested review later approved");
  // Only the five most common blocking issues are listed.
  const top = Array.from({ length: 8 }, (_, i) => ({ text: "issue " + i, count: 8 - i }));
  assert.deepEqual(U.insightsModel({ top_blocking_issues: top }).topBlocking.map((t) => t.text), ["issue 0", "issue 1", "issue 2", "issue 3", "issue 4"]);
});

// ---- the page -------------------------------------------------------------------

test("loading: the toggles are there and the body is busy until the daemon answers", () => {
  const w = boot({ hash: "#page=usage", now: X.NOW, daemon: X.daemon(), hold: () => true });
  assert.equal(w.page.querySelector("h1").textContent, "Usage & stats");
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), "true");
  assert.equal(w.page.querySelectorAll(".skeleton-row").length, 3);
  assert.deepEqual(w.byId("usage-window").querySelectorAll("label").map((l) => l.textContent), ["7 days", "30 days", "90 days", "All time"]);
  assert.deepEqual(w.byId("usage-group").querySelectorAll("label").map((l) => l.textContent), ["None", "Engine", "Repo", "PR", "Session"]);
  assert.equal(w.byId("usage-window").getAttribute("aria-label"), "Range");
  // The defaults are selected and asked for.
  assert.deepEqual(w.byId("usage-window").querySelectorAll("input").map((i) => i.checked), [false, true, false, false]);
  assert.deepEqual(w.byId("usage-group").querySelectorAll("input").map((i) => i.checked), [true, false, false, false, false]);
  assert.deepEqual(w.calls("usage.get"), [{ bucket: "day", window: "30d", group: "none" }]);
  assert.deepEqual(w.calls("insights.get"), [{ days: 30 }]);
});

test("populated: totals, the callout, the chart, the table and the insights", async () => {
  const w = await X.open("usage");
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), null);
  assert.deepEqual(stats(w), [["3.7k", "Tokens", "3k in · 400 cache · 300 out"], ["$1.50", "Reported cost", "Cost covers the 1 run whose engine reported one."],
    ["2", "Runs", "550 tokens on briefs"]]);
  const why = w.page.querySelector(".banner.warn");
  assert.equal(why.querySelector("span").textContent, "1 run without usage data is not in these totals. ");
  assert.equal(why.querySelector("summary").textContent, "Why?");
  assert.deepEqual(why.querySelectorAll("code").map((c) => c.textContent), ["claude", "codex", "usage_output", "pi"]);

  const chart = section(w, "Tokens per day");
  assert.equal(chart.querySelector("svg").getAttribute("aria-label"), "Tokens per day");
  assert.deepEqual(chart.querySelectorAll("rect").map((r) => r.textContent),
    ["Oct 13 · All runs, input: 1k tokens", "Oct 13 · All runs, output: 100 tokens", "Oct 14 · All runs, input: 2k tokens", "Oct 14 · All runs, output: 200 tokens"]);
  assert.equal(chart.querySelector("ul.chart-legend li").textContent, "All runs");

  const table = section(w, "Totals").querySelector("table");
  assert.deepEqual(table.querySelectorAll("th").map((th) => th.textContent), ["Group", "Tokens", "Input", "Output", "Cache read", "Cache write", "Runs", "Cost"]);
  assert.deepEqual(table.querySelectorAll("tbody tr").map((tr) => tr.querySelectorAll("td").map((td) => td.textContent)),
    [["All runs", "3.3k", "3k", "300", "0", "0", "2", "$1.50"]]);

  const ins = section(w, "Review insights");
  assert.deepEqual(ins.querySelectorAll(".insight > .row-label").map((n) => n.textContent), ["Reviews", "Verdicts", "Effectiveness", "Review to merge", "Top blocking issues"]);
  assert.equal(ins.querySelectorAll(".insight")[0].querySelectorAll("span")[1].textContent, "12 completed · 1 failed · 3 skipped");
  assert.equal(ins.querySelector(".mix").getAttribute("aria-label"), "Approved 50%, Changes requested 42%, Commented 8%");
  assert.deepEqual(ins.querySelectorAll(".mix-part").map((p) => [p.className, p.getAttribute("title"), p.style.flexGrow]),
    [["mix-part v-approved", "Approved: 6", "6"], ["mix-part v-changes_requested", "Changes requested: 5", "5"], ["mix-part v-comment_only", "Commented: 1", "1"]]);
  assert.deepEqual(ins.querySelectorAll("ul.chart-legend li").map((li) => li.textContent), ["Approved 6 (50%)", "Changes requested 5 (42%)", "Commented 1 (8%)"]);
  assert.equal(ins.querySelectorAll(".insight")[2].querySelectorAll("span")[1].textContent, "60% — 3 of 5 changes-requested reviews later approved");
  assert.deepEqual(ins.querySelectorAll("ol.top-issues li").map((li) => li.textContent), ["Unbounded query " + X.EVIL + " (3)", "Missing index (1)"]);
});

test("the toggles change the query: the bucket follows the range, insights the window", async () => {
  const w = await X.open("usage");
  choose(w, "window", "90 days");
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), "true");
  await w.settle();
  assert.deepEqual(w.calls("usage.get")[1], { bucket: "week", window: "90d", group: "none" });
  assert.deepEqual(w.calls("insights.get")[1], { days: 90 });
  assert.ok(section(w, "Tokens per week"));
  choose(w, "window", "All time");
  await w.settle();
  assert.deepEqual(w.calls("usage.get")[2], { bucket: "month", window: "all", group: "none" });
  assert.deepEqual(w.calls("insights.get")[2], {});
  choose(w, "group", "PR");
  await w.settle();
  assert.deepEqual(w.calls("usage.get")[3], { bucket: "month", window: "all", group: "pr" });
  assert.ok(section(w, "Per PR"));
  choose(w, "group", "Engine");
  await w.settle();
  assert.ok(section(w, "Per engine"));
  // The choice survives leaving the page and coming back.
  w.window.prxApp.go("history");
  w.window.prxApp.go("usage");
  await w.settle();
  assert.deepEqual(w.calls("usage.get").pop(), { bucket: "month", window: "all", group: "engine" });
  assert.deepEqual(w.byId("usage-group").querySelectorAll("input").map((i) => i.checked), [false, true, false, false, false]);
});

test("a toggle changed while a request is in flight wins: the stale answer is not drawn", async () => {
  const w = await X.open("usage", null, { hold: (m) => m.method === "usage.get" || m.method === "insights.get" });
  // The first answers are still held; change the grouping meanwhile.
  choose(w, "group", "Engine");
  w.release();
  await w.settle();
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), "true");
  assert.equal(section(w, "Totals"), undefined, "the answer for the old grouping is not drawn");
  assert.deepEqual(w.calls("usage.get").map((q) => q.group), ["none", "engine"]);
  w.release();
  await w.settle();
  assert.ok(section(w, "Per engine"));
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), null);
});

test("grouped: one stack and one table row per group, hostile names as text", async () => {
  const w = await X.open("usage", { "usage.get": (q) => X.usage(Object.assign({}, q, { group: "engine" })) });
  const table = section(w, "Per engine").querySelector("table");
  assert.deepEqual(table.querySelectorAll("tbody tr").map((tr) => tr.querySelectorAll("td").slice(0, 2).map((td) => td.textContent)),
    [[X.EVIL, "6.3k"], ["claude", "3.3k"]]);
  assert.deepEqual(table.querySelectorAll("tbody .swatch").map((s) => s.className), ["swatch c1", "swatch c2"]);
  const legend = section(w, "Tokens per day").querySelectorAll("ul.chart-legend li").map((li) => li.textContent);
  assert.deepEqual(legend, [X.EVIL, "claude", "input + cacheoutput"]);
  assert.ok(section(w, "Tokens per day").querySelector("rect").textContent.includes(X.EVIL));
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
});

test("empty usage says which engines report, and still shows insights", async () => {
  const w = await X.open("usage", { "usage.get": (q) => Object.assign(X.usage(q), { series: [], groups: [], overall: X.totals(0, 0, 3), runs_without_usage: 3 }) });
  assert.equal(w.page.querySelector(".empty-title").textContent, "No token usage in this window");
  assert.match(w.page.querySelector(".empty-text").textContent, /Other engines \(such as pi\) report none\.$/);
  assert.equal(w.page.querySelector(".banner.warn span").textContent, "3 runs without usage data are not in these totals. ");
  assert.equal(w.page.querySelector("svg.chart-svg"), null);
  assert.equal(w.page.querySelector("table"), null);
  assert.ok(section(w, "Review insights"));
  // Nothing at all recorded: no callout either.
  const none = await X.open("usage", { "usage.get": (q) => Object.assign(X.usage(q), { series: [], groups: [], overall: X.totals(0, 0, 0), runs_without_usage: 0 }) });
  assert.equal(none.page.querySelector(".banner.warn"), null);
});

test("insights with no reviews yet read as zeros and a plain sentence", async () => {
  const w = await X.open("usage", { "insights.get": () => ({ reviews: { total: 0, failed: 0, skipped: 0 }, per_repo: [], effectiveness: { changes_requested_reviews: 0 }, review_to_merge: { samples: 0 }, top_blocking_issues: [] }) });
  const ins = section(w, "Review insights");
  assert.deepEqual(ins.querySelectorAll(".insight > .row-label").map((n) => n.textContent), ["Reviews", "Verdicts", "Effectiveness"]);
  assert.equal(ins.querySelectorAll(".mix-part").length, 0);
  assert.equal(ins.querySelector(".mix").getAttribute("aria-label"), "Approved 0%, Changes requested 0%, Commented 0%");
  assert.equal(ins.querySelectorAll(".insight")[2].querySelectorAll("span")[1].textContent, "No changes-requested reviews yet.");
});

test("an older daemon without /usage gets an update note; insights still show", async () => {
  const w = await X.open("usage", { "usage.get": () => { throw bridgeError("unsupported", "404"); } });
  assert.equal(w.page.querySelector(".empty-title").textContent, "Update the PRX daemon");
  assert.equal(w.page.querySelector(".empty-text code").textContent, "prx daemon restart");
  assert.ok(section(w, "Review insights"));
  assert.equal(w.byId("daemon").textContent, "Daemon running");
});

test("a failing /usage or /insights shows what it can", async () => {
  const w = await X.open("usage", { "usage.get": X.failing("usage log unreadable: " + X.EVIL) });
  assert.equal(w.page.querySelector(".empty-title").textContent, "Couldn't load this page");
  assert.equal(w.page.querySelector(".empty-text").textContent, "usage log unreadable: " + X.EVIL);
  assert.ok(section(w, "Review insights"), "insights do not depend on usage");
  assert.deepEqual(X.markupIn(w.page), []);
  const v = await X.open("usage", { "insights.get": X.failing("insights failed") });
  assert.equal(stats(v).length, 3);
  assert.equal(section(v, "Review insights"), undefined);
});

test("daemon offline replaces the page, and Try again recovers", async () => {
  let up = false;
  const gate = (answer) => (q) => { if (!up) X.offline(); return answer(q); };
  const w = await X.open("usage", { "config.get": gate(() => X.config()), "usage.get": gate(X.usage), "insights.get": gate(() => X.insights()) });
  assert.equal(w.page.querySelector(".empty-title").textContent, "Daemon offline");
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), null);
  assert.equal(w.byId("daemon").textContent, "Daemon not running");
  assert.equal(section(w, "Review insights"), undefined);
  up = true;
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(stats(w).length, 3);
  assert.equal(w.byId("daemon").textContent, "Daemon running");
  // Only insights offline is offline too: the page never shows half a daemon.
  const half = await X.open("usage", { "insights.get": X.offline });
  assert.equal(half.page.querySelector(".empty-title").textContent, "Daemon offline");
});
