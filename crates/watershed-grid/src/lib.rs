//! A data grid on the watershed engine, compiled to WebAssembly.
//!
//! Rows live in a watershed [`Collection`](watershed_core::Collection): each
//! cell is a node, calculated columns are formulas, and the totals in the
//! bottom row and in group rows are aggregates. So editing a cell, or a
//! stream of ticking prices, recalculates only the cells and totals that
//! depend on what changed.
//!
//! Filtering, sorting and grouping happen here too, in [`Table`]. The page
//! (`web/grid/`) only draws: it asks for the lines in view as JSON and
//! renders that window, so a hundred thousand rows cost the same to show as
//! fifty.

use wasm_bindgen::prelude::*;

mod demo;
pub mod formula;
pub mod table;

pub use table::{Agg, Kind, Table, Update};

fn js_error(message: String) -> JsError {
    JsError::new(&message)
}

/// The grid, as JavaScript sees it. See [`Table`] for what each call does.
#[wasm_bindgen]
pub struct Grid {
    table: Table,
    demo: Option<demo::Demo>,
}

impl Default for Grid {
    fn default() -> Self {
        Grid::new()
    }
}

#[wasm_bindgen]
impl Grid {
    /// A new grid with no columns.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Grid {
        Grid {
            table: Table::new(),
            demo: None,
        }
    }

    /// Adds a field: `kind` is "number", "text" or "bool", and `agg` is
    /// "sum", "avg", "min", "max", "count" or "none". Returns the column's position.
    pub fn add_field(&mut self, name: &str, kind: &str, agg: &str) -> Result<u32, JsError> {
        let (kind, agg) = (
            Kind::parse(kind).map_err(js_error)?,
            Agg::parse(agg).map_err(js_error)?,
        );
        self.table
            .add_field(name, kind, agg)
            .map(|col| col as u32)
            .map_err(js_error)
    }

    /// Adds a calculated column, such as `qty * (mark - price)`. Returns its position.
    pub fn add_formula(&mut self, name: &str, formula: &str, agg: &str) -> Result<u32, JsError> {
        let agg = Agg::parse(agg).map_err(js_error)?;
        self.table
            .add_formula(name, formula, agg)
            .map(|col| col as u32)
            .map_err(js_error)
    }

    /// Adds a named number that formulas can use.
    pub fn add_value(&mut self, name: &str, value: f64) -> Result<(), JsError> {
        self.table.add_value(name, value).map_err(js_error)
    }

    /// Changes a named number.
    pub fn set_value(&mut self, name: &str, value: f64) -> Result<(), JsError> {
        self.table.set_value(name, value).map_err(js_error)
    }

    /// Adds or replaces a row: one text per field, read as the field's kind.
    pub fn upsert(&mut self, key: &str, values: Vec<String>) -> Result<(), JsError> {
        let kinds = self.table.field_kinds();
        if kinds.len() != values.len() {
            return Err(js_error(format!(
                "a row needs {} values, one per field, and this has {}",
                kinds.len(),
                values.len()
            )));
        }
        let values = kinds
            .into_iter()
            .zip(&values)
            .map(|(kind, text)| kind.read(text))
            .collect::<Result<Vec<_>, _>>()
            .map_err(js_error)?;
        self.table.upsert(key, values).map_err(js_error)
    }

    /// Sets one cell from text.
    pub fn set_cell(&mut self, key: &str, col: u32, text: &str) -> Result<(), JsError> {
        self.table
            .set_cell(key, col as usize, text)
            .map_err(js_error)
    }

    /// Removes a row. Returns whether there was one.
    pub fn remove(&mut self, key: &str) -> Result<bool, JsError> {
        self.table.remove(key).map_err(js_error)
    }

    /// Sorts by columns: `"4:desc,0:asc"`, the first mattering most. Blank for no sorting.
    pub fn set_sort(&mut self, spec: &str) -> Result<(), JsError> {
        let mut sort = Vec::new();
        for part in spec.split(',').filter(|p| !p.trim().is_empty()) {
            let (col, direction) = part.trim().split_once(':').unwrap_or((part.trim(), "asc"));
            let col = col
                .parse::<usize>()
                .map_err(|_| js_error(format!("{col:?} is not a column's position")))?;
            sort.push((col, direction == "desc"));
        }
        self.table.set_sort(&sort).map_err(js_error)
    }

    /// Sets a column's filter: `>100`, `10..20`, `eur`, `=EUR`, `true`. Blank clears it.
    pub fn set_filter(&mut self, col: u32, text: &str) -> Result<(), JsError> {
        self.table.set_filter(col as usize, text).map_err(js_error)
    }

    /// Shows only rows whose key or text contains this.
    pub fn set_quick_filter(&mut self, text: &str) {
        self.table.set_quick_filter(text);
    }

    /// Groups rows by a text field; a negative column for no grouping.
    pub fn set_group(&mut self, col: i32) -> Result<(), JsError> {
        self.table
            .set_group(usize::try_from(col).ok())
            .map_err(js_error)
    }

    pub fn toggle_group(&mut self, key: &str) {
        self.table.toggle_group(key);
    }

    pub fn expand_all(&mut self, open: bool) {
        self.table.expand_all(open);
    }

    /// Applies every change since the last update. Returns
    /// `{epoch, inputsChanged, recalculated, changed, nodes, rebuilt}`.
    pub fn update(&mut self) -> String {
        let Update { report, rebuilt } = self.table.update();
        format!(
            r#"{{"epoch":{},"inputsChanged":{},"recalculated":{},"changed":{},"nodes":{},"rebuilt":{rebuilt}}}"#,
            report.epoch,
            report.inputs_changed,
            report.recalculated,
            report.changed,
            self.table.nodes()
        )
    }

    /// How many lines the view has (rows, plus one per group).
    pub fn view_len(&self) -> u32 {
        self.table.view_len() as u32
    }

    pub fn row_count(&self) -> u32 {
        self.table.row_count() as u32
    }

    pub fn filtered_count(&self) -> u32 {
        self.table.filtered_count() as u32
    }

    pub fn group_count(&self) -> u32 {
        self.table.group_count() as u32
    }

    /// The columns, as JSON.
    pub fn columns(&self) -> String {
        self.table.columns_json()
    }

    /// `count` lines of the view from line `start`, as JSON.
    pub fn rows(&self, start: u32, count: u32) -> String {
        self.table.rows_json(start as usize, count as usize)
    }

    /// Every group and its totals, in the view's order, as JSON.
    pub fn groups(&self) -> String {
        self.table.groups_json()
    }

    /// The line of the view a group's row is on, or -1.
    pub fn group_line(&self, key: &str) -> i32 {
        self.table.group_line(key).map_or(-1, |line| line as i32)
    }

    pub fn group_is_open(&self, key: &str) -> bool {
        self.table.group_is_open(key)
    }

    /// A text column's distinct values and their row counts, as JSON.
    pub fn values(&self, col: u32, limit: u32) -> String {
        self.table.values_json(col as usize, limit as usize)
    }

    /// Shows the grid as of an earlier update; a negative number for now.
    pub fn set_as_of(&mut self, epoch: f64) -> Result<(), JsError> {
        let epoch = (epoch >= 0.0).then_some(epoch as u64);
        self.table.set_as_of(epoch).map_err(js_error)
    }

    /// The earliest and latest updates that can be shown: `[oldest, latest]`, or `null`.
    pub fn history_range(&self) -> String {
        match self.table.history_range() {
            Some((oldest, latest)) => format!("[{oldest},{latest}]"),
            None => "null".into(),
        }
    }

    /// Keeps values from the last `updates` updates, for `trends`.
    pub fn keep_history(&mut self, updates: u32) {
        self.table.keep_history(u64::from(updates));
    }

    /// How one column moved over recent updates, for lines of the view, as JSON.
    pub fn trends(&self, start: u32, count: u32, col: u32, points: u32) -> String {
        self.table.trends_json(
            start as usize,
            count as usize,
            col as usize,
            points as usize,
        )
    }

    /// How a column's total moved over recent updates, as JSON.
    pub fn total_trend(&self, col: u32, points: u32) -> String {
        self.table.total_trend_json(col as usize, points as usize)
    }

    /// The bottom row's totals, as JSON.
    pub fn totals(&self) -> String {
        self.table.totals_json()
    }

    /// Why a cell has its value.
    pub fn explain(&self, key: &str, col: u32) -> Result<String, JsError> {
        self.table.explain(key, col as usize).map_err(js_error)
    }

    /// Why a group's total has its value.
    pub fn explain_group(&self, key: &str, col: u32) -> Result<String, JsError> {
        self.table
            .explain_group(key, col as usize)
            .map_err(js_error)
    }

    /// Why a column's total has its value.
    pub fn explain_total(&self, col: u32) -> Result<String, JsError> {
        self.table.explain_total(col as usize).map_err(js_error)
    }

    /// The rows in view as CSV.
    pub fn csv(&self) -> String {
        self.table.csv()
    }

    /// Checks the incremental values against a full recalculation.
    pub fn check(&self) -> Option<String> {
        self.table.check().err()
    }
}
