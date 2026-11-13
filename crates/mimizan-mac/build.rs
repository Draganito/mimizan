//! The Mac App Store build's purchase is Swift (`swift/store.swift`):
//! StoreKit 2 has no other door. With the `store` feature on a Mac this
//! compiles it to a static library and names what it links. Without the
//! feature, and on every other system, nothing happens here.

use std::path::{Path, PathBuf};
use std::process::Command;

/// StoreKit 2 begins with macOS 12; the store package asks for it too.
const LEAST_MACOS: (u32, u32) = (12, 0);

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=swift/store.swift");
    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    let store = std::env::var_os("CARGO_FEATURE_STORE").is_some();
    let for_mac = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    if !(store && for_mac) {
        return;
    }
    if !cfg!(target_os = "macos") {
        // A check from another system has no Swift compiler. It does not
        // link either, so the Rust side is still checked.
        println!("cargo:warning=the store's Swift is compiled on a Mac alone; this build cannot be linked");
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let source =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).join("swift/store.swift");
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        other => panic!("no Mac of the architecture {other:?}"),
    };
    let (major, minor) = deployment_target();
    let library = out.join("libmimizan_store.a");
    let status = Command::new("xcrun")
        .args(["swiftc", "-parse-as-library", "-emit-library", "-static", "-O", "-swift-version", "5"])
        .args(["-module-name", "MimizanStore"])
        .args(["-target", &format!("{arch}-apple-macos{major}.{minor}")])
        .arg("-o")
        .arg(&library)
        .arg(&source)
        .status()
        .expect("xcrun swiftc: the Xcode command line tools are needed for the store build");
    assert!(status.success(), "swiftc failed on {}", source.display());

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=mimizan_store");
    // The Swift runtime is part of macOS. The SDK's stubs come first, so
    // the libraries are linked by their place in the system; the
    // toolchain's folder holds the small compatibility archives the
    // compiler asks for by name.
    let sdk = xcrun(&["--sdk", "macosx", "--show-sdk-path"]);
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    let swiftc = xcrun(&["--find", "swiftc"]);
    if let Some(toolchain) = Path::new(&swiftc).parent().and_then(Path::parent) {
        println!("cargo:rustc-link-search=native={}", toolchain.join("lib/swift/macosx").display());
    }
    println!("cargo:rustc-link-lib=framework=StoreKit");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

/// The system the Swift is compiled for: the build's own target when it
/// names a later one, never one before StoreKit 2.
fn deployment_target() -> (u32, u32) {
    let asked = std::env::var("MACOSX_DEPLOYMENT_TARGET").ok().and_then(|v| {
        let mut parts = v.trim().split('.');
        let major = parts.next()?.parse::<u32>().ok()?;
        let minor = parts.next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
        Some((major, minor))
    });
    asked.filter(|t| *t >= LEAST_MACOS).unwrap_or(LEAST_MACOS)
}

fn xcrun(args: &[&str]) -> String {
    let output = Command::new("xcrun").args(args).output().expect("xcrun");
    assert!(output.status.success(), "xcrun {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
