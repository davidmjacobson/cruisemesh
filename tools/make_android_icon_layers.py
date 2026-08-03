#!/usr/bin/env python3
"""Generate the Android adaptive launcher icon layers from art/cruisemesh-icon.svg.

Adaptive icons hand the launcher a 108dp layer canvas and only guarantee the
inner 66dp square is visible; everything outside can be masked or used for
parallax. So the layers are not just square renders of the icon:

  background  the plate gradient, full bleed, drawn at the same scale as the
              logo so the colour under the logo matches the artwork
  foreground  the logo alone, fitted into the 66dp safe zone, transparent
              everywhere else
  monochrome  the same shape as the foreground, flat white, for themed icons

Rendering uses headless Chrome because it is the one SVG renderer that is
present on every machine this repo gets built on. Pass the browser path with
--chrome if it is not in one of the usual places.
"""

from __future__ import annotations

import argparse
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw

REPO = pathlib.Path(__file__).resolve().parent.parent
SVG = REPO / "art" / "cruisemesh-icon.svg"
OUT = REPO / "android" / "app" / "src" / "main" / "res" / "drawable-nodpi"

CANVAS = 512  # px written per layer; 108dp at xxxhdpi is 432px, so this is ample
SAFE = 66 / 108  # fraction of the layer canvas that is guaranteed visible
# The logo is fitted a little inside the safe square: the safe square's corners
# still fall outside a circular mask, and the waves run to the bottom corner.
FIT = 60 / 108
VIEWPORT = 72 / 108  # fraction a launcher mask is cut from, for the preview only

CHROME_CANDIDATES = [
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "google-chrome",
    "chromium",
]


def find_chrome(explicit: str | None) -> str:
    for candidate in ([explicit] if explicit else []) + CHROME_CANDIDATES:
        if candidate and (pathlib.Path(candidate).exists() or shutil.which(candidate)):
            return candidate
    sys.exit("no Chrome/Edge binary found; pass --chrome /path/to/chrome")


def split_master() -> tuple[str, str, str]:
    """Return (defs, background rect, logo content) from the master SVG."""
    text = SVG.read_text(encoding="utf-8")
    defs = re.search(r"<defs>.*?</defs>", text, re.S).group(0)
    body = text.split("</defs>", 1)[1].rsplit("</svg>", 1)[0]
    bg_rect = re.search(r'<rect width="512" height="512" fill="url\(#bg\)"/>', body).group(0)
    logo = body.replace(bg_rect, "", 1)
    # The corner swoosh is a plate decoration: it runs off the bottom-right of the
    # square icon on purpose. Shrunk into a foreground layer it reads as a stray
    # blob floating beside the logo, so it stays behind with the plate.
    swoosh = re.search(r'<path fill="url\(#g_swoosh\)".*?/>', logo, re.S).group(0)
    return defs, bg_rect, logo.replace(swoosh, "", 1)


def render(chrome: str, svg: str, size: int, workdir: pathlib.Path, name: str) -> Image.Image:
    html = workdir / f"{name}.html"
    png = workdir / f"{name}.png"
    html.write_text(
        "<!doctype html><meta charset=utf-8>"
        "<style>html,body{margin:0;padding:0;background:transparent}</style>" + svg,
        encoding="utf-8",
    )
    subprocess.run(
        [
            chrome,
            "--headless=new",
            "--disable-gpu",
            "--hide-scrollbars",
            "--force-device-scale-factor=1",
            "--default-background-color=00000000",
            f"--window-size={size},{size}",
            f"--screenshot={png}",
            html.as_uri(),
        ],
        check=True,
        capture_output=True,
    )
    image = Image.open(png).convert("RGBA")
    if image.size != (size, size):
        image = image.crop((0, 0, size, size))
    return image


def wrap(defs: str, inner: str, size: int) -> str:
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" '
        f'width="{size}" height="{size}">{defs}{inner}</svg>'
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--chrome")
    args = parser.parse_args()
    chrome = find_chrome(args.chrome)

    defs, bg_rect, logo = split_master()

    with tempfile.TemporaryDirectory() as tmp:
        work = pathlib.Path(tmp)

        # Measure the logo at 1:1 so the safe-zone fit is driven by the real ink,
        # not by the 512x512 plate the logo happens to sit on.
        probe = render(chrome, wrap(defs, logo, CANVAS), CANVAS, work, "probe")
        left, top, right, bottom = probe.split()[3].getbbox()
        ink = max(right - left, bottom - top)
        scale = FIT * CANVAS / ink
        cx, cy = (left + right) / 2, (top + bottom) / 2
        # Transform is expressed in the 512 user space of the master viewBox.
        u = 512 / CANVAS
        transform = (
            f'transform="translate(256,256) scale({scale:.6f}) '
            f'translate({-cx * u:.3f},{-cy * u:.3f})"'
        )

        foreground = render(
            chrome, wrap(defs, f"<g {transform}>{logo}</g>", CANVAS), CANVAS, work, "fg"
        )

        # The plate is scaled with the logo and then over-drawn past the canvas
        # edge, so the gradient under the mesh is the same colour as the artwork.
        pad = 512 / scale
        big_rect = bg_rect.replace(
            '<rect width="512" height="512"',
            f'<rect x="{-pad:.1f}" y="{-pad:.1f}" '
            f'width="{512 + 2 * pad:.1f}" height="{512 + 2 * pad:.1f}"',
        )
        background = render(
            chrome, wrap(defs, f"<g {transform}>{big_rect}</g>", CANVAS), CANVAS, work, "bg"
        )

    background.convert("RGB").save(OUT / "ic_launcher_background.png", optimize=True)
    foreground.save(OUT / "ic_launcher_foreground.png", optimize=True)

    white = Image.new("RGBA", foreground.size, (255, 255, 255, 0))
    white.putalpha(foreground.split()[3])
    white.save(OUT / "ic_launcher_monochrome.png", optimize=True)

    # Preview what a launcher actually shows: the centre 72dp of the 108dp layers,
    # under a circular mask (the tightest common mask) and under a rounded square.
    composite = Image.alpha_composite(background.convert("RGBA"), foreground)
    inset = round(CANVAS * (1 - VIEWPORT) / 2)
    shown = composite.crop((inset, inset, CANVAS - inset, CANVAS - inset))
    side = shown.size[0]
    preview = Image.new("RGBA", (side * 2 + 48, side), (0, 0, 0, 0))
    for index, shape in enumerate(("circle", "square")):
        mask = Image.new("L", (side, side), 0)
        draw = ImageDraw.Draw(mask)
        if shape == "circle":
            draw.ellipse((0, 0, side - 1, side - 1), fill=255)
        else:
            draw.rounded_rectangle((0, 0, side - 1, side - 1), radius=side // 4, fill=255)
        preview.paste(shown, (index * (side + 48), 0), mask)
    preview.save(REPO / "tools" / "icon-preview.png", optimize=True)
    print(f"logo ink {ink}px -> scale {scale:.3f}; wrote layers to {OUT}")


if __name__ == "__main__":
    main()
