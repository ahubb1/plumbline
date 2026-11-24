//! Scripted input sequences for the Select tool, with exact golden geometry, on all eight
//! page layouts. Positions are screen pixels (see `common`).

mod common;

use common::*;
use pl_annot::{AnnotId, AnnotKind, Annotation, Flags, Geometry, HandleKind, Rgb};
use pl_geom::{Point, Vec2};
use pl_tools::{
    selection_handles, AnnotChange, Cursor, DocView, Intent, Key, Modifiers, Pointer, ToolInput,
    ToolKind, NUDGE, NUDGE_FINE, NUDGE_LARGE,
};

/// The markups every script starts with, bottom to top.
struct Scene {
    rig: Rig,
    /// A filled rectangle, screen (100,100)-(200,160).
    rect: AnnotId,
    /// A line from screen (250,100) to (350,160).
    line: AnnotId,
    /// A filled triangle: screen (100,250), (200,250), (150,330).
    tri: AnnotId,
    /// Two pen strokes inside screen (260,260)-(300,300).
    ink: AnnotId,
    /// A note centred on screen (380,300).
    note: AnnotId,
}

fn scene(l: &Layout) -> Scene {
    let mut rig = Rig::new(l, ToolKind::Select);
    let o = origin();
    let mut rect = Annotation::square(PAGE, l.rect(100.0, 100.0, 200.0, 160.0), &o);
    rect.style.fill = Some(Rgb::new(0.9, 0.9, 1.0));
    let line = Annotation::line(PAGE, l.at(250.0, 100.0), l.at(350.0, 160.0), &o);
    let mut tri = Annotation::polygon(
        PAGE,
        vec![l.at(100.0, 250.0), l.at(200.0, 250.0), l.at(150.0, 330.0)],
        &o,
    );
    tri.style.fill = Some(Rgb::new(1.0, 0.9, 0.9));
    let ink = Annotation::ink(
        PAGE,
        vec![
            vec![l.at(260.0, 260.0), l.at(300.0, 300.0)],
            vec![l.at(280.0, 260.0)],
        ],
        &o,
    );
    let note = Annotation::note(PAGE, l.rect(371.0, 291.0, 389.0, 309.0), "check", &o);
    // Something on the other page, at the same coordinates, that must never be touched.
    rig.add(Annotation::square(
        0,
        pl_geom::Rect::new(0.0, 0.0, 612.0, 792.0),
        &o,
    ));
    Scene {
        rect: rig.add(rect),
        line: rig.add(line),
        tri: rig.add(tri),
        ink: rig.add(ink),
        note: rig.add(note),
        rig,
    }
}

fn modify(id: &AnnotId, geometry: Geometry) -> Intent {
    Intent::Modify {
        id: id.clone(),
        change: AnnotChange::Geometry(geometry),
    }
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

#[test]
fn click_shift_click_and_escape() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        // A click selects; the release adds nothing.
        assert_eq!(
            rig.down(150.0, 130.0, NONE),
            [select(&[&s.rect])],
            "{}",
            l.name
        );
        assert_eq!(rig.up(150.0, 130.0, NONE), []);
        // Clicking it again changes nothing.
        assert_eq!(rig.click(150.0, 130.0, NONE), []);
        // Another one replaces the selection. The line is hit within four pixels.
        assert_eq!(
            rig.click(300.0, 133.0, NONE),
            [select(&[&s.line])],
            "{}",
            l.name
        );
        // Shift-click adds, in click order, and takes away.
        assert_eq!(rig.click(150.0, 290.0, SHIFT), [select(&[&s.line, &s.tri])]);
        assert_eq!(
            rig.click(150.0, 130.0, SHIFT),
            [select(&[&s.line, &s.tri, &s.rect])]
        );
        assert_eq!(rig.click(300.0, 130.0, SHIFT), [select(&[&s.tri, &s.rect])]);
        assert!(!rig.state.is_busy());
        // A plain click on one of several selected keeps them for the press (it might be the
        // start of a move) and narrows to that one on release.
        assert_eq!(rig.down(150.0, 290.0, NONE), []);
        assert_eq!(rig.up(150.0, 290.0, NONE), [select(&[&s.tri])]);
        // Shift-click on empty paper keeps the selection; a plain click there clears it.
        assert_eq!(rig.click(420.0, 420.0, SHIFT), []);
        assert_eq!(rig.selection(), [s.tri.clone()]);
        assert_eq!(rig.click(420.0, 420.0, NONE), [select(&[])]);
        assert_eq!(rig.click(420.0, 420.0, NONE), []);
        // Esc clears too.
        rig.click(380.0, 300.0, NONE);
        assert_eq!(rig.selection(), [s.note.clone()]);
        assert_eq!(rig.key(Key::Escape, NONE), [select(&[])]);
        assert_eq!(rig.key(Key::Escape, NONE), []);
    }
}

#[test]
fn the_topmost_markup_under_the_pointer_is_picked() {
    for l in layouts() {
        let mut s = scene(&l);
        let o = origin();
        // An unfilled rectangle over the filled one: it is picked on its border only.
        let hollow = s.rig.add(Annotation::square(
            PAGE,
            l.rect(120.0, 110.0, 180.0, 150.0),
            &o,
        ));
        assert_eq!(
            s.rig.click(150.0, 130.0, NONE),
            [select(&[&s.rect])],
            "{}",
            l.name
        );
        assert_eq!(
            s.rig.click(120.0, 130.0, NONE),
            [select(&[&hollow])],
            "{}",
            l.name
        );
        // A filled one on top wins where they overlap.
        let mut top = Annotation::circle(PAGE, l.rect(160.0, 120.0, 260.0, 200.0), &o);
        top.style.fill = Some(Rgb::WHITE);
        let top = s.rig.add(top);
        assert_eq!(s.rig.click(190.0, 150.0, NONE), [select(&[&top])]);
        // Hidden markups and popup windows are not picked.
        let mut hidden = Annotation::square(PAGE, l.rect(300.0, 350.0, 400.0, 420.0), &o);
        hidden.style.fill = Some(Rgb::BLACK);
        hidden.meta.flags = hidden.meta.flags.with(Flags::HIDDEN, true);
        s.rig.add(hidden);
        let mut parent = s.rig.get(&s.note).clone();
        let popup = Annotation::popup_for(&mut parent, l.rect(300.0, 350.0, 400.0, 420.0), &o);
        s.rig.add(popup);
        assert_eq!(s.rig.click(350.0, 380.0, NONE), [select(&[])], "{}", l.name);
        // Nor are they taken by a selection box.
        let got = s.rig.drag((290.0, 340.0), (410.0, 430.0), NONE);
        assert_eq!(got, []);
    }
}

#[test]
fn a_box_dragged_rightwards_takes_what_it_encloses_and_leftwards_what_it_touches() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        // Left to right on screen around the rectangle and the triangle; the line and the
        // pen strokes are outside.
        rig.down(90.0, 90.0, NONE);
        rig.mv(150.0, 200.0, NONE);
        assert!(!rig.preview.is_empty());
        assert_eq!(rig.preview.ops.len(), 2, "a tint and an outline");
        assert_eq!(
            rig.preview.ops[0].bounds().unwrap(),
            l.rect(90.0, 90.0, 150.0, 200.0)
        );
        assert_eq!(
            rig.up(210.0, 340.0, NONE),
            [select(&[&s.rect, &s.tri])],
            "{}",
            l.name
        );
        assert!(rig.preview.is_empty());
        // The same box cut short encloses only the rectangle: the triangle sticks out.
        assert_eq!(
            rig.drag((90.0, 90.0), (210.0, 300.0), NONE),
            [select(&[&s.rect])]
        );
        // Right to left on screen: everything the box touches, in stacking order.
        assert_eq!(
            rig.drag((260.0, 170.0), (190.0, 90.0), NONE),
            [select(&[&s.rect, &s.line])],
            "{}",
            l.name
        );
        // Shift adds to the selection.
        rig.down(250.0, 250.0, SHIFT);
        rig.mv(280.0, 280.0, SHIFT);
        assert_eq!(
            rig.up(310.0, 310.0, SHIFT),
            [select(&[&s.rect, &s.line, &s.ink])],
            "{}",
            l.name
        );
        // A box that takes nothing clears it; a second one has nothing left to clear.
        assert_eq!(
            rig.drag((400.0, 100.0), (440.0, 140.0), NONE),
            [select(&[])]
        );
        assert_eq!(rig.drag((400.0, 100.0), (440.0, 140.0), NONE), []);
        // Esc abandons a box.
        rig.click(150.0, 130.0, NONE);
        rig.down(90.0, 240.0, NONE);
        rig.mv(210.0, 340.0, NONE);
        assert_eq!(rig.key(Key::Escape, NONE), []);
        assert_eq!(rig.up(210.0, 340.0, NONE), []);
        assert_eq!(rig.selection(), [s.rect.clone()]);
    }
}

#[test]
fn dragging_moves_the_selection() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        // Press on an unselected markup and drag: it is selected and moved in one gesture.
        assert_eq!(rig.down(150.0, 130.0, NONE), [select(&[&s.rect])]);
        assert_eq!(rig.mv(170.0, 140.0, NONE), []);
        assert_eq!(rig.state.editing(), [s.rect.clone()]);
        let mut ghost = rig.get(&s.rect).detached();
        ghost.geometry = Geometry::Rect(l.rect(120.0, 110.0, 220.0, 170.0));
        assert_eq!(rig.preview, ghost.draw_list(), "{}", l.name);
        let moved = Geometry::Rect(l.rect(140.0, 125.0, 240.0, 185.0));
        assert_eq!(
            rig.up(190.0, 155.0, NONE),
            [modify(&s.rect, moved.clone())],
            "{}",
            l.name
        );
        assert_eq!(rig.geometry(&s.rect), moved);
        assert!(rig.state.editing().is_empty() && rig.preview.is_empty());

        // Several at once: one Modify each, in selection order.
        rig.click(300.0, 130.0, SHIFT);
        rig.click(150.0, 290.0, SHIFT);
        let got = rig.drag((300.0, 130.0), (280.0, 180.0), NONE);
        assert_eq!(
            got,
            [
                modify(&s.rect, Geometry::Rect(l.rect(120.0, 175.0, 220.0, 235.0))),
                modify(&s.line, line(l.at(230.0, 150.0), l.at(330.0, 210.0))),
                modify(
                    &s.tri,
                    Geometry::Points(vec![
                        l.at(80.0, 300.0),
                        l.at(180.0, 300.0),
                        l.at(130.0, 380.0)
                    ])
                ),
            ],
            "{}",
            l.name
        );
        assert_eq!(rig.selection().len(), 3, "a move keeps the selection");
        // The markup on the other page was never touched.
        assert_eq!(
            rig.view.annotations[0].geometry,
            Geometry::Rect(pl_geom::Rect::new(0.0, 0.0, 612.0, 792.0))
        );
    }
}

#[test]
fn shift_keeps_a_move_to_one_screen_axis_and_ctrl_leaves_a_copy() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        // Mostly across the screen: the 12 pixels down are dropped.
        let got = rig.drag((150.0, 130.0), (200.0, 142.0), SHIFT);
        assert_eq!(
            got,
            [
                select(&[&s.rect]),
                modify(&s.rect, Geometry::Rect(l.rect(150.0, 100.0, 250.0, 160.0)))
            ],
            "{}",
            l.name
        );
        // Mostly down the screen.
        let got = rig.drag((200.0, 130.0), (190.0, 190.0), SHIFT);
        assert_eq!(
            got,
            [modify(
                &s.rect,
                Geometry::Rect(l.rect(150.0, 160.0, 250.0, 220.0))
            )],
            "{}",
            l.name
        );

        // Ctrl at the release: the original stays, a copy lands at the new place and becomes
        // the selection.
        let before = rig.get(&s.line).clone();
        rig.down(300.0, 130.0, NONE);
        rig.mv(320.0, 160.0, CTRL);
        assert!(rig.state.editing().is_empty(), "the original stays visible");
        assert!(!rig.preview.is_empty());
        let got = rig.up(340.0, 190.0, CTRL);
        let [Intent::Create(copy), Intent::Select(ids)] = got.as_slice() else {
            panic!("{}: {got:?}", l.name)
        };
        assert_eq!(ids, std::slice::from_ref(&copy.id));
        assert_ne!(copy.id, before.id);
        assert_eq!(copy.geometry, line(l.at(290.0, 160.0), l.at(390.0, 220.0)));
        assert_eq!(copy.style, before.style);
        assert_eq!(copy.kind, AnnotKind::Line);
        assert_eq!(*rig.get(&s.line), before);
        assert_eq!(rig.selection(), std::slice::from_ref(&copy.id));
    }
}

#[test]
fn small_cancelled_and_locked_moves_change_nothing() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        let before = rig.view.annotations.clone();
        // Under three pixels is a click.
        assert_eq!(
            rig.drag((150.0, 130.0), (152.0, 131.0), NONE),
            [select(&[&s.rect])]
        );
        // Out and back to the start exactly.
        rig.down(150.0, 130.0, NONE);
        rig.mv(200.0, 200.0, NONE);
        assert_eq!(rig.up(150.0, 130.0, NONE), []);
        // Esc in the middle.
        rig.down(150.0, 130.0, NONE);
        rig.mv(200.0, 200.0, NONE);
        assert!(rig.state.is_busy());
        assert_eq!(rig.key(Key::Escape, NONE), []);
        assert!(!rig.state.is_busy() && rig.preview.is_empty());
        assert_eq!(rig.up(200.0, 200.0, NONE), []);
        assert_eq!(rig.view.annotations, before, "{}", l.name);
        assert_eq!(rig.selection(), [s.rect.clone()], "Esc keeps the selection");

        // A locked markup can be selected but not moved, reshaped, nudged or deleted.
        let locked = rig.get(&s.tri).meta.flags.with(Flags::LOCKED, true);
        rig.view
            .annotations
            .iter_mut()
            .find(|a| a.id == s.tri)
            .unwrap()
            .meta
            .flags = locked;
        let before = rig.view.annotations.clone();
        assert_eq!(
            rig.drag((150.0, 290.0), (250.0, 350.0), NONE),
            [select(&[&s.tri])]
        );
        assert!(selection_handles(&rig.view).is_empty());
        assert_eq!(rig.drag((150.0, 330.0), (150.0, 400.0), NONE), []);
        assert_eq!(rig.key(Key::Right, NONE), []);
        assert_eq!(rig.key(Key::Delete, NONE), []);
        assert_eq!(rig.view.annotations, before, "{}", l.name);
        // Moved together with a free one, only the free one goes.
        rig.click(150.0, 130.0, SHIFT);
        let got = rig.drag((150.0, 130.0), (160.0, 130.0), NONE);
        assert_eq!(
            got,
            [modify(
                &s.rect,
                Geometry::Rect(l.rect(110.0, 100.0, 210.0, 160.0))
            )]
        );
    }
}

/// The eight handles of the rectangle by screen position, and which screen edges each moves:
/// (handle x, handle y, moves left edge, moves top edge, moves right edge, moves bottom edge).
const HANDLES: [(f64, f64, bool, bool, bool, bool); 8] = [
    (100.0, 100.0, true, true, false, false),
    (150.0, 100.0, false, true, false, false),
    (200.0, 100.0, false, true, true, false),
    (200.0, 130.0, false, false, true, false),
    (200.0, 160.0, false, false, true, true),
    (150.0, 160.0, false, false, false, true),
    (100.0, 160.0, true, false, false, true),
    (100.0, 130.0, true, false, false, false),
];

#[test]
fn every_handle_resizes_its_own_edges() {
    for l in layouts() {
        for (hx, hy, left, top, right, bottom) in HANDLES {
            let mut s = scene(&l);
            let rig = &mut s.rig;
            rig.click(150.0, 130.0, NONE);
            assert_eq!(selection_handles(&rig.view).len(), 8);
            // Grabbed two pixels off the handle, dragged 30 right and 20 down the screen.
            assert_eq!(rig.down(hx + 2.0, hy - 1.0, NONE), []);
            assert_eq!(rig.mv(hx + 10.0, hy + 10.0, NONE), []);
            assert_eq!(rig.state.editing(), [s.rect.clone()]);
            assert!(!rig.preview.is_empty());
            let got = rig.up(hx + 30.0, hy + 20.0, NONE);
            let want = l.rect(
                if left { 130.0 } else { 100.0 },
                if top { 120.0 } else { 100.0 },
                if right { 230.0 } else { 200.0 },
                if bottom { 180.0 } else { 160.0 },
            );
            assert_eq!(
                got,
                [modify(&s.rect, Geometry::Rect(want))],
                "{} handle at ({hx}, {hy})",
                l.name
            );
            assert_eq!(rig.selection(), [s.rect.clone()]);
        }
    }
}

#[test]
fn shift_keeps_proportions_from_every_handle() {
    for l in layouts() {
        // The rectangle is 100 x 60 on screen. Each case: handle, where it is dragged to, and
        // the screen box that results.
        let cases = [
            // Corners scale about the opposite corner by the larger of the two ratios.
            ((200.0, 160.0), (250.0, 175.0), (100.0, 100.0, 250.0, 190.0)),
            ((200.0, 160.0), (300.0, 170.0), (100.0, 100.0, 300.0, 220.0)),
            ((100.0, 100.0), (150.0, 115.0), (125.0, 115.0, 200.0, 160.0)),
            ((200.0, 100.0), (250.0, 100.0), (100.0, 70.0, 250.0, 160.0)),
            ((100.0, 160.0), (75.0, 160.0), (75.0, 100.0, 200.0, 175.0)),
            // Edges scale about the opposite edge and grow evenly to both sides.
            ((200.0, 130.0), (250.0, 300.0), (100.0, 85.0, 250.0, 175.0)),
            ((100.0, 130.0), (150.0, 0.0), (150.0, 115.0, 200.0, 145.0)),
            ((150.0, 100.0), (0.0, 40.0), (50.0, 40.0, 250.0, 160.0)),
            ((150.0, 160.0), (400.0, 130.0), (125.0, 100.0, 175.0, 130.0)),
        ];
        for (handle, to, want) in cases {
            let mut s = scene(&l);
            let rig = &mut s.rig;
            rig.click(150.0, 130.0, NONE);
            let got = rig.drag(handle, to, SHIFT);
            let want = l.rect(want.0, want.1, want.2, want.3);
            assert_eq!(
                got,
                [modify(&s.rect, Geometry::Rect(want))],
                "{} handle {handle:?} to {to:?}",
                l.name
            );
        }
    }
}

#[test]
fn a_handle_dragged_through_the_shape_flips_it_and_never_empties_it() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        rig.click(150.0, 130.0, NONE);
        // The bottom-right screen corner, dragged past the top-left one.
        let got = rig.drag((200.0, 160.0), (60.0, 70.0), NONE);
        assert_eq!(
            got,
            [modify(
                &s.rect,
                Geometry::Rect(l.rect(60.0, 70.0, 100.0, 100.0))
            )],
            "{}",
            l.name
        );
        // The right screen edge dragged exactly onto the left one would leave no width: the
        // last shape that had some is kept.
        rig.down(100.0, 85.0, NONE);
        rig.mv(80.0, 85.0, NONE);
        let got = rig.up(60.0, 85.0, NONE);
        assert_eq!(
            got,
            [modify(
                &s.rect,
                Geometry::Rect(l.rect(60.0, 70.0, 80.0, 100.0))
            )],
            "{}",
            l.name
        );
        // A press on a handle with no drag changes nothing; nor does Esc in the middle.
        assert_eq!(rig.click(80.0, 100.0, NONE), []);
        rig.down(80.0, 100.0, NONE);
        rig.mv(200.0, 200.0, NONE);
        assert_eq!(rig.key(Key::Escape, NONE), []);
        assert_eq!(rig.up(200.0, 200.0, NONE), []);
        assert_eq!(
            rig.geometry(&s.rect),
            Geometry::Rect(l.rect(60.0, 70.0, 80.0, 100.0))
        );
    }
}

#[test]
fn pen_strokes_scale_with_their_box() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        rig.click(280.0, 280.0, NONE);
        assert_eq!(rig.selection(), [s.ink.clone()]);
        assert_eq!(selection_handles(&rig.view).len(), 8);
        // The bottom-right screen corner of the box, dragged out to twice the size.
        let got = rig.drag((300.0, 300.0), (340.0, 340.0), NONE);
        assert_eq!(
            got,
            [modify(
                &s.ink,
                Geometry::Ink(vec![
                    vec![l.at(260.0, 260.0), l.at(340.0, 340.0)],
                    vec![l.at(300.0, 260.0)],
                ])
            )],
            "{}",
            l.name
        );
        // With Shift from an edge: half the width, so half the height about the middle.
        let got = rig.drag((340.0, 300.0), (300.0, 300.0), SHIFT);
        assert_eq!(
            got,
            [modify(
                &s.ink,
                Geometry::Ink(vec![
                    vec![l.at(260.0, 280.0), l.at(300.0, 320.0)],
                    vec![l.at(280.0, 280.0)],
                ])
            )],
            "{}",
            l.name
        );
    }
}

#[test]
fn vertices_of_lines_and_polygons_drag_one_at_a_time() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        rig.click(300.0, 130.0, NONE);
        let handles = selection_handles(&rig.view);
        assert_eq!(handles.len(), 2);
        assert_eq!(handles[1].kind, HandleKind::Vertex(1));
        assert_eq!(handles[1].at, l.at(350.0, 160.0));
        // The end of the line.
        let got = rig.drag((350.0, 160.0), (380.0, 200.0), NONE);
        assert_eq!(
            got,
            [modify(
                &s.line,
                line(l.at(250.0, 100.0), l.at(380.0, 200.0))
            )],
            "{}",
            l.name
        );
        // With Shift it steps round the other end: nearly level on screen becomes level.
        let got = rig.drag((380.0, 200.0), (380.0, 105.0), SHIFT);
        assert_eq!(
            got,
            [modify(
                &s.line,
                line(l.at(250.0, 100.0), l.at(380.0, 100.0))
            )],
            "{}",
            l.name
        );
        // The start, stepped round the end: (-70, 60) from (380, 100) is the diagonal (-65, 65).
        let got = rig.drag((250.0, 100.0), (310.0, 160.0), SHIFT);
        assert_eq!(
            got,
            [modify(
                &s.line,
                line(l.at(315.0, 165.0), l.at(380.0, 100.0))
            )],
            "{}",
            l.name
        );

        // A polygon corner.
        rig.click(150.0, 290.0, NONE);
        assert_eq!(selection_handles(&rig.view).len(), 3);
        let got = rig.drag((150.0, 330.0), (170.0, 350.0), NONE);
        assert_eq!(
            got,
            [modify(
                &s.tri,
                Geometry::Points(vec![
                    l.at(100.0, 250.0),
                    l.at(200.0, 250.0),
                    l.at(170.0, 350.0)
                ])
            )],
            "{}",
            l.name
        );
        // The first corner with Shift steps round the second: (-110, 50) from (200, 250) is
        // the diagonal (-80, 80).
        let got = rig.drag((100.0, 250.0), (90.0, 300.0), SHIFT);
        assert_eq!(
            got,
            [modify(
                &s.tri,
                Geometry::Points(vec![
                    l.at(120.0, 330.0),
                    l.at(200.0, 250.0),
                    l.at(170.0, 350.0)
                ])
            )],
            "{}",
            l.name
        );
        // A later corner steps round the one before it: upright from (200, 250).
        let got = rig.drag((170.0, 350.0), (205.0, 400.0), SHIFT);
        assert_eq!(
            got,
            [modify(
                &s.tri,
                Geometry::Points(vec![
                    l.at(120.0, 330.0),
                    l.at(200.0, 250.0),
                    l.at(200.0, 400.0)
                ])
            )],
            "{}",
            l.name
        );
        // A snap point is where the vertex goes.
        rig.down(200.0, 400.0, NONE);
        let snapped = rig
            .pointer(230.0, 380.0, NONE)
            .with_snap(l.at(240.0, 384.0));
        rig.send(ToolInput::PointerMove(snapped));
        let got = rig.send(ToolInput::PointerUp(snapped));
        assert_eq!(
            got,
            [modify(
                &s.tri,
                Geometry::Points(vec![
                    l.at(120.0, 330.0),
                    l.at(200.0, 250.0),
                    l.at(240.0, 384.0)
                ])
            )]
        );
    }
}

#[test]
fn arrow_keys_nudge_the_way_they_point_on_screen() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        rig.click(150.0, 130.0, NONE);
        rig.click(300.0, 130.0, SHIFT);
        let fine = Modifiers::CTRL;
        // (key, modifiers, screen direction, distance in page units)
        let cases = [
            (Key::Right, NONE, (1.0, 0.0), NUDGE),
            (Key::Left, NONE, (-1.0, 0.0), NUDGE),
            (Key::Up, NONE, (0.0, -1.0), NUDGE),
            (Key::Down, NONE, (0.0, 1.0), NUDGE),
            (Key::Right, SHIFT, (1.0, 0.0), NUDGE_LARGE),
            (Key::Up, SHIFT, (0.0, -1.0), NUDGE_LARGE),
            (Key::Left, fine, (-1.0, 0.0), NUDGE_FINE),
            (Key::Down, fine, (0.0, 1.0), NUDGE_FINE),
        ];
        for (key, m, dir, distance) in cases {
            // `distance` page units are `distance / px` screen pixels.
            let by: Vec2 = l.vec(dir.0 * distance / l.px, dir.1 * distance / l.px);
            assert_eq!(by.hypot(), distance);
            let Geometry::Rect(r) = rig.geometry(&s.rect) else {
                panic!()
            };
            let Geometry::Line { a, b } = rig.geometry(&s.line) else {
                panic!()
            };
            let before_screen = l.screen(r.center());
            let got = rig.key(key, m);
            assert_eq!(
                got,
                [
                    modify(&s.rect, Geometry::Rect(r + by)),
                    modify(&s.line, line(a + by, b + by))
                ],
                "{} {key:?}",
                l.name
            );
            // Seen on screen, it went the way the arrow points.
            let Geometry::Rect(r) = rig.geometry(&s.rect) else {
                panic!()
            };
            let went = l.screen(r.center()) - before_screen;
            assert!(
                (went.x - dir.0 * distance / l.px).abs() < 1e-9
                    && (went.y - dir.1 * distance / l.px).abs() < 1e-9,
                "{} {key:?}: {went:?}",
                l.name
            );
        }
        // Not in the middle of a drag.
        rig.down(150.0, 130.0, NONE);
        rig.mv(180.0, 150.0, NONE);
        assert_eq!(rig.key(Key::Right, NONE), []);
        assert_eq!(rig.key(Key::Delete, NONE), []);
        rig.key(Key::Escape, NONE);
        // Nothing selected, nothing nudged.
        rig.click(420.0, 420.0, NONE);
        assert_eq!(rig.key(Key::Right, NONE), []);
    }
}

#[test]
fn delete_enter_and_double_click() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        assert_eq!(rig.key(Key::Delete, NONE), [], "nothing selected");
        rig.click(150.0, 130.0, NONE);
        rig.click(300.0, 130.0, SHIFT);
        // Enter opens text only for a single markup.
        assert_eq!(rig.key(Key::Enter, NONE), []);
        assert_eq!(
            rig.key(Key::Delete, NONE),
            [Intent::Delete(vec![s.rect.clone(), s.line.clone()])],
            "{}",
            l.name
        );
        assert!(rig.selection().is_empty());
        assert_eq!(rig.on_page().len(), 3);
        rig.click(150.0, 290.0, NONE);
        assert_eq!(
            rig.key(Key::Enter, NONE),
            [Intent::RequestTextEdit(s.tri.clone())]
        );
        assert_eq!(
            rig.key(Key::Backspace, NONE),
            [Intent::Delete(vec![s.tri.clone()])]
        );

        // A double-click on a note selects it and opens its text, whichever way the events
        // of the double-click are ordered.
        assert_eq!(rig.click(380.0, 300.0, NONE), [select(&[&s.note])]);
        assert_eq!(rig.down(380.0, 300.0, NONE), []);
        assert_eq!(
            rig.double(380.0, 300.0),
            [Intent::RequestTextEdit(s.note.clone())],
            "{}",
            l.name
        );
        assert_eq!(rig.up(380.0, 300.0, NONE), []);
        assert_eq!(rig.view.text_edit, Some(s.note.clone()));
        rig.click(420.0, 420.0, NONE);
        assert_eq!(
            rig.double(380.0, 300.0),
            [select(&[&s.note]), Intent::RequestTextEdit(s.note.clone())]
        );
        // On bare paper it does nothing.
        assert_eq!(rig.double(420.0, 100.0), []);
        // A note has no handles: it only moves.
        assert!(selection_handles(&rig.view).is_empty());
        let got = rig.drag((380.0, 300.0), (400.0, 330.0), NONE);
        assert_eq!(
            got,
            [modify(
                &s.note,
                Geometry::Rect(l.rect(391.0, 321.0, 409.0, 339.0))
            )]
        );
    }
}

#[test]
fn the_cursor_shows_what_a_press_would_do() {
    for l in layouts() {
        let mut s = scene(&l);
        assert_eq!(cursor_at(&s.rig, 420.0, 420.0), Cursor::Default);
        assert_eq!(cursor_at(&s.rig, 150.0, 130.0), Cursor::Move);
        // No handles until it is selected.
        assert_eq!(cursor_at(&s.rig, 200.0, 160.0), Cursor::Move);
        s.rig.click(150.0, 130.0, NONE);
        // Each handle's cursor follows the screen, whatever the page's rotation.
        let want = [
            Cursor::ResizeNwSe,       // top-left on screen
            Cursor::ResizeVertical,   // top
            Cursor::ResizeNeSw,       // top-right
            Cursor::ResizeHorizontal, // right
            Cursor::ResizeNwSe,       // bottom-right
            Cursor::ResizeVertical,   // bottom
            Cursor::ResizeNeSw,       // bottom-left
            Cursor::ResizeHorizontal, // left
        ];
        for ((hx, hy, ..), want) in HANDLES.iter().zip(want) {
            assert_eq!(cursor_at(&s.rig, *hx, *hy), want, "{} ({hx}, {hy})", l.name);
        }
        assert_eq!(cursor_at(&s.rig, 150.0, 130.0), Cursor::Move);
        // A vertex.
        s.rig.click(300.0, 130.0, NONE);
        assert_eq!(cursor_at(&s.rig, 350.0, 160.0), Cursor::Crosshair);
        // With two selected neither shows its own handles: the box around both does, from
        // (100, 100) to (350, 160) on screen.
        s.rig.click(150.0, 130.0, SHIFT);
        assert_eq!(cursor_at(&s.rig, 200.0, 160.0), Cursor::Move);
        assert_eq!(selection_handles(&s.rig.view).len(), 8);
        assert_eq!(
            cursor_at(&s.rig, 350.0, 160.0),
            Cursor::ResizeNwSe,
            "{}",
            l.name
        );
        assert_eq!(
            cursor_at(&s.rig, 350.0, 130.0),
            Cursor::ResizeHorizontal,
            "{}",
            l.name
        );
        assert_eq!(
            s.rig.view.cursor, None,
            "cursor_at does not apply its intents"
        );
        s.rig
            .send(ToolInput::PointerMove(s.rig.pointer(420.0, 420.0, NONE)));
        assert_eq!(s.rig.view.cursor, Some(Cursor::Default));
    }
}

#[test]
fn a_gesture_ignores_other_pages() {
    for l in layouts() {
        let mut s = scene(&l);
        let rig = &mut s.rig;
        let other = Pointer::new(0, Point::new(300.0, 300.0));
        rig.down(150.0, 130.0, NONE);
        rig.mv(170.0, 150.0, NONE);
        assert_eq!(rig.send(ToolInput::PointerMove(other)), []);
        // Released over the other page: the move ends where it last was on its own page.
        assert_eq!(
            rig.send(ToolInput::PointerUp(other)),
            [modify(
                &s.rect,
                Geometry::Rect(l.rect(120.0, 120.0, 220.0, 180.0))
            )],
            "{}",
            l.name
        );
        // A press on the other page picks what is there, not what is selected here.
        let got = rig.send(ToolInput::PointerDown(Pointer::new(
            0,
            Point::new(0.5, 300.0),
        )));
        let [Intent::Select(ids)] = got.as_slice() else {
            panic!("{got:?}")
        };
        assert_eq!(ids, &[rig.view.annotations[0].id.clone()]);
        rig.send(ToolInput::PointerUp(Pointer::new(
            0,
            Point::new(0.5, 300.0),
        )));
        assert_eq!(rig.view.annotation(&ids[0]).unwrap().page, 0);
    }
}
