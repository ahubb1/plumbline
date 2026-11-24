//! [`PdfiumBackend`]: PDFium on one dedicated thread behind a request channel.
//!
//! This is the only module in the workspace that touches `pdfium-render`.
//!
//! * `pdfium.dll` is loaded by absolute path only ([`resolve_pdfium_dll`]); the DLL search path
//!   is never consulted.
//! * PDFium is not thread-safe, and `pdfium-render` binds it once per process. So there is one
//!   PDFium thread per process, started by the first [`PdfiumBackend::new`]; later calls return
//!   another handle to the same thread. Handles are cheap to clone and are `Send + Sync`.
//! * Files are memory-mapped and handed to PDFium as a reader, so a document is never copied
//!   into RAM by us. On Windows the mapping keeps the file open: close the document before
//!   replacing the file on save, and open it again afterwards.
//! * A small LRU of open page handles means PDFium parses a dense sheet's content once, not
//!   once per tile. Only rendering fills that LRU. Text extraction, search, annotation lists
//!   and geometry reads borrow a handle that is already there or load theirs into a separate
//!   slot (one per document), so a document-wide search cannot push the sheet being viewed out
//!   of it.
//! * Page sizes for a whole document come from the page dictionaries
//!   ([`Request::PageSizes`]); no page is loaded for them.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};

use memmap2::Mmap;
use pdfium_render::prelude::*;

use crate::backend::RenderBackend;
use crate::text::search_page_text;
use crate::types::*;

/// Environment variable that may hold the absolute path of `pdfium.dll`.
pub const PDFIUM_DLL_ENV: &str = "PLUMBLINE_PDFIUM_DLL";

/// How many page handles rendering keeps open across requests.
pub const PAGE_HANDLE_LRU: usize = 8;

/// Pixels rendered to the left of and above a tile and then thrown away.
///
/// PDFium 8125 draws a cached glyph bitmap wrongly when the glyph is cut by the left or top edge
/// of the destination bitmap: without a gutter, text crossing a tile seam differed from the
/// full-page render by up to 133 grey levels in the fixtures. Glyph bitmaps are only used for
/// small text (about 50 px and below; larger text is drawn as paths, which are not affected),
/// so a gutter of 128 px keeps every such glyph that reaches into the tile whole.
pub const TILE_GUTTER: u32 = 128;

const DLL_NAME: &str = "pdfium.dll";

/// Finds `pdfium.dll` without ever using the DLL search path. In order:
///
/// 1. `explicit`, if given: it must be an absolute path to an existing file, or this fails
///    (there is no fall-through, so a wrong setting is noticed).
/// 2. `pdfium.dll` next to the running executable.
/// 3. In debug builds only: the path in the `PLUMBLINE_PDFIUM_DLL` environment variable (must
///    be absolute), for development and tests.
/// 4. In debug builds only: the workspace's `vendor/pdfium/8125/bin/pdfium.dll`, for tests.
///
/// A release build therefore loads the DLL from an explicit path or from the application
/// folder and from nowhere else: the environment cannot choose the code that is loaded into
/// the shipped program.
pub fn resolve_pdfium_dll(explicit: Option<&Path>) -> Result<PathBuf, RenderError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let env = env_dll();
    resolve_from(
        explicit,
        exe_dir.as_deref(),
        env.as_deref(),
        vendor_dll().as_deref(),
        &|p| p.is_file(),
    )
}

/// The `PLUMBLINE_PDFIUM_DLL` setting. Read in debug builds only.
#[cfg(debug_assertions)]
fn env_dll() -> Option<PathBuf> {
    std::env::var_os(PDFIUM_DLL_ENV).map(PathBuf::from)
}

#[cfg(not(debug_assertions))]
fn env_dll() -> Option<PathBuf> {
    None
}

#[cfg(debug_assertions)]
fn vendor_dll() -> Option<PathBuf> {
    Some(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("vendor")
            .join("pdfium")
            .join("8125")
            .join("bin")
            .join(DLL_NAME),
    )
}

#[cfg(not(debug_assertions))]
fn vendor_dll() -> Option<PathBuf> {
    None
}

fn resolve_from(
    explicit: Option<&Path>,
    exe_dir: Option<&Path>,
    env: Option<&Path>,
    vendor: Option<&Path>,
    is_file: &dyn Fn(&Path) -> bool,
) -> Result<PathBuf, RenderError> {
    let absolute = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    if let Some(p) = explicit {
        if !p.is_absolute() {
            return Err(RenderError::LibraryLoad(format!(
                "the PDFium path must be absolute, got {}",
                p.display()
            )));
        }
        if !is_file(p) {
            return Err(RenderError::LibraryLoad(format!(
                "{} does not exist",
                p.display()
            )));
        }
        return Ok(absolute(p));
    }
    if let Some(dir) = exe_dir {
        let p = dir.join(DLL_NAME);
        if p.is_absolute() && is_file(&p) {
            return Ok(absolute(&p));
        }
    }
    if let Some(p) = env {
        if !p.is_absolute() {
            return Err(RenderError::LibraryLoad(format!(
                "{PDFIUM_DLL_ENV} must be an absolute path, got {}",
                p.display()
            )));
        }
        if is_file(p) {
            return Ok(absolute(p));
        }
    }
    if let Some(p) = vendor {
        if p.is_absolute() && is_file(p) {
            return Ok(absolute(p));
        }
    }
    Err(RenderError::LibraryLoad(if cfg!(debug_assertions) {
        format!("{DLL_NAME} was not found next to the executable or through {PDFIUM_DLL_ENV}")
    } else {
        format!("{DLL_NAME} was not found next to the executable")
    }))
}

type ReplySender = SyncSender<Result<Reply, RenderError>>;

/// A handle to the process's PDFium thread. See the [module documentation](self).
#[derive(Clone)]
pub struct PdfiumBackend {
    tx: Sender<(Request, ReplySender)>,
    dll: Arc<PathBuf>,
}

static GLOBAL: Mutex<Option<PdfiumBackend>> = Mutex::new(None);

impl PdfiumBackend {
    /// Returns a handle to the PDFium thread, starting it (and loading the DLL found by
    /// [`resolve_pdfium_dll`]) on the first call.
    ///
    /// If PDFium is already loaded, `explicit` must be `None` or name the same file; a process
    /// cannot load two PDFium builds.
    pub fn new(explicit: Option<&Path>) -> Result<PdfiumBackend, RenderError> {
        let mut global = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = global.as_ref() {
            if let Some(p) = explicit {
                let wanted = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
                if !same_path(&wanted, &existing.dll) {
                    return Err(RenderError::LibraryLoad(format!(
                        "PDFium is already loaded from {}",
                        existing.dll.display()
                    )));
                }
            }
            return Ok(existing.clone());
        }

        let dll = resolve_pdfium_dll(explicit)?;
        let (tx, rx) = channel::<(Request, ReplySender)>();
        let (ready_tx, ready_rx) = sync_channel::<Result<(), RenderError>>(1);
        let thread_dll = dll.clone();
        std::thread::Builder::new()
            .name("pl-pdfium".into())
            .spawn(move || pdfium_thread(thread_dll, ready_tx, rx))
            .map_err(|e| RenderError::LibraryLoad(format!("could not start the thread: {e}")))?;
        ready_rx
            .recv()
            .map_err(|_| RenderError::LibraryLoad("the PDFium thread died on start".into()))??;

        let backend = PdfiumBackend {
            tx,
            dll: Arc::new(dll),
        };
        *global = Some(backend.clone());
        Ok(backend)
    }

    /// The absolute path PDFium was loaded from.
    pub fn dll_path(&self) -> &Path {
        &self.dll
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .replace('/', "\\")
        .eq_ignore_ascii_case(&b.to_string_lossy().replace('/', "\\"))
}

impl RenderBackend for PdfiumBackend {
    fn call(&self, request: Request) -> Result<Reply, RenderError> {
        let (reply_tx, reply_rx) = sync_channel(1);
        self.tx
            .send((request, reply_tx))
            .map_err(|_| RenderError::Disconnected)?;
        reply_rx.recv().map_err(|_| RenderError::Disconnected)?
    }
}

fn pdfium_thread(
    dll: PathBuf,
    ready: SyncSender<Result<(), RenderError>>,
    requests: Receiver<(Request, ReplySender)>,
) {
    let bindings = match Pdfium::bind_to_library(&dll) {
        Ok(b) => b,
        Err(e) => {
            let _ = ready.send(Err(RenderError::LibraryLoad(format!(
                "{}: {e:?}",
                dll.display()
            ))));
            return;
        }
    };
    let pdfium = Pdfium::new(bindings);
    if ready.send(Ok(())).is_err() {
        return;
    }
    let mut state = State {
        pdfium: &pdfium,
        pages: PageSlots::new(PAGE_HANDLE_LRU),
        docs: HashMap::new(),
        next_id: 1,
    };
    while let Ok((request, reply)) = requests.recv() {
        let result = catch_unwind(AssertUnwindSafe(|| state.handle(request)))
            .unwrap_or_else(|_| Err(RenderError::Backend("the render thread panicked".into())));
        let _ = reply.send(result);
    }
}

/// The mapped file followed by an optional in-memory tail, as one seekable stream.
struct MappedSource {
    map: Mmap,
    tail: Vec<u8>,
    pos: u64,
}

impl MappedSource {
    fn len(&self) -> u64 {
        self.map.len() as u64 + self.tail.len() as u64
    }
}

impl Read for MappedSource {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < buf.len() && self.pos < self.len() {
            let head = self.map.len() as u64;
            let src: &[u8] = if self.pos < head {
                &self.map[self.pos as usize..]
            } else {
                &self.tail[(self.pos - head) as usize..]
            };
            let n = src.len().min(buf.len() - written);
            buf[written..written + n].copy_from_slice(&src[..n]);
            written += n;
            self.pos += n as u64;
        }
        Ok(written)
    }
}

impl Seek for MappedSource {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::End(d) => self.len().checked_add_signed(d),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
        };
        match target {
            Some(n) => {
                self.pos = n;
                Ok(n)
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek before the start of the document",
            )),
        }
    }
}

struct DocEntry<'p> {
    doc: PdfDocument<'p>,
    page_count: u32,
    infos: HashMap<u32, PageInfo>,
    user_units: HashMap<u32, f64>,
    /// Page handles loaded so far (each load parses the page's content).
    page_loads: Cell<u64>,
}

impl<'p> DocEntry<'p> {
    fn load_page(&self, page: u32) -> Result<PdfPage<'p>, RenderError> {
        self.page_loads.set(self.page_loads.get() + 1);
        self.doc
            .pages()
            .get(page as PdfPageIndex)
            .map_err(backend_err)
    }
}

/// `(document, page)`.
type PageKey = (u64, u32);

/// The open page handles: an LRU that only rendering fills, and one spare slot per document
/// for everything else.
///
/// Loading a page makes PDFium parse its content, which for a dense sheet costs more than a
/// tile. Rendering therefore keeps its pages open. A text extraction, a search, an annotation
/// list or a geometry read needs a page handle too, but must not decide which sheets stay
/// parsed: eight unrelated pages searched would otherwise evict the sheet on screen. Such a
/// query uses the handle rendering already has, and otherwise loads the page into its
/// document's spare slot, replacing the page that was there. A page never has two handles:
/// when rendering wants the page in a spare slot, it takes that handle over.
struct PageSlots<H> {
    /// Rendering's handles, least recently used first.
    render: Vec<(PageKey, H)>,
    /// Per document: the page last loaded for a query, and its handle.
    spare: HashMap<u64, (u32, H)>,
    capacity: usize,
}

impl<H> PageSlots<H> {
    fn new(capacity: usize) -> PageSlots<H> {
        PageSlots {
            render: Vec::new(),
            spare: HashMap::new(),
            capacity: capacity.max(1),
        }
    }

    /// The handle for a page that is about to be rendered: now the most recently used one.
    fn for_render<E>(
        &mut self,
        key: PageKey,
        load: impl FnOnce() -> Result<H, E>,
    ) -> Result<&mut H, E> {
        let entry = if let Some(i) = self.render.iter().position(|(k, _)| *k == key) {
            self.render.remove(i)
        } else if self
            .spare
            .get(&key.0)
            .is_some_and(|(page, _)| *page == key.1)
        {
            let (_, handle) = self.spare.remove(&key.0).expect("checked");
            (key, handle)
        } else {
            (key, load()?)
        };
        if self.render.len() >= self.capacity {
            self.render.remove(0);
        }
        self.render.push(entry);
        Ok(&mut self.render.last_mut().expect("just pushed").1)
    }

    /// A handle for a page that is only being read. Rendering's order is not changed and none
    /// of its handles is dropped.
    fn for_query<E>(
        &mut self,
        key: PageKey,
        load: impl FnOnce() -> Result<H, E>,
    ) -> Result<&mut H, E> {
        if let Some(i) = self.render.iter().position(|(k, _)| *k == key) {
            return Ok(&mut self.render[i].1);
        }
        if !self
            .spare
            .get(&key.0)
            .is_some_and(|(page, _)| *page == key.1)
        {
            // Drop the old spare first: two parsed pages are not held for a moment longer
            // than needed.
            self.spare.remove(&key.0);
            self.spare.insert(key.0, (key.1, load()?));
        }
        Ok(&mut self.spare.get_mut(&key.0).expect("just set").1)
    }

    /// Drops every handle of a document (they must go before the document does).
    fn forget_doc(&mut self, doc: u64) {
        self.render.retain(|(k, _)| k.0 != doc);
        self.spare.remove(&doc);
    }
}

struct State<'p> {
    pdfium: &'p Pdfium,
    /// Open page handles. Declared before `docs` so that pages are dropped before their
    /// documents.
    pages: PageSlots<PdfPage<'p>>,
    docs: HashMap<u64, DocEntry<'p>>,
    next_id: u64,
}

/// Whether two rectangles are the same to within what single-precision box values can differ.
fn same_rect(a: RectArray, b: RectArray) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= 1e-4)
}

fn backend_err(e: PdfiumError) -> RenderError {
    RenderError::Backend(format!("{e:?}"))
}

fn rect_array(r: PdfRect) -> RectArray {
    [
        r.left().value as f64,
        r.bottom().value as f64,
        r.right().value as f64,
        r.top().value as f64,
    ]
}

impl<'p> State<'p> {
    fn handle(&mut self, request: Request) -> Result<Reply, RenderError> {
        match request {
            Request::Open(open) => self.open(open).map(Reply::Opened),
            Request::Close { doc } => {
                // Page handles must go before the document they belong to.
                self.pages.forget_doc(doc.0);
                self.docs
                    .remove(&doc.0)
                    .map(|_| Reply::Done)
                    .ok_or(RenderError::UnknownDocument(doc))
            }
            Request::PageCount { doc } => Ok(Reply::PageCount(self.doc(doc)?.page_count)),
            Request::PageInfo { doc, page } => self.page_info(doc, page).map(Reply::PageInfo),
            Request::PageLoads { doc } => Ok(Reply::PageLoads(self.doc(doc)?.page_loads.get())),
            Request::PageSizes { doc } => {
                let entry = self.doc(doc)?;
                let pages = entry.doc.pages();
                // FPDF_GetPageSizeByIndexF: read from the page dictionary, nothing is parsed.
                let sizes = (0..entry.page_count)
                    .map(|i| {
                        let size = pages.page_size(i as PdfPageIndex).ok()?;
                        let (w, h) = (size.width().value as f64, size.height().value as f64);
                        (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some([w, h])
                    })
                    .collect();
                Ok(Reply::PageSizes(sizes))
            }
            Request::SetPageUserUnit {
                doc,
                page,
                user_unit,
            } => {
                if !user_unit.is_finite() || user_unit <= 0.0 {
                    return Err(RenderError::InvalidRequest("bad user unit".into()));
                }
                self.check_page(doc, page)?;
                let entry = self.doc_mut(doc)?;
                entry.user_units.insert(page, user_unit);
                entry.infos.remove(&page);
                Ok(Reply::Done)
            }
            Request::RenderTile(t) => {
                let geometry = self.page_info(t.doc, t.page)?.geometry();
                let (grid, rect) = tile_grid_for(&geometry, t.tile())?;
                self.render(
                    t.doc,
                    t.page,
                    (grid.width_px(), grid.height_px()),
                    (rect.x, rect.y),
                    (rect.width, rect.height),
                    t.options,
                )
                .map(Reply::Bitmap)
            }
            Request::RenderPage {
                doc,
                page,
                scale,
                options,
            } => {
                if !scale.is_finite() || scale <= 0.0 {
                    return Err(RenderError::InvalidRequest("scale must be positive".into()));
                }
                let geometry = self.page_info(doc, page)?.geometry();
                let full = page_pixel_size(&geometry, scale);
                self.render(doc, page, full, (0, 0), full, options)
                    .map(Reply::Bitmap)
            }
            Request::Thumbnail { doc, page, max_px } => {
                let geometry = self.page_info(doc, page)?.geometry();
                let full = page_pixel_size(&geometry, thumbnail_scale(&geometry, max_px));
                self.render(doc, page, full, (0, 0), full, RenderOptions::default())
                    .map(Reply::Bitmap)
            }
            Request::PageText { doc, page } => self.page_text(doc, page).map(Reply::Text),
            Request::Search {
                doc,
                page,
                query,
                options,
            } => {
                let text = self.page_text(doc, page)?;
                Ok(Reply::Hits(search_page_text(&text, &query, options)))
            }
            Request::Outline { doc } => self.outline(doc).map(Reply::Outline),
            Request::Annotations { doc, page } => {
                let handle = self.query_page(doc, page)?;
                let annotations = handle.annotations();
                let mut out = Vec::with_capacity(annotations.len());
                for (index, a) in annotations.iter().enumerate() {
                    out.push(AnnotInfo {
                        index: index as u32,
                        subtype: format!("{:?}", a.annotation_type()),
                        rect: a.bounds().map(rect_array).unwrap_or([0.0; 4]),
                        hidden: a.is_hidden(),
                        name: a.name(),
                    });
                }
                Ok(Reply::Annotations(out))
            }
            Request::SetAnnotationHidden {
                doc,
                page,
                index,
                hidden,
            } => {
                // The flag is written into the annotation's dictionary in the document, so it
                // holds whichever handle of the page is used afterwards.
                let handle = self.query_page(doc, page)?;
                let annotations = handle.annotations();
                if index as usize >= annotations.len() {
                    return Err(RenderError::InvalidRequest(format!(
                        "annotation {index} does not exist (the page has {})",
                        annotations.len()
                    )));
                }
                let mut annotation = annotations.get(index as usize).map_err(backend_err)?;
                annotation.set_is_hidden(hidden).map_err(backend_err)?;
                Ok(Reply::Done)
            }
            Request::SetAnnotationRect {
                doc,
                page,
                index,
                rect,
            } => {
                if rect.iter().any(|v| !v.is_finite()) {
                    return Err(RenderError::InvalidRequest(
                        "the annotation rectangle is not finite".into(),
                    ));
                }
                // Like the hidden flag, /Rect is written into the annotation's dictionary in
                // the in-memory document, so every handle of the page sees it.
                let handle = self.query_page(doc, page)?;
                let annotations = handle.annotations();
                if index as usize >= annotations.len() {
                    return Err(RenderError::InvalidRequest(format!(
                        "annotation {index} does not exist (the page has {})",
                        annotations.len()
                    )));
                }
                let mut annotation = annotations.get(index as usize).map_err(backend_err)?;
                let (x0, x1) = (rect[0].min(rect[2]), rect[0].max(rect[2]));
                let (y0, y1) = (rect[1].min(rect[3]), rect[1].max(rect[3]));
                annotation
                    .set_bounds(PdfRect::new_from_values(
                        y0 as f32, x0 as f32, y1 as f32, x1 as f32,
                    ))
                    .map_err(backend_err)?;
                Ok(Reply::Done)
            }
        }
    }

    fn doc(&self, doc: DocId) -> Result<&DocEntry<'p>, RenderError> {
        self.docs
            .get(&doc.0)
            .ok_or(RenderError::UnknownDocument(doc))
    }

    fn doc_mut(&mut self, doc: DocId) -> Result<&mut DocEntry<'p>, RenderError> {
        self.docs
            .get_mut(&doc.0)
            .ok_or(RenderError::UnknownDocument(doc))
    }

    fn check_page(&self, doc: DocId, page: u32) -> Result<(), RenderError> {
        let count = self.doc(doc)?.page_count;
        if page >= count {
            return Err(RenderError::PageOutOfRange { page, count });
        }
        Ok(())
    }

    fn open(&mut self, open: OpenRequest) -> Result<DocInfo, RenderError> {
        let password = open.password.as_deref();
        let loaded = match open.source {
            DocSource::Bytes(bytes) => self.pdfium.load_pdf_from_byte_vec(bytes, password),
            DocSource::File { path, appended } => {
                let io_err =
                    |e: std::io::Error| RenderError::Open(format!("{}: {e}", path.display()));
                let file = File::open(&path).map_err(io_err)?;
                // SAFETY: the mapping is read-only and lives inside the reader that PDFium
                // owns for the lifetime of the document. Windows refuses to write to or
                // truncate a file while it is mapped, so the bytes cannot change under us.
                let map = unsafe { Mmap::map(&file) }.map_err(io_err)?;
                self.pdfium.load_pdf_from_reader(
                    MappedSource {
                        map,
                        tail: appended,
                        pos: 0,
                    },
                    password,
                )
            }
        };
        let doc = loaded.map_err(|e| match e {
            PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError) => {
                RenderError::PasswordRequired
            }
            other => RenderError::Open(format!("{other:?}")),
        })?;
        let page_count = doc.pages().len().max(0) as u32;
        let id = self.next_id;
        self.next_id += 1;
        self.docs.insert(
            id,
            DocEntry {
                doc,
                page_count,
                infos: HashMap::new(),
                user_units: HashMap::new(),
                page_loads: Cell::new(0),
            },
        );
        Ok(DocInfo {
            doc: DocId(id),
            page_count,
        })
    }

    /// The page's handle for rendering: kept in the LRU, most recently used.
    fn render_page(&mut self, doc: DocId, page: u32) -> Result<&mut PdfPage<'p>, RenderError> {
        self.check_page(doc, page)?;
        let entry = self
            .docs
            .get(&doc.0)
            .ok_or(RenderError::UnknownDocument(doc))?;
        self.pages
            .for_render((doc.0, page), || entry.load_page(page))
    }

    /// The page's handle for a read that is not a render (text, annotations, geometry): the
    /// one rendering has open if there is one, otherwise the spare slot. See [`PageSlots`].
    fn query_page(&mut self, doc: DocId, page: u32) -> Result<&mut PdfPage<'p>, RenderError> {
        self.check_page(doc, page)?;
        let entry = self
            .docs
            .get(&doc.0)
            .ok_or(RenderError::UnknownDocument(doc))?;
        self.pages
            .for_query((doc.0, page), || entry.load_page(page))
    }

    fn page_info(&mut self, doc: DocId, page: u32) -> Result<PageInfo, RenderError> {
        if let Some(info) = self.doc(doc)?.infos.get(&page) {
            return Ok(*info);
        }
        let handle = self.query_page(doc, page)?;
        let rotate = match handle.rotation().map_err(backend_err)? {
            PdfPageRenderRotation::None => 0,
            PdfPageRenderRotation::Degrees90 => 90,
            PdfPageRenderRotation::Degrees180 => 180,
            PdfPageRenderRotation::Degrees270 => 270,
        };
        let (width, height) = (handle.width().value as f64, handle.height().value as f64);
        let boundaries = handle.boundaries();
        // FPDFPage_GetMediaBox and FPDFPage_GetCropBox read the page's own dictionary only, so
        // a box inherited from a /Pages node is invisible to them. FPDF_GetPageBoundingBox is
        // the box PDFium itself renders: the crop box clipped to the media box, in user space,
        // with inheritance resolved. It is the visible box in every case, and it is the media
        // box whenever the page does not carry its own.
        let visible = boundaries.bounding().ok().map(|b| rect_array(b.bounds));
        let own_media = boundaries.media().ok().map(|b| rect_array(b.bounds));
        let media_box = own_media.or(visible).unwrap_or(
            // Neither is readable: fall back to the size PDFium reports, un-rotated.
            if rotate % 180 == 90 {
                [0.0, 0.0, height, width]
            } else {
                [0.0, 0.0, width, height]
            },
        );
        let crop_box = visible.filter(|v| !same_rect(*v, media_box));
        let mut info = PageInfo {
            media_box,
            crop_box,
            rotate,
            user_unit: 1.0,
        };
        let entry = self.doc_mut(doc)?;
        if let Some(unit) = entry.user_units.get(&page) {
            info.user_unit = *unit;
        }
        entry.infos.insert(page, info);
        Ok(info)
    }

    /// Renders the `size` window at `origin` of the page drawn at `full` pixels.
    ///
    /// The page is placed with origin offsets rather than a matrix: PDFium's form-field pass
    /// (`FPDF_FFLDraw`) has no matrix variant, so this is the only way tiles and form fields
    /// line up.
    fn render(
        &mut self,
        doc: DocId,
        page: u32,
        full: (u32, u32),
        origin: (u32, u32),
        size: (u32, u32),
        options: RenderOptions,
    ) -> Result<Bitmap, RenderError> {
        check_bitmap_size(size.0, size.1)?;
        let limit = i32::MAX as u32 / 2;
        if full.0 > limit || full.1 > limit {
            return Err(RenderError::TooLarge {
                width: full.0 as u64,
                height: full.1 as u64,
            });
        }
        // Render a gutter to the left of and above the window, then crop it away (see
        // `TILE_GUTTER`). Windows that start at the page edge need none.
        let pad = (TILE_GUTTER.min(origin.0), TILE_GUTTER.min(origin.1));
        let padded = (size.0 + pad.0, size.1 + pad.1);
        let handle = self.render_page(doc, page)?;
        let config = PdfRenderConfig::new()
            .set_fixed_size(full.0 as Pixels, full.1 as Pixels)
            .set_origin(
                -((origin.0 - pad.0) as Pixels),
                -((origin.1 - pad.1) as Pixels),
            )
            .render_annotations(options.annotations)
            .render_form_data(true);
        let mut bitmap = PdfBitmap::empty(
            padded.0 as Pixels,
            padded.1 as Pixels,
            PdfBitmapFormat::BGRx,
        )
        .map_err(backend_err)?;
        handle
            .render_into_bitmap_with_config(&mut bitmap, &config)
            .map_err(backend_err)?;

        // The render flags ask PDFium for RGB byte order, so the raw buffer is already RGBx.
        // Copy out the window, dropping the gutter and any row padding, and make it opaque.
        let raw = bitmap.as_raw_bytes();
        let stride = raw.len() / padded.1 as usize;
        if stride < padded.0 as usize * 4 {
            return Err(RenderError::Backend(
                "PDFium returned a short bitmap".into(),
            ));
        }
        let row_bytes = size.0 as usize * 4;
        let mut pixels = Vec::with_capacity(row_bytes * size.1 as usize);
        for y in pad.1 as usize..padded.1 as usize {
            let start = y * stride + pad.0 as usize * 4;
            pixels.extend_from_slice(&raw[start..start + row_bytes]);
        }
        for px in pixels.as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
        Ok(Bitmap {
            width: size.0,
            height: size.1,
            pixels,
        })
    }

    fn page_text(&mut self, doc: DocId, page: u32) -> Result<PageText, RenderError> {
        let handle = self.query_page(doc, page)?;
        let text = handle.text().map_err(backend_err)?;
        let chars = text.chars();
        let mut out = Vec::with_capacity(chars.len());
        for c in chars.iter() {
            let origin = c
                .origin()
                .map(|(x, y)| [x.value as f64, y.value as f64])
                .unwrap_or([0.0; 2]);
            out.push(TextChar {
                ch: c.unicode_char().unwrap_or(char::REPLACEMENT_CHARACTER),
                bbox: c.loose_bounds().map(rect_array).unwrap_or([0.0; 4]),
                origin,
                font_size: c.scaled_font_size().value,
                generated: c.is_generated().unwrap_or(false),
            });
        }
        Ok(PageText { page, chars: out })
    }

    fn outline(&mut self, doc: DocId) -> Result<Vec<OutlineItem>, RenderError> {
        const MAX_ITEMS: usize = 200_000;
        const MAX_DEPTH: usize = 64;
        let entry = self.doc(doc)?;
        let mut out = Vec::new();
        // Depth-first walk with an explicit stack of "next sibling to visit" per level. The
        // item and depth limits guard against cyclic outlines in malformed files.
        let mut stack: Vec<Option<PdfBookmark>> = vec![entry.doc.bookmarks().root()];
        while let Some(top) = stack.last_mut() {
            let Some(bookmark) = top.take() else {
                stack.pop();
                continue;
            };
            *top = bookmark.next_sibling();
            let level = (stack.len() - 1) as u32;
            let mut item = OutlineItem {
                title: bookmark.title().unwrap_or_default(),
                level,
                page: None,
                x: None,
                y: None,
                zoom: None,
            };
            if let Some(dest) = bookmark.destination() {
                item.page = dest
                    .page_index()
                    .ok()
                    .filter(|p| *p >= 0 && (*p as u32) < entry.page_count)
                    .map(|p| p as u32);
                match dest.view_settings() {
                    Ok(PdfDestinationViewSettings::SpecificCoordinatesAndZoom(x, y, zoom)) => {
                        item.x = x.map(|v| v.value as f64);
                        item.y = y.map(|v| v.value as f64);
                        item.zoom = zoom.map(|z| z as f64).filter(|z| *z > 0.0);
                    }
                    Ok(PdfDestinationViewSettings::FitPageHorizontallyToWindow(y))
                    | Ok(PdfDestinationViewSettings::FitBoundsHorizontallyToWindow(y)) => {
                        item.y = y.map(|v| v.value as f64);
                    }
                    Ok(PdfDestinationViewSettings::FitPageVerticallyToWindow(x))
                    | Ok(PdfDestinationViewSettings::FitBoundsVerticallyToWindow(x)) => {
                        item.x = x.map(|v| v.value as f64);
                    }
                    _ => {}
                }
            }
            out.push(item);
            if out.len() >= MAX_ITEMS {
                break;
            }
            if stack.len() < MAX_DEPTH {
                if let Some(child) = bookmark.first_child() {
                    stack.push(Some(child));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exists_in<'a>(present: &'a [&'a str]) -> impl Fn(&Path) -> bool + 'a {
        move |p: &Path| present.iter().any(|q| Path::new(q) == p)
    }

    #[test]
    fn explicit_path_wins_and_must_be_absolute_and_present() {
        let all = [
            "C:\\x\\pdfium.dll",
            "C:\\app\\pdfium.dll",
            "C:\\env\\pdfium.dll",
        ];
        let got = resolve_from(
            Some(Path::new("C:\\x\\pdfium.dll")),
            Some(Path::new("C:\\app")),
            Some(Path::new("C:\\env\\pdfium.dll")),
            None,
            &exists_in(&all),
        );
        assert_eq!(got.unwrap(), PathBuf::from("C:\\x\\pdfium.dll"));

        // A relative explicit path is refused, never searched for.
        let rel = resolve_from(Some(Path::new("pdfium.dll")), None, None, None, &|_| true);
        assert!(matches!(rel, Err(RenderError::LibraryLoad(_))));

        // A missing explicit path does not fall through to the other places.
        let missing = resolve_from(
            Some(Path::new("C:\\nope\\pdfium.dll")),
            Some(Path::new("C:\\app")),
            None,
            None,
            &exists_in(&all),
        );
        assert!(matches!(missing, Err(RenderError::LibraryLoad(_))));
    }

    #[test]
    fn falls_back_exe_dir_then_env_then_vendor() {
        let exe = Some(Path::new("C:\\app"));
        let env = Some(Path::new("C:\\env\\pdfium.dll"));
        let vendor = Some(Path::new("C:\\ws\\vendor\\pdfium.dll"));
        let resolve = |present: &[&str]| resolve_from(None, exe, env, vendor, &exists_in(present));

        let all = [
            "C:\\app\\pdfium.dll",
            "C:\\env\\pdfium.dll",
            "C:\\ws\\vendor\\pdfium.dll",
        ];
        assert_eq!(resolve(&all).unwrap(), PathBuf::from(all[0]));
        assert_eq!(resolve(&all[1..]).unwrap(), PathBuf::from(all[1]));
        assert_eq!(resolve(&all[2..]).unwrap(), PathBuf::from(all[2]));
        assert!(matches!(resolve(&[]), Err(RenderError::LibraryLoad(_))));
    }

    #[test]
    fn a_relative_env_path_is_an_error_not_a_search() {
        let got = resolve_from(
            None,
            Some(Path::new("C:\\app")),
            Some(Path::new("pdfium.dll")),
            Some(Path::new("C:\\ws\\vendor\\pdfium.dll")),
            &|p| p == Path::new("C:\\ws\\vendor\\pdfium.dll") || p == Path::new("pdfium.dll"),
        );
        assert!(matches!(got, Err(RenderError::LibraryLoad(_))));
    }

    #[test]
    fn the_environment_setting_is_read_in_debug_builds_only() {
        // Windows serialises environment access, so setting a variable from a test is safe
        // here; nothing else in this test binary reads this one.
        let saved = std::env::var_os(PDFIUM_DLL_ENV);
        std::env::set_var(PDFIUM_DLL_ENV, "C:\\somewhere\\pdfium.dll");
        let seen = env_dll();
        match saved {
            Some(value) => std::env::set_var(PDFIUM_DLL_ENV, value),
            None => std::env::remove_var(PDFIUM_DLL_ENV),
        }
        if cfg!(debug_assertions) {
            assert_eq!(seen, Some(PathBuf::from("C:\\somewhere\\pdfium.dll")));
        } else {
            // A release build never lets the environment name the DLL to load.
            assert_eq!(seen, None);
            assert_eq!(vendor_dll(), None);
        }
    }

    /// Slots whose "handles" are the serial number of the load that made them.
    struct Loads {
        slots: PageSlots<u32>,
        count: u32,
    }

    impl Loads {
        fn new(capacity: usize) -> Loads {
            Loads {
                slots: PageSlots::new(capacity),
                count: 0,
            }
        }
        fn render(&mut self, page: u32) -> u32 {
            let count = &mut self.count;
            *self
                .slots
                .for_render::<()>((1, page), || {
                    *count += 1;
                    Ok(*count)
                })
                .unwrap()
        }
        fn query(&mut self, page: u32) -> u32 {
            let count = &mut self.count;
            *self
                .slots
                .for_query::<()>((1, page), || {
                    *count += 1;
                    Ok(*count)
                })
                .unwrap()
        }
        fn render_pages(&self) -> Vec<u32> {
            self.slots.render.iter().map(|(k, _)| k.1).collect()
        }
        /// The page in document 1's spare slot.
        fn spare_page(&self) -> Option<u32> {
            self.slots.spare.get(&1).map(|(page, _)| *page)
        }
    }

    #[test]
    fn queries_never_evict_or_reorder_the_pages_kept_for_rendering() {
        let mut s = Loads::new(3);
        let dense = s.render(0);
        s.render(1);
        s.render(2);
        assert_eq!(s.render_pages(), [0, 1, 2]);

        // A search walks twenty other pages: each is loaded once into the spare slot, and the
        // render LRU is exactly as it was.
        for page in 10..30 {
            s.query(page);
        }
        assert_eq!(s.count, 3 + 20);
        assert_eq!(s.render_pages(), [0, 1, 2]);
        assert_eq!(s.spare_page(), Some(29));
        // The dense page is still the handle from its first load: no re-parse.
        assert_eq!(s.render(0), dense);
        assert_eq!(s.count, 23);
        assert_eq!(s.render_pages(), [1, 2, 0]);

        // A query for a page rendering has open uses that handle, loads nothing and does not
        // count as a use.
        let before = s.count;
        assert_eq!(s.query(1), 2);
        assert_eq!(s.count, before);
        assert_eq!(s.render_pages(), [1, 2, 0]);
        assert_eq!(s.spare_page(), Some(29));

        // The same page queried twice in a row is loaded once.
        let first = s.query(40);
        assert_eq!(s.query(40), first);
        assert_eq!(s.count, before + 1);
    }

    #[test]
    fn rendering_takes_over_the_spare_handle_and_evicts_only_its_own_oldest() {
        let mut s = Loads::new(2);
        let queried = s.query(5);
        // Rendering page 5 reuses the handle the query loaded: one handle per page, one parse.
        assert_eq!(s.render(5), queried);
        assert_eq!(s.count, 1);
        assert!(s.slots.spare.is_empty());
        s.render(6);
        s.render(7);
        assert_eq!(s.render_pages(), [6, 7], "the LRU holds its capacity");
        // Page 5 was evicted, so it is loaded again.
        assert_ne!(s.render(5), queried);

        // A failed load leaves the slots as they were.
        let failed = s
            .slots
            .for_query((1, 99), || Err::<u32, &str>("no such page"));
        assert_eq!(failed, Err("no such page"));
        let failed = s
            .slots
            .for_render((1, 98), || Err::<u32, &str>("no such page"));
        assert_eq!(failed, Err("no such page"));
        assert_eq!(s.render_pages(), [7, 5]);

        // Closing a document drops its handles from both places, and only its own.
        s.slots.for_render::<()>((2, 0), || Ok(77)).unwrap();
        s.slots.for_query::<()>((2, 1), || Ok(78)).unwrap();
        // A query in another document does not disturb this document's spare page.
        s.query(50);
        s.slots.for_query::<()>((2, 2), || Ok(79)).unwrap();
        assert_eq!(s.spare_page(), Some(50));
        assert_eq!(
            *s.slots.for_query::<()>((1, 50), || Ok(0)).unwrap(),
            s.count
        );
        s.slots.forget_doc(2);
        assert_eq!(s.render_pages(), [5]);
        assert_eq!(s.spare_page(), Some(50));
        assert_eq!(s.slots.spare.len(), 1);
        s.slots.forget_doc(1);
        assert!(s.slots.render.is_empty());
    }

    #[test]
    fn mapped_source_reads_across_the_file_and_tail_boundary() {
        let dir = std::env::temp_dir().join(format!("pl-render-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("head.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        {
            let file = File::open(&path).unwrap();
            let map = unsafe { Mmap::map(&file) }.unwrap();
            let mut src = MappedSource {
                map,
                tail: b"abcdef".to_vec(),
                pos: 0,
            };
            assert_eq!(src.seek(SeekFrom::End(0)).unwrap(), 16);
            src.seek(SeekFrom::Start(7)).unwrap();
            let mut buf = [0u8; 6];
            src.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"789abc");
            src.seek(SeekFrom::Current(-2)).unwrap();
            let mut rest = Vec::new();
            src.read_to_end(&mut rest).unwrap();
            assert_eq!(rest, b"bcdef");
            assert_eq!(src.read(&mut buf).unwrap(), 0);
            assert!(src.seek(SeekFrom::Current(-100)).is_err());
            src.seek(SeekFrom::Start(100)).unwrap();
            assert_eq!(src.read(&mut buf).unwrap(), 0);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
