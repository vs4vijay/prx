// DOM helpers for the settings form: element builder, one control per
// schema field type, and labelled rows that register their input path with
// the app so daemon field errors render next to the offending input.
(function () {
  "use strict";
  const F = window.prxFields;
  const Cron = window.prxCron;
  const icon = window.prxIcon;

  function el(tag, attrs, children) {
    const node = document.createElement(tag);
    Object.entries(attrs || {}).forEach(function ([key, value]) {
      if (value === undefined || value === null || value === false) return;
      if (key === "text") node.textContent = value;
      else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
      else node.setAttribute(key, value === true ? "" : value);
    });
    (children || []).forEach(function (child) { if (child) node.append(child); });
    return node;
  }

  // A push button; "link ..." classes make a text-only one instead.
  function button(text, onclick, cls) {
    const link = /(^| )link( |$)/.test(cls || "");
    return el("button", { type: "button", class: link ? cls : "btn" + (cls ? " " + cls : ""), onclick: onclick, text: text });
  }

  function iconButton(name, label, onclick, cls) {
    return el("button", { type: "button", class: "icon-button" + (cls ? " " + cls : ""), "aria-label": label, title: label, onclick: onclick }, [icon(name)]);
  }

  function fieldId(path) { return "f-" + path.replace(/[^A-Za-z0-9_-]/g, "_"); }

  // Text with `code` spans, as DOM (help lines, status lines).
  function rich(text) {
    const nodes = [];
    String(text).split(/(`[^`]+`)/).forEach(function (part) {
      if (!part) return;
      if (part[0] === "`" && part.length > 2) nodes.push(el("code", { text: part.slice(1, -1) }));
      else nodes.push(document.createTextNode(part));
    });
    return nodes;
  }

  // A radio group drawn as one segmented control.
  function segmented(id, options, value, onchange, label) {
    const group = el("div", { class: "segmented", role: "radiogroup", id: id, "aria-label": label || null });
    const inputs = options.map(function (option, i) {
      const input = el("input", { type: "radio", name: id, id: id + "-" + i, value: option.value });
      input.checked = option.value === value;
      input.addEventListener("change", function () { if (input.checked) onchange(option.value); });
      group.append(input, el("label", { for: id + "-" + i, text: option.label }));
      return input;
    });
    return { node: group, focus: inputs.find(function (i) { return i.checked; }) || inputs[0], group: group };
  }

  // ---------------------------------------------------------------- select
  //
  // The window's one dropdown (every choice, settings and filters alike):
  // an APG select-only combobox. Focus stays on the trigger button (role
  // combobox); the open listbox is tracked with aria-activedescendant.
  //   closed: ↓ ↑ Enter Space open; Home/End open at the ends; a letter
  //           opens on the first match.
  //   open:   ↓ ↑ move, Home/End/PageUp/PageDown jump, letters typed within
  //           0.7 s type ahead, Enter or Space picks, Esc closes unchanged,
  //           Tab closes unchanged.
  // The open list floats on <body> at fixed coordinates, so no scroll
  // container or overflow clips it; it flips above the trigger when there
  // is more room there, follows the trigger while the page scrolls, and
  // closes when the trigger scrolls out of view or leaves the page.
  //
  // opts: id, label (aria-label) or labelledBy, options, value,
  //   onchange(value, option) — returning false keeps the old selection —,
  //   defaultValue (the built-in value: its option is tagged "Default"),
  //   unset (the field has no value of its own, so the trigger says
  //   "Default" too), cls, disabled.
  // option: { value, label, detail?, icon? (icon name | {src}), tag?,
  //   display? (the trigger's text when selected), divider? (a rule above),
  //   disabled?, always? (fires onchange even when already selected) }.
  // Returns { node, focus, trigger, list, value(), set(value),
  //   setOptions(options, value), isOpen() }.
  let selectSeq = 0;
  const INHERIT = "\u0000inherit";

  function select(opts) {
    const id = opts.id || "select-" + (++selectSeq);
    let options = [];
    let value = opts.value;
    let unset = !!opts.unset;
    let selected = null;
    let active = -1;
    let typed = "";
    let typedAt = 0;
    let floating = false;
    let height = 0;
    const trigger = el("button", { type: "button", id: id, class: "select-trigger", role: "combobox", "aria-haspopup": "listbox",
      "aria-expanded": "false", "aria-controls": id + "-list", "aria-label": opts.label || null,
      "aria-labelledby": opts.labelledBy || null, disabled: !!opts.disabled });
    const list = el("ul", { role: "listbox", id: id + "-list", class: "select-list", tabindex: "-1",
      "aria-label": opts.label || null, "aria-labelledby": opts.labelledBy || null });
    list.hidden = true;
    const node = el("div", { class: "select" + (opts.cls ? " " + opts.cls : "") }, [trigger, list]);

    function same(a, b) { return a === b || ((a === null || a === undefined) && (b === null || b === undefined)); }

    function iconNode(o, slot) {
      if (o.icon && typeof o.icon === "object") return el("img", { class: "select-icon", alt: "", src: o.icon.src, width: "16", height: "16" });
      if (o.icon) return el("span", { class: "select-icon", "aria-hidden": "true" }, [icon(o.icon)]);
      return slot ? el("span", { class: "select-icon blank", "aria-hidden": "true" }) : null;
    }

    function tagOf(o, onTrigger) {
      if (o.tag) return o.tag;
      if (opts.defaultValue !== undefined && same(o.value, opts.defaultValue) && (!onTrigger || unset)) return "Default";
      return null;
    }

    function render() {
      selected = options.find(function (o) { return same(o.value, value); }) || null;
      const slot = options.some(function (o) { return !!o.icon; });
      list.replaceChildren.apply(list, options.map(function (o, i) {
        const tag = tagOf(o, false);
        return el("li", { role: "option", id: id + "-opt-" + i,
          class: "select-option" + (o.detail ? " has-detail" : "") + (o.divider ? " divider" : "") + (i === active ? " active" : ""),
          "aria-selected": o === selected ? "true" : "false", "aria-disabled": o.disabled ? "true" : null,
          onmousedown: function (event) { event.preventDefault(); },
          onclick: function () { choose(o); },
          onmousemove: function () { if (active !== i && !o.disabled) setActive(i); } }, [
          el("span", { class: "select-check", "aria-hidden": "true" }, [o === selected ? icon("check") : null]),
          iconNode(o, slot),
          el("span", { class: "select-text" }, [
            el("span", { class: "select-line" }, [el("span", { class: "select-label", text: o.label }),
              tag ? el("span", { class: "select-tag", text: tag }) : null]),
            o.detail ? el("span", { class: "select-detail", text: o.detail }) : null,
          ]),
        ]);
      }));
      const shown = selected || { label: opts.placeholder || "Choose…", value: undefined };
      const tag = selected ? tagOf(selected, true) : null;
      trigger.replaceChildren(
        iconNode(shown, false) || el("span", { hidden: true }),
        el("span", { class: "select-value" }, [el("span", { class: "select-label", text: shown.display || shown.label }),
          tag ? el("span", { class: "select-tag", text: tag }) : null]),
        el("span", { class: "select-caret", "aria-hidden": "true" }, [icon("updown")]));
      trigger.setAttribute("class", "select-trigger" + (selected && selected.value === INHERIT ? " inherited" : "") + (selected ? "" : " placeholder"));
    }

    function isOpen() { return !list.hidden; }

    function enabled(i) { return options[i] && !options[i].disabled; }

    function setActive(i) {
      active = Math.max(0, Math.min(options.length - 1, i));
      Array.prototype.forEach.call(list.children, function (item, n) {
        const cls = item.getAttribute("class").replace(/ active\b/g, "");
        item.setAttribute("class", n === active ? cls + " active" : cls);
      });
      if (isOpen()) trigger.setAttribute("aria-activedescendant", id + "-opt-" + active);
      const item = list.children[active];
      if (item && typeof item.scrollIntoView === "function") item.scrollIntoView({ block: "nearest" });
    }

    // Move by `step` over enabled options, clamped at the ends.
    function move(from, step) {
      for (let i = from + step; i >= 0 && i < options.length; i += step) if (enabled(i)) return setActive(i);
      return undefined;
    }

    function edge(last) {
      const order = options.map(function (o, i) { return i; });
      if (last) order.reverse();
      const i = order.find(enabled);
      if (i !== undefined) setActive(i);
    }

    // Float the list on <body> (fixed), below the trigger or above it.
    function place() {
      if (!floating) return;
      if (!trigger.isConnected) { close(false); return; }
      const r = trigger.getBoundingClientRect();
      const vh = window.innerHeight;
      const vw = window.innerWidth;
      if (r.bottom < 0 || r.top > vh) { close(false); return; }
      const below = vh - r.bottom - 10;
      const above = r.top - 10;
      const up = height > below && above > below;
      const room = Math.min(320, Math.max(96, up ? above : below));
      const h = Math.min(height, room);
      list.style.maxHeight = room + "px";
      list.style.minWidth = Math.round(r.width) + "px";
      const width = list.offsetWidth;
      list.style.left = Math.round(Math.max(8, Math.min(r.left, vw - width - 8))) + "px";
      list.style.top = Math.round(up ? r.top - 4 - h : r.bottom + 4) + "px";
      list.setAttribute("data-side", up ? "top" : "bottom");
    }

    function follow() {
      if (!floating) return;
      place();
      window.requestAnimationFrame(follow);
    }

    function onOutside(event) {
      if (!node.contains(event.target) && !list.contains(event.target)) close(false);
    }

    function open(at) {
      if (isOpen() || trigger.disabled) return;
      typed = "";
      ["aria-label", "aria-labelledby"].forEach(function (name) {
        const v = trigger.getAttribute(name);
        if (v) list.setAttribute(name, v);
      });
      list.hidden = false;
      trigger.setAttribute("aria-expanded", "true");
      if (document.body && typeof trigger.getBoundingClientRect === "function" && typeof window.requestAnimationFrame === "function") {
        document.body.append(list);
        list.setAttribute("class", "select-list floating" + (opts.cls ? " " + opts.cls : ""));
        list.style.maxHeight = "none";
        height = Math.min(list.scrollHeight, 320);
        floating = true;
        follow();
        document.addEventListener("mousedown", onOutside, true);
      }
      const start = at === "first" ? -1 : at === "last" ? options.length : options.indexOf(selected);
      if (at === "first") move(-1, 1);
      else if (at === "last") move(options.length, -1);
      else if (start >= 0) setActive(start);
      else move(-1, 1);
    }

    function close(refocus) {
      if (isOpen()) {
        list.hidden = true;
        trigger.setAttribute("aria-expanded", "false");
        trigger.removeAttribute("aria-activedescendant");
        if (floating) {
          floating = false;
          document.removeEventListener("mousedown", onOutside, true);
          list.setAttribute("class", "select-list");
          node.append(list);
        }
      }
      if (refocus) trigger.focus();
    }

    function choose(o) {
      if (!o || o.disabled) return;
      close(true);
      if (same(o.value, value) && !o.always) return;
      const previous = value;
      const wasUnset = unset;
      value = o.value;
      unset = false;
      render();
      const result = opts.onchange ? opts.onchange(o.value, o) : undefined;
      if (result === false) { value = previous; unset = wasUnset; render(); }
      // The change re-rendered the page: focus the control that replaced
      // this one (same id).
      if (trigger.isConnected === false && typeof document.getElementById === "function") {
        const again = document.getElementById(id);
        if (again) again.focus();
      }
    }

    // Letters typed within 0.7 s form a prefix; the next enabled option
    // (from the active one, wrapping) whose label starts with it is active.
    function typeAhead(key, at) {
      const now = typeof at === "number" ? at : Date.now();
      typed = now - typedAt > 700 ? key.toLowerCase() : typed + key.toLowerCase();
      typedAt = now;
      const start = typed.length === 1 ? active + 1 : Math.max(active, 0);
      for (let n = 0; n < options.length; n++) {
        const i = (start + n) % options.length;
        if (enabled(i) && options[i].label.toLowerCase().indexOf(typed) === 0) return setActive(i);
      }
      return undefined;
    }

    trigger.addEventListener("click", function () { if (isOpen()) close(true); else open(); });
    trigger.addEventListener("blur", function () { close(false); });
    trigger.addEventListener("keydown", function (event) {
      const key = event.key;
      const typing = key.length === 1 && /\S/.test(key) && !event.metaKey && !event.ctrlKey;
      if (!isOpen()) {
        if (key === "ArrowDown" || key === "ArrowUp" || key === "Enter" || key === " ") open();
        else if (key === "Home") open("first");
        else if (key === "End") open("last");
        else if (typing) { open(); typeAhead(key, event.timeStamp); }
        else return;
      } else if (key === "ArrowDown") { if (event.altKey) choose(options[active]); else move(active, 1); }
      else if (key === "ArrowUp") { if (event.altKey) choose(options[active]); else move(active, -1); }
      else if (key === "Home") edge(false);
      else if (key === "End") edge(true);
      else if (key === "PageDown") { setActive(active + 10); if (!enabled(active)) move(active, -1); }
      else if (key === "PageUp") { setActive(active - 10); if (!enabled(active)) move(active, 1); }
      else if (key === " " && typed && event.timeStamp - typedAt < 700) typeAhead(key, event.timeStamp);
      else if (key === "Enter" || key === " ") choose(options[active]);
      else if (key === "Escape") { close(true); if (event.stopPropagation) event.stopPropagation(); }
      else if (key === "Tab") { close(false); return; }
      else if (typing) typeAhead(key, event.timeStamp);
      else return;
      event.preventDefault();
    });

    function setOptions(next, nextValue) {
      options = next.slice();
      if (arguments.length > 1) value = nextValue;
      active = -1;
      render();
    }

    setOptions(opts.options || []);
    return {
      node: node, focus: trigger, trigger: trigger, list: list, isOpen: isOpen, setOptions: setOptions,
      value: function () { return value; },
      set: function (next) { value = next; unset = false; render(); },
    };
  }

  // A schema choice as a dropdown, with Inherit on org/repo rows.
  // items: [{value, label, detail, icon}]. opts.inherit: offer Inherit;
  // opts.inherited: the row has no own value; opts.parent: what Inherit
  // stands for (fields.js resolveParent); opts.parentText: its label when
  // it is not among the items. Picking Inherit commits null.
  function inheritOption(items, opts) {
    const parent = opts.parent || { value: null, source: "defaults" };
    const match = items.find(function (o) { return o.value === parent.value; });
    const text = opts.parentText || (match ? match.label : parent.value === null || parent.value === undefined ? "Not set" : String(parent.value));
    return { value: INHERIT, label: "Inherit", detail: text + ", from " + F.inheritSource(parent), display: text, tag: "Inherited",
      icon: match && match.icon ? match.icon : null };
  }

  function schemaSelect(id, items, value, commit, opts) {
    opts = opts || {};
    let options = items.slice();
    if (value !== null && value !== undefined && !(opts.inherit && opts.inherited) && !items.some(function (o) { return o.value === value; })) {
      options.push({ value: value, label: String(value), detail: "Not a known value. It is kept as it is." });
    }
    if (opts.inherit) {
      if (options.length) options[0] = Object.assign({}, options[0], { divider: true });
      options.unshift(inheritOption(items, opts));
    }
    return select({ id: id, label: opts.label, options: options,
      value: opts.inherit && opts.inherited ? INHERIT : value,
      defaultValue: opts.inherit ? undefined : opts.defaultValue, unset: opts.unset, cls: opts.cls,
      onchange: function (next) { commit(next === INHERIT ? null : next); } });
  }

  // Typing marks the form dirty at once, so Save enables while typing
  // (the value itself commits on change, which Save forces with a blur).
  function touchOnInput(app, path, input) {
    input.addEventListener("input", function () { if (typeof app.changed === "function") app.changed(path); });
  }

  function numberControl(app, path, field, value, commit) {
    const input = el("input", { id: fieldId(path), type: "text", inputmode: "numeric", class: "num", autocomplete: "off" });
    input.value = value === null || value === undefined ? "" : String(value);
    touchOnInput(app, path, input);
    // Only "is it a number" is checked here; ranges (e.g. a zero poll
    // interval) are the daemon's validation, surfaced on save.
    input.addEventListener("change", function () {
      const parsed = F.parseNumber(input.value);
      app.localError(path, parsed.ok ? null : parsed.error);
      if (parsed.ok) commit(parsed.value);
      else app.changed(path);
    });
    // The unit slot is kept even when empty so a column of numbers lines up.
    return { node: el("span", { class: "with-unit" }, [input, el("span", { class: "unit", text: field.unit || "" })]), focus: input };
  }

  // A choice field: every option with what it does as radio rows (the
  // page), or a dropdown with the same descriptions (the compact override
  // editor, which also offers Inherit).
  function choiceControl(path, field, value, commit, opts) {
    const id = fieldId(path);
    if (opts.compact) {
      return schemaSelect(id, field.options.map(function (o) {
        return { value: o.value, label: o.label, detail: o.detail, icon: o.icon || null };
      }), value, commit, { inherit: opts.inherit, inherited: opts.inherited, parent: opts.parent,
        defaultValue: field.fallback, unset: opts.unset });
    }
    const group = el("div", { class: "choices", role: "radiogroup" });
    const inputs = field.options.map(function (option, i) {
      const input = el("input", { type: "radio", name: id, id: id + "-" + i, value: option.value });
      input.checked = option.value === value;
      input.addEventListener("change", function () { if (input.checked) commit(option.value); });
      group.append(el("label", { class: "choice", for: id + "-" + i }, [input,
        el("span", { class: "choice-text" }, [el("span", { class: "choice-label", text: option.label }), el("span", { class: "choice-detail", text: option.detail })])]));
      return input;
    });
    return { node: group, focus: inputs.find(function (i) { return i.checked; }) || inputs[0], group: group };
  }

  // Review mode on its page: three cards, one per mode, each saying whether
  // reviews run, whether anything reaches GitHub, and where verdicts show
  // (schema MODE_OPTIONS facts). A radio group underneath (arrow keys move
  // and pick, like any radio group). The compact override editor uses the
  // dropdown, with the same one-line summaries and Inherit.
  function modeControl(path, field, value, commit, opts) {
    if (opts.compact) return choiceControl(path, field, value, commit, opts);
    const id = fieldId(path);
    const group = el("div", { class: "mode-cards", role: "radiogroup" });
    const inputs = field.options.map(function (option, i) {
      const input = el("input", { type: "radio", class: "mode-radio", name: id, id: id + "-" + i, value: option.value,
        "aria-describedby": id + "-" + i + "-facts" });
      input.checked = option.value === value;
      input.addEventListener("change", function () { if (input.checked) commit(option.value); });
      const isDefault = opts.unset && option.value === field.fallback;
      group.append(el("label", { class: "mode-card", for: input.id, "data-mode": option.value }, [input,
        el("span", { class: "mode-head" }, [
          el("span", { class: "mode-glyph", "aria-hidden": "true" }, [icon(option.icon)]),
          el("span", { class: "mode-title", text: option.label }),
          isDefault ? el("span", { class: "select-tag", text: "Default" }) : null,
          el("span", { class: "mode-tick", "aria-hidden": "true" }, [icon("check")]),
        ]),
        el("span", { class: "mode-summary", text: option.detail }),
        el("span", { class: "mode-facts", id: id + "-" + i + "-facts" }, (option.facts || []).map(function (f) {
          return el("span", { class: "mode-fact " + f[0] }, [icon(f[0] === "yes" ? "check" : "minus"), el("span", { text: f[1] })]);
        })),
      ]));
      return input;
    });
    return { node: group, focus: inputs.find(function (i) { return i.checked; }) || inputs[0], group: group };
  }

  // A list edited as removable chips: Enter or a comma adds, Backspace on
  // an empty input removes the last one, pasting a comma list adds all.
  function tokensControl(path, field, value, commit) {
    const id = fieldId(path);
    let items = (value || []).slice();
    const input = el("input", { id: id, type: "text", spellcheck: "false", autocomplete: "off", placeholder: items.length ? null : "Add " + (field.placeholder || "item") });
    const box = el("div", { class: "tokens" });
    function draw() {
      box.replaceChildren();
      items.forEach(function (item, i) {
        const remove = el("button", { type: "button", "aria-label": "Remove " + item, onclick: function () {
          items.splice(i, 1);
          draw();
          commit(items.slice());
          input.focus();
        } }, [icon("close")]);
        box.append(el("span", { class: "token" }, [el("span", { text: item }), remove]));
      });
      input.placeholder = items.length ? "" : "Add " + (field.placeholder || "item");
      box.append(input);
    }
    function take() {
      const added = F.splitList(input.value).filter(function (item) { return items.indexOf(item) === -1; });
      input.value = "";
      if (!added.length) return;
      items = items.concat(added);
      draw();
      commit(items.slice());
      input.focus();
    }
    input.addEventListener("keydown", function (e) {
      if (e.key === "Enter" || e.key === ",") { e.preventDefault(); take(); }
      else if (e.key === "Backspace" && input.value === "" && items.length) {
        items.pop();
        draw();
        commit(items.slice());
        input.focus();
      }
    });
    input.addEventListener("blur", take);
    input.addEventListener("paste", function () { setTimeout(function () { if (/[,\n]/.test(input.value)) take(); }, 0); });
    box.addEventListener("click", function (e) { if (e.target === box) input.focus(); });
    draw();
    return { node: box, focus: input };
  }

  // Where the daemon found the agent's binary. `onAgentsPage` words the
  // fixes for the page that can apply them.
  function agentStatus(agent, onAgentsPage) {
    if (!agent || !agent.detected) return null;
    let kind = "ok";
    let text;
    if (agent.path && agent.on_path) text = "Found at `" + agent.path + "`";
    else if (agent.path && !agent.usable) text = "Found at `" + agent.path + "`. Adding it uses that full path.";
    else if (agent.path) {
      kind = "warn";
      text = "Found at `" + agent.path + "`, which is not on the daemon's PATH. " +
        (onAgentsPage ? "Customize it and set that full path as the command." : "Set that full path as its command on the Engines page.");
    } else if (!agent.usable) return el("p", { class: "status-line" }, [el("span", { text: "Not installed on this Mac." })]);
    else { kind = "warn"; text = "`" + agent.command + "` was not found on this Mac. Reviews fail until it is installed."; }
    return el("p", { class: "status-line " + kind }, [icon(kind === "ok" ? "check" : "alert")].concat([el("span", {}, rich(text))]));
  }

  // Whether an LLM API engine's key variable is visible to the daemon.
  function llmStatus(engine) {
    if (!engine || engine.type !== "llm_api" || engine.key_present === null) return null;
    if (engine.key_present) {
      return el("p", { class: "status-line ok" }, [icon("check"), el("span", {}, rich("Key read from `" + engine.api_key_env + "`."))]);
    }
    return el("p", { class: "status-line warn" }, [icon("alert"), el("span", {},
      rich("`" + engine.api_key_env + "` is not set for the daemon. Reviews with this engine fail until it is."))]);
  }

  // The review engine: a usable agent or a configured LLM API engine.
  // Optional fields (the summary engine) offer "same as review" = unset.
  // Each engine with what it is: an agent CLI (and where it was found) or
  // an LLM API (provider and model). Values are engine names; the optional
  // "Same as the review engine" is "" and saves as unset.
  function engineItems(engines, field, value, inherit) {
    const usable = engines.filter(function (a) { return a.usable; });
    const providers = {};
    ((window.prxSchema && window.prxSchema.LLM_PROVIDERS) || []).forEach(function (p) { providers[p.value] = p.label; });
    const items = usable.map(function (a) {
      if (a.type === "llm_api") {
        return { value: a.name, label: a.name, icon: "external",
          detail: ["LLM API", providers[a.provider] || a.provider, a.model].filter(Boolean).join(" · ") };
      }
      const missing = a.detected && !a.path;
      return { value: a.name, label: a.label || a.name, icon: "terminal", tag: missing ? "Not found" : null,
        detail: [a.label && a.label !== a.name ? a.name : null, missing ? "Not found on this Mac" : a.path || "Agent CLI"].filter(Boolean).join(" · ") };
    });
    if (value && !usable.some(function (a) { return a.name === value; })) {
      items.push({ value: value, label: value, icon: "alert", tag: "Not configured", detail: "No engine has this name. Add it on the Engines page." });
    }
    if (field.optional && !inherit) {
      items.unshift({ value: "", label: "Same as the review engine", detail: "No engine of its own." });
      if (items[1]) items[1] = Object.assign({}, items[1], { divider: true });
    }
    return items;
  }

  function engineControl(app, path, field, value, commit, opts) {
    const engines = app.engines();
    const items = engineItems(engines, field, value, opts.inherit);
    const parentText = field.optional && opts.parent && !opts.parent.value ? "Same as the review engine" : null;
    const picker = schemaSelect(fieldId(path), items, field.optional && !value && !opts.inherit ? "" : value, function (next) {
      commit(next === "" && field.optional ? null : next);
    }, { inherit: opts.inherit, inherited: opts.inherited, parent: opts.parent, parentText: parentText, cls: "wide" });
    const current = engines.find(function (a) { return a.name === value; });
    const status = opts.compact || !current ? null : current.type === "llm_api" ? llmStatus(current) : agentStatus(current);
    return { node: status ? el("div", { class: "stack" }, [picker.node, status]) : picker.node, focus: picker.focus };
  }

  // Free text with the agent's known model names as suggestions.
  function modelControl(app, path, field, value, commit, context) {
    const id = fieldId(path);
    const engine = app.engines().find(function (a) { return a.name === context.engine; });
    const agent = engine && engine.type === "agent" ? engine : null;
    const list = el("datalist", { id: id + "-models" });
    ((agent && agent.models) || []).forEach(function (m) { list.append(el("option", { value: m })); });
    const input = el("input", { id: id, type: "text", list: id + "-models", spellcheck: "false", autocomplete: "off", placeholder: "The agent's own default" });
    input.value = value || "";
    input.addEventListener("change", function () { commit(input.value.trim() === "" ? null : input.value.trim()); });
    let note;
    if (engine && engine.type === "llm_api") {
      note = el("p", { class: "status-line warn" }, [icon("alert"), el("span", {},
        rich(engine.name + " is an LLM API engine with its own model (`" + (engine.model || "?") + "`), so this is ignored."))]);
    } else if (agent && agent.model_args.length === 0) {
      note = el("p", { class: "status-line warn" }, [icon("alert"), el("span", { text: (agent.label || agent.name) + " has no model arguments, so this is ignored. Add them on the Engines page." })]);
    } else if (agent) {
      note = el("p", { class: "help" }, rich("Passed as `" + agent.model_args.join(" ").replace("{{MODEL}}", value || "<model>") + "`. Any name the agent accepts works."));
    }
    return { node: el("div", { class: "stack" }, [input, list, note]), focus: input };
  }

  function runsLine(expr) {
    if (expr === null) return null;
    const runs = Cron.nextRuns(expr, new Date(), 3);
    if (!runs.length) return null;
    const day = new Intl.DateTimeFormat(undefined, { weekday: "short" });
    const time = new Intl.DateTimeFormat(undefined, { hour: "numeric", minute: "2-digit" });
    return "Next: " + runs.map(function (d) { return day.format(d) + " " + time.format(d); }).join(" · ");
  }

  // Presets plus a custom cron expression. The page shows every preset as a
  // radio row; the compact override editor uses a select.
  function scheduleControl(app, path, value, commit, opts) {
    const compact = !!opts.compact;
    const id = fieldId(path);
    // An org/repo row with no schedule of its own shows Inherit.
    let inherited = !!(compact && opts.inherit && opts.inherited);
    let preset = Cron.presetFor(value);
    let custom = !preset && !inherited;
    const cron = el("input", { type: "text", id: id + "-cron", class: "cron", spellcheck: "false", autocomplete: "off",
      placeholder: "minute hour day month weekday", "aria-label": "Cron expression" });
    cron.value = custom ? String(value) : "";
    const hint = el("p", { class: "help cron-hint", "aria-live": "polite" });
    function describe() {
      hint.className = "help cron-hint";
      if (!custom) { hint.textContent = (compact && preset && runsLine(preset.value)) || ""; hint.hidden = !hint.textContent; return; }
      const text = cron.value.trim();
      hint.hidden = false;
      if (!text) { hint.textContent = "Five fields: minute, hour, day of month, month, weekday. Example: 30 8 * * 1-5"; return; }
      const parsed = Cron.parse(text);
      if (!parsed.ok) { hint.textContent = parsed.error + ". The daemon has the final say when you save."; hint.classList.add("warn"); return; }
      hint.textContent = [Cron.describe(text), runsLine(text)].filter(Boolean).join(" ") || "Valid cron. Times are local.";
    }
    function commitCustom() {
      const text = cron.value.trim();
      app.localError(path, text ? null : "Enter a cron expression, or pick a preset");
      if (text) commit(text);
      describe();
    }
    cron.addEventListener("input", describe);
    cron.addEventListener("change", commitCustom);

    if (compact) {
      // "Every poll" is no schedule at all, which on an org/repo row is
      // the same as inheriting, so Inherit stands in for it there.
      const items = Cron.PRESETS.map(function (p, i) { return { value: String(i), label: p.label, detail: p.detail }; })
        .filter(function (o) { return !(opts.inherit && o.value === "0"); });
      items.push({ value: "custom", label: "Custom cron\u2026", detail: "Your own cron expression, in local time.", divider: true, always: true });
      const parent = opts.parent || { value: null, source: "defaults" };
      const parentPreset = Cron.presetFor(parent.value);
      const picker = schemaSelect(id, items, inherited ? null : custom ? "custom" : String(Cron.PRESETS.indexOf(preset)), function (next) {
        if (next === null) {
          inherited = true;
          custom = false;
          cron.hidden = true;
          app.localError(path, null);
          commit(null);
          describe();
          return;
        }
        inherited = false;
        custom = next === "custom";
        cron.hidden = !custom;
        if (custom) { cron.focus(); describe(); return; }
        preset = Cron.PRESETS[Number(next)];
        app.localError(path, null);
        commit(preset.value);
        describe();
      }, { inherit: opts.inherit, inherited: inherited, parent: { value: parentPreset ? String(Cron.PRESETS.indexOf(parentPreset)) : parent.value, source: parent.source, org: parent.org },
        parentText: parentPreset ? parentPreset.label : parent.value ? String(parent.value) : null });
      cron.hidden = !custom;
      describe();
      return { node: el("div", { class: "stack" }, [picker.node, cron, hint]), focus: picker.focus };
    }

    // The page (audit P1: nine radios were too many): three choices, with
    // the timed presets in one menu and the next runs spelled out.
    const group = el("div", { class: "choices", role: "radiogroup" });
    const timed = Cron.PRESETS.slice(1);
    const lastTimed = preset && preset.value !== null ? preset : timed[0];
    const presetSelect = select({ id: id + "-preset", label: "Schedule", cls: "wide",
      options: timed.map(function (p, i) { return { value: String(i), label: p.label, detail: p.detail }; }),
      value: String(timed.indexOf(lastTimed)), onchange: function () { pickTimed(); } });
    const timedNext = el("span", { class: "choice-next" });
    function option(index, label, detail, extra) {
      const input = el("input", { type: "radio", name: id, id: id + "-" + index });
      group.append(el("label", { class: "choice", for: id + "-" + index }, [input,
        el("span", { class: "choice-text" }, [
          el("span", { class: "choice-label", text: label }),
          el("span", { class: "choice-detail", text: detail }),
        ].concat(extra || []))]));
      return input;
    }
    const pollInput = option("poll", Cron.PRESETS[0].label, Cron.PRESETS[0].detail);
    const timedInput = option("timed", "On a schedule", "Reviews start only at these times, in local time.",
      [el("span", { class: "stack custom-cron" }, [presetSelect.node, timedNext])]);
    const customInput = option("custom", "Custom", "Your own cron expression, in local time.", [el("span", { class: "stack custom-cron" }, [cron, hint])]);
    const inputs = [pollInput, timedInput, customInput];
    pollInput.checked = !custom && (!preset || preset.value === null);
    timedInput.checked = !custom && !!preset && preset.value !== null;
    customInput.checked = custom;
    function showNext() {
      timedNext.textContent = timedInput.checked ? runsLine(timed[Number(presetSelect.value())].value) || "" : "";
    }
    function pickTimed() {
      custom = false;
      timedInput.checked = true;
      preset = timed[Number(presetSelect.value())];
      app.localError(path, null);
      commit(preset.value);
      describe();
      showNext();
    }
    pollInput.addEventListener("change", function () {
      if (!pollInput.checked) return;
      custom = false;
      preset = Cron.PRESETS[0];
      app.localError(path, null);
      commit(preset.value);
      describe();
      showNext();
    });
    timedInput.addEventListener("change", function () { if (timedInput.checked) pickTimed(); });
    customInput.addEventListener("change", function () {
      if (!customInput.checked) return;
      custom = true;
      cron.focus();
      describe();
      showNext();
    });
    // Typing in the field is choosing Custom.
    cron.addEventListener("focus", function () {
      if (custom) return;
      custom = true;
      customInput.checked = true;
      describe();
      showNext();
    });
    describe();
    showNext();
    return { node: group, focus: inputs.find(function (i) { return i.checked; }) || inputs[0], group: group };
  }

  // Multi-line text with an optional built-in value: placeholder chips
  // insert at the cursor, and "Use built-in" puts the shipped text back.
  function templateControl(app, path, field, value, commit, compact) {
    const isPrompt = field.type === "prompt";
    const shipped = app.shipped(field.key);
    const input = el("textarea", { id: fieldId(path), class: isPrompt ? "prompt" : null, spellcheck: "false",
      rows: isPrompt ? (compact ? 8 : 16) : field.rows || 3, placeholder: field.placeholder || null });
    input.value = isPrompt ? F.promptToForm(value, shipped) : F.templateToForm(value, shipped);
    function parsed() {
      if (isPrompt) return F.promptFromForm(input.value, shipped);
      if (field.optional) return F.templateFromForm(input.value, shipped);
      return input.value;
    }
    const reset = shipped ? button("Use built-in", function () {
      input.value = shipped;
      input.dispatchEvent(new Event("input", { bubbles: true }));
      commit(isPrompt || field.optional ? null : shipped);
      sync();
    }, "link") : null;
    let tools = null;
    function sync() {
      if (!reset) return;
      reset.hidden = input.value.trim() === shipped.trim();
      if (tools && !chips.length) tools.hidden = reset.hidden;
    }
    input.addEventListener("change", function () { commit(parsed()); });
    input.addEventListener("input", sync);
    touchOnInput(app, path, input);
    const chips = (field.placeholders || []).map(function (name) {
      const token = "{{" + name + "}}";
      return el("button", { type: "button", class: "chip", title: "Insert " + token, onclick: function () {
        const start = input.selectionStart;
        input.setRangeText(token, start, input.selectionEnd, "end");
        input.focus();
        input.dispatchEvent(new Event("input", { bubbles: true }));
        commit(parsed());
      }, text: token });
    });
    if (chips.length || reset) {
      tools = el("div", { class: "template-tools" }, [
        chips.length ? el("span", { class: "chips" }, [el("span", { class: "chips-label", text: "Insert" })].concat(chips)) : el("span"),
        reset,
      ]);
    }
    sync();
    const extras = isPrompt ? promptExtras(app, input) : [];
    return { node: el("div", { class: "stack" }, [input, tools].concat(extras)), focus: input };
  }

  // Under the review prompt (design C1/C2): a non-fatal hint when the text
  // still carries its own manifest schema, and a read-only, collapsible
  // preview of the output contract prx always appends. The preview is a
  // <pre> (selectable and copyable, never an input), from GET /config's
  // `review_contract` so it never drifts from what agents receive.
  function promptExtras(app, input) {
    const hint = el("p", { class: "help warn prompt-schema-hint", role: "status",
      text: "This prompt has its own output schema. PRX now adds the output instructions automatically, so you can remove them from the prompt." });
    function syncHint() { hint.hidden = !F.promptHasOwnSchema(input.value); }
    input.addEventListener("input", syncHint);
    syncHint();
    const contract = typeof app.contract === "function" ? app.contract() : "";
    if (!contract) return [hint];
    const preview = el("details", { class: "contract-preview" }, [
      el("summary", { text: "Always added by PRX" }),
      el("p", { class: "help", text: "PRX ends every review prompt with these rules and the manifest schema, after your prompt and extra instructions. They cannot be edited." }),
      el("pre", { class: "contract-text", tabindex: "0", "aria-label": "Output contract always added by PRX (read-only)", text: contract }),
    ]);
    return [hint, preview];
  }

  // "Open PRs with" (spec browser-picker, design W2): the window's dropdown
  // (select above; a native <select> cannot show app icons in WKWebView)
  // with Inherit (org/repo), System default (<name>), the installed
  // browsers with icons, an unmatched saved app as "<value> (not found)",
  // "Other app…" (the native chooser) and "Custom command…" (the
  // one-argument-per-line editor). A Test button opens the project page
  // with the current choice and reports a failure inline; it saves nothing.
  // The saved value only changes when the user picks another option.
  // opts.inherit: offer Inherit; opts.inherited: the row has no own value
  // (`value` is then the inherited one); opts.parent: what Inherit stands
  // for when the row has a value of its own.
  function browserControl(app, path, id, value, commit, opts) {
    opts = opts || {};
    const inherit = !!opts.inherit;
    let catalog = typeof app.browsers === "function" ? app.browsers() : null;
    // The field's own value (null = inherit).
    let own = inherit && opts.inherited ? null : value;
    let options = [];
    let selected = null;

    const cmd = el("textarea", { rows: 3, placeholder: "firefox\n--new-tab\n{{URL}}", spellcheck: "false", "aria-label": "Command, one argument per line" });
    const hint = el("p", { class: "help", text: "One argument per line. {{URL}} is replaced with the PR link and must appear once." });
    const test = button("Test", runTest);
    const testStatus = el("p", { class: "status-line browser-test-status", role: "status", hidden: true });
    const testError = el("p", { class: "field-error browser-test-error", role: "alert", hidden: true });
    const form = F.browserToForm(own, catalog, { inherit: inherit });
    if (form.kind === "command") cmd.value = form.text;
    const picker = select({ id: id, cls: "browser-picker wide", options: [], onchange: function (optionId, o) { return choose(o.browser); } });

    // What Inherit stands for: the inherited browser, or the parent's.
    function inherited() {
      if (!inherit) return "";
      if (opts.inherited) return F.browserLabel(value, catalog);
      return opts.parent ? F.browserLabel(opts.parent.value, catalog) : "";
    }

    function rebuild(optionId) {
      options = F.browserOptions(catalog, own, { inherit: inherit, inheritLabel: inherited() });
      selected = options.find(function (o) { return o.id === optionId; }) || options.find(function (o) {
        return o.id === F.browserToForm(own, catalog, { inherit: inherit }).option;
      }) || options[0];
      const shown = options.map(function (o) {
        const base = { value: o.id, label: o.label, browser: o, icon: o.icon ? { src: "data:image/png;base64," + o.icon } : null,
          divider: o.kind === "other", always: o.kind === "other" || o.kind === "command" };
        if (o.kind === "inherit") {
          const text = inherited();
          return Object.assign(base, { label: "Inherit", display: text || "Inherit", tag: "Inherited", divider: false,
            detail: text ? text + ", from " + F.inheritSource(opts.parent) : null });
        }
        if (o.id === "value" && form.missing) base.tag = "Not found";
        if (o.kind === "other") base.detail = "Choose any app on this Mac.";
        if (o.kind === "command") base.detail = "Run your own command with the PR link.";
        return base;
      });
      if (inherit && shown[1]) shown[1].divider = true;
      picker.setOptions(shown, selected.id);
      sync();
    }

    function sync() {
      const command = selected && selected.kind === "command";
      cmd.hidden = hint.hidden = !command;
      validateCommand();
    }

    function validateCommand() {
      if (typeof app.localError !== "function") return;
      if (!selected || selected.kind !== "command") return app.localError(path, null);
      const argv = F.browserFromForm("command", cmd.value);
      app.localError(path, argv.length && F.urlPlaceholders(argv) !== 1 ? "The command must contain {{URL}} exactly once." : null);
    }

    function clearTest() { testStatus.hidden = true; testError.hidden = true; }

    // Returns false when the picked option is not to show as selected.
    function choose(o) {
      clearTest();
      if (o.kind === "other") { chooseOther(); return false; }
      if (o.kind === "command") {
        rebuild("command");
        const argv = F.browserFromForm("command", cmd.value);
        if (argv.length) { own = argv; commit(argv); }
        cmd.focus();
        return undefined;
      }
      if (o.kind === "inherit") { own = null; rebuild("inherit"); commit(null); return undefined; }
      // An unmatched saved value re-picked: nothing changes.
      if (o.id === "value") { rebuild("value"); return undefined; }
      own = o.value;
      rebuild(o.id);
      commit(o.value);
      return undefined;
    }

    function chooseOther() {
      if (typeof app.chooseBrowserApp !== "function") return;
      app.chooseBrowserApp().then(function (entry) {
        if (!entry) return;
        if (typeof app.addBrowser === "function") app.addBrowser(entry);
        catalog = F.withExtraBrowser(catalog, entry);
        own = F.browserChosenValue(entry, catalog);
        rebuild();
        commit(own);
      }, function (error) {
        testError.textContent = error && error.message ? error.message : String(error);
        testError.hidden = false;
      });
    }

    // The value the Test button opens with: the choice on screen.
    function testValue() {
      if (selected.kind === "command") return F.browserFromForm("command", cmd.value);
      if (selected.kind === "inherit") return opts.inherited ? value : opts.parent ? opts.parent.value || "system" : value;
      return own === null || own === undefined ? "system" : own;
    }

    function runTest() {
      clearTest();
      if (typeof app.testBrowser !== "function") return;
      const browser = testValue();
      if (Array.isArray(browser) && F.urlPlaceholders(browser) !== 1) {
        testError.textContent = "The command must contain {{URL}} exactly once.";
        testError.hidden = false;
        return;
      }
      test.setAttribute("aria-busy", "true");
      app.testBrowser(browser).then(function () {
        test.removeAttribute("aria-busy");
        testStatus.replaceChildren(icon("check"), el("span", { text: "Opened the PRX project page." }));
        testStatus.setAttribute("class", "status-line ok browser-test-status");
        testStatus.hidden = false;
      }, function (error) {
        test.removeAttribute("aria-busy");
        testError.textContent = error && error.message ? error.message : String(error);
        testError.hidden = false;
      });
    }

    cmd.addEventListener("input", validateCommand);
    cmd.addEventListener("change", function () {
      const argv = F.browserFromForm("command", cmd.value);
      own = argv;
      clearTest();
      commit(argv);
      validateCommand();
    });

    rebuild();
    const node = el("div", { class: "stack browser-control" }, [
      el("div", { class: "inline" }, [picker.node, test]), cmd, hint, testStatus, testError,
    ]);
    return { node: node, focus: picker.focus };
  }

  // Build the input for `field` showing `value`; `commit(newValue)` fires on
  // change with the parsed value (null = unset for optional fields).
  // Number parse failures are reported via app.localError and not committed
  // (Save then refuses until they are fixed). opts.compact picks the dense
  // variant used by the override editor; opts.context carries the effective
  // engine for the model field.
  function control(app, path, field, value, commit, opts) {
    opts = opts || {};
    const id = fieldId(path);
    let input;
    switch (field.type) {
      case "number": return numberControl(app, path, field, value, commit);
      case "choice": return choiceControl(path, field, value, commit, opts);
      case "mode": return modeControl(path, field, value, commit, opts);
      case "tier": return segmented(id, field.options, value, commit);
      case "engine": return engineControl(app, path, field, value, commit, opts);
      case "model": return modelControl(app, path, field, value, commit, opts.context || {});
      case "schedule": return scheduleControl(app, path, value, commit, opts);
      case "tokens": return tokensControl(path, field, value, commit);
      case "browser": return browserControl(app, path, id, value, commit, opts);
      case "prompt":
      case "template": return templateControl(app, path, field, value, commit, opts.compact);
      case "bool":
        input = el("input", { id: id, type: "checkbox", role: "switch", class: "switch" });
        input.checked = !!value;
        input.addEventListener("change", function () { commit(input.checked); });
        return { node: input, focus: input };
      default: {
        const area = field.type === "textarea";
        input = el(area ? "textarea" : "input", { id: id, type: area ? null : "text", spellcheck: "false", autocomplete: "off",
          rows: area ? field.rows || 3 : null, placeholder: field.placeholder || null });
        input.value = value === null || value === undefined ? "" : String(value);
        touchOnInput(app, path, input);
        input.addEventListener("change", function () {
          const text = input.value;
          commit(field.optional && text.trim() === "" ? null : text);
        });
        return { node: input, focus: input };
      }
    }
  }

  // A labelled row; registers `path` so errors land in its error slot.
  // opts: help, aside (nodes right of the control), stack, bare, inherited.
  function row(app, path, label, parts, opts) {
    opts = opts || {};
    const error = el("p", { class: "field-error", role: "alert", hidden: true });
    const labelId = fieldId(path) + "-label";
    // A dropdown is named by the label (aria-labelledby), which is a plain
    // span: clicking a row label should not pop a menu open.
    const combo = !!parts.focus && typeof parts.focus.getAttribute === "function" && parts.focus.getAttribute("role") === "combobox";
    const target = !parts.group && !combo && parts.focus && parts.focus.id ? parts.focus.id : null;
    if (parts.group) parts.group.setAttribute("aria-labelledby", labelId);
    if (combo && !parts.focus.getAttribute("aria-label")) parts.focus.setAttribute("aria-labelledby", labelId);
    const node = el("div", { class: "row" + (opts.stack ? " stack-row" : "") + (opts.bare ? " bare-row" : "") + (opts.inherited ? " inherited" : ""), "data-field": path }, [
      el("div", { class: "row-head" }, [
        el(target ? "label" : "span", { class: "row-label", id: labelId, for: target, text: label }),
        opts.help ? el("p", { class: "help" }, rich(opts.help)) : null,
      ]),
      el("div", { class: "row-control" }, [parts.node, error]),
      opts.aside ? el("div", { class: "row-aside" }, opts.aside) : null,
    ]);
    app.register(path, { row: node, error: error, focus: parts.focus });
    return node;
  }

  function section(title, children, note) {
    return el("section", {}, [
      el("h2", { text: title }),
      note ? el("p", { class: "section-note", text: note }) : null,
      el("div", { class: "panel" }, children),
    ]);
  }

  function header(title, note) {
    return el("header", { class: "page-head" }, [el("h1", { text: title }), note ? el("p", { class: "lede", text: note }) : null]);
  }

  // A ⋯ button opening a small action menu (ARIA menu button). items:
  // {label, onclick, disabled} | {heading} | {separator}. ↑/↓ move, Enter or
  // Space runs, Escape closes and returns focus, Tab or a click elsewhere
  // closes.
  function menuButton(label, items, opts) {
    opts = opts || {};
    const id = "menu-" + Math.random().toString(36).slice(2, 9);
    const trigger = el("button", { type: "button", class: "icon-button", "aria-label": label, title: label,
      "aria-haspopup": "menu", "aria-expanded": "false", "aria-controls": id, disabled: !!opts.disabled }, [icon("more")]);
    const menu = el("div", { class: "action-menu", role: "menu", id: id, "aria-label": label });
    menu.hidden = true;
    const entries = [];
    items.forEach(function (item) {
      if (item.separator) { menu.append(el("div", { class: "menu-sep", role: "separator" })); return; }
      if (item.heading) { menu.append(el("div", { class: "menu-label", role: "presentation", text: item.heading })); return; }
      const entry = el("button", { type: "button", role: "menuitem", tabindex: "-1", disabled: !!item.disabled, text: item.label,
        onclick: function () { close(true); item.onclick(); } });
      entries.push(entry);
      menu.append(entry);
    });
    function enabled() { return entries.filter(function (e) { return !e.disabled; }); }
    function move(delta) {
      const list = enabled();
      if (!list.length) return;
      const at = list.indexOf(document.activeElement);
      list[(at + delta + list.length) % list.length].focus();
    }
    function onOutside(event) { if (!anchor.contains(event.target)) close(false); }
    function open() {
      menu.hidden = false;
      trigger.setAttribute("aria-expanded", "true");
      document.addEventListener("mousedown", onOutside, true);
      const first = enabled()[0];
      if (first) first.focus();
    }
    function close(refocus) {
      if (menu.hidden) return;
      menu.hidden = true;
      trigger.setAttribute("aria-expanded", "false");
      document.removeEventListener("mousedown", onOutside, true);
      if (refocus) trigger.focus();
    }
    trigger.addEventListener("click", function () { if (menu.hidden) open(); else close(true); });
    trigger.addEventListener("keydown", function (event) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") { event.preventDefault(); open(); }
    });
    menu.addEventListener("keydown", function (event) {
      if (event.key === "ArrowDown") move(1);
      else if (event.key === "ArrowUp") move(-1);
      else if (event.key === "Home") { const l = enabled(); if (l.length) l[0].focus(); }
      else if (event.key === "End") { const l = enabled(); if (l.length) l[l.length - 1].focus(); }
      else if (event.key === "Escape") close(true);
      else if (event.key === "Tab") { close(false); return; }
      else return;
      event.preventDefault();
    });
    const anchor = el("span", { class: "menu-anchor" }, [trigger, menu]);
    return anchor;
  }

  // A notice above a page's content: tone (info | warn | error), an icon,
  // the text (`code` spans allowed; `title` leads in bold), and actions.
  function banner(tone, iconName, title, text, actions) {
    return el("div", { class: "banner " + tone, role: tone === "error" ? "alert" : "status" }, [icon(iconName),
      el("p", { class: "banner-text" }, [title ? el("strong", { text: title + (text ? " " : "") }) : null].concat(text ? rich(text) : [])),
    ].concat(actions || []));
  }

  // A centered message for a page (or pane) with nothing to show yet.
  function empty(iconName, title, text, actions) {
    return el("div", { class: "empty" }, [
      el("span", { class: "empty-icon" }, [icon(iconName)]),
      el("p", { class: "empty-title", text: title }),
      text ? el("p", { class: "empty-text" }, rich(text)) : null,
      actions && actions.length ? el("div", { class: "empty-actions" }, actions) : null,
    ]);
  }

  window.prxForm = Object.freeze({ el: el, button: button, iconButton: iconButton, icon: icon, rich: rich, segmented: segmented,
    select: select, INHERIT: INHERIT,
    control: control, row: row, section: section, header: header, empty: empty, banner: banner, menuButton: menuButton,
    fieldId: fieldId, agentStatus: agentStatus,
    llmStatus: llmStatus });
})();
