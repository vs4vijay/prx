// A small stacked-bar chart as inline SVG (design D15): one bar per bucket,
// one stack per series (group), each split into input and output tokens.
// Colors come from the page's CSS variables through classes (`c1`..`c8`,
// `out` for the lighter output part), so light and dark mode follow the
// system, and the CSP needs no inline styles. `layout` is the pure geometry
// (tested in settings-ui-tests); `render` builds the DOM.
(function (root) {
  "use strict";
  const NS = "http://www.w3.org/2000/svg";
  const COLORS = 8;
  const MARGIN = { top: 10, right: 8, bottom: 24, left: 46 };

  // The axis top: the smallest 1 / 2 / 2.5 / 5 x 10^n at or above `value`.
  function niceMax(value) {
    if (!(value > 0)) return 1;
    const exp = Math.pow(10, Math.floor(Math.log10(value)));
    const f = value / exp;
    const nice = f <= 1 ? 1 : f <= 2 ? 2 : f <= 2.5 ? 2.5 : f <= 5 ? 5 : 10;
    return nice * exp;
  }

  // data: {labels: [..], series: [{name, values: [{input, output}]}]}
  // opts: {width, height, format(value) -> axis text, title(series, i, part, value) -> tooltip}
  // -> {width, height, plot, max, ticks: [{y, label}], bars: [{x, width, label, showLabel, total, segments: [{y, height, series, part, value, cls}]}]}
  function layout(data, opts) {
    const width = opts.width || 640, height = opts.height || 220;
    const format = opts.format || String;
    const plot = { x: MARGIN.left, y: MARGIN.top, width: width - MARGIN.left - MARGIN.right, height: height - MARGIN.top - MARGIN.bottom };
    const n = data.labels.length;
    const totals = data.labels.map(function (_, i) {
      return data.series.reduce(function (sum, s) { const v = s.values[i] || {}; return sum + (v.input || 0) + (v.output || 0); }, 0);
    });
    const max = niceMax(Math.max.apply(null, [0].concat(totals)));
    const scale = function (v) { return (v / max) * plot.height; };
    const slot = n ? plot.width / n : plot.width;
    const barWidth = Math.max(2, Math.min(40, slot * 0.68));
    // Thin the labels so they never overlap (about 44px each).
    const every = Math.max(1, Math.ceil((n * 44) / plot.width));
    const bars = data.labels.map(function (label, i) {
      let base = plot.y + plot.height;
      const segments = [];
      data.series.forEach(function (s, si) {
        const v = s.values[i] || {};
        [["input", v.input || 0], ["output", v.output || 0]].forEach(function (part) {
          if (!part[1]) return;
          const h = scale(part[1]);
          base -= h;
          segments.push({ y: base, height: h, series: si, part: part[0], value: part[1],
            cls: "c" + ((s.color === undefined ? si : s.color) % COLORS + 1) + (part[0] === "output" ? " out" : "") });
        });
      });
      return { x: plot.x + i * slot + (slot - barWidth) / 2, width: barWidth, label: label, showLabel: (n - 1 - i) % every === 0,
        total: totals[i], segments: segments };
    });
    const ticks = [0, 0.2, 0.4, 0.6, 0.8, 1].map(function (f) {
      return { y: plot.y + plot.height - f * plot.height, value: f * max, label: format(f * max) };
    });
    return { width: width, height: height, plot: plot, max: max, ticks: ticks, bars: bars };
  }

  function svg(tag, attrs, children) {
    const node = document.createElementNS(NS, tag);
    Object.keys(attrs || {}).forEach(function (k) { if (attrs[k] !== null && attrs[k] !== undefined) node.setAttribute(k, attrs[k]); });
    (children || []).forEach(function (c) { if (c) node.append(c); });
    return node;
  }

  // The chart plus its legend, as one element. opts as for layout, plus
  // `label` (the chart's accessible name).
  function render(data, opts) {
    const g = layout(data, opts);
    const round = function (v) { return Math.round(v * 10) / 10; };
    const title = opts.title || function (s, i, part, value) { return s.name + " " + part + ": " + value; };
    const nodes = [];
    g.ticks.forEach(function (t) {
      nodes.push(svg("line", { class: "grid", x1: g.plot.x, x2: g.plot.x + g.plot.width, y1: round(t.y), y2: round(t.y) }));
      const text = svg("text", { class: "axis", x: g.plot.x - 6, y: round(t.y) + 4, "text-anchor": "end" });
      text.textContent = t.label;
      nodes.push(text);
    });
    g.bars.forEach(function (bar, i) {
      const group = svg("g", { class: "bar" });
      const head = svg("title");
      head.textContent = bar.label + ": " + (opts.format || String)(bar.total) + " tokens";
      if (!bar.segments.length) group.append(head);
      bar.segments.forEach(function (seg) {
        const rect = svg("rect", { class: "seg " + seg.cls, x: round(bar.x), y: round(seg.y), width: round(bar.width), height: Math.max(0.5, round(seg.height)) });
        const tip = svg("title");
        tip.textContent = bar.label + " · " + title(data.series[seg.series], i, seg.part, seg.value);
        rect.append(tip);
        group.append(rect);
      });
      nodes.push(group);
      if (bar.showLabel) {
        const text = svg("text", { class: "axis", x: round(bar.x + bar.width / 2), y: g.plot.y + g.plot.height + 16, "text-anchor": "middle" });
        text.textContent = bar.label;
        nodes.push(text);
      }
    });
    nodes.push(svg("line", { class: "baseline", x1: g.plot.x, x2: g.plot.x + g.plot.width, y1: g.plot.y + g.plot.height, y2: g.plot.y + g.plot.height }));
    const chart = svg("svg", { class: "chart-svg", viewBox: "0 0 " + g.width + " " + g.height, role: "img", "aria-label": opts.label || "Chart" }, nodes);
    const legend = document.createElement("ul");
    legend.className = "chart-legend";
    data.series.forEach(function (s, si) {
      const item = document.createElement("li");
      const swatch = document.createElement("span");
      swatch.className = "swatch c" + ((s.color === undefined ? si : s.color) % COLORS + 1);
      item.append(swatch, document.createTextNode(s.name));
      legend.append(item);
    });
    // The input / output key uses the first series' colour, the same
    // opacity pair the bars use (not a grey that matches no bar).
    const first = data.series[0];
    const tone = "c" + ((first && first.color !== undefined ? first.color : 0) % COLORS + 1);
    const key = document.createElement("li");
    key.className = "legend-key";
    const solid = document.createElement("span");
    solid.className = "swatch " + tone;
    const light = document.createElement("span");
    light.className = "swatch out " + tone;
    key.append(solid, document.createTextNode("input + cache"), light, document.createTextNode("output"));
    legend.append(key);
    const wrap = document.createElement("figure");
    wrap.className = "chart";
    wrap.append(chart, legend);
    return wrap;
  }

  const api = { COLORS: COLORS, niceMax: niceMax, layout: layout, render: render };
  if (typeof module !== "undefined" && module.exports) module.exports = api;
  else root.prxChart = Object.freeze(api);
})(this);
