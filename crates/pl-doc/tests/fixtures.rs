//! The session layer against real PDFium and the pl-testkit fixtures. Headless: nothing here
//! opens a window.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use pl_doc::pl_geom::{Point, Rect, Rotation, Size, TileGrid, Vec2};
use pl_doc::pl_render::{
    Bitmap, PdfiumBackend, RenderBackend, Scheduler, SearchOptions, StreamId, TileCache, TileKey,
};
use pl_doc::pl_scene::{NoSnapReason, SceneLimits, SnapKind, SnapModes};
use pl_doc::{
    select_hit, DocError, FitMode, RecentFiles, SceneState, Session, SessionOptions, UserUnitPolicy,
};
use pl_testkit::{Fixture, Options};

fn vendor_dll() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    std::path::absolute(p).unwrap()
}

fn backend() -> Arc<PdfiumBackend> {
    Arc::new(PdfiumBackend::new(Some(&vendor_dll())).expect("load vendored pdfium.dll"))
}

fn session() -> Session {
    Session::with_options(backend(), SessionOptions::complete())
}

/// Writes a fixture into a fresh temporary directory.
fn on_disk(fixture: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    (dir, path)
}

fn small_fixtures() -> Vec<Fixture> {
    pl_testkit::all_fixtures_with(&Options::small())
}

#[test]
fn every_fixture_opens_from_disk_with_the_manifest_geometry() {
    let mut s = session();
    for fixture in small_fixtures() {
        let m = &fixture.manifest;
        let (_dir, path) = on_disk(&fixture);
        let id = s
            .open(&path, None)
            .unwrap_or_else(|e| panic!("open {}: {e}", m.name));
        let doc = s.document(id).unwrap();
        assert_eq!(doc.page_count() as usize, m.page_count(), "{}", m.name);
        assert_eq!(doc.title(), m.file_name());
        assert!(
            doc.warnings().is_empty(),
            "{}: {:?}",
            m.name,
            doc.warnings()
        );
        for (i, truth) in m.pages.iter().enumerate() {
            let g = doc.page_geometry(i as u32).unwrap();
            let v = truth.visible_box();
            let crop = g.crop_box();
            for (got, want) in [crop.x0, crop.y0, crop.x1, crop.y1].iter().zip(v) {
                assert!(
                    (got - want).abs() < 1e-3,
                    "{} page {i}: {crop:?} vs {v:?}",
                    m.name
                );
            }
            assert_eq!(g.rotate().degrees(), truth.rotate, "{} page {i}", m.name);
            assert_eq!(g.user_unit(), truth.user_unit, "{} page {i}", m.name);
            let d = g.display_size_points();
            let want = truth.display_size();
            assert!(
                (d.width - want[0]).abs() < 1e-3 && (d.height - want[1]).abs() < 1e-3,
                "{} page {i}: {d:?} vs {want:?}",
                m.name
            );
        }
        // The file must be closed before the temporary directory is removed.
        s.close(id).unwrap();
    }
    assert!(s.is_empty());
}

#[test]
fn user_unit_is_found_by_the_scan_and_skipped_when_told_to() {
    let fixture = pl_testkit::user_unit();
    let (_dir, path) = on_disk(&fixture);

    let mut scanning = session();
    let id = scanning.open(&path, None).unwrap();
    let doc = scanning.document(id).unwrap();
    assert_eq!(doc.page_geometry(0).unwrap().user_unit(), 2.0);
    // The backend was told, so tile grids agree with the session's geometry.
    let info = doc.backend().page_info(doc.render_id(), 0).unwrap();
    assert_eq!(info.user_unit, 2.0);
    // The lopdf parse was only borrowed for the lookup.
    assert!(!doc.pdf_is_loaded());
    scanning.close(id).unwrap();

    let mut never = Session::with_options(
        backend(),
        SessionOptions {
            user_unit: UserUnitPolicy::Never,
            ..SessionOptions::complete()
        },
    );
    let id = never.open(&path, None).unwrap();
    let doc = never.document_mut(id).unwrap();
    assert_eq!(doc.page_geometry(0).unwrap().user_unit(), 1.0);
    assert!(doc.refresh_user_units().unwrap());
    assert_eq!(doc.page_geometry(0).unwrap().user_unit(), 2.0);
    never.close(id).unwrap();

    // In memory too, and a plain file has nothing to find.
    let mut mem = session();
    let id = mem.open_bytes("uu", fixture.bytes.clone(), None).unwrap();
    assert_eq!(
        mem.document(id)
            .unwrap()
            .page_geometry(0)
            .unwrap()
            .user_unit(),
        2.0
    );
    let plain = mem
        .open_bytes("plain", pl_testkit::plain().bytes, None)
        .unwrap();
    assert_eq!(
        mem.document(plain)
            .unwrap()
            .page_geometry(0)
            .unwrap()
            .user_unit(),
        1.0
    );
}

#[test]
fn the_lopdf_document_is_created_only_when_asked_for() {
    let fixture = pl_testkit::annotations();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert!(!doc.pdf_is_loaded());
    let pdf = doc.pdf().unwrap();
    assert!(!pdf.is_loaded(), "opening the handle reads only the header");
    assert_eq!(pdf.page_count().unwrap(), fixture.manifest.page_count());
    let counts = pdf.document().unwrap().annotation_counts();
    assert_eq!(counts[0], fixture.manifest.pages[0].annotations.len());
    assert!(doc.pdf_is_loaded());
    // PDFium sees the same annotations.
    assert_eq!(doc.annotations(0).unwrap().len(), counts[0]);
    s.close(id).unwrap();

    let mut mem = session();
    let id = mem.open_bytes("a", fixture.bytes.clone(), None).unwrap();
    assert!(matches!(
        mem.document_mut(id).unwrap().pdf(),
        Err(DocError::NoFile)
    ));
}

#[test]
fn a_file_that_is_not_a_pdf_fails_to_open_and_leaves_no_document() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk.pdf");
    std::fs::write(&path, b"this is not a pdf").unwrap();
    let mut s = session();
    assert!(matches!(s.open(&path, None), Err(DocError::Render(_))));
    assert!(s.is_empty());
    // Nothing keeps the file open after the failure.
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn hit_testing_finds_known_page_points_on_rotated_cropped_and_scaled_pages() {
    let mut s = session();
    for fixture in [
        pl_testkit::rotated_pages(),
        pl_testkit::crop_box(),
        pl_testkit::user_unit(),
    ] {
        let name = fixture.manifest.name.clone();
        let id = s.open_bytes(&name, fixture.bytes.clone(), None).unwrap();
        let doc = s.document(id).unwrap();
        for view_rotation in Rotation::ALL {
            let mut view = doc.new_view(Size::new(1200.0, 900.0), 1.5);
            view.set_rotation(view_rotation);
            view.set_zoom(1.7);
            for (i, truth) in fixture.manifest.pages.iter().enumerate() {
                let page = i as u32;
                let b = truth.visible_box();
                let centre = Point::new((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
                // Put the middle of the page in the middle of the viewport, then look it up.
                view.scroll_point_to(page, centre, Point::new(600.0, 450.0));
                for seg in truth.segments.iter().take(6) {
                    let p = Point::new(seg.a[0], seg.a[1]);
                    let on_screen = view.page_to_view(page, p).unwrap();
                    let hit = view.hit_test_nearest(on_screen).unwrap();
                    let inside_box = p.x > b[0] && p.x < b[2] && p.y > b[1] && p.y < b[3];
                    if hit.page != page {
                        // Only possible for a point outside the visible box that lands nearer
                        // a neighbouring page.
                        assert!(!inside_box, "{name} page {i}: {p:?} hit page {}", hit.page);
                        continue;
                    }
                    assert!(
                        (hit.point - p).hypot() < 1e-6,
                        "{name} page {i} view {view_rotation:?}: {p:?} came back as {:?}",
                        hit.point
                    );
                    if inside_box {
                        assert!(hit.inside, "{name} page {i}: {p:?} should be on the page");
                    }
                }
            }
        }
    }
}

#[test]
fn a_drag_over_a_string_selects_it_and_matches_the_extracted_text() {
    let fixture = pl_testkit::plain();
    let mut s = session();
    let id = s.open_bytes("plain", fixture.bytes.clone(), None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let text = doc.page_text(0).unwrap();
    let truth = &fixture.manifest.pages[0];
    assert!(!truth.texts.is_empty());
    for t in &truth.texts {
        let y = t.origin[1] + 0.3 * t.font_size;
        let a = Point::new(t.origin[0] + 0.5, y);
        // Far to the right on the same line: the nearest character is the string's last one.
        let b = Point::new(t.origin[0] + 5000.0, y);
        let fwd = doc.select_text(0, a, b).unwrap().unwrap();
        assert_eq!(fwd.text, t.text, "drag over {:?}", t.text);
        let back = doc.select_text(0, b, a).unwrap().unwrap();
        assert_eq!(back, fwd, "direction must not matter");
        // The copied string is exactly what PDFium extracted for those characters.
        let extracted: String = text.chars[fwd.start..fwd.end]
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(fwd.text, extracted);
        // One line, and the quad sits on the string: starts at its origin, spans its height.
        assert_eq!(fwd.quads.len(), 1);
        let r = fwd.bounds().unwrap();
        assert!((r[0] - t.origin[0]).abs() < 2.0, "{r:?} vs {:?}", t.origin);
        assert!(r[1] < t.origin[1] + 0.1 && r[3] > t.origin[1] + 0.5 * t.font_size);
        // PDFium substitutes the font, so the width is only close to the Helvetica metric.
        assert!(
            ((r[2] - r[0]) - t.width).abs() < 0.15 * t.width + 2.0,
            "{:?}: width {} vs {}",
            t.text,
            r[2] - r[0],
            t.width
        );
    }

    // A drag from the first string to the last selects everything between, in reading order.
    let first = &truth.texts[0];
    let last = truth.texts.last().unwrap();
    let all = doc
        .select_text(
            0,
            Point::new(first.origin[0] + 0.5, first.origin[1] + 2.0),
            Point::new(last.origin[0] + 5000.0, last.origin[1] + 2.0),
        )
        .unwrap()
        .unwrap();
    for t in &truth.texts {
        assert!(
            all.text.contains(&t.text),
            "{:?} missing from {:?}",
            t.text,
            all.text
        );
    }
    assert!(all.quads.len() >= truth.texts.len());
}

#[test]
fn selection_works_through_the_view_on_a_rotated_page() {
    let fixture = pl_testkit::rotated_pages();
    let mut s = session();
    let id = s.open_bytes("rot", fixture.bytes.clone(), None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let mut view = doc.new_view(Size::new(1000.0, 800.0), 1.0);
    for (i, truth) in fixture.manifest.pages.iter().enumerate() {
        let page = i as u32;
        let t = &truth.texts[0];
        let y = t.origin[1] + 0.3 * t.font_size;
        let a = Point::new(t.origin[0] + 0.5, y);
        let b = Point::new(t.origin[0] + t.width - 0.5, y);
        view.scroll_point_to(page, a, Point::new(500.0, 400.0));
        // Screen positions of the two ends, as a mouse would report them.
        let a_view = view.page_to_view(page, a).unwrap();
        let b_view = view.page_to_view(page, b).unwrap();
        let ha = view.hit_test_nearest(a_view).unwrap();
        let hb = view.hit_test_nearest(b_view).unwrap();
        assert_eq!((ha.page, hb.page), (page, page));
        let sel = doc.select_text(page, ha.point, hb.point).unwrap().unwrap();
        assert_eq!(sel.text, t.text, "page {i} (/Rotate {})", truth.rotate);
    }
}

#[test]
fn search_finds_each_page_token_and_steps_through_hits() {
    let fixture = pl_testkit::multi_page();
    let mut s = session();
    let id = s.open_bytes("multi", fixture.bytes.clone(), None).unwrap();
    let doc = s.document_mut(id).unwrap();

    // A token that exists on exactly one page.
    let token = pl_testkit::multi_page_token(16);
    let found = doc.search(&token, SearchOptions::default()).unwrap();
    assert_eq!(found.len(), 1);
    let hit = &found.hits()[0];
    assert_eq!(hit.page, 16);
    let text = doc.page_text(16).unwrap();
    assert_eq!(select_hit(&text, hit).unwrap().text, token);
    // The hit's quad sits where the manifest says the token was drawn.
    let truth = &fixture.manifest.pages[16].texts[1];
    let q = pl_doc::quads_bounds(&hit.quads).unwrap();
    assert!((q[0] - truth.origin[0]).abs() < 2.0 && (q[1] - truth.origin[1]).abs() < 4.0);

    // Something on every page, case-insensitively, in page order.
    let mut every = doc.search("plumb-page", SearchOptions::default()).unwrap();
    assert_eq!(every.len(), fixture.manifest.page_count());
    let pages: Vec<u32> = every.hits().iter().map(|h| h.page).collect();
    assert_eq!(pages, (0..24).collect::<Vec<u32>>());
    assert_eq!(every.next_hit().unwrap().page, 0);
    assert_eq!(every.next_hit().unwrap().page, 1);
    assert_eq!(every.previous_hit().unwrap().page, 0);
    assert_eq!(
        every.previous_hit().unwrap().page,
        23,
        "previous wraps to the end"
    );
    assert_eq!(every.next_hit().unwrap().page, 0, "next wraps to the start");
    assert_eq!(every.select_first_from(10).unwrap().page, 10);

    // Case-sensitive search for the lower-case form finds nothing.
    let strict = SearchOptions {
        match_case: true,
        whole_word: false,
    };
    assert!(doc.search("plumb-page", strict).unwrap().is_empty());
    assert!(doc
        .search("no such words here", SearchOptions::default())
        .unwrap()
        .is_empty());

    // Stepped search reaches the same result.
    let mut stepped = doc.start_search("Sheet", SearchOptions::default());
    let mut rounds = 0;
    while !doc.step_search(&mut stepped, 5).unwrap() {
        rounds += 1;
    }
    assert_eq!(rounds, 4);
    assert_eq!(stepped.len(), 24);

    // Going to a hit brings it on screen.
    let mut view = doc.new_view(Size::new(900.0, 600.0), 1.0);
    let hit = every.set_current(20).unwrap().clone();
    let q = pl_doc::quads_bounds(&hit.quads).unwrap();
    let rect = Rect::new(q[0], q[1], q[2], q[3]);
    view.reveal_rect(hit.page, rect, 16.0);
    assert!(view.visible_pages().contains(&20));
    let at = view.page_to_view(20, rect.center()).unwrap();
    assert!(
        at.x > 0.0 && at.x < 900.0 && at.y > 0.0 && at.y < 600.0,
        "{at:?}"
    );
}

#[test]
fn bookmarks_form_the_two_level_tree_and_lead_to_their_pages() {
    let fixture = pl_testkit::multi_page();
    let mut s = session();
    let id = s.open_bytes("multi", fixture.bytes.clone(), None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let tree = doc.bookmarks().unwrap();
    let truth = &fixture.manifest.outline;
    assert_eq!(tree.len(), truth.len());
    assert_eq!(tree.len(), 28);
    assert_eq!(tree.roots.len(), 4);
    for root in &tree.roots {
        assert_eq!(root.children.len(), 6);
    }
    for (row, want) in tree.flatten().iter().zip(truth) {
        assert_eq!(row.node.title, want.title);
        assert_eq!(row.depth, want.level);
        assert_eq!(row.node.page, Some(want.page_index as u32));
        let p = row.node.point().expect("XYZ destination");
        assert!((p.x - want.dest[0]).abs() < 1e-3 && (p.y - want.dest[1]).abs() < 1e-3);
    }
    assert_eq!(tree.path_for_page(7), Some(vec![1, 1]));

    // Following a bookmark puts its destination at the top of the view.
    let mut view = doc.new_view(Size::new(900.0, 500.0), 1.0);
    let node = tree.get(&[2, 3]).unwrap();
    let page = node.page.unwrap();
    assert_eq!(page, 15);
    view.go_to_destination(page, node.x, node.y);
    assert_eq!(view.current_page(), Some(15));
    let at = view.page_to_view(page, node.point().unwrap()).unwrap();
    assert!((at.y - pl_doc::DEFAULT_PAGE_MARGIN).abs() < 1e-6, "{at:?}");

    // A document without an outline has an empty tree.
    let plain = s
        .open_bytes("plain", pl_testkit::plain().bytes, None)
        .unwrap();
    assert!(s
        .document_mut(plain)
        .unwrap()
        .bookmarks()
        .unwrap()
        .is_empty());
}

#[test]
fn scenes_are_built_per_page_on_demand_and_snap_to_known_endpoints() {
    let fixture = pl_testkit::plain();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let truth = &fixture.manifest.pages[0];

    // The first request never blocks; polling gets there.
    let mut state = doc.scene(0);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while matches!(state, SceneState::Pending) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
        state = doc.scene(0);
    }
    let scene = state.scene().expect("scene built").clone();
    assert_eq!(scene.len(), truth.segments.len());

    // Snap through a view: a cursor a few pixels from an endpoint lands exactly on it.
    let mut view = doc.new_view(Size::new(1000.0, 800.0), 1.0);
    view.set_zoom(2.0);
    let geometry = *doc.page_geometry(0).unwrap();
    for seg in &truth.segments {
        let end = Point::new(seg.a[0], seg.a[1]);
        view.scroll_point_to(0, end, Point::new(500.0, 400.0));
        let cursor = view.page_to_view(0, end).unwrap() + Vec2::new(2.0, -1.5);
        let hit = view.hit_test_nearest(cursor).unwrap();
        let radius = 8.0
            / view
                .view_transform(0)
                .unwrap()
                .logical_px_per_unit(&geometry);
        let snap = scene
            .snap(hit.point, radius, SnapModes::only(SnapKind::Endpoint))
            .expect("endpoint within 8 px");
        assert_eq!((snap.point.x, snap.point.y), (seg.a[0], seg.a[1]));
    }

    // Asking again returns the cached scene, and the file is not kept mapped by it.
    assert!(Arc::ptr_eq(doc.scene_wait(0).scene().unwrap(), &scene));
    assert!(matches!(
        doc.scene(1),
        SceneState::Unavailable(NoSnapReason::PageOutOfRange)
    ));
    s.close(id).unwrap();
    std::fs::write(&path, b"replaced").expect("nothing holds the file after close");
}

#[test]
fn scene_caps_and_encryption_degrade_to_no_content_snap() {
    let mut s = session();

    // Encrypted: off by default, as the plan says ...
    let enc = pl_testkit::encrypted_rc4();
    let id = s.open_bytes("enc", enc.bytes.clone(), None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert!(matches!(
        doc.scene_wait(0),
        SceneState::Unavailable(NoSnapReason::Encrypted)
    ));
    // ... and the session still reads its text through PDFium.
    let token = pl_testkit::encrypted_token(0);
    assert_eq!(
        doc.search(&token, SearchOptions::default()).unwrap().len(),
        1
    );

    // A segment cap far below the sheet's content.
    let dense = pl_testkit::dense_sheet_with(2_000, 3);
    let id = s.open_bytes("dense", dense.bytes.clone(), None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.set_scene_limits(SceneLimits {
        max_segments: 100,
        ..SceneLimits::default()
    });
    assert!(matches!(
        doc.scene_wait(0),
        SceneState::Unavailable(NoSnapReason::SegmentCap)
    ));
    // The failure is remembered, not retried on every call.
    assert!(matches!(
        doc.scene(0),
        SceneState::Unavailable(NoSnapReason::SegmentCap)
    ));
    // After the page changes, a build with the default caps succeeds.
    doc.set_scene_limits(SceneLimits::default());
    doc.invalidate_page(0);
    let state = doc.scene_wait(0);
    assert_eq!(state.scene().expect("built").len(), 2_000);
}

/// A page index, its tile grid and the rendered visible tiles.
type RenderedPage = (u32, TileGrid, Vec<(pl_doc::pl_geom::TileId, Arc<Bitmap>)>);

/// Renders every visible tile of a view through the scheduler and pastes them where the view
/// says they go, at one device pixel per bitmap pixel of the tile level.
fn render_visible(
    backend: &Arc<PdfiumBackend>,
    doc: &pl_doc::Document,
    view: &pl_doc::ViewState,
) -> Vec<RenderedPage> {
    let cache = Arc::new(TileCache::default());
    let dyn_backend: Arc<dyn RenderBackend> = backend.clone();
    let (scheduler, _results) = Scheduler::new(dyn_backend, cache.clone());
    let pages = view.visible_tiles();
    assert!(!pages.is_empty());
    let mut queued = Vec::new();
    for (i, t) in pages.iter().enumerate() {
        let request = t.view_request(doc.render_id(), doc.page_revision(t.page), 0, 0);
        let q = scheduler.submit_view(StreamId(i as u32), &request);
        // The scheduler picks the same grid and tiles the view listed.
        assert_eq!(q.grid, t.grid);
        assert_eq!(q.visible, t.range);
        queued.push(q);
    }
    assert!(
        scheduler.wait_idle(Duration::from_secs(60)),
        "renders did not finish"
    );
    assert_eq!(scheduler.stats().failed, 0);
    pages
        .iter()
        .map(|t| {
            let tiles = t
                .range
                .iter()
                .map(|id| {
                    let key =
                        TileKey::new(doc.render_id(), t.page, doc.page_revision(t.page), id, 0);
                    let bitmap = cache
                        .get(&key)
                        .unwrap_or_else(|| panic!("tile {id:?} missing"));
                    let px = t.grid.tile_pixel_rect(id).unwrap();
                    assert_eq!((bitmap.width, bitmap.height), (px.width, px.height));
                    (id, bitmap)
                })
                .collect();
            (t.page, t.grid, tiles)
        })
        .collect()
}

#[test]
fn the_visible_tile_set_renders_through_the_scheduler_at_every_view_rotation() {
    let backend = backend();
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    for fixture in [pl_testkit::rotated_pages(), pl_testkit::crop_box()] {
        let id = s
            .open_bytes(&fixture.manifest.name, fixture.bytes.clone(), None)
            .unwrap();
        let doc = s.document(id).unwrap();
        for view_rotation in Rotation::ALL {
            let mut view = doc.new_view(Size::new(1100.0, 850.0), 1.0);
            view.set_rotation(view_rotation);
            view.set_zoom(1.5);
            // Park the view across a page boundary so two pages contribute tiles.
            let r1 = view.layout().page_rect(1).unwrap();
            view.set_scroll(Vec2::new(40.0, r1.y0 - 400.0));
            assert_eq!(view.visible_pages(), 0..2);
            let rendered = render_visible(&backend, doc, &view);
            assert_eq!(rendered.len(), 2);
            for (page, grid, tiles) in &rendered {
                // Tiles match a direct full-page render of the same level, so the ids the view
                // computed really are the backend's tiles for that part of the page.
                let full = backend
                    .render_page(doc.render_id(), *page, grid.scale())
                    .unwrap();
                assert_eq!(
                    (full.width, full.height),
                    (grid.width_px(), grid.height_px())
                );
                let mut inked = 0usize;
                for (id, bitmap) in tiles {
                    let px = grid.tile_pixel_rect(*id).unwrap();
                    for (x, y) in [
                        (0, 0),
                        (px.width / 2, px.height / 2),
                        (px.width - 1, px.height - 1),
                    ] {
                        let a = bitmap.grey(x, y);
                        let b = full.grey(px.x + x, px.y + y);
                        assert!(
                            a.abs_diff(b) <= 2,
                            "page {page} tile {id:?} ({x},{y}): {a} vs {b}"
                        );
                    }
                    inked += bitmap
                        .pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .filter(|p| p[0] < 128)
                        .count();
                }
                assert!(inked > 0, "page {page}: the visible tiles are blank");
            }
        }
    }
}

#[test]
fn fit_page_shows_one_whole_page_of_a_mixed_document() {
    let fixture = pl_testkit::crop_box();
    let mut s = session();
    let id = s.open_bytes("crop", fixture.bytes.clone(), None).unwrap();
    let doc = s.document(id).unwrap();
    let mut view = doc.new_view(Size::new(1000.0, 700.0), 2.0);
    view.set_fit(FitMode::Page);
    for page in 0..doc.page_count() {
        view.go_to_page(page);
        assert_eq!(view.current_page(), Some(page));
        let r = view.page_rect_in_view(page).unwrap();
        assert!(
            r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= 1000.0 && r.y1 <= 700.0,
            "{r:?}"
        );
        // The page's size on screen is its display size at the fitted zoom.
        let d = doc.page_geometry(page).unwrap().display_size_points();
        let scale = view.zoom() * 96.0 / 72.0;
        assert!((r.width() - d.width * scale).abs() < 1e-6);
        assert!((r.height() - d.height * scale).abs() < 1e-6);
        let tiles = view.visible_tiles();
        assert!(tiles.iter().any(|t| t.page == page));
    }
}

#[test]
fn suspending_the_mirror_lets_the_file_be_replaced_and_reopening_picks_it_up() {
    let fixture = pl_testkit::plain();
    let (_dir, path) = on_disk(&fixture);
    let mut s = session();
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.page_count(), 1);
    doc.page_text(0).unwrap();

    doc.suspend_render().unwrap();
    // With the mirror closed the file is free to be replaced, as a save would.
    std::fs::write(&path, pl_testkit::multi_page().bytes).expect("file is not locked");
    doc.reopen_render(Vec::new()).unwrap();
    assert_eq!(doc.page_count(), 24);
    assert_eq!(doc.revision(), 1);
    let token = pl_testkit::multi_page_token(5);
    assert!(doc.page_text(5).unwrap().text().contains(&token));
    s.close(id).unwrap();
}

#[test]
fn recent_files_follow_what_the_session_opens_in_an_injected_directory() {
    let data = tempfile::tempdir().unwrap();
    let fixture_a = pl_testkit::plain();
    let fixture_b = pl_testkit::multi_page();
    let (_da, a) = on_disk(&fixture_a);
    let (_db, b) = on_disk(&fixture_b);

    let mut s = session();
    let mut recent = RecentFiles::load(data.path());
    for path in [&a, &b, &a] {
        let id = s.open(path, None).unwrap();
        let doc = s.document(id).unwrap();
        recent.touch(doc.path().unwrap());
    }
    assert_eq!(
        s.len(),
        2,
        "the second open of the same file reuses the document"
    );
    let mut view = s
        .document(s.find_by_path(&b).unwrap())
        .unwrap()
        .new_view(Size::new(800.0, 600.0), 1.0);
    view.go_to_page(9);
    recent.set_last_page(&b, view.current_page().unwrap());
    recent.save().unwrap();

    let back = RecentFiles::load(data.path());
    let paths: Vec<&Path> = back.entries().iter().map(|e| e.path.as_path()).collect();
    assert_eq!(paths, [a.as_path(), b.as_path()]);
    assert_eq!(back.get(&b).unwrap().last_page, 9);
    assert!(back.file().starts_with(data.path()));
    // Close before the temporary directories go away.
    let ids: Vec<_> = s.documents().map(|d| d.id()).collect();
    for id in ids {
        s.close(id).unwrap();
    }
}
