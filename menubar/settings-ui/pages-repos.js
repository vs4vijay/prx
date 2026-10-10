// Repos & Orgs page. Left: a picker that adds repos from the GitHub account
// (search by text or regular expression, or type any owner/repo) above the
// watched list, grouped by org. Right: the override editor for the selected
// repo or org — each field shows its effective value and where it comes
// from (this entry / org / default); editing creates an override, Reset
// removes it.
(function () {
  "use strict";
  const F = window.prxFields;
  const S = window.prxSchema;
  const UI = window.prxForm;
  const renderers = window.prxPageRenderers = window.prxPageRenderers || {};
  const MAX_OPTIONS = 60;

  function entryPath(kind, name) { return (kind === "org" ? "orgs" : "repos") + "[" + name + "]"; }

  function overrideCount(entry) {
    return S.OVERRIDE_FIELDS.filter(function (f) { return F.isSet(entry, f.key); }).length;
  }

  function plural(n, word) { return n + " " + word + (n === 1 ? "" : "s"); }

  // ---------------------------------------------------------------- picker

  // The "add repositories" combobox. `onAdd(names)` adds them to the draft.
  function picker(app, onAdd) {
    const draft = app.draft();
    const state = { open: false, regex: false, active: -1, options: [], total: 0 };
    const input = UI.el("input", { type: "text", id: "add-repo", role: "combobox", spellcheck: "false", autocomplete: "off",
      placeholder: "Search, or type owner/name", "aria-label": "Add repositories", "aria-expanded": "false", "aria-controls": "repo-options", "aria-autocomplete": "list" });
    const regexToggle = UI.el("button", { type: "button", class: "regex-toggle", "aria-pressed": "false",
      title: "Match with a regular expression", "aria-label": "Match with a regular expression", text: ".*" });
    const menu = UI.el("div", { class: "picker-menu", id: "repo-options", role: "listbox", "aria-label": "Repositories", hidden: true });
    const error = UI.el("p", { class: "field-error", role: "alert", hidden: true });
    const node = UI.el("div", { class: "picker" }, [
      UI.el("div", { class: "picker-field" }, [UI.icon("search"), input, regexToggle]), menu, error]);
    app.register("repos", { row: node, error: error, focus: input });

    function watched(name) { return !!F.findEntry(draft, "repo", name); }

    function add(names) {
      const fresh = names.filter(function (n) { return !watched(n); });
      if (!fresh.length) return;
      app.localError("repos", null);
      onAdd(fresh);
      draw();
    }

    function note(text, extra) {
      return UI.el("p", { class: "picker-note" }, [document.createTextNode(text)].concat(extra || []));
    }

    function draw() {
      const repos = app.repos();
      const query = input.value.trim();
      const matcher = F.repoMatcher(query, state.regex);
      const children = [];
      state.options = [];
      state.total = 0;
      regexToggle.setAttribute("aria-pressed", String(matcher.regex));
      node.classList.toggle("bad", !matcher.ok);

      if (!matcher.ok) {
        children.push(note("That pattern doesn't compile: " + matcher.error));
      } else {
        const matches = repos.list.filter(function (r) { return !watched(r.name) && matcher.test(r.name); });
        state.total = matches.length;
        // A typed owner/name can be added even when GitHub didn't list it.
        const typed = !matcher.regex && F.validRepoName(query) && !repos.list.some(function (r) { return r.name.toLowerCase() === query.toLowerCase(); });
        if (typed) state.options.push({ name: query, typed: true, disabled: watched(query) });
        matches.slice(0, MAX_OPTIONS).forEach(function (r) { state.options.push({ name: r.name, private: r.private }); });
        if (state.active >= state.options.length) state.active = state.options.length - 1;

        state.options.forEach(function (option, i) {
          const cut = option.name.indexOf("/");
          children.push(UI.el("div", { class: "option", role: "option", id: "repo-option-" + i, "aria-selected": String(i === state.active),
            "aria-disabled": option.disabled ? "true" : null, "data-index": String(i) }, [
            UI.el("span", { class: "option-name" }, [UI.el("span", { class: "option-owner", text: option.name.slice(0, cut + 1) }), document.createTextNode(option.name.slice(cut + 1))]),
            option.private ? UI.icon("lock", "Private") : null,
            UI.el("span", { class: "option-hint", text: option.disabled ? "Already watched" : option.typed ? "Add as typed" : "" }),
          ]));
        });
        if (repos.status === "loading") children.push(note("Loading your repositories…"));
        else if (repos.status === "error") {
          children.push(note("Couldn't list your repositories. " + repos.error + " You can still type an owner/name.", [
            UI.button("Try again", function () { load(true); }, "link")]));
        } else if (repos.status === "ready" && !matches.length && !typed) {
          children.push(note(query ? "No repository matches. Type a full owner/name to add one GitHub didn't list."
            : repos.list.length ? "Every repository GitHub listed is already watched." : "GitHub listed no repositories for this account."));
        }
        if (matches.length > MAX_OPTIONS) children.push(note("Showing " + MAX_OPTIONS + " of " + matches.length + ". Keep typing to narrow it down."));
        if (query && matches.length > 1) {
          children.push(UI.el("div", { class: "picker-foot" }, [
            UI.el("button", { type: "button", class: "btn", "data-all": "1", text: "Add all " + matches.length + " matches" }),
            matcher.regex ? UI.el("span", { class: "picker-hint", text: "Adds what matches today, not future repos." }) : null,
          ]));
          state.matches = matches.map(function (r) { return r.name; });
        }
      }
      menu.replaceChildren.apply(menu, children);
      menu.hidden = !state.open;
      input.setAttribute("aria-expanded", String(state.open));
      if (state.active >= 0 && state.open) input.setAttribute("aria-activedescendant", "repo-option-" + state.active);
      else input.removeAttribute("aria-activedescendant");
      const active = menu.querySelector('[aria-selected="true"]');
      if (active) active.scrollIntoView({ block: "nearest" });
    }

    function load(force) {
      const pending = app.loadRepos(force);
      draw();
      pending.then(draw);
    }

    function open() {
      if (state.open) return;
      state.open = true;
      load(false);
    }

    function close() {
      state.open = false;
      state.active = -1;
      draw();
    }

    function choose(index) {
      const option = state.options[index];
      if (!option || option.disabled) return;
      add([option.name]);
      if (option.typed) input.value = "";
      draw();
    }

    input.addEventListener("focus", open);
    input.addEventListener("click", open);
    input.addEventListener("input", function () { state.active = -1; state.open = true; draw(); });
    input.addEventListener("blur", function () { setTimeout(function () { if (document.activeElement !== input && !node.contains(document.activeElement)) close(); }, 0); });
    input.addEventListener("keydown", function (e) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        if (!state.open) { open(); return; }
        const count = state.options.length;
        if (!count) return;
        state.active = e.key === "ArrowDown" ? (state.active + 1) % count : (state.active <= 0 ? count - 1 : state.active - 1);
        draw();
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (state.active >= 0) choose(state.active);
        else if (state.options.length && state.options[0].typed) choose(0);
        else if (state.options.length === 1) choose(0);
      } else if (e.key === "Escape" && state.open) {
        e.preventDefault();
        close();
      }
    });
    // Keep focus in the input while the list is clicked.
    menu.addEventListener("mousedown", function (e) { e.preventDefault(); });
    menu.addEventListener("click", function (e) {
      const target = e.target instanceof Element ? e.target : null;
      if (!target) return;
      if (target.closest("[data-all]")) { add(state.matches || []); input.value = ""; input.focus(); draw(); return; }
      const option = target.closest(".option");
      if (option) { choose(Number(option.dataset.index)); input.focus(); }
    });
    regexToggle.addEventListener("click", function () {
      state.regex = !state.regex;
      state.open = true;
      input.focus();
      draw();
    });
    return node;
  }

  // ------------------------------------------------------------ watched list

  // Whether config.yml's current error names this entry (audit P1: flag
  // the broken repo or org in the tree), e.g. "repos[acme/api].schedule: …".
  function brokenIn(app, kind, name) {
    const loaded = app.loaded();
    const message = loaded && loaded.config_error && loaded.config_error.message;
    return !!message && message.indexOf((kind === "org" ? "orgs[" : "repos[") + name + "]") !== -1;
  }

  function entityButton(app, kind, name, label, entry, selection) {
    const count = overrideCount(entry);
    const selected = !!selection && selection.kind === kind && selection.name === name;
    const broken = brokenIn(app, kind, name);
    return UI.el("button", { type: "button", class: "entity", "aria-current": selected ? "true" : "false",
      title: broken ? name + ": config.yml has an error in this entry" : name,
      onclick: function () { app.ui.editorView = null; app.select({ kind: kind, name: name }); } }, [
      UI.el("span", { class: "entity-name", text: label }),
      broken ? UI.el("span", { class: "entity-flag" }, [UI.icon("alert", "Has a config error")]) : null,
      count ? UI.el("span", { class: "count", title: plural(count, "override") + " of the defaults", text: plural(count, "override") }) : null,
    ]);
  }

  function tree(app, selection, filter, fresh, redraw) {
    const draft = app.draft();
    const groups = F.groupRepos(draft);
    const term = filter.trim().toLowerCase();
    const items = [];
    groups.forEach(function (group) {
      const repos = group.repos.filter(function (r) { return !term || r.name.toLowerCase().indexOf(term) !== -1; });
      if (term && !repos.length && group.org.toLowerCase().indexOf(term) === -1) return;
      const orgError = UI.el("p", { class: "field-error", role: "alert", hidden: true });
      const head = UI.el("div", { class: "group-head" });
      if (group.entry) {
        head.append(entityButton(app, "org", group.org, group.org, group.entry, selection),
          UI.iconButton("close", "Remove the overrides for " + group.org, function () {
            draft.orgs.splice(draft.orgs.indexOf(group.entry), 1);
            if (selection && selection.kind === "org" && selection.name === group.org) app.ui.selection = null;
            app.changed("orgs");
            app.rerender();
          }, "remove"));
        app.register(entryPath("org", group.org), { row: head, error: orgError, focus: head.firstChild });
      } else {
        head.append(UI.el("span", { class: "group-name", text: group.org }),
          UI.button("Org overrides", function () {
            draft.orgs = draft.orgs || [];
            draft.orgs.push({ org: group.org });
            app.changed("orgs");
            app.ui.editorView = "all";
            app.select({ kind: "org", name: group.org });
          }, "link small"));
      }
      const children = repos.map(function (repo) {
        const error = UI.el("p", { class: "field-error", role: "alert", hidden: true });
        const cut = repo.name.indexOf("/");
        const pick = entityButton(app, "repo", repo.name, cut > 0 ? repo.name.slice(cut + 1) : repo.name, repo, selection);
        const line = UI.el("div", { class: "entity-line" + (fresh.indexOf(repo.name) !== -1 ? " fresh" : "") }, [pick,
          UI.iconButton("close", "Stop watching " + repo.name, function () {
            draft.repos.splice(draft.repos.indexOf(repo), 1);
            if (selection && selection.kind === "repo" && selection.name === repo.name) { app.ui.selection = null; app.changed("repos"); app.rerender(); return; }
            app.changed("repos");
            redraw();
          }, "remove")]);
        const node = UI.el("li", { "data-field": entryPath("repo", repo.name) }, [line, error]);
        app.register(entryPath("repo", repo.name), { row: node, error: error, focus: pick });
        return node;
      });
      items.push(UI.el("li", { class: "group" }, [head, orgError, children.length ? UI.el("ul", {}, children) : null]));
    });
    if (!groups.length) {
      return UI.el("p", { class: "list-empty", text: "No repos yet. Add the first one above and PRX starts watching its pull requests." });
    }
    if (!items.length) return UI.el("p", { class: "list-empty", text: "Nothing watched matches “" + filter.trim() + "”." });
    return UI.el("ul", { class: "tree" }, items);
  }

  // ----------------------------------------------------------------- editor

  function overrideRow(app, kind, name, field, refresh) {
    const draft = app.draft();
    const entry = F.findEntry(draft, kind, name);
    const path = entryPath(kind, name) + "." + field.key;
    const eff = F.resolveEffective(draft, kind, name, field.key);
    const own = eff.source === kind;
    function applied() {
      app.changed(path);
      // The model row describes the engine, and the "overridden" view lists
      // rows by state: both need the whole editor again.
      refresh(field.type === "engine" || app.ui.editorView === "own" ? path : null, function () {
        const old = node;
        node = overrideRow(app, kind, name, field, refresh);
        old.replaceWith(node);
      });
    }
    const parts = UI.control(app, path, field, eff.value, function (next) {
      if (next === null) F.unsetAt(entry, field.key);
      else F.setAt(entry, field.key, next);
      applied();
    }, { compact: true, context: { engine: F.resolveEffective(draft, kind, name, "engine").value },
      // Dropdowns (and the browser picker, spec browser-picker) offer
      // Inherit here, showing what it stands for.
      inherit: true, inherited: !own, parent: F.resolveParent(draft, kind, name, field.key) });
    const aside = [UI.el("span", { class: "badge " + eff.source, text: F.sourceLabel(eff, kind) })];
    if (own) {
      aside.push(UI.button("Reset", function () {
        F.unsetAt(entry, field.key);
        applied();
      }, "link"));
    }
    let node = UI.row(app, path, field.label, parts, { inherited: !own, aside: aside, stack: field.stack && !field.bare });
    return node;
  }

  function editor(app, selection, onChange) {
    const draft = app.draft();
    const host = UI.el("div", { class: "entity-editor" });
    function draw(focusPath) {
      const entry = selection && F.findEntry(draft, selection.kind, selection.name);
      if (!entry) {
        host.replaceChildren(UI.empty("repo", draft.repos && draft.repos.length ? "Pick a repo or org" : "Nothing watched yet",
          draft.repos && draft.repos.length
            ? "Select one on the left to give it its own settings. Everything it doesn't override follows its org, then the defaults."
            : "Add a repository on the left. It starts with the defaults, and you can override anything per repo or per org."));
        return;
      }
      const kind = selection.kind;
      const name = selection.name;
      const count = overrideCount(entry);
      if (!app.ui.editorView) app.ui.editorView = count ? "own" : "all";
      const view = app.ui.editorView;
      let context;
      if (kind === "repo") {
        const org = F.orgOf(name);
        context = org && F.findEntry(draft, "org", org) ? "Follows the " + org + " org overrides, then the defaults." : "Follows the defaults.";
      } else {
        const members = (draft.repos || []).filter(function (r) { return F.orgOf(r.name) === name; }).length;
        context = "Applies to " + (members ? plural(members, "watched repo") : "every repo you watch") + " under " + name + "/, unless a repo overrides it.";
      }
      function refresh(fullFocusPath, rebuildRow) {
        onChange();
        if (fullFocusPath) draw(fullFocusPath);
        else { rebuildRow(); toggle.count(overrideCount(entry)); }
      }
      const groups = [];
      S.OVERRIDE_FIELDS.forEach(function (field) {
        if (view === "own" && !F.isSet(entry, field.key)) return;
        let current = groups[groups.length - 1];
        if (!current || current.title !== field.section) {
          current = { title: field.section, rows: [] };
          groups.push(current);
        }
        current.rows.push(overrideRow(app, kind, name, field, refresh));
      });
      const options = [{ value: "own", label: "Overridden" }, { value: "all", label: "All settings" }];
      const seg = UI.segmented("editor-view", options, view, function (next) { app.ui.editorView = next; draw(); }, "Settings shown");
      const toggle = { count: function (n) { seg.node.querySelector("label").textContent = "Overridden" + (n ? " (" + n + ")" : ""); } };
      toggle.count(count);
      const body = groups.length ? groups.map(function (g) { return UI.section(g.title, g.rows); })
        : [UI.empty("sliders", "No overrides", context + " Switch to All settings to change one.",
          [UI.button("Show all settings", function () { app.ui.editorView = "all"; draw(); })])];
      host.replaceChildren.apply(host, [
        UI.el("div", { class: "editor-head" }, [
          UI.el("div", {}, [
            UI.el("h2", { class: "entity-title" }, [document.createTextNode(name), UI.el("span", { class: "kind", text: kind === "org" ? "Org" : "Repo" })]),
            UI.el("p", { class: "lede", text: context }),
          ]),
          seg.node,
        ]),
      ].concat(body));
      if (focusPath) {
        const row = host.querySelector('[data-field="' + CSS.escape(focusPath) + '"]');
        const target = row && row.querySelector("input, [role=combobox], textarea, button");
        if (target) target.focus({ preventScroll: true });
      }
    }
    draw();
    return host;
  }

  renderers.repos = function (root, app) {
    const draft = app.draft();
    draft.repos = draft.repos || [];
    draft.orgs = draft.orgs || [];
    const selection = app.ui.selection || null;
    let filter = "";
    let fresh = [];
    const listHost = UI.el("div", { class: "watched" });
    const filterInput = UI.el("input", { type: "text", class: "list-filter", placeholder: "Filter watched repos", spellcheck: "false", autocomplete: "off", "aria-label": "Filter watched repos" });
    filterInput.addEventListener("input", function () { filter = filterInput.value; drawList(); });
    const count = UI.el("p", { class: "list-count" });
    function drawList() {
      filterInput.hidden = draft.repos.length < 9 && !filter;
      count.textContent = draft.repos.length ? "Watching " + plural(draft.repos.length, "repo") : "";
      listHost.replaceChildren(tree(app, selection, filter, fresh, drawList));
    }
    const add = picker(app, function (names) {
      names.forEach(function (name) { draft.repos.push({ name: name }); });
      fresh = fresh.concat(names);
      app.changed("repos");
      drawList();
    });
    drawList();
    root.append(
      UI.header("Repos & Orgs", "What PRX watches. A repo's own settings win over its org's, which win over the defaults."),
      UI.el("div", { class: "split" }, [
        UI.el("div", { class: "entity-list" }, [add, UI.el("div", { class: "list-head" }, [count, filterInput]), listHost]),
        editor(app, selection, drawList),
      ])
    );
  };
})();
