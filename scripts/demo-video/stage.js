// The demo video's timeline. Everything on screen is a pure function of the
// time passed to renderAt(t), so render.py can step through it frame by frame
// and get the same picture every run.
//
// Sources (see README.md): ui/*.png are Playwright captures of the real
// settings UI on its fictional mock backend; term/*/ are vhs recordings of
// the real prx binary in the demo sandbox; menu/*/ are the frames of the
// recorded menu bar takes (menu-clips/); brand/ is the repo's logo.
"use strict";
(function () {
  const M = window.MANIFEST;
  const FPS = 30;
  const stage = document.getElementById("stage");

  // --- maths ---------------------------------------------------------------
  const clamp = (x, a = 0, b = 1) => Math.min(b, Math.max(a, x));
  const inOut = (x) => { x = clamp(x); return x < 0.5 ? 4 * x * x * x : 1 - Math.pow(-2 * x + 2, 3) / 2; };
  const outCubic = (x) => 1 - Math.pow(1 - clamp(x), 3);
  const ramp = (t, a, b) => inOut((t - a) / (b - a));     // 0 -> 1, eased, between a and b
  const lin = (t, a, b) => clamp((t - a) / (b - a));
  const mix = (a, b, p) => a + (b - a) * p;

  // --- DOM helpers ---------------------------------------------------------
  function el(tag, attrs, parent) {
    const node = document.createElement(tag);
    Object.entries(attrs || {}).forEach(([k, v]) => {
      if (k === "html") node.innerHTML = v; else if (k === "style") node.style.cssText = v; else node.setAttribute(k, v);
    });
    (parent || stage).appendChild(node);
    return node;
  }
  const pending = [];
  function img(src, parent, attrs) {
    const node = el("img", Object.assign({ src: src }, attrs || {}), parent);
    pending.push(node.decode().catch(() => { throw new Error("cannot load " + src); }));
    return node;
  }
  // A caption: fades up at `a`, fades out before `b` (scene-local seconds).
  function caption(scene, a, b, y, html, size) {
    const node = el("div", { class: "cap", html: html, style: "top:" + y + "px;" + (size ? "font-size:" + size + "px;line-height:" + Math.round(size * 1.2) + "px;" : "") }, scene.el);
    scene.caps.push({ a: a, b: b, node: node, text: node.textContent });
  }
  function showCaptions(scene, t) {
    scene.caps.forEach((c) => {
      const up = outCubic((t - c.a) / 0.5);
      const o = t < c.a ? 0 : up * (1 - lin(t, c.b - 0.35, c.b));
      c.node.style.opacity = o.toFixed(3);
      c.node.style.transform = "translateY(" + ((1 - up) * 16).toFixed(2) + "px)";
    });
  }
  // Fade-and-rise for any block.
  function rise(node, t, a, dist) {
    const p = outCubic((t - a) / 0.6);
    node.style.opacity = (t < a ? 0 : p).toFixed(3);
    node.style.transform = "translateY(" + ((1 - p) * (dist || 20)).toFixed(2) + "px)";
  }

  const scenes = [];
  function scene(id, dur, build, render) {
    const s = { id: id, dur: dur, caps: [], render: render };
    s.el = el("section", { class: "scene", id: id });
    build(s);
    scenes.push(s);
    return s;
  }

  // =========================================================================
  // 1. Open: logo and tagline
  // =========================================================================
  scene("open", 4.0, (s) => {
    s.logo = img("brand/logo.svg", s.el, { id: "open-logo" });
    s.sub = el("div", { id: "open-sub", class: "centre", html: "Local-first pull request review, by the coding agent you already run." }, s.el);
  }, (s, t) => {
    const p = outCubic(t / 0.9);
    s.logo.style.opacity = p.toFixed(3);
    s.logo.style.transform = "scale(" + mix(0.94, 1, p).toFixed(4) + ")";
    rise(s.sub, t, 1.1);
  });

  // =========================================================================
  // 2. The problem, and the rule that answers it
  // =========================================================================
  scene("problem", 6.2, (s) => {
    s.k = el("div", { class: "centre kicker", style: "top:300px", html: "The problem" }, s.el);
    s.a = el("div", { class: "centre big", style: "top:372px", html: "An agent that can post to GitHub<br>can also approve a pull request." }, s.el);
    s.b = el("div", { class: "centre big", style: "top:640px;font-size:60px;line-height:74px;font-weight:600;letter-spacing:-1px", html: "In prx, <b>the agent only reads.</b>" }, s.el);
  }, (s, t) => {
    rise(s.k, t, 0.15, 12);
    rise(s.a, t, 0.35);
    rise(s.b, t, 2.9);
  });

  // =========================================================================
  // 3-4, 7. Terminal shots (vhs frames, one PNG pair per video frame)
  // =========================================================================
  function terminal(id, clip, top, captions) {
    const lead = 0.35, tail = 0.45;
    const padX = 40, padY = 34;
    const w = clip.w + padX * 2, h = clip.h + padY * 2;
    return scene(id, lead + clip.frames / FPS + tail, (s) => {
      s.panel = el("div", { class: "panel term", style: "left:" + Math.round((1920 - w) / 2) + "px;top:" + top + "px;width:" + w + "px;height:" + h + "px" }, s.el);
      const screen = el("div", { class: "screen", style: "width:" + clip.w + "px;height:" + clip.h + "px" }, s.panel);
      s.text = el("img", {}, screen);
      s.cursor = el("img", {}, screen);
      captions.forEach((c) => caption(s, c[0] < 0 ? s.dur + c[0] : c[0], c[1] <= 0 ? s.dur + c[1] : c[1], c[2], c[3], c[4]));
    }, (s, t) => {
      const n = clamp(Math.round((t - lead) * FPS), 0, clip.frames - 1) + 1;
      const name = String(n).padStart(5, "0") + ".png";
      s.text.src = clip.dir + "/frame-text-" + name;
      s.cursor.src = clip.dir + "/frame-cursor-" + name;
      pending.push(s.text.decode(), s.cursor.decode());
      showCaptions(s, t);
    });
  }

  terminal("status", M.term.status, 250, [
    [0.3, -0.05, 120, "Every PR prx tracks in the repos you watch, and where it stands"],
  ]);
  terminal("brief", M.term.brief, 470, [
    [0.3, -0.05, 310, "A <b>brief</b> says what a PR does, in plain language", 56],
  ]);

  // =========================================================================
  // (optional) The menu bar: recorded takes of the real menu (menu-clips/)
  // =========================================================================
  // One scene, one camera. The takes are captures of the top of a 1728 pt wide
  // screen over a flat backdrop; the panel is a window onto that screen, so
  // the menu drops from the panel's top edge. Nothing is drawn in: there is
  // no menu bar strip or status item, because the takes do not have one.
  if (M.menu) {
    const S = M.menu.scale, K = M.menu.clips;      // stage px per screen pt
    // Takes: [clip, scene time it comes in, cross-fade, frames skipped at its
    // start]. Each holds its last frame until the next one is in.
    const takes = [["open", 0, 0, 0], ["pr", 6.2, 0.25, 0], ["approve", 13.3, 0.25, 0], ["approved", 19.95, 0.25, 3]];
    // Camera: [time, left, top, width] of the view, in screen points. The
    // view is 600 pt tall; the panel takes the view's width.
    const MENU = [965, 780], WIDE = [0, 1290], ALERT = [545, 640];
    const cam = [
      [0, MENU[0], 0, MENU[1]], [3.6, MENU[0], 0, MENU[1]],             // the menu opens
      [4.8, MENU[0], 160, MENU[1]], [6.9, MENU[0], 160, MENU[1]],       // down the groups
      [8.1, WIDE[0], 160, WIDE[1]], [12.9, WIDE[0], 160, WIDE[1]],      // a PR's submenu, to the left
      [13.9, MENU[0], 160, MENU[1]], [15.15, MENU[0], 160, MENU[1]],    // back: the inline Approve row
      [16.2, ALERT[0], 55, ALERT[1]], [19.8, ALERT[0], 55, ALERT[1]],   // the confirmation, centred
      [20.4, MENU[0], 0, MENU[1]],                                      // reopened: "approval posted"
    ];
    scene("menu", 24.4, (s) => {
      s.panel = el("div", { class: "panel menu", style: "top:180px;height:840px" }, s.el);
      s.canvas = el("div", { class: "canvas" }, s.panel);
      s.takes = takes.map((c, i) => {
        const k = K[c[0]];
        const layer = el("div", { class: "take" }, s.canvas);
        const node = el("img", { style: "left:" + k.x * S + "px;top:" + k.y * S + "px;width:" + k.w * S + "px;height:" + k.h * S + "px" }, layer);
        const next = takes[i + 1];
        return { k: k, at: c[1], fade: c[2], skip: c[3], until: next ? next[1] + next[2] : Infinity, layer: layer, node: node };
      });
      const y = 76;
      caption(s, 0.3, 3.7, y, "On macOS, <b>PRX.app</b> lives in the menu bar");
      caption(s, 4.0, 6.9, y, "Every PR you watch, <b>grouped by state</b>");
      caption(s, 7.2, 10.6, y, "Each PR opens to its <b>verdict and brief</b>");
      caption(s, 10.9, 13.2, y, "Review, block, snooze, or <b>approve</b>");
      caption(s, 13.6, 19.75, y, "Approving is <b>yours alone</b>, and PRX asks first");
      caption(s, 20.35, 24.35, y, "<b>Approval posted</b>, from your own account");
    }, (s, t) => {
      s.takes.forEach((k) => {
        const on = t >= k.at && t < k.until;
        k.layer.style.display = on ? "block" : "none";
        if (!on) return;
        k.layer.style.opacity = (k.fade ? lin(t, k.at, k.at + k.fade) : 1).toFixed(3);
        const n = clamp(Math.round((t - k.at) * FPS) + k.skip, 0, k.k.frames - 1);
        k.node.src = k.k.dir + "/f_" + String(n).padStart(5, "0") + ".png";
        pending.push(k.node.decode());
      });
      let i = 0;
      while (i < cam.length - 2 && t >= cam[i + 1][0]) i++;
      const c0 = cam[i], c1 = cam[i + 1];
      const p = ramp(t, c0[0], c1[0]);
      const vx = mix(c0[1], c1[1], p), vy = mix(c0[2], c1[2], p), w = Math.round(mix(c0[3], c1[3], p) * S);
      s.panel.style.width = w + "px";
      s.panel.style.left = Math.round((1920 - w) / 2) + "px";
      s.canvas.style.transform = "translate(" + (-vx * S).toFixed(2) + "px," + (-vy * S).toFixed(2) + "px)";
      showCaptions(s, t);
    });
  }

  // =========================================================================
  // 5. PRX.app: dashboard -> pull requests -> history -> usage
  // =========================================================================
  // Scene-local times of each beat.
  const A = {
    scrollDash: [1.9, 5.3], dashScroll: 318,
    toPrs: [6.2, 7.2], clickPrs: 7.5,
    toTitle: [9.0, 10.0], clickTitle: 10.45,
    zoomBrief: [10.9, 12.2], unzoomBrief: [15.5, 16.5],
    toHistory: [15.7, 16.8], clickHistory: 17.1,
    toRow: [18.3, 19.2], clickRow: 19.65,
    zoomFindings: [20.0, 21.2], unzoomFindings: [24.6, 25.6],
    toUsage: [24.8, 25.9], clickUsage: 26.2,
    scrollUsage: [27.8, 31.2],
    end: 33.6,
  };
  const FRAME = { x: 336, y: 180, w: 1248, h: 840, s: 1.2 };
  const win = M.ui.window;
  const mid = (b, fx) => [b.x + b.width * (fx === undefined ? 0.5 : fx), b.y + b.height / 2];
  const P = {
    rest: [742, 452],
    navPrs: mid(M.ui.targets.nav_prs, 0.45),
    title: mid(M.ui.targets.pr_title, 0.6),
    navHistory: mid(M.ui.targets.nav_history, 0.45),
    row: mid(M.ui.targets.hist_row, 0.3),
    navUsage: mid(M.ui.targets.nav_usage, 0.45),
  };
  const moves = [
    [A.toPrs, P.rest, P.navPrs], [A.toTitle, P.navPrs, P.title], [A.toHistory, P.title, P.navHistory],
    [A.toRow, P.navHistory, P.row], [A.toUsage, P.row, P.navUsage],
  ];
  const clicks = [A.clickPrs, A.clickTitle, A.clickHistory, A.clickRow, A.clickUsage];
  // Camera: [time, zoom, focus x, focus y] in window coordinates.
  const camera = [
    // Both push-ins keep the right edge and stop short of the sidebar, so the
    // frame shows the whole content pane and nothing cut in half beside it.
    [0, 1, 1040, 500], [A.zoomBrief[0], 1, 1040, 500], [A.zoomBrief[1], 1.27, 1040, 500],
    [A.unzoomBrief[0], 1.27, 1040, 500], [A.unzoomBrief[1], 1, 1040, 500],
    [A.zoomFindings[0], 1, 1040, 420], [A.zoomFindings[1], 1.26, 1040, 420],
    [A.unzoomFindings[0], 1.26, 1040, 420], [A.unzoomFindings[1], 1, 1040, 420],
  ];

  scene("app", A.end, (s) => {
    s.panel = el("div", { class: "panel", style: "left:" + FRAME.x + "px;top:" + FRAME.y + "px;width:" + FRAME.w + "px;height:" + FRAME.h + "px" }, s.el);
    s.content = el("div", { class: "content" }, s.panel);
    s.layers = [];
    const ui = (name) => "ui/" + M.ui.states[name].file;
    // A full-window state that fades in at `at`.
    const full = (name, at, fade) => {
      const node = img(ui(name), s.content, { style: "width:" + win.w + "px;height:" + win.h + "px" });
      s.layers.push({ node: node, at: at, fade: fade });
    };
    // Only the sidebar of a state (a hovered nav item), over whatever is in the pane.
    const sidebar = (name, at) => {
      const node = img(ui(name), s.content, { style: "width:" + win.w + "px;height:" + win.h + "px;clip-path:inset(0 " + (win.w - M.ui.states.dash_tall.x) + "px 0 0)" });
      s.layers.push({ node: node, at: at, fade: 0.16 });
    };
    // The scrolling pane as one tall image, moved by `scroll(t)`.
    const tall = (name, at, fade, scroll) => {
      const st = M.ui.states[name];
      const pane = el("div", { class: "pane", style: "left:" + st.x + "px;top:" + st.y + "px;width:" + st.w + "px;height:" + win.h + "px" }, s.content);
      const node = img(ui(name), pane, { style: "width:" + st.w + "px;height:" + st.h + "px" });
      s.layers.push({ node: pane, at: at, fade: fade, inner: node, scroll: scroll, max: st.h - win.h });
    };
    full("dash", -1, 0.01);
    tall("dash_tall", -1, 0.01, (t) => A.dashScroll * ramp(t, A.scrollDash[0], A.scrollDash[1]));
    sidebar("dash_nav_prs", A.toPrs[1] - 0.1);
    full("prs", A.clickPrs + 0.08, 0.3);
    full("prs_hover", A.toTitle[1] - 0.08, 0.16);
    full("prs_open", A.clickTitle + 0.08, 0.32);
    sidebar("prs_nav_history", A.toHistory[1] - 0.1);
    full("hist", A.clickHistory + 0.08, 0.3);
    full("hist_hover", A.toRow[1] - 0.08, 0.16);
    full("hist_open", A.clickRow + 0.08, 0.32);
    sidebar("hist_nav_usage", A.toUsage[1] - 0.1);
    full("usage", A.clickUsage + 0.08, 0.3);
    tall("usage_tall", A.clickUsage + 0.08, 0.3, (t, max) => max * ramp(t, A.scrollUsage[0], A.scrollUsage[1]));

    s.ripple = el("div", { id: "ripple" }, s.content);
    s.cursor = el("div", { id: "cursor", html:
      '<svg viewBox="0 0 22 30" width="17" height="23"><path d="M1.5 1.5 L1.5 23.5 L7.2 18.4 L11.2 27.6 L14.8 26 L10.9 17 L18.6 17 Z" fill="#111114" stroke="#FFFFFF" stroke-width="1.6" stroke-linejoin="round"/></svg>' }, s.content);

    const y = 76;
    // After the menu bar section the app has been introduced, so this names the window.
    caption(s, 0.4, A.clickPrs - 0.15, y, M.menu ? "The <b>PRX.app</b> window shows what needs you" : "On macOS, <b>PRX.app</b> shows what needs you");
    caption(s, A.clickPrs + 0.35, A.clickTitle + 0.1, y, "Every PR, with the agent's verdict");
    caption(s, A.clickTitle + 0.45, A.clickHistory - 0.1, y, "Read the brief <b>before</b> you open the diff");
    caption(s, A.clickHistory + 0.35, A.clickUsage - 0.1, y, "Every review is kept: <b>findings</b>, engine, tokens");
    caption(s, A.clickUsage + 0.35, A.end - 0.05, y, "What reviews cost, and how they turned out");
  }, (s, t) => {
    // layers
    s.layers.forEach((l) => {
      l.node.style.opacity = lin(t, l.at, l.at + l.fade).toFixed(3);
      if (l.inner) {
        const px = l.scroll(t, l.max);
        l.inner.style.transform = "translateY(" + (-px).toFixed(2) + "px)";
      }
    });
    // camera
    let k = 0;
    while (k < camera.length - 2 && t >= camera[k + 1][0]) k++;
    const c0 = camera[k], c1 = camera[k + 1];
    const p = c1[0] > c0[0] ? ramp(t, c0[0], c1[0]) : 1;
    const z = mix(c0[1], c1[1], p), fx = mix(c0[2], c1[2], p), fy = mix(c0[3], c1[3], p);
    const sc = FRAME.s * z;
    const tx = clamp(FRAME.w / 2 - fx * sc, FRAME.w - win.w * sc, 0);
    const ty = clamp(FRAME.h / 2 - fy * sc, FRAME.h - win.h * sc, 0);
    s.content.style.transform = "translate(" + tx.toFixed(2) + "px," + ty.toFixed(2) + "px) scale(" + sc.toFixed(5) + ")";
    // pointer: eased moves along a slight arc, a dip and a ripple on each click
    let pos = moves[0][1];
    moves.forEach((m) => {
      if (t < m[0][0]) return;
      const q = ramp(t, m[0][0], m[0][1]);
      const dx = m[2][0] - m[1][0], dy = m[2][1] - m[1][1];
      const len = Math.hypot(dx, dy) || 1, bow = Math.sin(Math.PI * q) * Math.min(26, len * 0.07);
      pos = [mix(m[1][0], m[2][0], q) - (dy / len) * bow, mix(m[1][1], m[2][1], q) + (dx / len) * bow];
    });
    let press = 1, rip = 0, ripP = 0;
    clicks.forEach((ct) => {
      const d = t - ct;
      if (d >= -0.08 && d < 0.16) press = 1 - 0.14 * Math.sin(Math.PI * (d + 0.08) / 0.24);
      if (d >= 0 && d < 0.5) { ripP = d / 0.5; rip = 1; }
    });
    const vis = lin(t, A.toPrs[0] - 0.7, A.toPrs[0] - 0.3) * (1 - lin(t, A.clickUsage + 0.9, A.clickUsage + 1.4));
    s.cursor.style.opacity = vis.toFixed(3);
    s.cursor.style.transform = "translate(" + pos[0].toFixed(2) + "px," + pos[1].toFixed(2) + "px) scale(" + press.toFixed(3) + ")";
    s.ripple.style.opacity = rip ? (0.9 * (1 - ripP)).toFixed(3) : "0";
    s.ripple.style.transform = "translate(" + pos[0].toFixed(2) + "px," + pos[1].toFixed(2) + "px) scale(" + mix(0.3, 1.25, outCubic(ripP)).toFixed(3) + ")";
    showCaptions(s, t);
  });

  // =========================================================================
  // 6. The human approval, back in the terminal
  // =========================================================================
  terminal("approve", M.term.approve, 520, [
    // With the menu bar section the approval has been shown as a click; this is the same thing typed.
    [0.3, -0.05, 330, M.menu ? "The same approval, from the <b>terminal</b>" : "Approving is <b>yours alone</b>", 64],
  ]);

  // =========================================================================
  // 7. The trust model
  // =========================================================================
  scene("trust", 8.0, (s) => {
    s.k = el("div", { class: "centre kicker", style: "top:196px", html: "One rule" }, s.el);
    s.h = el("div", { class: "centre big", style: "top:262px;font-size:72px;line-height:88px", html: "The agent analyzes. prx posts. <b>You approve.</b>" }, s.el);
    const cards = el("div", { class: "cards" }, s.el);
    s.cards = [
      ["01", "The agent", "Read-only. It writes one JSON review manifest, and nothing to GitHub.", ""],
      ["02", "prx", "Validates the manifest, then posts the review through your own <code>gh</code>.", ""],
      ["03", "You", "<code>prx approve</code>, or Approve in the app: the only approve path.", " you"],
    ].map((c) => el("div", { class: "card" + c[3], html: '<div class="n">' + c[0] + "</div><h3>" + c[1] + "</h3><p>" + c[2] + "</p>" }, cards));
  }, (s, t) => {
    rise(s.k, t, 0.15, 12);
    rise(s.h, t, 0.3);
    s.cards.forEach((c, i) => rise(c, t, 1.1 + i * 0.8, 26));
  });

  // =========================================================================
  // 8. Close: logo, install line, repository
  // =========================================================================
  scene("end", 7.5, (s) => {
    s.logo = img("brand/logo.svg", s.el, { id: "end-logo" });
    s.code = el("div", { id: "end-code", html: "<i>$</i> git clone https://github.com/vs4vijay/prx\n<i>$</i> cd prx &amp;&amp; cargo install --path ." }, s.el);
    s.url = el("div", { id: "end-url", class: "centre", html: "github.com/<b>vs4vijay/prx</b>" }, s.el);
    s.meta = el("div", { id: "end-meta", class: "centre", html: "Open source, MIT. CLI and daemon for macOS and Linux; PRX.app for macOS." }, s.el);
  }, (s, t) => {
    const p = outCubic(t / 0.8);
    s.logo.style.opacity = p.toFixed(3);
    s.logo.style.transform = "scale(" + mix(0.95, 1, p).toFixed(4) + ")";
    rise(s.code, t, 0.8);
    rise(s.url, t, 1.5);
    rise(s.meta, t, 2.1, 12);
  });

  // =========================================================================
  // Poster: a static composition, not part of the timeline
  // =========================================================================
  const poster = el("section", { class: "scene hidden", id: "poster" });
  img("brand/logo.svg", poster, { id: "poster-logo" });
  el("h1", { html: "AI reviews the PR.<br><b>You approve it.</b>" }, poster);
  el("p", { html: "Local-first pull request review, by the coding agent you already run." }, poster);
  el("div", { class: "url", html: "github.com/vs4vijay/prx" }, poster);
  img("ui/" + M.ui.states.prs_open.file, el("div", { class: "panel" }, poster));

  // --- timeline ------------------------------------------------------------
  let at = 0;
  scenes.forEach((s) => { s.start = at; at += s.dur; });
  const TOTAL = at;
  const FADE_IN = 0.45, FADE_OUT = 0.4;

  async function renderAt(t) {
    poster.classList.add("hidden");
    scenes.forEach((s, i) => {
      const local = t - s.start;
      const on = local >= 0 && local < s.dur;
      s.el.style.display = on ? "block" : "none";
      if (!on) return;
      // The first frame is the bare background, the last scene holds to the end.
      const fadeOut = i === scenes.length - 1 ? 1 : 1 - lin(local, s.dur - FADE_OUT, s.dur);
      const fadeIn = outCubic(local / FADE_IN);
      s.el.style.opacity = (fadeIn * fadeOut).toFixed(3);
      s.el.style.transform = "scale(" + mix(0.985, 1, fadeIn).toFixed(4) + ")";
      s.render(s, local);
    });
    await settle();
  }
  async function renderPoster() {
    scenes.forEach((s) => { s.el.style.display = "none"; });
    poster.classList.remove("hidden");
    poster.style.opacity = "1";
    await settle();
  }
  async function settle() {
    while (pending.length) await Promise.all(pending.splice(0));
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  }

  window.prxStage = {
    fps: FPS, total: TOTAL, frames: Math.round(TOTAL * FPS),
    renderAt: renderAt, renderPoster: renderPoster,
    ready: () => settle().then(() => document.fonts.ready),
    // The shot list: scene starts and caption windows, in video seconds.
    shots: () => scenes.map((s) => ({ id: s.id, start: +s.start.toFixed(2), end: +(s.start + s.dur).toFixed(2),
      captions: s.caps.map((c) => ({ from: +(s.start + c.a).toFixed(2), to: +(s.start + c.b).toFixed(2), text: c.text })) })),
  };
})();
