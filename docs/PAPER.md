# Interpolation-Free Monochrome Negatives from Bayer Mosaics by Carrier Separation

**The Mimizan algorithm — a complete description**

Dragan Bojovic, October 2026. Version of record for Mimizan 0.6.0
(`github.com/Draganito/mimizan`, GPL-3.0-or-later). Normative values are
those of `docs/SPEC.md`; where this text and the code disagree, the code
and its tests are right and this text has a bug.

---

## Abstract

A Bayer colour-filter array does not sample three colour channels; it
samples one signal in which luminance sits at base band and two chrominance
components ride on carriers at the Nyquist frequency. A monochrome
photograph needs only the luminance. Mimizan therefore never demosaics:
it demodulates and low-pass-filters the chrominance, subtracts it from the
mosaic, and keeps what is left as the negative. Every output pixel is a
weighted sum of measured sensor values with weights that do not depend on
the picture content at that pixel, so there is no interpolation, no
directional guess and no zipper. The chrominance estimate is made directional
by a two-hypothesis scheme after Dubois (2005), extended here to both
chrominance components and to a coherence test between the two carrier copies
that opens the chroma band wherever it demonstrably carries no luminance.
The weighting of the three colour responses (the "mix" — panchromatic,
filter emulation, or a fitted copy of a monochrome sensor) is applied after
separation and is band-limited, so resolution and grain are those of the
luminance estimate for every mix. An optional nonlinear step reconstructs
the last octave where luminance provably overlaps the carriers, and records
per pixel how much of the result was computed rather than measured. On a
24-picture Kodak benchmark re-mosaicked at lens-like sharpness the linear
default reaches 50.2 dB luminance PSNR against 50.6 dB for the best
demosaicer (RCD); band-limited at 2× it reaches 50.8 dB against 48.9 dB; with
the nonlinear step 51.9 dB and 52.0 dB. The whole method, including every
constant, is given here so that it can be re-implemented without access to
the source.

---

## 1. Problem and idea

A monochrome conversion of a Bayer RAW is normally made in two steps: a
demosaicer estimates R, G and B at every photosite (AMaZE, RCD, DCB, LMMSE,
AHD …), and a weighted sum of the three channels is taken. Both steps are
wrong for the task. The demosaicer solves a harder problem than the one
posed (three channels instead of one) and pays for it with directional
decisions, interpolation and their failure modes (zippers, mazes, false
colour that, after the sum, becomes false luminance texture). The weighted
sum then throws two-thirds of the computed information away.

The frequency-domain view of the mosaic (Alleysson, Süsstrunk and Hérault
2002/2005; Dubois 2005) makes the direct route obvious. Written as a single
signal, the mosaic is

$$ m(x,y) = L(x,y) + \text{(chrominance)} \times \text{(carriers at Nyquist)} , $$

so luminance is base band and the only thing that stands between the mosaic
and a monochrome picture is the chrominance on its carriers. Estimating the
chrominance well is a problem of two-dimensional low-pass filter design, not
of interpolation. Mimizan implements exactly that, with three additions that
the measurements of §8 forced:

1. the separation runs on *white-balanced* photosites, so that neutral
   subjects put nothing on the carriers and the carriers are free to carry
   luminance evidence;
2. the chrominance estimate is *directional* in the sense of Dubois: each
   chroma component is estimated under two hypotheses about the local
   orientation of structure and the two are combined by local energy;
3. a *coherence test* between the two carrier copies of the red-blue
   chrominance opens the chroma pass band wherever both copies agree, since
   luminance leakage can only ever reach one of them.

An optional fourth step (§5) leaves the linear world: where the linear
luminance still carries energy near the carriers, a directional LMMSE
estimator of the Zhang–Wu family computes the missing samples, and the
result is blended in with a per-pixel weight that is written to the output.

The remainder of the pipeline — mixing (§6) and encoding (§7) — is
deliberately simple; the method's claim rests on §§2–5.

---

## 2. Signal model

### 2.1 Conventions

$x$ is the column (0 = left), $y$ the row (0 = top). Define the two
alternating signs

$$ s_x = (-1)^x , \qquad s_y = (-1)^y . $$

Frequencies are in cycles per pixel (c/px); Nyquist is $0.5$. All image
values are `f64`, normalised so that the white level is $1.0$ and the black
level $0.0$. $\mathrm{LP}$ denotes a separable low-pass filter (rows, then
columns, same 1-D kernel unless two kernels are named).

### 2.2 The mosaic as a modulated signal

Let $R, G, B$ be the (hypothetical) full-resolution colour planes and let
$p_R, p_G, p_B \in \{0,1\}$ be the Bayer sampling functions,
$p_R + p_G + p_B = 1$. For the RGGB phase (R at even/even, B at odd/odd)

$$ p_R = \tfrac14 (1 + s_x)(1 + s_y), \qquad
   p_B = \tfrac14 (1 - s_x)(1 - s_y), \qquad
   p_G = 1 - p_R - p_B = \tfrac12 (1 - s_x s_y). $$

Expanding $m = R\,p_R + G\,p_G + B\,p_B$ and collecting terms gives

$$ m = \underbrace{\tfrac14 (R + 2G + B)}_{L}
     + s_x s_y \cdot \underbrace{\tfrac14 (R - 2G + B)}_{-C_1}
     + (s_x + s_y) \cdot \underbrace{\tfrac14 (R - B)}_{C_2} . $$

With the definitions

$$ L = \frac{R + 2G + B}{4}, \qquad
   C_1 = \frac{-R + 2G - B}{4}, \qquad
   C_2 = \frac{R - B}{4}, $$

the general form for the four Bayer phases is

$$ \boxed{\, m(x,y) = L + \varepsilon\, s_x s_y\, C_1 + C_2\,(a\, s_x + b\, s_y) \,} $$

| Phase (colour at (0,0),(1,0),(0,1),(1,1)) | $\varepsilon$ | $a$ | $b$ |
|---|---|---|---|
| RGGB | −1 | +1 | +1 |
| BGGR | −1 | −1 | −1 |
| GRBG | +1 | −1 | +1 |
| GBRG | +1 | +1 | −1 |

(For a general phase write $p_R = \tfrac14(1 + r_x s_x)(1 + r_y s_y)$ with
$r_x, r_y = \pm 1$ according to where R sits, $p_B$ with the opposite signs,
$p_G = 1 - p_R - p_B$, and read off the coefficients.)

The inverse, needed for the mix:

$$ G = L + C_1, \qquad R = L - C_1 + 2 C_2, \qquad B = L - C_1 - 2 C_2 . $$

### 2.3 Spectral picture

The carrier $s_x s_y$ moves $C_1$ to $(u, v) = (0.5, 0.5)$. The carrier
$s_x + s_y$ places *two copies* of $C_2$, at $(0.5, 0)$ (the $s_x$ copy,
"copy a") and at $(0, 0.5)$ (the $s_y$ copy, "copy b"). Each colour is
sampled on a lattice of pitch 2, so chrominance is physically band-limited
to $|f| \le 0.25$ c/px around its carrier. Luminance occupies the whole
plane; whatever of it lies within the chroma pass bands is irrecoverably
mixed with chrominance in the linear picture — that overlap is the only
information loss of the method and the subject of §4.3–4.4 and §5.

### 2.4 Scope

Input: Bayer RAW with a 2×2 pattern (RGGB, BGGR, GRBG, GBRG), integer
(≤ 16 bit) or float, and monochrome DNGs (`LinearRaw` / `BlackIsZero` with
one component, for which $L = m$ and nothing below applies). Excluded with an
error, no fallback: X-Trans, full-resolution Quad-Bayer, Foveon, Nikon
High-Efficiency NEF, files without a usable CFA description.

---

## 3. Pre-processing

### 3.1 Normalisation

Black level per photosite from the file's CFA-tile black levels
(`(y mod h)·w + (x mod w)`; a single value applies to all), white level a
single value or one per CFA colour. Crop: the manufacturer's recommended
area, else the active area, else the whole frame; the crop origin is forced
even (moved inward by one pixel if necessary) so that the phase is
preserved.

$$ m = \frac{\mathrm{raw} - \mathrm{black}}{\mathrm{white} - \mathrm{black}} . $$

Values below 0 stay negative (noise must not be clipped); values above 1
stay above 1 until the saturation rule.

### 3.2 Saturation

A photosite is saturated if $\mathrm{raw} \ge \mathrm{white}\,(1 -
\mathrm{sat\_margin})$ with $\mathrm{sat\_margin} = 0.02$. The saturation
mask is dilated by 2 px (the same-phase neighbours share the estimation
error). Saturated photosites keep their clipped value in the picture; they
are excluded from every statistic and fit and marked 255 in the mask sidecar
(§7.2).

### 3.3 Noise model

Standard deviation in normalised units at (balanced-before) raw level $s$:

$$ \sigma(s) = \mathrm{noise\_a} \sqrt{\max(s, 0)} + \mathrm{noise\_b}, \qquad
   \mathrm{noise\_a} = 0.005, \quad \mathrm{noise\_b} = 2\cdot 10^{-4} $$

until a dark-frame fit replaces the defaults (a 24-MP camera at base ISO
measures ≈ 28 DN at 14 bit; the model gives ≈ 34 DN; `noise_b` is ≈ 3 DN).
After white balance (3.5) a photosite of colour $c$ with balanced level $s$
has variance $k_c^2\,\sigma^2(s / k_c)$. The mean variance of a block is the
Bayer-weighted mean $\tfrac14 \mathrm{Var}_R + \tfrac12 \mathrm{Var}_G +
\tfrac14 \mathrm{Var}_B$ formed from the *per-channel* block means (a common
block mean underestimates red blocks by up to 15 %).

### 3.4 Defects

For each photosite $v$ take the eight same-phase neighbours (distance 2 in
$x$ and/or $y$), their median $\mathrm{med}$, minimum $\mathrm{lo}$ and
maximum $\mathrm{hi}$. If

$$ \max(v - \mathrm{hi},\ \mathrm{lo} - v) > \max\big(8\,\sigma(\mathrm{med}),\ \mathrm{hi} - \mathrm{lo}\big) $$

replace $v := \mathrm{med}$. The neighbour range $\mathrm{hi} - \mathrm{lo}$
is a model-free noise scale (≈ 3σ for eight samples) that keeps the rule
valid above the ISO the defaults describe. Comparing to the median alone
would flag one pixel at every sharp edge (8 % of all photosites on the Z f
test file); a defect lies *outside* the neighbour range, an edge does not.
Saturated photosites and their neighbours are exempt.

### 3.5 Channel balance

Each photosite is multiplied by the multiplier of its colour,
$k_R, k_G = 1, k_B$: by default the as-shot white balance of the file
normalised to green; alternatives are grey-world ($k_c = \bar G / \bar c$
over unsaturated photosites), none, or three given numbers.

This is a deliberate departure from the original idea of separating on raw
levels. On raw levels a neutral subject is *not* neutral — $C_1, C_2 \propto
L$ — and the whole luminance spectrum sits as a copy on every carrier; any
adaptive stage then has to call everything chrominance and is useless. With
balance the carriers of neutral subjects are empty and the directional
estimator of §4.3 can leave luminance alone (Siemens star MTF10 0.405 →
0.451, the true value). All of §§4–6 operate on balanced channels.

---

## 4. Linear separation (the default path)

### 4.1 The low-pass kernel

All separation filters are separable Kaiser-windowed sincs with three
parameters: cutoff $f_c$ (−6 dB point), transition width $\Delta f$ and
stop-band attenuation $A$ in dB.

$$ \beta = \begin{cases}
  0.1102\,(A - 8.7) & A > 50 \\
  0.5842\,(A - 21)^{0.4} + 0.07886\,(A - 21) & 21 \le A \le 50 \\
  0 & A < 21
\end{cases} $$

$$ N = \left\lceil \frac{A - 8}{2.285 \cdot 2\pi\,\Delta f} \right\rceil , \quad \text{rounded up to odd}; \qquad
   M = \frac{N - 1}{2} . $$

For $i = 0 \dots N-1$, $t = i - M$:

$$ h_i = \mathrm{sinc}_{f_c}(t) \cdot \frac{I_0\!\left(\beta \sqrt{1 - (2t/(N-1))^2}\right)}{I_0(\beta)},
   \qquad \mathrm{sinc}_{f_c}(t) = \begin{cases} 2 f_c & t = 0 \\ \dfrac{\sin(2\pi f_c t)}{\pi t} & t \ne 0 \end{cases} $$

($I_0$ the modified Bessel function of order zero, power series
$\sum_k (x^2/4)^k / (k!)^2$).

**Exact DC and Nyquist constraints.** The kernel is then corrected by a
constant plus an alternating constant so that its sum is exactly 1 and its
response at 0.5 c/px is exactly 0. With $S = \sum_i h_i$ and
$T = \sum_i (-1)^i h_i$ (for odd $N$, $\sum_i (-1)^i = 1$), solve

$$ \begin{pmatrix} N & 1 \\ 1 & N \end{pmatrix}
   \begin{pmatrix} \alpha \\ \gamma \end{pmatrix} =
   \begin{pmatrix} 1 - S \\ -T \end{pmatrix}, \qquad
   h_i := h_i + \alpha + (-1)^i \gamma . $$

Because the carriers sit exactly at Nyquist, this makes a flat colour field
separate into a flat luminance with *no* 2-px residue at all; without the
correction every flat colour field shows a faint checkerboard at the level
of the window's leakage.

Borders are mirrored without repeating the edge pixel ("reflect-101":
… 2 1 | 0 1 2 … n−2 n−1 | n−2 n−3 …).

Two kernel sets are used:

| Set | $f_c$ | $\Delta f$ | $A$ | $\beta$ | $N$ |
|---|---|---|---|---|---|
| fixed estimator (4.2), chroma core (Appendix A) | 0.15 | 0.05 | 60 dB | 5.65 | 73 |
| directional estimator (4.3–4.4) | 0.15 / 0.30 / 0.25 | 0.15 | 40 dB | 3.40 | 15 |

The separable product has a *square* pass band ($|f_x| < f_c$ and
$|f_y| < f_c$). For $f_c = 0.15$ the corners lie at 0.21 c/px, inside the
physical chroma limit of 0.25.

### 4.2 The fixed estimator (`--mask off`)

Demodulate each carrier and low-pass:

$$ \hat C_1 = \mathrm{LP}(\varepsilon\, s_x s_y\, m), \qquad
   \hat C_{2a} = \mathrm{LP}(a\, s_x\, m), \qquad
   \hat C_{2b} = \mathrm{LP}(b\, s_y\, m), \qquad
   \hat C_2 = \tfrac12 (\hat C_{2a} + \hat C_{2b}) , $$

$$ \hat L = m - \varepsilon\, s_x s_y\, \hat C_1 - (a\, s_x + b\, s_y)\, \hat C_2 . $$

This is the baseline of Alleysson et al. in space-domain form (the carriers
are fixed, so the "full FFT" of the original idea is a convolution with a
fixed kernel: faster, deterministic, identical up to rounding). It is kept as
an option and as the reference that the tests compare against. Its defect,
measured in §8, is that it averages the two $C_2$ copies although luminance
only ever leaks into one of them, and that it cannot widen the chroma band
along the carrier where the only neighbour is the other chroma component.

### 4.3 The directional estimator (default, `--mask dubois`)

Two hypotheses about the local structure, each with its own pair of
separable low-passes. $\mathrm{LP}_n$ is the narrow kernel ($f_c = 0.15$),
$\mathrm{LP}_w$ the wide one ($f_c = 0.30$), both with $\Delta f = 0.15$,
$A = 40$ dB; subscripts $x, y$ say along which axis the kernel runs.

**Hypothesis H** — horizontal structure, luminance spectrum extended along
$v$, so copy b at $(0, 0.5)$ is contaminated and copy a at $(0.5, 0)$ is
clean. Bands narrow in $u$ (towards base band), wide in $v$ (along the
carrier line):

$$ \hat C_{2a} = \mathrm{LP}_{n,x}\, \mathrm{LP}_{w,y}\,(a\, s_x\, m), \qquad
   \hat C_{1,H} = \mathrm{LP}_{n,x}\, \mathrm{LP}_{w,y}\,(\varepsilon\, s_x s_y\, m). $$

**Hypothesis V** — vertical structure, spectrum extended along $u$, copy b
clean:

$$ \hat C_{2b} = \mathrm{LP}_{w,x}\, \mathrm{LP}_{n,y}\,(b\, s_y\, m), \qquad
   \hat C_{1,V} = \mathrm{LP}_{w,x}\, \mathrm{LP}_{n,y}\,(\varepsilon\, s_x s_y\, m). $$

**Local energies and weight.** With $G_\sigma$ a normalised separable
Gaussian of $\sigma = 3$ px (half-length $\lceil 3\sigma \rceil$):

$$ e_a = G_\sigma * \hat C_{2a}^2, \qquad e_b = G_\sigma * \hat C_{2b}^2, \qquad
   w_H = \begin{cases} e_b / (e_a + e_b) & e_a + e_b > 10^{-18} \\ 0.5 & \text{otherwise} \end{cases} $$

The copy with the *smaller* local energy is the trustworthy one: the
chrominance is in both, the excess is luminance. The same weight picks the
orientation of the $C_1$ band:

$$ \hat C_2 = w_H\, \hat C_{2a} + (1 - w_H)\, \hat C_{2b}, \qquad
   \hat C_1 = w_H\, \hat C_{1,H} + (1 - w_H)\, \hat C_{1,V}, $$

$$ \boxed{\, \hat L = m - \varepsilon\, s_x s_y\, \hat C_1 - (a\, s_x + b\, s_y)\, \hat C_2 \,} $$

(The energies $e_a, e_b$ are computed from the estimates *before* the
coherence opening of 4.4; the opening is applied to $\hat C_{2a}, \hat C_{2b}$
before they are blended with $w_H$.)

The limit of the wide band: under V, $C_1$ at $(0.5, 0.5)$ and $\hat C_{2b}$
at $(0, 0.5)$ both want to be wide along the same line $v = 0.5$ and meet at
$u = 0.25$; with the 0.15 transition they overlap from 0.35 on (measured
dip at 0.40). Hence `wide` = 0.30 and not more.

Properties verified by unit tests: a flat colour field separates exactly
(`flat_colour_is_exact`); a neutral vertical line pattern at 0.42 c/px
survives with under 10 % of the error of the fixed estimator
(`neutral_vertical_lines_survive`).

### 4.4 The coherence ring

The elongated bands of 4.3 pay with chroma bandwidth everywhere, also where
no luminance lies near the carrier. Luminance can leak into only one $C_2$
copy; chrominance is in both. So where the *ring* between the narrow square
band and a wider square band carries the same content in both copies, that
ring is chrominance and the band may be opened to the wide square shape.

Let $\mathrm{LP}_r$ be the square low-pass at `ring_wide` = 0.25 c/px (same
$\Delta f$, $A$). Before the weighting of the copies:

$$ r_A = \mathrm{LP}_{r,x}\mathrm{LP}_{r,y}(a\, s_x\, m) - \mathrm{LP}_{n,x}\mathrm{LP}_{n,y}(a\, s_x\, m), \qquad
   r_B = \mathrm{LP}_{r,x}\mathrm{LP}_{r,y}(b\, s_y\, m) - \mathrm{LP}_{n,x}\mathrm{LP}_{n,y}(b\, s_y\, m), $$

$$ \kappa = \mathrm{clamp}\!\left( \frac{G_\sigma * (r_A\, r_B)}{\sqrt{(G_\sigma * r_A^2)\,(G_\sigma * r_B^2)}},\ 0,\ 1 \right)^{\!2}
   \qquad (\kappa = 0 \text{ if the denominator} \le 10^{-18}), $$

i.e. the normalised cross-correlation of the two rings in the window
$G_\sigma$ ($\sigma = 3$ px, the same window as 4.3), negative correlation
set to 0, squared (`ring_power` = 2). Then

$$ \hat C_{2a} := \hat C_{2a} + \kappa\,\big(\mathrm{LP}_{r,x}\mathrm{LP}_{r,y}(a\, s_x\, m) - \hat C_{2a}\big), $$
$$ \hat C_{2b} := \hat C_{2b} + \kappa\,\big(\mathrm{LP}_{r,x}\mathrm{LP}_{r,y}(b\, s_y\, m) - \hat C_{2b}\big), $$

followed by the $w_H$ blend of 4.3, and, after the directional blend of
$C_1$ (`ring_c1` = true),

$$ \hat C_1 := \hat C_1 + \kappa\,\big(\mathrm{LP}_{r,x}\mathrm{LP}_{r,y}(\varepsilon\, s_x s_y\, m) - \hat C_1\big). $$

`ring_wide` = 0.25 is the only working value: 0.30 loses in every
condition (same overlap argument as above), 0.20 lies inside the transition
band of the narrow filter and measures nothing. `ring_wide` = 0 switches the
step off and reproduces the 0.2.0 result.

### 4.5 Parameters of the default path

| Parameter | Default | Meaning |
|---|---|---|
| `c1_narrow` | 0.15 c/px | $C_1$ across the structure direction |
| `c1_wide` | 0.30 c/px | $C_1$ along the structure direction |
| `c2_narrow` | 0.15 c/px | $C_2$ across the carrier (towards luminance) |
| `c2_wide` | 0.30 c/px | $C_2$ along the carrier |
| `transition` | 0.15 c/px | transition width of all four |
| `attenuation_db` | 40 dB | stop-band attenuation |
| `energy_sigma` | 3 px | Gaussian window of the local energies and of $\kappa$ |
| `ring_wide` | 0.25 c/px | square wide band of the coherence ring; 0 = off |
| `ring_power` | 2 | exponent on $\kappa$ |
| `ring_c1` | true | open $C_1$ with $\kappa$ too |

Cost on a 24.4-MP file (Ryzen 7 5700U, 16 threads, f64): separation 2.17 s
(1.12 s without the ring, 0.75 s for the fixed estimator), 3.3 s from file
to TIFF; peak ten full f64 planes including the mosaic (2.0 GB).

### 4.6 What the linear path guarantees

Every operation in §4 is a convolution with a fixed kernel, a per-pixel
product, or a per-pixel convex combination of two such results with weights
that depend on *local energy*, never on the value being estimated. Hence:

- a flat field of any colour gives a flat $\hat L$ (exact, to rounding);
- a neutral subject gives $\hat L = m$ up to the noise the chroma filters
  remove from the carriers (the filters have zero response at the carriers,
  so the mosaic's own 2-px structure cannot survive);
- the resolution of $\hat L$ is that of the sensor: nothing is interpolated,
  the MTF is the lens–sensor MTF minus whatever luminance fell inside the
  chroma pass bands;
- the error, where it exists, is a periodic residue at the carrier
  frequencies (a faint 2-px grid), never a zipper, maze or smeared line.

---

## 5. Nonlinear reconstruction of the last octave (`--reconstruct`, off by default)

Where luminance lies near the carriers, no linear filter can separate it
from chrominance. Demosaicers resolve this with an assumption — colour
differences $G - R$, $G - B$ are locally smooth — and a per-pixel direction
decision. This optional step does the same, but only where the linear
estimate still carries energy near the carriers, and it writes per pixel
how much of the result is computed rather than measured.

### 5.1 Estimator (Zhang & Wu 2005, written from the paper)

Working domain $m^{1/\gamma}$ with $\gamma = 2.2$ — the one decisive
difference to the literature, which demosaics 8-bit sRGB pictures and never
had to notice: in linear light colour differences are not constant across
an edge; in a compressed domain they nearly are (1× subset 36.6 → 42.2 dB,
real binned 37.5 → 38.5 dB).

Directional estimate of the missing channel $X$ along $x$ (and likewise
along $y$), at every pixel, with $O$ the channel present at that pixel:

$$ \hat X(x) = (0.5 + A)\,\big(X(x-1) + X(x+1)\big) - A\,\big(X(x-3) + X(x+3)\big)
   + \mathrm{LAP}\,\big(2\,O(x) - O(x-2) - O(x+2)\big), $$
$$ A = 0.0625, \qquad \mathrm{LAP} = 0.125 \qquad (\text{Hamilton–Adams would be } A = 0,\ \mathrm{LAP} = 0.25). $$

Colour-difference signals $d_h = G - X$ along the row and $d_v$ along the
column (at G sites $G - \hat X$, at R/B sites $\hat G - X$). Per line, LMMSE
with a smoothing prior:

$$ s = d * \tfrac{1}{128}[4\ 9\ 15\ 23\ 26\ 23\ 15\ 9\ 4], \qquad
   v_s = \mathrm{Var}_{\pm 4}(s), \qquad v_n = \mathrm{Mean}_{\pm 4}\big((d - s)^2\big), $$
$$ \hat d = s + \frac{v_s}{v_s + v_n}\,(d - s), \qquad e = \frac{v_s\, v_n}{v_s + v_n}. $$

Fusion at the R/B sites with $\mu = 0.01$:

$$ w_h = \frac{1}{e_h + 10^{-12} + \mu\, \hat d_h^{\,2}}, \qquad w_v \text{ likewise}, \qquad
   \hat G = X + \frac{w_h\, \hat d_h + w_v\, \hat d_v}{w_h + w_v}. $$

$\mu$ is the tie-breaker: where both directions are self-consistent (a
neutral Nyquist pattern, zero error both ways) the one with less
chrominance wins (test `neutral_nyquist_lines_are_exact`). $R - G$ and
$B - G$: at the opposite site the mean of the four diagonal differences,
at G sites the mean of the two neighbours that carry the channel. Decode
with $(\cdot)^\gamma$, then

$$ L_n = \frac{R + 2G + B}{4}, \qquad C_{1,n} = \frac{2G - R - B}{4}, \qquad C_{2,n} = \frac{R - B}{4}. $$

### 5.2 Detector and blend

The linear luminance $\hat L$ of §4 is demodulated by the three carriers
and low-passed with a Gaussian $G_\sigma$, $\sigma = 2$ px; the summed
squared amplitudes are compared with the noise variance in the same band:

$$ r = \frac{\sum_{c \in \{\varepsilon s_x s_y,\ a s_x,\ b s_y\}} \big(G_\sigma * (c\, \hat L)\big)^2}
            {\sigma^2(\hat L)\, \|G_\sigma\|_2^2}, \qquad
   \|G_\sigma\|_2^2 = \Big(\sum_i g_i^2\Big)^2 \text{ for the separable 2-D kernel}, $$

$$ \beta = \mathrm{clamp}\!\left( \frac{r - \mathrm{lo}}{\mathrm{hi} - \mathrm{lo}},\ 0,\ 1 \right), \qquad \mathrm{lo} = 20,\ \mathrm{hi} = 80, $$

$$ \hat L := \hat L + \beta\,(L_n - \hat L), \qquad \hat C_1, \hat C_2 \text{ likewise}, \qquad
   \mathrm{mask\_max} := 0.8 + 0.19\,\beta . $$

Where the linear separation was clean, only noise remains near the carriers
($r \approx 1$) and $\beta = 0$: the pixel stays measured. The sidecar (§7.2)
carries 204 = measured, 252 = fully computed, linear in between. The
negative's description records `reconstruct: true` and `computed` (mean
$\beta$).

Rejected by measurement (§8.4): a detector on the difference of the two
luminance estimates, on the local gradient, or as a scale-free share of
carrier-band in local AC energy; a 2-D error statistic over neighbouring
lines; colour-difference smoothing as a refinement; longer directional
filters by 1-D demodulation. The detector itself is not optional: on
band-limited content with real noise the nonlinear estimate alone is 6 dB
worse than §4 (the smooth prior takes band-limited chroma for noise); with
the detector the condition is unchanged within 0.01 dB.

Cost: +2.2 s on 24.4 MP, four more planes at the peak.

---

## 6. Mixing

### 6.1 Weights

Three weights $w_R, w_G, w_B \ge 0$, $\sum w = 1$, on *balanced* channels;
default $0.25 / 0.50 / 0.25$. From the inverse of §2.2,

$$ \boxed{\, \mathrm{out} = \hat L + (w_G - w_R - w_B)\, \hat C_1 + 2\,(w_R - w_B)\, \hat C_2 \,} $$

For $0.25/0.50/0.25$ the chroma terms vanish and $\mathrm{out} = \hat L$
exactly; every other weight needs the (band-limited, ≤ 0.15–0.30 c/px)
chrominance, so the edge resolution and the pixel-to-pixel noise remain
those of $\hat L$ for every mix and a filter adds only band-limited chroma
noise (measured on the Z f: 19–21 % σ on smooth areas for the red filter,
pixel-to-pixel noise unchanged). The "matrix" variant takes $w$ as the Y
row of the camera's `cam_to_xyz`, divided per channel by $k_c$, normalised.

### 6.2 Weight spaces

The formula consumes weights on balanced channels. A monochrome camera has a
fixed spectral response and knows no balance; its emulation is a fixed
triple on the *raw* channels. The two spaces are related exactly by the
balance multipliers,

$$ w_{\mathrm{bal},c} \propto \frac{w_{\mathrm{raw},c}}{k_c}, \qquad
   w_{\mathrm{raw},c} \propto w_{\mathrm{bal},c}\, k_c , $$

each normalised to sum 1. Camera files store fitted weights in raw space
and the development converts them per picture. (Z f, same scene, daylight vs
tungsten: balanced-space weights jump from 0.171/0.489/0.340 to
0.297/0.550/0.153 because the as-shot multipliers are folded in; raw-space
weights stay at 0.284/0.404/0.312 vs 0.302/0.444/0.254.)

### 6.3 Contrast filters

A colour filter in front of a monochrome sensor is a spectral weighting
before the sum — exactly what the mix does. With channel transmissions
$T_R, T_G, T_B$ on balanced channels the base weights become

$$ w'_c = \frac{w_c\, T_c}{\sum_j w_j\, T_j}. $$

The filter factor cancels in the normalisation; digitally it costs no
exposure. Transmissions are approximations of typical curves over typical
channel bands, not measurements:

| Filter | Wratten | $T_R$ | $T_G$ | $T_B$ |
|---|---|---|---|---|
| yellow-8 | 8 (K2) | 1.00 | 0.90 | 0.10 |
| yellow-green-11 | 11 | 0.55 | 1.00 | 0.25 |
| orange-16 | 16 | 1.00 | 0.50 | 0.02 |
| red-25 | 25 | 1.00 | 0.08 | 0.00 |
| green-58 | 58 | 0.10 | 1.00 | 0.10 |
| blue-47 | 47 | 0.03 | 0.25 | 1.00 |

### 6.4 Fitting the weights to a monochrome reference

Colour chart as RAW and as the reference camera's JPEG (sRGB decoded to
linear) or a list of values. Per patch, balanced means $(R_i, G_i, B_i)$
without saturated photosites. Model $s\,(w_R R_i + w_G G_i + w_B B_i) \approx
\mathrm{ref}_i$ with free exposure scale $s$, $w \ge 0$, $\sum w = 1$;
exhaustive search on the simplex (step 0.01, refined to 0.001):
deterministic, no local minima. Pass: RMSE ≤ 5 % of the mean reference
brightness. The result is converted to raw space with the multipliers of the
calibration shot and written to the camera file.

---

## 7. The negative

### 7.1 File

16-bit TIFF, one channel, `PhotometricInterpretation = BlackIsZero`,
$\mathrm{value} = \mathrm{round}(\mathrm{clamp}(\mathrm{out}, 0, 1)\cdot 65535)$.
Embedded ICC: grey, D50, `kTRC` = gamma 1.0 (linear grey). `ImageDescription`
holds JSON with camera, phase, balance, weights in both spaces, mask mode,
rounds, noise parameters, `reconstruct`/`computed`, program version. The RAW
orientation is carried as the TIFF `Orientation` tag; pixels are not turned
(source: the raw IFD; if it reports nothing turned, the EXIF orientation).

### 7.2 Mask sidecar

`<name>.mask.tif`, 8 bit. In the default path it is 204 everywhere
(= threshold 0.8, "sharpening allowed": the carriers are suppressed by the
low-pass in every block) and 255 at dilated saturated photosites. With
`--reconstruct` it is $\mathrm{round}(255\,(0.8 + 0.19\,\beta))$. The print
stage sharpens only where $0.8 \le \mathrm{mask} < 1.0$.

### 7.3 Output stage (for completeness)

Order: mix → size → curve → [deconvolution →] USM *or* screen
compensation → file. Resizing is separable Lanczos-3 in f64 with the kernel
stretched by the scale factor when shrinking. The curve is a monotone PCHIP
through points in $[0,1]^2$ after $x^{1/2.2}$ encoding (`neutral` is pure
gamma 2.2; `reference` is fitted from a grey wedge against a monochrome
camera's JPEG by weighted isotonic regression). USM radius
$r_{\mathrm{px}} = 1.15\cdot 10^{-5}\, d_{\mathrm{mm}}\, \mathrm{DPI}$ (one
arc-minute at viewing distance $d$), Gaussian $\sigma = r_{\mathrm{px}}$,
masked as in 7.2. Deconvolution is Richardson–Lucy with the same Gaussian
PSF, $e_{k+1} = e_k \cdot G \circledast \big(b / (G \circledast e_k)\big)$,
$e_0 = \max(b, 0)$, 1–10 passes, every pixel, before the USM. Screen
compensation is a 9-tap least-squares FIR fit to the Wiener inverse
$T(f) = (1+\epsilon) M(f) / (M(f)^2 + \epsilon)$, $\epsilon = 1/(4 g_{\max}^2)$,
$g_{\max} = 2$, of the known Lanczos × pixel-aperture transfer
$M(f) = L(f)\,\mathrm{sinc}(f)$, weight $1/(f + 0.1)$ over $f \in [0, 0.5]$,
normalised to unit DC gain. None of this is part of the method's claim.

---

## 8. Evaluation

### 8.1 Benchmark

Kodak set (24 pictures). 8-bit sRGB → linear → RGGB mosaic → 16 bit. The
same samples go to Mimizan (`separate`, native mix) and, as a minimal CFA
DNG (ColorMatrix1 = XYZ→sRGB, AsShotNeutral 1,1,1), to libraw 0.21
(bilinear, AHD, DHT, AMaZE) and RawTherapee 5.11 (RCD); their RGB is reduced
with $(R + 2G + B)/4$. Ground truth is the same luminance of the linear
source. PSNR on sRGB-encoded 0..255 values, 16-px border excluded. Converter
chains were verified on a smooth synthetic picture (every method ≥ 59 dB:
no tone curve, scaling or offset is left in the external path).

Conditions: **pixel-sharp** (the Kodak pixels as they are, content sharper
than any lens delivers), **lens-like** (Gaussian σ = 0.75 px before
mosaicking), **band-limited 2×** (picture upscaled 2× before mosaicking), and
two **real** conditions from Nikon Z f NEFs: every 2×2 cell binned to one
RGB pixel, the 3024×2016 result is the truth and is re-mosaicked with the
camera's noise model, at 1× (twice as sharp as the camera saw it) and
upscaled 2× ("soft", the camera's own band limit).

### 8.2 Linear path

Mean luminance PSNR (dB):

| Condition | fixed (4.2) | Dubois 0.2.0 (4.3) | **+ ring (4.4), default** | best demosaicer |
|---|---|---|---|---|
| pixel-sharp (24) | 34.60 | 36.45 | **36.55** | AMaZE 40.72, AHD 38.71 |
| lens-like σ 0.75 (24) | 43.95 | – | **50.21** | RCD 50.59, AMaZE 50.50 |
| band-limited 2× (24) | 44.02 | 49.32 | **50.80** | RCD 48.88 |
| Z f binned (4) | 37.22 | 40.29 | **40.38** | RCD 42.81, AMaZE 42.10 |
| Z f binned, ×2 (4) | 42.61 | 48.16 | **51.92** | RCD 50.66, AMaZE 48.58 |

Per picture: at 2× every picture gains over 0.2.0 (0.26 to 4.87 dB) and 23
of 24 are above every demosaicer (kodim19, the fence, 0.5 dB below RCD);
lens-like, 6 of 24 beat every demosaicer, the largest gap is kodim19
(−4.4 dB to AMaZE); on the real files at the camera's own band limit three of
four are above RCD (+0.85, +3.35, +1.00 dB), NikonZF 0.13 below. The binned
real files at twice the camera's sharpness behave like the pixel-sharp set,
2.4 dB behind RCD.

Why the fixed estimator trailed by 4–6 dB — three defects, all in the
separation: (1) the two $C_2$ copies were averaged although luminance leaks
into only one; (2) an earlier block-wise texture/chroma mask (Appendix A)
could not distinguish a line pattern inside the band from chrominance and
reduced to the average almost everywhere; (3) the square band wasted chroma
bandwidth along the carrier. 4.3 fixes (1) and (2), 4.4 fixes (3).

Negative results kept for the record: cross-term rounds (re-estimate
$\hat L$, demodulate, subtract the estimated leak from the copies,
re-weight): −1 dB at 2×, none at 1×. Noise-energy subtraction in the
weights: no effect in any condition.

### 8.3 With reconstruction

| Condition | linear default | **+ reconstruct** | computed share | best demosaicer | at or above the best |
|---|---|---|---|---|---|
| pixel-sharp (24) | 36.55 | **42.04** | 49 % | AMaZE 40.72 | 0 → 22 of 24 |
| lens-like σ 0.75 (24) | 50.21 | **51.93** | 12 % | RCD 50.59 | 6 → 21 of 24 |
| band-limited 2× (24) | 50.80 | **51.96** | 5 % | RCD 48.88 | 23 → 24 of 24 |
| Z f binned (4) | 40.38 | **41.67** | 10 % | RCD 42.81 | 0 → 0 of 4 |
| Z f binned, ×2 (4) | 51.92 | 51.91 | 0.5 % | RCD 50.66 | 3 → 3 of 4 |

$\beta = 0$ reproduces the linear default bit for bit. On the real files at
twice the camera's sharpness the method stays 0.7–1.8 dB below RCD even with
$\beta = 1$ everywhere (38.52 vs 39.49): on real noise the estimator, not
the detector, is the limit. At the camera's own band limit the step is
inactive (0.5 %).

### 8.4 Design sweep for the nonlinear step (subset)

| Variant | 1× | 2× | raw | soft | lens |
|---|---|---|---|---|---|
| linear default | 34.59 | 47.56 | 36.66 | 48.57 | 49.84 |
| nonlinear everywhere, linear domain | 36.57 | 44.06 | 36.99 | 41.24 | 46.36 |
| nonlinear everywhere, domain $^{1/2.2}$ | 42.17 | 49.04 | 38.17 | 41.33 | 51.12 |
| — same, $A$ 0.0625, LAP 0.125 | 41.94 | 49.64 | 38.52 | 42.35 | 52.16 |
| hybrid, residue detector 100/400 | 39.56 | 49.53 | 38.09 | 48.60 | 51.19 |
| **hybrid, residue detector 20/80** | **40.67** | **49.85** | 38.46 | **48.56** | 52.13 |
| hybrid, residue detector 10/40 | 41.08 | 49.86 | **38.54** | 48.30 | **52.40** |
| hybrid, share detector 0.001/0.005 | 37.53 | 47.58 | 37.61 | 48.54 | 50.36 |
| best demosaicer on the subset | AMaZE 38.44 | RCD 47.02 | RCD 39.49 | – | AMaZE 51.22 |

20/80 was chosen over 10/40 for the soft condition, the only one where the
nonlinear step can only lose.

### 8.5 Other measurements

- Siemens star on the Z f: MTF10 0.405 c/px separating on raw levels,
  0.451 on balanced levels (= the true value of the chart).
- Weighting does not change resolution or pixel-to-pixel noise; a filter
  adds band-limited chroma noise only (red filter 19–21 % σ on smooth areas).
- Synthetic bench (`measure synth --size 1024`, RMSE/σ): star 3.77 (MTF10
  0.447), zone plate 10.05, edge 0.90, patches 2.87, wedge 0.96, colour
  fabric 0.86 (2.76 before the coherence ring).
- Determinism: results are bit-identical between 16 and 3 threads
  (`separation_is_deterministic_across_thread_counts`).

---

## 9. Implementation notes

- All arithmetic in f64; the mosaic and all estimates are full planes.
- Convolutions sum their taps in fixed index order $t = 0 \dots N-1$; 16
  outputs are computed together in registers (lanes), the column pass in
  bands of 32 rows × 512 columns so the source rows stay in L2.
- The carriers ($\pm 1$) are applied while filling a row or as row signs in
  the column pass: two row passes and three column passes serve the three
  chrominance estimates instead of three modulations and six passes;
  bit-identical to the plain reference (`fused_demodulation_matches_reference`).
- Parallel reductions (grey world, convergence tests) are formed per row
  and added in row order.
- The defect test forms the median only for pixels outside the neighbour
  range (identical result, no per-pixel sort).
- Allocator: mimalloc (glibc returns 200-MB blocks to the kernel at once and
  every new plane paid its page faults again).

---

## 10. Limitations

- The information loss of the linear path is the luminance inside the chroma
  pass bands; it shows as a faint periodic residue on achromatic Nyquist
  texture. The directional estimator and the coherence ring shrink that
  region; §5 fills it with an assumption and says so per pixel.
- On real sensor noise at the sensor's pixel count the method is 1–2 dB
  behind the best demosaicer (RCD) in luminance PSNR. The gap closes at the
  camera's own band limit (lens plus AA/pixel aperture), which is the
  condition real pictures are in.
- Filter transmissions are approximations; a measured variant would be
  fitted like the monochrome weights.
- The noise model defaults describe a 24-MP camera at base ISO; the defect
  rule's model-free fallback keeps it safe, the detector of §5.2 scales
  with it.
- Only 2×2 CFAs. X-Trans has carriers too, but at other positions with other
  copies; it is not implemented.

---

## References

- D. Alleysson, S. Süsstrunk, J. Hérault, "Color demosaicing by estimating
  luminance and opponent chromatic signals in the Fourier domain", CIC 2002;
  "Linear demosaicing inspired by the human visual system", IEEE Trans.
  Image Processing 14(4), 2005.
- E. Dubois, "Frequency-domain methods for demosaicking of Bayer-sampled
  color images", IEEE Signal Processing Letters 12(12), 2005.
- L. Zhang, X. Wu, "Color demosaicking via directional linear minimum mean
  square-error estimation", IEEE Trans. Image Processing 14(12), 2005.
- J. F. Kaiser, "Nonrecursive digital filter design using the I0-sinh window
  function", ISCAS 1974 (the $\beta$ and $N$ formulas of §4.1).
- W. H. Richardson, "Bayesian-based iterative method of image restoration",
  JOSA 62(1), 1972; L. B. Lucy, Astron. J. 79, 1974.
- Liu, Yang, Chen, "Frequency Enhancement for Image Demosaicking",
  arXiv:2503.15800 (as a bound only; no network in this method).
- D. Bojovic, `docs/whitepaper_v1.pdf` (the original idea), `docs/SPEC.md`
  (normative constants, German), `docs/RESULTS.md` (all measurements).

---

## Appendix A. The superseded block mask (`--mask adaptive`)

Kept as an option and documented because it is the honest account of a
failed design. Blocks 64×64, step 32, 2-D Hann, 2-D FFT in f64, block mean
removed before the window. Per block and carrier $k \in \{C_1, C_{2a},
C_{2b}\}$, from the block spectrum $M(f)$ of $m$ (sup-norm distances, matching
the square pass band):

| Region | Distance to carrier | Role |
|---|---|---|
| core | $\|f - f_k\|_\infty < 0.05$ | always chrominance (block hue, colour edges) |
| band | $0.05 \le \|f - f_k\|_\infty < 0.15$ | contested: $P_C = \sum \|M\|^2$ |
| ring | $0.15 \le \|f - f_k\|_\infty < 0.35$ | luminance evidence: $P_L = \sum \|M\|^2$ |

Expected noise energy per bin $N_{\mathrm{bin}} = \mathrm{Var}_{\mathrm{block}} \sum w^2$
(3.3, per-channel block means), $n_C, n_L$ bins per region:

$$ E_L = \max(P_L - n_L N_{\mathrm{bin}} - 3 N_{\mathrm{bin}} \sqrt{16\, n_L},\ 0), \qquad
   E_C = \max(P_C - n_C N_{\mathrm{bin}} - 3 N_{\mathrm{bin}} \sqrt{8\, n_C},\ 0), $$

$$ M_k = \begin{cases}
   0 & E_L = 0 \\
   1 & E_C = 0 \\
   \min\!\Big( \min(1,\ 0.3\, E_L / E_C)^2,\ \sqrt{n_C N_{\mathrm{bin}} / (E_C - 0.3\, E_L)} \Big) & \text{otherwise}
\end{cases} $$

(The factors 16 and 8 are the measured excess fluctuation of Hann-windowed
Bayer noise sums over independent $\chi^2$ bins; 0.3 is the share of ring
energy a $1/f^2$ edge spectrum puts into the band, $\int_{0.35}^{0.5} f^{-2} /
\int_{0.15}^{0.35} f^{-2} \approx 0.225$ with some slack.) Blocks with
> 25 % saturated photosites get $M_k = 0$. The block grid is eroded with a
3×3 minimum, then bilinearly interpolated to pixels (clamped outside the
outermost block centres). Each chroma estimate is split into core (low-pass
at 0.05 c/px, computed at half resolution) and band:

$$ \hat C_k' = g_k \hat C_k + (1 - g_k)\, \mathrm{Core}(\hat C_k), \qquad
   g_1 = 1 - M_1, \quad \rho_a = 1 - M_{2a}, \quad \rho_b = 1 - M_{2b}, $$
$$ \hat C_2' = \frac{\rho_a \hat C_{2a}' + \rho_b \hat C_{2b}'}{\rho_a + \rho_b} \ (\rho_a + \rho_b > 10^{-9}), \quad
   \text{else } \tfrac12 (\hat C_{2a}' + \hat C_{2b}'), $$

and $\hat L$ as in 4.2 with the primed estimates. Measured: neutral at 1×,
−1.5 to −3 dB in the band-limited condition against the fixed estimator,
because it keeps carrier energy it classifies as texture. An optional
Gauss–Seidel cross-term loop (subtract the other carriers' current estimates
before demodulating each one; stop at relative change $< 10^{-4}$, carrier
energy below noise, or 12 rounds) changed no figure beyond the fourth
decimal and doubled the run time; default one round.

## Appendix B. Reproduction recipe

```
input:  raw CFA array, black/white levels, CFA phase (ε, a, b), as-shot WB
1.  m := (raw − black) / (white − black)                      §3.1
2.  sat := raw ≥ white·0.98, dilate 2 px                       §3.2
3.  defect rule on same-phase 8-neighbourhood, σ(s) of §3.3    §3.4
4.  m := m · k_c  (k_G = 1, as-shot normalised to G)           §3.5
5.  kernels: Kaiser-sinc, Δf 0.15, A 40 dB, 15 taps, DC/Nyquist-corrected,
    fc = 0.15 (n), 0.30 (w), 0.25 (r); G_σ: Gaussian σ 3 px   §4.1
6.  C2a := LPn,x LPw,y (a·sx·m);  C2b := LPw,x LPn,y (b·sy·m)
    C1H := LPn,x LPw,y (ε·sx·sy·m);  C1V := LPw,x LPn,y (ε·sx·sy·m)
    wH := G*C2b² / (G*C2a² + G*C2b²)                           §4.3
7.  rA := LPr,x LPr,y(a·sx·m) − LPn,x LPn,y(a·sx·m);  rB likewise with b·sy
    κ := clamp(G*(rA·rB) / sqrt(G*rA² · G*rB²), 0, 1)²
    C2a += κ·(LPr²(a·sx·m) − C2a);  C2b += κ·(LPr²(b·sy·m) − C2b)   §4.4
8.  C2 := wH·C2a + (1−wH)·C2b;  C1 := wH·C1H + (1−wH)·C1V
    C1 += κ·(LPr²(ε·sx·sy·m) − C1)
9.  L := m − ε·sx·sy·C1 − (a·sx + b·sy)·C2                    §4.3
10. [optional] β from the carrier residue of L (σ 2 px, 20/80);
    Zhang–Wu LMMSE in m^(1/2.2); L, C1, C2 += β·(nonlinear − linear)   §5
11. out := L + (wG − wR − wB)·C1 + 2·(wR − wB)·C2              §6
12. TIFF 16 bit linear grey + JSON description; mask sidecar   §7
```

Every constant in this recipe is a default of the published code and can be
checked against `crates/mimizan-core/src/{filter,dubois,reconstruct,mix}.rs`
and their tests.
