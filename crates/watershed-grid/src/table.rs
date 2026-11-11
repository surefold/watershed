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

    /// The total of these numbers, worked out the plain way. Used where
    /// the graph's totals are not the ones to show: while a filter is on,
    /// and for the groups of a table too big for the graph to keep them. A
    /// cell that holds no number (an error) comes as not-a-number, and makes
    /// a sum or an average not a number too.
    fn fold(self, numbers: impl ExactSizeIterator<Item = f64>) -> f64 {
        let rows = numbers.len();
        match self {
            Agg::None => f64::NAN,
            Agg::Count => rows as f64,
            Agg::Sum => numbers.sum(),
            Agg::Avg => numbers.sum::<f64>() / rows as f64,
            Agg::Min => numbers.fold(f64::INFINITY, f64::min),
            Agg::Max => numbers.fold(f64::NEG_INFINITY, f64::max),
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

/// Whether a cell has a place in a sorted column. An error or a number that
/// is not a number has none, and goes last whichever way the column is sorted.
fn sortable(value: &Value) -> bool {
    match value {
        Value::F64(x) => !x.is_nan(),
        Value::Error(_) => false,
        _ => true,
    }
}

/// The order of two cells in a column sorted ascending, or descending.
fn compare_directed(a: &Value, b: &Value, descending: bool) -> Ordering {
    match (sortable(a), sortable(b)) {
        (true, true) if descending => compare(b, a),
        (true, true) => compare(a, b),
        (a, b) => b.cmp(&a),
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
    /// The nodes holding this column's sum, minimum and maximum over every
    /// row, for those it has had as its total (see [`slot`]).
    totals: [Option<NodeId>; 3],
    filter: Option<Filter>,
    filter_text: String,
}

impl Col {
    /// The node behind this column's total, if the graph keeps one for it.
    fn total(&self) -> Option<NodeId> {
        self.totals[slot(self.agg.op()?)]
    }
}

/// Where a total's node is kept among a column's three.
fn slot(op: AggregateOp) -> usize {
    match op {
        AggregateOp::Sum | AggregateOp::Count => 0,
        AggregateOp::Min => 1,
        AggregateOp::Max => 2,
    }
}

/// A cell someone set, and what it held before.
struct Edit {
    key: String,
    col: usize,
    before: Value,
    after: Value,
}

/// The most steps that can be undone.
const MOST_UNDO: usize = 200;

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

/// The most row-and-column pairs the graph is asked to keep group totals for
/// when rows are first grouped by a column. A group total in the graph costs
/// a node for each row in each totalled column, for each column ever grouped
/// by, and the graph cannot give them back: 50,000 rows with six totals took
/// about 175 MB per grouping column. Past this, the grid adds a group's
/// totals up itself, over its rows, whenever they may have changed.
const GROUP_BUDGET: usize = 100_000;

/// The most rows noted as set between updates; past that the view is worked
/// out from scratch, which is quicker by then anyway.
const MOST_TRACKED: usize = 1 << 16;

/// A group's rows while the view is being worked out, in sorted order; or
/// every row, when rows are not grouped.
struct Bucket {
    key: String,
    rows: Vec<u32>,
    /// Each column's total over these rows, if it is already known: the
    /// bucket is a group that did not change since it was last added up.
    plain: Option<Vec<f64>>,
}

impl Bucket {
    fn new(key: &str, rows: Vec<u32>) -> Bucket {
        Bucket {
            key: key.to_string(),
            rows,
            plain: None,
        }
    }
}

/// One entry of the view with this bit set is a group; otherwise a row.
const GROUP: u32 = 1 << 31;

/// What an update did.
#[derive(Debug, Clone, Copy)]
pub struct Update {
    pub report: Report,
    /// Whether the view (which rows show, in what order) was worked out
    /// again from scratch.
    pub rebuilt: bool,
    /// Whether the view was brought up to date by moving only the rows that were set.
    pub patched: bool,
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
    groupings: HashMap<usize, Vec<[Option<usize>; 3]>>,
    /// For each column with a total worked out from its numbers, every
    /// row's number in that column, side by side (empty for other columns).
    /// The grid adds totals up from these rather than from the graph, whose
    /// values for one column are scattered among every other node's. Kept
    /// up to date only while the grid is adding totals up: see `copies_fresh`.
    copies: Vec<Vec<f64>>,
    /// Whether `copies` matches the graph for every row.
    copies_fresh: bool,
    /// See [`GROUP_BUDGET`].
    group_budget: usize,
    /// The groups in view have their totals added up by the grid, not kept by the graph.
    plain_groups: bool,
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
    /// What [`set_cell`](Table::set_cell) changed, newest last, a step at a
    /// time; and the steps undone, for doing again.
    undo: Vec<Vec<Edit>>,
    redo: Vec<Vec<Edit>>,
    /// The step being gathered between `begin_edits` and `end_edits`.
    batch: Option<Vec<Edit>>,
    /// The rows with cells set since the last update (a row may be noted twice).
    changed_rows: Vec<u32>,
    /// More rows were set than `changed_rows` holds.
    changed_unknown: bool,

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
            copies: Vec::new(),
            copies_fresh: false,
            group_budget: GROUP_BUDGET,
            plain_groups: false,
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
            undo: Vec::new(),
            redo: Vec::new(),
            batch: None,
            changed_rows: Vec::new(),
            changed_unknown: false,
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
        let mut totals = [None; 3];
        if let Some(op) = agg.op() {
            totals[slot(op)] = Some(
                self.c
                    .add_total(&mut self.g, &format!("{op}({name})"), op, Some(&part))
                    .map_err(|e| e.to_string())?,
            );
        }
        self.cols.push(Col {
            name: name.to_string(),
            kind,
            part,
            formula,
            agg,
            totals,
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
            per_column.push([None; 3]);
        }
        if let Some(by) = self.group_by.filter(|_| !self.plain_groups) {
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
        if let Some(&row) = self.index.get(key) {
            for (touched, col) in self.touched.iter_mut().zip(&self.cols) {
                *touched |= col.formula.is_none();
            }
            self.note_changed(row);
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
            .ok_or_else(|| format!("there is no row {key:?}"))? as usize;
        let before = self
            .g
            .value(self.nodes[row * self.cols.len() + col])
            .clone();
        self.set_at(row, col, value.clone())?;
        if before.same_as(&value) {
            return Ok(());
        }
        let edit = Edit {
            key: key.to_string(),
            col,
            before,
            after: value,
        };
        self.redo.clear();
        match &mut self.batch {
            Some(batch) => batch.push(edit),
            None => self.push_undo(vec![edit]),
        }
        Ok(())
    }

    fn push_undo(&mut self, step: Vec<Edit>) {
        if step.is_empty() {
            return;
        }
        if self.undo.len() == MOST_UNDO {
            self.undo.remove(0);
        }
        self.undo.push(step);
    }

    /// From here to [`end_edits`](Table::end_edits), every cell set with
    /// [`set_cell`](Table::set_cell) is one step to undo: a paste, say.
    pub fn begin_edits(&mut self) {
        self.end_edits();
        self.batch = Some(Vec::new());
    }

    pub fn end_edits(&mut self) {
        if let Some(step) = self.batch.take() {
            self.push_undo(step);
        }
    }

    /// Puts back what the last [`set_cell`](Table::set_cell) (or step of
    /// several) changed. Returns what was put back, in words, or `None` if
    /// there was nothing to undo. Cells of rows since removed are left out.
    pub fn undo(&mut self) -> Result<Option<String>, String> {
        self.step(true)
    }

    /// Does again what the last [`undo`](Table::undo) put back.
    pub fn redo(&mut self) -> Result<Option<String>, String> {
        self.step(false)
    }

    fn step(&mut self, back: bool) -> Result<Option<String>, String> {
        self.check_now()?;
        self.end_edits();
        let Some(step) = (if back {
            self.undo.pop()
        } else {
            self.redo.pop()
        }) else {
            return Ok(None);
        };
        let mut set = 0;
        // Backwards when undoing, so a cell set twice in a step ends at its first value.
        let order: Vec<usize> = if back {
            (0..step.len()).rev().collect()
        } else {
            (0..step.len()).collect()
        };
        for k in order {
            let edit = &step[k];
            let Some(&row) = self.index.get(&edit.key) else {
                continue;
            };
            let value = if back { &edit.before } else { &edit.after };
            if self.set_at(row as usize, edit.col, value.clone()).is_ok() {
                set += 1;
            }
        }
        let what = match step.as_slice() {
            [one] => format!("{} of {}", self.cols[one.col].name, one.key),
            many => format!("{set} of {} cells", many.len()),
        };
        if back {
            self.redo.push(step);
        } else {
            self.undo.push(step);
        }
        Ok(Some(what))
    }

    /// Changes a column's total: in the bottom row, and in each group's row.
    pub fn set_agg(&mut self, col: usize, agg: Agg) -> Result<(), String> {
        let c = self.col(col)?;
        if !matches!(agg, Agg::None | Agg::Count) && c.kind != Kind::Number {
            return Err(format!(
                "{}: only number columns have totals (any column can have a count)",
                c.name
            ));
        }
        if let Some(op) = agg.op().filter(|&op| c.totals[slot(op)].is_none()) {
            // The total's node is made now, so it has no past to show.
            self.check_now()?;
            let c = &self.cols[col];
            let id = self
                .c
                .add_total(&mut self.g, &format!("{op}({})", c.name), op, Some(&c.part))
                .map_err(|e| e.to_string())?;
            self.cols[col].totals[slot(op)] = Some(id);
            self.pending = true;
        }
        self.cols[col].agg = agg;
        if let Some(by) = self.group_by.filter(|_| !self.plain_groups) {
            if agg.op().is_some_and(|op| {
                self.groupings
                    .get(&by)
                    .is_none_or(|per_column| per_column[col][slot(op)].is_none())
            }) {
                self.check_now()?;
            }
            self.ensure_groupings(by)?;
        }
        self.structural = true;
        Ok(())
    }

    /// The rows on `count` lines of the view from line `start` (group lines
    /// are passed over).
    fn rows_on(&self, start: usize, count: usize) -> impl Iterator<Item = u32> + '_ {
        let end = start.saturating_add(count).min(self.view.len());
        self.view[start.min(end)..end]
            .iter()
            .copied()
            .filter(|&entry| entry & GROUP == 0)
    }

    /// A block of cells, as tab-separated text for the clipboard: these
    /// columns, over `count` lines of the view from line `start`.
    pub fn range_tsv(&self, start: usize, count: usize, cols: &[usize]) -> String {
        let mut out = String::new();
        for r in self.rows_on(start, count) {
            for (k, &col) in cols.iter().filter(|&&c| c < self.cols.len()).enumerate() {
                if k > 0 {
                    out.push('\t');
                }
                match self.cell(r, col) {
                    Value::Str(s) => out.push_str(&s.replace(['\t', '\n', '\r'], " ")),
                    Value::Error(_) => out.push_str("#ERROR"),
                    other => out.push_str(&other.to_string()),
                }
            }
            out.push('\n');
        }
        out
    }

    /// What a block of cells comes to: how many cells, how many of them hold
    /// numbers, and those numbers' sum, average, least and greatest:
    /// `{"cells": 12, "numbers": 8, "sum": 40, "avg": 5, "min": 1, "max": 9}`
    /// (the last four null when there are no numbers).
    pub fn range_stats_json(&self, start: usize, count: usize, cols: &[usize]) -> String {
        let (mut cells, mut numbers) = (0usize, 0usize);
        let (mut sum, mut least, mut greatest) = (0.0, f64::INFINITY, f64::NEG_INFINITY);
        for r in self.rows_on(start, count) {
            for &col in cols.iter().filter(|&&c| c < self.cols.len()) {
                cells += 1;
                if let Value::F64(x) = self.cell(r, col) {
                    numbers += 1;
                    sum += x;
                    least = least.min(*x);
                    greatest = greatest.max(*x);
                }
            }
        }
        let mut out = format!(r#"{{"cells":{cells},"numbers":{numbers}"#);
        let none = numbers == 0;
        for (name, x) in [
            ("sum", sum),
            ("avg", sum / numbers as f64),
            ("min", least),
            ("max", greatest),
        ] {
            let _ = write!(out, r#","{name}":"#);
            write_number(&mut out, if none { f64::NAN } else { x });
        }
        out.push('}');
        out
    }

    /// Sets every cell of a block to what was typed, as one step to undo.
    /// Returns how many were set and how many were passed over (calculated
    /// columns, and columns the text does not read as).
    pub fn fill(
        &mut self,
        start: usize,
        count: usize,
        cols: &[usize],
        text: &str,
    ) -> (usize, usize) {
        let keys: Vec<String> = self
            .rows_on(start, count)
            .map(|r| self.keys[r as usize].clone())
            .collect();
        let (mut set, mut skipped) = (0, 0);
        self.begin_edits();
        for key in &keys {
            for &col in cols {
                match self.set_cell(key, col, text) {
                    Ok(()) => set += 1,
                    Err(_) => skipped += 1,
                }
            }
        }
        self.end_edits();
        (set, skipped)
    }

    /// The rows with these keys, in the view's order, as tab-separated text
    /// for the clipboard: the key, then each of `cols`.
    pub fn tsv(&self, keys: &HashSet<&str>, cols: &[usize]) -> String {
        let clean = |text: &str| text.replace(['\t', '\n', '\r'], " ");
        let mut out = String::new();
        for &r in &self.sorted {
            let key = self.keys[r as usize].as_str();
            if !keys.contains(key) {
                continue;
            }
            out.push_str(&clean(key));
            for &col in cols.iter().filter(|&&col| col < self.cols.len()) {
                out.push('\t');
                match self.cell(r, col) {
                    Value::Str(s) => out.push_str(&clean(s)),
                    Value::Error(_) => out.push_str("#ERROR"),
                    other => out.push_str(&other.to_string()),
                }
            }
            out.push('\n');
        }
        out
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
        self.note_changed(row as u32);
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
            // The graph keeps the groups' totals if it already does for this
            // column, or if that is cheap enough to start.
            let totalled = self.cols.iter().filter(|c| c.agg.op().is_some()).count();
            self.plain_groups = !self.groupings.contains_key(&col)
                && self.row_count() * totalled > self.group_budget;
            if !self.plain_groups {
                if !self.groupings.contains_key(&col) {
                    // The groups' totals are made now, so they have no past to show.
                    self.check_now()?;
                }
                self.ensure_groupings(col)?;
            }
        } else {
            self.plain_groups = false;
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
            .unwrap_or_else(|| vec![[None; 3]; self.cols.len()]);
        let mut result = Ok(());
        for (k, col) in self.cols.iter().enumerate() {
            let Some(op) = col.agg.op().filter(|&op| per_column[k][slot(op)].is_none()) else {
                continue;
            };
            let name = format!("{op}({}) by {}", col.name, self.cols[by].name);
            match self
                .c
                .add_grouping(&mut self.g, &name, op, &self.cols[by].part, Some(&col.part))
            {
                Ok(index) => per_column[k][slot(op)] = Some(index),
                Err(e) => {
                    result = Err(e.to_string());
                    break;
                }
            }
        }
        self.groupings.insert(by, per_column);
        result
    }

    /// Sets how many row-and-column pairs the graph may be asked to keep
    /// group totals for; past it the grid adds them up itself. Takes effect
    /// the next time rows are grouped by a column not grouped by before.
    pub fn set_group_budget(&mut self, pairs: usize) {
        self.group_budget = pairs;
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
        let changed = self.pending;
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
        let stale = self.view_is_stale();
        let patched = stale && self.can_patch() && self.patch();
        let rebuilt = stale && !patched;
        if rebuilt {
            self.rebuild();
        }
        if changed && !patched && !rebuilt {
            // Cells moved and nothing was added up, so the copies are behind.
            self.copies_fresh = false;
        }
        self.structural = false;
        self.value_touched = false;
        self.touched.fill(false);
        self.changed_rows.clear();
        self.changed_unknown = false;
        Update {
            report,
            rebuilt,
            patched,
        }
    }

    /// Works the view out from scratch, whatever has or has not changed.
    /// The grid does this itself when it needs to; this is for checking that
    /// a view kept up to date row by row is the one a fresh start gives.
    pub fn rebuild_view(&mut self) {
        self.rebuild();
    }

    /// Notes that a row's cells were set, so the view can move just that row.
    fn note_changed(&mut self, row: u32) {
        if self.changed_rows.len() < MOST_TRACKED {
            self.changed_rows.push(row);
        } else {
            self.changed_unknown = true;
        }
    }

    /// Whether the view can be brought up to date by moving only the rows
    /// whose cells were set: nothing else about it changed, and they are few
    /// enough for that to be quicker than starting again.
    ///
    /// A row's cells depend only on its own fields and on named values, so
    /// with no named value changed, a row that was not set shows what it
    /// showed before, passes the filters as before, and sorts where it did.
    fn can_patch(&self) -> bool {
        !self.structural
            && !self.value_touched
            && !self.changed_unknown
            && self.as_of.is_none()
            && self.changed_rows.len() <= (self.keys.len() / 8).max(64)
            // A row set into another group may need a group that is not there yet.
            && !self.group_by.is_some_and(|by| self.touched[by])
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
        // Totals the grid adds up itself have to be added up again.
        if self.filter_on || self.plain_groups {
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

    /// Whether any filter is set.
    fn filtering(&self) -> bool {
        !self.quick.is_empty() || self.cols.iter().any(|c| c.filter.is_some())
    }

    /// A test of whether a row passes every filter.
    fn row_test(&self) -> impl Fn(u32) -> bool + '_ {
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
        move |r| {
            // Looking back, only the rows that were there then.
            let there = match self.as_of {
                Some(epoch) if n > 0 => self.g.value_at(self.nodes[r as usize * n], epoch).is_ok(),
                _ => true,
            };
            there
                && filters.iter().all(|&(c, f)| f.accepts(self.cell(r, c)))
                && (quick.is_empty()
                    || contains_lower(&self.keys[r as usize], quick)
                    || text_cols.iter().any(
                        |&c| matches!(self.cell(r, c), Value::Str(s) if contains_lower(s, quick)),
                    ))
        }
    }

    /// The order of two rows by the sorted columns from `from` on. Rows that
    /// tie everywhere keep the order they arrived in.
    fn order_from(&self, from: usize, a: u32, b: u32) -> Ordering {
        for &(col, desc) in &self.sort[from..] {
            let order = compare_directed(self.cell(a, col), self.cell(b, col), desc);
            if order != Ordering::Equal {
                return order;
            }
        }
        a.cmp(&b)
    }

    /// The order of two rows in the sorted view.
    fn order(&self, a: u32, b: u32) -> Ordering {
        self.order_from(0, a, b)
    }

    /// Works out the view from scratch: filter, sort, group.
    fn rebuild(&mut self) {
        let filter_on = self.filtering();
        let pass = self.row_test();
        let mut rows: Vec<u32> = (0..self.keys.len() as u32).filter(|&r| pass(r)).collect();
        drop(pass);

        match self.sort.first() {
            None => {}
            Some(&(first, desc)) if self.cols[first].kind == Kind::Number => {
                // Numbers are read out once, so sorting compares them side by
                // side. What cannot be sorted (an error, not a number) is NaN here.
                let mut keyed: Vec<(f64, u32)> = rows
                    .iter()
                    .map(|&r| match self.cell(r, first) {
                        Value::F64(x) => (*x, r),
                        _ => (f64::NAN, r),
                    })
                    .collect();
                keyed.sort_by(|a, b| {
                    let order = match (a.0.is_nan(), b.0.is_nan()) {
                        (false, false) if desc => b.0.total_cmp(&a.0),
                        (false, false) => a.0.total_cmp(&b.0),
                        // The unsortable come last either way.
                        (x, y) => x.cmp(&y),
                    };
                    order.then_with(|| self.order_from(1, a.1, b.1))
                });
                rows = keyed.into_iter().map(|(_, r)| r).collect();
            }
            Some(_) => rows.sort_by(|&a, &b| self.order(a, b)),
        }

        let buckets = match self.group_by {
            None => vec![Bucket::new("", rows)],
            Some(by) => {
                // Each group's rows, in sorted order.
                let mut slots: HashMap<&str, usize> = HashMap::new();
                let mut buckets: Vec<Bucket> = Vec::new();
                for &r in &rows {
                    let key = self.group_key(r, by);
                    let slot = *slots.entry(key).or_insert_with(|| {
                        buckets.push(Bucket::new(key, Vec::new()));
                        buckets.len() - 1
                    });
                    buckets[slot].rows.push(r);
                }
                buckets
            }
        };
        self.finish(buckets, filter_on, None);
    }

    fn group_key(&self, row: u32, by: usize) -> &str {
        match self.cell(row, by) {
            Value::Str(s) => s,
            _ => "",
        }
    }

    /// Brings the view up to date by moving only the rows whose cells were
    /// set (see [`can_patch`](Self::can_patch)): they are taken out, and those
    /// that pass the filters go back where they now sort. Returns false,
    /// having changed nothing, if it turns out the view needs working out
    /// from scratch.
    fn patch(&mut self) -> bool {
        let filter_on = self.filtering();
        let n = self.cols.len();
        let mut is_changed = vec![false; self.keys.len()];
        for &r in &self.changed_rows {
            is_changed[r as usize] = true;
        }
        let kept = |rows: &[u32]| -> Vec<u32> {
            rows.iter()
                .copied()
                .filter(|&r| !is_changed[r as usize])
                .collect()
        };
        // One bucket per group, or a single one when rows are not grouped.
        // A row that was set is still in the group it was in (rows set into
        // another group are not handled here), so only the groups of the
        // rows that were set can have changed: the others keep their rows
        // and, where the grid adds totals up itself, their totals.
        let mut buckets: Vec<Bucket> = match self.group_by {
            None => vec![Bucket::new("", kept(&self.sorted))],
            Some(by) => {
                let slots: HashMap<&str, usize> = self
                    .groups
                    .iter()
                    .enumerate()
                    .map(|(k, g)| (g.key.as_str(), k))
                    .collect();
                let mut touched = vec![false; self.groups.len()];
                for &r in &self.changed_rows {
                    if let Some(&k) = slots.get(self.group_key(r, by)) {
                        touched[k] = true;
                    }
                }
                self.groups
                    .iter()
                    .zip(touched)
                    .map(|(g, touched)| {
                        let rows = &self.sorted[g.start..g.start + g.len];
                        if touched {
                            Bucket::new(&g.key, kept(rows))
                        } else {
                            Bucket {
                                key: g.key.clone(),
                                rows: rows.to_vec(),
                                plain: Some(g.plain.clone()).filter(|plain| plain.len() == n),
                            }
                        }
                    })
                    .collect()
            }
        };

        let pass = self.row_test();
        let mut back: Vec<Vec<u32>> = vec![Vec::new(); buckets.len()];
        for r in (0..self.keys.len() as u32).filter(|&r| is_changed[r as usize] && pass(r)) {
            let bucket = match self.group_by {
                None => 0,
                Some(by) => {
                    let key = self.group_key(r, by);
                    match buckets.iter().position(|b| b.key == key) {
                        Some(bucket) => bucket,
                        // Its group has no rows showing, so it has no line yet.
                        None => return false,
                    }
                }
            };
            back[bucket].push(r);
        }
        drop(pass);

        for (bucket, mut rows) in buckets.iter_mut().zip(back) {
            if rows.is_empty() {
                continue;
            }
            rows.sort_by(|&a, &b| self.order(a, b));
            let kept = std::mem::take(&mut bucket.rows);
            let mut merged = Vec::with_capacity(kept.len() + rows.len());
            let mut from = 0;
            for r in rows {
                let at =
                    from + kept[from..].partition_point(|&x| self.order(x, r) == Ordering::Less);
                merged.extend_from_slice(&kept[from..at]);
                merged.push(r);
                from = at;
            }
            merged.extend_from_slice(&kept[from..]);
            bucket.rows = merged;
        }
        if self.group_by.is_some() {
            buckets.retain(|bucket| !bucket.rows.is_empty());
        }
        let changed = std::mem::take(&mut self.changed_rows);
        self.finish(buckets, filter_on, Some(&changed));
        true
    }

    /// Stores the view from its rows in buckets, each in sorted order: one
    /// bucket per group, or a single one when rows are not grouped. Puts the
    /// groups in order and adds up what the grid has to add up itself.
    ///
    /// `changed` is the rows whose cells were set, when only they can differ
    /// from last time; `None` when any row may.
    fn finish(&mut self, mut buckets: Vec<Bucket>, filter_on: bool, changed: Option<&[u32]>) {
        let n = self.cols.len();
        // Whether the grid adds the groups' totals up, rather than reading the graph's.
        let plain = filter_on || self.plain_groups;
        if plain {
            self.copy_numbers(changed.filter(|_| self.copies_fresh));
        } else {
            self.copies_fresh = false;
        }
        let fold = |col: usize, rows: &[u32]| {
            let numbers = &self.copies[col];
            let agg = self.cols[col].agg;
            if numbers.is_empty() {
                // A count, or no total: nothing to read.
                return agg.fold(rows.iter().map(|_| f64::NAN));
            }
            agg.fold(rows.iter().map(|&r| numbers[r as usize]))
        };
        let mut groups: Vec<Group> = Vec::new();
        let rows = match self.group_by {
            None => buckets.pop().map(|bucket| bucket.rows).unwrap_or_default(),
            Some(by) => {
                if plain {
                    // A bucket that came with its totals did not change.
                    for bucket in &mut buckets {
                        if bucket.plain.is_none() {
                            bucket.plain =
                                Some((0..n).map(|col| fold(col, &bucket.rows)).collect());
                        }
                    }
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
                    .map(|(k, bucket)| {
                        let total = match (by_total, &bucket.plain) {
                            (Some((col, _)), Some(plain)) => plain[col],
                            (Some((col, _)), None) => self.cols[col].agg.of_graph(
                                self.group_number(by, &bucket.key, col),
                                bucket.rows.len(),
                            ),
                            (None, _) => 0.0,
                        };
                        (total, k)
                    })
                    .collect();
                order.sort_by(|a, b| {
                    let by_name = buckets[a.1].key.cmp(&buckets[b.1].key);
                    match by_total {
                        Some((_, desc)) => {
                            let order = a.0.total_cmp(&b.0);
                            (if desc { order.reverse() } else { order }).then(by_name)
                        }
                        None if name_desc => by_name.reverse(),
                        None => by_name,
                    }
                });
                let mut rows = Vec::with_capacity(buckets.iter().map(|b| b.rows.len()).sum());
                for (_, k) in order {
                    let bucket = &mut buckets[k];
                    groups.push(Group {
                        key: std::mem::take(&mut bucket.key),
                        start: rows.len(),
                        len: bucket.rows.len(),
                        plain: bucket.plain.take().unwrap_or_default(),
                    });
                    rows.extend_from_slice(&bucket.rows);
                }
                rows
            }
        };
        let plain_totals: Vec<f64> = if filter_on {
            (0..n).map(|col| fold(col, &rows)).collect()
        } else {
            Vec::new()
        };

        self.filter_on = filter_on;
        self.plain_totals = plain_totals;
        self.sorted = rows;
        self.groups = groups;
        self.layout();
    }

    /// Brings `copies` up to date: for these rows, or for every row if `None`.
    fn copy_numbers(&mut self, rows: Option<&[u32]>) {
        let (n, count) = (self.cols.len(), self.keys.len());
        let number = |value: &Value| match value {
            Value::F64(x) => *x,
            _ => f64::NAN,
        };
        let mut copies = std::mem::take(&mut self.copies);
        copies.resize_with(n, Vec::new);
        for (col, numbers) in copies.iter_mut().enumerate() {
            if !matches!(
                self.cols[col].agg,
                Agg::Sum | Agg::Avg | Agg::Min | Agg::Max
            ) {
                numbers.clear();
                continue;
            }
            match rows {
                Some(rows) if numbers.len() == count => {
                    for &r in rows {
                        numbers[r as usize] = number(self.cell(r, col));
                    }
                }
                _ => {
                    numbers.clear();
                    numbers.extend((0..count as u32).map(|r| number(self.cell(r, col))));
                }
            }
        }
        self.copies = copies;
        self.copies_fresh = true;
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
        let op = self.cols.get(col)?.agg.op()?;
        let index = self.groupings.get(&by)?.get(col)?[slot(op)]?;
        self.c.group_node(&self.g, index, &Value::Str(key.into()))
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
                let number = col.total().and_then(|id| match self.read(id) {
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
        self.why(self.nodes[row * self.cols.len() + col])
    }

    /// Why a node has its value: now, or as of the update the grid is
    /// showing, when it is looking back.
    fn why(&self, id: NodeId) -> Result<String, String> {
        match self.as_of {
            None => Ok(self.g.explain_node(id)),
            Some(epoch) => self.g.explain_node_at(id, epoch).map_err(|e| e.to_string()),
        }
    }

    /// Why a group's total has its value.
    pub fn explain_group(&self, key: &str, col: usize) -> Result<String, String> {
        let c = self.col(col)?;
        let by = self.group_by.ok_or("rows are not grouped")?;
        if c.agg == Agg::Count {
            return Ok("The count is how many of the group's rows show.".into());
        }
        if self.plain_groups {
            if c.agg == Agg::None {
                return Err(format!("{} has no total", c.name));
            }
            let rows = self
                .groups
                .iter()
                .find(|g| g.key == key)
                .map_or(0, |g| g.len);
            return Ok(format!(
                "The {} of {} over this group's {rows} rows, added up by the grid.\n\n\
                 With this many rows the graph is not asked to keep a total for every group:\n\
                 that takes a node for each row in each totalled column.",
                c.agg.name(),
                c.name
            ));
        }
        let id = self
            .group_node(by, key, col)
            .ok_or_else(|| format!("{} has no total", c.name))?;
        Ok(self.with_filter_note(self.with_average_note(c, self.why(id)?)))
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
        let id = c
            .total()
            .ok_or_else(|| format!("{} has no total", c.name))?;
        Ok(self.with_filter_note(self.with_average_note(c, self.why(id)?)))
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
        } else if self.filter_on || self.plain_groups {
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
                    .filter(|_| !self.filter_on && !self.plain_groups)
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
            .and_then(|c| Some((c.total()?, c.agg)))
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
