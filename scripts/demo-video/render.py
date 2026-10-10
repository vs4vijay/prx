#!/usr/bin/env python3
"""Render the demo video's frames from stage.html.

    render.py BUILD_DIR --frames OUT_DIR [--start N] [--end N]   PNG frames N..end-1
    render.py BUILD_DIR --poster FILE [--still SECONDS FILE]     poster / one frame
    render.py BUILD_DIR --shots                                  the shot list as JSON

BUILD_DIR holds stage.html, stage.js, manifest.js and the captured sources.
The stage is a pure function of time, so frame ranges can be rendered by
several processes at once (build.sh does).
"""
import argparse
import json
from pathlib import Path

from playwright.sync_api import sync_playwright

ap = argparse.ArgumentParser()
ap.add_argument("build")
ap.add_argument("--frames")
ap.add_argument("--start", type=int, default=0)
ap.add_argument("--end", type=int)
ap.add_argument("--poster")
ap.add_argument("--still", nargs=2, action="append", default=[], metavar=("SECONDS", "FILE"))
ap.add_argument("--shots", action="store_true")
args = ap.parse_args()

with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page(viewport={"width": 1920, "height": 1080}, device_scale_factor=1)
    errors = []
    page.on("pageerror", lambda e: errors.append(str(e)))
    page.goto((Path(args.build).resolve() / "stage.html").as_uri())
    page.evaluate("prxStage.ready()")
    info = page.evaluate("({fps: prxStage.fps, frames: prxStage.frames, total: prxStage.total})")
    if args.shots:
        print(json.dumps({"total": info["total"], "shots": page.evaluate("prxStage.shots()")}, indent=1))
    if args.poster:
        page.evaluate("prxStage.renderPoster()")
        page.screenshot(path=args.poster)
    for seconds, file in args.still:
        page.evaluate("t => prxStage.renderAt(t)", float(seconds))
        page.screenshot(path=file)
    if args.frames:
        out = Path(args.frames)
        out.mkdir(parents=True, exist_ok=True)
        end = info["frames"] if args.end is None else min(args.end, info["frames"])
        for n in range(args.start, end):
            page.evaluate("t => prxStage.renderAt(t)", n / info["fps"])
            page.screenshot(path=str(out / f"f_{n:05d}.png"))
    browser.close()
    if errors:
        raise SystemExit("stage errors: " + "; ".join(errors[:3]))
