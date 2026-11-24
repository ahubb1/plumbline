//! Headless tests of the M4 markups in the window: typing into text boxes, callouts and
//! typewriter text with the in-place editor, stamps and pictures, marking up the page's own
//! text, the note window, and the new entries of the properties panel.

mod support;

use egui::accesskit::Role;
use egui::Key;
use egui_kittest::kittest::{by, Queryable};
use pl_app::{Command, ScriptedDialogs, Tool};
use pl_doc::pl_annot::{
    AnnotKind, FontStyle, FreeTextIntent, HAlign, ReviewState, Rgb, StampContent, StampSpec,
};
use pl_doc::pl_draw::DrawOp;
use pl_doc::pl_geom::{Point, Rect};
use support::*;

fn has_text_op(ops: &[DrawOp]) -> bool {
    ops.iter().any(|op| match op {
        DrawOp::Text(_) => true,
        DrawOp::Group(g) => has_text_op(&g.ops),
        _ => false,
    })
}

/// Drags a text box out with the Text Box tool; the editor is open afterwards.
fn start_text_box(h: &mut App, rect: Rect) {
    set_tool(h, Tool::TextBox);
    drag_on_page(
        h,
        0,
        Point::new(rect.x0, rect.y1),
        Point::new(rect.x1, rect.y0),
    );
    assert!(h.state().text_edit_active(), "the editor opened");
    h.run();
}

#[test]
fn typing_into_a_text_box_lays_it_out_live_and_commits_on_ctrl_enter() {
    let mut h = mock_app(1);
    click(&mut h, "Text Box tool");
    assert_eq!(h.state().tool(), Tool::TextBox);
    let rect = Rect::new(100.0, 600.0, 250.0, 680.0);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 680.0),
        Point::new(250.0, 600.0),
    );

    // The gesture created nothing: the editor is open, upright, and has the keyboard.
    assert!(markups(&h).is_empty());
    assert!(h.state().text_edit_active());
    h.run();
    assert!(h.get_by_label("Markup text").is_focused());

    // What is typed is laid out on the page by pl-draw as it is typed.
    let typed = "Falls 1:60 min. to the outlet, both sides of the ridge";
    type_into(&mut h, "Markup text", typed);
    assert_eq!(h.state().text_edit_text(), Some(typed));
    let view = h.state().active().unwrap();
    let (page, preview) = h.state().preview(view).expect("a live preview");
    assert_eq!(page, 0);
    assert!(has_text_op(&preview.ops), "the preview shows the text");
    let live = preview.clone();
    assert!(markups(&h).is_empty(), "still nothing in the document");
    // The tool keys do not fire while typing ("s" toggles snapping, "l" is the Line tool).
    assert_eq!(h.state().tool(), Tool::TextBox);
    assert!(!h.state().snap_enabled());

    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    assert!(!h.state().text_edit_active());
    let all = markups(&h);
    assert_eq!(all.len(), 1);
    let text_box = &all[0];
    assert_eq!(text_box.kind, AnnotKind::FreeText);
    assert_eq!(text_box.meta.contents.as_deref(), Some(typed));
    assert_eq!(text_box.meta.subject.as_deref(), Some("Text Box"));
    assert_eq!(text_box.meta.author.as_deref(), Some("Test Author"));
    assert!(
        rect_near(rect_of(text_box), rect),
        "{:?}",
        text_box.geometry
    );
    let free = text_box.free_text.as_ref().expect("a modelled text box");
    assert_eq!(free.intent, FreeTextIntent::Box);
    assert_eq!(free.rotation, 0);

    // The layout: wrapped inside the box, every character accounted for, in order.
    let layout = text_box.text_layout().expect("a layout").expect("WinAnsi");
    assert!(layout.lines.len() >= 2, "{} lines", layout.lines.len());
    assert!(layout.overflow.is_none(), "{:?}", layout.overflow);
    let (frame, _) = text_box.text_frame().unwrap();
    for line in &layout.lines {
        assert!(line.width <= frame.width() + 1e-6, "{line:?}");
    }
    let joined: Vec<&str> = layout.lines.iter().map(|l| l.text.trim()).collect();
    assert_eq!(joined.join(" "), typed);
    // What was shown while typing is what the document now draws.
    assert_eq!(text_box.draw_list(), live);
    assert_eq!(selected_ids(&h), vec![text_box.id.as_str().to_owned()]);

    // One undo step takes the whole text box away again.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert!(markups(&h).is_empty());
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    assert_eq!(markups(&h).len(), 1);
}

#[test]
fn a_character_outside_winansi_is_refused_with_a_message_in_the_editor() {
    let mut h = mock_app_tofu(1, 0.25);
    start_text_box(&mut h, Rect::new(100.0, 600.0, 300.0, 680.0));
    assert!(h.state().text_edit_message().is_none());

    type_into(&mut h, "Markup text", "Gef\u{00e4}lle \u{2192} 1:60");
    // The arrow is not in WinAnsi: it is not taken, and the editor says so by name. The
    // a-umlaut is, and stays.
    assert_eq!(h.state().text_edit_text(), Some("Gef\u{00e4}lle  1:60"));
    let message = h.state().text_edit_message().expect("a message").to_owned();
    assert!(message.contains('\u{2192}'), "{message}");
    assert!(message.contains("WinAnsi"), "{message}");
    h.get_by_label(message.as_str());

    // More of the same, and a tab (which becomes spaces).
    type_into(&mut h, "Markup text", " \u{4e2d}\u{6587}\tok");
    assert_eq!(
        h.state().text_edit_text(),
        Some("Gef\u{00e4}lle  1:60     ok")
    );
    let message = h.state().text_edit_message().expect("a message").to_owned();
    assert!(
        message.contains("U+4E2D") && message.contains("U+6587"),
        "{message}"
    );

    // Typing something that can be written clears the message.
    type_into(&mut h, "Markup text", "!");
    assert!(h.state().text_edit_message().is_none());

    click(&mut h, "Done");
    let all = markups(&h);
    assert_eq!(all.len(), 1);
    assert_eq!(
        all[0].meta.contents.as_deref(),
        Some("Gef\u{00e4}lle  1:60     ok!")
    );
    assert!(all[0].validate().is_ok());
}

#[test]
fn escape_cancels_the_text_and_a_click_elsewhere_commits_it() {
    let mut h = mock_app(1);
    start_text_box(&mut h, Rect::new(100.0, 600.0, 300.0, 680.0));
    type_into(&mut h, "Markup text", "not kept");
    h.key_press(Key::Escape);
    h.run();
    assert!(!h.state().text_edit_active());
    assert!(markups(&h).is_empty());
    let view = h.state().active().unwrap();
    assert!(h.state().preview(view).is_none(), "the draft is gone");
    assert_eq!(
        h.state().tool(),
        Tool::TextBox,
        "only the text was cancelled"
    );

    // Again, and this time a click on the page finishes it. The click does nothing else.
    start_text_box(&mut h, Rect::new(100.0, 600.0, 300.0, 680.0));
    type_into(&mut h, "Markup text", "kept");
    click_on_page(&mut h, 0, Point::new(450.0, 500.0));
    assert!(!h.state().text_edit_active());
    let all = markups(&h);
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].meta.contents.as_deref(), Some("kept"));

    // An editor left empty creates nothing.
    start_text_box(&mut h, Rect::new(100.0, 400.0, 300.0, 480.0));
    click(&mut h, "Done");
    assert_eq!(markups(&h).len(), 1);

    // Picking another tool finishes the text too.
    start_text_box(&mut h, Rect::new(100.0, 400.0, 300.0, 480.0));
    type_into(&mut h, "Markup text", "second");
    click(&mut h, "Rectangle tool");
    assert_eq!(h.state().tool(), Tool::Rectangle);
    assert_eq!(markups(&h).len(), 2);
}

#[test]
fn typewriter_text_is_fitted_and_a_callout_gets_its_leader() {
    let mut h = mock_app(1);
    // Typewriter: one click, then type. The box takes the size of the text.
    h.key_press(Key::F);
    h.run();
    assert_eq!(h.state().tool(), Tool::Typewriter);
    click_on_page(&mut h, 0, Point::new(120.0, 700.0));
    assert!(h.state().text_edit_active());
    h.run();
    type_into(&mut h, "Markup text", "GL 12.450");
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    let typed = markups(&h).pop().expect("typewriter text");
    let free = typed.free_text.as_ref().unwrap();
    assert_eq!(free.intent, FreeTextIntent::TypeWriter);
    assert_eq!(typed.style.stroke, None, "no border");
    let layout = typed.text_layout().unwrap().unwrap();
    assert_eq!(layout.lines.len(), 1);
    let r = rect_of(&typed);
    assert!(
        r.width() < 100.0 && r.width() > layout.lines[0].width,
        "fitted to the text: {r:?}"
    );
    assert!(layout.overflow.is_none());

    // Callout: click what it points at, then drag the box, then type.
    h.key_press(Key::O);
    h.run();
    assert_eq!(h.state().tool(), Tool::Callout);
    let tip = Point::new(150.0, 400.0);
    click_on_page(&mut h, 0, tip);
    assert!(!h.state().text_edit_active());
    drag_on_page(
        &mut h,
        0,
        Point::new(260.0, 520.0),
        Point::new(400.0, 470.0),
    );
    assert!(h.state().text_edit_active());
    h.run();
    type_into(&mut h, "Markup text", "Check invert level");
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    let callout = markups(&h).pop().expect("a callout");
    let free = callout.free_text.as_ref().unwrap();
    assert_eq!(free.intent, FreeTextIntent::Callout);
    assert!(free.callout.len() >= 2, "{:?}", free.callout);
    assert!(near(free.callout[0], tip), "{:?}", free.callout);
    assert_eq!(callout.meta.contents.as_deref(), Some("Check invert level"));
    assert!(rect_near(
        rect_of(&callout),
        Rect::new(260.0, 470.0, 400.0, 520.0)
    ));
    assert_eq!(markups(&h).len(), 2);
}

#[test]
fn an_existing_text_box_is_edited_in_place_as_one_undo_step() {
    let mut h = mock_app_with(1, 0.04, None, ScriptedDialogs::default());
    start_text_box(&mut h, Rect::new(100.0, 600.0, 300.0, 680.0));
    type_into(&mut h, "Markup text", "First");
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    let id = markups(&h)[0].id.clone();

    // A double-click with the Select tool opens the editor on it, with its text.
    set_tool(&mut h, Tool::Select);
    let middle = on_screen(&h, 0, Point::new(200.0, 640.0));
    double_click_at(&mut h, middle);
    assert!(h.state().text_edit_active());
    assert_eq!(h.state().text_edit_text(), Some("First"));
    h.run();
    assert!(h.get_by_label("Markup text").is_focused());
    type_into(&mut h, "Markup text", " draft");
    type_into(&mut h, "Markup text", " of the note");
    // Each change is in the document as it is typed.
    assert_eq!(
        markups(&h)[0].meta.contents.as_deref(),
        Some("First draft of the note")
    );
    click(&mut h, "Done");
    assert!(!h.state().text_edit_active());
    assert_eq!(markups(&h)[0].id, id);

    // The whole session is one undo step.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert_eq!(markups(&h)[0].meta.contents.as_deref(), Some("First"));
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    assert_eq!(
        markups(&h)[0].meta.contents.as_deref(),
        Some("First draft of the note")
    );

    // Escape puts the text back as it was when the editor opened.
    double_click_at(&mut h, middle);
    assert!(h.state().text_edit_active());
    h.run();
    type_into(&mut h, "Markup text", " and more");
    assert!(markups(&h)[0]
        .meta
        .contents
        .as_deref()
        .unwrap()
        .ends_with("and more"));
    h.key_press(Key::Escape);
    h.run();
    assert!(!h.state().text_edit_active());
    assert_eq!(
        markups(&h)[0].meta.contents.as_deref(),
        Some("First draft of the note")
    );
}

#[test]
fn the_properties_panel_edits_text_size_colour_alignment_fill_and_border() {
    let mut h = mock_app(1);
    start_text_box(&mut h, Rect::new(100.0, 600.0, 300.0, 680.0));
    type_into(&mut h, "Markup text", "Sized");
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    set_tool(&mut h, Tool::Select);
    click(&mut h, "Properties panel");
    for name in [
        "Font size",
        "Font style",
        "Text alignment",
        "Text colour as hex",
        "Border colour as hex",
        "Fill colour as hex",
        "Line width",
    ] {
        assert!(h.query_by_label(name).is_some(), "no control named {name}");
    }
    assert_all_named(&h, "properties of a text box");

    click(&mut h, "Text colour Blue");
    click(&mut h, "Fill colour Yellow");
    click(&mut h, "Border colour Black");
    click_label(&mut h, "Text alignment");
    click(&mut h, "Centre");
    click_label(&mut h, "Font style");
    click(&mut h, "Bold");
    let text_box = markups(&h).pop().unwrap();
    let free = text_box.free_text.as_ref().unwrap();
    assert_eq!(free.color.to_u8(), Rgb::new(0.1, 0.3, 0.8).to_u8());
    assert_eq!(free.align, HAlign::Centre);
    assert_eq!(free.font, FontStyle::Bold);
    assert_eq!(
        text_box.style.fill.map(Rgb::to_u8),
        Some(Rgb::new(1.0, 0.9, 0.2).to_u8())
    );
    assert_eq!(
        text_box.style.stroke.map(Rgb::to_u8),
        Some(Rgb::new(0.0, 0.0, 0.0).to_u8())
    );
    assert!(text_box.validate().is_ok());

    // The font size box takes a typed value; the layout follows.
    let before = text_box.text_layout().unwrap().unwrap().line_advance;
    let size = h.get_by_label("Font size");
    size.focus();
    h.run();
    h.key_press_modifiers(CTRL, Key::A);
    h.get_by_label("Font size").type_text("20");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    let text_box = markups(&h).pop().unwrap();
    assert_eq!(text_box.free_text.as_ref().unwrap().size, 20.0);
    let after = text_box.text_layout().unwrap().unwrap().line_advance;
    assert!(after > before * 1.5, "{before} -> {after}");

    // With nothing selected the Text Box tool's own settings are edited, and the next box
    // is drawn with them.
    h.key_press(Key::Escape);
    h.run();
    set_tool(&mut h, Tool::TextBox);
    click(&mut h, "Text colour Green");
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 480.0),
        Point::new(300.0, 400.0),
    );
    h.run();
    type_into(&mut h, "Markup text", "green");
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    let green = markups(&h).pop().unwrap();
    assert_eq!(
        green.free_text.as_ref().unwrap().color.to_u8(),
        Rgb::new(0.1, 0.55, 0.2).to_u8()
    );
}

#[test]
fn each_built_in_stamp_is_placed_from_the_palette_and_a_picture_from_a_file() {
    let dir = tempfile::tempdir().unwrap();
    // A small picture on disk: 40 x 20, opaque orange.
    let picture = dir.path().join("mark.png");
    let mut pixels = Vec::new();
    for _ in 0..(40 * 20) {
        pixels.extend_from_slice(&[230, 120, 20, 255]);
    }
    let image = pl_doc::pl_draw::RgbaImage::new(40, 20, pixels).unwrap();
    std::fs::write(&picture, pl_doc::images::encode_png(&image)).unwrap();
    let dialogs = ScriptedDialogs::default().with_images([picture.clone()]);
    let mut h = mock_app_with(1, 0.25, None, dialogs);

    click(&mut h, "Stamp tool");
    assert_eq!(h.state().tool(), Tool::Stamp);
    click(&mut h, "Properties panel");
    let set = StampSpec::standard_set();
    assert_eq!(set.len(), 5);
    for (i, spec) in set.iter().enumerate() {
        click(&mut h, &format!("Stamp {}", spec.label));
        assert_eq!(h.state().stamp().as_ref(), Some(spec));
        // A click places it at its natural size, centred on the click.
        let at = Point::new(100.0 + 60.0 * (i % 2) as f64, 730.0 - 62.0 * i as f64);
        click_on_page(&mut h, 0, at);
        let placed = markups(&h).pop().expect("a stamp");
        assert_eq!(placed.kind, AnnotKind::Stamp, "{}", spec.label);
        let stamp = placed.stamp.as_ref().expect("a modelled stamp");
        assert_eq!(stamp.content, StampContent::Label(spec.clone()));
        let (w, hgt) = spec.natural_size().unwrap();
        let r = rect_of(&placed);
        assert!((r.width() - w).abs() < 0.01 && (r.height() - hgt).abs() < 0.01);
        assert!(near(r.center(), at), "{r:?}");
        assert!(placed.validate().is_ok());
        assert!(!placed.draw_list().is_empty());
        assert_eq!(markups(&h).len(), i + 1);
    }
    let labels: Vec<&str> = set.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(
        labels,
        ["APPROVED", "REVISED", "FOR REVIEW", "VOID", "DRAFT"]
    );

    // A drag sizes the stamp, keeping its proportions.
    drag_on_page(
        &mut h,
        0,
        Point::new(260.0, 700.0),
        Point::new(410.0, 600.0),
    );
    let sized = markups(&h).pop().unwrap();
    let r = rect_of(&sized);
    let (w, hgt) = set[4].natural_size().unwrap();
    assert!(r.width() > w, "{r:?}");
    assert!(((r.width() / r.height()) - (w / hgt)).abs() < 0.01, "{r:?}");

    // "Image as stamp" asks for a file and switches to the Image tool with it.
    assert!(!h.state().has_image());
    click_starting(&mut h, "Image as stamp");
    assert!(h.state().has_image());
    assert_eq!(h.state().tool(), Tool::Image);
    click_on_page(&mut h, 0, Point::new(330.0, 430.0));
    let placed = markups(&h).pop().expect("a picture");
    assert_eq!(placed.kind, AnnotKind::Stamp);
    match &placed.stamp.as_ref().unwrap().content {
        StampContent::Image(image) => {
            assert_eq!((image.width(), image.height()), (40, 20));
            assert_eq!(&image.pixels()[..4], &[230, 120, 20, 255]);
        }
        other => panic!("{other:?}"),
    }
    let r = rect_of(&placed);
    assert!((r.width() / r.height() - 2.0).abs() < 0.01, "{r:?}");
    assert!(near(r.center(), Point::new(330.0, 430.0)));
    assert_eq!(markups(&h).len(), 7);
    assert_all_named(&h, "the Image tool's properties");

    // Everything placed is undoable, one step each.
    for left in (0..7).rev() {
        h.key_press_modifiers(CTRL, Key::Z);
        h.run();
        assert_eq!(markups(&h).len(), left);
    }
}

#[test]
fn the_image_tool_asks_for_a_picture_and_stays_put_when_none_is_given() {
    // The dialog is cancelled: the tool that was active stays.
    let mut h = mock_app(1);
    set_tool(&mut h, Tool::Rectangle);
    run_command(&mut h, Command::Tool(Tool::Image));
    assert_eq!(h.state().tool(), Tool::Rectangle);
    assert!(!h.state().has_image());

    // A file that is not a picture is refused with a message.
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("notes.png");
    std::fs::write(&bad, b"not a png").unwrap();
    let dialogs = ScriptedDialogs::default().with_images([bad]);
    let mut h = mock_app_with(1, 0.25, None, dialogs);
    run_command(&mut h, Command::Tool(Tool::Image));
    assert_ne!(h.state().tool(), Tool::Image);
    assert!(
        h.state()
            .status()
            .is_some_and(|s| s.contains("could not be used as a picture")),
        "{:?}",
        h.state().status()
    );
}

#[test]
fn a_double_click_on_a_note_opens_its_window_with_status_and_replies() {
    let mut h = mock_app_tofu(1, 0.04);
    set_tool(&mut h, Tool::Note);
    let at = Point::new(300.0, 500.0);
    click_on_page(&mut h, 0, at);
    let note = markups(&h).pop().expect("a note");
    assert_eq!(note.kind, AnnotKind::Text);
    assert!(h.state().note_open().is_none());

    set_tool(&mut h, Tool::Select);
    let pos = on_screen(&h, 0, at);
    double_click_at(&mut h, pos);
    assert_eq!(h.state().note_open(), Some(&note.id));
    h.run();
    h.get_by_label("Note");
    assert!(h.get_by_label("Note text").is_focused());
    assert_all_named(&h, "the note window");

    // Its text is typed here; any character is fine in a note (it is not drawn on the page).
    type_into(&mut h, "Note text", "Check falls \u{2192} outlet");
    let note_now = h.state().markups().into_iter().find(|a| a.id == note.id);
    assert_eq!(
        note_now.unwrap().meta.contents.as_deref(),
        Some("Check falls \u{2192} outlet")
    );

    // The review status.
    click_label(&mut h, "Review status");
    click(&mut h, "Accepted");
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.review_state(&note.id), ReviewState::Accepted);

    // A reply, and it is listed.
    h.get_by_label("Reply text").focus();
    h.run();
    type_into(&mut h, "Reply text", "Agreed, 1:60");
    click(&mut h, "Add reply");
    let doc = h.state().active_document().unwrap();
    let replies = doc.editor().unwrap().set().replies(&note.id);
    assert!(
        replies
            .iter()
            .any(|r| r.meta.contents.as_deref() == Some("Agreed, 1:60")),
        "{replies:?}"
    );
    h.get_by_label("Test Author: Agreed, 1:60");

    // Each of the three was one undo step.
    click(&mut h, "Close note");
    assert!(h.state().note_open().is_none());
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    let doc = h.state().active_document().unwrap();
    assert!(doc
        .editor()
        .unwrap()
        .set()
        .replies(&note.id)
        .iter()
        .all(|r| r.meta.contents.as_deref() != Some("Agreed, 1:60")));
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.review_state(&note.id), ReviewState::None);
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    let note_now = h.state().markups().into_iter().find(|a| a.id == note.id);
    assert_eq!(note_now.unwrap().meta.contents, None);
}

#[test]
fn every_new_tool_and_control_has_an_accessible_name() {
    let mut h = mock_app(1);
    for tool in Tool::ALL {
        let name = format!("{} tool", tool.label());
        h.get_by_role_and_label(Role::Button, &name);
        assert!(tool.tooltip().contains(tool.key().name()));
        assert!(!tool.hint().is_empty());
    }
    h.get_by_role_and_label(Role::Button, "Markups list");
    // The names are the app's own.
    let labels: Vec<&str> = Tool::ALL.iter().map(|t| t.label()).collect();
    for wanted in [
        "Text Box",
        "Typewriter",
        "Callout",
        "Cloud Box",
        "Stamp",
        "Image",
        "Highlight Text",
        "Underline Text",
        "Strikeout Text",
    ] {
        assert!(labels.contains(&wanted), "{wanted}");
    }
    assert_all_named(&h, "viewer");
    click(&mut h, "Properties panel");
    for tool in [
        Tool::TextBox,
        Tool::Typewriter,
        Tool::Callout,
        Tool::CloudBox,
        Tool::Stamp,
        Tool::TextHighlight,
        Tool::TextUnderline,
        Tool::TextStrikeOut,
    ] {
        set_tool(&mut h, tool);
        assert_all_named(&h, tool.label());
    }
    // The editor.
    start_text_box(&mut h, Rect::new(100.0, 600.0, 300.0, 680.0));
    assert_all_named(&h, "the text editor");
    for name in ["Markup text", "Done", "Cancel"] {
        assert!(h.query_by_label(name).is_some(), "no control named {name}");
    }
    h.key_press(Key::Escape);
    h.run();
    // The View menu has the list.
    click(&mut h, "View");
    assert_all_named(&h, "View menu");
    let entries = h
        .query_all(by().predicate(|node| {
            node.role() == Role::Button
                && node.label().is_some_and(|l| l.starts_with("Markups list"))
        }))
        .count();
    assert_eq!(entries, 2, "the tool bar button and the View menu's entry");
    h.key_press(Key::Escape);
    h.run();
    // Its shortcut.
    assert!(!h.state().markups_list_open());
    let tool = h.state().tool();
    h.key_press_modifiers(CTRL, Key::L);
    h.run();
    assert!(h.state().markups_list_open());
    assert_eq!(h.state().tool(), tool, "Ctrl+L is not the Line tool's L");
    assert_all_named(&h, "the markups list");
}

#[test]
fn the_cloud_box_tool_drags_a_clouded_rectangle() {
    let mut h = mock_app(1);
    h.key_press(Key::Q);
    h.run();
    assert_eq!(h.state().tool(), Tool::CloudBox);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 700.0),
        Point::new(260.0, 600.0),
    );
    let cloud = markups(&h).pop().expect("a cloud");
    assert_eq!(cloud.kind, AnnotKind::Square);
    assert!(cloud.style.cloud.is_some());
    assert!(rect_near(
        rect_of(&cloud),
        Rect::new(100.0, 600.0, 260.0, 700.0)
    ));
    assert_eq!(cloud.meta.subject.as_deref(), Some("Cloud Box"));
}

// ---- real PDFium: the page's own text, and every page layout ---------------------------------

fn quad_bounds(quads: &[pl_doc::pl_annot::Quad]) -> Rect {
    let mut r = Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for q in quads {
        for p in q {
            r.x0 = r.x0.min(p.x);
            r.y0 = r.y0.min(p.y);
            r.x1 = r.x1.max(p.x);
            r.y1 = r.y1.max(p.y);
        }
    }
    r
}

/// Goes to a page and, once its real geometry is in, brings `rect` of it into view.
fn go_to(h: &mut App, page: u32, rect: Rect) {
    h.state_mut().active_view_mut().unwrap().go_to_page(page);
    settle(h);
    let dv = h.state_mut().active_view_mut().unwrap();
    dv.view.reveal_rect(page, rect, 80.0);
    settle(h);
}

/// Dragging across a line of the page's text with the Highlight, Underline and Strikeout
/// tools marks exactly that text: the markup's quad is the box PDFium gives the characters,
/// on plain, rotated, `/UserUnit` and CropBox pages, and it is in the saved file.
#[test]
fn real_pdfium_dragging_over_text_marks_it_up_with_the_characters_own_boxes() {
    for fixture in [
        pl_testkit::plain(),
        pl_testkit::rotated_pages(),
        pl_testkit::user_unit(),
        pl_testkit::crop_box(),
    ] {
        let name = fixture.manifest.file_name();
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &fixture);
        let mut h = real_app(&path);
        let mut made = Vec::new();
        for (page, truth) in fixture.manifest.pages.iter().enumerate() {
            let page = page as u32;
            let Some(text) = truth.texts.iter().find(|t| t.width > 30.0) else {
                continue;
            };
            let what = format!("{name} page {page} \"{}\"", text.text);
            let (x0, y0) = (text.origin[0], text.origin[1]);
            let mid = y0 + text.font_size * 0.3;
            let line = Rect::new(
                x0,
                y0 - text.font_size,
                x0 + text.width,
                y0 + text.font_size,
            );
            go_to(&mut h, page, line);
            for (tool, kind) in [
                (Tool::TextHighlight, AnnotKind::Highlight),
                (Tool::TextUnderline, AnnotKind::Underline),
                (Tool::TextStrikeOut, AnnotKind::StrikeOut),
            ] {
                set_tool(&mut h, tool);
                // The tool needs the text of the page; it is fetched for the pages in view.
                settle(&mut h);
                let before = markups(&h).len();
                drag_on_page(
                    &mut h,
                    page,
                    Point::new(x0 + 1.0, mid),
                    Point::new(x0 + text.width - 1.0, mid),
                );
                let all = markups(&h);
                assert_eq!(all.len(), before + 1, "{what}: {kind:?}");
                let markup = all.last().unwrap().clone();
                assert_eq!(markup.kind, kind, "{what}");
                assert_eq!(markup.page, page as usize);
                assert_eq!(
                    markup.meta.contents.as_deref(),
                    Some(text.text.as_str()),
                    "{what}"
                );
                let pl_doc::pl_annot::Geometry::Quads(quads) = &markup.geometry else {
                    panic!("{what}: {:?}", markup.geometry)
                };
                assert_eq!(quads.len(), 1, "{what}: one line of text, one quad");
                // The boxes PDFium gives those characters.
                let doc = h.state().active_document().unwrap();
                let page_text = doc
                    .cached_text(page)
                    .expect("the text of the page is cached");
                let all_text = page_text.text();
                let at = all_text
                    .find(&text.text)
                    .expect("the line is in the text of the page");
                let start = all_text[..at].chars().count();
                let count = text.text.chars().count();
                let mut boxes = Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for c in page_text.chars[start..start + count]
                    .iter()
                    .filter(|c| c.has_box())
                {
                    boxes.x0 = boxes.x0.min(c.bbox[0]);
                    boxes.y0 = boxes.y0.min(c.bbox[1]);
                    boxes.x1 = boxes.x1.max(c.bbox[2]);
                    boxes.y1 = boxes.y1.max(c.bbox[3]);
                }
                let got = quad_bounds(quads);
                let close = |a: f64, b: f64| (a - b).abs() <= 0.05;
                assert!(
                    close(got.x0, boxes.x0)
                        && close(got.y0, boxes.y0)
                        && close(got.x1, boxes.x1)
                        && close(got.y1, boxes.y1),
                    "{what}: quad {got:?} against the character boxes of PDFium {boxes:?}"
                );
                // And those are where the fixture put the text.
                assert!(
                    (got.x0 - x0).abs() < 1.5 && (got.x1 - (x0 + text.width)).abs() < 1.5,
                    "{what}: {got:?}"
                );
                assert!(
                    got.y0 < y0 && got.y1 > y0 + text.font_size * 0.5,
                    "{what}: {got:?}"
                );
                // The quad starts at the start of the baseline and runs along the text.
                let q = &quads[0];
                assert!(
                    q[0].x < q[1].x && (q[0].y - q[1].y).abs() < 1e-6,
                    "{what}: {q:?}"
                );
                made.push(markup);
            }
        }
        assert!(made.len() >= 3, "{name}: {} markups", made.len());

        // Saved and read back by a fresh app: the same markups.
        h.key_press_modifiers(CTRL, Key::S);
        settle(&mut h);
        assert!(
            h.state().status().is_some_and(|s| s.starts_with("Saved")),
            "{name}: {:?}",
            h.state().status()
        );
        drop(h);
        let mut h = real_app(&path);
        h.state_mut().select_all_markups();
        h.run();
        let doc = h.state().active_document().unwrap();
        for markup in &made {
            let read = doc.annotation(&markup.id).unwrap_or_else(|| {
                panic!("{name}: {} is not in the saved file", markup.id.as_str())
            });
            assert_eq!(read.kind, markup.kind, "{name}");
            let (pl_doc::pl_annot::Geometry::Quads(a), pl_doc::pl_annot::Geometry::Quads(b)) =
                (&read.geometry, &markup.geometry)
            else {
                panic!("{name}: {:?}", read.geometry)
            };
            assert!(rect_near(quad_bounds(a), quad_bounds(b)), "{name}");
            assert_eq!(read.meta.contents, markup.meta.contents);
        }
    }
}

/// A text box typed on every page layout reads upright as the page is displayed, is sized in
/// points whatever the `/UserUnit` of the page, and PDFium draws it from the saved file.
#[test]
fn real_pdfium_text_boxes_read_upright_on_every_page_layout_and_are_saved() {
    for fixture in [
        pl_testkit::rotated_pages(),
        pl_testkit::user_unit(),
        pl_testkit::crop_box(),
    ] {
        let name = fixture.manifest.file_name();
        let dir = tempfile::tempdir().unwrap();
        let path = write_fixture(dir.path(), &fixture);
        let mut h = real_app(&path);
        let mut made = Vec::new();
        for (page, truth) in fixture.manifest.pages.iter().enumerate() {
            let page = page as u32;
            let what = format!("{name} page {page}");
            h.state_mut().active_view_mut().unwrap().go_to_page(page);
            settle(&mut h);
            let crop = h
                .state()
                .active_view()
                .unwrap()
                .view
                .file_geometry(page)
                .unwrap()
                .crop_box();
            let centre = crop.center();
            go_to(
                &mut h,
                page,
                Rect::new(
                    centre.x - 80.0,
                    centre.y - 80.0,
                    centre.x + 80.0,
                    centre.y + 80.0,
                ),
            );
            // A drag of 150 x 70 pixels on screen, whichever way the page is turned.
            set_tool(&mut h, Tool::TextBox);
            let from = on_screen(&h, page, centre) + egui::vec2(-75.0, -35.0);
            drag(&mut h, from, from + egui::vec2(150.0, 70.0));
            assert!(h.state().text_edit_active(), "{what}");
            h.run();
            let typed = format!("Level {page} 12.50");
            type_into(&mut h, "Markup text", &typed);
            h.key_press_modifiers(CTRL, Key::Enter);
            h.run();
            let text_box = markups(&h).pop().expect("a text box");
            assert_eq!(text_box.kind, AnnotKind::FreeText, "{what}");
            assert_eq!(text_box.meta.contents.as_deref(), Some(typed.as_str()));
            let free = text_box.free_text.as_ref().unwrap();
            assert_eq!(
                i32::from(free.rotation),
                truth.rotate.rem_euclid(360),
                "{what}: turned with the page"
            );
            assert!(
                (free.size - 12.0 / truth.user_unit).abs() < 1e-9,
                "{what}: 12 pt on a page of unit {}: {}",
                truth.user_unit,
                free.size
            );
            // Upright as displayed: the text runs towards +x on screen, and the top of its
            // frame is above its bottom.
            let (frame, to_page) = text_box.text_frame().expect("a text frame");
            let dv = h.state().active_view().unwrap();
            let geometry = dv.view.page_geometry(page).unwrap();
            let to_view = dv.view.view_transform(page).unwrap().page_to_view(geometry);
            let origin = to_view * (to_page * Point::new(frame.x0, frame.y0));
            let along = to_view * (to_page * Point::new(frame.x1, frame.y0)) - origin;
            let up = to_view * (to_page * Point::new(frame.x0, frame.y1)) - origin;
            assert!(along.x > 100.0 && along.y.abs() < 1e-6, "{what}: {along:?}");
            assert!(up.y < -30.0 && up.x.abs() < 1e-6, "{what}: {up:?}");
            let layout = text_box.text_layout().unwrap().unwrap();
            assert_eq!(layout.lines.len(), 1, "{what}");
            assert!(layout.overflow.is_none(), "{what}: {:?}", layout.overflow);
            made.push((page, typed, text_box));
        }
        h.key_press_modifiers(CTRL, Key::S);
        settle(&mut h);
        assert!(
            h.state().status().is_some_and(|s| s.starts_with("Saved")),
            "{name}: {:?}",
            h.state().status()
        );
        drop(h);

        // PDFium, on its own, draws the saved boxes: red text and border inside each.
        let backend = pdfium();
        let info = backend
            .open(pl_doc::pl_render::DocSource::file(path.clone()), None)
            .unwrap();
        for (page, _, text_box) in &made {
            let unit = fixture.manifest.pages[*page as usize].user_unit;
            if unit != 1.0 {
                backend.set_page_user_unit(info.doc, *page, unit).unwrap();
            }
            let geometry = backend.page_info(info.doc, *page).unwrap().geometry();
            let bitmap = backend.render_page(info.doc, *page, 2.0).unwrap();
            let shown = (pl_doc::pl_geom::Affine::scale(2.0) * geometry.page_to_display())
                .transform_rect_bbox(rect_of(text_box));
            let mut red = 0;
            for y in (shown.y0.max(0.0) as u32)..(shown.y1 as u32).min(bitmap.height) {
                for x in (shown.x0.max(0.0) as u32)..(shown.x1 as u32).min(bitmap.width) {
                    let [r, g, b, _] = bitmap.pixel(x, y);
                    if r > 150 && g < 90 && b < 90 {
                        red += 1;
                    }
                }
            }
            assert!(
                red > 150,
                "{name} page {page}: {red} red pixels in the saved box"
            );
        }
        backend.close(info.doc).unwrap();

        // A fresh app reads the same text boxes back, text and all.
        let mut h = real_app(&path);
        h.state_mut().select_all_markups();
        h.run();
        let doc = h.state().active_document().unwrap();
        for (page, typed, text_box) in &made {
            let read = doc
                .annotation(&text_box.id)
                .unwrap_or_else(|| panic!("{name} page {page}: not in the saved file"));
            assert_eq!(read.meta.contents.as_deref(), Some(typed.as_str()));
            assert_eq!(read.free_text, text_box.free_text, "{name} page {page}");
            assert!(
                rect_near(rect_of(read), rect_of(text_box)),
                "{name} page {page}"
            );
            // The layout the appearance was written from holds every character, in order.
            let layout = read.text_layout().unwrap().unwrap();
            let lines: Vec<&str> = layout.lines.iter().map(|l| l.text.as_str()).collect();
            assert_eq!(lines.join(""), *typed);
        }
    }
}

/// A text box written by another program is shown as its author made it until it is edited;
/// a double-click opens the editor on its text, and the first change makes it ours.
#[test]
fn real_pdfium_a_text_box_from_another_program_is_taken_up_when_its_text_is_edited() {
    let dir = tempfile::tempdir().unwrap();
    let fixture = pl_testkit::annotations();
    let path = write_fixture(dir.path(), &fixture);
    let truth = fixture.manifest.pages[0]
        .annotations
        .iter()
        .find(|a| a.subtype == "FreeText")
        .expect("the fixture has a FreeText");
    let rect = Rect::new(truth.rect[0], truth.rect[1], truth.rect[2], truth.rect[3]);
    let mut h = file_app(pdfium(), &path, None, ScriptedDialogs::default(), 0.04);
    go_to(&mut h, 0, rect);
    let middle = on_screen(&h, 0, rect.center());
    // Something has to have asked for the markups before a double-click can land on one.
    h.hover_at(middle);
    h.run();
    let find = |h: &App| {
        h.state()
            .markups()
            .into_iter()
            .find(|a| a.id.as_str() == truth.name)
            .expect("the text box of the fixture")
    };
    let before = find(&h);
    assert!(before.free_text.is_none(), "shown from its own appearance");
    assert_eq!(
        before.meta.contents.as_deref(),
        Some(truth.contents.as_str())
    );

    double_click_at(&mut h, middle);
    assert!(h.state().text_edit_active());
    assert_eq!(h.state().text_edit_text(), Some(truth.contents.as_str()));
    h.run();
    type_into(&mut h, "Markup text", ", checked");
    click(&mut h, "Done");
    let after = find(&h);
    let expected = format!("{}, checked", truth.contents);
    assert_eq!(after.meta.contents.as_deref(), Some(expected.as_str()));
    assert!(after.free_text.is_some(), "it is one of ours now");
    assert!(after.validate().is_ok());
    assert!(rect_near(rect_of(&after), rect_of(&before)));
    // It is painted by the window now, and hidden in the page render.
    let doc = h.state().active_document().unwrap();
    assert!(doc.overlay(0).iter().any(|item| item.id == after.id));
    assert_eq!(doc.hidden_annotations().len(), 1);

    // Saved, it reads back as a text box with the new text.
    h.key_press_modifiers(CTRL, Key::S);
    settle(&mut h);
    assert!(
        h.state().status().is_some_and(|s| s.starts_with("Saved")),
        "{:?}",
        h.state().status()
    );
    drop(h);
    let mut h = real_app(&path);
    h.state_mut().select_all_markups();
    h.run();
    let read = find(&h);
    assert_eq!(read.meta.contents.as_deref(), Some(expected.as_str()));
    assert!(read.free_text.is_some());
}

// ---- snapshot of the interface (goldens depend on the GPU adapter) -------------------------

#[test]
fn snapshot_text_markups_light() {
    let _gpu = gpu();
    let mut h = mock_app(2);
    // The text cursor of the editor does not blink in the picture.
    h.ctx
        .all_styles_mut(|style| style.visuals.text_cursor.blink = false);
    set_tool(&mut h, Tool::Stamp);
    click_on_page(&mut h, 0, Point::new(340.0, 710.0));
    h.state_mut().set_stamp(StampSpec::for_review());
    click_on_page(&mut h, 0, Point::new(340.0, 650.0));
    set_tool(&mut h, Tool::CloudBox);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 580.0),
        Point::new(440.0, 500.0),
    );
    set_tool(&mut h, Tool::Callout);
    click_on_page(&mut h, 0, Point::new(320.0, 540.0));
    drag_on_page(&mut h, 0, Point::new(60.0, 580.0), Point::new(190.0, 540.0));
    h.run();
    type_into(&mut h, "Markup text", "Check invert level");
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    h.key_press(Key::Escape);
    h.run();
    set_tool(&mut h, Tool::TextBox);
    click(&mut h, "Properties panel");
    drag_on_page(&mut h, 0, Point::new(60.0, 740.0), Point::new(250.0, 660.0));
    h.run();
    type_into(
        &mut h,
        "Markup text",
        "Falls 1:60 min. to the outlet, both sides of the ridge",
    );
    settle(&mut h);
    assert!(h.state().text_edit_active());
    h.snapshot("text_markups_light");
}

#[test]
fn the_cloud_callout_eraser_and_squiggly_tools_work_in_the_window() {
    // Cloud Callout: drag the cloud, drag the box, type. A cloud and a callout pointing at it.
    let mut h = mock_app(1);
    h.key_press(Key::K);
    h.run();
    assert_eq!(h.state().tool(), Tool::CloudCallout);
    drag_on_page(
        &mut h,
        0,
        Point::new(300.0, 700.0),
        Point::new(420.0, 620.0),
    );
    assert!(markups(&h).is_empty(), "nothing until the text is in");
    drag_on_page(&mut h, 0, Point::new(80.0, 560.0), Point::new(220.0, 510.0));
    assert!(h.state().text_edit_active());
    h.run();
    type_into(&mut h, "Markup text", "Revised kerb line");
    h.key_press_modifiers(CTRL, Key::Enter);
    h.run();
    let all = markups(&h);
    assert_eq!(all.len(), 2, "{all:#?}");
    let cloud = all.iter().find(|a| a.kind == AnnotKind::Square).unwrap();
    let callout = all.iter().find(|a| a.kind == AnnotKind::FreeText).unwrap();
    assert!(cloud.style.cloud.is_some());
    assert!(rect_near(
        rect_of(cloud),
        Rect::new(300.0, 620.0, 420.0, 700.0)
    ));
    let free = callout.free_text.as_ref().unwrap();
    assert_eq!(free.intent, FreeTextIntent::Callout);
    assert_eq!(callout.meta.contents.as_deref(), Some("Revised kerb line"));
    // The leader starts on the cloud's rectangle.
    let tip = free.callout[0];
    let on_edge = (tip.x - 300.0).abs() < 0.01
        || (tip.x - 420.0).abs() < 0.01
        || (tip.y - 620.0).abs() < 0.01
        || (tip.y - 700.0).abs() < 0.01;
    assert!(on_edge, "{tip:?}");
    // Both came in one undo step.
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    assert!(markups(&h).is_empty());

    // Eraser: a pen stroke crossed by a drag is removed; a stroke it misses stays.
    set_tool(&mut h, Tool::Pen);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 700.0),
        Point::new(260.0, 700.0),
    );
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 600.0),
        Point::new(260.0, 600.0),
    );
    assert_eq!(markups(&h).len(), 2);
    h.key_press(Key::Escape);
    h.run();
    h.key_press(Key::X);
    h.run();
    assert_eq!(h.state().tool(), Tool::Eraser);
    drag_on_page(
        &mut h,
        0,
        Point::new(180.0, 730.0),
        Point::new(180.0, 670.0),
    );
    let left = markups(&h);
    assert_eq!(left.len(), 1, "the stroke that was crossed is gone");
    assert!((left[0].bounds().center().y - 600.0).abs() < 5.0);

    // Squiggly underline: over text the page has (the mock backend is given one line).
    let mock = std::sync::Arc::new(pl_doc::pl_render::MockBackend::with_pages(&letters(1)));
    let chars: Vec<pl_doc::pl_render::TextChar> = "Kerb line"
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let x = 100.0 + 10.0 * i as f64;
            pl_doc::pl_render::TextChar {
                ch,
                bbox: [x, 596.0, x + 10.0, 614.0],
                origin: [x, 600.0],
                font_size: 18.0,
                generated: false,
            }
        })
        .collect();
    mock.set_page_text(pl_doc::pl_render::PageText { page: 0, chars });
    let mut state = state_with(mock, None, ScriptedDialogs::default());
    state.open_bytes("fixture.pdf", Vec::new()).unwrap();
    let mut h = harness_dt(state, 0.25);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    h.key_press(Key::Z);
    h.run();
    assert_eq!(h.state().tool(), Tool::TextSquiggly);
    settle(&mut h);
    drag_on_page(
        &mut h,
        0,
        Point::new(102.0, 605.0),
        Point::new(137.0, 605.0),
    );
    let squiggly = markups(&h).pop().expect("a squiggly underline");
    assert_eq!(squiggly.kind, AnnotKind::Squiggly);
    assert_eq!(squiggly.meta.contents.as_deref(), Some("Kerb"));
    let pl_doc::pl_annot::Geometry::Quads(quads) = &squiggly.geometry else {
        panic!("{:?}", squiggly.geometry)
    };
    assert_eq!(quads.len(), 1);
    assert!(rect_near(
        quad_bounds(quads),
        Rect::new(100.0, 596.0, 140.0, 614.0)
    ));
}
