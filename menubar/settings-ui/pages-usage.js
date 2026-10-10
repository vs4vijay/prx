// Usage & stats (app-window spec): bucket / window / group toggles, a
// stacked-bar chart of tokens per bucket (chart.js), per-group totals
// sorted by tokens, the overall totals with runs lacking usage data called
// out, and review insights over the same window. Reads usage.get and
// insights.get; an older daemon without /usage (error kind `unsupported`)
// gets an "update the daemon" note. Pure mappings are exported for tests.
(function (root) {
  "use strict";
  const node = typeof module !== "undefined" && module.exports;
  const O = node ? require("./overview.js") : root.prxOverview;

  const BUCKETS = [{ value: "day", label: "Day" }, { value: "week", label: "Week" }, { value: "month", label: "Month" }];
  const WINDOWS = [{ value: "7d", label: "7 days" }, { value: "30d", label: "30 days" }, { value: "90d", label: "90 days" }, { value: "all", label: "All time" }];
  const GROUPINGS = [{ value: "none", label: "None" }, { value: "engine", label: "Engine" }, { value: "repo", label: "Repo" },
    { value: "pr", label: "PR" }, { value: "session", label: "Session" }];
  // The chart's bucket follows the range (audit P1: one decision fewer):
  // days up to a month, weeks for 90 days, months for all time.
  const RANGE_BUCKETS = { "7d": "day", "30d": "day", "90d": "week", all: "month" };
  function bucketFor(window) { return RANGE_BUCKETS[window] || DEFAULTS.bucket; }
  const DEFAULTS = { window: "30d", group: "none", bucket: "day" };
  // Stacks drawn individually; the rest are summed as "Other".
  const MAX_SERIES = 6;
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const REPORTING = "Usage comes from the built-in `claude` and `codex` engines, LLM API engines, and agents with `usage_output` set. Other engines (such as `pi`) report none.";

  // Toggle state -> usage.get params (notes.md /usage query).
  function usageQuery(view) {
    const window = view.window || DEFAULTS.window;
    const q = { bucket: view.bucket || bucketFor(window), window: window, group: view.group || DEFAULTS.group };
    if (view.repo) q.repo = view.repo;
    return q;
  }

  // The same window for insights.get (`days`; all time sends none).
  function insightsQuery(view) {
    const w = view.window || DEFAULTS.window;
    return w === "all" ? {} : { days: Number(w.slice(0, -1)) };
  }

  // "2026-10-05" -> "Oct 5" (day, week) or "Oct '26" (month).
  function bucketLabel(start, bucket) {
    const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(start || "");
    if (!m) return start || "";
    const month = MONTHS[Number(m[2]) - 1];
    return bucket === "month" ? month + " '" + m[1].slice(2) : month + " " + Number(m[3]);
  }

  // A group key for display: "all" -> "All runs", "claude:abc…" -> "claude · abc…".
  function groupName(name, group) {
    if (group === "none" || name === "all") return "All runs";
    if (group === "session") {
      const cut = name.indexOf(":");
      const engine = cut === -1 ? name : name.slice(0, cut), id = cut === -1 ? "" : name.slice(cut + 1);
      if (!id || id === "none") return engine + " · no session";
      return engine + " · " + (id.length > 14 ? id.slice(0, 12) + "…" : id);
    }
    return name;
  }

  // Input (with cache reads and writes) and output of a Totals.
  function inOut(t) {
    return { input: (t.input_tokens || 0) + (t.cache_read_tokens || 0) + (t.cache_write_tokens || 0), output: t.output_tokens || 0 };
  }

  // usage -> chart.js data: one label per bucket, the biggest MAX_SERIES-1
  // groups as their own stacks (all of them when they fit), the rest as Other.
  function chartData(usage) {
    const series = usage.series || [];
    const total = {};
    (usage.groups || []).forEach(function (g) { total[g.group] = O.tokenTotal(g.totals); });
    const ranked = series.slice().sort(function (a, b) { return (total[b.group] || 0) - (total[a.group] || 0) || (a.group < b.group ? -1 : 1); });
    const keep = ranked.length > MAX_SERIES ? ranked.slice(0, MAX_SERIES - 1) : ranked;
    const rest = ranked.slice(keep.length);
    const points = series.length ? series[0].points : [];
    const out = keep.map(function (s, i) {
      return { name: groupName(s.group, usage.group), color: i, values: s.points.map(function (p) { return inOut(p.totals); }) };
    });
    if (rest.length) {
      out.push({ name: "Other (" + rest.length + ")", color: keep.length, values: points.map(function (_, i) {
        return rest.reduce(function (sum, s) { const v = inOut(s.points[i].totals); return { input: sum.input + v.input, output: sum.output + v.output }; },
          { input: 0, output: 0 });
      }) });
    }
    return { labels: points.map(function (p) { return bucketLabel(p.bucket_start, usage.bucket); }), series: out };
  }

  // Per-group totals table rows, most tokens first.
  function groupRows(usage) {
    return (usage.groups || []).map(function (g) {
      const t = g.totals;
      return { group: g.group, name: groupName(g.group, usage.group), total: O.tokenTotal(t), input: t.input_tokens || 0, output: t.output_tokens || 0,
        cacheRead: t.cache_read_tokens || 0, cacheWrite: t.cache_write_tokens || 0, runs: t.runs || 0,
        cost: t.runs_with_cost ? O.formatCost(t.cost_usd) : "—", summaryTokens: O.tokenTotal(t.summary) };
    }).sort(function (a, b) { return b.total - a.total || (a.name < b.name ? -1 : 1); });
  }

  function overallModel(usage) {
    const t = usage.overall || {};
    const without = usage.runs_without_usage || 0;
    return {
      total: O.tokenTotal(t), input: t.input_tokens || 0, output: t.output_tokens || 0, cacheRead: t.cache_read_tokens || 0,
      cacheWrite: t.cache_write_tokens || 0, runs: t.runs || 0, runsWithCost: t.runs_with_cost || 0,
      cost: t.runs_with_cost ? O.formatCost(t.cost_usd) : null,
      review: O.tokenTotal(t.review), summary: O.tokenTotal(t.summary),
      withoutUsage: without,
      callout: without ? O.formatCount(without, "run") + " without usage data " + (without === 1 ? "is" : "are") + " not in these totals." : null,
      costNote: t.runs_with_cost && t.runs_with_cost < (t.runs || 0) ? "Cost covers the " + O.formatCount(t.runs_with_cost, "run") + " whose engine reported one." : null,
    };
  }

  // No usage at all in the window: say so instead of an empty chart.
  function isEmpty(usage) {
    const t = usage.overall;
    return !t || (O.tokenTotal(t) === 0 && !t.runs_with_cost);
  }

  function pct(part, whole) { return whole ? Math.round((part / whole) * 100) : 0; }

  // insights.get -> panel model: counts, verdict mix, effectiveness.
  function insightsModel(ins) {
    const repos = ins.per_repo || [];
    const sum = function (k) { return repos.reduce(function (n, r) { return n + (r[k] || 0); }, 0); };
    const verdicts = [["approved", "Approved"], ["changes_requested", "Changes requested"], ["comment_only", "Commented"]].map(function (v) {
      return { id: v[0], label: v[1], count: sum(v[0]) };
    });
    const reviewed = verdicts.reduce(function (n, v) { return n + v.count; }, 0);
    verdicts.forEach(function (v) { v.pct = pct(v.count, reviewed); });
    const eff = ins.effectiveness || {};
    const merge = ins.review_to_merge || {};
    return {
      reviews: (ins.reviews && ins.reviews.total) || 0, failed: (ins.reviews && ins.reviews.failed) || 0, skipped: (ins.reviews && ins.reviews.skipped) || 0,
      verdicts: verdicts,
      effectiveness: eff.changes_requested_reviews ? {
        rate: Math.round((eff.rate || 0) * 100) + "%",
        detail: eff.followed_by_approved + " of " + O.formatCount(eff.changes_requested_reviews, "changes-requested review") + " later approved",
      } : null,
      mergeLatency: merge.samples && merge.average_seconds ? Math.round(merge.average_seconds / 360) / 10 + " h on average (" + O.formatCount(merge.samples, "PR") + ")" : null,
      topBlocking: (ins.top_blocking_issues || []).slice(0, 5),
    };
  }

  const api = { BUCKETS: BUCKETS, WINDOWS: WINDOWS, GROUPINGS: GROUPINGS, DEFAULTS: DEFAULTS, MAX_SERIES: MAX_SERIES, REPORTING: REPORTING,
    RANGE_BUCKETS: RANGE_BUCKETS, bucketFor: bucketFor, usageQuery: usageQuery, insightsQuery: insightsQuery, bucketLabel: bucketLabel, groupName: groupName, chartData: chartData,
    groupRows: groupRows, overallModel: overallModel, isEmpty: isEmpty, insightsModel: insightsModel };
  if (node) { module.exports = api; return; }

  // ---- window -------------------------------------------------------------

  const UI = root.prxForm;
  const Chart = root.prxChart;
  const bridge = root.prxBridge;
  const renderers = root.prxPageRenderers = root.prxPageRenderers || {};
  const el = UI.el;

  function stat(value, label, detail) {
    return el("div", { class: "stat static" }, [el("span", { class: "stat-value", text: String(value) }),
      el("span", { class: "stat-label", text: label }), detail ? el("span", { class: "stat-detail", text: detail }) : null]);
  }

  // The missing-usage note: short, with the reason one click away.
  function why(text) {
    return el("div", { class: "banner warn", role: "status" }, [UI.icon("alert"), el("div", { class: "banner-text" }, [
      el("span", { text: text + " " }),
      el("details", { class: "why" }, [el("summary", { text: "Why?" }), el("p", { class: "help" }, UI.rich(REPORTING))]),
    ])]);
  }

  function usageView(usage) {
    const nodes = [];
    const o = overallModel(usage);
    if (isEmpty(usage)) {
      nodes.push(UI.empty("chart", "No token usage in this window", REPORTING));
      if (o.callout) nodes.push(why(o.callout));
      return nodes;
    }
    nodes.push(el("div", { class: "stats" }, [
      stat(O.formatTokens(o.total), "Tokens", O.usageBreakdown(usage.overall)),
      stat(o.cost || "—", "Reported cost", o.costNote || (o.cost ? null : "No engine reported a cost")),
      stat(o.runs, "Runs", o.summary ? O.formatTokens(o.summary) + " tokens on briefs" : null),
    ]));
    if (o.callout) nodes.push(why(o.callout));
    nodes.push(el("section", {}, [el("h2", { text: "Tokens per " + usage.bucket }), el("div", { class: "panel chart-panel" }, [
      Chart.render(chartData(usage), { width: 680, height: 220, format: O.formatTokens, label: "Tokens per " + usage.bucket,
        title: function (s, i, part, value) { return s.name + ", " + part + ": " + O.formatTokens(value) + " tokens"; } }),
    ])]));
    const rows = groupRows(usage);
    const head = ["Group", "Tokens", "Input", "Output", "Cache read", "Cache write", "Runs", "Cost"];
    nodes.push(el("section", {}, [el("h2", { text: usage.group === "none" ? "Totals" : "Per " + (usage.group === "pr" ? "PR" : usage.group) }),
      el("div", { class: "table-box" }, [el("table", { class: "data numbers" }, [
        el("thead", {}, [el("tr", {}, head.map(function (h) { return el("th", { scope: "col", text: h }); }))]),
        el("tbody", {}, rows.map(function (r, i) {
          return el("tr", {}, [
            el("td", {}, [el("span", { class: "swatch c" + ((i < MAX_SERIES ? i : MAX_SERIES - 1) % Chart.COLORS + 1) }), el("span", { text: r.name })]),
            el("td", { class: "num strong", text: O.formatTokens(r.total) }), el("td", { class: "num", text: O.formatTokens(r.input) }),
            el("td", { class: "num", text: O.formatTokens(r.output) }), el("td", { class: "num", text: O.formatTokens(r.cacheRead) }),
            el("td", { class: "num", text: O.formatTokens(r.cacheWrite) }), el("td", { class: "num", text: String(r.runs) }),
            el("td", { class: "num", text: r.cost }),
          ]);
        })),
      ])])]));
    return nodes;
  }

  function insightsView(ins) {
    const m = insightsModel(ins);
    const mix = el("div", { class: "mix", role: "img", "aria-label": m.verdicts.map(function (v) { return v.label + " " + v.pct + "%"; }).join(", ") },
      m.verdicts.filter(function (v) { return v.count; }).map(function (v) {
        const part = el("span", { class: "mix-part v-" + v.id, title: v.label + ": " + v.count });
        part.style.flexGrow = String(v.count);
        return part;
      }));
    const rows = [
      el("div", { class: "insight" }, [el("span", { class: "row-label", text: "Reviews" }),
        el("span", { text: m.reviews + " completed · " + m.failed + " failed · " + m.skipped + " skipped" })]),
      el("div", { class: "insight" }, [el("span", { class: "row-label", text: "Verdicts" }), el("div", { class: "mix-box" }, [mix,
        el("ul", { class: "chart-legend" }, m.verdicts.map(function (v) {
          return el("li", {}, [el("span", { class: "swatch v-" + v.id }), document.createTextNode(v.label + " " + v.count + " (" + v.pct + "%)")]);
        }))])]),
      el("div", { class: "insight" }, [el("span", { class: "row-label", text: "Effectiveness" }),
        el("span", { text: m.effectiveness ? m.effectiveness.rate + " — " + m.effectiveness.detail : "No changes-requested reviews yet." })]),
    ];
    if (m.mergeLatency) rows.push(el("div", { class: "insight" }, [el("span", { class: "row-label", text: "Review to merge" }), el("span", { text: m.mergeLatency })]));
    if (m.topBlocking.length) {
      rows.push(el("div", { class: "insight" }, [el("span", { class: "row-label", text: "Top blocking issues" }),
        el("ol", { class: "top-issues" }, m.topBlocking.map(function (t) { return el("li", { text: t.text + " (" + t.count + ")" }); }))]));
    }
    return el("section", {}, [el("h2", { text: "Review insights" }), el("div", { class: "panel insights" }, rows)]);
  }

  renderers.usage = function (rootNode, app) {
    const view = app.view("usage", function () { return { window: DEFAULTS.window, group: DEFAULTS.group }; });
    const body = el("div", { class: "overview-body", "aria-busy": "true" });
    function toggle(id, options, key, label) {
      return UI.segmented("usage-" + id, options, view[key], function (v) {
        view[key] = v;
        body.setAttribute("aria-busy", "true");
        live.refresh();
      }, label).node;
    }
    function labelled(text, id, options, key) {
      return el("div", { class: "control-group" }, [el("span", { class: "control-label", id: "usage-" + id + "-label", text: text }),
        toggle(id, options, key, text)]);
    }
    rootNode.append(UI.header("Usage & stats", "Tokens and reported cost of every review and brief run, and how the reviews turned out."),
      el("div", { class: "toolbar toggles" }, [labelled("Range", "window", WINDOWS, "window"), labelled("Group by", "group", GROUPINGS, "group")]),
      body);
    body.append(el("div", { class: "skeleton" }, [0, 1, 2].map(function () { return el("div", { class: "skeleton-row" }); })));

    const live = O.poller(app, function () {
      const usage = bridge.call("usage.get", usageQuery(view)).then(function (u) { return { usage: u, error: null }; },
        function (error) { if (O.isOffline(error)) throw error; return { usage: null, error: error }; });
      const insights = bridge.call("insights.get", insightsQuery(view)).then(null, function (error) { if (O.isOffline(error)) throw error; return null; });
      return Promise.all([usage, insights]);
    }, function (error, values) {
      body.removeAttribute("aria-busy");
      if (error) {
        body.replaceChildren(O.isOffline(error) ? O.offline(UI, live.refresh) : O.failure(UI, error, live.refresh));
        return;
      }
      const nodes = [];
      const u = values[0];
      if (u.error && u.error.kind === "unsupported") {
        nodes.push(UI.empty("alert", "Update the PRX daemon", "The running daemon doesn't serve usage statistics yet. Update PRX and restart the daemon (`prx daemon restart`); the other pages keep working."));
      } else if (u.error) {
        nodes.push(O.failure(UI, u.error, live.refresh));
      } else {
        nodes.push.apply(nodes, usageView(u.usage));
      }
      if (values[1]) nodes.push(insightsView(values[1]));
      body.replaceChildren.apply(body, nodes);
    });
    live.refresh();
    return live;
  };
})(this);
