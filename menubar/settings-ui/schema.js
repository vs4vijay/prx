// The overridable config fields, mirroring `Defaults` / `PartialOverrides`
// in src/config.rs (paths match Config::effective_sources keys). The
// defaults pages and the org/repo override editors both render from this
// table; a menubar Rust test fails if a config field is missing here.
(function (root) {
  "use strict";

  const MODES = ["review comment", "monitor", "off"];
  // Review mode (README "Review mode"): what each one does, in the three
  // terms that tell them apart — do reviews run, does anything reach
  // GitHub, where do verdicts show. `detail` is the one-line summary the
  // compact dropdown shows; `facts` drive the cards on General.
  const MODE_OPTIONS = [
    { value: "review comment", label: "Review and comment", icon: "message",
      detail: "Reviews PRs and posts each review on GitHub.",
      facts: [
        ["yes", "Reviews run on new PRs and pushes"],
        ["yes", "Posts a comment or requests changes, with inline notes"],
        ["yes", "Verdicts in the menu, History and on GitHub"],
      ] },
    { value: "monitor", label: "Monitor", icon: "review",
      detail: "Reviews PRs but never writes to GitHub.",
      facts: [
        ["yes", "Reviews run on new PRs and pushes"],
        ["no", "Nothing is posted to GitHub"],
        ["yes", "Verdicts in the menu and History only"],
      ] },
    { value: "off", label: "Off", icon: "stop",
      detail: "Stops checking GitHub. No reviews at all.",
      facts: [
        ["no", "No reviews, not even ones you ask for"],
        ["no", "Nothing is posted to GitHub"],
        ["no", "The menu keeps the last-known state"],
      ] },
  ];
  const TIERS = ["urgent", "informational", "silent"];
  const TIER_OPTIONS = [
    { value: "urgent", label: "Urgent" },
    { value: "informational", label: "Info" },
    { value: "silent", label: "Silent" },
  ];
  // [key, label, built-in tier] (config::Notifications::default).
  const EVENTS = [
    ["on_new_pr", "New PR", "informational"],
    ["on_review_started", "Review started", "informational"],
    ["on_review_complete_approved", "Review complete: approved", "informational"],
    ["on_review_complete_blocking", "Review complete: blocking", "urgent"],
    ["on_new_commits", "New commits", "informational"],
    ["on_re_review_triggered", "Re-review triggered", "informational"],
    ["on_merged", "Merged", "silent"],
    ["on_closed", "Closed", "silent"],
    ["on_failure", "Review failed", "urgent"],
    ["on_skipped", "Skipped", "silent"],
  ];
  // Built-in agent engines (config::builtin_engines); `engines:` entries
  // override them.
  const BUILTIN_ENGINES = ["pi", "claude", "codex"];
  const LLM_PROVIDERS = [
    { value: "anthropic", label: "Anthropic" },
    { value: "openai", label: "OpenAI" },
    { value: "openai_compatible", label: "OpenAI-compatible", detail: "A local or self-hosted server with the OpenAI API. Set its Base URL." },
  ];
  const AUTO_OPEN_OPTIONS = [
    { value: "off", label: "Never", detail: "PRs open only when you open them." },
    { value: "approved", label: "Ready to approve", detail: "Open the PR when a review approves it." },
    { value: "blocking", label: "Blocking", detail: "Open the PR when a review requests changes." },
    { value: "both", label: "Both", detail: "Open the PR after either verdict." },
  ];
  // The top-level `menu` section (menu-ordering spec): how the menu bar
  // orders and groups PR rows.
  const MENU_SORT_OPTIONS = [
    { value: "created_desc", label: "Newest first", detail: "Most recently opened PRs at the top." },
    { value: "created_asc", label: "Oldest first", detail: "PRs that have waited longest at the top." },
    { value: "last_review_desc", label: "Last reviewed", detail: "Most recent review first. PRs never reviewed come last." },
    { value: "key", label: "Repo and number", detail: "By repo name, then PR number (#9 before #10)." },
  ];
  const MENU_GROUP_OPTIONS = [
    { value: "lifecycle", label: "Review state", detail: "Needs review, In review, Ready for approval, and so on." },
    { value: "repo", label: "Repository", detail: "One group per repo, in name order." },
    { value: "none", label: "No grouping", detail: "One flat list of every PR." },
  ];
  // TokenUsage fields an `engines:` agent may map with `usage_output`
  // (token-usage spec), each to a dotted JSON path in the agent's output.
  const USAGE_FIELDS = ["input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "cost_usd", "model", "session_id"];
  // The closed placeholder set of comments.template (design D7).
  // EXEC_SUMMARY is kept as the legacy alias of BRIEF (design B5).
  const COMMENT_PLACEHOLDERS = ["VERDICT_LABEL", "SUMMARY", "BRIEF", "EXEC_SUMMARY", "BLOCKING_COUNT", "NIT_COUNT",
    "FINDINGS_BLOCKING", "FINDINGS_NITS", "PR_KEY", "IDENTITY"];

  // type: number | text | textarea | prompt | template | choice | mode |
  //       tier | bool | tokens | engine | model | schedule | browser
  // optional: unset is meaningful (empty input removes the key).
  // global: defaults only; org/repo entries cannot override it.
  // top: the key is a top-level config path (`menu.sort`), not under
  //      `defaults`; always global too. fallback: shown when it is unset.
  // stack: label above a full-width control (long text).
  // bare: on its own page the section title already names it, so the row
  //       label is kept for assistive tech only.
  // placeholders: {{NAME}} chips offered under a template.
  const FIELDS = [
    { key: "review_mode", label: "Review mode", type: "mode", options: MODE_OPTIONS, fallback: "review comment", stack: true, bare: true, page: "general", section: "Review mode" },
    { key: "poll_interval_minutes", label: "Check GitHub every", type: "number", unit: "min", page: "general", section: "Pace",
      help: "How often PRX looks for new PRs and pushes. The menu bar's Check GitHub now runs one at once." },
    { key: "max_concurrent_reviews", label: "Reviews at once", type: "number", page: "general", section: "Pace",
      help: "How many agent runs may overlap." },
    { key: "agent_timeout_seconds", label: "Agent time limit", type: "number", unit: "sec", page: "general", section: "Pace",
      help: "A run over the limit is stopped and retried once." },

    { key: "engine", label: "Review engine", type: "engine", page: "review", section: "Engine" },
    { key: "agent_model", label: "Model", type: "model", optional: true, page: "review", section: "Engine",
      help: "For agent engines. An LLM API engine uses the model set on the Engines page." },
    { key: "review_prompt", label: "Review prompt", type: "prompt", stack: true, page: "review", section: "Prompt",
      placeholders: ["PR_NUMBER", "REPO", "MANIFEST_PATH", "PARENT_PROMPT"],
      help: "How the agent should review. {{PARENT_PROMPT}} splices in the built-in prompt, so you can wrap it instead of replacing it. PRX always adds its output rules and manifest schema at the end." },
    { key: "extra_prompt", label: "Extra instructions", type: "textarea", rows: 4, optional: true, stack: true, page: "review", section: "Prompt",
      placeholder: "e.g. Pay special attention to database migrations.",
      help: "Added after the prompt on every review. Use this for house rules, and leave the prompt itself alone." },
    { key: "diff_max_files", label: "Largest diff, files", type: "number", unit: "files", page: "review", section: "Limits",
      help: "Bigger PRs are skipped, not reviewed." },
    { key: "diff_max_lines", label: "Largest diff, lines", type: "number", unit: "lines", page: "review", section: "Limits" },
    { key: "cooldown.before_first_review_minutes", label: "Wait before the first review", type: "number", unit: "min", page: "review", section: "Cooldowns",
      help: "Counted from when the PR was opened, so early pushes can settle." },
    { key: "cooldown.between_re_reviews_minutes", label: "Wait before a re-review", type: "number", unit: "min", page: "review", section: "Cooldowns",
      help: "Counted from the newest commits." },
    { key: "skip.drafts", label: "Skip draft PRs", type: "bool", fallback: true, page: "review", section: "Skip" },
    { key: "skip.authors", label: "Skip authors", type: "tokens", placeholder: "login", page: "review", section: "Skip",
      help: "PRs opened by these logins, e.g. dependabot[bot]." },
    { key: "skip.labels", label: "Skip labels", type: "tokens", placeholder: "label", page: "review", section: "Skip",
      help: "Case does not matter." },
    { key: "skip.branches", label: "Skip branches", type: "tokens", placeholder: "pattern", page: "review", section: "Skip",
      help: "Glob patterns on the head branch, e.g. release/*." },
    { key: "brief.enabled", label: "Write briefs", type: "bool", fallback: true, page: "review", section: "Brief",
      help: "A brief is 2–5 plain sentences on what a PR does and why, for someone who hasn't opened the diff. "
        + "It comes with the review when the agent writes one, otherwise it's generated separately. Shown in the menu and the PR detail." },
    { key: "brief.engine", label: "Brief engine", type: "engine", optional: true, page: "review", section: "Brief",
      help: "Used when a brief is generated separately. Unset uses the review engine. A small LLM API engine keeps this cheap." },
    { key: "brief.timeout_seconds", label: "Brief time limit", type: "number", unit: "sec", page: "review", section: "Brief",
      help: "A separate brief run that takes longer fails, and the previous brief is kept." },

    { key: "review_heading", label: "Review heading", type: "template", optional: true, rows: 2, stack: true, page: "messages", section: "Automated reviews",
      placeholders: ["PR", "VERDICT"],
      help: "First line of every review PRX posts. Markdown works." },
    { key: "identity.attribution_footer", label: "Sign-off", type: "template", optional: true, rows: 2, stack: true, page: "messages", section: "Automated reviews",
      help: "Last line of every posted review. It tells readers the review was automated." },
    { key: "comments.inline", label: "Inline comments", type: "bool", fallback: true, page: "messages", section: "Automated reviews",
      help: "Post each finding on its line. Off keeps every finding in the review body only." },
    { key: "comments.template", label: "Review body", type: "template", optional: true, rows: 8, stack: true, page: "messages", section: "Automated reviews",
      placeholders: COMMENT_PLACEHOLDERS,
      help: "The whole posted review. Unset posts the built-in body: the heading, the summary, the findings, and the sign-off." },
    { key: "block_message", label: "Block message", type: "template", rows: 3, stack: true, page: "messages", section: "Menu actions",
      help: "Posted as a request-changes review when you choose Block." },
    { key: "approve_message", label: "Approve message", type: "template", optional: true, rows: 3, stack: true, page: "messages", section: "Menu actions",
      placeholder: "No comment. The approval is posted on its own.",
      help: "Posted with the approval when you choose Approve." },

    { key: "schedule", label: "Review schedule", type: "schedule", optional: true, stack: true, bare: true, page: "schedule", section: "Schedule" },
    { key: "notifications.enabled", label: "Deliver notifications", type: "bool", fallback: true, global: true, page: "notifications", section: "Delivery",
      help: "Off silences every banner and sound. History still records each event." },
  ].concat(EVENTS.map(function (e) {
    return { key: "notifications." + e[0], label: e[1], type: "tier", options: TIER_OPTIONS, fallback: e[2], page: "notifications", section: "Events" };
  })).concat([
    { key: "identity.login", label: "GitHub login", type: "text", optional: true, page: "identity", section: "GitHub account",
      placeholder: "The account gh is signed in as",
      help: "Used to recognise PRX's own comments." },
    { key: "identity.credential", label: "Token variable", type: "text", optional: true, page: "identity", section: "GitHub account",
      placeholder: "e.g. GH_TOKEN_WORK",
      help: "Name of an environment variable holding a gh token. Never the token itself." },
    { key: "browser", label: "Open PRs with", type: "browser", page: "identity", section: "Browser" },
    { key: "auto_open", label: "Open PRs automatically", type: "choice", options: AUTO_OPEN_OPTIONS, fallback: "off", page: "identity", section: "Browser",
      help: "After a review finishes with this verdict, PRX opens the PR for you." },

    { key: "menu.sort", label: "Sort PRs by", type: "choice", options: MENU_SORT_OPTIONS, fallback: "created_desc", global: true, top: true,
      page: "menubar", section: "Menu bar", help: "The order of PR rows within each group." },
    { key: "menu.group_by", label: "Group PRs by", type: "choice", options: MENU_GROUP_OPTIONS, fallback: "lifecycle", global: true, top: true,
      page: "menubar", section: "Menu bar" },
  ]);

  // [id, label, icon, group] (design D16): Overview pages watch the daemon,
  // Settings pages edit the shared config draft.
  const PAGE_GROUPS = [["overview", "Overview"], ["settings", "Settings"]];
  const PAGES = [
    ["dashboard", "Dashboard", "gauge", "overview"],
    ["prs", "Pull requests", "pr", "overview"],
    ["history", "History", "clock", "overview"],
    ["usage", "Usage & stats", "chart", "overview"],
    ["general", "General", "sliders", "settings"],
    ["review", "Review", "review", "settings"],
    ["messages", "Messages", "message", "settings"],
    ["schedule", "Schedule", "calendar", "settings"],
    ["notifications", "Notifications", "bell", "settings"],
    ["engines", "Engines", "terminal", "settings"],
    ["repos", "Repos & Orgs", "repo", "settings"],
    ["menubar", "Menu bar", "menu", "settings"],
    ["identity", "Account & Browser", "user", "settings"],
    ["raw", "config.yml", "code", "settings"],
  ];

  // The sidebar: [{id, label, pages: [{id, label, icon}]}] in group order.
  function navGroups() {
    return PAGE_GROUPS.map(function (g) {
      return { id: g[0], label: g[1], pages: PAGES.filter(function (p) { return p[3] === g[0]; })
        .map(function (p) { return { id: p[0], label: p[1], icon: p[2] }; }) };
    });
  }

  function pageGroup(id) {
    const page = PAGES.find(function (p) { return p[0] === id; });
    return page ? page[3] : null;
  }

  // The page a window URL asks for (`#page=<id>`), or null.
  function pageFromHash(hash) {
    const match = /(?:^#|&)page=([A-Za-z_-]+)/.exec(hash || "");
    return match && pageGroup(match[1]) ? match[1] : null;
  }

  function fieldFor(rest) {
    return FIELDS.find(function (f) { return f.key === rest; }) ||
      FIELDS.find(function (f) { return rest.startsWith(f.key + "."); }) ||
      FIELDS.find(function (f) { return f.key.startsWith(rest + "."); }) || null;
  }

  // Which page shows a (normalized) field path: {page, entity?}.
  function pageForField(path, parsePath) {
    const parsed = parsePath(path);
    if (!parsed) return { page: "general" };
    if (parsed.root === "repos") return { page: "repos", entity: parsed.key !== null ? { kind: "repo", name: parsed.key } : null };
    if (parsed.root === "orgs") return { page: "repos", entity: parsed.key !== null ? { kind: "org", name: parsed.key } : null };
    if (parsed.root === "engines") return { page: "engines" };
    if (parsed.root === "notification_tiers") return { page: "notifications" };
    if (parsed.root === "menu") return { page: "menubar" };
    const field = fieldFor(parsed.rest);
    return { page: field ? field.page : "general" };
  }

  // Fields an org or repo entry may override (everything but `global`).
  const OVERRIDE_FIELDS = FIELDS.filter(function (f) { return !f.global; });

  const api = { MODES: MODES, MODE_OPTIONS: MODE_OPTIONS, TIERS: TIERS, EVENTS: EVENTS, BUILTIN_ENGINES: BUILTIN_ENGINES,
    LLM_PROVIDERS: LLM_PROVIDERS, USAGE_FIELDS: USAGE_FIELDS, FIELDS: FIELDS, OVERRIDE_FIELDS: OVERRIDE_FIELDS,
    PAGES: PAGES, PAGE_GROUPS: PAGE_GROUPS, navGroups: navGroups, pageGroup: pageGroup, pageFromHash: pageFromHash,
    fieldFor: fieldFor, pageForField: pageForField };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.prxSchema = Object.freeze(api);
})(this);
