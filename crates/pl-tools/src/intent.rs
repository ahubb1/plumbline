//! What a tool asks for.

use pl_annot::{AnnotId, Annotation, FreeText, Geometry, MeasureSpec, Measurement, Style};
use pl_geom::{Point, Rect, Scale};

/// A pointer shape, named by what it means. `pl-app` maps these to its own cursors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cursor {
    Default,
    /// Placing or drawing.
    Crosshair,
    /// Over something that a drag would move.
    Move,
    /// Resize along the screen's horizontal.
    ResizeHorizontal,
    /// Resize along the screen's vertical.
    ResizeVertical,
    /// Resize along the diagonal from the screen's top-left to its bottom-right.
    ResizeNwSe,
    /// Resize along the diagonal from the screen's top-right to its bottom-left.
    ResizeNeSw,
}

/// One change to an existing annotation. Each is absolute (the new value, not a difference),
/// so applying it twice is the same as applying it once and the old value is all that is
/// needed to undo it.
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotChange {
    Geometry(Geometry),
    Style(Style),
    /// `/Contents`: the text of a note or the comment of a markup.
    Contents(String),
    /// The text side of a modelled FreeText: font, size, colour, alignment and the leader of
    /// a callout (`/CL`). A move or a resize of a callout is a `Geometry` change for its box
    /// and one of these for its leader.
    FreeText(FreeText),
    /// The measurement side of a measurement: its cutouts, leader, caption offset, depth and
    /// presentation. Its value is worked out again from its geometry.
    Measure(Box<Measurement>),
}

fn finite_measurement(m: &Measurement) -> bool {
    let ok = |v: f64| v.is_finite();
    ok(m.leader)
        && ok(m.leader_extension)
        && ok(m.leader_offset)
        && ok(m.caption_offset.x)
        && ok(m.caption_offset.y)
        && ok(m.symbol_size)
        && ok(m.text_size)
        && m.depth.is_none_or(|d| d.is_finite() && d > 0.0)
        && m.cutouts.iter().flatten().all(|p| ok(p.x) && ok(p.y))
}

impl AnnotChange {
    /// Makes the change. Returns false, changing nothing, when the annotation cannot take it:
    /// a geometry of a different shape than the annotation has (a rectangle for a line), a
    /// non-finite coordinate, a restyle of an annotation shown from its own appearance, text
    /// settings for anything but a modelled FreeText, or measurement settings for anything
    /// that is not a measurement. A measurement's `/Contents` is brought in step with its new
    /// value (`Annotation::sync_measurement`).
    pub fn apply(&self, annotation: &mut Annotation) -> bool {
        match self {
            AnnotChange::Geometry(g) => {
                if !g.is_finite()
                    || std::mem::discriminant(g) != std::mem::discriminant(&annotation.geometry)
                {
                    return false;
                }
                annotation.geometry = g.clone();
                // A measurement's value follows its shape.
                annotation.sync_measurement();
                true
            }
            AnnotChange::Style(s) => {
                if !annotation.is_modelled() {
                    return false;
                }
                annotation.style = s.clone();
                true
            }
            AnnotChange::Contents(text) => {
                annotation.meta.contents = Some(text.clone());
                true
            }
            AnnotChange::FreeText(text) => {
                let usable = text.size.is_finite()
                    && text.line_spacing.is_finite()
                    && text.padding.is_finite()
                    && text
                        .callout
                        .iter()
                        .all(|p| p.x.is_finite() && p.y.is_finite());
                if annotation.free_text.is_none() || !usable {
                    return false;
                }
                annotation.free_text = Some(text.clone());
                true
            }
            AnnotChange::Measure(m) => {
                if annotation.measure.is_none() || !finite_measurement(m) {
                    return false;
                }
                annotation.measure = Some((**m).clone());
                annotation.sync_measurement();
                true
            }
        }
    }
}

/// What a tool wants done. `pl-doc` maps each to a command; a tool never edits anything.
///
/// Intents from one [`step`](crate::step) belong together: `pl-doc` should make them one undo
/// step (a move of five markups is five `Modify` intents).
#[derive(Debug, Clone, PartialEq)]
pub enum Intent {
    /// Add this annotation on top of its page.
    Create(Box<Annotation>),
    Modify {
        id: AnnotId,
        change: AnnotChange,
    },
    Delete(Vec<AnnotId>),
    /// Make exactly these the selection (empty clears it).
    Select(Vec<AnnotId>),
    /// Restack one page: `order` lists all of that page's annotations, bottom to top.
    Reorder {
        page: usize,
        order: Vec<AnnotId>,
    },
    SetCursor(Cursor),
    /// Open the text editor for this annotation's contents.
    RequestTextEdit(AnnotId),
    /// Give a page, one of its viewports, or a new viewport this scale (Calibrate, Viewport).
    /// The application changes the page's `/VP` (`pl_annot::PageViewports`) and brings the
    /// measurements taken at that scale up to date (`PageViewports::refresh`).
    SetScale(Box<ScaleChange>),
    /// The tool needs a value only the user can give: answer with
    /// [`step_measure`](crate::step_measure).
    RequestMeasure(MeasureRequest),
}

/// Which scale of a page a [`ScaleChange`] sets.
#[derive(Debug, Clone, PartialEq)]
pub enum ScaleTarget {
    /// The page's own scale.
    Page,
    /// The existing viewport of this `/Name`.
    Viewport(String),
    /// A viewport to add, with this name and box (unrotated page space).
    NewViewport { name: String, bbox: Rect },
}

/// A scale for a page or a region of it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScaleChange {
    pub page: usize,
    pub target: ScaleTarget,
    /// The drawing scale as the user would read it.
    pub scale: Scale,
    /// The measure dictionary for it on this page (its `/UserUnit` built in), formatted as
    /// the tool's preset says.
    pub spec: MeasureSpec,
}

/// What a measurement tool asks the user for.
#[derive(Debug, Clone, PartialEq)]
pub enum MeasureRequest {
    /// Calibrate: the real distance between the two picked points `a` and `b`. Answer with
    /// [`MeasureInput::Distance`](crate::MeasureInput::Distance).
    Distance {
        page: usize,
        a: Point,
        b: Point,
        /// Which scale the answer will set.
        target: ScaleTarget,
    },
    /// Viewport: the scale of the dragged region. Answer with
    /// [`MeasureInput::Scale`](crate::MeasureInput::Scale).
    Scale {
        page: usize,
        name: String,
        bbox: Rect,
    },
    /// Volume: the depth of the outlined area. Answer with
    /// [`MeasureInput::Depth`](crate::MeasureInput::Depth).
    Depth {
        page: usize,
        /// The area outlined, in square metres, when the scale's unit is known.
        area: Option<f64>,
    },
    /// A measurement was finished over points no scale covers, and was not made: the page
    /// needs a scale there (set one or calibrate, then measure again). Nothing answers it; the
    /// tool is idle again.
    NoScale { page: usize },
}
