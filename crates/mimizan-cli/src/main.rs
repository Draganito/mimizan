mod calibrate;
mod cmd;
mod kodak;
mod measure;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

// Full-resolution f64 planes are ~200 MB each; glibc hands such blocks straight
// back to the kernel on free, so every new plane pays its page faults again.
// mimalloc keeps and reuses them.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "mimizan", version, about = "Bayer RAW to linear monochrome negative, no demosaic")]
struct Cli {
    /// Log level filter (e.g. info, debug, mimizan_core=trace)
    #[arg(long, global = true, default_value = "info")]
    log: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print levels, phase, crop and camera of a RAW file
    Info {
        raw: PathBuf,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Write the normalised mosaic as 16-bit linear TIFF (no separation)
    Dump {
        raw: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        #[arg(long)]
        no_defects: bool,
        /// Also write a gamma-encoded PNG quick look, downscaled by --preview-scale
        #[arg(long)]
        preview: Option<PathBuf>,
        #[arg(long, default_value_t = 8)]
        preview_scale: usize,
    },
    /// RAW -> linear 16-bit monochrome negative (+ .mask.tif sidecar)
    Negative {
        raw: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Directory with cameras/*.json (auto-picked by make/model)
        #[arg(long, default_value = "cameras")]
        cameras: PathBuf,
        /// Explicit camera file (overrides auto-pick)
        #[arg(long)]
        camera: Option<PathBuf>,
        /// Separation mode: dubois (pixel-adaptive, default) | adaptive (block mask) | off (fixed)
        #[arg(long, default_value = "dubois")]
        mask: String,
        /// Override weights "r,g,b" (sum 1)
        #[arg(long)]
        weights: Option<String>,
        /// Channels --weights refers to: balanced (after --wb) | raw (sensor channels, illuminant-independent)
        #[arg(long, default_value = "balanced")]
        weights_space: String,
        /// Contrast filter on the weights (approximate): yellow-8 | yellow-green-11 | orange-16 | red-25 | green-58 | blue-47 (or the number)
        #[arg(long)]
        filter: Option<String>,
        /// Channel balance before separation: as-shot | gray | none | "r,g,b"
        #[arg(long, default_value = "as-shot")]
        wb: String,
        #[arg(long, default_value_t = 0.15)]
        cutoff: f64,
        /// Cross-term refinement rounds (1 = single pass)
        #[arg(long, default_value_t = 1)]
        rounds: usize,
        /// Nonlinear reconstruction of the last octave: where luminance and
        /// chrominance overlap, pixels are computed from a per-pixel direction
        /// decision instead of filtered. Missing pixels are then calculated,
        /// not measured; the sidecar records where.
        #[arg(long)]
        reconstruct: bool,
        #[arg(long)]
        no_defects: bool,
        #[arg(long)]
        preview: Option<PathBuf>,
        #[arg(long, default_value_t = 8)]
        preview_scale: usize,
        /// Write a 1:1 crop PNG "x,y,w,h" next to the preview (name + .crop.png)
        #[arg(long)]
        crop: Option<String>,
    },
    /// Negative TIFF -> print file: size, look curve, USM or screen compensation,
    /// 16-bit TIFF (gamma 2.2 grey ICC) or JPEG (-o *.jpg)
    Print {
        negative: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Look file (look/*.json); default: neutral
        #[arg(long)]
        look: Option<PathBuf>,
        /// Paper size "WxH<cm|mm|in>", e.g. 30x40cm (image fitted, box rotates with it),
        /// or long edge in pixels for the screen, e.g. 2048px
        #[arg(long)]
        size: Option<String>,
        /// Screen output: replace USM by the exact compensation of the known
        /// losses (Lanczos-3 resampling x display pixel aperture); value = amount 0..1
        #[arg(long, value_name = "AMOUNT")]
        screen: Option<f64>,
        /// Gain cap of the screen compensation near Nyquist (Wiener regularisation)
        #[arg(long, default_value_t = 2.0)]
        screen_max_gain: f64,
        #[arg(long, default_value_t = 300.0)]
        dpi: f64,
        /// USM amount (0 = off); default from the camera file named in the negative, else 0
        #[arg(long)]
        usm_amount: Option<f64>,
        /// USM radius constant: r_px = usm_k * distance_mm * dpi
        #[arg(long, default_value_t = mimizan_core::usm::USM_K)]
        usm_k: f64,
        /// Viewing distance in mm; default: print diagonal
        #[arg(long)]
        distance_mm: Option<f64>,
        /// Ignore the .mask.tif sidecar (USM everywhere)
        #[arg(long)]
        no_mask: bool,
        /// Directory with cameras/*.json for the USM default
        #[arg(long, default_value = "cameras")]
        cameras: PathBuf,
        /// Also write an 8-bit grey JPEG proof (full print size, quality 100, grey ICC)
        #[arg(long)]
        proof: Option<PathBuf>,
    },
    /// Generate a synthetic scene (mosaic + ground truth)
    Synth(measure::SynthArgs),
    /// Measurements: synthetic bench, star MTF, carrier energy, wedge linearity
    Measure {
        #[command(subcommand)]
        cmd: measure::MeasureCmd,
    },
    /// Benchmarks against external references
    Bench {
        #[command(subcommand)]
        cmd: BenchCmd,
    },
    /// Fits: look from a wedge, weights from colour patches, star MTF into the camera file
    Calibrate {
        #[command(subcommand)]
        cmd: calibrate::CalibrateCmd,
    },
}

#[derive(Subcommand)]
enum BenchCmd {
    /// Kodak pictures as fake Bayer mosaics: Mimizan vs libraw / RawTherapee demosaicers
    Kodak(kodak::KodakArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_new(&cli.log).unwrap_or_default())
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
    match cli.cmd {
        Cmd::Info { raw, json } => cmd::info(&raw, json),
        Cmd::Dump { raw, out, no_defects, preview, preview_scale } => {
            cmd::dump(&raw, &out, !no_defects, preview.as_deref(), preview_scale)
        }
        Cmd::Negative {
            raw,
            out,
            cameras,
            camera,
            mask,
            weights,
            weights_space,
            filter,
            wb,
            cutoff,
            rounds,
            reconstruct,
            no_defects,
            preview,
            preview_scale,
            crop,
        } => cmd::negative(&cmd::NegativeArgs {
            raw,
            out,
            cameras,
            camera,
            mask,
            weights,
            weights_space,
            filter,
            wb,
            cutoff,
            rounds,
            reconstruct,
            fix_defects: !no_defects,
            preview,
            preview_scale,
            crop,
        }),
        Cmd::Print {
            negative,
            out,
            look,
            size,
            screen,
            screen_max_gain,
            dpi,
            usm_amount,
            usm_k,
            distance_mm,
            no_mask,
            cameras,
            proof,
        } => cmd::print(&cmd::PrintArgs {
            negative,
            out,
            look,
            size,
            screen,
            screen_max_gain,
            dpi,
            usm_amount,
            usm_k,
            distance_mm,
            use_mask: !no_mask,
            cameras,
            proof,
        }),
        Cmd::Synth(a) => measure::synth(&a),
        Cmd::Measure { cmd } => measure::measure(&cmd),
        Cmd::Bench { cmd: BenchCmd::Kodak(a) } => kodak::kodak(&a),
        Cmd::Calibrate { cmd } => calibrate::calibrate(&cmd),
    }
}
