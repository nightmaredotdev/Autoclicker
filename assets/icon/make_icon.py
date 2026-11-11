#!/usr/bin/env python3
"""Generates the Cadence icon assets from cadence.svg.

cadence.svg is the canonical artwork and is never modified. This script
nests it, unchanged except for its colour (white instead of the SVG's
default dark colour), on a blue rounded-square tile; the tray icon for the
running state adds a green badge. Every size is rendered directly from the
vector artwork by resvg, a development-time tool pinned to one version and
checksum below.

Outputs (written next to this script):
    cadence.ico        multi-size Windows icon (16-256 px, PNG-compressed)
    icon-256.png       256 px window icon
    tray-idle.png      32 px tray icon (stopped)
    tray-running.png   32 px tray icon (running, green badge)

Requirements: Python 3.8+ (standard library only) and the official resvg
0.49.0 executable, found through the RESVG environment variable or on PATH:

    RESVG=C:\\tools\\resvg-0.49.0\\resvg.exe python assets/icon/make_icon.py

The script refuses to run with any other resvg version or binary. The output
is deterministic: running it again reproduces identical files.
"""

import hashlib
import os
import shutil
import struct
import subprocess
import sys
import xml.etree.ElementTree as ET

RESVG_VERSION = "0.49.0"
# SHA-256 of the executables inside the official release archives
# (https://github.com/linebender/resvg/releases/tag/v0.49.0). The archive
# itself is checked against the digest published with the release; see
# README.md.
RESVG_EXECUTABLES = {
    # resvg-win-x86_64.zip (sha256 38af9631b37e61118a32a5ef31e6be6af083eda63b4c17ffc4fdc63e449277df)
    "266b45d69cd80aafbd7ff2b6424a6a6c2e476888a535a4ce38d5a055bed95624": "resvg-win-x86_64",
}

SVG_NS = "http://www.w3.org/2000/svg"
CANVAS = 256  # coordinate system of the tile; matches cadence.svg's viewBox

# Tile: blue rounded square with a darker rim for contrast on light taskbars.
BACKGROUND = "#2563eb"
BACKGROUND_EDGE = "#1d4eba"
CORNER = 0.22 * CANVAS
RIM = 0.035 * CANVAS
GLYPH = "#ffffff"
# Running badge: green dot with a white ring, lower right.
BADGE = "#16a34a"
BADGE_RIM = "#ffffff"
BADGE_CENTER = 0.76 * CANVAS
BADGE_RADIUS = 0.24 * CANVAS
BADGE_INNER = 0.78  # green part, as a fraction of the badge radius

ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
TRAY_SIZE = 32


def fail(message):
    sys.exit(f"make_icon.py: {message}")


def find_resvg():
    """Path of the pinned resvg executable, or exits with an explanation."""
    path = os.environ.get("RESVG") or shutil.which("resvg")
    if not path or not os.path.isfile(path):
        fail(
            f"resvg {RESVG_VERSION} is required but was not found. Download the "
            "official release (see assets/icon/README.md) and set RESVG to the "
            "executable's path, or put it on PATH."
        )
    with open(path, "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    if digest not in RESVG_EXECUTABLES:
        fail(
            f"{path} is not the pinned resvg {RESVG_VERSION} executable "
            f"(sha256 {digest}). Use the official release (see assets/icon/README.md)."
        )
    version = subprocess.run(
        [path, "-V"], capture_output=True, text=True, check=False
    ).stdout.strip()
    if version != RESVG_VERSION:
        fail(f"{path} reports version {version!r}, expected {RESVG_VERSION}")
    return path


def tile_svg(glyph_path, badge):
    """The icon as SVG source: tile, the unmodified glyph, optional badge."""
    ET.register_namespace("", SVG_NS)
    glyph = ET.parse(glyph_path).getroot()
    if glyph.get("viewBox") != f"0 0 {CANVAS} {CANVAS}":
        fail(f"{glyph_path}: expected viewBox '0 0 {CANVAS} {CANVAS}'")
    # Only the colour (which every shape uses through currentColor) and the
    # placement change; the geometry and all other attributes stay as drawn.
    glyph.set("color", GLYPH)
    glyph.set("x", "0")
    glyph.set("y", "0")
    glyph.set("width", str(CANVAS))
    glyph.set("height", str(CANVAS))

    root = ET.Element(
        f"{{{SVG_NS}}}svg",
        {"viewBox": f"0 0 {CANVAS} {CANVAS}", "width": str(CANVAS), "height": str(CANVAS)},
    )

    def rect(inset, fill):
        size = CANVAS - 2 * inset
        ET.SubElement(root, f"{{{SVG_NS}}}rect", {
            "x": f"{inset:g}", "y": f"{inset:g}",
            "width": f"{size:g}", "height": f"{size:g}",
            "rx": f"{CORNER - inset:g}", "fill": fill,
        })

    def circle(radius, fill):
        ET.SubElement(root, f"{{{SVG_NS}}}circle", {
            "cx": f"{BADGE_CENTER:g}", "cy": f"{BADGE_CENTER:g}",
            "r": f"{radius:g}", "fill": fill,
        })

    rect(0, BACKGROUND_EDGE)
    rect(RIM, BACKGROUND)
    root.append(glyph)
    if badge:
        circle(BADGE_RADIUS, BADGE_RIM)
        circle(BADGE_RADIUS * BADGE_INNER, BADGE)
    return ET.tostring(root, encoding="utf-8")


def render(resvg, svg, size):
    """PNG bytes of `svg` rendered at `size` x `size` pixels."""
    result = subprocess.run(
        [resvg, "-w", str(size), "-h", str(size), "-", "-c"],
        input=svg, capture_output=True, check=False,
    )
    if result.returncode != 0:
        fail(f"resvg failed: {result.stderr.decode(errors='replace').strip()}")
    blob = result.stdout
    # Windows accepts PNG icon entries only as 32-bit RGBA.
    if blob[:8] != b"\x89PNG\r\n\x1a\n" or blob[12:16] != b"IHDR":
        fail("resvg did not produce a PNG")
    width, height, depth, colour = struct.unpack(">IIBB", blob[16:26])
    if (width, height, depth, colour) != (size, size, 8, 6):
        fail(f"unexpected PNG format {width}x{height}, depth {depth}, colour type {colour}")
    return blob


def ico(images):
    """ICO container with PNG-compressed entries (supported since Windows Vista)."""
    header = struct.pack("<HHH", 0, 1, len(images))
    entries = b""
    data = b""
    offset = 6 + 16 * len(images)
    for size, blob in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(blob), offset)
        data += blob
        offset += len(blob)
    return header + entries + data


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    resvg = find_resvg()
    plain = tile_svg(os.path.join(here, "cadence.svg"), badge=False)
    running = tile_svg(os.path.join(here, "cadence.svg"), badge=True)

    def write(name, blob):
        with open(os.path.join(here, name), "wb") as f:
            f.write(blob)
        print(f"wrote {name} ({len(blob)} bytes)")

    images = [(s, render(resvg, plain, s)) for s in ICO_SIZES]
    write("cadence.ico", ico(images))
    write("icon-256.png", images[-1][1])
    write("tray-idle.png", render(resvg, plain, TRAY_SIZE))
    write("tray-running.png", render(resvg, running, TRAY_SIZE))


if __name__ == "__main__":
    main()
