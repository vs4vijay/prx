#!/usr/bin/env bash
# build.sh: rebuild the launch demo from scratch.
#
#   scripts/demo-video/build.sh
#
# writes docs/media/demo.mp4, docs/media/demo.gif and docs/media/demo-poster.png.
# Everything on screen is the fictional demo dataset (user maya, orgs acme and
# globex): the terminal shots run the real prx inside scripts/demo-sandbox.sh,
# the window shots are the real settings UI on its mock backend, and the menu
# bar section plays the recorded takes in menu-clips/ (real captures of the
# sandboxed app; see README.md). Nothing here records the screen or starts the
# native app, and intermediate files stay in the build directory. Without the
# clips the build still succeeds and the video has no menu bar section.
#
# Environment:
#   PRX_DEMO_BUILD   build directory            (default /tmp/prx-demo-build)
#   PRX_DEMO_DIR     the prx demo sandbox        (default /tmp/prx-demo-video)
#   PRX_DEMO_PORT    port for the UI preview     (default 4174)
#   PRX_BIN          prx binary for the sandbox  (default target/debug/prx if it
#                    exists, else demo-sandbox.sh builds it)
#   PRX_DEMO_MENU    0 leaves the menu bar section out even if the clips in
#                    scripts/demo-video/menu-clips/ are there
#   PYTHON           a python3 with playwright   (default: Homebrew 3.14, else python3)
#   JOBS             parallel frame renderers    (default 4)
#   STEPS            which steps to run          (default "sandbox term ui stage frames encode")
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
BUILD="${PRX_DEMO_BUILD:-/tmp/prx-demo-build}"
export PRX_DEMO_DIR="${PRX_DEMO_DIR:-/tmp/prx-demo-video}"
PORT="${PRX_DEMO_PORT:-4174}"
JOBS="${JOBS:-4}"
STEPS="${STEPS:-sandbox term ui stage frames encode}"
OUT="$ROOT/docs/media"
if [[ -z "${PYTHON:-}" ]]; then
    PYTHON=/opt/homebrew/opt/python@3.14/bin/python3.14
    [[ -x "$PYTHON" ]] || PYTHON=python3
fi
for tool in node vhs ffmpeg ffprobe "$PYTHON"; do
    command -v "$tool" >/dev/null || { echo "error: $tool is needed" >&2; exit 2; }
done
mkdir -p "$BUILD" "$OUT"
has() { [[ " $STEPS " == *" $1 "* ]]; }

SERVER_PID=""
cleanup() { [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" 2>/dev/null || true; }
trap cleanup EXIT

# --- 1. the demo sandbox (fresh timestamps) -----------------------------------
if has sandbox; then
    echo "== sandbox: $PRX_DEMO_DIR"
    if [[ -z "${PRX_BIN:-}" && -x "$ROOT/target/debug/prx" ]]; then export PRX_BIN="$ROOT/target/debug/prx"; fi
    "$ROOT/scripts/demo-sandbox.sh" "$PRX_DEMO_DIR" >"$BUILD/sandbox.log"
fi

# --- 2. terminal shots (vhs -> PNG frames) --------------------------------------
if has term; then
    echo "== terminal: vhs"
    mkdir -p "$BUILD/term"
    # vhs starts ttyd through the shim, so the recorder's terminal measures
    # emoji the way current terminals do (see ttyd-shim/ttyd). prx's output is
    # not rewritten.
    VHS_PATH="$HERE/ttyd-shim:$PATH"
    cp "$HERE/term-setup.sh" "$BUILD/term/setup.sh"
    for shot in status brief approve; do
        rm -rf "$BUILD/term/$shot"
        (cd "$BUILD/term" && PATH="$VHS_PATH" vhs "$HERE/term-$shot.tape" >"$BUILD/term/$shot.log" 2>&1) \
            || { echo "error: vhs failed, see $BUILD/term/$shot.log" >&2; exit 1; }
    done
fi

# --- 3. window shots (Playwright on the UI preview) -----------------------------
if has ui; then
    echo "== window: preview server on 127.0.0.1:$PORT"
    node "$ROOT/menubar/settings-ui-tests/preview-server.js" "$PORT" >"$BUILD/server.log" 2>&1 &
    SERVER_PID=$!
    sleep 1
    kill -0 "$SERVER_PID" 2>/dev/null || { echo "error: the preview server did not start (port $PORT busy?)" >&2; exit 1; }
    rm -rf "$BUILD/ui"
    "$PYTHON" "$HERE/capture_ui.py" "$BUILD/ui" "http://127.0.0.1:$PORT"
    kill "$SERVER_PID" 2>/dev/null || true
    SERVER_PID=""
fi

# --- 4. the stage: compositor page, brand, manifest ---------------------------
if has stage; then
    echo "== stage"
    mkdir -p "$BUILD/brand"
    cp "$HERE/stage.html" "$HERE/stage.js" "$BUILD/"
    cp "$ROOT/assets/logo-readme-dark.svg" "$BUILD/brand/logo.svg"
    # The menu bar takes: one PNG per video frame, already at stage size.
    rm -rf "$BUILD/menu"
    MENU_CLIPS=""
    if [[ "${PRX_DEMO_MENU:-1}" != 0 && -f "$HERE/menu-clips/clips.json" ]]; then MENU_CLIPS="$HERE/menu-clips"; fi
    "$PYTHON" - "$BUILD" "$MENU_CLIPS" <<'PY'
import json, struct, subprocess, sys
from pathlib import Path
build = Path(sys.argv[1])
def png_size(path):
    with open(path, "rb") as f:
        return struct.unpack(">II", f.read(24)[16:24])
term = {}
for shot in ("status", "brief", "approve"):
    frames = sorted((build / "term" / shot).glob("frame-text-*.png"))
    assert frames, f"no vhs frames for {shot}"
    w, h = png_size(frames[0])
    term[shot] = {"dir": f"term/{shot}", "frames": len(frames), "w": w, "h": h}
menu = None
src = Path(sys.argv[2]) if sys.argv[2] else None
if src:
    spec = json.loads((src / "clips.json").read_text())
    missing = [c["file"] for c in spec["clips"].values() if not (src / c["file"]).is_file()]
    if missing:
        print("   menu clips missing:", ", ".join(missing))
    else:
        scale, clips = spec["scale"], {}
        for name, c in spec["clips"].items():
            out = build / "menu" / name
            out.mkdir(parents=True)
            w, h = round(c["w"] * scale), round(c["h"] * scale)
            subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", "-i", str(src / c["file"]),
                            "-vf", f"scale={w}:{h}:flags=lanczos:in_color_matrix=bt709:in_range=tv:out_range=pc,format=rgb24",
                            "-start_number", "0", str(out / "f_%05d.png")], check=True)
            clips[name] = {"dir": f"menu/{name}", "frames": len(list(out.glob("f_*.png"))),
                           "x": c["x"], "y": c["y"], "w": c["w"], "h": c["h"]}
        menu = {"scale": scale, "clips": clips}
manifest = {"ui": json.loads((build / "ui/meta.json").read_text()), "term": term, "menu": menu}
(build / "manifest.js").write_text("window.MANIFEST = " + json.dumps(manifest, indent=1) + ";\n")
print("   menu bar section:", "included" if menu else "not included")
PY
    "$PYTHON" "$HERE/render.py" "$BUILD" --shots >"$BUILD/shots.json"
fi

# --- 5. frames -----------------------------------------------------------------
if has frames; then
    TOTAL="$("$PYTHON" -c 'import json,sys; print(round(json.load(open(sys.argv[1]))["total"]*30))' "$BUILD/shots.json")"
    echo "== frames: $TOTAL at 1920x1080, $JOBS renderers"
    rm -rf "$BUILD/frames"
    mkdir -p "$BUILD/frames"
    CHUNK=$(( (TOTAL + JOBS - 1) / JOBS ))
    pids=()
    for ((j = 0; j < JOBS; j++)); do
        "$PYTHON" "$HERE/render.py" "$BUILD" --frames "$BUILD/frames" --start $((j * CHUNK)) --end $(((j + 1) * CHUNK)) &
        pids+=($!)
    done
    for pid in "${pids[@]}"; do wait "$pid"; done
    [[ "$(ls "$BUILD/frames" | wc -l | tr -d ' ')" == "$TOTAL" ]] || { echo "error: frames are missing" >&2; exit 1; }
fi

# --- 6. encode: mp4, poster, gif ----------------------------------------------
if has encode; then
    echo "== encode"
    ffmpeg -hide_banner -loglevel error -y -framerate 30 -i "$BUILD/frames/f_%05d.png" \
        -vf "scale=in_range=pc:out_range=tv:out_color_matrix=bt709,format=yuv420p" \
        -c:v libx264 -preset slow -crf "${CRF:-19}" -profile:v high -r 30 \
        -color_primaries bt709 -color_trc bt709 -colorspace bt709 -color_range tv \
        -movflags +faststart -an "$OUT/demo.mp4"

    "$PYTHON" "$HERE/render.py" "$BUILD" --poster "$BUILD/poster.png"
    ffmpeg -hide_banner -loglevel error -y -i "$BUILD/poster.png" -frames:v 1 -update 1 -compression_level 9 "$OUT/demo-poster.png"

    # The GIF: cuts from the same frames, cross-dissolved, fading to the
    # background colour so the loop has no jump: the menu opening on the PR
    # list (when the menu bar section is in the video), then the app window
    # (the Dashboard, a PR's brief, a review's findings). The terminal scenes
    # are left out: a 126-column table is not readable at 800 px. Times are
    # relative to each scene.
    CUTS=()
    while read -r start count; do CUTS+=("$start $count"); done < <("$PYTHON" - "$BUILD/shots.json" <<'PY'
import json, sys
shots = {s["id"]: s for s in json.load(open(sys.argv[1]))["shots"]}
cuts = [("menu", 0.35, 3.75), ("app", 0.4, 5.8), ("app", 6.0, 14.8), ("app", 18.1, 24.3)]
for i, a, b in cuts:
    if i in shots:
        print(round((shots[i]["start"] + a) * 30), round((b - a) * 30))
PY
    )
    X=0.35 FPSG="${GIF_FPS:-15}"
    GIF_IN=() CHAIN="" PREV="[0]" LEN=0
    for i in "${!CUTS[@]}"; do
        read -r start count <<<"${CUTS[$i]}"
        dur=$(echo "$count/30" | bc -l)
        GIF_IN+=(-framerate 30 -start_number "$start" -t "$dur" -i "$BUILD/frames/f_%05d.png")
        if ((i == 0)); then
            LEN=$dur
        else
            CHAIN+="${PREV}[$i]xfade=transition=fade:duration=$X:offset=$(echo "$LEN-$X" | bc -l)[x$i];"
            PREV="[x$i]"
            LEN=$(echo "$LEN+$dur-$X" | bc -l)
        fi
    done
    END=$(echo "$LEN-0.45" | bc -l)
    ffmpeg -hide_banner -loglevel error -y "${GIF_IN[@]}" \
        -filter_complex "${CHAIN}${PREV}fade=t=out:st=$END:d=0.45:color=0x0E0F12,fps=$FPSG,scale=${GIF_WIDTH:-800}:-1:flags=lanczos,split[p][q];\
[p]palettegen=max_colors=${GIF_COLORS:-128}:stats_mode=diff[pal];[q][pal]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle" \
        -loop 0 "$OUT/demo.gif"

    for f in demo.mp4 demo.gif demo-poster.png; do
        printf '   %-16s %s  %s bytes\n' "$f" \
            "$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height,r_frame_rate,pix_fmt:format=duration -of csv=p=0 "$OUT/$f" | tr '\n' ' ')" \
            "$(stat -f %z "$OUT/$f" 2>/dev/null || stat -c %s "$OUT/$f")"
    done
fi
has encode && echo "done: docs/media/demo.mp4, demo.gif, demo-poster.png (shot list: $BUILD/shots.json)" || echo "done: $STEPS"
