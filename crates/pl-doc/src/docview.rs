//! [`DocView`]: the read-only view of a document that a drawing or selection tool needs.
//!
//! The plan gives tools the shape `(ToolState, ToolInput, &DocView) -> (ToolState,
//! Vec<Command>, preview DrawList)`. This crate does not depend on pl-tools, so the trait here
//! is this crate's own minimal one; the user interface implements whatever trait pl-tools asks
//! for by forwarding to it. [`DocSnapshot`] is the implementation over a [`Document`]. It
//! borrows; it never loads anything and never waits for the render backend.

use pl_annot::{AnnotId, Annotation, Handle};
use pl_geom::{PageGeometry, Point};

use crate::session::Document;

/// What a tool may ask about the document it works on. All coordinates are in unrotated page
/// user space; a tool that needs screen directions (a rotated page's "up") gets the page's
/// [`PageGeometry`] and maps them itself.
pub trait DocView {
    fn page_count(&self) -> usize;

    /// The page's real geometry; `None` while it has not been read yet.
    fn page_geometry(&self, page: usize) -> Option<PageGeometry>;

    fn annotation(&self, id: &AnnotId) -> Option<&Annotation>;

    /// The page's annotations, bottom of the stack first.
    fn annotations_on(&self, page: usize) -> Vec<&Annotation>;

    /// The annotations of `page` under `point`, topmost first. `tolerance` is in page units.
    fn hits(&self, page: usize, point: Point, tolerance: f64) -> Vec<&Annotation>;

    /// The selected annotations, in the order they were selected.
    fn selection(&self) -> &[AnnotId];

    /// The topmost annotation under `point`.
    fn hit(&self, page: usize, point: Point, tolerance: f64) -> Option<&Annotation> {
        self.hits(page, point, tolerance).into_iter().next()
    }

    fn is_selected(&self, id: &AnnotId) -> bool {
        self.selection().contains(id)
    }

    /// The selected annotations themselves.
    fn selected(&self) -> Vec<&Annotation> {
        self.selection()
            .iter()
            .filter_map(|id| self.annotation(id))
            .collect()
    }

    /// The handle of a selected annotation of `page` under `point`, with the annotation it
    /// belongs to. The annotation selected last is tried first.
    fn handle_at(
        &self,
        page: usize,
        point: Point,
        tolerance: f64,
    ) -> Option<(&Annotation, Handle)> {
        self.selection()
            .iter()
            .rev()
            .filter_map(|id| self.annotation(id))
            .filter(|a| a.page == page)
            .find_map(|a| a.handle_at(point, tolerance).map(|h| (a, h)))
    }
}

/// [`DocView`] over a [`Document`]. Before the document's annotations have been loaded
/// ([`Document::load_annots`]) it shows none.
#[derive(Clone, Copy)]
pub struct DocSnapshot<'a> {
    doc: &'a Document,
}

impl<'a> DocSnapshot<'a> {
    pub fn new(doc: &'a Document) -> DocSnapshot<'a> {
        DocSnapshot { doc }
    }
}

impl DocView for DocSnapshot<'_> {
    fn page_count(&self) -> usize {
        self.doc.page_count() as usize
    }

    fn page_geometry(&self, page: usize) -> Option<PageGeometry> {
        let page = u32::try_from(page).ok()?;
        self.doc.page_geometry(page).ok().copied()
    }

    fn annotation(&self, id: &AnnotId) -> Option<&Annotation> {
        self.doc.editor()?.set().get(id)
    }

    fn annotations_on(&self, page: usize) -> Vec<&Annotation> {
        self.doc
            .editor()
            .map(|e| e.set().on_page(page).collect())
            .unwrap_or_default()
    }

    fn hits(&self, page: usize, point: Point, tolerance: f64) -> Vec<&Annotation> {
        self.doc
            .editor()
            .map(|e| e.set().hits(page, point, tolerance))
            .unwrap_or_default()
    }

    fn selection(&self) -> &[AnnotId] {
        self.doc.editor().map_or(&[], |e| e.selection())
    }
}
