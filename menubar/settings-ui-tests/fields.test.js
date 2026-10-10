// Unit tests for the settings UI's pure helpers (run by `node --test`,
// and from `cargo test -p prx-menubar` when node is installed).
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const F = require("../settings-ui/fields.js");
const S = require("../settings-ui/schema.js");

function draft() {
  return {
    defaults: {
      engine: "pi", poll_interval_minutes: 15, review_mode: "review comment",
      cooldown: { before_first_review_minutes: 60, between_re_reviews_minutes: 30 },
      skip: { drafts: true, authors: [], labels: ["wip"], branches: [] },
      browser: "system", identity: {},
    },
    orgs: [{ org: "acme", poll_interval_minutes: 30, cooldown: { before_first_review_minutes: 5, between_re_reviews_minutes: null } }],
    repos: [{ name: "acme/api" }, { name: "acme/web", poll_interval_minutes: 5 }, { name: "solo/tool" }],
  };
}

test("error fields normalize to the form's input paths", () => {
  const d = draft();
  assert.equal(F.normalizeErrorField("defaults.poll_interval_minutes", d), "defaults.poll_interval_minutes");
  assert.equal(F.normalizeErrorField("engine", d), "defaults.engine");
  assert.equal(F.normalizeErrorField("skip.branches", d), "defaults.skip.branches");
  assert.equal(F.normalizeErrorField("repos[1].poll_interval_minutes", d), "repos[acme/web].poll_interval_minutes");
  assert.equal(F.normalizeErrorField("repos[acme/web].schedule", d), "repos[acme/web].schedule");
  assert.equal(F.normalizeErrorField("orgs[0]", d), "orgs[acme]");
  assert.equal(F.normalizeErrorField("repos", d), "repos");
  assert.equal(F.normalizeErrorField(null, d), null);
  d.repos[2].engine = "nope";
  const msg = "unknown engine 'nope': define it under `engines:` in the config";
  assert.equal(F.normalizeErrorField("engine", d, msg), "repos[solo/tool].engine");
  d.defaults.engine = "nope";
  assert.equal(F.normalizeErrorField("engine", d, msg), "defaults.engine");
  assert.equal(F.normalizeErrorField("engines.cheap.model", d), "engines.cheap.model");
});

test("matchInput finds the exact input, a child, or the nearest ancestor", () => {
  const inputs = ["defaults.poll_interval_minutes", "defaults.identity.login", "repos", "repos[a/b.js]", "repos[a/b.js].schedule"];
  assert.equal(F.matchInput("defaults.poll_interval_minutes", inputs), "defaults.poll_interval_minutes");
  assert.equal(F.matchInput("defaults.identity", inputs), "defaults.identity.login");
  assert.equal(F.matchInput("repos[a/b.js].schedule", inputs), "repos[a/b.js].schedule");
  assert.equal(F.matchInput("repos[a/b.js].browser", inputs), "repos[a/b.js]");
  assert.equal(F.matchInput("repos[x/y].engine", inputs), "repos");
  assert.equal(F.matchInput("engines.foo.command", inputs), null);
});

test("pageForField routes errors to the page (and entry) that edits them", () => {
  const page = (p) => S.pageForField(p, F.parsePath);
  assert.deepEqual(page("defaults.poll_interval_minutes"), { page: "general" });
  assert.deepEqual(page("defaults.cooldown.before_first_review_minutes"), { page: "review" });
  assert.deepEqual(page("defaults.schedule"), { page: "schedule" });
  assert.deepEqual(page("defaults.identity.credential"), { page: "identity" });
  assert.deepEqual(page("defaults.notifications"), { page: "notifications" });
  assert.deepEqual(page("notification_tiers.urgent.sound"), { page: "notifications" });
  assert.deepEqual(page("engines.foo"), { page: "engines" });
  assert.deepEqual(page("defaults.comments.template"), { page: "messages" });
  assert.deepEqual(page("defaults.brief.engine"), { page: "review" });
  assert.deepEqual(page("repos[acme/web].schedule"), { page: "repos", entity: { kind: "repo", name: "acme/web" } });
  assert.deepEqual(page("orgs[acme].identity.login"), { page: "repos", entity: { kind: "org", name: "acme" } });
  assert.deepEqual(page("repos"), { page: "repos", entity: null });
});

test("effective values resolve defaults <- org <- repo with their source", () => {
  const d = draft();
  assert.deepEqual(F.resolveEffective(d, "repo", "acme/api", "poll_interval_minutes"), { value: 30, source: "org", org: "acme" });
  assert.deepEqual(F.resolveEffective(d, "repo", "acme/web", "poll_interval_minutes"), { value: 5, source: "repo", org: null });
  assert.deepEqual(F.resolveEffective(d, "repo", "solo/tool", "poll_interval_minutes"), { value: 15, source: "defaults", org: null });
  // Partial structures merge per leaf: a null org leaf falls through.
  assert.equal(F.resolveEffective(d, "repo", "acme/api", "cooldown.before_first_review_minutes").source, "org");
  assert.deepEqual(F.resolveEffective(d, "repo", "acme/api", "cooldown.between_re_reviews_minutes"), { value: 30, source: "defaults", org: null });
  assert.equal(F.resolveEffective(d, "org", "acme", "poll_interval_minutes").source, "org");
  assert.deepEqual(F.resolveEffective(d, "repo", "acme/api", "identity.login"), { value: null, source: "defaults", org: null });
  assert.equal(F.sourceLabel({ source: "org", org: "acme" }, "repo"), "Org acme");
  assert.equal(F.sourceLabel({ source: "repo" }, "repo"), "This repo");
  assert.equal(F.sourceLabel({ source: "defaults" }, "org"), "Default");
});

test("override set and reset restore inheritance without leaving empty partials", () => {
  const d = draft();
  const repo = F.findEntry(d, "repo", "acme/api");
  F.setAt(repo, "cooldown.between_re_reviews_minutes", 7);
  assert.deepEqual(F.resolveEffective(d, "repo", "acme/api", "cooldown.between_re_reviews_minutes"), { value: 7, source: "repo", org: null });
  F.unsetAt(repo, "cooldown.between_re_reviews_minutes");
  assert.deepEqual(repo, { name: "acme/api" });
  const org = F.findEntry(d, "org", "acme");
  F.unsetAt(org, "cooldown.before_first_review_minutes");
  assert.equal(org.cooldown, undefined, "all-null partial is pruned");
  F.unsetAt(repo, "skip.labels"); // unset of something never set is a no-op
  assert.deepEqual(repo, { name: "acme/api" });
});

test("orgs of watched repos are creatable once", () => {
  const d = draft();
  assert.deepEqual(F.orgsOfRepos(d.repos), ["acme", "solo"]);
  assert.deepEqual(F.creatableOrgs(d), ["solo"]);
});

test("form value parsing", () => {
  assert.deepEqual(F.parseNumber("15", 1), { ok: true, value: 15 });
  assert.equal(F.parseNumber("0", 1).ok, false);
  assert.equal(F.parseNumber("1.5").ok, false);
  assert.equal(F.parseNumber("").ok, false);
  assert.deepEqual(F.splitList("alice, bob,,\ncarol "), ["alice", "bob", "carol"]);
  assert.deepEqual(F.splitLines("-p\n{{PROMPT}}\n\n"), ["-p", "{{PROMPT}}"]);
  assert.deepEqual(F.browserToForm("system"), { kind: "system", text: "", option: "system", entry: null, missing: false });
  // No catalog yet: an app value is shown as itself, not flagged missing.
  assert.deepEqual(F.browserToForm("Firefox"), { kind: "app", text: "Firefox", option: "value", entry: null, missing: false });
  assert.deepEqual(F.browserToForm(["firefox", "{{URL}}"]), { kind: "command", text: "firefox\n{{URL}}", option: "command", entry: null, missing: false });
  assert.deepEqual(F.browserFromForm("command", "firefox\n--new-tab\n{{URL}}"), ["firefox", "--new-tab", "{{URL}}"]);
  assert.equal(F.browserFromForm("app", " Arc "), "Arc");
  assert.equal(F.browserFromForm("system", "ignored"), "system");
  assert.equal(F.hasYamlComments("# prx\ndefaults: {}\n"), true);
  assert.equal(F.hasYamlComments("defaults:\n  agent: pi\n"), false);
});

test("every schema field sits on a real page", () => {
  const pages = S.PAGES.map((p) => p[0]);
  S.FIELDS.forEach((f) => assert.ok(pages.includes(f.page), f.key));
  assert.equal(new Set(S.FIELDS.map((f) => f.key)).size, S.FIELDS.length);
});

test("review prompt is edited as text from the shipped default (design F5)", () => {
  const shipped = "# shipped\nReview {{REPO}}\n";
  assert.equal(F.promptToForm(null, shipped), shipped);
  assert.equal(F.promptToForm(undefined, shipped), shipped);
  assert.equal(F.promptToForm(" Default ", shipped), shipped);
  assert.equal(F.promptToForm("mine {{REPO}}", shipped), "mine {{REPO}}");
  assert.equal(F.promptFromForm("  \n", shipped), null);
  assert.equal(F.promptFromForm(shipped + "\n\n", shipped), "default");
  assert.equal(F.promptFromForm("# shipped\nReview {{REPO}} strictly\n", shipped), "# shipped\nReview {{REPO}} strictly\n");
  const prompt = S.FIELDS.find((f) => f.key === "review_prompt");
  assert.equal(prompt.type, "prompt");
  const extra = S.FIELDS.find((f) => f.key === "extra_prompt");
  assert.equal(extra.type, "textarea");
  assert.ok(extra.optional);
});

test("extra_prompt resolves most-specific-wins like the daemon", () => {
  const d = draft();
  d.defaults.extra_prompt = "D";
  d.orgs[0].extra_prompt = "O";
  d.repos[1].extra_prompt = "R";
  assert.deepEqual(F.resolveEffective(d, "repo", "acme/api", "extra_prompt"), { value: "O", source: "org", org: "acme" });
  assert.equal(F.resolveEffective(d, "repo", "acme/web", "extra_prompt").value, "R");
  assert.equal(F.resolveEffective(d, "repo", "solo/tool", "extra_prompt").source, "defaults");
  F.unsetAt(d.repos[1], "extra_prompt");
  assert.equal(F.resolveEffective(d, "repo", "acme/web", "extra_prompt").value, "O");
});

test("message templates show the built-in text and unset when it is typed back", () => {
  const shipped = "— automated review by prx";
  assert.equal(F.templateToForm(null, shipped), shipped);
  assert.equal(F.templateToForm("Ping @maya", shipped), "Ping @maya");
  assert.equal(F.templateToForm(null, ""), "");
  assert.equal(F.templateFromForm("  ", shipped), null);
  assert.equal(F.templateFromForm(shipped + "\n", shipped), null);
  assert.equal(F.templateFromForm("Reviewed by a bot", shipped), "Reviewed by a bot");
  assert.equal(F.templateFromForm("LGTM", ""), "LGTM");
  assert.equal(F.renderTemplate("## {{PR}} — {{VERDICT}} {{OTHER}}", { PR: "a/b#1", VERDICT: "approved" }), "## a/b#1 — approved {{OTHER}}");
  for (const key of ["review_heading", "identity.attribution_footer", "block_message", "approve_message"]) {
    const field = S.FIELDS.find((f) => f.key === key);
    assert.equal(field.type, "template", key);
    assert.equal(field.page, "messages", key);
  }
});

test("the repo picker filters by text, by regex, and by /pattern/", () => {
  const names = ["acme/api", "acme/api-gateway", "acme/web", "globex/platform"];
  const pick = (m) => names.filter(m.test);
  assert.deepEqual(pick(F.repoMatcher("", false)), names);
  assert.deepEqual(pick(F.repoMatcher("ACME api", false)), ["acme/api", "acme/api-gateway"]);
  assert.deepEqual(pick(F.repoMatcher("^acme/(api|web)$", true)), ["acme/api", "acme/web"]);
  assert.deepEqual(pick(F.repoMatcher("/^globex\\//", false)), ["globex/platform"]);
  assert.equal(F.repoMatcher("/x/", false).regex, true);
  const bad = F.repoMatcher("acme/(", true);
  assert.equal(bad.ok, false);
  assert.ok(bad.error.length > 0);
  assert.equal(F.validRepoName("acme/api"), true);
  for (const name of ["acme", "acme/", "/api", "a/b/c", "a b/c", ""]) assert.equal(F.validRepoName(name), false, name);
});

test("watched repos group under their org, with org-only entries last", () => {
  const d = draft();
  d.orgs.push({ org: "empty-org" });
  const groups = F.groupRepos(d);
  assert.deepEqual(groups.map((g) => g.org), ["acme", "solo", "empty-org"]);
  assert.deepEqual(groups[0].repos.map((r) => r.name), ["acme/api", "acme/web"]);
  assert.equal(groups[0].entry, d.orgs[0]);
  assert.equal(groups[1].entry, null);
  assert.deepEqual(groups[2].repos, []);
});

test("the agent catalog layers config entries over what the daemon found", () => {
  const found = {
    agents: [
      { name: "claude", label: "Claude Code", command: "claude", args: ["-p", "{{PROMPT}}"], model_args: ["--model", "{{MODEL}}"], models: ["opus"], builtin: true, path: "/bin/claude", on_path: true },
      { name: "gemini", label: "Gemini CLI", command: "gemini", args: ["-p", "{{PROMPT}}"], model_args: ["-m", "{{MODEL}}"], models: [], builtin: false, on_path: false },
    ],
    configured: { mine: "/opt/mine" },
  };
  const all = F.agentCatalog(found, { mine: { command: "mine", args: ["-"] } }, ["claude"]);
  assert.deepEqual(all.map((a) => [a.name, a.usable, a.configured]), [["claude", true, false], ["gemini", false, false], ["mine", true, true]]);
  assert.equal(all[0].path, "/bin/claude");
  assert.equal(all[2].path, "/opt/mine");
  // A customized built-in keeps its label and suggestions but runs the entry.
  const custom = F.agentCatalog(found, { claude: { command: "/x/claude", args: ["{{PROMPT}}"] } }, ["claude"])[0];
  assert.equal(custom.command, "/x/claude");
  assert.deepEqual(custom.model_args, []);
  assert.deepEqual(custom.models, ["opus"]);
  // Offline: the built-ins are still selectable.
  assert.deepEqual(F.agentCatalog(null, {}, ["pi", "claude"]).map((a) => [a.name, a.usable, a.detected]), [["pi", true, false], ["claude", true, false]]);
  assert.equal(F.commandLine({ command: "codex", args: ["exec", "-"], model_args: ["-m", "{{MODEL}}"] }, "gpt-5"), "codex exec -m gpt-5  (prompt on stdin)");
  assert.equal(F.commandLine({ command: "pi", args: ["-p", "{{PROMPT}}"], model_args: ["--model", "{{MODEL}}"] }, null), "pi -p {{PROMPT}}");
});

test("the engine catalog offers agents and configured LLM API engines", () => {
  const engines = {
    mine: { type: "agent", command: "mine", args: ["-"] },
    cheap: { type: "llm_api", provider: "anthropic", model: "claude-haiku", api_key_env: "ANTHROPIC_API_KEY" },
  };
  const found = {
    agents: [{ name: "claude", label: "Claude Code", command: "claude", args: ["-p", "{{PROMPT}}"], model_args: [], models: [], builtin: true, on_path: true }],
    configured: { mine: null },
    llm_apis: [{ name: "cheap", provider: "anthropic", model: "claude-haiku", api_key_env: "ANTHROPIC_API_KEY", key_present: false }],
  };
  assert.deepEqual(Object.keys(F.agentEntries(engines)), ["mine"]);
  const all = F.engineCatalog(found, engines, ["claude"]);
  assert.deepEqual(all.map((e) => [e.name, e.type, e.usable]), [["claude", "agent", true], ["mine", "agent", true], ["cheap", "llm_api", true]]);
  assert.equal(all[2].key_present, false);
  assert.equal(all[2].model, "claude-haiku");
  // Without discovery the key state is unknown, not "missing".
  assert.equal(F.llmEngines(engines, null)[0].key_present, null);
  assert.equal(F.engineType({ command: "x" }), "agent");
});

test("global fields are never offered as org/repo overrides", () => {
  assert.ok(S.FIELDS.some((f) => f.key === "notifications.enabled"));
  assert.ok(!S.OVERRIDE_FIELDS.some((f) => f.key === "notifications.enabled"));
  assert.ok(S.OVERRIDE_FIELDS.some((f) => f.key === "comments.inline"));
});

test("the Brief section (task 4.4): labels and help, per-org/repo overridable", () => {
  const brief = S.FIELDS.filter((f) => f.key.startsWith("brief."));
  assert.deepEqual(brief.map((f) => [f.key, f.label, f.type, f.page, f.section]), [
    ["brief.enabled", "Write briefs", "bool", "review", "Brief"],
    ["brief.engine", "Brief engine", "engine", "review", "Brief"],
    ["brief.timeout_seconds", "Brief time limit", "number", "review", "Brief"],
  ]);
  // The help says what a brief is.
  assert.match(brief[0].help, /2–5 plain sentences on what a PR does and why/);
  assert.equal(brief[1].optional, true);
  assert.ok(brief.every((f) => S.OVERRIDE_FIELDS.includes(f)));
  // Nothing in the schema still calls it an executive summary.
  assert.ok(S.FIELDS.every((f) => !/executive summary|summary engine/i.test([f.section, f.label, f.help].join(" "))));
});

test("the menu bar section: two global top-level choice fields (task 1.3)", () => {
  const sort = S.FIELDS.find((f) => f.key === "menu.sort");
  const group = S.FIELDS.find((f) => f.key === "menu.group_by");
  assert.ok(sort && group);
  for (const f of [sort, group]) {
    assert.equal(f.type, "choice");
    assert.equal(f.global, true);
    assert.equal(f.top, true);
    assert.equal(f.page, "menubar");
    assert.equal(f.section, "Menu bar");
    assert.ok(!S.OVERRIDE_FIELDS.includes(f));
    assert.ok(f.options.every((o) => o.label && o.label !== o.value));
  }
  assert.equal(sort.label, "Sort PRs by");
  assert.equal(group.label, "Group PRs by");
  assert.deepEqual(sort.options.map((o) => o.value), ["created_desc", "created_asc", "last_review_desc", "key"]);
  assert.deepEqual(group.options.map((o) => o.value), ["lifecycle", "repo", "none"]);
  assert.equal(sort.fallback, "created_desc");
  assert.equal(group.fallback, "lifecycle");
  // Errors on the top-level section stay there and land on its page.
  assert.equal(F.normalizeErrorField("menu.sort", draft()), "menu.sort");
  assert.deepEqual(S.pageForField("menu.group_by", F.parsePath), { page: "menubar" });
  const d = draft();
  F.setAt(d, "menu.sort", "key");
  assert.deepEqual(d.menu, { sort: "key" });
  assert.equal(d.defaults.menu, undefined);
});

test("usage_output is edited as `field = path` lines (task 8.3)", () => {
  assert.deepEqual(S.USAGE_FIELDS, ["input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "cost_usd", "model", "session_id"]);
  const text = F.usageOutputToText({ output_tokens: "usage.out", input_tokens: "usage.in", session_id: "sid" }, S.USAGE_FIELDS);
  assert.equal(text, "input_tokens = usage.in\noutput_tokens = usage.out\nsession_id = sid");
  assert.deepEqual(F.parseUsageOutput(text + "\n\n", S.USAGE_FIELDS).value, { input_tokens: "usage.in", output_tokens: "usage.out", session_id: "sid" });
  assert.deepEqual(F.parseUsageOutput("cost_usd: total_cost_usd", S.USAGE_FIELDS), { ok: true, value: { cost_usd: "total_cost_usd" } });
  assert.deepEqual(F.parseUsageOutput("  \n", S.USAGE_FIELDS), { ok: true, value: null });
  assert.equal(F.usageOutputToText(undefined, S.USAGE_FIELDS), "");
  assert.match(F.parseUsageOutput("tokens_in = x", S.USAGE_FIELDS).error, /Unknown usage field tokens_in/);
  assert.match(F.parseUsageOutput("input_tokens usage", S.USAGE_FIELDS).error, /field = json.path/);
  assert.match(F.parseUsageOutput("input_tokens = a..b", S.USAGE_FIELDS).error, /Invalid JSON path/);
  assert.match(F.parseUsageOutput("model = a\nmodel = b", S.USAGE_FIELDS).error, /mapped twice/);
});

test("navigation groups Overview before Settings and opens on #page= (design D16)", () => {
  const groups = S.navGroups();
  assert.deepEqual(groups.map((g) => g.label), ["Overview", "Settings"]);
  assert.deepEqual(groups[0].pages.map((p) => p.id), ["dashboard", "prs", "history", "usage"]);
  assert.deepEqual(groups[0].pages.map((p) => p.label), ["Dashboard", "Pull requests", "History", "Usage & stats"]);
  assert.equal(groups[1].pages[0].id, "general");
  assert.ok(groups[1].pages.some((p) => p.id === "menubar"));
  assert.ok(S.PAGES.every((p) => p[3] === "overview" || p[3] === "settings"));
  assert.equal(new Set(S.PAGES.map((p) => p[0])).size, S.PAGES.length);
  assert.equal(S.pageGroup("usage"), "overview");
  assert.equal(S.pageGroup("raw"), "settings");
  assert.equal(S.pageFromHash("#page=dashboard"), "dashboard");
  assert.equal(S.pageFromHash("#x=1&page=general"), "general");
  assert.equal(S.pageFromHash("#page=nope"), null);
  assert.equal(S.pageFromHash(""), null);
  // Settings fields never sit on an Overview page.
  S.FIELDS.forEach((f) => assert.equal(S.pageGroup(f.page), "settings", f.key));
});

// ---- browser picker (spec browser-picker, design W1/W2) -------------------

function browserEntry(name, path, extra) {
  return Object.assign({ name: name, bundle_id: "id." + name.toLowerCase().replace(/\W/g, ""), path: path, icon_png_base64: "PNG" + name, is_default: false }, extra || {});
}
const CATALOG = {
  browsers: [
    browserEntry("Arc", "/Applications/Arc.app", { is_default: true }),
    browserEntry("Firefox", "/Applications/Firefox.app"),
    browserEntry("Safari", "/Applications/Safari.app"),
    browserEntry("Safari", "/Users/me/Applications/Safari.app", { bundle_id: "id.safaritp" }),
  ],
  extra: [browserEntry("Zen", "/Applications/Zen.app", { bundle_id: "app.zen" })],
};

test("browser values map to the discovered entries", () => {
  // A unique name matches by name (case-insensitively, `.app` optional).
  for (const value of ["Firefox", "firefox", "Firefox.app"]) {
    const form = F.browserToForm(value, CATALOG);
    assert.equal(form.kind, "app");
    assert.equal(form.option, "app:/Applications/Firefox.app", value);
    assert.equal(form.entry.name, "Firefox");
    assert.equal(form.missing, false);
    assert.equal(form.text, value, "the saved value is kept verbatim");
  }
  // A path matches its entry (trailing slash ignored), including extras.
  assert.equal(F.browserToForm("/Applications/Arc.app/", CATALOG).option, "app:/Applications/Arc.app");
  assert.equal(F.browserToForm("/Applications/Zen.app", CATALOG).entry.name, "Zen");
  // system / commands are unchanged by the catalog.
  assert.equal(F.browserToForm("System", CATALOG).option, "system");
  assert.equal(F.browserToForm(["x", "{{URL}}"], CATALOG).option, "command");
});

test("name vs path rule: names when unique among discovered browsers, else paths", () => {
  const [arc, firefox, safari, safariTp] = CATALOG.browsers;
  assert.equal(F.browserValue(arc, CATALOG.browsers), "Arc");
  assert.equal(F.browserValue(firefox, CATALOG.browsers), "Firefox");
  assert.equal(F.browserValue(safari, CATALOG.browsers), "/Applications/Safari.app");
  assert.equal(F.browserValue(safariTp, CATALOG.browsers), "/Users/me/Applications/Safari.app");
  // An app that is not a discovered browser saves its path.
  assert.equal(F.browserValue(CATALOG.extra[0], CATALOG.browsers), "/Applications/Zen.app");
  // "Other app…" picking a discovered browser saves like picking it.
  assert.equal(F.browserChosenValue(browserEntry("Firefox", "/Applications/Firefox.app"), CATALOG), "Firefox");
  assert.equal(F.browserChosenValue(browserEntry("Firefox", "/Volumes/X/Firefox.app"), CATALOG), "Firefox", "same bundle id");
  assert.equal(F.browserChosenValue(browserEntry("Zen", "/Applications/Zen.app/", { bundle_id: "app.zen" }), CATALOG), "/Applications/Zen.app");
  assert.equal(F.browserChosenValue(browserEntry("Zen", "/Applications/Zen.app"), null), "/Applications/Zen.app");
});

test("an unknown saved app is preserved and shown as (not found)", () => {
  const form = F.browserToForm("Brave", CATALOG);
  assert.deepEqual(form, { kind: "app", text: "Brave", option: "value", entry: null, missing: true });
  const options = F.browserOptions(CATALOG, "Brave");
  const shown = options.find((o) => o.id === form.option);
  assert.equal(shown.label, "Brave (not found)");
  assert.equal(shown.value, "Brave", "re-picking it keeps the value");
  // A stale path too.
  assert.equal(F.browserOptions(CATALOG, "/Applications/Gone.app").find((o) => o.id === "value").label, "/Applications/Gone.app (not found)");
  // Known values get no such option.
  assert.equal(F.browserOptions(CATALOG, "Firefox").some((o) => o.id === "value"), false);
});

test("the dropdown lists system default, browsers, other app and custom command", () => {
  const options = F.browserOptions(CATALOG, "system");
  assert.deepEqual(options.map((o) => o.label), [
    "System default (Arc)", "Arc", "Firefox", "Safari", "Safari", "Zen", "Other app\u2026", "Custom command\u2026",
  ]);
  assert.deepEqual(options.map((o) => o.value), [
    "system", "Arc", "Firefox", "/Applications/Safari.app", "/Users/me/Applications/Safari.app", "/Applications/Zen.app", undefined, undefined,
  ]);
  assert.equal(options[1].icon, "PNGArc");
  // Discovery unavailable: still usable.
  assert.deepEqual(F.browserOptions(null, "system").map((o) => o.id), ["system", "other", "command"]);
});

test("org and repo levels offer Inherit", () => {
  assert.deepEqual(F.browserToForm(null, CATALOG, { inherit: true }), { kind: "inherit", text: "", option: "inherit", entry: null, missing: false });
  assert.deepEqual(F.browserToForm(undefined, CATALOG, { inherit: true }).kind, "inherit");
  // Without opts.inherit (defaults), unset means the system default.
  assert.equal(F.browserToForm(null, CATALOG).kind, "system");
  const options = F.browserOptions(CATALOG, null, { inherit: true, inheritLabel: F.browserLabel("Firefox", CATALOG) });
  assert.equal(options[0].id, "inherit");
  assert.equal(options[0].label, "Inherit (Firefox)");
  assert.equal(F.browserOptions(CATALOG, "system").some((o) => o.id === "inherit"), false);
  assert.equal(F.browserLabel("system", CATALOG), "System default (Arc)");
  assert.equal(F.browserLabel(["x", "{{URL}}"], CATALOG), "Custom command");
  assert.equal(F.browserLabel("Brave", CATALOG), "Brave");
});

test("browser helpers: extras, placeholders and config paths", () => {
  const zen = browserEntry("Zen", "/Applications/Zen.app");
  const base = { browsers: CATALOG.browsers, extra: [] };
  const added = F.withExtraBrowser(base, zen);
  assert.equal(added.extra.length, 1);
  assert.equal(F.withExtraBrowser(added, zen), added, "not added twice");
  assert.equal(F.withExtraBrowser(base, CATALOG.browsers[1]), base, "discovered browsers are not extras");
  assert.equal(F.withExtraBrowser(null, zen).extra[0], zen);
  assert.equal(F.urlPlaceholders(["firefox", "{{URL}}"]), 1);
  assert.equal(F.urlPlaceholders(["firefox"]), 0);
  assert.equal(F.urlPlaceholders(["a{{URL}}", "{{URL}}"]), 2);
  assert.deepEqual(F.browserPaths({
    defaults: { browser: "/Applications/Zen.app" },
    orgs: [{ org: "acme", browser: "Firefox" }],
    repos: [{ name: "a/b", browser: "/Applications/Zen.app" }, { name: "a/c", browser: ["x", "{{URL}}"] }, { name: "a/d", browser: "/opt/X.app" }],
  }), ["/Applications/Zen.app", "/opt/X.app"]);
});

test("unset tiers and choices have a built-in value to show (audit P1)", () => {
  const S2 = require("../settings-ui/schema.js");
  const tiers = S2.FIELDS.filter((f) => f.type === "tier");
  assert.equal(tiers.length, 10);
  tiers.forEach((f) => assert.ok(f.options.some((o) => o.value === f.fallback), f.key));
  const auto = S2.FIELDS.find((f) => f.key === "auto_open");
  assert.equal(auto.fallback, "off");
  assert.equal(S2.FIELDS.find((f) => f.key === "notifications.on_review_complete_blocking").fallback, "urgent");
});
