//! XFDF: export then import gives an equal model, per kind; standard-only files are read;
//! broken input is refused or skipped with a reason.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use pl_annot::{
    AnnotId, AnnotKind, AnnotSet, Annotation, BlendMode, Flags, FontStyle, FreeTextIntent,
    Geometry, HAlign, LineEnding, LinkTarget, Origin, PdfDate, ReviewState, Rgb, RgbaImage,
    StampSpec, VAlign,
};
use pl_geom::{Point, Rect, Vec2};
use pl_pdf::{PdfFile, SaveOptions};
use pl_takeoff::xml::{self, Element, Node};
use pl_takeoff::{
    checkmark_status, custom_value, export_xfdf, import_xfdf, set_custom_value, Builtin,
    ColumnType, ListContext, Schema, SkipReason, Table, Value, XfdfError, XfdfOptions,
};

fn origin() -> Origin {
    Origin::new(
        "Renée O'Brien",
        PdfDate::parse("D:20261124101500+01'00'").unwrap(),
    )
}

fn pt(o: Vec2, x: f64, y: f64) -> Point {
    Point::new(o.x + x, o.y + y)
}

fn rect(o: Vec2, x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    Rect::new(o.x + x0, o.y + y0, o.x + x1, o.y + y1)
}

/// One named case: the annotations that belong together, in set order.
struct Case {
    name: &'static str,
    annotations: Vec<Annotation>,
}

/// Every modelled kind, each with properties that differ from the defaults, placed relative
/// to `o` on `page`. `rotation` is the page's `/Rotate`, which the text kinds follow.
fn cases(page: usize, o: Vec2, rotation: u16) -> Vec<Case> {
    let og = origin();
    let mut out = Vec::new();
    let mut case = |name: &'static str, annotations: Vec<Annotation>| {
        out.push(Case { name, annotations });
    };

    let mut square = Annotation::square(page, rect(o, 10.03, 20.07, 110.5, 80.25), &og);
    square.style.stroke = Some(Rgb::new(1.0, 0.9, 0.2));
    square.style.fill = Some(Rgb::new(0.123, 0.456, 0.789));
    square.style.width = 2.5;
    square.style.dash = Some(vec![4.0, 1.5]);
    square.style.opacity = 0.35;
    square.meta.contents = Some("Fläche ≈ 5,4 m²\nline <two> & \"three\"\ttab".into());
    square.meta.subject = Some("Area (面積)".into());
    square.meta.flags = square
        .meta
        .flags
        .with(Flags::LOCKED, true)
        .with(Flags::LOCKED_CONTENTS, true);
    case("square", vec![square]);

    let mut circle = Annotation::circle(page, rect(o, 150.0, 20.0, 230.0, 70.0), &og);
    circle.style.stroke = None;
    circle.style.fill = Some(Rgb::new(0.0, 0.5, 1.0));
    circle.style.cloud = Some(2.0);
    circle.style.blend = BlendMode::Multiply;
    circle.meta.author = None;
    circle.meta.contents = Some(String::new());
    case("circle", vec![circle]);

    let mut cloud = Annotation::square(page, rect(o, 250.0, 20.0, 330.0, 70.0), &og);
    cloud.style.cloud = Some(1.0);
    cloud.style.dash = Some(vec![3.0]);
    case("cloud square", vec![cloud]);

    let mut line = Annotation::line(page, pt(o, 10.0, 100.0), pt(o, 120.125, 131.3), &og);
    line.style.line_start = LineEnding::Circle;
    line.style.line_end = LineEnding::ClosedArrow;
    line.style.fill = Some(Rgb::new(1.0, 1.0, 1.0));
    line.style.width = 0.75;
    case("line", vec![line]);

    let mut polyline = Annotation::polyline(
        page,
        vec![
            pt(o, 150.0, 100.0),
            pt(o, 180.0, 130.0),
            pt(o, 210.0, 100.0),
            pt(o, 240.0, 130.0),
        ],
        &og,
    );
    polyline.style.line_end = LineEnding::OpenArrow;
    polyline.style.dash = Some(vec![6.0, 2.0, 1.0, 2.0]);
    case("polyline", vec![polyline]);

    let mut polygon = Annotation::polygon(
        page,
        vec![
            pt(o, 260.0, 100.0),
            pt(o, 330.0, 100.0),
            pt(o, 295.0, 141.7),
        ],
        &og,
    );
    polygon.style.fill = Some(Rgb::new(0.9, 0.95, 1.0));
    polygon.style.cloud = Some(1.0);
    polygon.meta.modified = None;
    case("polygon", vec![polygon]);

    let mut ink = Annotation::ink(
        page,
        vec![
            vec![
                pt(o, 10.0, 160.0),
                pt(o, 20.5, 171.25),
                pt(o, 31.0, 160.0),
                pt(o, 42.0, 172.0),
            ],
            vec![pt(o, 60.0, 165.0)],
        ],
        &og,
    );
    ink.style.width = 3.0;
    ink.style.stroke = Some(Rgb::new(0.0, 0.2, 0.6));
    case("ink", vec![ink]);

    let boxes = [
        rect(o, 150.0, 160.0, 250.0, 172.0),
        rect(o, 150.0, 176.0, 220.0, 188.0),
    ];
    for (name, kind) in [
        ("highlight", AnnotKind::Highlight),
        ("underline", AnnotKind::Underline),
        ("strikeout", AnnotKind::StrikeOut),
        ("squiggly", AnnotKind::Squiggly),
    ] {
        let mut a = Annotation::text_markup_from_boxes(kind, page, &boxes, rotation, &og).unwrap();
        a.meta.contents = Some(format!("{name} text"));
        if name == "underline" {
            a.style.stroke = Some(Rgb::new(0.0, 0.6, 0.0));
            a.style.opacity = 0.8;
        }
        case(name, vec![a]);
    }

    let mut note = Annotation::note(page, rect(o, 10.0, 200.0, 28.0, 218.0), "See RFI 12", &og);
    note.icon = Some("Help".into());
    note.open = true;
    let mut popup = Annotation::popup_for(&mut note, rect(o, 40.0, 200.0, 220.0, 320.0), &og);
    popup.open = true;
    let reply = Annotation::reply(
        &note,
        "Answered in RFI 12a",
        &Origin::new("Bo", PdfDate::from_unix(1_790_000_000)),
    );
    let nested = Annotation::reply(&reply, "Thanks", &og);
    let status = Annotation::review_status(&note, ReviewState::Completed, &og);
    let check = checkmark_status(&note, true, &og);
    case(
        "note with popup, replies and statuses",
        vec![note, popup, reply, nested, status, check],
    );

    case(
        "link to a web address",
        vec![Annotation::link(
            page,
            rect(o, 250.0, 200.0, 330.0, 214.0),
            LinkTarget::Uri("https://example.com/a?b=1&c=\"2\"".into()),
            &og,
        )],
    );
    let mut to_page = Annotation::link(
        page,
        rect(o, 250.0, 220.0, 330.0, 234.0),
        LinkTarget::Page {
            page: 3,
            at: Some(Point::new(72.5, 700.0)),
        },
        &og,
    );
    to_page.style.stroke = Some(Rgb::new(0.0, 0.0, 1.0));
    to_page.style.width = 1.0;
    case("link to a place on a page", vec![to_page]);
    case(
        "link to a whole page",
        vec![Annotation::link(
            page,
            rect(o, 250.0, 240.0, 330.0, 254.0),
            LinkTarget::Page { page: 0, at: None },
            &og,
        )],
    );
    case(
        "link with a target that stays in the PDF",
        vec![Annotation::link(
            page,
            rect(o, 250.0, 260.0, 330.0, 274.0),
            LinkTarget::Other,
            &og,
        )],
    );

    let mut text_box = Annotation::free_text(
        page,
        rect(o, 10.0, 240.0, 130.3, 300.7),
        "Verify on site.\nSee detail 7/A-501 & note <3>",
        &og,
    );
    text_box.set_rotation(i32::from(rotation));
    text_box.style.fill = Some(Rgb::new(1.0, 1.0, 0.85));
    text_box.style.stroke = Some(Rgb::new(0.1, 0.3, 0.7));
    text_box.style.width = 1.5;
    text_box.style.dash = Some(vec![2.0, 2.0]);
    text_box.style.opacity = 0.9;
    {
        let t = text_box.free_text.as_mut().unwrap();
        t.font = FontStyle::BoldItalic;
        t.size = 10.5;
        t.color = Rgb::new(0.2, 0.25, 0.3);
        t.align = HAlign::Centre;
        t.v_align = VAlign::Middle;
        t.line_spacing = 1.4;
        t.padding = 4.5;
    }
    case("text box", vec![text_box]);

    let mut callout = Annotation::callout(
        page,
        rect(o, 150.0, 260.0, 240.0, 300.0),
        pt(o, 135.0, 225.0),
        "Slope 2%",
        &og,
    );
    callout.set_rotation(i32::from(rotation));
    callout.free_text.as_mut().unwrap().align = HAlign::Right;
    case("callout", vec![callout]);

    let mut typewriter =
        Annotation::typewriter(page, rect(o, 10.0, 310.0, 130.0, 326.0), "TYP. U.N.O.", &og);
    typewriter.set_rotation(i32::from(rotation));
    typewriter.free_text.as_mut().unwrap().v_align = VAlign::Bottom;
    case("typewriter", vec![typewriter]);

    let mut stamp = Annotation::stamp(
        page,
        rect(o, 150.0, 310.0, 290.0, 356.0),
        StampSpec::approved().with_detail("R. O'Brien 2026-11-24"),
        &og,
    );
    stamp.set_rotation(i32::from(rotation));
    stamp.style.opacity = 0.75;
    case(
        "stamp",
        vec![stamp, {
            let mut plain = Annotation::stamp(
                page,
                rect(o, 150.0, 360.0, 290.0, 400.0),
                StampSpec::void(),
                &og,
            );
            plain.set_rotation(i32::from(rotation));
            plain
        }],
    );
    out
}

fn by_id(annotations: &[Annotation]) -> BTreeMap<AnnotId, Annotation> {
    annotations
        .iter()
        .map(|a| (a.id.clone(), a.clone()))
        .collect()
}

#[test]
fn export_then_import_gives_an_equal_model_for_each_kind() {
    for rotation in [0u16, 90, 270] {
        for case in cases(2, Vec2::new(36.0, 48.0), rotation) {
            let exported = export_xfdf(&case.annotations, &XfdfOptions::default());
            assert!(
                exported.skipped.is_empty(),
                "{}: {:?}",
                case.name,
                exported.skipped
            );
            assert_eq!(exported.written, case.annotations.len(), "{}", case.name);
            let imported = import_xfdf(&exported.xml)
                .unwrap_or_else(|e| panic!("{}: {e}\n{}", case.name, exported.xml));
            assert!(
                imported.skipped.is_empty(),
                "{}: {:?}",
                case.name,
                imported.skipped
            );
            assert_eq!(
                imported.annotations.len(),
                case.annotations.len(),
                "{}",
                case.name
            );
            for (got, want) in imported.annotations.iter().zip(&case.annotations) {
                assert_eq!(
                    got, want,
                    "{} (rotation {rotation})\n{}",
                    case.name, exported.xml
                );
                assert!(got.baseline().is_none() && got.is_modelled());
            }
            // A second trip writes the same text: nothing drifts.
            let again = export_xfdf(&imported.annotations, &XfdfOptions::default());
            assert_eq!(again.xml, exported.xml, "{}", case.name);
        }
    }
}

#[test]
fn the_whole_document_round_trips_in_one_file() {
    let all: Vec<Annotation> = (0..3)
        .flat_map(|page| cases(page, Vec2::new(10.0 * page as f64, 5.0), 0))
        .flat_map(|c| c.annotations)
        .collect();
    let exported = export_xfdf(
        &all,
        &XfdfOptions {
            href: Some("plans & sections.pdf".into()),
        },
    );
    assert_eq!(exported.written, all.len());
    assert!(exported.xml.starts_with(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xfdf xmlns=\"http://ns.adobe.com/xfdf/\""
    ));
    let imported = import_xfdf(&exported.xml).unwrap();
    assert_eq!(imported.href.as_deref(), Some("plans & sections.pdf"));
    assert_eq!(imported.annotations, all);

    // The thread structure survives: the list built from the import is the same list.
    let before = Table::build(&all, &ListContext::default());
    let after = Table::build(&imported.annotations, &ListContext::default());
    assert_eq!(before, after);
    let note = before.rows.iter().find(|r| r.kind_name == "Note").unwrap();
    assert_eq!(
        (note.replies, note.status, note.checked),
        (2, ReviewState::Completed, true)
    );
}

#[test]
fn list_column_values_travel_with_the_annotation() {
    let mut s = Schema::new();
    s.add("Trade", ColumnType::Text, &[]).unwrap();
    s.add("Qty", ColumnType::Number, &[]).unwrap();
    s.add("Ratio", ColumnType::Number, &[]).unwrap();
    s.add("Third", ColumnType::Number, &[]).unwrap();
    s.add("Done", ColumnType::Checkmark, &[]).unwrap();
    s.add("Due", ColumnType::Date, &[]).unwrap();
    let values = [
        ("trade", Value::text("Électricité <LV> & \"data\"")),
        ("qty", Value::Number(12.0)),
        ("ratio", Value::Number(0.5)),
        ("third", Value::Number(1.0 / 3.0)),
        ("done", Value::Bool(false)),
        ("due", Value::day(2027, 2, 28)),
    ];
    let mut a = Annotation::square(0, Rect::new(0.0, 0.0, 10.0, 10.0), &origin());
    for (id, v) in &values {
        set_custom_value(&mut a, s.get(id).unwrap(), v.clone()).unwrap();
    }
    let exported = export_xfdf([&a], &XfdfOptions::default());
    let imported = import_xfdf(&exported.xml).unwrap();
    assert_eq!(imported.annotations, [a]);
    for (id, v) in &values {
        assert_eq!(
            &custom_value(&imported.annotations[0], s.get(id).unwrap()),
            v,
            "{id}"
        );
    }
}

#[test]
fn bluebeam_column_values_travel_with_the_annotation() {
    let text = |s: &str| {
        let mut bytes = vec![0xFE, 0xFF];
        for unit in s.encode_utf16() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        pl_pdf::Object::String(bytes, pl_pdf::StringFormat::Hexadecimal)
    };
    let mut a = Annotation::square(0, Rect::new(0.0, 0.0, 10.0, 10.0), &origin());
    a.column_data = Some(vec![
        text("Structural"),
        text(""),
        text("Réf № <A&B> \"L2\""),
        text(
            "line one
  line two",
        ),
        text("4"),
        text(""),
        text("True"),
    ]);
    let exported = export_xfdf([&a], &XfdfOptions::default());
    assert!(exported.xml.contains("pl:bscolumns"));
    let imported = import_xfdf(&exported.xml).unwrap();
    assert_eq!(imported.annotations, [a]);
    // An annotation without Bluebeam values gets none.
    let b = Annotation::square(0, Rect::new(0.0, 0.0, 10.0, 10.0), &origin());
    let imported = import_xfdf(&export_xfdf([&b], &XfdfOptions::default()).xml).unwrap();
    assert_eq!(imported.annotations[0].column_data, None);
}

/// Removes everything in the `pl:` namespace, leaving what any XFDF writer would produce.
fn standard_only(e: &Element) -> Element {
    let mut out = Element::new(&e.name);
    for (k, v) in &e.attributes {
        if !k.starts_with("pl:") && k != "xmlns:pl" {
            out.set(k, v.as_str());
        }
    }
    for child in &e.children {
        match child {
            Node::Text(t) => out.push_text(t),
            Node::Element(c) if c.name.starts_with("pl:") => {}
            Node::Element(c) => out.push(standard_only(c)),
        }
    }
    out
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.07
}

fn same_place(a: &Geometry, b: &Geometry) -> bool {
    let (ra, rb) = (a.bounds(), b.bounds());
    std::mem::discriminant(a) == std::mem::discriminant(b)
        && close(ra.x0, rb.x0)
        && close(ra.y0, rb.y0)
        && close(ra.x1, rb.x1)
        && close(ra.y1, rb.y1)
}

/// Without our extension attributes the file is plain XFDF: everything standard is still read,
/// colours to eight bits, a shape's box to within the rounding of `/Rect`.
#[test]
fn the_standard_attributes_alone_carry_the_annotation() {
    let all: Vec<Annotation> = cases(1, Vec2::new(36.0, 48.0), 90)
        .into_iter()
        .filter(|c| c.name != "stamp")
        .flat_map(|c| c.annotations)
        .collect();
    let exported = export_xfdf(&all, &XfdfOptions::default());
    let mut plain = String::new();
    standard_only(&xml::parse(&exported.xml).unwrap()).write(&mut plain);
    assert!(!plain.contains("pl:"));
    let imported = import_xfdf(&plain).unwrap();
    assert!(imported.skipped.is_empty(), "{:?}", imported.skipped);
    assert_eq!(imported.annotations.len(), all.len());
    let u8s = |c: Option<Rgb>| c.map(|c| c.to_u8());
    for (got, want) in imported.annotations.iter().zip(&all) {
        let name = format!("{} {}", want.kind.pdf_name(), want.id);
        assert_eq!(got.id, want.id);
        assert_eq!((got.page, &got.kind), (want.page, &want.kind), "{name}");
        assert_eq!(got.meta, want.meta, "{name}");
        assert_eq!(
            (&got.icon, &got.popup, &got.parent, &got.link),
            (&want.icon, &want.popup, &want.parent, &want.link),
            "{name}"
        );
        assert!(
            same_place(&got.geometry, &want.geometry),
            "{name}: {:?} / {:?}",
            got.geometry,
            want.geometry
        );
        if !matches!(want.geometry, Geometry::Rect(_)) {
            assert_eq!(got.geometry, want.geometry, "{name}");
        }
        assert_eq!(u8s(got.style.fill), u8s(want.style.fill), "{name}");
        if want.style.stroke.is_some() {
            assert_eq!(u8s(got.style.stroke), u8s(want.style.stroke), "{name}");
        }
        assert_eq!(
            (
                got.style.width,
                &got.style.dash,
                got.style.opacity,
                got.style.cloud
            ),
            (
                want.style.width,
                &want.style.dash,
                want.style.opacity,
                want.style.cloud
            ),
            "{name}"
        );
        assert_eq!(
            (got.style.line_start, got.style.line_end),
            (want.style.line_start, want.style.line_end),
            "{name}"
        );
        assert_eq!(got.rotation(), want.rotation(), "{name}");
        if let (Some(g), Some(w)) = (&got.free_text, &want.free_text) {
            assert_eq!(
                (g.intent, g.font, g.size, g.align),
                (w.intent, w.font, w.size, w.align),
                "{name}"
            );
            assert_eq!(g.color.to_u8(), w.color.to_u8(), "{name}");
            assert_eq!(g.callout, w.callout, "{name}");
        }
        assert_eq!(got.free_text.is_some(), want.free_text.is_some());
    }
    // A stamp without our label is another application's: left out, with the reason.
    let stamp = Annotation::stamp(
        0,
        Rect::new(0.0, 0.0, 90.0, 40.0),
        StampSpec::draft(),
        &origin(),
    );
    let mut plain = String::new();
    standard_only(&xml::parse(&export_xfdf([&stamp], &XfdfOptions::default()).xml).unwrap())
        .write(&mut plain);
    let imported = import_xfdf(&plain).unwrap();
    assert!(imported.annotations.is_empty());
    assert_eq!(imported.skipped.len(), 1);
    assert!(
        imported.skipped[0].contains("stamp from another application"),
        "{:?}",
        imported.skipped
    );
}

/// A text box as Acrobat writes it: the colour of the default appearance (non-stroking) is
/// the border's, and the text colour is in the default style. pl-annot reads the same data
/// from a PDF the same way.
#[test]
fn a_text_box_from_acrobat_keeps_its_border_and_text_colours_apart() {
    let text = r##"<xfdf xmlns="http://ns.adobe.com/xfdf/"><annots>
    <freetext page="0" rect="100,500,300,560" name="bordered" width="1" color="#FFFFFF">
      <defaultappearance>1 0 0 rg /Helv 12 Tf</defaultappearance>
      <defaultstyle>font: Helvetica 12pt; text-align:left; color:#0000FF</defaultstyle>
      <contents>Red border, blue words</contents>
    </freetext>
    <freetext page="0" rect="100,400,300,460" name="borderless" width="0">
      <defaultappearance>0 0.5 0 rg /Helv 10 Tf</defaultappearance>
      <contents>Typed on the page</contents>
    </freetext>
    <freetext page="0" rect="100,300,300,360" name="no-style" width="2">
      <defaultappearance>0 0 1 RG 0 0 1 rg /Helv 10 Tf</defaultappearance>
      <contents>One colour for both</contents>
    </freetext>
    <freetext page="0" rect="100,100,300,260" fringe="60,100,0,0" name="callout" width="1"
              callout="110,110,150,205,160,230">
      <defaultappearance>0 0 0 rg /Helv 10 Tf</defaultappearance>
      <contents>Leader below</contents>
    </freetext>
    </annots></xfdf>"##;
    let got = import_xfdf(text).unwrap();
    assert!(got.skipped.is_empty(), "{:?}", got.skipped);
    let by = by_id(&got.annotations);
    let a = |id: &str| &by[&AnnotId::from(id)];

    let bordered = a("bordered");
    assert_eq!(bordered.style.stroke, Some(Rgb::RED), "the /DA colour");
    assert_eq!(bordered.free_text.as_ref().unwrap().color, Rgb::BLUE);
    assert_eq!(bordered.style.fill, Some(Rgb::WHITE));
    assert_eq!(bordered.style.width, 1.0);

    let borderless = a("borderless");
    assert_eq!(borderless.style.stroke, None, "no width, no border");
    assert_eq!(
        borderless.free_text.as_ref().unwrap().color.to_u8(),
        [0, 128, 0],
        "with no default style the /DA colour is the text's"
    );

    let one = a("no-style");
    assert_eq!(one.style.stroke, Some(Rgb::BLUE));
    assert_eq!(one.free_text.as_ref().unwrap().color, Rgb::BLUE);

    // The fringe is left, bottom, right, top: the box is at the top of the rectangle, above
    // the room the leader takes.
    assert_eq!(
        a("callout").geometry,
        Geometry::Rect(Rect::new(160.0, 200.0, 300.0, 260.0))
    );

    // What we write says the same thing to such a reader: a blue border and red words.
    let mut ours = Annotation::free_text(0, Rect::new(10.0, 10.0, 110.0, 50.0), "Hi", &origin());
    ours.style.stroke = Some(Rgb::BLUE);
    ours.free_text.as_mut().unwrap().color = Rgb::RED;
    let mut none = Annotation::typewriter(0, Rect::new(10.0, 60.0, 110.0, 90.0), "Hi", &origin());
    none.style.width = 2.0;
    none.free_text.as_mut().unwrap().color = Rgb::RED;
    let exported = export_xfdf([&ours, &none], &XfdfOptions::default());
    assert!(
        exported
            .xml
            .contains("<defaultappearance>0 0 1 RG 0 0 1 rg /Helv 12 Tf</defaultappearance>"),
        "{}",
        exported.xml
    );
    assert!(exported.xml.contains("color:#FF0000"));
    let back = import_xfdf(&exported.xml).unwrap();
    assert_eq!(back.annotations[0].detached(), ours.detached());
    assert_eq!(back.annotations[1].detached(), none.detached());
    let mut plain = String::new();
    standard_only(&xml::parse(&exported.xml).unwrap()).write(&mut plain);
    let back = import_xfdf(&plain).unwrap();
    assert_eq!(back.annotations[0].style.stroke, Some(Rgb::BLUE));
    assert_eq!(
        back.annotations[0].free_text.as_ref().unwrap().color,
        Rgb::RED
    );
    assert_eq!(
        (
            back.annotations[1].style.stroke,
            back.annotations[1].style.width
        ),
        (None, 0.0),
        "a box of ours with no border has none for any reader"
    );
    assert_eq!(
        back.annotations[1].free_text.as_ref().unwrap().color,
        Rgb::RED
    );
}

/// Coordinates no page has are not brought in: drawing them (a cloud border most of all)
/// would take the memory of the machine.
#[test]
fn geometry_far_outside_any_page_is_left_out() {
    let text = r##"<xfdf xmlns="http://ns.adobe.com/xfdf/"><annots>
    <square page="0" rect="0,0,10000000,10000000" style="cloudy" name="huge"/>
    <square page="0" rect="0,0,1e300,50" name="wide"/>
    <line page="0" start="0,0" end="0,-5e9" name="long"/>
    <polygon page="0" name="far"><vertices>0,0;10,0;10,2e6</vertices></polygon>
    <freetext page="0" rect="0,0,100,50" callout="1e9,0,50,60" name="leader"><contents>x</contents></freetext>
    <square page="0" rect="0,0,14400,14400" style="cloudy" name="largest-page"/>
    <square page="0" rect="-50000,-50000,-49000,-49000" name="offset-box"/>
    </annots></xfdf>"##;
    let start = std::time::Instant::now();
    let got = import_xfdf(text).unwrap();
    let ids: Vec<&str> = got.annotations.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["largest-page", "offset-box"]);
    assert_eq!(got.skipped.len(), 5, "{:?}", got.skipped);
    for (why, name) in got
        .skipped
        .iter()
        .zip(["huge", "wide", "long", "far", "leader"])
    {
        assert!(
            why.contains(name) && why.contains("not on any page"),
            "{why}"
        );
    }
    // What is kept can be drawn, and promptly.
    for a in &got.annotations {
        assert!(!a.draw_list().is_empty(), "{}", a.id);
    }
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

/// A file in the shape other applications write, typed by hand.
#[test]
fn a_file_written_elsewhere_is_read() {
    let text = r##"<?xml version="1.0" encoding="UTF-8"?>
<!-- exported by some viewer -->
<xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">
  <annots>
    <square page="0" rect="100,200,300,400" color="#FF0000" interior-color="#00ff00" width="3"
            style="dash" dashes="3,2" opacity="0.5" name="sq-1" title="J. Smith" subject="Rectangle"
            date="D:20240102030405+02'00'" creationdate="D:20240101000000Z" flags="print,locked">
      <contents>Check &amp; confirm &#x2014; "soon"</contents>
      <popup flags="print,nozoom,norotate" open="no" page="0" rect="310,300,490,400"/>
    </square>
    <circle page="1" rect="10,10,60,40" fringe="5,5,5,5" style="cloudy" intensity="2" name="ci-1"/>
    <line page="0" start="10,20" end="110,120" head="OpenArrow" tail="None" color="#00F" name="ln-1"
          rect="5,15,115,125" title="A"><contents-richtext><body xmlns="http://www.w3.org/1999/xhtml"><p>first <b>bold</b></p><p>second</p></body></contents-richtext></line>
    <highlight page="2" coords="72,720,172,720,72,708,172,708" color="#FFFF00" name="hl-1" rect="72,708,172,720"/>
    <polygon page="0" name="pg-1" rect="0,0,10,10"><vertices>0,0;10,0;10,10</vertices></polygon>
    <ink page="0" name="ink-1" rect="0,0,9,9"><inklist><gesture>1,1;2,2;3,1</gesture><gesture>5,5;6,6</gesture></inklist></ink>
    <text page="0" rect="50,700,70,720" icon="Comment" name="t-1" inreplyto="sq-1" state="Accepted" statemodel="Review" title="K"/>
    <text page="0" rect="50,700,70,720" name="t-2" inreplyto="sq-1" replyType="reply"><contents>ok</contents></text>
    <freetext page="0" rect="100,500,300,560" name="ft-1" justification="right" rotation="90" width="2" color="#FFFFE0"
              callout="80,480,90,520,100,530" head="ClosedArrow" intent="FreeTextCallout">
      <contents-richtext><body xmlns="http://www.w3.org/1999/xhtml"><p>Callout text</p></body></contents-richtext>
      <defaultappearance>0 0 1 RG 1 0 0 rg /Helvetica-Bold 14 Tf</defaultappearance>
      <defaultstyle>font: bold Helvetica 14pt; color:#FF0000</defaultstyle>
    </freetext>
    <link page="0" rect="1,2,3,4" name="lk-1"><OnActivation><Action Trigger="U"><URI Name="https://example.org/"/></Action></OnActivation></link>
    <link page="0" rect="1,2,3,4" name="lk-2"><OnActivation><Action Trigger="U"><GoTo><Dest><XYZ Page="4" Left="10" Top="800" Zoom="0"/></Dest></GoTo></Action></OnActivation></link>
    <fileattachment page="0" rect="1,1,2,2" name="fa-1"/>
    <caret page="0" rect="1,1,2,2"/>
    <stamp page="0" rect="1,1,90,40" icon="Approved" name="st-1"/>
    <line page="0" start="1,1" name="broken-1"/>
    <square rect="1,1,2,2" name="no-page"/>
    <polygon page="x" name="bad-page"><vertices>0,0;1,1</vertices></polygon>
    <ink page="0" name="bad-ink"><inklist><gesture>1,1;nan,2</gesture></inklist></ink>
  </annots>
  <f href="set.pdf"/>
</xfdf>"##;
    let got = import_xfdf(text).unwrap();
    assert_eq!(got.href.as_deref(), Some("set.pdf"));
    let ids: Vec<&str> = got.annotations.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids.len(), 12);
    assert_eq!(ids[0], "sq-1");
    assert_eq!(
        ids[2..],
        ["ci-1", "ln-1", "hl-1", "pg-1", "ink-1", "t-1", "t-2", "ft-1", "lk-1", "lk-2"]
    );
    let by = by_id(&got.annotations);
    let a = |id: &str| &by[&AnnotId::from(id)];

    let sq = a("sq-1");
    assert_eq!(
        sq.geometry,
        Geometry::Rect(Rect::new(100.0, 200.0, 300.0, 400.0))
    );
    assert_eq!(sq.style.stroke, Some(Rgb::RED));
    assert_eq!(sq.style.fill, Some(Rgb::GREEN));
    assert_eq!((sq.style.width, sq.style.opacity), (3.0, 0.5));
    assert_eq!(sq.style.dash, Some(vec![3.0, 2.0]));
    assert_eq!(sq.meta.author.as_deref(), Some("J. Smith"));
    assert_eq!(
        sq.meta.contents.as_deref(),
        Some("Check & confirm — \"soon\"")
    );
    assert_eq!(sq.meta.modified, PdfDate::parse("D:20240102030405+02'00'"));
    assert_eq!(sq.meta.created.unwrap().to_unix(), 1_704_067_200);
    assert_eq!(sq.meta.flags, Flags(Flags::PRINT | Flags::LOCKED));
    // Its popup follows it, linked both ways, on the same page.
    let popup = &got.annotations[1];
    assert_eq!(popup.kind, AnnotKind::Popup);
    assert_eq!(
        (popup.parent.as_ref(), sq.popup.as_ref()),
        (Some(&sq.id), Some(&popup.id))
    );
    assert_eq!(
        popup.geometry,
        Geometry::Rect(Rect::new(310.0, 300.0, 490.0, 400.0))
    );
    assert!(!popup.open);

    // /Rect less the fringe is the shape; no colour attribute means the default black.
    let ci = a("ci-1");
    assert_eq!(
        ci.geometry,
        Geometry::Rect(Rect::new(15.0, 15.0, 55.0, 35.0))
    );
    assert_eq!(ci.style.cloud, Some(2.0));
    assert_eq!(ci.style.stroke, Some(Rgb::BLACK));
    assert_eq!(
        (ci.page, ci.meta.author.as_deref(), ci.meta.modified),
        (1, None, None)
    );

    let ln = a("ln-1");
    assert_eq!(
        ln.geometry,
        Geometry::Line {
            a: Point::new(10.0, 20.0),
            b: Point::new(110.0, 120.0)
        }
    );
    assert_eq!(
        (ln.style.line_start, ln.style.line_end),
        (LineEnding::OpenArrow, LineEnding::None)
    );
    assert_eq!(ln.style.stroke, Some(Rgb::BLUE));
    assert_eq!(
        ln.meta.contents.as_deref(),
        Some("first bold\nsecond"),
        "rich text read as plain lines"
    );

    // Acrobat's order (top-left, top-right, bottom-left, bottom-right) becomes our ring.
    assert_eq!(
        a("hl-1").geometry,
        Geometry::Quads(vec![[
            Point::new(72.0, 708.0),
            Point::new(172.0, 708.0),
            Point::new(172.0, 720.0),
            Point::new(72.0, 720.0)
        ]])
    );
    assert_eq!(a("hl-1").style.blend, BlendMode::Multiply);
    assert_eq!(
        a("pg-1").geometry,
        Geometry::Points(vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0)
        ])
    );
    match &a("ink-1").geometry {
        Geometry::Ink(strokes) => {
            assert_eq!(strokes.iter().map(Vec::len).collect::<Vec<_>>(), [3, 2])
        }
        other => panic!("{other:?}"),
    }

    // A status and a reply, attached to the square: the list shows them as such.
    assert_eq!(a("t-1").review_state(), Some(ReviewState::Accepted));
    assert_eq!(a("t-1").icon.as_deref(), Some("Comment"));
    assert!(a("t-2").is_reply());
    assert_eq!(a("t-2").meta.reply_type.as_deref(), Some("R"));
    let table = Table::build(&got.annotations, &ListContext::default());
    let row = &table.rows[table.row_of(&sq.id).unwrap()];
    assert_eq!((row.status, row.replies), (ReviewState::Accepted, 1));
    assert_eq!(
        table.value(table.row_of(&sq.id).unwrap(), &Builtin::Author.into()),
        Value::text("J. Smith")
    );

    let ft = a("ft-1");
    let t = ft.free_text.as_ref().expect("a text box we can edit");
    assert_eq!(t.intent, FreeTextIntent::Callout);
    assert_eq!(
        (t.font, t.size, t.align, t.rotation),
        (FontStyle::Bold, 14.0, HAlign::Right, 90)
    );
    assert_eq!(t.color, Rgb::RED);
    assert_eq!(
        t.callout,
        [
            Point::new(80.0, 480.0),
            Point::new(90.0, 520.0),
            Point::new(100.0, 530.0)
        ]
    );
    assert_eq!(ft.style.stroke, Some(Rgb::BLUE));
    assert_eq!(ft.style.fill.map(|c| c.to_u8()), Some([255, 255, 224]));
    assert_eq!(
        (ft.style.width, ft.style.line_end),
        (2.0, LineEnding::ClosedArrow)
    );
    assert_eq!(ft.meta.contents.as_deref(), Some("Callout text"));
    assert!(ft.is_modelled());
    assert!(ft.validate().is_ok());

    assert_eq!(
        a("lk-1").link,
        Some(LinkTarget::Uri("https://example.org/".into()))
    );
    assert_eq!(
        a("lk-2").link,
        Some(LinkTarget::Page {
            page: 4,
            at: Some(Point::new(10.0, 800.0))
        })
    );

    // What could not be read is listed by element and name, with the reason.
    let skipped = got.skipped.join("\n");
    assert_eq!(got.skipped.len(), 7, "{skipped}");
    for needle in [
        "<fileattachment> \"fa-1\": this kind of annotation is not read",
        "<caret>: this kind of annotation is not read",
        "<stamp> \"st-1\": it is a stamp from another application",
        "<line> \"broken-1\": it has no start and end",
        "<square> \"no-page\": it has no page",
        "<polygon> \"bad-page\": page \"x\" is not a page number",
        "<ink> \"bad-ink\": a gesture is not a list of points",
    ] {
        assert!(skipped.contains(needle), "missing: {needle}\n{skipped}");
    }
}

#[test]
fn what_cannot_be_written_is_reported_and_bad_files_are_refused() {
    let og = origin();
    let unknown = Annotation::new(
        AnnotKind::Unknown("Caret".into()),
        0,
        Geometry::Rect(Rect::new(0.0, 0.0, 5.0, 5.0)),
        &og,
    );
    let foreign_text = Annotation::new(
        AnnotKind::FreeText,
        0,
        Geometry::Rect(Rect::new(0.0, 0.0, 50.0, 20.0)),
        &og,
    );
    let picture = Annotation::image_stamp(
        0,
        Rect::new(0.0, 0.0, 20.0, 20.0),
        Arc::new(RgbaImage::new(1, 1, vec![255, 0, 0, 255]).unwrap()),
        &og,
    );
    let mut nan = Annotation::line(0, Point::new(0.0, 0.0), Point::new(f64::NAN, 1.0), &og);
    nan.meta.contents = Some("x".into());
    let mut parent = Annotation::square(0, Rect::new(0.0, 0.0, 5.0, 5.0), &og);
    let orphan = Annotation::popup_for(&mut parent, Rect::new(10.0, 0.0, 50.0, 40.0), &og);
    let good = Annotation::circle(0, Rect::new(0.0, 0.0, 5.0, 5.0), &og);
    let count = Annotation::count(0, &[Point::new(1.0, 1.0)], pl_annot::CountSymbol::Dot, &og);
    let all = [
        unknown.clone(),
        foreign_text.clone(),
        picture.clone(),
        nan.clone(),
        orphan.clone(),
        count.clone(),
        good.clone(),
    ];
    let exported = export_xfdf(&all, &XfdfOptions::default());
    assert_eq!(exported.written, 1);
    assert_eq!(
        exported.skipped,
        [
            (unknown.id, SkipReason::NotModelled),
            (foreign_text.id, SkipReason::NotModelled),
            (picture.id, SkipReason::ImageStamp),
            (nan.id, SkipReason::BadGeometry),
            (orphan.id, SkipReason::OrphanPopup),
            (count.id, SkipReason::Measurement),
        ]
    );
    assert_eq!(import_xfdf(&exported.xml).unwrap().annotations, [good]);

    // No annotations at all is a valid, empty file.
    let empty = export_xfdf(&[] as &[Annotation], &XfdfOptions::default());
    let read = import_xfdf(&empty.xml).unwrap();
    assert!(read.annotations.is_empty() && read.skipped.is_empty() && read.href.is_none());
    assert!(import_xfdf("<xfdf/>").unwrap().annotations.is_empty());

    assert_eq!(
        import_xfdf("<html><body/></html>"),
        Err(XfdfError::NotXfdf("html".into()))
    );
    match import_xfdf("<xfdf><annots><square page='0'></annots></xfdf>") {
        Err(XfdfError::Xml(e)) => assert!(e.message.contains("closes"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(import_xfdf(""), Err(XfdfError::Xml(_))));
    // An entity defined in a DOCTYPE is never expanded.
    let bomb = "<!DOCTYPE xfdf [<!ENTITY a \"aaaaaaaaaa\">]><xfdf><annots><text page='0' rect='0,0,1,1'><contents>&a;</contents></text></annots></xfdf>";
    assert!(
        matches!(import_xfdf(bomb), Err(XfdfError::Xml(e)) if e.message.contains("unknown reference"))
    );
    // An id is generated where the file gives none, and two imports do not collide.
    let nameless = "<xfdf><annots><text page='0' rect='0,0,18,18'/><text page='0' rect='0,0,18,18'/></annots></xfdf>";
    let read = import_xfdf(nameless).unwrap();
    assert_eq!(read.annotations.len(), 2);
    assert_ne!(read.annotations[0].id, read.annotations[1].id);
    assert_eq!(
        read.annotations[0].style.stroke,
        Some(Rgb::new(1.0, 0.9, 0.2)),
        "a note's default colour"
    );
}

/// Through a real file on plain, rotated, UserUnit and CropBox pages: what pl-annot reads back
/// after a save is what XFDF carries, and the text kinds keep the page's rotation.
#[test]
fn annotations_read_from_saved_files_round_trip_on_every_page_geometry() {
    type MakeFixture = fn() -> pl_testkit::Fixture;
    let targets: [(&str, MakeFixture, usize); 6] = [
        ("plain", pl_testkit::plain, 0),
        ("rotate90", pl_testkit::rotated_pages, 1),
        ("rotate180", pl_testkit::rotated_pages, 2),
        ("rotate270", pl_testkit::rotated_pages, 3),
        ("userunit", pl_testkit::user_unit, 0),
        ("cropbox-rotate90", pl_testkit::crop_box, 2),
    ];
    for (name, fixture, page) in targets {
        let fixture = fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{name}.pdf"));
        std::fs::write(&path, &fixture.bytes).unwrap();
        let mut file = PdfFile::open(&path).unwrap();
        let geometry = file.document().unwrap().page(page).unwrap().geometry;
        let rotation = geometry.rotate().degrees() as u16;
        let crop = geometry.crop_box();
        let o = Vec2::new(crop.x0 + 12.0, crop.y0 + 12.0);

        let mut set = AnnotSet::new();
        for case in cases(page, o, rotation) {
            // The fixture pages are small: keep what fits, and leave out links to pages the
            // fixture does not have.
            for a in case.annotations {
                let b = a.bounds();
                let fits = b.x1 < crop.x1 && b.y1 < crop.y1;
                let dangling = matches!(a.link, Some(LinkTarget::Page { page, .. }) if page >= fixture.manifest.page_count());
                if (fits || a.kind == AnnotKind::Popup || a.meta.reply_to.is_some()) && !dangling {
                    set.add(a).unwrap();
                }
            }
        }
        // Dropping a parent that did not fit drops what hangs off it.
        let ids: Vec<AnnotId> = set.iter().map(|a| a.id.clone()).collect();
        for id in ids {
            let Some(a) = set.get(&id) else { continue };
            let lost_parent = a.parent.as_ref().is_some_and(|p| !set.contains(p))
                || a.meta.reply_to.as_ref().is_some_and(|p| !set.contains(p));
            if lost_parent {
                set.remove(&id);
            }
        }
        assert!(
            set.len() >= 12,
            "{name}: only {} annotations fit",
            set.len()
        );
        let plan = set.plan_save(file.document().unwrap()).unwrap();
        file.save_incremental(&plan.edits, &SaveOptions::default())
            .unwrap();

        let mut reopened = PdfFile::open(&path).unwrap();
        let loaded = AnnotSet::load(reopened.document().unwrap());
        let on_page: Vec<Annotation> = loaded
            .iter()
            .filter(|a| a.page == page && set.contains(&a.id))
            .map(Annotation::detached)
            .collect();
        assert_eq!(on_page.len(), set.len(), "{name}");
        let exported = export_xfdf(&on_page, &XfdfOptions::default());
        assert!(
            exported.skipped.is_empty(),
            "{name}: {:?}",
            exported.skipped
        );
        let imported = import_xfdf(&exported.xml).unwrap();
        assert!(
            imported.skipped.is_empty(),
            "{name}: {:?}",
            imported.skipped
        );
        let (got, want) = (by_id(&imported.annotations), by_id(&on_page));
        assert_eq!(got.len(), want.len(), "{name}");
        let mut text_kinds = 0;
        for (id, w) in &want {
            let g = &got[id];
            assert_eq!(g, w, "{name}: {} {id}", w.kind.pdf_name());
            if g.free_text.is_some() || g.stamp.is_some() {
                assert_eq!(
                    g.rotation(),
                    rotation,
                    "{name}: {} follows the page",
                    g.kind.pdf_name()
                );
                text_kinds += 1;
            }
        }
        assert!(text_kinds >= 1, "{name}");
    }
}

fn sample_path() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    p.is_file().then_some(p)
}

/// The real drawing set (read only, nothing is written and none of its text is used): markups
/// placed on its first and last sheets list correctly and travel through XFDF unchanged.
#[test]
fn markups_on_the_real_sample_list_and_round_trip() {
    let Some(path) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap();
    let pages = doc.page_count();
    assert!(pages >= 2);
    let existing = AnnotSet::load(doc);
    let before = Table::build(existing.iter(), &ListContext::default())
        .rows
        .len();

    let mut set = existing.clone();
    let mut added = Vec::new();
    for page in [0, pages - 1] {
        let geometry = doc.page(page).unwrap().geometry;
        let crop = geometry.crop_box();
        let o = Vec2::new(crop.x0 + crop.width() / 3.0, crop.y0 + crop.height() / 3.0);
        for case in cases(page, o, geometry.rotate().degrees() as u16) {
            for a in case.annotations {
                if matches!(a.link, Some(LinkTarget::Page { page, .. }) if page >= pages) {
                    continue;
                }
                added.push(a.clone());
                set.add(a).unwrap();
            }
        }
    }
    let labels: Vec<String> = (0..pages).map(|i| format!("C-{:03}", i + 1)).collect();
    let table = Table::build(
        set.iter(),
        &ListContext {
            page_labels: &labels,
            ..ListContext::default()
        },
    );
    let new_rows: Vec<_> = table
        .rows
        .iter()
        .filter(|r| added.iter().any(|a| a.id == r.id))
        .collect();
    assert_eq!(table.rows.len(), before + new_rows.len());
    assert!(new_rows.iter().all(|r| r.page_label == labels[r.page]));
    assert_eq!(
        new_rows
            .iter()
            .filter(|r| r.depth == 0 && r.replies == 2)
            .count(),
        2,
        "one thread per sheet"
    );
    assert!(new_rows.iter().all(|r| r.x.is_finite() && r.width >= 0.0));

    let exported = export_xfdf(&added, &XfdfOptions::default());
    assert!(exported.skipped.is_empty());
    let imported = import_xfdf(&exported.xml).unwrap();
    assert_eq!(imported.annotations, added);
    eprintln!(
        "sample: {pages} sheets, {} markups listed, {} bytes of XFDF",
        new_rows.len(),
        exported.xml.len()
    );
}

/// The text itself, for one shape, one text box and one note with its popup: the standard
/// attributes other applications read, and our additions beside them.
#[test]
fn the_written_text_is_as_documented() {
    let og = Origin::new("Ada", PdfDate::parse("D:20261124101500Z").unwrap());
    let mut square = Annotation::square(0, Rect::new(10.0, 20.0, 110.0, 80.0), &og);
    square.id = AnnotId::from("sq-1");
    square.style.stroke = Some(Rgb::new(1.0, 0.9, 0.2));
    square.style.dash = Some(vec![3.0, 2.0]);
    square.meta.contents = Some("a < b".into());
    let mut text = Annotation::free_text(0, Rect::new(200.0, 20.0, 300.0, 60.0), "Hi", &og);
    text.id = AnnotId::from("ft-1");
    let mut note = Annotation::note(1, Rect::new(10.0, 10.0, 28.0, 28.0), "n", &og);
    note.id = AnnotId::from("t-1");
    let mut popup = Annotation::popup_for(&mut note, Rect::new(40.0, 10.0, 220.0, 130.0), &og);
    popup.id = AnnotId::from("p-1");
    note.popup = Some(popup.id.clone());
    let xml = export_xfdf(&[square, text, note, popup], &XfdfOptions::default()).xml;
    assert_eq!(xml, GOLDEN);
}

const GOLDEN: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/" xmlns:pl="urn:plumbline:xfdf:1" xml:space="preserve">
<annots>
<square page="0" rect="10,20,110,80" name="sq-1" title="Ada" subject="Rectangle" date="D:20261124101500Z" creationdate="D:20261124101500Z" flags="print" color="#FFE633" pl:color="1 0.9 0.2" width="1" style="dash" dashes="3,2"><contents>a &lt; b</contents></square>
<freetext page="0" rect="200,20,300,60" name="ft-1" title="Ada" subject="Text Box" date="D:20261124101500Z" creationdate="D:20261124101500Z" flags="print" pl:border="0.8 0 0" width="1" style="solid" justification="left" pl:font="Regular" pl:size="12" pl:text-color="0.8 0 0" pl:v-align="Top" pl:line-spacing="1.25" pl:padding="3"><contents-richtext><body xmlns="http://www.w3.org/1999/xhtml" xmlns:xfa="http://www.xfa.org/schema/xfa-data/1.0/" xfa:APIVersion="Acrobat:11.0.0" xfa:spec="2.0.2"><p>Hi</p></body></contents-richtext><defaultappearance>0.8 0 0 RG 0.8 0 0 rg /Helv 12 Tf</defaultappearance><defaultstyle>font: Helvetica,sans-serif 12pt; text-align:left; color:#CC0000</defaultstyle><contents>Hi</contents></freetext>
<text page="1" rect="10,10,28,28" name="t-1" title="Ada" subject="Note" date="D:20261124101500Z" creationdate="D:20261124101500Z" flags="print" color="#FFE633" pl:color="1 0.9 0.2" icon="Note"><contents>n</contents><popup page="1" rect="40,10,220,130" name="p-1" date="D:20261124101500Z" open="no"/></text>
</annots>
</xfdf>
"##;
