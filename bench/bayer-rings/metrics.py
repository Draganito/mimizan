"""Error measures against the known original.

All colour differences are taken in CIELAB (D65), reading the linear RGB of
the charts as linear sRGB, which is how the crops are shown.

* overall error      CPSNR on sRGB-encoded values (the convention of the
                     demosaicing literature) and the mean CIEDE2000.
* luminance error    PSNR of (R + 2G + B)/4, Mimizan's native luminance.
* chroma error       mean distance in the a*b* plane.
* false colour       distance in the a*b* plane from the line of mixtures
                     of the tile's two colours: a pattern that is merely
                     unresolved lies on that line, a colour that is neither
                     of the two nor a mixture of them does not. For black on
                     white it is the plain chroma.
* zipper             Lu and Tan 2003: the share of pixels whose difference
                     to their most similar neighbour grew by more than
                     2.3 Delta E against the original.
* resolved frequency the highest frequency up to which the pattern is
                     reproduced with a normalised error of 0.5 or less
                     (error power a quarter of the pattern's own).
"""

from __future__ import annotations

import numpy as np

SRGB_TO_XYZ = np.array(
    [[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]],
    dtype=np.float32,
)
WHITE_D65 = SRGB_TO_XYZ.sum(axis=1)
NRMSE_RESOLVED = 0.5
FALSE_COLOUR_VISIBLE = 5.0


def srgb_encode(v):
    v = np.clip(v, 0.0, 1.0)
    return np.where(v <= 0.0031308, v * 12.92, 1.055 * np.power(v, 1 / 2.4) - 0.055).astype(np.float32)


def rgb_to_lab(rgb):
    xyz = np.clip(rgb, 0.0, None).astype(np.float32) @ SRGB_TO_XYZ.T / WHITE_D65
    f = np.where(xyz > 216 / 24389, np.cbrt(xyz), (24389 / 27 * xyz + 16) / 116)
    lab = np.empty_like(f)
    lab[..., 0] = 116 * f[..., 1] - 16
    lab[..., 1] = 500 * (f[..., 0] - f[..., 1])
    lab[..., 2] = 200 * (f[..., 1] - f[..., 2])
    return lab


def delta_e00(lab1, lab2):
    """CIEDE2000 (Sharma, Wu, Dalal 2005)."""
    l1, a1, b1 = (lab1[..., i].astype(np.float64) for i in range(3))
    l2, a2, b2 = (lab2[..., i].astype(np.float64) for i in range(3))
    c_bar = (np.hypot(a1, b1) + np.hypot(a2, b2)) / 2
    g = 0.5 * (1 - np.sqrt(c_bar**7 / (c_bar**7 + 25.0**7)))
    a1p, a2p = (1 + g) * a1, (1 + g) * a2
    c1p, c2p = np.hypot(a1p, b1), np.hypot(a2p, b2)
    h1p = np.degrees(np.arctan2(b1, a1p)) % 360
    h2p = np.degrees(np.arctan2(b2, a2p)) % 360
    dlp = l2 - l1
    dcp = c2p - c1p
    dh = h2p - h1p
    dh = np.where(dh > 180, dh - 360, np.where(dh < -180, dh + 360, dh))
    dh = np.where(c1p * c2p == 0, 0.0, dh)
    dhp = 2 * np.sqrt(c1p * c2p) * np.sin(np.radians(dh) / 2)
    lp = (l1 + l2) / 2
    cp = (c1p + c2p) / 2
    hsum = h1p + h2p
    hp = np.where(
        c1p * c2p == 0,
        hsum,
        np.where(np.abs(h1p - h2p) <= 180, hsum / 2, np.where(hsum < 360, (hsum + 360) / 2, (hsum - 360) / 2)),
    )
    t = (
        1
        - 0.17 * np.cos(np.radians(hp - 30))
        + 0.24 * np.cos(np.radians(2 * hp))
        + 0.32 * np.cos(np.radians(3 * hp + 6))
        - 0.20 * np.cos(np.radians(4 * hp - 63))
    )
    dtheta = 30 * np.exp(-(((hp - 275) / 25) ** 2))
    rc = 2 * np.sqrt(cp**7 / (cp**7 + 25.0**7))
    sl = 1 + 0.015 * (lp - 50) ** 2 / np.sqrt(20 + (lp - 50) ** 2)
    sc = 1 + 0.045 * cp
    sh = 1 + 0.015 * cp * t
    rt = -np.sin(np.radians(2 * dtheta)) * rc
    return np.sqrt(
        (dlp / sl) ** 2 + (dcp / sc) ** 2 + (dhp / sh) ** 2 + rt * (dcp / sc) * (dhp / sh)
    ).astype(np.float32)


# ------------------------------------------------------------ colour match


def patch_means(img, tiles, inset=14):
    rows = []
    for t in tiles:
        if t["kind"] != "flat":
            continue
        x, y, w, h = t["rect"]
        rows.append(img[y + inset : y + h - inset, x + inset : x + w - inset].reshape(-1, 3).mean(axis=0))
    return np.asarray(rows, dtype=np.float64)


def fit_colour(out, truth, tiles):
    """The 3x3 that takes a developer's flat patches to the original's, and
    what is left of the patches' error after it (linear RMS)."""
    o, t = patch_means(out, tiles), patch_means(truth, tiles)
    m, *_ = np.linalg.lstsq(o, t, rcond=None)
    m = m.T
    residual = float(np.sqrt(np.mean((o @ m.T - t) ** 2)))
    return m, residual


def apply_colour(out, m):
    return np.clip(out @ m.T.astype(np.float32), 0.0, 1.0)


# ------------------------------------------------------------ per-image maps


class Maps:
    """What is computed once for a developed chart."""

    def __init__(self, out, truth):
        self.out, self.truth = out, truth
        self.enc_err2 = (srgb_encode(out) * 255.0 - srgb_encode(truth) * 255.0) ** 2
        y_out = (out[..., 0] + 2 * out[..., 1] + out[..., 2]) / 4
        y_truth = (truth[..., 0] + 2 * truth[..., 1] + truth[..., 2]) / 4
        self.luma_err2 = (srgb_encode(y_out) * 255.0 - srgb_encode(y_truth) * 255.0) ** 2
        self.lab_out, self.lab_truth = rgb_to_lab(out), rgb_to_lab(truth)
        self.de = delta_e00(self.lab_out, self.lab_truth)
        d = self.lab_out - self.lab_truth
        self.dl = np.abs(d[..., 0])
        self.dc = np.hypot(d[..., 1], d[..., 2])
        self.zipper = zipper_map(self.lab_out, self.lab_truth)
        self.enc_out, self.enc_truth = srgb_encode(out), srgb_encode(truth)


def zipper_map(lab_out, lab_truth, threshold=2.3):
    """Lu and Tan: a pixel zips when its difference to the neighbour it is
    most like in the original has grown by more than `threshold`."""
    h, w, _ = lab_truth.shape
    best = np.full((h - 2, w - 2), np.inf, dtype=np.float32)
    grown = np.zeros((h - 2, w - 2), dtype=np.float32)
    ct, co = lab_truth[1:-1, 1:-1], lab_out[1:-1, 1:-1]
    for dy in (-1, 0, 1):
        for dx in (-1, 0, 1):
            if dx == 0 and dy == 0:
                continue
            nt = lab_truth[1 + dy : h - 1 + dy, 1 + dx : w - 1 + dx]
            no = lab_out[1 + dy : h - 1 + dy, 1 + dx : w - 1 + dx]
            dt = np.sqrt(((ct - nt) ** 2).sum(axis=-1))
            do = np.sqrt(((co - no) ** 2).sum(axis=-1))
            closer = dt < best
            best = np.where(closer, dt, best)
            grown = np.where(closer, do - dt, grown)
    z = np.zeros((h, w), dtype=bool)
    z[1:-1, 1:-1] = grown > threshold
    return z


def mixture_locus(fg, bg, n=131):
    """a*b* of the mixtures of two colours, a little beyond both ends."""
    t = np.linspace(-0.15, 1.15, n, dtype=np.float32)[:, None]
    rgb = np.clip(np.asarray(bg, np.float32) + t * (np.asarray(fg, np.float32) - np.asarray(bg, np.float32)), 0, 1)
    return rgb_to_lab(rgb)[:, 1:]


def false_colour(lab, locus):
    ab = lab[..., 1:].reshape(-1, 1, 2)
    best = np.full(ab.shape[0], np.inf, dtype=np.float32)
    for k in range(0, locus.shape[0], 16):  # in slabs, to bound the memory
        d = np.sqrt(((ab - locus[None, k : k + 16]) ** 2).sum(axis=-1)).min(axis=1)
        best = np.minimum(best, d)
    return best.reshape(lab.shape[:-1])


# ------------------------------------------------------------- region stats


def region_stats(maps: Maps, sl, mask, fc):
    """Statistics of one region: `sl` the tile's slice, `mask` the region in
    it, `fc` the tile's false-colour map."""
    n = int(mask.sum())
    if n == 0:
        return None
    e2 = maps.enc_err2[sl][mask]
    mse = float(e2.mean())
    out, truth = maps.enc_out[sl][mask], maps.enc_truth[sl][mask]
    signal = float(((truth - truth.mean(axis=0)) ** 2).sum())
    noise = float(((out - truth) ** 2).sum())
    cov = float(((out - out.mean(axis=0)) * (truth - truth.mean(axis=0))).sum())
    de = maps.de[sl][mask]
    f = fc[mask]
    return {
        "n": n,
        "mse": mse,
        "mse_luma": float(maps.luma_err2[sl][mask].mean()),
        "de": float(de.mean()),
        "de95": float(np.percentile(de, 95)),
        "dl": float(maps.dl[sl][mask].mean()),
        "dc": float(maps.dc[sl][mask].mean()),
        "fc": float(f.mean()),
        "fc95": float(np.percentile(f, 95)),
        "fc_share": float((f > FALSE_COLOUR_VISIBLE).mean()),
        "zipper": float(maps.zipper[sl][mask].mean()),
        "nrmse": float(np.sqrt(noise / signal)) if signal > 1e-9 else None,
        "contrast": cov / signal if signal > 1e-9 else None,
    }


FREQ_EDGES = np.round(np.arange(0.05, 0.701, 0.025), 3)


def edge_stats(maps: Maps, sl, dist, mask, fg, bg):
    """Along an edge: the unrest at equal distance from it (the zipper's
    amplitude, in Delta E) and the 10-90 % width of the transition."""
    d = dist[mask]
    resid = (maps.lab_out[sl] - maps.lab_truth[sl])[mask]
    bins = np.floor((d + 4.0) / 0.25).astype(int)
    var, count = 0.0, 0
    for b in range(32):
        r = resid[bins == b]
        if len(r) > 3:
            var += float(((r - r.mean(axis=0)) ** 2).sum())
            count += len(r)
    axis = np.asarray(fg, np.float32) - np.asarray(bg, np.float32)

    def width(img):
        s = ((img[sl][mask] - np.asarray(bg, np.float32)) @ axis) / float(axis @ axis)
        centres, prof = [], []
        for b in range(32):
            v = s[bins == b]
            if len(v) > 3:
                centres.append(-4.0 + 0.25 * (b + 0.5))
                prof.append(float(v.mean()))
        centres, prof = np.asarray(centres), np.asarray(prof)
        prof = np.maximum.accumulate(prof)
        if prof[0] > 0.1 or prof[-1] < 0.9:
            return None
        return float(np.interp(0.9, prof, centres) - np.interp(0.1, prof, centres))

    return {
        "unrest": float(np.sqrt(var / count)) if count else None,
        "width": width(maps.out),
        "width_truth": width(maps.truth),
    }


def tile_records(maps: Maps, tile: dict, colours: dict, inset=10):
    """The measured regions of one tile, each a dict."""
    x, y, w, h = tile["rect"]
    sl = (slice(y, y + h), slice(x, x + w))
    kind, params = tile["kind"], tile["params"]
    fg, bg = colours[tile["pair"][0]], colours[tile["pair"][1]]
    fc = false_colour(maps.lab_out[sl], mixture_locus(fg, bg))
    yy, xx = np.mgrid[0:h, 0:w]
    xx = xx + 0.5 - w / 2.0 - 0.37
    yy = yy + 0.5 - h / 2.0 - 0.21
    r = np.hypot(xx, yy)
    inner = np.zeros((h, w), dtype=bool)
    inner[inset : h - inset, inset : w - inset] = True
    regions = []
    if kind == "rings":
        regions.append(({"freq": 1.0 / params["period"]}, inner))
    elif kind == "dpr":
        disc = r < params["radius"] - 3
        diamond = (np.abs(xx) + np.abs(yy)) < params["radius"] * 0.30
        regions.append(({"part": "all"}, disc))
        regions.append(({"part": "coarse", "freq": 1.0 / params["period"]}, disc & ~diamond & (xx + yy < -3)))
        regions.append(({"part": "fine", "freq": 2.0 / params["period"]}, disc & ~diamond & (xx + yy > 3)))
        regions.append(
            ({"part": "finest", "freq": 4.0 / params["period"]}, (np.abs(xx) + np.abs(yy)) < params["radius"] * 0.30 - 3)
        )
    elif kind in ("zone", "siemens"):
        with np.errstate(divide="ignore"):
            freq = r / params["n"] if kind == "zone" else params["cycles"] / (2 * np.pi * r)
        area = inner if kind == "zone" else (r < params["radius"] - 4)
        regions.append(({"part": "all"}, area & (freq < 0.5)))
        for lo, hi in zip(FREQ_EDGES[:-1], FREQ_EDGES[1:]):
            regions.append(({"freq": float((lo + hi) / 2)}, area & (freq >= lo) & (freq < hi)))
    elif kind == "edge":
        a = np.deg2rad(params["angle_deg"])
        dist = xx * np.cos(a) + yy * np.sin(a)
        band = inner & (np.abs(dist) < 4.0)
        rec = region_stats(maps, sl, band, fc)
        rec.update(edge_stats(maps, sl, dist, band, fg, bg))
        return [{"kind": kind, "pair": "/".join(tile["pair"]), **params, **rec}]
    else:
        regions.append(({}, inner))
    out = []
    for extra, mask in regions:
        rec = region_stats(maps, sl, mask, fc)
        if rec is not None:
            out.append({"kind": kind, "pair": "/".join(tile["pair"]), **params, **extra, **rec})
    return out


def psnr(mse):
    return float("inf") if mse <= 0 else float(10 * np.log10(255.0**2 / mse))


def resolved_frequency(freqs, nrmse, threshold=NRMSE_RESOLVED):
    """The frequency at which the normalised error first passes `threshold`,
    read upwards. Below the lowest frequency measured: that frequency, with
    `False`; never passed: the highest, with `True` (a lower bound)."""
    order = np.argsort(freqs)
    f, e = np.asarray(freqs, float)[order], np.asarray(nrmse, float)[order]
    if e[0] > threshold:
        return float(f[0]), False
    for i in range(1, len(f)):
        if e[i] > threshold:
            t = (threshold - e[i - 1]) / (e[i] - e[i - 1])
            return float(f[i - 1] + t * (f[i] - f[i - 1])), True
    return float(f[-1]), True
