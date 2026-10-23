# Mimizan Lab

A darkroom for the digital negative.

You open a colour RAW. Mimizan Lab makes a monochrome negative from it and prints that negative the way a split-grade enlarger prints a sheet of paper: one placing, a soft exposure and a hard one.

Mimizan Lab also accepts a file from a monochrome sensor, for anyone who wants to develop it here. That sensor has no colour filter, so the file opens as the measurement.

It is for analog photographers and fine-art printers who decide the print on the paper.

> Lift the colour off. Print what remains.
>
> Pure monochrome. A new split-grade digital darkroom, authentic for fine-art monochrome. A minimal, honest darkroom. The Bayer file is read as frequencies: luminance below, colour on the carriers. No demosaicing. The pixels stay measured. Only a small remainder is reconstructed, and the character and soul of the lens are left unchanged. Focus on the picture, and on what you want to say with it. Your art.
>
> Dragan Bojovic

The name is the beach at Mimizan, where the idea came up watching the surf: separate the waves, and keep the picture underneath. Lab is the darkroom.

![Mimizan Lab with a file open: the negative on the left, the print in the middle with the crop table, the enlarger below, the sheet and its 1:1 box on the right](docs/gui.jpg)

## The negative

A colour sensor records one signal. The luminance sits in the low frequencies. The colour rides on carriers at the highest frequency the sensor can hold. [Alleysson, Süsstrunk and Hérault](https://doi.org/10.1109/TIP.2004.841200) described that signal. [Dubois](https://doi.org/10.1109/LSP.2005.859503) showed how to lift the colour off the carriers. Their papers reconstruct a colour picture. A monochrome photograph needs only what remains once the colour is gone.

A RAW converter invents a red, a green and a blue at every photosite, by interpolation and, in the methods that lead these comparisons, by a guess about the local edge. A later step adds the three and keeps one. The guesses stay in the grey: zippers, mazes, and a colour error that has become fine texture. On a large print that texture is the part that never belonged to the lens.

Mimizan Lab subtracts the colour and keeps the luminance. On this path a pixel is a sum of photosites the sensor measured. Detail the lens put on the mosaic stays detail.

Alleysson and Dubois were not quite enough for a photograph. The separation runs on white-balanced photosites, as the camera metered them, so a grey subject puts nothing on the carriers and those carriers can carry real detail. The colour is estimated in two directions, in the way Dubois estimates it, and the copy the luminance has leaked into is trusted less. Where both copies agree, the colour band is opened and the detail underneath comes back. The white balance before the separation, the use of both colour components, and that agreement test are the part built here.

The command stops there. The window goes one step further, where luminance and colour really overlap: those samples are computed, and the mask beside the negative records, pixel by pixel, how much was calculated. On the Kodak set at the band limit of a lens, that share is 5%. On the Nikon Z f files at the sharpness the camera recorded, it is 0.5%. Where a lens-like picture still reaches the sensor, it is 12%.

The print is a measured sheet of Ilford Multigrade. Soft is grade 00, ISO R 160. Hard is grade 5, ISO R 50. There is no hidden tone curve. The preview is that paper, and the TIFF is that sheet. The same recipe gives the same bytes.

On the Kodak set, brought to the band limit of a photograph, the linear negative is 50.8 dB. The best demosaicer in the test, RCD, is 48.9 dB. Twenty-three of the twenty-four pictures are above every demosaicer we ran: bilinear, AHD, DHT and AMaZE in libraw, RCD in RawTherapee. With the window's extra step the mean is 52.0 dB, 5% of the pixels are computed, and all twenty-four pictures are at or above the best of them. On four Nikon Z f files at the sharpness the camera recorded, the linear mean is 51.9 dB against 50.7 dB for RCD. Three of the four are above it. One is 0.13 dB below. Where a lens-like picture still reaches the sensor, the linear mean is 50.2 dB and RCD is 50.6 dB. The window's step puts that mean at 51.9 dB, with 12% computed. On detail sharper than any lens delivers, AMaZE leads the linear negative, 40.7 dB to 36.6 dB. The extra step passes that Kodak test at 42.0 dB, and 49% of the pixels are computed. On the four Z f files made twice as sharp as the camera saw them, RCD still leads, with the step and without it. The means, and how many pictures lead, are in the paper.

It opens a Bayer RAW, and a file from a sensor with no colour filter. Not a RAW converter: no colour output, no noise reduction, no lens correction. X-Trans, Quad-Bayer, Foveon and Nikon HE/HE* are not supported.

## A session

Drop a RAW file or a folder on the window, or open one. The folder lies along the bottom as a strip of negatives. It closes when you click the picture and release, or drag the crosshair and release; B brings it back, and the arrow keys still change the file. Tab hides the three panels. Scroll zooms the print about the pointer; a double click switches between fitted and 1:1. Undo is Ctrl+Z, redo is Ctrl+Shift+Z or Ctrl+Y. Reset puts the controls back to their defaults and leaves the negative where it is.

The picture in the middle is the print. A white border is added around it, and a black edge can sit inside that white, never wider than it, so the sheet is judged as it will go on the wall. The crosshair marks the spot the 1:1 box in *Print* is reading.

*Negative* is the file and the development. The histogram reads the print, the linear negative, or the raw channels, on a linear or a log axis. White balance is as shot, gray world, none, or a manual red and blue. The mask is Dubois, adaptive, or off. *Reconstruct the last octave* computes the samples where luminance and colour overlap; the mask beside the negative records where. *Repair defects* fills dead photosites. Apply recomputes the picture. A monochrome sensor has none of the colour controls: the file is the measurement.

*Enlarger* holds the mix and the only tone controls a sheet of paper has. The mix is Native, one quarter, one half, one quarter, or Panchromatic when a camera file is loaded, or a filter in front of that: yellow 8, yellow-green 11, orange 16, red 25, green 58, blue 47. R and B move the same mix by hand; G is what remains, and the three sum to one. *soft* is filter 00, *hard* is filter 5, both in stops of time from the base print, two stops either way. *base* returns there. The zone bar is that paper from its black to its white. More soft light greys the highlights. Less hard light opens the shadows. A drag on the bar is one more or less second under the enlarger, and moves both exposures together. The line under the sliders names the single grade this pair equals, and how many stops of the scene the paper holds.

*Crop* frames the picture. The list opens as a table: the picture's own ratio, read from its pixels, then each sheet both ways — 4×5 and 5×4, on through 20×24 — and a square across the row. The side that is framed is sunk. *Apply* makes that window the picture, in whole pixels. Enter does the same. 90° turns the picture itself.

*Print* sets the sheet and writes one 16-bit TIFF. Native keeps the negative's pixels and writes the dpi into the file. Paper fits the picture into a size in centimetres at that dpi; the black edge and the white border are added around it. Resample is B-spline by default, or area (each output pixel is the source area it covers: a mean when shrinking, the pixel held when enlarging), Mitchell, Catmull–Rom, or Lanczos. Peak is a high-pass sharpening of that print: the picture plus an amount of itself minus a Gaussian blur. The radius is that blur, in print pixels, measured on the negative and multiplied by the print's scale, including a reduction. The amount does not change. The box under it is those print pixels at 1:1; drag it, or drag the crosshair on the sheet. The recipe is saved next to the RAW and comes back when the file is opened. Leaving a changed recipe asks first.

## Get it

Prebuilt builds are on the [releases page](https://github.com/Draganito/mimizan/releases).

Debian and Ubuntu: [mimizan_0.11.28-1_amd64.deb](https://github.com/Draganito/mimizan/releases/download/v0.11.28/mimizan_0.11.28-1_amd64.deb). Install the downloaded file:

```bash
sudo apt install ./mimizan_0.11.28-1_amd64.deb
```

That installs the darkroom window, `mimizan-gui`, and the `mimizan` command.

Windows, 64-bit Intel and AMD: [mimizan-0.11.28-x86_64-windows.zip](https://github.com/Draganito/mimizan/releases/download/v0.11.28/mimizan-0.11.28-x86_64-windows.zip). Unzip it and start `mimizan-gui.exe`. Leave `cameras` and `look` next to the two programs. The programs carry the Visual C++ runtime, and the window opens without a terminal. An unsigned download asks SmartScreen for confirmation. The same program is on the [Microsoft Store](https://apps.microsoft.com/detail/9ph4p5qb18z5), signed.

Apple silicon: [mimizan-0.11.28-aarch64-macos.zip](https://github.com/Draganito/mimizan/releases/download/v0.11.28/mimizan-0.11.28-aarch64-macos.zip). Unzip it and open `Mimizan Lab.app`. The command line is `Mimizan Lab.app/Contents/MacOS/mimizan`. The programs use the frameworks that come with macOS. The first launch is a right-click and Open.

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

`info` says what is in the file. `negative` writes a 16-bit linear TIFF, and a mask beside it. `print` writes the sheet: half a stop less hard light, a 20 mm paper-white border. The rest is `mimizan --help`.

## The method

The formulas and the constants are in [the paper](docs/PAPER.md). The note it started from is [the whitepaper](docs/whitepaper_v1.pdf).
