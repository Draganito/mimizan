#!/usr/bin/env python3
"""Variants against cases as one matrix; see `lab.py`.

    python bench/bayer-rings/lab_matrix.py --variants "now: bank=default; ..."

A column a case: CPSNR of the colour pairs, for the ring and zone charts
also the coarse band (to 0.20 c/px) and black on white. The last columns
are the mean over the Kodak pictures and the worst change among them.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np

import developers as dev
import lab
from run_synth import photographs

CHARTS = "rings-cam-lens:3,rings-cam-lens:3n,zone-cam-lens:3,siemens-cam-lens:3,edges-cam-lens:3,grids-cam-lens:3,dpr-cam-lens:3,dpr-cam-lens:3n"
KODAK = "kodak:01,kodak:03,kodak:08,kodak:13,kodak:15,kodak:19"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--charts", default=CHARTS)
    ap.add_argument("--kodak", default=KODAK)
    ap.add_argument("--variants", required=True)
    ap.add_argument("--save", default="")
    a = ap.parse_args()
    photos = photographs(a.out / "kodak")
    charts = [lab.Case(a.out, s, photos) for s in a.charts.split(",") if s]
    kodak = [lab.Case(a.out, s, photos) for s in a.kodak.split(",") if s]
    head = []
    for c in charts:
        short = c.spec.replace("-cam", "").replace("-lens", "l").replace("-sharp", "s").replace("-soft", "t").replace("-sat", "S")
        head.append(short)
        if c.spec.startswith(("rings", "zone")):
            head += ["≤.20", "s/w"]
        if c.spec.startswith(("grids", "edges")):
            head += ["s/w"]
    head += ["kodak", "worst"]
    print(f"{'':<22}" + "".join(f"{h:>10}" for h in head))
    base, saved = None, {}
    for name, opts in lab.parse_variants(a.variants):
        cells, secs = [], 0.0
        for c in charts:
            img, info = c.run(dict(opts))
            secs = max(secs, info.get("separated_s", 0.0))
            fig = c.measure(img)
            cells.append(fig["psnr"])
            if c.spec.startswith(("rings", "zone")):
                cells += [fig.get("bis 0,20 c/px"), fig.get("bw")]
            if c.spec.startswith(("grids", "edges")):
                cells += [fig.get("bw")]
            saved[f"{name}|{c.spec}"] = fig
        k = [c.measure(c.run(dict(opts))[0])["psnr"] for c in kodak]
        if base is None:
            base = k
        if k:
            cells += [float(np.mean(k)), float(min(x - b for x, b in zip(k, base)))]
        print(f"{name:<22}" + "".join(f"{v:10.2f}" if v is not None else f"{'–':>10}" for v in cells) + f"  {secs:.1f}s", flush=True)
    if a.save:
        (a.out / "lab" / a.save).write_text(json.dumps(saved, indent=1))


if __name__ == "__main__":
    main()
