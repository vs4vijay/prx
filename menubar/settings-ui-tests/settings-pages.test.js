// Every defaults-level settings page (General, Review, Messages, Schedule,
// Notifications, Menu bar, Account & Browser) through the real shell and
// form controls against the fake DOM: what each field type shows for a set
// and an unset value, what an edit puts in the draft, and the exact config a
// save sends. Also the review prompt's read-only output contract, and that
// credentials are only ever environment-variable names.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const S = require("../settings-ui/schema.js");
const X = require("./page-fixtures.js");
const { plain, withText, bridgeError, walk } = require("./fake-dom.js");

const RAW = () => X.config().raw;
const draft = (w) => plain(w.window.prxApp.app.draft());
const button = (root, text) => withText(root, text).find((n) => n.tagName === "BUTTON");
const help = (w, path) => { const h = X.rowOf(w, path).querySelector(".row-head .help"); return h ? h.textContent : null; };
// An event row's tier radios (its on/off switch is a checkbox before them).
const radios = (w, path) => X.rowOf(w, path).querySelectorAll("input").filter((i) => i.getAttribute("type") === "radio");
const tick = (w, ms) => w.intervals.filter((i) => i.ms === ms && !i.cleared).forEach((i) => i.run());

// ---- every page renders every one of its fields ------------------------------

test("each settings page shows exactly its schema fields, in order, grouped by section", async () => {
  for (const page of ["general", "review", "messages", "schedule", "notifications", "menubar", "identity"]) {
    const w = await X.open(page);
    const fields = S.FIELDS.filter((f) => f.page === page);
    const paths = fields.map((f) => (f.top ? f.key : "defaults." + f.key));
    const shown = w.page.querySelectorAll(".row").map((r) => r.getAttribute("data-field")).filter((p) => !/^(app\.|notification_tiers\.)/.test(p));
    assert.deepEqual(shown, paths, page);
    fields.forEach((f, i) => assert.equal(X.rowOf(w, paths[i]).querySelector(".row-label").textContent, f.label, f.key));
    const sections = fields.map((f) => f.section).filter((s, i, all) => all.indexOf(s) === i);
    const titles = w.page.querySelectorAll("section > h2").map((h) => h.textContent);
    assert.deepEqual(titles.filter((t) => sections.includes(t)), sections, page);
    // Every row's error slot starts hidden and empty.
    w.page.querySelectorAll(".row .field-error").forEach((e) => assert.deepEqual([e.hidden, e.textContent], [true, ""]));
  }
});

test("labels name their inputs; dropdowns and groups are named by the row label instead", async () => {
  const w = await X.open("review");
  const label = (path) => X.rowOf(w, path).querySelector(".row-label");
  // A text field: a real <label for>.
  assert.equal(label("defaults.diff_max_files").tagName, "LABEL");
  assert.equal(label("defaults.diff_max_files").getAttribute("for"), "f-defaults_diff_max_files");
  // A dropdown: a span (clicking the label must not open the menu), wired
  // with aria-labelledby.
  assert.equal(label("defaults.engine").tagName, "SPAN");
  assert.equal(X.field(w, "defaults.engine").getAttribute("aria-labelledby"), label("defaults.engine").id);
  const general = await X.open("general");
  const group = X.rowOf(general, "defaults.review_mode").querySelector("[role=radiogroup]");
  assert.equal(group.getAttribute("aria-labelledby"), X.rowOf(general, "defaults.review_mode").querySelector(".row-label").id);
  assert.ok(X.rowOf(general, "defaults.review_mode").classList.contains("bare-row"));
});

// ---- General --------------------------------------------------------------------

test("General: picking a review mode saves it; the scope lists who chose their own", async () => {
  const daemon = X.storing((c) => { c.raw.repos[1].review_mode = "monitor"; c.raw.orgs[0].review_mode = "off"; });
  const w = await X.open("general", daemon.handlers);
  const cards = w.page.querySelectorAll(".mode-card");
  assert.deepEqual(cards.map((c) => [c.dataset.mode, c.querySelector(".mode-title").textContent, c.querySelector("input").checked]),
    [["review comment", "Review and comment", true], ["monitor", "Monitor", false], ["off", "Off", false]]);
  assert.deepEqual(cards[1].querySelectorAll(".mode-fact").map((f) => [f.classList.contains("yes"), f.textContent]),
    [[true, "Reviews run on new PRs and pushes"], [false, "Nothing is posted to GitHub"], [true, "Verdicts in the menu and History only"]]);
  // No card or note claims PRX approves anything.
  assert.match(w.page.querySelector("section .section-note").textContent, /PRX never approves a PR/);
  assert.equal(w.page.querySelector(".scope-title").textContent, "2 exceptions choose their own mode");
  assert.deepEqual(w.page.querySelectorAll(".scope-item").map((b) => b.getAttribute("aria-label")), ["acme org: Off", "acme/web: Monitor"]);
  X.choose(w, cards[1].querySelector("input"));
  assert.equal(draft(w).defaults.review_mode, "monitor");
  await X.save(w);
  const expected = plain(daemon.state.raw);
  assert.equal(expected.defaults.review_mode, "monitor");
  assert.equal(X.lastPut(w).repos[1].review_mode, "monitor", "the exceptions are saved untouched");
  // An exception opens its override editor.
  w.fire(w.page.querySelectorAll(".scope-item")[1], "click");
  assert.equal(w.window.prxApp.state.page, "repos");
  assert.deepEqual(plain(w.window.prxApp.app.ui.selection), { kind: "repo", name: "acme/web" });
});

test("General: an unset review mode shows the default selected and tagged", async () => {
  const w = await X.open("general", { "config.get": () => X.config((c) => { delete c.raw.defaults.review_mode; delete c.effective.defaults.review_mode; }) });
  const cards = w.page.querySelectorAll(".mode-card");
  assert.deepEqual(cards.map((c) => c.querySelector("input").checked), [true, false, false]);
  assert.deepEqual(cards.map((c) => c.querySelectorAll(".select-tag").map((t) => t.textContent)), [["Default"], [], []]);
  assert.equal(help(w, "defaults.review_mode"), "Not set, so PRX uses the default (Review and comment).");
  assert.equal(w.page.querySelector(".scope-title").textContent, "Every org and repo uses this mode");
  // Showing the default is not an edit.
  assert.equal(w.window.prxApp.app.isDirty(), false);
  assert.equal("review_mode" in draft(w).defaults, false);
  // The daemon's effective value wins over the schema's built-in one.
  const merged = await X.open("general", { "config.get": () => X.config((c) => { delete c.raw.defaults.review_mode; c.effective.defaults.review_mode = "monitor"; }) });
  assert.deepEqual(merged.page.querySelectorAll(".mode-card").map((c) => c.querySelector("input").checked), [false, true, false]);
});

test("General: the pause panel reads the daemon, refreshes on its own timer, and stops when left", async () => {
  let paused;
  const w = await X.open("general", { "status.get": () => X.status((s) => { if (paused) s.paused_until = paused; }) });
  const title = () => w.page.querySelector(".pause-section .pause-title").textContent;
  assert.equal(title(), "Not paused");
  paused = new Date(X.NOW + 45 * X.MIN).toISOString();
  tick(w, 30000);
  await w.settle();
  assert.equal(title(), "Paused, 45m left");
  assert.ok(button(w.page.querySelector(".pause-section"), "Resume reviews"));
  // Pause is not a setting: nothing became dirty.
  assert.equal(w.window.prxApp.app.isDirty(), false);
  const calls = w.calls("status.get").length;
  w.window.prxApp.go("review");
  tick(w, 30000);
  await w.settle();
  assert.equal(w.calls("status.get").length, calls, "a page that was left stops asking");
  // Offline: the pause is unknown, and the form still works.
  const off = await X.open("general", { "status.get": X.offline });
  assert.equal(off.page.querySelector(".pause-section .pause-title").textContent, "Pause unknown");
  assert.equal(off.page.querySelector(".pause-section .pause-select"), null);
  assert.equal(X.field(off, "defaults.poll_interval_minutes").value, "10");
});

// ---- Review ---------------------------------------------------------------------

test("Review: engine and model", async () => {
  const w = await X.open("review", X.storing().handlers);
  const engine = X.field(w, "defaults.engine");
  // Built-in agents (the offline fallback list) and the configured LLM API.
  assert.deepEqual(X.optionLabels(w, engine), ["pi", "claude", "codex", "cheap"]);
  assert.equal(engine.textContent, "claude");
  assert.equal(w.byId(engine.id + "-list").querySelectorAll(".select-detail")[3].textContent, "LLM API · Anthropic · claude-haiku-4-5");
  X.pickOption(w, engine, "cheap");
  assert.equal(draft(w).defaults.engine, "cheap");
  // The model row now says the engine has its own model.
  assert.match(X.rowOf(w, "defaults.agent_model").querySelector(".status-line").textContent, /cheap is an LLM API engine with its own model \(claude-haiku-4-5\), so this is ignored/);
  X.pickOption(w, X.field(w, "defaults.engine"), "codex");
  X.type(w, X.field(w, "defaults.agent_model"), "  gpt-5-codex  ");
  assert.equal(draft(w).defaults.agent_model, "gpt-5-codex");
  // Clearing an optional field removes the key instead of saving "".
  X.type(w, X.field(w, "defaults.agent_model"), "   ");
  assert.equal("agent_model" in draft(w).defaults, false);
  await X.save(w);
  const expected = RAW();
  expected.defaults.engine = "codex";
  delete expected.defaults.agent_model;
  assert.deepEqual(X.lastPut(w), expected);
});

test("Review: an engine the config names but nobody defines is kept and flagged", async () => {
  const w = await X.open("review", { "config.get": () => X.config((c) => { c.raw.defaults.engine = "ghost"; }) });
  const engine = X.field(w, "defaults.engine");
  assert.equal(engine.textContent, "ghostNot configured");
  assert.deepEqual(X.optionLabels(w, engine), ["pi", "claude", "codex", "cheap", "ghost"]);
  assert.equal(w.window.prxApp.app.isDirty(), false);
  assert.equal(draft(w).defaults.engine, "ghost");
});

test("Review: the brief engine is optional, and unset means the review engine", async () => {
  const w = await X.open("review", X.storing().handlers);
  const engine = X.field(w, "defaults.brief.engine");
  assert.equal(engine.textContent, "Same as the review engine");
  assert.deepEqual(X.optionLabels(w, engine), ["Same as the review engine", "pi", "claude", "codex", "cheap"]);
  X.pickOption(w, engine, "cheap");
  assert.deepEqual(draft(w).defaults.brief, { enabled: true, timeout_seconds: 120, engine: "cheap" });
  X.pickOption(w, X.field(w, "defaults.brief.engine"), "Same as the review engine");
  assert.deepEqual(draft(w).defaults.brief, { enabled: true, timeout_seconds: 120 });
  X.toggle(w, X.field(w, "defaults.brief.enabled"), false);
  X.type(w, X.field(w, "defaults.brief.timeout_seconds"), "45");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).defaults.brief, { enabled: false, timeout_seconds: 45 });
});

test("Review: the prompt is edited from the shipped text and saved as `default` when unchanged", async () => {
  const w = await X.open("review", X.storing().handlers);
  const area = X.field(w, "defaults.review_prompt");
  assert.equal(area.value, X.SHIPPED_PROMPT);
  assert.equal(area.getAttribute("rows"), "16");
  assert.deepEqual(X.rowOf(w, "defaults.review_prompt").querySelectorAll(".chip").map((c) => c.textContent),
    ["{{PR_NUMBER}}", "{{REPO}}", "{{MANIFEST_PATH}}", "{{PARENT_PROMPT}}"]);
  // The shipped text is showing, so there is nothing to reset to.
  assert.equal(button(X.rowOf(w, "defaults.review_prompt"), "Use built-in").hidden, true);
  X.type(w, area, "Focus on SQL injection in {{REPO}}.\n");
  assert.equal(draft(w).defaults.review_prompt, "Focus on SQL injection in {{REPO}}.\n");
  assert.equal(button(X.rowOf(w, "defaults.review_prompt"), "Use built-in").hidden, false);
  // Typing the shipped text back (trailing space aside) is `default` again.
  X.type(w, area, X.SHIPPED_PROMPT + "  \n");
  assert.equal(draft(w).defaults.review_prompt, "default");
  // Emptying it unsets the key.
  X.type(w, area, "  \n ");
  assert.equal("review_prompt" in draft(w).defaults, false);
  // Use built-in puts the shipped text back in the box.
  X.type(w, area, "custom");
  w.fire(button(X.rowOf(w, "defaults.review_prompt"), "Use built-in"), "click");
  assert.equal(area.value, X.SHIPPED_PROMPT);
  assert.equal("review_prompt" in draft(w).defaults, false);
  await X.save(w);
  assert.equal("review_prompt" in X.lastPut(w).defaults, false);
});

test("Review: a placeholder chip inserts at the cursor and commits", async () => {
  const w = await X.open("review");
  const area = X.field(w, "defaults.review_prompt");
  area.value = "Review  carefully";
  area.setSelectionRange(7, 7);
  w.fire(X.rowOf(w, "defaults.review_prompt").querySelectorAll(".chip")[1], "click");
  assert.equal(area.value, "Review {{REPO}} carefully");
  assert.equal(draft(w).defaults.review_prompt, "Review {{REPO}} carefully");
  assert.equal(w.document.activeElement, area);
  // A selection is replaced.
  area.setSelectionRange(0, 6);
  w.fire(X.rowOf(w, "defaults.review_prompt").querySelectorAll(".chip")[0], "click");
  assert.equal(area.value, "{{PR_NUMBER}} {{REPO}} carefully");
});

test("Review: the output contract is shown read-only and is never part of what is saved", async () => {
  const w = await X.open("review", X.storing().handlers);
  const row = X.rowOf(w, "defaults.review_prompt");
  const preview = row.querySelector("details.contract-preview");
  assert.equal(preview.querySelector("summary").textContent, "Always added by PRX");
  assert.equal(preview.hasAttribute("open"), false);
  const pre = preview.querySelector("pre");
  assert.equal(pre.textContent, X.CONTRACT);
  assert.match(pre.getAttribute("aria-label"), /read-only/);
  assert.match(preview.querySelector("p.help").textContent, /They cannot be edited\./);
  // Nothing inside the preview can be typed into, toggled or removed.
  assert.deepEqual(preview.querySelectorAll("input, textarea, select, button, [contenteditable], [role=combobox]"), []);
  assert.equal(walk(preview).some((n) => Object.keys(n.listeners).length > 0), false);
  // The only editable text is the prompt, and the contract is not in it.
  assert.equal(row.querySelectorAll("textarea").length, 1);
  assert.equal(X.field(w, "defaults.review_prompt").value.includes("prx output contract"), false);
  // No field anywhere on the page edits it either.
  assert.deepEqual(w.page.querySelectorAll(".row").map((r) => r.getAttribute("data-field")).filter((p) => /contract|manifest/.test(p)), []);
  X.type(w, X.field(w, "defaults.review_prompt"), "Be brief.");
  await X.save(w);
  const sent = JSON.stringify(X.lastPut(w));
  assert.equal(sent.includes("prx output contract"), false);
  assert.equal(sent.includes("review_contract"), false);
  assert.equal(X.lastPut(w).defaults.review_prompt, "Be brief.");
  // After the save the preview is still there, unchanged.
  assert.equal(X.rowOf(w, "defaults.review_prompt").querySelector("details.contract-preview pre").textContent, X.CONTRACT);
});

test("Review: a prompt that carries its own schema gets a hint, not an error", async () => {
  const w = await X.open("review", X.storing((c) => { c.raw.defaults.review_prompt = "Write {\"manifest_version\": 1} to {{MANIFEST_PATH}}"; }).handlers);
  const hint = X.rowOf(w, "defaults.review_prompt").querySelector(".prompt-schema-hint");
  assert.equal(hint.hidden, false);
  assert.equal(hint.getAttribute("role"), "status");
  assert.equal(X.errorOf(w, "defaults.review_prompt"), null);
  const area = X.field(w, "defaults.review_prompt");
  area.value = "Plain instructions.";
  w.fire(area, "input");
  assert.equal(hint.hidden, true);
  // The hint never blocks a save.
  area.value = "Keep \"verdict\": \"approved\" in mind";
  w.fire(area, "input");
  w.fire(area, "change");
  assert.equal(hint.hidden, false);
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.review_prompt, "Keep \"verdict\": \"approved\" in mind");
});

test("Review: extra instructions, limits and cooldowns", async () => {
  const w = await X.open("review", X.storing().handlers);
  X.type(w, X.field(w, "defaults.extra_prompt"), "Check migrations.\nAnd " + X.EVIL);
  X.type(w, X.field(w, "defaults.diff_max_lines"), "9000");
  X.type(w, X.field(w, "defaults.cooldown.before_first_review_minutes"), "0");
  await X.save(w);
  const expected = RAW();
  expected.defaults.extra_prompt = "Check migrations.\nAnd " + X.EVIL;
  expected.defaults.diff_max_lines = 9000;
  expected.defaults.cooldown.before_first_review_minutes = 0;
  assert.deepEqual(X.lastPut(w), expected);
  // Text is kept exactly as typed (inner spaces, newlines); only an
  // all-blank value unsets an optional field.
  X.type(w, X.field(w, "defaults.extra_prompt"), " \n ");
  assert.equal("extra_prompt" in draft(w).defaults, false);
});

test("Review: skip lists are edited as chips", async () => {
  const w = await X.open("review", X.storing().handlers);
  const row = (key) => X.rowOf(w, "defaults.skip." + key);
  const chips = (key) => row(key).querySelectorAll(".token > span").map((s) => s.textContent);
  const input = (key) => X.field(w, "defaults.skip." + key);
  assert.deepEqual(chips("authors"), ["dependabot[bot]"]);
  assert.equal(input("labels").placeholder, "Add label");
  assert.equal(input("authors").placeholder, "");
  // Enter adds; a comma list adds each; duplicates and blanks are dropped.
  input("authors").value = "renovate[bot]";
  assert.equal(w.fire(input("authors"), "keydown", { key: "Enter" }).defaultPrevented, true);
  input("authors").value = " alice , dependabot[bot],, bob ";
  w.fire(input("authors"), "keydown", { key: "," });
  assert.deepEqual(chips("authors"), ["dependabot[bot]", "renovate[bot]", "alice", "bob"]);
  assert.deepEqual(draft(w).defaults.skip.authors, ["dependabot[bot]", "renovate[bot]", "alice", "bob"]);
  assert.equal(input("authors").value, "");
  // Leaving the field with text in it adds it too.
  input("labels").value = "wip";
  w.fire(input("labels"), "blur", { bubbles: false });
  assert.deepEqual(draft(w).defaults.skip.labels, ["wip"]);
  // Backspace in an empty input removes the last chip; with text it does not.
  w.fire(input("authors"), "keydown", { key: "Backspace" });
  assert.deepEqual(chips("authors"), ["dependabot[bot]", "renovate[bot]", "alice"]);
  input("authors").value = "x";
  w.fire(input("authors"), "keydown", { key: "Backspace" });
  assert.deepEqual(chips("authors"), ["dependabot[bot]", "renovate[bot]", "alice"]);
  input("authors").value = "";
  // Each chip has its own named remove button.
  const remove = row("authors").querySelectorAll(".token button");
  assert.deepEqual(remove.map((b) => b.getAttribute("aria-label")), ["Remove dependabot[bot]", "Remove renovate[bot]", "Remove alice"]);
  w.fire(remove[0], "click");
  assert.deepEqual(draft(w).defaults.skip.authors, ["renovate[bot]", "alice"]);
  // Hostile input is one more chip of text.
  input("branches").value = X.EVIL;
  w.fire(input("branches"), "keydown", { key: "Enter" });
  assert.deepEqual(chips("branches"), [X.EVIL]);
  assert.deepEqual(X.markupIn(w.page), []);
  X.toggle(w, X.field(w, "defaults.skip.drafts"), false);
  await X.save(w);
  assert.deepEqual(X.lastPut(w).defaults.skip, { drafts: false, authors: ["renovate[bot]", "alice"], labels: ["wip"], branches: [X.EVIL] });
  // Removing the last chip saves an empty list, not a missing key.
  w.fire(X.rowOf(w, "defaults.skip.labels").querySelector(".token button"), "click");
  assert.deepEqual(draft(w).defaults.skip.labels, []);
  assert.equal(X.field(w, "defaults.skip.labels").placeholder, "Add label");
});

// ---- Messages -------------------------------------------------------------------

test("Messages: templates show the built-in text and save only what differs from it", async () => {
  const w = await X.open("messages", X.storing().handlers);
  const heading = X.field(w, "defaults.review_heading");
  assert.equal(heading.value, "## Automated review of {{PR}}: {{VERDICT}}");
  assert.equal(X.field(w, "defaults.identity.attribution_footer").value, "Reviewed by prx.");
  assert.equal(X.field(w, "defaults.block_message").value, "Blocked pending a human follow-up.");
  assert.equal(X.field(w, "defaults.approve_message").value, "");
  assert.equal(X.field(w, "defaults.approve_message").placeholder, "No comment. The approval is posted on its own.");
  X.type(w, heading, "# {{VERDICT}} for {{PR}}");
  X.type(w, X.field(w, "defaults.identity.attribution_footer"), "Reviewed by prx.  ");
  X.type(w, X.field(w, "defaults.approve_message"), "LGTM, thanks!");
  X.type(w, X.field(w, "defaults.comments.template"), "{{SUMMARY}}\n\n{{FINDINGS_BLOCKING}}");
  X.toggle(w, X.field(w, "defaults.comments.inline"), false);
  await X.save(w);
  const expected = RAW();
  expected.defaults.review_heading = "# {{VERDICT}} for {{PR}}";
  expected.defaults.approve_message = "LGTM, thanks!";
  expected.defaults.comments = { template: "{{SUMMARY}}\n\n{{FINDINGS_BLOCKING}}", inline: false };
  // The sign-off equals the built-in, so it stays unset.
  delete expected.defaults.identity;
  assert.deepEqual(X.lastPut(w), expected);
  // Clearing an optional template unsets it; Use built-in does the same.
  X.type(w, X.field(w, "defaults.approve_message"), "");
  assert.equal("approve_message" in draft(w).defaults, false);
  w.fire(button(X.rowOf(w, "defaults.review_heading"), "Use built-in"), "click");
  assert.equal("review_heading" in draft(w).defaults, false);
  assert.equal(X.field(w, "defaults.review_heading").value, "## Automated review of {{PR}}: {{VERDICT}}");
});

test("Messages: the required block message is saved as typed, and reset to the built-in text", async () => {
  const w = await X.open("messages");
  X.type(w, X.field(w, "defaults.block_message"), "Hold on.");
  assert.equal(draft(w).defaults.block_message, "Hold on.");
  w.fire(button(X.rowOf(w, "defaults.block_message"), "Use built-in"), "click");
  assert.equal(draft(w).defaults.block_message, "Changes requested.");
  assert.equal(X.field(w, "defaults.block_message").value, "Changes requested.");
});

test("Messages: the sample review follows the templates as they are typed, as text", async () => {
  const w = await X.open("messages");
  const body = () => w.page.querySelector(".preview-body");
  assert.equal(w.page.querySelector(".preview-title").textContent, "Sample review on acme/api#128");
  assert.equal(body().children[0].textContent, "Automated review of acme/api#128: changes requested");
  assert.equal(body().children[0].className, "md-h md-h2");
  assert.equal(body().children[body().children.length - 1].textContent, "Reviewed by prx.");
  assert.deepEqual(body().querySelectorAll("li").map((li) => li.textContent), ["src/db/query.rs:42 — Unbounded query when limit is missing.", "README.md:10 — Typo in the setup steps."]);
  // Live: the heading re-renders on input, before the field is left.
  const heading = X.field(w, "defaults.review_heading");
  heading.value = "### **{{VERDICT}}** on `{{PR}}` {{UNKNOWN}} " + X.EVIL;
  w.fire(heading, "input");
  const first = body().children[0];
  assert.equal(first.className, "md-h md-h3");
  assert.equal(first.querySelector("strong").textContent, "changes requested");
  assert.equal(first.querySelector("code").textContent, "acme/api#128");
  assert.ok(first.textContent.endsWith("{{UNKNOWN}} " + X.EVIL), "unknown placeholders and markup stay as typed");
  assert.deepEqual(X.markupIn(w.page), []);
  // An emptied template previews the built-in text.
  heading.value = "";
  w.fire(heading, "input");
  assert.equal(body().children[0].textContent, "Automated review of acme/api#128: changes requested");
  // The verdict picker changes the sample, and is remembered.
  const radios = w.byId("preview-verdict").querySelectorAll("input");
  X.choose(w, radios[1]);
  assert.equal(body().children[0].textContent, "Automated review of acme/api#128: approved — no blocking issues");
  assert.equal(body().querySelectorAll("li").length, 1);
  w.window.prxApp.go("general");
  w.window.prxApp.go("messages");
  assert.equal(w.page.querySelector(".preview-body").children[0].textContent, "Automated review of acme/api#128: approved — no blocking issues");
  // The preview is not a setting.
  assert.equal("previewVerdict" in draft(w), false);
});

// ---- Schedule -------------------------------------------------------------------

test("Schedule: no schedule, a timed preset, and a custom cron expression", async () => {
  const daemon = X.storing();
  const w = await X.open("schedule", daemon.handlers);
  const radio = (id) => w.byId("f-defaults_schedule-" + id);
  assert.deepEqual([radio("poll").checked, radio("timed").checked, radio("custom").checked], [true, false, false]);
  assert.deepEqual(X.rowOf(w, "defaults.schedule").querySelectorAll(".choice-label").map((n) => n.textContent), ["With every poll", "On a schedule", "Custom"]);
  // On a schedule: the preset in the menu is committed.
  X.choose(w, radio("timed"));
  assert.equal(draft(w).defaults.schedule, "0 * * * *");
  assert.match(X.rowOf(w, "defaults.schedule").querySelector(".choice-next").textContent, /^Next: /);
  X.pickOption(w, radio("preset"), "Weekday mornings");
  assert.equal(draft(w).defaults.schedule, "0 9 * * 1-5");
  assert.deepEqual(X.optionLabels(w, radio("preset")), ["Every hour", "Every 2 hours", "Every 4 hours", "Weekday mornings", "Twice each weekday", "Every morning", "Weekly"]);
  // Custom: the expression is described while typing and saved as typed.
  X.choose(w, radio("custom"));
  const cron = radio("cron");
  const hint = X.rowOf(w, "defaults.schedule").querySelector(".cron-hint");
  assert.match(hint.textContent, /^Five fields: minute, hour, day of month, month, weekday\./);
  cron.value = "30 8 * * 1-5";
  w.fire(cron, "input");
  assert.match(hint.textContent, /^Monday to Friday at 8:30\. Next: /);
  assert.equal(hint.classList.contains("warn"), false);
  w.fire(cron, "change");
  assert.equal(draft(w).defaults.schedule, "30 8 * * 1-5");
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.schedule, "30 8 * * 1-5");
  // Reloaded: the custom expression is shown as custom.
  assert.deepEqual([radio("poll").checked, radio("timed").checked, radio("custom").checked], [false, false, true]);
  assert.equal(radio("cron").value, "30 8 * * 1-5");
  // Back to "with every poll": the key is removed.
  X.choose(w, radio("poll"));
  assert.equal("schedule" in draft(w).defaults, false);
  await X.save(w);
  assert.equal("schedule" in X.lastPut(w).defaults, false);
});

test("Schedule: an unreadable expression warns but is the daemon's call; an empty one blocks the save", async () => {
  const daemon = X.storing();
  const w = await X.open("schedule", daemon.handlers);
  const radio = (id) => w.byId("f-defaults_schedule-" + id);
  X.choose(w, radio("custom"));
  const cron = radio("cron");
  const hint = X.rowOf(w, "defaults.schedule").querySelector(".cron-hint");
  X.type(w, cron, "61 * * * *");
  assert.equal(hint.textContent, "Minute must be 0–59, not 61. The daemon has the final say when you save.");
  assert.ok(hint.classList.contains("warn"));
  assert.equal(X.errorOf(w, "defaults.schedule"), null);
  assert.equal(draft(w).defaults.schedule, "61 * * * *");
  // Both day fields set: valid, but no next runs are guessed.
  X.type(w, cron, "0 0 1 * 1");
  assert.equal(hint.textContent, "Valid cron. Times are local.");
  X.type(w, cron, "   ");
  assert.equal(X.errorOf(w, "defaults.schedule"), "Enter a cron expression, or pick a preset");
  await X.save(w);
  assert.equal(daemon.state.puts, 0);
  assert.equal(w.byId("save-status").textContent, "Fix the highlighted field before saving.");
  // Picking a preset instead clears the error.
  X.choose(w, radio("timed"));
  assert.equal(X.errorOf(w, "defaults.schedule"), null);
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.schedule, "0 * * * *");
  // Focusing the cron box is choosing Custom.
  radio("cron").focus();
  assert.equal(radio("custom").checked, true);
});

test("Schedule: a saved preset opens on its menu entry, shorthand included", async () => {
  for (const [value, label] of [["0 9 * * 1", "Weekly"], ["@hourly", "Every hour"], ["0  */4 * * *", "Every 4 hours"]]) {
    const w = await X.open("schedule", { "config.get": () => X.config((c) => { c.raw.defaults.schedule = value; }) });
    assert.equal(w.byId("f-defaults_schedule-timed").checked, true, value);
    assert.equal(w.byId("f-defaults_schedule-preset").textContent, label, value);
    assert.equal(w.byId("f-defaults_schedule-cron").value, "", value);
    assert.equal(w.window.prxApp.app.isDirty(), false);
  }
});

// ---- Notifications ----------------------------------------------------------------

test("Notifications: each event's tier, set or built-in, and an edit of one", async () => {
  const w = await X.open("notifications", X.storing().handlers);
  const tier = (key) => { const inputs = radios(w, "defaults.notifications." + key); return ["urgent", "informational", "silent"][inputs.findIndex((i) => i.checked)]; };
  assert.deepEqual(S.EVENTS.map((e) => tier(e[0])), ["informational", "informational", "informational", "urgent", "informational", "informational", "silent", "silent", "urgent", "silent"]);
  assert.deepEqual(X.rowOf(w, "defaults.notifications.on_merged").querySelectorAll("label").map((l) => l.textContent), ["Urgent", "Info", "Silent"]);
  // Only on_new_pr and on_failure are set; the rest say they are defaults.
  assert.equal(help(w, "defaults.notifications.on_new_pr"), null);
  assert.equal(help(w, "defaults.notifications.on_merged"), "Not set, so PRX uses the default (Silent).");
  X.choose(w, radios(w, "defaults.notifications.on_merged")[0]);
  X.toggle(w, X.field(w, "defaults.notifications.enabled"), false);
  await X.save(w);
  const expected = RAW();
  expected.defaults.notifications = { on_new_pr: "informational", on_failure: "urgent", on_merged: "urgent", enabled: false };
  assert.deepEqual(X.lastPut(w), expected);
});

test("Notifications: an event's switch turns it off as the Silent tier and back on to its last loud tier", async () => {
  const w = await X.open("notifications", X.storing().handlers);
  const onOff = (key) => w.byId("f-defaults_notifications_" + key + "-on");
  const tier = (key) => S.TIERS[radios(w, "defaults.notifications." + key).findIndex((i) => i.checked)];
  // On for every loud event, off for the silent ones; named for VoiceOver.
  assert.deepEqual(S.EVENTS.map((e) => onOff(e[0]).checked), [true, true, true, true, true, true, false, false, true, false]);
  assert.equal(onOff("on_merged").getAttribute("role"), "switch");
  assert.equal(onOff("on_merged").getAttribute("aria-label"), "Notify: Merged");
  // Off: the Silent radio is picked. On again: back to Urgent.
  X.toggle(w, onOff("on_review_complete_blocking"), false);
  assert.equal(tier("on_review_complete_blocking"), "silent");
  X.toggle(w, onOff("on_review_complete_blocking"), true);
  assert.equal(tier("on_review_complete_blocking"), "urgent");
  // A silent event switched on goes to Info; picking a radio moves the switch.
  X.toggle(w, onOff("on_merged"), true);
  assert.equal(tier("on_merged"), "informational");
  X.choose(w, radios(w, "defaults.notifications.on_new_pr")[2]);
  assert.equal(onOff("on_new_pr").checked, false);
  X.toggle(w, onOff("on_review_complete_approved"), false);
  await X.save(w);
  const expected = RAW();
  expected.defaults.notifications = { on_new_pr: "silent", on_failure: "urgent", on_review_complete_blocking: "urgent", on_merged: "informational",
    on_review_complete_approved: "silent" };
  assert.deepEqual(X.lastPut(w), expected);
});

test("Notifications: each event's sound is built in, the tier's, none, or a name, and clears back to unset", async () => {
  const daemon = X.storing((c) => { c.raw.defaults.notifications.sounds = { on_failure: "Sosumi" }; });
  const w = await X.open("notifications", daemon.handlers);
  const sound = (key) => X.field(w, "defaults.notifications.sounds." + key);
  // Only a review that approves has a sound of its own.
  assert.equal(sound("on_review_complete_approved").querySelector(".select-label").textContent, "Built in (Glass)");
  assert.equal(sound("on_review_complete_blocking").querySelector(".select-label").textContent, "Tier's sound");
  assert.equal(sound("on_failure").querySelector(".select-label").textContent, "Sosumi");
  assert.equal(sound("on_failure").getAttribute("aria-label"), "Review failed sound");
  assert.deepEqual(X.optionLabels(w, sound("on_new_pr")).slice(0, 4), ["Tier's sound", "No sound", "Basso", "Blow"]);
  assert.equal(X.optionLabels(w, sound("on_new_pr")).length, 2 + S.SOUNDS.length);
  X.pickOption(w, sound("on_review_complete_blocking"), "No sound");
  X.pickOption(w, sound("on_review_complete_approved"), "Hero");
  X.pickOption(w, sound("on_failure"), "Tier's sound");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).defaults.notifications,
    { on_new_pr: "informational", on_failure: "urgent", sounds: { on_review_complete_blocking: "none", on_review_complete_approved: "Hero" } });
  // Clearing the last ones leaves no empty `sounds` behind.
  X.pickOption(w, sound("on_review_complete_blocking"), "Tier's sound");
  X.pickOption(w, sound("on_review_complete_approved"), "Built in (Glass)");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).defaults.notifications, { on_new_pr: "informational", on_failure: "urgent" });
});

test("Notifications: the menu bar bell is on unless turned off, and saves under defaults", async () => {
  const w = await X.open("notifications", X.storing().handlers);
  const bell = X.field(w, "defaults.notifications.menubar_bell");
  assert.equal(bell.checked, true);
  assert.equal(help(w, "defaults.notifications.menubar_bell"),
    "PRX.app adds a bell to its icon and counts ready (✓) and blocked (✕) PRs until you open them. Not set, so PRX uses the default.");
  X.toggle(w, bell, false);
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.notifications.menubar_bell, false);
});

test("Notifications: tier banners and sounds, and a sound name the menu does not list", async () => {
  const w = await X.open("notifications", X.storing((c) => { c.raw.notification_tiers.silent.sound = "Chirp"; }).handlers);
  const sound = (name) => w.byId("tier-" + name + "-sound");
  assert.equal(sound("urgent").textContent, "Basso");
  assert.equal(sound("informational").textContent, "No sound");
  assert.equal(sound("urgent").getAttribute("aria-label"), "Urgent sound");
  // A custom name set in config.yml is offered so it is never dropped.
  assert.equal(sound("silent").textContent, "Chirp");
  assert.equal(X.optionLabels(w, sound("silent")).pop(), "Chirp");
  assert.equal(X.optionLabels(w, sound("urgent")).includes("Chirp"), false);
  assert.deepEqual(X.optionLabels(w, sound("urgent")).slice(0, 3), ["No sound", "Basso", "Blow"]);
  X.pickOption(w, sound("urgent"), "No sound");
  X.pickOption(w, sound("informational"), "Glass");
  X.toggle(w, w.byId("tier-silent-banner"), true);
  await X.save(w);
  assert.deepEqual(X.lastPut(w).notification_tiers, { urgent: { banner: true, sound: null }, informational: { banner: true, sound: "Glass" }, silent: { banner: true, sound: "Chirp" } });
});

test("Notifications: with delivery off the event and tier panels are dimmed", async () => {
  const w = await X.open("notifications");
  const panels = () => w.page.querySelectorAll("section > .panel");
  // Delivery (with the bell, which is the app's own), Events, Sounds, Tiers.
  assert.deepEqual(panels().map((p) => p.classList.contains("inactive")), [false, false, false, false]);
  X.toggle(w, X.field(w, "defaults.notifications.enabled"), false);
  assert.deepEqual(panels().map((p) => p.classList.contains("inactive")), [false, true, true, true]);
  X.toggle(w, X.field(w, "defaults.notifications.enabled"), true);
  assert.deepEqual(panels().map((p) => p.classList.contains("inactive")), [false, false, false, false]);
  const off = await X.open("notifications", { "config.get": () => X.config((c) => { c.raw.defaults.notifications.enabled = false; }) });
  assert.deepEqual(off.page.querySelectorAll("section > .panel").map((p) => p.classList.contains("inactive")), [false, true, true, true]);
  assert.equal(X.field(off, "defaults.notifications.enabled").checked, false);
});

test("Notifications: a daemon error on a tier lands on that tier's row", async () => {
  const w = await X.open("notifications", { "config.put": () => { throw bridgeError("rejected", "notification_tiers.urgent.sound: unknown sound 'Boom'", "notification_tiers.urgent.sound"); } });
  X.toggle(w, w.byId("tier-urgent-banner"), false);
  await X.save(w);
  assert.equal(X.errorOf(w, "notification_tiers.urgent"), "notification_tiers.urgent.sound: unknown sound 'Boom'");
  assert.equal(X.errorOf(w, "notification_tiers.silent"), null);
});

// ---- Menu bar -------------------------------------------------------------------

test("Menu bar: sort and grouping are saved at the top level, never under defaults", async () => {
  const w = await X.open("menubar", X.storing().handlers);
  const radios = (path) => X.rowOf(w, path).querySelectorAll("input");
  assert.deepEqual(radios("menu.sort").map((r) => r.checked), [true, false, false, false]);
  assert.deepEqual(X.rowOf(w, "menu.sort").querySelectorAll(".choice-label").map((n) => n.textContent), ["Newest first", "Oldest first", "Last reviewed", "Repo and number"]);
  X.choose(w, radios("menu.sort")[3]);
  X.choose(w, radios("menu.group_by")[1]);
  await X.save(w);
  const expected = RAW();
  expected.menu = { sort: "key", group_by: "repo" };
  assert.deepEqual(X.lastPut(w), expected);
  assert.equal("menu" in X.lastPut(w).defaults, false);
  // No `menu` section in the file: the built-in choices show, unsaved.
  const bare = await X.open("menubar", { "config.get": () => X.config((c) => { delete c.raw.menu; }) });
  assert.deepEqual(X.rowOf(bare, "menu.sort").querySelectorAll("input").map((r) => r.checked), [true, false, false, false]);
  assert.deepEqual(X.rowOf(bare, "menu.group_by").querySelectorAll("input").map((r) => r.checked), [true, false, false]);
  assert.equal(bare.window.prxApp.app.isDirty(), false);
  X.choose(bare, X.rowOf(bare, "menu.group_by").querySelectorAll("input")[2]);
  assert.deepEqual(draft(bare).menu, { group_by: "none" });
});

// ---- Account & Browser ------------------------------------------------------------

test("Account: the login and the token variable's name; clearing them unsets identity", async () => {
  const w = await X.open("identity", X.storing().handlers);
  const login = X.field(w, "defaults.identity.login");
  const credential = X.field(w, "defaults.identity.credential");
  assert.equal(login.placeholder, "The account gh is signed in as");
  // A variable name, not a secret: a plain text field that says so.
  assert.equal(credential.getAttribute("type"), "text");
  assert.equal(credential.placeholder, "e.g. GH_TOKEN_WORK");
  assert.equal(X.rowOf(w, "defaults.identity.credential").querySelector(".row-label").textContent, "Token variable");
  assert.equal(help(w, "defaults.identity.credential"), "Name of an environment variable holding a gh token. Never the token itself.");
  X.type(w, login, "maya");
  X.type(w, credential, "GH_TOKEN_WORK");
  await X.save(w);
  assert.deepEqual(X.lastPut(w).defaults.identity, { login: "maya", credential: "GH_TOKEN_WORK" });
  X.type(w, X.field(w, "defaults.identity.login"), "");
  X.type(w, X.field(w, "defaults.identity.credential"), "  ");
  assert.equal("identity" in draft(w).defaults, false, "an emptied partial is removed, not saved as {}");
});

test("Account: a pasted token is refused by the daemon, shown on the field, and never echoed elsewhere", async () => {
  const token = "ghp_0123456789abcdefghij0123456789abcdef";
  const w = await X.open("identity", { "config.put": () => { throw bridgeError("rejected",
    "defaults.identity.credential: the value must be an environment variable name (letters, digits and underscores)", "defaults.identity.credential"); } });
  X.type(w, X.field(w, "defaults.identity.credential"), token);
  await X.save(w);
  assert.match(X.errorOf(w, "defaults.identity.credential"), /must be an environment variable name/);
  // The only place the pasted text exists is the field it was typed into.
  assert.equal(w.document.body.textContent.includes(token), false);
  assert.deepEqual(walk(w.document.body).filter((n) => Object.values(n.attrs).some((v) => v.includes(token))), []);
});

test("no settings page has a password field or asks for a key, token or secret value", async () => {
  for (const p of S.PAGES.filter((p) => p[3] === "settings")) {
    const w = await X.open(p[0]);
    assert.deepEqual(w.page.querySelectorAll("input").filter((i) => i.getAttribute("type") === "password"), [], p[0]);
    const labels = w.page.querySelectorAll(".row-label, .agent-field label, .agent-field-label").map((l) => l.textContent);
    labels.filter((l) => /\b(key|token|secret|password|credential)\b/i.test(l)).forEach((l) => assert.match(l, /variable$/i, p[0] + ": " + l));
  }
});

test("Browser: automatic opening is a choice with a built-in default", async () => {
  const w = await X.open("identity", X.storing().handlers);
  const radios = X.rowOf(w, "defaults.auto_open").querySelectorAll("input");
  assert.deepEqual(radios.map((r) => r.checked), [true, false, false, false]);
  assert.equal(help(w, "defaults.auto_open"), "After a review finishes with this verdict, PRX opens the PR for you. Not set, so PRX uses the default (Never).");
  X.choose(w, radios[2]);
  assert.equal(draft(w).defaults.auto_open, "blocking");
  assert.equal(X.field(w, "defaults.browser").textContent, "System default");
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.auto_open, "blocking");
  assert.equal(X.lastPut(w).defaults.browser, "system");
  // Once set, the "not set" note is gone.
  assert.equal(help(w, "defaults.auto_open"), "After a review finishes with this verdict, PRX opens the PR for you.");
});

test("a value the schema does not know is shown as it is and saved unchanged", async () => {
  const w = await X.open("identity", X.storing((c) => { c.raw.defaults.auto_open = "sometimes"; }).handlers);
  assert.deepEqual(X.rowOf(w, "defaults.auto_open").querySelectorAll("input").map((r) => r.checked), [false, false, false, false]);
  X.type(w, X.field(w, "defaults.identity.login"), "maya");
  await X.save(w);
  assert.equal(X.lastPut(w).defaults.auto_open, "sometimes");
});
