//! Scales, viewports, measurements, snapping and the takeoff definitions through the whole
//! path: the JSON command driver or the `Document` API, a verified save, and a fresh session
//! reading the saved file back. Every value is checked against its analytic figure on the
//! plain, rotated, `/UserUnit 2` and CropBox/MediaBox-offset fixtures. Headless: PDFium
//! renders nothing to a screen, and every file lives in a temporary directory.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{AnnotId, CountSymbol, Geometry, MeasureKind, Origin, PdfDate, ScaleRef};
use pl_doc::pl_geom::{Point, Rect, Unit};
use pl_doc::pl_pdf::{Object, PdfDocument};
use pl_doc::pl_render::PdfiumBackend;
use pl_doc::script::{self, LogOnly, Script};
use pl_doc::{
    Command, DocSettings, Document, DocumentId, EditError, LegendDef, MeasureShape, PageScalesData,
    ScaleSetting, ScaleStore, SequenceDef, Session, SessionOptions, SnapKind, SnapOptions,
    SnapOutcome, SpaceDef,
};
use pl_testkit::Fixture;
use serde_json::json;

const FT: f64 = 0.3048;

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

fn on_disk(fixture: &Fixture) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(fixture.manifest.file_name());
    std::fs::write(&path, &fixture.bytes).unwrap();
    (dir, path)
}

fn session(backend: &Arc<PdfiumBackend>, journal: Option<&Path>) -> Session {
    let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
    s.set_journal_dir(journal.map(Path::to_path_buf));
    s
}

fn origin() -> Origin {
    Origin::new("Measurer", PdfDate::parse("D:20261124101500Z").unwrap())
}

fn when() -> PdfDate {
    PdfDate::parse("D:20261124111500Z").unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn setting(text: &str) -> ScaleSetting {
    ScaleSetting::parse(text, None).unwrap()
}

fn run(session: &mut Session, steps: serde_json::Value, base: &Path) -> Vec<String> {
    let script: Script = serde_json::from_value(
        json!({ "author": "Driver", "date": "D:20261124101500Z", "steps": steps }),
    )
    .unwrap();
    let mut log = LogOnly::default();
    if let Err(e) = script::run(session, &script, base, &mut log) {
        panic!("{e}\n{}", log.lines.join("\n"));
    }
    log.lines
}

/// One measurement the driver makes on a page, with its analytic value at 1" = 20' on a page
/// of user unit `uu`.
struct Planned {
    id: String,
    step: serde_json::Value,
    expect: serde_json::Value,
}

/// Every kind of measurement, placed relative to the corner `(ox, oy)` of the page's visible
/// box, with what each must come to. One page unit is 20/72 ft times the user unit.
fn plan(page: usize, ox: f64, oy: f64, uu: f64) -> Vec<Planned> {
    let p = |x: f64, y: f64| json!([ox + x, oy + y]);
    let k = 20.0 / 72.0 * FT * uu; // metres per page unit
    let feet = |page_units: f64| format!("{:.2}'", page_units * 20.0 / 72.0 * uu);
    let rect = vec![p(0.0, 0.0), p(144.0, 0.0), p(144.0, 72.0), p(0.0, 72.0)];
    let hole = vec![p(10.0, 10.0), p(46.0, 10.0), p(46.0, 46.0), p(10.0, 46.0)];
    let n = page + 1;
    let id = |kind: &str| format!("{kind}-{n}");
    let sq_ft = |page_sq: f64| format!("{:.2} sq ft", page_sq * (20.0 / 72.0 * uu).powi(2));
    vec![
        Planned {
            id: id("length"),
            step: json!({"op": "measure", "id": id("length"), "kind": "Length", "page": n,
                         "points": [p(0.0, 100.0), p(72.0, 100.0)]}),
            expect: json!({"length": 72.0 * k, "text": feet(72.0)}),
        },
        Planned {
            id: id("polylength"),
            step: json!({"op": "measure", "id": id("polylength"), "kind": "Polylength",
                         "page": n, "points": [p(0.0, 120.0), p(72.0, 120.0), p(72.0, 156.0)]}),
            expect: json!({"length": 108.0 * k, "text": feet(108.0)}),
        },
        Planned {
            id: id("perimeter"),
            step: json!({"op": "measure", "id": id("perimeter"), "kind": "Perimeter",
                         "page": n, "points": rect}),
            expect: json!({"length": 432.0 * k, "text": feet(432.0)}),
        },
        Planned {
            id: id("area"),
            step: json!({"op": "measure", "id": id("area"), "kind": "Area", "page": n,
                         "points": rect, "cutouts": [hole]}),
            expect: json!({"area": (144.0 * 72.0 - 36.0 * 36.0) * k * k, "length": 432.0 * k,
                           "text": sq_ft(144.0 * 72.0 - 36.0 * 36.0)}),
        },
        Planned {
            id: id("volume"),
            step: json!({"op": "measure", "id": id("volume"), "kind": "Volume", "page": n,
                         "points": rect, "depth": "0.5 m"}),
            expect: json!({"volume": 144.0 * 72.0 * k * k * 0.5}),
        },
        Planned {
            id: id("angle"),
            step: json!({"op": "measure", "id": id("angle"), "kind": "Angle", "page": n,
                         "points": [p(72.0, 0.0), p(0.0, 0.0), p(36.0, 36.0)]}),
            expect: json!({"angle": 45.0, "text": "45.0\u{b0}"}),
        },
        Planned {
            id: id("radius"),
            step: json!({"op": "measure", "id": id("radius"), "kind": "Radius", "page": n,
                         "points": [p(100.0, 120.0), p(100.0, 156.0)]}),
            expect: json!({"length": 36.0 * k, "text": format!("R {}", feet(36.0))}),
        },
        Planned {
            id: id("diameter"),
            step: json!({"op": "measure", "id": id("diameter"), "kind": "Diameter", "page": n,
                         "points": [p(0.0, 170.0), p(144.0, 170.0)]}),
            expect: json!({"length": 144.0 * k, "text": format!("\u{d8} {}", feet(144.0))}),
        },
        Planned {
            id: id("count"),
            step: json!({"op": "measure", "id": id("count"), "kind": "Count", "page": n,
                         "points": [p(5.0, 5.0), p(15.0, 5.0)], "symbol": "Diamond"}),
            expect: json!({"count": 2, "text": "2"}),
        },
    ]
}

fn fixtures() -> Vec<Fixture> {
    vec![
        pl_testkit::plain(),
        pl_testkit::rotated_pages(),
        pl_testkit::user_unit(),
        pl_testkit::crop_box(),
    ]
}

/// Every measurement of a document as (id, text, quantities, rotation).
fn rows(doc: &mut Document) -> Vec<(String, String, String, u16)> {
    let rows = doc.measurements().unwrap();
    rows.iter()
        .map(|r| {
            let a = doc.annotation(&r.id).unwrap();
            (
                r.id.0.clone(),
                r.text.clone(),
                format!("{:?}", r.quantities),
                a.measurement().unwrap().rotation,
            )
        })
        .collect()
}

#[test]
fn the_driver_measures_saves_and_reads_back_analytic_values_on_every_fixture() {
    let backend = backend();
    for fixture in fixtures() {
        let name = fixture.manifest.name.clone();
        let (dir, path) = on_disk(&fixture);
        let saved = dir.path().join("measured.pdf");
        let mut steps = vec![
            json!({"op": "open", "path": path}),
            json!({"op": "set_scale", "scale": "1\" = 20'"}),
        ];
        let mut checks = Vec::new();
        for (i, page) in fixture.manifest.pages.iter().enumerate() {
            let vb = page.visible_box();
            for planned in plan(i, vb[0] + 20.0, vb[1] + 20.0, page.user_unit) {
                steps.push(planned.step);
                let mut expect = planned.expect;
                expect["op"] = json!("expect_measure");
                expect["id"] = json!(planned.id);
                // Every kind takes the page's scale, a count too (Revu's /BBMeasure).
                expect["ratio"] = json!("1 in = 20 ft");
                expect["viewport"] = json!("");
                checks.push(expect);
            }
        }
        steps.extend(checks.iter().cloned());
        // A cutout added, a count marker added and one taken off, a depth: each recomputes.
        steps.push(json!({"op": "count", "id": "count-1", "add": [300, 300]}));
        steps.push(json!({"op": "count", "id": "count-1", "remove": 0}));
        steps.push(json!({"op": "expect_measure", "id": "count-1", "count": 2, "text": "2"}));
        steps.push(json!({"op": "save_as", "path": saved}));
        steps.push(json!({"op": "expect", "count": checks.len(), "modified": false}));
        let mut s = session(&backend, None);
        let log = run(&mut s, json!(steps), dir.path());
        assert!(log.iter().any(|l| l.contains("save_as")), "{name}");
        let id = s.documents().next().unwrap().id();
        let before = rows(s.document_mut(id).unwrap());
        assert_eq!(before.len(), checks.len(), "{name}");
        drop(s);

        // A new session reads the saved file: the same values, captions and scales.
        let mut s = session(&backend, None);
        let mut again = vec![json!({"op": "open", "path": saved})];
        again.extend(checks.iter().cloned());
        for i in 0..fixture.manifest.page_count() {
            again.push(
                json!({"op": "expect_scale", "page": i + 1, "ratio": "1 in = 20 ft",
                              "viewports": []}),
            );
        }
        run(&mut s, json!(again), dir.path());
        let id = s.find_by_path(&saved).unwrap();
        let doc = s.document_mut(id).unwrap();
        let after = rows(doc);
        assert_eq!(after, before, "{name}: captions and values read back equal");
        // The caption reads upright on the displayed page.
        for (i, page) in fixture.manifest.pages.iter().enumerate() {
            let a = doc
                .annotation(&AnnotId(format!("length-{}", i + 1)))
                .unwrap();
            assert_eq!(
                a.measurement().unwrap().rotation as i32,
                page.rotate,
                "{name} page {}",
                i + 1
            );
        }
        assert!(!doc.is_modified() && !doc.editor().unwrap().has_unsaved());
        // The file holds standard dimension annotations with /Measure and every page a /VP.
        let pdf = PdfDocument::from_bytes(std::fs::read(&saved).unwrap(), None).unwrap();
        for i in 0..pdf.page_count() {
            let page = pdf
                .object(pdf.page_id(i).unwrap())
                .unwrap()
                .as_dict()
                .unwrap();
            assert!(page.get(b"VP").is_ok(), "{name} page {}: /VP", i + 1);
            let store = ScaleStore::load(&pdf);
            assert_eq!(
                store.page(i).unwrap().page_scale.as_ref().unwrap().ratio,
                "1 in = 20 ft"
            );
        }
        // The original bytes are untouched at the front of the saved file.
        assert!(
            std::fs::read(&saved).unwrap().starts_with(&fixture.bytes),
            "{name}"
        );
    }
}

/// A Letter page document on disk, its store loaded, at 1" = 20'.
fn plain_doc(backend: &Arc<PdfiumBackend>) -> (tempfile::TempDir, PathBuf, Session, DocumentId) {
    let (dir, path) = on_disk(&pl_testkit::plain());
    let mut s = session(backend, None);
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.set_page_scale(0, Some(setting("1\" = 20'"))).unwrap();
    (dir, path, s, id)
}

fn length_m(doc: &Document, id: &AnnotId) -> f64 {
    doc.annotation(id)
        .unwrap()
        .quantities()
        .unwrap()
        .length
        .unwrap()
}

#[test]
fn a_scale_change_recomputes_every_dependent_measurement_in_one_undo_step() {
    let backend = backend();
    let (_dir, path, mut s, id) = plain_doc(&backend);
    let doc = s.document_mut(id).unwrap();
    let o = origin();
    let p = Point::new;
    let len = doc
        .add_measurement(
            0,
            MeasureShape::Length {
                a: p(100.0, 300.0),
                b: p(172.0, 300.0),
            },
            &o,
        )
        .unwrap();
    let area = doc
        .add_measurement(
            0,
            MeasureShape::Area {
                outline: vec![
                    p(100.0, 400.0),
                    p(244.0, 400.0),
                    p(244.0, 472.0),
                    p(100.0, 472.0),
                ],
                cutouts: Vec::new(),
            },
            &o,
        )
        .unwrap();
    let count = doc
        .add_measurement(
            0,
            MeasureShape::Count {
                points: vec![p(50.0, 50.0)],
                symbol: CountSymbol::Star,
            },
            &o,
        )
        .unwrap();
    let plain_line = doc
        .apply(Command::add(pl_doc::pl_annot::Annotation::line(
            0,
            p(10.0, 10.0),
            p(60.0, 10.0),
            &o,
        )))
        .unwrap();
    assert!(plain_line.changed);
    assert!(close(length_m(doc, &len), 20.0 * FT));
    let steps = doc.editor().unwrap().undo_len();

    let out = doc.set_page_scale(0, Some(setting("1\" = 40'"))).unwrap();
    assert_eq!(doc.editor().unwrap().undo_len(), steps + 1, "one undo step");
    let mut touched = out.touched.clone();
    touched.sort();
    // The count carries the scale too (its value does not change); the plain line has none.
    let mut want = vec![len.clone(), area.clone(), count.clone()];
    want.sort();
    assert_eq!(touched, want, "the plain line has no scale");
    assert_eq!(
        doc.annotation(&count).unwrap().measurement().unwrap().spec.as_ref().unwrap().ratio,
        "1 in = 40 ft"
    );
    assert!(close(length_m(doc, &len), 40.0 * FT));
    let a = doc.annotation(&area).unwrap();
    assert!(close(
        a.quantities().unwrap().area.unwrap(),
        80.0 * 40.0 * FT * FT
    ));
    assert_eq!(a.meta.contents.as_deref(), Some("3200.00 sq ft"));
    assert_eq!(
        doc.annotation(&len).unwrap().meta.contents.as_deref(),
        Some("40.00'")
    );

    assert!(doc.undo().unwrap());
    assert!(close(length_m(doc, &len), 20.0 * FT));
    assert_eq!(
        doc.annotation(&len).unwrap().meta.contents.as_deref(),
        Some("20.00'")
    );
    assert_eq!(
        doc.page_scales(0)
            .unwrap()
            .page_scale
            .as_ref()
            .unwrap()
            .ratio,
        "1 in = 20 ft"
    );
    assert!(doc.redo().unwrap());
    assert!(close(length_m(doc, &len), 40.0 * FT));

    // Saved and read back: the new scale and the recomputed values.
    assert!(doc.save().unwrap().wrote());
    assert!(!doc.editor().unwrap().has_unsaved());
    drop(s);
    let mut s = session(&backend, None);
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert!(close(length_m(doc, &len), 40.0 * FT));
    assert_eq!(
        doc.annotation(&len).unwrap().meta.contents.as_deref(),
        Some("40.00'")
    );
    assert_eq!(
        doc.annotation(&count).unwrap().meta.contents.as_deref(),
        Some("1")
    );
    // A scale change after the save is undone and written like any change.
    doc.set_page_scale(0, Some(setting("1:100"))).unwrap();
    assert!(doc.editor().unwrap().has_unsaved());
    assert!(doc.undo().unwrap());
    assert!(
        !doc.editor().unwrap().has_unsaved(),
        "back to what the file has"
    );
}

#[test]
fn the_topmost_viewport_holding_a_measurement_decides_its_scale() {
    let backend = backend();
    let (_dir, path, mut s, id) = plain_doc(&backend);
    let doc = s.document_mut(id).unwrap();
    let o = origin();
    let p = Point::new;
    doc.add_viewport(
        0,
        "Big",
        [100.0, 100.0, 400.0, 400.0],
        Some(setting("1\" = 10'")),
    )
    .unwrap();
    doc.add_viewport(
        0,
        "Small",
        [200.0, 200.0, 300.0, 300.0],
        Some(setting("1\" = 5'")),
    )
    .unwrap();
    let length = |doc: &mut Document, a: Point, b: Point| {
        doc.add_measurement(0, MeasureShape::Length { a, b }, &o)
            .unwrap()
    };
    let small = length(doc, p(210.0, 250.0), p(282.0, 250.0));
    let big = length(doc, p(110.0, 150.0), p(182.0, 150.0));
    let across = length(doc, p(250.0, 250.0), p(450.0, 250.0));
    let outside = length(doc, p(450.0, 600.0), p(522.0, 600.0));
    let scale_of = |doc: &Document, id: &AnnotId| {
        doc.annotation(id)
            .unwrap()
            .measurement()
            .unwrap()
            .scale
            .clone()
    };
    assert!(close(length_m(doc, &small), 5.0 * FT));
    assert_eq!(scale_of(doc, &small), ScaleRef::Viewport("Small".into()));
    assert!(close(length_m(doc, &big), 10.0 * FT));
    assert_eq!(scale_of(doc, &big), ScaleRef::Viewport("Big".into()));
    assert!(
        close(length_m(doc, &across), 200.0 / 72.0 * 20.0 * FT),
        "not all inside: page"
    );
    assert_eq!(scale_of(doc, &across), ScaleRef::Page);
    assert!(close(length_m(doc, &outside), 20.0 * FT));

    // Editing the small viewport's scale changes only what is in it, in one step.
    let steps = doc.editor().unwrap().undo_len();
    let out = doc
        .edit_viewport(
            0,
            "Small",
            [200.0, 200.0, 300.0, 300.0],
            Some(setting("1\" = 2'")),
        )
        .unwrap();
    assert_eq!(out.touched, vec![small.clone()]);
    assert_eq!(doc.editor().unwrap().undo_len(), steps + 1);
    assert!(close(length_m(doc, &small), 2.0 * FT));
    assert_eq!(
        doc.annotation(&small).unwrap().meta.contents.as_deref(),
        Some("2.00'")
    );
    // Removing it hands the measurement to the viewport beneath.
    doc.remove_viewport(0, "Small").unwrap();
    assert!(close(length_m(doc, &small), 10.0 * FT));
    assert_eq!(scale_of(doc, &small), ScaleRef::Viewport("Big".into()));
    assert!(doc.undo().unwrap());
    assert!(close(length_m(doc, &small), 2.0 * FT));
    assert_eq!(scale_of(doc, &small), ScaleRef::Viewport("Small".into()));

    // What is refused changes nothing.
    let err = doc
        .add_viewport(0, "Big", [0.0, 0.0, 9.0, 9.0], None)
        .unwrap_err();
    assert!(err.to_string().contains("taken"), "{err}");
    assert!(doc
        .add_viewport(0, "Flat", [0.0, 0.0, 0.0, 9.0], None)
        .is_err());
    assert!(doc.remove_viewport(0, "Nowhere").is_err());
    assert!(doc
        .apply(Command::SetPageScale {
            page: 7,
            scale: None
        })
        .is_err());

    // Saved and read back: the /VP keeps both, and each measurement its viewport.
    doc.save().unwrap();
    drop(s);
    let mut s = session(&backend, None);
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    let names: Vec<String> = doc
        .page_scales(0)
        .unwrap()
        .viewports
        .iter()
        .map(|v| v.name.clone())
        .collect();
    assert_eq!(names, ["Big", "Small"]);
    assert_eq!(
        doc.page_scales(0).unwrap().get("Small").unwrap().bbox,
        Rect::new(200.0, 200.0, 300.0, 300.0)
    );
    assert!(close(length_m(doc, &small), 2.0 * FT));
    assert_eq!(scale_of(doc, &small), ScaleRef::Viewport("Small".into()));
    assert_eq!(scale_of(doc, &big), ScaleRef::Viewport("Big".into()));
    assert!(close(length_m(doc, &outside), 20.0 * FT));
}

#[test]
fn calibration_takes_the_user_unit_into_account() {
    let backend = backend();
    for (fixture, page) in [
        (pl_testkit::plain(), 0u32),
        (pl_testkit::user_unit(), 0),
        (pl_testkit::rotated_pages(), 1),
        (pl_testkit::crop_box(), 2),
    ] {
        let (_dir, path) = on_disk(&fixture);
        let uu = fixture.manifest.pages[page as usize].user_unit;
        let mut s = session(&backend, None);
        let id = s.open(&path, None).unwrap();
        let doc = s.document_mut(id).unwrap();
        // 144 units picked as 40 ft: every unit is 40/144 ft whatever the user unit.
        let (a, b) = (Point::new(150.0, 300.0), Point::new(294.0, 300.0));
        doc.calibrate(page, a, b, 40.0 * FT, Unit::Foot, None)
            .unwrap();
        let o = origin();
        let id = doc
            .add_measurement(
                page,
                MeasureShape::Length {
                    a,
                    b: Point::new(222.0, 300.0),
                },
                &o,
            )
            .unwrap();
        assert!(
            close(length_m(doc, &id), 20.0 * FT),
            "{}",
            fixture.manifest.name
        );
        assert_eq!(
            doc.annotation(&id).unwrap().meta.contents.as_deref(),
            Some("20.00'")
        );
        // The paper side of the scale is what the page really is: 144 units of 1/72 in.
        let scale = doc
            .calibrated_scale(page, a, b, 40.0 * FT, Unit::Foot)
            .unwrap();
        assert!(close(
            scale.factor(),
            40.0 * FT / (144.0 * uu * 0.0254 / 72.0)
        ));
        assert!(doc.calibrated_scale(page, a, a, 1.0, Unit::Foot).is_err());
    }
}

/// Polls the snap service until the page's scene is built (it never blocks).
fn snap_ready(doc: &mut Document, page: u32, at: Point, options: &SnapOptions) -> SnapOutcome {
    let first = doc.snap(page, at, options);
    assert!(
        matches!(first, SnapOutcome::Pending | SnapOutcome::Snapped(_)),
        "{first:?}"
    );
    doc.scene_wait(page);
    doc.snap(page, at, options)
}

fn hit(outcome: &SnapOutcome) -> (SnapKind, Point) {
    let t = outcome.target().unwrap_or_else(|| panic!("{outcome:?}"));
    (t.kind, t.point)
}

#[test]
fn the_snap_service_finds_exact_fixture_points_and_markup_vertices() {
    let backend = backend();
    let options = SnapOptions::all(3.0);
    // The plain page: an endpoint, the crossing at the centre, a midpoint.
    let (_dir, _path, mut s, id) = plain_doc(&backend);
    let doc = s.document_mut(id).unwrap();
    let out = snap_ready(doc, 0, Point::new(244.6, 207.5), &options);
    assert_eq!(hit(&out), (SnapKind::Endpoint, Point::new(244.0, 208.0)));
    let out = doc.snap(0, Point::new(307.0, 395.2), &options);
    assert_eq!(
        hit(&out),
        (SnapKind::Intersection, Point::new(306.0, 396.0))
    );
    let out = doc.snap(0, Point::new(244.5, 154.3), &options);
    assert_eq!(hit(&out), (SnapKind::Midpoint, Point::new(244.0, 154.0)));
    let out = doc.snap(0, Point::new(200.0, 101.0), &options);
    assert_eq!(hit(&out), (SnapKind::Nearest, Point::new(200.0, 100.0)));
    assert_eq!(
        doc.snap(0, Point::new(580.0, 30.0), &options),
        SnapOutcome::Nothing
    );

    // A markup's corner, and a measurement's end beside it (which wins).
    let o = origin();
    let square = pl_doc::pl_annot::Annotation::square(0, Rect::new(400.0, 400.0, 450.0, 450.0), &o);
    let square_id = square.id.clone();
    doc.apply(Command::add(square)).unwrap();
    let out = doc.snap(0, Point::new(450.8, 449.5), &options);
    assert_eq!(
        hit(&out),
        (SnapKind::MarkupVertex, Point::new(450.0, 450.0))
    );
    assert_eq!(out.target().unwrap().annotation.as_ref(), Some(&square_id));
    let len = doc
        .add_measurement(
            0,
            MeasureShape::Length {
                a: Point::new(452.0, 452.0),
                b: Point::new(500.0, 452.0),
            },
            &o,
        )
        .unwrap();
    let out = doc.snap(0, Point::new(451.0, 451.0), &options);
    assert_eq!(
        hit(&out),
        (SnapKind::MeasureEndpoint, Point::new(452.0, 452.0))
    );
    // While it is being dragged it is not snapped to.
    let dragging = SnapOptions {
        exclude: Some(len),
        ..options.clone()
    };
    let out = doc.snap(0, Point::new(451.0, 451.0), &dragging);
    assert_eq!(hit(&out).0, SnapKind::MarkupVertex);
    drop(s);

    // Rotated, user unit and offset pages: the manifest's own endpoints, bit for bit.
    for (fixture, page) in [
        (pl_testkit::rotated_pages(), 1u32),
        (pl_testkit::rotated_pages(), 3),
        (pl_testkit::user_unit(), 0),
        (pl_testkit::crop_box(), 1),
        (pl_testkit::crop_box(), 2),
    ] {
        let (_dir, path) = on_disk(&fixture);
        let mut s = session(&backend, None);
        let id = s.open(&path, None).unwrap();
        let doc = s.document_mut(id).unwrap();
        doc.load_annots().unwrap();
        let seg = fixture.manifest.pages[page as usize].segments[0];
        let end = Point::new(seg.b[0], seg.b[1]);
        let out = snap_ready(
            doc,
            page,
            end + pl_doc::pl_geom::Vec2::new(0.7, -0.4),
            &options,
        );
        assert_eq!(
            hit(&out),
            (SnapKind::Endpoint, end),
            "{} page {}",
            fixture.manifest.name,
            page + 1
        );
    }
}

#[test]
fn measurement_edits_recompute_and_survive_a_save() {
    let backend = backend();
    let (_dir, path, mut s, id) = plain_doc(&backend);
    let doc = s.document_mut(id).unwrap();
    let o = origin();
    let p = Point::new;
    let outline = vec![
        p(100.0, 100.0),
        p(244.0, 100.0),
        p(244.0, 172.0),
        p(100.0, 172.0),
    ];
    let slab = doc
        .add_measurement(
            0,
            MeasureShape::Area {
                outline,
                cutouts: Vec::new(),
            },
            &o,
        )
        .unwrap();
    let area = |doc: &Document| doc.annotation(&slab).unwrap().quantities().unwrap();
    assert!(close(area(doc).area.unwrap(), 800.0 * FT * FT));
    let hole = vec![
        p(110.0, 110.0),
        p(146.0, 110.0),
        p(146.0, 146.0),
        p(110.0, 146.0),
    ];
    doc.add_cutout(&slab, hole, when()).unwrap();
    assert!(close(area(doc).area.unwrap(), 700.0 * FT * FT));
    // A cutout outside the outline is refused.
    let outside = vec![p(300.0, 300.0), p(310.0, 300.0), p(310.0, 310.0)];
    assert!(matches!(
        doc.add_cutout(&slab, outside, when()).unwrap_err(),
        pl_doc::DocError::Edit(EditError::Unsupported { .. })
    ));
    doc.set_depth(&slab, 0.25, when()).unwrap();
    let a = doc.annotation(&slab).unwrap();
    assert_eq!(a.measurement().unwrap().kind, MeasureKind::Volume);
    assert!(close(area(doc).volume.unwrap(), 700.0 * FT * FT * 0.25));
    // The outline dragged to twice the width.
    let wide = Geometry::Points(vec![
        p(100.0, 100.0),
        p(388.0, 100.0),
        p(388.0, 172.0),
        p(100.0, 172.0),
    ]);
    doc.set_measure_geometry(&slab, wide, when()).unwrap();
    assert!(close(area(doc).volume.unwrap(), 1500.0 * FT * FT * 0.25));
    let text = doc.annotation(&slab).unwrap().meta.contents.clone();
    assert_eq!(text, doc.annotation(&slab).unwrap().measure_text());
    let count = doc
        .add_measurement(
            0,
            MeasureShape::Count {
                points: vec![p(500.0, 500.0)],
                symbol: CountSymbol::Square,
            },
            &o,
        )
        .unwrap();
    doc.add_count_marker(&count, p(510.0, 500.0), when())
        .unwrap();
    doc.add_count_marker(&count, p(520.0, 500.0), when())
        .unwrap();
    doc.remove_count_marker(&count, 0, when()).unwrap();
    assert!(doc
        .measurements()
        .unwrap()
        .iter()
        .any(|r| r.id == count && r.text == "2"));
    doc.save().unwrap();
    drop(s);

    let mut s = session(&backend, None);
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert!(close(area(doc).volume.unwrap(), 1500.0 * FT * FT * 0.25));
    assert!(close(area(doc).area.unwrap(), 1500.0 * FT * FT));
    assert_eq!(doc.annotation(&slab).unwrap().meta.contents, text);
    let c = doc.annotation(&count).unwrap();
    assert_eq!(c.count_points(), [p(510.0, 500.0), p(520.0, 500.0)]);
    assert_eq!(c.meta.contents.as_deref(), Some("2"));
    // The file holds the count as one annotation per marker. Edited again, the count is
    // drawn by the window, and the page render hides both annotations it was saved as.
    assert!(doc.hidden_annotations().is_empty());
    doc.add_count_marker(&count, p(530.0, 500.0), when())
        .unwrap();
    assert_eq!(doc.hidden_annotations().len(), 2);
}


fn settings() -> DocSettings {
    DocSettings {
        default_scale: Some(setting("1:100")),
        legends: vec![LegendDef {
            id: "L1".into(),
            title: "Doors".into(),
            page: 0,
            rect: [400.0, 600.0, 560.0, 700.0],
            subjects: vec!["Count".into()],
            columns: vec!["subject".into(), "count".into()],
        }],
        spaces: vec![SpaceDef {
            id: "kitchen".into(),
            name: "Kitchen".into(),
            page: 0,
            outline: vec![[72.0, 72.0], [300.0, 72.0], [300.0, 300.0], [72.0, 300.0]],
            parent: None,
        }],
        sequences: vec![SequenceDef {
            id: "doors".into(),
            name: "Doors".into(),
            prefix: "D-".into(),
            suffix: String::new(),
            next: 1,
            step: 1,
            digits: 3,
        }],
        list_units: Some(pl_doc::pl_takeoff::QuantityUnits::imperial()),
    }
}

#[test]
fn takeoff_definitions_and_the_default_scale_are_saved_in_the_catalog() {
    let backend = backend();
    let (_dir, path) = on_disk(&pl_testkit::user_unit());
    let mut s = session(&backend, None);
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let o = origin();
    // No page scale: a length is refused until the document has a default scale.
    let shape = MeasureShape::Length {
        a: Point::new(10.0, 10.0),
        b: Point::new(82.0, 10.0),
    };
    assert!(doc.add_measurement(0, shape.clone(), &o).is_err());
    doc.take_takeoff_refresh();
    doc.set_doc_settings(settings()).unwrap();
    let refresh = doc.take_takeoff_refresh();
    assert!(
        refresh.definitions,
        "the refresh hook sees the new definitions"
    );
    let len = doc.add_measurement(0, shape, &o).unwrap();
    // 72 units of a user unit 2 page are 2 in of paper: 200 in at 1:100.
    assert!(close(length_m(doc, &len), 2.0 * 0.0254 * 100.0));
    let refresh = doc.take_takeoff_refresh();
    assert_eq!(refresh.pages.iter().copied().collect::<Vec<_>>(), [0]);
    assert!(!refresh.definitions);
    assert_eq!(doc.take_sequence_label("doors").unwrap(), "D-001");
    assert_eq!(doc.take_sequence_label("doors").unwrap(), "D-002");
    assert_eq!(
        doc.doc_settings()
            .unwrap()
            .space_at(0, Point::new(100.0, 100.0))
            .map(|s| s.id.as_str()),
        Some("kitchen")
    );
    // A page scale outranks the default; the measurement follows in the same step.
    doc.set_page_scale(0, Some(setting("1:50"))).unwrap();
    assert!(close(length_m(doc, &len), 2.0 * 0.0254 * 50.0));
    doc.undo().unwrap();
    // A change of the default scale puts the measurements it applies to on it.
    doc.set_default_scale(Some(setting("1:200"))).unwrap();
    assert!(close(length_m(doc, &len), 2.0 * 0.0254 * 200.0));
    let want = doc.doc_settings().unwrap().clone();
    assert!(doc.save().unwrap().wrote());
    drop(s);

    let mut s = session(&backend, None);
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert_eq!(doc.doc_settings().unwrap(), &want);
    assert_eq!(want.sequences[0].next, 3);
    assert!(close(length_m(doc, &len), 2.0 * 0.0254 * 200.0));
    // The catalog keeps its own keys beside the private one.
    let pdf = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
    let catalog = pdf.catalog().unwrap();
    assert!(catalog.get(b"Pages").is_ok());
    let private = catalog.get(b"PlumblineData").unwrap().as_dict().unwrap();
    assert!(matches!(
        private.get(b"PlDocSettings"),
        Ok(Object::String(..))
    ));
    // Bad definitions are refused whole.
    let mut bad = want.clone();
    bad.legends[0].page = 9;
    assert!(doc.set_doc_settings(bad).is_err());
    assert_eq!(doc.doc_settings().unwrap(), &want);
}

#[test]
fn journal_replay_restores_scales_viewports_measurements_and_definitions() {
    let backend = backend();
    let (_dir, path) = on_disk(&pl_testkit::rotated_pages());
    let journal = tempfile::tempdir().unwrap();
    type Seen = (
        Vec<pl_doc::pl_annot::Annotation>,
        Vec<PageScalesData>,
        DocSettings,
        usize,
        usize,
    );
    let see = |doc: &mut Document| -> Seen {
        let ed = doc.annots().unwrap();
        (
            ed.model(),
            ed.scales().data(),
            ed.scales().settings().clone(),
            ed.undo_len(),
            ed.redo_len(),
        )
    };
    let mut s = session(&backend, Some(journal.path()));
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let file_state = see(doc);
    let o = origin();
    let p = Point::new;
    doc.apply_scale_to_pages(0, 3, Some(setting("1/4\" = 1'-0\"")))
        .unwrap();
    doc.add_viewport(
        1,
        "Detail",
        [100.0, 100.0, 300.0, 300.0],
        Some(setting("1:20")),
    )
    .unwrap();
    let a = doc
        .add_measurement(
            1,
            MeasureShape::Length {
                a: p(120.0, 150.0),
                b: p(192.0, 150.0),
            },
            &o,
        )
        .unwrap();
    let area = doc
        .add_measurement(
            2,
            MeasureShape::Area {
                outline: vec![p(100.0, 100.0), p(172.0, 100.0), p(172.0, 172.0)],
                cutouts: Vec::new(),
            },
            &o,
        )
        .unwrap();
    doc.add_cutout(
        &area,
        vec![p(150.0, 110.0), p(160.0, 110.0), p(160.0, 120.0)],
        when(),
    )
    .unwrap();
    doc.edit_viewport(
        1,
        "Detail",
        [90.0, 90.0, 310.0, 310.0],
        Some(setting("1:10")),
    )
    .unwrap();
    doc.set_doc_settings(settings()).unwrap();
    doc.set_page_scale(3, None).unwrap();
    doc.undo().unwrap();
    let before = see(doc);
    assert!(close(length_m(doc, &a), 72.0 / 72.0 * 0.0254 * 10.0));
    // The kill.
    drop(s);

    let mut s = session(&backend, Some(journal.path()));
    let id = s.open(&path, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    doc.load_annots().unwrap();
    assert!(doc.recovered_changes() >= 8, "{}", doc.recovered_changes());
    assert_eq!(see(doc), before, "the state before the kill");
    while doc.undo().unwrap() {}
    let back = see(doc);
    assert_eq!(
        (back.0, back.1, back.2),
        (file_state.0, file_state.1, file_state.2)
    );
    assert!(!doc.editor().unwrap().has_unsaved());
    while doc.redo().unwrap() {}
    assert_eq!(see(doc).0, before.0);
}

#[test]
fn scale_commands_survive_the_journal_form() {
    let commands = vec![
        Command::SetPageScale {
            page: 2,
            scale: Some(setting("1/8\" = 1'-0\"")),
        },
        Command::ApplyScaleToPages {
            first: 0,
            last: 3,
            scale: None,
        },
        Command::AddViewport {
            page: 0,
            name: "A".into(),
            bbox: [0.5, 1.0 / 3.0, 100.0, 200.0],
            scale: Some(setting("1:250")),
        },
        Command::EditViewport {
            page: 0,
            name: "A".into(),
            bbox: [1.0, 2.0, 3.0, 4.0],
            scale: None,
        },
        Command::RemoveViewport {
            page: 0,
            name: "A".into(),
        },
        Command::SetDocSettings(Box::new(settings())),
        Command::RestoreDocSettings(Box::default()),
    ];
    for c in commands {
        let json = serde_json::to_string(&c).unwrap();
        let back: Command = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c, "{json}");
    }
}

/// The real sample sheet set, when it is there: a copy under `target` is calibrated from its
/// own page size, measured, saved and read back. Nothing of the file is printed.
#[test]
fn the_real_sample_is_measured_saved_and_read_back_when_present() {
    let sample =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/site-development-plans.pdf");
    if !sample.exists() {
        println!("SKIPPED: the real sample is not present");
        return;
    }
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pl-doc-measure-sample");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("sample.pdf");
    std::fs::copy(&sample, &copy).unwrap();
    let backend = backend();
    let mut s = session(&backend, None);
    let id = s.open(&copy, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    let pages = doc.page_count();
    assert!(pages >= 2);
    let o = origin();
    // Each sheet's own width is called 1000 ft: then any 72-unit length is 1000/width*72 ft.
    for page in [0, pages - 1] {
        let media = doc.page_geometry(page).unwrap().media_box();
        let (a, b) = (
            Point::new(media.x0, media.y0),
            Point::new(media.x1, media.y0),
        );
        doc.calibrate(page, a, b, 1000.0 * FT, Unit::Foot, None)
            .unwrap();
        let start = Point::new(media.x0 + 100.0, media.y0 + 100.0);
        let id = doc
            .add_measurement(
                page,
                MeasureShape::Length {
                    a: start,
                    b: Point::new(start.x + 72.0, start.y),
                },
                &o,
            )
            .unwrap();
        let want = 72.0 / media.width() * 1000.0 * FT;
        assert!(close(length_m(doc, &id), want));
    }
    let before = rows(doc);
    doc.save().unwrap();
    drop(s);
    let mut s = session(&backend, None);
    let id = s.open(&copy, None).unwrap();
    let doc = s.document_mut(id).unwrap();
    assert_eq!(rows(doc), before);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}
