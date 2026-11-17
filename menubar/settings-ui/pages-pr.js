// PR details (spec "PR details page"): one tracked PR on its own page,
// opened from the menu bar ("Show details…", the brief line) or the Pull
// requests page, at `#page=pr&pr=<key>` / app.openPr(key). It shows the
// title, key, author, age and state, an actions row, the brief card in
// large readable text, the latest review (verdict, findings, blocking
// finding texts from history), and the PR's facts. Reads status.get and
// history.list (the PR's repo); sends review.preflight, review.request,
// brief.request and app.review_chat.open. Its reviews run the same
// pre-flight as the Pull requests page: a PR that is skipped or over the
// diff caps shows a confirm box first and is sent with `confirm: true` only
// on "Review anyway". A skipped PR says why in its header and facts, and a
// repo whose GitHub polls keep failing gets a banner (the page may be stale).
//
// Extending it: the actions row is built from `actionsFor` descriptors and
// run through the page's `HANDLERS` (add one of each), and `factsFor` lists
// the label/value lines under the review. The pure parts are exported for
// the node tests.
(function (root) {
  "use strict";
  const node = typeof module !== "undefined" && module.exports;
  const O = node ? require("./overview.js") : root.prxOverview;

  // History entries read to find the PR's latest review (one repo's).
  const HISTORY_LIMIT = 200;

  function splitPrKey(key) {
    const cut = String(key || "").lastIndexOf("#");
    return cut < 0 ? { repo: String(key || ""), number: null } : { repo: key.slice(0, cut), number: Number(key.slice(cut + 1)) };
  }

  // "2m 30s", "45s", "1h 5m" (empty for unknown).
  function formatDuration(seconds) {
    if (typeof seconds !== "number" || !isFinite(seconds) || seconds < 0) return "";
    const s = Math.round(seconds);
    if (s < 60) return s + "s";
    if (s < 3600) return Math.floor(s / 60) + "m" + (s % 60 ? " " + (s % 60) + "s" : "");
    return Math.floor(s / 3600) + "h" + (Math.floor(s % 3600 / 60) ? " " + Math.floor(s % 3600 / 60) + "m" : "");
  }

  // The page header's view of a status entry.
  function detailModel(pr, now) {
    const id = O.stateOf(pr);
    return {
      key: pr.key, title: pr.title || pr.key, author: pr.author || "", url: pr.html_url || "",
      stateId: id, state: O.stateLabel(pr), terminal: O.isTerminal(pr), reviewable: !O.isTerminal(pr),
      opened: pr.created_at ? O.ago(pr.created_at, now) : "", draft: !!pr.is_draft,
      // Why prx is not reviewing it ("author dependabot[bot] excluded"; for a
      // PR the diff cap held back, "Review skipped — …"). "" otherwise.
      skip: O.skipNote(pr),
    };
  }

  // The newest `review_completed` entry for `key` in a newest-first history
  // page, or null; `failedSince` when a newer run failed.
  function latestReview(entries, key) {
    const want = splitPrKey(key);
    let failedSince = false;
    const list = entries || [];
    for (let i = 0; i < list.length; i++) {
      const e = list[i];
      if (!e || e.repo !== want.repo || e.number !== want.number) continue;
      if (e.event === "review_failed") failedSince = true;
      if (e.event === "review_completed") return { entry: e, failedSince: failedSince };
    }
    return { entry: null, failedSince: failedSince };
  }

  // The "Latest review" section: from the history entry when there is one,
  // else from the status entry's verdict and counts (history compacted, or
  // older than the page read). `reviewed` is false for a PR never reviewed.
  function reviewModel(pr, latest, now) {
    const e = latest && latest.entry;
    const verdict = e ? e.verdict : pr.last_verdict || null;
    if (!verdict) return { reviewed: false, failedSince: !!(latest && latest.failedSince) };
    const blocking = e ? e.blocking_count || 0 : pr.blocking_count || 0;
    const nits = e ? e.nit_count || 0 : pr.nit_count || 0;
    const texts = e && Array.isArray(e.blocking_texts) ? e.blocking_texts.filter(function (t) { return String(t || "").trim(); }) : [];
    const at = e ? e.ts : pr.last_review_at;
    return {
      reviewed: true,
      verdict: O.verdictLabel(verdict),
      verdictState: O.VERDICT_STATES[verdict] || "commented",
      findings: blocking || nits ? O.findings(blocking, nits) : "No findings",
      when: at ? O.ago(at, now) : "",
      at: at || null,
      engine: (e && e.agent) || "",
      posted: e && typeof e.posted === "boolean" ? (e.posted ? "Posted to GitHub" : "Not posted") : "",
      duration: e ? formatDuration(e.duration_seconds) : "",
      summary: pr.summary || "",
      blocking: texts,
      // Blocking findings the entry counts but has no text for (older lines).
      unlisted: Math.max(0, blocking - texts.length),
      nits: nits,
      failedSince: !!(latest && latest.failedSince),
    };
  }

  // The actions row: `buttons` in order, then the ⋯ menu's `menu` items
  // ({heading} / {separator} allowed). Each action is {id, label, kind?,
  // icon?, engine?, disabled}; the page runs HANDLERS[id].
  function actionsFor(pr, status) {
    const reviewable = !O.isTerminal(pr);
    const buttons = [];
    if (pr.html_url) buttons.push({ id: "open", label: "Open in GitHub", icon: "external", disabled: false });
    buttons.push({ id: "review", label: "Review", kind: "primary", disabled: !reviewable });
    const menu = [];
    const engines = (status && status.engines) || [];
    if (engines.length) {
      menu.push({ heading: "Review with" });
      engines.forEach(function (name) {
        menu.push({ id: "engine", engine: name, label: name + (name === pr.engine ? " (default)" : ""), disabled: !reviewable });
      });
      menu.push({ separator: true });
    }
    menu.push({ id: "nopost", label: "Review without posting", disabled: !reviewable });
    // Review Chat changes nothing in PRX, so it stays on merged PRs too.
    menu.push({ separator: true });
    menu.push({ id: "chat", label: "Open Review Chat\u2026", disabled: false });
    return { buttons: buttons, menu: menu };
  }

  // The review.request params an action sends (the Pull requests page's).
  function reviewRequest(action, pr) {
    const req = { target: pr.key };
    if (action.id === "engine") req.engine = action.engine;
    if (action.id === "nopost") req.post = false;
    return req;
  }

  // Label/value facts under the review (nothing for what is unknown).
  function factsFor(pr, now) {
    const facts = [];
    if (pr.head_branch) facts.push({ label: "Branch", value: pr.head_branch, mono: true });
    const detail = typeof pr.skip_detail === "string" ? pr.skip_detail.trim() : "";
    if (detail) facts.push({ label: pr.state === "skipped" ? "Skipped" : "Review skipped", value: detail });
    if (pr.engine) facts.push({ label: "Review engine", value: pr.engine });
    if (pr.created_at) facts.push({ label: "Opened", value: O.ago(pr.created_at, now) });
    if (pr.human_approved_at) facts.push({ label: "You approved", value: O.ago(pr.human_approved_at, now) });
    return facts;
  }

  // The PR's repo when its polls keep failing (GET /status poll_health):
  // {repo, failures, error, since}, else null.
  function syncProblem(status, key, now) {
    const repo = splitPrKey(key).repo;
    const h = status && status.poll_health && status.poll_health[repo];
    if (!h || (h.consecutive_failures || 0) < O.FAILING_AFTER) return null;
    return { repo: repo, failures: h.consecutive_failures, error: h.last_error || "unknown error",
      since: h.failing_since ? O.ago(h.failing_since, now) : "" };
  }

  const api = { HISTORY_LIMIT: HISTORY_LIMIT, syncProblem: syncProblem, splitPrKey: splitPrKey, formatDuration: formatDuration, detailModel: detailModel,
    latestReview: latestReview, reviewModel: reviewModel, actionsFor: actionsFor, reviewRequest: reviewRequest, factsFor: factsFor };
  if (node) { module.exports = api; return; }

  // ---- window -------------------------------------------------------------

  const UI = root.prxForm;
  const bridge = root.prxBridge;
  const renderers = root.prxPageRenderers = root.prxPageRenderers || {};
  const el = UI.el;

  renderers.pr = function (rootNode, app) {
    const key = app.prKey();
    const view = app.view("pr", function () { return { notes: {}, confirm: {} }; });
    if (!view.confirm) view.confirm = {};
    let status = null, latest = null;
    const back = el("button", { type: "button", class: "link back-link", onclick: function () { app.go("prs"); } },
      [el("span", { "aria-hidden": "true", text: "‹ " }), el("span", { text: "Pull requests" })]);
    const body = el("div", { class: "pr-page", "aria-busy": "true" });
    rootNode.append(back, body);

    if (!key) {
      body.removeAttribute("aria-busy");
      body.replaceChildren(notTracked(null));
      return null;
    }

    const briefs = root.prxBrief.controller(app, bridge, function () { draw(); }, function () { live.refresh(); });

    function notTracked(k) {
      return UI.empty("pr", k ? k + " isn't tracked" : "No pull request selected",
        k ? "PRX no longer tracks this PR: it may have been removed from your watched repos, or its record was cleaned up after it closed."
          : "Pick a PR on the Pull requests page to see its brief and latest review.",
        [UI.button("Show pull requests", function () { app.go("prs"); }, "primary")]);
    }

    function note(text, kind) { view.notes[key] = { text: text, kind: kind || "" }; draw(); }

    function send(req) {
      note("Requesting…");
      bridge.call("review.request", req).then(function (result) {
        const queued = (result && result.enqueued) || [];
        note(result && result.note ? result.note : queued.length ? "Queued " + queued.join(", ") + "." : "Nothing was queued.", "ok");
        live.refresh();
      }, function (error) {
        note(O.isOffline(error) ? "The PRX daemon is not running." : error.message, "error");
      });
    }

    // A review: the pre-flight first; when it warns, the confirm box under
    // the actions row, which sends with `confirm: true` on "Review anyway".
    function request(pr, action) {
      const req = reviewRequest(action, pr);
      delete view.confirm[key];
      note("Requesting…");
      O.reviewPreflight(bridge, req, function (answer) {
        delete view.notes[key];
        view.confirm[key] = { req: req, answer: answer };
        draw();
      }, send, function (error) {
        note(O.isOffline(error) ? "The PRX daemon is not running." : error.message, "error");
      });
    }

    // Review Chat: PRX.app opens a terminal running `prx chat <key>`.
    function openChat(pr) {
      note("Opening Review Chat…");
      bridge.call("app.review_chat.open", { key: pr.key }).then(function () {
        note("Review Chat opened in Terminal.", "ok");
      }, function (error) {
        note(error.message, "error");
      });
    }

    // What each action id does. Add an entry with its actionsFor descriptor.
    const HANDLERS = {
      open: function (pr) { window.open(pr.html_url, "_blank", "noopener"); },
      review: request, engine: request, nopost: request, chat: openChat,
    };
    function runner(pr, action) { return function () { if (HANDLERS[action.id]) HANDLERS[action.id](pr, action); }; }

    function actionsRow(pr) {
      const spec = actionsFor(pr, status);
      const buttons = spec.buttons.map(function (a) {
        const b = el("button", { type: "button", class: "btn" + (a.kind ? " " + a.kind : ""), "data-action": a.id, onclick: runner(pr, a) },
          [a.icon ? UI.icon(a.icon) : null, el("span", { text: a.label })]);
        b.disabled = !!a.disabled;
        return b;
      });
      const items = spec.menu.map(function (a) {
        return a.heading || a.separator ? a : { label: a.label, disabled: a.disabled, onclick: runner(pr, a) };
      });
      const n = view.notes[key];
      const ask = view.confirm[key] && !O.isTerminal(pr) ? view.confirm[key] : null;
      return el("div", { class: "pr-actions" }, [
        el("div", { class: "pr-actions-row", role: "toolbar", "aria-label": "Actions for " + pr.key },
          buttons.concat(items.length ? [UI.menuButton("More actions for " + pr.key, items)] : [])),
        n ? el("p", { class: "inline-note " + n.kind, role: "status" }, [el("span", { text: n.text }),
          UI.iconButton("close", "Dismiss", function () { delete view.notes[key]; draw(); })]) : null,
        ask ? O.confirmBox(UI, ask.answer, function () {
          delete view.confirm[key];
          note("Not reviewed.");
        }, function () {
          delete view.confirm[key];
          send(Object.assign({}, ask.req, { confirm: true }));
        }) : null,
      ]);
    }

    function reviewSection(pr) {
      const r = reviewModel(pr, latest, Date.now());
      const head = el("div", { class: "brief-head" }, [el("h3", { text: "Latest review" })]);
      if (!r.reviewed) {
        return el("section", { class: "review-card", "aria-label": "Latest review of " + pr.key }, [head,
          el("p", { class: "brief-empty", text: O.isTerminal(pr) ? "This PR was never reviewed." : "Not reviewed yet." }),
          r.failedSince ? el("p", { class: "inline-note error", text: "The last review run failed. See History for details." }) : null]);
      }
      const meta = [r.when ? "Reviewed " + r.when : null, r.engine ? "with " + r.engine : null, r.duration ? "in " + r.duration : null]
        .filter(Boolean).join(" ");
      return el("section", { class: "review-card", "aria-label": "Latest review of " + pr.key }, [
        head,
        el("div", { class: "review-verdict" }, [O.pill(UI, r.verdictState, r.verdict), el("span", { class: "review-findings", text: r.findings }),
          r.posted ? el("span", { class: "badge" + (r.posted === "Not posted" ? " warn" : ""), text: r.posted }) : null]),
        meta ? el("p", { class: "brief-dates", text: meta }) : null,
        r.summary ? el("p", { class: "review-summary", text: r.summary }) : null,
        r.blocking.length ? el("div", { class: "review-findings-list" }, [
          el("h4", { text: "Blocking findings" }),
          el("ol", {}, r.blocking.map(function (t) { return el("li", { text: t }); })),
        ]) : null,
        r.unlisted && r.blocking.length ? el("p", { class: "brief-dates", text: O.formatCount(r.unlisted, "more blocking finding") + " not recorded in history." }) : null,
        r.nits ? el("p", { class: "brief-dates", text: O.formatCount(r.nits, "nit") + ": read them in the review on GitHub." }) : null,
        r.failedSince ? el("p", { class: "inline-note error", text: "A later review run failed. See History for details." }) : null,
      ]);
    }

    function factsList(pr) {
      const facts = factsFor(pr, Date.now());
      if (!facts.length) return null;
      return el("dl", { class: "pr-facts-list" }, facts.reduce(function (out, f) {
        return out.concat([el("dt", { text: f.label }), el("dd", { class: f.mono ? "mono" : null, text: f.value })]);
      }, []));
    }

    function draw() {
      if (!status) return;
      body.removeAttribute("aria-busy");
      const pr = (status.prs || []).find(function (p) { return p.key === key; });
      if (!pr) { body.replaceChildren(notTracked(key)); return; }
      const m = detailModel(pr, Date.now());
      const sync = syncProblem(status, key, Date.now());
      // replaceChildren would show a null as the text "null": drop them.
      body.replaceChildren.apply(body, [
        sync ? UI.banner("error", "alert", "PRX can't sync " + sync.repo + " with GitHub.",
          "Its polls have failed " + sync.failures + " times in a row" + (sync.since ? " (since " + sync.since + ")" : "") +
          ", so this page may be out of date: `" + sync.error + "`. To restart the daemon, choose the Sync failing line in the PRX menu.") : null,
        el("header", { class: "pr-head" }, [
          el("p", { class: "pr-head-meta" }, [O.pill(UI, m.stateId, m.state),
            m.skip ? el("span", { class: "pr-skip", text: m.skip }) : null, el("span", { class: "mono", text: m.key }),
            m.author ? el("span", { text: "by " + m.author }) : null, m.opened ? el("span", { text: "opened " + m.opened }) : null,
            m.draft ? el("span", { class: "badge", text: "Draft" }) : null]),
          el("h1", { class: "pr-title", text: m.title }),
        ]),
        actionsRow(pr),
        briefs.card(pr),
        reviewSection(pr),
        factsList(pr),
      ].filter(Boolean));
    }

    const live = O.poller(app, function () {
      const repo = splitPrKey(key).repo;
      // History is best effort: without it the review comes from status.
      return Promise.all([bridge.call("status.get", {}),
        bridge.call("history.list", { repo: repo, limit: HISTORY_LIMIT }).then(null, function () { return null; })]);
    }, function (error, values) {
      if (error) {
        status = null;
        body.removeAttribute("aria-busy");
        body.replaceChildren(O.isOffline(error) ? O.offline(UI, live.refresh) : O.failure(UI, error, live.refresh));
        return;
      }
      status = values[0];
      if (values[1]) latest = latestReview(values[1].entries, key);
      briefs.update(status);
      // Don't rebuild under an open action menu.
      const active = document.activeElement;
      if (active && body.contains(active) && active.closest(".menu-anchor")) return;
      draw();
    });
    live.refresh();
    return live;
  };
})(this);
