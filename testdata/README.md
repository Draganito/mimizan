# testdata

Not in git (size). Expected files for the real-sample checks:

- `NikonZF.nef`, `NikonZF_2.nef` — Nikon Z f, lossless 14-bit NEF, RGGB, 6064x4040
  (studio scene, daylight and low-light setup).
- `nikon_zf_01.nef`, `nikon_zf_27.nef` — Nikon Z f field shots (tree, 35 mm f/5.6;
  architecture, 28 mm f/11); the other two files of `docs/KODAK_ZF.md` and
  `docs/KODAK_ZF_SOFT.md`. Any Z f NEF does for `bench kodak --raw`.
- `MonoRef.dng`, `MonoRef_2.dng` — the monochrome reference camera (full-frame
  sensor without colour filter array), 12-bit LinearRaw DNG, 5984x4000,
  WhiteLevel 3750; same two setups.
- `MonoRef_sooc.jpg` — the reference camera's own out-of-camera JPEG of
  `MonoRef.dng` (look fit).

- `kodak/kodim01.png` … `kodim24.png` — Kodak PhotoCD test pictures (768×512, 8-bit sRGB,
  free for unrestricted use; <https://r0k.us/graphics/kodak/>) for `mimizan bench kodak`.

Synthetic scenes are generated with `mimizan synth` and need no files here.
