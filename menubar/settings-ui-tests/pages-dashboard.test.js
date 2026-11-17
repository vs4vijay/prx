// The Dashboard page end to end (pages-dashboard.js with the real shell,
// bridge, form helpers and brief card) against the fake DOM: loading,
// populated, first run, daemon offline and failed states, the review mode and
// pause panel, the links into the other pages, polling, and hostile PR
// content shown as text.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const D = require("../settings-ui/pages-dashboard.js");
const O = require("../settings-ui/overview.js");
const X = require("./page-fixtures.js");
const { boot, plain, withText, bridgeError } = require("./fake-dom.js");

const texts = (root, selector) => root.querySelectorAll(selector).map((n) => n.textContent);
const stats = (w) => w.page.querySelectorAll(".stats")[0].querySelectorAll(".stat").map((s) => [s.querySelector(".stat-value").textContent, s.querySelector(".stat-label").textContent]);
const section = (w, title) => w.page.querySelectorAll("section").find((s) => s.querySelector("h2") && s.querySelector("h2").textContent === title);
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");

// ---- view model edge cases ----------------------------------------------------

test("model: an idle daemon with nothing tracked is all zeros, not NaN", () => {
  const m = D.dashboardModel({ prs: [] }, { entries: [] }, null, null, X.NOW);
  assert.deepEqual(m.counts, { tracked: 0, attention: 0, ready: 0, running: 0, inFlight: 0, queued: 0 });
  assert.deepEqual(m.attention, []);
  assert.deepEqual(m.recent, []);
  assert.equal(m.attentionMore, 0);
  assert.equal(m.week, null);
  assert.equal(m.usageUnsupported, false);
  assert.equal(m.mode.mode, "review comment");
  assert.equal(m.mode.label, "Review and comment");
  // An older daemon's payload: no prs, no history, no mode.
  assert.deepEqual(D.dashboardModel({}, null, null, null, X.NOW).counts, m.counts);
});

test("model: terminal PRs are not tracked; attention is capped with a remainder", () => {
  const many = Array.from({ length: 8 }, (_, i) => X.pr("acme/api#" + (i + 1), "reviewed_changes_requested", { last_review_at: X.ago((i + 1) * X.HOUR) }));
  const prs = many.concat([X.pr("acme/api#20", "merged"), X.pr("acme/api#21", "closed"), X.pr("acme/api#22", "in_review")]);
  const m = D.dashboardModel({ prs: prs, in_flight: 1 }, null, null, null, X.NOW);
  assert.equal(m.counts.tracked, 9);
  assert.equal(m.counts.attention, 9);
  assert.deepEqual(m.attention.map((a) => a.key), ["acme/api#1", "acme/api#2", "acme/api#3", "acme/api#4", "acme/api#5"]);
  assert.equal(m.attentionMore, 4);
  assert.ok(m.attention.every((a) => a.blocking && a.stateId === "changes_requested"));
  // Equal review times fall back to key order (#9 before #10).
  const tie = [X.pr("a/b#10", "in_review"), X.pr("a/b#9", "in_review")];
  assert.deepEqual(D.dashboardModel({ prs: tie }, null, null, null, X.NOW).attention.map((a) => a.key), ["a/b#9", "a/b#10"]);
});

test("model: recent activity maps each event to a state pill or plain text", () => {
  const state = (event, extra) => D.entryState(Object.assign({ event: event }, extra || {}));
  assert.equal(state("review_completed", { verdict: "approved" }), "approved");
  assert.equal(state("review_completed", { verdict: "changes_requested" }), "changes_requested");
  assert.equal(state("review_completed", { verdict: "comment_only" }), "commented");
  assert.equal(state("review_completed", { verdict: "unheard_of" }), null);
  assert.equal(state("review_failed"), "failed");
  assert.equal(state("review_started"), "in_review");
  assert.equal(state("review_skipped"), "skipped");
  assert.equal(state("pr_merged"), "merged");
  assert.equal(state("pr_closed"), "closed");
  assert.equal(state("new_commits"), null);
  assert.equal(state("brief"), null);
  // Every pill id is a real state.
  O.EVENTS.forEach((e) => { const id = state(e[0], { verdict: "approved" }); if (id) assert.ok(O.STATES[id], e[0]); });
  const entries = X.history();
  const latest = D.latestPerPr(entries);
  assert.deepEqual(latest.map(O.entryKey), ["acme/api#2", "acme/web#6", "acme/web#3", "acme/web#5", "acme/api#1", "solo/tool#7", "acme/api#11", "acme/api#12"]);
  assert.equal(latest[1].event, "review_completed", "the newest event of a PR wins");
});

test("model: the week's cost is shown only when an engine reported one", () => {
  const usage = X.usage({ group: "none" });
  const m = D.dashboardModel(X.status(), null, usage, null, X.NOW);
  assert.deepEqual([m.week.tokens, m.week.tokensText, m.week.breakdown, m.week.cost, m.week.runs, m.week.withoutUsage],
    [3700, "3.7k", "3k in · 400 cache · 300 out", "$1.50", 2, 1]);
  usage.overall.runs_with_cost = 0;
  assert.equal(D.dashboardModel(X.status(), null, usage, null, X.NOW).week.cost, null);
  // Only `unsupported` means "update the daemon".
  assert.equal(D.dashboardModel(X.status(), null, null, { kind: "daemon" }, X.NOW).usageUnsupported, false);
  assert.equal(D.dashboardModel(X.status(), null, null, { kind: "unsupported" }, X.NOW).usageUnsupported, true);
});

test("the queries one refresh sends", () => {
  assert.deepEqual(D.QUERIES, { status: {}, history: { limit: 24 }, usage: { bucket: "day", window: "7d", group: "none" } });
});

// ---- the page -------------------------------------------------------------------

test("loading: a busy skeleton until the daemon answers", () => {
  const w = boot({ hash: "#page=dashboard", now: X.NOW, daemon: X.daemon(), hold: () => true });
  const body = w.page.querySelector(".overview-body");
  assert.equal(body.getAttribute("aria-busy"), "true");
  assert.equal(body.querySelectorAll(".skeleton-row").length, 3);
  assert.equal(w.page.querySelector("h1").textContent, "Dashboard");
  assert.equal(w.byId("daemon").textContent, "Connecting…");
  assert.deepEqual(w.sent.map((m) => m.method).sort(), ["config.get", "config.get", "history.list", "status.get", "usage.get"]);
});

test("populated: counts, attention, recent activity and the week's usage", async () => {
  const w = await X.open("dashboard");
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), null);
  assert.equal(w.byId("daemon").textContent, "Daemon running");
  assert.deepEqual(stats(w), [["10", "Tracked PRs"], ["2", "Need attention"], ["1", "Ready for approval"], ["3", "Running or queued"]]);
  assert.equal(w.page.querySelector(".stat .stat-detail").textContent, "1 running · 2 queued");
  assert.deepEqual(w.page.querySelectorAll(".stats")[0].querySelectorAll(".stat").map((s) => s.className), ["stat", "stat alert", "stat good", "stat"]);

  const attention = section(w, "Needs attention");
  assert.deepEqual(texts(attention, ".attention-item .list-key"), ["acme/web#6", "acme/api#2"]);
  assert.deepEqual(texts(attention, ".attention-item .state-text"), ["Changes requested", "In review"]);
  assert.deepEqual(attention.querySelectorAll(".attention-item .state").map((s) => s.className), ["state tone-error", "state tone-progress"]);
  assert.deepEqual(texts(attention, ".attention-item .list-title"), [X.LONG, X.UNICODE]);
  assert.deepEqual(texts(attention, ".attention-item .list-aside"), ["2 blocking", "reviewing for 3m"]);
  // Each attention row carries its brief card.
  const cards = attention.querySelectorAll("section.brief-card");
  assert.deepEqual(cards.map((c) => c.getAttribute("aria-label")), ["Brief for acme/web#6", "Brief for acme/api#2"]);
  assert.equal(cards[0].querySelector(".brief-text").textContent, "Rewrites the session store.");
  assert.ok(cards[0].classList.contains("stale"));
  assert.equal(cards[1].querySelector(".brief-action").textContent, "Write brief");

  const recent = section(w, "Recent activity");
  assert.equal(recent.querySelectorAll(".list-row").length, 6);
  assert.deepEqual(texts(recent, ".list-key"), ["acme/api#2", "acme/web#6", "acme/web#3", "acme/web#5", "acme/api#1", "solo/tool#7"]);
  assert.deepEqual(texts(recent, ".list-when"), ["3m ago", "30m ago", "2h ago", "6h ago", "7h ago", "8h ago"]);
  assert.deepEqual(texts(recent, ".list-aside"), ["Review started", "Changes requested · 2 blocking, 1 nit", "Approved · 2 nits",
    "Commented · 0 nits", "Review failed", "Skipped: draft " + X.EVIL]);
  assert.deepEqual(recent.querySelectorAll(".list-aside .state").map((s) => s.dataset.state),
    ["in_review", "changes_requested", "approved", "commented", "failed", "skipped"]);
  assert.equal(recent.querySelector("time").getAttribute("datetime"), X.ago(3 * X.MIN));

  const week = section(w, "Last 7 days");
  assert.deepEqual(week.querySelectorAll(".stat").map((s) => texts(s, "span")),
    [["3.7k", "Tokens", "3k in · 400 cache · 300 out"], ["$1.50", "Reported cost"], ["2", "Runs", "1 without usage data"]]);
  assert.deepEqual(w.calls("usage.get"), [{ bucket: "day", window: "7d", group: "none" }]);
  assert.deepEqual(w.calls("history.list"), [{ limit: 24 }]);
});

test("hostile PR titles, authors and reasons are text, never elements or handlers", async () => {
  const w = await X.open("dashboard");
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
  const recent = section(w, "Recent activity");
  assert.ok(texts(recent, ".list-title").includes(X.EVIL), "the title is shown exactly as sent");
  assert.ok(w.page.textContent.includes("<script>alert(2)</script>"));
});

test("review mode and pause: the posture panel, Change, pause and resume", async () => {
  let paused = null;
  const w = await X.open("dashboard", {
    "status.get": () => X.status((s) => { s.review_mode = "monitor"; if (paused) s.paused_until = paused; }),
    "pause.start": (p) => { paused = new Date(X.NOW + p.hours * X.HOUR).toISOString(); return { ok: true, paused_until: paused }; },
    "pause.resume": () => { paused = null; return { ok: true, paused_until: null }; },
  });
  const mode = w.page.querySelector(".posture-mode");
  assert.equal(mode.dataset.mode, "monitor");
  assert.deepEqual(texts(mode, "p"), ["Review mode", "Monitor", "Reviews PRs but never writes to GitHub."]);
  assert.equal(w.page.querySelector(".pause-panel .pause-title").textContent, "Not paused");
  assert.equal(w.page.querySelector(".pause-panel").classList.contains("paused"), false);
  assert.equal(button(w.page, "Resume reviews"), undefined);

  // Pick "For 2 hours" from the pause dropdown.
  const trigger = w.page.querySelector(".pause-select [role=combobox]");
  assert.equal(trigger.textContent, "Pause for…");
  w.fire(trigger, "click");
  w.fire(w.page.querySelectorAll(".pause-select [role=option]")[2], "click");
  await w.settle();
  assert.deepEqual(w.calls("pause.start"), [{ hours: 2 }]);
  const panel = w.page.querySelector(".pause-panel");
  assert.ok(panel.classList.contains("paused"));
  assert.equal(panel.querySelector(".pause-title").textContent, "Paused, 2h left");
  // The pick was an action, not a setting: the trigger shows no selection.
  assert.equal(w.page.querySelector(".pause-select [role=combobox]").textContent, "Pause for…");

  w.fire(button(w.page, "Resume reviews"), "click");
  await w.settle();
  assert.deepEqual(w.calls("pause.resume"), [{}]);
  assert.equal(w.page.querySelector(".pause-panel .pause-title").textContent, "Not paused");

  w.fire(button(w.page.querySelector(".posture-mode"), "Change"), "click");
  assert.equal(w.window.prxApp.state.page, "general");
});

test("review mode Off is marked; an unknown mode still renders", async () => {
  const off = await X.open("dashboard", { "status.get": () => X.status((s) => { s.review_mode = "off"; }) });
  assert.ok(off.page.querySelector(".posture-mode").classList.contains("off"));
  assert.equal(off.page.querySelector(".posture-mode .pause-title").textContent, "Off");
  const odd = await X.open("dashboard", { "status.get": () => X.status((s) => { s.review_mode = "future mode"; }) });
  assert.equal(odd.page.querySelector(".posture-mode .pause-title").textContent, "future mode");
  assert.equal(stats(odd).length, 4);
});

test("a failed pause shows inline and the page keeps its data", async () => {
  const w = await X.open("dashboard", { "pause.start": X.failing("pause refused: review mode is off") });
  w.fire(w.page.querySelector(".pause-select [role=combobox]"), "click");
  w.fire(w.page.querySelectorAll(".pause-select [role=option]")[0], "click");
  await w.settle();
  assert.deepEqual(w.calls("pause.start"), [{ hours: 0.5 }]);
  const error = w.page.querySelector(".pause-error");
  assert.equal(error.hidden, false);
  assert.equal(error.textContent, "pause refused: review mode is off");
  assert.equal(stats(w).length, 4);
});

test("a broken config shows the banner with the daemon's message, linking to config.yml", async () => {
  const w = await X.open("dashboard", { "status.get": () => X.status((s) => { s.config_error = { message: "repos[acme/api].schedule: " + X.EVIL, at: X.ago(X.MIN) }; }) });
  const banner = w.page.querySelector(".banner.error");
  assert.equal(banner.getAttribute("role"), "alert");
  assert.equal(banner.querySelector("strong").textContent, "config.yml has an error. ");
  assert.equal(banner.querySelector("code").textContent, "repos[acme/api].schedule: " + X.EVIL);
  assert.deepEqual(X.markupIn(banner), []);
  w.fire(button(banner, "Open config.yml"), "click");
  assert.equal(w.window.prxApp.state.page, "raw");
  // No message at all still explains itself.
  const bare = await X.open("dashboard", { "status.get": () => X.status((s) => { s.config_error = {}; }) });
  assert.equal(bare.page.querySelector(".banner.error code").textContent, "invalid configuration");
});

test("first run: nothing watched says how to start instead of zeros", async () => {
  const empty = (c) => { c.raw.repos = []; c.raw.orgs = []; };
  const w = await X.open("dashboard", { "config.get": () => X.config(empty), "status.get": () => X.status((s) => { s.prs = []; s.in_flight = 0; s.queue_depth = 0; }), "history.list": () => ({ entries: [] }) });
  assert.equal(w.page.querySelector(".onboarding .empty-title").textContent, "Watch your first repo");
  assert.equal(w.page.querySelector(".stats"), null);
  // The mode and pause are still shown above it.
  assert.ok(w.page.querySelector(".posture"));
  w.fire(button(w.page, "Add a repo"), "click");
  assert.equal(w.window.prxApp.state.page, "repos");
});

test("repos watched but no PRs or history yet: zeros and the empty lines", async () => {
  const w = await X.open("dashboard", { "status.get": () => X.status((s) => { s.prs = []; s.in_flight = 0; s.queue_depth = 0; }), "history.list": () => ({ entries: [] }) });
  assert.equal(w.page.querySelector(".onboarding"), null);
  assert.deepEqual(stats(w).map((s) => s[0]), ["0", "0", "0", "0"]);
  assert.deepEqual(w.page.querySelectorAll(".stats")[0].querySelectorAll(".stat").map((s) => s.className), ["stat", "stat", "stat", "stat"]);
  assert.deepEqual(texts(w.page, ".list-empty"), ["Nothing needs you right now.", "No history yet. Reviews, skips and merges show up here as PRX sees them."]);
});

test("more attention than fits says how many more", async () => {
  const many = Array.from({ length: 7 }, (_, i) => X.pr("acme/api#" + (i + 1), "reviewed_changes_requested", { last_review_at: X.ago((i + 1) * X.HOUR) }));
  const w = await X.open("dashboard", { "status.get": () => X.status((s) => { s.prs = many; }) });
  const attention = section(w, "Needs attention");
  assert.equal(attention.querySelectorAll(".attention-item").length, 5);
  assert.deepEqual(texts(attention, ".list-empty"), ["+2 more"]);
});

test("usage: an older daemon and a failing /usage each get their own note", async () => {
  const old = await X.open("dashboard", { "usage.get": () => { throw bridgeError("unsupported", "no /usage"); } });
  const note = section(old, "Last 7 days").querySelector(".list-empty");
  assert.equal(note.textContent, "Update the PRX daemon to see token usage (it has no /usage endpoint yet).");
  assert.equal(note.querySelector("code").textContent, "/usage");
  // The rest of the page is unaffected.
  assert.equal(stats(old).length, 4);
  const broken = await X.open("dashboard", { "usage.get": X.failing("usage log unreadable") });
  assert.equal(section(broken, "Last 7 days").querySelector(".list-empty").textContent, "Token usage is unavailable right now.");
  assert.equal(stats(broken).length, 4);
  // No engine reported a cost: a dash, with the reason.
  const free = await X.open("dashboard", { "usage.get": () => { const u = X.usage({ group: "none" }); u.overall.runs_with_cost = 0; u.runs_without_usage = 0; return u; } });
  assert.deepEqual(section(free, "Last 7 days").querySelectorAll(".stat").map((s) => texts(s, "span")).slice(1),
    [["—", "Reported cost", "No engine reported a cost"], ["2", "Runs"]]);
});

test("daemon offline: the offline state, the sidebar, and recovery on Try again", async () => {
  let up = false;
  const gate = (answer) => () => { if (!up) X.offline(); return answer(); };
  const w = await X.open("dashboard", { "config.get": gate(X.config), "status.get": gate(X.status), "history.list": gate(() => ({ entries: X.history() })), "usage.get": gate(() => X.usage({})) });
  assert.equal(w.page.querySelector(".empty-title").textContent, "Daemon offline");
  assert.match(w.page.querySelector(".empty-text").textContent, /PRX starts it on its own.*choose Start daemon in the PRX menu bar, or run prx daemon start/);
  assert.equal(w.page.querySelector(".overview-body").getAttribute("aria-busy"), null);
  assert.equal(w.byId("daemon").textContent, "Daemon not running");
  assert.equal(w.byId("daemon").className, "daemon off");
  assert.equal(w.page.querySelector(".stats"), null);
  up = true;
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(w.byId("daemon").textContent, "Daemon running");
  assert.equal(stats(w).length, 4);
  // The settings pages got their config too once the daemon was back.
  assert.equal(plain(w.window.prxApp.app.draft()).defaults.engine, "claude");
});

test("a daemon error shows the message and a retry, not stale data", async () => {
  let fail = false;
  const w = await X.open("dashboard", { "status.get": () => { if (fail) throw bridgeError("daemon", "state file is corrupt: " + X.EVIL); return X.status(); } });
  assert.equal(stats(w).length, 4);
  fail = true;
  w.intervals.find((i) => i.ms === 10000).run();
  await w.settle();
  assert.equal(w.page.querySelector(".empty-title").textContent, "Couldn't load this page");
  assert.equal(w.page.querySelector(".empty-text").textContent, "state file is corrupt: " + X.EVIL);
  assert.equal(w.page.querySelector(".stats"), null);
  assert.deepEqual(X.markupIn(w.page), []);
  // A plain failure is not "offline".
  assert.equal(w.byId("daemon").textContent, "Daemon running");
  fail = false;
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(stats(w).length, 4);
});

test("the counts link into Pull requests with the matching filter", async () => {
  for (const [label, state, shown] of [["Tracked PRs", "", "12 PRs"], ["Need attention", "attention", "2 of 12"], ["Ready for approval", "ready", "1 of 12"], ["Running or queued", "in_review", "1 of 12"]]) {
    const w = await X.open("dashboard");
    w.fire(withText(w.page, label)[0].closest("button"), "click");
    await w.settle();
    assert.equal(w.window.prxApp.state.page, "prs", label);
    assert.equal(plain(w.window.prxApp.app.view("prs").filter).state, state, label);
    assert.equal(w.page.querySelector(".toolbar .count").textContent, shown, label);
  }
  const w = await X.open("dashboard");
  w.fire(button(section(w, "Recent activity"), "History"), "click");
  assert.equal(w.window.prxApp.state.page, "history");
  w.window.prxApp.go("dashboard");
  await w.settle();
  w.fire(button(section(w, "Last 7 days"), "Usage & stats"), "click");
  assert.equal(w.window.prxApp.state.page, "usage");
});

test("polling refreshes in place, and an answer for a page already left is dropped", async () => {
  let prs = X.prs();
  const w = await X.open("dashboard", { "status.get": () => X.status((s) => { s.prs = prs; }) });
  assert.equal(stats(w)[0][0], "10");
  prs = prs.slice(0, 3);
  const tick = w.intervals.find((i) => i.ms === 10000).run;
  tick();
  await w.settle();
  assert.equal(stats(w)[0][0], "3");
  assert.equal(w.calls("status.get").length, 2);
  // Hidden windows do not poll.
  w.document.visibilityState = "hidden";
  tick();
  await w.settle();
  assert.equal(w.calls("status.get").length, 2);
  w.document.visibilityState = "visible";
  // Leave while a refresh is in flight: its answer must not draw anywhere.
  tick();
  w.window.prxApp.go("raw");
  const rawPage = w.page.textContent;
  await w.settle();
  assert.equal(w.calls("status.get").length, 3);
  assert.equal(w.page.textContent, rawPage);
  assert.equal(w.page.querySelector(".stats"), null);
});

test("Write brief asks the daemon once and shows progress until the brief lands", async () => {
  let brief;
  let pending = false;
  const w = await X.open("dashboard", {
    "status.get": () => X.status((s) => { const pr = s.prs.find((p) => p.key === "acme/api#2"); pr.brief = brief; pr.brief_pending = pending; }),
    "brief.request": () => { pending = true; return { ok: true }; },
  });
  const card = () => section(w, "Needs attention").querySelectorAll("section.brief-card")[1];
  w.fire(card().querySelector(".brief-action"), "click");
  await w.settle();
  assert.deepEqual(w.calls("brief.request"), [{ key: "acme/api#2" }]);
  assert.equal(card().querySelector(".brief-action").textContent, "Writing brief…");
  assert.equal(card().querySelector(".brief-action").disabled, true);
  assert.equal(card().getAttribute("aria-busy"), "true");
  // A click while busy does nothing.
  w.fire(card().querySelector(".brief-action"), "click");
  await w.settle();
  assert.equal(w.calls("brief.request").length, 1);
  pending = false;
  brief = { text: "Adds a rate limiter. " + X.EVIL, created_at: X.ago(0), updated_at: X.ago(0), head_sha: "new", source: "on_demand", stale: false };
  w.intervals.find((i) => i.ms === 10000).run();
  await w.settle();
  assert.equal(card().querySelector(".brief-text").textContent, "Adds a rate limiter. " + X.EVIL);
  assert.equal(card().querySelector(".brief-action").textContent, "Regenerate");
  assert.equal(card().getAttribute("aria-busy"), null);
  assert.deepEqual(X.markupIn(card()), []);
});

test("a refused brief shows why, inline, on its own card only", async () => {
  const w = await X.open("dashboard", { "brief.request": () => { throw bridgeError("daemon", "briefs are off for acme/api"); } });
  const cards = () => section(w, "Needs attention").querySelectorAll("section.brief-card");
  w.fire(cards()[1].querySelector(".brief-action"), "click");
  await w.settle();
  assert.equal(cards()[1].querySelector("[role=alert]").textContent, "Couldn't write the brief: briefs are off for acme/api");
  assert.equal(cards()[0].querySelector("[role=alert]"), null);
  // Offline has its own wording.
  const off = await X.open("dashboard", { "brief.request": X.offline });
  off.fire(section(off, "Needs attention").querySelectorAll(".brief-action")[1], "click");
  await off.settle();
  assert.equal(section(off, "Needs attention").querySelectorAll("section.brief-card")[1].querySelector("[role=alert]").textContent,
    "Couldn't write the brief: the PRX daemon is not running.");
});

// ---- poll health -------------------------------------------------------------

const TLS = "tls: failed to verify certificate: x509: OSStatus -26276";
const failing = (n, since) => ({ consecutive_failures: n, last_error: TLS, failing_since: since, poll_interval_minutes: 15 });

test("model: the sync warning needs three failed polls and names the repos", () => {
  const base = { prs: [], poll_health: { "acme/api": failing(2, X.ago(3 * 60 * X.MIN)), "acme/web": { consecutive_failures: 0, last_success: X.ago(X.MIN) } } };
  assert.equal(D.dashboardModel(base, null, null, null, X.NOW).sync, null);
  assert.equal(D.dashboardModel({ prs: [] }, null, null, null, X.NOW).sync, null, "an older daemon reports nothing");
  base.poll_health["acme/api"] = failing(5, X.ago(3 * 60 * X.MIN));
  const one = D.dashboardModel(base, null, null, null, X.NOW).sync;
  assert.deepEqual(one, { repos: ["acme/api"], label: "acme/api", failures: 5, error: TLS, since: "3h ago", restart: false });
  base.poll_health["acme/web"] = failing(7, X.ago(60 * X.MIN));
  base.restart_recommended = true;
  const all = D.dashboardModel(base, null, null, null, X.NOW).sync;
  assert.equal(all.label, "all 2 repos");
  assert.equal(all.failures, 7);
  assert.equal(all.since, "3h ago", "the earliest streak");
  assert.equal(all.restart, true);
});

test("failing polls show a banner with the error, as text", async () => {
  const w = await X.open("dashboard", { "status.get": () => X.status((s) => {
    s.poll_health = { "acme/api": Object.assign(failing(4, X.ago(2 * 60 * X.MIN)), { last_error: X.EVIL }) };
    s.restart_recommended = true;
  }) });
  const banner = w.page.querySelector(".banner.error");
  assert.equal(banner.getAttribute("role"), "alert");
  assert.equal(banner.querySelector("strong").textContent, "PRX can't sync with GitHub. ");
  assert.match(banner.textContent, /Polls for acme\/api have failed 4 times in a row \(since 2h ago\)/);
  assert.equal(banner.querySelector("code").textContent, X.EVIL);
  assert.match(banner.textContent, /PRX restarts it on its own/);
  assert.deepEqual(X.markupIn(banner), []);
  // Synced again: the banner goes away on the next poll.
  const healthy = await X.open("dashboard");
  assert.equal(healthy.page.querySelector(".banner.error"), null);
});
