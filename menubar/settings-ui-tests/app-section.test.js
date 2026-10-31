// The "PRX app" section of Settings → General (app-section.js): the Open at
// login switch and the command-line tool link. Both are app-local bridge
// calls that apply at once, never touch the config draft, and report their
// failures inline.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const X = require("./page-fixtures.js");
const { boot, withText, bridgeError } = require("./fake-dom.js");

const APP = "/Users/test/Applications/PRX.app";
const info = (change) => {
  const out = { bundle: APP, version: "0.1.1", login_item: { enabled: false, app: null, this_app: false },
    cli: { link: "/Users/test/.local/bin/prx", target: APP + "/Contents/Helpers/prx", state: "missing", on_path: true } };
  if (change) change(out);
  return out;
};
const section = (w) => w.page.querySelectorAll("section").find((s) => s.querySelector("h2").textContent === "PRX app");
const toggle = (w) => w.byId("app-login-item");
const install = (w) => withText(X.rowOf(w, "app.cli"), "Install command-line tool…").concat(withText(X.rowOf(w, "app.cli"), "Installed")).find((n) => n.tagName === "BUTTON");
const note = (w, path) => X.rowOf(w, path).querySelector(".row-head [role=status]").textContent;
const alertOf = (w, path) => { const e = X.rowOf(w, path).querySelector(".row-control .field-error"); return e.hidden ? null : e.textContent; };

test("until the app answers, both controls are off", async () => {
  const w = boot({ hash: "#page=general", now: X.NOW, daemon: X.daemon(), hold: (m) => m.method === "app.info" });
  await w.settle();
  assert.equal(section(w).querySelector(".section-note").textContent, "Applies right away; no Save needed.");
  assert.equal(toggle(w).disabled, true);
  assert.equal(install(w).disabled, true);
  assert.equal(X.rowOf(w, "app.login_item").querySelector(".row-label").getAttribute("for"), "app-login-item");
});

test("in PRX.app: the switch and the install button reflect the app's state", async () => {
  const w = await X.open("general", { "app.info": () => info() });
  assert.deepEqual([toggle(w).checked, toggle(w).disabled], [false, false]);
  assert.equal(toggle(w).getAttribute("role"), "switch");
  assert.equal(note(w, "app.login_item"), "Starts PRX in the menu bar when you log in. The window stays closed.");
  assert.deepEqual([install(w).textContent, install(w).disabled], ["Install command-line tool…", false]);
  assert.equal(note(w, "app.cli"), "Adds /Users/test/.local/bin/prx so prx works in Terminal.");
  assert.deepEqual(X.rowOf(w, "app.cli").querySelectorAll(".row-head code").map((c) => c.textContent), ["/Users/test/.local/bin/prx", "prx"]);
});

test("outside a bundle the login item is unavailable, unless one is already registered", async () => {
  const dev = await X.open("general", { "app.info": () => info((i) => { i.bundle = null; i.cli = { state: "unavailable" }; }) });
  assert.equal(toggle(dev).disabled, true);
  assert.match(note(dev, "app.login_item"), /^Available when PRX runs from PRX\.app/);
  assert.equal(install(dev).disabled, true);
  assert.equal(note(dev, "app.cli"), "Not available in this build.");
  // Registered by another copy: it can still be turned off from here.
  const other = await X.open("general", { "app.info": () => info((i) => { i.bundle = null; i.login_item = { enabled: true, app: "/Applications/PRX.app", this_app: false }; }) });
  assert.deepEqual([toggle(other).checked, toggle(other).disabled], [true, false]);
  const copy = await X.open("general", { "app.info": () => info((i) => { i.login_item = { enabled: true, app: "/Applications/PRX.app", this_app: false }; }) });
  assert.equal(note(copy, "app.login_item"), "Registered for another copy: /Applications/PRX.app. Turn off and on to use this one.");
});

test("each command-line tool state says what Install would do", async () => {
  const state = async (cli) => {
    const w = await X.open("general", { "app.info": () => info((i) => { Object.assign(i.cli, cli); }) });
    return [note(w, "app.cli"), install(w).textContent, install(w).disabled];
  };
  assert.deepEqual(await state({ state: "linked" }), ["Installed: /Users/test/.local/bin/prx runs this app's prx.", "Installed", true]);
  assert.deepEqual(await state({ state: "linked_elsewhere" }), ["/Users/test/.local/bin/prx points to another prx. Installing repoints it to this app.", "Install command-line tool…", false]);
  assert.deepEqual(await state({ state: "blocked" }), ["/Users/test/.local/bin/prx is a file PRX did not create, so PRX will not replace it.", "Install command-line tool…", true]);
  assert.deepEqual(await state({ state: "missing", link: undefined }), ["Adds ~/.local/bin/prx so prx works in Terminal.", "Install command-line tool…", false]);
  assert.deepEqual(await state({ state: "something_new" }), ["Not available in this build.", "Install command-line tool…", false]);
  // A warning from the app (e.g. the link's folder is not on PATH) follows the status.
  const warned = await X.open("general", { "app.info": () => info((i) => { i.cli.state = "linked"; i.warning = "~/.local/bin is not on your PATH."; }) });
  assert.equal(note(warned, "app.cli"), "Installed: /Users/test/.local/bin/prx runs this app's prx. ~/.local/bin is not on your PATH.");
});

test("the switch applies at once through the app, not through Save", async () => {
  let enabled = false;
  const w = await X.open("general", { "app.info": () => info(), "app.login_item.set": (p) => { enabled = p.enabled; return info((i) => { i.login_item = { enabled: enabled, app: APP, this_app: true }; }); } });
  toggle(w).checked = true;
  w.fire(toggle(w), "change");
  assert.equal(toggle(w).disabled, true, "off while the call is in flight");
  await w.settle();
  assert.deepEqual(w.calls("app.login_item.set"), [{ enabled: true }]);
  assert.deepEqual([toggle(w).checked, toggle(w).disabled], [true, false]);
  toggle(w).checked = false;
  w.fire(toggle(w), "change");
  await w.settle();
  assert.deepEqual(w.calls("app.login_item.set")[1], { enabled: false });
  // Not a config change: nothing to save, nothing sent to the daemon's config.
  assert.equal(w.window.prxApp.app.isDirty(), false);
  assert.equal(w.byId("save").disabled, true);
  assert.deepEqual(w.calls("config.put"), []);
});

test("a refused switch flips back and says why", async () => {
  const w = await X.open("general", { "app.info": () => info(), "app.login_item.set": () => { throw bridgeError("daemon", "macOS refused: not allowed " + X.EVIL); } });
  toggle(w).checked = true;
  w.fire(toggle(w), "change");
  await w.settle();
  assert.deepEqual([toggle(w).checked, toggle(w).disabled], [false, false]);
  assert.equal(alertOf(w, "app.login_item"), "macOS refused: not allowed " + X.EVIL);
  assert.deepEqual(X.markupIn(w.page), []);
  // The next try clears the old error first.
  toggle(w).checked = true;
  w.fire(toggle(w), "change");
  assert.equal(alertOf(w, "app.login_item"), null);
  await w.settle();
});

test("Install links the tool, and a failure leaves the button usable", async () => {
  let fail = true;
  const w = await X.open("general", { "app.info": () => info(), "app.cli.install": () => {
    if (fail) throw bridgeError("daemon", "could not create ~/.local/bin: permission denied");
    return info((i) => { i.cli.state = "linked"; });
  } });
  w.fire(install(w), "click");
  await w.settle();
  assert.equal(alertOf(w, "app.cli"), "could not create ~/.local/bin: permission denied");
  assert.equal(install(w).disabled, false);
  fail = false;
  w.fire(install(w), "click");
  await w.settle();
  assert.equal(alertOf(w, "app.cli"), null);
  assert.deepEqual([install(w).textContent, install(w).disabled], ["Installed", true]);
  assert.equal(w.calls("app.cli.install").length, 2);
  assert.equal(w.window.prxApp.app.isDirty(), false);
});

test("a host without app.info reports it and leaves the rest of General working", async () => {
  const w = await X.open("general", { "app.info": () => { throw bridgeError("bad_request", "unknown method app.info"); } });
  assert.equal(alertOf(w, "app.login_item"), "unknown method app.info");
  assert.equal(toggle(w).disabled, true);
  assert.equal(X.field(w, "defaults.poll_interval_minutes").value, "10");
});
