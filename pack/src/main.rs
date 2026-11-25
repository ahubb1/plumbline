//! Single-file launcher for Plumbline: unpacks the app, PDFium and the licence notices once into
//! %LOCALAPPDATA%\Plumbline\app\<payload id>\ and starts the app there with the same arguments.
//! It writes what it does to the app's log (%LOCALAPPDATA%\Plumbline\logs\plumbline.log).
//!
//! Unless the user chose a renderer (`--renderer`, or PLUMBLINE_RENDERER), it walks a ladder:
//! the renderer that worked last time (%LOCALAPPDATA%\Plumbline\renderer.txt), then the rest of
//! wgpu, glow, software. Each attempt starts the app with `--renderer <it> --ready-file <path>`
//! and waits (up to 30 s per attempt) for the app to create that file after its first painted
//! frame; past that the app is left running and the launcher exits. A failure before
//! that (a non-zero exit: a native crash in a graphics driver, or exit code 3) moves on to the
//! next renderer, one window at a time; exit code 0 means the user closed the window. If all
//! fail, one message box says so. With a renderer chosen by the user, it only watches the app
//! for a few seconds, as before.
#![cfg_attr(not(test), windows_subsystem = "windows")]

use std::{
    env,
    ffi::{OsStr, OsString},
    fs,
    io::Write,
    os::windows::io::AsRawHandle,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// How long the launcher watches the app for an early failure when the user chose the renderer.
const WATCH: Duration = Duration::from_secs(6);
/// How long the ladder waits for one renderer's first frame. It is per attempt, not for the
/// whole ladder: a renderer that hangs and then crashes must not leave the next one only a
/// moment. The app counts on it (`LAUNCHER_PATIENCE`, 25 s, in crates/pl-app/src/main.rs: until
/// then it leaves a failure to the launcher, after that it shows its own message).
const READY_WAIT: Duration = Duration::from_secs(30);
/// Ready files older than this, left by a launcher that was killed or by an app that painted
/// after its launcher had gone, are deleted when the launcher starts. Younger ones may belong to
/// another launcher that is still waiting.
const STALE_READY: Duration = Duration::from_secs(120);
/// Each wait for the app's exit lasts this long before the ready file is checked again.
const WAIT_SLICE_MS: u32 = 100;
/// File in [`base_dir`] holding the renderer that worked last time.
const REMEMBERED_FILE: &str = "renderer.txt";
/// Environment variable through which the user forces a renderer (as in pl_app::startup).
const RENDERER_ENV: &str = "PLUMBLINE_RENDERER";

/// A renderer the ladder can ask the app for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Renderer {
    Wgpu,
    Glow,
    Software,
}

/// The app's own order when nothing is remembered.
const ORDER: [Renderer; 3] = [Renderer::Wgpu, Renderer::Glow, Renderer::Software];

impl Renderer {
    /// The value of `--renderer`, and the contents of [`REMEMBERED_FILE`].
    fn arg(self) -> &'static str {
        match self {
            Renderer::Wgpu => "wgpu",
            Renderer::Glow => "glow",
            Renderer::Software => "software",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        ORDER.into_iter().find(|r| r.arg().eq_ignore_ascii_case(text))
    }
}

/// Whether the user chose the renderer: a `--renderer` option before any `--`, or a
/// non-empty PLUMBLINE_RENDERER. Then the ladder is skipped and the app does as told.
fn user_forced(args: &[OsString], env_renderer: Option<&OsStr>) -> bool {
    let flag = args
        .iter()
        .map(|a| a.to_string_lossy())
        .take_while(|a| a != "--")
        .any(|a| a == "--renderer" || a.starts_with("--renderer="));
    flag || env_renderer.is_some_and(|v| !v.is_empty())
}

/// The renderers to try, in order: the remembered one first, then the rest of [`ORDER`].
fn ladder(remembered: Option<Renderer>) -> Vec<Renderer> {
    remembered
        .into_iter()
        .chain(ORDER.into_iter().filter(|r| Some(*r) != remembered))
        .collect()
}

/// How one attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// The ready file appeared: the first frame is on screen.
    Ready,
    /// The app exited before that, with this code.
    Exited(i32),
    /// Still running without a first frame after [`READY_WAIT`].
    StillStarting,
}

/// What the launcher does after an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Then {
    /// Remember this renderer and exit, leaving the app running.
    Remember,
    /// The user closed the window: exit without retrying or saying anything.
    Stop,
    /// The renderer failed: try the next one.
    TryNext,
    /// Exit and leave the app running, remembering nothing.
    LeaveRunning,
}

/// Exit code 0 before the first frame is the user closing the window; anything else (a native
/// crash's exception code, or exit code 3 after a reported failure) is a failed start.
fn next_step(attempt: Attempt) -> Then {
    match attempt {
        Attempt::Ready => Then::Remember,
        Attempt::Exited(0) => Then::Stop,
        Attempt::Exited(_) => Then::TryNext,
        Attempt::StillStarting => Then::LeaveRunning,
    }
}

/// The one message after every renderer failed.
fn failure_message(failures: &[(Renderer, i32)], log: &str) -> String {
    let codes: Vec<String> = failures
        .iter()
        .map(|(r, code)| format!("{}: exit code {code} ({:#010x})", r.arg(), *code as u32))
        .collect();
    format!(
        "Plumbline could not start on this PC. It tried each of its ways of drawing the window, \
         and each one stopped before the window appeared.\n\n{}\n\n{log}\n\n\
         This usually means the graphics driver is out of date. Open Settings > Windows Update > \
         Advanced options > Optional updates, install any driver updates offered there (or the \
         latest driver from the PC maker's website), restart, and try again. If it still does \
         not start, send that log file.",
        codes.join("\n")
    )
}

/// Removes payload folders of older builds from `app_root` (`...\Plumbline\app`), keeping
/// `keep`. Only folders are removed, and only inside `app_root`: the remembered renderer and
/// the log live in its parent and are never touched. Folders still in use are skipped.
fn remove_old_payloads(app_root: &Path, keep: &Path) {
    if let Ok(entries) = fs::read_dir(app_root) {
        for e in entries.flatten() {
            if e.path() != keep && e.file_type().is_ok_and(|t| t.is_dir()) {
                let _ = fs::remove_dir_all(e.path());
            }
        }
    }
}
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
        remove_old_payloads(parent, dir);
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
    fn WaitForSingleObject(handle: *mut core::ffi::c_void, milliseconds: u32) -> u32;
}

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 0x102;

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

fn remembered_path() -> PathBuf {
    base_dir().join(REMEMBERED_FILE)
}

/// Whether `name` is one of the launcher's ready files (`ready-<...>.tmp`).
fn is_ready_file_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|n| n.starts_with("ready-") && n.ends_with(".tmp") && n.len() > 10)
}

/// Deletes ready files in `dir` last written at least `max_age` ago. Nothing else is touched.
fn remove_stale_ready_files(dir: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let old = e
            .metadata()
            .ok()
            .filter(|m| m.is_file())
            .and_then(|m| m.modified().ok())
            .is_some_and(|t| t.elapsed().map_or(true, |age| age >= max_age));
        if old && is_ready_file_name(&e.file_name()) {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// A ready-file path in `dir` that does not exist yet: named after this launcher, the attempt
/// and the time, so a file left by an earlier run can never pass for this attempt's.
fn fresh_ready_path(dir: &Path, attempt: usize) -> Option<PathBuf> {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    (0..8u32)
        .map(|n| dir.join(format!("ready-{}-{attempt}-{nanos}-{n}.tmp", std::process::id())))
        .find(|p| !p.exists())
}

/// Waits for `child` to create `ready` or to exit, at most [`READY_WAIT`], in short blocking
/// waits on the process handle (no busy loop).
fn wait_for_ready(child: &mut Child, ready: &Path) -> Attempt {
    let started = Instant::now();
    let mut wait_failed = false;
    loop {
        // SAFETY: the handle belongs to `child`, which outlives the call.
        let wait = unsafe { WaitForSingleObject(child.as_raw_handle().cast(), WAIT_SLICE_MS) };
        // A first frame counts even if the app has exited since (closed by the user at once).
        if ready.exists() {
            return Attempt::Ready;
        }
        if wait != WAIT_TIMEOUT {
            // Logged once; after that the loop polls with try_wait and a sleep instead.
            if wait != WAIT_OBJECT_0 && !std::mem::replace(&mut wait_failed, true) {
                log(&format!("cannot wait for the app (wait result {wait:#x}); polling instead"));
            }
            match child.try_wait() {
                Ok(Some(status)) => return Attempt::Exited(status.code().unwrap_or(-1)),
                Ok(None) => std::thread::sleep(Duration::from_millis(WAIT_SLICE_MS.into())),
                Err(e) => {
                    log(&format!("cannot watch the app: {e}"));
                    return Attempt::StillStarting;
                }
            }
        }
        if started.elapsed() >= READY_WAIT {
            return Attempt::StillStarting;
        }
    }
}

/// Tries the renderers of the ladder in turn; see the module comment.
fn run_ladder(exe: &Path, args: &[OsString]) {
    let remembered = fs::read_to_string(remembered_path()).ok().and_then(|t| Renderer::parse(&t));
    let order = ladder(remembered);
    log(&format!(
        "renderer ladder: {} (remembered: {})",
        order.iter().map(|r| r.arg()).collect::<Vec<_>>().join(", "),
        remembered.map_or("none", Renderer::arg)
    ));
    remove_stale_ready_files(&base_dir(), STALE_READY);
    let mut failures = Vec::new();
    for (i, renderer) in order.iter().copied().enumerate() {
        let Some(ready) = fresh_ready_path(&base_dir(), i) else {
            log("no unused ready-file name; the app is started without the ladder");
            run_forced(exe, args);
            return;
        };
        let started = Instant::now();
        let mut child = match Command::new(exe)
            .arg("--renderer")
            .arg(renderer.arg())
            .arg("--ready-file")
            .arg(&ready)
            .args(args)
            .spawn()
        {
            Ok(child) => child,
            Err(e) => fail(&format!("Plumbline could not start.\n\n{e}\n\n{}", details(false))),
        };
        let attempt = wait_for_ready(&mut child, &ready);
        let _ = fs::remove_file(&ready);
        let ms = started.elapsed().as_millis();
        match next_step(attempt) {
            Then::Remember => {
                log(&format!("the app painted its first frame with {} after {ms} ms", renderer.arg()));
                if remembered != Some(renderer) {
                    if let Err(e) = fs::write(remembered_path(), renderer.arg()) {
                        log(&format!("cannot remember the renderer: {e}"));
                    }
                }
                return;
            }
            Then::Stop => {
                log(&format!("the app exited after {ms} ms with code 0 before its first frame (closed)"));
                return;
            }
            Then::LeaveRunning => {
                log(&format!(
                    "no first frame from {} after {ms} ms; the app is left running and the launcher exits",
                    renderer.arg()
                ));
                return;
            }
            Then::TryNext => {
                let Attempt::Exited(code) = attempt else { unreachable!() };
                log(&format!(
                    "the {} renderer failed: the app exited after {ms} ms with code {code} ({:#010x})",
                    renderer.arg(),
                    code as u32
                ));
                failures.push((renderer, code));
            }
        }
    }
    fail(&failure_message(&failures, &details(true)));
}

/// With a renderer chosen by the user: watch the app for a few seconds, since a crash or a
/// failed start right away would otherwise look like nothing happening. Exit code 0 means the
/// user closed it; EXIT_REPORTED means the app has already shown its own message. Either way,
/// say nothing.
fn run_forced(exe: &Path, args: &[OsString]) {
    let mut child = match Command::new(exe).args(args).spawn() {
        Ok(child) => child,
        Err(e) => fail(&format!("Plumbline could not start.\n\n{e}\n\n{}", details(false))),
    };
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
                         Try another renderer: --renderer glow or --renderer software (see \
                         Troubleshooting in the README), and send that log file if it keeps \
                         happening.",
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
    let exe = dir.join("plumbline.exe");
    if user_forced(&args, env::var_os(RENDERER_ENV).as_deref()) {
        log("the renderer was chosen by the user; no ladder");
        run_forced(&exe, &args);
    } else {
        run_ladder(&exe, &args);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn ladder_puts_the_remembered_renderer_first() {
        use Renderer::*;
        assert_eq!(ladder(None), [Wgpu, Glow, Software]);
        assert_eq!(ladder(Some(Wgpu)), [Wgpu, Glow, Software]);
        assert_eq!(ladder(Some(Glow)), [Glow, Wgpu, Software]);
        assert_eq!(ladder(Some(Software)), [Software, Wgpu, Glow]);
    }

    #[test]
    fn remembered_file_contents() {
        assert_eq!(Renderer::parse("glow\r\n"), Some(Renderer::Glow));
        assert_eq!(Renderer::parse(" Software "), Some(Renderer::Software));
        assert_eq!(Renderer::parse("wgpu"), Some(Renderer::Wgpu));
        assert_eq!(Renderer::parse(""), None);
        assert_eq!(Renderer::parse("vulkan"), None);
        for r in ORDER {
            assert_eq!(Renderer::parse(r.arg()), Some(r));
        }
    }

    #[test]
    fn exit_codes_are_classified() {
        assert_eq!(next_step(Attempt::Ready), Then::Remember);
        assert_eq!(next_step(Attempt::Exited(0)), Then::Stop);
        assert_eq!(next_step(Attempt::Exited(EXIT_REPORTED)), Then::TryNext);
        // A native crash: STATUS_ACCESS_VIOLATION as an exit code.
        assert_eq!(next_step(Attempt::Exited(0xC000_0005_u32 as i32)), Then::TryNext);
        assert_eq!(next_step(Attempt::Exited(-1)), Then::TryNext);
        assert_eq!(next_step(Attempt::StillStarting), Then::LeaveRunning);
    }

    #[test]
    fn a_renderer_chosen_by_the_user_bypasses_the_ladder() {
        assert!(!user_forced(&os(&[]), None));
        assert!(!user_forced(&os(&["a.pdf"]), Some(OsStr::new(""))));
        assert!(user_forced(&os(&["--renderer", "glow", "a.pdf"]), None));
        assert!(user_forced(&os(&["a.pdf", "--renderer=software"]), None));
        assert!(user_forced(&os(&["a.pdf"]), Some(OsStr::new("glow"))));
        // After `--` it is a file name, as in the app.
        assert!(!user_forced(&os(&["--", "--renderer"]), None));
        // Only the exact option counts.
        assert!(!user_forced(&os(&["--renderers.pdf"]), None));
    }

    #[test]
    fn failure_message_lists_every_code_and_the_log() {
        let text = failure_message(
            &[(Renderer::Wgpu, 0xC000_0005_u32 as i32), (Renderer::Glow, 3), (Renderer::Software, 1)],
            "Details may be in X:\\log",
        );
        assert!(text.contains("wgpu: exit code -1073741819 (0xc0000005)"), "{text}");
        assert!(text.contains("glow: exit code 3 (0x00000003)"), "{text}");
        assert!(text.contains("software: exit code 1"), "{text}");
        assert!(text.contains("X:\\log") && text.contains("Windows Update"), "{text}");
    }

    #[test]
    fn ready_files_are_fresh_and_stale_ones_are_cleared() {
        let base = env::temp_dir().join(format!("plumbline-pack-ready-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        assert!(is_ready_file_name(OsStr::new("ready-1-0.tmp")));
        assert!(!is_ready_file_name(OsStr::new("ready-.tmp")));
        assert!(!is_ready_file_name(OsStr::new(REMEMBERED_FILE)));
        assert!(!is_ready_file_name(OsStr::new("plumbline.log")));
        let a = fresh_ready_path(&base, 0).unwrap();
        assert!(is_ready_file_name(a.file_name().unwrap()));
        // A leftover with that name is never reused.
        fs::write(&a, b"wgpu").unwrap();
        let b = fresh_ready_path(&base, 0).unwrap();
        assert_ne!(a, b);
        assert!(!b.exists());
        fs::write(base.join(REMEMBERED_FILE), b"glow").unwrap();
        fs::create_dir_all(base.join("logs")).unwrap();
        // Young files stay (another launcher may be waiting on one); old ones go.
        remove_stale_ready_files(&base, Duration::from_secs(3600));
        assert!(a.exists());
        remove_stale_ready_files(&base, Duration::ZERO);
        assert!(!a.exists());
        assert!(base.join(REMEMBERED_FILE).exists() && base.join("logs").is_dir());
        fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn old_payloads_go_but_the_remembered_renderer_stays() {
        let base = env::temp_dir().join(format!("plumbline-pack-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let app = base.join("app");
        let (keep, old) = (app.join("new"), app.join("old"));
        fs::create_dir_all(&keep).unwrap();
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("plumbline.exe"), b"x").unwrap();
        fs::write(app.join("stray.txt"), b"x").unwrap();
        fs::write(base.join(REMEMBERED_FILE), b"glow").unwrap();
        remove_old_payloads(&app, &keep);
        assert!(keep.is_dir());
        assert!(!old.exists());
        assert!(app.join("stray.txt").exists());
        assert_eq!(fs::read_to_string(base.join(REMEMBERED_FILE)).unwrap(), "glow");
        fs::remove_dir_all(&base).unwrap();
    }
}
