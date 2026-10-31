// The Repos & Orgs page end to end against the fake DOM: the watched list
// grouped by org, adding repos from the account (search, regular
// expression, typed owner/name), and the override editor, where a repo's own
// settings win over its org's, which win over the defaults. Checks the
// source shown for each value, that an edit creates exactly one override,
// that Reset and Inherit remove it again, and the config a save sends.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const S = require("../settings-ui/schema.js");
const X = require("./page-fixtures.js");
const { plain, withText, bridgeError } = require("./fake-dom.js");

const RAW = () => X.config().raw;
const draft = (w) => plain(w.window.prxApp.app.draft());
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
const entities = (w) => w.page.querySelectorAll("button.entity");
const entity = (w, title) => entities(w).find((b) => b.getAttribute("title").split(":")[0] === title);
const select = (w, title) => w.fire(entity(w, title), "click");
const rows = (w) => w.page.querySelectorAll(".entity-editor .row");
const path = (kind, name, key) => (kind === "org" ? "orgs[" : "repos[") + name + "]." + key;
const source = (w, p) => X.rowOf(w, p).querySelector(".row-aside .badge").textContent;
const showAll = (w) => X.choose(w, w.byId("editor-view").querySelectorAll("input")[1]);
const options = (w) => w.byId("repo-options").querySelectorAll(".option");
const LISTED = [{ name: "acme/api" }, { name: "acme/zeta", private: true }, { name: "acme/zebra" }, { name: "other/thing" }];
// Open the picker (its list loads from discover.repos on focus).
async function openPicker(w) {
  w.byId("add-repo").focus();
  await w.settle();
  return w.byId("add-repo");
}
function search(w, text) { const input = w.byId("add-repo"); input.value = text; w.fire(input, "input"); }

// ---- the watched list -----------------------------------------------------------

test("the watched list groups repos under their org with override counts", async () => {
  const w = await X.open("repos");
  assert.equal(w.page.querySelector("h1").textContent, "Repos & Orgs");
  assert.equal(w.page.querySelector(".list-count").textContent, "Watching 3 repos");
  assert.deepEqual(entities(w).map((b) => [b.getAttribute("title"), b.querySelector(".entity-name").textContent, (b.querySelector(".count") || {}).textContent]),
    [["acme", "acme", "1 override"], ["acme/api", "api", "1 override"], ["acme/web", "web", undefined], ["solo/tool", "tool", undefined]]);
  // An org without an entry is a plain heading with a way to add one.
  assert.equal(w.page.querySelector(".group-name").textContent, "solo");
  assert.ok(button(w.page.querySelectorAll("li.group")[1], "Org overrides"));
  assert.equal(w.page.querySelector(".list-filter").hidden, true, "a short list needs no filter");
  assert.equal(w.page.querySelector(".entity-editor .empty-title").textContent, "Pick a repo or org");
  assert.deepEqual(entities(w).map((b) => b.getAttribute("aria-current")), ["false", "false", "false", "false"]);
});

test("nothing watched yet: both panes say how to start", async () => {
  const w = await X.open("repos", { "config.get": () => X.config((c) => { c.raw.repos = []; c.raw.orgs = []; }) });
  assert.equal(w.page.querySelector(".watched .list-empty").textContent, "No repos yet. Add the first one above and PRX starts watching its pull requests.");
  assert.equal(w.page.querySelector(".entity-editor .empty-title").textContent, "Nothing watched yet");
  assert.equal(w.page.querySelector(".list-count").textContent, "");
  // A config without the lists at all behaves the same, and gains them.
  const bare = await X.open("repos", { "config.get": () => X.config((c) => { delete c.raw.repos; delete c.raw.orgs; }) });
  assert.equal(bare.page.querySelector(".entity-editor .empty-title").textContent, "Nothing watched yet");
  assert.equal(bare.window.prxApp.app.isDirty(), false);
});

test("a long list can be filtered by repo or org name", async () => {
  const many = Array.from({ length: 10 }, (_, i) => ({ name: (i < 6 ? "acme/svc-" : "globex/tool-") + i }));
  const w = await X.open("repos", { "config.get": () => X.config((c) => { c.raw.repos = many; c.raw.orgs = [{ org: "initech" }]; }) });
  const filter = w.page.querySelector(".list-filter");
  assert.equal(filter.hidden, false);
  assert.equal(w.page.querySelector(".list-count").textContent, "Watching 10 repos");
  filter.value = "TOOL";
  w.fire(filter, "input");
  assert.deepEqual(entities(w).map((b) => b.getAttribute("title")), ["globex/tool-6", "globex/tool-7", "globex/tool-8", "globex/tool-9"]);
  // An org name matches its whole group, and an org with no repos is found.
  filter.value = "initech";
  w.fire(filter, "input");
  assert.deepEqual(entities(w).map((b) => b.getAttribute("title")), ["initech"]);
  filter.value = "nothing-like-this " + X.EVIL;
  w.fire(filter, "input");
  assert.equal(w.page.querySelector(".watched .list-empty").textContent, "Nothing watched matches “nothing-like-this " + X.EVIL + "”.");
  assert.deepEqual(X.markupIn(w.page), []);
});

test("a config error naming an entry flags that entry in the list", async () => {
  const w = await X.open("repos", { "config.get": () => X.config((c) => { c.config_error = { message: "repos[acme/web].schedule: invalid cron" }; }) });
  assert.equal(entity(w, "acme/web").getAttribute("title"), "acme/web: config.yml has an error in this entry");
  assert.equal(entity(w, "acme/web").querySelector(".entity-flag svg").getAttribute("aria-label"), "Has a config error");
  assert.equal(entity(w, "acme/api").querySelector(".entity-flag"), null);
  assert.equal(entity(w, "acme").querySelector(".entity-flag"), null);
});

// ---- overrides ------------------------------------------------------------------

test("the editor shows each value with where it comes from: repo, org, or defaults", async () => {
  const w = await X.open("repos");
  select(w, "acme/api");
  assert.equal(entity(w, "acme/api").getAttribute("aria-current"), "true");
  assert.equal(w.page.querySelector(".entity-title").textContent, "acme/apiRepo");
  assert.equal(w.page.querySelector(".editor-head .lede").textContent, "Follows the acme org overrides, then the defaults.");
  // It has an override, so the editor opens on just those.
  assert.deepEqual(rows(w).map((r) => r.getAttribute("data-field")), ["repos[acme/api].engine"]);
  assert.equal(w.byId("editor-view").querySelector("label").textContent, "Overridden (1)");
  showAll(w);
  // Every overridable field, and none of the global ones.
  assert.deepEqual(rows(w).map((r) => r.getAttribute("data-field")), S.OVERRIDE_FIELDS.map((f) => "repos[acme/api]." + f.key));
  assert.equal(rows(w).some((r) => /notifications\.enabled$|menu\./.test(r.getAttribute("data-field"))), false);
  const p = (key) => path("repo", "acme/api", key);
  // Repo wins over org wins over defaults.
  assert.deepEqual([X.field(w, p("engine")).textContent, source(w, p("engine"))], ["codex", "This repo"]);
  assert.deepEqual([X.field(w, p("poll_interval_minutes")).value, source(w, p("poll_interval_minutes"))], ["30", "Org acme"]);
  assert.deepEqual([X.field(w, p("max_concurrent_reviews")).value, source(w, p("max_concurrent_reviews"))], ["2", "Default"]);
  assert.deepEqual([X.field(w, p("skip.drafts")).checked, source(w, p("skip.drafts"))], [true, "Default"]);
  // Inherited rows are marked and have no Reset; the repo's own row has one.
  assert.ok(X.rowOf(w, p("poll_interval_minutes")).classList.contains("inherited"));
  assert.equal(button(X.rowOf(w, p("poll_interval_minutes")), "Reset"), undefined);
  assert.equal(X.rowOf(w, p("engine")).classList.contains("inherited"), false);
  assert.ok(button(X.rowOf(w, p("engine")), "Reset"));
  // A repo outside any configured org follows the defaults only.
  select(w, "solo/tool");
  assert.equal(w.page.querySelector(".editor-head .lede").textContent, "Follows the defaults.");
  assert.deepEqual([X.field(w, path("repo", "solo/tool", "poll_interval_minutes")).value, source(w, path("repo", "solo/tool", "poll_interval_minutes"))], ["10", "Default"]);
  // The org itself: its own value, and everything else from the defaults.
  select(w, "acme");
  showAll(w);
  assert.equal(w.page.querySelector(".entity-title").textContent, "acmeOrg");
  assert.equal(w.page.querySelector(".editor-head .lede").textContent, "Applies to 2 watched repos under acme/, unless a repo overrides it.");
  assert.equal(source(w, path("org", "acme", "poll_interval_minutes")), "This org");
  assert.equal(source(w, path("org", "acme", "engine")), "Default");
  // Looking is not editing.
  assert.equal(w.window.prxApp.app.isDirty(), false);
  assert.deepEqual(draft(w), RAW());
});

test("editing an inherited value creates one override; Reset removes it again", async () => {
  const w = await X.open("repos", X.storing().handlers);
  select(w, "acme/web");
  // No overrides yet: the editor opens on all settings.
  assert.equal(rows(w).length, S.OVERRIDE_FIELDS.length);
  const p = path("repo", "acme/web", "poll_interval_minutes");
  X.type(w, X.field(w, p), "3");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web", poll_interval_minutes: 3 });
  assert.equal(source(w, p), "This repo");
  assert.equal(X.rowOf(w, p).classList.contains("inherited"), false);
  assert.equal(w.byId("editor-view").querySelector("label").textContent, "Overridden (1)");
  assert.equal(entity(w, "acme/web").querySelector(".count").textContent, "1 override");
  // The org and the defaults are untouched.
  assert.deepEqual(draft(w).orgs, RAW().orgs);
  assert.deepEqual(draft(w).defaults, RAW().defaults);
  await X.save(w);
  const expected = RAW();
  expected.repos[1] = { name: "acme/web", poll_interval_minutes: 3 };
  assert.deepEqual(X.lastPut(w), expected);
  // Reset: back to the org's value, and the entry is just its name.
  select(w, "acme/web");
  showAll(w);
  w.fire(button(X.rowOf(w, p), "Reset"), "click");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web" });
  assert.deepEqual([X.field(w, p).value, source(w, p)], ["30", "Org acme"]);
  assert.equal(entity(w, "acme/web").querySelector(".count"), null);
  await X.save(w);
  assert.deepEqual(X.lastPut(w), RAW());
});

test("nested overrides merge field by field and leave no empty partials behind", async () => {
  const w = await X.open("repos", X.storing().handlers);
  select(w, "acme/web");
  const before = path("repo", "acme/web", "cooldown.before_first_review_minutes");
  X.type(w, X.field(w, before), "5");
  X.toggle(w, X.field(w, path("repo", "acme/web", "skip.drafts")), false);
  // Only the leaf that was edited is set; its sibling still inherits.
  assert.deepEqual(draft(w).repos[1], { name: "acme/web", cooldown: { before_first_review_minutes: 5 }, skip: { drafts: false } });
  assert.equal(source(w, path("repo", "acme/web", "cooldown.between_re_reviews_minutes")), "Default");
  assert.equal(X.field(w, path("repo", "acme/web", "cooldown.between_re_reviews_minutes")).value, "30");
  w.fire(button(X.rowOf(w, before), "Reset"), "click");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web", skip: { drafts: false } });
  w.fire(button(X.rowOf(w, path("repo", "acme/web", "skip.drafts")), "Reset"), "click");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web" });
  // A bad number on an override row blocks the save like any other.
  X.type(w, X.field(w, before), "soon");
  assert.equal(X.errorOf(w, before), "Enter a whole number");
  await X.save(w);
  assert.equal(w.calls("config.put").length, 0);
  assert.deepEqual(draft(w).repos[1], { name: "acme/web" });
});

test("dropdown overrides offer Inherit, which says what it stands for and unsets the override", async () => {
  const w = await X.open("repos", X.storing().handlers);
  select(w, "acme/web");
  const mode = X.field(w, path("repo", "acme/web", "review_mode"));
  assert.equal(mode.querySelector(".select-label").textContent, "Review and comment");
  assert.equal(mode.querySelector(".select-tag").textContent, "Inherited");
  assert.deepEqual(X.optionLabels(w, mode), ["Inherit", "Review and comment", "Monitor", "Off"]);
  assert.equal(w.byId(mode.id + "-list").querySelector(".select-detail").textContent, "Review and comment, from the defaults");
  X.pickOption(w, mode, "Monitor");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web", review_mode: "monitor" });
  X.pickOption(w, X.field(w, path("repo", "acme/web", "review_mode")), "Inherit");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web" });
  // The engine of a repo under an org that sets one inherits from the org.
  const v = await X.open("repos", { "config.get": () => X.config((c) => { c.raw.orgs[0].engine = "pi"; }) });
  select(v, "acme/web");
  const engine = X.field(v, path("repo", "acme/web", "engine"));
  assert.equal(engine.querySelector(".select-label").textContent, "pi");
  assert.equal(v.byId(engine.id + "-list").querySelector(".select-detail").textContent, "pi, from the acme org");
  assert.equal(source(v, path("repo", "acme/web", "engine")), "Org acme");
});

test("an event sound override inherits the defaults' choice, or the built-in sound when none is set", async () => {
  const w = await X.open("repos", X.storing((c) => { c.raw.defaults.notifications.sounds = { on_review_complete_blocking: "Sosumi" }; }).handlers);
  select(w, "acme/web");
  showAll(w);
  const sound = (key) => X.field(w, path("repo", "acme/web", "notifications.sounds." + key));
  const detail = (trigger) => w.byId(trigger.id + "-list").querySelector(".select-detail").textContent;
  assert.equal(sound("on_review_complete_blocking").querySelector(".select-label").textContent, "Sosumi");
  assert.equal(detail(sound("on_review_complete_blocking")), "Sosumi, from the defaults");
  assert.equal(detail(sound("on_review_complete_approved")), "Built in (Glass), from the defaults");
  assert.equal(detail(sound("on_new_pr")), "Tier's sound, from the defaults");
  assert.deepEqual(X.optionLabels(w, sound("on_new_pr")).slice(0, 3), ["Inherit", "No sound", "Basso"]);
  X.pickOption(w, sound("on_review_complete_blocking"), "No sound");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web", notifications: { sounds: { on_review_complete_blocking: "none" } } });
  X.pickOption(w, sound("on_review_complete_blocking"), "Inherit");
  assert.deepEqual(draft(w).repos[1], { name: "acme/web" }, "no empty notifications left behind");
});

test("the Overridden view lists only what the entry sets, and empties when the last one is reset", async () => {
  const w = await X.open("repos");
  select(w, "acme/api");
  assert.equal(rows(w).length, 1);
  w.fire(button(X.rowOf(w, "repos[acme/api].engine"), "Reset"), "click");
  assert.equal(rows(w).length, 0);
  assert.equal(w.page.querySelector(".entity-editor .empty-title").textContent, "No overrides");
  assert.deepEqual(draft(w).repos[0], { name: "acme/api" });
  w.fire(button(w.page.querySelector(".entity-editor"), "Show all settings"), "click");
  assert.equal(rows(w).length, S.OVERRIDE_FIELDS.length);
});

test("org overrides: created from the group, edited, and removed without touching its repos", async () => {
  const w = await X.open("repos", X.storing().handlers);
  w.fire(button(w.page.querySelectorAll("li.group")[1], "Org overrides"), "click");
  assert.deepEqual(draft(w).orgs, [{ org: "acme", poll_interval_minutes: 30 }, { org: "solo" }]);
  assert.deepEqual(plain(w.window.prxApp.app.ui.selection), { kind: "org", name: "solo" });
  assert.equal(w.page.querySelector(".editor-head .lede").textContent, "Applies to 1 watched repo under solo/, unless a repo overrides it.");
  X.type(w, X.field(w, path("org", "solo", "diff_max_files")), "10");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).orgs[1], { org: "solo", diff_max_files: 10 });
  // Its repo now inherits from it.
  select(w, "solo/tool");
  showAll(w);
  assert.deepEqual([X.field(w, path("repo", "solo/tool", "diff_max_files")).value, source(w, path("repo", "solo/tool", "diff_max_files"))], ["10", "Org solo"]);
  // Removing the org's overrides keeps watching its repos.
  const remove = w.page.querySelectorAll("button.remove").find((b) => b.getAttribute("aria-label") === "Remove the overrides for solo");
  w.fire(remove, "click");
  assert.deepEqual(draft(w).orgs, [{ org: "acme", poll_interval_minutes: 30 }]);
  assert.deepEqual(draft(w).repos, RAW().repos);
  assert.deepEqual([X.field(w, path("repo", "solo/tool", "diff_max_files")).value, source(w, path("repo", "solo/tool", "diff_max_files"))], ["60", "Default"]);
});

test("Stop watching removes the repo, and closes its editor when it was open", async () => {
  const w = await X.open("repos", X.storing().handlers);
  select(w, "acme/api");
  const stop = (name) => w.page.querySelectorAll("button.remove").find((b) => b.getAttribute("aria-label") === "Stop watching " + name);
  w.fire(stop("acme/web"), "click");
  assert.deepEqual(draft(w).repos.map((r) => r.name), ["acme/api", "solo/tool"]);
  assert.equal(w.page.querySelector(".entity-title").textContent, "acme/apiRepo", "another repo's editor stays open");
  assert.equal(w.page.querySelector(".list-count").textContent, "Watching 2 repos");
  w.fire(stop("acme/api"), "click");
  assert.equal(w.window.prxApp.app.ui.selection, null);
  assert.equal(w.page.querySelector(".entity-editor .empty-title").textContent, "Pick a repo or org");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).repos, [{ name: "solo/tool" }]);
  // The org entry is kept even with no watched repo left under it.
  assert.deepEqual(X.lastPut(w).orgs, RAW().orgs);
});

// ---- adding repos ---------------------------------------------------------------

test("the picker lists the account's repos that are not watched yet", async () => {
  const w = await X.open("repos", { "discover.repos": () => ({ repos: LISTED }) });
  const input = w.byId("add-repo");
  assert.equal(input.getAttribute("role"), "combobox");
  assert.equal(input.getAttribute("aria-expanded"), "false");
  assert.equal(w.byId("repo-options").hidden, true);
  assert.equal(w.calls("discover.repos").length, 0, "the account is only listed when asked");
  input.focus();
  assert.equal(w.byId("repo-options").querySelector(".picker-note").textContent, "Loading your repositories…");
  await w.settle();
  assert.equal(input.getAttribute("aria-expanded"), "true");
  assert.deepEqual(options(w).map((o) => o.querySelector(".option-name").textContent), ["acme/zeta", "acme/zebra", "other/thing"]);
  assert.equal(options(w)[0].querySelector("svg").getAttribute("aria-label"), "Private");
  assert.equal(options(w)[1].querySelector("svg"), null);
  // Clicking one adds it, removes it from the list and marks the row fresh.
  w.fire(options(w)[0], "click");
  assert.deepEqual(draft(w).repos.map((r) => r.name), ["acme/api", "acme/web", "solo/tool", "acme/zeta"]);
  assert.deepEqual(draft(w).repos[3], { name: "acme/zeta" });
  assert.deepEqual(options(w).map((o) => o.querySelector(".option-name").textContent), ["acme/zebra", "other/thing"]);
  assert.ok(entity(w, "acme/zeta").closest(".entity-line").classList.contains("fresh"));
  assert.equal(w.window.prxApp.app.isDirty(), true);
  // Asked once: reopening does not list the account again.
  w.fire(input, "keydown", { key: "Escape" });
  assert.equal(w.byId("repo-options").hidden, true);
  w.fire(input, "click");
  await w.settle();
  assert.equal(w.calls("discover.repos").length, 1);
});

test("the picker filters by text, by regular expression, and adds every match", async () => {
  const w = await X.open("repos", X.storing(null, { "discover.repos": () => ({ repos: LISTED }) }).handlers);
  await openPicker(w);
  search(w, "ZE acme");
  assert.deepEqual(options(w).map((o) => o.querySelector(".option-name").textContent), ["acme/zeta", "acme/zebra"]);
  const all = w.byId("repo-options").querySelector("[data-all]");
  assert.equal(all.textContent, "Add all 2 matches");
  assert.equal(w.byId("repo-options").querySelector(".picker-hint"), null);
  // A /pattern/ is a regular expression, and says what "all" means then.
  search(w, "/^other\\/|zebra$/");
  assert.equal(w.page.querySelector(".regex-toggle").getAttribute("aria-pressed"), "true");
  assert.deepEqual(options(w).map((o) => o.querySelector(".option-name").textContent), ["acme/zebra", "other/thing"]);
  assert.equal(w.byId("repo-options").querySelector(".picker-hint").textContent, "Adds what matches today, not future repos.");
  w.fire(w.byId("repo-options").querySelector("[data-all]"), "click");
  assert.deepEqual(draft(w).repos.map((r) => r.name), ["acme/api", "acme/web", "solo/tool", "acme/zebra", "other/thing"]);
  assert.equal(w.byId("add-repo").value, "");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).repos.slice(3), [{ name: "acme/zebra" }, { name: "other/thing" }]);
  // A pattern that does not compile says so and offers nothing.
  w.fire(w.page.querySelector(".regex-toggle"), "click");
  search(w, "acme/(");
  assert.match(w.byId("repo-options").querySelector(".picker-note").textContent, /^That pattern doesn't compile: /);
  assert.ok(w.page.querySelector(".picker").classList.contains("bad"));
  assert.equal(options(w).length, 0);
  search(w, "zeta");
  assert.equal(w.page.querySelector(".picker").classList.contains("bad"), false);
  assert.deepEqual(options(w).map((o) => o.querySelector(".option-name").textContent), ["acme/zeta"]);
});

test("a typed owner/name can be added even when GitHub did not list it", async () => {
  const w = await X.open("repos", { "discover.repos": () => ({ repos: LISTED }) });
  const input = await openPicker(w);
  search(w, "secret-org/tool");
  assert.deepEqual(options(w).map((o) => [o.querySelector(".option-name").textContent, o.querySelector(".option-hint").textContent]), [["secret-org/tool", "Add as typed"]]);
  assert.equal(w.fire(input, "keydown", { key: "Enter" }).defaultPrevented, true);
  assert.deepEqual(draft(w).repos[3], { name: "secret-org/tool" });
  assert.equal(input.value, "");
  // Already watched: offered as such, and Enter adds nothing.
  search(w, "acme/web");
  assert.equal(options(w)[0].querySelector(".option-hint").textContent, "Already watched");
  assert.equal(options(w)[0].getAttribute("aria-disabled"), "true");
  w.fire(input, "keydown", { key: "Enter" });
  w.fire(options(w)[0], "click");
  assert.equal(draft(w).repos.length, 4);
  // Not an owner/name: nothing to add.
  for (const text of ["just-a-name", "a/b/c", "a b/c", "/x"]) {
    search(w, text);
    assert.equal(options(w).length, 0, text);
  }
  assert.equal(w.byId("repo-options").querySelector(".picker-note").textContent, "No repository matches. Type a full owner/name to add one GitHub didn't list.");
});

test("the picker's arrow keys move through the options and Enter adds the active one", async () => {
  const w = await X.open("repos", { "discover.repos": () => ({ repos: LISTED }) });
  const input = await openPicker(w);
  const active = () => options(w).findIndex((o) => o.getAttribute("aria-selected") === "true");
  assert.equal(active(), -1);
  w.fire(input, "keydown", { key: "ArrowDown" });
  assert.equal(active(), 0);
  assert.equal(input.getAttribute("aria-activedescendant"), "repo-option-0");
  w.fire(input, "keydown", { key: "ArrowUp" });
  assert.equal(active(), 2, "wraps to the last option");
  w.fire(input, "keydown", { key: "ArrowDown" });
  w.fire(input, "keydown", { key: "ArrowDown" });
  assert.equal(active(), 1);
  // Several matches and none active: Enter adds nothing.
  search(w, "acme");
  w.fire(input, "keydown", { key: "Enter" });
  assert.equal(draft(w).repos.length, 3);
  w.fire(input, "keydown", { key: "ArrowDown" });
  w.fire(input, "keydown", { key: "Enter" });
  assert.deepEqual(draft(w).repos[3], { name: "acme/zeta" });
  // A single match needs no arrow.
  search(w, "other");
  w.fire(input, "keydown", { key: "Enter" });
  assert.deepEqual(draft(w).repos[4], { name: "other/thing" });
});

test("the picker's empty, failing and oversized lists each explain themselves", async () => {
  const note = (w) => w.byId("repo-options").querySelectorAll(".picker-note").map((n) => n.textContent);
  const none = await X.open("repos", { "discover.repos": () => ({ repos: [] }) });
  await openPicker(none);
  assert.deepEqual(note(none), ["GitHub listed no repositories for this account."]);
  const watched = await X.open("repos", { "discover.repos": () => ({ repos: [{ name: "acme/api" }, { name: "acme/web" }] }) });
  await openPicker(watched);
  assert.deepEqual(note(watched), ["Every repository GitHub listed is already watched."]);
  // gh failing: the reason, a retry, and typing still works.
  let fail = true;
  const broken = await X.open("repos", { "discover.repos": () => { if (fail) throw bridgeError("daemon", "gh: not logged in " + X.EVIL); return { repos: LISTED }; } });
  await openPicker(broken);
  assert.deepEqual(note(broken), ["Couldn't list your repositories. gh: not logged in " + X.EVIL + " You can still type an owner/name.Try again"]);
  assert.deepEqual(X.markupIn(broken.page), []);
  search(broken, "typed/anyway");
  assert.equal(options(broken)[0].querySelector(".option-hint").textContent, "Add as typed");
  fail = false;
  search(broken, "");
  broken.fire(button(broken.byId("repo-options"), "Try again"), "click");
  await broken.settle();
  assert.equal(options(broken).length, 3);
  // Offline has its own wording.
  const off = await X.open("repos", { "discover.repos": X.offline });
  await openPicker(off);
  assert.match(note(off)[0], /^Couldn't list your repositories\. The PRX daemon is not running\./);
  // More matches than fit: the first 60, and how many there are.
  const many = await X.open("repos", { "discover.repos": () => ({ repos: Array.from({ length: 75 }, (_, i) => ({ name: "big/repo-" + i })) }) });
  await openPicker(many);
  assert.equal(options(many).length, 60);
  assert.deepEqual(note(many), ["Showing 60 of 75. Keep typing to narrow it down."]);
});

test("hostile repo names from the account, and typed ones, are text", async () => {
  const evil = "evil/" + X.EVIL.replace(/[\s/]/g, "_");
  const w = await X.open("repos", { "discover.repos": () => ({ repos: [{ name: evil }] }) });
  await openPicker(w);
  assert.equal(options(w)[0].querySelector(".option-name").textContent, evil);
  w.fire(options(w)[0], "click");
  assert.equal(entity(w, evil).querySelector(".entity-name").textContent, evil.slice(5));
  select(w, evil);
  assert.equal(w.page.querySelector(".entity-title").textContent, evil + "Repo");
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
  // Its override rows still work: the path survives the odd characters.
  const p = "repos[" + evil + "].poll_interval_minutes";
  X.type(w, X.field(w, p), "4");
  assert.equal(draft(w).repos[3].poll_interval_minutes, 4);
  assert.equal(source(w, p), "This repo");
});
