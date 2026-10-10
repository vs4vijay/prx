# PRX visual identity

## Chosen direction: C, "Verdict X"

Chosen by the user at the task 7.1 checkpoint (OpenSpec change
`prx-desktop-app-and-briefs`, design V1).

The mark is the x of "prx", drawn as a check (approve) crossed by a
counter-stroke (request changes). The counter-stroke is interrupted where the
two would meet. That gap is the human's call: AI informs, you decide. The mark
reads as a letter, a verdict, and a symbol. The app icon is the white mark on
the app's system-blue accent (`#2F7BF5`, lit from `#4C96FF` at the top to
`#1459CC` at the bottom), so the icon and the window share one color.

Directions not chosen (kept in `explorations/`). The user picked C; the notes
below are the trade-offs seen during exploration, not the user's stated reasons:

- **A, "Loop, evolved"**: the current interrupted loop, tightened, with the
  check breaking out of the ring. It kept continuity with the old README and
  social art, but a ring is common among round menu-bar icons, and the loop
  says "polling" more than "review".
- **B, "Proof P"**: a P monogram with an open bowl and a proofreader's tick in
  editor's red on warm paper. It says "PR, reviewed", but not the human
  verdict, and its color story (paper and red) departs from the app's
  system-blue accent.

`explorations/contact-sheet.svg` shows all three directions side by side.

## Files

| File | What it is |
| --- | --- |
| `app-icon.svg` | App icon master for 64 px and up: 1024 grid, Apple continuous-corner squircle on the 824 px content box (radius 185.4), 100 px margin for the shadow. |
| `app-icon-32.svg`, `app-icon-16.svg` | Pixel-hinted small-size masters, drawn on their own 32 and 16 px grids. They render `icon_32x32`, `icon_16x16@2x` and `icon_16x16`. |
| `AppIcon.iconset/` | Committed PNG fallback (16 to 512 at 1x and 2x) for builds without `rsvg-convert`. |
| `menubar-glyph.svg` | Menu-bar template glyphs, 18 pt, one group per state: `idle`, `attention`, `reviewing`, `paused`, `offline`. |
| `menubar-glyph-states.png` | Grayscale contact sheet of the five states at 1x and 2x on light and dark menu bars, plus attention with its count title. |
| `wordmark.svg` | "prx" in the stroke letterforms with the Verdict X. Uses `currentColor` (default `#18181B`). |
| `logo-mark.svg` | The bare mark in ink, 24 grid. |
| `logo.svg` | Icon, wordmark, and tagline on white. |
| `social-preview.svg`, `.png` | 1280x640 GitHub social preview. |
| `render.sh` | Regenerates every PNG above from the SVG masters. |
| `explorations/previous/` | The icon, logo, wordmark, social preview and iconset before the polish pass, for comparison. |
| `explorations/icon-polish-compare.png` | Before/after sheet: Dock (256 px), 128, 32 and 16 px, plus magnified 32 and 16, on light and dark. |
| `explorations/icon-polish-gen.py` | Generator for the polished SVGs (mark geometry, squircle, letter spacing). |

## Polish pass (direction unchanged)

A craft pass on direction C against macOS Big Sur and later app-icon
conventions. Compare with `explorations/icon-polish-compare.png`; the old
files are in `explorations/previous/`.

- **Shape.** The squircle is now Apple's continuous-curvature corner (the
  iOS 7 / Big Sur curve), not a generic superellipse, on the same 824/1024
  grid. The tile lines up with system icons in the Dock and Finder.
- **Light and depth.** The flat two-stop gradient (`#2F7BF5` to `#0A4DB5`)
  read heavier and darker than the system blues (Mail, App Store) next to it.
  It is now a brighter, top-lit three-stop gradient through the same accent,
  with a soft top sheen, a hairline edge (light at the top, dark at the
  bottom) so the tile holds on dark Docks, and a two-layer contact and
  ambient shadow. The mark has a soft lift shadow. It is still flat, not glossy.
- **The gap.** The gap was off-center: 0.8 units of clearance above the
  check, 0.34 below, so it nearly closed at small sizes. The gap is now
  computed from the stroke widths and crossing angle, and it is equal on
  both sides.
- **Mark balance.** The stroked mark is centered on the tile with a 6 px
  optical lift. It is about 59% of the tile width (was 57%). The
  counter-stroke's lower segment is longer, so it reads as a stroke, not a
  dot. The counter-stroke is an opaque light-blue tint, not 60% white, so it
  stays crisp at small sizes and the lift shadow doesn't show through it.
- **16 and 32 px.** Each has a hand-hinted master: the tile snaps to whole
  pixels, the strokes are heavier, the gap is wider, and the check's short arm
  is moved off the counter-stroke so they don't merge. The 16 px icon now
  reads as a check and a crossing stroke, not a blur.
- **Wordmark.** The x is refitted to the x-height with a 0.2 overshoot. It
  used to rise 0.8 above the x-height and drop 0.85 below the baseline. The
  x's crossing is moved toward the center, so at text weight both
  counter-stroke pieces read as strokes. The p to r and r to x spacing is
  evened out.
- **Lockups.** The tagline is now 2.0 strokes (was 2.5) in a muted gray,
  centered under the wordmark and no wider than it. It is re-spaced from
  measured glyph ink, which fixes collisions such as `fo` and `rm` (`informs`
  used to read as `inforns`). The social preview sets the X of "eXaminer" as
  a cap-height Verdict X and adds a faint accent glow behind the icon. The
  logo is centered vertically on its canvas.

macOS 26 and later can draw icons with Liquid Glass effects. This icon is
still a flat, Big Sur-style `.icns`. It fills the standard squircle exactly,
so the system masks it cleanly. A native Icon Composer `.icon` would be the
follow-up.

## README / public logo

The README hero and the link-unfurl card live in `docs/media/`. They are
rendered from masters in this directory, so edit the masters, not the PNGs.

| File | Use | Source |
| --- | --- | --- |
| `docs/media/logo-light.png` | README hero on light themes: ink wordmark (`#18181B`), gray tagline (`#52525B`). | `logo-readme-light.svg` |
| `docs/media/logo-dark.png` | README hero on dark themes: near-white wordmark (`#F4F4F5`), tagline `#A1A1AA`. | `logo-readme-dark.svg` |
| `docs/media/logo.png` | One-file fallback for places that can't switch on theme (the `<img>` inside `<picture>`, package registries, launch posts). Brand-blue wordmark (`#2F7BF5`) and mid-gray tagline (`#7F848E`), legible on both white and `#0d1117`. | `logo-readme.svg` |
| `docs/media/social-preview.png` | 1280x640 GitHub social card (Settings, Social preview) and link unfurls. A copy of `social-preview.png`. | `social-preview.svg` |

The lockup is the app-icon tile, the wordmark, and the tagline. The text block
is as tall as the tile and the tagline is as wide as the wordmark. The PNGs are
911x368 (2x of a 455x184 layout) with a transparent background; show them at
about 420 px wide. The padding is equal on the left and right (it holds the
tile's shadow), so the lockup centers correctly. `logo-light.png` and
`logo-dark.png` each vanish on the other theme, so use them only in a
`<picture>` with `prefers-color-scheme` sources. `logo-mark.svg` is ink with
no background; do not use it as a standalone logo on GitHub.

Regenerate:

```sh
uv run --with svgpathtools python assets/explorations/icon-polish-gen.py  # only after layout or color changes; rewrites logo-readme*.svg
assets/render.sh logo   # docs/media/*.png only; does not touch AppIcon.iconset (needs rsvg-convert)
```

## Menu-bar states

States differ by shape, never by color, so they survive template tinting and
grayscale. Idle is the full mark. Every other state drops the counter-stroke's
lower segment and puts a badge in the bottom-right corner, knocked out 1 pt
from the mark:

- **Idle**: the Verdict X.
- **Attention**: a solid dot.
- **Reviewing**: three dots.
- **Paused / stopped**: pause bars (a pause, or review mode `off`).
- **Offline**: the mark at 45% opacity, plus a slashed ring.

The attention count is not drawn into the glyph. The app sets it as the status
item's title, in the menu bar font beside the glyph (`99+` cap). See
`docs/internal/design-audit.md` Part 0.

`menubar/src/icon.rs` rasterizes the same geometry into a template `NSImage`
(18, 36, and 54 px). A unit test pins its constants to `menubar-glyph.svg`, so
change both together.

## Regenerating

```sh
uv run --with svgpathtools python assets/explorations/icon-polish-gen.py  # only after geometry changes
assets/render.sh icons   # needs rsvg-convert (brew install librsvg) and magick
assets/render.sh glyph   # only menubar-glyph-states.png (all: both)
```

`scripts/bundle-macos.sh` renders the `.icns` from `app-icon.svg` when
`rsvg-convert` is present, using `app-icon-16.svg` and `app-icon-32.svg` for
the 16 and 32 px slots, and otherwise copies `AppIcon.iconset/`.
