// The settings form end to end: the real shell (app.js), bridge, form
// controls and pages against the fake DOM and a fake daemon. Covers loading,
// dirty tracking, the exact config a save sends, local validation, daemon
// rejections routed to the offending field, revert, offline, and the
// config.yml page.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const X = require("./page-fixtures.js");
const { boot, plain, withText, bridgeError } = require("./fake-dom.js");

const RAW = () => X.config().raw;
const status = (w) => [w.byId("save-status").className.trim(), w.byId("save-status").textContent];
const bar = (w) => [w.byId("save").disabled, w.byId("revert").disabled, w.document.body.classList.contains("dirty")];
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
const poll = (w) => X.field(w, "defaults.poll_interval_minutes");
// Every key at any depth of a value.
function keysOf(value, out) {
  out = out || new Set();
  if (value && typeof value === "object") Object.keys(value).forEach((k) => { if (!Array.isArray(value)) out.add(k); keysOf(value[k], out); });
  return out;
}

test("loading: a skeleton, a quiet save bar, and nothing editable until the config arrives", () => {
  const w = boot({ hash: "#page=general", now: X.NOW, daemon: X.daemon(), hold: () => true });
  const skeleton = w.page.querySelector(".skeleton");
  assert.equal(skeleton.getAttribute("aria-busy"), "true");
  assert.equal(skeleton.getAttribute("aria-label"), "Loading settings");
  assert.equal(w.page.querySelectorAll("input, textarea, [role=combobox]").length, 0);
  assert.deepEqual(bar(w), [true, true, false]);
  assert.equal(w.byId("daemon").textContent, "Connecting…");
  assert.deepEqual(w.sent.map((m) => m.method), ["config.get"]);
});

test("loaded: the page shows the config, clean, and the draft is a copy of it", async () => {
  const w = await X.open("general");
  assert.equal(w.page.querySelector("h1").textContent, "General");
  assert.equal(poll(w).value, "10");
  assert.equal(X.field(w, "defaults.max_concurrent_reviews").value, "2");
  assert.equal(X.field(w, "defaults.agent_timeout_seconds").value, "900");
  assert.deepEqual(w.page.querySelectorAll(".row .unit").map((u) => u.textContent).slice(0, 3), ["min", "", "sec"]);
  assert.deepEqual(bar(w), [true, true, false]);
  assert.deepEqual(status(w), ["status", ""]);
  assert.equal(w.byId("daemon").textContent, "Daemon running");
  assert.equal(w.byId("save-note").hidden, true);
  assert.equal(w.page.querySelector(".footnote code").textContent, "/Users/test/.config/prx/config.yml");
  const app = w.window.prxApp.app;
  assert.deepEqual(plain(app.draft()), RAW());
  assert.notEqual(app.draft(), app.loaded().raw);
  assert.equal(app.isDirty(), false);
});

test("an edit marks the form dirty, and Save sends the whole config with just that change", async () => {
  const daemon = X.storing();
  const w = await X.open("general", daemon.handlers);
  X.type(w, poll(w), "5");
  assert.deepEqual(bar(w), [false, false, true]);
  assert.deepEqual(status(w), ["status pending", "Unsaved changes"]);
  // The loaded config is untouched until the daemon accepts the save.
  assert.equal(w.window.prxApp.app.loaded().raw.defaults.poll_interval_minutes, 10);
  await X.save(w);
  const expected = RAW();
  expected.defaults.poll_interval_minutes = 5;
  assert.deepEqual(w.calls("config.put"), [{ config: expected }]);
  assert.deepEqual(status(w), ["status ok", "Saved and applied"]);
  assert.deepEqual(bar(w), [true, true, false]);
  // The form was reloaded from what the daemon now has.
  assert.equal(poll(w).value, "5");
  assert.equal(w.window.prxApp.app.isDirty(), false);
  // Nothing to save: the button is off, and a ⌘S sends nothing either.
  w.byId("save").click();
  w.fire(w.document.body, "keydown", { key: "s", metaKey: true });
  await w.settle();
  assert.equal(daemon.state.puts, 1);
});

test("a save never invents settings: unset fields stay unset, and only current names are sent", async () => {
  const w = await X.open("review", X.storing().handlers);
  X.type(w, X.field(w, "defaults.diff_max_files"), "80");
  await X.save(w);
  const sent = X.lastPut(w);
  const expected = RAW();
  expected.defaults.diff_max_files = 80;
  assert.deepEqual(sent, expected);
  // `auto_open`, `schedule` and `comments` were never set and still are not.
  assert.deepEqual(["auto_open", "schedule", "comments", "review_heading"].filter((k) => k in sent.defaults), []);
  const keys = keysOf(sent);
  assert.ok(keys.has("engines") && keys.has("engine") && keys.has("brief"));
  for (const legacy of ["agents", "agent", "executive_summary", "exec_summary"]) assert.equal(keys.has(legacy), false, legacy);
  // Nothing the daemon only reports is sent back as config.
  for (const reported of ["effective", "yaml", "path", "review_contract", "shipped_review_prompt", "shipped_templates", "config_error"]) assert.equal(keys.has(reported), false, reported);
});

test("typing enables Save before the field is left, and Save commits the focused field", async () => {
  const w = await X.open("general", X.storing().handlers);
  const input = poll(w);
  input.focus();
  input.value = "7";
  w.fire(input, "input");
  assert.deepEqual(bar(w), [false, false, true]);
  assert.equal(w.window.prxApp.app.draft().defaults.poll_interval_minutes, 10, "not committed while still typing");
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.poll_interval_minutes, 7);
});

test("⌘S and Ctrl+S save; other chords do not", async () => {
  const daemon = X.storing();
  const w = await X.open("general", daemon.handlers);
  X.type(w, poll(w), "6");
  for (const chord of [{ key: "s", metaKey: true, shiftKey: true }, { key: "s", metaKey: true, altKey: true }, { key: "s" }, { key: "x", metaKey: true }]) {
    assert.equal(w.fire(w.document.body, "keydown", chord).defaultPrevented, false);
  }
  await w.settle();
  assert.equal(daemon.state.puts, 0);
  assert.equal(w.fire(w.document.body, "keydown", { key: "s", metaKey: true }).defaultPrevented, true);
  await w.settle();
  assert.equal(daemon.state.puts, 1);
  X.type(w, poll(w), "8");
  w.fire(w.document.body, "keydown", { key: "s", ctrlKey: true });
  await w.settle();
  assert.equal(daemon.state.puts, 2);
  assert.equal(X.lastPut(w).defaults.poll_interval_minutes, 8);
});

test("a number that is not a whole number is flagged at once and blocks the save", async () => {
  const daemon = X.storing();
  const w = await X.open("general", daemon.handlers);
  for (const bad of ["abc", "", "1.5", "-3", "1e3", "12 min"]) {
    X.type(w, poll(w), bad);
    assert.equal(X.errorOf(w, "defaults.poll_interval_minutes"), "Enter a whole number", JSON.stringify(bad));
    assert.ok(X.rowOf(w, "defaults.poll_interval_minutes").classList.contains("invalid"));
    assert.equal(w.window.prxApp.app.draft().defaults.poll_interval_minutes, 10, "the bad value never reaches the draft");
  }
  await X.save(w);
  assert.equal(daemon.state.puts, 0);
  assert.deepEqual(status(w), ["status error", "Fix the highlighted field before saving."]);
  assert.equal(w.document.activeElement, poll(w), "focus goes to the field to fix");
  // The error is found from another page too.
  w.window.prxApp.go("review");
  await X.save(w);
  assert.equal(w.window.prxApp.state.page, "general");
  assert.equal(X.errorOf(w, "defaults.poll_interval_minutes"), "Enter a whole number");
  assert.equal(daemon.state.puts, 0);
  // Surrounding spaces are fine; ranges are the daemon's call.
  X.type(w, poll(w), " 0 ");
  assert.equal(X.errorOf(w, "defaults.poll_interval_minutes"), null);
  assert.equal(X.rowOf(w, "defaults.poll_interval_minutes").classList.contains("invalid"), false);
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.poll_interval_minutes, 0);
});

test("Revert reloads the saved config and drops edits and local errors", async () => {
  const w = await X.open("general", X.storing().handlers);
  X.type(w, poll(w), "99");
  X.type(w, X.field(w, "defaults.max_concurrent_reviews"), "many");
  assert.equal(X.errorOf(w, "defaults.max_concurrent_reviews"), "Enter a whole number");
  w.fire(w.byId("revert"), "click");
  await w.settle();
  assert.equal(poll(w).value, "10");
  assert.equal(X.field(w, "defaults.max_concurrent_reviews").value, "2");
  assert.equal(X.errorOf(w, "defaults.max_concurrent_reviews"), null);
  assert.deepEqual(bar(w), [true, true, false]);
  assert.deepEqual(status(w), ["status", ""]);
  assert.deepEqual(plain(w.window.prxApp.app.draft()), RAW());
  assert.equal(w.calls("config.put").length, 0);
});

test("a rejected save shows the daemon's message on the field it names and keeps the edits", async () => {
  let reject = true;
  const daemon = X.storing(null, { "config.put": (p) => {
    if (reject) throw bridgeError("rejected", "defaults.poll_interval_minutes must be at least 1", "defaults.poll_interval_minutes");
    return { ok: true };
  } });
  const w = await X.open("general", daemon.handlers);
  X.type(w, poll(w), "0");
  X.type(w, X.field(w, "defaults.max_concurrent_reviews"), "4");
  await X.save(w);
  assert.deepEqual(status(w), ["status error", "Not saved: defaults.poll_interval_minutes must be at least 1"]);
  assert.equal(X.errorOf(w, "defaults.poll_interval_minutes"), "defaults.poll_interval_minutes must be at least 1");
  assert.ok(X.rowOf(w, "defaults.poll_interval_minutes").classList.contains("invalid"));
  assert.equal(X.errorOf(w, "defaults.max_concurrent_reviews"), null);
  assert.equal(w.document.activeElement, poll(w));
  // Still dirty, with both edits, so the user can fix and retry.
  assert.deepEqual(bar(w), [false, false, true]);
  assert.equal(poll(w).value, "0");
  assert.equal(X.field(w, "defaults.max_concurrent_reviews").value, "4");
  // Editing the field clears its error and the red status.
  X.type(w, poll(w), "3");
  assert.equal(X.errorOf(w, "defaults.poll_interval_minutes"), null);
  assert.deepEqual(status(w), ["status pending", "Unsaved changes"]);
  reject = false;
  await X.save(w);
  assert.deepEqual(status(w), ["status ok", "Saved and applied"]);
  assert.equal(w.calls("config.put").length, 2);
});

test("a rejection for a field on another page opens that page and focuses the field", async () => {
  const cases = [
    ["defaults.cooldown.before_first_review_minutes", "cooldown too long", "review", "defaults.cooldown.before_first_review_minutes"],
    // Unrooted daemon paths are defaults-level.
    ["skip.branches", "invalid glob 'rel[ease'", "review", "defaults.skip.branches"],
    ["schedule", "invalid cron", "schedule", "defaults.schedule"],
    ["menu.sort", "unknown sort", "menubar", "menu.sort"],
    ["notification_tiers.urgent.sound", "unknown sound", "notifications", "notification_tiers.urgent"],
    // A parent path lands on the first field under it.
    ["defaults.cooldown", "cooldown is invalid", "review", "defaults.cooldown.before_first_review_minutes"],
    ["defaults.identity.credential", "'ghp_x' must be an environment variable name", "identity", "defaults.identity.credential"],
  ];
  for (const [fieldPath, message, page, row] of cases) {
    const w = await X.open("general", { "config.put": () => { throw bridgeError("rejected", message, fieldPath); } });
    X.type(w, poll(w), "5");
    await X.save(w);
    assert.equal(w.window.prxApp.state.page, page, fieldPath);
    assert.equal(X.errorOf(w, row), message, fieldPath);
    assert.deepEqual(status(w), ["status error", "Not saved: " + message], fieldPath);
    // The edit made before the rejection is still in the draft.
    assert.equal(w.window.prxApp.app.draft().defaults.poll_interval_minutes, 5);
  }
});

test("a rejection naming a repo or org opens Repos & Orgs on that entry", async () => {
  const w = await X.open("general", { "config.put": () => { throw bridgeError("rejected", "repos[1].schedule: invalid cron", "repos[1].schedule"); } });
  X.type(w, poll(w), "5");
  await X.save(w);
  assert.equal(w.window.prxApp.state.page, "repos");
  // The daemon's index became the repo's name, and its editor is open.
  assert.deepEqual(plain(w.window.prxApp.app.ui.selection), { kind: "repo", name: "acme/web" });
  assert.equal(w.page.querySelector(".entity-title").textContent, "acme/webRepo");
  assert.equal(X.errorOf(w, "repos[acme/web].schedule"), "repos[1].schedule: invalid cron");
  // "unknown engine" without a level points at the entry that uses it.
  const v = await X.open("general", { "config.put": () => { throw bridgeError("rejected", "unknown engine 'codex': define it under `engines:` in the config", "engine"); } });
  X.type(v, poll(v), "5");
  await X.save(v);
  assert.equal(v.window.prxApp.state.page, "repos");
  assert.deepEqual(plain(v.window.prxApp.app.ui.selection), { kind: "repo", name: "acme/api" });
  assert.match(X.errorOf(v, "repos[acme/api].engine"), /^unknown engine 'codex'/);
});

test("a rejection without a field, or for one no page shows, is still reported", async () => {
  for (const fieldPath of [null, "defaults.no_such_setting.deep"]) {
    const w = await X.open("review", { "config.put": () => { throw bridgeError("rejected", "the config is invalid: " + X.EVIL, fieldPath); } });
    X.type(w, X.field(w, "defaults.diff_max_lines"), "1");
    await X.save(w);
    assert.deepEqual(status(w), ["status error", "Not saved: the config is invalid: " + X.EVIL], String(fieldPath));
    assert.deepEqual(X.markupIn(w.document.body), []);
    assert.deepEqual(bar(w), [false, false, true]);
  }
});

test("saving while the daemon is down keeps the edits and says so", async () => {
  let up = true;
  const daemon = X.storing(null, { "config.put": () => { if (!up) X.offline(); return { ok: true }; } });
  const w = await X.open("general", daemon.handlers);
  X.type(w, poll(w), "5");
  up = false;
  await X.save(w);
  assert.deepEqual(status(w), ["status error", "Not saved. The PRX daemon is not running."]);
  assert.deepEqual(bar(w), [false, false, true]);
  assert.equal(poll(w).value, "5");
  up = true;
  await X.save(w);
  assert.deepEqual(status(w), ["status ok", "Saved and applied"]);
  // Any other failure shows its own message.
  const v = await X.open("general", { "config.put": X.failing("disk full") });
  X.type(v, poll(v), "5");
  await X.save(v);
  assert.deepEqual(status(v), ["status error", "Not saved. disk full"]);
});

test("the saved message names repos that started or stopped being watched", async () => {
  const say = async (result) => {
    const w = await X.open("general", { "config.put": () => result });
    X.type(w, poll(w), "5");
    await X.save(w);
    return w.byId("save-status").textContent;
  };
  assert.equal(await say({ ok: true, added: ["new/repo", "new/other"], removed: [] }), "Saved: now watching new/repo, new/other");
  assert.equal(await say({ ok: true, added: [], removed: ["old/repo"] }), "Saved: stopped watching old/repo");
  assert.equal(await say({ ok: true, added: ["a/b"], removed: ["c/d"] }), "Saved: now watching a/b; stopped watching c/d");
  assert.equal(await say({ ok: true }), "Saved and applied");
});

test("a second Save while one is in flight sends nothing more", async () => {
  const w = await X.open("general", X.storing().handlers, { hold: (m) => m.method === "config.put" });
  X.type(w, poll(w), "5");
  w.fire(w.byId("save"), "click");
  assert.deepEqual(status(w), ["status", "Saving…"]);
  assert.deepEqual([w.byId("save").disabled, w.byId("revert").disabled], [true, true]);
  w.fire(w.byId("save"), "click");
  w.fire(w.document.body, "keydown", { key: "s", metaKey: true });
  assert.equal(w.calls("config.put").length, 1);
  w.release();
  await w.settle();
  assert.deepEqual(status(w), ["status ok", "Saved and applied"]);
});

test("the save bar warns when a save would drop comments or replace a broken file", async () => {
  const commented = await X.open("general", { "config.get": () => X.config((c) => { c.yaml = "# my notes\ndefaults:\n  engine: claude\n"; }) });
  assert.equal(commented.byId("save-note").hidden, true, "no warning until there is something to save");
  X.type(commented, poll(commented), "5");
  assert.equal(commented.byId("save-note").hidden, false);
  assert.equal(commented.byId("save-note").textContent, "Saving the form rewrites config.yml without its comments. Edit config.yml directly to keep them.");
  const plainYaml = await X.open("general");
  X.type(plainYaml, poll(plainYaml), "5");
  assert.equal(plainYaml.byId("save-note").hidden, true);
  const broken = await X.open("general", { "config.get": () => X.config((c) => { c.config_error = { message: "line 4: " + X.EVIL }; }) });
  assert.equal(broken.byId("save-note").textContent, "The config file has errors: saving the form replaces it with the running config plus your edits.");
  const banner = broken.page.querySelector(".banner.error");
  assert.equal(banner.getAttribute("role"), "alert");
  assert.equal(banner.textContent, "config.yml has an error, so the daemon is still running the last good version. line 4: " + X.EVIL);
  assert.deepEqual(X.markupIn(broken.page), []);
});

test("offline at start: the offline page, then the form once the daemon is back", async () => {
  let up = false;
  const w = await X.open("review", { "config.get": () => { if (!up) X.offline(); return X.config(); } });
  assert.equal(w.page.querySelector(".empty-title").textContent, "The PRX daemon isn't running");
  assert.match(w.page.querySelector(".empty-text").textContent, /Start it from the PRX menu bar \(Start daemon\) or with prx daemon start/);
  assert.equal(w.byId("daemon").textContent, "Daemon not running");
  assert.equal(w.page.querySelectorAll("input, textarea").length, 0);
  assert.deepEqual(bar(w), [true, true, false]);
  // Still offline: the page stays, without an error status.
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".empty-title").textContent, "The PRX daemon isn't running");
  assert.deepEqual(status(w), ["status", ""]);
  up = true;
  w.fire(button(w.page, "Try again"), "click");
  await w.settle();
  assert.equal(w.page.querySelector("h1").textContent, "Review");
  assert.equal(w.byId("daemon").textContent, "Daemon running");
});

test("a config that cannot be loaded for another reason says why", async () => {
  const w = await X.open("general", { "config.get": X.failing("config.yml: permission denied") });
  assert.deepEqual(status(w), ["status error", "Could not load settings: config.yml: permission denied"]);
  assert.equal(w.page.querySelector(".skeleton").getAttribute("aria-busy"), "true");
  assert.equal(w.byId("daemon").textContent, "Connecting…");
});

test("window focus picks up outside edits when clean, and never replaces unsaved ones", async () => {
  let interval = 10;
  const w = await X.open("general", { "config.get": () => X.config((c) => { c.raw.defaults.poll_interval_minutes = interval; }) });
  const before = poll(w);
  w.fireWindow("focus");
  await w.settle();
  assert.equal(w.calls("config.get").length, 2);
  assert.equal(poll(w), before, "an unchanged config does not rebuild the page");
  interval = 20;
  w.fireWindow("focus");
  await w.settle();
  assert.equal(poll(w).value, "20");
  // Dirty: the file is not even read.
  X.type(w, poll(w), "5");
  interval = 30;
  w.fireWindow("focus");
  await w.settle();
  assert.equal(w.calls("config.get").length, 3);
  assert.equal(poll(w).value, "5");
  // A daemon that went away meanwhile only changes the indicator.
  const v = await X.open("general", { "config.get": (() => { let n = 0; return () => { if (n++) X.offline(); return X.config(); }; })() });
  v.fireWindow("focus");
  await v.settle();
  assert.equal(v.byId("daemon").textContent, "Daemon not running");
  assert.equal(poll(v).value, "10");
  assert.deepEqual(status(v), ["status", ""]);
});

test("the sidebar lists every page once and marks the current one", async () => {
  const w = await X.open("review");
  const buttons = w.byId("nav").querySelectorAll("button");
  assert.deepEqual(buttons.map((b) => b.textContent), ["Dashboard", "Pull requests", "History", "Usage & stats", "General", "Review", "Messages", "Schedule",
    "Notifications", "Engines", "Repos & Orgs", "Menu bar", "Account & Browser", "config.yml"]);
  assert.deepEqual(buttons.filter((b) => b.getAttribute("aria-current") === "page").map((b) => b.dataset.page), ["review"]);
  w.fire(buttons.find((b) => b.dataset.page === "menubar"), "click");
  assert.equal(w.window.prxApp.state.page, "menubar");
  assert.equal(w.page.querySelector("h1").textContent, "Menu bar");
  assert.equal(w.document.body.dataset.page, "menubar");
  // An unknown page falls back to General instead of a blank window.
  w.window.prxApp.go("no-such-page");
  assert.equal(w.window.prxApp.state.page, "general");
  // The skip link moves focus to the page.
  w.fire(w.byId("skip"), "click");
  assert.equal(w.document.activeElement, w.page);
});

// ---- config.yml ---------------------------------------------------------------

test("config.yml: the file's text is saved verbatim, not as the form's config", async () => {
  const yaml = "# keep me\ndefaults:\n  engine: claude # inline\n";
  const w = await X.open("raw", { "config.get": () => X.config((c) => { c.yaml = yaml; }) });
  const area = w.byId("raw-yaml");
  assert.equal(area.value, yaml);
  assert.equal(area.getAttribute("aria-label"), "config.yml");
  assert.equal(w.page.querySelector("code.path").textContent, "/Users/test/.config/prx/config.yml");
  assert.equal(w.page.querySelector(".banner.warn"), null);
  area.value = yaml + "repos:\n  - name: new/repo\n";
  // ⌘S on this page saves the text.
  w.fire(w.document.body, "keydown", { key: "s", metaKey: true });
  await w.settle();
  assert.deepEqual(w.calls("config.put"), [{ yaml: yaml + "repos:\n  - name: new/repo\n" }]);
  assert.deepEqual(status(w), ["status ok", "Saved and applied"]);
});

test("config.yml: a rejected file keeps its text and shows the error under it", async () => {
  const w = await X.open("raw", { "config.put": () => { throw bridgeError("rejected", "line 3: mapping values are not allowed here " + X.EVIL, "repos[0].name"); } });
  const area = w.byId("raw-yaml");
  area.value = "defaults: [oops\n";
  w.fire(w.byId("raw-save"), "click");
  await w.settle();
  assert.equal(w.window.prxApp.state.page, "raw", "a raw rejection is not routed to a form page");
  assert.equal(w.byId("raw-yaml"), area);
  assert.equal(area.value, "defaults: [oops\n");
  const error = w.page.querySelector(".field-error");
  assert.equal(error.hidden, false);
  assert.equal(error.textContent, "line 3: mapping values are not allowed here " + X.EVIL);
  assert.deepEqual(X.markupIn(w.page), []);
  assert.match(w.byId("save-status").textContent, /^Not saved: line 3/);
  // Focus on the window does not reload over the text being fixed.
  w.fireWindow("focus");
  await w.settle();
  assert.equal(w.byId("raw-yaml").value, "defaults: [oops\n");
});

test("config.yml: unsaved form edits are called out, and Reload from disk drops them", async () => {
  const w = await X.open("general");
  X.type(w, poll(w), "5");
  w.window.prxApp.go("raw");
  assert.equal(w.page.querySelector(".banner.warn").textContent,
    "You have unsaved changes on other pages. This shows the file as last saved, and saving here discards them.");
  // The form's Save still saves the form, not the file text.
  w.fire(button(w.page, "Reload from disk"), "click");
  await w.settle();
  assert.equal(w.page.querySelector(".banner.warn"), null);
  assert.equal(w.window.prxApp.app.isDirty(), false);
  assert.equal(w.window.prxApp.app.draft().defaults.poll_interval_minutes, 10);
});

// ---- discovery ------------------------------------------------------------------

const DISCOVERED = { agents: [{ name: "pi", label: "Pi", command: "pi", args: ["-p", "{{PROMPT}}"], model_args: [], models: [], builtin: true, path: "/opt/homebrew/bin/pi", on_path: true }],
  configured: {}, llm_apis: [] };

test("agent discovery answering late refreshes the engine pages, but not under a field being edited", async () => {
  const handlers = { "discover.agents": () => DISCOVERED };
  const hold = { hold: (m) => m.method === "discover.agents" };
  const w = await X.open("review", handlers, hold);
  // Until it answers, the built-in names stand in.
  assert.deepEqual(X.optionLabels(w, X.field(w, "defaults.engine")), ["pi", "claude", "codex", "cheap"]);
  w.release();
  await w.settle();
  // (`claude` is the configured engine, kept on offer though it was not found.)
  assert.deepEqual(X.optionLabels(w, X.field(w, "defaults.engine")), ["Pi", "cheap", "claude"]);
  assert.equal(w.window.prxApp.app.isDirty(), false);
  // Mid-edit: the page is left alone, and the next visit shows the result.
  const v = await X.open("review", handlers, hold);
  const model = X.field(v, "defaults.agent_model");
  model.focus();
  model.value = "half-typed";
  v.release();
  await v.settle();
  assert.ok(X.field(v, "defaults.agent_model") === model, "the focused field was not replaced");
  assert.equal(model.value, "half-typed");
  v.window.prxApp.go("engines");
  assert.equal(v.page.querySelector(".agent-name").textContent, "Pi");
  // Discovery failing is not an error the user sees.
  const off = await X.open("review", { "discover.agents": X.failing("scan failed") });
  assert.deepEqual(status(off), ["status", ""]);
  assert.deepEqual(X.optionLabels(off, X.field(off, "defaults.engine")), ["pi", "claude", "codex", "cheap"]);
});

test("installed browsers are listed once per window, with the app paths the config names", async () => {
  const daemon = X.storing((c) => {
    c.raw.defaults.browser = "/Applications/Arc.app";
    c.raw.orgs[0].browser = "Safari";
    c.raw.repos[0].browser = "/Applications/Firefox.app";
    c.raw.repos[1].browser = ["open", "-a", "Thing", "{{URL}}"];
    c.raw.repos[2].browser = "/Applications/Arc.app";
  });
  const w = await X.open("general", daemon.handlers);
  // Paths only: names resolve by themselves, commands are not apps.
  assert.deepEqual(w.calls("browsers.list"), [{ paths: ["/Applications/Arc.app", "/Applications/Firefox.app"] }]);
  X.type(w, poll(w), "5");
  await X.save(w);
  w.fireWindow("focus");
  await w.settle();
  assert.equal(w.calls("browsers.list").length, 1);
  // Agent discovery, by contrast, is repeated after a save.
  assert.equal(w.calls("discover.agents").length, 2);
});
