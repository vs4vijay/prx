"""Generate the polished PRX brand SVGs (direction C, Verdict X).

Run: uv run --with svgpathtools python assets/explorations/icon-polish-gen.py
Writes app-icon*.svg, wordmark.svg, logo-mark.svg, logo.svg, social-preview.svg,
logo-readme*.svg (README lockup; rasterise with assets/render.sh logo);
reads the tagline letterforms from explorations/previous/. Then run assets/render.sh."""
import math, re
from pathlib import Path
from svgpathtools import parse_path

ROOT = Path(__file__).resolve().parent.parent
PREV = ROOT / "explorations/previous"
OUT = Path(__import__("os").environ.get("OUT", str(ROOT)))

def f(v):
    s = f"{v:.2f}".rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s

# ---------------------------------------------------------------- squircle
def squircle(x, y, w, h, r):
    """Apple continuous-curvature rounded rect (iOS 7+ / Big Sur icon shape; PaintCode coefficients)."""
    # Top-right corner, as (dx from right edge, dy from top) in units of r; mirrored for the others.
    seg = [("L", [(1.52866471, 0)]),
           ("C", [(1.08849323, 0), (0.86840689, 0), (0.66993427, 0.06549600)]),
           ("L", [(0.63149399, 0.07491100)]),
           ("C", [(0.37282392, 0.16905899), (0.16905883, 0.37282401), (0.07491176, 0.63149399)]),
           ("L", [(0.06549569, 0.66993493)]),
           ("C", [(0, 0.86840701), (0, 1.08849299), (0, 1.52866483)])]
    X0, Y0, X1, Y1 = x, y, x + w, y + h
    corners = [lambda a, b: (X1 - a*r, Y0 + b*r), lambda a, b: (X1 - b*r, Y1 - a*r),
               lambda a, b: (X0 + a*r, Y1 - b*r), lambda a, b: (X0 + b*r, Y0 + a*r)]
    out = [f"M {f(X0 + 1.52866483*r)} {f(Y0)}"]
    for C in corners:
        for cmd, pts in seg:
            out.append(cmd + " " + " ".join(f"{f(C(a, b)[0])} {f(C(a, b)[1])}" for a, b in pts))
    out.append("Z")
    return " ".join(out)

# ---------------------------------------------------------------- the mark
# Skeleton on the 24 grid (centerlines). Check A -> V -> B; counter-stroke C1 -> C2.
SK = dict(A=(4.6, 13.2), V=(9.0, 17.6), B=(19.5, 4.6), C1=(4.7, 4.7), C2=(18.8, 18.8))

def verdict(scale, tx, ty, wc, wx, gap, sk=SK):
    T = lambda p: (tx + p[0]*scale, ty + p[1]*scale)
    A, V, B, C1, C2 = (T(sk[k]) for k in ("A", "V", "B", "C1", "C2"))
    u = (C2[0]-C1[0], C2[1]-C1[1]); L = math.hypot(*u); u = (u[0]/L, u[1]/L)
    d = (B[0]-V[0], B[1]-V[1]); Ld = math.hypot(*d); d = (d[0]/Ld, d[1]/Ld)
    # intersection of C1 + t u with V + s d
    den = u[0]*d[1] - u[1]*d[0]
    t = ((V[0]-C1[0])*d[1] - (V[1]-C1[1])*d[0]) / den
    P = (C1[0]+t*u[0], C1[1]+t*u[1])
    sin = abs(den)
    clear = (wc/2)/sin + gap + wx/2   # check edge + gap + round cap
    U = (P[0]-u[0]*clear, P[1]-u[1]*clear)
    Lo = (P[0]+u[0]*clear, P[1]+u[1]*clear)
    pt = lambda p: f"{f(p[0])} {f(p[1])}"
    return dict(check=f"M {pt(A)} L {pt(V)} L {pt(B)}",
                up=f"M {pt(C1)} L {pt(U)}", lo=f"M {pt(Lo)} L {pt(C2)}",
                bbox=(min(A[0], C1[0]) - max(wc, wx)/2, min(B[1], C1[1]) - max(wc, wx)/2,
                      max(B[0], C2[0]) + max(wc, wx)/2, max(V[1] + wc/2, C2[1] + wx/2)))

def fit_verdict(cx, cy, size, wc_u, wx_u, gap_u):
    """Center the stroked mark on (cx, cy); size = stroked width in final units."""
    m = verdict(1, 0, 0, wc_u, wx_u, gap_u)
    x0, y0, x1, y1 = m["bbox"]
    s = size / (x1 - x0)
    tx = cx - (x0 + x1)/2*s; ty = cy - (y0 + y1)/2*s
    m = verdict(s, tx, ty, wc_u*s, wx_u*s, gap_u*s)
    m.update(scale=s, wc=wc_u*s, wx=wx_u*s)
    return m

SK_TYPE = dict(A=(4.6, 14.0), V=(9.0, 18.4), B=(20.0, 4.6), C1=(6.3, 4.6), C2=(20.1, 18.4))

def fit_verdict_box(x0, y0, y1, w, gap, wc=None, sk=SK_TYPE):
    """Fit skeleton so that the stroked mark spans y0..y1 (height) starting at x0."""
    SK = sk
    wc = wc or w
    m = verdict(1, 0, 0, 0, 0, 0)
    sx0 = min(SK["A"][0], SK["C1"][0]); sy0 = min(SK["B"][1], SK["C1"][1])
    sy1 = max(SK["V"][1], SK["C2"][1]); sx1 = max(SK["B"][0], SK["C2"][0])
    s = ((y1 - y0) - w) / (sy1 - sy0)
    tx = x0 + w/2 - sx0*s; ty = y0 + w/2 - sy0*s
    m = verdict(s, tx, ty, wc, w, gap, sk=SK)
    m["right"] = tx + sx1*s + w/2
    return m

# ---------------------------------------------------------------- app icon
def icon_defs(p):
    sq = squircle(100, 100, 824, 824, 185.4)
    return f'''  <defs>
    <path id="{p}sq" d="{sq}"/>
    <clipPath id="{p}clip"><use href="#{p}sq"/></clipPath>
    <linearGradient id="{p}bg" x1="0" y1="100" x2="0" y2="924" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#4C96FF"/>
      <stop offset="0.45" stop-color="#2F7BF5"/>
      <stop offset="1" stop-color="#1459CC"/>
    </linearGradient>
    <radialGradient id="{p}sheen" cx="512" cy="40" r="640" gradientTransform="translate(512 40) scale(1.25 0.8) translate(-512 -40)" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#FFFFFF" stop-opacity="0.22"/>
      <stop offset="0.6" stop-color="#FFFFFF" stop-opacity="0.04"/>
      <stop offset="1" stop-color="#FFFFFF" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="{p}edge" x1="0" y1="100" x2="0" y2="924" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#FFFFFF" stop-opacity="0.42"/>
      <stop offset="0.18" stop-color="#FFFFFF" stop-opacity="0.08"/>
      <stop offset="0.75" stop-color="#001A4D" stop-opacity="0"/>
      <stop offset="1" stop-color="#001A4D" stop-opacity="0.32"/>
    </linearGradient>
    <linearGradient id="{p}counter" x1="0" y1="250" x2="0" y2="780" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#C0D9FF"/>
      <stop offset="1" stop-color="#9DBEF5"/>
    </linearGradient>
    <filter id="{p}drop" x="-12%" y="-12%" width="124%" height="130%" color-interpolation-filters="sRGB">
      <feGaussianBlur in="SourceAlpha" stdDeviation="3"/>
      <feOffset dy="3" result="contact"/>
      <feGaussianBlur in="SourceAlpha" stdDeviation="16"/>
      <feOffset dy="14" result="ambient"/>
      <feComponentTransfer in="contact" result="c"><feFuncA type="linear" slope="0.22"/></feComponentTransfer>
      <feComponentTransfer in="ambient" result="a"><feFuncA type="linear" slope="0.26"/></feComponentTransfer>
      <feMerge><feMergeNode in="a"/><feMergeNode in="c"/><feMergeNode in="SourceGraphic"/></feMerge>
    </filter>
    <filter id="{p}lift" x="-20%" y="-20%" width="140%" height="150%" color-interpolation-filters="sRGB">
      <feDropShadow dx="0" dy="9" stdDeviation="11" flood-color="#032A6B" flood-opacity="0.34"/>
    </filter>
  </defs>'''

MARK = fit_verdict(512, 506, 486, 3.0, 2.8, 1.15)

def icon_body(p):
    m = MARK
    return f'''  <use href="#{p}sq" fill="url(#{p}bg)" filter="url(#{p}drop)"/>
  <use href="#{p}sq" fill="url(#{p}sheen)"/>
  <use href="#{p}sq" fill="none" stroke="url(#{p}edge)" stroke-width="5" clip-path="url(#{p}clip)"/>
  <g filter="url(#{p}lift)" fill="none" stroke-linecap="round" stroke-linejoin="round">
    <g stroke="url(#{p}counter)" stroke-width="{f(m['wx'])}">
      <path d="{m['up']}"/>
      <path d="{m['lo']}"/>
    </g>
    <path d="{m['check']}" stroke="#FFFFFF" stroke-width="{f(m['wc'])}"/>
  </g>'''

def app_icon():
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024" role="img" aria-labelledby="title desc">
  <title id="title">PRX app icon: Verdict X</title>
  <desc id="desc">The x of prx, drawn as a check (approve) crossed by an interrupted counter-stroke (request changes). The gap where they would meet is the human's call: AI informs, you decide. White on the system-blue accent. 1024 grid: Apple continuous-corner squircle on the 824 px content box (100 px margin, corner radius 185.4), top-lit gradient, hairline edge, soft two-layer shadow. Master for 64 px and up; app-icon-32.svg and app-icon-16.svg are the pixel-hinted small sizes.</desc>
{icon_defs("")}
{icon_body("")}
</svg>
'''

# ---------------------------------------------------------------- hinted small icons
def small_icon(N):
    if N == 32:
        box, r, sh = (3, 2, 26, 26), 5.85, (0.6, 0.7, 0.35)
        mark = dict(A=(9.5, 16.0), V=(13.0, 19.5), B=(22.5, 8.5), C1=(9.5, 9.0), C2=(21.5, 21.0))
        wc, wx, gap = 2.6, 2.3, 1.35
        cnt = "#B4D0FF"
    else:
        box, r, sh = (1, 1, 14, 14), 3.15, (0.35, 0.45, 0.4)
        mark = dict(A=(3.75, 8.25), V=(6.5, 11.0), B=(12.0, 4.75), C1=(4.5, 4.5), C2=(11.5, 11.5))
        wc, wx, gap = 1.6, 1.45, 0.85
        cnt = "#C2D9FF"
    m = verdict(1, 0, 0, wc, wx, gap, sk=mark)
    x, y, w, h = box
    sq = squircle(x, y, w, h, r)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {N} {N}" width="{N}" height="{N}" role="img" aria-labelledby="title">
  <title id="title">PRX app icon, pixel-hinted {N} px master (Verdict X)</title>
  <!-- Drawn on the {N} px grid: heavier strokes, a wider gap, an opaque counter-stroke tint
       and a simplified squircle, so the mark stays crisp where the 1024 master would blur.
       Used for icon_{N}x{N}.png{" and icon_16x16@2x.png" if N == 32 else ""}. -->
  <defs>
    <linearGradient id="bg" x1="0" y1="{f(y)}" x2="0" y2="{f(y+h)}" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#4590FC"/>
      <stop offset="1" stop-color="#175DCF"/>
    </linearGradient>
    <linearGradient id="edge" x1="0" y1="{f(y)}" x2="0" y2="{f(y+h)}" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#FFFFFF" stop-opacity="0.35"/>
      <stop offset="0.3" stop-color="#FFFFFF" stop-opacity="0"/>
      <stop offset="1" stop-color="#001A4D" stop-opacity="0.25"/>
    </linearGradient>
    <clipPath id="clip"><path d="{sq}"/></clipPath>
    <filter id="drop" x="-20%" y="-20%" width="140%" height="150%">
      <feDropShadow dx="0" dy="{sh[1]}" stdDeviation="{sh[0]}" flood-color="#000000" flood-opacity="{sh[2]}"/>
    </filter>
  </defs>
  <path d="{sq}" fill="url(#bg)" filter="url(#drop)"/>
  <path d="{sq}" fill="none" stroke="url(#edge)" stroke-width="{f(1.0 if N == 32 else 0.8)}" clip-path="url(#clip)"/>
  <g fill="none" stroke-linecap="round" stroke-linejoin="round">
    <g stroke="{cnt}" stroke-width="{f(wx)}">
      <path d="{m['up']}"/>
      <path d="{m['lo']}"/>
    </g>
    <path d="{m['check']}" stroke="#FFFFFF" stroke-width="{f(wc)}"/>
  </g>
</svg>
'''

# ---------------------------------------------------------------- letterforms
def load_glyphs():
    lib = {}
    def take(svgfile, start_marker, text):
        src = (PREV / svgfile).read_text()
        seg = src[src.index(start_marker):]
        seg = seg[:seg.index("</g>\n  </g>")]
        order, paths = [], {}
        for off, d in re.findall(r'translate\(([0-9.]+) 0\)" d="([^"]*)"', seg):
            if off not in paths: order.append(off); paths[off] = []
            paths[off].append(d)
        chars = [c for c in text if c != " "]
        assert len(chars) == len(order), (svgfile, len(chars), len(order))
        for c, off in zip(chars, order):
            lib.setdefault(c, paths[off])
    take("social-preview.svg", "translate(469.0 524.4)", "the Pull Request eXaminer")
    take("logo.svg", "translate(100.3 439.6)", "AI informs, human decides.")
    return lib

GLYPHS = load_glyphs()
CLS = {  # (left, right) side class: s straight, r round, o open/diagonal
    "t": "hh", "h": "ss", "e": "rh", "P": "sr", "u": "ss", "l": "ss", "R": "so", "q": "rs",
    "s": "rr", "X": "oo", "a": "rs", "m": "ss", "i": "ss", "n": "ss", "r": "sh", "A": "oo",
    "I": "ss", "f": "hh", "o": "rr", ",": "oo", "d": "rs", "c": "rh", ".": "oo", "p": "sr", "x": "oo"}
ADJ = {"s": 0.0, "h": -0.25, "r": -0.45, "o": -0.75}

def ink(paths, w):
    xs = []
    for d in paths:
        b = parse_path(d).bbox(); xs += [b[0], b[1]]
    return min(xs) - w/2, max(xs) + w/2

def set_text(text, w, gap, space=5.2, verdict_x=False, color="currentColor"):
    """Lay out text in the stroke letterforms with optical spacing. Returns (svg_paths, width)."""
    out, x, prev = [], 0.0, None
    for c in text:
        if c == " ":
            x += space; prev = None; continue
        if c == "X" and verdict_x:
            gx0 = x if prev is None else x + gap + ADJ[CLS[prev][1]] + ADJ["o"]
            m = fit_verdict_box(gx0, -14 - w/2, w/2, w, 0.55*w)
            for k in ("check", "up", "lo"):
                out.append(f'<path d="{m[k]}"/>')
            x = m["right"]; prev = "X"; continue
        paths = GLYPHS[c]
        a, b = ink(paths, w)
        if prev is None: ox = x - a
        else:
            g = gap + ADJ[CLS[prev][1]] + ADJ[CLS[c][0]]
            if c in ",.": g = 0.55*gap
            ox = x + g - a
        for d in paths:
            out.append(f'<path transform="translate({f(ox)} 0)" d="{d}"/>')
        x = ox + b; prev = c
    return out, x

# ---------------------------------------------------------------- wordmark
P_GLYPH = ["M 1.25 -10 L 1.25 5", "M 8.75 -5 A 3.75 3.75 0 1 1 1.25 -5 A 3.75 3.75 0 1 1 8.75 -5"]
R_GLYPH = ["M 1.25 -10 L 1.25 0", "M 1.25 -5.1 A 4.75 4.75 0 0 1 6.0 -9.85 L 7.2 -9.85"]

def wordmark_paths():
    w = 2.5
    out = [f'<path d="{d}"/>' for d in P_GLYPH]
    # p ink right edge 10.0; r stem ink left edge = ox + 0  -> gap 1.25 (round -> straight)
    rx = 11.25
    out += [f'<path transform="translate({f(rx)} 0)" d="{d}"/>' for d in R_GLYPH]
    r_right = rx + 7.2 + w/2
    # Verdict X sized to the x-height with a 0.2 overshoot top and bottom
    m = fit_verdict_box(r_right + 1.25, -10.2, 0.2, w, 0.5*w, wc=2.6)
    out += [f'<path d="{m[k]}"/>' for k in ("up", "lo", "check")]
    return out, m["right"]

def wordmark():
    paths, right = wordmark_paths()
    body = "\n      ".join(paths)
    vb_w = right + 0.0
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -11.5 {f(vb_w)} 18" width="{f(vb_w*16)}" height="288" role="img" aria-labelledby="title desc" color="#18181B">
  <title id="title">PRX wordmark</title>
  <desc id="desc">prx set in the PRX stroke letterforms (2.5 stroke, x-height 10), with the x drawn as the Verdict X: a check crossed by an interrupted counter-stroke, fitted to the x-height with a small optical overshoot and an even gap on both sides of the crossing. Strokes use currentColor (default #18181B), so inline use can recolor it for dark backgrounds.</desc>
  <g fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
      {body}
  </g>
</svg>
'''

def logo_mark():
    m = fit_verdict(12, 12, 18.2, 2.75, 2.5, 1.1)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" role="img" aria-labelledby="t">
  <title id="t">PRX: the Verdict X</title>
  <g fill="none" stroke="#18181B" stroke-linecap="round" stroke-linejoin="round">
    <path d="{m['up']}" stroke-width="{f(m['wx'])}"/>
    <path d="{m['lo']}" stroke-width="{f(m['wx'])}"/>
    <path d="{m['check']}" stroke-width="{f(m['wc'])}"/>
  </g>
</svg>
'''

def nested_icon(p, x, y, size):
    return f'''  <svg x="{f(x)}" y="{f(y)}" width="{f(size)}" height="{f(size)}" viewBox="0 0 1024 1024">
{icon_defs(p)}
{icon_body(p)}
  </svg>'''

def logo():
    W = 512
    wm, wm_w = wordmark_paths()
    s_wm = 7.0
    tag, tag_w = set_text("AI informs, human decides.", 2.0, 2.6, space=6.0)
    s_tag = min(1.08, (wm_w*s_wm)/tag_w)
    icon = 200
    # vertical rhythm: icon tile (visible 824/1024) -> 40 -> wordmark x-height -> 30 -> tagline
    def place(tile_top):
        tile_bottom = tile_top + icon*824/1024
        wm_base = tile_bottom + 46 + 10*s_wm + 1.5
        tag_base = wm_base + 5*s_wm + 30 + 10*s_tag
        return wm_base, tag_base
    tile_top = (W - (place(0)[1] + 1.0*s_tag)) / 2
    iy = tile_top - icon*100/1024
    wm_base, tag_base = place(tile_top)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {W}" width="{W}" height="{W}" role="img" aria-labelledby="t">
  <title id="t">PRX: Verdict X app icon, wordmark, tagline</title>
  <rect width="{W}" height="{W}" fill="#FFFFFF"/>
{nested_icon("i-", (W-icon)/2, iy, icon)}
  <g transform="translate({f((W - wm_w*s_wm)/2)} {f(wm_base)}) scale({f(s_wm)})">
    <g fill="none" stroke="#18181B" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
      {chr(10).join("      " + l for l in wm).strip()}
    </g>
  </g>
  <g transform="translate({f((W - tag_w*s_tag)/2)} {f(tag_base)}) scale({f(s_tag)})">
    <g fill="none" stroke="#52525B" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      {chr(10).join("      " + l for l in tag).strip()}
    </g>
  </g>
</svg>
'''

def social():
    W, H = 1280, 640
    wm, wm_w = wordmark_paths()
    s_wm = 11.0
    l1, l1w = set_text("the Pull Request eXaminer", 2.1, 2.6, space=6.0, verdict_x=True)
    l2, l2w = set_text("AI informs, human decides.", 2.1, 2.6, space=6.0)
    s1 = 1.55; s2 = 1.3
    icon = 216
    tile_top = 70; iy = tile_top - icon*100/1024
    tile_bottom = tile_top + icon*824/1024
    wm_base = tile_bottom + 52 + 10*s_wm
    l1_base = wm_base + 5*s_wm + 46 + 14*s1 - 4*s1
    l2_base = l1_base + 30 + 10*s2
    g = lambda paths: chr(10).join("      " + l for l in paths).strip()
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" role="img" aria-labelledby="t">
  <title id="t">PRX: the Pull Request eXaminer</title>
  <defs>
    <radialGradient id="glow" cx="640" cy="{f(tile_top + icon*0.4)}" r="420" gradientTransform="translate(640 {f(tile_top + icon*0.4)}) scale(1.6 1) translate(-640 -{f(tile_top + icon*0.4)})" gradientUnits="userSpaceOnUse">
      <stop offset="0" stop-color="#2F7BF5" stop-opacity="0.20"/>
      <stop offset="0.55" stop-color="#2F7BF5" stop-opacity="0.05"/>
      <stop offset="1" stop-color="#2F7BF5" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="{W}" height="{H}" fill="#0E0F12"/>
  <rect width="{W}" height="{H}" fill="url(#glow)"/>
{nested_icon("i-", (W-icon)/2, iy, icon)}
  <g transform="translate({f((W - wm_w*s_wm)/2)} {f(wm_base)}) scale({f(s_wm)})">
    <g fill="none" stroke="#F4F4F5" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
      {g(wm)}
    </g>
  </g>
  <g transform="translate({f((W - l1w*s1)/2)} {f(l1_base)}) scale({f(s1)})">
    <g fill="none" stroke="#F4F4F5" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
      {g(l1)}
    </g>
  </g>
  <g transform="translate({f((W - l2w*s2)/2)} {f(l2_base)}) scale({f(s2)})">
    <g fill="none" stroke="#A1A1AA" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">
      {g(l2)}
    </g>
  </g>
</svg>
'''

# ---------------------------------------------------------------- README lockup
# Horizontal lockup for the README hero: icon tile on the left; wordmark and
# tagline on the right, set so the text block is exactly as tall and the tagline
# exactly as wide as the wordmark. Transparent background, three ink variants:
#   light  ink on GitHub light (#ffffff)       dark  near-white on GitHub dark (#0d1117)
#   both   brand-blue wordmark and mid-gray tagline, legible on either (the <img> fallback)
LOCKUP_INK = {
    "light": ("#18181B", "#52525B"),
    "dark": ("#F4F4F5", "#A1A1AA"),
    "both": ("#2F7BF5", "#7F848E"),
}

def readme_logo(variant):
    wm_ink, tag_ink = LOCKUP_INK[variant]
    wm, wm_w = wordmark_paths()
    tag, tag_w = set_text("AI informs, human decides.", 2.0, 2.6, space=6.0)
    T = 160.0                      # visible tile edge
    icon = T * 1024 / 824          # nested icon box (tile + shadow margin)
    k = wm_w / tag_w               # tagline scale per unit of wordmark scale (equal widths)
    lead = 2.6                     # descender bottom -> tagline cap top, in wordmark units
    # block height: wordmark ink (-11.25 .. 6.25) + lead + tagline (cap 14, stroke 2)
    s_wm = T / (17.5 + lead + 16*k)
    s_tag = k * s_wm
    gap = 38.0                     # tile -> wordmark
    px, pt, pb = 12.0, 8.0, 16.0   # padding = room for the tile's shadow; equal left and right
    W = px + T + gap + wm_w*s_wm + px
    H = pt + T + pb
    tx = px + T + gap
    wm_base = pt + 11.25*s_wm
    tag_base = pt + T - 1.0*s_tag
    g = lambda paths: chr(10).join("      " + l for l in paths).strip()
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {f(W)} {f(H)}" width="{f(W)}" height="{f(H)}" role="img" aria-labelledby="t">
  <title id="t">prx: AI informs, human decides.</title>
{nested_icon("i-", px - icon*100/1024, pt - icon*100/1024, icon)}
  <g transform="translate({f(tx)} {f(wm_base)}) scale({f(s_wm)})">
    <g fill="none" stroke="{wm_ink}" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
      {g(wm)}
    </g>
  </g>
  <g transform="translate({f(tx)} {f(tag_base)}) scale({f(s_tag)})">
    <g fill="none" stroke="{tag_ink}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
      {g(tag)}
    </g>
  </g>
</svg>
'''

OUT.mkdir(parents=True, exist_ok=True)
(OUT / "logo-readme-light.svg").write_text(readme_logo("light"))
(OUT / "logo-readme-dark.svg").write_text(readme_logo("dark"))
(OUT / "logo-readme.svg").write_text(readme_logo("both"))
(OUT / "app-icon.svg").write_text(app_icon())
(OUT / "app-icon-32.svg").write_text(small_icon(32))
(OUT / "app-icon-16.svg").write_text(small_icon(16))
(OUT / "wordmark.svg").write_text(wordmark())
(OUT / "logo-mark.svg").write_text(logo_mark())
(OUT / "logo.svg").write_text(logo())
(OUT / "social-preview.svg").write_text(social())
print("mark scale", round(MARK["scale"], 2), "bbox", [round(v) for v in MARK["bbox"]])
