// Defaults-level pages (General, Review, Schedule, Notifications,
// Account & Browser): every schema field of the page, grouped by section,
// editing `draft.defaults`. Org/repo overrides live on the Repos & Orgs page.
(function () {
  "use strict";
  const F = window.prxFields;
  const S = window.prxSchema;
  const UI = window.prxForm;
  const renderers = window.prxPageRenderers = window.prxPageRenderers || {};

  // What an unset, non-optional field effectively is: the daemon's merged
  // defaults (GET /config `effective`), else the schema's built-in value.
  function effectiveDefault(app, field) {
    if (field.optional) return undefined;
    const loaded = app.loaded();
    const merged = loaded && loaded.effective && loaded.effective.defaults;
    const value = merged ? F.getAt(merged, field.key) : undefined;
    return value === undefined || value === null ? field.fallback : value;
  }

  function defaultsRow(app, field) {
    const path = "defaults." + field.key;
    const defaults = app.draft().defaults;
    const own = F.getAt(defaults, field.key);
    // Audit P1: an unset field shows its effective value selected, marked
    // as the default, instead of nothing.
    const fallback = own === undefined ? effectiveDefault(app, field) : undefined;
    const value = own === undefined ? fallback : own;
    function set(next) {
      if (next === null) F.unsetAt(defaults, field.key);
      else F.setAt(defaults, field.key, next);
      app.changed(path);
      // The model row describes the chosen engine.
      if (field.type === "engine") app.rerender(path);
    }
    const parts = UI.control(app, path, field, value === undefined ? null : value, set,
      { context: { engine: defaults.engine }, unset: own === undefined });
    const help = fallback !== undefined ? [field.help, "Not set, so PRX uses the default" + optionLabel(field, fallback) + "."].filter(Boolean).join(" ")
      : field.help;
    return UI.row(app, path, field.label, parts, { help: help, stack: field.stack, bare: field.bare });
  }

  function optionLabel(field, value) {
    const option = (field.options || []).find(function (o) { return o.value === value; });
    return option ? " (" + option.label + ")" : "";
  }

  // The page's schema fields as sections, in schema order.
  function fieldSections(app, page) {
    const sections = [];
    S.FIELDS.filter(function (f) { return f.page === page; }).forEach(function (field) {
      let current = sections[sections.length - 1];
      if (!current || current.title !== field.section) {
        current = { title: field.section, rows: [] };
        sections.push(current);
      }
      current.rows.push(defaultsRow(app, field));
    });
    return sections;
  }

  function build(sections) {
    return sections.map(function (s) { return UI.section(s.title, s.rows, s.note); });
  }

  const OVERRIDE_NOTE = "These apply to every repo. An org or a single repo can override any of them under Repos & Orgs.";

  function modeLabel(value) {
    const option = S.MODE_OPTIONS.find(function (o) { return o.value === value; });
    return option ? option.label : String(value);
  }

  // Under the mode cards: which orgs and repos pick their own mode (each a
  // link to its override editor), so "this applies to every repo" is never
  // a surprise.
  function modeScope(app) {
    const own = F.ownSettings(app.draft(), "review_mode");
    function open(entry) {
      return function () {
        app.ui.selection = { kind: entry.kind, name: entry.name };
        app.ui.editorView = "own";
        app.go("repos");
      };
    }
    const text = own.length
      ? [UI.el("p", { class: "scope-title", text: own.length === 1 ? "One exception chooses its own mode" : own.length + " exceptions choose their own mode" }),
        UI.el("ul", { class: "scope-list" }, own.map(function (e) {
          return UI.el("li", {}, [UI.el("button", { type: "button", class: "scope-item", onclick: open(e),
            "aria-label": e.name + (e.kind === "org" ? " org" : "") + ": " + modeLabel(e.value) }, [
            UI.icon(e.kind === "org" ? "user" : "repo"),
            UI.el("span", { class: "scope-name", text: e.name }),
            UI.el("span", { class: "scope-mode", text: modeLabel(e.value) }),
          ])]);
        }))]
      : [UI.el("p", { class: "scope-title", text: "Every org and repo uses this mode" })];
    text.push(UI.el("p", { class: "help", text: "An org or a single repo can choose its own mode, or inherit this one, under Repos & Orgs." }));
    return UI.el("div", { class: "mode-scope" }, [
      UI.el("div", { class: "mode-scope-text" }, text),
      UI.button("Repos & Orgs", function () { app.go("repos"); }, "small"),
    ]);
  }

  // The pause next to the mode: read from the daemon now and every 30 s
  // while the page is shown (General is not an Overview page, so the app
  // does not poll it).
  function pauseSlot(app) {
    const O = window.prxOverview;
    const bridge = window.prxBridge;
    const slot = UI.el("div", { class: "pause-slot" });
    let status;
    // Start (the menu bar's durations), restart, or lift the pause; a
    // failure (daemon offline, a refusal) shows inline under the text.
    const controls = O && bridge ? O.pauseControls(UI, bridge, refresh) : null;
    function draw() {
      slot.replaceChildren(O.pausePanel(UI, status === null ? null : O.pauseState(status, Date.now()), controls));
    }
    function refresh() {
      if (!O || !bridge) return Promise.resolve();
      return bridge.call("status.get", {}).then(function (s) { status = s; draw(); }, function () { status = null; draw(); });
    }
    refresh();
    const timer = setInterval(function () {
      if (!slot.isConnected) { clearInterval(timer); return; }
      refresh();
    }, 30000);
    return slot;
  }

  renderers.general = function (root, app) {
    const loaded = app.loaded();
    const nodes = [UI.header("General", OVERRIDE_NOTE)];
    if (loaded.config_error) {
      const ce = loaded.config_error;
      nodes.push(UI.el("p", { class: "banner error", role: "alert" }, [UI.icon("alert"),
        UI.el("span", { text: "config.yml has an error, so the daemon is still running the last good version. " + (ce.message || JSON.stringify(ce)) })]));
    }
    const sections = fieldSections(app, "general");
    const mode = sections.find(function (s) { return s.title === "Review mode"; });
    if (mode) {
      mode.note = "What PRX does with the PRs it watches. PRX never approves a PR: Approve and Block in the menu are yours in every mode.";
      mode.rows.push(modeScope(app));
    }
    nodes.push.apply(nodes, build(sections));
    // Pause sits with the mode but is not a setting (nothing to save), so
    // it is its own section.
    if (mode) {
      nodes.push(UI.el("section", { class: "pause-section" }, [
        UI.el("h2", { text: "Pause" }),
        UI.el("p", { class: "section-note", text: "A temporary hold on automatic reviews, separate from the mode. Monitor lasts until you change it; a pause ends on its own." }),
        UI.el("div", { class: "panel" }, [pauseSlot(app)]),
      ]));
      const at = nodes.indexOf(nodes.find(function (n) { return n.querySelector && n.querySelector(".mode-cards"); }));
      if (at !== -1) nodes.splice(at + 1, 0, nodes.pop());
    }
    // App-local controls (login item, CLI link): app-section.js.
    if (window.prxAppSection) nodes.push(window.prxAppSection(app));
    nodes.push(UI.el("p", { class: "footnote" }, [document.createTextNode("Stored in "), UI.el("code", { text: loaded.path || "config.yml" })]));
    root.append.apply(root, nodes);
  };

  renderers.review = function (root, app) {
    const sections = fieldSections(app, "review");
    const skip = sections.find(function (s) { return s.title === "Skip"; });
    if (skip) skip.note = "PRs matching any of these are never reviewed.";
    root.append.apply(root, [UI.header("Review", OVERRIDE_NOTE)].concat(build(sections)));
  };

  renderers.schedule = function (root, app) {
    root.append.apply(root, [UI.header("Schedule",
      "When reviews are allowed to start. GitHub is still checked at the poll interval, so the menu stays current between runs.")]
      .concat(build(fieldSections(app, "schedule"))));
  };

  // macOS alert sounds (/System/Library/Sounds), offered in each tier's sound menu.
  const SOUNDS = ["Basso", "Blow", "Bottle", "Frog", "Funk", "Glass", "Hero", "Morse", "Ping", "Pop", "Purr", "Sosumi", "Submarine", "Tink"];
  const TIER_NOTES = {
    urgent: "Needs you now.",
    informational: "Good to know.",
    silent: "Recorded in history only, unless you turn the banner on.",
  };

  function tierRows(app) {
    const tiers = app.draft().notification_tiers;
    return S.TIERS.map(function (tier) {
      const banner = UI.el("input", { type: "checkbox", role: "switch", class: "switch", id: "tier-" + tier + "-banner" });
      banner.checked = !!tiers[tier].banner;
      banner.addEventListener("change", function () {
        tiers[tier].banner = banner.checked;
        app.changed("notification_tiers." + tier + ".banner");
      });
      // A macOS alert sound, or none. A name that is not one of them (set
      // in config.yml) stays listed, so it is never dropped.
      const current = tiers[tier].sound || "";
      const names = SOUNDS.indexOf(current) === -1 && current ? SOUNDS.concat([current]) : SOUNDS;
      const sound = UI.select({ id: "tier-" + tier + "-sound", label: tier[0].toUpperCase() + tier.slice(1) + " sound", cls: "sound", value: current,
        options: [{ value: "", label: "No sound" }].concat(names.map(function (n, i) { return { value: n, label: n, divider: i === 0 }; })),
        onchange: function (next) {
          tiers[tier].sound = next === "" ? null : next;
          app.changed("notification_tiers." + tier + ".sound");
        } });
      const control = UI.el("div", { class: "inline tier" }, [
        UI.el("label", { class: "check", for: banner.id }, [banner, document.createTextNode("Banner")]), sound.node]);
      return UI.row(app, "notification_tiers." + tier, tier[0].toUpperCase() + tier.slice(1), { node: control, focus: banner },
        { help: TIER_NOTES[tier] });
    });
  }

  renderers.notifications = function (root, app) {
    const sections = fieldSections(app, "notifications");
    const events = sections.find(function (s) { return s.title === "Events"; });
    if (events) events.note = "How loudly each event reaches you.";
    root.append.apply(root, [UI.header("Notifications", "Every event belongs to a tier, and each tier decides whether you get a banner and a sound.")]
      .concat(build(sections), [UI.section("Tiers", tierRows(app), "What each tier does when an event lands in it.")]));
    // With delivery off, the event and tier settings do nothing: dim them.
    const deliver = root.querySelector("#" + UI.fieldId("defaults.notifications.enabled"));
    const panels = Array.prototype.slice.call(root.querySelectorAll("section > .panel")).slice(1);
    function sync() { panels.forEach(function (p) { p.classList.toggle("inactive", !!deliver && !deliver.checked); }); }
    if (deliver) deliver.addEventListener("change", sync);
    sync();
  };

  renderers.identity = function (root, app) {
    root.append.apply(root, [UI.header("Account & Browser", OVERRIDE_NOTE)].concat(build(fieldSections(app, "identity"))));
  };

  window.prxPages = Object.freeze({ fieldSections: fieldSections, build: build, OVERRIDE_NOTE: OVERRIDE_NOTE });
})();
