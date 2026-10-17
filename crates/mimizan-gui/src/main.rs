//! Mimizan Lab desktop: open a RAW, judge the luminance estimate interactively,
//! export the negative or a print. The program is GPL-3.0-or-later. The
//! separation lives in `mimizan-core`.

#![forbid(unsafe_code)]

mod app;
mod browser;
mod edit;
mod histogram;
mod session;
mod worker;
mod zone_bar;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let initial = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    // `MIMIZAN_WINDOW=1600x1000` overrides the initial size (unattended
    // screenshots).
    let size = std::env::var("MIMIZAN_WINDOW")
        .ok()
        .and_then(|s| {
            let (w, h) = s.split_once('x')?;
            Some([w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?])
        })
        .unwrap_or([1360.0, 860.0]);
    let options = eframe::NativeOptions {
        // Fits a 1366x768 laptop at scale 1 and an HiDPI screen at scale 2;
        // the panels float, scroll and can be moved, Ctrl+/- zooms the whole UI.
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(size)
            .with_min_inner_size([820.0, 560.0])
            .with_title("Mimizan Lab"),
        ..Default::default()
    };
    eframe::run_native("Mimizan Lab", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, initial)))))
}
