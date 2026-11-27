#!/usr/bin/env python3
"""The least-squares demosaic before and after a change, on camera files.

    python bench/bayer-rings/before_after_real.py --keep NikonZF.nef   # before the change
    cargo build --release -p mimizan-cli --example bayer_ls_develop
    python bench/bayer-rings/before_after_real.py --media DIR NikonZF.nef

`run_real.py` must have run on the files: its crops, its reference frame
and its colour are used. `--keep` puts its frame of the demosaic aside;
the second call develops the file again and draws each crop from before,
from after and from three other developers. A frame that is there is not
developed again: delete OUT/real/<file>/mz_ls.* after another change.
There is no original; the one figure is the colour in the crops
`run_real.py` was told are black and white. Written to
OUT/report/before-after-real.json.
"""

from __future__ import annotations

import argparse
import json
import shutil
from pathlib import Path

import numpy as np

import developers as dev
import metrics
import montage
from run_real import MAX_SHIFT, camera_to_srgb, colour_to_reference, placed, shift_between

KEY = "mz_ls"
BEFORE = "mz_ls_before"
BESIDE = ["dt_rcd", "rt_amaze", "lr_ahd"]


def cached(work: Path, key: str):
    info = json.loads((work / f"{key}.json").read_text())
    return np.load(work / f"{key}.npy").astype(np.float32) * (info["scale"] / 65535.0), info


def run_file(raw: Path, out: Path, media: Path, progs, zoom: int, prefix: str):
    work = out / "real" / raw.stem
    said = json.loads((work / "real.json").read_text())
    if not (work / f"{BEFORE}.npy").exists():
        raise SystemExit(f"{raw.name}: nothing was kept from before; run with --keep first")
    _, info = dev.develop(dev.BY_KEY[KEY], raw, work, progs)
    print(f"  {raw.stem}: developed in {info['seconds']} s", flush=True)
    ref, _ = cached(work, said["reference"])
    sides = (json.loads(p.read_text()) for p in sorted(work.glob("mz_*.json")))
    matrix = next((s["xyz_to_cam"] for s in sides if "xyz_to_cam" in s), None)
    to_srgb = camera_to_srgb(matrix) if matrix is not None else np.eye(3, dtype=np.float32)
    h, w = ref.shape[:2]
    gain = said["gain"]
    neutral = {c["name"] for c in said["crops"] if "stray_colour" in c}
    cut, stray, seconds = {}, {}, {}
    for key in (BEFORE, KEY):
        img, info = cached(work, key)
        seconds[key] = info["seconds"]
        dx, dy = shift_between(ref, img)
        if max(abs(dx), abs(dy)) > MAX_SHIFT:
            raise SystemExit(f"{raw.name}: {key} does not match the reference frame (shift {dx}, {dy})")
        on_ref = img if (dx, dy) == (0, 0) and img.shape == ref.shape else placed(img, ref.shape, dx, dy)
        cm = colour_to_reference(on_ref, ref)
        for c in said["crops"]:
            (cx, cy), size = c["centre"], c["size"]
            x0 = int(np.clip(round(cx * w - size / 2), 0, w - size))
            y0 = int(np.clip(round(cy * h - size / 2), 0, h - size))
            part = np.clip(on_ref[y0 : y0 + size, x0 : x0 + size] @ cm.T @ to_srgb.T * gain, 0.0, 1.0)
            cut[(c["name"], key)] = montage.to_srgb8(part)
            if c["name"] in neutral:
                lab = metrics.rgb_to_lab(part)
                ab = lab[..., 1:] - np.median(lab[..., 1:].reshape(-1, 2), axis=0)
                stray[(c["name"], key)] = float(np.hypot(ab[..., 0], ab[..., 1]).mean())
        del img, on_ref
    media.mkdir(parents=True, exist_ok=True)
    report = {"file": raw.name, "seconds": seconds, "crops": []}
    for c in said["crops"]:
        name = c["name"]
        panels = []
        for key, label in ((BEFORE, "Mimizan LS-Demosaic vorher"), (KEY, "Mimizan LS-Demosaic nachher")):
            note = f"Farbe im Schwarzweiß {stray[(name, key)]:.2f}".replace(".", ",") if (name, key) in stray else ""
            panels.append((label, note, "mimizan", cut[(name, key)]))
        for key in BESIDE:
            path = work / f"crop-{name}-{key}.npy"
            if path.exists():
                note = ""
                if key in c.get("stray_colour", {}):
                    note = f"Farbe im Schwarzweiß {c['stray_colour'][key]:.2f}".replace(".", ",")
                panels.append((dev.BY_KEY[key].label, note, "other", np.load(path)))
        differ = float(np.abs(cut[(name, KEY)].astype(np.float32) - cut[(name, BEFORE)].astype(np.float32)).mean())
        path = media / f"{prefix}real-{raw.stem}-{name}.png"
        title = f"{raw.name}: Ausschnitt „{name}“, {c['size']}×{c['size']} px, {zoom}× vergrößert"
        montage.grid(panels, len(panels), zoom, title=title, subtitle="Weißabgleich wie aufgenommen, linear entwickelt, keine Schärfung").save(path, optimize=True)
        print(f"  wrote {path}", flush=True)
        entry = {"name": name, "image": str(path), "mean_change_of_255": differ}
        if name in neutral:
            entry["stray_colour"] = {"before": stray[(name, BEFORE)], "after": stray[(name, KEY)]}
        report["crops"].append(entry)
    return report


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("raw", nargs="+", type=Path, help="camera files `run_real.py` has run on")
    ap.add_argument("--out", type=Path, default=dev.REPO / "out" / "bayer-rings")
    ap.add_argument("--keep", action="store_true", help="put the present frames of the demosaic aside as the ones before")
    ap.add_argument("--media", type=Path, default=None, help="folder for the strips (default OUT/media-before-after)")
    ap.add_argument("--prefix", default="vn-", help="what the strips' names begin with")
    ap.add_argument("--zoom", type=int, default=3)
    a = ap.parse_args()
    if a.keep:
        for raw in a.raw:
            work = a.out / "real" / raw.stem
            for ext in ("npy", "json"):
                if (work / f"{KEY}.{ext}").exists():
                    shutil.move(work / f"{KEY}.{ext}", work / f"{BEFORE}.{ext}")
            print(f"{raw.stem}: put aside")
        return
    progs = dev.programs()
    reports = [run_file(raw, a.out, a.media or a.out / "media-before-after", progs, a.zoom, a.prefix) for raw in a.raw]
    path = a.out / "report" / "before-after-real.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(reports, indent=1))
    print(f"wrote {path}")


if __name__ == "__main__":
    main()
