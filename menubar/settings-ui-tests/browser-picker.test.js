// The "Open PRs with" picker (spec browser-picker, design W2), built on the
// window's dropdown (form.js select), run against a tiny fake DOM: an ARIA
// combobox + listbox with keyboard navigation (arrows, Home/End,
// type-ahead, Enter, Esc), selection commits, "Other app…",
// "Custom command…", Inherit, and the Test button's inline error.
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

function loadForm() {
  const document = { createElement: (tag) => new FakeElement(tag) };
  const window = {
    prxFields: require("../settings-ui/fields.js"),
    prxCron: {},
    prxIcon: () => new FakeElement("svg"),
  };
  class Event { constructor(type) { this.type = type; } }
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, "../settings-ui/form.js"), "utf8"),
    { window, document, Event, console, Object, Array, String, Math, Date });
  return window.prxForm;
}

function walk(node, out) {
  out = out || [];
  if (!node || typeof node !== "object") return out;
  out.push(node);
  (node.children || []).forEach((c) => walk(c, out));
  return out;
}

const FIELD = { key: "browser", label: "Open PRs with", type: "browser" };
function entry(name, p, extra) {
  return Object.assign({ name: name, bundle_id: "id." + name.toLowerCase(), path: p, icon_png_base64: "PNG" + name, is_default: false }, extra || {});
}
const CATALOG = {
  browsers: [
    entry("Arc", "/Applications/Arc.app", { is_default: true }),
    entry("Firefox", "/Applications/Firefox.app"),
    entry("Safari", "/Applications/Safari.app"),
  ],
  extra: [],
};

function render(value, opts, appOverrides) {
  const form = loadForm();
  const calls = { commits: [], tests: [], errors: {} };
  const app = Object.assign({
    browsers: () => CATALOG,
    chooseBrowserApp: () => Promise.resolve(entry("Zen", "/Applications/Zen.app")),
    addBrowser: (e) => { calls.added = e; },
    testBrowser: (b) => { calls.tests.push(b); return Promise.resolve({}); },
    localError: (p, m) => { calls.errors[p] = m; },
  }, appOverrides || {});
  const ctl = form.control(app, "defaults.browser", FIELD, value, (v) => calls.commits.push(v), opts || {});
  const nodes = () => walk(ctl.node);
  const trigger = () => nodes().find((n) => n.attrs["aria-haspopup"] === "listbox");
  const list = () => nodes().find((n) => n.attrs.role === "listbox");
  const options = () => list().children;
  const labelOf = (n) => walk(n).filter((x) => x.attrs.class === "select-label").map((x) => x.textContent)[0];
  const tagOf = (n) => walk(n).filter((x) => x.attrs.class === "select-tag").map((x) => x.textContent)[0];
  const selected = () => options().find((o) => o.attrs["aria-selected"] === "true");
  const shown = () => labelOf(trigger());
  const key = (target, k) => target.dispatchEvent({ type: "keydown", key: k, preventDefault() {}, timeStamp: (key.t = (key.t || 0) + 100) });
  const active = () => {
    const id = trigger().attrs["aria-activedescendant"];
    return options().find((o) => o.attrs.id === id);
  };
  const button = (text) => nodes().find((n) => n.tagName === "BUTTON" && n.textContent === text);
  const textarea = () => nodes().find((n) => n.tagName === "TEXTAREA");
  return { ctl, calls, trigger, list, options, labelOf, tagOf, selected, shown, key, active, button, textarea };
}

const tick = () => new Promise((r) => setImmediate(r));

test("the picker is an ARIA combobox + listbox with every option and icons", () => {
  const p = render("system");
  const trigger = p.trigger();
  assert.equal(trigger.tagName, "BUTTON");
  assert.equal(trigger.attrs.role, "combobox");
  assert.equal(trigger.attrs["aria-expanded"], "false");
  assert.equal(trigger.attrs["aria-controls"], p.list().attrs.id);
  assert.equal(p.ctl.focus, trigger, "the row label targets the trigger");
  assert.equal(p.list().hidden, true);
  assert.deepEqual(p.options().map(p.labelOf), [
    "System default (Arc)", "Arc", "Firefox", "Safari", "Other app…", "Custom command…",
  ]);
  assert.ok(p.options().every((o) => o.attrs.role === "option"));
  assert.equal(p.shown(), "System default (Arc)");
  const firefoxIcon = walk(p.options()[2]).find((n) => n.tagName === "IMG");
  assert.equal(firefoxIcon.attrs.src, "data:image/png;base64,PNGFirefox");
  assert.equal(firefoxIcon.attrs.alt, "", "icons are decorative");
});

test("keyboard: open, move, type-ahead, choose and escape", () => {
  const p = render("system");
  p.key(p.trigger(), "ArrowDown");
  assert.equal(p.list().hidden, false);
  assert.equal(p.trigger().attrs["aria-expanded"], "true");
  assert.equal(p.labelOf(p.active()), "System default (Arc)", "starts on the selection");
  p.key(p.trigger(), "ArrowDown");
  p.key(p.trigger(), "ArrowDown");
  assert.equal(p.labelOf(p.active()), "Firefox");
  p.key(p.trigger(), "End");
  assert.equal(p.labelOf(p.active()), "Custom command…");
  p.key(p.trigger(), "ArrowDown");
  assert.equal(p.labelOf(p.active()), "Custom command…", "clamped at the end");
  p.key(p.trigger(), "Home");
  assert.equal(p.labelOf(p.active()), "System default (Arc)");
  p.key(p.trigger(), "s");
  assert.equal(p.labelOf(p.active()), "Safari", "type-ahead finds the next match");
  p.key(p.trigger(), "Escape");
  assert.equal(p.list().hidden, true);
  assert.equal(p.trigger().attrs["aria-activedescendant"], undefined);
  assert.equal(focused, p.trigger(), "focus stays on the trigger");
  assert.deepEqual(p.calls.commits, [], "Esc changes nothing");

  p.key(p.trigger(), "Enter");
  p.key(p.trigger(), "f");
  p.key(p.trigger(), "Enter");
  assert.deepEqual(p.calls.commits, ["Firefox"], "Firefox is saved as `browser: Firefox`");
  assert.equal(p.list().hidden, true);
  assert.equal(p.shown(), "Firefox");
  assert.equal(p.labelOf(p.selected()), "Firefox");
  assert.equal(p.selected().attrs["aria-selected"], "true");
});

test("clicking an option selects it; re-picking the selection commits nothing", () => {
  const p = render("Firefox");
  assert.equal(p.shown(), "Firefox");
  p.trigger().dispatchEvent({ type: "click" });
  p.options()[2].dispatchEvent({ type: "click" });
  assert.deepEqual(p.calls.commits, []);
  p.trigger().dispatchEvent({ type: "click" });
  p.options()[1].dispatchEvent({ type: "click" });
  assert.deepEqual(p.calls.commits, ["Arc"]);
  p.trigger().dispatchEvent({ type: "click" });
  p.options()[0].dispatchEvent({ type: "click" });
  assert.deepEqual(p.calls.commits, ["Arc", "system"]);
});

test("an unknown saved browser shows as (not found) and is never dropped", () => {
  const p = render("Brave");
  assert.equal(p.shown(), "Brave (not found)");
  assert.deepEqual(p.calls.commits, [], "rendering does not rewrite the value");
  assert.equal(p.tagOf(p.trigger()), "Not found");
  p.key(p.trigger(), "ArrowDown");
  p.key(p.trigger(), "Enter");
  assert.deepEqual(p.calls.commits, [], "re-picking it keeps the value");
});

test("Other app… saves the chosen app and shows it selected", async () => {
  const p = render("system");
  p.trigger().dispatchEvent({ type: "click" });
  p.options().find((o) => p.labelOf(o) === "Other app…").dispatchEvent({ type: "click" });
  await tick();
  assert.deepEqual(p.calls.commits, ["/Applications/Zen.app"]);
  assert.equal(p.calls.added.name, "Zen");
  assert.equal(p.shown(), "Zen");
  // Cancelled: nothing changes.
  const q = render("system", {}, { chooseBrowserApp: () => Promise.resolve(null) });
  q.trigger().dispatchEvent({ type: "click" });
  q.options().find((o) => q.labelOf(o) === "Other app…").dispatchEvent({ type: "click" });
  await tick();
  assert.deepEqual(q.calls.commits, []);
  assert.equal(q.shown(), "System default (Arc)");
});

test("Custom command… reveals the argv editor, which needs {{URL}} once", () => {
  const p = render("system");
  assert.equal(p.textarea().hidden, true);
  p.trigger().dispatchEvent({ type: "click" });
  p.options().find((o) => p.labelOf(o) === "Custom command…").dispatchEvent({ type: "click" });
  assert.equal(p.textarea().hidden, false);
  assert.deepEqual(p.calls.commits, [], "an empty editor commits nothing yet");
  p.textarea().value = "firefox\n--new-tab";
  p.textarea().dispatchEvent({ type: "change" });
  assert.match(p.calls.errors["defaults.browser"], /\{\{URL\}\} exactly once/);
  p.textarea().value = "firefox\n--new-tab\n{{URL}}";
  p.textarea().dispatchEvent({ type: "change" });
  assert.equal(p.calls.errors["defaults.browser"], null);
  assert.deepEqual(p.calls.commits.at(-1), ["firefox", "--new-tab", "{{URL}}"]);
  // An existing template opens in the editor.
  const q = render(["firefox", "{{URL}}"]);
  assert.equal(q.shown(), "Custom command…");
  assert.equal(q.textarea().value, "firefox\n{{URL}}");
});

test("org and repo rows offer Inherit", () => {
  const p = render("Firefox", { inherit: true, inherited: true, parent: { value: "Firefox", source: "org", org: "acme" } });
  assert.equal(p.shown(), "Firefox", "the trigger shows what is inherited…");
  assert.equal(p.tagOf(p.trigger()), "Inherited", "…marked as inherited");
  assert.equal(p.labelOf(p.options()[0]), "Inherit");
  assert.match(walk(p.options()[0]).find((x) => x.attrs.class === "select-detail").textContent, /Firefox, from the acme org/);
  p.trigger().dispatchEvent({ type: "click" });
  p.options().find((o) => p.labelOf(o) === "Safari").dispatchEvent({ type: "click" });
  assert.deepEqual(p.calls.commits, ["Safari"]);
  const own = render("Safari", { inherit: true, inherited: false, parent: { value: "system", source: "defaults" } });
  assert.equal(own.shown(), "Safari");
  assert.equal(own.tagOf(own.trigger()), undefined);
  assert.match(walk(own.options()[0]).find((x) => x.attrs.class === "select-detail").textContent, /System default \(Arc\), from the defaults/);
  own.trigger().dispatchEvent({ type: "click" });
  own.options()[0].dispatchEvent({ type: "click" });
  assert.deepEqual(own.calls.commits, [null], "Inherit unsets the override");
  // The defaults have no Inherit.
  assert.equal(render("system").options().some((o) => /^Inherit/.test(render("system").labelOf(o))), false);
});

test("Test opens the current choice and reports failures inline", async () => {
  const p = render("Firefox");
  p.button("Test").dispatchEvent({ type: "click" });
  await tick();
  assert.deepEqual(p.calls.tests, ["Firefox"]);
  const failing = render(["nosuchbrowser", "{{URL}}"], {}, {
    testBrowser: (b) => { failing.calls.tests.push(b); return Promise.reject(new Error("cannot run nosuchbrowser: No such file or directory")); },
  });
  failing.button("Test").dispatchEvent({ type: "click" });
  await tick();
  assert.deepEqual(failing.calls.tests, [["nosuchbrowser", "{{URL}}"]]);
  const error = walk(failing.ctl.node).find((n) => n.attrs.role === "alert");
  assert.equal(error.hidden, false);
  assert.match(error.textContent, /nosuchbrowser/);
  assert.deepEqual(failing.calls.commits, [], "a test saves nothing");
  // An inherited row tests the inherited browser.
  const inherited = render("Arc", { inherit: true, inherited: true });
  inherited.button("Test").dispatchEvent({ type: "click" });
  assert.deepEqual(inherited.calls.tests, ["Arc"]);
});
