#!/usr/bin/env python3
"""The least-squares demosaic before and after a change, on the synthetic scenes.

    python bench/bayer-rings/before_after.py --keep      # before the change
    cargo build --release -p mimizan-cli --example bayer_ls_develop
    python bench/bayer-rings/run_synth.py --methods mz_ls
    python bench/bayer-rings/before_after.py --media DIR # after it

`--keep` puts what `run_synth.py` has of that demosaic aside, under the
keys `mz_ls_before` and `mz_ls_ctx_before`, so the next run develops the
scenes again. `--drop` throws the present results away and leaves the ones
put aside, for a change that was changed again. Without either the two are
compared: OUT/report/before-after.md and, to --media, strips of the
original, both and three other developers, of the chart among photographs
and of the chart alone (`-allein` in the name).
"""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path

import numpy as np

import developers as dev
import report
import scenes
from report import BW, CHART_NAMES, LENS_NAMES, PAIR_NAMES, PALETTE_NAMES, colour, num, pool, whole
from run_synth import CONTEXT_KEY, LENSES, scene_name

KEYS = [CONTEXT_KEY, "mz_ls"]
WHERE = {CONTEXT_KEY: "Im Fotorahmen", "mz_ls": "Karte allein"}
BESIDE = ["dt_rcd", "rt_amaze", "lr_ahd"]
FREQUENCY = ("rings", "zone", "siemens")


def before(key):
    return f"{key}_before"


def keep(out: Path):
    moved = 0
    for path in sorted((out / "synth").glob("*/records.json")):
        done = json.loads(path.read_text())
        for key in KEYS:
            if key not in done:
                continue
            for ext in ("npy", "json"):
                src = path.parent / f"{key}.{ext}"
                if src.exists():
                    shutil.move(src, path.parent / f"{before(key)}.{ext}")
            done[before(key)] = done.pop(key)
            moved += 1
        path.write_text(json.dumps(done))
    print(f"{moved} results put aside")


def drop(out: Path):
    dropped = 0
    for path in sorted((out / "synth").glob("*/records.json")):
        done = json.loads(path.read_text())
        for key in KEYS:
            for ext in ("npy", "json"):
                (path.parent / f"{key}.{ext}").unlink(missing_ok=True)
            dropped += done.pop(key, None) is not None
        path.write_text(json.dumps(done))
    print(f"{dropped} results dropped")


def load(out: Path):
    data = {}
    for path in sorted((out / "synth").glob("*/records.json")):
        meta = json.loads((path.parent / "scene.json").read_text())
        recs = {k: report.patterns(v["records"]) for k, v in json.loads(path.read_text()).items() if "records" in v}
        data[(meta["chart"], meta["palette"], meta["lens"])] = recs
    return data


def cpsnr(recs):
    p = pool(colour(whole(recs)))
    return p["psnr"] if p else None


def bw(recs):
    p = pool([r for r in whole(recs) if r["pair"] == BW])
    return p["psnr"] if p else None


def band(recs, lo, hi, pairs=colour):
    p = pool(report.banded(pairs(recs), lo, hi))
    return p["psnr"] if p else None


def place(recs_by_method, key):
    """Place of `key` among the other programs' methods, 1 the best."""
    score = {k: cpsnr(v) for k, v in recs_by_method.items() if k in report.OTHERS or k == key}
    return report.rank(score, key), len(score)


def signed(d):
    return "–" if d is None else f"{d:+.2f}".replace(".", ",").replace("-", "−")


def md(head, rows):
    out = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out += ["| " + " | ".join(r) + " |" for r in rows]
    return "\n".join(out) + "\n"


def conditions(data):
    seen = []
    for palette in ("cam", "sat"):
        for lens in LENSES:
            if any((c, palette, lens) in data for c in scenes.CHARTS):
                seen.append((palette, lens))
    return seen


def overall(data, key):
    """One line a scene: colour pairs, place, black on white."""
    rows, deltas, lost = [], [], []
    for palette, lens in conditions(data):
        for chart in scenes.CHARTS:
            recs = data.get((chart, palette, lens), {})
            if key not in recs or before(key) not in recs:
                continue
            a, b = cpsnr(recs[before(key)]), cpsnr(recs[key])
            pa, n = place(recs, before(key))
            pb, _ = place(recs, key)
            wa, wb = bw(recs[before(key)]), bw(recs[key])
            deltas.append(b - a)
            if wa is not None:
                lost.append(wb - wa)
            rows.append(
                [
                    f"{CHART_NAMES[chart]}, {PALETTE_NAMES[palette]}, {LENS_NAMES[lens]}",
                    num(a), num(b), signed(b - a), f"{pa} → {pb} von {n}",
                    num(wa), num(wb), signed(None if wa is None else wb - wa),
                ]
            )  # fmt: skip
    head = ["Szene", "Farbpaare vorher", "nachher", "Δ dB", "Platz", "Schwarz/Weiß vorher", "nachher", "Δ dB"]
    return md(head, rows), deltas, lost


def bands(data, key, pairs=colour):
    rows = []
    for palette, lens in conditions(data):
        for chart in FREQUENCY:
            recs = data.get((chart, palette, lens), {})
            if key not in recs or before(key) not in recs:
                continue
            cells = [f"{CHART_NAMES[chart]}, {PALETTE_NAMES[palette]}, {LENS_NAMES[lens]}"]
            for _, lo, hi in report.BANDS[:3]:
                a, b = band(recs[before(key)], lo, hi, pairs), band(recs[key], lo, hi, pairs)
                cells += [num(a), num(b)]
            best = {k: band(v, 0.0, 0.21, pairs) for k, v in recs.items() if k in report.OTHERS}
            best = {k: v for k, v in best.items() if v is not None}
            top = max(best, key=best.get) if best else None
            cells.append(f"{num(best[top])} ({report.LABELS[top]})" if top else "–")
            rows.append(cells)
    head = ["Szene"]
    for name, _, _ in report.BANDS[:3]:
        head += [f"{name} vorher", "nachher"]
    head.append("bestes anderes Programm bis 0,20 c/px")
    return md(head, rows)


def ring_periods(data, key, pair):
    """The ring chart tile by tile, one pair."""
    rows = []
    for palette, lens in conditions(data):
        recs = data.get(("rings", palette, lens), {})
        if key not in recs or before(key) not in recs:
            continue
        periods = sorted({r["period"] for r in recs[key] if r["pair"] == pair}, reverse=True)
        cells = [f"{PALETTE_NAMES[palette]}, {LENS_NAMES[lens]}"]
        for per in periods:
            a = pool([r for r in recs[before(key)] if r["pair"] == pair and r["period"] == per])
            b = pool([r for r in recs[key] if r["pair"] == pair and r["period"] == per])
            cells.append(f"{num(a['psnr'], 1)} → {num(b['psnr'], 1)}")
        rows.append((periods, cells))
    if not rows:
        return ""
    head = ["Bedingung"] + [num(p, 1) + " px" for p in rows[0][0]]
    return md(head, [c for _, c in rows])


def write_report(data, out: Path):
    lines = [
        "# Der LS-Demosaic vorher und nachher\n",
        "CPSNR in dB auf sRGB-kodierten Werten, wie in `report.py`. »Platz« ist der Rang unter den Verfahren",
        "der anderen Programme (LibRaw, RawTherapee, darktable, Referenzen), 1 ist das beste.\n",
    ]
    for key in KEYS:
        text, deltas, lost = overall(data, key)
        if not deltas:
            continue
        better, worse = sum(d > 0.05 for d in deltas), sum(d < -0.05 for d in deltas)
        lines += [
            f"## {WHERE[key]}\n",
            f"Farbpaare: in {better} von {len(deltas)} Szenen besser, in {worse} schlechter, im Mittel"
            f" {signed(float(np.mean(deltas)))} dB (von {signed(min(deltas))} bis {signed(max(deltas))})."
            f" Schwarz auf Weiß im Mittel {signed(float(np.mean(lost)))} dB (von {signed(min(lost))} bis {signed(max(lost))}).\n",
            text,
            "### Nach Frequenz, Farbpaare\n",
            bands(data, key),
            "### Nach Frequenz, Schwarz auf Weiß\n",
            bands(data, key, lambda rs: [r for r in rs if r["pair"] == BW]),
            "### Ringe, Schwarz auf Weiß, nach Periode (vorher → nachher)\n",
            ring_periods(data, key, BW),
            "### Ringe, Blau auf Rot, nach Periode (vorher → nachher)\n",
            ring_periods(data, key, "blue/red"),
        ]
    path = out / "report" / "before-after.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines))
    print(f"wrote {path}")


def write_images(out: Path, media: Path, wanted, pairs, key=CONTEXT_KEY, prefix="vn-"):
    media.mkdir(parents=True, exist_ok=True)
    alone = "" if key == CONTEXT_KEY else "-allein"
    report.LABELS[before(key)] = "Mimizan LS-Demosaic vorher"
    report.LABELS[key] = "Mimizan LS-Demosaic nachher"
    keys = [before(key), key] + BESIDE

    def save(img, name):
        img.save(media / name, optimize=True)
        print(f"wrote {media / name}", flush=True)

    for palette, lens in wanted:
        tag = f"{palette}-{lens}"
        for chart in scenes.CHARTS:
            if alone and chart not in FREQUENCY and chart != "dpr":
                continue
            if not (out / "synth" / scene_name(chart, palette, lens) / f"{key}.npy").exists():
                continue
            sc = report.Scene(out, chart, palette, lens)
            have = [k for k in keys if (sc.work / f"{k}.npy").exists() and "records" in sc.records.get(k, {})]
            for pair in pairs:
                slug, name = pair.replace("/", "-"), PAIR_NAMES[pair]
                if chart == "dpr":
                    for i, t in sc.tiles(pair, period=8.8):
                        title = f"Ringscheibe wie bei DPReview, {name}: Ringe 8,8 px, feines Feld 4,4 px, Mitte 2,2 px"
                        save(report.grid_image(sc, i, t, have, 208, 3, 6, title, 30, 30, "all"), f"{prefix}dpr-{tag}-{slug}{alone}.png")
                elif chart == "rings":
                    tiles = sc.tiles(pair)
                    if tiles:
                        cols = [num(t["params"]["period"], 1) + " px" for _, t in tiles]
                        title = f"Ringe, {name}: Periode von 8 px bis 1,6 px (Nyquist: 2 px)"
                        save(report.ladder_image(sc, tiles, have, 48, 3, title, cols), f"{prefix}ringe-{tag}-{slug}{alone}.png")
                elif chart == "zone":
                    for i, t in sc.tiles(pair):
                        title = f"Zonenplatte, {name}: Frequenz steigt mit dem Abstand von links oben, 0,5 c/px bei 160 px"
                        save(report.grid_image(sc, i, t, have, 192, 3, 6, title, 96, 96, "all"), f"{prefix}zone-{tag}-{slug}{alone}.png")
                elif chart == "siemens":
                    for i, t in sc.tiles(pair):
                        title = f"Siemensstern, {name}: 60 Strahlenpaare, 0,5 c/px bei 19 px vom Mittelpunkt"
                        save(report.grid_image(sc, i, t, have, 128, 4, 6, title, 0, 0, "all"), f"{prefix}siemens-{tag}-{slug}{alone}.png")
                elif chart == "edges":
                    tiles = sc.tiles(pair)
                    if tiles:
                        cols = [num(t["params"]["angle_deg"], 0) + "°" for _, t in tiles]
                        save(report.ladder_image(sc, tiles, have, 40, 4, f"Schräge Kanten, {name}", cols), f"{prefix}kanten-{tag}-{slug}{alone}.png")
                elif chart == "grids":
                    tiles = sc.tiles(pair)
                    if tiles:
                        cols = ["| 2", "— 2", "| 3", "| 4", "8° 2,5", "45° 2,83", "Schach 1", "Schach 2", "Haar 5", "Punkte 3"]
                        title = f"Feine Gitter, {name} (Linien mit Periode in px, Schachbrett, Haarlinien, Punkte)"
                        save(report.ladder_image(sc, tiles, have, 32, 5, title, cols), f"{prefix}gitter-{tag}-{slug}{alone}.png")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--keep", action="store_true", help="put the present results aside as the ones before")
    ap.add_argument("--drop", action="store_true", help="throw the present results away, not the ones put aside")
    ap.add_argument("--media", type=Path, default=None, help="where the strips go; none without it")
    ap.add_argument("--prefix", default="vn-", help="what the strips' names begin with")
    ap.add_argument("--conditions", default="cam-lens,cam-sharp,cam-soft,sat-lens")
    ap.add_argument("--pairs", default="blue/red,magenta/orange,red/green,black/white")
    a = ap.parse_args()
    if a.keep or a.drop:
        (keep if a.keep else drop)(a.out)
        return
    write_report(load(a.out), a.out)
    if a.media:
        wanted = [tuple(c.split("-")) for c in a.conditions.split(",")]
        for key in KEYS:
            write_images(a.out, a.media, wanted, a.pairs.split(","), key, a.prefix)


if __name__ == "__main__":
    main()
