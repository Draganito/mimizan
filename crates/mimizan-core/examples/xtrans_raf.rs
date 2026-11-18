//! Experiment (branch xtrans-experiment): an X-Trans RAF through the
//! separation, as pictures to look at.
//!
//! xtrans_raf <file.raf> <outdir> [--ppm demosaiced.ppm] [--crop x,y,size]... [--zoom n] [--single]
//!   [--prior alpha,alpha_c,chroma1,chroma2,noise,lens]  instead of the search
//!   [--free 001111]  which of those six the search may move (the others stay 3, 3, 0.05, 0.05, 1e-4, 0.3)
//!   [--flat x,y,size]  mean of each of the 36 tile positions in a flat window, and stop
//!   [--region x,y,w,h]  that part of the frame only (crops are then counted from its corner)
//!   [--gain v]  the scale of the pictures, instead of the 99.9th percentile's
//!   [--orient n,k[,f..]]  n oriented priors (cos^2k) beside the others, at f times the colour's share (1 and 16)
//!   [--two]  the second step (colour on one line)
//!   [--patch x,y,size]  mean of each channel in that window, photosites against separation
//!   [--curve root|srgb]  separate the mosaic under that tone curve and come back
//!   [--clip]  the balanced mosaic cut at the level the first channel burns out at, so that burnt areas are white
//!
//! Writes the luminance and the colour picture (camera matrix towards
//! sRGB) as 8-bit PNGs, both scaled so that the 99.9th percentile of the
//! luminance is white, and for every --crop the same window of each at
//! 100 %. With --ppm (libraw's `dcraw_emu -4 -o 0`, the camera's channels
//! balanced, linear) the demosaiced picture is put beside each crop.

use mimizan_core::cfa::Color;
use mimizan_core::kodak::{self, srgb_encode};
use mimizan_core::plane::Plane;
use mimizan_core::plate::{PlateSpace, PlateWeights};
use mimizan_core::synth::Rgb;
use mimizan_core::xtrans::{self, Prior, Selection, XParams, XTile};
use std::path::{Path, PathBuf};

fn save_rgb(path: &Path, rgb: [&Plane; 3], x0: usize, y0: usize, w: usize, h: usize, gain: f64) {
    let mut buf = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                buf[(y * w + x) * 3 + c] =
                    (srgb_encode(gain * rgb[c].at(x0 + x, y0 + y)) * 255.0).round() as u8;
            }
        }
    }
    image::save_buffer(path, &buf, w as u32, h as u32, image::ColorType::Rgb8).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (file, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[1]));
    std::fs::create_dir_all(&out).unwrap();
    let mut crops: Vec<(usize, usize, usize)> = Vec::new();
    let mut ppm: Option<PathBuf> = None;
    let mut single = false;
    let mut flat: Option<(usize, usize, usize)> = None;
    let mut zoom = 1usize;
    let mut given: Option<Prior> = None;
    let mut two = false;
    let mut clip = false;
    let mut curve = String::from("linear");
    let mut patches: Vec<(usize, usize, usize)> = Vec::new();
    let mut free = [true; 6];
    let mut orient: Option<(usize, f64, Vec<f64>)> = None;
    let mut region: Option<(usize, usize, usize, usize)> = None;
    let mut gain_given: Option<f64> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--crop" => {
                let v: Vec<usize> = args[i + 1].split(',').map(|x| x.parse().unwrap()).collect();
                crops.push((v[0], v[1], v[2]));
                i += 1;
            }
            "--ppm" => {
                ppm = Some(PathBuf::from(&args[i + 1]));
                i += 1;
            }
            "--single" => single = true,
            "--two" => two = true,
            "--clip" => clip = true,
            "--curve" => {
                curve = args[i + 1].clone();
                i += 1;
            }
            "--patch" => {
                let v: Vec<usize> = args[i + 1].split(',').map(|x| x.parse().unwrap()).collect();
                patches.push((v[0], v[1], v[2]));
                i += 1;
            }
            "--free" => {
                for (f, c) in free.iter_mut().zip(args[i + 1].chars()) {
                    *f = c == '1';
                }
                i += 1;
            }
            "--orient" => {
                let v: Vec<f64> = args[i + 1].split(',').map(|x| x.parse().unwrap()).collect();
                orient =
                    Some((v[0] as usize, v[1], if v.len() > 2 { v[2..].to_vec() } else { vec![1.0, 16.0] }));
                i += 1;
            }
            "--region" => {
                let v: Vec<usize> = args[i + 1].split(',').map(|x| x.parse().unwrap()).collect();
                region = Some((v[0], v[1], v[2], v[3]));
                i += 1;
            }
            "--gain" => {
                gain_given = Some(args[i + 1].parse().unwrap());
                i += 1;
            }
            "--flat" => {
                let v: Vec<usize> = args[i + 1].split(',').map(|x| x.parse().unwrap()).collect();
                flat = Some((v[0], v[1], v[2]));
                i += 1;
            }
            "--zoom" => {
                zoom = args[i + 1].parse().unwrap();
                i += 1;
            }
            "--prior" => {
                let v: Vec<f64> = args[i + 1].split(',').map(|x| x.parse().unwrap()).collect();
                given = Some(Prior {
                    alpha: v[0],
                    alpha_c: v[1],
                    chroma: [v[2], v[3]],
                    noise: v[4],
                    lens: v[5],
                    ..Prior::default()
                });
                i += 1;
            }
            o => panic!("unknown {o}"),
        }
        i += 1;
    }

    let t0 = std::time::Instant::now();
    let source = rawler::rawsource::RawSource::new(&file).unwrap();
    let decoder = rawler::get_decoder(&source).unwrap();
    let params = rawler::decoders::RawDecodeParams::default();
    let img = decoder.raw_image(&source, &params, false).unwrap();
    let rawler::rawimage::RawPhotometricInterpretation::Cfa(cfg) = &img.photometric else {
        panic!("no mosaic")
    };
    assert!(cfg.cfa.width == 6 && cfg.cfa.height == 6, "not a 6x6 tile");
    let sensor = XTile::from_name(&cfg.cfa.name).expect("tile of R, G, B");
    let rawler::RawImageData::Integer(data) = &img.data else { panic!("integer samples expected") };
    let black = f64::from(img.blacklevel.levels[0].as_f32());
    let white = f64::from(img.whitelevel.0[0]);
    let wb = [f64::from(img.wb_coeffs[0]), f64::from(img.wb_coeffs[1]), f64::from(img.wb_coeffs[2])];
    let wb = [wb[0] / wb[1], 1.0, wb[2] / wb[1]];
    let crop = img.crop_area.expect("crop");
    // The frame: inside the file's crop, first and last column and row on
    // symmetry lines of the tile, so that mirroring the edges continues it.
    let mut frame = None;
    'find: for cy in crop.p.y..crop.p.y + 6 {
        for cx in crop.p.x..crop.p.x + 6 {
            let (w, h) =
                ((crop.p.x + crop.d.w - cx - 1) / 3 * 3 + 1, (crop.p.y + crop.d.h - cy - 1) / 3 * 3 + 1);
            let t = sensor.shifted(cx as isize, cy as isize);
            if t.mirrors(w, h) {
                frame = Some((cx, cy, w, h, t));
                break 'find;
            }
        }
    }
    let (mut cx, mut cy, mut w, mut h, tile) = frame.expect("a frame the tile mirrors on");
    // A part of the frame only: from a whole tile on, so that the tile and
    // its symmetry lines stay where they are.
    let mut origin = (0usize, 0usize);
    if let Some((rx, ry, rw, rh)) = region {
        origin = (rx / 6 * 6, ry / 6 * 6);
        cx += origin.0;
        cy += origin.1;
        w = ((rw.min(w - origin.0)) - 1) / 3 * 3 + 1;
        h = ((rh.min(h - origin.1)) - 1) / 3 * 3 + 1;
        assert!(tile.mirrors(w, h));
    }
    eprintln!(
        "{} {}: {}x{}, frame {w}x{h} at ({cx},{cy}), black {black} white {white}, balance {wb:.3?}, decoded in {:.1} s",
        img.make,
        img.model,
        img.width,
        img.height,
        t0.elapsed().as_secs_f64()
    );
    let mut m = Plane::zeros(w, h);
    // A photosite at white measures nothing but "at least this much".
    // Cut at the lowest of the three balanced ceilings, a burnt area is
    // flat and white instead of the balance's colour.
    let ceiling = if clip { wb.iter().copied().fold(f64::INFINITY, f64::min) } else { f64::INFINITY };
    let mut sums = [(0.0, 0.0); 3];
    let mut clipped = 0usize;
    for y in 0..h {
        for x in 0..w {
            let raw = f64::from(data[(cy + y) * img.width + cx + x]);
            let c = tile.color_at(x as isize, y as isize);
            let v = (raw - black) / (white - black);
            clipped += (raw >= white - 1.0) as usize;
            sums[c as usize].0 += v;
            sums[c as usize].1 += 1.0;
            m.set(x, y, (v * wb[c as usize]).min(ceiling));
        }
    }
    let mean = |c: Color| sums[c as usize].0 / sums[c as usize].1;
    eprintln!(
        "raw means R {:.4} G {:.4} B {:.4} (G/R {:.2}, G/B {:.2}); {clipped} photosites at white",
        mean(Color::R),
        mean(Color::G),
        mean(Color::B),
        mean(Color::G) / mean(Color::R),
        mean(Color::G) / mean(Color::B)
    );

    if let Some((fx, fy, size)) = flat {
        // Positions relative to the frame's tile; the window starts on a tile.
        let (fx, fy) = (fx / 6 * 6, fy / 6 * 6);
        let mut acc = [[(0.0, 0.0); 6]; 6];
        for y in 0..size / 6 * 6 {
            for x in 0..size / 6 * 6 {
                let a = &mut acc[y % 6][x % 6];
                a.0 += m.at(fx + x, fy + y);
                a.1 += 1.0;
            }
        }
        let mut by_colour = [(0.0, 0.0); 3];
        for (y, line) in acc.iter().enumerate() {
            for (x, a) in line.iter().enumerate() {
                let c = tile.color_at(x as isize, y as isize) as usize;
                by_colour[c].0 += a.0 / a.1;
                by_colour[c].1 += 1.0;
            }
        }
        println!(
            "balanced means R {:.5} G {:.5} B {:.5}",
            by_colour[0].0 / by_colour[0].1,
            by_colour[1].0 / by_colour[1].1,
            by_colour[2].0 / by_colour[2].1
        );
        println!("each position against the mean of its colour, per cent:");
        for (y, line) in acc.iter().enumerate() {
            let row: Vec<String> = line
                .iter()
                .enumerate()
                .map(|(x, a)| {
                    let c = tile.color_at(x as isize, y as isize);
                    let mean = by_colour[c as usize].0 / by_colour[c as usize].1;
                    format!("{}{:+6.2}", ["R", "G", "B"][c as usize], 100.0 * (a.0 / a.1 / mean - 1.0))
                })
                .collect();
            println!("  {}", row.join("  "));
        }
        return;
    }

    // The mosaic of a picture under a tone curve is the mosaic under
    // that curve: separate there, and come back channel by channel.
    let light = m.clone();
    let tone = xtrans::Curve::from_name(&curve).unwrap_or_else(|| panic!("curve {curve}"));
    if tone != xtrans::Curve::Linear {
        m = tone.mosaic(&m);
    }

    let p = XParams::default();
    let t1 = std::time::Instant::now();
    let prior = given.unwrap_or_else(|| {
        xtrans::fit_prior(&m, &tile, &p, Prior { lens: 0.3, ..Prior::default() }, 8, free)
    });
    eprintln!(
        "prior from the file: alpha {:.2} alpha_c {:.2} chroma {:.4} {:.4} noise {:.1e} lens {:.2}  ({:.1} s)",
        prior.alpha,
        prior.alpha_c,
        prior.chroma[0],
        prior.chroma[1],
        prior.noise,
        prior.lens,
        t1.elapsed().as_secs_f64()
    );
    let mut priors = vec![prior];
    if !single {
        for f in [0.0625, 0.25, 4.0, 16.0] {
            priors.push(Prior { chroma: [prior.chroma[0] * f, prior.chroma[1] * f], ..prior });
        }
    }
    if let Some((count, k, factors)) = orient {
        for f in factors {
            for j in 0..count {
                let theta = std::f64::consts::PI * j as f64 / count as f64;
                priors.push(Prior {
                    chroma: [prior.chroma[0] * f, prior.chroma[1] * f],
                    oriented: Some((theta, k)),
                    ..prior
                });
            }
        }
    }
    let t2 = std::time::Instant::now();
    let mut sep = xtrans::separate_bank(&m, &tile, &p, &priors, &Selection::default());
    eprintln!("separated under {} priors in {:.1} s", priors.len(), t2.elapsed().as_secs_f64());
    if two {
        let t3 = std::time::Instant::now();
        sep = xtrans::refine(&m, &tile, &sep, &xtrans::Refine::default());
        eprintln!("second step in {:.1} s", t3.elapsed().as_secs_f64());
    }
    if tone != xtrans::Curve::Linear {
        sep = sep.mapped(|v| tone.back(v));
    }

    // Mean of each channel in a window: the photosites of each colour
    // against the separation's three channels.
    for &(px, py, size) in &patches {
        let out = sep.rgb();
        let mut site = [(0.0, 0.0); 3];
        let mut mean = [0.0; 3];
        for y in py..py + size {
            for x in px..px + size {
                let c = tile.color_at(x as isize, y as isize) as usize;
                site[c].0 += light.at(x, y);
                site[c].1 += 1.0;
                for (k, pl) in [&out.r, &out.g, &out.b].iter().enumerate() {
                    mean[k] += pl.at(x, y) / (size * size) as f64;
                }
            }
        }
        eprintln!(
            "patch {px},{py}: photosites R {:.5} G {:.5} B {:.5} | separation R {:.5} G {:.5} B {:.5}",
            site[0].0 / site[0].1,
            site[1].0 / site[1].1,
            site[2].0 / site[2].1,
            mean[0],
            mean[1],
            mean[2]
        );
    }
    let mut sorted: Vec<f64> = sep.lum.data.iter().step_by(97).copied().collect();
    sorted.sort_by(f64::total_cmp);
    let gain = gain_given.unwrap_or(1.0 / sorted[(sorted.len() as f64 * 0.999) as usize]);
    eprintln!("gain {gain:.4}");
    let to_srgb = img
        .color_matrix
        .get(&rawler::imgop::xyz::Illuminant::D65)
        .map(|c| {
            let r = |i: usize| [f64::from(c[3 * i]), f64::from(c[3 * i + 1]), f64::from(c[3 * i + 2])];
            PlateWeights::camera(&[r(0), r(1), r(2)], PlateSpace::Srgb).unwrap()
        })
        .unwrap_or_else(PlateWeights::channels);
    let colour = |s: &dyn Fn([f64; 3]) -> Plane| [s(to_srgb.rows[0]), s(to_srgb.rows[1]), s(to_srgb.rows[2])];
    let [r, g, b] = colour(&|wt| sep.mix(wt));
    let scaled = Plane::from_vec(w, h, sep.lum.data.iter().map(|v| v * gain).collect());
    kodak::write_gray_png(&out.join("luminance.png"), &scaled).unwrap();
    save_rgb(&out.join("colour.png"), [&r, &g, &b], 0, 0, w, h, gain);
    // A sixth-size view (box mean) to find the crops on.
    let small = |p: &Plane| {
        let (sw, sh) = (w / 6, h / 6);
        let mut o = Plane::zeros(sw, sh);
        for y in 0..sh {
            for x in 0..sw {
                let mut a = 0.0;
                for dy in 0..6 {
                    for dx in 0..6 {
                        a += p.at(6 * x + dx, 6 * y + dy);
                    }
                }
                o.set(x, y, a / 36.0);
            }
        }
        o
    };
    let (sr, sg, sb) = (small(&r), small(&g), small(&b));
    save_rgb(&out.join("colour_sixth.png"), [&sr, &sg, &sb], 0, 0, w / 6, h / 6, gain);

    // The demosaiced picture, brought onto the frame: offset by search, level by fit.
    let reference: Option<(Plane, [Plane; 3])> = ppm.map(|path| {
        let d = kodak::read_ppm16(&path).unwrap();
        let lum_of = |rgb: &Rgb, x: usize, y: usize| {
            sep.g * rgb.g.at(x, y) + (1.0 - sep.g) * (rgb.r.at(x, y) + rgb.b.at(x, y)) / 2.0
        };
        let size = 256usize.min(w).min(h);
        let (px, py) = ((w - size) / 2, (h - size) / 2);
        let mut best = (f64::INFINITY, 0isize, 0isize, 1.0);
        for dy in -24isize..=24 {
            for dx in -24isize..=24 {
                let (mut ab, mut bb) = (0.0, 0.0);
                let mut pairs = Vec::with_capacity(size * size);
                for y in 0..size {
                    for x in 0..size {
                        let (sx, sy) = ((origin.0 + px + x) as isize + dx, (origin.1 + py + y) as isize + dy);
                        if sx < 0 || sy < 0 || sx as usize >= d.width() || sy as usize >= d.height() {
                            continue;
                        }
                        let (a, b) = (sep.lum.at(px + x, py + y), lum_of(&d, sx as usize, sy as usize));
                        ab += a * b;
                        bb += b * b;
                        pairs.push((a, b));
                    }
                }
                let k = ab / bb;
                let e: f64 = pairs.iter().map(|(a, b)| (a - k * b).powi(2)).sum();
                if e < best.0 {
                    best = (e, dx, dy, k);
                }
            }
        }
        let (_, dx, dy, k) = best;
        eprintln!("demosaiced {}x{}: offset ({dx},{dy}), level {k:.4}", d.width(), d.height());
        let at = |p: &Plane, x: usize, y: usize| {
            let (sx, sy) = (
                ((origin.0 + x) as isize + dx).clamp(0, p.width as isize - 1) as usize,
                ((origin.1 + y) as isize + dy).clamp(0, p.height as isize - 1) as usize,
            );
            k * p.at(sx, sy)
        };
        let mut moved = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                moved.set(x, y, at(&d.r, x, y), at(&d.g, x, y), at(&d.b, x, y));
            }
        }
        let n = w * h;
        let lum = Plane::from_vec(
            w,
            h,
            (0..n)
                .map(|i| sep.g * moved.g.data[i] + (1.0 - sep.g) * (moved.r.data[i] + moved.b.data[i]) / 2.0)
                .collect(),
        );
        let mix = |wt: [f64; 3]| {
            Plane::from_vec(
                w,
                h,
                (0..n)
                    .map(|i| wt[0] * moved.r.data[i] + wt[1] * moved.g.data[i] + wt[2] * moved.b.data[i])
                    .collect(),
            )
        };
        (lum, colour(&mix))
    });

    // Per crop one picture: the separation on the left, the demosaiced
    // picture (if given) on the right, each pixel `zoom` wide.
    for (x0, y0, size) in crops {
        let panels = if reference.is_some() { 2 } else { 1 };
        let (pw, gap) = (size * zoom, 8usize);
        let tw = panels * pw + (panels - 1) * gap;
        let mut grey = vec![255u8; tw * pw];
        let mut col = vec![255u8; tw * pw * 3];
        let enc = |v: f64| (srgb_encode(gain * v) * 255.0).round() as u8;
        for k in 0..panels {
            let (lum, rgb): (&Plane, [&Plane; 3]) = match (&reference, k) {
                (Some((dl, dc)), 1) => (dl, [&dc[0], &dc[1], &dc[2]]),
                _ => (&sep.lum, [&r, &g, &b]),
            };
            for y in 0..pw {
                for x in 0..pw {
                    let (sx, sy) = (x0 + x / zoom, y0 + y / zoom);
                    let o = y * tw + k * (pw + gap) + x;
                    grey[o] = enc(lum.at(sx, sy));
                    for c in 0..3 {
                        col[3 * o + c] = enc(rgb[c].at(sx, sy));
                    }
                }
            }
        }
        let name = format!("crop_{x0}_{y0}");
        image::save_buffer(
            out.join(format!("{name}_luminance.png")),
            &grey,
            tw as u32,
            pw as u32,
            image::ColorType::L8,
        )
        .unwrap();
        image::save_buffer(
            out.join(format!("{name}_colour.png")),
            &col,
            tw as u32,
            pw as u32,
            image::ColorType::Rgb8,
        )
        .unwrap();
    }
    eprintln!("written to {} in {:.1} s over all", out.display(), t0.elapsed().as_secs_f64());
}
