// The review prompt editor (design C1/C2), run against a tiny fake DOM: the
// output contract prx always appends is previewed read-only in a
// collapsible "Always added by PRX" section, and a custom prompt that still
// carries its own manifest schema gets a non-fatal hint.
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
    this.className = "";
    this.textContent = "";
    this.hidden = false;
    this.value = "";
  }
  setAttribute(k, v) {
    this.attrs[k] = String(v);
    if (k === "class") this.className = String(v);
  }
  getAttribute(k) { return this.attrs[k]; }
  hasAttribute(k) { return Object.prototype.hasOwnProperty.call(this.attrs, k); }
  append(...c) { this.children.push(...c); }
  addEventListener(t, f) { (this.listeners[t] = this.listeners[t] || []).push(f); }
  dispatchEvent(e) { (this.listeners[e.type] || []).forEach((f) => f(e)); }
  focus() {}
}

function loadForm() {
  const document = { createElement: (tag) => new FakeElement(tag) };
  const window = {
    prxFields: require("../settings-ui/fields.js"),
    prxCron: {},
    prxIcon: () => new FakeElement("svg"),
  };
  class Event { constructor(type) { this.type = type; } }
  vm.runInNewContext(fs.readFileSync(path.join(__dirname, "../settings-ui/form.js"), "utf8"),
    { window, document, Event, console, Object, Array, String });
  return window.prxForm;
}

function walk(node, out) {
  out = out || [];
  if (!node || typeof node !== "object") return out;
  out.push(node);
  (node.children || []).forEach((c) => walk(c, out));
  return out;
}

const S = require("../settings-ui/schema.js");
const CONTRACT = "## prx output contract (always added by prx)\n\n\"manifest_version\": 1, \"brief\": \"optional\"\n";
const SHIPPED = "# Review {{REPO}}\n\nRead the diff.\n";

function promptField() {
  const field = (S.FIELDS || S.fields || []).find((f) => f.key === "review_prompt");
  return field || { key: "review_prompt", type: "prompt", placeholders: [] };
}

function render(value, contract) {
  const form = loadForm();
  const app = { shipped: () => SHIPPED, contract: () => contract };
  const commits = [];
  const ctl = form.control(app, "defaults.review_prompt", promptField(), value, (v) => commits.push(v), {});
  const nodes = walk(ctl.node);
  return { ctl, nodes, commits };
}

test("the prompt editor previews the contract read-only under 'Always added by PRX'", () => {
  const { ctl, nodes, commits } = render("default", CONTRACT);
  const details = nodes.find((n) => n.tagName === "DETAILS");
  assert.ok(details, "a collapsible <details> preview is rendered");
  assert.ok(!details.hasAttribute("open"), "collapsed by default");
  const summary = details.children.find((n) => n.tagName === "SUMMARY");
  assert.equal(summary.textContent, "Always added by PRX");
  const pre = walk(details).find((n) => n.tagName === "PRE");
  assert.equal(pre.textContent, CONTRACT, "the preview shows the contract from GET /config");
  // Not editable: a <pre> (selectable text), never an input, textarea, or
  // contenteditable element; nothing in the preview commits a value.
  const editable = walk(details).filter((n) => ["INPUT", "TEXTAREA", "SELECT"].includes(n.tagName) || n.hasAttribute("contenteditable"));
  assert.deepEqual(editable, []);
  assert.equal(pre.hasAttribute("contenteditable"), false);
  walk(details).forEach((n) => Object.keys(n.listeners).forEach((t) => n.dispatchEvent({ type: t })));
  assert.deepEqual(commits, []);
  // The editable textarea is the prompt only: the contract is not in it.
  const area = nodes.filter((n) => n.tagName === "TEXTAREA");
  assert.equal(area.length, 1);
  assert.equal(area[0], ctl.focus);
  assert.equal(area[0].value, SHIPPED);
  assert.ok(!area[0].value.includes("prx output contract"));
});

test("a custom prompt with its own manifest schema shows the non-fatal hint", () => {
  const hintOf = (nodes) => nodes.find((n) => (n.className || "").includes("prompt-schema-hint"));
  const plain = render("Focus on SQL injection in {{REPO}}.", CONTRACT);
  assert.equal(hintOf(plain.nodes).hidden, true);

  const own = render("Write {\"manifest_version\": 1} to {{MANIFEST_PATH}}", CONTRACT);
  const hint = hintOf(own.nodes);
  assert.equal(hint.hidden, false);
  assert.match(hint.textContent, /adds the output instructions automatically/);

  // Live: editing the schema out hides the hint again.
  const area = plain.nodes.find((n) => n.tagName === "TEXTAREA");
  area.value = "now with \"verdict\": \"approved\"";
  area.dispatchEvent({ type: "input" });
  assert.equal(hintOf(plain.nodes).hidden, false);
});

test("no preview is rendered before the daemon supplies the contract", () => {
  const { nodes } = render("default", "");
  assert.equal(nodes.find((n) => n.tagName === "DETAILS"), undefined);
});

test("the schema hint heuristic matches prx doctor", () => {
  const F = require("../settings-ui/fields.js");
  assert.equal(F.promptHasOwnSchema("x \"manifest_version\": 1"), true);
  assert.equal(F.promptHasOwnSchema("{\"verdict\": \"approved\"}"), true);
  assert.equal(F.promptHasOwnSchema("Review for verdict quality"), false);
  assert.equal(F.promptHasOwnSchema(null), false);
});
