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
use std::collections::hash_map::Entry;
use std::fmt;
use std::hash::{BuildHasher, Hasher};

use crate::decimal::{self, Decimal};
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
        (AggregateOp::Sum, Type::Decimal) => decimal::value(Decimal::ZERO),
        (AggregateOp::Min, Type::Decimal) => decimal::value(Decimal::MAX),
        (AggregateOp::Max, Type::Decimal) => decimal::value(Decimal::MIN),
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

/// Combines two float partial results, as `combine` does for two `F64`s.
fn combine_f64(op: AggregateOp, x: f64, y: f64) -> f64 {
    if x.is_nan() || y.is_nan() {
        return if x.is_nan() { x } else { y };
    }
    match op {
        AggregateOp::Sum | AggregateOp::Count => x + y,
        AggregateOp::Min => x.min(y),
        AggregateOp::Max => x.max(y),
    }
}

/// Combines two partial results. An error wins, the left one first.
fn combine(op: AggregateOp, a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Error(e), _) | (_, Value::Error(e)) => Value::Error(e.clone()),
        // NaN wins, as it does in a sum: Rust's `f64::min` and `f64::max`
        // ignore NaN, which would make the result depend on the tree's shape
        // (NaN against empty room gives the padding, alone it gives NaN).
        (Value::F64(x), Value::F64(y)) => Value::F64(combine_f64(op, *x, *y)),
        (Value::I64(x), Value::I64(y)) => match op {
            AggregateOp::Sum | AggregateOp::Count => x
                .checked_add(*y)
                .map_or_else(|| Value::error("integer overflow"), Value::I64),
            AggregateOp::Min => Value::I64(*x.min(y)),
            AggregateOp::Max => Value::I64(*x.max(y)),
        },
        (Value::Decimal(x), Value::Decimal(y)) => match op {
            AggregateOp::Sum | AggregateOp::Count => decimal::add(x, y),
            AggregateOp::Min => decimal::min(x, y),
            AggregateOp::Max => decimal::max(x, y),
        },
        (a, b) => Value::error(format!("internal type error: {op} of {a} and {b}")),
    }
}

/// Room in the tree for `members` members.
fn capacity(members: usize) -> usize {
    members.max(1).next_power_of_two()
}

/// The partial results. A float sum, minimum or maximum keeps them as plain
/// `f64`s, half the room of a `Value` each; the first value that is not a
/// float (an error) turns the tree into `Value`s for good, so that it can
/// hold it.
#[derive(Debug)]
enum Tree {
    Floats(Vec<f64>),
    Values(Vec<Value>),
}

impl Tree {
    /// A tree of `len` slots, each holding `fill`.
    fn new(op: AggregateOp, value_type: Type, len: usize, fill: Value) -> Tree {
        match fill {
            Value::F64(x) if value_type == Type::F64 && op != AggregateOp::Count => {
                Tree::Floats(vec![x; len])
            }
            fill => Tree::Values(vec![fill; len]),
        }
    }

    fn len(&self) -> usize {
        match self {
            Tree::Floats(t) => t.len(),
            Tree::Values(t) => t.len(),
        }
    }

    fn get(&self, k: usize) -> Value {
        match self {
            Tree::Floats(t) => Value::F64(t[k]),
            Tree::Values(t) => t[k].clone(),
        }
    }

    fn set(&mut self, k: usize, value: Value) {
        match (&mut *self, value) {
            (Tree::Floats(t), Value::F64(x)) => t[k] = x,
            (Tree::Values(t), value) => t[k] = value,
            (Tree::Floats(t), value) => {
                let mut values: Vec<Value> = t.iter().map(|&x| Value::F64(x)).collect();
                values[k] = value;
                *self = Tree::Values(values);
            }
        }
    }

    /// Works out slot `k` from its two halves.
    fn recombine(&mut self, op: AggregateOp, k: usize) {
        match self {
            Tree::Floats(t) => t[k] = combine_f64(op, t[2 * k], t[2 * k + 1]),
            Tree::Values(t) => t[k] = combine(op, &t[2 * k], &t[2 * k + 1]),
        }
    }
}

/// From a node to the member it is part of: the member's index, or, with
/// this bit set, an index into `Aggregate::many`.
const MANY: u32 = 1 << 31;

/// One aggregate's members and its tree of partial results.
#[derive(Debug)]
pub(crate) struct Aggregate {
    pub(crate) op: AggregateOp,
    /// The type of the members' values.
    pub(crate) value_type: Type,
    /// Each member's value node and presence node.
    pub(crate) members: Vec<(u32, u32)>,
    /// From a node to the member it is part of (see `MANY`). Nearly every
    /// node is part of one, so this holds a number, not a list: eight bytes
    /// an entry, where a million members are two million entries.
    by_node: HashMap<u32, u32, ById>,
    /// The members of each node that is part of more than one.
    many: Vec<Vec<u32>>,
    /// `tree[capacity + i]` is member i's contribution; `tree[k]` combines
    /// `tree[2k]` and `tree[2k + 1]`; `tree[1]` is the result.
    tree: Tree,
    /// Nodes that changed since the last recalculation.
    pub(crate) changed: Vec<u32>,
    /// When members joined: for each update that added some, the update
    /// and how many members there were before it. Members are only ever
    /// added at the end, so this says which members there were at any
    /// earlier update, at the cost of an entry per update that added any.
    joined: Vec<(u64, u32)>,
}

/// Hashes node ids for `by_node`. Ids are small numbers handed out in
/// order, not keys chosen by anyone outside, so one multiplication spreads
/// them well, at a fraction of the default hasher's cost.
#[derive(Debug, Default, Clone, Copy)]
struct ById;

impl BuildHasher for ById {
    type Hasher = IdHasher;
    fn build_hasher(&self) -> IdHasher {
        IdHasher(0)
    }
}

struct IdHasher(u64);

impl Hasher for IdHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.write_u32(u32::from(byte));
        }
    }
    fn write_u32(&mut self, id: u32) {
        let mixed = (self.0 ^ u64::from(id)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = mixed ^ (mixed >> 32);
    }
}

impl Aggregate {
    pub(crate) fn new(op: AggregateOp, value_type: Type) -> Self {
        Aggregate {
            op,
            value_type,
            members: Vec::new(),
            by_node: HashMap::default(),
            many: Vec::new(),
            tree: Tree::new(op, value_type, 2, identity(op, value_type)),
            changed: Vec::new(),
            joined: Vec::new(),
        }
    }

    /// Notes that members pushed from now on count from update `epoch`.
    pub(crate) fn joining_from(&mut self, epoch: u64) {
        if self.joined.last().is_none_or(|&(last, _)| last < epoch) {
            self.joined.push((epoch, self.members.len() as u32));
        }
    }

    /// The members there were at the end of update `epoch`.
    pub(crate) fn members_at(&self, epoch: u64) -> &[(u32, u32)] {
        let later = self.joined.partition_point(|&(from, _)| from <= epoch);
        match self.joined.get(later) {
            Some(&(_, before)) => &self.members[..before as usize],
            None => &self.members,
        }
    }

    fn capacity(&self) -> usize {
        self.tree.len() / 2
    }

    /// Adds a member, to be counted at the next recalculation. Returns, for
    /// its value node and its presence node, whether that node is new to
    /// this aggregate (and so needs a link to it). When they are one node,
    /// only the first answer can be true.
    pub(crate) fn push(&mut self, value: u32, present: u32) -> (bool, bool) {
        let index = self.members.len() as u32;
        self.members.push((value, present));
        let new = (
            self.note(value, index),
            present != value && self.note(present, index),
        );
        if self.members.len() > self.capacity() {
            self.grow();
        }
        self.changed.push(value);
        new
    }

    /// Makes room for `more` members, growing the tree once rather than
    /// doubling it as they arrive.
    pub(crate) fn reserve(&mut self, more: usize) {
        let wanted = self.members.len() + more;
        self.members.reserve(more);
        // A member's value node and its presence node.
        self.by_node.reserve(2 * more);
        while self.capacity() < wanted {
            self.grow();
        }
    }

    /// Records that `node` is part of member `index`. Returns whether the
    /// node was not part of any member before.
    fn note(&mut self, node: u32, index: u32) -> bool {
        assert!(index < MANY, "an aggregate holds fewer than 2^31 members");
        match self.by_node.entry(node) {
            Entry::Vacant(slot) => {
                slot.insert(index);
                true
            }
            Entry::Occupied(mut slot) => {
                let held = *slot.get();
                if held & MANY != 0 {
                    self.many[(held & !MANY) as usize].push(index);
                } else {
                    slot.insert(MANY | self.many.len() as u32);
                    self.many.push(vec![held, index]);
                }
                false
            }
        }
    }

    /// Doubles the room in the tree. The new right half holds only the
    /// identity, so the result does not change.
    fn grow(&mut self) {
        let old = self.capacity();
        let new = old * 2;
        let id = identity(self.op, self.value_type);
        let mut tree = match &self.tree {
            Tree::Floats(_) => Tree::new(self.op, self.value_type, 2 * new, id),
            Tree::Values(_) => Tree::Values(vec![id; 2 * new]),
        };
        for i in 0..old {
            tree.set(new + i, self.tree.get(old + i));
        }
        for k in (1..new).rev() {
            tree.recombine(self.op, k);
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
            match self.by_node.get(&node) {
                Some(&held) if held & MANY != 0 => {
                    let list = &self.many[(held & !MANY) as usize];
                    indexes.extend(list.iter().map(|&i| i as usize));
                }
                Some(&i) => indexes.push(i as usize),
                None => {}
            }
        }
        indexes.sort_unstable();
        indexes.dedup();
        let capacity = self.capacity();
        for &i in &indexes {
            let leaf = self.leaf(i, values);
            self.tree.set(capacity + i, leaf);
        }
        // Each changed member has `steps` partial results above it. When so
        // many members changed that those paths add up to more than the
        // tree holds (a table being filled), working out every partial
        // result once is less work, and gives the same tree.
        let steps = capacity.ilog2().max(1) as usize;
        if indexes.len().saturating_mul(steps) >= capacity {
            for k in (1..capacity).rev() {
                self.tree.recombine(self.op, k);
            }
        } else {
            for &i in &indexes {
                let mut k = (capacity + i) / 2;
                while k >= 1 {
                    self.tree.recombine(self.op, k);
                    k /= 2;
                }
            }
        }
        self.tree.get(1)
    }

    /// Recomputes every leaf and the whole tree.
    pub(crate) fn recalculate_everything(&mut self, values: &[Value]) -> Value {
        self.changed.clear();
        let capacity = self.capacity();
        for i in 0..self.members.len() {
            let leaf = self.leaf(i, values);
            self.tree.set(capacity + i, leaf);
        }
        for k in (1..capacity).rev() {
            self.tree.recombine(self.op, k);
        }
        self.tree.get(1)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every partial result is its two halves combined.
    fn tree_is_whole(a: &Aggregate) {
        for k in 1..a.capacity() {
            let both = combine(a.op, &a.tree.get(2 * k), &a.tree.get(2 * k + 1));
            assert!(a.tree.get(k).same_as(&both), "partial result {k}");
        }
    }

    #[test]
    fn many_members_changing_at_once_leave_the_same_tree_as_one_at_a_time() {
        let mut values: Vec<Value> = (0..100).map(|i| Value::F64(f64::from(i))).collect();
        values.push(Value::Bool(true));
        let mut a = Aggregate::new(AggregateOp::Sum, Type::F64);
        for i in 0..100 {
            a.push(i, 100);
        }
        // All of them are new, which is far more than the tree's depth.
        assert!(a.recalculate(&values).same_as(&Value::F64(4950.0)));
        tree_is_whole(&a);

        // One member changing takes the path from it to the top.
        values[7] = Value::F64(1007.0);
        a.changed.push(7);
        assert!(a.recalculate(&values).same_as(&Value::F64(5950.0)));
        tree_is_whole(&a);
        assert!(a.folded_from_scratch(&values).same_as(&Value::F64(5950.0)));

        // The presence node is part of every member.
        values[100] = Value::Bool(false);
        a.changed.push(100);
        let none = a.recalculate(&values);
        assert!(none.same_as(&a.folded_from_scratch(&values)));
        tree_is_whole(&a);
    }

    #[test]
    fn a_float_total_keeps_plain_floats_until_an_error_arrives_and_still_wins() {
        let mut values: Vec<Value> = (0..10).map(|i| Value::F64(f64::from(i))).collect();
        values.push(Value::Bool(true));
        let mut a = Aggregate::new(AggregateOp::Max, Type::F64);
        for i in 0..10 {
            a.push(i, 10);
        }
        assert!(a.recalculate(&values).same_as(&Value::F64(9.0)));
        assert!(
            matches!(a.tree, Tree::Floats(_)),
            "a float maximum holds floats"
        );

        // An error in one member is the result, as before.
        values[3] = Value::error("bad price");
        a.changed.push(3);
        let wrong = a.recalculate(&values);
        assert!(matches!(wrong, Value::Error(_)), "{wrong}");
        assert!(
            matches!(a.tree, Tree::Values(_)),
            "the tree now holds values"
        );
        tree_is_whole(&a);

        // Mended, the result is a number again, the same as from scratch.
        values[3] = Value::F64(30.0);
        a.changed.push(3);
        assert!(a.recalculate(&values).same_as(&Value::F64(30.0)));
        assert!(a.folded_from_scratch(&values).same_as(&Value::F64(30.0)));
        tree_is_whole(&a);
    }

    #[test]
    fn a_count_and_an_integer_sum_keep_values() {
        assert!(matches!(
            Aggregate::new(AggregateOp::Count, Type::F64).tree,
            Tree::Values(_)
        ));
        assert!(matches!(
            Aggregate::new(AggregateOp::Sum, Type::I64).tree,
            Tree::Values(_)
        ));
        assert!(matches!(
            Aggregate::new(AggregateOp::Sum, Type::F64).tree,
            Tree::Floats(_)
        ));
    }
}
