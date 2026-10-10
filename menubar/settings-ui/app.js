// Settings app shell: one shared draft of the config (GET /config `raw`)
// edited by every page, a save bar submitting it through PUT /config, and
// error routing — a rejected save names a field path, which is mapped to the
// page + input that edits it so the message renders next to that input.
// Pages register in window.prxPageRenderers[id] = render(root, app).
// Overview pages (design D16) read the daemon, not the draft: their render
// may return {refresh}, which runs about every 10 s while the window is
// visible and the page is current. The draft lives here, not in a page, so
// unsaved edits survive switching to an Overview page and back. The window
// opens on `#page=<id>`; the host switches pages with prxApp.go(id).
(function () {
  "use strict";
  const bridge = window.prxBridge;
  const F = window.prxFields;
  const S = window.prxSchema;
  const UI = window.prxForm;
  const renderers = window.prxPageRenderers || {};

  const POLL_MS = 10000;
  const state = {
    loaded: null, draft: null, dirty: false, saving: false, offline: false,
    page: S.pageFromHash(window.location && window.location.hash) || "general",
    // The mounted Overview page's {refresh}, the mount counter (stale async
    // answers check it), and each Overview page's view state (filters, sort).
    live: null, mount: 0, overview: {},
    ui: { selection: null }, registry: {}, errors: {}, localErrors: {},
    // What the daemon found on this machine (discover.agents), and the
    // repositories the gh account can see (discover.repos, fetched on demand).
    found: null, repos: { status: "idle", list: [], error: null, pending: null },
    // Installed browsers for the "Open PRs with" picker (browsers.list,
    // app-local, design W1); null until it answers.
    browsers: null, browsersAsked: false,
  };
  const pageNode = document.getElementById("page");
  const statusNode = document.getElementById("save-status");
  const noteNode = document.getElementById("save-note");
  const saveButton = document.getElementById("save");
  const revertButton = document.getElementById("revert");
  const daemonNode = document.getElementById("daemon");

  function setStatus(kind, text) {
    statusNode.className = "status " + (kind || "");
    statusNode.textContent = text || "";
  }

  function describe(error) {
    return error.kind === "offline" ? "The PRX daemon is not running." : error.message;
  }

  function updateSavebar() {
    saveButton.disabled = !state.dirty || state.saving;
    revertButton.disabled = !state.dirty || state.saving;
    document.body.classList.toggle("dirty", state.dirty);
    if (state.dirty && !state.saving && !statusNode.classList.contains("error")) setStatus("pending", "Unsaved changes");
    const notes = [];
    if (state.loaded && state.loaded.config_error) {
      notes.push("The config file has errors: saving the form replaces it with the running config plus your edits.");
    }
    if (state.dirty && state.loaded && F.hasYamlComments(state.loaded.yaml)) {
      notes.push("Saving the form rewrites config.yml without its comments. Edit config.yml directly to keep them.");
    }
    noteNode.textContent = notes.join(" ");
    noteNode.hidden = notes.length === 0;
  }

  function updateDaemon() {
    daemonNode.className = "daemon " + (state.offline ? "off" : state.loaded ? "on" : "");
    daemonNode.textContent = state.offline ? "Daemon not running" : state.loaded ? "Daemon running" : "Connecting…";
  }

  function showErrors() {
    const available = Object.keys(state.registry);
    Object.values(state.registry).forEach(function (entry) {
      entry.error.hidden = true;
      entry.error.textContent = "";
      entry.row.classList.remove("invalid");
    });
    const all = Object.assign({}, state.errors, state.localErrors);
    Object.keys(all).forEach(function (path) {
      const target = F.matchInput(path, available);
      if (!target) return;
      const entry = state.registry[target];
      entry.error.hidden = false;
      entry.error.textContent = entry.error.textContent ? entry.error.textContent + " " + all[path] : all[path];
      entry.row.classList.add("invalid");
    });
  }

  function focusField(path, quiet) {
    const target = F.matchInput(path, Object.keys(state.registry));
    const entry = target && state.registry[target];
    if (!entry) return;
    if (!quiet) entry.row.scrollIntoView({ block: "center" });
    if (entry.focus) entry.focus.focus({ preventScroll: !!quiet });
  }

  function offlinePage() {
    return UI.empty("offline", "The PRX daemon isn't running",
      "Settings are read from and saved through the daemon. Start it from the PRX menu bar (Start daemon) or with `prx daemon start`, then try again.",
      [UI.button("Try again", function () { load(false); }, "primary")]);
  }

  // keepScroll: re-render in place (same page) without jumping to the top.
  function isOverview(id) { return S.pageGroup(id) === "overview"; }

  function show(id, keepScroll) {
    const scroll = keepScroll ? pageNode.scrollTop : 0;
    state.page = renderers[id] ? id : "general";
    state.registry = {};
    state.live = null;
    state.mount += 1;
    pageNode.replaceChildren();
    document.querySelectorAll("#nav button").forEach(function (b) {
      b.setAttribute("aria-current", b.dataset.page === state.page ? "page" : "false");
    });
    pageNode.dataset.page = state.page;
    document.body.dataset.page = state.page;
    document.body.classList.toggle("overview", isOverview(state.page));
    if (isOverview(state.page)) {
      state.live = renderers[state.page](pageNode, app) || null;
      pageNode.scrollTop = scroll;
      return;
    }
    if (!state.draft) {
      if (state.offline) pageNode.append(offlinePage());
      else pageNode.append(UI.el("div", { class: "skeleton", "aria-busy": "true", "aria-label": "Loading settings" },
        [0, 1, 2, 3, 4].map(function () { return UI.el("div", { class: "skeleton-row" }); })));
      return;
    }
    renderers[state.page](pageNode, app);
    showErrors();
    pageNode.scrollTop = scroll;
  }

  // Route an error path to its page (selecting the org/repo it names).
  function reveal(path) {
    const where = S.pageForField(path, F.parsePath);
    if (where.entity) { state.ui.selection = where.entity; state.ui.editorView = "all"; }
    show(where.page, where.page === state.page);
    focusField(path);
  }

  function discover() {
    bridge.call("discover.agents").then(function (found) {
      state.found = found;
      // Refresh the pages that show engines, unless someone is mid-edit.
      const busy = document.activeElement && /^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement.tagName);
      if (state.draft && !busy && (state.page === "review" || state.page === "engines")) show(state.page, true);
    }, function () { /* the form works without discovery */ });
  }

  // Browser discovery runs once per window (installed apps rarely change);
  // the config's app paths are resolved too, so a browser picked with
  // "Other app…" shows its name and icon.
  function discoverBrowsers() {
    if (state.browsersAsked) return;
    state.browsersAsked = true;
    bridge.call("browsers.list", { paths: F.browserPaths(state.draft) }).then(function (catalog) {
      state.browsers = { browsers: catalog.browsers || [], extra: catalog.extra || [] };
      const focused = document.activeElement;
      const busy = focused && (/^(INPUT|TEXTAREA|SELECT)$/.test(focused.tagName) || (focused.closest && focused.closest(".browser-picker")));
      if (state.draft && !busy && (state.page === "identity" || state.page === "repos")) show(state.page, true);
    }, function () { /* the picker works without discovery */ });
  }

  // keepStatus: leave the status line alone; quiet: also skip re-rendering
  // when nothing changed (focus refresh, so a click is never swallowed).
  function load(keepStatus, quiet) {
    if (!keepStatus) setStatus("", "");
    state.loading = true;
    return bridge.getConfig().then(function (config) {
      state.loading = false;
      state.offline = false;
      updateDaemon();
      if (quiet && state.loaded && JSON.stringify(config) === JSON.stringify(state.loaded)) return;
      const first = !state.loaded;
      state.loaded = config;
      state.draft = F.clone(config.raw);
      state.dirty = false;
      state.errors = {};
      state.localErrors = {};
      updateSavebar();
      updateDaemon();
      // Overview pages poll on their own; re-mounting would reset them.
      if (!isOverview(state.page)) show(state.page, !first);
      if (first || !state.found) discover();
      discoverBrowsers();
    }, function (error) {
      state.loading = false;
      state.offline = error.kind === "offline";
      updateDaemon();
      if (quiet) return;
      if (state.draft) setStatus("error", describe(error));
      else if (!state.offline) setStatus("error", "Could not load settings: " + describe(error));
      updateSavebar();
      if (!isOverview(state.page)) show(state.page);
    });
  }

  function savedMessage(result) {
    const changes = [];
    if (result.added && result.added.length) changes.push("now watching " + result.added.join(", "));
    if (result.removed && result.removed.length) changes.push("stopped watching " + result.removed.join(", "));
    return "Saved" + (changes.length ? ": " + changes.join("; ") : " and applied");
  }

  // A rejected save: the raw view keeps its text and shows the error under
  // it; a form save maps the field to its input (on whichever page).
  function rejected(error, raw) {
    state.errors = {};
    setStatus("error", "Not saved: " + describe(error));
    if (raw) {
      state.errors.raw = error.message;
      showErrors();
      return;
    }
    const path = F.normalizeErrorField(error.field, state.draft, error.message);
    if (path) {
      state.errors[path] = error.message;
      reveal(path);
    } else {
      showErrors();
    }
  }

  function finish(promise, raw) {
    state.saving = true;
    updateSavebar();
    setStatus("", "Saving…");
    return promise.then(function (result) {
      state.saving = false;
      return load(true).then(function () {
        setStatus("ok", savedMessage(result));
        discover();
      });
    }, function (error) {
      state.saving = false;
      updateSavebar();
      if (error.kind === "rejected") rejected(error, raw);
      else setStatus("error", "Not saved. " + describe(error));
    });
  }

  function save() {
    if (!state.dirty || state.saving) return Promise.resolve();
    // Commit a field that is still focused (its change event fires on blur).
    if (document.activeElement && document.activeElement !== document.body) document.activeElement.blur();
    const local = Object.keys(state.localErrors);
    if (local.length) {
      setStatus("error", "Fix the highlighted field before saving.");
      reveal(local[0]);
      return Promise.resolve();
    }
    return finish(bridge.putConfig(state.draft), false);
  }

  function loadRepos(force) {
    const repos = state.repos;
    if (repos.pending) return repos.pending;
    if (repos.status === "ready" && !force) return Promise.resolve(repos);
    repos.status = "loading";
    repos.pending = bridge.call("discover.repos").then(function (result) {
      repos.status = "ready";
      repos.list = result.repos || [];
      repos.error = null;
    }, function (error) {
      repos.status = "error";
      repos.error = describe(error);
    }).then(function () { repos.pending = null; return repos; });
    return repos.pending;
  }

  const app = {
    draft: function () { return state.draft; },
    loaded: function () { return state.loaded; },
    // The built-in text of a template field ("" when it has none).
    shipped: function (key) {
      if (!state.loaded) return "";
      if (key === "review_prompt") return state.loaded.shipped_review_prompt || "";
      return (state.loaded.shipped_templates || {})[key] || "";
    },
    // The output contract prx appends to every review prompt (design C2),
    // previewed read-only under the prompt editor.
    contract: function () { return (state.loaded && state.loaded.review_contract) || ""; },
    ui: state.ui,
    isDirty: function () { return state.dirty; },
    register: function (path, entry) { state.registry[path] = entry; },
    changed: function (path) {
      state.dirty = true;
      Object.keys(state.errors).forEach(function (p) {
        if (p === path || p.startsWith(path + ".") || path.startsWith(p + ".")) delete state.errors[p];
      });
      if (statusNode.classList.contains("ok") || statusNode.classList.contains("error")) setStatus("", "");
      updateSavebar();
      showErrors();
    },
    localError: function (path, message) {
      if (message) state.localErrors[path] = message; else delete state.localErrors[path];
      showErrors();
    },
    // Every agent the form can offer (see fields.js agentCatalog), and every
    // engine: those agents plus the configured LLM API engines.
    agents: function () { return F.agentCatalog(state.found, F.agentEntries(state.draft.engines), S.BUILTIN_ENGINES); },
    engines: function () { return F.engineCatalog(state.found, state.draft.engines, S.BUILTIN_ENGINES); },
    // The browser picker (form.js browserControl): the installed-browser
    // catalog (null until browsers.list answers), the native "Other app…"
    // chooser (resolves an entry, or null when cancelled), an app picked
    // there, and a test open (saves nothing).
    browsers: function () { return state.browsers; },
    chooseBrowserApp: function () { return bridge.call("browsers.choose"); },
    addBrowser: function (entry) { state.browsers = F.withExtraBrowser(state.browsers, entry); },
    testBrowser: function (browser) { return bridge.call("browsers.test", { browser: browser }); },
    // What discover.agents reported (null until it answers).
    found: function () { return state.found; },
    repos: function () { return state.repos; },
    loadRepos: loadRepos,
    go: function (page) { show(page); },
    // Overview pages: the mount a render belongs to (async answers for an
    // old mount are dropped), its persistent view state, and daemon health.
    mount: function () { return state.mount; },
    isMounted: function (mount) { return mount === state.mount; },
    view: function (page, init) {
      if (!state.overview[page]) state.overview[page] = init ? init() : {};
      return state.overview[page];
    },
    online: function (up) {
      if (state.offline === up) { state.offline = !up; updateDaemon(); }
      // The daemon came back: settings pages need the config too.
      if (up && !state.loaded && !state.loading) load(true);
    },
    select: function (entity) { state.ui.selection = entity; show(state.page, true); },
    // Re-render the current page in place; `focusPath` puts focus back.
    rerender: function (focusPath) {
      show(state.page, true);
      if (focusPath) focusField(focusPath, true);
    },
    reload: function () { return load(false); },
    saveYaml: function (yaml) { return finish(bridge.putConfigYaml(yaml), true); },
  };

  // The sidebar's brand mark, and the skip link (14 sidebar stops sit
  // before the page otherwise).
  const mark = document.getElementById("brand-mark");
  if (mark) mark.append(UI.icon("mark"));
  const skip = document.getElementById("skip");
  if (skip) skip.addEventListener("click", function () { pageNode.focus(); });

  const nav = document.getElementById("nav");
  S.navGroups().forEach(function (group) {
    nav.append(UI.el("p", { class: "nav-group", id: "nav-" + group.id, text: group.label }));
    group.pages.forEach(function (p) {
      nav.append(UI.el("button", { type: "button", "data-page": p.id, onclick: function () { show(p.id); } },
        [UI.icon(p.icon), UI.el("span", { text: p.label })]));
    });
  });
  saveButton.addEventListener("click", save);
  revertButton.addEventListener("click", function () { load(false); });
  document.addEventListener("keydown", function (e) {
    if ((e.metaKey || e.ctrlKey) && !e.shiftKey && !e.altKey && e.key === "s") {
      e.preventDefault();
      // The raw view saves its own text, not the form draft.
      const rawSave = state.page === "raw" && document.getElementById("raw-save");
      if (rawSave) rawSave.click(); else save();
    }
  });
  // The file is the source of truth: pick up external edits on focus
  // unless the user has unsaved changes here (the raw page reloads only on
  // its Reload button, so in-progress text is never replaced).
  window.addEventListener("focus", function () {
    if (!state.dirty && !state.saving && state.page !== "raw") load(true, true);
  });
  window.addEventListener("hashchange", function () {
    const page = S.pageFromHash(window.location.hash);
    if (page && page !== state.page) show(page);
  });
  function poll() {
    if (state.live && state.live.refresh && document.visibilityState === "visible") state.live.refresh();
  }
  setInterval(poll, POLL_MS);
  document.addEventListener("visibilitychange", poll);
  window.prxApp = Object.freeze({ show: show, go: show, app: app, state: state });
  updateDaemon();
  show(state.page);
  load(false);
})();
