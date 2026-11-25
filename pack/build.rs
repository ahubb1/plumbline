// Gives the payload a content id so each build unpacks into its own folder, and embeds the
// application manifest res/windows/gui.manifest (asInvoker: no UAC prompt) in Plumbline.exe.
use std::{env, fs, path::Path};
fn main() {
    let root = Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).to_path_buf();
    let manifest = root.join("../res/windows/gui.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }
    let dir = root.join("payload");
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for name in ["plumbline.exe", "pdfium.dll", "THIRD-PARTY-LICENSES.txt"] {
        let p = dir.join(name);
        println!("cargo:rerun-if-changed={}", p.display());
        for b in fs::read(&p).expect("payload file missing") {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=PAYLOAD_ID={h:016x}");
}
