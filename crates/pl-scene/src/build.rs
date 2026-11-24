//! Building a [`Scene`] from PDF bytes with hayro, under caps.

use std::fmt;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hayro_interpret::font::GlyphRun;
use hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image,
    ImageDrawProps, InterpreterCache, InterpreterSettings, RenderContent, SoftMask,
};
use hayro_syntax::{LoadPdfError, Pdf, PdfData};
use kurbo::{Affine, BezPath, PathEl};
use pl_geom::{Point, Rect};

use crate::scene::{Scene, SceneRect, SceneStats, Segment, SegmentKind, BYTES_PER_SEGMENT};

/// Caps for one scene build.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneLimits {
    /// Wall-clock budget for parsing, interpreting and indexing.
    pub time_cap: Duration,
    /// Most segments a scene may hold.
    pub max_segments: usize,
    /// Most heap bytes a scene may hold, by estimate.
    pub max_bytes: usize,
    /// Curve flattening tolerance in user units.
    pub flatten_tolerance: f64,
    /// Build scenes for encrypted files that hayro can open with the empty
    /// user password. Off by default, following the plan ("content snapping
    /// is off for encrypted files").
    pub allow_encrypted: bool,
}

impl Default for SceneLimits {
    fn default() -> Self {
        SceneLimits {
            time_cap: Duration::from_secs(4),
            max_segments: 1_500_000,
            max_bytes: 192 * 1024 * 1024,
            flatten_tolerance: 0.05,
            allow_encrypted: false,
        }
    }
}

impl SceneLimits {
    /// The segment count at which the build stops, and which cap sets it.
    fn segment_budget(&self) -> (usize, NoSnapReason) {
        let by_memory = self.max_bytes / BYTES_PER_SEGMENT;
        if by_memory < self.max_segments {
            (by_memory, NoSnapReason::MemoryCap)
        } else {
            (self.max_segments, NoSnapReason::SegmentCap)
        }
    }
}

/// Why a page has no content snapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NoSnapReason {
    /// The file is encrypted (and [`SceneLimits::allow_encrypted`] is off, or
    /// hayro could not decrypt it).
    Encrypted,
    /// hayro could not parse the file.
    Malformed,
    /// The file has no page with that index.
    PageOutOfRange,
    /// The build ran past [`SceneLimits::time_cap`].
    TimeCap,
    /// The page has more segments than [`SceneLimits::max_segments`].
    SegmentCap,
    /// The scene would need more than [`SceneLimits::max_bytes`].
    MemoryCap,
    /// The build was cancelled through [`SceneJob::cancel`].
    Cancelled,
    /// hayro panicked on the file; the panic was contained.
    InterpreterPanic,
    /// The worker thread could not be started or vanished without a result.
    WorkerFailed,
}

impl fmt::Display for NoSnapReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NoSnapReason::Encrypted => "the file is encrypted",
            NoSnapReason::Malformed => "the file could not be parsed",
            NoSnapReason::PageOutOfRange => "the page does not exist",
            NoSnapReason::TimeCap => "the page took too long to index",
            NoSnapReason::SegmentCap => "the page has too many segments",
            NoSnapReason::MemoryCap => "the page index would use too much memory",
            NoSnapReason::Cancelled => "indexing was cancelled",
            NoSnapReason::InterpreterPanic => "the page content could not be interpreted",
            NoSnapReason::WorkerFailed => "the indexing worker failed",
        })
    }
}

/// The outcome of a scene build. Degradation is a value, not an error.
#[derive(Debug)]
pub enum SceneBuild {
    /// The page's geometry, ready for snap queries.
    Ready(Scene),
    /// No content snapping for this page, and why.
    NoContentSnap(NoSnapReason),
}

impl SceneBuild {
    /// The scene, if the build succeeded.
    pub fn scene(&self) -> Option<&Scene> {
        match self {
            SceneBuild::Ready(scene) => Some(scene),
            SceneBuild::NoContentSnap(_) => None,
        }
    }

    /// The scene by value, if the build succeeded.
    pub fn into_scene(self) -> Option<Scene> {
        match self {
            SceneBuild::Ready(scene) => Some(scene),
            SceneBuild::NoContentSnap(_) => None,
        }
    }

    /// Why there is no scene, if the build degraded.
    pub fn reason(&self) -> Option<NoSnapReason> {
        match self {
            SceneBuild::Ready(_) => None,
            SceneBuild::NoContentSnap(reason) => Some(*reason),
        }
    }
}

/// The bytes of a PDF file, shared without copying: a `Vec<u8>`, or any
/// `Arc` of something that derefs to bytes (a memory map, for example).
///
/// # A memory map keeps the file open
///
/// A worker started by [`spawn_scene_build`] holds its source until its thread has really
/// exited, which can be later than the job reporting a result: a build stuck inside hayro is
/// reported as [`NoSnapReason::TimeCap`] and left to finish on its own. While any clone of a
/// memory-mapped source is alive, Windows refuses to replace the mapped file. Before a save
/// replaces the file, the owner must therefore see every worker gone:
/// keep the [`SceneWorker`] of each job ([`SceneJob::worker`]) and check
/// [`SceneWorker::is_finished`], or wait with [`SceneWorker::wait_finished`].
#[derive(Clone)]
pub struct SceneSource(Arc<dyn AsRef<[u8]> + Send + Sync>);

impl SceneSource {
    /// Wraps shared bytes.
    pub fn new<T: AsRef<[u8]> + Send + Sync + 'static>(bytes: Arc<T>) -> SceneSource {
        SceneSource(bytes)
    }

    /// The bytes.
    pub fn bytes(&self) -> &[u8] {
        (*self.0).as_ref()
    }
}

impl AsRef<[u8]> for SceneSource {
    fn as_ref(&self) -> &[u8] {
        self.bytes()
    }
}

impl From<Vec<u8>> for SceneSource {
    fn from(bytes: Vec<u8>) -> Self {
        SceneSource(Arc::new(bytes))
    }
}

impl<T: AsRef<[u8]> + Send + Sync + 'static> From<Arc<T>> for SceneSource {
    fn from(bytes: Arc<T>) -> Self {
        SceneSource(bytes)
    }
}

impl fmt::Debug for SceneSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SceneSource({} bytes)", self.bytes().len())
    }
}

/// Builds the scene of the zero-based page `page_index` on the calling
/// thread. Blocks for at most about `limits.time_cap` as long as hayro keeps
/// making progress; use [`spawn_scene_build`] when the caller must not block.
pub fn build_scene(source: &SceneSource, page_index: usize, limits: &SceneLimits) -> SceneBuild {
    build_contained(
        source,
        page_index,
        limits,
        &Arc::new(AtomicBool::new(false)),
    )
}

/// Starts a scene build on a worker thread and returns a handle to it.
pub fn spawn_scene_build(source: SceneSource, page_index: usize, limits: SceneLimits) -> SceneJob {
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let give_up_at = Instant::now() + limits.time_cap + SceneJob::GRACE;
    let worker_cancel = cancel.clone();
    let finished = Arc::new(AtomicBool::new(false));
    let worker_finished = finished.clone();
    let spawned = std::thread::Builder::new()
        .name("pl-scene".into())
        // hayro recurses through nested form XObjects.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            // Locals drop in reverse order: the source (and with it the file mapping) goes
            // first, and only then is the worker marked finished, on every way out.
            let _mark = MarkFinished(worker_finished);
            let source = source;
            let result = build_contained(&source, page_index, &limits, &worker_cancel);
            drop(source);
            // The receiver may be gone; the result is then dropped.
            let _ = tx.send(result);
        });
    if spawned.is_err() {
        // No thread, so nothing holds the source (the closure was dropped with the error).
        finished.store(true, Ordering::Release);
    }
    SceneJob {
        rx,
        cancel: cancel.clone(),
        give_up_at,
        early: spawned.is_err().then_some(NoSnapReason::WorkerFailed),
        taken: false,
        worker: SceneWorker { finished, cancel },
    }
}

struct MarkFinished(Arc<AtomicBool>);

impl Drop for MarkFinished {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// A handle on the worker thread of a scene build that outlives its [`SceneJob`]. It answers
/// one question: has the thread exited and let go of its [`SceneSource`]?
#[derive(Clone, Debug)]
pub struct SceneWorker {
    finished: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
}

impl SceneWorker {
    /// `true` once the worker thread has dropped its source and is on its way out. From then
    /// on the build holds no reference to the file.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Asks the worker to stop at its next check, as [`SceneJob::cancel`] does.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Waits until the worker has finished or `timeout` has passed; returns whether it has.
    pub fn wait_finished(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_finished() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// A scene build running on a worker thread.
///
/// Dropping the job cancels the build. A build that stops making progress
/// inside hayro cannot be interrupted; the job then reports
/// [`NoSnapReason::TimeCap`] and the thread is left to finish on its own.
/// It still holds its [`SceneSource`] until it does: see [`SceneJob::worker`].
#[derive(Debug)]
pub struct SceneJob {
    rx: Receiver<SceneBuild>,
    cancel: Arc<AtomicBool>,
    give_up_at: Instant,
    early: Option<NoSnapReason>,
    taken: bool,
    worker: SceneWorker,
}

impl SceneJob {
    /// How long past the time cap the job waits for the worker to notice it.
    const GRACE: Duration = Duration::from_millis(500);

    /// A handle that tells whether the worker thread has exited. Keep it when the job itself
    /// is dropped or has timed out and the source is a memory map of a file about to be
    /// replaced.
    pub fn worker(&self) -> SceneWorker {
        self.worker.clone()
    }

    /// Asks the worker to stop. The result becomes
    /// [`NoSnapReason::Cancelled`] unless the build had already finished.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Returns the result once it is available, without blocking. After it
    /// has returned `Some`, later calls return `None`.
    pub fn poll(&mut self) -> Option<SceneBuild> {
        if self.taken {
            return None;
        }
        let result = if let Some(reason) = self.early {
            SceneBuild::NoContentSnap(reason)
        } else {
            match self.rx.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Disconnected) => {
                    SceneBuild::NoContentSnap(NoSnapReason::WorkerFailed)
                }
                Err(TryRecvError::Empty) if Instant::now() >= self.give_up_at => {
                    self.cancel();
                    SceneBuild::NoContentSnap(NoSnapReason::TimeCap)
                }
                Err(TryRecvError::Empty) => return None,
            }
        };
        self.taken = true;
        Some(result)
    }

    /// Blocks until the result is available, for at most the time cap plus a
    /// short grace period.
    pub fn wait(mut self) -> SceneBuild {
        if let Some(result) = self.poll() {
            return result;
        }
        if self.taken {
            // The result was already handed out by `poll`.
            return SceneBuild::NoContentSnap(NoSnapReason::WorkerFailed);
        }
        let timeout = self.give_up_at.saturating_duration_since(Instant::now());
        self.taken = true;
        match self.rx.recv_timeout(timeout) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => {
                self.cancel();
                SceneBuild::NoContentSnap(NoSnapReason::TimeCap)
            }
            Err(RecvTimeoutError::Disconnected) => {
                SceneBuild::NoContentSnap(NoSnapReason::WorkerFailed)
            }
        }
    }
}

impl Drop for SceneJob {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Payload used to leave hayro's interpreter early. It is raised with
/// `resume_unwind`, so the panic hook does not run and nothing is printed.
struct Abort(NoSnapReason);

fn abort(reason: NoSnapReason) -> ! {
    resume_unwind(Box::new(Abort(reason)))
}

fn build_contained(
    source: &SceneSource,
    page_index: usize,
    limits: &SceneLimits,
    cancel: &Arc<AtomicBool>,
) -> SceneBuild {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        build_inner(source, page_index, limits, cancel)
    }));
    match outcome {
        Ok(result) => result,
        Err(payload) => SceneBuild::NoContentSnap(match payload.downcast::<Abort>() {
            Ok(abort) => abort.0,
            Err(_) => NoSnapReason::InterpreterPanic,
        }),
    }
}

fn build_inner(
    source: &SceneSource,
    page_index: usize,
    limits: &SceneLimits,
    cancel: &Arc<AtomicBool>,
) -> SceneBuild {
    let started = Instant::now();
    let deadline = started + limits.time_cap;
    let no_snap = SceneBuild::NoContentSnap;

    if !limits.allow_encrypted && has_encrypt_entry(source.bytes()) {
        return no_snap(NoSnapReason::Encrypted);
    }
    let pdf = match Pdf::new(PdfData::from(Arc::new(source.clone()))) {
        Ok(pdf) => pdf,
        Err(LoadPdfError::Decryption(_)) => return no_snap(NoSnapReason::Encrypted),
        Err(LoadPdfError::Invalid) => return no_snap(NoSnapReason::Malformed),
    };
    let pages = pdf.pages();
    let Some(page) = pages.get(page_index) else {
        return no_snap(NoSnapReason::PageOutOfRange);
    };

    let (budget, budget_reason) = limits.segment_budget();
    let mut collector = Collector {
        segments: Vec::new(),
        rects: Vec::new(),
        paint_ops: 0,
        calls: 0,
        budget,
        budget_reason,
        deadline,
        cancel: cancel.clone(),
        tolerance: if limits.flatten_tolerance.is_finite() && limits.flatten_tolerance > 0.0 {
            limits.flatten_tolerance
        } else {
            SceneLimits::default().flatten_tolerance
        },
    };
    // Also catches a time cap that the parse alone used up.
    collector.check_clock();

    // Page content only: annotations are never part of the scene. Text is
    // not collected, so no fonts or cmaps are resolved.
    let settings = InterpreterSettings {
        font_resolver: Arc::new(|_| None),
        cmap_resolver: Arc::new(|_| None),
        render_content: RenderContent::PAGE,
        ..Default::default()
    };
    let media = page.media_box();
    let bbox = Rect::new(media.x0, media.y0, media.x1, media.y1);
    let cache = InterpreterCache::new();
    // The identity as the initial transform leaves geometry in unrotated
    // page user space, y-up: the pl-geom convention.
    let mut context = Context::new(Affine::IDENTITY, bbox, &cache, pdf.xref(), settings);
    interpret_page(page, &mut context, &mut collector);
    collector.check_clock();

    let stats = SceneStats {
        paint_ops: collector.paint_ops,
        interpret_time: started.elapsed(),
        ..SceneStats::default()
    };
    let scene = Scene::from_parts(collector.segments, collector.rects, stats);
    if Instant::now() > deadline {
        return no_snap(NoSnapReason::TimeCap);
    }
    SceneBuild::Ready(scene)
}

/// Whether the file declares an `/Encrypt` entry. The entry sits in a
/// trailer or in a cross-reference stream dictionary, neither of which is
/// ever compressed or encrypted, so a byte search finds it. A false positive
/// (the same bytes inside binary data) only costs content snapping.
fn has_encrypt_entry(bytes: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"/Encrypt";
    let mut rest = bytes;
    while let Some(at) = rest.iter().position(|&b| b == b'/') {
        rest = &rest[at..];
        if rest.starts_with(NEEDLE) {
            return true;
        }
        rest = &rest[1..];
    }
    false
}

/// The hayro device that gathers segments.
struct Collector {
    segments: Vec<Segment>,
    rects: Vec<SceneRect>,
    paint_ops: u32,
    calls: u32,
    budget: usize,
    budget_reason: NoSnapReason,
    deadline: Instant,
    cancel: Arc<AtomicBool>,
    tolerance: f64,
}

/// Curves with a control point further than this from the origin (in user units) are not
/// flattened. A page is at most 14,400 units a side times its `/UserUnit`, so nothing a
/// reader could show lies out here; such coordinates come from a runaway `cm` matrix.
/// kurbo's piece count grows with the square root of the coordinates, so without a limit one
/// short curve could ask for any number of pieces.
const MAX_CURVE_COORD: f64 = 1.0e7;

fn in_curve_range(p: Point) -> bool {
    p.x.is_finite()
        && p.y.is_finite()
        && p.x.abs() <= MAX_CURVE_COORD
        && p.y.abs() <= MAX_CURVE_COORD
}

impl Collector {
    /// Called from every device callback; looks at the clock now and then.
    fn tick(&mut self) {
        self.calls = self.calls.wrapping_add(1);
        if self.calls.is_multiple_of(64) {
            self.check_clock();
        }
    }

    fn check_clock(&self) {
        if self.cancel.load(Ordering::Relaxed) {
            abort(NoSnapReason::Cancelled);
        }
        if Instant::now() > self.deadline {
            abort(NoSnapReason::TimeCap);
        }
    }

    /// Stores a segment; returns its index, or `None` if it was degenerate.
    fn push(
        &mut self,
        a: Point,
        b: Point,
        kind: SegmentKind,
        a_vertex: bool,
        b_vertex: bool,
    ) -> Option<u32> {
        let segment = Segment {
            a,
            b,
            kind,
            a_vertex,
            b_vertex,
            path: self.paint_ops,
        };
        if !segment.is_usable() {
            return None;
        }
        if self.segments.len() >= self.budget || self.segments.len() >= u32::MAX as usize {
            abort(self.budget_reason);
        }
        if self.segments.len().is_multiple_of(4096) {
            self.check_clock();
        }
        self.segments.push(segment);
        Some((self.segments.len() - 1) as u32)
    }

    fn painted(mode: &DrawMode) -> bool {
        !matches!(mode, DrawMode::Invisible)
    }

    fn add_path(&mut self, path: &BezPath, transform: Affine) {
        let mut start: Option<Point> = None;
        let mut last: Option<Point> = None;
        for (n, el) in path.elements().iter().enumerate() {
            // A path can have any number of elements that store nothing (degenerate or
            // non-finite ones), so the clock is also looked at per element.
            if n % 256 == 255 {
                self.check_clock();
            }
            match transform * *el {
                PathEl::MoveTo(p) => {
                    start = Some(p);
                    last = Some(p);
                }
                PathEl::LineTo(p) => {
                    if let Some(from) = last {
                        self.push(from, p, SegmentKind::Line, true, true);
                    } else {
                        start = Some(p);
                    }
                    last = Some(p);
                }
                PathEl::ClosePath => {
                    if let (Some(from), Some(to)) = (last, start) {
                        self.push(from, to, SegmentKind::Line, true, true);
                    }
                    last = start;
                }
                curve @ (PathEl::QuadTo(..) | PathEl::CurveTo(..)) => {
                    let end = curve.end_point().unwrap_or_default();
                    let Some(from) = last else {
                        start = Some(end);
                        last = Some(end);
                        continue;
                    };
                    self.add_curve(from, curve, end);
                    last = Some(end);
                }
            }
        }
    }
}

impl Collector {
    /// Flattens one curve from `from` and stores its pieces.
    ///
    /// Pieces are stored as kurbo produces them, one behind, so nothing is collected first:
    /// the segment budget and the clock apply to every piece, and a curve that would need
    /// more pieces than the budget stops the build after at most that many, whatever its
    /// size. The piece held back is what lets the last one end exactly on the curve's end
    /// point and be marked as a vertex.
    fn add_curve(&mut self, from: Point, curve: PathEl, end: Point) {
        let control_points_in_range = match curve {
            PathEl::QuadTo(p1, p2) => [from, p1, p2, p2],
            PathEl::CurveTo(p1, p2, p3) => [from, p1, p2, p3],
            _ => [from, end, end, end],
        }
        .into_iter()
        .all(in_curve_range);
        if !control_points_in_range {
            // Out of any page's reach: keep the chord (it is dropped if it is not finite)
            // and do not ask kurbo for a piece count that grows with the coordinates.
            self.push(from, end, SegmentKind::CurvePiece, true, true);
            return;
        }
        let tolerance = self.tolerance;
        let mut prev = from;
        let mut held: Option<Point> = None;
        let mut first = true;
        let mut pieces: usize = 0;
        kurbo::flatten([PathEl::MoveTo(from), curve], tolerance, |piece| {
            let PathEl::LineTo(p) = piece else {
                return;
            };
            pieces += 1;
            // Pieces that store nothing (zero length) pass `push` without its checks.
            if pieces > self.budget {
                abort(self.budget_reason);
            }
            if pieces.is_multiple_of(1024) {
                self.check_clock();
            }
            if let Some(q) = held.replace(p) {
                self.push(prev, q, SegmentKind::CurvePiece, first, false);
                first = false;
                prev = q;
            }
        });
        if held.is_some() {
            // The last piece ends exactly on the curve's end point.
            self.push(prev, end, SegmentKind::CurvePiece, first, true);
        }
    }
}

impl<'a> Device<'a> for Collector {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, draw_mode: &DrawMode) {
        self.tick();
        if !Self::painted(draw_mode) {
            return;
        }
        self.add_path(path, props.transform);
        self.paint_ops = self.paint_ops.wrapping_add(1);
    }

    fn draw_rect(&mut self, rect: &kurbo::Rect, props: DrawProps<'a>, draw_mode: &DrawMode) {
        self.tick();
        if !Self::painted(draw_mode) {
            return;
        }
        let t = props.transform;
        let corners = [
            t * Point::new(rect.x0, rect.y0),
            t * Point::new(rect.x1, rect.y0),
            t * Point::new(rect.x1, rect.y1),
            t * Point::new(rect.x0, rect.y1),
        ];
        if corners.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
            let before = self.segments.len();
            for i in 0..4 {
                self.push(
                    corners[i],
                    corners[(i + 1) % 4],
                    SegmentKind::RectEdge,
                    true,
                    true,
                );
            }
            // Only a rectangle with four real edges is recorded as one.
            let first_segment = (self.segments.len() == before + 4).then_some(before as u32);
            self.rects.push(SceneRect {
                corners,
                first_segment,
            });
        }
        self.paint_ops = self.paint_ops.wrapping_add(1);
    }

    fn push_clip_path(&mut self, _: &ClipPath) {
        self.tick();
    }

    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {
        self.tick();
    }

    fn draw_glyph_run(&mut self, _: &GlyphRun<'_, 'a>, _: DrawProps<'a>, _: &DrawMode) {
        self.tick();
    }

    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {
        self.tick();
    }

    fn pop_clip(&mut self) {
        self.tick();
    }

    fn pop_transparency_group(&mut self) {
        self.tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_marker_scan() {
        assert!(has_encrypt_entry(
            b"trailer << /Root 1 0 R /Encrypt 9 0 R >>"
        ));
        assert!(!has_encrypt_entry(
            b"trailer << /Root 1 0 R /Encoding /X >>"
        ));
        assert!(!has_encrypt_entry(b""));
        assert!(!has_encrypt_entry(b"///Encryp"));
        assert!(has_encrypt_entry(b"//Encrypt"));
    }

    #[test]
    fn budget_names_the_binding_cap() {
        let limits = SceneLimits {
            max_segments: 10,
            ..SceneLimits::default()
        };
        assert_eq!(limits.segment_budget(), (10, NoSnapReason::SegmentCap));
        let limits = SceneLimits {
            max_bytes: BYTES_PER_SEGMENT * 7 + 1,
            ..SceneLimits::default()
        };
        assert_eq!(limits.segment_budget(), (7, NoSnapReason::MemoryCap));
    }

    #[test]
    fn abort_payload_becomes_a_typed_result() {
        let source = SceneSource::from(b"not a pdf at all".to_vec());
        let build = build_scene(&source, 0, &SceneLimits::default());
        assert_eq!(build.reason(), Some(NoSnapReason::Malformed));
        assert!(build.scene().is_none());

        let caught = catch_unwind(|| abort(NoSnapReason::TimeCap));
        let payload = caught.unwrap_err();
        assert_eq!(
            payload.downcast::<Abort>().unwrap().0,
            NoSnapReason::TimeCap
        );
    }
}
