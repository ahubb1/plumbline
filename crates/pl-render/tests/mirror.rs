//! The render mirror: form-field display, the hidden-annotation spike, the layer-override
//! spike, and the scheduler driving real PDFium.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use pl_geom::{level_scale, Size, TileGrid, Vec2, ViewTransform};
use pl_render::{
    Bitmap, DocSource, RenderBackend, RenderOptions, Scheduler, StreamId, TileCache, TileKey,
    ViewRequest,
};

const SCALE: f64 = 2.0;

/// Pixels that differ between two renders, split into those inside and outside a page-space
/// rectangle (grown by `grow` points).
fn changed_pixels(
    a: &Bitmap,
    b: &Bitmap,
    geometry: &pl_geom::PageGeometry,
    rect: [f64; 4],
    grow: f64,
) -> (usize, usize) {
    let (x0, y0) = page_to_pixel(geometry, SCALE, [rect[0] - grow, rect[3] + grow]);
    let (x1, y1) = page_to_pixel(geometry, SCALE, [rect[2] + grow, rect[1] - grow]);
    let (mut inside, mut outside) = (0, 0);
    for y in 0..a.height {
        for x in 0..a.width {
            if a.pixel(x, y) != b.pixel(x, y) {
                let (fx, fy) = (x as f64, y as f64);
                if fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1 {
                    inside += 1;
                } else {
                    outside += 1;
                }
            }
        }
    }
    (inside, outside)
}

#[test]
fn the_filled_form_field_shows_its_value() {
    let fixture = pl_testkit::acro_form();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let full = backend.render_page(doc, 0, SCALE).unwrap();
    assert_eq!(fixture.manifest.form_fields.len(), 2);
    for field in &fixture.manifest.form_fields {
        // The value is drawn inside the widget: look at the band where the manifest says the
        // text sits, well inside the border.
        let size = field.font_size;
        let width = pl_testkit::helvetica_width(&field.value, size);
        let text_band = [
            field.text_origin[0],
            field.text_origin[1],
            field.text_origin[0] + width,
            field.text_origin[1] + 0.7 * size,
        ];
        let ink = ink_in_rect(&full, &geometry, SCALE, text_band);
        assert!(
            ink > 40,
            "field {:?}: only {ink} dark pixels where its value should be",
            field.name
        );
        // The rest of the field, to the right of the value, is empty.
        let blank = [
            text_band[2] + size,
            text_band[1],
            field.rect[2] - 3.0,
            text_band[3],
        ];
        if blank[2] > blank[0] + 5.0 {
            assert_eq!(
                ink_in_rect(&full, &geometry, SCALE, blank),
                0,
                "field {:?}",
                field.name
            );
        }
    }
    // The two values differ in length, and so does the amount of ink.
    let ink_of = |i: usize| {
        let f = &fixture.manifest.form_fields[i];
        ink_in_rect(
            &full,
            &geometry,
            SCALE,
            [
                f.rect[0] + 2.0,
                f.rect[1] + 2.0,
                f.rect[2] - 2.0,
                f.rect[3] - 2.0,
            ],
        )
    };
    assert!(
        ink_of(0) > ink_of(1),
        "the longer value should have more ink"
    );

    // The widgets are listed as annotations of the mirror, and a tile shows the same value.
    let annots = backend.annotations(doc, 0).unwrap();
    assert_eq!(annots.len(), 2);
    assert!(
        annots.iter().all(|a| a.subtype == "Widget" && !a.hidden),
        "{annots:?}"
    );
    let grid = TileGrid::new(&geometry, 1);
    assert_eq!(level_scale(1), SCALE);
    assert!(max_diff(&stitch(&backend, doc, 0, &grid), &full) <= 2);
    backend.close(doc).unwrap();
}

/// Spike (a): setting the hidden flag of one annotation in the in-memory document changes the
/// render at once, without saving, and only under that annotation.
#[test]
fn spike_hiding_an_annotation_changes_the_render_without_saving() {
    let fixture = pl_testkit::annotations();
    let backend = backend();
    let path = write_temp(&fixture, "hidden");
    let doc = backend.open(DocSource::file(&path), None).unwrap().doc;
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let truths = &fixture.manifest.pages[0].annotations;

    let listed = backend.annotations(doc, 0).unwrap();
    assert_eq!(listed.len(), truths.len());
    for (got, want) in listed.iter().zip(truths) {
        assert!(!got.hidden);
        assert_eq!(got.name.as_deref(), Some(want.name.as_str()));
        for k in 0..4 {
            assert!(
                (got.rect[k] - want.rect[k]).abs() < 0.01,
                "{got:?} vs {want:?}"
            );
        }
    }
    let square = truths.iter().position(|a| a.subtype == "Square").unwrap();
    let rect = truths[square].rect;

    let before = backend.render_page(doc, 0, SCALE).unwrap();
    let drawn = ink_in_rect(&before, &geometry, SCALE, rect);
    assert!(
        drawn > 200,
        "the square should be drawn from its appearance ({drawn} px)"
    );

    // Warm a tile under the square first, so the test also shows that a cached page handle
    // picks the change up.
    let grid = TileGrid::new(&geometry, 1);
    let tile = grid
        .tiles_covering_page_rect(
            &geometry,
            pl_geom::Rect::new(rect[0], rect[1], rect[2], rect[3]),
        )
        .iter()
        .next()
        .unwrap();
    let tile_before = backend.render_tile(doc, 0, tile).unwrap();

    backend
        .set_annotation_hidden(doc, 0, square as u32, true)
        .unwrap();
    assert!(backend.annotations(doc, 0).unwrap()[square].hidden);
    let hidden = backend.render_page(doc, 0, SCALE).unwrap();
    let (inside, outside) = changed_pixels(&before, &hidden, &geometry, rect, 2.0);
    println!("SPIKE hidden-flag: {inside} px changed under the annotation, {outside} elsewhere");
    assert!(
        inside > 200,
        "hiding the square changed only {inside} pixels"
    );
    assert_eq!(outside, 0, "hiding one annotation changed pixels elsewhere");
    assert_eq!(
        ink_in_rect(&hidden, &geometry, SCALE, rect),
        0,
        "the square is still visible"
    );
    assert_ne!(backend.render_tile(doc, 0, tile).unwrap(), tile_before);

    // Rendering without annotations hides all of them, hidden flag or not.
    let bare = backend
        .render_page_with(doc, 0, SCALE, RenderOptions { annotations: false })
        .unwrap();
    assert!(
        diff_count(&bare, &hidden) > 0,
        "the other annotations should still be drawn"
    );
    assert_eq!(ink_in_rect(&bare, &geometry, SCALE, rect), 0);

    // Showing it again restores the original pixels exactly.
    backend
        .set_annotation_hidden(doc, 0, square as u32, false)
        .unwrap();
    assert_eq!(backend.render_page(doc, 0, SCALE).unwrap(), before);
    assert_eq!(backend.render_tile(doc, 0, tile).unwrap(), tile_before);

    // Nothing was saved: the file on disk is byte-identical, and a fresh open still shows it.
    backend
        .set_annotation_hidden(doc, 0, square as u32, true)
        .unwrap();
    backend.close(doc).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
    let again = backend.open(DocSource::file(&path), None).unwrap().doc;
    assert!(!backend.annotations(again, 0).unwrap()[square].hidden);
    assert_eq!(backend.render_page(again, 0, SCALE).unwrap(), before);
    backend.close(again).unwrap();
    std::fs::remove_file(&path).unwrap();
}

/// Giving an annotation a new rectangle in the in-memory document moves what PDFium draws for
/// it (its own appearance stream, which this application may not be able to regenerate) at
/// once, without saving: the old place is empty, the new place has the ink, nothing else
/// changes, and the file is untouched.
#[test]
fn moving_an_annotation_rect_redraws_it_at_its_new_place_without_saving() {
    let fixture = pl_testkit::annotations();
    let backend = backend();
    let path = write_temp(&fixture, "moved");
    let doc = backend.open(DocSource::file(&path), None).unwrap().doc;
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let listed = backend.annotations(doc, 0).unwrap();
    let index = listed.iter().position(|a| a.subtype == "Square").unwrap();
    let rect = listed[index].rect;
    let before = backend.render_page(doc, 0, SCALE).unwrap();
    let drawn = ink_in_rect(&before, &geometry, SCALE, rect);
    assert!(drawn > 200, "the square is drawn ({drawn} px)");

    // Somewhere on the page that is blank now: try a few shifts.
    let (w, h) = (rect[2] - rect[0], rect[3] - rect[1]);
    let page = geometry.crop_box();
    let target = [
        (0.0, -1.5),
        (1.5, 0.0),
        (0.0, 1.5),
        (-1.5, 0.0),
        (1.5, 1.5),
        (-1.5, -1.5),
        (0.0, -3.0),
        (0.0, 3.0),
    ]
    .into_iter()
    .map(|(fx, fy)| {
        let (dx, dy) = (fx * w, fy * h);
        [rect[0] + dx, rect[1] + dy, rect[2] + dx, rect[3] + dy]
    })
    .find(|r| {
        r[0] > page.x0 + 6.0
            && r[1] > page.y0 + 6.0
            && r[2] < page.x1 - 6.0
            && r[3] < page.y1 - 6.0
            && ink_in_rect(
                &before,
                &geometry,
                SCALE,
                [r[0] - 3.0, r[1] - 3.0, r[2] + 3.0, r[3] + 3.0],
            ) == 0
    })
    .expect("a blank place to move the square to");

    // Opposite corners in any order are accepted.
    backend
        .set_annotation_rect(
            doc,
            0,
            index as u32,
            [target[2], target[3], target[0], target[1]],
        )
        .unwrap();
    let relisted = backend.annotations(doc, 0).unwrap();
    assert_eq!(relisted.len(), listed.len());
    for k in 0..4 {
        assert!(
            (relisted[index].rect[k] - target[k]).abs() < 0.01,
            "{:?} vs {target:?}",
            relisted[index].rect
        );
    }
    let after = backend.render_page(doc, 0, SCALE).unwrap();
    assert_eq!(
        ink_in_rect(&after, &geometry, SCALE, rect),
        0,
        "the old place is empty"
    );
    let moved = ink_in_rect(&after, &geometry, SCALE, target);
    assert!(
        moved * 10 >= drawn * 8 && moved * 8 <= drawn * 10,
        "{moved} px at the new place, {drawn} px before"
    );
    let (in_old, outside_old) = changed_pixels(&before, &after, &geometry, rect, 2.0);
    let (in_new, _) = changed_pixels(&before, &after, &geometry, target, 2.0);
    println!("moved rect: {in_old} px changed at the old place, {in_new} at the new");
    assert!(in_old > 200 && in_new > 200);
    assert_eq!(
        outside_old, in_new,
        "pixels changed outside the old and the new rectangle"
    );

    // Putting it back restores the original pixels exactly.
    backend
        .set_annotation_rect(doc, 0, index as u32, rect)
        .unwrap();
    assert_eq!(backend.render_page(doc, 0, SCALE).unwrap(), before);

    // An index that does not exist and a rectangle that is not finite are refused.
    assert!(backend.set_annotation_rect(doc, 0, 999, target).is_err());
    assert!(backend
        .set_annotation_rect(doc, 0, index as u32, [0.0, 0.0, f64::INFINITY, 1.0])
        .is_err());

    // Nothing was saved.
    backend
        .set_annotation_rect(doc, 0, index as u32, target)
        .unwrap();
    backend.close(doc).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
    std::fs::remove_file(&path).unwrap();
}

/// The bytes to append to `original` so that the named layers are off, from pl-pdf's
/// layer-override builder (the only code in the workspace that writes PDF objects).
fn layer_override_tail(original: &[u8], off: &[&str]) -> Vec<u8> {
    let doc = pl_pdf::PdfDocument::from_bytes(original.to_vec(), None).unwrap();
    let ids: Vec<_> = doc
        .layers()
        .iter()
        .filter(|layer| off.contains(&layer.name.as_str()))
        .map(|layer| layer.id)
        .collect();
    assert_eq!(ids.len(), off.len(), "layer names not found");
    doc.layer_override_tail(&ids).unwrap()
}

/// Spike (b): layer visibility for rendering. PDFium has no API to switch optional-content
/// groups, so the mirror is reopened from the original file plus an in-memory appended
/// `/OCProperties /D` override. The file itself is never modified.
#[test]
fn spike_layer_visibility_changes_with_an_appended_override() {
    let fixture = pl_testkit::layers();
    let backend = backend();
    let path = write_temp(&fixture, "layers");
    let layers = &fixture.manifest.layers;
    assert_eq!(layers.len(), 2);

    let open_with = |off: &[&str]| {
        let appended = if off.is_empty() {
            Vec::new()
        } else {
            layer_override_tail(&fixture.bytes, off)
        };
        backend
            .open(
                DocSource::File {
                    path: path.clone(),
                    appended,
                },
                None,
            )
            .unwrap()
            .doc
    };
    // How many of a layer's segments are visible in a render.
    let visible_segments = |bitmap: &Bitmap, geometry: &pl_geom::PageGeometry, layer: usize| {
        layers[layer]
            .segments
            .iter()
            .filter(|s| {
                // Sample a quarter of the way along, away from crossings with the other layer.
                let p = [
                    s.a[0] + 0.03 * (s.b[0] - s.a[0]),
                    s.a[1] + 0.03 * (s.b[1] - s.a[1]),
                ];
                let (x, y) = page_to_pixel(geometry, SCALE, p);
                has_ink_near(bitmap, x, y, 2)
            })
            .count()
    };

    let both = open_with(&[]);
    let geometry = backend.page_info(both, 0).unwrap().geometry();
    let all_on = backend.render_page(both, 0, SCALE).unwrap();
    assert_eq!(
        visible_segments(&all_on, &geometry, 0),
        layers[0].segments.len()
    );
    assert_eq!(
        visible_segments(&all_on, &geometry, 1),
        layers[1].segments.len()
    );

    // Layer A off: its lines and text vanish, layer B and the base content stay.
    let a_off = open_with(&[layers[0].name.as_str()]);
    let render_a_off = backend.render_page(a_off, 0, SCALE).unwrap();
    assert_eq!(
        visible_segments(&render_a_off, &geometry, 0),
        0,
        "layer A is still drawn"
    );
    assert_eq!(
        visible_segments(&render_a_off, &geometry, 1),
        layers[1].segments.len()
    );
    let a_text = layers[0].texts[0].bbox();
    let b_text = layers[1].texts[0].bbox();
    assert_eq!(ink_in_rect(&render_a_off, &geometry, SCALE, a_text), 0);
    assert!(ink_in_rect(&render_a_off, &geometry, SCALE, b_text) > 20);
    assert!(ink_in_rect(&all_on, &geometry, SCALE, a_text) > 20);
    println!(
        "SPIKE layer override: {} px differ with layer A off",
        diff_count(&all_on, &render_a_off)
    );

    // Both off: only the base content is left.
    let none = open_with(&[layers[0].name.as_str(), layers[1].name.as_str()]);
    let render_none = backend.render_page(none, 0, SCALE).unwrap();
    assert_eq!(visible_segments(&render_none, &geometry, 0), 0);
    assert_eq!(visible_segments(&render_none, &geometry, 1), 0);
    let base = &fixture.manifest.pages[0].texts[0];
    assert_eq!(base.text, "BASE");
    assert!(ink_in_rect(&render_none, &geometry, SCALE, base.bbox()) > 20);

    // The original mirror is unaffected, and so is the file.
    assert_eq!(backend.render_page(both, 0, SCALE).unwrap(), all_on);
    for doc in [both, a_off, none] {
        backend.close(doc).unwrap();
    }
    assert_eq!(std::fs::read(&path).unwrap(), fixture.bytes);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn the_scheduler_fills_the_cache_from_real_pdfium() {
    let fixture = pl_testkit::plain();
    let backend = backend();
    let doc = open(&backend, &fixture);
    let geometry = backend.page_info(doc, 0).unwrap().geometry();
    let budget = 6 * (512 * 512 * 4 + 1024);
    let cache = Arc::new(TileCache::new(budget));
    let (scheduler, results) = Scheduler::new(Arc::new(backend.clone()), cache.clone());

    let request = ViewRequest {
        doc,
        page: 0,
        revision: 0,
        layer_hash: 0,
        geometry,
        view: ViewTransform::new(2.0, 1.0, Vec2::new(300.0, 400.0)),
        viewport: Size::new(900.0, 700.0),
        prefetch_ring: 1,
        options: RenderOptions::default(),
    };
    let queued = scheduler.submit_view(StreamId(0), &request);
    assert!(queued.queued_visible.len() >= 4);
    assert!(scheduler.wait_idle(Duration::from_secs(60)));

    let finished: Vec<_> = results.try_iter().collect();
    assert_eq!(
        finished.len(),
        queued.queued_visible.len() + queued.queued_prefetch.len()
    );
    assert!(finished.iter().all(|r| r.outcome.is_ok()));
    assert!(cache.stats().peak_bytes <= budget);
    assert!(cache.bytes() <= budget);

    // Whatever is still cached equals a direct render of that tile.
    let mut compared = 0;
    for tile in queued.queued_visible.iter().chain(&queued.queued_prefetch) {
        if let Some(cached) = cache.get(&TileKey::new(doc, 0, 0, *tile, 0)) {
            assert_eq!(*cached, backend.render_tile(doc, 0, *tile).unwrap());
            compared += 1;
        }
    }
    assert!(compared > 0);
    drop(scheduler);
    backend.close(doc).unwrap();
}
