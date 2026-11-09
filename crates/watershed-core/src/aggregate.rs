//! Totals over many nodes (`sum`, `count`, `min`, `max`) that update by only
//! the members that changed.
//!
//! Each member is a pair of nodes: a value, and a `bool` saying whether the
//! member is present (a removed row is absent, and counts as nothing). An
//! aggregate keeps a balanced binary tree of partial results over its
//! members, so a change to one member updates only the path from its leaf to
//! the root: about 20 steps for a million members.
//!
//! The tree's shape depends only on the number of members (it has room for
//! the next power of two), so the result is exactly, bit for bit, what folding
//! the same tree from scratch gives. A running total that subtracted old
//! values and added new ones would drift with floating-point rounding. Empty
//! room in the tree holds the operation's identity, chosen so that it never
//! changes a result: `-0.0` for a float sum (`x + -0.0` is `x` for every `x`,
//! including `-0.0`), `0` for an integer sum or a count, and the largest or
//! smallest value for `min` and `max`. Growing the tree therefore never
//! changes the result either.

use std::collections::HashMap;
use std::fmt;

use crate::value::{Type, Value};

/// How an aggregate combines its members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateOp {
    /// The sum of the present members' values (`f64` or `i64`; `0` if none).
    Sum,
    /// How many members are present (`i64`). Values are ignored.
    Count,
    /// The smallest present value (`f64` or `i64`; +∞ or `i64::MAX` if none;
    /// NaN if any present value is NaN).
    Min,
    /// The largest present value (`f64` or `i64`; -∞ or `i64::MIN` if none;
    /// NaN if any present value is NaN).
    Max,
}

impl fmt::Display for AggregateOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            AggregateOp::Sum => "sum",
            AggregateOp::Count => "count",
            AggregateOp::Min => "min",
            AggregateOp::Max => "max",
        })
    }
}

impl AggregateOp {
    /// The type of the result, given the type of the members' values.
    pub(crate) fn result_type(self, value: Type) -> Type {
        match self {
            AggregateOp::Count => Type::I64,
            _ => value,
        }
    }
}

/// The value that never changes a result: what absent members and empty
/// room in the tree hold.
fn identity(op: AggregateOp, ty: Type) -> Value {
    match (op, ty) {
        (AggregateOp::Sum, Type::F64) => Value::F64(-0.0),
        (AggregateOp::Min, Type::F64) => Value::F64(f64::INFINITY),
        (AggregateOp::Max, Type::F64) => Value::F64(f64::NEG_INFINITY),
        (AggregateOp::Min, Type::I64) => Value::I64(i64::MAX),
        (AggregateOp::Max, Type::I64) => Value::I64(i64::MIN),
        _ => Value::I64(0),
    }
}

/// What one member contributes.
fn leaf(op: AggregateOp, ty: Type, value: &Value, present: &Value) -> Value {
    match present {
        Value::Bool(false) => identity(op, ty),
        Value::Error(e) => Value::Error(e.clone()),
        Value::Bool(true) => match (op, value) {
            (AggregateOp::Count, _) => Value::I64(1),
            (_, v) => v.clone(),
        },
        other => Value::error(format!("internal type error: presence is {other}")),
    }
}

/// Combines two partial results. An error wins, the left one first.
fn combine(op: AggregateOp, a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Error(e), _) | (_, Value::Error(e)) => Value::Error(e.clone()),
        // NaN wins, as it does in a sum: Rust's `f64::min` and `f64::max`
        // ignore NaN, which would make the result depend on the tree's shape
        // (NaN against empty room gives the padding, alone it gives NaN).
        (Value::F64(x), Value::F64(y)) if x.is_nan() || y.is_nan() => {
            Value::F64(if x.is_nan() { *x } else { *y })
        }
        (Value::F64(x), Value::F64(y)) => Value::F64(match op {
            AggregateOp::Sum | AggregateOp::Count => x + y,
            AggregateOp::Min => x.min(*y),
            AggregateOp::Max => x.max(*y),
        }),
        (Value::I64(x), Value::I64(y)) => match op {
            AggregateOp::Sum | AggregateOp::Count => x
                .checked_add(*y)
                .map_or_else(|| Value::error("integer overflow"), Value::I64),
            AggregateOp::Min => Value::I64(*x.min(y)),
            AggregateOp::Max => Value::I64(*x.max(y)),
        },
        (a, b) => Value::error(format!("internal type error: {op} of {a} and {b}")),
    }
}

/// Room in the tree for `members` members.
fn capacity(members: usize) -> usize {
    members.max(1).next_power_of_two()
}

/// One aggregate's members and its tree of partial results.
#[derive(Debug)]
pub(crate) struct Aggregate {
    pub(crate) op: AggregateOp,
    /// The type of the members' values.
    pub(crate) value_type: Type,
    /// Each member's value node and presence node.
    pub(crate) members: Vec<(u32, u32)>,
    /// From a node to the members it is part of.
    by_node: HashMap<u32, Vec<u32>>,
    /// `tree[capacity + i]` is member i's contribution; `tree[k]` combines
    /// `tree[2k]` and `tree[2k + 1]`; `tree[1]` is the result.
    tree: Vec<Value>,
    /// Nodes that changed since the last recalculation.
    pub(crate) changed: Vec<u32>,
}

impl Aggregate {
    pub(crate) fn new(op: AggregateOp, value_type: Type) -> Self {
        Aggregate {
            op,
            value_type,
            members: Vec::new(),
            by_node: HashMap::new(),
            tree: vec![identity(op, value_type); 2],
            changed: Vec::new(),
        }
    }

    fn capacity(&self) -> usize {
        self.tree.len() / 2
    }

    /// Whether `node` is already part of a member (so it already has a link
    /// to this aggregate).
    pub(crate) fn has_node(&self, node: u32) -> bool {
        self.by_node.contains_key(&node)
    }

    /// Adds a member, to be counted at the next recalculation.
    pub(crate) fn push(&mut self, value: u32, present: u32) {
        let index = self.members.len() as u32;
        self.members.push((value, present));
        self.by_node.entry(value).or_default().push(index);
        if present != value {
            self.by_node.entry(present).or_default().push(index);
        }
        if self.members.len() > self.capacity() {
            self.grow();
        }
        self.changed.push(value);
    }

    /// Doubles the room in the tree. The new right half holds only the
    /// identity, so the result does not change.
    fn grow(&mut self) {
        let old = self.capacity();
        let new = old * 2;
        let id = identity(self.op, self.value_type);
        let mut tree = vec![id; 2 * new];
        tree[new..new + old].clone_from_slice(&self.tree[old..2 * old]);
        for k in (1..new).rev() {
            tree[k] = combine(self.op, &tree[2 * k], &tree[2 * k + 1]);
        }
        self.tree = tree;
    }

    fn leaf(&self, index: usize, values: &[Value]) -> Value {
        let (value, present) = self.members[index];
        leaf(
            self.op,
            self.value_type,
            &values[value as usize],
            &values[present as usize],
        )
    }

    /// Brings the result up to date with the members that changed, updating
    /// only their paths to the root.
    pub(crate) fn recalculate(&mut self, values: &[Value]) -> Value {
        let mut indexes: Vec<usize> = Vec::new();
        for node in std::mem::take(&mut self.changed) {
            if let Some(found) = self.by_node.get(&node) {
                indexes.extend(found.iter().map(|&i| i as usize));
            }
        }
        indexes.sort_unstable();
        indexes.dedup();
        let capacity = self.capacity();
        for &i in &indexes {
            self.tree[capacity + i] = self.leaf(i, values);
        }
        for &i in &indexes {
            let mut k = (capacity + i) / 2;
            while k >= 1 {
                self.tree[k] = combine(self.op, &self.tree[2 * k], &self.tree[2 * k + 1]);
                k /= 2;
            }
        }
        self.tree[1].clone()
    }

    /// Recomputes every leaf and the whole tree.
    pub(crate) fn recalculate_everything(&mut self, values: &[Value]) -> Value {
        self.changed.clear();
        let capacity = self.capacity();
        for i in 0..self.members.len() {
            self.tree[capacity + i] = self.leaf(i, values);
        }
        for k in (1..capacity).rev() {
            self.tree[k] = combine(self.op, &self.tree[2 * k], &self.tree[2 * k + 1]);
        }
        self.tree[1].clone()
    }

    /// The result calculated from scratch, without the stored tree: the
    /// simple reference that the consistency check compares against.
    pub(crate) fn folded_from_scratch(&self, values: &[Value]) -> Value {
        fn fold(a: &Aggregate, values: &[Value], lo: usize, hi: usize) -> Value {
            if hi - lo == 1 {
                return match a.members.get(lo) {
                    Some(_) => a.leaf(lo, values),
                    None => identity(a.op, a.value_type),
                };
            }
            let mid = (lo + hi) / 2;
            combine(a.op, &fold(a, values, lo, mid), &fold(a, values, mid, hi))
        }
        fold(self, values, 0, capacity(self.members.len()))
    }
}
