//! The least-squares Bayer demosaic taken apart, for experiments
//! (`bench/bayer-rings/lab.py`).
//!
//! It reads a mosaic as raw little-endian `f32`, one value a photosite in
//! light between 0 and 1, the corner red. The search over the frame is
//! kept in a JSON file and read back, so a run that only changes the bank
//! or the weighing does not search again. Everything else is said by
//! `key=value` arguments:
//!
//! ```text
//! bayer_ls_lab mosaic.f32 WIDTH out.f32 spectrum=found.json \
//!     region=832,904,1248,904 bank=default sigma=6 cmul=1
//! ```
//!
//! - `spectrum=FILE`: where the found tone and spectrum are kept.
//! - `region=x,y,w,h`: separate only this part (even numbers), with a
//!   margin around it, under the spectrum found on the whole frame.
//! - `alpha= alpha_c= c1= c2= cmul= noise= gauss=`: the found prior
//!   changed; `gauss=σ` drops the measured lens for a Gaussian one.
//! - `tone=auto|srgb|EXPONENT`.
//! - `bank=`: `default` (the one of `bayer_ls`), `before` (that one without
//!   its vivid spectra), `found`, or a list joined
//!   by `+` of `f` (the found one), `cM` (M times the colour), `oM:K`
//!   (eight directions, M times the colour, `cos²` to the power K),
//!   `aK` (eight directions, luminance falling as `f¹`), `aM:K` (those
//!   with M times the colour), `sM:A` (M times the colour falling with the
//!   exponent A), `tM:A:K` (eight directions of that). `@H` after an
//!   item puts it H behind in the score. `CL`, `OL:K` and `TL:A:K` are
//!   `c`, `o` and `t` with the colour at the level L instead of a multiple.
//!   `SL:K:LENS:NOISE` and `SL:K:LENS:NOISE:ALPHA:ALPHA_C` are eight
//!   directions that take nothing from the frame: the prior the search
//!   starts from, the colour at the level L, behind a Gaussian lens;
//!   `PL:LENS:NOISE` is that spectrum without a direction. The
//!   bank of `bayer_ls` is `before` and `S1:8:0.75:0.01@0.5`.
//! - `sigma= near= left_out= sharp= bias= rounds= check= hold=`: the knobs.
//! - `second=0`: without the second step; `checker=0`: without the line checker.
//!
//! The picture goes out as raw `f32`, three values a pixel, and one line
//! of JSON on stdout says what was found and how long each part took.

use anyhow::{anyhow, bail, Context, Result};
use mimizan_cli::bayer2::{self, Spectrum, Weighing};
use mimizan_cli::bayer_ls::{self, Knobs, Tone, BAYER6};
use mimizan_lab_xtrans::plane::Plane;
use mimizan_lab_xtrans::xtrans::{self, Prior, Refine, XTile};
use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

const MARGIN: usize = 64;

fn tone_name(t: Tone) -> serde_json::Value {
    match t {
        Tone::Srgb => serde_json::json!("srgb"),
        Tone::Power { exponent, .. } => serde_json::json!(exponent),
    }
}

fn tone_from(v: &serde_json::Value) -> Result<Tone> {
    match v {
        serde_json::Value::String(s) if s == "srgb" => Ok(Tone::Srgb),
        serde_json::Value::Number(n) => Ok(Tone::power(n.as_f64().ok_or_else(|| anyhow!("no exponent"))?)),
        other => bail!("no tone: {other}"),
    }
}

fn spectrum_json(tone: Tone, s: &Spectrum) -> serde_json::Value {
    serde_json::json!({
        "tone": tone_name(tone),
        "alpha": s.prior.alpha,
        "alpha_c": s.prior.alpha_c,
        "f0": s.prior.f0,
        "chroma": s.prior.chroma,
        "noise": s.prior.noise,
        "lens_sigma": s.prior.lens,
        "lens": s.lens.as_ref().map(|l| l.as_ref().clone()),
    })
}

fn spectrum_from(v: &serde_json::Value) -> Result<(Tone, Spectrum)> {
    let f = |k: &str| v[k].as_f64().ok_or_else(|| anyhow!("the spectrum file has no {k}"));
    let chroma = v["chroma"].as_array().ok_or_else(|| anyhow!("no chroma"))?;
    let prior = Prior {
        alpha: f("alpha")?,
        alpha_c: f("alpha_c")?,
        f0: f("f0")?,
        chroma: [chroma[0].as_f64().unwrap_or(0.05), chroma[1].as_f64().unwrap_or(0.05)],
        noise: f("noise")?,
        lens: f("lens_sigma")?,
        oriented: None,
    };
    let lens = v["lens"]
        .as_array()
        .map(|l| Arc::new(l.iter().map(|x| x.as_f64().unwrap_or(1.0)).collect::<Vec<f64>>()));
    Ok((tone_from(&v["tone"])?, Spectrum { prior, lens }))
}

fn towards(q: &Spectrum, k: f64) -> Vec<Spectrum> {
    (0..8).map(|j| q.with(Prior { oriented: Some((PI * j as f64 / 8.0, k)), ..q.prior })).collect()
}

fn bank_of(name: &str, found: &Spectrum) -> Result<Vec<(Spectrum, f64)>> {
    if name == "default" {
        let (spectra, behind) = bayer_ls::bayer_priors(found);
        return Ok(spectra.into_iter().zip(behind).collect());
    }
    // The bank before the vivid spectra came into it.
    let name = if name == "before" { "f+c0.0625+c0.25+c4+c16+o4:8+a32" } else { name };
    if name == "found" {
        return Ok(vec![(found.clone(), 0.0)]);
    }
    let colour = |f: f64| found.with(Prior { chroma: found.prior.chroma.map(|c| c * f), ..found.prior });
    let mut all = Vec::new();
    for item in name.split('+') {
        // `@H` after an item: it starts H behind in the score.
        let (item, behind) = match item.split_once('@') {
            Some((i, h)) => (i, h.parse::<f64>().with_context(|| format!("handicap of {i}"))?),
            None => (item, 0.0),
        };
        let mut bank = Vec::new();
        let (kind, rest) = item.split_at(1);
        let nums: Vec<f64> = if rest.is_empty() {
            Vec::new()
        } else {
            rest.split(':')
                .map(|s| s.parse::<f64>().with_context(|| format!("bank item {item}")))
                .collect::<Result<_>>()?
        };
        match (kind, nums.as_slice()) {
            ("f", []) => bank.push(found.clone()),
            // The colour at a level of its own, both chrominances alike.
            ("C", [l]) => bank.push(found.with(Prior { chroma: [*l, *l], ..found.prior })),
            ("O", [l, k]) => {
                let q = found.with(Prior { chroma: [*l, *l], ..found.prior });
                bank.extend(towards(&q, *k));
            }
            ("T", [l, a, k]) => {
                let q = found.with(Prior { chroma: [*l, *l], alpha_c: *a, ..found.prior });
                bank.extend(towards(&q, *k));
            }
            // Eight directions of the prior the search starts from, the colour
            // at the level L behind a Gaussian lens: nothing of it is the
            // frame's.
            ("S", [l, k, lens, noise]) => {
                let q = Spectrum::plain(Prior {
                    lens: *lens,
                    noise: *noise,
                    chroma: [*l, *l],
                    ..Prior::default()
                });
                bank.extend(towards(&q, *k));
            }
            // That spectrum without a direction.
            ("P", [l, lens, noise]) => {
                bank.push(Spectrum::plain(Prior {
                    lens: *lens,
                    noise: *noise,
                    chroma: [*l, *l],
                    ..Prior::default()
                }));
            }
            ("S", [l, k, lens, noise, alpha, alpha_c]) => {
                let q = Spectrum::plain(Prior {
                    lens: *lens,
                    noise: *noise,
                    chroma: [*l, *l],
                    alpha: *alpha,
                    alpha_c: *alpha_c,
                    ..Prior::default()
                });
                bank.extend(towards(&q, *k));
            }
            ("c", [m]) => bank.push(colour(*m)),
            ("o", [m, k]) => bank.extend(towards(&colour(*m), *k)),
            ("a", [k]) => {
                let along = found.with(Prior { alpha: found.prior.alpha.min(1.0), ..found.prior });
                bank.extend(towards(&along, *k));
            }
            ("a", [m, k]) => {
                let q = colour(*m);
                let along = q.with(Prior { alpha: q.prior.alpha.min(1.0), ..q.prior });
                bank.extend(towards(&along, *k));
            }
            ("s", [m, a]) => {
                let q = colour(*m);
                bank.push(q.with(Prior { alpha_c: *a, ..q.prior }));
            }
            ("t", [m, a, k]) => {
                let q = colour(*m);
                let q = q.with(Prior { alpha_c: *a, ..q.prior });
                bank.extend(towards(&q, *k));
            }
            _ => bail!("bank item {item} is not understood"),
        }
        all.extend(bank.into_iter().map(|s| (s, behind)));
    }
    if all.is_empty() {
        bail!("the bank is empty");
    }
    Ok(all)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        bail!("usage: bayer_ls_lab <mosaic.f32> <width> <out.f32> [key=value ...]");
    }
    let (input, out) = (PathBuf::from(&args[0]), PathBuf::from(&args[2]));
    let width: usize = args[1].parse().context("the width")?;
    let mut opt: BTreeMap<String, String> = BTreeMap::new();
    for a in &args[3..] {
        let (k, v) = a.split_once('=').ok_or_else(|| anyhow!("{a} is not key=value"))?;
        opt.insert(k.to_string(), v.to_string());
    }
    let num = |k: &str| -> Result<Option<f64>> {
        opt.get(k).map(|v| v.parse::<f64>().with_context(|| format!("{k}={v}"))).transpose()
    };

    let bytes = std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
    let linear: Vec<f64> =
        bytes.as_chunks::<4>().0.iter().map(|b| f64::from(f32::from_le_bytes(*b))).collect();
    if width == 0 || !linear.len().is_multiple_of(width) {
        bail!("{} values are no rows of {width}", linear.len());
    }
    let height = linear.len() / width;

    let d = Knobs::default();
    let knobs = Knobs {
        sigma: num("sigma")?.unwrap_or(d.sigma),
        near: num("near")?.unwrap_or(d.near),
        left_out: num("left_out")?.unwrap_or(d.left_out),
        sharp: num("sharp")?.unwrap_or(d.sharp),
        bias: num("bias")?.unwrap_or(d.bias),
        lens_rounds: num("lens_rounds")?.map_or(d.lens_rounds, |v| v as usize),
        tone: None,
        rounds: num("rounds")?.map_or(d.rounds, |v| v as usize),
        check: num("check")?.unwrap_or(d.check),
        hold: num("hold")?.unwrap_or(d.hold),
    };

    // The search, or what an earlier run kept of it.
    let t0 = Instant::now();
    let kept = opt.get("spectrum").map(PathBuf::from);
    let (mut tone, found) = match kept.as_deref().filter(|p| Path::exists(p)) {
        Some(p) => spectrum_from(&serde_json::from_slice(&std::fs::read(p)?)?)?,
        None => {
            let fit = Plane::from_vec(width, height, linear.clone());
            let tone = bayer_ls::tone_of(&fit, &knobs);
            let start = Prior { lens: 0.3, ..Prior::default() };
            let found = bayer2::fit_spectrum(&tone.mosaic(&fit), start, 1, knobs.lens_rounds);
            if let Some(p) = &kept {
                std::fs::write(p, serde_json::to_vec(&spectrum_json(tone, &found))?)?;
            }
            (tone, found)
        }
    };
    let searched = t0.elapsed().as_secs_f64();
    match opt.get("tone").map(String::as_str) {
        None | Some("auto") => {}
        Some("srgb") => tone = Tone::Srgb,
        Some(g) => tone = Tone::power(g.parse().context("tone")?),
    }

    let mut p = found.prior;
    if let Some(v) = num("alpha")? {
        p.alpha = v;
    }
    if let Some(v) = num("alpha_c")? {
        p.alpha_c = v;
    }
    if let Some(v) = num("c1")? {
        p.chroma[0] = v;
    }
    if let Some(v) = num("c2")? {
        p.chroma[1] = v;
    }
    if let Some(v) = num("cmul")? {
        p.chroma = p.chroma.map(|c| c * v);
    }
    if let Some(v) = num("noise")? {
        p.noise = v;
    }
    let used = match num("gauss")? {
        Some(sigma) => Spectrum::plain(Prior { lens: sigma, ..p }),
        None => found.with(p),
    };
    let bank = bank_of(opt.get("bank").map_or("default", String::as_str), &used)?;
    // How probable the whole frame is under the spectrum the bank is built on.
    let probable = (opt.get("likelihood").map(String::as_str) == Some("1")).then(|| {
        let whole = tone.mosaic_of(&linear, width);
        bayer2::likelihood(&whole, &used, &Plane::from_vec(width, height, vec![0.0; width * height]))
    });

    // The part that is separated: all of it, or a region with its margin.
    let (rx, ry, rw, rh) = match opt.get("region") {
        None => (0, 0, width, height),
        Some(r) => {
            let v: Vec<usize> = r.split(',').map(|s| s.parse().context("region")).collect::<Result<_>>()?;
            let [x, y, w, h] = v[..] else { bail!("region=x,y,w,h") };
            if x % 2 != 0 || y % 2 != 0 || w % 2 != 0 || h % 2 != 0 || x + w > width || y + h > height {
                bail!("the region is odd or outside the frame");
            }
            (x, y, w, h)
        }
    };
    let (x0, y0) = (rx.saturating_sub(MARGIN), ry.saturating_sub(MARGIN));
    let (x1, y1) = ((rx + rw + MARGIN).min(width), (ry + rh + MARGIN).min(height));
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut part = Vec::with_capacity(cw * ch);
    for y in y0..y1 {
        part.extend_from_slice(&linear[y * width + x0..y * width + x1]);
    }

    let t1 = Instant::now();
    let m = tone.mosaic_of(&part, cw);
    let sel = Weighing {
        sigma: knobs.sigma,
        near: knobs.near,
        left_out: knobs.left_out,
        sharp: knobs.sharp,
        bias: knobs.bias,
    };
    let (spectra, behind): (Vec<Spectrum>, Vec<f64>) = bank.into_iter().unzip();
    let (c1, c2) = bayer2::separate_handicapped(&m, &spectra, &behind, &sel, &|_| {});
    let tile = XTile::from_name(BAYER6).ok_or_else(|| anyhow!("the Bayer tile"))?;
    let sep = xtrans::from_chroma(&m, &tile, c1, c2);
    let sep = if opt.get("second").map(String::as_str) == Some("0") {
        sep
    } else {
        let refine =
            Refine { rounds: knobs.rounds, check: knobs.check, hold: knobs.hold, ..Refine::default() };
        xtrans::refine(&m, &tile, &sep, &refine)
    };
    let rgb = sep.mapped(|v| tone.back(v)).rgb();
    let rgb = if opt.get("checker").map(String::as_str) == Some("0") {
        rgb
    } else {
        bayer_ls::without_line_checker(rgb)
    };
    let separated = t1.elapsed().as_secs_f64();

    let mut bytes = Vec::with_capacity(rw * rh * 12);
    for y in 0..rh {
        let at = (ry - y0 + y) * cw + (rx - x0);
        for x in 0..rw {
            for plane in [&rgb.r, &rgb.g, &rgb.b] {
                bytes.extend_from_slice(&(plane.data[at + x] as f32).to_le_bytes());
            }
        }
    }
    std::fs::write(&out, bytes).with_context(|| format!("writing {}", out.display()))?;
    println!(
        "{}",
        serde_json::json!({
            "width": rw,
            "height": rh,
            "found": spectrum_json(tone, &found),
            "bank": spectra.len(),
            "likelihood": probable,
            "searched_s": searched,
            "separated_s": separated,
        })
    );
    Ok(())
}
