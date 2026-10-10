// Messages page: every piece of text prx posts to GitHub on your behalf —
// the heading and sign-off of automated reviews, and the bodies of the
// menu's Block and Approve actions — with a sample review that re-renders
// as the templates are typed.
(function () {
  "use strict";
  const F = window.prxFields;
  const UI = window.prxForm;
  const P = window.prxPages;
  const renderers = window.prxPageRenderers = window.prxPageRenderers || {};

  // Same phrases as review::build_review_body.
  const VERDICTS = [
    { value: "changes", label: "Changes requested", phrase: "changes requested" },
    { value: "approved", label: "Approved", phrase: "approved — no blocking issues" },
    { value: "comment", label: "Comment", phrase: "comment-only review" },
  ];
  const SAMPLE = {
    pr: "acme/api#128",
    changes: {
      summary: "Solid change overall. One query needs a guard before this merges.",
      blocks: ["### Blocking (1)", "- `src/db/query.rs:42` — Unbounded query when `limit` is missing.", "", "### Nits (1)", "- `README.md:10` — Typo in the setup steps."],
    },
    approved: { summary: "Clean, well-tested change. Nothing blocking.", blocks: ["### Nits (1)", "- `README.md:10` — Typo in the setup steps."] },
    comment: { summary: "A few observations, none of them blocking.", blocks: ["### Nits (1)", "- `README.md:10` — Typo in the setup steps."] },
  };

  function inline(text) {
    const nodes = [];
    String(text).split(/(`[^`]+`|\*\*[^*]+\*\*)/).forEach(function (part) {
      if (!part) return;
      if (part[0] === "`" && part.length > 2) nodes.push(UI.el("code", { text: part.slice(1, -1) }));
      else if (part.slice(0, 2) === "**" && part.length > 4) nodes.push(UI.el("strong", { text: part.slice(2, -2) }));
      else nodes.push(document.createTextNode(part));
    });
    return nodes;
  }

  // The slice of Markdown a review body uses: headings, lists, paragraphs.
  function markdown(text) {
    const out = [];
    let list = null;
    String(text).split("\n").forEach(function (line) {
      const heading = /^(#{1,6})\s+(.*)$/.exec(line);
      const item = /^\s*[-*]\s+(.*)$/.exec(line);
      if (item) {
        if (!list) { list = UI.el("ul"); out.push(list); }
        list.append(UI.el("li", {}, inline(item[1])));
        return;
      }
      list = null;
      if (heading) out.push(UI.el("p", { class: "md-h md-h" + Math.min(heading[1].length, 3) }, inline(heading[2])));
      else if (line.trim()) out.push(UI.el("p", {}, inline(line)));
    });
    return out;
  }

  function preview(app) {
    let verdict = app.ui.previewVerdict || "changes";
    const body = UI.el("div", { class: "preview-body" });
    function text(key) {
      const node = document.getElementById(UI.fieldId("defaults." + key));
      const typed = node ? node.value : "";
      return typed.trim() ? typed : app.shipped(key);
    }
    function draw() {
      const choice = VERDICTS.find(function (v) { return v.value === verdict; });
      const sample = SAMPLE[verdict];
      const heading = F.renderTemplate(text("review_heading"), { PR: SAMPLE.pr, VERDICT: choice.phrase });
      body.replaceChildren.apply(body, markdown([heading, "", sample.summary, ""].concat(sample.blocks, ["", text("identity.attribution_footer")]).join("\n")));
    }
    const picker = UI.segmented("preview-verdict", VERDICTS, verdict, function (next) {
      verdict = app.ui.previewVerdict = next;
      draw();
    }, "Sample verdict");
    const node = UI.el("div", { class: "preview" }, [
      UI.el("div", { class: "preview-head" }, [
        UI.el("span", { class: "preview-title", text: "Sample review on " + SAMPLE.pr }),
        picker.node,
      ]),
      body,
    ]);
    return { node: node, draw: draw };
  }

  renderers.messages = function (root, app) {
    const sections = P.fieldSections(app, "messages");
    const auto = sections.find(function (s) { return s.title === "Automated reviews"; });
    const sample = preview(app);
    auto.rows.push(sample.node);
    auto.note = "Wrapped around every review PRX posts for you.";
    const actions = sections.find(function (s) { return s.title === "Menu actions"; });
    if (actions) actions.note = "Posted when you act on a PR from the menu bar or the CLI.";
    const built = P.build(sections);
    root.append.apply(root, [UI.header("Messages", "The words PRX posts to GitHub under your name. " + P.OVERRIDE_NOTE)].concat(built));
    // The templates are in the document now: draw, and redraw as they change.
    sample.draw();
    built[sections.indexOf(auto)].addEventListener("input", function (e) {
      if (e.target instanceof HTMLTextAreaElement) sample.draw();
    });
  };
})();
