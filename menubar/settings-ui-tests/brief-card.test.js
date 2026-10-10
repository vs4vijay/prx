// The brief card (task 4.3, spec "Brief in the app window"): the view model
// for each state (none, fresh, stale, pending, failed, briefs off), the
// regenerate progress that lasts until `updated_at` changes or an error
// shows, the card's DOM (through a tiny fake `el`), the Dashboard's
// attention rows carrying their PR, and the preview mock's brief.request.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const B = require("../settings-ui/brief-card.js");
const D = require("../settings-ui/pages-dashboard.js");
const mock = require("./preview-mock.js");

const NOW = Date.parse("2026-10-05T12:00:00Z");
const BRIEF = { text: "Adds rate limiting to login.", created_at: "2026-10-01T09:00:00Z", updated_at: "2026-10-05T10:00:00Z",
  head_sha: "aaaa", source: "review", engine: "claude", stale: false };
function pr(extra) { return Object.assign({ key: "acme/api#42", brief_pending: false, brief_available: true }, extra); }

// A fake `el`: plain objects with the attributes and children kept.
function el(tag, attrs, children) {
  const node = { tag: tag, attrs: {}, text: "", children: [], listeners: {} };
  Object.entries(attrs || {}).forEach(([k, v]) => {
    if (v === undefined || v === null || v === false) return;
    if (k === "text") node.text = v;
    else if (k.startsWith("on")) node.listeners[k.slice(2)] = v;
    else node.attrs[k] = v === true ? "" : String(v);
  });
  (children || []).forEach((c) => { if (c) node.children.push(c); });
  return node;
}
function all(node) { return [node].concat(...node.children.map(all)); }
function byClass(node, cls) { return all(node).filter((n) => (n.attrs.class || "").split(" ").includes(cls)); }
function text(node) { return node.text + node.children.map(text).join(""); }
const fmt = (iso) => "D(" + iso.slice(0, 10) + ")";

test("view: a fresh brief offers Regenerate with both dates", () => {
  const v = B.briefView(pr({ brief: BRIEF }), null, NOW);
  assert.equal(v.hasBrief, true);
  assert.equal(v.text, BRIEF.text);
  assert.equal(v.createdAt, BRIEF.created_at);
  assert.equal(v.updatedAgo, "2h ago");
  assert.equal(v.stale, false);
  assert.equal(v.action, "Regenerate");
  assert.equal(v.canAct, true);
  assert.equal(v.error, null);
});

test("view: no brief offers Write brief, and nothing when briefs are off", () => {
  const v = B.briefView(pr({}), null, NOW);
  assert.equal(v.hasBrief, false);
  assert.equal(v.action, "Write brief");
  const off = B.briefView(pr({ brief_available: false }), null, NOW);
  assert.equal(off.action, null);
  assert.equal(off.canAct, false);
  // A blank text counts as no brief.
  assert.equal(B.briefView(pr({ brief: Object.assign({}, BRIEF, { text: "  " }) }), null, NOW).hasBrief, false);
});

test("view: stale and pending states", () => {
  const stale = B.briefView(pr({ brief: Object.assign({}, BRIEF, { stale: true }) }), null, NOW);
  assert.equal(stale.stale, true);
  assert.equal(stale.action, "Regenerate");
  const pending = B.briefView(pr({ brief: BRIEF, brief_pending: true, brief_error: "old" }), null, NOW);
  assert.equal(pending.busy, true);
  assert.equal(pending.action, "Writing brief…");
  assert.equal(pending.canAct, false);
  assert.equal(pending.error, null, "an old failure is hidden while a new run is under way");
  // The previous brief stays visible meanwhile.
  assert.equal(pending.text, BRIEF.text);
});

test("view: the daemon's last failure shows inline", () => {
  const v = B.briefView(pr({ brief: BRIEF, brief_error: "engine timed out" }), null, NOW);
  assert.equal(v.error, "engine timed out");
  assert.equal(v.canAct, true);
});

test("regenerate: progress lasts until updated_at changes", () => {
  const before = pr({ brief: BRIEF });
  let local = B.start(before);
  assert.equal(B.briefView(before, local, NOW).busy, true, "busy as soon as it is clicked");
  // A poll that raced the request changes nothing yet.
  assert.deepEqual(B.reconcile(local, before), local);
  local = B.accepted(local);
  local = B.reconcile(local, pr({ brief: BRIEF, brief_pending: true }));
  assert.equal(local.sawPending, true);
  assert.equal(B.briefView(pr({ brief: BRIEF, brief_pending: true }), local, NOW).action, "Writing brief…");
  // Finished: a new updated_at settles it.
  const after = pr({ brief: Object.assign({}, BRIEF, { updated_at: "2026-10-05T12:00:00Z" }) });
  assert.equal(B.reconcile(local, after), null);
  assert.equal(B.briefView(after, null, NOW).updatedAgo, "just now");
});

test("regenerate: a failed run settles and shows brief_error", () => {
  let local = B.accepted(B.start(pr({ brief: BRIEF })));
  local = B.reconcile(local, pr({ brief: BRIEF, brief_pending: true }));
  const failed = pr({ brief: BRIEF, brief_error: "engine timed out" });
  assert.equal(B.reconcile(local, failed), null);
  assert.equal(B.briefView(failed, null, NOW).error, "engine timed out");
  // Without seeing pending, a new error text still settles it.
  const quick = B.accepted(B.start(pr({ brief: BRIEF, brief_error: "first" })));
  assert.equal(B.reconcile(quick, pr({ brief: BRIEF, brief_error: "second" })), null);
});

test("regenerate: a refused request shows its error until the next change", () => {
  const before = pr({ brief: BRIEF });
  const local = B.rejected(B.start(before), "a brief for acme/api#42 is already being written");
  const v = B.briefView(before, local, NOW);
  assert.equal(v.busy, false);
  assert.equal(v.error, "a brief for acme/api#42 is already being written");
  assert.equal(v.action, "Regenerate");
  assert.deepEqual(B.reconcile(local, before), local, "kept while nothing changed");
  assert.equal(B.reconcile(local, pr({ brief: BRIEF, brief_pending: true })), null);
});

test("regenerate: gives up waiting after idle polls, and drops untracked PRs", () => {
  let local = B.accepted(B.start(pr({ brief: BRIEF })));
  for (let i = 1; i < B.IDLE_POLLS; i++) local = B.reconcile(local, pr({ brief: BRIEF }));
  assert.ok(local);
  assert.equal(B.reconcile(local, pr({ brief: BRIEF })), null);
  const locals = { "acme/api#42": B.accepted(B.start(pr({}))), "gone/x#1": B.accepted(B.start(pr({ key: "gone/x#1" }))) };
  B.reconcileAll(locals, [pr({ brief_pending: true })]);
  assert.deepEqual(Object.keys(locals), ["acme/api#42"]);
});

test("card: renders text, dates, stale marker and the action", () => {
  let clicked = 0;
  const v = B.briefView(pr({ brief: Object.assign({}, BRIEF, { stale: true }) }), null, NOW);
  const card = B.card(el, v, () => { clicked++; }, fmt);
  assert.equal(card.tag, "section");
  assert.equal(card.attrs["aria-label"], "Brief for acme/api#42");
  assert.ok(card.attrs.class.includes("stale"));
  assert.equal(text(byClass(card, "brief-text")[0]), BRIEF.text);
  const dates = text(byClass(card, "brief-dates")[0]);
  assert.equal(dates, "Created D(2026-10-01) · Updated D(2026-10-05) (2h ago)");
  const times = all(card).filter((n) => n.tag === "time");
  assert.deepEqual(times.map((t) => t.attrs.datetime), [BRIEF.created_at, BRIEF.updated_at]);
  assert.equal(byClass(card, "badge")[0].text, "Stale");
  assert.match(text(byClass(card, "brief-stale")[0]), /earlier commit/);
  const button = byClass(card, "brief-action")[0];
  assert.equal(button.text, "Regenerate");
  assert.equal(button.attrs.disabled, undefined);
  button.listeners.click();
  assert.equal(clicked, 1);
});

test("card: pending disables the action and says so; errors are alerts", () => {
  const pending = B.card(el, B.briefView(pr({ brief_pending: true }), null, NOW), () => {}, fmt);
  const button = byClass(pending, "brief-action")[0];
  assert.equal(button.text, "Writing brief…");
  assert.equal(button.attrs.disabled, "");
  assert.equal(button.listeners.click, undefined);
  assert.equal(pending.attrs["aria-busy"], "true");
  assert.ok(all(pending).some((n) => n.attrs.role === "status" && /Writing brief/.test(n.text)));
  assert.match(text(byClass(pending, "brief-empty")[0]), /No brief yet/);

  const failed = B.card(el, B.briefView(pr({ brief_error: "engine timed out" }), null, NOW), () => {}, fmt);
  const alert = all(failed).find((n) => n.attrs.role === "alert");
  assert.equal(alert.text, "Couldn't write the brief: engine timed out");
  assert.equal(byClass(failed, "brief-action")[0].text, "Write brief");

  const off = B.card(el, B.briefView(pr({ brief_available: false }), null, NOW), () => {}, fmt);
  assert.equal(byClass(off, "brief-action").length, 0);
  assert.match(text(byClass(off, "brief-empty")[0]), /off for this repo/);
});

test("dashboard: attention rows carry their PR (for the brief card)", () => {
  const s = mock.call("status.get");
  const m = D.dashboardModel(s, { entries: [] }, null, null, mock.NOW);
  m.attention.forEach((a) => assert.equal(a.pr.key, a.key));
  const stale = m.attention.find((a) => a.key === "acme/web#1279");
  assert.equal(B.briefView(stale.pr, null, mock.NOW).stale, true);
  const pending = m.attention.find((a) => a.key === "acme/billing#164");
  assert.equal(B.briefView(pending.pr, null, mock.NOW).busy, true);
});

test("preview mock: brief.request starts a run once; unknown PRs are refused", () => {
  const key = "acme/api#412";
  assert.equal(mock.call("status.get").prs.find((p) => p.key === key).brief, undefined);
  assert.deepEqual(mock.call("brief.request", { key: key }), { accepted: true, key: key });
  const entry = mock.call("status.get").prs.find((p) => p.key === key);
  assert.equal(entry.brief_pending, true);
  assert.equal(B.briefView(entry, null, mock.NOW).action, "Writing brief…");
  assert.throws(() => mock.call("brief.request", { key: key }), /already being written/);
  assert.throws(() => mock.call("brief.request", { key: "nope/x#1" }), /not a tracked PR/);
});
