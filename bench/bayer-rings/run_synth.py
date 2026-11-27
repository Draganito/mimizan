#!/usr/bin/env python3
"""Synthetic part of the colour-ring bench.

Renders every chart for every palette and lens, samples it through an RGGB
mosaic with base-ISO noise, writes the DNG, develops it with every method
that is installed and measures each tile against the original.

    python bench/bayer-rings/run_synth.py --out out/bayer-rings

Everything is cached under `--out`; a second run only measures.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import numpy as np

import developers as dev
import dngio
import metrics
import scenes

KODAK_URL = "https://r0k.us/graphics/kodak/kodak/{}.png"
KODAK = ["kodim01", "kodim05", "kodim08", "kodim13", "kodim19", "kodim23"]
CONTEXT_KEY = "mz_ls_ctx"
CONTEXT_FACTOR = 3  # the chart is a ninth of the photograph it lies in

LENSES = {
    "sharp": (0.0, "pixelscharf: nur die Pixelfläche, kein Objektiv (MTF an der Nyquist-Grenze 64 %)"),
    "lens": (0.5, "scharfes Objektiv ohne Tiefpassfilter: Gauß σ 0,5 px (MTF an der Nyquist-Grenze 19 %)"),
    "soft": (0.75, "Objektiv mit Tiefpassfilter: Gauß σ 0,75 px (MTF an der Nyquist-Grenze 4 %)"),
}


def scene_name(chart, palette, lens):
    return f"{chart}-{palette}-{lens}"


def photographs(folder: Path):
    """Six Kodak pictures (free for unrestricted use), fetched once, turned
    to landscape, enlarged 2x (Lanczos) so they are as sharp as a lens draws,
    linear. `None` when they cannot be had."""
    from PIL import Image
    from urllib.request import urlopen

    folder.mkdir(parents=True, exist_ok=True)
    out = []
    for name in KODAK:
        path = folder / f"{name}.png"
        if not path.exists():
            try:
                with urlopen(KODAK_URL.format(name), timeout=60) as r:
                    path.write_bytes(r.read())
            except OSError as e:
                print(f"no photographs for the context run ({e}); skipped", file=sys.stderr)
                return None
        im = Image.open(path).convert("RGB")
        if im.width < im.height:
            im = im.rotate(90, expand=True)
        im = im.resize((im.width * 2, im.height * 2), Image.LANCZOS)
        out.append(dev.srgb_decode(np.asarray(im, np.float32) / 255.0) * 0.9)
    return out


def context_canvas(truth, photos, factor=CONTEXT_FACTOR):
    """The chart lying in a frame `factor` times its size, the rest of it
    photographs; and where the chart lies in it, (x, y)."""
    h, w, _ = truth.shape
    ch, cw = (h * factor) // 2 * 2, (w * factor) // 2 * 2
    canvas = np.zeros((ch, cw, 3), dtype=np.float32)
    ph, pw = photos[0].shape[:2]
    i = 0
    for y in range(0, ch, ph):
        for x in range(0, cw, pw):
            p = photos[i % len(photos)]
            i += 1
            canvas[y : y + ph, x : x + pw] = p[: min(ph, ch - y), : min(pw, cw - x)]
    oy, ox = ((ch - h) // 4) * 2, ((cw - w) // 4) * 2
    canvas[oy : oy + h, ox : ox + w] = truth
    return canvas, ox, oy


def context_ls(work: Path, truth, photos, progs, factor=CONTEXT_FACTOR, key=CONTEXT_KEY):
    """The least-squares demosaic of the chart lying in a photograph.

    That demosaic searches one prior for the whole frame. A frame that is
    nothing but fine colour rings gives it a prior no photograph has, so
    the chart is also developed as a ninth of a frame of ordinary pictures,
    and the same chart pixels are measured."""
    h, w, _ = truth.shape
    cache, side = work / f"{key}.npy", work / f"{key}.json"
    if cache.exists() and side.exists():
        return np.load(cache).astype(np.float32) / 65535.0, json.loads(side.read_text())
    canvas, ox, oy = context_canvas(truth, photos, factor)
    dng = work / f"{key}.dng"
    dngio.write_cfa_dng(dng, scenes.mosaic(canvas, seed=2), model="context")
    t0 = time.time()
    img, info = dev.RUNNERS["mimizan"](dev.BY_KEY["mz_ls"], dng, work, progs)
    dng.unlink()
    info["seconds"] = round(time.time() - t0, 2)
    fx, fy = info.get("offset", [0, 0])
    img = img[oy - fy : oy - fy + h, ox - fx : ox - fx + w]
    np.save(cache, np.round(np.clip(img, 0.0, 1.0) * 65535.0).astype(np.uint16))
    side.write_text(json.dumps(info))
    return np.load(cache).astype(np.float32) / 65535.0, info


def run_scene(out: Path, chart: str, palette: str, lens: str, methods, progs, force_measure=False, photos=None):
    name = scene_name(chart, palette, lens)
    work = out / "synth" / name
    work.mkdir(parents=True, exist_ok=True)
    truth_path, meta_path, dng_path = work / "truth.npy", work / "scene.json", work / "scene.dng"
    if not (truth_path.exists() and meta_path.exists() and dng_path.exists()):
        truth, tiles = scenes.render(chart, palette, LENSES[lens][0])
        np.save(truth_path, truth)
        meta = {
            "chart": chart,
            "palette": palette,
            "lens": lens,
            "sigma": LENSES[lens][0],
            "tiles": [t.meta() for t in tiles],
        }
        meta_path.write_text(json.dumps(meta))
        dngio.write_cfa_dng(dng_path, scenes.mosaic(truth, seed=1), model=name)
    truth = np.load(truth_path)
    meta = json.loads(meta_path.read_text())
    colours = scenes.palette(palette)
    mosaic = None
    records_path = work / "records.json"
    done = json.loads(records_path.read_text()) if records_path.exists() and not force_measure else {}
    todo = [(m.key, m) for m in methods]
    if photos is not None and progs["mimizan_ls"] and any(m.key == "mz_ls" for m in methods):
        todo.append((CONTEXT_KEY, None))
    for key, m in todo:
        if key in done:
            continue
        if m is not None and not dev.available(m, progs):
            continue
        if m is not None and m.program == "reference" and mosaic is None:
            mosaic = scenes.mosaic(truth, seed=1).astype(np.float64) / 65535.0
        try:
            if m is None:
                img, info = context_ls(work, truth, photos, progs)
                info = {k: v for k, v in info.items() if k != "offset"}
            else:
                img, info = dev.develop(m, dng_path, work, progs, mosaic=mosaic)
        except Exception as e:  # a developer that fails is reported, not fatal
            print(f"  {name}: {key} failed: {str(e)[:300]}", file=sys.stderr)
            done[key] = {"error": str(e)[:300]}
            continue
        ox, oy = info.get("offset", [0, 0])
        if (ox, oy) != (0, 0) or img.shape != truth.shape:
            full = np.zeros_like(truth)
            hh, ww = min(img.shape[0], truth.shape[0] - oy), min(img.shape[1], truth.shape[1] - ox)
            full[oy : oy + hh, ox : ox + ww] = img[:hh, :ww]
            img = full
        cm, residual = metrics.fit_colour(img, truth, meta["tiles"])
        maps = metrics.Maps(metrics.apply_colour(img, cm), truth)
        recs = []
        for i, t in enumerate(meta["tiles"]):
            for r in metrics.tile_records(maps, t, colours):
                r["tile"] = i
                recs.append(r)
        done[key] = {
            "seconds": info.get("seconds"),
            "colour_matrix_off_identity": float(np.abs(cm - np.eye(3)).max()),
            "colour_residual": residual,
            "records": recs,
        }
        records_path.write_text(json.dumps(done))
    records_path.write_text(json.dumps(done))
    return done


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--charts", default=",".join(scenes.CHARTS))
    ap.add_argument("--palettes", default="cam,sat")
    ap.add_argument("--lenses", default=",".join(LENSES))
    ap.add_argument("--methods", default="", help="comma-separated keys; default every method")
    ap.add_argument("--remeasure", action="store_true", help="measure again from the cached developments")
    ap.add_argument("--no-context", action="store_true", help="skip the least-squares demosaic inside a photograph")
    a = ap.parse_args()
    progs = dev.programs()
    for k, v in progs.items():
        print(f"{k}: {v or 'not found, skipped'}")
    methods = [dev.BY_KEY[k] for k in a.methods.split(",")] if a.methods else dev.METHODS
    a.out.mkdir(parents=True, exist_ok=True)
    (a.out / "versions.json").write_text(json.dumps(dev.versions(progs), indent=1))
    photos = None if a.no_context else photographs(a.out / "kodak")
    for palette in a.palettes.split(","):
        for lens in a.lenses.split(","):
            for chart in a.charts.split(","):
                t0 = time.time()
                done = run_scene(a.out, chart, palette, lens, methods, progs, a.remeasure, photos)
                ok = sum(1 for v in done.values() if "records" in v)
                print(f"{scene_name(chart, palette, lens)}: {ok} methods, {time.time() - t0:.0f} s", flush=True)


if __name__ == "__main__":
    main()
