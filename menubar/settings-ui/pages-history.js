// History (app-window spec): history events newest first with the engine,
// tokens and a "not posted" marker; filters by repo (sent to the daemon),
// event type and engine; "Load more" pages back with `before`; a completed
// review opens to its brief and review / brief usage, and an on-demand
// brief run to its brief and usage. Reads
// history.list. Pure paging and filtering are exported for the node tests.
(function (root) {
  "use strict";
  const node = typeof module !== "undefined" && module.exports;
  const O = node ? require("./overview.js") : root.prxOverview;

  const PAGE = 50;

  // history.list params: the first page, or the page older than `before`
  // (the oldest loaded entry's `ts`).
  function historyQuery(filter, before) {
    const q = { limit: PAGE };
    if (filter && filter.repo) q.repo = filter.repo;
    if (before) q.before = before;
    return q;
  }

  // A stable identity for an entry (paging may overlap at a timestamp).
  function entryId(e) { return [e.ts, e.event, e.repo, e.number, e.agent || ""].join("|"); }

  // Append an older page; `done` once a page comes back short.
  function mergeOlder(entries, page, limit) {
    const seen = new Set(entries.map(entryId));
    return { entries: entries.concat(page.filter(function (e) { return !seen.has(entryId(e)); })), done: page.length < (limit || PAGE) };
  }

  // A poll's first page: anything new goes on top, in order.
  function mergeNewer(entries, fresh) {
    const seen = new Set(entries.map(entryId));
    return fresh.filter(function (e) { return !seen.has(entryId(e)); }).concat(entries);
  }

  function oldestTs(entries) { return entries.length ? entries[entries.length - 1].ts : null; }

  // filter: {repo, event, engine}; "" means any. Engine is the run's agent.
  function filterEntries(entries, filter) {
    return entries.filter(function (e) {
      return (!filter.repo || e.repo === filter.repo) && (!filter.event || e.event === filter.event) &&
        (!filter.engine || e.agent === filter.engine);
    });
  }

  function names(entries, pick) {
    const out = [];
    entries.forEach(function (e) { const v = pick(e); if (v && out.indexOf(v) === -1) out.push(v); });
    return out.sort();
  }
  function engineNames(entries) { return names(entries, function (e) { return e.agent; }); }
  function repoNames(entries) { return names(entries, function (e) { return e.repo; }); }

  function entryView(e, now) {
    return {
      id: entryId(e), ts: e.ts, when: O.ago(e.ts, now), key: O.entryKey(e), title: e.title, event: e.event,
      eventLabel: O.eventLabel(e.event), summary: O.eventSummary(e), engine: e.agent || "",
      usage: O.usageLine(e.usage), notPosted: e.posted === false,
      // Fixed trailing columns: total tokens, then the reported cost.
      tokens: O.hasUsage(e.usage) ? O.formatTokens(O.tokenTotal(e.usage)) + " tokens" : "",
      cost: e.usage && e.usage.cost_usd !== undefined && e.usage.cost_usd !== null ? O.formatCost(e.usage.cost_usd) : "",
      stateId: rowState(e),
      hasDetail: e.event === "review_completed" || e.event === "review_failed" || e.event === "brief",
    };
  }

  // The pill a row shows (verdicts and failures), or null.
  function rowState(e) {
    if (e.event === "review_completed") return O.VERDICT_STATES[e.verdict] || null;
    if (e.event === "review_failed") return "failed";
    return null;
  }

  // [label, value] rows of one usage record, or null when unknown. The
  // same definition as every other page: input excludes cache, and Total
  // (the row's "tokens") is every kind.
  function usageRows(u) {
    if (!O.hasUsage(u)) return null;
    const rows = [];
    [["input_tokens", "Input"], ["output_tokens", "Output"], ["cache_read_tokens", "Cache read"], ["cache_write_tokens", "Cache write"]]
      .forEach(function (f) { if (u[f[0]] !== undefined && u[f[0]] !== null) rows.push([f[1], O.formatTokens(u[f[0]])]); });
    if (rows.length) rows.push(["Total", O.formatTokens(O.tokenTotal(u))]);
    if (u.cost_usd !== undefined && u.cost_usd !== null) rows.push(["Cost", O.formatCost(u.cost_usd)]);
    if (u.model) rows.push(["Model", u.model]);
    if (u.session_id) rows.push(["Session", u.session_id]);
    return rows;
  }

  function detailModel(e) {
    return {
      // `brief` since the rename; older lines carry `exec_summary`. A brief
      // event's own usage is the brief run's (design B3).
      brief: e.brief || e.exec_summary || null,
      review: e.event === "brief" ? null : usageRows(e.usage),
      briefUsage: usageRows(e.event === "brief" ? e.usage : e.summary_usage),
      duration: e.duration_seconds ? Math.round(e.duration_seconds / 60 * 10) / 10 + " min" : null,
      blocking: e.blocking_texts || [], posted: e.posted === undefined ? null : e.posted,
    };
  }

  const api = { PAGE: PAGE, historyQuery: historyQuery, entryId: entryId, mergeOlder: mergeOlder, mergeNewer: mergeNewer,
    oldestTs: oldestTs, filterEntries: filterEntries, engineNames: engineNames, repoNames: repoNames, entryView: entryView,
    usageRows: usageRows, detailModel: detailModel, rowState: rowState };
  if (node) { module.exports = api; return; }

  // ---- window -------------------------------------------------------------

  const UI = root.prxForm;
  const bridge = root.prxBridge;
  const renderers = root.prxPageRenderers = root.prxPageRenderers || {};
  const el = UI.el;

  // A toolbar dropdown (form.js select). The id is stable across redraws,
  // so focus comes back to it after a pick re-renders the toolbar.
  function select(options, value, label, onchange) {
    return UI.select({ id: "history-" + label.toLowerCase().replace(/[^a-z]+/g, "-"), label: label, cls: "toolbar-select",
      options: options.map(function (o) { return { value: o[0], label: o[1] }; }), value: value, onchange: onchange }).node;
  }

  function facts(rows) {
    return el("dl", { class: "facts" }, [].concat.apply([], rows.map(function (r) {
      const cls = r[0] === "Total" ? "total" : null;
      return [el("dt", { class: cls, text: r[0] }), el("dd", { class: cls, text: r[1] })];
    })));
  }

  function detail(e) {
    const d = detailModel(e);
    const parts = [];
    if (d.brief) parts.push(el("div", { class: "detail-block" }, [el("h3", { text: "Brief" }), el("p", { text: d.brief })]));
    if (e.event === "brief" && !d.brief) parts.push(el("p", { class: "muted", text: "This brief run failed; the previous brief was kept." }));
    if (d.blocking.length) parts.push(el("div", { class: "detail-block" }, [el("h3", { text: "Blocking findings" }),
      el("ul", {}, d.blocking.map(function (t) { return el("li", { text: t }); }))]));
    const usage = [];
    if (e.event !== "brief") {
      usage.push(el("div", {}, [el("h3", { text: "Review usage" }), d.review ? facts(d.review) : el("p", { class: "muted", text: "Not reported by the engine." })]));
    }
    if (e.event === "review_completed" || e.event === "brief") {
      usage.push(el("div", {}, [el("h3", { text: "Brief usage" }), d.briefUsage ? facts(d.briefUsage)
        : el("p", { class: "muted", text: e.event === "brief" ? "Not reported by the engine."
          : d.brief ? "No separate brief run (the review wrote it), or the engine reported nothing." : "No brief for this run." })]));
    }
    parts.push(el("div", { class: "detail-usage" }, usage));
    const meta = [];
    if (d.duration) meta.push("Took " + d.duration);
    if (d.posted === false) meta.push("Not posted to GitHub");
    if (d.posted === true) meta.push("Posted to GitHub");
    if (meta.length) parts.push(el("p", { class: "muted", text: meta.join(" · ") }));
    return el("div", { class: "history-detail" }, parts);
  }

  renderers.history = function (rootNode, app) {
    const view = app.view("history", function () {
      return { filter: { repo: "", event: "", engine: "" }, entries: [], done: false, loaded: false, open: null, repos: [], engines: [] };
    });
    const bar = el("div", { class: "toolbar" });
    const listBox = el("div", { class: "table-box", "aria-busy": view.loaded ? null : "true" });
    const more = el("div", { class: "load-more" });
    rootNode.append(UI.header("History", "Everything PRX did, newest first. Select a review to see its brief and token usage."), bar, listBox, more);
    listBox.append(el("div", { class: "skeleton" }, [0, 1, 2].map(function () { return el("div", { class: "skeleton-row" }); })));

    function remember() {
      view.repos = repoNames(view.entries).concat(view.repos).filter(function (r, i, all) { return all.indexOf(r) === i; }).sort();
      view.engines = engineNames(view.entries).concat(view.engines).filter(function (r, i, all) { return all.indexOf(r) === i; }).sort();
    }

    function drawBar() {
      bar.replaceChildren(
        select([["", "All repos"]].concat(view.repos.map(function (r) { return [r, r]; })), view.filter.repo, "Filter by repo", function (v) {
          view.filter.repo = v;
          view.entries = [];
          view.loaded = false;
          view.done = false;
          listBox.setAttribute("aria-busy", "true");
          live.refresh();
        }),
        select([["", "All events"]].concat(O.EVENTS), view.filter.event, "Filter by event", function (v) { view.filter.event = v; draw(); }),
        select([["", "All engines"]].concat(view.engines.map(function (r) { return [r, r]; })), view.filter.engine, "Filter by engine",
          function (v) { view.filter.engine = v; draw(); }));
    }

    function draw() {
      drawBar();
      if (!view.loaded) return;
      listBox.removeAttribute("aria-busy");
      const now = Date.now();
      const shown = filterEntries(view.entries, view.filter);
      if (!view.entries.length) {
        listBox.replaceChildren(UI.empty("clock", "No history yet", "Reviews, skips and merges show up here as PRX sees them."));
      } else if (!shown.length) {
        listBox.replaceChildren(el("p", { class: "list-empty", text: "No loaded event matches these filters." + (view.done ? "" : " Load more to search further back.") }));
      } else {
        listBox.replaceChildren(el("div", { class: "panel list history" }, shown.map(function (e) {
          const v = entryView(e, now);
          const open = view.open === v.id;
          const tags = [v.stateId ? O.pill(UI, v.stateId, v.summary) : el("span", { class: "ev", text: v.summary })];
          if (v.notPosted) tags.push(el("span", { class: "badge warn", text: "Not posted" }));
          if (v.engine) tags.push(el("span", { class: "badge", text: v.engine }));
          // Two lines (PR, then what happened) and two fixed trailing
          // columns (tokens, cost), so rows scan as a table.
          const line = [
            el("time", { class: "list-when", datetime: v.ts, title: new Date(v.ts).toLocaleString(), text: v.when }),
            el("span", { class: "history-main" }, [
              el("span", { class: "list-main" }, [el("span", { class: "list-key", text: v.key }), el("span", { class: "list-title", text: v.title })]),
              el("span", { class: "ev-tags" }, tags),
            ]),
            el("span", { class: "tokens-line", text: v.tokens }),
            el("span", { class: "cost", text: v.cost }),
          ];
          const row = v.hasDetail
            ? el("button", { type: "button", class: "list-row", "aria-expanded": open ? "true" : "false",
              onclick: function () { view.open = open ? null : v.id; draw(); } }, line)
            : el("div", { class: "list-row" }, line);
          return el("div", { class: "history-item" + (open ? " open" : "") }, [row, open ? detail(e) : null]);
        })));
      }
      more.replaceChildren(view.done ? el("p", { class: "muted", text: view.entries.length ? "That's everything." : "" })
        : UI.button("Load more", loadMore));
    }

    function failed(error) {
      listBox.removeAttribute("aria-busy");
      listBox.replaceChildren(O.isOffline(error) ? O.offline(UI, live.refresh) : O.failure(UI, error, live.refresh));
      more.replaceChildren();
    }

    function loadMore() {
      const mount = app.mount();
      more.replaceChildren(el("p", { class: "muted", text: "Loading…" }));
      bridge.call("history.list", historyQuery(view.filter, oldestTs(view.entries))).then(function (result) {
        const merged = mergeOlder(view.entries, result.entries || []);
        view.entries = merged.entries;
        view.done = merged.done;
        remember();
        if (app.isMounted(mount)) draw();
      }, function (error) { if (app.isMounted(mount)) failed(error); });
    }

    const live = O.poller(app, function () { return bridge.call("history.list", historyQuery(view.filter, null)); }, function (error, result) {
      if (error) { failed(error); return; }
      const fresh = result.entries || [];
      if (!view.loaded) {
        view.entries = fresh;
        view.done = fresh.length < PAGE;
        view.loaded = true;
      } else {
        view.entries = mergeNewer(view.entries, fresh);
      }
      remember();
      // Don't rebuild a filter the user is choosing from.
      const active = document.activeElement;
      if (active && rootNode.contains(active) && active.tagName === "SELECT") return;
      draw();
    });
    draw();
    live.refresh();
    return live;
  };
})(this);
