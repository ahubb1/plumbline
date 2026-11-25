//! The `eframe` wrapper: a native window around [`AppState`]. Nothing in here is reachable from
//! the tests, which never open a window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pl_doc::pl_render::{PdfiumBackend, RenderBackend};

use crate::app::{AppConfig, AppState};
use crate::platform::{NativeClipboard, NativeDialogs, UnavailableBackend};
use crate::startup::{log, RendererChoice};

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

/// Set once a renderer has created the window and handed it to the app. A wgpu panic before
/// that is a failed start and falls back to OpenGL; a panic after it is a crash of the app.
static RENDERER_STARTED: AtomicBool = AtomicBool::new(false);

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
    log(&format!("starting the {} renderer", name(renderer)));
    eframe::run_native(
        "Plumbline",
        options(renderer),
        Box::new(move |cc| {
            RENDERER_STARTED.store(true, Ordering::Relaxed);
            log(&format!("renderer started: {}", started(cc)));
            crate::theme::install(&cc.egui_ctx);
            let (backend, problem) = load_backend();
            let config = AppConfig {
                data_dir: pl_doc::default_data_dir(),
                ..AppConfig::default()
            };
            let mut state = AppState::new(backend, config, Box::new(NativeDialogs));
            state.set_clipboard(Box::new(NativeClipboard));
            if let Some(problem) = problem {
                log(&problem);
                state.chrome.status = Some(problem);
            }
            for file in &files {
                state.open_path(file);
            }
            Ok(Box::new(WindowApp { state }))
        }),
    )
}

fn name(renderer: eframe::Renderer) -> &'static str {
    match renderer {
        eframe::Renderer::Glow => "glow (OpenGL)",
        eframe::Renderer::Wgpu => "wgpu",
    }
}

/// The renderer that created the window, with the wgpu adapter when there is one.
fn started(cc: &eframe::CreationContext<'_>) -> String {
    if let Some(wgpu) = &cc.wgpu_render_state {
        let info = wgpu.adapter.get_info();
        format!(
            "wgpu, {:?} on {} ({:?})",
            info.backend, info.name, info.device_type
        )
    } else if cc.gl.is_some() {
        "glow (OpenGL)".to_owned()
    } else {
        "unknown".to_owned()
    }
}

/// The wgpu attempt of [`RendererChoice::Auto`]. wgpu can panic instead of returning an error
/// while it starts (on an unusual driver, for example); a panic before the window reached the
/// app counts as a failed start, so OpenGL still gets its turn. A later panic is passed on.
fn wgpu_attempt(files: &[PathBuf]) -> Result<(), String> {
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_with(eframe::Renderer::Wgpu, files)
    }));
    match attempt {
        Ok(result) => result.map_err(|e| e.to_string()),
        Err(payload) if !RENDERER_STARTED.load(Ordering::Relaxed) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".to_owned());
            Err(format!("it panicked while starting: {message}"))
        }
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Opens the window and runs until it is closed. With [`RendererChoice::Auto`] the wgpu
/// renderer is tried first and, if it cannot start (an error, or a panic before the window
/// reached the app), the glow (OpenGL) renderer before giving up; the other choices try only
/// that renderer. Each attempt is written to the startup log.
pub fn run(files: Vec<PathBuf>, choice: RendererChoice) -> Result<(), String> {
    let only = |renderer| {
        run_with(renderer, &files).map_err(|e| {
            log(&format!("the {} renderer failed: {e}", name(renderer)));
            format!("the {} renderer could not start ({e})", name(renderer))
        })
    };
    match choice {
        RendererChoice::Wgpu => only(eframe::Renderer::Wgpu),
        RendererChoice::Glow => only(eframe::Renderer::Glow),
        RendererChoice::Auto => match wgpu_attempt(&files) {
            Ok(()) => Ok(()),
            Err(wgpu_error) => {
                log(&format!(
                    "the wgpu renderer failed: {wgpu_error}; trying glow (OpenGL)"
                ));
                run_with(eframe::Renderer::Glow, &files).map_err(|glow_error| {
                    log(&format!("the glow (OpenGL) renderer failed: {glow_error}"));
                    format!("no renderer could start (wgpu: {wgpu_error}; glow: {glow_error})")
                })
            }
        },
    }
}
