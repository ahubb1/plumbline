//! Scripted input sequences for the text and placement tools (Text Box, Typewriter, Callout,
//! Cloud Box, Cloud Callout, Stamp, Image, the text markup tools and the Eraser), with golden
//! geometry on all eight page layouts. Scripts and expectations are in screen pixels (see
//! `common`), so "top-left", "below" and "upright" mean what the user sees on a rotated page.

mod common;

use std::sync::Arc;

use common::*;
use pl_annot::{
    callout_route, AnnotId, AnnotKind, Annotation, Flags, FreeTextIntent, Geometry, LineEnding,
    Quad, RgbaImage, StampContent, StampSpec, Style,
};
use pl_draw::text::measure_text;
use pl_draw::{layout_text, FontStyle};
use pl_geom::{Point, Rect, Vec2};
use pl_tools::{
    step_text, to_display, Intent, Key, TextInput, ToolInput, ToolKind, ToolPreset, ToolState,
    CALLOUT_BOX_SIZE, TEXT_BOX_SIZE,
};

/// The one intent of a text tool's finished pointer gesture: a request for the text of a
/// markup that is not in the document yet.
#[track_caller]
fn requested(rig: &Rig, intents: &[Intent]) -> Annotation {
    let [Intent::RequestTextEdit(id)] = intents else {
        panic!("expected only a text request, got {intents:?}");
    };
    let draft = rig.state.text_draft().expect("the tool waits for text");
    assert_eq!(draft.id, *id);
    assert!(
        rig.view.get(id).is_none(),
        "nothing is created before the text"
    );
    assert_eq!(rig.view.text_edit.as_ref(), Some(id));
    assert!(rig.state.is_busy());
    assert_eq!(rig.state.page(), Some(PAGE));
    draft.clone()
}

/// The text reads left to right and bottom to top as the user sees the page.
#[track_caller]
fn assert_upright(l: &Layout, a: &Annotation) {
    assert_eq!(
        i32::from(a.rotation()),
        l.rotation().degrees(),
        "{}",
        l.name
    );
    let (_, to_page) = a.text_frame().expect("a text markup");
    let o = to_page * Point::new(0.0, 0.0);
    let along = to_page * Point::new(1.0, 0.0) - o;
    let up = to_page * Point::new(0.0, 1.0) - o;
    assert_eq!(
        to_display(l.rotation(), along),
        Vec2::new(1.0, 0.0),
        "{}",
        l.name
    );
    assert_eq!(
        to_display(l.rotation(), up),
        Vec2::new(0.0, -1.0),
        "{}",
        l.name
    );
}

fn unit(l: &Layout) -> f64 {
    l.geometry.user_unit()
}

// ---------------------------------------------------------------------------------------
// Text Box
// ---------------------------------------------------------------------------------------

#[test]
fn a_dragged_text_box_waits_for_its_text_and_is_created_with_it() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::TextBox);
        let intents = rig.drag((100.0, 100.0), (260.0, 180.0), NONE);
        let draft = requested(&rig, &intents);
        let want = l.rect(100.0, 100.0, 260.0, 180.0);
        assert_eq!(draft.geometry, Geometry::Rect(want), "{}", l.name);
        assert_eq!(draft.kind, AnnotKind::FreeText);
        assert_eq!(draft.meta.contents.as_deref(), Some(""));
        assert!(
            !rig.preview.is_empty(),
            "the empty box is shown while typing"
        );
        assert!(rig.on_page().is_empty());

        // The pointer does nothing while the editor is open.
        let before = rig.state.clone();
        assert!(rig.click(300.0, 300.0, NONE).is_empty());
        assert!(rig.drag((20.0, 20.0), (90.0, 90.0), NONE).is_empty());
        assert_eq!(rig.state, before, "{}", l.name);

        let a = created(&rig.commit("Check dimension\non site"));
        assert_eq!(a.id, draft.id, "the id the editor was opened for");
        assert_eq!(a.page, PAGE);
        assert_eq!(a.geometry, Geometry::Rect(want), "{}: as drawn", l.name);
        assert_eq!(a.meta.contents.as_deref(), Some("Check dimension\non site"));
        assert_eq!(a.meta.subject.as_deref(), Some("Text Box"));
        assert_eq!(a.meta.author.as_deref(), Some("Plumbline Tester"));
        assert_eq!(a.style, ToolPreset::default_for(ToolKind::TextBox).style);
        let t = a.free_text.as_ref().expect("a modelled text box");
        assert_eq!(t.intent, FreeTextIntent::Box);
        assert_eq!(t.font, FontStyle::Regular);
        assert_eq!(t.size, 12.0 / unit(&l), "{}: 12 pt on paper", l.name);
        assert_eq!(t.padding, 3.0 / unit(&l));
        assert!(t.callout.is_empty());
        assert!(a.is_modelled() && a.baseline().is_none());
        assert_upright(&l, &a);
        a.validate().expect("the text can be written");
        assert!(!a.draw_list().is_empty());
        assert_eq!(rig.selection(), std::slice::from_ref(&a.id));
        assert_eq!(rig.on_page().len(), 1);
        assert!(!rig.state.is_busy() && rig.state.text_draft().is_none());
        assert!(rig.preview.is_empty());
    }
}

#[test]
fn a_clicked_text_box_has_the_default_size_and_grows_down_the_screen() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::TextBox);
        let draft = {
            let intents = rig.click(100.0, 100.0, NONE);
            requested(&rig, &intents)
        };
        let (w, h) = TEXT_BOX_SIZE;
        let default = Rect::new(100.0, 100.0, 100.0 + w, 100.0 + h);
        assert_rect(on_screen(&l, rect_of(&draft)), default, l.name);

        // Two lines fit: the box stays.
        let a = created(&rig.commit("Fits in\ntwo lines"));
        assert_eq!(a.geometry, draft.geometry, "{}", l.name);
        assert_upright(&l, &a);

        // Six lines do not: the top-left corner on screen and the width stay, the bottom
        // moves down to hold them, to the next quarter unit.
        rig.click(100.0, 200.0, NONE);
        let a = created(&rig.commit("one\ntwo\nthree\nfour\nfive\nsix"));
        let got = on_screen(&l, rect_of(&a));
        assert!(
            (got.x0 - 100.0).abs() < 1e-9 && (got.y0 - 200.0).abs() < 1e-9,
            "{}",
            l.name
        );
        assert!((got.width() - w).abs() < 1e-9, "{}", l.name);
        assert!(got.height() > h + 20.0, "{}: {got:?}", l.name);
        let layout = a.text_layout().unwrap().unwrap();
        assert_eq!(layout.lines.len(), 6);
        assert!(layout.overflow.is_none(), "{}: nothing is clipped", l.name);
        // Exactly: what the text needs inside the border, plus the border, rounded up.
        let (frame, _) = a.text_frame().unwrap();
        let slack = frame.height() - layout.overflow.height_needed;
        assert!(
            (0.0..0.26 + 1e-9).contains(&slack),
            "{}: slack {slack}",
            l.name
        );
        let page_height = got.height() * l.px;
        assert!(
            ((page_height * 4.0).round() - page_height * 4.0).abs() < 1e-6,
            "{}: {page_height} is a multiple of a quarter",
            l.name
        );
    }
}

#[test]
fn text_is_abandoned_by_escape_cancel_or_emptiness() {
    for l in layouts() {
        for tool in [ToolKind::TextBox, ToolKind::Typewriter] {
            let mut rig = Rig::new(&l, tool);
            // Esc while the box is being dragged.
            rig.down(100.0, 100.0, NONE);
            rig.mv(200.0, 160.0, NONE);
            assert!(rig.state.is_busy());
            assert!(rig.key(Key::Escape, NONE).is_empty());
            assert!(!rig.state.is_busy());
            assert!(rig.up(200.0, 160.0, NONE).is_empty());
            assert!(
                !rig.state.is_busy(),
                "{}: a stray release starts nothing",
                l.name
            );

            // Esc while the editor is open.
            {
                let intents = rig.drag((100.0, 100.0), (200.0, 160.0), NONE);
                requested(&rig, &intents)
            };
            assert!(rig.key(Key::Escape, NONE).is_empty());
            assert!(!rig.state.is_busy() && rig.preview.is_empty());

            // The editor is cancelled.
            rig.drag((100.0, 100.0), (200.0, 160.0), NONE);
            assert!(rig.text(TextInput::Cancel).is_empty());
            assert!(!rig.state.is_busy());

            // The editor closes with nothing typed, or only white space.
            for empty in ["", "  \n\t "] {
                rig.drag((100.0, 100.0), (200.0, 160.0), NONE);
                assert!(rig.commit(empty).is_empty(), "{}", l.name);
                assert!(!rig.state.is_busy());
            }
            assert!(rig.on_page().is_empty(), "{} {tool:?}", l.name);
            assert!(rig.selection().is_empty());
        }
        // A tool that is not waiting for text ignores the editor.
        for tool in [ToolKind::Select, ToolKind::Rectangle, ToolKind::TextBox] {
            let state = ToolState::for_tool(tool);
            let rig = Rig::new(&l, tool);
            let (after, intents, preview) =
                step_text(state.clone(), &TextInput::Commit("stray".into()), &rig.view);
            assert_eq!(after, state);
            assert!(intents.is_empty() && preview.is_empty());
        }
    }
}

#[test]
fn text_that_cannot_be_written_is_refused_and_asked_for_again() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::TextBox);
        let draft = {
            let intents = rig.drag((100.0, 100.0), (260.0, 180.0), NONE);
            requested(&rig, &intents)
        };
        // Greek is outside WinAnsi: nothing is created, the tool keeps waiting with the same
        // draft, and the editor is asked for again.
        let intents = rig.commit("height \u{03b1}\u{03b2}");
        assert_eq!(
            intents,
            [Intent::RequestTextEdit(draft.id.clone())],
            "{}",
            l.name
        );
        assert!(rig.on_page().is_empty());
        assert_eq!(rig.state.text_draft(), Some(&draft));
        // The draft with the text says what is wrong.
        let mut probe = draft.clone();
        probe.meta.contents = Some("height \u{03b1}\u{03b2}".into());
        let message = probe.validate().unwrap_err().to_string();
        assert!(!message.is_empty());
        // Corrected, it goes through.
        let a = created(&rig.commit("height ab"));
        assert_eq!(a.id, draft.id);
        assert_eq!(a.meta.contents.as_deref(), Some("height ab"));
    }
}

#[test]
fn the_preset_styles_the_text_and_is_read_when_the_text_arrives() {
    use pl_annot::{HAlign, Rgb, VAlign};
    for l in layouts() {
        let mut preset = ToolPreset::default_for(ToolKind::TextBox);
        preset.text.size = 18.0;
        preset.text.color = Rgb::new(0.0, 0.2, 0.6);
        preset.text.align = HAlign::Centre;
        preset.text.v_align = VAlign::Middle;
        preset.text.font = FontStyle::Bold;
        preset.style.fill = Some(Rgb::new(1.0, 1.0, 0.85));
        preset.style.stroke = Some(Rgb::new(0.0, 0.2, 0.6));
        preset.style.width = 2.0;
        preset.subject = "Site note".into();
        let mut rig = Rig::new(&l, ToolKind::TextBox);
        rig.state = ToolState::new(preset.clone());
        rig.drag((100.0, 100.0), (300.0, 200.0), NONE);
        let draft = rig.state.text_draft().unwrap().clone();
        assert_eq!(draft.style, preset.style);
        // The user changes the colour while typing: the finished box has it.
        preset.text.color = Rgb::new(0.5, 0.0, 0.0);
        rig.state = rig.state.clone().with_preset(preset.clone());
        assert!(rig.state.is_busy(), "the same tool keeps waiting");
        let a = created(&rig.commit("Bold and centred"));
        let t = a.free_text.as_ref().unwrap();
        assert_eq!(t.size, 18.0 / unit(&l), "{}", l.name);
        assert_eq!(t.color, Rgb::new(0.5, 0.0, 0.0));
        assert_eq!(
            (t.align, t.v_align, t.font),
            (HAlign::Centre, VAlign::Middle, FontStyle::Bold)
        );
        assert_eq!(a.style, preset.style);
        assert_eq!(a.meta.subject.as_deref(), Some("Site note"));
        assert_upright(&l, &a);
        // Another tool's preset ends the wait.
        rig.drag((100.0, 100.0), (300.0, 200.0), NONE);
        rig.state = rig
            .state
            .clone()
            .with_preset(ToolPreset::default_for(ToolKind::Callout));
        assert!(!rig.state.is_busy());
    }
}

// ---------------------------------------------------------------------------------------
// Typewriter
// ---------------------------------------------------------------------------------------

#[test]
fn a_typewriter_click_is_fitted_to_its_text() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Typewriter);
        let draft = {
            let intents = rig.click(100.0, 100.0, NONE);
            requested(&rig, &intents)
        };
        let u = unit(&l);
        // One line of the default width until there is text.
        assert_rect(
            on_screen(&l, rect_of(&draft)),
            Rect::new(
                100.0,
                100.0,
                100.0 + TEXT_BOX_SIZE.0,
                100.0 + 12.0 * 1.25 + 2.0,
            ),
            l.name,
        );
        assert_eq!(draft.style.stroke, None);
        assert_eq!(draft.style.width, 0.0);
        assert!(rig.preview.is_empty(), "typewriter text has no box to show");

        let text = "Hello\nlonger line here";
        let a = created(&rig.commit(text));
        let t = a.free_text.as_ref().unwrap();
        assert_eq!(t.intent, FreeTextIntent::TypeWriter);
        assert_eq!((t.size, t.padding), (12.0 / u, 1.0 / u), "{}", l.name);
        assert_eq!(a.meta.subject.as_deref(), Some("Typewriter"));
        assert_upright(&l, &a);

        // The box is the widest line by the two lines, plus the padding, rounded up to a
        // quarter unit (with a hundredth to spare); its top-left on screen is the click.
        let snug = |v: f64| ((v + 0.01) * 4.0).ceil() / 4.0;
        let widest = measure_text("longer line here", FontStyle::Regular, t.size)
            .unwrap()
            .width;
        let roomy = layout_text(text, Rect::new(0.0, 0.0, 5000.0, 5000.0), &t.box_style()).unwrap();
        let want_w = snug(widest + 2.0 * t.padding);
        let want_h = snug(roomy.overflow.height_needed);
        assert_rect(
            on_screen(&l, rect_of(&a)),
            Rect::new(100.0, 100.0, 100.0 + want_w / l.px, 100.0 + want_h / l.px),
            l.name,
        );
        if l.name == "plain" {
            // The same in plain numbers, for Helvetica 12.
            assert_eq!((want_w, want_h), (84.75, 30.5));
        }
        let layout = a.text_layout().unwrap().unwrap();
        assert_eq!(layout.lines.len(), 2, "{}: no line is wrapped", l.name);
        assert!(layout.overflow.is_none());

        // A dragged box sets the width to wrap at; the height is fitted.
        rig.drag((100.0, 200.0), (180.0, 400.0), NONE);
        let a = created(&rig.commit("wrap these four words"));
        let got = on_screen(&l, rect_of(&a));
        assert!(
            (got.x0 - 100.0).abs() < 1e-9 && (got.y0 - 200.0).abs() < 1e-9,
            "{}",
            l.name
        );
        assert!(
            (got.width() - 80.0).abs() < 1e-9,
            "{}: the width is kept",
            l.name
        );
        let layout = a.text_layout().unwrap().unwrap();
        assert!(
            layout.lines.len() >= 2 && layout.overflow.is_none(),
            "{}",
            l.name
        );
        let (frame, _) = a.text_frame().unwrap();
        let slack = frame.height() - layout.overflow.height_needed;
        assert!(
            (0.0..0.26 + 1e-9).contains(&slack),
            "{}: slack {slack}",
            l.name
        );
        assert!(got.height() < 200.0, "{}: shrunk to the text", l.name);
    }
}

// ---------------------------------------------------------------------------------------
// Callout
// ---------------------------------------------------------------------------------------

#[test]
fn a_callout_is_tip_knee_box_then_text() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Callout);
        assert!(rig.click(100.0, 300.0, NONE).is_empty());
        assert!(rig.state.is_busy(), "the tip is placed");
        rig.mv(130.0, 270.0, NONE);
        assert!(
            !rig.preview.is_empty(),
            "{}: the leader follows the pointer",
            l.name
        );
        assert!(rig.click(150.0, 240.0, NONE).is_empty());
        rig.down(200.0, 150.0, NONE);
        rig.mv(260.0, 175.0, NONE);
        assert!(!rig.preview.is_empty());
        rig.mv(320.0, 200.0, NONE);
        let intents = rig.up(320.0, 200.0, NONE);
        let draft = requested(&rig, &intents);
        let rect = l.rect(200.0, 150.0, 320.0, 200.0);
        assert_eq!(draft.geometry, Geometry::Rect(rect), "{}", l.name);
        // The knee is to the left of the box on screen (50 px) more than it is below it
        // (40 px), so the leader meets the middle of the box's left side.
        let want = vec![l.at(100.0, 300.0), l.at(150.0, 240.0), l.at(200.0, 175.0)];
        assert_eq!(
            draft.free_text.as_ref().unwrap().callout,
            want,
            "{}",
            l.name
        );

        let a = created(&rig.commit("See detail 5"));
        let t = a.free_text.as_ref().unwrap();
        assert_eq!(t.intent, FreeTextIntent::Callout);
        assert_eq!(t.callout, want, "{}", l.name);
        assert_eq!(a.geometry, Geometry::Rect(rect));
        assert_eq!(a.style, ToolPreset::default_for(ToolKind::Callout).style);
        assert_eq!(a.style.line_end, LineEnding::OpenArrow);
        assert_eq!(a.meta.subject.as_deref(), Some("Callout"));
        assert_eq!(a.meta.contents.as_deref(), Some("See detail 5"));
        assert_upright(&l, &a);
        a.validate().unwrap();
        assert_eq!(rig.selection(), std::slice::from_ref(&a.id));
        // The leader is part of what the callout paints and of where it is hit.
        assert!(a.hit_test(l.at(125.0, 270.0), 1.0 * l.px), "{}", l.name);

        // Without a knee the leader is routed by pl-annot.
        rig.click(100.0, 300.0, NONE);
        let intents = rig.drag((200.0, 150.0), (320.0, 200.0), NONE);
        let draft = requested(&rig, &intents);
        let route = callout_route(rect, l.at(100.0, 300.0));
        assert_eq!(
            draft.free_text.as_ref().unwrap().callout,
            route,
            "{}",
            l.name
        );
        assert_eq!(route[0], l.at(100.0, 300.0));
        rig.text(TextInput::Cancel);

        // Shift holds the knee to 45 degree steps from the tip, as seen on screen.
        rig.click(100.0, 300.0, NONE);
        rig.click(180.0, 290.0, SHIFT);
        let draft = {
            let intents = rig.drag((250.0, 150.0), (370.0, 200.0), NONE);
            requested(&rig, &intents)
        };
        assert_eq!(
            draft.free_text.as_ref().unwrap().callout[..2],
            [l.at(100.0, 300.0), l.at(180.0, 300.0)],
            "{}",
            l.name
        );
        rig.key(Key::Escape, NONE);
        assert!(!rig.state.is_busy());
        assert_eq!(
            rig.on_page().len(),
            1,
            "{}: only the committed one exists",
            l.name
        );
    }
}

#[test]
fn a_callout_box_of_the_default_size_by_click_or_double_click() {
    for l in layouts() {
        let (w, h) = CALLOUT_BOX_SIZE;
        let centred = |x: f64, y: f64| l.rect(x - w / 2.0, y - h / 2.0, x + w / 2.0, y + h / 2.0);
        let tip = l.at(100.0, 300.0);

        // The double-click as two clicks, as two clicks and the event, and as the event
        // alone or before its clicks: always the same box, routed without a knee.
        for script in 0..4 {
            let mut rig = Rig::new(&l, ToolKind::Callout);
            rig.click(100.0, 300.0, NONE);
            let mut intents = Vec::new();
            match script {
                0 => {
                    intents.extend(rig.click(250.0, 150.0, NONE));
                    intents.extend(rig.click(250.0, 150.0, NONE));
                }
                1 => {
                    intents.extend(rig.click(250.0, 150.0, NONE));
                    intents.extend(rig.click(250.0, 150.0, NONE));
                    intents.extend(rig.double(250.0, 150.0));
                }
                2 => intents.extend(rig.double(250.0, 150.0)),
                _ => {
                    intents.extend(rig.double(250.0, 150.0));
                    intents.extend(rig.click(250.0, 150.0, NONE));
                    intents.extend(rig.click(250.0, 150.0, NONE));
                }
            }
            let draft = requested(&rig, &intents);
            let rect = centred(250.0, 150.0);
            assert_rect(rect_of(&draft), rect, l.name);
            assert_eq!(
                draft.free_text.as_ref().unwrap().callout,
                callout_route(rect_of(&draft), tip),
                "{} script {script}",
                l.name
            );
        }

        // With a knee placed, a single click puts the box there and keeps the knee.
        let mut rig = Rig::new(&l, ToolKind::Callout);
        rig.click(100.0, 300.0, NONE);
        rig.click(150.0, 240.0, NONE);
        let draft = {
            let intents = rig.click(300.0, 160.0, NONE);
            requested(&rig, &intents)
        };
        assert_rect(rect_of(&draft), centred(300.0, 160.0), l.name);
        let leader = &draft.free_text.as_ref().unwrap().callout;
        assert_eq!(leader.len(), 3);
        assert_eq!(leader[1], l.at(150.0, 240.0), "{}", l.name);
        // The knee is 90 px left of the box and 60 px below its bottom edge on screen.
        assert_point(leader[2], l.at(240.0, 160.0), l.name);

        // A double-click on the tip itself is just the tip.
        let mut rig = Rig::new(&l, ToolKind::Callout);
        rig.click(100.0, 300.0, NONE);
        rig.click(101.0, 301.0, NONE);
        assert!(rig.double(101.0, 301.0).is_empty());
        let draft = {
            let intents = rig.drag((200.0, 150.0), (320.0, 200.0), NONE);
            requested(&rig, &intents)
        };
        assert_eq!(
            draft.free_text.as_ref().unwrap().callout,
            callout_route(l.rect(200.0, 150.0, 320.0, 200.0), tip),
            "{}",
            l.name
        );
    }
}

#[test]
fn a_callout_is_taken_back_point_by_point_or_cancelled() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Callout);
        rig.click(100.0, 300.0, NONE);
        rig.click(150.0, 240.0, NONE);
        rig.key(Key::Backspace, NONE);
        assert!(
            rig.state.is_busy(),
            "{}: the knee is gone, the tip stays",
            l.name
        );
        let draft = {
            let intents = rig.drag((200.0, 150.0), (320.0, 200.0), NONE);
            requested(&rig, &intents)
        };
        assert_eq!(
            draft.free_text.as_ref().unwrap().callout,
            callout_route(l.rect(200.0, 150.0, 320.0, 200.0), l.at(100.0, 300.0))
        );
        assert!(rig.text(TextInput::Cancel).is_empty());
        assert!(!rig.state.is_busy());

        rig.click(100.0, 300.0, NONE);
        rig.key(Key::Backspace, NONE);
        assert!(!rig.state.is_busy(), "{}: the tip is gone too", l.name);

        // Esc at every stage.
        rig.click(100.0, 300.0, NONE);
        rig.key(Key::Escape, NONE);
        assert!(!rig.state.is_busy());
        rig.click(100.0, 300.0, NONE);
        rig.click(150.0, 240.0, NONE);
        rig.down(200.0, 150.0, NONE);
        rig.mv(300.0, 200.0, NONE);
        rig.key(Key::Escape, NONE);
        assert!(!rig.state.is_busy() && rig.preview.is_empty());
        assert!(rig.up(300.0, 200.0, NONE).is_empty());
        rig.click(100.0, 300.0, NONE);
        rig.drag((200.0, 150.0), (320.0, 200.0), NONE);
        rig.key(Key::Escape, NONE);
        assert!(!rig.state.is_busy());
        assert!(rig.commit("too late").is_empty());
        assert!(rig.on_page().is_empty(), "{}", l.name);
    }
}

// ---------------------------------------------------------------------------------------
// Cloud Box, Cloud Callout
// ---------------------------------------------------------------------------------------

#[test]
fn a_cloud_is_dragged_as_a_rectangle() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::CloudBox);
        let a = created(&rig.drag((100.0, 100.0), (260.0, 200.0), NONE));
        assert_eq!(a.kind, AnnotKind::Square);
        assert_eq!(
            a.geometry,
            Geometry::Rect(l.rect(100.0, 100.0, 260.0, 200.0)),
            "{}",
            l.name
        );
        assert_eq!(a.style.cloud, Some(1.0));
        assert_eq!(a.meta.subject.as_deref(), Some("Cloud Box"));
        assert!(a.is_modelled() && !a.draw_list().is_empty());
        // Shift: square on screen; Ctrl: from the centre.
        let b = created(&rig.drag((100.0, 250.0), (220.0, 300.0), SHIFT));
        assert_eq!(
            b.geometry,
            Geometry::Rect(l.rect(100.0, 250.0, 220.0, 370.0)),
            "{}",
            l.name
        );
        let c = created(&rig.drag((300.0, 300.0), (340.0, 330.0), CTRL));
        assert_eq!(
            c.geometry,
            Geometry::Rect(l.rect(260.0, 270.0, 340.0, 330.0)),
            "{}",
            l.name
        );
        // The polygon cloud is still there, point by point.
        assert_eq!(ToolKind::Cloud.annot_kind(), Some(AnnotKind::Polygon));
    }
}

#[test]
fn a_cloud_callout_is_a_cloud_with_a_text_box_led_from_it() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::CloudCallout);
        assert!(rig.drag((60.0, 60.0), (200.0, 160.0), NONE).is_empty());
        assert!(
            rig.state.is_busy(),
            "{}: the cloud is drawn, the box is next",
            l.name
        );
        assert!(!rig.preview.is_empty());
        assert!(rig.on_page().is_empty());
        let intents = rig.drag((260.0, 220.0), (380.0, 270.0), NONE);
        let draft = requested(&rig, &intents);
        let cloud_rect = l.rect(60.0, 60.0, 200.0, 160.0);
        let box_rect = l.rect(260.0, 220.0, 380.0, 270.0);
        assert_eq!(draft.geometry, Geometry::Rect(box_rect));
        // The leader starts at the point of the cloud's rectangle nearest the middle of the
        // box: here its bottom-right corner on screen.
        let tip = l.at(200.0, 160.0);
        assert_eq!(
            draft.free_text.as_ref().unwrap().callout,
            callout_route(box_rect, tip)
        );

        let intents = rig.commit("Revise per RFI");
        let [Intent::Create(cloud), Intent::Create(text), Intent::Select(ids)] = &intents[..]
        else {
            panic!(
                "{}: expected two creates and a select, got {intents:?}",
                l.name
            );
        };
        assert_eq!(*ids, [cloud.id.clone(), text.id.clone()]);
        assert_eq!(cloud.kind, AnnotKind::Square);
        assert_eq!(cloud.geometry, Geometry::Rect(cloud_rect), "{}", l.name);
        assert_eq!(cloud.style.cloud, Some(1.0));
        assert_eq!(cloud.meta.subject.as_deref(), Some("Cloud Callout"));
        assert_eq!(text.kind, AnnotKind::FreeText);
        assert_eq!(text.id, draft.id);
        let t = text.free_text.as_ref().unwrap();
        assert_eq!(t.intent, FreeTextIntent::Callout);
        assert_eq!(t.callout[0], tip, "{}", l.name);
        assert_eq!(text.style.cloud, None, "the text box has a plain border");
        assert_eq!(text.style.line_end, LineEnding::None);
        assert_eq!(text.meta.contents.as_deref(), Some("Revise per RFI"));
        assert_upright(&l, text);
        assert_eq!(rig.selection(), *ids);
        assert_eq!(rig.on_page().len(), 2);
        assert!(!rig.state.is_busy());

        // A click places the default box; no text leaves the cloud alone; cancel leaves
        // nothing.
        rig.drag((60.0, 300.0), (160.0, 380.0), NONE);
        let draft = {
            let intents = rig.click(300.0, 340.0, NONE);
            requested(&rig, &intents)
        };
        let (w, h) = CALLOUT_BOX_SIZE;
        assert_rect(
            rect_of(&draft),
            l.rect(
                300.0 - w / 2.0,
                340.0 - h / 2.0,
                300.0 + w / 2.0,
                340.0 + h / 2.0,
            ),
            l.name,
        );
        assert_point(
            draft.free_text.as_ref().unwrap().callout[0],
            l.at(160.0, 340.0),
            l.name,
        );
        let lone = created(&rig.commit(""));
        assert_eq!(lone.kind, AnnotKind::Square);
        assert_eq!(
            lone.geometry,
            Geometry::Rect(l.rect(60.0, 300.0, 160.0, 380.0))
        );
        assert_eq!(rig.on_page().len(), 3);

        rig.drag((60.0, 300.0), (160.0, 380.0), NONE);
        rig.drag((260.0, 320.0), (380.0, 370.0), NONE);
        assert!(rig.text(TextInput::Cancel).is_empty());
        rig.drag((60.0, 300.0), (160.0, 380.0), NONE);
        rig.key(Key::Backspace, NONE);
        assert!(
            !rig.state.is_busy(),
            "{}: Backspace takes the cloud back",
            l.name
        );
        assert_eq!(rig.on_page().len(), 3, "{}", l.name);
    }
}

// ---------------------------------------------------------------------------------------
// Stamp, Image
// ---------------------------------------------------------------------------------------

#[test]
fn a_stamp_is_placed_by_a_click_and_sized_by_a_drag() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Stamp);
        let (w, h) = StampSpec::approved().natural_size().unwrap();
        // A click: the designed size, centred on the click, the long side along the screen.
        let a = created(&rig.click(200.0, 200.0, NONE));
        assert_eq!(a.kind, AnnotKind::Stamp);
        assert_rect(
            on_screen(&l, rect_of(&a)),
            Rect::new(
                200.0 - w / 2.0,
                200.0 - h / 2.0,
                200.0 + w / 2.0,
                200.0 + h / 2.0,
            ),
            l.name,
        );
        let stamp = a.stamp.as_ref().expect("one of our stamps");
        assert_eq!(stamp.content, StampContent::Label(StampSpec::approved()));
        assert_eq!(
            i32::from(stamp.rotation),
            l.rotation().degrees(),
            "{}: upright",
            l.name
        );
        assert_eq!(a.style.stroke, None);
        assert_eq!(a.meta.subject.as_deref(), Some("Stamp"));
        assert!(a.is_modelled() && !a.draw_list().is_empty());
        a.validate().unwrap();
        assert_eq!(rig.selection(), std::slice::from_ref(&a.id));

        // A drag keeps the proportions: here the 200 px across asks for more than the 30
        // down, so the stamp is 200 px wide on screen.
        rig.down(100.0, 300.0, NONE);
        rig.mv(200.0, 315.0, NONE);
        assert!(!rig.preview.is_empty());
        rig.mv(300.0, 330.0, NONE);
        let b = created(&rig.up(300.0, 330.0, NONE));
        let k = 200.0 / w;
        assert_rect(
            on_screen(&l, rect_of(&b)),
            Rect::new(100.0, 300.0, 300.0, 300.0 + k * h),
            l.name,
        );
        // Up and to the left, the tall offset winning.
        let c = created(&rig.drag((400.0, 400.0), (390.0, 324.0), NONE));
        let k = 76.0 / h;
        assert_rect(
            on_screen(&l, rect_of(&c)),
            Rect::new(400.0 - k * w, 324.0, 400.0, 400.0),
            l.name,
        );
        // Shift frees the proportions.
        let d = created(&rig.drag((100.0, 400.0), (160.0, 440.0), SHIFT));
        assert_eq!(
            d.geometry,
            Geometry::Rect(l.rect(100.0, 400.0, 160.0, 440.0)),
            "{}",
            l.name
        );

        // Esc abandons a drag.
        rig.down(20.0, 20.0, NONE);
        rig.mv(80.0, 60.0, NONE);
        rig.key(Key::Escape, NONE);
        assert!(rig.up(80.0, 60.0, NONE).is_empty());
        assert_eq!(rig.on_page().len(), 4, "{}", l.name);

        // The preset names the stamp.
        let spec = StampSpec::void().with_detail("superseded");
        let mut rig = Rig::new(&l, ToolKind::Stamp);
        rig.state = ToolState::new(ToolPreset::for_stamp(spec.clone()));
        let v = created(&rig.click(200.0, 200.0, NONE));
        assert_eq!(
            v.stamp.as_ref().unwrap().content,
            StampContent::Label(spec.clone())
        );
        let (w, h) = spec.natural_size().unwrap();
        let got = on_screen(&l, rect_of(&v));
        assert!(
            (got.width() - w).abs() < 1e-9 && (got.height() - h).abs() < 1e-9,
            "{}",
            l.name
        );
    }
}

fn picture(width: u32, height: u32) -> Arc<RgbaImage> {
    let pixels = (0..width * height)
        .flat_map(|i| {
            [
                (i % 251) as u8,
                (i % 7 * 30) as u8,
                90,
                if i % 3 == 0 { 255 } else { 128 },
            ]
        })
        .collect();
    Arc::new(RgbaImage::new(width, height, pixels).unwrap())
}

#[test]
fn a_picture_keeps_its_proportions_unless_shift_is_held() {
    for l in layouts() {
        // Without a picture the tool does nothing.
        let mut rig = Rig::new(&l, ToolKind::Image);
        assert!(rig.click(200.0, 200.0, NONE).is_empty());
        assert!(rig.drag((100.0, 100.0), (300.0, 200.0), NONE).is_empty());
        assert!(!rig.state.is_busy());

        // 200 x 100 pixels is 150 x 75 points.
        let image = picture(200, 100);
        rig.state = ToolState::for_tool(ToolKind::Image).with_image(image.clone());
        let a = created(&rig.click(200.0, 200.0, NONE));
        assert_eq!(a.kind, AnnotKind::Stamp);
        assert_rect(
            on_screen(&l, rect_of(&a)),
            Rect::new(125.0, 162.5, 275.0, 237.5),
            l.name,
        );
        let stamp = a.stamp.as_ref().unwrap();
        assert_eq!(stamp.content, StampContent::Image(image.clone()));
        assert_eq!(i32::from(stamp.rotation), l.rotation().degrees());
        assert_eq!(a.meta.subject.as_deref(), Some("Image"));
        assert!(a.is_modelled() && !a.draw_list().is_empty());

        // A drag: 200 px across asks for 4/3 of the size, 50 px down for 2/3.
        let b = created(&rig.drag((100.0, 300.0), (300.0, 350.0), NONE));
        assert_rect(
            on_screen(&l, rect_of(&b)),
            Rect::new(100.0, 300.0, 300.0, 400.0),
            l.name,
        );
        // Shift: whatever is dragged.
        let c = created(&rig.drag((100.0, 300.0), (300.0, 350.0), SHIFT));
        assert_eq!(
            c.geometry,
            Geometry::Rect(l.rect(100.0, 300.0, 300.0, 350.0)),
            "{}",
            l.name
        );

        // A large picture is placed at no more than half the page as displayed.
        rig.state = ToolState::for_tool(ToolKind::Image).with_image(picture(2000, 1000));
        let d = created(&rig.click(200.0, 200.0, NONE));
        let got = on_screen(&l, rect_of(&d));
        let shown = l.geometry.display_size();
        let (page_w, page_h) = (shown.width / l.px, shown.height / l.px);
        assert!(
            (got.width() / got.height() - 2.0).abs() < 1e-9,
            "{}",
            l.name
        );
        let share = (got.width() / page_w).max(got.height() / page_h);
        assert!(
            (share - 0.5).abs() < 1e-9,
            "{}: {got:?} on {page_w} x {page_h}",
            l.name
        );
        assert!(((got.x0 + got.x1) / 2.0 - 200.0).abs() < 1e-9);
    }
}

// ---------------------------------------------------------------------------------------
// Text markups
// ---------------------------------------------------------------------------------------

/// The quad of a character whose box on screen is `x0..x1` by `y0..y1`: it starts at the
/// start of the baseline (bottom-left on screen) and goes round anticlockwise.
fn char_quad(l: &Layout, x0: f64, y0: f64, x1: f64, y1: f64) -> Quad {
    [l.at(x0, y1), l.at(x1, y1), l.at(x1, y0), l.at(x0, y0)]
}

/// Two lines of ten 8 px characters, upright on screen: one at y 100..112, one at 120..132,
/// both starting at x 100.
fn two_lines(l: &Layout) -> Vec<(usize, Quad)> {
    let mut text = Vec::new();
    for top in [100.0, 120.0] {
        for i in 0..10 {
            let x = 100.0 + 8.0 * f64::from(i);
            text.push((PAGE, char_quad(l, x, top, x + 8.0, top + 12.0)));
        }
    }
    // Text on another page is never picked up.
    text.push((0, char_quad(l, 100.0, 100.0, 108.0, 112.0)));
    text
}

#[track_caller]
fn assert_quads(got: &Geometry, want: &[Quad], what: &str) {
    let Geometry::Quads(got) = got else {
        panic!("{what}: not quads: {got:?}");
    };
    assert_eq!(got.len(), want.len(), "{what}: {got:?}");
    for (g, w) in got.iter().zip(want) {
        for (p, q) in g.iter().zip(w) {
            assert!((*p - *q).hypot() < 1e-9, "{what}: got {g:?}, want {w:?}");
        }
    }
}

#[test]
fn text_markups_cover_the_dragged_text_one_quad_per_line() {
    let tools = [
        (ToolKind::TextHighlight, AnnotKind::Highlight, "Highlight"),
        (ToolKind::TextUnderline, AnnotKind::Underline, "Underline"),
        (ToolKind::TextStrikeOut, AnnotKind::StrikeOut, "Strikeout"),
        (ToolKind::TextSquiggly, AnnotKind::Squiggly, "Squiggly"),
    ];
    for l in layouts() {
        for (tool, kind, subject) in tools.clone() {
            let mut rig = Rig::new(&l, tool);
            rig.view.text = two_lines(&l);
            // From the third character of the first line to the sixth of the second.
            rig.down(118.0, 106.0, NONE);
            rig.mv(150.0, 110.0, NONE);
            assert!(
                !rig.preview.is_empty(),
                "{} {tool:?}: the marking shows",
                l.name
            );
            rig.mv(142.0, 126.0, NONE);
            let intents = rig.up(142.0, 126.0, NONE);
            assert_eq!(intents.len(), 1, "{}: created, not selected", l.name);
            let a = created(&intents);
            assert_eq!(a.kind, kind);
            assert_eq!(a.page, PAGE);
            let want = [
                char_quad(&l, 116.0, 100.0, 180.0, 112.0),
                char_quad(&l, 100.0, 120.0, 148.0, 132.0),
            ];
            assert_quads(&a.geometry, &want, &format!("{} {tool:?}", l.name));
            assert_eq!(a.style, Style::default_for(&kind));
            assert_eq!(a.meta.subject.as_deref(), Some(subject));
            assert!(a.is_modelled() && !a.draw_list().is_empty());
            assert!(rig.selection().is_empty());
            assert!(!rig.state.is_busy() && rig.preview.is_empty());

            // Dragged backwards: the same text.
            let b = created(&rig.drag((142.0, 126.0), (118.0, 106.0), NONE));
            assert_quads(&b.geometry, &want, l.name);
            // Within one line: one quad.
            let c = created(&rig.drag((101.0, 125.0), (130.0, 127.0), NONE));
            assert_quads(
                &c.geometry,
                &[char_quad(&l, 100.0, 120.0, 132.0, 132.0)],
                l.name,
            );

            // A click marks nothing; nor does Esc mid-drag; nor a drag over a page with no
            // text.
            assert!(rig.click(118.0, 106.0, NONE).is_empty());
            rig.down(118.0, 106.0, NONE);
            rig.mv(160.0, 106.0, NONE);
            rig.key(Key::Escape, NONE);
            assert!(rig.up(160.0, 106.0, NONE).is_empty());
            assert_eq!(rig.on_page().len(), 3, "{}", l.name);
            rig.view.text.clear();
            assert!(rig.drag((118.0, 106.0), (142.0, 126.0), NONE).is_empty());
            assert!(rig.preview.is_empty());
        }
    }
}

// ---------------------------------------------------------------------------------------
// Eraser
// ---------------------------------------------------------------------------------------

#[test]
fn the_eraser_rubs_out_the_pen_strokes_it_crosses() {
    for l in layouts() {
        let mut rig = Rig::new(&l, ToolKind::Eraser);
        let stroke = |x0: f64, y0: f64, x1: f64, y1: f64| vec![l.at(x0, y0), l.at(x1, y1)];
        let top = stroke(100.0, 100.0, 200.0, 100.0);
        let bottom = stroke(100.0, 200.0, 200.0, 200.0);
        let two: AnnotId = rig.add(Annotation::ink(PAGE, vec![top, bottom.clone()], &origin()));
        let one = rig.add(Annotation::ink(
            PAGE,
            vec![stroke(300.0, 100.0, 300.0, 200.0)],
            &origin(),
        ));
        let mut locked = Annotation::ink(PAGE, vec![stroke(100.0, 300.0, 200.0, 300.0)], &origin());
        locked.meta.flags = locked.meta.flags.with(Flags::LOCKED, true);
        let locked = rig.add(locked);
        let line = rig.add(Annotation::line(
            PAGE,
            l.at(100.0, 400.0),
            l.at(200.0, 400.0),
            &origin(),
        ));
        let dot = rig.add(Annotation::ink(
            PAGE,
            vec![vec![l.at(400.0, 400.0)]],
            &origin(),
        ));

        // 20 px clear of the end of a stroke: nothing.
        assert!(
            rig.drag((220.0, 80.0), (220.0, 120.0), NONE).is_empty(),
            "{}",
            l.name
        );
        // Across the top stroke: the drawing keeps its other stroke.
        let intents = rig.drag((150.0, 80.0), (150.0, 120.0), NONE);
        assert_eq!(intents.len(), 1, "{}: {intents:?}", l.name);
        assert_eq!(
            rig.geometry(&two),
            Geometry::Ink(vec![bottom]),
            "{}",
            l.name
        );
        // Across the only stroke of a drawing: it is deleted, as soon as it is touched.
        rig.down(280.0, 150.0, NONE);
        assert_eq!(rig.on_page().len(), 5);
        let intents = rig.mv(320.0, 150.0, NONE);
        assert_eq!(intents, [Intent::Delete(vec![one.clone()])], "{}", l.name);
        assert!(rig.view.get(&one).is_none());
        assert!(rig.up(320.0, 150.0, NONE).is_empty());
        assert!(!rig.state.is_busy());
        // A press within reach of a dot rubs it out without a drag.
        let intents = rig.click(403.0, 403.0, NONE);
        assert_eq!(intents, [Intent::Delete(vec![dot])], "{}", l.name);
        // A locked drawing and a line that is not ink are left alone.
        assert!(rig.drag((150.0, 280.0), (150.0, 320.0), NONE).is_empty());
        assert!(rig.drag((150.0, 380.0), (150.0, 420.0), NONE).is_empty());
        assert!(rig.view.get(&locked).is_some() && rig.view.get(&line).is_some());
        assert_eq!(rig.on_page().len(), 3, "{}", l.name);
        // One sweep through both remaining... the other stroke of the first drawing goes too.
        let intents = rig.drag((150.0, 180.0), (150.0, 220.0), NONE);
        assert_eq!(intents, [Intent::Delete(vec![two])], "{}", l.name);
        // The pointer simply hovering asks for the drawing cursor and changes nothing.
        let (state, intents, _) = pl_tools::step(
            rig.state.clone(),
            &ToolInput::PointerMove(rig.pointer(150.0, 300.0, NONE)),
            &rig.view,
        );
        assert!(!state.is_busy());
        assert_eq!(intents, [Intent::SetCursor(pl_tools::Cursor::Crosshair)]);
    }
}
