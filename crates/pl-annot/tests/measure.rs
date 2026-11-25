//! Measurements in files: every kind on a plain page and on rotated, `/UserUnit 2` and
//! offset-box pages.
//!
//! * Lengths, areas (with and without cutouts), volumes, angles, radii, perimeters and counts
//!   have their analytic values, worked out from the geometry, the scale and the user unit.
//! * They are written as standard dimension annotations: `/IT`, `/Measure` with golden `/R` and
//!   `/C`, the dimension keys, the value in `/Contents`; load-save-load gives an equal model and
//!   nothing else in the file is rewritten.
//! * The caption PDFium extracts from the saved appearance is the computed value.
//! * A viewport's scale applies inside its `/BBox` only; `/VP` is added, edited and removed as
//!   a patch of the page that keeps every other page key.
//! * Another application's dimensions with the standard keys are read as measurements and left
//!   untouched by a save that does not concern them; ones that cannot be drawn again stay
//!   unmodelled.

mod common;

use common::{Target, Work, TARGETS};
use pl_annot::{
    AnnotKind, AnnotSet, Annotation, CaptionPosition, CountSymbol, Geometry, LineEnding,
    MeasureFormat, MeasureKind, MeasureSpec, PageViewports, Quantities, Rgb, ScaleRef, Viewport,
    ViewportError, PAGE_SCALE_NAME, PRIVATE_KEY,
};
use pl_geom::{AreaFormat, AreaUnit, LengthFormat, Point, Rect, Scale, Unit, Vec2};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, SaveOptions, StringFormat};

const FT: f64 = 0.3048;

fn text_obj(s: &str) -> Object {
    Object::String(s.as_bytes().to_vec(), StringFormat::Literal)
}

fn string(dict: &Dictionary, key: &str) -> String {
    match dict.get(key.as_bytes()) {
        Ok(Object::String(bytes, _)) => pl_annot::decode_text(bytes),
        other => panic!("/{key} is not a string: {other:?}"),
    }
}

fn real(o: &Object) -> f64 {
    pl_pdf::number(o).unwrap()
}

fn dict_of(doc: &PdfDocument, id: ObjectId) -> &Dictionary {
    doc.object(id).unwrap().as_dict().unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

/// 1" = 20' in decimal feet, areas in square feet, on a page with this user unit.
fn eng(user_unit: f64) -> MeasureSpec {
    MeasureSpec::new(
        &Scale::engineering(20.0).unwrap(),
        user_unit,
        &MeasureFormat::new(
            LengthFormat::decimal(Unit::Foot, 2),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        ),
    )
}

/// 1/4" = 1'-0" in feet and inches to a sixteenth.
fn arch(user_unit: f64) -> MeasureSpec {
    MeasureSpec::new(
        &Scale::architectural(0.25).unwrap(),
        user_unit,
        &MeasureFormat::new(
            LengthFormat::architectural(16),
            AreaFormat::new(AreaUnit::SqFoot, 2),
        ),
    )
}

fn pt(o: Vec2, x: f64, y: f64) -> Point {
    Point::new(o.x + x, o.y + y)
}

fn square(o: Vec2, x: f64, y: f64, side: f64) -> Vec<Point> {
    vec![
        pt(o, x, y),
        pt(o, x + side, y),
        pt(o, x + side, y + side),
        pt(o, x, y + side),
    ]
}

/// One measurement and what it must amount to on a page with user unit `u` at 1" = 20'.
struct Case {
    name: &'static str,
    annotation: Annotation,
    /// The value in SI: metres, square metres, cubic metres, radians or a count.
    value: f64,
    text: String,
}

/// One of every kind, placed from the visible box's corner `o`. On a page whose unit is `u`
/// points, 72 units are `u` inches of paper: `20 u` feet.
fn cases(page: usize, o: Vec2, u: f64, rotate: i32) -> Vec<Case> {
    let og = common::origin();
    let s = eng(u);
    let ft = |units: f64| units / 72.0 * 20.0 * u;
    let mut out = Vec::new();

    // 144 units across, with leader lines.
    let mut length =
        Annotation::measure_length(page, pt(o, 20.0, 20.0), pt(o, 164.0, 20.0), s.clone(), &og);
    {
        let m = length.measure.as_mut().unwrap();
        m.leader = 12.0;
        m.leader_extension = 4.0;
        m.leader_offset = 2.0;
    }
    out.push(Case {
        name: "length",
        value: ft(144.0) * FT,
        text: format!("{:.2}'", ft(144.0)),
        annotation: length,
    });
    // A 3-4-5 slant of 90 units, its caption in the line.
    let mut slant =
        Annotation::measure_length(page, pt(o, 20.0, 60.0), pt(o, 74.0, 132.0), s.clone(), &og);
    slant.measure.as_mut().unwrap().caption_position = CaptionPosition::Inline;
    slant.style.line_start = LineEnding::Slash;
    slant.style.line_end = LineEnding::Slash;
    out.push(Case {
        name: "slant",
        value: ft(90.0) * FT,
        text: format!("{:.2}'", ft(90.0)),
        annotation: slant,
    });
    out.push(Case {
        name: "polylength",
        value: ft(216.0) * FT,
        text: format!("{:.2}'", ft(216.0)),
        annotation: Annotation::measure_polylength(
            page,
            vec![pt(o, 20.0, 160.0), pt(o, 20.0, 232.0), pt(o, 164.0, 232.0)],
            s.clone(),
            &og,
        ),
    });
    out.push(Case {
        name: "perimeter",
        value: ft(288.0) * FT,
        text: format!("{:.2}'", ft(288.0)),
        annotation: Annotation::measure_perimeter(
            page,
            square(o, 180.0, 20.0, 72.0),
            s.clone(),
            &og,
        ),
    });
    // 72 x 72 less an 18 x 18 cutout: 4860 square units, (20 u / 72)^2 sq ft each.
    let sq_ft = 4860.0 * (20.0 * u / 72.0).powi(2);
    let mut area = Annotation::measure_area(
        page,
        square(o, 180.0, 110.0, 72.0),
        vec![square(o, 200.0, 130.0, 18.0)],
        s.clone(),
        &og,
    );
    area.style.fill = Some(Rgb::new(0.6, 0.8, 1.0));
    out.push(Case {
        name: "area",
        value: sq_ft * FT * FT,
        text: format!("{sq_ft:.2} sq ft"),
        annotation: area,
    });
    // The same, six inches deep, in cubic yards.
    out.push(Case {
        name: "volume",
        value: sq_ft * 0.5 * FT * FT * FT,
        text: format!("{:.2} cu yd", sq_ft * 0.5 / 27.0),
        annotation: Annotation::measure_volume(
            page,
            square(o, 180.0, 200.0, 72.0),
            vec![square(o, 200.0, 220.0, 18.0)],
            0.5 * FT,
            s.clone(),
            &og,
        ),
    });
    // The angle of a 3-4-5 triangle at its long leg.
    out.push(Case {
        name: "angle",
        value: (3.0f64 / 4.0).atan(),
        text: "36.9\u{b0}".into(),
        annotation: Annotation::measure_angle(
            page,
            pt(o, 60.0, 260.0),
            pt(o, 20.0, 260.0),
            pt(o, 52.0, 284.0),
            Some(s.clone()),
            &og,
        ),
    });
    out.push(Case {
        name: "radius",
        value: ft(36.0) * FT,
        text: format!("R {:.2}'", ft(36.0)),
        annotation: Annotation::measure_radius(
            page,
            pt(o, 110.0, 300.0),
            pt(o, 146.0, 300.0),
            s.clone(),
            &og,
        ),
    });
    out.push(Case {
        name: "diameter",
        value: ft(36.0) * FT,
        text: format!("\u{d8} {:.2}'", ft(36.0)),
        annotation: Annotation::measure_diameter(
            page,
            pt(o, 100.0, 100.0),
            pt(o, 136.0, 100.0),
            s,
            &og,
        ),
    });
    let markers = [
        (200.0, 300.0),
        (220.0, 300.0),
        (240.0, 300.0),
        (200.0, 320.0),
        (220.0, 320.0),
    ];
    let points: Vec<Point> = markers.iter().map(|(x, y)| pt(o, *x, *y)).collect();
    let mut count = Annotation::count(page, &points, CountSymbol::Diamond, &og);
    count.measure.as_mut().unwrap().caption = true;
    count.sync_measurement();
    out.push(Case {
        name: "count",
        value: 5.0,
        text: "5".into(),
        annotation: count,
    });
    for c in &mut out {
        c.annotation.set_rotation(rotate);
    }
    out
}

/// The SI value a case is about, from the annotation's quantities.
fn value_of(a: &Annotation) -> f64 {
    let q = a.quantities().unwrap();
    let m = a.measurement().unwrap();
    match m.kind {
        MeasureKind::Area => q.area.unwrap(),
        MeasureKind::Volume => q.volume.unwrap(),
        MeasureKind::Angle => q.angle.unwrap(),
        MeasureKind::Count => q.count.unwrap() as f64,
        _ => q.length.unwrap(),
    }
}

fn target_setup(target: &Target) -> (Work, f64, i32, Vec2) {
    let fixture = (target.fixture)();
    let mut work = Work::new(&fixture);
    let geometry = work.geometry(target.page);
    let o = work.sample_origin(target.page);
    (work, geometry.user_unit(), geometry.rotate().degrees(), o)
}

#[test]
fn every_kind_has_its_analytic_value_and_round_trips_on_every_page() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for target in TARGETS {
        let label = target.name;
        let (mut work, u, rotate, o) = target_setup(&target);
        let geometry = work.geometry(target.page);
        let cases = cases(target.page, o, u, rotate);
        for c in &cases {
            let a = &c.annotation;
            assert!(a.is_modelled(), "{label} {}", c.name);
            assert!(
                close(value_of(a), c.value),
                "{label} {}: {} vs {}",
                c.name,
                value_of(a),
                c.value
            );
            assert_eq!(a.measure_text().unwrap(), c.text, "{label} {}", c.name);
            // /Contents is the bare value, as Revu writes it (no R or diameter sign).
            assert_eq!(
                a.meta.contents,
                a.measure_value_text(),
                "{label} {}",
                c.name
            );
            assert!(c.text.ends_with(a.meta.contents.as_deref().unwrap()));
            // The same from nothing but the geometry, the scale and the user unit.
            let m = a.measurement().unwrap();
            let q = Quantities::compute(
                m.kind,
                &a.geometry,
                &m.cutouts,
                m.depth,
                &Scale::engineering(20.0).unwrap(),
                u,
            );
            let q_value = match m.kind {
                MeasureKind::Area => q.area,
                MeasureKind::Volume => q.volume,
                MeasureKind::Angle => q.angle,
                MeasureKind::Count => q.count.map(|n| n as f64),
                _ => q.length,
            };
            assert!(close(q_value.unwrap(), c.value), "{label} {}", c.name);
        }

        let before = work.doc().clone();
        let page_id = before.page_id(target.page).unwrap();
        let mut set = AnnotSet::load(&before);
        for c in &cases {
            set.add(c.annotation.clone()).unwrap();
        }
        let (plan, _) = work.save(&set);
        assert_eq!(plan.added.len(), cases.len());
        let after = work.doc().clone();
        common::assert_untouched(&before, &after, &[page_id]);

        // The standard keys other viewers read, and the form Revu writes each kind in.
        for (c, change) in cases.iter().zip(&plan.added) {
            let d = dict_of(&after, change.object);
            let kind = c.annotation.measurement().unwrap().kind;
            // The bare value, as Revu writes it: no R or diameter sign before it.
            assert_eq!(
                Some(string(d, "Contents")),
                c.annotation.measure_value_text(),
                "{label} {}",
                c.name
            );
            if matches!(kind, MeasureKind::Radius | MeasureKind::Diameter) {
                assert!(c.text.ends_with(&string(d, "Contents")));
                assert_ne!(string(d, "Contents"), c.text);
                // Revu's Polygon radius and Circle diameter carry no fill, no line endings
                // and no caption position; ours are in the private part.
                for key in [&b"IC"[..], b"LE", b"CP"] {
                    assert!(!d.has(key), "{label} {}", c.name);
                }
            }
            assert!(d.has(b"AP") && d.has(PRIVATE_KEY.as_bytes()));
            assert_eq!(
                d.get(b"Subtype").unwrap(),
                &Object::Name(kind.file_subtype().pdf_name().as_bytes().to_vec()),
                "{label} {}",
                c.name
            );
            assert_eq!(
                d.get(b"IT").unwrap(),
                &Object::Name(kind.intent().unwrap().as_bytes().to_vec()),
                "{label} {}",
                c.name
            );
            assert_eq!(
                d.get(b"MeasurementTypes").unwrap(),
                &Object::Integer(kind.measurement_types())
            );
            // Every measurement with a scale carries it (a count has none here).
            if kind != MeasureKind::Count {
                let measure = d.get(b"Measure").unwrap().as_dict().unwrap();
                assert_eq!(
                    measure.get(b"Subtype").unwrap(),
                    &Object::Name(b"RL".to_vec())
                );
                assert_eq!(string(measure, "R"), "1 in = 20 ft", "{label}");
                let x = measure.get(b"X").unwrap().as_array().unwrap()[0]
                    .as_dict()
                    .unwrap();
                // Golden: 0.2777778 on a plain page, 0.5555556 on a /UserUnit 2 one.
                let golden = if u == 2.0 { 0.5555556 } else { 0.2777778 };
                assert_eq!(real(x.get(b"C").unwrap()), golden, "{label}");
                assert_eq!(x.get(b"C").unwrap(), &Object::Real(golden as f32));
                assert_eq!(string(x, "U"), "'");
                // An angle and a volume carry the formats Revu shows them with.
                assert!(measure.has(b"T"), "{label} {}", c.name);
                if kind == MeasureKind::Volume {
                    let v = measure.get(b"V").unwrap().as_array().unwrap()[0]
                        .as_dict()
                        .unwrap();
                    assert_eq!(string(v, "U"), "cu yd");
                    // Cubic feet to cubic yards.
                    assert!((real(v.get(b"C").unwrap()) - 1.0 / 27.0).abs() < 1e-6);
                }
            } else {
                assert!(!d.has(b"Measure") && !d.has(b"BBMeasure"), "no scale given");
            }
            if kind == MeasureKind::Length && c.name == "length" {
                assert_eq!(real(d.get(b"LL").unwrap()), 12.0);
                assert_eq!(real(d.get(b"LLE").unwrap()), 4.0);
                assert_eq!(real(d.get(b"LLO").unwrap()), 2.0);
                assert_eq!(d.get(b"Cap").unwrap(), &Object::Boolean(true));
                assert_eq!(d.get(b"CP").unwrap(), &Object::Name(b"Top".to_vec()));
            }
            if c.name == "slant" {
                assert_eq!(d.get(b"CP").unwrap(), &Object::Name(b"Inline".to_vec()));
                assert!(!d.has(b"LL"));
            }
            let numbers = |d: &Dictionary, key: &[u8]| -> Vec<f64> {
                d.get(key)
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(real)
                    .collect()
            };
            match kind {
                MeasureKind::Count => {
                    // One small polygon per marker: this one and four grouped under it.
                    assert_eq!(change.members.len(), 4, "{label}");
                    assert_eq!(d.get(b"NumCounts").unwrap(), &Object::Integer(5));
                    assert!(!d.has(b"InkList"));
                    for member in &change.members {
                        let md = dict_of(&after, *member);
                        assert_eq!(
                            md.get(b"Subtype").unwrap(),
                            &Object::Name(b"Polygon".to_vec())
                        );
                        assert_eq!(md.get(b"IRT").unwrap(), &Object::Reference(change.object));
                        assert_eq!(md.get(b"RT").unwrap(), &Object::Name(b"Group".to_vec()));
                        assert_eq!(md.get(b"NumCounts").unwrap(), &Object::Integer(5));
                        assert_eq!(string(md, "Contents"), "5");
                        assert!(md.has(b"AP"));
                        // The diamond: four corners.
                        assert_eq!(numbers(md, b"Vertices").len(), 8);
                    }
                }
                MeasureKind::Perimeter => {
                    // A closed polyline dimension, as other applications write a perimeter.
                    let v = numbers(d, b"Vertices");
                    let Geometry::Points(model) = &c.annotation.geometry else {
                        panic!("a perimeter has points");
                    };
                    assert_eq!(v.len(), 2 * (model.len() + 1), "{label}");
                    assert_eq!(v[..2], v[v.len() - 2..], "{label}: closed");
                }
                MeasureKind::Radius => {
                    // Three vertices: the edge, the centre, the edge again.
                    let v = numbers(d, b"Vertices");
                    assert_eq!(v.len(), 6);
                    assert_eq!(v[..2], v[4..]);
                    assert!(!d.has(b"L"));
                }
                MeasureKind::Diameter => {
                    // A Circle whose /Rect less /RD is the measured circle.
                    let (rd, r) = (numbers(d, b"RD"), numbers(d, b"Rect"));
                    let width = (r[2] - rd[2]) - (r[0] + rd[0]);
                    let height = (r[3] - rd[3]) - (r[1] + rd[1]);
                    assert!(
                        (width - 36.0).abs() < 0.01 && (height - 36.0).abs() < 0.01,
                        "{label}: {width} {height}"
                    );
                    assert!(!d.has(b"L"));
                }
                _ => {}
            }
        }

        // Load-save-load: an equal model that is clean, and a second save writes nothing.
        let loaded = AnnotSet::load(&after);
        assert!(!loaded.is_dirty(), "{label}");
        for c in &cases {
            let got = loaded.get(&c.annotation.id).unwrap();
            assert_eq!(
                got.detached(),
                c.annotation.detached(),
                "{label} {}",
                c.name
            );
            assert!(close(value_of(got), c.value));
        }
        assert!(loaded.plan_save(&after).unwrap().is_empty(), "{label}");

        // PDFium draws what the tiny-skia raster of the draw lists draws.
        let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
        let mut list = pl_annot::DrawList::new();
        for c in &cases {
            list.extend(c.annotation.draw_list().ops);
        }
        let ours = common::raster_over(&list, &geometry, &r.backdrop);
        if let Some(why) = common::ink_mismatch(
            &r.pdfium,
            &ours,
            &r.backdrop,
            common::scale_for(&geometry),
            2.0,
        ) {
            common::dump(&r.pdfium, &format!("measure-{label}-pdfium"));
            common::dump(&ours, &format!("measure-{label}-ours"));
            failures.push(format!("{label}: {why}"));
        }
        assert_eq!(
            common::pdfium_annotation_counts(
                &backend,
                pl_render::DocSource::Bytes(work.bytes()),
                &[target.page]
            ),
            // The count is five annotations, one per marker.
            [before.annotations(target.page).unwrap().len() + cases.len() + 4]
        );
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The region an area with cutouts paints, in PDFium's render of the saved appearance and in
/// the raster of its draw list, is the area worked out from its geometry: on every page
/// layout (rotated, `/UserUnit 2`, offset boxes).
#[test]
fn the_painted_cutout_area_is_the_computed_area() {
    let backend = common::backend();
    // An orange no fixture uses.
    let orange = pl_annot::Rgb::new(1.0, 0.5, 0.0);
    let is_orange = |p: &[u8; 4]| p[0] > 220 && (100..156).contains(&p[1]) && p[2] < 40;
    let count_orange = |b: &pl_render::Bitmap| {
        b.pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| is_orange(p))
            .count()
    };
    for target in TARGETS {
        let label = target.name;
        let (mut work, u, rotate, o) = target_setup(&target);
        let geometry = work.geometry(target.page);
        let at = |x: f64, y: f64| pt(o, x, y);
        let outer = vec![
            at(4.0, 4.0),
            at(154.0, 14.0),
            at(129.0, 119.0),
            at(59.0, 149.0),
            at(0.0, 89.0),
        ];
        let holes = vec![
            vec![at(29.0, 29.0), at(69.0, 34.0), at(54.0, 74.0)],
            vec![
                at(89.0, 59.0),
                at(119.0, 59.0),
                at(119.0, 99.0),
                at(89.0, 99.0),
            ],
        ];
        let mut area = Annotation::measure_area(
            target.page,
            outer.clone(),
            holes.clone(),
            eng(u),
            &common::origin(),
        );
        area.style.fill = Some(orange);
        area.style.stroke = None;
        area.style.width = 0.0;
        area.style.opacity = 1.0;
        if let Some(m) = area.measure.as_mut() {
            m.caption = false;
        }
        area.set_rotation(rotate);
        area.sync_measurement();
        let units = pl_geom::polygon_area_with_holes(&outer, &holes);
        // Square metres per square page unit: 20 ft an inch of paper, times the user unit.
        let k = 20.0 / 72.0 * FT * u;
        let computed = area.quantities().unwrap().area.unwrap();
        assert!(close(computed, units * k * k), "{label}");

        let before = work.doc().clone();
        let mut set = AnnotSet::load(&before);
        set.add(area.clone()).unwrap();
        work.save(&set);
        let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
        let ours = common::raster_over(&area.draw_list(), &geometry, &r.backdrop);
        let px = common::scale_for(&geometry);
        assert_eq!(
            count_orange(&r.backdrop),
            0,
            "{label}: the page has no orange"
        );
        for (what, image) in [("pdfium", &r.pdfium), ("draw list", &ours)] {
            let painted = count_orange(image) as f64 / (px * px);
            assert!(
                (painted - units).abs() / units < 0.02,
                "{label} {what}: painted {painted} square units, computed {units}"
            );
        }
    }
}

#[test]
fn the_caption_pdfium_extracts_is_the_computed_value() {
    let backend = common::backend();
    for target in TARGETS {
        let label = target.name;
        let (_, u, rotate, o) = target_setup(&target);
        for c in cases(target.page, o, u, rotate) {
            if ![
                "length", "slant", "area", "volume", "angle", "radius", "diameter", "count",
            ]
            .contains(&c.name)
            {
                continue;
            }
            let (mut work, ..) = target_setup(&target);
            let geometry = work.geometry(target.page);
            let mut set = AnnotSet::load(work.doc());
            set.add(c.annotation.clone()).unwrap();
            let (plan, _) = work.save(&set);
            // A count's caption is drawn by the annotation of its last marker.
            let appearance = match plan.added[0].members.last() {
                Some(last) => {
                    let ap = dict_of(work.doc(), *last)
                        .get(b"AP")
                        .unwrap()
                        .as_dict()
                        .unwrap();
                    match ap.get(b"N").unwrap() {
                        Object::Reference(id) => *id,
                        other => panic!("{other:?}"),
                    }
                }
                None => plan.added[0].appearance.expect("an appearance"),
            };

            common::flatten_onto_page(&mut work, target.page, appearance);
            let text = common::page_text(&backend, work.bytes(), target.page, &geometry);
            assert_eq!(text.text().trim(), c.text, "{label} {}", c.name);
            // The caption is where the model says: PDFium's first glyph origin is the
            // caption's baseline start, within half a unit.
            let caption = c.annotation.caption().unwrap();
            let first = text.chars.iter().find(|ch| !ch.ch.is_whitespace()).unwrap();
            let got = Point::new(first.origin[0], first.origin[1]);
            assert!(
                (got - caption.origin).hypot() < 0.5,
                "{label} {}: PDFium {got:?}, model {:?}",
                c.name,
                caption.origin
            );
        }
    }
}

#[test]
fn captions_read_upright_on_the_displayed_page() {
    for target in TARGETS {
        let (mut work, u, rotate, o) = target_setup(&target);
        let geometry = work.geometry(target.page);
        let to_display = geometry.page_to_display();
        for c in cases(target.page, o, u, rotate) {
            let caption = c.annotation.caption().unwrap();
            // Display space is y-down: text reads left to right, or bottom to top when it is
            // vertical there.
            let d = to_display * caption.direction.to_point() - to_display * Point::ZERO;
            assert!(
                d.x > 1e-9 || (d.x.abs() <= 1e-9 && d.y < 0.0),
                "{} {}: {d:?}",
                target.name,
                c.name
            );
        }
    }
}

#[test]
fn a_viewport_scale_applies_inside_its_box_only() {
    for target in [TARGETS[0], TARGETS[1], TARGETS[4], TARGETS[5]] {
        let label = target.name;
        let (mut work, u, _, o) = target_setup(&target);
        let doc = work.doc().clone();
        let page = target.page;
        let media = doc.page(page).unwrap().media_box;
        let mut vps = PageViewports::load(&doc, page).unwrap();
        assert!(vps.page_scale.is_none() && vps.viewports.is_empty() && !vps.is_dirty());
        vps.page_scale = Some(eng(u));
        let detail = Rect::new(o.x + 100.0, o.y + 100.0, o.x + 250.0, o.y + 250.0);
        vps.add(Viewport::new("Detail A", detail, Some(arch(u))))
            .unwrap();
        assert_eq!(
            vps.add(Viewport::new("Detail A", detail, None)),
            Err(ViewportError::DuplicateName("Detail A".into()))
        );
        assert_eq!(
            vps.add(Viewport::new(PAGE_SCALE_NAME, detail, None)),
            Err(ViewportError::DuplicateName(PAGE_SCALE_NAME.into()))
        );
        assert_eq!(
            vps.add(Viewport::new(" ", detail, None)),
            Err(ViewportError::EmptyName)
        );
        assert!(vps.is_dirty());

        // 72 units: 20 u feet on the sheet, 4 u feet (1/4" = 1') in the detail.
        let og = common::origin();
        let mut inside =
            Annotation::measure_length(page, pt(o, 120.0, 150.0), pt(o, 192.0, 150.0), eng(u), &og);
        let mut outside =
            Annotation::measure_length(page, pt(o, 10.0, 20.0), pt(o, 82.0, 20.0), arch(u), &og);
        // Half in and half out of the detail: the sheet's scale.
        let mut across =
            Annotation::measure_length(page, pt(o, 60.0, 150.0), pt(o, 132.0, 150.0), arch(u), &og);
        for a in [&mut inside, &mut outside, &mut across] {
            assert!(vps.apply_to(a));
        }
        let inches = |ft: f64| format!("{}'-0\"", ft);
        assert_eq!(
            inside.measure.as_ref().unwrap().scale,
            ScaleRef::Viewport("Detail A".into())
        );
        assert_eq!(inside.measure_text().unwrap(), inches(4.0 * u), "{label}");
        assert!(close(
            inside.quantities().unwrap().length.unwrap(),
            4.0 * u * FT
        ));
        for a in [&outside, &across] {
            assert_eq!(a.measure.as_ref().unwrap().scale, ScaleRef::Page);
            assert_eq!(
                a.measure_text().unwrap(),
                format!("{:.2}'", 20.0 * u),
                "{label}"
            );
        }

        // Saved as a patch of the page that names /VP only.
        let before = doc.clone();
        let page_id = before.page_id(page).unwrap();
        let mut set = AnnotSet::load(&before);
        for a in [&inside, &outside, &across] {
            set.add(a.clone()).unwrap();
        }
        let mut plan = set.plan_save(&before).unwrap();
        assert!(vps.plan(&before, &mut plan.edits).unwrap());
        work.file
            .save_incremental(&plan.edits, &SaveOptions::default())
            .unwrap();
        let after = work.doc().clone();
        common::assert_untouched(&before, &after, &[page_id]);
        let (old_page, new_page) = (dict_of(&before, page_id), dict_of(&after, page_id));
        for (key, value) in old_page.iter() {
            if key.as_slice() != b"Annots" {
                assert_eq!(
                    new_page.get(key).unwrap(),
                    value,
                    "{label}: /{}",
                    String::from_utf8_lossy(key)
                );
            }
        }
        let vp = new_page.get(b"VP").unwrap().as_array().unwrap();
        assert_eq!(vp.len(), 2);
        let first = vp[0].as_dict().unwrap();
        assert_eq!(
            first.get(b"Type").unwrap(),
            &Object::Name(b"Viewport".to_vec())
        );
        assert_eq!(string(first, "Name"), PAGE_SCALE_NAME);
        assert_eq!(
            pl_pdf::rect_from_object(first.get(b"BBox").unwrap()),
            Some(media),
            "the page scale covers the whole page"
        );
        let second = vp[1].as_dict().unwrap();
        assert_eq!(string(second, "Name"), "Detail A");
        assert_eq!(
            pl_pdf::rect_from_object(second.get(b"BBox").unwrap()),
            Some(detail)
        );
        let m = second.get(b"Measure").unwrap().as_dict().unwrap();
        assert_eq!(string(m, "R"), "0.25 in = 1 ft");
        let x = m.get(b"X").unwrap().as_array().unwrap();
        // Golden: 4/72 ft per unit (8/72 on a /UserUnit 2 page), then 12 inches to the foot.
        let golden = if u == 2.0 { 0.11111111 } else { 0.055555556 };
        assert_eq!(
            real(x[0].as_dict().unwrap().get(b"C").unwrap()),
            golden,
            "{label}"
        );
        assert_eq!(
            x[1].as_dict().unwrap().get(b"C").unwrap(),
            &Object::Integer(12)
        );

        // Read back equal, clean; the measurements too.
        let again = PageViewports::load(&after, page).unwrap();
        assert!(!again.is_dirty(), "{label}");
        assert_eq!(again.page_scale, vps.page_scale);
        assert_eq!(again.viewports.len(), 1);
        assert_eq!(again.get("Detail A").unwrap().measure, Some(arch(u)));
        let loaded = AnnotSet::load(&after);
        assert_eq!(
            loaded.get(&inside.id).unwrap().detached(),
            inside.detached()
        );
        let mut edits = after.edit();
        assert!(!again.plan(&after, &mut edits).unwrap() && edits.is_empty());

        // Edit the detail to 1/2" = 1': the measurement in it follows; then remove it.
        let mut edited = again.clone();
        let half = MeasureSpec::new(
            &Scale::architectural(0.5).unwrap(),
            u,
            &MeasureFormat::new(
                LengthFormat::architectural(16),
                AreaFormat::new(AreaUnit::SqFoot, 2),
            ),
        );
        assert!(edited.edit("Detail A", detail, Some(half.clone())).unwrap());
        assert!(!edited.edit("Nope", detail, None).unwrap());
        let mut set = AnnotSet::load(&after);
        let a = set.get_mut(&inside.id).unwrap();
        assert!(edited.refresh(a));
        assert_eq!(a.measure_text().unwrap(), inches(2.0 * u));
        let mut plan = set.plan_save(&after).unwrap();
        assert_eq!(plan.modified.len(), 1);
        assert!(plan.modified[0].keys.contains(&"Measure".to_string()));
        assert!(edited.plan(&after, &mut plan.edits).unwrap());
        work.file
            .save_incremental(&plan.edits, &SaveOptions::default())
            .unwrap();
        let third = work.doc().clone();
        let reread = PageViewports::load(&third, page).unwrap();
        assert_eq!(reread.get("Detail A").unwrap().measure, Some(half));
        assert_eq!(
            string(
                AnnotSet::load(&third)
                    .get(&inside.id)
                    .unwrap()
                    .original_dict()
                    .unwrap(),
                "Contents"
            ),
            inches(2.0 * u)
        );

        let mut removed = reread.clone();
        assert!(removed.remove("Detail A").is_some());
        removed.page_scale = None;
        let mut edits = third.edit();
        assert!(removed.plan(&third, &mut edits).unwrap());
        work.file
            .save_incremental(&edits, &SaveOptions::default())
            .unwrap();
        let fourth = work.doc().clone();
        assert!(
            !dict_of(&fourth, page_id).has(b"VP"),
            "{label}: an empty /VP is removed"
        );
        let last = PageViewports::load(&fourth, page).unwrap();
        assert!(last.page_scale.is_none() && last.viewports.is_empty());
    }
}

#[test]
fn foreign_viewports_keep_their_own_keys() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let page_id = doc.page_id(0).unwrap();
    // A viewport of another application: a measure dictionary of its own (1 in = 4 ft), a
    // vendor key, and one entry this crate cannot read (no /BBox).
    let mut nf = Dictionary::new();
    nf.set("U", text_obj("ft"));
    nf.set("C", Object::Real(4.0 / 72.0));
    let mut measure = Dictionary::new();
    measure.set("Type", Object::Name(b"Measure".to_vec()));
    measure.set("Subtype", Object::Name(b"RL".to_vec()));
    measure.set("R", text_obj("1 in = 4 ft"));
    measure.set("X", Object::Array(vec![Object::Dictionary(nf.clone())]));
    let mut dist = nf;
    dist.set("C", Object::Integer(1));
    measure.set("D", Object::Array(vec![Object::Dictionary(dist)]));
    let mut vp = Dictionary::new();
    vp.set("Type", Object::Name(b"Viewport".to_vec()));
    vp.set(
        "BBox",
        Object::Array([100, 100, 300, 300].map(Object::Integer).to_vec()),
    );
    vp.set("Name", text_obj("Their detail"));
    vp.set("Measure", Object::Dictionary(measure));
    vp.set("VNDRScaleId", Object::Integer(7));
    let mut odd = Dictionary::new();
    odd.set("Type", Object::Name(b"Viewport".to_vec()));
    odd.set("Name", text_obj("No box"));
    let mut edits = doc.edit();
    edits.patch(page_id).set(
        "VP",
        Object::Array(vec![
            Object::Dictionary(odd.clone()),
            Object::Dictionary(vp),
        ]),
    );
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let doc = work.doc().clone();

    let mut vps = PageViewports::load(&doc, 0).unwrap();
    assert!(!vps.is_dirty());
    assert!(vps.page_scale.is_none());
    let theirs = vps.get("Their detail").unwrap();
    assert_eq!(theirs.bbox, Rect::new(100.0, 100.0, 300.0, 300.0));
    let spec = theirs.measure.clone().unwrap();
    assert!((spec.scale(1.0).unwrap().factor() - 48.0).abs() < 1e-4);
    // A measurement inside it, at their scale: 72 units are 4 ft.
    let mut a = Annotation::measure_length(
        0,
        Point::new(150.0, 150.0),
        Point::new(222.0, 150.0),
        eng(1.0),
        &common::origin(),
    );
    assert!(vps.apply_to(&mut a));
    assert_eq!(a.measure_text().unwrap(), "4.00 ft");

    // A page scale added in front: their entry, vendor key and all, and the unreadable one
    // are kept.
    vps.page_scale = Some(eng(1.0));
    let mut edits = doc.edit();
    assert!(vps.plan(&doc, &mut edits).unwrap());
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let after = work.doc().clone();
    let array = dict_of(&after, page_id)
        .get(b"VP")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(array.len(), 3);
    assert_eq!(string(array[0].as_dict().unwrap(), "Name"), PAGE_SCALE_NAME);
    assert_eq!(array[1], Object::Dictionary(odd));
    assert_eq!(
        array[2],
        dict_of(&doc, page_id)
            .get(b"VP")
            .unwrap()
            .as_array()
            .unwrap()[1]
    );
    let back = PageViewports::load(&after, 0).unwrap();
    assert_eq!(back.page_scale, Some(eng(1.0)));
    assert!(!back.is_dirty());
}

/// Editing a viewport of another application keeps what this crate does not model: a
/// geospatial `/Measure` it cannot read, the keys of an entry stored as an indirect object,
/// and other entries of the private dictionary.
#[test]
fn edited_foreign_viewports_keep_what_is_not_modelled() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let page_id = doc.page_id(0).unwrap();
    let mut geo_measure = acrobat_measure(20.0 / 72.0, false);
    geo_measure.set("Subtype", Object::Name(b"GEO".to_vec()));
    let mut geo = Dictionary::new();
    geo.set("Type", Object::Name(b"Viewport".to_vec()));
    geo.set(
        "BBox",
        Object::Array([100, 100, 300, 300].map(Object::Integer).to_vec()),
    );
    geo.set("Name", text_obj("Their geo"));
    geo.set("Measure", Object::Dictionary(geo_measure.clone()));
    let mut held = Dictionary::new();
    held.set("Type", Object::Name(b"Viewport".to_vec()));
    held.set(
        "BBox",
        Object::Array([300, 300, 400, 400].map(Object::Integer).to_vec()),
    );
    held.set("Name", text_obj("Held"));
    held.set(
        "Measure",
        Object::Dictionary(acrobat_measure(20.0 / 72.0, true)),
    );
    held.set("VNDRScaleId", Object::Integer(9));
    let mut private = Dictionary::new();
    private.set("VNDRNote", Object::Integer(1));
    held.set(PRIVATE_KEY, Object::Dictionary(private));
    let mut edits = doc.edit();
    let held_id = edits.add_object(Object::Dictionary(held));
    edits.patch(page_id).set(
        "VP",
        Object::Array(vec![Object::Dictionary(geo), Object::Reference(held_id)]),
    );
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let doc = work.doc().clone();

    let mut vps = PageViewports::load(&doc, 0).unwrap();
    assert!(vps.get("Their geo").unwrap().measure.is_none());
    assert!(vps.get("Held").unwrap().measure.is_some());
    assert!(vps
        .edit("Their geo", Rect::new(90.0, 90.0, 310.0, 290.0), None)
        .unwrap());
    assert!(vps
        .edit(
            "Held",
            Rect::new(310.0, 310.0, 410.0, 410.0),
            Some(eng(1.0))
        )
        .unwrap());
    let mut edits = doc.edit();
    assert!(vps.plan(&doc, &mut edits).unwrap());
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    let after = work.doc().clone();
    let array = dict_of(&after, page_id)
        .get(b"VP")
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(array.len(), 2);
    let geo = array[0].as_dict().unwrap();
    assert_eq!(
        geo.get(b"Measure").unwrap(),
        &Object::Dictionary(geo_measure),
        "the geospatial measure stays"
    );
    assert!(!geo.has(PRIVATE_KEY.as_bytes()));
    let held = array[1].as_dict().unwrap();
    assert_eq!(held.get(b"VNDRScaleId").unwrap(), &Object::Integer(9));
    assert_eq!(string(held, "Name"), "Held");
    let private = held.get(PRIVATE_KEY.as_bytes()).unwrap().as_dict().unwrap();
    assert_eq!(private.get(b"VNDRNote").unwrap(), &Object::Integer(1));
    assert!(private.has(b"PlModel"));
    let back = PageViewports::load(&after, 0).unwrap();
    assert_eq!(back.get("Held").unwrap().measure, Some(eng(1.0)));
    assert_eq!(
        back.get("Their geo").unwrap().bbox,
        Rect::new(90.0, 90.0, 310.0, 290.0)
    );
}

/// A foreign annotation dictionary with a plain appearance of its own.
fn foreign(
    edits: &mut pl_pdf::EditSet,
    page: usize,
    mut dict: Dictionary,
    rect: [i64; 4],
) -> ObjectId {
    let mut form = Dictionary::new();
    form.set("Type", Object::Name(b"XObject".to_vec()));
    form.set("Subtype", Object::Name(b"Form".to_vec()));
    form.set(
        "BBox",
        Object::Array(rect.iter().map(|v| Object::Integer(*v)).collect()),
    );
    let ap_id = edits.add_stream(form, b"0 0 1 RG 2 w 100 100 m 300 300 l S".to_vec());
    let mut ap = Dictionary::new();
    ap.set("N", Object::Reference(ap_id));
    dict.set("Type", Object::Name(b"Annot".to_vec()));
    dict.set(
        "Rect",
        Object::Array(rect.iter().map(|v| Object::Integer(*v)).collect()),
    );
    dict.set("AP", Object::Dictionary(ap));
    edits.add_annotation(page, dict)
}

fn acrobat_measure(feet_per_unit: f64, area: bool) -> Dictionary {
    let mut x = Dictionary::new();
    x.set("Type", Object::Name(b"NumberFormat".to_vec()));
    x.set("U", text_obj("ft"));
    x.set("C", Object::Real(feet_per_unit as f32));
    x.set("D", Object::Integer(100));
    let mut d = x.clone();
    d.set("C", Object::Integer(1));
    let mut m = Dictionary::new();
    m.set("Type", Object::Name(b"Measure".to_vec()));
    m.set("Subtype", Object::Name(b"RL".to_vec()));
    m.set("R", text_obj("1 in = 20 ft"));
    m.set("X", Object::Array(vec![Object::Dictionary(x)]));
    m.set("D", Object::Array(vec![Object::Dictionary(d)]));
    if area {
        let mut a = Dictionary::new();
        a.set("U", text_obj("sq ft"));
        a.set("C", Object::Integer(1));
        a.set("D", Object::Integer(100));
        m.set("A", Object::Array(vec![Object::Dictionary(a)]));
    }
    m
}

#[test]
fn foreign_dimensions_with_the_standard_keys_become_measurements() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let k = 20.0 / 72.0;
    let mut edits = doc.edit();
    let name = |s: &str| Object::Name(s.as_bytes().to_vec());
    let nums = |v: &[i64]| Object::Array(v.iter().map(|x| Object::Integer(*x)).collect());

    let mut line = Dictionary::new();
    line.set("Subtype", name("Line"));
    line.set("NM", text_obj("their-length"));
    line.set("L", nums(&[100, 100, 316, 100]));
    line.set("IT", name("LineDimension"));
    line.set("Measure", Object::Dictionary(acrobat_measure(k, false)));
    line.set("LL", Object::Integer(20));
    line.set("LLE", Object::Integer(5));
    line.set("Cap", Object::Boolean(true));
    line.set(
        "LE",
        Object::Array(vec![name("OpenArrow"), name("OpenArrow")]),
    );
    line.set("Contents", text_obj("60.00 ft"));
    line.set("VNDRKey", Object::Integer(3));
    foreign(&mut edits, 0, line, [90, 90, 330, 130]);

    let mut poly = Dictionary::new();
    poly.set("Subtype", name("Polygon"));
    poly.set("NM", text_obj("their-area"));
    poly.set("Vertices", nums(&[100, 200, 244, 200, 244, 272, 100, 272]));
    poly.set("IT", name("PolygonDimension"));
    poly.set("Measure", Object::Dictionary(acrobat_measure(k, true)));
    foreign(&mut edits, 0, poly, [98, 198, 246, 274]);

    let mut run = Dictionary::new();
    run.set("Subtype", name("PolyLine"));
    run.set("NM", text_obj("their-run"));
    run.set("Vertices", nums(&[100, 400, 172, 400, 172, 472]));
    run.set("IT", name("PolyLineDimension"));
    run.set("Measure", Object::Dictionary(acrobat_measure(k, false)));
    foreign(&mut edits, 0, run, [98, 398, 174, 474]);

    // A perimeter as Acrobat writes one: a polyline dimension that ends where it starts.
    let mut ring = Dictionary::new();
    ring.set("Subtype", name("PolyLine"));
    ring.set("NM", text_obj("their-perimeter"));
    ring.set(
        "Vertices",
        nums(&[300, 400, 444, 400, 444, 472, 300, 472, 300, 400]),
    );
    ring.set("IT", name("PolyLineDimension"));
    ring.set("Measure", Object::Dictionary(acrobat_measure(k, false)));
    foreign(&mut edits, 0, ring, [298, 398, 446, 474]);

    // One with an ending this crate cannot draw, and one with a geospatial measure.
    let mut odd = Dictionary::new();
    odd.set("Subtype", name("Line"));
    odd.set("NM", text_obj("their-odd"));
    odd.set("L", nums(&[100, 500, 200, 500]));
    odd.set("IT", name("LineDimension"));
    odd.set("Measure", Object::Dictionary(acrobat_measure(k, false)));
    odd.set("LE", Object::Array(vec![name("VNDRFancy"), name("None")]));
    foreign(&mut edits, 0, odd, [98, 490, 202, 510]);
    let mut geo = Dictionary::new();
    geo.set("Subtype", name("Line"));
    geo.set("NM", text_obj("their-geo"));
    geo.set("L", nums(&[100, 550, 200, 550]));
    geo.set("IT", name("LineDimension"));
    let mut g = acrobat_measure(k, false);
    g.set("Subtype", name("GEO"));
    geo.set("Measure", Object::Dictionary(g));
    foreign(&mut edits, 0, geo, [98, 540, 202, 560]);

    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    work.original = work.bytes();
    let doc = work.doc().clone();
    let set = AnnotSet::load(&doc);
    assert!(!set.is_dirty());

    let length = set.get(&"their-length".into()).unwrap();
    let m = length.measurement().expect("a length");
    assert_eq!(m.kind, MeasureKind::Length);
    assert_eq!((m.leader, m.leader_extension, m.caption), (20.0, 5.0, true));
    assert_eq!(
        m.caption_position,
        CaptionPosition::Inline,
        "the PDF default"
    );
    assert_eq!(m.scale, ScaleRef::Page);
    assert!((length.quantities().unwrap().length.unwrap() - 60.0 * FT).abs() < 1e-4);
    assert_eq!(length.measure_text().unwrap(), "60.00 ft");
    assert_eq!(length.style.line_end, LineEnding::OpenArrow);

    let area = set.get(&"their-area".into()).unwrap();
    let m = area.measurement().unwrap();
    assert_eq!(m.kind, MeasureKind::Area);
    assert!(!m.caption, "their polygon dimension draws no value of ours");
    // 144 x 72 units: 40 ft x 20 ft.
    assert!((area.quantities().unwrap().area.unwrap() - 800.0 * FT * FT).abs() < 1e-3);
    assert_eq!(area.measure_text().unwrap(), "800.00 sq ft");

    let run = set.get(&"their-run".into()).unwrap();
    assert_eq!(run.measurement().unwrap().kind, MeasureKind::Polylength);
    assert_eq!(run.measure_text().unwrap(), "40.00 ft");

    // 144 x 72 units round: 120 ft, four corners (the repeated one is not a vertex).
    let ring = set.get(&"their-perimeter".into()).unwrap();
    assert_eq!(ring.measurement().unwrap().kind, MeasureKind::Perimeter);
    assert_eq!(ring.kind, AnnotKind::Polygon);
    assert!(matches!(&ring.geometry, Geometry::Points(p) if p.len() == 4));
    assert_eq!(ring.measure_text().unwrap(), "120.00 ft");

    for unmodelled in ["their-odd", "their-geo"] {
        let a = set.get(&unmodelled.into()).unwrap();
        assert!(
            a.measurement().is_none() && !a.is_modelled(),
            "{unmodelled}"
        );
        assert!(matches!(a.geometry, Geometry::Rect(_)));
    }

    // A save that does not concern them leaves them alone, and they keep their appearance.
    let before = doc.clone();
    let mut set = AnnotSet::load(&before);
    set.add(Annotation::square(
        0,
        Rect::new(400.0, 600.0, 450.0, 650.0),
        &common::origin(),
    ))
    .unwrap();
    let (plan, _) = work.save(&set);
    assert!(plan.modified.is_empty());
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[before.page_id(0).unwrap()]);

    // Moving their length redraws it as ours, with the value they would not have written.
    let mut set = AnnotSet::load(&after);
    let id = "their-length".into();
    set.get_mut(&id).unwrap().translate(Vec2::new(0.0, 30.0));
    let (plan, _) = work.save(&set);
    let change = &plan.modified[0];
    assert!(change.appearance.is_some());
    for key in ["L", "AP", "Rect"] {
        assert!(
            change.keys.contains(&key.to_string()),
            "{key}: {:?}",
            change.keys
        );
    }
    let last = work.doc().clone();
    let d = dict_of(&last, change.object);
    assert_eq!(
        d.get(b"VNDRKey").unwrap(),
        &Object::Integer(3),
        "vendor keys survive"
    );
    assert_eq!(d.get(b"IT").unwrap(), &name("LineDimension"));
    let measure = d.get(b"Measure").unwrap().as_dict().unwrap();
    assert!(measure.has(b"X") && measure.has(b"D"));
    assert!(!measure.has(b"A"), "no empty /A where the file had none");
    let reread = AnnotSet::load(&last);
    let got = reread.get(&id).unwrap();
    assert_eq!(got.measurement().unwrap().leader, 20.0);
    assert_eq!(got.measure_text().unwrap(), "60.00 ft");
    assert!(!reread.is_dirty());
}

#[test]
fn edits_follow_the_geometry_and_the_scale() {
    let fixture = pl_testkit::user_unit();
    let mut work = Work::new(&fixture);
    let u = work.geometry(0).user_unit();
    assert_eq!(u, 2.0);
    let o = work.sample_origin(0);
    let og = common::origin();
    let mut area = Annotation::measure_area(
        0,
        square(o, 20.0, 20.0, 72.0),
        vec![square(o, 40.0, 40.0, 18.0)],
        eng(u),
        &og,
    );
    let id = area.id.clone();
    // 72 units on a /UserUnit 2 page: two inches of paper, 40 ft. 1600 - 100 sq ft.
    assert_eq!(area.measure_text().unwrap(), "1500.00 sq ft");
    area.style.fill = Some(Rgb::new(1.0, 0.9, 0.6));
    let mut set = AnnotSet::load(work.doc());
    set.add(area).unwrap();
    work.save(&set);

    // Moved: the cutout goes with it and the value stays.
    let mut set = AnnotSet::load(work.doc());
    let a = set.get_mut(&id).unwrap();
    a.translate(Vec2::new(10.0, 5.0));
    assert_eq!(a.measure.as_ref().unwrap().cutouts[0][0], pt(o, 50.0, 45.0));
    // A vertex dragged out by 36 units: 72 x 108 less the cutout.
    a.move_handle(pl_annot::HandleKind::Vertex(2), pt(o, 102.0, 133.0));
    a.move_handle(pl_annot::HandleKind::Vertex(3), pt(o, 30.0, 133.0));
    a.sync_measurement();
    let want = (72.0 * 108.0 - 18.0 * 18.0) * (40.0 / 72.0f64).powi(2);
    assert_eq!(a.measure_text().unwrap(), format!("{want:.2} sq ft"));
    let (plan, _) = work.save(&set);
    let d = dict_of(work.doc(), plan.modified[0].object).clone();
    assert_eq!(string(&d, "Contents"), format!("{want:.2} sq ft"));
    let reread = AnnotSet::load(work.doc());
    let got = reread.get(&id).unwrap();
    assert_eq!(
        got.measure.as_ref().unwrap().cutouts[0][0],
        pt(o, 50.0, 45.0)
    );
    assert!(!reread.is_dirty());

    // Another scale, without syncing: the save still writes the new value.
    let mut set = AnnotSet::load(work.doc());
    let a = set.get_mut(&id).unwrap();
    a.measure.as_mut().unwrap().spec = Some(MeasureSpec::new(
        &Scale::ratio(100.0).unwrap(),
        u,
        &MeasureFormat::new(
            LengthFormat::decimal(Unit::Metre, 2),
            AreaFormat::new(AreaUnit::SqMetre, 2),
        ),
    ));
    let (plan, _) = work.save(&set);
    let keys = &plan.modified[0].keys;
    for key in ["Measure", "Contents", "AP", PRIVATE_KEY] {
        assert!(keys.contains(&key.to_string()), "{key}: {keys:?}");
    }
    // A unit is 2/72 in; at 1:100 that is 100 * 2 * 0.0254 / 72 m.
    let k = 100.0 * 2.0 * 0.0254 / 72.0;
    let sq_m = (72.0 * 108.0 - 18.0 * 18.0) * k * k;
    let d = dict_of(work.doc(), plan.modified[0].object).clone();
    assert_eq!(string(&d, "Contents"), format!("{sq_m:.2} sq m"));
    let measure = d.get(b"Measure").unwrap().as_dict().unwrap();
    assert_eq!(string(measure, "R"), "1 mm = 100 mm");

    // A count gains a marker and is dragged by one of them.
    let mut set = AnnotSet::load(work.doc());
    let mut count = Annotation::count(
        0,
        &[pt(o, 150.0, 30.0), pt(o, 170.0, 30.0)],
        CountSymbol::Star,
        &og,
    );
    assert!(count.add_count_point(pt(o, 190.0, 30.0)));
    let handles = count.handles();
    assert_eq!(handles.len(), 3);
    assert!(count.move_handle(handles[1].kind, pt(o, 170.0, 60.0)));
    assert_eq!(count.count_points()[1], pt(o, 170.0, 60.0));
    count.sync_measurement();
    let count_id = set.add(count.clone()).unwrap();
    work.save(&set);
    let reread = AnnotSet::load(work.doc());
    let got = reread.get(&count_id).unwrap();
    assert_eq!(got.kind, AnnotKind::Ink);
    assert_eq!(got.detached(), count.detached());
    assert_eq!(got.meta.contents.as_deref(), Some("3"));
}

/// The real-world sample: large civil sheets. Skipped when the file is absent. Its text is
/// never printed.
#[test]
fn a_measurement_on_a_real_sheet() {
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    if !sample.exists() {
        eprintln!("SKIPPED: {} is not there", sample.display());
        return;
    }
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("pl-annot-measure-sample");
    std::fs::create_dir_all(&dir).unwrap();
    let copy = dir.join("sheet.pdf");
    std::fs::copy(&sample, &copy).unwrap();
    let mut work = Work::from_bytes("sheet.pdf", std::fs::read(&copy).unwrap());
    let _ = std::fs::remove_file(&copy);
    let geometry = work.geometry(0);
    let u = geometry.user_unit();
    let media = geometry.media_box();
    // 34 x 22 in.
    assert!(
        (media.width() * u - 34.0 * 72.0).abs() < 1.0
            && (media.height() * u - 22.0 * 72.0).abs() < 1.0
    );
    let o = Vec2::new(media.x0 + 200.0, media.y0 + 200.0);
    let og = common::origin();
    // Ten inches of paper at 1" = 20': 200 ft.
    let mut a = Annotation::measure_length(0, pt(o, 0.0, 0.0), pt(o, 720.0 / u, 0.0), eng(u), &og);
    a.set_rotation(geometry.rotate().degrees());
    assert_eq!(a.measure_text().unwrap(), "200.00'");
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    set.add(a.clone()).unwrap();
    work.save(&set);
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[before.page_id(0).unwrap()]);
    let reread = AnnotSet::load(&after);
    assert_eq!(reread.get(&a.id).unwrap().detached(), a.detached());
}
