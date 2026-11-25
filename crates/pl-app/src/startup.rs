//! What `plumbline.exe` does before and around the window: the command line, the startup log
//! and the facts about the PC that go in it.
//!
//! The release exe has no console, so anything that goes wrong before the window shows would
//! be invisible. Each run therefore appends to `%LOCALAPPDATA%\Plumbline\logs\plumbline.log`
//! (at [`LOG_LIMIT`] it moves to `plumbline.log.1` and a new one starts). Nothing here shows a
//! message box: that is left to `main.rs`, so tests never block.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// The log is rotated when it reaches this size.
pub const LOG_LIMIT: u64 = 1024 * 1024;
/// File name of the log inside [`log_dir`].
pub const LOG_FILE_NAME: &str = "plumbline.log";
/// Environment variable that forces a renderer, like `--renderer`.
pub const RENDERER_ENV: &str = "PLUMBLINE_RENDERER";
/// Exit code of `plumbline.exe` after it has shown the user a message about a failure, so the
/// single-file launcher does not show a second one.
pub const EXIT_REPORTED: u8 = 3;

/// Which graphics back end the window uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RendererChoice {
    /// wgpu (Direct3D 12 or Vulkan), then OpenGL if wgpu cannot start.
    #[default]
    Auto,
    /// wgpu only.
    Wgpu,
    /// OpenGL (glow) only.
    Glow,
}

impl RendererChoice {
    /// Parses `wgpu` or `glow` (any case; `opengl` and `gl` also mean glow).
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "wgpu" => Ok(RendererChoice::Wgpu),
            "glow" | "opengl" | "gl" => Ok(RendererChoice::Glow),
            "auto" => Ok(RendererChoice::Auto),
            other => Err(format!(
                "unknown renderer \"{other}\" (use \"wgpu\" or \"glow\")"
            )),
        }
    }
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Launch {
    pub renderer: RendererChoice,
    /// PDF files to open.
    pub files: Vec<PathBuf>,
}

/// Reads the command line (without the program name) and the value of [`RENDERER_ENV`].
/// `--renderer wgpu|glow` (or `--renderer=...`) wins over the environment variable; every
/// other argument is a file to open, and everything after `--` is a file even if it looks like
/// an option.
pub fn parse_args(
    args: impl IntoIterator<Item = OsString>,
    env_renderer: Option<OsString>,
) -> Result<Launch, String> {
    let mut launch = Launch::default();
    let mut flag_seen = false;
    let mut args = args.into_iter();
    let mut only_files = false;
    while let Some(arg) = args.next() {
        if only_files {
            launch.files.push(PathBuf::from(arg));
            continue;
        }
        let text = arg.to_string_lossy();
        if text == "--" {
            only_files = true;
        } else if text == "--renderer" {
            let value = args
                .next()
                .ok_or("--renderer needs a value (\"wgpu\" or \"glow\")")?;
            launch.renderer = RendererChoice::parse(&value.to_string_lossy())
                .map_err(|e| format!("--renderer: {e}"))?;
            flag_seen = true;
        } else if let Some(value) = text.strip_prefix("--renderer=") {
            launch.renderer =
                RendererChoice::parse(value).map_err(|e| format!("--renderer: {e}"))?;
            flag_seen = true;
        } else {
            launch.files.push(PathBuf::from(arg));
        }
    }
    // The environment variable counts only without the flag, so a bad value in it cannot stop
    // a start that names the renderer on the command line.
    if !flag_seen {
        if let Some(value) = env_renderer.filter(|v| !v.is_empty()) {
            launch.renderer = RendererChoice::parse(&value.to_string_lossy())
                .map_err(|e| format!("{RENDERER_ENV}: {e}"))?;
        }
    }
    Ok(launch)
}

/// `%LOCALAPPDATA%\Plumbline\logs`, or the temp folder's `Plumbline\logs` without one.
/// A `LOCALAPPDATA` that is empty or relative counts as unset (as in `pl_doc`), so the log
/// never lands under whatever the current folder happens to be.
pub fn log_dir() -> PathBuf {
    log_dir_from(std::env::var_os("LOCALAPPDATA"))
}

fn log_dir_from(local_app_data: Option<OsString>) -> PathBuf {
    local_app_data
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(std::env::temp_dir)
        .join("Plumbline")
        .join("logs")
}

/// Moves `path` to `<path>.1` (replacing an older one) once it has reached `limit` bytes.
pub fn rotate_if_full(path: &Path, limit: u64) -> std::io::Result<bool> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() >= limit => {
            let mut old = path.as_os_str().to_owned();
            old.push(".1");
            let old = PathBuf::from(old);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(path, &old)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// `YYYY-MM-DD hh:mm:ssZ` for seconds since 1970 (UTC).
pub fn utc_timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// An append-only log file that rotates itself at [`LOG_LIMIT`].
#[derive(Debug)]
pub struct LogFile {
    path: PathBuf,
    limit: u64,
    lock: Mutex<()>,
    /// Whether the last write failed, so a message can say the log is not there.
    failed: AtomicBool,
}

impl LogFile {
    /// A log at `dir/plumbline.log` (the folder is created on the first write).
    pub fn in_dir(dir: &Path, limit: u64) -> Self {
        LogFile {
            path: dir.join(LOG_FILE_NAME),
            limit,
            lock: Mutex::new(()),
            failed: AtomicBool::new(false),
        }
    }

    /// Whether the most recent [`LogFile::write`] could not write to the file.
    pub fn last_write_failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one entry, prefixed with the time and `app`. Failures are ignored: the log
    /// must never stop the app.
    pub fn write(&self, message: &str) {
        let ok = self.try_write(message).is_ok();
        self.failed.store(!ok, Ordering::Relaxed);
    }

    fn try_write(&self, message: &str) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        rotate_if_full(&self.path, self.limit)?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let mut line = format!(
            "{} app[{}] {message}",
            utc_timestamp(secs),
            std::process::id()
        );
        if !line.ends_with('\n') {
            line.push('\n');
        }
        file.write_all(line.as_bytes())
    }
}

static LOG: OnceLock<LogFile> = OnceLock::new();

/// Starts logging to [`log_dir`]. Called once by `main.rs`; until then (and in tests) [`log`]
/// does nothing. Returns the log's path.
pub fn init_log() -> &'static Path {
    LOG.get_or_init(|| LogFile::in_dir(&log_dir(), LOG_LIMIT))
        .path()
}

/// Appends to the startup log, if [`init_log`] has been called.
pub fn log(message: &str) {
    if let Some(log) = LOG.get() {
        log.write(message);
    }
}

/// Whether the last [`log`] call could not write to the file.
pub fn log_failed() -> bool {
    LOG.get().is_some_and(LogFile::last_write_failed)
}

/// One line about this run: version, Windows build, elevation and the exe's folder.
pub fn describe_run() -> String {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "unknown".into());
    format!(
        "Plumbline {} starting; Windows {}; {}; exe folder {exe_dir}",
        env!("CARGO_PKG_VERSION"),
        sys::windows_version().unwrap_or_else(|| "version unknown".into()),
        match sys::is_elevated() {
            Some(true) => "elevated (run as administrator)",
            Some(false) => "not elevated",
            None => "elevation unknown",
        },
    )
}

#[cfg(windows)]
mod sys {
    use core::ffi::c_void;

    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        service_pack: [u16; 128],
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            info: *mut c_void,
            len: u32,
            returned: *mut u32,
        ) -> i32;
    }

    /// `major.minor.build`, read with `RtlGetVersion` (which is not subject to the
    /// compatibility shims of `GetVersionEx`).
    pub fn windows_version() -> Option<String> {
        let mut info = OsVersionInfoW {
            size: std::mem::size_of::<OsVersionInfoW>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            service_pack: [0; 128],
        };
        // SAFETY: `info` is a correctly sized OSVERSIONINFOW with its size field set.
        let status = unsafe { RtlGetVersion(&mut info) };
        (status == 0).then(|| format!("{}.{}.{}", info.major, info.minor, info.build))
    }

    /// Whether this process runs with an elevated (administrator) token.
    pub fn is_elevated() -> Option<bool> {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_ELEVATION: u32 = 20;
        let mut token = std::ptr::null_mut();
        // SAFETY: plain Win32 calls with valid out-pointers; the token is closed below.
        unsafe {
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut elevated: u32 = 0;
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TOKEN_ELEVATION,
                (&mut elevated as *mut u32).cast(),
                4,
                &mut len,
            );
            CloseHandle(token);
            (ok != 0).then_some(elevated != 0)
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn windows_version() -> Option<String> {
        None
    }
    pub fn is_elevated() -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn renderer_defaults_to_auto_and_files_pass_through() {
        let launch = parse_args(os(&["a.pdf", "b c.pdf"]), None).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Auto);
        assert_eq!(
            launch.files,
            [PathBuf::from("a.pdf"), PathBuf::from("b c.pdf")]
        );
        assert_eq!(
            parse_args(os(&[]), Some("".into())).unwrap(),
            Launch::default()
        );
    }

    #[test]
    fn renderer_flag_in_both_spellings() {
        let launch = parse_args(os(&["--renderer", "glow", "a.pdf"]), None).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Glow);
        assert_eq!(launch.files, [PathBuf::from("a.pdf")]);
        let launch = parse_args(os(&["a.pdf", "--renderer=WGPU"]), None).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Wgpu);
        assert_eq!(launch.files, [PathBuf::from("a.pdf")]);
    }

    #[test]
    fn environment_variable_applies_and_the_flag_wins() {
        let launch = parse_args(os(&[]), Some("glow".into())).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Glow);
        let launch = parse_args(os(&["--renderer", "wgpu"]), Some("glow".into())).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Wgpu);
    }

    #[test]
    fn invalid_values_are_clear_errors() {
        let err = parse_args(os(&["--renderer", "vulkan"]), None).unwrap_err();
        assert!(
            err.contains("--renderer") && err.contains("vulkan") && err.contains("glow"),
            "{err}"
        );
        let err = parse_args(os(&["--renderer"]), None).unwrap_err();
        assert!(err.contains("needs a value"), "{err}");
        let err = parse_args(os(&[]), Some("dx9".into())).unwrap_err();
        assert!(
            err.starts_with(RENDERER_ENV) && err.contains("dx9"),
            "{err}"
        );
    }

    #[test]
    fn a_valid_flag_overrides_an_invalid_environment_value() {
        let launch = parse_args(os(&["--renderer", "glow"]), Some("dx9".into())).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Glow);
        let launch = parse_args(os(&["--renderer=wgpu", "a.pdf"]), Some("dx9".into())).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Wgpu);
        assert_eq!(launch.files, [PathBuf::from("a.pdf")]);
    }

    #[test]
    fn double_dash_ends_the_options() {
        let launch = parse_args(os(&["--", "--renderer", "x.pdf"]), None).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Auto);
        assert_eq!(
            launch.files,
            [PathBuf::from("--renderer"), PathBuf::from("x.pdf")]
        );
    }

    #[test]
    fn log_appends_and_rotates_at_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let log = LogFile::in_dir(&dir.path().join("logs"), 200);
        log.write("first");
        log.write("second");
        let text = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().next().unwrap().ends_with("first"));
        let old = dir.path().join("logs").join("plumbline.log.1");
        assert!(!old.exists());

        // Fill past the limit: the next write moves the full log aside and starts a new one.
        log.write(&"x".repeat(300));
        log.write("after");
        assert!(old.exists());
        let rotated = std::fs::read_to_string(&old).unwrap();
        assert!(rotated.contains("first") && rotated.contains(&"x".repeat(300)));
        let fresh = std::fs::read_to_string(log.path()).unwrap();
        assert_eq!(fresh.lines().count(), 1);
        assert!(fresh.ends_with("after\n"));

        // A second rotation replaces the old .1.
        log.write(&"y".repeat(300));
        log.write("again");
        let rotated = std::fs::read_to_string(&old).unwrap();
        assert!(!rotated.contains("first") && rotated.contains("after"));
    }

    #[test]
    fn log_remembers_a_failed_write() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the log folder should be: the folder cannot be created.
        let blocker = dir.path().join("logs");
        std::fs::write(&blocker, "not a folder").unwrap();
        let log = LogFile::in_dir(&blocker, 200);
        log.write("lost");
        assert!(log.last_write_failed());
        let log = LogFile::in_dir(&dir.path().join("ok"), 200);
        log.write("kept");
        assert!(!log.last_write_failed());
    }

    #[test]
    fn relative_or_empty_local_app_data_falls_back_to_temp() {
        let temp_logs = std::env::temp_dir().join("Plumbline").join("logs");
        assert_eq!(log_dir_from(None), temp_logs);
        assert_eq!(log_dir_from(Some("".into())), temp_logs);
        assert_eq!(log_dir_from(Some("relative\\dir".into())), temp_logs);
        let base = std::env::temp_dir().join("lad");
        assert_eq!(
            log_dir_from(Some(base.clone().into_os_string())),
            base.join("Plumbline").join("logs")
        );
    }

    #[test]
    fn rotate_leaves_small_or_missing_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plumbline.log");
        assert!(!rotate_if_full(&path, 10).unwrap());
        std::fs::write(&path, "abc").unwrap();
        assert!(!rotate_if_full(&path, 10).unwrap());
        assert!(rotate_if_full(&path, 3).unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn timestamps_are_utc_calendar_dates() {
        assert_eq!(utc_timestamp(0), "1970-01-01 00:00:00Z");
        assert_eq!(utc_timestamp(951_782_400), "2000-02-29 00:00:00Z");
        assert_eq!(utc_timestamp(1_795_000_000), "2026-11-18 11:06:40Z");
    }

    #[test]
    fn run_description_names_the_version() {
        assert!(describe_run().contains(env!("CARGO_PKG_VERSION")));
    }
}
