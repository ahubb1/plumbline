//! Every modelled kind, added to a plain page and to rotated, `/UserUnit` and offset-box pages
//! and saved through pl-pdf's verified incremental save. For each kind on each page:
//!
//! * the original bytes are an exact prefix of the saved file (checked in `Work::save`);
//! * no object of the original is rewritten except the page's `/Annots` holder;
//! * loading the saved file gives a model equal to the one that was added;
//! * a further, unrelated save leaves that annotation's object alone and the model equal;
//! * PDFium's render of the saved file matches the tiny-skia raster of the `DrawList`.

mod common;

use common::{Sample, Target, Work, TARGETS};
use pl_annot::{AnnotKind, AnnotSet, Annotation, DrawList, Origin, PdfDate};
use pl_geom::Rect;
use pl_pdf::{Object, ObjectId};
use pl_render::PdfiumBackend;

/// Neighbourhood radius and grey-level tolerance of the render comparison (`common::Diff`).
const RADIUS: i32 = 1;
const TOLERANCE: i32 = 40;
/// Upper bound on the mean absolute difference, in grey levels, over the whole page.
const MAX_MEAN: f64 = 0.6;

/// Out-of-tolerance pixels allowed per case, of about 1.9 million on a page. The limits and
/// their reasons are pl-draw's (see its `tests/pdfium.rs`): single anti-aliased pixels on the
/// edge of a composited group, and for dashed strokes the few pixels where PDFium and Skia
/// join a dash at a corner or a closing point differently.
fn allowance(name: &str) -> usize {
    if name.starts_with("dash-") {
        40
    } else {
        3
    }
}

fn check_sample(
    backend: &PdfiumBackend,
    target: &Target,
    sample: &Sample,
    failures: &mut Vec<String>,
) {
    let label = format!("{}@{}", sample.name, target.name);
    let fixture = (target.fixture)();
    let mut work = Work::new(&fixture);
    let geometry = work.geometry(target.page);
    let before = work.doc().clone();
    let page_id = before.page_id(target.page).unwrap();

    // Add and save.
    let mut set = AnnotSet::load(&before);
    assert!(set.is_empty(), "{label}: the fixture has no annotations");
    for a in &sample.annotations {
        set.add(a.clone()).unwrap();
    }
    assert!(set.is_dirty());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.added.len(), sample.annotations.len(), "{label}");
    assert!(
        plan.modified.is_empty() && plan.deleted.is_empty(),
        "{label}"
    );
    assert_eq!(plan.touched_pages(), [target.page], "{label}");

    // Only the page object (which gained /Annots) was rewritten, and only in that key.
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[page_id]);
    let (Some(Object::Dictionary(old_page)), Some(Object::Dictionary(new_page))) =
        (before.object(page_id), after.object(page_id))
    else {
        panic!("{label}: page objects");
    };
    for (key, value) in old_page.iter() {
        assert_eq!(
            new_page.get(key).ok(),
            Some(value),
            "{label}: page key changed"
        );
    }
    assert_eq!(
        new_page.len(),
        old_page.len() + 1,
        "{label}: only /Annots was added"
    );
    assert_eq!(
        after.annotation_counts()[target.page],
        sample.annotations.len()
    );

    // Load: the model is equal, clean, and tied to the written objects.
    let loaded = AnnotSet::load(&after);
    assert_eq!(loaded.len(), sample.annotations.len(), "{label}");
    assert!(!loaded.is_dirty(), "{label}: a freshly loaded set is clean");
    assert!(loaded.plan_save(&after).unwrap().is_empty(), "{label}");
    for (a, change) in sample.annotations.iter().zip(&plan.added) {
        let got = loaded
            .get(&a.id)
            .unwrap_or_else(|| panic!("{label}: {} is missing", a.id));
        assert_eq!(
            got.detached(),
            a.detached(),
            "{label}: load gives an equal model"
        );
        assert_eq!(got.object_id(), Some(change.object));
        assert!(got.is_modelled());
        let dict = got.original_dict().unwrap();
        // The standard keys every annotation we write carries.
        for key in ["Type", "Subtype", "Rect", "NM", "P", "F", "M"] {
            assert!(dict.has(key.as_bytes()), "{label}: /{key} is missing");
        }
        if a.kind.is_markup() {
            for key in ["T", "Subj", "CreationDate", "CA", "C", "AP"] {
                assert!(dict.has(key.as_bytes()), "{label}: /{key} is missing");
            }
            assert_eq!(
                dict.get(b"F").unwrap(),
                &Object::Integer(4),
                "{label}: print flag"
            );
            assert!(got.baseline().unwrap().has_appearance);
        }
        // /Rect is the box of what is drawn, and equals the appearance's /BBox.
        let placement = a.placement();
        assert_eq!(
            pl_pdf::rect_from_object(dict.get(b"Rect").unwrap()),
            Some(placement.rect)
        );
        if let Some(bounds) = placement.list.bounds() {
            // To within the rounding noise of the bounds computation.
            let roomy = placement.rect.inflate(1e-3, 1e-3);
            assert_eq!(
                roomy.union(bounds),
                roomy,
                "{label}: /Rect covers the drawing"
            );
            // And is tight: no more than the rounding grid (and, for a pen, half its width)
            // beyond what is drawn.
            let room = 0.07 + a.style.width / 2.0;
            let most = bounds.union(a.geometry.bounds()).inflate(room, room);
            assert_eq!(most.union(placement.rect), most, "{label}: /Rect is tight");
        }
        if let Some(ap) = change.appearance {
            let Some(Object::Stream(stream)) = after.object(ap) else {
                panic!("{label}: the appearance is a stream");
            };
            assert_eq!(
                pl_pdf::rect_from_object(stream.dict.get(b"BBox").unwrap()),
                Some(placement.rect),
                "{label}: /BBox equals /Rect"
            );
            assert!(
                !stream.dict.has(b"Group"),
                "{label}: the top-level appearance is not a group"
            );
            assert!(stream.dict.has(b"Resources"));
        }
    }

    // A further, unrelated save: these annotations' objects are not written again and the
    // model read back is the same, baseline included.
    let mut second = loaded.clone();
    let mut extra = Annotation::square(
        target.page,
        Rect::new(0.0, 0.0, 12.0, 12.0) + work.sample_origin(target.page),
        &Origin::new("Someone Else", PdfDate::parse("D:20270101000000Z").unwrap()),
    );
    extra.style.width = 1.0;
    let extra_id = second.add(extra).unwrap();
    let (plan2, _) = work.save(&second);
    assert_eq!(plan2.added.len(), 1);
    assert!(plan2.modified.is_empty(), "{label}: nothing else was dirty");
    let last = work.doc().clone();
    let holder: Vec<ObjectId> = vec![page_id];
    common::assert_untouched(&after, &last, &holder);
    let reloaded = AnnotSet::load(&last);
    assert_eq!(reloaded.len(), sample.annotations.len() + 1);
    for a in loaded.iter() {
        assert_eq!(
            reloaded.get(&a.id),
            Some(a),
            "{label}: load-save-load gives an equal model"
        );
    }
    assert!(reloaded.contains(&extra_id));

    // PDFium's render of the first save against the tiny-skia raster of the same draw lists.
    let mut list = DrawList::new();
    for a in &sample.annotations {
        list.extend(a.draw_list().ops);
    }
    let rendered = common::render_pdf(backend, after.bytes().to_vec(), target.page, &geometry);
    let ours = common::raster_over(&list, &geometry, &rendered.backdrop);
    let drawn_pdfium = common::changed_pixels(&rendered.backdrop, &rendered.pdfium, 12);
    let drawn_ours = common::changed_pixels(&rendered.backdrop, &ours, 12);
    let d = common::diff(&ours, &rendered.pdfium, RADIUS, TOLERANCE);
    if std::env::var_os("PL_ANNOT_VERBOSE").is_some() {
        println!("{label:50} pdfium {drawn_pdfium:7} ours {drawn_ours:7} {d:?}");
    }
    let mut problems = Vec::new();
    if drawn_pdfium < 200 || drawn_ours < 200 {
        problems.push(format!(
            "barely drawn (pdfium {drawn_pdfium} px, ours {drawn_ours} px)"
        ));
    }
    if d.bad > allowance(sample.name) {
        problems.push(format!("{} pixels disagree beyond tolerance", d.bad));
    }
    if d.mean > MAX_MEAN {
        problems.push(format!("mean difference {:.3}", d.mean));
    }
    if !problems.is_empty() {
        let p = common::dump(&rendered.pdfium, &format!("{label}-pdfium"));
        common::dump(&ours, &format!("{label}-ours"));
        failures.push(format!(
            "{label}: {} (images beside {})",
            problems.join("; "),
            p.display()
        ));
    }
}

fn run(targets: &[Target]) {
    let backend = common::backend();
    let mut failures = Vec::new();
    let mut count = 0;
    for target in targets {
        let mut work = Work::new(&(target.fixture)());
        let origin = work.sample_origin(target.page);
        for sample in common::samples(target.page, origin) {
            check_sample(&backend, target, &sample, &mut failures);
            count += 1;
        }
    }
    assert!(count >= 19 * targets.len());
    assert!(
        failures.is_empty(),
        "{} of {count} cases failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_kind_round_trips_and_renders_on_a_plain_page() {
    run(&TARGETS[..1]);
}

#[test]
fn every_kind_round_trips_and_renders_on_rotated_pages() {
    run(&TARGETS[1..4]);
}

#[test]
fn every_kind_round_trips_and_renders_on_a_user_unit_page() {
    run(&TARGETS[4..5]);
}

#[test]
fn every_kind_round_trips_and_renders_on_offset_box_pages() {
    run(&TARGETS[5..]);
}

#[test]
fn the_samples_cover_every_modelled_kind() {
    let kinds: std::collections::HashSet<AnnotKind> = common::samples(0, pl_geom::Vec2::ZERO)
        .into_iter()
        .flat_map(|s| s.annotations)
        .map(|a| a.kind)
        .collect();
    for kind in AnnotKind::MODELLED {
        assert!(kinds.contains(&kind), "no sample of {kind:?}");
    }
}

/// Text under a highlight keeps its dark pixels: the highlight multiplies, it does not cover.
#[test]
fn text_under_a_highlight_keeps_its_dark_pixels() {
    let backend = common::backend();
    for target in &TARGETS[..2] {
        let fixture = (target.fixture)();
        let mut work = Work::new(&fixture);
        let geometry = work.geometry(target.page);
        let truth = &fixture.manifest.pages[target.page];
        // Highlight every text on the page.
        let quads: Vec<pl_annot::Quad> = truth
            .texts
            .iter()
            .map(|t| {
                let b = t.bbox();
                pl_draw::shapes::rect_quad(Rect::new(b[0], b[1], b[2], b[3]))
            })
            .collect();
        assert!(!quads.is_empty());
        let highlight =
            Annotation::text_markup(AnnotKind::Highlight, target.page, quads, &common::origin());
        let mut set = AnnotSet::new();
        set.add(highlight.clone()).unwrap();
        work.save(&set);

        let rendered = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
        let dark_before = common::dark_pixels(&rendered.backdrop, 80);
        let dark_after = common::dark_pixels(&rendered.pdfium, 80);
        assert!(
            dark_before > 2000,
            "{}: the page has dark text ({dark_before})",
            target.name
        );
        let lost = dark_before.abs_diff(dark_after) as f64 / dark_before as f64;
        assert!(
            lost < 0.01,
            "{}: dark pixels {dark_before} -> {dark_after}",
            target.name
        );
        // And the highlight is really there, in both renders.
        let coloured = common::changed_pixels(&rendered.backdrop, &rendered.pdfium, 40);
        assert!(
            coloured > 2_000,
            "{}: highlighted pixels {coloured}",
            target.name
        );
        let ours = common::raster_over(&highlight.draw_list(), &geometry, &rendered.backdrop);
        let d = common::diff(&ours, &rendered.pdfium, RADIUS, TOLERANCE);
        assert!(d.bad <= 3 && d.mean < MAX_MEAN, "{}: {d:?}", target.name);
        assert_eq!(
            common::dark_pixels(&ours, 80).abs_diff(dark_before) * 100 / dark_before,
            0
        );
    }
}
