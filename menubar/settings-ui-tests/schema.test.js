// The schema table (schema.js) every settings page renders from: each field
// is well formed for its type, option sets are closed and labelled, error
// paths route to a page, and nothing in it holds a secret or lets the
// output contract be edited.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const S = require("../settings-ui/schema.js");
const F = require("../settings-ui/fields.js");
const { boot, plain } = require("./fake-dom.js");

// The types form.js `control` has a branch for (anything else falls to a
// plain text input, which would silently mis-edit a structured value).
const TYPES = ["number", "text", "textarea", "prompt", "template", "choice", "mode", "tier", "sound", "bool", "tokens", "engine", "model", "schedule", "browser"];
const WITH_OPTIONS = ["choice", "mode", "tier"];
const field = (key) => S.FIELDS.find((f) => f.key === key);

test("every field has a known type, a label, a page and a section", () => {
  S.FIELDS.forEach((f) => {
    assert.ok(TYPES.includes(f.type), f.key + ": type " + f.type);
    assert.ok(f.label && f.label.trim() === f.label, f.key + ": label");
    assert.ok(f.section, f.key + ": section");
    assert.match(f.key, /^[a-z_]+(\.[a-z_]+)*$/, f.key);
    if (f.help !== undefined) assert.ok(f.help.length > 0, f.key + ": empty help");
  });
  // Every type the form implements is in use, so none is dead code.
  assert.deepEqual(Array.from(new Set(S.FIELDS.map((f) => f.type))).sort(), TYPES.slice().sort());
});

test("option fields carry a closed, labelled set that includes their fallback", () => {
  S.FIELDS.forEach((f) => {
    if (!WITH_OPTIONS.includes(f.type)) { assert.equal(f.options, undefined, f.key); return; }
    assert.ok(f.options.length >= 2, f.key);
    const values = f.options.map((o) => o.value);
    assert.equal(new Set(values).size, values.length, f.key + ": duplicate value");
    assert.equal(new Set(f.options.map((o) => o.label)).size, values.length, f.key + ": duplicate label");
    assert.ok(f.options.every((o) => typeof o.value === "string" && o.label), f.key);
    assert.ok(values.includes(f.fallback), f.key + ": fallback " + f.fallback);
  });
});

test("review modes, tiers and events match the daemon's vocabulary", () => {
  assert.deepEqual(S.MODES, ["review comment", "monitor", "off"]);
  assert.deepEqual(S.MODE_OPTIONS.map((o) => o.value), S.MODES);
  assert.deepEqual(field("review_mode").options, S.MODE_OPTIONS);
  assert.equal(field("review_mode").fallback, "review comment");
  // Each mode states three facts, each a plain yes or no.
  S.MODE_OPTIONS.forEach((o) => {
    assert.equal(o.facts.length, 3, o.value);
    assert.ok(o.facts.every((fact) => ["yes", "no"].includes(fact[0]) && fact[1]), o.value);
  });
  assert.deepEqual(S.MODE_OPTIONS.find((o) => o.value === "monitor").facts.map((f) => f[0]), ["yes", "no", "yes"]);
  assert.deepEqual(S.MODE_OPTIONS.find((o) => o.value === "off").facts.map((f) => f[0]), ["no", "no", "no"]);
  assert.deepEqual(S.TIERS, ["urgent", "informational", "silent"]);
  const tiers = S.FIELDS.filter((f) => f.type === "tier");
  assert.deepEqual(tiers.map((f) => f.key), S.EVENTS.map((e) => "notifications." + e[0]));
  tiers.forEach((f, i) => {
    assert.deepEqual(f.options.map((o) => o.value), S.TIERS, f.key);
    assert.equal(f.fallback, S.EVENTS[i][2], f.key);
    assert.equal(f.label, S.EVENTS[i][1], f.key);
  });
  // Failures and blocking verdicts are loud by default; terminal events quiet.
  const tierOf = (key) => field("notifications." + key).fallback;
  assert.equal(tierOf("on_failure"), "urgent");
  assert.equal(tierOf("on_review_complete_blocking"), "urgent");
  assert.equal(tierOf("on_merged"), "silent");
  assert.deepEqual(field("auto_open").options.map((o) => o.value), ["off", "approved", "blocking", "both"]);
  assert.equal(field("auto_open").fallback, "off");
});

test("every event has an optional sound field; only the ready event has a built-in sound", () => {
  const sounds = S.FIELDS.filter((f) => f.type === "sound");
  assert.deepEqual(sounds.map((f) => f.key), S.EVENTS.map((e) => "notifications.sounds." + e[0]));
  sounds.forEach((f, i) => {
    assert.equal(f.label, S.EVENTS[i][1], f.key);
    assert.equal(f.optional, true, f.key);
    assert.equal(f.global, undefined, f.key + ": orgs and repos may override it");
  });
  assert.deepEqual(sounds.filter((f) => f.builtin).map((f) => [f.key, f.builtin]), [["notifications.sounds.on_review_complete_approved", "Glass"]]);
  assert.ok(S.SOUNDS.includes("Glass") && S.SOUNDS.includes("Basso"));
  assert.equal(field("notifications.menubar_bell").fallback, true);
  assert.equal(S.pageForField("defaults.notifications.sounds.on_failure", F.parsePath).page, "notifications");
});

test("engine and LLM provider vocabularies", () => {
  assert.deepEqual(S.BUILTIN_ENGINES, ["pi", "claude", "codex"]);
  assert.deepEqual(S.LLM_PROVIDERS.map((p) => p.value), ["anthropic", "openai", "openai_compatible"]);
  assert.ok(S.LLM_PROVIDERS.every((p) => p.label));
  assert.equal(field("engine").type, "engine");
  assert.equal(field("engine").optional, undefined, "the review engine is required");
  assert.equal(field("brief.engine").optional, true);
  assert.equal(field("agent_model").type, "model");
});

test("the config uses the new names: engine and brief, never agent or executive_summary", () => {
  const keys = S.FIELDS.map((f) => f.key);
  assert.ok(keys.includes("engine"));
  assert.ok(keys.some((k) => k.startsWith("brief.")));
  assert.ok(!keys.includes("agent"));
  assert.ok(!keys.some((k) => /^(agents|executive_summary|exec_summary)(\.|$)/.test(k)), "a legacy key is in the schema");
  // The page that edits the `engines:` map is called Engines.
  assert.deepEqual(S.PAGES.find((p) => p[0] === "engines").slice(0, 2), ["engines", "Engines"]);
  assert.deepEqual(S.pageForField("engines.cheap.model", F.parsePath), { page: "engines" });
});

test("no field holds a secret: credentials are environment-variable names", () => {
  // The only credential-like settings, both plain text and both saying so.
  const sensitive = S.FIELDS.filter((f) => /credential|token|secret|password|api_key/i.test(f.key));
  assert.deepEqual(sensitive.map((f) => f.key), ["identity.credential"]);
  const credential = field("identity.credential");
  assert.equal(credential.type, "text");
  assert.equal(credential.label, "Token variable");
  assert.match(credential.help, /Name of an environment variable/);
  assert.match(credential.help, /Never the token itself/);
  assert.match(credential.placeholder, /^e\.g\. [A-Z_]+$/);
  // Usage mappings name JSON paths, and include no key or secret field.
  assert.ok(S.USAGE_FIELDS.every((name) => !/key|secret|token$/.test(name) || /_tokens$/.test(name)));
});

test("the output contract is not a field: only the prompt before it is editable", () => {
  assert.ok(S.FIELDS.every((f) => !/contract|manifest|output_rules/i.test(f.key)));
  const prompt = field("review_prompt");
  assert.equal(prompt.type, "prompt");
  assert.match(prompt.help, /PRX always adds its output rules and manifest schema at the end/);
  // The manifest path is a placeholder the prompt may mention, nothing more.
  assert.deepEqual(prompt.placeholders, ["PR_NUMBER", "REPO", "MANIFEST_PATH", "PARENT_PROMPT"]);
  assert.equal(S.FIELDS.filter((f) => f.type === "prompt").length, 1);
});

test("template placeholders are a closed set, with the legacy summary alias kept", () => {
  assert.deepEqual(field("review_heading").placeholders, ["PR", "VERDICT"]);
  const body = field("comments.template").placeholders;
  assert.ok(body.includes("BRIEF") && body.includes("EXEC_SUMMARY"));
  assert.equal(new Set(body).size, body.length);
  S.FIELDS.forEach((f) => (f.placeholders || []).forEach((name) => assert.match(name, /^[A-Z_]+$/, f.key)));
  // Only free-text templates offer placeholders.
  S.FIELDS.filter((f) => f.placeholders).forEach((f) => assert.ok(["prompt", "template"].includes(f.type), f.key));
});

test("global and top-level fields stay out of org and repo overrides", () => {
  const global = S.FIELDS.filter((f) => f.global);
  assert.deepEqual(global.map((f) => f.key), ["notifications.enabled", "notifications.menubar_bell", "menu.sort", "menu.group_by"]);
  S.FIELDS.filter((f) => f.top).forEach((f) => assert.equal(f.global, true, f.key));
  assert.equal(S.OVERRIDE_FIELDS.length, S.FIELDS.length - global.length);
  assert.ok(S.OVERRIDE_FIELDS.every((f) => !f.global && !f.top));
  // Everything a repo can reasonably differ in is overridable.
  for (const key of ["review_mode", "engine", "review_prompt", "extra_prompt", "schedule", "skip.drafts", "browser", "brief.enabled"]) {
    assert.ok(S.OVERRIDE_FIELDS.includes(field(key)), key);
  }
});

test("fieldFor finds a field by exact path, by a child path, or by a parent path", () => {
  assert.equal(S.fieldFor("cooldown.before_first_review_minutes").label, "Wait before the first review");
  // A path below a field (an item of a list) is that field.
  assert.equal(S.fieldFor("skip.authors.0").key, "skip.authors");
  // A path above several fields is the first of them.
  assert.equal(S.fieldFor("cooldown").key, "cooldown.before_first_review_minutes");
  assert.equal(S.fieldFor("identity").key, "identity.attribution_footer");
  assert.equal(S.fieldFor("nope"), null);
  // "skip" must not match "skipped…"-style neighbours by plain prefix.
  assert.equal(S.fieldFor("brief").key, "brief.enabled");
  assert.equal(S.fieldFor("brie"), null);
});

test("pageForField: unknown and malformed paths land on General, never throw", () => {
  const page = (p) => S.pageForField(p, F.parsePath);
  assert.deepEqual(page("defaults.no_such_field"), { page: "general" });
  assert.deepEqual(page(""), { page: "general" });
  assert.deepEqual(page("[oops"), { page: "general" });
  assert.deepEqual(page("orgs"), { page: "repos", entity: null });
  assert.deepEqual(page("menu"), { page: "menubar" });
  assert.deepEqual(page("engines"), { page: "engines" });
  assert.deepEqual(page("defaults.browser"), { page: "identity" });
  assert.deepEqual(page("defaults.review_prompt"), { page: "review" });
  assert.deepEqual(page("defaults.skip.authors.0"), { page: "review" });
  // Repo names with dots, dashes and a `#` survive the routing.
  assert.deepEqual(page("repos[acme/api.v2-next].skip.labels"), { page: "repos", entity: { kind: "repo", name: "acme/api.v2-next" } });
});

test("pageFromHash accepts only known page ids", () => {
  assert.equal(S.pageFromHash("#page=prs"), "prs");
  assert.equal(S.pageFromHash("#a=b&page=usage&c=d"), "usage");
  assert.equal(S.pageFromHash("#page="), null);
  assert.equal(S.pageFromHash("#page=<script>"), null);
  assert.equal(S.pageFromHash("#page=__proto__"), null);
  assert.equal(S.pageFromHash("#mypage=prs"), null);
  assert.equal(S.pageFromHash("page=prs"), null);
  assert.equal(S.pageFromHash(null), null);
  assert.equal(S.pageFromHash(undefined), null);
  S.PAGES.forEach((p) => assert.equal(S.pageFromHash("#page=" + p[0]), p[0]));
});

test("in the window the schema is one frozen global", () => {
  const w = boot({ scripts: ["schema.js"] });
  const schema = w.window.prxSchema;
  assert.equal(Object.isFrozen(schema), true);
  assert.equal(schema.FIELDS.length, S.FIELDS.length);
  assert.deepEqual(plain(schema.navGroups().map((g) => g.pages.length)), [4, 10]);
});

test("the PR details page: hidden from the sidebar, opened at a validated PR key", () => {
  assert.equal(S.pageGroup("pr"), "overview");
  assert.equal(S.pageFromHash("#page=pr&pr=acme%2Fapi%2342"), "pr");
  assert.ok(!S.navGroups().some((g) => g.pages.some((p) => p.id === "pr")));
  assert.equal(S.navPage("pr"), "prs");
  assert.equal(S.navPage("usage"), "usage");
  assert.equal(S.navPage("nope"), null);
  assert.equal(S.prFromHash("#page=pr&pr=acme%2Fapi%2342"), "acme/api#42");
  assert.equal(S.prFromHash("#pr=a.b_c-d%2Fx.y%231&page=pr"), "a.b_c-d/x.y#1");
  for (const bad of ["#page=pr", "#page=pr&pr=", "#page=pr&pr=acme%2Fapi", "#page=pr&pr=acme%2Fapi%23x", "#page=pr&pr=%E0%A4%A",
    "#page=pr&pr=" + encodeURIComponent("a/b#1\");alert(1)//"), "#page=pr&pr=" + encodeURIComponent("a/b/c#1"), "#page=pr&xpr=a%2Fb%231", null]) {
    assert.equal(S.prFromHash(bad), null, String(bad));
  }
  assert.equal(S.prKeyValid("acme/api#42"), true);
  assert.equal(S.prKeyValid(42), false);
});
