// Engines page (the `engines:` map, design D1: agent CLIs the daemon found
// on this machine with their command, argument template, and model
// arguments, plus LLM API engines with provider, model, and the key's
// environment variable) and the config.yml page (the file's text, saved
// verbatim through the same validated PUT /config path — comments survive).
(function () {
  "use strict";
  const F = window.prxFields;
  const S = window.prxSchema;
  const UI = window.prxForm;
  const renderers = window.prxPageRenderers = window.prxPageRenderers || {};

  function textInput(value, label, placeholder, onchange) {
    const input = UI.el("input", { type: "text", spellcheck: "false", autocomplete: "off", "aria-label": label, placeholder: placeholder || null });
    input.value = value || "";
    input.addEventListener("change", function () { onchange(input.value.trim()); });
    return input;
  }

  function lines(value, label, placeholder, onchange) {
    const area = UI.el("textarea", { rows: Math.max(2, Math.min(6, (value || []).length + 1)), spellcheck: "false", "aria-label": label, placeholder: placeholder });
    area.value = (value || []).join("\n");
    area.addEventListener("change", function () { onchange(F.splitLines(area.value)); });
    return area;
  }

  // Rename an `engines:` entry in place, following the defaults' engine.
  function renameEngine(app, engines, from, base) {
    return function (next) {
      if (!next || next === from) { app.rerender(); return; }
      if (engines[next]) { app.localError(base, "An engine named " + next + " already exists"); return; }
      // Rebuild the map so the renamed entry keeps its position, and
      // follow the rename where the defaults point at it.
      const rebuilt = {};
      Object.keys(engines).forEach(function (k) { rebuilt[k === from ? next : k] = engines[k]; });
      app.draft().engines = rebuilt;
      if (app.draft().defaults.engine === from) app.draft().defaults.engine = next;
      app.localError(base, null);
      app.changed(base);
      app.rerender();
    };
  }

  function agentEditor(app, agents, agent) {
    const own = agents[agent.name];
    const base = "engines." + agent.name;
    const grid = [];
    function field(label, help, control) {
      const id = UI.fieldId(base + "." + label);
      control.id = id;
      grid.push(UI.el("div", { class: "agent-field" }, [
        UI.el("label", { for: id, text: label }), control, help ? UI.el("p", { class: "help" }, UI.rich(help)) : null]));
    }
    if (!agent.known) {
      field("Name", null, textInput(agent.name, "Agent name", null, renameEngine(app, agents, agent.name, base)));
    }
    field("Command", "A program name on the daemon's PATH, or a full path.", textInput(own.command, "Command", "command", function (next) {
      own.command = next;
      app.changed(base + ".command");
    }));
    field("Arguments", "One per line. `{{PROMPT}}` becomes the review prompt; a lone `-` sends it on stdin instead.",
      lines(own.args, "Arguments, one per line", "-p\n{{PROMPT}}", function (next) {
        own.args = next;
        app.changed(base + ".args");
      }));
    field("Model arguments", "Added only when a model is chosen. `{{MODEL}}` becomes its name.",
      lines(own.model_args, "Model arguments, one per line", "--model\n{{MODEL}}", function (next) {
        if (next.length) own.model_args = next; else delete own.model_args;
        app.changed(base + ".model_args");
      }));
    // usage_output (token-usage spec): optional; an explicit mapping wins
    // over the built-in claude/codex parsing.
    const usage = UI.el("textarea", { rows: Math.max(2, Math.min(7, Object.keys(own.usage_output || {}).length + 1)), spellcheck: "false",
      "aria-label": "Usage fields, one per line", placeholder: "input_tokens = usage.input_tokens\noutput_tokens = usage.output_tokens" });
    usage.value = F.usageOutputToText(own.usage_output, S.USAGE_FIELDS);
    usage.addEventListener("change", function () {
      const parsed = F.parseUsageOutput(usage.value, S.USAGE_FIELDS);
      if (!parsed.ok) { app.localError(base + ".usage_output", parsed.error); return; }
      app.localError(base + ".usage_output", null);
      if (parsed.value) own.usage_output = parsed.value; else delete own.usage_output;
      app.changed(base + ".usage_output");
    });
    field("Usage fields", "Optional. Where the token counts sit in the agent's JSON output, one `field = json.path` per line. " +
      "Fields: " + S.USAGE_FIELDS.map(function (f) { return "`" + f + "`"; }).join(", ") + ". Leave empty when the agent reports no usage.", usage);
    return UI.el("div", { class: "agent-editor" }, grid);
  }

  function useButton(app, name) {
    const draft = app.draft();
    return UI.button("Use for reviews", function () {
      draft.defaults.engine = name;
      app.changed("defaults.engine");
      app.rerender();
    });
  }

  // Flags that let an agent act without asking (trust-sensitive, audit P2).
  const UNATTENDED = /^--(dangerously-skip-permissions|dangerously-allow-all|yolo|force|allow-all-tools)$/;

  function trustNote(agent) {
    const args = (agent.args && agent.args.length ? agent.args : agent.preset && agent.preset.args) || [];
    const flag = args.find(function (a) { return UNATTENDED.test(a); });
    if (!flag) return null;
    return UI.el("p", { class: "help" }, UI.rich("`" + flag + "` lets the agent use its tools without asking, which a review in the background needs. " +
      "It runs as you; PRX's prompt tells it to stay read-only, and only PRX posts to GitHub."));
  }

  function agentItem(app, agents, agent) {
    const draft = app.draft();
    const base = "engines." + agent.name;
    const current = draft.defaults.engine === agent.name;
    const error = UI.el("p", { class: "field-error", role: "alert", hidden: true });
    const actions = [];
    if (agent.usable && !current) actions.push(useButton(app, agent.name));
    if (!agent.configured && agent.preset && agent.preset.args.length) {
      // Found outside the daemon's PATH: pin the full path so it runs.
      const command = agent.path && !agent.on_path ? agent.path : agent.preset.command;
      actions.push(UI.button(agent.builtin ? "Customize" : "Add", function () {
        const entry = { type: "agent", command: command, args: agent.preset.args.slice() };
        if (agent.preset.model_args.length) entry.model_args = agent.preset.model_args.slice();
        agents[agent.name] = entry;
        app.changed(base);
        app.rerender();
      }));
    } else if (agent.configured) {
      actions.push(UI.button(agent.builtin ? "Restore built-in" : "Remove", function () {
        delete agents[agent.name];
        app.localError(base, null);
        app.changed("engines");
        app.rerender();
      }, agent.builtin ? null : "danger"));
    }
    const tags = [];
    if (current) tags.push(UI.el("span", { class: "badge accent", text: "Reviewing" }));
    if (agent.builtin && agent.configured) tags.push(UI.el("span", { class: "badge", text: "Customized" }));
    else if (agent.builtin) tags.push(UI.el("span", { class: "badge", text: "Built in" }));
    else if (!agent.known) tags.push(UI.el("span", { class: "badge", text: "Custom" }));
    const node = UI.el("div", { class: "agent" + (agent.usable ? "" : " unused"), "data-field": base }, [
      UI.el("div", { class: "agent-head" }, [
        UI.el("div", { class: "agent-title" }, [
          UI.el("span", { class: "agent-name", text: agent.label }),
          agent.label !== agent.name ? UI.el("code", { text: agent.name }) : null,
        ].concat(tags)),
        UI.el("div", { class: "agent-actions" }, actions),
      ]),
      UI.agentStatus(agent, true),
      trustNote(agent),
      agent.configured ? agentEditor(app, agents, agent)
        : agent.preset && agent.preset.args.length ? UI.el("p", { class: "status-line" }, [UI.el("code", { text: F.commandLine(agent) })]) : null,
      error,
    ]);
    app.register(base, { row: node, error: error, focus: node.querySelector("input, button") });
    return node;
  }

  // An `llm_api` entry: provider, endpoint, model, key variable, sampling.
  function llmEditor(app, engines, name) {
    const own = engines[name];
    const base = "engines." + name;
    const grid = [];
    // `control` is an input, or a form.js select ({node, focus}) whose
    // trigger already carries the id and is named by the label.
    function field(label, key, help, control) {
      const id = UI.fieldId(base + "." + key);
      const combo = !!control.node && typeof control.focus === "object";
      if (combo) control.focus.setAttribute("aria-labelledby", id + "-label");
      else control.id = id;
      grid.push(UI.el("div", { class: "agent-field", "data-field": base + "." + key }, [
        UI.el(combo ? "span" : "label", { for: combo ? null : id, id: id + "-label", class: "agent-field-label", text: label }),
        combo ? control.node : control, help ? UI.el("p", { class: "help" }, UI.rich(help)) : null]));
    }
    function setOrUnset(key, value) {
      if (value === null || value === "") delete own[key]; else own[key] = value;
      app.changed(base + "." + key);
    }
    function numberInput(key, label, parse) {
      const input = textInput(own[key] === undefined || own[key] === null ? "" : String(own[key]), label, "Provider default", function (next) {
        if (next === "") { app.localError(base + "." + key, null); setOrUnset(key, null); return; }
        const value = parse(next);
        if (value === null) { app.localError(base + "." + key, label + " must be a number"); return; }
        app.localError(base + "." + key, null);
        setOrUnset(key, value);
      });
      input.setAttribute("inputmode", "decimal");
      return input;
    }
    field("Name", "name", null, textInput(name, "Engine name", null, renameEngine(app, engines, name, base)));
    const provider = UI.select({ id: UI.fieldId(base + ".provider"), value: own.provider || "anthropic",
      options: S.LLM_PROVIDERS.map(function (p) { return { value: p.value, label: p.label, detail: p.detail || null }; }),
      onchange: function (next) { setOrUnset("provider", next); } });
    field("Provider", "provider", null, provider);
    field("Base URL", "base_url", "Leave empty for the provider's own endpoint. Set it for a local or self-hosted OpenAI-compatible server.",
      textInput(own.base_url, "Base URL", "https://…", function (next) { setOrUnset("base_url", next); }));
    field("Model", "model", null, textInput(own.model, "Model", "e.g. claude-haiku-4-5", function (next) { setOrUnset("model", next); }));
    field("Key variable", "api_key_env", "Name of the environment variable holding the API key, e.g. `ANTHROPIC_API_KEY`. The key itself is never stored.",
      textInput(own.api_key_env, "Key variable", "ANTHROPIC_API_KEY", function (next) { setOrUnset("api_key_env", next); }));
    field("Temperature", "temperature", null, numberInput("temperature", "Temperature", function (t) {
      return /^\d+(\.\d+)?$/.test(t) ? Number(t) : null;
    }));
    field("Max tokens", "max_tokens", null, numberInput("max_tokens", "Max tokens", function (t) {
      const parsed = F.parseNumber(t, 1);
      return parsed.ok ? parsed.value : null;
    }));
    return UI.el("div", { class: "agent-editor" }, grid);
  }

  function llmItem(app, engines, engine) {
    const draft = app.draft();
    const base = "engines." + engine.name;
    const current = draft.defaults.engine === engine.name;
    const error = UI.el("p", { class: "field-error", role: "alert", hidden: true });
    const actions = [];
    if (!current) actions.push(useButton(app, engine.name));
    actions.push(UI.button("Remove", function () {
      delete engines[engine.name];
      app.localError(base, null);
      app.changed("engines");
      app.rerender();
    }, "danger"));
    const tags = [UI.el("span", { class: "badge", text: "LLM API" })];
    if (current) tags.unshift(UI.el("span", { class: "badge accent", text: "Reviewing" }));
    const node = UI.el("div", { class: "agent", "data-field": base }, [
      UI.el("div", { class: "agent-head" }, [
        UI.el("div", { class: "agent-title" }, [UI.el("span", { class: "agent-name", text: engine.name })].concat(tags)),
        UI.el("div", { class: "agent-actions" }, actions),
      ]),
      UI.llmStatus(engine),
      llmEditor(app, engines, engine.name),
      error,
    ]);
    app.register(base, { row: node, error: error, focus: node.querySelector("input, button") });
    return node;
  }

  // "Name + Add" row creating a fresh entry from `make()`.
  function addRow(app, engines, id, label, help, make) {
    const all = app.engines();
    const newName = UI.el("input", { type: "text", placeholder: "Name, e.g. my-reviewer", spellcheck: "false", autocomplete: "off", id: id });
    function add() {
      const name = newName.value.trim();
      if (!name) { newName.focus(); return; }
      if (/\s/.test(name)) { app.localError("engines", "An engine name can't contain spaces"); return; }
      if (engines[name] || all.some(function (a) { return a.name === name && a.usable; })) {
        app.localError("engines", "An engine named " + name + " already exists");
        return;
      }
      app.localError("engines", null);
      engines[name] = make();
      app.changed("engines");
      app.rerender("engines." + name);
    }
    newName.addEventListener("keydown", function (e) { if (e.key === "Enter") { e.preventDefault(); add(); } });
    // Only the first add row owns the map-level error slot.
    return UI.row(app, id === "add-agent" ? "engines" : "engines-" + id, label,
      { node: UI.el("div", { class: "inline" }, [newName, UI.button("Add", add)]), focus: newName }, { help: help });
  }

  renderers.engines = function (root, app) {
    const draft = app.draft();
    draft.engines = draft.engines || {};
    const engines = draft.engines;
    const all = app.agents();
    const ready = all.filter(function (a) { return a.usable; });
    const more = all.filter(function (a) { return !a.usable; });
    const llms = F.llmEngines(engines, app.found());
    const nodes = [
      UI.header("Engines", "What reads each PR: a command-line agent, or an LLM API that PRX calls itself. Either way PRX collects the findings, and the engine never posts to GitHub."),
      UI.section("Agents", ready.map(function (a) { return agentItem(app, engines, a); }).concat([
        addRow(app, engines, "add-agent", "Add your own", "Any CLI that takes a prompt and can write a file.",
          function () { return { type: "agent", command: "", args: ["{{PROMPT}}"] }; }),
      ]), "Pick the one that reviews on the Review page, or here."),
      UI.section("LLM APIs", llms.map(function (l) { return llmItem(app, engines, l); }).concat([
        addRow(app, engines, "add-llm", "Add an LLM API", "PRX sends the prompt and the diff to the API and reads the findings from its answer.",
          function () { return { type: "llm_api", provider: "anthropic", model: "", api_key_env: "ANTHROPIC_API_KEY" }; }),
      ]), "Reviews through a model API. Keys stay in environment variables."),
    ];
    if (more.length) {
      nodes.push(UI.section("More agents", more.map(function (a) { return agentItem(app, engines, a); }),
        "Presets for other agent CLIs. Add one to make it selectable, then adjust its arguments if your setup differs."));
    }
    root.append.apply(root, nodes);
  };

  // Global top-level fields (`top: true`, e.g. the `menu` section): edited
  // at their own config path, never per org or repo.
  function topRow(app, field) {
    const draft = app.draft();
    const value = F.getAt(draft, field.key);
    function set(next) {
      if (next === null) F.unsetAt(draft, field.key);
      else F.setAt(draft, field.key, next);
      app.changed(field.key);
    }
    const parts = UI.control(app, field.key, field, value === undefined || value === null ? field.fallback || null : value, set, {});
    return UI.row(app, field.key, field.label, parts, { help: field.help, stack: field.stack, bare: field.bare });
  }

  renderers.menubar = function (root, app) {
    const sections = [];
    S.FIELDS.filter(function (f) { return f.page === "menubar"; }).forEach(function (field) {
      let current = sections[sections.length - 1];
      if (!current || current.title !== field.section) sections.push(current = { title: field.section, rows: [] });
      current.rows.push(topRow(app, field));
    });
    root.append.apply(root, [UI.header("Menu bar",
      "How the menu bar lists your PRs. These apply everywhere; orgs and repos can't change them. The menu picks up a saved change on its next refresh.")]
      .concat(sections.map(function (s) { return UI.section(s.title, s.rows); })));
  };

  renderers.raw = function (root, app) {
    const loaded = app.loaded();
    const yaml = UI.el("textarea", { id: "raw-yaml", spellcheck: "false", "aria-label": "config.yml" });
    yaml.value = loaded.yaml || "";
    const error = UI.el("p", { class: "field-error", role: "alert", hidden: true });
    app.register("raw", { row: yaml, error: error, focus: yaml });
    root.append.apply(root, [
      UI.header("config.yml", "The file itself. It is saved exactly as typed, comments included, once the daemon accepts it. Nothing is written while it is invalid."),
      app.isDirty() ? UI.el("p", { class: "banner warn" }, [UI.icon("alert"),
        UI.el("span", { text: "You have unsaved changes on other pages. This shows the file as last saved, and saving here discards them." })]) : null,
      yaml,
      error,
      UI.el("div", { class: "actions" }, [
        UI.el("code", { class: "path", text: loaded.path || "config.yml" }),
        UI.button("Reload from disk", function () { app.reload(); }),
        UI.el("button", { type: "button", id: "raw-save", class: "btn primary", onclick: function () { app.saveYaml(yaml.value); }, text: "Save file" }),
      ]),
    ].filter(Boolean));
  };
})();
