#!/usr/bin/env python3
"""How much the least-squares demosaic depends on the rest of the frame.

    python bench/bayer-rings/ls_prior.py --out out/bayer-rings \\
        --scenes rings-cam-lens,dpr-cam-lens

That demosaic searches one prior for the whole frame and weighs a bank of
priors around it per pixel. Here the same chart is developed alone and as
a part of frames of photographs 2, 3 and 5 times its size; the chart's
pixels are measured each time, and the prior the search found is written
beside the figures. `run_synth.py` must have rendered the scenes.

Writes OUT/report/ls-prior.md, OUT/report/ls-prior.json and, to --media,
one strip a frame for the ring ladder.
"""

from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

import numpy as np

import developers as dev
import dngio
import metrics
import montage
import report
import scenes
from run_synth import context_canvas, photographs

FACTORS = [1, 2, 3, 5]
COMPARE = ["mz_dubois", "rt_amaze", "rt_rcd", "lr_dht"]


def develop(work: Path, truth, photos, progs, factor: int):
    """The chart's pixels out of a frame `factor` times its size, and the
    runner's info with the prior."""
    key = f"lsp_f{factor}"
    cache, side = work / f"{key}.npy", work / f"{key}.json"
    if cache.exists() and side.exists():
        return np.load(cache).astype(np.float32) / 65535.0, json.loads(side.read_text())
    h, w, _ = truth.shape
    if factor == 1:
        dng, ox, oy, made = work / "scene.dng", 0, 0, False
    else:
        canvas, ox, oy = context_canvas(truth, photos, factor)
        dng, made = work / f"{key}.dng", True
        dngio.write_cfa_dng(dng, scenes.mosaic(canvas, seed=2), model="context")
    t0 = time.time()
    img, info = dev.RUNNERS["mimizan"](dev.BY_KEY["mz_ls"], dng, work, progs)
    if made:
        dng.unlink()
    info["seconds"] = round(time.time() - t0, 2)
    fx, fy = info.get("offset", [0, 0])
    img = img[oy - fy : oy - fy + h, ox - fx : ox - fx + w]
    np.save(cache, np.round(np.clip(img, 0.0, 1.0) * 65535.0).astype(np.uint16))
    side.write_text(json.dumps(info))
    return np.load(cache).astype(np.float32) / 65535.0, info


def measure(img, truth, meta, colours):
    cm, _ = metrics.fit_colour(img, truth, meta["tiles"])
    maps = metrics.Maps(metrics.apply_colour(img, cm), truth)
    recs = []
    for i, t in enumerate(meta["tiles"]):
        for r in metrics.tile_records(maps, t, colours):
            r["tile"] = i
            recs.append(r)
    return report.patterns(recs), montage.to_srgb8(np.clip(metrics.apply_colour(img, cm), 0.0, 1.0))


def figures(recs):
    col = report.colour(recs)
    whole = report.pool(report.whole(col))
    out = {"psnr": whole["psnr"], "de": whole["de"], "fc": whole["fc"], "zipper": whole["zipper"]}
    for name, lo, hi in report.BANDS[:2]:
        p = report.pool(report.banded(col, lo, hi))
        out[name] = p["psnr"] if p else None
    f = [report.resolved(recs, p) for p in report.COLOUR_PAIRS]
    out["fres"] = float(np.mean([v if v is not None else 0.0 for v in f])) if any(v is not None for v in f) else None
    out["fres_unresolved"] = sum(v is None for v in f)
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--media", type=Path, default=None)
    ap.add_argument("--scenes", default="rings-cam-lens,dpr-cam-lens")
    ap.add_argument("--factors", default=",".join(map(str, FACTORS)))
    ap.add_argument("--pair", default="blue/red", help="the colour pair drawn")
    a = ap.parse_args()
    progs = dev.programs()
    if not progs["mimizan_ls"]:
        raise SystemExit("the example bayer_ls_develop is not built")
    photos = photographs(a.out / "kodak")
    if photos is None:
        raise SystemExit("no photographs to lay the chart in")
    media = a.media or a.out / "media"
    media.mkdir(parents=True, exist_ok=True)
    factors = [int(f) for f in a.factors.split(",")]
    head = [
        "Szene", "Rahmen", "α Luminanz", "α Farbe", "Farbe C1", "Farbe C2", "Objektiv σ px", "CPSNR dB",
        "CPSNR bis 0,20 c/px", "CPSNR 0,25–0,37 c/px", "ΔE00", "Falschfarbe", "aufgelöst c/px", "Paare nicht aufgelöst",
    ]  # fmt: skip
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    result = {}
    num = report.num
    for name in a.scenes.split(","):
        work = a.out / "synth" / name
        meta = json.loads((work / "scene.json").read_text())
        truth = np.load(work / "truth.npy")
        colours = scenes.palette(meta["palette"])
        strips = [("Original", "", "truth", montage.to_srgb8(truth))]
        result[name] = {}
        for factor in factors:
            img, info = develop(work, truth, photos, progs, factor)
            recs, shown = measure(img, truth, meta, colours)
            fig, prior = figures(recs), info.get("prior", {})
            frame = "Chart allein" if factor == 1 else f"Chart in {factor}×{factor} Fotofläche"
            lines.append(
                "| "
                + " | ".join(
                    [name, frame, num(prior.get("alpha"), 2), num(prior.get("alpha_c"), 2)]
                    + [num(c, 4) for c in prior.get("chroma", [None, None])]
                    + [num(prior.get("lens"), 2), num(fig["psnr"], 2)]
                    + [num(fig[b[0]], 2) for b in report.BANDS[:2]]
                    + [num(fig["de"], 2), num(fig["fc"], 2), num(fig["fres"], 3), str(fig["fres_unresolved"])]
                )
                + " |"
            )
            result[name][str(factor)] = {"prior": prior, "seconds": info["seconds"], **fig}
            strips.append((f"LS-Demosaic, {frame}", f"CPSNR {num(fig['psnr'], 1)} dB", "mimizan", shown))
            print(f"{name} x{factor}: {fig['psnr']:.2f} dB, prior {prior}", flush=True)
        records = json.loads((work / "records.json").read_text())
        for key in COMPARE:
            if "records" not in records.get(key, {}):
                continue
            fig = figures(report.patterns(records[key]["records"]))
            lines.append(
                "| "
                + " | ".join(
                    [name, report.LABELS[key], "", "", "", "", "", num(fig["psnr"], 2)]
                    + [num(fig[b[0]], 2) for b in report.BANDS[:2]]
                    + [num(fig["de"], 2), num(fig["fc"], 2), num(fig["fres"], 3), str(fig["fres_unresolved"])]
                )
                + " |"
            )
            result[name][key] = fig
        if meta["chart"] == "rings":
            sc = report.Scene(a.out, meta["chart"], meta["palette"], meta["lens"])
            tiles = sc.tiles(a.pair)
            rows = []
            for label, sub, kind, img8 in strips:
                rows.append((label, sub, kind, np.concatenate([report.crop_of(img8, t, 48) for _, t in tiles], axis=1)))
            for key in COMPARE:
                if key in sc.keys():
                    strip = np.concatenate([report.crop_of(sc.image(key), t, 48) for _, t in tiles], axis=1)
                    rows.append((report.LABELS[key], "", report.kind_of(key), strip))
            cols = [num(t["params"]["period"], 1) + " px" for _, t in tiles]
            title = f"LS-Demosaic und der Rest des Bildes: Ringe, {report.PAIR_NAMES[a.pair]}"
            path = media / f"synth-ls-rahmen-{name}-{a.pair.replace('/', '-')}.png"
            montage.ladder(rows, 2, title=title, subtitle=sc.condition(), columns=cols, cell=48).save(path, optimize=True)
            print(f"wrote {path}", flush=True)
    rep = a.out / "report"
    rep.mkdir(parents=True, exist_ok=True)
    (rep / "ls-prior.md").write_text("\n".join(lines) + "\n")
    (rep / "ls-prior.json").write_text(json.dumps(result, indent=1))
    print(f"wrote {rep / 'ls-prior.md'}")


if __name__ == "__main__":
    main()
