//! Columns: the built-in ones, the user's own (a small per-document schema), and where the
//! value of a user column lives in an annotation.
//!
//! # Storage
//!
//! pl-annot writes an annotation's private application data as one dictionary under the single
//! key `PlumblineData` ([`pl_annot::PRIVATE_KEY`]), exposed as `Annotation::app_data`. The
//! values of the user's columns are the dictionary [`COLUMNS_KEY`] inside it, keyed by column
//! id:
//!
//! ```text
//! /PlumblineData << /Columns << /trade (Electrical) /qty 12 /done true /due (D:20261201) >> >>
//! ```
//!
//! Text and choice values are PDF text strings, a number is an integer, a real when that is
//! exact, or else its decimal digits as a string (a PDF real is single precision), a checkmark
//! is a boolean and a date a PDF date string. An empty value has no entry, and an annotation
//! with no values has no `/Columns` (and no `/PlumblineData` if nothing else is in it).

use pl_annot::{decode_text, encode_text, Annotation, PdfDate};
use pl_pdf::{Dictionary, Object, StringFormat};
use serde::{Deserialize, Serialize};

use crate::value::{format_date, is_day, parse_date, Value};

/// The key inside `Annotation::app_data` that holds the values of the user's columns.
pub const COLUMNS_KEY: &str = "Columns";

/// What a column holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColumnType {
    Text,
    Number,
    /// One of a fixed list of options.
    Choice,
    Checkmark,
    Date,
    /// Built-in columns only.
    Colour,
    /// Worked out per row from a formula ([`crate::formula`]); never typed in.
    Formula,
}

impl ColumnType {
    /// The types a user column can have.
    pub const CUSTOM: [ColumnType; 5] = [
        ColumnType::Text,
        ColumnType::Number,
        ColumnType::Choice,
        ColumnType::Checkmark,
        ColumnType::Date,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ColumnType::Text => "Text",
            ColumnType::Number => "Number",
            ColumnType::Choice => "Choice",
            ColumnType::Checkmark => "Checkmark",
            ColumnType::Date => "Date",
            ColumnType::Colour => "Colour",
            ColumnType::Formula => "Formula",
        }
    }
}

/// A built-in column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Builtin {
    Subject,
    /// The page number, counted from 1.
    Page,
    /// The page's label (`iv`, `A-101`), the page number when the document has none.
    PageLabel,
    Kind,
    Author,
    Created,
    Modified,
    /// `/Contents`.
    Comment,
    /// The line, border or highlight colour.
    Colour,
    /// The interior colour.
    Fill,
    Layer,
    /// The review status (`Accepted`, `Rejected`, ...); empty for none.
    Status,
    /// The checkmark (`/StateModel (Marked)`).
    Checkmark,
    Locked,
    /// How many replies hang off the markup, replies to replies included.
    Replies,
    /// Left edge, in page units (unrotated page space).
    X,
    /// Bottom edge, in page units (unrotated page space, y up).
    Y,
    Width,
    Height,
    /// Measured length: of a Length or Polylength measurement. No value until measurements
    /// exist.
    Length,
    /// Measured area: of an Area measurement (a Volume's is in [`Builtin::BaseArea`]). No
    /// value until measurements exist.
    Area,
    /// The number of markers of a Count measurement.
    Count,
    /// The perimeter of a Perimeter measurement. An Area's or Volume's outline is not in it,
    /// so the total holds perimeters alone.
    Perimeter,
    /// The volume of a Volume measurement: its net area times its depth.
    Volume,
    /// The length (or the outline of a Perimeter, Area or Volume) of a measurement times the
    /// row's height, from the user
    /// column the takeoff names as the height (by default one titled `Height`).
    WallArea,
    /// The angle of an Angle measurement, in degrees.
    Angle,
    /// The radius of a Radius measurement, half the diameter of a Diameter.
    Radius,
    /// The depth of a Volume measurement.
    Depth,
    /// The scale a measurement was taken at, as its `/Measure` states it.
    Scale,
    /// The unit of the measurement's main quantity, as the list shows it.
    Unit,
    /// The name of the space the markup lies in (see [`crate::Space`]).
    Space,
    /// The diameter of a Diameter measurement, twice the radius of a Radius. Not summed, and
    /// (like Radius) not part of Length.
    Diameter,
    /// The base (plan) area of a Volume measurement: its net area, cutouts taken off. A
    /// Volume's area is shown here and never in Area, so Area totals only Area measurements.
    BaseArea,
}

impl Builtin {
    pub const ALL: [Builtin; 33] = [
        Builtin::Subject,
        Builtin::Page,
        Builtin::PageLabel,
        Builtin::Kind,
        Builtin::Author,
        Builtin::Created,
        Builtin::Modified,
        Builtin::Comment,
        Builtin::Colour,
        Builtin::Fill,
        Builtin::Layer,
        Builtin::Status,
        Builtin::Checkmark,
        Builtin::Locked,
        Builtin::Replies,
        Builtin::X,
        Builtin::Y,
        Builtin::Width,
        Builtin::Height,
        Builtin::Length,
        Builtin::Area,
        Builtin::Count,
        Builtin::Perimeter,
        Builtin::Volume,
        Builtin::BaseArea,
        Builtin::WallArea,
        Builtin::Angle,
        Builtin::Radius,
        Builtin::Diameter,
        Builtin::Depth,
        Builtin::Scale,
        Builtin::Unit,
        Builtin::Space,
    ];

    /// The column heading.
    pub fn title(self) -> &'static str {
        match self {
            Builtin::Subject => "Subject",
            Builtin::Page => "Page",
            Builtin::PageLabel => "Page Label",
            Builtin::Kind => "Type",
            Builtin::Author => "Author",
            Builtin::Created => "Created",
            Builtin::Modified => "Modified",
            Builtin::Comment => "Comment",
            Builtin::Colour => "Colour",
            Builtin::Fill => "Fill",
            Builtin::Layer => "Layer",
            Builtin::Status => "Status",
            Builtin::Checkmark => "Checked",
            Builtin::Locked => "Locked",
            Builtin::Replies => "Replies",
            Builtin::X => "X",
            Builtin::Y => "Y",
            Builtin::Width => "Width",
            Builtin::Height => "Height",
            Builtin::Length => "Length",
            Builtin::Area => "Area",
            Builtin::Count => "Count",
            Builtin::Perimeter => "Perimeter",
            Builtin::Volume => "Volume",
            Builtin::WallArea => "Wall Area",
            Builtin::Angle => "Angle",
            Builtin::Radius => "Radius",
            Builtin::Depth => "Depth",
            Builtin::Scale => "Scale",
            Builtin::Unit => "Unit",
            Builtin::Space => "Space",
            Builtin::Diameter => "Diameter",
            Builtin::BaseArea => "Base Area",
        }
    }

    pub fn column_type(self) -> ColumnType {
        match self {
            Builtin::Subject
            | Builtin::PageLabel
            | Builtin::Kind
            | Builtin::Author
            | Builtin::Comment
            | Builtin::Layer
            | Builtin::Status
            | Builtin::Scale
            | Builtin::Unit
            | Builtin::Space => ColumnType::Text,
            Builtin::Page
            | Builtin::Replies
            | Builtin::X
            | Builtin::Y
            | Builtin::Width
            | Builtin::Height
            | Builtin::Length
            | Builtin::Area
            | Builtin::Count
            | Builtin::Perimeter
            | Builtin::Volume
            | Builtin::WallArea
            | Builtin::Angle
            | Builtin::Radius
            | Builtin::Diameter
            | Builtin::BaseArea
            | Builtin::Depth => ColumnType::Number,
            Builtin::Created | Builtin::Modified => ColumnType::Date,
            Builtin::Colour | Builtin::Fill => ColumnType::Colour,
            Builtin::Checkmark | Builtin::Locked => ColumnType::Checkmark,
        }
    }

    /// True for the quantity columns that add up: the ones a group shows a subtotal for
    /// (Length, Area, Count, Perimeter, Volume, Base Area, Wall Area). Angle, Radius, Diameter
    /// and Depth are quantities too ([`Builtin::quantity_kind`]) but a sum of them means
    /// nothing.
    pub fn is_quantity(self) -> bool {
        matches!(
            self,
            Builtin::Length
                | Builtin::Area
                | Builtin::Count
                | Builtin::Perimeter
                | Builtin::Volume
                | Builtin::BaseArea
                | Builtin::WallArea
        )
    }

    /// The kind of quantity a measured column holds, which decides its unit.
    pub fn quantity_kind(self) -> Option<crate::QuantityKind> {
        use crate::QuantityKind as Q;
        Some(match self {
            Builtin::Length
            | Builtin::Perimeter
            | Builtin::Radius
            | Builtin::Diameter
            | Builtin::Depth => Q::Length,
            Builtin::Area | Builtin::BaseArea | Builtin::WallArea => Q::Area,
            Builtin::Volume => Q::Volume,
            Builtin::Angle => Q::Angle,
            Builtin::Count => Q::Count,
            _ => return None,
        })
    }

    /// Whether a new layout shows the column.
    pub fn default_visible(self) -> bool {
        matches!(
            self,
            Builtin::Subject
                | Builtin::PageLabel
                | Builtin::Kind
                | Builtin::Author
                | Builtin::Modified
                | Builtin::Comment
                | Builtin::Colour
                | Builtin::Status
                | Builtin::Checkmark
        )
    }

    /// The width a new layout gives the column, in logical pixels.
    pub fn default_width(self) -> f32 {
        match self {
            Builtin::Comment => 260.0,
            Builtin::Subject | Builtin::Created | Builtin::Modified => 140.0,
            Builtin::Author | Builtin::Layer | Builtin::Kind => 110.0,
            Builtin::Page | Builtin::Checkmark | Builtin::Locked | Builtin::Replies => 64.0,
            _ => 84.0,
        }
    }
}

/// Names a column of the list.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColumnId {
    Builtin(Builtin),
    /// A user column, by its id in the [`Schema`].
    Custom(String),
}

impl From<Builtin> for ColumnId {
    fn from(b: Builtin) -> ColumnId {
        ColumnId::Builtin(b)
    }
}

impl ColumnId {
    pub fn custom(id: &str) -> ColumnId {
        ColumnId::Custom(id.to_string())
    }
}

/// What can go wrong defining a column or setting one of its values.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ColumnError {
    #[error("a column needs a name")]
    EmptyTitle,
    #[error("there is already a column called \"{0}\"")]
    DuplicateTitle(String),
    #[error("a column cannot be of type {0}")]
    TypeNotAllowed(&'static str),
    #[error("a choice column needs at least one option")]
    NoOptions,
    #[error("the option \"{0}\" is listed twice")]
    DuplicateOption(String),
    #[error("an option cannot be empty")]
    EmptyOption,
    #[error("there is no column \"{0}\"")]
    UnknownColumn(String),
    #[error(
        "column id \"{0}\" is not valid (letters, digits and underscores, starting with a letter)"
    )]
    BadId(String),
    #[error("column id \"{0}\" is used twice")]
    DuplicateId(String),
    #[error("\"{value}\" is not a {expected} (column \"{column}\")")]
    WrongType {
        column: String,
        expected: &'static str,
        value: String,
    },
    #[error("\"{value}\" is not one of the options of column \"{column}\"")]
    NotAnOption { column: String, value: String },
    /// A formula column is worked out, never typed in.
    #[error("column \"{0}\" is worked out by its formula and cannot be set")]
    Computed(String),
    #[error("the formula of column \"{column}\" cannot be used: {error}")]
    Formula {
        column: String,
        error: crate::formula::FormulaError,
    },
    #[error("the value of option \"{0}\" is not a finite number")]
    BadOptionValue(String),
    /// A Bluebeam column shown from what Bluebeam stored ([`crate::bluebeam`]).
    #[error("column \"{0}\" shows the value Bluebeam stored and cannot be changed here")]
    ReadOnly(String),
}

/// How a number column (or a formula's number) is written in the list: a fixed number of
/// decimal places with an optional prefix and suffix, e.g. `$` and two places for a cost.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NumberDisplay {
    pub places: u8,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suffix: String,
}

impl NumberDisplay {
    pub fn places(places: u8) -> NumberDisplay {
        NumberDisplay {
            places,
            ..NumberDisplay::default()
        }
    }

    /// Money: a prefix (`$`, `£`) and two places.
    pub fn currency(prefix: &str) -> NumberDisplay {
        NumberDisplay {
            places: 2,
            prefix: prefix.to_string(),
            suffix: String::new(),
        }
    }

    pub fn format(&self, n: f64) -> String {
        let digits = crate::quantity::fixed(n.abs(), self.places);
        let sign = if n < 0.0 && digits.chars().any(|c| c.is_ascii_digit() && c != '0') {
            "-"
        } else {
            ""
        };
        format!("{sign}{}{digits}{}", self.prefix, self.suffix)
    }
}

/// One user column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomColumn {
    /// The permanent id: the key the values are stored under. Renaming a column keeps it.
    pub id: String,
    /// The heading.
    pub title: String,
    pub kind: ColumnType,
    /// The options of a [`ColumnType::Choice`] column, in the order they are offered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Whether groups show a subtotal for a [`ColumnType::Number`] or [`ColumnType::Formula`]
    /// column.
    #[serde(default = "yes")]
    pub subtotal: bool,
    /// The number each option of a choice column stands for in a formula (a unit cost, a
    /// rate), in the order of `options`; empty when no option has one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub option_values: Vec<Option<f64>>,
    /// The formula of a [`ColumnType::Formula`] column.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub formula: String,
    /// How the column's numbers are written; the shortest exact decimal when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<NumberDisplay>,
    /// A [`ColumnType::Text`] column whose values may run over several lines (Bluebeam's
    /// "Multiline" text type). The list shows them on one line; the value keeps its line
    /// breaks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multiline: bool,
    /// Set for a column defined by the document itself in Bluebeam Revu's `/BSIAnnotColumns`:
    /// its values are then read from and written to each annotation's `/BSIColumnData` at the
    /// column's position, never to Plumbline's private storage (see [`crate::bluebeam`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bluebeam: Option<crate::bluebeam::BluebeamColumn>,
}

fn yes() -> bool {
    true
}

impl CustomColumn {
    pub fn column_id(&self) -> ColumnId {
        ColumnId::Custom(self.id.clone())
    }

    /// True for a column whose cells cannot be typed in: a formula, and a Bluebeam column
    /// shown from what Bluebeam stored (a formula Plumbline cannot work out the same way, or
    /// a column type it does not know).
    pub fn is_read_only(&self) -> bool {
        self.kind == ColumnType::Formula || self.bluebeam.as_ref().is_some_and(|b| b.stored)
    }

    fn read_only_error(&self) -> Option<ColumnError> {
        if self.kind == ColumnType::Formula {
            Some(ColumnError::Computed(self.title.clone()))
        } else if self.is_read_only() {
            Some(ColumnError::ReadOnly(self.title.clone()))
        } else {
            None
        }
    }

    /// The number an option of a choice column stands for, when it has one. `value` is
    /// matched to the options without regard to case.
    pub fn option_value(&self, value: &Value) -> Option<f64> {
        if self.kind != ColumnType::Choice {
            return None;
        }
        let at = self
            .options
            .iter()
            .position(|o| Value::text(o.as_str()).same(value))?;
        self.option_values.get(at).copied().flatten()
    }

    /// The cell's text as the list shows it: numbers with the column's [`NumberDisplay`].
    pub fn display(&self, value: &Value) -> String {
        match (value, &self.display) {
            (Value::Number(n), Some(d)) => d.format(*n),
            _ => value.display(),
        }
    }

    fn wrong(&self, value: &Value) -> ColumnError {
        ColumnError::WrongType {
            column: self.title.clone(),
            expected: match self.kind {
                ColumnType::Number => "number",
                ColumnType::Checkmark => "checkmark",
                ColumnType::Date => "date",
                _ => "text",
            },
            value: value.display(),
        }
    }

    /// Checks `value` against the column's type and returns it as it is stored: text of all
    /// white space becomes `Empty`, and a choice is matched to its option without regard to
    /// case. `Empty` is always accepted (it clears the cell).
    pub fn check(&self, value: Value) -> Result<Value, ColumnError> {
        if let Some(e) = self.read_only_error() {
            return Err(e);
        }
        if value.is_empty() {
            return Ok(Value::Empty);
        }
        match (self.kind, &value) {
            (ColumnType::Text, Value::Text(_))
            | (ColumnType::Checkmark, Value::Bool(_))
            | (ColumnType::Date, Value::Date(_)) => Ok(value),
            (ColumnType::Number, Value::Number(n)) if n.is_finite() => Ok(value),
            (ColumnType::Choice, Value::Text(s)) => self
                .options
                .iter()
                .find(|o| Value::text(o.as_str()).same(&value))
                .map(|o| Value::text(o.as_str()))
                .ok_or_else(|| ColumnError::NotAnOption {
                    column: self.title.clone(),
                    value: s.clone(),
                }),
            _ => Err(self.wrong(&value)),
        }
    }

    /// Reads what a user typed into a cell of this column: a number (a decimal comma is
    /// accepted, but not one that could as well be a thousands separator: `1,500` is refused), `yes`/`no`/`true`/`false`/`1`/`0`/`x` for a checkmark, a date as
    /// [`parse_date`] reads it, one of the options for a choice. Blank text is `Empty`.
    pub fn parse(&self, text: &str) -> Result<Value, ColumnError> {
        // A multi-line value keeps its inner line breaks and spaces.
        let t = if self.multiline {
            text.trim_matches([' ', '\t'])
        } else {
            text.trim()
        };
        if let Some(e) = self.read_only_error() {
            return Err(e);
        }
        if t.is_empty() {
            return Ok(Value::Empty);
        }
        let typed = Value::text(t);
        match self.kind {
            ColumnType::Text | ColumnType::Choice | ColumnType::Colour | ColumnType::Formula => {
                self.check(typed)
            }
            ColumnType::Number => t
                .parse::<f64>()
                .ok()
                .or_else(|| {
                    // "12,5" but not "1,250.5", and not "1,500": a comma before exactly three
                    // digits is as likely a thousands separator (1500) as a decimal comma
                    // (1.5), so it is refused rather than guessed.
                    (t.matches(',').count() == 1 && !t.contains('.') && !thousands_like(t))
                        .then(|| t.replace(',', ".").parse::<f64>().ok())
                        .flatten()
                })
                .filter(|n| n.is_finite())
                .map(Value::number)
                .ok_or_else(|| self.wrong(&typed)),
            ColumnType::Checkmark => match t.to_ascii_lowercase().as_str() {
                "yes" | "y" | "true" | "1" | "x" | "checked" => Ok(Value::Bool(true)),
                "no" | "n" | "false" | "0" | "unchecked" => Ok(Value::Bool(false)),
                _ => Err(self.wrong(&typed)),
            },
            ColumnType::Date => parse_date(t)
                .or_else(|| {
                    // A Bluebeam date column also takes a date in the form Revu shows it in.
                    let format = self.bluebeam.as_ref()?.date_format.as_deref()?;
                    crate::bluebeam::parse_formatted_date(t, format)
                })
                .map(Value::Date)
                .ok_or_else(|| self.wrong(&typed)),
        }
    }
}

/// Whether `t` (one comma, no dot) reads as a whole number with a thousands separator: one to
/// three digits (after an optional sign), a comma, exactly three digits (`1,500`, `-12,000`).
fn thousands_like(t: &str) -> bool {
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let Some((int, frac)) = t.split_once(',') else {
        return false;
    };
    (1..=3).contains(&int.len())
        && frac.len() == 3
        && int.bytes().all(|b| b.is_ascii_digit())
        && frac.bytes().all(|b| b.is_ascii_digit())
}

/// The user's columns for one document. Serialisable; the application decides where it is
/// kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    /// Format version of this structure.
    pub version: u32,
    pub columns: Vec<CustomColumn>,
}

impl Default for Schema {
    fn default() -> Schema {
        Schema::new()
    }
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && id.len() <= 48
}

fn check_options(kind: ColumnType, options: &[String]) -> Result<(), ColumnError> {
    if kind != ColumnType::Choice {
        return Ok(());
    }
    if options.is_empty() {
        return Err(ColumnError::NoOptions);
    }
    for (i, o) in options.iter().enumerate() {
        if o.trim().is_empty() {
            return Err(ColumnError::EmptyOption);
        }
        if options[..i]
            .iter()
            .any(|p| Value::text(p.as_str()).same(&Value::text(o.as_str())))
        {
            return Err(ColumnError::DuplicateOption(o.clone()));
        }
    }
    Ok(())
}

impl Schema {
    pub const VERSION: u32 = 1;

    pub fn new() -> Schema {
        Schema {
            version: Schema::VERSION,
            columns: Vec::new(),
        }
    }

    pub fn get(&self, id: &str) -> Option<&CustomColumn> {
        self.columns.iter().find(|c| c.id == id)
    }

    fn title_taken(&self, title: &str, except: Option<&str>) -> bool {
        self.columns.iter().any(|c| {
            Some(c.id.as_str()) != except
                && Value::text(c.title.trim()).same(&Value::text(title.trim()))
        })
    }

    /// Adds a column and returns its id. The id is made from the title (ASCII letters and
    /// digits, lower case) and made unique; it never changes afterwards. `options` is used by
    /// a choice column only.
    pub fn add(
        &mut self,
        title: &str,
        kind: ColumnType,
        options: &[&str],
    ) -> Result<String, ColumnError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(ColumnError::EmptyTitle);
        }
        if kind == ColumnType::Colour || kind == ColumnType::Formula {
            // A formula column is added with its formula: `Schema::add_formula`.
            return Err(ColumnError::TypeNotAllowed(kind.name()));
        }
        self.push_column(title, kind, options)
    }

    /// Adds a formula column and returns its id. The formula must parse; the columns it names
    /// are looked up when the list is built (a name that is not a column there shows as an
    /// error in the cells, not here, since columns can be added in any order).
    pub fn add_formula(&mut self, title: &str, formula: &str) -> Result<String, ColumnError> {
        let checked = crate::formula::Formula::parse(formula).map(|_| ());
        let id = self.push_column(title.trim(), ColumnType::Formula, &[])?;
        if let Err(error) = checked {
            self.columns.pop();
            return Err(ColumnError::Formula {
                column: title.trim().to_string(),
                error,
            });
        }
        if let Some(c) = self.columns.last_mut() {
            c.formula = formula.trim().to_string();
        }
        Ok(id)
    }

    /// Replaces the formula of a formula column.
    pub fn set_formula(&mut self, id: &str, formula: &str) -> Result<(), ColumnError> {
        let column = self
            .columns
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ColumnError::UnknownColumn(id.to_string()))?;
        if column.kind != ColumnType::Formula {
            return Err(ColumnError::TypeNotAllowed(column.kind.name()));
        }
        crate::formula::Formula::parse(formula).map_err(|error| ColumnError::Formula {
            column: column.title.clone(),
            error,
        })?;
        column.formula = formula.trim().to_string();
        Ok(())
    }

    /// Adds a choice column whose options stand for numbers in formulas (a material and its
    /// unit cost). An option may have no number.
    pub fn add_choice(
        &mut self,
        title: &str,
        options: &[(&str, Option<f64>)],
    ) -> Result<String, ColumnError> {
        if let Some((o, _)) = options
            .iter()
            .find(|(_, v)| v.is_some_and(|v| !v.is_finite()))
        {
            return Err(ColumnError::BadOptionValue(o.to_string()));
        }
        let names: Vec<&str> = options.iter().map(|(o, _)| *o).collect();
        let id = self.add(title, ColumnType::Choice, &names)?;
        if options.iter().any(|(_, v)| v.is_some()) {
            if let Some(c) = self.columns.last_mut() {
                c.option_values = options.iter().map(|(_, v)| *v).collect();
            }
        }
        Ok(id)
    }

    /// Sets the number one option of a choice column stands for (`None` removes it).
    pub fn set_option_value(
        &mut self,
        id: &str,
        option: &str,
        value: Option<f64>,
    ) -> Result<(), ColumnError> {
        if value.is_some_and(|v| !v.is_finite()) {
            return Err(ColumnError::BadOptionValue(option.to_string()));
        }
        let column = self
            .columns
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ColumnError::UnknownColumn(id.to_string()))?;
        if column.kind != ColumnType::Choice {
            return Err(ColumnError::TypeNotAllowed(column.kind.name()));
        }
        let at = column
            .options
            .iter()
            .position(|o| Value::text(o.as_str()).same(&Value::text(option)))
            .ok_or_else(|| ColumnError::NotAnOption {
                column: column.title.clone(),
                value: option.to_string(),
            })?;
        column.option_values.resize(column.options.len(), None);
        column.option_values[at] = value;
        if column.option_values.iter().all(Option::is_none) {
            column.option_values.clear();
        }
        Ok(())
    }

    /// Marks a text column as one whose values may run over several lines. Returns whether
    /// the column is there.
    pub fn set_multiline(&mut self, id: &str, multiline: bool) -> bool {
        match self.columns.iter_mut().find(|c| c.id == id) {
            Some(c) => {
                c.multiline = multiline && c.kind == ColumnType::Text;
                true
            }
            None => false,
        }
    }

    /// Sets how a column's numbers are written (`None`: the shortest exact decimal).
    pub fn set_display(
        &mut self,
        id: &str,
        display: Option<NumberDisplay>,
    ) -> Result<(), ColumnError> {
        let column = self
            .columns
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ColumnError::UnknownColumn(id.to_string()))?;
        column.display = display;
        Ok(())
    }

    fn push_column(
        &mut self,
        title: &str,
        kind: ColumnType,
        options: &[&str],
    ) -> Result<String, ColumnError> {
        if title.is_empty() {
            return Err(ColumnError::EmptyTitle);
        }
        if self.title_taken(title, None) {
            return Err(ColumnError::DuplicateTitle(title.to_string()));
        }
        let options: Vec<String> = if kind == ColumnType::Choice {
            options.iter().map(|o| o.trim().to_string()).collect()
        } else {
            Vec::new()
        };
        check_options(kind, &options)?;

        let mut stem: String = title
            .chars()
            .filter_map(|c| match c {
                c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
                ' ' | '_' | '-' => Some('_'),
                _ => None,
            })
            .take(24)
            .collect();
        if !stem.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            stem.insert_str(0, "c_");
        }
        let stem = stem.trim_end_matches('_').to_string();
        let mut id = stem.clone();
        let mut n = 2;
        while self.get(&id).is_some() {
            id = format!("{stem}_{n}");
            n += 1;
        }
        self.columns.push(CustomColumn {
            id: id.clone(),
            title: title.to_string(),
            kind,
            options,
            subtotal: true,
            option_values: Vec::new(),
            formula: String::new(),
            display: None,
            multiline: false,
            bluebeam: None,
        });
        Ok(id)
    }

    /// Removes a column from the schema. Values already stored in annotations stay there
    /// until [`clear_custom_value`] removes them.
    pub fn remove(&mut self, id: &str) -> Option<CustomColumn> {
        let at = self.columns.iter().position(|c| c.id == id)?;
        Some(self.columns.remove(at))
    }

    pub fn rename(&mut self, id: &str, title: &str) -> Result<(), ColumnError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(ColumnError::EmptyTitle);
        }
        if self.title_taken(title, Some(id)) {
            return Err(ColumnError::DuplicateTitle(title.to_string()));
        }
        let column = self
            .columns
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ColumnError::UnknownColumn(id.to_string()))?;
        column.title = title.to_string();
        Ok(())
    }

    /// Replaces the options of a choice column. Stored values that are no longer an option
    /// are still shown; they cannot be chosen again.
    pub fn set_options(&mut self, id: &str, options: &[&str]) -> Result<(), ColumnError> {
        let options: Vec<String> = options.iter().map(|o| o.trim().to_string()).collect();
        let column = self
            .columns
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ColumnError::UnknownColumn(id.to_string()))?;
        if column.kind != ColumnType::Choice {
            return Err(ColumnError::TypeNotAllowed(column.kind.name()));
        }
        check_options(ColumnType::Choice, &options)?;
        // Numbers stay with their option, found by name.
        if !column.option_values.is_empty() {
            let old: Vec<(String, Option<f64>)> = column
                .options
                .iter()
                .cloned()
                .zip(column.option_values.iter().copied())
                .collect();
            column.option_values = options
                .iter()
                .map(|o| {
                    old.iter()
                        .find(|(p, _)| Value::text(p.as_str()).same(&Value::text(o.as_str())))
                        .and_then(|(_, v)| *v)
                })
                .collect();
            if column.option_values.iter().all(Option::is_none) {
                column.option_values.clear();
            }
        }
        column.options = options;
        Ok(())
    }

    /// Checks a schema that was read from storage.
    pub fn validate(&self) -> Result<(), ColumnError> {
        for (i, c) in self.columns.iter().enumerate() {
            if !valid_id(&c.id) {
                return Err(ColumnError::BadId(c.id.clone()));
            }
            if self.columns[..i].iter().any(|p| p.id == c.id) {
                return Err(ColumnError::DuplicateId(c.id.clone()));
            }
            if c.title.trim().is_empty() {
                return Err(ColumnError::EmptyTitle);
            }
            if c.kind == ColumnType::Colour {
                return Err(ColumnError::TypeNotAllowed(c.kind.name()));
            }
            check_options(c.kind, &c.options)?;
            if !c.option_values.is_empty() && c.option_values.len() != c.options.len() {
                return Err(ColumnError::BadOptionValue(c.title.clone()));
            }
            if let Some(i) = c
                .option_values
                .iter()
                .position(|v| v.is_some_and(|v| !v.is_finite()))
            {
                return Err(ColumnError::BadOptionValue(c.options[i].clone()));
            }
            if c.kind == ColumnType::Formula {
                crate::formula::Formula::parse(&c.formula).map_err(|error| {
                    ColumnError::Formula {
                        column: c.title.clone(),
                        error,
                    }
                })?;
            }
        }
        Ok(())
    }
}

fn string_of(o: &Object) -> Option<String> {
    match o {
        Object::String(bytes, _) => Some(decode_text(bytes)),
        Object::Name(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

fn read_value(kind: ColumnType, o: &Object) -> Value {
    match kind {
        ColumnType::Text | ColumnType::Choice | ColumnType::Colour => {
            string_of(o).map_or(Value::Empty, Value::text)
        }
        ColumnType::Number => match o {
            Object::Integer(i) => Value::Number(*i as f64),
            Object::Real(r) => Value::number(f64::from(*r)),
            other => string_of(other)
                .and_then(|s| s.trim().parse::<f64>().ok())
                .map_or(Value::Empty, Value::number),
        },
        ColumnType::Checkmark => match o {
            Object::Boolean(b) => Value::Bool(*b),
            _ => Value::Empty,
        },
        ColumnType::Date => string_of(o)
            .and_then(|s| PdfDate::parse(&s))
            .map_or(Value::Empty, Value::Date),
        // Worked out, never stored.
        ColumnType::Formula => Value::Empty,
    }
}

fn write_value(value: &Value) -> Option<Object> {
    Some(match value {
        Value::Empty => return None,
        Value::Text(s) => encode_text(s),
        Value::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 9.0e15 {
                Object::Integer(*n as i64)
            } else if f64::from(*n as f32) == *n {
                Object::Real(*n as f32)
            } else {
                Object::String(format!("{n}").into_bytes(), StringFormat::Literal)
            }
        }
        Value::Bool(b) => Object::Boolean(*b),
        Value::Date(d) => {
            // A date-only value is written without a time, and reads back as one.
            let s = if is_day(d) {
                format!("D:{}", format_date(d).replace('-', ""))
            } else {
                d.to_pdf_string()
            };
            Object::String(s.into_bytes(), StringFormat::Literal)
        }
        Value::Colour(_) => encode_text(&value.display()),
    })
}

fn columns_dict(a: &Annotation) -> Option<&Dictionary> {
    a.app_data
        .as_ref()?
        .get(COLUMNS_KEY.as_bytes())
        .ok()?
        .as_dict()
        .ok()
}

/// The value `a` holds for a user column; `Empty` when it holds none or what is stored is not
/// of the column's type. For a Bluebeam column ([`CustomColumn::bluebeam`]) it is read from
/// `/BSIColumnData`, and text that does not read as the column's type is shown as the text it
/// is (never as a number or a date it is not).
pub fn custom_value(a: &Annotation, column: &CustomColumn) -> Value {
    if let Some(bb) = &column.bluebeam {
        return crate::bluebeam::value_of(a, column, bb);
    }
    columns_dict(a)
        .and_then(|d| d.get(column.id.as_bytes()).ok())
        .map_or(Value::Empty, |o| read_value(column.kind, o))
}

/// The ids of every column `a` holds a value for, whether the schema still has them or not.
pub fn stored_column_ids(a: &Annotation) -> Vec<String> {
    columns_dict(a)
        .map(|d| {
            d.iter()
                .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn store(a: &mut Annotation, id: &str, object: Option<Object>) -> bool {
    let before = a.app_data.clone();
    let mut data = a.app_data.take().unwrap_or_default();
    let mut columns = match data.get(COLUMNS_KEY.as_bytes()) {
        Ok(Object::Dictionary(d)) => d.clone(),
        _ => Dictionary::new(),
    };
    match object {
        Some(o) => columns.set(id.as_bytes().to_vec(), o),
        None => {
            columns.remove(id.as_bytes());
        }
    }
    if columns.is_empty() {
        data.remove(COLUMNS_KEY.as_bytes());
    } else {
        data.set(COLUMNS_KEY, Object::Dictionary(columns));
    }
    a.app_data = (!data.is_empty()).then_some(data);
    a.app_data != before
}

/// Sets the value `a` holds for a user column, after [`CustomColumn::check`]. `Empty` removes
/// it. Returns whether the annotation changed (the caller then stamps `/M` if it wants to).
pub fn set_custom_value(
    a: &mut Annotation,
    column: &CustomColumn,
    value: Value,
) -> Result<bool, ColumnError> {
    if !valid_id(&column.id) {
        return Err(ColumnError::BadId(column.id.clone()));
    }
    let value = column.check(value)?;
    if let Some(bb) = &column.bluebeam {
        return Ok(crate::bluebeam::store_value(a, column, bb, &value));
    }
    Ok(store(a, &column.id, write_value(&value)))
}

/// Removes whatever `a` holds under the column id `id` (also one the schema no longer has).
/// Returns whether the annotation changed.
pub fn clear_custom_value(a: &mut Annotation, id: &str) -> bool {
    store(a, id, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_annot::Origin;
    use pl_geom::Rect;

    fn square() -> Annotation {
        let origin = Origin::new("Ada", PdfDate::from_unix(1_700_000_000));
        Annotation::square(0, Rect::new(0.0, 0.0, 10.0, 10.0), &origin)
    }

    #[test]
    fn ids_come_from_titles_and_stay_unique() {
        let mut s = Schema::new();
        assert_eq!(s.add("Trade", ColumnType::Text, &[]).unwrap(), "trade");
        assert_eq!(
            s.add("Unit Cost (£)", ColumnType::Number, &[]).unwrap(),
            "unit_cost"
        );
        assert_eq!(
            s.add("2nd fix", ColumnType::Checkmark, &[]).unwrap(),
            "c_2nd_fix"
        );
        assert_eq!(s.add("Trade?", ColumnType::Text, &[]).unwrap(), "trade_2");
        assert_eq!(s.add("Ärger", ColumnType::Text, &[]).unwrap(), "rger");
        assert_eq!(s.add("日本", ColumnType::Text, &[]).unwrap(), "c");
        assert_eq!(
            s.add(" trade ", ColumnType::Date, &[]),
            Err(ColumnError::DuplicateTitle("trade".into()))
        );
        assert_eq!(
            s.add("  ", ColumnType::Text, &[]),
            Err(ColumnError::EmptyTitle)
        );
        assert_eq!(
            s.add("Tint", ColumnType::Colour, &[]),
            Err(ColumnError::TypeNotAllowed("Colour"))
        );
        assert_eq!(
            s.add("Phase", ColumnType::Choice, &[]),
            Err(ColumnError::NoOptions)
        );
        assert_eq!(
            s.add("Phase", ColumnType::Choice, &["One", "one"]),
            Err(ColumnError::DuplicateOption("one".into()))
        );
        s.validate().unwrap();

        s.rename("trade", "Discipline").unwrap();
        assert_eq!(s.get("trade").unwrap().title, "Discipline");
        assert_eq!(
            s.rename("trade", "trade?"),
            Err(ColumnError::DuplicateTitle("trade?".into()))
        );
        assert!(s.remove("trade").is_some() && s.get("trade").is_none());
    }

    #[test]
    fn a_stored_schema_is_validated() {
        let mut s = Schema::new();
        s.add("Phase", ColumnType::Choice, &["One", "Two"]).unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let back: Schema = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        back.validate().unwrap();

        let mut bad = s.clone();
        bad.columns[0].id = "has space".into();
        assert_eq!(bad.validate(), Err(ColumnError::BadId("has space".into())));
        let mut twice = s.clone();
        twice.columns.push(s.columns[0].clone());
        assert_eq!(
            twice.validate(),
            Err(ColumnError::DuplicateId("phase".into()))
        );
        // An older file without the later fields still reads.
        let old: Schema = serde_json::from_str(
            r#"{"version":1,"columns":[{"id":"n","title":"N","kind":"Number"}]}"#,
        )
        .unwrap();
        assert!(old.columns[0].subtotal && old.columns[0].options.is_empty());
    }

    #[test]
    fn typed_text_is_read_per_column_type() {
        let mut s = Schema::new();
        for (t, k) in [
            ("N", ColumnType::Number),
            ("C", ColumnType::Checkmark),
            ("D", ColumnType::Date),
        ] {
            s.add(t, k, &[]).unwrap();
        }
        s.add("P", ColumnType::Choice, &["Shell", "Fit-out"])
            .unwrap();
        let col = |id: &str| s.get(id).unwrap();
        assert_eq!(col("n").parse(" 12.5 "), Ok(Value::Number(12.5)));
        assert_eq!(col("n").parse("12,5"), Ok(Value::Number(12.5)));
        assert_eq!(col("n").parse("-1e3"), Ok(Value::Number(-1000.0)));
        // "-0" is zero, not a second kind of zero with its own group.
        assert!(matches!(col("n").parse("-0"), Ok(Value::Number(n)) if n.is_sign_positive()));
        assert!(col("n").parse("1,250.5").is_err());
        // A comma before exactly three digits may be a thousands separator: never read as 1.5.
        assert!(col("n").parse("1,500").is_err());
        assert!(col("n").parse("-2,000").is_err());
        assert_eq!(col("n").parse("0,25"), Ok(Value::Number(0.25)));
        assert_eq!(col("n").parse("1234,500"), Ok(Value::Number(1234.5)));
        assert!(col("n").parse("inf").is_err());
        assert_eq!(col("n").parse("  "), Ok(Value::Empty));
        assert_eq!(col("c").parse("Yes"), Ok(Value::Bool(true)));
        assert_eq!(col("c").parse("0"), Ok(Value::Bool(false)));
        assert!(col("c").parse("maybe").is_err());
        assert_eq!(col("d").parse("2026-12-01"), Ok(Value::day(2026, 12, 1)));
        assert!(col("d").parse("12/01/2026").is_err());
        assert_eq!(col("p").parse("fit-OUT"), Ok(Value::text("Fit-out")));
        assert_eq!(
            col("p").parse("Roof"),
            Err(ColumnError::NotAnOption {
                column: "P".into(),
                value: "Roof".into()
            })
        );
        let message = col("n").parse("abc").unwrap_err().to_string();
        assert_eq!(message, "\"abc\" is not a number (column \"N\")");
    }

    #[test]
    fn values_live_under_one_key_and_leave_nothing_behind() {
        let mut s = Schema::new();
        s.add("Trade", ColumnType::Text, &[]).unwrap();
        s.add("Qty", ColumnType::Number, &[]).unwrap();
        let (trade, qty) = (s.get("trade").unwrap(), s.get("qty").unwrap());
        let mut a = square();
        let clean = a.clone();

        assert_eq!(custom_value(&a, trade), Value::Empty);
        assert_eq!(set_custom_value(&mut a, trade, Value::Empty), Ok(false));
        assert_eq!(a, clean);

        assert_eq!(
            set_custom_value(&mut a, trade, Value::text("Électricité")),
            Ok(true)
        );
        assert_eq!(
            set_custom_value(&mut a, trade, Value::text("Électricité")),
            Ok(false)
        );
        assert_eq!(set_custom_value(&mut a, qty, Value::Number(0.1)), Ok(true));
        let data = a.app_data.as_ref().unwrap();
        assert_eq!(data.len(), 1, "everything sits under /Columns");
        assert_eq!(custom_value(&a, trade), Value::text("Électricité"));
        assert_eq!(custom_value(&a, qty), Value::Number(0.1));
        assert_eq!(stored_column_ids(&a), ["trade", "qty"]);

        assert_eq!(
            set_custom_value(&mut a, qty, Value::text("many")),
            Err(ColumnError::WrongType {
                column: "Qty".into(),
                expected: "number",
                value: "many".into()
            })
        );
        assert_eq!(custom_value(&a, qty), Value::Number(0.1));

        assert_eq!(set_custom_value(&mut a, trade, Value::text("  ")), Ok(true));
        assert!(clear_custom_value(&mut a, "qty"));
        assert!(!clear_custom_value(&mut a, "qty"));
        assert_eq!(a, clean, "no empty dictionaries are left");
    }

    #[test]
    fn other_private_data_is_left_alone() {
        let mut s = Schema::new();
        s.add("Done", ColumnType::Checkmark, &[]).unwrap();
        let done = s.get("done").unwrap();
        let mut a = square();
        let mut other = Dictionary::new();
        other.set("Tool", Object::Name(b"Preset7".to_vec()));
        a.app_data = Some(other.clone());
        set_custom_value(&mut a, done, Value::Bool(false)).unwrap();
        assert_eq!(custom_value(&a, done), Value::Bool(false));
        assert_eq!(a.app_data.as_ref().unwrap().len(), 2);
        set_custom_value(&mut a, done, Value::Empty).unwrap();
        assert_eq!(a.app_data, Some(other));
    }

    #[test]
    fn numbers_keep_full_precision_in_every_form() {
        for (n, form) in [
            (12.0, "Integer"),
            (-3.0, "Integer"),
            (0.5, "Real"),
            (0.1, "String"),
            (1.0e300, "String"),
            (123456.789, "String"),
        ] {
            let o = write_value(&Value::Number(n)).unwrap();
            assert_eq!(o.enum_variant(), form, "{n}");
            assert_eq!(read_value(ColumnType::Number, &o), Value::Number(n));
        }
        // What is stored under a column whose type has since changed reads as empty.
        assert_eq!(
            read_value(ColumnType::Checkmark, &Object::Integer(1)),
            Value::Empty
        );
        assert_eq!(
            read_value(ColumnType::Number, &encode_text("abc")),
            Value::Empty
        );
        let day = Value::day(2026, 12, 1);
        let o = write_value(&day).unwrap();
        assert_eq!(
            o,
            Object::String(b"D:20261201".to_vec(), StringFormat::Literal)
        );
        assert_eq!(read_value(ColumnType::Date, &o), day);
    }
}
