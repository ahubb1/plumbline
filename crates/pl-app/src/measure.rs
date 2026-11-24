//! Measurement and takeoff in the window: the scale control and its panel, the dialogs that
//! answer a measurement tool (the known distance of a calibration, a viewport's scale, a
//! volume's depth), the snap marker and the read-out beside the pointer, the measurement
//! section of the properties panel, legends, spaces and the numbering of counts.
//!
//! The tools themselves are pl-tools' (pure state machines); the scales, viewports and
//! measurement commands are pl-doc's; quantities, legends and spaces are worked out by
//! pl-takeoff. This module only joins them to the window:
//!
//! * A page (or a region of it) with no scale shows **Scale not set** in the status bar, and a
//!   tool that would write a scaled value there opens the scale panel instead of measuring at
//!   the wrong scale ([`Tool::needs_scale`]).
//! * Calibrate and Viewport end with `Intent::SetScale`, which becomes one of pl-doc's scale
//!   commands (`SetPageScale`, `AddViewport`, `EditViewport`): the page's `/VP` changes and the
//!   measurements on it follow, in one undo step.
//! * A legend is a text box (a standard FreeText, so any viewer shows it) marked with its
//!   definition's id under the private key; the definition is pl-doc's [`LegendDef`] in the
//!   document settings. Whenever the markups change, the legend's text is worked out again
//!   through pl-takeoff and written in the same undo step as the change
//!   ([`AppState::exec`](crate::AppState)).
//! * A space is a polygon marked with a name ([`pl_takeoff::set_space_name`]), drawn with the
//!   Space tool, or a region the document settings define ([`SpaceDef`]).
//! * Counts can be numbered from a sequence ([`SequenceDef`] with the id [`COUNT_SEQUENCE`]):
//!   each new count gets the next label in the list column [`NUMBER_COLUMN`].

use egui::{
    Align2, Button, Color32, FontId, Id, Key, Pos2, RichText, Stroke, StrokeKind, TextEdit, Ui,
};
use pl_doc::pl_annot::{
    decode_text, encode_text, AnnotId, AnnotKind, Annotation, CountSymbol, FontStyle,
    MeasureFormat, MeasureKind, MeasureSpec, Origin, PdfDate, Rgb, ScaleRef,
};
use pl_doc::pl_geom::{
    parse_length, AreaFormat, AreaUnit, LengthFormat, Point, Precision, Rect, Scale, ScaleFamily,
    Unit,
};
use pl_doc::pl_pdf::Object;
use pl_doc::pl_takeoff::{
    self, Builtin, ColumnFilter, ColumnId, ColumnType, Condition, Filter, ListContext,
    QuantityUnits, Schema, Table, TakeoffContext, Value,
};
use pl_doc::{
    Command as DocCommand, DocSettings, DocumentId, LegendDef, ScaleSetting, SequenceDef, SnapKind,
    SpaceDef, GROUP_KEY,
};
use pl_tools::{MeasureInput, MeasureRequest, Readout, ScaleTarget};

use crate::app::{AppState, ViewId};
use crate::markup::Tool;
use crate::theme::{self, Palette};

/// The key a legend's text box carries its definition's id under, in its private data.
pub const LEGEND_KEY: &str = "Legend";
/// The subject of a legend's text box.
pub const LEGEND_SUBJECT: &str = "Legend";
/// The subject of a space drawn with the Space tool.
pub const SPACE_SUBJECT: &str = "Space";
/// The id of the document's sequence that numbers counts.
pub const COUNT_SEQUENCE: &str = "counts";
/// The list column a count's number is kept in.
pub const NUMBER_COLUMN: &str = "Number";
/// The list column the height of a wall is typed in (Wall Area is length times height).
pub const HEIGHT_COLUMN: &str = "Height";

/// Which kinds of point the pointer snaps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapToggles {
    pub endpoint: bool,
    pub midpoint: bool,
    pub intersection: bool,
    /// The closest point on a line of the drawing.
    pub nearest: bool,
    /// The points of markups and measurements already on the page.
    pub markups: bool,
}

impl Default for SnapToggles {
    fn default() -> SnapToggles {
        SnapToggles {
            endpoint: true,
            midpoint: true,
            intersection: true,
            nearest: false,
            markups: true,
        }
    }
}

/// The scale panel: the scale of one page or one viewport.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScalePanel {
    /// Zero-based.
    pub page: u32,
    /// The viewport whose scale is set; `None` for the page's own.
    pub target: Option<String>,
    /// The scale as text (`1/4 in = 1 ft`, `1:100`); a preset fills it in.
    pub text: String,
    /// The unit values are shown in; `None` for the unit the scale's real side is stated in.
    pub unit: Option<Unit>,
    pub precision: Precision,
    /// The pages the scale is applied to, as typed (one-based).
    pub first: String,
    pub last: String,
    pub error: Option<String>,
    /// Why the panel was opened by itself (a measurement on a page with no scale).
    pub prompt: Option<String>,
}

/// A dialog that answers a measurement tool's question.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RequestDialog {
    pub view: ViewId,
    pub request: MeasureRequest,
    pub text: String,
    pub unit: Unit,
    /// Calibrate: set the whole page's scale even though the points are in a viewport.
    pub to_page: bool,
    pub error: Option<String>,
    pub focus: bool,
}

/// Everything about measuring that is not tied to one view.
pub(crate) struct MeasureState {
    pub snap: SnapToggles,
    pub panel: Option<ScalePanel>,
    pub request: Option<RequestDialog>,
    /// Where the next Calibrate answer sets the scale instead of where the tool says.
    pub target_override: Option<ScaleTarget>,
    /// Text boxes of the measurement properties: (what, buffer).
    pub fields: std::collections::HashMap<&'static str, String>,
    /// The units of each document's takeoff list, once chosen (see [`list_units`]).
    pub list_units: std::cell::RefCell<std::collections::HashMap<DocumentId, QuantityUnits>>,
    next_key: u64,
}

/// The units a document's takeoff list is in: the document's own setting
/// ([`DocSettings::list_units`]); else the system most of its measurements are on, chosen the
/// first time it has any and then kept for the session. Formula columns and Wall Area heights
/// read quantities in these units, so measurements of the other system added later must not
/// change them behind the user's back.
pub(crate) fn list_units(
    memo: &std::cell::RefCell<std::collections::HashMap<DocumentId, QuantityUnits>>,
    doc_id: DocumentId,
    editor: &pl_doc::Editor,
) -> QuantityUnits {
    if let Some(units) = editor.scales().settings().list_units {
        return units;
    }
    if let Some(units) = memo.borrow().get(&doc_id) {
        return *units;
    }
    let units = QuantityUnits::for_annotations(editor.set().iter());
    let measured = editor.set().iter().any(|a| {
        a.measurement().is_some_and(|m| {
            m.spec.is_some() && !matches!(m.kind, MeasureKind::Angle | MeasureKind::Count)
        })
    });
    if measured {
        memo.borrow_mut().insert(doc_id, units);
    }
    units
}

impl MeasureState {
    pub(crate) fn new() -> MeasureState {
        MeasureState {
            snap: SnapToggles::default(),
            panel: None,
            request: None,
            target_override: None,
            fields: std::collections::HashMap::new(),
            list_units: Default::default(),
            next_key: 0,
        }
    }

    /// The undo-merging key a change is made with when legends follow it: the change and the
    /// legends' new text are one undo step (see [`AppState::exec`](crate::AppState)).
    pub(crate) fn merge_key(&mut self, drag: Option<u64>) -> u64 {
        const FRESH: u64 = 1 << 61;
        const PLAIN: u64 = 1 << 62;
        match drag {
            Some(k) if k & GROUP_KEY != 0 => k,
            Some(k) => GROUP_KEY | PLAIN | (k & (FRESH - 1)),
            None => {
                self.next_key += 1;
                GROUP_KEY | FRESH | (self.next_key & (FRESH - 1))
            }
        }
    }
}

// ---- units and precision ----------------------------------------------------------------------

/// The units values can be shown in, with their names.
pub const UNITS: [(Unit, &str); 6] = [
    (Unit::FeetInches, "Feet and inches"),
    (Unit::Foot, "Feet"),
    (Unit::Inch, "Inches"),
    (Unit::Metre, "Metres"),
    (Unit::Centimetre, "Centimetres"),
    (Unit::Millimetre, "Millimetres"),
];

pub fn unit_name(unit: Unit) -> &'static str {
    UNITS
        .iter()
        .find(|(u, _)| *u == unit)
        .map_or("Points", |(_, n)| n)
}

/// Whether a unit is shown with fractions rather than decimals.
fn fractional(unit: Unit) -> bool {
    matches!(unit, Unit::FeetInches)
}

/// The precisions offered for a unit.
pub fn precisions(unit: Unit) -> Vec<(Precision, &'static str)> {
    if fractional(unit) {
        vec![
            (Precision::Fraction(1), "1\""),
            (Precision::Fraction(2), "1/2\""),
            (Precision::Fraction(4), "1/4\""),
            (Precision::Fraction(8), "1/8\""),
            (Precision::Fraction(16), "1/16\""),
            (Precision::Fraction(32), "1/32\""),
            (Precision::Fraction(64), "1/64\""),
        ]
    } else {
        vec![
            (Precision::Decimal(0), "0"),
            (Precision::Decimal(1), "0.0"),
            (Precision::Decimal(2), "0.00"),
            (Precision::Decimal(3), "0.000"),
            (Precision::Decimal(4), "0.0000"),
        ]
    }
}

fn precision_name(unit: Unit, precision: Precision) -> String {
    precisions(unit)
        .into_iter()
        .find(|(p, _)| *p == precision)
        .map_or_else(|| format!("{precision:?}"), |(_, n)| n.to_owned())
}

/// The usual precision of a unit.
pub fn default_precision(unit: Unit) -> Precision {
    match unit {
        Unit::FeetInches => Precision::Fraction(16),
        Unit::Millimetre => Precision::Decimal(0),
        Unit::Centimetre => Precision::Decimal(1),
        _ => Precision::Decimal(2),
    }
}

/// How measurements at `scale` are shown: in `unit` (the scale's own when `None`) to
/// `precision`; areas in the matching square unit.
pub fn format_for(scale: &Scale, unit: Option<Unit>, precision: Precision) -> MeasureFormat {
    let mut format = MeasureFormat::for_scale(scale);
    let unit = unit.unwrap_or(format.length.unit);
    let precision = match (fractional(unit), precision) {
        (true, Precision::Fraction(_)) | (false, Precision::Decimal(_)) => precision,
        _ => default_precision(unit),
    };
    format.length = LengthFormat::new(unit, precision);
    let places = match precision {
        Precision::Decimal(p) => p.max(1),
        Precision::Fraction(_) => 2,
    };
    format.area = AreaFormat::new(AreaUnit::for_length(unit), places);
    format
}

// ---- reading ------------------------------------------------------------------------------------

/// The scale that applies at `points` on `page` of a loaded document, as the status bar
/// names it.
fn scale_name(scale: &ScaleRef, spec: &MeasureSpec) -> String {
    match scale {
        ScaleRef::Page => spec.ratio.clone(),
        ScaleRef::Viewport(name) => format!("{} ({name})", spec.ratio),
    }
}

/// A legend's id, from its text box.
pub fn legend_id(a: &Annotation) -> Option<String> {
    match a.app_data.as_ref()?.get(LEGEND_KEY.as_bytes()).ok()? {
        Object::String(bytes, _) => Some(decode_text(bytes)),
        _ => None,
    }
}

/// The spaces the document settings define, for pl-takeoff.
pub(crate) fn settings_spaces(settings: &DocSettings) -> Vec<pl_takeoff::Space> {
    settings
        .spaces
        .iter()
        .map(|s: &SpaceDef| {
            let outline = s.outline.iter().map(|p| Point::new(p[0], p[1])).collect();
            pl_takeoff::Space::new(&s.name, s.page, outline, Vec::new())
        })
        .collect()
}

/// The list column a legend totals, from the name pl-doc's [`LegendDef::columns`] keeps it
/// under (`length`, `area`, `wall area`...).
fn legend_quantity(def: &LegendDef) -> Option<Builtin> {
    def.columns.iter().find_map(|name| {
        Builtin::ALL
            .into_iter()
            .find(|b| b.is_quantity() && b.title().eq_ignore_ascii_case(name.trim()))
    })
}

/// What a legend shows, for pl-takeoff: the markups by subject (legends and spaces left out,
/// and those of other subjects when the definition names some), with their count or the
/// total of the quantity.
fn takeoff_legend(def: &LegendDef) -> pl_takeoff::LegendDef {
    let quantity = legend_quantity(def);
    let mut legend =
        pl_takeoff::LegendDef::new(&def.title, Builtin::Subject, quantity.map(ColumnId::from));
    let mut filter = Filter::default().with(ColumnFilter::not(
        Builtin::Subject,
        Condition::OneOf(vec![
            Value::text(LEGEND_SUBJECT),
            Value::text(SPACE_SUBJECT),
        ]),
    ));
    if !def.subjects.is_empty() {
        filter = filter.with(ColumnFilter::new(
            Builtin::Subject,
            Condition::OneOf(
                def.subjects
                    .iter()
                    .map(|s| Value::text(s.as_str()))
                    .collect(),
            ),
        ));
    }
    if let Some(q) = quantity {
        filter = filter.with(ColumnFilter::not(q, Condition::IsEmpty));
    }
    legend.filter = filter;
    legend
}

/// The size of a legend's text box for these lines, upright.
fn legend_box_size(lines: &[String], size: f64, padding: f64, spacing: f64) -> (f64, f64) {
    let width = lines
        .iter()
        .map(|l| {
            pl_doc::pl_draw::measure_text(l, FontStyle::Regular, size).map_or(0.0, |m| m.width)
        })
        .fold(0.0_f64, f64::max);
    let height = lines.len().max(1) as f64 * size * spacing;
    (width + 2.0 * padding + 6.0, height + 2.0 * padding + 3.0)
}

/// The corner of a rectangle that is the top-left one on a page shown with `/Rotate`.
fn shown_top_left(r: Rect, rotation: u16) -> Point {
    match rotation {
        90 => Point::new(r.x0, r.y0),
        180 => Point::new(r.x1, r.y0),
        270 => Point::new(r.x1, r.y1),
        _ => Point::new(r.x0, r.y1),
    }
}

/// A rectangle of the upright size `(w, h)` whose shown top-left corner is `corner`.
fn rect_from_top_left(corner: Point, w: f64, h: f64, rotation: u16) -> Rect {
    // Across the shown page is +x at 0, +y at 90, -x at 180, -y at 270 (pl-takeoff's legend
    // layout); down the shown page is a quarter turn on from that.
    let (dx, dy) = match rotation {
        90 => (h, w),
        270 => (-h, -w),
        180 => (-w, h),
        _ => (w, -h),
    };
    Rect::new(
        corner.x.min(corner.x + dx),
        corner.y.min(corner.y + dy),
        corner.x.max(corner.x + dx),
        corner.y.max(corner.y + dy),
    )
}

impl AppState {
    /// What the status bar says of the scale for a view: at the pointer, or on the page in
    /// view when the pointer is elsewhere.
    pub(crate) fn scale_text(&self, dv: &crate::app::DocView) -> String {
        let Ok(doc) = self.session.document(dv.doc) else {
            return String::new();
        };
        let Some(editor) = doc.editor() else {
            return String::new();
        };
        let (page, points) = match dv.cursor.filter(|h| h.inside) {
            Some(hit) => (hit.page, vec![hit.point]),
            None => (dv.shown_page(), Vec::new()),
        };
        match editor.scales().resolve(page as usize, &points) {
            Some((scale, spec)) => scale_name(&scale, &spec),
            None => "Scale not set".to_owned(),
        }
    }

    /// Whether a scaled value can be taken at `point` on `page`: the document's scales say
    /// what applies there.
    pub(crate) fn scale_known(&self, doc: DocumentId, page: usize, point: Point) -> bool {
        self.session
            .document(doc)
            .ok()
            .and_then(|d| d.editor())
            .is_some_and(|e| e.scales().resolve(page, &[point]).is_some())
    }

    /// The snap point kinds offered.
    pub fn snap_toggles(&self) -> SnapToggles {
        self.measure.snap
    }

    pub fn set_snap_toggles(&mut self, toggles: SnapToggles) {
        self.measure.snap = toggles;
    }

    /// The kind of point the pointer is snapped to in a view.
    pub fn snap_kind(&self, view: ViewId) -> Option<SnapKind> {
        self.views.get(&view).and_then(|dv| dv.snap_kind)
    }

    /// What the measurement in progress in a view amounts to so far.
    pub fn readout(&self, view: ViewId) -> Option<&Readout> {
        self.views.get(&view).and_then(|dv| dv.readout.as_ref())
    }

    // ---- the scale panel --------------------------------------------------------------------

    pub fn scale_panel_open(&self) -> bool {
        self.measure.panel.is_some()
    }

    /// Why the scale panel opened by itself, if it did.
    pub fn scale_prompt(&self) -> Option<&str> {
        self.measure
            .panel
            .as_ref()
            .and_then(|p| p.prompt.as_deref())
    }

    /// What the scale panel says is wrong with what was typed into it.
    pub fn scale_panel_error(&self) -> Option<&str> {
        self.measure.panel.as_ref().and_then(|p| p.error.as_deref())
    }

    /// Opens the scale panel for the page (or viewport) under the pointer, else the page in
    /// view. `prompt` says why, when the app opened it by itself.
    pub fn open_scale_panel(&mut self, prompt: Option<String>) {
        let Some(dv) = self.active_view() else {
            return;
        };
        let doc_id = dv.doc;
        let (page, point) = match dv.cursor.filter(|h| h.inside) {
            Some(hit) => (hit.page, Some(hit.point)),
            None => (dv.shown_page(), None),
        };
        self.ensure_annots(doc_id);
        let resolved = self
            .session
            .document(doc_id)
            .ok()
            .and_then(|d| d.editor())
            .and_then(|e| {
                e.scales()
                    .resolve(page as usize, &point.into_iter().collect::<Vec<_>>())
            });
        let (target, text, precision) = match &resolved {
            Some((scale, spec)) => {
                let s = self
                    .session
                    .document(doc_id)
                    .ok()
                    .map(|d| d.pages().get(page as usize).map_or(1.0, |g| g.user_unit()))
                    .and_then(|uu| spec.scale(uu));
                let target = match scale {
                    ScaleRef::Viewport(name) => Some(name.clone()),
                    ScaleRef::Page => None,
                };
                let text = s.map(|s| s.to_string()).unwrap_or_default();
                let unit = s.map(|s| MeasureFormat::for_scale(&s).length.unit);
                (
                    target,
                    text,
                    unit.map_or(Precision::Decimal(2), default_precision),
                )
            }
            None => (None, String::new(), Precision::Fraction(16)),
        };
        self.measure.panel = Some(ScalePanel {
            page,
            target,
            text,
            unit: None,
            precision,
            first: (page + 1).to_string(),
            last: (page + 1).to_string(),
            error: None,
            prompt,
        });
    }

    /// Fills the scale panel in, as typing into it does: the scale text, the pages it goes
    /// to (one-based, both included) and the unit and precision.
    pub fn set_scale_panel(
        &mut self,
        text: &str,
        pages: Option<(u32, u32)>,
        unit: Option<Unit>,
        precision: Option<Precision>,
    ) {
        let Some(panel) = self.measure.panel.as_mut() else {
            return;
        };
        panel.text = text.to_owned();
        if let Some((first, last)) = pages {
            panel.first = first.to_string();
            panel.last = last.to_string();
        }
        panel.unit = unit;
        if let Some(p) = precision {
            panel.precision = p;
        }
    }

    /// Applies what the scale panel says. Returns whether the scale was set; what is wrong
    /// is shown in the panel.
    pub fn apply_scale_panel(&mut self) -> bool {
        let Some(panel) = self.measure.panel.clone() else {
            return false;
        };
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return false;
        };
        let fail = |app: &mut AppState, message: String| {
            if let Some(p) = app.measure.panel.as_mut() {
                p.error = Some(message);
            }
            false
        };
        let scale = match Scale::parse(panel.text.trim()) {
            Ok(s) => s,
            Err(e) => return fail(self, format!("That is not a scale: {e}")),
        };
        let format = format_for(&scale, panel.unit, panel.precision);
        let setting = ScaleSetting::new(&scale, Some(&format));
        let page_count = self.session.document(doc_id).map_or(0, |d| d.page_count());
        let number = |text: &str| text.trim().parse::<u32>().ok();
        let (first, last) = match (number(&panel.first), number(&panel.last)) {
            (Some(a), Some(b)) if a >= 1 && a <= b && b <= page_count => (a - 1, b - 1),
            _ => {
                return fail(
                    self,
                    format!("Give the pages as numbers from 1 to {page_count}, first to last."),
                )
            }
        };
        let command = match &panel.target {
            Some(name) => {
                let bbox = self
                    .session
                    .document(doc_id)
                    .ok()
                    .and_then(|d| d.page_scales(panel.page))
                    .and_then(|v| v.get(name))
                    .map(|v| [v.bbox.x0, v.bbox.y0, v.bbox.x1, v.bbox.y1]);
                let Some(bbox) = bbox else {
                    return fail(self, format!("The viewport {name} is not on this page."));
                };
                DocCommand::EditViewport {
                    page: panel.page as usize,
                    name: name.clone(),
                    bbox,
                    scale: Some(setting),
                }
            }
            None if first == last => DocCommand::SetPageScale {
                page: first as usize,
                scale: Some(setting),
            },
            None => DocCommand::ApplyScaleToPages {
                first: first as usize,
                last: last as usize,
                scale: Some(setting),
            },
        };
        if !self.ensure_annots(doc_id) {
            return false;
        }
        if self.exec(doc_id, command, None) {
            self.measure.panel = None;
            self.chrome.status = Some(format!("Scale set to {scale}"));
            true
        } else {
            let message = self
                .chrome
                .status
                .clone()
                .unwrap_or_else(|| "The scale is already that.".to_owned());
            fail(self, message)
        }
    }

    /// Takes the page's own scale away (the panel's page).
    pub fn clear_page_scale(&mut self) -> bool {
        let Some(panel) = self.measure.panel.clone() else {
            return false;
        };
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return false;
        };
        let done = self.exec(
            doc_id,
            DocCommand::SetPageScale {
                page: panel.page as usize,
                scale: None,
            },
            None,
        );
        if done {
            self.measure.panel = None;
        }
        done
    }

    /// A tool that writes a scaled value was used where there is no scale: the scale panel
    /// asks for one, and nothing is measured.
    pub(crate) fn prompt_for_scale(&mut self, page: u32) {
        let message = format!(
            "Page {} has no scale here. Set the scale, or calibrate, before measuring.",
            page + 1
        );
        self.chrome.status = Some(message.clone());
        self.open_scale_panel(Some(message));
        if let Some(panel) = self.measure.panel.as_mut() {
            panel.page = page;
            panel.first = (page + 1).to_string();
            panel.last = (page + 1).to_string();
        }
    }

    // ---- the tools' questions ---------------------------------------------------------------

    /// What a measurement tool is asking the user, while its dialog is open.
    pub fn measure_request(&self) -> Option<&MeasureRequest> {
        self.measure.request.as_ref().map(|r| &r.request)
    }

    /// A measurement tool asked for something only the user knows.
    pub(crate) fn open_request(&mut self, view: ViewId, request: MeasureRequest) {
        // A measurement finished where no scale applies was not made: ask for a scale, as a
        // first click there does.
        if let MeasureRequest::NoScale { page } = request {
            self.prompt_for_scale(u32::try_from(page).unwrap_or(u32::MAX));
            return;
        }
        // Values are asked for in the unit of the scale that applies there, else feet.
        let unit = self
            .views
            .get(&view)
            .and_then(|dv| self.session.document(dv.doc).ok())
            .and_then(|d| {
                let page = match &request {
                    MeasureRequest::Distance { page, .. }
                    | MeasureRequest::Scale { page, .. }
                    | MeasureRequest::Depth { page, .. }
                    | MeasureRequest::NoScale { page } => *page,
                };
                let uu = d.pages().get(page).map_or(1.0, |g| g.user_unit());
                let (_, spec) = d.editor()?.scales().resolve(page, &[])?;
                spec.scale(uu)
            })
            .map_or(Unit::Foot, |s| match s.real_unit() {
                Unit::FeetInches => Unit::Foot,
                u => u,
            });
        self.measure.request = Some(RequestDialog {
            view,
            request,
            text: String::new(),
            unit,
            to_page: false,
            error: None,
            focus: true,
        });
    }

    /// Answers the open dialog's question with what is typed in it. Returns whether the
    /// answer was taken; what is wrong is shown in the dialog.
    pub fn submit_measure_request(&mut self) -> bool {
        let Some(dialog) = self.measure.request.clone() else {
            return false;
        };
        let input = match &dialog.request {
            MeasureRequest::Distance { .. } => match parse_length(&dialog.text, dialog.unit) {
                Ok(metres) if metres > 0.0 && metres.is_finite() => MeasureInput::Distance {
                    metres,
                    unit: dialog.unit,
                },
                Ok(_) => {
                    return self.request_error("The distance must be more than zero.");
                }
                Err(e) => return self.request_error(&format!("That is not a distance: {e}")),
            },
            MeasureRequest::Depth { .. } => match parse_length(&dialog.text, dialog.unit) {
                Ok(metres) if metres > 0.0 && metres.is_finite() => MeasureInput::Depth(metres),
                Ok(_) => return self.request_error("The depth must be more than zero."),
                Err(e) => return self.request_error(&format!("That is not a depth: {e}")),
            },
            MeasureRequest::Scale { .. } => match Scale::parse(dialog.text.trim()) {
                Ok(scale) => MeasureInput::Scale(scale),
                Err(e) => return self.request_error(&format!("That is not a scale: {e}")),
            },
            // Never opened as a dialog (see `open_request`).
            MeasureRequest::NoScale { .. } => {
                self.measure.request = None;
                return false;
            }
        };
        if dialog.to_page {
            if let MeasureRequest::Distance { .. } = dialog.request {
                self.measure.target_override = Some(ScaleTarget::Page);
            }
        }
        self.measure.request = None;
        self.answer_measure(dialog.view, input);
        self.measure.target_override = None;
        true
    }

    fn request_error(&mut self, message: &str) -> bool {
        if let Some(d) = self.measure.request.as_mut() {
            d.error = Some(message.to_owned());
        }
        false
    }

    /// Closes the open dialog without an answer: the measurement in progress is abandoned.
    pub fn cancel_measure_request(&mut self) {
        if let Some(dialog) = self.measure.request.take() {
            self.answer_measure(dialog.view, MeasureInput::Cancel);
        }
    }

    /// Fills the open dialog in, as typing into it does.
    pub fn set_measure_request_text(&mut self, text: &str, unit: Option<Unit>, to_page: bool) {
        if let Some(d) = self.measure.request.as_mut() {
            d.text = text.to_owned();
            if let Some(u) = unit {
                d.unit = u;
            }
            d.to_page = to_page;
        }
    }

    // ---- scale changes from the tools -------------------------------------------------------

    /// The pl-doc command a tool's scale change is made with.
    pub(crate) fn scale_command(
        &mut self,
        doc_id: DocumentId,
        change: &pl_tools::ScaleChange,
    ) -> Option<DocCommand> {
        let kind = self.markup.tool.kind()?;
        let format = self
            .markup
            .presets
            .current(kind)
            .measure_preset()
            .format_for(&change.scale);
        let setting = ScaleSetting::new(&change.scale, Some(&format));
        let target = self
            .measure
            .target_override
            .take()
            .unwrap_or_else(|| change.target.clone());
        Some(match target {
            ScaleTarget::Page => DocCommand::SetPageScale {
                page: change.page,
                scale: Some(setting),
            },
            ScaleTarget::Viewport(name) => {
                let doc = self.session.document(doc_id).ok()?;
                let vp = doc.page_scales(change.page as u32)?.get(&name)?;
                DocCommand::EditViewport {
                    page: change.page,
                    name,
                    bbox: [vp.bbox.x0, vp.bbox.y0, vp.bbox.x1, vp.bbox.y1],
                    scale: Some(setting),
                }
            }
            ScaleTarget::NewViewport { name, bbox } => DocCommand::AddViewport {
                page: change.page,
                name,
                bbox: [bbox.x0, bbox.y0, bbox.x1, bbox.y1],
                scale: Some(setting),
            },
        })
    }

    // ---- creation hooks ---------------------------------------------------------------------

    /// Finishes a markup a tool creates: a space gets its name and look, a count its number.
    pub(crate) fn dress_new(&mut self, doc_id: DocumentId, a: &mut Annotation) {
        if self.markup.tool == Tool::Space && a.kind == AnnotKind::Polygon {
            let used: Vec<String> = self
                .session
                .document(doc_id)
                .ok()
                .and_then(|d| d.editor())
                .map(|e| {
                    pl_takeoff::spaces_in(e.set().iter())
                        .into_iter()
                        .map(|s| s.name)
                        .collect()
                })
                .unwrap_or_default();
            let name = (1..)
                .map(|n| format!("Space {n}"))
                .find(|n| !used.contains(n))
                .unwrap_or_default();
            pl_takeoff::set_space_name(a, Some(&name));
            a.meta.subject = Some(SPACE_SUBJECT.to_owned());
            a.style.stroke = Some(Rgb::new(0.25, 0.4, 0.55));
            a.style.fill = None;
            a.style.width = 1.0;
            a.style.dash = Some(vec![5.0, 3.0]);
        }
        if a.measurement()
            .is_some_and(|m| m.kind == MeasureKind::Count)
        {
            self.number_count(doc_id, a);
        }
    }

    /// The sequence that numbers counts, if the document has one.
    pub fn count_sequence(&self) -> Option<SequenceDef> {
        self.active_document()
            .and_then(|d| d.editor())
            .and_then(|e| {
                e.scales()
                    .settings()
                    .sequences
                    .iter()
                    .find(|s| s.id == COUNT_SEQUENCE)
                    .cloned()
            })
    }

    /// Turns the numbering of new counts on (with this prefix) or off, as one undo step.
    pub fn set_count_numbering(&mut self, prefix: Option<&str>) -> bool {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return false;
        };
        if !self.ensure_annots(doc_id) {
            return false;
        }
        let Some(mut settings) = self.doc_settings_of(doc_id) else {
            return false;
        };
        settings.sequences.retain(|s| s.id != COUNT_SEQUENCE);
        if let Some(prefix) = prefix {
            settings.sequences.push(SequenceDef {
                id: COUNT_SEQUENCE.to_owned(),
                name: "Counts".to_owned(),
                prefix: prefix.to_owned(),
                suffix: String::new(),
                next: 1,
                step: 1,
                digits: 2,
            });
            self.ensure_list_column(NUMBER_COLUMN, ColumnType::Text);
        }
        self.exec(doc_id, DocCommand::SetDocSettings(Box::new(settings)), None)
    }

    fn doc_settings_of(&self, doc_id: DocumentId) -> Option<DocSettings> {
        self.session
            .document(doc_id)
            .ok()?
            .editor()
            .map(|e| e.scales().settings().clone())
    }

    /// The id of the list column with this title, made (of this type) when there is none.
    pub(crate) fn ensure_list_column(&mut self, title: &str, kind: ColumnType) -> Option<String> {
        if let Some(c) = self
            .list
            .schema
            .columns
            .iter()
            .find(|c| c.title.eq_ignore_ascii_case(title))
        {
            return Some(c.id.clone());
        }
        self.add_list_column(title, kind, &[]).ok()
    }

    fn number_count(&mut self, doc_id: DocumentId, a: &mut Annotation) {
        let Some(seq) = self.session.document(doc_id).ok().and_then(|d| {
            d.editor()?
                .scales()
                .settings()
                .sequences
                .iter()
                .find(|s| s.id == COUNT_SEQUENCE)
                .cloned()
        }) else {
            return;
        };
        let Some(column_id) = self.ensure_list_column(NUMBER_COLUMN, ColumnType::Text) else {
            return;
        };
        let Some(column) = self.list.schema.get(&column_id).cloned() else {
            return;
        };
        let existing: Vec<String> = self
            .session
            .document(doc_id)
            .ok()
            .and_then(|d| d.editor())
            .map(|e| {
                e.set()
                    .iter()
                    .map(|x| pl_takeoff::custom_value(x, &column).display())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let sequence = pl_takeoff::Sequence::new(&seq.prefix, seq.next, seq.step)
            .with_digits(seq.digits)
            .with_suffix(&seq.suffix);
        let label = sequence.next(existing.iter().map(String::as_str));
        let _ = pl_takeoff::set_custom_value(a, &column, Value::text(label));
    }

    // ---- legends ----------------------------------------------------------------------------

    /// The takeoff table of a document, as legends and the list see it.
    pub(crate) fn takeoff_table(&self, doc_id: DocumentId) -> Option<Table> {
        let doc = self.session.document(doc_id).ok()?;
        let editor = doc.editor()?;
        let layers: Vec<(pl_doc::pl_pdf::ObjectId, String)> = doc
            .cached_layers()
            .map(|layers| layers.iter().map(|l| (l.id, l.name.clone())).collect())
            .unwrap_or_default();
        let spaces = settings_spaces(editor.scales().settings());
        let context = ListContext {
            layers: &layers,
            schema: Some(&self.list.schema),
            ..ListContext::default()
        };
        let takeoff = TakeoffContext {
            spaces: &spaces,
            units: Some(list_units(&self.measure.list_units, doc_id, editor)),
            ..TakeoffContext::default()
        };
        Some(Table::build_takeoff(
            editor.set().iter(),
            &context,
            &takeoff,
        ))
    }

    /// Whether a document has legends to keep up to date.
    pub(crate) fn has_legends(&self, doc_id: DocumentId) -> bool {
        self.session
            .document(doc_id)
            .ok()
            .and_then(|d| d.editor())
            .is_some_and(|e| !e.scales().settings().legends.is_empty())
    }

    /// The text box of a legend with its text worked out for `table`: its contents, and its
    /// box refitted with its shown top-left corner kept.
    fn legend_markup(table: &Table, def: &LegendDef, mut a: Annotation) -> Annotation {
        let legend = takeoff_legend(def).evaluate(table);
        let lines = legend.lines();
        let text = lines.join("\n");
        let (size, padding, spacing, rotation) =
            a.free_text.as_ref().map_or((9.0, 4.0, 1.2, 0), |t| {
                (t.size, t.padding, t.line_spacing, t.rotation)
            });
        let (w, h) = legend_box_size(&lines, size, padding, spacing);
        if let pl_doc::pl_annot::Geometry::Rect(r) = a.geometry {
            let corner = shown_top_left(r, rotation);
            a.geometry =
                pl_doc::pl_annot::Geometry::Rect(rect_from_top_left(corner, w, h, rotation));
        }
        a.meta.contents = Some(text);
        a
    }

    /// Places a legend on the page in view: the markups by subject with their count, or with
    /// the total of `quantity`. The legend's definition goes into the document settings and its
    /// text box onto the page, as one undo step. Returns the text box's id.
    pub fn insert_legend(&mut self, quantity: Option<Builtin>) -> Option<AnnotId> {
        let id = self.active?;
        let dv = self.views.get(&id)?;
        let doc_id = dv.doc;
        let page = dv.shown_page();
        let geometry = dv.view.page_geometry(page).copied()?;
        let viewport = dv.view.viewport();
        let centre = Point::new(viewport.width / 2.0, viewport.height / 2.0);
        let at = dv
            .view
            .view_transform(page)
            .map(|t| t.view_to_page(&geometry) * centre)
            .filter(|p| p.x.is_finite() && p.y.is_finite())
            .unwrap_or_else(|| geometry.crop_box().center());
        if !self.ensure_annots(doc_id) {
            return None;
        }
        let mut settings = self.doc_settings_of(doc_id)?;
        let legend_id = (1..)
            .map(|n| format!("legend-{n}"))
            .find(|n| !settings.legends.iter().any(|l| &l.id == n))?;
        let title = match quantity {
            Some(q) => format!("{} legend", q.title()),
            None => "Count legend".to_owned(),
        };
        let rotation = geometry.rotate().degrees().rem_euclid(360) as u16;
        let def = LegendDef {
            id: legend_id.clone(),
            title,
            page: page as usize,
            rect: [at.x, at.y, at.x + 1.0, at.y + 1.0],
            subjects: Vec::new(),
            columns: quantity
                .map(|q| vec![q.title().to_lowercase()])
                .unwrap_or_default(),
        };
        let table = self.takeoff_table(doc_id)?;
        let uu = geometry.user_unit();
        let origin = Origin::now(self.markup.author.as_str());
        let mut a = Annotation::free_text(
            page as usize,
            Rect::new(at.x, at.y, at.x + 1.0, at.y + 1.0),
            "",
            &origin,
        );
        a.meta.subject = Some(LEGEND_SUBJECT.to_owned());
        a.style.stroke = Some(Rgb::new(0.35, 0.35, 0.35));
        a.style.width = 0.75 / uu;
        a.style.fill = Some(Rgb::new(1.0, 1.0, 1.0));
        if let Some(t) = a.free_text.as_mut() {
            t.size = 9.0 / uu;
            t.padding = 4.0 / uu;
            t.color = Rgb::new(0.0, 0.0, 0.0);
            t.font = FontStyle::Regular;
            t.rotation = rotation;
        }
        let mut data = a.app_data.take().unwrap_or_default();
        data.set(LEGEND_KEY, encode_text(&legend_id));
        a.app_data = Some(data);
        let a = Self::legend_markup(&table, &def, a);
        if let pl_doc::pl_annot::Geometry::Rect(r) = a.geometry {
            let mut def = def;
            def.rect = [r.x0, r.y0, r.x1, r.y1];
            settings.legends.push(def);
        }
        let new_id = a.id.clone();
        let command = DocCommand::Batch(vec![
            DocCommand::SetDocSettings(Box::new(settings)),
            DocCommand::AddAnnotations(vec![a]),
        ]);
        if self.exec(doc_id, command, None) {
            if let Ok(doc) = self.session.document_mut(doc_id) {
                let _ = doc.select(vec![new_id.clone()]);
            }
            self.set_tool(Tool::Select);
            Some(new_id)
        } else {
            None
        }
    }

    /// Works every legend of a document out again and writes those whose text changed, with
    /// the merge key of the change that made them stale (so the change and its legends are
    /// one undo step).
    pub(crate) fn refresh_legends(&mut self, doc_id: DocumentId, key: u64) {
        let Some(table) = self.takeoff_table(doc_id) else {
            return;
        };
        let Ok(doc) = self.session.document_mut(doc_id) else {
            return;
        };
        let _ = doc.take_takeoff_refresh();
        let Some(editor) = doc.editor() else {
            return;
        };
        let defs = editor.scales().settings().legends.clone();
        let now = PdfDate::now();
        let mut changed = Vec::new();
        for a in editor.set().iter() {
            let Some(id) = legend_id(a) else {
                continue;
            };
            let Some(def) = defs.iter().find(|d| d.id == id) else {
                continue;
            };
            if a.meta.flags.is_locked() || a.free_text.is_none() {
                continue;
            }
            let mut fresh = Self::legend_markup(&table, def, a.clone());
            if fresh.meta.contents != a.meta.contents || fresh.geometry != a.geometry {
                fresh.touch(now);
                changed.push(fresh);
            }
        }
        if changed.is_empty() {
            return;
        }
        if let Err(e) = doc.apply_drag(DocCommand::ModifyAnnotations(changed), key) {
            self.chrome.status = Some(format!("A legend could not be brought up to date: {e}"));
        }
    }

    /// The text of the legends of the active document, by legend id.
    pub fn legend_texts(&self) -> Vec<(String, String)> {
        let Some(doc) = self.active_document() else {
            return Vec::new();
        };
        let Some(editor) = doc.editor() else {
            return Vec::new();
        };
        editor
            .set()
            .iter()
            .filter_map(|a| Some((legend_id(a)?, a.meta.contents.clone().unwrap_or_default())))
            .collect()
    }

    // ---- spaces -----------------------------------------------------------------------------

    /// Names (or, with an empty name, unnames) the selected polygon or rectangle as a space.
    pub fn set_space_name(&mut self, name: &str) -> bool {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return false;
        };
        let changed: Vec<Annotation> = match self.session.document(doc_id) {
            Ok(doc) => doc
                .selection()
                .iter()
                .filter_map(|id| doc.annotation(id))
                .filter(|a| matches!(a.kind, AnnotKind::Polygon | AnnotKind::Square))
                .filter(|a| !a.meta.flags.is_locked())
                .filter_map(|a| {
                    let mut b = a.clone();
                    let name = name.trim();
                    pl_takeoff::set_space_name(&mut b, (!name.is_empty()).then_some(name)).then(
                        || {
                            b.touch(PdfDate::now());
                            b
                        },
                    )
                })
                .collect(),
            Err(_) => return false,
        };
        !changed.is_empty() && self.exec(doc_id, DocCommand::ModifyAnnotations(changed), None)
    }

    // ---- the list's takeoff layout ----------------------------------------------------------

    /// Lays the markups list out for a takeoff: subject, page, type, the quantity columns and
    /// the space, grouped by subject with subtotals and a total.
    pub fn use_takeoff_layout(&mut self) {
        let shown = [
            Builtin::Subject,
            Builtin::PageLabel,
            Builtin::Kind,
            Builtin::Count,
            Builtin::Length,
            Builtin::Perimeter,
            Builtin::Area,
            Builtin::Volume,
            Builtin::WallArea,
            Builtin::Scale,
            Builtin::Space,
            Builtin::Comment,
        ];
        self.edit_list_layout(|layout| {
            let schema_columns: Vec<ColumnId> = layout
                .columns
                .iter()
                .filter(|c| matches!(c.column, ColumnId::Custom(_)))
                .map(|c| c.column.clone())
                .collect();
            let mut ordered = Vec::new();
            for b in shown {
                if let Some(state) = layout
                    .columns
                    .iter()
                    .find(|c| c.column == ColumnId::Builtin(b))
                {
                    let mut s = state.clone();
                    s.visible = true;
                    ordered.push(s);
                }
            }
            for c in &layout.columns {
                if !ordered.iter().any(|o| o.column == c.column) {
                    let mut s = c.clone();
                    s.visible = schema_columns.contains(&c.column);
                    ordered.push(s);
                }
            }
            layout.columns = ordered;
            layout.group = vec![pl_takeoff::GroupKey::asc(Builtin::Subject)];
            layout.sort.clear();
            layout.collapsed.clear();
        });
        self.set_markups_list_open(true);
    }

    // ---- the properties of a measurement ----------------------------------------------------

    /// Changes the measurement side of the selected measurements (one undo step; `drag` as in
    /// [`AppState::exec`]).
    pub(crate) fn edit_measurement(
        &mut self,
        drag: Option<u64>,
        edit: &dyn Fn(&mut Annotation, f64),
    ) -> bool {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return false;
        };
        let changed: Vec<Annotation> = match self.session.document(doc_id) {
            Ok(doc) => doc
                .selection()
                .iter()
                .filter_map(|id| doc.annotation(id))
                .filter(|a| a.measure.is_some() && !a.meta.flags.is_locked())
                .filter_map(|a| {
                    let uu = doc.pages().get(a.page).map_or(1.0, |g| g.user_unit());
                    let mut b = a.clone();
                    edit(&mut b, uu);
                    b.sync_measurement();
                    (b != *a).then(|| {
                        b.touch(PdfDate::now());
                        b
                    })
                })
                .collect(),
            Err(_) => return false,
        };
        !changed.is_empty() && self.exec(doc_id, DocCommand::ModifyAnnotations(changed), drag)
    }

    /// Shows the selected measurements in `unit` to `precision`.
    pub fn set_measure_units(&mut self, unit: Unit, precision: Precision) -> bool {
        self.edit_measurement(None, &move |a, uu| {
            let Some(m) = a.measure.as_mut() else {
                return;
            };
            let Some(spec) = m.spec.as_ref() else {
                return;
            };
            if spec.y.is_some() {
                return;
            }
            if let Some(scale) = spec.scale(uu) {
                m.spec = Some(MeasureSpec::new(
                    &scale,
                    uu,
                    &format_for(&scale, Some(unit), precision),
                ));
            }
        })
    }

    /// Sets the depth of the selected area or volume measurements, in metres.
    pub fn set_measure_depth(&mut self, metres: f64) -> bool {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return false;
        };
        let Some(id) = self.first_selected().map(|a| a.id.clone()) else {
            return false;
        };
        let command = match self
            .session
            .document(doc_id)
            .ok()
            .and_then(|d| d.editor())
            .map(|e| pl_doc::measure_ops::set_depth(e.set(), &id, metres, PdfDate::now()))
        {
            Some(Ok(c)) => c,
            Some(Err(e)) => {
                self.chrome.status = Some(format!("That depth cannot be used: {e}"));
                return false;
            }
            None => return false,
        };
        self.exec(doc_id, command, None)
    }

    /// Sets the wall height of the selected measurements (the list's Height column, in the
    /// list's length unit), which Wall Area multiplies by.
    pub fn set_wall_height(&mut self, height: Option<f64>) -> bool {
        let Some(doc_id) = self.active_view().map(|v| v.doc) else {
            return false;
        };
        let Some(column_id) = self.ensure_list_column(HEIGHT_COLUMN, ColumnType::Number) else {
            return false;
        };
        let Some(column) = self.list.schema.get(&column_id).cloned() else {
            return false;
        };
        let value = height.map_or(Value::Empty, Value::number);
        let changed: Vec<Annotation> = match self.session.document(doc_id) {
            Ok(doc) => doc
                .selection()
                .iter()
                .filter_map(|id| doc.annotation(id))
                .filter(|a| !a.meta.flags.is_locked())
                .filter_map(|a| {
                    let mut b = a.clone();
                    match pl_takeoff::set_custom_value(&mut b, &column, value.clone()) {
                        Ok(true) => {
                            b.touch(PdfDate::now());
                            Some(b)
                        }
                        _ => None,
                    }
                })
                .collect(),
            Err(_) => return false,
        };
        !changed.is_empty() && self.exec(doc_id, DocCommand::ModifyAnnotations(changed), None)
    }

    /// The wall height of a markup, from the list's Height column.
    fn wall_height(&self, a: &Annotation) -> Option<f64> {
        let column = self
            .list
            .schema
            .columns
            .iter()
            .find(|c| c.title.eq_ignore_ascii_case(HEIGHT_COLUMN))?;
        pl_takeoff::custom_value(a, column).as_number()
    }
}

/// The schema's column titles and the built-in ones, as a formula names them.
pub(crate) fn formula_names(schema: &Schema) -> Vec<String> {
    let mut names: Vec<String> = schema.columns.iter().map(|c| c.title.clone()).collect();
    names.extend(Builtin::ALL.iter().map(|b| b.title().to_owned()));
    names
}

/// What is wrong with a formula for a list with these columns: a parse error, or a column it
/// names that the list does not have. `None` when it can be used.
pub fn formula_problem(schema: &Schema, title: &str, source: &str) -> Option<String> {
    if source.trim().is_empty() {
        return Some("Type a formula, such as [Length] * 2.".to_owned());
    }
    let formula = match pl_takeoff::Formula::parse(source) {
        Ok(f) => f,
        Err(e) => return Some(format!("{e} ({})", e.code())),
    };
    let names = formula_names(schema);
    for name in &formula.references {
        let folded = name.trim().to_lowercase();
        if folded == title.trim().to_lowercase() {
            return Some(format!(
                "The formula uses its own column [{}] (#CYCLE!)",
                name.trim()
            ));
        }
        let known = names.iter().any(|n| n.trim().to_lowercase() == folded)
            || schema.columns.iter().any(|c| c.id.to_lowercase() == folded);
        if !known {
            return Some(format!("There is no column [{}] (#REF!)", name.trim()));
        }
    }
    None
}

// ---- painting ---------------------------------------------------------------------------------

/// Draws the snap marker at `at` (screen), with a glyph of its own for each kind of point.
pub(crate) fn paint_snap_marker(
    painter: &egui::Painter,
    at: Pos2,
    kind: Option<SnapKind>,
    accent: Color32,
) {
    let s = 5.0_f32;
    let stroke = Stroke::new(1.5, accent);
    let line = |a: Pos2, b: Pos2| {
        painter.line_segment([a, b], stroke);
    };
    match kind {
        Some(SnapKind::Midpoint) => {
            painter.add(egui::Shape::closed_line(
                vec![
                    Pos2::new(at.x, at.y - s),
                    Pos2::new(at.x + s, at.y + s * 0.8),
                    Pos2::new(at.x - s, at.y + s * 0.8),
                ],
                stroke,
            ));
        }
        Some(SnapKind::Intersection) => {
            line(Pos2::new(at.x - s, at.y - s), Pos2::new(at.x + s, at.y + s));
            line(Pos2::new(at.x - s, at.y + s), Pos2::new(at.x + s, at.y - s));
        }
        Some(SnapKind::Nearest) => {
            painter.add(egui::Shape::closed_line(
                vec![
                    Pos2::new(at.x - s, at.y - s),
                    Pos2::new(at.x + s, at.y - s),
                    Pos2::new(at.x - s, at.y + s),
                    Pos2::new(at.x + s, at.y + s),
                ],
                stroke,
            ));
        }
        Some(SnapKind::MeasureEndpoint) => {
            painter.add(egui::Shape::closed_line(
                vec![
                    Pos2::new(at.x, at.y - s * 1.2),
                    Pos2::new(at.x + s * 1.2, at.y),
                    Pos2::new(at.x, at.y + s * 1.2),
                    Pos2::new(at.x - s * 1.2, at.y),
                ],
                stroke,
            ));
        }
        Some(SnapKind::MarkupVertex) => {
            painter.circle_stroke(at, s, stroke);
        }
        Some(SnapKind::Endpoint) | None => {
            let square = egui::Rect::from_center_size(at, egui::Vec2::splat(2.0 * s));
            painter.rect_stroke(square, 0.0, stroke, StrokeKind::Middle);
            let half = s + 3.0;
            painter.line_segment(
                [Pos2::new(at.x - half, at.y), Pos2::new(at.x + half, at.y)],
                Stroke::new(1.0, accent),
            );
            painter.line_segment(
                [Pos2::new(at.x, at.y - half), Pos2::new(at.x, at.y + half)],
                Stroke::new(1.0, accent),
            );
        }
    }
}

/// The lines of the read-out beside the pointer.
pub fn readout_lines(r: &Readout) -> Vec<String> {
    let mut lines = Vec::new();
    let mut push = |name: &str, v: &Option<pl_tools::Value>| {
        if let Some(v) = v {
            lines.push(format!("{name} {}", v.text));
        }
    };
    push("Segment", &r.segment);
    push("Total", &r.total);
    push("Area", &r.area);
    push("Volume", &r.volume);
    push("Angle", &r.angle);
    push("Radius", &r.radius);
    if let Some(n) = r.count {
        lines.push(format!("Count {n}"));
    }
    lines
}

/// Draws the read-out of the measurement in progress beside the pointer: a small box of lines
/// in the interface font.
pub(crate) fn paint_readout(
    painter: &egui::Painter,
    pointer: Pos2,
    readout: &Readout,
    palette: &Palette,
    visuals: &egui::Visuals,
) {
    let lines = readout_lines(readout);
    if lines.is_empty() {
        return;
    }
    let font = FontId::proportional(11.5);
    let galleys: Vec<_> = lines
        .iter()
        .map(|l| painter.layout_no_wrap(l.clone(), font.clone(), visuals.text_color()))
        .collect();
    let width = galleys.iter().map(|g| g.size().x).fold(0.0_f32, f32::max);
    let line_h = galleys.first().map_or(14.0, |g| g.size().y);
    let pad = 5.0;
    let size = egui::vec2(width + 2.0 * pad, line_h * galleys.len() as f32 + 2.0 * pad);
    let mut origin = pointer + egui::vec2(16.0, 14.0);
    let clip = painter.clip_rect();
    if origin.x + size.x > clip.right() {
        origin.x = pointer.x - 16.0 - size.x;
    }
    if origin.y + size.y > clip.bottom() {
        origin.y = pointer.y - 14.0 - size.y;
    }
    let rect = egui::Rect::from_min_size(origin, size);
    painter.rect(
        rect,
        3.0,
        visuals.panel_fill.gamma_multiply(0.96),
        Stroke::new(1.0, palette.page_edge),
        StrokeKind::Inside,
    );
    for (i, galley) in galleys.into_iter().enumerate() {
        painter.galley(
            Pos2::new(rect.left() + pad, rect.top() + pad + line_h * i as f32),
            galley,
            visuals.text_color(),
        );
    }
}

// ---- the scale panel and the dialogs ----------------------------------------------------------

fn unit_combo(
    ui: &mut Ui,
    id: &str,
    name: &str,
    unit: &mut Option<Unit>,
    allow_none: bool,
) -> bool {
    let mut changed = false;
    let current = unit.map_or("As the scale", unit_name);
    egui::ComboBox::from_id_salt(id)
        .selected_text(current)
        .width(130.0)
        .show_ui(ui, |ui| {
            if allow_none
                && ui
                    .selectable_label(unit.is_none(), "As the scale")
                    .clicked()
            {
                *unit = None;
                changed = true;
            }
            for (u, label) in UNITS {
                if ui.selectable_label(*unit == Some(u), label).clicked() {
                    *unit = Some(u);
                    changed = true;
                }
            }
        })
        .response
        .accessible_name(name);
    changed
}

fn precision_combo(
    ui: &mut Ui,
    id: &str,
    name: &str,
    unit: Unit,
    precision: &mut Precision,
) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(id)
        .selected_text(precision_name(unit, *precision))
        .width(80.0)
        .show_ui(ui, |ui| {
            for (p, label) in precisions(unit) {
                if ui.selectable_label(*precision == p, label).clicked() {
                    *precision = p;
                    changed = true;
                }
            }
        })
        .response
        .accessible_name(name);
    changed
}

fn preset_combo(ui: &mut Ui, id: &str, text: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .selected_text("Presets")
        .width(150.0)
        .height(320.0)
        .show_ui(ui, |ui| {
            for (family, name) in [
                (ScaleFamily::Architectural, "Architectural"),
                (ScaleFamily::Engineering, "Engineering"),
                (ScaleFamily::Metric, "Metric"),
            ] {
                ui.label(RichText::new(name).font(theme::bold(12.0)));
                for preset in Scale::presets().into_iter().filter(|p| p.family == family) {
                    if ui
                        .selectable_label(*text == preset.label, &preset.label)
                        .clicked()
                    {
                        *text = preset.label.clone();
                    }
                }
            }
        })
        .response
        .accessible_name("Scale presets");
}

/// The scale panel, when it is open.
pub fn scale_panel(app: &mut AppState, ctx: &egui::Context) {
    let Some(mut panel) = app.measure.panel.take() else {
        return;
    };
    let palette = Palette::of(&ctx.global_style().visuals);
    let viewports: Vec<String> = app
        .active_document()
        .and_then(|d| d.page_scales(panel.page))
        .map(|v| v.viewports.iter().map(|v| v.name.clone()).collect())
        .unwrap_or_default();
    let mut apply = false;
    let mut close = false;
    let mut clear = false;
    let mut calibrate = false;
    let mut open = true;
    egui::Window::new("Scale")
        .id(Id::unique("plumbline-scale-panel"))
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .anchor(Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -36.0))
        .show(ctx, |ui| {
            ui.set_width(330.0);
            if let Some(prompt) = &panel.prompt {
                ui.label(RichText::new(prompt).color(palette.warning));
                ui.add_space(4.0);
            }
            ui.horizontal(|ui| {
                ui.label(format!("Page {}", panel.page + 1));
                let current = panel
                    .target
                    .clone()
                    .unwrap_or_else(|| "Whole page".to_owned());
                egui::ComboBox::from_id_salt("plumbline-scale-target")
                    .selected_text(current)
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(panel.target.is_none(), "Whole page")
                            .clicked()
                        {
                            panel.target = None;
                        }
                        for name in &viewports {
                            let on = panel.target.as_deref() == Some(name.as_str());
                            if ui
                                .selectable_label(on, format!("Viewport {name}"))
                                .clicked()
                            {
                                panel.target = Some(name.clone());
                            }
                        }
                    })
                    .response
                    .accessible_name("Scale applies to");
            });
            ui.horizontal(|ui| {
                let response = ui
                    .add(
                        TextEdit::singleline(&mut panel.text)
                            .id(Id::unique("plumbline-scale-text"))
                            .desired_width(150.0)
                            .hint_text("1/4 in = 1 ft, 1:100"),
                    )
                    .accessible_name("Drawing scale");
                apply |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                preset_combo(ui, "plumbline-scale-presets", &mut panel.text);
            });
            ui.horizontal(|ui| {
                ui.label("Units");
                if unit_combo(
                    ui,
                    "plumbline-scale-unit",
                    "Display units",
                    &mut panel.unit,
                    true,
                ) {
                    if let Some(u) = panel.unit {
                        panel.precision = default_precision(u);
                    }
                }
                let unit = panel
                    .unit
                    .or_else(|| {
                        Scale::parse(panel.text.trim())
                            .ok()
                            .map(|s| MeasureFormat::for_scale(&s).length.unit)
                    })
                    .unwrap_or(Unit::Foot);
                precision_combo(
                    ui,
                    "plumbline-scale-precision",
                    "Precision",
                    unit,
                    &mut panel.precision,
                );
            });
            if panel.target.is_none() {
                ui.horizontal(|ui| {
                    ui.label("Apply to pages");
                    ui.add(
                        TextEdit::singleline(&mut panel.first)
                            .id(Id::unique("plumbline-scale-first"))
                            .desired_width(36.0),
                    )
                    .accessible_name("First page");
                    ui.label("to");
                    ui.add(
                        TextEdit::singleline(&mut panel.last)
                            .id(Id::unique("plumbline-scale-last"))
                            .desired_width(36.0),
                    )
                    .accessible_name("Last page");
                });
            }
            if let Some(error) = &panel.error {
                ui.label(RichText::new(error).color(palette.warning));
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                apply |= ui.button("Apply scale").clicked();
                calibrate |= ui
                    .button("Calibrate\u{2026}")
                    .on_hover_text("Pick two points a known distance apart")
                    .clicked();
                if panel.target.is_none() {
                    clear |= ui
                        .add(Button::new("Clear page scale").frame(false))
                        .clicked();
                }
                close |= ui.button("Close scale panel").clicked();
            });
        });
    app.measure.panel = Some(panel);
    if apply {
        app.apply_scale_panel();
    } else if clear {
        app.clear_page_scale();
    } else if calibrate {
        app.measure.panel = None;
        app.set_tool(Tool::Calibrate);
    } else if close || !open {
        app.measure.panel = None;
    }
}

/// The dialog of a measurement tool's question, when one is open.
pub fn request_dialog(app: &mut AppState, ctx: &egui::Context) {
    let Some(mut dialog) = app.measure.request.take() else {
        return;
    };
    let palette = Palette::of(&ctx.global_style().visuals);
    let (title, field, hint) = match &dialog.request {
        MeasureRequest::Distance { .. } => ("Calibrate", "Known distance", "12'-6\", 3.5 m"),
        MeasureRequest::Scale { .. } => ("New viewport", "Viewport scale", "1/8 in = 1 ft, 1:50"),
        MeasureRequest::Depth { .. } => ("New volume", "Depth of the volume", "8', 2.4 m"),
        // Never opened as a dialog (see `open_request`).
        MeasureRequest::NoScale { .. } => return,
    };
    let in_viewport = matches!(
        &dialog.request,
        MeasureRequest::Distance {
            target: ScaleTarget::Viewport(_) | ScaleTarget::NewViewport { .. },
            ..
        }
    );
    let mut ok = false;
    let mut cancel = false;
    let modal = egui::Modal::new(Id::unique("plumbline-measure-request")).show(ctx, |ui| {
        ui.set_width(340.0);
        ui.label(RichText::new(title).font(theme::bold(15.0)));
        ui.add_space(4.0);
        if let MeasureRequest::Distance { a, b, .. } = &dialog.request {
            let span = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
            ui.label(format!(
                "The two points are {span:.2} page units apart. How far apart are they on the \
                 ground?"
            ));
        }
        ui.horizontal(|ui| {
            let response = ui
                .add(
                    TextEdit::singleline(&mut dialog.text)
                        .id(Id::unique("plumbline-measure-request-text"))
                        .desired_width(150.0)
                        .hint_text(hint),
                )
                .accessible_name(field);
            if std::mem::take(&mut dialog.focus) {
                response.request_focus();
            }
            ok |= response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            match &dialog.request {
                MeasureRequest::Scale { .. } => {
                    preset_combo(ui, "plumbline-request-presets", &mut dialog.text);
                }
                _ => {
                    let mut unit = Some(dialog.unit);
                    if unit_combo(
                        ui,
                        "plumbline-request-unit",
                        "Distance unit",
                        &mut unit,
                        false,
                    ) {
                        if let Some(u) = unit {
                            dialog.unit = u;
                        }
                    }
                }
            }
        });
        if in_viewport {
            ui.checkbox(
                &mut dialog.to_page,
                "Set the whole page's scale instead of the viewport's",
            );
        }
        if let Some(error) = &dialog.error {
            ui.label(RichText::new(error).color(palette.warning));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ok |= ui.button("OK").clicked();
            cancel |= ui.button("Cancel").clicked();
        });
    });
    if !ok && !cancel && modal.should_close() {
        cancel = true;
    }
    app.measure.request = Some(dialog);
    if ok {
        app.submit_measure_request();
    } else if cancel {
        app.cancel_measure_request();
    }
}

/// The measurement section of the properties panel: for a selected measurement, or for the
/// active measurement tool.
pub(crate) fn measure_properties(app: &mut AppState, ui: &mut Ui) {
    let muted = Palette::of(ui.visuals()).muted_text;
    if let Some(first) = app.first_selected().cloned() {
        if let Some(m) = first.measure.clone() {
            ui.add_space(4.0);
            ui.label(RichText::new("Measurement").font(theme::bold(12.5)));
            if let Some(text) = first.measure_text() {
                ui.label(RichText::new(text).font(theme::bold(13.0)));
            }
            if let Some(spec) = &m.spec {
                ui.label(RichText::new(format!("Scale {}", spec.ratio)).color(muted));
            }
            let editable = !first.meta.flags.is_locked();
            ui.add_enabled_ui(editable, |ui| {
                if let Some(spec) = m.spec.as_ref().filter(|s| s.y.is_none()) {
                    let uu = app
                        .active_document()
                        .and_then(|d| d.pages().get(first.page))
                        .map_or(1.0, |g| g.user_unit());
                    if let Some(scale) = spec.scale(uu) {
                        let shown = spec.x.first().map(|f| f.label.clone()).unwrap_or_default();
                        // Feet and inches are written as two number formats, feet then inches.
                        let mut unit = if spec.x.len() > 1 {
                            Some(Unit::FeetInches)
                        } else {
                            UNITS
                                .iter()
                                .find(|(u, _)| u.abbreviation() == shown)
                                .map(|(u, _)| *u)
                        }
                        .or(Some(MeasureFormat::for_scale(&scale).length.unit));
                        let base = unit.unwrap_or(Unit::Foot);
                        let mut precision = match spec.x.last() {
                            Some(f) if fractional(base) => Precision::Fraction(f.precision.max(1)),
                            Some(f) => Precision::Decimal(f.places()),
                            None => default_precision(base),
                        };
                        ui.horizontal(|ui| {
                            ui.label("Units");
                            let mut changed = unit_combo(
                                ui,
                                "plumbline-m-unit",
                                "Measurement units",
                                &mut unit,
                                false,
                            );
                            let u = unit.unwrap_or(base);
                            if changed {
                                precision = default_precision(u);
                            }
                            changed |= precision_combo(
                                ui,
                                "plumbline-m-precision",
                                "Measurement precision",
                                u,
                                &mut precision,
                            );
                            if changed {
                                app.set_measure_units(u, precision);
                            }
                        });
                    }
                }
                let mut caption = m.caption;
                if ui
                    .checkbox(&mut caption, "Show the value on the page")
                    .changed()
                {
                    app.edit_measurement(None, &move |a, _| {
                        if let Some(m) = a.measure.as_mut() {
                            m.caption = caption;
                        }
                    });
                }
                if m.kind == MeasureKind::Count {
                    symbol_combo(ui, m.symbol, |s| {
                        app.edit_measurement(None, &move |a, _| {
                            if let Some(m) = a.measure.as_mut() {
                                m.symbol = s;
                            }
                        });
                    });
                }
                if matches!(m.kind, MeasureKind::Area | MeasureKind::Volume) {
                    let shown = m
                        .depth
                        .map(|d| LengthFormat::decimal(Unit::Foot, 2).format(d))
                        .unwrap_or_default();
                    if let Some(text) = field(app, ui, "depth", "Depth", &shown, "8', 2.4 m") {
                        match parse_length(&text, Unit::Foot) {
                            Ok(metres) if metres > 0.0 => {
                                app.set_measure_depth(metres);
                            }
                            _ => {
                                app.chrome.status =
                                    Some(format!("\"{text}\" is not a depth that can be used"));
                            }
                        }
                    }
                }
                if matches!(
                    m.kind,
                    MeasureKind::Length | MeasureKind::Polylength | MeasureKind::Perimeter
                ) {
                    let shown = app
                        .wall_height(&first)
                        .map(|h| format!("{h}"))
                        .unwrap_or_default();
                    if let Some(text) =
                        field(app, ui, "height", "Wall height", &shown, "list units")
                    {
                        let text = text.trim().to_owned();
                        if text.is_empty() {
                            app.set_wall_height(None);
                        } else if let Ok(h) = text.parse::<f64>() {
                            app.set_wall_height(Some(h));
                        } else {
                            app.chrome.status = Some(format!("\"{text}\" is not a height"));
                        }
                    }
                }
            });
        }
        return;
    }
    let tool = app.tool();
    let Some(kind) = tool
        .kind()
        .filter(|_| tool.is_measure() && tool.drawing().is_some())
    else {
        return;
    };
    let mut preset = app.presets().current(kind);
    let mut mp = preset.measure_preset();
    let before = mp;
    ui.add_space(4.0);
    ui.label(RichText::new("Measurement").font(theme::bold(12.5)));
    if kind != pl_tools::ToolKind::Count && kind != pl_tools::ToolKind::Angle {
        let mut unit = mp.length.map(|l| l.unit);
        let base = unit.unwrap_or(Unit::Foot);
        let mut precision = mp.length.map_or(default_precision(base), |l| l.precision);
        ui.horizontal(|ui| {
            ui.label("Units");
            let mut changed = unit_combo(ui, "plumbline-tool-unit", "Tool units", &mut unit, true);
            let u = unit.unwrap_or(base);
            if changed {
                precision = default_precision(u);
            }
            if unit.is_some() {
                changed |= precision_combo(
                    ui,
                    "plumbline-tool-precision",
                    "Tool precision",
                    u,
                    &mut precision,
                );
            }
            if changed {
                mp.length = unit.map(|u| LengthFormat::new(u, precision));
                mp.area = unit.map(|u| AreaFormat::new(AreaUnit::for_length(u), 2));
            }
        });
    }
    ui.checkbox(&mut mp.caption, "Show the value on the page");
    if kind == pl_tools::ToolKind::Count {
        symbol_combo(ui, mp.symbol, |s| mp.symbol = s);
        let sequence = app.count_sequence();
        let mut numbered = sequence.is_some();
        if ui
            .checkbox(&mut numbered, "Number each new count")
            .changed()
        {
            app.set_count_numbering(numbered.then_some("C-"));
        }
        if let Some(seq) = sequence {
            if let Some(text) = field(app, ui, "prefix", "Number prefix", &seq.prefix, "C-") {
                app.set_count_numbering(Some(&text));
            }
        }
    }
    if kind == pl_tools::ToolKind::Volume {
        let shown = mp
            .depth
            .map(|d| LengthFormat::decimal(Unit::Foot, 2).format(d))
            .unwrap_or_default();
        if let Some(text) = field(app, ui, "tool-depth", "Tool depth", &shown, "ask each time") {
            mp.depth = parse_length(&text, Unit::Foot).ok().filter(|d| *d > 0.0);
        }
    }
    if mp != before {
        preset.measure = Some(mp);
        preset.name.clear();
        app.set_current_preset(preset);
    }
}

/// The space name of a selected polygon or rectangle (the end of the properties panel).
pub(crate) fn space_properties(app: &mut AppState, ui: &mut Ui) {
    let Some(first) = app.first_selected().cloned() else {
        return;
    };
    if !matches!(first.kind, AnnotKind::Polygon | AnnotKind::Square) || first.measure.is_some() {
        return;
    }
    ui.add_enabled_ui(!first.meta.flags.is_locked(), |ui| {
        let shown = pl_takeoff::space_name(&first).unwrap_or_default();
        if let Some(text) = field(app, ui, "space", "Space name", &shown, "Room 101") {
            app.set_space_name(&text);
        }
    });
}

fn symbol_combo(ui: &mut Ui, current: CountSymbol, mut pick: impl FnMut(CountSymbol)) {
    ui.horizontal(|ui| {
        ui.label("Symbol");
        let mut picked = None;
        egui::ComboBox::from_id_salt("plumbline-count-symbol")
            .selected_text(current.name())
            .width(96.0)
            .show_ui(ui, |ui| {
                for s in CountSymbol::ALL {
                    if ui.selectable_label(current == s, s.name()).clicked() {
                        picked = Some(s);
                    }
                }
            })
            .response
            .accessible_name("Count symbol");
        if let Some(s) = picked {
            pick(s);
        }
    });
}

/// A labelled one-line text box of the measurement properties that shows `shown` until it is
/// typed in. Returns the text when the typing is finished (Enter, or the box is left).
fn field(
    app: &mut AppState,
    ui: &mut Ui,
    key: &'static str,
    name: &str,
    shown: &str,
    hint: &str,
) -> Option<String> {
    let id = Id::unique(("plumbline-measure-field", key));
    let focused = ui.memory(|m| m.has_focus(id));
    let mut buffer = app.measure.fields.remove(key).unwrap_or_default();
    if !focused {
        buffer = shown.to_owned();
    }
    let mut done = None;
    ui.horizontal(|ui| {
        // The box is named by its label (one name in the accessibility tree, not two).
        let label = ui.label(name);
        let response = ui
            .add(
                TextEdit::singleline(&mut buffer)
                    .id(id)
                    .desired_width(110.0)
                    .hint_text(hint),
            )
            .labelled_by(label.id);
        if response.lost_focus() && buffer.trim() != shown.trim() {
            done = Some(buffer.clone());
        }
    });
    app.measure.fields.insert(key, buffer);
    done
}

/// The Snap entries of a menu: one box for each kind of point.
pub fn snap_menu(app: &mut AppState, ui: &mut Ui) {
    let mut t = app.snap_toggles();
    let mut on = app.snap_enabled();
    let mut changed = false;
    if ui.checkbox(&mut on, "Snap").changed() {
        app.set_snap(on);
    }
    ui.separator();
    changed |= ui.checkbox(&mut t.endpoint, "Snap to endpoints").changed();
    changed |= ui.checkbox(&mut t.midpoint, "Snap to midpoints").changed();
    changed |= ui
        .checkbox(&mut t.intersection, "Snap to intersections")
        .changed();
    changed |= ui
        .checkbox(&mut t.nearest, "Snap to nearest point on a line")
        .changed();
    changed |= ui
        .checkbox(&mut t.markups, "Snap to markups and measurements")
        .changed();
    if changed {
        app.set_snap_toggles(t);
    }
}
