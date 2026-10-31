// A small fake DOM and window for the node tests that run the settings UI's
// pages end to end (not a test file itself). `boot()` loads the scripts
// index.html lists, in its order, into one vm context whose `window.ipc` is a
// fake host: every bridge message is recorded and answered by the test's
// `daemon(method, params)`. Timers are collected instead of run, and `Date`
// is pinned to `now`, so nothing depends on the clock or the timezone.
//
// Only what the pages use is implemented: element trees, attributes that
// reflect (`id`, `class`, `hidden`, `disabled`), classList, dataset, simple
// selectors (tag, #id, .class, [attr], [attr=value], descendant and child
// combinators, comma lists), focus, and bubbling events. There is no HTML
// parser on purpose: `innerHTML` and friends throw, so a page that ever
// builds markup from text fails its test.
"use strict";
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

const UI_DIR = path.join(__dirname, "..", "settings-ui");
const NO_MARKUP = ["innerHTML", "outerHTML"];

// Links to other nodes are kept out of the enumerable properties, so a
// failed assertion prints one element, not the whole document.
function hide(node, props) {
  Object.keys(props).forEach((name) => Object.defineProperty(node, name, { value: props[name], writable: true, enumerable: false }));
}

class FakeText {
  constructor(text) {
    this.nodeType = 3;
    this.textContent = String(text);
    hide(this, { parentNode: null });
  }
}

class FakeElement {
  constructor(tag, ownerDocument, namespaced) {
    this.nodeType = 1;
    this.tagName = namespaced ? tag : tag.toUpperCase();
    this.attrs = {};
    this.text = "";
    this.value = "";
    this.checked = false;
    this.scrollTop = 0;
    this.offsetWidth = 0;
    this.scrollHeight = 0;
    const self = this;
    hide(this, {
      ownerDocument: ownerDocument, childNodes: [], parentNode: null, listeners: {}, style: {}, selection: null, valueAtFocus: undefined,
      dataset: new Proxy({}, {
        get(_, key) { return typeof key === "string" ? self.attrs["data-" + key] : undefined; },
        set(_, key, value) { self.attrs["data-" + key] = String(value); return true; },
      }),
    });
  }

  // ---- attributes ---------------------------------------------------------
  setAttribute(k, v) { this.attrs[k] = String(v); }
  getAttribute(k) { return this.hasAttribute(k) ? this.attrs[k] : null; }
  removeAttribute(k) { delete this.attrs[k]; }
  hasAttribute(k) { return Object.prototype.hasOwnProperty.call(this.attrs, k); }
  get id() { return this.attrs.id || ""; }
  set id(v) { this.attrs.id = String(v); }
  get className() { return this.attrs.class || ""; }
  set className(v) { this.attrs.class = String(v); }
  get placeholder() { return this.attrs.placeholder || ""; }
  set placeholder(v) { this.attrs.placeholder = String(v); }
  get hidden() { return this.hasAttribute("hidden"); }
  set hidden(v) { if (v) this.attrs.hidden = ""; else delete this.attrs.hidden; }
  get disabled() { return this.hasAttribute("disabled"); }
  set disabled(v) { if (v) this.attrs.disabled = ""; else delete this.attrs.disabled; }
  get classList() {
    const self = this;
    const list = () => self.className.split(/\s+/).filter(Boolean);
    const write = (items) => { self.className = items.join(" "); };
    return {
      contains: (c) => list().includes(c),
      add: (c) => { if (!list().includes(c)) write(list().concat([c])); },
      remove: (c) => write(list().filter((x) => x !== c)),
      toggle: (c, on) => {
        const has = list().includes(c);
        if (on === undefined) on = !has;
        write(on ? list().concat(has ? [] : [c]) : list().filter((x) => x !== c));
        return on;
      },
    };
  }

  // ---- tree ---------------------------------------------------------------
  get children() { return this.childNodes.filter((n) => n.nodeType === 1); }
  get firstChild() { return this.childNodes[0] || null; }
  get textContent() { return this.childNodes.length ? this.childNodes.map((n) => n.textContent).join("") : this.text; }
  set textContent(v) {
    this.childNodes.forEach((n) => { n.parentNode = null; });
    this.childNodes = [];
    this.text = v === null || v === undefined ? "" : String(v);
  }
  append(...nodes) {
    nodes.forEach((n) => {
      const node = typeof n === "string" ? new FakeText(n) : n;
      if (node.parentNode) node.parentNode.childNodes = node.parentNode.childNodes.filter((x) => x !== node);
      node.parentNode = this;
      this.text = "";
      this.childNodes.push(node);
    });
  }
  replaceChildren(...nodes) {
    this.textContent = "";
    this.append(...nodes);
  }
  remove() {
    if (!this.parentNode) return;
    this.parentNode.childNodes = this.parentNode.childNodes.filter((x) => x !== this);
    this.parentNode = null;
  }
  replaceWith(node) {
    const parent = this.parentNode;
    if (!parent) return;
    if (node.parentNode) node.remove();
    parent.childNodes[parent.childNodes.indexOf(this)] = node;
    node.parentNode = parent;
    this.parentNode = null;
  }
  contains(node) {
    for (let n = node; n; n = n.parentNode) if (n === this) return true;
    return false;
  }
  get isConnected() { return !!this.ownerDocument && this.ownerDocument.body.contains(this); }

  // ---- selectors ----------------------------------------------------------
  matches(selector) { return matchesAny(this, selector); }
  closest(selector) {
    for (let n = this; n && n.nodeType === 1; n = n.parentNode) if (matchesAny(n, selector)) return n;
    return null;
  }
  querySelectorAll(selector) { return descendants(this).filter((n) => matchesAny(n, selector)); }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }

  // ---- events and focus -----------------------------------------------------
  addEventListener(type, f) { (this.listeners[type] = this.listeners[type] || []).push(f); }
  removeEventListener(type, f) { this.listeners[type] = (this.listeners[type] || []).filter((x) => x !== f); }
  dispatchEvent(event) {
    if (!event.target) event.target = this;
    for (let n = this; n; n = n.parentNode) {
      (n.listeners[event.type] || []).slice().forEach((f) => f(event));
      if (!event.bubbles || event.stopped) return;
    }
    const doc = this.ownerDocument;
    if (doc && this.isConnected) (doc.listeners[event.type] || []).slice().forEach((f) => f(event));
  }
  focus() {
    const doc = this.ownerDocument;
    if (this.disabled || !doc || doc.activeElement === this) return;
    const previous = doc.activeElement;
    doc.activeElement = this;
    if (previous && previous !== doc.body) previous.lostFocus();
    this.valueAtFocus = this.value;
    this.dispatchEvent(makeEvent("focus", { bubbles: false }));
  }
  blur() {
    const doc = this.ownerDocument;
    if (!doc || doc.activeElement !== this) return;
    doc.activeElement = doc.body;
    this.lostFocus();
  }
  // Like a browser, a text field edited while focused fires `change` when
  // focus leaves it.
  lostFocus() {
    if ((this.tagName === "INPUT" || this.tagName === "TEXTAREA") && this.value !== this.valueAtFocus) {
      this.valueAtFocus = this.value;
      this.dispatchEvent(makeEvent("change"));
    }
    this.dispatchEvent(makeEvent("blur", { bubbles: false }));
  }
  click() { if (!this.disabled) this.dispatchEvent(makeEvent("click")); }
  scrollIntoView() {}

  // ---- text inputs ----------------------------------------------------------
  get selectionStart() { return this.selection === null ? this.value.length : this.selection[0]; }
  get selectionEnd() { return this.selection === null ? this.value.length : this.selection[1]; }
  setSelectionRange(start, end) { this.selection = [start, end]; }
  setRangeText(text, start, end) {
    this.value = this.value.slice(0, start) + text + this.value.slice(end);
    this.selection = [start + text.length, start + text.length];
  }
}
NO_MARKUP.forEach((name) => {
  Object.defineProperty(FakeElement.prototype, name, {
    get() { throw new Error(name + " read: the settings UI must build DOM nodes, never markup"); },
    set() { throw new Error(name + " set: the settings UI must build DOM nodes, never markup"); },
  });
});
FakeElement.prototype.insertAdjacentHTML = function () { throw new Error("insertAdjacentHTML: the settings UI must never build markup"); };

function makeEvent(type, props) {
  const event = Object.assign({ type: type, bubbles: true, defaultPrevented: false, stopped: false, key: "", timeStamp: 0 }, props || {});
  event.preventDefault = function () { event.defaultPrevented = true; };
  event.stopPropagation = function () { event.stopped = true; };
  return event;
}

function descendants(node, out) {
  out = out || [];
  node.children.forEach((c) => { out.push(c); descendants(c, out); });
  return out;
}

// "a > b c, d" -> [[{compound, combinator}, ...], ...], right-most last.
function parseSelector(selector) {
  const lists = [[]];
  let token = "", depth = 0, quote = null, combinator = " ";
  const flush = () => {
    if (token) lists[lists.length - 1].push({ compound: parseCompound(token), combinator: combinator });
    token = "";
  };
  for (let i = 0; i < selector.length; i++) {
    const ch = selector[i];
    if (ch === "\\") { token += ch + selector[++i]; continue; }
    if (quote) { token += ch; if (ch === quote) quote = null; continue; }
    if (ch === '"' || ch === "'") { quote = ch; token += ch; continue; }
    if (ch === "[") depth += 1;
    if (ch === "]") depth -= 1;
    if (depth === 0 && ch === ",") { flush(); lists.push([]); combinator = " "; continue; }
    if (depth === 0 && (ch === " " || ch === ">")) {
      if (token) { flush(); combinator = " "; }
      if (ch === ">") combinator = ">";
      continue;
    }
    token += ch;
  }
  flush();
  return lists.filter((l) => l.length);
}

function parseCompound(text) {
  const out = { tag: null, id: null, classes: [], attrs: [] };
  let i = 0;
  const name = () => {
    let s = "";
    while (i < text.length && /[\w*-]/.test(text[i])) s += text[i++];
    return s;
  };
  const tag = name();
  if (tag && tag !== "*") out.tag = tag.toLowerCase();
  while (i < text.length) {
    const ch = text[i++];
    if (ch === "#") out.id = name();
    else if (ch === ".") out.classes.push(name());
    else if (ch === "[") {
      const key = name();
      let value = null;
      if (text[i] === "=") {
        i += 1;
        const quote = text[i] === '"' || text[i] === "'" ? text[i++] : null;
        value = "";
        while (i < text.length && text[i] !== (quote || "]")) {
          if (text[i] === "\\") i += 1;
          value += text[i++];
        }
        if (quote) i += 1;
      }
      i += 1; // the closing bracket
      out.attrs.push([key, value]);
    } else throw new Error("fake-dom: unsupported selector " + text);
  }
  return out;
}

function matchesCompound(node, c) {
  if (!node || node.nodeType !== 1) return false;
  if (c.tag && node.tagName.toLowerCase() !== c.tag) return false;
  if (c.id !== null && node.id !== c.id) return false;
  if (!c.classes.every((cls) => node.classList.contains(cls))) return false;
  return c.attrs.every(([k, v]) => node.hasAttribute(k) && (v === null || node.getAttribute(k) === v));
}

function matchesChain(node, chain, at) {
  if (!matchesCompound(node, chain[at].compound)) return false;
  if (at === 0) return true;
  if (chain[at].combinator === ">") return matchesChain(node.parentNode, chain, at - 1);
  for (let p = node.parentNode; p; p = p.parentNode) if (matchesChain(p, chain, at - 1)) return true;
  return false;
}

function matchesAny(node, selector) {
  return parseSelector(selector).some((chain) => matchesChain(node, chain, chain.length - 1));
}

// A document holding index.html's skeleton (the ids the scripts look up).
function createDocument() {
  const doc = { listeners: {}, visibilityState: "visible" };
  const make = (tag, id, parent) => {
    const node = new FakeElement(tag, doc);
    if (id) node.id = id;
    if (parent) parent.append(node);
    return node;
  };
  doc.body = make("body");
  doc.activeElement = doc.body;
  const aside = make("aside", "sidebar", doc.body);
  make("button", "skip", doc.body);
  make("span", "brand-mark", aside);
  make("nav", "nav", aside);
  make("p", "daemon", aside);
  const main = make("div", "main", doc.body);
  make("main", "page", main);
  const bar = make("footer", "savebar", main);
  make("p", "save-status", bar);
  make("p", "save-note", bar).hidden = true;
  make("button", "revert", bar).disabled = true;
  make("button", "save", bar).disabled = true;
  doc.createElement = (tag) => new FakeElement(tag, doc);
  doc.createElementNS = (ns, tag) => Object.assign(new FakeElement(tag, doc, true), { namespaceURI: ns });
  doc.createTextNode = (text) => new FakeText(text);
  doc.getElementById = (id) => descendants(doc.body).find((n) => n.id === id) || null;
  doc.querySelectorAll = (selector) => doc.body.querySelectorAll(selector);
  doc.querySelector = (selector) => doc.body.querySelector(selector);
  doc.addEventListener = (type, f) => { (doc.listeners[type] = doc.listeners[type] || []).push(f); };
  doc.removeEventListener = (type, f) => { doc.listeners[type] = (doc.listeners[type] || []).filter((x) => x !== f); };
  return doc;
}

// The scripts index.html loads, in order.
function indexScripts() {
  const html = fs.readFileSync(path.join(UI_DIR, "index.html"), "utf8");
  return Array.from(html.matchAll(/<script src="([^"]+)"><\/script>/g), (m) => m[1]);
}

// Values created inside the vm context have that realm's prototypes; this
// makes them comparable with assert.deepEqual.
function plain(value) { return value === undefined ? undefined : JSON.parse(JSON.stringify(value)); }

// A bridge failure as the host reports it.
function bridgeError(kind, message, field) {
  return Object.assign(new Error(message), { kind: kind, field: field || null });
}

// Boot the window. opts: hash ("#page=…"), now (ms), scripts (default: all
// of index.html's), daemon(method, params) -> result, or throws a
// bridgeError. Returns the window, the document, `sent` (every bridge
// message, in order), `settle()` (lets pending replies land), `fire(node,
// type, props)`, and the collected timers.
function boot(opts) {
  opts = opts || {};
  const now = opts.now === undefined ? Date.UTC(2026, 9, 14, 12, 0, 0) : opts.now;
  const document = createDocument();
  const sent = [];
  const timers = [];
  const intervals = [];
  const win = { document: document, location: { hash: opts.hash || "" }, listeners: {}, opened: [] };
  win.window = win;
  class FixedDate extends Date {
    constructor(...args) { if (args.length) super(...args); else super(now); }
    static now() { return now; }
  }
  win.Date = FixedDate;
  win.Intl = Intl;
  win.console = console;
  win.Element = FakeElement;
  win.HTMLTextAreaElement = class { static [Symbol.hasInstance](node) { return !!node && node.tagName === "TEXTAREA"; } };
  win.Event = function (type, init) { return makeEvent(type, { bubbles: !!(init && init.bubbles) }); };
  win.CSS = { escape: (text) => String(text).replace(/[^\w-]/g, "\\$&") };
  win.setTimeout = (f) => { timers.push(f); return timers.length; };
  win.setInterval = (f, ms) => { intervals.push({ run: f, ms: ms, cleared: false }); return intervals.length; };
  win.clearInterval = (id) => { if (intervals[id - 1]) intervals[id - 1].cleared = true; };
  win.addEventListener = (type, f) => { (win.listeners[type] = win.listeners[type] || []).push(f); };
  win.open = (...args) => { win.opened.push(args); };
  win.ipc = {
    postMessage(text) {
      const message = JSON.parse(text);
      sent.push(message);
      let reply;
      try {
        const result = opts.daemon ? opts.daemon(message.method, message.params) : undefined;
        reply = { id: message.id, ok: true, result: result === undefined ? {} : JSON.parse(JSON.stringify(result)) };
      } catch (error) {
        if (!error.kind) throw error;
        reply = { id: message.id, ok: false, error: { kind: error.kind, message: error.message, field: error.field || null } };
      }
      if (opts.hold && opts.hold(message)) { held.push(reply); return; }
      Promise.resolve().then(() => win.prxBridge.__resolve(reply));
    },
  };
  const held = [];
  vm.createContext(win);
  (opts.scripts || indexScripts()).forEach((name) => {
    vm.runInContext(fs.readFileSync(path.join(UI_DIR, name), "utf8"), win, { filename: name });
  });
  const settle = async () => { for (let i = 0; i < 4; i++) await new Promise((resolve) => setImmediate(resolve)); };
  return {
    window: win, document: document, sent: sent, timers: timers, intervals: intervals, settle: settle, now: now,
    page: document.getElementById("page"),
    $: (selector) => document.querySelector(selector),
    $$: (selector) => document.querySelectorAll(selector),
    byId: (id) => document.getElementById(id),
    fire: (node, type, props) => { const e = makeEvent(type, props); node.dispatchEvent(e); return e; },
    fireWindow: (type) => (win.listeners[type] || []).forEach((f) => f()),
    // Answer the held replies (opts.hold) now.
    release: () => held.splice(0).forEach((reply) => win.prxBridge.__resolve(reply)),
    runTimers: () => timers.splice(0).forEach((f) => f()),
    calls: (method) => sent.filter((m) => m.method === method).map((m) => plain(m.params)),
  };
}

// Every element under `node`, itself first.
function walk(node) { return [node].concat(descendants(node)); }

// The elements whose own text (not a descendant's) is exactly `text`.
function withText(node, text) {
  return walk(node).filter((n) => n.childNodes.every((c) => c.nodeType === 3) && n.textContent === text);
}

module.exports = { FakeElement, FakeText, createDocument, makeEvent, boot, indexScripts, plain, bridgeError, walk, withText, UI_DIR };
