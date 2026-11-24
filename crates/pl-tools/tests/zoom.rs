//! Tolerances at zooms other than 100%, snapping against the click threshold, and small
//! selected markups. Positions are screen pixels (see `common`); every layout is run at the
//! lowest zoom (5%: 20 page units per pixel on a plain page), at 100% and at the highest
//! (6400%).

mod common;

use common::*;
use pl_annot::{AnnotId, Annotation, Geometry, HandleKind, RectHandle, Rgb};
use pl_tools::{
    grab_handle, AnnotChange, Cursor, DocView, Intent, Pointer, ToolInput, ToolKind, CLOSE_PX,
    DRAG_PX, HANDLE_PX, HIT_PX,
};

const ZOOMS: [f64; 3] = [0.05, 1.0, 64.0];

/// Every layout at every zoom: `px` is page units per screen pixel.
fn zoomed() -> Vec<(Layout, f64)> {
    let mut out = Vec::new();
    for l in layouts() {
        for zoom in ZOOMS {
            let mut z = l;
            z.px = l.px / zoom;
            out.push((z, zoom));
        }
    }
    out
}

fn shifted(g: &Geometry, by: pl_geom::Vec2) -> Geometry {
    g.map_points(|p| p + by)
}

fn select(ids: &[&AnnotId]) -> Intent {
    Intent::Select(ids.iter().map(|id| (*id).clone()).collect())
}

fn cursor_at(rig: &Rig, sx: f64, sy: f64) -> Cursor {
    let input = ToolInput::PointerMove(rig.pointer(sx, sy, NONE));
    let (_, intents, _) = pl_tools::step(rig.state.clone(), &input, &rig.view);
    match intents.as_slice() {
        [Intent::SetCursor(c)] => *c,
        other => panic!("expected one cursor, got {other:?}"),
    }
}

/// A filled rectangle between two screen corners.
fn filled(l: &Layout, x0: f64, y0: f64, x1: f64, y1: f64) -> Annotation {
    let mut a = Annotation::square(PAGE, l.rect(x0, y0, x1, y1), &origin());
    a.style.fill = Some(Rgb::new(0.9, 0.9, 1.0));
    // A hairline on screen at every zoom, so the fill decides what is hit.
    a.style.width = l.px * 0.5;
    a
}

#[test]
fn pick_tolerance_is_in_screen_pixels_at_every_zoom() {
    for (l, zoom) in zoomed() {
        let what = format!("{} at {zoom}", l.name);
        let mut rig = Rig::new(&l, ToolKind::Select);
        assert_eq!(rig.view.pixel_size(), l.px);
        let mut line = Annotation::line(PAGE, l.at(100.0, 100.0), l.at(200.0, 100.0), &origin());
        line.style.width = l.px;
        let line = rig.add(line);
        // Just inside the pick distance of the line, and just outside it.
        let near = HIT_PX + 0.5 - 0.3;
        let far = HIT_PX + 0.5 + 0.6;
        assert_eq!(
            rig.click(150.0, 100.0 + far, NONE),
            [],
            "{what}: beyond HIT_PX"
        );
        assert_eq!(
            rig.click(150.0, 100.0 + near, NONE),
            [select(&[&line])],
            "{what}: within HIT_PX"
        );
        // In page units that is HIT_PX pixels of this zoom.
        let a = rig.get(&line);
        let p = l.at(150.0, 100.0 + near);
        assert!(a.hit_test(p, HIT_PX * l.px), "{what}");
        assert!(
            !a.hit_test(l.at(150.0, 100.0 + far), HIT_PX * l.px),
            "{what}"
        );
    }
}

#[test]
fn handle_reach_is_in_screen_pixels_at_every_zoom() {
    for (l, zoom) in zoomed() {
        let what = format!("{} at {zoom}", l.name);
        let mut rig = Rig::new(&l, ToolKind::Select);
        let rect = rig.add(filled(&l, 100.0, 100.0, 300.0, 220.0));
        rig.click(200.0, 160.0, NONE);
        assert_eq!(rig.selection(), std::slice::from_ref(&rect));
        let a = rig.get(&rect).clone();
        // Outside the corner at screen (300,100): within reach, then beyond it.
        let within = l.at(300.0 + HANDLE_PX - 0.5, 100.0);
        let beyond = l.at(300.0 + HANDLE_PX + 0.5, 100.0);
        assert!(grab_handle(&a, within, l.px).is_some(), "{what}");
        assert!(grab_handle(&a, beyond, l.px).is_none(), "{what}");
        // The middle of a large markup is not a handle, and its cursor says "move".
        assert!(
            grab_handle(&a, l.at(200.0, 160.0), l.px).is_none(),
            "{what}"
        );
        assert_eq!(cursor_at(&rig, 200.0, 160.0), Cursor::Move, "{what}");
        assert_ne!(
            cursor_at(&rig, 300.0 + HANDLE_PX - 0.5, 100.0),
            Cursor::Move,
            "{what}"
        );
        // Dragging that corner 40 px resizes by 40 px of this zoom.
        let got = rig.drag((302.0, 98.0), (342.0, 58.0), NONE);
        let Some(Intent::Modify {
            change: AnnotChange::Geometry(Geometry::Rect(r)),
            ..
        }) = got.last()
        else {
            panic!("{what}: {got:?}");
        };
        let want = l.rect(100.0, 58.0, 342.0, 220.0);
        let tol = 1e-9 * (1.0 + l.px) * 1000.0;
        assert!(
            (r.x0 - want.x0).abs() < tol
                && (r.y0 - want.y0).abs() < tol
                && (r.x1 - want.x1).abs() < tol
                && (r.y1 - want.y1).abs() < tol,
            "{what}: {r:?} vs {want:?}"
        );
    }
}

#[test]
fn a_small_selected_markup_moves_when_pressed_in_its_middle() {
    for (l, zoom) in zoomed() {
        let what = format!("{} at {zoom}", l.name);
        // Ten by eight pixels on screen: every point of it is within HANDLE_PX of a handle.
        for (w, h) in [(10.0, 8.0), (5.0, 5.0), (11.0, 3.0)] {
            let mut rig = Rig::new(&l, ToolKind::Select);
            let rect = rig.add(filled(&l, 100.0, 100.0, 100.0 + w, 100.0 + h));
            let (cx, cy) = (100.0 + w / 2.0, 100.0 + h / 2.0);
            assert_eq!(rig.click(cx, cy, NONE), [select(&[&rect])], "{what}");
            let before = rig.geometry(&rect);
            assert_eq!(cursor_at(&rig, cx, cy), Cursor::Move, "{what} {w}x{h}");
            let got = rig.drag((cx, cy), (cx + 60.0, cy + 40.0), NONE);
            let want = shifted(&before, l.vec(60.0, 40.0));
            match got.as_slice() {
                [Intent::Modify {
                    id,
                    change: AnnotChange::Geometry(g),
                }] => {
                    assert_eq!(id, &rect);
                    let (Geometry::Rect(g), Geometry::Rect(want)) = (g, &want) else {
                        panic!("{what}: rectangles");
                    };
                    let tol = 1e-6 * (1.0 + l.px);
                    assert!(
                        (g.x0 - want.x0).abs() < tol && (g.y1 - want.y1).abs() < tol,
                        "{what} {w}x{h}: {g:?} vs {want:?}"
                    );
                    assert!(
                        (g.width() - want.width()).abs() < tol
                            && (g.height() - want.height()).abs() < tol,
                        "{what} {w}x{h}: a move, not a resize"
                    );
                }
                other => panic!("{what} {w}x{h}: expected one move, got {other:?}"),
            }
            // From just outside a corner it can still be resized.
            let a = rig.get(&rect).clone();
            let b = a.geometry.bounds();
            let outside = pl_geom::Point::new(b.x1 + 2.0 * l.px, b.y1 + 2.0 * l.px);
            assert!(
                matches!(
                    grab_handle(&a, outside, l.px).map(|h| h.kind),
                    Some(HandleKind::Rect(RectHandle::TopRight))
                ),
                "{what} {w}x{h}"
            );
        }
    }
}

#[test]
fn a_pen_dot_and_a_short_line_can_be_moved() {
    for (l, zoom) in zoomed() {
        let what = format!("{} at {zoom}", l.name);
        // A dot: eight handles on one point, nothing to resize.
        let mut rig = Rig::new(&l, ToolKind::Select);
        let mut dot = Annotation::ink(PAGE, vec![vec![l.at(150.0, 150.0)]], &origin());
        dot.style.width = 4.0 * l.px;
        let dot = rig.add(dot);
        assert_eq!(rig.click(150.0, 150.0, NONE), [select(&[&dot])], "{what}");
        let got = rig.drag((151.0, 150.0), (201.0, 150.0), NONE);
        assert!(
            matches!(got.as_slice(), [Intent::Modify { id, .. }] if *id == dot),
            "{what}: {got:?}"
        );
        let Geometry::Ink(strokes) = rig.geometry(&dot) else {
            panic!("ink");
        };
        let want = l.at(200.0, 150.0);
        let tol = 1e-6 * (1.0 + l.px);
        assert!(
            (strokes[0][0].x - want.x).abs() < tol && (strokes[0][0].y - want.y).abs() < tol,
            "{what}: {:?} vs {want:?}",
            strokes[0][0]
        );

        // A nine-pixel line: its middle moves it, its ends reshape it.
        let mut rig = Rig::new(&l, ToolKind::Select);
        let mut line = Annotation::line(PAGE, l.at(100.0, 100.0), l.at(109.0, 100.0), &origin());
        line.style.width = l.px;
        let line = rig.add(line);
        rig.click(104.5, 100.0, NONE);
        assert_eq!(rig.selection(), std::slice::from_ref(&line));
        let a = rig.get(&line).clone();
        assert!(
            grab_handle(&a, l.at(104.5, 100.0), l.px).is_none(),
            "{what}"
        );
        assert!(
            matches!(
                grab_handle(&a, l.at(99.0, 100.0), l.px).map(|h| h.kind),
                Some(HandleKind::Vertex(0))
            ),
            "{what}"
        );
        let got = rig.drag((104.5, 100.0), (154.5, 130.0), NONE);
        let want = shifted(&a.geometry, l.vec(50.0, 30.0));
        let Some(Intent::Modify {
            change: AnnotChange::Geometry(Geometry::Line { a: p, b: q }),
            ..
        }) = got.first()
        else {
            panic!("{what}: {got:?}");
        };
        let Geometry::Line { a: wp, b: wq } = want else {
            panic!("a line");
        };
        assert!((*p - wp).hypot() < tol && (*q - wq).hypot() < tol, "{what}");
    }
}

#[test]
fn a_click_whose_snap_point_jumps_moves_nothing_and_draws_nothing() {
    for (l, zoom) in zoomed() {
        let what = format!("{} at {zoom}", l.name);
        // Select: press without a snap, release a pixel away with a snap nine pixels off.
        let mut rig = Rig::new(&l, ToolKind::Select);
        let rect = rig.add(filled(&l, 100.0, 100.0, 300.0, 220.0));
        let other = rig.add(filled(&l, 320.0, 100.0, 420.0, 220.0));
        let before = rig.geometry(&rect);
        let press = rig.pointer(200.0, 160.0, NONE);
        let release = rig
            .pointer(201.0, 160.5, NONE)
            .with_snap(l.at(209.0, 163.0));
        let mut got = rig.send(ToolInput::PointerDown(press));
        got.extend(rig.send(ToolInput::PointerMove(release)));
        got.extend(rig.send(ToolInput::PointerUp(release)));
        assert_eq!(got, [select(&[&rect])], "{what}: a click only selects");
        assert_eq!(rig.geometry(&rect), before, "{what}");

        // The other way round: the press snaps, the release does not.
        let press = rig
            .pointer(200.0, 160.0, NONE)
            .with_snap(l.at(193.0, 155.0));
        let release = rig.pointer(200.5, 160.0, NONE);
        let mut got = rig.send(ToolInput::PointerDown(press));
        got.extend(rig.send(ToolInput::PointerUp(release)));
        assert_eq!(got, [], "{what}");
        assert_eq!(rig.geometry(&rect), before, "{what}");

        // A click on one of several selected markups still narrows the selection when the
        // press snapped somewhere else.
        rig.click(370.0, 160.0, SHIFT);
        assert_eq!(rig.selection(), [rect.clone(), other.clone()]);
        let press = rig
            .pointer(200.0, 160.0, NONE)
            .with_snap(l.at(208.0, 160.0));
        let mut got = rig.send(ToolInput::PointerDown(press));
        got.extend(rig.send(ToolInput::PointerUp(press)));
        assert_eq!(got, [select(&[&rect])], "{what}");

        // A real drag with snapping still lands on the snap point.
        let press = rig.pointer(200.0, 160.0, NONE);
        let end = rig
            .pointer(250.0, 190.0, NONE)
            .with_snap(l.at(256.0, 196.0));
        rig.send(ToolInput::PointerDown(press));
        rig.send(ToolInput::PointerMove(end));
        let got = rig.send(ToolInput::PointerUp(end));
        let want = shifted(&before, l.vec(56.0, 36.0));
        match (got.as_slice(), &want) {
            (
                [Intent::Modify {
                    change: AnnotChange::Geometry(Geometry::Rect(g)),
                    ..
                }],
                Geometry::Rect(w),
            ) => {
                let tol = 1e-6 * (1.0 + l.px);
                assert!(
                    (g.x0 - w.x0).abs() < tol && (g.y0 - w.y0).abs() < tol,
                    "{what}: {g:?} vs {w:?}"
                );
            }
            other => panic!("{what}: {other:?}"),
        }

        // The drawing tools: a click with a jumping snap point makes no shape.
        for tool in [
            ToolKind::Rectangle,
            ToolKind::Ellipse,
            ToolKind::Line,
            ToolKind::Arrow,
        ] {
            let mut rig = Rig::new(&l, tool);
            let press = rig.pointer(150.0, 150.0, NONE);
            let release = rig
                .pointer(151.0, 151.0, NONE)
                .with_snap(l.at(158.0, 157.0));
            let mut got = rig.send(ToolInput::PointerDown(press));
            got.extend(rig.send(ToolInput::PointerMove(release)));
            assert!(
                rig.preview.is_empty(),
                "{what} {tool:?}: no preview of a click"
            );
            got.extend(rig.send(ToolInput::PointerUp(release)));
            assert_eq!(got, [], "{what} {tool:?}");
            assert!(rig.on_page().is_empty());
            // Just under the threshold is still a click; just over it draws.
            let under = DRAG_PX - 0.2;
            let got = rig.drag((150.0, 150.0), (150.0 + under, 150.0), NONE);
            assert_eq!(got, [], "{what} {tool:?}");
            let over = DRAG_PX + 30.0;
            let got = rig.drag((150.0, 150.0), (150.0 + over, 150.0 + over), NONE);
            assert!(
                matches!(got.first(), Some(Intent::Create(_))),
                "{what} {tool:?}: {got:?}"
            );
        }
    }
}

#[test]
fn a_polygon_closes_within_close_px_at_every_zoom() {
    for (l, zoom) in zoomed() {
        let what = format!("{} at {zoom}", l.name);
        let mut rig = Rig::new(&l, ToolKind::Polygon);
        rig.click(100.0, 100.0, NONE);
        rig.click(200.0, 100.0, NONE);
        rig.click(150.0, 180.0, NONE);
        // Beyond the closing distance of the first point: a fourth corner.
        assert_eq!(rig.click(100.0 + CLOSE_PX + 1.0, 100.0, NONE), [], "{what}");
        assert!(rig.state.is_busy(), "{what}");
        // Within it: the polygon closes with the four corners placed so far.
        let got = rig.click(100.0 + CLOSE_PX - 1.0, 100.0, NONE);
        let a = created(&got);
        let Geometry::Points(points) = &a.geometry else {
            panic!("{what}: points");
        };
        assert_eq!(points.len(), 4, "{what}");
        assert!(!rig.state.is_busy());
        let _ = Pointer::new(PAGE, l.at(0.0, 0.0));
    }
}
