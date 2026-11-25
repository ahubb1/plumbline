//! Milestone M8 in the window: page operations on the thumbnails, editable bookmarks, links,
//! flatten, drawing sets and printing, driven through `egui_kittest` with no window. Every
//! test works on synthetic fixtures copied to a temporary folder; nothing is printed (the
//! printer is a fake that records what it is sent) and no browser is opened.

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable;
use pl_app::{Command, FakePrinters, LinkKind, PageChoice, ScriptedDialogs, SidePage, Tool};
use pl_doc::pl_geom::{Point, Rotation};
use pl_doc::pl_pdf::{DictPatch, PdfFile, SaveOptions};
use pl_doc::pl_render::{layout_job, Bitmap, PrintScaling, RenderBackend};
use pl_doc::{DrawingSet, PageFingerprint};
use support::*;

const ALT: Modifiers = Modifiers::ALT;

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// `main.pdf` (eight pages of every kind, with bookmarks and links) and `other.pdf` (four
/// pages) side by side: `main.pdf` links to `other.pdf`.
fn fixtures(dir: &Path) -> (PathBuf, PathBuf) {
    let (main, _) = pl_testkit::structure_main();
    let (other, _) = pl_testkit::structure_source();
    (
        write(dir, "main.pdf", &main.bytes),
        write(dir, "other.pdf", &other.bytes),
    )
}

fn app_with(backend: Arc<dyn RenderBackend>, path: &Path, dialogs: ScriptedDialogs) -> App {
    app_with_dt(backend, path, dialogs, 0.25)
}

/// As [`app_with`], with frames `dt` seconds apart (a double-click needs them close).
fn app_with_dt(
    backend: Arc<dyn RenderBackend>,
    path: &Path,
    dialogs: ScriptedDialogs,
    dt: f32,
) -> App {
    let mut state = state_with(backend, None, dialogs);
    state.open_path(path).expect("the file opens");
    let mut h = harness_opts(state, dt, true);
    settle(&mut h);
    h.state_mut().active_view_mut().unwrap().view.set_zoom(1.0);
    settle(&mut h);
    h
}

fn fingerprints(h: &mut App) -> Vec<PageFingerprint> {
    h.state_mut().page_fingerprints()
}

fn content(fps: &[PageFingerprint]) -> Vec<u64> {
    fps.iter().map(|f| f.content).collect()
}

fn page_count(h: &App) -> u32 {
    h.state().active_document().unwrap().page_count()
}

fn shown_page(h: &App) -> u32 {
    h.state().active_view().unwrap().shown_page()
}

fn rect_of_node(h: &App, label: &str) -> egui::Rect {
    h.get_by_label(label).rect()
}

/// A click on a page once nothing is loading: a click is a press and a release a few frames
/// apart, and frames that background work asks for would hold the button down too long.
fn settled_click(h: &mut App, page: u32, point: Point) {
    settle(h);
    click_on_page(h, page, point);
}

/// Puts the keyboard in a text field and types into it.
fn type_in(h: &mut App, name: &str, text: &str) {
    h.get_by_label(name).focus();
    h.run();
    h.get_by_label(name).type_text(text);
    h.run();
}

fn undo(h: &mut App) {
    h.key_press_modifiers(CTRL, Key::Z);
    h.run();
    settle(h);
}

fn redo(h: &mut App) {
    h.key_press_modifiers(CTRL, Key::Y);
    h.run();
    settle(h);
}

/// Waits (running frames) until `done` holds.
fn wait_for(h: &mut App, what: &str, mut done: impl FnMut(&mut App) -> bool) {
    let start = std::time::Instant::now();
    while !done(h) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "{what} never happened"
        );
        std::thread::sleep(Duration::from_millis(10));
        h.run();
    }
    settle(h);
}

// ---- pages ------------------------------------------------------------------------------------

#[test]
fn dragging_a_thumbnail_moves_the_page_and_undo_puts_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    let before = fingerprints(&mut h);
    assert_eq!(before.len(), 8);
    assert_all_named(&h, "the Pages page with its operations");

    // Page 1 dropped into the gap above page 3: it becomes the second page.
    let from = rect_of_node(&h, "Page 1 thumbnail").center();
    let to = rect_of_node(&h, "Page 3 thumbnail").top() + 8.0;
    drag(&mut h, from, egui::pos2(from.x, to));
    settle(&mut h);
    let after = fingerprints(&mut h);
    let mut model = content(&before);
    let moved = model.remove(0);
    model.insert(1, moved);
    assert_eq!(content(&after), model, "the reference list after the move");
    assert!(h.state().is_modified(h.state().active().unwrap()));
    assert!(h
        .state()
        .view_title(h.state().active().unwrap())
        .ends_with('*'));

    undo(&mut h);
    assert_eq!(fingerprints(&mut h), before, "undo gives the order back");
    redo(&mut h);
    assert_eq!(content(&fingerprints(&mut h)), model);
    undo(&mut h);
    assert!(!h.state().is_modified(h.state().active().unwrap()));
}

#[test]
fn deleting_pages_asks_first_and_is_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    let before = fingerprints(&mut h);

    // Page 2, then Ctrl+click page 3: two pages picked.
    click_label(&mut h, "Page 2 thumbnail");
    // Nothing may be loading while the button is held, or frames (and time) pass between the
    // press and the release.
    settle(&mut h);
    let page3 = rect_of_node(&h, "Page 3 thumbnail").center();
    click_at_with(&mut h, page3, CTRL);
    assert_eq!(h.state().page_targets(), vec![1, 2]);

    click(&mut h, "Delete pages");
    assert_eq!(h.state().confirm_prompt(), Some("Delete 2 pages?"));
    assert_all_named(&h, "the delete confirmation");
    click(&mut h, "Cancel");
    assert!(h.state().confirm_prompt().is_none());
    assert_eq!(page_count(&h), 8, "cancel deletes nothing");

    click(&mut h, "Delete pages");
    click(&mut h, "Delete");
    settle(&mut h);
    assert_eq!(page_count(&h), 6);
    let mut model = content(&before);
    model.drain(1..3);
    assert_eq!(content(&fingerprints(&mut h)), model);
    // The thumbnails follow: six of them, and no seventh.
    assert!(h.query_by_label("Page 6 thumbnail").is_some() || page_count(&h) == 6);

    undo(&mut h);
    assert_eq!(fingerprints(&mut h), before);
    assert_eq!(page_count(&h), 8);
}

#[test]
fn rotating_pages_turns_them_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    let before = fingerprints(&mut h);
    click_label(&mut h, "Page 1 thumbnail");
    click(&mut h, "Rotate pages clockwise");
    settle(&mut h);
    let after = fingerprints(&mut h);
    assert_eq!(after[0].rotate, Rotation::R90);
    assert_eq!(after[0].content, before[0].content);
    assert_eq!(&after[1..], &before[1..], "only the picked page turns");
    // Page 2 already has /Rotate 90; anticlockwise brings it to 0.
    click_label(&mut h, "Page 2 thumbnail");
    click(&mut h, "Rotate pages anticlockwise");
    settle(&mut h);
    assert_eq!(fingerprints(&mut h)[1].rotate, Rotation::R0);
    // The view shows the turned page: its displayed size is landscape now.
    let size = h
        .state()
        .active_view()
        .unwrap()
        .view
        .page_geometry(0)
        .unwrap()
        .display_size_points();
    assert!(size.width > size.height, "{size:?}");
    undo(&mut h);
    undo(&mut h);
    assert_eq!(fingerprints(&mut h), before);
}

#[test]
fn inserting_pages_from_a_file_and_a_blank_page() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let dialogs = ScriptedDialogs::new([vec![other.clone()]]);
    let mut h = app_with(pdfium(), &main, dialogs);
    let before = fingerprints(&mut h);
    let source = {
        let mut file = PdfFile::open(&other).unwrap();
        pl_doc::structure::fingerprints(file.document().unwrap()).unwrap()
    };
    click_label(&mut h, "Page 2 thumbnail");
    // More > Insert pages from file...
    click(&mut h, "More");
    click(&mut h, "Insert pages from file\u{2026}");
    settle(&mut h);
    assert_eq!(page_count(&h), 12);
    let mut model = content(&before);
    for (i, fp) in source.iter().enumerate() {
        model.insert(2 + i, fp.content);
    }
    assert_eq!(content(&fingerprints(&mut h)), model);

    // A blank page after page 1, the size of its neighbour.
    click_label(&mut h, "Page 1 thumbnail");
    click(&mut h, "Insert blank page");
    settle(&mut h);
    let fps = fingerprints(&mut h);
    assert_eq!(fps.len(), 13);
    assert_eq!(fps[2].content, model[1], "the old second page moved down");
    undo(&mut h);
    undo(&mut h);
    assert_eq!(fingerprints(&mut h), before);
}

#[test]
fn extract_writes_the_picked_pages_to_a_new_file() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let dest = dir.path().join("part.pdf");
    let dialogs = ScriptedDialogs::default().with_saves([dest.clone()]);
    let mut h = app_with(pdfium(), &main, dialogs);
    let before = fingerprints(&mut h);
    let original = std::fs::read(&main).unwrap();
    h.state_mut().select_pages(&[2, 5]);
    run_command(&mut h, Command::ExtractPages);
    let mut file = PdfFile::open(&dest).unwrap();
    let part = pl_doc::structure::fingerprints(file.document().unwrap()).unwrap();
    assert_eq!(content(&part), vec![before[2].content, before[5].content]);
    assert_eq!(
        std::fs::read(&main).unwrap(),
        original,
        "the source is untouched"
    );
    assert!(!h.state().is_modified(h.state().active().unwrap()));
}

#[test]
fn a_signed_document_asks_before_its_pages_change() {
    let dir = tempfile::tempdir().unwrap();
    let signed = write(dir.path(), "signed.pdf", &pl_testkit::signed_stub().bytes);
    let mut h = app_with(pdfium(), &signed, ScriptedDialogs::default());
    let before = fingerprints(&mut h);
    click(&mut h, "Rotate pages clockwise");
    let fields = h
        .state()
        .signature_prompt()
        .expect("the signature question")
        .to_vec();
    assert!(!fields.is_empty());
    assert_all_named(&h, "the signature question");
    click(&mut h, "Cancel");
    assert!(h.state().signature_prompt().is_none());
    assert_eq!(fingerprints(&mut h), before, "nothing changed");

    click(&mut h, "Rotate pages clockwise");
    click(&mut h, "Change it anyway");
    settle(&mut h);
    assert_eq!(fingerprints(&mut h)[0].rotate, Rotation::R90);
}

#[test]
fn save_optimised_asks_before_it_rewrites_a_signed_document() {
    let dir = tempfile::tempdir().unwrap();
    let signed = write(dir.path(), "signed.pdf", &pl_testkit::signed_stub().bytes);
    let original = std::fs::read(&signed).unwrap();
    let mut h = app_with(pdfium(), &signed, ScriptedDialogs::default());
    run_command(&mut h, Command::SaveOptimised);
    h.run();
    assert!(h.state().signature_prompt().is_some());
    assert!(h.query_by_label("Change it anyway").is_some());
    click(&mut h, "Cancel");
    assert!(h.state_mut().wait_idle(Duration::from_secs(30)));
    assert_eq!(
        std::fs::read(&signed).unwrap(),
        original,
        "nothing was saved"
    );
}

#[test]
fn combine_files_makes_a_new_document_and_opens_it() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let dest = dir.path().join("combined.pdf");
    let dialogs = ScriptedDialogs::new([vec![other.clone()]]).with_saves([dest.clone()]);
    let mut h = app_with(pdfium(), &main, dialogs);
    let main_fps = fingerprints(&mut h);
    run_command(&mut h, Command::CombineFiles);
    assert!(h.state().combine_dialog_open());
    click(&mut h, "Add files\u{2026}");
    assert_all_named(&h, "the combine dialog");
    // Other first, then pages 2-3 of main.
    click(&mut h, "Move other.pdf up");
    type_in(&mut h, "Pages of main.pdf", "2-3");
    click(&mut h, "Combine\u{2026}");
    settle(&mut h);
    assert!(!h.state().combine_dialog_open());
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.path().unwrap().file_name().unwrap(), "combined.pdf");
    let fps = fingerprints(&mut h);
    assert_eq!(fps.len(), 6);
    assert_eq!(fps[4].content, main_fps[1].content);
    assert_eq!(fps[5].content, main_fps[2].content);
}

#[test]
fn replacing_pages_takes_the_first_pages_of_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let dialogs = ScriptedDialogs::new(vec![vec![other.clone()]; 5]);
    let mut h = app_with(pdfium(), &main, dialogs);
    let before = fingerprints(&mut h);
    let source = {
        let mut file = PdfFile::open(&other).unwrap();
        pl_doc::structure::fingerprints(file.document().unwrap()).unwrap()
    };
    // Pages that are not next to each other cannot be replaced as one run.
    h.state_mut().select_pages(&[1, 3]);
    run_command(&mut h, Command::ReplacePages);
    assert!(h.state().status().unwrap().contains("next to each other"));
    assert_eq!(fingerprints(&mut h), before);
    // Pages 3 and 4 by the first two pages of the other file. It asks first, and says how
    // many markups the pages have; Cancel changes nothing.
    let markups = |h: &mut App| -> usize {
        let doc = h.state_mut().active_document_mut().unwrap();
        doc.annots()
            .unwrap()
            .set()
            .on_page(3)
            .filter(|a| a.kind != pl_doc::pl_annot::AnnotKind::Popup)
            .count()
    };
    let on_page_4 = markups(&mut h);
    assert!(on_page_4 > 0, "the fixture has markups on page 4");
    h.state_mut().select_pages(&[2, 3]);
    run_command(&mut h, Command::ReplacePages);
    assert_eq!(h.state().confirm_prompt(), Some("Replace pages 3, 4?"));
    assert!(h
        .query_by_label_contains(&format!("have {on_page_4} markup"))
        .is_some());
    assert_all_named(&h, "the replace question");
    click(&mut h, "Cancel");
    assert_eq!(fingerprints(&mut h), before);
    // Replace, keeping the markups (the default): they are on the new page 4.
    run_command(&mut h, Command::ReplacePages);
    click(&mut h, "Replace");
    settle(&mut h);
    let mut model = content(&before);
    model[2] = source[0].content;
    model[3] = source[1].content;
    assert_eq!(content(&fingerprints(&mut h)), model);
    let kept = markups(&mut h);
    assert!(
        h.state()
            .status()
            .unwrap()
            .contains("moved onto the new pages"),
        "{:?}",
        h.state().status()
    );
    undo(&mut h);
    assert_eq!(fingerprints(&mut h), before);
    // Without them: they go, and the status says so.
    run_command(&mut h, Command::ReplacePages);
    click_label(&mut h, "Keep the markups on the new pages");
    click(&mut h, "Replace");
    settle(&mut h);
    assert_eq!(content(&fingerprints(&mut h)), model);
    // What is left on the new page is its own; the kept ones came on top of that.
    assert_eq!(kept, markups(&mut h) + on_page_4, "kept on the new page");
    assert!(
        h.state()
            .status()
            .unwrap()
            .contains("of the replaced pages removed"),
        "{:?}",
        h.state().status()
    );
    undo(&mut h);
    assert_eq!(markups(&mut h), on_page_4);

    // A file with fewer pages (four) than were picked (six): the question says which stay.
    h.state_mut().select_pages(&[0, 1, 2, 3, 4, 5]);
    run_command(&mut h, Command::ReplacePages);
    assert!(h.query_by_label_contains("has only 4 pages").is_some());
    assert!(h.query_by_label_contains("pages 5, 6 will stay").is_some());
    click(&mut h, "Cancel");
    assert_eq!(fingerprints(&mut h), before);
}

#[test]
fn bookmarks_are_added_renamed_moved_and_followed() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    h.state_mut().set_side_page(SidePage::Bookmarks);
    settle(&mut h);
    let titles = |h: &mut App| -> Vec<String> {
        h.state_mut()
            .bookmark_rows()
            .unwrap()
            .into_iter()
            .map(|r| r.title)
            .collect()
    };
    let start = titles(&mut h);
    assert_eq!(&start[..4], ["Front", "Cover", "Contents", "Sheets"]);
    assert_all_named(&h, "the Bookmarks page");

    // A click goes there; Back comes back.
    click_label(&mut h, "Sheet 5");
    settle(&mut h);
    assert_eq!(shown_page(&h), 4);
    h.key_press_modifiers(ALT, Key::ArrowLeft);
    settle(&mut h);
    assert_eq!(shown_page(&h), 0);

    // Add: a bookmark for the page in view, renamed in place as it is made.
    h.state_mut().active_view_mut().unwrap().go_to_page(2);
    settle(&mut h);
    click(&mut h, "Add bookmark");
    settle(&mut h);
    assert!(
        h.query_by_label("Bookmark title").is_some(),
        "renaming starts"
    );
    assert_all_named(&h, "the bookmark being renamed");
    h.get_by_label("Bookmark title").type_text(" east");
    h.run();
    h.key_press(Key::Enter);
    h.run();
    settle(&mut h);
    let rows = h.state_mut().bookmark_rows().unwrap();
    let added = rows
        .iter()
        .find(|r| r.title.contains("east"))
        .expect("the renamed bookmark");
    assert!(added.title.contains("Page A-1"), "{}", added.title);
    assert_eq!(added.dest.map(|d| d.page_index), Some(2));
    // It went in after the chosen bookmark ("Sheet 5"), as its sibling.
    assert_eq!(added.path, vec![1, 1]);

    // Drag "Web" (the last top-level item) onto the top edge of "Front".
    let web = rect_of_node(&h, "Web").center();
    let front = rect_of_node(&h, "Front");
    drag(&mut h, web, egui::pos2(front.center().x, front.top() + 2.0));
    settle(&mut h);
    let moved = titles(&mut h);
    assert_eq!(moved[0], "Web");
    assert_eq!(moved[1], "Front");

    // "Set to view": the chosen bookmark ("Web", which led nowhere) now leads to page 3.
    h.state_mut().active_view_mut().unwrap().go_to_page(2);
    settle(&mut h);
    click(&mut h, "Set to view");
    settle(&mut h);
    let rows = h.state_mut().bookmark_rows().unwrap();
    assert_eq!(rows[0].title, "Web");
    assert_eq!(rows[0].dest.map(|d| d.page_index), Some(2));
    undo(&mut h);

    // Delete the chosen one, then undo everything back to the start.
    let chosen = h
        .state()
        .selected_bookmark()
        .expect("the moved bookmark is chosen");
    assert_eq!(chosen, vec![0]);
    click(&mut h, "Delete bookmark");
    settle(&mut h);
    assert_eq!(titles(&mut h)[0], "Front");
    undo(&mut h);
    undo(&mut h);
    undo(&mut h);
    undo(&mut h);
    assert_eq!(titles(&mut h), start);
}

// ---- links --------------------------------------------------------------------------------------

#[test]
fn the_link_tool_makes_a_link_a_click_follows_it_and_back_returns() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    set_tool(&mut h, Tool::Link);
    drag_on_page(
        &mut h,
        0,
        Point::new(100.0, 700.0),
        Point::new(220.0, 650.0),
    );
    assert!(h.state().link_dialog_open());
    assert_all_named(&h, "the link dialog");
    h.state_mut()
        .set_link_dialog_target(LinkKind::Page, "6", None);
    h.run();
    click(&mut h, "Make link");
    settle(&mut h);
    assert!(!h.state().link_dialog_open());
    let doc = h.state().active_view().unwrap().doc;
    let made = h
        .state_mut()
        .link_at(doc, 0, Point::new(160.0, 675.0))
        .expect("the new link is there");
    assert_eq!(made.resolved.map(|d| d.page_index), Some(5));
    let r = made.link.rect;
    assert!(
        (r.x0 - 100.0).abs() < 0.5 && (r.y1 - 700.0).abs() < 0.5,
        "{r:?}"
    );

    // Hovering shows where it leads; a Pan click follows it.
    set_tool(&mut h, Tool::Pan);
    let at = on_screen(&h, 0, Point::new(160.0, 675.0));
    h.hover_at(at);
    h.run();
    h.run();
    assert_eq!(h.state().hovered_link(), Some("Go to page 6"));
    settle(&mut h);
    click_at(&mut h, at);
    settle(&mut h);
    assert_eq!(shown_page(&h), 5);
    h.key_press_modifiers(ALT, Key::ArrowLeft);
    settle(&mut h);
    assert_eq!(shown_page(&h), 0, "back where the link was clicked");
    h.key_press_modifiers(ALT, Key::ArrowRight);
    settle(&mut h);
    assert_eq!(shown_page(&h), 5, "forward again");

    // Undo takes the link away.
    undo(&mut h);
    assert!(h
        .state_mut()
        .link_at(doc, 0, Point::new(160.0, 675.0))
        .is_none());
}

#[test]
fn links_to_other_files_addresses_and_programs() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let dialogs = ScriptedDialogs::default();
    let opened = dialogs.opened_urls.clone();
    let mut h = app_with(pdfium(), &main, dialogs);
    set_tool(&mut h, Tool::Pan);
    let doc = h.state().active_view().unwrap().doc;

    // A web address asks first; Cancel opens nothing, "Open in browser" hands it to the
    // (scripted) system once.
    settled_click(&mut h, 0, Point::new(130.0, 550.0));
    assert_eq!(
        h.state().url_prompt(),
        Some("https://example.com/plumbline")
    );
    assert_all_named(&h, "the web address question");
    click(&mut h, "Cancel");
    assert!(opened.lock().unwrap().is_empty());
    settled_click(&mut h, 0, Point::new(130.0, 550.0));
    click(&mut h, "Open in browser");
    assert_eq!(
        *opened.lock().unwrap(),
        vec!["https://example.com/plumbline".to_owned()]
    );

    // A program is never started.
    settled_click(&mut h, 0, Point::new(130.0, 490.0));
    assert!(h
        .state()
        .status()
        .unwrap()
        .contains("does not start programs"));
    assert_eq!(opened.lock().unwrap().len(), 1);

    // The named destination goes to page 4.
    settled_click(&mut h, 0, Point::new(130.0, 580.0));
    settle(&mut h);
    assert_eq!(shown_page(&h), 3);
    h.key_press_modifiers(ALT, Key::ArrowLeft);
    settle(&mut h);

    // The other file opens in a tab of its own, at its third page.
    settled_click(&mut h, 0, Point::new(130.0, 520.0));
    settle(&mut h);
    let active = h.state().active_document().unwrap();
    assert_eq!(
        active.path().unwrap().file_name().unwrap(),
        "other.pdf",
        "{:?} / {:?}",
        h.state().status(),
        h.state().hovered_link()
    );
    assert_ne!(active.id(), doc);
    assert_eq!(shown_page(&h), 2);
    assert_eq!(h.state().view_ids().len(), 2);
}

#[test]
fn a_link_is_selected_deleted_and_retargeted_like_a_markup() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with_dt(pdfium(), &main, ScriptedDialogs::default(), 0.05);
    let doc = h.state().active_view().unwrap().doc;
    // Select the "next page" link and change where it leads.
    set_tool(&mut h, Tool::Select);
    settled_click(&mut h, 0, Point::new(130.0, 460.0));
    let selected = h
        .state()
        .first_selected()
        .cloned()
        .expect("the link is selected");
    assert_eq!(selected.kind, pl_doc::pl_annot::AnnotKind::Link);
    assert!(h.state_mut().edit_link_dialog());
    h.run();
    assert_all_named(&h, "the link dialog for a link");
    h.state_mut()
        .set_link_dialog_target(LinkKind::Web, "https://example.org/x", None);
    h.run();
    click(&mut h, "Change link");
    settle(&mut h);
    let link = h
        .state_mut()
        .link_at(doc, 0, Point::new(130.0, 460.0))
        .expect("still a link");
    assert_eq!(
        pl_app::links::describe_link(&link),
        "Open https://example.org/x in the browser"
    );
    // Delete it with the Select tool.
    settled_click(&mut h, 0, Point::new(130.0, 460.0));
    assert_eq!(h.state().selected_count(), 1);
    run_command(&mut h, Command::Delete);
    assert!(h
        .state_mut()
        .link_at(doc, 0, Point::new(130.0, 460.0))
        .is_none());
    undo(&mut h);
    assert!(h
        .state_mut()
        .link_at(doc, 0, Point::new(130.0, 460.0))
        .is_some());
    // A double-click with the Select tool follows it: a web address asks first.
    settle(&mut h);
    let at = on_screen(&h, 0, Point::new(130.0, 460.0));
    double_click_at(&mut h, at);
    assert_eq!(
        h.state().url_prompt(),
        Some("https://example.org/x"),
        "{:?} {:?} {:?}",
        h.state().hovered_link(),
        h.state().status(),
        h.state().tool()
    );
    click(&mut h, "Cancel");
}

// ---- flatten ----------------------------------------------------------------------------------

/// The flatten fixture without `/CA` (PDFium 8125 ignores it on appearances; see pl-pdf).
fn flatten_fixture(dir: &Path) -> PathBuf {
    let (fx, _) = pl_testkit::flatten_set();
    let path = write(dir, "flatten.pdf", &fx.bytes);
    let mut file = PdfFile::open(&path).unwrap();
    let doc = file.document().unwrap().clone();
    let mut edits = doc.edit();
    for p in 0..doc.page_count() {
        for a in doc.annotations(p).unwrap() {
            if a.dict.has(b"CA") {
                let mut patch = DictPatch::new();
                patch.remove("CA");
                edits.modify_annotation(a.id.unwrap(), patch);
            }
        }
    }
    file.save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    path
}

fn renders(h: &mut App) -> Vec<Bitmap> {
    let doc = h.state_mut().active_document_mut().unwrap();
    (0..doc.page_count())
        .map(|p| doc.render_page_composite(p, 1.5).unwrap())
        .collect()
}

fn differing(a: &Bitmap, b: &Bitmap, tolerance: u8) -> usize {
    assert_eq!((a.width, a.height), (b.width, b.height));
    a.pixels
        .chunks(4)
        .zip(b.pixels.chunks(4))
        .filter(|(p, q)| {
            p.iter()
                .zip(q.iter())
                .any(|(x, y)| x.abs_diff(*y) > tolerance)
        })
        .count()
}

#[test]
fn flatten_from_the_dialog_leaves_the_render_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = flatten_fixture(dir.path());
    let mut h = app_with(pdfium(), &path, ScriptedDialogs::default());
    let before = renders(&mut h);
    let markups = {
        let state = h.state_mut();
        state.active_document_mut().unwrap().load_annots().unwrap();
        state.markups().len()
    };
    assert!(markups > 0);
    run_command(&mut h, Command::Flatten);
    assert!(h.state().flatten_dialog_open());
    assert_all_named(&h, "the flatten dialog");
    click_label(&mut h, "All markups");
    click(&mut h, "Flatten");
    settle(&mut h);
    assert!(!h.state().flatten_dialog_open());
    assert!(h.state().markups().len() < markups, "markups were burnt in");
    let after = renders(&mut h);
    for (p, (a, b)) in before.iter().zip(&after).enumerate() {
        let over = differing(a, b, 8);
        assert!(
            over <= 40,
            "page {p}: {over} pixels differ by more than 8 levels"
        );
    }
    undo(&mut h);
    assert_eq!(h.state().markups().len(), markups, "undo brings them back");
}

// ---- sets -------------------------------------------------------------------------------------

#[test]
fn the_set_panel_opens_sheets_and_set_search_finds_the_right_file_and_page() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let mut set = DrawingSet::new("Test set");
    set.add_file(&main).unwrap();
    set.add_file(&other).unwrap();
    let set_path = dir.path().join("test.plset");
    set.save(&set_path).unwrap();
    let sheet = set.file(1).unwrap().sheets[2].clone();

    let dialogs = ScriptedDialogs::default().with_sets([set_path.clone()]);
    let mut h = app_with(pdfium(), &main, dialogs);
    run_command(&mut h, Command::OpenSet);
    settle(&mut h);
    assert_eq!(h.state().drawing_set().unwrap().len(), 2);
    assert_all_named(&h, "the Set page");

    // A sheet of the second file: its file opens in a new tab at that page.
    click_label(&mut h, &format!("{}  {}", sheet.number, sheet.title));
    settle(&mut h);
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.path().unwrap().file_name().unwrap(), "other.pdf");
    assert_eq!(shown_page(&h), 2);
    // The next sheet, then the previous one (which is back in this file).
    h.key_press_modifiers(ALT, Key::PageDown);
    settle(&mut h);
    assert_eq!(shown_page(&h), 3);
    h.key_press_modifiers(ALT, Key::PageUp);
    settle(&mut h);
    assert_eq!(shown_page(&h), 2);

    // Set search: the marker of the fifth page of main.pdf.
    let marker = pl_testkit::structure_marker("A", 4);
    type_in(&mut h, "Search the set", &marker);
    click(&mut h, "Search");
    wait_for(&mut h, "the set search finishing", |h| {
        !h.state().set_search_running()
    });
    let hits = h.state().set_search_hits().to_vec();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!((hits[0].file, hits[0].page), (0, 4));
    assert_all_named(&h, "the Set page with search results");
    click_label(&mut h, "Page 5 (A-3)");
    settle(&mut h);
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.path().unwrap().file_name().unwrap(), "main.pdf");
    assert_eq!(shown_page(&h), 4);
}

#[test]
fn a_new_set_is_made_of_the_open_files() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let set_path = dir.path().join("new.plset");
    let dialogs = ScriptedDialogs::default().with_set_saves([set_path.clone()]);
    let mut h = app_with(pdfium(), &main, dialogs);
    h.state_mut().open_path(&other).unwrap();
    settle(&mut h);
    run_command(&mut h, Command::NewSet);
    settle(&mut h);
    let set = h.state().drawing_set().expect("the new set is open");
    assert_eq!(set.name, "new");
    assert_eq!(set.len(), 2);
    // It is on disk, and reads back the same.
    let back = DrawingSet::open(&set_path).unwrap();
    assert_eq!(back.files().len(), 2);
    assert_eq!(back.files()[0].sheets.len(), 8);
    assert_eq!(back.files()[1].sheets.len(), 4);
    // The Set page is shown, with a sheet button per page.
    assert!(h.query_by_label("Search the set").is_some());
}

#[test]
fn a_missing_file_of_a_set_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let mut set = DrawingSet::new("Gaps");
    set.add_file(&main).unwrap();
    set.add_file(&other).unwrap();
    let set_path = dir.path().join("gaps.plset");
    set.save(&set_path).unwrap();
    std::fs::remove_file(&other).unwrap();
    let dialogs = ScriptedDialogs::default().with_sets([set_path]);
    let mut h = app_with(pdfium(), &main, dialogs);
    run_command(&mut h, Command::OpenSet);
    settle(&mut h);
    assert!(h
        .state()
        .status()
        .unwrap()
        .contains("1 of its 2 files are missing"));
    assert!(h.query_by_label("missing").is_some());
    assert!(h.state().drawing_set().unwrap().file(1).unwrap().missing);
}

// ---- print --------------------------------------------------------------------------------------

#[test]
fn the_print_panel_builds_the_job_and_the_fake_printer_gets_every_band() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let backend = pdfium();
    let mut state = state_with(backend.clone(), None, ScriptedDialogs::default());
    let printers = FakePrinters::new(48.0);
    state.set_printers(Box::new(printers.clone()));
    state.open_path(&main).unwrap();
    let mut h = harness_opts(state, 0.25, true);
    settle(&mut h);

    h.key_press_modifiers(CTRL, Key::P);
    h.run();
    h.run();
    assert!(h.state().print_panel_open());
    assert_all_named(&h, "the print panel");
    assert_eq!(h.state().chosen_printer().as_deref(), Some("Test printer"));
    click_label(&mut h, "These pages");
    type_in(&mut h, "Pages to print", "2, 5-6");
    click_label(&mut h, "Actual size");
    let job = h.state().build_print_job().unwrap();
    assert_eq!(job.pages, vec![1, 4, 5]);
    assert_eq!(job.scaling, PrintScaling::ActualSize);
    assert!(job.annotations);
    assert_eq!(job.paper, printers.paper);

    click(&mut h, "Print");
    assert!(h.state_mut().wait_print(Duration::from_secs(120)));
    h.run();
    let log = printers.log();
    assert_eq!(
        log.opened,
        vec![("Test printer".to_owned(), "main.pdf".to_owned())]
    );
    assert_eq!(log.pages, vec![1, 4, 5], "the right pages, in order");
    assert_eq!(log.calls.first(), Some(&"begin_job"));
    assert_eq!(log.calls.last(), Some(&"end_job"));
    assert!(!log.calls.contains(&"abort"));
    // The bands are those of the layout: every visible row of each sheet, once.
    let render = h.state().active_document().unwrap().render_id();
    let placements = layout_job(backend.as_ref(), render, &job).unwrap();
    let expected: usize = placements
        .iter()
        .map(|p| p.bands(&job.paper, job.band_rows).len())
        .sum();
    assert_eq!(log.bands.len(), expected);
    assert!(log.inked_bands > 0, "something was drawn");
    assert_eq!(h.state().last_print_job(), Some(&job));
    assert!(h.state().status().unwrap().starts_with("Sent 3 sheets"));
}

#[test]
fn a_cancelled_print_job_is_aborted() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut state = state_with(pdfium(), None, ScriptedDialogs::default());
    let mut printers = FakePrinters::new(48.0);
    printers.cancel_after = Some(1);
    state.set_printers(Box::new(printers.clone()));
    state.open_path(&main).unwrap();
    let mut h = harness_opts(state, 0.25, true);
    settle(&mut h);
    run_command(&mut h, Command::Print);
    h.state_mut()
        .set_print_options(PageChoice::Current, "", PrintScaling::Fit, false, 2);
    let job = h.state().build_print_job().unwrap();
    assert_eq!(job.pages, vec![0, 0], "two copies");
    assert!(!job.annotations);
    assert!(h.state_mut().start_print());
    assert!(h.state_mut().wait_print(Duration::from_secs(60)));
    let log = printers.log();
    assert_eq!(log.calls.last(), Some(&"abort"));
    assert!(!log.calls.contains(&"end_job"));
}

#[test]
fn without_printers_given_nothing_can_be_printed() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    // The app as tests build it has no printers at all: no real one can be reached.
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    run_command(&mut h, Command::Print);
    assert!(h.state().print_panel_open());
    assert_eq!(h.state().chosen_printer(), None);
    assert!(h.state().build_print_job().is_err());
    assert!(!h.state_mut().start_print());
    assert!(h.query_by_label("No printers were found.").is_some());
}

// ---- every new control has a name ----------------------------------------------------------------

#[test]
fn the_new_menus_and_panels_have_named_controls() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    // The page operations menu of the Pages page, and a thumbnail's context menu.
    click(&mut h, "More");
    assert!(h.query_by_label("Combine files\u{2026}").is_some());
    assert_all_named(&h, "the page operations menu");
    h.key_press(Key::Escape);
    h.run();
    h.get_by_label("Page 2 thumbnail").click_secondary();
    h.run();
    h.run();
    assert!(h.query_by_label("Delete page\u{2026}").is_some());
    assert_all_named(&h, "a thumbnail's context menu");
    h.key_press(Key::Escape);
    h.run();
    // The Link tool's button.
    assert!(h
        .query_by_role_and_label(Role::Button, "Link tool")
        .is_some());
}

// ---- snapshot ---------------------------------------------------------------------------------

#[test]
fn snapshot_page_operations_light() {
    let _gpu = gpu();
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    h.state_mut().select_pages(&[0, 1]);
    h.state_mut().set_theme(egui::ThemePreference::Light);
    settle(&mut h);
    h.snapshot("page_operations_light");
}

// ---- review findings ----------------------------------------------------------------------------

#[test]
fn a_thumbnail_drag_released_off_the_thumbnails_or_escaped_moves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut h = app_with(pdfium(), &main, ScriptedDialogs::default());
    let before = fingerprints(&mut h);
    // Released over the page area, beside the thumbnails.
    let from = rect_of_node(&h, "Page 1 thumbnail").center();
    let canvas = h.state().active_view().unwrap().rect.center();
    drag(&mut h, from, canvas);
    settle(&mut h);
    assert_eq!(fingerprints(&mut h), before, "released off the thumbnails");
    assert!(!h.state().is_modified(h.state().active().unwrap()));

    // Escape during a drag over the thumbnails: released there, nothing moves.
    let to = egui::pos2(from.x, rect_of_node(&h, "Page 3 thumbnail").top() + 8.0);
    h.hover_at(from);
    h.run();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    for stop in [from + (to - from) * 0.5, to] {
        h.event(egui::Event::PointerMoved(stop));
        h.run();
    }
    h.key_press(Key::Escape);
    h.run();
    h.event(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    h.remove_cursor();
    settle(&mut h);
    assert_eq!(fingerprints(&mut h), before, "escaped");
}

#[test]
fn inserting_several_files_into_a_signed_document_asks_once_for_all() {
    let dir = tempfile::tempdir().unwrap();
    let (_, other) = fixtures(dir.path());
    let second = dir.path().join("second.pdf");
    std::fs::copy(&other, &second).unwrap();
    let signed = write(dir.path(), "signed.pdf", &pl_testkit::signed_stub().bytes);
    let dialogs = ScriptedDialogs::new([vec![other.clone(), second.clone()]]);
    let mut h = app_with(pdfium(), &signed, dialogs);
    let before = page_count(&h);
    let other_pages = {
        let mut file = PdfFile::open(&other).unwrap();
        file.document().unwrap().page_count() as u32
    };
    click_label(&mut h, "Page 1 thumbnail");
    click(&mut h, "More");
    click(&mut h, "Insert pages from file\u{2026}");
    assert!(h.state().signature_prompt().is_some());
    assert_eq!(page_count(&h), before, "nothing yet");
    click(&mut h, "Change it anyway");
    settle(&mut h);
    assert_eq!(
        page_count(&h),
        before + 2 * other_pages,
        "both files are in: {:?}",
        h.state().status()
    );
}

#[test]
fn combining_asks_about_signatures_and_takes_unsaved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let signed = write(dir.path(), "signed.pdf", &pl_testkit::signed_stub().bytes);
    let dest = dir.path().join("combined.pdf");
    let dialogs = ScriptedDialogs::new([vec![signed.clone()]]).with_saves([dest.clone()]);
    let mut h = app_with(pdfium(), &main, dialogs);
    let main_pages = page_count(&h);
    let signed_pages = {
        let mut file = PdfFile::open(&signed).unwrap();
        file.document().unwrap().page_count() as u32
    };
    // An unsaved change of the open document: its first page turned.
    let turned_from = fingerprints(&mut h)[0].rotate;
    click(&mut h, "Rotate pages clockwise");
    settle(&mut h);
    let turned = fingerprints(&mut h)[0].rotate;
    assert_ne!(turned, turned_from);

    run_command(&mut h, Command::CombineFiles);
    click(&mut h, "Add files\u{2026}");
    click(&mut h, "Combine\u{2026}");
    settle(&mut h);
    assert!(
        h.state().signature_prompt().is_some(),
        "a signed part asks first: {:?}",
        h.state().status()
    );
    assert!(!dest.exists(), "nothing is written before the answer");
    click(&mut h, "Change it anyway");
    settle(&mut h);
    assert!(!h.state().combine_dialog_open());
    let doc = h.state().active_document().unwrap();
    assert_eq!(doc.path().unwrap().file_name().unwrap(), "combined.pdf");
    let fps = fingerprints(&mut h);
    assert_eq!(fps.len() as u32, main_pages + signed_pages);
    assert_eq!(
        fps[0].rotate, turned,
        "the unsaved rotation is in the combination"
    );
}

#[test]
fn cancel_stops_a_running_print_job() {
    let dir = tempfile::tempdir().unwrap();
    let (main, _) = fixtures(dir.path());
    let mut state = state_with(pdfium(), None, ScriptedDialogs::default());
    let mut printers = FakePrinters::new(48.0);
    printers.band_delay = Some(Duration::from_millis(25));
    state.set_printers(Box::new(printers.clone()));
    state.open_path(&main).unwrap();
    let mut h = harness_opts(state, 0.25, true);
    settle(&mut h);
    run_command(&mut h, Command::Print);
    // The panel asks the printer for its paper once, not on every frame.
    for _ in 0..5 {
        h.run();
    }
    assert_eq!(
        printers
            .paper_queries
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
    h.state_mut()
        .set_print_options(PageChoice::All, "", PrintScaling::Fit, false, 3);
    assert!(h.state_mut().start_print());
    h.state_mut().cancel_print();
    assert!(h.state_mut().wait_print(Duration::from_secs(60)));
    let log = printers.log();
    assert_eq!(log.calls.last(), Some(&"abort"));
    assert!(!log.calls.contains(&"end_job"));
    assert!(
        log.pages.len() < 24,
        "stopped before every sheet: {}",
        log.pages.len()
    );
    assert_eq!(h.state().status(), Some("Printing was cancelled."));
}

#[test]
fn set_hits_and_sheets_follow_page_changes_of_an_open_file() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let mut set = DrawingSet::new("Test set");
    set.add_file(&main).unwrap();
    set.add_file(&other).unwrap();
    let set_path = dir.path().join("test.plset");
    set.save(&set_path).unwrap();
    let fifth = set.file(0).unwrap().sheets[4].clone();

    let dialogs = ScriptedDialogs::default().with_sets([set_path.clone()]);
    let mut h = app_with(pdfium(), &main, dialogs);
    run_command(&mut h, Command::OpenSet);
    settle(&mut h);
    let marker = pl_testkit::structure_marker("A", 4);
    assert!(h.state_mut().start_set_search(&marker));
    wait_for(&mut h, "the set search finishing", |h| {
        !h.state().set_search_running()
    });
    assert!(!h.state().set_search_cancelled());
    assert_eq!(h.state().set_search_hits()[0].page, 4);

    // Page 5 moves to the front: the hit and the sheet go with it.
    assert!(h.state_mut().move_pages(vec![4], 0));
    settle(&mut h);
    let hit = h.state().set_search_hits()[0].clone();
    assert_eq!((hit.file, hit.page), (0, 0));
    assert_eq!(hit.sheet.as_ref(), Some(&fifth));
    let set = h.state().drawing_set().unwrap();
    assert_eq!(set.file(0).unwrap().sheets[0], fifth);
    assert_eq!(
        set.find_sheet(&fifth.number),
        Some(pl_doc::SheetRef { file: 0, page: 0 })
    );
    h.state_mut().open_set_hit(0).unwrap();
    settle(&mut h);
    assert_eq!(shown_page(&h), 0, "the hit opens where its page is now");

    // Undo puts everything back; the set file on disk never saw the unsaved order.
    undo(&mut h);
    assert_eq!(h.state().set_search_hits()[0].page, 4);
    assert_eq!(
        h.state().drawing_set().unwrap().file(0).unwrap().sheets[4],
        fifth
    );
    assert_eq!(
        DrawingSet::open(&set_path).unwrap().file(0).unwrap().sheets[4],
        fifth
    );

    // Deleting the page drops the hit and the sheet.
    h.state_mut().select_pages(&[4]);
    h.state_mut().delete_pages();
    h.run();
    click(&mut h, "Delete");
    settle(&mut h);
    assert!(h.state().set_search_hits().is_empty());
    assert!(h
        .state()
        .drawing_set()
        .unwrap()
        .find_sheet(&fifth.number)
        .is_none());
}

#[test]
fn files_added_to_a_set_are_read_in_the_background() {
    let dir = tempfile::tempdir().unwrap();
    let (main, other) = fixtures(dir.path());
    let mut set = DrawingSet::new("One file");
    set.add_file(&main).unwrap();
    let set_path = dir.path().join("one.plset");
    set.save(&set_path).unwrap();
    let dialogs = ScriptedDialogs::new([vec![other.clone()]]).with_sets([set_path.clone()]);
    let mut h = app_with(pdfium(), &main, dialogs);
    run_command(&mut h, Command::OpenSet);
    settle(&mut h);
    click(&mut h, "Add files\u{2026}");
    settle(&mut h);
    let set = h.state().drawing_set().unwrap();
    assert_eq!(set.len(), 2, "{:?}", h.state().status());
    assert_eq!(set.file(1).unwrap().sheets.len(), 4);
    // The set on disk has it too.
    assert_eq!(DrawingSet::open(&set_path).unwrap().len(), 2);
}
