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
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
    /// wgpu on a Direct3D 12 GPU, then OpenGL, then the software renderer, each only if the
    /// one before cannot start.
    #[default]
    Auto,
    /// wgpu on a Direct3D 12 GPU only.
    Wgpu,
    /// OpenGL (glow) only.
    Glow,
    /// wgpu on Direct3D 12's software rasteriser (WARP, part of Windows) only: slow, but it
    /// needs no working graphics driver.
    Software,
}

impl RendererChoice {
    /// Parses `wgpu`, `glow` or `software` (any case; `opengl` and `gl` also mean glow,
    /// `warp` and `cpu` also mean software).
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "wgpu" => Ok(RendererChoice::Wgpu),
            "glow" | "opengl" | "gl" => Ok(RendererChoice::Glow),
            "software" | "warp" | "cpu" => Ok(RendererChoice::Software),
            "auto" => Ok(RendererChoice::Auto),
            other => Err(format!(
                "unknown renderer \"{other}\" (use \"wgpu\", \"glow\" or \"software\")"
            )),
        }
    }

    /// How the log and messages name this renderer.
    pub fn label(self) -> &'static str {
        RENDERER_LABELS[self.index()]
    }

    fn index(self) -> usize {
        match self {
            RendererChoice::Auto => 0,
            RendererChoice::Wgpu => 1,
            RendererChoice::Glow => 2,
            RendererChoice::Software => 3,
        }
    }

    /// The renderers `shell::run` tries for this choice, in order, each only if the one
    /// before returned an error.
    pub fn attempts(self) -> &'static [RendererChoice] {
        match self {
            RendererChoice::Auto => &[
                RendererChoice::Wgpu,
                RendererChoice::Glow,
                RendererChoice::Software,
            ],
            RendererChoice::Wgpu => &[RendererChoice::Wgpu],
            RendererChoice::Glow => &[RendererChoice::Glow],
            RendererChoice::Software => &[RendererChoice::Software],
        }
    }
}

const RENDERER_LABELS: [&str; 4] = [
    "automatic",
    "wgpu (Direct3D 12)",
    "glow (OpenGL)",
    "software (Direct3D 12 WARP)",
];

/// The renderer being started or running, for the native crash line: an index into
/// [`RENDERER_LABELS`], with `u8::MAX` while none is.
static RENDERER_IN_USE: AtomicU8 = AtomicU8::new(u8::MAX);

/// Records which renderer is being started (`None`: none is).
pub fn set_renderer_in_use(choice: Option<RendererChoice>) {
    let index = choice.map_or(u8::MAX, |c| c.index() as u8);
    RENDERER_IN_USE.store(index, Ordering::Relaxed);
}

/// The label of the renderer last passed to [`set_renderer_in_use`]. Lock- and allocation-free,
/// so the native crash filter can call it.
pub fn renderer_in_use() -> &'static str {
    RENDERER_LABELS
        .get(usize::from(RENDERER_IN_USE.load(Ordering::Relaxed)))
        .copied()
        .unwrap_or("none yet")
}

/// What the command line asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Launch {
    pub renderer: RendererChoice,
    /// `--ready-file <path>` (hidden; the single-file launcher passes it): created once the
    /// first frame has been painted, so the launcher knows this renderer works.
    pub ready_file: Option<PathBuf>,
    /// PDF files to open.
    pub files: Vec<PathBuf>,
}

/// Reads the command line (without the program name) and the value of [`RENDERER_ENV`].
/// `--renderer wgpu|glow|software` (or `--renderer=...`) wins over the environment variable;
/// `--ready-file <path>` (or `--ready-file=<path>`) is for the launcher; every other argument
/// is a file to open, and everything after `--` is a file even if it looks like an option.
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
                .ok_or("--renderer needs a value (\"wgpu\", \"glow\" or \"software\")")?;
            launch.renderer = RendererChoice::parse(&value.to_string_lossy())
                .map_err(|e| format!("--renderer: {e}"))?;
            flag_seen = true;
        } else if let Some(value) = text.strip_prefix("--renderer=") {
            launch.renderer =
                RendererChoice::parse(value).map_err(|e| format!("--renderer: {e}"))?;
            flag_seen = true;
        } else if text == "--ready-file" {
            let value = args.next().ok_or("--ready-file needs a path")?;
            launch.ready_file = Some(PathBuf::from(value));
        } else if let Some(value) = text.strip_prefix("--ready-file=") {
            launch.ready_file = Some(PathBuf::from(value));
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
    let (year, month, day, h, m, s) = civil_time(secs);
    format!("{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}Z")
}

/// Year, month, day, hour, minute and second (UTC) of seconds since 1970. No allocation.
fn civil_time(secs: u64) -> (i64, i64, i64, u64, u64, u64) {
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
    (year, month, day, rem / 3600, rem / 60 % 60, rem % 60)
}

/// A `fmt::Write` into a fixed buffer that drops what does not fit (no allocation).
struct StackWriter<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl core::fmt::Write for StackWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let room = self.buf.len() - self.len;
        let mut take = s.len().min(room);
        // Never cut a character in half.
        while !s.is_char_boundary(take) {
            take -= 1;
        }
        self.buf[self.len..self.len + take].copy_from_slice(&s.as_bytes()[..take]);
        self.len += take;
        Ok(())
    }
}

/// A short name for common Windows exception codes.
fn exception_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "access violation",
        0xC000_001D => "illegal instruction",
        0xC000_0094 => "integer divide by zero",
        0xC000_00FD => "stack overflow",
        0xC000_0374 => "heap corruption",
        0xC000_0409 => "stack buffer overrun",
        0x8000_0003 => "breakpoint",
        0xE06D_7363 => "C++ exception",
        _ => "exception",
    }
}

/// Formats the log line for a native crash (an unhandled exception, typically inside a graphics
/// driver) into `out` and returns its length. `module` is the full path of the faulting module
/// as UTF-16 with the faulting address's offset in it; only its file name is written, so the
/// line holds no user folder. Uses no heap and no locks: it runs inside the unhandled-exception
/// filter. A line that does not fit is cut short but always ends in a newline.
pub fn format_crash_line(
    out: &mut [u8],
    secs: u64,
    pid: u32,
    code: u32,
    address: usize,
    module: Option<(&[u16], usize)>,
    renderer: &str,
) -> usize {
    use core::fmt::Write as _;
    let Some(last) = out.len().checked_sub(1) else {
        return 0;
    };
    let mut w = StackWriter {
        buf: &mut out[..last],
        len: 0,
    };
    let (year, month, day, h, m, s) = civil_time(secs);
    let _ = write!(
        w,
        "{year:04}-{month:02}-{day:02} {h:02}:{m:02}:{s:02}Z app[{pid}] native crash: {} \
         {code:#010x} at {address:#x} in ",
        exception_name(code)
    );
    match module {
        Some((path, offset)) => {
            let start = path
                .iter()
                .rposition(|&c| c == u16::from(b'\\') || c == u16::from(b'/'))
                .map_or(0, |i| i + 1);
            for c in char::decode_utf16(path[start..].iter().copied()) {
                let _ = w.write_char(c.unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            let _ = write!(w, "+{offset:#x}");
        }
        None => {
            let _ = w.write_str("an unknown module");
        }
    }
    let _ = write!(w, "; renderer: {renderer}");
    let len = w.len;
    out[len] = b'\n';
    len + 1
}

/// Set once the first frame has been painted (see [`ReadySignal`]).
static READY: AtomicBool = AtomicBool::new(false);

/// Whether this run has painted its first frame. After that a failure is no longer a failed
/// start, and the launcher has already gone.
pub fn ready_reached() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Tells the launcher, once, that the window has painted its first frame: creates the
/// `--ready-file` (holding the renderer's name) and logs it.
#[derive(Debug, Default)]
pub struct ReadySignal {
    path: Option<PathBuf>,
    done: bool,
}

impl ReadySignal {
    pub fn new(path: Option<PathBuf>) -> Self {
        ReadySignal { path, done: false }
    }

    /// Whether [`ReadySignal::signal`] has been called.
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Marks the run as ready. Only the first call does anything; it returns whether it wrote
    /// the ready file.
    pub fn signal(&mut self, renderer: &str) -> bool {
        if std::mem::replace(&mut self.done, true) {
            return false;
        }
        READY.store(true, Ordering::Relaxed);
        let Some(path) = &self.path else {
            log(&format!("first frame painted ({renderer})"));
            return false;
        };
        match std::fs::write(path, renderer) {
            Ok(()) => {
                log(&format!(
                    "first frame painted ({renderer}); ready file written"
                ));
                true
            }
            Err(e) => {
                log(&format!(
                    "first frame painted ({renderer}); the ready file could not be written: {e}"
                ));
                false
            }
        }
    }
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
    fn software_renderer_in_every_spelling() {
        for spelling in ["software", "SOFTWARE", " warp ", "cpu"] {
            let launch = parse_args(os(&["--renderer", spelling]), None).unwrap();
            assert_eq!(launch.renderer, RendererChoice::Software, "{spelling}");
        }
        let launch = parse_args(os(&["--renderer=software", "a.pdf"]), None).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Software);
        assert_eq!(launch.files, [PathBuf::from("a.pdf")]);
        let launch = parse_args(os(&[]), Some("Software".into())).unwrap();
        assert_eq!(launch.renderer, RendererChoice::Software);
        let err = parse_args(os(&["--renderer", "dx11"]), None).unwrap_err();
        assert!(err.contains("software"), "{err}");
    }

    #[test]
    fn automatic_order_is_wgpu_then_glow_then_software() {
        assert_eq!(
            RendererChoice::Auto.attempts(),
            [
                RendererChoice::Wgpu,
                RendererChoice::Glow,
                RendererChoice::Software
            ]
        );
        for one in [
            RendererChoice::Wgpu,
            RendererChoice::Glow,
            RendererChoice::Software,
        ] {
            assert_eq!(one.attempts(), [one]);
        }
    }

    #[test]
    fn ready_file_flag_in_both_spellings() {
        let launch = parse_args(
            os(&[
                "--ready-file",
                "C:\\t\\r.tmp",
                "--renderer",
                "glow",
                "a.pdf",
            ]),
            None,
        )
        .unwrap();
        assert_eq!(launch.ready_file, Some(PathBuf::from("C:\\t\\r.tmp")));
        assert_eq!(launch.renderer, RendererChoice::Glow);
        assert_eq!(launch.files, [PathBuf::from("a.pdf")]);
        let launch = parse_args(os(&["--ready-file=r.tmp"]), None).unwrap();
        assert_eq!(launch.ready_file, Some(PathBuf::from("r.tmp")));
        assert!(launch.files.is_empty());
        let err = parse_args(os(&["--ready-file"]), None).unwrap_err();
        assert!(err.contains("needs a path"), "{err}");
        assert_eq!(parse_args(os(&["a.pdf"]), None).unwrap().ready_file, None);
    }

    #[test]
    fn ready_file_is_written_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ready.tmp");
        let mut signal = ReadySignal::new(Some(path.clone()));
        assert!(signal.signal("wgpu"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "wgpu");
        assert!(ready_reached());
        // The launcher removes it; later frames must not create it again.
        std::fs::remove_file(&path).unwrap();
        assert!(!signal.signal("wgpu"));
        assert!(!signal.signal("wgpu"));
        assert!(!path.exists());
        // Without a path (a direct start) nothing is written.
        assert!(!ReadySignal::new(None).signal("glow"));
    }

    #[test]
    fn crash_line_names_code_address_module_and_renderer() {
        let mut buf = [0u8; 512];
        let module: Vec<u16> = "C:\\Windows\\System32\\DriverStore\\igd12umd64.dll"
            .encode_utf16()
            .collect();
        let n = format_crash_line(
            &mut buf,
            1_795_000_000,
            42,
            0xC000_0005,
            0x7ffa_1234_5678,
            Some((&module, 0x1234)),
            RendererChoice::Wgpu.label(),
        );
        let line = std::str::from_utf8(&buf[..n]).unwrap();
        assert_eq!(
            line,
            "2026-11-18 11:06:40Z app[42] native crash: access violation 0xc0000005 at \
             0x7ffa12345678 in igd12umd64.dll+0x1234; renderer: wgpu (Direct3D 12)\n"
        );

        let n = format_crash_line(&mut buf, 0, 1, 0x1234, 0, None, renderer_in_use());
        let line = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(
            line.contains("exception 0x00001234 at 0x0 in an unknown module"),
            "{line}"
        );
        assert!(line.ends_with('\n'));
    }

    #[test]
    fn crash_line_is_cut_to_the_buffer_and_still_ends_in_a_newline() {
        let mut small = [0u8; 40];
        let n = format_crash_line(&mut small, 0, 1, 0xC000_0005, 1, None, "glow (OpenGL)");
        assert_eq!(n, 40);
        assert_eq!(small[39], b'\n');
        assert!(std::str::from_utf8(&small[..n]).is_ok());
        assert_eq!(format_crash_line(&mut [], 0, 1, 0, 0, None, "x"), 0);
        // A multi-byte character at the cut is dropped whole.
        let module: Vec<u16> = "é".repeat(40).encode_utf16().collect();
        let mut buf = [0u8; 100];
        let n = format_crash_line(&mut buf, 0, 1, 0, 0, Some((&module, 0)), "x");
        assert!(std::str::from_utf8(&buf[..n]).is_ok());
    }

    #[test]
    fn renderer_in_use_labels() {
        set_renderer_in_use(Some(RendererChoice::Software));
        assert_eq!(renderer_in_use(), "software (Direct3D 12 WARP)");
        set_renderer_in_use(None);
        assert_eq!(renderer_in_use(), "none yet");
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
