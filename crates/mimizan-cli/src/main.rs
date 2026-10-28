//! `mimizan`: the command-line front end of Mimizan Lab.

mod calibrate;
mod cmd;
mod kodak;
mod measure;
mod plate_bench;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

// Full-resolution f64 planes are ~200 MB each; glibc hands such blocks straight
// back to the kernel on free, so every new plane pays its page faults again.
// mimalloc keeps and reuses them.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(
    name = "mimizan",
    version,
    about = "Bayer RAW to a linear monochrome negative or a three-colour plate; no demosaic"
)]
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
        /// Replace defective photosites (far outside their same-colour neighbours) by the
        /// neighbour median. Off by default: every photosite stays as measured, the report
        /// counts the defects
        #[arg(long)]
        repair_defects: bool,
        /// Also write a gamma-encoded PNG quick look, downscaled by --preview-scale
        #[arg(long)]
        preview: Option<PathBuf>,
        /// Reduction factor of the quick look
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
        /// Chrominance cut-off, c/px (adaptive and off only; dubois has its own bands)
        #[arg(long, default_value_t = 0.15)]
        cutoff: f64,
        /// Cross-term refinement rounds, 1 = single pass (adaptive only)
        #[arg(long, default_value_t = 1)]
        rounds: usize,
        /// Replace defective photosites (far outside their same-colour neighbours) by the
        /// neighbour median. Off by default: every photosite stays as measured, the report
        /// counts the defects
        #[arg(long)]
        repair_defects: bool,
        /// Also write a gamma-encoded PNG quick look, downscaled by --preview-scale
        #[arg(long)]
        preview: Option<PathBuf>,
        /// Reduction factor of the quick look
        #[arg(long, default_value_t = 8)]
        preview_scale: usize,
        /// Write a 1:1 crop PNG "x,y,w,h" next to the preview (name + .crop.png)
        #[arg(long)]
        crop: Option<String>,
        /// Three-colour plate instead of one negative (Prokudin-Gorsky):
        /// the three taking filters are camera (the file's colour matrix,
        /// default) | wratten (25/58/47 in front of the panchromatic emulsion: the camera file's response, or the native) |
        /// channels (the balanced channels as they are). The file holds the
        /// three frames stacked, blue, green, red; `print` makes it a colour print.
        #[arg(long, value_name = "FILTERS", num_args = 0..=1, default_missing_value = "camera")]
        plate: Option<String>,
        /// Primaries the plate is projected through: adobe-rgb (default) | srgb
        #[arg(long, value_name = "SPACE", default_value = "adobe-rgb")]
        plate_space: String,
        /// Recipe JSON as the desktop writes it next to the RAW: its mix and
        /// its plate (process, filters, primaries) develop this negative.
        /// --weights and --plate override them.
        #[arg(long)]
        recipe: Option<PathBuf>,
        /// Linear DNG instead of the negative TIFF: the three balanced
        /// channels of the separation with the white balance undone, three
        /// samples a pixel (LinearRaw), the camera's colour matrix and
        /// as-shot neutral in the tags. A RAW converter opens it as a RAW of
        /// the camera and skips its own demosaicing. Bayer sensors only; the
        /// recipe's crop is applied; no mask sidecar.
        #[arg(long)]
        dng: bool,
    },
    /// Negative TIFF -> print file in one pass: size, place, paper or look, border,
    /// [Richardson-Lucy,] USM; 16-bit TIFF (gamma 2.2 grey ICC). Every print
    /// decision can come from a recipe file (--recipe); explicit flags override its fields.
    Print {
        negative: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Recipe JSON (as written by --write-recipe or the GUI); flags override
        #[arg(long)]
        recipe: Option<PathBuf>,
        /// Write the recipe this run used (after overrides) to a JSON file
        #[arg(long)]
        write_recipe: Option<PathBuf>,
        /// Look file (look/*.json); default: neutral
        #[arg(long)]
        look: Option<PathBuf>,
        /// Print on paper: split-grade exposures "SOFT,HARD" in stops of time
        /// from the base exposure (e.g. "0,0" for the base print, "0,-0.5"
        /// for half a stop less hard light). Replaces --look.
        #[arg(long, value_name = "SOFT,HARD", allow_hyphen_values = true)]
        paper: Option<String>,
        /// Placing in stops before the curve: L * 2^place (0 = untouched)
        #[arg(long, value_name = "STOPS", allow_hyphen_values = true)]
        place: Option<f64>,
        /// Paper size "WxH<cm|mm|in>", e.g. 30x40cm (image fitted, box rotates with it)
        #[arg(long)]
        size: Option<String>,
        /// Resampling kernel: b-spline (default, no overshoot) | area (alias
        /// box: mean of the covered pixels) | lanczos3 | catmull-rom | mitchell
        #[arg(long, value_name = "KERNEL")]
        resample: Option<String>,
        /// Paper border around the picture, mm, filled with paper white
        #[arg(long, value_name = "MM")]
        border_mm: Option<f64>,
        /// Black film edge around the picture, mm, inside the white border
        /// when there is one; either ring stands on its own.
        #[arg(long, value_name = "MM")]
        black_mm: Option<f64>,
        /// Output resolution, dots per inch (default from the recipe, 360)
        #[arg(long)]
        dpi: Option<f64>,
        /// Scale compensation: USM amount of the enlargement blur (0 = off).
        /// Native size and reductions stay. Default from the camera file, else 0.
        #[arg(long)]
        usm_amount: Option<f64>,
        /// Recorded arc minute: r_px = usm_k * distance_mm * dpi. Not applied.
        #[arg(long, default_value_t = mimizan_core::usm::USM_K)]
        usm_k: f64,
        /// Viewing distance in mm recorded with the print; default: the diagonal
        #[arg(long)]
        distance_mm: Option<f64>,
        /// Ignore the .mask.tif sidecar (USM everywhere)
        #[arg(long)]
        no_mask: bool,
        /// Scale compensation: Richardson–Lucy passes (1..10) of that blur,
        /// before the USM. Native size and reductions stay.
        #[arg(long, value_name = "PASSES")]
        deconv: Option<u32>,
        /// Directory with cameras/*.json for the USM default
        #[arg(long, default_value = "cameras")]
        cameras: PathBuf,
        /// Plate only: the projector's lamps, stops added to the red and blue
        /// positive ("R,B"; green is the reference), within ±2
        #[arg(long, value_name = "R,B", allow_hyphen_values = true)]
        lamp: Option<String>,
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
    /// The same mosaics in colour: the three-colour plate vs the demosaicers' RGB (CPSNR)
    Plate(plate_bench::PlateBenchArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let filter = tracing_subscriber::EnvFilter::try_new(&cli.log)
        .with_context(|| format!("--log '{}' is not a valid filter", cli.log))?;
    tracing_subscriber::fmt().with_env_filter(filter).with_target(false).with_writer(std::io::stderr).init();
    match cli.cmd {
        Cmd::Info { raw, json } => cmd::info(&raw, json),
        Cmd::Dump { raw, out, repair_defects, preview, preview_scale } => {
            cmd::dump(&raw, &out, repair_defects, preview.as_deref(), preview_scale)
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
            repair_defects,
            preview,
            preview_scale,
            crop,
            plate,
            plate_space,
            recipe,
            dng,
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
            repair_defects,
            preview,
            preview_scale,
            crop,
            plate,
            plate_space,
            recipe,
            dng,
        }),
        Cmd::Print {
            negative,
            out,
            recipe,
            write_recipe,
            look,
            paper,
            place,
            size,
            resample,
            border_mm,
            black_mm,
            dpi,
            usm_amount,
            usm_k,
            distance_mm,
            no_mask,
            deconv,
            cameras,
            lamp,
        } => cmd::print(&cmd::PrintArgs {
            negative,
            out,
            recipe,
            write_recipe,
            look,
            paper,
            place,
            size,
            resample,
            border_mm,
            black_mm,
            dpi,
            usm_amount,
            usm_k,
            distance_mm,
            use_mask: !no_mask,
            deconv,
            cameras,
            lamp,
        }),
        Cmd::Synth(a) => measure::synth(&a),
        Cmd::Measure { cmd } => measure::measure(&cmd),
        Cmd::Bench { cmd: BenchCmd::Kodak(a) } => kodak::kodak(&a),
        Cmd::Bench { cmd: BenchCmd::Plate(a) } => plate_bench::plate_bench(&a),
        Cmd::Calibrate { cmd } => calibrate::calibrate(&cmd),
    }
}
