//! Copies the vendored `pdfium.dll` next to the built executables (`target/<profile>/`), so
//! `plumbline.exe` finds it beside itself, which is the only place the shipped app looks.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dll = manifest.join("../../vendor/pdfium/8125/bin/pdfium.dll");
    println!("cargo:rerun-if-changed={}", dll.display());
    if !dll.is_file() {
        println!(
            "cargo:warning=pdfium.dll is not vendored; run `cargo run -p xtask -- fetch-pdfium`"
        );
        return;
    }
    // OUT_DIR is somewhere under target/<profile>/build/ (how deep depends on the cargo version);
    // the executables land in target/<profile>/.
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("out dir"));
    let Some(profile_dir) = out
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))
        .and_then(Path::parent)
    else {
        println!("cargo:warning=could not find the target directory from OUT_DIR");
        return;
    };
    let dest = profile_dir.join("pdfium.dll");
    if same_file(&dll, &dest) {
        return;
    }
    if let Err(e) = std::fs::copy(&dll, &dest) {
        // A running program may hold the old copy open; that copy is still usable.
        if !dest.is_file() {
            println!(
                "cargo:warning=could not copy pdfium.dll to {}: {e}",
                dest.display()
            );
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => {
            ma.len() == mb.len()
                && matches!((ma.modified(), mb.modified()), (Ok(ta), Ok(tb)) if tb >= ta)
        }
        _ => false,
    }
}
