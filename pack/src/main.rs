//! Single-file launcher for Plumbline: unpacks the app, PDFium and the licence notices once into
//! %LOCALAPPDATA%\Plumbline\app\<payload id>\ and starts the app there with the same arguments.
#![windows_subsystem = "windows"]

use std::{env, fs, io::Write, path::{Path, PathBuf}, process::Command};

const FILES: [(&str, &[u8]); 3] = [
    ("plumbline.exe", include_bytes!("../payload/plumbline.exe")),
    ("pdfium.dll", include_bytes!("../payload/pdfium.dll")),
    ("THIRD-PARTY-LICENSES.txt", include_bytes!("../payload/THIRD-PARTY-LICENSES.txt")),
];

fn install_dir() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(env::temp_dir);
    base.join("Plumbline").join("app").join(env!("PAYLOAD_ID"))
}

fn unpack(dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    for (name, bytes) in FILES {
        let dest = dir.join(name);
        if fs::metadata(&dest).map(|m| m.len() == bytes.len() as u64).unwrap_or(false) {
            continue;
        }
        let tmp = dir.join(format!("{name}.part"));
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        if dest.exists() {
            let _ = fs::remove_file(&dest);
        }
        fs::rename(&tmp, &dest)?;
    }
    // Remove folders left by older builds; ignore ones still in use.
    if let Some(parent) = dir.parent() {
        if let Ok(entries) = fs::read_dir(parent) {
            for e in entries.flatten() {
                if e.path() != dir {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
    }
    Ok(())
}

#[link(name = "user32")]
extern "system" {
    fn MessageBoxW(hwnd: *mut core::ffi::c_void, text: *const u16, caption: *const u16, utype: u32) -> i32;
}

fn fail(msg: &str) -> ! {
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    unsafe { MessageBoxW(core::ptr::null_mut(), w(msg).as_ptr(), w("Plumbline").as_ptr(), 0x10) };
    std::process::exit(1);
}

fn main() {
    let dir = install_dir();
    if let Err(e) = unpack(&dir) {
        fail(&format!("Plumbline could not unpack into\n{}\n\n{e}", dir.display()));
    }
    let args: Vec<_> = env::args_os().skip(1).collect();
    if let Err(e) = Command::new(dir.join("plumbline.exe")).args(args).spawn() {
        fail(&format!("Plumbline could not start.\n\n{e}"));
    }
}
