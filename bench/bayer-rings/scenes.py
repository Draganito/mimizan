"""Ground-truth scenes of extreme colour geometry.

Every chart is a grid of tiles. A tile holds one two-colour pattern: a
foreground colour on a background colour, mixed by a scalar field `t` in
0..1. The chart is rendered at `SUPER` samples a pixel side, blurred there
by the lens (a Gaussian, sigma in pixels) and averaged over the pixel (a
100 % fill-factor aperture), so the truth is what an ideal three-colour
sensor with the same pixels would have recorded behind the same lens.

Charts:

* `dpr`     the DPReview ring target rebuilt: per colour pair a disc of
            concentric rings, coarse on one half, twice as fine on the
            other, four times as fine in a central diamond.
* `rings`   per colour pair a row of ring patches of one period each, from
            8 px a cycle down to 1.6 px (Nyquist is 2 px).
* `zone`    sinusoidal colour zone plates, frequency rising with radius.
* `siemens` colour Siemens stars, frequency rising towards the centre.
* `edges`   slanted colour edges at six angles.
* `grids`   pixel-fine line gratings, checkers, hairline grids and dots.

Each chart ends in a strip of flat patches of every colour used; a
developer's colour rendering is fitted on them (3x3) before any error is
measured, so a difference in colour management is not counted as a
demosaicing error.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np
from scipy import ndimage

SUPER = 8
STRIP = 48  # output rows rendered at once
PATCH_H = 72  # height of the calibration strip

# Saturated colours, linear light, channels of an ideal sRGB-primaries sensor.
SAT = {
    "magenta": (0.80, 0.08, 0.55),
    "orange": (0.90, 0.38, 0.04),
    "blue": (0.04, 0.10, 0.70),
    "red": (0.75, 0.04, 0.05),
    "cyan": (0.05, 0.60, 0.80),
    "yellow": (0.88, 0.78, 0.05),
    "green": (0.08, 0.60, 0.10),
    "white": (0.88, 0.88, 0.88),
    "black": (0.03, 0.03, 0.03),
}
GREYS = {"grey20": (0.20,) * 3, "grey45": (0.45,) * 3, "grey70": (0.70,) * 3}

# (foreground, background). The first four are the pairs of the DPReview target.
PAIRS = [
    ("magenta", "orange"),
    ("blue", "red"),
    ("cyan", "blue"),
    ("yellow", "cyan"),
    ("red", "green"),
    ("green", "magenta"),
    ("blue", "yellow"),
    ("black", "white"),
]

SRGB_TO_XYZ = np.array(
    [[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]]
)
# Nikon Z f, XYZ (D65) -> camera, as rawler and the Adobe tables state it.
ZF_XYZ_TO_CAM = np.array(
    [[1.1607, -0.4491, -0.0977], [-0.4522, 1.2460, 0.2304], [-0.0458, 0.1519, 0.7616]]
)


def camera_palette(colours: dict) -> dict:
    """The colours as the balanced channels of a Nikon Z f see them: the
    filters of a real sensor overlap, so a saturated colour is paler in the
    camera's channels than in sRGB primaries."""
    m = ZF_XYZ_TO_CAM @ SRGB_TO_XYZ
    m = m / m.sum(axis=1, keepdims=True)  # white balance: grey stays grey
    return {k: tuple(float(x) for x in np.clip(m @ np.asarray(v), 0.0, 1.0)) for k, v in colours.items()}


def palette(name: str) -> dict:
    base = {**SAT, **GREYS}
    if name == "sat":
        return base
    if name == "cam":
        return camera_palette(base)
    raise ValueError(f"unknown palette {name}")


@dataclass
class Tile:
    kind: str
    pair: tuple
    x: int
    y: int
    w: int
    h: int
    params: dict = field(default_factory=dict)

    def meta(self) -> dict:
        return {
            "kind": self.kind,
            "pair": list(self.pair),
            "rect": [self.x, self.y, self.w, self.h],
            "params": self.params,
        }


# ---------------------------------------------------------------- patterns
# Each takes coordinates relative to the tile centre, in pixels, and returns
# the share of the foreground colour.


def _square(phase):
    return (np.mod(phase, 1.0) < 0.5).astype(np.float32)


def p_rings(x, y, period, ox=0.37, oy=0.21):
    return _square(np.hypot(x - ox, y - oy) / period)


def p_dpr(x, y, period, radius):
    """The DPReview target: a disc of rings, cut by a diagonal into a coarse
    and a fine half, a diamond of the finest rings in the middle."""
    r = np.hypot(x - 0.37, y - 0.21)
    coarse = _square(r / period)
    fine = _square(r / (period / 2.0))
    finest = _square(r / (period / 4.0))
    t = np.where(x + y < 0, coarse, fine)
    t = np.where(np.abs(x) + np.abs(y) < radius * 0.30, finest, t)
    return np.where(r < radius, t, 0.0).astype(np.float32)


def p_zone(x, y, n):
    r2 = (x - 0.37) ** 2 + (y - 0.21) ** 2
    return (0.5 + 0.5 * np.cos(np.pi * r2 / n)).astype(np.float32)


def p_siemens(x, y, cycles, radius):
    xx, yy = x - 0.37, y - 0.21
    t = _square(np.arctan2(yy, xx) * cycles / (2 * np.pi))
    return np.where(np.hypot(xx, yy) < radius, t, 0.0).astype(np.float32)


def p_edge(x, y, angle_deg):
    a = np.deg2rad(angle_deg)
    return ((x - 0.37) * np.cos(a) + (y - 0.21) * np.sin(a) > 0).astype(np.float32)


def p_lines(x, y, period, angle_deg, offset=0.0):
    a = np.deg2rad(angle_deg)
    return _square(((x + offset) * np.cos(a) + (y + offset) * np.sin(a)) / period)


def p_checker(x, y, cell):
    return ((np.floor(x / cell) + np.floor(y / cell)) % 2).astype(np.float32)


def p_hairgrid(x, y, pitch):
    return ((np.mod(np.floor(x), pitch) == 0) | (np.mod(np.floor(y), pitch) == 0)).astype(np.float32)


def p_dots(x, y, pitch):
    return ((np.mod(np.floor(x), pitch) == 0) & (np.mod(np.floor(y), pitch) == 0)).astype(np.float32)


def p_flat(x, y):
    return np.ones_like(x, dtype=np.float32)


PATTERNS = {
    "rings": p_rings,
    "dpr": p_dpr,
    "zone": p_zone,
    "siemens": p_siemens,
    "edge": p_edge,
    "lines": p_lines,
    "checker": p_checker,
    "hairgrid": p_hairgrid,
    "dots": p_dots,
    "flat": p_flat,
}

RING_PERIODS = [8.0, 6.0, 5.0, 4.0, 3.5, 3.0, 2.7, 2.4, 2.2, 2.0, 1.8, 1.6]
EDGE_ANGLES = [4.0, 14.0, 31.0, 47.0, 76.0, 86.0]
ZONE_N = 320.0  # local frequency r / ZONE_N cycles a pixel
SIEMENS_CYCLES = 60
GRIDS = [
    ("lines", {"period": 2.0, "angle_deg": 0.0}, "senkrechte Linien, Periode 2 px"),
    ("lines", {"period": 2.0, "angle_deg": 90.0}, "waagrechte Linien, Periode 2 px"),
    ("lines", {"period": 3.0, "angle_deg": 0.0}, "senkrechte Linien, Periode 3 px"),
    ("lines", {"period": 4.0, "angle_deg": 0.0}, "senkrechte Linien, Periode 4 px"),
    ("lines", {"period": 2.5, "angle_deg": 8.0}, "Linien 8° geneigt, Periode 2,5 px"),
    ("lines", {"period": 2.83, "angle_deg": 45.0}, "Diagonale Linien, Periode 2,83 px"),
    ("checker", {"cell": 1.0}, "Schachbrett 1 px"),
    ("checker", {"cell": 2.0}, "Schachbrett 2 px"),
    ("hairgrid", {"pitch": 5.0}, "Haarlinien-Gitter, 1 px alle 5 px"),
    ("dots", {"pitch": 3.0}, "Punkte, 1 px alle 3 px"),
]


def _grid(kind_params, tile, pairs=PAIRS):
    """Rows are the colour pairs, columns the entries of `kind_params`."""
    tiles = []
    for row, pair in enumerate(pairs):
        for col, (kind, params) in enumerate(kind_params):
            tiles.append(Tile(kind, pair, col * tile, row * tile, tile, tile, dict(params)))
    return tiles, len(kind_params) * tile, len(pairs) * tile


def chart(name: str):
    """Tiles, width and height of the chart without its calibration strip."""
    if name == "rings":
        return _grid([("rings", {"period": p}) for p in RING_PERIODS], 104)
    if name == "edges":
        return _grid([("edge", {"angle_deg": a}) for a in EDGE_ANGLES], 128)
    if name == "grids":
        return _grid([(k, p) for k, p, _ in GRIDS], 96)
    if name in ("zone", "siemens", "dpr"):
        size = {"zone": 400, "siemens": 320, "dpr": 360}[name]
        tiles = []
        for i, pair in enumerate(PAIRS):
            x, y = (i % 4) * size, (i // 4) * size
            if name == "zone":
                tiles.append(Tile("zone", pair, x, y, size, size, {"n": ZONE_N}))
            elif name == "siemens":
                tiles.append(
                    Tile("siemens", pair, x, y, size, size, {"cycles": SIEMENS_CYCLES, "radius": size * 0.47})
                )
            else:
                # Top row: 12 / 6 / 3 px a cycle. Bottom row, the same four
                # DPReview pairs again: 8.8 / 4.4 / 2.2 px.
                pair = PAIRS[i % 4]
                period = 12.0 if i < 4 else 8.8
                tiles.append(Tile("dpr", pair, x, y, size, size, {"period": period, "radius": size * 0.46}))
        return tiles, 4 * size, 2 * size
    raise ValueError(f"unknown chart {name}")


CHARTS = ["dpr", "rings", "zone", "siemens", "edges", "grids"]


def _calibration(width: int, y: int, colours: dict):
    names = list(colours)
    edges = np.linspace(0, width, len(names) + 1).round().astype(int)
    return [
        Tile("flat", (n, n), int(edges[i]), y, int(edges[i + 1] - edges[i]), PATCH_H)
        for i, n in enumerate(names)
    ]


def render(name: str, palette_name: str, sigma: float):
    """The truth (H, W, 3) float32, linear, and the tiles with the
    calibration strip appended."""
    colours = palette(palette_name)
    tiles, width, body_h = chart(name)
    tiles = tiles + _calibration(width, body_h, colours)
    height = body_h + PATCH_H
    s = SUPER
    margin = int(np.ceil(4 * sigma)) + 1 if sigma > 0 else 0
    out = np.zeros((height, width, 3), dtype=np.float32)
    for r0 in range(0, height, STRIP):
        r1 = min(r0 + STRIP, height)
        a0, a1 = max(r0 - margin, 0), min(r1 + margin, height)
        rows = (a1 - a0) * s
        buf = np.zeros((rows, width * s, 3), dtype=np.float32)
        for t in tiles:
            ty0, ty1 = max(t.y, a0), min(t.y + t.h, a1)
            if ty0 >= ty1:
                continue
            ys = (np.arange(ty0 * s, ty1 * s) + 0.5) / s - (t.y + t.h / 2.0)
            xs = (np.arange(t.x * s, (t.x + t.w) * s) + 0.5) / s - (t.x + t.w / 2.0)
            xx, yy = np.meshgrid(xs.astype(np.float32), ys.astype(np.float32))
            share = PATTERNS[t.kind](xx, yy, **t.params)
            fg = np.asarray(colours[t.pair[0]], dtype=np.float32)
            bg = np.asarray(colours[t.pair[1]], dtype=np.float32)
            block = bg + share[..., None] * (fg - bg)
            buf[(ty0 - a0) * s : (ty1 - a0) * s, t.x * s : (t.x + t.w) * s] = block
        if sigma > 0:
            for c in range(3):
                buf[..., c] = ndimage.gaussian_filter(buf[..., c], sigma * s, mode="nearest", truncate=4.0)
        small = buf.reshape(a1 - a0, s, width, s, 3).mean(axis=(1, 3))
        out[r0:r1] = small[r0 - a0 : r1 - a0]
    return out, tiles


# The noise of a base-ISO exposure, the model Mimizan assumes for a file it
# has no camera file for: sigma = a * sqrt(signal) + b.
NOISE_A, NOISE_B = 0.005, 0.0002


def mosaic(truth: np.ndarray, seed: int = 1, noise: bool = True) -> np.ndarray:
    """RGGB mosaic of `truth`, with noise, quantised to 16 bit."""
    h, w, _ = truth.shape
    m = truth[..., 1].astype(np.float64).copy()
    m[0::2, 0::2] = truth[0::2, 0::2, 0]
    m[1::2, 1::2] = truth[1::2, 1::2, 2]
    if noise:
        rng = np.random.default_rng(seed)
        m = m + rng.standard_normal(m.shape) * (NOISE_A * np.sqrt(np.clip(m, 0, None)) + NOISE_B)
    return np.round(np.clip(m, 0.0, 1.0) * 65535.0).astype(np.uint16)


def save(out_dir: Path, name: str, truth: np.ndarray, tiles, extra: dict):
    out_dir.mkdir(parents=True, exist_ok=True)
    np.save(out_dir / f"{name}.truth.npy", truth)
    meta = {"tiles": [t.meta() for t in tiles], **extra}
    (out_dir / f"{name}.json").write_text(json.dumps(meta, indent=1))
