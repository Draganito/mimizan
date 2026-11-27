# Colour rings: the Bayer separation against other RAW developers

A measurement, not a part of the packages. It asks how Mimizan's colour
plate (`--mask dubois`, `adaptive`, `off`) and its least-squares demosaic
stand on extreme colour geometry: fine rings, sectors, stars, edges and
grids in pairs of saturated colours, up to the Nyquist limit and past it.
The reference is the ring target in the DPReview studio scene.

## What it does

`run_synth.py` renders the charts 8 times supersampled, blurs them as a
lens does, integrates over the pixel, samples an RGGB mosaic with base-ISO
noise and writes a DNG. Every developer opens that DNG. Each result is
measured against the scene before the mosaic.

| Chart | Content |
|---|---|
| `dpr` | the DPReview ring disc: rings, a sector of rings twice as fine, a centre four times as fine; periods 12/6/3 px and 8.8/4.4/2.2 px |
| `rings` | concentric rings, period 8 px down to 1.6 px (Nyquist is 2 px) |
| `zone` | zone plate, frequency rising to 0.88 cycles a pixel in the corner |
| `siemens` | Siemens star, 60 cycles |
| `edges` | slanted edges at 4, 14, 31, 47, 76 and 86 degrees |
| `grids` | lines of 2 to 4 px period, checkerboards, hairlines, dots |

Eight colour pairs: the four of the DPReview target (magenta on orange,
blue on red, cyan on blue, yellow on cyan), red on green, green on magenta,
blue on yellow, and black on white. Two palettes: `sat`, saturated sRGB
colours taken as camera values, and `cam`, the same colours as a Nikon Z f
records them after white balance (paler, what a camera file holds). Three
lenses: `sharp` (pixel aperture only), `lens` (Gaussian sigma 0.5 px) and
`soft` (sigma 0.75 px, a lens with a low-pass filter).

Equal terms: as-shot white balance, linear output, no sharpening, no noise
reduction, no lens correction, no highlight reconstruction, no tone curve.
A 3x3 matrix fitted on twelve flat patches takes each developer's colour to
the original's; on the synthetic files it stays within 0.001 of identity.

| Program | Methods |
|---|---|
| Mimizan | plate with mask Dubois, Adaptive, Off (`mimizan negative --dng`); least-squares demosaic (`examples/bayer_ls_develop.rs`), alone and with the chart lying in a frame of photographs |
| LibRaw (`dcraw_emu`) | bilinear, VNG, PPG, AHD, DCB, DHT, AAHD |
| RawTherapee | AMaZE, RCD, DCB, LMMSE, IGV, AHD, EAHD, HPHD, VNG4, Fast |
| darktable | RCD, AMaZE, LMMSE, PPG, VNG4 |
| colour-demosaicing | Malvar 2004, Menon 2007 (from the mosaic, no file) |

Measured per tile: CPSNR and luma PSNR on sRGB-encoded values, CIEDE2000,
chroma error, false colour (distance in a\*b\* from the line between the
tile's two colours), zipper (Lu and Tan 2003), the unrest and the width of
edges, and the resolved frequency (where the normalised error first passes
0.5).

## Running it

```sh
python3 -m venv .venv && .venv/bin/pip install -r bench/bayer-rings/requirements.txt
cargo build --release -p mimizan-cli
cargo build --release -p mimizan-cli --example bayer_ls_develop

.venv/bin/python bench/bayer-rings/run_synth.py --out out/bayer-rings
.venv/bin/python bench/bayer-rings/report.py --out out/bayer-rings
```

The programs are found on `PATH`, or named by `MIMIZAN`, `MIMIZAN_LS`,
`DCRAW_EMU`, `RAWTHERAPEE_CLI` and `DARKTABLE_CLI`; one that is missing is
left out. The versions used are written to `out/bayer-rings/versions.json`.
All 36 scenes take about two hours on four cores; everything is cached
under `--out`, which git ignores. `report.py` writes
`out/bayer-rings/report/tables.md` and the comparison images to
`out/bayer-rings/media/`.

Two more tables come from the same cache:

```sh
.venv/bin/python bench/bayer-rings/programs.py --out out/bayer-rings
.venv/bin/python bench/bayer-rings/ls_prior.py --out out/bayer-rings
```

`programs.py` sets the least-squares demosaic and the Dubois plate beside
what each program does by default (LibRaw AHD, RawTherapee AMaZE, darktable
RCD) and beside the best each program can do, scene by scene
(`report/programs.md`). `ls_prior.py` develops one chart alone and inside
frames of photographs two, three and five times its size with the
least-squares demosaic, and writes the prior the frame-wide search settled
on beside the error on the chart (`report/ls-prior.md`). The chart is the
same in every frame; figures that differ show how far the result hangs on
the rest of the picture.

## Changing the least-squares demosaic

```sh
cargo build --release -p mimizan-cli --example bayer_ls_lab
.venv/bin/python bench/bayer-rings/lab.py --cases rings-cam-lens:3,kodak:08 \
    --variants "now: bank=default; before: bank=before"
.venv/bin/python bench/bayer-rings/lab_matrix.py --variants "now: bank=default; before: bank=before"
```

`examples/bayer_ls_lab.rs` is that demosaic taken apart: the found
spectrum can be changed, the bank spelt out item by item, a spectrum put
behind the others, and the second step left off. `lab.py` keeps the search
of each frame and separates only the chart again, so a variant takes
seconds. A case is a chart in a frame of photographs (`rings-cam-lens:3`
in the six photographs enlarged twice, `:3n` in ten at their own size
behind the chart's lens, `:1` alone) or a Kodak picture (`kodak:08`,
`kodak2:08` enlarged twice). `lab_matrix.py` prints one line a variant
across charts and the change on the Kodak pictures.

When a change is in the demosaic itself, the 36 scenes before and after:

```sh
.venv/bin/python bench/bayer-rings/before_after.py --keep
cargo build --release -p mimizan-cli --example bayer_ls_develop
.venv/bin/python bench/bayer-rings/run_synth.py --methods mz_ls
.venv/bin/python bench/bayer-rings/before_after.py --media out/bayer-rings/media-before-after
```

`--keep` puts the cached results of the demosaic aside as the ones
before; the last line writes `report/before-after.md` and strips of the
original, both, and three other developers, of the chart among
photographs and of the chart alone. After another change `--drop` throws
the results away that are not put aside. `before_after_real.py` does the
same for camera files `run_real.py` has run on.

## Camera files

```sh
.venv/bin/python bench/bayer-rings/run_real.py --out out/bayer-rings \
    --crop ringe:0.636,0.779,200 NikonZF.nef
```

Every method develops the whole file, the frames are aligned and fitted to
one colour, and the same crop of each is drawn side by side. There is no
original, so this part gives pictures and one figure only: `--neutral NAME`
names a crop that holds nothing but black and white, and the colour that
strays from its median is written to `real/<file>/real.json`. The frames
are aligned to `--reference` (LibRaw AHD by default); name another method
where LibRaw cannot read the file. The studio scene of
DPReview cannot be fetched by a script (the site asks for a human); the
files have to be downloaded by hand. The default crop is where the colour
rings lie in that scene; check it against the picture and move it with
`--crop NAME:CX,CY,SIZE`.
