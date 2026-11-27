#!/usr/bin/env python3
"""Real part of the colour-ring bench: camera files, the same crop from
every developer, side by side.

    python bench/bayer-rings/run_real.py --out out/bayer-rings \\
        --crop ringe:0.636,0.779,440 NikonZF.nef SonyA7IV.arw

Every method develops the whole file on the terms of `developers.py`
(as-shot balance, linear, nothing sharpened or denoised). The frames are
then brought to one geometry (a whole-pixel shift, the programs crop the
sensor differently) and to one colour: each is fitted with one 3x3 to the
LibRaw AHD frame, on the frame reduced 16 times, where no demosaicer
differs from another. The camera's matrix takes that colour to sRGB, the
sRGB curve and one exposure for all follow. What is left to see in a crop
is the demosaicing.

`--crop NAME:CX,CY,SIZE` is a square of SIZE px around the point CX, CY,
both as fractions of the frame; the default is where the colour rings lie
in the DPReview studio scene. Each crop is drawn twice: a selection of the
methods, and all of them one step smaller. There is no original to measure
against, so this part gives pictures. The one figure is for a crop named in
`--neutral`, whose subject is black and white: the mean distance in a*b*
from the crop's median, which is colour that was not in the subject.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

import developers as dev
import metrics
import montage
from scenes import SRGB_TO_XYZ

REFERENCE = "lr_ahd"
DPREVIEW_RINGS = "ringe:0.636,0.779,200"
SELECTION = [
    "mz_dubois", "mz_adaptive", "mz_off", "mz_ls", "lr_ahd", "lr_dcb",
    "lr_dht", "rt_amaze", "rt_rcd", "rt_lmmse", "dt_rcd", "dt_amaze",
]  # fmt: skip
PROGRAMS = {"mimizan": "Mimizan", "libraw": "LibRaw", "rawtherapee": "RawTherapee", "darktable": "darktable"}


def shift_between(ref, img, window=1024):
    """Whole-pixel (dx, dy) with img[y + dy, x + dx] == ref[y, x], from the
    peak of the cross-correlation of the green channels in the middle."""
    h = min(ref.shape[0], img.shape[0])
    w = min(ref.shape[1], img.shape[1])
    n = min(window, (h // 2) * 2 - 64, (w // 2) * 2 - 64)
    y0, x0 = (h - n) // 2, (w - n) // 2
    a = ref[y0 : y0 + n, x0 : x0 + n, 1].astype(np.float64)
    b = img[y0 : y0 + n, x0 : x0 + n, 1].astype(np.float64)
    a, b = a - a.mean(), b - b.mean()
    corr = np.fft.irfft2(np.fft.rfft2(b) * np.conj(np.fft.rfft2(a)), s=a.shape)
    dy, dx = np.unravel_index(np.argmax(corr), corr.shape)
    dy = dy - n if dy > n // 2 else dy
    dx = dx - n if dx > n // 2 else dx
    return int(dx), int(dy)


def placed(img, shape, dx, dy):
    """`img` moved onto the reference's grid; what it does not cover is 0."""
    out = np.zeros(shape, dtype=np.float32)
    h, w = shape[:2]
    ys, xs = max(0, -dy), max(0, -dx)
    ye, xe = min(h, img.shape[0] - dy), min(w, img.shape[1] - dx)
    out[ys:ye, xs:xe] = img[ys + dy : ye + dy, xs + dx : xe + dx]
    return out


def reduced(img, k=16):
    h, w = (img.shape[0] // k) * k, (img.shape[1] // k) * k
    return img[:h, :w].reshape(h // k, k, w // k, k, 3).mean(axis=(1, 3))


def colour_to_reference(img, ref):
    """The 3x3 that takes `img` to `ref`, fitted on both reduced 16 times,
    without the frame's edge and without anything near clipping."""
    a, b = reduced(img)[4:-4, 4:-4].reshape(-1, 3), reduced(ref)[4:-4, 4:-4].reshape(-1, 3)
    keep = (b.max(axis=1) < 0.85 * ref.max()) & (a.min(axis=1) > 0)
    m, *_ = np.linalg.lstsq(a[keep].astype(np.float64), b[keep].astype(np.float64), rcond=None)
    return m.T.astype(np.float32)


def camera_to_srgb(xyz_to_cam):
    m = np.asarray(xyz_to_cam) @ SRGB_TO_XYZ
    m = m / m.sum(axis=1, keepdims=True)
    return np.linalg.inv(m).astype(np.float32)


def parse_crop(text):
    name, rest = text.split(":")
    cx, cy, size = rest.split(",")
    return name, float(cx), float(cy), int(size)


def run_file(raw: Path, out: Path, crops, methods, progs, media: Path, zoom: int, cols: int, neutral=()):
    stem = raw.stem
    work = out / "real" / stem
    infos = {}
    for m in methods:
        if m.program == "reference" or not dev.available(m, progs):
            continue
        try:
            frame, infos[m.key] = dev.develop(m, raw, work, progs)
            print(f"  {stem}: {m.label} {frame.shape[1]}x{frame.shape[0]} {infos[m.key]['seconds']} s", flush=True)
            del frame
        except Exception as e:
            print(f"  {stem}: {m.label} failed: {str(e)[:400]}", file=sys.stderr, flush=True)
    if not infos:
        raise SystemExit(f"{raw}: no developer produced a frame")
    ref_key = REFERENCE if REFERENCE in infos else next(iter(infos))
    ref, _ = dev.develop(dev.BY_KEY[ref_key], raw, work, progs)
    matrix = next((i["xyz_to_cam"] for i in infos.values() if "xyz_to_cam" in i), None)
    to_srgb = camera_to_srgb(matrix) if matrix is not None else np.eye(3, dtype=np.float32)
    if matrix is None:
        print(f"  {stem}: no colour matrix at hand, the crops stay in the camera's channels", file=sys.stderr)
    luma = reduced(ref) @ np.array([0.25, 0.5, 0.25], dtype=np.float32)
    gain = float(np.clip(0.9 / np.percentile(luma, 99.5), 0.5, 8.0))
    h, w = ref.shape[:2]
    report = {"file": raw.name, "reference": ref_key, "gain": gain, "methods": {}, "crops": []}
    cut, stray = {}, {}
    # One frame in memory besides the reference: 24 MP in float are 290 MB each.
    for key in infos:
        img = ref if key == ref_key else dev.develop(dev.BY_KEY[key], raw, work, progs)[0]
        dx, dy = (0, 0) if key == ref_key else shift_between(ref, img)
        on_ref = img if (dx, dy) == (0, 0) and img.shape == ref.shape else placed(img, ref.shape, dx, dy)
        cm = np.eye(3, dtype=np.float32) if key == ref_key else colour_to_reference(on_ref, ref)
        report["methods"][key] = {
            "shift": [dx, dy],
            "size": [img.shape[1], img.shape[0]],
            "seconds": infos[key]["seconds"],
            "colour_matrix": cm.tolist(),
        }
        for name, cx, cy, size in crops:
            x0 = int(np.clip(round(cx * w - size / 2), 0, w - size))
            y0 = int(np.clip(round(cy * h - size / 2), 0, h - size))
            c = np.clip(on_ref[y0 : y0 + size, x0 : x0 + size] @ cm.T @ to_srgb.T * gain, 0.0, 1.0)
            cut[(name, key)] = montage.to_srgb8(c)
            if name in neutral:
                # A black-and-white subject has one a*, b*: what stands off it
                # is colour the developer made (and the lens, the same for all).
                lab = metrics.rgb_to_lab(c)
                ab = lab[..., 1:] - np.median(lab[..., 1:].reshape(-1, 2), axis=0)
                stray[(name, key)] = float(np.hypot(ab[..., 0], ab[..., 1]).mean())
        del on_ref, img
    media.mkdir(parents=True, exist_ok=True)
    for name, cx, cy, size in crops:
        panels = []
        for m in methods:
            if (name, m.key) not in cut:
                continue
            kind = "mimizan" if m.is_mimizan else "other"
            note = PROGRAMS[m.program]
            if (name, m.key) in stray:
                note += f" · Farbe im Schwarzweiß {stray[(name, m.key)]:.2f}".replace(".", ",")
            panels.append((m.key, (m.label, note, kind, cut[(name, m.key)])))
            np.save(work / f"crop-{name}-{m.key}.npy", cut[(name, m.key)])
        sub = "Weißabgleich wie aufgenommen, linear entwickelt, keine Schärfung, keine Rauschminderung; Farbe auf LibRaw AHD abgeglichen (3×3), sRGB-Kurve"
        images = {}
        for tag, chosen, z in (
            ("auswahl", [p for k, p in panels if k in SELECTION], zoom),
            ("alle", [p for _, p in panels], max(zoom - 1, 1)),
        ):
            if not chosen:
                continue
            path = media / f"real-{stem}-{name}-{tag}.png"
            title = f"{raw.name}: Ausschnitt „{name}“, {size}×{size} px, {z}× vergrößert"
            montage.grid(chosen, cols, z, title=title, subtitle=sub).save(path, optimize=True)
            images[tag] = str(path)
            print(f"  wrote {path}", flush=True)
        entry = {"name": name, "centre": [cx, cy], "size": size, "images": images}
        if name in neutral:
            entry["stray_colour"] = {k: v for (n, k), v in stray.items() if n == name}
        report["crops"].append(entry)
    (work / "real.json").write_text(json.dumps(report, indent=1))
    return report


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("raw", nargs="+", type=Path, help="Bayer camera files")
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--media", type=Path, default=None, help="folder for the comparison PNGs (default OUT/media)")
    ap.add_argument("--crop", action="append", default=[], help="NAME:CX,CY,SIZE; may be given more than once")
    ap.add_argument("--neutral", default="", help="comma-separated names of crops whose subject is black and white; the colour found in them is measured")
    ap.add_argument("--methods", default="", help="comma-separated keys; default every method")
    ap.add_argument("--zoom", type=int, default=4, help="enlargement of the selection; all methods are drawn one step smaller")
    ap.add_argument("--cols", type=int, default=6)
    a = ap.parse_args()
    progs = dev.programs()
    dev.DT_ICC = "LIN_REC2020"
    methods = [dev.BY_KEY[k] for k in a.methods.split(",")] if a.methods else dev.METHODS
    crops = [parse_crop(c) for c in (a.crop or [DPREVIEW_RINGS])]
    media = a.media or a.out / "media"
    for raw in a.raw:
        if not raw.is_file():
            print(f"{raw}: not there, skipped", file=sys.stderr)
            continue
        print(f"{raw.name}", flush=True)
        run_file(raw, a.out, crops, methods, progs, media, a.zoom, a.cols, set(filter(None, a.neutral.split(","))))


if __name__ == "__main__":
    main()
