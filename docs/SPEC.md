# Mimizan Lab SPEC — the numbers the code needs

As of October 2026. Read with `whitepaper_v1.pdf`. Where this document
departs from the whitepaper, this document governs; every departure is
marked **[Deviation]** and given its reason. Nothing here is "TBD". Every
default has a value and a unit.

Conventions: `x` is the column (0 = left), `y` the row (0 = top).
`sx = (−1)^x`, `sy = (−1)^y`. Frequencies in cycles per pixel (c/px),
Nyquist = 0.5. All image values are `f64`, normalised so that white level = 1.0
and black level = 0.0.

---

## 1. Scope

Input: Bayer RAW with a 2×2 pattern (RGGB, BGGR, GRBG, GBRG), integer
(≤ 16 bit) or float, and monochrome DNGs (photometric `LinearRaw` without
a CFA). Excluded (an error, no fallback): X-Trans (6×6), Quad-Bayer at
full resolution (4×4), Foveon, Nikon high-efficiency NEF (HE/HE*), files
with no usable CFA statement. The saturation mask, the noise model, the
separation and the mix apply per file; a camera file (`cameras/*.json`) is
optional.

## 2. Ingest

Decoder: `rawler` 0.8 behind the `RawDecoder` trait. What is read is the raw
CFA array (`RawImage.data`), never a demosaiced image.

| Quantity | Source | Rule |
|---|---|---|
| Black level | `blacklevel.levels`: one value for every photosite, or a 2×2 tile (CFA order); any other tile size is an error | subtract per photosite |
| White level | `whitelevel.0`; one value or four (RGBE order by colour) | divide per photosite |
| CFA phase | `photometric = Cfa(cfg)`, `cfg.cfa.name` ∈ {RGGB, BGGR, GRBG, GBRG}, relative to the full sensor (before the crop) | required; a camera file may only confirm it, not override it |
| Crop | `crop_area` (the manufacturer's recommendation); if `None`: `active_area`; if `None`: the whole image | the crop origin must be even, otherwise shift it inward by 1 so the phase is kept |
| Monochrome | `photometric = LinearRaw` or `BlackIsZero` with `cpp = 1` | no separation; `L = m` |

Normalisation: `m = (raw − black) / (white − black)`. Values below 0 stay
negative (noise must not be clipped), values above 1 stay above 1 until
the saturation rule.

**Saturation.** A photosite is saturated when `raw ≥ white·(1 − sat_margin)`
with `sat_margin = 0.02` (**[Deviation]**: the whitepaper says "8 steps",
with no unit; at 14 bit, 8 DN would be too tight, and sensors go nonlinear
a percentage short of the white level). The saturation mask is dilated by
2 px (the same-phase neighbours share the estimation error). Saturated
photosites stay in the image at their clipped value; they are excluded from
statistics (fits, measurements) and marked 255 in the mask sidecar.

**Defects.** For each photosite `v`, the eight same-phase neighbours
(distance 2 in x and/or y), median `med`, minimum `lo`, maximum `hi`.
If `max(v − hi, lo − v) > max(8·σ(med), hi − lo)` (σ from section 3),
replace `v := med`. The neighbour span `hi − lo` is a model-free noise
scale (≈ 3σ for eight neighbours), so the rule still holds at ISO values
above the noise defaults. **[Deviation]**: the whitepaper compares against
the median only; on every sharp edge a pixel lies more than 8σ from the
neighbour median (measured: 8 % of all photosites in the Z f sample).
A defect lies outside the neighbours' value range, an edge does not.
Saturated photosites and their neighbours are exempt from the defect rule.
The defect map is counted and logged (count, fraction, row clusters as a
PDAF hint). The repair replaces measured values; in the GUI it is therefore
off by default (Development → *repair defects*), and the CLI keeps it on
(`--no-defects` turns it off).

**Channel balance.** After normalisation and the defect rule, each photosite
is multiplied by its channel's factor `k_R, k_G = 1, k_B` (`--wb`): default
`as-shot` from the file (`wb_coeffs`, normalised to G), with fallback and
option `gray` (gray-world: `k_c = mean_G / mean_c` over unsaturated
photosites), `none`, or three numbers. **[Deviation, measured]**: the
whitepaper separates on raw levels. A neutral subject is then not neutral
in the raw channels, the chrominance is proportional to the luminance
(`C1, C2 ∝ L`), and the *entire* luminance spectrum sits as a copy on the
carriers. The mask then has to call everything "chrominance"; the adaptive
stage does nothing. With the balance, the carriers of a neutral subject
are empty, and the mask can leave luminance alone (Siemens star:
MTF10 0.405 → 0.451 = truth, RESULTS.md). Every section below works on
balanced channels; the weights in section 6 refer to balanced channels (a
neutral grey gives the same output for every weight triple, as
filter-factor-compensated black-and-white filters do).

## 3. Noise model

Standard deviation in normalised units:

    σ(s) = noise_a · sqrt(max(s, 0)) + noise_b

**[Deviation]**: the whitepaper names `noise_a = 0.005`, `noise_b = 10⁻⁵`
with no model form. As a variance, `a = 0.005` would be ISO-50 000 territory
(SNR ≈ 6 at mid-grey) and the mask would stay at 0.5 everywhere. As a
standard deviation, `a = 0.005` fits a 24 MP camera at base ISO (measured
≈ 28 DN at 14 bit, model ≈ 34 DN).

Defaults until a dark frame exists: `noise_a = 0.005`, `noise_b = 2·10⁻⁴`
(≈ 3 DN at 14 bit; the whitepaper's `10⁻⁵` would be 0.16 DN, below any
real read noise).

The model applies to raw levels. After the channel balance, a photosite of
colour c at balanced level `s` has variance `k_c²·σ²(s / k_c)`. The mean
variance of a block is the Bayer-weighted mean
`¼·Var_R + ½·Var_G + ¼·Var_B` of the *per-channel* block means (one shared
block mean underestimates red blocks by up to 15 %, measured).

## 4. Decomposition

With `L = (R + 2G + B)/4`, `C1 = (−R + 2G − B)/4`, `C2 = (R − B)/4`
(signs as in the whitepaper), every 2×2 phase satisfies

    m(x, y) = L + ε·sx·sy·C1 + C2·(a·sx + b·sy)

| Phase (colour at (0,0),(1,0),(0,1),(1,1)) | ε | a | b |
|---|---|---|---|
| RGGB | −1 | +1 | +1 |
| BGGR | −1 | −1 | −1 |
| GRBG | +1 | −1 | +1 |
| GBRG | +1 | +1 | −1 |

Derivation: `p_R = (1 + r_x·sx)(1 + r_y·sy)/4` with `r_x, r_y = ±1`
according to where R sits, and B likewise with the signs reversed;
`p_G = 1 − p_R − p_B`. The whitepaper formula `C2·[(−1)^x − (−1)^y]` is
the GBRG case.

Inverse (needed for the mix):

    G = L + C1
    R = L − C1 + 2·C2
    B = L − C1 − 2·C2

Carriers: C1 at (0.5, 0.5); C2 at (0.5, 0) and (0, 0.5). Chrominance is
physically limited to |f| ≤ 0.25 c/px around its carrier.

## 5. Separation

### 5.1 Demodulation and low-pass (fixed stage)

    z1  = ε·sx·sy·m          Ĉ1  = LP(z1)
    z2a = a·sx·m             Ĉ2a = LP(z2a)
    z2b = b·sy·m             Ĉ2b = LP(z2b)

`LP` is a separable low-pass (rows first, then columns) with the same
1-D kernel, a Kaiser-windowed sinc:

| Parameter | Default | Meaning |
|---|---|---|
| `chroma_cutoff` | 0.15 c/px | −6 dB point |
| `chroma_transition` | 0.05 c/px | transition width (passband to 0.125, stopband from 0.175) |
| `chroma_attenuation` | 60 dB | stopband attenuation → Kaiser β = 0.1102·(A − 8.7) = 5.65 |
| Taps | computed from the three, odd | N = ceil((A − 8)/(2.285·2π·Δf)) → 73 |

Edge: reflection without repeating the edge pixel ("reflect-101").
The kernel is corrected (a constant plus an alternating constant) so that
the sum is exactly 1 and the response at 0.5 c/px is exactly 0: a flat
colour field then separates with no 2 px residue at all (the carriers sit
at Nyquist). The passband is square
(|fx| < fc and |fy| < fc), not circular; the corners of the square lie
at 0.21 c/px and so still inside the physical chroma limit.

**[Deviation]**: the whitepaper says "full FFT". The carriers are fixed,
so the separation is a convolution with a fixed kernel; in the spatial
domain it is faster, leaner in memory (no complex 45 MP arrays in f64) and
deterministic. The result is identical up to rounding.

### 5.2 Block mask (`--mask adaptive`, no longer the default, see 5.5)

Blocks of 64×64, stride 32 (50 % overlap), 2-D Hann window, 2-D FFT in
f64, block mean subtracted before the window. Per block and carrier
k ∈ {C1, C2a, C2b}, from the block spectrum of `m` (supremum norm, matching
the square passband):

    Core   |f − f_k|_∞ < 0.05           always chrominance (the block's hue, colour edges)
    Band   0.05 ≤ |f − f_k|_∞ < 0.15    disputed: P_C(k) = Σ |M(f)|²
    Ring   0.15 ≤ |f − f_k|_∞ < 0.35    luminance evidence: P_L(k) = Σ |M(f)|²

Expected noise energy per bin `N_bin = Var_Block·Σ w²` (section 3,
per-channel block means). With `n_C, n_L` bins in each region:

    E_L = max(P_L − n_L·N_bin − 3·N_bin·sqrt(16·n_L), 0)
    E_C = max(P_C − n_C·N_bin − 3·N_bin·sqrt(8·n_C), 0)
    M_k = 0                                        if E_L = 0  (no luminance evidence)
    M_k = 1                                        if E_C = 0  (only noise in the band)
    M_k = min( min(1, 0.3·E_L / E_C)²,
               sqrt(n_C·N_bin / (E_C − 0.3·E_L)) )  otherwise

`M_k` ∈ [0, 1]: 1 means "the band content is luminance, leave it", 0 means
"the band content is chrominance, remove it". Blocks with more than 25 %
saturated photosites get `M_k = 0` for every k.

**[Deviation, measured]** from `M = E_L/(E_L + E_C)`, 0.5 below 2σ:

- *Always remove the core.* A block's hue and the transition at colour
  edges lie under 0.05 c/px around the carrier; there is no luminance
  there to save. Without this rule every block with M > 0 left the hue
  standing as a 2 px grid.
- *Factors 16 and 8.* Sums over the bins of a Hann-windowed Bayer noise
  spectrum do not scatter like independent χ² bins (`sqrt(2·n)`), but
  2.9× (ring) and 2× (band) more strongly: window correlation, conjugate
  symmetry and the 2×2 sublattices (each colour spectrum repeats with
  period 0.5). Measured on pure noise (test
  `noise_sum_fluctuation_matches_constants`). With `sqrt(2·n)` the mask
  kept firing in flat fields.
- *Leak ratio 0.3.* Luminance with a 1/f² spectrum (edges) puts only
  ∫₀.₃₅^0.5 f⁻² / ∫₀.₁₅^0.35 f⁻² ≈ 0.225 of the ring energy into the band;
  0.3 leaves textures a little room. Whatever exceeds that in the band is
  chrominance.
- *Square.* Doubtful cases (a colour edge with a luminance edge) fall back
  to the fixed estimator; a colour edge with 16 % of its band chrominance
  left standing would be a visible grid.
- *Cap.* The residue left standing from band energy the ring does not
  explain stays under the noise floor of the band:
  `M²·(E_C − leak) ≤ n_C·N_bin`.
- *0 instead of 0.5.* With no evidence the fixed estimator applies (M = 0).
  0.5 would leave half the band chrominance standing on every colour edge.
- *Erosion.* Before interpolation the block grid is eroded with a 3×3
  minimum: a block that says "chrominance" wins against its neighbours, so
  bilinear interpolation carries no M > 0 into the 32 px beside a colour
  edge (measured: colour-patch chart, carrier energy 96× noise → 0.8×
  after erosion).

Block values sit on the block centres and are interpolated bilinearly onto
pixels; outside the outermost block centres they are clamped. The block
analysis is analysis only (the window is applied once; Hann at 50 % overlap
is COLA); there is no block synthesis and therefore no block-edge artefact.
Images under 64 px in either direction run with the fixed estimator.

Two C2 copies (Dubois): `ρ_a = 1 − M_2a`, `ρ_b = 1 − M_2b`,

    Ĉ2' = (ρ_a·Ĉ2a' + ρ_b·Ĉ2b') / (ρ_a + ρ_b)      if ρ_a + ρ_b > 10⁻⁹
        = (Ĉ2a' + Ĉ2b') / 2                        otherwise (both fully masked)
    g2 = max(ρ_a, ρ_b)
    g1 = 1 − M_1

The combination applies to the estimates `Ĉ'` already decomposed by 5.3.
**[Deviation, measured]**: an earlier form with `+10⁻⁶` in the denominator
made the core of both copies vanish at `M_2a = M_2b = 1`
(`0/10⁻⁶ = 0`), so in textured areas with a colour cast the chroma DC
stayed in `L̂` as a 2 px grid (visible in the Z f test picture, on the
cast on tinted paper). With the mean, the core is always chroma, as 5.3
requires.

### 5.3 Luminance

Each chrominance estimate is split into core and band. The core is the
low-pass of `Ĉ` at 0.05 c/px (Kaiser sinc as in 5.1, transition 0.05 c/px,
computed at half resolution, since `Ĉ` is band-limited below 0.175 c/px,
and bilinearly back):

    Ĉ_k' = g_k·Ĉ_k + (1 − g_k)·Core(Ĉ_k)
    L̂ = m − ε·sx·sy·Ĉ1' − (a·sx + b·sy)·Ĉ2'

(The form `g·Ĉ + (1−g)·Core` is exact at `g = 1` and `g = 0`, so the
adaptive path is bit-identical to the fixed estimator on areas the mask
leaves alone.)

Without the adaptive stage (`--mask off`), `g1 = g2 = 1`, so `Ĉ' = Ĉ`, and
`Ĉ2 = (Ĉ2a + Ĉ2b)/2`.

### 5.4 Consistency loop

**[Deviation]**: the whitepaper names POCS. A projection onto the
measurements would write the mosaic back into `L̂`, which the whitepaper
itself forbids. What remains is the cross-term cleanup between the
carriers (Gauss–Seidel): before demodulating carrier k, the current
estimates of the other carriers' contributions are subtracted.

    Round n:
      for k in (C1, C2a, C2b):
        m_k = m − Σ_{j≠k} g_j·carrier_j·Ĉ_j        (with the latest Ĉ_j)
        Ĉ_k = LP(carrier_k · m_k)
      L̂_n by 5.3
    Stop when
      ‖L̂_n − L̂_{n−1}‖₂ / ‖L̂_{n−1}‖₂ < 10⁻⁴
      or n = consistency_rounds_max

Default `consistency_rounds_max = 1` (**[measured]**: on the bench no
further round changed a figure past the fourth digit, while the run time
doubled; `--rounds` stays as an option). The mask is not recomputed inside
the loop.

### 5.5 Pixel-adaptive separation (`--mask dubois`, the default)

**[Deviation, measured]** from 5.1–5.3 (Kodak bench, RESULTS.md, last
section): the fixed stage averages the two C2 copies, although luminance
leaks into only one of them at a time (vertical line patterns into the
copy at (0.5, 0), horizontal into the one at (0, 0.5)); the block mask
cannot tell a line pattern inside the band from chrominance (the same
band/ring ratio) and, after the erosion, collapses almost everywhere to
the mean; and the square band gives away chroma bandwidth along the
carrier, where the only neighbour is the other chroma component.
Replacement after Dubois (2005), extended to C1:

Two hypotheses with separable Kaiser-sinc low-passes (kernels as in 5.1,
`LP_n` narrow, `LP_w` wide; rows × columns):

    H (horizontal structure, spectrum along v; copy a clean):
      Ĉ2a = LP_n,x LP_w,y (a·sx·m)       Ĉ1_H = LP_n,x LP_w,y (ε·sx·sy·m)
    V (vertical structure, spectrum along u; copy b clean):
      Ĉ2b = LP_w,x LP_n,y (b·sy·m)       Ĉ1_V = LP_w,x LP_n,y (ε·sx·sy·m)

Local energies and a weight per pixel:

    e_a = G_σ * Ĉ2a²      e_b = G_σ * Ĉ2b²
    w_H = e_b / (e_a + e_b)        (0.5 if e_a + e_b ≤ 10⁻¹⁸)
    Ĉ2 = w_H·Ĉ2a + (1 − w_H)·Ĉ2b
    Ĉ1 = w_H·Ĉ1_H + (1 − w_H)·Ĉ1_V
    L̂ = m − ε·sx·sy·Ĉ1 − (a·sx + b·sy)·Ĉ2

The copy with the smaller local energy is the one to trust: the
chrominance is in both, the excess is luminance. The same weight chooses
the direction of the C1 band.

**Coherence ring** (since 0.3.0, **[measured]**, RESULTS "Coherence ring"):
the elongated band pays with chroma bandwidth everywhere, including where
no luminance lies near the carrier. Where the ring between the narrow
square band (`c2_narrow`) and a wider square band (`ring_wide`) carries
the same content in both copies, it is chrominance (luminance leaks into
only one copy) and the band may be opened to the wide square form. With
`LP_r` the square low-pass at `ring_wide`, before the copies are weighted:

    rA = LP_r,x LP_r,y (a·sx·m) − LP_n,x LP_n,y (a·sx·m)       (copy a, square wide minus narrow)
    rB = LP_r,x LP_r,y (b·sy·m) − LP_n,x LP_n,y (b·sy·m)
    κ  = clamp( G_σ*(rA·rB) / sqrt( G_σ*rA² · G_σ*rB² ), 0, 1 )^ring_power     (0 if the denominator ≤ 10⁻¹⁸)
    Ĉ2a := Ĉ2a + κ·(LP_r,x LP_r,y (a·sx·m) − Ĉ2a)       likewise Ĉ2b; then the mix with w_H
    Ĉ1  := Ĉ1  + κ·(LP_r,x LP_r,y (ε·sx·sy·m) − Ĉ1)     if ring_c1, after the directional mix

κ is the normalised cross-correlation of the two rings inside the window
G_σ: 1 for identical content, 0 for uncorrelated, and a negative
correlation becomes 0. `ring_wide` = 0 turns the step off (the 0.2.0
result).

| Parameter | Default | Meaning |
|---|---|---|
| `c1_narrow` | 0.15 c/px | C1 across the structure direction |
| `c1_wide` | 0.30 c/px | C1 along the structure direction |
| `c2_narrow` | 0.15 c/px | C2 across the carrier (towards the luminance) |
| `c2_wide` | 0.30 c/px | C2 along the carrier |
| `transition` | 0.15 c/px | transition width |
| `attenuation_db` | 40 dB | stopband attenuation |
| `energy_sigma` | 3 px | σ of the Gaussian window of the local energy and the coherence |
| `ring_wide` | 0.25 c/px | wide square band of the coherence ring; 0 = off |
| `ring_power` | 2 | exponent on κ |
| `ring_c1` | true | open C1 with κ as well |

Limit of the wide band: under V, C1 (0.5, 0.5) and Ĉ2b (0, 0.5) both want
to be wide along the same line v = 0.5 and they meet at u = 0.25; with a
transition of 0.15 they overlap from 0.35 (the measured drop is at 0.40).
The same limit applies to `ring_wide`: 0.30 loses in every condition, 0.20
still lies in the transition band of the narrow filter and measures
nothing. Flat colour field: exact (test `flat_colour_is_exact`); neutral
vertical line pattern at 0.42 c/px: under 10 % of the fixed stage's error
(test `neutral_vertical_lines_survive`). The mask of 5.2 is not computed
in this mode; `mask_max` is the constant 0.8, as with `--mask off`
(section 7). 5.3 (core/band split) and 5.4 do not apply.

Separation run time 2.17 s on 24.4 MP (without the coherence ring 1.12 s,
fixed 0.75 s, block mask 1.16 s), 3.3 s in all — **[Deviation]** from the
budget in section 12. Memory: a peak of ten full f64 planes including the
mosaic (2.0 GB measured; seven without the coherence ring): the energy and
coherence low-passes run in the buffers of their operands, with one scratch
plane for the row pass, and the mosaic's row passes share one plane.

### 5.6 Nonlinear reconstruction (`--reconstruct`)

**[Extension, measured]** (RESULTS "Reconstruct"). The library and the
command leave the step out until `--reconstruct` is set. The GUI starts
with the checkbox on (Development → *reconstruct the last octave*); turning
it off and *Apply* restores the linear path. Everything up to here is
linear: every output pixel is a sum of measured values with weights that
do not depend on the picture at that place (5.5 chooses between two such
sums, and invents none). The limit of that path is the last octave: where
luminance lies near the carriers, no filter can separate it from
chrominance. Demosaicers solve this with an assumption — the colour
differences `G − R`, `G − B` are locally smooth — and a direction decision
per pixel. This step does the same, only where the linear estimate still
carries energy near the carriers, and writes into the sidecar, per pixel,
how much of it was computed rather than measured.

Estimator (Zhang & Wu 2005, written from the paper), in the working space
`m^(1/γ)` with `γ = 2.2`:

    Direction estimate of the missing channel along x (likewise y), at every pixel:
      X̂(x) = (0.5 + A)·(X(x−1) + X(x+1)) − A·(X(x−3) + X(x+3)) + LAP·(2·O(x) − O(x−2) − O(x+2))
      A = 0.0625, LAP = 0.125 (Hamilton–Adams is A = 0, LAP = 0.25); O is the pixel's own channel
    Colour-difference signals  d_h = G − X along the row,  d_v along the column
    LMMSE per line: s = d * [4 9 15 23 26 23 15 9 4]/128 (prior),
      in the window ±4:  v_s = Var(s),  v_n = Mean((d − s)²)
      d̂ = s + v_s/(v_s + v_n)·(d − s),   error e = v_s·v_n/(v_s + v_n)
    Fusion at the R/B sites:  w_h = 1/(e_h + 10⁻¹² + μ·d̂_h²),  w_v likewise, μ = 0.01
      Ĝ = X + (w_h·d̂_h + w_v·d̂_v)/(w_h + w_v)
    R − G and B − G: at the opposite site the mean of the four diagonals, at G sites
      the mean of the two neighbours that carry the channel
    Transform back by ^γ, then L = (R + 2G + B)/4, C1 = (2G − R − B)/4, C2 = (R − B)/4

μ is the tie-breaker: where both directions are self-consistent (a neutral
Nyquist pattern, error zero on both sides), the one with less chrominance
wins (test `neutral_nyquist_lines_are_exact`); a flat colour field is exact
(`flat_colour_is_exact`). The working space `γ = 2.2` is *the* difference
from the literature, which demosaics 8-bit sRGB pictures directly: in
linear light, colour differences are not constant across an edge, and
compressed they nearly are (1× subset: 36.6 → 42.2 dB, real binned
37.5 → 38.5).

Detector (`Residue`): the linear luminance L̂ from 5.5 is demodulated with
the three carriers and low-pass filtered with `G_σ` (σ = 2 px); the sum of
the squared amplitudes against the noise variance in the same band:

    r = Σ_c ( G_σ * (c·L̂) )²  /  ( σ²(L̂) · ‖G_σ‖⁴ )
    β = clamp( (r − lo)/(hi − lo), 0, 1 ),   lo = 20, hi = 80
    L̂ := L̂ + β·(L_n − L̂),  Ĉ1, Ĉ2 likewise;  mask_max := 0.8 + 0.19·β

Where the linear separation was clean, only noise lies near the carriers
(r ≈ 1) and β = 0: the pixel stays measured. Sidecar (§7): 204 = measured,
252 = fully computed, linear in between; 255 stays saturation, and the
sharpening threshold 0.8 stays satisfied.

| Parameter | Default | Meaning |
|---|---|---|
| `detector` | `Residue` | `All` sets β = 1 everywhere (for measurement) |
| `lo`, `hi` | 20, 80 | ramp of β in units of energy over noise variance |
| `sigma` | 2 px | window of the detector |
| `mu` | 0.01 | chroma tie-breaker of the directional fusion |
| `gamma` | 2.2 | working space of the estimator; 1.0 = linear |

Rejected, measured: a detector on the difference of the two luminance
estimates (worse in every condition), on the local gradient (useless), as
a scale-free share of carrier-band over AC energy (does not distinguish
signal from noise in the carrier band; −3 dB at 1×); a 2-D error statistic
over the neighbouring lines (−0.2 dB); smoothing the colour differences as
a refinement pass (−0.7 to −1.3 dB); longer directional filters by 1-D
demodulation (equal to −0.2 dB with the LMMSE, catastrophic without). The
condition "band-limited with real noise" (`--raw --scale 2`) is why the
detector exists: there the nonlinear estimator alone is 6 dB worse than
5.5 (the smooth prior takes the content of band-limited chroma for its
own), and with the detector it is unchanged (48.56 against 48.57).

Run time: +2.2 s on 24.4 MP (separation 4.5 s instead of 2.3 s), a peak of
four more planes. The negative JSON (§7) carries `reconstruct`
(true/false) and `computed` (mean β).

## 6. Mixing

Three weights `w_R, w_G, w_B ≥ 0`, summing to 1, on *balanced* channels
(section 2). Default 0.25 / 0.50 / 0.25.

    out = L̂ + (w_G − w_R − w_B)·Ĉ1' + 2·(w_R − w_B)·Ĉ2'

(`Ĉ'` from 5.3; the weights g are already inside it.) A matrix variant
(the Y row of `cam_to_xyz`) existed; it gave way to the fitted camera file
(§10).

**Weight space [addition, measured].** The formula consumes weights on
balanced channels (`balanced`). A monochrome camera, though, has a fixed
spectral response and knows no balance; its emulation is a fixed triple on
the *raw* channels (`raw`). The two spaces are related exactly:

    w_bal,c ∝ w_raw,c / k_c        w_raw,c ∝ w_bal,c · k_c      (each normalised to sum 1)

The camera file stores fitted weights in raw space
(`weights_space = "raw"`); the development converts them per picture into
balanced space with the balance multipliers `k_c`, then mixes as above.
The negative carries both forms in its description (`weights`,
`weights_raw`). Reason (RESULTS.md): the same scene in daylight and in
tungsten — the Z f weights in balanced space jump from
(0.171/0.489/0.340) to (0.297/0.550/0.153), because the as-shot
multipliers (R 2.02→1.26, B 1.11→2.05) are folded into them; in raw space
they stay at (0.284/0.404/0.312) against (0.302/0.444/0.254). With no
statement, `balanced` applies (older files, the default triple, the CLI
override `--weights`).

**[Deviation/clarification]**: the whitepaper says "C stays outside" and
at the same time asks for three weights. Both at once work only for
0.25/0.50/0.25 (then `out = L̂` exactly). Every other weight needs the
low-pass chrominance; see the inverse in section 4. The chrominance
correction is band-limited (≤ 0.15 c/px), and the edge resolution stays
that of `L̂`.

**Contrast filters [addition].** A colour filter in front of a monochrome
sensor is a spectral weighting before the sum, which is exactly what the
mix does. A filter with channel transmissions `T_R, T_G, T_B` on the
balanced channels turns the base weights into

    w'_c = w_c · T_c / Σ_j w_j · T_j

The filter factor (the light lost) drops out through the normalisation;
digitally it costs no exposure. The base is the camera file's weights
(with no file: native). The pipeline does not change; a filter is only
another triple, and `filter` is recorded in the negative's description.
Table (`ColorFilter`), transmissions as *approximations* of typical filter
curves over typical channel bands, not measured:

| Filter | Wratten | T_R | T_G | T_B |
|---|---|---|---|---|
| yellow-8 | 8 (K2) | 1.00 | 0.90 | 0.10 |
| yellow-green-11 | 11 | 0.55 | 1.00 | 0.25 |
| orange-16 | 16 | 1.00 | 0.50 | 0.02 |
| red-25 | 25 | 1.00 | 0.08 | 0.00 |
| green-58 | 58 | 0.10 | 1.00 | 0.10 |
| blue-47 | 47 | 0.03 | 0.25 | 1.00 |

Quality (RESULTS.md, Z f): resolution and pixel noise stay those of `L̂`
for every weighting; the filter adds only band-limited chroma noise
(≤ 0.15 c/px), 19–21 % σ on smooth areas for the red filter, with
pixel-to-pixel noise unchanged. A measured variant would be fitted the way
the monochrome weights are: reference camera plus the physical filter plus
a colour chart (`calibrate weights`).

## 7. Negative file

16-bit TIFF, one channel, `PhotometricInterpretation = BlackIsZero`,
`value = round(clamp(out, 0, 1)·65535)`. Embedded ICC profile: grey,
D50 white point, `kTRC` = gamma 1.0 (linear grey). `ImageDescription`
holds JSON with the camera, the phase, the balance (`wb`), the weights in
both spaces (`weights` balanced, `weights_raw` raw, §6), the mask mode,
the round count, the noise parameters and the program version. The
orientation from the RAW is copied as the TIFF `Orientation`; the pixels
are not rotated. The source is the raw IFD; if it reports nothing rotated,
the EXIF `Orientation` applies (the Z f writes it only there). The capture
time (`DateTimeOriginal`) is stored as `captured` with the exposure.

Sidecar `<name>.mask.tif`: 8-bit, `round(255·max(M_1, M_2a, M_2b))`,
saturated photosites (dilated) overwritten with 255. The print reads it
for the sharpening threshold (§9). Without a block mask (`--mask off`, and
the default `--mask dubois`) and for monochrome sensors, the sidecar
carries 204 everywhere (= threshold 0.8, "sharpening allowed": the
carriers are suppressed by the 60 dB low-pass in every block, RESULTS
phase 3) and 255 at saturated photosites. With `--reconstruct` (5.6) it
carries `round(255·(0.8 + 0.19·β))`: 204 = measured, 252 = fully
computed; the JSON carries `reconstruct: true` and `computed` (mean β).

## 8. Measurements

- **Carrier energy** of a picture: energy of the 2-D spectrum (blockwise
  as in 5.2, averaged over all blocks) in `|f − f_k|_∞ < 0.15` for the
  three carriers, relative to the expected noise energy of the same bins.
  Pass limit on colour patches: ≤ 1.5.
- **Grey-wedge linearity**: patch means against relative exposure, a
  log-log fit; pass at slope 1.00 ± 0.02 and a maximum relative residue
  ≤ 2 %. **[Deviation]**: R² ≥ 0.98 is also passed by gamma 1.2 and by
  toes; it is not a linearity test.
- **MTF on a Siemens star**: contrast against spatial frequency, separate
  for axis-parallel (±10° about 0°/90°) and diagonal (±10° about
  45°/135°); MTF50 and MTF10 in c/px; aliasing ring = a rise in contrast
  beyond the first minimum.
- **RMSE against truth** (synthetic only): `L_true = (R + 2G + B)/4` of
  the generated scene, RMSE and the 99th percentile of the error, with a
  40 px border excluded.

No percentage without one of these measurements.

## 9. Output (print)

Order: mix → orientation → window (`crop`, whole pixels; if the field is
absent, the whole negative) → size → placing and tone (paper or look) in
one step → border → [deconvolution →] USM → file.
One pass in f64, one quantisation at the end.

- **Recipe [addition, zone-system whitepaper §5 and Appendix A]**: the
  print is a parameter set, not a sequence of edits. `Recipe`
  (`recipe.rs`, `schema: 1`) holds `place`, `curve` (the tone: the paper
  with its two exposures
  `{"paper":"multigrade","soft":s,"hard":h}`, or a look file inline;
  `tone.rs`, distinguishable in untagged JSON by the field `paper` or
  `points`), `resample`, `size`, `dpi`, `border_mm`, `crop`
  (window `x,y,w,h` in whole pixels, upright after the file orientation
  and before the quarter-turns; if absent, the whole negative),
  `sharpen` (`{"mode":"off"}` | `usm` | `deconv`). Omitted when absent are
  `place` (0), `size` (native), `border_mm` (0) and `crop` (the whole
  negative); `schema`, `resample` and `sharpen` are always in the file.
  The empty recipe `{"schema":1,"curve":<neutral>,"dpi":300}` is
  bit-identical to the run with none of these steps (acceptance test
  `recipe_acceptance.rs`); the GUI starts on the paper at its base
  exposure. The recipe is stored as JSON in the print TIFF's
  `ImageDescription` and by `--write-recipe`; `--recipe` reads it, and
  flags on the command line override single fields. The GUI history is a
  list of such states (plus weights and rotation), never of pixels.
  Nothing in the recipe reads the picture: no content-dependent mask, no
  tone equaliser (§1, §10 of the whitepaper).
- **Orientation**: the negative carries the RAW orientation only as a tag
  (§7). The print first turns the pixels and the sidecar mask upright
  (orientation 1–8); the print TIFF then has `Orientation = 1`.
- **Size**: target `--size <W>x<H><cm|mm|in>` plus `--dpi`; target pixels
  = inches·DPI, and the picture is fitted (the aspect ratio stays). The
  statement is the paper, not the picture's orientation: a landscape
  picture on "30x40cm" is fitted into 40×30 cm. With `--border-mm` the
  border is taken off the paper first (target box = paper − 2·border),
  and the picture is fitted into what remains. Kernel `--resample`
  (recipe field `resample`), separable in f64: `b-spline` (default,
  B = 1, C = 0), `lanczos3` (the kernel of the benchmark numbers),
  `mitchell` (Mitchell–Netravali B = C = ⅓) and `catmull-rom` (B = 0,
  C = ½). When reducing, the kernel is stretched by the scale factor
  (that is the low-pass beforehand), and edges are folded (weights
  outside are laid on the edge pixel, sum exactly 1). Never scale past
  the target pixels. The GUI shows the picture, not this box: the two
  centimetre fields are coupled, one side sets the other through the
  picture's aspect ratio (the window, when a `crop` is set, otherwise the
  upright negative; one scale, `round` on the side that follows), and the
  border is added outside. The recipe still stores the box, picture plus
  border, so that `layout` returns exactly those picture pixels
  (`box_for_picture_width`, `box_for_picture_height`). The command-line
  `--size` stays the box; the border is still taken off it first. The
  mask is scaled along with Lanczos-3 (it is a weight field). Without
  `--size` the negative's pixel count stays. A `crop` in the recipe
  (x, y, w, h, upright after the file orientation, before the
  quarter-turns) replaces that area with the window: whole pixels, no new
  sampling. `dpi` is in the file anyway; the GUI shows the sheet size,
  picture plus border, in cm. A pixel size (`<N>px`, the long edge)
  existed until 0.9.0 for the screen; it was removed with the screen
  output (below).
  **[Deviation from Appendix A.2]**: the "Cubic B = 0, C = 1" recommended
  there is not Catmull–Rom (that is C = ½) and overshoots an edge more
  than Lanczos-3 (RESULTS.md, table of kernel overshoot); the B-spline
  alone is free of overshoot and visibly soft for it. The kernel is
  therefore a parameter of the recipe, not a replacement of the default.
- **Placing [addition, whitepaper eq. 1]** (`--place <stops>`, recipe
  field `place`, |place| ≤ 5): `L ← L · 2^place` on the linear luminance
  after the size step, before the tone. `place = 0` leaves every value
  untouched (no arithmetic step). On paper, a placing of `p` scene stops
  is the same as `−γ_n·p` stops of time on both exposures (acceptance
  test, ±1 LSB); the GUI moves the two exposures for that and leaves
  `place` at 0.
- **Dodging and burning [Deviation from Appendix A, removed in 0.9.0]**:
  there is no local exposure. Appendix A provides shapes with one EV;
  0.7.0 and 0.8.0 had them (a disc, a line, and last a choice of filter).
  They were removed with no replacement: the print is the negative, one
  placing and two exposures — a straight print. A pixel is nowhere
  treated differently from its neighbour with the same scene value, so
  the preview, the 1:1 window, the histogram and the export are the same
  function `L ↦ Tone(L·2^place)` (`tone::render`). Recipes from older
  versions that contain `dodge_burn` are read; the field is discarded.
- **Paper [addition, whitepaper §3–4, in place of the zone curve]**
  (`--paper SOFT,HARD`, GUI "Enlarger", the only tone control there;
  `paper.rs`): there is no curve to drag. The tone of a scene value is
  what a multigrade paper shows after a **soft** exposure (filter 00) and
  a **hard** one (filter 5) through a negative of that scene —
  split-grade. The only two numbers of the tone are the two exposures in
  stops of time (f-stop timing), |t| ≤ 3; everything a darkroom can do
  with a tone scale (shift, stretch, compress, toe and shoulder) follows
  from that, and everything it cannot (a kink, a local steepening, a zone
  squeezed to nothing) cannot be expressed here. Model in log₁₀ exposure
  on the paper:
  - The negative is a straight line with `γ_n = 0.6` (normal
    development): a scene value `L` (grey card 0.18, clip 1) exposes the
    paper with `log₁₀E = log₁₀2 · (t − γ_n · log₂L)`; one stop of time is
    worth `1/γ_n` scene stops.
  - Two emulsion components. The hard one (filter 5) is a normal CDF in
    log exposure, `b = Φ((log₁₀E − μ)/σ)`, `σ` from ISO R 50 (grade 5;
    R = 100·Δlog₁₀E between 0.04 above base and 90 % of Dmax). The soft
    one (filter 00) is the measured characteristic of Ilford Multigrade
    RC Deluxe, grade 00: the logit fitted to the separated curve, and
    below it a toe that sets the range to ISO R 160. `μ` from the speed
    match: each exposure alone, at `t = 0`, puts the grey card on the ISO
    speed point, 0.6 above base.
  - Shared silver: `D/Dmax = 1 − (1 − a)(1 − b)`, Dmax = 2.0 above base.
    The soft component, with the long toe, is what the highlights see
    first; the hard, steep one fills the shadows.
  - Reflectance `10^−D` relative to the paper base, Dmax onto the black
    of the output (black-point compensation), encoding `^(1/2.2)`.
    Unexposed paper is 1, a clear negative (L = 0) is 0.
  - `t = 0` on both exposures is the **base exposure**: the pair that
    puts the grey card at D 0.67 (18 % reflectance) and the clip at
    D 0.10 (Zone VIII, a textured white), solved once from the constants
    (bisection, deterministic). It is about grade 2 over 5.2 scene stops;
    zones 0 and I lie on the paper's black. More soft light greys the
    highlights and leaves the shadows; less hard light opens the shadows
    and leaves the highlights (acceptance test: +1 stop soft moves Zone
    VII four times as far as Zone III; −1 stop hard moves Zone III ten
    times as far as Zone VII). More hard light is a harder grade (the
    ISO R of the combined curve gets smaller), more soft light a softer
    one; the GUI names the equivalent single grade (published ranges
    160, 130, 110, 90, 70, 60, 50 for 00, 0, 1, 2, 3, 4, 5) and the scene
    range.
  **[Deviation from whitepaper §3–4]**: the whitepaper describes a zone
  curve with a knot per zone, each dragged on its own. Such a curve is a
  curve editor with another face: setting each boundary on its own means
  kinks, local steep places and zones of zero width — results no paper
  delivers, and that make a picture look digitally edited. The GUI bar
  therefore only shows where the zones lie on the paper (axis = tone,
  width = share of the paper); a drag on it moves both exposures together
  so that the tone under the pointer follows the pointer
  (`PaperCurve::shift_for`, bisection). Preview and export evaluate the
  paper per pixel in f64 (the same chain, `tone::render`); the 16-bit LUT
  serves the histograms only. Not modelled, and left out on purpose: the
  film's toe (the negative is a straight line) and a development N±k (a
  second contrast parameter beside the split-grade would be double).
- **Look** (`--look <file>`, the measurement pipeline and older recipes):
  `look/*.json` with monotone knots in [0,1]², PCHIP (Fritsch–Carlson,
  monotone). `encoding = gamma22` (default): input and output encoded,
  that is `u = clamp(L,0,1)^(1/2.2)`, `v = PCHIP(u)`; `encoding = none`:
  knots on the linear axis. Preset `neutral`: pure gamma-2.2 encoding.
  Preset `reference`: fitted by `calibrate wedge` from the grey wedge of
  the reference monochrome camera (linear DNG against its own camera
  JPEG), RESULTS.md. Identity (no curve is computed) is recognised when
  the first point is (0,0), the last is (1,1) and every `x = y`. The GUI
  puts a recipe whose curve is the neutral look onto the paper at its
  base exposure; it leaves any other look as a look and offers the switch
  to paper in the Enlarger. The print TIFF gets a grey ICC with gamma 2.2.
- **Border [addition, Appendix B.3]** (`--border-mm`, recipe field
  `border_mm`): after the tone, the picture is placed in a paper field
  `round(border_mm / 25.4 · DPI)` pixels wide on each side, filled with
  paper white = unexposed paper (1.0), or, for a look, that look's value
  at linear 1 (1.0 for `neutral`); the mask is filled with 0 so no
  sharpening runs into the border. The GUI shows the border in the
  preview: what is fitted is the sheet (picture plus border), the border
  is painted in paper white, not rendered (a flat value), in the print's
  proportion `border_px / image_w`.
- **Scale compensation** (`--usm-amount`, `--deconv <N>`, recipe field
  `sharpen`): compensation of the blur the kernel adds when enlarging.
  When enlarging, `s` is the ratio of the long sides, picture pixels /
  negative pixels. For `s > 1` the resampler has a blur `σ = k · s` in
  print pixels. `k` is measured per kernel on the 10–90 % ramp of an edge
  (Lanczos-3 0.281, Catmull–Rom 0.275, Mitchell 0.330, B-spline 0.515).
  USM and Richardson–Lucy use this σ. At native size and when reducing,
  σ = 0, and the step is then bit-identical to the unsharpened print.
  USM: `out = x + amount·(x − Gaussian(σ))`, `amount` ∈ [0, 1.5], default
  from the camera file, otherwise 0. Only where the sidecar mask is
  ≥ 0.8 (204/255) **and < 1.0**: 255 marks saturated photosites. With no
  sidecar, everywhere, with a warning. Gaussian kernel, length
  2·⌈3σ⌉+1, edge reflect-101, on the encoded value after the curve.
  Deconvolution (`--deconv <N>`, N ∈ [1, 10], off without the flag; the
  mobile app starts at 3). The desktop GUI sets `sharpen` to off: the
  1:1 box, the export and a recipe saved there do not contain the step.
  The command line and the mobile app still set it. Richardson–Lucy with
  the same Gaussian PSF,

      e_{k+1} = e_k · G ⊛ ( b / (G ⊛ e_k) ),   e_0 = max(b, 0)

  on **every** pixel — the mask does not apply here. The PSF is the
  measured ramp of the resampler, not a measured lens. More than a few
  passes starts to ring. A USM of the same width may follow. The file
  description names `resample_sigma_px` and, while it is > 0,
  `deconvolution_iterations`. It also names `usm_radius_px` =
  `usm_k · d_mm · DPI` with `usm_k = 1.15·10⁻⁵` (one arcminute; 400 mm,
  300 DPI → 1.4 px). That number is only recorded; `--usm-k` and
  `--distance-mm` change it. It is not applied.
- **Screen output [Deviation, removed in 0.10.0]**: until 0.9.0 there was
  `--screen <amount>` (a 9-tap FIR as the Wiener inverse of the known
  transfer of reduction kernel × pixel aperture, gain limited to 2),
  `--size <N>px`, `-o *.jpg`, `--proof`, and in the GUI a second export
  file `<name>_2048px.jpg`. All removed. Reason: the compensation assumes
  one image pixel = one device pixel, which no HiDPI display and no
  browser keeps; the "known losses" are precisely not known on the
  viewer's screen, and they are known for the print (resampler, format,
  distance). And one recipe produced two different pictures. The print is
  one file, the TIFF. The derivation of the compensation is in RESULTS.md
  (phase 4). The mobile app still writes a full-size JPEG (its album),
  without compensation.
- **File**: 16-bit TIFF with the curve and a gamma-2.2 grey ICC (a linear
  ICC when `encoding = none`), `XResolution/YResolution` = DPI,
  `ImageDescription` = JSON with the source, the complete recipe, size
  and border in pixels, the USM parameters and the negative's
  description. The CLI writes TIFF only (`-o *.jpg` is refused);
  `print::write` can write an 8-bit grey JPEG (quality 100, one channel,
  the same grey ICC), which the mobile app uses.

## 10. Calibration files

`cameras/<clean_make>_<clean_model>.json` (lowercase, spaces → `_`):

```json
{
  "schema": 1,
  "make": "Nikon", "model": "Z f",
  "bayer_phase": "RGGB",
  "noise_a": 0.005, "noise_b": 0.0002,
  "weights_rgb": [0.25, 0.50, 0.25],
  "weights_space": "balanced",
  "usm_amount": 0.0,
  "fitted": { "noise": false, "weights": false }
}
```

Rules: `bayer_phase` must match the file, otherwise an error.
`weights_rgb` non-negative, sum 1 ± 10⁻⁶. `weights_space` is `balanced`
(default, the triple is mixed directly) or `raw` (triple on the sensor
channels, converted per picture by §6; that is how `calibrate weights`
writes). If the file is missing: the defaults above. Black level and
white level always come from the RAW file, never from the camera file.
Optional fields, written by `calibrate`: `weights_fitted_against` (the
reference file of the weight fit) and `star` (`mtf50_axis`,
`mtf50_diag`, `mtf10_axis`, `mtf10_diag` in c/px, `ring`, `source`).

### Calibration procedure (`mimizan calibrate`)

- **`wedge`**: a grey wedge as a negative (linear) and as the reference
  JPEG of the target camera. Per patch (rectangles, or a grid
  `x,y,w,h,cols,rows` with an inset) the mean in the negative `n_i` and
  in the reference `r_i` (8-bit decoded, /255). Knots
  `(n_i^(1/2.2), r_i)` at `gamma22`, sorted, patches with `|Δx| < 10⁻³`
  averaged, monotonicity forced by weighted isotonic regression
  (pool-adjacent-violators), anchors (0,0) and (1,1) added. Patches with
  > 1 % saturated photosites (sidecar = 255) are discarded. Pass: ≤ 3/255
  on the patches; the synthetic self-test additionally measures between
  the steps (hold-out) and the linearity of §8.
- **`weights`**: a colour chart as RAW and as a reference (JPEG of the
  target camera, sRGB-decoded to linear, or a list of values). Per patch,
  balanced means `(R_i, G_i, B_i)` with no saturated photosites. Model
  `s · (w_R R_i + w_G G_i + w_B B_i) ≈ ref_i`, `s` free (the exposure),
  `w ≥ 0`, `Σw = 1`. Search exhaustive on the simplex (step 0.01, refined
  to 0.001) — deterministic, no local minima. Pass: RMSE ≤ 5 % of the
  mean reference brightness (§11). The fit runs in the balanced space of
  the capture balance (`--wb`); `--write` converts the result into raw
  space with that capture's multipliers and writes it into the camera
  file as `weights_space = "raw"` (`fitted.weights = true`). With
  `--wb none` the fit is already in raw space.
- **`star`**: MTF by §8 on a negative, written into the camera file's
  `star` with `--write`; optionally set `--usm-amount`. Centre and radii
  are given, not searched for.

`look/<name>.json`:

```json
{ "schema": 1, "name": "neutral", "points": [[0.0, 0.0], [1.0, 1.0]], "encoding": "gamma22", "fitted_against": null }
```

`encoding = gamma22` means: linear input → `x^(1/2.2)` → then the knots.
`encoding = none` means: knots directly on linear.

## 11. Acceptance

| Test | Pass | Fail |
|---|---|---|
| Negative, grey wedge | log-log slope 1.00 ± 0.02; residue ≤ 2 % | a curve, a gamma or a WB in the negative |
| Re-mosaic | carrier energy of `L̂` on grey patches ≤ 1.0× noise | a visible 2 px grid |
| Colour patch | carrier energy ≤ 1.5× local noise | a visible 2 px grid |
| Look, grey wedge | ≤ 3/255 against the JPEG of the reference monochrome camera | fitted to some other JPEG |
| Weights | patch RMSE ≤ 5 % of the linear brightness of the reference monochrome camera | a matrix before the separation |
| Star | MTF50 axis-parallel and at 45° measured, no ring | a percentage without a measurement |
| Two cameras, grey | the same curve, ≤ 3/255 after a mid-grey anchor | a look per body |
| Synthetic | RMSE against `L_true` on neutral scenes ≤ 1.5·σ; MTF50 axis-parallel ≥ 0.35 c/px | — |

## 12. Performance budget

Reference: AMD Ryzen 7 5700U, 16 threads, f64 throughout. A 24 MP negative
(ingest through TIFF) ≤ 3 s, 45 MP ≤ 6 s. Memory ≤ 6 full f64 planes
(45 MP: ≤ 2.2 GB). Parallel reductions in a fixed chunk order
(deterministic, bit-exact between runs).

**[Deviation, measured]** since 0.3.0: the default path (5.5 with the
coherence ring) takes 3.3 s and ten full planes on 24 MP (2.0 GB; 45 MP
extrapolated ≈ 6 s, 3.7 GB). The budget still holds for `--mask off` and
`--mask adaptive` (2.2 s, six planes). The extra cost is the price of
+1.5 to +3.8 dB in the band-limited conditions (RESULTS "Coherence
ring"); it is accepted until a plane is saved without changing the result
bit for bit.

Implementation (**[measured]**, RESULTS "Phase 5"):

- Convolution: every output value sums its taps in the fixed order
  `t = 0..n`; 16 output values are computed at once in registers (lanes),
  and the column convolution runs in bands of 32 rows × 512 columns
  (source rows stay in L2). The carriers (±1) are applied while filling
  the row, or as a row sign in the column convolution: two row passes and
  three column passes for the three chrominance estimates, instead of
  three modulations and six passes — bit-identical to the reference
  (`lowpass_reference`, test `fused_demodulation_matches_reference`).
- Memory: mask values stay on the block grid and are sampled bilinearly
  per pixel at mix time; the core split and the Dubois combination run in
  the estimates' buffers; the mix overwrites `L̂`. Peak on the adaptive
  path: mosaic, three estimates, `L̂`, mask plane = 6 full planes.
  Allocator mimalloc (glibc returns 200 MB blocks to the kernel at once,
  and every new plane paid its page faults again).
- Determinism: parallel sums (gray-world, loop exit) are formed per row
  and added in row order; test
  `separation_is_deterministic_across_thread_counts` (16 against 3
  threads, bit-identical).
- Defect check: the median of the eight neighbours is formed only for
  pixels outside the neighbour span (identical result, no sort per
  pixel).

## 13. References

Alleysson, Süsstrunk, Hérault: Color demosaicing by estimating luminance and
opponent chromatic signals in the Fourier domain, CIC 2002; Linear
demosaicing inspired by the human visual system, IEEE TIP 2005.
Dubois: Frequency-domain methods for demosaicking of Bayer-sampled color
images, IEEE SPL 2005.
Liu, Yang, Chen: Frequency Enhancement for Image Demosaicking,
arXiv:2503.15800 (as a bound only; no network in this app).
