// The window's dropdown (form.js select) and the controls built on it, run
// against a tiny fake DOM: ARIA combobox + listbox semantics, keyboard
// navigation (arrows, Home/End, PageUp/PageDown, type-ahead, Enter, Space,
// Esc, Tab), selection, the "Default" and "Inherited" displays, and that
// every schema dropdown commits exactly the values the native <select>s
// did. Also the review-mode cards, the mode scope and pause helpers.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

class FakeElement {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.attrs = {};
    this.listeners = {};
    this.textContent = "";
    this.hidden = false;
    this.value = "";
    this.checked = false;
  }
  setAttribute(k, v) { this.attrs[k] = String(v); }
  getAttribute(k) { return this.attrs[k]; }
  removeAttribute(k) { delete this.attrs[k]; }
  hasAttribute(k) { return Object.prototype.hasOwnProperty.call(this.attrs, k); }
  append(...c) { this.children.push(...c); }
  replaceChildren(...c) { this.children = c; }
  addEventListener(t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); }
  dispatchEvent(e) { (this.listeners[e.type] || []).forEach((f) => f(e)); }
  focus() { focused = this; }
  get id() { return this.attrs.id; }
}
let focused = null;

const S = require("../settings-ui/schema.js");
const F = require("../settings-ui/fields.js");
const O = require("../settings-ui/overview.js");
const Cron = require("../settings-ui/cron.js");

function loadForm() {
  const document = { createElement: (tag) => new FakeElement(tag),
    createTextNode: (t) => Object.assign(new FakeElement("#text"), { textContent: String(t) }) };
  const window = { prxFields: F, prxCron: Cron, prxSchema: S, prxIcon: (name) => Object.assign(new FakeElement("svg"), { icon: name }) };
  class Event { constructor(type) { this.type = type; } }
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, "../settings-ui/form.js"), "utf8"),
    { window, document, Event, console, Object, Array, String, Math, Date, Number, Intl });
  return window.prxForm;
}
const UI = loadForm();

function walk(node, out) {
  out = out || [];
  if (!node || typeof node !== "object") return out;
  out.push(node);
  (node.children || []).forEach((c) => walk(c, out));
  return out;
}
const byClass = (node, cls) => walk(node).filter((n) => (n.attrs.class || "").split(" ").includes(cls));
const text = (node, cls) => (byClass(node, cls)[0] || {}).textContent;

// Drive one dropdown: its trigger, list, options, and keys.
function drive(ctl) {
  const trigger = walk(ctl.node).find((n) => n.attrs.role === "combobox");
  const list = walk(ctl.node).find((n) => n.attrs.role === "listbox");
  let t = 0;
  const key = (k, extra) => trigger.dispatchEvent(Object.assign({ type: "keydown", key: k, preventDefault() {}, timeStamp: (t += 1000) }, extra || {}));
  const quick = (k) => trigger.dispatchEvent({ type: "keydown", key: k, preventDefault() {}, timeStamp: (t += 100) });
  const options = () => list.children;
  const active = () => options().find((o) => o.attrs.id === trigger.attrs["aria-activedescendant"]);
  const label = (n) => text(n, "select-label");
  const shown = () => label(trigger);
  const tag = (n) => text(n || trigger, "select-tag");
  const click = (i) => { trigger.dispatchEvent({ type: "click" }); options()[i].dispatchEvent({ type: "click" }); };
  return { trigger, list, key, quick, options, active, label, shown, tag, click };
}

const FRUIT = [
  { value: "apple", label: "Apple", detail: "Crisp", icon: "check" },
  { value: "apricot", label: "Apricot" },
  { value: "banana", label: "Banana", disabled: true },
  { value: "blueberry", label: "Blueberry" },
  { value: "cherry", label: "Cherry", divider: true },
];
function fruit(extra) {
  const changes = [];
  const ctl = UI.select(Object.assign({ id: "fruit", label: "Fruit", options: FRUIT, value: "apricot",
    onchange: (v) => { changes.push(v); } }, extra || {}));
  return Object.assign(drive(ctl), { ctl, changes });
}

test("select: combobox + listbox semantics and option content", () => {
  const d = fruit();
  assert.equal(d.trigger.tagName, "BUTTON");
  assert.equal(d.trigger.attrs.role, "combobox");
  assert.equal(d.trigger.attrs["aria-haspopup"], "listbox");
  assert.equal(d.trigger.attrs["aria-expanded"], "false");
  assert.equal(d.trigger.attrs["aria-controls"], d.list.attrs.id);
  assert.equal(d.trigger.attrs["aria-label"], "Fruit");
  assert.equal(d.list.hidden, true);
  assert.equal(d.ctl.focus, d.trigger);
  assert.deepEqual(d.options().map(d.label), ["Apple", "Apricot", "Banana", "Blueberry", "Cherry"]);
  assert.ok(d.options().every((o) => o.attrs.role === "option"));
  assert.equal(d.shown(), "Apricot");
  assert.deepEqual(d.options().map((o) => o.attrs["aria-selected"]), ["false", "true", "false", "false", "false"]);
  assert.equal(text(d.options()[0], "select-detail"), "Crisp", "secondary description");
  assert.equal(byClass(d.options()[0], "select-icon")[0].children[0].icon, "check", "option icon");
  assert.equal(byClass(d.options()[1], "select-icon")[0].attrs.class, "select-icon blank", "icon slot keeps labels aligned");
  assert.equal(d.options()[2].attrs["aria-disabled"], "true");
  assert.match(d.options()[4].attrs.class, /divider/);
});

test("select: arrows skip disabled options and clamp; Home/End/Page keys jump", () => {
  const d = fruit();
  d.key("ArrowDown");
  assert.equal(d.list.hidden, false);
  assert.equal(d.trigger.attrs["aria-expanded"], "true");
  assert.equal(d.label(d.active()), "Apricot", "opens on the selection");
  assert.match(d.active().attrs.class, /\bactive\b/);
  d.key("ArrowDown");
  assert.equal(d.label(d.active()), "Blueberry", "Banana is disabled");
  d.key("ArrowDown");
  d.key("ArrowDown");
  assert.equal(d.label(d.active()), "Cherry", "clamped at the end");
  d.key("ArrowUp");
  d.key("ArrowUp");
  assert.equal(d.label(d.active()), "Apricot");
  d.key("End");
  assert.equal(d.label(d.active()), "Cherry");
  d.key("Home");
  assert.equal(d.label(d.active()), "Apple");
  d.key("PageDown");
  assert.equal(d.label(d.active()), "Cherry");
  d.key("PageUp");
  assert.equal(d.label(d.active()), "Apple");
  assert.deepEqual(d.changes, [], "moving changes nothing");
});

test("select: Enter and Space pick; Esc and Tab close unchanged", () => {
  const d = fruit();
  d.key("Enter");
  d.key("ArrowDown");
  d.key("Enter");
  assert.deepEqual(d.changes, ["blueberry"]);
  assert.equal(d.list.hidden, true);
  assert.equal(focused, d.trigger, "focus stays on the trigger");
  assert.equal(d.shown(), "Blueberry");
  d.key(" ");
  d.key("ArrowUp");
  d.key(" ");
  assert.deepEqual(d.changes, ["blueberry", "apricot"]);
  d.key("ArrowDown");
  d.key("ArrowDown");
  d.key("Escape");
  assert.equal(d.list.hidden, true);
  assert.equal(d.trigger.attrs["aria-expanded"], "false");
  d.key("ArrowDown");
  d.key("ArrowDown");
  d.key("Tab");
  assert.equal(d.list.hidden, true);
  assert.deepEqual(d.changes, ["blueberry", "apricot"], "Esc and Tab pick nothing");
  assert.equal(d.shown(), "Apricot");
});

test("select: type-ahead, opened by a letter, prefixes within 0.7 s", () => {
  const d = fruit();
  d.key("c");
  assert.equal(d.list.hidden, false, "a letter opens the list");
  assert.equal(d.label(d.active()), "Cherry");
  d.key("a");
  assert.equal(d.label(d.active()), "Apple", "wraps around");
  d.key("a");
  assert.equal(d.label(d.active()), "Apricot", "the same letter again moves to the next match");
  d.key("b");
  assert.equal(d.label(d.active()), "Blueberry", "disabled Banana is skipped");
  d.key("Home");
  d.quick("a");
  d.quick("p");
  d.quick("r");
  assert.equal(d.label(d.active()), "Apricot", "a typed prefix");
  d.key("Enter");
  assert.deepEqual(d.changes, [], "already selected: nothing to commit");
});

test("select: clicks, re-picks, a disabled option, and onchange returning false", () => {
  const d = fruit();
  d.click(0);
  assert.deepEqual(d.changes, ["apple"]);
  d.click(0);
  assert.deepEqual(d.changes, ["apple"], "re-picking the selection commits nothing");
  d.click(2);
  assert.deepEqual(d.changes, ["apple"], "a disabled option cannot be picked");
  const veto = fruit({ onchange: () => false });
  veto.click(4);
  assert.equal(veto.shown(), "Apricot", "a vetoed pick keeps the old selection");
  assert.equal(veto.options()[1].attrs["aria-selected"], "true");
});

test("select: Default tag for an unset field, cleared by a pick; set() and setOptions()", () => {
  const d = fruit({ value: "apple", defaultValue: "apple", unset: true });
  assert.equal(d.shown(), "Apple");
  assert.equal(d.tag(), "Default", "the trigger says the value is the default");
  assert.equal(d.tag(d.options()[0]), "Default", "and so does its option");
  d.click(1);
  d.click(0);
  assert.equal(d.tag(), undefined, "an explicit pick is no longer 'the default'");
  assert.equal(d.tag(d.options()[0]), "Default");
  d.ctl.set("cherry");
  assert.equal(d.shown(), "Cherry");
  assert.equal(d.ctl.value(), "cherry");
  d.ctl.setOptions([{ value: "x", label: "Xigua" }], "x");
  assert.deepEqual(d.options().map(d.label), ["Xigua"]);
  assert.equal(d.shown(), "Xigua");
});

// ---- schema dropdowns: value round-trip and Inherit ----------------------

function field(key) { return S.FIELDS.find((f) => f.key === key); }
function app(extra) {
  return Object.assign({
    engines: () => F.engineCatalog(null, { haiku: { type: "llm_api", provider: "anthropic", model: "claude-haiku-4-5", api_key_env: "K" } }, S.BUILTIN_ENGINES),
    localError: () => {}, changed: () => {}, shipped: () => "",
  }, extra || {});
}
function control(key, value, opts) {
  const commits = [];
  const ctl = UI.control(app(), "repos[acme/api]." + key, field(key), value, (v) => commits.push(v), opts || {});
  return Object.assign(drive(ctl), { ctl, commits });
}
const pick = (d, label) => { d.trigger.dispatchEvent({ type: "click" }); d.options().find((o) => d.label(o) === label).dispatchEvent({ type: "click" }); };

test("choice dropdown commits the same values as before", () => {
  const d = control("auto_open", "off", { compact: true });
  assert.deepEqual(d.options().map(d.label), ["Never", "Ready to approve", "Blocking", "Both"]);
  assert.equal(text(d.options()[2], "select-detail"), "Open the PR when a review requests changes.");
  for (const o of field("auto_open").options) pick(d, o.label);
  assert.deepEqual(d.commits, ["approved", "blocking", "both"], "Never was already selected");
  pick(d, "Never");
  assert.deepEqual(d.commits.at(-1), "off");
});

test("review mode dropdown (override editor): icons, summaries, Inherit", () => {
  const parent = { value: "monitor", source: "org", org: "acme" };
  const d = control("review_mode", "monitor", { compact: true, inherit: true, inherited: true, parent });
  assert.deepEqual(d.options().map(d.label), ["Inherit", "Review and comment", "Monitor", "Off"]);
  assert.equal(d.shown(), "Monitor", "shows the inherited mode…");
  assert.equal(d.tag(), "Inherited", "…marked as inherited");
  assert.equal(text(d.options()[0], "select-detail"), "Monitor, from the acme org");
  assert.equal(text(d.options()[2], "select-detail"), "Reviews PRs but never writes to GitHub.");
  assert.equal(byClass(d.options()[3], "select-icon")[0].children[0].icon, "stop");
  pick(d, "Off");
  pick(d, "Inherit");
  pick(d, "Review and comment");
  assert.deepEqual(d.commits, ["off", null, "review comment"], "Inherit unsets the override");
  // With an own value, the Inherit option still says what it stands for.
  const own = control("review_mode", "off", { compact: true, inherit: true, inherited: false, parent: { value: "review comment", source: "defaults" } });
  assert.equal(own.shown(), "Off");
  assert.equal(own.tag(), undefined);
  assert.equal(text(own.options()[0], "select-detail"), "Review and comment, from the defaults");
});

test("engine dropdown: agents and LLM APIs with descriptions; optional and inherited values", () => {
  const d = control("engine", "claude", {});
  const labels = d.options().map(d.label);
  assert.ok(["pi", "claude", "codex", "haiku"].every((n) => labels.includes(n)));
  const haiku = d.options().find((o) => d.label(o) === "haiku");
  assert.equal(text(haiku, "select-detail"), "LLM API · Anthropic · claude-haiku-4-5");
  pick(d, "haiku");
  assert.deepEqual(d.commits, ["haiku"]);
  // Optional (brief engine): "Same as the review engine" is unset.
  const brief = control("brief.engine", null, {});
  assert.equal(brief.shown(), "Same as the review engine");
  pick(brief, "codex");
  pick(brief, "Same as the review engine");
  assert.deepEqual(brief.commits, ["codex", null]);
  // A name nobody configured is kept, flagged.
  const odd = control("engine", "ghost", {});
  assert.equal(odd.shown(), "ghost");
  assert.equal(odd.tag(), "Not configured");
  // Override editor: Inherit stands for the parent's choice.
  const inh = control("brief.engine", null, { compact: true, inherit: true, inherited: true, parent: { value: null, source: "defaults" } });
  assert.equal(inh.shown(), "Same as the review engine");
  assert.equal(inh.tag(), "Inherited");
  assert.ok(!inh.options().some((o) => inh.label(o) === "Same as the review engine"), "unset is Inherit here");
  pick(inh, "pi");
  pick(inh, "Inherit");
  assert.deepEqual(inh.commits, ["pi", null]);
});

test("schedule dropdown (override editor) commits preset cron, Inherit as null", () => {
  const d = control("schedule", "0 9 * * 1-5", { compact: true, inherit: true, inherited: false, parent: { value: null, source: "defaults" } });
  assert.equal(d.shown(), "Weekday mornings");
  assert.equal(text(d.options()[0], "select-detail"), "With every poll, from the defaults");
  assert.ok(!d.options().some((o) => d.label(o) === "With every poll"), "no schedule = Inherit on an override row");
  pick(d, "Every hour");
  pick(d, "Inherit");
  assert.deepEqual(d.commits, ["0 * * * *", null]);
  const inherited = control("schedule", "0 9 * * 1", { compact: true, inherit: true, inherited: true, parent: { value: "0 9 * * 1", source: "org", org: "acme" } });
  assert.equal(inherited.shown(), "Weekly");
  assert.equal(inherited.tag(), "Inherited");
});

// ---- review mode cards -----------------------------------------------------

test("review mode cards: three radios saying what runs, what is posted, where verdicts show", () => {
  const commits = [];
  const ctl = UI.control(app(), "defaults.review_mode", field("review_mode"), "review comment", (v) => commits.push(v), { unset: true });
  assert.equal(ctl.group.attrs.role, "radiogroup");
  const radios = walk(ctl.node).filter((n) => n.attrs.type === "radio");
  assert.deepEqual(radios.map((r) => r.attrs.value), ["review comment", "monitor", "off"]);
  assert.equal(ctl.focus, radios[0], "focus lands on the selected mode");
  assert.ok(radios[0].checked && !radios[1].checked && !radios[2].checked);
  const cards = byClass(ctl.node, "mode-card");
  assert.deepEqual(cards.map((c) => text(c, "mode-title")), ["Review and comment", "Monitor", "Off"]);
  assert.equal(text(cards[0], "select-tag"), "Default", "unset: the default is marked");
  for (const card of cards) {
    const facts = byClass(card, "mode-fact");
    assert.equal(facts.length, 3, "reviews, GitHub, verdicts");
    const radio = walk(card).find((n) => n.attrs.type === "radio");
    assert.equal(radio.attrs["aria-describedby"], byClass(card, "mode-facts")[0].attrs.id);
  }
  const facts = (i) => byClass(cards[i], "mode-fact").map((f) => f.attrs.class.split(" ")[1] + ": " + f.children[1].textContent);
  assert.match(facts(0)[1], /^yes: Posts a comment or requests changes/);
  assert.match(facts(1)[0], /^yes: Reviews run/);
  assert.match(facts(1)[1], /^no: Nothing is posted to GitHub/);
  assert.match(facts(1)[2], /menu and History/);
  assert.match(facts(2)[0], /^no: No reviews/);
  radios[1].checked = true;
  radios[1].dispatchEvent({ type: "change" });
  assert.deepEqual(commits, ["monitor"]);
  // Set explicitly: no Default tag.
  const set = UI.control(app(), "defaults.review_mode", field("review_mode"), "monitor", () => {}, { unset: false });
  assert.equal(byClass(set.node, "select-tag").length, 0);
});

test("review mode copy never claims PRX approves", () => {
  const all = S.MODE_OPTIONS.map((o) => [o.detail].concat(o.facts.map((f) => f[1])).join(" ")).join(" ");
  assert.ok(!/\bapproves?\b/i.test(all));
  assert.deepEqual(S.MODE_OPTIONS.map((o) => o.value), S.MODES);
});

// ---- helpers ------------------------------------------------------------

test("resolveParent / ownSettings: what Inherit stands for and who overrides", () => {
  const draft = {
    defaults: { review_mode: "review comment" },
    orgs: [{ org: "acme", review_mode: "monitor" }],
    repos: [{ name: "acme/api", review_mode: "off" }, { name: "acme/web" }, { name: "solo/one" }],
  };
  assert.deepEqual(F.resolveParent(draft, "repo", "acme/api", "review_mode"), { value: "monitor", source: "org", org: "acme" });
  assert.deepEqual(F.resolveParent(draft, "repo", "solo/one", "review_mode"), { value: "review comment", source: "defaults", org: null });
  assert.deepEqual(F.resolveParent(draft, "org", "acme", "review_mode"), { value: "review comment", source: "defaults", org: null });
  assert.equal(F.inheritSource({ source: "org", org: "acme" }), "the acme org");
  assert.equal(F.inheritSource({ source: "defaults" }), "the defaults");
  assert.deepEqual(F.ownSettings(draft, "review_mode"), [
    { kind: "org", name: "acme", value: "monitor" }, { kind: "repo", name: "acme/api", value: "off" },
  ]);
});

test("pause state: countdown text, and a past end is not a pause", () => {
  const now = Date.parse("2026-10-10T12:00:00Z");
  const at = (ms) => ({ paused_until: new Date(now + ms).toISOString() });
  assert.deepEqual(O.pauseState({}, now), { paused: false, until: null, left: "" });
  assert.equal(O.pauseState(at(-1000), now).paused, false);
  assert.equal(O.pauseState(at(30e3), now).left, "<1m left");
  assert.equal(O.pauseState(at(45 * 60e3), now).left, "45m left");
  assert.equal(O.pauseState(at(72 * 60e3), now).left, "1h 12m left");
  assert.equal(O.pauseState(at(120 * 60e3), now).left, "2h left");
  assert.equal(O.pauseState(at(60e3), now).until, new Date(now + 60e3).toISOString());
});

// ---- pause controls (General's Pause section, the Dashboard) ------------

// A bridge that records calls and answers from `replies` (method → fn).
function fakeBridge(replies) {
  const calls = [];
  return { calls, call: (method, params) => { calls.push([method, params]); return replies[method](params); } };
}
const allText = (node) => walk(node).map((n) => n.textContent).join("");
const flush = () => new Promise((r) => setImmediate(r));

test("pause choices match the menu bar's durations", () => {
  const model = fs.readFileSync(path.join(__dirname, "../src/model.rs"), "utf8");
  const menu = [...model.matchAll(/PAUSE_\w+_ID, "(For [^"]+)"/g)].map((m) => m[1]);
  assert.deepEqual(O.PAUSE_CHOICES.map((c) => c.label), menu);
  assert.deepEqual(O.PAUSE_CHOICES.map((c) => c.value), [0.5, 1, 2, 4]);
});

test("pause panel: not paused offers the durations; a pick posts pause.start and refreshes", async () => {
  let refreshed = 0;
  const bridge = fakeBridge({ "pause.start": (p) => Promise.resolve({ ok: true, changed: true, paused_until: "x", hours: p.hours }) });
  const c = O.pauseControls(UI, bridge, () => { refreshed++; });
  const panel = O.pausePanel(UI, { paused: false, until: null, left: "" }, c);
  assert.ok(walk(panel).includes(c.start), "the dropdown is shown");
  assert.ok(!walk(panel).includes(c.resume), "no Resume when not paused");
  assert.ok(!/prx pause --hours/.test(allText(panel)), "no pointer to the CLI when the window can pause");
  const d = drive(c.picker);
  assert.equal(d.shown(), "Pause for\u2026");
  assert.deepEqual(d.options().map(d.label), ["For 30 minutes", "For 1 hour", "For 2 hours", "For 4 hours"]);
  d.click(0);
  assert.deepEqual(bridge.calls, [["pause.start", { hours: 0.5 }]]);
  assert.equal(c.busy(), true);
  assert.equal(d.trigger.disabled, true, "no second pick while one is in flight");
  assert.equal(d.shown(), "Pause for\u2026", "an action, not a kept selection");
  await flush();
  assert.equal(refreshed, 1);
  assert.equal(c.busy(), false);
  assert.equal(d.trigger.disabled, false);
  assert.equal(c.error.hidden, true);
});

test("pause panel: paused keeps the countdown and Resume, and can restart the pause", async () => {
  const bridge = fakeBridge({ "pause.resume": () => Promise.resolve({ paused_until: null }), "pause.start": () => Promise.resolve({}) });
  const c = O.pauseControls(UI, bridge, () => {});
  const panel = O.pausePanel(UI, { paused: true, until: "2026-10-10T13:00:00Z", left: "45m left" }, c, "Pause");
  assert.equal(text(panel, "pause-title"), "Paused, 45m left");
  assert.ok(walk(panel).includes(c.resume));
  assert.ok(walk(panel).includes(c.start));
  assert.match(allText(panel), /restart the pause from now/);
  c.resume.dispatchEvent({ type: "click" });
  assert.deepEqual(bridge.calls, [["pause.resume", {}]]);
  assert.equal(c.resume.disabled, true);
  await flush();
  assert.equal(c.resume.disabled, false);
  drive(c.picker).click(3);
  assert.deepEqual(bridge.calls[1], ["pause.start", { hours: 4 }]);
});

test("pause panel: daemon offline hides the choices; failures show inline", async () => {
  const offline = Object.assign(new Error("prx daemon is not running"), { kind: "offline" });
  const refused = Object.assign(new Error("pause end must be in the future"), { kind: "daemon" });
  let next = offline;
  const bridge = fakeBridge({ "pause.start": () => Promise.reject(next), "pause.resume": () => Promise.reject(next) });
  let refreshed = 0;
  const c = O.pauseControls(UI, bridge, () => { refreshed++; });
  const unknown = O.pausePanel(UI, null, c);
  assert.equal(text(unknown, "pause-title"), "Pause unknown");
  assert.ok(!walk(unknown).includes(c.start), "nothing to pick while the daemon is offline");
  assert.ok(!walk(unknown).includes(c.resume));
  drive(c.picker).click(1);
  await flush();
  assert.equal(refreshed, 0);
  assert.equal(c.error.hidden, false);
  assert.equal(c.error.attrs.role, "alert");
  assert.match(allText(c.error), /isn't running, so nothing changed/);
  assert.ok(walk(c.error).some((n) => n.tagName === "CODE" && n.textContent === "prx daemon start"));
  assert.ok(walk(O.pausePanel(UI, null, c)).includes(c.error), "the error survives a redraw");
  next = refused;
  c.resume.dispatchEvent({ type: "click" });
  assert.equal(c.error.hidden, true, "a new attempt clears the old error");
  await flush();
  assert.equal(allText(c.error), "pause end must be in the future");
  assert.equal(c.resume.disabled, false);
});
