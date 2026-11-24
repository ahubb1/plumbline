//! The read-only view of a document that tools work against.

use pl_annot::{AnnotId, Annotation, Origin, PageViewports, Quad, ScaleRef, Viewport};
use pl_geom::{PageGeometry, Point, Rotation};

use crate::intent::{AnnotChange, Intent, MeasureRequest, ScaleTarget};

/// What a tool may know about the document. `pl-doc` implements this for its session; tests
/// and simple callers can use [`SimpleView`].
pub trait DocView {
    /// The geometry of a page **as displayed**: its `rotate()` is the page's `/Rotate` plus
    /// any view rotation (`PageGeometry::rotated_by`). `None` for a page that does not exist;
    /// tools ignore pointer events there.
    fn page(&self, page: usize) -> Option<PageGeometry>;

    /// The annotations of a page in stacking order, bottom to top (the `/Annots` order).
    fn annotations(&self, page: usize) -> Vec<&Annotation>;

    fn annotation(&self, id: &AnnotId) -> Option<&Annotation>;

    /// The selected annotations, in the order they were selected.
    fn selection(&self) -> &[AnnotId];

    /// The size of one screen pixel in page units at the current zoom (for a `/UserUnit` page,
    /// in that page's units). Hit distances and drag thresholds are multiples of it.
    fn pixel_size(&self) -> f64;

    /// The author and time a new markup is stamped with.
    fn origin(&self) -> Origin;

    /// The rotation page `page` is displayed at; `R0` for a page that does not exist.
    fn rotation(&self, page: usize) -> Rotation {
        self.page(page).map_or(Rotation::R0, |g| g.rotate())
    }

    /// The text a drag from `from` to `to` selects on a page, for the text markup tools: one
    /// quad per character (or per word), in reading order, each starting at the start of its
    /// baseline (`pl_annot::quad_for_box` makes one from a character box and the direction
    /// the text reads in). The tools merge them into one quad per line. A view with no text
    /// to offer returns nothing, and the text markup tools then create nothing.
    fn text_quads(&self, _page: usize, _from: Point, _to: Point) -> Vec<Quad> {
        Vec::new()
    }

    /// The scales of a page: its own and its viewports (`/VP`). The measurement tools take a
    /// new measurement at the scale [`PageViewports::scale_for`] gives for its points. A view
    /// that offers none (the default) makes every measurement at full size, 1:1, until a
    /// scale is set.
    fn viewports(&self, _page: usize) -> Option<&PageViewports> {
        None
    }
}

/// A [`DocView`] over plain vectors, for tests and for callers with no session.
///
/// [`SimpleView::apply`] carries out intents on it, so a scripted sequence of inputs can be
/// played end to end.
#[derive(Debug, Clone, PartialEq)]
pub struct SimpleView {
    pub pages: Vec<PageGeometry>,
    /// All annotations, bottom to top within each page.
    pub annotations: Vec<Annotation>,
    pub selection: Vec<AnnotId>,
    pub pixel_size: f64,
    pub origin: Origin,
    /// The last cursor a tool asked for.
    pub cursor: Option<crate::Cursor>,
    /// The last annotation a tool asked to open the text editor for.
    pub text_edit: Option<AnnotId>,
    /// The characters of the pages, as `(page, quad)` in reading order: what
    /// [`DocView::text_quads`] selects from.
    pub text: Vec<(usize, Quad)>,
    /// The scales of the pages that have any.
    pub viewports: Vec<PageViewports>,
    /// The last value a measurement tool asked the user for.
    pub request: Option<MeasureRequest>,
}

impl SimpleView {
    /// A view of `pages` with no annotations, at one page unit per pixel.
    pub fn new(pages: Vec<PageGeometry>, origin: Origin) -> SimpleView {
        SimpleView {
            pages,
            annotations: Vec::new(),
            selection: Vec::new(),
            pixel_size: 1.0,
            origin,
            cursor: None,
            text_edit: None,
            text: Vec::new(),
            viewports: Vec::new(),
            request: None,
        }
    }

    /// Carries out intents the way a document would: creates go on top, a delete also leaves
    /// the selection, a selection keeps only ids that exist.
    pub fn apply(&mut self, intents: &[Intent]) {
        for intent in intents {
            match intent {
                Intent::Create(a) => {
                    if !self.annotations.iter().any(|b| b.id == a.id) {
                        self.annotations.push((**a).clone());
                    }
                }
                Intent::Modify { id, change } => {
                    if let Some(a) = self.annotations.iter_mut().find(|a| a.id == *id) {
                        if change.apply(a) {
                            a.touch(self.origin.when);
                        }
                    }
                }
                Intent::Delete(ids) => {
                    self.annotations.retain(|a| !ids.contains(&a.id));
                    self.selection.retain(|id| !ids.contains(id));
                }
                Intent::Select(ids) => {
                    self.selection = ids
                        .iter()
                        .filter(|id| self.annotations.iter().any(|a| a.id == **id))
                        .cloned()
                        .collect();
                }
                Intent::Reorder { page, order } => {
                    let mut on_page: Vec<Annotation> = Vec::new();
                    let mut slots = Vec::new();
                    for (i, a) in self.annotations.iter().enumerate() {
                        if a.page == *page {
                            slots.push(i);
                            on_page.push(a.clone());
                        }
                    }
                    let rank = |a: &Annotation| {
                        order
                            .iter()
                            .position(|id| *id == a.id)
                            .unwrap_or(usize::MAX)
                    };
                    on_page.sort_by_key(rank);
                    for (slot, a) in slots.into_iter().zip(on_page) {
                        self.annotations[slot] = a;
                    }
                }
                Intent::SetCursor(c) => self.cursor = Some(*c),
                Intent::RequestTextEdit(id) => self.text_edit = Some(id.clone()),
                Intent::RequestMeasure(r) => self.request = Some(r.clone()),
                Intent::SetScale(change) => self.set_scale(change),
            }
        }
    }

    /// Sets a scale the way a document would: changes the page's viewports, then brings every
    /// measurement of the page that was taken at that scale up to date.
    fn set_scale(&mut self, change: &crate::intent::ScaleChange) {
        let Some(geometry) = self.pages.get(change.page) else {
            return;
        };
        let page = change.page;
        if !self.viewports.iter().any(|v| v.page == page) {
            self.viewports
                .push(PageViewports::new(page, geometry.media_box()));
        }
        let Some(vps) = self.viewports.iter_mut().find(|v| v.page == page) else {
            return;
        };
        let spec = Some(change.spec.clone());
        let scale = match &change.target {
            ScaleTarget::Page => {
                vps.page_scale = spec;
                ScaleRef::Page
            }
            ScaleTarget::Viewport(name) => {
                let Some(bbox) = vps.get(name).map(|v| v.bbox) else {
                    return;
                };
                if vps.edit(name, bbox, spec).is_err() {
                    return;
                }
                ScaleRef::Viewport(name.clone())
            }
            ScaleTarget::NewViewport { name, bbox } => {
                if vps.add(Viewport::new(name.clone(), *bbox, spec)).is_err() {
                    return;
                }
                ScaleRef::Viewport(name.clone())
            }
        };
        let vps = vps.clone();
        for a in self.annotations.iter_mut().filter(|a| a.page == page) {
            if a.measure.as_ref().is_some_and(|m| m.scale == scale) && vps.refresh(a) {
                a.touch(self.origin.when);
            }
        }
    }

    /// The annotation with this id.
    pub fn get(&self, id: &AnnotId) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == *id)
    }

    /// Changes one annotation directly (a properties-panel edit, in tests).
    pub fn change(&mut self, id: &AnnotId, change: AnnotChange) -> bool {
        self.annotations
            .iter_mut()
            .find(|a| a.id == *id)
            .is_some_and(|a| change.apply(a))
    }
}

impl DocView for SimpleView {
    fn page(&self, page: usize) -> Option<PageGeometry> {
        self.pages.get(page).copied()
    }

    fn annotations(&self, page: usize) -> Vec<&Annotation> {
        self.annotations.iter().filter(|a| a.page == page).collect()
    }

    fn annotation(&self, id: &AnnotId) -> Option<&Annotation> {
        self.get(id)
    }

    fn selection(&self) -> &[AnnotId] {
        &self.selection
    }

    fn pixel_size(&self) -> f64 {
        self.pixel_size
    }

    fn viewports(&self, page: usize) -> Option<&PageViewports> {
        self.viewports.iter().find(|v| v.page == page)
    }

    fn origin(&self) -> Origin {
        self.origin.clone()
    }

    /// The characters from the one nearest `from` to the one nearest `to`, both included, in
    /// reading order whichever way the drag went. "Nearest" is by the middle of the character.
    fn text_quads(&self, page: usize, from: Point, to: Point) -> Vec<Quad> {
        let quads: Vec<Quad> = self
            .text
            .iter()
            .filter(|(p, _)| *p == page)
            .map(|(_, q)| *q)
            .collect();
        let nearest = |p: Point| {
            quads
                .iter()
                .enumerate()
                .map(|(i, q)| {
                    let c = Point::new(
                        q.iter().map(|v| v.x).sum::<f64>() / 4.0,
                        q.iter().map(|v| v.y).sum::<f64>() / 4.0,
                    );
                    ((c - p).hypot(), i)
                })
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, i)| i)
        };
        match (nearest(from), nearest(to)) {
            (Some(a), Some(b)) => quads[a.min(b)..=a.max(b)].to_vec(),
            _ => Vec::new(),
        }
    }
}
