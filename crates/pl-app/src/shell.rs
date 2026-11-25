//! The `eframe` wrapper: a native window around [`AppState`]. Only [`pick_adapter`] is reachable
//! from the tests, which never open a window.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use pl_doc::pl_render::{PdfiumBackend, RenderBackend};

use crate::app::{AppConfig, AppState};
use crate::platform::{NativeClipboard, NativeDialogs, UnavailableBackend};
use crate::startup::{log, set_renderer_in_use, ReadySignal, RendererChoice};

use eframe::wgpu;

struct WindowApp {
    state: AppState,
    /// Written once the first frame is on screen.
    ready: ReadySignal,
    renderer: &'static str,
}

impl eframe::App for WindowApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // A frame is painted right after the pass that built it, so once one pass has completed
        // and the next has begun, the first frame is on screen (eframe also shows the window
        // only then). Ask for that second pass in case nothing else does. A frame that wgpu
        // skipped for want of a surface texture was not on screen: wait for one that was,
        // asking again at a gentle rate (the launcher gives up after its own limit).
        if !self.ready.is_done() {
            let skipped = SURFACE_TROUBLE.swap(false, Ordering::Relaxed);
            if ui.ctx().cumulative_frame_nr() >= 1 && !skipped {
                self.ready.signal(self.renderer);
            } else if skipped {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            } else {
                ui.ctx().request_repaint();
            }
        }
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

/// Which adapter of `adapters` (device type, and whether it can present to the window) the
/// wgpu renderer uses: for hardware the first discrete GPU, else the first integrated, virtual
/// or other one, never a CPU (software) adapter; for software the first CPU adapter (WARP).
pub fn pick_adapter(adapters: &[(wgpu::DeviceType, bool)], software: bool) -> Option<usize> {
    use wgpu::DeviceType as T;
    let rank = |t: T| match t {
        T::DiscreteGpu => 0,
        T::IntegratedGpu => 1,
        T::VirtualGpu => 2,
        T::Other => 3,
        T::Cpu => 4,
    };
    adapters
        .iter()
        .enumerate()
        .filter(|(_, &(t, presents))| presents && ((t == T::Cpu) == software))
        .min_by_key(|(i, &(t, _))| (rank(t), *i))
        .map(|(i, _)| i)
}

/// Why [`pick_adapter`] found nothing in `adapters`.
fn no_adapter_reason(adapters: &[(wgpu::DeviceType, bool)], software: bool) -> &'static str {
    if software {
        "no Direct3D 12 software (WARP) adapter was found"
    } else if adapters.is_empty() {
        "no Direct3D 12 adapter was found"
    } else if adapters.iter().all(|&(t, _)| t == wgpu::DeviceType::Cpu) {
        // WARP is always listed; the GPU is missing when its driver failed to create a device.
        "no Direct3D 12 hardware adapter was found (only the software one)"
    } else {
        "no Direct3D 12 graphics adapter could draw to the window"
    }
}

fn describe_adapter(info: &wgpu::AdapterInfo) -> String {
    format!(
        "{} ({:?}, {:?}; driver {} {})",
        info.name, info.backend, info.device_type, info.driver, info.driver_info
    )
}

/// wgpu limited to Direct3D 12: no Vulkan (buggy drivers, and overlays' implicit layers load
/// into the app) and no wgpu OpenGL (glow is the OpenGL renderer). `software` picks WARP, the
/// software rasteriser Windows ships, as `force_fallback_adapter` would (egui-wgpu does not
/// expose that flag; its adapter selector does the same). WARP does not draw with the GPU's
/// driver, but listing the adapters still creates a Direct3D 12 device on every one of them
/// (wgpu-hal asks each for its capabilities), so a driver that crashes even there takes this
/// renderer down too; glow is the only one that never loads the Direct3D 12 driver.
fn wgpu_options(software: bool) -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{WgpuConfiguration, WgpuSetup, WgpuSetupCreateNew};
    let mut setup = WgpuSetupCreateNew::without_display_handle();
    setup.instance_descriptor.backends = if cfg!(windows) {
        wgpu::Backends::DX12
    } else {
        wgpu::Backends::PRIMARY
    };
    setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
        let found: Vec<_> = adapters
            .iter()
            .map(|a| {
                let info = a.get_info();
                let presents = surface.is_none_or(|s| a.is_surface_supported(s));
                log(&format!(
                    "wgpu adapter: {}{}",
                    describe_adapter(&info),
                    if presents {
                        ""
                    } else {
                        ", cannot present to the window"
                    }
                ));
                (info.device_type, presents)
            })
            .collect();
        match pick_adapter(&found, software) {
            Some(i) => Ok(adapters[i].clone()),
            None => Err(no_adapter_reason(&found, software).into()),
        }
    }));
    let default = WgpuConfiguration::default();
    let on_status = default.on_surface_status.clone();
    WgpuConfiguration {
        wgpu_setup: WgpuSetup::CreateNew(setup),
        // egui-wgpu skips a frame silently when it gets no surface texture; note it, so the
        // first frame counts as painted only once one was really presented.
        on_surface_status: Arc::new(move |status| {
            SURFACE_TROUBLE.store(true, Ordering::Relaxed);
            if !SURFACE_TROUBLE_LOGGED.swap(true, Ordering::Relaxed) {
                log(&format!(
                    "wgpu: no surface texture for a frame ({status:?})"
                ));
            }
            on_status(status)
        }),
        ..default
    }
}

/// Set when a wgpu frame was skipped for want of a surface texture; cleared by [`WindowApp`].
static SURFACE_TROUBLE: AtomicBool = AtomicBool::new(false);
/// Only the first skipped frame of a run is logged (a minimised window skips every frame).
static SURFACE_TROUBLE_LOGGED: AtomicBool = AtomicBool::new(false);

fn options(choice: RendererChoice) -> eframe::NativeOptions {
    let (renderer, wgpu_options) = match choice {
        RendererChoice::Glow => (eframe::Renderer::Glow, Default::default()),
        RendererChoice::Software => (eframe::Renderer::Wgpu, wgpu_options(true)),
        RendererChoice::Wgpu | RendererChoice::Auto => {
            (eframe::Renderer::Wgpu, wgpu_options(false))
        }
    };
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Plumbline")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([640.0, 420.0])
            .with_drag_and_drop(true),
        renderer,
        wgpu_options,
        ..eframe::NativeOptions::default()
    }
}

fn run_with(
    choice: RendererChoice,
    files: &[PathBuf],
    ready_file: &Option<PathBuf>,
) -> eframe::Result {
    let files = files.to_vec();
    let ready = ReadySignal::new(ready_file.clone());
    log(&format!("starting the {} renderer", choice.label()));
    set_renderer_in_use(Some(choice));
    eframe::run_native(
        "Plumbline",
        options(choice),
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
            // The system's printers: the only place the app is given real ones.
            #[cfg(windows)]
            state.set_printers(Box::new(crate::print_win32::WinPrinters));
            state.set_clipboard(Box::new(NativeClipboard));
            if let Some(problem) = problem {
                log(&problem);
                state.chrome.status = Some(problem);
            }
            for file in &files {
                state.open_path(file);
            }
            Ok(Box::new(WindowApp {
                state,
                ready,
                renderer: choice.label(),
            }))
        }),
    )
}

/// The renderer that created the window, with the wgpu adapter when there is one.
fn started(cc: &eframe::CreationContext<'_>) -> String {
    if let Some(wgpu) = &cc.wgpu_render_state {
        format!("wgpu on {}", describe_adapter(&wgpu.adapter.get_info()))
    } else if cc.gl.is_some() {
        "glow (OpenGL)".to_owned()
    } else {
        "unknown".to_owned()
    }
}

/// One renderer's attempt. A renderer can panic instead of returning an error while it starts
/// (wgpu on an unusual driver, for example); a panic before the window reached the app counts
/// as a failed start, so the next renderer still gets its turn. A later panic is passed on.
fn attempt(
    choice: RendererChoice,
    files: &[PathBuf],
    ready_file: &Option<PathBuf>,
) -> Result<(), String> {
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_with(choice, files, ready_file)
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

/// Opens the window and runs until it is closed. With [`RendererChoice::Auto`] wgpu on a
/// Direct3D 12 GPU is tried first, then glow (OpenGL), then the software renderer, each only
/// if the one before could not start (an error, or a panic before the window reached the app);
/// the other choices try only that renderer. Each attempt is written to the startup log, and
/// `ready_file` (from `--ready-file`) is created once the first frame is painted.
pub fn run(
    files: Vec<PathBuf>,
    choice: RendererChoice,
    ready_file: Option<PathBuf>,
) -> Result<(), String> {
    let attempts = choice.attempts();
    let mut errors = Vec::new();
    for (i, &renderer) in attempts.iter().enumerate() {
        match attempt(renderer, &files, &ready_file) {
            Ok(()) => {
                set_renderer_in_use(None);
                return Ok(());
            }
            Err(e) => {
                // Once a window has reached the app, a failure is not a failed start: opening
                // another renderer's window after it would surprise the user.
                let started = RENDERER_STARTED.load(Ordering::Relaxed);
                let next = attempts
                    .get(i + 1)
                    .filter(|_| !started)
                    .map(|n| format!("; trying {}", n.label()))
                    .unwrap_or_default();
                log(&format!(
                    "the {} renderer failed: {e}{next}",
                    renderer.label()
                ));
                errors.push((renderer, e));
                if started {
                    break;
                }
            }
        }
    }
    set_renderer_in_use(None);
    Err(match errors.as_slice() {
        [(renderer, e)] => format!("the {} renderer could not start ({e})", renderer.label()),
        all => format!(
            "no renderer could start ({})",
            all.iter()
                .map(|(r, e)| format!("{}: {e}", r.label()))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::DeviceType as T;

    #[test]
    fn hardware_prefers_a_discrete_gpu_and_never_takes_warp() {
        let list = [
            (T::Cpu, true),
            (T::IntegratedGpu, true),
            (T::DiscreteGpu, true),
        ];
        assert_eq!(pick_adapter(&list, false), Some(2));
        let list = [(T::Cpu, true), (T::IntegratedGpu, true)];
        assert_eq!(pick_adapter(&list, false), Some(1));
        // A GPU that cannot present to the window is skipped.
        let list = [(T::DiscreteGpu, false), (T::IntegratedGpu, true)];
        assert_eq!(pick_adapter(&list, false), Some(1));
        assert_eq!(pick_adapter(&[(T::Cpu, true)], false), None);
        assert_eq!(pick_adapter(&[], false), None);
    }

    #[test]
    fn software_takes_only_the_cpu_adapter() {
        let list = [(T::IntegratedGpu, true), (T::Cpu, true)];
        assert_eq!(pick_adapter(&list, true), Some(1));
        assert_eq!(pick_adapter(&[(T::IntegratedGpu, true)], true), None);
        assert_eq!(pick_adapter(&[(T::Cpu, false)], true), None);
    }

    #[test]
    fn no_adapter_reasons() {
        assert!(no_adapter_reason(&[], true).contains("WARP"));
        assert_eq!(
            no_adapter_reason(&[], false),
            "no Direct3D 12 adapter was found"
        );
        assert!(no_adapter_reason(&[(T::Cpu, true)], false).contains("only the software one"));
        assert!(
            no_adapter_reason(&[(T::Cpu, true), (T::IntegratedGpu, false)], false)
                .contains("could draw to the window")
        );
    }
}
