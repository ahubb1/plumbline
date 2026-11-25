//! Evaluates the formula columns of a [`Table`], row by row, in dependency order.

use crate::column::{Builtin, ColumnType};
use crate::formula::{Formula, FormulaError, Scalar};
use crate::rows::{Col, Row, Table};
use crate::value::Value;

/// Where a name in a formula gets its value.
#[derive(Debug, Clone)]
enum Source {
    Col(Col),
    /// A choice column whose options carry numbers.
    Choice(usize),
    /// Another formula column, by its index among the table's user columns.
    Formula(usize),
    Unknown(String),
}

/// What a formula column amounts to before any row is evaluated.
enum Plan {
    /// Every cell is this error (the formula does not parse, or is part of a loop).
    Fails(FormulaError),
    Runs(Formula, Vec<Source>),
}

fn fold(s: &str) -> String {
    s.trim().to_lowercase()
}

/// The source of a bracketed name: a user column by heading, then by id, then a built-in
/// column by heading.
fn resolve(table: &Table, name: &str) -> Source {
    let key = fold(name);
    let custom = table
        .custom
        .iter()
        .position(|c| fold(&c.title) == key)
        .or_else(|| table.custom.iter().position(|c| c.id.to_lowercase() == key));
    if let Some(i) = custom {
        let c = &table.custom[i];
        return match c.kind {
            ColumnType::Formula => Source::Formula(i),
            ColumnType::Choice if !c.option_values.is_empty() => Source::Choice(i),
            _ => Source::Col(Col::Custom(i)),
        };
    }
    match Builtin::ALL.into_iter().find(|b| fold(b.title()) == key) {
        Some(b) => Source::Col(Col::Builtin(b)),
        None => Source::Unknown(name.trim().to_string()),
    }
}

pub(crate) fn scalar_of(value: Value) -> Scalar {
    match value {
        Value::Empty => Scalar::Empty,
        Value::Number(n) => Scalar::Number(n),
        Value::Text(s) => Scalar::Text(s),
        Value::Bool(b) => Scalar::Bool(b),
        other => Scalar::Text(other.display()),
    }
}

fn value_of(s: Scalar) -> Value {
    match s {
        Scalar::Empty => Value::Empty,
        Scalar::Number(n) => Value::number(n),
        Scalar::Text(t) => Value::text(t),
        Scalar::Bool(b) => Value::Bool(b),
        Scalar::Choice { number, .. } => Value::number(number),
    }
}

fn fetch(table: &Table, row: &Row, source: &Source) -> Result<Scalar, FormulaError> {
    match source {
        Source::Col(col) => Ok(scalar_of(row.value(*col))),
        Source::Choice(i) => {
            let v = row.custom.get(*i).cloned().unwrap_or_default();
            match table.custom[*i].option_value(&v) {
                Some(number) => Ok(Scalar::Choice {
                    text: v.display(),
                    number,
                }),
                None => Ok(scalar_of(v)),
            }
        }
        Source::Formula(i) => match row.formula_errors.iter().find(|(c, _)| c == i) {
            Some((_, e)) => Err(e.clone()),
            None => Ok(scalar_of(row.custom.get(*i).cloned().unwrap_or_default())),
        },
        Source::Unknown(name) => Err(FormulaError::UnknownColumn(name.clone())),
    }
}

fn store(row: &mut Row, column: usize, result: Result<Scalar, FormulaError>) {
    match result {
        Ok(s) => row.custom[column] = value_of(s),
        Err(e) => {
            row.custom[column] = Value::text(e.code());
            row.formula_errors.push((column, e));
        }
    }
}

/// Evaluates every formula column of every row and records each column's own problem (one
/// that does not parse or is part of a loop) in [`Table::formula_status`].
pub(crate) fn recalculate(table: &mut Table) {
    let formula_columns: Vec<usize> = (0..table.custom.len())
        .filter(|i| table.custom[*i].kind == ColumnType::Formula)
        .collect();
    for row in &mut table.rows {
        row.formula_errors.clear();
    }
    table.formula_status.clear();
    if formula_columns.is_empty() {
        return;
    }
    let n = formula_columns.len();
    let position = |custom: usize| formula_columns.iter().position(|c| *c == custom);

    // Parse and resolve.
    let mut plans: Vec<Plan> = formula_columns
        .iter()
        .map(|i| match Formula::parse(&table.custom[*i].formula) {
            Ok(f) => {
                let sources = f.references.iter().map(|r| resolve(table, r)).collect();
                Plan::Runs(f, sources)
            }
            Err(e) => Plan::Fails(e),
        })
        .collect();

    // Wall Area is worked out from the height column; when that is a formula column, Wall
    // Area comes after it, and a formula that uses Wall Area depends on it.
    let height_formula = table.height_column.filter(|h| {
        table
            .custom
            .get(*h)
            .is_some_and(|c| c.kind == ColumnType::Formula)
    });
    let height_position = height_formula.and_then(position);

    // Dependencies between formula columns, and the loops among them.
    let deps: Vec<Vec<usize>> = plans
        .iter()
        .map(|p| match p {
            Plan::Runs(_, sources) => sources
                .iter()
                .filter_map(|s| match s {
                    Source::Formula(c) => position(*c),
                    Source::Col(Col::Builtin(Builtin::WallArea)) => height_position,
                    _ => None,
                })
                .collect(),
            Plan::Fails(_) => Vec::new(),
        })
        .collect();
    let reach: Vec<Vec<bool>> = (0..n)
        .map(|start| {
            let mut seen = vec![false; n];
            let mut stack: Vec<usize> = deps[start].clone();
            while let Some(k) = stack.pop() {
                if !seen[k] {
                    seen[k] = true;
                    stack.extend(deps[k].iter().copied());
                }
            }
            seen
        })
        .collect();
    for k in 0..n {
        if reach[k][k] {
            let members: Vec<String> = (0..n)
                .filter(|j| reach[k][*j] && reach[*j][k])
                .map(|j| table.custom[formula_columns[j]].title.clone())
                .collect();
            plans[k] = Plan::Fails(FormulaError::Cycle(members));
        }
    }

    // Evaluation order: what a column uses comes before it.
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut state = vec![0u8; n];
    fn visit(
        k: usize,
        deps: &[Vec<usize>],
        plans: &[Plan],
        state: &mut [u8],
        order: &mut Vec<usize>,
    ) {
        if state[k] != 0 {
            return;
        }
        state[k] = 1;
        if let Plan::Runs(..) = plans[k] {
            for d in &deps[k] {
                visit(*d, deps, plans, state, order);
            }
        }
        state[k] = 2;
        order.push(k);
    }
    for k in 0..n {
        visit(k, &deps, &plans, &mut state, &mut order);
    }

    table.formula_status = formula_columns
        .iter()
        .zip(&plans)
        .map(|(c, p)| {
            (
                table.custom[*c].id.clone(),
                match p {
                    Plan::Fails(e) => Some(e.clone()),
                    Plan::Runs(..) => None,
                },
            )
        })
        .collect();

    let units = table.units;
    let height_unit = table.height_unit;
    let mut rows = std::mem::take(&mut table.rows);
    for row in &mut rows {
        for k in &order {
            let column = formula_columns[*k];
            let result = match &plans[*k] {
                Plan::Fails(e) => Err(e.clone()),
                Plan::Runs(f, sources) => {
                    let row_ref: &Row = row;
                    // A Bluebeam formula with nothing in any of its columns (a reply, a form
                    // field, a markup no value was given) is left blank, as Bluebeam stores
                    // it, rather than worked out to 0.
                    let blank = table.custom[column].bluebeam.is_some()
                        && !sources.is_empty()
                        && sources
                            .iter()
                            .all(|s| matches!(fetch(table, row_ref, s), Ok(Scalar::Empty)));
                    if blank {
                        Ok(Scalar::Empty)
                    } else {
                        f.eval(&mut |i| fetch(table, row_ref, &sources[i]))
                    }
                }
            };
            let failed = result.is_err();
            store(row, column, result);
            if Some(column) == height_formula {
                let height = (!failed)
                    .then(|| row.custom[column].as_number())
                    .flatten()
                    .map(|v| height_unit.to_metres(v));
                row.set_wall_area(&units, height);
            }
        }
    }
    table.rows = rows;
}
