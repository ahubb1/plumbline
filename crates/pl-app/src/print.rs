//! Printing (Ctrl+P): the window's own print panel (not the Windows print dialog), and the
//! print job on a worker thread.
//!
//! The panel picks a printer, the pages (all, the page in view, or a range), fit or actual
//! size, whether markups print, and the number of copies; the orientation is chosen per page
//! (a wide sheet turns onto portrait paper when it prints larger that way). It shows where the
//! first page lands on the paper.
//!
//! The job renders each page in bands through pl-render's [`print_job`] into a
//! [`PrintSink`]. Which printers exist and the sink that reaches one come from a [`Printers`]
//! implementation given to the app: the window gets the Windows printers
//! ([`crate::print_win32`]); [`AppState::new`] starts with [`NoPrinters`], so a test (or
//! anything else that does not ask for them) can never reach a real printer. Tests use
//! [`FakePrinters`], which records what it is sent.
//!
//! What prints is the document as it stands: its own file when nothing is unsaved, otherwise
//! the bytes a save would write ([`pl_doc::Document::print_source`]), opened as a separate
//! render document for the job and closed afterwards.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

use egui::{Id, RichText, Stroke, StrokeKind};
use pl_doc::pl_render::{
    layout_page, parse_page_range, print_job, Band, BandFormat, DocSource, JobStart, PageInfo,
    Paper, PrintJob, PrintReport, PrintScaling, PrintSink, SheetStart, SinkError,
};
use pl_doc::{DocumentId, PrintSource};

use crate::app::AppState;
use crate::theme::{self, Palette};

/// One printer, as the system names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrinterInfo {
    pub name: String,
    /// The system's default printer.
    pub default: bool,
}

/// The printers of the machine, and a way to send a job to one.
pub trait Printers: Send + Sync {
    /// The printers there are, the default first.
    fn list(&self) -> Vec<PrinterInfo>;
    /// The sheet a printer prints on now (its default paper and resolution).
    fn paper(&self, printer: &str) -> Result<Paper, String>;
    /// Starts talking to a printer for one job named `title`. The sink is used from a worker
    /// thread.
    fn open(&self, printer: &str, title: &str) -> Result<Box<dyn PrintSink + Send>, String>;
}

/// No printers at all: what the app has until it is given real ones.
#[derive(Debug, Default)]
pub struct NoPrinters;

impl Printers for NoPrinters {
    fn list(&self) -> Vec<PrinterInfo> {
        Vec::new()
    }

    fn paper(&self, printer: &str) -> Result<Paper, String> {
        Err(format!("there is no printer {printer}"))
    }

    fn open(&self, printer: &str, _title: &str) -> Result<Box<dyn PrintSink + Send>, String> {
        Err(format!("there is no printer {printer}"))
    }
}

/// What a [`FakePrinters`] was sent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FakePrintLog {
    /// `(printer, title)` of every job opened.
    pub opened: Vec<(String, String)>,
    /// Every sink call, in order: `begin_job`, `begin_page`, `band`, `end_page`, `end_job`,
    /// `abort`.
    pub calls: Vec<&'static str>,
    /// The page of every sheet, in order.
    pub pages: Vec<u32>,
    /// `(sheet, band index, y, height)` of every band.
    pub bands: Vec<(usize, u32, u32, u32)>,
    /// Bands whose pixels were not all white: something was drawn there.
    pub inked_bands: usize,
}

/// Printers that print nowhere and remember what they were sent. Clones share the log.
#[derive(Clone, Debug)]
pub struct FakePrinters {
    pub log: Arc<Mutex<FakePrintLog>>,
    pub printers: Vec<PrinterInfo>,
    pub paper: Paper,
    /// Stops each job after this many bands, as a user's cancel would.
    pub cancel_after: Option<usize>,
    /// Each band takes this long to send (a slow printer).
    pub band_delay: Option<std::time::Duration>,
    /// How many times a printer's paper was asked for.
    pub paper_queries: Arc<AtomicU32>,
}

impl FakePrinters {
    /// Two printers with Letter paper at `dpi` and quarter-inch margins.
    pub fn new(dpi: f64) -> FakePrinters {
        FakePrinters {
            log: Arc::default(),
            printers: vec![
                PrinterInfo {
                    name: "Test printer".into(),
                    default: true,
                },
                PrinterInfo {
                    name: "Second test printer".into(),
                    default: false,
                },
            ],
            paper: Paper::from_inches(8.5, 11.0, dpi, 0.25),
            cancel_after: None,
            band_delay: None,
            paper_queries: Arc::default(),
        }
    }

    pub fn log(&self) -> FakePrintLog {
        self.log.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Printers for FakePrinters {
    fn list(&self) -> Vec<PrinterInfo> {
        self.printers.clone()
    }

    fn paper(&self, printer: &str) -> Result<Paper, String> {
        self.paper_queries.fetch_add(1, Ordering::AcqRel);
        if self.printers.iter().any(|p| p.name == printer) {
            Ok(self.paper)
        } else {
            Err(format!("there is no printer {printer}"))
        }
    }

    fn open(&self, printer: &str, title: &str) -> Result<Box<dyn PrintSink + Send>, String> {
        if !self.printers.iter().any(|p| p.name == printer) {
            return Err(format!("there is no printer {printer}"));
        }
        if let Ok(mut log) = self.log.lock() {
            log.opened.push((printer.to_owned(), title.to_owned()));
        }
        Ok(Box::new(FakeSink {
            log: self.log.clone(),
            cancel_after: self.cancel_after,
            delay: self.band_delay,
            bands: 0,
        }))
    }
}

struct FakeSink {
    log: Arc<Mutex<FakePrintLog>>,
    cancel_after: Option<usize>,
    delay: Option<std::time::Duration>,
    bands: usize,
}

impl FakeSink {
    fn call(&self, name: &'static str) {
        if let Ok(mut log) = self.log.lock() {
            log.calls.push(name);
        }
    }
}

impl PrintSink for FakeSink {
    fn begin_job(&mut self, _job: &JobStart<'_>) -> Result<(), SinkError> {
        self.call("begin_job");
        Ok(())
    }

    fn begin_page(&mut self, sheet: &SheetStart) -> Result<(), SinkError> {
        self.call("begin_page");
        if let Ok(mut log) = self.log.lock() {
            log.pages.push(sheet.placement.page);
        }
        Ok(())
    }

    fn band(&mut self, band: &Band<'_>) -> Result<(), SinkError> {
        if self.cancel_after.is_some_and(|n| self.bands >= n) {
            return Err(SinkError::Cancelled);
        }
        if let Some(delay) = self.delay {
            std::thread::sleep(delay);
        }
        self.bands += 1;
        self.call("band");
        let bpp = band.format.bytes_per_pixel();
        let inked = (0..band.height as usize).any(|row| {
            let start = row * band.stride;
            band.data[start..start + band.width as usize * bpp]
                .iter()
                .any(|b| *b < 250)
        });
        if let Ok(mut log) = self.log.lock() {
            log.bands
                .push((band.sheet, band.index, band.y, band.height));
            if inked {
                log.inked_bands += 1;
            }
        }
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), SinkError> {
        self.call("end_page");
        Ok(())
    }

    fn end_job(&mut self) -> Result<(), SinkError> {
        self.call("end_job");
        Ok(())
    }

    fn abort(&mut self) {
        self.call("abort");
    }
}

/// Which pages the panel prints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageChoice {
    All,
    Current,
    Range,
}

/// A print job running on a worker thread.
pub(crate) struct RunningPrint {
    pub cancel: Arc<AtomicBool>,
    /// Bands sent so far, and the bands of the whole job.
    pub done: Arc<AtomicU32>,
    pub total: u32,
    pub rx: Receiver<Result<PrintReport, String>>,
    pub printer: String,
    pub sheets: usize,
}

/// The print panel's choices, and the job it started.
pub(crate) struct PrintUi {
    pub open: bool,
    pub printers: Arc<dyn Printers>,
    /// Each printer's paper as last asked for: asking a printer driver can take a while, so
    /// it is asked once per printer while the panel is open, not on every frame.
    pub papers: Mutex<std::collections::HashMap<String, Result<Paper, String>>>,
    pub list: Option<Vec<PrinterInfo>>,
    pub printer: usize,
    pub pages: PageChoice,
    pub range: String,
    pub scaling: PrintScaling,
    pub markups: bool,
    pub copies: u32,
    pub error: Option<String>,
    pub running: Option<RunningPrint>,
    /// The job last sent (for the tests and the status bar).
    pub last_job: Option<PrintJob>,
}

impl Default for PrintUi {
    fn default() -> Self {
        PrintUi {
            open: false,
            printers: Arc::new(NoPrinters),
            papers: Mutex::default(),
            list: None,
            printer: 0,
            pages: PageChoice::All,
            range: String::new(),
            scaling: PrintScaling::Fit,
            markups: true,
            copies: 1,
            error: None,
            running: None,
            last_job: None,
        }
    }
}

/// A sink that counts bands and stops when asked, around the printer's own sink.
struct Watched {
    inner: Box<dyn PrintSink + Send>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicU32>,
}

impl PrintSink for Watched {
    fn begin_job(&mut self, job: &JobStart<'_>) -> Result<(), SinkError> {
        self.inner.begin_job(job)
    }

    fn begin_page(&mut self, sheet: &SheetStart) -> Result<(), SinkError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(SinkError::Cancelled);
        }
        self.inner.begin_page(sheet)
    }

    fn band(&mut self, band: &Band<'_>) -> Result<(), SinkError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(SinkError::Cancelled);
        }
        self.inner.band(band)?;
        self.done.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), SinkError> {
        self.inner.end_page()
    }

    fn end_job(&mut self) -> Result<(), SinkError> {
        self.inner.end_job()
    }

    fn abort(&mut self) {
        self.inner.abort();
    }
}

/// The page as pl-render's layout wants it, from the view's own geometry.
fn page_info(geometry: &pl_doc::pl_geom::PageGeometry) -> PageInfo {
    let r = |r: pl_doc::pl_geom::Rect| [r.x0, r.y0, r.x1, r.y1];
    PageInfo {
        media_box: r(geometry.media_box()),
        crop_box: Some(r(geometry.crop_box())),
        rotate: geometry.rotate().degrees(),
        user_unit: geometry.user_unit(),
    }
}

impl AppState {
    /// Gives the app the printers it prints to (the window gives it the system's).
    pub fn set_printers(&mut self, printers: Box<dyn Printers>) {
        self.print.printers = Arc::from(printers);
        self.print.list = None;
        self.forget_papers();
    }

    fn forget_papers(&mut self) {
        if let Ok(mut papers) = self.print.papers.lock() {
            papers.clear();
        }
    }

    /// The paper of a printer, asked for once per printer while the panel is open.
    fn paper_of(&self, printer: &str) -> Result<Paper, String> {
        if let Ok(papers) = self.print.papers.lock() {
            if let Some(known) = papers.get(printer) {
                return known.clone();
            }
        }
        let paper = self.print.printers.paper(printer);
        if let Ok(mut papers) = self.print.papers.lock() {
            papers.insert(printer.to_owned(), paper.clone());
        }
        paper
    }

    /// Shows the print panel.
    pub fn open_print_panel(&mut self) {
        if self.active.is_none() {
            return;
        }
        self.print.open = true;
        self.print.error = None;
        // A printer's paper may have changed since the panel was last open.
        self.forget_papers();
        let list = self.print.printers.list();
        self.print.printer = list.iter().position(|p| p.default).unwrap_or(0);
        self.print.list = Some(list);
    }

    pub fn print_panel_open(&self) -> bool {
        self.print.open
    }

    /// Sets the panel's choices (as its controls do).
    pub fn set_print_options(
        &mut self,
        pages: PageChoice,
        range: &str,
        scaling: PrintScaling,
        markups: bool,
        copies: u32,
    ) {
        self.print.pages = pages;
        self.print.range = range.to_owned();
        self.print.scaling = scaling;
        self.print.markups = markups;
        self.print.copies = copies.clamp(1, 99);
    }

    /// The printer the panel has chosen.
    pub fn chosen_printer(&self) -> Option<String> {
        self.print
            .list
            .as_ref()
            .and_then(|l| l.get(self.print.printer))
            .map(|p| p.name.clone())
    }

    /// The job the panel's choices make, for the chosen printer.
    pub fn build_print_job(&self) -> Result<PrintJob, String> {
        let dv = self.active_view().ok_or("there is no document")?;
        let count = dv.view.page_count();
        let printer = self
            .chosen_printer()
            .ok_or("there is no printer to print to")?;
        let paper = self.paper_of(&printer)?;
        let pages = match self.print.pages {
            PageChoice::All => (0..count).collect(),
            PageChoice::Current => vec![dv.shown_page()],
            PageChoice::Range => {
                if self.print.range.trim().is_empty() {
                    return Err("Give the pages to print, such as 1-3, 5.".into());
                }
                parse_page_range(&self.print.range, count).map_err(|e| e.to_string())?
            }
        };
        let copies = self.print.copies.max(1) as usize;
        let mut all = Vec::with_capacity(pages.len() * copies);
        for _ in 0..copies {
            all.extend_from_slice(&pages);
        }
        let mut job = PrintJob::new(all, paper);
        job.scaling = self.print.scaling;
        job.annotations = self.print.markups;
        // 24-bit rows are what every printer driver takes.
        job.format = BandFormat::Bgr8;
        job.validate(count).map_err(|e| e.to_string())?;
        Ok(job)
    }

    /// Starts printing what the panel says on a worker thread. Returns whether it started.
    pub fn start_print(&mut self) -> bool {
        if self.print.running.is_some() {
            return false;
        }
        let job = match self.build_print_job() {
            Ok(job) => job,
            Err(e) => {
                self.print.error = Some(e);
                return false;
            }
        };
        let Some(doc_id) = self.active_doc_id() else {
            return false;
        };
        let printer = self.chosen_printer().unwrap_or_default();
        match self.spawn_print(doc_id, &printer, job.clone()) {
            Ok(()) => {
                self.print.last_job = Some(job);
                self.print.open = false;
                self.print.error = None;
                true
            }
            Err(e) => {
                self.print.error = Some(e);
                false
            }
        }
    }

    fn spawn_print(
        &mut self,
        doc_id: DocumentId,
        printer: &str,
        job: PrintJob,
    ) -> Result<(), String> {
        let backend = self.session.backend().clone();
        let doc = self
            .session
            .document_mut(doc_id)
            .map_err(|e| e.to_string())?;
        if doc.is_saving() {
            return Err("The document is being saved; print it in a moment.".into());
        }
        let title = doc.title();
        let units: Vec<f64> = doc.pages().iter().map(|p| p.user_unit()).collect();
        let source = doc.print_source().map_err(|e| e.to_string())?;
        let password = doc.open_password().map(str::to_owned);
        let source = match source {
            PrintSource::File(path) => DocSource::file(path),
            PrintSource::Bytes(bytes) => DocSource::Bytes(bytes),
        };
        let geometry: Vec<_> = self
            .active_view()
            .map(|dv| {
                job.pages
                    .iter()
                    .map(|p| dv.view.file_geometry(*p).copied())
                    .collect()
            })
            .unwrap_or_default();
        let total: u32 = job
            .pages
            .iter()
            .zip(&geometry)
            .map(|(page, g)| match g {
                Some(g) => layout_page(*page, &page_info(g), &job)
                    .bands(&job.paper, job.band_rows)
                    .len() as u32,
                None => 1,
            })
            .sum();
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicU32::new(0));
        let (tx, rx) = channel();
        let sheets = job.pages.len();
        let repaint = self.repaint_context();
        let printers = self.print.printers.clone();
        let (thread_cancel, thread_done, thread_printer) =
            (cancel.clone(), done.clone(), printer.to_owned());
        // Opening the document and the printer (a driver that may be slow to answer) happen
        // on the worker too, so the window never waits for them.
        std::thread::Builder::new()
            .name("plumbline-print".into())
            .spawn(move || {
                let run = || -> Result<PrintReport, String> {
                    let info = backend.open(source, password.as_deref()).map_err(|e| {
                        format!("the document could not be opened for printing: {e}")
                    })?;
                    for (page, unit) in units.iter().enumerate() {
                        if (*unit - 1.0).abs() > 1e-9 {
                            let _ = backend.set_page_user_unit(info.doc, page as u32, *unit);
                        }
                    }
                    let sink = match printers.open(&thread_printer, &title) {
                        Ok(sink) => sink,
                        Err(e) => {
                            let _ = backend.close(info.doc);
                            return Err(format!("the printer could not be reached: {e}"));
                        }
                    };
                    let mut watched = Watched {
                        inner: sink,
                        cancel: thread_cancel,
                        done: thread_done,
                    };
                    let result = print_job(backend.as_ref(), info.doc, &job, &mut watched)
                        .map_err(|e| e.to_string());
                    let _ = backend.close(info.doc);
                    result
                };
                let result = run();
                let _ = tx.send(result);
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            })
            .map_err(|e| format!("the print job could not be started: {e}"))?;
        self.print.running = Some(RunningPrint {
            cancel,
            done,
            total: total.max(1),
            rx,
            printer: printer.to_owned(),
            sheets,
        });
        Ok(())
    }

    /// Asks the running print job to stop.
    pub fn cancel_print(&mut self) {
        if let Some(running) = &self.print.running {
            running.cancel.store(true, Ordering::Release);
        }
    }

    pub fn print_running(&self) -> bool {
        self.print.running.is_some()
    }

    /// The job last sent to a printer.
    pub fn last_print_job(&self) -> Option<&PrintJob> {
        self.print.last_job.as_ref()
    }

    /// Takes in the end of a print job. Returns whether one is still running.
    pub(crate) fn poll_print(&mut self) -> bool {
        let Some(running) = self.print.running.as_ref() else {
            return false;
        };
        let Ok(result) = running.rx.try_recv() else {
            return true;
        };
        let running = self.print.running.take().expect("checked above");
        self.chrome.status = Some(match result {
            Ok(_) => format!(
                "Sent {} sheet{} to {}.",
                running.sheets,
                if running.sheets == 1 { "" } else { "s" },
                running.printer
            ),
            Err(e) if running.cancel.load(Ordering::Acquire) => {
                let _ = e;
                "Printing was cancelled.".to_owned()
            }
            Err(e) => format!("Printing failed: {e}"),
        });
        false
    }

    /// Waits for a running print job (tests and shutdown).
    pub fn wait_print(&mut self, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        while self.print.running.is_some() {
            if !self.poll_print() {
                break;
            }
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        true
    }
}

/// The print panel, and the progress of a running job.
pub fn panel(app: &mut AppState, ctx: &egui::Context) {
    if let Some(running) = &app.print.running {
        let done = running.done.load(Ordering::Acquire);
        let total = running.total;
        let printer = running.printer.clone();
        let mut cancel = false;
        egui::Window::new("Printing")
            .id(Id::unique("plumbline-print-progress"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -36.0))
            .show(ctx, |ui| {
                ui.label(format!("Sending to {printer}"));
                ui.add(
                    egui::ProgressBar::new(done as f32 / total as f32)
                        .desired_width(220.0)
                        .show_percentage(),
                )
                .accessible_name("Print progress")
                .on_hover_text("How much of the job has been sent");
                cancel = ui
                    .button("Cancel")
                    .on_hover_text("Stop printing; what was sent is discarded")
                    .clicked();
            });
        if cancel {
            app.cancel_print();
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    if !app.print.open {
        return;
    }
    let mut go = false;
    let mut close = false;
    let preview = app.build_print_job().ok().and_then(|job| {
        let dv = app.active_view()?;
        let first = *job.pages.first()?;
        let geometry = *dv.view.file_geometry(first)?;
        let render = app.session.document(dv.doc).ok()?.render_id();
        Some((job, first, geometry, render))
    });
    let modal = egui::Modal::new(Id::unique("plumbline-print")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.label(RichText::new("Print").font(theme::bold(15.0)));
        ui.add_space(6.0);
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(270.0);
                let list = app.print.list.clone().unwrap_or_default();
                if list.is_empty() {
                    ui.label("No printers were found.");
                } else {
                    let shown = list
                        .get(app.print.printer)
                        .map(|p| p.name.clone())
                        .unwrap_or_default();
                    egui::ComboBox::from_id_salt("plumbline-print-printer")
                        .selected_text(shown)
                        .width(250.0)
                        .show_ui(ui, |ui| {
                            for (i, p) in list.iter().enumerate() {
                                ui.selectable_value(&mut app.print.printer, i, &p.name);
                            }
                        })
                        .response
                        .accessible_name("Printer")
                        .on_hover_text("The printer to send the pages to");
                }
                ui.add_space(6.0);
                ui.label(RichText::new("Which pages").strong());
                ui.radio_value(&mut app.print.pages, PageChoice::All, "All pages")
                    .on_hover_text("Every page of the document");
                ui.radio_value(&mut app.print.pages, PageChoice::Current, "Page in view")
                    .on_hover_text("Only the page shown now");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut app.print.pages, PageChoice::Range, "These pages")
                        .on_hover_text("The pages given");
                    let field = ui
                        .add(
                            egui::TextEdit::singleline(&mut app.print.range)
                                .desired_width(110.0)
                                .hint_text("1-3, 5"),
                        )
                        .accessible_name("Pages to print")
                        .on_hover_text("Page numbers and ranges, such as 1-3, 5");
                    if field.changed() {
                        app.print.pages = PageChoice::Range;
                    }
                });
                ui.add_space(6.0);
                ui.label(RichText::new("Size").strong());
                ui.radio_value(&mut app.print.scaling, PrintScaling::Fit, "Fit to paper")
                    .on_hover_text("Scale each page to fill the printable area");
                ui.radio_value(
                    &mut app.print.scaling,
                    PrintScaling::ActualSize,
                    "Actual size",
                )
                .on_hover_text("Print at the drawing's own size; a large sheet is cut off");
                ui.add_space(6.0);
                ui.checkbox(&mut app.print.markups, "Print markups")
                    .on_hover_text("Print the markups as well as the page");
                ui.horizontal(|ui| {
                    ui.label("Copies");
                    ui.add(egui::DragValue::new(&mut app.print.copies).range(1..=99))
                        .accessible_name("Copies")
                        .on_hover_text("How many copies of the pages to print");
                });
                ui.label(
                    RichText::new("Orientation follows each page.")
                        .color(Palette::of(ui.visuals()).muted_text),
                );
            });
            preview_box(ui, preview.as_ref(), &app.textures);
        });
        if let Some(error) = &app.print.error {
            ui.add_space(4.0);
            ui.label(error);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let can = app.print.list.as_ref().is_some_and(|l| !l.is_empty());
            go = ui
                .add_enabled(can, egui::Button::new("Print"))
                .on_hover_text("Send the pages to the printer")
                .clicked();
            close = ui
                .button("Cancel")
                .on_hover_text("Close without printing")
                .clicked();
        });
    });
    if close || (!go && modal.should_close()) {
        app.print.open = false;
    } else if go {
        app.start_print();
    }
}

/// A small picture of the first sheet: the paper, its printable area, and the page on it.
fn preview_box(
    ui: &mut egui::Ui,
    preview: Option<&(
        PrintJob,
        u32,
        pl_doc::pl_geom::PageGeometry,
        pl_doc::pl_render::DocId,
    )>,
    textures: &crate::textures::TextureStore,
) {
    let palette = Palette::of(ui.visuals());
    let (rect, response) = ui.allocate_exact_size(egui::vec2(150.0, 190.0), egui::Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::Role::Image, true, "Preview of the first sheet")
    });
    let Some((job, page, geometry, render)) = preview else {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No preview",
            egui::FontId::proportional(11.0),
            palette.muted_text,
        );
        return;
    };
    let paper = job.paper;
    let placement = layout_page(*page, &page_info(geometry), job);
    let k = (rect.width() / paper.size[0] as f32).min(rect.height() / paper.size[1] as f32);
    let sheet = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(paper.size[0] as f32 * k, paper.size[1] as f32 * k),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(sheet, 0.0, palette.paper);
    painter.rect_stroke(
        sheet,
        0.0,
        Stroke::new(1.0, palette.page_edge),
        StrokeKind::Outside,
    );
    let p = paper.printable;
    let printable = egui::Rect::from_min_size(
        sheet.min + egui::vec2(p.x as f32 * k, p.y as f32 * k),
        egui::vec2(p.width as f32 * k, p.height as f32 * k),
    );
    painter.rect_stroke(
        printable,
        0.0,
        Stroke::new(0.5, palette.muted_text.gamma_multiply(0.5)),
        StrokeKind::Inside,
    );
    let placed = egui::Rect::from_min_size(
        printable.min
            + egui::vec2(
                placement.offset[0] as f32 * k,
                placement.offset[1] as f32 * k,
            ),
        egui::vec2(placement.size[0] as f32 * k, placement.size[1] as f32 * k),
    );
    let page_painter = painter.with_clip_rect(printable);
    if let Some(texture) = textures.thumb(*render, *page) {
        // The thumbnail is the page as the file shows it; the placement may turn it.
        let turns = placement.extra_rotation().quarter_turns();
        crate::canvas::paint_texture(&page_painter, texture, placed, turns);
    } else {
        page_painter.rect_filled(placed, 0.0, palette.canvas.gamma_multiply(0.3));
    }
    page_painter.rect_stroke(
        placed,
        0.0,
        Stroke::new(1.0, palette.accent),
        StrokeKind::Inside,
    );
}
