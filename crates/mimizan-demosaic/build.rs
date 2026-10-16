//! Build the vendored librtprocess and the thin C wrapper around it.
//!
//! librtprocess is GPL-3.0-or-later. This crate links it. The rest of
//! Mimizan Lab is under the same license.
//!
//! MSVC has no `<x86intrin.h>` and does not define `__SSE2__`. Apple silicon
//! is the same: no SSE, and no libgomp on the system. The vendored sources
//! then take their scalar paths. The default Mimizan negative does not
//! demosaic; AMaZE, RCD, DCB and LMMSE still run, without the SSE kernels.

use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/librtprocess");
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed=src/wrap.cpp");

    // MSVC's OpenMP rejects the vendored `simd` and `reduction(min/max)`
    // pragmas unless a different runtime is selected. Apple Clang has no
    // OpenMP unless a separate libomp is installed, and that would be a
    // dependency the finished program must not have. The demosaicers still
    // run; the parallel loops fall back to one thread. Linux keeps OpenMP.
    let openmp = if cfg!(target_env = "msvc") || cfg!(target_os = "macos") { "OFF" } else { "ON" };
    let dst = cmake::Config::new(&root)
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("OPTION_OMP", openmp)
        .profile("Release")
        .build();

    let lib_file = if cfg!(target_env = "msvc") { "rtprocess.lib" } else { "librtprocess.a" };
    let libdir = find_lib_dir(&dst, lib_file)
        .unwrap_or_else(|| panic!("{lib_file} not installed under {}", dst.display()));
    let header = dst.join("include/rtprocess");
    if !header.join("librtprocess.h").exists() {
        panic!("librtprocess.h not installed under {}", header.display());
    }

    cc::Build::new().cpp(true).file("src/wrap.cpp").include(&header).std("c++17").compile("mimizan_rtwrap");

    println!("cargo:rustc-link-search=native={}", libdir.display());
    println!("cargo:rustc-link-lib=static=rtprocess");
    if cfg!(target_os = "macos") {
        // libc++, the C++ library that ships with macOS. Not libstdc++.
        println!("cargo:rustc-link-lib=c++");
    } else if cfg!(not(target_env = "msvc")) {
        println!("cargo:rustc-link-lib=gomp");
        println!("cargo:rustc-link-lib=stdc++");
    }
}

fn find_lib_dir(prefix: &Path, lib_file: &str) -> Option<PathBuf> {
    ["lib", "lib64"].into_iter().map(|dir| prefix.join(dir)).find(|dir| dir.join(lib_file).exists())
}
