# Mimizan Lab

A darkroom for the digital negative.

You open a colour RAW. Mimizan Lab makes a monochrome negative from it and prints that negative the way a split-grade enlarger prints a sheet of paper: one placing, a soft exposure and a hard one.

Mimizan Lab also accepts a file from a monochrome sensor, for anyone who wants to develop it here. That sensor has no colour filter, so the file opens as the measurement.

It is for analog photographers and fine-art printers who decide the print on the paper.

The name is the beach at Mimizan, where the idea came up watching the surf: separate the waves, and keep the picture underneath. Lab is the darkroom.

![A session in Mimizan Lab: the print in the middle, the enlarger below, the sheet on the right, the folder as a film strip](docs/gui.png)

## A session

The folder lies along the bottom as a strip of negatives. It closes when you click the picture and release, or drag the crosshair and release; B brings it back, and the arrow keys still change the file. The picture in the middle is the print. A white border is added around it, and a black edge can sit inside that white, never wider than it, so the sheet is judged as it will go on the wall.

*Enlarger* holds the mix and the only tone controls a sheet of paper has. *soft* is filter 00, *hard* is filter 5, both in stops of time from the base print. The zone bar is that paper from its black to its white. More soft light greys the highlights. Less hard light opens the shadows. A drag on the bar is one more or less second under the enlarger. The line under the sliders names the single grade this pair equals, and how many stops of the scene the paper holds.

*Crop* frames a darkroom ratio: 4×5, 5×7, 8×10, 11×14, 16×20, 20×24, or square. *Turn* stands that ratio portrait on a landscape negative. *Apply* makes the window the picture, in whole pixels.

*Print* sets the sheet, the dpi and the border, and writes one 16-bit TIFF. The recipe is saved next to the RAW and comes back when the file is opened. Leaving a changed recipe asks first.

## The negative

A colour sensor records luminance and chrominance in one mosaic. Mimizan Lab takes the luminance straight from that mosaic. The default path does not demosaic. There is no hidden tone curve. Every step is measured, and the same recipe gives the same bytes.

The print is a sheet of Ilford Multigrade. The soft pass is the measured grade-00 curve (ISO R 160). The hard pass is grade 5 (ISO R 50). The preview is that print.

It opens a Bayer RAW, and a file from a sensor with no colour filter. Not a RAW converter: no colour output, no noise reduction, no lens correction. X-Trans, Quad-Bayer, Foveon and Nikon HE/HE* are not supported.

## Get it

Prebuilt builds are on the [releases page](https://github.com/Draganito/mimizan/releases).

Debian and Ubuntu:

```bash
sudo apt install ./mimizan_0.11.14-1_amd64.deb
```

That installs the darkroom window, `mimizan-gui`, and the `mimizan` command.

Windows, 64-bit Intel and AMD: unzip `mimizan-0.11.14-x86_64-windows.zip` and start `mimizan-gui.exe`. Leave `cameras` and `look` next to the two programs. The programs carry the Visual C++ runtime, and the window opens without a terminal. An unsigned download asks SmartScreen for confirmation.

Apple silicon: unzip `mimizan-0.11.14-aarch64-macos.zip` and open `Mimizan Lab.app`. The command line is `Mimizan Lab.app/Contents/MacOS/mimizan`. The programs use the frameworks that come with macOS. The first launch is a right-click and Open.

From source (a recent stable Rust, a C compiler for the small allocator mimalloc, and a working OpenGL for the GUI):

```bash
git clone https://github.com/Draganito/mimizan
cd mimizan
cargo build --release
./target/release/mimizan-gui
```

![Mimizan Lab on 64-bit Windows](docs/windows.png)

The same paper is on the phone: [Mimizan Lab for Android](https://github.com/Draganito/mimizan-lab-mobile).

Free software, GPL-3.0-or-later.

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
