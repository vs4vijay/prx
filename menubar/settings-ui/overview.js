// Shared helpers for the Overview pages (Dashboard, Pull requests, History,
// Usage & stats): the menu's lifecycle grouping, labels, number/time
// formatting, and the token usage summaries. Pure (no DOM) except `offline`,
// `failure` and `poller`, which only run in the window. Loaded as a plain
// script (window.prxOverview) and by the node tests via module.exports.
(function (root) {
  "use strict";

  // The menu bar's lifecycle groups (menubar/src/model.rs GROUPS), in order.
  const GROUPS = [
    ["needs_review", "Needs review"],
    ["in_review", "In review"],
    ["ready", "Ready for approval"],
    ["reviewed", "Reviewed"],
    ["skipped", "Skipped"],
    ["terminal", "Merged / closed"],
  ];

  // The PR state system (task 7.5): one label, tone (app.css .tone-*) and
  // glyph (icons.js) per state, used by Dashboard, Pull requests, History
  // and Usage. The menu bar (model.rs) uses the same labels.
  const STATES = {
    needs_review: { label: "Needs review", tone: "neutral", glyph: "s-waiting" },
    in_review: { label: "In review", tone: "progress", glyph: "s-reviewing" },
    ready: { label: "Ready for approval", tone: "ok", glyph: "s-ready" },
    approved: { label: "Approved", tone: "ok", glyph: "check" },
    changes_requested: { label: "Changes requested", tone: "error", glyph: "s-changes" },
    commented: { label: "Commented", tone: "neutral", glyph: "message" },
    skipped: { label: "Skipped", tone: "neutral", glyph: "offline" },
    merged: { label: "Merged", tone: "merged", glyph: "s-merged" },
    closed: { label: "Closed", tone: "neutral", glyph: "close" },
    failed: { label: "Failed", tone: "error", glyph: "alert" },
  };
  // A verdict as a state (History rows, the verdict mix).
  const VERDICT_STATES = { approved: "approved", changes_requested: "changes_requested", comment_only: "commented" };
  const VERDICT_LABELS = { approved: "Approved", changes_requested: "Changes requested", comment_only: "Commented" };
  const SKIP_LABELS = { draft: "draft", config: "skip rule", snoozed: "snoozed", ignored: "ignored" };
  const MODE_LABELS = { "review comment": "Review and comment", monitor: "Monitor", off: "Off" };

  function isTerminal(pr) { return pr.state === "merged" || pr.state === "closed"; }

  // model.rs awaiting_human_approval: the agent approved, no human has yet.
  function readyForApproval(pr) {
    return pr.state === "reviewed_approved" && pr.last_verdict === "approved" && !pr.human_approved_at;
  }

  // model.rs group_of.
  function groupOf(pr) {
    switch (pr.state) {
      case "pending": return "needs_review";
      case "in_review": return "in_review";
      case "reviewed_approved": return readyForApproval(pr) ? "ready" : "reviewed";
      case "reviewed_changes_requested": return "reviewed";
      case "skipped": return "skipped";
      default: return "terminal";
    }
  }

  // model.rs attention_count: changes requested + reviews in flight.
  function needsAttention(pr) { return pr.state === "reviewed_changes_requested" || pr.state === "in_review"; }

  function groupLabel(id) {
    const group = GROUPS.find(function (g) { return g[0] === id; });
    return group ? group[1] : id;
  }

  // A PR's display state: the lifecycle state, split by the agent's verdict
  // (ready for approval, approved, commented).
  function stateOf(pr) {
    switch (pr.state) {
      case "pending": return "needs_review";
      case "in_review": return "in_review";
      case "reviewed_approved":
        if (readyForApproval(pr)) return "ready";
        return pr.last_verdict === "comment_only" ? "commented" : "approved";
      case "reviewed_changes_requested": return "changes_requested";
      case "skipped": return "skipped";
      case "merged": return "merged";
      case "closed": return "closed";
      default: return "needs_review";
    }
  }

  // {id, label, tone, glyph} for a state id.
  function stateView(id) {
    const s = STATES[id] || { label: id, tone: "neutral", glyph: "s-waiting" };
    return { id: id, label: s.label, tone: s.tone, glyph: s.glyph };
  }

  function stateLabel(pr) {
    const label = stateView(stateOf(pr)).label;
    return pr.state === "skipped" && pr.skip_reason ? label + " (" + (SKIP_LABELS[pr.skip_reason] || pr.skip_reason) + ")" : label;
  }

  function verdictLabel(verdict) { return verdict ? VERDICT_LABELS[verdict] || verdict : ""; }
  function modeLabel(mode) { return MODE_LABELS[mode] || mode || MODE_LABELS["review comment"]; }

  // "acme/api#42" -> {repo: "acme/api", number: 42}
  function splitKey(key) {
    const match = /^(.*)#(\d+)$/.exec(key || "");
    return match ? { repo: match[1], number: Number(match[2]) } : { repo: key || "", number: 0 };
  }

  // Repo name, then PR number numerically (#9 before #10).
  function compareKeys(a, b) {
    const x = splitKey(a), y = splitKey(b);
    return x.repo < y.repo ? -1 : x.repo > y.repo ? 1 : x.number - y.number;
  }

  // 950 -> "950", 12345 -> "12.3k", 4200000 -> "4.2M".
  function formatTokens(n) {
    n = Number(n) || 0;
    if (n < 1000) return String(Math.round(n));
    if (n < 1e6) return trim(n / 1e3) + "k";
    if (n < 1e9) return trim(n / 1e6) + "M";
    return trim(n / 1e9) + "B";
  }
  function trim(x) { return (x >= 100 ? Math.round(x) : Math.round(x * 10) / 10).toString(); }

  function formatCost(usd) {
    if (usd === null || usd === undefined) return "";
    if (usd > 0 && usd < 0.01) return "<$0.01";
    return "$" + Number(usd).toFixed(2);
  }

  function formatCount(n, one, many) { return n + " " + (n === 1 ? one : many || one + "s"); }

  // A compact age: "now", "5m", "3h", "2d", "6w", "4mo", "2y".
  function shortAge(iso, now) {
    if (!iso) return "";
    const seconds = Math.max(0, (now - Date.parse(iso)) / 1000);
    if (seconds < 60) return "now";
    const steps = [[3600, 60, "m"], [86400, 3600, "h"], [86400 * 14, 86400, "d"], [86400 * 60, 86400 * 7, "w"], [86400 * 730, 86400 * 30, "mo"]];
    for (const step of steps) if (seconds < step[0]) return Math.floor(seconds / step[1]) + step[2];
    return Math.floor(seconds / (86400 * 365)) + "y";
  }

  function ago(iso, now) {
    const age = shortAge(iso, now);
    return !age ? "" : age === "now" ? "just now" : age + " ago";
  }

  // Time left on a pause, the menu bar's way: "45m left", "1h 12m left",
  // "2h left", "<1m left".
  function pauseLeft(ms) {
    const minutes = Math.ceil(Math.max(0, ms) / 60000);
    if (ms < 60000) return "<1m left";
    if (minutes < 60) return minutes + "m left";
    const h = Math.floor(minutes / 60), m = minutes % 60;
    return h + "h" + (m ? " " + m + "m" : "") + " left";
  }

  // The daemon-wide pause from GET /status: {paused, until, left}.
  // `paused_until` in the past (the daemon has not dropped it yet) is over.
  function pauseState(status, now) {
    const until = status && status.paused_until ? Date.parse(status.paused_until) : NaN;
    if (!(until > now)) return { paused: false, until: null, left: "" };
    return { paused: true, until: status.paused_until, left: pauseLeft(until - now) };
  }

  // Tokens a usage record (or a Totals/Split) accounts for: every kind.
  function tokenTotal(u) {
    if (!u) return 0;
    return (u.input_tokens || 0) + (u.output_tokens || 0) + (u.cache_read_tokens || 0) + (u.cache_write_tokens || 0);
  }

  function hasUsage(u) {
    return !!u && ["input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "cost_usd"]
      .some(function (k) { return u[k] !== undefined && u[k] !== null; });
  }

  // One token-accounting definition for every page (audit P1): "tokens"
  // is the total of every kind (tokenTotal); a breakdown always names
  // input, cache and output separately, and input never includes cache.

  // "122k tokens · $0.04", or "" when the engine reported nothing.
  function usageLine(u) {
    if (!hasUsage(u)) return "";
    const parts = [formatTokens(tokenTotal(u)) + " tokens"];
    if (u.cost_usd !== undefined && u.cost_usd !== null) parts.push(formatCost(u.cost_usd));
    return parts.join(" · ");
  }

  // "29.8k in · 91k cache · 1.1k out" (cache = reads + writes; omitted
  // when zero).
  function usageBreakdown(u) {
    if (!u) return "";
    const cache = (u.cache_read_tokens || 0) + (u.cache_write_tokens || 0);
    return [formatTokens(u.input_tokens || 0) + " in"].concat(cache ? [formatTokens(cache) + " cache"] : [],
      [formatTokens(u.output_tokens || 0) + " out"]).join(" · ");
  }

  // A history entry's PR key.
  function entryKey(e) { return e.repo + "#" + e.number; }

  // History event types (src/history.rs HistoryEntry tags), in filter order.
  const EVENTS = [
    ["review_completed", "Review completed"],
    ["review_failed", "Review failed"],
    ["review_started", "Review started"],
    ["review_skipped", "Review skipped"],
    ["new_commits", "New commits"],
    ["brief", "Brief"],
    ["pr_merged", "Merged"],
    ["pr_closed", "Closed"],
  ];

  function eventLabel(event) {
    const found = EVENTS.find(function (e) { return e[0] === event; });
    return found ? found[1] : event;
  }

  // "2 blocking, 3 nits" (blocking only when there are any).
  function findings(blocking, nits) {
    return (blocking ? blocking + " blocking, " : "") + formatCount(nits || 0, "nit");
  }

  // One line saying what happened: "Approved · 2 nits", "Skipped: draft".
  function eventSummary(e) {
    switch (e.event) {
      case "review_completed": return verdictLabel(e.verdict) + " · " + findings(e.blocking_count, e.nit_count);
      case "review_skipped": return "Skipped: " + e.reason;
      case "brief": return e.brief ? "Brief written" : "Brief failed";
      default: return eventLabel(e.event);
    }
  }

  // Whether the loaded config (GET /config) watches any repo or org: true,
  // false, or null while the config is not loaded.
  function watchesRepos(loaded) {
    if (!loaded || !loaded.raw) return null;
    return ((loaded.raw.repos || []).length + (loaded.raw.orgs || []).length) > 0;
  }

  function isOffline(error) { return !!error && error.kind === "offline"; }

  // ---- window-only helpers ---------------------------------------------

  // The "daemon offline" state every Overview page shows instead of data.
  function offline(UI, retry) {
    return UI.empty("offline", "Daemon offline",
      "The PRX daemon isn't running, so there is nothing live to show. Start it from the PRX menu bar (Start daemon) or with `prx daemon start`; this page picks it up within a few seconds.",
      retry ? [UI.button("Try again", retry, "primary")] : []);
  }

  // A state pill: tone, glyph and label (app.css .state). `label`
  // overrides the state's own (e.g. "Skipped (draft)").
  function pill(UI, id, label) {
    const v = stateView(id);
    return UI.el("span", { class: "state tone-" + v.tone, "data-state": id }, [UI.icon(v.glyph),
      UI.el("span", { class: "state-text", text: label || v.label })]);
  }

  // The menu bar's "Pause automatic reviews" durations (model.rs
  // pause_item), as `pause.start {hours}` choices.
  const PAUSE_CHOICES = [
    { value: 0.5, label: "For 30 minutes" },
    { value: 1, label: "For 1 hour" },
    { value: 2, label: "For 2 hours" },
    { value: 4, label: "For 4 hours" },
  ];

  // What a failed pause/resume says inline.
  function pauseErrorText(error) {
    if (isOffline(error)) return "The PRX daemon isn't running, so nothing changed. Start it from the PRX menu bar or with `prx daemon start`.";
    return (error && error.message) || "The daemon did not answer.";
  }

  // One page's pause controls, built once and reused by every redraw so a
  // call in flight or an error survives the page's refresh: `resume` (the
  // Resume button), `start` (a "Pause for…" dropdown of PAUSE_CHOICES; while
  // paused, a pick restarts the pause from now, as in the menu bar),
  // `error` (the inline failure line). `done` refreshes the page after a
  // change. Returns {resume, start, error, run(method, params), busy()}.
  function pauseControls(UI, bridge, done) {
    let busy = false;
    const error = UI.el("p", { class: "field-error pause-error", role: "alert" });
    error.hidden = true;
    function sync() {
      resume.disabled = busy;
      picker.trigger.disabled = busy;
    }
    function run(method, params) {
      if (busy) return Promise.resolve();
      busy = true;
      error.hidden = true;
      error.replaceChildren();
      sync();
      return bridge.call(method, params || {}).then(function (result) {
        busy = false;
        sync();
        return done ? done(result) : undefined;
      }, function (failure) {
        busy = false;
        sync();
        error.replaceChildren.apply(error, UI.rich(pauseErrorText(failure)));
        error.hidden = false;
      });
    }
    const resume = UI.button("Resume reviews", function () { run("pause.resume", {}); }, "primary small");
    // Never keeps a selection: each pick is an action, not a setting.
    const picker = UI.select({ label: "Pause automatic reviews", placeholder: "Pause for\u2026", options: PAUSE_CHOICES,
      value: undefined, cls: "pause-select",
      onchange: function (hours) { run("pause.start", { hours: hours }); return false; } });
    return { resume: resume, start: picker.node, picker: picker, error: error, run: run, busy: function () { return busy; } };
  }

  // The pause, next to the review mode (Dashboard, General): its state,
  // the countdown, and what a pause does — a timed, daemon-wide hold on
  // automatic reviews that ends by itself. `pause` is pauseState() or null
  // (unknown: daemon offline); `controls` pauseControls() (the "Pause for…"
  // choices, Resume while paused, the inline error); `caption` a small
  // label over the state (the Dashboard's "Pause").
  function pausePanel(UI, pause, controls, caption) {
    const c = controls || {};
    const time = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
    let title, text;
    if (!pause) {
      title = "Pause unknown";
      text = "The PRX daemon isn't running, so its pause can't be read.";
    } else if (pause.paused) {
      title = "Paused, " + pause.left;
      text = "Until " + time.format(new Date(pause.until)) + ". Automatic reviews wait in every repo; reviews you ask for still run. The review mode is unchanged." +
        (c.start ? " Pick a time to restart the pause from now." : "");
    } else {
      title = "Not paused";
      text = "A pause holds automatic reviews in every repo for a set time, then ends by itself. Reviews you ask for still run." +
        (c.start ? "" : " Start one from the menu bar or with `prx pause --hours 2`.");
    }
    return UI.el("div", { class: "pause-panel" + (pause && pause.paused ? " paused" : ""), role: "status" }, [
      UI.el("span", { class: "pause-glyph", "aria-hidden": "true" }, [UI.icon("pause")]),
      UI.el("div", { class: "pause-text" }, [
        caption ? UI.el("p", { class: "posture-label", text: caption }) : null,
        UI.el("p", { class: "pause-title", text: title }),
        UI.el("p", { class: "help" }, UI.rich(text)),
        pause && c.start ? UI.el("div", { class: "pause-actions" }, [c.start]) : null,
        c.error || null,
      ]),
      pause && pause.paused && c.resume ? c.resume : null,
    ]);
  }

  function failure(UI, error, retry) {
    return UI.empty("alert", "Couldn't load this page", error.message || String(error),
      retry ? [UI.button("Try again", retry)] : []);
  }

  // Run `fetch()` (returning a promise) for a mounted page, dropping the
  // answer when the page was switched away meanwhile, and keeping the
  // sidebar's daemon indicator honest. A refresh asked for while one is in
  // flight (a toggle changed the query) drops that answer and fetches
  // again. Returns {refresh}.
  function poller(app, fetch, done) {
    const mount = app.mount();
    let busy = false, again = false;
    function settle(error, value) {
      busy = false;
      if (!app.isMounted(mount)) return undefined;
      if (again) { again = false; return refresh(); }
      if (error) {
        if (isOffline(error)) app.online(false);
      } else {
        app.online(true);
      }
      done(error, value);
      return undefined;
    }
    function refresh() {
      if (!app.isMounted(mount)) return Promise.resolve();
      if (busy) { again = true; return Promise.resolve(); }
      busy = true;
      return fetch().then(function (value) { return settle(null, value); }, function (error) { return settle(error || new Error("failed"), null); });
    }
    return { refresh: refresh };
  }

  const api = {
    GROUPS: GROUPS, STATES: STATES, VERDICT_STATES: VERDICT_STATES, stateOf: stateOf, stateView: stateView, pill: pill,
    usageBreakdown: usageBreakdown, watchesRepos: watchesRepos, isTerminal: isTerminal, readyForApproval: readyForApproval, groupOf: groupOf, groupLabel: groupLabel,
    needsAttention: needsAttention, stateLabel: stateLabel, verdictLabel: verdictLabel, modeLabel: modeLabel,
    splitKey: splitKey, compareKeys: compareKeys, formatTokens: formatTokens, formatCost: formatCost,
    formatCount: formatCount, shortAge: shortAge, ago: ago, tokenTotal: tokenTotal, hasUsage: hasUsage,
    usageLine: usageLine, entryKey: entryKey, EVENTS: EVENTS, eventLabel: eventLabel, findings: findings, eventSummary: eventSummary, isOffline: isOffline, offline: offline, failure: failure, poller: poller,
    pauseLeft: pauseLeft, pauseState: pauseState, pausePanel: pausePanel,
    PAUSE_CHOICES: PAUSE_CHOICES, pauseErrorText: pauseErrorText, pauseControls: pauseControls,
  };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.prxOverview = Object.freeze(api);
})(this);
