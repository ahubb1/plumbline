//! Bluebeam Revu's custom columns: the definitions in the catalog's `/BSIAnnotColumns` and the
//! values in each annotation's `/BSIColumnData`.
//!
//! # The format (read from redcolumn's Revu-checked A3 test set, 2026-11-24)
//!
//! ```text
//! catalog:    /BSIAnnotColumns 16 0 R          % must be an indirect reference (Revu ignores a direct array)
//! 16 0 obj    [ << /Subtype /Choice /DisplayOrder 0 /Name (Discipline) /Items [ (Architectural) ... ] >>
//!               << /Subtype /Choice /DisplayOrder 1 /Name (Priority) /DefaultValue (Medium) /Items [...] >>
//!               << /Subtype /Text /DisplayOrder 3 /Name (Review Notes) /Multiline true >>
//!               << /Subtype /Number /DisplayOrder 4 /Name (Qty) /Format /Normal /Precision 0 >>
//!               << /Subtype /Formula /DisplayOrder 6 /Name (Total Cost) /Format /Normal /Precision 2
//!                  /Expression ([Qty] * [Unit Cost]) >>
//!               << /Subtype /Date /DisplayOrder 7 /Name (Due Date) /Format (MM/dd/yyyy) >>
//!               << /Subtype /Checkmark /DisplayOrder 8 /Name (Verified) /DefaultValue (False) >> ... ]
//! annotation: /BSIColumnData [ (Structural) () () () (2) (13.50) () () (True) () () ]
//! ```
//!
//! * The values are positional: entry *i* of `/BSIColumnData` belongs to entry *i* of
//!   `/BSIAnnotColumns` (array order, not `/DisplayOrder`, which is only the order Revu shows
//!   them in). Every value is a text string; an empty string is no value. A markup can have no
//!   `/BSIColumnData` at all.
//! * Types (`/Subtype`): `Text` (with `/Multiline true` for multi-line text), `Number`
//!   (`/Format /Normal`, `/Precision` places), `Choice` (`/Items`), `Date` (stored as a PDF
//!   date, `D:20261225000000`; `/Format` is how Revu shows it), `Checkmark` (`True` / `False`)
//!   and `Formula` (`/Expression`, columns named in square brackets). redcolumn also accepts
//!   `Dropdown` and `List` as choice types and skips an entry marked `/Deleted true` (Revu
//!   keeps a removed column's place so the values keep theirs); both are honoured here.
//!   `/DefaultValue` is kept but not applied to values that are empty.
//! * A "required" flag is not visible in the files (redcolumn's README calls Priority
//!   required, but its entry has only `/DefaultValue`), so none is read.
//!
//! # How Plumbline uses it
//!
//! Each live entry becomes a [`CustomColumn`] with a [`BluebeamColumn`] binding: its values are
//! read from and written to `/BSIColumnData` at the entry's position, and nothing of them goes
//! into Plumbline's private storage. A value that does not read as the column's type (a number
//! column holding `abc` or `1,5`, a date column holding `tomorrow`, a checkmark holding
//! anything but `True`/`False`, a choice not in the list) is shown as the text it is, and is
//! never rewritten unless the user sets that cell. A Bluebeam formula whose names are all
//! Bluebeam columns and whose syntax Plumbline's formula language reads the same way is worked
//! out by Plumbline (a checkmark counts 1 or 0 in it, as in Revu); any other is a read-only
//! column showing the text Bluebeam stored in its position.

use pl_annot::{decode_text, Annotation, PdfDate};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, StringFormat};
use serde::{Deserialize, Serialize};

use crate::column::{ColumnError, ColumnType, CustomColumn, NumberDisplay};
use crate::value::{parse_date, Value};

/// The catalog key that holds the column definitions.
pub const ANNOT_COLUMNS_KEY: &str = "BSIAnnotColumns";
pub use pl_annot::COLUMN_DATA_KEY;

/// What ties a [`CustomColumn`] to an entry of `/BSIAnnotColumns`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BluebeamColumn {
    /// The entry's position in `/BSIAnnotColumns`, and so the value's in `/BSIColumnData`.
    pub index: usize,
    /// How many entries `/BSIAnnotColumns` has: a `/BSIColumnData` that is written is padded
    /// to this many values.
    pub width: usize,
    /// `/Subtype` as the file names it.
    pub subtype: String,
    /// `/Name` as the file has it.
    pub name: String,
    /// `/Expression` of a formula, as Bluebeam wrote it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expression: String,
    /// True when the column shows what Bluebeam stored and cannot be typed in: a formula
    /// Plumbline does not work out the same way, or a type it does not know.
    #[serde(default)]
    pub stored: bool,
    /// `/DefaultValue`, kept for reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
    /// `/Format` of a date column (`MM/dd/yyyy`): the form Revu shows its dates in, which a
    /// typed date may also be given in ([`parse_formatted_date`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_format: Option<String>,
}

/// Reads a date typed in a Bluebeam date column's display format (`/Format`, such as
/// `MM/dd/yyyy` or `dd.MM.yy`): its day, month and year in the order the format names them,
/// separated by anything but digits. A two-digit year is read as 20yy only when the format's
/// year has two letters. `None` for anything else, or a day that does not exist.
pub fn parse_formatted_date(text: &str, format: &str) -> Option<PdfDate> {
    // The order of the day, month and year fields, and the year's width.
    let mut order: Vec<char> = Vec::new();
    let mut year_letters = 0;
    for c in format.chars() {
        if matches!(c, 'd' | 'M' | 'y') && !order.contains(&c) {
            order.push(c);
        }
        if c == 'y' {
            year_letters += 1;
        }
    }
    if order.len() != 3 {
        return None;
    }
    let fields: Vec<&str> = text
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|f| !f.is_empty())
        .collect();
    if fields.len() != 3 || text.trim().chars().any(|c| c.is_alphabetic()) {
        return None;
    }
    let (mut day, mut month, mut year) = (0u32, 0u32, 0u32);
    for (field, which) in fields.iter().zip(&order) {
        if field.len() > 4 {
            return None;
        }
        let n: u32 = field.parse().ok()?;
        match which {
            'd' if field.len() <= 2 => day = n,
            'M' if field.len() <= 2 => month = n,
            'y' if field.len() == 4 => year = n,
            'y' if field.len() == 2 && year_letters == 2 => year = 2000 + n,
            _ => return None,
        }
    }
    crate::value::parse_date(&format!("{year:04}-{month:02}-{day:02}"))
}

/// The catalog's `/BSIAnnotColumns` as the file holds it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BluebeamColumns {
    /// The array's own object, when the catalog refers to it (the form Revu reads).
    pub object: Option<ObjectId>,
    /// True when the catalog holds the array directly (Revu ignores that form; a save that
    /// changes it writes it as an object of its own).
    pub direct: bool,
    /// The entries, each resolved, in array order.
    pub entries: Vec<Object>,
}

pub(crate) fn text_object(s: &str) -> Object {
    // UTF-16BE with a byte-order mark, as Bluebeam's files (and redcolumn's) write every value.
    let mut bytes = vec![0xFE, 0xFF];
    for unit in s.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Object::String(bytes, StringFormat::Hexadecimal)
}

fn text_of(o: &Object) -> Option<String> {
    match o {
        Object::String(bytes, _) => Some(decode_text(bytes)),
        Object::Name(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

fn name_of(o: &Object) -> Option<String> {
    match o {
        Object::Name(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Object::String(bytes, _) => Some(decode_text(bytes)),
        _ => None,
    }
}

fn int_of(o: &Object) -> Option<i64> {
    match o {
        Object::Integer(i) => Some(*i),
        Object::Real(r) if r.fract() == 0.0 => Some(*r as i64),
        _ => None,
    }
}

/// The column type a `/Subtype` maps to; `None` for one Plumbline does not know.
fn kind_of(subtype: &str) -> Option<ColumnType> {
    Some(match subtype {
        "Text" => ColumnType::Text,
        "Number" => ColumnType::Number,
        "Choice" | "Dropdown" | "List" => ColumnType::Choice,
        "Date" => ColumnType::Date,
        "Checkmark" => ColumnType::Checkmark,
        "Formula" => ColumnType::Formula,
        _ => return None,
    })
}

/// The `/Subtype` written for a column type.
fn subtype_of(kind: ColumnType) -> Option<&'static str> {
    Some(match kind {
        ColumnType::Text => "Text",
        ColumnType::Number => "Number",
        ColumnType::Choice => "Choice",
        ColumnType::Date => "Date",
        ColumnType::Checkmark => "Checkmark",
        ColumnType::Formula => "Formula",
        ColumnType::Colour => return None,
    })
}

fn id_stem(name: &str) -> String {
    let stem: String = name
        .chars()
        .filter_map(|c| match c {
            c if c.is_ascii_alphanumeric() => Some(c.to_ascii_lowercase()),
            ' ' | '_' | '-' => Some('_'),
            _ => None,
        })
        .take(24)
        .collect();
    let stem = stem.trim_matches('_');
    if stem.is_empty() {
        "bb_column".to_string()
    } else {
        format!("bb_{stem}")
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// One live entry, read.
struct Entry<'a> {
    index: usize,
    dict: &'a Dictionary,
    name: String,
    subtype: String,
    order: i64,
}

impl BluebeamColumns {
    /// Reads the catalog's `/BSIAnnotColumns`. A file without it reads as empty.
    pub fn read(doc: &PdfDocument) -> BluebeamColumns {
        let Some(value) = doc
            .catalog()
            .ok()
            .and_then(|c| c.get(ANNOT_COLUMNS_KEY.as_bytes()).ok())
        else {
            return BluebeamColumns::default();
        };
        let object = match value {
            Object::Reference(id) => Some(*id),
            _ => None,
        };
        let entries: Vec<Object> = doc
            .resolve(value)
            .and_then(|o| o.as_array().ok())
            .map(|a| {
                a.iter()
                    .map(|e| doc.resolve(e).cloned().unwrap_or(Object::Null))
                    .collect()
            })
            .unwrap_or_default();
        BluebeamColumns {
            direct: object.is_none(),
            object,
            entries,
        }
    }

    /// True when the document has `/BSIAnnotColumns` (or one is to be written).
    pub fn is_present(&self) -> bool {
        self.object.is_some() || self.direct || !self.entries.is_empty()
    }

    fn live(&self) -> Vec<Entry<'_>> {
        let mut seen: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for (index, e) in self.entries.iter().enumerate() {
            let Ok(dict) = e.as_dict() else {
                continue;
            };
            let name = dict
                .get(b"Name")
                .ok()
                .and_then(text_of)
                .map(|n| n.trim().to_string())
                .unwrap_or_default();
            let deleted = matches!(dict.get(b"Deleted"), Ok(Object::Boolean(true)));
            // A column listed twice: the first entry is the column (as redcolumn reads it); the
            // other's values stay where they are, untouched.
            if name.is_empty() || deleted || seen.iter().any(|s| same_name(s, &name)) {
                continue;
            }
            seen.push(name.clone());
            let subtype = dict
                .get(b"Subtype")
                .ok()
                .and_then(name_of)
                .unwrap_or_default();
            let order = dict
                .get(b"DisplayOrder")
                .ok()
                .and_then(int_of)
                .unwrap_or(index as i64);
            out.push(Entry {
                index,
                dict,
                name,
                subtype,
                order,
            });
        }
        out
    }

    /// The columns, in Bluebeam's display order (`/DisplayOrder`, then array order).
    pub fn columns(&self) -> Vec<CustomColumn> {
        let width = self.entries.len();
        let live = self.live();
        // Ids first, so formulas can name the columns by id.
        let mut ids: Vec<String> = Vec::new();
        for e in &live {
            let stem = id_stem(&e.name);
            let mut id = stem.clone();
            let mut n = 2;
            while ids.contains(&id) {
                id = format!("{stem}_{n}");
                n += 1;
            }
            ids.push(id);
        }
        let mut columns: Vec<(i64, usize, CustomColumn)> = live
            .iter()
            .zip(&ids)
            .map(|(e, id)| {
                let d = e.dict;
                let known = kind_of(&e.subtype);
                let precision = d
                    .get(b"Precision")
                    .ok()
                    .and_then(int_of)
                    .map(|p| p.clamp(0, 10) as u8);
                let expression = d
                    .get(b"Expression")
                    .ok()
                    .and_then(text_of)
                    .unwrap_or_default();
                let mut column = CustomColumn {
                    id: id.clone(),
                    title: e.name.clone(),
                    kind: known.unwrap_or(ColumnType::Text),
                    options: Vec::new(),
                    subtotal: true,
                    option_values: Vec::new(),
                    formula: String::new(),
                    display: precision.map(NumberDisplay::places),
                    multiline: matches!(d.get(b"Multiline"), Ok(Object::Boolean(true))),
                    bluebeam: Some(BluebeamColumn {
                        index: e.index,
                        width,
                        subtype: e.subtype.clone(),
                        name: e.name.clone(),
                        expression: expression.clone(),
                        stored: known.is_none(),
                        default_value: d.get(b"DefaultValue").ok().and_then(text_of),
                        date_format: (known == Some(ColumnType::Date))
                            .then(|| d.get(b"Format").ok().and_then(text_of))
                            .flatten()
                            .filter(|f| !f.trim().is_empty()),
                    }),
                };
                if column.kind == ColumnType::Choice {
                    let items = d
                        .get(b"Items")
                        .or_else(|_| d.get(b"Options"))
                        .or_else(|_| d.get(b"Choices"))
                        .ok()
                        .and_then(|o| o.as_array().ok());
                    for item in items.into_iter().flatten() {
                        // An item is text, or a small array whose last text is the label.
                        let text = match item {
                            Object::Array(parts) => parts.iter().rev().find_map(text_of),
                            other => text_of(other),
                        };
                        if let Some(t) = text.filter(|t| !t.trim().is_empty()) {
                            if !column.options.iter().any(|o| same_name(o, &t)) {
                                column.options.push(t);
                            }
                        }
                    }
                }
                (e.order, e.index, column)
            })
            .collect();
        // Formulas: Plumbline's own reading where it is the same, else what Bluebeam stored.
        let names: Vec<(String, String, ColumnType)> = columns
            .iter()
            .map(|(_, _, c)| (c.title.clone(), c.id.clone(), c.kind))
            .collect();
        for (_, _, c) in &mut columns {
            if c.kind != ColumnType::Formula {
                continue;
            }
            let expression = c.bluebeam.as_ref().map(|b| b.expression.clone());
            match expression
                .as_deref()
                .and_then(|e| translate_formula(e, &names))
            {
                Some(formula) => c.formula = formula,
                None => {
                    c.kind = ColumnType::Number;
                    if let Some(b) = c.bluebeam.as_mut() {
                        b.stored = true;
                    }
                }
            }
        }
        columns.sort_by_key(|(order, index, _)| (*order, *index));
        columns.into_iter().map(|(_, _, c)| c).collect()
    }

    /// Appends a column in Bluebeam's own structure and returns the new column. `places` is
    /// the precision of a number column (2 when `None`). A formula column cannot be added
    /// here (its expression would have to be Bluebeam's).
    pub fn add(
        &mut self,
        title: &str,
        kind: ColumnType,
        options: &[&str],
        multiline: bool,
        places: Option<u8>,
    ) -> Result<CustomColumn, ColumnError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(ColumnError::EmptyTitle);
        }
        let subtype = match kind {
            ColumnType::Formula | ColumnType::Colour => {
                return Err(ColumnError::TypeNotAllowed(kind.name()))
            }
            k => subtype_of(k).ok_or(ColumnError::TypeNotAllowed(k.name()))?,
        };
        if self.live().iter().any(|e| same_name(&e.name, title)) {
            return Err(ColumnError::DuplicateTitle(title.to_string()));
        }
        let mut check = crate::column::Schema::new();
        check.add(title, kind, options)?;
        let options: Vec<String> = check.columns[0].options.clone();

        let order = self
            .live()
            .iter()
            .map(|e| e.order)
            .max()
            .map_or(0, |m| m + 1)
            .max(0);
        let mut d = Dictionary::new();
        d.set("Subtype", Object::Name(subtype.as_bytes().to_vec()));
        d.set("DisplayOrder", Object::Integer(order));
        d.set("Name", text_object(title));
        match kind {
            ColumnType::Text if multiline => d.set("Multiline", Object::Boolean(true)),
            ColumnType::Number => {
                d.set("Format", Object::Name(b"Normal".to_vec()));
                d.set(
                    "Precision",
                    Object::Integer(i64::from(places.unwrap_or(2).min(10))),
                );
            }
            ColumnType::Choice => d.set(
                "Items",
                Object::Array(options.iter().map(|o| text_object(o)).collect()),
            ),
            ColumnType::Date => d.set("Format", text_object("MM/dd/yyyy")),
            _ => {}
        }
        self.entries.push(Object::Dictionary(d));
        let index = self.entries.len() - 1;
        self.columns()
            .into_iter()
            .find(|c| c.bluebeam.as_ref().is_some_and(|b| b.index == index))
            .ok_or_else(|| ColumnError::UnknownColumn(title.to_string()))
    }

    /// The array as it is written: the entries, in order.
    pub fn to_object(&self) -> Object {
        Object::Array(self.entries.clone())
    }
}

/// Bluebeam's formula in Plumbline's formula language, when every name in it is a Bluebeam
/// column of the document and the result parses: each `[Name]` becomes the column's id (a
/// checkmark `if([id], 1, 0)`, since Revu counts a ticked box as 1). `None` otherwise: a name
/// that is not one of the document's columns (a measurement column of Revu's, whose unit
/// Plumbline cannot be sure of), or syntax Plumbline does not read.
pub fn translate_formula(
    expression: &str,
    columns: &[(String, String, ColumnType)],
) -> Option<String> {
    let mut out = String::new();
    let mut rest = expression;
    while let Some(open) = rest.find('[') {
        if rest[..open].contains(']') {
            return None;
        }
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after.find(']')?;
        let name = &after[..close];
        let (_, id, kind) = columns
            .iter()
            .find(|(title, _, _)| same_name(title, name))?;
        if *kind == ColumnType::Checkmark {
            out.push_str(&format!("if([{id}], 1, 0)"));
        } else {
            out.push_str(&format!("[{id}]"));
        }
        rest = &after[close + 1..];
    }
    if rest.contains(']') {
        return None;
    }
    out.push_str(rest);
    crate::formula::Formula::parse(&out).ok()?;
    Some(out)
}

/// The text `a` stores for the column at `index`; `None` when it stores nothing there (no
/// `/BSIColumnData`, too short a one, or an entry that is not text).
pub fn stored_text(a: &Annotation, index: usize) -> Option<String> {
    match a.column_data.as_ref()?.get(index)? {
        Object::String(bytes, _) => Some(decode_text(bytes)),
        _ => None,
    }
}

fn valid_day(d: &PdfDate) -> bool {
    let leap =
        (d.year.is_multiple_of(4) && !d.year.is_multiple_of(100)) || d.year.is_multiple_of(400);
    let days = match d.month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=12).contains(&d.month) && d.day >= 1 && d.day <= days
}

/// What a stored text means in a column of `kind`: its typed value when it reads as one, else
/// the text itself.
pub fn typed_value(kind: ColumnType, raw: &str) -> Value {
    if raw.is_empty() {
        return Value::Empty;
    }
    let text = || Value::text(raw);
    match kind {
        ColumnType::Number => {
            let t = raw.trim();
            let plain = !t.is_empty()
                && t.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'));
            match t.parse::<f64>() {
                Ok(n) if plain && n.is_finite() => Value::number(n),
                _ => text(),
            }
        }
        ColumnType::Date => {
            let t = raw.trim();
            let parsed = if t.starts_with("D:") {
                PdfDate::parse(t)
            } else {
                parse_date(t)
            };
            match parsed.filter(valid_day) {
                Some(d) => Value::Date(d),
                None => text(),
            }
        }
        ColumnType::Checkmark => {
            if raw.trim().eq_ignore_ascii_case("true") {
                Value::Bool(true)
            } else if raw.trim().eq_ignore_ascii_case("false") {
                Value::Bool(false)
            } else {
                text()
            }
        }
        // Worked out by the list.
        ColumnType::Formula => Value::Empty,
        ColumnType::Text | ColumnType::Choice | ColumnType::Colour => text(),
    }
}

/// The value of a Bluebeam column in `a`.
pub(crate) fn value_of(a: &Annotation, column: &CustomColumn, bb: &BluebeamColumn) -> Value {
    stored_text(a, bb.index).map_or(Value::Empty, |raw| typed_value(column.kind, &raw))
}

/// The text a value is stored as.
fn stored_form(value: &Value) -> String {
    match value {
        Value::Empty => String::new(),
        Value::Text(s) => s.clone(),
        Value::Number(n) => format!("{n}"),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Date(d) => d.to_pdf_string(),
        Value::Colour(_) => value.display(),
    }
}

/// Writes a (checked) value into `a`'s `/BSIColumnData`. Only the column's own position is
/// replaced; every other entry keeps its object (bytes and string form). Nothing changes
/// when the value is what is stored already (`13.50` stays `13.50` when 13.5 is set). A
/// `/BSIColumnData` that is created or too short is padded with empty strings to the
/// number of columns. Returns whether the annotation changed.
pub(crate) fn store_value(
    a: &mut Annotation,
    column: &CustomColumn,
    bb: &BluebeamColumn,
    value: &Value,
) -> bool {
    if value_of(a, column, bb) == *value {
        return false;
    }
    let mut data = a.column_data.take().unwrap_or_default();
    let len = data.len().max(bb.width).max(bb.index + 1);
    while data.len() < len {
        data.push(text_object(""));
    }
    data[bb.index] = text_object(&stored_form(value));
    a.column_data = Some(data);
    true
}

/// Pads `a`'s `/BSIColumnData`, when it has one, to `width` values with empty strings (the
/// form a column added to `/BSIAnnotColumns` needs). Returns whether it changed.
pub fn pad_column_data(a: &mut Annotation, width: usize) -> bool {
    match a.column_data.as_mut() {
        Some(data) if data.len() < width => {
            while data.len() < width {
                data.push(text_object(""));
            }
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_annot::Origin;
    use pl_geom::Rect;

    fn entry(subtype: &str, name: &str, extra: &[(&str, Object)]) -> Object {
        let mut d = Dictionary::new();
        d.set("Subtype", Object::Name(subtype.as_bytes().to_vec()));
        d.set("Name", text_object(name));
        for (k, v) in extra {
            d.set(*k, v.clone());
        }
        Object::Dictionary(d)
    }

    #[test]
    fn a_date_is_read_in_the_columns_own_format() {
        let day = |y, m, d| crate::value::parse_date(&format!("{y:04}-{m:02}-{d:02}"));
        assert_eq!(
            parse_formatted_date("12/25/2027", "MM/dd/yyyy"),
            day(2027, 12, 25)
        );
        assert_eq!(
            parse_formatted_date(" 2/9/2027 ", "MM/dd/yyyy"),
            day(2027, 2, 9)
        );
        assert_eq!(
            parse_formatted_date("25.12.27", "dd.MM.yy"),
            day(2027, 12, 25)
        );
        // Not a day, not the format's order, a two-digit year for a four-letter one.
        assert_eq!(parse_formatted_date("02/29/2027", "MM/dd/yyyy"), None);
        assert_eq!(parse_formatted_date("25/12/2027", "MM/dd/yyyy"), None);
        assert_eq!(parse_formatted_date("12/25/27", "MM/dd/yyyy"), None);
        assert_eq!(parse_formatted_date("Dec 25 2027", "MM/dd/yyyy"), None);
        assert_eq!(parse_formatted_date("12/25", "MM/dd/yyyy"), None);
        assert_eq!(parse_formatted_date("12/25/2027", "Normal"), None);
    }

    fn sample() -> BluebeamColumns {
        BluebeamColumns {
            object: None,
            direct: false,
            entries: vec![
                entry(
                    "Choice",
                    "Priority",
                    &[
                        ("DisplayOrder", Object::Integer(1)),
                        (
                            "Items",
                            Object::Array(vec![text_object("Low"), text_object("High")]),
                        ),
                    ],
                ),
                entry(
                    "Number",
                    "Qty",
                    &[
                        ("DisplayOrder", Object::Integer(0)),
                        ("Precision", Object::Integer(0)),
                    ],
                ),
                entry("Number", "Unit Cost", &[("Precision", Object::Integer(2))]),
                entry(
                    "Formula",
                    "Total",
                    &[("Expression", text_object("[Qty] * [Unit Cost]"))],
                ),
                entry("Checkmark", "Done", &[]),
                entry(
                    "Formula",
                    "Done Qty",
                    &[("Expression", text_object("[qty]*[DONE]"))],
                ),
                entry(
                    "Formula",
                    "Odd",
                    &[("Expression", text_object("[Length] & \"x\""))],
                ),
                entry("Text", "Gone", &[("Deleted", Object::Boolean(true))]),
                entry("Signature", "Sign", &[]),
                entry("Date", "Due", &[]),
            ],
        }
    }

    fn square() -> Annotation {
        let o = Origin::new("Ada", PdfDate::from_unix(1_700_000_000));
        Annotation::square(0, Rect::new(0.0, 0.0, 10.0, 10.0), &o)
    }

    #[test]
    fn entries_become_columns_in_display_order() {
        let cols = sample().columns();
        let titles: Vec<&str> = cols.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(
            titles,
            [
                "Qty",
                "Priority",
                "Unit Cost",
                "Total",
                "Done",
                "Done Qty",
                "Odd",
                "Sign",
                "Due"
            ]
        );
        let get = |t: &str| cols.iter().find(|c| c.title == t).unwrap();
        assert_eq!(get("Qty").bluebeam.as_ref().unwrap().index, 1);
        assert_eq!(get("Qty").display, Some(NumberDisplay::places(0)));
        assert_eq!(get("Priority").options, ["Low", "High"]);
        assert_eq!(get("Total").kind, ColumnType::Formula);
        assert_eq!(get("Total").formula, "[bb_qty] * [bb_unit_cost]");
        assert_eq!(get("Done Qty").formula, "[bb_qty]*if([bb_done], 1, 0)");
        // A name that is not a column of the file: what Bluebeam stored, read-only.
        let odd = get("Odd");
        assert!(odd.is_read_only() && odd.kind == ColumnType::Number);
        assert_eq!(
            odd.bluebeam.as_ref().unwrap().expression,
            "[Length] & \"x\""
        );
        assert!(get("Sign").is_read_only());
        assert!(cols.iter().all(|c| c.title != "Gone"));
        assert!(cols
            .iter()
            .all(|c| c.bluebeam.as_ref().unwrap().width == 10));
    }

    #[test]
    fn values_are_positional_and_untouched_entries_keep_their_bytes() {
        let cols = sample().columns();
        let get = |t: &str| cols.iter().find(|c| c.title == t).unwrap();
        let mut a = square();
        // Literal (non-UTF-16) strings as another writer might use.
        let lit = |s: &str| Object::String(s.as_bytes().to_vec(), StringFormat::Literal);
        a.column_data = Some(vec![
            lit("High"),
            lit("abc"),
            lit("13.50"),
            lit(""),
            lit("yes"),
            lit(""),
            lit("7"),
            lit("kept"),
            lit(""),
            lit("D:20280229000000"),
            lit("beyond"),
        ]);
        assert_eq!(
            crate::custom_value(&a, get("Priority")),
            Value::text("High")
        );
        assert_eq!(crate::custom_value(&a, get("Qty")), Value::text("abc"));
        assert_eq!(
            crate::custom_value(&a, get("Unit Cost")),
            Value::Number(13.5)
        );
        assert_eq!(crate::custom_value(&a, get("Done")), Value::text("yes"));
        assert_eq!(crate::custom_value(&a, get("Odd")), Value::Number(7.0));
        assert_eq!(crate::custom_value(&a, get("Due")), Value::day(2028, 2, 29));

        let before = a.column_data.clone().unwrap();
        // The same number: nothing is written.
        assert_eq!(
            crate::set_custom_value(&mut a, get("Unit Cost"), Value::Number(13.5)),
            Ok(false)
        );
        assert_eq!(
            crate::set_custom_value(&mut a, get("Qty"), Value::Number(4.0)),
            Ok(true)
        );
        let after = a.column_data.clone().unwrap();
        assert_eq!(after.len(), before.len(), "an extra position is kept");
        for (i, (x, y)) in before.iter().zip(&after).enumerate() {
            if i == 1 {
                assert_eq!(*y, text_object("4"));
            } else {
                assert_eq!(x, y, "position {i}");
            }
        }
        assert!(matches!(
            crate::set_custom_value(&mut a, get("Odd"), Value::Number(1.0)),
            Err(ColumnError::ReadOnly(_))
        ));
        crate::set_custom_value(&mut a, get("Done"), Value::Bool(true)).unwrap();
        assert_eq!(stored_text(&a, 4).as_deref(), Some("True"));
        crate::set_custom_value(&mut a, get("Due"), Value::day(2026, 12, 25)).unwrap();
        assert_eq!(stored_text(&a, 9).as_deref(), Some("D:20261225000000"));
    }

    #[test]
    fn a_markup_without_data_gets_a_full_padded_array() {
        let cols = sample().columns();
        let due = cols.iter().find(|c| c.title == "Due").unwrap();
        let mut a = square();
        assert_eq!(
            crate::set_custom_value(&mut a, due, Value::Empty),
            Ok(false)
        );
        assert!(a.column_data.is_none());
        crate::set_custom_value(&mut a, due, Value::day(1999, 12, 31)).unwrap();
        let data = a.column_data.as_ref().unwrap();
        assert_eq!(data.len(), 10);
        assert_eq!(stored_text(&a, 0).as_deref(), Some(""));
        assert!(pad_column_data(&mut a, 12));
        assert_eq!(a.column_data.as_ref().unwrap().len(), 12);
        assert!(!pad_column_data(&mut a, 12));
    }

    #[test]
    fn odd_stored_text_is_shown_as_text() {
        assert_eq!(typed_value(ColumnType::Number, "1,5"), Value::text("1,5"));
        assert_eq!(
            typed_value(ColumnType::Number, "-12.5"),
            Value::Number(-12.5)
        );
        assert_eq!(typed_value(ColumnType::Number, "inf"), Value::text("inf"));
        assert_eq!(
            typed_value(ColumnType::Date, "tomorrow"),
            Value::text("tomorrow")
        );
        assert_eq!(
            typed_value(ColumnType::Date, "D:20270229000000"),
            Value::text("D:20270229000000")
        );
        assert_eq!(
            typed_value(ColumnType::Checkmark, "FALSE"),
            Value::Bool(false)
        );
        assert_eq!(
            typed_value(ColumnType::Choice, "Orphan"),
            Value::text("Orphan")
        );
    }

    #[test]
    fn a_column_is_added_in_bluebeams_structure() {
        let mut bb = sample();
        let c = bb
            .add("Phase", ColumnType::Choice, &["One", "Two"], false, None)
            .unwrap();
        assert_eq!(c.bluebeam.as_ref().unwrap().index, 10);
        assert_eq!(c.bluebeam.as_ref().unwrap().width, 11);
        let d = bb.entries[10].as_dict().unwrap();
        assert_eq!(
            d.get(b"Subtype").unwrap(),
            &Object::Name(b"Choice".to_vec())
        );
        // After the last shown (an entry without /DisplayOrder is shown at its position).
        assert_eq!(d.get(b"DisplayOrder").unwrap(), &Object::Integer(10));
        assert_eq!(d.get(b"Items").unwrap().as_array().unwrap().len(), 2);
        assert_eq!(
            bb.add("qty", ColumnType::Text, &[], false, None),
            Err(ColumnError::DuplicateTitle("qty".into()))
        );
        let notes = bb.add("Notes", ColumnType::Text, &[], true, None).unwrap();
        assert!(notes.multiline);
        assert!(bb.add("F", ColumnType::Formula, &[], false, None).is_err());
    }
}
