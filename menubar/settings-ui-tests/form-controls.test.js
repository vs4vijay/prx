// The form's building blocks (form.js) against the fake DOM: the element
// builder, rows and their error slots, notices, the ⋯ action menu, and the
// simple field controls. The dropdown and the browser picker have their own
// suites (select.test.js, browser-picker.test.js), the prompt editor
// form.test.js.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const { boot, walk } = require("./fake-dom.js");

const EVIL = '<img src=x onerror="alert(1)">';

function load() {
  const w = boot({ scripts: ["icons.js", "fields.js", "cron.js", "schema.js", "form.js"] });
  return { w: w, UI: w.window.prxForm, body: w.document.body };
}
// A stand-in for the app shell that records what a control reports.
function fakeApp(extra) {
  const app = { changed: [], errors: {}, registered: {} };
  return Object.assign(app, {
    api: Object.assign({
      changed: (path) => app.changed.push(path),
      localError: (path, message) => { if (message) app.errors[path] = message; else delete app.errors[path]; },
      register: (path, entry) => { app.registered[path] = entry; },
      shipped: () => "", contract: () => "", engines: () => [], browsers: () => null,
    }, extra || {}),
  });
}
const key = (w, node, k, extra) => w.fire(node, "keydown", Object.assign({ key: k }, extra || {}));

// ---- el, button, rich -----------------------------------------------------------

test("el: attributes, text, listeners and children", () => {
  const { w, UI } = load();
  let clicks = 0;
  const node = UI.el("button", { type: "button", class: "a b", disabled: true, hidden: false, title: null, "aria-label": undefined, text: "Go",
    onclick: () => { clicks += 1; } }, [null, UI.el("span", { text: "x" }), undefined, false]);
  assert.equal(node.tagName, "BUTTON");
  assert.deepEqual(Object.keys(node.attrs).sort(), ["class", "disabled", "type"]);
  assert.equal(node.getAttribute("disabled"), "", "true is a bare attribute");
  assert.equal(node.children.length, 1);
  // A handler is a listener, never an attribute.
  assert.equal(node.hasAttribute("onclick"), false);
  w.fire(node, "click");
  assert.equal(clicks, 1);
  // `text` is text whatever it contains.
  const hostile = UI.el("p", { text: EVIL });
  assert.equal(hostile.textContent, EVIL);
  assert.equal(hostile.children.length, 0);
  assert.equal(UI.el("div").children.length, 0);
  // 0 and "" are real attribute values.
  assert.equal(UI.el("td", { colspan: 0 }).getAttribute("colspan"), "0");
  assert.equal(UI.el("input", { value: "" }).getAttribute("value"), "");
});

test("button, iconButton and fieldId", () => {
  const { UI } = load();
  assert.equal(UI.button("Save").getAttribute("class"), "btn");
  assert.equal(UI.button("Save", null, "primary small").getAttribute("class"), "btn primary small");
  // "link" makes a text-only button, wherever it sits in the class list.
  assert.equal(UI.button("More", null, "link").getAttribute("class"), "link");
  assert.equal(UI.button("More", null, "small link").getAttribute("class"), "small link");
  assert.equal(UI.button("More", null, "linked").getAttribute("class"), "btn linked");
  assert.equal(UI.button("Save").getAttribute("type"), "button");
  assert.equal(UI.button("Save").textContent, "Save");
  const icon = UI.iconButton("close", "Dismiss", null, "remove");
  assert.deepEqual([icon.getAttribute("aria-label"), icon.getAttribute("title"), icon.getAttribute("class")], ["Dismiss", "Dismiss", "icon-button remove"]);
  assert.equal(icon.querySelector("svg").getAttribute("aria-hidden"), "true");
  assert.equal(UI.fieldId("defaults.skip.drafts"), "f-defaults_skip_drafts");
  assert.equal(UI.fieldId("repos[acme/api.v2].engine"), "f-repos_acme_api_v2__engine");
  assert.match(UI.fieldId("repos[" + EVIL + "].x"), /^f-[A-Za-z0-9_-]+$/);
});

test("rich: backticks become code, everything else stays text", () => {
  const { UI } = load();
  const parts = (text) => Array.from(UI.rich(text), (n) => (n.nodeType === 3 ? n.textContent : [n.tagName, n.textContent]));
  assert.deepEqual(parts("Run `prx daemon start` now"), ["Run ", ["CODE", "prx daemon start"], " now"]);
  assert.deepEqual(parts("`a` and `b`"), [["CODE", "a"], " and ", ["CODE", "b"]]);
  assert.deepEqual(parts("no code here"), ["no code here"]);
  assert.deepEqual(parts("a lone ` stays"), ["a lone ` stays"]);
  assert.deepEqual(parts("empty `` pair"), ["empty `` pair"]);
  assert.deepEqual(parts(""), []);
  assert.deepEqual(parts(42), ["42"]);
  assert.deepEqual(parts("`" + EVIL + "` " + EVIL), [["CODE", EVIL], " " + EVIL]);
});

// ---- row, section, notices ------------------------------------------------------

test("row: the label, help, error slot and registration", () => {
  const { UI } = load();
  const app = fakeApp();
  const input = UI.el("input", { id: "f-x", type: "text" });
  const row = UI.row(app.api, "defaults.x", "Label " + EVIL, { node: input, focus: input },
    { help: "Use `x`. " + EVIL, stack: true, bare: true, inherited: true, aside: [UI.el("span", { text: "Default" })] });
  assert.equal(row.getAttribute("class"), "row stack-row bare-row inherited");
  assert.equal(row.getAttribute("data-field"), "defaults.x");
  const label = row.querySelector(".row-label");
  assert.deepEqual([label.tagName, label.getAttribute("for"), label.id, label.textContent], ["LABEL", "f-x", "f-defaults_x-label", "Label " + EVIL]);
  assert.equal(row.querySelector(".row-head .help code").textContent, "x");
  assert.equal(row.querySelector(".row-aside").textContent, "Default");
  const error = row.querySelector(".field-error");
  assert.deepEqual([error.getAttribute("role"), error.hidden, error.textContent], ["alert", true, ""]);
  assert.equal(error.parentNode, row.querySelector(".row-control"), "the error sits under the control");
  const entry = app.registered["defaults.x"];
  assert.deepEqual([entry.row, entry.error, entry.focus], [row, error, input]);
  assert.deepEqual(walk(row).filter((n) => ["IMG", "SCRIPT"].includes(n.tagName)), []);
  // No options: a plain row.
  const plainRow = UI.row(app.api, "defaults.y", "Y", { node: UI.el("span"), focus: null });
  assert.equal(plainRow.getAttribute("class"), "row");
  assert.equal(plainRow.querySelector(".row-label").tagName, "SPAN");
  assert.equal(plainRow.querySelector(".help"), null);
  assert.equal(plainRow.querySelector(".row-aside"), null);
});

test("row: a radio group and a dropdown are named by the label, not targeted by it", () => {
  const { UI } = load();
  const app = fakeApp();
  const group = UI.el("div", { role: "radiogroup" });
  const first = UI.el("input", { type: "radio", id: "g-0" });
  const grouped = UI.row(app.api, "defaults.g", "Group", { node: group, focus: first, group: group });
  assert.equal(grouped.querySelector(".row-label").tagName, "SPAN");
  assert.equal(group.getAttribute("aria-labelledby"), "f-defaults_g-label");
  const dropdown = UI.select({ id: "f-defaults_d", options: [{ value: "a", label: "A" }], value: "a" });
  const combo = UI.row(app.api, "defaults.d", "Dropdown", dropdown);
  assert.equal(combo.querySelector(".row-label").tagName, "SPAN");
  assert.equal(dropdown.trigger.getAttribute("aria-labelledby"), "f-defaults_d-label");
  // A dropdown with its own name keeps it.
  const named = UI.select({ id: "s2", label: "Own name", options: [], value: undefined });
  UI.row(app.api, "defaults.e", "Row", named);
  assert.equal(named.trigger.getAttribute("aria-labelledby"), null);
  assert.equal(named.trigger.getAttribute("aria-label"), "Own name");
});

test("section, header, banner and empty", () => {
  const { UI } = load();
  const section = UI.section("Pace", [UI.el("div", { class: "row" })], "A note");
  assert.deepEqual(section.children.map((c) => [c.tagName, c.textContent]), [["H2", "Pace"], ["P", "A note"], ["DIV", ""]]);
  assert.equal(UI.section("Pace", []).children.length, 2);
  assert.deepEqual(UI.header("Title", "Lede").children.map((c) => [c.tagName, c.textContent]), [["H1", "Title"], ["P", "Lede"]]);
  assert.equal(UI.header("Title").children.length, 1);
  // Only errors interrupt a screen reader.
  assert.equal(UI.banner("error", "alert", "Broken.", "See `config.yml`").getAttribute("role"), "alert");
  assert.equal(UI.banner("warn", "alert", "Careful.", "").getAttribute("role"), "status");
  assert.equal(UI.banner("info", "check", null, "Fine").getAttribute("role"), "status");
  const banner = UI.banner("error", "alert", "Broken.", "See `config.yml` " + EVIL, [UI.button("Open")]);
  assert.equal(banner.getAttribute("class"), "banner error");
  assert.equal(banner.querySelector("strong").textContent, "Broken. ");
  assert.equal(banner.querySelector("code").textContent, "config.yml");
  assert.equal(banner.querySelector(".banner-text").textContent, "Broken. See config.yml " + EVIL);
  assert.equal(banner.children[banner.children.length - 1].textContent, "Open");
  assert.equal(UI.banner("info", "check", "Only a title").querySelector("strong").textContent, "Only a title");
  const empty = UI.empty("repo", "Nothing here", "Add `one`.", [UI.button("Add")]);
  assert.deepEqual(empty.children.map((c) => c.getAttribute("class")), ["empty-icon", "empty-title", "empty-text", "empty-actions"]);
  assert.deepEqual(UI.empty("repo", "Nothing here").children.map((c) => c.getAttribute("class")), ["empty-icon", "empty-title"]);
  assert.deepEqual(UI.empty("repo", "Nothing here", "", []).children.map((c) => c.getAttribute("class")), ["empty-icon", "empty-title"]);
});

// ---- the ⋯ menu -----------------------------------------------------------------

function menu(items, opts) {
  const { w, UI, body } = load();
  const ran = [];
  const anchor = UI.menuButton("More actions", items || [
    { heading: "Review with" },
    { label: "claude", onclick: () => ran.push("claude") },
    { label: "codex", disabled: true, onclick: () => ran.push("codex") },
    { separator: true },
    { label: "Open", onclick: () => ran.push("open") },
  ], opts);
  body.append(anchor);
  const trigger = anchor.querySelector("button[aria-haspopup=menu]");
  const list = anchor.querySelector("[role=menu]");
  return { w: w, anchor: anchor, trigger: trigger, list: list, ran: ran, items: () => list.querySelectorAll("[role=menuitem]"), body: body };
}

test("action menu: a labelled menu button whose menu is closed until asked", () => {
  const m = menu();
  assert.deepEqual([m.trigger.getAttribute("aria-label"), m.trigger.getAttribute("aria-expanded"), m.trigger.getAttribute("aria-controls")],
    ["More actions", "false", m.list.id]);
  assert.equal(m.list.hidden, true);
  assert.equal(m.list.getAttribute("aria-label"), "More actions");
  assert.deepEqual(m.items().map((i) => [i.textContent, i.disabled, i.getAttribute("tabindex")]), [["claude", false, "-1"], ["codex", true, "-1"], ["Open", false, "-1"]]);
  // Headings and separators are not items.
  assert.equal(m.list.querySelector(".menu-label").getAttribute("role"), "presentation");
  assert.equal(m.list.querySelector(".menu-sep").getAttribute("role"), "separator");
  // Two menus on a page never share an id.
  assert.notEqual(menu().list.id, menu().list.id);
});

test("action menu: click or arrow opens on the first enabled item; arrows wrap past disabled ones", () => {
  const m = menu();
  m.w.fire(m.trigger, "click");
  assert.equal(m.list.hidden, false);
  assert.equal(m.trigger.getAttribute("aria-expanded"), "true");
  assert.equal(m.w.document.activeElement, m.items()[0]);
  key(m.w, m.w.document.activeElement, "ArrowDown");
  assert.equal(m.w.document.activeElement.textContent, "Open", "the disabled item is skipped");
  key(m.w, m.w.document.activeElement, "ArrowDown");
  assert.equal(m.w.document.activeElement.textContent, "claude", "wraps to the top");
  key(m.w, m.w.document.activeElement, "ArrowUp");
  assert.equal(m.w.document.activeElement.textContent, "Open", "wraps to the bottom");
  key(m.w, m.w.document.activeElement, "Home");
  assert.equal(m.w.document.activeElement.textContent, "claude");
  assert.equal(key(m.w, m.w.document.activeElement, "End").defaultPrevented, true);
  assert.equal(m.w.document.activeElement.textContent, "Open");
  // Other keys are left to the browser.
  assert.equal(key(m.w, m.w.document.activeElement, "a").defaultPrevented, false);
  // A second click on the trigger closes it and keeps focus there.
  m.w.fire(m.trigger, "click");
  assert.equal(m.list.hidden, true);
  assert.equal(m.w.document.activeElement, m.trigger);
  // The arrow keys open it from the trigger.
  assert.equal(key(m.w, m.trigger, "ArrowDown").defaultPrevented, true);
  assert.equal(m.list.hidden, false);
});

test("action menu: picking runs the action once and closes; Escape, Tab and an outside click close without running", () => {
  const m = menu();
  m.w.fire(m.trigger, "click");
  m.w.fire(m.items()[2], "click");
  assert.deepEqual(m.ran, ["open"]);
  assert.equal(m.list.hidden, true);
  assert.equal(m.trigger.getAttribute("aria-expanded"), "false");
  assert.equal(m.w.document.activeElement, m.trigger);
  m.w.fire(m.trigger, "click");
  key(m.w, m.w.document.activeElement, "Escape");
  assert.equal(m.list.hidden, true);
  assert.equal(m.w.document.activeElement, m.trigger, "Escape returns focus");
  m.w.fire(m.trigger, "click");
  const tab = key(m.w, m.w.document.activeElement, "Tab");
  assert.equal(m.list.hidden, true);
  assert.equal(tab.defaultPrevented, false, "Tab still moves focus on");
  m.w.fire(m.trigger, "click");
  m.w.fire(m.body, "mousedown");
  assert.equal(m.list.hidden, true);
  // A press inside the menu is not "outside".
  m.w.fire(m.trigger, "click");
  m.w.fire(m.items()[0], "mousedown");
  assert.equal(m.list.hidden, false);
  assert.deepEqual(m.ran, ["open"]);
});

test("action menu: a disabled trigger, and a menu with nothing enabled", () => {
  const off = menu(null, { disabled: true });
  assert.equal(off.trigger.disabled, true);
  off.trigger.click();
  assert.equal(off.list.hidden, true);
  const none = menu([{ label: "a", disabled: true, onclick: () => {} }]);
  none.w.fire(none.trigger, "click");
  assert.equal(none.list.hidden, false);
  assert.doesNotThrow(() => key(none.w, none.list, "ArrowDown"));
  assert.doesNotThrow(() => key(none.w, none.list, "Home"));
});

// ---- simple controls ------------------------------------------------------------

test("segmented: one radio per option, reporting only the one that became checked", () => {
  const { w, UI } = load();
  const picked = [];
  const seg = UI.segmented("tone", [{ value: "a", label: "A" }, { value: "b", label: "B" }, { value: "c", label: "C" }], "b", (v) => picked.push(v), "Tone");
  assert.equal(seg.node.getAttribute("role"), "radiogroup");
  assert.equal(seg.node.getAttribute("aria-label"), "Tone");
  const inputs = seg.node.querySelectorAll("input");
  assert.deepEqual(inputs.map((i) => [i.id, i.getAttribute("name"), i.getAttribute("value"), i.checked]), [["tone-0", "tone", "a", false], ["tone-1", "tone", "b", true], ["tone-2", "tone", "c", false]]);
  assert.deepEqual(seg.node.querySelectorAll("label").map((l) => [l.getAttribute("for"), l.textContent]), [["tone-0", "A"], ["tone-1", "B"], ["tone-2", "C"]]);
  assert.equal(seg.focus, inputs[1]);
  inputs[2].checked = true;
  w.fire(inputs[2], "change");
  // The radio that lost the selection also fires in some browsers: ignored.
  inputs[1].checked = false;
  w.fire(inputs[1], "change");
  assert.deepEqual(picked, ["c"]);
  // No current value: focus starts on the first option.
  const none = UI.segmented("x", [{ value: "a", label: "A" }], undefined, () => {});
  assert.equal(none.focus, none.node.querySelector("input"));
  assert.equal(none.node.hasAttribute("aria-label"), false);
});

test("control: a switch commits its state", () => {
  const { w, UI } = load();
  const commits = [];
  const on = UI.control(fakeApp().api, "defaults.skip.drafts", { type: "bool" }, true, (v) => commits.push(v));
  assert.deepEqual([on.node.getAttribute("type"), on.node.getAttribute("role"), on.node.id, on.node.checked], ["checkbox", "switch", "f-defaults_skip_drafts", true]);
  assert.equal(on.focus, on.node);
  on.node.checked = false;
  w.fire(on.node, "change");
  assert.deepEqual(commits, [false]);
  // null, undefined and 0 all show as off.
  for (const off of [null, undefined, 0, false]) assert.equal(UI.control(fakeApp().api, "p", { type: "bool" }, off, () => {}).node.checked, false);
});

test("control: text and textarea keep what is typed; only optional fields unset when blank", () => {
  const { w, UI } = load();
  const run = (field, value) => {
    const app = fakeApp();
    const commits = [];
    const ctl = UI.control(app.api, "defaults.t", field, value, (v) => commits.push(v));
    return { app: app, commits: commits, input: ctl.focus, type: (text) => { ctl.focus.value = text; w.fire(ctl.focus, "input"); w.fire(ctl.focus, "change"); } };
  };
  const optional = run({ type: "text", optional: true, placeholder: "e.g. X" }, null);
  assert.deepEqual([optional.input.tagName, optional.input.getAttribute("type"), optional.input.value, optional.input.getAttribute("placeholder")], ["INPUT", "text", "", "e.g. X"]);
  assert.equal(optional.input.getAttribute("autocomplete"), "off");
  optional.type("  maya  ");
  optional.type("   ");
  assert.deepEqual(optional.commits, ["  maya  ", null]);
  // Typing marks the form dirty before the value commits.
  assert.deepEqual(optional.app.changed, ["defaults.t", "defaults.t"]);
  const required = run({ type: "text" }, 0);
  assert.equal(required.input.value, "0");
  required.type("");
  assert.deepEqual(required.commits, [""]);
  const area = run({ type: "textarea", rows: 4, optional: true }, "a\nb");
  assert.deepEqual([area.input.tagName, area.input.getAttribute("rows"), area.input.value, area.input.hasAttribute("type")], ["TEXTAREA", "4", "a\nb", false]);
  assert.equal(run({ type: "textarea" }, "").input.getAttribute("rows"), "3");
  // A type the form does not know is edited as plain text, not dropped.
  const unknown = run({ type: "future-widget" }, "kept");
  assert.equal(unknown.input.value, "kept");
});

test("control: numbers keep a unit slot and report bad input instead of committing it", () => {
  const { w, UI } = load();
  const app = fakeApp();
  const commits = [];
  const ctl = UI.control(app.api, "defaults.n", { type: "number", unit: "min" }, 15, (v) => commits.push(v));
  assert.equal(ctl.focus.value, "15");
  assert.equal(ctl.focus.getAttribute("inputmode"), "numeric");
  assert.equal(ctl.node.querySelector(".unit").textContent, "min");
  const type = (text) => { ctl.focus.value = text; w.fire(ctl.focus, "change"); };
  type("20");
  assert.deepEqual([commits, app.errors], [[20], {}]);
  type("twenty");
  assert.deepEqual([commits, app.errors], [[20], { "defaults.n": "Enter a whole number" }]);
  type("007");
  assert.deepEqual([commits, app.errors], [[20, 7], {}]);
  // No unit: the slot is kept so a column of numbers lines up.
  const bare = UI.control(fakeApp().api, "defaults.m", { type: "number" }, null, () => {});
  assert.equal(bare.focus.value, "");
  assert.equal(bare.node.querySelector(".unit").textContent, "");
});

test("control: the model field suggests the agent's models and says when it is ignored", () => {
  const { w, UI } = load();
  const engines = [
    { name: "claude", label: "Claude Code", type: "agent", models: ["opus", "sonnet"], model_args: ["--model", "{{MODEL}}"] },
    { name: "bare", label: "Bare", type: "agent", models: [], model_args: [] },
    { name: "cheap", type: "llm_api", model: "claude-haiku-4-5" },
    { name: "nomodel", type: "llm_api", model: "" },
  ];
  const build = (engine, value) => {
    const commits = [];
    const ctl = UI.control(fakeApp({ engines: () => engines }).api, "defaults.agent_model", { type: "model", optional: true }, value, (v) => commits.push(v), { context: { engine: engine } });
    return { ctl: ctl, commits: commits, note: () => { const n = ctl.node.children[2]; return n ? [n.getAttribute("class"), n.textContent] : null; } };
  };
  const claude = build("claude", "opus");
  assert.equal(claude.ctl.focus.value, "opus");
  assert.equal(claude.ctl.focus.getAttribute("list"), "f-defaults_agent_model-models");
  assert.deepEqual(claude.ctl.node.querySelectorAll("datalist option").map((o) => o.getAttribute("value")), ["opus", "sonnet"]);
  assert.deepEqual(claude.note(), ["help", "Passed as --model opus. Any name the agent accepts works."]);
  assert.deepEqual(build("claude", null).note(), ["help", "Passed as --model <model>. Any name the agent accepts works."]);
  assert.deepEqual(build("bare", "x").note(), ["status-line warn", "Bare has no model arguments, so this is ignored. Add them on the Engines page."]);
  assert.deepEqual(build("cheap", "x").note(), ["status-line warn", "cheap is an LLM API engine with its own model (claude-haiku-4-5), so this is ignored."]);
  assert.deepEqual(build("nomodel", "x").note(), ["status-line warn", "nomodel is an LLM API engine with its own model (?), so this is ignored."]);
  // An engine nobody defines: no claim either way, and no suggestions.
  const ghost = build("ghost", "x");
  assert.equal(ghost.note(), null);
  assert.equal(ghost.ctl.node.querySelectorAll("datalist option").length, 0);
  claude.ctl.focus.value = "  sonnet ";
  w.fire(claude.ctl.focus, "change");
  claude.ctl.focus.value = "  ";
  w.fire(claude.ctl.focus, "change");
  assert.deepEqual(claude.commits, ["sonnet", null]);
});

test("agentStatus and llmStatus: every state, and silence when nothing is known", () => {
  const { UI } = load();
  const say = (agent, onAgentsPage) => { const n = UI.agentStatus(agent, onAgentsPage); return n ? [n.getAttribute("class"), n.textContent] : null; };
  assert.equal(say(null), null);
  assert.equal(say({ detected: false, path: "/x", on_path: true }), null, "not scanned: nothing is claimed");
  assert.deepEqual(say({ detected: true, path: "/bin/a", on_path: true, usable: true }), ["status-line ok", "Found at /bin/a"]);
  assert.deepEqual(say({ detected: true, path: "/bin/a", on_path: false, usable: false }), ["status-line ok", "Found at /bin/a. Adding it uses that full path."]);
  assert.deepEqual(say({ detected: true, path: "/bin/a", on_path: false, usable: true }, false),
    ["status-line warn", "Found at /bin/a, which is not on the daemon's PATH. Set that full path as its command on the Engines page."]);
  assert.deepEqual(say({ detected: true, path: "/bin/a", on_path: false, usable: true }, true),
    ["status-line warn", "Found at /bin/a, which is not on the daemon's PATH. Customize it and set that full path as the command."]);
  assert.deepEqual(say({ detected: true, path: null, usable: false }), ["status-line", "Not installed on this Mac."]);
  assert.deepEqual(say({ detected: true, path: null, usable: true, command: "mine " + EVIL }), ["status-line warn", "mine " + EVIL + " was not found on this Mac. Reviews fail until it is installed."]);
  const llm = (engine) => { const n = UI.llmStatus(engine); return n ? [n.getAttribute("class"), n.textContent] : null; };
  assert.equal(llm(null), null);
  assert.equal(llm({ type: "agent", key_present: true }), null);
  assert.equal(llm({ type: "llm_api", key_present: null, api_key_env: "K" }), null, "unknown is not reported as missing");
  assert.deepEqual(llm({ type: "llm_api", key_present: true, api_key_env: "K" }), ["status-line ok", "Key read from K."]);
  assert.deepEqual(llm({ type: "llm_api", key_present: false, api_key_env: "K" }), ["status-line warn", "K is not set for the daemon. Reviews with this engine fail until it is."]);
});
