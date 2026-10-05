# RESULTS

Measured numbers only. Every table names the command that produced it.
Nothing here is an estimate. Machine: AMD Ryzen 7 5700U, 16 threads, f64.

Definitions are in `SPEC.md` section 8. `σ` is the mean model noise of the
truth image; `Interior RMSE/σ` is measured 16 px inside flat patches, so an
estimator that neither denoises nor amplifies noise scores 1.00.
`bilinear-demosaic` is the "standard way" (bilinear interpolation, then
(R+2G+B)/4) and is here for comparison only.

## Phase 2 — fixed estimator (`--mask off`), synthetic bench

Command: `mimizan measure synth --mask off --size 1024`
(phase RGGB, raw gains 0.5/1.0/0.7, noise a=0.005 b=0.0002, seed 1)

| Scene | Size | Estimator | RMSE | RMSE/σ | p99 | Interior RMSE/σ | MTF50 axis | MTF50 diag | MTF10 axis | Ring | Carrier max | Wedge slope | Wedge resid | ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Star | 1024x1024 | truth (pixel aperture) | 0 | 0 | 0 | - | 0.275 | 0.265 | 0.451 | false | - | - | - | - |
| Star | 1024x1024 | mimizan | 0.02471 | 7.83 | 0.1002 | - | 0.281 | 0.273 | 0.421 | false | - | - | - | 132 |
| Star | 1024x1024 | bilinear-demosaic | 0.03641 | 11.54 | 0.1612 | - | 0.207 | 0.201 | 0.373 | false | - | - | - | 7 |
| Zoneplate | 1024x1024 | mimizan | 0.06986 | 21.34 | 0.2328 | - | - | - | - | - | - | - | - | 110 |
| Zoneplate | 1024x1024 | bilinear-demosaic | 0.10699 | 32.68 | 0.2365 | - | - | - | - | - | - | - | - | 4 |
| Edge | 1024x1024 | mimizan | 0.00403 | 1.26 | 0.0105 | - | - | - | - | - | - | - | - | 110 |
| Edge | 1024x1024 | bilinear-demosaic | 0.00351 | 1.10 | 0.0055 | - | - | - | - | - | - | - | - | 4 |
| Patches | 1024x1024 | mimizan | 0.00911 | 3.85 | 0.0399 | 0.98 | - | - | - | - | 0.876 | - | - | 112 |
| Patches | 1024x1024 | bilinear-demosaic | 0.00537 | 2.27 | 0.0198 | 0.52 | - | - | - | - | 0.220 | - | - | 4 |
| Wedge | 1024x1024 | mimizan | 0.00291 | 1.44 | 0.0101 | 1.04 | - | - | - | - | 0.659 | 1.0000 | 0.01 % | 99 |
| Wedge | 1024x1024 | bilinear-demosaic | 0.00258 | 1.27 | 0.0083 | 0.57 | - | - | - | - | 0.166 | 1.0000 | 0.02 % | 4 |
| Fabric | 1024x1024 | mimizan | 0.00281 | 0.92 | 0.0082 | - | - | - | - | - | - | - | - | 100 |
| Fabric | 1024x1024 | bilinear-demosaic | 0.00797 | 2.62 | 0.0217 | - | - | - | - | - | - | - | - | 5 |
| Alias | 1024x1024 | mimizan | 0.10938 | 35.99 | 0.2421 | - | - | - | - | - | - | - | - | 102 |
| Alias | 1024x1024 | bilinear-demosaic | 0.04016 | 13.21 | 0.1045 | - | - | - | - | - | - | - | - | 4 |

Reading:

- Star: the fixed estimator reaches the pixel-aperture limit at MTF50
  (0.281 vs truth 0.275 c/px; bilinear 0.207 = 75 %). MTF10 93 % of truth
  (bilinear 83 %).
- Flat patches and wedge: interior noise 0.98–1.04 σ, i.e. the negative
  carries exactly the sensor noise, neither denoised nor amplified. Bilinear
  shows 0.52–0.57 σ because it low-passes. Carrier energy on flat colour
  fields 0.88× noise (limit 1.5). Wedge slope 1.0000, residual 0.01 %.
- Fabric (colour texture at 0.12 c/px, inside the chroma band): 0.92 σ vs
  2.62 σ for bilinear.
- Alias (colour stripes at 0.30 c/px, beyond the chroma Nyquist): both fail,
  the fixed estimator fails louder (36 σ vs 13 σ) because it keeps the aliased
  chroma as sharp false luminance while bilinear blurs it. This is the
  "Nur Rot oder Blau: über 0,25 leer" row of the whitepaper, measured.
- The whole-image RMSE on Star/Zoneplate is dominated by content above the
  aperture limit and is not a quality number; MTF50/MTF10 are.

## Phase 3 — channel balance, adaptive mask, consistency loop

Note: the Phase 2 table above was measured *without* channel balance (raw
gains stayed in the mosaic). From Phase 3 on, the synthetic mosaic is
balanced with the exact inverse gains and the truth is the luminance of the
balanced scene, so the two tables are not directly comparable row by row.
The fixed estimator was therefore re-measured (first table below).

### Fixed estimator with channel balance

Command: `mimizan measure synth --mask off --size 1024`

| Scene | Size | Estimator | RMSE | RMSE/σ | p99 | Interior RMSE/σ | MTF50 axis | MTF50 diag | MTF10 axis | Ring | Carrier max | Wedge slope | Wedge resid | ms (rounds) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Star | 1024x1024 | truth (pixel aperture) | 0 | 0 | 0 | - | 0.275 | 0.265 | 0.451 | false | - | - | - | - |
| Star | 1024x1024 | mimizan | 0.02016 | 5.75 | 0.0843 | - | 0.274 | 0.265 | 0.405 | false | - | - | - | 130 (1) |
| Star | 1024x1024 | bilinear-demosaic | 0.04649 | 13.27 | 0.1896 | - | 0.203 | 0.199 | 0.365 | false | - | - | - | 4 (1) |
| Zoneplate | 1024x1024 | mimizan | 0.06814 | 18.73 | 0.2858 | - | - | - | - | - | - | - | - | 113 (1) |
| Zoneplate | 1024x1024 | bilinear-demosaic | 0.13531 | 37.20 | 0.2698 | - | - | - | - | - | - | - | - | 4 (1) |
| Edge | 1024x1024 | mimizan | 0.00459 | 1.29 | 0.0124 | - | - | - | - | - | - | - | - | 108 (1) |
| Edge | 1024x1024 | bilinear-demosaic | 0.00447 | 1.26 | 0.0063 | - | - | - | - | - | - | - | - | 4 (1) |
| Patches | 1024x1024 | mimizan | 0.01204 | 4.52 | 0.0550 | 1.14 | - | - | - | - | 0.804 | - | - | 113 (1) |
| Patches | 1024x1024 | bilinear-demosaic | 0.00752 | 2.82 | 0.0281 | 0.56 | - | - | - | - | 0.203 | - | - | 4 (1) |
| Wedge | 1024x1024 | mimizan | 0.00335 | 1.49 | 0.0119 | 1.20 | - | - | - | - | 0.557 | 1.0000 | 0.02 % | 100 (1) |
| Wedge | 1024x1024 | bilinear-demosaic | 0.00331 | 1.48 | 0.0120 | 0.61 | - | - | - | - | 0.139 | 1.0000 | 0.02 % | 4 (1) |
| Fabric | 1024x1024 | mimizan | 0.00353 | 1.05 | 0.0098 | - | - | - | - | - | - | - | - | 103 (1) |
| Fabric | 1024x1024 | bilinear-demosaic | 0.00835 | 2.49 | 0.0220 | - | - | - | - | - | - | - | - | 4 (1) |
| Alias | 1024x1024 | mimizan | 0.13962 | 41.52 | 0.3458 | - | - | - | - | - | - | - | - | 101 (1) |
| Alias | 1024x1024 | bilinear-demosaic | 0.04096 | 12.18 | 0.1045 | - | - | - | - | - | - | - | - | 4 (1) |

Interior noise is now 1.14–1.20 σ instead of 0.98–1.04: σ is still the raw
model noise, but the balanced red channel carries 2× the noise amplitude
(k_R = 2) and the mosaic noise is accordingly higher. The estimator itself
is unchanged (it adds nothing).

### Adaptive block mask (default until the Dubois mode, see the Kodak section)

Command: `mimizan measure synth --mask adaptive --size 1024`

| Scene | Size | Estimator | RMSE | RMSE/σ | p99 | Interior RMSE/σ | MTF50 axis | MTF50 diag | MTF10 axis | Ring | Carrier max | Wedge slope | Wedge resid | ms (rounds) |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Star | 1024x1024 | truth (pixel aperture) | 0 | 0 | 0 | - | 0.275 | 0.265 | 0.451 | false | - | - | - | - |
| Star | 1024x1024 | mimizan | 0.01559 | 4.45 | 0.0620 | - | 0.275 | 0.265 | 0.451 | false | - | - | - | 295 (1) |
| Zoneplate | 1024x1024 | mimizan | 0.06827 | 18.77 | 0.2859 | - | - | - | - | - | - | - | - | 239 (1) |
| Edge | 1024x1024 | mimizan | 0.00459 | 1.29 | 0.0124 | - | - | - | - | - | - | - | - | 217 (1) |
| Patches | 1024x1024 | mimizan | 0.01205 | 4.52 | 0.0550 | 1.14 | - | - | - | - | 0.805 | - | - | 216 (1) |
| Wedge | 1024x1024 | mimizan | 0.00335 | 1.50 | 0.0119 | 1.20 | - | - | - | - | 0.558 | 1.0000 | 0.02 % | 213 (1) |
| Fabric | 1024x1024 | mimizan | 0.00353 | 1.05 | 0.0099 | - | - | - | - | - | - | - | - | 233 (1) |
| Alias | 1024x1024 | mimizan | 0.15387 | 45.76 | 0.4313 | - | - | - | - | - | - | - | - | 248 (1) |

Reading:

- Star: MTF50 and MTF10 are now exactly the pixel-aperture truth (0.275 /
  0.265 / 0.451 c/px); the fixed estimator stops at MTF10 0.405 because its
  notch removes luminance within 0.15 c/px of the carriers. RMSE 0.0156 vs
  0.0202 fixed. The mask image is 1 over the whole star and 0 outside.
- Edge, Patches, Wedge, Fabric: identical to the fixed estimator to the last
  digit. The mask is 0 everywhere there (no luminance evidence in the ring,
  or band energy that the ring cannot explain); flat colour fields keep
  their 0.80× carrier energy (limit 1.5).
- Alias: still a fail scene (46 σ vs 42 σ fixed); colour stripes at 0.30
  c/px are beyond the chroma Nyquist for both estimators.
- Cost: 2.1–2.3× the fixed time at 1024² (block spectra + core split).
  Phase 5 item.

Mask variants measured on the way (same command, Patches carrier max /
Star MTF10), all rejected:

| Variant | Patches carrier max | Star MTF10 | Why rejected |
|---|---|---|---|
| Whitepaper `E_L/(E_L+E_C)`, 0.5 below 2σ, no core split, raw gains | 2.418 | 0.442 | colour-edge zipper; mask fires on noise |
| + core split, M = 0 below threshold | 2.316 | 0.442 | same, the "0.5" was not the cause |
| Wiener form, i.i.d. χ² thresholds, raw gains | 1.064 | 0.448 | fires on noise in flat fields (ring std 0.10, not 0.035) |
| + channel balance | 96.0 | 0.450 | M > 0 blocks next to colour edges, interpolated into the edge |
| + hard cap `√(n_C·N_bin/E_C)` | 509 | 0.401 | cap kills legitimate luminance in the band (Star) and did not fix the interpolation |
| + measured fluctuation (16/8), cap on unexplained energy, 3×3 erosion | **0.805** | **0.451** | adopted (SPEC 5.2) |

### Consistency loop

`--rounds 12` vs `--rounds 1` on all scenes: no figure changed beyond the
fourth digit (Star RMSE 0.02740 vs 0.02737, Patches 0.01003 vs 0.01004),
run time 1.5–2.2×. Default is now 1 round; the option stays.

## Phase 4 — print

Unit checks (core tests): Lanczos-3 keeps a flat field to 1e-12 at any
scale, identity is bit-exact, a 2-px checkerboard shrunk by 2 collapses to
its mean (the stretched kernel is the pre-filter), a low-frequency ramp
survives a 4× shrink to < 1e-3. PCHIP reproduces the control points, is
monotone between them, and the `neutral` look is exactly `x^(1/2.2)`. USM
with amount 0 or a zero mask is the identity; saturated mask pixels (1.0)
and mask < 0.8 are not sharpened.

`mimizan print out/zf_v2.tif --size 30x40cm --dpi 300 --usm-amount 0.5
--proof`: 6048×4032 → 4724×3149 (box rotated to landscape), USM radius
1.66 px at the diagonal (481 mm), masked by the sidecar; render 873 ms,
write 412 ms (TIFF 29.8 MB + JPEG), total 1.3 s. Without `--size` and
with `usm_amount 0` from `cameras/nikon_z_f.json`: 0.9 s total.

### Screen compensation (`--screen`)

Known losses for a 1:1 screen view, in cycles per output pixel:

| f (c/px) | Lanczos-3 L(f) | aperture A(f) | M = L·A | 9-tap H(f) | H·M |
|---|---|---|---|---|---|
| 0.10 | 1.001 | 0.984 | 0.985 | 0.990 | 0.976 |
| 0.20 | 1.013 | 0.935 | 0.947 | 1.059 | 1.003 |
| 0.25 | 1.012 | 0.900 | 0.911 | 1.082 | 0.985 |
| 0.30 | 0.990 | 0.858 | 0.849 | 1.114 | 0.946 |
| 0.40 | 0.823 | 0.757 | 0.623 | 1.501 | 0.935 |
| 0.50 | 0.501 | 0.637 | 0.319 | 1.911 | 0.610 |

(Lanczos-3 has the familiar ~1 % passband overshoot up to 0.25 c/px; the
aperture dominates there, the resampler above 0.3.)

Taps (downscale, amount 1, gain cap 2): 0.0224, −0.0507, 0.0934, −0.1771,
**1.2240**, −0.1771, 0.0934, −0.0507, 0.0224 (sum 1). Without a downscale
(aperture only) the centre tap is 1.1350. A Gaussian USM fitted to the same
target degenerates to σ < 0.3 px and cannot follow the rise, which is why
this is a designed FIR and not a USM preset. The product `H·M` is within
−5.4 %/+0.3 % of flat up to 0.3 c/px and never above 1.003, i.e. the
compensation does not exceed the measured loss anywhere; the remaining
roll-off above 0.4 c/px is the price of the 2× gain cap (noise).

`mimizan print out/zf_cal.tif --look look/reference.json --size 2048px
--screen 1.0 -o out/zf_screen_2048.jpg`: 6048×4032 → 2048×1365, masked by
the sidecar, render 794 ms (386 ms without compensation), JPEG 1.40 MB
(1.34 MB plain). Visually: text and the star target are crisper, no halos
at the chart edges.

### Calibration self-tests (no charts shot yet)

`calibrate wedge --self-test` (512 px wedge, noise on, separation as in
production, reference = ground truth through a gamma-2.2 S-curve): fit error
0.00/255 on the 16 steps, 0.09/255 half-way between the steps (hold-out),
linearity slope 1.0001, max residual 0.17 % — PASS (limit 3/255).

`calibrate weights --self-test` (384 px patches, noise on, reference =
`w_true · rgb` with an arbitrary exposure factor): truth (0.30, 0.50, 0.20)
→ fitted (0.300, 0.500, 0.200), RMSE 0.05 %; truth (0.60, 0.30, 0.10) at
512 px → exact, RMSE 0.03 % — PASS (limit 5 %, deviation ≤ 0.01).

Plumbing on files: `calibrate wedge out/zf_v2.tif --reference
out/zf_print_full.jpg --grid ...` over the Kodak grey scale of the Z f chart
reproduces the proof's own curve to 0.00/255 on the fields (20 fields, 22
points). A star on the chart needs the true centre and spoke count; the
measurement is manual by design (`--center`, `--radius`, `--spokes`).

### Reference look (real data)

The reference camera is a full-frame monochrome camera (24 MP sensor
without colour filter array, 12-bit LinearRaw DNG); it is called "the
reference" below. `look/reference.json` is fitted from the reference itself:
`MonoRef.dng` (passed through `negative` without separation) against the
out-of-camera JPEG of the same frame (`MonoRef_sooc.jpg`, 5952×3968 = the
DNG crop minus 12 px per side, offset −4 px against our crop origin).
Kodak grey scale, 20 fields (`--grid 2260,322,1382,113,20,1`, reference
`--ref-grid 2256,318,1378,113,20,1`), encoding gamma 2.2:

| field | negative (linear) | encoded x | JPEG y |
|------:|------------------:|----------:|-------:|
| A     | 0.382 | 0.646 | 0.815 |
| 2     | 0.244 | 0.527 | 0.704 |
| 10    | 0.044 | 0.242 | 0.221 |
| 15    | 0.021 | 0.173 | 0.123 |
| 19    | 0.016 | 0.151 | 0.099 |

All 20 pairs monotone (nothing merged or dropped), fit 0.00/255 on the
fields. The curve lifts the light end (encoded 0.65 → 0.82, about +0.7 EV
in the mids) and steepens the shadows (slope ≈ 0.6 below field 15), i.e.
the reference's out-of-camera rendering is brighter and more contrasty than
gamma 2.2; the roll-off above field A (0.65 → 1.0) is interpolated, not
measured. Check: `print out/ref_neg.tif --look look/reference.json`
reproduces the reference JPEG visually across the chart (1:1 crops,
`measure crop --encoded`). The same look on the Z f negative gives the
reference tonality on the Z f frame; remaining differences are the
different exposure of the two frames, not the curve.

### Z f mix weights against the reference (real data)

Both RAWs show the same scene under the same light, ColorChecker Classic
included. Reference values = the 24 patch means of the reference negative
(linear, from `MonoRef.dng`; grid `2416,533,1068,712,6,4`, inset 0.25),
fed as `--ref-values`; the Z f patches via `--grid 2442,536,1106,740,6,4`,
as-shot balance. The free scale `s` absorbs the exposure difference of the
two frames (fitted s = 1.109).

Result: **w = (0.171, 0.489, 0.340)** on the balanced channels — raw-channel
form (0.284, 0.404, 0.312), which is what the camera file stores since the
second pair below — RMSE 2.2 %, max 6.5 % (blue flower),
grey row residuals within −1.5 %, PASS (limit 5 %). Written to
`cameras/nikon_z_f.json` (`fitted.weights = true`). Compared with the native
mix (0.25, 0.5, 0.25) the reference weighs blue more and red less; orange
and purplish blue come out equally bright on the reference (0.1032 vs
0.1032 linear), and the fitted mix reproduces that. The weights are valid
for this light spectrum; a second session under different light would show
how stable they are.

Visual check: `negative` with the camera file + `print --look
look/reference.json` on the Z f frame puts every ColorChecker patch at the
tonality of the reference JPEG (1:1 crops); the whole frame is ~11 %
darker, which is the exposure difference the scale factor measured.

### Second pair, low light (real data, not written to the camera file)

Same studio scene under the low-light setup: Z f ISO 100, 3 s f/5.6
(2023-12-12, `testdata/NikonZF_2.nef`, WB as-shot R 1.26 B 2.05); reference
at base ISO, 1 s f/8 (2016-11-30, `testdata/MonoRef_2.dng`). First pair for
comparison: Z f 1/40 s, reference 1/125 s f/4.8, WB R 2.02 B 1.11.
Identical framing, so the grids of the first pair apply unchanged.

**Grey scale (Kodak, 20 fields).** Reference against Z f: first pair
`m = 1.115·z + 0.0004` (no pedestal); second pair `m = 0.603·z + 0.0054`,
i.e. the reference frame carries a pedestal of 3.8 % of field A (~21 ADU
at 12 bit) that the Z f frame does not have. The BlackLevel tag of the DNG
is 0 in both frames; the pedestal only appears in the 1 s exposure, so dark
current / black clipping of the long exposure is the likely cause, not
flare. Consequence: long-exposure reference frames need a dark frame, or
an offset term in the fit.

**Weights.** With the pedestal subtracted:
- WB space (as the camera file used to store them): best fit (0.297,
  0.550, 0.153), RMSE 5.4 %, FAIL; the first-pair weights (0.171, 0.489,
  0.340) give 15.8 % here. The WB-space weights swing with the illuminant
  because the as-shot multipliers (R 2.02→1.26, B 1.11→2.05) are folded
  into them.
- Raw-channel space (`--wb none`): first pair (0.284, 0.404, 0.312) RMSE
  2.2 %, second pair (0.302, 0.444, 0.254) RMSE 5.4 %. Converting raw-space
  weights to WB space is exact: `w_bal,i ∝ w_raw,i / wb_i`, and reproduces
  the first-pair WB fit to the digit. Raw space is the physically right
  frame for a mono-sensor emulation (a monochrome sensor does not
  white-balance) and is far more stable. **Implemented** (SPEC §6, §10):
  the camera file now carries `weights_space`, `calibrate weights --write`
  stores the raw-space form, the pipeline converts per image with the
  as-shot multipliers, and the negative description records both forms.
  `cameras/nikon_z_f.json` holds (0.284, 0.404, 0.312) raw; the first-pair
  negative is bit-identical to the one made with the balanced triple
  (`measure diff identical`). Cross-check on the second pair: raw-space
  weights 8.3 % RMSE, native 9.9 %, the old balanced-space triple 15.8 %.
- The remaining 5 % of the second pair is not the pedestal: single
  patches (blue sky +13 %, purplish blue +15 %, moderate red −18 %) point
  to a spectral difference between the two sessions (seven years apart:
  lamps and/or chart replaced), so the second pair is a cross-check, not a
  clean calibration target.

**Star MTF (`calibrate star --spokes 45`, 45 cycles, centres refined to
the 45th harmonic).** Z f negative: MTF50 0.221 cy/px (axis) / 0.214
(diag), MTF10 0.395 / 0.396, no carrier ring. Reference: MTF50 0.238 /
0.231, MTF10 0.449 / 0.450. Pixel pitch is 5.94 vs 6.0 µm, so cy/px
compare directly: the Z f through Mimizan Lab reaches 93 % of the reference
at MTF50 and 88 % at MTF10 on this setup (system MTF including the lenses:
an 85 mm at f/5.6 on the Z f, a 50 mm at f/8 on the reference). Curves are
smooth down to 0.37 cy/px; no ringing, no alias flags.

Still open: the noise model (`fitted.noise = false`, needs a dark frame);
a clean second colour session under a known second illuminant.

## Phase 5 — performance, determinism, property tests

Reference: Nikon Z f, 24.4 MP, adaptive default, 16 threads, release build.

| stage                | before (ms) | after (ms) |
|----------------------|------------:|-----------:|
| decode (rawler)      |         470 |        448 |
| ingest               |         607 |        393 |
| chroma estimates     |        2603 |        554 |
| mask grids (FFT)     |         378 |        166 |
| core + blend + Dubois|        1426 |        363 |
| luminance + mix      |         148 |        133 |
| write TIFF + sidecar |         138 |        124 |
| **total**            |    **6120** |  **2104–2216** |

Budget ≤ 3 s met; 45 MP extrapolates linearly to ≈ 4 s (budget 6 s).

What changed, in order of effect:

1. Register-blocked convolution (16 lanes, taps summed in index order per
   output, column pass in 32×512 bands): 2603 → 1532 ms, output bit-identical
   to the previous code (`measure diff` on the full Z f negative:
   `identical: true`).
2. Memory-lean adaptive path: mask kept as block grids and sampled bilinearly
   at mix time, half-resolution chroma core, blend and Dubois combine in
   place, `mix_into` consuming `L̂`, mosaic dropped before mixing. Full f64
   planes alive at the peak: ~20 → 6. Total 6.1 → 3.8 s; this step also
   removed most of the page-fault time that showed up in the separate stage.
3. Fused demodulation: carriers applied while filling the padded row (±1) and
   as a per-row sign in the column pass; the `sx·m` row pass is shared by Ĉ1
   and Ĉ2a. 2 row + 3 column passes instead of 3 modulations + 6 passes:
   1532 → 554 ms, bit-identical to `modulate + lowpass_reference`
   (property test `fused_demodulation_matches_reference`).
4. Ingest: median of the 8 neighbours only for pixels outside the neighbour
   range (same result): 615 → 391 ms.
5. mimalloc as global allocator: glibc returned every 200 MB plane to the
   kernel at once, so each new plane paid its page faults again.
6. `-C target-cpu=x86-64-v3` gave ~10 % only (memory/latency bound); not
   adopted, the binary stays portable.

Bug fix found by the property tests: `adaptive_equals_fixed_on_flat_colour`
(adaptive must equal the fixed estimator to 1e-12 where the mask is 0)
failed because of the `+10⁻⁶` regulariser in the Dubois combine and the
`c + g·(w − c)` blend form. Combine is now the weighted mean with a plain
mean when both ρ are 0; blend is `g·w + (1−g)·c`. On the Z f negative this
changes 916 863 px (max 8211 steps, RMS 181 steps of 65535): all in textured
neutral regions where both C2 masks are 1. There the old code dropped the
chroma core entirely (`0/10⁻⁶`) and left a visible 2-px grid on the tinted
engraving; the new output is clean (`measure crop` 1:1 comparisons). Bench
effect: Star RMSE 0.01559 → 0.01605, MTF10 0.451 → 0.450, Alias 0.1539 →
0.1396, Patches carrier 0.805 → 0.804, everything else unchanged.

Determinism: two consecutive runs are bit-identical; the separation in a
3-thread rayon pool equals the 16-thread result bit for bit
(`separation_is_deterministic_across_thread_counts`). Reductions (gray
world, loop convergence) use per-row partials added in row order.

Property tests (`tests/properties.rs`, proptest, 24 cases each, all four CFA
phases): flat field exact, adaptive = fixed on flat colour (1e-12), neutral
gradient passes, fused demodulation = reference, mask values bounded in
[0, 1] and monotone in texture. Core: 47 unit tests + 6 property tests.

Criterion (`cargo bench -p mimizan-core`, mean):

| bench                              |     time |
|------------------------------------|---------:|
| filter/rows_73 1024²               |  5.15 ms |
| filter/cols_73 1024²               |  6.81 ms |
| filter/lowpass_73 1024²            | 21.3 ms  |
| filter/chroma_estimates 1024²      | 50.0 ms  |
| filter/rows_73 3000×2000           | 32.2 ms  |
| filter/cols_73 3000×2000           | 39.5 ms  |
| filter/lowpass_73 3000×2000        | 65.7 ms  |
| filter/chroma_estimates 3000×2000  | 163 ms   |
| separate/fixed 1024 / 2048         | 59.3 / 228 ms |
| separate/adaptive 1024 / 2048      | 83.2 / 344 ms |
| separate/mask_only 1024 / 2048     | 7.24 / 28.4 ms |
| blockfft/power_64                  | 57.2 µs  |
| resize/lanczos3 down 0.6 / up 1.5 (1024²) | 13.6 / 94.8 ms |

## Phase 6 — GUI (`mimizan-gui`)

egui/eframe 0.36, glow backend, no system-library build dependencies (the
file dialog is egui-file-dialog, pure Rust). Three columns: file (incl.
EXIF exposure) and development on the left, preview in the centre, mix /
look / export controls on the right. Opening a file runs decode → ingest →
separate with `keep_separation`, then builds upright, downscaled copies of
L̂, Ĉ1', Ĉ2' and the mask (longest edge 1800 px). Every control change
re-mixes and applies the curve on those copies in a second thread; only the
newest request is rendered. The preview is never sharpened. As soon as the
view magnifies the fit preview beyond one screen pixel per preview pixel,
the visible window is rendered 1:1 from the full-resolution separation: the
upright rectangle is mapped back into the sensor frame (inverse of the
`orient` table over the four corners, unit-tested for all eight tags),
L̂/Ĉ1/Ĉ2 are cropped, mixed, curved and oriented, and the result is painted
over the fit preview with nearest-neighbour filtering. The worker renders
only what changed (a pan re-renders the window, a slider the whole image);
a 1024×600 pt window costs ~50 ms. The `1:1` button and a double click go
to one negative pixel per device pixel; the status line then says so.

Z f, 16 threads: open 2.0 s (decode 452, ingest 385, separate 1132 ms
adaptive; 798 ms with the fixed estimator, the GUI default) + preview
planes 527 ms; preview update 16 ms (mix + curve). One export writes two
files through the same core calls as the CLI (`mix` → `print::render_planes`
→ `print::write`): the full-resolution TIFF (look, unsharpened) and the
screen JPEG (fixed 2048 px long edge, compensation). The earlier print panel
(paper size, dpi, viewing-distance USM, proof) and the later export panel
were removed from the GUI; the toolbar button's hover text states what is
written, the CLI `print` keeps the print path. The histogram readout is
painted inside the widget (cursor line + text) instead of a tooltip, whose
per-frame text changes made it appear only intermittently.

Weights and image quality (Z f chart, mask Off). Native ¼ ½ ¼ returns L̂
unchanged; the camera-file weights (raw R 0.284 G 0.404 B 0.312 → balanced
R 0.171 G 0.489 B 0.340) add k1·Ĉ1' + k2·Ĉ2' with k1 = −0.022, k2 = −0.338.
Because Ĉ1'/Ĉ2' are low-passed at 0.15 c/px (≈ 9 % of the Nyquist area),
the mix changes neither noise nor resolution measurably: on the six grey
patches (plane-fit detrended, 100×110 px) σ ratio camera/native =
0.997–1.000 at SNR 33–163, Laplacian (pixel-to-pixel) noise ratio 1.000;
Siemens star MTF50 0.306/0.303 c/px (axis/diag) vs 0.305/0.304. At a
colour edge (red patch against the dark frame) the tone shift of −16 %
settles within ~5 px, no overshoot. Even an extreme "red filter" mix
(0.9/0.1/0.0 balanced) costs only 19–21 % σ on the greys (pixel-to-pixel
noise +0.6 %) and MTF50 0.300/0.302: the chroma noise it adds is
band-limited, so it shows as faint low-frequency mottle, not grain.
On that basis the classic contrast filters were added as weight presets
(`ColorFilter`, SPEC §6): CLI `--filter`; in the GUI the Mix section is one
row of presets, *Native*, *Panchromatic* (the camera file's calibrated
weights; the word for a response that sees all colours like a film without
filters) and the six filters in front of it, each just a weight triple
that sets the two sliders. The matching preset is highlighted while the
sliders stand on it; moving a slider is the free form. The transmissions
are approximations and are labelled as such.

Rename: the project is now Mimizan Lab (crates `mimizan-core`,
`mimizan-cli` with binary `mimizan`, `mimizan-gui`; env vars `MIMIZAN_*`).
Earlier entries in this file keep the measurements but use the new name.

Clipping check (Z f chart, native weights, neutral look). The histogram
used to be taken from the 1800 px preview copy, which is Lanczos-3
resampled: ringing at the chart's black/white edges produced false clipped
pixels (black 0.280 % in the preview vs 0.021 % in the full-resolution mix,
13× too much) and the averaging hid isolated saturated pixels (white
0.071 % vs 0.088 %). Both histograms are now accumulated in one pass over
the full-resolution L̂/Ĉ1/Ĉ2 (per-row partial counts, 16-bit input
quantisation, two 65 536-entry LUTs for the gamma-2.2 axis and the look
curve), and black/white count the pixels that are 0 or 65535 in 16 bit,
which is exactly what the TIFF clips (the 8-bit end bins would add pixels
≥ 0.998 that are not clipped; on this chart the difference is 0.001 %).
Cost ~60 ms for 24 MP, cached per (weights, look), so rotation and the mask
view do not pay it. Sensor saturation stays a separate number in the File
column (0.072 % raw, 0.198 % dilated); the mix clips slightly more (0.087 %)
because the separation filters spread a saturated site over its
neighbours. A `⟳ 90°`
button adds clockwise quarter turns on top of the file's orientation tag;
the preview rotates its 8-bit image, the export composes the TIFF
orientation (`print::rotate_cw`, verified against `orient` for all eight
tags), so the exported pixels are upright and `orientation_applied` records
what was done. The toolbar status text is truncated to the free width so a
long log line can never cover the buttons; the window opens at 1360×860 pt
(minimum 820×560) and both side panels scroll and resize.

Unattended check: `MIMIZAN_SCREENSHOT=out.png mimizan-gui file.nef`
captures the window once the first preview is on screen and quits
(`MIMIZAN_SCREENSHOT_CUSTOM=1` switches the custom curve editor on,
`MIMIZAN_SCREENSHOT_1TO1=1` waits for the 1:1 window first).

Known platform limit: under Wayland the compositor stops frame callbacks
for a window it considers suspended (fully covered, or opened behind the
focused window). eframe then neither repaints nor runs `App::logic`, so a
running export finishes but its log line appears when the window is shown
again. X11/XWayland (`WAYLAND_DISPLAY= mimizan-gui`) does not have this.

## Real samples

`mimizan negative testdata/NikonZF.nef` (adaptive, as-shot balance R 2.0156
B 1.1113): decode 448 ms, ingest 393 ms, separate ≈ 1220 ms, total 2.1–2.2 s
for 24.4 MP (Phase 5; before Phase 5: 6.3 s). Defects repaired: 9658
(0.04 %), saturated photosites 17594 (dilated 48248).

`mimizan dump testdata/MonoRef.dng` (monochrome reference camera): 12-bit LinearRaw, white 3750, raw max
3971 (values above white are flagged saturated, as the file demands).

## Kodak fake-Bayer benchmark (`mimizan bench kodak`)

Full tables and crops: `docs/KODAK.md` (pixel-sharp) and `docs/KODAK_2X.md`
(2× magnified, band-limited). Generated from `testdata/kodak/kodim01..24.png`
with libraw 0.21 (`dcraw_emu`) and RawTherapee 5.11 (`rawtherapee-cli`);
24 pictures run in 55 s.

Method: 8-bit sRGB → linear → RGGB mosaic → 16 bit. The same samples go to
`separate` + `Native` mix and, as a minimal CFA DNG (`kodak::write_dng`,
ColorMatrix1 = XYZ→sRGB, AsShotNeutral 1,1,1), to the converters; their RGB
is reduced with `(R + 2G + B)/4`, the ground truth is the same luminance of
the linear source. PSNR/SSIM on sRGB-encoded 0..255 values, 16 px border
excluded (48 px changes nothing beyond 0.3 dB for every method).
Converter chains verified on a smooth synthetic picture: every method
≥ 59 dB, i.e. no tone curve, scaling or offset is left in the external path
(`dcraw_emu -4 -o 0 -r 1 1 1 1 -c 0 -H 0`; RT neutral profile, input profile
none, sRGB output curve inverted on read, `Border=0` so the frame is not
cropped).

Mean luminance PSNR over 24 pictures (dB). "Mimizan fixed" is `--mask off`,
"block mask" the former default `--mask adaptive`, "Mimizan" the present
default `--mask dubois` (next subsection):

| Condition | Bilinear | AHD | DHT | AMaZE | RCD | Mimizan fixed | Mimizan block mask | Mimizan |
|---|---|---|---|---|---|---|---|---|
| pixel-sharp (1×) | 32.61 | 38.71 | 38.30 | 40.72 | 38.48 | 34.60 | 34.56 | 36.45 |
| band-limited (2×) | 39.94 | 47.61 | 44.81 | 47.99 | 48.88 | 44.02 | 42.54 | **49.32** |

The first analysis (fixed and block mask only): Mimizan was below the best
external demosaicer on 24 of 24 pictures in both conditions. Three pictures
with large saturated colour areas (kodim02, 04, 15) were below even bilinear
at 1×. The residual is chroma leakage: the 0.15 c/px low-pass neither
captures pixel-scale chroma edges nor keeps luminance near the carriers (the
error alternates in sign between R and G/B sites in the worst blocks).
Parameter sweep with the mask off (means over 24):

| cutoff / transition / attenuation | 1× | 2× |
|---|---|---|
| 0.15 / 0.05 / 60 dB (default) | 34.60 | 44.02 |
| 0.10 / 0.05 / 60 dB | 34.45 | 39.82 |
| 0.20 / 0.05 / 60 dB | 33.56 | 48.12 |
| 0.15 / 0.15 / 40 dB | 35.57 | 44.62 |
| 0.20 / 0.20 / 40 dB | 35.02 | **48.48** |
| 0.20 / 0.20 / 40 dB, adaptive mask | 35.10 | 45.34 |

At 2× a wider pass band (0.20/0.20/40 dB) puts the pure separation on par
with the best demosaicers (48.5 vs RCD 48.9, AMaZE 48.0, AHD 47.6); at 1×
no setting of the present estimator comes within 3 dB of AHD. The adaptive
mask costs 1.5–3 dB in the band-limited condition (it keeps carrier energy
it classifies as texture), and is neutral at 1×. Both findings are open
items for the estimator, not for the benchmark: the numbers and the
converter chains were cross-checked as described above.

### Real RAW variant (`mimizan bench kodak --raw`)

No synthetic picture: the Z f NEF is decoded and balanced as in production,
every 2×2 CFA cell is binned to one RGB pixel (R, B measured; G the mean of
the two greens), the 3024×2016 result is the ground truth and is mosaicked
again with the camera's noise model (`docs/KODAK_ZF.md`). Relative to the
new grid the content is twice as sharp as the camera saw it, so the test is
harder than the camera's own files, equally for every method.

| File | Bilinear | AHD | DHT | AMaZE | RCD | Mimizan fixed | Mimizan block mask | Mimizan |
|---|---|---|---|---|---|---|---|---|
| NikonZF (daylight) | 33.58 | 38.00 | 37.22 | 38.62 | 38.90 | 33.53 | 33.76 | 36.33 |
| NikonZF_2 (tungsten, low light) | 34.67 | 39.14 | 38.53 | 39.87 | 40.08 | 34.30 | 34.19 | 37.07 |

Fixed and block mask sit at the bilinear level, 5–6 dB behind RCD. The
hardest windows (`docs/kodak_zf/*.png`) are the achromatic resolution
charts: line patterns and the Siemens star near Nyquist come out as a
checkerboard, i.e. luminance at the carrier frequencies is demodulated as
chroma and subtracted. This is the texture/chroma ambiguity the block mask
exists for, and it changes the result by ±0.2 dB only. The Dubois mode
(next subsection) gains 2.8 dB on these files and leaves 2.8 dB to RCD.

### Root cause of the deficit and the Dubois mode (`--mask dubois`, default)

Why the estimator trailed the demosaicers by 4–6 dB, found by taking the
fixed estimator apart with the Kodak bench as the meter. Three defects, all
in the separation, none in the benchmark:

1. **The two C2 copies were simply averaged.** `C2` sits at (0.5, 0) and
   (0, 0.5) with identical content; luminance leaks into a copy only where
   the picture has energy near that carrier (vertical line patterns spoil
   the (0.5, 0) copy, horizontal patterns the (0, 0.5) one), and a natural
   picture rarely has both at one place. The fixed estimator added the
   leak of both copies at half weight everywhere; the block mask could in
   principle weight them, but its 64 px block / 32 px step grid with 3×3
   erosion reduced to the plain mean nearly everywhere.
2. **The block mask cannot see the failure it was built for.** It decides
   from a band/ring energy ratio that presumes 1/f² luminance. A line
   pattern inside the band (the resolution charts, a picket fence) has
   exactly the spectrum of chroma and passes as such. The Z f result
   (±0.2 dB) is the measurement of that.
3. **Square 0.15 c/px bands waste chroma bandwidth.** Along a carrier the
   only neighbour is the other chroma component, so the pass band can be
   about twice as wide there; across the carrier, towards the luminance,
   it cannot. The same holds for `C1` at (0.5, 0.5) once the local
   structure direction is known: for a vertical structure (spectrum on the
   u axis) `C1` may be wide in u and narrow in v, and vice versa.

The fix (`crates/mimizan-core/src/dubois.rs`, after Dubois 2005, extended to
`C1`): two hypotheses are computed in full, H (horizontal structure: copy
(0.5, 0) clean, bands narrow in u and wide in v for `C2a` and `C1`) and V
(copy (0, 0.5) clean, bands wide in u, narrow in v for `C2b` and `C1`).
Per pixel the local energies `e_a = G_σ * C2a²`, `e_b = G_σ * C2b²`
(σ = 3 px) give the weight of H as `e_b / (e_a + e_b)`; both `C2` and `C1`
are blended with that weight. Default bands: narrow 0.15, wide 0.30 c/px,
transition 0.15, 40 dB. A flat colour field is reproduced exactly; a neutral
vertical line pattern at 0.42 c/px loses less than 10 % of the error of the
fixed estimator (unit tests). Run time: separation 1124 ms on the 24.4 MP
Z f file (fixed 752 ms, block mask 1161 ms); the mask sidecar is constant
in this mode, as with `--mask off`.

Sweep (mean luminance PSNR dB; 1× = all 24 Kodak pictures, 2× = kodim02,
08, 13, 19, 23 magnified, raw = both Z f files). `c1` is the `C1` band
(square unless two values), `n`/`w` the narrow/wide `C2` bands, `t`/`a`
transition and attenuation, σ the energy window:

| Variant | 1× | 2× | raw |
|---|---|---|---|
| fixed (`--mask off`) | 34.60 | 42.47 | 33.92 |
| block mask (`--mask adaptive`) | 34.56 | 40.65 | 33.98 |
| Dubois C2 only, c1 .15, n .15, w .30, t .10, 40 dB, σ 3 | 36.35 | 46.07 | 36.15 |
| … w .40 | 35.65 | 45.56 | 36.21 |
| … n .12 | 36.53 | 44.11 | 35.94 |
| … n .20 | 35.62 | 47.39 | 36.40 |
| … c1 .20 | 36.14 | 47.64 | 36.23 |
| … c1 .10 | 35.81 | 43.39 | 35.88 |
| … σ 1.5 / σ 6 | 36.43 / 36.14 | 46.12 / 46.07 | 36.30 / 36.10 |
| … t .15, 40 dB | 36.78 | 46.15 | 36.51 |
| … t .05, 60 dB | 35.71 | 45.48 | 35.72 |

On a harder subset (1× = kodim01, 02, 05, 08, 13, 19, 23; 2× = kodim08, 19;
raw = both Z f files) the direction-dependent `C1` band was added to the
best row above (t .15, 40 dB):

| Variant | 1× | 2× | raw |
|---|---|---|---|
| fixed | 32.48 | 42.00 | 33.92 |
| Dubois, C1 square .15 | 34.82 | 45.58 | 36.51 |
| Dubois, C1 .15/.30 (default) | 34.53 | 47.01 | 36.70 |
| Dubois, C1 .15/.35, C2 .15/.35 | 33.98 | 47.51 | 36.63 |
| Dubois, C1 .15/.40, C2 .15/.40 | 33.02 | 45.50 | 36.25 |
| Dubois, C1 .12/.35, C2 .12/.35 | 34.56 | 45.20 | 36.52 |
| default with energies squared before weighting | 34.64 | 46.86 | 36.54 |

The wide band cannot go beyond about 0.30: under the V hypothesis `C1`
(0.5, 0.5) and `C2b` (0, 0.5) both want to be wide along the same line
v = 0.5 and meet at u = 0.25, so with transition 0.15 they start to overlap
at 0.35 (the collapse of the 0.40 rows). Choosing 0.30 for `C1` gives up
0.3 dB on pixel-sharp content for +1.4 dB band-limited and +0.2 dB on the
real files; real cameras sit between the two Kodak conditions.

Two more experiments, both negative, kept for the record:

- Error decomposition of the Dubois result by carrier band (demodulate the
  luminance error by each carrier, low-pass 0.15): at 1× the `C2` bands hold
  2 × 10–28 %, `C1` 2–12 %, and 38–68 % lies outside all bands, i.e. is
  chroma beyond the pass band (sharp coloured edges) that no band shape can
  recover without taking luminance along. At 2× and on the real files a
  16–38 % base-band share appears, the noise difference between the
  single measured value per site and the mean the ground truth contains;
  every method pays it.
- Least-squares designed demultiplexing filters (11×11 and 15×15, trained on
  one Z f file or on the even Kodak pictures, tested on the rest): 33.4–35.0
  dB at 1×, 39.2–39.6 at 2×, 35.6–37.0 raw, i.e. no better than the
  hand-designed bands with the same weighting. Short fixed filters cannot
  express a 0.15 c/px transition; longer ones were not tried.

Result with the default on the full sets (`docs/KODAK.md`, `docs/KODAK_2X.md`,
`docs/KODAK_ZF.md`):

| Condition | fixed | block mask | Dubois | gain | best demosaicer |
|---|---|---|---|---|---|
| pixel-sharp (24) | 34.60 | 34.56 | 36.45 | +1.9 | AMaZE 40.72, AHD 38.71 |
| band-limited (24) | 44.02 | 42.54 | **49.32** | +5.3 | RCD 48.88 |
| Z f binned (2) | 33.92 | 33.98 | 36.70 | +2.8 | RCD 39.49 |

Synthetic bench with the new default (`measure synth --size 1024`, RMSE/σ,
Dubois vs fixed): Star 3.75 vs 5.75 (MTF10 0.447 vs 0.405), Zoneplate 9.7
vs 18.7, Edge 0.92 vs 1.29, Patches 2.94 vs 4.52 (carrier max 0.853 vs
0.804), Wedge 0.98 vs 1.49, Alias 34.5 vs 41.5 (still the fail scene),
**Fabric 2.76 vs 1.05**: the pure colour texture at 0.12 c/px sits in the
wider transition band (pass to 0.075 c/px at 0.15/0.15 instead of 0.125 at
0.15/0.05) and is partly left in `L̂`. The trade-off is deliberate; the
Kodak sweep rates the narrow transition 1.1 dB worse at 1× and 0.8 dB at
2×, and a pure chroma sine at 0.1 c/px over a whole picture is a
test-chart case. It was the one regression of the 0.2.0 default; the
coherence step of the next subsection removes it (Fabric 0.86).

Band-limited, Mimizan is now the best method in the mean and on 15 of 24
pictures; every picture is above bilinear in every condition (kodim02, 04,
15 included). Pixel-sharp it stays 2.3 dB behind AHD and 4.3 dB behind
AMaZE, on the real files 1.9 dB behind AHD and 2.8 dB behind RCD. What is
left is the chroma beyond the pass band at sharp coloured edges and the
Nyquist line patterns that contaminate both copies at once; a linear
pass-band estimator cannot separate either, the demosaicers get them with
nonlinear decisions (homogeneity selection, median of colour differences).

### Coherence ring (0.3.0 default: `ring_wide` 0.25, `ring_power` 2, `ring_c1`)

The elongated bands of the previous subsection buy luminance safety with
chroma bandwidth everywhere, also where no luminance is near the carrier.
The two `C2` copies tell where that price is unnecessary: chroma is in both
copies, a luminance leak in one only. So the content of the ring between
the narrow square band (0.15) and a wider square band (`ring_wide`) is
compared between the copies; where it is coherent it is chroma and the
band may be opened to the square wide shape, where it is not it stays
elongated. Per pixel, with `rA`, `rB` the ring signals (wide square copy
minus narrow square copy of each carrier) and `G_σ` the same 3 px window
as the energies:

    κ = clamp( G_σ * (rA·rB) / sqrt( G_σ * rA² · G_σ * rB² ), 0, 1 )^ring_power
    Ĉ2a := Ĉ2a + κ·(Ĉ2a_square_wide − Ĉ2a)      (same for Ĉ2b, then the energy blend)
    Ĉ1  := Ĉ1  + κ·(Ĉ1_square_wide  − Ĉ1)       (if ring_c1, after the direction blend)

κ is a normalised cross-correlation of the two rings, 1 for identical
content, 0 for uncorrelated content, negative correlation clamped to 0;
the square makes the opening cautious at κ ≈ 0.5.

Sweep on the hard subsets (1× = kodim01, 02, 05, 08, 13, 19, 23; 2× =
kodim08, 19; raw = NikonZF, NikonZF_2 binned; soft = NikonZF_2 and
nikon_zf_27 binned then magnified ×2 (`--raw --scale 2`, the camera's
pixel count and band limit); lens = kodim08, 19, 23 blurred with σ 0.75 px,
MTF50 at 0.25 c/px, content to Nyquist). Mean luminance PSNR dB:

| Variant | 1× | 2× | raw | soft | lens |
|---|---|---|---|---|---|
| fixed (`--mask off`) | 32.48 | 42.00 | 33.92 | 43.70 | 43.65 |
| Dubois 0.2.0 (no ring) | 34.53 | 47.01 | 36.70 | 49.70 | 49.27 |
| ring 0.20, power 2 | 34.38 | 47.23 | 36.59 | 49.07 | 49.06 |
| **ring 0.25, power 2** | **34.59** | **47.56** | 36.66 | 53.39 | **49.84** |
| ring 0.30, power 2 | 34.19 | 47.00 | 36.28 | 51.24 | 48.90 |
| ring 0.25, power 1 | 34.44 | 47.33 | 36.47 | **53.60** | 49.42 |
| ring 0.25, power 4 | 34.63 | 47.34 | **36.72** | 52.64 | 49.83 |
| ring 0.25, power 2, C1 not opened | 34.57 | 47.45 | 36.76 | 52.24 | 49.60 |

0.20 leaves the ring inside the 0.15 transition band and measures nothing;
0.30 runs into the `C1`/`C2b` overlap at u = 0.25 described above. The
sharp optimum at 0.25 for the soft condition is partly the brick-wall band
limit of that condition (Lanczos ×2 stops at 0.25 c/px exactly); the lens
condition, whose content reaches Nyquist, confirms a smaller real gain
(+0.6) at the same setting, and 1×/2× do not move against it. Power 2 and
opening `C1` as well are each worth 0.1–1.2 dB on the band-limited
conditions and nothing elsewhere.

Negative results of the same round, kept for the record:

- Cross-term rounds (re-estimate `L̂`, demodulate it, subtract the
  estimated leak from the copies, re-weight): −1 dB at 2× and soft, no
  change at 1×. The leak estimate carries the chroma error back into the
  copies.
- Noise-energy subtraction in the weights (`e − σ²·‖G_σ‖²` before the
  ratio): no measurable effect in any condition; the energies that decide
  the weight are far above the noise floor wherever the weight matters.

Full sets with the new default (`docs/KODAK.md`, `docs/KODAK_LENS.md`,
`docs/KODAK_2X.md`, `docs/KODAK_ZF.md`, `docs/KODAK_ZF_SOFT.md`; the Z f set
now has four files, two scenes):

| Condition | fixed | block mask | Dubois 0.2.0 | **0.3.0** | best demosaicer |
|---|---|---|---|---|---|
| pixel-sharp (24) | 34.60 | 34.56 | 36.45 | **36.55** | AMaZE 40.72, AHD 38.71 |
| lens-like σ 0.75 (24) | 43.95 | 43.07 | – | **50.21** | RCD 50.59, AMaZE 50.50 |
| band-limited 2× (24) | 44.02 | 42.54 | 49.32 | **50.80** | RCD 48.88 |
| Z f binned (4) | 37.22 | 37.18 | 40.29 | **40.38** | RCD 42.81, AMaZE 42.10 |
| Z f binned, ×2 (4) | 42.61 | 42.49 | 48.16 | **51.92** | RCD 50.66, AMaZE 48.58 |

Per picture: at 1× no picture loses more than 0.06 dB (kodim08), kodim04
gains 0.79; at 2× every picture gains (0.26 to 4.87 dB) and 23 of 24 are
above every demosaicer (kodim19, the fence, is 0.5 dB below RCD);
lens-like, 6 of 24 beat every demosaicer and the largest gap is again
kodim19 (−4.4 dB to AMaZE); on the real files at the camera's pixel count
three of four are above RCD (+0.85, +3.35, +1.00 dB) and NikonZF is 0.13
below. The binned real files, twice as sharp as the camera saw them,
behave like the pixel-sharp Kodak set: unchanged within 0.05 dB, 2.4 dB
behind RCD.

Synthetic bench (`measure synth --size 1024`, RMSE/σ, 0.3.0 vs 0.2.0): Star
3.77 vs 3.75 (MTF10 0.447 unchanged), Zoneplate 10.05 vs 9.7, Edge 0.90 vs
0.92, Patches 2.87 vs 2.94, Wedge 0.96 vs 0.98, Alias 34.5 unchanged,
**Fabric 0.86 vs 2.76**: the pure colour texture is coherent in both
copies, κ opens the band and the regression of 0.2.0 is gone.

Cost: separation 2.17 s instead of 1.12 s on the 24.4 MP file (total 3.3 s,
**[Abweichung]** from the 3 s budget of SPEC §12), peak ten full f64 planes
(2.0 GB measured) instead of six. The low-passes of the energies and of the
coherence run in place with one scratch plane; a first version with
separate planes needed 2.9 GB and 3.6 s.

## Z f negative against the reference camera (0.3.0, both pairs)

The question this project set out to answer, measured as directly as the
material allows: how close is the Mimizan negative of the Z f to the
negative of a real monochrome sensor on the same scene. Both studio pairs,
Z f through `negative` with `cameras/nikon_z_f.json` (0.3.0 default), the
reference DNG through `negative` without separation; both linear, 16 bit,
1.0 = white level. Star centres refined to 0.05 px on the 45th harmonic,
the Z f grey-scale grid mapped from the reference grid through the
ColorChecker transform and aligned on the residual sd of the fields
(offset +1/−9 px and +2/−9 px). Pair 1: Z f 85 mm f/5.6 1/40 s ISO 100,
reference 50 mm f/4.8 1/125 s ISO 320. Pair 2: Z f f/5.6 3 s, reference f/8
1 s. All resolution figures are system MTF, lens included; the two pairs
bracket the lens difference (the reference at f/8 is sharper than at f/4.8).

**Resolution and micro-contrast** (Siemens star, 45 cycles, radii 10–118 px
= 0.06–0.70 c/px, Z f as percentage of the reference):

| | Pair 1 | Pair 2 |
|---|---|---|
| MTF50 axis / diagonal (c/px) | 0.263 / 0.261 vs 0.251 / 0.251 = **105 % / 104 %** | 0.247 / 0.241 vs 0.278 / 0.260 = **89 % / 93 %** |
| MTF10 axis / diagonal | 0.490 / 0.439 vs 0.537 / 0.503 = 91 % / 87 % | 0.451 / 0.432 vs 0.561 / 0.541 = 80 % / 80 % |
| MTF at 0.10 c/px | 0.915 vs 0.906 = 101 % | 0.910 vs 0.918 = 99 % |
| MTF at 0.20 | 0.656 vs 0.630 = 104 % | 0.612 vs 0.659 = 93 % |
| MTF at 0.30 | 0.407 vs 0.379 = 107 % | 0.349 vs 0.424 = 82 % |
| MTF at 0.40 | 0.196 vs 0.200 = 98 % | 0.145 vs 0.246 = 59 % |
| MTF at 0.50 | 0.076 vs 0.094 = 80 % | 0.047 vs 0.126 = 37 % |
| area under the MTF to 0.30 c/px | 104 % | 93 % |
| area to Nyquist | 103 % | 86 % |
| ring / alias flags | none / none | none / none |

Up to 0.30 c/px, where micro-contrast lives, the Mimizan negative is at
93–104 % of the reference; above 0.35 c/px it falls off systematically, to
37–80 % at Nyquist. That is the chroma notch around the carriers, where a
linear estimator cannot tell luminance from chrominance. The reference's
MTF10 beyond 0.5 c/px (no AA filter, no CFA) is aliasing, not usable detail,
and is not penalised here.

**Tonality** (Kodak grey scale, 20 fields, inset 0.25, sd after a plane fit
per field; SNR interpolated log-log between fields at equal fraction of
full scale; ColorChecker 24 patches with one free scale):

| | Pair 1 | Pair 2 |
|---|---|---|
| SNR at 10 % of full scale | 101 vs 48 (+6.4 dB) | 108 vs 36 (+9.5 dB) |
| SNR at 2 % | 39 vs 24 (+4.4 dB) | 43 vs 21 (+6.4 dB) |
| grey scale, ref = s·zf + p | s 1.117, p 0.0005; residual rms 5.5 %, max 11.8 % | s 0.580, p 0.0053 (the known pedestal); rms 4.2 %, max 6.8 % |
| fields above 2 % of full scale | within ±2 % | within ±4 % |
| ColorChecker rms / max | **3.6 % / 9.6 %** (purple −8.7, black −9.6) | 9.9 % / 30 % (session mismatch, see "Second pair") |

The SNR advantage is real in the file and still to be read with care: the
separation band-limits the luminance (the noise in the chroma bands goes
with them), and a 14-bit sensor at ISO 100 stands against a 12-bit file at
ISO 320 from 2016. It is a property of the file, not a merit of the method
alone; per pixel the reference holds more independent measurement. Grey
tracking: lights and mids run parallel within ±2 % (0.03 EV); in the deep
shadows (fields 17–20, below 2 % of full scale) the Z f negative of pair 1
is 10–15 % darker than the reference, flare of the shorter reference
exposure or black point, undecidable on two files. ColorChecker 3.6 % rms =
0.05 EV: the spectral mapping of colour to grey meets the reference within a
twentieth of a stop in the mean, the worst patch at 0.14 EV.

**Artefacts.** Carrier-band modulation in the flat grey fields (demodulated
by the three carriers, low-pass 0.10 c/px, rms in % of the field mean, mean
of fields 2–20): Z f C1 0.014 %, C2 0.27 / 0.25 %; the reference, which has
no carriers, shows 0.52 / 0.48 / 0.51 % (pair 1) and 0.77 / 0.76 / 0.76 %
(pair 2) of pure noise in the same bands. What Mimizan leaves at the
carriers in flat areas lies below the noise of a real monochrome sensor.
In structures near Nyquist the residue does not appear as a pattern but as
the MTF fall-off above.

**Sum.** Coarse detail and micro-contrast to 0.30 c/px 93–104 % of the
reference; limiting resolution (MTF10) 80–91 %, at Nyquist 37–80 %;
spectral tone mapping 96 % (3.6 % rms) on the clean pair; grey tracking ±2 %
in lights and mids, up to 15 % off in the deepest shadows; noise better than
the reference, partly for foreign reasons; no artefacts above noise in flat
areas. Not covered: two scenes, two lenses, no hair, foliage or fabric near
Nyquist, and the reference is not charged for its aliasing. Measured with a
temporary example (star refinement, patch statistics, carrier demodulation)
that is not part of the crate.


## Reconstruct — the optional nonlinear step (`--reconstruct`, GUI checkbox, default off)

Everything above is linear: every output pixel is a weighted sum of
measured values whose weights do not depend on the content at that pixel
(the Dubois mode chooses between two such sums, it invents none). The
tables show where that ends: the last octave before Nyquist, where a
filter cannot tell luminance from chrominance, 2–4 dB behind the
demosaicers on pixel-sharp content and 2.4 dB on the real files sampled
at twice the camera's sharpness. This section measures what a nonlinear
step gains there, built so that it stays optional and is accounted for per
pixel: the result is no longer a measurement in those pixels but a
decision, and the sidecar says where.

**Estimator** (`reconstruct.rs`, SPEC 5.6; Zhang & Wu 2005, written from
the paper): directional colour-difference signals `G − X` along rows and
columns at every pixel (missing channel by a 4-tap neighbour filter plus a
Laplacian of the own channel, `A` 0.0625, `LAP` 0.125), per-line LMMSE with
a 9-tap prior and ±4 statistics, fusion of the two directions by their
error variances with a chroma tie-breaker `μ` (where both directions are
self-consistent, the one implying less chrominance wins: a neutral Nyquist
pattern over a saturated one), `R`, `B` by colour-difference means. The
whole thing runs on `m^(1/2.2)` and is decoded afterwards.

**Detector** (`Residue`): the linear luminance of the Dubois mode is
demodulated by the three carriers and low-passed (Gaussian σ 2 px); the
summed squared amplitude against the noise variance in the same band is
`r`; β ramps from 0 at `r = 20` to 1 at `r = 80`. Where the linear
separation was clean, only noise sits near the carriers (`r ≈ 1`) and the
pixel stays measured. The sidecar carries `round(255·(0.8 + 0.19·β))`:
204 measured, 252 computed, 255 saturated as before; the sharpening gate
(≥ 0.8) still opens.

Development on the hard subsets of the coherence-ring section (here soft =
NikonZF_2 only), mean luminance PSNR dB; the Dubois row is the 0.3.0
default, `all` is the nonlinear estimate everywhere (β = 1), the rest are
hybrids with the `Residue` detector at `lo/hi`:

| Variant | 1× | 2× | raw | soft | lens |
|---|---|---|---|---|---|
| Dubois 0.3.0 | 34.59 | 47.56 | 36.66 | 48.57 | 49.84 |
| nonlinear everywhere, linear domain | 36.57 | 44.06 | 36.99 | 41.24 | 46.36 |
| nonlinear everywhere, domain `^1/2.2` | 42.17 | 49.04 | 38.17 | 41.33 | 51.12 |
| — same, `A` 0.0625, `LAP` 0.125 | 41.94 | 49.64 | 38.52 | 42.35 | 52.16 |
| hybrid `Residue` 100/400 | 39.56 | 49.53 | 38.09 | 48.60 | 51.19 |
| **hybrid `Residue` 20/80** | **40.67** | **49.85** | 38.46 | **48.56** | 52.13 |
| hybrid `Residue` 10/40 | 41.08 | 49.86 | **38.54** | 48.30 | **52.40** |
| hybrid, share detector 0.001/0.005 | 37.53 | 47.58 | 37.61 | 48.54 | 50.36 |
| best demosaicer on the subset | AMaZE 38.44 | RCD 47.02 | RCD 39.49 | – | AMaZE 51.22 |

Three findings decided the design. First, the processing domain: the same
estimator gains 5.6 dB at 1× and 1.2 dB on the real files by running on
`m^(1/2.2)` instead of linear light; colour differences are nearly
constant across an edge in a compressed domain and far from it in linear
light, which is why the demosaicing literature, which works on 8-bit sRGB
pictures, never had to notice. Second, the detector is not optional: on
band-limited content with real noise (soft) the nonlinear estimate alone
is 6–7 dB worse than the linear one, because the smooth prior of the LMMSE
takes band-limited chroma for noise; the `Residue` detector leaves those
pixels alone (β 0.5 %) and the condition is unchanged within 0.01 dB. A
scale-free detector (carrier-band energy as a share of local AC energy)
cannot tell carrier-band signal from carrier-band noise and loses 3 dB at
1×; a detector on the difference of the two luminance estimates is worse
everywhere; one on the local gradient is useless. Third, the estimator
itself: a 2-D error statistic over the neighbouring same-channel lines
(−0.2 dB), colour-difference smoothing as a refinement pass (−0.7 to
−1.3 dB), and longer directional filters by 1-D demodulation (equal to
−0.2 dB with the LMMSE, catastrophic without) all lost against the
per-line form. `20/80` was chosen over `10/40` for the soft condition
(−0.27 there, the only condition where the nonlinear step can only lose).

Full sets (`docs/KODAK.md`, `docs/KODAK_LENS.md`, `docs/KODAK_2X.md`,
`docs/KODAK_ZF.md`, `docs/KODAK_ZF_SOFT.md`, fourth Mimizan column):

| Condition | Mimizan 0.3.0 | **+ reconstruct** | computed | best demosaicer | at or above the best |
|---|---|---|---|---|---|
| pixel-sharp (24) | 36.55 | **42.04** | 49 % | AMaZE 40.72 | 0 → 22 of 24 |
| lens-like σ 0.75 (24) | 50.21 | **51.93** | 12 % | RCD 50.59 | 6 → 21 of 24 |
| band-limited 2× (24) | 50.80 | **51.96** | 5 % | RCD 48.88 | 23 → 24 of 24 |
| Z f binned (4) | 40.38 | **41.67** | 10 % | RCD 42.81 | 0 → 0 of 4 |
| Z f binned, ×2 (4) | 51.92 | 51.91 | 0.5 % | RCD 50.66 | 3 → 3 of 4 |

Per picture: at 1× every picture gains, 2.7 (kodim04) to 7.2 dB (kodim13),
and only kodim03 (−0.55) and kodim12 (−0.09) stay below AMaZE; lens-like
every picture gains (0.2 to 3.2 dB), three stay below the best (kodim19
−1.2, kodim03 −0.7, kodim07 −0.2); at 2× 23 gain and kodim24 loses 0.7
(still 1.0 above RCD); on the real files at twice the camera's sharpness
all four gain (0.4 to 2.0 dB) and all four remain 0.7–1.8 dB below RCD,
now above AHD and DHT and 0.4 below AMaZE; with β = 1 everywhere the two
NikonZF files reach 38.52 against RCD's 39.49, so on real noise the
estimator, not the detector, is the limit. At the camera's own band limit
(soft) the step is inactive (0.5 %) and changes nothing beyond ±0.2 dB.

What the step does and does not change about the method: the linear path
is untouched (β = 0 reproduces 0.3.0 bit for bit, the bench's third column
did not move); where β > 0 the output is a direction decision on
interpolated colour differences, the kind of thing the demosaicers do, and
a wrong decision there is a zipper or a smeared line, not a periodic
residue. The claim of the README is therefore split: the linear default is
the interpolation-free conversion; the checkbox is the best result we can
measure, and the negative records per pixel which of the two it is.

Cost: separation 4.5 s instead of 2.3 s on the 24.4 MP file (gamma
encode/decode, two LMMSE passes with a blocked transpose, the detector's
five filter passes), four more planes at the peak. GUI: checkbox
"reconstruct the last octave" under Development with the warning text,
needs Apply; the preview and the export use the same separation, the
info panel shows the computed share.
