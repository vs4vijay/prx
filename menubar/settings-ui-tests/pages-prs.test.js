// The Pull requests page end to end (pages-prs.js with the real shell,
// bridge and form helpers) against the fake DOM: one row per lifecycle
// state and skip reason, sorting and filtering, the detail row, every review
// action and the exact request it sends, the loading, empty, offline and
// failed states, and hostile PR content shown as text.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const P = require("../settings-ui/pages-prs.js");
const O = require("../settings-ui/overview.js");
const X = require("./page-fixtures.js");
const { boot, plain, withText, bridgeError } = require("./fake-dom.js");

const rows = (w) => w.page.querySelectorAll("tr.pr-row");
const keys = (w) => rows(w).map((r) => r.querySelector(".pr-meta .mono").textContent);
const row = (w, key) => rows(w).find((r) => r.querySelector(".pr-meta .mono").textContent === key);
const count = (w) => w.page.querySelector(".toolbar .count").textContent;
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
const tick = (w) => w.intervals.find((i) => i.ms === 10000).run();
// Open a row's ⋯ menu and return its items by label.
function menu(w, key) {
  const anchor = row(w, key).querySelector(".menu-anchor");
  return { anchor: anchor, items: anchor.querySelectorAll("[role=menuitem]"), item: (label) => anchor.querySelectorAll("[role=menuitem]").find((i) => i.textContent === label) };
}
// Pick an option of a toolbar dropdown by its label.
function pick(w, id, label) {
  const trigger = w.byId(id);
  w.fire(trigger, "click");
  w.fire(w.byId(id + "-list").querySelectorAll("[role=option]").find((o) => o.querySelector(".select-label").textContent === label), "click");
}

// ---- view model edge cases ----------------------------------------------------

test("model: every lifecycle state and skip reason has its own label and state", () => {
  const view = Object.fromEntries(X.prs().map((pr) => [pr.key, P.rowModel(pr, X.NOW)]));
  assert.deepEqual(Object.values(view).map((r) => [r.key, r.stateId, r.state, r.group]), [
    ["acme/api#1", "needs_review", "Needs review", "needs_review"],
    ["acme/api#2", "in_review", "In review", "in_review"],
    ["acme/web#3", "ready", "Ready for approval", "ready"],
    ["acme/web#4", "approved", "Approved", "reviewed"],
    ["acme/web#5", "commented", "Commented", "reviewed"],
    ["acme/web#6", "changes_requested", "Changes requested", "reviewed"],
    ["solo/tool#7", "skipped", "Skipped (draft)", "skipped"],
    ["solo/tool#8", "skipped", "Skipped (skip rule)", "skipped"],
    ["solo/tool#9", "skipped", "Skipped (snoozed)", "skipped"],
    ["solo/tool#10", "skipped", "Skipped (ignored)", "skipped"],
    ["acme/api#11", "merged", "Merged", "terminal"],
    ["acme/api#12", "closed", "Closed", "terminal"],
  ]);
  // A reason or state this build does not know is shown as sent, not hidden.
  assert.equal(P.rowModel(X.pr("a/b#1", "skipped", { skip_reason: "quota" }), X.NOW).state, "Skipped (quota)");
  assert.equal(P.rowModel(X.pr("a/b#1", "skipped"), X.NOW).state, "Skipped");
  const unknown = P.rowModel(X.pr("a/b#1", "quarantined"), X.NOW);
  assert.deepEqual([unknown.stateId, unknown.group, unknown.terminal, unknown.reviewable], ["needs_review", "terminal", false, true]);
  assert.deepEqual([view["acme/api#11"].terminal, view["acme/api#11"].reviewable, view["acme/api#12"].reviewable], [true, false, false]);
});

test("model: age, last review, findings and the fields a row may lack", () => {
  const r = P.rowModel(X.pr("a/b#1", "reviewed_changes_requested", { last_verdict: "changes_requested", blocking_count: 1, nit_count: 0,
    created_at: X.ago(3 * X.DAY), last_review_at: X.ago(90 * X.MIN) }), X.NOW);
  assert.deepEqual([r.age, r.lastReview, r.verdict, r.findings], ["3d", "1h ago", "Changes requested", "1 blocking, 0 nits"]);
  const clean = P.rowModel(X.pr("a/b#2", "reviewed_approved", { last_verdict: "approved" }), X.NOW);
  assert.equal(clean.findings, "No findings");
  const nits = P.rowModel(X.pr("a/b#2", "reviewed_approved", { last_verdict: "approved", nit_count: 1 }), X.NOW);
  assert.equal(nits.findings, "1 nit");
  // An older daemon's entry: no url, engine, dates or author.
  const bare = P.rowModel({ key: "a/b#3", state: "pending", title: "t" }, X.NOW);
  assert.deepEqual([bare.url, bare.engine, bare.age, bare.lastReview, bare.verdict, bare.findings, bare.summary, bare.author],
    ["", "", "", "never", "", "", "", undefined]);
  // A date that does not parse shows nothing rather than "NaN".
  assert.equal(P.rowModel(X.pr("a/b#4", "pending", { created_at: "not a date" }), X.NOW).age, "");
});

test("model: sorting by every column keeps missing values last and ties in key order", () => {
  const prs = X.prs();
  const sorted = (column, dir) => P.sortRows(prs, { column: column, dir: dir }).map((p) => p.key);
  // By state: the menu's group order, each group in key order.
  assert.deepEqual(sorted("state", "asc"), ["acme/api#1", "acme/api#2", "acme/web#3", "acme/web#4", "acme/web#5", "acme/web#6",
    "solo/tool#7", "solo/tool#8", "solo/tool#9", "solo/tool#10", "acme/api#11", "acme/api#12"]);
  assert.deepEqual(sorted("state", "desc").slice(0, 2), ["acme/api#11", "acme/api#12"]);
  // Never-reviewed PRs are last whichever way the column runs.
  const never = prs.filter((p) => !p.last_review_at).map((p) => p.key).sort(O.compareKeys);
  assert.deepEqual(sorted("last_review", "desc").slice(-never.length), never);
  assert.deepEqual(sorted("last_review", "asc").slice(-never.length), never);
  assert.equal(sorted("last_review", "desc")[0], "acme/web#6");
  assert.equal(sorted("last_review", "asc")[0], "acme/api#11");
  assert.deepEqual(sorted("key", "desc").slice(0, 3), ["solo/tool#10", "solo/tool#9", "solo/tool#8"]);
  assert.equal(sorted("created", "asc")[0], "acme/api#12");
  // An unknown column changes nothing but the tie-break.
  assert.deepEqual(sorted("nope", "asc"), sorted("key", "asc"));
  // The input is not reordered.
  assert.deepEqual(prs.map((p) => p.key), X.prs().map((p) => p.key));
  assert.deepEqual(P.sortRows([], { column: "key", dir: "asc" }), []);
});

test("model: filters combine, trim, ignore case, and tolerate missing fields", () => {
  const prs = X.prs();
  const found = (f) => P.filterRows(prs, Object.assign({ repo: "", state: "", text: "" }, f)).map((p) => p.key);
  assert.deepEqual(found({ state: "skipped" }), ["solo/tool#7", "solo/tool#8", "solo/tool#9", "solo/tool#10"]);
  assert.deepEqual(found({ state: "needs_review" }), ["acme/api#1"]);
  assert.deepEqual(found({ state: "reviewed" }), ["acme/web#4", "acme/web#5", "acme/web#6"]);
  assert.deepEqual(found({ state: "terminal" }), ["acme/api#11", "acme/api#12"]);
  assert.deepEqual(found({ text: "  MALLORY " }), ["acme/api#1"]);
  assert.deepEqual(found({ text: "🚀" }), ["acme/api#2"]);
  assert.deepEqual(found({ text: "<script>" }), ["acme/api#1"]);
  assert.deepEqual(found({ text: "tool#1" }), ["solo/tool#10"]);
  assert.deepEqual(found({ repo: "acme/api", state: "attention" }), ["acme/api#2"]);
  assert.deepEqual(found({ repo: "acme", text: "" }), [], "a repo filter is the whole owner/name");
  assert.deepEqual(found({ text: "no such thing" }), []);
  assert.deepEqual(P.filterRows([{ key: "a/b#1", state: "pending" }], { repo: "", state: "", text: "b#1" }).length, 1);
  assert.deepEqual(P.STATE_FILTERS.map((f) => f[0]), ["", "attention", "needs_review", "in_review", "ready", "reviewed", "skipped", "terminal"]);
});

test("model: the first sort tolerates an unknown menu.sort", () => {
  assert.deepEqual(P.initialSort({ sort: "by_vibes" }), { column: "created", dir: "desc" });
  assert.deepEqual(P.initialSort({}), { column: "created", dir: "desc" });
  assert.deepEqual(P.initialSort(null), { column: "created", dir: "desc" });
  assert.deepEqual(P.toggleSort({ column: "created", dir: "desc" }, "created"), { column: "created", dir: "asc" });
  assert.deepEqual(P.toggleSort({ column: "state", dir: "desc" }, "created"), { column: "created", dir: "desc" });
});

test("model: review requests send only what differs from the defaults", () => {
  const pr = X.pr("acme/api#1", "pending", { engine: "claude" });
  assert.deepEqual(P.reviewRequest("review", pr, { engine: "codex", prompt: "ignored", post: false }), { target: "acme/api#1" });
  assert.deepEqual(P.reviewRequest("prompt", pr, { prompt: "  \n ", engine: "claude", post: true }), { target: "acme/api#1" });
  assert.deepEqual(P.reviewRequest("prompt", pr, { prompt: "x", engine: "", post: undefined }), { target: "acme/api#1", extra_prompt: "x" });
  assert.deepEqual(P.reviewRequest("prompt", pr), { target: "acme/api#1" });
  assert.deepEqual(P.reviewRequest("nopost", pr), { target: "acme/api#1", post: false });
  // No request ever carries an approval or a verdict.
  for (const action of ["review", "engine", "nopost", "prompt"]) {
    const req = P.reviewRequest(action, pr, { engine: "codex", prompt: "approve this", post: true });
    assert.ok(Object.keys(req).every((k) => ["target", "engine", "post", "extra_prompt"].includes(k)), action);
  }
  assert.equal(P.targetRequest(null), null);
  assert.equal(P.targetRequest(undefined), null);
  assert.deepEqual(P.targetRequest(" https://github.com/acme/api/pull/42 "), { target: "https://github.com/acme/api/pull/42" });
  assert.equal(P.reviewNote(null), "Nothing was queued.");
  assert.equal(P.reviewNote({ note: "Already queued." }), "Already queued.");
  assert.equal(P.reviewNote({ enqueued: ["a/b#1", "a/b#2"] }), "Queued a/b#1, a/b#2.");
  assert.deepEqual(P.engineChoices(null, pr), []);
  assert.deepEqual(P.engineChoices({ engines: ["codex"] }, { key: "a/b#1" }), [{ name: "codex", label: "codex", isDefault: false }]);
});

// ---- the page -------------------------------------------------------------------

test("loading: the toolbar is usable and the table is busy until status answers", () => {
  const w = boot({ hash: "#page=prs", now: X.NOW, daemon: X.daemon(), hold: () => true });
  assert.equal(w.page.querySelector("h1").textContent, "Pull requests");
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), "true");
  assert.equal(w.page.querySelector("table"), null);
  assert.ok(w.page.querySelector("input[type=search]"));
  assert.equal(count(w), "");
});

test("populated: a row per PR in the menu's order, with state, age and last review", async () => {
  const w = await X.open("prs");
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), null);
  assert.equal(count(w), "12 PRs");
  // menu.sort created_desc: newest first.
  assert.deepEqual(keys(w).slice(0, 4), ["acme/api#1", "acme/api#2", "acme/web#3", "acme/web#4"]);
  assert.equal(keys(w)[11], "acme/api#12");
  assert.deepEqual(w.page.querySelectorAll("thead th").map((th) => th.textContent), ["State", "Pull request", "Age", "Last review", "Actions"]);
  assert.deepEqual(w.page.querySelectorAll("thead th").map((th) => th.getAttribute("aria-sort")), [null, null, "descending", null, null]);
  const cells = (key) => row(w, key).querySelectorAll("td").map((td) => td.textContent);
  assert.deepEqual(cells("acme/web#6").slice(0, 4), ["Changes requested2 blocking, 1 nit", X.LONG + "acme/web#6· noor", "2d", "30m ago"]);
  assert.deepEqual(cells("acme/web#3").slice(0, 4), ["Ready for approval2 nits", "PR acme/web#3acme/web#3· noor", "1d", "2h ago"]);
  assert.deepEqual(cells("acme/api#1").slice(2, 4), ["3h", "never"]);
  assert.deepEqual(rows(w).map((r) => r.querySelector(".state-text").textContent), ["Needs review", "In review", "Ready for approval",
    "Approved", "Commented", "Changes requested", "Skipped (draft)", "Skipped (skip rule)", "Skipped (snoozed)", "Skipped (ignored)", "Merged", "Closed"]);
  assert.deepEqual(rows(w).map((r) => r.querySelector(".state").className.replace("state tone-", "")),
    ["neutral", "progress", "ok", "ok", "neutral", "error", "neutral", "neutral", "neutral", "neutral", "merged", "neutral"]);
  // Merged and closed rows are marked and cannot be reviewed.
  assert.deepEqual(rows(w).filter((r) => r.classList.contains("terminal")).map((r) => r.querySelector(".mono").textContent), ["acme/api#11", "acme/api#12"]);
});

test("hostile, long and non-ASCII titles and authors are shown exactly, as text", async () => {
  const w = await X.open("prs");
  const title = (key) => row(w, key).querySelector("button.row-open");
  assert.equal(title("acme/api#1").textContent, X.EVIL);
  assert.equal(title("acme/api#1").getAttribute("title"), X.EVIL);
  assert.equal(row(w, "acme/api#1").querySelector(".pr-meta").textContent, "acme/api#1· <b>mallory</b>");
  assert.equal(title("acme/api#2").textContent, X.UNICODE);
  assert.equal(title("acme/web#6").textContent, X.LONG);
  w.fire(title("acme/api#1"), "click");
  w.fire(row(w, "acme/web#3").querySelector("button.row-open"), "click");
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
  // The only links are the PRs' own GitHub pages.
  assert.deepEqual(w.page.querySelectorAll("a").map((a) => [a.getAttribute("href"), a.getAttribute("rel"), a.getAttribute("target")]).slice(0, 2),
    [["https://github.com/acme/api/pull/1", "noopener", "_blank"], ["https://github.com/acme/api/pull/2", "noopener", "_blank"]]);
  // A PR without a URL (an older daemon) has no link and no "Open in GitHub".
  assert.equal(row(w, "acme/api#12").querySelector("a"), null);
  assert.equal(menu(w, "acme/api#12").item("Open in GitHub"), undefined);
  assert.ok(menu(w, "acme/api#11").item("Open in GitHub"));
});

test("the detail row: one open at a time, with the brief card and the PR's facts", async () => {
  const w = await X.open("prs");
  const open = (key) => w.fire(row(w, key).querySelector("button.row-open"), "click");
  open("acme/web#6");
  let detail = w.page.querySelector("tr.detail-row.open");
  assert.equal(detail.querySelector(".pr-detail").id, "pr-detail-acme_web_6");
  assert.equal(row(w, "acme/web#6").querySelector("button.row-open").getAttribute("aria-controls"), "pr-detail-acme_web_6");
  assert.equal(row(w, "acme/web#6").querySelector("button.row-open").getAttribute("aria-expanded"), "true");
  assert.equal(detail.querySelector("td").getAttribute("colspan"), "5");
  assert.equal(detail.querySelector(".pr-facts").textContent,
    "Last verdict: Changes requested (2 blocking, 1 nit) · Last review: 30m ago · Engine: codex · Author: noor · 2 blocking");
  const card = detail.querySelector("section.brief-card");
  assert.equal(card.querySelector(".brief-text").textContent, "Rewrites the session store.");
  assert.equal(card.querySelector(".badge.warn").textContent, "Stale");
  assert.equal(card.querySelector(".brief-action").textContent, "Regenerate");
  // Opening another closes the first; clicking it again closes it.
  open("acme/api#1");
  assert.equal(w.page.querySelectorAll("tr.detail-row.open").length, 1);
  detail = w.page.querySelector("tr.detail-row.open");
  assert.equal(detail.querySelector(".pr-facts").textContent, "Not reviewed yet · Last review: never · Engine: claude · Author: <b>mallory</b>");
  assert.equal(detail.querySelector(".brief-empty").textContent, "No brief yet. A brief says what this PR does and why, in a few plain sentences.");
  open("acme/api#1");
  assert.equal(w.page.querySelector("tr.detail-row.open"), null);
  // Briefs off for the repo: said plainly, and no action offered.
  open("solo/tool#10");
  detail = w.page.querySelector("tr.detail-row.open");
  assert.equal(detail.querySelector(".brief-empty").textContent, "Briefs are off for this repo.");
  assert.equal(detail.querySelector(".brief-action"), null);
});

test("sorting: a header click sorts, a second click reverses, and aria-sort follows", async () => {
  const w = await X.open("prs");
  const header = (label) => w.page.querySelectorAll("thead th button").find((b) => b.textContent === label);
  w.fire(header("Pull request"), "click");
  assert.deepEqual(keys(w).slice(0, 3), ["acme/api#1", "acme/api#2", "acme/api#11"]);
  assert.deepEqual(w.page.querySelectorAll("thead th").map((th) => th.getAttribute("aria-sort")), [null, "ascending", null, null, null]);
  assert.equal(header("Pull request").className, "sort active asc");
  w.fire(header("Pull request"), "click");
  assert.deepEqual(keys(w).slice(0, 2), ["solo/tool#10", "solo/tool#9"]);
  assert.equal(header("Pull request").className, "sort active desc");
  w.fire(header("Last review"), "click");
  assert.equal(keys(w)[0], "acme/web#6");
  assert.deepEqual(w.page.querySelectorAll("thead th").map((th) => th.getAttribute("aria-sort")), [null, null, null, "descending", null]);
  // The sort survives a poll and a trip to another page.
  tick(w);
  await w.settle();
  assert.equal(keys(w)[0], "acme/web#6");
  w.window.prxApp.go("history");
  w.window.prxApp.go("prs");
  await w.settle();
  assert.equal(keys(w)[0], "acme/web#6");
});

test("filtering: text, state and repo narrow the table and say how many are shown", async () => {
  const w = await X.open("prs");
  const search = w.page.querySelector("input[type=search]");
  search.value = "tool";
  w.fire(search, "input");
  assert.equal(count(w), "4 of 12");
  assert.deepEqual(keys(w), ["solo/tool#7", "solo/tool#8", "solo/tool#9", "solo/tool#10"]);
  pick(w, "prs-filter-by-state", "Needs attention");
  assert.equal(count(w), "0 of 12");
  assert.equal(w.page.querySelector("td.list-empty").textContent, "No PR matches these filters.");
  assert.equal(w.page.querySelector("td.list-empty").getAttribute("colspan"), "5");
  search.value = "";
  w.fire(search, "input");
  assert.deepEqual(keys(w), ["acme/api#2", "acme/web#6"]);
  pick(w, "prs-filter-by-repo", "acme/web");
  assert.deepEqual(keys(w), ["acme/web#6"]);
  assert.deepEqual(w.byId("prs-filter-by-repo-list").querySelectorAll(".select-label").map((n) => n.textContent), ["All repos", "acme/api", "acme/web", "solo/tool"]);
  assert.deepEqual(w.byId("prs-filter-by-state-list").querySelectorAll(".select-label").map((n) => n.textContent),
    ["All states", "Needs attention", "Needs review", "In review", "Ready for approval", "Reviewed", "Skipped", "Merged / closed"]);
  pick(w, "prs-filter-by-state", "All states");
  pick(w, "prs-filter-by-repo", "All repos");
  assert.equal(count(w), "12 PRs");
});

test("a repo filter is dropped when its repo is no longer tracked", async () => {
  let prs = X.prs();
  const w = await X.open("prs", { "status.get": () => X.status((s) => { s.prs = prs; }) });
  pick(w, "prs-filter-by-repo", "solo/tool");
  assert.equal(count(w), "4 of 12");
  prs = prs.filter((p) => !p.key.startsWith("solo/"));
  w.document.activeElement.blur();
  tick(w);
  await w.settle();
  assert.equal(count(w), "8 PRs");
  assert.equal(w.byId("prs-filter-by-repo").textContent, "All repos");
});

test("Review sends the PR as the target and reports what was queued", async () => {
  const w = await X.open("prs");
  const review = row(w, "acme/api#1").querySelector(".row-actions > button.btn");
  assert.equal(review.getAttribute("aria-label"), "Review acme/api#1");
  w.fire(review, "click");
  // Before the answer: a pending note under the row.
  assert.equal(w.page.querySelector("tr.detail-row .inline-note").textContent, "Requesting…");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "acme/api#1" }]);
  const note = w.page.querySelector("tr.detail-row .inline-note");
  assert.equal(note.className, "inline-note ok");
  assert.equal(note.querySelector("span").textContent, "Queued acme/api#1.");
  // The table was refreshed after the request.
  assert.equal(w.calls("status.get").length, 2);
  // Dismiss removes the note.
  w.fire(note.querySelector("button[aria-label=Dismiss]"), "click");
  assert.equal(w.page.querySelector("tr.detail-row .inline-note"), null);
});

test("the ⋯ menu: Review with an engine, without posting, and open in GitHub", async () => {
  const w = await X.open("prs");
  const m = menu(w, "acme/web#6");
  assert.equal(m.anchor.querySelector("button[aria-haspopup=menu]").getAttribute("aria-label"), "More actions for acme/web#6");
  assert.deepEqual(m.items.map((i) => i.textContent), ["cheap", "claude", "codex (default)", "Review without posting", "Review with prompt…", "Show details", "Open Review Chat…", "Open in GitHub"]);
  assert.equal(m.anchor.querySelector(".menu-label").textContent, "Review with");
  w.fire(m.item("cheap"), "click");
  await w.settle();
  w.fire(menu(w, "acme/web#6").item("Review without posting"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "acme/web#6", engine: "cheap" }, { target: "acme/web#6", post: false }]);
  w.fire(menu(w, "acme/web#6").item("Open in GitHub"), "click");
  assert.deepEqual(plain(w.window.opened), [["https://github.com/acme/web/pull/6", "_blank", "noopener"]]);
  // An older daemon lists no engines: no "Review with" group.
  const old = await X.open("prs", { "status.get": () => X.status((s) => { delete s.engines; }) });
  assert.deepEqual(menu(old, "acme/web#6").items.map((i) => i.textContent), ["Review without posting", "Review with prompt…", "Show details", "Open Review Chat…", "Open in GitHub"]);
  assert.equal(menu(old, "acme/web#6").anchor.querySelector(".menu-label"), null);
});

test("Open Review Chat asks the app for a terminal and says how it went", async () => {
  const w = await X.open("prs");
  w.fire(menu(w, "acme/web#6").item("Open Review Chat…"), "click");
  await w.settle();
  assert.deepEqual(w.calls("app.review_chat.open"), [{ key: "acme/web#6" }]);
  assert.equal(w.calls("review.request").length, 0);
  assert.match(w.page.textContent, /Review Chat opened in Terminal\./);
  // A refusal (e.g. no bundled prx) is shown, not swallowed.
  const failing = await X.open("prs", { "app.review_chat.open": () => { throw bridgeError("rejected", "cannot open Review Chat: the bundled prx command-line tool was not found"); } });
  failing.fire(menu(failing, "acme/web#6").item("Open Review Chat…"), "click");
  await failing.settle();
  assert.match(failing.page.textContent, /bundled prx command-line tool was not found/);
});

test("merged and closed PRs offer no review action at all", async () => {
  const w = await X.open("prs");
  for (const key of ["acme/api#11", "acme/api#12"]) {
    const review = row(w, key).querySelector(".row-actions > button.btn");
    assert.equal(review.disabled, true, key);
    review.click();
    const items = menu(w, key).items.filter((i) => !["Open in GitHub", "Show details", "Open Review Chat…"].includes(i.textContent));
    assert.equal(menu(w, key).item("Open Review Chat…").disabled, false, key);
    assert.ok(items.length >= 2 && items.every((i) => i.disabled), key);
  }
  await w.settle();
  assert.deepEqual(w.calls("review.request"), []);
  // Skipped and already-reviewed PRs can still be reviewed on request.
  for (const key of ["solo/tool#7", "acme/web#4"]) assert.equal(row(w, key).querySelector(".row-actions > button.btn").disabled, false, key);
});

test("Review with prompt: the form sends the instructions, the engine and posting choice", async () => {
  const w = await X.open("prs");
  w.fire(menu(w, "acme/api#1").item("Review with prompt…"), "click");
  let form = w.page.querySelector(".prompt-form");
  const area = form.querySelector("textarea");
  assert.equal(area.getAttribute("aria-label"), "Extra instructions for this review");
  assert.equal(form.querySelector("#prompt-post").checked, true);
  assert.equal(form.querySelector("[role=combobox]").textContent, "claude (default)");
  area.value = "  Check the rollback path.  ";
  w.fire(area, "input");
  // Typing survives a poll: the table is not rebuilt under the cursor.
  area.focus();
  tick(w);
  await w.settle();
  assert.equal(w.page.querySelector(".prompt-form textarea"), area);
  w.fire(form.querySelector("[role=combobox]"), "click");
  w.fire(form.querySelectorAll("[role=option]").find((o) => o.querySelector(".select-label").textContent === "codex"), "click");
  const post = form.querySelector("#prompt-post");
  post.checked = false;
  w.fire(post, "change");
  w.fire(button(form, "Review"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "acme/api#1", extra_prompt: "Check the rollback path.", engine: "codex", post: false }]);
  assert.equal(w.page.querySelector(".prompt-form"), null, "the form closes once sent");
  // Cancel sends nothing.
  w.fire(menu(w, "acme/api#2").item("Review with prompt…"), "click");
  form = w.page.querySelector(".prompt-form");
  form.querySelector("textarea").value = "never sent";
  w.fire(form.querySelector("textarea"), "input");
  w.fire(button(form, "Cancel"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".prompt-form"), null);
  assert.equal(w.calls("review.request").length, 1);
  // An empty prompt with the defaults is a plain review.
  w.fire(menu(w, "acme/api#2").item("Review with prompt…"), "click");
  w.fire(button(w.page.querySelector(".prompt-form"), "Review"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.request")[1], { target: "acme/api#2" });
});

test("a refused or offline review request says why under its row", async () => {
  let mode = "refused";
  const w = await X.open("prs", { "review.request": () => { throw mode === "offline" ? bridgeError("offline", "connection refused") : bridgeError("daemon", "review mode is off for acme/api " + X.EVIL); } });
  w.fire(row(w, "acme/api#1").querySelector(".row-actions > button.btn"), "click");
  await w.settle();
  let note = w.page.querySelector("tr.detail-row .inline-note");
  assert.equal(note.className, "inline-note error");
  assert.equal(note.querySelector("span").textContent, "review mode is off for acme/api " + X.EVIL);
  assert.deepEqual(X.markupIn(w.page), []);
  mode = "offline";
  w.fire(row(w, "acme/api#2").querySelector(".row-actions > button.btn"), "click");
  await w.settle();
  note = w.page.querySelectorAll("tr.detail-row .inline-note")[1];
  assert.equal(note.querySelector("span").textContent, "The PRX daemon is not running.");
  // A failed request does not trigger a refresh.
  assert.equal(w.calls("status.get").length, 1);
});

test("Review PR…: any reference goes to the daemon as typed, on Enter or the button", async () => {
  let refuse = false;
  const w = await X.open("prs", { "review.request": (p) => { if (refuse) throw bridgeError("daemon", "no such PR: " + p.target); return { enqueued: ["acme/api#42"], note: "Queued acme/api#42 (not watched: reviewed once)" }; } });
  const box = w.page.querySelector(".review-pr");
  const target = box.querySelector("input");
  const note = box.querySelector(".inline-note");
  // Nothing typed: nothing sent.
  w.fire(button(box, "Review"), "click");
  w.fire(target, "keydown", { key: "Enter" });
  await w.settle();
  assert.deepEqual(w.calls("review.request"), []);
  target.value = "  https://github.com/acme/api/pull/42  ";
  const event = w.fire(target, "keydown", { key: "Enter" });
  assert.equal(event.defaultPrevented, true);
  assert.equal(note.textContent, "Requesting…");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "https://github.com/acme/api/pull/42" }]);
  assert.equal(note.textContent, "Queued acme/api#42 (not watched: reviewed once)");
  assert.equal(note.className, "inline-note ok");
  assert.equal(target.value, "", "the box clears once the request is accepted");
  // Other keys do not submit.
  target.value = "42";
  w.fire(target, "keydown", { key: "a" });
  await w.settle();
  assert.equal(w.calls("review.request").length, 1);
  // A refusal keeps the text so it can be corrected.
  refuse = true;
  w.fire(button(box, "Review"), "click");
  await w.settle();
  assert.equal(note.textContent, "no such PR: 42");
  assert.equal(note.className, "inline-note error");
  assert.equal(target.value, "42");
});

test("empty: watching repos with no PRs, and watching nothing, say different things", async () => {
  const none = (s) => { s.prs = []; };
  const watching = await X.open("prs", { "status.get": () => X.status(none) });
  assert.equal(watching.page.querySelector(".empty-title").textContent, "No pull requests yet");
  assert.equal(watching.page.querySelector(".empty-actions"), null);
  assert.equal(count(watching), "0 PRs");
  assert.equal(watching.page.querySelector("table"), null);
  // Opened straight on this page, the first render already knows nothing is
  // watched: the page asks for the config with the status, without waiting
  // for a poll.
  const nothing = { "status.get": () => X.status(none), "config.get": () => X.config((c) => { c.raw.repos = []; c.raw.orgs = []; }) };
  const fresh = await X.open("prs", nothing);
  assert.equal(fresh.calls("status.get").length, 1, "the first answer, before any poll");
  assert.equal(fresh.page.querySelector(".empty-title").textContent, "No repos watched yet");
  assert.equal(count(fresh), "0 PRs");
  // A later poll says the same.
  tick(fresh);
  await fresh.settle();
  assert.equal(fresh.page.querySelector(".empty-title").textContent, "No repos watched yet");
  fresh.fire(button(fresh.page, "Add a repo"), "click");
  assert.equal(fresh.window.prxApp.state.page, "repos");
  // Once the app has the config, the page uses it and asks for no other.
  const back = await X.open("prs", nothing);
  const asked = back.calls("config.get").length;
  back.window.prxApp.go("history");
  back.window.prxApp.go("prs");
  await back.settle();
  assert.equal(back.calls("config.get").length, asked);
  assert.equal(back.page.querySelector(".empty-title").textContent, "No repos watched yet");
  // A config that cannot be read leaves the general wording, not an error.
  const unread = await X.open("prs", { "status.get": () => X.status(none), "config.get": X.failing("config.yml is not valid YAML") });
  assert.equal(unread.page.querySelector(".table-box .empty-title").textContent, "No pull requests yet");
  // One PR is singular.
  const single = await X.open("prs", { "status.get": () => X.status((s) => { s.prs = s.prs.slice(0, 1); }) });
  assert.equal(count(single), "1 PR");
});

test("daemon offline and daemon errors replace the table, and Try again recovers", async () => {
  let mode = "offline";
  const w = await X.open("prs", {
    "config.get": () => { if (mode === "offline") X.offline(); return X.config(); },
    "status.get": () => { if (mode === "offline") X.offline(); if (mode === "error") throw bridgeError("daemon", "state file is corrupt"); return X.status(); },
  });
  assert.equal(w.page.querySelector(".table-box").getAttribute("aria-busy"), null);
  assert.equal(w.page.querySelector(".table-box .empty-title").textContent, "Daemon offline");
  assert.equal(w.byId("daemon").textContent, "Daemon not running");
  assert.equal(w.page.querySelector("table"), null);
  mode = "error";
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".table-box .empty-title").textContent, "Couldn't load this page");
  assert.equal(w.page.querySelector(".table-box .empty-text").textContent, "state file is corrupt");
  mode = "up";
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(rows(w).length, 12);
  assert.equal(w.byId("daemon").textContent, "Daemon running");
});

test("a poll does not rebuild the table while a row menu is open", async () => {
  let prs = X.prs();
  const w = await X.open("prs", { "status.get": () => X.status((s) => { s.prs = prs; }) });
  const trigger = menu(w, "acme/api#1").anchor.querySelector("button[aria-haspopup=menu]");
  trigger.focus();
  w.fire(trigger, "click");
  assert.equal(trigger.getAttribute("aria-expanded"), "true");
  prs = prs.slice(0, 2);
  tick(w);
  await w.settle();
  assert.equal(rows(w).length, 12, "the open menu's table is left alone");
  assert.equal(trigger.isConnected, true);
  // Once focus leaves the table, the next poll catches up.
  w.document.activeElement.blur();
  tick(w);
  await w.settle();
  assert.equal(rows(w).length, 2);
});

// ---- skip details and the review confirmation ---------------------------------

test("model: the confirm box lists the warnings, the size, and the cost", () => {
  const risky = X.riskyPreflight("solo/tool#8");
  assert.equal(P.needsConfirmation(risky), true);
  assert.equal(P.needsConfirmation(null), false, "an older daemon has no pre-flight");
  assert.equal(P.needsConfirmation({ key: "a/b#1", warnings: [] }), false);
  assert.equal(P.needsConfirmation(Object.assign({}, risky, { blocked: "review mode is off for a/b" })), false);
  assert.deepEqual(P.confirmLines(risky), ["Skipped — author dependabot[bot] excluded", "Large PR — 2,140 lines changed (limit 1,500)"]);
  assert.deepEqual(P.confirmLines({ warnings: [{ kind: "skipped", message: "Skipped — draft" }], size: { files_changed: 1, lines: 12000 } }),
    ["Skipped — draft", "Size — 12,000 lines in 1 file"]);
  assert.equal(P.confirmCost(risky), "Reviewing it runs the engine and can use many tokens. The PR stays skipped for automatic reviews.");
  assert.equal(P.confirmCost({ warnings: [{ kind: "too_large", message: "m" }] }), "Reviewing it runs the engine and can use many tokens.");
});

test("a skipped row says why under its state", async () => {
  const w = await X.open("prs", { "status.get": () => X.status((s) => {
    s.prs.push(X.pr("acme/api#13", "pending", { skip_detail: "2,140 lines changed (limit 1,500)", summary: "Skipped — 2,140 lines changed (limit 1,500)" }));
  }) });
  const note = (key) => { const n = row(w, key).querySelector(".state-cell .skip-note"); return n && n.textContent; };
  assert.equal(note("solo/tool#7"), "draft");
  assert.equal(note("solo/tool#8"), "author dependabot[bot] excluded");
  assert.equal(note("acme/api#13"), "Review skipped — 2,140 lines changed (limit 1,500)");
  assert.equal(note("acme/api#1"), null, "nothing holds it back");
  assert.equal(note("solo/tool#9"), null, "an older daemon sends no detail");
});

test("Review on a risky PR asks first; Cancel sends nothing, Review anyway sends it confirmed", async () => {
  const w = await X.open("prs", { "review.preflight": (p) => X.riskyPreflight(p.target) });
  w.fire(row(w, "solo/tool#8").querySelector(".row-actions > button.btn"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.preflight"), [{ target: "solo/tool#8" }]);
  assert.deepEqual(w.calls("review.request"), [], "nothing is sent before the answer");
  let box = w.page.querySelector(".confirm-box");
  assert.equal(box.getAttribute("role"), "alertdialog");
  assert.equal(box.getAttribute("aria-label"), "Review solo/tool#8 anyway?");
  assert.equal(box.querySelector(".confirm-title").textContent, "Review solo/tool#8 anyway?");
  assert.deepEqual(box.querySelectorAll("li").map((li) => li.textContent),
    ["Skipped — author dependabot[bot] excluded", "Large PR — 2,140 lines changed (limit 1,500)"]);
  assert.match(box.querySelector(".confirm-cost").textContent, /stays skipped/);

  w.fire(button(box, "Cancel"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".confirm-box"), null);
  assert.deepEqual(w.calls("review.request"), []);
  assert.equal(w.page.querySelector("tr.detail-row .inline-note span").textContent, "Not reviewed.");

  // The ⋯ menu's actions ask too, and keep their options.
  w.fire(menu(w, "solo/tool#8").item("Review without posting"), "click");
  await w.settle();
  box = w.page.querySelector(".confirm-box");
  w.fire(button(box, "Review anyway"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "solo/tool#8", post: false, confirm: true }]);
  assert.equal(w.page.querySelector(".confirm-box"), null);
  assert.equal(w.page.querySelector("tr.detail-row .inline-note span").textContent, "Queued solo/tool#8.");
});

test("no confirmation without warnings, from an older daemon, or when the review is blocked", async () => {
  for (const preflight of [null, { key: "acme/api#1", warnings: [] }, Object.assign(X.riskyPreflight("acme/api#1"), { blocked: "review mode is off for acme/api" })]) {
    const w = await X.open("prs", { "review.preflight": () => preflight });
    w.fire(row(w, "acme/api#1").querySelector(".row-actions > button.btn"), "click");
    await w.settle();
    assert.equal(w.page.querySelector(".confirm-box"), null, JSON.stringify(preflight));
    assert.deepEqual(w.calls("review.request"), [{ target: "acme/api#1" }], JSON.stringify(preflight));
  }
  // A shell without the method (bad_request) sends as before; any other
  // pre-flight failure is shown and sends nothing.
  const old = await X.open("prs", { "review.preflight": () => { throw bridgeError("bad_request", "unknown bridge method 'review.preflight'"); } });
  w0(old);
  await old.settle();
  assert.deepEqual(old.calls("review.request"), [{ target: "acme/api#1" }]);
  const down = await X.open("prs", { "review.preflight": X.failing("no watched repo has an open PR #1") });
  w0(down);
  await down.settle();
  assert.deepEqual(down.calls("review.request"), []);
  const note = down.page.querySelector("tr.detail-row .inline-note");
  assert.equal(note.className, "inline-note error");
  assert.equal(note.querySelector("span").textContent, "no watched repo has an open PR #1");

  function w0(w) { w.fire(row(w, "acme/api#1").querySelector(".row-actions > button.btn"), "click"); }
});

test("Review PR… asks before a risky target and sends it confirmed", async () => {
  const w = await X.open("prs", { "review.preflight": (p) => X.riskyPreflight("solo/tool#8") });
  const input = w.page.querySelector(".review-pr input");
  input.value = "tool#8";
  w.fire(input, "input");
  w.fire(button(w.page.querySelector(".review-pr"), "Review"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.preflight"), [{ target: "tool#8" }]);
  const box = w.page.querySelector(".confirm-box");
  assert.equal(box.querySelector(".confirm-title").textContent, "Review solo/tool#8 anyway?");
  w.fire(button(box, "Review anyway"), "click");
  await w.settle();
  assert.deepEqual(w.calls("review.request"), [{ target: "tool#8", confirm: true }]);
  assert.equal(w.page.querySelector(".review-pr .inline-note").textContent, "Queued tool#8.");
});
