#!/usr/bin/env bash
# Regenerate the raster assets from the SVG masters in this directory.
# Needs rsvg-convert (brew install librsvg) and ImageMagick 7 (magick).
#   AppIcon.iconset/          <- app-icon.svg (fallback for scripts/bundle-macos.sh)
#   social-preview.png        <- social-preview.svg
#   menubar-glyph-states.png  <- menubar-glyph.svg (grayscale contact sheet)
#   ../docs/media/logo*.png   <- logo-readme*.svg (README lockup, 2x, transparent)
#   ../docs/media/social-preview.png  <- copy of social-preview.png
# Usage: render.sh [all|icons|glyph|logo]   (default all = icons + glyph;
#   "glyph" = contact sheet only; "logo" = docs/media only, leaves the iconset alone)
set -euo pipefail
cd "$(dirname "$0")"
what=${1:-all}

render_icons() {
rm -rf AppIcon.iconset && mkdir AppIcon.iconset
# 16 and 32 px come from the pixel-hinted masters; 64 px and up from the 1024 master.
src() { if [ "$1" -le 32 ] && [ -f "app-icon-$1.svg" ]; then echo "app-icon-$1.svg"; else echo app-icon.svg; fi; }
for size in 16 32 128 256 512; do
  rsvg-convert -w "$size" -h "$size" "$(src "$size")" -o "AppIcon.iconset/icon_${size}x${size}.png"
  rsvg-convert -w $((size * 2)) -h $((size * 2)) "$(src $((size * 2)))" -o "AppIcon.iconset/icon_${size}x${size}@2x.png"
done

rsvg-convert -w 1280 -h 640 social-preview.svg -o social-preview.png
}

# Contact sheet: each state at 1x (18 px) and 2x (36 px), enlarged with
# nearest-neighbor so the real pixels show, on a light and a dark menu bar,
# in grayscale. Template ink is black; the dark row inverts it (as AppKit's
# tint does). The states sit 20 pt apart in menubar-glyph.svg. A sixth cell
# shows the attention glyph as the menu bar presents it: with the count as
# the status item's title (approximated here in Helvetica 13 pt).
render_glyph() {
tmp=$(mktemp -d); trap 'rm -rf "$tmp"' EXIT
rsvg-convert -h 18 menubar-glyph.svg -o "$tmp/row1.png"
rsvg-convert -h 36 menubar-glyph.svg -o "$tmp/row2.png"
row() { # $1 = background, $2 = negate flag
  local cells=() scale i
  for scale in 1 2; do
    for i in 0 1 2 3 4; do
      local s=$((18 * scale)) x=$((20 * scale * i))
      magick "$tmp/row$scale.png" -crop "${s}x${s}+${x}+0" +repage $2 \
        -background "$1" -alpha remove -filter point -resize $((800 / scale))% \
        -bordercolor "$1" -border 12 "$tmp/c$scale$i.png"
      cells+=("$tmp/c$scale$i.png")
    done
    local s=$((18 * scale))
    magick "$tmp/row$scale.png" -crop "${s}x${s}+$((20 * scale))+0" +repage \
      -background none -gravity west -extent "$((30 * scale))x${s}" \
      -font Helvetica -pointsize $((13 * scale)) -fill black \
      -annotate "+$((21 * scale))+0" '4' $2 \
      -background "$1" -alpha remove -filter point -resize $((800 / scale))% \
      -bordercolor "$1" -border 12 "$tmp/t$scale.png"
    cells+=("$tmp/t$scale.png")
  done
  magick "${cells[@]:0:6}" +append "$tmp/a.png"
  magick "${cells[@]:6:6}" +append "$tmp/b.png"
  magick "$tmp/a.png" "$tmp/b.png" -append "$3"
}
row '#ECECEC' '' "$tmp/light.png"
row '#1E1E1E' '-channel RGB -negate +channel' "$tmp/dark.png"
magick "$tmp/light.png" "$tmp/dark.png" -append +repage -colorspace Gray \
  -background '#FFFFFF' -gravity north -splice 0x28 \
  -font Helvetica -pointsize 15 -fill '#333333' \
  -annotate +0+6 'idle · attention · reviewing · paused/stopped · offline · attention with its count title — rows: 1x, 2x on light; 1x, 2x on dark' \
  menubar-glyph-states.png
}

# README / public logo: the horizontal lockup at 2x with a transparent
# background. logo.png is the one-file fallback (blue wordmark, reads on light
# and dark); logo-light.png and logo-dark.png are the <picture> sources.
render_logo() {
mkdir -p ../docs/media
rsvg-convert -z 2 logo-readme.svg -o ../docs/media/logo.png
rsvg-convert -z 2 logo-readme-light.svg -o ../docs/media/logo-light.png
rsvg-convert -z 2 logo-readme-dark.svg -o ../docs/media/logo-dark.png
cp social-preview.png ../docs/media/social-preview.png
}

case $what in
  all) render_icons; render_glyph; echo "rendered AppIcon.iconset, social-preview.png, menubar-glyph-states.png" ;;
  icons) render_icons; echo "rendered AppIcon.iconset, social-preview.png" ;;
  glyph) render_glyph; echo "rendered menubar-glyph-states.png" ;;
  logo) render_logo; echo "rendered docs/media/logo.png, logo-light.png, logo-dark.png, social-preview.png" ;;
  *) echo "usage: $0 [all|icons|glyph|logo]" >&2; exit 2 ;;
esac
