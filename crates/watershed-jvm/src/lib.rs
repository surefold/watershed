//! watershed for Kotlin and the JVM.
//!
//! [UniFFI](https://mozilla.github.io/uniffi-rs/) turns the types and
//! methods here into Kotlin classes that call this library through JNA. The
//! surface is small and made of plain things (names, numbers, formulas as
//! text), so that crossing into the JVM stays simple:
//!
//! ```kotlin
//! val g = Graph()
//! val qty = g.input("qty", 10.0)                // Input<Double>
//! val price = g.input("price", 2.5)
//! val total = g.define("total", qty * price * 1.2)   // Node<Double>
//! g[price] = 3.0
//! g.stabilize()
//! val now: Double = g[total]
//! ```
//!
//! That is the typed layer in `kotlin/`, written by hand over the calls
//! here. In it an expression has a type, so adding a number to a string
//! does not compile, as in Rust. The calls here deal in [`Value`]s, node
//! numbers and expressions as lists of [`Op`]s, and have plainer names
//! (`input_value`, `define_expression`...) so that the typed layer can have
//! the good ones.
//!
//! `examples/kotlin` is a project that builds this library, generates the
//! Kotlin and runs it.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use watershed_core::{
    AggregateOp, BinOp, CalcError, CalcType, Collection as CoreCollection, Decimal, Ir, NodeId,
    Rounding, Type, UnOp,
};

uniffi::setup_scaffolding!();

/// Anything the engine refuses, with its message.
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum WatershedError {
    Refused(String),
}

impl fmt::Display for WatershedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let WatershedError::Refused(message) = self;
        f.write_str(message)
    }
}

impl std::error::Error for WatershedError {}

impl From<CalcError> for WatershedError {
    fn from(e: CalcError) -> Self {
        WatershedError::Refused(e.to_string())
    }
}

type Result<T> = std::result::Result<T, WatershedError>;

fn refused<T>(message: impl Into<String>) -> Result<T> {
    Err(WatershedError::Refused(message.into()))
}

/// A value of a node.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum Value {
    Number {
        value: f64,
    },
    Whole {
        value: i64,
    },
    /// An exact decimal, as its digits: `"19.99"`. A `BigDecimal` in Kotlin.
    Decimal {
        value: String,
    },
    Flag {
        value: bool,
    },
    Text {
        value: String,
    },
    /// Strings in order.
    Texts {
        items: Vec<String>,
    },
    /// The node could not be calculated, and why (a division by zero, say).
    Failed {
        message: String,
    },
}

impl Value {
    fn to_core(&self) -> Result<watershed_core::Value> {
        Ok(match self {
            Value::Number { value } => watershed_core::Value::F64(*value),
            Value::Whole { value } => watershed_core::Value::I64(*value),
            Value::Decimal { value } => match value.parse::<Decimal>() {
                Ok(decimal) => decimal.into_value(),
                Err(_) => {
                    return refused(format!(
                        "{value} is not a decimal the engine can hold: it keeps 28 significant digits"
                    ));
                }
            },
            Value::Flag { value } => watershed_core::Value::Bool(*value),
            Value::Text { value } => watershed_core::Value::Str(value.as_str().into()),
            Value::Texts { .. } => return refused("an input cannot be a list"),
            Value::Failed { message } => watershed_core::Value::error(message.clone()),
        })
    }

    fn from_core(value: &watershed_core::Value) -> Value {
        match value {
            watershed_core::Value::F64(x) => Value::Number { value: *x },
            watershed_core::Value::I64(x) => Value::Whole { value: *x },
            watershed_core::Value::Decimal(x) => Value::Decimal {
                value: x.to_string(),
            },
            watershed_core::Value::Bool(x) => Value::Flag { value: *x },
            watershed_core::Value::Str(s) => Value::Text {
                value: s.to_string(),
            },
            watershed_core::Value::List(items) => Value::Texts {
                items: items.iter().map(|s| s.to_string()).collect(),
            },
            watershed_core::Value::Error(message) => Value::Failed {
                message: message.to_string(),
            },
        }
    }
}

/// What a node or a field of a collection holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Kind {
    Number,
    Whole,
    /// An exact decimal of up to 28 digits.
    Decimal,
    Flag,
    Text,
    /// A list of strings. Calculated nodes only: not an input or a field.
    Texts,
}

impl Kind {
    fn of(ty: Type) -> Kind {
        match ty {
            Type::F64 => Kind::Number,
            Type::I64 => Kind::Whole,
            Type::Decimal => Kind::Decimal,
            Type::Bool => Kind::Flag,
            Type::Str => Kind::Text,
            Type::StrList => Kind::Texts,
        }
    }
}

/// A whole number given where a number is held becomes that number, so
/// that `12` can be set where `12.0` is meant.
fn fitted(value: watershed_core::Value, ty: Type) -> watershed_core::Value {
    match (value, ty) {
        (watershed_core::Value::I64(whole), Type::F64) => watershed_core::Value::F64(whole as f64),
        (watershed_core::Value::I64(whole), Type::Decimal) => Decimal::from(whole).into_value(),
        (value, _) => value,
    }
}

/// One step of an expression. An expression crosses into the engine as a
/// list of these in the order a calculator with a stack would take them:
/// `qty * price + 1` is `qty`, `price`, multiply, `1`, add.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum Op {
    /// A node of the graph.
    Node {
        node: u32,
    },
    /// A field or column of the collection the expression is for.
    Part {
        name: String,
    },
    Constant {
        value: Value,
    },
    /// Takes one value.
    Unary {
        op: UnaryOp,
    },
    /// Takes two values.
    Binary {
        op: BinaryOp,
    },
    /// Takes a condition and two values; only the chosen one is worked out.
    If,
    /// Takes an index and this many options.
    Choose {
        options: u32,
    },
    /// Takes this many strings and makes a list of them.
    Texts {
        items: u32,
    },
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum UnaryOp {
    Negate,
    Not,
    Absolute,
    ToText,
    /// A whole number as the same decimal.
    ToDecimal,
    /// A decimal as the nearest number.
    ToNumber,
}

#[derive(Debug, Clone, Copy, uniffi::Enum)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Least,
    Greatest,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Equal,
    NotEqual,
    And,
    Or,
    Concat,
    Join,
    /// A decimal to a number of places; a tie goes to the even neighbour.
    RoundHalfEven,
    /// A tie goes away from zero.
    RoundHalfUp,
    /// Toward zero.
    RoundDown,
    /// Away from zero.
    RoundUp,
}

/// Builds the engine's expression from a list of steps. `node` and `part`
/// say what a node number and a part's name stand for. The engine checks
/// the types when a node or column is made from the result.
fn expression(
    ops: &[Op],
    node: &dyn Fn(u32) -> Result<Ir>,
    part: &dyn Fn(&str) -> Result<Ir>,
) -> Result<Ir> {
    fn take(stack: &mut Vec<Ir>) -> Result<Arc<Ir>> {
        match stack.pop() {
            Some(ir) => Ok(Arc::new(ir)),
            None => refused("the expression is missing a value"),
        }
    }
    fn take_many(stack: &mut Vec<Ir>, count: u32) -> Result<Vec<Arc<Ir>>> {
        let mut items = (0..count)
            .map(|_| take(stack))
            .collect::<Result<Vec<_>>>()?;
        items.reverse();
        Ok(items)
    }
    let mut stack: Vec<Ir> = Vec::new();
    for op in ops {
        let next = match op {
            Op::Node { node: number } => node(*number)?,
            Op::Part { name } => part(name)?,
            Op::Constant { value } => Ir::Const(value.to_core()?),
            Op::Unary { op } => {
                let op = match op {
                    UnaryOp::Negate => UnOp::Neg,
                    UnaryOp::Not => UnOp::Not,
                    UnaryOp::Absolute => UnOp::Abs,
                    UnaryOp::ToText => UnOp::ToText,
                    UnaryOp::ToDecimal => UnOp::ToDecimal,
                    UnaryOp::ToNumber => UnOp::ToFloat,
                };
                Ir::Unary(op, take(&mut stack)?)
            }
            Op::Binary { op } => {
                let op = match op {
                    BinaryOp::Add => BinOp::Add,
                    BinaryOp::Subtract => BinOp::Sub,
                    BinaryOp::Multiply => BinOp::Mul,
                    BinaryOp::Divide => BinOp::Div,
                    BinaryOp::Least => BinOp::Min,
                    BinaryOp::Greatest => BinOp::Max,
                    BinaryOp::Less => BinOp::Lt,
                    BinaryOp::LessOrEqual => BinOp::Le,
                    BinaryOp::Greater => BinOp::Gt,
                    BinaryOp::GreaterOrEqual => BinOp::Ge,
                    BinaryOp::Equal => BinOp::Eq,
                    BinaryOp::NotEqual => BinOp::Ne,
                    BinaryOp::And => BinOp::And,
                    BinaryOp::Or => BinOp::Or,
                    BinaryOp::Concat => BinOp::Concat,
                    BinaryOp::Join => BinOp::Join,
                    BinaryOp::RoundHalfEven => BinOp::Round(Rounding::HalfEven),
                    BinaryOp::RoundHalfUp => BinOp::Round(Rounding::HalfUp),
                    BinaryOp::RoundDown => BinOp::Round(Rounding::Down),
                    BinaryOp::RoundUp => BinOp::Round(Rounding::Up),
                };
                let right = take(&mut stack)?;
                let left = take(&mut stack)?;
                Ir::Binary(op, left, right)
            }
            Op::If => {
                let otherwise = take(&mut stack)?;
                let then = take(&mut stack)?;
                let condition = take(&mut stack)?;
                Ir::If(condition, then, otherwise)
            }
            Op::Choose { options } => {
                let options = take_many(&mut stack, *options)?;
                Ir::Choose(take(&mut stack)?, options)
            }
            Op::Texts { items } => Ir::List(take_many(&mut stack, *items)?),
        };
        stack.push(next);
    }
    match (stack.pop(), stack.is_empty()) {
        (Some(ir), true) => Ok(ir),
        _ => refused("the expression does not come to one value"),
    }
}

/// What one update did.
#[derive(Debug, Clone, uniffi::Record)]
pub struct Report {
    /// This update's number; the first is 1.
    pub update: u64,
    /// Inputs whose value actually changed.
    pub inputs_changed: u64,
    /// Calculated nodes that were recalculated.
    pub recalculated: u64,
    /// Of those, how many came out with a different value.
    pub changed: u64,
}

/// The engine's graph, and the nodes that have been handed to the JVM.
///
/// The JVM names a node by a number. The engine's own ids cannot be made
/// from a number outside it, so each node handed out is kept here and its
/// place in this list is the number.
struct State {
    graph: watershed_core::Graph,
    handed_out: Vec<NodeId>,
    numbers: HashMap<NodeId, u32>,
}

impl State {
    /// The number the JVM knows this node by.
    fn number(&mut self, id: NodeId) -> u32 {
        if let Some(&number) = self.numbers.get(&id) {
            return number;
        }
        let number = u32::try_from(self.handed_out.len()).expect("too many nodes handed out");
        self.handed_out.push(id);
        self.numbers.insert(id, number);
        number
    }

    /// The node the JVM means by this number.
    fn node(&self, number: u32) -> Result<NodeId> {
        match self.handed_out.get(number as usize) {
            Some(&id) => Ok(id),
            None => refused(format!("there is no node {number}")),
        }
    }
}

impl std::ops::Deref for State {
    type Target = watershed_core::Graph;
    fn deref(&self) -> &watershed_core::Graph {
        &self.graph
    }
}

impl std::ops::DerefMut for State {
    fn deref_mut(&mut self) -> &mut watershed_core::Graph {
        &mut self.graph
    }
}

/// A graph of inputs and calculated nodes.
#[derive(uniffi::Object)]
pub struct Graph {
    inner: Mutex<State>,
}

impl Graph {
    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while the graph was held would be a bug in the engine;
        // the graph is still the best there is to carry on with.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[uniffi::export]
impl Graph {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Graph {
            inner: Mutex::new(State {
                graph: watershed_core::Graph::new(),
                handed_out: Vec::new(),
                numbers: HashMap::new(),
            }),
        })
    }

    /// Adds an input with a starting value. Returns the node's number.
    pub fn input_value(&self, name: String, value: Value) -> Result<u32> {
        let mut g = self.lock();
        let id = g.input_value(&name, value.to_core()?)?;
        Ok(g.number(id))
    }

    /// Adds a calculated node from a formula such as `qty * price * 1.2`.
    /// A name in it is the node of that name; a name with spaces or
    /// punctuation goes between backticks.
    pub fn define_formula(&self, name: String, formula: String) -> Result<u32> {
        let mut g = self.lock();
        let id = g.define_formula(&name, &formula)?;
        Ok(g.number(id))
    }

    /// Adds a calculated node from an expression. Returns the node's number.
    pub fn define_expression(&self, name: String, ops: Vec<Op>) -> Result<u32> {
        let mut g = self.lock();
        let ir = expression(&ops, &|number| Ok(g.reference(g.node(number)?)), &|name| {
            refused(format!(
                "{name:?} is part of a collection's rows, and can only be used in its columns"
            ))
        })?;
        let id = g.define_ir(&name, ir)?;
        Ok(g.number(id))
    }

    /// Records a new value for an input; it counts from the next `stabilize`.
    pub fn set_value(&self, node: u32, value: Value) -> Result<()> {
        let mut g = self.lock();
        let id = g.node(node)?;
        let value = fitted(value.to_core()?, g.node_type(id));
        Ok(g.set_value(id, value)?)
    }

    /// Applies every recorded change and recalculates what depends on them,
    /// and nothing else.
    pub fn stabilize(&self) -> Report {
        let report = self.lock().stabilize();
        Report {
            update: report.epoch,
            inputs_changed: report.inputs_changed as u64,
            recalculated: report.recalculated as u64,
            changed: report.changed as u64,
        }
    }

    /// The node's value as of the last update.
    pub fn value(&self, node: u32) -> Result<Value> {
        let g = self.lock();
        let id = g.node(node)?;
        if g.is_removed(id) {
            return refused(format!("{:?} has been removed", g.name(id)));
        }
        if !g.is_calculated(id) && !g.is_input(id) {
            return refused(format!(
                "{:?} is not being calculated: nothing that is watched uses it",
                g.name(id)
            ));
        }
        Ok(Value::from_core(g.value(id)))
    }

    /// Why the node has its value, as a tree of what it was worked out from.
    pub fn explain(&self, node: u32) -> Result<String> {
        let g = self.lock();
        Ok(g.explain_node(g.node(node)?))
    }

    /// What the node holds.
    pub fn kind(&self, node: u32) -> Result<Kind> {
        let g = self.lock();
        Ok(Kind::of(g.node_type(g.node(node)?)))
    }

    /// The node with this name, if there is one.
    pub fn find(&self, name: String) -> Option<u32> {
        let mut g = self.lock();
        g.find(&name).map(|id| g.number(id))
    }

    /// The node's name.
    pub fn name(&self, node: u32) -> Result<String> {
        let g = self.lock();
        Ok(g.name(g.node(node)?).to_string())
    }

    /// The names of the nodes the last update recalculated, in order.
    pub fn last_recalculated(&self) -> Vec<String> {
        let g = self.lock();
        g.last_recalculated()
            .iter()
            .map(|&id| g.name(id).to_string())
            .collect()
    }

    /// How many nodes the graph has had.
    pub fn size(&self) -> u64 {
        self.lock().len() as u64
    }

    /// Removes a node that nothing uses; its name is then free.
    pub fn remove(&self, node: u32) -> Result<()> {
        let mut g = self.lock();
        let id = g.node(node)?;
        Ok(g.remove_node(id)?)
    }

    /// Watches a node: in pull mode, it and what it uses are kept up to date.
    pub fn watch(&self, node: u32) -> Result<()> {
        let mut g = self.lock();
        let id = g.node(node)?;
        Ok(g.watch_node(id)?)
    }

    /// Stops watching a node.
    pub fn unwatch(&self, node: u32) -> Result<()> {
        let mut g = self.lock();
        let id = g.node(node)?;
        g.unwatch_node(id);
        Ok(())
    }

    /// Pull mode: while on, an update calculates only the watched nodes and
    /// what they use.
    pub fn calculate_only_watched(&self, on: bool) {
        self.lock().calculate_only_watched(on);
    }

    /// Checks every node against a calculation from scratch.
    pub fn check_consistency(&self) -> Result<()> {
        self.lock()
            .check_consistency()
            .map_err(WatershedError::Refused)
    }

    /// A new, empty keyed table of rows in this graph.
    pub fn collection(self: Arc<Self>, name: String) -> Arc<Collection> {
        let inner = CoreCollection::new(&self.lock(), &name);
        Arc::new(Collection {
            graph: self,
            inner: Mutex::new(inner),
            fields: Mutex::new(HashMap::new()),
        })
    }
}

/// A keyed table of rows, with fields, calculated columns and totals that
/// update by only the rows that change.
#[derive(uniffi::Object)]
pub struct Collection {
    graph: Arc<Graph>,
    inner: Mutex<CoreCollection>,
    /// What each field holds, to fit whole numbers to number fields.
    fields: Mutex<HashMap<String, Type>>,
}

impl Collection {
    fn lock(&self) -> MutexGuard<'_, CoreCollection> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn total(&self, name: &str, op: AggregateOp, of: Option<&str>) -> Result<u32> {
        // The graph first, then the collection, everywhere: one order, so
        // two threads can never each hold what the other is waiting for.
        let mut g = self.graph.lock();
        let mut c = self.lock();
        let of = match of {
            Some(part) => Some(c.parse_formula(&g, &format!("`{part}`"))?.0),
            None => None,
        };
        let id = c.add_total(&mut g, name, op, of.as_ref())?;
        Ok(g.number(id))
    }
}

#[uniffi::export]
impl Collection {
    /// The graph this collection is in.
    pub fn graph(&self) -> Arc<Graph> {
        self.graph.clone()
    }

    /// Adds a field. Fields come before the first row.
    pub fn add_field(&self, name: String, kind: Kind) -> Result<()> {
        let ty = match kind {
            Kind::Number => Type::F64,
            Kind::Whole => Type::I64,
            Kind::Decimal => Type::Decimal,
            Kind::Flag => Type::Bool,
            Kind::Text => Type::Str,
            Kind::Texts => return refused("a field cannot be a list"),
        };
        self.lock().add_field(&name, ty)?;
        self.fields
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name, ty);
        Ok(())
    }

    /// Adds a column from a formula written as text, over this collection's
    /// fields and columns and the graph's nodes.
    pub fn add_formula_column(&self, name: String, formula: String) -> Result<()> {
        let mut g = self.graph.lock();
        self.lock().add_formula_column(&mut g, &name, &formula)?;
        Ok(())
    }

    /// Adds a column, worked out in every row by an expression over this
    /// collection's fields and columns ([`Op::Part`]) and the graph's nodes.
    pub fn add_column(&self, name: String, ops: Vec<Op>) -> Result<()> {
        let mut g = self.graph.lock();
        let mut c = self.lock();
        let ir = expression(&ops, &|number| Ok(g.reference(g.node(number)?)), &|part| {
            Ok(c.parse_formula(&g, &format!("`{part}`"))?.0)
        })?;
        c.add_column(&mut g, &name, ir)?;
        Ok(())
    }

    /// A total: the sum of a field or column over the rows. Returns its node.
    pub fn add_sum(&self, name: String, of: String) -> Result<u32> {
        self.total(&name, AggregateOp::Sum, Some(&of))
    }

    /// The smallest value of a field or column. Returns its node.
    pub fn add_min(&self, name: String, of: String) -> Result<u32> {
        self.total(&name, AggregateOp::Min, Some(&of))
    }

    /// The largest value of a field or column. Returns its node.
    pub fn add_max(&self, name: String, of: String) -> Result<u32> {
        self.total(&name, AggregateOp::Max, Some(&of))
    }

    /// How many rows there are. Returns its node.
    pub fn add_count(&self, name: String) -> Result<u32> {
        self.total(&name, AggregateOp::Count, None)
    }

    /// Adds a row, or changes the fields given of one that is there. A new
    /// row needs every field. It counts from the next `stabilize`.
    pub fn upsert_values(&self, key: String, fields: HashMap<String, Value>) -> Result<()> {
        let values: Vec<(&str, watershed_core::Value)> = {
            let kinds = self.fields.lock().unwrap_or_else(|e| e.into_inner());
            fields
                .iter()
                .map(|(name, value)| {
                    let value = value.to_core()?;
                    Ok((
                        name.as_str(),
                        match kinds.get(name) {
                            Some(&ty) => fitted(value, ty),
                            None => value,
                        },
                    ))
                })
                .collect::<Result<_>>()?
        };
        let mut g = self.graph.lock();
        Ok(self.lock().upsert_named(&mut g, &key, &values)?)
    }

    /// Removes a row. Returns whether there was one.
    pub fn remove(&self, key: String) -> Result<bool> {
        let mut g = self.graph.lock();
        Ok(self.lock().remove(&mut g, &key)?)
    }

    /// A field or column of a row, as of the last update.
    pub fn value(&self, key: String, of: String) -> Result<Value> {
        let g = self.graph.lock();
        let c = self.lock();
        let (part, _) = c.parse_formula(&g, &format!("`{of}`"))?;
        Ok(Value::from_core(&c.get_value(&g, &key, &part)?))
    }

    /// How many rows there are, counting changes since the last update.
    pub fn size(&self) -> u64 {
        self.lock().len() as u64
    }
}
