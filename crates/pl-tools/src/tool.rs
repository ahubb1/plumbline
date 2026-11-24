//! The tool state and the one function that advances it.

use std::sync::Arc;

use pl_annot::{AnnotId, Annotation, HandleKind, RectHandle, RgbaImage};
use pl_draw::DrawList;
use pl_geom::{Point, Rect, Vec2};

use crate::arrange::Shape;
use crate::input::{MeasureInput, Pointer, TextInput, ToolInput};
use crate::intent::{Intent, MeasureRequest};
use crate::measure::{Gesture, Readout};
use crate::measure_edit::MeasureGrip;
use crate::preset::{ToolKind, ToolPreset};
use crate::snap::SnapKinds;
use crate::view::DocView;
use crate::{draw, measure, place, select, text};

/// How near the pointer must be to a markup to pick it, in screen pixels.
pub const HIT_PX: f64 = 4.0;
/// How near the pointer must be to a handle to grab it, in screen pixels.
pub const HANDLE_PX: f64 = 6.0;
/// A press that travels less than this, in screen pixels, is a click and not a drag: it draws
/// no shape and moves nothing.
pub const DRAG_PX: f64 = 3.0;
/// Clicking this near the first point closes a polygon or cloud, in screen pixels. A click
/// this near the last point adds no second point there (it is the first half of a
/// double-click).
pub const CLOSE_PX: f64 = 6.0;
/// Freehand points nearer than this to the last one kept are dropped as they arrive, in
/// screen pixels.
pub const THIN_PX: f64 = 1.5;
/// A finished freehand stroke is simplified to within this, in screen pixels.
pub const SIMPLIFY_PX: f64 = 0.5;
/// The side of a new note icon, in points (divided by the page's `/UserUnit`).
pub const NOTE_SIZE: f64 = 18.0;
/// The width and height of the text box a click places, in points as seen on screen (divided
/// by the page's `/UserUnit`). A text box grows downwards to hold what is typed; a typewriter
/// line is fitted to it.
pub const TEXT_BOX_SIZE: (f64, f64) = (144.0, 54.0);
/// The width and height of the box a click places for a callout's text.
pub const CALLOUT_BOX_SIZE: (f64, f64) = (120.0, 40.0);
/// A picture is placed at this many points per pixel (96 pixels to the inch) unless that
/// would cover more than [`IMAGE_PAGE_SHARE`] of the page's width or height.
pub const IMAGE_POINTS_PER_PIXEL: f64 = 0.75;
/// The most of the page's displayed width or height a picture placed by a click may cover.
pub const IMAGE_PAGE_SHARE: f64 = 0.5;
/// The eraser rubs out a pen stroke that comes this near its path, in screen pixels (plus
/// half the stroke's width).
pub const ERASER_PX: f64 = 6.0;
/// An arrow key moves the selection this far, in page units.
pub const NUDGE: f64 = 1.0;
/// With Shift.
pub const NUDGE_LARGE: f64 = 10.0;
/// With Ctrl.
pub const NUDGE_FINE: f64 = 0.25;

/// The press-and-drag that draws the text box of a callout.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BoxDrag {
    /// Where the pointer itself went down.
    pub press: Point,
    pub anchor: Point,
    pub current: Point,
    pub moved: bool,
}

/// How a text markup's box follows the text typed into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fit {
    /// The box stays as drawn.
    Exact,
    /// The box keeps its width and grows down the screen when the text needs more height.
    GrowHeight,
    /// The box keeps its width and takes the height of the text.
    Height,
    /// The box takes the width and the height of the text (no line is wrapped).
    Both,
}

/// The gesture in progress.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Phase {
    Idle,
    /// Rectangle, Ellipse, Cloud Box, Line, Arrow, Text Box, Typewriter, Stamp, Image, the
    /// text markup tools and the cloud of a Cloud Callout: the button is down.
    Drag {
        page: usize,
        /// Where the pointer itself went down (the anchor may be a snap point elsewhere).
        press: Point,
        anchor: Point,
        current: Point,
        /// Rectangle and Ellipse: drawn from the centre (Ctrl). Stamp and Image: the
        /// proportions are free (Shift).
        centred: bool,
        /// The pointer itself has travelled at least [`DRAG_PX`] from the press.
        moved: bool,
    },
    /// Callout, Cloud Callout: the leader's start is placed and the text box is next.
    Leader {
        page: usize,
        /// Callout: the tip, then the knee if one was clicked. Cloud Callout: empty (its
        /// leader starts on the cloud, wherever the box ends up).
        points: Vec<Point>,
        /// Cloud Callout: the cloud's rectangle.
        cloud: Option<Rect>,
        /// Where the pointer is while the button is up.
        hover: Option<Point>,
        /// The button is down: the text box being dragged, or a click.
        drag: Option<BoxDrag>,
    },
    /// The text tools: the box is placed and the application's editor has been asked for the
    /// text. Nothing is in the document yet.
    TextEntry {
        page: usize,
        /// The markup as it will be created, with no text yet.
        draft: Box<Annotation>,
        /// Cloud Callout: the cloud that is created with it.
        companion: Option<Box<Annotation>>,
        fit: Fit,
    },
    /// Polyline, Polygon, Cloud: points placed so far, and where the next one would go.
    Path {
        page: usize,
        points: Vec<Point>,
        cursor: Option<Point>,
    },
    /// Pen, Highlighter: the button is down.
    Stroke {
        page: usize,
        points: Vec<Point>,
        end: Point,
        straight: bool,
    },
    /// Select: the button went down on a markup; a drag moves the selection.
    Move {
        page: usize,
        /// Where the pointer itself went down; `start` is that or the snap point there.
        press: Point,
        start: Point,
        delta: Vec2,
        ids: Vec<AnnotId>,
        clicked: AnnotId,
        /// The click landed on one of several selected markups: if it turns out not to be a
        /// drag, that one becomes the selection.
        collapse: bool,
        moved: bool,
        copy: bool,
    },
    /// Select: the button went down on empty paper; a drag is a selection box.
    Marquee {
        page: usize,
        anchor: Point,
        current: Point,
        additive: bool,
    },
    /// Select: a handle of the selected markup is being dragged.
    Handle {
        page: usize,
        id: AnnotId,
        handle: HandleKind,
        start: Point,
        shape: Shape,
        moved: bool,
    },
    /// Select: a handle of the box around several selected markups is being dragged.
    Group {
        page: usize,
        ids: Vec<AnnotId>,
        handle: RectHandle,
        start: Point,
        /// The box around the markups when the drag began, and where it has been dragged to.
        from: Rect,
        to: Rect,
        moved: bool,
    },
    /// The measurement tools: the points placed so far, a first drag, or a wait for the
    /// user's answer.
    Measure(Box<Gesture>),
    /// Select: the dimension line, the caption or a cutout corner of the selected
    /// measurement is being dragged.
    Grip {
        page: usize,
        id: AnnotId,
        grip: MeasureGrip,
        start: Point,
        /// The measurement as the drag has made it so far.
        measure: Box<pl_annot::Measurement>,
        moved: bool,
    },
}

/// A tool, the style it draws with, and its gesture in progress.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolState {
    pub(crate) preset: ToolPreset,
    pub(crate) phase: Phase,
    /// The picture the Image tool places.
    pub(crate) image: Option<Arc<RgbaImage>>,
}

impl ToolState {
    /// The tool `preset.tool`, idle, drawing with `preset`.
    pub fn new(preset: ToolPreset) -> ToolState {
        ToolState {
            preset,
            phase: Phase::Idle,
            image: None,
        }
    }

    /// The same tool holding the picture the Image tool places. Without one the Image tool
    /// does nothing.
    pub fn with_image(mut self, image: Arc<RgbaImage>) -> ToolState {
        self.image = Some(image);
        self
    }

    /// The picture the Image tool places.
    pub fn image(&self) -> Option<&Arc<RgbaImage>> {
        self.image.as_ref()
    }

    /// `tool`, idle, with its built-in style.
    pub fn for_tool(tool: ToolKind) -> ToolState {
        ToolState::new(ToolPreset::default_for(tool))
    }

    pub fn tool(&self) -> ToolKind {
        self.preset.tool
    }

    pub fn preset(&self) -> &ToolPreset {
        &self.preset
    }

    /// The same tool with another style. The gesture in progress carries on (the style is
    /// read when the markup is finished) unless the preset is for a different tool.
    pub fn with_preset(self, preset: ToolPreset) -> ToolState {
        let phase = if preset.tool == self.preset.tool {
            self.phase
        } else {
            Phase::Idle
        };
        ToolState {
            preset,
            phase,
            image: self.image,
        }
    }

    /// True while a gesture is in progress (a button is down, or a polyline is part drawn).
    pub fn is_busy(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// The tool with its gesture abandoned; nothing is created or changed.
    pub fn cancelled(self) -> ToolState {
        ToolState {
            preset: self.preset,
            phase: Phase::Idle,
            image: self.image,
        }
    }

    /// The text markup a text tool is waiting for the text of: placed, styled, and not yet in
    /// the document. Its id is the one [`Intent::RequestTextEdit`] named; its rectangle,
    /// rotation and `free_text` say where and how the editor should show the text. Answer
    /// with [`step_text`].
    pub fn text_draft(&self) -> Option<&Annotation> {
        match &self.phase {
            Phase::TextEntry { draft, .. } => Some(draft),
            _ => None,
        }
    }

    /// The page the gesture in progress is on.
    pub fn page(&self) -> Option<usize> {
        match &self.phase {
            Phase::Idle => None,
            Phase::Drag { page, .. }
            | Phase::Leader { page, .. }
            | Phase::TextEntry { page, .. }
            | Phase::Group { page, .. }
            | Phase::Path { page, .. }
            | Phase::Stroke { page, .. }
            | Phase::Move { page, .. }
            | Phase::Marquee { page, .. }
            | Phase::Grip { page, .. }
            | Phase::Handle { page, .. } => Some(*page),
            Phase::Measure(g) => Some(g.page),
        }
    }

    /// The annotations the preview shows in a new place or shape right now. The application
    /// can leave their originals undrawn meanwhile. Empty while a move is leaving a copy.
    pub fn editing(&self) -> Vec<AnnotId> {
        match &self.phase {
            Phase::Move {
                ids,
                moved: true,
                copy: false,
                ..
            } => ids.clone(),
            Phase::Handle {
                id, moved: true, ..
            } => vec![id.clone()],
            Phase::Group {
                ids, moved: true, ..
            } => ids.clone(),
            Phase::Grip {
                id, moved: true, ..
            } => vec![id.clone()],
            _ => Vec::new(),
        }
    }

    /// What the gesture in progress looks like, in page space: the markup being drawn exactly
    /// as it would be created, the moved or reshaped markups, or the selection box.
    pub fn preview(&self, view: &dyn DocView) -> DrawList {
        match self.preset.tool {
            ToolKind::Select => select::preview(self, view),
            ToolKind::TextBox
            | ToolKind::Typewriter
            | ToolKind::Callout
            | ToolKind::CloudCallout => text::preview(self, view),
            ToolKind::Stamp | ToolKind::Image => place::stamp_preview(self, view),
            ToolKind::TextHighlight
            | ToolKind::TextUnderline
            | ToolKind::TextStrikeOut
            | ToolKind::TextSquiggly => place::markup_preview(self, view),
            ToolKind::Eraser => DrawList::new(),
            t if t.is_measure() => measure::preview(self, view),
            _ => draw::preview(self, view),
        }
    }

    /// What the measurement in progress amounts to so far, through the scale it will be taken
    /// at: for a live tooltip. `None` for a tool that is not a measurement tool, or when
    /// nothing is in progress (Count: when no count is being made).
    pub fn readout(&self, view: &dyn DocView) -> Option<Readout> {
        measure::readout(self, view)
    }

    /// The kinds of snap point to offer this tool ([`ToolKind::snap_kinds`]).
    pub fn snap_kinds(&self) -> SnapKinds {
        self.preset.tool.snap_kinds()
    }

    /// What a measurement tool has asked the user for and is waiting for
    /// ([`Intent::RequestMeasure`]); answer with [`step_measure`].
    pub fn measure_request(&self) -> Option<&MeasureRequest> {
        match &self.phase {
            Phase::Measure(g) => g.waiting.as_ref(),
            _ => None,
        }
    }
}

/// True for a pointer event a tool can use: finite, and on a page that exists.
pub(crate) fn usable(view: &dyn DocView, pointer: &Pointer) -> bool {
    pointer.is_finite() && view.page(pointer.page).is_some()
}

/// Advances a tool by one input.
///
/// Returns the new state, what the tool wants done, and the preview of the gesture in
/// progress (empty when there is none). The function is pure: it reads `view` and changes
/// nothing.
pub fn step(
    mut state: ToolState,
    input: &ToolInput,
    view: &dyn DocView,
) -> (ToolState, Vec<Intent>, DrawList) {
    let mut intents = Vec::new();
    match state.preset.tool {
        ToolKind::Select => select::step(&mut state, input, view, &mut intents),
        ToolKind::Rectangle
        | ToolKind::Ellipse
        | ToolKind::CloudBox
        | ToolKind::Line
        | ToolKind::Arrow => draw::drag_step(&mut state, input, view, &mut intents),
        ToolKind::TextBox | ToolKind::Typewriter => {
            text::box_step(&mut state, input, view, &mut intents)
        }
        ToolKind::Callout | ToolKind::CloudCallout => {
            text::callout_step(&mut state, input, view, &mut intents)
        }
        ToolKind::Stamp | ToolKind::Image => {
            place::stamp_step(&mut state, input, view, &mut intents)
        }
        ToolKind::TextHighlight
        | ToolKind::TextUnderline
        | ToolKind::TextStrikeOut
        | ToolKind::TextSquiggly => place::markup_step(&mut state, input, view, &mut intents),
        ToolKind::Eraser => place::eraser_step(&mut state, input, view, &mut intents),
        ToolKind::Polyline | ToolKind::Polygon | ToolKind::Cloud => {
            draw::path_step(&mut state, input, view, &mut intents)
        }
        ToolKind::Pen | ToolKind::Highlighter => {
            draw::stroke_step(&mut state, input, view, &mut intents)
        }
        ToolKind::Note => draw::note_step(&mut state, input, view, &mut intents),
        ToolKind::Calibrate
        | ToolKind::Viewport
        | ToolKind::Length
        | ToolKind::Polylength
        | ToolKind::Perimeter
        | ToolKind::Area
        | ToolKind::Cutout
        | ToolKind::Volume
        | ToolKind::Angle
        | ToolKind::Radius
        | ToolKind::Diameter
        | ToolKind::Count => measure::step(&mut state, input, view, &mut intents),
    }
    let preview = state.preview(view);
    (state, intents, preview)
}

/// Gives a text tool the answer to its [`Intent::RequestTextEdit`]: the typed text, or word
/// that the editor was abandoned.
///
/// * [`TextInput::Commit`] with text creates the markup ([`ToolState::text_draft`] with the
///   text in it) and selects it. A box placed by a click is fitted to the text first: a text
///   box grows down the screen, a typewriter line takes the width and height of its text.
///   Cloud Callout creates its cloud as well.
/// * Text that cannot be shown (a character outside WinAnsi; `Annotation::validate` on the
///   draft with the text names it) creates nothing: the tool keeps waiting and asks for the
///   editor again, so the application can say what is wrong.
/// * Empty text, or [`TextInput::Cancel`], creates no text markup. With empty text a Cloud
///   Callout still creates its cloud.
///
/// A tool that is not waiting for text ignores the input.
pub fn step_text(
    mut state: ToolState,
    input: &TextInput,
    view: &dyn DocView,
) -> (ToolState, Vec<Intent>, DrawList) {
    let mut intents = Vec::new();
    text::commit(&mut state, input, view, &mut intents);
    let preview = state.preview(view);
    (state, intents, preview)
}

/// Gives a measurement tool the answer to its [`Intent::RequestMeasure`].
///
/// * Calibrate: [`MeasureInput::Distance`] sets the scale at which the two picked points are
///   that far apart ([`Intent::SetScale`]): for the page, for the viewport holding both
///   points, or for a new viewport over the region dragged first. Values are shown in the
///   unit given (feet and inches to a sixteenth for `Unit::FeetInches`) unless the preset
///   names its own. A distance that makes no scale (zero, negative, not a number) changes
///   nothing and the tool keeps waiting.
/// * Viewport: [`MeasureInput::Scale`] adds the dragged region as a viewport at that scale.
/// * Volume: [`MeasureInput::Depth`] (metres, above zero) creates the volume.
/// * [`MeasureInput::Cancel`] abandons the gesture.
///
/// An answer of the wrong kind, or to a tool that is not waiting, is ignored.
pub fn step_measure(
    mut state: ToolState,
    input: &MeasureInput,
    view: &dyn DocView,
) -> (ToolState, Vec<Intent>, DrawList) {
    let mut intents = Vec::new();
    measure::answer(&mut state, input, view, &mut intents);
    let preview = state.preview(view);
    (state, intents, preview)
}
