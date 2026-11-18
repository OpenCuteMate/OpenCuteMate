#!/usr/bin/env python3
"""Pixel-validate an OpenCuteMate overlay screenshot.

The compositor background is a known solid color (magenta #FF00FF). A correct
PoC frame therefore contains, in one image:
  * fully transparent pixels  -> exact magenta (desktop visible through us)
  * opaque pixels            -> the cube's face colors (not magenta)
  * partially transparent px  -> the premultiplied shadow: magenta darkened
                                by a factor of (1 - a) for a in [0..0.42]

usage: analyze_screenshot.py IMG LOGICAL_W LOGICAL_H POS_X POS_Y SIZE_FRAC [--json]
exit 0 if all checks pass, 1 otherwise.
"""
from __future__ import annotations

import json
import sys

from PIL import Image

BG = (255, 0, 255)  # magenta
CUBE_TOL = 60.0  # distance from magenta to count as "not background"


def dist2(a, b):
    return sum((a[i] - b[i]) ** 2 for i in range(3))


def main():
    img_path, w, h, px, py, size_frac = sys.argv[1:7]
    w, h = int(w), int(h)
    px, py, size_frac = float(px), float(py), float(size_frac)
    as_json = "--json" in sys.argv

    image = Image.open(img_path).convert("RGB")
    iw, ih = image.size
    # The compositor screenshot may be at a different resolution than the
    # logical size (scale); map logical -> pixels.
    sx, sy = iw / w, ih / h

    results = {}
    ok = True

    def check(name, passed, detail):
        nonlocal ok
        results[name] = {"pass": bool(passed), "detail": detail}
        ok = ok and passed

    # --- A: transparent areas show the exact compositor background ---------
    corner_samples = []
    insets = [(40, 40), (iw - 40, 40), (40, ih - 40), (iw - 40, ih - 40)]
    for cx, cy in insets:
        corner_samples.append(image.getpixel((cx, cy)))
    bg_ok = all(dist2(c, BG) < 3 * 8 * 8 for c in corner_samples)
    check("background_visible_through_overlay", bg_ok,
          {"corners": corner_samples, "expected": list(BG)})

    # --- B: the cube is drawn near the requested position ------------------
    cx, cy = int(px * w * sx), int(py * h * sy)
    cube_half = int(size_frac * h * sy / 2 * 0.8)
    x0, x1 = max(0, cx - cube_half), min(iw, cx + cube_half)
    y0, y1 = max(0, cy - cube_half), min(ih, cy + cube_half)
    cube_px = 0
    sample_box = 0
    for yy in range(y0, y1, 2):
        for xx in range(x0, x1, 2):
            p = image.getpixel((xx, yy))
            sample_box += 1
            if dist2(p, BG) > CUBE_TOL ** 2:
                cube_px += 1
    frac = cube_px / max(sample_box, 1)
    check("cube_visible_at_position", frac > 0.25,
          {"non_bg_fraction": round(frac, 3), "box": [x0, y0, x1, y1]})

    # --- C: partially transparent pixels (shadow gradient) ------------------
    # Scan a band below the cube for magenta darkened by 5%..55%: that is the
    # premultiplied shadow blending over the background.
    band_y0 = int(cy + cube_half * 0.7)
    band_y1 = min(int(cy + cube_half * 1.9), ih)
    partial = 0
    samples = 0
    for yy in range(band_y0, band_y1, 1):
        for xx in range(x0, x1, 1):
            r, g, b = image.getpixel((xx, yy))
            samples += 1
            # darkened magenta: r ~= b in [0.45*255 .. 0.95*255], g small,
            # r/b ratio near 1 (background scaled by (1 - alpha))
            if 110 < r < 242 and abs(r - b) < 25 and g < 70 and r > b * 0.9:
                partial += 1
    # The shadow's dark center should be well-represented near the band top.
    check("partially_transparent_pixels", partial > 150,
          {"partial_count": partial, "band": [x0, band_y0, x1, band_y1]})

    # --- D: fully transparent pixels dominate the image --------------------
    hist = image.histogram()
    total = iw * ih
    magentaish = sum(hist[255 - 4:255 + 5])  # R near 255
    # simpler: sample grid
    hits = 0
    grid = 40
    for yy in range(0, ih, ih // grid):
        for xx in range(0, iw, iw // grid):
            if dist2(image.getpixel((xx, yy)), BG) < 3 * 8 * 8:
                hits += 1
    frac_bg = hits / (grid * grid)
    # The character (cube + shadow + hover tint) legitimately covers
    # 10-20% of a fullscreen surface; the check only guards against the
    # surface being mostly opaque.
    check("mostly_transparent_surface", frac_bg > 0.70,
          {"bg_fraction_sampled": round(frac_bg, 3)})

    if as_json:
        print(json.dumps(results, indent=2))
    else:
        for name, r in results.items():
            status = "PASS" if r["pass"] else "FAIL"
            print(f"[{status}] {name}: {r['detail']}")
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
