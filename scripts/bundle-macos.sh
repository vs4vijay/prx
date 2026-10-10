#!/usr/bin/env bash
# Build PRX.app (design A5): one app bundle with the menu bar UI as
# Contents/MacOS/PRX and the CLI/daemon as Contents/Helpers/prx, ad-hoc
# signed for local use (no Apple developer account; no notarization).
# (The CLI cannot sit beside the UI as MacOS/prx: on the default
# case-insensitive APFS volume, "prx" and "PRX" are the same file.)
#
#   scripts/bundle-macos.sh                  build, then install to ~/Applications
#   scripts/bundle-macos.sh --install DIR    build, then install to DIR
#   scripts/bundle-macos.sh --no-install     build only (prints the bundle path)
#
# Honors CARGO_TARGET_DIR. The bundle is assembled under
# <target>/bundle.noindex/PRX.app. The icon comes from assets/app-icon.svg (needs
# rsvg-convert), else a committed assets/AppIcon.iconset, else the
# placeholder scripts/macos/placeholder-icon.svg (needs rsvg-convert).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_DIR="$HOME/Applications"
INSTALL=1

while [ $# -gt 0 ]; do
  case "$1" in
    --install)
      [ $# -ge 2 ] || { echo "error: --install needs a directory" >&2; exit 2; }
      INSTALL_DIR="$2"; shift 2 ;;
    --no-install) INSTALL=0; shift ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 2 ;;
  esac
done

[ "$(uname -s)" = "Darwin" ] || { echo "error: PRX.app is macOS-only" >&2; exit 1; }
cd "$ROOT"

echo "==> cargo build --release --workspace"
cargo build --release --workspace

# Target dir and version from cargo metadata (plutil reads JSON: no jq).
META="$(mktemp)"
trap 'rm -f "$META"' EXIT
cargo metadata --format-version 1 --no-deps > "$META"
TARGET_DIR="$(plutil -extract target_directory raw -o - "$META")"
VERSION=""
i=0
while name="$(plutil -extract "packages.$i.name" raw -o - "$META" 2>/dev/null)"; do
  if [ "$name" = "prx" ]; then
    VERSION="$(plutil -extract "packages.$i.version" raw -o - "$META")"
    break
  fi
  i=$((i + 1))
done
[ -n "$VERSION" ] || { echo "error: cannot read the prx version from cargo metadata" >&2; exit 1; }

RELEASE="$TARGET_DIR/release"
# ".noindex": Spotlight indexes only the installed copy, not the build one.
OUT="$TARGET_DIR/bundle.noindex"
APP="$OUT/PRX.app"
echo "==> assembling $APP (v$VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Helpers" "$APP/Contents/Resources"
cp "$RELEASE/prx-menubar" "$APP/Contents/MacOS/PRX"
cp "$RELEASE/prx" "$APP/Contents/Helpers/prx"
sed "s/@VERSION@/$VERSION/g" "$ROOT/scripts/macos/Info.plist.in" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null
printf 'APPL????' > "$APP/Contents/PkgInfo"

# Icon: SVG master > committed iconset > placeholder.
ICONSET="$OUT/AppIcon.iconset"
render_iconset() {
  local svg="$1"
  rm -rf "$ICONSET"
  mkdir -p "$ICONSET"
  local size
  # 16 and 32 px use the pixel-hinted masters (app-icon-16.svg, app-icon-32.svg) when present.
  src() { local h="${svg%.svg}-$1.svg"; if [ "$1" -le 32 ] && [ -f "$h" ]; then echo "$h"; else echo "$svg"; fi; }
  for size in 16 32 128 256 512; do
    rsvg-convert -w "$size" -h "$size" "$(src "$size")" -o "$ICONSET/icon_${size}x${size}.png"
    rsvg-convert -w $((size * 2)) -h $((size * 2)) "$(src $((size * 2)))" -o "$ICONSET/icon_${size}x${size}@2x.png"
  done
}
ICON_SOURCE=""
if [ -f "$ROOT/assets/app-icon.svg" ] && command -v rsvg-convert >/dev/null; then
  render_iconset "$ROOT/assets/app-icon.svg"; ICON_SOURCE="assets/app-icon.svg"
elif [ -d "$ROOT/assets/AppIcon.iconset" ]; then
  rm -rf "$ICONSET"; cp -R "$ROOT/assets/AppIcon.iconset" "$ICONSET"; ICON_SOURCE="assets/AppIcon.iconset"
elif command -v rsvg-convert >/dev/null; then
  render_iconset "$ROOT/scripts/macos/placeholder-icon.svg"; ICON_SOURCE="placeholder"
fi
if [ -n "$ICON_SOURCE" ]; then
  iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"
  echo "==> icon from $ICON_SOURCE"
else
  echo "warning: no icon (install rsvg-convert: brew install librsvg); the bundle uses the generic app icon" >&2
fi

echo "==> ad-hoc signing"
codesign --force --deep -s - "$APP"
codesign -v "$APP"

if [ "$INSTALL" -eq 0 ]; then
  echo "built $APP"
  exit 0
fi

DEST="$INSTALL_DIR/PRX.app"
echo "==> installing to $DEST"
mkdir -p "$INSTALL_DIR"
rm -rf "$DEST"
ditto "$APP" "$DEST"
# Refresh Launch Services and Spotlight so "PRX" is found right away.
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
[ -x "$LSREGISTER" ] && "$LSREGISTER" -f "$DEST" || true
mdimport "$DEST" 2>/dev/null || true
echo "installed $DEST"
echo "next: open \"$DEST\"   (then: prx daemon install, to start PRX at login)"
