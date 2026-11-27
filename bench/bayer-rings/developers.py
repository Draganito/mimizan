"""The developers of the bench, each called on equal terms.

Every method takes a Bayer RAW (a CFA DNG of the synthetic charts, or a
camera file) and returns linear, white-balanced camera RGB: the as-shot
balance, no sharpening, no noise reduction, no false-colour suppression, no
lens correction, no tone curve. Colour management differs between the
programs; `metrics.fit_colour` takes that out afterwards with one 3x3.

Programs are looked up through environment variables, then on PATH:

    MIMIZAN            target/release/mimizan of this checkout
    MIMIZAN_LS         target/release/examples/bayer_ls_develop
    DCRAW_EMU          dcraw_emu (LibRaw)
    RAWTHERAPEE_CLI    rawtherapee-cli
    DARKTABLE_CLI      darktable-cli

A program that is not there is skipped, and the report says so.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import tifffile

import dngio

REPO = Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class Method:
    key: str
    label: str
    program: str  # mimizan | libraw | rawtherapee | darktable | reference
    arg: str

    @property
    def is_mimizan(self) -> bool:
        return self.program == "mimizan"


METHODS = [
    Method("mz_dubois", "Mimizan Dubois", "mimizan", "dubois"),
    Method("mz_adaptive", "Mimizan Adaptive", "mimizan", "adaptive"),
    Method("mz_off", "Mimizan Off", "mimizan", "off"),
    Method("mz_ls", "Mimizan LS-Demosaic", "mimizan", "ls"),
    Method("lr_bilinear", "LibRaw bilinear", "libraw", "0"),
    Method("lr_vng", "LibRaw VNG", "libraw", "1"),
    Method("lr_ppg", "LibRaw PPG", "libraw", "2"),
    Method("lr_ahd", "LibRaw AHD", "libraw", "3"),
    Method("lr_dcb", "LibRaw DCB", "libraw", "4"),
    Method("lr_dht", "LibRaw DHT", "libraw", "11"),
    Method("lr_aahd", "LibRaw AAHD", "libraw", "12"),
    Method("rt_amaze", "RawTherapee AMaZE", "rawtherapee", "amaze"),
    Method("rt_rcd", "RawTherapee RCD", "rawtherapee", "rcd"),
    Method("rt_dcb", "RawTherapee DCB", "rawtherapee", "dcb"),
    Method("rt_lmmse", "RawTherapee LMMSE", "rawtherapee", "lmmse"),
    Method("rt_igv", "RawTherapee IGV", "rawtherapee", "igv"),
    Method("rt_ahd", "RawTherapee AHD", "rawtherapee", "ahd"),
    Method("rt_eahd", "RawTherapee EAHD", "rawtherapee", "eahd"),
    Method("rt_hphd", "RawTherapee HPHD", "rawtherapee", "hphd"),
    Method("rt_vng4", "RawTherapee VNG4", "rawtherapee", "vng4"),
    Method("rt_fast", "RawTherapee Fast", "rawtherapee", "fast"),
    Method("dt_rcd", "darktable RCD", "darktable", "5"),
    Method("dt_amaze", "darktable AMaZE", "darktable", "1"),
    Method("dt_lmmse", "darktable LMMSE", "darktable", "6"),
    Method("dt_ppg", "darktable PPG", "darktable", "0"),
    Method("dt_vng4", "darktable VNG4", "darktable", "2"),
    Method("ref_malvar", "Malvar 2004", "reference", "malvar"),
    Method("ref_menon", "Menon 2007", "reference", "menon"),
]
BY_KEY = {m.key: m for m in METHODS}


def _program(env: str, default: str) -> str | None:
    cand = os.environ.get(env, default)
    if os.sep in cand:
        return cand if Path(cand).is_file() else None
    return shutil.which(cand)


def programs() -> dict:
    return {
        "mimizan": _program("MIMIZAN", str(REPO / "target/release/mimizan")),
        "mimizan_ls": _program("MIMIZAN_LS", str(REPO / "target/release/examples/bayer_ls_develop")),
        "libraw": _program("DCRAW_EMU", "dcraw_emu"),
        "rawtherapee": _program("RAWTHERAPEE_CLI", "rawtherapee-cli"),
        "darktable": _program("DARKTABLE_CLI", "darktable-cli"),
    }


def _first_line(cmd) -> str:
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired):
        return "?"
    text = (p.stdout or "") + (p.stderr or "")
    for line in text.splitlines():
        if line.strip():
            return line.strip()
    return "?"


def _libraw_version(dcraw_emu: str) -> str:
    """dcraw_emu does not say its version; the library it loads does."""
    try:
        ldd = subprocess.run(["ldd", dcraw_emu], capture_output=True, text=True).stdout
        lib = re.search(r"=>\s*(\S*libraw\S*)", ldd).group(1)
        m = re.search(rb"(\d+\.\d+\.\d+)-Release", Path(lib).read_bytes())
        return m.group(1).decode()
    except Exception:
        return ""


def versions(progs: dict) -> dict:
    out = {}
    if progs["mimizan"]:
        out["mimizan"] = _first_line([progs["mimizan"], "--version"])
    if progs["libraw"]:
        out["libraw"] = f"LibRaw {_libraw_version(progs['libraw'])} (dcraw_emu)".replace("  ", " ")
    if progs["rawtherapee"]:
        out["rawtherapee"] = _first_line([progs["rawtherapee"], "-h"]).rstrip(".")
    if progs["darktable"]:
        out["darktable"] = _first_line([progs["darktable"], "--version"])
    try:
        import colour_demosaicing

        out["reference"] = f"colour-demosaicing {colour_demosaicing.__version__}"
    except Exception:
        pass
    return out


def available(method: Method, progs: dict) -> bool:
    if method.program == "mimizan":
        return bool(progs["mimizan_ls"] if method.arg == "ls" else progs["mimizan"])
    if method.program == "reference":
        try:
            import colour_demosaicing  # noqa: F401
        except Exception:
            return False
        return True
    return bool(progs[method.program])


def _run(cmd, **kw):
    p = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if p.returncode != 0:
        raise RuntimeError(f"{' '.join(map(str, cmd))}\n{p.stdout[-2000:]}\n{p.stderr[-2000:]}")
    return p


def srgb_decode(v):
    v = np.asarray(v, dtype=np.float32)
    return np.where(v <= 0.04045, v / 12.92, ((v + 0.055) / 1.055) ** 2.4).astype(np.float32)


def read_ppm16(path) -> np.ndarray:
    buf = Path(path).read_bytes()
    fields, pos = [], 0
    while len(fields) < 4:
        while buf[pos : pos + 1].isspace():
            pos += 1
        if buf[pos : pos + 1] == b"#":
            pos = buf.index(b"\n", pos)
            continue
        start = pos
        while not buf[pos : pos + 1].isspace():
            pos += 1
        fields.append(buf[start:pos])
    pos += 1
    w, h, maxval = int(fields[1]), int(fields[2]), int(fields[3])
    dtype = ">u2" if maxval > 255 else "u1"
    img = np.frombuffer(buf, dtype=dtype, count=w * h * 3, offset=pos).reshape(h, w, 3)
    return (img.astype(np.float32) / maxval).astype(np.float32)


# ------------------------------------------------------------------ Mimizan


def _mimizan(method: Method, raw: Path, work: Path, progs: dict):
    if method.arg == "ls":
        out = work / "mz_ls.ppm"
        p = _run([progs["mimizan_ls"], str(raw), str(out)])
        info = json.loads(p.stdout.strip().splitlines()[-1])
        img = read_ppm16(out) * np.asarray(info["balance"], dtype=np.float32)
        out.unlink()
        prior = {k: info[k] for k in ("alpha", "alpha_c", "chroma", "noise", "lens") if k in info}
        return img, {"offset": [info["x"], info["y"]], "balance": info["balance"], "prior": prior}
    out = work / f"{method.key}.dng"
    _run(
        [
            progs["mimizan"], "--log", "warn", "negative", str(raw), "--mask", method.arg, "--dng",
            "--cameras", str(REPO / "cameras"), "-o", str(out),
        ]
    )  # fmt: skip
    img, neutral = dngio.read_linear_dng(out)
    matrix = dngio.colour_matrix(out)
    out.unlink()
    info = {"balance": (1.0 / neutral).tolist()}
    if matrix is not None:
        info["xyz_to_cam"] = matrix.tolist()
    return (img / neutral).astype(np.float32), info


# ------------------------------------------------------------------- LibRaw


def _libraw(method: Method, raw: Path, work: Path, progs: dict):
    # Linear 16 bit, the camera's channels, the as-shot balance, highlights
    # clipped, no automatic brightening, no maximum adjustment.
    local = work / f"lr_{raw.name}"
    if not local.exists():
        try:
            local.symlink_to(raw.resolve())
        except OSError:
            shutil.copy(raw, local)
    suffix = f".{method.key}.ppm"
    _run([progs["libraw"], "-4", "-o", "0", "-w", "-c", "0", "-H", "0", "-q", method.arg, "-Z", suffix, str(local)])
    out = Path(str(local) + suffix)
    img = read_ppm16(out)
    out.unlink()
    local.unlink()
    return img, {}


# -------------------------------------------------------------- RawTherapee

RT_PROFILE = """[Version]
AppVersion=5.11
Version=351

[Exposure]
Auto=false
Clip=0
Compensation=0
Brightness=0
Contrast=0
Saturation=0
Black=0
HighlightCompr=0
HighlightComprThreshold=0
ShadowCompr=0
HistogramMatching=false
CurveFromHistogramMatching=false
ClampOOG=true
CurveMode=Standard
CurveMode2=Standard
Curve=0
Curve2=0

[HLRecovery]
Enabled=false
Method=Blend

[White Balance]
Enabled=true
Setting=Camera
Temperature=6504
Green=1
Equal=1

[Color Management]
InputProfile=(none)
ToneCurve=false
ApplyLookTable=false
ApplyBaselineExposureOffset=false
ApplyHueSatMap=false
DCPIlluminant=0
WorkingProfile=sRGB
OutputProfile=RTv4_sRGB
OutputProfileIntent=Relative
OutputBPC=true

[Sharpening]
Enabled=false

[SharpenEdge]
Enabled=false

[SharpenMicro]
Enabled=false

[PostDemosaicSharpening]
Enabled=false

[Impulse Denoising]
Enabled=false

[Defringing]
Enabled=false

[Directional Pyramid Denoising]
Enabled=false

[LensProfile]
LcMode=none
UseDistortion=false
UseVignette=false
UseCA=false

[RAW]
CA=false
HotPixelFilter=false
DeadPixelFilter=false
PreExposure=1

[RAW Bayer]
Method={method}
Border=0
CcSteps=0
PreBlack0=0
PreBlack1=0
PreBlack2=0
PreBlack3=0
PreTwoGreen=true
LineDenoise=0
GreenEqThreshold=0
DCBIterations=2
DCBEnhance=true
LMMSEIterations=2
DualDemosaicAutoContrast=false
DualDemosaicContrast=0
PDAFLinesFilter=false
"""


def _rawtherapee(method: Method, raw: Path, work: Path, progs: dict):
    pp3 = work / f"{method.key}.pp3"
    pp3.write_text(RT_PROFILE.format(method=method.arg))
    out = work / f"{method.key}.tif"
    _run([progs["rawtherapee"], "-o", str(out), "-t", "-b16", "-Y", "-q", "-p", str(pp3), "-c", str(raw)])
    img = srgb_decode(tifffile.imread(out)[..., :3].astype(np.float32) / 65535.0)
    out.unlink()
    pp3.unlink()
    return img, {}


# ---------------------------------------------------------------- darktable

# dt_iop_demosaic_params_t, version 6: green_eq, median_thrs,
# color_smoothing, demosaicing_method, lmmse_refine, dual_thrs, then the
# capture sharpening fields, which stay off. Only the method is changed.
DT_DEMOSAIC = re.compile(r'(darktable:operation="demosaic"\s+darktable:enabled="1"\s+darktable:modversion="6"\s+darktable:params=")([0-9a-f]{96})(")')
DT_HIGHLIGHTS = re.compile(r'(darktable:operation="highlights"\s+darktable:enabled=")1(")')


# The space darktable exports in. The synthetic files state sRGB primaries,
# so Rec. 709 leaves their numbers as they are; a camera file is exported in
# Rec. 2020, which holds a camera's saturated colours without clipping them.
DT_ICC = "LIN_REC709"


def _darktable_base(progs: dict, work: Path):
    cfg = work / "dtcfg"
    cfg.mkdir(exist_ok=True)
    return [
        "--core", "--configdir", str(cfg), "--library", str(cfg / "library.db"),
        "--conf", "plugins/darkroom/workflow=none",
        "--conf", "plugins/darkroom/chromatic-adaptation=legacy",
        "--conf", "plugins/imageio/format/tiff/bpp=16",
        "--conf", "plugins/imageio/format/tiff/compress=0",
        "--conf", "write_sidecar_files=on import",
        "--disable-opencl",
    ]  # fmt: skip


def _darktable(method: Method, raw: Path, work: Path, progs: dict):
    # darktable writes the history it gives a new file into a sidecar. That
    # sidecar is the template: the demosaicer is replaced in it and the
    # highlight reconstruction switched off, nothing else.
    local = work / f"dt_{raw.name}"
    template = work / f"dt_{raw.name}.template.xmp"
    if not template.exists():
        shutil.copy(raw, local)
        probe = work / "dt_probe.tif"
        _run([progs["darktable"], str(local), str(probe), "--icc-type", DT_ICC, *_darktable_base(progs, work)])
        side = Path(str(local) + ".xmp")
        text = side.read_text()
        if not DT_DEMOSAIC.search(text):
            raise RuntimeError("darktable wrote a demosaic history this bench does not know")
        template.write_text(text)
        side.unlink()
        probe.unlink()
        local.unlink()
    text = template.read_text()
    code = int(method.arg).to_bytes(4, "little").hex()
    text = DT_DEMOSAIC.sub(lambda m: m.group(1) + m.group(2)[:24] + code + m.group(2)[32:] + m.group(3), text)
    text = DT_HIGHLIGHTS.sub(r"\g<1>0\g<2>", text)
    xmp = work / f"{method.key}.xmp"
    xmp.write_text(text)
    out = work / f"{method.key}.tif"
    out.unlink(missing_ok=True)
    cfg = work / "dtcfg" / "library.db"
    cfg.unlink(missing_ok=True)
    _run([progs["darktable"], str(raw), str(xmp), str(out), "--icc-type", DT_ICC, *_darktable_base(progs, work)])
    img = tifffile.imread(out)[..., :3].astype(np.float32) / 65535.0
    out.unlink()
    xmp.unlink()
    return img, {}


# ---------------------------------------------------- reference algorithms


def _reference(method: Method, raw: Path, work: Path, progs: dict, mosaic=None, balance=None):
    """Malvar-He-Cutler 2004 and Menon 2007 from colour-demosaicing, on the
    balanced mosaic. Only for the synthetic charts, where the mosaic is at
    hand; a camera file would need a decoder of its own."""
    import colour_demosaicing as cd

    if mosaic is None:
        raise RuntimeError("the reference algorithms run on the synthetic mosaics only")
    cfa = mosaic.astype(np.float64)
    if method.arg == "malvar":
        img = cd.demosaicing_CFA_Bayer_Malvar2004(cfa, "RGGB")
    else:
        img = cd.demosaicing_CFA_Bayer_Menon2007(cfa, "RGGB")
    return np.clip(img, 0.0, 1.0).astype(np.float32), {}


RUNNERS = {
    "mimizan": _mimizan,
    "libraw": _libraw,
    "rawtherapee": _rawtherapee,
    "darktable": _darktable,
    "reference": _reference,
}


def develop(method: Method, raw: Path, work: Path, progs: dict, mosaic=None):
    """Linear balanced RGB (H, W, 3) float32, an info dict, and the seconds
    it took. The result is cached in `work`."""
    work.mkdir(parents=True, exist_ok=True)
    cache = work / f"{method.key}.npy"
    side = work / f"{method.key}.json"
    if cache.exists() and side.exists():
        info = json.loads(side.read_text())
        return np.load(cache).astype(np.float32) * (info["scale"] / 65535.0), info
    t0 = time.time()
    if method.program == "reference":
        img, info = _reference(method, raw, work, progs, mosaic=mosaic)
    else:
        img, info = RUNNERS[method.program](method, raw, work, progs)
    info["seconds"] = round(time.time() - t0, 2)
    # 16 bits a sample; a balanced channel of a camera file may stand above 1.
    info["scale"] = float(max(1.0, img.max()))
    np.save(cache, np.round(np.clip(img / info["scale"], 0.0, 1.0) * 65535.0).astype(np.uint16))
    side.write_text(json.dumps(info))
    return np.load(cache).astype(np.float32) * (info["scale"] / 65535.0), info
