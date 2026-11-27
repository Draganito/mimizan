//! The least-squares Bayer demosaic of one file, for the colour-ring bench
//! (`bench/bayer-rings`).
//!
//! The desktop writes this demosaic as a DNG when *least-squares demosaic*
//! is on; the command has no switch for it. This example runs the same
//! `bayer_ls::develop` on a RAW or a CFA DNG and writes the three channels
//! as a 16-bit PPM, the camera's own channels, not balanced, 65535 at the
//! sensor's white. One line of JSON on stdout says where the frame lies in
//! the mosaic, what the balance is and which prior the search found.
//!
//! ```text
//! cargo run --release -p mimizan-cli --example bayer_ls_develop -- in.dng out.ppm
//! ```

use anyhow::{anyhow, bail, Context, Result};
use mimizan_cli::bayer_ls;
use rawler::decoders::RawDecodeParams;
use std::io::Write;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let [raw, out] = args.as_slice() else {
        bail!("usage: bayer_ls_develop <raw or CFA DNG> <out.ppm>");
    };
    let source =
        mimizan_core::decode::raw_source(raw).with_context(|| format!("reading {}", raw.display()))?;
    let decoder = rawler::get_decoder(&source).map_err(|e| anyhow!("{}: {e}", raw.display()))?;
    let img = decoder
        .raw_image(&source, &RawDecodeParams::default(), false)
        .map_err(|e| anyhow!("{}: {e}", raw.display()))?;
    let dev = bayer_ls::develop(&img, &|step| eprintln!("{step}")).map_err(|e| anyhow!(e))?;

    let (w, h) = (dev.frame.width, dev.frame.height);
    let mut bytes = format!("P6\n{w} {h}\n65535\n").into_bytes();
    bytes.reserve(dev.data.len() * 2);
    for v in &dev.data {
        bytes.extend_from_slice(&v.to_be_bytes());
    }
    std::fs::File::create(out)
        .and_then(|mut f| f.write_all(&bytes))
        .with_context(|| format!("writing {}", out.display()))?;
    println!(
        "{}",
        serde_json::json!({
            "x": dev.frame.x,
            "y": dev.frame.y,
            "width": w,
            "height": h,
            "balance": dev.balance,
            "balance_from_file": dev.balance_from_file,
            "alpha": dev.prior.alpha,
            "alpha_c": dev.prior.alpha_c,
            "chroma": dev.prior.chroma,
            "noise": dev.prior.noise,
            "lens": dev.prior.lens,
            "burnt": dev.burnt,
        })
    );
    Ok(())
}
