#!/usr/bin/env python3
"""Generates the Autoclicker icon assets from the geometry defined below.

This script is the source of the artwork: a blue rounded square with a
white mouse pointer and three short "click" strokes. The tray variant for
the running state adds a green badge.

Outputs (written next to this script):
    autoclicker.ico    multi-size Windows icon (16-256 px, PNG-compressed)
    tray-idle.png      32 px tray icon (stopped)
    tray-running.png   32 px tray icon (running, green badge)
    icon-256.png       256 px preview

Usage (Python 3.8+, standard library only):
    python3 assets/icon/make_icon.py

The output is deterministic, so regenerating without changes reproduces
identical files.
"""

import math
import os
import struct
import zlib

SUPERSAMPLE = 8  # samples per pixel along each axis

BACKGROUND = (37, 99, 235)      # blue
BACKGROUND_EDGE = (29, 78, 186)  # darker rim for contrast on light taskbars
FOREGROUND = (255, 255, 255)
BADGE = (22, 163, 74)           # green: running
BADGE_RIM = (255, 255, 255)

# Geometry in a 0..1 unit square.
CORNER = 0.22
RIM = 0.035
# Mouse pointer (classic arrow), tip at the upper left.
ARROW = [
    (0.34, 0.25),
    (0.34, 0.78),
    (0.455, 0.665),
    (0.545, 0.85),
    (0.625, 0.81),
    (0.535, 0.625),
    (0.70, 0.625),
]
# Three "click" strokes radiating from the arrow tip: (angle deg, from, to).
RAYS = [(-90, 0.08, 0.15), (-135, 0.08, 0.15), (-180, 0.08, 0.15)]
RAY_WIDTH = 0.05
TIP = (0.34, 0.25)


def inside_rounded_square(x, y, inset=0.0):
    r = CORNER - inset
    lo, hi = inset, 1.0 - inset
    if x < lo or x > hi or y < lo or y > hi:
        return False
    cx = min(max(x, lo + r), hi - r)
    cy = min(max(y, lo + r), hi - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def inside_polygon(x, y, poly):
    inside = False
    n = len(poly)
    for i in range(n):
        x1, y1 = poly[i]
        x2, y2 = poly[(i + 1) % n]
        if (y1 > y) != (y2 > y):
            xi = x1 + (y - y1) * (x2 - x1) / (y2 - y1)
            if x < xi:
                inside = not inside
    return inside


def near_segment(x, y, a, b, half_width):
    (ax, ay), (bx, by) = a, b
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)))
    px, py = ax + t * dx, ay + t * dy
    return (x - px) ** 2 + (y - py) ** 2 <= half_width * half_width


def ray_segments():
    segs = []
    for angle, r0, r1 in RAYS:
        a = math.radians(angle)
        ux, uy = math.cos(a), math.sin(a)
        segs.append(((TIP[0] + ux * r0, TIP[1] + uy * r0), (TIP[0] + ux * r1, TIP[1] + uy * r1)))
    return segs


def sample(x, y, badge):
    """Colour (r, g, b, a) of the artwork at unit coordinates (x, y)."""
    if badge:
        bx, by, br = 0.76, 0.76, 0.24
        d2 = (x - bx) ** 2 + (y - by) ** 2
        if d2 <= (br * 0.78) ** 2:
            return BADGE + (255,)
        if d2 <= br * br:
            return BADGE_RIM + (255,)
    if not inside_rounded_square(x, y):
        return (0, 0, 0, 0)
    if inside_polygon(x, y, ARROW):
        return FOREGROUND + (255,)
    for a, b in ray_segments():
        if near_segment(x, y, a, b, RAY_WIDTH / 2):
            return FOREGROUND + (255,)
    if not inside_rounded_square(x, y, RIM):
        return BACKGROUND_EDGE + (255,)
    return BACKGROUND + (255,)


def render(size, badge=False):
    """RGBA bytes of the icon at `size` x `size`, anti-aliased by supersampling."""
    out = bytearray()
    n = SUPERSAMPLE
    for py in range(size):
        for px in range(size):
            r = g = b = a = 0
            for sy in range(n):
                for sx in range(n):
                    x = (px + (sx + 0.5) / n) / size
                    y = (py + (sy + 0.5) / n) / size
                    cr, cg, cb, ca = sample(x, y, badge)
                    r += cr * ca
                    g += cg * ca
                    b += cb * ca
                    a += ca
            if a:
                out += bytes((round(r / a), round(g / a), round(b / a), round(a / (n * n))))
            else:
                out += b"\0\0\0\0"
    return bytes(out)


def png(size, rgba):
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = b"".join(b"\0" + rgba[y * size * 4:(y + 1) * size * 4] for y in range(size))
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b""))


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

    def write(name, blob):
        with open(os.path.join(here, name), "wb") as f:
            f.write(blob)
        print(f"wrote {name} ({len(blob)} bytes)")

    sizes = [16, 20, 24, 32, 40, 48, 64, 256]
    images = [(s, png(s, render(s))) for s in sizes]
    write("autoclicker.ico", ico(images))
    write("icon-256.png", images[-1][1])
    write("tray-idle.png", png(32, render(32)))
    write("tray-running.png", png(32, render(32, badge=True)))


if __name__ == "__main__":
    main()
