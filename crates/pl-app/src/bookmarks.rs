//! The Bookmarks page of the side panel, editable: a click goes to the bookmark, a
//! double-click (or Rename) renames it in place, dragging a bookmark moves it (onto the upper
//! or lower edge of another to put it before or after that one, onto its middle to put it
//! inside), and the bar above the list adds a bookmark for the place in view, renames,
//! deletes, and points the chosen bookmark at the place in view. Every edit is one undo step
//! (see [`crate::structure`]).
//!
//! The bookmarks are read from the document's structure (pl-pdf's outline) once per display
//! of it. A document the structure layer cannot read shows PDFium's reading of its
//! bookmarks, which can be followed but not edited.

use egui::{Button, Id, Key, Sense, Stroke, TextEdit, Ui};
use pl_doc::pl_pdf::FlatOutlineItem;
use pl_doc::pl_render::DocId;
use pl_doc::{BookmarkSpec, BookmarkTarget, DestSpec, DocumentId, StructCommand};

use crate::app::AppState;
use crate::icons::{icon_button_tip, Icon};
use crate::theme::{self, Palette};

const TITLE_FIELD: &str = "plumbline-bookmark-title";

/// The bookmarks of a document, read for one display of it.
pub(crate) struct OutlineCache {
    pub render: DocId,
    pub rows: Result<Vec<FlatOutlineItem>, String>,
}

/// A bookmark being renamed in place.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BookmarkEdit {
    pub doc: DocumentId,
    pub path: Vec<usize>,
    pub text: String,
    pub focus: bool,
}

/// Where a dragged bookmark would go.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct DropAt {
    /// The parent's path and the index among its children, before the dragged item is taken
    /// out.
    pub parent: Vec<usize>,
    pub index: usize,
    /// Where the indicator is drawn: the row's rect and whether it goes inside it.
    pub row: egui::Rect,
    pub inside: bool,
    pub below: bool,
}

/// A bookmark being dragged.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BookmarkDrag {
    pub doc: DocumentId,
    pub from: Vec<usize>,
    pub drop: Option<DropAt>,
}

/// The parent and index of a move once the moved item has been taken out: the coordinates
/// [`StructCommand::MoveBookmark`] wants. `None` when the move would change nothing, or would
/// put an item inside itself.
pub(crate) fn move_destination(
    from: &[usize],
    parent: &[usize],
    index: usize,
) -> Option<(Vec<usize>, usize)> {
    if from.is_empty() || (parent.len() >= from.len() && parent[..from.len()] == *from) {
        return None;
    }
    let (last, from_parent) = from.split_last()?;
    let depth = from_parent.len();
    let mut parent = parent.to_vec();
    let mut index = index;
    let same_parent = parent == from_parent;
    // A path through a later sibling of the moved item shifts up by one.
    if parent.len() > depth && parent[..depth] == *from_parent && parent[depth] > *last {
        parent[depth] -= 1;
    }
    if same_parent {
        if index > *last {
            index -= 1;
        }
        if index == *last {
            return None;
        }
    }
    Some((parent, index))
}

impl AppState {
    /// The active document's bookmarks as the structure has them (pre-order, with their
    /// paths); `None` while there is no document or they cannot be read.
    pub fn bookmark_rows(&mut self) -> Option<Vec<FlatOutlineItem>> {
        let doc = self.active_doc_id()?;
        self.outline_rows_of(doc).ok()
    }

    pub(crate) fn outline_rows_of(
        &mut self,
        doc_id: DocumentId,
    ) -> Result<Vec<FlatOutlineItem>, String> {
        let doc = self
            .session
            .document_mut(doc_id)
            .map_err(|e| e.to_string())?;
        if doc.is_saving() {
            return Err("the document is being saved".into());
        }
        let render = doc.render_id();
        if let Some(cache) = self.structure.outlines.get(&doc_id) {
            if cache.render == render {
                return cache.rows.clone();
            }
        }
        let rows = doc.outline_rows().map_err(|e| e.to_string());
        self.structure.outlines.insert(
            doc_id,
            OutlineCache {
                render,
                rows: rows.clone(),
            },
        );
        rows
    }

    /// The bookmark chosen in the Bookmarks page of the active document.
    pub fn selected_bookmark(&self) -> Option<Vec<usize>> {
        let doc = self.active_doc_id()?;
        self.structure
            .bookmark_selected
            .as_ref()
            .filter(|(d, _)| *d == doc)
            .map(|(_, p)| p.clone())
    }

    pub fn select_bookmark(&mut self, path: Vec<usize>) {
        if let Some(doc) = self.active_doc_id() {
            self.structure.bookmark_selected = Some((doc, path));
        }
    }

    /// The place in view of the active view, as a bookmark target: the point at the top left of
    /// the view on the page in view.
    fn view_target(&self) -> Option<DestSpec> {
        let dv = self.active_view()?;
        let page = dv.shown_page();
        let doc = self.session.document(dv.doc).ok()?;
        if doc.geometry_is_exact(page) {
            if let Some(hit) = dv
                .view
                .hit_test_nearest(pl_doc::pl_geom::Point::ZERO)
                .filter(|h| h.page == page)
            {
                return Some(DestSpec::at(page, hit.point.x, hit.point.y));
            }
        }
        Some(DestSpec::page(page))
    }

    /// Adds a bookmark to the place in view, after the chosen bookmark (or at the end), and
    /// starts renaming it. Returns whether it was added.
    pub fn add_bookmark(&mut self) -> bool {
        let Some(doc) = self.active_doc_id() else {
            return false;
        };
        let Some(target) = self.view_target() else {
            return false;
        };
        let top_level = self
            .outline_rows_of(doc)
            .map(|rows| rows.iter().filter(|r| r.level == 0).count())
            .unwrap_or(0);
        let (parent, index) = match self.selected_bookmark() {
            Some(path) if !path.is_empty() => {
                let (last, parent) = path.split_last().expect("not empty");
                (parent.to_vec(), last + 1)
            }
            _ => (Vec::new(), top_level),
        };
        let label = self
            .session
            .document_mut(doc)
            .ok()
            .and_then(|d| d.page_labels().ok())
            .and_then(|labels| labels.get(target.page as usize).cloned())
            .unwrap_or_else(|| (target.page + 1).to_string());
        let title = format!("Page {label}");
        let mut spec = BookmarkSpec::to_page(title.clone(), target.page);
        spec.target = BookmarkTarget::Page(target);
        let mut path = parent.clone();
        path.push(index);
        if self.apply_structure(StructCommand::AddBookmark {
            parent,
            index,
            bookmark: spec,
        }) {
            self.structure.bookmark_selected = Some((doc, path.clone()));
            self.structure.bookmark_edit = Some(BookmarkEdit {
                doc,
                path,
                text: title,
                focus: true,
            });
            true
        } else {
            false
        }
    }

    pub fn rename_bookmark(&mut self, path: Vec<usize>, title: &str) -> bool {
        self.apply_structure(StructCommand::RenameBookmark {
            path,
            title: title.to_owned(),
        })
    }

    pub fn delete_bookmark(&mut self, path: Vec<usize>) -> bool {
        let done = self.apply_structure(StructCommand::DeleteBookmark { path });
        if done {
            self.structure.bookmark_selected = None;
        }
        done
    }

    /// Moves a bookmark to child `index` of `parent` (paths as they are before the move).
    pub fn move_bookmark(&mut self, from: Vec<usize>, parent: Vec<usize>, index: usize) -> bool {
        let Some((parent, index)) = move_destination(&from, &parent, index) else {
            return false;
        };
        let Some(doc) = self.active_doc_id() else {
            return false;
        };
        let mut moved = parent.clone();
        moved.push(index);
        let done = self.apply_structure(StructCommand::MoveBookmark {
            from,
            parent,
            index,
        });
        if done {
            self.structure.bookmark_selected = Some((doc, moved));
        }
        done
    }

    /// Points a bookmark at the place in view.
    pub fn set_bookmark_to_view(&mut self, path: Vec<usize>) -> bool {
        let Some(target) = self.view_target() else {
            return false;
        };
        self.apply_structure(StructCommand::SetBookmarkTarget {
            path,
            target: BookmarkTarget::Page(target),
        })
    }

    /// Goes to a bookmark's destination, remembering where the view was.
    pub fn go_to_bookmark(&mut self, path: &[usize]) -> bool {
        let Some(id) = self.active else {
            return false;
        };
        let Some(rows) = self.bookmark_rows() else {
            return false;
        };
        let Some(dest) = rows.iter().find(|r| r.path == path).and_then(|r| r.dest) else {
            return false;
        };
        let page = dest.page_index as u32;
        let (left, top) = dest.view.target_point();
        self.push_history(id);
        let Some(dv) = self.views.get_mut(&id) else {
            return false;
        };
        let exact = self
            .session
            .document(dv.doc)
            .is_ok_and(|d| d.geometry_is_exact(page));
        dv.go_to_destination(page, left, top, exact);
        true
    }
}

/// The Bookmarks page.
pub fn panel(app: &mut AppState, ui: &mut Ui) {
    let Some(doc_id) = app.active_doc_id() else {
        return;
    };
    let rows = match app.outline_rows_of(doc_id) {
        Ok(rows) => rows,
        Err(_) => {
            read_only(app, ui);
            return;
        }
    };
    let selected = app.selected_bookmark();
    let saving = app.doc_is_saving(doc_id);

    // ---- the bar --------------------------------------------------------------------------------
    let mut add = false;
    let mut rename = false;
    let mut set_here = false;
    let mut delete = false;
    ui.add_enabled_ui(!saving, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            add = icon_button_tip(
                ui,
                Icon::Plus,
                "Add bookmark",
                "Add a bookmark to the place in view",
                false,
            )
            .clicked();
            ui.add_enabled_ui(selected.is_some(), |ui| {
                rename = ui
                    .add(Button::new("Rename").small())
                    .on_hover_text("Rename the chosen bookmark (or double-click it)")
                    .clicked();
                set_here = ui
                    .add(Button::new("Set to view").small())
                    .on_hover_text("Make the chosen bookmark lead to the place in view")
                    .clicked();
                delete = icon_button_tip(
                    ui,
                    Icon::Cross,
                    "Delete bookmark",
                    "Delete the chosen bookmark and the ones inside it",
                    false,
                )
                .clicked();
            });
        });
    });
    ui.separator();

    if rows.is_empty() {
        ui.add_space(6.0);
        ui.weak("This document has no bookmarks. Add one for the place in view with +.");
    }

    // ---- the list -------------------------------------------------------------------------------
    let palette = Palette::of(ui.visuals());
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let released = ui.input(|i| i.pointer.any_released());
    let enter = ui.input(|i| i.key_pressed(Key::Enter));
    let escape = ui.input(|i| i.key_pressed(Key::Escape));
    let mut go: Option<Vec<usize>> = None;
    let mut start_edit: Option<Vec<usize>> = None;
    let mut commit: Option<(Vec<usize>, String)> = None;
    let mut cancel_edit = false;
    let mut drag_start: Option<Vec<usize>> = None;
    let mut row_rects: Vec<(Vec<usize>, egui::Rect)> = Vec::new();
    let editing = app
        .structure
        .bookmark_edit
        .as_ref()
        .filter(|e| e.doc == doc_id)
        .map(|e| e.path.clone());
    let scroll = egui::ScrollArea::vertical()
        .id_salt("plumbline-bookmarks")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in &rows {
                ui.horizontal(|ui| {
                    ui.add_space(4.0 + 14.0 * row.level as f32);
                    if editing.as_ref() == Some(&row.path) {
                        let Some(edit) = app.structure.bookmark_edit.as_mut() else {
                            return;
                        };
                        let field = ui
                            .add(
                                TextEdit::singleline(&mut edit.text)
                                    .id(Id::unique(TITLE_FIELD))
                                    .desired_width(f32::INFINITY),
                            )
                            .accessible_name("Bookmark title")
                            .on_hover_text("Enter keeps the title, Esc cancels");
                        if std::mem::take(&mut edit.focus) {
                            field.request_focus();
                        }
                        if field.lost_focus() {
                            if escape {
                                cancel_edit = true;
                            } else {
                                commit = Some((row.path.clone(), edit.text.clone()));
                            }
                        } else if field.has_focus() && enter {
                            commit = Some((row.path.clone(), edit.text.clone()));
                        }
                        row_rects.push((row.path.clone(), field.rect));
                        return;
                    }
                    let is_selected = selected.as_ref() == Some(&row.path);
                    let tip = match &row.dest {
                        Some(d) => format!("Go to page {}", d.page_index + 1),
                        None => "This bookmark leads nowhere".to_owned(),
                    };
                    let response = ui
                        .add(Button::selectable(is_selected, row.title.as_str()).truncate())
                        .on_hover_text(format!("{tip}. Double-click to rename; drag to move."));
                    let response = response.interact(Sense::click_and_drag());
                    row_rects.push((row.path.clone(), response.rect));
                    if response.double_clicked() {
                        start_edit = Some(row.path.clone());
                    } else if response.clicked() {
                        go = Some(row.path.clone());
                    }
                    if response.drag_started_by(egui::PointerButton::Primary) && !saving {
                        drag_start = Some(row.path.clone());
                    }
                });
            }
            // The drop indicator.
            if let Some(drop) = app
                .structure
                .bookmark_drag
                .as_ref()
                .filter(|d| d.doc == doc_id)
                .and_then(|d| d.drop.clone())
            {
                let stroke = Stroke::new(2.0, palette.accent);
                if drop.inside {
                    ui.painter().rect_stroke(
                        drop.row.expand(1.0),
                        2.0,
                        stroke,
                        egui::StrokeKind::Outside,
                    );
                } else {
                    let y = if drop.below {
                        drop.row.bottom() + 1.0
                    } else {
                        drop.row.top() - 1.0
                    };
                    ui.painter().line_segment(
                        [
                            egui::pos2(drop.row.left(), y),
                            egui::pos2(drop.row.right(), y),
                        ],
                        stroke,
                    );
                }
            }
        });
    theme::name_scroll_bars(ui.ctx(), scroll.id, "Bookmarks");

    // ---- what was asked ----------------------------------------------------------------------
    if let Some(path) = go {
        app.structure.bookmark_selected = Some((doc_id, path.clone()));
        app.go_to_bookmark(&path);
    }
    if let Some(path) = start_edit.or_else(|| rename.then(|| selected.clone()).flatten()) {
        let text = rows
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.title.clone())
            .unwrap_or_default();
        app.structure.bookmark_selected = Some((doc_id, path.clone()));
        app.structure.bookmark_edit = Some(BookmarkEdit {
            doc: doc_id,
            path,
            text,
            focus: true,
        });
    }
    if cancel_edit {
        app.structure.bookmark_edit = None;
    }
    if let Some((path, title)) = commit {
        app.structure.bookmark_edit = None;
        let old = rows
            .iter()
            .find(|r| r.path == path)
            .map(|r| r.title.as_str());
        let title = title.trim();
        if !title.is_empty() && Some(title) != old {
            app.rename_bookmark(path, title);
        }
    }
    if add {
        app.add_bookmark();
    }
    if let (true, Some(path)) = (set_here, selected.clone()) {
        app.set_bookmark_to_view(path);
    }
    if let (true, Some(path)) = (delete, selected) {
        app.delete_bookmark(path);
    }

    // ---- dragging ------------------------------------------------------------------------------
    if let Some(from) = drag_start {
        app.structure.bookmark_drag = Some(BookmarkDrag {
            doc: doc_id,
            from,
            drop: None,
        });
    }
    if let (Some(drag), Some(pos)) = (
        app.structure
            .bookmark_drag
            .as_mut()
            .filter(|d| d.doc == doc_id),
        pointer,
    ) {
        drag.drop = drop_at(&row_rects, &drag.from, pos);
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    if released {
        if let Some(drag) = app.structure.bookmark_drag.take() {
            if let Some(drop) = drag.drop.filter(|_| drag.doc == doc_id) {
                app.move_bookmark(drag.from, drop.parent, drop.index);
            }
        }
    }
}

/// Where a bookmark dragged to `pos` would go: before, after or inside the row under it.
fn drop_at(rows: &[(Vec<usize>, egui::Rect)], from: &[usize], pos: egui::Pos2) -> Option<DropAt> {
    let (path, rect) = rows
        .iter()
        .find(|(_, r)| pos.y >= r.top() - 1.0 && pos.y < r.bottom() + 1.0)?;
    if path.len() >= from.len() && path[..from.len()] == *from {
        // Onto itself or one of its own.
        return None;
    }
    let (last, parent) = path.split_last()?;
    let quarter = rect.height() / 4.0;
    let (parent, index, inside, below) = if pos.y < rect.top() + quarter {
        (parent.to_vec(), *last, false, false)
    } else if pos.y > rect.bottom() - quarter {
        (parent.to_vec(), last + 1, false, true)
    } else {
        (path.clone(), 0, true, false)
    };
    move_destination(from, &parent, index)?;
    Some(DropAt {
        parent,
        index,
        row: *rect,
        inside,
        below,
    })
}

/// PDFium's reading of the bookmarks, for a document the structure layer cannot read: they
/// can be followed, not edited.
fn read_only(app: &mut AppState, ui: &mut Ui) {
    let Some(id) = app.active else {
        return;
    };
    let Some(doc_id) = app.views.get(&id).map(|v| v.doc) else {
        return;
    };
    let Ok(doc) = app.session.document(doc_id) else {
        return;
    };
    let render = doc.render_id();
    // The outline is read by the scheduler; the panel shows it when it has arrived.
    let Some(tree) = doc.cached_bookmarks() else {
        ui.add_space(6.0);
        match app.queries.outline_error(render) {
            Some(e) => {
                ui.weak(format!("Bookmarks could not be read: {e}"));
            }
            None => {
                // Not while the document is being saved: its display is the save's.
                if !doc.is_saving() {
                    app.queries.want_outline(&app.scheduler, render);
                }
                ui.weak("Reading bookmarks...");
            }
        }
        return;
    };
    if tree.is_empty() {
        ui.add_space(6.0);
        ui.weak("This document has no bookmarks.");
        return;
    }
    let mut target = None;
    let scroll = egui::ScrollArea::vertical()
        .id_salt("plumbline-bookmarks")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for row in tree.flatten() {
                ui.horizontal(|ui| {
                    ui.add_space(4.0 + 14.0 * row.depth as f32);
                    let enabled = row.node.page.is_some();
                    let button = Button::selectable(false, row.node.title.as_str()).truncate();
                    if ui.add_enabled(enabled, button).clicked() {
                        target = row.node.page.map(|p| (p, row.node.x, row.node.y));
                    }
                });
            }
        });
    theme::name_scroll_bars(ui.ctx(), scroll.id, "Bookmarks");
    if let Some((page, x, y)) = target {
        app.push_history(id);
        if let (Ok(doc), Some(dv)) = (app.session.document(doc_id), app.views.get_mut(&id)) {
            let exact = doc.geometry_is_exact(page);
            dv.go_to_destination(page, x, y, exact);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_are_given_after_the_item_is_taken_out() {
        // [a, b, c]: a moved after c (index 3 before removal) is index 2 after.
        assert_eq!(move_destination(&[0], &[], 3), Some((vec![], 2)));
        // c moved before a.
        assert_eq!(move_destination(&[2], &[], 0), Some((vec![], 0)));
        // b before itself or after itself changes nothing.
        assert_eq!(move_destination(&[1], &[], 1), None);
        assert_eq!(move_destination(&[1], &[], 2), None);
        // a moved inside c: c's path shifts up.
        assert_eq!(move_destination(&[0], &[2], 0), Some((vec![1], 0)));
        // a child moved to the top level.
        assert_eq!(move_destination(&[1, 0], &[], 0), Some((vec![], 0)));
        // Never inside itself.
        assert_eq!(move_destination(&[1], &[1, 0], 0), None);
        assert_eq!(move_destination(&[1], &[1], 0), None);
    }

    #[test]
    fn a_bookmark_dropped_onto_the_next_one_nests_under_it() {
        use pl_doc::pl_pdf::{Outline, OutlineItem, OutlineTarget};
        let item = |t: &str| OutlineItem::new(t, OutlineTarget::None);
        let mut outline = Outline {
            items: vec![item("A"), item("B")],
        };
        // A dragged onto B: B is [0] once A is taken out.
        let (parent, index) = move_destination(&[0], &[1], 0).unwrap();
        assert_eq!((parent.as_slice(), index), ([0].as_slice(), 0));
        outline.move_item(&[0], &parent, index).unwrap();
        assert_eq!(outline.items.len(), 1);
        assert_eq!(outline.items[0].title, "B");
        assert_eq!(outline.items[0].children[0].title, "A");
    }
}
