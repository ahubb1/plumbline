//! Scene builds and snap queries against the pl-testkit fixtures.

use std::time::{Duration, Instant};

use pl_geom::kurbo::{CubicBez, ParamCurve, ParamCurveNearest};
use pl_geom::{PageGeometry, Point, Rect, Rotation, ViewTransform};
use pl_scene::{
    build_scene, spawn_scene_build, NoSnapReason, Scene, SceneBuild, SceneLimits, SceneSource,
    SegmentKind, SnapKind, SnapModes,
};
use pl_testkit::{Fixture, PageTruth};

fn pt(p: [f64; 2]) -> Point {
    Point::new(p[0], p[1])
}

fn build(fixture: &Fixture, page: usize) -> Scene {
    build_with(fixture, page, &SceneLimits::default())
}

fn build_with(fixture: &Fixture, page: usize, limits: &SceneLimits) -> Scene {
    let source = SceneSource::from(fixture.bytes.clone());
    match build_scene(&source, page, limits) {
        SceneBuild::Ready(scene) => scene,
        SceneBuild::NoContentSnap(reason) => {
            panic!(
                "{} page {page}: no scene: {reason:?}",
                fixture.manifest.name
            )
        }
    }
}

/// The scene's segments as plain endpoint pairs.
fn dump(scene: &Scene) -> Vec<([f64; 2], [f64; 2])> {
    scene
        .segments()
        .iter()
        .map(|s| ([s.a.x, s.a.y], [s.b.x, s.b.y]))
        .collect()
}

fn truth_dump(page: &PageTruth) -> Vec<([f64; 2], [f64; 2])> {
    page.segments.iter().map(|s| (s.a, s.b)).collect()
}

fn geometry(page: &PageTruth) -> PageGeometry {
    let rect = |r: [f64; 4]| Rect::new(r[0], r[1], r[2], r[3]);
    PageGeometry::new(
        rect(page.media_box),
        page.crop_box.map(rect),
        Rotation::from_degrees(page.rotate).unwrap(),
        page.user_unit,
    )
}

#[test]
fn plain_dump_returns_exactly_the_known_segments() {
    let fixture = pl_testkit::plain();
    let scene = build(&fixture, 0);
    let truth = &fixture.manifest.pages[0];
    assert_eq!(scene.len(), 12);
    // Bit-for-bit equal coordinates, in drawing order.
    assert_eq!(dump(&scene), truth_dump(truth));
    assert!(scene
        .segments()
        .iter()
        .all(|s| s.kind == SegmentKind::Line && s.a_vertex && s.b_vertex));
    // Each fixture segment is its own stroke.
    let paths: Vec<u32> = scene.segments().iter().map(|s| s.path).collect();
    assert_eq!(paths, (0..12).collect::<Vec<u32>>());
    assert!(scene.rects().is_empty());
    assert_eq!(scene.bounds(), Some(Rect::new(72.0, 72.0, 540.0, 720.0)));
    assert_eq!(scene.stats().paint_ops, 12);
    assert!(scene.stats().approx_bytes > 0);
}

#[test]
fn plain_snaps_return_exact_coordinates() {
    let scene = build(&pl_testkit::plain(), 0);
    let snap = |x: f64, y: f64, r: f64, modes: SnapModes| {
        let hit = scene.snap(Point::new(x, y), r, modes);
        hit.map(|h| (h.kind, h.point.x, h.point.y))
    };
    let all = SnapModes::ALL;

    // Endpoints: a frame corner, the triangle's apex, a fractional end.
    assert_eq!(
        snap(538.7, 721.1, 3.0, all),
        Some((SnapKind::Endpoint, 540.0, 720.0))
    );
    assert_eq!(
        snap(245.0, 207.0, 3.0, all),
        Some((SnapKind::Endpoint, 244.0, 208.0))
    );
    assert_eq!(
        snap(400.0, 151.0, 2.0, all),
        Some((SnapKind::Endpoint, 400.25, 150.5))
    );
    assert_eq!(
        snap(451.0, 150.0, 2.0, all),
        Some((SnapKind::Endpoint, 450.75, 150.5))
    );

    // Intersection: four segments cross at the page centre.
    assert_eq!(
        snap(306.8, 395.1, 3.0, all),
        Some((SnapKind::Intersection, 306.0, 396.0))
    );
    // The same point with only intersections allowed.
    let only_x = SnapModes::only(SnapKind::Intersection);
    let (kind, x, y) = snap(306.5, 396.5, 3.0, only_x).unwrap();
    assert_eq!((kind, x, y), (SnapKind::Intersection, 306.0, 396.0));

    // Midpoints: bottom frame edge, the 3-4-5 hypotenuse, the short segment.
    assert_eq!(
        snap(306.4, 72.6, 2.0, all),
        Some((SnapKind::Midpoint, 306.0, 72.0))
    );
    assert_eq!(
        snap(171.0, 155.0, 2.0, all),
        Some((SnapKind::Midpoint, 172.0, 154.0))
    );
    assert_eq!(
        snap(425.0, 151.0, 2.0, all),
        Some((SnapKind::Midpoint, 425.5, 150.5))
    );

    // Nearest: the foot of the perpendicular on the bottom frame edge.
    assert_eq!(
        snap(450.0, 73.0, 2.0, all),
        Some((SnapKind::Nearest, 450.0, 72.0))
    );
    // Nothing within tolerance.
    assert_eq!(snap(300.0, 200.0, 2.0, all), None);

    // With endpoints off, a corner is still found as an exact intersection.
    let no_end = SnapModes {
        endpoint: false,
        ..all
    };
    assert_eq!(
        snap(73.0, 71.0, 3.0, no_end),
        Some((SnapKind::Intersection, 72.0, 72.0))
    );
}

/// Builds every page of a fixture, checks the dump against the manifest and
/// snaps from a cursor given in view pixels, as the app will.
fn check_pages_in_page_space(fixture: &Fixture) {
    for (index, truth) in fixture.manifest.pages.iter().enumerate() {
        let ctx = format!("{} page {index}", fixture.manifest.name);
        let scene = build(fixture, index);
        // Results are in unrotated page space whatever /Rotate, /UserUnit
        // and the boxes say.
        assert_eq!(dump(&scene), truth_dump(truth), "{ctx}");

        let geom = geometry(truth);
        let view = ViewTransform::new(1.75, 1.5, pl_geom::Vec2::new(37.0, 211.0));
        let to_view = view.page_to_view(&geom);
        let to_page = view.view_to_page(&geom);
        // An 8 px tolerance in view space, in user units.
        let radius = 8.0 / view.logical_px_per_unit(&geom);
        let nudge = pl_geom::Vec2::new(3.0, -2.0);

        for (i, seg) in truth.segments.iter().enumerate() {
            // Endpoints.
            for end in [seg.a, seg.b] {
                let cursor = to_page * (to_view * pt(end) + nudge);
                let hit = scene.snap(cursor, radius, SnapModes::ALL).unwrap();
                assert_eq!(hit.kind, SnapKind::Endpoint, "{ctx} segment {i}");
                assert_eq!(hit.point, pt(end), "{ctx} segment {i}");
            }
            // Midpoint.
            let mid = seg.midpoint();
            let cursor = to_page * (to_view * pt(mid) + nudge);
            let hit = scene
                .snap(cursor, radius, SnapModes::only(SnapKind::Midpoint))
                .unwrap();
            assert_eq!(hit.point, pt(mid), "{ctx} segment {i} midpoint");
        }

        // Intersection: the first three marks meet at one corner.
        let corner = truth.segments[0].a;
        assert_eq!(truth.segments[1].a, corner);
        assert_eq!(truth.segments[2].a, corner);
        let cursor = to_page * (to_view * pt(corner) + nudge);
        let hit = scene
            .snap(cursor, radius, SnapModes::only(SnapKind::Intersection))
            .unwrap();
        assert_eq!(hit.point, pt(corner), "{ctx} intersection");
        assert!(hit.other_segment.is_some());
    }
}

#[test]
fn rotated_pages_stay_in_unrotated_page_space() {
    let fixture = pl_testkit::rotated_pages();
    assert_eq!(fixture.manifest.pages.len(), 4);
    check_pages_in_page_space(&fixture);
    // The four pages hold identical geometry although they display differently.
    let first = dump(&build(&fixture, 0));
    for page in 1..4 {
        assert_eq!(dump(&build(&fixture, page)), first);
    }
}

#[test]
fn user_unit_page_stays_in_user_units() {
    let fixture = pl_testkit::user_unit();
    assert_eq!(fixture.manifest.pages[0].user_unit, 2.0);
    check_pages_in_page_space(&fixture);
    let scene = build(&fixture, 0);
    // Inside the 306 x 396 unit page, not scaled to 612 x 792 points.
    let bounds = scene.bounds().unwrap();
    assert_eq!((bounds.x1, bounds.y1), (306.0, 396.0));
}

#[test]
fn crop_box_pages_keep_media_box_coordinates() {
    let fixture = pl_testkit::crop_box();
    assert_eq!(fixture.manifest.pages.len(), 3);
    check_pages_in_page_space(&fixture);
    // The segment outside the CropBox of page 1 is still indexed, at its
    // MediaBox coordinates.
    let scene = build(&fixture, 0);
    let hit = scene
        .snap(Point::new(4.5, 20.5), 1.0, SnapModes::ALL)
        .unwrap();
    assert_eq!(
        (hit.kind, hit.point),
        (SnapKind::Endpoint, Point::new(4.0, 20.0))
    );
}

#[test]
fn annotations_are_ignored() {
    let fixture = pl_testkit::annotations();
    let truth = &fixture.manifest.pages[0];
    assert_eq!(truth.annotations.len(), 4);
    let scene = build(&fixture, 0);
    // Only the page's own line; nothing from the four appearance streams.
    assert_eq!(dump(&scene), truth_dump(truth));
    assert_eq!(scene.len(), 1);
    assert!(scene.rects().is_empty());
    for annot in &truth.annotations {
        let r = annot.rect;
        let centre = Point::new((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
        let radius = ((r[2] - r[0]).max(r[3] - r[1])) / 2.0 + 1.0;
        let near: Vec<_> = scene
            .snap_candidates(centre, radius, SnapModes::ALL)
            .into_iter()
            .filter(|h| h.segment != 0)
            .collect();
        assert!(
            near.is_empty(),
            "{} left geometry in the scene",
            annot.subtype
        );
    }
}

#[test]
fn other_fixtures_dump_their_known_segments() {
    for fixture in [
        pl_testkit::layers(),
        pl_testkit::xref_classic(),
        pl_testkit::xref_object_streams(),
        pl_testkit::multi_page(),
        pl_testkit::acro_form(),
    ] {
        for (index, truth) in fixture.manifest.pages.iter().enumerate() {
            let scene = build(&fixture, index);
            assert_eq!(
                dump(&scene),
                truth_dump(truth),
                "{} page {index}",
                fixture.manifest.name
            );
        }
    }
}

#[test]
fn page_out_of_range_is_typed() {
    let fixture = pl_testkit::plain();
    let source = SceneSource::from(fixture.bytes);
    let result = build_scene(&source, 1, &SceneLimits::default());
    assert_eq!(result.reason(), Some(NoSnapReason::PageOutOfRange));
    let result = build_scene(&source, usize::MAX, &SceneLimits::default());
    assert_eq!(result.reason(), Some(NoSnapReason::PageOutOfRange));
}

#[test]
fn encrypted_files_degrade_and_can_be_opted_in() {
    for fixture in [pl_testkit::encrypted_rc4(), pl_testkit::encrypted_aes256()] {
        let name = fixture.manifest.name.clone();
        let source = SceneSource::from(fixture.bytes.clone());
        for page in 0..fixture.manifest.pages.len() {
            let result = build_scene(&source, page, &SceneLimits::default());
            assert_eq!(result.reason(), Some(NoSnapReason::Encrypted), "{name}");
            assert!(result.scene().is_none());
        }
        // The worker path gives the same typed result.
        let job = spawn_scene_build(source.clone(), 0, SceneLimits::default());
        assert_eq!(job.wait().reason(), Some(NoSnapReason::Encrypted), "{name}");

        // hayro can decrypt with the empty user password when asked to.
        let limits = SceneLimits {
            allow_encrypted: true,
            ..SceneLimits::default()
        };
        for (page, truth) in fixture.manifest.pages.iter().enumerate() {
            let scene = build_with(&fixture, page, &limits);
            assert_eq!(dump(&scene), truth_dump(truth), "{name} page {page}");
        }
    }
}

#[test]
fn malformed_files_degrade_without_panicking() {
    let limits = SceneLimits::default();
    let reason = |bytes: Vec<u8>| build_scene(&SceneSource::from(bytes), 0, &limits).reason();

    assert_eq!(reason(Vec::new()), Some(NoSnapReason::Malformed));
    assert_eq!(
        reason(b"%PDF-1.4\n".to_vec()),
        Some(NoSnapReason::Malformed)
    );
    let noise: Vec<u8> = (0..20_000u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .filter(|&b| b != b'/')
        .collect();
    assert_eq!(reason(noise), Some(NoSnapReason::Malformed));

    // Truncations and corruptions of a real file: any typed result is fine,
    // a panic or a hang is not.
    let good = pl_testkit::plain().bytes;
    for cut in [
        10,
        good.len() / 4,
        good.len() / 2,
        good.len() - 40,
        good.len() - 5,
    ] {
        let result = build_scene(&SceneSource::from(good[..cut].to_vec()), 0, &limits);
        if let Some(scene) = result.scene() {
            assert!(scene.len() <= 12, "cut at {cut}");
        }
    }
    for step in [7usize, 31, 101] {
        let mut bad = good.clone();
        for i in (step..bad.len()).step_by(step) {
            bad[i] = bad[i].wrapping_add(i as u8) | 1;
        }
        let job = spawn_scene_build(SceneSource::from(bad), 0, limits);
        // Must come back with some typed value.
        let _ = job.wait();
    }
}

#[test]
fn dense_sheet_builds_within_the_cap_and_queries_fast() {
    let fixture = pl_testkit::dense_sheet();
    let truth = &fixture.manifest.pages[0];
    let grid = truth.grid.as_ref().expect("dense sheet has a grid");
    assert_eq!(grid.count, 200_000);

    let limits = SceneLimits::default();
    let source = SceneSource::from(fixture.bytes.clone());
    let started = Instant::now();
    let result = spawn_scene_build(source, 0, limits).wait();
    let wall = started.elapsed();
    let scene = match result {
        SceneBuild::Ready(scene) => scene,
        SceneBuild::NoContentSnap(reason) => panic!("dense sheet degraded: {reason:?}"),
    };
    assert!(wall < limits.time_cap, "build took {wall:?}");
    assert_eq!(scene.len(), grid.count);
    assert!(scene.stats().approx_bytes < limits.max_bytes);

    // Every segment matches the manifest bit for bit.
    for (i, seg) in scene.segments().iter().enumerate() {
        let t = grid.segment(i);
        assert_eq!(
            ([seg.a.x, seg.a.y], [seg.b.x, seg.b.y]),
            (t.a, t.b),
            "segment {i}"
        );
    }

    // Snap near the end and the middle of segments spread over the sheet.
    let picks: Vec<usize> = (0..10_000).map(|k| (k * 7919) % grid.count).collect();
    let radius = 0.2;
    let started = Instant::now();
    let mut hits = 0usize;
    for &i in &picks {
        let t = grid.segment(i);
        let cursor = Point::new(t.b[0] + 0.05, t.b[1] - 0.04);
        let hit = scene.snap(cursor, radius, SnapModes::ALL).unwrap();
        assert_eq!(
            (hit.kind, hit.point),
            (SnapKind::Endpoint, pt(t.b)),
            "segment {i}"
        );
        hits += 1;
    }
    let endpoint_time = started.elapsed();
    let started = Instant::now();
    for &i in &picks {
        let t = grid.segment(i);
        let mid = t.midpoint();
        let cursor = Point::new(mid[0] + 0.03, mid[1] + 0.03);
        let hit = scene.snap(cursor, radius, SnapModes::ALL).unwrap();
        assert_eq!(hit.kind, SnapKind::Midpoint, "segment {i}");
        assert_eq!(hit.point, pt(mid), "segment {i}");
        assert_eq!(hit.segment as usize, i);
        hits += 1;
    }
    let midpoint_time = started.elapsed();
    // A wide query that touches many segments (about 10 x 10 cells).
    let wide_radius = 5.0 * (grid.pitch_cpt[0].max(grid.pitch_cpt[1]) as f64 / 100.0);
    let started = Instant::now();
    for &i in picks.iter().take(1_000) {
        let t = grid.segment(i);
        assert!(scene.snap(pt(t.a), wide_radius, SnapModes::ALL).is_some());
    }
    let wide_time = started.elapsed();

    let stats = scene.stats();
    println!(
        "MEASURE dense_sheet: segments={} wall={:?} interpret={:?} index={:?} approx_bytes={} \
         endpoint_snap={:?}/query midpoint_snap={:?}/query wide_snap(r={:.2})={:?}/query hits={}",
        scene.len(),
        wall,
        stats.interpret_time,
        stats.index_time,
        stats.approx_bytes,
        endpoint_time / picks.len() as u32,
        midpoint_time / picks.len() as u32,
        wide_radius,
        wide_time / 1_000,
        hits
    );
    // Generous bounds; the real figures are printed above.
    assert!(endpoint_time / (picks.len() as u32) < Duration::from_millis(1));
    assert!(wide_time / 1_000 < Duration::from_millis(5));
}

#[test]
fn tiny_caps_degrade_gracefully() {
    let fixture = pl_testkit::dense_sheet_with(20_000, 3);
    let source = SceneSource::from(fixture.bytes.clone());
    let run = |limits: SceneLimits| {
        let direct = build_scene(&source, 0, &limits).reason();
        let worker = spawn_scene_build(source.clone(), 0, limits).wait().reason();
        assert_eq!(direct, worker);
        direct
    };

    let segment_cap = SceneLimits {
        max_segments: 1_000,
        ..SceneLimits::default()
    };
    assert_eq!(run(segment_cap), Some(NoSnapReason::SegmentCap));

    let memory_cap = SceneLimits {
        max_bytes: 64 * 1024,
        ..SceneLimits::default()
    };
    assert_eq!(run(memory_cap), Some(NoSnapReason::MemoryCap));

    let time_cap = SceneLimits {
        time_cap: Duration::ZERO,
        ..SceneLimits::default()
    };
    assert_eq!(run(time_cap), Some(NoSnapReason::TimeCap));

    let zero = SceneLimits {
        max_segments: 0,
        ..SceneLimits::default()
    };
    assert_eq!(run(zero), Some(NoSnapReason::SegmentCap));

    // A cap exactly at the page's size still builds.
    let exact = SceneLimits {
        max_segments: 20_000,
        ..SceneLimits::default()
    };
    assert_eq!(run(exact), None);
}

#[test]
fn job_poll_cancel_and_drop() {
    let fixture = pl_testkit::dense_sheet_with(20_000, 3);
    let source = SceneSource::from(fixture.bytes);

    // Polling never blocks and hands the result out once.
    let mut job = spawn_scene_build(source.clone(), 0, SceneLimits::default());
    let started = Instant::now();
    let result = loop {
        if let Some(result) = job.poll() {
            break result;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "job never finished"
        );
        std::thread::yield_now();
    };
    assert_eq!(result.scene().map(Scene::len), Some(20_000));
    assert!(job.poll().is_none());

    // A cancelled job reports Cancelled unless it had already finished.
    let job = spawn_scene_build(source.clone(), 0, SceneLimits::default());
    job.cancel();
    match job.wait() {
        SceneBuild::Ready(scene) => assert_eq!(scene.len(), 20_000),
        SceneBuild::NoContentSnap(reason) => assert_eq!(reason, NoSnapReason::Cancelled),
    }

    // Dropping a job must not block or panic.
    drop(spawn_scene_build(source, 0, SceneLimits::default()));
}

/// A one-page PDF with the given content stream, written by hand so that it
/// can hold rectangles and curves, which the testkit fixtures do not.
fn tiny_pdf(content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << >> >>"
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

fn build_bytes(bytes: Vec<u8>, limits: &SceneLimits) -> Scene {
    match build_scene(&SceneSource::from(bytes), 0, limits) {
        SceneBuild::Ready(scene) => scene,
        SceneBuild::NoContentSnap(reason) => panic!("no scene: {reason:?}"),
    }
}

#[test]
fn rectangles_closed_paths_and_transforms() {
    let content = "10 20 100 50 re S\n\
                   q 0 1 -1 0 300 300 cm 0 0 10 20 re f Q\n\
                   50 600 m 100 600 l 100 650 l h S\n\
                   q 2 0 0 2 5 7 cm 10 10 m 20 10 l S Q\n\
                   400 100 30 40 re n";
    let scene = build_bytes(tiny_pdf(content), &SceneLimits::default());
    let p = Point::new;
    let d = dump(&scene);
    assert_eq!(
        d,
        vec![
            // Upright rectangle, four edges.
            ([10.0, 20.0], [110.0, 20.0]),
            ([110.0, 20.0], [110.0, 70.0]),
            ([110.0, 70.0], [10.0, 70.0]),
            ([10.0, 70.0], [10.0, 20.0]),
            // Filled rectangle under a quarter-turn CTM.
            ([300.0, 300.0], [300.0, 310.0]),
            ([300.0, 310.0], [280.0, 310.0]),
            ([280.0, 310.0], [280.0, 300.0]),
            ([280.0, 300.0], [300.0, 300.0]),
            // Triangle with its closing edge.
            ([50.0, 600.0], [100.0, 600.0]),
            ([100.0, 600.0], [100.0, 650.0]),
            ([100.0, 650.0], [50.0, 600.0]),
            // Line under a scale and translate.
            ([25.0, 27.0], [45.0, 27.0]),
            // The unpainted (`n`) rectangle leaves nothing.
        ]
    );
    let kinds: Vec<_> = scene.segments().iter().map(|s| s.kind).collect();
    assert_eq!(kinds[..8], [SegmentKind::RectEdge; 8]);
    assert_eq!(kinds[8..], [SegmentKind::Line; 4]);

    assert_eq!(scene.rects().len(), 2);
    assert_eq!(scene.rects()[0].first_segment, Some(0));
    assert_eq!(
        scene.rects()[0].corners,
        [p(10.0, 20.0), p(110.0, 20.0), p(110.0, 70.0), p(10.0, 70.0)]
    );
    assert_eq!(scene.rects()[1].first_segment, Some(4));
    assert_eq!(
        scene.rects()[1].corners,
        [
            p(300.0, 300.0),
            p(300.0, 310.0),
            p(280.0, 310.0),
            p(280.0, 300.0)
        ]
    );

    // Rectangle corner, edge midpoint and the closing edge's midpoint.
    let hit = scene.snap(p(110.6, 70.4), 1.0, SnapModes::ALL).unwrap();
    assert_eq!((hit.kind, hit.point), (SnapKind::Endpoint, p(110.0, 70.0)));
    let hit = scene.snap(p(60.3, 20.4), 1.0, SnapModes::ALL).unwrap();
    assert_eq!((hit.kind, hit.point), (SnapKind::Midpoint, p(60.0, 20.0)));
    let hit = scene.snap(p(75.4, 625.0), 1.0, SnapModes::ALL).unwrap();
    assert_eq!((hit.kind, hit.point), (SnapKind::Midpoint, p(75.0, 625.0)));
    // Segments of one paint operation share a path index.
    let paths: Vec<u32> = scene.segments().iter().map(|s| s.path).collect();
    assert_eq!(paths, [0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 3]);
}

#[test]
fn curves_are_flattened_with_exact_endpoints() {
    let content = "100 400 m 150 500 250 500 300 400 c 350 400 l S";
    let limits = SceneLimits::default();
    let scene = build_bytes(tiny_pdf(content), &limits);
    let p = Point::new;
    let segs = scene.segments();
    let pieces: Vec<_> = segs
        .iter()
        .filter(|s| s.kind == SegmentKind::CurvePiece)
        .collect();
    assert!(pieces.len() >= 4, "only {} pieces", pieces.len());
    assert_eq!(pieces.len() + 1, segs.len());

    // The chain is continuous, starts and ends exactly on the curve's ends,
    // and only those two ends are vertices.
    assert_eq!(pieces[0].a, p(100.0, 400.0));
    assert_eq!(pieces[pieces.len() - 1].b, p(300.0, 400.0));
    for (i, piece) in pieces.iter().enumerate() {
        assert_eq!(piece.a_vertex, i == 0);
        assert_eq!(piece.b_vertex, i + 1 == pieces.len());
        if i > 0 {
            assert_eq!(piece.a, pieces[i - 1].b);
        }
    }
    // Every joint and every piece midpoint lies within tolerance of the curve.
    let curve = CubicBez::new(
        p(100.0, 400.0),
        p(150.0, 500.0),
        p(250.0, 500.0),
        p(300.0, 400.0),
    );
    for piece in &pieces {
        for q in [piece.a, piece.b, piece.midpoint()] {
            let near = curve.nearest(q, 1e-9);
            assert!(
                near.distance_sq.sqrt() <= limits.flatten_tolerance * 1.01,
                "{q:?}"
            );
        }
    }
    assert_eq!(curve.eval(0.5), p(200.0, 475.0));

    // The following line is an ordinary segment.
    let line = segs.last().unwrap();
    assert_eq!(
        (line.a, line.b, line.kind),
        (p(300.0, 400.0), p(350.0, 400.0), SegmentKind::Line)
    );

    // Endpoint snap at the curve's ends, nothing but "nearest" along it.
    let hit = scene.snap(p(100.5, 399.5), 2.0, SnapModes::ALL).unwrap();
    assert_eq!((hit.kind, hit.point), (SnapKind::Endpoint, p(100.0, 400.0)));
    let hit = scene.snap(p(200.0, 475.3), 1.0, SnapModes::ALL).unwrap();
    assert_eq!(hit.kind, SnapKind::Nearest);
    assert!(curve.nearest(hit.point, 1e-9).distance_sq.sqrt() <= limits.flatten_tolerance);

    // A coarser tolerance gives fewer pieces; a bad one falls back to the default.
    let coarse = SceneLimits {
        flatten_tolerance: 5.0,
        ..limits
    };
    assert!(build_bytes(tiny_pdf(content), &coarse).len() < scene.len());
    let bad = SceneLimits {
        flatten_tolerance: f64::NAN,
        ..limits
    };
    assert_eq!(build_bytes(tiny_pdf(content), &bad).len(), scene.len());
}

#[test]
fn empty_page_is_an_empty_scene() {
    let scene = build_bytes(tiny_pdf("q Q"), &SceneLimits::default());
    assert!(scene.is_empty());
    assert_eq!(scene.bounds(), None);
    assert_eq!(
        scene.snap(Point::new(10.0, 10.0), 50.0, SnapModes::ALL),
        None
    );
}
