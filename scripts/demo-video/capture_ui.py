#!/usr/bin/env python3
"""Capture the PRX window UI for the demo video.

Drives the settings-UI preview (menubar/settings-ui-tests/preview-server.js,
fictional mock backend) in headless Chromium at the native window size,
1040x700, with a device scale factor of 2, and writes one PNG per UI state
plus meta.json (element positions the compositor's pointer aims at).

    capture_ui.py OUT_DIR [BASE_URL]
"""
import json
import sys
from pathlib import Path

from playwright.sync_api import sync_playwright

OUT = Path(sys.argv[1])
BASE = sys.argv[2] if len(sys.argv) > 2 else "http://127.0.0.1:4174"
W, H, DPR = 1040, 700, 2
# Strings that must never be on screen: transient or broken states.
FORBIDDEN = ["Writing brief", "undefined", "NaN", "Loading", "[object"]

OUT.mkdir(parents=True, exist_ok=True)
meta = {"window": {"w": W, "h": H, "dpr": DPR}, "states": {}, "targets": {}}


def box(locator):
    b = locator.bounding_box()
    assert b, "target not on screen"
    return {k: round(v, 1) for k, v in b.items()}


def centre(b, fx=0.5, fy=0.5):
    return b["x"] + b["width"] * fx, b["y"] + b["height"] * fy


with sync_playwright() as p:
    browser = p.chromium.launch()
    ctx = browser.new_context(
        viewport={"width": W, "height": H},
        device_scale_factor=DPR,
        color_scheme="dark",
        timezone_id="Europe/Berlin",
        locale="en-US",
    )
    page = ctx.new_page()

    def settle(ms=450):
        page.wait_for_timeout(ms)

    def check():
        text = page.inner_text("body")
        for bad in FORBIDDEN:
            assert bad not in text, f"{bad!r} is on screen"

    def shot(name):
        check()
        page.screenshot(path=str(OUT / f"{name}.png"))
        meta["states"][name] = {"file": f"{name}.png"}

    def tall(name):
        """The whole scrolling pane as one image, for smooth scrolling."""
        full = page.evaluate("document.querySelector('#page').scrollHeight")
        page.set_viewport_size({"width": W, "height": max(H, full)})
        settle(300)
        rect = box(page.locator("#page"))
        check()
        page.screenshot(path=str(OUT / f"{name}.png"), clip=rect)
        meta["states"][name] = {"file": f"{name}.png", "x": rect["x"], "y": rect["y"],
                                "w": rect["width"], "h": rect["height"]}
        page.set_viewport_size({"width": W, "height": H})
        settle(300)

    def nav(label):
        return page.locator("nav a, nav button, aside a, aside button").filter(has_text=label).first

    def go(page_id):
        page.goto(f"{BASE}/#page={page_id}")
        page.reload()
        settle(900)

    # --- Dashboard -----------------------------------------------------------
    page.mouse.move(700, 560)
    go("dashboard")
    # The top brief shows a "Writing brief..." spinner for the first seconds.
    # (The page's CSP forbids wait_for_function with a string, so poll.)
    for _ in range(60):
        if "Writing brief" not in page.inner_text("body"):
            break
        settle(500)
    settle(600)
    shot("dash")
    tall("dash_tall")
    meta["targets"]["nav_prs"] = box(nav("Pull requests"))
    meta["targets"]["nav_history"] = box(nav("History"))
    meta["targets"]["nav_usage"] = box(nav("Usage"))
    page.mouse.move(*centre(meta["targets"]["nav_prs"], 0.45))
    settle()
    shot("dash_nav_prs")

    # --- Pull requests -------------------------------------------------------
    nav("Pull requests").click()
    settle(900)
    shot("prs")
    title = page.get_by_text("Checkout: show tax before payment", exact=True).first
    meta["targets"]["pr_title"] = box(title)
    page.mouse.move(*centre(meta["targets"]["pr_title"], 0.6))
    settle()
    shot("prs_hover")
    page.mouse.down()
    page.mouse.up()
    settle(900)
    shot("prs_open")
    meta["targets"]["pr_brief"] = box(page.locator("text=Shoppers now see the tax amount").first)
    page.mouse.move(*centre(meta["targets"]["nav_history"], 0.45))
    settle()
    shot("prs_nav_history")

    # --- History -------------------------------------------------------------
    nav("History").click()
    settle(900)
    shot("hist")
    row = page.get_by_text("Retry Stripe webhooks with backoff", exact=True).first
    meta["targets"]["hist_row"] = box(row)
    page.mouse.move(*centre(meta["targets"]["hist_row"], 0.3))
    settle()
    shot("hist_hover")
    page.mouse.down()
    page.mouse.up()
    settle(900)
    shot("hist_open")
    meta["targets"]["hist_findings"] = box(page.get_by_text("Blocking findings", exact=True).first)
    page.mouse.move(*centre(meta["targets"]["nav_usage"], 0.45))
    settle()
    shot("hist_nav_usage")

    # --- Usage & stats -------------------------------------------------------
    nav("Usage").click()
    settle(1200)
    page.mouse.move(640, 300)
    settle()
    shot("usage")
    page.mouse.move(1030, 690)
    settle()
    tall("usage_tall")

    browser.close()

(OUT / "meta.json").write_text(json.dumps(meta, indent=1))
print(f"captured {len(meta['states'])} UI states into {OUT}")
