//! Plumbline's window: the only crate that uses egui.
//!
//! Everything the window does is in [`AppState`] and its [`AppState::ui`] function, which take
//! an injected render backend and file dialogs. A frame never waits for the backend: tiles,
//! thumbnails, page text, page geometry and the outline are all scheduler jobs whose results
//! are picked up at the start of a later frame ([`queries`]). The binary wraps them in an `eframe` app
//! ([`shell`]); the tests drive the same state with `egui_kittest` and no window.
//!
//! Markup editing sits on top of the viewer: [`markup`] owns the active tool and turns pointer
//! and key events into pl-tools inputs and its intents into pl-doc commands; [`paint`] draws
//! a markup's `DrawList` with the egui painter; [`overlay`] composites Multiply markups
//! against the page tiles and paints the selection; [`panels`] is the tool bar, the
//! properties panel and the dialogs; [`textedit`] is the in-place editor of text boxes and the
//! note window; [`list`] is the markups list at the bottom of the window. Annotation commands
//! go through pl-doc on the frame that asks for them; saving a document and the first read of
//! its annotations and layers run on worker threads ([`jobs`]) and are taken in by a later
//! frame.
//!
//! Milestone M8 adds the document's structure: [`pages`] (page operations on the thumbnails,
//! "Combine files"), [`bookmarks`] (the editable bookmarks), [`links`] (following and making
//! links, Back and Forward), [`structure`] (the glue to pl-doc's structural commands, the
//! confirmations and the flatten dialog), [`setpanel`] (drawing sets and set search) and
//! [`print`] (the print panel and the banded print job; [`print_win32`] reaches the Windows
//! printers, and only the window's start-up gives the app those).

pub mod app;
pub mod bookmarks;
pub mod canvas;
pub mod chrome;
pub mod commands;
pub mod icons;
pub mod jobs;
pub mod links;
pub mod list;
pub mod markup;
pub mod measure;
pub mod overlay;
pub mod pages;
pub mod paint;
pub mod panels;
pub mod platform;
pub mod print;
#[cfg(windows)]
pub mod print_win32;
pub mod queries;
pub mod setpanel;
pub mod shell;
pub mod startup;
pub mod structure;
pub mod textedit;
pub mod textures;
pub mod theme;

pub use app::{AppConfig, AppState, DocView, LengthUnit, SidePage, StatusInfo, ViewId};
pub use commands::{Arrange, Command};
pub use links::LinkKind;
pub use list::{ListLineView, ListStats, LIST_FILE_NAME, ROW_HEIGHT};
pub use markup::{CloseChoice, SaveHow, Tool};
pub use measure::SnapToggles;
pub use platform::{
    ClipboardContents, ExportKind, FakeClipboard, FileDialogs, NativeClipboard, NativeDialogs,
    ScriptedDialogs, SystemClipboard, UnavailableBackend,
};
pub use print::{FakePrintLog, FakePrinters, NoPrinters, PageChoice, PrinterInfo, Printers};
#[cfg(windows)]
pub use print_win32::WinPrinters;
pub use structure::FlattenChoice;

/// The whole interface for one frame. This is what both `eframe` and the test harness call.
pub fn ui(ui: &mut egui::Ui, state: &mut AppState) {
    state.ui(ui);
}
