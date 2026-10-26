# The three-colour plate

Colour pictures without demosaicing, on Prokudin-Gorsky's principle.

## What Prokudin-Gorsky did

Miethe's additive three-colour process (Berlin 1902, camera by Bermpohl),
which Prokudin-Gorsky learned in 1902 and used for the Russian survey of
1909–1915. One 9×24 cm glass plate, three 8×9 cm fields, exposed one after
the other through a blue, a green and a red filter (blue at the top of the
plate, green, red at the bottom); three to four seconds for the three in
1904, 1/10, 1/10 and 1/4 s in bright sun, the red exposure longer because
the ethyl-red sensitised plates were weakest there. The ratio of the three
exposures is the white balance. The filters cut the spectrum into
400–500, 500–600 and 600–700 nm. The negative plate was contact-copied
onto one positive plate, so the three positives share one exposure and
one development and the relation of their densities is kept. The three
positives were projected through three lenses and the same three filters,
and the light added up on the screen; the lamps and the register of the
side lenses were the only controls. The Library of Congress today scans
the plate, cuts it into three frames, registers red and blue on green and
puts the frames straight into the RGB channels, without a colour matrix.

Colour film did the same thing simultaneously (three sensitised layers)
and subtractively (dye couplers, cyan, magenta, yellow), and added a
colour matrix in the emulsion (the orange mask: coloured couplers that
correct the dyes' unwanted absorptions). Each layer has its own curve;
where the three are not parallel the colour turns with the density
(crossover). The red layer, at the bottom of the pack, resolves least:
colour is softer than drawing in every film.

## The plate in Mimizan

The three exposures are three mixes of one separation. From §2.2 of the
paper, with weights `w` on the balanced channels,

    out = L̂ + (w_G − w_R − w_B)·Ĉ1' + 2·(w_R − w_B)·Ĉ2'

so pure red (1,0,0) gives `R̂ = L̂ − Ĉ1' + 2Ĉ2'`, pure green `Ĝ = L̂ + Ĉ1'`,
pure blue `B̂ = L̂ − Ĉ1' − 2Ĉ2'`. Every plate is the same full-resolution
luminance with a different band-limited chrominance added; every pixel of
every plate is a weighted sum of photosites the sensor measured. The three
plates are in register by construction. The drawing of the colour print
is, pixel for pixel, the drawing of the monochrome negative.

The taking filters (`crates/mimizan-core/src/plate.rs`) are three weight
triples, each summing to one so grey gives three equal plates:

- `camera` (default): the rows of the inverse of the normalised
  `xyz_to_cam · rgb_to_xyz`, i.e. the camera's D65 colour matrix towards
  the output primaries, as a RAW converter applies it. Colorimetric; the
  weights are negative off the diagonal. For the Z f towards Adobe RGB:
  red `+1.010 +0.263 −0.273`, green `−0.156 +1.552 −0.396`,
  blue `−0.023 −0.302 +1.325`. The matrix is read from rawler's
  per-illuminant table (D65 first); the file's own single matrix was
  empty for the Z f.
- `wratten`: the panchromatic base weights behind Wratten 25, 58 and 47
  from `mix.rs`, crosstalk included (red passes 8 % of green, blue 25 %).
  The softer palette of the historical glass. For the Z f: red
  `0.820 0.180 0`, green `0.033 0.905 0.062`, blue `0.012 0.264 0.725`.
- `channels`: the balanced channels as they are, what the Library of
  Congress does. The fake-Bayer bench uses it, since the truth there is the
  picture itself and there is no camera.

The white balance before the separation stays what it was: it is the ratio
of the three exposure times. Clipped photosites clamp at 1 in every
channel, so a clipped highlight goes to paper white in all three, like
the shoulder of a slide; there is no highlight reconstruction.

**The file.** One 16-bit linear grey TIFF with the three frames stacked,
blue at the top, green, red at the bottom, in the sensor orientation; the
negative's JSON carries a `plate` object with the frame order, the frame
height, the filter set and its three weight triples. Any viewer shows it
as what it is, a three-frame plate; the existing print path prints it as
a contact copy. The mask sidecar is one frame, the same for all three.

**The print.** `print` recognises the `plate` object, splits the frames
and runs the one pass three times with the same layout, the same crop,
the same paper and the same two exposures. Grey therefore stays grey at
every density (no crossover, by construction, which film never had), and
saturated colours run towards white in the shoulder and darker in the toe,
the slide look. The three positives become the three channels of a 16-bit
RGB TIFF (or JPEG) whose primaries are the projection filters: Adobe RGB
(1998) by default, since its transfer curve is exactly the gamma 2.2 of the
paper's encoding and its gamut holds the saturated primaries; sRGB
primaries on request. The embedded profile is a matrix/TRC profile with the
D50-adapted colorants. `--lamp R,B` adds stops of exposure to the red and
blue positive, the lamps of the projector; it is the only colour control.

```bash
mimizan negative photo.nef -o photo_plate.tif --plate            # camera filters
mimizan negative photo.nef -o photo_plate.tif --plate wratten
mimizan print photo_plate.tif -o photo_colour.tif --paper 0,0 --size 30x40cm
mimizan print photo_plate.tif -o photo_colour.tif --paper 0,-0.5 --lamp 0.1,-0.1
```

Without `--plate` nothing changes: the same file and the same recipe give
the same bytes as 0.12.0. The plate came in 0.13.0; 0.14.0 made it the second process of the desktop.

**The desktop.** The plate is the second process of the darkroom, beside
the monochrome negative: *Monochrome · Plate* in the toolbar, or the key
P. The choice is the darkroom's, not the file's: the strip shows the folder
as slides, the Enlarger shows the taking filters and the primaries where
the monochrome mix stands (the Wratten set keeps the mix beside it, since
it follows it), the two lamps follow the paper's two exposures as sliders
in stops, the Print window names the RGB file and its profile, and the next
file opens in the same process. It is a re-mix, not a re-development: the
separation stays, the picture turns to colour at once, in the fitted view,
at 100 % and in the print box, all through the same table as the grey. The
print histogram stays the monochrome mix's, the plate's luminance, with the
three channels drawn as outlines over it. The plate and the lamps are part
of the development the history undoes, and the recipe beside the RAW
carries them (`plate`: filters, primaries, lamps) and the mix (`mix`), so
a file comes back in the process it was left in; `negative --recipe` and
`print --recipe` read the same file.

## Measured

`mimizan bench plate`: the Kodak set as fake Bayer mosaics, the plate with
`channels` filters against the demosaiced RGB of libraw and RawTherapee
5.11. CPSNR on sRGB-encoded values, border 16 px, 24 pictures. Report and
JSON in `out/plate/`.

Three conditions, as in the monochrome bench: the pictures pixel-sharp
(`bench plate`), behind a lens (`--blur 0.75`, a Gaussian of σ 0.75 px
before sampling, the sharpest real lens on a real sensor), and at twice
the size (`--scale 2`, the picture upsampled 2× before sampling, a 48 MP
sensor seen from the distance of 12 MP). Mean CPSNR and R/G/B PSNR in dB;
the leads column counts pictures won.

Pixel-sharp (the synthetic case no camera produces):

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| LMMSE (RawTherapee) | 39.42 | 39.14 | 42.29 | 38.27 | 23/24 |
| **plate** + reconstruct | 38.31 | 38.51 | 40.86 | 36.71 | 1/24 |
| AMaZE (RawTherapee) | 36.49 | 36.04 | 39.93 | 35.42 | 0/24 |
| DCB (RawTherapee) | 35.87 | 35.69 | 38.70 | 34.90 | 0/24 |
| DHT (libraw) | 35.35 | 34.78 | 37.13 | 34.75 | 0/24 |
| **plate** (linear) | 33.46 | 33.57 | 36.57 | 31.70 | 0/24 |
| VNG (libraw) | 33.26 | 32.94 | 36.60 | 31.77 | 0/24 |

Behind a lens (σ 0.75 px):

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| **plate** + reconstruct | 46.51 | 47.96 | 52.63 | 43.75 | 7/24 |
| RCD (RawTherapee) | 46.19 | 45.91 | 50.05 | 45.18 | 6/24 |
| LMMSE (RawTherapee) | 46.16 | 44.99 | 49.27 | 45.95 | 8/24 |
| DCB (RawTherapee) | 46.03 | 46.18 | 51.55 | 44.61 | 3/24 |
| AMaZE (RawTherapee) | 45.61 | 45.55 | 49.64 | 44.51 | 0/24 |
| **plate** (linear) | 44.98 | 47.16 | 51.83 | 41.92 | 0/24 |
| AHD (libraw) | 41.90 | 44.58 | 48.43 | 38.74 | 0/24 |

Twice the size (scale 2):

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| **plate** + reconstruct | 47.53 | 48.15 | 52.96 | 45.20 | 19/24 |
| **plate** (linear) | 47.24 | 48.37 | 53.09 | 44.62 | 4/24 |
| RCD (RawTherapee) | 45.42 | 45.32 | 49.21 | 44.16 | 0/24 |
| DCB (RawTherapee) | 45.22 | 45.42 | 49.73 | 43.78 | 0/24 |
| LMMSE (RawTherapee) | 44.75 | 44.09 | 47.65 | 44.04 | 1/24 |
| AMaZE (RawTherapee) | 44.51 | 44.45 | 47.74 | 43.46 | 0/24 |
| AHD (libraw) | 42.43 | 44.06 | 47.89 | 39.65 | 0/24 |

Reading: on the synthetic pixel-sharp pictures the band limit of the
chrominance costs the linear plate 6 dB against the best demosaicer, and
`--reconstruct` recovers all but 1.1 dB of it. As soon as the picture
has passed a lens the plate is level with the best (RCD, LMMSE, DCB within
0.5 dB, every other demosaicer behind), and at the sampling of a real
high-resolution sensor the plate leads by 2 dB with no reconstruction at
all. The green channel of the plate is ahead everywhere by 2–5 dB: it is
the luminance plus Ĉ1' only, not interpolated. The blue trails, as it does
in every method: one photosite in four, and the sRGB encoding weights its
errors most.

## Where it stands

- The colour is on the carriers and band-limited to about 0.25 c/px: the
  colour resolves half as fine as the drawing. On a print that is the red
  layer of a film; on a pixel-sharp chart it costs decibels against the
  edge-directed demosaicers, which reconstruct colour at the pixel.
- The Ĉ1 leak on diagonal colour alternation at 3–4 px period (the colour
  zone plates of the studio scene) is false colour here, where the
  negative showed grey beads. Same cause; `--reconstruct` reduces it.
- The chrominance carries low-frequency colour noise (19–21 % σ for the
  red filter on the Z f, paper §6.1): colour mottle like dye clouds. No
  noise reduction, as a matter of principle; not yet measured for the plate.
- The window, the paper and the method are those of the monochrome
  darkroom; the plate adds two lamps and nothing else. There is no
  saturation, no hue, no per-channel curve, by design.
