#!/usr/bin/env python3
"""Tables and comparison images from the measured scenes.

    python bench/bayer-rings/report.py --out out/bayer-rings

Reads OUT/synth/*/records.json and writes OUT/report/tables.md (German,
one section a chart), OUT/report/summary.json and, to --media (default
OUT/media), the crops of every method side by side.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

import developers as dev
import metrics
import montage
import scenes
from run_synth import CONTEXT_KEY, LENSES, scene_name

LABELS = {m.key: m.label for m in dev.METHODS}
LABELS[CONTEXT_KEY] = "Mimizan LS-Demosaic im Foto"
# The demosaicer first, then the plates of the frequency separation, then
# the other programs as `developers.py` lists them.
ORDER = [CONTEXT_KEY, "mz_ls"] + [m.key for m in dev.METHODS if m.key != "mz_ls"]
MIMIZAN = [k for k in ORDER if k.startswith("mz_")]
OTHERS = [k for k in ORDER if not k.startswith("mz_")]
SELECTION = [
    CONTEXT_KEY, "mz_ls", "mz_dubois", "mz_adaptive", "mz_off", "lr_ahd",
    "lr_dht", "rt_amaze", "rt_rcd", "rt_lmmse", "dt_rcd",
]  # fmt: skip
CHART_NAMES = {
    "dpr": "DPReview-Ringe",
    "rings": "Ringe",
    "zone": "Zonenplatte",
    "siemens": "Siemensstern",
    "edges": "Kanten",
    "grids": "Gitter",
}
PAIR_NAMES = {
    "magenta/orange": "Magenta auf Orange",
    "blue/red": "Blau auf Rot",
    "cyan/blue": "Cyan auf Blau",
    "yellow/cyan": "Gelb auf Cyan",
    "red/green": "Rot auf Grün",
    "green/magenta": "Grün auf Magenta",
    "blue/yellow": "Blau auf Gelb",
    "black/white": "Schwarz auf Weiß",
}
PAIRS = list(PAIR_NAMES)
COLOUR_PAIRS = [p for p in PAIRS if p != "black/white"]
BW = "black/white"
PALETTE_NAMES = {"cam": "Kamerafarben", "sat": "gesättigt"}
LENS_NAMES = {"sharp": "pixelscharf", "lens": "scharfes Objektiv", "soft": "mit Tiefpass"}
BANDS = [
    ("bis 0,20 c/px", 0.0, 0.21),
    ("0,25–0,37 c/px", 0.21, 0.40),
    ("0,42–0,50 c/px", 0.40, 0.501),
    ("über Nyquist", 0.501, 9.0),
]
POOLED = ("mse", "mse_luma", "de", "dc", "dl", "fc", "fc_share", "zipper")
HIGHER = {"psnr", "psnr_luma", "fres"}


# ---------------------------------------------------------------- figures


def pool(recs):
    recs = [r for r in recs if r]
    n = sum(r["n"] for r in recs)
    if not n:
        return None
    out = {k: sum(r[k] * r["n"] for r in recs) / n for k in POOLED}
    out["psnr"] = metrics.psnr(out["mse"])
    out["psnr_luma"] = metrics.psnr(out["mse_luma"])
    out["n"] = n
    return out


def whole(recs):
    """The records that tile a chart without overlap."""
    return [r for r in recs if r["kind"] not in ("dpr", "zone", "siemens") or r.get("part") == "all"]


def patterns(recs):
    return [r for r in recs if r["kind"] != "flat"]


def colour(recs):
    return [r for r in recs if r["pair"] != BW]


def resolved(recs, pair):
    """Resolved frequency of one pair: from the rings their tiles, from
    zone plate and Siemens star their frequency bins."""
    rs = [r for r in recs if r["pair"] == pair and "freq" in r and "part" not in r and r.get("nrmse") is not None]
    if len(rs) < 3:
        return None
    f, ok = metrics.resolved_frequency([r["freq"] for r in rs], [r["nrmse"] for r in rs])
    return f if ok else None


def load(out: Path):
    data = {}
    for path in sorted((out / "synth").glob("*/records.json")):
        meta = json.loads((path.parent / "scene.json").read_text())
        recs = {
            k: patterns(v["records"]) for k, v in json.loads(path.read_text()).items() if "records" in v and k in LABELS
        }
        data[(meta["chart"], meta["palette"], meta["lens"])] = recs
    return data


# ----------------------------------------------------------------- tables


def num(v, nd=2):
    if v is None or (isinstance(v, float) and not np.isfinite(v)):
        return "–"
    return f"{v:.{nd}f}".replace(".", ",")


def table(head, rows, best=None):
    """Markdown. `rows` are (key, [values]); `best` is a list, one entry a
    column: +1 the highest wins, -1 the lowest, 0 none. The best of a column
    is bold, a Mimizan row has its name bold."""
    best = best or [0] * (len(head) - 1)
    winners = []
    for c, sign in enumerate(best):
        vals = [r[1][c][0] for r in rows if r[1][c][0] is not None]
        winners.append((max(vals) if sign > 0 else min(vals)) if sign and vals else None)
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    for key, cells in rows:
        name = LABELS.get(key, key)
        out = [f"**{name}**" if key.startswith("mz_") else name]
        for c, (v, nd) in enumerate(cells):
            s = num(v, nd)
            if v is not None and winners[c] is not None and abs(v - winners[c]) < 0.5 * 10**-nd:
                s = f"**{s}**"
            out.append(s)
        lines.append("| " + " | ".join(out) + " |")
    return "\n".join(lines)


def rank(values: dict, key, higher=True):
    """Place of `key` among `values` (1 is best), ties share the better place."""
    v = values[key]
    return 1 + sum(1 for k, o in values.items() if k != key and ((o > v + 1e-9) if higher else (o < v - 1e-9)))


def chart_table(recs_by_method, order):
    head = ["Verfahren", "CPSNR dB", "Luma-PSNR dB", "ΔE00", "Chroma ΔC", "Falschfarbe", "Falschfarbe > 5, %", "Zipper %"]
    has_bw = any(r["pair"] == BW for rs in recs_by_method.values() for r in rs)
    if has_bw:
        head.append("Schwarz/Weiß CPSNR dB")
    rows = []
    for k in order:
        if k not in recs_by_method:
            continue
        p = pool(colour(whole(recs_by_method[k])))
        cells = [
            (p["psnr"], 2),
            (p["psnr_luma"], 2),
            (p["de"], 2),
            (p["dc"], 2),
            (p["fc"], 2),
            (100 * p["fc_share"], 1),
            (100 * p["zipper"], 1),
        ]
        if has_bw:
            b = pool([r for r in whole(recs_by_method[k]) if r["pair"] == BW])
            cells.append((b["psnr"], 2))
        rows.append((k, cells))
    return table(head, rows, [1, 1, -1, -1, -1, -1, -1] + ([1] if has_bw else []))


def fres_table(recs_by_method, order):
    head = ["Verfahren"] + [PAIR_NAMES[p] for p in PAIRS] + ["Mittel Farbpaare"]
    rows = []
    for k in order:
        if k not in recs_by_method:
            continue
        f = [resolved(recs_by_method[k], p) for p in PAIRS]
        col = [v for v, p in zip(f, PAIRS) if p != BW and v is not None]
        rows.append((k, [(v, 3) for v in f] + [(float(np.mean(col)) if len(col) == len(COLOUR_PAIRS) else None, 3)]))
    return table(head, rows, [1] * (len(PAIRS) + 1))


def banded(recs, lo, hi):
    """The records of one frequency band: ring tiles, or the frequency bins
    of a zone plate or a Siemens star."""
    return [r for r in recs if "freq" in r and "part" not in r and lo <= r["freq"] < hi]


def band_table(recs_by_method, order):
    head = ["Verfahren"] + [f"CPSNR {b[0]}" for b in BANDS] + [f"Falschfarbe {b[0]}" for b in BANDS]
    rows = []
    for k in order:
        if k not in recs_by_method:
            continue
        ps = [pool(banded(colour(recs_by_method[k]), lo, hi)) for _, lo, hi in BANDS]
        rows.append((k, [(p["psnr"], 2) for p in ps] + [(p["fc"], 2) for p in ps]))
    return table(head, rows, [1] * 4 + [-1] * 4)


FREQUENCY_CHARTS = ("rings", "zone", "siemens")


def band_pools(data, palette, lens, key):
    """One pool a band over rings, zone plate and Siemens star, colour pairs."""
    out = []
    for _, lo, hi in BANDS:
        recs = []
        for c in FREQUENCY_CHARTS:
            scene = data.get((c, palette, lens))
            if scene is None or key not in scene:
                return None
            recs += banded(colour(scene[key]), lo, hi)
        out.append(pool(recs))
    return out


def band_standing(data, palette, lens, ours="mz_dubois"):
    """Ours against every method a frequency band: CPSNR in dB and false
    colour, positive where ours is better."""
    mine = band_pools(data, palette, lens, ours)
    if mine is None:
        return None, None
    head = ["Gegen"] + [f"CPSNR {b[0]}" for b in BANDS] + [f"Falschfarbe {b[0]}" for b in BANDS]
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]

    def signed(d):
        return ("+" if d >= 0 else "−") + num(abs(d), 2)

    places = []
    scores = {ours: mine}
    for k in ORDER:
        if k == ours:
            continue
        theirs = band_pools(data, palette, lens, k)
        if theirs is None:
            continue
        scores[k] = theirs
        name = f"**{LABELS[k]}**" if k.startswith("mz_") else LABELS[k]
        cells = [signed(a["psnr"] - b["psnr"]) for a, b in zip(mine, theirs)]
        cells += [signed(b["fc"] - a["fc"]) for a, b in zip(mine, theirs)]
        lines.append("| " + " | ".join([name] + cells) + " |")
    for i, (band, _, _) in enumerate(BANDS):
        field = {k: v[i]["psnr"] for k, v in scores.items() if k == ours or k in OTHERS}
        best = max((k for k in field if k != ours), key=lambda k: field[k])
        places.append(
            {"band": band, "psnr": field[ours], "place": rank(field, ours), "of": len(field), "best": best, "best_psnr": field[best]}
        )
    own = f"| **{LABELS[ours]} selbst** | " + " | ".join([num(p["psnr"], 2) for p in mine] + [num(p["fc"], 2) for p in mine]) + " |"
    place = "| **Platz unter den anderen Programmen** | " + " | ".join([f"{p['place']} von {p['of']}" for p in places] + [""] * 4) + " |"
    best = "| Bestes anderes Verfahren | " + " | ".join([f"{LABELS[p['best']]} {num(p['best_psnr'], 2)}" for p in places] + [""] * 4) + " |"
    return "\n".join(lines + [own, place, best]), places


def dpr_table(recs_by_method, order):
    parts = [("coarse", "grob"), ("fine", "fein"), ("finest", "Mitte")]
    periods = sorted({r["period"] for rs in recs_by_method.values() for r in rs}, reverse=True)
    cols = [(per, part, name) for per in periods for part, name in parts]
    head = ["Verfahren"] + [
        f"{name} {num(per / {'coarse': 1, 'fine': 2, 'finest': 4}[part], 1)} px" for per, part, name in cols
    ]
    rows = []
    for k in order:
        if k not in recs_by_method:
            continue
        cells = []
        for per, part, _ in cols:
            p = pool([r for r in recs_by_method[k] if r["period"] == per and r.get("part") == part])
            cells.append((p["psnr"], 2))
        rows.append((k, cells))
    return table(head, rows, [1] * len(cols))


def pair_table(recs_by_method, order, pairs):
    head = ["Verfahren"] + [PAIR_NAMES[p] for p in pairs]
    rows = []
    for k in order:
        if k not in recs_by_method:
            continue
        rows.append((k, [(pool([r for r in whole(recs_by_method[k]) if r["pair"] == p])["psnr"], 2) for p in pairs]))
    return table(head, rows, [1] * len(pairs))


def edge_table(recs_by_method, order):
    groups = [("4° und 86°", (4.0, 86.0)), ("14° und 76°", (14.0, 76.0)), ("31° und 47°", (31.0, 47.0))]
    head = (
        ["Verfahren", "CPSNR dB", "ΔE00", "Falschfarbe", "Zipper %"]
        + [f"Unruhe {g[0]}" for g in groups]
        + ["Breite 10–90 % px", "Schwarz/Weiß Breite px"]
    )
    rows = []
    for k in order:
        if k not in recs_by_method:
            continue
        rs = colour(recs_by_method[k])
        p = pool(rs)
        cells = [(p["psnr"], 2), (p["de"], 2), (p["fc"], 2), (100 * p["zipper"], 1)]
        for _, angles in groups:
            u = [r["unrest"] for r in rs if r["angle_deg"] in angles and r.get("unrest") is not None]
            cells.append((float(np.sqrt(np.mean(np.square(u)))) if u else None, 2))
        w = [r["width"] for r in rs if r.get("width") is not None]
        cells.append((float(np.mean(w)) if len(w) > len(rs) // 2 else None, 2))
        wb = [r["width"] for r in recs_by_method[k] if r["pair"] == BW and r.get("width") is not None]
        cells.append((float(np.mean(wb)) if wb else None, 2))
        rows.append((k, cells))
    return table(head, rows, [1, -1, -1, -1, -1, -1, -1, -1, -1])


def grid_key(r):
    return (r["kind"],) + tuple(sorted((k, r[k]) for k in ("period", "angle_deg", "cell", "pitch") if k in r))


def grid_table(recs_by_method, order, field):
    names = [(grid_key({"kind": k, **p}), label) for k, p, label in scenes.GRIDS]
    head = ["Verfahren"] + [label for _, label in names]
    rows = []
    for k in order:
        if k not in recs_by_method:
            continue
        cells = []
        for gk, _ in names:
            p = pool([r for r in colour(recs_by_method[k]) if grid_key(r) == gk])
            cells.append((p[field], 2))
        rows.append((k, cells))
    return table(head, rows, [1 if field in HIGHER else -1] * len(names))


def by_mean(recs_by_method):
    score = {k: pool(colour(whole(v)))["psnr"] for k, v in recs_by_method.items()}
    return sorted(score, key=lambda k: -score[k])


def overview(data, palette, lens):
    """CPSNR of the colour pairs, one column a chart, sorted by the mean."""
    charts = [c for c in scenes.CHARTS if (c, palette, lens) in data]
    keys = [k for k in ORDER if all(k in data[(c, palette, lens)] for c in charts)]
    score = {k: [pool(colour(whole(data[(c, palette, lens)][k])))["psnr"] for c in charts] for k in keys}
    keys.sort(key=lambda k: -float(np.mean(score[k])))
    head = ["Verfahren"] + [CHART_NAMES[c] for c in charts] + ["Mittel"]
    rows = [(k, [(v, 2) for v in score[k]] + [(float(np.mean(score[k])), 2)]) for k in keys]
    return table(head, rows, [1] * (len(charts) + 1)), score, charts


def standing(data, palette, lens, ours="mz_dubois"):
    """Ours against every other method: the difference in CPSNR a chart
    (positive: ours is better) and in how many of all scenes ours wins."""
    charts = [c for c in scenes.CHARTS if (c, palette, lens) in data]
    head = (
        ["Gegen"]
        + [f"{CHART_NAMES[c]} dB" for c in charts]
        + ["CPSNR besser in", "ΔE00 besser in", "Falschfarbe geringer in", "Zipper geringer in"]
    )
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    summary = {}
    for k in ORDER:
        if k == ours:
            continue
        cells, ok = [], True
        for c in charts:
            recs = data[(c, palette, lens)]
            if k not in recs or ours not in recs:
                ok = False
                break
            d = pool(colour(whole(recs[ours])))["psnr"] - pool(colour(whole(recs[k])))["psnr"]
            cells.append(("+" if d >= 0 else "−") + num(abs(d), 2))
        if not ok:
            continue
        wins = {f: 0 for f in ("psnr", "de", "fc", "zipper")}
        total = 0
        for scene, recs in data.items():
            if k not in recs or ours not in recs:
                continue
            a, b = pool(colour(whole(recs[ours]))), pool(colour(whole(recs[k])))
            total += 1
            wins["psnr"] += a["psnr"] > b["psnr"]
            for f in ("de", "fc", "zipper"):
                wins[f] += a[f] < b[f]
        name = f"**{LABELS[k]}**" if k.startswith("mz_") else LABELS[k]
        lines.append("| " + " | ".join([name] + cells + [f"{wins[f]} von {total}" for f in wins]) + " |")
        summary[k] = {"wins": wins, "scenes": total}
    return "\n".join(lines), summary


def conditions(data, ours="mz_dubois"):
    """Ours a chart and condition: its CPSNR, its place among the methods
    of the other programs by CPSNR, Delta E, false colour and zipper, and
    the best of those by CPSNR."""
    head = [
        "Farben", "Optik", "Chart", f"{LABELS[ours]} dB", "Platz CPSNR", "Bestes anderes Verfahren", "dessen dB",
        "Abstand dB", "Platz ΔE00", "Platz Falschfarbe", "Platz Zipper",
    ]  # fmt: skip
    lines = ["| " + " | ".join(head) + " |", "|" + "---|" * len(head)]
    out = []
    for palette in PALETTE_NAMES:
        for lens in LENS_NAMES:
            for c in scenes.CHARTS:
                recs = data.get((c, palette, lens))
                if not recs or ours not in recs:
                    continue
                pools = {k: pool(colour(whole(v))) for k, v in recs.items() if k == ours or k in OTHERS}
                if len(pools) < 2:
                    continue
                score = {k: v["psnr"] for k, v in pools.items()}
                best = max((k for k in score if k != ours), key=lambda k: score[k])
                place = rank(score, ours)
                places = {f: rank({k: v[f] for k, v in pools.items()}, ours, higher=False) for f in ("de", "fc", "zipper")}
                d = score[ours] - score[best]
                lines.append(
                    "| "
                    + " | ".join(
                        [
                            PALETTE_NAMES[palette],
                            LENS_NAMES[lens],
                            CHART_NAMES[c],
                            num(score[ours], 2),
                            f"{place} von {len(score)}",
                            LABELS[best],
                            num(score[best], 2),
                            ("+" if d >= 0 else "−") + num(abs(d), 2),
                        ]
                        + [str(places[f]) for f in ("de", "fc", "zipper")]
                    )
                    + " |"
                )
                out.append(
                    {
                        "chart": c, "palette": palette, "lens": lens, "psnr": score[ours], "place": place,
                        "of": len(score), "best": best, "best_psnr": score[best], "places": places,
                        "de": pools[ours]["de"], "fc": pools[ours]["fc"], "zipper": pools[ours]["zipper"],
                    }  # fmt: skip
                )
    return "\n".join(lines), out


# The demosaicer first: it is what is being worked on. The plate of the
# frequency separation stands beside it for comparison.
SUBJECTS = [CONTEXT_KEY, "mz_ls", "mz_dubois"]


def write_tables(data, out: Path):
    parts, summary = [], {"conditions": {}, "scenes": {}}
    for ours in SUBJECTS:
        text, summary["conditions"][ours] = conditions(data, ours)
        if summary["conditions"][ours]:
            parts.append(
                f"## {LABELS[ours]} in jeder Bedingung\n\nCPSNR der Farbpaare; die Plätze zählen unter den 24 Verfahren der anderen Programme und diesem einen (1 ist der beste).\n\n{text}"
            )
    for palette in PALETTE_NAMES:
        for lens in LENS_NAMES:
            if not any((c, palette, lens) in data for c in scenes.CHARTS):
                continue
            title = f"{PALETTE_NAMES[palette]}, {LENS_NAMES[lens]}"
            text, score, charts = overview(data, palette, lens)
            parts.append(f"## Übersicht: {title}\n\nCPSNR der Farbpaare in dB, je Chart; sortiert nach dem Mittel.\n\n{text}")
            entry = {"charts": charts, "cpsnr": score, "wins": {}, "bands": {}}
            summary["scenes"][f"{palette}-{lens}"] = entry
            for ours in SUBJECTS:
                name = LABELS[ours]
                text, entry["wins"][ours] = standing(data, palette, lens, ours)
                if entry["wins"][ours]:
                    parts.append(f"### {name} gegen jedes Verfahren: {title}\n\nDifferenz der CPSNR in dB (plus: {name} ist besser); die vier letzten Spalten zählen über alle Szenen aller Bedingungen.\n\n{text}")
                text, places = band_standing(data, palette, lens, ours)
                if text:
                    parts.append(f"### {name} nach Ortsfrequenz: {title}\n\nRinge, Zonenplatte und Siemensstern zusammen, nur Farbpaare. Differenz zu jedem Verfahren: CPSNR in dB und Falschfarbe, plus heißt {name} ist besser.\n\n{text}")
                    entry["bands"][ours] = places
            for c in charts:
                recs = data[(c, palette, lens)]
                order = by_mean(recs)
                sec = [f"### {CHART_NAMES[c]}: {title}\n\n{chart_table(recs, order)}"]
                if c == "dpr":
                    sec.append("CPSNR in dB je Feld der Ringscheibe (Periode der Ringe):\n\n" + dpr_table(recs, order))
                    sec.append("CPSNR in dB je Farbpaar:\n\n" + pair_table(recs, order, PAIRS[:4]))
                if c == "rings":
                    sec.append("Farbpaare zusammen, nach Ortsfrequenz:\n\n" + band_table(recs, order))
                if c in ("zone", "siemens"):
                    sec.append("Farbpaare zusammen, nach Ortsfrequenz:\n\n" + band_table(recs, order))
                if c in ("rings", "zone", "siemens"):
                    sec.append("Aufgelöste Frequenz in c/px je Farbpaar (– : schon die gröbste gemessene Frequenz ist nicht aufgelöst):\n\n" + fres_table(recs, order))
                if c in ("zone", "siemens"):
                    sec.append("CPSNR in dB je Farbpaar:\n\n" + pair_table(recs, order, PAIRS))
                if c == "edges":
                    sec.append("Kanten der Farbpaare (Unruhe: Streuung in ΔE entlang der Kante):\n\n" + edge_table(recs, order))
                if c == "grids":
                    sec.append("CPSNR in dB je Muster, Farbpaare zusammen:\n\n" + grid_table(recs, order, "psnr"))
                    sec.append("Falschfarbe je Muster, Farbpaare zusammen:\n\n" + grid_table(recs, order, "fc"))
                parts.append("\n\n".join(sec))
    report = out / "report"
    report.mkdir(parents=True, exist_ok=True)
    (report / "tables.md").write_text("\n\n".join(parts) + "\n")
    (report / "summary.json").write_text(json.dumps(summary, indent=1))


# ----------------------------------------------------------------- images


class Scene:
    def __init__(self, out: Path, chart, palette, lens):
        self.work = out / "synth" / scene_name(chart, palette, lens)
        self.meta = json.loads((self.work / "scene.json").read_text())
        self.truth = np.load(self.work / "truth.npy")
        self.records = json.loads((self.work / "records.json").read_text())
        self.chart, self.palette, self.lens = chart, palette, lens
        self._img = {}

    def keys(self):
        return [k for k in ORDER if "records" in self.records.get(k, {}) and (self.work / f"{k}.npy").exists()]

    def image(self, key):
        """A method's frame as it was measured, sRGB, 8 bits."""
        if key == "truth":
            return montage.to_srgb8(self.truth)
        if key not in self._img:
            info = json.loads((self.work / f"{key}.json").read_text())
            img = np.load(self.work / f"{key}.npy").astype(np.float32) * (info.get("scale", 1.0) / 65535.0)
            ox, oy = (0, 0) if key == CONTEXT_KEY else info.get("offset", [0, 0])
            if (ox, oy) != (0, 0) or img.shape != self.truth.shape:
                full = np.zeros_like(self.truth)
                hh, ww = min(img.shape[0], full.shape[0] - oy), min(img.shape[1], full.shape[1] - ox)
                full[oy : oy + hh, ox : ox + ww] = img[:hh, :ww]
                img = full
            cm, _ = metrics.fit_colour(img, self.truth, self.meta["tiles"])
            self._img[key] = montage.to_srgb8(np.clip(metrics.apply_colour(img, cm), 0.0, 1.0))
        return self._img[key]

    def tiles(self, pair=None, **params):
        out = []
        for i, t in enumerate(self.meta["tiles"]):
            if t["kind"] == "flat" or (pair and "/".join(t["pair"]) != pair):
                continue
            if all(t["params"].get(k) == v for k, v in params.items()):
                out.append((i, t))
        return out

    def stats(self, key, tile_indices, part=None):
        recs = [
            r
            for r in self.records[key]["records"]
            if r["tile"] in tile_indices and ((r.get("part") == part) if part else ("part" not in r))
        ]
        return pool(recs)

    def condition(self):
        return f"{PALETTE_NAMES[self.palette]}, {LENS_NAMES[self.lens]} ({LENSES[self.lens][1]})"


def kind_of(key):
    return "truth" if key == "truth" else ("mimizan" if key.startswith("mz_") else "other")


def crop_of(img, tile, size, dx=0, dy=0):
    x, y, w, h = tile["rect"]
    x0 = int(np.clip(x + w // 2 + dx - size // 2, x, x + w - size))
    y0 = int(np.clip(y + h // 2 + dy - size // 2, y, y + h - size))
    return img[y0 : y0 + size, x0 : x0 + size]


def sub(p):
    return f"CPSNR {num(p['psnr'], 1)} dB · ΔE {num(p['de'], 1)} · Falschfarbe {num(p['fc'], 1)}" if p else ""


def grid_image(scene: Scene, tile_index, tile, keys, size, zoom, cols, title, dx=0, dy=0, part=None):
    panels = [("Original", "die Szene vor dem Mosaik", "truth", crop_of(scene.image("truth"), tile, size, dx, dy))]
    for k in keys:
        panels.append((LABELS[k], sub(scene.stats(k, {tile_index}, part)), kind_of(k), crop_of(scene.image(k), tile, size, dx, dy)))
    return montage.grid(panels, cols, zoom, title=title, subtitle=scene.condition() + "; Zahlen über das ganze Feld")


def ladder_image(scene: Scene, tiles, keys, cell, zoom, title, columns):
    rows = []
    for k in ["truth"] + keys:
        strip = np.concatenate([crop_of(scene.image(k), t, cell) for _, t in tiles], axis=1)
        label = "Original" if k == "truth" else LABELS[k]
        s = "" if k == "truth" else f"CPSNR {num(scene.stats(k, {i for i, _ in tiles})['psnr'], 1)} dB"
        rows.append((label, s, kind_of(k), strip))
    return montage.ladder(rows, zoom, title=title, subtitle=scene.condition(), columns=columns, cell=cell)


def write_images(out: Path, media: Path, conditions_wanted, pairs_wanted, charts_wanted=tuple(scenes.CHARTS)):
    media.mkdir(parents=True, exist_ok=True)
    written = []

    def save(img, name):
        path = media / name
        img.save(path, optimize=True)
        written.append(str(path))
        print(f"wrote {path}", flush=True)

    for palette, lens in conditions_wanted:
        tag = f"{palette}-{lens}"
        for chart in charts_wanted:
            if not (out / "synth" / scene_name(chart, palette, lens) / "records.json").exists():
                continue
            sc = Scene(out, chart, palette, lens)
            keys = sc.keys()
            chosen = [k for k in SELECTION if k in keys]
            for pair in pairs_wanted:
                slug = pair.replace("/", "-")
                name = PAIR_NAMES[pair]
                if chart == "dpr":
                    for i, t in sc.tiles(pair, period=8.8):
                        title = f"Ringscheibe wie bei DPReview, {name}: Ringe 8,8 px, feines Feld 4,4 px, Mitte 2,2 px"
                        save(grid_image(sc, i, t, keys, 208, 2, 6, title, 30, 30, "all"), f"synth-dpr-{tag}-{slug}-alle.png")
                        save(grid_image(sc, i, t, chosen, 208, 3, 4, title, 30, 30, "all"), f"synth-dpr-{tag}-{slug}-auswahl.png")
                elif chart == "rings":
                    tiles = sc.tiles(pair)
                    if tiles:
                        cols = [num(t["params"]["period"], 1) + " px" for _, t in tiles]
                        save(ladder_image(sc, tiles, keys, 48, 2, f"Ringe, {name}: Periode von 8 px bis 1,6 px (Nyquist: 2 px)", cols), f"synth-ringe-{tag}-{slug}.png")
                elif chart == "zone":
                    for i, t in sc.tiles(pair):
                        title = f"Zonenplatte, {name}: Frequenz steigt mit dem Abstand von links oben, 0,5 c/px bei 160 px"
                        save(grid_image(sc, i, t, keys, 192, 2, 6, title, 96, 96, "all"), f"synth-zone-{tag}-{slug}.png")
                elif chart == "siemens":
                    for i, t in sc.tiles(pair):
                        title = f"Siemensstern, {name}: 60 Strahlenpaare, 0,5 c/px bei 19 px vom Mittelpunkt"
                        save(grid_image(sc, i, t, keys, 128, 3, 6, title, 0, 0, "all"), f"synth-siemens-{tag}-{slug}.png")
                elif chart == "edges":
                    tiles = sc.tiles(pair)
                    if tiles:
                        cols = [num(t["params"]["angle_deg"], 0) + "°" for _, t in tiles]
                        save(ladder_image(sc, tiles, keys, 40, 3, f"Schräge Kanten, {name}", cols), f"synth-kanten-{tag}-{slug}.png")
                elif chart == "grids":
                    tiles = sc.tiles(pair)
                    if tiles:
                        cols = ["| 2", "— 2", "| 3", "| 4", "8° 2,5", "45° 2,83", "Schach 1", "Schach 2", "Haar 5", "Punkte 3"]
                        save(ladder_image(sc, tiles, keys, 32, 4, f"Feine Gitter, {name} (Linien mit Periode in px, Schachbrett, Haarlinien, Punkte)", cols), f"synth-gitter-{tag}-{slug}.png")
    return written


# ----------------------------------------------------------------- curves

CURVES = [
    (CONTEXT_KEY, "#c0392b", "-", 2.6),
    ("mz_ls", "#c0392b", "--", 1.8),
    ("mz_dubois", "#e08e00", "-", 2.0),
    ("mz_off", "#e08e00", ":", 1.6),
    ("lr_ahd", "#1f77b4", "-", 1.2),
    ("lr_dht", "#1f77b4", "--", 1.2),
    ("rt_amaze", "#2ca02c", "-", 1.2),
    ("rt_rcd", "#2ca02c", "--", 1.2),
    ("rt_lmmse", "#2ca02c", ":", 1.4),
    ("dt_rcd", "#9467bd", "-", 1.2),
    ("lr_bilinear", "#7f7f7f", ":", 1.2),
]


def curve(recs, pairs, field):
    """`field` over frequency, the mean of `pairs`, from a zone plate's bins."""
    by_f = {}
    for r in recs:
        if r["pair"] in pairs and "freq" in r and "part" not in r and r.get(field) is not None:
            by_f.setdefault(round(r["freq"], 4), []).append(r[field])
    f = sorted(k for k, v in by_f.items() if len(v) == len(pairs))
    return f, [float(np.mean(by_f[k])) for k in f]


def write_curves(data, media: Path, wanted):
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    written = []
    panels = [
        ("contrast", "Übertragener Kontrast des Musters", (0.0, 1.15)),
        ("nrmse", "Fehler, auf das Muster bezogen (0,5: nicht mehr aufgelöst)", (0.0, 1.6)),
        ("fc", "Falschfarbe (Abstand in a*b* von der Linie der zwei Farben)", None),
    ]
    for palette, lens in wanted:
        recs = data.get(("zone", palette, lens))
        if not recs:
            continue
        for pairs, tag, name in ((COLOUR_PAIRS, "farbe", "Mittel der sieben Farbpaare"), ([BW], "schwarzweiss", "Schwarz auf Weiß")):
            fig, axes = plt.subplots(1, 3, figsize=(19, 6.2), dpi=110)
            for ax, (field, label, ylim) in zip(axes, panels):
                for key, colour_, style, width in CURVES:
                    if key in recs:
                        f, v = curve(recs[key], pairs, field)
                        ax.plot(f, v, color=colour_, linestyle=style, linewidth=width, label=LABELS[key])
                ax.axvline(0.5, color="#999999", linewidth=0.8)
                ax.axvline(0.25, color="#cccccc", linewidth=0.8)
                if field == "nrmse":
                    ax.axhline(0.5, color="#999999", linewidth=0.8, linestyle="--")
                ax.set_xlabel("Ortsfrequenz in Perioden je Pixel (Nyquist: 0,5)")
                ax.set_title(label, fontsize=11)
                ax.set_xlim(0.05, 0.7)
                if ylim:
                    ax.set_ylim(*ylim)
                ax.grid(alpha=0.25)
            axes[0].legend(fontsize=9, loc="lower left")
            fig.suptitle(
                f"Zonenplatte, {name}: {PALETTE_NAMES[palette]}, {LENS_NAMES[lens]} ({LENSES[lens][1]})", fontsize=12
            )
            fig.tight_layout()
            path = media / f"synth-kurven-{palette}-{lens}-{tag}.png"
            fig.savefig(path)
            plt.close(fig)
            written.append(str(path))
            print(f"wrote {path}", flush=True)
    return written


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--media", type=Path, default=None)
    ap.add_argument("--conditions", default="cam-lens", help="comma-separated PALETTE-LENS to draw; `all` for every one")
    ap.add_argument("--pairs", default=",".join(PAIRS), help="comma-separated colour pairs to draw")
    ap.add_argument("--charts", default=",".join(scenes.CHARTS), help="comma-separated charts to draw")
    ap.add_argument("--no-images", action="store_true")
    ap.add_argument("--no-tables", action="store_true")
    a = ap.parse_args()
    data = load(a.out)
    if not data:
        raise SystemExit(f"nothing measured under {a.out / 'synth'}; run run_synth.py first")
    if not a.no_tables:
        write_tables(data, a.out)
        print(f"wrote {a.out / 'report' / 'tables.md'}")
    if not a.no_images:
        wanted = (
            [(p, l) for p in PALETTE_NAMES for l in LENS_NAMES]
            if a.conditions == "all"
            else [tuple(c.split("-")) for c in a.conditions.split(",")]
        )
        media = a.media or a.out / "media"
        media.mkdir(parents=True, exist_ok=True)
        write_curves(data, media, wanted)
        write_images(a.out, media, wanted, a.pairs.split(","), a.charts.split(","))


if __name__ == "__main__":
    main()
