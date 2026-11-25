//! What the app needs from the machine, behind traits so tests can stand in for it: the file
//! dialogs, the system clipboard, and a render backend for when PDFium could not be loaded.

use std::collections::VecDeque;
use std::path::PathBuf;

use pl_doc::pl_render::{RenderBackend, RenderError, Reply, Request};

/// The native file dialogs.
pub trait FileDialogs {
    /// Asks the user for PDF files to open. Empty when they cancel.
    fn pick_pdfs(&mut self) -> Vec<PathBuf>;

    /// Asks where to save a PDF, offering `suggested` as the file name. `None` when they
    /// cancel.
    fn save_pdf(&mut self, _suggested: &str) -> Option<PathBuf> {
        None
    }

    /// Asks for a picture (PNG or JPEG) to place on a page. `None` when they cancel.
    fn pick_image(&mut self) -> Option<PathBuf> {
        None
    }

    /// Asks where to write an export of the markups list. `None` when they cancel.
    fn save_export(&mut self, _kind: ExportKind, _suggested: &str) -> Option<PathBuf> {
        None
    }

    /// Asks for an XFDF file to bring markups in from. `None` when they cancel.
    fn pick_xfdf(&mut self) -> Option<PathBuf> {
        None
    }

    /// Asks for a drawing set (`.plset`) to open. `None` when they cancel.
    fn pick_set(&mut self) -> Option<PathBuf> {
        None
    }

    /// Asks where to write a new drawing set, offering `suggested` as the file name. `None`
    /// when they cancel.
    fn save_set(&mut self, _suggested: &str) -> Option<PathBuf> {
        None
    }

    /// Opens a web address in the user's browser, after the user has agreed in the window.
    /// The default opens nothing.
    fn open_url(&mut self, _url: &str) -> Result<(), String> {
        Err("opening addresses is not available here".to_owned())
    }
}

/// Whether an address is one a browser may be asked to open: `http`, `https` or `mailto`.
/// Anything else (a `file:` address, a program) is refused.
pub fn url_is_openable(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:"))
        && !url.chars().any(|c| c.is_control())
}

/// The file formats the markups list is exported to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportKind {
    Csv,
    Xlsx,
    Xfdf,
}

impl ExportKind {
    pub const ALL: [ExportKind; 3] = [ExportKind::Csv, ExportKind::Xlsx, ExportKind::Xfdf];

    /// The format's name as menus show it.
    pub fn label(self) -> &'static str {
        match self {
            ExportKind::Csv => "CSV",
            ExportKind::Xlsx => "XLSX",
            ExportKind::Xfdf => "XFDF",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportKind::Csv => "csv",
            ExportKind::Xlsx => "xlsx",
            ExportKind::Xfdf => "xfdf",
        }
    }

    fn filter_name(self) -> &'static str {
        match self {
            ExportKind::Csv => "Comma-separated values",
            ExportKind::Xlsx => "Excel workbook",
            ExportKind::Xfdf => "XFDF markups",
        }
    }
}

/// The operating system's dialogs, through `rfd`. Blocks until the dialog closes.
#[derive(Debug, Default)]
pub struct NativeDialogs;

impl FileDialogs for NativeDialogs {
    fn pick_pdfs(&mut self) -> Vec<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Open PDF")
            .add_filter("PDF documents", &["pdf"])
            .add_filter("All files", &["*"])
            .pick_files()
            .unwrap_or_default()
    }

    fn save_pdf(&mut self, suggested: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Save PDF as")
            .add_filter("PDF documents", &["pdf"])
            .set_file_name(suggested)
            .save_file()
    }

    fn pick_image(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Choose a picture")
            .add_filter("Pictures", &["png", "jpg", "jpeg"])
            .pick_file()
    }

    fn save_export(&mut self, kind: ExportKind, suggested: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(format!("Export markups as {}", kind.label()))
            .add_filter(kind.filter_name(), &[kind.extension()])
            .set_file_name(suggested)
            .save_file()
    }

    fn pick_xfdf(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Import markups from XFDF")
            .add_filter("XFDF markups", &["xfdf"])
            .add_filter("All files", &["*"])
            .pick_file()
    }

    fn pick_set(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("Open drawing set")
            .add_filter("Drawing sets", &[pl_doc::SET_EXTENSION])
            .pick_file()
    }

    fn save_set(&mut self, suggested: &str) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("New drawing set")
            .add_filter("Drawing sets", &[pl_doc::SET_EXTENSION])
            .set_file_name(suggested)
            .save_file()
    }

    fn open_url(&mut self, url: &str) -> Result<(), String> {
        if !url_is_openable(url) {
            return Err(format!("{url} is not a web or mail address"));
        }
        shell::open(url)
    }
}

/// Opening an address with the program Windows has for it.
#[cfg(windows)]
mod shell {
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: *mut core::ffi::c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn open(url: &str) -> Result<(), String> {
        const SW_SHOWNORMAL: i32 = 1;
        let (verb, file) = (wide("open"), wide(url));
        // SAFETY: both strings are NUL-terminated and outlive the call; the other pointers
        // may be null.
        let code = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // Values above 32 mean success.
        if code > 32 {
            Ok(())
        } else {
            Err(format!("Windows could not open the address (code {code})"))
        }
    }
}

#[cfg(not(windows))]
mod shell {
    pub fn open(_url: &str) -> Result<(), String> {
        Err("opening addresses is only supported on Windows".to_owned())
    }
}

/// Dialogs that answer from a prepared list and never show anything. For tests.
#[derive(Debug, Default)]
pub struct ScriptedDialogs {
    pub answers: VecDeque<Vec<PathBuf>>,
    /// Answers to "save as"; when it runs out the dialog is "cancelled".
    pub saves: VecDeque<PathBuf>,
    /// Answers to "choose a picture".
    pub images: VecDeque<PathBuf>,
    /// Answers to "export as", whatever the format.
    pub exports: VecDeque<PathBuf>,
    /// Answers to "import XFDF".
    pub imports: VecDeque<PathBuf>,
    /// Answers to "open drawing set".
    pub sets: VecDeque<PathBuf>,
    /// Answers to "new drawing set".
    pub set_saves: VecDeque<PathBuf>,
    /// The addresses the app asked to open; nothing is ever opened. Clones share the list, so
    /// a test keeps one.
    pub opened_urls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl ScriptedDialogs {
    pub fn new(answers: impl IntoIterator<Item = Vec<PathBuf>>) -> ScriptedDialogs {
        ScriptedDialogs {
            answers: answers.into_iter().collect(),
            ..ScriptedDialogs::default()
        }
    }

    /// Adds answers to the "choose a picture" dialog.
    pub fn with_images(mut self, images: impl IntoIterator<Item = PathBuf>) -> ScriptedDialogs {
        self.images.extend(images);
        self
    }

    /// Adds answers to the export dialogs.
    pub fn with_exports(mut self, exports: impl IntoIterator<Item = PathBuf>) -> ScriptedDialogs {
        self.exports.extend(exports);
        self
    }

    /// Adds answers to the "import XFDF" dialog.
    pub fn with_imports(mut self, imports: impl IntoIterator<Item = PathBuf>) -> ScriptedDialogs {
        self.imports.extend(imports);
        self
    }

    /// Adds answers to the "save as" dialog.
    pub fn with_saves(mut self, saves: impl IntoIterator<Item = PathBuf>) -> ScriptedDialogs {
        self.saves.extend(saves);
        self
    }

    /// Adds answers to the "open drawing set" dialog.
    pub fn with_sets(mut self, sets: impl IntoIterator<Item = PathBuf>) -> ScriptedDialogs {
        self.sets.extend(sets);
        self
    }

    /// Adds answers to the "new drawing set" dialog.
    pub fn with_set_saves(mut self, saves: impl IntoIterator<Item = PathBuf>) -> ScriptedDialogs {
        self.set_saves.extend(saves);
        self
    }
}

impl FileDialogs for ScriptedDialogs {
    fn pick_pdfs(&mut self) -> Vec<PathBuf> {
        self.answers.pop_front().unwrap_or_default()
    }

    fn save_pdf(&mut self, _suggested: &str) -> Option<PathBuf> {
        self.saves.pop_front()
    }

    fn pick_image(&mut self) -> Option<PathBuf> {
        self.images.pop_front()
    }

    fn save_export(&mut self, _kind: ExportKind, _suggested: &str) -> Option<PathBuf> {
        self.exports.pop_front()
    }

    fn pick_xfdf(&mut self) -> Option<PathBuf> {
        self.imports.pop_front()
    }

    fn pick_set(&mut self) -> Option<PathBuf> {
        self.sets.pop_front()
    }

    fn save_set(&mut self, _suggested: &str) -> Option<PathBuf> {
        self.set_saves.pop_front()
    }

    fn open_url(&mut self, url: &str) -> Result<(), String> {
        if let Ok(mut opened) = self.opened_urls.lock() {
            opened.push(url.to_owned());
        }
        Ok(())
    }
}

/// The system clipboard, for what egui's own clipboard commands do not carry: images, and
/// text in a build without egui's clipboard support.
pub trait SystemClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String>;

    /// Puts an image on the clipboard. `rgba` is straight-alpha RGBA8, top row first.
    fn set_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String>;
}

/// The operating system's clipboard, through `arboard`.
#[derive(Debug, Default)]
pub struct NativeClipboard;

impl SystemClipboard for NativeClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        arboard::Clipboard::new()
            .and_then(|mut c| c.set_text(text.to_owned()))
            .map_err(|e| e.to_string())
    }

    fn set_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        let image = arboard::ImageData {
            width: width as usize,
            height: height as usize,
            bytes: std::borrow::Cow::Borrowed(rgba),
        };
        arboard::Clipboard::new()
            .and_then(|mut c| c.set_image(image))
            .map_err(|e| e.to_string())
    }
}

/// What a [`FakeClipboard`] was last given.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipboardContents {
    pub text: Option<String>,
    /// Width, height and RGBA8 pixels.
    pub image: Option<(u32, u32, Vec<u8>)>,
}

/// A clipboard that only remembers, for tests and for a machine with none. Clones share what
/// they hold, so a test keeps one and gives the app the other.
#[derive(Clone, Debug, Default)]
pub struct FakeClipboard(pub std::sync::Arc<std::sync::Mutex<ClipboardContents>>);

impl FakeClipboard {
    pub fn contents(&self) -> ClipboardContents {
        self.0.lock().map(|c| c.clone()).unwrap_or_default()
    }
}

impl SystemClipboard for FakeClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        let mut held = self.0.lock().map_err(|e| e.to_string())?;
        held.text = Some(text.to_owned());
        held.image = None;
        Ok(())
    }

    fn set_image(&mut self, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
        if rgba.len() != width as usize * height as usize * 4 {
            return Err("the image size does not match its pixels".to_owned());
        }
        let mut held = self.0.lock().map_err(|e| e.to_string())?;
        held.image = Some((width, height, rgba.to_vec()));
        held.text = None;
        Ok(())
    }
}

/// A backend that fails every request with the reason PDFium could not be loaded, so the app
/// can still start and say what is wrong.
#[derive(Debug)]
pub struct UnavailableBackend(pub String);

impl RenderBackend for UnavailableBackend {
    fn call(&self, _request: Request) -> Result<Reply, RenderError> {
        Err(RenderError::LibraryLoad(self.0.clone()))
    }
}
