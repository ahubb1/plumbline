//! Rows: one per markup, with the replies made to it as child rows.

use std::collections::HashMap;

use pl_annot::{
    AnnotId, AnnotKind, Annotation, Flags, FreeTextIntent, MeasureKind, PdfDate, ReviewState, Rgb,
};
use pl_pdf::ObjectId;

use pl_geom::Point;

use crate::column::{custom_value, Builtin, ColumnId, ColumnType, CustomColumn, Schema};
use crate::formula::FormulaError;
use crate::quantity::{QuantityUnits, RowMeasure};
use crate::space::{anchor, space_at, spaces_in, Space};
use crate::value::Value;

/// The `/StateModel` of a checkmark, and its two states, as the PDF specification names them.
pub const MARKED_MODEL: &str = "Marked";
pub const MARKED: &str = "Marked";
pub const UNMARKED: &str = "Unmarked";

/// A status annotation that sets the checkmark of `parent`: the standard hidden Text reply
/// with `/StateModel (Marked)` and `/State (Marked)` or `(Unmarked)`. Add it to the set after
/// its parent; the latest one is the markup's checkmark.
pub fn checkmark_status(
    parent: &Annotation,
    checked: bool,
    origin: &pl_annot::Origin,
) -> Annotation {
    let mut a = Annotation::review_status(parent, ReviewState::None, origin);
    let state = if checked { MARKED } else { UNMARKED };
    a.meta.state = Some(state.to_string());
    a.meta.state_model = Some(MARKED_MODEL.to_string());
    a.meta.contents = Some(format!("{state} set by {}", origin.author));
    a
}

/// Which annotations become rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowOptions {
    /// Links are not markups; they are left out unless this is set (or the link carries
    /// Bluebeam column values or a subject: see [`Table::build`]).
    pub include_links: bool,
    /// Annotations flagged hidden or no-view (status annotations are never rows).
    pub include_hidden: bool,
}

impl Default for RowOptions {
    fn default() -> RowOptions {
        RowOptions {
            include_links: false,
            include_hidden: true,
        }
    }
}

/// What the rows are built from besides the annotations.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListContext<'a> {
    /// The label of each page, by page index. A page without one (or an empty slice) is
    /// labelled with its number.
    pub page_labels: &'a [String],
    /// Layer names by optional-content group. An unknown layer is shown by its object number.
    pub layers: &'a [(ObjectId, String)],
    /// The user's columns; `None` for none.
    pub schema: Option<&'a Schema>,
    pub options: RowOptions,
}

/// What the takeoff side of the list is built with, besides [`ListContext`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TakeoffContext<'a> {
    /// The units the quantity columns are shown in; chosen from the measurements when `None`
    /// ([`QuantityUnits::for_annotations`]).
    pub units: Option<QuantityUnits>,
    /// Spaces besides the ones marked among the annotations ([`crate::set_space_name`]).
    pub spaces: &'a [Space],
    /// The id of the user column that gives the height for Wall Area. When `None`, the first
    /// number column titled `Height` (case ignored).
    pub height_column: Option<&'a str>,
    /// The unit heights are typed in; the length unit of `units` (feet for feet and inches)
    /// when `None`.
    pub height_unit: Option<pl_geom::Unit>,
}

/// A row of the markups list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: AnnotId,
    /// Position of the annotation in the input.
    pub source: usize,
    /// The row this one replies to.
    pub parent: Option<usize>,
    /// The replies made directly to this row, oldest first (input order).
    pub children: Vec<usize>,
    /// 0 for a markup, 1 for a reply to it, 2 for a reply to that reply, and so on.
    pub depth: usize,
    /// Zero-based page index.
    pub page: usize,
    pub page_label: String,
    pub kind: AnnotKind,
    /// The kind as the list names it (`Rectangle`, `Callout`, `Note`, ...).
    pub kind_name: String,
    pub subject: String,
    pub author: String,
    pub comment: String,
    pub created: Option<PdfDate>,
    pub modified: Option<PdfDate>,
    pub colour: Option<[u8; 3]>,
    pub fill: Option<[u8; 3]>,
    pub layer: String,
    /// The review status, when the latest status is one of the standard review states;
    /// [`ReviewState::None`] otherwise.
    pub status: ReviewState,
    /// The state of the markup's latest status of any model but the checkmark's (a review
    /// state or a custom status set's, such as Bluebeam's `In Progress`); empty for none (and
    /// for the state `None`). This is what the Status column shows.
    pub status_name: String,
    /// The `/StateModel` of that status (`Review`, or a custom set's name); empty for none.
    pub status_model: String,
    pub checked: bool,
    pub locked: bool,
    /// Replies in the whole thread below this row.
    pub replies: usize,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// Measured quantities, in the units of [`Table::units`]; `None` for a markup that is not
    /// a measurement of a kind that has the quantity (see [`Builtin`]).
    pub length: Option<f64>,
    pub area: Option<f64>,
    pub count: Option<f64>,
    pub perimeter: Option<f64>,
    pub volume: Option<f64>,
    pub wall_area: Option<f64>,
    /// Degrees.
    pub angle: Option<f64>,
    pub radius: Option<f64>,
    pub diameter: Option<f64>,
    /// The base area of a Volume (never in `area`).
    pub base_area: Option<f64>,
    pub measure_depth: Option<f64>,
    /// The scale as the measurement's `/Measure` states it.
    pub scale: String,
    /// The unit of the measurement's main quantity.
    pub unit: String,
    /// The name of the space the markup is in.
    pub space: String,
    /// The point that decides the space ([`crate::anchor`]).
    pub anchor: Option<Point>,
    /// The measurement in SI units, for a measurement.
    pub measure: Option<RowMeasure>,
    /// One value per user column, in schema order. A formula column holds its result, or the
    /// [`FormulaError::code`] of its error as text.
    pub custom: Vec<Value>,
    /// The errors of the row's formula cells, by index into `custom`.
    pub formula_errors: Vec<(usize, FormulaError)>,
    /// For a count shown as one row for its whole group ([`count_group_key`]): the other
    /// annotations of the group, in input order (`id` is the first). Empty otherwise.
    pub members: Vec<AnnotId>,
}

/// The key that puts count annotations into one row of the list: one count is one row with
/// the total of its items, however many annotations carry them (Revu writes one PolygonCount
/// annotation per item and groups them with the standard annotation group; Plumbline's own
/// count is one annotation holding many).
///
/// It is the count's group as pl-annot gives it ([`Annotation::count_group`]: the group's
/// first annotation, through `/IRT` + `/RT /Group`) with the page, so a group never spans
/// pages in the list. Counts that only look alike (same subject, paint and marker) are
/// separate counts, as the application that wrote them says.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CountGroupKey {
    page: usize,
    group: AnnotId,
}

/// The [`CountGroupKey`] of a count measurement; `None` for any other annotation, and for a
/// reply or a status (a group member's `/IRT` with `/RT /Group` is neither).
pub fn count_group_key(a: &Annotation) -> Option<CountGroupKey> {
    if a.is_reply() || a.meta.state.is_some() {
        return None;
    }
    Some(CountGroupKey {
        page: a.page,
        group: a.count_group()?,
    })
}

/// A [`ColumnId`] looked up once against a [`Table`], for reading many rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    Builtin(Builtin),
    Custom(usize),
    /// A column the table does not have (a user column that was removed). Always empty.
    Missing,
}

fn u8s(c: Rgb) -> [u8; 3] {
    c.to_u8()
}

fn kind_name(a: &Annotation) -> String {
    if let Some(t) = &a.free_text {
        return match t.intent {
            FreeTextIntent::Box => "Text Box",
            FreeTextIntent::Callout => "Callout",
            FreeTextIntent::TypeWriter => "Typewriter",
        }
        .to_string();
    }
    // A count is a Count whatever annotation carries its items (Ink, Revu's PolygonCount
    // Polygons, ...).
    if a.measure
        .as_ref()
        .is_some_and(|m| m.kind == MeasureKind::Count)
    {
        return "Count".to_string();
    }
    match &a.kind {
        AnnotKind::FreeText => "Text Box".to_string(),
        kind if a.is_reply() && *kind == AnnotKind::Text => "Reply".to_string(),
        kind => kind.default_subject().to_string(),
    }
}

impl Row {
    fn opt_number(n: Option<f64>) -> Value {
        n.map_or(Value::Empty, Value::number)
    }

    /// The value of a column.
    pub fn value(&self, col: Col) -> Value {
        let b = match col {
            Col::Builtin(b) => b,
            Col::Custom(i) => return self.custom.get(i).cloned().unwrap_or_default(),
            Col::Missing => return Value::Empty,
        };
        match b {
            Builtin::Subject => Value::text(self.subject.as_str()),
            Builtin::Page => Value::Number(self.page as f64 + 1.0),
            Builtin::PageLabel => Value::text(self.page_label.as_str()),
            Builtin::Kind => Value::text(self.kind_name.as_str()),
            Builtin::Author => Value::text(self.author.as_str()),
            Builtin::Created => self.created.map_or(Value::Empty, Value::Date),
            Builtin::Modified => self.modified.map_or(Value::Empty, Value::Date),
            Builtin::Comment => Value::text(self.comment.as_str()),
            Builtin::Colour => self.colour.map_or(Value::Empty, Value::Colour),
            Builtin::Fill => self.fill.map_or(Value::Empty, Value::Colour),
            Builtin::Layer => Value::text(self.layer.as_str()),
            Builtin::Status => Value::text(self.status_name.as_str()),
            Builtin::Checkmark => Value::Bool(self.checked),
            Builtin::Locked => Value::Bool(self.locked),
            Builtin::Replies => Value::Number(self.replies as f64),
            Builtin::X => Value::number(self.x),
            Builtin::Y => Value::number(self.y),
            Builtin::Width => Value::number(self.width),
            Builtin::Height => Value::number(self.height),
            Builtin::Length => Row::opt_number(self.length),
            Builtin::Area => Row::opt_number(self.area),
            Builtin::Count => Row::opt_number(self.count),
            Builtin::Perimeter => Row::opt_number(self.perimeter),
            Builtin::Volume => Row::opt_number(self.volume),
            Builtin::WallArea => Row::opt_number(self.wall_area),
            Builtin::Angle => Row::opt_number(self.angle),
            Builtin::Radius => Row::opt_number(self.radius),
            Builtin::Diameter => Row::opt_number(self.diameter),
            Builtin::BaseArea => Row::opt_number(self.base_area),
            Builtin::Depth => Row::opt_number(self.measure_depth),
            Builtin::Scale => Value::text(self.scale.as_str()),
            Builtin::Unit => Value::text(self.unit.as_str()),
            Builtin::Space => Value::text(self.space.as_str()),
        }
    }

    /// The error of a formula cell, by index into `custom`.
    pub fn formula_error(&self, custom: usize) -> Option<&FormulaError> {
        self.formula_errors
            .iter()
            .find(|(c, _)| *c == custom)
            .map(|(_, e)| e)
    }

    /// Wall Area: the row's length (else its outline: a Perimeter, or an Area's or Volume's
    /// outer ring) times `height` (in metres), in the area unit of `units`; empty without
    /// either.
    pub(crate) fn set_wall_area(&mut self, units: &QuantityUnits, height: Option<f64>) {
        let m = self.measure.as_ref();
        let run = m
            .and_then(RowMeasure::length)
            .or_else(|| m.and_then(RowMeasure::outline));
        self.wall_area = run.zip(height).map(|(l, h)| units.area(l * h));
    }

    /// Fills the quantity columns from [`Row::measure`] in `units`. `height` is the row's
    /// height in metres, for Wall Area.
    fn fill_quantities(&mut self, units: &QuantityUnits, height: Option<f64>) {
        use crate::QuantityKind as Q;
        let m = self.measure.as_ref();
        let q = m.map(|m| m.quantities).unwrap_or_default();
        let length = m.and_then(RowMeasure::length);
        let perimeter = m.and_then(RowMeasure::perimeter);
        self.length = length.map(|v| units.length(v));
        self.perimeter = perimeter.map(|v| units.length(v));
        self.radius = m.and_then(RowMeasure::radius).map(|v| units.length(v));
        self.diameter = m.and_then(RowMeasure::diameter).map(|v| units.length(v));
        self.measure_depth = m.and_then(|m| m.depth).map(|v| units.length(v));
        // A Volume's area is its base: shown on its own so that Area adds up areas only.
        self.area = m.and_then(RowMeasure::area).map(|v| units.area(v));
        self.base_area = m.and_then(RowMeasure::base_area).map(|v| units.area(v));
        self.volume = q.volume.map(|v| units.volume(v));
        self.angle = q.angle.map(f64::to_degrees);
        self.count = q.count.map(|c| c as f64);
        self.scale = m.map(|m| m.scale.clone()).unwrap_or_default();
        self.unit = match m.map(|m| m.kind) {
            None => String::new(),
            Some(kind) => {
                let k = match kind {
                    MeasureKind::Area => Q::Area,
                    MeasureKind::Volume if q.volume.is_some() => Q::Volume,
                    MeasureKind::Volume => Q::Area,
                    MeasureKind::Angle => Q::Angle,
                    MeasureKind::Count => Q::Count,
                    _ => Q::Length,
                };
                units.label(k).to_string()
            }
        };
        self.set_wall_area(units, height);
    }

    /// Appends the column's text as [`Value::display`] gives it, without building a `Value`
    /// for the text columns.
    pub fn display_into(&self, col: Col, out: &mut String) {
        match col {
            Col::Builtin(Builtin::Subject) => out.push_str(&self.subject),
            Col::Builtin(Builtin::PageLabel) => out.push_str(&self.page_label),
            Col::Builtin(Builtin::Kind) => out.push_str(&self.kind_name),
            Col::Builtin(Builtin::Author) => out.push_str(&self.author),
            Col::Builtin(Builtin::Comment) => out.push_str(&self.comment),
            Col::Builtin(Builtin::Layer) => out.push_str(&self.layer),
            Col::Builtin(Builtin::Space) => out.push_str(&self.space),
            Col::Custom(i) => {
                if let Some(v) = self.custom.get(i) {
                    v.display_into(out);
                }
            }
            other => self.value(other).display_into(out),
        }
    }
}

/// The rows of a document's markups list.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub rows: Vec<Row>,
    /// The rows that are not replies, in input order.
    pub roots: Vec<usize>,
    /// The user columns the rows hold values for.
    pub custom: Vec<CustomColumn>,
    /// The units the quantity columns are in.
    pub units: QuantityUnits,
    /// The spaces markups are assigned to.
    pub spaces: Vec<Space>,
    /// The user column (index into `custom`) Wall Area takes its height from.
    pub height_column: Option<usize>,
    /// The unit of the heights in that column. It stays when [`Table::set_units`] changes the
    /// list's units, so a height keeps its meaning.
    pub height_unit: pl_geom::Unit,
    /// Each formula column by id, with the problem that makes every one of its cells an error
    /// (it does not parse, or it is part of a loop); `None` when it works.
    pub formula_status: Vec<(String, Option<FormulaError>)>,
}

impl Table {
    /// Builds the rows from every annotation of a document (all pages, in stacking order, as
    /// `AnnotSet::iter` gives them).
    ///
    /// * Popups are never rows; links only with [`RowOptions::include_links`], or when a markup
    ///   application placed them: a link with Bluebeam column values (`/BSIColumnData`) or a
    ///   subject (`/Subj`) is listed as a markup, as Revu lists its hyperlinks.
    /// * A status annotation (`/State`) is not a row: the latest one of any model but `Marked`
    ///   (the review model or a custom status set) is its markup's [`Row::status_name`] (and
    ///   [`Row::status`] when it is a standard review state), and the latest `Marked` one its
    ///   [`Row::checked`] ("latest" by `/M`, input order when the dates tie or are missing).
    /// * A reply (`/IRT` with `/RT` absent or `R`) is a child row of the row it answers. A
    ///   reply whose target is not a row, or that is part of a reply cycle, is an ordinary
    ///   row. An `/RT /Group` member is an ordinary row too.
    /// * A count is one row however many annotations carry its items: the count annotations
    ///   that share a [`count_group_key`] are one row, with the first one's id, comment and
    ///   columns, the item total in Count, the union of their boxes, and the others' ids in
    ///   [`Row::members`]. Replies to any of them are replies to that row.
    ///
    /// Quantities, spaces and formulas are filled in as [`Table::build_takeoff`] does with a
    /// default [`TakeoffContext`].
    pub fn build<'a, I>(annotations: I, ctx: &ListContext<'_>) -> Table
    where
        I: IntoIterator<Item = &'a Annotation>,
    {
        Table::build_takeoff(annotations, ctx, &TakeoffContext::default())
    }

    /// [`Table::build`], with the takeoff side given explicitly:
    ///
    /// * the quantity columns of every measurement, in `takeoff.units` (see
    ///   [`QuantityUnits::for_annotations`] when it is `None`);
    /// * the Space of every markup, from `takeoff.spaces` and the spaces marked among the
    ///   annotations (see [`crate::space`]);
    /// * the value of every formula column, in dependency order ([`Table::recalculate`]).
    pub fn build_takeoff<'a, I>(
        annotations: I,
        ctx: &ListContext<'_>,
        takeoff: &TakeoffContext<'_>,
    ) -> Table
    where
        I: IntoIterator<Item = &'a Annotation>,
    {
        let all: Vec<&Annotation> = annotations.into_iter().collect();
        let custom: Vec<CustomColumn> = ctx.schema.map(|s| s.columns.clone()).unwrap_or_default();
        let layers: HashMap<ObjectId, &str> =
            ctx.layers.iter().map(|(id, n)| (*id, n.as_str())).collect();

        // Status annotations: the latest per target and model.
        struct Latest<'s> {
            stamp: Option<i64>,
            state: &'s str,
            model: &'s str,
        }
        // Every status model but the checkmark's shares one latest status (a markup moved from
        // a review state to a custom set's state shows the custom one, and back).
        let mut review: HashMap<&AnnotId, Latest<'_>> = HashMap::new();
        let mut marked: HashMap<&AnnotId, Latest<'_>> = HashMap::new();
        for a in &all {
            let (Some(target), Some(state)) = (&a.meta.reply_to, a.meta.state.as_deref()) else {
                continue;
            };
            let Some(model) = a.meta.state_model.as_deref() else {
                continue;
            };
            let map = match model {
                MARKED_MODEL => &mut marked,
                _ => &mut review,
            };
            let stamp = a.meta.modified.map(|d| d.to_unix());
            let newer = match map.get(target) {
                None => true,
                // Later in the input wins unless both are dated and this one is older.
                Some(old) => !matches!((old.stamp, stamp), (Some(o), Some(n)) if n < o),
            };
            if newer {
                map.insert(
                    target,
                    Latest {
                        stamp,
                        state,
                        model,
                    },
                );
            }
        }

        let mut rows: Vec<Row> = Vec::new();
        let mut row_of: HashMap<&AnnotId, usize> = HashMap::new();
        let mut reply_targets: Vec<Option<&AnnotId>> = Vec::new();
        // The row of each count group so far ([`count_group_key`]).
        let mut count_groups: Vec<(CountGroupKey, usize)> = Vec::new();
        for (source, a) in all.iter().enumerate() {
            let is_status = a.meta.reply_to.is_some() && a.meta.state.is_some();
            if a.kind == AnnotKind::Popup
                || is_status
                || (a.kind == AnnotKind::Link
                    && !ctx.options.include_links
                    && a.column_data.is_none()
                    && a.meta.subject.is_none())
                || (a.meta.flags.is_hidden() && !ctx.options.include_hidden)
            {
                continue;
            }
            let b = a.geometry.bounds();
            let finite = |v: f64| if v.is_finite() { v } else { 0.0 };
            let measure = RowMeasure::of(a);
            // A count whose group already has a row adds its items to that row.
            let group_key = measure.as_ref().and_then(|_| count_group_key(a));
            if let Some(key) = &group_key {
                if let Some(&(_, at)) = count_groups.iter().find(|(k, _)| k == key) {
                    let row: &mut Row = &mut rows[at];
                    let items = measure.as_ref().and_then(|m| m.quantities.count);
                    if let Some(m) = row.measure.as_mut() {
                        m.quantities.count = match (m.quantities.count, items) {
                            (Some(x), Some(y)) => Some(x + y),
                            (x, y) => x.or(y),
                        };
                    }
                    let (x0, y0) = (row.x.min(finite(b.x0)), row.y.min(finite(b.y0)));
                    let x1 = (row.x + row.width).max(finite(b.x0) + finite(b.width()));
                    let y1 = (row.y + row.height).max(finite(b.y0) + finite(b.height()));
                    (row.x, row.y, row.width, row.height) = (x0, y0, x1 - x0, y1 - y0);
                    row.members.push(a.id.clone());
                    // Replies to any item of the count belong to its row.
                    row_of.entry(&a.id).or_insert(at);
                    continue;
                }
            }
            let page_label = ctx
                .page_labels
                .get(a.page)
                .filter(|l| !l.is_empty())
                .cloned()
                .unwrap_or_else(|| (a.page + 1).to_string());
            let layer = a.meta.layer.map_or(String::new(), |id| {
                layers
                    .get(&id)
                    .map_or_else(|| format!("Layer {}", id.0), |n| n.to_string())
            });
            let row = Row {
                id: a.id.clone(),
                source,
                parent: None,
                children: Vec::new(),
                depth: 0,
                page: a.page,
                page_label,
                kind: a.kind.clone(),
                kind_name: kind_name(a),
                subject: a.meta.subject.clone().unwrap_or_default(),
                author: a.meta.author.clone().unwrap_or_default(),
                comment: a.meta.contents.clone().unwrap_or_default(),
                created: a.meta.created,
                modified: a.meta.modified,
                colour: match (&a.free_text, a.style.stroke) {
                    // A text box with no border is named by its text colour.
                    (Some(t), None) => Some(u8s(t.color)),
                    (_, c) => c.map(u8s),
                },
                fill: a.style.fill.map(u8s),
                layer,
                status: review
                    .get(&a.id)
                    .filter(|l| l.model == pl_annot::REVIEW_MODEL)
                    .and_then(|l| ReviewState::from_name(l.state))
                    .unwrap_or(ReviewState::None),
                status_name: review
                    .get(&a.id)
                    .map(|l| l.state.trim())
                    .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"))
                    .unwrap_or_default()
                    .to_string(),
                status_model: review
                    .get(&a.id)
                    .map(|l| l.model.to_string())
                    .unwrap_or_default(),
                checked: marked.get(&a.id).is_some_and(|l| l.state == MARKED),
                locked: a.meta.flags.has(Flags::LOCKED) || a.meta.flags.has(Flags::READ_ONLY),
                replies: 0,
                x: finite(b.x0),
                y: finite(b.y0),
                width: finite(b.width()),
                height: finite(b.height()),
                length: None,
                area: None,
                count: None,
                perimeter: None,
                volume: None,
                wall_area: None,
                angle: None,
                radius: None,
                diameter: None,
                base_area: None,
                measure_depth: None,
                scale: String::new(),
                unit: String::new(),
                space: String::new(),
                anchor: anchor(a),
                measure,
                custom: custom.iter().map(|c| custom_value(a, c)).collect(),
                formula_errors: Vec::new(),
                members: Vec::new(),
            };
            if let Some(key) = group_key {
                count_groups.push((key, rows.len()));
            }
            // The first annotation with an id is the one replies to that id attach to.
            row_of.entry(&a.id).or_insert(rows.len());
            reply_targets.push(a.is_reply().then_some(a.meta.reply_to.as_ref()).flatten());
            rows.push(row);
        }

        // Thread structure. A link that would close a cycle is dropped.
        let mut parent: Vec<Option<usize>> = reply_targets
            .iter()
            .enumerate()
            .map(|(i, t)| t.and_then(|id| row_of.get(id).copied()).filter(|p| *p != i))
            .collect();
        // 0 = not visited, 1 = on the current walk, 2 = done.
        let mut mark = vec![0u8; rows.len()];
        for start in 0..rows.len() {
            let mut path = Vec::new();
            let mut at = start;
            loop {
                match mark[at] {
                    2 => break,
                    1 => {
                        // `at` is reached again on this walk: cut the link that led back.
                        if let Some(last) = path.last() {
                            parent[*last] = None;
                        }
                        break;
                    }
                    _ => {}
                }
                mark[at] = 1;
                path.push(at);
                match parent[at] {
                    Some(p) => at = p,
                    None => break,
                }
            }
            for i in path {
                mark[i] = 2;
            }
        }
        for i in 0..rows.len() {
            rows[i].parent = parent[i];
            if let Some(p) = parent[i] {
                rows[p].children.push(i);
            }
        }
        let roots: Vec<usize> = (0..rows.len()).filter(|i| parent[*i].is_none()).collect();
        // Depth and reply counts, walking each thread once.
        let mut order = Vec::with_capacity(rows.len());
        let mut stack: Vec<usize> = roots.iter().rev().copied().collect();
        while let Some(i) = stack.pop() {
            order.push(i);
            let depth = rows[i].depth + 1;
            for c in rows[i].children.clone().into_iter().rev() {
                rows[c].depth = depth;
                stack.push(c);
            }
        }
        for i in order.into_iter().rev() {
            if let Some(p) = rows[i].parent {
                rows[p].replies += rows[i].replies + 1;
            }
        }
        let units = takeoff
            .units
            .unwrap_or_else(|| QuantityUnits::for_annotations(all.iter().copied()));
        let height_column = match takeoff.height_column {
            Some(id) => custom.iter().position(|c| c.id == id),
            None => custom.iter().position(|c| {
                c.kind == ColumnType::Number && c.title.trim().eq_ignore_ascii_case("height")
            }),
        };
        let mut spaces: Vec<Space> = takeoff.spaces.to_vec();
        for s in spaces_in(all.iter().copied()) {
            if !spaces
                .iter()
                .any(|t| t.source.is_some() && t.source == s.source)
            {
                spaces.push(s);
            }
        }
        let height_unit = takeoff.height_unit.unwrap_or(match units.length {
            pl_geom::Unit::FeetInches => pl_geom::Unit::Foot,
            u => u,
        });
        let mut table = Table {
            rows,
            roots,
            custom,
            units,
            spaces: Vec::new(),
            height_column,
            height_unit,
            formula_status: Vec::new(),
        };
        table.fill_quantities();
        table.assign_spaces(spaces);
        table
    }

    fn fill_quantities(&mut self) {
        let units = self.units;
        // A formula's height is not known until the formulas run: Wall Area is filled then
        // ([`Table::recalculate`]), never from the last pass's value.
        let height = self.height_column.filter(|i| {
            self.custom
                .get(*i)
                .is_some_and(|c| c.kind != ColumnType::Formula)
        });
        let height_unit = self.height_unit;
        for row in &mut self.rows {
            let h = height
                .and_then(|i| row.custom.get(i))
                .and_then(Value::as_number)
                .map(|v| height_unit.to_metres(v));
            row.fill_quantities(&units, h);
        }
    }

    /// Shows the quantity columns in other units, and evaluates the formulas again (they see
    /// quantities in the list's units).
    pub fn set_units(&mut self, units: QuantityUnits) {
        self.units = units;
        self.fill_quantities();
        self.recalculate();
    }

    /// Assigns every row to one of `spaces` (see [`crate::space`]) and evaluates the formulas
    /// again.
    pub fn assign_spaces(&mut self, spaces: Vec<Space>) {
        self.spaces = spaces;
        for row in &mut self.rows {
            row.space = row
                .anchor
                .and_then(|p| space_at(&self.spaces, row.page, p, Some(&row.id)))
                .map(|s| s.name.clone())
                .unwrap_or_default();
        }
        self.recalculate();
    }

    /// Evaluates every formula column of every row again, in dependency order: a formula that
    /// uses another formula column's value sees it already worked out. A formula column that
    /// does not parse, or that is part of a loop of formula columns, has that error in every
    /// cell (and in [`Table::formula_status`]); any other error is per cell.
    pub fn recalculate(&mut self) {
        crate::calc::recalculate(self);
    }

    /// The error of one formula cell.
    pub fn formula_error(&self, row: usize, column: &ColumnId) -> Option<&FormulaError> {
        match self.col(column) {
            Col::Custom(i) => self.rows.get(row)?.formula_error(i),
            _ => None,
        }
    }

    /// The label of the unit a column is shown in, for the quantity columns.
    pub fn unit_label(&self, column: &ColumnId) -> Option<&'static str> {
        match column {
            ColumnId::Builtin(b) => b.quantity_kind().map(|k| self.units.label(k)),
            ColumnId::Custom(_) => None,
        }
    }

    /// The heading with the unit, as exports write it: `Length (ft)`.
    pub fn heading(&self, column: &ColumnId) -> String {
        match self.unit_label(column) {
            Some(unit) => format!("{} ({unit})", self.title(column)),
            None => self.title(column),
        }
    }

    /// The decimal places a number column is written with, when it has a fixed number.
    pub fn places(&self, column: &ColumnId) -> Option<u8> {
        match column {
            ColumnId::Builtin(b) => b.quantity_kind().map(|k| self.units.places(k)),
            ColumnId::Custom(id) => self
                .custom
                .iter()
                .find(|c| c.id == *id)
                .and_then(|c| c.display.as_ref())
                .map(|d| d.places),
        }
    }

    /// A cell as the list shows it: quantities in their unit's places (feet and inches as
    /// such), a user column's numbers with its [`crate::NumberDisplay`], a formula error by its
    /// code, anything else as [`Value::display`].
    pub fn cell_text(&self, row: usize, column: &ColumnId) -> String {
        let Some(r) = self.rows.get(row) else {
            return String::new();
        };
        let col = self.col(column);
        let value = r.value(col);
        match (column, &value, col) {
            (ColumnId::Builtin(b), Value::Number(n), _) => match b.quantity_kind() {
                Some(k) => self.units.number_text(k, *n),
                None => value.display(),
            },
            (_, _, Col::Custom(i)) => self.custom[i].display(&value),
            _ => value.display(),
        }
    }

    /// Looks a column up for [`Row::value`].
    pub fn col(&self, column: &ColumnId) -> Col {
        match column {
            ColumnId::Builtin(b) => Col::Builtin(*b),
            ColumnId::Custom(id) => self
                .custom
                .iter()
                .position(|c| c.id == *id)
                .map_or(Col::Missing, Col::Custom),
        }
    }

    /// The value of one cell.
    pub fn value(&self, row: usize, column: &ColumnId) -> Value {
        self.rows
            .get(row)
            .map_or(Value::Empty, |r| r.value(self.col(column)))
    }

    /// The column heading; the id itself for a user column the table does not have.
    pub fn title(&self, column: &ColumnId) -> String {
        match column {
            ColumnId::Builtin(b) => b.title().to_string(),
            ColumnId::Custom(id) => self
                .custom
                .iter()
                .find(|c| c.id == *id)
                .map_or_else(|| id.clone(), |c| c.title.clone()),
        }
    }

    /// The column's type; `Text` for a user column the table does not have.
    pub fn column_type(&self, column: &ColumnId) -> ColumnType {
        match column {
            ColumnId::Builtin(b) => b.column_type(),
            ColumnId::Custom(id) => self
                .custom
                .iter()
                .find(|c| c.id == *id)
                .map_or(ColumnType::Text, |c| c.kind),
        }
    }

    /// True for the columns that groups total: the adding quantity columns
    /// ([`Builtin::is_quantity`]) and the user's number and formula columns (unless one has its
    /// subtotal switched off). A formula cell adds in when it holds a number.
    pub fn is_summed(&self, column: &ColumnId) -> bool {
        match column {
            ColumnId::Builtin(b) => b.is_quantity(),
            ColumnId::Custom(id) => self.custom.iter().any(|c| {
                c.id == *id
                    && matches!(c.kind, ColumnType::Number | ColumnType::Formula)
                    && c.subtotal
            }),
        }
    }

    /// Every column the table has: the built-in ones, then the user's.
    pub fn all_columns(&self) -> Vec<ColumnId> {
        Builtin::ALL
            .into_iter()
            .map(ColumnId::Builtin)
            .chain(self.custom.iter().map(CustomColumn::column_id))
            .collect()
    }

    /// The summed columns ([`Table::is_summed`]), in [`Table::all_columns`] order.
    pub fn summed_columns(&self) -> Vec<ColumnId> {
        self.all_columns()
            .into_iter()
            .filter(|c| self.is_summed(c))
            .collect()
    }

    /// The row of an annotation: for an item of a count, its count's row.
    pub fn row_of(&self, id: &AnnotId) -> Option<usize> {
        self.rows
            .iter()
            .position(|r| r.id == *id)
            .or_else(|| self.rows.iter().position(|r| r.members.contains(id)))
    }

    /// A row's thread below it, each reply after what it answers.
    pub fn thread(&self, row: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = match self.rows.get(row) {
            Some(r) => r.children.iter().rev().copied().collect(),
            None => return out,
        };
        while let Some(i) = stack.pop() {
            out.push(i);
            stack.extend(self.rows[i].children.iter().rev().copied());
        }
        out
    }
}
