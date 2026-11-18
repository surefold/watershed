//! From a workbook's formulas to a graph.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};

use watershed_core::prelude::*;
use watershed_core::{IntoExpr, NodeId, Value, when};

use crate::Error;
use crate::book::{Cell, Val, Workbook};
use crate::parse::{Ast, parse};

/// A workbook's formulas between some inputs and some outputs, as a graph.
pub struct Model {
    graph: Graph,
    /// Each varied cell, its input, and the value the workbook has for it.
    inputs: Vec<(Cell, Input<f64>, f64)>,
    outputs: Vec<(Cell, NodeId)>,
    /// Each cell that became a node, with what Excel saved for it.
    nodes: Vec<Translated>,
    by_cell: HashMap<Cell, NodeId>,
    folded: usize,
    folded_wrong: Vec<Mismatch>,
    functions: BTreeMap<String, usize>,
}

struct Translated {
    name: String,
    id: NodeId,
    excel: Val,
}

/// A cell whose value here is not what Excel saved for it.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub cell: String,
    pub here: String,
    pub excel: String,
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} here, {} in Excel",
            self.cell, self.here, self.excel
        )
    }
}

/// Following a chain of cells uses the stack, a few hundred bytes a cell; a
/// long running total can be a hundred thousand cells deep.
const STACK: usize = 1 << 30;

impl Model {
    /// Translates the formulas between the cells to vary and the cells to
    /// read. Each cell to vary must hold a number, and becomes an input
    /// whose value is what the workbook has now. Each cell to read must be
    /// a number that depends on at least one of them.
    pub fn build(book: &Workbook, vary: &[Cell], outputs: &[Cell]) -> Result<Model, Error> {
        std::thread::scope(|s| {
            std::thread::Builder::new()
                .name("watershed-xlsx".into())
                .stack_size(STACK)
                .spawn_scoped(s, || Model::build_here(book, vary, outputs))
                .expect("a thread to follow the formulas on")
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    }

    fn build_here(book: &Workbook, vary: &[Cell], outputs: &[Cell]) -> Result<Model, Error> {
        let mut b = Builder::new(book);
        let mut inputs = Vec::new();
        for &c in vary {
            let refuse = |problem: &str| Error::Cell {
                cell: book.name(c),
                problem: problem.into(),
            };
            if c.sheet >= book.sheets.len() {
                return Err(refuse("is on no sheet"));
            }
            if b.inputs.contains_key(&c) {
                return Err(refuse("is given twice as an input"));
            }
            let Val::Num(now) = book.value(c) else {
                return Err(refuse("does not hold a number, so cannot be varied"));
            };
            let input =
                b.g.input(&book.name(c), now)
                    .map_err(|e| refuse(&e.to_string()))?;
            b.inputs.insert(c, input);
            inputs.push((c, input, now));
        }
        let mut output_ids = Vec::new();
        for &c in outputs {
            let refuse = |problem: &str| Error::Cell {
                cell: book.name(c),
                problem: problem.into(),
            };
            if c.sheet >= book.sheets.len() {
                return Err(refuse("is on no sheet"));
            }
            if b.inputs.contains_key(&c) {
                return Err(refuse("is an input, so cannot also be an output"));
            }
            match b.cell(c)? {
                Out::Num(_) => output_ids.push((c, b.by_cell[&c])),
                Out::Bool(_) => return Err(refuse("is true or false, not a number")),
                Out::Fixed(_) => return Err(refuse("does not depend on any of the inputs given")),
            }
        }
        let mut graph = b.g;
        graph.stabilize();
        Ok(Model {
            graph,
            inputs,
            outputs: output_ids,
            by_cell: b.by_cell,
            nodes: b.nodes,
            folded: b.folded,
            folded_wrong: b.folded_wrong,
            functions: b.functions,
        })
    }

    /// Each translated cell against what Excel saved for it, with the inputs
    /// as the workbook has them (which this sets them back to). Empty when
    /// all agree, to about twelve figures.
    pub fn check(&mut self) -> Vec<Mismatch> {
        let now: Vec<f64> = self.inputs.iter().map(|i| i.2).collect();
        self.set(&now).expect("the workbook's own inputs");
        let mut wrong = self.folded_wrong.clone();
        for t in &self.nodes {
            let here = self.graph.value(t.id);
            let same = match (here, &t.excel) {
                (Value::F64(here), Val::Num(excel)) => close(*here, *excel),
                (Value::Bool(here), Val::Bool(excel)) => here == excel,
                // An error in Excel is not a finite number here.
                (Value::F64(here), Val::Error) => !here.is_finite(),
                _ => false,
            };
            if !same {
                wrong.push(Mismatch {
                    cell: t.name.clone(),
                    here: match here {
                        Value::F64(x) => x.to_string(),
                        other => format!("{other:?}"),
                    },
                    excel: show(&t.excel),
                });
            }
        }
        wrong
    }

    /// Sets the inputs, in the order they were given, and works the graph
    /// out again.
    pub fn set(&mut self, values: &[f64]) -> Result<(), Error> {
        if values.len() != self.inputs.len() {
            return Err(Error::Read(format!(
                "{} values for {} inputs",
                values.len(),
                self.inputs.len()
            )));
        }
        for (&(c, input, _), &x) in self.inputs.iter().zip(values) {
            self.graph.set(input, x).map_err(|e| Error::Cell {
                cell: format!("{c:?}"),
                problem: e.to_string(),
            })?;
        }
        self.graph.stabilize();
        Ok(())
    }

    /// What a translated cell, or an input, comes to now. `None` for a cell
    /// that was not translated; true and false are 1 and 0.
    pub fn value(&self, c: Cell) -> Option<f64> {
        let id = match self.inputs.iter().find(|i| i.0 == c) {
            Some(i) => i.1.id(),
            None => *self.by_cell.get(&c)?,
        };
        match self.graph.value(id) {
            Value::F64(x) => Some(*x),
            Value::Bool(b) => Some(f64::from(u8::from(*b))),
            _ => None,
        }
    }

    /// The node a translated cell became.
    pub fn node(&self, c: Cell) -> Option<NodeId> {
        self.by_cell.get(&c).copied()
    }

    /// The inputs' nodes, in the order the cells were given.
    pub fn inputs(&self) -> Vec<NodeId> {
        self.inputs.iter().map(|i| i.1.id()).collect()
    }

    /// What the workbook has for each input, in the same order.
    pub fn workbook_inputs(&self) -> Vec<f64> {
        self.inputs.iter().map(|i| i.2).collect()
    }

    /// The outputs' nodes, in the order the cells were given.
    pub fn outputs(&self) -> Vec<NodeId> {
        self.outputs.iter().map(|o| o.1).collect()
    }

    /// How many cells became nodes.
    pub fn translated(&self) -> usize {
        self.nodes.len()
    }

    /// How many cells were followed through their formula to a value that
    /// does not depend on the inputs, such as a condition's.
    pub fn folded(&self) -> usize {
        self.folded
    }

    /// Each function met on the way, and how many times.
    pub fn functions(&self) -> &BTreeMap<String, usize> {
        &self.functions
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    /// The graph, to change. A cell's value read with [`Model::value`]
    /// comes from it, so it should be worked out (`stabilize`) after.
    pub fn graph_mut(&mut self) -> &mut Graph {
        &mut self.graph
    }

    pub fn into_graph(self) -> Graph {
        self.graph
    }
}

fn show(v: &Val) -> String {
    match v {
        Val::Num(x) => x.to_string(),
        Val::Text(t) => format!("{t:?}"),
        Val::Bool(b) => b.to_string().to_uppercase(),
        Val::Empty => "empty".into(),
        Val::Error => "an error".into(),
    }
}

/// The same to about twelve figures: Excel keeps fifteen and works things
/// out in an order of its own.
pub(crate) fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6) || (a.is_nan() && b.is_nan())
}

// ---- Building ----

/// What a formula, or part of one, comes to.
#[derive(Clone)]
pub(crate) enum Out {
    /// The same whatever the varied inputs are.
    Fixed(Val),
    Num(Expr<f64>),
    Bool(Expr<bool>),
}

pub(crate) struct Builder<'a> {
    pub(crate) book: &'a Workbook,
    pub(crate) g: Graph,
    pub(crate) inputs: HashMap<Cell, Input<f64>>,
    /// What each cell reached so far comes to.
    cells: HashMap<Cell, Out>,
    /// Whether each cell looked at depends on a varied input.
    depends: HashMap<Cell, bool>,
    /// Working out which cells read each other in loops.
    loops: Loops,
    /// The cells whose formulas are being followed, the innermost last.
    building: Vec<Cell>,
    building_set: HashSet<Cell>,
    nodes: Vec<Translated>,
    by_cell: HashMap<Cell, NodeId>,
    folded: usize,
    folded_wrong: Vec<Mismatch>,
    pub(crate) functions: BTreeMap<String, usize>,
}

pub(crate) type Built<T> = Result<T, Error>;

/// What Tarjan's way of finding loops keeps, for the cells not yet settled.
#[derive(Default)]
struct Loops {
    next: usize,
    index: HashMap<Cell, usize>,
    low: HashMap<Cell, usize>,
    stack: Vec<Cell>,
    on_stack: HashSet<Cell>,
    /// Whether a cell in a loop not yet closed reads, outside the loop,
    /// something that depends on an input.
    reaches: HashMap<Cell, bool>,
}

/// Every cell a formula reads, on every branch.
fn mentioned(ast: &Ast, out: &mut Vec<Cell>) {
    match ast {
        Ast::Num(_) | Ast::Text(_) | Ast::Bool(_) | Ast::Error | Ast::Missing => {}
        Ast::Cell(c) => out.push(*c),
        Ast::Range(a, b) => out.extend(in_range(*a, *b)),
        Ast::Negate(a) | Ast::Percent(a) => mentioned(a, out),
        Ast::Binary(_, a, b) => {
            mentioned(a, out);
            mentioned(b, out);
        }
        Ast::Call(_, args) => args.iter().for_each(|a| mentioned(a, out)),
    }
}

impl<'a> Builder<'a> {
    fn new(book: &'a Workbook) -> Builder<'a> {
        Builder {
            book,
            g: Graph::new(),
            inputs: HashMap::new(),
            cells: HashMap::new(),
            depends: HashMap::new(),
            loops: Loops::default(),
            building: Vec::new(),
            building_set: HashSet::new(),
            nodes: Vec::new(),
            by_cell: HashMap::new(),
            folded: 0,
            folded_wrong: Vec::new(),
            functions: BTreeMap::new(),
        }
    }

    /// A problem with the cell whose formula is being followed.
    pub(crate) fn fail<T>(&self, problem: impl Into<String>) -> Built<T> {
        Err(Error::Cell {
            cell: self
                .building
                .last()
                .map_or_else(|| "?".into(), |&c| self.book.name(c)),
            problem: problem.into(),
        })
    }

    fn parsed(&self, c: Cell) -> Built<Option<Ast>> {
        let Some(formula) = self.book.formula(c) else {
            return Ok(None);
        };
        parse(self.book, c.sheet, formula)
            .map(Some)
            .map_err(|problem| Error::Cell {
                cell: self.book.name(c),
                problem,
            })
    }

    /// Whether a cell's formula mentions, through any number of cells, a
    /// varied input. This follows every branch, so it may say yes where the
    /// branch that mentions one is never taken; that is found out later.
    ///
    /// Cells that read each other in a loop are found together, as one
    /// strongly connected component (Tarjan's way), and all depend on an
    /// input if any does. Whether a loop is a problem is found out only if
    /// the formulas followed go round it.
    fn depends(&mut self, c: Cell) -> Built<bool> {
        if self.inputs.contains_key(&c) {
            return Ok(true);
        }
        if let Some(&known) = self.depends.get(&c) {
            return Ok(known);
        }
        self.connect(c)?;
        Ok(self.depends[&c])
    }

    fn connect(&mut self, v: Cell) -> Built<()> {
        let index = self.loops.next;
        self.loops.next += 1;
        self.loops.index.insert(v, index);
        self.loops.low.insert(v, index);
        self.loops.stack.push(v);
        self.loops.on_stack.insert(v);
        let mut reaches = false;
        let mut read = Vec::new();
        if let Some(ast) = self.parsed(v)? {
            mentioned(&ast, &mut read);
        }
        for w in read {
            if self.inputs.contains_key(&w) {
                reaches = true;
            } else if let Some(&known) = self.depends.get(&w) {
                reaches |= known;
            } else if let Some(&seen) = self.loops.index.get(&w) {
                // Still being looked at, so in a loop with this one.
                debug_assert!(self.loops.on_stack.contains(&w));
                let low = self.loops.low[&v].min(seen);
                self.loops.low.insert(v, low);
            } else {
                self.connect(w)?;
                if let Some(&known) = self.depends.get(&w) {
                    // A loop of its own, or no loop, now settled.
                    reaches |= known;
                } else {
                    // In a loop with this one, not yet closed.
                    let low = self.loops.low[&v].min(self.loops.low[&w]);
                    self.loops.low.insert(v, low);
                    reaches |= self.loops.reaches.remove(&w).unwrap_or(false);
                }
            }
        }
        if self.loops.low[&v] == index {
            // The first of a loop, or a cell on its own: everything in it
            // depends on an input if anything does. What the others reach
            // has been gathered into this one's as each was finished.
            let mut members = Vec::new();
            loop {
                let w = self.loops.stack.pop().expect("on the stack");
                self.loops.on_stack.remove(&w);
                members.push(w);
                if w == v {
                    break;
                }
            }
            for w in members {
                self.loops.index.remove(&w);
                self.loops.low.remove(&w);
                self.depends.insert(w, reaches);
            }
        } else {
            self.loops.reaches.insert(v, reaches);
        }
        Ok(())
    }

    /// What a cell comes to, making nodes for it and whatever it reads.
    pub(crate) fn cell(&mut self, c: Cell) -> Built<Out> {
        if let Some(input) = self.inputs.get(&c) {
            return Ok(Out::Num(input.into_expr()));
        }
        if let Some(known) = self.cells.get(&c) {
            return Ok(known.clone());
        }
        if !self.depends(c)? {
            let out = Out::Fixed(self.book.value(c));
            self.cells.insert(c, out.clone());
            return Ok(out);
        }
        if self.building_set.contains(&c) {
            return Err(Error::Cell {
                cell: self.book.name(c),
                problem: "reads itself, through other cells, and depends on an input".into(),
            });
        }
        let ast = self.parsed(c)?.expect("a cell that depends has a formula");
        self.building.push(c);
        self.building_set.insert(c);
        let out = self.eval(&ast);
        self.building.pop();
        self.building_set.remove(&c);
        let out = self.translated(c, out?)?;
        self.cells.insert(c, out.clone());
        Ok(out)
    }

    fn translated(&mut self, c: Cell, out: Out) -> Built<Out> {
        let name = self.book.name(c);
        let excel = self.book.value(c);
        let define = |e: CalcError| Error::Cell {
            cell: name.clone(),
            problem: e.to_string(),
        };
        Ok(match out {
            Out::Fixed(v) => {
                // The formula was followed; it should have come to what
                // Excel has.
                self.folded += 1;
                let same = match (&v, &excel) {
                    (Val::Num(a), Val::Num(b)) => close(*a, *b),
                    (a, b) => {
                        order(a, b) == Some(Ordering::Equal)
                            || (*a == Val::Error && *b == Val::Error)
                    }
                };
                if !same {
                    self.folded_wrong.push(Mismatch {
                        cell: name.clone(),
                        here: show(&v),
                        excel: show(&excel),
                    });
                }
                Out::Fixed(v)
            }
            Out::Num(e) => {
                let node = self.g.define(&name, e).map_err(define)?;
                self.record(c, name.clone(), node.id(), excel);
                Out::Num(node.into_expr())
            }
            Out::Bool(e) => {
                let node = self.g.define(&name, e).map_err(define)?;
                self.record(c, name.clone(), node.id(), excel);
                Out::Bool(node.into_expr())
            }
        })
    }

    fn record(&mut self, cell: Cell, name: String, id: NodeId, excel: Val) {
        self.by_cell.insert(cell, id);
        self.nodes.push(Translated { name, id, excel });
    }

    pub(crate) fn eval(&mut self, ast: &Ast) -> Built<Out> {
        match ast {
            Ast::Num(x) => Ok(Out::Fixed(Val::Num(*x))),
            Ast::Text(t) => Ok(Out::Fixed(Val::Text(t.clone()))),
            Ast::Bool(b) => Ok(Out::Fixed(Val::Bool(*b))),
            Ast::Error => Ok(Out::Fixed(Val::Error)),
            Ast::Missing => Ok(Out::Fixed(Val::Empty)),
            Ast::Cell(c) => self.cell(*c),
            Ast::Range(a, b) if a == b => self.cell(*a),
            Ast::Range(..) => self.fail("a range where one value is wanted"),
            Ast::Negate(a) => Ok(match self.eval(a)? {
                Out::Fixed(v) => Out::Fixed(number(&v).map_or(Val::Error, |x| Val::Num(-x))),
                other => Out::Num(-self.as_num(other)?),
            }),
            Ast::Percent(a) => Ok(match self.eval(a)? {
                Out::Fixed(v) => Out::Fixed(number(&v).map_or(Val::Error, |x| num(x / 100.0))),
                other => Out::Num(self.as_num(other)? / 100.0),
            }),
            Ast::Binary(op, a, b) => {
                let (a, b) = (self.eval(a)?, self.eval(b)?);
                self.binary(op, a, b)
            }
            Ast::Call(name, args) => {
                *self.functions.entry(name.clone()).or_default() += 1;
                self.call(name, args)
            }
        }
    }

    pub(crate) fn as_num(&self, o: Out) -> Built<Expr<f64>> {
        match o {
            Out::Num(e) => Ok(e),
            Out::Bool(b) => Ok(when(b).then(1.0).otherwise(0.0)),
            // Where Excel has an error, this has not-a-number.
            Out::Fixed(Val::Error) => Ok(f64::NAN.into_expr()),
            Out::Fixed(v) => match number(&v) {
                Some(x) => Ok(x.into_expr()),
                None => self.fail(format!(
                    "{} where a number that depends on an input is wanted",
                    show(&v)
                )),
            },
        }
    }

    pub(crate) fn as_bool(&self, o: Out) -> Built<Expr<bool>> {
        match o {
            Out::Bool(b) => Ok(b),
            Out::Num(e) => Ok(e.not_equals(0.0)),
            Out::Fixed(v) => match truth(&v) {
                Some(t) => Ok(t.into_expr()),
                None => self.fail(format!(
                    "{} where true or false that depends on an input is wanted",
                    show(&v)
                )),
            },
        }
    }

    pub(crate) fn binary(&self, op: &str, a: Out, b: Out) -> Built<Out> {
        use Ordering::*;
        if matches!(a, Out::Fixed(Val::Error)) || matches!(b, Out::Fixed(Val::Error)) {
            return Ok(Out::Fixed(Val::Error));
        }
        let compares = matches!(op, "=" | "<>" | "<" | ">" | "<=" | ">=");
        let decide = |o: Ordering| match op {
            "=" => o == Equal,
            "<>" => o != Equal,
            "<" => o == Less,
            ">" => o == Greater,
            "<=" => o != Greater,
            _ => o != Less,
        };
        if let (Out::Fixed(a), Out::Fixed(b)) = (&a, &b) {
            if compares {
                return Ok(Out::Fixed(match order(a, b) {
                    Some(o) => Val::Bool(decide(o)),
                    None => Val::Error,
                }));
            }
            if op == "&" {
                return Ok(Out::Fixed(Val::Text(as_text(a) + &as_text(b))));
            }
            let (Some(x), Some(y)) = (number(a), number(b)) else {
                return Ok(Out::Fixed(Val::Error));
            };
            return Ok(Out::Fixed(match op {
                "+" => num(x + y),
                "-" => num(x - y),
                "*" => num(x * y),
                "/" => num(x / y),
                _ if x == 0.0 && y == 0.0 => Val::Error,
                _ => num(x.powf(y)),
            }));
        }
        if op == "&" {
            return self.fail("text joined to something that depends on an input");
        }
        if compares {
            // Excel puts every number below every piece of text, and both
            // below true and false; an empty cell is whichever it is beside.
            let rank = |o: &Out| match o {
                Out::Num(_) | Out::Fixed(Val::Num(_)) => Some(0),
                Out::Fixed(Val::Text(_)) => Some(1),
                Out::Bool(_) | Out::Fixed(Val::Bool(_)) => Some(2),
                Out::Fixed(_) => None,
            };
            if let (Some(x), Some(y)) = (rank(&a), rank(&b)) {
                if x != y {
                    return Ok(Out::Fixed(Val::Bool(decide(x.cmp(&y)))));
                }
            }
        } else if [&a, &b]
            .iter()
            .any(|o| matches!(o, Out::Fixed(v @ Val::Text(_)) if number(v).is_none()))
        {
            return Ok(Out::Fixed(Val::Error));
        }
        let (a, b) = (self.as_num(a)?, self.as_num(b)?);
        Ok(match op {
            "+" => Out::Num(a + b),
            "-" => Out::Num(a - b),
            "*" => Out::Num(a * b),
            "/" => Out::Num(a / b),
            "^" => Out::Num(a.pow(b)),
            "=" => Out::Bool(a.equals(b)),
            "<>" => Out::Bool(a.not_equals(b)),
            "<" => Out::Bool(a.lt(b)),
            ">" => Out::Bool(a.gt(b)),
            "<=" => Out::Bool(a.le(b)),
            _ => Out::Bool(a.ge(b)),
        })
    }
}

pub(crate) fn in_range(a: Cell, b: Cell) -> impl Iterator<Item = Cell> {
    (a.row..=b.row).flat_map(move |row| {
        (a.col..=b.col).map(move |col| Cell {
            sheet: a.sheet,
            row,
            col,
        })
    })
}

/// A number Excel would keep: anything not finite is an error.
pub(crate) fn num(x: f64) -> Val {
    if x.is_finite() {
        Val::Num(x)
    } else {
        Val::Error
    }
}

pub(crate) fn number(v: &Val) -> Option<f64> {
    match v {
        Val::Num(x) => Some(*x),
        Val::Bool(b) => Some(f64::from(u8::from(*b))),
        Val::Empty => Some(0.0),
        Val::Text(t) => t.trim().parse().ok().filter(|x: &f64| x.is_finite()),
        Val::Error => None,
    }
}

pub(crate) fn truth(v: &Val) -> Option<bool> {
    match v {
        Val::Bool(b) => Some(*b),
        Val::Num(x) => Some(*x != 0.0),
        Val::Empty => Some(false),
        Val::Text(t) if t.eq_ignore_ascii_case("TRUE") => Some(true),
        Val::Text(t) if t.eq_ignore_ascii_case("FALSE") => Some(false),
        Val::Text(_) | Val::Error => None,
    }
}

fn as_text(v: &Val) -> String {
    match v {
        Val::Text(t) => t.clone(),
        Val::Num(x) => x.to_string(),
        Val::Bool(b) => b.to_string().to_uppercase(),
        Val::Empty | Val::Error => String::new(),
    }
}

/// How Excel orders two values: numbers, then text (ignoring case), then
/// true and false. An empty cell is 0 beside a number, "" beside text and
/// false beside true or false.
pub(crate) fn order(a: &Val, b: &Val) -> Option<Ordering> {
    let rank = |v: &Val| match v {
        Val::Num(_) => 0,
        Val::Text(_) => 1,
        Val::Bool(_) => 2,
        Val::Empty | Val::Error => 3,
    };
    match (a, b) {
        (Val::Error, _) | (_, Val::Error) => None,
        (Val::Empty, Val::Empty) => Some(Ordering::Equal),
        (Val::Empty, Val::Num(_)) => order(&Val::Num(0.0), b),
        (Val::Num(_), Val::Empty) => order(a, &Val::Num(0.0)),
        (Val::Empty, Val::Text(_)) => order(&Val::Text(String::new()), b),
        (Val::Text(_), Val::Empty) => order(a, &Val::Text(String::new())),
        (Val::Empty, Val::Bool(_)) => order(&Val::Bool(false), b),
        (Val::Bool(_), Val::Empty) => order(a, &Val::Bool(false)),
        (Val::Num(x), Val::Num(y)) => x.partial_cmp(y),
        (Val::Text(x), Val::Text(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
        (Val::Bool(x), Val::Bool(y)) => Some(x.cmp(y)),
        _ => Some(rank(a).cmp(&rank(b))),
    }
}
