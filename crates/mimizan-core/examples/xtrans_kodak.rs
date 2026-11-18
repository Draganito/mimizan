//! Experiment (branch xtrans-experiment): the X-Trans separation against
//! the Bayer one on the same pictures, sampled through each filter.
//!
//! xtrans_kodak <dir> [--blur s] [--scale n] [--only a,b] [--border n] [--bayer] [--rows]
//!   [--two | --refine pre,window,hold,floor,power[,rounds[,check[,check window]]]]  the second step (colour on one line)
//!   [--synth]  instead of the pictures in <dir>: saturated shapes with hard edges
//!   [--dump dir] [--crop x,y,size]...  the truth, the X-Trans and the Bayer result as PNG,
//!     and each crop of the three side by side, four times enlarged
//!   [--raw a.nef,b.nef]  instead of the pictures in <dir>: Bayer files, every 2x2 cell
//!     binned to one RGB pixel (`kodak::binned_raw`); [--seq] one file at a time
//!   one band per copy: [--c1 along,across] [--axis along,across] [--c2 along,across]
//!     [--t width] [--weight fixed|quad|energy]
//!   both: [--sigma px] [--floor v] [--power p]
//!   least squares: --lmmse alpha,alpha_c,chroma1,chroma2,noise,lens [--f0 v]
//!   [--fit stride]  the prior from the mosaic itself, searched from --lmmse;
//!     [--free 001111]  which of alpha, alpha_c, chroma1, chroma2, noise, lens the search may move
//!   [--cfa bayer]  the same estimator on a Bayer mosaic (least squares only)
//!   [--orient n,k[,f..]]  besides the isotropic prior, n oriented ones (cos^2k) at f times
//!     the colour's share, and
//!     [--oracle]    the best of them per pixel as the truth tells it (a bound)
//!   [--bank "a,ac,c1,c2,n,l;..."]  further priors for the oracle to choose from
//!   [--vary "chroma:0.25,4;lens:-0.2,0.2"]  further priors as changes of the (fitted) one
//!   [--select sigma,sharp,bias]  choose by the local evidence instead of the oracle

use mimizan_core::cfa::BayerPhase;
use mimizan_core::kodak;
use mimizan_core::plane::Plane;
use mimizan_core::plate::{mix_plate, PlateWeights};
use mimizan_core::separate::{separate, SeparateParams};
use mimizan_core::synth::Rgb;
use mimizan_core::xtrans::{self, Band, Prior, Weighting, XParams, XTile};
use rayon::prelude::*;
use std::path::PathBuf;

fn band(s: &str) -> Band {
    let v: Vec<f64> = s.split(',').map(|x| x.parse().expect("number")).collect();
    Band { along: v[0], across: v[1] }
}

fn quantise(p: &mut Plane) {
    p.data.iter_mut().for_each(|v| *v = (v.clamp(0.0, 1.0) * 65535.0).round() / 65535.0);
}

/// PSNR of sRGB-encoded values on 0..255, as `kodak::quality` has it.
fn psnr(a: &Plane, b: &Plane, border: usize) -> f64 {
    let (w, h) = (a.width, a.height);
    let (mut s, mut n) = (0.0, 0.0);
    for y in border..h - border {
        for x in border..w - border {
            let d = 255.0 * (kodak::srgb_encode(a.at(x, y)) - kodak::srgb_encode(b.at(x, y)));
            s += d * d;
            n += 1.0;
        }
    }
    10.0 * (255.0 * 255.0 / (s / n)).log10()
}

/// CPSNR and the three channels' PSNR.
fn cpsnr(a: &Rgb, b: &Rgb, border: usize) -> [f64; 4] {
    let ch = [psnr(&a.r, &b.r, border), psnr(&a.g, &b.g, border), psnr(&a.b, &b.b, border)];
    let mse = ch.iter().map(|p| 255.0 * 255.0 / 10f64.powf(p / 10.0)).sum::<f64>() / 3.0;
    [10.0 * (255.0 * 255.0 / mse).log10(), ch[0], ch[1], ch[2]]
}

/// Saturated shapes with hard edges on six pairs of colours: a star (every
/// direction), a ring, thin lines at four angles and a field of dots. Each
/// pixel is the mean of 8×8 points of the drawing, in linear light.
fn synth_edges() -> Rgb {
    const P: usize = 256;
    let lin = |v: u8| kodak::srgb_decode(v as f64 / 255.0);
    let col = |c: [u8; 3]| [lin(c[0]), lin(c[1]), lin(c[2])];
    let (black, white) = ([20, 20, 20], [235, 235, 235]);
    let (red, blue, yellow) = ([215, 30, 35], [30, 50, 170], [230, 200, 40]);
    let pairs = [(black, red), (white, red), (blue, yellow), (white, blue), (black, yellow), (red, blue)];
    let lines = |x: f64, y: f64, deg: f64, width: f64, pitch: f64| {
        let t = deg.to_radians();
        let d = -x * t.sin() + y * t.cos();
        (d.rem_euclid(pitch) - pitch / 2.0).abs() < width / 2.0
    };
    let inside = |x: f64, y: f64| -> bool {
        let (dx, dy) = (x - 128.0, y - 128.0);
        let r = dx.hypot(dy);
        if r < 96.0 {
            return r > 20.0 && (dy.atan2(dx) * 8.0 / std::f64::consts::PI).rem_euclid(2.0) < 1.0;
        }
        if r < 110.0 {
            return (102.0..106.0).contains(&r);
        }
        match (dx > 0.0, dy > 0.0) {
            (false, false) => lines(x, y, 15.0, 2.0, 8.0),
            (true, false) => lines(x, y, 40.0, 2.5, 9.0),
            (false, true) => lines(x, y, 75.0, 1.5, 6.0),
            (true, true) => {
                let (u, v) = (x.rem_euclid(9.0) - 4.5, y.rem_euclid(9.0) - 4.5);
                u.hypot(v) < 2.5
            }
        }
    };
    let (w, h) = (3 * P, 2 * P);
    let mut rgb = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (bg, fg) = pairs[(y / P) * 3 + x / P];
            let (bg, fg) = (col(bg), col(fg));
            let mut cover = 0.0;
            for j in 0..8 {
                for i in 0..8 {
                    let (sx, sy) =
                        ((x % P) as f64 + (i as f64 + 0.5) / 8.0, (y % P) as f64 + (j as f64 + 0.5) / 8.0);
                    cover += inside(sx, sy) as u8 as f64 / 64.0;
                }
            }
            let mix = |k: usize| bg[k] + cover * (fg[k] - bg[k]);
            rgb.set(x, y, mix(0), mix(1), mix(2));
        }
    }
    rgb
}

/// The window `(x0, y0, w, h)` of `pictures` side by side, sRGB, each
/// pixel `zoom` wide.
fn save_side_by_side(
    path: &std::path::Path,
    pictures: &[&Rgb],
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    zoom: usize,
) {
    let gap = if pictures.len() > 1 { 4 } else { 0 };
    let tw = pictures.len() * w * zoom + (pictures.len() - 1) * gap;
    let mut buf = vec![255u8; tw * h * zoom * 3];
    for (k, pic) in pictures.iter().enumerate() {
        for y in 0..h * zoom {
            for x in 0..w * zoom {
                let (sx, sy) = (x0 + x / zoom, y0 + y / zoom);
                let o = (y * tw + k * (w * zoom + gap) + x) * 3;
                for (c, pl) in [&pic.r, &pic.g, &pic.b].iter().enumerate() {
                    buf[o + c] = (kodak::srgb_encode(pl.at(sx, sy).clamp(0.0, 1.0)) * 255.0).round() as u8;
                }
            }
        }
    }
    image::save_buffer(path, &buf, tw as u32, (h * zoom) as u32, image::ColorType::Rgb8).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(&args[0]);
    let mut p = XParams::default();
    let (mut blur, mut scale, mut border) = (0.0f64, 1usize, 16usize);
    let (mut bayer, mut rows) = (false, false);
    let mut only: Option<Vec<String>> = None;
    let mut cfa = String::from("xtrans");
    let mut orient: Option<(usize, f64, Vec<f64>)> = None;
    let mut oracle = false;
    let mut bank: Vec<Prior> = Vec::new();
    let mut fit: Option<usize> = None;
    let mut free = [true; 6];
    let mut vary = String::new();
    let mut select: Option<(f64, f64, f64)> = None;
    let mut raw: Vec<PathBuf> = Vec::new();
    let mut seq = false;
    let mut synth = false;
    let mut refine: Option<xtrans::Refine> = None;
    let mut dump: Option<PathBuf> = None;
    let mut crops: Vec<(usize, usize, usize)> = Vec::new();
    let mut i = 1;
    while i < args.len() {
        let val = |i: usize| args[i + 1].as_str();
        let mut flag = false;
        match args[i].as_str() {
            "--blur" => blur = val(i).parse().unwrap(),
            "--scale" => scale = val(i).parse().unwrap(),
            "--border" => border = val(i).parse().unwrap(),
            "--only" => only = Some(val(i).split(',').map(str::to_string).collect()),
            "--c1" => p.c1_diag = band(val(i)),
            "--axis" => p.c1_axis = Some(band(val(i))),
            "--c2" => p.c2 = band(val(i)),
            "--t" => p.transition = val(i).parse().unwrap(),
            "--sigma" => p.sigma = val(i).parse().unwrap(),
            "--floor" => p.floor = val(i).parse().unwrap(),
            "--power" => p.power = val(i).parse().unwrap(),
            "--weight" => {
                p.weight = match val(i) {
                    "fixed" => Weighting::Fixed,
                    "quad" => Weighting::Quadrature,
                    "energy" => Weighting::Energy,
                    o => panic!("weight {o}"),
                }
            }
            "--lmmse" => {
                let v: Vec<f64> = val(i).split(',').map(|x| x.parse().unwrap()).collect();
                let d = p.lmmse.unwrap_or_default();
                p.lmmse = Some(Prior {
                    alpha: v[0],
                    alpha_c: v[1],
                    chroma: [v[2], v[3]],
                    noise: v[4],
                    lens: v[5],
                    ..d
                });
            }
            "--f0" => p.lmmse.as_mut().expect("--lmmse first").f0 = val(i).parse().unwrap(),
            "--cfa" => cfa = val(i).to_string(),
            "--orient" => {
                let v: Vec<f64> = val(i).split(',').map(|x| x.parse().unwrap()).collect();
                orient = Some((v[0] as usize, v[1], if v.len() > 2 { v[2..].to_vec() } else { vec![1.0] }));
            }
            "--fit" => fit = Some(val(i).parse().unwrap()),
            "--free" => {
                for (f, c) in free.iter_mut().zip(val(i).chars()) {
                    *f = c == '1';
                }
            }
            "--vary" => vary = val(i).to_string(),
            "--raw" => raw = val(i).split(',').map(PathBuf::from).collect(),
            "--seq" => {
                seq = true;
                flag = true;
            }
            "--select" => {
                let v: Vec<f64> = val(i).split(',').map(|x| x.parse().unwrap()).collect();
                select = Some((v[0], v[1], v[2]));
            }
            "--bank" => {
                for spec in val(i).split(';') {
                    let v: Vec<f64> = spec.split(',').map(|x| x.parse().unwrap()).collect();
                    bank.push(Prior {
                        alpha: v[0],
                        alpha_c: v[1],
                        chroma: [v[2], v[3]],
                        noise: v[4],
                        lens: v[5],
                        ..Prior::default()
                    });
                }
            }
            "--two" => {
                refine = Some(xtrans::Refine::default());
                flag = true;
            }
            "--refine" => {
                let v: Vec<f64> = val(i).split(',').map(|x| x.parse().unwrap()).collect();
                refine = Some(xtrans::Refine {
                    pre: v[0],
                    window: v[1],
                    hold: v[2],
                    floor: v[3],
                    power: v[4],
                    rounds: v.get(5).map_or(1, |r| *r as usize),
                    check: v.get(6).copied().unwrap_or(4.0),
                    check_window: v.get(7).copied().unwrap_or(1.0),
                });
            }
            "--synth" => {
                synth = true;
                flag = true;
            }
            "--dump" => dump = Some(PathBuf::from(val(i))),
            "--crop" => {
                let v: Vec<usize> = val(i).split(',').map(|x| x.parse().unwrap()).collect();
                crops.push((v[0], v[1], v[2]));
            }
            "--oracle" => {
                oracle = true;
                flag = true;
            }
            "--bayer" => {
                bayer = true;
                flag = true;
            }
            "--rows" => {
                rows = true;
                flag = true;
            }
            o => panic!("unknown {o}"),
        }
        i += if flag { 1 } else { 2 };
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|f| f.extension().is_some_and(|e| e == "png"))
        .collect();
    files.sort();
    if !raw.is_empty() {
        files = raw.clone();
    }
    if synth {
        files = vec![PathBuf::from("edges")];
    }
    if let Some(d) = &dump {
        std::fs::create_dir_all(d).unwrap();
    }
    if let Some(o) = &only {
        files.retain(|f| o.iter().any(|s| f.file_stem().unwrap().to_string_lossy() == *s));
    }
    // Column and row 0 on symmetry lines of the tile, so mirroring continues it.
    let tile = match cfa.as_str() {
        "xtrans" => XTile::from_name(xtrans::FUJI).unwrap().shifted(2, 2),
        "bayer" => XTile::from_name("RGRGRGGBGBGBRGRGRGGBGBGBRGRGRGGBGBGB").unwrap(),
        o => panic!("cfa {o}"),
    };
    eprintln!("{p:?}");

    // Per picture: X-Trans [L, L as (R+2G+B)/4, CPSNR, R, G, B], Bayer [L, CPSNR, R, G, B].
    let one = |f: &PathBuf| -> (String, [f64; 6], [f64; 5]) {
        let mut p = p;
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let mut rgb = if synth {
            synth_edges()
        } else if raw.is_empty() {
            kodak::load_srgb8(f).unwrap()
        } else {
            kodak::binned_raw(f).unwrap().0
        };
        if scale > 1 {
            let up = |p: &Plane| mimizan_core::resize::resize(p, p.width * scale, p.height * scale);
            rgb = Rgb { r: up(&rgb.r), g: up(&rgb.g), b: up(&rgb.b) };
        }
        if blur > 0.0 {
            let bl = |p: &Plane| mimizan_core::usm::gaussian_blur(p, blur);
            rgb = Rgb { r: bl(&rgb.r), g: bl(&rgb.g), b: bl(&rgb.b) };
        }
        // Same frame for both: width and height ≡ 1 (mod 3).
        let (w, h) = ((rgb.width() - 1) / 3 * 3 + 1, (rgb.height() - 1) / 3 * 3 + 1);
        let crop = |p: &Plane| {
            let mut o = Plane::zeros(w, h);
            for y in 0..h {
                o.row_mut(y).copy_from_slice(&p.row(y)[..w]);
            }
            o
        };
        let rgb = Rgb { r: crop(&rgb.r), g: crop(&rgb.g), b: crop(&rgb.b) };
        assert!(tile.mirrors(w, h));
        let truth_b = rgb.luminance();

        let mut b = [f64::NAN; 5];
        let mut bayer_rgb: Option<Rgb> = None;
        if bayer {
            let fb = kodak::mosaic_rgb(&rgb, BayerPhase::RGGB, kodak::quantisation_noise(), &stem);
            let sep = separate(&fb.mosaic, &SeparateParams::default()).unwrap();
            let [br, bg, bb] = mix_plate(&sep, &PlateWeights::channels());
            let out = Rgb { r: br, g: bg, b: bb };
            let c = cpsnr(&out, &rgb, border);
            b = [psnr(&sep.lum, &truth_b, border), c[0], c[1], c[2], c[3]];
            bayer_rgb = Some(out);
        }

        let mut m = xtrans::mosaic_of(&rgb, &tile);
        quantise(&mut m);
        if let (Some(stride), Some(start)) = (fit, p.lmmse) {
            let found = xtrans::fit_prior(&m, &tile, &p, start, stride, free);
            if rows {
                eprintln!(
                    "{stem}: alpha {:.2} alpha_c {:.2} chroma {:.4} {:.4} noise {:.1e} lens {:.2}",
                    found.alpha, found.alpha_c, found.chroma[0], found.chroma[1], found.noise, found.lens
                );
            }
            p.lmmse = Some(found);
        }
        let n = w * h;
        let mut priors: Vec<Prior> = Vec::new();
        if let Some(base) = p.lmmse {
            priors.push(base);
            priors.extend(bank.iter().copied());
            for spec in vary.split(';').filter(|s| !s.is_empty()) {
                let (what, vals) = spec.split_once(':').expect("what:values");
                for v in vals.split(',').map(|x| x.parse::<f64>().unwrap()) {
                    priors.push(match what {
                        "chroma" => Prior { chroma: [base.chroma[0] * v, base.chroma[1] * v], ..base },
                        "c1" => Prior { chroma: [base.chroma[0] * v, base.chroma[1]], ..base },
                        "c2" => Prior { chroma: [base.chroma[0], base.chroma[1] * v], ..base },
                        "lens" => Prior { lens: (base.lens + v).max(0.0), ..base },
                        "alpha" => Prior { alpha: base.alpha + v, ..base },
                        "noise" => Prior { noise: base.noise * v, ..base },
                        o => panic!("vary {o}"),
                    });
                }
            }
            if let Some((count, k, factors)) = &orient {
                for f in factors {
                    for j in 0..*count {
                        let theta = std::f64::consts::PI * j as f64 / *count as f64;
                        priors.push(Prior {
                            chroma: [base.chroma[0] * f, base.chroma[1] * f],
                            oriented: Some((theta, *k)),
                            ..base
                        });
                    }
                }
            }
        }
        let xs = if let (true, Some((sigma, sharp, bias))) = (priors.len() > 1, select) {
            xtrans::separate_bank(&m, &tile, &p, &priors, &xtrans::Selection { sigma, sharp, bias })
        } else if priors.len() > 1 {
            let hyps: Vec<xtrans::Hypothesis> = priors
                .iter()
                .map(|q| xtrans::hypothesis(&m, &tile, &XParams { lmmse: Some(*q), ..p }))
                .collect();
            let (c1, c2) = if let Some((sigma, sharp, bias)) = select {
                xtrans::select(&hyps, &xtrans::Selection { sigma, sharp, bias })
            } else {
                assert!(oracle, "several priors need --oracle or --select");
                // Score: local squared error of the remodulated chrominance.
                let g = mimizan_core::filter::gaussian_kernel(p.sigma);
                let scores: Vec<Plane> = hyps
                    .iter()
                    .map(|hy| {
                        let e = Plane::from_vec(
                            w,
                            h,
                            (0..n)
                                .map(|i| {
                                    let t1 = rgb.g.data[i] - (rgb.r.data[i] + rgb.b.data[i]) / 2.0;
                                    let t2 = (rgb.r.data[i] - rgb.b.data[i]) / 2.0;
                                    0.25 * (hy.c1.data[i] - t1).powi(2) + 0.44 * (hy.c2.data[i] - t2).powi(2)
                                })
                                .collect(),
                        );
                        mimizan_core::filter::lowpass(&e, &g)
                    })
                    .collect();
                let mut c1 = Plane::zeros(w, h);
                let mut c2 = Plane::zeros(w, h);
                for i in 0..n {
                    let best = (0..hyps.len())
                        .min_by(|a, b| scores[*a].data[i].total_cmp(&scores[*b].data[i]))
                        .unwrap();
                    c1.data[i] = hyps[best].c1.data[i];
                    c2.data[i] = hyps[best].c2.data[i];
                }
                (c1, c2)
            };
            xtrans::from_chroma(&m, &tile, c1, c2)
        } else {
            xtrans::separate_xtrans(&m, &tile, &p)
        };
        let xs = match &refine {
            Some(r) => xtrans::refine(&m, &tile, &xs, r),
            None => xs,
        };
        let truth_x = Plane::from_vec(
            w,
            h,
            (0..n)
                .map(|i| xs.g * rgb.g.data[i] + (1.0 - xs.g) * (rgb.r.data[i] + rgb.b.data[i]) / 2.0)
                .collect(),
        );
        let xrgb = xs.rgb();
        let c = cpsnr(&xrgb, &rgb, border);
        if let Some(d) = &dump {
            let mut pics: Vec<&Rgb> = vec![&rgb, &xrgb];
            pics.extend(bayer_rgb.as_ref());
            save_side_by_side(&d.join(format!("{stem}_truth.png")), &[&rgb], 0, 0, w, h, 1);
            save_side_by_side(&d.join(format!("{stem}_xtrans.png")), &[&xrgb], 0, 0, w, h, 1);
            for &(cx, cy, size) in &crops {
                let (cw, ch) = (size.min(w - cx), size.min(h - cy));
                save_side_by_side(&d.join(format!("{stem}_crop_{cx}_{cy}.png")), &pics, cx, cy, cw, ch, 4);
            }
        }
        if synth {
            // Per pair of colours, and in each the star, the lines and the dots.
            let part = |x0: usize, y0: usize, pw: usize, ph: usize| {
                let cut = |p: &Plane| {
                    let mut o = Plane::zeros(pw, ph);
                    for y in 0..ph {
                        o.row_mut(y).copy_from_slice(&p.row(y0 + y)[x0..x0 + pw]);
                    }
                    o
                };
                let (a, b) = (
                    Rgb { r: cut(&xrgb.r), g: cut(&xrgb.g), b: cut(&xrgb.b) },
                    Rgb { r: cut(&rgb.r), g: cut(&rgb.g), b: cut(&rgb.b) },
                );
                cpsnr(&a, &b, 0)[0]
            };
            let names = ["black/red", "white/red", "blue/yellow", "white/blue", "black/yellow", "red/blue"];
            for (k, name) in names.iter().enumerate() {
                let (px, py) = ((k % 3) * 256, (k / 3) * 256);
                eprintln!(
                    "  {name:13} star {:5.2}  lines {:5.2} {:5.2} {:5.2}  dots {:5.2}",
                    part(px + 58, py + 58, 140, 140),
                    part(px + 6, py + 6, 44, 44),
                    part(px + 206, py + 6, 44, 44),
                    part(px + 6, py + 206, 44, 44),
                    part(px + 206, py + 206, 40, 40),
                );
            }
        }
        let x = [
            psnr(&xs.lum, &truth_x, border),
            psnr(&xs.mix([0.25, 0.5, 0.25]), &truth_b, border),
            c[0],
            c[1],
            c[2],
            c[3],
        ];
        (stem, x, b)
    };
    let results: Vec<(String, [f64; 6], [f64; 5])> =
        if seq { files.iter().map(one).collect() } else { files.par_iter().map(one).collect() };

    let line = |name: &str, x: &[f64; 6], b: &[f64; 5]| {
        println!(
            "{name:10} | X-Trans L {:6.2}  L(R+2G+B) {:6.2}  RGB {:6.2} (R {:6.2} G {:6.2} B {:6.2}) | Bayer L {:6.2}  RGB {:6.2} (R {:6.2} G {:6.2} B {:6.2})",
            x[0], x[1], x[2], x[3], x[4], x[5], b[0], b[1], b[2], b[3], b[4]
        );
    };
    let (mut sx, mut sb) = ([0.0; 6], [0.0; 5]);
    let mut ahead = [0usize; 2];
    for (stem, x, b) in &results {
        if rows {
            line(stem, x, b);
        }
        sx.iter_mut().zip(x).for_each(|(s, v)| *s += v);
        sb.iter_mut().zip(b).for_each(|(s, v)| *s += v);
        ahead[0] += (x[1] > b[0]) as usize;
        ahead[1] += (x[2] > b[1]) as usize;
    }
    let k = results.len() as f64;
    sx.iter_mut().for_each(|v| *v /= k);
    sb.iter_mut().for_each(|v| *v /= k);
    line(&format!("mean ({})", results.len()), &sx, &sb);
    if bayer {
        println!(
            "X-Trans ahead of Bayer: luminance {} of {}, colour {} of {}",
            ahead[0],
            results.len(),
            ahead[1],
            results.len()
        );
    }
}
