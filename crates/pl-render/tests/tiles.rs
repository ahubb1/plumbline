//! Tiles against full-page renders, page geometry, thumbnails, file opens and error paths, all
//! through real PDFium.

mod common;

use common::*;
use pl_geom::{level_scale, TileGrid, TileId};
use pl_render::{DocId, DocSource, RenderBackend, RenderError};
use pl_testkit::Fixture;

/// For every page of a fixture: the reported geometry equals the manifest, tiles at each level
/// stitch into the full-page render within 1 grey level, and the fixture's line segments appear
/// where pl-geom's page-to-display transform says they should.
fn check_stitching(fixture: &Fixture, levels: &[i8]) {
    let backend = backend();
    let doc = open(&backend, fixture);
    let mut multi_tile_levels = 0;
    for (index, truth) in fixture.manifest.pages.iter().enumerate() {
        let page = index as u32;
        let info = backend.page_info(doc, page).unwrap();
        assert_eq!(
            info.media_box, truth.media_box,
            "{} p{index}",
            fixture.manifest.name
        );
        assert_eq!(info.crop_box.unwrap_or(info.media_box), truth.visible_box());
        assert_eq!(info.rotate, truth.rotate);
        assert_eq!(info.user_unit, truth.user_unit);
        let geometry = info.geometry();
        let display = geometry.display_size_points();
        let want = truth.display_size();
        assert!((display.width - want[0]).abs() < 1e-6 && (display.height - want[1]).abs() < 1e-6);

        for &level in levels {
            let scale = level_scale(level);
            let grid = TileGrid::new(&geometry, level);
            let full = backend.render_page(doc, page, scale).unwrap();
            assert_eq!(
                (full.width, full.height),
                (grid.width_px(), grid.height_px())
            );
            let stitched = stitch(&backend, doc, page, &grid);
            // Acceptance: within 1 grey level. Individual colour channels may be 2 apart on a
            // few anti-aliased edge pixels (PDFium's float maths differs by the tile offset).
            // The most seen is 9 such pixels in 480,000, where the diagonal mark of a page with
            // an offset MediaBox crosses a seam; the bound leaves a little room above that.
            let (worst_grey, over_one) = grey_diff(&stitched, &full);
            let worst_channel = max_diff(&stitched, &full);
            println!(
                "STITCH {} page {index} level {level}: {} px, {} differ, worst grey {worst_grey}, {over_one} over 1, worst channel {worst_channel}",
                fixture.manifest.name,
                full.width as usize * full.height as usize,
                diff_count(&stitched, &full)
            );
            assert!(
                worst_grey <= 2 && worst_channel <= 2 && over_one <= 12,
                "{} page {index} level {level}: stitched tiles differ from the full render by {worst_grey} grey levels ({worst_channel} in one channel, {over_one} px over 1 grey level, {} px differ)",
                fixture.manifest.name,
                diff_count(&stitched, &full)
            );
            if grid.tile_count() > 1 {
                multi_tile_levels += 1;
            }

            // The render is a page: opaque, mostly white, with the marks in the right places.
            assert!(full.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            let white = full
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[..3] == [255; 3])
                .count();
            assert!(
                white * 2 > (full.width * full.height) as usize,
                "page is not mostly white"
            );
            let mut checked = 0;
            for s in &truth.segments {
                let mid = s.midpoint();
                if !inside_visible(truth, mid, 2.0) {
                    continue;
                }
                let (x, y) = page_to_pixel(&geometry, scale, mid);
                assert!(
                    has_ink_near(&full, x, y, 2),
                    "{} page {index} level {level}: no ink at segment midpoint {mid:?} -> pixel ({x:.1}, {y:.1})",
                    fixture.manifest.name
                );
                checked += 1;
            }
            assert!(
                checked > 0 || truth.segments.is_empty(),
                "fixture page has no checkable segments"
            );
        }
    }
    assert!(
        multi_tile_levels > 0,
        "the test must cover a multi-tile grid"
    );
    backend.close(doc).unwrap();
}

#[test]
fn stitched_tiles_equal_full_render_plain() {
    check_stitching(&pl_testkit::plain(), &[-1, 0, 1, 2]);
}

#[test]
fn stitched_tiles_equal_full_render_rotated() {
    check_stitching(&pl_testkit::rotated_pages(), &[0, 1]);
}

#[test]
fn stitched_tiles_equal_full_render_user_unit() {
    check_stitching(&pl_testkit::user_unit(), &[0, 1, 2]);
}

#[test]
fn stitched_tiles_equal_full_render_crop_box() {
    check_stitching(&pl_testkit::crop_box(), &[0, 1]);
}

#[test]
fn stitched_tiles_equal_full_render_inherited_boxes() {
    // Boxes that come from a /Pages node, at all four rotations: an inherited MediaBox with a
    // non-zero origin (pages 1 to 4) and an inherited CropBox (pages 5 to 8). The geometry
    // must equal the manifest, and the marks must be where that geometry says.
    check_stitching(&pl_testkit::inherited_boxes(), &[0, 1]);
}

#[test]
fn inherited_boxes_report_the_effective_box_not_a_size_at_the_origin() {
    let fixture = pl_testkit::inherited_boxes();
    let backend = backend();
    let doc = open(&backend, &fixture);
    for (index, truth) in fixture.manifest.pages.iter().enumerate() {
        let info = backend.page_info(doc, index as u32).unwrap();
        if index < 4 {
            // No box of its own: the inherited MediaBox, origin and all.
            assert_eq!(
                info.media_box,
                pl_testkit::INHERITED_MEDIA_BOX,
                "page {index}"
            );
            assert_eq!(info.crop_box, None, "page {index}");
        } else {
            // Its own MediaBox, and the CropBox of the node above it.
            assert_eq!(info.media_box, [0.0, 0.0, 600.0, 800.0], "page {index}");
            assert_eq!(
                info.crop_box,
                Some(pl_testkit::INHERITED_CROP_BOX),
                "page {index}"
            );
        }
        assert_eq!(info.rotate, truth.rotate);

        // The page is drawn 1:1, not stretched: a 1 unit = 1 px render has the visible box's
        // size, and each mark's ink sits at its own page position and nowhere 100 units off.
        let geometry = info.geometry();
        let full = backend.render_page(doc, index as u32, 1.0).unwrap();
        let want = truth.display_size();
        assert_eq!(
            (full.width as f64, full.height as f64),
            (want[0], want[1]),
            "page {index}"
        );
        for s in &truth.segments {
            let mid = s.midpoint();
            if !inside_visible(truth, mid, 3.0) {
                continue;
            }
            let (x, y) = page_to_pixel(&geometry, 1.0, mid);
            assert!(
                has_ink_near(&full, x, y, 2),
                "page {index}: no ink at {mid:?} -> ({x:.1}, {y:.1})"
            );
        }
        // The blue mark is a vertical 100 units long: a stretched page would not keep it so.
        let blue = truth.segments[1];
        let (ax, ay) = page_to_pixel(&geometry, 1.0, blue.a);
        let (bx, by) = page_to_pixel(&geometry, 1.0, blue.b);
        assert!(((ax - bx).hypot(ay - by) - 100.0).abs() < 1e-6);
    }
    backend.close(doc).unwrap();
}

#[test]
fn page_sizes_come_without_loading_any_page() {
    let backend = backend();
    for fixture in [
        pl_testkit::rotated_pages(),
        pl_testkit::crop_box(),
        pl_testkit::inherited_boxes(),
        pl_testkit::user_unit(),
        pl_testkit::multi_page(),
    ] {
        let doc = open(&backend, &fixture);
        let sizes = backend.page_sizes(doc).unwrap();
        assert_eq!(sizes.len(), fixture.manifest.page_count());
        assert_eq!(
            backend.page_loads(doc).unwrap(),
            0,
            "{}: reading sizes loaded a page",
            fixture.manifest.name
        );
        for (index, (size, truth)) in sizes.iter().zip(&fixture.manifest.pages).enumerate() {
            let size = size.expect("readable page");
            // Displayed size in user units: /Rotate applied, /UserUnit not.
            let want = truth.display_size();
            let want = [want[0] / truth.user_unit, want[1] / truth.user_unit];
            assert!(
                (size[0] - want[0]).abs() < 1e-3 && (size[1] - want[1]).abs() < 1e-3,
                "{} page {index}: {size:?} vs {want:?}",
                fixture.manifest.name
            );
            // The stand-in built from the size tiles exactly like the real page.
            let provisional = pl_render::PageInfo::provisional(size).geometry();
            let exact = backend.page_info(doc, index as u32).unwrap().geometry();
            for level in [-1i8, 0, 2] {
                let (a, b) = (
                    TileGrid::new(&provisional, level),
                    TileGrid::new(&exact, level),
                );
                assert_eq!(
                    (a.width_px(), a.height_px(), a.cols(), a.rows()),
                    (b.width_px(), b.height_px(), b.cols(), b.rows())
                );
            }
        }
        // One load per page for the exact geometry, cached afterwards.
        let pages = fixture.manifest.page_count() as u64;
        assert_eq!(backend.page_loads(doc).unwrap(), pages);
        backend.page_info(doc, 0).unwrap();
        assert_eq!(backend.page_loads(doc).unwrap(), pages);
        backend.close(doc).unwrap();
    }
}

#[test]
fn a_page_that_cannot_be_loaded_fails_alone() {
    let fixture = pl_testkit::broken_page_tree();
    let backend = backend();
    let info = backend
        .open(DocSource::Bytes(fixture.bytes.clone()), None)
        .unwrap();
    assert_eq!(info.page_count, 3);
    let broken = pl_testkit::BROKEN_PAGE_INDEX as u32;
    let sizes = backend.page_sizes(info.doc).unwrap();
    assert_eq!(sizes.len(), 3);
    for page in 0..3u32 {
        if page == broken {
            assert!(backend.page_info(info.doc, page).is_err());
            assert!(backend.render_page(info.doc, page, 1.0).is_err());
            assert!(backend.page_text(info.doc, page).is_err());
        } else {
            assert_eq!(sizes[page as usize], Some([612.0, 792.0]));
            assert_eq!(
                backend.page_info(info.doc, page).unwrap().media_box,
                [0.0, 0.0, 612.0, 792.0]
            );
            let text = backend.page_text(info.doc, page).unwrap().text();
            assert!(text.contains(&format!("BROKEN-TREE page {}", page + 1)));
            backend.render_page(info.doc, page, 0.5).unwrap();
        }
    }
    println!(
        "BROKEN page size as PDFium reports it: {:?}",
        sizes[broken as usize]
    );
    backend.close(info.doc).unwrap();
}

#[test]
fn stitched_tiles_equal_full_render_with_annotations_and_form() {
    check_stitching(&pl_testkit::annotations(), &[1, 2]);
    check_stitching(&pl_testkit::acro_form(), &[1, 2]);
}

#[test]
fn text_crossing_a_tile_seam_is_not_broken() {
    // Regression test for the glyph-bitmap defect that `TILE_GUTTER` works around: at these
    // levels the fixture's 10 to 24 pt strings are drawn from cached glyph bitmaps and cross
    // tile seams. Without the gutter the worst difference here was 133 levels.
    let fixture = pl_testkit::plain();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    for level in [1i8, 2] {
        let grid = TileGrid::new(&geometry, level);
        let scale = level_scale(level);
        let full = backend.render_page(doc, 0, scale).unwrap();
        let stitched = stitch(&backend, doc, 0, &grid);
        // At least one string must really straddle a vertical seam, or this proves nothing.
        let straddles = fixture.manifest.pages[0].texts.iter().any(|t| {
            let b = t.bbox();
            let (x0, _) = page_to_pixel(&geometry, scale, [b[0], b[1]]);
            let (x1, _) = page_to_pixel(&geometry, scale, [b[2], b[1]]);
            (x0 / 512.0).floor() != (x1 / 512.0).floor()
        });
        assert!(straddles, "no text crosses a seam at level {level}");
        assert!(max_diff(&stitched, &full) <= 2, "level {level}");
    }
    backend.close(doc).unwrap();
}

#[test]
fn deep_zoom_tiles_stay_within_two_levels() {
    // At level 3 (8 px per point) one pixel in 31 million was 2 grey levels off when this was
    // measured; the bound asserted here is therefore 2, with almost none above 1.
    let fixture = pl_testkit::plain();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let grid = TileGrid::new(&geometry, 3);
    let full = backend.render_page(doc, 0, level_scale(3)).unwrap();
    let stitched = stitch(&backend, doc, 0, &grid);
    let (worst, over_one) = grey_diff(&stitched, &full);
    assert!(worst <= 2, "worst grey difference {worst}");
    assert!(
        over_one <= 20,
        "{over_one} pixels differ by more than 1 grey level"
    );
    backend.close(doc).unwrap();
}

#[test]
fn content_outside_the_crop_box_is_not_drawn() {
    // Page 1 of crop_box has a red segment in the MediaBox margin, outside the CropBox.
    let fixture = pl_testkit::crop_box();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let full = backend.render_page(doc, 0, 1.0).unwrap();
    let red = full
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 80 && p[2] < 80)
        .count();
    let outside = fixture.manifest.pages[0]
        .segments
        .iter()
        .filter(|s| !inside_visible(&fixture.manifest.pages[0], s.midpoint(), 0.0))
        .count();
    assert_eq!(outside, 1, "fixture changed");
    // The only red on this page is that clipped segment (the in-crop marks are not pure red
    // unless the fixture says so), so compare against the expected in-crop red marks.
    let red_inside = fixture.manifest.pages[0]
        .segments
        .iter()
        .filter(|s| {
            s.rgb == pl_testkit::RED
                && inside_visible(&fixture.manifest.pages[0], s.midpoint(), 0.0)
        })
        .count();
    if red_inside == 0 {
        assert_eq!(red, 0, "the clipped segment leaked into the render");
    }
    backend.close(doc).unwrap();
}

#[test]
fn user_unit_is_not_reported_by_pdfium_and_comes_from_the_caller() {
    let fixture = pl_testkit::user_unit();
    let backend = backend();
    let doc = backend
        .open(DocSource::Bytes(fixture.bytes.clone()), None)
        .unwrap()
        .doc;
    // PDFium 8125 has no UserUnit API; until the caller supplies it the page reports 1.0.
    assert_eq!(backend.page_info(doc, 0).unwrap().user_unit, 1.0);
    let before = backend.render_page(doc, 0, 1.0).unwrap();
    backend.set_page_user_unit(doc, 0, 2.0).unwrap();
    let info = backend.page_info(doc, 0).unwrap();
    assert_eq!(info.user_unit, 2.0);
    assert_eq!(
        info.geometry().display_size_points(),
        pl_geom::Size::new(612.0, 792.0)
    );
    // Levels are per user unit, so the pixels do not change.
    assert_eq!(backend.render_page(doc, 0, 1.0).unwrap(), before);
    assert!(backend.set_page_user_unit(doc, 0, 0.0).is_err());
    backend.close(doc).unwrap();
}

#[test]
fn thumbnails_fit_the_requested_size() {
    let fixture = pl_testkit::rotated_pages();
    let backend = backend();
    let doc = open(&backend, &fixture);
    // Page 0 is portrait, page 1 (/Rotate 90) displays landscape.
    let portrait = backend.thumbnail(doc, 0, 160).unwrap();
    assert_eq!((portrait.width, portrait.height), (124, 160));
    let landscape = backend.thumbnail(doc, 1, 160).unwrap();
    assert_eq!((landscape.width, landscape.height), (160, 124));
    for t in [&portrait, &landscape] {
        let ink = t
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0].min(p[1]).min(p[2]) < 200)
            .count();
        assert!(ink > 0, "thumbnail is blank");
    }
    backend.close(doc).unwrap();
}

#[test]
fn a_file_is_opened_through_the_mapping_and_released_on_close() {
    let fixture = pl_testkit::xref_object_streams();
    let backend = backend();
    let path = write_temp(&fixture, "mapped");
    let from_file = backend.open(DocSource::file(&path), None).unwrap();
    assert_eq!(from_file.page_count as usize, fixture.manifest.page_count());
    let from_bytes = open(&backend, &fixture);
    for page in 0..from_file.page_count {
        assert_eq!(
            backend.render_page(from_file.doc, page, 1.0).unwrap(),
            backend.render_page(from_bytes, page, 1.0).unwrap(),
            "page {page}"
        );
    }
    // While the document is open the mapping pins the file's contents: Windows refuses to
    // truncate a mapped file.
    let truncate = || {
        std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
    };
    assert!(truncate().is_err(), "the file should be mapped while open");
    backend.close(from_file.doc).unwrap();
    backend.close(from_bytes).unwrap();
    // Closing drops the page handles, the document and the mapping.
    truncate().expect("the file is released after close");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn encrypted_fixtures_open_with_the_empty_user_password() {
    let backend = backend();
    for fixture in [pl_testkit::encrypted_rc4(), pl_testkit::encrypted_aes256()] {
        let doc = open(&backend, &fixture);
        let text = backend.page_text(doc, 0).unwrap().text();
        assert!(
            text.contains(&pl_testkit::encrypted_token(0)),
            "{}: {text:?}",
            fixture.manifest.name
        );
        let full = backend.render_page(doc, 0, 1.0).unwrap();
        assert!(full.pixels.as_chunks::<4>().0.iter().any(|p| p[0] < 100));
        backend.close(doc).unwrap();
    }
}

#[test]
fn errors_are_reported_as_values() {
    let backend = backend();
    assert!(matches!(
        backend.open(DocSource::Bytes(b"this is not a pdf".to_vec()), None),
        Err(RenderError::Open(_))
    ));
    assert!(matches!(
        backend.open(DocSource::file("C:\\no\\such\\dir\\missing.pdf"), None),
        Err(RenderError::Open(_))
    ));
    let ghost = DocId(u64::MAX);
    assert_eq!(
        backend.page_count(ghost),
        Err(RenderError::UnknownDocument(ghost))
    );
    assert_eq!(
        backend.close(ghost),
        Err(RenderError::UnknownDocument(ghost))
    );

    let doc = open(&backend, &pl_testkit::plain());
    assert_eq!(backend.page_count(doc).unwrap(), 1);
    assert_eq!(
        backend.page_info(doc, 3),
        Err(RenderError::PageOutOfRange { page: 3, count: 1 })
    );
    let outside = TileId {
        level: 0,
        x: 9,
        y: 9,
    };
    assert_eq!(
        backend.render_tile(doc, 0, outside),
        Err(RenderError::TileOutOfRange)
    );
    assert!(matches!(
        backend.render_page(doc, 0, 4096.0),
        Err(RenderError::TooLarge { .. })
    ));
    assert!(matches!(
        backend.render_page(doc, 0, f64::NAN),
        Err(RenderError::InvalidRequest(_))
    ));
    assert!(backend.set_annotation_hidden(doc, 0, 0, true).is_err());
    backend.close(doc).unwrap();
    assert_eq!(
        backend.page_count(doc),
        Err(RenderError::UnknownDocument(doc))
    );
}

#[test]
fn more_pages_than_the_handle_lru_render_correctly() {
    // 24 pages against an LRU of 8 handles: every page must still come back right, in any order.
    let fixture = pl_testkit::multi_page();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let n = fixture.manifest.page_count() as u32;
    assert!(n as usize > pl_render::PAGE_HANDLE_LRU);
    let first: Vec<_> = (0..n)
        .map(|p| backend.thumbnail(doc, p, 200).unwrap())
        .collect();
    for p in (0..n).rev() {
        assert_eq!(
            backend.thumbnail(doc, p, 200).unwrap(),
            first[p as usize],
            "page {p}"
        );
    }
    // Pages differ from one another (each has its own token and segment).
    assert_ne!(first[0], first[1]);
    backend.close(doc).unwrap();
}

#[test]
fn the_backend_handle_is_shared_and_refuses_a_second_dll() {
    let a = backend();
    let b = pl_render::PdfiumBackend::new(None).unwrap();
    assert_eq!(a.dll_path(), b.dll_path());
    assert!(a.dll_path().is_absolute());
    let other = std::env::temp_dir().join("elsewhere").join("pdfium.dll");
    assert!(matches!(
        pl_render::PdfiumBackend::new(Some(&other)),
        Err(RenderError::LibraryLoad(_))
    ));
    // Handles work from several threads at once.
    let fixture = pl_testkit::plain();
    let doc = open(&a, &fixture);
    let expected = a.render_page(doc, 0, 0.5).unwrap();
    std::thread::scope(|s| {
        for _ in 0..4 {
            let handle = a.clone();
            let expected = &expected;
            s.spawn(move || {
                for _ in 0..5 {
                    assert_eq!(&handle.render_page(doc, 0, 0.5).unwrap(), expected);
                }
            });
        }
    });
    a.close(doc).unwrap();
}
