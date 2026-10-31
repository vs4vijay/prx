// Fixtures for the page tests (not a test file): a small config, a status
// payload with one PR per lifecycle state and skip reason, history entries of
// every event type, and a usage / insights answer, all in the shapes the
// daemon serves (src/ipc.rs, src/status.rs, src/history.rs, src/usage.rs).
// Times hang off the fixed NOW the fake window's clock is pinned to. Several
// strings are hostile on purpose: PR content is untrusted and must only ever
// be shown as text.
"use strict";
const { boot, bridgeError, walk } = require("./fake-dom.js");

const NOW = Date.UTC(2026, 9, 14, 12, 0, 0);
const MIN = 60e3, HOUR = 3600e3, DAY = 86400e3;
const ago = (ms) => new Date(NOW - ms).toISOString();

const EVIL = '<img src=x onerror="alert(1)"><script>alert(2)</script>';
const LONG = "Refactor ".repeat(40).trim();
const UNICODE = "修复 🚀 — naïve café ‮txt.exe مرحبا";
const CONTRACT = "## prx output contract (always added by prx)\n\nWrite one JSON manifest. \"manifest_version\": 1\n";
const SHIPPED_PROMPT = "# Review {{REPO}}#{{PR_NUMBER}}\n\nRead the diff carefully.\n";

// GET /config. `raw` is the daemon's typed config, so it only ever carries
// the current names (`engines`, `engine`, `brief`).
function config(change) {
  const raw = {
    defaults: {
      engine: "claude", agent_model: "sonnet", poll_interval_minutes: 10, review_mode: "review comment",
      cooldown: { before_first_review_minutes: 60, between_re_reviews_minutes: 30 },
      review_prompt: "default", max_concurrent_reviews: 2, diff_max_files: 60, diff_max_lines: 5000, agent_timeout_seconds: 900,
      skip: { drafts: true, authors: ["dependabot[bot]"], labels: [], branches: [] },
      brief: { enabled: true, timeout_seconds: 120 },
      notifications: { on_new_pr: "informational", on_failure: "urgent" },
      browser: "system", identity: {}, block_message: "Blocked pending a human follow-up.",
    },
    notification_tiers: { urgent: { banner: true, sound: "Basso" }, informational: { banner: true, sound: null }, silent: { banner: false, sound: null } },
    engines: { cheap: { type: "llm_api", provider: "anthropic", model: "claude-haiku-4-5", api_key_env: "ANTHROPIC_API_KEY" } },
    orgs: [{ org: "acme", poll_interval_minutes: 30 }],
    repos: [{ name: "acme/api", engine: "codex" }, { name: "acme/web" }, { name: "solo/tool" }],
    menu: { sort: "created_desc", group_by: "lifecycle" },
  };
  const out = {
    path: "/Users/test/.config/prx/config.yml",
    yaml: "defaults:\n  engine: claude\n",
    raw: raw,
    effective: { defaults: JSON.parse(JSON.stringify(raw.defaults)) },
    shipped_review_prompt: SHIPPED_PROMPT,
    review_contract: CONTRACT,
    shipped_templates: {
      review_heading: "## Automated review of {{PR}}: {{VERDICT}}",
      "identity.attribution_footer": "Reviewed by prx.",
      block_message: "Changes requested.",
    },
  };
  if (change) change(out);
  return out;
}

function pr(key, state, extra) {
  const repo = key.slice(0, key.indexOf("#"));
  return Object.assign({
    key: key, state: state, skip_reason: null, title: "PR " + key, author: "noor", last_verdict: null, blocking_count: 0, nit_count: 0,
    last_review_at: null, summary: "", created_at: ago(2 * DAY), brief_pending: false, brief_available: true,
    html_url: "https://github.com/" + repo + "/pull/" + key.slice(key.indexOf("#") + 1), engine: "claude",
  }, extra || {});
}

// One PR per lifecycle state and skip reason, plus hostile, long and
// non-ASCII content.
function prs() {
  return [
    pr("acme/api#1", "pending", { title: EVIL, author: "<b>mallory</b>", created_at: ago(3 * HOUR) }),
    pr("acme/api#2", "in_review", { title: UNICODE, created_at: ago(5 * HOUR), summary: "reviewing for 3m" }),
    pr("acme/web#3", "reviewed_approved", { last_verdict: "approved", nit_count: 2, last_review_at: ago(2 * HOUR), created_at: ago(DAY),
      brief: { text: "Adds retries to the upload path. " + EVIL, created_at: ago(3 * HOUR), updated_at: ago(2 * HOUR), head_sha: "abc", source: "review", engine: "claude", stale: false } }),
    pr("acme/web#4", "reviewed_approved", { last_verdict: "approved", human_approved_at: ago(HOUR), last_review_at: ago(4 * HOUR) }),
    pr("acme/web#5", "reviewed_approved", { last_verdict: "comment_only", last_review_at: ago(6 * HOUR) }),
    pr("acme/web#6", "reviewed_changes_requested", { title: LONG, last_verdict: "changes_requested", blocking_count: 2, nit_count: 1,
      last_review_at: ago(30 * MIN), summary: "2 blocking", engine: "codex",
      brief: { text: "Rewrites the session store.", created_at: ago(DAY), updated_at: ago(DAY), head_sha: "old", source: "on_demand", stale: true } }),
    pr("solo/tool#7", "skipped", { skip_reason: "draft" }),
    pr("solo/tool#8", "skipped", { skip_reason: "config" }),
    pr("solo/tool#9", "skipped", { skip_reason: "snoozed" }),
    pr("solo/tool#10", "skipped", { skip_reason: "ignored", brief_available: false }),
    pr("acme/api#11", "merged", { last_verdict: "approved", last_review_at: ago(3 * DAY), created_at: ago(9 * DAY) }),
    pr("acme/api#12", "closed", { created_at: ago(40 * DAY), html_url: "" }),
  ];
}

function status(change) {
  const out = {
    version: "0.1.1", pid: 4242, uptime_seconds: 3600, in_flight: 1, queue_depth: 2, next_poll: new Date(NOW + 6 * MIN).toISOString(),
    review_mode: "review comment", engines: ["cheap", "claude", "codex"], menu: { sort: "created_desc", group_by: "lifecycle" }, prs: prs(),
  };
  if (change) change(out);
  return out;
}

const USAGE = { input_tokens: 12000, output_tokens: 1100, cache_read_tokens: 300, cache_write_tokens: 200, cost_usd: 0.04, model: "claude-sonnet-4-5", session_id: "c0ffee" };

function entry(event, key, at, extra) {
  const cut = key.indexOf("#");
  return Object.assign({ event: event, ts: ago(at), repo: key.slice(0, cut), number: Number(key.slice(cut + 1)), title: "PR " + key, author: "noor" }, extra || {});
}

// Newest first, one of every event type (and both outcomes of a brief run).
function history() {
  return [
    entry("review_started", "acme/api#2", 3 * MIN, { title: UNICODE, agent: "claude" }),
    entry("review_completed", "acme/web#6", 30 * MIN, { title: LONG, agent: "codex", verdict: "changes_requested", blocking_count: 2, nit_count: 1,
      duration_seconds: 150, blocking_texts: ["Unbounded query " + EVIL, "Missing index"], brief: "Rewrites the session store. " + EVIL,
      posted: true, usage: USAGE, summary_usage: { input_tokens: 900, output_tokens: 80, model: "claude-haiku-4-5" } }),
    entry("review_completed", "acme/web#3", 2 * HOUR, { agent: "claude", verdict: "approved", blocking_count: 0, nit_count: 2, posted: false, usage: USAGE }),
    entry("review_completed", "acme/web#5", 6 * HOUR, { agent: "pi", verdict: "comment_only", blocking_count: 0, nit_count: 0 }),
    entry("review_failed", "acme/api#1", 7 * HOUR, { title: EVIL, agent: "claude", usage: { input_tokens: 500 } }),
    entry("review_skipped", "solo/tool#7", 8 * HOUR, { reason: "draft " + EVIL }),
    entry("new_commits", "acme/web#6", 9 * HOUR),
    entry("brief", "acme/web#3", 10 * HOUR, { agent: "cheap", brief: "Adds retries to the upload path.", usage: { input_tokens: 700, output_tokens: 60, cost_usd: 0.001 } }),
    entry("brief", "acme/web#5", 11 * HOUR, { agent: "cheap" }),
    entry("pr_merged", "acme/api#11", 2 * DAY),
    entry("pr_closed", "acme/api#12", 3 * DAY),
    entry("new_pr", "acme/api#12", 4 * DAY),
  ];
}

function totals(input, output, runs, extra) {
  const split = { input_tokens: 0, output_tokens: 0, cache_read_tokens: 0, cache_write_tokens: 0, cost_usd: 0 };
  return Object.assign({ input_tokens: input, output_tokens: output, cache_read_tokens: 0, cache_write_tokens: 0, cost_usd: 0, runs: runs, runs_with_cost: 0,
    review: Object.assign({}, split, { input_tokens: input, output_tokens: output }), summary: split }, extra || {});
}

// GET /usage for {bucket, window, group}: two buckets, two groups.
function usage(query) {
  const group = (query && query.group) || "none";
  const names = group === "none" ? ["all"] : ["claude", EVIL];
  const days = ["2026-10-13", "2026-10-14"];
  return {
    bucket: (query && query.bucket) || "day", window: (query && query.window) || "30d", group: group,
    series: names.map((name, i) => ({ group: name, points: days.map((day, d) => ({ bucket_start: day, totals: totals(1000 * (i + 1) * (d + 1), 100 * (d + 1), 1) })) })),
    groups: names.map((name, i) => ({ group: name, totals: totals(3000 * (i + 1), 300, 2, i === 0 ? { cost_usd: 1.5, runs_with_cost: 1 } : {}) })),
    overall: totals(group === "none" ? 3000 : 9000, group === "none" ? 300 : 600, group === "none" ? 2 : 4,
      { cost_usd: 1.5, runs_with_cost: 1, cache_read_tokens: 400, summary: { input_tokens: 500, output_tokens: 50, cache_read_tokens: 0, cache_write_tokens: 0, cost_usd: 0 } }),
    runs_without_usage: 1,
  };
}

function insights() {
  return {
    reviews: { total: 12, failed: 1, skipped: 3 },
    per_repo: [{ repo: "acme/api", reviews: 8, approved: 5, changes_requested: 2, comment_only: 1 }, { repo: "acme/web", reviews: 4, approved: 1, changes_requested: 3, comment_only: 0 }],
    effectiveness: { changes_requested_reviews: 5, followed_by_approved: 3, rate: 0.6 },
    review_to_merge: { samples: 4, average_seconds: 5400 },
    top_blocking_issues: [{ text: "Unbounded query " + EVIL, count: 3 }, { text: "Missing index", count: 1 }],
  };
}

// A daemon answering every bridge call the pages make. `handlers` overrides
// per method: a value is returned as is, a function is called with the
// params (and may throw a bridgeError).
function daemon(handlers) {
  const base = {
    "config.get": () => config(),
    "config.put": () => ({ ok: true }),
    "status.get": () => status(),
    "history.list": () => ({ entries: history() }),
    "usage.get": (params) => usage(params),
    "insights.get": () => insights(),
    "discover.agents": () => ({ agents: [], configured: {}, llm_apis: [] }),
    "discover.repos": () => ({ repos: [] }),
    "browsers.list": () => ({ browsers: [], extra: [] }),
    "app.info": () => ({ bundle: null, login_item: { enabled: false }, cli: { state: "unavailable" } }),
    "review.request": (params) => ({ enqueued: [params.target] }),
    "brief.request": () => ({ ok: true }),
    "pause.start": () => ({ ok: true }),
    "pause.resume": () => ({ ok: true }),
  };
  const all = Object.assign(base, handlers || {});
  return function (method, params) {
    if (!Object.prototype.hasOwnProperty.call(all, method)) throw bridgeError("bad_request", "unknown method " + method);
    return typeof all[method] === "function" ? all[method](params) : all[method];
  };
}

// The window opened on `page`, with the first answers landed.
async function open(page, handlers, opts) {
  const w = boot(Object.assign({ hash: "#page=" + page, now: NOW, daemon: daemon(handlers) }, opts || {}));
  await w.settle();
  return w;
}

// ---- driving a settings page ----------------------------------------------

// The input the form registers for a config path ("defaults.engine").
function field(w, path) { return w.byId("f-" + path.replace(/[^A-Za-z0-9_-]/g, "_")); }
// The row of a config path, with its label, help and error slot.
function rowOf(w, path) { return w.page.querySelectorAll(".row").find((r) => r.getAttribute("data-field") === path) || null; }
function errorOf(w, path) { const row = rowOf(w, path); const e = row && row.querySelector(".field-error"); return e && !e.hidden ? e.textContent : null; }
// Type into a text field and leave it (input, then change).
function type(w, node, text) { node.value = text; w.fire(node, "input"); w.fire(node, "change"); }
// Flip a switch, or pick a radio (unchecking the rest of its group).
function toggle(w, node, on) { node.checked = on; w.fire(node, "change"); }
function choose(w, node) {
  w.document.querySelectorAll("input").filter((i) => i.getAttribute("type") === "radio" && i.getAttribute("name") === node.getAttribute("name"))
    .forEach((i) => { i.checked = i === node; });
  w.fire(node, "change");
}
// Pick an option of a form.js dropdown by its label; `trigger` is its button.
function pickOption(w, trigger, label) {
  if (trigger.getAttribute("aria-expanded") !== "true") w.fire(trigger, "click");
  const list = w.byId(trigger.id + "-list");
  const option = list.querySelectorAll("[role=option]").find((o) => o.querySelector(".select-label").textContent === label);
  if (!option) throw new Error("no option " + label + " in " + trigger.id + ": " + list.querySelectorAll(".select-label").map((n) => n.textContent).join(", "));
  w.fire(option, "click");
}
function optionLabels(w, trigger) { return w.byId(trigger.id + "-list").querySelectorAll(".select-label").map((n) => n.textContent); }
async function save(w) { w.fire(w.byId("save"), "click"); await w.settle(); }
// The config the last save sent (PUT /config), or undefined.
function lastPut(w) { const puts = w.calls("config.put"); return puts.length ? puts[puts.length - 1].config : undefined; }
// A daemon that keeps what is saved and serves it back, like the real one.
function storing(change, extra) {
  const state = { raw: config(change).raw, puts: 0 };
  const handlers = Object.assign({
    "config.get": () => config((c) => { if (change) change(c); c.raw = JSON.parse(JSON.stringify(state.raw)); c.effective = { defaults: c.raw.defaults }; }),
    "config.put": (p) => { state.puts += 1; if (p.config) state.raw = p.config; return { ok: true, added: [], removed: [] }; },
  }, extra || {});
  return { state: state, handlers: handlers };
}

const offline = () => { throw bridgeError("offline", "connection refused"); };
const failing = (message) => () => { throw bridgeError("daemon", message); };

// The tags a page built: hostile text must never turn into one of these.
const MARKUP_TAGS = ["script", "img", "iframe", "object", "embed", "link", "style", "form", "b"];
function markupIn(node) {
  return walk(node).filter((n) => MARKUP_TAGS.includes(n.tagName.toLowerCase())).map((n) => n.tagName.toLowerCase());
}
// Attributes that could run script or load something.
function activeAttributes(node) {
  const out = [];
  walk(node).forEach((n) => Object.keys(n.attrs).forEach((k) => {
    if (/^on/i.test(k) || k === "srcdoc" || (k === "src" && !/^data:image\/png;base64,/.test(n.attrs[k]))) out.push(n.tagName + "[" + k + "]");
    if (k === "href" && !/^https:\/\/github\.com\//.test(n.attrs[k])) out.push(n.tagName + "[href=" + n.attrs[k] + "]");
  }));
  return out;
}

module.exports = { NOW, MIN, HOUR, DAY, ago, EVIL, LONG, UNICODE, CONTRACT, SHIPPED_PROMPT, USAGE, config, pr, prs, status, entry, history, totals, usage,
  insights, daemon, open, offline, failing, markupIn, activeAttributes,
  field, rowOf, errorOf, type, toggle, choose, pickOption, optionLabels, save, lastPut, storing };
