//! The emitted content streams, read back by an independent parser (hayro-syntax): every
//! operator is one the emitter means to write, with the right number of operands; every
//! resource a stream names is described; and the path geometry survives the trip.

mod cases;
mod common;

use hayro_syntax::content::ops::TypedInstruction;
use hayro_syntax::content::{TypedIter, UntypedIter};
use hayro_syntax::object::dict::keys::ANNOTS;
use hayro_syntax::object::{Array, Object};
use pl_draw::kurbo::{BezPath, PathEl, Point};
use pl_draw::{Appearance, DrawOp, XObject};

/// Operators the emitter writes, with their operand counts.
const OPERATORS: [(&str, usize); 25] = [
    ("q", 0),
    ("Q", 0),
    ("cm", 6),
    ("gs", 1),
    ("w", 1),
    ("J", 1),
    ("j", 1),
    ("M", 1),
    ("d", 2),
    ("rg", 3),
    ("RG", 3),
    ("m", 2),
    ("l", 2),
    ("c", 6),
    ("h", 0),
    ("S", 0),
    ("f", 0),
    ("f*", 0),
    ("B", 0),
    ("B*", 0),
    ("BT", 0),
    ("ET", 0),
    ("Tf", 2),
    ("Tm", 6),
    ("Tj", 1),
];

struct Parsed {
    /// Operators in order.
    operators: Vec<String>,
    /// The first path constructed in the stream.
    first_path: BezPath,
    /// Number of streams read, nested forms included.
    streams: usize,
}

fn number(o: &Object<'_>) -> f64 {
    match o {
        Object::Number(n) => n.as_f64(),
        other => panic!("expected a number, got {other:?}"),
    }
}

fn name(o: &Object<'_>) -> String {
    match o {
        Object::Name(n) => n.as_str().to_owned(),
        other => panic!("expected a name, got {other:?}"),
    }
}

fn parse(appearance: &Appearance, label: &str) -> Parsed {
    let mut operators = Vec::new();
    let mut first_path = BezPath::new();
    let mut path_done = false;
    let mut depth = 0i32;
    let mut in_text = false;
    let mut iter = UntypedIter::new(&appearance.content);
    while let Some(instruction) = iter.next() {
        let op = std::str::from_utf8(instruction.operator)
            .unwrap()
            .to_owned();
        let operands: Vec<Object<'_>> = instruction.operands().cloned().collect();
        let known = OPERATORS
            .iter()
            .chain([("Do", 1)].iter())
            .find(|(o, _)| *o == op)
            .unwrap_or_else(|| panic!("{label}: unexpected operator {op:?}"));
        assert_eq!(operands.len(), known.1, "{label}: operands of {op}");
        let res = &appearance.resources;
        match op.as_str() {
            "q" => depth += 1,
            "Q" => {
                depth -= 1;
                assert!(depth >= 0, "{label}: Q without q");
            }
            "BT" => {
                assert!(!in_text, "{label}: nested BT");
                in_text = true;
            }
            "ET" => {
                assert!(in_text, "{label}: ET without BT");
                in_text = false;
            }
            "gs" => {
                let n = name(&operands[0]);
                assert!(
                    res.ext_gstates.iter().any(|g| g.name == n),
                    "{label}: /{n} gs is not in the resources"
                );
            }
            "Tf" => {
                let n = name(&operands[0]);
                assert!(in_text, "{label}: Tf outside a text object");
                assert!(
                    res.fonts.iter().any(|f| f.name == n),
                    "{label}: font /{n} is not in the resources"
                );
                assert!(number(&operands[1]) > 0.0);
            }
            "Tj" => {
                assert!(in_text, "{label}: Tj outside a text object");
                assert!(matches!(operands[0], Object::String(_)));
            }
            "Do" => {
                let n = name(&operands[0]);
                assert!(
                    res.xobjects.iter().any(|x| x.name == n),
                    "{label}: XObject /{n} is not in the resources"
                );
            }
            "d" => {
                let Object::Array(a) = &operands[0] else {
                    panic!("{label}: d without an array");
                };
                let lens: Vec<f64> = a.iter::<f64>().collect();
                assert!(
                    !lens.is_empty() && lens.len().is_multiple_of(2),
                    "{label}: dash array {lens:?}"
                );
                assert!(lens.iter().all(|v| *v >= 0.0) && lens.iter().sum::<f64>() > 0.0);
            }
            "m" if !path_done => {
                first_path.move_to(Point::new(number(&operands[0]), number(&operands[1])))
            }
            "l" if !path_done => {
                first_path.line_to(Point::new(number(&operands[0]), number(&operands[1])))
            }
            "c" if !path_done => {
                let v: Vec<f64> = operands.iter().map(number).collect();
                first_path.curve_to((v[0], v[1]), (v[2], v[3]), (v[4], v[5]));
            }
            "h" if !path_done => first_path.close_path(),
            "S" | "f" | "f*" | "B" | "B*" => path_done = true,
            _ => {}
        }
        for o in &operands {
            if let Object::Number(n) = o {
                assert!(
                    n.as_f64().is_finite(),
                    "{label}: non-finite operand of {op}"
                );
            }
        }
        operators.push(op);
    }
    assert_eq!(depth, 0, "{label}: unbalanced q/Q");
    assert!(!in_text, "{label}: unterminated text object");

    // The typed reader agrees: it understands every instruction and stops nowhere early.
    let mut typed = TypedIter::new(&appearance.content);
    let mut typed_count = 0;
    while let Some(instruction) = typed.next() {
        assert!(
            !matches!(instruction, TypedInstruction::Fallback(_)),
            "{label}: an instruction hayro does not recognise: {instruction:?}"
        );
        typed_count += 1;
    }
    assert_eq!(
        typed_count,
        operators.len(),
        "{label}: the typed reader stopped early"
    );

    let mut streams = 1;
    for x in &appearance.resources.xobjects {
        match &x.object {
            XObject::Form(f) => {
                assert!(f.form.bbox.width() > 0.0 && f.form.bbox.height() > 0.0);
                streams += parse(&f.form, &format!("{label}/{}", x.name)).streams;
            }
            XObject::Image(i) => {
                assert_eq!(i.rgb.len(), (i.width * i.height * 3) as usize);
                if let Some(a) = &i.alpha {
                    assert_eq!(a.len(), (i.width * i.height) as usize);
                }
            }
        }
    }
    // Resource names are unique within their kind.
    let unique = |names: Vec<&str>| {
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.len() == names.len()
    };
    let r = &appearance.resources;
    assert!(unique(
        r.ext_gstates.iter().map(|g| g.name.as_str()).collect()
    ));
    assert!(unique(r.fonts.iter().map(|g| g.name.as_str()).collect()));
    assert!(unique(r.xobjects.iter().map(|g| g.name.as_str()).collect()));
    Parsed {
        operators,
        first_path,
        streams,
    }
}

#[test]
fn every_emitted_stream_reads_back_cleanly() {
    let mut cases = cases::all_geometry_cases();
    cases.extend(cases::text_cases());
    let (dashed, solid) = cases::dashed_cloud_cases();
    cases.extend([dashed, solid]);
    assert!(cases.len() > 100, "{} cases", cases.len());
    let mut streams = 0;
    for case in &cases {
        let appearance = pl_draw::emit(&case.list).unwrap();
        assert!(!appearance.content.is_empty(), "{}", case.name);
        assert_eq!(Some(appearance.bbox), case.list.bounds(), "{}", case.name);
        let parsed = parse(&appearance, &case.name);
        assert!(
            parsed
                .operators
                .iter()
                .any(|o| matches!(o.as_str(), "S" | "f" | "f*" | "B" | "B*" | "Tj" | "Do")),
            "{} paints nothing",
            case.name
        );
        streams += parsed.streams;
    }
    assert!(streams > cases.len(), "some cases have nested forms");
}

/// Coordinates are written as 32-bit floats; what is read back must still be the path.
#[test]
fn path_geometry_survives_the_round_trip() {
    let mut checked = 0;
    for case in cases::all_geometry_cases() {
        let [DrawOp::Path(op)] = &case.list.ops[..] else {
            continue;
        };
        let parsed = parse(&pl_draw::emit(&case.list).unwrap(), &case.name);
        let original = op.path.elements();
        let read = parsed.first_path.elements();
        assert_eq!(original.len(), read.len(), "{}", case.name);
        let near = |a: Point, b: Point| (a - b).hypot() < 1e-3;
        for (a, b) in original.iter().zip(read) {
            let same = match (*a, *b) {
                (PathEl::MoveTo(p), PathEl::MoveTo(q)) | (PathEl::LineTo(p), PathEl::LineTo(q)) => {
                    near(p, q)
                }
                (PathEl::CurveTo(p1, p2, p3), PathEl::CurveTo(q1, q2, q3)) => {
                    near(p1, q1) && near(p2, q2) && near(p3, q3)
                }
                (PathEl::ClosePath, PathEl::ClosePath) => true,
                _ => false,
            };
            assert!(same, "{}: {a:?} came back as {b:?}", case.name);
        }
        checked += 1;
    }
    assert!(checked > 40, "{checked} single-path cases");
}

/// The whole test document (page, annotation, appearance, nested forms, images, fonts) is a PDF
/// that hayro-syntax opens, with the one annotation on its page.
#[test]
fn the_test_documents_open_in_a_second_parser() {
    let mut cases = cases::all_geometry_cases();
    cases.extend(cases::text_cases());
    for case in cases.iter().step_by(7) {
        for variant in [&common::PLAIN, &common::CROP_ROTATED] {
            let appearance = pl_draw::emit(&case.list).unwrap();
            let bytes = common::build_pdf(
                variant,
                &appearance,
                common::Attach::Annotation(pdf_writer::types::AnnotationType::Square),
            );
            let pdf = hayro_syntax::Pdf::new(bytes)
                .unwrap_or_else(|e| panic!("{}: hayro-syntax cannot open it: {e:?}", case.name));
            let pages = pdf.pages();
            assert_eq!(pages.len(), 1);
            let annots = pages
                .iter()
                .next()
                .unwrap()
                .raw()
                .get::<Array<'_>>(ANNOTS)
                .map_or(0, |a| a.raw_iter().count());
            assert_eq!(annots, 1, "{}", case.name);
        }
    }
}
