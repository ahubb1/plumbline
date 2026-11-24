//! What a tool is told: pointer and key events in page space.

use pl_geom::{Point, Scale, Unit};

/// The modifier keys held during an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    /// Constrain: a square or circle, 45 degree steps, one axis of movement, kept proportions.
    pub shift: bool,
    /// Rectangle and Ellipse: draw from the centre. Select: a move leaves a copy behind.
    /// Arrow keys: a fine nudge.
    pub ctrl: bool,
    /// Ignore the snap point for this event.
    pub alt: bool,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: false,
    };
    pub const SHIFT: Modifiers = Modifiers {
        shift: true,
        ctrl: false,
        alt: false,
    };
    pub const CTRL: Modifiers = Modifiers {
        shift: false,
        ctrl: true,
        alt: false,
    };
    pub const ALT: Modifiers = Modifiers {
        shift: false,
        ctrl: false,
        alt: true,
    };
}

/// Where the pointer is.
///
/// `point` is in the unrotated page space of page `page`. While a gesture is in progress the
/// caller keeps reporting positions on the page the gesture started on (pointer capture); an
/// event that names another page is ignored until the gesture ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    pub page: usize,
    pub point: Point,
    pub modifiers: Modifiers,
    /// A snap target near the pointer (an endpoint, a midpoint, a grid point), found by the
    /// caller. When present, and Alt is not held, it is used in place of `point`. Freehand
    /// tools ignore it.
    pub snap: Option<Point>,
}

impl Pointer {
    pub fn new(page: usize, point: Point) -> Pointer {
        Pointer {
            page,
            point,
            modifiers: Modifiers::NONE,
            snap: None,
        }
    }

    pub fn with_modifiers(mut self, modifiers: Modifiers) -> Pointer {
        self.modifiers = modifiers;
        self
    }

    pub fn with_snap(mut self, snap: Point) -> Pointer {
        self.snap = Some(snap);
        self
    }

    /// The snap point unless Alt is held or there is none; otherwise the pointer position.
    pub fn effective(&self) -> Point {
        match self.snap {
            Some(s) if !self.modifiers.alt && s.x.is_finite() && s.y.is_finite() => s,
            _ => self.point,
        }
    }

    pub(crate) fn is_finite(&self) -> bool {
        self.point.x.is_finite() && self.point.y.is_finite()
    }
}

/// The keys tools react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// Cancels the gesture in progress; with none, clears the selection.
    Escape,
    /// Finishes a polyline, polygon or cloud; opens the text of the selected markup.
    Enter,
    /// Deletes the selection.
    Delete,
    /// Removes the last point of a polyline, polygon or cloud in progress; otherwise as Delete.
    Backspace,
    /// Nudge the selection the way the arrow points on screen.
    Left,
    Right,
    Up,
    Down,
}

/// One event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ToolInput {
    /// The primary button went down.
    PointerDown(Pointer),
    /// The pointer moved, with the button up or down.
    PointerMove(Pointer),
    /// The primary button came up.
    PointerUp(Pointer),
    /// The second click of a double-click. It may be sent as well as that click's
    /// `PointerDown` and `PointerUp`, before or after them, or in their place; tools give the
    /// same result either way.
    DoubleClick(Pointer),
    Key {
        key: Key,
        modifiers: Modifiers,
    },
}

impl ToolInput {
    pub fn key(key: Key) -> ToolInput {
        ToolInput::Key {
            key,
            modifiers: Modifiers::NONE,
        }
    }
}

/// What the application's text editor tells a tool that asked for text.
///
/// The text tools (Text Box, Typewriter, Callout, Cloud Callout) end their pointer gesture by
/// asking for the text with [`Intent::RequestTextEdit`](crate::Intent::RequestTextEdit) and
/// then wait: nothing exists in the document yet. The answer goes to
/// [`step_text`](crate::step_text), not [`step`](crate::step), because it carries a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TextInput {
    /// The user finished typing: create the markup with this text. Empty text (or white space
    /// only) creates no text markup.
    Commit(String),
    /// The user abandoned the editor: nothing is created.
    Cancel,
}

/// What the application tells a measurement tool that asked for a value with
/// [`Intent::RequestMeasure`](crate::Intent::RequestMeasure). The answer goes to
/// [`step_measure`](crate::step_measure).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeasureInput {
    /// Calibrate: the two points are `metres` apart in the world. Measurements taken at the
    /// new scale are shown in `unit` (unless the tool's preset names units of its own).
    Distance { metres: f64, unit: Unit },
    /// Viewport: the region's scale.
    Scale(Scale),
    /// Volume: the depth, in metres.
    Depth(f64),
    /// The user declined: nothing is created or changed.
    Cancel,
}
