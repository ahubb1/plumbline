//! The render scheduler: a priority queue in front of a [`RenderBackend`], with
//! generation-based cancellation and a [`TileCache`] behind it.
//!
//! Order of work: visible tiles centre-out, then queries (page text, page geometry, the
//! outline), then the prefetch ring, then thumbnails. Within a class, jobs run in the order
//! given by their `order` field and then in submission order.
//!
//! Queries are jobs too, because the backend is a single thread: a caller that asked it for a
//! page's text directly would wait behind whatever tile is rendering. A user interface submits
//! the query here and picks the answer up from the result channel, so its frame never waits.
//!
//! Cancellation: every job belongs to a *stream* (one per view pane, one for the thumbnail
//! strip, ...) and carries the stream's generation at the time it was queued.
//! [`Scheduler::advance`] starts a new generation; every job of an older generation is dropped
//! from the queue, and the check is repeated under the queue lock at the moment a worker takes a
//! job, so a stale job is never handed to the backend. A job that is already rendering when its
//! generation ends is allowed to finish (PDFium cannot be interrupted) and its tile is cached.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use pl_geom::{visible_tiles, PageGeometry, Size, TileGrid, TileId, TileRange, ViewTransform};

use crate::backend::RenderBackend;
use crate::cache::{TileCache, TileKey};
use crate::types::{
    Bitmap, DocId, OutlineItem, PageInfo, PageText, RenderError, RenderOptions, TileRequest,
};

/// The class of a job. Lower runs first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Priority {
    Visible,
    /// Text, geometry and outline requests: after what is on screen, before speculative work.
    Query,
    Prefetch,
    Thumbnail,
}

/// A source of jobs that is cancelled as a unit, for example one view pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StreamId(pub u32);

/// A stream's generation counter. Jobs of any generation but the current one are stale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Generation(pub u64);

/// What a job renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    /// A tile, rendered with `key.options`; the result is also stored in the cache under
    /// `key`.
    Tile { key: TileKey },
    /// A page thumbnail; the result is only delivered, not cached here.
    Thumbnail { doc: DocId, page: u32, max_px: u32 },
    /// A page's characters and their boxes.
    PageText { doc: DocId, page: u32 },
    /// A page's boxes and rotation (this loads the page).
    PageInfo { doc: DocId, page: u32 },
    /// The bookmark tree, flattened.
    Outline { doc: DocId },
}

/// What a finished job produced.
#[derive(Clone, Debug)]
pub enum JobOutput {
    Bitmap(Arc<Bitmap>),
    Text(Arc<PageText>),
    PageInfo(PageInfo),
    Outline(Arc<Vec<OutlineItem>>),
}

impl JobOutput {
    /// The bitmap of a tile or thumbnail job.
    pub fn bitmap(&self) -> Option<&Arc<Bitmap>> {
        match self {
            JobOutput::Bitmap(bitmap) => Some(bitmap),
            _ => None,
        }
    }
}

/// One unit of work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Job {
    pub stream: StreamId,
    pub generation: Generation,
    pub priority: Priority,
    /// Position within the priority class; lower runs first.
    pub order: u32,
    pub kind: JobKind,
}

/// A finished job. Cancelled jobs produce no result.
#[derive(Clone, Debug)]
pub struct JobResult {
    pub job: Job,
    pub outcome: Result<JobOutput, RenderError>,
    /// `true` if the tile was already in the cache and the backend was not called.
    pub from_cache: bool,
}

/// Counters, for diagnostics and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerStats {
    /// Jobs accepted into the queue.
    pub submitted: u64,
    /// Jobs handed to the backend that returned a bitmap.
    pub rendered: u64,
    /// Jobs dropped because their generation had ended.
    pub cancelled: u64,
    /// Tile jobs answered from the cache without calling the backend.
    pub cache_hits: u64,
    /// Jobs the backend failed.
    pub failed: u64,
}

/// A page as one view shows it; input to [`Scheduler::queue_view`].
#[derive(Clone, Copy, Debug)]
pub struct ViewRequest {
    pub doc: DocId,
    pub page: u32,
    /// The page's content revision (see [`TileKey`]).
    pub revision: u64,
    pub layer_hash: u64,
    pub geometry: PageGeometry,
    /// `scroll` is relative to this page's own top-left corner.
    pub view: ViewTransform,
    /// The viewport in logical pixels.
    pub viewport: Size,
    /// How many tiles beyond the visible range to prefetch on each side; 0 for none.
    pub prefetch_ring: u32,
    pub options: RenderOptions,
}

/// What [`Scheduler::queue_view`] decided.
#[derive(Clone, Debug)]
pub struct QueuedView {
    pub generation: Generation,
    pub grid: TileGrid,
    /// The visible tiles (cached or not), for drawing.
    pub visible: TileRange,
    /// Visible tiles queued, in render order (those already cached are left out).
    pub queued_visible: Vec<TileId>,
    /// Prefetch tiles queued, in render order.
    pub queued_prefetch: Vec<TileId>,
}

struct Queued {
    rank: Reverse<(Priority, u32, u64)>,
    job: Job,
}

impl PartialEq for Queued {
    fn eq(&self, other: &Self) -> bool {
        self.rank == other.rank
    }
}
impl Eq for Queued {}
impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank.cmp(&other.rank)
    }
}

#[derive(Default)]
struct State {
    heap: BinaryHeap<Queued>,
    generations: HashMap<StreamId, u64>,
    seq: u64,
    active: usize,
    paused: bool,
    shutdown: bool,
    stats: SchedulerStats,
}

impl State {
    fn current(&self, stream: StreamId) -> u64 {
        self.generations.get(&stream).copied().unwrap_or(0)
    }

    fn is_stale(&self, job: &Job) -> bool {
        job.generation.0 != self.current(job.stream)
    }
}

type Notify = Box<dyn Fn() + Send + Sync>;

struct Shared {
    backend: Arc<dyn RenderBackend>,
    cache: Arc<TileCache>,
    state: Mutex<State>,
    work: Condvar,
    idle: Condvar,
    notify: Option<Notify>,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// See the [module documentation](self).
pub struct Scheduler {
    shared: Arc<Shared>,
    workers: Vec<JoinHandle<()>>,
}

impl Scheduler {
    /// Starts a scheduler with one worker thread. PDFium renders on a single thread, so more
    /// workers only help with a backend that can run requests in parallel.
    pub fn new(
        backend: Arc<dyn RenderBackend>,
        cache: Arc<TileCache>,
    ) -> (Scheduler, Receiver<JobResult>) {
        Scheduler::with_options(backend, cache, 1, None)
    }

    /// Starts a scheduler with `workers` threads (at least one). `notify`, if given, is called
    /// from a worker thread after each result is sent; a GUI uses it to request a repaint.
    pub fn with_options(
        backend: Arc<dyn RenderBackend>,
        cache: Arc<TileCache>,
        workers: usize,
        notify: Option<Box<dyn Fn() + Send + Sync>>,
    ) -> (Scheduler, Receiver<JobResult>) {
        let shared = Arc::new(Shared {
            backend,
            cache,
            state: Mutex::new(State::default()),
            work: Condvar::new(),
            idle: Condvar::new(),
            notify,
        });
        let (tx, rx) = channel();
        let workers = (0..workers.max(1))
            .map(|i| {
                let shared = shared.clone();
                let tx = tx.clone();
                std::thread::Builder::new()
                    .name(format!("pl-render-sched-{i}"))
                    .spawn(move || worker(shared, tx))
                    .expect("spawn scheduler worker")
            })
            .collect();
        (Scheduler { shared, workers }, rx)
    }

    /// The cache tiles are stored in.
    pub fn cache(&self) -> &Arc<TileCache> {
        &self.shared.cache
    }

    /// The current generation of a stream (0 until first advanced).
    pub fn generation(&self, stream: StreamId) -> Generation {
        Generation(self.shared.lock().current(stream))
    }

    /// Ends the stream's current generation and starts a new one. Every queued job of the
    /// stream is dropped; none of them will reach the backend.
    pub fn advance(&self, stream: StreamId) -> Generation {
        let mut s = self.shared.lock();
        let next = s.current(stream) + 1;
        s.generations.insert(stream, next);
        let before = s.heap.len();
        s.heap.retain(|q| q.job.stream != stream);
        s.stats.cancelled += (before - s.heap.len()) as u64;
        if s.heap.is_empty() && s.active == 0 {
            self.shared.idle.notify_all();
        }
        Generation(next)
    }

    /// Queues one job. Returns `false` if its generation is not the stream's current one.
    pub fn submit(&self, job: Job) -> bool {
        let mut s = self.shared.lock();
        if s.shutdown || s.is_stale(&job) {
            return false;
        }
        s.seq += 1;
        let seq = s.seq;
        s.heap.push(Queued {
            rank: Reverse((job.priority, job.order, seq)),
            job,
        });
        s.stats.submitted += 1;
        drop(s);
        self.shared.work.notify_one();
        true
    }

    /// Queues the tiles one page needs for a view under an existing generation: the visible
    /// tiles centre-out, then the prefetch ring. Tiles already cached are not queued. Call
    /// [`advance`](Scheduler::advance) once per frame of scrolling or zooming, then this once
    /// per visible page.
    pub fn queue_view(
        &self,
        stream: StreamId,
        generation: Generation,
        request: &ViewRequest,
    ) -> QueuedView {
        let (grid, visible) = visible_tiles(&request.geometry, &request.view, request.viewport);
        let key = |tile: TileId| {
            TileKey::new(
                request.doc,
                request.page,
                request.revision,
                tile,
                request.layer_hash,
            )
            .with_options(request.options)
        };
        let queue = |tiles: Vec<TileId>, priority: Priority| -> Vec<TileId> {
            let mut queued = Vec::new();
            for (i, tile) in tiles.into_iter().enumerate() {
                let key = key(tile);
                if self.shared.cache.contains(&key) {
                    continue;
                }
                let accepted = self.submit(Job {
                    stream,
                    generation,
                    priority,
                    order: i as u32,
                    kind: JobKind::Tile { key },
                });
                if accepted {
                    queued.push(tile);
                }
            }
            queued
        };
        let queued_visible = queue(visible.centre_out(), Priority::Visible);
        let queued_prefetch = if request.prefetch_ring > 0 {
            queue(
                visible.prefetch_ring(request.prefetch_ring, &grid),
                Priority::Prefetch,
            )
        } else {
            Vec::new()
        };
        QueuedView {
            generation,
            grid,
            visible,
            queued_visible,
            queued_prefetch,
        }
    }

    /// [`advance`](Scheduler::advance) followed by [`queue_view`](Scheduler::queue_view): the
    /// whole update for a single-page view.
    pub fn submit_view(&self, stream: StreamId, request: &ViewRequest) -> QueuedView {
        let generation = self.advance(stream);
        self.queue_view(stream, generation, request)
    }

    /// Queues a thumbnail under the stream's current generation.
    pub fn submit_thumbnail(
        &self,
        stream: StreamId,
        doc: DocId,
        page: u32,
        max_px: u32,
        order: u32,
    ) -> bool {
        let generation = self.generation(stream);
        self.submit(Job {
            stream,
            generation,
            priority: Priority::Thumbnail,
            order,
            kind: JobKind::Thumbnail { doc, page, max_px },
        })
    }

    /// Queues a query (page text, page geometry or the outline) under the stream's current
    /// generation, in the [`Priority::Query`] class. The answer arrives on the result channel;
    /// this never waits for the backend.
    pub fn submit_query(&self, stream: StreamId, kind: JobKind, order: u32) -> bool {
        let generation = self.generation(stream);
        self.submit(Job {
            stream,
            generation,
            priority: Priority::Query,
            order,
            kind,
        })
    }

    /// Stops workers from taking new jobs (a job already rendering finishes).
    pub fn pause(&self) {
        self.shared.lock().paused = true;
    }

    pub fn resume(&self) {
        self.shared.lock().paused = false;
        self.shared.work.notify_all();
    }

    /// Number of jobs waiting in the queue.
    pub fn queued(&self) -> usize {
        self.shared.lock().heap.len()
    }

    pub fn stats(&self) -> SchedulerStats {
        self.shared.lock().stats
    }

    /// Blocks until the queue is empty and no job is rendering, or the timeout passes. Returns
    /// `true` if idle. A paused scheduler with queued jobs is not idle.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut s = self.shared.lock();
        loop {
            if s.heap.is_empty() && s.active == 0 {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            s = self
                .shared
                .idle
                .wait_timeout(s, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        {
            let mut s = self.shared.lock();
            s.shutdown = true;
            s.heap.clear();
        }
        self.shared.work.notify_all();
        self.shared.idle.notify_all();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

fn worker(shared: Arc<Shared>, results: Sender<JobResult>) {
    loop {
        // Take the best job that is still current. The staleness check and the `active` count
        // change under the same lock as `advance`, so a stale job can never be started.
        let job = {
            let mut s = shared.lock();
            loop {
                if s.shutdown {
                    return;
                }
                let mut taken = None;
                if !s.paused {
                    while let Some(q) = s.heap.pop() {
                        if s.is_stale(&q.job) {
                            s.stats.cancelled += 1;
                        } else {
                            taken = Some(q.job);
                            break;
                        }
                    }
                }
                if let Some(job) = taken {
                    s.active += 1;
                    break job;
                }
                if s.heap.is_empty() && s.active == 0 {
                    shared.idle.notify_all();
                }
                s = shared.work.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        };

        let (outcome, from_cache) = run(&shared, &job);

        {
            let mut s = shared.lock();
            match (&outcome, from_cache) {
                (Ok(_), true) => s.stats.cache_hits += 1,
                (Ok(_), false) => s.stats.rendered += 1,
                (Err(_), _) => s.stats.failed += 1,
            }
        }
        // The receiver may be gone during shutdown; that is not an error.
        let _ = results.send(JobResult {
            job,
            outcome,
            from_cache,
        });
        if let Some(notify) = &shared.notify {
            notify();
        }
        let mut s = shared.lock();
        s.active -= 1;
        if s.heap.is_empty() && s.active == 0 {
            shared.idle.notify_all();
        }
    }
}

fn run(shared: &Shared, job: &Job) -> (Result<JobOutput, RenderError>, bool) {
    let backend = &shared.backend;
    match job.kind {
        JobKind::Tile { key } => {
            if let Some(bitmap) = shared.cache.get(&key) {
                return (Ok(JobOutput::Bitmap(bitmap)), true);
            }
            let request = TileRequest {
                doc: key.doc,
                page: key.page,
                level: key.level,
                x: key.x,
                y: key.y,
                options: key.options,
            };
            let outcome = backend.render_tile_request(request).map(|bitmap| {
                let bitmap = Arc::new(bitmap);
                shared.cache.insert(key, bitmap.clone());
                JobOutput::Bitmap(bitmap)
            });
            (outcome, false)
        }
        JobKind::Thumbnail { doc, page, max_px } => (
            backend
                .thumbnail(doc, page, max_px)
                .map(|b| JobOutput::Bitmap(Arc::new(b))),
            false,
        ),
        JobKind::PageText { doc, page } => (
            backend
                .page_text(doc, page)
                .map(|t| JobOutput::Text(Arc::new(t))),
            false,
        ),
        JobKind::PageInfo { doc, page } => {
            (backend.page_info(doc, page).map(JobOutput::PageInfo), false)
        }
        JobKind::Outline { doc } => (
            backend
                .outline(doc)
                .map(|o| JobOutput::Outline(Arc::new(o))),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock::{tiles_of, MockBackend};
    use crate::types::Request;
    use pl_geom::Vec2;

    const WAIT: Duration = Duration::from_secs(20);
    const VIEW: StreamId = StreamId(1);
    const THUMBS: StreamId = StreamId(2);

    fn setup(budget: usize) -> (Arc<MockBackend>, Scheduler, Receiver<JobResult>, DocId) {
        let mock = Arc::new(MockBackend::new());
        let doc = mock.add_document();
        let cache = Arc::new(TileCache::new(budget));
        let (sched, rx) = Scheduler::new(mock.clone(), cache);
        (mock, sched, rx, doc)
    }

    /// A Letter page at 400% with a viewport somewhere in the middle: level 3, 9 visible tiles.
    fn view(doc: DocId, scroll: Vec2, ring: u32) -> ViewRequest {
        ViewRequest {
            doc,
            page: 0,
            revision: 0,
            layer_hash: 0,
            geometry: PageGeometry::simple(612.0, 792.0),
            view: ViewTransform::new(4.0, 1.0, scroll),
            viewport: Size::new(1000.0, 800.0),
            prefetch_ring: ring,
            options: RenderOptions::default(),
        }
    }

    #[test]
    fn renders_visible_centre_out_then_prefetch_then_thumbnails() {
        let (mock, sched, rx, doc) = setup(64 << 20);
        sched.pause();
        // Queue in the "wrong" order on purpose: thumbnails first.
        assert!(sched.submit_thumbnail(THUMBS, doc, 0, 96, 1));
        assert!(sched.submit_thumbnail(THUMBS, doc, 0, 64, 0));
        let q = sched.submit_view(VIEW, &view(doc, Vec2::new(700.0, 900.0), 1));
        assert_eq!(q.grid.level(), 3);
        assert!(q.queued_visible.len() >= 4, "{:?}", q.visible);
        assert!(!q.queued_prefetch.is_empty());
        assert_eq!(q.queued_visible, q.visible.centre_out());
        assert_eq!(q.queued_prefetch, q.visible.prefetch_ring(1, &q.grid));

        sched.resume();
        assert!(sched.wait_idle(WAIT));

        let rendered = mock.rendered();
        let n_vis = q.queued_visible.len();
        let n_pre = q.queued_prefetch.len();
        assert_eq!(rendered.len(), n_vis + n_pre + 2);
        let tiles = tiles_of(&mock.rendered_tiles());
        assert_eq!(&tiles[..n_vis], &q.queued_visible[..]);
        assert_eq!(&tiles[n_vis..], &q.queued_prefetch[..]);
        assert!(matches!(
            rendered[n_vis + n_pre],
            Request::Thumbnail { max_px: 64, .. }
        ));
        assert!(matches!(
            rendered[n_vis + n_pre + 1],
            Request::Thumbnail { max_px: 96, .. }
        ));

        // Every job produced a result, and every tile is in the cache.
        let results: Vec<JobResult> = rx.try_iter().collect();
        assert_eq!(results.len(), n_vis + n_pre + 2);
        assert!(results.iter().all(|r| r.outcome.is_ok() && !r.from_cache));
        assert_eq!(sched.cache().len(), n_vis + n_pre);
        let stats = sched.stats();
        assert_eq!(stats.rendered as usize, n_vis + n_pre + 2);
        assert_eq!(stats.cancelled, 0);
    }

    #[test]
    fn a_cancelled_generation_is_never_rendered() {
        let (mock, sched, rx, doc) = setup(64 << 20);
        sched.pause();
        let old = sched.submit_view(VIEW, &view(doc, Vec2::new(0.0, 0.0), 1));
        let old_count = old.queued_visible.len() + old.queued_prefetch.len();
        assert!(old_count > 0);
        assert_eq!(sched.queued(), old_count);

        // The user scrolls far away before anything was rendered.
        let new = sched.submit_view(VIEW, &view(doc, Vec2::new(1800.0, 2800.0), 0));
        assert_eq!(new.generation, Generation(old.generation.0 + 1));
        assert_eq!(sched.queued(), new.queued_visible.len());
        for t in &old.queued_visible {
            assert!(!new.queued_visible.contains(t), "views must not overlap");
        }

        sched.resume();
        assert!(sched.wait_idle(WAIT));

        let tiles = tiles_of(&mock.rendered_tiles());
        assert_eq!(tiles, new.queued_visible);
        for t in old.queued_visible.iter().chain(&old.queued_prefetch) {
            assert!(!tiles.contains(t), "stale tile {t:?} was rendered");
        }
        assert_eq!(sched.stats().cancelled as usize, old_count);
        for r in rx.try_iter() {
            assert_eq!(r.job.generation, new.generation);
        }
        // A job submitted with the old generation is refused outright.
        assert!(!sched.submit(Job {
            stream: VIEW,
            generation: old.generation,
            priority: Priority::Visible,
            order: 0,
            kind: JobKind::Thumbnail {
                doc,
                page: 0,
                max_px: 32
            },
        }));
    }

    #[test]
    fn only_the_job_already_in_flight_survives_a_generation_change() {
        let (mock, sched, _rx, doc) = setup(64 << 20);
        // The backend blocks, so the worker is stuck inside the first tile of generation 1.
        mock.pause();
        let old = sched.submit_view(VIEW, &view(doc, Vec2::new(0.0, 0.0), 0));
        let total = old.queued_visible.len();
        assert!(total >= 4);
        let start = Instant::now();
        while sched.queued() == total {
            assert!(start.elapsed() < WAIT, "worker never picked a job up");
            std::thread::yield_now();
        }
        assert!(mock.rendered().is_empty(), "the backend is still blocked");

        let new = sched.submit_view(VIEW, &view(doc, Vec2::new(1800.0, 2800.0), 0));
        mock.resume();
        assert!(sched.wait_idle(WAIT));

        let tiles = tiles_of(&mock.rendered_tiles());
        // Exactly one old tile (the in-flight one, first in centre-out order), then the new view.
        assert_eq!(tiles[0], old.queued_visible[0]);
        assert_eq!(&tiles[1..], &new.queued_visible[..]);
        assert_eq!(sched.stats().cancelled as usize, total - 1);
    }

    #[test]
    fn streams_are_cancelled_independently() {
        let (mock, sched, _rx, doc) = setup(64 << 20);
        sched.pause();
        let g = sched.advance(THUMBS);
        assert_eq!(sched.generation(THUMBS), g);
        assert!(sched.submit_thumbnail(THUMBS, doc, 0, 80, 0));
        sched.submit_view(VIEW, &view(doc, Vec2::new(0.0, 0.0), 0));
        // Scrolling the view must not cancel the thumbnail strip.
        let new = sched.submit_view(VIEW, &view(doc, Vec2::new(1800.0, 2800.0), 0));
        sched.resume();
        assert!(sched.wait_idle(WAIT));
        let rendered = mock.rendered();
        assert_eq!(rendered.len(), new.queued_visible.len() + 1);
        assert!(matches!(rendered.last(), Some(Request::Thumbnail { .. })));
    }

    #[test]
    fn cached_tiles_are_not_rendered_again() {
        let (mock, sched, rx, doc) = setup(64 << 20);
        let first = sched.submit_view(VIEW, &view(doc, Vec2::new(700.0, 900.0), 0));
        assert!(sched.wait_idle(WAIT));
        let rendered = mock.rendered().len();
        assert_eq!(rendered, first.queued_visible.len());
        let _ = rx.try_iter().count();

        // Same view again: nothing to queue, nothing rendered.
        let again = sched.submit_view(VIEW, &view(doc, Vec2::new(700.0, 900.0), 0));
        assert!(again.queued_visible.is_empty());
        assert_eq!(again.visible, first.visible);
        assert!(sched.wait_idle(WAIT));
        assert_eq!(mock.rendered().len(), rendered);

        // A new content revision misses the cache and renders again.
        let mut bumped = view(doc, Vec2::new(700.0, 900.0), 0);
        bumped.revision = 1;
        let third = sched.submit_view(VIEW, &bumped);
        assert_eq!(third.queued_visible.len(), first.queued_visible.len());
        assert!(sched.wait_idle(WAIT));
        assert_eq!(mock.rendered().len(), rendered * 2);

        // A job whose tile is cached by the time it runs is answered from the cache.
        let key = TileKey::new(doc, 0, 1, third.queued_visible[0], 0);
        let generation = sched.generation(VIEW);
        assert!(sched.submit(Job {
            stream: VIEW,
            generation,
            priority: Priority::Visible,
            order: 0,
            kind: JobKind::Tile { key },
        }));
        assert!(sched.wait_idle(WAIT));
        assert_eq!(mock.rendered().len(), rendered * 2);
        let last = rx.try_iter().last().unwrap();
        assert!(last.from_cache);
        assert_eq!(sched.stats().cache_hits, 1);
    }

    #[test]
    fn the_cache_budget_holds_while_the_scheduler_fills_it() {
        // Room for about three full tiles; the view needs many more.
        let budget = 3 * (512 * 512 * 4 + 1024);
        let (mock, sched, rx, doc) = setup(budget);
        let q = sched.submit_view(VIEW, &view(doc, Vec2::new(700.0, 900.0), 2));
        let total = q.queued_visible.len() + q.queued_prefetch.len();
        assert!(total > 9);
        assert!(sched.wait_idle(WAIT));
        assert_eq!(mock.rendered().len(), total);
        assert_eq!(rx.try_iter().filter(|r| r.outcome.is_ok()).count(), total);
        let stats = sched.cache().stats();
        assert!(
            stats.peak_bytes <= budget,
            "{} > {budget}",
            stats.peak_bytes
        );
        assert!(sched.cache().bytes() <= budget);
        assert!(stats.evictions > 0);
        assert!(sched.cache().len() <= 3);
    }

    #[test]
    fn backend_failures_are_reported_and_not_cached() {
        let (mock, sched, rx, doc) = setup(64 << 20);
        mock.set_fail_renders(true);
        let q = sched.submit_view(VIEW, &view(doc, Vec2::new(0.0, 0.0), 0));
        assert!(sched.wait_idle(WAIT));
        let results: Vec<JobResult> = rx.try_iter().collect();
        assert_eq!(results.len(), q.queued_visible.len());
        assert!(results.iter().all(|r| r.outcome.is_err()));
        assert!(sched.cache().is_empty());
        assert_eq!(sched.stats().failed as usize, results.len());
    }

    #[test]
    fn notify_is_called_and_drop_joins_with_work_queued() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mock = Arc::new(MockBackend::new());
        let doc = mock.add_document();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let (sched, _rx) = Scheduler::with_options(
            mock.clone(),
            Arc::new(TileCache::new(64 << 20)),
            2,
            Some(Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            })),
        );
        let q = sched.submit_view(VIEW, &view(doc, Vec2::new(0.0, 0.0), 0));
        assert!(sched.wait_idle(WAIT));
        assert_eq!(count.load(Ordering::SeqCst), q.queued_visible.len());

        sched.pause();
        sched.submit_view(VIEW, &view(doc, Vec2::new(1800.0, 2800.0), 1));
        assert!(!sched.wait_idle(Duration::from_millis(20)));
        drop(sched); // must not hang
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::mock::MockBackend;
    use crate::text::synthetic_text;
    use crate::types::Request;
    use pl_geom::Vec2;

    const WAIT: Duration = Duration::from_secs(20);
    const VIEW: StreamId = StreamId(1);
    const QUERIES: StreamId = StreamId(7);

    fn view(doc: DocId, options: RenderOptions) -> ViewRequest {
        ViewRequest {
            doc,
            page: 0,
            revision: 0,
            layer_hash: 0,
            geometry: PageGeometry::simple(612.0, 792.0),
            view: ViewTransform::new(4.0, 1.0, Vec2::new(700.0, 900.0)),
            viewport: Size::new(1000.0, 800.0),
            prefetch_ring: 0,
            options,
        }
    }

    #[test]
    fn tiles_rendered_with_different_options_do_not_share_a_cache_entry() {
        let mock = Arc::new(MockBackend::new());
        let doc = mock.add_document();
        let (sched, rx) = Scheduler::new(mock.clone(), Arc::new(TileCache::new(64 << 20)));
        let with = RenderOptions { annotations: true };
        let without = RenderOptions { annotations: false };

        let first = sched.submit_view(VIEW, &view(doc, with));
        assert!(sched.wait_idle(WAIT));
        let n = first.queued_visible.len();
        assert!(n >= 4);
        assert_eq!(mock.rendered().len(), n);

        // The same tiles without annotations are different tiles: all are queued and all
        // reach the backend with that option, none is answered from the first set.
        let second = sched.submit_view(VIEW, &view(doc, without));
        assert_eq!(second.queued_visible, first.queued_visible);
        assert!(sched.wait_idle(WAIT));
        let tiles = mock.rendered_tiles();
        assert_eq!(tiles.len(), 2 * n);
        assert!(tiles[..n].iter().all(|t| t.options == with));
        assert!(tiles[n..].iter().all(|t| t.options == without));
        assert!(rx.try_iter().all(|r| !r.from_cache));
        assert_eq!(sched.cache().len(), 2 * n, "both variants are cached");

        // Each variant is found again under its own key only.
        let key = TileKey::new(doc, 0, 0, first.queued_visible[0], 0);
        assert_eq!(key.options, with);
        assert!(sched.cache().contains(&key));
        assert!(sched.cache().contains(&key.with_options(without)));
        assert_ne!(key, key.with_options(without));
        assert!(sched
            .submit_view(VIEW, &view(doc, with))
            .queued_visible
            .is_empty());
        assert!(sched
            .submit_view(VIEW, &view(doc, without))
            .queued_visible
            .is_empty());
    }

    #[test]
    fn queries_run_after_visible_tiles_and_before_prefetch_and_thumbnails() {
        let mock = Arc::new(MockBackend::new());
        let doc = mock.add_document();
        mock.set_page_text(synthetic_text(0, &[("north wall", [10.0, 100.0], 10.0)]));
        let (sched, rx) = Scheduler::new(mock.clone(), Arc::new(TileCache::new(64 << 20)));
        sched.pause();
        assert!(sched.submit_thumbnail(StreamId(2), doc, 0, 64, 0));
        assert!(sched.submit_query(QUERIES, JobKind::Outline { doc }, 2));
        assert!(sched.submit_query(QUERIES, JobKind::PageText { doc, page: 0 }, 0));
        assert!(sched.submit_query(QUERIES, JobKind::PageInfo { doc, page: 0 }, 1));
        let mut request = view(doc, RenderOptions::default());
        request.prefetch_ring = 1;
        let queued = sched.submit_view(VIEW, &request);
        sched.resume();
        assert!(sched.wait_idle(WAIT));

        let results: Vec<JobResult> = rx.try_iter().collect();
        let classes: Vec<Priority> = results.iter().map(|r| r.job.priority).collect();
        let mut sorted = classes.clone();
        sorted.sort();
        assert_eq!(classes, sorted, "jobs ran in priority order");
        let visible = queued.queued_visible.len();
        assert!(matches!(
            results[visible].job.kind,
            JobKind::PageText { page: 0, .. }
        ));
        match &results[visible].outcome {
            Ok(JobOutput::Text(text)) => assert_eq!(text.text(), "north wall"),
            other => panic!("{other:?}"),
        }
        match &results[visible + 1].outcome {
            Ok(JobOutput::PageInfo(info)) => {
                assert_eq!(info.media_box, [0.0, 0.0, 612.0, 792.0]);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            &results[visible + 2].outcome,
            Ok(JobOutput::Outline(items)) if items.is_empty()
        ));
        assert!(results[visible + 2]
            .outcome
            .as_ref()
            .unwrap()
            .bitmap()
            .is_none());
        assert!(matches!(
            results.last().unwrap().job.kind,
            JobKind::Thumbnail { .. }
        ));

        // A query for a page that does not exist is a failed job, not a panic, and a query
        // stream is cancelled like any other.
        assert!(sched.submit_query(QUERIES, JobKind::PageText { doc, page: 9 }, 0));
        assert!(sched.wait_idle(WAIT));
        assert!(rx.try_iter().last().unwrap().outcome.is_err());
        sched.pause();
        assert!(sched.submit_query(QUERIES, JobKind::PageText { doc, page: 0 }, 0));
        sched.advance(QUERIES);
        sched.resume();
        assert!(sched.wait_idle(WAIT));
        assert_eq!(rx.try_iter().count(), 0);
        let texts = mock
            .calls()
            .iter()
            .filter(|r| matches!(r, Request::PageText { .. }))
            .count();
        assert_eq!(texts, 2);
    }

    #[test]
    fn a_query_does_not_block_its_caller_while_the_backend_is_busy() {
        let mock = Arc::new(MockBackend::new());
        let doc = mock.add_document();
        let (sched, rx) = Scheduler::new(mock.clone(), Arc::new(TileCache::new(64 << 20)));
        // The backend is stuck (as PDFium is inside a dense tile); submitting still returns.
        mock.pause_all();
        let started = Instant::now();
        sched.submit_view(VIEW, &view(doc, RenderOptions::default()));
        assert!(sched.submit_query(QUERIES, JobKind::PageText { doc, page: 0 }, 0));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(rx.try_recv().is_err(), "nothing can have finished");
        mock.resume();
        assert!(sched.wait_idle(WAIT));
        assert!(rx
            .try_iter()
            .any(|r| matches!(r.outcome, Ok(JobOutput::Text(_)))));
    }
}
