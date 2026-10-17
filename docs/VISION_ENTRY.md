# Proposed entry for `PlatformIO/VISION.md`

Not applied automatically: `VISION.md` is the cross-project document and its
wording is Dragan's. Two pieces, ready to paste.

## 1. Table row for "Projects: local folder ↔ GitHub repo"

```
| `mimizan` | [mimizan](https://github.com/Draganito/mimizan) | GPL-3.0-or-later | v0.11.5 (Debian `.deb` + Linux tarball + Windows x64 zip + Apple silicon app, CI). The demosaic comparison stays in v0.10.12 and on branch `demosaic` |
```

## 2. Paragraph under "Line 2: Splitgrade darkroom" (or a third line, see note)

```
### Mimizan Lab — the digital negative for the darkroom line

A colour sensor records luminance and chrominance in one Bayer mosaic.
Mimizan Lab (`mimizan/`, Rust, GPL-3.0-or-later) estimates luminance directly from the
mosaic (Dubois frequency separation by default), writes a 16-bit linear
negative with a mask sidecar, and prints it through placing and a
split-grade paper: a soft exposure (filter 00) and a hard one (filter 5),
then paper size and border. The desktop leaves sharpening off; the command
line and the phone can add USM or Richardson–Lucy deconvolution of an
enlargement. Everything is `f64`, bit-deterministic across runs and thread
counts, measured on synthetic scenes with known ground truth and against
libraw/RawTherapee demosaicers on the Kodak set (`docs/RESULTS.md`,
`docs/KODAK.md`) against a frozen specification (`docs/SPEC.md`). On
band-limited photographs the linear default leads that field; on lens-like
photographs it sits in the leading group; on pixel-sharp content it trails.
With reconstruct the mean leads both photographic conditions (DETAILED.md,
"Where it stands").

Shipped as `mimizan` (CLI: info, dump, negative, print, bench, calibrate,
synth, measure) and `mimizan-gui` (egui desktop app: live preview from a
downscaled separation, exports in a background thread). Test camera Nikon
Z f; a full-frame monochrome camera (sensor without colour filter array)
serves as the reference and passes through without separation. On the
Ryzen 7 5700U a 24.4 MP negative separates in 2.3 s, and in 4.5 s with
reconstruct, which the desktop starts with. Camera and look calibration are
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
| `mimizan` | Rust (cargo workspace: core, cli, gui). The demosaic comparison stays on branch `demosaic` and in release 0.10.12 | rust-analyzer; `cargo test --release`, `cargo bench -p mimizan-core` |
```
