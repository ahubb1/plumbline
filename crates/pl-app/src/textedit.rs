//! Typing into markups: the in-place editor of text boxes, callouts and typewriter text, and
//! the note window.
//!
//! The editor is an ordinary upright egui text box in screen space, placed beside the markup
//! (above it when there is room, so a box that grows down the page does not push it about).
//! What is typed is laid out again by pl-draw on every change and painted on the page where
//! the markup is, so the page shows what will be saved: for a markup that does not exist yet
//! the tool is asked what it would create from the text so far ([`pl_tools::step_text`] on a
//! copy of its state); for an existing one the change goes through pl-doc at once, as one
//! undo step for the whole editing session.
//!
//! Text on the page can only use the characters of WinAnsi until embedded fonts arrive. Other
//! characters are refused as they are typed, with a message in the editor that names them.
//! Esc cancels; Ctrl+Enter, the Done button or a click elsewhere commits.
//!
//! A note keeps its text out of the page, so it has no such limit: a double-click on one
//! opens a small window with its text, its review status and its replies.

use egui::{Id, Key, Modifiers, RichText, TextEdit};
use pl_doc::pl_annot::{AnnotId, AnnotKind, Annotation, DrawError, Origin, PdfDate, ReviewState};
use pl_doc::pl_draw::DrawList;
use pl_doc::pl_geom::Rect;
use pl_doc::{Command as DocCommand, DocumentId};
use pl_tools::{Intent, TextInput, ToolState};

use crate::app::{AppState, ViewId};
use crate::markup::{Tool, ToolView};
use crate::theme::{self, Palette};

/// The accessible name of the in-place editor's text box.
pub const EDITOR_NAME: &str = "Markup text";
const EDITOR_ID: &str = "plumbline-text-editor";
const NOTE_TEXT_ID: &str = "plumbline-note-text";
const NOTE_REPLY_ID: &str = "plumbline-note-reply";

/// Whether `id` is one of the note window's text boxes.
pub(crate) fn is_note_field(id: Id) -> bool {
    id == Id::unique(NOTE_TEXT_ID) || id == Id::unique(NOTE_REPLY_ID)
}
/// How many spaces a tab becomes (a tab cannot be written to the page).
const TAB_SPACES: &str = "    ";

/// What the editor is typing into.
#[derive(Clone, Debug, PartialEq)]
enum TextTarget {
    /// A markup the active tool is about to create.
    Draft,
    /// A text box that exists.
    Existing(AnnotId),
}

/// The in-place editor's state.
pub(crate) struct TextEditor {
    pub view: ViewId,
    pub doc: DocumentId,
    target: TextTarget,
    pub text: String,
    /// Why something that was typed was not taken.
    pub message: Option<String>,
    focus: bool,
    /// The keyboard focus was on the editor (its text or one of its buttons) in the last
    /// frame.
    had_focus: bool,
    /// A change has gone into the document (an existing markup): cancelling undoes it.
    applied: bool,
    /// The undo key of this editing session.
    key: u64,
    /// The page and the box of the markup as it would be saved, to place the editor by.
    anchor: Option<(u32, Rect)>,
    /// Where the editor was on screen in the last frame.
    area: egui::Rect,
}

impl TextEditor {
    pub(crate) fn is_draft(&self) -> bool {
        self.target == TextTarget::Draft
    }
}

/// The note window's state.
pub(crate) struct NotePopup {
    pub doc: DocumentId,
    pub id: AnnotId,
    text: String,
    reply: String,
    focus: bool,
    key: u64,
}

/// Takes out of `text` what `probe` (the markup the text is for) cannot show. Returns the
/// text that is left and the characters that were refused.
fn sanitize(text: &str, probe: &Annotation) -> (String, Vec<char>) {
    // One pass with the encoder itself takes out every character the font cannot show,
    // however many different ones there are (a pasted paragraph of another script has
    // hundreds).
    let mut clean = String::with_capacity(text.len());
    let mut refused: Vec<char> = Vec::new();
    let mut known: std::collections::HashSet<char> = std::collections::HashSet::new();
    for c in text.chars() {
        match c {
            '\t' => clean.push_str(TAB_SPACES),
            '\n' | '\r' => clean.push(c),
            c if pl_doc::pl_draw::winansi_byte(c).is_some() => clean.push(c),
            c => {
                if known.insert(c) {
                    refused.push(c);
                }
            }
        }
    }
    let mut probe = probe.clone();
    // The markup's own check has the last word. Each round removes one character everywhere,
    // so this ends; after the pass above it has nothing left to do.
    for _ in 0..256 {
        probe.meta.contents = Some(clean.clone());
        match probe.validate() {
            Err(DrawError::NotWinAnsi { ch, .. }) => {
                clean.retain(|c| c != ch);
                refused.push(ch);
            }
            _ => break,
        }
    }
    (clean, refused)
}

/// What [`sanitize`] leaves of the first `chars` characters of `text`, in characters: where a
/// cursor that stood after them stands in the cleaned text.
fn cursor_after(text: &str, chars: usize, refused: &[char]) -> usize {
    text.chars()
        .take(chars)
        .map(|c| match c {
            '\t' => TAB_SPACES.len(),
            c if refused.contains(&c) => 0,
            _ => 1,
        })
        .sum()
}

fn refusal(refused: &[char]) -> Option<String> {
    if refused.is_empty() {
        return None;
    }
    let shown: Vec<String> = refused
        .iter()
        .take(6)
        .map(|c| {
            // Named by the character where the interface's own font is likely to have it,
            // and by its code point otherwise.
            let code = *c as u32;
            let drawable = !c.is_control() && (code < 0x0530 || (0x2000..0x2200).contains(&code));
            if drawable {
                format!("\u{201c}{c}\u{201d}")
            } else {
                format!("U+{code:04X}")
            }
        })
        .collect();
    Some(format!(
        "{} cannot be used here: text on the page takes Western European (WinAnsi) characters \
         only.",
        shown.join(", ")
    ))
}

impl AppState {
    // ---- reading -----------------------------------------------------------------------------

    /// Whether the in-place text editor is open.
    pub fn text_edit_active(&self) -> bool {
        self.markup.text_editor.is_some()
    }

    /// What has been typed into the in-place editor so far.
    pub fn text_edit_text(&self) -> Option<&str> {
        self.markup.text_editor.as_ref().map(|e| e.text.as_str())
    }

    /// The in-place editor's message, when something typed was refused.
    pub fn text_edit_message(&self) -> Option<&str> {
        self.markup
            .text_editor
            .as_ref()
            .and_then(|e| e.message.as_deref())
    }

    /// The markup whose note window is open.
    pub fn note_open(&self) -> Option<&AnnotId> {
        self.markup.note.as_ref().map(|n| &n.id)
    }

    // ---- opening -----------------------------------------------------------------------------

    /// A tool asked for the text of a markup: the in-place editor for a text box (one that is
    /// about to be created, or an existing one), the note window for a note opened with the
    /// Select tool, and the Comment box of the properties panel for everything else.
    pub(crate) fn request_text_edit(&mut self, view: ViewId, doc_id: DocumentId, target: AnnotId) {
        let existing = self
            .session
            .document(doc_id)
            .ok()
            .and_then(|d| d.annotation(&target))
            .cloned();
        let Some(annotation) = existing else {
            // A markup the tool is waiting to create.
            let waiting = self
                .views
                .get(&view)
                .is_some_and(|dv| dv.tool.text_draft().is_some());
            if !waiting {
                return;
            }
            if let Some(editor) = self
                .markup
                .text_editor
                .as_mut()
                .filter(|e| e.view == view && e.is_draft())
            {
                // The tool asked again: it would not take what was committed.
                editor.focus = true;
                return;
            }
            self.markup.fields.session += 1;
            self.markup.text_editor = Some(TextEditor {
                view,
                doc: doc_id,
                target: TextTarget::Draft,
                text: String::new(),
                message: None,
                focus: true,
                had_focus: false,
                applied: false,
                key: self.markup.fields.session.wrapping_mul(16).wrapping_add(9),
                anchor: None,
                area: egui::Rect::NOTHING,
            });
            self.refresh_text_preview();
            return;
        };
        if let Ok(doc) = self.session.document_mut(doc_id) {
            let _ = doc.select([target.clone()]);
        }
        let locked = annotation.meta.flags.is_locked();
        let mut probe = annotation.clone();
        if annotation.kind == AnnotKind::FreeText && probe.make_editable() {
            if locked {
                self.chrome.status = Some("This text box is locked.".to_owned());
                return;
            }
            self.commit_text_edit();
            self.markup.fields.session += 1;
            self.markup.text_editor = Some(TextEditor {
                view,
                doc: doc_id,
                target: TextTarget::Existing(target),
                text: annotation.meta.contents.clone().unwrap_or_default(),
                message: None,
                focus: true,
                had_focus: false,
                applied: false,
                key: self.markup.fields.session.wrapping_mul(16).wrapping_add(9),
                anchor: None,
                area: egui::Rect::NOTHING,
            });
        } else if annotation.kind == AnnotKind::Text && self.markup.tool == Tool::Select {
            self.open_note_in(doc_id, &target);
        } else {
            self.markup.properties_open = true;
            self.markup.fields.focus_comment = true;
        }
    }

    /// Opens the note window for a markup of the active document. Returns whether it opened.
    pub fn open_note(&mut self, id: &AnnotId) -> bool {
        match self.active_view().map(|v| v.doc) {
            Some(doc) => self.open_note_in(doc, id),
            None => false,
        }
    }

    fn open_note_in(&mut self, doc_id: DocumentId, id: &AnnotId) -> bool {
        let Some(text) = self
            .session
            .document(doc_id)
            .ok()
            .and_then(|d| d.annotation(id))
            .map(|a| a.meta.contents.clone().unwrap_or_default())
        else {
            return false;
        };
        self.markup.fields.session += 1;
        self.markup.note = Some(NotePopup {
            doc: doc_id,
            id: id.clone(),
            text,
            reply: String::new(),
            focus: true,
            key: self.markup.fields.session.wrapping_mul(16).wrapping_add(10),
        });
        true
    }

    pub fn close_note(&mut self) {
        self.markup.note = None;
    }

    // ---- the in-place editor -----------------------------------------------------------------

    /// What the tool would create from the text typed so far, as the view's preview: the
    /// markup exactly as a commit would make it, laid out by pl-draw.
    fn refresh_text_preview(&mut self) {
        let Some(editor) = self.markup.text_editor.as_mut().filter(|e| e.is_draft()) else {
            return;
        };
        let Some(dv) = self.views.get_mut(&editor.view) else {
            return;
        };
        let Ok(doc) = self.session.document(editor.doc) else {
            return;
        };
        let Some(draft) = dv.tool.text_draft().cloned() else {
            return;
        };
        let page = draft.page as u32;
        let view = ToolView::new(doc, &dv.view, &self.markup.author, page);
        let (_, intents, _) = pl_tools::step_text(
            dv.tool.clone(),
            &TextInput::Commit(editor.text.clone()),
            &view,
        );
        let mut list = DrawList::new();
        let mut anchor = None;
        for intent in intents {
            if let Intent::Create(made) = intent {
                if made.kind == AnnotKind::FreeText {
                    anchor = Some(made.bounds());
                }
                list.ops.extend(made.draw_list().ops);
            }
        }
        if anchor.is_none() {
            // Nothing would be created yet (no text): the tool's own picture of the box.
            list = dv.tool.preview(&view);
            anchor = Some(draft.bounds());
        }
        editor.anchor = anchor.map(|rect| (page, rect));
        dv.preview = list;
        dv.preview_serial += 1;
        dv.preview_page = Some(page);
    }

    /// The editor's text changed: what cannot be written is taken out again (and named), and
    /// the page follows.
    pub(crate) fn text_edit_changed(&mut self) {
        self.text_edit_changed_at(None);
    }

    /// As [`AppState::text_edit_changed`]. `cursor` is where the text cursor stands, in
    /// characters; the result is where it stands in the text as it is afterwards.
    pub(crate) fn text_edit_changed_at(&mut self, cursor: Option<usize>) -> Option<usize> {
        let mut editor = self.markup.text_editor.take()?;
        let probe = match &editor.target {
            TextTarget::Draft => self
                .views
                .get(&editor.view)
                .and_then(|dv| dv.tool.text_draft().cloned()),
            TextTarget::Existing(id) => self
                .session
                .document(editor.doc)
                .ok()
                .and_then(|d| d.annotation(id))
                .cloned()
                .map(|mut a| {
                    a.make_editable();
                    a
                }),
        };
        // (When there is none, what was being typed into is gone: undone, deleted, the tool
        // changed. The editor goes with it.)
        let probe = probe?;
        let (clean, refused) = sanitize(&editor.text, &probe);
        let cursor = cursor.map(|at| cursor_after(&editor.text, at, &refused));
        editor.text = clean;
        editor.message = refusal(&refused);
        if let TextTarget::Existing(id) = &editor.target {
            let current = self
                .session
                .document(editor.doc)
                .ok()
                .and_then(|d| d.annotation(id))
                .cloned();
            let mut changed = probe;
            changed.meta.contents = (!editor.text.is_empty()).then(|| editor.text.clone());
            if current
                .as_ref()
                .is_some_and(|c| c.meta.contents != changed.meta.contents)
            {
                changed.touch(PdfDate::now());
                let command = DocCommand::ModifyAnnotations(vec![changed]);
                editor.applied |= self.exec(editor.doc, command, Some(editor.key));
            }
        }
        self.markup.text_editor = Some(editor);
        self.refresh_text_preview();
        cursor
    }

    /// Replaces the text of the in-place editor, as typing does (for tests and scripts).
    pub fn set_text_edit_text(&mut self, text: &str) {
        if let Some(editor) = self.markup.text_editor.as_mut() {
            editor.text = text.to_owned();
            self.text_edit_changed();
        }
    }

    /// Finishes the in-place editor, keeping what was typed. Returns whether an editor was
    /// open and is closed now.
    pub fn commit_text_edit(&mut self) -> bool {
        let Some(mut editor) = self.markup.text_editor.take() else {
            return false;
        };
        if let TextTarget::Existing(_) = editor.target {
            // Every change is in the document already; the next edit is another undo step.
            self.markup.fields.session += 1;
            return true;
        }
        let view_id = editor.view;
        let Some(dv) = self.views.get_mut(&view_id) else {
            return true;
        };
        let Ok(doc) = self.session.document(editor.doc) else {
            return true;
        };
        let kind = dv.tool.tool();
        let page = dv.tool.page().map_or(dv.shown_page(), |p| p as u32);
        let state = std::mem::replace(&mut dv.tool, ToolState::for_tool(kind));
        let view = ToolView::new(doc, &dv.view, &self.markup.author, page);
        let (state, intents, preview) =
            pl_tools::step_text(state, &TextInput::Commit(editor.text.clone()), &view);
        dv.tool = state;
        dv.preview = preview;
        dv.preview_serial += 1;
        if dv.preview.is_empty() {
            dv.preview_page = None;
        }
        let refused = intents
            .iter()
            .any(|i| matches!(i, Intent::RequestTextEdit(_)));
        if refused {
            // The tool would not write this text: the editor stays, and says so.
            editor.message.get_or_insert_with(|| {
                "This text cannot be written to the page as it is.".to_owned()
            });
            editor.focus = true;
            self.markup.text_editor = Some(editor);
            self.apply_intents(view_id, intents);
            self.refresh_text_preview();
            return false;
        }
        self.apply_intents(view_id, intents);
        true
    }

    /// Closes the in-place editor and throws away what was typed. Returns whether an editor
    /// was open.
    pub fn cancel_text_edit(&mut self) -> bool {
        let Some(editor) = self.markup.text_editor.take() else {
            return false;
        };
        match editor.target {
            TextTarget::Existing(_) => {
                if editor.applied {
                    if let Ok(doc) = self.session.document_mut(editor.doc) {
                        // The whole session was one undo step.
                        let _ = doc.undo();
                    }
                    self.after_edit(editor.doc);
                }
                self.markup.fields.session += 1;
            }
            TextTarget::Draft => {
                let Some(dv) = self.views.get_mut(&editor.view) else {
                    return true;
                };
                let Ok(doc) = self.session.document(editor.doc) else {
                    return true;
                };
                let kind = dv.tool.tool();
                let page = dv.tool.page().map_or(dv.shown_page(), |p| p as u32);
                let state = std::mem::replace(&mut dv.tool, ToolState::for_tool(kind));
                let view = ToolView::new(doc, &dv.view, &self.markup.author, page);
                let (state, _, _) = pl_tools::step_text(state, &TextInput::Cancel, &view);
                dv.tool = state.cancelled();
                dv.preview = DrawList::new();
                dv.preview_serial += 1;
                dv.preview_page = None;
            }
        }
        true
    }
}

/// Shows the in-place editor of `view`, if it has one, beside the markup it types into.
/// `canvas` is where the view's pages are on screen.
pub(crate) fn show_editor(
    app: &mut AppState,
    ctx: &egui::Context,
    view: ViewId,
    canvas: egui::Rect,
) {
    if app
        .markup
        .text_editor
        .as_ref()
        .is_none_or(|e| e.view != view)
    {
        return;
    }
    let Some(mut editor) = app.markup.text_editor.take() else {
        return;
    };
    // An existing markup is where the document has it now; it may be gone (undone, deleted).
    if let TextTarget::Existing(id) = &editor.target {
        let found = app
            .session
            .document(editor.doc)
            .ok()
            .and_then(|d| d.annotation(id))
            .map(|a| (a.page as u32, a.bounds()));
        match found {
            Some(anchor) => editor.anchor = Some(anchor),
            None => return,
        }
    }
    let saving = app.doc_is_saving(editor.doc);
    // The markup's box on screen.
    let on_screen = editor.anchor.and_then(|(page, rect)| {
        let dv = app.views.get(&view)?;
        let transform = dv.view.view_transform(page)?;
        let geometry = dv.view.page_geometry(page)?;
        let r = transform.page_to_view(geometry).transform_rect_bbox(rect);
        Some(egui::Rect::from_min_max(
            egui::pos2(canvas.min.x + r.x0 as f32, canvas.min.y + r.y0 as f32),
            egui::pos2(canvas.min.x + r.x1 as f32, canvas.min.y + r.y1 as f32),
        ))
    });
    let markup = on_screen
        .unwrap_or_else(|| egui::Rect::from_center_size(canvas.center(), egui::Vec2::ZERO));
    let width = markup.width().clamp(240.0, 420.0);
    let height = if editor.area.is_positive() {
        editor.area.height()
    } else {
        118.0
    };
    let gap = 8.0;
    let above = markup.top() - height - gap >= canvas.top();
    let y = if above {
        markup.top() - height - gap
    } else {
        (markup.bottom() + gap)
            .min(canvas.bottom() - height)
            .max(canvas.top())
    };
    let x = markup
        .left()
        .min(canvas.right() - width - 12.0)
        .max(canvas.left() + 4.0);

    let mut commit = false;
    let mut cancel = false;
    let mut changed = false;
    let palette = Palette::of(&ctx.global_style().visuals);
    let area = egui::Area::new(Id::unique(EDITOR_ID))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(x, y))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(width);
                ui.add_enabled_ui(!saving, |ui| {
                    // Ctrl+Enter finishes; it must not reach the text box as a line break.
                    commit |= ui.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter));
                    let response = ui
                        .add(
                            TextEdit::multiline(&mut editor.text)
                                .id(Id::unique(EDITOR_ID).with("text"))
                                .desired_rows(3)
                                .desired_width(f32::INFINITY)
                                .hint_text("Type the text"),
                        )
                        .accessible_name(EDITOR_NAME);
                    let pointer_busy =
                        ui.input(|i| i.pointer.any_down() || i.pointer.any_released());
                    if editor.focus && !pointer_busy {
                        response.request_focus();
                        editor.focus = false;
                    } else if editor.focus {
                        ui.ctx().request_repaint();
                    }
                    changed = response.changed();
                    if let Some(message) = &editor.message {
                        ui.label(RichText::new(message).color(palette.warning));
                    }
                    let mut focused = response.has_focus();
                    ui.horizontal(|ui| {
                        let done = ui
                            .button("Done")
                            .on_hover_text("Keep the text (Ctrl+Enter)");
                        let dismiss = ui
                            .button("Cancel")
                            .on_hover_text("Throw the text away (Esc)");
                        focused |= done.has_focus() || dismiss.has_focus();
                        commit |= done.clicked();
                        cancel |= dismiss.clicked();
                    });
                    // Leaving the editor finishes the text: a click somewhere else, or the
                    // keyboard focus moving on past its buttons. Where the pointer happens
                    // to rest decides nothing: Tab from the text goes to Done and Cancel,
                    // which are part of the editor, and they are worked from the keyboard.
                    let left = editor.had_focus && !focused;
                    if left && !commit && !cancel {
                        let clicking_inside = ui.input(|i| {
                            let p = &i.pointer;
                            (p.any_pressed() || p.any_down() || p.any_released())
                                && p.interact_pos()
                                    .is_some_and(|at| editor.area.expand(4.0).contains(at))
                        });
                        let escaped = ui.input(|i| i.key_pressed(Key::Escape));
                        if escaped {
                            cancel = true;
                        } else if !clicking_inside {
                            commit = true;
                        }
                    }
                    editor.had_focus = focused;
                });
            });
        });
    editor.area = area.response.rect;
    app.markup.text_editor = Some(editor);
    if changed {
        // Where the text cursor stands, so that it stays behind what was just typed when
        // some of that is taken out again.
        let text_id = Id::unique(EDITOR_ID).with("text");
        let state = TextEdit::load_state(ctx, text_id);
        let before = state
            .as_ref()
            .and_then(|s| s.cursor.char_range())
            .map(|range| range.primary.index.0);
        let after = app.text_edit_changed_at(before);
        if let (Some(mut state), Some(after), true) = (state, after, after != before) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(after),
                )));
            TextEdit::store_state(ctx, text_id, state);
        }
    }
    if cancel {
        app.cancel_text_edit();
    } else if commit {
        app.commit_text_edit();
    }
}

// ---- the note window ------------------------------------------------------------------------

/// What the note window shows of a markup.
struct NoteFacts {
    title: String,
    text: String,
    state: ReviewState,
    replies: Vec<(String, String)>,
    locked: bool,
}

fn note_facts(app: &AppState, note: &NotePopup) -> Option<NoteFacts> {
    let doc = app.session.document(note.doc).ok()?;
    let editor = doc.editor()?;
    let a = editor.set().get(&note.id)?;
    let author = a.meta.author.clone().unwrap_or_default();
    let subject = a
        .meta
        .subject
        .clone()
        .unwrap_or_else(|| crate::panels::kind_label(&a.kind).to_owned());
    let title = if author.is_empty() {
        format!("{subject}, page {}", a.page + 1)
    } else {
        format!("{subject} by {author}, page {}", a.page + 1)
    };
    let replies = editor
        .set()
        .thread(&note.id)
        .into_iter()
        // Status annotations of every model (review, checkmark, a custom set) are not replies.
        .filter(|r| r.status_entry().is_none() && r.review_state().is_none() && r.id != note.id)
        .filter(|r| r.meta.contents.as_deref().is_some_and(|c| !c.is_empty()))
        .map(|r| {
            (
                r.meta
                    .author
                    .clone()
                    .unwrap_or_else(|| "Someone".to_owned()),
                r.meta.contents.clone().unwrap_or_default(),
            )
        })
        .collect();
    Some(NoteFacts {
        title,
        text: a.meta.contents.clone().unwrap_or_default(),
        state: editor.set().review_state(&note.id),
        replies,
        locked: a.meta.flags.is_locked(),
    })
}

/// The note window: a markup's text, its review status and its replies.
pub(crate) fn show_note(app: &mut AppState, ctx: &egui::Context) {
    let Some(mut note) = app.markup.note.take() else {
        return;
    };
    let Some(facts) = note_facts(app, &note) else {
        // The markup is gone (deleted, undone, its document closed).
        return;
    };
    let saving = app.doc_is_saving(note.doc);
    let muted = Palette::of(&ctx.global_style().visuals).muted_text;
    let mut close = false;
    let mut new_text = None;
    let mut new_state = None;
    let mut add_reply = false;
    let mut started = false;
    egui::Window::new("Note")
        .id(Id::unique("plumbline-note-window"))
        .collapsible(false)
        .resizable(false)
        .default_width(280.0)
        .show(ctx, |ui| {
            ui.set_width(280.0);
            ui.label(RichText::new(&facts.title).color(muted));
            ui.add_enabled_ui(!saving && !facts.locked, |ui| {
                let id = Id::unique(NOTE_TEXT_ID);
                if !ui.memory(|m| m.has_focus(id)) {
                    note.text = facts.text.clone();
                }
                let response = ui
                    .add(
                        TextEdit::multiline(&mut note.text)
                            .id(id)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY),
                    )
                    .accessible_name("Note text");
                let pointer_busy = ui.input(|i| i.pointer.any_down() || i.pointer.any_released());
                if note.focus && !pointer_busy {
                    response.request_focus();
                    note.focus = false;
                } else if note.focus {
                    ui.ctx().request_repaint();
                }
                started |= response.gained_focus();
                if response.changed() {
                    new_text = Some(note.text.clone());
                }
            });
            ui.add_enabled_ui(!saving, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Status");
                    egui::ComboBox::from_id_salt("plumbline-note-status")
                        .selected_text(facts.state.name())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for state in ReviewState::ALL {
                                if ui
                                    .selectable_label(facts.state == state, state.name())
                                    .clicked()
                                {
                                    new_state = Some(state);
                                }
                            }
                        })
                        .response
                        .accessible_name("Review status");
                });
                ui.add_space(2.0);
                ui.label(RichText::new("Replies").font(theme::bold(12.5)));
                if facts.replies.is_empty() {
                    ui.label(RichText::new("No replies yet.").color(muted));
                }
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("plumbline-note-replies")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for (author, text) in &facts.replies {
                            ui.add(egui::Label::new(format!("{author}: {text}")).wrap());
                        }
                    });
                theme::name_scroll_bars(ui.ctx(), scroll.id, "Replies");
                ui.horizontal(|ui| {
                    let response = ui
                        .add(
                            TextEdit::singleline(&mut note.reply)
                                .id(Id::unique(NOTE_REPLY_ID))
                                .desired_width(170.0)
                                .hint_text("Write a reply"),
                        )
                        .accessible_name("Reply text");
                    add_reply |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    add_reply |= ui.button("Add reply").clicked();
                });
            });
            ui.add_space(4.0);
            close = ui.button("Close note").clicked();
        });
    if started {
        app.markup.fields.session += 1;
        note.key = app.markup.fields.session.wrapping_mul(16).wrapping_add(10);
    }
    let origin = Origin::now(app.markup.author.as_str());
    if let Some(text) = new_text {
        let command = app
            .session
            .document(note.doc)
            .ok()
            .and_then(|d| d.editor())
            .map(|e| pl_doc::markup_ops::set_text(e.set(), &note.id, &text, origin.when));
        match command {
            Some(Ok(command)) => {
                app.exec(note.doc, command, Some(note.key));
            }
            Some(Err(e)) => app.chrome.status = Some(format!("The text could not be changed: {e}")),
            None => {}
        }
    }
    if let Some(state) = new_state {
        app.set_review_status(note.doc, &note.id, state);
    }
    if add_reply && !note.reply.trim().is_empty() {
        let text = std::mem::take(&mut note.reply);
        app.add_reply_to(note.doc, &note.id, text.trim());
    }
    if !close {
        app.markup.note = Some(note);
    }
}

impl AppState {
    /// Records a review status for a markup, as one undo step.
    pub(crate) fn set_review_status(
        &mut self,
        doc_id: DocumentId,
        id: &AnnotId,
        state: ReviewState,
    ) {
        let origin = Origin::now(self.markup.author.as_str());
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return;
        };
        match doc.set_status(id, state, &origin) {
            Ok(_) => self.after_edit(doc_id),
            Err(e) => self.chrome.status = Some(format!("The status could not be set: {e}")),
        }
    }

    /// Sets a markup's status to a state of any status set (a custom set's under its own
    /// state model), as one undo step.
    pub(crate) fn set_named_status(
        &mut self,
        doc_id: DocumentId,
        id: &AnnotId,
        status: &pl_doc::pl_annot::Status,
    ) {
        let origin = Origin::now(self.markup.author.as_str());
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return;
        };
        match doc.set_status_named(id, status, &origin) {
            Ok(_) => self.after_edit(doc_id),
            Err(e) => self.chrome.status = Some(format!("The status could not be set: {e}")),
        }
    }

    /// Adds a reply to a markup (or to another reply), as one undo step.
    pub(crate) fn add_reply_to(&mut self, doc_id: DocumentId, id: &AnnotId, text: &str) -> bool {
        let origin = Origin::now(self.markup.author.as_str());
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return false;
        };
        match doc.add_reply(id, text, &origin) {
            Ok(_) => {
                self.after_edit(doc_id);
                true
            }
            Err(e) => {
                self.chrome.status = Some(format!("The reply could not be added: {e}"));
                false
            }
        }
    }
}
