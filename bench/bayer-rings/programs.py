#!/usr/bin/env python3
"""Mimizan against each program: its default method and its best one.

    python bench/bayer-rings/programs.py --out out/bayer-rings

Reads the measured scenes and writes OUT/report/programs.md: for every
chart and condition the CPSNR of the colour pairs of the least-squares
demosaic (the chart lying in a photograph) and of the Dubois plate, beside
the default and the best method of LibRaw, RawTherapee and darktable; then
how often Mimizan is ahead of each, and compact tables of one condition.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

import developers as dev
import report
import scenes
from report import BW, CHART_NAMES, CONTEXT_KEY, LABELS, LENS_NAMES, PALETTE_NAMES, colour, num, pool, whole

PROGRAMS = {
    "LibRaw": ("lr_", "lr_ahd"),
    "RawTherapee": ("rt_", "rt_amaze"),
    "darktable": ("dt_", "dt_rcd"),
}
SUBJECTS = [CONTEXT_KEY, "mz_ls", "mz_dubois"]
FIELDS = [("psnr", "CPSNR", True), ("de", "ΔE00", False), ("fc", "Falschfarbe", False), ("zipper", "Zipper", False)]
COMPACT = [
    CONTEXT_KEY, "mz_ls", "mz_dubois", "mz_adaptive", "mz_off", "lr_bilinear", "lr_vng", "lr_ahd", "lr_dcb",
    "lr_dht", "rt_amaze", "rt_rcd", "rt_lmmse", "rt_igv", "dt_rcd", "dt_amaze", "dt_vng4",
]  # fmt: skip


def pools(recs, pairs="colour"):
    out = {}
    for k, v in recs.items():
        rs = whole(v)
        rs = colour(rs) if pairs == "colour" else [r for r in rs if r["pair"] == BW]
        p = pool(rs)
        if p:
            out[k] = p
    return out


def signed(d, nd=2):
    return ("+" if d >= 0 else "−") + num(abs(d), nd)


def versus(data, pairs="colour"):
    """One row a scene: ours, and each program's default and best method."""
    head = ["Farben", "Optik", "Chart"] + [LABELS[s].replace("Mimizan ", "") for s in SUBJECTS]
    for name, (_, default) in PROGRAMS.items():
        head += [f"{name} Standard ({LABELS[default].split(' ', 1)[1]})", f"{name} bestes"]
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    rows = []
    for palette in PALETTE_NAMES:
        for lens in LENS_NAMES:
            for c in scenes.CHARTS:
                recs = data.get((c, palette, lens))
                if not recs:
                    continue
                p = pools(recs, pairs)
                if not p:
                    continue
                cells = [PALETTE_NAMES[palette], LENS_NAMES[lens], CHART_NAMES[c]]
                top = max(v["psnr"] for k, v in p.items() if k in SUBJECTS or not k.startswith(("mz_", "ref_")))
                row = {"chart": c, "palette": palette, "lens": lens}
                for s in SUBJECTS:
                    v = p[s]["psnr"] if s in p else None
                    cells.append(f"**{num(v)}**" if v is not None and abs(v - top) < 0.005 else num(v))
                    row[s] = p.get(s)
                for name, (prefix, default) in PROGRAMS.items():
                    best = max((k for k in p if k.startswith(prefix)), key=lambda k: p[k]["psnr"])
                    d = p[default]["psnr"]
                    cells.append(f"**{num(d)}**" if abs(d - top) < 0.005 else num(d))
                    b = f"{num(p[best]['psnr'])} ({LABELS[best].split(' ', 1)[1]})"
                    cells.append(f"**{b}**" if abs(p[best]["psnr"] - top) < 0.005 else b)
                    row[name] = {"default": p[default], "best": best, "best_pool": p[best], "all": {k: p[k] for k in p if k.startswith(prefix)}}
                lines.append("| " + " | ".join(cells) + " |")
                rows.append(row)
    return "\n".join(lines), rows


def tally(rows, subject):
    """In how many scenes `subject` is ahead of each program's default
    method and of every one of its methods, by each figure."""
    head = ["Gegen"] + [f"{label} besser" for _, label, _ in FIELDS] + ["CPSNR im Mittel dB"]
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out = {}
    use = [r for r in rows if r.get(subject)]
    for name in PROGRAMS:
        for what in ("default", "all"):
            cells, entry = [], {}
            for f, _, higher in FIELDS:
                wins = 0
                for r in use:
                    mine = r[subject][f]
                    theirs = [r[name]["default"][f]] if what == "default" else [v[f] for v in r[name]["all"].values()]
                    wins += all((mine > t) if higher else (mine < t) for t in theirs)
                cells.append(f"{wins} von {len(use)}")
                entry[f] = wins
            if what == "default":
                d = float(np.mean([r[subject]["psnr"] - r[name]["default"]["psnr"] for r in use]))
            else:
                d = float(np.mean([r[subject]["psnr"] - r[name]["best_pool"]["psnr"] for r in use]))
            entry["mean_db"], entry["scenes"] = d, len(use)
            label = f"{name}, Standard" if what == "default" else f"{name}, jedes Verfahren (Mittel: gegen das beste)"
            lines.append("| " + " | ".join([label] + cells + [signed(d)]) + " |")
            out[f"{name}-{what}"] = entry
    return "\n".join(lines), out


def compact(data, palette, lens):
    """The tables of one condition, the methods of `COMPACT` only."""
    parts = {}
    for c in scenes.CHARTS:
        recs = data.get((c, palette, lens))
        if not recs:
            continue
        order = [k for k in report.by_mean(recs) if k in COMPACT]
        sec = [report.chart_table(recs, order)]
        if c == "dpr":
            sec.append("CPSNR in dB je Feld der Ringscheibe (Periode der Ringe):\n\n" + report.dpr_table(recs, order))
            sec.append("CPSNR in dB je Farbpaar:\n\n" + report.pair_table(recs, order, report.PAIRS[:4]))
        if c in ("rings", "zone", "siemens"):
            sec.append("Farbpaare zusammen, nach Ortsfrequenz:\n\n" + report.band_table(recs, order))
            sec.append(
                "Aufgelöste Frequenz in c/px je Farbpaar (– : schon die gröbste gemessene Frequenz ist nicht aufgelöst):\n\n"
                + report.fres_table(recs, order)
            )
        if c == "edges":
            sec.append("Kanten der Farbpaare (Unruhe: Streuung in ΔE entlang der Kante):\n\n" + report.edge_table(recs, order))
        if c == "grids":
            sec.append("CPSNR in dB je Muster, Farbpaare zusammen:\n\n" + report.grid_table(recs, order, "psnr"))
            sec.append("Falschfarbe je Muster, Farbpaare zusammen:\n\n" + report.grid_table(recs, order, "fc"))
        parts[c] = "\n\n".join(sec)
    return parts


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--condition", default="cam-lens", help="PALETTE-LENS of the compact tables")
    a = ap.parse_args()
    data = report.load(a.out)
    if not data:
        raise SystemExit(f"nothing measured under {a.out / 'synth'}; run run_synth.py first")
    parts, summary = [], {}
    text, rows = versus(data)
    parts.append("## Farbpaare: Mimizan neben dem Standard und dem besten Verfahren jedes Programms\n\nCPSNR in dB; fett ist der höchste Wert der Zeile.\n\n" + text)
    for s in SUBJECTS:
        text, summary[s] = tally(rows, s)
        parts.append(f"## {LABELS[s]}: in wie vielen Szenen voraus\n\n{text}")
    text, bw = versus(data, "bw")
    parts.append("## Schwarz auf Weiß: dieselbe Gegenüberstellung\n\nCPSNR in dB; fett ist der höchste Wert der Zeile.\n\n" + text)
    for s in SUBJECTS:
        text, summary[f"{s}-bw"] = tally(bw, s)
        parts.append(f"## {LABELS[s]}, Schwarz auf Weiß: in wie vielen Szenen voraus\n\n{text}")
    palette, lens = a.condition.split("-")
    for c, text in compact(data, palette, lens).items():
        parts.append(f"## {CHART_NAMES[c]}: {PALETTE_NAMES[palette]}, {LENS_NAMES[lens]}\n\n{text}")
    rep = a.out / "report"
    rep.mkdir(parents=True, exist_ok=True)
    (rep / "programs.md").write_text("\n\n".join(parts) + "\n")
    (rep / "programs.json").write_text(json.dumps(summary, indent=1))
    print(f"wrote {rep / 'programs.md'}")


if __name__ == "__main__":
    main()
