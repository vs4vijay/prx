// Pull requests (app-window spec): every tracked PR in a sortable,
// filterable table whose first sort follows `menu.sort`, with the menu bar's
// review actions per row (Review, Review with an engine, Review without
// posting, Review with prompt), a "Review PR…" box taking any PR
// reference, and a detail row per PR (open it from the title) with the
// PR's brief card (task 4.3). Reads status.get, sends review.request and
// brief.request. The pure parts (sort,
// filters, the request each action sends) are exported for the node tests.
(function (root) {
  "use strict";
  const node = typeof module !== "undefined" && module.exports;
  const O = node ? require("./overview.js") : root.prxOverview;

  // [id, header] (audit P0: at most five columns at the default 1000px
  // window). The verdict and its findings live in the State cell; the
  // title, key and author share one cell; the engine and every other fact
  // are in the PR detail.
  const COLUMNS = [
    ["state", "State"], ["key", "Pull request"], ["created", "Age"], ["last_review", "Last review"],
  ];
  // Columns whose natural first click is newest / largest first.
  const DESC_FIRST = ["created", "last_review"];
  const MENU_SORTS = {
    created_desc: { column: "created", dir: "desc" },
    created_asc: { column: "created", dir: "asc" },
    last_review_desc: { column: "last_review", dir: "desc" },
    key: { column: "key", dir: "asc" },
  };
  const STATE_FILTERS = [["", "All states"], ["attention", "Needs attention"]].concat(O.GROUPS);

  // The table's first sort: the menu bar's `menu.sort` (status.menu).
  function initialSort(menu) {
    const sort = MENU_SORTS[(menu && menu.sort) || "created_desc"] || MENU_SORTS.created_desc;
    return { column: sort.column, dir: sort.dir };
  }

  // A header click: flip the current column, or start a new one.
  function toggleSort(sort, column) {
    if (sort.column === column) return { column: column, dir: sort.dir === "asc" ? "desc" : "asc" };
    return { column: column, dir: DESC_FIRST.indexOf(column) === -1 ? "asc" : "desc" };
  }

  function sortValue(pr, column) {
    switch (column) {
      case "state": return O.GROUPS.findIndex(function (g) { return g[0] === O.groupOf(pr); });
      case "title": return (pr.title || "").toLowerCase();
      case "author": return (pr.author || "").toLowerCase();
      case "verdict": return pr.last_verdict || null;
      case "engine": return pr.engine || null;
      case "created": return pr.created_at ? Date.parse(pr.created_at) : null;
      case "last_review": return pr.last_review_at ? Date.parse(pr.last_review_at) : null;
      default: return null;
    }
  }

  // Sorted copy. Missing values (never reviewed, unknown age) always come
  // last; ties fall back to the key order, like the menu (design D8).
  function sortRows(prs, sort) {
    const sign = sort.dir === "desc" ? -1 : 1;
    return prs.slice().sort(function (a, b) {
      if (sort.column === "key") return sign * O.compareKeys(a.key, b.key);
      const x = sortValue(a, sort.column), y = sortValue(b, sort.column);
      if (x === null || y === null) return (x === null) - (y === null) || O.compareKeys(a.key, b.key);
      return sign * (x < y ? -1 : x > y ? 1 : 0) || O.compareKeys(a.key, b.key);
    });
  }

  // filter: {repo, state, text}. `state` is a menu group id, "attention",
  // or "" for all.
  function filterRows(prs, filter) {
    const text = (filter.text || "").trim().toLowerCase();
    return prs.filter(function (pr) {
      if (filter.repo && O.splitKey(pr.key).repo !== filter.repo) return false;
      if (filter.state === "attention" && !O.needsAttention(pr)) return false;
      if (filter.state && filter.state !== "attention" && O.groupOf(pr) !== filter.state) return false;
      if (text && [pr.key, pr.title, pr.author].every(function (v) { return (v || "").toLowerCase().indexOf(text) === -1; })) return false;
      return true;
    });
  }

  function repoNames(prs) {
    const names = [];
    prs.forEach(function (pr) { const r = O.splitKey(pr.key).repo; if (names.indexOf(r) === -1) names.push(r); });
    return names.sort();
  }

  // The engines "Review with" offers for a PR: the daemon's list, with the
  // repo's own engine marked. Empty for an older daemon (no `engines`).
  function engineChoices(status, pr) {
    return ((status && status.engines) || []).map(function (name) {
      return { name: name, label: name + (name === pr.engine ? " (default)" : ""), isDefault: name === pr.engine };
    });
  }

  // Review actions are off for merged / closed PRs (the menu's rule).
  function canReview(pr) { return !O.isTerminal(pr); }

  // The review.request params each row action sends (the menu bar's
  // review actions): action is review | engine | nopost | prompt; opts has
  // engine (engine, prompt), prompt and post (prompt). Only options that
  // differ from the run's defaults are sent.
  function reviewRequest(action, pr, opts) {
    opts = opts || {};
    const req = { target: pr.key };
    if (action === "engine") req.engine = opts.engine;
    if (action === "nopost") req.post = false;
    if (action === "prompt") {
      const prompt = (opts.prompt || "").trim();
      if (prompt) req.extra_prompt = prompt;
      if (opts.engine && opts.engine !== pr.engine) req.engine = opts.engine;
      if (opts.post === false) req.post = false;
    }
    return req;
  }

  // "Review PR…": any reference form goes to the daemon as typed.
  function targetRequest(text) {
    const target = (text || "").trim();
    return target ? { target: target } : null;
  }

  // What to show after review.request answers.
  function reviewNote(result) {
    if (result && result.note) return result.note;
    const queued = (result && result.enqueued) || [];
    return queued.length ? "Queued " + queued.join(", ") + "." : "Nothing was queued.";
  }

  // Display row for a PR.
  function rowModel(pr, now) {
    return {
      key: pr.key, title: pr.title, author: pr.author, url: pr.html_url || "", group: O.groupOf(pr), state: O.stateLabel(pr),
      stateId: O.stateOf(pr), terminal: O.isTerminal(pr),
      age: O.shortAge(pr.created_at, now), verdict: O.verdictLabel(pr.last_verdict),
      findings: !pr.last_verdict ? "" : pr.blocking_count || pr.nit_count ? O.findings(pr.blocking_count, pr.nit_count) : "No findings",
      lastReview: pr.last_review_at ? O.ago(pr.last_review_at, now) : "never", engine: pr.engine || "", summary: pr.summary || "",
      reviewable: canReview(pr),
    };
  }

  const api = { COLUMNS: COLUMNS, STATE_FILTERS: STATE_FILTERS, initialSort: initialSort, toggleSort: toggleSort, sortRows: sortRows,
    filterRows: filterRows, repoNames: repoNames, engineChoices: engineChoices, canReview: canReview, reviewRequest: reviewRequest,
    targetRequest: targetRequest, reviewNote: reviewNote, rowModel: rowModel };
  if (node) { module.exports = api; return; }

  // ---- window -------------------------------------------------------------

  const UI = root.prxForm;
  const bridge = root.prxBridge;
  const renderers = root.prxPageRenderers = root.prxPageRenderers || {};
  const el = UI.el;

  // A toolbar dropdown (form.js select). The id is stable across redraws,
  // so focus comes back to it after a pick re-renders the toolbar.
  function select(options, value, label, onchange) {
    return UI.select({ id: "prs-" + label.toLowerCase().replace(/[^a-z]+/g, "-"), label: label, cls: "toolbar-select",
      options: options.map(function (o) { return { value: o[0], label: o[1] }; }), value: value, onchange: onchange }).node;
  }

  renderers.prs = function (rootNode, app) {
    // View state survives page switches and polls: filters, sort, row notes
    // and the open prompt form.
    const view = app.view("prs", function () { return { filter: { repo: "", state: "", text: "" }, sort: null, notes: {}, prompt: null, pick: "", open: null }; });
    if (view.preset) { view.filter = view.preset; view.preset = null; }
    let status = null;
    const search = el("input", { type: "search", placeholder: "Filter by PR, title, or author", "aria-label": "Filter pull requests", spellcheck: "false" });
    search.value = view.filter.text;
    search.addEventListener("input", function () { view.filter.text = search.value; draw(); });
    const repoBox = el("span");
    const stateSelect = select(STATE_FILTERS, view.filter.state, "Filter by state", function (v) { view.filter.state = v; draw(); });
    const target = el("input", { type: "text", placeholder: "acme/api#42, a PR URL, api#42, or 42", "aria-label": "PR to review", spellcheck: "false" });
    target.value = view.pick;
    const pickNote = el("p", { class: "inline-note", role: "status" });
    function pick() {
      const req = targetRequest(target.value);
      view.pick = target.value;
      if (!req) { target.focus(); return; }
      pickNote.className = "inline-note";
      pickNote.textContent = "Requesting…";
      bridge.call("review.request", req).then(function (result) {
        pickNote.className = "inline-note ok";
        pickNote.textContent = reviewNote(result);
        target.value = view.pick = "";
        live.refresh();
      }, function (error) {
        pickNote.className = "inline-note error";
        pickNote.textContent = O.isOffline(error) ? "The PRX daemon is not running." : error.message;
      });
    }
    target.addEventListener("keydown", function (e) { if (e.key === "Enter") { e.preventDefault(); pick(); } });
    const tableBox = el("div", { class: "table-box", "aria-busy": "true" });
    const count = el("span", { class: "count" });
    rootNode.append(
      UI.header("Pull requests", "Every open PR in your watched repos. Select a title to see its brief and details."),
      el("div", { class: "review-pr" }, [el("label", { class: "row-label", text: "Review PR" }), target, UI.button("Review", pick, "primary"), pickNote]),
      el("div", { class: "toolbar" }, [search, repoBox, stateSelect, count]),
      tableBox);

    const briefs = root.prxBrief.controller(app, bridge, function () { draw(); }, function () { live.refresh(); });

    // The PR detail (task 4.3): the brief card, then the row's facts.
    function detail(pr, r) {
      const facts = [r.verdict ? "Last verdict: " + r.verdict + (r.findings ? " (" + r.findings + ")" : "") : "Not reviewed yet",
        "Last review: " + r.lastReview, r.engine ? "Engine: " + r.engine : null, r.author ? "Author: " + r.author : null,
        r.summary || null].filter(Boolean);
      return el("div", { class: "pr-detail", id: "pr-detail-" + pr.key.replace(/[^A-Za-z0-9_-]/g, "_") }, [
        briefs.card(pr),
        el("p", { class: "pr-facts", text: facts.join(" · ") }),
      ]);
    }

    function run(pr, action, opts) {
      const req = reviewRequest(action, pr, opts);
      view.notes[pr.key] = { kind: "", text: "Requesting…" };
      if (action === "prompt") view.prompt = null;
      draw();
      bridge.call("review.request", req).then(function (result) {
        view.notes[pr.key] = { kind: "ok", text: reviewNote(result) };
        draw();
        live.refresh();
      }, function (error) {
        view.notes[pr.key] = { kind: "error", text: O.isOffline(error) ? "The PRX daemon is not running." : error.message };
        draw();
      });
    }

    function promptForm(pr) {
      const p = view.prompt;
      const text = el("textarea", { rows: 3, placeholder: "e.g. Focus on the migration's rollback path.", "aria-label": "Extra instructions for this review" });
      text.value = p.prompt;
      text.addEventListener("input", function () { p.prompt = text.value; });
      const engines = engineChoices(status, pr);
      const engine = engines.length ? select(engines.map(function (e) { return [e.name, e.label]; }), p.engine || pr.engine, "Engine",
        function (v) { p.engine = v; }) : null;
      const post = el("input", { type: "checkbox", role: "switch", class: "switch", id: "prompt-post" });
      post.checked = p.post;
      post.addEventListener("change", function () { p.post = post.checked; });
      setTimeout(function () { if (text.isConnected) text.focus(); }, 0);
      return el("div", { class: "prompt-form" }, [
        text,
        el("div", { class: "inline" }, [
          engine ? el("span", { class: "check" }, [el("span", { class: "control-label", "aria-hidden": "true", text: "Engine" }), engine]) : null,
          el("label", { class: "check", for: "prompt-post" }, [post, document.createTextNode("Post to GitHub")]),
          el("span", { class: "spacer" }),
          UI.button("Cancel", function () { view.prompt = null; draw(); }),
          UI.button("Review", function () { run(pr, "prompt", { prompt: p.prompt, engine: p.engine, post: p.post }); }, "primary"),
        ]),
      ]);
    }

    function actions(pr, row) {
      const disabled = !row.reviewable;
      const engines = engineChoices(status, pr);
      const items = [];
      if (engines.length) {
        items.push({ heading: "Review with" });
        engines.forEach(function (e) { items.push({ label: e.label, disabled: disabled, onclick: function () { run(pr, "engine", { engine: e.name }); } }); });
        items.push({ separator: true });
      }
      items.push({ label: "Review without posting", disabled: disabled, onclick: function () { run(pr, "nopost"); } });
      items.push({ label: "Review with prompt…", disabled: disabled, onclick: function () {
        view.prompt = { key: pr.key, prompt: "", engine: pr.engine || "", post: true };
        draw();
      } });
      if (row.url) {
        items.push({ separator: true });
        items.push({ label: "Open in GitHub", onclick: function () { window.open(row.url, "_blank", "noopener"); } });
      }
      const review = UI.button("Review", function () { run(pr, "review"); }, "small");
      review.disabled = disabled;
      review.setAttribute("aria-label", "Review " + pr.key);
      return el("div", { class: "row-actions" }, [
        row.url ? el("a", { class: "icon-button", href: row.url, target: "_blank", rel: "noopener", title: "Open in GitHub", "aria-label": "Open " + pr.key + " in GitHub" }, [UI.icon("external")]) : null,
        review, UI.menuButton("More actions for " + pr.key, items),
      ]);
    }

    function draw() {
      if (!status) return;
      const all = status.prs || [];
      const repos = repoNames(all);
      if (view.filter.repo && repos.indexOf(view.filter.repo) === -1) view.filter.repo = "";
      repoBox.replaceChildren(select([["", "All repos"]].concat(repos.map(function (r) { return [r, r]; })), view.filter.repo, "Filter by repo",
        function (v) { view.filter.repo = v; draw(); }));
      if (!view.sort) view.sort = initialSort(status.menu);
      const rows = sortRows(filterRows(all, view.filter), view.sort);
      count.textContent = rows.length === all.length ? O.formatCount(all.length, "PR") : rows.length + " of " + all.length;
      tableBox.removeAttribute("aria-busy");
      if (!all.length) {
        const watching = O.watchesRepos(app.loaded());
        tableBox.replaceChildren(watching === false
          ? UI.empty("repo", "No repos watched yet", "Add a repo or an org on Repos & Orgs, and PRX lists its open PRs here after the next check.",
            [UI.button("Add a repo", function () { app.go("repos"); }, "primary")])
          : UI.empty("pr", "No pull requests yet", "PRX lists the open PRs of your watched repos after its next check of GitHub."));
        return;
      }
      const now = Date.now();
      const head = el("tr", {}, COLUMNS.map(function (c) {
        const active = view.sort.column === c[0];
        return el("th", { scope: "col", class: c[0] === "created" ? "num" : null,
          "aria-sort": active ? (view.sort.dir === "asc" ? "ascending" : "descending") : null }, [
          el("button", { type: "button", class: "sort" + (active ? " active " + view.sort.dir : ""), text: c[1],
            onclick: function () { view.sort = toggleSort(view.sort, c[0]); draw(); } })]);
      }).concat([el("th", { scope: "col" }, [el("span", { class: "sr-only", text: "Actions" })])]));
      const body = el("tbody");
      rows.forEach(function (pr) {
        const r = rowModel(pr, now);
        const open = view.open === pr.key;
        const detailId = "pr-detail-" + pr.key.replace(/[^A-Za-z0-9_-]/g, "_");
        body.append(el("tr", { class: "pr-row" + (r.terminal ? " terminal" : "") + (open ? " open" : "") }, [
          el("td", { class: "state-cell" }, [O.pill(UI, r.stateId, r.state), r.findings ? el("span", { class: "sub", text: r.findings }) : null]),
          el("td", {}, [el("div", { class: "pr-cell" }, [
            el("button", { type: "button", class: "row-open", title: r.title, "aria-expanded": open ? "true" : "false", "aria-controls": detailId,
              onclick: function () { view.open = open ? null : pr.key; draw(); }, text: r.title }),
            el("span", { class: "pr-meta" }, [el("span", { class: "mono", text: r.key }), r.author ? el("span", { text: "· " + r.author }) : null]),
          ])]),
          el("td", { class: "num", text: r.age }),
          el("td", { text: r.lastReview }),
          el("td", {}, [actions(pr, r)]),
        ]));
        const note = view.notes[pr.key];
        if (open) {
          body.append(el("tr", { class: "detail-row open" }, [el("td", { colspan: COLUMNS.length + 1 }, [detail(pr, r)])]));
        }
        if (view.prompt && view.prompt.key === pr.key && r.reviewable) {
          body.append(el("tr", { class: "detail-row" }, [el("td", { colspan: COLUMNS.length + 1 }, [promptForm(pr)])]));
        }
        if (note) {
          body.append(el("tr", { class: "detail-row" }, [el("td", { colspan: COLUMNS.length + 1 }, [
            el("p", { class: "inline-note " + note.kind, role: "status" }, [el("span", { text: note.text }),
              UI.iconButton("close", "Dismiss", function () { delete view.notes[pr.key]; draw(); })])])]));
        }
      });
      if (!rows.length) body.append(el("tr", {}, [el("td", { colspan: COLUMNS.length + 1, class: "list-empty", text: "No PR matches these filters." })]));
      const cols = el("colgroup", {}, ["c-state", "c-pr", "c-age", "c-review", "c-actions"].map(function (c) { return el("col", { class: c }); }));
      tableBox.replaceChildren(el("table", { class: "data prs" }, [cols, el("thead", {}, [head]), body]));
    }

    const live = O.poller(app, function () { return bridge.call("status.get", {}); }, function (error, value) {
      if (error) {
        status = null;
        tableBox.removeAttribute("aria-busy");
        tableBox.replaceChildren(O.isOffline(error) ? O.offline(UI, live.refresh) : O.failure(UI, error, live.refresh));
        return;
      }
      status = value;
      briefs.update(value);
      // Don't rebuild under the user's hands (the prompt form, a select, an
      // open action menu).
      const active = document.activeElement;
      if (active && tableBox.contains(active) && (/^(INPUT|TEXTAREA)$/.test(active.tagName) || active.closest(".menu-anchor, .select"))) return;
      draw();
    });
    live.refresh();
    return live;
  };
})(this);
