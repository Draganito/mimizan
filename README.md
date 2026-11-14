# Mimizan Lab

A darkroom for the digital negative, with two processes.

You open a colour RAW. Mimizan Lab reads the Bayer mosaic as a signal, lifts the colour off its carriers and keeps the luminance underneath: a negative in which every pixel is a sum of photosites the sensor measured. No demosaicing, in either process.

**The first photographic application that takes the black-and-white negative directly from the luma-chroma separation of the sensor signal ([Alleysson, Süsstrunk and Hérault 2005](https://doi.org/10.1109/TIP.2004.841200), [Dubois 2005](https://doi.org/10.1109/LSP.2005.859503)) without ever reconstructing an RGB image.**

**Monochrome.** The negative is printed the way a split-grade enlarger prints a sheet of paper: one placing, a soft exposure and a hard one. A measured Ilford Multigrade, no hidden curve.

**Plate.** The same separation is mixed three times, through three taking filters, into three plates, and the three are printed together through the same paper and the same two exposures into one colour file: Prokudin-Gorsky's three-colour process on the Bayer mosaic. Two lamps, red and blue, are the only colour control.

The switch between the two is in the toolbar, and the whole darkroom follows it: the strip of film, the Enlarger, the print. A file from a monochrome sensor opens as the measurement; that sensor has no colour filter and no plate.

It is for analog photographers and fine-art printers who decide the print on the paper.

> Lift the colour off. Print what remains.
>
> Pure monochrome. A new split-grade digital darkroom, authentic for fine-art monochrome. A minimal, honest darkroom. The Bayer file is read as frequencies: luminance below, colour on the carriers. No demosaicing. The pixels stay measured. Nothing is reconstructed, and the character and soul of the lens are left unchanged. Focus on the picture, and on what you want to say with it. Your art.
>
> Dragan Bojovic

**Not a filter that looks like film. A darkroom.** Presets paint a look over a finished photo. Mimizan Lab develops a real negative on real paper. Nothing is pretended, because nothing has to be.

**Never invents a pixel.** Every other converter guesses two of three colours per pixel. Mimizan Lab reads the sensor the way analogue television read its signal: luminance and colour separated, nothing interpolated, nothing made up.

**A negative, not a file.** Your photo is a real negative in linear light. You develop it, you don't edit it.

**Black and white like film.** Measured variable-contrast paper, soft and hard light mixed like an enlarger. Highlights roll off the way silver does.

**Colour the way it was first photographed.** Three separate colour plates printed together. Saturated, deep, alive. Two lamps are the only colour control.

**Sharp without the halo.** The measured sharpening stops before the edge becomes a line.

**Printed like a sheet.** Centimetres and dpi, a border and a film edge. The file is a print, not a screen picture.

**Draws finer than any demosaicer.** What your lens puts on the sensor stays in the negative: every tone in the grey is a sum of measured photosites, none is guessed. On real camera files that is 2 to 4 dB closer to the scene than the best of sixteen demosaicers.

**One paper, two darkrooms.** The phone's negative is printed here, by `mimizan print`, through the same paper.

**Your pictures are yours.** No cloud. No AI. No account. Everything happens on your computer. Nothing leaves it.

The name is the beach at Mimizan, where the idea came up watching the surf: separate the waves, and keep the picture underneath. Lab is the darkroom.

![Mimizan Lab in the plate process: a cabin at the Swedish coast, a photograph by seume (CC0) from a Sony ILCE-6000. The toolbar with the film strip, the three panels and the two processes and, in its second row, the 90° turn, Crop and Dodge & burn, and beside them what the picture carries; the Negative window on the left with the file, its development and the histogram of the three channels; the colour print in the middle, fitted, with its white border and its black edge; the Enlarger below with the taking filters, the paper strip and the two lamps; Print on the right with the sheet, its 1:1 box and the sharpening](docs/gui.jpg)

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

On the Kodak set, brought to the band limit of a photograph, the linear negative is 51.8 dB. The best of the sixteen demosaicers we ran, DCB in RawTherapee, is 49.1 dB; RCD is 48.9 dB. All twenty-four pictures are above every one of them: VNG, PPG, AHD, DCB, DHT and AAHD in libraw, and fast, VNG4, AHD, EAHD, HPHD, DCB, LMMSE, IGV, AMaZE and RCD in RawTherapee. On four Nikon Z f files at the sharpness the camera recorded, the mean is 53.9 dB against 50.7 dB for RCD, and all four are above it by 2.4 to 4.3 dB. Where a lens-like picture still reaches the sensor, the mean is 51.6 dB and DCB is 51.5 dB; nine of the twenty-four lead every demosaicer. On detail sharper than any lens delivers, LMMSE in RawTherapee leads, 43.1 dB to 37.3 dB. On the four Z f files made twice as sharp as the camera saw them, RCD leads by 0.5 to 1.9 dB. Those last two conditions are where the removed nonlinear step used to help, and they are conditions no camera produces. The means, and how many pictures lead, are in the paper.

It opens a Bayer RAW, and a file from a sensor with no colour filter. Not a RAW converter: no noise reduction, no lens correction, no saturation, no hue. X-Trans, Quad-Bayer, Foveon and Nikon HE/HE* are not supported.

## The plate

> Three plates, one paper, two lamps. The colour is a relation of exposures, not a value at a pixel.

**Where it comes from.** In 1902 Adolf Miethe built a camera that exposed one long glass plate three times in a row, through a blue, a green and a red filter. Sergei Prokudin-Gorsky took it across Russia from 1909 to 1915: three to four seconds for the three exposures, the red one longer because the plates were weakest there. The ratio of those three times was the white balance. The negative was contact-copied onto one positive plate, so the three positives shared one exposure and one development, and the relation of their densities was kept. Projected through three lenses and the same three filters, the light added up on the screen; the lamps and the register of the side lenses were the only controls. The Library of Congress today scans those plates, cuts them into three frames, registers red and blue on green and puts them straight into the RGB channels. Colour film did the same thing later, in three layers at once, with dyes instead of light, each layer with its own curve, and the red layer, at the bottom of the pack, resolving least. Colour has always been softer than drawing.

**How it is done here.** The separation of the negative gives three things: the luminance, and the two colour differences lifted off the carriers. The monochrome mix adds them up with one set of weights. The plate adds them up three times, with three sets of weights, the taking filters:

```
R̂ = L̂ − Ĉ1' + 2Ĉ2'      Ĝ = L̂ + Ĉ1'      B̂ = L̂ − Ĉ1' − 2Ĉ2'
```

Every plate is the same full-resolution luminance with a different band-limited colour added. Every pixel of every plate is still a weighted sum of photosites the sensor measured; no value is interpolated, no edge is guessed. The three plates are in register by construction, since they come from one separation. The taking filters are the camera's own colour matrix towards the output primaries (the default, colorimetric), Wratten 25, 58 and 47 in front of the panchromatic emulsion, the camera file's calibrated response or the native one (the softer palette of the historical glass), or the bare channels, as the Library of Congress does. The white balance stays where it was, before the separation, as the camera metered it: it is the ratio of the three exposure times, and it is what makes a grey subject give three equal plates. It also sets where each channel burns out, and green burns out first. Where a green photosite is burnt out, every colour difference around it is false, towards magenta, whatever the scene was; there the plates carry no colour and take the luminance, and the highlight goes to paper white in all three, as it is in the monochrome print. Where only red or blue burn out, the colour stays, a lower bound in the right direction. Nothing is reconstructed.

The negative is written as one grey TIFF with the three frames stacked, blue, green, red from the top, the way the glass looked. The print runs its one pass three times over those frames, with the same crop, the same paper and the same two exposures; the three positives become the channels of a 16-bit RGB TIFF with an sRGB profile by default, or Adobe RGB. The two lamps, red and blue, add stops to their positive; green is the reference. That is the only colour control. There is no saturation, no hue, no per-channel curve.

**What it gives.** The drawing of the colour print is, pixel for pixel, the drawing of the monochrome negative: red, green and blue carry the same edges in the same places, so there are no colour fringes, no zippers, no mazes, and the detail a lens put on the mosaic stays where it was. Grey stays grey at every density, since all three channels go through one paper curve from one pair of exposures, which film never managed (crossover). Saturated colours run towards white in the shoulder and darker in the toe, the look of a slide. The colour itself is band-limited to about a quarter of the sampling, half as fine as the drawing, and lies under a sharp luminance as a soft layer: the structure of a tripack film, or of a dye-transfer print, and the reason the pictures read as prints rather than as files. The noise is film's too: fine luminance grain, and under it colour mottle, slow and soft, never a pixel-sized rainbow.

Measured on the Kodak set against sixteen demosaicers of libraw and RawTherapee, CPSNR: behind a lens the plate is 45.0 dB against 46.2 for RCD and LMMSE, ahead of all sixteen on three pictures of the twenty-four; at twice the sampling 47.2 dB against 45.4, ahead on twenty-two. Only on pixel-sharp charts, which no camera produces, does the band limit cost it against the edge-directed demosaicers. The green channel is the plate's best, 3 to 5 dB above its red, and behind a lens and at twice the sampling it is above every demosaicer's green, by 0.3 and 3.4 dB: it is `L̂ + Ĉ1'`, not interpolated.

In the window the plate is the second process: *Plate* in the toolbar, next to *Monochrome*, or the key P. The picture turns to colour at once, fitted, at 1:1 and in the print box; it is a re-mix of the separation, not a re-development. The Enlarger shows the taking filters where the monochrome mix stood, and the two lamps under the paper strip; the primaries are in Print, with the file. The strip shows the folder as slides. The Print panel says what it will write, a 16-bit RGB TIFF with the profile. The recipe saved next to the RAW remembers the process, the filters and the lamps, so the file opens again in colour. On the command line it is `--plate` on `negative` and `--lamp R,B` on `print`, or `--recipe` with the desktop's file on both. [docs/PLATE.md](docs/PLATE.md) has the method, the file format and the measurements; the paper has it in §7.5.

## A session

Drop a RAW file or a folder on the window, or open one. The folder lies along the bottom as a strip of film: negatives in the monochrome process, slides in the plate, either one inverted on request. It closes when you click the picture and release, or drag the crosshair and release; B brings it back, and the arrow keys still change the file. Tab hides the three panels; 1, 2 and 3 show or hide the one that carries that number in the toolbar.

*Monochrome* and *Plate* in the toolbar, or P, are the process. The choice is the darkroom's: the strip, the Enlarger and the print follow it, and the next file opens in it. A file with a recipe beside it opens in the process the recipe was saved in. Scroll zooms the print about the pointer; a double click switches between fitted and 1:1. Undo is Ctrl+Z, redo is Ctrl+Shift+Z or Ctrl+Y. *Reset all* puts every control back to its default, the crop and the strokes with them, and leaves the negative where it is; undo brings them back.

The picture in the middle is the print. A white border can be added around it, a black film edge against the picture, or both, the black inside the white, so the sheet is judged as it will go on the wall. The crosshair marks the spot the 1:1 box in *Print* is reading.

*Negative* is the file and the development. The histogram reads the print, the linear negative, or the raw channels, on a linear or a log axis. White balance is as shot, gray world, none, or a manual red and blue; *card* reads a grey card in the picture on the next click, 32×32 photosites averaged without the clipped ones, and puts the balance into manual. The mask is Dubois, adaptive, or off. Apply recomputes the picture. Both are the file's: the recipe next to the RAW keeps them as `develop`, a file opens as its recipe says, and one without a recipe opens as shot with Dubois, whatever the file before it was set to. The *defects* line counts the photosites that lie far outside their neighbours; the desktop leaves them as measured, and the command's `--repair-defects` is there for a hot pixel from a long exposure. A monochrome sensor has none of the colour controls: the file is the measurement. The *process* line names what the file is developed in; in the plate, the print histogram carries the three channels as outlines over the mix.

*Enlarger* holds the filter in front of the paper and the only tone controls a sheet of paper has. The filter is one row across the window, the same on the phone. In the monochrome process it is the mix: Native, one quarter, one half, one quarter; Pan, the panchromatic response when a camera file is loaded; or a glass in front of that, Y8, YG11, O16, R25, G58, B47, each written in its own colour when it is the one. Click a field or draw the mouse along the row; a word on a field says what the glass does to the picture, and the weights it comes to, R, G and B summing to one, are in the Negative window's process line. Under the row is the paper strip: the scene as a grey wedge, a zone to the stop, from five stops under the grey card to the clip two and a half above it, the card at the mark, each slice printed as the paper stands. A drag on the strip is one more or less second under the enlarger, and moves both exposures together. The wheel over the strip is the grade: up is harder, down is softer, a twelfth of a stop a notch. The mid-grey of the print stays where it is, as when one speed-matched filter is changed for another, and the ramp turns round it; at the base exposure that tone is the grey card. The wheel stops at grade 00 and at grade 5, where the paper has no further grade to give, and sooner where a lighter print leaves the hard light no room: a stop lighter, the soft end is grade 0. A double click is the base print, and a second one brings the pair back; the frame is amber whenever the pair is off the base. The line under the strip names the single grade the pair equals and how many stops of the scene the paper holds; the pair itself, *soft* for filter 00 and *hard* for filter 5 in stops of time from the base print, is in the recipe file, nowhere on the glass. In the plate the row is the three taking filters, Camera (the camera's matrix), Wratten 25/58/47 in front of the panchromatic emulsion, or Channels. The emulsion behind the Wratten glass is the file's, the camera file's calibrated response or the native one, shown in the Negative window, not chosen: the mix is the filter of one negative and does not reach the plate. Two bars follow the strip, the width of the paper and half its height: the red and the blue lamp of the projector, stops added to that positive. Each bar is dark and says nothing: the mark in the middle is zero, the handle stands on the lamp's stops, the letter at the left edge, in the lamp's colour, says which lamp; what the lamp does is in the picture. A click or a drag puts the handle there, the wheel is a twelfth a notch, a double click is zero and a second one brings the lamp back; the frame is amber off zero. Green is the reference; grey stays grey at zero.

*Crop* frames the picture. While it is open, the second row of the toolbar holds what it needs: the sheet, the straighten, and at the right the two ways out. The list opens as a table: the picture's own ratio, read from its pixels, then each sheet both ways — 4×5 and 5×4, on through 20×24 — and a square across the row. The side that is framed is sunk. *Straighten* beside it levels the picture: draw a line along a horizon or a wall, and the negative turns so the line lies level or upright; or type the degrees, up to 45 either way; 0°, or the × beside the number, takes it back. The picture is then the largest rectangle of the negative's proportion that fits inside the turned negative, a little smaller, with no empty corner, and the window lies in that. *Apply crop* makes window and straighten the picture, in whole pixels, one step of the history. Enter does the same. *Cancel*, or Escape, puts the easel down and the picture stays as it was. Until one of the two, the toolbar says *not applied*: undo, *Reset all*, the brush and another file put the easel down without keeping the window. 90° turns the picture itself, without loss; the recipe keeps it as one number, `quarter_turns`, so the file next to the RAW brings the turn back and the command makes the same print.

The straighten is in the recipe as one number, `angle`, and it sits in the pipeline between the 90° turns and the crop, so the crop, the sheet and everything after see a picture and nothing else. It costs nothing where there is none: at 0° the file is the same bytes as before. With one, the turn, the crop and the size step are one resampling, so the print is not resampled twice. Where there is a size step it uses the print's kernel, as the size step always did; a turn alone, at 1:1, uses Catmull–Rom, whatever *Resample* says. That was measured, not assumed: against a scene rendered at eight times the negative's resolution and on a real negative turned there and back, the default B-spline at scale 1 keeps half of the fine detail and loses a fifth of the grain, Lanczos keeps nearly all of it but overshoots a hard highlight by 2–4 %, and Catmull–Rom has the smallest total error at the usual degree or two, keeps 85–93 % of the detail and overshoots by under 1 %. At 1:1 the picture was never resampled before, and that is the kernel nearest to it. The linear exports under *Negative…* are straightened the same way.

*Dodge & burn* is the darkroom's dodging and burning: the hand, or a card, held between the lens and the paper for part of the exposure. The tool is a brush that belongs to the program, as the darkroom worker's tools belong to the worker: made once, kept, used on every negative until it is remade. It has two tips. The *Hand* is an ellipse with a soft edge that is painted along a path; the *Card* is a straight edge with a soft shadow that is pushed from one place to another. With the tool in the hand and *Shape* chosen, the tip stands in the middle of the picture with its handles and nothing in the picture changes: the dots size the hand, the ring turns it or the card, the square is the penumbra (the shadow the hand throws when it is held above the paper, dashed either side of the edge), the number at the centre is the stops, dragged up and down in hundredths (Shift slows the drag), and the row beneath the tip is its light, *both*, *soft* or *hard* (both unless you choose). Enter, or *Paint*, locks the brush and saves it with the program's settings; Escape opens it again, and a second Escape puts the tool down. Locked, the tip follows the pointer: a drag of the hand is one stroke, a drag of the card pushes its edge from the press to the release, the paper beyond where it ends up having had the light the whole time, the paper behind where it started none of it, and a ramp between; a click is the tip, or the card, held still, and a stroke begins and ends with its stops, the hand being set down at its start and lifted at its end. The stroke prints as it is made and the release is one step of the history; the other mouse buttons still pan, and Alt shows the strokes' paths. At the right of the toolbar's second row stands how many strokes the picture has, and *Clear strokes* takes them all away. With no tool in the hand that row says what the picture carries: its crop, its straighten, its strokes. The physics is the darkroom's. The stops are the time at the middle of a straight pass: $+1$ doubles the exposure there, burning it in darker, $-1$ halves it, holding it back lighter, four stops either way, in hundredths. A point the pass only brushes gets the chord, the hand going back and forth adds, as seconds do, and a hand that stands still over one place gives it what the tip's shadow gives, no more; times add, not stops, so two one-stop passes make three times the base, not four. The time is added to the working grade, or to the soft pass alone, or to the hard pass alone, the split-grade burn, which moves the highlights without the shadows or the shadows without the highlights. A stroke is in the recipe on the upright negative, in coordinates normalised to its long side, so it holds under any crop, straighten, 90° turn or print size; the fitted print, the 1:1 box, the preview and the histogram all read it through the same map as the pixels. Burns in orange, dodges in blue. In the file they are the list `burns`, one entry per stroke, such as `{"tool": "hand", "tip": {"rx": 0.08, "ry": 0.08, "feather": 0.03}, "stops": -0.25, "points": [[-0.3, 0.12], [-0.22, 0.16]]}` or `{"tool": "card", "x": 0, "y": -0.1, "angle": -90, "travel": 0.15, "feather": 0.1, "stops": 1, "light": "soft"}`; `mimizan print --recipe` applies them and names them in its log.

*Print* sets the sheet and writes it as one file; its first line says which. The first box is the container: a TIFF with the recipe in its description, in 16 bits for the archive and one's own printer or in 8 bits for a print service that takes no more, or an 8-bit JPEG at quality 100 with no chroma subsampling, for the screen and for a service with a limit on the file's size; all three carry the ICC profile and the dpi. The second box is the colour of the file. In the monochrome process it is grey, one channel with the paper's gamma 2.2, or RGB, the same sheet as three equal channels in sRGB, for a service that prints no greyscale file. In the plate it is the primaries, sRGB by default or Adobe RGB. An sRGB file is sRGB in full: its values lie on the sRGB curve under the profile *sRGB IEC61966-2.1*, so it reads the same whether a program honours the profile or only assumes it. The Adobe RGB file keeps the paper's gamma 2.2, which is that space's own curve, under a profile made here that says *compatible with Adobe RGB (1998)*; a service that looks for Adobe's own profile by its name may not know it. The container and the grey sheet's channels are the program's, not the recipe's, and are remembered. The size fields are the sheet in the printer, in the crop's proportion: type one side, the other follows the picture, and the file is that sheet, turned the picture's way. With *free* on, the two sides are typed apart, for a paper a print service offers, 30 × 40 cm for a picture of 2:3; the picture is fitted into it and centred, and a new crop leaves the sheet as it is. The black edge and the white border are taken from inside the sheet, each on its own or black inside white, and the picture fills what they leave, centred; with rings, a sheet of the picture's shape leaves paper beyond the border on two sides, as on an easel. A black edge with no white border outside it lies where a print service trims, about 2 mm a side and never quite evenly; the row says *at the trim* then. A new crop is a new picture, and the sheet takes its proportion with its long side held, unless it is free. The *picture* row under the rings says how large the picture comes out and by what scale. A file opens at 1:1, the sheet that is the negative's pixels with nothing around them; *1:1* in the size row is lit while the sheet is that, and returns there. The ruler at the right of the size row shows every length in centimetres and millimetres, in inches, or in pixels of the file; the recipe keeps inches whichever is on. The dpi is written into the file; with the cm or in ruler a new dpi keeps the sheet and moves the pixels, with the px ruler it keeps the pixels and moves the sheet. An older recipe without a sheet is read as the sheet its file had. Resample is B-spline by default, or area (each output pixel is the source area it covers: a mean when shrinking, the pixel held when enlarging), Mitchell, Catmull–Rom, or Lanczos. *sharpen* is the phone's Peak, a high-pass sharpening of that print: the picture plus an amount of itself minus a Gaussian blur. The radius is that blur, in print pixels, measured on the negative and multiplied by the print's scale, including a reduction. The amount does not change. The two numbers are offered while the sharpening is on, and *Reset to measured* puts them back. The box above it is those print pixels at 1:1; drag it, or drag the crosshair on the sheet. The Export button stands at the foot of the window and stays there when the rows above it scroll; it names the file it writes, and the line under it says how much memory the export holds at once. The recipe is saved next to the RAW, with the process, the mix or the filters and the lamps, and comes back when the file is opened. Leaving a changed recipe asks first. A recipe the command line wrote with its unsharp mask or Richardson–Lucy, or through the neutral look, opens without that sharpening and on paper: the window says so, and the recipe counts as changed.

*Negative…* under the Export button is the way out of the darkroom for anyone who wants Mimizan's separation and their own converter on top: the picture without paper, lamps or border, turned and cropped as the recipe says, nothing else applied. In the monochrome process it is the mixed negative as a linear 16-bit grey TIFF with the mask beside it: the same samples and description as the file `mimizan negative` writes, but turned upright and cut to the crop, where the command keeps the sensor frame and records the orientation in the tag. In the plate process it is a linear DNG: the three balanced channels of the separation with the white balance undone, three 16-bit samples per pixel, the camera's colour matrix and as-shot white balance in the tags. A RAW converter opens it as a RAW of that camera and has no demosaicing left to do; white balance, profile and tone are its own. Every sample is still a weighted sum of measured photosites; the colour in it is band-limited to the carriers, under the full-resolution luminance.

## Get it

Prebuilt builds are on the [releases page](https://github.com/Draganito/mimizan/releases).

The separation works in double precision on the whole frame: a 24-megapixel file holds about 1.8 GB while it is developed, 45 megapixels 3.4 GB, 61 megapixels 4.5 GB, and the open picture keeps most of that afterwards. When the next file would not fit beside the open one, the desktop lets the open one go first; a file that does not fit at all is refused with the two figures in the log rather than started.

Debian and Ubuntu: [mimizan_0.18.13-1_amd64.deb](https://github.com/Draganito/mimizan/releases/download/v0.18.13/mimizan_0.18.13-1_amd64.deb). Install the downloaded file:

```bash
sudo apt install ./mimizan_0.18.13-1_amd64.deb
```

That installs the darkroom window, `mimizan-gui`, and the `mimizan` command.

Windows, 64-bit Intel and AMD: [mimizan-0.18.13-x86_64-windows.zip](https://github.com/Draganito/mimizan/releases/download/v0.18.13/mimizan-0.18.13-x86_64-windows.zip). Unzip it and start `mimizan-gui.exe`. Leave `cameras` and `look` next to the two programs. The programs carry the Visual C++ runtime, and the window opens without a terminal. An unsigned download asks SmartScreen for confirmation. The same program is on the [Microsoft Store](https://apps.microsoft.com/detail/9ph4p5qb18z5), signed.

Apple silicon: [mimizan-0.18.13-aarch64-macos.zip](https://github.com/Draganito/mimizan/releases/download/v0.18.13/mimizan-0.18.13-aarch64-macos.zip). Unzip it and open `Mimizan Lab.app`. The command line is `Mimizan Lab.app/Contents/MacOS/mimizan`. The programs use the frameworks that come with macOS. The first launch is a right-click and Open. A signed and sandboxed build of the window, without the command, is uploaded to App Store Connect for TestFlight; it is not on the Mac App Store yet. That build alone has a purchase: the JPEG at full size is free there, and the TIFF and the negative for another program are one purchase, made once and shared with the iPhone. This download, Linux and Windows have no purchase and lock nothing.

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

The window can photograph itself, for a handbook or a store page. `MIMIZAN_SCREENSHOT=shot.png mimizan-gui photo.nef` opens the file, writes the window's picture as a PNG once the print and its 1:1 box are on screen, and quits. `MIMIZAN_WINDOW=1280x800` sets the size in points, and `MIMIZAN_WINDOW_BARE=1` leaves the frame off and holds that size, so the picture has the pixels asked for: 2560 × 1600 on a screen at scale 2.

## The method

The formulas and the constants are in [the paper](docs/PAPER.md). The note it started from is [the whitepaper](docs/whitepaper_v1.pdf).
