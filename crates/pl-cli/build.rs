//! Embeds the application manifest `res/windows/cli.manifest` in `plumb.exe`.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    embed_manifest(&manifest.join("../../res/windows/cli.manifest"));
}

/// Has the MSVC linker embed `file` as the application manifest of this crate's executables
/// (bins only: test executables are left alone). `cargo run -p xtask -- dist` checks the result.
fn embed_manifest(file: &Path) {
    println!("cargo:rerun-if-changed={}", file.display());
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if os != "windows" || env != "msvc" {
        return;
    }
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}",
        file.display()
    );
}
