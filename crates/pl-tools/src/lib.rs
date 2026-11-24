//! pl-tools: the markup tools as pure state machines (see docs/plan.md sections 6 and 7).
//!
//! One function drives every tool:
//!
//! ```text
//! step(ToolState, &ToolInput, &dyn DocView) -> (ToolState, Vec<Intent>, DrawList)
//! ```
//!
//! * [`ToolState`] is the tool, its [`ToolPreset`] (the style new markups get) and whatever
//!   gesture is in progress. It is plain data: clone it, compare it, drop it.
//! * [`ToolInput`] is one pointer or key event, already in **unrotated page space**.
//! * [`DocView`] is the read-only view of the document a tool needs: page geometry (with the
//!   rotation the page is *displayed* at), the annotations, the selection and the size of a
//!   screen pixel in page units.
//! * [`Intent`] is what the tool wants done. Tools never change a document; `pl-doc` turns
//!   intents into undoable commands.
//! * The returned [`DrawList`](pl_draw::DrawList) is the in-progress preview, in page space.
//!
//! Nothing here depends on egui, on `pl-doc`, or on a clock: the author and time of a new
//! markup come from [`DocView::origin`], so the same inputs always give the same output
//! (apart from the generated `/NM` id of a new markup).
//!
//! # Coordinates and rotation
//!
//! All points are in unrotated page user space, y up (pl-geom's convention). The page's
//! rotation matters only where the user's sense of direction does: arrow-key nudges, align
//! and distribute, the direction a selection box is dragged in, which way a resize cursor
//! points, and how ties are broken when Shift constrains a drag. Those go through
//! [`to_display`] and [`to_page`].
//!
//! # Gestures
//!
//! | Tool | Gesture |
//! |---|---|
//! | Select | click; Shift-click adds or removes; drag a box (left to right takes what it encloses, right to left what it touches); drag to move (Shift: one axis; Ctrl: leave a copy); drag one of 8 handles (Shift: keep proportions; a stamp or picture keeps them unless Shift); with several selected, drag one of the 8 handles of the box around them to resize them together; drag a vertex (Shift: 45 degree steps); drag the tip or the knee of a callout's leader; double-click or Enter opens the text; arrows nudge; Delete; Esc |
//! | Rectangle, Ellipse, Cloud Box | drag (Shift: square or circle; Ctrl: from the centre) |
//! | Line, Arrow | drag (Shift: 45 degree steps) |
//! | Polyline, Polygon, Cloud | click each point (Shift: 45 degree steps); double-click or Enter finishes; clicking the first point closes a polygon or cloud; Backspace removes the last point; Esc cancels |
//! | Pen, Highlighter | drag freehand (Shift: a straight line in 45 degree steps) |
//! | Note | click |
//! | Text Box | drag the box, or click for one of [`TEXT_BOX_SIZE`]; then type |
//! | Typewriter | click where the text starts (the box is fitted to what is typed), or drag a box for the width to wrap at; then type |
//! | Callout | click what it points at; optionally click a knee (Shift: 45 degree steps); drag the text box, or click (with a knee placed) or double-click for one of [`CALLOUT_BOX_SIZE`]; then type. Backspace takes back the knee, then the tip |
//! | Cloud Callout | drag the cloud; drag the text box (or click for the default one); then type |
//! | Stamp, Image | click to place at the natural size; drag to size it (proportions kept unless Shift) |
//! | Highlight, Underline, Strikeout, Squiggly | drag across text; the view supplies the characters ([`DocView::text_quads`]) |
//! | Eraser | drag across pen strokes |
//! | Measurement tools | see below |
//!
//! # Text
//!
//! The text tools do not create anything when the pointer gesture ends. They ask for the text
//! with [`Intent::RequestTextEdit`], naming a markup that is not in the document yet
//! ([`ToolState::text_draft`] is that markup: where it is, how it is styled and which way up
//! it reads), and wait. The application's editor answers with [`step_text`]:
//! [`TextInput::Commit`] creates the markup with the text in it, as one undoable step;
//! [`TextInput::Cancel`], Esc, or empty text creates nothing. While waiting the tool ignores
//! the pointer. Text markups are turned to read upright on the page as it is displayed
//! (`Annotation::set_rotation` with the displayed rotation), and sizes given in points (the
//! default boxes, the font size, the padding, the natural size of a stamp) are divided by
//! the page's `/UserUnit`.
//!
//! For a markup that exists already, Select's double-click or Enter asks for its text the
//! same way; the application then makes the change itself ([`AnnotChange::Contents`]), after
//! `Annotation::make_editable` for a text box from another program.

//!
//! # Measurements
//!
//! Calibrate, Viewport, Length, Polylength, Perimeter, Area, Cutout, Volume, Angle, Radius,
//! Diameter and Count make standard PDF measurement annotations (pl-annot's `Measurement`).
//! Their gestures are in the table of the `measure` module documentation; in short, points
//! are clicked (or a first drag gives a line or a rectangle), Enter or a double-click ends an
//! open-ended one, Backspace takes back a point, Shift keeps a segment to steps of the
//! preset's [`MeasurePreset::angle_step`] as the page is displayed ([`constrain_step`]), and
//! the caller's snap point ([`Pointer::snap`]) is used unless Alt is held. Which snap kinds a
//! tool wants is [`ToolKind::snap_kinds`].
//!
//! * A new measurement is taken at the scale the view gives for its points
//!   ([`DocView::viewports`]), in the units of the tool's [`MeasurePreset`].
//! * [`ToolState::readout`] is what the measurement in progress amounts to so far (segment,
//!   total, area, angle...), through that scale, for a tooltip.
//! * What only the user can say (the real distance of a calibration, a viewport's scale, a
//!   volume's depth) is asked for with [`Intent::RequestMeasure`] and answered with
//!   [`step_measure`]. Calibrate and Viewport end with [`Intent::SetScale`]; the application
//!   changes the page's `/VP` and brings the measurements at that scale up to date.
//! * The Select tool edits a selected measurement: its vertices (a value is worked out again
//!   from the new shape), the offset of a dimension line and the place of its value
//!   ([`measure_handles`]), the corners of cutouts; a double-click on a segment inserts a
//!   vertex and one on a vertex deletes it.

mod arrange;
mod draw;
mod geom;
mod input;
mod intent;
mod measure;
mod measure_edit;
mod place;
mod preset;
mod select;
mod snap;
mod text;
mod tool;
mod view;

pub use arrange::{
    align, bring_forward, bring_to_front, distribute, nudge, reorder_intent, send_backward,
    send_to_back, Align, Axis,
};
pub use geom::{
    constrain_angle, constrain_axis, constrain_square, simplify_points, thin_points, to_display,
    to_page,
};
pub use input::{Key, MeasureInput, Modifiers, Pointer, TextInput, ToolInput};
pub use intent::{AnnotChange, Cursor, Intent, MeasureRequest, ScaleChange, ScaleTarget};
pub use measure::{Readout, Value};
pub use measure_edit::{measure_handles, MeasureGrip, MeasureHandle};
pub use preset::{MeasurePreset, PresetError, PresetLibrary, TextPreset, ToolKind, ToolPreset};
pub use select::{
    grab_handle, group_bounds, handle_cursor, resize_rect, selection_handles,
    selection_measure_handles,
};
pub use snap::{constrain_step, SnapKind, SnapKinds};
pub use tool::{
    step, step_measure, step_text, ToolState, CALLOUT_BOX_SIZE, CLOSE_PX, DRAG_PX, ERASER_PX,
    HANDLE_PX, HIT_PX, IMAGE_PAGE_SHARE, IMAGE_POINTS_PER_PIXEL, NOTE_SIZE, NUDGE, NUDGE_FINE,
    NUDGE_LARGE, SIMPLIFY_PX, TEXT_BOX_SIZE, THIN_PX,
};
pub use view::{DocView, SimpleView};
