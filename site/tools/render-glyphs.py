#!/usr/bin/env python3
"""Render the shell's own glyphs for the website, in the shell's own material.

Every glyph LineXinBar ships is a *shape*: a white silhouette the shell
measures into a signed distance field (`icons::builtin_distance_field`) and
the quad shader stands a bead of water up out of (`glyph_material` in
`crates/lxb-desktop/src/shaders.wgsl`). A web page has neither, so this does
both, offline, the same way:

  1. rasterise the SVG at four times the 128-texel cell (rsvg-convert),
  2. threshold its alpha at half, take the exact Euclidean distance transform
     inside and out, average it back down to the cell and store it as the
     shell stores it (0.5 on the edge, SDF_RANGE of the cell either way),
  3. shade that field with a line-for-line port of `glyph_material`, in linear
     light, and write the result as an sRGB WebP with straight alpha.

The material is white and does not depend on the accent, which is why a
picture of it can stand in for the shader: what a glyph shows of the palette
comes through it from the tile it sits on, exactly as in the shell.

Usage:  python3 site/tools/render-glyphs.py            (from the repository root)
Needs:  rsvg-convert, numpy, scipy, Pillow.
"""

from __future__ import annotations

import io
import subprocess
import sys
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "site" / "assets" / "glyphs"

SOURCES = [
    ROOT / "crates" / "lxb-desktop" / "src" / "glyphs",
    ROOT / "crates" / "lxb-retroarch" / "glyphs",
    ROOT / "crates" / "lxb-heroic" / "glyphs",
    ROOT / "crates" / "lxb-rpcs3" / "glyphs",
    ROOT / "site" / "tools" / "glyphs",
]

# The glyphs the site draws. Kept to what the pages use, so the site does not
# carry two hundred pictures to show forty.
GLYPHS = """
logo category-games category-steam category-multimedia category-music
category-video category-images category-files category-settings category-system
category-software category-development category-trophies category-internet
category-utilities category-waydroid category-office category-graphics
category-education category-other steam retroarch epic ps3 console-psx
setting-display setting-hdr setting-night-light setting-screen-rest
setting-resolution setting-refresh setting-orientation setting-scale
setting-accent setting-theme setting-wallpaper setting-particles setting-icons
setting-appearance setting-language setting-network setting-wifi
setting-bluetooth setting-users setting-updates setting-system setting-info
setting-input setting-keyboard setting-mouse setting-pip setting-person
setting-compatibility setting-install-to setting-update-all setting-schedule
setting-microphone setting-typed setting-order setting-keys setting-layout
pad-guide pad-south pad-east pad-north pad-west pad-stick pad-left-bumper
pad-right-bumper pad-start pad-select key-enter key-escape key-super key-space
key-shift mouse-right search screenshot volume volume-mixer notifications
do-not-disturb brightness pointer-stick launch shutdown sign-out file-folder
file-drive file-home compress extract copy move rename trash-empty
authenticate media-play media-next keyboard-hide swatch chosen refresh add
arrow-right arrow-left open-with signal-strong battery-high sort select-multiple
key-g key-p volume-muted setting-connect file-page
""".split()

# Sizes the pictures are made at: large enough for a focused category tile on
# a high-density display, and small enough for a legend or a guide tile.
SIZES = {"lg": 320, "sm": 96}

# The shell's numbers, as the shader and `icons` spell them.
CELL = 128
SUPERSAMPLE = 4
SDF_RANGE = 0.125
GLYPH_DEPTH = 0.075
GLYPH_LAMP = np.array([-0.4915, -0.7078, 0.5069])
GLYPH_SHADOW = 0.30
GLASS_DISPERSION = 0.055


def find(name: str) -> Path:
    for folder in SOURCES:
        path = folder / f"{name}.svg"
        if path.exists():
            return path
    raise SystemExit(f"no glyph called {name}")


def distance_field(svg: Path) -> np.ndarray:
    """The cell as the shell stores it: 0..1, 0.5 on the edge."""
    fine = CELL * SUPERSAMPLE
    png = subprocess.run(
        ["rsvg-convert", "-w", str(fine), "-h", str(fine), str(svg)],
        check=True,
        capture_output=True,
    ).stdout
    alpha = np.asarray(Image.open(io.BytesIO(png)).convert("RGBA"))[..., 3]
    inside = alpha >= 128
    # How far each pixel outside is from the shape, and each pixel inside from
    # getting out: their difference is the signed field, zero on the boundary.
    out = ndimage.distance_transform_edt(~inside)
    within = ndimage.distance_transform_edt(inside)
    field = (out - within).reshape(CELL, SUPERSAMPLE, CELL, SUPERSAMPLE).mean(axis=(1, 3))
    stored = 0.5 + (field / fine) / (2.0 * SDF_RANGE)
    # Eight bits, as the atlas holds it: the quantisation is part of the look.
    return np.round(np.clip(stored, 0.0, 1.0) * 255.0) / 255.0


def sample(field: np.ndarray, u: np.ndarray, v: np.ndarray) -> np.ndarray:
    """Bilinear, clamped half a texel inside the cell, as `glyph_at` reads it."""
    lo, hi = 0.5 / CELL, 1.0 - 0.5 / CELL
    x = np.clip(u, lo, hi) * CELL - 0.5
    y = np.clip(v, lo, hi) * CELL - 0.5
    x0 = np.floor(x).astype(int)
    y0 = np.floor(y).astype(int)
    fx, fy = x - x0, y - y0
    x1 = np.minimum(x0 + 1, CELL - 1)
    y1 = np.minimum(y0 + 1, CELL - 1)
    top = field[y0, x0] * (1 - fx) + field[y0, x1] * fx
    bottom = field[y1, x0] * (1 - fx) + field[y1, x1] * fx
    stored = top * (1 - fy) + bottom * fy
    return (stored - 0.5) * 2.0 * SDF_RANGE


def smoothstep(a: float, b, x):
    t = np.clip((x - a) / (b - a), 0.0, 1.0)
    return t * t * (3.0 - 2.0 * t)


def bevel_rise(inset):
    return np.sqrt(np.maximum(1.0 - (1.0 - inset) ** 2, 0.0))


def bevel_slope(inset):
    return (1.0 - inset) / np.maximum(bevel_rise(inset), 0.16)


def normalize(v):
    return v / np.maximum(np.linalg.norm(v, axis=-1, keepdims=True), 1e-12)


def material(field: np.ndarray, size: int) -> np.ndarray:
    """`glyph_material`, for a white mark at full gloss: linear RGBA, straight."""
    px = (np.arange(size) + 0.5) / size
    u, v = np.meshgrid(px, px)
    local_y = v * size
    per_pixel = 1.0 / size

    d = sample(field, u, v) * size
    coverage = 1.0 - smoothstep(-0.75, 0.75, d)

    arm = 1.5 / CELL
    east = sample(field, u + arm, v)
    west = sample(field, u - arm, v)
    south = sample(field, u, v + arm)
    north = sample(field, u, v - arm)
    gradient = np.stack([east - west, south - north], axis=-1)
    outward = normalize(gradient + np.array([1e-6, 0.0]))
    slope = np.clip(np.linalg.norm(gradient, axis=-1) / (2.0 * 1.5 / CELL), 0.0, 1.0)
    ridge = smoothstep(0.20, 0.80, slope)

    slab = max(size * GLYPH_DEPTH, 0.5)
    inset = np.clip(-d / slab, 0.0, 1.0)
    tilt = outward * (bevel_slope(inset) * ridge)[..., None]
    surface = normalize(np.concatenate([tilt, np.ones_like(d)[..., None]], axis=-1))

    foot = np.clip(local_y / size, 0.0, 1.0)
    glass = np.repeat((0.50 + 0.50 * foot * foot)[..., None], 3, axis=-1)
    alpha = 0.52 + (0.94 - 0.52) * foot * foot

    facing = np.clip(surface @ GLYPH_LAMP, 0.0, 1.0)
    glass = glass * (0.70 + (1.26 - 0.70) * facing)[..., None]

    edge = (1.0 - inset) ** 2 * ridge
    split = GLASS_DISPERSION * edge * outward[..., 0]
    glass = glass * np.stack([1.0 + split, np.ones_like(split), 1.0 - split], axis=-1)

    fresnel = 0.04 + 0.96 * (1.0 - np.clip(surface[..., 2], 0.0, 1.0)) ** 5
    lit = fresnel
    mirrored = surface * (2.0 * surface[..., 2])[..., None]
    mirrored[..., 2] -= 1.0
    value = 0.42 + (1.15 - 0.42) * (0.5 + 0.5 * (mirrored @ -GLYPH_LAMP))
    glass = glass + (value[..., None] - glass) * lit[..., None]
    half_way = GLYPH_LAMP + np.array([0.0, 0.0, 1.0])
    half_way = half_way / np.linalg.norm(half_way)
    spec = np.clip(surface @ half_way, 0.0, 1.0) ** 42
    glass = glass + (spec * 0.9)[..., None]
    alpha = np.maximum(alpha, np.maximum(lit, spec))

    toward = GLYPH_LAMP[:2] / np.linalg.norm(GLYPH_LAMP[:2]) * slab * 0.22
    occluder = sample(field, u + toward[0] * per_pixel, v + toward[1] * per_pixel) * size
    blocked = 1.0 - smoothstep(-slab * 0.1, slab * 0.4, occluder)
    shade = blocked * GLYPH_SHADOW * (1.0 - coverage)

    mark = alpha * coverage
    total = mark + shade * (1.0 - mark)
    rgb = np.maximum(glass, 0.0) * (mark / np.maximum(total, 1e-4))[..., None]
    return np.concatenate([rgb, total[..., None]], axis=-1)


def encode(linear: np.ndarray) -> np.ndarray:
    c = np.clip(linear, 0.0, 1.0)
    return np.where(c <= 0.0031308, c * 12.92, 1.055 * np.power(c, 1.0 / 2.4) - 0.055)


def main() -> None:
    names = sys.argv[1:] or GLYPHS
    for key in SIZES:
        (OUT / key).mkdir(parents=True, exist_ok=True)
    for name in names:
        field = distance_field(find(name))
        for key, size in SIZES.items():
            rgba = material(field, size)
            rgb = encode(rgba[..., :3])
            pixels = np.round(np.concatenate([rgb, rgba[..., 3:]], axis=-1) * 255.0)
            image = Image.fromarray(pixels.astype(np.uint8), "RGBA")
            image.save(OUT / key / f"{name}.webp", "WEBP", quality=92, alpha_quality=100, method=6)
        print(name)


if __name__ == "__main__":
    main()
