"""Side-by-side crops, each labelled with its developer and method."""

from __future__ import annotations

from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

from metrics import srgb_encode

BG = (24, 24, 24)
LABEL_BG = {"truth": (20, 84, 42), "mimizan": (150, 78, 0), "other": (52, 52, 52)}
FONT_PATHS = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/Library/Fonts/Arial.ttf",
    "C:/Windows/Fonts/arial.ttf",
]


def font(size: int):
    for p in FONT_PATHS:
        if Path(p).exists():
            return ImageFont.truetype(p, size)
    return ImageFont.load_default()


def to_srgb8(rgb_linear) -> np.ndarray:
    return (srgb_encode(rgb_linear) * 255.0 + 0.5).astype(np.uint8)


def grid(panels, cols: int, zoom: int, title: str = "", subtitle: str = "", font_size: int = 15) -> Image.Image:
    """`panels` is a list of (label, sublabel, kind, uint8 RGB crop); `kind`
    is truth, mimizan or other and colours the label. Crops are enlarged
    `zoom` times, nearest neighbour, so a pixel stays a pixel."""
    f, small, big = font(font_size), font(max(font_size - 3, 9)), font(font_size + 5)
    h, w = panels[0][3].shape[:2]
    pw, ph = w * zoom, h * zoom
    label_h = font_size + 8 + (font_size + 1 if any(p[1] for p in panels) else 0)
    gap = 8
    rows = (len(panels) + cols - 1) // cols
    head = (font_size + 22 if title else 0) + (font_size + 6 if subtitle else 0)
    width = cols * pw + (cols + 1) * gap
    height = head + rows * (ph + label_h + gap) + gap
    img = Image.new("RGB", (width, height), BG)
    d = ImageDraw.Draw(img)
    if title:
        d.text((gap, 6), title, fill=(240, 240, 240), font=big)
    if subtitle:
        d.text((gap, font_size + 20), subtitle, fill=(190, 190, 190), font=small)
    for i, (label, sub, kind, crop) in enumerate(panels):
        x = gap + (i % cols) * (pw + gap)
        y = head + gap + (i // cols) * (ph + label_h + gap)
        d.rectangle([x, y, x + pw - 1, y + label_h - 1], fill=LABEL_BG.get(kind, LABEL_BG["other"]))
        d.text((x + 5, y + 3), label, fill=(255, 255, 255), font=f)
        if sub:
            d.text((x + 5, y + font_size + 6), sub, fill=(225, 225, 225), font=small)
        tile = Image.fromarray(crop).resize((pw, ph), Image.NEAREST)
        img.paste(tile, (x, y + label_h))
    return img


def ladder(rows, zoom: int, title: str = "", subtitle: str = "", columns=None, cell: int = 0, font_size: int = 15) -> Image.Image:
    """One strip a method, the label at its left. `rows` is a list of
    (label, sublabel, kind, uint8 RGB strip); `columns` names the cells of
    `cell` px a strip is made of."""
    f, small, big = font(font_size), font(max(font_size - 3, 9)), font(font_size + 5)
    h, w = rows[0][3].shape[:2]
    pw, ph = w * zoom, h * zoom
    probe = ImageDraw.Draw(Image.new("RGB", (8, 8)))
    label_w = int(max(max(probe.textlength(r[0], font=f), probe.textlength(r[1] or "", font=small)) for r in rows)) + 14
    gap = 4
    head = (font_size + 22 if title else 0) + (font_size + 6 if subtitle else 0) + (font_size + 6 if columns else 0)
    width = label_w + pw + 3 * gap
    height = head + len(rows) * (ph + gap) + 2 * gap
    img = Image.new("RGB", (width, height), BG)
    d = ImageDraw.Draw(img)
    y = 6
    if title:
        d.text((gap, y), title, fill=(240, 240, 240), font=big)
        y += font_size + 16
    if subtitle:
        d.text((gap, y), subtitle, fill=(190, 190, 190), font=small)
        y += font_size + 6
    if columns:
        for i, c in enumerate(columns):
            d.text((label_w + 2 * gap + i * cell * zoom + 3, y), c, fill=(210, 210, 210), font=small)
    for i, (label, sub, kind, strip) in enumerate(rows):
        y = head + gap + i * (ph + gap)
        d.rectangle([gap, y, gap + label_w - 1, y + ph - 1], fill=LABEL_BG.get(kind, LABEL_BG["other"]))
        d.text((gap + 6, y + 3), label, fill=(255, 255, 255), font=f)
        if sub:
            d.text((gap + 6, y + font_size + 6), sub, fill=(225, 225, 225), font=small)
        img.paste(Image.fromarray(strip).resize((pw, ph), Image.NEAREST), (label_w + 2 * gap, y))
    return img
