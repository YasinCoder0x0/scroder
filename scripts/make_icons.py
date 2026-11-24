#!/usr/bin/env python3
"""Generates the Scroder application icon (ICO, PNGs and raw RGBA for egui).

The script only uses the Python standard library so it can be re-run on any
machine without extra tooling. Run it from the project root:

    python3 scripts/make_icons.py
"""

from __future__ import annotations

import math
import os
import struct
import zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RES = os.path.join(ROOT, "resources")
SIZES = [16, 24, 32, 48, 64, 128, 256]
SS = 4  # supersampling factor

PRIMARY = (72, 75, 255)        # #484BFF
PRIMARY_DEEP = (43, 38, 196)   # bottom-right of the gradient
WHITE = (255, 255, 255)
CORAL = (255, 84, 112)
CORAL_DEEP = (226, 46, 86)


def smoothstep(edge0: float, edge1: float, x: float) -> float:
    if edge0 == edge1:
        return 1.0 if x >= edge1 else 0.0
    t = max(0.0, min(1.0, (x - edge0) / (edge1 - edge0)))
    return t * t * (3.0 - 2.0 * t)


def lerp(a: float, b: float, t: float) -> float:
    return a + (b - a) * t


def mix(c1, c2, t: float):
    return tuple(lerp(a, b, t) for a, b in zip(c1, c2))


def squircle_inside(px: float, py: float, size: float) -> bool:
    """Superellipse |x|^5 + |y|^5 <= 1 with a small margin."""
    half = size * 0.5 * 0.94
    nx = abs(px - size * 0.5) / half
    ny = abs(py - size * 0.5) / half
    return nx ** 5 + ny ** 5 <= 1.0


def seg_dist(px, py, ax, ay, bx, by):
    vx, vy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * vx + (py - ay) * vy) / (vx * vx + vy * vy)))
    return math.hypot(px - (ax + vx * t), py - (ay + vy * t))


def bracket_segments(size: float):
    lo, hi, arm = 0.25 * size, 0.75 * size, 0.15 * size
    segs = []
    for cx, dx in ((lo, 1), (hi, -1)):
        for cy, dy in ((lo, 1), (hi, -1)):
            segs.append((cx, cy, cx + dx * arm, cy))
            segs.append((cx, cy, cx, cy + dy * arm))
    return segs


def render_hires(size: int, ss: int = SS):
    """Returns a list of straight-alpha RGBA tuples at size*ss resolution."""
    hi = size * ss
    c = hi * 0.5
    # Thicker strokes at tiny sizes keep the mark legible in the taskbar.
    stroke = hi * (0.075 if size <= 24 else 0.062)
    segs = bracket_segments(hi)
    ring_outer = hi * 0.168
    ring_inner = ring_outer - hi * (0.05 if size <= 24 else 0.04)
    dot = hi * (0.13 if size <= 24 else 0.105)

    pixels = []
    for y in range(hi):
        for x in range(hi):
            px, py = x + 0.5, y + 0.5
            if not squircle_inside(px, py, hi):
                pixels.append((0, 0, 0, 0))
                continue

            t = (px + py) / (2.0 * hi)
            col = mix(PRIMARY, PRIMARY_DEEP, smoothstep(0.15, 1.0, t))
            # Soft top highlight.
            glow = max(0.0, 1.0 - math.hypot(px / hi - 0.3, py / hi - 0.15) * 2.2)
            col = mix(col, WHITE, glow * 0.12)

            if any(seg_dist(px, py, *s) <= stroke * 0.5 for s in segs):
                col = WHITE
            d = math.hypot(px - c, py - c)
            if size > 24 and ring_inner <= d <= ring_outer:
                col = WHITE
            if d <= dot:
                col = mix(CORAL, CORAL_DEEP, d / dot * 0.6)

            r, g, b = (int(round(v)) for v in col)
            pixels.append((r, g, b, 255))
    return pixels, hi


def downsample(pixels, hi: int, size: int):
    out = []
    scale = hi // size
    inv = 1.0 / (scale * scale)
    for y in range(size):
        for x in range(size):
            ar = ag = ab = aa = 0.0
            for sy in range(scale):
                row = (y * scale + sy) * hi
                for sx in range(scale):
                    r, g, b, a = pixels[row + x * scale + sx]
                    af = a / 255.0
                    ar += r * af
                    ag += g * af
                    ab += b * af
                    aa += af
            if aa <= 0.0001:
                out.append((0, 0, 0, 0))
                continue
            out.append(
                (
                    int(round(ar / aa)),
                    int(round(ag / aa)),
                    int(round(ab / aa)),
                    int(round(aa * inv * 255.0)),
                )
            )
    return out


def ico_bitmap_bits(rgba, size: int):
    header = struct.pack(
        "<IiiHHIIiiII",
        40,
        size,
        size * 2,
        1,
        32,
        0,
        size * size * 4,
        0,
        0,
        0,
        0,
    )
    body = bytearray()
    for y in range(size - 1, -1, -1):
        for x in range(size):
            r, g, b, a = rgba[y * size + x]
            body += bytes((b, g, r, a))
    mask_row = ((size + 31) // 32) * 4
    mask = bytearray()
    for y in range(size - 1, -1, -1):
        row = bytearray(mask_row)
        for x in range(size):
            if rgba[y * size + x][3] < 128:
                row[x // 8] |= 0x80 >> (x % 8)
        mask += row
    return header + bytes(body) + bytes(mask)


def build_ico(path: str):
    images = []
    for size in SIZES:
        pixels, hi = render_hires(size)
        rgba = downsample(pixels, hi, size)
        images.append((size, ico_bitmap_bits(rgba, size)))

    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries = bytearray()
    data = bytearray()
    for size, bits in images:
        entries += struct.pack(
            "<BBBBHHII",
            0 if size >= 256 else size,
            0 if size >= 256 else size,
            0,
            0,
            1,
            32,
            len(bits),
            offset,
        )
        data += bits
        offset += len(bits)

    with open(path, "wb") as fh:
        fh.write(header)
        fh.write(entries)
        fh.write(data)


def write_raw_rgba(path: str, size: int):
    pixels, hi = render_hires(size)
    rgba = downsample(pixels, hi, size)
    with open(path, "wb") as fh:
        for r, g, b, a in rgba:
            fh.write(bytes((r, g, b, a)))


def write_png(path: str, size: int, ss: int = SS):
    pixels, hi = render_hires(size, ss)
    rgba = downsample(pixels, hi, size)
    raw = bytearray()
    for y in range(size):
        raw.append(0)
        for x in range(size):
            raw += bytes(rgba[y * size + x])

    def chunk(tag: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + tag
            + payload
            + struct.pack(">I", zlib.crc32(tag + payload) & 0xFFFFFFFF)
        )

    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as fh:
        fh.write(png)


def main():
    os.makedirs(RES, exist_ok=True)
    build_ico(os.path.join(RES, "Scroder.ico"))
    write_raw_rgba(os.path.join(RES, "icon_256.rgba"), 256)
    write_png(os.path.join(RES, "icon_256.png"), 256)
    write_png(os.path.join(RES, "icon_1024.png"), 1024, ss=1)
    print("Generated resources/Scroder.ico, icon_256.rgba, icon_256.png, icon_1024.png")


if __name__ == "__main__":
    main()
