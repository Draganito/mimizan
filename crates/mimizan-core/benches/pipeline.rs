//! Criterion benches for the hot stages (SPEC section 12). Run with
//! `cargo bench -p mimizan-core`; the figures in docs/RESULTS.md come from here
//! and from `mimizan negative` on the real samples.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use mimizan_core::blockfft::BlockFft;
use mimizan_core::cfa::BayerPhase;
use mimizan_core::filter::{convolve_cols_signed, convolve_rows_mod, lowpass, LowpassSpec};
use mimizan_core::mask::compute_mask;
use mimizan_core::plane::Plane;
use mimizan_core::resize::resize;
use mimizan_core::separate::{chroma_estimates, separate, MaskMode, SeparateParams};
use mimizan_core::synth::{mosaic, Scene, SynthParams};
use std::hint::black_box;

fn plane(w: usize, h: usize) -> Plane {
    let mut p = Plane::zeros(w, h);
    for y in 0..h {
        for x in 0..w {
            p.set(x, y, ((x * 7 + y * 13) % 97) as f64 / 97.0);
        }
    }
    p
}

fn bench_filter(c: &mut Criterion) {
    let k = LowpassSpec::default().kernel();
    let mut g = c.benchmark_group("filter");
    for &(w, h) in &[(1024usize, 1024usize), (3000, 2000)] {
        let p = plane(w, h);
        g.throughput(Throughput::Elements((w * h) as u64));
        g.bench_with_input(BenchmarkId::new("rows_73", format!("{w}x{h}")), &p, |b, p| {
            b.iter(|| convolve_rows_mod(black_box(p), &k, |x, _| if x & 1 == 0 { 1.0 } else { -1.0 }))
        });
        g.bench_with_input(BenchmarkId::new("cols_73", format!("{w}x{h}")), &p, |b, p| {
            b.iter(|| convolve_cols_signed(black_box(p), &k, |y| if y & 1 == 0 { 1.0 } else { -1.0 }))
        });
        g.bench_with_input(BenchmarkId::new("lowpass_73", format!("{w}x{h}")), &p, |b, p| {
            b.iter(|| lowpass(black_box(p), &k))
        });
        g.bench_with_input(BenchmarkId::new("chroma_estimates", format!("{w}x{h}")), &p, |b, p| {
            b.iter(|| chroma_estimates(black_box(p), BayerPhase::RGGB, &k))
        });
    }
    g.finish();
}

fn bench_separate(c: &mut Criterion) {
    let mut g = c.benchmark_group("separate");
    g.sample_size(10);
    for &size in &[1024usize, 2048] {
        let s = mosaic(Scene::Patches, size, size, &SynthParams::default());
        g.throughput(Throughput::Elements((size * size) as u64));
        let fixed = SeparateParams { mask: MaskMode::Off, ..Default::default() };
        let adaptive = SeparateParams::default();
        g.bench_with_input(BenchmarkId::new("fixed", size), &s.mosaic, |b, m| {
            b.iter(|| separate(black_box(m), &fixed))
        });
        g.bench_with_input(BenchmarkId::new("adaptive", size), &s.mosaic, |b, m| {
            b.iter(|| separate(black_box(m), &adaptive))
        });
        g.bench_with_input(BenchmarkId::new("mask_only", size), &s.mosaic, |b, m| {
            b.iter(|| compute_mask(black_box(m)))
        });
    }
    g.finish();
}

fn bench_blockfft(c: &mut Criterion) {
    let bf = BlockFft::new(64);
    let p = plane(256, 256);
    let mut scratch = Vec::new();
    c.bench_function("blockfft/power_64", |b| b.iter(|| bf.power(black_box(&p), 64, 64, &mut scratch)));
}

fn bench_resize(c: &mut Criterion) {
    let p = plane(3000, 2000);
    let mut g = c.benchmark_group("resize");
    g.sample_size(10);
    g.throughput(Throughput::Elements((3000 * 2000) as u64));
    g.bench_function("lanczos3_down_0.6", |b| b.iter(|| resize(black_box(&p), 1800, 1200)));
    g.bench_function("lanczos3_up_1.5", |b| b.iter(|| resize(black_box(&p), 4500, 3000)));
    g.finish();
}

criterion_group!(benches, bench_filter, bench_separate, bench_blockfft, bench_resize);
criterion_main!(benches);
