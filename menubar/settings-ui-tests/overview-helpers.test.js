// Boundary cases of the Overview pages' shared helpers (overview.js): number
// and age formatting at every unit change, the PR state system for every
// lifecycle state, verdict and skip reason, PR keys, usage summaries, and the
// poller that keeps a mounted page fresh.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const O = require("../settings-ui/overview.js");

const NOW = Date.UTC(2026, 9, 14, 12, 0, 0);
const before = (seconds) => new Date(NOW - seconds * 1000).toISOString();

test("formatTokens: each unit ends where it would read 1000", () => {
  const cases = [[0, "0"], [1, "1"], [999, "999"], [999.4, "999"], [1000, "1k"], [1049, "1k"], [1050, "1.1k"], [9999, "10k"], [99949, "99.9k"],
    [99950, "100k"], [123456, "123k"], [999499, "999k"], [999500, "1M"], [999999, "1M"], [1e6, "1M"], [1.25e6, "1.3M"], [999.4e6, "999M"],
    [999.5e6, "1B"], [2.5e9, "2.5B"], [1e12, "1000B"]];
  cases.forEach(([n, text]) => assert.equal(O.formatTokens(n), text, String(n)));
  // No value ever reads "1000k" or "1000M".
  for (let n = 990000; n <= 1010000; n += 37) assert.doesNotMatch(O.formatTokens(n), /^1000/, String(n));
  for (const junk of [null, undefined, "", "abc", NaN]) assert.equal(O.formatTokens(junk), "0", String(junk));
  assert.equal(O.formatTokens("1500"), "1.5k");
});

test("formatCost and formatCount", () => {
  assert.equal(O.formatCost(0), "$0.00");
  assert.equal(O.formatCost(0.0099), "<$0.01");
  assert.equal(O.formatCost(0.01), "$0.01");
  assert.equal(O.formatCost(0.005), "<$0.01");
  assert.equal(O.formatCost(12.345), "$12.35");
  assert.equal(O.formatCost(1234.5), "$1234.50");
  assert.equal(O.formatCost(null), "");
  assert.equal(O.formatCount(0, "run"), "0 runs");
  assert.equal(O.formatCount(1, "run"), "1 run");
  assert.equal(O.formatCount(2, "reply", "replies"), "2 replies");
  assert.equal(O.findings(0, 0), "0 nits");
  assert.equal(O.findings(0, 1), "1 nit");
  assert.equal(O.findings(3, undefined), "3 blocking, 0 nits");
});

test("shortAge changes unit at each boundary and never reads NaN", () => {
  const DAY = 86400;
  const cases = [[0, "now"], [59, "now"], [60, "1m"], [3599, "59m"], [3600, "1h"], [DAY - 1, "23h"], [DAY, "1d"], [14 * DAY - 1, "13d"],
    [14 * DAY, "2w"], [60 * DAY - 1, "8w"], [60 * DAY, "2mo"], [730 * DAY - 1, "24mo"], [730 * DAY, "2y"], [3650 * DAY, "10y"]];
  cases.forEach(([seconds, text]) => assert.equal(O.shortAge(before(seconds), NOW), text, seconds + "s"));
  // A clock ahead of ours is "now", not a negative age.
  assert.equal(O.shortAge(before(-300), NOW), "now");
  for (const missing of [null, undefined, "", "not a date", "2026-13-45T99:00:00Z"]) {
    assert.equal(O.shortAge(missing, NOW), "", String(missing));
    assert.equal(O.ago(missing, NOW), "", String(missing));
  }
  assert.equal(O.ago(before(59), NOW), "just now");
  assert.equal(O.ago(before(60), NOW), "1m ago");
  assert.equal(O.ago(before(3 * DAY), NOW), "3d ago");
});

test("pauseLeft rounds up to the minute and drops zero minutes", () => {
  const cases = [[0, "<1m left"], [59999, "<1m left"], [60000, "1m left"], [60001, "2m left"], [59 * 60000, "59m left"], [3600000, "1h left"],
    [72 * 60000, "1h 12m left"], [2 * 3600000, "2h left"], [25 * 3600000 + 1, "25h 1m left"], [-5, "<1m left"]];
  cases.forEach(([ms, text]) => assert.equal(O.pauseLeft(ms), text, ms + "ms"));
  assert.deepEqual(O.pauseState(null, NOW), { paused: false, until: null, left: "" });
  assert.deepEqual(O.pauseState({ paused_until: "garbage" }, NOW), { paused: false, until: null, left: "" });
  assert.deepEqual(O.pauseState({ paused_until: new Date(NOW).toISOString() }, NOW), { paused: false, until: null, left: "" });
});

test("PR keys split on the last #, and sort by repo then number", () => {
  assert.deepEqual(O.splitKey("acme/api#42"), { repo: "acme/api", number: 42 });
  assert.deepEqual(O.splitKey("acme/c#-lib#7"), { repo: "acme/c#-lib", number: 7 });
  assert.deepEqual(O.splitKey("acme/api"), { repo: "acme/api", number: 0 });
  assert.deepEqual(O.splitKey("acme/api#x"), { repo: "acme/api#x", number: 0 });
  assert.deepEqual(O.splitKey(null), { repo: "", number: 0 });
  const keys = ["b/a#2", "a/b#10", "a/b#9", "a/a#100", "a/b#9"];
  assert.deepEqual(keys.slice().sort(O.compareKeys), ["a/a#100", "a/b#9", "a/b#9", "a/b#10", "b/a#2"]);
  assert.equal(O.compareKeys("a/b#1", "a/b#1"), 0);
  assert.equal(O.entryKey({ repo: "acme/api", number: 7 }), "acme/api#7");
});

test("every lifecycle state, verdict and approval combination has one state and group", () => {
  const of = (state, extra) => { const pr = Object.assign({ state: state }, extra || {}); return [O.stateOf(pr), O.groupOf(pr), O.needsAttention(pr), O.isTerminal(pr)]; };
  assert.deepEqual(of("pending"), ["needs_review", "needs_review", false, false]);
  assert.deepEqual(of("in_review"), ["in_review", "in_review", true, false]);
  assert.deepEqual(of("reviewed_approved", { last_verdict: "approved" }), ["ready", "ready", false, false]);
  assert.deepEqual(of("reviewed_approved", { last_verdict: "approved", human_approved_at: "2026-10-14T00:00:00Z" }), ["approved", "reviewed", false, false]);
  assert.deepEqual(of("reviewed_approved", { last_verdict: "comment_only" }), ["commented", "reviewed", false, false]);
  // A human approval without any agent verdict is approved, not "ready".
  assert.deepEqual(of("reviewed_approved"), ["approved", "reviewed", false, false]);
  assert.deepEqual(of("reviewed_changes_requested", { last_verdict: "changes_requested" }), ["changes_requested", "reviewed", true, false]);
  assert.deepEqual(of("skipped", { skip_reason: "draft" }), ["skipped", "skipped", false, false]);
  assert.deepEqual(of("merged", { last_verdict: "approved" }), ["merged", "terminal", false, true]);
  assert.deepEqual(of("closed"), ["closed", "terminal", false, true]);
  // An approved verdict never makes a merged PR "ready for approval".
  assert.equal(O.readyForApproval({ state: "merged", last_verdict: "approved" }), false);
  // Every state id is drawn with a label, a tone and a glyph; groups are labelled.
  Object.keys(O.STATES).forEach((id) => { const v = O.stateView(id); assert.ok(v.label && v.tone && v.glyph, id); assert.equal(v.id, id); });
  assert.deepEqual(O.stateView("mystery"), { id: "mystery", label: "mystery", tone: "neutral", glyph: "s-waiting" });
  O.GROUPS.forEach((g) => assert.equal(O.groupLabel(g[0]), g[1]));
  assert.equal(O.groupLabel("mystery"), "mystery");
  assert.deepEqual(Object.keys(O.VERDICT_STATES), ["approved", "changes_requested", "comment_only"]);
});

test("labels: skip reasons, verdicts and modes, known and unknown", () => {
  const skipped = (reason) => O.stateLabel({ state: "skipped", skip_reason: reason });
  assert.deepEqual(["draft", "config", "snoozed", "ignored", "quota", null].map(skipped),
    ["Skipped (draft)", "Skipped (skip rule)", "Skipped (snoozed)", "Skipped (ignored)", "Skipped (quota)", "Skipped"]);
  // A skip reason on a PR that is no longer skipped is not shown.
  assert.equal(O.stateLabel({ state: "pending", skip_reason: "draft" }), "Needs review");
  assert.deepEqual(["approved", "changes_requested", "comment_only", "weird", null, undefined].map(O.verdictLabel),
    ["Approved", "Changes requested", "Commented", "weird", "", ""]);
  assert.deepEqual(["review comment", "monitor", "off", "future", undefined, ""].map(O.modeLabel),
    ["Review and comment", "Monitor", "Off", "future", "Review and comment", "Review and comment"]);
  assert.equal(O.eventLabel("pr_merged"), "Merged");
  assert.equal(O.eventLabel("unheard_of"), "unheard_of");
});

test("usage summaries: totals count every kind; nothing reported reads as nothing", () => {
  assert.equal(O.tokenTotal(null), 0);
  assert.equal(O.tokenTotal({}), 0);
  assert.equal(O.tokenTotal({ input_tokens: 1, output_tokens: 2, cache_read_tokens: 3, cache_write_tokens: 4, cost_usd: 99 }), 10);
  assert.equal(O.hasUsage(null), false);
  assert.equal(O.hasUsage({ model: "m", session_id: "s" }), false);
  assert.equal(O.hasUsage({ input_tokens: null, cost_usd: null }), false);
  assert.equal(O.hasUsage({ output_tokens: 0 }), true, "a reported zero is still a report");
  assert.equal(O.hasUsage({ cost_usd: 0 }), true);
  assert.equal(O.usageLine({ output_tokens: 0 }), "0 tokens");
  assert.equal(O.usageLine({ cost_usd: 0.5 }), "0 tokens · $0.50");
  assert.equal(O.usageLine({ input_tokens: 1500 }), "1.5k tokens");
  assert.equal(O.usageBreakdown(null), "");
  assert.equal(O.usageBreakdown({}), "0 in · 0 out");
  assert.equal(O.usageBreakdown({ cache_write_tokens: 2000 }), "0 in · 2k cache · 0 out");
});

test("watchesRepos and the failure wording", () => {
  assert.equal(O.watchesRepos(null), null);
  assert.equal(O.watchesRepos({}), null);
  assert.equal(O.watchesRepos({ raw: {} }), false);
  assert.equal(O.watchesRepos({ raw: { repos: [], orgs: [] } }), false);
  assert.equal(O.watchesRepos({ raw: { repos: [{ name: "a/b" }] } }), true);
  assert.equal(O.watchesRepos({ raw: { orgs: [{ org: "acme" }] } }), true);
  assert.equal(O.isOffline({ kind: "offline" }), true);
  assert.equal(O.isOffline({ kind: "daemon" }), false);
  assert.equal(O.isOffline(null), false);
  assert.match(O.pauseErrorText({ kind: "offline", message: "ECONNREFUSED" }), /isn't running, so nothing changed/);
  assert.equal(O.pauseErrorText({ kind: "daemon", message: "refused" }), "refused");
  assert.equal(O.pauseErrorText(null), "The daemon did not answer.");
  assert.equal(O.pauseErrorText({}), "The daemon did not answer.");
});

// ---- poller ---------------------------------------------------------------------

// A stand-in for the app shell: one mount, and a log of online() calls.
function fakeApp() {
  const app = { current: 1, online: [], mount: () => app.current, isMounted: (m) => m === app.current };
  const shell = { mount: app.mount, isMounted: app.isMounted, online: (up) => app.online.push(up) };
  return { app: app, shell: shell };
}
// A fetch whose answers the test settles by hand.
function manualFetch() {
  const pending = [];
  const fetch = () => new Promise((resolve, reject) => pending.push({ resolve: resolve, reject: reject }));
  return { fetch: fetch, pending: pending };
}

test("poller: an answer reaches the page and marks the daemon up", async () => {
  const { app, shell } = fakeApp();
  const { fetch, pending } = manualFetch();
  const seen = [];
  const live = O.poller(shell, fetch, (error, value) => seen.push([error, value]));
  const done = live.refresh();
  assert.equal(pending.length, 1);
  pending[0].resolve("data");
  await done;
  assert.deepEqual(seen, [[null, "data"]]);
  assert.deepEqual(app.online, [true]);
});

test("poller: offline marks the daemon down; other failures do not", async () => {
  const { app, shell } = fakeApp();
  const { fetch, pending } = manualFetch();
  const seen = [];
  const live = O.poller(shell, fetch, (error, value) => seen.push([error && error.kind, value]));
  let done = live.refresh();
  pending[0].reject({ kind: "offline", message: "down" });
  await done;
  done = live.refresh();
  pending[1].reject({ kind: "daemon", message: "boom" });
  await done;
  assert.deepEqual(seen, [["offline", null], ["daemon", null]]);
  assert.deepEqual(app.online, [false]);
  // A rejection without a reason is still an error for the page.
  const errors = [];
  const bare = O.poller(shell, () => Promise.reject(undefined), (error) => errors.push(error));
  await bare.refresh();
  assert.equal(errors.length, 1);
  assert.equal(errors[0].message, "failed");
});

test("poller: a refresh asked for mid-flight drops that answer and fetches once more", async () => {
  const { shell } = fakeApp();
  const { fetch, pending } = manualFetch();
  const seen = [];
  const live = O.poller(shell, fetch, (error, value) => seen.push(value));
  const flush = () => new Promise((resolve) => setImmediate(resolve));
  live.refresh();
  live.refresh();
  live.refresh();
  assert.equal(pending.length, 1, "no second request while one is in flight");
  pending[0].resolve("stale");
  await flush();
  assert.deepEqual(seen, [], "the stale answer is not drawn");
  assert.equal(pending.length, 2, "exactly one follow-up request");
  pending[1].resolve("fresh");
  await flush();
  assert.deepEqual(seen, ["fresh"]);
});

test("poller: nothing is fetched or drawn for a page that was left", async () => {
  const { app, shell } = fakeApp();
  const { fetch, pending } = manualFetch();
  const seen = [];
  const live = O.poller(shell, fetch, (error, value) => seen.push(value));
  const done = live.refresh();
  app.current = 2;
  pending[0].resolve("late");
  await done;
  assert.deepEqual(seen, []);
  assert.deepEqual(app.online, []);
  await live.refresh();
  assert.equal(pending.length, 1, "an unmounted page stops fetching");
});
