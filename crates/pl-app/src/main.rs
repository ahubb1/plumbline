//! `plumbline.exe`: the window. Any arguments are PDF files to open, plus
//! `--renderer wgpu|glow|software` (or the `PLUMBLINE_RENDERER` environment variable) to force
//! one graphics back end, and the launcher's hidden `--ready-file <path>`.
//!
//! The release exe has no console, so every failure is written to the startup log
//! (`%LOCALAPPDATA%\Plumbline\logs\plumbline.log`, see `pl_app::startup`) and shown in a message
//! box. The boxes live here, not in the library, so the tests never meet one. A native crash
//! (inside a graphics driver, say) cannot be caught, but an unhandled-exception filter writes
//! one line about it to the log first.

// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use pl_app::startup::{self, log, RendererChoice};

/// The message of the last panic on the main thread, for the message box after it unwinds.
static MAIN_PANIC: Mutex<Option<String>> = Mutex::new(None);
/// Set once a backtrace has been logged. Only the first panic of a run gets one: worker threads
/// catch expected panics (a malformed PDF can raise many), and symbolising a backtrace for each
/// would slow them down for nothing.
static BACKTRACE_LOGGED: AtomicBool = AtomicBool::new(false);

/// Advice for a renderer that failed with wgpu.
const GLOW_HINT: &str = "If this keeps happening, try starting Plumbline with --renderer glow \
                         (see Troubleshooting in the README).";
/// Advice for a renderer that failed with OpenGL.
const SOFTWARE_HINT: &str = "If this keeps happening, try starting Plumbline with --renderer \
                             software (see Troubleshooting in the README).";
/// Advice when every renderer, or the software one, failed.
const DRIVER_HINT: &str = "Updating the graphics driver (in Windows Update, under Advanced \
                           options > Optional updates) may help. If it does not, send the log \
                           (see Troubleshooting in the README).";
/// Advice for a bad command line or PLUMBLINE_RENDERER value.
const ARGS_HINT: &str = "Use --renderer wgpu, --renderer glow or --renderer software, or remove \
                         the PLUMBLINE_RENDERER setting.";

/// Whether a failure should only be logged: the launcher started this run with `--ready-file`
/// and is still waiting for it, so it will try the next renderer or show one message itself.
/// See [`launcher_still_waiting`].
static LAUNCHER_WAITING: AtomicBool = AtomicBool::new(false);
/// When this run started.
static STARTED: OnceLock<Instant> = OnceLock::new();
/// How long after the start the launcher is still taken to be waiting. The launcher waits 30 s
/// for each attempt (`READY_WAIT` in pack/src/main.rs) and then leaves the app running and
/// exits; after this, with a 5 s margin, the app shows its own message again.
const LAUNCHER_PATIENCE: Duration = Duration::from_secs(25);

/// Whether a failure now is the launcher's to report: it started this run, the first frame has
/// not been painted, and its wait for that has not yet run out.
fn launcher_still_waiting() -> bool {
    LAUNCHER_WAITING.load(Ordering::Relaxed)
        && !startup::ready_reached()
        && STARTED
            .get()
            .is_some_and(|t| t.elapsed() < LAUNCHER_PATIENCE)
}

/// The unhandled-exception filter: one line in the log for a native crash, then on to Windows
/// Error Reporting. Everything it needs is prepared by [`install`] before the window opens.
#[cfg(windows)]
mod crash {
    use core::ffi::c_void;
    use std::path::Path;
    use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

    use pl_app::startup;

    #[repr(C)]
    struct ExceptionRecord {
        code: u32,
        flags: u32,
        record: *mut ExceptionRecord,
        address: *mut c_void,
        // (parameters follow; not read)
    }

    #[repr(C)]
    struct ExceptionPointers {
        record: *mut ExceptionRecord,
        context: *mut c_void,
    }

    type Filter = unsafe extern "system" fn(*const ExceptionPointers) -> i32;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetUnhandledExceptionFilter(filter: Option<Filter>) -> Option<Filter>;
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *mut c_void,
            disposition: u32,
            flags: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn WriteFile(
            file: *mut c_void,
            buffer: *const u8,
            len: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut *mut c_void) -> i32;
        fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
        fn GetSystemTimeAsFileTime(time: *mut u64);
        fn GetCurrentProcessId() -> u32;
    }

    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
    const FILE_APPEND_DATA: u32 = 0x0004;
    /// Read, write and delete sharing, as std opens files, so the log can still be written and
    /// rotated by the app while this handle is open.
    const FILE_SHARE_ALL: u32 = 0x7;
    const OPEN_ALWAYS: u32 = 4;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;
    const INVALID_HANDLE_VALUE: *mut c_void = usize::MAX as *mut c_void;
    const FROM_ADDRESS_UNCHANGED_REFCOUNT: u32 = 0x4 | 0x2;
    /// 100 ns intervals between 1601 and 1970.
    const EPOCH_DIFFERENCE: u64 = 116_444_736_000_000_000;

    /// The log, opened for appending before anything can crash.
    static LOG: AtomicPtr<c_void> = AtomicPtr::new(core::ptr::null_mut());
    static PID: AtomicUsize = AtomicUsize::new(0);
    static PREVIOUS: AtomicUsize = AtomicUsize::new(0);

    /// Opens `log_path` for the filter and installs it. If the rotation renames the log during
    /// the run, the line goes to `plumbline.log.1`, which is sent along anyway.
    pub fn install(log_path: &Path) {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = log_path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: a NUL-terminated path; the handle stays open for the life of the process.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_APPEND_DATA,
                FILE_SHARE_ALL,
                core::ptr::null_mut(),
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                core::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            startup::log("the native crash filter could not open the log; it is not installed");
            return;
        }
        LOG.store(handle, Ordering::Release);
        // SAFETY: plain Win32 calls.
        unsafe {
            PID.store(GetCurrentProcessId() as usize, Ordering::Relaxed);
            let previous = SetUnhandledExceptionFilter(Some(filter));
            PREVIOUS.store(previous.map_or(0, |f| f as usize), Ordering::Release);
        }
    }

    /// Writes one line (on the stack, with a pre-opened handle: no heap, no locks) and lets the
    /// search go on, so Windows Error Reporting still records the crash.
    unsafe extern "system" fn filter(info: *const ExceptionPointers) -> i32 {
        let record = if info.is_null() {
            core::ptr::null_mut()
        } else {
            (*info).record
        };
        let (code, address) = if record.is_null() {
            (0, 0)
        } else {
            ((*record).code, (*record).address as usize)
        };
        let mut module_name = [0u16; 520];
        let mut module_len = 0usize;
        let mut module: *mut c_void = core::ptr::null_mut();
        if address != 0
            && GetModuleHandleExW(
                FROM_ADDRESS_UNCHANGED_REFCOUNT,
                address as *const u16,
                &mut module,
            ) != 0
        {
            module_len =
                GetModuleFileNameW(module, module_name.as_mut_ptr(), module_name.len() as u32)
                    as usize;
        }
        let mut now = 0u64;
        GetSystemTimeAsFileTime(&mut now);
        let secs = now.saturating_sub(EPOCH_DIFFERENCE) / 10_000_000;
        let mut line = [0u8; 1024];
        let len = startup::format_crash_line(
            &mut line,
            secs,
            PID.load(Ordering::Relaxed) as u32,
            code,
            address,
            (module_len > 0).then(|| {
                (
                    &module_name[..module_len.min(module_name.len())],
                    address.wrapping_sub(module as usize),
                )
            }),
            startup::renderer_in_use(),
        );
        let log = LOG.load(Ordering::Acquire);
        if !log.is_null() {
            let mut written = 0u32;
            WriteFile(
                log,
                line.as_ptr(),
                len as u32,
                &mut written,
                core::ptr::null_mut(),
            );
        }
        // A filter installed before this one still runs, but its answer is ignored: one that
        // returned EXCEPTION_EXECUTE_HANDLER would end the process before Windows Error
        // Reporting saw the crash.
        let previous = PREVIOUS.load(Ordering::Acquire);
        if previous != 0 {
            // SAFETY: the value came from SetUnhandledExceptionFilter as a filter pointer.
            core::mem::transmute::<usize, Filter>(previous)(info);
        }
        EXCEPTION_CONTINUE_SEARCH
    }
}

#[cfg(windows)]
fn message_box(text: &str) {
    #[link(name = "user32")]
    extern "system" {
        fn MessageBoxW(
            hwnd: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            utype: u32,
        ) -> i32;
    }
    const MB_ICONERROR: u32 = 0x10;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the call.
    unsafe {
        MessageBoxW(
            core::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("Plumbline").as_ptr(),
            MB_ICONERROR | MB_SETFOREGROUND,
        )
    };
}

#[cfg(not(windows))]
fn message_box(text: &str) {
    eprintln!("{text}");
}

/// Logs every panic (worker threads catch theirs, so most are only logged) and keeps the
/// message of one on the main thread for [`main`] to show.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown location".to_owned());
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        if BACKTRACE_LOGGED.swap(true, Ordering::Relaxed) {
            log(&format!(
                "panic on thread '{name}' at {location}: {message}"
            ));
        } else {
            let backtrace = std::backtrace::Backtrace::force_capture();
            log(&format!(
                "panic on thread '{name}' at {location}: {message}\nbacktrace:\n{backtrace}"
            ));
        }
        if name == "main" {
            *MAIN_PANIC.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(format!("{message} (at {location})"));
        }
        default(info);
    }));
}

/// Logs a fatal error, tells the user and returns [`startup::EXIT_REPORTED`].
fn fatal(what: &str, reason: &str, hint: &str, log_path: &Path) -> ExitCode {
    log(&format!("fatal: {what}: {reason}"));
    if launcher_still_waiting() {
        log("not shown: the launcher tries the next renderer or reports it");
        return ExitCode::from(startup::EXIT_REPORTED);
    }
    let details = if startup::log_failed() {
        format!("The log could not be written to {}", log_path.display())
    } else {
        format!("Details are in {}", log_path.display())
    };
    message_box(&format!("{what}: {reason}.\n\n{details}\n\n{hint}"));
    ExitCode::from(startup::EXIT_REPORTED)
}

fn main() -> ExitCode {
    STARTED.get_or_init(Instant::now);
    let log_path = startup::init_log();
    install_panic_hook();
    log(&startup::describe_run());
    #[cfg(windows)]
    crash::install(log_path);

    let launch = match startup::parse_args(
        std::env::args_os().skip(1),
        std::env::var_os(startup::RENDERER_ENV),
    ) {
        Ok(launch) => launch,
        Err(e) => return fatal("Plumbline could not start", &e, ARGS_HINT, log_path),
    };
    let choice = launch.renderer;
    LAUNCHER_WAITING.store(launch.ready_file.is_some(), Ordering::Relaxed);
    log(&format!(
        "renderer choice {choice:?}; {} file(s) to open{}",
        launch.files.len(),
        if launch.ready_file.is_some() {
            "; started by the launcher"
        } else {
            ""
        }
    ));

    let outcome = std::panic::catch_unwind(move || {
        pl_app::shell::run(launch.files, choice, launch.ready_file)
    });
    match outcome {
        Ok(Ok(())) => {
            log("closed normally");
            ExitCode::SUCCESS
        }
        Ok(Err(message)) => {
            // A forced renderer suggests the next one; Auto has already tried them all.
            let hint = match choice {
                RendererChoice::Wgpu => GLOW_HINT,
                RendererChoice::Glow => SOFTWARE_HINT,
                RendererChoice::Auto | RendererChoice::Software => DRIVER_HINT,
            };
            fatal("Plumbline could not start", &message, hint, log_path)
        }
        Err(_) => {
            let message = MAIN_PANIC
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| "an internal error".to_owned());
            let hint = match choice {
                RendererChoice::Wgpu | RendererChoice::Auto => GLOW_HINT,
                RendererChoice::Glow => SOFTWARE_HINT,
                RendererChoice::Software => DRIVER_HINT,
            };
            fatal("Plumbline stopped unexpectedly", &message, hint, log_path)
        }
    }
}
