// The brief card (task 4.3, spec "Brief in the app window"): a PR's brief
// with its created and updated dates, a stale marker, and Write /
// Regenerate with progress and an inline error. Shown in the Pull requests
// detail and the Dashboard's attention list. Sends brief.request (POST
// /brief); progress follows `brief_pending` on status.get.
//
// The regenerate state per PR ("local") lives in the app's shared view
// state, so both pages agree. It is folded with every status poll by
// `reconcile`: the button shows progress until the brief's `updated_at`
// changes or the daemon reports a failure (`brief_error`). The pure parts
// are exported for the node tests; `card` builds DOM through an `el`
// factory (UI.el in the window, a fake in tests).
(function (root) {
  "use strict";
  const node = typeof module !== "undefined" && module.exports;
  const O = node ? require("./overview.js") : root.prxOverview;

  // Polls without any sign of the run (no pending, no new brief, no new
  // error) before the card stops waiting. One poll may predate the
  // request, so the second idle one settles it.
  const IDLE_POLLS = 2;

  // The user clicked Write / Regenerate: remember what to wait for.
  function start(pr) {
    return { phase: "requesting", from: pr.brief ? pr.brief.updated_at : null, prevError: pr.brief_error || null, sawPending: false, idle: 0, error: null };
  }

  // brief.request answered: accepted (wait for the run) or refused.
  function accepted(local) { return Object.assign({}, local, { phase: "waiting" }); }
  function rejected(local, message) { return Object.assign({}, local, { phase: "failed", error: message }); }

  // Fold a fresh status entry into the local state. Returns the next local
  // state, or null once the run settled (the entry itself then shows the
  // new brief, or its `brief_error`).
  function reconcile(local, pr) {
    if (!local || !pr) return null;
    const updated = pr.brief ? pr.brief.updated_at : null;
    if (local.phase === "failed") return updated !== local.from || pr.brief_pending ? null : local;
    if (local.phase === "requesting") return local;
    if (pr.brief_pending) return Object.assign({}, local, { sawPending: true, idle: 0 });
    if (updated !== local.from) return null;
    if (pr.brief_error && (local.sawPending || pr.brief_error !== local.prevError)) return null;
    if (local.sawPending) return null;
    const idle = local.idle + 1;
    return idle >= IDLE_POLLS ? null : Object.assign({}, local, { idle: idle });
  }

  // Reconcile every local state against a status payload (drops PRs that
  // are no longer tracked). Mutates and returns `locals`.
  function reconcileAll(locals, prs) {
    Object.keys(locals).forEach(function (key) {
      const next = reconcile(locals[key], (prs || []).find(function (pr) { return pr.key === key; }));
      if (next) locals[key] = next; else delete locals[key];
    });
    return locals;
  }

  // What the card shows for `pr` (a status entry) given its local state.
  function briefView(pr, local, now) {
    const brief = pr.brief && String(pr.brief.text || "").trim() ? pr.brief : null;
    const available = pr.brief_available !== false;
    const busy = !!pr.brief_pending || (!!local && (local.phase === "requesting" || local.phase === "waiting"));
    const error = local && local.phase === "failed" ? local.error : !busy && pr.brief_error ? pr.brief_error : null;
    return {
      key: pr.key,
      hasBrief: !!brief,
      text: brief ? brief.text : "",
      createdAt: brief ? brief.created_at : null,
      updatedAt: brief ? brief.updated_at : null,
      updatedAgo: brief ? O.ago(brief.updated_at, now) : "",
      stale: !!(brief && brief.stale),
      source: brief ? brief.source : null,
      engine: brief ? brief.engine || null : null,
      busy: busy,
      error: error,
      available: available,
      // The action button: absent when briefs are off for the repo and
      // nothing is running.
      action: busy ? "Writing brief…" : !available ? null : brief ? "Regenerate" : "Write brief",
      canAct: !busy && available,
    };
  }

  // The card as DOM. `el(tag, attrs, children)` builds nodes (UI.el);
  // `onAction` runs Write / Regenerate; `formatDate(iso)` renders a date.
  function card(el, v, onAction, formatDate) {
    const date = function (label, iso) {
      return el("span", {}, [el("span", { text: label + " " }), el("time", { datetime: iso, title: iso, text: formatDate(iso) })]);
    };
    const button = v.action ? el("button", {
      type: "button", class: "btn small brief-action", disabled: !v.canAct, "aria-busy": v.busy ? "true" : null,
      onclick: v.canAct ? onAction : null, text: v.action,
    }) : null;
    return el("section", { class: "brief-card" + (v.stale ? " stale" : ""), "aria-label": "Brief for " + v.key, "aria-busy": v.busy ? "true" : null }, [
      el("div", { class: "brief-head" }, [
        el("h3", { text: "Brief" }),
        v.stale ? el("span", { class: "badge warn", text: "Stale", title: "New commits were pushed after this brief was written." }) : null,
        el("span", { class: "spacer" }),
        button,
      ]),
      v.hasBrief ? el("p", { class: "brief-text", text: v.text })
        : el("p", { class: "brief-empty", text: v.available ? "No brief yet. A brief says what this PR does and why, in a few plain sentences."
          : "Briefs are off for this repo." }),
      v.hasBrief ? el("p", { class: "brief-dates" }, [date("Created", v.createdAt), el("span", { "aria-hidden": "true", text: " · " }),
        date("Updated", v.updatedAt), el("span", { text: " (" + v.updatedAgo + ")" })]) : null,
      v.stale ? el("p", { class: "brief-stale", text: "Describes an earlier commit. Regenerate it to cover the latest push." }) : null,
      v.busy ? el("p", { class: "inline-note", role: "status", text: "Writing brief… This takes up to a minute." }) : null,
      v.error ? el("p", { class: "inline-note error", role: "alert", text: "Couldn't write the brief: " + v.error }) : null,
    ]);
  }

  const api = { IDLE_POLLS: IDLE_POLLS, start: start, accepted: accepted, rejected: rejected, reconcile: reconcile,
    reconcileAll: reconcileAll, briefView: briefView, card: card };
  if (node) { module.exports = api; return; }

  // ---- window -------------------------------------------------------------

  const dateFormat = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
  function formatDate(iso) { return iso ? dateFormat.format(new Date(iso)) : ""; }

  // One page's brief cards: `locals` is the app-wide regenerate state,
  // `redraw` repaints the page, `refresh` polls status again.
  function controller(app, bridge, redraw, refresh) {
    const locals = app.view("briefs", function () { return {}; });
    function request(pr) {
      locals[pr.key] = start(pr);
      redraw();
      bridge.call("brief.request", { key: pr.key }).then(function () {
        if (locals[pr.key]) locals[pr.key] = accepted(locals[pr.key]);
        redraw();
        refresh();
      }, function (error) {
        locals[pr.key] = rejected(locals[pr.key] || start(pr), O.isOffline(error) ? "the PRX daemon is not running." : error.message);
        redraw();
      });
    }
    return {
      // Fold a status payload in; true while any brief is being written.
      update: function (status) {
        reconcileAll(locals, status && status.prs);
        return Object.keys(locals).length > 0 || ((status && status.prs) || []).some(function (pr) { return pr.brief_pending; });
      },
      card: function (pr) {
        return card(root.prxForm.el, briefView(pr, locals[pr.key], Date.now()), function () { request(pr); }, formatDate);
      },
    };
  }

  root.prxBrief = Object.freeze(Object.assign({ controller: controller }, api));
})(this);
