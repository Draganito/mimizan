"""Minimal DNG input and output for the colour-ring bench.

`write_cfa_dng` writes an uncompressed 16-bit Bayer DNG (one IFD, one strip)
with the tags `mimizan_core::kodak::write_dng` writes, which libraw,
RawTherapee, darktable and Mimizan's own decoder open.

`read_linear_dng` reads the linear DNG `mimizan negative --dng` writes (three
16-bit samples a pixel, `LinearRaw`), with its `AsShotNeutral`.
"""

from __future__ import annotations

import struct
from pathlib import Path

import numpy as np

T_BYTE, T_ASCII, T_SHORT, T_LONG, T_RATIONAL, T_SRATIONAL = 1, 2, 3, 4, 5, 10
TYPE_SIZE = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 6: 1, 7: 1, 8: 2, 9: 4, 10: 8, 11: 4, 12: 8}

# XYZ (D65) -> linear sRGB: the "camera" of a sensor whose channels are the
# sRGB primaries. With AsShotNeutral 1,1,1 a converter reproduces the scene.
XYZ_TO_SRGB = [3.2406, -1.5372, -0.4986, -0.9689, 1.8758, 0.0415, 0.0557, -0.2040, 1.0570]

CFA_CODES = {"R": 0, "G": 1, "B": 2}


def _ascii(tag, s):
    data = s.encode("ascii") + b"\0"
    return (tag, T_ASCII, len(data), data)


def _shorts(tag, v):
    return (tag, T_SHORT, len(v), struct.pack(f"<{len(v)}H", *v))


def _longs(tag, v):
    return (tag, T_LONG, len(v), struct.pack(f"<{len(v)}I", *v))


def _bytes(tag, v):
    return (tag, T_BYTE, len(v), bytes(v))


def _rationals(tag, v, den=1_000_000):
    data = b"".join(struct.pack("<II", int(round(x * den)), den) for x in v)
    return (tag, T_RATIONAL, len(v), data)


def _srationals(tag, v, den=10_000):
    data = b"".join(struct.pack("<ii", int(round(x * den)), den) for x in v)
    return (tag, T_SRATIONAL, len(v), data)


def write_cfa_dng(
    path,
    samples: np.ndarray,
    phase: str = "RGGB",
    model: str = "rings",
    neutral=(1.0, 1.0, 1.0),
    xyz_to_cam=XYZ_TO_SRGB,
    white_level: int = 65535,
):
    """`samples` is a (H, W) uint16 mosaic, `phase` the 2x2 tile row-major."""
    samples = np.ascontiguousarray(samples, dtype="<u2")
    height, width = samples.shape
    cfa = [CFA_CODES[c] for c in phase]
    entries = [
        _longs(254, [0]),
        _longs(256, [width]),
        _longs(257, [height]),
        _shorts(258, [16]),
        _shorts(259, [1]),
        _shorts(262, [32803]),
        _ascii(271, "Mimizan"),
        _ascii(272, model),
        _longs(273, [0]),  # StripOffsets, patched below
        _shorts(274, [1]),
        _shorts(277, [1]),
        _longs(278, [height]),
        _longs(279, [samples.nbytes]),
        _shorts(284, [1]),
        _ascii(305, "mimizan bayer-rings bench"),
        _shorts(33421, [2, 2]),
        _bytes(33422, cfa),
        _bytes(50706, [1, 4, 0, 0]),
        _bytes(50707, [1, 1, 0, 0]),
        _ascii(50708, f"Mimizan {model}"),
        _shorts(50711, [1]),
        _longs(50714, [0]),
        _longs(50717, [white_level]),
        _srationals(50721, xyz_to_cam),
        _rationals(50728, neutral),
        _shorts(50778, [21]),
    ]
    entries.sort(key=lambda e: e[0])
    ifd_len = 2 + 12 * len(entries) + 4
    extra = 8 + ifd_len
    offsets = []
    for _, _, _, data in entries:
        if len(data) > 4:
            offsets.append(extra)
            extra += len(data) + (len(data) & 1)
        else:
            offsets.append(None)
    strip_off = extra
    out = bytearray(b"II*\0" + struct.pack("<I", 8) + struct.pack("<H", len(entries)))
    for (tag, typ, count, data), off in zip(entries, offsets):
        if tag == 273:
            data = struct.pack("<I", strip_off)
        out += struct.pack("<HHI", tag, typ, count)
        out += struct.pack("<I", off) if off is not None else data.ljust(4, b"\0")
    out += struct.pack("<I", 0)
    for (_, _, _, data), off in zip(entries, offsets):
        if off is not None:
            out += data + (b"\0" if len(data) & 1 else b"")
    assert len(out) == strip_off
    Path(path).write_bytes(bytes(out) + samples.tobytes())


def _read_ifd(buf: bytes):
    if buf[:2] == b"II":
        e = "<"
    elif buf[:2] == b"MM":
        e = ">"
    else:
        raise ValueError("not a TIFF")
    (ifd,) = struct.unpack_from(e + "I", buf, 4)
    (n,) = struct.unpack_from(e + "H", buf, ifd)
    tags = {}
    for i in range(n):
        o = ifd + 2 + 12 * i
        tag, typ, count = struct.unpack_from(e + "HHI", buf, o)
        size = TYPE_SIZE[typ] * count
        at = o + 8 if size <= 4 else struct.unpack_from(e + "I", buf, o + 8)[0]
        raw = buf[at : at + size]
        if typ in (1, 6, 7):
            val = list(raw)
        elif typ == 2:
            val = raw.split(b"\0")[0].decode("ascii", "replace")
        elif typ == 3:
            val = list(struct.unpack(e + f"{count}H", raw))
        elif typ == 4:
            val = list(struct.unpack(e + f"{count}I", raw))
        elif typ == 5:
            v = struct.unpack(e + f"{2 * count}I", raw)
            val = [v[2 * k] / v[2 * k + 1] for k in range(count)]
        elif typ == 10:
            v = struct.unpack(e + f"{2 * count}i", raw)
            val = [v[2 * k] / v[2 * k + 1] for k in range(count)]
        else:
            val = raw
        tags[tag] = val
    return e, tags


def read_linear_dng(path):
    """The three channels of a Mimizan linear DNG as float (H, W, 3) in 0..1,
    as stored (white balance undone), and the as-shot neutral."""
    buf = Path(path).read_bytes()
    e, t = _read_ifd(buf)
    width, height = t[256][0], t[257][0]
    if t[262][0] != 34892 or t[277][0] != 3 or t[258][0] != 16 or t[259][0] != 1:
        raise ValueError(f"{path}: not an uncompressed 16-bit LinearRaw DNG")
    data = bytearray()
    for off, n in zip(t[273], t[279]):
        data += buf[off : off + n]
    img = np.frombuffer(bytes(data), dtype=e + "u2").reshape(height, width, 3)
    white = np.asarray(t.get(50717, [65535] * 3), dtype=np.float64)
    black = np.asarray(t.get(50714, [0] * 3), dtype=np.float64)
    neutral = np.asarray(t.get(50728, [1.0, 1.0, 1.0]), dtype=np.float64)
    return (img.astype(np.float64) - black) / (white - black), neutral


def colour_matrix(path):
    """`ColorMatrix1` of a DNG: XYZ -> camera, 3x3, or `None`."""
    _, t = _read_ifd(Path(path).read_bytes())
    m = t.get(50721)
    return np.asarray(m, dtype=np.float64).reshape(3, 3) if m is not None and len(m) == 9 else None
