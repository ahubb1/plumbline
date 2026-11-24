//! The drawings the integration tests use: one named [`DrawList`] per shape helper, dash
//! pattern, line ending, opacity and Multiply case. Everything stays inside x 30..230,
//! y 20..150 so it is visible on every page variant (see `common::VARIANTS`).
#![allow(dead_code, clippy::vec_init_then_push)]

use std::sync::Arc;

use pl_draw::kurbo::{Affine, Point, Rect};
use pl_draw::*;

pub struct Case {
    pub name: String,
    pub list: DrawList,
}

fn case(name: impl Into<String>, list: DrawList) -> Case {
    Case {
        name: name.into(),
        list,
    }
}

fn one(op: DrawOp) -> DrawList {
    DrawList::from_ops(vec![op])
}

const RED: Rgb = Rgb::new(0.85, 0.1, 0.1);
const BLUE: Rgb = Rgb::new(0.1, 0.25, 0.8);
const GREEN: Rgb = Rgb::new(0.1, 0.6, 0.2);
const ORANGE: Rgb = Rgb::new(1.0, 0.6, 0.1);
const YELLOW: Rgb = Rgb::new(1.0, 0.95, 0.2);

fn star() -> Vec<Point> {
    let c = Point::new(130.0, 85.0);
    (0..5)
        .map(|i| {
            let a = std::f64::consts::FRAC_PI_2 + i as f64 * 4.0 * std::f64::consts::PI / 5.0;
            Point::new(c.x + 60.0 * a.cos(), c.y + 60.0 * a.sin())
        })
        .collect()
}

fn zigzag() -> Vec<Point> {
    vec![
        Point::new(40.0, 40.0),
        Point::new(80.0, 130.0),
        Point::new(120.0, 45.0),
        Point::new(160.0, 130.0),
        Point::new(215.0, 60.0),
    ]
}

fn scribble() -> Vec<Point> {
    (0..40)
        .map(|i| {
            let t = i as f64 / 39.0;
            Point::new(
                40.0 + 180.0 * t,
                85.0 + 45.0 * (t * 9.0).sin() * (0.4 + 0.6 * t),
            )
        })
        .collect()
}

fn blob() -> Vec<Point> {
    vec![
        Point::new(50.0, 40.0),
        Point::new(200.0, 35.0),
        Point::new(215.0, 110.0),
        Point::new(140.0, 90.0),
        Point::new(70.0, 135.0),
    ]
}

/// A small test picture: a 12 x 8 gradient with a transparent corner when `alpha` is set.
pub fn test_image(alpha: bool) -> Arc<RgbaImage> {
    let (w, h) = (12u32, 8u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if alpha && x + y < 7 {
                40 + 20 * (x + y) as u8
            } else {
                255
            };
            px.extend_from_slice(&[(x * 21) as u8, (y * 32) as u8, 200 - (x * 12) as u8, a]);
        }
    }
    Arc::new(RgbaImage::new(w, h, px).unwrap())
}

/// A larger smooth picture (soft colour waves), for drawing reduced.
pub fn smooth_image(w: u32, h: u32) -> Arc<RgbaImage> {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f64 / w as f64, y as f64 / h as f64);
            let wave = |v: f64| (127.5 + 127.5 * (v * std::f64::consts::TAU).sin()) as u8;
            px.extend_from_slice(&[wave(fx * 2.0), wave(fy * 1.5 + 0.2), wave(fx + fy), 255]);
        }
    }
    Arc::new(RgbaImage::new(w, h, px).unwrap())
}

/// Shape helpers, each stroked and filled the plain way.
pub fn shape_cases() -> Vec<Case> {
    let frame = Rect::new(45.0, 35.0, 215.0, 135.0);
    let mut v = Vec::new();

    v.push(case(
        "rect-stroke",
        one(DrawOp::Path(PathOp::stroked(
            rect_path(frame),
            Stroke::new(2.0, RED),
        ))),
    ));
    v.push(case(
        "rect-fill-stroke",
        one(DrawOp::Path(
            PathOp::stroked(
                rect_path(inset_for_stroke(frame, 5.0)),
                Stroke::new(5.0, BLUE),
            )
            .with_fill(Fill::new(ORANGE)),
        )),
    ));
    v.push(case(
        "ellipse-stroke",
        one(DrawOp::Path(PathOp::stroked(
            ellipse_path(frame),
            Stroke::new(3.0, GREEN),
        ))),
    ));
    v.push(case(
        "ellipse-fill-stroke",
        one(DrawOp::Path(
            PathOp::stroked(ellipse_path(frame), Stroke::new(1.0, Rgb::BLACK))
                .with_fill(Fill::new(YELLOW)),
        )),
    ));
    v.push(case(
        "polyline",
        one(DrawOp::Path(PathOp::stroked(
            polyline_path(&zigzag()),
            Stroke::new(3.0, RED),
        ))),
    ));
    v.push(case(
        "polygon-stroke",
        one(DrawOp::Path(PathOp::stroked(
            polygon_path(&blob()),
            Stroke::new(2.0, BLUE),
        ))),
    ));
    v.push(case(
        "polygon-nonzero",
        one(DrawOp::Path(
            PathOp::filled(polygon_path(&star()), Fill::new(GREEN))
                .with_stroke(Stroke::new(1.0, Rgb::BLACK)),
        )),
    ));
    v.push(case(
        "polygon-evenodd",
        one(DrawOp::Path(
            PathOp::filled(polygon_path(&star()), Fill::even_odd(GREEN))
                .with_stroke(Stroke::new(1.0, Rgb::BLACK)),
        )),
    ));
    v.push(case(
        "ink",
        one(DrawOp::Path(PathOp::stroked(
            ink_path(&scribble()),
            Stroke::new(3.0, BLUE)
                .with_cap(LineCap::Round)
                .with_join(LineJoin::Round),
        ))),
    ));
    v.push(case(
        "ink-dot",
        one(DrawOp::Path(PathOp::stroked(
            ink_path(&[Point::new(100.0, 100.0)]),
            Stroke::new(12.0, RED)
                .with_cap(LineCap::Round)
                .with_join(LineJoin::Round),
        ))),
    ));
    v.push(case(
        "cloud-polygon",
        one(DrawOp::Path(PathOp::stroked(
            cloud_path(&blob(), &CloudStyle::new(14.0)),
            Stroke::new(1.5, RED).with_join(LineJoin::Round),
        ))),
    ));
    v.push(case(
        "cloud-rect-filled",
        one(DrawOp::Path(
            PathOp::stroked(
                cloud_rect_path(Rect::new(60.0, 45.0, 200.0, 125.0), &CloudStyle::new(18.0)),
                Stroke::new(2.0, BLUE),
            )
            .with_fill(Fill::new(YELLOW)),
        )),
    ));
    v.push(case(
        "cloud-deep-scallops",
        one(DrawOp::Path(PathOp::stroked(
            cloud_path(
                &blob().into_iter().rev().collect::<Vec<_>>(),
                &CloudStyle {
                    scallop: 22.0,
                    bulge: 0.5,
                },
            ),
            Stroke::new(2.5, GREEN).with_join(LineJoin::Round),
        ))),
    ));
    v.push(case(
        "hairline",
        one(DrawOp::Path(PathOp::stroked(
            polygon_path(&blob()),
            Stroke::new(0.0, Rgb::BLACK),
        ))),
    ));
    v
}

/// Caps, joins and miter limits.
pub fn stroke_style_cases() -> Vec<Case> {
    let mut v = Vec::new();
    for (name, cap) in [
        ("butt", LineCap::Butt),
        ("round", LineCap::Round),
        ("square", LineCap::Square),
    ] {
        v.push(case(
            format!("cap-{name}"),
            one(DrawOp::Path(PathOp::stroked(
                polyline_path(&[Point::new(60.0, 50.0), Point::new(200.0, 120.0)]),
                Stroke::new(14.0, BLUE).with_cap(cap),
            ))),
        ));
    }
    for (name, join) in [
        ("miter", LineJoin::Miter),
        ("round", LineJoin::Round),
        ("bevel", LineJoin::Bevel),
    ] {
        v.push(case(
            format!("join-{name}"),
            one(DrawOp::Path(PathOp::stroked(
                polyline_path(&zigzag()),
                Stroke::new(9.0, GREEN).with_join(join),
            ))),
        ));
    }
    // A 30 degree corner: mitered at the default limit, bevelled at a limit of 2.
    let corner = [
        Point::new(200.0, 60.0),
        Point::new(70.0, 85.0),
        Point::new(200.0, 120.0),
    ];
    for (name, limit) in [("default", Stroke::DEFAULT_MITER_LIMIT), ("low", 2.0)] {
        v.push(case(
            format!("miter-limit-{name}"),
            one(DrawOp::Path(PathOp::stroked(
                polyline_path(&corner),
                Stroke::new(8.0, RED).with_miter_limit(limit),
            ))),
        ));
    }
    v
}

/// Dash patterns on straight and curved outlines.
pub fn dash_cases() -> Vec<Case> {
    let frame = Rect::new(45.0, 35.0, 215.0, 135.0);
    let patterns: [(&str, Dash, LineCap); 7] = [
        ("6-3", Dash::new([6.0, 3.0], 0.0), LineCap::Butt),
        ("12-4-phase5", Dash::new([12.0, 4.0], 5.0), LineCap::Butt),
        (
            "dash-dot",
            Dash::new([10.0, 4.0, 2.0, 4.0], 0.0),
            LineCap::Butt,
        ),
        ("odd-7", Dash::new([7.0], 0.0), LineCap::Butt),
        ("odd-9-3-5", Dash::new([9.0, 3.0, 5.0], 2.0), LineCap::Butt),
        ("dots-round", Dash::dotted(7.0), LineCap::Round),
        ("square-caps", Dash::new([4.0, 9.0], 0.0), LineCap::Square),
    ];
    let mut v = Vec::new();
    for (name, dash, cap) in patterns {
        let stroke = Stroke::new(3.0, RED).with_dash(dash).with_cap(cap);
        v.push(case(
            format!("dash-{name}-rect"),
            one(DrawOp::Path(PathOp::stroked(
                rect_path(frame),
                stroke.clone(),
            ))),
        ));
        v.push(case(
            format!("dash-{name}-ellipse"),
            one(DrawOp::Path(PathOp::stroked(
                ellipse_path(frame),
                stroke.clone(),
            ))),
        ));
        v.push(case(
            format!("dash-{name}-polyline"),
            one(DrawOp::Path(PathOp::stroked(
                polyline_path(&zigzag()),
                stroke.clone(),
            ))),
        ));
    }
    v.push(case(
        "dash-polygon-filled",
        one(DrawOp::Path(
            PathOp::stroked(
                polygon_path(&blob()),
                Stroke::new(2.0, Rgb::BLACK).with_dash(Dash::new([8.0, 4.0], 0.0)),
            )
            .with_fill(Fill::new(YELLOW)),
        )),
    ));
    v.push(case(
        "dash-ink",
        one(DrawOp::Path(PathOp::stroked(
            ink_path(&scribble()),
            Stroke::new(2.0, GREEN).with_dash(Dash::even(5.0)),
        ))),
    ));
    v
}

/// Every `/LE` ending, at both ends, thin and thick, with and without an interior colour, on a
/// plain and a dashed line, and at the ends of a polyline.
pub fn ending_cases() -> Vec<Case> {
    let a = Point::new(55.0, 45.0);
    let b = Point::new(205.0, 120.0);
    let mut v = Vec::new();
    for ending in LineEnding::ALL {
        let name = ending.pdf_name();
        for (wname, width) in [("thin", 1.0), ("thick", 4.0)] {
            v.push(case(
                format!("le-{name}-{wname}"),
                DrawList::from_ops(line_ops(
                    a,
                    b,
                    ending,
                    ending,
                    &Stroke::new(width, RED),
                    Some(YELLOW),
                )),
            ));
        }
        v.push(case(
            format!("le-{name}-hollow"),
            DrawList::from_ops(line_ops(
                a,
                b,
                ending,
                ending,
                &Stroke::new(2.0, BLUE),
                None,
            )),
        ));
    }
    // Different endings at each end, on axis-aligned and steep lines.
    let pairs = [
        (LineEnding::OpenArrow, LineEnding::Circle),
        (LineEnding::ClosedArrow, LineEnding::Butt),
        (LineEnding::Square, LineEnding::Diamond),
        (LineEnding::Slash, LineEnding::ClosedArrow),
    ];
    for (i, (s, e)) in pairs.into_iter().enumerate() {
        let mut ops = line_ops(
            Point::new(50.0, 60.0),
            Point::new(210.0, 60.0),
            s,
            e,
            &Stroke::new(2.0, RED),
            Some(GREEN),
        );
        ops.extend(line_ops(
            Point::new(90.0 + 20.0 * i as f64, 140.0),
            Point::new(110.0 + 20.0 * i as f64, 75.0),
            s,
            e,
            &Stroke::new(3.0, BLUE),
            Some(ORANGE),
        ));
        v.push(case(
            format!("le-mixed-{}-{}", s.pdf_name(), e.pdf_name()),
            DrawList::from_ops(ops),
        ));
    }
    v.push(case(
        "le-dashed-line",
        DrawList::from_ops(line_ops(
            a,
            b,
            LineEnding::ClosedArrow,
            LineEnding::Circle,
            &Stroke::new(2.5, RED).with_dash(Dash::new([9.0, 4.0], 0.0)),
            Some(YELLOW),
        )),
    ));
    v.push(case(
        "le-polyline",
        DrawList::from_ops(polyline_ops(
            &zigzag(),
            LineEnding::Diamond,
            LineEnding::OpenArrow,
            &Stroke::new(2.0, GREEN),
            Some(YELLOW),
        )),
    ));
    v
}

/// Translucent ops and groups, all with Normal blending.
pub fn opacity_cases() -> Vec<Case> {
    let frame = Rect::new(45.0, 35.0, 215.0, 135.0);
    let mut v = Vec::new();
    for (name, opacity) in [("25", 0.25), ("50", 0.5), ("80", 0.8)] {
        v.push(case(
            format!("opacity-fill-{name}"),
            one(DrawOp::Path(
                PathOp::filled(rect_path(frame), Fill::new(RED)).with_opacity(opacity),
            )),
        ));
        v.push(case(
            format!("opacity-stroke-{name}"),
            one(DrawOp::Path(
                PathOp::stroked(ellipse_path(frame), Stroke::new(12.0, BLUE)).with_opacity(opacity),
            )),
        ));
    }
    // Fill and stroke with op opacity: the stroke shows through over its own fill.
    v.push(case(
        "opacity-op-fill-and-stroke",
        one(DrawOp::Path(
            PathOp::stroked(ellipse_path(frame), Stroke::new(14.0, BLUE))
                .with_fill(Fill::new(ORANGE))
                .with_opacity(0.5),
        )),
    ));
    // The same shape as one translucent object.
    let mut shape = DrawList::new();
    shape.push_shape(
        ellipse_path(frame),
        &ShapeStyle::stroked(Stroke::new(14.0, BLUE))
            .with_fill(Fill::new(ORANGE))
            .with_opacity(0.5),
    );
    v.push(case("opacity-shape-group", shape));
    // A translucent arrow: the head must not show the line through it.
    let mut arrow = DrawList::new();
    arrow.push_composite(
        line_ops(
            Point::new(55.0, 45.0),
            Point::new(205.0, 120.0),
            LineEnding::Circle,
            LineEnding::ClosedArrow,
            &Stroke::new(6.0, RED),
            Some(RED),
        ),
        0.45,
        BlendMode::Normal,
    );
    v.push(case("opacity-arrow-group", arrow));
    // A translucent group inside a translucent group.
    let inner = Group::new(vec![
        DrawOp::Path(PathOp::filled(
            rect_path(Rect::new(60.0, 50.0, 150.0, 120.0)),
            Fill::new(GREEN),
        )),
        DrawOp::Path(PathOp::filled(
            rect_path(Rect::new(110.0, 40.0, 200.0, 100.0)),
            Fill::new(BLUE),
        )),
    ])
    .with_opacity(0.6);
    let outer = Group::new(vec![
        DrawOp::Path(PathOp::filled(ellipse_path(frame), Fill::new(YELLOW))),
        DrawOp::Group(inner),
    ])
    .with_opacity(0.7);
    v.push(case("opacity-nested-groups", one(DrawOp::Group(outer))));
    v
}

/// Multiply ops and groups over the backdrop.
pub fn multiply_cases() -> Vec<Case> {
    let quads = [
        rect_quad(Rect::new(40.0, 46.0, 222.0, 62.0)),
        rect_quad(Rect::new(40.0, 68.0, 180.0, 84.0)),
        // Overlaps the previous quad: must not darken twice.
        rect_quad(Rect::new(150.0, 76.0, 222.0, 106.0)),
    ];
    let mut v = Vec::new();
    v.push(case(
        "multiply-highlight",
        one(highlight_op(&quads, YELLOW, 1.0)),
    ));
    v.push(case(
        "multiply-highlight-60",
        one(highlight_op(&quads, YELLOW, 0.6)),
    ));
    v.push(case(
        "multiply-highlight-dark",
        one(highlight_op(&quads, Rgb::new(0.2, 0.7, 0.9), 1.0)),
    ));
    // A slanted quad, as on rotated text.
    v.push(case(
        "multiply-highlight-slanted",
        one(highlight_op(
            &[[
                Point::new(50.0, 40.0),
                Point::new(210.0, 100.0),
                Point::new(203.0, 119.0),
                Point::new(43.0, 59.0),
            ]],
            ORANGE,
            1.0,
        )),
    ));
    // A marker pen: a thick Multiply stroke.
    v.push(case(
        "multiply-marker-stroke",
        one(DrawOp::Path(
            PathOp::stroked(
                ink_path(&scribble()),
                Stroke::new(14.0, GREEN)
                    .with_cap(LineCap::Round)
                    .with_join(LineJoin::Round),
            )
            .with_blend(BlendMode::Multiply),
        )),
    ));
    v.push(case(
        "multiply-marker-stroke-50",
        one(DrawOp::Path(
            PathOp::stroked(
                polyline_path(&zigzag()),
                Stroke::new(16.0, RED).with_join(LineJoin::Round),
            )
            .with_opacity(0.5)
            .with_blend(BlendMode::Multiply),
        )),
    ));
    // Fill and stroke multiplied separately (the stroke also multiplies with its own fill).
    v.push(case(
        "multiply-op-fill-and-stroke",
        one(DrawOp::Path(
            PathOp::stroked(
                rect_path(Rect::new(50.0, 40.0, 210.0, 130.0)),
                Stroke::new(12.0, Rgb::new(0.5, 0.8, 1.0)),
            )
            .with_fill(Fill::new(YELLOW))
            .with_blend(BlendMode::Multiply),
        )),
    ));
    // A filled and stroked shape multiplied as one object.
    for (name, opacity) in [("", 1.0), ("-50", 0.5)] {
        let mut shape = DrawList::new();
        shape.push_shape(
            ellipse_path(Rect::new(45.0, 35.0, 215.0, 135.0)),
            &ShapeStyle::stroked(Stroke::new(10.0, Rgb::new(0.3, 0.6, 1.0)))
                .with_fill(Fill::new(YELLOW))
                .with_opacity(opacity)
                .with_blend(BlendMode::Multiply),
        );
        v.push(case(format!("multiply-shape-group{name}"), shape));
    }
    // A Multiply mark beside and over Normal marks in one list.
    let mut mixed = DrawList::new();
    mixed.push(DrawOp::Path(PathOp::filled(
        rect_path(Rect::new(60.0, 100.0, 140.0, 140.0)),
        Fill::new(Rgb::new(0.9, 0.9, 0.9)),
    )));
    mixed.push(highlight_op(
        &[rect_quad(Rect::new(40.0, 90.0, 220.0, 125.0))],
        Rgb::new(1.0, 0.5, 0.7),
        1.0,
    ));
    mixed.push(DrawOp::Path(PathOp::stroked(
        rect_path(Rect::new(100.0, 30.0, 170.0, 110.0)),
        Stroke::new(3.0, BLUE),
    )));
    v.push(case("multiply-mixed-with-normal", mixed));
    v
}

/// Group transforms and images.
pub fn group_and_image_cases() -> Vec<Case> {
    let mut v = Vec::new();
    let bar = vec![
        DrawOp::Path(
            PathOp::stroked(
                rect_path(Rect::new(-60.0, -20.0, 60.0, 20.0)),
                Stroke::new(3.0, RED),
            )
            .with_fill(Fill::new(YELLOW)),
        ),
        DrawOp::Path(PathOp::stroked(
            polyline_path(&[Point::new(-50.0, 0.0), Point::new(50.0, 0.0)]),
            Stroke::new(2.0, BLUE).with_dash(Dash::new([6.0, 3.0], 0.0)),
        )),
    ];
    let turn = Affine::translate((130.0, 85.0)) * Affine::rotate(0.6);
    v.push(case(
        "group-rotated",
        one(DrawOp::Group(Group::new(bar.clone()).with_transform(turn))),
    ));
    v.push(case(
        "group-scaled-skewed",
        one(DrawOp::Group(Group::new(bar.clone()).with_transform(
            Affine::translate((130.0, 85.0)) * Affine::new([1.3, 0.2, -0.4, 1.6, 0.0, 0.0]),
        ))),
    ));
    v.push(case(
        "group-rotated-translucent",
        one(DrawOp::Group(
            Group::new(bar.clone())
                .with_transform(turn)
                .with_opacity(0.5),
        )),
    ));
    v.push(case(
        "group-rotated-multiply",
        one(DrawOp::Group(
            Group::new(bar.clone())
                .with_transform(turn)
                .with_blend(BlendMode::Multiply),
        )),
    ));
    // A group inside a group: transforms multiply.
    v.push(case(
        "group-nested-transforms",
        one(DrawOp::Group(
            Group::new(vec![DrawOp::Group(
                Group::new(bar).with_transform(Affine::rotate(-0.9) * Affine::scale(0.6)),
            )])
            .with_transform(Affine::translate((120.0, 90.0))),
        )),
    ));

    let rect = Rect::new(60.0, 45.0, 180.0, 125.0);
    v.push(case(
        "image-opaque",
        one(DrawOp::Image(ImageOp::new(test_image(false), rect))),
    ));
    v.push(case(
        "image-alpha",
        one(DrawOp::Image(ImageOp::new(test_image(true), rect))),
    ));
    let mut faded = ImageOp::new(test_image(true), rect);
    faded.opacity = 0.5;
    v.push(case("image-translucent", one(DrawOp::Image(faded))));
    let mut multiplied = ImageOp::new(test_image(false), rect);
    multiplied.blend = BlendMode::Multiply;
    v.push(case("image-multiply", one(DrawOp::Image(multiplied))));
    // Reduced: 4 image pixels per page unit, so 1.33 per device pixel at the test scale.
    v.push(case(
        "image-reduced",
        one(DrawOp::Image(ImageOp::new(smooth_image(480, 320), rect))),
    ));
    v.push(case(
        "image-rotated",
        one(DrawOp::Group(
            Group::new(vec![DrawOp::Image(ImageOp::new(
                test_image(false),
                Rect::new(-50.0, -30.0, 50.0, 30.0),
            ))])
            .with_transform(Affine::translate((130.0, 85.0)) * Affine::rotate(0.5)),
        )),
    ));
    v
}

/// Every case that is compared pixel for pixel with PDFium.
pub fn all_geometry_cases() -> Vec<Case> {
    let mut v = shape_cases();
    v.extend(stroke_style_cases());
    v.extend(dash_cases());
    v.extend(ending_cases());
    v.extend(opacity_cases());
    v.extend(multiply_cases());
    v.extend(group_and_image_cases());
    v
}

/// Text runs. PDFium draws these with its own substitute for Helvetica, so they are compared
/// by position and extent rather than pixel for pixel.
pub fn text_cases() -> Vec<Case> {
    let mut v = Vec::new();
    for style in FontStyle::ALL {
        v.push(case(
            format!("text-{style:?}"),
            one(DrawOp::Text(
                TextRun::new(
                    "Wall type A2 - 140 mm",
                    Point::new(45.0, 95.0),
                    16.0,
                    style,
                    Rgb::BLACK,
                )
                .unwrap(),
            )),
        ));
    }
    v.push(case(
        "text-winansi-high",
        one(DrawOp::Text(
            TextRun::new(
                "\u{20AC}45 \u{2013} caf\u{e9} \u{2022} \u{b1}5\u{b0}",
                Point::new(45.0, 95.0),
                18.0,
                FontStyle::Regular,
                RED,
            )
            .unwrap(),
        )),
    ));
    v.push(case(
        "text-translucent",
        one(DrawOp::Text(
            TextRun::new(
                "REVISED",
                Point::new(50.0, 90.0),
                30.0,
                FontStyle::Bold,
                BLUE,
            )
            .unwrap()
            .with_opacity(0.5),
        )),
    ));
    v.push(case(
        "text-rotated",
        one(DrawOp::Group(
            Group::new(vec![DrawOp::Text(
                TextRun::new(
                    "North elevation",
                    Point::new(0.0, 0.0),
                    14.0,
                    FontStyle::Regular,
                    Rgb::BLACK,
                )
                .unwrap(),
            )])
            .with_transform(Affine::translate((90.0, 35.0)) * Affine::rotate(0.7)),
        )),
    ));
    // A paragraph wrapped into a box by the layout, centred both ways.
    let mut style = TextBoxStyle::new(11.0);
    style.align = HAlign::Centre;
    style.v_align = VAlign::Middle;
    style.padding = 4.0;
    style.color = BLUE;
    let layout = layout_text(
        "Wrap this note inside its box.
Second paragraph, centred.",
        Rect::new(70.0, 30.0, 190.0, 140.0),
        &style,
    )
    .unwrap();
    assert!(layout.lines.len() >= 3 && layout.overflow.is_none());
    v.push(case(
        "text-layout-wrapped",
        DrawList::from_ops(layout.ops()),
    ));
    // The stamp artwork, with and without a second line.
    v.push(case(
        "text-stamp-approved",
        stamp_list(&StampSpec::approved(), Rect::new(50.0, 60.0, 210.0, 110.0)).unwrap(),
    ));
    v.push(case(
        "text-stamp-revised-detail",
        stamp_list(
            &StampSpec::revised().with_detail("A. Reviewer  2026-11-24"),
            Rect::new(45.0, 45.0, 215.0, 125.0),
        )
        .unwrap(),
    ));
    v
}

/// A dashed cloud border, and the same border solid.
///
/// This is compared differently from the other dash cases: along a run of forty small arcs
/// PDFium's dashes drift out of step with the true arc length (it measures the flattened
/// curve), so the dashes end up at different places on the same outline. Position along the
/// outline is renderer-dependent; the outline itself and the amount of ink are not.
pub fn dashed_cloud_cases() -> (Case, Case) {
    let path = cloud_path(&blob(), &CloudStyle::new(16.0));
    let solid = Stroke::new(2.0, BLUE).with_join(LineJoin::Round);
    let dashed = solid.clone().with_dash(Dash::new([5.0, 3.0], 0.0));
    (
        case(
            "dash-cloud",
            one(DrawOp::Path(PathOp::stroked(path.clone(), dashed))),
        ),
        case(
            "dash-cloud-solid",
            one(DrawOp::Path(PathOp::stroked(path, solid))),
        ),
    )
}
