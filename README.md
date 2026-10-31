# Mimizan Lab

A darkroom for the digital negative, with two processes.

You open a colour RAW. Mimizan Lab reads the Bayer mosaic as a signal, lifts the colour off its carriers and keeps the luminance underneath: a negative in which every pixel is a sum of photosites the sensor measured. No demosaicing, in either process.

**Monochrome.** The negative is printed the way a split-grade enlarger prints a sheet of paper: one placing, a soft exposure and a hard one. A measured Ilford Multigrade, no hidden curve.

**Plate.** The same separation is mixed three times, through three taking filters, into three plates, and the three are printed together through the same paper and the same two exposures into one colour file: Prokudin-Gorsky's three-colour process on the Bayer mosaic. Two lamps, red and blue, are the only colour control.

The switch between the two is in the toolbar, and the whole darkroom follows it: the strip of film, the Enlarger, the print. A file from a monochrome sensor opens as the measurement; that sensor has no colour filter and no plate.

It is for analog photographers and fine-art printers who decide the print on the paper.

![Dragan Bojovic, a plate printed with Mimizan Lab: Nikon Z f, Micro-Nikkor 60 mm, Wratten filters, Multigrade soft −1.26 hard +0.33, a 5.7 cm sheet with a 3.5 mm white border and a 0.1 mm film edge, 676 × 676 pixels at 300 dpi, shown 1:1 inside a one-pixel line that is not part of the print](docs/dragan.jpg)

> Lift the colour off. Print what remains.
>
> Pure monochrome. A new split-grade digital darkroom, authentic for fine-art monochrome. A minimal, honest darkroom. The Bayer file is read as frequencies: luminance below, colour on the carriers. No demosaicing. The pixels stay measured. Nothing is reconstructed, and the character and soul of the lens are left unchanged. Focus on the picture, and on what you want to say with it. Your art.
>
> Dragan Bojovic

The name is the beach at Mimizan, where the idea came up watching the surf: separate the waves, and keep the picture underneath. Lab is the darkroom.

![Mimizan Lab in the plate process: the negative on the left, the colour print in the middle at 21 %, the Enlarger below with the taking filters, the primaries, soft and hard light and the two lamps, the sheet and its 1:1 box with the measured sharpening on the right](docs/gui.jpg)

## The negative

> With Mimizan your digital negative is a negative again: every tone comes from the light, none from an algorithm guessing what might have been there. And that is exactly why, where your lenses really resolve, it draws finer than any demosaicer.

A colour sensor records one signal. The luminance sits in the low frequencies. The colour rides on carriers at the highest frequency the sensor can hold. [Alleysson, Süsstrunk and Hérault](https://doi.org/10.1109/TIP.2004.841200) described that signal. [Dubois](https://doi.org/10.1109/LSP.2005.859503) showed how to lift the colour off the carriers. Their papers reconstruct a colour picture. A monochrome photograph needs only what remains once the colour is gone.

A RAW converter invents a red, a green and a blue at every photosite, by interpolation and, in the methods that lead these comparisons, by a guess about the local edge. A later step adds the three and keeps one. The guesses stay in the grey: zippers, mazes, and a colour error that has become fine texture. On a large print that texture is the part that never belonged to the lens.

Mimizan Lab subtracts the colour and keeps the luminance. On this path a pixel is a sum of photosites the sensor measured. Detail the lens put on the mosaic stays detail.

Alleysson and Dubois were not quite enough for a photograph. The separation runs on white-balanced photosites, as the camera metered them, so a grey subject puts nothing on the carriers and those carriers can carry real detail. The colour is estimated in two directions, in the way Dubois estimates it, and the copy the luminance has leaked into is trusted less. Where both copies agree, the colour band is opened and the detail underneath comes back. The white balance before the separation, the use of both colour components, and that agreement test are the part built here.

Both stop there. Where the colour is strong, the two carrier copies alone cannot say which one the luminance has spoiled, so the separation also reads the direction of the structure from the part of the signal below the carriers, which is free of colour. That is still a choice between measured sums, never a computed value. It is also the last step. Earlier versions had a switch beyond it that computed the samples where luminance and colour truly overlap; 0.15 removed it. On the Nikon Z f files at the sharpness the camera recorded it touched 0.7% of the pixels and cost 0.3 dB, and on every real lens the linear negative was already ahead. What remains is a pipeline with no computed pixel anywhere.

### How the separation works

The Bayer mosaic is a modulated signal. Multiply a picture by the pattern of the colour filters and the luminance stays where it was, in the low frequencies, while the two colour differences are shifted to the corners of the spectrum: one to the diagonal carrier, the other to two copies, one at the horizontal limit and one at the vertical. The grey photograph is what sits below those carriers. Separating it means estimating the colour on the carriers and subtracting it, so that each pixel of the negative is a weighted sum of photosites the sensor measured, and nothing else.

The two copies of the second colour difference ought to be identical. They are not, because luminance leaks into them: fine vertical detail reaches the copy at the horizontal limit, fine horizontal detail reaches the other. Dubois measured the energy around each copy and trusted the one with less, since the leak adds energy. That works until the colour is strong. Then both copies carry a lot of energy, the weight falls back to one half, and the leak passes into the grey as texture that was never in the picture.

Since 0.12 the separation also reads the base band. Below the carriers the mosaic is free of colour, so the orientation of the structure can be measured there without the colour getting in the way: second differences along x and along y, squared and smoothed, give a structure tensor, and its anisotropy says whether the structure runs horizontally or vertically. That orientation tells which copy the luminance can have leaked into, and that copy is trusted less, even where the colour is strong. The decision is no longer a guess between two spoiled copies; it is measured on the part of the signal that cannot be spoiled.

A third measurement opens the colour band again. Where the two copies agree, their correlation is high, the colour there is real and the band can be opened, so the detail underneath, the last octave the lens delivered, comes back. In 0.12 the tensor also opens the band of the copy it has found clean. All of this is weights: energies, an orientation, a correlation, each of them measured on the file. No value is computed from a model of what an edge should look like. The same file and the same recipe give the same bytes, on any machine and any number of threads.

That is why the negative is finer where a lens really draws. A demosaicer invents two of the three colour values at every photosite and needs an assumption about the local edge to do it; the error of that assumption survives the sum to grey. The separation leaves the luminance intact up to the carriers and takes away only the colour. Detail a lens puts below the band limit of the sensor passes through untouched, which is where the 2.4 to 4.3 dB over RCD on the Z f files and the twenty-four of twenty-four at the band limit come from. Only where the detail is sharper than any lens delivers do luminance and colour truly overlap in the spectrum; there the band limit of the sensor is the limit of the picture, and Mimizan leaves it so rather than compute what the lens did not deliver.

## The monochrome print

The print is a measured sheet of Ilford Multigrade. Soft is grade 00, ISO R 160. Hard is grade 5, ISO R 50. There is no hidden tone curve. The preview is that paper, and the TIFF is that sheet. The same recipe gives the same bytes.

On the Kodak set, brought to the band limit of a photograph, the linear negative is 51.8 dB. The best of the sixteen demosaicers we ran, DCB in RawTherapee, is 49.1 dB; RCD is 48.9 dB. All twenty-four pictures are above every one of them: VNG, PPG, AHD, DCB, DHT and AAHD in libraw, and fast, VNG4, AHD, EAHD, HPHD, DCB, LMMSE, IGV, AMaZE and RCD in RawTherapee. On four Nikon Z f files at the sharpness the camera recorded, the mean is 53.9 dB against 50.7 dB for RCD, and all four are above it by 2.4 to 4.3 dB. Where a lens-like picture still reaches the sensor, the mean is 51.6 dB and DCB is 51.5 dB; nine of the twenty-four lead every demosaicer. On detail sharper than any lens delivers, LMMSE in RawTherapee leads, 43.1 dB to 37.3 dB. On the four Z f files made twice as sharp as the camera saw them, RCD leads by 0.5 to 1.9 dB. Those last two conditions are where the removed nonlinear step used to help, and they are conditions no camera produces. The means, and how many pictures lead, are in the paper.

It opens a Bayer RAW, and a file from a sensor with no colour filter. Not a RAW converter: no noise reduction, no lens correction, no saturation, no hue. X-Trans, Quad-Bayer, Foveon and Nikon HE/HE* are not supported.

## The plate

> Three plates, one paper, two lamps. The colour is a relation of exposures, not a value at a pixel.

**Where it comes from.** In 1902 Adolf Miethe built a camera that exposed one long glass plate three times in a row, through a blue, a green and a red filter. Sergei Prokudin-Gorsky took it across Russia from 1909 to 1915: three to four seconds for the three exposures, the red one longer because the plates were weakest there. The ratio of those three times was the white balance. The negative was contact-copied onto one positive plate, so the three positives shared one exposure and one development, and the relation of their densities was kept. Projected through three lenses and the same three filters, the light added up on the screen; the lamps and the register of the side lenses were the only controls. The Library of Congress today scans those plates, cuts them into three frames, registers red and blue on green and puts them straight into the RGB channels. Colour film did the same thing later, in three layers at once, with dyes instead of light, each layer with its own curve, and the red layer, at the bottom of the pack, resolving least. Colour has always been softer than drawing.

**How it is done here.** The separation of the negative gives three things: the luminance, and the two colour differences lifted off the carriers. The monochrome mix adds them up with one set of weights. The plate adds them up three times, with three sets of weights, the taking filters:

```
R̂ = L̂ − Ĉ1' + 2Ĉ2'      Ĝ = L̂ + Ĉ1'      B̂ = L̂ − Ĉ1' − 2Ĉ2'
```

Every plate is the same full-resolution luminance with a different band-limited colour added. Every pixel of every plate is still a weighted sum of photosites the sensor measured; no value is interpolated, no edge is guessed. The three plates are in register by construction, since they come from one separation. The taking filters are the camera's own colour matrix towards the output primaries (the default, colorimetric), Wratten 25, 58 and 47 in front of the panchromatic mix (the softer palette of the historical glass), or the bare channels, as the Library of Congress does. The white balance stays where it was, before the separation, as the camera metered it: it is the ratio of the three exposure times, and it is what makes a grey subject give three equal plates. It also sets where each channel burns out, and green burns out first. Where a green photosite is burnt out, every colour difference around it is false, towards magenta, whatever the scene was; there the plates carry no colour and take the luminance, and the highlight goes to paper white in all three, as it is in the monochrome print. Where only red or blue burn out, the colour stays, a lower bound in the right direction. Nothing is reconstructed.

The negative is written as one grey TIFF with the three frames stacked, blue, green, red from the top, the way the glass looked. The print runs its one pass three times over those frames, with the same crop, the same paper and the same two exposures; the three positives become the channels of a 16-bit RGB TIFF with an Adobe RGB or sRGB profile. The two lamps, red and blue, add stops to their positive; green is the reference. That is the only colour control. There is no saturation, no hue, no per-channel curve.

**What it gives.** The drawing of the colour print is, pixel for pixel, the drawing of the monochrome negative: red, green and blue carry the same edges in the same places, so there are no colour fringes, no zippers, no mazes, and the detail a lens put on the mosaic stays where it was. Grey stays grey at every density, since all three channels go through one paper curve from one pair of exposures, which film never managed (crossover). Saturated colours run towards white in the shoulder and darker in the toe, the look of a slide. The colour itself is band-limited to about a quarter of the sampling, half as fine as the drawing, and lies under a sharp luminance as a soft layer: the structure of a tripack film, or of a dye-transfer print, and the reason the pictures read as prints rather than as files. The noise is film's too: fine luminance grain, and under it colour mottle, slow and soft, never a pixel-sized rainbow.

Measured on the Kodak set against sixteen demosaicers of libraw and RawTherapee, CPSNR: behind a lens the plate is 45.0 dB against 46.2 for RCD and LMMSE; at twice the sampling 47.2 dB against 45.4, nineteen of twenty-four ahead. Only on pixel-sharp charts, which no camera produces, does the band limit cost it against the edge-directed demosaicers. The green channel leads everywhere by 2 to 5 dB: it is `L̂ + Ĉ1'`, not interpolated.

In the window the plate is the second process: *Plate* in the toolbar, next to *Monochrome*, or the key P. The picture turns to colour at once, fitted, at 1:1 and in the print box; it is a re-mix of the separation, not a re-development. The Enlarger shows the taking filters and the primaries where the monochrome mix stood, and the two lamps under the paper's exposures. The strip shows the folder as slides. The Print panel says what it will write, a 16-bit RGB TIFF with the profile. The recipe saved next to the RAW remembers the process, the filters and the lamps, so the file opens again in colour. On the command line it is `--plate` on `negative` and `--lamp R,B` on `print`, or `--recipe` with the desktop's file on both. [docs/PLATE.md](docs/PLATE.md) has the method, the file format and the measurements; the paper has it in §8.7.

## A session

Drop a RAW file or a folder on the window, or open one. The folder lies along the bottom as a strip of film: negatives in the monochrome process, slides in the plate, either one inverted on request. It closes when you click the picture and release, or drag the crosshair and release; B brings it back, and the arrow keys still change the file. Tab hides the three panels.

*Monochrome* and *Plate* in the toolbar, or P, are the process. The choice is the darkroom's: the strip, the Enlarger and the print follow it, and the next file opens in it. A file with a recipe beside it opens in the process the recipe was saved in. Scroll zooms the print about the pointer; a double click switches between fitted and 1:1. Undo is Ctrl+Z, redo is Ctrl+Shift+Z or Ctrl+Y. Reset puts the controls back to their defaults and leaves the negative where it is.

The picture in the middle is the print. A white border can be added around it, a black film edge against the picture, or both, the black inside the white, so the sheet is judged as it will go on the wall. The crosshair marks the spot the 1:1 box in *Print* is reading.

*Negative* is the file and the development. The histogram reads the print, the linear negative, or the raw channels, on a linear or a log axis. White balance is as shot, gray world, none, or a manual red and blue; *card* reads a grey card in the picture on the next click, 32×32 photosites averaged without the clipped ones, and puts the balance into manual. The mask is Dubois, adaptive, or off. Apply recomputes the picture. The *defects* line counts the photosites that lie far outside their neighbours; the desktop leaves them as measured, and the command's `--repair-defects` is there for a hot pixel from a long exposure. A monochrome sensor has none of the colour controls: the file is the measurement. The *process* line names what the file is developed in; in the plate, the print histogram carries the three channels as outlines over the mix.

*Enlarger* holds the filter in front of the paper and the only tone controls a sheet of paper has. In the monochrome process the filter is the mix: Native, one quarter, one half, one quarter, or Panchromatic when a camera file is loaded, or a filter in front of that: yellow 8, yellow-green 11, orange 16, red 25, green 58, blue 47. R and B move the same mix by hand; G is what remains, and the three sum to one. *soft* is filter 00, *hard* is filter 5, both in stops of time from the base print, two stops either way. *base* returns there. The zone bar is that paper from its black to its white. More soft light greys the highlights. Less hard light opens the shadows. A drag on the bar is one more or less second under the enlarger, and moves both exposures together. The line under the sliders names the single grade this pair equals, and how many stops of the scene the paper holds. In the plate the filter row is the three taking filters, the camera's matrix, Wratten 25/58/47 in front of the panchromatic emulsion, or the bare channels, and the primaries the plates are projected through. The emulsion behind the Wratten glass is the file's, the camera file's calibrated response or the native one, shown, not chosen: the mix is the filter of one negative and does not reach the plate; two more sliders follow the exposures: the red and the blue lamp of the projector, in stops added to that positive. Green is the reference; grey stays grey at zero.

*Crop* frames the picture. The list opens as a table: the picture's own ratio, read from its pixels, then each sheet both ways — 4×5 and 5×4, on through 20×24 — and a square across the row. The side that is framed is sunk. *Apply* makes that window the picture, in whole pixels. Enter does the same. 90° turns the picture itself.

*Print* sets the sheet and writes one 16-bit TIFF; its first line says which, grey with the paper's gamma, or RGB with the Adobe RGB or sRGB profile in the plate. The size fields are the sheet in the printer, always in the crop's proportion: type one side, the other follows the picture, and the file is that sheet, turned the picture's way. The black edge and the white border are taken from inside it, each on its own or black inside white, and the picture fills what they leave, centred; with rings, a sheet of the picture's shape leaves paper beyond the border on two sides, as on an easel. A new crop is a new picture, and the sheet takes its proportion with its long side held. The line under the fields says how large the picture comes out and by what scale. A file opens at 1:1, the sheet that is the negative's pixels with nothing around them, and the 1:1 button returns there. The ruler at the right of the size row shows every length in centimetres and millimetres, in inches, or in pixels of the file; the recipe keeps inches whichever is on. The dpi is written into the file; with the cm or in ruler a new dpi keeps the sheet and moves the pixels, with the px ruler it keeps the pixels and moves the sheet. An older recipe without a sheet is read as the sheet its file had. Resample is B-spline by default, or area (each output pixel is the source area it covers: a mean when shrinking, the pixel held when enlarging), Mitchell, Catmull–Rom, or Lanczos. Peak is a high-pass sharpening of that print: the picture plus an amount of itself minus a Gaussian blur. The radius is that blur, in print pixels, measured on the negative and multiplied by the print's scale, including a reduction. The amount does not change. The box under it is those print pixels at 1:1; drag it, or drag the crosshair on the sheet. The recipe is saved next to the RAW, with the process, the mix or the filters and the lamps, and comes back when the file is opened. Leaving a changed recipe asks first.

*Negative…* under the Export button is the way out of the darkroom for anyone who wants Mimizan's separation and their own converter on top: the picture without paper, lamps or border, turned and cropped as the recipe says, nothing else applied. In the monochrome process it is the mixed negative as a linear 16-bit grey TIFF with the mask beside it, the same file the command writes. In the plate process it is a linear DNG: the three balanced channels of the separation with the white balance undone, three 16-bit samples per pixel, the camera's colour matrix and as-shot white balance in the tags. A RAW converter opens it as a RAW of that camera and has no demosaicing left to do; white balance, profile and tone are its own. Every sample is still a weighted sum of measured photosites; the colour in it is band-limited to the carriers, under the full-resolution luminance.

## Get it

Prebuilt builds are on the [releases page](https://github.com/Draganito/mimizan/releases).

The separation works in double precision on the whole frame: a 24-megapixel file holds about 1.8 GB while it is developed, 45 megapixels 3.4 GB, 61 megapixels 4.5 GB, and the open picture keeps most of that afterwards. When the next file would not fit beside the open one, the desktop lets the open one go first; a file that does not fit at all is refused with the two figures in the log rather than started.

Debian and Ubuntu: [mimizan_0.16.5-1_amd64.deb](https://github.com/Draganito/mimizan/releases/download/v0.16.5/mimizan_0.16.5-1_amd64.deb). Install the downloaded file:

```bash
sudo apt install ./mimizan_0.16.5-1_amd64.deb
```

That installs the darkroom window, `mimizan-gui`, and the `mimizan` command.

Windows, 64-bit Intel and AMD: [mimizan-0.16.5-x86_64-windows.zip](https://github.com/Draganito/mimizan/releases/download/v0.16.5/mimizan-0.16.5-x86_64-windows.zip). Unzip it and start `mimizan-gui.exe`. Leave `cameras` and `look` next to the two programs. The programs carry the Visual C++ runtime, and the window opens without a terminal. An unsigned download asks SmartScreen for confirmation. The same program is on the [Microsoft Store](https://apps.microsoft.com/detail/9ph4p5qb18z5), signed.

Apple silicon: [mimizan-0.16.5-aarch64-macos.zip](https://github.com/Draganito/mimizan/releases/download/v0.16.5/mimizan-0.16.5-aarch64-macos.zip). Unzip it and open `Mimizan Lab.app`. The command line is `Mimizan Lab.app/Contents/MacOS/mimizan`. The programs use the frameworks that come with macOS. The first launch is a right-click and Open.

If macOS says the app is damaged and should be moved to the Trash, it is blocking the unsigned download. Open a terminal next to the unzipped app and run this to clear the quarantine flag and sign it locally:

```bash
xattr -cr "Mimizan Lab.app" && codesign --force --deep -s - "Mimizan Lab.app"
```

From source (a recent stable Rust, a C compiler for the small allocator mimalloc, and on Linux and macOS a working OpenGL for the window; Windows uses DirectX 12, which comes with the system):

```bash
git clone https://github.com/Draganito/mimizan
cd mimizan
cargo build --release
./target/release/mimizan-gui
```

The same paper is on the phone: [Mimizan Lab for Android](https://github.com/Draganito/mimizan-lab-mobile).

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

`info` says what is in the file. `negative` writes a 16-bit linear TIFF, and a mask beside it. `print` writes the sheet: half a stop less hard light, a 20 mm paper-white border.

```bash
mimizan negative photo.nef -o photo_plate.tif --plate              # camera filters; or --plate wratten
mimizan print photo_plate.tif -o photo_colour.tif --size 30x40cm --lamp 0.1,-0.1
```

`--plate` writes the three-frame plate instead of the negative; `print` recognises it and writes the RGB sheet, here with a tenth of a stop more red lamp and less blue.

```bash
mimizan negative photo.nef -o photo.dng --dng
```

`--dng` writes the linear DNG instead: the channels of the separation for another converter, the camera's matrix and white balance in the tags, no mask, the recipe's crop applied when `--recipe` names one.

```bash
mimizan negative photo.nef -o photo_plate.tif --recipe photo.recipe.json
mimizan print photo_plate.tif -o photo_colour.tif --recipe photo.recipe.json
```

The file the desktop saves next to the RAW carries the whole development, process, mix or filters, lamps and print; the two commands read it and reproduce the window's export. The rest is `mimizan --help`.

## The method

The formulas and the constants are in [the paper](docs/PAPER.md). The note it started from is [the whitepaper](docs/whitepaper_v1.pdf).
