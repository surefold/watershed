//! The grid's model: rows in a watershed collection, and the view of them a
//! page shows (filtered, sorted, grouped). No JavaScript here, so it can be
//! tested like any other Rust.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use watershed_core::{AggregateOp, Collection, Graph, Ir, NodeId, Report, Type, Value};

use crate::formula;

/// What a column holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Number,
    Text,
    Bool,
}

impl Kind {
    pub fn parse(text: &str) -> Result<Kind, String> {
        match text {
            "number" => Ok(Kind::Number),
            "text" => Ok(Kind::Text),
            "bool" => Ok(Kind::Bool),
            other => Err(format!(
                "{other:?} is not a kind of column (there are number, text and bool)"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Number => "number",
            Kind::Text => "text",
            Kind::Bool => "bool",
        }
    }

    fn value_type(self) -> Type {
        match self {
            Kind::Number => Type::F64,
            Kind::Text => Type::Str,
            Kind::Bool => Type::Bool,
        }
    }

    /// A value of this kind from what someone typed.
    pub fn read(self, text: &str) -> Result<Value, String> {
        let text = text.trim();
        match self {
            Kind::Text => Ok(Value::Str(text.into())),
            Kind::Number => text
                .replace([',', '_'], "")
                .parse::<f64>()
                .ok()
                .filter(|x| x.is_finite())
                .map(Value::F64)
                .ok_or_else(|| format!("{text:?} is not a number")),
            Kind::Bool => match text.to_ascii_lowercase().as_str() {
                "true" | "yes" | "y" | "1" => Ok(Value::Bool(true)),
                "false" | "no" | "n" | "0" => Ok(Value::Bool(false)),
                _ => Err(format!("{text:?} is not true or false")),
            },
        }
    }
}

/// A number column's total, in the bottom row and in each group's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    None,
    Sum,
    Min,
    Max,
    /// The mean. The graph keeps the sum; the grid divides by the rows.
    Avg,
    /// How many rows. Any column can have this one.
    Count,
}

impl Agg {
    pub fn parse(text: &str) -> Result<Agg, String> {
        match text {
            "" | "none" => Ok(Agg::None),
            "sum" => Ok(Agg::Sum),
            "min" => Ok(Agg::Min),
            "max" => Ok(Agg::Max),
            "avg" => Ok(Agg::Avg),
            "count" => Ok(Agg::Count),
            other => Err(format!(
                "{other:?} is not a total (there are sum, avg, min, max, count and none)"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Agg::None => "none",
            Agg::Sum => "sum",
            Agg::Min => "min",
            Agg::Max => "max",
            Agg::Avg => "avg",
            Agg::Count => "count",
        }
    }

    fn op(self) -> Option<AggregateOp> {
        match self {
            Agg::None | Agg::Count => None,
            Agg::Sum | Agg::Avg => Some(AggregateOp::Sum),
            Agg::Min => Some(AggregateOp::Min),
            Agg::Max => Some(AggregateOp::Max),
        }
    }

    /// The total of these values, worked out the plain way. Used while a
    /// filter is on, when the graph's totals (of every row) are not the ones
    /// to show.
    fn fold<'a>(self, values: impl Iterator<Item = &'a Value>) -> f64 {
        if self == Agg::Count {
            return values.count() as f64;
        }
        let numbers = values.filter_map(|v| match v {
            Value::F64(x) => Some(*x),
            _ => None,
        });
        match self {
            Agg::None | Agg::Count => f64::NAN,
            Agg::Sum => numbers.sum(),
            Agg::Min => numbers.fold(f64::INFINITY, f64::min),
            Agg::Max => numbers.fold(f64::NEG_INFINITY, f64::max),
            Agg::Avg => {
                let (sum, n) = numbers.fold((0.0, 0usize), |(sum, n), x| (sum + x, n + 1));
                sum / n as f64
            }
        }
    }

    /// What to show for a total the graph keeps, over `rows` rows: the
    /// graph's number, or for an average that sum over the rows.
    fn of_graph(self, number: Option<f64>, rows: usize) -> f64 {
        match self {
            Agg::None => f64::NAN,
            Agg::Count => rows as f64,
            Agg::Avg => number.unwrap_or(f64::NAN) / rows as f64,
            Agg::Sum | Agg::Min | Agg::Max => number.unwrap_or(f64::NAN),
        }
    }
}

/// A column's filter, from what someone typed in its filter box.
#[derive(Debug, Clone, PartialEq)]
enum Filter {
    /// Text containing this (already lower case).
    Contains(String),
    /// Text equal to this, ignoring case (already lower case).
    Exactly(Vec<String>),
    Compare(Ordering, bool, f64),
    NotEqual(f64),
    Between(f64, f64),
    Is(bool),
}

impl Filter {
    /// `>100`, `<= 5`, `10..20`, `!=0` or `42` for numbers; text to look for,
    /// or `=text` for an exact match; `true` or `false`. Blank is no filter.
    fn parse(kind: Kind, text: &str) -> Result<Option<Filter>, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let number = |s: &str| match Kind::Number.read(s)? {
            Value::F64(x) => Ok::<f64, String>(x),
            _ => unreachable!("a number reads as a number"),
        };
        Ok(Some(match kind {
            Kind::Text => match text.strip_prefix('=') {
                // `=a|b` is any of several values.
                Some(exact) => {
                    Filter::Exactly(exact.split('|').map(|v| v.trim().to_lowercase()).collect())
                }
                None => Filter::Contains(text.to_lowercase()),
            },
            Kind::Bool => match Kind::Bool.read(text)? {
                Value::Bool(b) => Filter::Is(b),
                _ => unreachable!("a bool reads as a bool"),
            },
            Kind::Number => {
                if let Some((low, high)) = text.split_once("..") {
                    Filter::Between(number(low)?, number(high)?)
                } else if let Some(rest) = text.strip_prefix(">=") {
                    Filter::Compare(Ordering::Greater, true, number(rest)?)
                } else if let Some(rest) = text.strip_prefix("<=") {
                    Filter::Compare(Ordering::Less, true, number(rest)?)
                } else if let Some(rest) = text.strip_prefix("!=") {
                    Filter::NotEqual(number(rest)?)
                } else if let Some(rest) = text.strip_prefix('>') {
                    Filter::Compare(Ordering::Greater, false, number(rest)?)
                } else if let Some(rest) = text.strip_prefix('<') {
                    Filter::Compare(Ordering::Less, false, number(rest)?)
                } else {
                    let x = number(text.strip_prefix('=').unwrap_or(text))?;
                    Filter::Between(x, x)
                }
            }
        }))
    }

    fn accepts(&self, value: &Value) -> bool {
        match (self, value) {
            (Filter::Contains(needle), Value::Str(s)) => contains_lower(s, needle),
            (Filter::Exactly(wanted), Value::Str(s)) => wanted.iter().any(|want| {
                if want.is_ascii() {
                    s.eq_ignore_ascii_case(want)
                } else {
                    s.to_lowercase() == *want
                }
            }),
            (Filter::Compare(side, or_equal, limit), Value::F64(x)) => match x.partial_cmp(limit) {
                Some(Ordering::Equal) => *or_equal,
                Some(found) => found == *side,
                None => false,
            },
            (Filter::NotEqual(limit), Value::F64(x)) => x != limit,
            (Filter::Between(low, high), Value::F64(x)) => low <= x && x <= high,
            (Filter::Is(want), Value::Bool(b)) => b == want,
            _ => false,
        }
    }
}

/// Whether `text` contains `needle`, ignoring case. `needle` is lower case.
fn contains_lower(text: &str, needle: &str) -> bool {
    if needle.is_ascii() {
        let (text, needle) = (text.as_bytes(), needle.as_bytes());
        needle.is_empty()
            || text
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle))
    } else {
        text.to_lowercase().contains(needle)
    }
}

/// The order of two cells in a sorted column. Errors come last.
fn compare(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::F64(a), Value::F64(b)) => a.total_cmp(b),
        (Value::I64(a), Value::I64(b)) => a.cmp(b),
        (Value::Str(a), Value::Str(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        _ => a.is_error().cmp(&b.is_error()),
    }
}

struct Col {
    name: String,
    kind: Kind,
    /// The column in formulas and in calls to the collection.
    part: Ir,
    /// The formula as typed; `None` for a field.
    formula: Option<String>,
    agg: Agg,
    /// The node holding this column's total over every row.
    total: Option<NodeId>,
    filter: Option<Filter>,
    filter_text: String,
}

/// One group's row in the view.
struct Group {
    key: String,
    /// Where its rows are in `sorted`.
    start: usize,
    len: usize,
    /// Each column's total over the group's filtered rows, when a filter is
    /// on. Without one, the totals are read from the graph.
    plain: Vec<f64>,
}

/// One entry of the view with this bit set is a group; otherwise a row.
const GROUP: u32 = 1 << 31;

/// What an update did.
#[derive(Debug, Clone, Copy)]
pub struct Update {
    pub report: Report,
    /// Whether the view (which rows show, in what order) was worked out again.
    pub rebuilt: bool,
}

/// A table of keyed rows with calculated columns and totals, and a filtered,
/// sorted, grouped view of it.
///
/// Changes (rows, cells, sorting, filters, grouping) are recorded as they are
/// made and take effect at the next [`update`](Table::update).
pub struct Table {
    g: Graph,
    c: Collection,
    cols: Vec<Col>,
    /// Named values every row's formulas can use.
    values: Vec<(String, NodeId)>,

    /// Rows in the order they arrived.
    keys: Vec<String>,
    index: HashMap<String, u32>,
    /// Each row's node for each column: `nodes[row * cols + col]`.
    nodes: Vec<NodeId>,
    /// Rows removed since the last update, still in `keys`.
    dead: HashSet<u32>,

    sort: Vec<(usize, bool)>,
    quick: String,
    group_by: Option<usize>,
    /// For each column that has been grouped by, each column's grouping in the collection.
    groupings: HashMap<usize, Vec<Option<usize>>>,
    open: HashSet<String>,

    /// The rows that pass the filters, sorted, and with each group's rows together.
    sorted: Vec<u32>,
    groups: Vec<Group>,
    /// What the page shows, top to bottom: rows, and group rows with `GROUP` set.
    view: Vec<u32>,
    filter_on: bool,
    /// Each column's total over the filtered rows, when a filter is on.
    plain_totals: Vec<f64>,

    /// Rows, columns, sorting, filters or grouping changed since the last update.
    structural: bool,
    /// Columns with cells set since the last update.
    touched: Vec<bool>,
    /// A named value changed since the last update.
    value_touched: bool,
    /// Something was set, added or removed since the last update.
    pending: bool,

    /// The update whose values the grid shows, when looking back.
    as_of: Option<u64>,
    /// What a cell shows when looking back to before it existed.
    missing: Value,
}

impl Default for Table {
    fn default() -> Self {
        Table::new()
    }
}

fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
        && !matches!(name, "true" | "false" | "if" | "min" | "max" | "abs")
}

impl Table {
    pub fn new() -> Table {
        let g = Graph::new();
        let c = Collection::new(&g, "row");
        Table {
            g,
            c,
            cols: Vec::new(),
            values: Vec::new(),
            keys: Vec::new(),
            index: HashMap::new(),
            nodes: Vec::new(),
            dead: HashSet::new(),
            sort: Vec::new(),
            quick: String::new(),
            group_by: None,
            groupings: HashMap::new(),
            open: HashSet::new(),
            sorted: Vec::new(),
            groups: Vec::new(),
            view: Vec::new(),
            filter_on: false,
            plain_totals: Vec::new(),
            structural: false,
            touched: Vec::new(),
            value_touched: false,
            pending: false,
            as_of: None,
            missing: Value::error("not there yet"),
        }
    }

    // ------------------------------------------------------------ columns

    fn check_name(&self, name: &str) -> Result<(), String> {
        self.check_now()?;
        if !is_name(name) {
            return Err(format!(
                "{name:?} can't be a name: use letters, digits and _, starting with a letter"
            ));
        }
        if self.cols.iter().any(|c| c.name == name) || self.values.iter().any(|v| v.0 == name) {
            return Err(format!(
                "there is already a column or value called {name:?}"
            ));
        }
        Ok(())
    }

    /// Adds a column that holds what rows are given. Fields come before the first row.
    pub fn add_field(&mut self, name: &str, kind: Kind, agg: Agg) -> Result<usize, String> {
        self.check_name(name)?;
        if !matches!(agg, Agg::None | Agg::Count) && kind != Kind::Number {
            return Err(format!(
                "{name}: only number columns have totals (any column can have a count)"
            ));
        }
        let part = self
            .c
            .add_field(name, kind.value_type())
            .map_err(|e| e.to_string())?;
        self.push_col(name, kind, part, None, agg)
    }

    /// Adds a column worked out in every row by a formula over other columns
    /// and named values, such as `qty * (mark - price)`.
    pub fn add_formula(&mut self, name: &str, source: &str, agg: Agg) -> Result<usize, String> {
        self.check_name(name)?;
        let ir = formula::parse(source, &|n| {
            self.cols
                .iter()
                .find(|c| c.name == n)
                .map(|c| c.part.clone())
                .or_else(|| {
                    self.values
                        .iter()
                        .find(|v| v.0 == n)
                        .map(|v| self.g.reference(v.1))
                })
        })?;
        let (part, ty) = self
            .c
            .add_column(&mut self.g, name, ir)
            .map_err(|e| e.to_string())?;
        let kind = match ty {
            Type::F64 => Kind::Number,
            Type::Str => Kind::Text,
            Type::Bool => Kind::Bool,
            other => return Err(format!("{name}: a column can't hold a {other}")),
        };
        let agg = if kind == Kind::Number || agg == Agg::Count {
            agg
        } else {
            Agg::None
        };
        self.push_col(name, kind, part, Some(source.trim().to_string()), agg)
    }

    fn push_col(
        &mut self,
        name: &str,
        kind: Kind,
        part: Ir,
        formula: Option<String>,
        agg: Agg,
    ) -> Result<usize, String> {
        let total = match agg.op() {
            None => None,
            Some(op) => Some(
                self.c
                    .add_total(&mut self.g, &format!("{op}({name})"), op, Some(&part))
                    .map_err(|e| e.to_string())?,
            ),
        };
        self.cols.push(Col {
            name: name.to_string(),
            kind,
            part,
            formula,
            agg,
            total,
            filter: None,
            filter_text: String::new(),
        });
        self.touched.push(false);
        // Every row gains a node for the new column.
        self.compact();
        let n = self.cols.len();
        let mut nodes = Vec::with_capacity(self.keys.len() * n);
        for (r, key) in self.keys.iter().enumerate() {
            nodes.extend_from_slice(&self.nodes[r * (n - 1)..(r + 1) * (n - 1)]);
            nodes.push(
                self.c
                    .node_id(key, &self.cols[n - 1].part)
                    .expect("a row has a node for every column"),
            );
        }
        self.nodes = nodes;
        for per_column in self.groupings.values_mut() {
            per_column.push(None);
        }
        if let Some(by) = self.group_by {
            self.ensure_groupings(by)?;
        }
        self.structural = true;
        self.pending = true;
        Ok(n - 1)
    }

    /// Adds a named number that formulas can use, such as a fee or a rate.
    pub fn add_value(&mut self, name: &str, value: f64) -> Result<(), String> {
        self.check_name(name)?;
        self.pending = true;
        let id = self
            .g
            .input_value(name, Value::F64(value))
            .map_err(|e| e.to_string())?;
        self.values.push((name.to_string(), id));
        Ok(())
    }

    /// Changes a named number. Every cell that uses it is recalculated at the next update.
    pub fn set_value(&mut self, name: &str, value: f64) -> Result<(), String> {
        self.check_now()?;
        self.pending = true;
        let id = self
            .values
            .iter()
            .find(|v| v.0 == name)
            .ok_or_else(|| format!("there is no value called {name:?}"))?
            .1;
        self.g
            .set_value(id, Value::F64(value))
            .map_err(|e| e.to_string())?;
        self.value_touched = true;
        Ok(())
    }

    pub fn columns(&self) -> usize {
        self.cols.len()
    }

    pub fn column_named(&self, name: &str) -> Option<usize> {
        self.cols.iter().position(|c| c.name == name)
    }

    fn col(&self, col: usize) -> Result<&Col, String> {
        self.cols
            .get(col)
            .ok_or_else(|| format!("there is no column {col}"))
    }

    // --------------------------------------------------------------- rows

    /// Adds a row, or replaces every field of an existing one. `values` has
    /// one value per field, in the order the fields were added.
    pub fn upsert(&mut self, key: &str, values: Vec<Value>) -> Result<(), String> {
        self.check_now()?;
        self.pending = true;
        let fields: Vec<&str> = self
            .cols
            .iter()
            .filter(|c| c.formula.is_none())
            .map(|c| c.name.as_str())
            .collect();
        if values.len() != fields.len() {
            return Err(format!(
                "a row needs {} values, one per field, and this has {}",
                fields.len(),
                values.len()
            ));
        }
        let row: Vec<(&str, Value)> = fields.into_iter().zip(values).collect();
        self.c
            .upsert_named(&mut self.g, key, &row)
            .map_err(|e| e.to_string())?;
        if self.index.contains_key(key) {
            for (touched, col) in self.touched.iter_mut().zip(&self.cols) {
                *touched |= col.formula.is_none();
            }
            return Ok(());
        }
        self.index.insert(key.to_string(), self.keys.len() as u32);
        self.keys.push(key.to_string());
        for col in &self.cols {
            self.nodes.push(
                self.c
                    .node_id(key, &col.part)
                    .expect("a row has a node for every column"),
            );
        }
        self.structural = true;
        Ok(())
    }

    /// Sets one cell from what someone typed. Only fields can be set.
    pub fn set_cell(&mut self, key: &str, col: usize, text: &str) -> Result<(), String> {
        let value = self.col(col)?.kind.read(text)?;
        let row = *self
            .index
            .get(key)
            .ok_or_else(|| format!("there is no row {key:?}"))?;
        self.set_at(row as usize, col, value)
    }

    /// Sets one cell of the row at `row` (its position in arrival order).
    pub fn set_at(&mut self, row: usize, col: usize, value: Value) -> Result<(), String> {
        self.check_now()?;
        self.col(col)?;
        self.pending = true;
        let c = &self.cols[col];
        if c.formula.is_some() {
            return Err(format!(
                "{} is calculated ({}), so it can't be set",
                c.name,
                c.formula.as_deref().unwrap_or_default()
            ));
        }
        if row >= self.keys.len() || self.dead.contains(&(row as u32)) {
            return Err(format!("there is no row {row}"));
        }
        if self.groupings.contains_key(&col) {
            // The collection moves the row between this column's groups.
            self.c
                .upsert_named(&mut self.g, &self.keys[row], &[(c.name.as_str(), value)])
                .map_err(|e| e.to_string())?;
        } else {
            if value.type_of() != Some(c.kind.value_type()) {
                return Err(format!("{} holds a {}", c.name, c.kind.name()));
            }
            self.g
                .set_value(self.nodes[row * self.cols.len() + col], value)
                .map_err(|e| e.to_string())?;
        }
        self.touched[col] = true;
        Ok(())
    }

    /// Removes a row. Returns whether there was one.
    pub fn remove(&mut self, key: &str) -> Result<bool, String> {
        self.check_now()?;
        self.pending = true;
        let Some(row) = self.index.remove(key) else {
            return Ok(false);
        };
        self.c.remove(&mut self.g, key).map_err(|e| e.to_string())?;
        self.dead.insert(row);
        self.structural = true;
        Ok(true)
    }

    /// Drops the rows removed since the last update.
    fn compact(&mut self) {
        if self.dead.is_empty() {
            return;
        }
        let n = self.cols.len();
        let mut kept = 0;
        for r in 0..self.keys.len() {
            if self.dead.contains(&(r as u32)) {
                continue;
            }
            if kept != r {
                self.keys.swap(kept, r);
                self.nodes.copy_within(r * n..(r + 1) * n, kept * n);
            }
            kept += 1;
        }
        self.keys.truncate(kept);
        self.nodes.truncate(kept * n);
        self.dead.clear();
        for (r, key) in self.keys.iter().enumerate() {
            *self.index.get_mut(key).expect("every row is indexed") = r as u32;
        }
    }

    /// How many rows there are, before filtering.
    pub fn row_count(&self) -> usize {
        self.keys.len() - self.dead.len()
    }

    /// How many row positions there are for [`set_at`](Table::set_at):
    /// the rows, and any removed since the last update.
    pub fn row_slots(&self) -> usize {
        self.keys.len()
    }

    /// Each field's kind, in the order rows give their values.
    pub fn field_kinds(&self) -> Vec<Kind> {
        self.cols
            .iter()
            .filter(|c| c.formula.is_none())
            .map(|c| c.kind)
            .collect()
    }

    /// The cell at a row position, as of the last update; `None` if the row was removed.
    pub fn value_at(&self, row: usize, col: usize) -> Option<&Value> {
        (row < self.keys.len() && col < self.cols.len() && !self.dead.contains(&(row as u32)))
            .then(|| self.g.value(self.nodes[row * self.cols.len() + col]))
    }

    /// A cell's value as of the last update.
    pub fn value(&self, key: &str, col: usize) -> Option<&Value> {
        let row = *self.index.get(key)? as usize;
        (col < self.cols.len()).then(|| self.g.value(self.nodes[row * self.cols.len() + col]))
    }

    // --------------------------------------------------------------- view

    /// Sorts by these columns, the first mattering most; `true` is descending.
    pub fn set_sort(&mut self, sort: &[(usize, bool)]) -> Result<(), String> {
        for &(col, _) in sort {
            self.col(col)?;
        }
        self.sort = sort.to_vec();
        self.structural = true;
        Ok(())
    }

    /// Sets a column's filter from text: `>100`, `10..20`, `eur`, `=EUR`, `true`.
    /// Blank clears it.
    pub fn set_filter(&mut self, col: usize, text: &str) -> Result<(), String> {
        let filter = Filter::parse(self.col(col)?.kind, text)?;
        self.cols[col].filter = filter;
        self.cols[col].filter_text = text.trim().to_string();
        self.structural = true;
        Ok(())
    }

    /// Shows only rows whose key or any text column contains this.
    pub fn set_quick_filter(&mut self, text: &str) {
        self.quick = text.trim().to_lowercase();
        self.structural = true;
    }

    /// Groups rows by a text field, or by nothing.
    pub fn set_group(&mut self, col: Option<usize>) -> Result<(), String> {
        if let Some(col) = col {
            let c = self.col(col)?;
            if c.kind != Kind::Text || c.formula.is_some() {
                return Err(format!("{}: rows are grouped by a text field", c.name));
            }
            if !self.groupings.contains_key(&col) {
                // The groups' totals are made now, so they have no past to show.
                self.check_now()?;
            }
            self.ensure_groupings(col)?;
        }
        self.group_by = col;
        self.structural = true;
        Ok(())
    }

    /// Makes sure each column with a total has that total per group of `by`.
    fn ensure_groupings(&mut self, by: usize) -> Result<(), String> {
        self.pending = true;
        let mut per_column = self
            .groupings
            .remove(&by)
            .unwrap_or_else(|| vec![None; self.cols.len()]);
        let mut result = Ok(());
        for (k, col) in self.cols.iter().enumerate() {
            let (None, Some(op)) = (per_column[k], col.agg.op()) else {
                continue;
            };
            let name = format!("{op}({}) by {}", col.name, self.cols[by].name);
            match self
                .c
                .add_grouping(&mut self.g, &name, op, &self.cols[by].part, Some(&col.part))
            {
                Ok(index) => per_column[k] = Some(index),
                Err(e) => {
                    result = Err(e.to_string());
                    break;
                }
            }
        }
        self.groupings.insert(by, per_column);
        result
    }

    /// Opens a closed group, or closes an open one.
    pub fn toggle_group(&mut self, key: &str) {
        if !self.open.remove(key) {
            self.open.insert(key.to_string());
        }
        self.layout();
    }

    /// Opens every group, or closes them all.
    pub fn expand_all(&mut self, open: bool) {
        self.open.clear();
        if open {
            self.open.extend(self.groups.iter().map(|g| g.key.clone()));
        }
        self.layout();
    }

    /// Applies everything recorded since the last update: recalculates what
    /// the changes reach, then works the view out again if they could have
    /// changed it.
    pub fn update(&mut self) -> Update {
        // With nothing to apply, the graph is left alone: an update would
        // only use up one of the updates history keeps.
        let report = if self.pending {
            self.g.stabilize()
        } else {
            Report {
                epoch: self.g.epoch(),
                inputs_changed: 0,
                recalculated: 0,
                changed: 0,
            }
        };
        self.pending = false;
        self.compact();
        let rebuilt = self.view_is_stale();
        if rebuilt {
            self.rebuild();
        }
        self.structural = false;
        self.value_touched = false;
        self.touched.fill(false);
        Update { report, rebuilt }
    }

    /// Whether the changes since the last update could change which rows
    /// show, their order, or totals that are worked out the plain way.
    fn view_is_stale(&self) -> bool {
        if self.structural {
            return true;
        }
        let any = self.value_touched || self.touched.iter().any(|&t| t);
        if !any {
            return false;
        }
        if self.filter_on {
            return true;
        }
        // A calculated column can change when any field or named value does.
        let reads = |col: usize| self.cols[col].formula.is_some() || self.touched[col];
        self.sort.iter().any(|&(col, _)| reads(col)) || self.group_by.is_some_and(reads)
    }

    fn cell(&self, row: u32, col: usize) -> &Value {
        self.read(self.nodes[row as usize * self.cols.len() + col])
    }

    /// A node's value: the latest, or when looking back, the one it had then.
    fn read(&self, id: NodeId) -> &Value {
        match self.as_of {
            None => self.g.value(id),
            Some(epoch) => self.g.value_at(id, epoch).unwrap_or(&self.missing),
        }
    }

    /// Changes are refused while looking back.
    fn check_now(&self) -> Result<(), String> {
        match self.as_of {
            None => Ok(()),
            Some(epoch) => Err(format!(
                "the grid is showing update {epoch}; go back to now to change it"
            )),
        }
    }

    /// Works out the view from scratch: filter, sort, group.
    fn rebuild(&mut self) {
        let n = self.cols.len();
        let filters: Vec<(usize, &Filter)> = self
            .cols
            .iter()
            .enumerate()
            .filter_map(|(k, c)| c.filter.as_ref().map(|f| (k, f)))
            .collect();
        let text_cols: Vec<usize> = (0..n)
            .filter(|&k| self.cols[k].kind == Kind::Text)
            .collect();
        let quick = self.quick.as_str();
        let filter_on = !filters.is_empty() || !quick.is_empty();

        // Looking back, only the rows that were there then.
        let there = |r: u32| match self.as_of {
            Some(epoch) if n > 0 => self.g.value_at(self.nodes[r as usize * n], epoch).is_ok(),
            _ => true,
        };
        let mut rows: Vec<u32> = (0..self.keys.len() as u32)
            .filter(|&r| {
                there(r)
                    && filters.iter().all(|&(c, f)| f.accepts(self.cell(r, c)))
                    && (quick.is_empty()
                        || contains_lower(&self.keys[r as usize], quick)
                        || text_cols.iter().any(|&c| {
                            matches!(self.cell(r, c), Value::Str(s) if contains_lower(s, quick))
                        }))
            })
            .collect();

        if let Some((&(first, first_desc), rest)) = self.sort.split_first() {
            let then = |a: u32, b: u32| {
                for &(col, desc) in rest {
                    let order = compare(self.cell(a, col), self.cell(b, col));
                    if order != Ordering::Equal {
                        return if desc { order.reverse() } else { order };
                    }
                }
                Ordering::Equal
            };
            let directed = |order: Ordering| if first_desc { order.reverse() } else { order };
            if self.cols[first].kind == Kind::Number {
                // Numbers are read out once, so sorting compares them side by side.
                let mut keyed: Vec<(f64, u32)> = rows
                    .iter()
                    .map(|&r| match self.cell(r, first) {
                        Value::F64(x) => (*x, r),
                        _ => (f64::NAN, r),
                    })
                    .collect();
                keyed.sort_by(|a, b| {
                    // Errors (not a number) come last either way.
                    let order = match (a.0.is_nan(), b.0.is_nan()) {
                        (false, false) => directed(a.0.total_cmp(&b.0)),
                        (x, y) => x.cmp(&y),
                    };
                    order.then_with(|| then(a.1, b.1))
                });
                rows = keyed.into_iter().map(|(_, r)| r).collect();
            } else {
                rows.sort_by(|&a, &b| {
                    directed(compare(self.cell(a, first), self.cell(b, first)))
                        .then_with(|| then(a, b))
                });
            }
        }

        let fold = |col: usize, rows: &[u32]| {
            self.cols[col]
                .agg
                .fold(rows.iter().map(|&r| self.cell(r, col)))
        };
        let plain_totals: Vec<f64> = if filter_on {
            (0..n).map(|col| fold(col, &rows)).collect()
        } else {
            Vec::new()
        };

        let mut groups: Vec<Group> = Vec::new();
        if let Some(by) = self.group_by {
            // Each group's rows, in sorted order, and the groups in the order first met.
            let mut slots: HashMap<&str, usize> = HashMap::new();
            let mut buckets: Vec<(&str, Vec<u32>)> = Vec::new();
            for &r in &rows {
                let key = match self.cell(r, by) {
                    Value::Str(s) => &**s,
                    _ => "",
                };
                let slot = *slots.entry(key).or_insert_with(|| {
                    buckets.push((key, Vec::new()));
                    buckets.len() - 1
                });
                buckets[slot].1.push(r);
            }
            // Groups go by the first sorted column's total if it has one, and by name otherwise.
            let by_total = self
                .sort
                .first()
                .filter(|&&(col, _)| col != by && self.cols[col].agg != Agg::None)
                .copied();
            let name_desc = matches!(self.sort.first(), Some(&(col, true)) if col == by);
            let mut order: Vec<(f64, usize)> = buckets
                .iter()
                .enumerate()
                .map(|(k, (key, rows))| {
                    let total = match by_total {
                        Some((col, _)) if filter_on => fold(col, rows),
                        Some((col, _)) => self.cols[col]
                            .agg
                            .of_graph(self.group_number(by, key, col), rows.len()),
                        None => 0.0,
                    };
                    (total, k)
                })
                .collect();
            order.sort_by(|a, b| {
                let by_name = buckets[a.1].0.cmp(buckets[b.1].0);
                match by_total {
                    Some((_, desc)) => {
                        let order = a.0.total_cmp(&b.0);
                        (if desc { order.reverse() } else { order }).then(by_name)
                    }
                    None if name_desc => by_name.reverse(),
                    None => by_name,
                }
            });
            rows = Vec::with_capacity(rows.len());
            for (_, k) in order {
                let (key, members) = &buckets[k];
                groups.push(Group {
                    key: key.to_string(),
                    start: rows.len(),
                    len: members.len(),
                    plain: if filter_on {
                        (0..n).map(|col| fold(col, members)).collect()
                    } else {
                        Vec::new()
                    },
                });
                rows.extend_from_slice(members);
            }
        }

        self.filter_on = filter_on;
        self.plain_totals = plain_totals;
        self.sorted = rows;
        self.groups = groups;
        self.layout();
    }

    /// Lays the view out: every row, or each group's row and, if it is open, its rows.
    fn layout(&mut self) {
        self.view.clear();
        if self.group_by.is_none() {
            self.view.extend_from_slice(&self.sorted);
            return;
        }
        for (k, group) in self.groups.iter().enumerate() {
            self.view.push(GROUP | k as u32);
            if self.open.contains(&group.key) {
                self.view
                    .extend_from_slice(&self.sorted[group.start..group.start + group.len]);
            }
        }
    }

    /// A group's total of a column over every row, from the graph.
    fn group_node(&self, by: usize, key: &str, col: usize) -> Option<NodeId> {
        let index = (*self.groupings.get(&by)?.get(col)?)?;
        self.c.group_node(index, &Value::Str(key.into()))
    }

    fn group_number(&self, by: usize, key: &str, col: usize) -> Option<f64> {
        match self.read(self.group_node(by, key, col)?) {
            Value::F64(x) => Some(*x),
            _ => None,
        }
    }

    /// How many lines the view has: rows, plus a line per group.
    pub fn view_len(&self) -> usize {
        self.view.len()
    }

    /// How many rows pass the filters.
    pub fn filtered_count(&self) -> usize {
        self.sorted.len()
    }

    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// The keys of the rows in view, top to bottom (group lines left out).
    pub fn view_keys(&self) -> Vec<&str> {
        self.view
            .iter()
            .filter(|&&e| e & GROUP == 0)
            .map(|&r| self.keys[r as usize].as_str())
            .collect()
    }

    /// The groups in view, top to bottom: key and how many rows.
    pub fn view_groups(&self) -> Vec<(&str, usize)> {
        self.groups
            .iter()
            .map(|g| (g.key.as_str(), g.len))
            .collect()
    }

    // ------------------------------------------------------------ reading

    /// The columns: `[{name, kind, formula, agg, editable, groupable, filter}]`.
    pub fn columns_json(&self) -> String {
        let mut out = String::from("[");
        for (k, c) in self.cols.iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            let _ = write!(
                out,
                r#"{{"name":{},"kind":"{}","formula":{},"agg":"{}","editable":{},"groupable":{},"filter":{}}}"#,
                json_str(&c.name),
                c.kind.name(),
                c.formula.as_deref().map_or("null".into(), json_str),
                c.agg.name(),
                c.formula.is_none(),
                c.formula.is_none() && c.kind == Kind::Text,
                json_str(&c.filter_text),
            );
        }
        out.push(']');
        out
    }

    /// `count` lines of the view from line `start`. A row is
    /// `{"k": key, "c": [cells]}`; a group is
    /// `{"g": key, "n": rows, "o": open, "c": [totals]}`. A cell is a number,
    /// text, true or false, null (nothing to show) or `{"e": "why"}` for an error.
    pub fn rows_json(&self, start: usize, count: usize) -> String {
        let n = self.cols.len();
        let end = start.saturating_add(count).min(self.view.len());
        let mut out = String::from("[");
        for (i, &entry) in self.view[start.min(end)..end].iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if entry & GROUP == 0 {
                let _ = write!(
                    out,
                    r#"{{"k":{},"c":["#,
                    json_str(&self.keys[entry as usize])
                );
                for col in 0..n {
                    if col > 0 {
                        out.push(',');
                    }
                    write_value(&mut out, self.cell(entry, col));
                }
            } else {
                let group = &self.groups[(entry & !GROUP) as usize];
                let _ = write!(
                    out,
                    r#"{{"g":{},"n":{},"o":{},"c":["#,
                    json_str(&group.key),
                    group.len,
                    self.open.contains(&group.key)
                );
                let by = self.group_by.expect("a group line means rows are grouped");
                for col in 0..n {
                    if col > 0 {
                        out.push(',');
                    }
                    write_number(&mut out, self.group_total(by, group, col));
                }
            }
            out.push_str("]}");
        }
        out.push(']');
        out
    }

    /// The bottom row: each column's total over the rows that pass the
    /// filters, or null if it has none.
    pub fn totals_json(&self) -> String {
        let mut out = String::from("[");
        for (k, col) in self.cols.iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            let total = if col.agg == Agg::None {
                f64::NAN
            } else if self.filter_on {
                self.plain_totals[k]
            } else {
                let number = col.total.and_then(|id| match self.read(id) {
                    Value::F64(x) => Some(*x),
                    _ => None,
                });
                col.agg.of_graph(number, self.sorted.len())
            };
            write_number(&mut out, total);
        }
        out.push(']');
        out
    }

    /// Why a cell has its value, as a tree of what it was worked out from.
    pub fn explain(&self, key: &str, col: usize) -> Result<String, String> {
        self.col(col)?;
        let row = *self
            .index
            .get(key)
            .ok_or_else(|| format!("there is no row {key:?}"))? as usize;
        Ok(self.g.explain_node(self.nodes[row * self.cols.len() + col]))
    }

    /// Why a group's total has its value.
    pub fn explain_group(&self, key: &str, col: usize) -> Result<String, String> {
        let c = self.col(col)?;
        let by = self.group_by.ok_or("rows are not grouped")?;
        if c.agg == Agg::Count {
            return Ok("The count is how many of the group's rows show.".into());
        }
        let id = self
            .group_node(by, key, col)
            .ok_or_else(|| format!("{} has no total", c.name))?;
        Ok(self.with_filter_note(self.with_average_note(c, self.g.explain_node(id))))
    }

    /// Why a column's total has its value.
    pub fn explain_total(&self, col: usize) -> Result<String, String> {
        let c = self.col(col)?;
        if c.agg == Agg::Count {
            return Ok(format!(
                "The count is how many rows show: {}.",
                self.sorted.len()
            ));
        }
        let id = c.total.ok_or_else(|| format!("{} has no total", c.name))?;
        Ok(self.with_filter_note(self.with_average_note(c, self.g.explain_node(id))))
    }

    fn with_average_note(&self, col: &Col, explanation: String) -> String {
        if col.agg == Agg::Avg {
            format!("The average is this sum divided by the number of rows.\n\n{explanation}")
        } else {
            explanation
        }
    }

    /// A group's total of a column, as its line shows it.
    fn group_total(&self, by: usize, group: &Group, col: usize) -> f64 {
        let agg = self.cols[col].agg;
        if agg == Agg::None {
            f64::NAN
        } else if self.filter_on {
            group.plain[col]
        } else {
            agg.of_graph(self.group_number(by, &group.key, col), group.len)
        }
    }

    /// Every group, in the view's order, open or not:
    /// `[{"g": key, "n": rows, "c": [totals]}]`. Empty when rows are not grouped.
    pub fn groups_json(&self) -> String {
        let mut out = String::from("[");
        if let Some(by) = self.group_by {
            for (k, group) in self.groups.iter().enumerate() {
                if k > 0 {
                    out.push(',');
                }
                let _ = write!(
                    out,
                    r#"{{"g":{},"n":{},"c":["#,
                    json_str(&group.key),
                    group.len
                );
                for col in 0..self.cols.len() {
                    if col > 0 {
                        out.push(',');
                    }
                    write_number(&mut out, self.group_total(by, group, col));
                }
                out.push_str("]}");
            }
        }
        out.push(']');
        out
    }

    /// The line of the view a group's row is on.
    pub fn group_line(&self, key: &str) -> Option<usize> {
        self.view.iter().position(|&entry| {
            entry & GROUP != 0 && self.groups[(entry & !GROUP) as usize].key == key
        })
    }

    /// Whether a group is open.
    pub fn group_is_open(&self, key: &str) -> bool {
        self.open.contains(key)
    }

    /// A text column's distinct values and how many rows have each, sorted:
    /// `{"more": false, "values": [["EU", 2], ["US", 3]]}`. With more than
    /// `limit` distinct values, `more` is true and none are listed.
    pub fn values_json(&self, col: usize, limit: usize) -> String {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        if col < self.cols.len() {
            for r in (0..self.keys.len() as u32).filter(|r| !self.dead.contains(r)) {
                if let Value::Str(s) = self.cell(r, col) {
                    *counts.entry(&**s).or_default() += 1;
                }
            }
        }
        if counts.len() > limit {
            return r#"{"more":true,"values":[]}"#.to_string();
        }
        let mut values: Vec<(&str, usize)> = counts.into_iter().collect();
        values.sort();
        let mut out = String::from(r#"{"more":false,"values":["#);
        for (k, (value, rows)) in values.into_iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            let _ = write!(out, "[{},{rows}]", json_str(value));
        }
        out.push_str("]}");
        out
    }

    // ------------------------------------------------------------- history

    /// Shows the grid as it was at the end of an earlier update: cells,
    /// totals, and which rows pass the filters and in what order. `None`
    /// goes back to now. Needs [`keep_history`](Table::keep_history); while
    /// looking back, nothing can be changed.
    pub fn set_as_of(&mut self, epoch: Option<u64>) -> Result<(), String> {
        if let Some(epoch) = epoch {
            let (oldest, latest) = self
                .g
                .history_range()
                .ok_or("the grid is not keeping earlier values")?;
            if self.pending {
                return Err("update the grid before looking back".into());
            }
            if epoch < oldest || epoch > latest {
                return Err(format!(
                    "update {epoch} is not kept: the grid has updates {oldest} to {latest}"
                ));
            }
        }
        self.as_of = epoch;
        self.structural = true;
        Ok(())
    }

    /// The update the grid is showing, when looking back.
    pub fn as_of(&self) -> Option<u64> {
        self.as_of
    }

    /// The earliest and latest updates that can be shown, if history is kept.
    pub fn history_range(&self) -> Option<(u64, u64)> {
        self.g.history_range()
    }

    /// Keeps each cell's and total's values from the last `updates` updates,
    /// for [`trends_json`](Table::trends_json). Each value an update
    /// overwrites costs 32 bytes until it is forgotten.
    pub fn keep_history(&mut self, updates: u64) {
        self.g.keep_history(updates);
    }

    /// A node's values over the updates history covers, oldest first, at up
    /// to `points` evenly spaced updates, each multiplied by `scale`. `None`
    /// if history is off or there are not yet two numbers to show.
    fn trend(&self, id: NodeId, points: usize, scale: f64) -> Option<Vec<f64>> {
        let (mut oldest, latest) = self.g.history_range()?;
        // A node made since history began has values only from the update after that.
        let mut known = latest;
        while oldest < known {
            let middle = oldest + (known - oldest) / 2;
            if self.g.value_at(id, middle).is_ok() {
                known = middle;
            } else {
                oldest = middle + 1;
            }
        }
        let span = latest.checked_sub(oldest).filter(|&s| s > 0)?;
        let n = (points.max(2) as u64).min(span + 1);
        let numbers: Vec<f64> = (0..n)
            .filter_map(|i| match self.g.value_at(id, oldest + span * i / (n - 1)) {
                Ok(Value::F64(x)) if x.is_finite() => Some(x * scale),
                _ => None,
            })
            .collect();
        (numbers.len() >= 2).then_some(numbers)
    }

    /// How one column moved over recent updates, for `count` lines of the
    /// view from line `start`: for each line, its numbers oldest first, or
    /// null. Needs [`keep_history`](Table::keep_history). A group's line
    /// gives its total's history, unless a filter is on (those totals are
    /// worked out the plain way and are not kept).
    pub fn trends_json(&self, start: usize, count: usize, col: usize, points: usize) -> String {
        let end = start.saturating_add(count).min(self.view.len());
        let mut out = String::from("[");
        for (i, &entry) in self.view[start.min(end)..end].iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let trend = if col >= self.cols.len() {
                None
            } else if entry & GROUP == 0 {
                self.trend(
                    self.nodes[entry as usize * self.cols.len() + col],
                    points,
                    1.0,
                )
            } else {
                let group = &self.groups[(entry & !GROUP) as usize];
                let agg = self.cols[col].agg;
                self.group_by
                    .filter(|_| !self.filter_on)
                    .and_then(|by| self.group_node(by, &group.key, col))
                    .and_then(|id| self.trend(id, points, agg.of_graph(Some(1.0), group.len)))
            };
            write_numbers(&mut out, trend.as_deref());
        }
        out.push(']');
        out
    }

    /// How a column's total moved over recent updates: its numbers oldest
    /// first, or null (no total, history off, or a filter is on).
    pub fn total_trend_json(&self, col: usize, points: usize) -> String {
        let trend = self
            .cols
            .get(col)
            .filter(|_| !self.filter_on)
            .and_then(|c| Some((c.total?, c.agg)))
            .and_then(|(id, agg)| {
                self.trend(id, points, agg.of_graph(Some(1.0), self.sorted.len()))
            });
        let mut out = String::new();
        write_numbers(&mut out, trend.as_deref());
        out
    }

    fn with_filter_note(&self, explanation: String) -> String {
        if self.filter_on {
            format!(
                "A filter is on, so the grid shows the total of the rows that pass it.\n\
                 This is the total of every row:\n\n{explanation}"
            )
        } else {
            explanation
        }
    }

    /// The rows in view (filtered and sorted, every group open) as CSV.
    pub fn csv(&self) -> String {
        fn field(out: &mut String, text: &str) {
            if text.contains([',', '"', '\n', '\r']) {
                out.push('"');
                out.push_str(&text.replace('"', "\"\""));
                out.push('"');
            } else {
                out.push_str(text);
            }
        }
        let mut out = String::from("key");
        for col in &self.cols {
            out.push(',');
            field(&mut out, &col.name);
        }
        out.push('\n');
        for &r in &self.sorted {
            field(&mut out, &self.keys[r as usize]);
            for col in 0..self.cols.len() {
                out.push(',');
                match self.cell(r, col) {
                    Value::Str(s) => field(&mut out, s),
                    Value::Error(e) => field(&mut out, &format!("#ERROR {e}")),
                    other => field(&mut out, &other.to_string()),
                }
            }
            out.push('\n');
        }
        out
    }

    /// How many nodes the graph has.
    pub fn nodes(&self) -> usize {
        self.g.len()
    }

    /// Checks the incremental values against a full recalculation.
    pub fn check(&self) -> Result<(), String> {
        self.g.check_consistency()
    }
}

fn write_number(out: &mut String, x: f64) {
    if x.is_finite() {
        // `+ 0.0` turns -0 (an empty sum) into 0.
        let _ = write!(out, "{}", x + 0.0);
    } else {
        out.push_str("null");
    }
}

fn write_numbers(out: &mut String, numbers: Option<&[f64]>) {
    let Some(numbers) = numbers else {
        out.push_str("null");
        return;
    };
    out.push('[');
    for (k, &x) in numbers.iter().enumerate() {
        if k > 0 {
            out.push(',');
        }
        write_number(out, x);
    }
    out.push(']');
}

fn write_value(out: &mut String, value: &Value) {
    match value {
        Value::F64(x) => write_number(out, *x),
        Value::I64(x) => {
            let _ = write!(out, "{x}");
        }
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Str(s) => out.push_str(&json_str(s)),
        Value::List(items) => out.push_str(&json_str(&format!("{items:?}"))),
        Value::Error(e) => {
            let _ = write!(out, r#"{{"e":{}}}"#, json_str(e));
        }
    }
}

pub(crate) fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
