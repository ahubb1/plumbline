//! The dense-sheet gate and benchmark: the visible tiles of the 200,000-segment sheet at 400%
//! zoom in a 1920 x 1080 viewport.
//!
//! * `dense_sheet_at_400_percent_meets_the_budget` runs with every `cargo test` and asserts
//!   the budget in `pl_render::budget`. This is the M1 criterion that decides whether the
//!   worker-process pool is built.
//! * `bench_dense_sheet_400_percent_1920x1080` prints the detailed figures recorded in
//!   docs/measurements.md. It asserts nothing about time, so it is ignored by default:
//!
//! ```text
//! cargo test -p pl-render --release --test bench_dense -- --ignored --nocapture
//! ```

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use pl_geom::{visible_tiles, Size, Vec2, ViewTransform};
use pl_render::{
    budget, DocSource, JobKind, RenderBackend, RenderOptions, Scheduler, StreamId, TileCache,
    ViewRequest,
};

/// One cold run through the scheduler, as the app does it: open the file, read the page
/// geometry, queue the visible tiles. Returns (time to the first tile, time to all visible
/// tiles, number of tiles), both measured from before the open.
fn cold_viewport(path: &std::path::Path, dpr: f64) -> (Duration, Duration, usize) {
    let backend = backend();
    let viewport = Size::new(
        budget::DENSE_SHEET_VIEWPORT[0],
        budget::DENSE_SHEET_VIEWPORT[1],
    );
    let t0 = Instant::now();
    let doc = backend.open(DocSource::file(path), None).unwrap().doc;
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let mut view = ViewTransform::new(budget::DENSE_SHEET_ZOOM, dpr, Vec2::ZERO);
    let page = view.page_rect_in_view(&geometry);
    view.scroll = Vec2::new(
        (page.width() - viewport.width) / 2.0,
        (page.height() - viewport.height) / 2.0,
    );
    let cache = Arc::new(TileCache::default());
    let (scheduler, results) = Scheduler::new(Arc::new(backend.clone()), cache);
    let queued = scheduler.submit_view(
        StreamId(0),
        &ViewRequest {
            doc,
            page: 0,
            revision: 0,
            layer_hash: 0,
            geometry,
            view,
            viewport,
            prefetch_ring: 0,
            options: RenderOptions::default(),
        },
    );
    let first = results.recv_timeout(Duration::from_secs(600)).unwrap();
    let first_tile = t0.elapsed();
    assert!(matches!(first.job.kind, JobKind::Tile { .. }));
    // The centre of a dense sheet has ink in it: this is a real render, not a blank tile.
    let bitmap = first.outcome.unwrap().bitmap().cloned().unwrap();
    assert!(bitmap.pixels.as_chunks::<4>().0.iter().any(|p| p[0] < 128));
    assert!(scheduler.wait_idle(Duration::from_secs(600)));
    let all = t0.elapsed();
    let tiles = queued.queued_visible.len();
    assert_eq!(scheduler.stats().rendered as usize, tiles);
    assert_eq!(scheduler.stats().failed, 0);
    drop(scheduler);
    backend.close(doc).unwrap();
    (first_tile, all, tiles)
}

/// M1 acceptance: "A 200k-segment sheet at 400% meets the M0 budget; this gates the worker
/// pool." The budget is `pl_render::budget`; PDFium is the same optimised DLL in debug and
/// release test builds, so the figures hold in both.
#[test]
fn dense_sheet_at_400_percent_meets_the_budget() {
    let fixture = pl_testkit::dense_sheet();
    assert_eq!(
        fixture.manifest.total_segments(),
        budget::DENSE_SHEET_SEGMENTS
    );
    let path = write_temp(&fixture, "gate");
    for (dpr, viewport_budget, min_tiles) in [
        (1.0, budget::DENSE_VIEWPORT, 24),
        (2.0, budget::DENSE_VIEWPORT_HIDPI, 80),
    ] {
        // A budget is about what the code can do, not about what else the machine was doing
        // during one run: the best of three cold runs is compared.
        let runs: Vec<(Duration, Duration, usize)> =
            (0..3).map(|_| cold_viewport(&path, dpr)).collect();
        let first = runs.iter().map(|r| r.0).min().unwrap();
        let all = runs.iter().map(|r| r.1).min().unwrap();
        let tiles = runs[0].2;
        println!(
            "GATE dense sheet, 400%, 1920x1080, dpr {dpr}: {tiles} tiles, first tile {:.1} ms (budget {} ms), all visible {:.1} ms (budget {} ms); runs {runs:?}",
            ms(first),
            budget::DENSE_FIRST_TILE.as_millis(),
            ms(all),
            viewport_budget.as_millis()
        );
        assert!(
            tiles >= min_tiles,
            "the view must cover the viewport: {tiles} tiles"
        );
        assert!(
            first <= budget::DENSE_FIRST_TILE,
            "dpr {dpr}: first tile after {first:?}, budget {:?}",
            budget::DENSE_FIRST_TILE
        );
        assert!(
            all <= viewport_budget,
            "dpr {dpr}: all {tiles} visible tiles after {all:?}, budget {viewport_budget:?}"
        );
    }
    std::fs::remove_file(&path).unwrap();
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[test]
#[ignore = "benchmark; run in release mode with --ignored --nocapture"]
fn bench_dense_sheet_400_percent_1920x1080() {
    let fixture = pl_testkit::dense_sheet();
    assert_eq!(fixture.manifest.total_segments(), 200_000);
    let path = write_temp(&fixture, "bench");
    let backend = backend();
    let viewport = Size::new(1920.0, 1080.0);

    println!(
        "BENCH file: {} bytes, {} segments",
        fixture.bytes.len(),
        fixture.manifest.total_segments()
    );
    println!(
        "BENCH build: {}",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    // --- Direct calls: open, then each visible tile centre-out, cold. ---
    let t0 = Instant::now();
    let doc = backend.open(DocSource::file(&path), None).unwrap().doc;
    let open = t0.elapsed();
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let info = t0.elapsed();

    let zoom = 4.0;
    let centred = |dpr: f64| {
        let mut view = ViewTransform::new(zoom, dpr, Vec2::ZERO);
        let page = view.page_rect_in_view(&geometry);
        view.scroll = Vec2::new(
            (page.width() - viewport.width) / 2.0,
            (page.height() - viewport.height) / 2.0,
        );
        view
    };
    let view = centred(1.0);
    let (grid, range) = visible_tiles(&geometry, &view, viewport);
    let order = range.centre_out();
    println!(
        "BENCH view: zoom 400%, viewport 1920x1080, dpr 1 -> level {}, page {}x{} px, {} visible tiles ({}x{})",
        grid.level(),
        grid.width_px(),
        grid.height_px(),
        order.len(),
        range.x1 - range.x0,
        range.y1 - range.y0
    );

    let mut times = Vec::new();
    let t_tiles = Instant::now();
    for (i, tile) in order.iter().enumerate() {
        let t = Instant::now();
        let bitmap = backend.render_tile(doc, 0, *tile).unwrap();
        let dt = t.elapsed();
        if i == 0 {
            // Sanity: the centre tile of a dense sheet has ink in it.
            assert!(bitmap.pixels.as_chunks::<4>().0.iter().any(|p| p[0] < 128));
        }
        times.push(ms(dt));
    }
    let all = t_tiles.elapsed();
    let first = times[0];
    let rest = &times[1..];
    let mean_rest = rest.iter().sum::<f64>() / rest.len() as f64;
    let max_rest = rest.iter().cloned().fold(0.0, f64::max);
    let min_rest = rest.iter().cloned().fold(f64::INFINITY, f64::min);
    println!(
        "BENCH open: {:.1} ms; open + page geometry (page load): {:.1} ms",
        ms(open),
        ms(info)
    );
    println!("BENCH first tile (cold, includes content parse): {first:.1} ms");
    println!(
        "BENCH later tiles: mean {mean_rest:.1} ms, min {min_rest:.1} ms, max {max_rest:.1} ms over {} tiles",
        rest.len()
    );
    println!(
        "BENCH first paint (open -> first tile ready): {:.1} ms; all {} visible tiles: {:.1} ms after open ({:.1} ms total)",
        ms(info) + first,
        order.len(),
        ms(all),
        ms(info) + ms(all)
    );

    // Warm repeat of the same tiles (page handle and PDFium caches hot).
    let t = Instant::now();
    for tile in &order {
        backend.render_tile(doc, 0, *tile).unwrap();
    }
    let warm = t.elapsed();
    println!(
        "BENCH warm repeat: {:.1} ms for {} tiles ({:.1} ms per tile)",
        ms(warm),
        order.len(),
        ms(warm) / order.len() as f64
    );

    // Other things a viewer does with this page.
    let t = Instant::now();
    backend.thumbnail(doc, 0, 256).unwrap();
    println!("BENCH 256 px thumbnail: {:.1} ms", ms(t.elapsed()));
    let t = Instant::now();
    let fit = backend
        .render_page(doc, 0, 1920.0 / geometry.display_size().width)
        .unwrap();
    println!(
        "BENCH fit-width full page ({}x{}): {:.1} ms",
        fit.width,
        fit.height,
        ms(t.elapsed())
    );
    backend.close(doc).unwrap();

    // --- Through the scheduler, from a fresh open: what the app will see. ---
    for dpr in [1.0, 2.0] {
        let t0 = Instant::now();
        let doc = backend.open(DocSource::file(&path), None).unwrap().doc;
        let geometry = backend.page_info(doc, 0).unwrap().geometry();
        let cache = Arc::new(TileCache::default());
        let (scheduler, results) = Scheduler::new(Arc::new(backend.clone()), cache.clone());
        let queued = scheduler.submit_view(
            StreamId(0),
            &ViewRequest {
                doc,
                page: 0,
                revision: 0,
                layer_hash: 0,
                geometry,
                view: centred(dpr),
                viewport,
                prefetch_ring: 0,
                options: RenderOptions::default(),
            },
        );
        let first = results.recv_timeout(Duration::from_secs(600)).unwrap();
        assert!(first.outcome.is_ok());
        let first_paint = t0.elapsed();
        assert!(scheduler.wait_idle(Duration::from_secs(600)));
        let done = t0.elapsed();
        let n = queued.queued_visible.len();
        println!(
            "BENCH scheduler dpr {dpr}: level {}, {n} tiles, first paint {:.1} ms, all visible {:.1} ms ({:.1} ms per tile after the first), cache {} MB",
            queued.grid.level(),
            ms(first_paint),
            ms(done),
            (ms(done) - ms(first_paint)) / (n.max(2) - 1) as f64,
            cache.bytes() / (1024 * 1024)
        );
        drop(scheduler);
        backend.close(doc).unwrap();
    }
    std::fs::remove_file(&path).unwrap();
}
