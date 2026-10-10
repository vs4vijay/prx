// Dashboard (app-window spec): headline counts, the review mode and pause
// state, the PRs needing attention (each with its brief card, task 4.3),
// the latest history events, and the last 7 days' tokens and cost. Reads status.get, history.list and usage.get;
// refreshed by the app's poll while visible. `dashboardModel` is the pure
// view model (tested against preview-mock.js).
(function (root) {
  "use strict";
  const node = typeof module !== "undefined" && module.exports;
  const O = node ? require("./overview.js") : root.prxOverview;

  const ATTENTION_ROWS = 5;
  const RECENT_ROWS = 6;
  // The bridge calls one refresh makes. History asks for more rows than it
  // shows: Recent activity keeps only the latest event per PR.
  const QUERIES = {
    status: {},
    history: { limit: RECENT_ROWS * 4 },
    usage: { bucket: "day", window: "7d", group: "none" },
  };

  // status: GET /status; history: GET /history ({entries}); usage: GET
  // /usage over 7 days, or null; usageError: the usage.get error (kind
  // `unsupported` = an older daemon); now: ms.
  function dashboardModel(status, history, usage, usageError, now) {
    const prs = (status && status.prs) || [];
    const open = prs.filter(function (pr) { return !O.isTerminal(pr); });
    // Changes requested before reviews in flight, then the newest review.
    const attention = open.filter(O.needsAttention).sort(function (a, b) {
      const rank = function (pr) { return pr.state === "reviewed_changes_requested" ? 0 : 1; };
      return rank(a) - rank(b) || (Date.parse(b.last_review_at || 0) || 0) - (Date.parse(a.last_review_at || 0) || 0) || O.compareKeys(a.key, b.key);
    });
    const paused = !!(status && status.paused_until && Date.parse(status.paused_until) > now);
    const overall = usage && usage.overall;
    return {
      counts: {
        tracked: open.length,
        attention: attention.length,
        ready: open.filter(O.readyForApproval).length,
        running: (status.in_flight || 0) + (status.queue_depth || 0),
        inFlight: status.in_flight || 0,
        queued: status.queue_depth || 0,
      },
      mode: { mode: status.review_mode || "review comment", label: O.modeLabel(status.review_mode), paused: paused,
        pausedUntil: paused ? status.paused_until : null, pause: O.pauseState(status, now) },
      attention: attention.slice(0, ATTENTION_ROWS).map(function (pr) {
        return { key: pr.key, title: pr.title, state: O.stateLabel(pr), stateId: O.stateOf(pr), detail: pr.summary || "",
          blocking: pr.state === "reviewed_changes_requested", pr: pr };
      }),
      attentionMore: Math.max(0, attention.length - ATTENTION_ROWS),
      // The latest event per PR (a "Review started" is dropped once the
      // same PR has a newer outcome).
      recent: latestPerPr((history && history.entries) || []).slice(0, RECENT_ROWS).map(function (e) {
        return { key: O.entryKey(e), title: e.title, when: O.ago(e.ts, now), ts: e.ts, text: O.eventSummary(e), event: e.event,
          stateId: entryState(e) };
      }),
      configError: (status && status.config_error) || null,
      week: overall ? {
        tokens: O.tokenTotal(overall),
        tokensText: O.formatTokens(O.tokenTotal(overall)),
        breakdown: O.usageBreakdown(overall),
        cost: overall.runs_with_cost ? O.formatCost(overall.cost_usd) : null,
        runs: overall.runs,
        withoutUsage: usage.runs_without_usage || 0,
      } : null,
      usageUnsupported: !!usageError && usageError.kind === "unsupported",
    };
  }

  // Newest first in, newest first out; one entry per PR key.
  function latestPerPr(entries) {
    const seen = new Set();
    return entries.filter(function (e) {
      const key = O.entryKey(e);
      if (seen.has(key)) return false;
      seen.add(key);
      return true;
    });
  }

  // The state pill a history event shows (null: plain text only).
  function entryState(e) {
    if (e.event === "review_completed") return O.VERDICT_STATES[e.verdict] || null;
    if (e.event === "review_failed") return "failed";
    if (e.event === "review_started") return "in_review";
    if (e.event === "pr_merged") return "merged";
    if (e.event === "pr_closed") return "closed";
    if (e.event === "review_skipped") return "skipped";
    return null;
  }

  const api = { QUERIES: QUERIES, dashboardModel: dashboardModel, latestPerPr: latestPerPr, entryState: entryState };
  if (node) { module.exports = api; return; }

  // ---- window -------------------------------------------------------------

  const UI = root.prxForm;
  const bridge = root.prxBridge;
  const renderers = root.prxPageRenderers = root.prxPageRenderers || {};
  const el = UI.el;

  function stat(value, label, detail, onclick, tone) {
    return el("button", { type: "button", class: "stat" + (tone ? " " + tone : ""), onclick: onclick }, [
      el("span", { class: "stat-value", text: String(value) }),
      el("span", { class: "stat-label", text: label }),
      detail ? el("span", { class: "stat-detail", text: detail }) : null,
    ]);
  }

  function link(text, onclick) { return UI.button(text, onclick, "link small"); }

  // Open Pull requests with a state filter preset.
  function toPrs(app, state) {
    return function () {
      app.view("prs").preset = { state: state || "", repo: "", text: "" };
      app.go("prs");
    };
  }

  // Above the counts: the broken-config banner, then the review mode and
  // the pause side by side — the lasting posture and the temporary hold.
  function statusBanners(app, m, actions) {
    const nodes = [];
    if (m.configError) {
      nodes.push(UI.banner("error", "alert", "config.yml has an error.",
        "PRX keeps running the last good version until it is fixed: `" + (m.configError.message || "invalid configuration") + "`",
        [UI.button("Open config.yml", function () { app.go("raw"); })]));
    }
    const S = root.prxSchema;
    const option = (S && S.MODE_OPTIONS || []).find(function (o) { return o.value === m.mode.mode; }) || { icon: "review", detail: "" };
    nodes.push(el("div", { class: "posture" }, [
      el("div", { class: "posture-mode" + (m.mode.mode === "off" ? " off" : ""), "data-mode": m.mode.mode }, [
        el("span", { class: "mode-glyph", "aria-hidden": "true" }, [UI.icon(option.icon)]),
        el("div", { class: "pause-text" }, [
          el("p", { class: "posture-label", text: "Review mode" }),
          el("p", { class: "pause-title", text: m.mode.label }),
          el("p", { class: "help", text: option.detail }),
        ]),
        UI.button("Change", function () { app.go("general"); }, "small"),
      ]),
      O.pausePanel(UI, m.mode.pause, actions.pause, "Pause"),
    ]));
    return nodes;
  }

  function build(app, m, briefs, actions) {
    const nodes = statusBanners(app, m, actions);
    // First run: nothing watched yet. Say how to start instead of zeros.
    if (!m.counts.tracked && O.watchesRepos(app.loaded() || actions.config) === false) {
      nodes.push(el("div", { class: "panel onboarding" }, [UI.empty("repo", "Watch your first repo",
        "PRX reviews the open PRs of the repos and orgs you watch. Add one on Repos & Orgs, and its PRs show up here after the next check of GitHub.",
        [UI.button("Add a repo", function () { app.go("repos"); }, "primary")])]));
      return nodes;
    }
    nodes.push(el("div", { class: "stats" }, [
      stat(m.counts.tracked, "Tracked PRs", null, toPrs(app, "")),
      stat(m.counts.attention, "Need attention", null, toPrs(app, "attention"), m.counts.attention ? "alert" : null),
      stat(m.counts.ready, "Ready for approval", null, toPrs(app, "ready"), m.counts.ready ? "good" : null),
      stat(m.counts.running, "Running or queued", m.counts.inFlight + " running · " + m.counts.queued + " queued", toPrs(app, "in_review")),
    ]));
    const attention = m.attention.length ? m.attention.map(function (a) {
      return el("div", { class: "attention-item" }, [
        el("button", { type: "button", class: "list-row", onclick: toPrs(app, "attention") }, [
          O.pill(UI, a.stateId, a.state),
          el("span", { class: "list-main" }, [el("span", { class: "list-key", text: a.key }), el("span", { class: "list-title", text: a.title })]),
          el("span", { class: "list-aside", text: a.detail }),
        ]),
        briefs.card(a.pr),
      ]);
    }) : [el("p", { class: "list-empty", text: "Nothing needs you right now." })];
    if (m.attentionMore) attention.push(el("p", { class: "list-empty", text: "+" + m.attentionMore + " more" }));
    nodes.push(el("section", {}, [
      el("div", { class: "section-head" }, [el("h2", { text: "Needs attention" }), link("All pull requests", toPrs(app, ""))]),
      el("div", { class: "panel list" }, attention),
    ]));
    const recent = m.recent.length ? m.recent.map(function (r) {
      return el("div", { class: "list-row" }, [
        el("time", { class: "list-when", datetime: r.ts, text: r.when }),
        el("span", { class: "list-main" }, [el("span", { class: "list-key", text: r.key }), el("span", { class: "list-title", text: r.title })]),
        el("span", { class: "list-aside" }, [r.stateId ? O.pill(UI, r.stateId, r.text) : el("span", { text: r.text })]),
      ]);
    }) : [el("p", { class: "list-empty", text: "No history yet. Reviews, skips and merges show up here as PRX sees them." })];
    nodes.push(el("section", {}, [
      el("div", { class: "section-head" }, [el("h2", { text: "Recent activity" }), link("History", function () { app.go("history"); })]),
      el("div", { class: "panel list" }, recent),
    ]));
    let usage;
    if (m.week) {
      usage = el("div", { class: "stats" }, [
        stat(m.week.tokensText, "Tokens", m.week.breakdown, function () { app.go("usage"); }),
        stat(m.week.cost || "—", "Reported cost", m.week.cost ? null : "No engine reported a cost", function () { app.go("usage"); }),
        stat(m.week.runs, "Runs", m.week.withoutUsage ? m.week.withoutUsage + " without usage data" : null, function () { app.go("usage"); }),
      ]);
    } else {
      usage = el("p", { class: "list-empty" }, UI.rich(m.usageUnsupported ? "Update the PRX daemon to see token usage (it has no `/usage` endpoint yet)."
        : "Token usage is unavailable right now."));
    }
    nodes.push(el("section", {}, [
      el("div", { class: "section-head" }, [el("h2", { text: "Last 7 days" }), link("Usage & stats", function () { app.go("usage"); })]),
      usage,
    ]));
    return nodes;
  }

  renderers.dashboard = function (rootNode, app) {
    const body = el("div", { class: "overview-body", "aria-busy": "true" }, [el("div", { class: "skeleton" }, [0, 1, 2].map(function () { return el("div", { class: "skeleton-row" }); }))]);
    rootNode.append(UI.header("Dashboard", "What PRX is doing across your watched repos."), body);
    let last = null;
    // The pause's Start / Resume controls (overview.js), kept across redraws.
    const pause = O.pauseControls(UI, bridge, function () { return live.refresh(); });
    function draw() {
      if (!last) return;
      body.replaceChildren.apply(body, build(app, dashboardModel(last[0], last[1], last[2].usage, last[2].error, Date.now()), briefs,
        { pause: pause, config: last[3] }));
    }
    const briefs = root.prxBrief.controller(app, bridge, draw, function () { live.refresh(); });
    const live = O.poller(app, function () {
      const usage = bridge.call("usage.get", QUERIES.usage).then(function (u) { return { usage: u, error: null }; },
        function (error) { if (O.isOffline(error)) throw error; return { usage: null, error: error }; });
      // The first-run state needs the watched repos: ask for the config
      // too until the app has loaded it.
      const config = app.loaded() ? Promise.resolve(null) : bridge.getConfig().then(null, function () { return null; });
      return Promise.all([bridge.call("status.get", QUERIES.status), bridge.call("history.list", QUERIES.history), usage, config]);
    }, function (error, values) {
      body.removeAttribute("aria-busy");
      if (error) {
        last = null;
        body.replaceChildren(O.isOffline(error) ? O.offline(UI, live.refresh) : O.failure(UI, error, live.refresh));
        return;
      }
      last = values;
      briefs.update(values[0]);
      draw();
    });
    live.refresh();
    return live;
  };
})(this);
