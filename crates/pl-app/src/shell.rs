//! The `eframe` wrapper: a native window around [`AppState`]. Nothing in here is reachable from
//! the tests, which never open a window.

use std::path::PathBuf;
use std::sync::Arc;

use pl_doc::pl_render::{PdfiumBackend, RenderBackend};

use crate::app::{AppConfig, AppState};
use crate::platform::{NativeClipboard, NativeDialogs, UnavailableBackend};

struct WindowApp {
    state: AppState,
}

impl eframe::App for WindowApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.state.ui(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.state.shutdown();
    }
}

/// Loads PDFium from beside the executable (or, in a debug build, from the vendored copy).
/// When it cannot be loaded the app still starts, and every open reports why.
pub fn load_backend() -> (Arc<dyn RenderBackend>, Option<String>) {
    match PdfiumBackend::new(None) {
        Ok(backend) => (Arc::new(backend), None),
        Err(e) => {
            let message = format!("PDFium could not be loaded: {e}");
            (Arc::new(UnavailableBackend(message.clone())), Some(message))
        }
    }
}

fn options(renderer: eframe::Renderer) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Plumbline")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true),
        renderer,
        ..eframe::NativeOptions::default()
    }
}

fn run_with(renderer: eframe::Renderer, files: &[PathBuf]) -> eframe::Result {
    let files = files.to_vec();
    eframe::run_native(
        "Plumbline",
        options(renderer),
        Box::new(move |cc| {
            crate::theme::install(&cc.egui_ctx);
            let (backend, problem) = load_backend();
            let config = AppConfig {
                data_dir: pl_doc::default_data_dir(),
                ..AppConfig::default()
            };
            let mut state = AppState::new(backend, config, Box::new(NativeDialogs));
            state.set_clipboard(Box::new(NativeClipboard));
            if let Some(problem) = problem {
                state.chrome.status = Some(problem);
            }
            for file in &files {
                state.open_path(file);
            }
            Ok(Box::new(WindowApp { state }))
        }),
    )
}

/// Opens the window and runs until it is closed. The wgpu renderer is tried first; if it
/// cannot start, the glow (OpenGL) renderer is tried before giving up.
pub fn run(files: Vec<PathBuf>) -> Result<(), String> {
    match run_with(eframe::Renderer::Wgpu, &files) {
        Ok(()) => Ok(()),
        Err(wgpu_error) => run_with(eframe::Renderer::Glow, &files).map_err(|glow_error| {
            format!("no renderer could start (wgpu: {wgpu_error}; glow: {glow_error})")
        }),
    }
}
