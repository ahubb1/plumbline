//! Tests for what a review of the M4 and M5 work found in the window: keys that must stay
//! with the text field being typed into, Escape and Tab in text fields, undo steps that are
//! one per user action, an XFDF import that leaves out only what it cannot take, and a list
//! that stays quick with a large selection.

mod support;

use std::time::{Duration, Instant};

use egui::Key;
use egui_kittest::kittest::{by, NodeT, Queryable};
use pl_app::{Command, ScriptedDialogs, Tool};
use pl_doc::pl_annot::{AnnotId, AnnotKind, Annotation, Origin, PdfDate};
use pl_doc::pl_geom::{Point, Rect};
use pl_doc::pl_takeoff::{Builtin, ColumnId, GroupKey, SortKey};
use support::*;

fn origin(author: &str) -> Origin {
    Origin::new(author, PdfDate::from_unix(1_700_000_000))
}

fn square(x: f64, y: f64, subject: &str, comment: &str) -> Annotation {
    let mut a = Annotation::square(0, Rect::new(x, y, x + 60.0, y + 40.0), &origin("Ada"));
    a.meta.subject = Some(subject.to_owned());
    a.meta.contents = (!comment.is_empty()).then(|| comment.to_owned());
    a
}

fn sample() -> Vec<Annotation> {
    vec![
        square(100.0, 600.0, "Beam 10", "Beam B10 over door"),
        square(200.0, 600.0, "Column", "Column C1 grid 4"),
        square(100.0, 500.0, "Beam 2", ""),
    ]
}

fn show_columns(h: &mut App, columns: &[Builtin]) {
    let wanted: Vec<ColumnId> = columns.iter().map(|b| ColumnId::Builtin(*b)).collect();
    h.state_mut().edit_list_layout(|layout| {
        for state in &mut layout.columns {
            state.visible = wanted.contains(&state.column);
        }
        for column in wanted.iter().rev() {
            let first = layout.columns.first().map(|c| c.column.clone());
            layout.move_column(column, first.as_ref());
        }
    });
    h.run();
}

fn list_app(markups: Vec<Annotation>, dialogs: ScriptedDialogs) -> App {
    let mut h = mock_app_with(1, 0.04, None, dialogs);
    assert!(h.state_mut().add_markups(markups));
    h.state_mut().set_markups_list_open(true);
    settle(&mut h);
    h
}

fn undo_len(h: &App) -> usize {
    h.state()
        .active_document()
        .and_then(|d| d.editor())
        .map_or(0, |e| e.undo_len())
}

/// What the keys below must leave alone.
#[derive(Debug, PartialEq)]
struct Seen {
    tool: Tool,
    snap: bool,
    selection: Vec<String>,
    markups: Vec<Annotation>,
    undo: usize,
}

fn see(h: &App) -> Seen {
    Seen {
        tool: h.state().tool(),
        snap: h.state().snap_enabled(),
        selection: selected_ids(h),
        markups: markups(h),
        undo: undo_len(h),
    }
}

/// Real key presses (not text events): a tool letter, the snapping key, Delete, Backspace,
/// the arrows and undo. With a text field focused every one of them is the field's.
fn press_the_keys_a_tool_would_take(h: &mut App) {
    for key in [
        Key::L,
        Key::S,
        Key::R,
        Key::V,
        Key::Delete,
        Key::Backspace,
        Key::ArrowLeft,
        Key::ArrowRight,
        Key::ArrowUp,
        Key::ArrowDown,
    ] {
        h.key_press(key);
        h.run();
    }
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    h.key_press_modifiers(CTRL, Key::D);
    h.run();
}

#[test]
fn keys_pressed_while_typing_stay_with_the_text_field() {
    // ---- the in-place editor of a text box ------------------------------------------------
    let mut h = mock_app(1);
    let rect = draw_rect(&mut h, 0, Rect::new(300.0, 620.0, 380.0, 670.0));
    assert_eq!(selected_ids(&h), [rect.id.as_str().to_owned()]);
    set_tool(&mut h, Tool::TextBox);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 680.0),
        Point::new(250.0, 600.0),
    );
    assert!(h.state().text_edit_active());
    h.run();
    assert!(h.get_by_label("Markup text").is_focused());
    type_into(&mut h, "Markup text", "slab");
    let before = see(&h);
    assert_eq!(before.tool, Tool::TextBox);
    press_the_keys_a_tool_would_take(&mut h);
    assert_eq!(see(&h), before, "the editor's keys reached the tools");
    assert!(h.state().text_edit_active(), "and the editor is still open");
    assert!(h.get_by_label("Markup text").is_focused());
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    assert_eq!(markups(&h).len(), 2);

    // ---- the note window --------------------------------------------------------------------
    let mut h = mock_app_tofu(1, 0.04);
    let rect = draw_rect(&mut h, 0, Rect::new(300.0, 620.0, 380.0, 670.0));
    set_tool(&mut h, Tool::Note);
    let at = Point::new(300.0, 500.0);
    click_on_page(&mut h, 0, at);
    set_tool(&mut h, Tool::Select);
    let pos = on_screen(&h, 0, at);
    double_click_at(&mut h, pos);
    h.run();
    assert!(h.get_by_label("Note text").is_focused());
    type_into(&mut h, "Note text", "check");
    // (Backspace and Delete edit the note's own text, which is the note's contents; the
    // rest is compared.)
    let apart_from_the_text = |h: &App| {
        let seen = see(h);
        let ids: Vec<AnnotId> = seen.markups.iter().map(|a| a.id.clone()).collect();
        let places: Vec<Rect> = seen.markups.iter().map(rect_of).collect();
        (seen.tool, seen.snap, seen.selection, ids, places)
    };
    let before = apart_from_the_text(&h);
    assert_eq!(before.3.len(), 2);
    press_the_keys_a_tool_would_take(&mut h);
    assert_eq!(
        apart_from_the_text(&h),
        before,
        "the note's keys reached the tools"
    );
    assert!(h.state().note_open().is_some());
    assert!(before.3.contains(&rect.id));

    // ---- the list: a cell being edited, the search box, a filter box -----------------------
    let mut h = list_app(sample(), ScriptedDialogs::default());
    show_columns(&mut h, &[Builtin::Subject, Builtin::Comment]);
    click_label(&mut h, "Subject Beam 10, row 1");
    assert_eq!(selected_ids(&h).len(), 1);
    let cell = h
        .get_by_label("Comment Beam B10 over door, row 1")
        .rect()
        .center();
    double_click_at(&mut h, cell);
    h.run();
    assert!(h.get_by_label("Edit Comment, row 1").is_focused());
    let before = see(&h);
    press_the_keys_a_tool_would_take(&mut h);
    assert_eq!(see(&h), before, "the cell's keys reached the tools");
    assert!(h.get_by_label("Edit Comment, row 1").is_focused());
    h.key_press(Key::Escape);
    h.run();
    assert_eq!(see(&h), before, "Escape only gave the edit up");
    assert!(h.query_by_label("Edit Comment, row 1").is_none());

    h.get_by_label("Search markups").focus();
    h.run();
    assert!(h.get_by_label("Search markups").is_focused());
    let before = see(&h);
    press_the_keys_a_tool_would_take(&mut h);
    assert_eq!(see(&h), before, "the search box's keys reached the tools");
    assert!(h.get_by_label("Search markups").is_focused());
}

#[test]
fn escape_in_a_field_with_nothing_to_cancel_only_leaves_the_field() {
    let mut h = list_app(sample(), ScriptedDialogs::default());
    show_columns(&mut h, &[Builtin::Subject, Builtin::Comment]);
    click_label(&mut h, "Subject Beam 10, row 1");
    // A tool other than Select, a selection and the find bar: everything Escape would
    // otherwise work through.
    set_tool(&mut h, Tool::Rectangle);
    run_command(&mut h, Command::Find);
    h.run();
    let before = see(&h);
    assert_eq!(before.tool, Tool::Rectangle);
    assert_eq!(before.selection.len(), 1);
    assert!(h.state().find_bar_open());

    h.get_by_label("Search markups").focus();
    h.run();
    type_into(&mut h, "Search markups", "Beam");
    assert!(h.get_by_label("Search markups").is_focused());
    h.key_press(Key::Escape);
    h.run();
    assert!(
        !h.get_by_label("Search markups").is_focused(),
        "Escape left the box"
    );
    assert_eq!(see(&h), before, "and changed nothing else");
    assert!(h.state().find_bar_open(), "the find bar is still open");
    assert_eq!(h.state().list_layout().filter.search, "Beam");

    // With no field to leave, Escape works through what is open, as before: the find bar,
    // then the selection, then the tool.
    h.key_press(Key::Escape);
    h.run();
    assert!(!h.state().find_bar_open());
    assert_eq!(selected_ids(&h).len(), 1);
    h.key_press(Key::Escape);
    h.run();
    assert!(selected_ids(&h).is_empty());
    assert_eq!(h.state().tool(), Tool::Rectangle);
    h.key_press(Key::Escape);
    h.run();
    assert_eq!(h.state().tool(), Tool::Select);
}

fn start_text_box(h: &mut App) {
    set_tool(h, Tool::TextBox);
    drag_on_page(h, 0, Point::new(100.0, 680.0), Point::new(250.0, 600.0));
    assert!(h.state().text_edit_active(), "the editor opened");
    h.run();
    assert!(h.get_by_label("Markup text").is_focused());
}

#[test]
fn tab_reaches_done_and_cancel_wherever_the_pointer_rests() {
    // Cancel, from the keyboard, with the pointer resting far from the editor.
    let mut h = mock_app(1);
    start_text_box(&mut h);
    type_into(&mut h, "Markup text", "Not wanted");
    h.hover_at(egui::pos2(900.0, 40.0));
    h.run();
    h.key_press(Key::Tab);
    h.run();
    assert!(
        h.state().text_edit_active(),
        "Tab moved on to the buttons; it did not finish the text"
    );
    assert!(markups(&h).is_empty());
    assert!(h.get_by_label("Done").is_focused());
    h.key_press(Key::Tab);
    h.run();
    assert!(h.state().text_edit_active());
    assert!(h.get_by_label("Cancel").is_focused());
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert!(!h.state().text_edit_active(), "Cancel closed the editor");
    assert!(markups(&h).is_empty(), "and nothing was kept");

    // Done, from the keyboard.
    let mut h = mock_app(1);
    start_text_box(&mut h);
    type_into(&mut h, "Markup text", "Wanted");
    h.key_press(Key::Tab);
    h.run();
    assert!(h.get_by_label("Done").is_focused());
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert!(!h.state().text_edit_active());
    assert_eq!(markups(&h).len(), 1);
    assert_eq!(markups(&h)[0].meta.contents.as_deref(), Some("Wanted"));

    // The focus moving on past the buttons leaves the editor, which keeps the text.
    let mut h = mock_app(1);
    start_text_box(&mut h);
    type_into(&mut h, "Markup text", "Kept on leaving");
    for _ in 0..3 {
        h.key_press(Key::Tab);
        h.run();
    }
    h.run();
    assert!(!h.state().text_edit_active());
    assert_eq!(
        markups(&h)[0].meta.contents.as_deref(),
        Some("Kept on leaving")
    );

    // A click somewhere else still finishes the text, as it always did.
    let mut h = mock_app(1);
    start_text_box(&mut h);
    type_into(&mut h, "Markup text", "Clicked away");
    set_tool(&mut h, Tool::Select);
    if h.state().text_edit_active() {
        click_on_page(&mut h, 0, Point::new(450.0, 200.0));
        h.run();
    }
    assert!(!h.state().text_edit_active());
    assert_eq!(
        markups(&h)[0].meta.contents.as_deref(),
        Some("Clicked away")
    );
}

#[test]
fn a_paste_of_hundreds_of_unsupported_characters_is_cleaned_in_one_go() {
    // (The test font has no CJK glyphs either: missing ones are drawn as boxes.)
    let mut h = mock_app_tofu(1, 0.25);
    start_text_box(&mut h);
    // 600 different CJK characters around two words the font has.
    let mut pasted = String::from("Slab ");
    pasted.extend((0..600u32).filter_map(|i| char::from_u32(0x4E00 + i)));
    pasted.push_str(" edge");
    type_into(&mut h, "Markup text", &pasted);
    h.run();
    assert_eq!(h.state().text_edit_text(), Some("Slab  edge"));
    let message = h.state().text_edit_message().expect("a message");
    assert!(message.contains("U+4E00"), "{message}");
    // Nothing is left for Done to refuse.
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    assert!(!h.state().text_edit_active(), "{:?}", h.state().status());
    assert_eq!(markups(&h)[0].meta.contents.as_deref(), Some("Slab  edge"));
}

#[test]
fn one_drag_of_the_eraser_is_one_undo_step() {
    let mut h = mock_app(1);
    // Two pen markups: one of two strokes (the drag removes one of them), one of a single
    // stroke (the drag removes the markup).
    let o = origin("Ada");
    let line = |x: f64| vec![Point::new(x, 500.0), Point::new(x, 600.0)];
    let two = Annotation::ink(0, vec![line(200.0), line(400.0)], &o);
    let one = Annotation::ink(0, vec![line(260.0)], &o);
    let third = Annotation::ink(0, vec![line(320.0)], &o);
    assert!(h
        .state_mut()
        .add_markups(vec![two.clone(), one.clone(), third.clone()]));
    h.run();
    let before = markups(&h);
    let steps = undo_len(&h);

    set_tool(&mut h, Tool::Eraser);
    // Across all three, in several moves.
    let (from, to) = (
        on_screen(&h, 0, Point::new(180.0, 550.0)),
        on_screen(&h, 0, Point::new(340.0, 550.0)),
    );
    h.hover_at(from);
    h.run();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    for i in 1..=8 {
        h.event(egui::Event::PointerMoved(
            from + (to - from) * (i as f32 / 8.0),
        ));
        h.run();
    }
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.run();
    h.remove_cursor();
    h.run();

    let after = markups(&h);
    assert_eq!(after.len(), 1, "two markups were rubbed out whole");
    assert_eq!(after[0].id, two.id);
    assert!(
        matches!(&after[0].geometry, pl_doc::pl_annot::Geometry::Ink(s) if s.len() == 1),
        "and one stroke of the third"
    );
    assert_eq!(
        undo_len(&h),
        steps + 1,
        "the drag made several changes and one undo step"
    );
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(markups(&h), before, "one undo brings every stroke back");
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    assert_eq!(markups(&h), after);
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();

    // A second drag is a step of its own.
    set_tool(&mut h, Tool::Eraser);
    let (from, to) = (
        on_screen(&h, 0, Point::new(240.0, 520.0)),
        on_screen(&h, 0, Point::new(280.0, 520.0)),
    );
    drag(&mut h, from, to);
    assert_eq!(undo_len(&h), steps + 1);
    let (from, to) = (
        on_screen(&h, 0, Point::new(300.0, 520.0)),
        on_screen(&h, 0, Point::new(340.0, 520.0)),
    );
    drag(&mut h, from, to);
    assert_eq!(undo_len(&h), steps + 2);
    assert_eq!(markups(&h).len(), 1);
}

#[test]
fn delete_rows_is_one_undo_step_and_an_unchanged_subject_is_no_step() {
    let mut h = list_app(sample(), ScriptedDialogs::default());
    show_columns(&mut h, &[Builtin::Subject, Builtin::Comment]);
    // Two replies to the first markup, one of them to the other reply.
    let beam = h.state().markups()[0].id.clone();
    let reply = {
        let parent = h.state().markups()[0].clone();
        let first = Annotation::reply(&parent, "Is this right?", &origin("Ben"));
        let second = Annotation::reply(&first, "Yes", &origin("Ada"));
        let id = first.id.clone();
        assert!(h.state_mut().add_markups(vec![first, second]));
        id
    };
    assert_eq!(h.state().markups()[0].id, beam);
    h.run();
    assert_eq!(h.state_mut().list_lines().len(), 5);

    // A subject cell opened and left as it was: nothing changes.
    let steps = undo_len(&h);
    let stamped = h.state().markups()[2].meta.modified;
    let cell = h.get_by_label("Subject Beam 2, row 5").rect().center();
    double_click_at(&mut h, cell);
    h.run();
    assert!(h.get_by_label("Edit Subject, row 5").is_focused());
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert_eq!(undo_len(&h), steps, "no undo step for an unchanged subject");
    assert_eq!(
        h.state().markups()[2].meta.modified,
        stamped,
        "not re-stamped"
    );
    // Changed, it is one step.
    double_click_at(&mut h, cell);
    h.run();
    type_into(&mut h, "Edit Subject, row 5", " east");
    h.key_press(Key::Enter);
    h.run();
    h.run();
    assert_eq!(undo_len(&h), steps + 1);
    assert_eq!(
        h.state().markups()[2].meta.subject.as_deref(),
        Some("Beam 2 east")
    );

    // The markup "Column", the first reply and the reply to it, selected together.
    click_label(&mut h, "Subject Column, row 4");
    let first_reply = h
        .get_by_label("Comment Is this right?, row 2")
        .rect()
        .center();
    click_at_with(&mut h, first_reply, CTRL);
    let second_reply = h.get_by_label("Comment Yes, row 3").rect().center();
    click_at_with(&mut h, second_reply, CTRL);
    let before = h.state().markups();
    assert_eq!(before.len(), 5);
    let steps = undo_len(&h);
    click(&mut h, "Delete rows");
    h.run();
    let left = h.state().markups();
    assert_eq!(left.len(), 2, "{:?}", h.state().status());
    assert!(left.iter().all(|a| a.id != reply));
    assert_eq!(undo_len(&h), steps + 1, "one step for all the rows");
    assert!(
        h.state().status().is_none_or(|s| !s.contains("could not")),
        "{:?}",
        h.state().status()
    );
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(h.state().markups(), before, "one undo brings them all back");
}

#[test]
fn an_xfdf_import_leaves_out_only_what_it_cannot_take() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("from elsewhere.xfdf");
    std::fs::write(
        &path,
        r##"<?xml version="1.0" encoding="UTF-8"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/"><annots>
  <square page="0" rect="100,100,200,160" name="good-1" color="#FF0000" width="1"/>
  <freetext page="0" rect="100,300,300,360" name="cjk" width="1">
    <defaultappearance>0 0 0 rg /Helv 12 Tf</defaultappearance>
    <contents>&#x65E5;&#x672C;&#x8A9E; text</contents>
  </freetext>
  <square page="0" rect="220,100,320,160" name="dup" color="#00FF00" width="1"/>
  <square page="0" rect="340,100,440,160" name="dup" color="#0000FF" width="1"/>
  <freetext page="0" rect="100,400,300,460" name="good-text" width="1">
    <defaultappearance>1 0 0 rg /Helv 12 Tf</defaultappearance>
    <defaultstyle>font: Helvetica 12pt; color:#0000FF</defaultstyle>
    <contents>Plain words</contents>
  </freetext>
  <text page="0" rect="100,300,118,318" name="orphan" inreplyto="cjk"><contents>about the box</contents></text>
  <square page="0" rect="0,0,10000000,10000000" style="cloudy" name="huge"/>
  <circle page="7" rect="10,10,60,40" name="no-such-page"/>
  <circle page="0" rect="10,10,60,40" name="good-2"/>
</annots></xfdf>"##,
    )
    .unwrap();
    let dialogs = ScriptedDialogs::default().with_imports([path.clone()]);
    let mut h = mock_app_with(1, 0.04, None, dialogs);
    let steps = undo_len(&h);
    let started = Instant::now();
    assert_eq!(h.state_mut().import_xfdf(), 4);
    h.run();
    assert!(started.elapsed() < Duration::from_secs(20));
    let ids: Vec<String> = markups(&h).iter().map(|a| a.id.to_string()).collect();
    assert_eq!(ids, ["good-1", "dup", "good-text", "good-2"]);
    let status = h.state().status().unwrap().to_owned();
    assert!(
        status.contains("Imported 4 markup(s)") && status.contains("5 were left out"),
        "{status}"
    );
    // The first of the two with one name is the one kept.
    let dup = &markups(&h)[1];
    assert_eq!(
        dup.style.stroke.map(|c| c.to_u8()),
        Some([0, 255, 0]),
        "{dup:?}"
    );
    // The text box came in the way Acrobat means it: a red border, blue words.
    let text = &markups(&h)[2];
    assert_eq!(text.kind, AnnotKind::FreeText);
    assert_eq!(text.style.stroke.map(|c| c.to_u8()), Some([255, 0, 0]));
    assert_eq!(text.free_text.as_ref().unwrap().color.to_u8(), [0, 0, 255]);
    // One undo step for the whole import.
    assert_eq!(undo_len(&h), steps + 1);
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert!(markups(&h).is_empty());
}

#[test]
fn every_keyboard_stop_on_a_group_line_has_a_name() {
    let mut h = list_app(sample(), ScriptedDialogs::default());
    show_columns(
        &mut h,
        &[
            Builtin::Subject,
            Builtin::Kind,
            Builtin::Comment,
            Builtin::Author,
        ],
    );
    h.state_mut().edit_list_layout(|layout| {
        layout.group = vec![GroupKey::asc(Builtin::Kind)];
    });
    h.run();
    let lines = h.state_mut().list_lines();
    assert!(lines[0].group.is_some(), "{lines:#?}");
    let heading = h
        .query_all(by().predicate(|n| n.label().is_some_and(|l| l.starts_with("Group Type"))))
        .next()
        .expect("a group heading")
        .rect();
    // Walk the whole window with Tab: no stop on the group's line may be nameless (the cells
    // of the line that hold nothing used to be stops with no name and no role).
    let mut unnamed = Vec::new();
    let mut stops = 0;
    let mut on_group_line = false;
    for _ in 0..160 {
        h.key_press(Key::Tab);
        h.step();
        h.step();
        for node in h.query_all(by().predicate(|n| n.is_focused())) {
            stops += 1;
            let ak = node.accesskit_node();
            let label = ak.label().unwrap_or_default();
            on_group_line |= label.starts_with("Group Type");
            let at = node.rect().center();
            let on_the_line = at.y > heading.top() && at.y < heading.bottom();
            if on_the_line && label.trim().is_empty() && ak.labelled_by().next().is_none() {
                unnamed.push(format!("{node:?} at {at:?}"));
            }
        }
    }
    assert!(stops > 20, "the walk moved the focus ({stops} stops)");
    assert!(on_group_line, "the walk reached the group heading");
    assert!(
        unnamed.is_empty(),
        "unnamed keyboard stops on the group line: {unnamed:#?}"
    );
    // A click on a cell of the line that is not a stop still opens and closes the group.
    let heading = h
        .query_all(by().predicate(|n| n.label().is_some_and(|l| l.starts_with("Group Type"))))
        .next()
        .expect("a group heading")
        .rect();
    let before = h.state_mut().list_lines().len();
    click_at(
        &mut h,
        egui::pos2(heading.right() + 200.0, heading.center().y),
    );
    h.run();
    let after = h.state_mut().list_lines().len();
    assert!(
        after < before,
        "the group closed: {before} then {after} lines"
    );
}

#[test]
fn a_selection_of_fifty_thousand_rows_keeps_frames_short() {
    const ROWS: usize = 50_000;
    const PAGES: usize = 100;
    let mut h = mock_app_with(PAGES, 0.25, None, ScriptedDialogs::default());
    let when = origin("Ada");
    let markups: Vec<Annotation> = (0..ROWS)
        .map(|i| {
            let (page, k) = (i % PAGES, i / PAGES);
            let (x, y) = (20.0 + 11.0 * (k % 50) as f64, 20.0 + 70.0 * (k / 50) as f64);
            let mut a = Annotation::square(page, Rect::new(x, y, x + 9.0, y + 9.0), &when);
            a.meta.subject = Some(format!("Item {i:05}"));
            a
        })
        .collect();
    let ids: Vec<AnnotId> = markups.iter().map(|a| a.id.clone()).collect();
    let started = Instant::now();
    assert!(h.state_mut().add_markups(markups));
    let adding = started.elapsed();
    assert!(
        adding < Duration::from_secs(5),
        "adding 50,000 markups took {adding:?}"
    );
    h.state_mut().set_markups_list_open(true);
    settle(&mut h);
    show_columns(&mut h, &[Builtin::Subject, Builtin::Page, Builtin::Kind]);
    settle(&mut h);
    run_command(&mut h, Command::TogglePropertiesPanel);
    h.run();

    let mut idle = Duration::ZERO;
    for _ in 0..10 {
        let started = Instant::now();
        h.step();
        idle = idle.max(started.elapsed());
    }

    // Every row selected, as a Shift-click over the whole list leaves it.
    let started = Instant::now();
    h.state_mut().select_markups(ids.clone());
    let selecting = started.elapsed();
    assert!(
        selecting < Duration::from_secs(2),
        "selecting 50,000 markups took {selecting:?}"
    );
    h.run();
    assert_eq!(selected_ids(&h).len(), ROWS);
    assert!(h.state_mut().list_lines()[0].selected);
    let stats = h.state().list_stats();
    let mut longest = Duration::ZERO;
    for _ in 0..10 {
        let started = Instant::now();
        h.step();
        longest = longest.max(started.elapsed());
    }
    eprintln!("50,000 rows selected: longest frame {longest:?} (with nothing selected {idle:?})");
    assert!(
        longest < Duration::from_millis(150),
        "a frame with 50,000 rows selected took {longest:?}"
    );
    assert_eq!(
        h.state().list_stats().builds,
        stats.builds,
        "nothing rebuilt"
    );

    // A Ctrl-click takes one row out of the selection without a pause.
    let cell = h.get_by_label("Subject Item 00000, row 1").rect().center();
    let started = Instant::now();
    click_at_with(&mut h, cell, CTRL);
    let toggling = started.elapsed();
    assert_eq!(selected_ids(&h).len(), ROWS - 1);
    assert!(
        toggling < Duration::from_secs(2),
        "a Ctrl-click in the selection took {toggling:?}"
    );

    // Sorting, filtering and folding do not read the document again.
    let tables = h.state().list_stats().tables;
    let views = h.state().list_stats().views;
    h.state_mut().edit_list_layout(|layout| {
        layout.sort = vec![SortKey::desc(Builtin::Subject)];
        layout.group = vec![GroupKey::asc(Builtin::Kind)];
    });
    h.run();
    assert_eq!(h.state().list_stats().tables, tables, "the rows were kept");
    assert_eq!(h.state().list_stats().views, views + 1);
    let builds = h.state().list_stats().builds;
    let heading = h
        .query_all(by().predicate(|n| n.label().is_some_and(|l| l.starts_with("Group Type"))))
        .next()
        .expect("a group heading")
        .rect()
        .center();
    let started = Instant::now();
    click_at(&mut h, heading);
    let folding = started.elapsed();
    assert_eq!(h.state_mut().list_lines().len(), 1, "one closed group");
    let now = h.state().list_stats();
    assert_eq!(
        (now.tables, now.views),
        (tables, views + 1),
        "closing a group neither read the document nor sorted it again"
    );
    assert!(now.builds > builds);
    assert!(
        folding < Duration::from_secs(2),
        "closing a group took {folding:?}"
    );

    // Deleting the whole selection, and undoing that, are quick too.
    let started = Instant::now();
    assert!(h.state_mut().delete_selection());
    h.run();
    assert_eq!(
        h.state().markups().len(),
        1,
        "the one the Ctrl-click took out"
    );
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(h.state().markups().len(), ROWS);
    let deleting = started.elapsed();
    assert!(
        deleting < Duration::from_secs(10),
        "deleting and restoring 49,999 markups took {deleting:?}"
    );
}
