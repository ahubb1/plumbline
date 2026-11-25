//! `plumbline.exe`: the window. Any arguments are PDF files to open, plus
//! `--renderer wgpu|glow` (or the `PLUMBLINE_RENDERER` environment variable) to force one
//! graphics back end.
//!
//! The release exe has no console, so every failure is written to the startup log
//! (`%LOCALAPPDATA%\Plumbline\logs\plumbline.log`, see `pl_app::startup`) and shown in a message
//! box. The boxes live here, not in the library, so the tests never meet one.

// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

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
/// Advice when both renderers, or OpenGL alone, failed.
const DRIVER_HINT: &str = "Updating the graphics driver may help. If it does not, send the log \
                           (see Troubleshooting in the README).";
/// Advice for a bad command line or PLUMBLINE_RENDERER value.
const ARGS_HINT: &str = "Use --renderer wgpu or --renderer glow, or remove the \
                         PLUMBLINE_RENDERER setting.";

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
    let details = if startup::log_failed() {
        format!("The log could not be written to {}", log_path.display())
    } else {
        format!("Details are in {}", log_path.display())
    };
    message_box(&format!("{what}: {reason}.\n\n{details}\n\n{hint}"));
    ExitCode::from(startup::EXIT_REPORTED)
}

fn main() -> ExitCode {
    let log_path = startup::init_log();
    install_panic_hook();
    log(&startup::describe_run());

    let launch = match startup::parse_args(
        std::env::args_os().skip(1),
        std::env::var_os(startup::RENDERER_ENV),
    ) {
        Ok(launch) => launch,
        Err(e) => return fatal("Plumbline could not start", &e, ARGS_HINT, log_path),
    };
    let choice = launch.renderer;
    log(&format!(
        "renderer choice {choice:?}; {} file(s) to open",
        launch.files.len()
    ));

    let outcome = std::panic::catch_unwind(move || pl_app::shell::run(launch.files, choice));
    match outcome {
        Ok(Ok(())) => {
            log("closed normally");
            ExitCode::SUCCESS
        }
        Ok(Err(message)) => {
            // Only a forced wgpu has OpenGL left to try; Auto has already tried both.
            let hint = match choice {
                RendererChoice::Wgpu => GLOW_HINT,
                RendererChoice::Auto | RendererChoice::Glow => DRIVER_HINT,
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
                RendererChoice::Glow => DRIVER_HINT,
            };
            fatal("Plumbline stopped unexpectedly", &message, hint, log_path)
        }
    }
}
