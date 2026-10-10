# Demo video

Rebuilds `docs/media/demo.mp4`, `demo.gif` and `demo-poster.png`:

```bash
scripts/demo-video/build.sh
```

Needs `node`, `vhs` (with `ttyd`), `ffmpeg` and a Python with Playwright and its Chromium
(`PYTHON=/path/to/python3` if it is not the Homebrew 3.14 one). Takes about four minutes,
most of it rendering frames.
Intermediate files go to `/tmp/prx-demo-build`; the options are at the top of `build.sh`.

Everything on screen is the fictional demo dataset (user `maya`, orgs `acme` and `globex`). The
build never records the screen and never starts the native app. The menu bar section is the one
part it cannot generate: it plays takes that were recorded once and are checked in under
`menu-clips/`.

| File | What it does |
|------|--------------|
| `build.sh` | The whole pipeline: sandbox, terminal, window, frames, encode. `STEPS="stage frames encode"` reruns only the later steps. |
| `term-*.tape`, `term-setup.sh` | vhs recordings of the real `prx` inside `scripts/demo-sandbox.sh`: `status`, `brief`, `approve`. |
| `ttyd-shim/ttyd` | Stands in for `ttyd` while vhs records, so the recorder's terminal measures emoji like a current terminal (see below). |
| `capture_ui.py` | Playwright captures of the settings UI on its mock backend (`menubar/settings-ui-tests/preview-server.js`) at 1040x700, 2x. |
| `stage.html`, `stage.js` | The compositor and the timeline: scenes, captions, pointer, camera. Edit captions and timing here. |
| `render.py` | Steps the stage one frame at a time and screenshots it; also renders the poster and prints the shot list. |
| `menu-clips/` | The recorded menu bar takes (H.264, 30 fps, 2 pixels per screen point, no audio, about 2 MB together) and `clips.json`, which says where each one sits on the captured screen. |
| `menu-capture.swift` | The tool the takes were captured with. Not run by the build; see below. |

Two things to know before changing it:

- The terminal scenes show `prx`'s real output. `term-setup.sh` only drops stderr (the "daemon
  not running" note). `prx status` pads its columns by terminal display width, counting every
  status emoji as two cells, as current terminals do. The xterm.js that vhs records with counts
  an emoji written with a variation selector (the warning sign, the bin, the tick) as one cell,
  so `build.sh` starts ttyd through `ttyd-shim/ttyd`, which corrects that width in the
  recorder's terminal and leaves the output alone. It fails if a new ttyd no longer matches the
  patch; fix the shim then, do not re-pad the output.
- `demo.gif` is cut from the menu bar scene (the menu opening) and the app-window scene; the
  status table is too wide to read at 800 px. Without the menu clips it is the app window only.

## The menu bar section

`menu-clips/` holds four takes of the real menu, captured from a sandbox app that shows only the
demo data, over a flat `#1E1E1E` backdrop:

| Clip | Shows | Cut from |
|------|-------|----------|
| `1-open.mp4` | The pointer goes up, the menu opens on the grouped list. | the "open" take, 0.4 s to the end |
| `2-pr-actions.mp4` | The pointer reaches `acme/web#1288`, its submenu opens to the left, the pointer rests on *Approve…*. | the "submenu" take from 4.45 s, after the stretch where other rows' submenus flash open |
| `3-approve.mp4` | The inline *Approve #1288…* row is clicked, the confirmation alert appears, *Approve* is clicked. | the "approve" take, 3.5 s to 10.3 s, reframed wider to the left so the alert can be centred |
| `4-approved.mp4` | The menu reopened: *acme/web#1288: approval posted*. | the same take, a few seconds later |

`build.sh` turns each clip into PNG frames at stage size (`scale` in `clips.json`: 1.4 stage
pixels per screen point, so a menu row is as large as a row of the app window) and `stage.js`
plays them in one scene, `menu`, through a camera that moves over the captured screen: the
`takes` and `cam` tables at the top of that scene hold all its timing. The clips are cropped, not
scaled, so the geometry in `clips.json` is in screen points of the original 1728x925 capture.

If a clip is missing, or with `PRX_DEMO_MENU=0`, the build leaves the scene out, the app-window
scene opens with "On macOS, PRX.app shows what needs you" and the terminal approval with
"Approving is yours alone", as they did before the menu existed.

What the takes do not contain, and the stage does not fake: the status item in the menu bar (the
capture could only see the app's own windows, so the pointer clicks an empty strip and the menu
drops from there) and a light-appearance variant. The panel around the menu is the stage's usual
panel, filled with the backdrop colour; it is not a drawing of a menu bar. The approve alert shows
a generic folder icon because the capture ran a debug binary, not the bundled app.

To record new takes, read "The menu bar captures" in
[docs/development.md](../../docs/development.md#the-menu-bar-captures). In short: start a sandbox
app with `scripts/demo-sandbox.sh DIR && DIR/run-app --background`, compile the tool with
`swiftc -O menu-capture.swift -o /tmp/menu-capture`, and use its `backdrop`, `clickif`, `menu`
and `rec2` commands with the sandbox app's process id (the header of the file lists them all).
The tool only provides the steps; the sequence of pointer moves for each take was run by hand and
is not scripted here. Crop the recordings to the region the camera needs, keep 2 pixels per point,
strip metadata, update `clips.json`, and look at every frame before adding a clip: it is a real
screen capture and must show only the demo data.
