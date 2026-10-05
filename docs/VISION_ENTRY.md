# Proposed entry for `PlatformIO/VISION.md`

Not applied automatically: `VISION.md` is the cross-project document and its
wording is Dragan's. Two pieces, ready to paste.

## 1. Table row for "Projects: local folder ↔ GitHub repo"

```
| `mimizan` | [mimizan](https://github.com/Draganito/mimizan) | GPL-3.0-or-later | v0.4.0 (Debian `.deb` + Linux tarball, CI) |
```

## 2. Paragraph under "Line 2: Splitgrade darkroom" (or a third line, see note)

```
### Mimizan Lab — the digital negative for the darkroom line

A colour sensor records luminance and chrominance in one Bayer mosaic.
Mimizan Lab (`mimizan/`, Rust, GPL-3.0-or-later) estimates luminance directly from the
mosaic (frequency separation, optional chroma/texture mask), writes a
16-bit linear negative with a mask sidecar, and renders it to print size
with a look curve and viewing-distance USM. Everything is `f64`,
bit-deterministic across runs and thread counts, measured on synthetic
scenes with known ground truth and against libraw/RawTherapee demosaicers
on the Kodak set (`docs/RESULTS.md`, `docs/KODAK.md`) against a frozen
specification (`docs/SPEC.md`). Status: experimental; the estimator
trails state-of-the-art demosaicers by 4–6 dB luminance PSNR on Kodak, the
benchmark is the acceptance test for the next estimator.

Shipped as `mimizan` (CLI: info, negative, print, calibrate, synth,
measure) and `mimizan-gui` (egui desktop app: live preview from a
downscaled separation, exports in a background thread). Test camera Nikon
Z f; a full-frame monochrome camera (sensor without colour filter array)
serves as the reference and passes through without separation. 24 MP →
negative in 2.1 s on the Ryzen 7 5700U. Camera and look calibration are
fitted from charts (`calibrate wedge|weights|star`): the shipped reference
look and the Z f mix weights are measured against that camera on a Kodak
grey scale and a ColorChecker in the same scene.
```

Note on placement: Mimizan Lab belongs to the darkroom line in intent (a
negative that prints like film), but it is software for the digital camera,
not for the enlarger. If that feels wrong under "Line 2", a short "Line 3:
digital negative" heading with the same paragraph is the alternative.

## 3. Tooling table row ("How this was built")

```
| `mimizan` | Rust (cargo workspace: core, cli, gui) | rust-analyzer; `cargo test --release`, `cargo bench -p mimizan-core` |
```
