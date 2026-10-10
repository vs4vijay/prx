// The app shell's navigation (design D16), run against a tiny fake DOM:
// grouped sidebar, opening on #page=, prxApp.go, the settings draft
// surviving trips to Overview pages, and polling only the visible page.
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
    this.dataset = {};
    this.listeners = {};
    this.className = "";
    this.textContent = "";
    this.disabled = false;
    this.hidden = false;
    this.scrollTop = 0;
  }
  get classList() {
    const self = this;
    const list = () => self.className.split(/\s+/).filter(Boolean);
    return {
      contains: (c) => list().includes(c),
      toggle: (c, on) => {
        const has = list().includes(c);
        if (on === undefined) on = !has;
        self.className = (on ? list().concat(has ? [] : [c]) : list().filter((x) => x !== c)).join(" ");
      },
      add: (c) => { if (!list().includes(c)) self.className = list().concat([c]).join(" "); },
      remove: (c) => { self.className = list().filter((x) => x !== c).join(" "); },
    };
  }
  setAttribute(k, v) {
    this.attrs[k] = String(v);
    if (k === "class") this.className = String(v);
    if (k.startsWith("data-")) this.dataset[k.slice(5)] = String(v);
  }
  getAttribute(k) { return this.attrs[k]; }
  removeAttribute(k) { delete this.attrs[k]; }
  append(...c) { this.children.push(...c); }
  replaceChildren(...c) { this.children = c; }
  addEventListener(t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); }
  fire(t) { (this.listeners[t] || []).forEach((f) => f({ preventDefault() {} })); }
  focus() {}
  blur() {}
  scrollIntoView() {}
}

function boot(hash) {
  const ids = {};
  ["page", "save-status", "save-note", "save", "revert", "daemon", "nav"].forEach((id) => { ids[id] = new FakeElement(id === "nav" ? "nav" : "div"); });
  const body = new FakeElement("body");
  const docListeners = {};
  const document = {
    body, activeElement: body, visibilityState: "visible",
    getElementById: (id) => ids[id],
    querySelectorAll: (sel) => (sel === "#nav button" ? ids.nav.children.filter((c) => c.tagName === "BUTTON") : []),
    createElement: (tag) => new FakeElement(tag),
    addEventListener: (t, f) => { (docListeners[t] = docListeners[t] || []).push(f); },
  };
  const winListeners = {};
  const rendered = [];
  const polls = { history: 0 };
  const config = { path: "/x/config.yml", yaml: "defaults: {}\n", raw: { defaults: { engine: "pi", poll_interval_minutes: 15 }, repos: [] } };
  const el = (tag, attrs, children) => {
    const node = new FakeElement(tag);
    Object.entries(attrs || {}).forEach(([k, v]) => {
      if (v === undefined || v === null || v === false) return;
      if (k === "text") node.textContent = v;
      else if (k.startsWith("on")) node.addEventListener(k.slice(2), v);
      else node.setAttribute(k, v);
    });
    (children || []).forEach((c) => { if (c) node.append(c); });
    return node;
  };
  const settingsPage = (id) => function (root, app) { rendered.push(id); root.append(el("p", { text: id + ":" + app.draft().defaults.poll_interval_minutes })); };
  const window = {
    location: { hash: hash || "" },
    addEventListener: (t, f) => { (winListeners[t] = winListeners[t] || []).push(f); },
    prxBridge: {
      getConfig: () => Promise.resolve(JSON.parse(JSON.stringify(config))),
      call: () => Promise.resolve({ agents: [], configured: {} }),
      putConfig: () => Promise.resolve({ ok: true }),
    },
    prxFields: require("../settings-ui/fields.js"),
    prxSchema: require("../settings-ui/schema.js"),
    prxForm: { el, icon: () => el("svg"), button: (t, f, c) => el("button", { text: t, onclick: f, class: c }), empty: (i, t) => el("div", { text: t }) },
    prxPageRenderers: {
      general: settingsPage("general"),
      review: settingsPage("review"),
      dashboard: function () { rendered.push("dashboard"); return null; },
      history: function () { rendered.push("history"); return { refresh: () => { polls.history += 1; } }; },
    },
  };
  let tick = null;
  const context = { window, document, console, setInterval: (f) => { tick = f; }, Promise, JSON, Object };
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, "../settings-ui/app.js"), "utf8"), context);
  return { window, document, ids, body, rendered, polls, tick: () => tick(), fireWindow: (t) => (winListeners[t] || []).forEach((f) => f()),
    fireDoc: (t) => (docListeners[t] || []).forEach((f) => f()) };
}

const settle = () => new Promise((resolve) => setImmediate(resolve));

test("the sidebar shows Overview and Settings groups", async () => {
  const w = boot("");
  await settle();
  const nav = w.ids.nav.children;
  const headers = nav.filter((c) => c.tagName === "P").map((c) => c.textContent);
  assert.deepEqual(headers, ["Overview", "Settings"]);
  const order = nav.map((c) => (c.tagName === "P" ? "#" + c.textContent : c.dataset.page));
  assert.deepEqual(order.slice(0, 6), ["#Overview", "dashboard", "prs", "history", "usage", "#Settings"]);
  // No hash: the settings window's old behavior, General.
  assert.equal(w.window.prxApp.state.page, "general");
});

test("the window opens on #page= and switches with prxApp.go / hashchange", async () => {
  const w = boot("#page=dashboard");
  assert.equal(w.window.prxApp.state.page, "dashboard");
  assert.equal(w.ids.page.dataset.page, "dashboard");
  await settle();
  // Loading the config doesn't re-mount an Overview page.
  assert.deepEqual(w.rendered, ["dashboard"]);
  assert.ok(w.body.classList.contains("overview"));
  w.window.prxApp.go("general");
  assert.equal(w.window.prxApp.state.page, "general");
  assert.ok(!w.body.classList.contains("overview"));
  w.window.location.hash = "#page=history";
  w.fireWindow("hashchange");
  assert.equal(w.window.prxApp.state.page, "history");
  const current = w.ids.nav.children.filter((c) => c.attrs["aria-current"] === "page").map((c) => c.dataset.page);
  assert.deepEqual(current, ["history"]);
});

test("unsaved settings survive a trip to an Overview page", async () => {
  const w = boot("#page=general");
  await settle();
  const app = w.window.prxApp.app;
  app.draft().defaults.poll_interval_minutes = 5;
  app.changed("defaults.poll_interval_minutes");
  assert.equal(w.ids.save.disabled, false);
  app.go("history");
  // The save bar stays dirty (and so visible) on Overview pages.
  assert.ok(w.body.classList.contains("dirty"));
  assert.ok(w.body.classList.contains("overview"));
  app.go("general");
  assert.equal(app.draft().defaults.poll_interval_minutes, 5);
  assert.equal(app.isDirty(), true);
  assert.equal(w.ids.save.disabled, false);
  assert.equal(w.ids["save-status"].textContent, "Unsaved changes");
  assert.equal(w.ids.page.children[0].textContent, "general:5");
  // A focus refresh never replaces a dirty draft.
  w.fireWindow("focus");
  await settle();
  assert.equal(app.draft().defaults.poll_interval_minutes, 5);
});

test("only the visible, current Overview page polls", async () => {
  const w = boot("#page=history");
  await settle();
  w.tick();
  assert.equal(w.polls.history, 1);
  w.document.visibilityState = "hidden";
  w.tick();
  assert.equal(w.polls.history, 1);
  w.document.visibilityState = "visible";
  w.fireDoc("visibilitychange");
  assert.equal(w.polls.history, 2);
  w.window.prxApp.go("general");
  w.tick();
  assert.equal(w.polls.history, 2);
  // Mount ids let async answers for a replaced page be dropped.
  const app = w.window.prxApp.app;
  const mount = app.mount();
  assert.equal(app.isMounted(mount), true);
  app.go("history");
  assert.equal(app.isMounted(mount), false);
  // Per-page view state survives re-mounts.
  app.view("history").filter = "x";
  app.go("general");
  assert.equal(app.view("history").filter, "x");
});
