# Mimizan Lab

> Lift the colour off. Print what remains.

A darkroom for the digital negative, with two processes.

You open a colour RAW. Mimizan Lab reads the Bayer mosaic as a signal, lifts the colour off its carriers and keeps the luminance underneath: a negative in which every pixel is a sum of photosites the sensor measured. No demosaicing, in either process. That is true of a Bayer file until you ask otherwise. A Fuji X-Trans RAF opens in the same window. The first time, it is written out as a linear DNG beside the RAF, highlights kept, and that file is a demosaic: three colour values a pixel, worked out from the readings around it. The plate then takes those three channels as it takes any linear DNG. The figures for that demosaic are [below](#fuji-x-trans). For a Bayer file the frequency separation stays the path. *least-squares demosaic* in the Negative window is the alternative, and it stays off: on, it writes a linear DNG beside the RAW and the darkroom opens that DNG the way it opens the X-Trans one. The figures are [below](#bayer-the-least-squares-demosaic).

> **On the 24 Kodak pictures, sampled with the X-Trans tile and compared with the original, this program leads RawTherapee's Markesteijn on every picture.**

**The first photographic application that takes the black-and-white negative directly from the luma-chroma separation of the sensor signal ([Alleysson, Süsstrunk and Hérault 2005](https://doi.org/10.1109/TIP.2004.841200), [Dubois 2005](https://doi.org/10.1109/LSP.2005.859503)) without ever reconstructing an RGB image.**

**Monochrome.** The negative is printed the way a split-grade enlarger prints a sheet of paper: one placing, a soft exposure and a hard one. A measured Ilford Multigrade, no hidden curve.

**Plate.** The same separation is mixed three times, through three taking filters, into three plates, and the three are printed together through the same paper and the same two exposures into one colour file: Prokudin-Gorsky's three-colour process on the Bayer mosaic. Two lamps, red and blue, are the only colour control.

The switch between the two is in the toolbar, and the whole darkroom follows it: the strip of film, the Enlarger, the print. A file from a monochrome sensor opens as the measurement; that sensor has no colour filter and no plate.

It is for analog photographers and fine-art printers who decide the print on the paper.

**Draws finer than any demosaicer.** What your lens puts on the sensor stays in the negative: every tone in the grey is a sum of measured photosites, none is guessed. On real camera files that is 2 to 4 dB closer to the scene than the best of sixteen demosaicers.

**Never invents a pixel.** Every other converter guesses two of three colours per pixel. Mimizan Lab reads the sensor the way analogue television read its signal: luminance and colour separated, nothing interpolated, nothing made up.

**Not a filter that looks like film. A darkroom.** Presets paint a look over a finished photo. Mimizan Lab develops a real negative on real paper. Nothing is pretended, because nothing has to be.

**A negative, not a file.** Your photo is a real negative in linear light. You develop it, you don't edit it.

**Black and white like film.** Measured variable-contrast paper, soft and hard light mixed like an enlarger. Highlights roll off the way silver does.

**The zones on a strip of paper.** In the Enlarger lies the scene as a grey wedge, a zone to the stop, from five stops under the grey card to the clip, each slice printed as the paper stands. Drag it and the print is lighter or darker; turn the wheel over it and the grade is harder or softer, from 00 to 5. You see where every zone of the scene falls on the paper.

**Sharp without the halo.** The measured sharpening stops before the edge becomes a line.

**Printed like a sheet.** Centimetres and dpi, a border and a film edge. The file is a print, not a screen picture.

**Colour the way it was first photographed.** Three separate colour plates printed together. Saturated, deep, alive. Two lamps are the only colour control.

**Three palettes, chosen as a film once was.** The taking filters decide how much colour reaches the paper. *Camera* is the colour of the scene, vivid and deep. *Channels* is the sensor's own colour, calm and natural. *Wratten* is the historical glass, the quietest of the three. On a Fuji X-Trans file the matrix behind *Camera* is a peculiar one; use *Channels* there, as set out under [Fuji X-Trans](#fuji-x-trans). The paper does the rest: deeper in the shadows, paler in the lights. Three characters, and no slider between them.

**One colour look, and it holds.** The colour is the process's own, as a film stock's is. Saturated colours run to white in the highlights and go deeper in the shadows, and the colour lies softer than the drawing. With one palette, two lamps and nothing else to set, a series keeps one character. It is a slide, not a colour chart: where a product's colour has to match a sample, the linear DNG goes to your own converter.

**One paper, two darkrooms.** The phone's negative is printed here, by `mimizan print`, through the same paper.

**Your pictures are yours.** No cloud. No AI. No account. Everything happens on your computer. Nothing leaves it.

> Pure monochrome. A new split-grade digital darkroom, authentic for fine-art monochrome. A minimal, honest darkroom. The Bayer file is read as frequencies: luminance below, colour on the carriers. No demosaicing. The pixels stay measured. Nothing is reconstructed, and the character and soul of the lens are left unchanged. Focus on the picture, and on what you want to say with it. Your art.
>
> Dragan Bojovic

The name is the beach at Mimizan, where the idea came up watching the surf: separate the waves, and keep the picture underneath. Lab is the darkroom.

![Mimizan Lab in the plate process: a cabin at the Swedish coast, a photograph by seume (CC0) from a Sony ILCE-6000. The toolbar with the film strip, the three panels and the two processes and, in its second row, the 90° turn, Crop and Dodge & burn, and beside them what the picture carries; the Negative window on the left with the file, its development and the histogram of the three channels; the colour print in the middle, fitted, with its white border and its black edge; the Enlarger below with the taking filters, the paper strip and the two lamps; Print on the right with the sheet, its 1:1 box and the sharpening, and the export at its foot](docs/gui.jpg)

## The negative

> With Mimizan your digital negative is a negative again: every tone comes from the light, none from an algorithm guessing what might have been there. And that is exactly why, where your lenses really resolve, it draws finer than any demosaicer.

A colour sensor records one signal. The luminance sits in the low frequencies. The colour rides on carriers at the highest frequency the sensor can hold. [Alleysson, Süsstrunk and Hérault](https://doi.org/10.1109/TIP.2004.841200) described that signal. [Dubois](https://doi.org/10.1109/LSP.2005.859503) showed how to lift the colour off the carriers. Their papers reconstruct a colour picture. A monochrome photograph needs only what remains once the colour is gone.

A RAW converter invents a red, a green and a blue at every photosite, by interpolation and, in the methods that lead these comparisons, by a guess about the local edge. A later step adds the three and keeps one. The guesses stay in the grey: zippers, mazes, and a colour error that has become fine texture. On a large print that texture is the part that never belonged to the lens.

Mimizan Lab subtracts the colour and keeps the luminance. On this path a pixel is a sum of photosites the sensor measured. Detail the lens put on the mosaic stays detail.

The two papers were not quite enough for a photograph. The separation runs on white-balanced photosites, as the camera metered them, so a grey subject puts nothing on the carriers and those carriers can carry real detail. The colour is estimated in two directions, in the way Dubois estimates it, and the copy the luminance has leaked into is trusted less. Where both copies agree, the colour band is opened and the detail underneath comes back. The white balance before the separation, the use of both colour components, and that agreement test are the part built here.

Both stop there. Where the colour is strong, the two carrier copies alone cannot say which one the luminance has spoiled, so the separation also reads the direction of the structure from the part of the signal below the carriers, which is free of colour. That is still a choice between measured sums, never a computed value. It is also the last step. Earlier versions had a switch beyond it that computed the samples where luminance and colour truly overlap; 0.15 removed it. On the Nikon Z f files at the sharpness the camera recorded it touched 0.7% of the pixels and cost 0.3 dB, and on every real lens the linear negative was already ahead. What remains is a pipeline with no computed pixel anywhere.

### How the separation works

The Bayer mosaic is a modulated signal. Multiply a picture by the pattern of the colour filters and the luminance stays where it was, in the low frequencies, while the two colour differences are shifted to the corners of the spectrum: one to the diagonal carrier, the other to two copies, one at the horizontal limit and one at the vertical. The grey photograph is what sits below those carriers. Separating it means estimating the colour on the carriers and subtracting it, so that each pixel of the negative is a weighted sum of photosites the sensor measured, and nothing else.

The two copies of the second colour difference ought to be identical. They are not, because luminance leaks into them: fine vertical detail reaches the copy at the horizontal limit, fine horizontal detail reaches the other. Dubois measured the energy around each copy and trusted the one with less, since the leak adds energy. That works until the colour is strong. Then both copies carry a lot of energy, the weight falls back to one half, and the leak passes into the grey as texture that was never in the picture.

Since 0.12 the separation also reads the base band. Below the carriers the mosaic is free of colour, so the orientation of the structure can be measured there without the colour getting in the way: second differences along x and along y, squared and smoothed, give a structure tensor, and its anisotropy says whether the structure runs horizontally or vertically. That orientation tells which copy the luminance can have leaked into, and that copy is trusted less, even where the colour is strong. The decision is no longer a guess between two spoiled copies; it is measured on the part of the signal that cannot be spoiled.

A third measurement opens the colour band again. Where the two copies agree, their correlation is high, the colour there is real and the band can be opened, so the detail underneath, the last octave the lens delivered, comes back. In 0.12 the tensor also opens the band of the copy it has found clean. All of this is weights: energies, an orientation, a correlation, each of them measured on the file. No value is computed from a model of what an edge should look like. The same file and the same recipe give the same bytes, on any machine and any number of threads.

That is why the negative is finer where a lens really draws. A demosaicer invents two of the three colour values at every photosite and needs an assumption about the local edge to do it; the error of that assumption survives the sum to grey. The separation leaves the luminance intact up to the carriers and takes away only the colour. Detail a lens puts below the band limit of the sensor passes through untouched, which is where the 2.4 to 4.3 dB over RCD on the Z f files and the twenty-four of twenty-four at the band limit come from. Only where the detail is sharper than any lens delivers do luminance and colour truly overlap in the spectrum; there the band limit of the sensor is the limit of the picture, and Mimizan leaves it so rather than compute what the lens did not deliver.

## The monochrome print

The print is a sheet of Ilford Multigrade, taken from its datasheet. Soft is grade 00, the datasheet's curve, ISO R 160. Hard is grade 5, modelled on its ISO R 50. There is no hidden tone curve. The preview is that paper, and the TIFF is that sheet. The same recipe gives the same bytes.

**Highlights you can burn in.** The negative does not end where green is full. Red and blue fill later, so a sky that has burnt out green still holds measured light above that level, and the negative keeps it. On the base print those are the lightest tones before the paper's white; a burn brings them down with their gradation, as it does on a dense negative. Where all three colours were full the sensor has nothing more, and the program adds nothing: that stays one flat tone.

**To keep the sensor as it measured and the lens as it draws:** leave the sheet at *1:1*, the straighten at 0° and the sharpening off. Then one pixel of the print is one pixel of the sensor. A 90° turn and a crop cost nothing. A size or a straighten puts the picture on a new grid: nothing is guessed, but each pixel becomes a mix of its neighbours. So hold the camera level when you take the picture.

On the Kodak set, brought to the band limit of a photograph, the linear negative is 51.8 dB. The best of the sixteen demosaicers we ran, DCB in RawTherapee, is 49.1 dB; RCD is 48.9 dB. All twenty-four pictures are above every one of them: VNG, PPG, AHD, DCB, DHT and AAHD in libraw, and fast, VNG4, AHD, EAHD, HPHD, DCB, LMMSE, IGV, AMaZE and RCD in RawTherapee. On four Nikon Z f files at the sharpness the camera recorded, the mean is 54.0 dB against 50.7 dB for RCD, and all four are above it by 2.4 to 4.3 dB. Where a lens-like picture still reaches the sensor, the mean is 51.6 dB and DCB is 51.5 dB; nine of the twenty-four lead every demosaicer. On detail sharper than any lens delivers, LMMSE in RawTherapee leads, 43.1 dB to 37.3 dB. On the four Z f files made twice as sharp as the camera saw them, RCD leads by 0.5 to 1.9 dB. Those last two conditions are where the removed nonlinear step used to help, and they are conditions no camera produces. The means, and how many pictures lead, are in the paper.

It opens a Bayer RAW, a file from a sensor with no colour filter, a linear DNG, and a Fuji X-Trans RAF. A linear DNG has its three channels already: it is taken as it is, nothing is separated, and its colour is the work of whatever wrote it; the darkroom after that is the same. The X-Trans RAF is written once to that kind of DNG and opened as the DNG. Not a RAW converter: no noise reduction, no lens correction, no saturation, no hue. Quad-Bayer, Foveon and Nikon HE/HE* are not supported.

## The plate

> Three plates, one paper, two lamps. The colour is a relation of exposures, not a value at a pixel.

**Where it comes from.** In 1902 Adolf Miethe built a camera that exposed one long glass plate three times in a row, through a blue, a green and a red filter. Sergei Prokudin-Gorsky took it across Russia from 1909 to 1915: three to four seconds for the three exposures, the red one longer because the plates were weakest there. The ratio of those three times was the white balance. The negative was contact-copied onto one positive plate, so the three positives shared one exposure and one development, and the relation of their densities was kept. Projected through three lenses and the same three filters, the light added up on the screen; the lamps and the register of the side lenses were the only controls. The Library of Congress today scans those plates, cuts them into three frames, registers red and blue on green and puts them straight into the RGB channels. Colour film did the same thing later, in three layers at once, with dyes instead of light, each layer with its own curve, and the red layer, at the bottom of the pack, resolving least. Colour has always been softer than drawing.

**How it is done here.** The separation of the negative gives three things: the luminance, and the two colour differences lifted off the carriers. The monochrome mix adds them up with one set of weights. The plate adds them up three times, with three sets of weights, the taking filters:

```
R̂ = L̂ − Ĉ1' + 2Ĉ2'      Ĝ = L̂ + Ĉ1'      B̂ = L̂ − Ĉ1' − 2Ĉ2'
```

Every plate is the same full-resolution luminance with a different band-limited colour added. Every pixel of every plate is still a weighted sum of photosites the sensor measured; no value is interpolated, no edge is guessed. The three plates are in register by construction, since they come from one separation. The taking filters are the camera's own colour matrix towards the output primaries (the default, colorimetric), Wratten 25, 58 and 47 in front of the panchromatic emulsion, the camera file's calibrated response or the native one (the softer palette of the historical glass), or the bare channels, as the Library of Congress does. The white balance stays where it was, before the separation, as the camera metered it: it is the ratio of the three exposure times, and it is what makes a grey subject give three equal plates. It also sets where each channel burns out, and green burns out first. Above that level two channels can no longer be compared, and a burnt-out white would read as magenta, whatever the scene was. So what a photosite reads above it counts as light and not as colour: a burnt-out highlight has no colour and goes to paper white in all three plates, as it is in the monochrome print; a channel that is still measured beside burnt-out ones keeps its colour; a channel that burns out alone pales towards white with the exposure. A plate the matrix drives below no light is brought back to none, and the other two give up the same share, so the hue stays and the light stays. Nothing is reconstructed.

The negative is written as one grey TIFF with the three frames stacked, blue, green, red from the top, the way the glass looked. The print runs its one pass three times over those frames, with the same crop, the same paper and the same two exposures; the three positives become the channels of a 16-bit RGB TIFF with an sRGB profile by default, or Adobe RGB. The two lamps, red and blue, add stops to their positive; green is the reference. That is the only colour control. There is no saturation, no hue, no per-channel curve.

**What it gives.** The drawing of the colour print is, pixel for pixel, the drawing of the monochrome negative: red, green and blue carry the same edges in the same places, so there are no colour fringes, no zippers, no mazes, and the detail a lens put on the mosaic stays where it was. Grey stays grey at every density, since all three channels go through one paper curve from one pair of exposures, which film never managed (crossover). Saturated colours run towards white in the shoulder and darker in the toe, the look of a slide. The colour itself is band-limited to about a quarter of the sampling, half as fine as the drawing, and lies under a sharp luminance as a soft layer: the structure of a tripack film, or of a dye-transfer print, and the reason the pictures read as prints rather than as files. The noise is film's too: fine luminance grain, and under it colour mottle, slow and soft, never a pixel-sized rainbow.

Measured on the Kodak set against sixteen demosaicers of libraw and RawTherapee, CPSNR: behind a lens the plate is 45.0 dB against 46.2 for RCD and LMMSE, ahead of all sixteen on three pictures of the twenty-four; at twice the sampling 47.2 dB against 45.4, ahead on twenty-two. Only on pixel-sharp charts, which no camera produces, does the band limit cost it against the edge-directed demosaicers. The green channel is the plate's best, 3 to 5 dB above its red, and behind a lens and at twice the sampling it is above every demosaicer's green, by 0.3 and 3.4 dB: it is `L̂ + Ĉ1'`, not interpolated.

**Three palettes.** The taking filters are chosen as a film once was, and what they differ in is how much colour they hand to the paper. The filters on the sensor overlap widely, so its bare channels hold less colour than the scene: *Channels* is the calm palette. *Camera* takes the overlap out with the camera's matrix and hands over the colour of the scene, about half as much again as *Channels*. *Wratten* hands over the least, about a fifth less than *Channels*, since each glass lets its neighbours through. Then the paper takes each plate on its own. Around the grey card and below it the paper is steeper than the scene, half as steep again at the card and more than twice in the shadows, so the colour grows deeper there; in the lights it is flatter than the scene, and the colour pales towards paper white. That is why *Camera* prints vivid and deep, a slide, and why *Channels*, which starts with less, reads as natural on the sheet. None of the three corrects another: *Camera* is the exact one up to the paper, *Channels* keeps the hues of the sensor, *Wratten* those of the glass. (The colour is measured as the saturation in linear light on three Z f files, the paper on a printed wedge, both at the base print.) On a Fuji X-Trans file that matrix is a peculiar one, and *Channels* is the palette to print with until it has been measured, as [below](#fuji-x-trans).

**What it is not.** A neutral reproduction, and it is not meant to become one. The camera filters are colorimetric for daylight, and *card* in the Negative window puts the balance on a grey card in the picture; up to the paper the colour is as exact as the camera's matrix. Then the three plates meet the paper, and the paper has a toe and a shoulder: the same red prints paler in a highlight and deeper in a shadow than a colour chart would have it. Fine patterns of alternating colour, three to four pixels wide, can print as false colour, and there is no noise reduction for the mottle. None of this can be tuned away, which is why a series holds together: the same light and the same paper give the same colour on every sheet. For general photography and portraits that is the look. Where the colour of a product has to match a sample, export the linear DNG (*Negative…* in Print) and give it your profile in your own converter.

In the window the plate is the second process: *Plate* in the toolbar, next to *Monochrome*, or the key P. The picture turns to colour at once, fitted, at 1:1 and in the print box; it is a re-mix of the separation, not a re-development. The Enlarger shows the taking filters where the monochrome mix stood, and the two lamps under the paper strip; the primaries are in Print, with the file. The strip shows the folder as slides. The Print panel says what it will write, a 16-bit RGB TIFF with the profile. The recipe saved next to the RAW remembers the process, the filters and the lamps, so the file opens again in colour. On the command line it is `--plate` on `negative` and `--lamp R,B` on `print`, or `--recipe` with the desktop's file on both. [docs/PLATE.md](docs/PLATE.md) has the method, the file format and the measurements; the paper has it in §7.5.

## Fuji X-Trans

A RAF from an X-Trans sensor opens in the window. While no DNG sits beside it, the strip shows the camera's embedded JPEG. Opening the file writes that DNG once, next to the RAF, with the highlights left as recorded, and the RAF leaves the strip. Every later open reads the DNG and does not look at the RAF again. A Bayer RAF, including one from Fuji, stays on the ordinary path. The writing is the slow part: about three quarters of a minute for 26 megapixels, where RawTherapee's 3-pass takes about five seconds. After that the darkroom is as fast as for any linear DNG.

The DNG is a demosaic, and it is not what the two processes do with a Bayer file. There, every pixel is a sum of photosites the sensor measured and nothing is estimated. Here, one reading per photosite goes in and three colour values per pixel come out, worked out from the readings around it. Brightness and colour are read off the frequencies where the X-Trans pattern carries them. The plate then prints those three channels as it prints any other linear DNG: nothing is separated a second time.

*Camera* on one of these files is Fujifilm's own colour matrix, the one the RAF carries, turned toward the output primaries. It is a peculiar matrix. The colours it prints do not read as the scene, and it is not yet clear what Fuji has done wrong in it. That will take the matrices of a dozen Fuji cameras, measured. Until then use *Channels*, which prints the three channels of the DNG as they stand.

It leads Markesteijn, the 3-pass in RawTherapee 5.11 and the family around it, on all 24 pictures of the Kodak set, in each of the three conditions below. Adobe, Iridient and Capture One demosaic X-Trans in their own way, and none of them was in this measurement.

On one test chart from an X-Pro3, at ISO 160 and at ISO 3200, both pictures were printed by Mimizan Lab 0.19.0 and measured on the 8-bit prints, in steps of 255. The figures are ISO 160 / ISO 3200. It is as sharp as the 3-pass, with about a third of its false colour and half its colour noise, and it keeps the colour of fine detail, which the suppression takes out. Lines 2 to 2.8 pixels apart are not as calm as RawTherapee's yet; the blank paper beside them measures 0.56.

| | 3-pass | 3-pass, false colour suppression 3 | Mimizan Lab |
|---|---|---|---|
| Edge contrast in the Siemens star | 34.0 / 36.5 | 33.5 / 35.7 | 33.3 / 35.8 |
| False colour in small white print on black | 20.5 / 30.3 | 6.1 / 11.7 | 6.3 / 9.8 |
| Colour noise in a dark, flat area | 4.0 / 20.3 | 1.7 / 8.5 | 2.4 / 10.0 |
| Colour contrast in fine coloured rings | 70.7 / 73.6 | 53.8 / 54.8 | 74.7 / 76.8 |
| Unrest along lines 2 to 2.8 px apart, ISO 160 | 0.60 | not measured | 0.88 |

### On the Kodak set

The 24 pictures of the Kodak set, linearised, sampled through the X-Trans tile and quantised to 16 bit. The same mosaic goes to this demosaic and to RawTherapee 5.11. The balance is 1, 1, 1, because the picture is already the colour. The separation runs under the sRGB curve and comes back to linear light, as a camera file does. Nothing in these pictures is treated as burnt out. CPSNR, and the PSNR of red, green and blue, in dB on sRGB-encoded values; 16 px of the edge are left out. The last column is how many of the 24 pictures a method leads. A smooth picture, measured the same way, stays above 59 dB for every method, so the path has no tone curve and no offset left in it. On one picture, shifting RawTherapee by a pixel drops it from 31.5 dB to 22.9 dB; the unshifted place is the one that was measured.

Three conditions, the same ones the plate uses. Pixel-sharp is the Kodak pixels as they stand, sharper than a lens delivers. Behind a lens is a Gaussian of σ 0.75 px before the sampling, the sharpest real lens on a real sensor. Twice the size is the picture enlarged 2× with Lanczos-3 before the sampling, a 48 MP sensor seen from the distance of 12 MP.

The same-colour average is the floor, computed here: a photosite of a colour is kept, and a missing colour is the inverse-distance mean of that colour within four pixels. It is not a method anyone ships. The bench that measured the tables is not part of the packages.

Pixel-sharp:

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| Mimizan Lab | 39.81 | 39.19 | 42.30 | 38.82 | 24/24 |
| 3-pass | 34.17 | 33.58 | 36.60 | 33.52 | 0/24 |
| 1-pass (medium) | 33.78 | 33.20 | 36.05 | 33.18 | 0/24 |
| 3-pass, false colour suppression 3 | 32.28 | 34.09 | 35.77 | 29.74 | 0/24 |
| same-colour average | 27.81 | 27.14 | 29.55 | 27.18 | 0/24 |

Behind a lens (σ 0.75 px):

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| Mimizan Lab | 50.97 | 49.92 | 54.48 | 50.05 | 24/24 |
| 3-pass | 42.93 | 42.44 | 47.45 | 42.17 | 0/24 |
| 1-pass (medium) | 42.76 | 42.30 | 47.09 | 42.05 | 0/24 |
| 3-pass, false colour suppression 3 | 36.81 | 40.39 | 43.62 | 33.46 | 0/24 |
| same-colour average | 32.88 | 32.09 | 34.44 | 32.49 | 0/24 |

Twice the size:

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| Mimizan Lab | 48.89 | 48.46 | 52.52 | 47.35 | 24/24 |
| 3-pass | 43.05 | 42.73 | 47.98 | 41.87 | 0/24 |
| 1-pass (medium) | 43.04 | 42.78 | 47.78 | 41.87 | 0/24 |
| 3-pass, false colour suppression 3 | 37.40 | 40.40 | 44.02 | 34.16 | 0/24 |
| same-colour average | 31.84 | 31.38 | 33.50 | 31.03 | 0/24 |

Ahead of Markesteijn on all 24 pictures in each condition: by 5.6 dB pixel-sharp, by 8.0 dB behind a lens, by 5.8 dB at twice the size. The 1-pass sits 0.2 to 0.4 dB under the 3-pass. False colour suppression 3 lowers the figure, and the blue most of all, because it takes out colour the original picture has. Green is this demosaic's best channel. These pictures have no lens of their own: every difference from the original is the demosaic's.

## Bayer, the least-squares demosaic

The frequency separation stays the path: every pixel a sum of photosites the sensor measured, and nothing is estimated. *least-squares demosaic* is the alternative. It is in the Negative window, under Development, and it is off. On, a Bayer file is written once as a linear DNG beside the RAW, named with `.mimizan` before `.dng`, highlights kept, and the RAW leaves the strip. Every later open with the checkbox on reads that DNG and does not look at the mosaic again. A newer version of the program writes the DNG again, so the picture is the separation that version makes. The darkroom from there is the linear DNG's: three channels, nothing separated a second time, the plate printing them as it prints the X-Trans DNG. Off, the file stays the measured separation, and the DNG is not a tile of the strip. The choice is remembered.

The separation is solved on the 2×2 period. Four frequencies that differ by half a cycle are one solve: the luminance and both chrominances, under the prior the mosaic itself makes most probable. The search counts every such set. Counting every eighth, which is what writing the mosaic out to six and solving that did, leaves a fine colour short of the prior it has. The eight line priors of the X-Trans bank are left out: a Bayer row does not hold red, green and blue, and on a saturated picture they take the colour out. In their place the bank weighs eight directions whose luminance falls only as f¹, held with cos³². The colour stays the share the search found. The frequency separation is not mixed into it. A one-pixel checker that runs along a horizontal line is taken off; the line stays.

### On the Kodak set

The 24 pictures of the Kodak set, the same three conditions as the plate and the X-Trans note, against the sixteen demosaicers of libraw and RawTherapee. CPSNR, and the PSNR of red, green and blue, in dB on sRGB-encoded values; 16 px of the edge are left out. The last column is how many of the 24 pictures a method leads. The bench is `mimizan bench bayer`. It is not part of the packages.

Pixel-sharp:

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| Mimizan Bayer LS | 40.58 | 40.43 | 43.25 | 39.17 | 22/24 |
| VNG (libraw) | 33.26 | 32.94 | 36.60 | 31.77 | 0/24 |
| PPG (libraw) | 33.29 | 34.08 | 36.20 | 31.27 | 0/24 |
| AHD (libraw) | 33.83 | 34.78 | 37.64 | 31.50 | 0/24 |
| DCB (libraw) | 32.49 | 33.11 | 34.52 | 30.83 | 0/24 |
| DHT (libraw) | 35.35 | 34.78 | 37.13 | 34.75 | 0/24 |
| AAHD (libraw) | 34.26 | 35.21 | 37.55 | 32.04 | 0/24 |
| fast (RawTherapee) | 33.70 | 33.56 | 35.17 | 33.11 | 0/24 |
| VNG4 (RawTherapee) | 33.24 | 33.23 | 34.39 | 32.69 | 0/24 |
| AHD (RawTherapee) | 34.65 | 34.07 | 37.75 | 33.78 | 0/24 |
| EAHD (RawTherapee) | 34.88 | 34.43 | 37.83 | 33.99 | 0/24 |
| HPHD (RawTherapee) | 35.60 | 35.46 | 37.96 | 34.78 | 0/24 |
| DCB (RawTherapee) | 35.87 | 35.69 | 38.70 | 34.90 | 0/24 |
| LMMSE (RawTherapee) | 39.42 | 39.14 | 42.29 | 38.27 | 2/24 |
| IGV (RawTherapee) | 35.69 | 35.48 | 38.53 | 34.82 | 0/24 |
| AMaZE (RawTherapee) | 36.49 | 36.04 | 39.93 | 35.42 | 0/24 |
| RCD (RawTherapee) | 35.46 | 35.32 | 36.91 | 34.88 | 0/24 |

Behind a lens (σ 0.75 px):

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| Mimizan Bayer LS | 49.60 | 49.12 | 52.18 | 48.51 | 24/24 |
| VNG (libraw) | 41.00 | 41.76 | 44.31 | 39.03 | 0/24 |
| PPG (libraw) | 41.97 | 44.37 | 47.67 | 38.99 | 0/24 |
| AHD (libraw) | 41.90 | 44.58 | 48.43 | 38.74 | 0/24 |
| DCB (libraw) | 41.10 | 43.50 | 45.06 | 38.38 | 0/24 |
| DHT (libraw) | 45.01 | 43.65 | 47.34 | 45.46 | 0/24 |
| AAHD (libraw) | 42.95 | 44.94 | 48.22 | 40.10 | 0/24 |
| fast (RawTherapee) | 42.45 | 42.42 | 45.35 | 41.88 | 0/24 |
| VNG4 (RawTherapee) | 41.12 | 41.24 | 43.32 | 40.61 | 0/24 |
| AHD (RawTherapee) | 43.55 | 43.34 | 48.09 | 42.53 | 0/24 |
| EAHD (RawTherapee) | 43.56 | 43.40 | 48.03 | 42.58 | 0/24 |
| HPHD (RawTherapee) | 42.98 | 43.14 | 45.93 | 42.21 | 0/24 |
| DCB (RawTherapee) | 46.02 | 46.18 | 51.55 | 44.61 | 0/24 |
| LMMSE (RawTherapee) | 46.16 | 44.99 | 49.27 | 45.95 | 0/24 |
| IGV (RawTherapee) | 40.59 | 40.36 | 43.93 | 39.85 | 0/24 |
| AMaZE (RawTherapee) | 45.61 | 45.55 | 49.64 | 44.51 | 0/24 |
| RCD (RawTherapee) | 46.19 | 45.91 | 50.05 | 45.18 | 0/24 |

Twice the size:

| Method | CPSNR | R | G | B | leads |
|---|---|---|---|---|---|
| Mimizan Bayer LS | 47.03 | 46.82 | 49.17 | 45.87 | 16/24 |
| VNG (libraw) | 41.01 | 41.76 | 43.95 | 39.09 | 0/24 |
| PPG (libraw) | 42.65 | 44.24 | 47.89 | 39.93 | 0/24 |
| AHD (libraw) | 42.43 | 44.06 | 47.89 | 39.65 | 0/24 |
| DCB (libraw) | 41.63 | 43.68 | 44.37 | 39.27 | 0/24 |
| DHT (libraw) | 42.19 | 41.72 | 44.46 | 41.51 | 0/24 |
| AAHD (libraw) | 42.37 | 43.20 | 46.32 | 40.11 | 0/24 |
| fast (RawTherapee) | 42.33 | 42.20 | 45.16 | 41.58 | 0/24 |
| VNG4 (RawTherapee) | 41.25 | 41.42 | 43.05 | 40.71 | 0/24 |
| AHD (RawTherapee) | 43.21 | 42.80 | 47.30 | 42.20 | 0/24 |
| EAHD (RawTherapee) | 43.21 | 42.83 | 47.27 | 42.21 | 0/24 |
| HPHD (RawTherapee) | 42.19 | 42.18 | 44.50 | 41.52 | 0/24 |
| DCB (RawTherapee) | 45.22 | 45.42 | 49.73 | 43.78 | 5/24 |
| LMMSE (RawTherapee) | 44.75 | 44.09 | 47.65 | 44.04 | 2/24 |
| IGV (RawTherapee) | 40.34 | 40.16 | 43.49 | 39.37 | 0/24 |
| AMaZE (RawTherapee) | 44.51 | 44.45 | 47.74 | 43.46 | 0/24 |
| RCD (RawTherapee) | 45.42 | 45.32 | 49.21 | 44.16 | 1/24 |

The mean leads in each condition: by 1.16 dB pixel-sharp against LMMSE, by 3.41 dB behind a lens against RCD, by 1.61 dB at twice the size against RCD. Not every picture does. Pixel-sharp, kodim08 stays behind LMMSE by 0.08 dB and kodim09 by 0.38 dB. Behind a lens, every picture leads. At twice the size, eight pictures stay behind, by up to 0.94 dB (kodim12 against DCB).

## A session

Drop a RAW file or a folder on the window, or open one. The folder lies along the bottom as a strip of film: negatives in the monochrome process, slides in the plate, either one inverted on request. An X-Trans RAF with no DNG beside it shows the camera's JPEG there until the file has been opened once. It closes when you click the picture and release, or drag the crosshair and release; B brings it back, and the arrow keys still change the file. Tab hides the three panels; 1, 2 and 3 show or hide the one that carries that number in the toolbar.

*Monochrome* and *Plate* in the toolbar, or P, are the process. The choice is the darkroom's: the strip, the Enlarger and the print follow it, and the next file opens in it. A file with a recipe beside it opens in the process the recipe was saved in. Scroll zooms the print about the pointer; a double click switches between fitted and 1:1. Undo is Ctrl+Z, redo is Ctrl+Shift+Z or Ctrl+Y. *Reset all* puts every control back to its default, the crop and the strokes with them, and leaves the negative where it is; undo brings them back.

The picture in the middle is the print. A white border can be added around it, a black film edge against the picture, or both, the black inside the white, so the sheet is judged as it will go on the wall. The crosshair marks the spot the 1:1 box in *Print* is reading.

*Negative* is the file and the development. The histogram reads the print, the linear negative, or the raw channels, on a linear or a log axis. The negative's is the file's: its axis ends at 1, the level at which green is full, or at the highest value when highlights stand above it. White balance is as shot, gray world, none, or a manual red and blue; *card* reads a grey card in the picture on the next click, 32×32 photosites averaged without the clipped ones, and puts the balance into manual. The mask is Dubois, adaptive, or off. Apply recomputes the picture. Both are the file's: the recipe next to the RAW keeps them as `develop`, a file opens as its recipe says, and one without a recipe opens as shot with Dubois, whatever the file before it was set to. *least-squares demosaic* is the program's, and it is remembered: on, the Bayer file is written out as a linear DNG and opened the way a Fuji X-Trans file is opened; off, it stays the measured separation. The *defects* line counts the photosites that lie far outside their neighbours; the desktop leaves them as measured, and the command's `--repair-defects` is there for a hot pixel from a long exposure. A monochrome sensor has none of the colour controls: the file is the measurement. The *process* line names what the file is developed in; in the plate, the print histogram carries the three channels as outlines over the mix.

*Enlarger* holds the filter in front of the paper and the only tone controls a sheet of paper has. The filter is one row across the window, the same on the phone. In the monochrome process it is the mix: Native, one quarter, one half, one quarter; Pan, the panchromatic response when a camera file is loaded; or a glass in front of that, Y8, YG11, O16, R25, G58, B47, each written in its own colour when it is the one. Click a field or draw the mouse along the row; a word on a field says what the glass does to the picture, and the weights it comes to, R, G and B summing to one, are in the Negative window's process line. Under the row is the paper strip: the scene as a grey wedge, a zone to the stop, from five stops under the grey card to the clip two and a half above it, the card at the mark, each slice printed as the paper stands. A drag on the strip is one more or less second under the enlarger, and moves both exposures together. The wheel over the strip is the grade: up is harder, down is softer, a twelfth of a stop a notch. The mid-grey of the print stays where it is, as when one speed-matched filter is changed for another, and the ramp turns round it; at the base exposure that tone is the grey card. The wheel stops at grade 00 and at grade 5, where the paper has no further grade to give. The drag is three stops of time either side of the speed-matched exposure of the grade in the window, so a grade 5 print can still be made lighter and a grade 00 print darker. A double click is the base print, and a second one brings the pair back; the frame is amber whenever the pair is off the base. The line under the strip names the single grade the pair equals and how many stops of the scene the paper holds; the pair itself, *soft* for filter 00 and *hard* for filter 5 in stops of time from the base print, is in the recipe file, nowhere on the glass. In the plate the row is the three taking filters, Camera (the camera's matrix), Wratten 25/58/47 in front of the panchromatic emulsion, or Channels. The emulsion behind the Wratten glass is the file's, the camera file's calibrated response or the native one, shown in the Negative window, not chosen: the mix is the filter of one negative and does not reach the plate. Two bars follow the strip, the width of the paper and half its height: the red and the blue lamp of the projector, stops added to that positive. Each bar is dark and says nothing: the mark in the middle is zero, the handle stands on the lamp's stops, the letter at the left edge, in the lamp's colour, says which lamp; what the lamp does is in the picture. A click or a drag puts the handle there, the wheel is a twelfth a notch, a double click is zero and a second one brings the lamp back; the frame is amber off zero. Green is the reference; grey stays grey at zero.

*Crop* frames the picture. While it is open, the second row of the toolbar holds what it needs: the sheet, the straighten, and at the right the two ways out. The list opens as a table: the picture's own ratio, read from its pixels, then each sheet both ways — 4×5 and 5×4, on through 20×24 — and a square across the row. The side that is framed is sunk. *Straighten* beside it levels the picture: draw a line along a horizon or a wall, and the negative turns so the line lies level or upright; or type the degrees, up to 45 either way; 0°, or the × beside the number, takes it back. The picture is then the largest rectangle of the negative's proportion that fits inside the turned negative, a little smaller, with no empty corner, and the window lies in that. *Apply crop* makes window and straighten the picture, in whole pixels, one step of the history. Enter does the same. *Cancel*, or Escape, puts the easel down and the picture stays as it was. Until one of the two, the toolbar says *not applied*: undo, *Reset all*, the brush and another file put the easel down without keeping the window. 90° turns the picture itself, without loss; the recipe keeps it as one number, `quarter_turns`, so the file next to the RAW brings the turn back and the command makes the same print.

The straighten is in the recipe as one number, `angle`, and it sits in the pipeline between the 90° turns and the crop, so the crop, the sheet and everything after see a picture and nothing else. It costs nothing where there is none: at 0° the file is the same bytes as before. With one, the turn, the crop and the size step are one resampling, so the print is not resampled twice. Where there is a size step it uses the print's kernel, as the size step always did; a turn alone, at 1:1, uses Catmull–Rom, whatever *Resample* says. That was measured, not assumed: against a scene rendered at eight times the negative's resolution and on a real negative turned there and back, the default B-spline at scale 1 keeps half of the fine detail and loses a fifth of the grain, Lanczos keeps nearly all of it but overshoots a hard highlight by 2–4 %, and Catmull–Rom has the smallest total error at the usual degree or two, keeps 85–93 % of the detail and overshoots by under 1 %. At 1:1 the picture was never resampled before, and that is the kernel nearest to it. The linear exports under *Negative…* are straightened the same way.

*Dodge & burn* is the darkroom's dodging and burning: the hand, or a card, held between the lens and the paper for part of the exposure. The tool is a brush that belongs to the program, as the darkroom worker's tools belong to the worker: made once, kept, used on every negative until it is remade. It has two tips. The *Hand* is an ellipse with a soft edge that is painted along a path; the *Card* is a straight edge with a soft shadow that is pushed from one place to another. With the tool in the hand and *Shape* chosen, the tip stands in the middle of the picture with its handles and nothing in the picture changes: the dots size the hand, the ring turns it or the card, the square is the penumbra (the shadow the hand throws when it is held above the paper, dashed either side of the edge), the number at the centre is the stops, dragged up and down in hundredths (Shift slows the drag), and the row beneath the tip is its light, *both*, *soft* or *hard* (both unless you choose). Enter, or *Paint*, locks the brush and saves it with the program's settings; Escape opens it again, and a second Escape puts the tool down. Locked, the tip follows the pointer: a drag of the hand is one stroke, a drag of the card pushes its edge from the press to the release, the paper beyond where it ends up having had the light the whole time, the paper behind where it started none of it, and a ramp between; a click is the tip, or the card, held still, and a stroke begins and ends with its stops, the hand being set down at its start and lifted at its end. The stroke prints as it is made and the release is one step of the history; the other mouse buttons still pan, and Alt shows the strokes' paths. At the right of the toolbar's second row stands how many strokes the picture has, and *Clear strokes* takes them all away. With no tool in the hand that row says what the picture carries: its crop, its straighten, its strokes. The physics is the darkroom's. The stops are the time at the middle of a straight pass: $+1$ doubles the exposure there, burning it in darker, $-1$ halves it, holding it back lighter, four stops either way, in hundredths. A point the pass only brushes gets the chord, the hand going back and forth adds, as seconds do, and a hand that stands still over one place gives it what the tip's shadow gives, no more; times add, not stops, so two one-stop passes make three times the base, not four. The time is added to the working grade, or to the soft pass alone, or to the hard pass alone, the split-grade burn, which moves the highlights without the shadows or the shadows without the highlights. A stroke is in the recipe on the upright negative, in coordinates normalised to its long side, so it holds under any crop, straighten, 90° turn or print size; the fitted print, the 1:1 box, the preview and the histogram all read it through the same map as the pixels. Burns in orange, dodges in blue. In the file they are the list `burns`, one entry per stroke, such as `{"tool": "hand", "tip": {"rx": 0.08, "ry": 0.08, "feather": 0.03}, "stops": -0.25, "points": [[-0.3, 0.12], [-0.22, 0.16]]}` or `{"tool": "card", "x": 0, "y": -0.1, "angle": -90, "travel": 0.15, "feather": 0.1, "stops": 1, "light": "soft"}`; `mimizan print --recipe` applies them and names them in its log.

*Print* sets the sheet and writes it as one file; its first line says which. The first box is the container: a TIFF with the recipe in its description, in 16 bits for the archive and one's own printer or in 8 bits for a print service that takes no more, or an 8-bit JPEG at quality 100 with no chroma subsampling, for the screen and for a service with a limit on the file's size; all three carry the ICC profile and the dpi. The second box is the colour of the file. In the monochrome process it is grey, one channel with the paper's gamma 2.2, or RGB, the same sheet as three equal channels in sRGB, for a service that prints no greyscale file. In the plate it is the primaries, sRGB by default or Adobe RGB. An sRGB file is sRGB in full: its values lie on the sRGB curve under the profile *sRGB IEC61966-2.1*, so it reads the same whether a program honours the profile or only assumes it. The window shows a plate in sRGB as that file: the picture, the 1:1 box, the print box, the paper strip and the rings carry the file's samples, so the deep shadows stand on the screen where the file has them; the histogram and its clip marks go on counting the print's values. The Adobe RGB file keeps the paper's gamma 2.2, which is that space's own curve, under a profile made here that says *compatible with Adobe RGB (1998)*; a service that looks for Adobe's own profile by its name may not know it. The container and the grey sheet's channels are the program's, not the recipe's, and are remembered. The size fields are the sheet in the printer, in the crop's proportion: type one side, the other follows the picture, and the file is that sheet, turned the picture's way. With *free* on, the two sides are typed apart, for a paper a print service offers, 30 × 40 cm for a picture of 2:3; the picture is fitted into it and centred, and a new crop leaves the sheet as it is. The black edge and the white border are taken from inside the sheet, each on its own or black inside white, and the picture fills what they leave, centred; with rings, a sheet of the picture's shape leaves paper beyond the border on two sides, as on an easel. A black edge with no white border outside it lies where a print service trims, about 2 mm a side and never quite evenly; the row says *at the trim* then. A new crop is a new picture, and the sheet takes its proportion with its long side held, unless it is free. The *picture* row under the rings says how large the picture comes out and by what scale. A file opens at 1:1, the sheet that is the negative's pixels with nothing around them; *1:1* in the size row is lit while the sheet is that, and returns there. The ruler at the right of the size row shows every length in centimetres and millimetres, in inches, or in pixels of the file; the recipe keeps inches whichever is on. The dpi is written into the file; with the cm or in ruler a new dpi keeps the sheet and moves the pixels, with the px ruler it keeps the pixels and moves the sheet. An older recipe without a sheet is read as the sheet its file had. Resample is B-spline by default, or area (each output pixel is the source area it covers: a mean when shrinking, the pixel held when enlarging), Mitchell, Catmull–Rom, or Lanczos. *sharpen* is the phone's Peak, a high-pass sharpening of that print: the picture plus an amount of itself minus a Gaussian blur. The radius is that blur, in print pixels, measured on the negative and multiplied by the print's scale, including a reduction. The amount does not change. The two numbers are offered while the sharpening is on, and *Reset to measured* puts them back. The box above it is those print pixels at 1:1; drag it, or drag the crosshair on the sheet. The Export button stands at the foot of the window and stays there when the rows above it scroll; it names the file it writes, and the line under it says how much memory the export holds at once. The recipe is saved next to the RAW, with the process, the mix or the filters and the lamps, and comes back when the file is opened. Leaving a changed recipe asks first. A recipe the command line wrote with its unsharp mask or Richardson–Lucy, or through the neutral look, opens without that sharpening and on paper: the window says so, and the recipe counts as changed.

*Negative…* under the Export button is the way out of the darkroom for anyone who wants Mimizan's separation and their own converter on top: the picture without paper, lamps or border, turned and cropped as the recipe says, nothing else applied. In the monochrome process it is the mixed negative as a linear 16-bit grey TIFF with the mask beside it: the same samples and description as the file `mimizan negative` writes, but turned upright and cut to the crop, where the command keeps the sensor frame and records the orientation in the tag. Where highlights stand above 1 the samples are divided by the highest value and the description names it as `range`; Mimizan multiplies it back, and a program that does not read it shows the picture that much darker. In the plate process it is a linear DNG: the three balanced channels of the separation with the white balance undone, three 16-bit samples per pixel, the camera's colour matrix and as-shot white balance in the tags. A RAW converter opens it as a RAW of that camera and has no demosaicing left to do; white balance, profile and tone are its own. Every sample is still a weighted sum of measured photosites; the colour in it is band-limited to the carriers, under the full-resolution luminance. The DNG ends at its white level, as a RAW does.

## Get it

Prebuilt builds are on the [releases page](https://github.com/Draganito/mimizan/releases).

The separation works in double precision on the whole frame: a 24-megapixel file holds about 1.8 GB while it is developed, 45 megapixels 3.4 GB, 61 megapixels 4.5 GB, and the open picture keeps most of that afterwards. When the next file would not fit beside the open one, the desktop lets the open one go first. What still does not fit in the free memory is begun through the swap: on Linux for as long as the swap has room, and on macOS and Windows the page file grows and the file is started. A file past the swap as well is refused with the figures in the log rather than started.

Debian and Ubuntu: [mimizan_0.20.14-1_amd64.deb](https://github.com/Draganito/mimizan/releases/download/v0.20.14/mimizan_0.20.14-1_amd64.deb). Install the downloaded file:

```bash
sudo apt install ./mimizan_0.20.14-1_amd64.deb
```

That installs the darkroom window, `mimizan-gui`, the `mimizan` command, and `mimizan-compare`.

Windows, 64-bit Intel and AMD: [mimizan-0.20.14-x86_64-windows.zip](https://github.com/Draganito/mimizan/releases/download/v0.20.14/mimizan-0.20.14-x86_64-windows.zip). Unzip it and start `mimizan-gui.exe`. `mimizan-compare.exe` is the loupe in the same folder. Leave `cameras` and `look` next to the programs. The programs carry the Visual C++ runtime, and the window opens without a terminal. An unsigned download asks SmartScreen for confirmation. The same program is on the [Microsoft Store](https://apps.microsoft.com/detail/9ph4p5qb18z5), signed. That package has no loupe.

Apple silicon: [mimizan-0.20.14-aarch64-macos.zip](https://github.com/Draganito/mimizan/releases/download/v0.20.14/mimizan-0.20.14-aarch64-macos.zip). Unzip it and open `Mimizan Lab.app`. The command line is `Mimizan Lab.app/Contents/MacOS/mimizan`, and `mimizan-compare` sits next to the app. The programs use the frameworks that come with macOS. The first launch is a right-click and Open. A signed and sandboxed build of the window, without the command and without the loupe, is uploaded to App Store Connect for TestFlight; it is not on the Mac App Store yet. That build alone has a purchase: the JPEG at full size is free there, and the TIFF and the negative for another program are one purchase, made once and shared with the iPhone. This download, Linux and Windows have no purchase and lock nothing.

If macOS says the app is damaged and should be moved to the Trash, it is blocking the unsigned download. Open a terminal next to the unzipped app and run this to clear the quarantine flag and sign it locally:

```bash
xattr -cr "Mimizan Lab.app" mimizan-compare && codesign --force --deep -s - "Mimizan Lab.app" && codesign --force -s - mimizan-compare
```

From source (a recent stable Rust, a C compiler for the small allocator mimalloc, and on Linux and macOS a working OpenGL for the window; Windows uses DirectX 12, which comes with the system):

```bash
git clone https://github.com/Draganito/mimizan
cd mimizan
cargo build --release
./target/release/mimizan-gui
```

The same paper is on the phone: [Mimizan Lab Mobile](https://github.com/Draganito/mimizan-lab-mobile) on Android and on iOS (TestFlight).

A short handbook for the window and the phone: [docs/HANDBOOK.md](docs/HANDBOOK.md).

Free software, GPL-3.0-or-later.

## Privacy

Mimizan Lab runs on your computer. It does not send photographs, settings, or anything else anywhere.

## The command

The window is the darkroom. The command is there when the print is already decided.

```bash
mimizan info photo.nef
mimizan negative photo.nef -o photo_neg.tif
mimizan print photo_neg.tif -o photo_print.tif --size 30x40cm --paper 0,-0.5 --border-mm 20
```

`info` says what is in the file. `negative` writes a 16-bit linear TIFF, and a mask beside it. `print` writes the sheet: half a stop less hard light, a 20 mm paper-white border. A `-o` that ends in `.jpg` writes the 8-bit JPEG instead of the 16-bit TIFF. `--bits 8` writes the TIFF in 8 bits, and `--rgb` writes the grey sheet as RGB with three equal channels, in sRGB: together the file a print service takes. `--angle -1.5` straightens the picture a degree and a half anticlockwise before the crop, as the window's *Straighten* does.

```bash
mimizan negative photo.nef -o photo_plate.tif --plate              # camera filters; or --plate wratten
mimizan print photo_plate.tif -o photo_colour.tif --size 30x40cm --lamp 0.1,-0.1
```

`--plate` writes the three-frame plate instead of the negative; `print` recognises it and writes the RGB sheet, here with a tenth of a stop more red lamp and less blue. The primaries are the plate's: sRGB, unless `negative` was given `--plate-space adobe-rgb`. The sRGB file is written on the sRGB curve under the profile *sRGB IEC61966-2.1*; the Adobe RGB file keeps the gamma 2.2 under a profile named *compatible with Adobe RGB (1998)*.

```bash
mimizan negative photo.nef -o photo.dng --dng
```

`--dng` writes the linear DNG instead: the channels of the separation for another converter, the camera's matrix and white balance in the tags, no mask, the recipe's 90° turns, straighten and crop applied when `--recipe` names them.

```bash
mimizan negative photo.nef -o photo_plate.tif --recipe photo.recipe.json
mimizan print photo_plate.tif -o photo_colour.tif --recipe photo.recipe.json
```

The file the desktop saves next to the RAW carries the whole development, white balance and mask, process, mix or filters, lamps and print; the two commands read it and reproduce the window's export. `--wb` and `--mask` on `negative` override the recipe's, as `--weights` and `--plate` do. The rest is `mimizan --help`.

The window can photograph itself, for a handbook or a store page. `MIMIZAN_SCREENSHOT=shot.png mimizan-gui photo.nef` opens the file, writes the window's picture as a PNG once the print and its 1:1 box are on screen, and quits. The pointer is left out of it, so no hover is in the picture, wherever the mouse rests. `MIMIZAN_WINDOW=1280x800` sets the size in points, and `MIMIZAN_WINDOW_BARE=1` leaves the frame off and holds that size, so the picture has the pixels asked for: 2560 × 1600 on a screen at scale 2.

## The method

The formulas and the constants are in [the paper](docs/PAPER.md). The note it started from is [the whitepaper](docs/whitepaper_v1.pdf).
