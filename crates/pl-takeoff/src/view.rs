//! Sort, group and filter, and the serialisable layout of the list.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

use crate::column::{Builtin, ColumnId, Schema};
use crate::rows::{Col, Row, Table};
use crate::value::Value;

/// One key of a sort.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortKey {
    pub column: ColumnId,
    #[serde(default)]
    pub descending: bool,
}

impl SortKey {
    pub fn asc(column: impl Into<ColumnId>) -> SortKey {
        SortKey {
            column: column.into(),
            descending: false,
        }
    }

    pub fn desc(column: impl Into<ColumnId>) -> SortKey {
        SortKey {
            column: column.into(),
            descending: true,
        }
    }
}

/// One level of grouping. Groups are ordered by their key, with the group of rows that have no
/// value last.
pub type GroupKey = SortKey;

/// A test on one column's value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Condition {
    /// The value is the same ([`Value::same`]: text without regard to case). `Equals(Empty)`
    /// is `IsEmpty`.
    Equals(Value),
    /// The value is one of these.
    OneOf(Vec<Value>),
    /// The cell's text contains this, without regard to case.
    Contains(String),
    /// The value is of the bounds' type and within them, both inclusive. A missing bound is
    /// open. An empty cell is never in a range.
    Range {
        min: Option<Value>,
        max: Option<Value>,
    },
    IsEmpty,
}

/// A [`Condition`] on a column.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnFilter {
    pub column: ColumnId,
    pub condition: Condition,
    /// Keeps the rows that do not meet the condition.
    #[serde(default)]
    pub negate: bool,
}

impl ColumnFilter {
    pub fn new(column: impl Into<ColumnId>, condition: Condition) -> ColumnFilter {
        ColumnFilter {
            column: column.into(),
            condition,
            negate: false,
        }
    }

    pub fn not(column: impl Into<ColumnId>, condition: Condition) -> ColumnFilter {
        ColumnFilter {
            column: column.into(),
            condition,
            negate: true,
        }
    }
}

/// Which rows are shown. A row passes when it meets the text search and every column filter.
///
/// A markup is shown with its whole thread when the markup itself or any reply in the thread
/// passes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Filter {
    /// Words to find, separated by white space. Each word must appear somewhere in the row's
    /// searched columns (not necessarily all in the same one), without regard to case.
    #[serde(default)]
    pub search: String,
    /// The columns the search reads; every column when empty.
    #[serde(default)]
    pub search_in: Vec<ColumnId>,
    #[serde(default)]
    pub conditions: Vec<ColumnFilter>,
}

impl Filter {
    pub fn search(text: &str) -> Filter {
        Filter {
            search: text.to_string(),
            ..Filter::default()
        }
    }

    pub fn with(mut self, filter: ColumnFilter) -> Filter {
        self.conditions.push(filter);
        self
    }

    /// True when nothing is filtered out.
    pub fn is_empty(&self) -> bool {
        self.search.trim().is_empty() && self.conditions.is_empty()
    }
}

fn condition_holds(condition: &Condition, row: &Row, col: Col, scratch: &mut String) -> bool {
    match condition {
        Condition::IsEmpty => row.value(col).is_empty(),
        Condition::Equals(v) => row.value(col).same(v),
        Condition::OneOf(values) => {
            let cell = row.value(col);
            values.iter().any(|v| cell.same(v))
        }
        Condition::Contains(needle) => {
            scratch.clear();
            row.display_into(col, scratch);
            contains_folded(scratch, &fold(needle))
        }
        Condition::Range { min, max } => {
            let cell = row.value(col);
            if cell.is_empty() {
                return false;
            }
            let same_type =
                |bound: &Value| std::mem::discriminant(bound) == std::mem::discriminant(&cell);
            min.as_ref()
                .is_none_or(|m| same_type(m) && cell.order(m) != Ordering::Less)
                && max
                    .as_ref()
                    .is_none_or(|m| same_type(m) && cell.order(m) != Ordering::Greater)
        }
    }
}

fn fold(s: &str) -> String {
    if s.is_ascii() {
        s.to_ascii_lowercase()
    } else {
        s.to_lowercase()
    }
}

/// `haystack` (any case) contains `needle` (already folded).
fn contains_folded(haystack: &mut str, needle: &str) -> bool {
    if haystack.is_ascii() {
        haystack.make_ascii_lowercase();
        haystack.contains(needle)
    } else {
        haystack.to_lowercase().contains(needle)
    }
}

/// A filter prepared against a table.
struct Matcher<'f> {
    terms: Vec<String>,
    search_cols: Vec<Col>,
    conditions: Vec<(Col, &'f ColumnFilter)>,
}

impl<'f> Matcher<'f> {
    fn new(table: &Table, filter: &'f Filter) -> Matcher<'f> {
        let search_cols = if filter.search_in.is_empty() {
            table.all_columns().iter().map(|c| table.col(c)).collect()
        } else {
            filter.search_in.iter().map(|c| table.col(c)).collect()
        };
        Matcher {
            terms: filter.search.split_whitespace().map(fold).collect(),
            search_cols,
            conditions: filter
                .conditions
                .iter()
                .map(|f| (table.col(&f.column), f))
                .collect(),
        }
    }

    fn passes(&self, row: &Row, scratch: &mut String) -> bool {
        for (col, f) in &self.conditions {
            if condition_holds(&f.condition, row, *col, scratch) == f.negate {
                return false;
            }
        }
        if self.terms.is_empty() {
            return true;
        }
        scratch.clear();
        for col in &self.search_cols {
            row.display_into(*col, scratch);
            // A separator no search word can contain, so a word never matches across cells.
            scratch.push('\n');
        }
        let folded;
        let text: &str = if scratch.is_ascii() {
            scratch.make_ascii_lowercase();
            scratch
        } else {
            folded = scratch.to_lowercase();
            &folded
        };
        self.terms.iter().all(|t| text.contains(t.as_str()))
    }
}

/// The sum of one column over the markups of a group (replies are not counted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subtotal {
    pub column: ColumnId,
    pub sum: f64,
    /// How many of the group's markups have a number in the column.
    pub values: usize,
}

/// What a group holds: more groups, or rows.
#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Groups(Vec<Group>),
    /// Indices into [`Table::rows`] of markups (never replies), in display order.
    Rows(Vec<usize>),
}

/// The rows that share one value of a grouping column.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub column: ColumnId,
    /// The value the rows share (the first row's, where case differs).
    pub key: Value,
    /// The key as text; `(none)` for the rows with no value.
    pub label: String,
    /// Markups in the group, nested groups included.
    pub count: usize,
    /// One entry per summed column of the table.
    pub subtotals: Vec<Subtotal>,
    pub body: Body,
}

/// The label of the group of rows that have no value in the grouping column.
pub const NO_VALUE_LABEL: &str = "(none)";

/// One line of the list as it is shown.
#[derive(Debug, Clone, PartialEq)]
pub enum Line<'a> {
    Group {
        /// 0 for the outermost grouping.
        depth: usize,
        /// The keys from the outermost group down to this one.
        path: Vec<&'a Value>,
        group: &'a Group,
        collapsed: bool,
    },
    Row {
        /// Index into [`Table::rows`].
        row: usize,
        /// How many groups enclose the row.
        group_depth: usize,
        /// 0 for a markup, 1 and up for replies.
        reply_depth: usize,
    },
}

/// The list after filter, sort and group.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub body: Body,
    /// Markups shown.
    pub count: usize,
    /// Markups before the filter.
    pub total: usize,
    /// One entry per summed column of the table, over the markups shown.
    pub subtotals: Vec<Subtotal>,
}

fn compare(a: &Value, b: &Value, descending: bool) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        // Empty cells stay last whichever way the column is sorted.
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ if descending => b.order(a),
        _ => a.order(b),
    }
}

struct Summer {
    columns: Vec<(ColumnId, Col)>,
}

impl Summer {
    fn sum(&self, table: &Table, rows: &[usize]) -> Vec<Subtotal> {
        self.columns
            .iter()
            .map(|(id, col)| {
                let mut sum = 0.0;
                let mut values = 0;
                for r in rows {
                    if let Some(n) = table.rows[*r].value(*col).as_number() {
                        sum += n;
                        values += 1;
                    }
                }
                Subtotal {
                    column: id.clone(),
                    sum,
                    values,
                }
            })
            .collect()
    }
}

fn group_rows(table: &Table, rows: Vec<usize>, keys: &[GroupKey], summer: &Summer) -> Body {
    let Some((key, rest)) = keys.split_first() else {
        return Body::Rows(rows);
    };
    let col = table.col(&key.column);
    let mut keyed: Vec<(Value, usize)> = rows
        .into_iter()
        .map(|r| (table.rows[r].value(col), r))
        .collect();
    // Stable: rows keep their sorted order inside a group.
    keyed.sort_by(|a, b| compare(&a.0, &b.0, key.descending));
    let mut groups: Vec<Group> = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut run_key: Option<Value> = None;
    let mut close = |run_key: &mut Option<Value>, run: &mut Vec<usize>| {
        if let Some(k) = run_key.take() {
            let members = std::mem::take(run);
            let k = if k.is_empty() { Value::Empty } else { k };
            groups.push(Group {
                column: key.column.clone(),
                label: if k.is_empty() {
                    NO_VALUE_LABEL.to_string()
                } else {
                    k.display()
                },
                key: k,
                count: members.len(),
                subtotals: summer.sum(table, &members),
                body: group_rows(table, members, rest, summer),
            });
        }
    };
    for (value, row) in keyed {
        if run_key.as_ref().is_some_and(|k| !k.same(&value)) {
            close(&mut run_key, &mut run);
        }
        if run_key.is_none() {
            run_key = Some(value);
        }
        run.push(row);
    }
    close(&mut run_key, &mut run);
    Body::Groups(groups)
}

impl View {
    /// Filters, sorts and groups the table's markups.
    ///
    /// * **Filter:** see [`Filter`].
    /// * **Sort:** by the keys in turn. The sort is stable, so rows that tie on every key stay
    ///   in document order. Text is in natural order ([`crate::natural_cmp`]). Rows with no
    ///   value in a column come last whether the column is ascending or descending.
    /// * **Group:** nested, one level per key, each group with its count and the subtotal of
    ///   every summed column. A column the table does not have groups everything under
    ///   `(none)`.
    ///
    /// Replies are not sorted or grouped; they stay under their markup ([`View::lines`]).
    pub fn build(table: &Table, filter: &Filter, sort: &[SortKey], group: &[GroupKey]) -> View {
        let total = table.roots.len();
        let mut roots: Vec<usize> = if filter.is_empty() {
            table.roots.clone()
        } else {
            let matcher = Matcher::new(table, filter);
            let mut scratch = String::new();
            table
                .roots
                .iter()
                .copied()
                .filter(|root| {
                    let mut stack = vec![*root];
                    while let Some(i) = stack.pop() {
                        let row = &table.rows[i];
                        if matcher.passes(row, &mut scratch) {
                            return true;
                        }
                        stack.extend(row.children.iter().copied());
                    }
                    false
                })
                .collect()
        };

        if !sort.is_empty() {
            // Keys are read once per row, then positions are sorted.
            let cols: Vec<Col> = sort.iter().map(|k| table.col(&k.column)).collect();
            let keys: Vec<Vec<Value>> = roots
                .iter()
                .map(|r| cols.iter().map(|c| table.rows[*r].value(*c)).collect())
                .collect();
            let mut order: Vec<usize> = (0..roots.len()).collect();
            order.sort_by(|a, b| {
                for (i, key) in sort.iter().enumerate() {
                    let o = compare(&keys[*a][i], &keys[*b][i], key.descending);
                    if o != Ordering::Equal {
                        return o;
                    }
                }
                Ordering::Equal
            });
            roots = order.into_iter().map(|i| roots[i]).collect();
        }

        let summer = Summer {
            columns: table
                .summed_columns()
                .into_iter()
                .map(|c| {
                    let col = table.col(&c);
                    (c, col)
                })
                .collect(),
        };
        View {
            count: roots.len(),
            total,
            subtotals: summer.sum(table, &roots),
            body: group_rows(table, roots, group, &summer),
        }
    }

    /// [`View::build`] with the filter, sort and grouping of a layout.
    pub fn of(table: &Table, layout: &Layout) -> View {
        View::build(table, &layout.filter, &layout.sort, &layout.group)
    }

    /// The markups shown, in display order.
    pub fn rows(&self) -> Vec<usize> {
        fn walk(body: &Body, out: &mut Vec<usize>) {
            match body {
                Body::Rows(rows) => out.extend(rows),
                Body::Groups(groups) => groups.iter().for_each(|g| walk(&g.body, out)),
            }
        }
        let mut out = Vec::with_capacity(self.count);
        walk(&self.body, &mut out);
        out
    }

    /// The list line by line: group headings, markups, and under each markup its replies when
    /// `show_replies` is set. A group whose key path is in `collapsed` is one line.
    pub fn lines<'a>(
        &'a self,
        table: &Table,
        show_replies: bool,
        collapsed: &[Vec<Value>],
    ) -> Vec<Line<'a>> {
        fn walk<'a>(
            body: &'a Body,
            table: &Table,
            show_replies: bool,
            collapsed: &[Vec<Value>],
            path: &mut Vec<&'a Value>,
            out: &mut Vec<Line<'a>>,
        ) {
            match body {
                Body::Rows(rows) => {
                    for r in rows {
                        out.push(Line::Row {
                            row: *r,
                            group_depth: path.len(),
                            reply_depth: 0,
                        });
                        if show_replies {
                            for reply in table.thread(*r) {
                                out.push(Line::Row {
                                    row: reply,
                                    group_depth: path.len(),
                                    reply_depth: table.rows[reply].depth,
                                });
                            }
                        }
                    }
                }
                Body::Groups(groups) => {
                    for g in groups {
                        path.push(&g.key);
                        let is_collapsed = collapsed.iter().any(|c| {
                            c.len() == path.len()
                                && c.iter().zip(path.iter()).all(|(a, b)| a.same(b))
                        });
                        out.push(Line::Group {
                            depth: path.len() - 1,
                            path: path.clone(),
                            group: g,
                            collapsed: is_collapsed,
                        });
                        if !is_collapsed {
                            walk(&g.body, table, show_replies, collapsed, path, out);
                        }
                        path.pop();
                    }
                }
            }
        }
        let mut out = Vec::new();
        walk(
            &self.body,
            table,
            show_replies,
            collapsed,
            &mut Vec::new(),
            &mut out,
        );
        out
    }
}

/// One column of the layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnState {
    pub column: ColumnId,
    pub visible: bool,
    /// Logical pixels.
    pub width: f32,
}

/// How the list is laid out and what it shows: everything the UI needs to put it back the way
/// the user left it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub version: u32,
    /// Every column, in display order.
    pub columns: Vec<ColumnState>,
    #[serde(default)]
    pub sort: Vec<SortKey>,
    #[serde(default)]
    pub group: Vec<GroupKey>,
    #[serde(default)]
    pub filter: Filter,
    #[serde(default = "yes")]
    pub show_replies: bool,
    /// Key paths of the groups shown closed.
    #[serde(default)]
    pub collapsed: Vec<Vec<Value>>,
}

fn yes() -> bool {
    true
}

impl Default for Layout {
    fn default() -> Layout {
        Layout::for_schema(&Schema::new())
    }
}

impl Layout {
    pub const VERSION: u32 = 1;
    pub const MIN_WIDTH: f32 = 24.0;
    pub const MAX_WIDTH: f32 = 2000.0;
    /// The width a user column starts with.
    pub const CUSTOM_WIDTH: f32 = 110.0;

    /// The starting layout: the built-in columns (the common ones visible), then the user's,
    /// all visible; no sort, grouping or filter.
    pub fn for_schema(schema: &Schema) -> Layout {
        let mut layout = Layout {
            version: Layout::VERSION,
            columns: Builtin::ALL
                .into_iter()
                .map(|b| ColumnState {
                    column: ColumnId::Builtin(b),
                    visible: b.default_visible(),
                    width: b.default_width(),
                })
                .collect(),
            sort: Vec::new(),
            group: Vec::new(),
            filter: Filter::default(),
            show_replies: true,
            collapsed: Vec::new(),
        };
        layout.reconcile(schema);
        layout
    }

    /// The built-in columns a takeoff layout shows, in order ([`Layout::takeoff`]): what was
    /// measured, the adding quantities (Count, Length, Perimeter, Area, Volume, Base Area, Wall
    /// Area), then the ones that do not add up (Angle, Radius, Diameter), scale, space and
    /// comment.
    pub const TAKEOFF_COLUMNS: [Builtin; 16] = [
        Builtin::Subject,
        Builtin::PageLabel,
        Builtin::Kind,
        Builtin::Count,
        Builtin::Length,
        Builtin::Perimeter,
        Builtin::Area,
        Builtin::Volume,
        Builtin::BaseArea,
        Builtin::WallArea,
        Builtin::Angle,
        Builtin::Radius,
        Builtin::Diameter,
        Builtin::Scale,
        Builtin::Space,
        Builtin::Comment,
    ];

    /// The list laid out for a takeoff: [`Layout::TAKEOFF_COLUMNS`] shown in that order, then
    /// the user's columns (shown), then the other built-in columns (hidden); grouped by
    /// subject, so every subject has its subtotals. A Volume's base area is in Base Area, never
    /// in Area, so the Area total is areas only.
    pub fn takeoff(schema: &Schema) -> Layout {
        let mut layout = Layout::for_schema(schema);
        let mut ordered: Vec<ColumnState> = Vec::with_capacity(layout.columns.len());
        for b in Layout::TAKEOFF_COLUMNS {
            if let Some(state) = layout
                .columns
                .iter()
                .find(|c| c.column == ColumnId::Builtin(b))
            {
                ordered.push(ColumnState {
                    visible: true,
                    ..state.clone()
                });
            }
        }
        for c in &layout.columns {
            if !ordered.iter().any(|o| o.column == c.column) {
                ordered.push(ColumnState {
                    visible: matches!(c.column, ColumnId::Custom(_)),
                    ..c.clone()
                });
            }
        }
        layout.columns = ordered;
        layout.group = vec![GroupKey::asc(Builtin::Subject)];
        layout
    }

    /// Brings a stored layout in line with the schema as it is now: columns the schema no
    /// longer has are dropped (from sort, grouping and filter too), new ones are added at the
    /// end, visible; a built-in column the stored layout predates is added hidden; duplicates
    /// are removed and widths clamped.
    pub fn reconcile(&mut self, schema: &Schema) {
        let known = |c: &ColumnId| match c {
            ColumnId::Builtin(_) => true,
            ColumnId::Custom(id) => schema.get(id).is_some(),
        };
        let mut seen: Vec<ColumnId> = Vec::new();
        self.columns.retain(|c| {
            let keep = known(&c.column) && !seen.contains(&c.column);
            if keep {
                seen.push(c.column.clone());
            }
            keep
        });
        for b in Builtin::ALL {
            if !seen.contains(&ColumnId::Builtin(b)) {
                self.columns.push(ColumnState {
                    column: ColumnId::Builtin(b),
                    visible: false,
                    width: b.default_width(),
                });
            }
        }
        for c in &schema.columns {
            if !seen.contains(&c.column_id()) {
                self.columns.push(ColumnState {
                    column: c.column_id(),
                    visible: true,
                    width: Layout::CUSTOM_WIDTH,
                });
            }
        }
        for c in &mut self.columns {
            c.width = clamp_width(c.width);
        }
        self.sort.retain(|k| known(&k.column));
        self.group.retain(|k| known(&k.column));
        self.filter.conditions.retain(|f| known(&f.column));
        self.filter.search_in.retain(known);
        let levels = self.group.len();
        self.collapsed
            .retain(|p| !p.is_empty() && p.len() <= levels);
    }

    /// The visible columns in display order.
    pub fn visible(&self) -> Vec<ColumnId> {
        self.columns
            .iter()
            .filter(|c| c.visible)
            .map(|c| c.column.clone())
            .collect()
    }

    fn state_mut(&mut self, column: &ColumnId) -> Option<&mut ColumnState> {
        self.columns.iter_mut().find(|c| c.column == *column)
    }

    pub fn state(&self, column: &ColumnId) -> Option<&ColumnState> {
        self.columns.iter().find(|c| c.column == *column)
    }

    /// Shows or hides a column. Returns false when the layout has no such column.
    pub fn set_visible(&mut self, column: &ColumnId, visible: bool) -> bool {
        self.state_mut(column)
            .map(|c| c.visible = visible)
            .is_some()
    }

    /// Sets a column's width, clamped to [`Layout::MIN_WIDTH`]..=[`Layout::MAX_WIDTH`].
    pub fn set_width(&mut self, column: &ColumnId, width: f32) -> bool {
        self.state_mut(column)
            .map(|c| c.width = clamp_width(width))
            .is_some()
    }

    /// Moves a column so that it sits just before `before` (at the end for `None`).
    pub fn move_column(&mut self, column: &ColumnId, before: Option<&ColumnId>) -> bool {
        if before == Some(column) {
            return self.state(column).is_some();
        }
        let Some(from) = self.columns.iter().position(|c| c.column == *column) else {
            return false;
        };
        let moved = self.columns.remove(from);
        let to = before
            .and_then(|b| self.columns.iter().position(|c| c.column == *b))
            .unwrap_or(self.columns.len());
        self.columns.insert(to, moved);
        true
    }

    /// What a click on a column heading does: sort by that column ascending, then descending,
    /// then not at all. With `add` the column joins (or changes within) the existing keys;
    /// without it, it replaces them.
    pub fn toggle_sort(&mut self, column: &ColumnId, add: bool) {
        let current = self
            .sort
            .iter()
            .position(|k| k.column == *column)
            .map(|i| (i, self.sort[i].descending));
        if !add {
            let only = self.sort.len() == 1;
            self.sort.clear();
            match current {
                Some((_, false)) if only => self.sort.push(SortKey::desc(column.clone())),
                Some((_, true)) if only => {}
                _ => self.sort.push(SortKey::asc(column.clone())),
            }
            return;
        }
        match current {
            None => self.sort.push(SortKey::asc(column.clone())),
            Some((i, false)) => self.sort[i].descending = true,
            Some((i, true)) => {
                self.sort.remove(i);
            }
        }
    }
}

fn clamp_width(w: f32) -> f32 {
    if w.is_finite() {
        w.clamp(Layout::MIN_WIDTH, Layout::MAX_WIDTH)
    } else {
        Layout::CUSTOM_WIDTH
    }
}
