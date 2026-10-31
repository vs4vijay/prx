// The History page end to end (pages-history.js with the real shell, bridge
// and form helpers) against the fake DOM: a row per event type, the detail
// of a review and of a brief run, filters, paging with `before`, polling,
// the loading, empty, offline and failed states, and hostile content shown
// as text.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const H = require("../settings-ui/pages-history.js");
const O = require("../settings-ui/overview.js");
const X = require("./page-fixtures.js");
const { boot, withText, bridgeError } = require("./fake-dom.js");

const items = (w) => w.page.querySelectorAll(".history-item");
const tags = (item) => item.querySelectorAll(".ev-tags > span").map((n) => n.textContent);
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
const tick = (w) => w.intervals.find((i) => i.ms === 10000).run();
function pick(w, id, label) {
  w.fire(w.byId(id), "click");
  w.fire(w.byId(id + "-list").querySelectorAll("[role=option]").find((o) => o.querySelector(".select-label").textContent === label), "click");
}
// `count` entries, newest first, one minute apart, starting `from` minutes ago.
function page(count, from) {
  return Array.from({ length: count }, (_, i) => X.entry("review_started", "acme/api#" + (from + i), (from + i) * X.MIN, { agent: "claude" }));
}

// ---- view model edge cases ----------------------------------------------------

test("model: every event type has a label, a summary, and a pill only for outcomes", () => {
  const views = X.history().map((e) => H.entryView(e, X.NOW));
  assert.deepEqual(views.map((v) => [v.eventLabel, v.summary, v.stateId, v.hasDetail]), [
    ["Review started", "Review started", null, false],
    ["Review completed", "Changes requested · 2 blocking, 1 nit", "changes_requested", true],
    ["Review completed", "Approved · 2 nits", "approved", true],
    ["Review completed", "Commented · 0 nits", "commented", true],
    ["Review failed", "Review failed", "failed", true],
    ["Review skipped", "Skipped: draft " + X.EVIL, null, false],
    ["New commits", "New commits", null, false],
    ["Brief", "Brief written", null, true],
    ["Brief", "Brief failed", null, true],
    ["Merged", "Merged", null, false],
    ["Closed", "Closed", null, false],
    ["New PR", "New PR", null, false],
  ]);
  // The event filter offers exactly the types the daemon writes.
  assert.deepEqual(O.EVENTS.map((e) => e[0]).sort(), Array.from(new Set(X.history().map((e) => e.event))).sort());
  // An event this build does not know is listed under its own name.
  const future = H.entryView(X.entry("pr_reopened", "a/b#1", X.MIN), X.NOW);
  assert.deepEqual([future.eventLabel, future.summary, future.stateId, future.hasDetail], ["pr_reopened", "pr_reopened", null, false]);
});

test("model: tokens, cost, engine and the not-posted marker per row", () => {
  const [, changes, approved, comment, failed] = X.history().map((e) => H.entryView(e, X.NOW));
  assert.deepEqual([changes.tokens, changes.cost, changes.engine, changes.notPosted, changes.usage], ["13.6k tokens", "$0.04", "codex", false, "13.6k tokens · $0.04"]);
  assert.equal(approved.notPosted, true);
  // An engine that reports nothing: empty cells, never "0 tokens".
  assert.deepEqual([comment.tokens, comment.cost, comment.usage, comment.engine, comment.notPosted], ["", "", "", "pi", false]);
  // A partial report (a failed run): tokens without a cost.
  assert.deepEqual([failed.tokens, failed.cost], ["500 tokens", ""]);
  assert.equal(H.entryView(X.entry("review_completed", "a/b#1", 0, { verdict: "approved", usage: { cost_usd: 0 } }), X.NOW).cost, "$0.00");
  assert.equal(H.entryView(X.entry("pr_merged", "a/b#1", 0), X.NOW).engine, "");
  assert.equal(changes.key, "acme/web#6");
  assert.equal(changes.when, "30m ago");
});

test("model: usage rows list only what was reported, with one Total", () => {
  assert.deepEqual(H.usageRows(X.USAGE), [["Input", "12k"], ["Output", "1.1k"], ["Cache read", "300"], ["Cache write", "200"], ["Total", "13.6k"],
    ["Cost", "$0.04"], ["Model", "claude-sonnet-4-5"], ["Session", "c0ffee"]]);
  assert.deepEqual(H.usageRows({ input_tokens: 0, output_tokens: 0 }), [["Input", "0"], ["Output", "0"], ["Total", "0"]]);
  // A cost alone has no Total row.
  assert.deepEqual(H.usageRows({ cost_usd: 0.004 }), [["Cost", "<$0.01"]]);
  assert.equal(H.usageRows({ model: "x", session_id: "y" }), null);
  assert.equal(H.usageRows(null), null);
  assert.equal(H.usageRows(undefined), null);
  assert.deepEqual(H.usageRows({ input_tokens: null, output_tokens: 7 }), [["Output", "7"], ["Total", "7"]]);
});

test("model: the detail's duration, posting and blocking findings", () => {
  const d = H.detailModel(X.history()[1]);
  assert.deepEqual([d.duration, d.posted, d.blocking.length], ["2.5 min", true, 2]);
  assert.equal(H.detailModel({ event: "review_completed", duration_seconds: 59 }).duration, "1 min");
  assert.equal(H.detailModel({ event: "review_completed", duration_seconds: 0 }).duration, null);
  // `posted` is three-valued: an older line says nothing either way.
  assert.equal(H.detailModel({ event: "review_completed" }).posted, null);
  assert.equal(H.detailModel({ event: "review_completed", posted: false }).posted, false);
  assert.deepEqual(H.detailModel({ event: "review_failed" }), { brief: null, review: null, briefUsage: null, duration: null, blocking: [], posted: null });
});

test("model: paging and merging are stable with duplicate timestamps and empty pages", () => {
  const a = X.entry("review_started", "a/b#1", X.MIN, { agent: "claude" });
  const b = X.entry("review_completed", "a/b#1", X.MIN, { agent: "claude" });
  const c = X.entry("review_completed", "a/b#2", X.MIN, { agent: "claude" });
  const d = X.entry("review_completed", "a/b#2", X.MIN, { agent: "codex" });
  // Same instant, different event, PR or engine: four distinct entries.
  assert.equal(new Set([a, b, c, d].map(H.entryId)).size, 4);
  assert.equal(H.entryId(X.entry("pr_merged", "a/b#1", 0)), X.ago(0) + "|pr_merged|a/b|1|");
  assert.deepEqual(H.mergeOlder([a, b], [b, c, d], 3), { entries: [a, b, c, d], done: false });
  assert.deepEqual(H.mergeOlder([a], [], 3), { entries: [a], done: true });
  assert.deepEqual(H.mergeOlder([], [], 3), { entries: [], done: true });
  assert.deepEqual(H.mergeNewer([c, d], [a, b, c]), [a, b, c, d]);
  assert.deepEqual(H.mergeNewer([], [a]), [a]);
  assert.deepEqual(H.mergeNewer([a], []), [a]);
  assert.equal(H.oldestTs([]), null);
  assert.equal(H.oldestTs([a, X.entry("pr_merged", "a/b#1", X.DAY)]), X.ago(X.DAY));
  // A full page is never "done": exactly PAGE entries may have more behind.
  assert.equal(H.mergeOlder([], page(H.PAGE, 1)).done, false);
  assert.equal(H.mergeOlder([], page(H.PAGE - 1, 1)).done, true);
  assert.deepEqual(H.historyQuery(null, null), { limit: 50 });
  assert.deepEqual(H.historyQuery({ repo: "", event: "review_failed", engine: "pi" }, ""), { limit: 50 }, "event and engine are filtered in the window");
});

// ---- the page -------------------------------------------------------------------

test("loading: the filters are there and the list is busy until history answers", () => {
  const w = boot({ hash: "#page=history", now: X.NOW, daemon: X.daemon(), hold: () => true });
  assert.equal(w.page.querySelector("h1").textContent, "History");
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), "true");
  assert.equal(w.page.querySelectorAll(".skeleton-row").length, 3);
  assert.equal(w.page.querySelectorAll(".toolbar [role=combobox]").length, 3);
  assert.equal(items(w).length, 0);
});

test("populated: a row per event, newest first, with pills, badges, tokens and cost", async () => {
  const w = await X.open("history");
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), null);
  assert.deepEqual(w.calls("history.list"), [{ limit: 50 }]);
  assert.equal(items(w).length, 12);
  assert.deepEqual(items(w).map((i) => i.querySelector(".list-key").textContent), ["acme/api#2", "acme/web#6", "acme/web#3", "acme/web#5", "acme/api#1",
    "solo/tool#7", "acme/web#6", "acme/web#3", "acme/web#5", "acme/api#11", "acme/api#12", "acme/api#12"]);
  assert.deepEqual(items(w).map((i) => i.querySelector(".list-when").textContent), ["3m ago", "30m ago", "2h ago", "6h ago", "7h ago", "8h ago", "9h ago", "10h ago", "11h ago", "2d ago", "3d ago", "4d ago"]);
  assert.deepEqual(items(w).slice(0, 6).map(tags), [
    ["Review started", "claude"],
    ["Changes requested · 2 blocking, 1 nit", "codex"],
    ["Approved · 2 nits", "Not posted", "claude"],
    ["Commented · 0 nits", "pi"],
    ["Review failed", "claude"],
    ["Skipped: draft " + X.EVIL],
  ]);
  assert.deepEqual(items(w).map((i) => (i.querySelector(".state") || { dataset: {} }).dataset.state || null),
    [null, "changes_requested", "approved", "commented", "failed", null, null, null, null, null, null, null]);
  assert.deepEqual(items(w).map((i) => [i.querySelector(".tokens-line").textContent, i.querySelector(".cost").textContent]).slice(1, 5),
    [["13.6k tokens", "$0.04"], ["13.6k tokens", "$0.04"], ["", ""], ["500 tokens", ""]]);
  // Only rows with something to open are buttons.
  assert.deepEqual(items(w).map((i) => i.querySelector(".list-row").tagName), ["DIV", "BUTTON", "BUTTON", "BUTTON", "BUTTON", "DIV", "DIV", "BUTTON", "BUTTON", "DIV", "DIV", "DIV"]);
  assert.equal(items(w)[0].querySelector("time").getAttribute("datetime"), X.ago(3 * X.MIN));
  // Fewer than a page came back, so there is nothing more to load.
  assert.equal(w.page.querySelector(".load-more").textContent, "That's everything.");
  assert.equal(button(w.page, "Load more"), undefined);
});

test("hostile titles, reasons, briefs and findings are shown exactly, as text", async () => {
  const w = await X.open("history");
  assert.equal(items(w)[4].querySelector(".list-title").textContent, X.EVIL);
  assert.equal(items(w)[0].querySelector(".list-title").textContent, X.UNICODE);
  assert.equal(items(w)[1].querySelector(".list-title").textContent, X.LONG);
  w.fire(items(w)[1].querySelector("button.list-row"), "click");
  const detail = w.page.querySelector(".history-detail");
  assert.equal(detail.querySelector(".detail-block p").textContent, "Rewrites the session store. " + X.EVIL);
  assert.deepEqual(detail.querySelectorAll("li").map((li) => li.textContent), ["Unbounded query " + X.EVIL, "Missing index"]);
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
});

test("a completed review opens to its brief, findings, both usages and how it ended", async () => {
  const w = await X.open("history");
  const rowOf = (i) => items(w)[i].querySelector(".list-row");
  w.fire(rowOf(1), "click");
  assert.equal(rowOf(1).getAttribute("aria-expanded"), "true");
  assert.ok(items(w)[1].classList.contains("open"));
  let detail = w.page.querySelector(".history-detail");
  assert.deepEqual(detail.querySelectorAll("h3").map((h) => h.textContent), ["Brief", "Blocking findings", "Review usage", "Brief usage"]);
  const lists = detail.querySelectorAll("dl.facts");
  const pairs = (dl) => dl.querySelectorAll("dt").map((dt, i) => [dt.textContent, dl.querySelectorAll("dd")[i].textContent]);
  assert.deepEqual(pairs(lists[0]), [["Input", "12k"], ["Output", "1.1k"], ["Cache read", "300"], ["Cache write", "200"], ["Total", "13.6k"],
    ["Cost", "$0.04"], ["Model", "claude-sonnet-4-5"], ["Session", "c0ffee"]]);
  assert.deepEqual(pairs(lists[1]), [["Input", "900"], ["Output", "80"], ["Total", "980"], ["Model", "claude-haiku-4-5"]]);
  assert.equal(lists[0].querySelector("dt.total").textContent, "Total");
  assert.equal(detail.querySelectorAll("p.muted").pop().textContent, "Took 2.5 min · Posted to GitHub");
  // Opening another closes this one; a second click closes it.
  w.fire(rowOf(2), "click");
  assert.equal(w.page.querySelectorAll(".history-detail").length, 1);
  detail = w.page.querySelector(".history-detail");
  assert.deepEqual(detail.querySelectorAll("p.muted").map((p) => p.textContent), ["No brief for this run.", "Not posted to GitHub"]);
  w.fire(rowOf(2), "click");
  assert.equal(w.page.querySelector(".history-detail"), null);
  assert.equal(rowOf(2).getAttribute("aria-expanded"), "false");
});

test("details for a silent engine, a failed review, and both outcomes of a brief run", async () => {
  const w = await X.open("history");
  const open = (i) => { w.fire(items(w)[i].querySelector(".list-row"), "click"); return w.page.querySelector(".history-detail"); };
  // pi reports no usage.
  let detail = open(3);
  assert.deepEqual(detail.querySelectorAll("h3").map((h) => h.textContent), ["Review usage", "Brief usage"]);
  assert.deepEqual(detail.querySelectorAll("p.muted").map((p) => p.textContent), ["Not reported by the engine.", "No brief for this run."]);
  // A failed review: its partial usage, and no brief section at all.
  detail = open(4);
  assert.deepEqual(detail.querySelectorAll("h3").map((h) => h.textContent), ["Review usage"]);
  assert.deepEqual(detail.querySelectorAll("dt").map((d) => d.textContent), ["Input", "Total"]);
  // A brief run: its text and its own usage, never "Review usage".
  detail = open(7);
  assert.deepEqual(detail.querySelectorAll("h3").map((h) => h.textContent), ["Brief", "Brief usage"]);
  assert.equal(detail.querySelector(".detail-block p").textContent, "Adds retries to the upload path.");
  assert.deepEqual(detail.querySelectorAll("dt").map((d) => d.textContent), ["Input", "Output", "Total", "Cost"]);
  detail = open(8);
  assert.deepEqual(detail.querySelectorAll("p.muted").map((p) => p.textContent), ["This brief run failed; the previous brief was kept.", "Not reported by the engine."]);
});

test("a review whose brief came with it explains the missing brief usage", async () => {
  const entry = X.entry("review_completed", "acme/api#9", X.MIN, { agent: "claude", verdict: "approved", blocking_count: 0, nit_count: 0, brief: "Renames a flag.", usage: X.USAGE });
  const legacy = X.entry("review_completed", "acme/api#8", 2 * X.MIN, { agent: "claude", verdict: "approved", blocking_count: 0, nit_count: 0, exec_summary: "Old name, same text." });
  const w = await X.open("history", { "history.list": () => ({ entries: [entry, legacy] }) });
  w.fire(items(w)[0].querySelector(".list-row"), "click");
  assert.equal(w.page.querySelector(".history-detail").querySelectorAll("p.muted")[0].textContent, "No separate brief run (the review wrote it), or the engine reported nothing.");
  // Lines written before the rename carry `exec_summary`: still the brief.
  w.fire(items(w)[1].querySelector(".list-row"), "click");
  assert.equal(w.page.querySelector(".history-detail .detail-block p").textContent, "Old name, same text.");
});

test("filters: event and engine filter what is loaded; repo asks the daemon again", async () => {
  const w = await X.open("history", { "history.list": (q) => ({ entries: X.history().filter((e) => !q.repo || e.repo === q.repo) }) });
  assert.deepEqual(w.byId("history-filter-by-repo-list").querySelectorAll(".select-label").map((n) => n.textContent), ["All repos", "acme/api", "acme/web", "solo/tool"]);
  assert.deepEqual(w.byId("history-filter-by-engine-list").querySelectorAll(".select-label").map((n) => n.textContent), ["All engines", "cheap", "claude", "codex", "pi"]);
  assert.deepEqual(w.byId("history-filter-by-event-list").querySelectorAll(".select-label").map((n) => n.textContent),
    ["All events", "Review completed", "Review failed", "Review started", "Review skipped", "New commits", "New PR", "Brief", "Merged", "Closed"]);
  pick(w, "history-filter-by-event", "Review completed");
  assert.equal(items(w).length, 3);
  assert.equal(w.calls("history.list").length, 1, "an event filter needs no new request");
  pick(w, "history-filter-by-engine", "pi");
  assert.deepEqual(items(w).map((i) => i.querySelector(".list-key").textContent), ["acme/web#5"]);
  pick(w, "history-filter-by-engine", "cheap");
  assert.equal(w.page.querySelector(".list-empty").textContent, "No loaded event matches these filters.");
  pick(w, "history-filter-by-engine", "All engines");
  pick(w, "history-filter-by-event", "All events");
  // The repo filter is the daemon's: a fresh first page, busy meanwhile.
  pick(w, "history-filter-by-repo", "solo/tool");
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), "true");
  await w.settle();
  assert.deepEqual(w.calls("history.list")[1], { limit: 50, repo: "solo/tool" });
  assert.deepEqual(items(w).map((i) => i.querySelector(".list-key").textContent), ["solo/tool#7"]);
  // Repos and engines seen earlier stay on offer while one is filtered.
  assert.deepEqual(w.byId("history-filter-by-repo-list").querySelectorAll(".select-label").map((n) => n.textContent), ["All repos", "acme/api", "acme/web", "solo/tool"]);
  assert.equal(w.byId("history-filter-by-repo").textContent, "solo/tool");
});

test("Load more pages back with `before` until a short page, without duplicates", async () => {
  const all = page(120, 1);
  const w = await X.open("history", { "history.list": (q) => {
    const older = q.before ? all.filter((e) => e.ts <= q.before) : all;
    return { entries: older.slice(0, q.limit) };
  } });
  assert.equal(items(w).length, 50);
  w.fire(button(w.page, "Load more"), "click");
  assert.equal(w.page.querySelector(".load-more").textContent, "Loading…");
  await w.settle();
  assert.deepEqual(w.calls("history.list")[1], { limit: 50, before: all[49].ts });
  // The page overlaps on the boundary entry, which is not shown twice.
  assert.equal(items(w).length, 99);
  w.fire(button(w.page, "Load more"), "click");
  await w.settle();
  assert.deepEqual(w.calls("history.list")[2], { limit: 50, before: all[98].ts });
  assert.equal(items(w).length, 120);
  assert.equal(new Set(items(w).map((i) => i.querySelector(".list-key").textContent)).size, 120);
  assert.equal(w.page.querySelector(".load-more").textContent, "That's everything.");
  // With more behind, an empty filter result says where to look.
  const more = await X.open("history", { "history.list": () => ({ entries: page(50, 1) }) });
  pick(more, "history-filter-by-event", "Merged");
  assert.equal(more.page.querySelector(".list-empty").textContent, "No loaded event matches these filters. Load more to search further back.");
});

test("a poll puts new events on top and keeps the open detail and loaded pages", async () => {
  let entries = X.history();
  const w = await X.open("history", { "history.list": () => ({ entries: entries }) });
  w.fire(items(w)[1].querySelector(".list-row"), "click");
  entries = [X.entry("pr_merged", "acme/web#3", 0)].concat(entries);
  tick(w);
  await w.settle();
  assert.equal(items(w).length, 13);
  assert.equal(items(w)[0].querySelector(".ev").textContent, "Merged");
  assert.ok(items(w)[2].classList.contains("open"), "the opened review is still open, one row down");
  // The same answer again adds nothing.
  tick(w);
  await w.settle();
  assert.equal(items(w).length, 13);
  // Coming back to the page shows what was loaded at once, then refreshes.
  w.window.prxApp.go("general");
  w.window.prxApp.go("history");
  assert.equal(items(w).length, 13);
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), null);
});

test("a poll leaves an open filter dropdown alone, then catches up", async () => {
  let entries = X.history();
  const w = await X.open("history", { "history.list": () => ({ entries: entries }) });
  const trigger = w.byId("history-filter-by-event");
  trigger.focus();
  w.fire(trigger, "click");
  assert.equal(trigger.getAttribute("aria-expanded"), "true");
  entries = [X.entry("pr_merged", "acme/web#3", 0)].concat(entries);
  tick(w);
  await w.settle();
  assert.equal(w.byId("history-filter-by-event"), trigger, "the toolbar was not rebuilt under the open list");
  assert.equal(trigger.getAttribute("aria-expanded"), "true");
  assert.equal(items(w).length, 12);
  // Closed (still focused): the next poll draws both answers.
  w.fire(trigger, "keydown", { key: "Escape" });
  tick(w);
  await w.settle();
  assert.equal(items(w).length, 13);
});

test("empty history says so, with nothing to load", async () => {
  const w = await X.open("history", { "history.list": () => ({ entries: [] }) });
  assert.equal(w.page.querySelector(".empty-title").textContent, "No history yet");
  assert.equal(w.page.querySelector(".load-more").textContent, "");
  assert.equal(button(w.page, "Load more"), undefined);
  // An older daemon's answer without `entries` is the same.
  const bare = await X.open("history", { "history.list": () => ({}) });
  assert.equal(bare.page.querySelector(".empty-title").textContent, "No history yet");
});

test("a filter that matches nothing says so, and is not mistaken for an empty history", async () => {
  // The daemon has nothing left for one repo (its events were compacted away).
  const w = await X.open("history", { "history.list": (q) => ({ entries: q.repo === "solo/tool" ? [] : X.history() }) });
  pick(w, "history-filter-by-repo", "solo/tool");
  await w.settle();
  assert.deepEqual(w.calls("history.list")[1], { limit: 50, repo: "solo/tool" });
  assert.equal(w.page.querySelector(".empty-title"), null, "not the empty-history state");
  assert.equal(w.page.querySelector(".table-box .list-empty").textContent, "No loaded event matches these filters.");
  assert.equal(items(w).length, 0);
  assert.equal(button(w.page, "Load more"), undefined);
  assert.equal(w.page.querySelector(".load-more").textContent, "");
  // The filter stays selectable, and a poll keeps the message.
  assert.equal(w.byId("history-filter-by-repo").textContent, "solo/tool");
  tick(w);
  await w.settle();
  assert.equal(w.page.querySelector(".table-box .list-empty").textContent, "No loaded event matches these filters.");
  // Clearing it brings the history back.
  pick(w, "history-filter-by-repo", "All repos");
  await w.settle();
  assert.equal(items(w).length, 12);
  assert.equal(w.page.querySelector(".list-empty"), null);
  // An event filter picked on an empty history is a filter with no match too.
  const empty = await X.open("history", { "history.list": () => ({ entries: [] }) });
  pick(empty, "history-filter-by-event", "Merged");
  assert.equal(empty.page.querySelector(".empty-title"), null);
  assert.equal(empty.page.querySelector(".list-empty").textContent, "No loaded event matches these filters.");
  pick(empty, "history-filter-by-event", "All events");
  assert.equal(empty.page.querySelector(".empty-title").textContent, "No history yet");
});

test("daemon offline and daemon errors replace the list, and Try again recovers", async () => {
  let mode = "offline";
  const w = await X.open("history", {
    "config.get": () => { if (mode === "offline") X.offline(); return X.config(); },
    "history.list": () => { if (mode === "offline") X.offline(); if (mode === "error") throw bridgeError("daemon", "history.jsonl line 3: " + X.EVIL); return { entries: X.history() }; },
  });
  assert.equal(w.page.querySelector(".table-box .empty-title").textContent, "Daemon offline");
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), null);
  assert.equal(w.page.querySelector(".load-more").textContent, "");
  assert.equal(w.byId("daemon").textContent, "Daemon not running");
  mode = "error";
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".table-box .empty-title").textContent, "Couldn't load this page");
  assert.equal(w.page.querySelector(".table-box .empty-text").textContent, "history.jsonl line 3: " + X.EVIL);
  assert.deepEqual(X.markupIn(w.page), []);
  mode = "up";
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(items(w).length, 12);
});

test("a failing Load more shows the error; one answered after leaving is dropped", async () => {
  let fail = false;
  const handlers = { "history.list": (q) => { if (fail && q.before) throw bridgeError("daemon", "history unreadable"); return { entries: page(50, 1) }; } };
  const w = await X.open("history", handlers);
  fail = true;
  w.fire(button(w.page, "Load more"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".table-box .empty-text").textContent, "history unreadable");
  // Leave before the page arrives: nothing is drawn into the new page.
  fail = false;
  const v = await X.open("history", handlers);
  v.fire(button(v.page, "Load more"), "click");
  v.window.prxApp.go("raw");
  const raw = v.page.textContent;
  await v.settle();
  assert.equal(v.page.textContent, raw);
});
