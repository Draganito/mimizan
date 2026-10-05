//! Build the vendored librtprocess and the thin C wrapper around it.
//!
//! librtprocess is GPL-3.0-or-later. This crate links it. The rest of
//! Mimizan Lab is under the same license.

use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/librtprocess");
    println!("cargo:rerun-if-changed={}", root.display());
    println!("cargo:rerun-if-changed=src/wrap.cpp");

    let dst = cmake::Config::new(&root)
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("OPTION_OMP", "ON")
        .profile("Release")
        .build();

    let libdir = ["lib", "lib64"]
        .into_iter()
        .map(|d| dst.join(d))
        .find(|d| d.join("librtprocess.a").exists())
        .unwrap_or_else(|| panic!("librtprocess.a not installed under {}", dst.display()));
    let header = dst.join("include/rtprocess");
    if !header.join("librtprocess.h").exists() {
        panic!("librtprocess.h not installed under {}", header.display());
    }

    cc::Build::new()
        .cpp(true)
        .file("src/wrap.cpp")
        .include(&header)
        .flag_if_supported("-std=c++17")
        .compile("mimizan_rtwrap");

    println!("cargo:rustc-link-search=native={}", libdir.display());
    println!("cargo:rustc-link-lib=static=rtprocess");
    println!("cargo:rustc-link-lib=gomp");
    println!("cargo:rustc-link-lib=stdc++");
}
