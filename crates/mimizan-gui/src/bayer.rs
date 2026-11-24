//! A Bayer RAW written out as a linear DNG.
//!
//! *least-squares demosaic* in the Negative window. The mosaic is separated
//! by the least squares the bench measured, and the picture is written
//! beside the RAW, named `<stem>.mimizan.dng` so the camera file is left
//! alone. The darkroom then opens that DNG and does not separate it again,
//! which is how a Fuji X-Trans file is opened. Off, the checkbox leaves the
//! file on the measured separation.

use mimizan_cli::bayer_ls::{self, Knobs};
use mimizan_lab_xtrans::convert::{Developed, Frame, WHITE};
use mimizan_lab_xtrans::dng;
use mimizan_lab_xtrans::xtrans::XTile;
use rawler::decoders::RawDecodeParams;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::{RawImage, RawImageData};
use rayon::prelude::*;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What a DNG written here ends its name with.
const MARK: &str = ".mimizan.dng";

/// What the DNG says it was written by.
const SOFTWARE: &str = concat!("Mimizan Lab ", env!("CARGO_PKG_VERSION"), " (Bayer least squares)");

/// Where a Bayer file stands when the checkbox is on.
pub enum Place {
    /// Not a Bayer mosaic, or already the DNG this path writes.
    NotBayer,
    /// The DNG beside the RAW is the file to open.
    Ready(PathBuf),
    /// The RAW still has to be written out.
    Needs(Head),
}

/// What the head of a Bayer file says, and where its DNG will be.
pub struct Head {
    pub raw: PathBuf,
    pub dng: PathBuf,
    pub width: usize,
    pub height: usize,
}

/// A DNG this path wrote. The stem before `.mimizan.dng` is the RAW's.
pub fn is_ours(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.len() > MARK.len() && n.to_ascii_lowercase().ends_with(MARK))
}

/// The DNG of `raw`, beside it.
pub fn dng_of(raw: &Path) -> PathBuf {
    let stem = raw.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    raw.with_file_name(format!("{stem}.mimizan.dng"))
}

/// A DNG that is there and holds something. An empty file is not one:
/// the writer will not replace a file that exists, and an empty one would
/// otherwise block the RAW for good.
pub fn dng_is_ready(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// The RAW a DNG written here came from, when that RAW is still beside it.
/// A path that is not such a DNG is returned as it stands.
pub fn raw_of(path: &Path) -> Option<PathBuf> {
    if !is_ours(path) {
        return Some(path.to_path_buf());
    }
    let name = path.file_name()?.to_str()?;
    let stem = &name[..name.len() - MARK.len()];
    let dir = path.parent()?;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p == path {
            continue;
        }
        let Some(file_stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if file_stem.eq_ignore_ascii_case(stem) && crate::browser::is_raw(&p) {
            return Some(p);
        }
    }
    None
}

/// The head only. A file this path does not write out is [`Place::NotBayer`].
pub fn place(path: &Path) -> Place {
    if is_ours(path) {
        return Place::NotBayer;
    }
    let Ok(source) = RawSource::new(path) else {
        return Place::NotBayer;
    };
    let Ok(decoder) = rawler::get_decoder(&source) else {
        return Place::NotBayer;
    };
    let Ok(img) = decoder.raw_image(&source, &RawDecodeParams::default(), true) else {
        return Place::NotBayer;
    };
    if bayer_name(&img).is_none() {
        return Place::NotBayer;
    }
    let dng = dng_of(path);
    if dng_is_ready(&dng) {
        return Place::Ready(dng);
    }
    Place::Needs(Head { raw: path.to_path_buf(), dng, width: img.width, height: img.height })
}

/// Write the DNG of `head`. The highlights stay in the file, as they do for
/// an X-Trans plate. `tell` hears each step, and the share of the separation.
pub fn write(head: &Head, tell: &dyn Fn(String)) -> Result<String, String> {
    if head.dng.exists() {
        return Err(format!("{} is there already", head.dng.display()));
    }
    let started = Instant::now();
    tell("reading the Bayer file".into());
    let source = RawSource::new(&head.raw).map_err(|e| format!("cannot be read: {e}"))?;
    let decoder =
        rawler::get_decoder(&source).map_err(|e| format!("not a raw file this program reads: {e}"))?;
    let img = decoder
        .raw_image(&source, &RawDecodeParams::default(), false)
        .map_err(|e| format!("cannot be decoded: {e}"))?;
    let dev = develop(&img, tell)?;
    let burnt = dev.burnt as f64 / (dev.frame.width * dev.frame.height) as f64;
    let (alpha, alpha_c) = (dev.prior.alpha, dev.prior.alpha_c);

    tell("writing the DNG".into());
    let part = PathBuf::from(format!("{}.part", head.dng.display()));
    let written = File::create(&part).map_err(|e| format!("{}: {e}", part.display())).and_then(|file| {
        let mut out = BufWriter::new(file);
        dng::write(&mut out, &source, decoder.as_ref(), img, dev, true, SOFTWARE)?;
        let file = out.into_inner().map_err(|e| format!("{}: {e}", part.display()))?;
        file.sync_all().map_err(|e| format!("{}: {e}", part.display()))
    });
    let renamed = written
        .and_then(|()| fs::rename(&part, &head.dng).map_err(|e| format!("{}: {e}", head.dng.display())));
    if let Err(e) = renamed {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    Ok(format!(
        "DNG written in {:.0} s, prior f^{alpha:.2} and f^{alpha_c:.2}, {:.3}% burnt out and kept",
        started.elapsed().as_secs_f64(),
        100.0 * burnt
    ))
}

/// The picture, in the camera's own channels. The frame's corner is a red
/// photosite, so the tile is the RGGB one the bench measured.
fn develop(img: &RawImage, tell: &dyn Fn(String)) -> Result<Developed, String> {
    let name = bayer_name(img).ok_or("the file holds no Bayer mosaic")?;
    let (rx, ry) = red_site(&name).ok_or("the mosaic has no red photosite")?;
    let (cx, cy, cw, ch) = crop_of(img)?;
    let x0 = cx + usize::from(cx % 2 != rx);
    let y0 = cy + usize::from(cy % 2 != ry);
    let width = (cx + cw).saturating_sub(x0);
    let height = (cy + ch).saturating_sub(y0);
    if width < 24 || height < 24 {
        return Err("the frame is too small to separate".into());
    }
    let RawImageData::Integer(raw) = &img.data else {
        return Err("the mosaic is not in whole numbers".into());
    };
    if img.cpp != 1 || raw.len() != img.width * img.height {
        return Err("the mosaic does not have one value per photosite".into());
    }
    let white = f64::from(*img.whitelevel.0.first().ok_or("the file gives no white level")?);
    let levels = &img.blacklevel;
    if levels.cpp != 1 || levels.levels.len() != levels.width * levels.height || levels.levels.is_empty() {
        return Err("the file's black levels are not per photosite".into());
    }
    let blacks: Vec<f64> = levels.levels.iter().map(|l| f64::from(l.as_f32())).collect();
    let black_at = |x: usize, y: usize| blacks[(y % levels.height) * levels.width + x % levels.width];
    let black = blacks.iter().sum::<f64>() / blacks.len() as f64;
    if !(white.is_finite() && black.is_finite() && white > black && black >= 0.0 && white <= 65535.0) {
        return Err(format!("black {black} and white {white} are no range"));
    }

    let mut mosaic = vec![0.0; width * height];
    mosaic.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let src = y0 + y;
        let line = &raw[src * img.width + x0..][..width];
        for (x, (v, sample)) in row.iter_mut().zip(line).enumerate() {
            *v = (f64::from(*sample) - black_at(x0 + x, src)) / (white - black);
        }
    });
    let burnt: usize = (0..height)
        .into_par_iter()
        .map(|y| {
            let src = y0 + y;
            raw[src * img.width + x0..][..width].iter().filter(|v| f64::from(**v) >= white - 1.0).count()
        })
        .sum::<usize>();

    // The balance: the camera's, or the one that makes the frame grey.
    // Green is 1. The frame's corner is red, then green, then green, then blue.
    let from_file = [img.wb_coeffs[0], img.wb_coeffs[1], img.wb_coeffs[2]].map(f64::from);
    let balance_from_file = from_file.iter().all(|v| v.is_finite() && *v > 0.0);
    let balance = if balance_from_file {
        [from_file[0] / from_file[1], 1.0, from_file[2] / from_file[1]]
    } else {
        let mut sums = [(0.0, 0.0); 3];
        for (i, v) in mosaic.iter().enumerate() {
            let c = channel_at(i % width, i / width);
            sums[c] = (sums[c].0 + v, sums[c].1 + 1.0);
        }
        let mean = sums.map(|(s, n)| (s / n).max(1e-6));
        [mean[1] / mean[0], 1.0, mean[1] / mean[2]]
    };
    mosaic.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v *= balance[channel_at(x, y)];
        }
    });
    // A photosite at white measures nothing but "at least this much".
    // The prior is searched on the mosaic cut there, and the picture keeps
    // what was recorded above it.
    let ceiling = balance.iter().copied().fold(f64::INFINITY, f64::min);
    let fit: Vec<f64> = mosaic.par_iter().map(|v| v.min(ceiling)).collect();

    tell("searching the prior".into());
    tell("separating luminance and colour".into());
    let (rgb, prior) = bayer_ls::separate(&mosaic, &fit, width, &Knobs::default(), &|share| {
        tell(format!("separating luminance and colour, {:.0}%", share.clamp(0.0, 1.0) * 100.0));
    });

    let mut data = vec![0u16; width * height * 3];
    let below: usize = data
        .par_chunks_mut(3 * width)
        .enumerate()
        .map(|(y, row)| {
            let lines = [rgb.r.row(y), rgb.g.row(y), rgb.b.row(y)];
            let mut below = 0;
            for (x, px) in row.chunks_mut(3).enumerate() {
                for (c, v) in px.iter_mut().enumerate() {
                    let level = (lines[c][x] / balance[c] * f64::from(WHITE)).round();
                    below += usize::from(level < 0.0);
                    *v = level.clamp(0.0, f64::from(WHITE)) as u16;
                }
            }
            below
        })
        .sum();
    let tile = XTile::from_name(bayer_ls::BAYER6).expect("the Bayer tile");
    Ok(Developed {
        frame: Frame { x: x0, y: y0, width, height, tile },
        data,
        below,
        balance,
        balance_from_file,
        prior,
        burnt,
        sums: [0.0; 3],
        seconds: [0.0; 3],
    })
}

fn bayer_name(img: &RawImage) -> Option<String> {
    let RawPhotometricInterpretation::Cfa(config) = &img.photometric else {
        return None;
    };
    if config.cfa.width != 2 || config.cfa.height != 2 || config.cfa.name.len() != 4 {
        return None;
    }
    Some(config.cfa.name.clone())
}

/// Column and row, inside the 2×2, of the red photosite. The sensor's
/// origin is that tile's origin.
fn red_site(name: &str) -> Option<(usize, usize)> {
    let b = name.as_bytes();
    (0..4).find(|i| b[*i].eq_ignore_ascii_case(&b'R')).map(|i| (i % 2, i / 2))
}

fn crop_of(img: &RawImage) -> Result<(usize, usize, usize, usize), String> {
    let crop = img
        .crop_area
        .or(img.active_area)
        .map_or((0, 0, img.width, img.height), |r| (r.p.x, r.p.y, r.d.w, r.d.h));
    if crop.0 + crop.2 > img.width || crop.1 + crop.3 > img.height || crop.2 == 0 || crop.3 == 0 {
        return Err("the file's crop lies outside its mosaic".into());
    }
    Ok(crop)
}

/// 0 red, 1 green, 2 blue, on a frame whose corner is red.
fn channel_at(x: usize, y: usize) -> usize {
    match (x % 2, y % 2) {
        (0, 0) => 0,
        (1, 1) => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dng_sits_beside_the_raw_and_names_it() {
        let raw = PathBuf::from("/photos/DSC_100.NEF");
        let dng = dng_of(&raw);
        assert_eq!(dng, PathBuf::from("/photos/DSC_100.mimizan.dng"));
        assert!(!is_ours(&raw));
        assert!(is_ours(&dng));
        assert!(is_ours(Path::new("/photos/DSC_100.MIMIZAN.DNG")));
        assert_eq!(red_site("RGGB"), Some((0, 0)));
        assert_eq!(red_site("BGGR"), Some((1, 1)));
        assert_eq!(red_site("GRBG"), Some((1, 0)));
        assert_eq!(red_site("GBRG"), Some((0, 1)));
        assert_eq!(channel_at(0, 0), 0);
        assert_eq!(channel_at(1, 0), 1);
        assert_eq!(channel_at(0, 1), 1);
        assert_eq!(channel_at(1, 1), 2);
    }

    #[test]
    fn the_raw_is_found_beside_its_dng() {
        let dir = std::env::temp_dir().join(format!("mimizan-bayer-{}", std::process::id()));
        let _ = fs::create_dir(&dir);
        let raw = dir.join("DSC.NEF");
        let dng = dir.join("DSC.mimizan.dng");
        fs::write(&raw, b"x").unwrap();
        fs::write(&dng, b"x").unwrap();
        assert_eq!(raw_of(&dng).unwrap(), raw);
        assert_eq!(raw_of(&raw).unwrap(), raw);
        assert!(dng_is_ready(&dng));
        fs::write(&dng, b"").unwrap();
        assert!(!dng_is_ready(&dng));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_that_is_not_a_bayer_file_is_left_alone() {
        assert!(matches!(place(Path::new("DSC.NEF")), Place::NotBayer));
        assert!(matches!(place(Path::new("no-such-file.RAF")), Place::NotBayer));
    }
}
