//! FreeText: text boxes, callouts and typewriter text, on a plain page and on rotated,
//! `/UserUnit` and offset-box pages.
//!
//! * The wrap of the saved `/AP` is where PDFium draws it: the appearance stream is made the
//!   page's content and PDFium's character origins and advances are compared with the layout,
//!   within 1 pt; the extracted text is the typed string.
//! * On a rotated page the text reads upright in the displayed orientation.
//! * A callout's leader and ending render like the tiny-skia raster.
//! * Text outside WinAnsi is refused with the typed error and nothing is written.
//! * A foreign FreeText keeps its own appearance until it is edited, and its `/RC` and unknown
//!   keys until the text is changed.

mod common;

use common::{Target, Work, TARGETS};
use pl_annot::{
    reading_direction, rich_text, AnnotError, AnnotId, AnnotKind, AnnotSet, Annotation, DrawError,
    FontStyle, FreeTextIntent, Geometry, HAlign, LineEnding, Rgb, VAlign,
};
use pl_geom::{Point, Rect, Vec2};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, SaveOptions};
use pl_render::PdfiumBackend;

const TYPED: &str = "Wall type A2 is 140 mm block, plastered both sides. Check the fire \
                     rating: 60 min.\nNote caf\u{e9} \u{a3}5";

fn dict_of(doc: &PdfDocument, id: ObjectId) -> &Dictionary {
    doc.object(id).unwrap().as_dict().unwrap()
}

fn text_of(dict: &Dictionary, key: &str) -> String {
    let Object::String(bytes, _) = dict.get(key.as_bytes()).unwrap() else {
        panic!("/{key} is a string");
    };
    pl_annot::decode_text(bytes)
}

fn words(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A box of `w` by `h` as displayed, at the sample origin: its sides are swapped on a page
/// turned a quarter, so that the text has the same room on every page.
fn boxed(o: Vec2, rotate: i32, w: f64, h: f64) -> Rect {
    let (w, h) = if rotate % 180 == 90 { (h, w) } else { (w, h) };
    Rect::new(o.x + 40.0, o.y + 30.0, o.x + 40.0 + w, o.y + 30.0 + h)
}

/// A text box on the target page, turned to read upright there.
fn text_box(work: &mut Work, target: &Target) -> Annotation {
    let geometry = work.geometry(target.page);
    let rotate = geometry.rotate().degrees();
    let o = work.sample_origin(target.page);
    let mut a = Annotation::free_text(
        target.page,
        boxed(o, rotate, 150.0, 90.0),
        TYPED,
        &common::origin(),
    );
    a.set_rotation(rotate);
    a.style.width = 1.5;
    a.style.stroke = Some(Rgb::new(0.0, 0.25, 0.75));
    a.style.fill = Some(Rgb::new(1.0, 1.0, 0.75));
    let t = a.free_text.as_mut().unwrap();
    t.size = 10.0;
    t.color = Rgb::new(0.25, 0.0, 0.5);
    a
}

fn check_wrap(backend: &PdfiumBackend, target: &Target, failures: &mut Vec<String>) {
    let label = target.name;
    let fixture = (target.fixture)();
    let mut work = Work::new(&fixture);
    let geometry = work.geometry(target.page);
    let rotate = geometry.rotate().degrees();
    let a = text_box(&mut work, target);
    assert_eq!(i32::from(a.rotation()), rotate);

    let layout = a.text_layout().unwrap().unwrap();
    let (_, to) = a.text_frame().unwrap();
    assert!(
        layout.lines.len() >= 4,
        "{label}: the text wraps ({})",
        layout.lines.len()
    );
    assert!(layout.overflow.is_none(), "{label}: {:?}", layout.overflow);

    // Save, and read back an equal model.
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    set.add(a.clone()).unwrap();
    let (plan, _) = work.save(&set);
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[before.page_id(target.page).unwrap()]);
    let loaded = AnnotSet::load(&after);
    assert!(!loaded.is_dirty(), "{label}");
    let got = loaded.get(&a.id).unwrap();
    assert_eq!(got.detached(), a.detached(), "{label}: an equal model");
    assert!(got.is_modelled());

    // The keys other viewers read.
    let dict = dict_of(&after, plan.added[0].object);
    assert_eq!(
        dict.get(b"Subtype").unwrap(),
        &Object::Name(b"FreeText".to_vec())
    );
    assert_eq!(text_of(dict, "Contents"), TYPED);
    assert_eq!(
        text_of(dict, "DA"),
        "0 0.25 0.75 RG 0 0.25 0.75 rg /Helv 10 Tf",
        "the non-stroking colour of /DA is the border colour, as Acrobat reads it"
    );
    assert_eq!(
        got.free_text.as_ref().unwrap().color,
        Rgb::new(0.25, 0.0, 0.5),
        "{label}: the text colour comes back exactly although /DA does not hold it"
    );
    assert_eq!(
        text_of(dict, "DS"),
        "font: Helvetica,sans-serif 10pt; text-align:left; color:#400080"
    );
    assert_eq!(text_of(dict, "RC"), rich_text(TYPED));
    assert_eq!(dict.get(b"Q").unwrap(), &Object::Integer(0));
    assert_eq!(
        dict.get(b"IT").unwrap(),
        &Object::Name(b"FreeText".to_vec())
    );
    assert_eq!(
        pl_pdf::rect_from_object(dict.get(b"Rect").unwrap()),
        Some(a.geometry.bounds())
    );
    // /C is the background, as the specification says for a FreeText.
    assert_eq!(
        dict.get(b"C").unwrap(),
        &Object::Array(vec![
            Object::Integer(1),
            Object::Integer(1),
            Object::Real(0.75)
        ])
    );
    let bs = dict.get(b"BS").unwrap().as_dict().unwrap();
    assert_eq!(bs.get(b"W").unwrap(), &Object::Real(1.5));
    match rotate {
        0 => assert!(!dict.has(b"Rotate"), "{label}"),
        r => assert_eq!(dict.get(b"Rotate").unwrap(), &Object::Integer(i64::from(r))),
    }
    assert!(
        !dict.has(b"CL") && !dict.has(b"LE") && !dict.has(b"RD"),
        "{label}"
    );
    for key in ["NM", "M", "CreationDate", "T", "Subj", "F", "CA", "P", "AP"] {
        assert!(dict.has(key.as_bytes()), "{label}: /{key}");
    }
    // The appearance names the standard-14 font and nothing else.
    let ap = plan.added[0].appearance.expect("an appearance was written");
    let Some(Object::Stream(stream)) = after.object(ap) else {
        panic!("{label}: /AP /N is a stream");
    };
    let fonts = stream
        .dict
        .get(b"Resources")
        .and_then(Object::as_dict)
        .and_then(|r| r.get(b"Font"))
        .and_then(Object::as_dict)
        .unwrap();
    assert_eq!(fonts.len(), 1);
    let (_, font) = fonts.iter().next().unwrap();
    let font = after.resolve(font).unwrap().as_dict().unwrap();
    assert_eq!(
        font.get(b"BaseFont").unwrap(),
        &Object::Name(b"Helvetica".to_vec())
    );
    assert_eq!(
        font.get(b"Subtype").unwrap(),
        &Object::Name(b"Type1".to_vec())
    );
    assert_eq!(
        font.get(b"Encoding").unwrap(),
        &Object::Name(b"WinAnsiEncoding".to_vec())
    );

    // PDFium draws the saved appearance like the raster of the draw list.
    let r = common::render_pdf(backend, work.bytes(), target.page, &geometry);
    let ours = common::raster_over(&a.draw_list(), &geometry, &r.backdrop);
    let scale = common::scale_for(&geometry);
    if let Some(why) = common::ink_mismatch(&r.pdfium, &ours, &r.backdrop, scale, 1.0) {
        common::dump(&r.pdfium, &format!("text-{label}-pdfium"));
        common::dump(&ours, &format!("text-{label}-ours"));
        failures.push(format!("{label}: {why}"));
    }

    // Where PDFium puts each character of the saved appearance.
    common::flatten_onto_page(&mut work, target.page, ap);
    let text = common::page_text(backend, work.bytes(), target.page, &geometry);
    assert_eq!(
        words(&text.text()),
        words(TYPED),
        "{label}: extraction returns the typed string"
    );
    let typed: Vec<char> = TYPED.chars().collect();
    let ours: Vec<(char, Point, Point)> = (0..typed.len())
        .filter(|i| !typed[*i].is_whitespace())
        .map(|i| {
            let b = layout.char_box(i).expect("a drawn character has a box");
            let start = layout.carets[i].position;
            let end = start + Vec2::new(b.width(), 0.0);
            (typed[i], to * start, to * end)
        })
        .collect();
    let theirs: Vec<&pl_render::TextChar> = text
        .chars
        .iter()
        .filter(|c| !c.generated && !c.ch.is_whitespace())
        .collect();
    assert_eq!(
        ours.len(),
        theirs.len(),
        "{label}: every character is drawn"
    );
    // 1 pt, in the page's units.
    let tolerance = 1.0 / geometry.user_unit();
    let mut worst: f64 = 0.0;
    let mut worst_origin: f64 = 0.0;
    for ((ch, start, end), c) in ours.iter().zip(&theirs) {
        assert_eq!(*ch, c.ch, "{label}");
        let origin = Point::new(c.origin[0], c.origin[1]);
        worst_origin = worst_origin.max((origin - *start).hypot());
        // The loose box spans the advance along the reading direction.
        let bbox = Rect::new(c.bbox[0], c.bbox[1], c.bbox[2], c.bbox[3]);
        let (lo, hi, a0, a1) = if rotate % 180 == 90 {
            (bbox.y0, bbox.y1, start.y.min(end.y), start.y.max(end.y))
        } else {
            (bbox.x0, bbox.x1, start.x.min(end.x), start.x.max(end.x))
        };
        worst = worst.max((lo - a0).abs()).max((hi - a1).abs());
    }
    // The glyph origins are where the layout put them to a tenth of a point (measured: 0.013
    // units, the single-precision coordinates of the stream). PDFium's loose boxes are wider
    // than the advance for glyphs that overhang it (f, r: up to 0.35 units at this size), and
    // still within the 1 pt the plan asks for.
    if worst_origin > tolerance / 10.0 || worst > tolerance {
        failures.push(format!(
            "{label}: origins are up to {worst_origin:.4} and boxes up to {worst:.3} units              from the layout (limits {} and {tolerance})",
            tolerance / 10.0
        ));
    }
    if std::env::var_os("PL_ANNOT_VERBOSE").is_some() {
        println!("{label:20} worst origin {worst_origin:.5}, worst box edge {worst:.5} units");
    }

    // Upright as displayed: on screen every line runs left to right and lines go down.
    let to_display = geometry.page_to_display();
    let first = to_display * (to * layout.lines[0].origin);
    let first_end =
        to_display * (to * (layout.lines[0].origin + Vec2::new(layout.lines[0].width, 0.0)));
    let second = to_display * (to * layout.lines[1].origin);
    assert!(
        first_end.x > first.x + 10.0 && (first_end.y - first.y).abs() < 1e-6,
        "{label}: a line runs left to right on screen"
    );
    assert!(
        second.y > first.y + 1.0 && (second.x - first.x).abs() < 1e-6,
        "{label}: the next line is below"
    );

    // The same characters as a text selection: the reading direction is the annotation's
    // rotation, and the selection merges into one quad per line that matches the layout.
    let origins: Vec<Point> = theirs
        .iter()
        .take(12)
        .map(|c| Point::new(c.origin[0], c.origin[1]))
        .collect();
    let direction = reading_direction(&origins);
    assert_eq!(i32::from(direction), rotate, "{label}: reading direction");
    let boxes: Vec<Rect> = theirs
        .iter()
        .map(|c| Rect::new(c.bbox[0], c.bbox[1], c.bbox[2], c.bbox[3]))
        .collect();
    let markup = Annotation::text_markup_from_boxes(
        AnnotKind::Underline,
        target.page,
        &boxes,
        direction,
        &common::origin(),
    )
    .unwrap();
    let Geometry::Quads(quads) = &markup.geometry else {
        panic!()
    };
    let drawn: Vec<_> = layout.lines.iter().filter(|l| !l.text.is_empty()).collect();
    assert_eq!(quads.len(), drawn.len(), "{label}: one quad per line");
    for (quad, line) in quads.iter().zip(&drawn) {
        let start = to * line.origin;
        let end = to * (line.origin + Vec2::new(line.width, 0.0));
        // The quad's first edge is the bottom of the line, below the baseline by the descent.
        let along = (quad[1] - quad[0]).hypot();
        assert!(
            (along - line.width).abs() < tolerance,
            "{label}: quad length {along} against line width {}",
            line.width
        );
        let below = (quad[0] - start).hypot();
        assert!(
            below < 0.4 * 10.0,
            "{label}: the quad starts under the line start ({below})"
        );
        assert!(
            ((quad[1] - quad[0]) / along - (end - start) / line.width).hypot() < 1e-6,
            "{label}: the quad runs along the baseline"
        );
    }
}

#[test]
fn free_text_wrap_matches_pdfium_and_extracts_the_typed_string() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for target in &TARGETS {
        check_wrap(&backend, target, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn alignment_fonts_and_spacing_round_trip_and_move_the_text() {
    let backend = common::backend();
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let geometry = work.geometry(0);
    let o = work.sample_origin(0);
    let mut set = AnnotSet::load(work.doc());
    let mut made = Vec::new();
    for (i, (align, v_align, font)) in [
        (HAlign::Left, VAlign::Top, FontStyle::Regular),
        (HAlign::Centre, VAlign::Middle, FontStyle::Bold),
        (HAlign::Right, VAlign::Bottom, FontStyle::Italic),
        (HAlign::Centre, VAlign::Top, FontStyle::BoldItalic),
    ]
    .into_iter()
    .enumerate()
    {
        let y = o.y + 20.0 + 70.0 * i as f64;
        let mut a = Annotation::typewriter(
            0,
            Rect::new(o.x + 20.0, y, o.x + 220.0, y + 60.0),
            "Aligned text\nsecond line",
            &common::origin(),
        );
        let t = a.free_text.as_mut().unwrap();
        (t.align, t.v_align, t.font) = (align, v_align, font);
        t.size = 11.0;
        t.line_spacing = 1.3;
        t.padding = 2.5;
        t.color = Rgb::BLACK;
        set.add(a.clone()).unwrap();
        made.push(a);
    }
    let (plan, _) = work.save(&set);
    let after = work.doc().clone();
    let loaded = AnnotSet::load(&after);
    for (a, change) in made.iter().zip(&plan.added) {
        assert_eq!(loaded.get(&a.id).unwrap().detached(), a.detached());
        let dict = dict_of(&after, change.object);
        let t = a.free_text.as_ref().unwrap();
        assert_eq!(
            dict.get(b"Q").unwrap(),
            &Object::Integer(t.align.quadding())
        );
        assert_eq!(
            dict.get(b"IT").unwrap(),
            &Object::Name(b"FreeTextTypeWriter".to_vec())
        );
        // A typewriter has no border: no stroking colour in /DA, a zero width, no /C.
        assert!(!text_of(dict, "DA").contains("RG"));
        let bs = dict.get(b"BS").unwrap().as_dict().unwrap();
        assert_eq!(bs.get(b"W").unwrap(), &Object::Integer(0));
        assert!(!dict.has(b"C"));
        assert!(text_of(dict, "DS").contains(&format!("text-align:{}", t.align.css())));
        let da = pl_annot::parse_default_appearance(&text_of(dict, "DA"));
        assert_eq!((da.font, da.size), (Some(t.font), Some(11.0)));
    }
    // Each is drawn where the raster puts it: the ink boxes agree per annotation.
    let r = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    let scale = common::scale_for(&geometry);
    for a in &made {
        let ours = common::raster_over(&a.draw_list(), &geometry, &r.backdrop);
        // PDFium's render of this annotation alone: mask the others out by comparing only
        // inside this annotation's rectangle.
        let rect = a.geometry.bounds();
        let to_px = pl_draw::page_transform(&geometry, scale);
        let px = to_px.transform_rect_bbox(rect);
        let crop = |image: &pl_render::Bitmap| {
            let mut out = r.backdrop.clone();
            for y in px.y0.floor() as u32..px.y1.ceil() as u32 {
                for x in px.x0.floor() as u32..px.x1.ceil() as u32 {
                    let i = ((y * image.width + x) * 4) as usize;
                    out.pixels[i..i + 4].copy_from_slice(&image.pixels[i..i + 4]);
                }
            }
            out
        };
        let why = common::ink_mismatch(&crop(&r.pdfium), &crop(&ours), &r.backdrop, scale, 1.0);
        assert_eq!(why, None, "{:?}", a.free_text);
    }
    // Right-aligned text ends at the right padding; bottom-aligned text sits on the bottom.
    let right = &made[2];
    let layout = right.text_layout().unwrap().unwrap();
    let frame = right.geometry.bounds();
    for line in &layout.lines {
        assert!((line.origin.x + line.width - (frame.x1 - 2.5)).abs() < 1e-9);
    }
    let last = layout.lines.last().unwrap();
    assert!((last.origin.y + layout.descent - (frame.y0 + 2.5)).abs() < 1e-9);
}

#[test]
fn a_callout_leader_and_ending_render_like_the_raster() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for target in &TARGETS {
        let label = target.name;
        let fixture = (target.fixture)();
        let mut probe = Work::new(&fixture);
        let geometry = probe.geometry(target.page);
        let o = probe.sample_origin(target.page);
        let rect = Rect::new(o.x + 110.0, o.y + 90.0, o.x + 230.0, o.y + 140.0);
        let tip = Point::new(o.x + 20.0, o.y + 20.0);
        // Without text the comparison is pixel for pixel; with text, by ink.
        for (name, text) in [("bare", ""), ("worded", "See detail 4")] {
            let mut work = Work::new(&fixture);
            let mut a = Annotation::callout(target.page, rect, tip, text, &common::origin());
            a.set_rotation(geometry.rotate().degrees());
            a.style.width = 2.0;
            a.style.fill = Some(Rgb::new(0.75, 1.0, 0.75));
            a.style.line_end = LineEnding::ClosedArrow;
            let t = a.free_text.as_ref().unwrap();
            assert_eq!(t.intent, FreeTextIntent::Callout);
            assert_eq!(t.callout.len(), 3, "tip, knee, box");
            assert_eq!(t.callout[0], tip);

            let before = work.doc().clone();
            let mut set = AnnotSet::load(&before);
            set.add(a.clone()).unwrap();
            let (plan, _) = work.save(&set);
            let after = work.doc().clone();
            let loaded = AnnotSet::load(&after);
            let got = loaded.get(&a.id).unwrap();
            assert_eq!(got.detached(), a.detached(), "{label} {name}");

            let dict = dict_of(&after, plan.added[0].object);
            assert_eq!(
                dict.get(b"IT").unwrap(),
                &Object::Name(b"FreeTextCallout".to_vec())
            );
            assert_eq!(
                dict.get(b"LE").unwrap(),
                &Object::Name(b"ClosedArrow".to_vec())
            );
            let cl: Vec<f64> = dict
                .get(b"CL")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|n| pl_pdf::number(n).unwrap())
                .collect();
            assert_eq!(cl.len(), 6, "{label}: three leader points");
            assert_eq!(
                (cl[0], cl[1]),
                (tip.x, tip.y),
                "{label}: the tip comes first"
            );
            assert_eq!(
                (cl[4], cl[5]),
                (rect.x0, rect.center().y),
                "{label}: ends on the box"
            );
            // /Rect holds the leader too, and /RD brings it back to the text box.
            let file_rect = pl_pdf::rect_from_object(dict.get(b"Rect").unwrap()).unwrap();
            assert!(file_rect.x0 <= tip.x && file_rect.y0 <= tip.y);
            let rd: Vec<f64> = dict
                .get(b"RD")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|n| pl_pdf::number(n).unwrap())
                .collect();
            assert_eq!(
                Rect::new(
                    file_rect.x0 + rd[0],
                    file_rect.y0 + rd[1],
                    file_rect.x1 - rd[2],
                    file_rect.y1 - rd[3]
                ),
                rect,
                "{label}"
            );

            let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
            let ours = common::raster_over(&a.draw_list(), &geometry, &r.backdrop);
            let scale = common::scale_for(&geometry);
            // The leader is there: ink at the middle of each of its two segments and at the
            // arrowhead, in PDFium's render.
            let to_px = pl_draw::page_transform(&geometry, scale);
            let inked = |p: Point| {
                let q = to_px * p;
                let (x, y) = (q.x as u32, q.y as u32);
                let (a, b) = (r.pdfium.pixel(x, y), r.backdrop.pixel(x, y));
                (0..3).any(|c| (a[c] as i32 - b[c] as i32).abs() > 40)
            };
            let c = &t.callout;
            let head = c[0] + (c[1] - c[0]) / (c[1] - c[0]).hypot() * 4.0;
            for (what, p) in [
                ("first segment", c[0].midpoint(c[1])),
                ("second segment", c[1].midpoint(c[2])),
                ("arrowhead", head),
            ] {
                if !inked(p) {
                    failures.push(format!("{label} {name}: no ink on the {what}"));
                }
            }
            if name == "bare" {
                let d = common::diff(&r.pdfium, &ours, 1, 40);
                if d.bad > 3 || d.mean > 0.6 {
                    common::dump(&r.pdfium, &format!("callout-{label}-pdfium"));
                    common::dump(&ours, &format!("callout-{label}-ours"));
                    failures.push(format!("{label} bare: {d:?}"));
                }
            } else if let Some(why) =
                common::ink_mismatch(&r.pdfium, &ours, &r.backdrop, scale, 1.0)
            {
                failures.push(format!("{label} worded: {why}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Text that does not fit its box is cut off at the box. For a callout `/Rect` (and so the
/// appearance's `/BBox`) also covers the leader, so the box itself has to do the clipping.
#[test]
fn text_that_overflows_a_callout_is_clipped_to_its_box() {
    let backend = common::backend();
    let mut failures = Vec::new();
    for target in &TARGETS {
        let label = target.name;
        let fixture = (target.fixture)();
        let mut work = Work::new(&fixture);
        let geometry = work.geometry(target.page);
        let o = work.sample_origin(target.page);
        // The tip is straight below the box, so /Rect reaches far below it.
        let rect = Rect::new(o.x + 40.0, o.y + 150.0, o.x + 160.0, o.y + 190.0);
        let tip = Point::new(o.x + 100.0, o.y + 10.0);
        let text = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten";
        let mut a = Annotation::callout(target.page, rect, tip, text, &common::origin());
        // The box is upright in page space here: the overflow runs down towards the tip.
        a.style.width = 1.0;
        a.free_text.as_mut().unwrap().color = Rgb::BLUE;
        a.style.stroke = Some(Rgb::BLACK);
        assert!(!a.text_layout().unwrap().unwrap().overflow.is_none());
        let placement = a.placement();
        assert!(
            placement.rect.y0 <= tip.y,
            "{label}: /Rect covers the leader"
        );

        let before = work.doc().clone();
        let mut set = AnnotSet::load(&before);
        set.add(a.clone()).unwrap();
        work.save(&set);
        let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
        let ours = common::raster_over(&a.draw_list(), &geometry, &r.backdrop);
        let scale = common::scale_for(&geometry);
        let to_px = pl_draw::page_transform(&geometry, scale);
        // Blue ink (the text) outside the box, in either render, is overflow that got out.
        let inside = to_px.transform_rect_bbox(rect.inflate(0.5, 0.5));
        let stray = |image: &pl_render::Bitmap| {
            let mut count = 0;
            for y in 0..image.height {
                for x in 0..image.width {
                    let (p, b) = (image.pixel(x, y), r.backdrop.pixel(x, y));
                    let blue = p[2] as i32 - p[0] as i32 > 80 && p[2] as i32 - b[2] as i32 >= -40;
                    let was_blue = b[2] as i32 - b[0] as i32 > 80;
                    let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                    let out = fx < inside.x0 || fx > inside.x1 || fy < inside.y0 || fy > inside.y1;
                    if blue && !was_blue && out {
                        count += 1;
                    }
                }
            }
            count
        };
        let (theirs, mine) = (stray(&r.pdfium), stray(&ours));
        if theirs > 0 || mine > 0 {
            common::dump(&r.pdfium, &format!("clip-{label}-pdfium"));
            common::dump(&ours, &format!("clip-{label}-ours"));
            failures.push(format!(
                "{label}: text outside the box (PDFium {theirs} px, raster {mine} px)"
            ));
        }
        // The text that does fit is there, in both.
        let shown = |image: &pl_render::Bitmap| {
            let mut count = 0;
            for y in inside.y0.max(0.0) as u32..(inside.y1 as u32).min(image.height) {
                for x in inside.x0.max(0.0) as u32..(inside.x1 as u32).min(image.width) {
                    let p = image.pixel(x, y);
                    if p[2] as i32 - p[0] as i32 > 80 {
                        count += 1;
                    }
                }
            }
            count
        };
        if shown(&r.pdfium) < 20 || shown(&ours) < 20 {
            failures.push(format!("{label}: the text that fits is missing"));
        }
        // The stream says so in the standard way.
        let loaded = AnnotSet::load(work.doc());
        assert_eq!(
            loaded.get(&a.id).unwrap().detached(),
            a.detached(),
            "{label}"
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn text_outside_winansi_is_refused_by_name_and_nothing_is_written() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let rect = Rect::new(100.0, 100.0, 300.0, 160.0);
    for (text, ch, index) in [
        ("Beam \u{3b1} = 30\u{b0}", '\u{3b1}', 5),
        ("\u{4e2d}", '\u{4e2d}', 0),
        ("tab\there", '\t', 3),
        ("line one\nline \u{2192} two", '\u{2192}', 14),
    ] {
        let a = Annotation::free_text(0, rect, text, &common::origin());
        assert_eq!(
            a.validate(),
            Err(DrawError::NotWinAnsi { ch, index }),
            "{text:?}"
        );
        let mut set = AnnotSet::load(&doc);
        let id = set.add(a).unwrap();
        match set.plan_save(&doc) {
            Err(AnnotError::Draw { id: bad, source }) => {
                assert_eq!(bad, id);
                assert_eq!(source, DrawError::NotWinAnsi { ch, index });
                let message = AnnotError::Draw { id: bad, source }.to_string();
                assert!(message.contains("WinAnsi"), "{message}");
            }
            other => panic!("{text:?}: expected a draw error, got {other:?}"),
        }
    }
    // The same for a stamp's label, and for an edit that puts such text into a saved box.
    let stamp = Annotation::stamp(
        0,
        rect,
        pl_annot::StampSpec::new(
            "Custom",
            "\u{41f}\u{420}\u{418}\u{41d}\u{42f}\u{422}\u{41e}",
            Default::default(),
        ),
        &common::origin(),
    );
    assert!(matches!(
        stamp.validate(),
        Err(DrawError::NotWinAnsi { index: 0, .. })
    ));
    let mut set = AnnotSet::load(&doc);
    set.add(stamp).unwrap();
    assert!(matches!(
        set.plan_save(&doc),
        Err(AnnotError::Draw {
            source: DrawError::NotWinAnsi { .. },
            ..
        })
    ));

    let mut set = AnnotSet::load(&doc);
    let id = set
        .add(Annotation::free_text(0, rect, "fine", &common::origin()))
        .unwrap();
    work.save(&set);
    let saved = work.doc().clone();
    let bytes = work.bytes();
    let mut set = AnnotSet::load(&saved);
    set.get_mut(&id).unwrap().meta.contents = Some("now \u{65e5}\u{672c}".into());
    assert!(matches!(
        set.plan_save(&saved),
        Err(AnnotError::Draw {
            source: DrawError::NotWinAnsi {
                ch: '\u{65e5}',
                index: 4
            },
            ..
        })
    ));
    assert_eq!(work.bytes(), bytes, "the file is as it was");
    // Every WinAnsi character is accepted, the ones outside Latin-1 included.
    let mut set = AnnotSet::load(&saved);
    set.get_mut(&id).unwrap().meta.contents = Some(
        "\u{20ac}45 \u{2013} \u{201c}caf\u{e9}\u{201d} \u{2022} \u{b1}5\u{b0} \u{2122}".into(),
    );
    work.save(&set);
    let again = AnnotSet::load(work.doc());
    assert_eq!(
        again.get(&id).unwrap().meta.contents.as_deref(),
        Some("\u{20ac}45 \u{2013} \u{201c}caf\u{e9}\u{201d} \u{2022} \u{b1}5\u{b0} \u{2122}")
    );
}

#[test]
fn editing_our_text_box_rewrites_only_what_changed() {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let rect = Rect::new(100.0, 300.0, 280.0, 360.0);
    let mut set = AnnotSet::load(work.doc());
    let id = set
        .add(Annotation::free_text(
            0,
            rect,
            "First words",
            &common::origin(),
        ))
        .unwrap();
    let other = set
        .add(Annotation::square(
            0,
            Rect::new(300.0, 300.0, 380.0, 360.0),
            &common::origin(),
        ))
        .unwrap();
    work.save(&set);

    // Alignment only: /Q and /DS (and the appearance); the rich text is left alone.
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let object = set.get(&id).unwrap().object_id().unwrap();
    let other_object = set.get(&other).unwrap().object_id().unwrap();
    set.get_mut(&id).unwrap().free_text.as_mut().unwrap().align = HAlign::Right;
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert_eq!(plan.modified[0].keys, ["AP", "DS", "Q"]);
    assert!(plan.added.is_empty() && plan.deleted.is_empty());
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[object]);
    assert_eq!(
        after.object_offset(other_object),
        before.object_offset(other_object)
    );
    let (old, new) = (dict_of(&before, object), dict_of(&after, object));
    assert_eq!(old.get(b"RC").unwrap(), new.get(b"RC").unwrap());
    assert_eq!(new.get(b"Q").unwrap(), &Object::Integer(2));

    // The text: /Contents, /RC and the appearance.
    let before = after;
    let mut set = AnnotSet::load(&before);
    set.get_mut(&id).unwrap().meta.contents = Some("Other words <now>".into());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["AP", "Contents", "RC"]);
    let after = work.doc().clone();
    let new = dict_of(&after, object);
    assert_eq!(text_of(new, "RC"), rich_text("Other words <now>"));
    assert!(text_of(new, "RC").contains("&lt;now&gt;"));

    // A move: /Rect and the appearance (which is in page coordinates).
    let before = after;
    let mut set = AnnotSet::load(&before);
    set.get_mut(&id).unwrap().translate(Vec2::new(12.0, -8.0));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["AP", "Rect"]);
    let loaded = AnnotSet::load(work.doc());
    let got = loaded.get(&id).unwrap();
    assert_eq!(
        got.geometry,
        Geometry::Rect(Rect::new(112.0, 292.0, 292.0, 352.0))
    );
    assert_eq!(got.free_text.as_ref().unwrap().align, HAlign::Right);
    assert!(!loaded.is_dirty());
}

/// The annotations fixture with `/RC` and a vendor key added to its FreeText.
fn foreign_with_rich_text() -> Work {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let set = AnnotSet::load(&doc);
    let object = set
        .get(&AnnotId::from("fixture-freetext-1"))
        .unwrap()
        .object_id()
        .unwrap();
    let mut edits = doc.edit();
    edits
        .patch(object)
        .set(
            "RC",
            Object::string_literal("<body><p><b>Foreign</b> FreeText note</p></body>"),
        )
        .set(
            "DS",
            Object::string_literal("font: 12pt Arial; color:#0000FF"),
        )
        .set("VendorKey", Object::Integer(42));
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    work.original = work.bytes();
    work
}

#[test]
fn a_foreign_free_text_keeps_its_appearance_and_rich_text_until_edited() {
    let backend = common::backend();
    let mut work = foreign_with_rich_text();
    let geometry = work.geometry(0);
    let before = work.doc().clone();
    let id = AnnotId::from("fixture-freetext-1");
    let shown_before = common::render_pdf(&backend, work.bytes(), 0, &geometry).pdfium;

    // As read: not modelled, shown from its own appearance.
    let mut set = AnnotSet::load(&before);
    let object = set.get(&id).unwrap().object_id().unwrap();
    assert!(!set.get(&id).unwrap().is_modelled());
    assert!(set.get(&id).unwrap().free_text.is_none());

    // Made editable: its keys are read as a text box, and it is still clean.
    let a = set.get_mut(&id).unwrap();
    assert!(a.make_editable());
    assert!(a.is_modelled() && !a.is_dirty());
    let t = a.free_text.clone().unwrap();
    assert_eq!(
        (t.size, t.font, t.intent),
        (12.0, FontStyle::Regular, FreeTextIntent::Box)
    );
    assert_eq!(t.color, Rgb::BLUE, "the /DS colour is the text colour");
    assert_eq!(
        a.style.stroke,
        Some(Rgb::BLACK),
        "the /DA colour is the border"
    );
    assert_eq!(a.style.fill, Some(Rgb::WHITE), "/C is the background");
    assert_eq!(a.style.width, 1.0);
    assert_eq!(a.meta.contents.as_deref(), Some("Foreign FreeText note"));
    assert_eq!(
        a.geometry,
        Geometry::Rect(Rect::new(100.0, 250.0, 340.0, 300.0))
    );
    assert!(!set.is_dirty());
    assert!(
        set.plan_save(&before).unwrap().is_empty(),
        "nothing to write"
    );

    // A change of look: our keys and appearance are written, and the other application's
    // rich text (which carries its own size and colour) is written again as plain paragraphs
    // that take their style from the new /DS. /Contents and the vendor key are as they were.
    set.get_mut(&id).unwrap().free_text.as_mut().unwrap().size = 14.0;
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    let keys = &plan.modified[0].keys;
    for key in ["AP", "DA", "DS", "IT", "Q", "PlumblineData"] {
        assert!(keys.iter().any(|k| k == key), "/{key} in {keys:?}");
    }
    assert!(keys.iter().any(|k| k == "RC"), "{keys:?}");
    assert!(!keys.iter().any(|k| k == "Contents"), "{keys:?}");
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[object]);
    let (old, new) = (dict_of(&before, object), dict_of(&after, object));
    assert_eq!(
        text_of(new, "RC"),
        rich_text("Foreign FreeText note"),
        "no inline style is left to contradict /DS"
    );
    assert!(text_of(new, "DS").contains("14pt"));
    for key in [
        "Contents",
        "VendorKey",
        "NM",
        "T",
        "CreationDate",
        "F",
        "P",
        "Subj",
    ] {
        assert_eq!(
            old.get(key.as_bytes()).unwrap(),
            new.get(key.as_bytes()).unwrap(),
            "/{key} is untouched"
        );
    }
    assert_ne!(old.get(b"AP").unwrap(), new.get(b"AP").unwrap());
    assert!(text_of(new, "DA").ends_with("/Helv 14 Tf"));
    // It is ours now: read back as a text box, equal to what was saved.
    let loaded = AnnotSet::load(&after);
    let got = loaded.get(&id).unwrap();
    assert!(got.is_modelled() && !got.is_dirty());
    assert_eq!(got.detached(), set.get(&id).unwrap().detached());
    let shown_after = common::render_pdf(&backend, work.bytes(), 0, &geometry);
    assert!(
        common::changed_pixels(&shown_after.pdfium, &shown_before, 60) > 200,
        "the larger text shows"
    );
    let ours = common::raster_over(&got.draw_list(), &geometry, &shown_after.backdrop);
    // The other three fixture markups are in both PDFium renders; compare inside the box.
    let scale = common::scale_for(&geometry);
    let px = pl_draw::page_transform(&geometry, scale)
        .transform_rect_bbox(got.geometry.bounds().inflate(2.0, 2.0));
    let crop = |image: &pl_render::Bitmap| {
        let mut out = shown_after.backdrop.clone();
        for y in px.y0 as u32..px.y1 as u32 {
            for x in px.x0 as u32..px.x1 as u32 {
                let i = ((y * image.width + x) * 4) as usize;
                out.pixels[i..i + 4].copy_from_slice(&image.pixels[i..i + 4]);
            }
        }
        out
    };
    assert_eq!(
        common::ink_mismatch(
            &crop(&shown_after.pdfium),
            &crop(&ours),
            &shown_after.backdrop,
            scale,
            1.0
        ),
        None
    );

    // A change of text: now the rich text follows it.
    let before = after;
    let mut set = AnnotSet::load(&before);
    set.get_mut(&id).unwrap().meta.contents = Some("Edited here".into());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["AP", "Contents", "RC"]);
    let new = dict_of(work.doc(), object).clone();
    assert_eq!(text_of(&new, "RC"), rich_text("Edited here"));
    assert_eq!(new.get(b"VendorKey").unwrap(), &Object::Integer(42));
}

/// Acrobat writes `/RC` on nearly every note, reply and shape comment and shows the pop-up
/// text from it: a comment edited here must not leave the old rich text behind.
#[test]
fn a_comment_edited_on_a_markup_with_rich_text_takes_the_rich_text_along() {
    let fixture = pl_testkit::annotations();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let mut set = AnnotSet::load(&doc);
    let o = common::origin();
    let note = Annotation::note(0, Rect::new(400.0, 400.0, 418.0, 418.0), "Old note", &o);
    let reply = Annotation::reply(&note, "Old reply", &o);
    let plain = Annotation::note(0, Rect::new(440.0, 400.0, 458.0, 418.0), "No rich text", &o);
    let (note_id, reply_id, plain_id) = (note.id.clone(), reply.id.clone(), plain.id.clone());
    let square_id = AnnotId::from("fixture-square-1");
    set.add(note).unwrap();
    set.add(reply).unwrap();
    set.add(plain).unwrap();
    work.save(&set);

    // Another application's rich text on the note, the reply and the square's comment.
    let doc = work.doc().clone();
    let set = AnnotSet::load(&doc);
    let object = |id: &AnnotId| set.get(id).unwrap().object_id().unwrap();
    let rich = "<body><p><span style=\"color:#FF0000\">Old</span> words</p></body>";
    let mut edits = doc.edit();
    for id in [&note_id, &reply_id, &square_id] {
        edits
            .patch(object(id))
            .set("RC", Object::string_literal(rich));
    }
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    work.original = work.bytes();

    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    assert!(!set.is_dirty());
    // Something other than the text: the rich text is not touched.
    set.get_mut(&note_id).unwrap().meta.author = Some("Someone Else".into());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["T"]);

    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    set.get_mut(&note_id).unwrap().meta.contents = Some("New <note>".into());
    set.get_mut(&reply_id).unwrap().meta.contents = Some("New reply".into());
    set.get_mut(&square_id).unwrap().meta.contents = None;
    set.get_mut(&plain_id).unwrap().meta.contents = Some("Still plain".into());
    let (plan, _) = work.save(&set);
    let keys = |id: &AnnotId| {
        plan.modified
            .iter()
            .find(|c| c.id == *id)
            .unwrap_or_else(|| panic!("{id} was written"))
            .keys
            .clone()
    };
    assert_eq!(keys(&note_id), ["Contents", "RC"]);
    assert_eq!(keys(&reply_id), ["Contents", "RC"]);
    assert_eq!(keys(&square_id), ["Contents", "RC"]);
    assert_eq!(keys(&plain_id), ["Contents"], "no rich text is invented");
    let after = work.doc().clone();
    let dict = |id: &AnnotId| dict_of(&after, object(id));
    assert_eq!(text_of(dict(&note_id), "Contents"), "New <note>");
    assert_eq!(text_of(dict(&note_id), "RC"), rich_text("New <note>"));
    assert!(text_of(dict(&note_id), "RC").contains("New &lt;note&gt;"));
    assert_eq!(text_of(dict(&reply_id), "RC"), rich_text("New reply"));
    assert!(
        !dict(&square_id).has(b"RC") && !dict(&square_id).has(b"Contents"),
        "a cleared comment leaves no rich text saying the old one"
    );
    assert!(!dict(&plain_id).has(b"RC"));
    for id in [&note_id, &reply_id] {
        assert!(!text_of(dict(id), "RC").contains("Old"), "{id}");
    }
    assert!(!AnnotSet::load(&after).is_dirty());
}

#[test]
fn a_foreign_free_text_that_is_only_moved_or_described_keeps_its_own_appearance() {
    let mut work = foreign_with_rich_text();
    let before = work.doc().clone();
    let id = AnnotId::from("fixture-freetext-1");
    let mut set = AnnotSet::load(&before);
    let object = set.get(&id).unwrap().object_id().unwrap();
    // Not made editable: a move changes /Rect and nothing else.
    set.get_mut(&id).unwrap().translate(Vec2::new(10.0, 20.0));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["Rect"]);
    let after = work.doc().clone();
    let (old, new) = (dict_of(&before, object), dict_of(&after, object));
    assert_eq!(old.get(b"AP").unwrap(), new.get(b"AP").unwrap());
    assert_eq!(old.get(b"RC").unwrap(), new.get(b"RC").unwrap());

    // Made editable after the move, it is where it was moved to, and still clean; a new
    // author alone leaves the appearance and every FreeText key alone.
    let mut set = AnnotSet::load(&after);
    let a = set.get_mut(&id).unwrap();
    assert!(a.make_editable());
    assert_eq!(
        a.geometry,
        Geometry::Rect(Rect::new(110.0, 270.0, 350.0, 320.0))
    );
    assert!(!a.is_dirty());
    a.meta.author = Some("Second Author".into());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified[0].keys, ["T"]);
    let last = dict_of(work.doc(), object).clone();
    assert_eq!(last.get(b"AP").unwrap(), new.get(b"AP").unwrap());
    assert!(!last.has(b"PlumblineData"), "still the other application's");
    // A stamp or a square cannot be made editable this way.
    let mut set = AnnotSet::load(work.doc());
    let square = set.get_mut(&AnnotId::from("fixture-square-1")).unwrap();
    assert!(!square.make_editable());
}

#[test]
fn hit_testing_and_handles_of_a_callout() {
    let rect = Rect::new(200.0, 200.0, 320.0, 250.0);
    let tip = Point::new(100.0, 120.0);
    let mut a = Annotation::callout(0, rect, tip, "note", &common::origin());
    assert!(a.hit_test(Point::new(260.0, 225.0), 1.0), "inside the box");
    let leader = a.free_text.as_ref().unwrap().callout.clone();
    assert!(
        a.hit_test(leader[0].midpoint(leader[1]), 1.5),
        "on the leader"
    );
    assert!(!a.hit_test(Point::new(120.0, 240.0), 1.5));
    // Eight box handles, then the tip and the knee.
    let handles = a.handles();
    assert_eq!(handles.len(), 10);
    assert_eq!(handles[8].kind, pl_annot::HandleKind::Vertex(0));
    assert_eq!(handles[8].at, tip);
    // Dragging the tip to the other side re-routes the leader from that side of the box.
    assert!(a.move_handle(pl_annot::HandleKind::Vertex(0), Point::new(420.0, 400.0)));
    let leader = &a.free_text.as_ref().unwrap().callout;
    assert_eq!(leader[0], Point::new(420.0, 400.0));
    assert_eq!(
        *leader.last().unwrap(),
        Point::new(260.0, 250.0),
        "leaves the top now"
    );
    assert!(!a.move_handle(pl_annot::HandleKind::Vertex(7), tip));
    // Moving the whole annotation moves the leader with it.
    a.translate(Vec2::new(5.0, -5.0));
    assert_eq!(
        a.free_text.as_ref().unwrap().callout[0],
        Point::new(425.0, 395.0)
    );
    assert_eq!(
        a.geometry,
        Geometry::Rect(Rect::new(205.0, 195.0, 325.0, 245.0))
    );
}
