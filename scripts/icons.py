#!/usr/bin/env python3
"""Draws steambro's cloud in its three moods and renders the icon files.

Writes, relative to the repository:
  ui/logo-<mood>.svg            the cloud in full detail, for the window's top bar
  icons/icon.png, icon.ico      the app icon (wink); the ico's images up to 32 px use the small drawing
  icons/tray-<mood>-<size>.png  the tray icon at 16, 20, 24 and 32 px, small drawing

The small drawing has a thicker outline, a bigger face, and no blush lines, so the face still
reads at 16 px. Needs resvg on PATH (cargo install resvg).
"""

import struct
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MOODS = ("wink", "glance", "squeeze")
TRAY_SIZES = (16, 20, 24, 32)
ICO_SIZES = (256, 48, 32, 24, 16)

INK = "#151a3d"
# The cloud is a union of circles (x, y, r) on a 256 canvas. With the outline, it reaches all four
# edges: an icon has little room, so it gets no margin.
PUFFS = [
    (128, 70, 62),   # top
    (66, 104, 44),   # top left
    (192, 100, 46),  # top right
    (46, 156, 38),   # left
    (210, 156, 38),  # right
    (76, 200, 44),   # bottom left
    (128, 204, 44),  # bottom
    (180, 200, 44),  # bottom right
    (128, 150, 82),  # middle
]
FULL = dict(outline=8, face=1.0, drop=(0, 0), lines=True)
# The bigger face would touch the drop, so the drop moves up and right.
SMALL = dict(outline=14, face=1.4, drop=(10, -20), lines=False)


def puffs(style):
    """The circles, shrunk by the extra outline width so the outer edge stays at the canvas edge."""
    shrink = style["outline"] - FULL["outline"]
    return [(x, y, r - shrink) for x, y, r in PUFFS]


def circles(cloud, fill, grow=0, dy=0):
    return "".join(f'<circle cx="{x}" cy="{y + dy}" r="{r + grow}" fill="{fill}"/>' for x, y, r in cloud)


def stroke(d, width, fill="none"):
    return (f'<path d="{d}" fill="{fill}" stroke="{INK}" stroke-width="{width}" '
            'stroke-linecap="round" stroke-linejoin="round"/>')


def blush(cx, cy, lines):
    out = f'<ellipse cx="{cx}" cy="{cy}" rx="18" ry="10.5" fill="#ff8fbf" opacity="0.9"/>'
    if lines:
        d = " ".join(f"M{cx - 10 + i * 8} {cy + 5} l5 -9" for i in range(3))
        out += f'<path d="{d}" stroke="#ff5a9a" stroke-width="3" stroke-linecap="round"/>'
    return out


def open_eye(cx, cy):
    return (f'<ellipse cx="{cx}" cy="{cy}" rx="11.5" ry="13.5" fill="{INK}"/>'
            f'<circle cx="{cx + 4}" cy="{cy - 5}" r="4.6" fill="#fff"/>'
            f'<circle cx="{cx - 3.5}" cy="{cy + 5}" r="2" fill="#fff"/>')


def side_eye(cx, cy):
    """A big eye looking left, away from the sweat drop."""
    return (f'<ellipse cx="{cx}" cy="{cy}" rx="15" ry="17" fill="#fff" stroke="{INK}" stroke-width="5"/>'
            f'<ellipse cx="{cx - 5}" cy="{cy + 2}" rx="9.5" ry="12" fill="{INK}"/>'
            f'<circle cx="{cx - 2}" cy="{cy - 4}" r="4" fill="#fff"/>'
            f'<circle cx="{cx - 8}" cy="{cy + 7}" r="1.8" fill="#fff"/>')


def face(mood, lines):
    # Left eye at (102, 150); the right eye mirrors it around x = 128.
    if mood == "wink":
        eyes = stroke("M89 154 L114 145", 8) + open_eye(154, 150)
        mouth = stroke("M113 178 q7.5 11 15 0 q7.5 11 15 0", 6)
    elif mood == "glance":
        eyes = side_eye(100, 148) + side_eye(156, 148)
        mouth = stroke("M116 184 q4 -6 8 0 t8 0 t8 0", 5.5)
    else:  # squeeze
        eyes = stroke("M91 139 L111 150 L91 161", 7.5) + stroke("M165 139 L145 150 L165 161", 7.5)
        mouth = (stroke("M117 176 Q128 173 139 176 Q138 194 128 194 Q118 194 117 176 Z", 3, fill=INK)
                 + '<path d="M122 190 Q128 184 134 190 Q128 194 122 190 Z" fill="#ff7aa8"/>')
    return eyes + blush(70, 176, lines) + blush(186, 176, lines) + mouth


def scaled(content, factor, cx, cy):
    if factor == 1:
        return content
    return f'<g transform="translate({cx} {cy}) scale({factor}) translate({-cx} {-cy})">{content}</g>'


def drop():
    """One anime sweat drop on the upper right of the forehead."""
    return (f'<path d="M190 82 C 197 98, 208 108, 208 121 A 18 18 0 0 1 172 121 C 172 108, 183 98, 190 82 Z" '
            f'fill="url(#drop)" stroke="{INK}" stroke-width="5" stroke-linejoin="round"/>'
            '<ellipse cx="183" cy="120" rx="4" ry="6.5" fill="#fff"/>')


def cloud_svg(mood, style):
    cloud = puffs(style)
    return f'''<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 256 256">
  <defs>
    <linearGradient id="body" gradientUnits="userSpaceOnUse" x1="40" y1="8" x2="216" y2="248"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#eef3ff"/></linearGradient>
    <linearGradient id="drop" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#d6f1ff"/><stop offset="1" stop-color="#7fc6ff"/></linearGradient>
    <mask id="rim" maskUnits="userSpaceOnUse" x="0" y="0" width="256" height="256">{circles(cloud, "#fff")}{circles(cloud, "#000", grow=-2, dy=-8)}</mask>
  </defs>
  {circles(cloud, INK, grow=style["outline"])}
  {circles(cloud, "url(#body)")}
  <rect width="256" height="256" fill="#a9c4ff" opacity="0.75" mask="url(#rim)"/>
  {scaled(face(mood, style["lines"]), style["face"], 128, 165)}
  <g transform="translate({style["drop"][0]} {style["drop"][1]})">{drop()}</g>
</svg>
'''


def render(svg, size, out):
    with tempfile.NamedTemporaryFile("w", suffix=".svg", delete=False) as f:
        f.write(svg)
    try:
        subprocess.run(["resvg", "-w", str(size), "-h", str(size), f.name, str(out)], check=True)
    finally:
        Path(f.name).unlink()


def write_ico(pngs, out):
    """An .ico of PNG images, largest first. `pngs` maps size to PNG bytes."""
    head = struct.pack("<HHH", 0, 1, len(pngs))
    body = b""
    offset = 6 + 16 * len(pngs)
    for size, data in pngs.items():
        head += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset + len(body))
        body += data
    out.write_bytes(head + body)


def main():
    icons = ROOT / "icons"
    for mood in MOODS:
        (ROOT / "ui" / f"logo-{mood}.svg").write_text(cloud_svg(mood, FULL))
        for size in TRAY_SIZES:
            render(cloud_svg(mood, SMALL), size, icons / f"tray-{mood}-{size}.png")
    with tempfile.TemporaryDirectory() as tmp:
        pngs = {}
        for size in ICO_SIZES:
            path = Path(tmp) / f"{size}.png"
            render(cloud_svg("wink", SMALL if size <= 32 else FULL), size, path)
            pngs[size] = path.read_bytes()
        write_ico(pngs, icons / "icon.ico")
        (icons / "icon.png").write_bytes(pngs[256])
    print("icons written")


if __name__ == "__main__":
    sys.exit(main())
