// The Engines page end to end against the fake DOM: agent CLIs as the
// daemon found them, custom agents, LLM API engines, and what each edit puts
// in the `engines:` map a save sends. API keys never appear here: an LLM
// API engine holds the name of an environment variable.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const X = require("./page-fixtures.js");
const { plain, withText, walk, bridgeError } = require("./fake-dom.js");

const RAW = () => X.config().raw;
const draft = (w) => plain(w.window.prxApp.app.draft());
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
const item = (w, name) => w.page.querySelectorAll(".agent").find((a) => a.getAttribute("data-field") === "engines." + name) || null;
const names = (w) => w.page.querySelectorAll(".agent").map((a) => a.getAttribute("data-field").slice(8));
const badges = (w, name) => item(w, name).querySelectorAll(".agent-title .badge").map((b) => b.textContent);
const actions = (w, name) => item(w, name).querySelectorAll(".agent-actions button").map((b) => b.textContent);
// The control of an engine's editor field, by its label.
function control(w, name, label) {
  const field = item(w, name).querySelectorAll(".agent-field").find((f) => f.querySelector("label, .agent-field-label").textContent === label);
  return field.querySelector("input, textarea, [role=combobox]");
}
const errorOn = (w, name) => { const e = item(w, name).children.filter((c) => c.classList.contains("field-error")).pop(); return e.hidden ? null : e.textContent; };

function agent(name, label, extra) {
  return Object.assign({ name: name, label: label, command: name, args: ["-p", "{{PROMPT}}"], model_args: ["--model", "{{MODEL}}"], models: [], builtin: true,
    path: "/opt/homebrew/bin/" + name, on_path: true }, extra || {});
}
// discover.agents: three built-ins in different states, and one preset.
const FOUND = {
  agents: [
    agent("pi", "Pi"),
    agent("claude", "Claude Code", { args: ["-p", "{{PROMPT}}", "--dangerously-skip-permissions"], path: "/Users/test/.local/bin/claude", on_path: false }),
    agent("codex", "Codex CLI", { args: ["exec", "-"], model_args: ["-m", "{{MODEL}}"], path: null, on_path: false }),
    agent("gemini", "Gemini CLI", { builtin: false }),
    agent("ghost", "Ghost CLI", { builtin: false, path: null, on_path: false }),
  ],
  configured: {},
  llm_apis: [{ name: "cheap", api_key_env: "ANTHROPIC_API_KEY", key_present: true }],
};
const found = (change) => () => { const f = JSON.parse(JSON.stringify(FOUND)); if (change) change(f); return f; };

test("agents: where each was found, what it is, and what can be done with it", async () => {
  const w = await X.open("engines", { "discover.agents": found() });
  assert.deepEqual(w.page.querySelectorAll("section > h2").map((h) => h.textContent), ["Agents", "LLM APIs", "More agents"]);
  assert.deepEqual(names(w), ["pi", "claude", "codex", "cheap", "gemini", "ghost"]);
  assert.deepEqual(badges(w, "claude"), ["Reviewing", "Built in"]);
  assert.deepEqual(badges(w, "pi"), ["Built in"]);
  // The engine in use cannot be "used" again; the others can.
  assert.deepEqual(actions(w, "claude"), ["Customize"]);
  assert.deepEqual(actions(w, "pi"), ["Use for reviews", "Customize"]);
  assert.deepEqual(actions(w, "gemini"), ["Add"]);
  const status = (name) => { const s = item(w, name).querySelector(".status-line"); return [s.className, s.textContent]; };
  assert.deepEqual(status("pi"), ["status-line ok", "Found at /opt/homebrew/bin/pi"]);
  assert.deepEqual(status("claude"), ["status-line warn", "Found at /Users/test/.local/bin/claude, which is not on the daemon's PATH. Customize it and set that full path as the command."]);
  assert.deepEqual(status("codex"), ["status-line warn", "codex was not found on this Mac. Reviews fail until it is installed."]);
  assert.deepEqual(status("gemini"), ["status-line ok", "Found at /opt/homebrew/bin/gemini"]);
  assert.deepEqual(status("ghost"), ["status-line", "Not installed on this Mac."]);
  assert.ok(item(w, "gemini").classList.contains("unused"));
  // An unattended flag is explained next to the agent that uses it.
  assert.match(item(w, "claude").querySelector("p.help").textContent, /^--dangerously-skip-permissions lets the agent use its tools without asking/);
  assert.match(item(w, "claude").querySelector("p.help").textContent, /only PRX posts to GitHub\.$/);
  assert.equal(item(w, "pi").querySelector("p.help"), null);
  // A built-in that is not customized has nothing to edit.
  assert.equal(item(w, "pi").querySelector(".agent-editor"), null);
  // The page says who posts.
  assert.match(w.page.querySelector(".lede").textContent, /the engine never posts to GitHub\.$/);
  assert.equal(w.window.prxApp.app.isDirty(), false);
});

test("before discovery answers, the built-in engines are still offered", async () => {
  const w = await X.open("engines", { "discover.agents": X.offline });
  assert.deepEqual(names(w), ["pi", "claude", "codex", "cheap"]);
  assert.deepEqual(badges(w, "claude"), ["Reviewing", "Built in"]);
  // Nothing is claimed about where they are.
  assert.equal(item(w, "pi").querySelector(".status-line"), null);
  // Without the catalog's arguments there is nothing to copy into a custom entry.
  assert.deepEqual(actions(w, "pi"), ["Use for reviews"]);
});

test("Use for reviews sets the review engine", async () => {
  const w = await X.open("engines", X.storing(null, { "discover.agents": found() }).handlers);
  w.fire(button(item(w, "pi"), "Use for reviews"), "click");
  assert.equal(draft(w).defaults.engine, "pi");
  assert.deepEqual(badges(w, "pi"), ["Reviewing", "Built in"]);
  assert.deepEqual(badges(w, "claude"), ["Built in"]);
  await X.save(w);
  const expected = RAW();
  expected.defaults.engine = "pi";
  assert.deepEqual(X.lastPut(w), expected);
});

test("Customize copies the built-in command line, pinning the path when it is off the daemon's PATH", async () => {
  const w = await X.open("engines", X.storing(null, { "discover.agents": found() }).handlers);
  w.fire(button(item(w, "claude"), "Customize"), "click");
  assert.deepEqual(draft(w).engines.claude, { type: "agent", command: "/Users/test/.local/bin/claude",
    args: ["-p", "{{PROMPT}}", "--dangerously-skip-permissions"], model_args: ["--model", "{{MODEL}}"] });
  assert.deepEqual(badges(w, "claude"), ["Reviewing", "Customized"]);
  assert.deepEqual(actions(w, "claude"), ["Restore built-in"]);
  assert.equal(control(w, "claude", "Command").value, "/Users/test/.local/bin/claude");
  assert.equal(control(w, "claude", "Arguments").value, "-p\n{{PROMPT}}\n--dangerously-skip-permissions");
  // The pinned path is where it was found, so it now reads as found.
  assert.deepEqual([item(w, "claude").querySelector(".status-line").className, item(w, "claude").querySelector(".status-line").textContent],
    ["status-line ok", "Found at /Users/test/.local/bin/claude"]);
  // One on the PATH keeps its plain command name.
  w.fire(button(item(w, "pi"), "Customize"), "click");
  assert.equal(draft(w).engines.pi.command, "pi");
  // A built-in cannot be renamed.
  assert.equal(item(w, "pi").querySelectorAll(".agent-field label").map((l) => l.textContent).includes("Name"), false);
  // Edits are trimmed and split one argument per line.
  X.type(w, control(w, "pi", "Command"), "  /opt/pi/bin/pi  ");
  X.type(w, control(w, "pi", "Arguments"), "--quiet\n\n-p\n{{PROMPT}}  \n");
  X.type(w, control(w, "pi", "Model arguments"), "");
  assert.deepEqual(draft(w).engines.pi, { type: "agent", command: "/opt/pi/bin/pi", args: ["--quiet", "-p", "{{PROMPT}}"] });
  await X.save(w);
  assert.deepEqual(Object.keys(X.lastPut(w).engines), ["cheap", "claude", "pi"]);
  // Restore built-in removes the entry again.
  w.fire(button(item(w, "pi"), "Restore built-in"), "click");
  assert.equal("pi" in draft(w).engines, false);
  assert.deepEqual(badges(w, "pi"), ["Built in"]);
});

test("a preset agent is added to make it selectable, and removed again", async () => {
  const w = await X.open("engines", { "discover.agents": found() });
  w.fire(button(item(w, "gemini"), "Add"), "click");
  assert.deepEqual(draft(w).engines.gemini, { type: "agent", command: "gemini", args: ["-p", "{{PROMPT}}"], model_args: ["--model", "{{MODEL}}"] });
  assert.equal(item(w, "gemini").classList.contains("unused"), false);
  assert.deepEqual(actions(w, "gemini"), ["Use for reviews", "Remove"]);
  // It is now on offer as the review engine.
  w.window.prxApp.go("review");
  assert.ok(X.optionLabels(w, X.field(w, "defaults.engine")).includes("Gemini CLI"));
  w.window.prxApp.go("engines");
  w.fire(button(item(w, "gemini"), "Remove"), "click");
  assert.equal("gemini" in draft(w).engines, false);
  assert.ok(item(w, "gemini").classList.contains("unused"));
});

test("Add your own: a new agent entry, with name checks", async () => {
  const w = await X.open("engines", X.storing(null, { "discover.agents": found() }).handlers);
  const name = w.byId("add-agent");
  const add = () => w.fire(button(X.rowOf(w, "engines"), "Add"), "click");
  // Nothing typed: nothing added.
  add();
  assert.deepEqual(Object.keys(draft(w).engines), ["cheap"]);
  assert.equal(w.window.prxApp.app.isDirty(), false);
  for (const [text, message] of [["my reviewer", "An engine name can't contain spaces"], ["cheap", "An engine named cheap already exists"], ["claude", "An engine named claude already exists"]]) {
    w.byId("add-agent").value = text;
    add();
    assert.equal(X.errorOf(w, "engines"), message, text);
    assert.deepEqual(Object.keys(draft(w).engines), ["cheap"]);
  }
  // A preset that is not in use yet does not block its name.
  w.byId("add-agent").value = "  mine  ";
  assert.equal(w.fire(w.byId("add-agent"), "keydown", { key: "Enter" }).defaultPrevented, true);
  assert.equal(X.errorOf(w, "engines"), null);
  assert.deepEqual(draft(w).engines.mine, { type: "agent", command: "", args: ["{{PROMPT}}"] });
  assert.deepEqual(badges(w, "mine"), ["Custom"]);
  assert.equal(name.isConnected, false, "the page was redrawn with the new entry");
  X.type(w, control(w, "mine", "Command"), "/usr/local/bin/mine");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).engines, { cheap: RAW().engines.cheap, mine: { type: "agent", command: "/usr/local/bin/mine", args: ["{{PROMPT}}"] } });
});

test("usage fields are edited as `field = path` lines and checked before they are kept", async () => {
  const w = await X.open("engines", { "config.get": () => X.config((c) => { c.raw.engines.mine = { type: "agent", command: "mine", args: ["{{PROMPT}}"], usage_output: { output_tokens: "u.out", input_tokens: "u.in" } }; }) });
  const usage = () => control(w, "mine", "Usage fields");
  assert.equal(usage().value, "input_tokens = u.in\noutput_tokens = u.out");
  X.type(w, usage(), "tokens = u.in");
  assert.match(errorOn(w, "mine"), /^Unknown usage field tokens\. Use one of: input_tokens, /);
  assert.deepEqual(draft(w).engines.mine.usage_output, { output_tokens: "u.out", input_tokens: "u.in" }, "a bad mapping is not kept");
  X.type(w, usage(), "cost_usd: total.cost\nmodel = meta.model");
  assert.equal(errorOn(w, "mine"), null);
  assert.deepEqual(draft(w).engines.mine.usage_output, { cost_usd: "total.cost", model: "meta.model" });
  X.type(w, usage(), "  \n");
  assert.equal("usage_output" in draft(w).engines.mine, false);
});

// ---- LLM APIs -------------------------------------------------------------------

test("LLM API engines hold a key variable's name, and whether the daemon can see it", async () => {
  const w = await X.open("engines", { "discover.agents": found() });
  assert.deepEqual(badges(w, "cheap"), ["LLM API"]);
  assert.deepEqual(item(w, "cheap").querySelectorAll(".agent-field-label").map((l) => l.textContent), ["Name", "Provider", "Base URL", "Model", "Key variable", "Temperature", "Max tokens"]);
  const key = control(w, "cheap", "Key variable");
  assert.equal(key.value, "ANTHROPIC_API_KEY");
  assert.equal(key.getAttribute("type"), "text");
  assert.equal(key.getAttribute("placeholder"), "ANTHROPIC_API_KEY");
  assert.match(key.closest(".agent-field").querySelector(".help").textContent, /^Name of the environment variable holding the API key.*The key itself is never stored\.$/);
  assert.deepEqual([item(w, "cheap").querySelector(".status-line").className, item(w, "cheap").querySelector(".status-line").textContent],
    ["status-line ok", "Key read from ANTHROPIC_API_KEY."]);
  assert.equal(control(w, "cheap", "Provider").textContent, "Anthropic");
  assert.equal(control(w, "cheap", "Model").value, "claude-haiku-4-5");
  assert.match(w.page.querySelectorAll("section .section-note")[1].textContent, /Keys stay in environment variables\.$/);
  // The daemon reports only whether the variable is set, never its value.
  const missing = await X.open("engines", { "discover.agents": found((f) => { f.llm_apis[0].key_present = false; }) });
  assert.deepEqual([item(missing, "cheap").querySelector(".status-line").className, item(missing, "cheap").querySelector(".status-line").textContent],
    ["status-line warn", "ANTHROPIC_API_KEY is not set for the daemon. Reviews with this engine fail until it is."]);
  // Renamed variable since discovery ran: nothing is claimed either way.
  const stale = await X.open("engines", { "discover.agents": found((f) => { f.llm_apis[0].api_key_env = "OLD_KEY"; }) });
  assert.equal(item(stale, "cheap").querySelector(".status-line"), null);
});

test("Add an LLM API: the new entry names a key variable and carries no key", async () => {
  const w = await X.open("engines", X.storing(null, { "discover.agents": found() }).handlers);
  w.byId("add-llm").value = "local";
  w.fire(button(X.rowOf(w, "engines-add-llm"), "Add"), "click");
  assert.deepEqual(draft(w).engines.local, { type: "llm_api", provider: "anthropic", model: "", api_key_env: "ANTHROPIC_API_KEY" });
  X.pickOption(w, control(w, "local", "Provider"), "OpenAI-compatible");
  X.type(w, control(w, "local", "Base URL"), " http://127.0.0.1:11434/v1 ");
  X.type(w, control(w, "local", "Model"), "qwen3");
  X.type(w, control(w, "local", "Key variable"), "LOCAL_LLM_KEY");
  X.type(w, control(w, "local", "Temperature"), "0.2");
  X.type(w, control(w, "local", "Max tokens"), "4096");
  await X.save(w);
  const local = X.lastPut(w).engines.local;
  assert.deepEqual(local, { type: "llm_api", provider: "openai_compatible", model: "qwen3", api_key_env: "LOCAL_LLM_KEY", base_url: "http://127.0.0.1:11434/v1", temperature: 0.2, max_tokens: 4096 });
  // Only the documented keys exist on an LLM API entry.
  assert.deepEqual(Object.keys(local).filter((k) => /key|secret|token/.test(k)).sort(), ["api_key_env", "max_tokens"]);
  // The engine can now review, and be the brief engine.
  w.window.prxApp.go("review");
  assert.ok(X.optionLabels(w, X.field(w, "defaults.brief.engine")).includes("local"));
});

test("LLM API numbers are checked here; emptied optional settings are removed", async () => {
  const w = await X.open("engines", { "discover.agents": found(), "config.get": () => X.config((c) => { Object.assign(c.raw.engines.cheap, { temperature: 0.5, max_tokens: 800, base_url: "https://example.invalid/v1" }); }) });
  assert.equal(control(w, "cheap", "Temperature").value, "0.5");
  assert.equal(control(w, "cheap", "Max tokens").value, "800");
  for (const bad of ["warm", "-1", ".5", "1e1"]) {
    X.type(w, control(w, "cheap", "Temperature"), bad);
    assert.equal(errorOn(w, "cheap"), "Temperature must be a number", bad);
    assert.equal(draft(w).engines.cheap.temperature, 0.5);
  }
  X.type(w, control(w, "cheap", "Temperature"), "0");
  assert.equal(errorOn(w, "cheap"), null);
  assert.equal(draft(w).engines.cheap.temperature, 0);
  for (const bad of ["0", "2.5", "lots"]) {
    X.type(w, control(w, "cheap", "Max tokens"), bad);
    assert.equal(errorOn(w, "cheap"), "Max tokens must be a number", bad);
    assert.equal(draft(w).engines.cheap.max_tokens, 800);
  }
  X.type(w, control(w, "cheap", "Max tokens"), "");
  X.type(w, control(w, "cheap", "Temperature"), "");
  X.type(w, control(w, "cheap", "Base URL"), "");
  assert.equal(errorOn(w, "cheap"), null);
  assert.deepEqual(draft(w).engines.cheap, RAW().engines.cheap);
});

test("renaming an engine follows every setting that names it", async () => {
  const daemon = X.storing((c) => {
    c.raw.defaults.engine = "cheap";
    c.raw.defaults.brief.engine = "cheap";
    c.raw.orgs[0].brief = { engine: "cheap" };
    c.raw.repos[1].engine = "cheap";
    c.raw.engines.other = { type: "agent", command: "other", args: ["{{PROMPT}}"] };
  }, { "discover.agents": found() });
  const w = await X.open("engines", daemon.handlers);
  X.type(w, control(w, "cheap", "Name"), "haiku");
  const d = draft(w);
  // The entry keeps its place in the map and its settings.
  assert.deepEqual(Object.keys(d.engines), ["haiku", "other"]);
  assert.deepEqual(d.engines.haiku, RAW().engines.cheap);
  assert.equal(d.defaults.engine, "haiku");
  assert.equal(d.defaults.brief.engine, "haiku");
  assert.deepEqual(d.orgs[0].brief, { engine: "haiku" });
  assert.equal(d.repos[1].engine, "haiku");
  // A repo naming another engine is left alone.
  assert.equal(d.repos[0].engine, "codex");
  assert.equal(JSON.stringify(d).includes("cheap"), false);
  assert.deepEqual(badges(w, "haiku"), ["Reviewing", "LLM API"]);
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.engine, "haiku");
  // A name already taken is refused; an unchanged or empty name changes nothing.
  X.type(w, control(w, "haiku", "Name"), "other");
  assert.equal(errorOn(w, "haiku"), "An engine named other already exists");
  assert.deepEqual(Object.keys(draft(w).engines), ["haiku", "other"]);
  X.type(w, control(w, "haiku", "Name"), "  ");
  assert.deepEqual(Object.keys(draft(w).engines), ["haiku", "other"]);
  assert.equal(control(w, "haiku", "Name").value, "haiku");
});

test("removing an engine removes only its entry", async () => {
  const w = await X.open("engines", X.storing(null, { "discover.agents": found() }).handlers);
  w.fire(button(item(w, "cheap"), "Remove"), "click");
  assert.deepEqual(draft(w).engines, {});
  assert.equal(item(w, "cheap"), null);
  await X.save(w);
  const expected = RAW();
  expected.engines = {};
  assert.deepEqual(X.lastPut(w), expected);
});

test("a daemon error on an engine lands on that engine", async () => {
  const w = await X.open("general", { "discover.agents": found(),
    "config.put": () => { throw bridgeError("rejected", "engines.cheap.model is required for llm_api engines", "engines.cheap.model"); } });
  X.type(w, X.field(w, "defaults.poll_interval_minutes"), "5");
  await X.save(w);
  assert.equal(w.window.prxApp.state.page, "engines");
  assert.equal(errorOn(w, "cheap"), "engines.cheap.model is required for llm_api engines");
  assert.ok(item(w, "cheap").classList.contains("invalid"));
  assert.equal(item(w, "pi").classList.contains("invalid"), false);
});

test("engine names and commands from the config are shown as text", async () => {
  const evil = X.EVIL.replace(/\s/g, "_");
  const w = await X.open("engines", { "discover.agents": found(), "config.get": () => X.config((c) => {
    c.raw.engines[evil] = { type: "agent", command: X.EVIL, args: [X.EVIL] };
    c.raw.engines["llm" + evil] = { type: "llm_api", provider: X.EVIL, model: X.EVIL, api_key_env: "K" };
  }) });
  assert.equal(item(w, evil).querySelector(".agent-name").textContent, evil);
  assert.equal(control(w, evil, "Command").value, X.EVIL);
  assert.deepEqual(X.markupIn(w.page), []);
  assert.deepEqual(X.activeAttributes(w.page), []);
  assert.equal(walk(w.page).some((n) => n.tagName === "INPUT" && n.getAttribute("type") === "password"), false);
});
