#!/usr/bin/env python3
"""Experiments on the least-squares demosaic, on the synthetic charts.

    python bench/bayer-rings/lab.py --cases rings-cam-lens:3,dpr-cam-lens:3 \\
        --variants "now: bank=default; wide: bank=default+c64+c256"

A case is a chart lying in a frame of photographs `factor` times its size
(1: the chart alone), or `photo:NAME`, a photograph alone. A variant is a
name and the `key=value` arguments of `examples/bayer_ls_lab.rs`. The
search over the frame is done once a case and kept; a variant only
separates the chart's region again, so it takes seconds. Every result is
measured against the original as `run_synth.py` measures.

Everything is kept under OUT/lab.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import time
from pathlib import Path

import numpy as np

import developers as dev
import metrics
import report
import scenes
from ls_prior import figures
from run_synth import context_canvas, photographs

LAB = Path(os.environ.get("MIMIZAN_LS_LAB", dev.REPO / "target" / "release" / "examples" / "bayer_ls_lab"))
BORDER = 16


KODAK_NATIVE = [f"kodim{i:02d}" for i in (1, 3, 5, 7, 8, 13, 15, 19, 21, 23)]


def kodak(folder: Path, name: str, scale: int):
    """A Kodak picture in light, landscape, even sides."""
    from PIL import Image

    im = Image.open(folder / f"{name}.png").convert("RGB")
    if im.width < im.height:
        im = im.rotate(90, expand=True)
    if scale > 1:
        im = im.resize((im.width * scale, im.height * scale), Image.LANCZOS)
    v = dev.srgb_decode(np.asarray(im, np.float32) / 255.0) * 0.9
    return v[: v.shape[0] // 2 * 2, : v.shape[1] // 2 * 2]


def lens(img, sigma: float):
    """`img` behind a Gaussian lens of `sigma` px."""
    from scipy import ndimage

    if sigma <= 0:
        return img
    return np.stack([ndimage.gaussian_filter(img[..., c], sigma, mode="nearest") for c in range(3)], axis=-1)


class Case:
    """One mosaic, where the measured part lies in it and its original."""

    def __init__(self, out: Path, spec: str, photos):
        self.spec = spec
        self.dir = out / "lab" / spec.replace(":", "-f")
        self.dir.mkdir(parents=True, exist_ok=True)
        if spec.startswith("kodak"):
            # kodak:07 is the picture as it is, kodak2:07 enlarged twice (Lanczos).
            kind, number = spec.split(":")
            canvas = kodak(out / "kodak", f"kodim{number}", 2 if kind == "kodak2" else 1)
            self.truth, self.meta, self.colours = canvas, None, None
            self.region = (0, 0, canvas.shape[1], canvas.shape[0])
        else:
            # rings-cam-lens:3 lies in photographs enlarged twice, :3n in
            # photographs as they are, behind the chart's own lens.
            name, factor = spec.split(":")
            native, factor = factor.endswith("n"), int(factor.rstrip("n"))
            work = out / "synth" / name
            self.meta = json.loads((work / "scene.json").read_text())
            self.truth = np.load(work / "truth.npy")
            self.colours = scenes.palette(self.meta["palette"])
            h, w, _ = self.truth.shape
            if factor == 1:
                canvas, ox, oy = self.truth, 0, 0
            elif native:
                behind = [lens(kodak(out / "kodak", n, 1), self.meta["sigma"]) for n in KODAK_NATIVE]
                canvas, ox, oy = context_canvas(self.truth, behind, factor)
            else:
                canvas, ox, oy = context_canvas(self.truth, photos, factor)
            self.region = (ox, oy, w, h)
        self.width = canvas.shape[1]
        self.mosaic = self.dir / "mosaic.f32"
        if not self.mosaic.exists():
            seed = 1 if self.region[2] == self.width and self.meta else 2
            m = scenes.mosaic(canvas, seed=seed).astype(np.float32) / 65535.0
            m.tofile(self.mosaic)

    def run(self, opts: dict):
        """The measured part under `opts`, and what the program said."""
        opts = {k: str(v) for k, v in opts.items()}
        said = json.dumps(opts, sort_keys=True)
        if opts.get("bank", "default") == "default":
            # The program's own bank changes with the program; a bank spelt out does not.
            said += str(LAB.stat().st_mtime_ns)
        key = hashlib.sha1(said.encode()).hexdigest()[:16]
        raw, side = self.dir / f"{key}.f32", self.dir / f"{key}.json"
        x, y, w, h = self.region
        if not (raw.exists() and side.exists()):
            spectrum = self.dir / "spectrum.json"
            if "spectrum_of" in opts:  # the spectrum another case found
                spectrum = self.dir.parent / opts.pop("spectrum_of").replace(":", "-f") / "spectrum.json"
            args = [str(LAB), str(self.mosaic), str(self.width), str(raw), f"spectrum={spectrum}"]
            if self.meta is not None and w != self.width:
                args.append(f"region={x},{y},{w},{h}")
            args += [f"{k}={v}" for k, v in opts.items()]
            t0 = time.time()
            p = subprocess.run(args, capture_output=True, text=True)
            if p.returncode != 0:
                raise RuntimeError(p.stderr[-600:])
            info = json.loads(p.stdout.strip().splitlines()[-1])
            info["seconds"] = round(time.time() - t0, 2)
            side.write_text(json.dumps(info))
        img = np.fromfile(raw, dtype=np.float32).reshape(h, w, 3)
        return img, json.loads(side.read_text())

    def measure(self, img):
        """The figures of `img` against the original."""
        if self.meta is None:
            b = BORDER
            a = metrics.srgb_encode(np.clip(img, 0, 1))[b:-b, b:-b]
            t = metrics.srgb_encode(np.clip(self.truth, 0, 1))[b:-b, b:-b]
            mse = float(np.mean((a - t) ** 2))
            return {"psnr": -10 * np.log10(mse)}
        cm, _ = metrics.fit_colour(img, self.truth, self.meta["tiles"])
        maps = metrics.Maps(metrics.apply_colour(img, cm), self.truth)
        recs = []
        for i, t in enumerate(self.meta["tiles"]):
            for r in metrics.tile_records(maps, t, self.colours):
                r["tile"] = i
                recs.append(r)
        recs = report.patterns(recs)
        fig = figures(recs)
        bw = report.pool(report.whole([r for r in recs if r["pair"] == "black/white"]))
        fig["bw"] = bw["psnr"] if bw else None
        return fig


def parse_variants(text: str):
    out = []
    for part in text.split(";"):
        if not part.strip():
            continue
        name, _, rest = part.partition(":")
        out.append((name.strip(), dict(kv.split("=", 1) for kv in rest.split())))
    return out


def table(cases, variants, keys=("psnr", "bis 0,20 c/px", "0,25–0,37 c/px", "fc", "zipper", "bw")):
    """One line a variant and case."""
    rows = {}
    for name, opts in variants:
        for c in cases:
            img, info = c.run(opts)
            fig = c.measure(img)
            rows[(name, c.spec)] = fig
            cells = " ".join(
                f"{k.split(' ')[0][:9]}={fig[k]:6.2f}" if fig.get(k) is not None else f"{k.split(' ')[0][:9]}=   –  "
                for k in keys
                if k in fig
            )
            print(f"{name:<22} {c.spec:<22} {cells}  ({info['seconds']} s)", flush=True)
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--cases", default="rings-cam-lens:3,dpr-cam-lens:3")
    ap.add_argument("--variants", default="now: bank=default")
    a = ap.parse_args()
    photos = photographs(a.out / "kodak")
    cases = [Case(a.out, s, photos) for s in a.cases.split(",")]
    rows = table(cases, parse_variants(a.variants))
    (a.out / "lab" / "last.json").write_text(json.dumps({f"{k[0]}|{k[1]}": v for k, v in rows.items()}, indent=1))


if __name__ == "__main__":
    main()
