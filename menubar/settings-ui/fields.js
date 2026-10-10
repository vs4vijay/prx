// Pure helpers for the settings form (no DOM): config field paths, the
// defaults <- org <- repo merge used to show effective values and their
// source, override set/reset on partial entries, and form value parsing.
// Loaded as a plain script (window.prxFields) and by the node unit tests
// (menubar/settings-ui-tests/) via module.exports.
(function (root) {
  "use strict";

  const ROOTS = ["defaults", "repos", "orgs", "engines", "notification_tiers", "menu"];
  const PATH_RE = /^([A-Za-z_]+)(?:\[([^\]]*)\])?(?:\.(.+))?$/;

  // "repos[acme/api].cooldown.x" -> {root: "repos", key: "acme/api", rest: "cooldown.x"}
  function parsePath(path) {
    const match = PATH_RE.exec(path || "");
    if (!match) return null;
    return { root: match[1], key: match[2] === undefined ? null : match[2], rest: match[3] || "" };
  }

  function formatPath(rootName, key, rest) {
    let out = rootName;
    if (key !== null && key !== undefined) out += "[" + key + "]";
    if (rest) out += "." + rest;
    return out;
  }

  function entryKey(rootName) { return rootName === "orgs" ? "org" : "name"; }

  // Normalize a daemon error `field` to the path the form registers inputs
  // under: unrooted paths (`engine`, `skip.branches`) are defaults-level,
  // and serde's index paths (`repos[0]`) become name paths (`repos[a/b]`).
  // `message` refines the one unrooted daemon field that is ambiguous
  // across levels: "unknown engine 'x'" points at whichever entry uses x.
  function normalizeErrorField(field, draft, message) {
    if (!field) return null;
    const parsed = parsePath(field);
    if (!parsed) return null;
    const unknown = field === "engine" && /unknown engine '([^']*)'/.exec(message || "");
    if (unknown && draft && !(draft.defaults && draft.defaults.engine === unknown[1])) {
      const org = (draft.orgs || []).find(function (o) { return o.engine === unknown[1]; });
      const repo = (draft.repos || []).find(function (r) { return r.engine === unknown[1]; });
      if (repo) return "repos[" + repo.name + "].engine";
      if (org) return "orgs[" + org.org + "].engine";
    }
    if (ROOTS.indexOf(parsed.root) === -1) return "defaults." + field;
    if ((parsed.root === "repos" || parsed.root === "orgs") && parsed.key !== null && /^\d+$/.test(parsed.key)) {
      const list = (draft && draft[parsed.root]) || [];
      const named = list.some(function (e) { return e[entryKey(parsed.root)] === parsed.key; });
      const entry = list[Number(parsed.key)];
      if (!named && entry) return formatPath(parsed.root, entry[entryKey(parsed.root)], parsed.rest);
    }
    return field;
  }

  // Drop the last segment: "a[b].c.d" -> "a[b].c" -> "a[b]" -> "a" -> null.
  function parentPath(path) {
    const parsed = parsePath(path);
    if (!parsed) return null;
    if (parsed.rest) {
      const cut = parsed.rest.lastIndexOf(".");
      return formatPath(parsed.root, parsed.key, cut === -1 ? "" : parsed.rest.slice(0, cut));
    }
    if (parsed.key !== null) return parsed.root;
    return null;
  }

  // The registered input path that best shows an error on `field`: exact,
  // else the first input under it, else the nearest registered ancestor.
  function matchInput(field, available) {
    for (let path = field; path; path = parentPath(path)) {
      if (available.indexOf(path) !== -1) return path;
      const child = available.find(function (p) { return p.startsWith(path + ".") || p.startsWith(path + "["); });
      if (child) return child;
    }
    return null;
  }

  function getAt(obj, path) {
    return path.split(".").reduce(function (node, part) {
      return node === null || node === undefined ? undefined : node[part];
    }, obj);
  }

  function isSet(obj, path) {
    const value = getAt(obj, path);
    return value !== undefined && value !== null;
  }

  function setAt(obj, path, value) {
    const parts = path.split(".");
    let node = obj;
    parts.slice(0, -1).forEach(function (part) {
      if (node[part] === null || typeof node[part] !== "object" || Array.isArray(node[part])) node[part] = {};
      node = node[part];
    });
    node[parts[parts.length - 1]] = value;
  }

  // Remove an override; partial parents left with nothing set are removed
  // too, so a reset never leaves `cooldown: {}` behind.
  function unsetAt(obj, path) {
    const parts = path.split(".");
    const chain = [obj];
    for (let i = 0; i < parts.length - 1; i++) {
      const next = chain[i][parts[i]];
      if (next === null || typeof next !== "object") return;
      chain.push(next);
    }
    delete chain[chain.length - 1][parts[parts.length - 1]];
    for (let i = chain.length - 1; i > 0; i--) {
      const empty = Object.keys(chain[i]).every(function (k) { return chain[i][k] === null || chain[i][k] === undefined; });
      if (!empty) break;
      delete chain[i - 1][parts[i - 1]];
    }
  }

  function orgOf(repoName) {
    const cut = (repoName || "").indexOf("/");
    return cut > 0 ? repoName.slice(0, cut) : null;
  }

  function findEntry(draft, kind, name) {
    const list = (kind === "org" ? draft.orgs : draft.repos) || [];
    return list.find(function (e) { return (kind === "org" ? e.org : e.name) === name; }) || null;
  }

  // Effective value of `path` for a repo or org entry and where it comes
  // from — the same defaults <- org <- repo rule as Config::effective_repo.
  function resolveEffective(draft, kind, name, path) {
    const own = findEntry(draft, kind, name);
    if (own && isSet(own, path)) return { value: getAt(own, path), source: kind, org: kind === "org" ? name : null };
    if (kind === "repo") {
      const orgName = orgOf(name);
      const org = orgName && findEntry(draft, "org", orgName);
      if (org && isSet(org, path)) return { value: getAt(org, path), source: "org", org: orgName };
    }
    const value = getAt(draft.defaults || {}, path);
    return { value: value === undefined ? null : value, source: "defaults", org: null };
  }

  // What `path` would be for the entry if it set nothing itself: the value
  // its Inherit option stands for (repo -> org -> defaults; org -> defaults).
  function resolveParent(draft, kind, name, path) {
    if (kind === "repo") {
      const orgName = orgOf(name);
      const org = orgName && findEntry(draft, "org", orgName);
      if (org && isSet(org, path)) return { value: getAt(org, path), source: "org", org: orgName };
    }
    const value = getAt(draft.defaults || {}, path);
    return { value: value === undefined ? null : value, source: "defaults", org: null };
  }

  // Where an inherited value comes from, as a phrase: "the defaults" or
  // "the acme org".
  function inheritSource(effective) {
    return effective && effective.source === "org" ? "the " + effective.org + " org" : "the defaults";
  }

  // The orgs and repos that set `path` themselves, orgs first, each with
  // its value: [{kind, name, value}] (the General page's review-mode scope).
  function ownSettings(draft, path) {
    const out = [];
    (draft.orgs || []).forEach(function (o) { if (isSet(o, path)) out.push({ kind: "org", name: o.org, value: getAt(o, path) }); });
    (draft.repos || []).forEach(function (r) { if (isSet(r, path)) out.push({ kind: "repo", name: r.name, value: getAt(r, path) }); });
    return out;
  }

  function sourceLabel(effective, kind) {
    if (effective.source === "defaults") return "Default";
    if (effective.source === "org" && kind === "repo") return "Org " + effective.org;
    return kind === "org" ? "This org" : "This repo";
  }

  function orgsOfRepos(repos) {
    const seen = [];
    (repos || []).forEach(function (r) {
      const org = orgOf(r.name);
      if (org && seen.indexOf(org) === -1) seen.push(org);
    });
    return seen.sort();
  }

  function creatableOrgs(draft) {
    const existing = (draft.orgs || []).map(function (o) { return o.org; });
    return orgsOfRepos(draft.repos).filter(function (o) { return existing.indexOf(o) === -1; });
  }

  function splitList(text) {
    return String(text || "").split(/[,\n]/).map(function (s) { return s.trim(); }).filter(Boolean);
  }

  function splitLines(text) {
    return String(text || "").split("\n").map(function (s) { return s.replace(/\s+$/, ""); }).filter(Boolean);
  }

  // Whole-number input; `min` is enforced so obvious mistakes never leave
  // the form, everything else is the daemon's validation.
  function parseNumber(text, min) {
    const trimmed = String(text === undefined || text === null ? "" : text).trim();
    if (!/^\d+$/.test(trimmed)) return { ok: false, error: "Enter a whole number" };
    const value = Number(trimmed);
    if (min !== undefined && value < min) return { ok: false, error: "Must be at least " + min };
    return { ok: true, value: value };
  }

  // `browser` is "system", an app name/path, or an argv list with {{URL}}.
  //
  // The picker (spec browser-picker, design W1/W2) works from a catalog of
  // installed browsers from the bridge's `browsers.list`:
  //   { browsers: [entry…], extra: [entry…] }, entry =
  //   { name, bundle_id, path, icon_png_base64, is_default }
  // `browsers` are the discovered https handlers (default first), `extra`
  // the apps the config names by path (or picked with "Other app…") that
  // are not among them. `null` while discovery is pending or unavailable.
  //
  // browserToForm(value, catalog, opts) -> { kind, text, option, entry,
  // missing }: kind is inherit | system | app | command, option the id of
  // the dropdown option to show selected (see browserOptions). opts.inherit
  // (org/repo levels) maps an unset value to "inherit". An app value that
  // matches no catalog entry is kept as-is under the "value" option and
  // flagged `missing` once the catalog is known ("<value> (not found)").
  function browserToForm(value, catalog, opts) {
    opts = opts || {};
    if (opts.inherit && (value === null || value === undefined)) {
      return { kind: "inherit", text: "", option: "inherit", entry: null, missing: false };
    }
    if (Array.isArray(value)) return { kind: "command", text: value.join("\n"), option: "command", entry: null, missing: false };
    if (!value || String(value).trim().toLowerCase() === "system") {
      return { kind: "system", text: "", option: "system", entry: null, missing: false };
    }
    const text = String(value);
    const entry = findBrowser(text, catalogEntries(catalog));
    if (entry) return { kind: "app", text: text, option: "app:" + entry.path, entry: entry, missing: false };
    return { kind: "app", text: text, option: "value", entry: null, missing: !!catalog };
  }

  function catalogEntries(catalog) {
    if (!catalog) return [];
    return (catalog.browsers || []).concat(catalog.extra || []);
  }

  function trimPath(path) { return String(path || "").replace(/\/+$/, ""); }

  function appKey(name) { return String(name || "").trim().replace(/\.app$/i, "").toLowerCase(); }

  // The catalog entry a saved app value names: its path, or (for a value
  // without a slash) its name, case-insensitively, `.app` optional.
  function findBrowser(value, entries) {
    const text = String(value || "").trim();
    if (!text) return null;
    if (text.indexOf("/") !== -1) {
      return entries.find(function (e) { return trimPath(e.path) === trimPath(text); }) || null;
    }
    return entries.find(function (e) { return appKey(e.name) === appKey(text); }) || null;
  }

  // The value saved for an entry (design W1): its name when that name is
  // unique among the discovered browsers (portable, `open -a` resolves it),
  // otherwise its path.
  function browserValue(entry, browsers) {
    const same = (browsers || []).filter(function (e) { return appKey(e.name) === appKey(entry.name); });
    const listed = same.some(function (e) { return trimPath(e.path) === trimPath(entry.path); });
    return listed && same.length === 1 ? entry.name : trimPath(entry.path);
  }

  // The value saved for an app picked with "Other app…": a discovered
  // browser (same path or bundle) saves like picking it from the list;
  // anything else saves its path (it is not an https handler, so a name
  // could not be matched back to it).
  function browserChosenValue(entry, catalog) {
    const browsers = (catalog && catalog.browsers) || [];
    const match = browsers.find(function (e) { return trimPath(e.path) === trimPath(entry.path); })
      || (entry.bundle_id ? browsers.find(function (e) { return e.bundle_id === entry.bundle_id; }) : null);
    return match ? browserValue(match, browsers) : trimPath(entry.path);
  }

  // The catalog with an app picked through "Other app…" added to `extra`
  // (unless it is already listed).
  function withExtraBrowser(catalog, entry) {
    const base = catalog || { browsers: [], extra: [] };
    if (catalogEntries(base).some(function (e) { return trimPath(e.path) === trimPath(entry.path); })) return base;
    return { browsers: base.browsers || [], extra: (base.extra || []).concat([entry]) };
  }

  // A short description of a browser value (the Inherit option's hint).
  function browserLabel(value, catalog) {
    const form = browserToForm(value, catalog);
    if (form.kind === "system") {
      const def = catalog && (catalog.browsers || []).find(function (e) { return e.is_default; });
      return "System default" + (def ? " (" + def.name + ")" : "");
    }
    if (form.kind === "command") return "Custom command";
    return form.entry ? form.entry.name : form.text;
  }

  // The dropdown's options for `value`, in order:
  //   Inherit (org/repo) · System default (<name>) · every catalog entry ·
  //   "<value> (not found)" (an unmatched app value) · Other app… ·
  //   Custom command…
  // Each is { id, kind, label, value, icon }; `value` is what picking it
  // saves (undefined for "inherit"/"other"/"command", which act instead).
  // opts: inherit, inheritLabel (e.g. the inherited browser).
  function browserOptions(catalog, value, opts) {
    opts = opts || {};
    const form = browserToForm(value, catalog, opts);
    const browsers = (catalog && catalog.browsers) || [];
    const def = browsers.find(function (e) { return e.is_default; });
    const options = [];
    if (opts.inherit) {
      options.push({ id: "inherit", kind: "inherit", label: "Inherit" + (opts.inheritLabel ? " (" + opts.inheritLabel + ")" : ""), value: undefined, icon: null });
    }
    options.push({ id: "system", kind: "system", label: "System default" + (def ? " (" + def.name + ")" : ""), value: "system", icon: null });
    browsers.forEach(function (e) {
      options.push({ id: "app:" + e.path, kind: "app", label: e.name, value: browserValue(e, browsers), icon: e.icon_png_base64 || null });
    });
    ((catalog && catalog.extra) || []).forEach(function (e) {
      if (options.some(function (o) { return o.id === "app:" + e.path; })) return;
      options.push({ id: "app:" + e.path, kind: "app", label: e.name, value: trimPath(e.path), icon: e.icon_png_base64 || null });
    });
    if (form.option === "value") {
      options.push({ id: "value", kind: "app", label: form.text + (form.missing ? " (not found)" : ""), value: form.text, icon: null });
    }
    options.push({ id: "other", kind: "other", label: "Other app\u2026", value: undefined, icon: null });
    options.push({ id: "command", kind: "command", label: "Custom command\u2026", value: undefined, icon: null });
    return options;
  }

  // How many times a command template uses {{URL}} (it must be exactly 1).
  function urlPlaceholders(argv) {
    return (argv || []).reduce(function (n, arg) { return n + String(arg).split("{{URL}}").length - 1; }, 0);
  }

  // App paths a config names as `browser` (defaults, orgs, repos), so the
  // bridge can resolve apps that are not https handlers.
  function browserPaths(config) {
    const out = [];
    function add(value) { if (typeof value === "string" && value.indexOf("/") !== -1 && out.indexOf(value) === -1) out.push(value); }
    if (!config) return out;
    add(config.defaults && config.defaults.browser);
    (config.orgs || []).forEach(function (o) { add(o.browser); });
    (config.repos || []).forEach(function (r) { add(r.browser); });
    return out;
  }

  function browserFromForm(kind, text) {
    if (kind === "command") return splitLines(text);
    if (kind === "app") return String(text || "").trim();
    return "system";
  }

  // `review_prompt` (design F5) is prompt text; `default`/unset shows the
  // shipped prompt so it can be edited from there.
  function isDefaultPrompt(value) {
    return typeof value === "string" && value.trim().toLowerCase() === "default";
  }

  function promptToForm(value, shipped) {
    if (value === null || value === undefined || isDefaultPrompt(value)) return shipped || "";
    return String(value);
  }

  // Empty -> null (unset: inherit); unchanged shipped text -> "default";
  // anything else is the prompt text as typed.
  function promptFromForm(text, shipped) {
    const value = String(text === undefined || text === null ? "" : text);
    if (value.trim() === "") return null;
    if (shipped && value.replace(/\s+$/, "") === shipped.replace(/\s+$/, "")) return "default";
    return value;
  }

  // Design C1: prx appends its output contract (rules, manifest path,
  // schema) to every prompt, so a custom prompt that still carries its own
  // manifest schema duplicates it. Same heuristic as `prx doctor`.
  function promptHasOwnSchema(text) {
    const value = String(text === undefined || text === null ? "" : text);
    return value.indexOf("manifest_version") !== -1 || value.indexOf('"verdict"') !== -1;
  }

  // Message templates with a built-in text (review heading, attribution
  // footer): unset shows the built-in, and typing the built-in back (or
  // clearing the field) unsets it again.
  function templateToForm(value, shipped) {
    return value === null || value === undefined ? shipped || "" : String(value);
  }

  function templateFromForm(text, shipped) {
    const value = String(text === undefined || text === null ? "" : text);
    if (value.trim() === "") return null;
    if (shipped && value.trim() === shipped.trim()) return null;
    return value;
  }

  // Fill {{NAME}} placeholders from `vars`; unknown ones are left as typed.
  function renderTemplate(text, vars) {
    return String(text || "").replace(/\{\{([A-Z_]+)\}\}/g, function (whole, name) {
      return Object.prototype.hasOwnProperty.call(vars, name) ? vars[name] : whole;
    });
  }

  function validRepoName(name) {
    return /^[^\s/]+\/[^\s/]+$/.test(name || "");
  }

  // The repo picker's filter. Plain text matches every space-separated term
  // anywhere in the name; `regex` (or a /pattern/ query) is a
  // case-insensitive regular expression over the whole `owner/repo`.
  function repoMatcher(query, regex) {
    let text = String(query || "").trim();
    const slashed = /^\/(.+)\/$/.exec(text);
    if (slashed) { text = slashed[1]; regex = true; }
    if (!text) return { ok: true, regex: !!regex, empty: true, test: function () { return true; } };
    if (regex) {
      let re;
      try { re = new RegExp(text, "i"); } catch (error) {
        return { ok: false, regex: true, error: String(error.message || error).replace(/^Invalid regular expression: /, "") };
      }
      return { ok: true, regex: true, empty: false, test: function (name) { return re.test(name); } };
    }
    const terms = text.toLowerCase().split(/\s+/);
    return {
      ok: true, regex: false, empty: false,
      test: function (name) {
        const lower = name.toLowerCase();
        return terms.every(function (t) { return lower.indexOf(t) !== -1; });
      },
    };
  }

  // The watched list as a tree: one group per org (in first-seen order,
  // org entries without watched repos last), each with its org entry (or
  // null) and its repos.
  function groupRepos(draft) {
    const groups = [];
    const byOrg = {};
    function group(org) {
      if (!byOrg[org]) {
        byOrg[org] = { org: org, entry: findEntry(draft, "org", org), repos: [] };
        groups.push(byOrg[org]);
      }
      return byOrg[org];
    }
    (draft.repos || []).forEach(function (repo) { group(orgOf(repo.name) || repo.name).repos.push(repo); });
    (draft.orgs || []).forEach(function (org) { group(org.org); });
    return groups;
  }

  // An agent's command line as it will be spawned (for display).
  function commandLine(agent, model) {
    const args = (agent.args || []).filter(function (a) { return a !== "-"; });
    const extra = model ? (agent.model_args || []).map(function (a) { return a.replace("{{MODEL}}", model); }) : [];
    const stdin = (agent.args || []).indexOf("-") !== -1;
    return [agent.command || "?"].concat(args, extra).join(" ") + (stdin ? "  (prompt on stdin)" : "");
  }

  // An `engines:` entry's type (design D1); entries without one are agents.
  function engineType(entry) {
    return entry && entry.type === "llm_api" ? "llm_api" : "agent";
  }

  // The agent-type entries of an `engines:` map.
  function agentEntries(engines) {
    const out = {};
    Object.keys(engines || {}).forEach(function (name) {
      if (engineType(engines[name]) === "agent") out[name] = engines[name];
    });
    return out;
  }

  // The `llm_api` entries of an `engines:` map, with whether the daemon
  // sees their key variable (from discovery; null when unknown).
  function llmEngines(engines, discovered) {
    const seen = {};
    ((discovered && discovered.llm_apis) || []).forEach(function (l) { seen[l.name] = l; });
    return Object.keys(engines || {}).filter(function (name) { return engineType(engines[name]) === "llm_api"; })
      .map(function (name) {
        const own = engines[name];
        const found = seen[name];
        return {
          name: name, label: name, type: "llm_api", usable: true, configured: true,
          provider: own.provider || "", model: own.model || "", api_key_env: own.api_key_env || "",
          key_present: found && found.api_key_env === own.api_key_env ? !!found.key_present : null,
        };
      });
  }

  // Every engine the form can offer: agents (see agentCatalog) then the
  // configured LLM API engines.
  function engineCatalog(discovered, engines, builtins) {
    return agentCatalog(discovered, agentEntries(engines), builtins)
      .map(function (a) { return Object.assign({ type: "agent" }, a); })
      .concat(llmEngines(engines, discovered));
  }

  // Every agent the form can offer: the detected catalog (built-ins and
  // presets), with agent-type `engines:` entries from the draft layered on
  // top — an entry overrides the catalog agent of the same name, others are
  // custom. `builtins` is the offline fallback for the catalog.
  function agentCatalog(discovered, configured, builtins) {
    const catalog = discovered && discovered.agents && discovered.agents.length ? discovered.agents
      : (builtins || []).map(function (name) { return { name: name, label: name, command: name, args: [], model_args: [], models: [], builtin: true }; });
    const resolved = (discovered && discovered.configured) || {};
    const scanned = !!(discovered && discovered.agents && discovered.agents.length);
    const out = catalog.map(function (known) {
      const own = configured && configured[known.name];
      // An entry that keeps the catalog command (or pins the path it was
      // found at) is located by the scan; any other command only by the
      // daemon's lookup of the saved config.
      const same = !own || own.command === known.command;
      const pinned = !!own && !!known.path && own.command === known.path;
      const looked = Object.prototype.hasOwnProperty.call(resolved, known.name);
      return {
        name: known.name, label: known.label || known.name, builtin: !!known.builtin, known: true,
        configured: !!own, usable: !!known.builtin || !!own,
        command: own ? own.command : known.command,
        args: own ? own.args || [] : known.args || [],
        model_args: own ? own.model_args || [] : known.model_args || [],
        models: known.models || [], preset: known,
        path: same || pinned ? known.path || null : resolved[known.name] || null,
        on_path: pinned ? true : same ? !!known.on_path : !!resolved[known.name],
        detected: scanned && (same || pinned || looked),
      };
    });
    Object.keys(configured || {}).forEach(function (name) {
      if (out.some(function (a) { return a.name === name; })) return;
      const own = configured[name];
      out.push({
        name: name, label: name, builtin: false, known: false, configured: true, usable: true,
        command: own.command, args: own.args || [], model_args: own.model_args || [], models: [], preset: null,
        path: resolved[name] || null, on_path: !!resolved[name],
        detected: Object.prototype.hasOwnProperty.call(resolved, name),
      });
    });
    return out;
  }

  // Comment lines a structured save would drop (raw saves keep them).
  function hasYamlComments(yaml) {
    return /(^|\s)#/m.test(yaml || "");
  }

  function clone(value) { return value === undefined ? undefined : JSON.parse(JSON.stringify(value)); }

  // An agent's `usage_output` map (token-usage spec) edited as text, one
  // `field = dotted.path` per line, in the given field order.
  function usageOutputToText(map, fields) {
    if (!map) return "";
    const keys = Object.keys(map);
    keys.sort(function (a, b) {
      const ia = fields.indexOf(a), ib = fields.indexOf(b);
      return (ia === -1 ? fields.length : ia) - (ib === -1 ? fields.length : ib) || (a < b ? -1 : a > b ? 1 : 0);
    });
    return keys.map(function (k) { return k + " = " + map[k]; }).join("\n");
  }

  // Parse that text back: {ok, value} (null value = no mapping, so the key
  // is removed) or {ok: false, error} naming the first bad line.
  function parseUsageOutput(text, fields) {
    const value = {};
    const lines = String(text || "").split("\n").map(function (l) { return l.trim(); }).filter(Boolean);
    for (const line of lines) {
      const match = /^([A-Za-z_]+)\s*[=:]\s*(\S+)$/.exec(line);
      if (!match) return { ok: false, error: "Write each line as `field = json.path`, e.g. `input_tokens = usage.input_tokens`." };
      if (fields.indexOf(match[1]) === -1) return { ok: false, error: "Unknown usage field " + match[1] + ". Use one of: " + fields.join(", ") + "." };
      if (!/^[^.\s]+(\.[^.\s]+)*$/.test(match[2])) return { ok: false, error: "Invalid JSON path " + match[2] + " for " + match[1] + "." };
      if (value[match[1]] !== undefined) return { ok: false, error: match[1] + " is mapped twice." };
      value[match[1]] = match[2];
    }
    return { ok: true, value: Object.keys(value).length ? value : null };
  }

  const api = {
    usageOutputToText: usageOutputToText, parseUsageOutput: parseUsageOutput,
    parsePath: parsePath, formatPath: formatPath, normalizeErrorField: normalizeErrorField,
    parentPath: parentPath, matchInput: matchInput, getAt: getAt, isSet: isSet, setAt: setAt,
    unsetAt: unsetAt, orgOf: orgOf, findEntry: findEntry, resolveEffective: resolveEffective,
    resolveParent: resolveParent, inheritSource: inheritSource, ownSettings: ownSettings,
    sourceLabel: sourceLabel, orgsOfRepos: orgsOfRepos, creatableOrgs: creatableOrgs,
    splitList: splitList, splitLines: splitLines, parseNumber: parseNumber,
    browserToForm: browserToForm, browserFromForm: browserFromForm, browserOptions: browserOptions,
    browserValue: browserValue, browserChosenValue: browserChosenValue, browserLabel: browserLabel,
    findBrowser: findBrowser, withExtraBrowser: withExtraBrowser, urlPlaceholders: urlPlaceholders,
    browserPaths: browserPaths,
    hasYamlComments: hasYamlComments, clone: clone,
    isDefaultPrompt: isDefaultPrompt, promptToForm: promptToForm, promptFromForm: promptFromForm,
    promptHasOwnSchema: promptHasOwnSchema,
    templateToForm: templateToForm, templateFromForm: templateFromForm, renderTemplate: renderTemplate,
    validRepoName: validRepoName, repoMatcher: repoMatcher, groupRepos: groupRepos,
    commandLine: commandLine, agentCatalog: agentCatalog, engineType: engineType,
    agentEntries: agentEntries, llmEngines: llmEngines, engineCatalog: engineCatalog,
  };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.prxFields = Object.freeze(api);
})(this);
