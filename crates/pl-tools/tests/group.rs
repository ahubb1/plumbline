//! The Select tool on what M4 added: resizing several markups together, a callout's leader
//! (its handles, and how it follows a move, a resize, a nudge and a copy), opening the text
//! of a text markup, and stamps that keep their proportions. Scripts and expectations are in
//! screen pixels on all eight page layouts.

mod common;

use common::*;
use pl_annot::{
    callout_route, AnnotId, AnnotKind, Annotation, Flags, Geometry, HandleKind, RectHandle,
    StampSpec,
};
use pl_geom::{Point, Rect};
use pl_tools::{group_bounds, selection_handles, AnnotChange, Cursor, Intent, Key, ToolKind};

struct Group {
    rig: Rig,
    square: AnnotId,
    circle: AnnotId,
    line: AnnotId,
    note: AnnotId,
}

/// A rectangle, an ellipse, a line and a note, all selected. On screen the box around them is
/// (100, 100) to (400, 300).
fn group(l: &Layout) -> Group {
    let mut rig = Rig::new(l, ToolKind::Select);
    let o = origin();
    let square = rig.add(Annotation::square(
        PAGE,
        l.rect(100.0, 100.0, 200.0, 160.0),
        &o,
    ));
    let circle = rig.add(Annotation::circle(
        PAGE,
        l.rect(300.0, 200.0, 400.0, 300.0),
        &o,
    ));
    let line = rig.add(Annotation::line(
        PAGE,
        l.at(150.0, 250.0),
        l.at(250.0, 150.0),
        &o,
    ));
    let note = rig.add(Annotation::note(
        PAGE,
        l.rect(291.0, 141.0, 309.0, 159.0),
        "n",
        &o,
    ));
    rig.view.selection = vec![square.clone(), circle.clone(), line.clone(), note.clone()];
    Group {
        rig,
        square,
        circle,
        line,
        note,
    }
}

fn modified(intents: &[Intent]) -> Vec<AnnotId> {
    intents
        .iter()
        .map(|i| match i {
            Intent::Modify { id, .. } => id.clone(),
            other => panic!("expected only changes, got {other:?}"),
        })
        .collect()
}

#[test]
fn several_selected_markups_resize_together() {
    for l in layouts() {
        let mut g = group(&l);
        let bounds = l.rect(100.0, 100.0, 400.0, 300.0);
        assert_eq!(group_bounds(&g.rig.view, PAGE), Some(bounds), "{}", l.name);
        assert_eq!(group_bounds(&g.rig.view, 0), None);
        let handles = selection_handles(&g.rig.view);
        assert_eq!(handles.len(), 8);
        for (h, which) in handles.iter().zip(RectHandle::ALL) {
            assert_eq!(h.kind, HandleKind::Rect(which));
            assert_eq!(h.at, which.position(bounds), "{}", l.name);
        }
        // The box's bottom-right corner on screen resizes along that diagonal.
        g.rig.mv(400.0, 300.0, NONE);
        assert_eq!(g.rig.view.cursor, Some(Cursor::ResizeNwSe), "{}", l.name);
        g.rig.mv(400.0, 200.0, NONE);
        assert_eq!(
            g.rig.view.cursor,
            Some(Cursor::ResizeHorizontal),
            "{}",
            l.name
        );

        // Dragged out to (550, 400): half as big again both ways, about the top-left.
        assert!(g.rig.down(400.0, 300.0, NONE).is_empty());
        assert!(g.rig.state.editing().is_empty(), "not until it moves");
        g.rig.mv(480.0, 350.0, NONE);
        assert_eq!(g.rig.state.editing().len(), 4);
        assert!(!g.rig.preview.is_empty());
        g.rig.mv(550.0, 400.0, NONE);
        let intents = g.rig.up(550.0, 400.0, NONE);
        assert_eq!(
            modified(&intents),
            [
                g.square.clone(),
                g.circle.clone(),
                g.line.clone(),
                g.note.clone()
            ],
            "{}: one change each, one undo step",
            l.name
        );
        assert_rect(
            rect_of(g.rig.get(&g.square)),
            l.rect(100.0, 100.0, 250.0, 190.0),
            l.name,
        );
        assert_rect(
            rect_of(g.rig.get(&g.circle)),
            l.rect(400.0, 250.0, 550.0, 400.0),
            l.name,
        );
        let Geometry::Line { a, b } = g.rig.geometry(&g.line) else {
            panic!("still a line");
        };
        assert_point(a, l.at(175.0, 325.0), l.name);
        assert_point(b, l.at(325.0, 175.0), l.name);
        // The note keeps its size; its middle keeps its place in the box.
        assert_rect(
            rect_of(g.rig.get(&g.note)),
            l.rect(391.0, 166.0, 409.0, 184.0),
            l.name,
        );
        assert_eq!(g.rig.selection().len(), 4, "still selected");
        assert!(!g.rig.state.is_busy() && g.rig.preview.is_empty());
        assert_eq!(
            group_bounds(&g.rig.view, PAGE).map(|r| on_screen(&l, r).width().round()),
            Some(450.0)
        );

        // Shift keeps the box's proportions: 150 px across asks for 1.5, 10 px down for less.
        let mut g = group(&l);
        g.rig.drag((400.0, 300.0), (550.0, 310.0), SHIFT);
        assert_rect(
            rect_of(g.rig.get(&g.square)),
            l.rect(100.0, 100.0, 250.0, 190.0),
            l.name,
        );
        assert_rect(
            rect_of(g.rig.get(&g.circle)),
            l.rect(400.0, 250.0, 550.0, 400.0),
            l.name,
        );

        // The middle of the right-hand side squeezes the width only.
        let mut g = group(&l);
        let intents = g.rig.drag((400.0, 200.0), (250.0, 230.0), NONE);
        assert_eq!(intents.len(), 4);
        assert_rect(
            rect_of(g.rig.get(&g.square)),
            l.rect(100.0, 100.0, 150.0, 160.0),
            l.name,
        );
        assert_rect(
            rect_of(g.rig.get(&g.circle)),
            l.rect(200.0, 200.0, 250.0, 300.0),
            l.name,
        );
        assert_rect(
            rect_of(g.rig.get(&g.note)),
            l.rect(191.0, 141.0, 209.0, 159.0),
            l.name,
        );
    }
}

#[test]
fn a_group_resize_can_be_cancelled_and_leaves_locked_markups_alone() {
    for l in layouts() {
        // Esc mid-drag; a press that does not travel.
        let mut g = group(&l);
        let before = g.rig.view.annotations.clone();
        g.rig.down(400.0, 300.0, NONE);
        g.rig.mv(500.0, 380.0, NONE);
        assert!(g.rig.state.is_busy());
        g.rig.key(Key::Escape, NONE);
        assert!(!g.rig.state.is_busy() && g.rig.preview.is_empty());
        assert!(g.rig.up(500.0, 380.0, NONE).is_empty());
        assert!(g.rig.click(400.0, 300.0, NONE).is_empty(), "{}", l.name);
        g.rig.down(400.0, 300.0, NONE);
        g.rig.mv(401.0, 301.0, NONE);
        assert!(g.rig.up(401.0, 301.0, NONE).is_empty());
        assert_eq!(g.rig.view.annotations, before, "{}", l.name);
        assert_eq!(g.rig.selection().len(), 4);

        // A locked markup is not part of the box and does not change.
        let mut g = group(&l);
        let lock = |a: &mut Annotation| a.meta.flags = a.meta.flags.with(Flags::LOCKED, true);
        lock(
            g.rig
                .view
                .annotations
                .iter_mut()
                .find(|a| a.id == g.circle)
                .unwrap(),
        );
        // Without the ellipse the box ends at the note's right edge and the line's bottom.
        let bounds = l.rect(100.0, 100.0, 309.0, 250.0);
        assert_eq!(group_bounds(&g.rig.view, PAGE), Some(bounds), "{}", l.name);
        let circle = g.rig.geometry(&g.circle);
        let intents = g.rig.drag((309.0, 250.0), (518.0, 400.0), NONE);
        assert_eq!(
            modified(&intents),
            [g.square.clone(), g.line.clone(), g.note.clone()]
        );
        assert_eq!(g.rig.geometry(&g.circle), circle, "{}", l.name);
        assert_rect(
            rect_of(g.rig.get(&g.square)),
            l.rect(100.0, 100.0, 300.0, 220.0),
            l.name,
        );

        // One movable markup among the selected is not a group.
        let mut g = group(&l);
        for a in g
            .rig
            .view
            .annotations
            .iter_mut()
            .filter(|a| a.id != g.square)
        {
            lock(a);
        }
        assert_eq!(group_bounds(&g.rig.view, PAGE), None);
        assert!(selection_handles(&g.rig.view).is_empty(), "{}", l.name);
        // Nor is a single selected markup: it shows its own handles.
        g.rig.view.selection = vec![g.square.clone()];
        assert_eq!(group_bounds(&g.rig.view, PAGE), None);
        assert_eq!(selection_handles(&g.rig.view).len(), 8);
    }
}

// ---------------------------------------------------------------------------------------
// Callouts
// ---------------------------------------------------------------------------------------

/// A callout whose box is (200, 150) to (320, 200) on screen, with its tip at (100, 300), a
/// knee at (150, 240) and the leader meeting the middle of the box's left side.
fn callout(l: &Layout) -> (Rig, AnnotId) {
    let mut rig = Rig::new(l, ToolKind::Select);
    let rect = l.rect(200.0, 150.0, 320.0, 200.0);
    let mut c = Annotation::callout(PAGE, rect, l.at(100.0, 300.0), "Detail", &origin());
    c.set_rotation(l.rotation().degrees());
    c.free_text.as_mut().unwrap().callout =
        vec![l.at(100.0, 300.0), l.at(150.0, 240.0), l.at(200.0, 175.0)];
    let id = rig.add(c);
    rig.view.selection = vec![id.clone()];
    (rig, id)
}

fn leader(rig: &Rig, id: &AnnotId) -> Vec<Point> {
    rig.get(id).free_text.as_ref().unwrap().callout.clone()
}

#[test]
fn the_tip_and_the_knee_of_a_callout_are_dragged_by_their_handles() {
    for l in layouts() {
        let (mut rig, id) = callout(&l);
        let rect = l.rect(200.0, 150.0, 320.0, 200.0);
        let tip = l.at(100.0, 300.0);
        let handles = selection_handles(&rig.view);
        assert_eq!(
            handles.len(),
            10,
            "{}: eight for the box, the tip, the knee",
            l.name
        );
        assert_eq!(
            (handles[8].kind, handles[8].at),
            (HandleKind::Vertex(0), tip)
        );
        assert_eq!(
            (handles[9].kind, handles[9].at),
            (HandleKind::Vertex(1), l.at(150.0, 240.0))
        );
        rig.mv(150.0, 240.0, NONE);
        assert_eq!(rig.view.cursor, Some(Cursor::Crosshair));

        // The knee, dragged to below the box: the leader now meets the bottom side.
        rig.down(150.0, 240.0, NONE);
        rig.mv(200.0, 250.0, NONE);
        assert_eq!(rig.state.editing(), std::slice::from_ref(&id));
        assert!(!rig.preview.is_empty());
        rig.mv(260.0, 260.0, NONE);
        let intents = rig.up(260.0, 260.0, NONE);
        let want = vec![tip, l.at(260.0, 260.0), l.at(260.0, 200.0)];
        let [Intent::Modify {
            id: changed,
            change: AnnotChange::FreeText(text),
        }] = &intents[..]
        else {
            panic!(
                "{}: expected one change to the text side, got {intents:?}",
                l.name
            );
        };
        assert_eq!(*changed, id);
        assert_eq!(text.callout, want, "{}", l.name);
        assert_eq!(leader(&rig, &id), want);
        assert_eq!(rig.geometry(&id), Geometry::Rect(rect), "the box stays");
        assert_eq!(rig.get(&id).meta.contents.as_deref(), Some("Detail"));

        // Shift holds the knee to 45 degree steps from the tip: level with it on screen.
        rig.drag((260.0, 260.0), (190.0, 310.0), SHIFT);
        assert_eq!(
            leader(&rig, &id),
            [tip, l.at(190.0, 300.0), l.at(260.0, 200.0)],
            "{}",
            l.name
        );

        // The tip: the leader is routed again from where it lands.
        let (mut rig, id) = callout(&l);
        let intents = rig.drag((100.0, 300.0), (120.0, 330.0), NONE);
        assert_eq!(intents.len(), 1);
        let route = callout_route(rect, l.at(120.0, 330.0));
        assert_eq!(leader(&rig, &id), route, "{}", l.name);
        assert_eq!(route[0], l.at(120.0, 330.0));
        assert_eq!(rig.geometry(&id), Geometry::Rect(rect));

        // Esc leaves the leader alone.
        let (mut rig, id) = callout(&l);
        let before = leader(&rig, &id);
        rig.down(150.0, 240.0, NONE);
        rig.mv(260.0, 260.0, NONE);
        rig.key(Key::Escape, NONE);
        assert!(rig.up(260.0, 260.0, NONE).is_empty());
        assert_eq!(leader(&rig, &id), before);
    }
}

#[test]
fn a_callouts_leader_follows_a_move_a_nudge_a_copy_and_a_resize() {
    for l in layouts() {
        let rect = l.rect(200.0, 150.0, 320.0, 200.0);
        let by = l.vec(30.0, 20.0);

        // Dragged by its box.
        let (mut rig, id) = callout(&l);
        let before = leader(&rig, &id);
        let intents = rig.drag((260.0, 175.0), (290.0, 195.0), NONE);
        assert_eq!(
            modified(&intents),
            [id.clone(), id.clone()],
            "{}: box and leader",
            l.name
        );
        assert_eq!(
            rig.geometry(&id),
            Geometry::Rect(l.rect(230.0, 170.0, 350.0, 220.0))
        );
        let moved: Vec<Point> = before.iter().map(|p| *p + by).collect();
        assert_eq!(leader(&rig, &id), moved, "{}", l.name);

        // Dragged by its leader.
        let (mut rig, id) = callout(&l);
        rig.drag((125.0, 270.0), (155.0, 290.0), NONE);
        assert_eq!(leader(&rig, &id), moved, "{}", l.name);

        // Nudged: one unit the way the arrow points on screen.
        let (mut rig, id) = callout(&l);
        let intents = rig.key(Key::Right, NONE);
        assert_eq!(intents.len(), 2);
        let step = match rig.geometry(&id) {
            Geometry::Rect(r) => r.origin() - rect.origin(),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            pl_tools::to_display(l.rotation(), step),
            pl_geom::Vec2::new(1.0, 0.0)
        );
        let nudged: Vec<Point> = before.iter().map(|p| *p + step).collect();
        assert_eq!(leader(&rig, &id), nudged, "{}", l.name);

        // Ctrl leaves the original and makes a copy with its own leader.
        let (mut rig, id) = callout(&l);
        rig.drag((260.0, 175.0), (290.0, 195.0), CTRL);
        assert_eq!(leader(&rig, &id), before, "{}: the original stays", l.name);
        let copy = rig.selection();
        assert_eq!(copy.len(), 1);
        assert_ne!(copy[0], id);
        assert_eq!(leader(&rig, &copy[0]), moved, "{}", l.name);
        assert_eq!(rig.get(&copy[0]).kind, AnnotKind::FreeText);

        // The box resized by its bottom-right corner on screen: the leader is routed to the
        // new box.
        let (mut rig, id) = callout(&l);
        rig.drag((320.0, 200.0), (360.0, 230.0), NONE);
        let grown = l.rect(200.0, 150.0, 360.0, 230.0);
        assert_eq!(rig.geometry(&id), Geometry::Rect(grown), "{}", l.name);
        assert_eq!(
            leader(&rig, &id),
            callout_route(grown, l.at(100.0, 300.0)),
            "{}",
            l.name
        );

        // In a group it is scaled, leader and all.
        let (mut rig, id) = callout(&l);
        let other = rig.add(Annotation::square(
            PAGE,
            l.rect(100.0, 100.0, 150.0, 140.0),
            &origin(),
        ));
        rig.view.selection = vec![id.clone(), other];
        // The box around both is (100, 100) to (320, 300) on screen: the tip is its bottom.
        assert_eq!(
            group_bounds(&rig.view, PAGE),
            Some(l.rect(100.0, 100.0, 320.0, 300.0))
        );
        rig.drag((320.0, 300.0), (540.0, 500.0), NONE);
        assert_rect(
            rect_of(rig.get(&id)),
            l.rect(300.0, 200.0, 540.0, 300.0),
            l.name,
        );
        let scaled = leader(&rig, &id);
        assert_point(scaled[0], l.at(100.0, 500.0), l.name);
        assert_point(scaled[1], l.at(200.0, 380.0), l.name);
        assert_point(scaled[2], l.at(300.0, 250.0), l.name);
    }
}

// ---------------------------------------------------------------------------------------
// Text and stamps
// ---------------------------------------------------------------------------------------

#[test]
fn a_double_click_or_enter_opens_the_text_of_a_text_markup() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Select);
        let rect = l.rect(200.0, 150.0, 320.0, 200.0);
        let text = rig.add(Annotation::free_text(PAGE, rect, "Hello", &origin()));
        let type_rect = l.rect(200.0, 250.0, 320.0, 270.0);
        let typed = rig.add(Annotation::typewriter(PAGE, type_rect, "Typed", &origin()));
        let (callout_rig, _) = callout(&l);
        // A callout whose box is further down; its leader still runs from (100, 300).
        let mut lead = callout_rig.view.annotations[0].clone();
        lead.geometry = Geometry::Rect(l.rect(200.0, 300.0, 320.0, 350.0));
        let lead_id = rig.add(lead);

        let intents = rig.double(260.0, 175.0);
        assert_eq!(
            intents,
            [
                Intent::Select(vec![text.clone()]),
                Intent::RequestTextEdit(text.clone())
            ],
            "{}",
            l.name
        );
        assert_eq!(rig.view.text_edit, Some(text.clone()));
        assert!(!rig.state.is_busy());
        // The whole sequence of a real double-click ends the same way and moves nothing.
        rig.view.text_edit = None;
        rig.click(260.0, 260.0, NONE);
        rig.click(260.0, 260.0, NONE);
        rig.double(260.0, 260.0);
        assert_eq!(rig.view.text_edit, Some(typed.clone()), "{}", l.name);
        assert_eq!(rig.selection(), std::slice::from_ref(&typed));
        assert_eq!(rig.geometry(&typed), Geometry::Rect(type_rect));
        // On a callout's leader too.
        rig.double(125.0, 270.0);
        assert_eq!(rig.view.text_edit, Some(lead_id.clone()), "{}", l.name);
        // Enter, with one markup selected.
        rig.view.text_edit = None;
        rig.click(260.0, 175.0, NONE);
        assert_eq!(
            rig.key(Key::Enter, NONE),
            [Intent::RequestTextEdit(text.clone())]
        );
        // The application then makes the change itself.
        assert!(rig
            .view
            .change(&text, AnnotChange::Contents("Changed".into())));
        assert_eq!(rig.get(&text).meta.contents.as_deref(), Some("Changed"));
        // Text settings go through the same door, and only onto a text markup.
        let mut settings = rig.get(&text).free_text.clone().unwrap();
        settings.size = 20.0;
        assert!(rig
            .view
            .change(&text, AnnotChange::FreeText(settings.clone())));
        assert_eq!(rig.get(&text).free_text.as_ref().unwrap().size, 20.0);
        let square = rig.add(Annotation::square(
            PAGE,
            l.rect(20.0, 20.0, 60.0, 60.0),
            &origin(),
        ));
        assert!(!rig
            .view
            .change(&square, AnnotChange::FreeText(settings.clone())));
        settings.size = f64::NAN;
        assert!(!rig.view.change(&text, AnnotChange::FreeText(settings)));
    }
}

#[test]
fn a_stamp_keeps_its_proportions_unless_shift_is_held() {
    for l in layouts() {
        let place = |rig: &mut Rig| {
            let rect = l.rect(100.0, 100.0, 260.0, 140.0);
            let mut s = Annotation::stamp(PAGE, rect, StampSpec::draft(), &origin());
            s.set_rotation(l.rotation().degrees());
            let id = rig.add(s);
            rig.view.selection = vec![id.clone()];
            id
        };
        let mut rig = Rig::new(&l, ToolKind::Select);
        let id = place(&mut rig);
        assert_eq!(selection_handles(&rig.view).len(), 8);
        // 160 px more across asks for twice the size, 10 px more down for less: twice.
        let intents = rig.drag((260.0, 140.0), (420.0, 150.0), NONE);
        assert_eq!(intents.len(), 1);
        assert_rect(
            rect_of(rig.get(&id)),
            l.rect(100.0, 100.0, 420.0, 180.0),
            l.name,
        );
        assert!(rig.get(&id).is_modelled());

        // Shift lets it stretch.
        let mut rig = Rig::new(&l, ToolKind::Select);
        let id = place(&mut rig);
        rig.drag((260.0, 140.0), (420.0, 150.0), SHIFT);
        assert_eq!(
            rig.geometry(&id),
            Geometry::Rect(l.rect(100.0, 100.0, 420.0, 150.0)),
            "{}",
            l.name
        );

        // A plain rectangle is the other way round, as before.
        let mut rig = Rig::new(&l, ToolKind::Select);
        let rect = Rect::from_points(l.at(100.0, 100.0), l.at(260.0, 140.0));
        let id = rig.add(Annotation::square(PAGE, rect, &origin()));
        rig.view.selection = vec![id.clone()];
        rig.drag((260.0, 140.0), (420.0, 150.0), NONE);
        assert_eq!(
            rig.geometry(&id),
            Geometry::Rect(l.rect(100.0, 100.0, 420.0, 150.0))
        );
    }
}
