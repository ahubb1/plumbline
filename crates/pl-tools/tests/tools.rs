//! Scripted input sequences for every drawing tool, with exact golden geometry, on all eight
//! page layouts. Scripts and expectations are in screen pixels (see `common`), so a Shift
//! constraint is checked against the axes the user sees, whatever the page's rotation.

mod common;

use common::*;
use pl_annot::{AnnotKind, BlendMode, Geometry, LineEnding, Rgb, Style};
use pl_geom::Point;
use pl_tools::{step, Intent, Key, Pointer, ToolInput, ToolKind, ToolPreset, ToolState};

fn box_tools() -> [(ToolKind, AnnotKind, &'static str); 2] {
    [
        (ToolKind::Rectangle, AnnotKind::Square, "Rectangle"),
        (ToolKind::Ellipse, AnnotKind::Circle, "Ellipse"),
    ]
}

#[test]
fn rectangle_and_ellipse_drag() {
    for l in layouts() {
        for (tool, kind, subject) in box_tools() {
            let mut rig = Rig::new(&l, tool);
            let intents = rig.drag((100.0, 100.0), (220.0, 180.0), NONE);
            assert_eq!(intents.len(), 2, "{}: create and select", l.name);
            let a = created(&intents);
            assert_eq!(a.kind, kind, "{}", l.name);
            assert_eq!(a.page, PAGE);
            assert_eq!(
                a.geometry,
                Geometry::Rect(l.rect(100.0, 100.0, 220.0, 180.0)),
                "{} {tool:?}",
                l.name
            );
            assert_eq!(a.style, Style::default_for(&kind));
            assert_eq!(a.meta.subject.as_deref(), Some(subject));
            assert_eq!(a.meta.author.as_deref(), Some("Plumbline Tester"));
            assert_eq!(a.meta.created, Some(origin().when));
            assert!(a.baseline().is_none() && a.is_modelled());
            assert_eq!(rig.selection(), std::slice::from_ref(&a.id));
            assert_eq!(rig.on_page().len(), 1);
            assert!(!rig.state.is_busy());
            assert!(rig.preview.is_empty(), "no preview once the shape exists");

            // Dragged up and to the left on screen: the same normalised box.
            let b = created(&rig.drag((220.0, 180.0), (100.0, 100.0), NONE));
            assert_eq!(b.geometry, a.geometry, "{}", l.name);
            assert_ne!(b.id, a.id);
        }
    }
}

#[test]
fn shift_makes_a_square_on_screen_and_ctrl_draws_from_the_centre() {
    for l in layouts() {
        for (tool, _, _) in box_tools() {
            let mut rig = Rig::new(&l, tool);
            let rect = |r: &mut Rig, from, to, m| created(&r.drag(from, to, m)).geometry;
            let want = |x0, y0, x1, y1| Geometry::Rect(l.rect(x0, y0, x1, y1));

            // The longer screen offset (120 across) sets the side.
            assert_eq!(
                rect(&mut rig, (100.0, 100.0), (220.0, 150.0), SHIFT),
                want(100.0, 100.0, 220.0, 220.0),
                "{}",
                l.name
            );
            // Up and to the left: 80 up is the longer offset.
            assert_eq!(
                rect(&mut rig, (300.0, 300.0), (250.0, 220.0), SHIFT),
                want(220.0, 220.0, 300.0, 300.0),
                "{}",
                l.name
            );
            // A drag straight along the screen's horizontal grows down the screen.
            assert_eq!(
                rect(&mut rig, (100.0, 100.0), (160.0, 100.0), SHIFT),
                want(100.0, 100.0, 160.0, 160.0),
                "{}",
                l.name
            );
            // And one straight down the screen grows to the screen's right.
            assert_eq!(
                rect(&mut rig, (100.0, 100.0), (100.0, 160.0), SHIFT),
                want(100.0, 100.0, 160.0, 160.0),
                "{}",
                l.name
            );
            // From the centre.
            assert_eq!(
                rect(&mut rig, (200.0, 200.0), (240.0, 230.0), CTRL),
                want(160.0, 170.0, 240.0, 230.0),
                "{}",
                l.name
            );
            // Both: a square about the centre.
            let both = pl_tools::Modifiers {
                shift: true,
                ctrl: true,
                alt: false,
            };
            assert_eq!(
                rect(&mut rig, (200.0, 200.0), (240.0, 230.0), both),
                want(160.0, 160.0, 240.0, 240.0),
                "{}",
                l.name
            );
            // The squares really are square in page units too.
            for a in rig.on_page().iter().take(4) {
                let Geometry::Rect(r) = a.geometry else {
                    panic!()
                };
                assert_eq!(r.width(), r.height(), "{}", l.name);
            }
        }
    }
}

#[test]
fn tiny_flat_and_cancelled_drags_draw_nothing() {
    for l in layouts() {
        for tool in [
            ToolKind::Rectangle,
            ToolKind::Ellipse,
            ToolKind::Line,
            ToolKind::Arrow,
            ToolKind::Highlighter,
        ] {
            let mut rig = Rig::new(&l, tool);
            // Under three pixels.
            assert_eq!(rig.drag((100.0, 100.0), (102.0, 101.0), NONE), []);
            // A plain click.
            assert_eq!(rig.click(300.0, 300.0, NONE), []);
            // Out and back again (for the highlighter that is a stroke, so it is left out).
            if tool != ToolKind::Highlighter {
                rig.down(100.0, 100.0, NONE);
                rig.mv(200.0, 200.0, NONE);
                assert!(rig.state.is_busy());
                assert!(!rig.preview.is_empty(), "{} {tool:?}", l.name);
                assert_eq!(rig.up(101.0, 101.0, NONE), []);
            }
            // Esc in the middle; the release that follows does nothing.
            rig.down(100.0, 100.0, NONE);
            rig.mv(200.0, 200.0, NONE);
            assert_eq!(rig.key(Key::Escape, NONE), []);
            assert!(!rig.state.is_busy());
            assert!(rig.preview.is_empty());
            assert_eq!(rig.up(200.0, 200.0, NONE), []);
            // A release with no press.
            assert_eq!(rig.up(250.0, 250.0, NONE), []);
            assert!(rig.view.annotations.is_empty(), "{} {tool:?}", l.name);
            assert!(rig.selection().is_empty());
        }
        // A rectangle with no height is not a rectangle.
        for (tool, _, _) in box_tools() {
            let mut rig = Rig::new(&l, tool);
            assert_eq!(rig.drag((100.0, 100.0), (200.0, 100.0), NONE), []);
            assert_eq!(rig.drag((100.0, 100.0), (100.0, 200.0), NONE), []);
        }
    }
}

#[test]
fn line_and_arrow_drag_with_shift_steps() {
    for l in layouts() {
        for (tool, end, subject) in [
            (ToolKind::Line, LineEnding::None, "Line"),
            (ToolKind::Arrow, LineEnding::OpenArrow, "Arrow"),
        ] {
            let mut rig = Rig::new(&l, tool);
            let a = created(&rig.drag((100.0, 100.0), (220.0, 180.0), NONE));
            assert_eq!(a.kind, AnnotKind::Line);
            assert_eq!(
                a.geometry,
                line(l.at(100.0, 100.0), l.at(220.0, 180.0)),
                "{}",
                l.name
            );
            assert_eq!(a.style.line_start, LineEnding::None);
            assert_eq!(a.style.line_end, end);
            assert_eq!(a.meta.subject.as_deref(), Some(subject));
            // The start stays the start when dragged right to left.
            let b = created(&rig.drag((220.0, 180.0), (100.0, 100.0), NONE));
            assert_eq!(b.geometry, line(l.at(220.0, 180.0), l.at(100.0, 100.0)));

            let mut end_of = |to: (f64, f64)| {
                let a = created(&rig.drag((100.0, 100.0), to, SHIFT));
                let Geometry::Line { a: start, b: end } = a.geometry else {
                    panic!()
                };
                assert_eq!(start, l.at(100.0, 100.0));
                end
            };
            // Each expectation is a screen position: the constraint follows the screen.
            let cases = [
                ((180.0, 110.0), (180.0, 100.0)), // nearly level
                ((20.0, 95.0), (20.0, 100.0)),
                ((95.0, 30.0), (100.0, 30.0)), // nearly upright
                ((104.0, 190.0), (100.0, 190.0)),
                ((160.0, 140.0), (150.0, 150.0)), // diagonals
                ((40.0, 140.0), (50.0, 150.0)),
                ((160.0, 60.0), (150.0, 50.0)),
                ((60.0, 40.0), (50.0, 50.0)),
            ];
            for (to, want) in cases {
                assert_eq!(end_of(to), l.at(want.0, want.1), "{} to {to:?}", l.name);
            }
        }
    }
}

#[test]
fn a_snap_point_replaces_the_pointer_unless_alt_is_held() {
    for l in layouts() {
        let run = |alt: bool, shift: bool| {
            let mut rig = Rig::new(&l, ToolKind::Line);
            let m = pl_tools::Modifiers {
                shift,
                ctrl: false,
                alt,
            };
            let p = |x, y, sx, sy| {
                Pointer::new(PAGE, l.at(x, y))
                    .with_modifiers(m)
                    .with_snap(l.at(sx, sy))
            };
            rig.send(ToolInput::PointerDown(p(100.0, 100.0, 96.0, 96.0)));
            rig.send(ToolInput::PointerMove(p(150.0, 150.0, 151.0, 149.0)));
            let done = rig.send(ToolInput::PointerUp(p(200.0, 206.0, 216.0, 100.0)));
            created(&done).geometry
        };
        assert_eq!(
            run(false, false),
            line(l.at(96.0, 96.0), l.at(216.0, 100.0)),
            "{}",
            l.name
        );
        assert_eq!(
            run(true, false),
            line(l.at(100.0, 100.0), l.at(200.0, 206.0)),
            "{}",
            l.name
        );
        // Shift constrains from the snapped start towards the snapped end.
        assert_eq!(
            run(false, true),
            line(l.at(96.0, 96.0), l.at(216.0, 96.0)),
            "{}",
            l.name
        );
    }
}

fn path_tools() -> [(ToolKind, AnnotKind); 3] {
    [
        (ToolKind::Polyline, AnnotKind::PolyLine),
        (ToolKind::Polygon, AnnotKind::Polygon),
        (ToolKind::Cloud, AnnotKind::Polygon),
    ]
}

fn points(l: &Layout, screen: &[(f64, f64)]) -> Geometry {
    Geometry::Points(screen.iter().map(|(x, y)| l.at(*x, *y)).collect())
}

const SQUARE: [(f64, f64); 4] = [
    (100.0, 100.0),
    (200.0, 100.0),
    (200.0, 200.0),
    (100.0, 200.0),
];

fn click_all(rig: &mut Rig, pts: &[(f64, f64)]) {
    for (x, y) in pts {
        assert_eq!(rig.click(*x, *y, NONE), [], "a point alone creates nothing");
        assert!(rig.state.is_busy());
    }
}

#[test]
fn multi_click_shapes_finish_by_double_click_in_every_event_order() {
    for l in layouts() {
        for (tool, kind) in path_tools() {
            // The double-click arrives after the second click's own press and release.
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE[..3]);
            rig.click(100.0, 200.0, NONE);
            rig.click(101.0, 201.0, NONE);
            let a = created(&rig.double(101.0, 201.0));
            assert_eq!(a.kind, kind);
            assert_eq!(a.geometry, points(&l, &SQUARE), "{} {tool:?}", l.name);
            assert_eq!(a.style, ToolPreset::default_for(tool).style);
            assert_eq!(a.meta.subject.as_deref(), Some(tool.label()));
            assert_eq!(rig.selection(), std::slice::from_ref(&a.id));
            assert!(!rig.state.is_busy() && rig.preview.is_empty());

            // The double-click arrives between the second press and its release.
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE);
            rig.down(100.0, 200.0, NONE);
            let a = created(&rig.double(100.0, 200.0));
            assert_eq!(a.geometry, points(&l, &SQUARE), "{} {tool:?}", l.name);
            assert_eq!(rig.up(100.0, 200.0, NONE), []);
            assert_eq!(rig.on_page().len(), 1);

            // The double-click arrives in place of the last click.
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE[..3]);
            let a = created(&rig.double(100.0, 200.0));
            assert_eq!(a.geometry, points(&l, &SQUARE), "{} {tool:?}", l.name);
        }
    }
}

#[test]
fn enter_finishes_backspace_steps_back_and_esc_cancels() {
    for l in layouts() {
        for (tool, _) in path_tools() {
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE);
            rig.mv(150.0, 300.0, NONE);
            let a = created(&rig.key(Key::Enter, NONE));
            assert_eq!(
                a.geometry,
                points(&l, &SQUARE),
                "{}: the pointer's resting place is not a point",
                l.name
            );

            // Backspace removes the last point; drawing goes on from the one before.
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE);
            assert_eq!(rig.key(Key::Backspace, NONE), []);
            rig.click(120.0, 260.0, NONE);
            let a = created(&rig.key(Key::Enter, NONE));
            assert_eq!(
                a.geometry,
                points(
                    &l,
                    &[
                        (100.0, 100.0),
                        (200.0, 100.0),
                        (200.0, 200.0),
                        (120.0, 260.0)
                    ]
                ),
                "{}",
                l.name
            );
            // Backspace past the first point ends the gesture.
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE[..2]);
            rig.key(Key::Backspace, NONE);
            assert!(rig.state.is_busy());
            rig.key(Key::Backspace, NONE);
            assert!(!rig.state.is_busy());
            assert_eq!(rig.key(Key::Enter, NONE), []);

            // Esc throws the whole shape away.
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE);
            rig.mv(150.0, 300.0, NONE);
            assert!(!rig.preview.is_empty());
            assert_eq!(rig.key(Key::Escape, NONE), []);
            assert!(!rig.state.is_busy() && rig.preview.is_empty());
            assert_eq!(rig.key(Key::Enter, NONE), []);
            assert_eq!(rig.double(300.0, 300.0), []);
            assert!(rig.view.annotations.is_empty());
        }
    }
}

#[test]
fn too_few_points_make_nothing() {
    for l in layouts() {
        for (tool, _) in path_tools() {
            let need = if tool == ToolKind::Polyline { 2 } else { 3 };
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE[..need - 1]);
            assert_eq!(rig.key(Key::Enter, NONE), [], "{} {tool:?}", l.name);
            assert!(!rig.state.is_busy());
            // A double-click on bare paper: one point, nothing made.
            assert_eq!(rig.click(300.0, 300.0, NONE), []);
            assert_eq!(rig.click(300.0, 300.0, NONE), []);
            assert_eq!(rig.double(300.0, 300.0), []);
            assert!(!rig.state.is_busy());
            assert!(rig.view.annotations.is_empty());
            // Exactly enough.
            click_all(&mut rig, &SQUARE[..need]);
            let a = created(&rig.key(Key::Enter, NONE));
            assert_eq!(a.geometry, points(&l, &SQUARE[..need]));
        }
    }
}

#[test]
fn clicking_the_first_point_closes_a_polygon_or_cloud() {
    for l in layouts() {
        for tool in [ToolKind::Polygon, ToolKind::Cloud] {
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE[..2]);
            // With two points there is nothing to close yet: a click on the first is ignored.
            assert_eq!(rig.click(102.0, 101.0, NONE), []);
            click_all(&mut rig, &SQUARE[2..]);
            // The pointer near the first point shows the closed shape.
            rig.mv(103.0, 102.0, NONE);
            assert!(!rig.preview.is_empty());
            let intents = rig.down(103.0, 102.0, NONE);
            let a = created(&intents);
            assert_eq!(intents.len(), 2);
            assert_eq!(a.kind, AnnotKind::Polygon);
            assert_eq!(a.geometry, points(&l, &SQUARE), "{} {tool:?}", l.name);
            assert_eq!(
                a.style.cloud,
                (tool == ToolKind::Cloud).then_some(1.0),
                "a cloud is a polygon with a cloud border"
            );
            assert!(!rig.state.is_busy());
            assert_eq!(rig.up(103.0, 102.0, NONE), []);
        }
        // A polyline does not close: a click on its first point is one more point.
        let mut rig = Rig::new(&l, ToolKind::Polyline);
        click_all(&mut rig, &SQUARE);
        assert_eq!(rig.click(100.0, 100.0, NONE), []);
        let a = created(&rig.key(Key::Enter, NONE));
        let mut want = SQUARE.to_vec();
        want.push((100.0, 100.0));
        assert_eq!(a.geometry, points(&l, &want), "{}", l.name);
    }
}

#[test]
fn shift_steps_each_segment_from_the_point_before() {
    for l in layouts() {
        for (tool, _) in path_tools() {
            let mut rig = Rig::new(&l, tool);
            rig.click(100.0, 100.0, NONE);
            rig.click(180.0, 110.0, SHIFT); // level: (180, 100)
            rig.click(190.0, 160.0, SHIFT); // upright from (180, 100): (180, 160)
            rig.mv(120.0, 215.0, SHIFT);
            rig.click(120.0, 215.0, SHIFT); // diagonal from (180, 160): (-60, 55) -> (-57.5, 57.5)
            let a = created(&rig.key(Key::Enter, NONE));
            assert_eq!(
                a.geometry,
                points(
                    &l,
                    &[
                        (100.0, 100.0),
                        (180.0, 100.0),
                        (180.0, 160.0),
                        (122.5, 217.5)
                    ]
                ),
                "{} {tool:?}",
                l.name
            );
        }
    }
}

#[test]
fn the_preview_is_the_shape_that_would_be_made() {
    for l in layouts() {
        // A drag tool: the preview is exactly the finished markup's drawing.
        for tool in [
            ToolKind::Rectangle,
            ToolKind::Ellipse,
            ToolKind::Line,
            ToolKind::Arrow,
        ] {
            let mut rig = Rig::new(&l, tool);
            rig.down(100.0, 100.0, NONE);
            assert!(
                rig.preview.is_empty(),
                "nothing to show before the first move"
            );
            rig.mv(220.0, 180.0, NONE);
            let shown = rig.preview.clone();
            let a = created(&rig.up(220.0, 180.0, NONE));
            assert_eq!(shown, a.draw_list(), "{} {tool:?}", l.name);
        }
        // A multi-click tool: placed points plus the pointer.
        for (tool, _) in path_tools() {
            let mut rig = Rig::new(&l, tool);
            click_all(&mut rig, &SQUARE[..3]);
            rig.mv(100.0, 200.0, NONE);
            let shown = rig.preview.clone();
            rig.click(100.0, 200.0, NONE);
            let a = created(&rig.key(Key::Enter, NONE));
            assert_eq!(shown, a.draw_list(), "{} {tool:?}", l.name);
            // The first edge alone is shown as a line.
            let mut rig = Rig::new(&l, tool);
            rig.click(100.0, 100.0, NONE);
            assert!(rig.preview.is_empty());
            rig.mv(200.0, 100.0, NONE);
            let b = rig.preview.bounds().expect("one edge is shown");
            let edge = l.rect(100.0, 100.0, 200.0, 100.0);
            assert!(
                b.x0 <= edge.x0 && b.x1 >= edge.x1 && b.y0 <= edge.y0 && b.y1 >= edge.y1,
                "{} {tool:?}: {b:?}",
                l.name
            );
            assert!(b.width().max(b.height()) < 100.0 * l.px + 4.0);
        }
        for tool in [ToolKind::Pen, ToolKind::Highlighter] {
            let mut rig = Rig::new(&l, tool);
            rig.down(100.0, 100.0, NONE);
            rig.mv(160.0, 100.0, NONE);
            rig.mv(160.0, 150.0, NONE);
            let shown = rig.preview.clone();
            let a = created(&rig.up(160.0, 150.0, NONE));
            assert_eq!(shown, a.draw_list(), "{} {tool:?}", l.name);
            assert_eq!(
                shown.uses_blend(),
                tool == ToolKind::Highlighter,
                "the highlighter previews in Multiply"
            );
        }
    }
}

#[test]
fn the_pen_thins_and_simplifies_its_stroke() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Pen);
        rig.down(100.0, 100.0, NONE);
        for (x, y) in [
            (101.0, 100.0), // closer than 1.5 px to the last kept point: dropped
            (104.0, 100.0),
            (110.0, 100.0),
            (120.0, 100.0),
            (120.0, 110.0),
            (120.0, 130.0),
        ] {
            assert_eq!(rig.mv(x, y, NONE), []);
        }
        let intents = rig.up(120.0, 130.0, NONE);
        assert_eq!(intents.len(), 1, "a pen stroke is not selected");
        let a = created(&intents);
        assert_eq!(a.kind, AnnotKind::Ink);
        // The points along each straight run are gone; the corner stays.
        assert_eq!(
            a.geometry,
            Geometry::Ink(vec![vec![
                l.at(100.0, 100.0),
                l.at(120.0, 100.0),
                l.at(120.0, 130.0)
            ]]),
            "{}",
            l.name
        );
        assert_eq!(a.style.width, 2.0);
        assert_eq!(a.style.blend, BlendMode::Normal);
        assert_eq!(a.meta.subject.as_deref(), Some("Pen"));
        assert!(rig.selection().is_empty());

        // A one-pixel zigzag: every peak is nearer than 1.5 px to the last kept point, so only
        // the points on the line are kept and the stroke comes out straight. Without thinning
        // the peaks (1 px off the line, more than the 0.5 px simplifying tolerance) would stay.
        let mut rig = Rig::new(&l, ToolKind::Pen);
        rig.down(100.0, 100.0, NONE);
        for i in 1..=10 {
            let y = if i % 2 == 1 { 101.0 } else { 100.0 };
            rig.mv(100.0 + i as f64, y, NONE);
        }
        let a = created(&rig.up(110.0, 100.0, NONE));
        assert_eq!(
            a.geometry,
            Geometry::Ink(vec![vec![l.at(100.0, 100.0), l.at(110.0, 100.0)]]),
            "{}",
            l.name
        );

        // A curve keeps its shape: every point the pointer passed through is within about half
        // a pixel of the result (half a pixel from simplifying, a hair more from thinning).
        let mut rig = Rig::new(&l, ToolKind::Pen);
        let arc: Vec<(f64, f64)> = (0..=90)
            .map(|d| {
                let t = (d as f64).to_radians();
                (200.0 + 80.0 * t.cos(), 200.0 - 80.0 * t.sin())
            })
            .collect();
        rig.down(arc[0].0, arc[0].1, NONE);
        for (x, y) in &arc[1..] {
            rig.mv(*x, *y, NONE);
        }
        let a = created(&rig.up(arc[90].0, arc[90].1, NONE));
        let Geometry::Ink(strokes) = &a.geometry else {
            panic!()
        };
        let kept = &strokes[0];
        assert!(
            kept.len() >= 4 && kept.len() < 30,
            "{}: {}",
            l.name,
            kept.len()
        );
        assert_eq!(kept[0], l.at(arc[0].0, arc[0].1));
        assert_eq!(*kept.last().unwrap(), l.at(arc[90].0, arc[90].1));
        for (x, y) in &arc {
            let p = l.at(*x, *y);
            let d = kept
                .windows(2)
                .map(|w| pl_geom::point_segment_distance(p, w[0], w[1]))
                .fold(f64::INFINITY, f64::min);
            assert!(d <= 0.6 * l.px, "{}: {d}", l.name);
        }
    }
}

#[test]
fn a_pen_tap_is_a_dot_and_shift_rules_a_straight_line() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Pen);
        let a = created(&rig.click(100.0, 100.0, NONE));
        assert_eq!(a.geometry, Geometry::Ink(vec![vec![l.at(100.0, 100.0)]]));
        // A wobble of a pixel or two is still a dot.
        let a = created(&rig.drag((300.0, 300.0), (302.0, 301.0), NONE));
        assert_eq!(a.geometry, Geometry::Ink(vec![vec![l.at(300.0, 300.0)]]));

        for tool in [ToolKind::Pen, ToolKind::Highlighter] {
            let mut rig = Rig::new(&l, tool);
            rig.down(100.0, 100.0, NONE);
            rig.mv(150.0, 112.0, SHIFT);
            rig.mv(180.0, 90.0, SHIFT);
            let a = created(&rig.up(180.0, 110.0, SHIFT));
            assert_eq!(
                a.geometry,
                Geometry::Ink(vec![vec![l.at(100.0, 100.0), l.at(180.0, 100.0)]]),
                "{} {tool:?}: level on screen",
                l.name
            );
            let a = created(&rig.drag((100.0, 100.0), (96.0, 220.0), SHIFT));
            assert_eq!(
                a.geometry,
                Geometry::Ink(vec![vec![l.at(100.0, 100.0), l.at(100.0, 220.0)]]),
                "{} {tool:?}: upright on screen",
                l.name
            );
        }
        // Freehand tools draw where the pointer is, not where a snap point is.
        let mut rig = Rig::new(&l, ToolKind::Pen);
        let p = |x, y| Pointer::new(PAGE, l.at(x, y)).with_snap(l.at(400.0, 400.0));
        rig.send(ToolInput::PointerDown(p(100.0, 100.0)));
        let a = created(&rig.send(ToolInput::PointerUp(p(150.0, 100.0))));
        assert_eq!(
            a.geometry,
            Geometry::Ink(vec![vec![l.at(100.0, 100.0), l.at(150.0, 100.0)]])
        );
    }
}

#[test]
fn the_highlighter_is_a_wide_multiply_stroke() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Highlighter);
        rig.down(100.0, 100.0, NONE);
        rig.mv(150.0, 100.0, NONE);
        rig.mv(150.0, 140.0, NONE);
        let intents = rig.up(220.0, 140.0, NONE);
        assert_eq!(intents.len(), 1);
        let a = created(&intents);
        assert_eq!(a.kind, AnnotKind::Ink);
        assert_eq!(
            a.geometry,
            Geometry::Ink(vec![vec![
                l.at(100.0, 100.0),
                l.at(150.0, 100.0),
                l.at(150.0, 140.0),
                l.at(220.0, 140.0)
            ]]),
            "{}",
            l.name
        );
        assert_eq!(a.style.blend, BlendMode::Multiply);
        assert_eq!(a.style.width, 12.0);
        assert_eq!(a.style.stroke, Some(Rgb::new(1.0, 0.9, 0.2)));
        assert_eq!(a.meta.subject.as_deref(), Some("Highlighter"));
        assert!(a.draw_list().uses_blend());
    }
}

#[test]
fn a_click_places_a_note_and_asks_for_its_text() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Note);
        let intents = rig.down(150.0, 150.0, NONE);
        assert_eq!(intents.len(), 3, "create, select, edit text");
        let a = created(&intents);
        assert!(matches!(&intents[2], Intent::RequestTextEdit(id) if *id == a.id));
        assert_eq!(a.kind, AnnotKind::Text);
        // 18 points square on paper, centred on the click: 18 screen pixels at 100%.
        assert_eq!(
            a.geometry,
            Geometry::Rect(l.rect(141.0, 141.0, 159.0, 159.0)),
            "{}",
            l.name
        );
        assert_eq!(a.icon.as_deref(), Some("Note"));
        assert_eq!(rig.view.text_edit, Some(a.id.clone()));
        assert_eq!(rig.up(150.0, 150.0, NONE), []);
        // The second click of a double-click lands on the new note and adds no second one.
        assert_eq!(rig.down(152.0, 151.0, NONE), []);
        assert_eq!(rig.double(152.0, 151.0), []);
        assert_eq!(rig.up(152.0, 151.0, NONE), []);
        assert_eq!(rig.on_page().len(), 1);
        // A click elsewhere is another note.
        let b = created(&rig.click(300.0, 200.0, NONE));
        assert_eq!(
            b.geometry,
            Geometry::Rect(l.rect(291.0, 191.0, 309.0, 209.0))
        );
        assert_eq!(rig.on_page().len(), 2);
        assert!(!rig.state.is_busy());
    }
}

#[test]
fn the_current_preset_styles_what_is_drawn() {
    let l = layouts()[1];
    let mut preset = ToolPreset::default_for(ToolKind::Rectangle);
    preset.name = "Demolition".into();
    preset.subject = "Demo".into();
    preset.style.stroke = Some(Rgb::new(0.0, 0.3, 0.9));
    preset.style.fill = Some(Rgb::new(0.9, 0.95, 1.0));
    preset.style.width = 3.5;
    preset.style.dash = Some(vec![6.0, 3.0]);
    preset.style.opacity = 0.6;
    let mut rig = Rig::new(&l, ToolKind::Rectangle);
    rig.state = ToolState::new(preset.clone());
    assert_eq!(rig.state.tool(), ToolKind::Rectangle);
    let a = created(&rig.drag((100.0, 100.0), (220.0, 180.0), NONE));
    assert_eq!(a.style, preset.style);
    assert_eq!(a.meta.subject.as_deref(), Some("Demo"));

    // Changing the preset in the middle of a gesture keeps the gesture and restyles it.
    rig.down(100.0, 100.0, NONE);
    rig.mv(200.0, 200.0, NONE);
    let mut thin = preset.clone();
    thin.style.width = 0.5;
    rig.state = rig.state.clone().with_preset(thin.clone());
    assert!(rig.state.is_busy());
    let a = created(&rig.up(200.0, 200.0, NONE));
    assert_eq!(a.style.width, 0.5);
    // A preset for another tool ends it.
    rig.down(100.0, 100.0, NONE);
    rig.state = rig
        .state
        .clone()
        .with_preset(ToolPreset::default_for(ToolKind::Line));
    assert!(!rig.state.is_busy());
    assert_eq!(rig.state.tool(), ToolKind::Line);
    // `cancelled` drops a gesture too.
    rig.down(100.0, 100.0, NONE);
    assert_eq!(rig.state.page(), Some(PAGE));
    rig.state = rig.state.clone().cancelled();
    assert!(!rig.state.is_busy());
    assert_eq!(rig.state.page(), None);
}

#[test]
fn events_off_the_document_or_off_the_gesture_page_are_ignored() {
    for l in layouts() {
        for tool in ToolKind::ALL {
            let rig = Rig::new(&l, tool);
            let nowhere = Pointer::new(99, Point::new(10.0, 10.0));
            let nan = Pointer::new(PAGE, Point::new(f64::NAN, 10.0));
            for p in [nowhere, nan] {
                for input in [
                    ToolInput::PointerDown(p),
                    ToolInput::PointerMove(p),
                    ToolInput::PointerUp(p),
                    ToolInput::DoubleClick(p),
                ] {
                    let before = rig.state.clone();
                    let (state, intents, preview) = step(before.clone(), &input, &rig.view);
                    assert_eq!(state, before, "{tool:?} {input:?}");
                    assert_eq!(intents, []);
                    assert!(preview.is_empty());
                }
            }
        }
        // A gesture stays on the page it started on: an event from another page changes
        // nothing, and the release finishes with the last position on the gesture's page.
        let other = |x, y| Pointer::new(0, Point::new(x, y));
        let mut rig = Rig::new(&l, ToolKind::Rectangle);
        rig.down(100.0, 100.0, NONE);
        rig.mv(220.0, 180.0, NONE);
        assert_eq!(rig.send(ToolInput::PointerMove(other(5.0, 5.0))), []);
        let a = created(&rig.send(ToolInput::PointerUp(other(5.0, 5.0))));
        assert_eq!(a.page, PAGE);
        assert_eq!(
            a.geometry,
            Geometry::Rect(l.rect(100.0, 100.0, 220.0, 180.0))
        );

        let mut rig = Rig::new(&l, ToolKind::Polygon);
        click_all(&mut rig, &SQUARE[..3]);
        assert_eq!(rig.send(ToolInput::PointerDown(other(300.0, 300.0))), []);
        assert_eq!(rig.send(ToolInput::DoubleClick(other(300.0, 300.0))), []);
        assert!(rig.state.is_busy());
        let a = created(&rig.key(Key::Enter, NONE));
        assert_eq!(a.geometry, points(&l, &SQUARE[..3]));
        assert_eq!(a.page, PAGE);
    }
}

#[test]
fn a_step_is_a_pure_function_of_its_arguments() {
    let l = layouts()[3];
    for tool in ToolKind::ALL {
        let mut rig = Rig::new(&l, tool);
        let script = [
            ToolInput::PointerMove(rig.pointer(90.0, 90.0, NONE)),
            ToolInput::PointerDown(rig.pointer(100.0, 100.0, NONE)),
            ToolInput::PointerMove(rig.pointer(160.0, 130.0, SHIFT)),
            ToolInput::PointerUp(rig.pointer(160.0, 130.0, SHIFT)),
            ToolInput::PointerDown(rig.pointer(200.0, 100.0, NONE)),
            ToolInput::PointerUp(rig.pointer(200.0, 100.0, NONE)),
            ToolInput::key(Key::Enter),
        ];
        for input in script {
            let view_before = rig.view.clone();
            let (s1, i1, p1) = step(rig.state.clone(), &input, &rig.view);
            let (s2, i2, p2) = step(rig.state.clone(), &input, &rig.view);
            assert_eq!(rig.view, view_before, "the view is only read");
            match (s1.text_draft(), s2.text_draft()) {
                // A text tool waiting for its text holds the markup it will create, which
                // has a generated id like any other new markup.
                (Some(x), Some(y)) => {
                    let mut y = y.clone();
                    y.id = x.id.clone();
                    assert_eq!(*x, y, "{tool:?} {input:?}");
                    assert_eq!((s1.is_busy(), s1.page()), (s2.is_busy(), s2.page()));
                }
                _ => assert_eq!(s1, s2, "{tool:?} {input:?}"),
            }
            assert_eq!(p1, p2);
            assert_eq!(i1.len(), i2.len());
            // New markups differ only in their generated id.
            for (a, b) in i1.iter().zip(&i2) {
                match (a, b) {
                    (Intent::Create(x), Intent::Create(y)) => {
                        assert_ne!(x.id, y.id);
                        let mut y = y.clone();
                        y.id = x.id.clone();
                        assert_eq!(*x, y);
                    }
                    (Intent::Select(_), Intent::Select(_))
                    | (Intent::RequestTextEdit(_), Intent::RequestTextEdit(_)) => {}
                    _ => assert_eq!(a, b),
                }
            }
            rig.send(input);
        }
    }
}
