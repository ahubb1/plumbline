//! The time, segment and memory caps under curve flattening, measured with a counting
//! allocator. Tests here run one at a time (a lock) so the peak belongs to the build measured.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use pl_scene::{
    build_scene, spawn_scene_build, NoSnapReason, SceneBuild, SceneLimits, SceneSource, SegmentKind,
};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, new_size) };
        if !q.is_null() {
            if new_size >= layout.size() {
                let grow = new_size - layout.size();
                let live = LIVE.fetch_add(grow, Ordering::Relaxed) + grow;
                PEAK.fetch_max(live, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn alone() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs `f` and returns its result, how long it took and the most heap it held beyond what
/// was live when it started.
fn measured<T>(f: impl FnOnce() -> T) -> (T, Duration, usize) {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let started = Instant::now();
    let out = f();
    let elapsed = started.elapsed();
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    (out, elapsed, peak)
}

/// A one-page PDF with the given content stream.
fn tiny_pdf(content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>"
            .to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

const TINY: SceneLimits = SceneLimits {
    time_cap: Duration::from_millis(50),
    max_segments: 1000,
    max_bytes: 1024 * 1024,
    flatten_tolerance: 0.05,
    allow_encrypted: false,
};

/// Heap a build may use beyond its caps: hayro's own parse and interpreter state for a
/// four-object file, the R-tree, and slack. The uncapped flattening this guards against used
/// 68 MiB to 262 MiB on these inputs.
const OVERHEAD: usize = 3 * 1024 * 1024;

#[test]
fn a_huge_coordinate_curve_cannot_outrun_the_caps() {
    let _guard = alone();
    // One short curve under a runaway matrix. Flattened at 0.05 units this is tens of
    // thousands to millions of pieces; collected before any check, it once used 262 MiB and
    // six times the time cap under these limits.
    for scale in ["1000", "100000", "10000000", "1000000000000"] {
        let content = format!(
            "{scale} 0 0 {scale} 0 0 cm 0 0 m 1000000 1000000 2000000 0 3000000 1000000 c S"
        );
        let source = SceneSource::from(tiny_pdf(&content));
        let (build, elapsed, peak) = measured(|| build_scene(&source, 0, &TINY));
        println!(
            "CURVE cm {scale}: {:?} in {elapsed:?}, peak extra heap {} KiB",
            build.reason(),
            peak / 1024
        );
        assert!(
            peak <= TINY.max_bytes + OVERHEAD,
            "cm {scale}: {} KiB of heap against a 1 MiB cap",
            peak / 1024
        );
        assert!(
            elapsed < TINY.time_cap + Duration::from_millis(100),
            "cm {scale}: {elapsed:?} against a 50 ms cap"
        );
        // Out of any page's reach: the curve is kept as its chord, not flattened.
        let scene = build.scene().expect("a chord is a scene");
        assert_eq!(scene.len(), 1, "cm {scale}");
        assert_eq!(scene.segments()[0].kind, SegmentKind::CurvePiece);
    }
}

#[test]
fn a_curve_needing_more_pieces_than_the_budget_stops_at_the_budget() {
    let _guard = alone();
    // In range (control points within 1e7 units) but far too fine for 1,000 segments:
    // about 40,000 pieces at this size.
    let content = "0 0 m 2000000 9000000 7000000 9000000 9000000 0 c S";
    let source = SceneSource::from(tiny_pdf(content));
    let (build, elapsed, peak) = measured(|| build_scene(&source, 0, &TINY));
    assert_eq!(build.reason(), Some(NoSnapReason::SegmentCap));
    assert!(peak <= TINY.max_bytes + OVERHEAD, "{} KiB", peak / 1024);
    assert!(elapsed < TINY.time_cap + Duration::from_millis(100));

    // The memory cap, when it is the tighter one, is the reason given.
    let by_memory = SceneLimits {
        max_segments: 10_000_000,
        max_bytes: 64 * 1024,
        ..TINY
    };
    let (build, _, peak) = measured(|| build_scene(&source, 0, &by_memory));
    assert_eq!(build.reason(), Some(NoSnapReason::MemoryCap));
    assert!(
        peak <= by_memory.max_bytes + OVERHEAD,
        "{} KiB",
        peak / 1024
    );

    // With room, the same curve flattens completely, ends exactly on its end point, and the
    // pieces are stored without first being collected: the peak is the scene, not twice it.
    let roomy = SceneLimits {
        time_cap: Duration::from_secs(20),
        max_segments: 1_000_000,
        max_bytes: 256 * 1024 * 1024,
        ..TINY
    };
    let build = build_scene(&source, 0, &roomy);
    let scene = build.scene().expect("fits");
    assert!(scene.len() > 10_000, "{} pieces", scene.len());
    let segs = scene.segments();
    assert!(segs[0].a_vertex && !segs[0].b_vertex);
    let last = segs.last().unwrap();
    assert!(last.b_vertex && !last.a_vertex);
    assert_eq!((last.b.x, last.b.y), (9_000_000.0, 0.0));
    assert!(segs.windows(2).all(|w| w[0].b == w[1].a));
}

#[test]
fn non_finite_and_degenerate_curves_store_nothing_and_do_not_hang() {
    let _guard = alone();
    // 20,000 curves that are a single point each: every piece is degenerate, so `push` never
    // runs its own checks. The per-element clock check has to end the build.
    let mut content = String::from("5 5 m\n");
    for _ in 0..20_000 {
        content.push_str("5 5 5 5 5 5 c\n");
    }
    content.push('S');
    let source = SceneSource::from(tiny_pdf(&content));
    let build = build_scene(&source, 0, &SceneLimits::default());
    assert_eq!(build.scene().map(|s| s.len()), Some(0));
    let no_time = SceneLimits {
        time_cap: Duration::ZERO,
        ..SceneLimits::default()
    };
    assert_eq!(
        build_scene(&source, 0, &no_time).reason(),
        Some(NoSnapReason::TimeCap)
    );
}

#[test]
fn a_worker_reports_when_it_has_let_go_of_its_source() {
    let _guard = alone();
    let fixture = pl_testkit::dense_sheet_with(20_000, 3);
    let bytes = Arc::new(fixture.bytes);
    let source = SceneSource::new(bytes.clone());

    // A build that runs to the end: the source is released once the worker is finished, and
    // the worker handle outlives the job.
    let job = spawn_scene_build(source.clone(), 0, SceneLimits::default());
    let worker = job.worker();
    let build = job.wait();
    assert!(matches!(build, SceneBuild::Ready(_)));
    assert!(worker.wait_finished(Duration::from_secs(20)));
    assert!(worker.is_finished());

    // A dropped (cancelled) job: its thread may still be running, and says so.
    let job = spawn_scene_build(source.clone(), 0, SceneLimits::default());
    let worker = job.worker();
    drop(job);
    assert!(worker.wait_finished(Duration::from_secs(20)));

    // A job that times out reports TimeCap while its worker is still alive, and the worker
    // then stops at its next check.
    let slow = SceneLimits {
        time_cap: Duration::ZERO,
        ..SceneLimits::default()
    };
    let job = spawn_scene_build(source.clone(), 0, slow);
    let worker = job.worker();
    assert_eq!(job.wait().reason(), Some(NoSnapReason::TimeCap));
    worker.cancel();
    assert!(worker.wait_finished(Duration::from_secs(20)));

    // With every worker finished, only this test's two handles on the bytes are left: no
    // thread still holds the (stand-in for a) file mapping.
    drop(source);
    assert_eq!(Arc::strong_count(&bytes), 1);
}
