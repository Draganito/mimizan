# Mimizan Lab — Handbook

For someone who already knows a darkroom, and wants to know why a control
is there, and why another one was left out. The session in detail, the
commands and the measurements live in the
[detailed manual](../DETAILED.md).

Four chapters. The first is the reason. The three that follow are the
three rooms of the window: **Negative**, **Enlarger**, **Print**.

---

## 1. What Mimizan is, and why I built it

Mimizan Lab takes a colour RAW and makes a monochrome negative from it,
then prints that negative the way a split-grade enlarger does: one soft
light, and one hard light.

I loved Iridient Developer. I used Photoshop, and Capture One, which I
liked better than Lightroom. My enemy was a ghost in the room.
Demosaicing estimates. That estimate stole the soul of the picture.

A digital photograph was always a fake to me, something like fast food.
Too many possibilities, from the cameras all the way through the
software. I was never satisfied, and I spent more time working the
pictures than taking them. The possibilities shifted the focus. I would
rather photograph on film and make analog prints. Analog is simple,
concentrated, a real craft. Digital I distrusted, for the
artificial artifacts, and for programs that can do anything to a
photograph. Far too much.

The trouble is time. I do not have it. With a family, the analog craft
is not always possible. On a walk the camera goes on the tripod, the
focus is found on the ground glass with a loupe, the shutter goes, and
the family is already a kilometre ahead. At home there is still the
developing, and then the print. All of it takes time. Once, at the very
top of the lighthouse in Biarritz, I set the analog camera up to make a
photograph of myself and the family. Far too crazy.

So the attempt to work digitally, and to carry over as much of the
analog way as a file will hold. First, chase off the ghost. Turn the
sensor into black and white in the frequency domain, inventing as few
pixels as possible, and leave the digital artifacts behind. When I
spoke about this with an AI, it showed me Alleysson and Dubois. That is
what their work is about. I was on the beach at Mimizan, watching the
surfers, and there I wrote the whitepaper. Hence the name. Lab is the
darkroom.

Black and white turns the eye toward form, geometry, light and shadow.
Its expression is stronger than colour. Colour distracts me from the
subject. If a photograph can be made well on the rebuilt split-grade
paper, then it is a good photograph. If it cannot, it is not one. I do
not force it to be good with too much technique. Nothing in it should
be something an analog darkroom could not also do.

A Bayer sensor is not built for fine-art black and white. A Leica
Monochrom is. I cannot afford one, and I would not want one even if I
had the money. A camera that expensive would change the way I
photograph. I would watch it, so that it did not get scratched. That is
another distraction from the craft. A camera has to be inexpensive.
Hence Bayer. Mimizan already draws a great deal out of it.

The difference from a demosaic is hard to see on the screen. It is
there anyway. It bothers me while I am still working the picture, and I
will not settle. Fine art is printed large. Someone who steps closer,
to look more carefully, has to find a picture that is still authentic.
On a large print the chance is small that anyone thinks it is digital.
That is pleasant, and it is real.

What remains, then, is a small number of decisions: the crop, in the
format of the print; the white border; and how the light and the shadow
are shared.

## 2. Negative

Here the negative is judged, not designed. The camera has already made
the exposure.

**As shot.** I usually leave the white balance as the camera wrote it.
It is usually good. Gray world, none, and manual are there when an
exposure has gone astray. They are not where the work happens.

**raw.** First the **raw** histogram. It tells me whether the picture is
technically sound. Then I can go on. After that the **print** histogram,
and then the picture as a whole. Whether the highlights burn out, or are
held back a little further, is always a decision for that one picture.
The forms and the structures have to be given their weight. That is hard
to describe, and there is no slider for it.

**negative.** The third histogram is the negative on the lightbox,
before the two lights. The decision is not made there. When the exposure
is sound and the print sits, this histogram says nothing new.

**The separation.** The default is the Dubois separation. It is not
perfect, and perhaps it never can be. I am still working on the method.
Physical residuals remain. The last octave takes them away: *reconstruct
the last octave*. Many photographs do not need it at all. On mine it is
usually 4–8 % invented pixels. That is much less than a demosaicer, and
above all there are far fewer artifacts that give the picture away as
digital. Type and fine detail look more detailed, less falsified, more
like the analog thing. So does the noise. That is why the control is on
when a file opens. Turn it off and press *Apply*, and you are on the
path that estimates nothing.

**Repair defects** stays off. It is not ready. It brings in more
artifacts than it takes out. Work on it continues.

The comparison with AMaZE, RCD, DCB and LMMSE is not in this window.
It stays in the older line, release 0.10.12 and the branch `demosaic`,
for anyone who wants to see the difference beside the separation.
On the screen that difference is easy to miss. On the large print,
when someone steps closer, it is there.

## 3. Enlarger

The crop comes first. The light comes after. Someone who sets the tones
and then crops is judging a different picture from the one that will
meet the paper.

**Panchromatic** is the film. It is a measured approach to a Leica
Monochrom, made together against a real monochrome camera. Good, and not
perfect, because the light changes. **Native** is the sensor without
that emulation: the plain luminance of the mosaic.

A file from a sensor that has no colour filter is a further case. There
is no mosaic and nothing to mix. The enlarger says so. What you see is
the measurement, and the two lights stay.

**The filters.** Yellow, orange, red, green and blue have been in
black-and-white photography for a century. A filter is a spectral
weighting before the sum, which is exactly what the mix is. Yellow,
orange and red hold the blue back. Green lightens foliage. Blue darkens
red. With a camera file the named filters sit in front of
**Panchromatic**. The program also has yellow-green. The R and B fields
are the same gesture by hand, when none of the names is the one you
want.

**The two lights.** First the brightness, so that nothing clips: the
*soft* slider, filter 00. Printers call that soft exposure grade 0.
Then the contrast, with *hard*, filter 5. Then the soft slider once
more. More soft light greys the highlights and leaves the shadows.
More hard light deepens the shadows, and the highlights stay. The line
under the sliders is a different number from either slider. It names
the single grade the two lights equal together. At the start, both at
zero, that line says about grade 2, not 0. Filter 00 alone would be
grade 00. Beside it, how many stops of the scene the paper still holds.

**The bar.** The zone bar can be dragged. Both lights move together,
like one more second under the enlarger, or one less. It is good for
playing several versions through quickly. The fine work stays with the
two sliders. *Base* puts back the measured pair, about grade 2, the grey
card at 18 %.

If the picture holds on this paper, it is a good picture. There is no
curve to pull at a single point, and no local exposure. That would be
forcing the photograph to be good by technique.

## 4. Print

**The crop** is the format of the print: 4×5, 5×7, 8×10, 11×14, 16×20,
20×24, or square. The long side follows the picture, until *Turn* swaps
the ratio between landscape and portrait. A landscape negative can then
stand portrait on the paper. *Apply* makes the window the picture, in
whole pixels, with no new sampling. After that the histogram, the
centimetres and the export see only that window.

**The white border** comes from the easel. It is a side effect, and hard
to avoid once you use one. It gives the picture a certain something. It
holds the eye on it. In the program it is paper white around the
centimetres, and it is already in the preview, so you judge it as it
will stand on the sheet.

**B-spline** is the default. It does what a darkroom enlargement does.
It blows the picture up, plainly, without inventing anything that looks
artificial. Lanczos-3 is the other kernel. It rings. That is the
artifact this enlargement is not to have.

**No sharpening.** I decided against it on purpose. Unsharp masking
comes from the darkroom, and it was little used. An analog print simply
grows softer when it is enlarged too far. That is all right. The
darkroom does not write the step into the print. The command line can
still set it. The window cannot.

**The recipe** is the note in my notebook. It sits beside the negative,
carries the file's name, ends in `.recipe.json`, and comes back when the
file is opened again. Crop, the two lights, the size, the border. Not
the mix weights, and not the quarter-turns of the view. Those belong to
the session, not to the notebook. Changing the file, or closing the
window, asks first. *Save* writes the note. *Don't save* leaves it as it
lies. *Reset* puts the controls back and does not overwrite the
notebook. After that a new note can be made.

The print is one file, a 16-bit TIFF. The same two lights, the same
crop, the same white border as the preview.

## 5. What the craft already said

These are not my sentences. They are the places where a printer, a
photographer, or the paper itself had already found the same words.
Each one is narrower than the chapter it stands beside.

**Black and white, and colour.** Ted Grant, the Canadian
photojournalist, confirmed the wording himself. When you photograph
people in colour, you photograph their clothes. When you photograph them
in black and white, you photograph their souls. He said it of people.
The turn is the same one I make about a subject: colour distracts, and
black and white stays with the form.
([Leica Users Group, August 2008](http://leica-users.org/leica-users/v37/msg08775.html);
[The Globe and Mail](https://www.theglobeandmail.com/news/national/turning-the-camera-on-a-man-whose-photographs-live-in-our-collective-memory/article668439/))

**Do not force it.** Ansel Adams, in *A Personal Credo* (American Annual
of Photography, 1944), had no patience with unnecessary complications of
technique. He used the legitimate controls of exposure, development and
printing only to augment the photographic effect, and he thought a rigid
refusal of every control was ridiculous. Simplicity, he wrote, was a
prime requisite. Alfred Stieglitz and Edward Weston worked with less
equipment than many an amateur could barely get along with. In *The
Print* he set the limit in one line: emphasis on technique is justified
only so far as it simplifies and clarifies the statement. The negative,
as he taught it, is the score. The print is the performance.
([PhotoQuotes, the 1944 credo](https://photoquotes.com/article/from----a-personal-credo----american-annual-of-photography-1944))

**Soft light first, then the hard.** Les McLean begins a split-grade
print with a grade-0 test, choosing the time that just places a tone in
the brightest highlight, and only then makes the grade-5 strip. The hard
exposure, he notes, darkens the low values and leaves the highlights
relatively untouched. The Ilford head printers Mike Walden and Terry
Offord taught the same split: grade 0 for the light greys, grade 5 for
the blacks. Ilford adds that the chemical order of the two exposures
does not change the print. My order is a working order, not a law of the
paper. In the window, grade 0 in that printer's sense is the *soft*
slider, filter 00. The line under the sliders is the single grade the
pair equals, and at the base exposure it reads about 2.
([Les McLean](https://www.lesmcleanphotography.com/articles.php?article=21&page=full);
[Ilford, via Darkroom Dave](https://www.darkroomdave.com/tutorial/split-grade-printing/))

**The ghost is the interpolation.** Roland Wolff, then at Leica, said
you have to see the work as a print, and that taking the colour out of a
Bayer file is not what a monochrome sensor does. The software is piecing
the colour tiles together. On the Monochrom you are not interpolating.
A working report on an early Monochrom says the same thing of the file:
with no demosaic, each pixel is the measurement, and the pictures have
an un-processed look. Marc Erwin Babej, who prints from that camera, was
told by friends who still print film that the tonal gradation was on a
par with film. He names the practical gain for art photography: a file
that will make a museum-size print, up to 80 inches on the long side. I
do not own that camera, and I do not want its price standing between me
and the picture. Panchromatic is the measured approach to what it sees.
The difference those writers are talking about is the one I only trust
on a large print, and when someone steps closer.
([Complex, 2012](https://www.complex.com/pop-culture/a/complex/interview-leica-roland-wolf-justin-staley-m-monochrom-camera);
[Henri, a Monochrom report](http://www.slack.co.uk/slack/Monochrom.html);
[Leica Blog, Babej, 2014](https://leica-camera.blog/2014/05/12/marc-erwin-babej-creating-images-with-layers-of-meaning/))

**Sharpening was a mask, not a habit.** Unsharp masking was described
for the darkroom by G. Spiegler and K. Juris in 1931, and later by
J. A. C. Yule at Kodak: a blurred positive, printed in register with the
negative, so that what remains is the fine detail. Its ordinary home was
graphic arts and radiography, not the fine-art enlarger. The window
leaves the step out. The command line can still set it, the way a
darkroom could make the mask when a job actually wanted it.

**The white edge.** An enlarging easel holds the sheet with blades.
Those blades cover a strip of the paper. That strip never receives the
light, and it stays white. The border I keep is that strip. It holds my
eye on the picture.
