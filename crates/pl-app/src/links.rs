//! Links: following them, the history of where the view has been, the Link tool and the
//! dialog that says where a link leads.
//!
//! A click with the Pan tool, or a double-click with the Select tool, follows the link under
//! the pointer; the pointer over a link shows where it leads. A link to a place in the document
//! moves the view and remembers where it was (Alt+Left goes back, Alt+Right forward again); a
//! link to another PDF opens it in a tab; a web address is opened in the browser only after
//! the user says yes in the window; a link that would start a program or open another kind of
//! file is refused. Nothing is ever started from a link.
//!
//! The Link tool drags out the link's area, then asks where it leads (a page of this
//! document, a page of another PDF, or a web address). Links are selected, moved and deleted
//! with the Select tool like markups; their target is changed from the properties panel.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use egui::{Id, Pos2, RichText, Ui};
use pl_doc::pl_annot::Geometry;
use pl_doc::pl_geom::{Point, Rect, Vec2};
use pl_doc::pl_pdf::{LinkAction, LinkDest, RemoteDest, SignaturePolicy};
use pl_doc::pl_render::DocId;
use pl_doc::{DestSpec, DocumentId, LinkData, LinkInfo, LinkTarget, PageMap, StructCommand};

use crate::app::{AppState, ViewId};
use crate::theme::{self, Palette};

/// The links of every page of a document, read for one display of it.
pub(crate) struct LinkCache {
    pub render: DocId,
    pub links: Option<Arc<Vec<Vec<LinkInfo>>>>,
}

/// A place a view has been, for Back and Forward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NavPoint {
    pub page: u32,
    /// The scroll position, while the pages have not changed since.
    pub scroll: Option<Vec2>,
}

/// Where a view has been and, after going back, where it can go forward to.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NavHistory {
    pub back: Vec<NavPoint>,
    pub forward: Vec<NavPoint>,
}

impl NavHistory {
    /// Follows a structural change: places on pages that are gone are forgotten.
    pub(crate) fn remap(&mut self, map: &PageMap) {
        let fix = |list: &mut Vec<NavPoint>| {
            *list = list
                .iter()
                .filter_map(|p| {
                    map.map(p.page).map(|page| NavPoint {
                        page,
                        scroll: p.scroll.filter(|_| page == p.page),
                    })
                })
                .collect();
        };
        fix(&mut self.back);
        fix(&mut self.forward);
    }
}

/// A link area being dragged out with the Link tool (page space).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LinkDrag {
    pub view: ViewId,
    pub page: u32,
    pub from: Point,
    pub to: Point,
}

/// What kind of place the link dialog makes the link lead to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Page,
    File,
    Web,
}

/// The dialog that says where a new (or edited) link leads.
pub(crate) struct LinkDialog {
    pub doc: DocumentId,
    pub page: u32,
    pub rect: Rect,
    /// The link being changed (its id in the store); `None` for a new link.
    pub edit: Option<String>,
    pub kind: LinkKind,
    /// One-based page number in this document.
    pub page_text: String,
    pub file_text: String,
    /// One-based page number in the other file.
    pub file_page_text: String,
    pub url_text: String,
    pub error: Option<String>,
}

/// Where a link leads, as words: what the pointer over it shows.
pub fn describe_link(info: &LinkInfo) -> String {
    match &info.link.action {
        LinkAction::GoTo(LinkDest::Broken) => "Leads to a page that does not exist".into(),
        LinkAction::GoTo(_) => match &info.resolved {
            Some(dest) => format!("Go to page {}", dest.page_index + 1),
            None => "Leads to a place that does not exist".into(),
        },
        LinkAction::GoToR { file, dest, .. } => match dest {
            Some(RemoteDest::Page { page_index, .. }) => {
                format!("Open {file}, page {}", page_index + 1)
            }
            _ => format!("Open {file}"),
        },
        LinkAction::Uri(uri) => format!("Open {uri} in the browser"),
        LinkAction::Launch { file, .. } => format!("Would start {file} (not followed)"),
        LinkAction::Named(name) => match name.as_str() {
            "NextPage" => "Next page".into(),
            "PrevPage" => "Previous page".into(),
            "FirstPage" => "First page".into(),
            "LastPage" => "Last page".into(),
            other => format!("Viewer action {other}"),
        },
        LinkAction::Other(kind) => format!("A {kind} action (not followed)"),
        LinkAction::None => "Leads nowhere".into(),
    }
}

/// What following a link to another file may do. Nothing here touches the file system: even
/// asking whether a network path exists makes Windows contact that computer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkedFile {
    /// A PDF named relative to the document, in its folder or below: opened at once.
    Beside(PathBuf),
    /// A PDF somewhere else on this computer: opened only after the user's yes.
    Ask(PathBuf),
    /// Never followed, with the reason for the status line.
    Refused(String),
}

/// The host of a web address (`mailto:` addresses: the part after `@`). Non-ASCII characters
/// elsewhere are percent-encoded when the link is written; a host's must not be.
pub fn url_host(url: &str) -> &str {
    let rest = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => match url.find(':') {
            Some(i) => &url[i + 1..],
            None => url,
        },
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.rsplit('@').next().unwrap_or(authority)
}

/// Where the file a link names is, and whether it may be opened (see [`LinkedFile`]).
pub fn linked_file(file: &str, document: Option<&Path>) -> LinkedFile {
    let normal = file.replace('/', std::path::MAIN_SEPARATOR_STR);
    if normal.starts_with(r"\\") {
        return LinkedFile::Refused(format!(
            "This link leads to {file}, a file on another computer. Plumbline does not open \
             network files from links."
        ));
    }
    let path = PathBuf::from(&normal);
    let is_pdf = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    if !is_pdf {
        return LinkedFile::Refused(format!(
            "This link would open {file}. Plumbline opens only PDF files from links."
        ));
    }
    let plain_relative = path.components().all(|c| {
        matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    match document.and_then(Path::parent) {
        Some(dir) if plain_relative => LinkedFile::Beside(dir.join(path)),
        Some(dir)
            if path.is_relative()
                && !path.has_root()
                && !path
                    .components()
                    .any(|c| matches!(c, std::path::Component::Prefix(_))) =>
        {
            LinkedFile::Ask(dir.join(path))
        }
        _ if path.is_absolute() => LinkedFile::Ask(path),
        // A path that is neither (no folder to read it in, or a drive-relative one).
        _ => LinkedFile::Refused(format!("The linked file {file} cannot be found from here.")),
    }
}

impl AppState {
    // ---- reading the links -------------------------------------------------------------------

    /// Every link of a document, read once per display of it. `None` when they cannot be read
    /// (a document the structure layer cannot parse) or while it is being saved.
    pub(crate) fn document_links(&mut self, doc_id: DocumentId) -> Option<Arc<Vec<Vec<LinkInfo>>>> {
        let doc = self.session.document_mut(doc_id).ok()?;
        if doc.is_saving() {
            return None;
        }
        let render = doc.render_id();
        if let Some(cache) = self.structure.links.get(&doc_id) {
            if cache.render == render {
                return cache.links.clone();
            }
        }
        let links = doc.document_links().ok().map(Arc::new);
        self.structure.links.insert(
            doc_id,
            LinkCache {
                render,
                links: links.clone(),
            },
        );
        links
    }

    /// The link of a document's page under a page point: the topmost, where it is now (a link
    /// moved or deleted with the Select tool counts as it stands).
    pub fn link_at(&mut self, doc_id: DocumentId, page: u32, point: Point) -> Option<LinkInfo> {
        let links = self.document_links(doc_id)?;
        let doc = self.session.document(doc_id).ok()?;
        let loaded = doc.annots_loaded();
        links
            .get(page as usize)?
            .iter()
            .rev()
            .find(|info| {
                let rect = match (&info.id, loaded) {
                    (Some(id), true) => match doc.annotation(id).map(|a| &a.geometry) {
                        Some(Geometry::Rect(r)) => *r,
                        Some(_) => info.link.rect,
                        // Deleted and not saved yet.
                        None => return false,
                    },
                    _ => info.link.rect,
                };
                rect.contains(point)
            })
            .cloned()
    }

    /// The link of a document with this store id, as it was read from the structure.
    pub(crate) fn link_by_id(&mut self, doc_id: DocumentId, id: &str) -> Option<LinkInfo> {
        let links = self.document_links(doc_id)?;
        links
            .iter()
            .flatten()
            .find(|l| l.id.as_ref().is_some_and(|i| i.as_str() == id))
            .cloned()
    }

    /// Where the link under the pointer leads, as the tooltip says it; `None` when the pointer
    /// is not over a link.
    pub fn hovered_link(&self) -> Option<&str> {
        self.structure.hovered_link.as_deref()
    }

    // ---- following ---------------------------------------------------------------------------

    /// Remembers where a view is before it follows a link or a bookmark.
    pub(crate) fn push_history(&mut self, view: ViewId) {
        let Some(dv) = self.views.get(&view) else {
            return;
        };
        let point = NavPoint {
            page: dv.shown_page(),
            scroll: Some(dv.view.scroll()),
        };
        let history = self.structure.history.entry(view).or_default();
        if history.back.last() != Some(&point) {
            history.back.push(point);
        }
        history.forward.clear();
        // A long session does not keep every place forever.
        if history.back.len() > 100 {
            history.back.remove(0);
        }
    }

    /// Goes back to where the active view was before the last link or bookmark it followed.
    pub fn navigate_back(&mut self) -> bool {
        self.navigate(true)
    }

    /// Goes forward again after [`AppState::navigate_back`].
    pub fn navigate_forward(&mut self) -> bool {
        self.navigate(false)
    }

    fn navigate(&mut self, back: bool) -> bool {
        let Some(id) = self.active else {
            return false;
        };
        let Some(dv) = self.views.get(&id) else {
            return false;
        };
        let here = NavPoint {
            page: dv.shown_page(),
            scroll: Some(dv.view.scroll()),
        };
        let history = self.structure.history.entry(id).or_default();
        let (from, to) = if back {
            (&mut history.back, &mut history.forward)
        } else {
            (&mut history.forward, &mut history.back)
        };
        let Some(target) = from.pop() else {
            return false;
        };
        to.push(here);
        let Some(dv) = self.views.get_mut(&id) else {
            return false;
        };
        dv.go_to_page(target.page);
        if let Some(scroll) = target.scroll {
            dv.view.set_scroll(scroll);
            dv.go_to_page_pinned(target.page);
        }
        true
    }

    /// Follows a link from a view: see the module documentation.
    pub fn follow_link(&mut self, view: ViewId, info: &LinkInfo) {
        let Some(dv) = self.views.get(&view) else {
            return;
        };
        let doc_id = dv.doc;
        match &info.link.action {
            LinkAction::GoTo(_) => match &info.resolved {
                Some(dest) => {
                    self.push_history(view);
                    let exact = self
                        .session
                        .document(doc_id)
                        .is_ok_and(|d| d.geometry_is_exact(dest.page_index as u32));
                    if let Some(dv) = self.views.get_mut(&view) {
                        dv.go_to_destination(dest.page_index as u32, dest.left, dest.top, exact);
                    }
                }
                None => {
                    self.chrome.status =
                        Some("This link leads to a place that does not exist.".into());
                }
            },
            LinkAction::Named(name) => {
                let Some(dv) = self.views.get_mut(&view) else {
                    return;
                };
                match name.as_str() {
                    "NextPage" => dv.next_page(),
                    "PrevPage" => dv.previous_page(),
                    "FirstPage" => dv.go_to_page(0),
                    "LastPage" => dv.go_to_page(u32::MAX),
                    other => {
                        self.chrome.status =
                            Some(format!("The viewer action {other} is not supported."));
                    }
                }
            }
            LinkAction::GoToR { file, dest, .. } => {
                let base = self
                    .session
                    .document(doc_id)
                    .ok()
                    .and_then(|d| d.path().map(Path::to_path_buf));
                let page = match dest {
                    Some(RemoteDest::Page { page_index, .. }) => *page_index as u32,
                    _ => 0,
                };
                match linked_file(file, base.as_deref()) {
                    LinkedFile::Beside(path) => self.open_linked_file(&path, page),
                    LinkedFile::Ask(path) => self.structure.file_prompt = Some((path, page)),
                    LinkedFile::Refused(why) => self.chrome.status = Some(why),
                }
            }
            LinkAction::Uri(uri) => {
                self.structure.url_prompt = Some(uri.clone());
            }
            LinkAction::Launch { file, .. } => {
                self.chrome.status = Some(format!(
                    "This link would start {file}. Plumbline does not start programs or open \
                     other files from links."
                ));
            }
            LinkAction::Other(kind) => {
                self.chrome.status = Some(format!("Links with a {kind} action are not followed."));
            }
            LinkAction::None => {
                self.chrome.status = Some("This link leads nowhere.".into());
            }
        }
    }

    /// Follows the link clicked on a page this frame, now that the tabs are laid out.
    pub(crate) fn follow_clicked_link(&mut self) {
        if let Some((view, info)) = self.structure.clicked_link.take() {
            self.follow_link(view, &info);
        }
    }

    /// Follows the link at a page point of a view, if there is one. Returns whether there was.
    pub fn follow_link_at(&mut self, view: ViewId, page: u32, point: Point) -> bool {
        let Some(doc) = self.views.get(&view).map(|v| v.doc) else {
            return false;
        };
        match self.link_at(doc, page, point) {
            Some(info) => {
                self.follow_link(view, &info);
                true
            }
            None => false,
        }
    }

    fn open_linked_file(&mut self, path: &Path, page: u32) {
        if !path.is_file() {
            self.chrome.status = Some(format!("The linked file {} was not found.", path.display()));
            return;
        }
        if let Some(opened) = self.open_path(path) {
            if let Some(dv) = self.views.get_mut(&opened) {
                dv.go_to_page(page);
            }
        }
    }

    /// The file outside the document's folder a link leads to, waiting for a yes before it
    /// is opened, if one is.
    pub fn file_prompt(&self) -> Option<&Path> {
        self.structure
            .file_prompt
            .as_ref()
            .map(|(p, _)| p.as_path())
    }

    /// Answers the question about a linked file: `true` opens it.
    pub fn answer_file_prompt(&mut self, open: bool) {
        if let Some((path, page)) = self.structure.file_prompt.take() {
            if open {
                self.open_linked_file(&path, page);
            }
        }
    }

    /// The web address waiting for a yes before the browser opens, if one is.
    pub fn url_prompt(&self) -> Option<&str> {
        self.structure.url_prompt.as_deref()
    }

    /// Answers the question about a web address: `true` opens it in the browser.
    pub fn answer_url_prompt(&mut self, open: bool) {
        let Some(url) = self.structure.url_prompt.take() else {
            return;
        };
        if !open {
            return;
        }
        if !crate::platform::url_is_openable(&url) {
            self.chrome.status = Some(format!(
                "{url} is not a web address; Plumbline opens only http, https and mailto links."
            ));
            return;
        }
        if let Err(e) = self.dialogs.open_url(&url) {
            self.chrome.status = Some(format!("The address could not be opened: {e}"));
        }
    }

    // ---- making and changing links -----------------------------------------------------------

    /// Opens the link dialog for a new link over `rect` of `page` of the active document.
    pub fn new_link_dialog(&mut self, page: u32, rect: Rect) {
        let Some(doc) = self.active_doc_id() else {
            return;
        };
        let next = self
            .session
            .document(doc)
            .map(|d| (page + 2).min(d.page_count()))
            .unwrap_or(1);
        self.structure.link_dialog = Some(LinkDialog {
            doc,
            page,
            rect,
            edit: None,
            kind: LinkKind::Page,
            page_text: next.to_string(),
            file_text: String::new(),
            file_page_text: "1".into(),
            url_text: "https://".into(),
            error: None,
        });
    }

    /// Opens the link dialog to change where the selected link leads.
    pub fn edit_link_dialog(&mut self) -> bool {
        let Some(doc_id) = self.active_doc_id() else {
            return false;
        };
        let Some(markup) = self.first_selected().cloned() else {
            return false;
        };
        if markup.kind != pl_doc::pl_annot::AnnotKind::Link {
            return false;
        }
        let rect = match markup.geometry {
            Geometry::Rect(r) => r,
            _ => return false,
        };
        let info = self.link_by_id(doc_id, markup.id.as_str());
        let mut dialog = LinkDialog {
            doc: doc_id,
            page: markup.page as u32,
            rect,
            edit: Some(markup.id.as_str().to_owned()),
            kind: LinkKind::Page,
            page_text: "1".into(),
            file_text: String::new(),
            file_page_text: "1".into(),
            url_text: "https://".into(),
            error: None,
        };
        if let Some(info) = info {
            match &info.link.action {
                LinkAction::Uri(u) => {
                    dialog.kind = LinkKind::Web;
                    dialog.url_text = u.clone();
                }
                LinkAction::GoToR { file, dest, .. } => {
                    dialog.kind = LinkKind::File;
                    dialog.file_text = file.clone();
                    if let Some(RemoteDest::Page { page_index, .. }) = dest {
                        dialog.file_page_text = (page_index + 1).to_string();
                    }
                }
                _ => {
                    if let Some(dest) = &info.resolved {
                        dialog.page_text = (dest.page_index + 1).to_string();
                    }
                }
            }
        }
        self.structure.link_dialog = Some(dialog);
        true
    }

    pub fn link_dialog_open(&self) -> bool {
        self.structure.link_dialog.is_some()
    }

    /// Sets what the open link dialog makes the link lead to (as its fields would).
    pub fn set_link_dialog_target(&mut self, kind: LinkKind, text: &str, page: Option<u32>) {
        let Some(dialog) = self.structure.link_dialog.as_mut() else {
            return;
        };
        dialog.kind = kind;
        match kind {
            LinkKind::Page => dialog.page_text = text.to_owned(),
            LinkKind::File => {
                dialog.file_text = text.to_owned();
                dialog.file_page_text = page.unwrap_or(1).to_string();
            }
            LinkKind::Web => dialog.url_text = text.to_owned(),
        }
    }

    /// Makes (or changes) the link as the dialog says. Returns whether the document changed.
    pub fn finish_link_dialog(&mut self) -> bool {
        let Some(dialog) = self.structure.link_dialog.as_ref() else {
            return false;
        };
        let count = self
            .session
            .document(dialog.doc)
            .map(|d| d.page_count())
            .unwrap_or(0);
        let page_number = |text: &str, max: Option<u32>| -> Result<u32, String> {
            let n: u32 = text
                .trim()
                .parse()
                .map_err(|_| format!("\"{}\" is not a page number", text.trim()))?;
            if n == 0 || max.is_some_and(|m| n > m) {
                return Err(match max {
                    Some(m) => format!("There is no page {n} (1 to {m})."),
                    None => format!("There is no page {n}."),
                });
            }
            Ok(n - 1)
        };
        let target = match dialog.kind {
            LinkKind::Page => page_number(&dialog.page_text, Some(count))
                .map(|p| LinkTarget::Page(DestSpec::page(p))),
            LinkKind::File => {
                if dialog.file_text.trim().is_empty() {
                    Err("Choose the file the link opens.".to_owned())
                } else {
                    page_number(&dialog.file_page_text, None).map(|p| LinkTarget::File {
                        file: dialog.file_text.trim().to_owned(),
                        page: Some(p),
                    })
                }
            }
            LinkKind::Web => {
                let url = dialog.url_text.trim();
                if crate::platform::url_is_openable(url) && !url_host(url).is_ascii() {
                    Err(
                        "Write the domain name in its ASCII form (xn--...); a PDF link \
                         address is plain ASCII."
                            .to_owned(),
                    )
                } else if crate::platform::url_is_openable(url) && url.len() > "https://".len() {
                    Ok(LinkTarget::Uri(url.to_owned()))
                } else {
                    Err("Give a web address starting with http:// or https://.".to_owned())
                }
            }
        };
        let target = match target {
            Ok(t) => t,
            Err(e) => {
                if let Some(d) = self.structure.link_dialog.as_mut() {
                    d.error = Some(e);
                }
                return false;
            }
        };
        let dialog = self.structure.link_dialog.take().expect("checked above");
        let link = LinkData::new(dialog.rect, target);
        let command = match dialog.edit {
            Some(id) => StructCommand::EditLink { id, link },
            None => StructCommand::AddLink {
                page: dialog.page,
                link,
            },
        };
        self.apply_structure_to(dialog.doc, command, SignaturePolicy::Refuse)
    }

    /// The file chooser of the link dialog: the chosen file is named relative to the
    /// document's folder when it is inside it.
    fn browse_link_file(&mut self) {
        let Some(path) = self.dialogs.pick_pdfs().into_iter().next() else {
            return;
        };
        let Some(dialog) = self.structure.link_dialog.as_ref() else {
            return;
        };
        let base = self
            .session
            .document(dialog.doc)
            .ok()
            .and_then(|d| d.path().and_then(Path::parent).map(Path::to_path_buf));
        let shown = base
            .and_then(|dir| pl_doc::relative_path(&path, &dir))
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        if let Some(dialog) = self.structure.link_dialog.as_mut() {
            dialog.file_text = shown;
            dialog.error = None;
        }
    }
}

// ---- the page canvas ----------------------------------------------------------------------------

/// What the canvas does with links this frame: the Link tool's drag, a click that follows a
/// link (Pan tool, or a Select double-click), and the pointer's hover. Returns whether the
/// Link tool had the pointer (no other tool gets it then).
pub(crate) fn canvas_links(
    app: &mut AppState,
    ui: &Ui,
    id: ViewId,
    response: &egui::Response,
    rect: egui::Rect,
) -> bool {
    use crate::markup::Tool;
    let tool = app.markup.tool;
    let origin = rect.min;
    let local = |p: Pos2| Point::new((p.x - origin.x) as f64, (p.y - origin.y) as f64);
    let Some(dv) = app.views.get(&id) else {
        return false;
    };
    let doc_id = dv.doc;
    let pointer = ui.input(|i| i.pointer.latest_pos());
    let hit = pointer
        .filter(|p| rect.contains(*p))
        .and_then(|p| dv.view.hit_test(local(p)))
        .filter(|h| h.inside);

    // ---- the Link tool ------------------------------------------------------------------------
    if tool == Tool::Link && !app.markup.snapshot_pick {
        let saving = app.doc_is_saving(doc_id);
        if response.drag_started_by(egui::PointerButton::Primary) && !saving {
            let start = ui.input(|i| i.pointer.press_origin()).or(pointer);
            if let Some(h) = start
                .and_then(|p| app.views.get(&id)?.view.hit_test(local(p)))
                .filter(|h| h.inside)
            {
                app.structure.link_drag = Some(LinkDrag {
                    view: id,
                    page: h.page,
                    from: h.point,
                    to: h.point,
                });
            }
        }
        if let (Some(drag), Some(pos)) = (app.structure.link_drag.as_mut(), pointer) {
            if drag.view == id {
                if let Some(dv) = app.views.get(&id) {
                    let point = dv
                        .view
                        .view_transform(drag.page)
                        .zip(dv.view.page_geometry(drag.page))
                        .map(|(v, g)| v.view_to_page(g) * local(pos));
                    if let Some(point) = point {
                        drag.to = point;
                    }
                }
            }
        }
        if response.drag_stopped() {
            if let Some(drag) = app.structure.link_drag.take().filter(|d| d.view == id) {
                let area = Rect::new(
                    drag.from.x.min(drag.to.x),
                    drag.from.y.min(drag.to.y),
                    drag.from.x.max(drag.to.x),
                    drag.from.y.max(drag.to.y),
                );
                // Kept inside the page.
                let page_box = app
                    .views
                    .get(&id)
                    .and_then(|v| v.view.file_geometry(drag.page))
                    .map(|g| g.crop_box());
                let area = match page_box {
                    Some(b) => Rect::new(
                        area.x0.max(b.x0),
                        area.y0.max(b.y0),
                        area.x1.min(b.x1),
                        area.y1.min(b.y1),
                    ),
                    None => area,
                };
                if area.width() >= 2.0 && area.height() >= 2.0 {
                    app.active = Some(id);
                    app.new_link_dialog(drag.page, area);
                } else {
                    app.chrome.status = Some("Drag out the area of the link.".into());
                }
            }
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        paint_link_drag(app, ui, id, rect);
        app.structure.hovered_link = None;
        return true;
    }

    // ---- hover and click ------------------------------------------------------------------------
    let over = match hit {
        Some(h) if response.hovered() && !app.doc_is_saving(doc_id) => {
            app.link_at(doc_id, h.page, h.point)
        }
        _ => None,
    };
    let follow = match tool {
        Tool::Pan => response.clicked(),
        // The second of two quick clicks at one place.
        Tool::Select => response.clicked() && second_click(app, ui, response),
        _ => false,
    };
    app.structure.hovered_link = over.as_ref().map(describe_link);
    if let Some(info) = &over {
        if matches!(tool, Tool::Pan | Tool::Select) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
            .clone()
            .on_hover_text_at_pointer(describe_link(info));
    }
    if follow && !app.doc_is_saving(doc_id) {
        // Where the click was (the pointer may have left the window since).
        let clicked = response
            .interact_pointer_pos()
            .or(pointer)
            .filter(|p| rect.contains(*p))
            .and_then(|p| app.views.get(&id)?.view.hit_test(local(p)))
            .filter(|h| h.inside)
            .and_then(|h| app.link_at(doc_id, h.page, h.point));
        if let Some(info) = clicked {
            app.structure.clicked_link = Some((id, info));
        }
    }
    false
}

/// Whether this click on a page is the second of two quick ones at the same place: both in
/// this frame, or this one soon after the last. (egui's own count is lost when the pointer
/// leaves the window between the clicks.)
fn second_click(app: &mut AppState, ui: &Ui, response: &egui::Response) -> bool {
    const DELAY: f64 = 0.5;
    const DISTANCE: f32 = 6.0;
    let (now, releases) = ui.input(|i| {
        let releases = i
            .events
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    egui::Event::PointerButton {
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        ..
                    }
                )
            })
            .count();
        (i.time, releases)
    });
    let Some(pos) = response.interact_pointer_pos() else {
        return false;
    };
    let quick = app
        .structure
        .last_click
        .is_some_and(|(t, p)| now - t <= DELAY && p.distance(pos) <= DISTANCE);
    let double = releases >= 2 || quick;
    app.structure.last_click = if double { None } else { Some((now, pos)) };
    double
}

/// The area of the link being dragged out.
fn paint_link_drag(app: &AppState, ui: &Ui, id: ViewId, rect: egui::Rect) {
    let Some(drag) = app.structure.link_drag.filter(|d| d.view == id) else {
        return;
    };
    let Some(dv) = app.views.get(&id) else {
        return;
    };
    let (Some(a), Some(b)) = (
        dv.view.page_to_view(drag.page, drag.from),
        dv.view.page_to_view(drag.page, drag.to),
    ) else {
        return;
    };
    let to_pos = |p: Point| egui::pos2(rect.min.x + p.x as f32, rect.min.y + p.y as f32);
    let area = egui::Rect::from_two_pos(to_pos(a), to_pos(b));
    let palette = Palette::of(ui.visuals());
    let painter = ui.painter_at(rect);
    painter.rect_filled(area, 0.0, palette.selection.gamma_multiply(0.4));
    painter.rect_stroke(
        area,
        0.0,
        egui::Stroke::new(1.0, palette.accent),
        egui::StrokeKind::Inside,
    );
}

// ---- dialogs ------------------------------------------------------------------------------------

/// "Open this address in the browser?"
pub fn url_prompt(app: &mut AppState, ctx: &egui::Context) {
    let Some(url) = app.structure.url_prompt.clone() else {
        return;
    };
    let mut answer = None;
    let modal = egui::Modal::new(Id::unique("plumbline-url-prompt")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.label(RichText::new("Open web address?").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label("This link leads to an address outside the document:");
        ui.add(egui::Label::new(RichText::new(&url).monospace()).wrap());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button("Open in browser")
                .on_hover_text("Open the address in the default browser")
                .clicked()
            {
                answer = Some(true);
            }
            if ui
                .button("Cancel")
                .on_hover_text("Do not open it")
                .clicked()
            {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    if let Some(answer) = answer {
        app.answer_url_prompt(answer);
    }
}

/// "Open this file?" for a link to a PDF outside the document's folder.
pub fn file_prompt(app: &mut AppState, ctx: &egui::Context) {
    let Some(path) = app.file_prompt().map(Path::to_path_buf) else {
        return;
    };
    let mut answer = None;
    let modal = egui::Modal::new(Id::unique("plumbline-file-prompt")).show(ctx, |ui| {
        ui.set_width(380.0);
        ui.label(RichText::new("Open linked file?").font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label("This link leads to a file outside the document's folder:");
        ui.add(egui::Label::new(RichText::new(path.display().to_string()).monospace()).wrap());
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .button("Open file")
                .on_hover_text("Open the file in a new tab")
                .clicked()
            {
                answer = Some(true);
            }
            if ui
                .button("Cancel")
                .on_hover_text("Do not open it")
                .clicked()
            {
                answer = Some(false);
            }
        });
    });
    if answer.is_none() && modal.should_close() {
        answer = Some(false);
    }
    if let Some(answer) = answer {
        app.answer_file_prompt(answer);
    }
}

/// Where a link leads: a page here, a page of another PDF, or a web address.
pub fn link_dialog(app: &mut AppState, ctx: &egui::Context) {
    if app.structure.link_dialog.is_none() {
        return;
    }
    let mut finish = false;
    let mut cancel = false;
    let mut browse = false;
    let modal = egui::Modal::new(Id::unique("plumbline-link-dialog")).show(ctx, |ui| {
        let Some(dialog) = app.structure.link_dialog.as_mut() else {
            return;
        };
        ui.set_width(380.0);
        let title = if dialog.edit.is_some() {
            "Change link"
        } else {
            "New link"
        };
        ui.label(RichText::new(title).font(theme::bold(15.0)));
        ui.add_space(4.0);
        ui.label("Where does the link lead?");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut dialog.kind, LinkKind::Page, "Page of this document")
                .on_hover_text("A page of the document the link is in");
            ui.add_enabled(
                dialog.kind == LinkKind::Page,
                egui::TextEdit::singleline(&mut dialog.page_text).desired_width(50.0),
            )
            .accessible_name("Page number")
            .on_hover_text("The page the link goes to");
        });
        ui.radio_value(&mut dialog.kind, LinkKind::File, "Page of another PDF")
            .on_hover_text("Opens another PDF file at a page");
        ui.add_enabled_ui(dialog.kind == LinkKind::File, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.add(
                    egui::TextEdit::singleline(&mut dialog.file_text)
                        .desired_width(200.0)
                        .hint_text("file.pdf"),
                )
                .accessible_name("Linked file")
                .on_hover_text("The PDF the link opens, relative to this document's folder");
                browse = ui
                    .button("Browse\u{2026}")
                    .on_hover_text("Choose the PDF the link opens")
                    .clicked();
            });
            ui.horizontal(|ui| {
                ui.add_space(20.0);
                ui.label("Page");
                ui.add(egui::TextEdit::singleline(&mut dialog.file_page_text).desired_width(50.0))
                    .accessible_name("Page of the linked file")
                    .on_hover_text("The page of that file to show");
            });
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut dialog.kind, LinkKind::Web, "Web address")
                .on_hover_text("Opens an address in the browser, after asking");
            ui.add_enabled(
                dialog.kind == LinkKind::Web,
                egui::TextEdit::singleline(&mut dialog.url_text).desired_width(200.0),
            )
            .accessible_name("Web address")
            .on_hover_text("An address starting with http:// or https://");
        });
        if let Some(error) = &dialog.error {
            ui.label(error);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let ok = if dialog.edit.is_some() {
                "Change link"
            } else {
                "Make link"
            };
            finish = ui
                .button(ok)
                .on_hover_text("Make the link lead there")
                .clicked();
            cancel = ui
                .button("Cancel")
                .on_hover_text("Close without changing anything")
                .clicked();
        });
    });
    if cancel || (!finish && !browse && modal.should_close()) {
        app.structure.link_dialog = None;
    } else if browse {
        app.browse_link_file();
    } else if finish {
        app.finish_link_dialog();
    }
}

/// The link section of the properties panel, for a selected link: where it leads, and
/// buttons to follow it or change it.
pub(crate) fn link_properties(app: &mut AppState, ui: &mut Ui) {
    let Some(markup) = app.first_selected().cloned() else {
        return;
    };
    if markup.kind != pl_doc::pl_annot::AnnotKind::Link {
        return;
    }
    let Some(doc) = app.active_doc_id() else {
        return;
    };
    let info = app.link_by_id(doc, markup.id.as_str());
    ui.add_space(6.0);
    ui.label(RichText::new("Link").font(theme::bold(12.5)));
    let muted = Palette::of(ui.visuals()).muted_text;
    match &info {
        Some(info) => ui.label(RichText::new(describe_link(info)).color(muted)),
        None => ui.label(RichText::new("Where it leads is read when it is saved.").color(muted)),
    };
    ui.horizontal(|ui| {
        if ui
            .button("Change target\u{2026}")
            .on_hover_text("Change where the link leads")
            .clicked()
        {
            app.edit_link_dialog();
        }
        let follow = ui
            .add_enabled(info.is_some(), egui::Button::new("Follow"))
            .on_hover_text("Go where the link leads");
        if follow.clicked() {
            if let (Some(info), Some(view)) = (info, app.active) {
                app.follow_link(view, &info);
            }
        }
    });
}

impl crate::app::DocView {
    /// Goes to `page`, keeping the scroll the view has now (after a scroll was restored).
    pub(crate) fn go_to_page_pinned(&mut self, page: u32) {
        let scroll = self.view.scroll();
        self.pin_page(page, scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_files_are_found_beside_the_document() {
        let doc = Path::new("C:/plans/a.pdf");
        let LinkedFile::Beside(found) = linked_file("sheets/b.pdf", Some(doc)) else {
            panic!("a file beside the document is opened at once");
        };
        assert!(found.ends_with(Path::new("sheets").join("b.pdf")));
        assert!(found.starts_with("C:/plans"));
        // With no folder to read it in, a relative name leads nowhere.
        assert!(matches!(linked_file("b.pdf", None), LinkedFile::Refused(_)));
    }

    #[test]
    fn history_forgets_places_on_pages_that_are_gone() {
        let mut h = NavHistory {
            back: vec![
                NavPoint {
                    page: 0,
                    scroll: Some(Vec2::new(0.0, 10.0)),
                },
                NavPoint {
                    page: 2,
                    scroll: Some(Vec2::new(0.0, 900.0)),
                },
            ],
            forward: vec![NavPoint {
                page: 1,
                scroll: None,
            }],
        };
        h.remap(&PageMap {
            old_to_new: vec![Some(0), None, Some(1)],
            new_page_count: 2,
        });
        assert_eq!(h.back.len(), 2);
        assert_eq!(h.back[1].page, 1);
        assert_eq!(h.back[1].scroll, None, "a moved page loses its scroll");
        assert_eq!(h.back[0].scroll, Some(Vec2::new(0.0, 10.0)));
        assert!(h.forward.is_empty());
    }
    #[test]
    fn linked_files_are_classified_without_touching_the_disk() {
        let doc = Path::new(r"C:\Jobs\A\set.pdf");
        let beside = |p: &str| LinkedFile::Beside(Path::new(r"C:\Jobs\A").join(p));
        assert_eq!(linked_file("other.pdf", Some(doc)), beside("other.pdf"));
        assert_eq!(
            linked_file("Sheets/E-201.PDF", Some(doc)),
            beside(r"Sheets\E-201.PDF")
        );
        // Network paths are never followed (not even looked for).
        for unc in [
            r"\\host\share\x.pdf",
            "//host/share/x.pdf",
            r"\\?\UNC\h\s\x.pdf",
        ] {
            assert!(
                matches!(
                    linked_file(unc, Some(doc)),
                    LinkedFile::Refused(ref w) if w.contains("another computer")
                ),
                "{unc}"
            );
        }
        // Other kinds of file are not opened.
        assert!(matches!(
            linked_file("run.bat", Some(doc)),
            LinkedFile::Refused(_)
        ));
        assert!(matches!(
            linked_file("other", Some(doc)),
            LinkedFile::Refused(_)
        ));
        // Elsewhere on this computer: asked first.
        assert_eq!(
            linked_file(r"D:\Plans\x.pdf", Some(doc)),
            LinkedFile::Ask(PathBuf::from(r"D:\Plans\x.pdf"))
        );
        assert_eq!(
            linked_file("../B/x.pdf", Some(doc)),
            LinkedFile::Ask(Path::new(r"C:\Jobs\A").join(r"..\B\x.pdf"))
        );
        assert!(matches!(
            linked_file("C:x.pdf", Some(doc)),
            LinkedFile::Refused(_)
        ));
        assert!(matches!(linked_file("x.pdf", None), LinkedFile::Refused(_)));
    }

    #[test]
    fn hosts_of_web_addresses() {
        assert_eq!(url_host("https://example.org/a?b"), "example.org");
        assert_eq!(
            url_host("https://user@example.org:8080#x"),
            "example.org:8080"
        );
        assert_eq!(
            url_host("mailto:j\u{f6}e@example.org?subject=x"),
            "example.org"
        );
        assert!(!url_host("https://b\u{fc}cher.example/\u{e9}").is_ascii());
        assert!(url_host("https://xn--bcher-kva.example/\u{e9}").is_ascii());
    }
}
