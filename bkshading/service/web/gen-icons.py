#!/usr/bin/env python3
"""Deterministic generator for the bkshading PWA icons (issue 1305).

Pure stdlib (zlib + struct) PNG writer — NO new dependency (no Pillow). Draws a simple
aperture/lens glyph on the panel's dark background so the installed web app has a recognisable
icon in the Windows dock / Start. Emits:

  * icon-192.png / icon-512.png  — RGBA, opaque; the glyph sits inside the central ~60% so the
                                    icons are safe as a maskable icon (the crop keeps the glyph).
  * favicon.svg                  — the same glyph as scalable SVG.

Run from anywhere: it writes next to itself (bkshading/service/web/). Committed as small binaries
so the service embeds them via include_bytes! (self-contained, no runtime file dependency). Re-run
after changing the palette/glyph; the output is deterministic (fixed geometry + zlib level 9).
"""
import os
import struct
import zlib

# Palette (matches style.css): bg #14171c, accent #4f9dff, inner panel #262c36, text #e7ecf2.
BG = (20, 23, 28)
RING = (79, 157, 255)
DISK = (38, 44, 54)
HI = (231, 236, 242)

HERE = os.path.dirname(os.path.abspath(__file__))


def _sample(fx, fy, size):
    """Topmost glyph colour at point (fx, fy) on an `size`x`size` canvas."""
    cx = cy = size / 2.0
    r_out = 0.30 * size
    r_in = 0.22 * size
    hi_cx = cx - 0.09 * size
    hi_cy = cy - 0.09 * size
    r_hi = 0.05 * size
    dh = ((fx - hi_cx) ** 2 + (fy - hi_cy) ** 2) ** 0.5
    if dh <= r_hi:
        return HI
    d = ((fx - cx) ** 2 + (fy - cy) ** 2) ** 0.5
    if d <= r_in:
        return DISK
    if d <= r_out:
        return RING
    return BG


def _render_rgba(size):
    """Raw PNG scanlines (each prefixed with a 0 filter byte), RGBA, 3x3 supersampled edges."""
    ss = 3
    n = ss * ss
    raw = bytearray()
    for y in range(size):
        raw.append(0)  # filter type 0 (None)
        for x in range(size):
            r = g = b = 0
            for sy in range(ss):
                for sx in range(ss):
                    fx = x + (sx + 0.5) / ss
                    fy = y + (sy + 0.5) / ss
                    cr, cg, cb = _sample(fx, fy, size)
                    r += cr
                    g += cg
                    b += cb
            raw += bytes((round(r / n), round(g / n), round(b / n), 255))
    return bytes(raw)


def _chunk(tag, data):
    return (
        struct.pack(">I", len(data))
        + tag
        + data
        + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
    )


def _write_png(path, size):
    raw = _render_rgba(size)
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # 8-bit, colour type 6 (RGBA)
    idat = zlib.compress(raw, 9)
    png = b"\x89PNG\r\n\x1a\n" + _chunk(b"IHDR", ihdr) + _chunk(b"IDAT", idat) + _chunk(b"IEND", b"")
    with open(path, "wb") as fh:
        fh.write(png)
    print(f"wrote {path} ({len(png)} bytes)")


def _hex(c):
    return "#%02x%02x%02x" % c


def _write_svg(path):
    # viewBox 0..64, geometry scaled from the same fractions as the PNG glyph.
    svg = (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" '
        'role="img" aria-label="bkshading">\n'
        f'  <rect width="64" height="64" rx="12" fill="{_hex(BG)}"/>\n'
        f'  <circle cx="32" cy="32" r="19.2" fill="{_hex(RING)}"/>\n'
        f'  <circle cx="32" cy="32" r="14.08" fill="{_hex(DISK)}"/>\n'
        f'  <circle cx="26.24" cy="26.24" r="3.2" fill="{_hex(HI)}"/>\n'
        "</svg>\n"
    )
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(svg)
    print(f"wrote {path} ({len(svg)} bytes)")


def main():
    _write_png(os.path.join(HERE, "icon-192.png"), 192)
    _write_png(os.path.join(HERE, "icon-512.png"), 512)
    _write_svg(os.path.join(HERE, "favicon.svg"))


if __name__ == "__main__":
    main()
