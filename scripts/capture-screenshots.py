#!/usr/bin/env python3
"""capture-screenshots.py: regenerate the PRX.app window screenshots in
docs/media/screenshots/ from the settings UI preview (fictional demo data,
no daemon, nothing of yours on screen).

Usage:
    python3 scripts/capture-screenshots.py               # every shot
    python3 scripts/capture-screenshots.py overview prs  # only these
    python3 scripts/capture-screenshots.py --list
    PRX_SHOTS_OUT=/tmp/shots python3 scripts/capture-screenshots.py

Needs: node, and the Python packages playwright (with its WebKit build:
`python3 -m playwright install webkit`) and Pillow.

How it works: starts menubar/settings-ui-tests/preview-server.js on a free
port, loads each page in WebKit (the engine the app's own webview uses) at
the window's real content size (1040x700 points, 2x), and frames the result
with the window's rounded corners and a soft shadow on a transparent
background. No title bar is drawn. PNGs carry no metadata.

The terminal shot is separate, because it needs the demo sandbox and vhs:
    scripts/demo-sandbox.sh /tmp/prx-demo
    python3 scripts/capture-screenshots.py --cli /tmp/prx-demo
It runs `prx status`, `prx brief` and `prx usage` inside the sandbox under vhs (which
needs ttyd) and frames the result the same way. The shell function it
defines drops prx's one-line "daemon not running" note on stderr; stdout is
shown byte for byte. vhs starts ttyd through scripts/demo-video/ttyd-shim,
which makes the recorder's terminal measure emoji the way current terminals
do (see the comment in that file).

The menu bar menu (menu.png) is a native capture and is not scripted: with
the sandbox app running (`/tmp/prx-demo/run-app --background`) open its menu,
press Cmd-Shift-4, then Space, and click the menu; macOS saves the menu panel
alone, with its own shadow on a transparent background. Then
    python3 scripts/capture-screenshots.py --import ~/Desktop/<that file>.png menu
re-encodes it into docs/media/screenshots/menu.png without metadata.
"""
import io
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent.parent
OUT = Path(os.environ.get("PRX_SHOTS_OUT", ROOT / "docs/media/screenshots"))
WIDTH, HEIGHT, SCALE = 1040, 700, 2  # menubar/src/settings/mod.rs WIDTH/HEIGHT
RADIUS = 12   # window corner radius, points
PAD = 28      # transparent margin that holds the shadow, points
CLI_HEIGHT = int(os.environ.get("PRX_CLI_HEIGHT", 1000))  # pixels; the terminal is drawn at 2080 wide to match the window shots
MAX_BYTES = 580 * 1024
TIMEZONE ="Europe/Berlin"  # the demo sandbox's default (scripts/demo-sandbox.sh)


def select_repo(page):
    page.get_by_text("billing", exact=True).first.click()
    page.wait_for_timeout(400)


def open_brief(page):
    page.locator("button.row-open", has_text="Checkout: show tax before payment").click()
    page.wait_for_timeout(400)


# name -> (page id, colour scheme, query string, action after load)
SHOTS = {
    "overview": ("dashboard", "light", "", None),
    "overview-dark": ("dashboard", "dark", "", None),
    "prs": ("prs", "light", "", open_brief),
    "prs-dark": ("prs", "dark", "", open_brief),
    "history": ("history", "light", "", None),
    "usage": ("usage", "light", "", None),
    "usage-dark": ("usage", "dark", "", None),
    "settings": ("repos", "light", "", select_repo),
    "settings-dark": ("repos", "dark", "", select_repo),
    "review": ("review", "light", "", None),
    "schedule": ("schedule", "light", "", None),
    "notifications": ("notifications", "light", "", None),
    "engines": ("engines", "light", "", None),
    "menubar-settings": ("menubar", "light", "", None),
}


TAPE = """Output "{out}/cli.gif"
Set Shell "bash"
Set FontSize 23
Set Width 2080
Set Height {height}
Set Padding 44
Set TypingSpeed 0
Set CursorBlink false
Set Theme {{ "name": "prx", "background": "#0E0F12", "foreground": "#E4E4E7", "cursor": "#0E0F12", "selection": "#2F7BF5", "black": "#18181B", "red": "#F87171", "green": "#4ADE80", "yellow": "#FACC15", "blue": "#2F7BF5", "magenta": "#C084FC", "cyan": "#22D3EE", "white": "#E4E4E7", "brightBlack": "#71717A", "brightRed": "#F87171", "brightGreen": "#4ADE80", "brightYellow": "#FACC15", "brightBlue": "#60A5FA", "brightMagenta": "#C084FC", "brightCyan": "#22D3EE", "brightWhite": "#FAFAFA" }}
Hide
Type "source {out}/setup.sh"
Enter
Sleep 1s
Show
Type "prx status"
Enter
Sleep 2s
Type "prx brief 'acme/web#1288'"
Enter
Sleep 2s
Type "prx usage --by week"
Enter
Sleep 2s
Screenshot "{out}/cli-raw.png"
Sleep 200ms
"""


def capture_cli(sandbox):
    """cli.png: status, brief and usage in the demo sandbox, rendered by vhs."""
    import tempfile
    sandbox = Path(sandbox).resolve()
    if not (sandbox / ".prx-demo-sandbox").exists():
        print(f"{sandbox} is not a demo sandbox; run scripts/demo-sandbox.sh {sandbox} first", file=sys.stderr)
        return 2
    OUT.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        tape = Path(tmp) / "cli.tape"
        (Path(tmp) / "setup.sh").write_text(
            f"source {sandbox}/env.sh; cd\n"
            'prx() { command prx "$@" 2>/dev/null; }\n'
            "PS1='\\[\\e[38;2;47;123;245m\\]$\\[\\e[0m\\] '\nclear\n")
        tape.write_text(TAPE.format(out=tmp, sandbox=sandbox, height=CLI_HEIGHT))
        env = dict(os.environ, PATH=f"{ROOT / 'scripts/demo-video/ttyd-shim'}{os.pathsep}{os.environ['PATH']}")
        subprocess.run(["vhs", str(tape)], check=True, cwd=tmp, stdout=subprocess.DEVNULL, env=env)
        save(frame((Path(tmp) / "cli-raw.png").read_bytes()), OUT / "cli.png")
    print(f"cli.png  {(OUT / 'cli.png').stat().st_size // 1024} KB")
    return 0


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def frame(png_bytes):
    """Round the corners, add a hairline edge and a soft shadow on transparent."""
    shot = Image.open(io.BytesIO(png_bytes)).convert("RGBA")
    w, h = shot.size
    r, pad = RADIUS * SCALE, PAD * SCALE
    mask = Image.new("L", (w * 2, h * 2), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, w * 2 - 1, h * 2 - 1), r * 2, fill=255)
    mask = mask.resize((w, h), Image.LANCZOS)
    shot.putalpha(mask)
    # Hairline so the window edge reads on GitHub's dark and light pages alike.
    edge = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    ImageDraw.Draw(edge).rounded_rectangle((0, 0, w - 1, h - 1), r, outline=(128, 128, 128, 110), width=SCALE // 2 or 1)
    edge.putalpha(ImageChops.multiply(edge.getchannel("A"), mask))
    shot = Image.alpha_composite(shot, edge)

    canvas = Image.new("RGBA", (w + pad * 2, h + pad * 2), (0, 0, 0, 0))
    shadow_alpha = Image.new("L", canvas.size, 0)
    shadow_alpha.paste(mask.point(lambda v: v * 70 // 255), (pad, pad + 6 * SCALE))
    shadow_alpha = shadow_alpha.filter(ImageFilter.GaussianBlur(9 * SCALE))
    shadow = Image.new("RGBA", canvas.size, (0, 0, 0, 0))
    shadow.putalpha(shadow_alpha)
    canvas = Image.alpha_composite(canvas, shadow)
    canvas.alpha_composite(shot, (pad, pad))
    return canvas


def save(image, path):
    """Write a metadata-free PNG; fall back to a 256-colour palette only if the
    full-colour file would be too heavy for git (it bands the shadow a little)."""
    image.save(path, optimize=True)
    if path.stat().st_size > MAX_BYTES:
        small = image.quantize(colors=256, method=Image.Quantize.FASTOCTREE, dither=Image.Dither.NONE)
        small.save(path, optimize=True)


def main(argv):
    if "--list" in argv:
        print("\n".join(SHOTS))
        return 0
    if "--import" in argv:
        src, name = argv[argv.index("--import") + 1:][:2]
        OUT.mkdir(parents=True, exist_ok=True)
        pixels = Image.open(src).convert("RGBA")
        clean = Image.new("RGBA", pixels.size)
        clean.paste(pixels)
        save(clean, OUT / f"{name}.png")
        print(f"{name}.png  {clean.size[0]}x{clean.size[1]}  {(OUT / (name + '.png')).stat().st_size // 1024} KB")
        return 0
    if "--cli" in argv:
        rest = argv[argv.index("--cli") + 1:]
        return capture_cli(rest[0] if rest else os.environ.get("PRX_DEMO_DIR", "/tmp/prx-demo"))
    names = [a for a in argv if not a.startswith("-")] or list(SHOTS)
    unknown = [n for n in names if n not in SHOTS]
    if unknown:
        print("unknown shot(s): " + ", ".join(unknown), file=sys.stderr)
        return 2
    OUT.mkdir(parents=True, exist_ok=True)
    port = free_port()
    server = subprocess.Popen(
        ["node", str(ROOT / "menubar/settings-ui-tests/preview-server.js"), str(port)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(0.8)
        with sync_playwright() as p:
            browser = p.webkit.launch()
            for name in names:
                page_id, scheme, query, action = SHOTS[name]
                ctx = browser.new_context(
                    viewport={"width": WIDTH, "height": HEIGHT}, device_scale_factor=SCALE,
                    color_scheme=scheme, timezone_id=TIMEZONE, locale="en-US")
                page = ctx.new_page()
                page.goto(f"http://127.0.0.1:{port}/{query}#page={page_id}")
                page.wait_for_selector("main h1, main h2")
                page.wait_for_timeout(1200)
                if action:
                    action(page)
                # The demo data starts with one brief being written; it lands on
                # the page's 10 s poll. Never capture a loading state.
                page.wait_for_function("() => !document.body.innerText.includes('Writing brief')", timeout=40000)
                page.mouse.move(WIDTH - 2, HEIGHT - 2)
                page.wait_for_timeout(300)
                save(frame(page.screenshot(type="png")), OUT / f"{name}.png")
                print(f"{name}.png  {(OUT / (name + '.png')).stat().st_size // 1024} KB")
                ctx.close()
            browser.close()
    finally:
        server.terminate()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
