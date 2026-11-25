//! Single-file launcher for Plumbline: unpacks the app, PDFium and the licence notices once into
//! %LOCALAPPDATA%\Plumbline\app\<payload id>\ and starts the app there with the same arguments.
//! It writes what it does to the app's log (%LOCALAPPDATA%\Plumbline\logs\plumbline.log) and,
//! when the app quits with an error within a few seconds, says so in a message box.
#![windows_subsystem = "windows"]

use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long the launcher watches the app for an early failure.
const WATCH: Duration = Duration::from_secs(6);
/// Exit code of plumbline.exe after it has shown its own message (pl_app::startup::EXIT_REPORTED).
const EXIT_REPORTED: i32 = 3;
/// The log moves to plumbline.log.1 at this size (as in pl_app::startup).
const LOG_LIMIT: u64 = 1024 * 1024;

const FILES: [(&str, &[u8]); 3] = [
    ("plumbline.exe", include_bytes!("../payload/plumbline.exe")),
    ("pdfium.dll", include_bytes!("../payload/pdfium.dll")),
    ("THIRD-PARTY-LICENSES.txt", include_bytes!("../payload/THIRD-PARTY-LICENSES.txt")),
];

fn base_dir() -> PathBuf {
    // An empty or relative LOCALAPPDATA counts as unset, as in the app.
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(env::temp_dir);
    base.join("Plumbline")
}

fn install_dir() -> PathBuf {
    base_dir().join("app").join(env!("PAYLOAD_ID"))
}

fn log_path() -> PathBuf {
    base_dir().join("logs").join("plumbline.log")
}

/// `YYYY-MM-DD hh:mm:ssZ` (UTC), as the app writes it.
fn timestamp() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let z = (secs / 86_400) as i64 + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let rem = secs % 86_400;
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

fn try_log(path: &Path, message: &str) -> std::io::Result<()> {
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    if fs::metadata(path).map(|m| m.len() >= LOG_LIMIT).unwrap_or(false) {
        let old = path.with_extension("log.1");
        let _ = fs::remove_file(&old);
        fs::rename(path, &old)?;
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{} launcher[{}] {message}", timestamp(), std::process::id())
}

/// Whether the last [`log`] call could not write to the file.
static LOG_FAILED: AtomicBool = AtomicBool::new(false);

/// Appends one line to the app's log; failures are only remembered.
fn log(message: &str) {
    LOG_FAILED.store(try_log(&log_path(), message).is_err(), Ordering::Relaxed);
}

/// "Details are in <log>", or that the log could not be written there.
fn details(may: bool) -> String {
    if LOG_FAILED.load(Ordering::Relaxed) {
        format!("The log could not be written to {}", log_path().display())
    } else if may {
        format!("Details may be in {}", log_path().display())
    } else {
        format!("Details are in {}", log_path().display())
    }
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

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut core::ffi::c_void;
    fn CloseHandle(handle: *mut core::ffi::c_void) -> i32;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: *mut core::ffi::c_void, access: u32, token: *mut *mut core::ffi::c_void) -> i32;
    fn GetTokenInformation(
        token: *mut core::ffi::c_void,
        class: u32,
        info: *mut core::ffi::c_void,
        len: u32,
        returned: *mut u32,
    ) -> i32;
}

/// Whether the launcher runs with an elevated (administrator) token, for the log.
fn elevation() -> &'static str {
    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_ELEVATION: u32 = 20;
    let mut token = core::ptr::null_mut();
    let (mut elevated, mut len) = (0u32, 0u32);
    // SAFETY: plain Win32 calls with valid out-pointers; the token is closed after use.
    let ok = unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) != 0 && {
            let ok = GetTokenInformation(token, TOKEN_ELEVATION, (&mut elevated as *mut u32).cast(), 4, &mut len) != 0;
            CloseHandle(token);
            ok
        }
    };
    match (ok, elevated != 0) {
        (false, _) => "elevation unknown",
        (true, true) => "elevated (run as administrator)",
        (true, false) => "not elevated",
    }
}

fn fail(msg: &str) -> ! {
    log(&format!("fatal: {}", msg.replace('\n', " ")));
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    // MB_ICONERROR | MB_SETFOREGROUND
    unsafe { MessageBoxW(core::ptr::null_mut(), w(msg).as_ptr(), w("Plumbline").as_ptr(), 0x10 | 0x0001_0000) };
    std::process::exit(1);
}

fn main() {
    log(&format!(
        "Plumbline launcher {} starting (payload {}); {}",
        env!("CARGO_PKG_VERSION"),
        env!("PAYLOAD_ID"),
        elevation()
    ));
    let dir = install_dir();
    if let Err(e) = unpack(&dir) {
        fail(&format!("Plumbline could not unpack into\n{}\n\n{e}", dir.display()));
    }
    let args: Vec<_> = env::args_os().skip(1).collect();
    let mut child = match Command::new(dir.join("plumbline.exe")).args(args).spawn() {
        Ok(child) => child,
        Err(e) => fail(&format!("Plumbline could not start.\n\n{e}\n\n{}", details(false))),
    };
    // Watch the app for a few seconds: a crash or a failed start right away would otherwise look
    // like nothing happening. Exit code 0 means the user closed it; EXIT_REPORTED means the app
    // has already shown its own message. Either way, say nothing.
    let started = Instant::now();
    while started.elapsed() < WATCH {
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code().unwrap_or(-1);
                let ms = started.elapsed().as_millis();
                log(&format!("the app exited after {ms} ms with code {code} ({:#010x})", code as u32));
                if code != 0 && code != EXIT_REPORTED {
                    fail(&format!(
                        "Plumbline closed right after starting (exit code {code}, {:#010x}).\n\n\
                         {}\n\n\
                         Try starting it with --renderer glow (see Troubleshooting in the README), \
                         and send that log file if it keeps happening.",
                        code as u32,
                        details(true)
                    ));
                }
                return;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => {
                log(&format!("cannot watch the app: {e}"));
                return;
            }
        }
    }
    log("the app is still running after the watch; the launcher exits");
}
