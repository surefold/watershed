//! Derivatives of one node with respect to others, as more nodes.
//!
//! [`Graph::gradient`] adds a node for each derivative. They are ordinary
//! nodes: they update with the graph, can be explained, and run in a
//! scenario plan. They are worked out backwards from the node being
//! differentiated (reverse mode), so the whole gradient costs a small
//! multiple of the model itself, however many inputs it is taken in.
//!
//! Each node between the inputs and the output gets an *adjoint*: the
//! derivative of the output with respect to it, the sum of one term for each
//! node that uses it. A term is the user's adjoint times the derivative of
//! the user's own formula, which is found from the formula's expression.

use std::collections::hash_map::Entry;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;

use crate::aggregate::AggregateOp;
use crate::error::CalcError;
use crate::graph::Graph;
use crate::handle::NodeId;
use crate::ir::{BinOp, Ir, UnOp};
use crate::value::{Type, Value};

/// A formula of more operations than this, of more than one node, is
/// differentiated in steps.
const IN_STEPS_ABOVE: usize = 12;

/// A sum of more terms than this is a total, so that a change reaches it
/// through the one term that changed; fewer are added in one formula.
const ADD_IN_ONE: usize = 4;

impl Graph {
    /// Adds nodes holding the derivative of `of` with respect to each of
    /// `with_respect_to`, and returns them in that order.
    ///
    /// `of` and `with_respect_to` are `f64` nodes. Each of `with_respect_to`
    /// is taken as free to vary on its own, whether it is an input or not: a
    /// calculated node's derivative is what moving that node alone would do,
    /// and nothing is followed through it to the nodes it uses. A node that
    /// `of` does not depend on has a derivative of zero.
    ///
    /// The new nodes are named `name` followed by the node's own name in
    /// square brackets (`"slope[price]"`); the nodes made on the way start
    /// with `name` and a `~`.
    ///
    /// Where a formula has a corner the derivative is taken on one side of
    /// it: `abs` at zero as for a positive number, `min` and `max` of equal
    /// values as for the first, and a condition as for the branch it takes.
    /// A total's smallest or largest value passes its derivative to every
    /// member equal to it, so members that tie are each counted. A condition,
    /// a comparison and a count have a derivative of zero.
    ///
    /// Fails, naming the node, if a whole number or a decimal lies between
    /// the two: nothing is added in that case.
    pub fn gradient(
        &mut self,
        of: NodeId,
        with_respect_to: &[NodeId],
        name: &str,
    ) -> Result<Vec<NodeId>, CalcError> {
        for &id in with_respect_to.iter().chain([&of]) {
            if id.index() >= self.len() || self.is_removed(id) {
                return Err(CalcError::UnknownNode(id));
            }
            if self.node_type(id) != Type::F64 {
                return Err(CalcError::Invalid(format!(
                    "{name:?}: a derivative is of and by f64 nodes, and {:?} is {}",
                    self.name(id),
                    self.node_type(id)
                )));
            }
        }
        let mut between = self.between(of, with_respect_to, name)?;

        // The terms of each node's adjoint, gathered as its users are
        // reached, and the nodes that have some, highest first: a node's
        // users are all above it, so its terms are complete when it comes up.
        let mut terms: HashMap<NodeId, Vec<Arc<Ir>>> = HashMap::new();
        let mut waiting = BinaryHeap::new();
        terms.insert(of, vec![number(1.0)]);
        waiting.push((self.height(of), of));
        let mut adjoints: HashMap<NodeId, Arc<Ir>> = HashMap::new();
        let free: HashSet<NodeId> = with_respect_to.iter().copied().collect();
        let mut present = None;

        while let Some((_, node)) = waiting.pop() {
            let Some(parts) = terms.remove(&node) else {
                continue;
            };
            let label = if free.contains(&node) {
                format!("{name}[{}]", self.name(node))
            } else {
                format!("{name}~{}", self.name(node))
            };
            let adjoint = self.sum_of(&label, parts, name, &mut present)?;
            if free.contains(&node) {
                adjoints.insert(node, adjoint);
                continue;
            }
            let mut pass = |to: NodeId, term: Arc<Ir>, g: &Graph| {
                terms.entry(to).or_default().push(term);
                waiting.push((g.height(to), to));
            };
            if let Some((op, members)) = self.aggregate_members(node) {
                for (value, there) in members {
                    if !between.contains(&value) {
                        continue;
                    }
                    let there = Arc::new(self.reference(there));
                    let passes = match op {
                        AggregateOp::Sum => there,
                        AggregateOp::Min | AggregateOp::Max => binary(
                            BinOp::And,
                            there,
                            binary(
                                BinOp::Eq,
                                Arc::new(self.reference(value)),
                                Arc::new(self.reference(node)),
                            ),
                        ),
                        AggregateOp::Count => continue,
                    };
                    let term = Arc::new(Ir::If(passes, adjoint.clone(), number(0.0)));
                    pass(value, term, self);
                }
                continue;
            }
            let Some(formula) = self.expression(node).map(Arc::new) else {
                continue;
            };
            let used: Vec<NodeId> = self
                .dependencies(node)
                .into_iter()
                .filter(|used| between.contains(used))
                .collect();
            if used.len() > 1 && size(&formula) > IN_STEPS_ABOVE {
                // A long formula of several nodes: its derivative by each
                // would work most of it out again. Its steps are made nodes
                // of their own, the last of them equal to this node, and the
                // derivative passes down through them, each step's work
                // shared by all that follow.
                let mut steps = Steps {
                    label: format!("{label}~step"),
                    made: HashMap::new(),
                };
                let last = self.in_steps(&formula, &mut between, &mut steps)?;
                if let Ir::Ref(r) = &*last {
                    pass(r.node, adjoint, self);
                    continue;
                }
            }
            for used in used {
                let Some(slope) = slope(&formula, used) else {
                    continue;
                };
                // A slope that can be infinite (a division by nothing, on a
                // branch not taken) would make nothing times infinity, which
                // is not a number. Where nothing comes down, nothing is
                // passed on.
                let term = match unbounded(&slope) {
                    true => Arc::new(Ir::If(
                        binary(BinOp::Eq, adjoint.clone(), number(0.0)),
                        number(0.0),
                        times(adjoint.clone(), slope),
                    )),
                    false => times(adjoint.clone(), slope),
                };
                pass(used, term, self);
            }
        }

        let mut gradient = Vec::with_capacity(with_respect_to.len());
        let mut made: HashMap<NodeId, NodeId> = HashMap::new();
        for &id in with_respect_to {
            if let Some(&node) = made.get(&id) {
                gradient.push(node);
                continue;
            }
            let label = format!("{name}[{}]", self.name(id));
            let node = match adjoints.get(&id).map(|a| a.as_ref()) {
                // A sum made above already carries the name.
                Some(Ir::Ref(r)) if self.name(r.node) == label => r.node,
                Some(other) => self.define_ir(&label, other.clone())?,
                None => self.define_ir(&label, Ir::Const(Value::F64(0.0)))?,
            };
            made.insert(id, node);
            gradient.push(node);
        }
        Ok(gradient)
    }

    /// The nodes `of` depends on that depend on one of `free`, with `of` and
    /// those of `free` themselves: the nodes a derivative passes through.
    fn between(&self, of: NodeId, free: &[NodeId], name: &str) -> Result<Between, CalcError> {
        // Whether each node `of` depends on reaches a free node: found on
        // the way back up a walk down from `of`, which stops at a free node.
        let mut reaches: HashMap<NodeId, bool> = free.iter().map(|&id| (id, true)).collect();
        let mut walk = vec![(of, false)];
        while let Some((node, done)) = walk.pop() {
            if done {
                let any = self
                    .dependencies(node)
                    .iter()
                    .any(|used| reaches.get(used).copied().unwrap_or(false));
                reaches.insert(node, any);
            } else if let Entry::Vacant(unseen) = reaches.entry(node) {
                // Marked so that a node met again while its own walk is
                // under way is not walked twice.
                unseen.insert(false);
                walk.push((node, true));
                for used in self.dependencies(node) {
                    if !reaches.contains_key(&used) {
                        walk.push((used, false));
                    }
                }
            }
        }
        let mut order: Vec<NodeId> = reaches
            .iter()
            .filter(|&(_, &reaches)| reaches)
            .map(|(&id, _)| id)
            .collect();
        // A derivative passes through numbers only. A yes-or-no on the way
        // (a condition's test) stops it; a whole number or a decimal would
        // carry it, and cannot.
        order.retain(|&id| self.node_type(id) != Type::Bool);
        for &id in &order {
            if self.node_type(id) != Type::F64 {
                return Err(CalcError::Invalid(format!(
                    "{name:?}: no derivative passes through {:?}, which is {}",
                    self.name(id),
                    self.node_type(id)
                )));
            }
        }
        Ok(order.into_iter().collect())
    }

    /// Makes a node of each step of `formula` that works on a node in
    /// `between`, and returns the formula's value as the last of them. A
    /// part that works on none of them is left whole inside the step that
    /// uses it, and so is a test: no derivative passes through either. A
    /// step that occurs twice is made once.
    fn in_steps(
        &mut self,
        formula: &Arc<Ir>,
        between: &mut Between,
        steps: &mut Steps,
    ) -> Result<Arc<Ir>, CalcError> {
        let step = match &**formula {
            Ir::Const(_) | Ir::Ref(_) | Ir::List(_) => return Ok(formula.clone()),
            _ if !a_number(formula) || !touches(formula, between) => return Ok(formula.clone()),
            Ir::Unary(op, a) => Ir::Unary(*op, self.in_steps(a, between, steps)?),
            Ir::Binary(op, a, b) => Ir::Binary(
                *op,
                self.in_steps(a, between, steps)?,
                self.in_steps(b, between, steps)?,
            ),
            Ir::If(test, then, otherwise) => Ir::If(
                test.clone(),
                self.in_steps(then, between, steps)?,
                self.in_steps(otherwise, between, steps)?,
            ),
            Ir::Choose(index, options) => {
                let options = options
                    .iter()
                    .map(|o| self.in_steps(o, between, steps))
                    .collect::<Result<_, _>>()?;
                Ir::Choose(index.clone(), options)
            }
        };
        let written = step.render(&|id| format!("#{id}"));
        if let Some(&made) = steps.made.get(&written) {
            return Ok(Arc::new(self.reference(made)));
        }
        let made = self.define_ir(&format!("{}{}", steps.label, steps.made.len()), step)?;
        steps.made.insert(written, made);
        between.insert(made);
        Ok(Arc::new(self.reference(made)))
    }

    /// The sum of `parts` as one expression: itself if it is already a
    /// constant or a node, and otherwise a node called `label`.
    fn sum_of(
        &mut self,
        label: &str,
        mut parts: Vec<Arc<Ir>>,
        name: &str,
        present: &mut Option<NodeId>,
    ) -> Result<Arc<Ir>, CalcError> {
        if parts.len() == 1 && matches!(*parts[0], Ir::Const(_) | Ir::Ref(_)) {
            return Ok(parts.remove(0));
        }
        let sum = if parts.len() <= ADD_IN_ONE {
            let all = parts
                .into_iter()
                .reduce(|a, b| binary(BinOp::Add, a, b))
                .expect("a node between has a term");
            self.define_ir(label, (*all).clone())?
        } else {
            let there = match *present {
                Some(there) => there,
                None => {
                    let there =
                        self.define_ir(&format!("{name}~present"), Ir::Const(Value::Bool(true)))?;
                    *present = Some(there);
                    there
                }
            };
            let mut members = Vec::with_capacity(parts.len());
            for (k, part) in parts.into_iter().enumerate() {
                let member = match &*part {
                    Ir::Ref(r) => r.node,
                    other => self.define_ir(&format!("{label}~{k}"), other.clone())?,
                };
                members.push((member, there));
            }
            self.aggregate(label, AggregateOp::Sum, Type::F64, 0, &members)?
        };
        Ok(Arc::new(self.reference(sum)))
    }
}

/// The nodes a derivative passes through.
type Between = HashSet<NodeId>;

/// The steps of one formula that have been made nodes.
struct Steps {
    /// What their names start with.
    label: String,
    /// Each step as written, and its node.
    made: HashMap<String, NodeId>,
}

/// How many operations an expression has.
fn size(of: &Ir) -> usize {
    match of {
        Ir::Const(_) | Ir::Ref(_) => 0,
        Ir::Unary(_, a) => 1 + size(a),
        Ir::Binary(_, a, b) => 1 + size(a) + size(b),
        Ir::If(test, then, otherwise) => 1 + size(test) + size(then) + size(otherwise),
        Ir::Choose(index, options) => {
            1 + size(index) + options.iter().map(|o| size(o)).sum::<usize>()
        }
        Ir::List(items) => 1 + items.iter().map(|i| size(i)).sum::<usize>(),
    }
}

/// Whether an expression refers to a node in `between`.
fn touches(of: &Ir, between: &Between) -> bool {
    let mut refs = Vec::new();
    of.refs(&mut refs);
    refs.iter().any(|r| between.contains(&r.node))
}

/// Whether an expression inside an `f64` formula is itself a number, going
/// by the operation at its top: a test or a comparison is not.
fn a_number(of: &Ir) -> bool {
    match of {
        Ir::Unary(op, _) => !matches!(op, UnOp::Not | UnOp::ToText | UnOp::ToDecimal),
        Ir::Binary(op, _, _) => matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Min
                | BinOp::Max
                | BinOp::Pow
        ),
        Ir::Const(_) | Ir::Ref(_) | Ir::If(..) | Ir::Choose(..) => true,
        Ir::List(_) => false,
    }
}

/// Whether an expression can come to infinity from finite numbers: it
/// divides, or takes a logarithm, a root or a power.
fn unbounded(of: &Ir) -> bool {
    match of {
        Ir::Const(_) | Ir::Ref(_) | Ir::List(_) => false,
        Ir::Unary(op, a) => matches!(op, UnOp::Ln | UnOp::Sqrt) || unbounded(a),
        Ir::Binary(op, a, b) => {
            matches!(op, BinOp::Div | BinOp::Pow) || unbounded(a) || unbounded(b)
        }
        Ir::If(test, then, otherwise) => unbounded(test) || unbounded(then) || unbounded(otherwise),
        Ir::Choose(index, options) => unbounded(index) || options.iter().any(|o| unbounded(o)),
    }
}

fn number(x: f64) -> Arc<Ir> {
    Arc::new(Ir::Const(Value::F64(x)))
}

fn binary(op: BinOp, a: Arc<Ir>, b: Arc<Ir>) -> Arc<Ir> {
    Arc::new(Ir::Binary(op, a, b))
}

fn unary(op: UnOp, a: Arc<Ir>) -> Arc<Ir> {
    Arc::new(Ir::Unary(op, a))
}

fn is_one(a: &Ir) -> bool {
    matches!(a, Ir::Const(Value::F64(x)) if *x == 1.0)
}

/// `a * b`, without the multiplication when one of them is one.
fn times(a: Arc<Ir>, b: Arc<Ir>) -> Arc<Ir> {
    if is_one(&a) {
        b
    } else if is_one(&b) {
        a
    } else {
        binary(BinOp::Mul, a, b)
    }
}

fn negated(a: Arc<Ir>) -> Arc<Ir> {
    match &*a {
        Ir::Unary(UnOp::Neg, inner) => inner.clone(),
        _ => unary(UnOp::Neg, a),
    }
}

/// `a + b` where either may be nothing, which is zero.
fn plus(a: Option<Arc<Ir>>, b: Option<Arc<Ir>>) -> Option<Arc<Ir>> {
    match (a, b) {
        (Some(a), Some(b)) => Some(binary(BinOp::Add, a, b)),
        (a, b) => a.or(b),
    }
}

/// One of two derivatives picked by a test, where either may be nothing.
fn either(test: Arc<Ir>, then: Option<Arc<Ir>>, otherwise: Option<Arc<Ir>>) -> Option<Arc<Ir>> {
    if then.is_none() && otherwise.is_none() {
        return None;
    }
    let zero = || number(0.0);
    Some(Arc::new(Ir::If(
        test,
        then.unwrap_or_else(zero),
        otherwise.unwrap_or_else(zero),
    )))
}

/// The derivative of an expression with respect to the node `by`, taking
/// every other node it refers to as fixed. `None` is zero.
fn slope(of: &Arc<Ir>, by: NodeId) -> Option<Arc<Ir>> {
    match &**of {
        Ir::Const(_) | Ir::List(_) => None,
        Ir::Ref(r) => (r.node == by).then(|| number(1.0)),
        Ir::Unary(op, a) => {
            let da = slope(a, by)?;
            match op {
                UnOp::Neg => Some(negated(da)),
                UnOp::Abs => {
                    let positive = binary(BinOp::Ge, a.clone(), number(0.0));
                    either(positive, Some(da.clone()), Some(negated(da)))
                }
                UnOp::Exp => Some(times(da, of.clone())),
                UnOp::Ln => Some(binary(BinOp::Div, da, a.clone())),
                UnOp::Sqrt => Some(binary(
                    BinOp::Div,
                    da,
                    binary(BinOp::Mul, number(2.0), of.clone()),
                )),
                UnOp::Sin => Some(times(da, unary(UnOp::Cos, a.clone()))),
                UnOp::Cos => Some(negated(times(da, unary(UnOp::Sin, a.clone())))),
                // Flat between its steps.
                UnOp::Floor => None,
                // Not numbers, or not `f64`: `between` has refused a model
                // whose derivative would have to pass through these.
                UnOp::Not | UnOp::ToText | UnOp::ToDecimal | UnOp::ToFloat => None,
            }
        }
        Ir::Binary(op, a, b) => {
            let (da, db) = (slope(a, by), slope(b, by));
            match op {
                BinOp::Add => plus(da, db),
                BinOp::Sub => plus(da, db.map(negated)),
                BinOp::Mul => plus(
                    da.map(|da| times(da, b.clone())),
                    db.map(|db| times(a.clone(), db)),
                ),
                // (a / b)' = a' / b - (a / b) b' / b
                BinOp::Div => plus(
                    da.map(|da| binary(BinOp::Div, da, b.clone())),
                    db.map(|db| {
                        negated(binary(
                            BinOp::Div,
                            binary(BinOp::Mul, of.clone(), db),
                            b.clone(),
                        ))
                    }),
                ),
                BinOp::Min => either(binary(BinOp::Le, a.clone(), b.clone()), da, db),
                BinOp::Max => either(binary(BinOp::Ge, a.clone(), b.clone()), da, db),
                // (a ^ b)' = b a ^ (b - 1) a' + a ^ b ln(a) b'
                BinOp::Pow => plus(
                    da.map(|da| {
                        let lower = binary(BinOp::Sub, b.clone(), number(1.0));
                        let power = binary(BinOp::Pow, a.clone(), lower);
                        times(da, binary(BinOp::Mul, b.clone(), power))
                    }),
                    db.map(|db| {
                        let log = unary(UnOp::Ln, a.clone());
                        times(db, binary(BinOp::Mul, of.clone(), log))
                    }),
                ),
                BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::And
                | BinOp::Or
                | BinOp::Concat
                | BinOp::Join
                | BinOp::Round(_) => None,
            }
        }
        Ir::If(test, then, otherwise) => {
            either(test.clone(), slope(then, by), slope(otherwise, by))
        }
        Ir::Choose(index, options) => {
            let slopes: Vec<Option<Arc<Ir>>> = options.iter().map(|o| slope(o, by)).collect();
            if slopes.iter().all(Option::is_none) {
                return None;
            }
            let slopes = slopes
                .into_iter()
                .map(|s| s.unwrap_or_else(|| number(0.0)))
                .collect();
            Some(Arc::new(Ir::Choose(index.clone(), slopes)))
        }
    }
}
