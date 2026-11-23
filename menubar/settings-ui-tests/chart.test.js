// The stacked-bar chart (chart.js): the axis and bar geometry for empty,
// single-bucket, sparse, negative and very large data, and the SVG and
// legend `render` builds, run against the fake DOM.
"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const Chart = require("../settings-ui/chart.js");
const { boot, walk } = require("./fake-dom.js");

const one = (input, output) => ({ labels: ["Oct 14"], series: [{ name: "all", values: [{ input: input, output: output }] }] });
const bottom = (g) => g.plot.y + g.plot.height;

test("niceMax picks the next 1 / 2 / 2.5 / 5 step at every magnitude", () => {
  assert.equal(Chart.niceMax(1), 1);
  assert.equal(Chart.niceMax(1.01), 2);
  assert.equal(Chart.niceMax(2), 2);
  assert.equal(Chart.niceMax(2.5), 2.5);
  assert.equal(Chart.niceMax(2.6), 5);
  assert.equal(Chart.niceMax(5.1), 10);
  assert.equal(Chart.niceMax(10), 10);
  assert.equal(Chart.niceMax(1000), 1000);
  assert.equal(Chart.niceMax(1001), 2000);
  assert.equal(Chart.niceMax(999999), 1000000);
  assert.equal(Chart.niceMax(3.2e12), 5e12);
  assert.equal(Chart.niceMax(0.3), 0.5);
  // Nothing to scale by: the axis still has a top.
  for (const bad of [0, -5, NaN, undefined, null]) assert.equal(Chart.niceMax(bad), 1, String(bad));
});

test("an empty chart has an axis, six ticks and no bars", () => {
  const g = Chart.layout({ labels: [], series: [] }, {});
  assert.deepEqual([g.width, g.height], [640, 220]);
  assert.equal(g.max, 1);
  assert.deepEqual(g.bars, []);
  assert.deepEqual(g.ticks.map((t) => t.label), ["0", "0.2", "0.4", "0.6", "0.8", "1"]);
  assert.equal(g.ticks[0].y, bottom(g));
  assert.equal(g.ticks[5].y, g.plot.y);
  // Buckets without any series, and series without values, draw empty bars.
  const bare = Chart.layout({ labels: ["a", "b"], series: [] }, {});
  assert.deepEqual(bare.bars.map((b) => [b.total, b.segments.length]), [[0, 0], [0, 0]]);
  const short = Chart.layout({ labels: ["a", "b"], series: [{ name: "x", values: [{ input: 5 }] }] }, {});
  assert.deepEqual(short.bars.map((b) => b.total), [5, 0]);
  assert.deepEqual(short.bars[0].segments.map((s) => s.part), ["input"]);
});

test("a single bucket is one centred bar, labelled, inside the plot", () => {
  const g = Chart.layout(one(300, 100), { width: 400, height: 200 });
  assert.equal(g.bars.length, 1);
  const bar = g.bars[0];
  assert.equal(bar.width, 40);
  assert.equal(bar.showLabel, true);
  assert.ok(Math.abs(bar.x + bar.width / 2 - (g.plot.x + g.plot.width / 2)) < 1e-9);
  assert.equal(g.max, 500);
  assert.equal(bar.total, 400);
  // Input sits on the baseline, output on top of it.
  assert.deepEqual(bar.segments.map((s) => [s.part, s.value]), [["input", 300], ["output", 100]]);
  assert.ok(Math.abs(bar.segments[0].y + bar.segments[0].height - bottom(g)) < 1e-9);
  assert.ok(Math.abs(bar.segments[1].y - (bottom(g) - 0.8 * g.plot.height)) < 1e-9);
});

test("negative and non-numeric values are drawn as nothing, never below the axis", () => {
  const data = { labels: ["a", "b"], series: [
    { name: "x", values: [{ input: -500, output: 200 }, { input: NaN, output: Infinity }] },
    { name: "y", values: [{ input: 100, output: "12" }, { input: null }] },
  ] };
  const g = Chart.layout(data, { width: 300, height: 200 });
  assert.deepEqual(g.bars.map((b) => b.total), [300, 0]);
  assert.equal(g.max, 500);
  assert.deepEqual(g.bars[0].segments.map((s) => [s.series, s.part, s.value]), [[0, "output", 200], [1, "input", 100]]);
  assert.equal(g.bars[1].segments.length, 0);
  g.bars.forEach((bar) => bar.segments.forEach((s) => {
    assert.ok(s.height > 0, "a drawn segment has a height");
    assert.ok(s.y >= g.plot.y - 1e-9 && s.y + s.height <= bottom(g) + 1e-9, "segments stay inside the plot");
  }));
  assert.ok(g.ticks.every((t) => Number.isFinite(t.y) && Number.isFinite(t.value)));
});

test("very large totals keep finite geometry and a readable axis", () => {
  const g = Chart.layout(one(3.2e15, 4e14), { width: 640, height: 220, format: (v) => v / 1e15 + "Q" });
  assert.equal(g.max, 5e15);
  assert.deepEqual(g.ticks.map((t) => t.label), ["0Q", "1Q", "2Q", "3Q", "4Q", "5Q"]);
  const heights = g.bars[0].segments.map((s) => s.height);
  assert.ok(Math.abs(heights[0] + heights[1] - (3.6e15 / 5e15) * g.plot.height) < 1e-6);
  // One tiny bucket beside a huge one is still a segment (render gives it
  // a visible minimum height).
  const mixed = Chart.layout({ labels: ["a", "b"], series: [{ name: "x", values: [{ input: 1 }, { input: 9e12 }] }] }, {});
  assert.equal(mixed.bars[0].segments.length, 1);
  assert.ok(mixed.bars[0].segments[0].height > 0);
});

test("bars never overlap or leave the plot, however many buckets or narrow the chart", () => {
  for (const [n, width] of [[1, 120], [7, 300], [90, 300], [400, 640]]) {
    const data = { labels: Array.from({ length: n }, (_, i) => "b" + i), series: [{ name: "x", values: Array.from({ length: n }, () => ({ input: 1 })) }] };
    const g = Chart.layout(data, { width: width, height: 160 });
    assert.equal(g.bars.length, n);
    assert.ok(g.bars.every((b) => b.width >= 2 && b.width <= 40), n + " bars");
    assert.ok(g.bars.every((b, i) => i === 0 || b.x > g.bars[i - 1].x), n + " bars advance");
    // The newest bucket is always labelled; labels are evenly thinned.
    assert.equal(g.bars[n - 1].showLabel, true);
    const shown = g.bars.map((b, i) => (b.showLabel ? i : -1)).filter((i) => i !== -1);
    const gaps = new Set(shown.slice(1).map((v, i) => v - shown[i]));
    assert.ok(gaps.size <= 1, "one label interval");
  }
});

test("series colours follow `color`, wrap after eight, and mark the output part", () => {
  const series = Array.from({ length: 10 }, (_, i) => ({ name: "s" + i, values: [{ input: 1, output: 1 }] }));
  const g = Chart.layout({ labels: ["a"], series: series }, {});
  assert.deepEqual(g.bars[0].segments.filter((s) => s.part === "input").map((s) => s.cls),
    ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c1", "c2"]);
  assert.ok(g.bars[0].segments.filter((s) => s.part === "output").every((s) => / out$/.test(s.cls)));
  const pinned = Chart.layout({ labels: ["a"], series: [{ name: "Other", color: 5, values: [{ input: 1 }] }] }, {});
  assert.equal(pinned.bars[0].segments[0].cls, "c6");
});

// `render` needs a document: load chart.js the way the window does.
function windowChart() {
  const w = boot({ scripts: ["chart.js"] });
  return w.window.prxChart;
}

test("render: an accessible SVG with one tooltip per segment, and a legend", () => {
  const C = windowChart();
  const data = { labels: ["Oct 13", "Oct 14"], series: [
    { name: "claude", values: [{ input: 300, output: 100 }, { input: 0, output: 0 }] },
    { name: "codex", values: [{ input: 200, output: 0 }, { input: 0, output: 0 }] },
  ] };
  const figure = C.render(data, { width: 300, height: 200, label: "Tokens per day", format: (v) => v + "t",
    title: (s, i, part, value) => s.name + "/" + part + "/" + value });
  assert.equal(figure.tagName, "FIGURE");
  const svg = figure.querySelector("svg");
  assert.equal(svg.getAttribute("role"), "img");
  assert.equal(svg.getAttribute("aria-label"), "Tokens per day");
  assert.equal(svg.getAttribute("viewBox"), "0 0 300 200");
  const rects = svg.querySelectorAll("rect");
  assert.deepEqual(rects.map((r) => r.textContent), ["Oct 13 · claude/input/300", "Oct 13 · claude/output/100", "Oct 13 · codex/input/200"]);
  assert.ok(rects.every((r) => Number(r.getAttribute("height")) >= 0.5));
  // An empty bucket still says what it is.
  const bars = svg.querySelectorAll("g.bar");
  assert.equal(bars.length, 2);
  assert.equal(bars[1].textContent, "Oct 14: 0t tokens");
  assert.deepEqual(svg.querySelectorAll("text.axis").slice(0, 6).map((t) => t.textContent), ["0t", "200t", "400t", "600t", "800t", "1000t"]);
  const legend = figure.querySelector("ul.chart-legend");
  assert.deepEqual(legend.children.map((li) => li.textContent), ["claude", "codex", "input + cacheoutput"]);
  assert.deepEqual(legend.querySelectorAll("li > span.swatch").map((s) => s.className), ["swatch c1", "swatch c2", "swatch c1", "swatch out c1"]);
});

test("render: no data draws the axis only; names are text, never markup", () => {
  const C = windowChart();
  const empty = C.render({ labels: [], series: [] }, {});
  assert.equal(empty.querySelector("svg").getAttribute("aria-label"), "Chart");
  assert.equal(empty.querySelectorAll("rect").length, 0);
  assert.equal(empty.querySelectorAll("line.grid").length, 6);
  assert.equal(empty.querySelectorAll("line.baseline").length, 1);
  assert.equal(empty.querySelector("ul.chart-legend").children.length, 1);

  const evil = "<img src=x onerror=alert(1)>";
  const figure = C.render({ labels: [evil], series: [{ name: evil, values: [{ input: 5 }] }] }, {});
  const tags = new Set(walk(figure).map((n) => n.tagName.toLowerCase()));
  assert.deepEqual(Array.from(tags).sort(), ["figure", "g", "li", "line", "rect", "span", "svg", "text", "title", "ul"]);
  assert.equal(figure.querySelector("ul.chart-legend").children[0].textContent, evil);
  assert.equal(figure.querySelector("rect").textContent, evil + " · " + evil + " input: 5");
});
