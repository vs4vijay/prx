// The PR details page end to end (pages-pr.js with the real shell, bridge,
// brief card and form helpers) against the fake DOM: opening it at a PR
// from the URL, prxApp.openPr and the Pull requests page; the header, the
// brief, the latest review from history (and from status without it); every
// action and the request it sends; the not-tracked, no-PR, offline states;
// and hostile PR content shown as text.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const D = require("../settings-ui/pages-pr.js");
const X = require("./page-fixtures.js");
const { plain, withText, bridgeError } = require("./fake-dom.js");

const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
const text = (w, selector) => { const n = w.page.querySelector(selector); return n ? n.textContent : null; };
const actions = (w) => w.page.querySelector(".pr-actions-row");
const act = (w, id) => actions(w).querySelector("button[data-action=" + id + "]");
function menu(w) {
  const anchor = w.page.querySelector(".pr-actions-row .menu-anchor");
  return { anchor: anchor, items: anchor.querySelectorAll("[role=menuitem]"), item: (label) => anchor.querySelectorAll("[role=menuitem]").find((i) => i.textContent === label) };
}
const navCurrent = (w) => w.byId("nav").querySelectorAll("button").filter((b) => b.getAttribute("aria-current") === "page").map((b) => b.dataset.page);

// ---- view model -------------------------------------------------------------

test("model: durations, keys and the header view of a status entry", () => {
  assert.deepEqual([0, 45, 60, 150, 3600, 3900].map(D.formatDuration), ["0s", "45s", "1m", "2m 30s", "1h", "1h 5m"]);
  assert.deepEqual([undefined, null, -1, NaN, "9"].map(D.formatDuration), ["", "", "", "", ""]);
  assert.deepEqual(D.splitPrKey("acme/web#6"), { repo: "acme/web", number: 6 });
  const m = D.detailModel(X.pr("acme/web#6", "reviewed_changes_requested", { last_verdict: "changes_requested", is_draft: true }), X.NOW);
  assert.deepEqual([m.key, m.stateId, m.state, m.opened, m.draft, m.reviewable], ["acme/web#6", "changes_requested", "Changes requested", "2d ago", true, true]);
  // An older daemon's bare entry: the key stands in for the title.
  const bare = D.detailModel({ key: "a/b#1", state: "merged" }, X.NOW);
  assert.deepEqual([bare.title, bare.url, bare.author, bare.opened, bare.terminal, bare.reviewable], ["a/b#1", "", "", "", true, false]);
});

test("model: the latest review is the PR's newest completed run, noting a later failure", () => {
  const h = X.history();
  const six = D.latestReview(h, "acme/web#6");
  assert.equal(six.entry.verdict, "changes_requested");
  assert.equal(six.failedSince, false);
  assert.deepEqual(D.latestReview(h, "acme/api#1"), { entry: null, failedSince: true });
  // Another PR's number in another repo never matches.
  assert.equal(D.latestReview([X.entry("review_completed", "acme/api#6", X.MIN, { verdict: "approved" })], "acme/web#6").entry, null);
  const failedLater = D.latestReview([X.entry("review_failed", "acme/web#6", X.MIN), h[1]], "acme/web#6");
  assert.equal(failedLater.failedSince, true);
  assert.equal(failedLater.entry, h[1]);
  assert.deepEqual(D.latestReview(undefined, "a/b#1"), { entry: null, failedSince: false });
});

test("model: the review section from history, from status, and for a PR never reviewed", () => {
  const status = X.status();
  const six = status.prs.find((p) => p.key === "acme/web#6");
  const r = D.reviewModel(six, D.latestReview(X.history(), six.key), X.NOW);
  assert.deepEqual([r.verdict, r.verdictState, r.findings, r.when, r.engine, r.posted, r.duration, r.blocking.length, r.unlisted, r.nits],
    ["Changes requested", "changes_requested", "2 blocking, 1 nit", "30m ago", "codex", "Posted to GitHub", "2m 30s", 2, 0, 1]);
  // No history entry (compacted, or beyond the page read): status counts.
  const three = status.prs.find((p) => p.key === "acme/web#3");
  const s = D.reviewModel(three, { entry: null, failedSince: false }, X.NOW);
  assert.deepEqual([s.verdict, s.findings, s.when, s.engine, s.posted, s.blocking], ["Approved", "2 nits", "2h ago", "", "", []]);
  const counted = D.reviewModel(six, { entry: X.entry("review_completed", "acme/web#6", X.MIN, { verdict: "changes_requested", blocking_count: 3, nit_count: 0, posted: false }) }, X.NOW);
  assert.deepEqual([counted.unlisted, counted.posted, counted.findings], [3, "Not posted", "3 blocking, 0 nits"]);
  assert.deepEqual(D.reviewModel(X.pr("a/b#1", "pending"), null, X.NOW), { reviewed: false, failedSince: false });
});

test("model: the actions row and the requests its review actions send", () => {
  const status = X.status();
  const six = status.prs.find((p) => p.key === "acme/web#6");
  const a = D.actionsFor(six, status);
  assert.deepEqual(a.buttons.map((b) => [b.id, b.label, b.disabled]), [["open", "Open in GitHub", false], ["review", "Review", false]]);
  assert.deepEqual(a.menu.map((m) => m.label || (m.heading ? "# " + m.heading : "---")),
    ["# Review with", "cheap", "claude", "codex (default)", "---", "Review without posting", "---", "Open Review Chat…"]);
  assert.deepEqual(a.menu.filter((m) => m.id && m.id !== "chat").map((m) => D.reviewRequest(m, six)),
    [{ target: "acme/web#6", engine: "cheap" }, { target: "acme/web#6", engine: "claude" }, { target: "acme/web#6", engine: "codex" }, { target: "acme/web#6", post: false }]);
  assert.deepEqual(D.reviewRequest(a.buttons[1], six), { target: "acme/web#6" });
  // Closed without a URL, older daemon without engines: no Open, review off.
  const closed = D.actionsFor(status.prs.find((p) => p.key === "acme/api#12"), {});
  assert.deepEqual(closed.buttons.map((b) => [b.id, b.disabled]), [["review", true]]);
  // Review Chat changes nothing in PRX: offered on a closed PR too.
  assert.deepEqual(closed.menu.filter((m) => m.id).map((m) => [m.id, m.disabled]), [["nopost", true], ["chat", false]]);
});

test("model: facts list only what the entry knows", () => {
  const facts = D.factsFor(X.pr("a/b#1", "reviewed_approved", { head_branch: "fix/retry", human_approved_at: X.ago(X.HOUR) }), X.NOW);
  assert.deepEqual(facts.map((f) => [f.label, f.value]), [["Branch", "fix/retry"], ["Review engine", "claude"], ["Opened", "2d ago"], ["You approved", "1h ago"]]);
  assert.deepEqual(D.factsFor({ key: "a/b#1" }, X.NOW), []);
  // Why prx is not reviewing it: the skip rule, or the diff size the cap held back.
  const skipped = D.factsFor(X.pr("a/b#2", "skipped", { skip_reason: "config", skip_detail: "author dependabot[bot] excluded" }), X.NOW);
  assert.deepEqual(skipped[0], { label: "Skipped", value: "author dependabot[bot] excluded" });
  const capped = D.factsFor(X.pr("a/b#3", "pending", { skip_detail: "2,140 lines changed (limit 1,500)" }), X.NOW);
  assert.deepEqual(capped[0], { label: "Review skipped", value: "2,140 lines changed (limit 1,500)" });
  assert.equal(D.detailModel(X.pr("a/b#3", "pending", { skip_detail: "2,140 lines changed (limit 1,500)" }), X.NOW).skip,
    "Review skipped — 2,140 lines changed (limit 1,500)");
  assert.equal(D.detailModel(X.pr("a/b#1", "pending"), X.NOW).skip, "");
});

test("model: a sync problem is the PR's own repo failing 3+ polls in a row", () => {
  const health = (n) => ({ consecutive_failures: n, last_error: "tls: failed to verify certificate", failing_since: X.ago(2 * X.HOUR) });
  assert.equal(D.syncProblem({ poll_health: { "acme/web": health(2) } }, "acme/web#6", X.NOW), null);
  assert.equal(D.syncProblem({ poll_health: { "acme/api": health(5) } }, "acme/web#6", X.NOW), null);
  assert.equal(D.syncProblem({}, "acme/web#6", X.NOW), null, "an older daemon sends no poll_health");
  assert.deepEqual(D.syncProblem({ poll_health: { "acme/web": health(3) } }, "acme/web#6", X.NOW),
    { repo: "acme/web", failures: 3, error: "tls: failed to verify certificate", since: "2h ago" });
});

// ---- the page -----------------------------------------------------------------

test("opened at a PR: header, brief, latest review with blocking findings, as text only", async () => {
  const w = await X.openPr("acme/web#6");
  assert.equal(w.window.prxApp.state.page, "pr");
  // The sidebar has no entry of its own; Pull requests is marked current.
  assert.ok(!w.byId("nav").querySelectorAll("button").some((b) => b.dataset.page === "pr"));
  assert.deepEqual(navCurrent(w), ["prs"]);
  assert.equal(text(w, "h1.pr-title"), X.LONG);
  assert.equal(text(w, ".pr-head-meta .mono"), "acme/web#6");
  assert.equal(text(w, ".pr-head-meta .state-text"), "Changes requested");
  assert.equal(text(w, ".brief-text"), "Rewrites the session store.");
  assert.ok(w.page.querySelector(".brief-card.stale"));
  // The latest review comes from the repo's history.
  assert.deepEqual(w.calls("history.list"), [{ repo: "acme/web", limit: D.HISTORY_LIMIT }]);
  const review = w.page.querySelector(".review-card");
  assert.equal(review.querySelector(".state-text").textContent, "Changes requested");
  assert.equal(review.querySelector(".review-findings").textContent, "2 blocking, 1 nit");
  assert.equal(review.querySelector(".badge").textContent, "Posted to GitHub");
  assert.deepEqual(review.querySelectorAll("li").map((li) => li.textContent), ["Unbounded query " + X.EVIL, "Missing index"]);
  assert.ok(withText(review, "Reviewed 30m ago with codex in 2m 30s").length);
  assert.ok(withText(review, "1 nit: read them in the review on GitHub.").length);
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
});

test("hostile titles, authors and briefs stay text", async () => {
  const w = await X.openPr("acme/web#3", { "status.get": () => X.status((s) => { s.prs[2].title = X.EVIL; s.prs[2].author = "<b>mallory</b>"; }) });
  assert.equal(text(w, "h1.pr-title"), X.EVIL);
  assert.ok(withText(w.page, "by <b>mallory</b>").length);
  assert.equal(text(w, ".brief-text"), "Adds retries to the upload path. " + X.EVIL);
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
});

test("actions: Review, Review with an engine, without posting, Open in GitHub", async () => {
  const w = await X.openPr("acme/web#6");
  assert.equal(actions(w).getAttribute("role"), "toolbar");
  assert.equal(actions(w).getAttribute("aria-label"), "Actions for acme/web#6");
  w.fire(act(w, "review"), "click");
  await w.settle();
  assert.equal(text(w, ".pr-actions .inline-note span"), "Queued acme/web#6.");
  w.fire(menu(w).item("codex (default)"), "click");
  await w.settle();
  w.fire(menu(w).item("Review without posting"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "acme/web#6" }, { target: "acme/web#6", engine: "codex" }, { target: "acme/web#6", post: false }]);
  w.fire(act(w, "open"), "click");
  assert.deepEqual(plain(w.window.opened), [["https://github.com/acme/web/pull/6", "_blank", "noopener"]]);
  // A refusal shows the daemon's words; Dismiss clears the note.
  const refused = await X.openPr("acme/web#6", { "review.request": X.failing("acme/web#6 is already being reviewed") });
  w.fire(act(refused, "review"), "click");
  await refused.settle();
  assert.equal(text(refused, ".pr-actions .inline-note.error span"), "acme/web#6 is already being reviewed");
  w.fire(refused.page.querySelector(".pr-actions button[aria-label=Dismiss]"), "click");
  assert.equal(refused.page.querySelector(".pr-actions .inline-note"), null);
});

test("merged PRs: review actions disabled, nothing is requested", async () => {
  const w = await X.openPr("acme/api#11");
  assert.equal(act(w, "review").disabled, true);
  assert.ok(menu(w).items.filter((i) => i.textContent !== "Open Review Chat…").every((i) => i.disabled));
  assert.equal(menu(w).item("Open Review Chat…").disabled, false);
  act(w, "review").click();
  await w.settle();
  assert.deepEqual(w.calls("review.request"), []);
});

test("Open Review Chat… asks PRX.app for a terminal; a refusal shows its words", async () => {
  const w = await X.openPr("acme/web#6");
  w.fire(menu(w).item("Open Review Chat…"), "click");
  await w.settle();
  assert.deepEqual(w.calls("app.review_chat.open"), [{ key: "acme/web#6" }]);
  assert.deepEqual(w.calls("review.request"), []);
  assert.equal(text(w, ".pr-actions .inline-note.ok span"), "Review Chat opened in Terminal.");
  const refused = await X.openPr("acme/web#6", { "app.review_chat.open": X.failing("cannot open Review Chat: the bundled prx command-line tool was not found") });
  refused.fire(menu(refused).item("Open Review Chat…"), "click");
  await refused.settle();
  assert.equal(text(refused, ".pr-actions .inline-note.error span"), "cannot open Review Chat: the bundled prx command-line tool was not found");
});

test("a skipped PR says why in its header and facts", async () => {
  const w = await X.openPr("solo/tool#8");
  assert.equal(text(w, ".pr-head-meta .pr-skip"), "author dependabot[bot] excluded");
  const facts = w.page.querySelector(".pr-facts-list");
  assert.deepEqual(facts.querySelectorAll("dt").map((n) => n.textContent).slice(0, 1), ["Skipped"]);
  assert.equal(facts.querySelectorAll("dd")[0].textContent, "author dependabot[bot] excluded");
  const plainPr = await X.openPr("acme/web#6");
  assert.equal(plainPr.page.querySelector(".pr-skip"), null);
});

test("Review on a skipped or oversized PR asks first; Cancel sends nothing, Review anyway sends it confirmed", async () => {
  const w = await X.openPr("solo/tool#8", { "review.preflight": (p) => X.riskyPreflight(p.target) });
  w.fire(act(w, "review"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.preflight"), [{ target: "solo/tool#8" }]);
  assert.deepEqual(w.calls("review.request"), [], "nothing is sent before the answer");
  let box = w.page.querySelector(".pr-actions .confirm-box");
  assert.equal(box.getAttribute("role"), "alertdialog");
  assert.equal(box.getAttribute("aria-label"), "Review solo/tool#8 anyway?");
  assert.deepEqual(box.querySelectorAll("li").map((li) => li.textContent),
    ["Skipped — author dependabot[bot] excluded", "Large PR — 2,140 lines changed (limit 1,500)"]);
  assert.match(box.querySelector(".confirm-cost").textContent, /stays skipped/);
  w.fire(button(box, "Cancel"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".confirm-box"), null);
  assert.deepEqual(w.calls("review.request"), []);
  assert.equal(text(w, ".pr-actions .inline-note span"), "Not reviewed.");

  // The ⋯ menu's actions ask too, and keep their options.
  w.fire(menu(w).item("Review without posting"), "click");
  await w.settle();
  box = w.page.querySelector(".pr-actions .confirm-box");
  w.fire(button(box, "Review anyway"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "solo/tool#8", post: false, confirm: true }]);
  assert.equal(w.page.querySelector(".confirm-box"), null);
  assert.equal(text(w, ".pr-actions .inline-note span"), "Queued solo/tool#8.");
  assert.deepEqual(X.markupIn(w.page), []);

  // No warnings, an older daemon without the route, or blocked: no box, sent as before.
  for (const preflight of [{ key: "acme/web#6", warnings: [] }, Object.assign(X.riskyPreflight("acme/web#6"), { blocked: "review mode is off for acme/web" })]) {
    const plainPr = await X.openPr("acme/web#6", { "review.preflight": () => preflight });
    plainPr.fire(act(plainPr, "review"), "click");
    await plainPr.settle();
    assert.equal(plainPr.page.querySelector(".confirm-box"), null);
    assert.deepEqual(plainPr.calls("review.request"), [{ target: "acme/web#6" }]);
  }
  const old = await X.openPr("acme/web#6", { "review.preflight": () => { throw bridgeError("bad_request", "unknown method review.preflight"); } });
  old.fire(act(old, "review"), "click");
  await old.settle();
  assert.deepEqual(old.calls("review.request"), [{ target: "acme/web#6" }]);
  // Any other pre-flight failure sends nothing and says why.
  const down = await X.openPr("acme/web#6", { "review.preflight": X.failing("gh failed") });
  down.fire(act(down, "review"), "click");
  await down.settle();
  assert.deepEqual(down.calls("review.request"), []);
  assert.equal(text(down, ".pr-actions .inline-note.error span"), "gh failed");
});

test("failing polls for the PR's repo: a banner says the page may be out of date", async () => {
  const failing = { consecutive_failures: 4, last_error: "tls: failed to verify certificate " + X.EVIL, failing_since: X.ago(2 * X.HOUR) };
  const w = await X.openPr("acme/web#6", { "status.get": () => X.status((s) => { s.poll_health = { "acme/web": failing }; }) });
  const banner = w.page.querySelector(".pr-page .banner.error");
  assert.equal(banner.getAttribute("role"), "alert");
  assert.match(banner.textContent, /^PRX can't sync acme\/web with GitHub\. Its polls have failed 4 times in a row \(since 2h ago\), so this page may be out of date/);
  assert.deepEqual(X.markupIn(w.page), []);
  // Another repo failing, or two failures: no banner.
  for (const health of [{ "acme/api": failing }, { "acme/web": Object.assign({}, failing, { consecutive_failures: 2 }) }]) {
    const other = await X.openPr("acme/web#6", { "status.get": () => X.status((s) => { s.poll_health = health; }) });
    assert.equal(other.page.querySelector(".pr-page .banner"), null);
  }
});

test("without a brief: Write brief requests one; never reviewed says so", async () => {
  const w = await X.openPr("acme/api#1");
  assert.equal(w.page.querySelector(".brief-text"), null);
  assert.ok(withText(w.page, "Not reviewed yet.").length);
  assert.ok(withText(w.page, "The last review run failed. See History for details.").length);
  w.fire(button(w.page.querySelector(".brief-card"), "Write brief"), "click");
  await w.settle();
  assert.deepEqual(w.calls("brief.request"), [{ key: "acme/api#1" }]);
});

test("history unavailable: the review falls back to the status entry", async () => {
  const w = await X.openPr("acme/web#3", { "history.list": X.failing("history file unreadable") });
  const review = w.page.querySelector(".review-card");
  assert.equal(review.querySelector(".state-text").textContent, "Approved");
  assert.equal(review.querySelector(".review-findings").textContent, "2 nits");
  assert.equal(review.querySelector("li"), null);
});

test("untracked, invalid or missing PR: an empty state that leads to Pull requests", async () => {
  const gone = await X.openPr("acme/web#99");
  assert.ok(withText(gone.page, "acme/web#99 isn't tracked").length);
  gone.fire(button(gone.page, "Show pull requests"), "click");
  await gone.settle();
  assert.equal(gone.window.prxApp.state.page, "prs");
  for (const hash of ["#page=pr", "#page=pr&pr=" + encodeURIComponent("acme/web#6\");alert(1)//"), "#page=pr&pr=%E0%A4%A"]) {
    const w = await X.openPr(hash);
    assert.ok(withText(w.page, "No pull request selected").length, hash);
    assert.deepEqual(w.calls("status.get"), [], hash);
  }
});

test("daemon offline: the shared offline state", async () => {
  const w = await X.openPr("acme/web#6", { "status.get": X.offline });
  assert.ok(withText(w.page, "Daemon offline").length);
});

test("prxApp.openPr, the URL and the Pull requests page all switch to a PR", async () => {
  const w = await X.open("dashboard");
  w.window.prxApp.openPr("acme/web#3");
  await w.settle();
  assert.equal(w.window.prxApp.state.page, "pr");
  assert.equal(text(w, ".pr-head-meta .mono"), "acme/web#3");
  // An invalid key is ignored.
  w.window.prxApp.openPr("not a key");
  assert.equal(text(w, ".pr-head-meta .mono"), "acme/web#3");
  w.window.location.hash = "#page=pr&pr=" + encodeURIComponent("acme/web#6");
  w.fireWindow("hashchange");
  await w.settle();
  assert.equal(text(w, ".pr-head-meta .mono"), "acme/web#6");
  // Back to the list, then "Show details" in a row's ⋯ menu and its detail.
  w.fire(w.page.querySelector(".back-link"), "click");
  await w.settle();
  assert.equal(w.window.prxApp.state.page, "prs");
  const row = w.page.querySelectorAll("tr.pr-row").find((r) => r.querySelector(".pr-meta .mono").textContent === "acme/web#5");
  w.fire(row.querySelector(".menu-anchor").querySelectorAll("[role=menuitem]").find((i) => i.textContent === "Show details"), "click");
  await w.settle();
  assert.equal(text(w, ".pr-head-meta .mono"), "acme/web#5");
  w.fire(w.page.querySelector(".back-link"), "click");
  await w.settle();
  const four = w.page.querySelectorAll("tr.pr-row").find((r) => r.querySelector(".pr-meta .mono").textContent === "acme/web#4");
  w.fire(four.querySelector("button.row-open"), "click");
  w.fire(button(w.page.querySelector(".pr-detail"), "Show details"), "click");
  await w.settle();
  assert.equal(text(w, ".pr-head-meta .mono"), "acme/web#4");
});
