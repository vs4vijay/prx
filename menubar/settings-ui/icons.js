// The settings app's icon set: 24-unit outlines drawn at 16px with one
// stroke weight, built as DOM nodes (the page's CSP allows no inline markup
// sources and no network).
(function () {
  "use strict";
  const NS = "http://www.w3.org/2000/svg";

  // Each icon is a list of [tag, attributes].
  const ICONS = {
    sliders: [["path", { d: "M4 6h9M17 6h3M4 12h3M11 12h9M4 18h11M19 18h1" }],
      ["circle", { cx: 15, cy: 6, r: 2 }], ["circle", { cx: 9, cy: 12, r: 2 }], ["circle", { cx: 17, cy: 18, r: 2 }]],
    review: [["path", { d: "M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12Z" }], ["circle", { cx: 12, cy: 12, r: 3 }]],
    message: [["path", { d: "M4 5h16v11H9.5L5 19.5V16H4Z" }], ["path", { d: "M8 9h8M8 12.5h5" }]],
    clock: [["circle", { cx: 12, cy: 12, r: 8.5 }], ["path", { d: "M12 7.5V12l3 2" }]],
    bell: [["path", { d: "M6 16.5V11a6 6 0 0 1 12 0v5.5l1.5 1.5h-15Z" }], ["path", { d: "M10 20.5a2 2 0 0 0 4 0" }]],
    terminal: [["rect", { x: 3, y: 4.5, width: 18, height: 15, rx: 2.5 }], ["path", { d: "M7.5 10l3 2.5-3 2.5M13 15h4" }]],
    repo: [["path", { d: "M6 3.5h12.5v14H7a2 2 0 0 0-2 2V5.5a2 2 0 0 1 1-2Z" }], ["path", { d: "M5 19.5a2 2 0 0 0 2 2h11.5v-4" }], ["path", { d: "M9.5 7.5h5" }]],
    user: [["circle", { cx: 12, cy: 8.5, r: 3.5 }], ["path", { d: "M5 20a7 7 0 0 1 14 0" }]],
    code: [["path", { d: "M8.5 7.5 4 12l4.5 4.5M15.5 7.5 20 12l-4.5 4.5M13.5 5l-3 14" }]],
    search: [["circle", { cx: 11, cy: 11, r: 6.5 }], ["path", { d: "m16 16 4.5 4.5" }]],
    close: [["path", { d: "M6.5 6.5l11 11M17.5 6.5l-11 11" }]],
    plus: [["path", { d: "M12 5.5v13M5.5 12h13" }]],
    check: [["path", { d: "m5 12.5 4.5 4.5L19 7.5" }]],
    chevron: [["path", { d: "m7 10 5 5 5-5" }]],
    alert: [["path", { d: "M12 4 21 19.5H3Z" }], ["path", { d: "M12 10v4.5M12 17.2v.3" }]],
    refresh: [["path", { d: "M19.5 12a7.5 7.5 0 1 1-2.2-5.3" }], ["path", { d: "M19.5 4.5v4.5H15" }]],
    lock: [["rect", { x: 5.5, y: 10.5, width: 13, height: 9.5, rx: 2 }], ["path", { d: "M8.5 10.5V8a3.5 3.5 0 0 1 7 0v2.5" }]],
    offline: [["circle", { cx: 12, cy: 12, r: 8.5 }], ["path", { d: "M6 6l12 12" }]],
    gauge: [["path", { d: "M4.5 17a8 8 0 1 1 15 0" }], ["path", { d: "M12 13.5 15.5 9" }], ["circle", { cx: 12, cy: 14, r: 1.2 }]],
    pr: [["circle", { cx: 6.5, cy: 5.5, r: 2 }], ["circle", { cx: 6.5, cy: 18.5, r: 2 }], ["circle", { cx: 17.5, cy: 18.5, r: 2 }],
      ["path", { d: "M6.5 7.5v9M17.5 16.5V9.5a2.5 2.5 0 0 0-2.5-2.5h-4M12.5 4.5 10.5 7l2 2.5" }]],
    chart: [["path", { d: "M4 20h16" }], ["path", { d: "M7 16.5V11M12 16.5V6.5M17 16.5v-3.5" }]],
    calendar: [["rect", { x: 4, y: 5.5, width: 16, height: 14, rx: 2.5 }], ["path", { d: "M4 10h16M8.5 3.5v4M15.5 3.5v4" }]],
    menu: [["rect", { x: 3.5, y: 4.5, width: 17, height: 15, rx: 2.5 }], ["path", { d: "M3.5 8.5h17M7.5 12.5h9M7.5 15.5h6" }]],
    external: [["path", { d: "M13.5 4.5h6v6M19.5 4.5 11 13" }], ["path", { d: "M17.5 14v4a1.5 1.5 0 0 1-1.5 1.5H6A1.5 1.5 0 0 1 4.5 18V8A1.5 1.5 0 0 1 6 6.5h4" }]],
    more: [["path", { d: "M6.5 12h.01M12 12h.01M17.5 12h.01", "stroke-width": 2.6 }]],
    pause: [["path", { d: "M9 6.5v11M15 6.5v11" }]],
    // A pop-up button's double chevron (custom select, form.js).
    updown: [["path", { d: "m8 9.5 4-4 4 4M8 14.5l4 4 4-4" }]],
    stop: [["circle", { cx: 12, cy: 12, r: 8.5 }], ["rect", { x: 9, y: 9, width: 6, height: 6, rx: 1 }]],
    minus: [["path", { d: "M6.5 12h11" }]],
    // The PRX mark (assets/logo-mark.svg): the Verdict X, a check crossed
    // by an interrupted counter-stroke.
    mark: [["path", { d: "M4.8 4.8 10.2 10.2M15.2 15.2 19.4 19.4", "stroke-width": 2.5 }],
      ["path", { d: "M4.6 13.4 8.9 17.7 19.6 4.6", "stroke-width": 2.75 }]],
    // PR state glyphs (overview.js STATES): each state differs by shape,
    // not only by colour, like the menu-bar glyphs.
    "s-waiting": [["circle", { cx: 12, cy: 12, r: 7.5 }]],
    "s-reviewing": [["circle", { cx: 12, cy: 12, r: 8.5 }], ["path", { d: "M8 12h.01M12 12h.01M16 12h.01", "stroke-width": 2.6 }]],
    "s-ready": [["circle", { cx: 12, cy: 12, r: 8.5 }], ["path", { d: "m8 12.3 2.7 2.7L16.2 9.3" }]],
    "s-changes": [["circle", { cx: 12, cy: 12, r: 8.5 }], ["path", { d: "M12 7.5v5.2M12 16.3v.2" }]],
    "s-merged": [["circle", { cx: 7, cy: 6, r: 2 }], ["circle", { cx: 7, cy: 18, r: 2 }], ["circle", { cx: 17, cy: 13, r: 2 }],
      ["path", { d: "M7 8v8M7 8.5c.5 3 3.5 4.5 8 4.5" }]],
  };

  function icon(name, label) {
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("class", "icon");
    svg.setAttribute("fill", "none");
    svg.setAttribute("stroke", "currentColor");
    svg.setAttribute("stroke-width", "1.75");
    svg.setAttribute("stroke-linecap", "round");
    svg.setAttribute("stroke-linejoin", "round");
    if (label) {
      svg.setAttribute("role", "img");
      svg.setAttribute("aria-label", label);
    } else {
      svg.setAttribute("aria-hidden", "true");
    }
    (ICONS[name] || []).forEach(function (shape) {
      const node = document.createElementNS(NS, shape[0]);
      Object.keys(shape[1]).forEach(function (key) { node.setAttribute(key, shape[1][key]); });
      svg.append(node);
    });
    return svg;
  }

  window.prxIcon = icon;
})();
