//! The takeoff summary: quantities totalled by subject, layer, space or any other column, as
//! plain data for a report or a sheet.

use serde::{Deserialize, Serialize};

use crate::column::{Builtin, ColumnId};
use crate::rows::Table;
use crate::view::{Body, Filter, GroupKey, View};

/// A column the summary totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryColumn {
    pub column: ColumnId,
    pub title: String,
    /// The unit of a quantity column.
    pub unit: Option<String>,
    /// The decimal places the column is written with, when it has a fixed number.
    pub places: Option<u8>,
}

/// One group of the summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryLine {
    /// 0 for the outermost grouping.
    pub depth: usize,
    /// The labels from the outermost group down to this one.
    pub path: Vec<String>,
    /// Markups in the group.
    pub count: usize,
    /// One entry per [`TakeoffSummary::columns`]: the sum, when any markup has a number there.
    pub totals: Vec<Option<f64>>,
}

/// Quantities totalled per group, nested in the order of the grouping columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeoffSummary {
    /// The grouping columns, outermost first, and their headings.
    pub by: Vec<(ColumnId, String)>,
    pub columns: Vec<SummaryColumn>,
    /// Every group, each followed by its nested groups.
    pub lines: Vec<SummaryLine>,
    pub count: usize,
    pub totals: Vec<Option<f64>>,
}

fn sums(columns: &[SummaryColumn], subtotals: &[crate::view::Subtotal]) -> Vec<Option<f64>> {
    columns
        .iter()
        .map(|c| {
            subtotals
                .iter()
                .find(|s| s.column == c.column && s.values > 0)
                .map(|s| s.sum)
        })
        .collect()
}

impl TakeoffSummary {
    /// Totals every summed column of the table that holds any number ([`Table::is_summed`]:
    /// the adding quantities, number and formula columns) per group of `by` (for instance
    /// `[Subject]`, `[Layer]`, `[Space, Subject]`), over the markups `filter` keeps.
    pub fn build(table: &Table, by: &[ColumnId], filter: &Filter) -> TakeoffSummary {
        let keys: Vec<GroupKey> = by.iter().cloned().map(GroupKey::asc).collect();
        let view = View::build(table, filter, &[], &keys);
        let columns: Vec<SummaryColumn> = view
            .subtotals
            .iter()
            .filter(|s| s.values > 0)
            .map(|s| SummaryColumn {
                column: s.column.clone(),
                title: table.title(&s.column),
                unit: table.unit_label(&s.column).map(str::to_string),
                places: table.places(&s.column),
            })
            .collect();
        let mut lines = Vec::new();
        fn walk(
            body: &Body,
            depth: usize,
            path: &mut Vec<String>,
            columns: &[SummaryColumn],
            out: &mut Vec<SummaryLine>,
        ) {
            if let Body::Groups(groups) = body {
                for g in groups {
                    path.push(g.label.clone());
                    out.push(SummaryLine {
                        depth,
                        path: path.clone(),
                        count: g.count,
                        totals: sums(columns, &g.subtotals),
                    });
                    walk(&g.body, depth + 1, path, columns, out);
                    path.pop();
                }
            }
        }
        walk(&view.body, 0, &mut Vec::new(), &columns, &mut lines);
        TakeoffSummary {
            by: by.iter().map(|c| (c.clone(), table.title(c))).collect(),
            totals: sums(&columns, &view.subtotals),
            columns,
            lines,
            count: view.count,
        }
    }

    /// Totals by subject.
    pub fn by_subject(table: &Table) -> TakeoffSummary {
        TakeoffSummary::build(table, &[Builtin::Subject.into()], &Filter::default())
    }

    /// Totals by layer.
    pub fn by_layer(table: &Table) -> TakeoffSummary {
        TakeoffSummary::build(table, &[Builtin::Layer.into()], &Filter::default())
    }

    /// Totals by space, then subject.
    pub fn by_space(table: &Table) -> TakeoffSummary {
        TakeoffSummary::build(
            table,
            &[Builtin::Space.into(), Builtin::Subject.into()],
            &Filter::default(),
        )
    }

    /// The total of one column over everything, when it is in the summary.
    pub fn total(&self, column: &ColumnId) -> Option<f64> {
        let at = self.columns.iter().position(|c| c.column == *column)?;
        self.totals[at]
    }
}
