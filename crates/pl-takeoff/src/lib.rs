//! pl-takeoff: the markups list (see docs/plan.md sections 6 and 8, milestone M5).
//!
//! The list is built from pl-annot annotations and plain data; nothing here knows about
//! pl-doc, a window or a file on disk.
//!
//! * [`Table::build`] turns the annotations of a document into [`Row`]s: one per markup, with
//!   the replies made to it (`/IRT`) as child rows, its review status and checkmark folded in
//!   from the status annotations, and one [`Value`] per user column.
//! * [`Builtin`] names the built-in columns, among them the quantity columns of the takeoff
//!   (Length, Area, Perimeter, Volume, Base Area, Wall Area, Count, Angle, Radius, Diameter,
//!   Depth, Scale, Unit),
//!   worked out from each measurement annotation and its scale and shown in one unit per
//!   quantity for the whole list ([`QuantityUnits`], [`quantity`]).
//! * Formula columns ([`formula`]): our own expression language over the row's columns,
//!   evaluated per row in dependency order, with typed errors shown in the cell.
//! * Choice columns whose options stand for numbers, number columns with a display format
//!   (rates, costs), and totals and subtotals for every number and formula column.
//! * [`legend`]: legends evaluated to plain data and laid out as a pl-draw `DrawList`;
//!   [`space`]: named regions each markup is assigned to; [`sequence`]: auto-numbering;
//!   [`TakeoffSummary`]: totals by subject, layer or space.
//! * [`Schema`] is the user's own columns (text, number, choice, checkmark, date), kept per
//!   document and serialisable. Their values live in each annotation under pl-annot's one
//!   private key ([`set_custom_value`], [`custom_value`]; see [`column`] for the layout).
//! * [`View::build`] filters, sorts and groups; [`Layout`] is the serialisable state of the
//!   list (column order, visibility and widths, sort, grouping, filter, collapsed groups).
//! * [`to_csv`], [`to_xlsx`] and [`Report`] export the list as displayed.
//! * [`export_xfdf`] and [`import_xfdf`] exchange the annotations themselves as XFDF.
//!
//! Positions and sizes are in unrotated page user space (PDF points, y up), like everything in
//! pl-annot; turning them into what the user sees on a rotated page is the view's business.

pub mod bluebeam;
mod calc;
pub mod column;
mod export;
pub mod formula;
pub mod legend;
pub mod quantity;
mod rows;
pub mod sequence;
pub mod space;
mod summary;
mod value;
mod view;
pub mod xfdf;
pub mod xml;

pub use bluebeam::{BluebeamColumn, BluebeamColumns, ANNOT_COLUMNS_KEY, COLUMN_DATA_KEY};
pub use column::{
    clear_custom_value, custom_value, set_custom_value, stored_column_ids, Builtin, ColumnError,
    ColumnId, ColumnType, CustomColumn, NumberDisplay, Schema, COLUMNS_KEY,
};
pub use export::{
    file_number, to_csv, to_xlsx, CsvOptions, ExportError, Report, ReportColumn, ReportLine,
    FILE_EXTRA_PLACES, SHEET_GROUPS, SHEET_ROWS,
};
pub use formula::{evaluate, Formula, FormulaError, Scalar};
pub use legend::{
    legend_draw_list, Legend, LegendDef, LegendDrawing, LegendRow, LegendStyle, Swatch, SwatchShape,
};
pub use quantity::{QuantityKind, QuantityUnits, RowMeasure};
pub use rows::{
    checkmark_status, count_group_key, Col, CountGroupKey, ListContext, Row, RowOptions, Table,
    TakeoffContext, MARKED, MARKED_MODEL, UNMARKED,
};
pub use sequence::Sequence;
pub use space::{anchor, set_space_name, space_at, space_name, spaces_in, Space, SPACE_KEY};
pub use summary::{SummaryColumn, SummaryLine, TakeoffSummary};
pub use value::{format_date, is_day, natural_cmp, parse_date, Value};
pub use view::{
    Body, ColumnFilter, ColumnState, Condition, Filter, Group, GroupKey, Layout, Line, SortKey,
    Subtotal, View, NO_VALUE_LABEL,
};
pub use xfdf::{
    export_xfdf, import_xfdf, SkipReason, XfdfError, XfdfExport, XfdfImport, XfdfOptions,
};
