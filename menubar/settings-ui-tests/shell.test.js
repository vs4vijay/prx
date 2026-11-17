// The window's plumbing: the bridge to the hosting process (bridge.js), the
// icon set (icons.js), and the page's isolation. The settings UI makes no
// network request of its own and builds no markup from text: every daemon
// call is a `window.ipc.postMessage`, and index.html's CSP allows nothing
// else. The static checks here read the shipped sources.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { boot, plain, indexScripts, walk, UI_DIR } = require("./fake-dom.js");
const S = require("../settings-ui/schema.js");
const O = require("../settings-ui/overview.js");

const sources = () => fs.readdirSync(UI_DIR).filter((f) => f.endsWith(".js"));
const read = (name) => fs.readFileSync(path.join(UI_DIR, name), "utf8");

// ---- bridge -----------------------------------------------------------------

// The bridge alone, with a host that only records what it is sent.
function bridgeOnly() {
  const w = boot({ scripts: ["bridge.js"], hold: () => true });
  return { w: w, bridge: w.window.prxBridge, reply: (r) => w.window.prxBridge.__resolve(r) };
}

test("bridge: each call posts {id, method, params} with a fresh id", () => {
  const { w, bridge } = bridgeOnly();
  bridge.call("status.get");
  bridge.call("history.list", { limit: 5, repo: "acme/api" });
  bridge.call("status.get", null);
  assert.deepEqual(w.sent, [
    { id: 1, method: "status.get", params: {} },
    { id: 2, method: "history.list", params: { limit: 5, repo: "acme/api" } },
    { id: 3, method: "status.get", params: {} },
  ]);
});

test("bridge: the named calls map onto the documented methods and payloads", () => {
  const { w, bridge } = bridgeOnly();
  const config = { defaults: { engine: "claude" }, repos: [{ name: "acme/api" }] };
  bridge.getConfig();
  bridge.putConfig(config);
  bridge.putConfigYaml("defaults: {}\n");
  bridge.setReviewMode("monitor");
  bridge.openReviewChat("acme/api#1");
  assert.deepEqual(w.sent.map((m) => [m.method, m.params]), [
    ["config.get", {}],
    ["config.put", { config: config }],
    ["config.put", { yaml: "defaults: {}\n" }],
    ["review_mode.set", { mode: "monitor" }],
    ["app.review_chat.open", { key: "acme/api#1" }],
  ]);
  assert.equal(Object.isFrozen(bridge), true);
  assert.deepEqual(Object.keys(bridge).sort(), ["__resolve", "call", "getConfig", "openReviewChat", "putConfig", "putConfigYaml", "setReviewMode"]);
});

test("bridge: replies resolve their own call, in any order, once", async () => {
  const { bridge, reply } = bridgeOnly();
  const first = bridge.call("a");
  const second = bridge.call("b");
  reply({ id: 2, ok: true, result: { which: "b" } });
  reply({ id: 1, ok: true, result: { which: "a" } });
  assert.deepEqual(plain(await first), { which: "a" });
  assert.deepEqual(plain(await second), { which: "b" });
  // A repeated or unknown id is ignored instead of throwing in the host's call.
  assert.doesNotThrow(() => reply({ id: 1, ok: true, result: { which: "again" } }));
  assert.doesNotThrow(() => reply({ id: 99, ok: false, error: { kind: "daemon", message: "x" } }));
  assert.deepEqual(plain(await first), { which: "a" });
});

test("bridge: a failure rejects with the host's kind, message and field", async () => {
  const { bridge, reply } = bridgeOnly();
  const kinds = ["rejected", "offline", "bad_request", "unsupported", "daemon"];
  const calls = kinds.map((kind) => bridge.call("x." + kind));
  kinds.forEach((kind, i) => reply({ id: i + 1, ok: false,
    error: { kind: kind, message: kind + " happened", field: kind === "rejected" ? "defaults.poll_interval_minutes" : undefined } }));
  for (const [i, kind] of kinds.entries()) {
    const error = await calls[i].then(() => null, (e) => e);
    assert.equal(error.kind, kind);
    assert.equal(error.message, kind + " happened");
    assert.equal(error.field, kind === "rejected" ? "defaults.poll_interval_minutes" : null);
  }
  // A failure without details is still a rejection with a kind.
  const bare = bridge.call("y");
  reply({ id: kinds.length + 1, ok: false });
  const error = await bare.then(() => null, (e) => e);
  assert.equal(error.kind, "daemon");
  assert.equal(error.message, "unknown error");
  assert.equal(error.field, null);
});

// ---- isolation ----------------------------------------------------------------

test("no script reaches the network or builds markup from text", () => {
  const banned = [
    [/\bfetch\s*\(\s*["'`]|\bwindow\.fetch\b|\bXMLHttpRequest\b|\bWebSocket\b|\bEventSource\b|\bsendBeacon\b|\bimportScripts\b/, "a network API"],
    [/\.innerHTML\b|\.outerHTML\b|insertAdjacentHTML|document\.write|DOMParser|createContextualFragment|srcdoc/, "markup built from text"],
    [/\beval\s*\(|new Function\b|setTimeout\s*\(\s*["'`]|setInterval\s*\(\s*["'`]/, "code built from text"],
    // The SVG namespace and the Base URL field's "https://…" placeholder.
    [/https?:\/\/(?!www\.w3\.org\/2000\/svg|…)/, "an external URL"],
    [/localStorage|sessionStorage|indexedDB|document\.cookie/, "browser storage"],
  ];
  sources().forEach((name) => {
    const code = read(name).split("\n").filter((line) => !/^\s*\/\//.test(line)).join("\n");
    banned.forEach(([pattern, what]) => assert.equal(pattern.test(code), false, name + " uses " + what));
  });
});

test("only the bridge talks to the host, and only through window.ipc", () => {
  const users = sources().filter((name) => /\bipc\b/.test(read(name)));
  assert.deepEqual(users, ["bridge.js"]);
  assert.equal((read("bridge.js").match(/window\.ipc\.postMessage/g) || []).length, 1);
});

test("index.html: a locked-down CSP and only local assets, bridge first, app last", () => {
  const html = read("index.html");
  const csp = /http-equiv="Content-Security-Policy"\s+content="([^"]+)"/.exec(html)[1];
  const policy = {};
  csp.split(";").forEach((part) => { const words = part.trim().split(/\s+/); policy[words[0]] = words.slice(1); });
  assert.deepEqual(policy["default-src"], ["'none'"]);
  assert.deepEqual(policy["connect-src"], ["'none'"]);
  assert.deepEqual(policy["form-action"], ["'none'"]);
  assert.deepEqual(policy["frame-src"], ["'none'"]);
  assert.deepEqual(policy["script-src"], ["'self'", "prx:"]);
  assert.deepEqual(policy["style-src"], ["'self'", "prx:"]);
  assert.ok(!/unsafe-inline|unsafe-eval|\*|https?:/.test(csp), "the CSP has no escape hatch");
  // No inline script, style or handler for that CSP to block.
  assert.equal(/<script(?![^>]*\bsrc=)[^>]*>/.test(html), false);
  assert.equal(/<style|\sstyle=|\son[a-z]+=/i.test(html), false);
  const scripts = indexScripts();
  assert.equal(scripts[0], "bridge.js");
  assert.equal(scripts[scripts.length - 1], "app.js");
  // Every script on disk is loaded, and every loaded script exists.
  assert.deepEqual(scripts.slice().sort(), sources().sort());
  assert.ok(scripts.every((src) => /^[a-z-]+\.js$/.test(src)));
  assert.deepEqual(Array.from(html.matchAll(/(?:href|src)="([^"]+)"/g), (m) => m[1]).filter((u) => !scripts.includes(u)), ["app.css"]);
  // A helper is loaded before the pages that use it.
  const at = (name) => scripts.indexOf(name);
  assert.ok(at("fields.js") < at("form.js") && at("cron.js") < at("form.js") && at("icons.js") < at("form.js"));
  assert.ok(at("form.js") < at("app-section.js") && at("app-section.js") < at("pages-defaults.js"));
  assert.ok(at("pages-defaults.js") < at("pages-messages.js"));
  assert.ok(at("overview.js") < at("brief-card.js") && at("brief-card.js") < at("pages-dashboard.js"));
  assert.ok(at("chart.js") < at("pages-usage.js"));
});

test("the preview server serves only settings-ui files on localhost", () => {
  const code = fs.readFileSync(path.join(__dirname, "preview-server.js"), "utf8");
  assert.match(code, /\.listen\(port, "127\.0\.0\.1"/);
  // Request paths are reduced to a file name, so `..` cannot leave the folder.
  assert.match(code, /path\.join\(root, path\.basename\(file\)\)/);
});

// ---- icons ---------------------------------------------------------------------

function icons() {
  const w = boot({ scripts: ["icons.js"] });
  return w.window.prxIcon;
}

test("icons: decorative by default, an image with a name when labelled", () => {
  const icon = icons();
  const plainIcon = icon("check");
  assert.equal(plainIcon.tagName, "svg");
  assert.equal(plainIcon.namespaceURI, "http://www.w3.org/2000/svg");
  assert.equal(plainIcon.getAttribute("aria-hidden"), "true");
  assert.equal(plainIcon.getAttribute("role"), null);
  assert.equal(plainIcon.getAttribute("class"), "icon");
  assert.equal(plainIcon.getAttribute("viewBox"), "0 0 24 24");
  assert.equal(plainIcon.getAttribute("fill"), "none");
  assert.equal(plainIcon.getAttribute("stroke"), "currentColor");
  const named = icon("lock", "Private");
  assert.equal(named.getAttribute("role"), "img");
  assert.equal(named.getAttribute("aria-label"), "Private");
  assert.equal(named.getAttribute("aria-hidden"), null);
  // Shapes only: paths, circles and rectangles, each with geometry.
  walk(named).slice(1).forEach((shape) => {
    assert.ok(["path", "circle", "rect"].includes(shape.tagName), shape.tagName);
    assert.ok(Object.keys(shape.attrs).length > 0);
  });
  // Two calls never share nodes (an icon can only sit in one place).
  assert.notEqual(icon("check"), icon("check"));
});

test("icons: an unknown name is an empty icon, not an error", () => {
  const icon = icons();
  for (const name of ["no-such-icon", "", undefined, "__proto__", "constructor"]) {
    const svg = icon(name);
    assert.equal(svg.tagName, "svg");
    assert.equal(svg.children.length, 0, String(name));
  }
});

test("icons: every icon the UI names is drawn", () => {
  const icon = icons();
  const drawn = (name) => icon(name).children.length > 0;
  S.PAGES.forEach((p) => assert.ok(drawn(p[2]), "page " + p[0] + ": " + p[2]));
  S.MODE_OPTIONS.forEach((o) => assert.ok(drawn(o.icon), "mode " + o.value + ": " + o.icon));
  Object.keys(O.STATES).forEach((id) => assert.ok(drawn(O.STATES[id].glyph), "state " + id + ": " + O.STATES[id].glyph));
  // Icon names written out in the sources: icon("x"), iconButton("x", …),
  // empty("x", …), banner(tone, "x", …), and `icon: "x"` / `glyph: "x"`.
  const named = new Set();
  sources().forEach((name) => {
    const code = read(name);
    for (const m of code.matchAll(/\b(?:icon|iconButton|empty)\(\s*"([a-z0-9-]+)"/g)) named.add(m[1]);
    for (const m of code.matchAll(/\bbanner\(\s*"[a-z]+",\s*"([a-z0-9-]+)"/g)) named.add(m[1]);
    for (const m of code.matchAll(/\b(?:icon|glyph):\s*"([a-z0-9-]+)"/g)) named.add(m[1]);
  });
  assert.ok(named.size > 20, "the scan found the icon calls");
  named.forEach((name) => assert.ok(drawn(name), "icon " + name + " is used but not drawn"));
});
