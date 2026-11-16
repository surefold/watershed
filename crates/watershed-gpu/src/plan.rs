//! Works out, once, what has to be calculated to run a graph for many sets of
//! inputs, and runs that on the CPU.
//!
//! A node the outputs depend on is one of three kinds:
//!
//! - it depends on a varied input, so it is worked out for every scenario;
//! - it does not, but depends on a shared input, so it is worked out once
//!   each time the shared inputs are given new values;
//! - neither, so it has the same value always and becomes a constant.
//!
//! The nodes worked out for every scenario are put in levels: a node's level
//! is one more than the highest level it reads, so a whole level can be
//! worked out at once.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::fmt;
use std::ops::Range;

use rayon::prelude::*;
use watershed_core::{AggregateOp, BinOp, Graph, Ir, NodeId, Type, UnOp, Value};

use crate::fuse::{Fused, fuse};
mod check;
mod file;
pub use file::LoadError;

// The instruction codes. `kernel.cu` has the same numbers; the two must be
// changed together. An instruction is one `u32`: the code in the low five
// bits, and a slot, shared value or constant number in the rest. A bool is
// held as 1.0 or 0.0.
pub(crate) const CODE_BITS: u32 = 5;
pub(crate) const CODE: u32 = (1 << CODE_BITS) - 1;
pub(crate) const LOAD: u32 = 0;
pub(crate) const CONST: u32 = 1;
const ADD: u32 = 2;
const SUB: u32 = 3;
const MUL: u32 = 4;
const DIV: u32 = 5;
const MIN: u32 = 6;
const MAX: u32 = 7;
const NEG: u32 = 8;
const ABS: u32 = 9;
pub(crate) const LOAD_SHARED: u32 = 10;
const LT: u32 = 11;
const LE: u32 = 12;
const GT: u32 = 13;
const GE: u32 = 14;
const EQ: u32 = 15;
const NE: u32 = 16;
const AND: u32 = 17;
const OR: u32 = 18;
const NOT: u32 = 19;
/// Pops otherwise, then, condition; pushes the one the condition picks.
const SELECT: u32 = 20;
const EXP: u32 = 21;
const LN: u32 = 22;
const SQRT: u32 = 23;
const POW: u32 = 24;
// 25 is `fuse::LOCAL`.
// A total's two halves combined as the graph's totals combine them: a NaN
// on the left wins, then one on the right; otherwise added, or the smaller
// or larger. (`MIN` and `MAX` let a number win over a NaN, as `f64::min`
// does; a total does not, so that its result does not depend on its tree.)
const AGG_SUM: u32 = 26;
const AGG_MIN: u32 = 27;
const AGG_MAX: u32 = 28;

/// Where a load reads from.
#[cfg(feature = "gpu")]
pub(crate) enum Source {
    Slot,
    Const,
    Shared,
}

/// How many values an instruction takes off the stack.
pub(crate) fn arity(code: u32) -> usize {
    match code {
        LOAD | CONST | LOAD_SHARED | crate::fuse::LOCAL => 0,
        NEG | ABS | NOT | EXP | LN | SQRT => 1,
        SELECT => 3,
        _ => 2,
    }
}

/// Where a load reads from.
#[cfg(feature = "gpu")]
pub(crate) fn source_of(code: u32) -> Source {
    match code {
        LOAD => Source::Slot,
        CONST => Source::Const,
        _ => Source::Shared,
    }
}

/// An instruction that is not a load, written in C on the values named. It
/// must give what `eval` gives and what `kernel.cu` gives for the same code.
#[cfg(feature = "gpu")]
pub(crate) fn in_c(code: u32, on: &[&str]) -> String {
    let truth = |test: String| format!("({test}) ? 1.0 : 0.0");
    let a = on[0];
    let b = on.get(1).copied().unwrap_or("");
    match code {
        NEG => format!("-{a}"),
        ABS => format!("fabs({a})"),
        EXP => format!("exp({a})"),
        LN => format!("log({a})"),
        SQRT => format!("sqrt({a})"),
        NOT => truth(format!("{a} == 0.0")),
        // Condition, then, otherwise.
        SELECT => format!("({a} != 0.0) ? {b} : {}", on[2]),
        ADD => format!("{a} + {b}"),
        SUB => format!("{a} - {b}"),
        MUL => format!("{a} * {b}"),
        DIV => format!("{a} / {b}"),
        POW => format!("pow({a}, {b})"),
        // As Rust's `f64::min` and `f64::max` come out on x86-64: a NaN
        // gives the other side, and of two equal values the second.
        MIN => format!("({b} != {b}) ? {a} : (({a} < {b}) ? {a} : {b})"),
        MAX => format!("({b} != {b}) ? {a} : (({a} > {b}) ? {a} : {b})"),
        AGG_SUM => format!("({a} != {a}) ? {a} : (({b} != {b}) ? {b} : {a} + {b})"),
        AGG_MIN => format!("({a} != {a}) ? {a} : (({b} != {b}) ? {b} : (({a} < {b}) ? {a} : {b}))"),
        AGG_MAX => format!("({a} != {a}) ? {a} : (({b} != {b}) ? {b} : (({a} > {b}) ? {a} : {b}))"),
        LT => truth(format!("{a} < {b}")),
        LE => truth(format!("{a} <= {b}")),
        GT => truth(format!("{a} > {b}")),
        GE => truth(format!("{a} >= {b}")),
        EQ => truth(format!("{a} == {b}")),
        NE => truth(format!("{a} != {b}")),
        AND => truth(format!("{a} != 0.0 && {b} != 0.0")),
        OR => truth(format!("{a} != 0.0 || {b} != 0.0")),
        _ => unreachable!("no such instruction"),
    }
}

/// The most slots, shared values or constants a plan can number: what fits
/// beside the code.
const MAX_ARG: usize = (1 << (32 - CODE_BITS)) - 1;

/// The deepest stack one node's expression may need. `kernel.cu` has the same
/// number as `DEPTH`.
pub const MAX_STACK: usize = 32;

/// Why a graph cannot be run as scenarios.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanError {
    /// There must be at least one input to vary.
    NoInputs,
    /// There must be at least one output.
    NoOutputs,
    /// No node has this id.
    UnknownNode(NodeId),
    /// A node to vary or to share is not an input.
    NotAnInput(String),
    /// The same input was listed twice, among those varied and shared.
    Repeated(String),
    /// A varied or shared input or an output is not an `f64`, or a node
    /// between them is neither an `f64` nor a bool.
    NotAFloat { node: String, ty: Type },
    /// A node that depends on a varied or shared input does something
    /// scenarios cannot yet do: a choice, a count, or work on text, whole
    /// numbers or decimals.
    Unsupported { node: String, what: String },
    /// A node that stays the same in every scenario holds an error.
    HoldsError { node: String, error: String },
    /// Pull mode is on and this node is not being calculated, so its value
    /// may be out of date.
    NotCalculated(String),
    /// One node's expression needs a deeper stack than [`MAX_STACK`]. No
    /// longer returned: such an expression is now split into nodes that
    /// each need less.
    TooDeep(String),
    /// More nodes or constants than a plan can number.
    TooLarge,
    /// What a node reads has changed in the graph since kept values were
    /// made from it (see [`Graph::reshaped`]), so what a change can reach is
    /// not known: make them again.
    Reshaped,
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanError::NoInputs => write!(f, "there are no inputs to vary"),
            PlanError::NoOutputs => write!(f, "there are no outputs to read"),
            PlanError::UnknownNode(id) => write!(f, "no node has the id {id}"),
            PlanError::NotAnInput(node) => {
                write!(
                    f,
                    "{node} is not an input, so it cannot be varied or shared"
                )
            }
            PlanError::Repeated(node) => write!(f, "{node} is listed twice"),
            PlanError::NotAFloat { node, ty } => {
                write!(f, "{node} is {ty}; scenarios only handle f64 and bool")
            }
            PlanError::Unsupported { node, what } => write!(
                f,
                "{node} depends on a varied or shared input and is {what}, which scenarios cannot yet do"
            ),
            PlanError::HoldsError { node, error } => write!(f, "{node} holds an error: {error}"),
            PlanError::NotCalculated(node) => write!(
                f,
                "{node} is not being calculated (pull mode), so its value may be out of date"
            ),
            PlanError::TooDeep(node) => {
                write!(
                    f,
                    "{node}'s expression needs a stack deeper than {MAX_STACK}"
                )
            }
            PlanError::TooLarge => write!(f, "too many nodes or constants for one plan"),
            PlanError::Reshaped => write!(
                f,
                "the graph's shape has changed since these values were kept (a node redefined or removed, or a total's members changed): keep them again"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Output {
    /// Read from this slot.
    Slot(u32),
    /// The same in every scenario: this shared value.
    Shared(u32),
    /// The same always.
    Fixed(f64),
}

/// Nodes next to each other in a level whose instructions differ only in
/// what they read: a kernel compiled for the shape works them all out.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
pub(crate) struct Group {
    /// Which of the plan's shapes.
    pub(crate) shape: u32,
    pub(crate) first: u32,
    pub(crate) count: u32,
}

/// How many scenarios are added up at a time when outputs are totalled, on
/// the GPU and on the CPU alike, so that both add in the same order.
pub(crate) const BLOCK: usize = 1024;

/// A graph prepared to run for many sets of inputs.
///
/// It is a copy: later changes to the graph do not reach it. Values of nodes
/// that depend on neither a varied nor a shared input are taken as they are
/// when the plan is made, so call [`Graph::stabilize`] first.
#[derive(Debug, Clone)]
#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
pub struct Scenarios {
    /// How many inputs are varied. They take slots `0..vary`.
    pub(crate) vary: usize,
    /// How many slots a scenario needs. A calculated node's slot is used
    /// again once the last level that reads the node has been worked out.
    slots: usize,
    /// Per calculated node: where its instructions start in `ops`, how many
    /// there are, and the slot its value goes in.
    pub(crate) programs: Vec<u32>,
    pub(crate) ops: Vec<u32>,
    pub(crate) consts: Vec<f64>,
    /// The calculated nodes of each level, lowest first.
    pub(crate) levels: Vec<Range<u32>>,
    /// The instruction codes of each shape of node, without what they read.
    pub(crate) shapes: Vec<Vec<u32>>,
    /// The nodes again as runs of one shape, in the order they are worked
    /// out: each level's groups come before the next level's.
    pub(crate) groups: Vec<Group>,
    /// What every node reads (a slot, a constant or a shared value for each
    /// of its loads, in order), one node after another.
    pub(crate) operands: Vec<u32>,
    /// Per calculated node: where its operands start.
    pub(crate) operand_at: Vec<u32>,
    /// The nodes again as clusters a GPU thread works out in one go (see
    /// the `fuse` module), and where the outputs are when it is run so.
    pub(crate) fused: Option<Fused>,
    pub(crate) fused_outputs: Vec<Output>,
    pub(crate) outputs: Vec<Output>,
    /// How many inputs are shared. Their values are the first of `shared`.
    shared_inputs: usize,
    /// The shared inputs' values, then those of the nodes that depend only
    /// on them, each after the ones it reads.
    pub(crate) shared: Vec<f64>,
    /// The instructions of the nodes that depend only on shared inputs:
    /// per node, a start in `shared_ops` and a length.
    shared_programs: Vec<u32>,
    shared_ops: Vec<u32>,
}

/// Where a node's value is, when it is not the same always.
#[derive(Clone, Copy)]
enum Place {
    /// Worked out for every scenario. A calculated node's slot is
    /// provisional until the levels are sorted. [`INLINE`] for a node worked
    /// out inside the totals that read it.
    Slot(u32),
    /// Worked out once for all scenarios.
    Shared(u32),
}

impl Place {
    /// The instruction that reads a value from here.
    fn load(self) -> u32 {
        match self {
            Place::Slot(slot) => LOAD | slot << CODE_BITS,
            Place::Shared(at) => LOAD_SHARED | at << CODE_BITS,
        }
    }
}

/// A node worked out for every scenario, before the nodes are put in level
/// order.
struct Draft {
    level: u32,
    ops: Range<usize>,
}

/// What an expression leaves on the stack. A whole number (a count, or
/// what is made from one by comparing or choosing) is held as the `f64` of
/// the same value, which is exact up to 2^53.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Float,
    Bool,
    Int,
}

/// The largest whole number a plan holds: every whole number up to it is an
/// `f64` exactly.
const MAX_EXACT: i64 = 1 << 53;

/// A whole number as the `f64` a plan holds it as, if it is exact.
fn exact(x: i64) -> Option<f64> {
    (x.unsigned_abs() <= MAX_EXACT as u64).then_some(x as f64)
}

/// The slot numbers from here up mark the nodes worked out inside the totals
/// that read them, in place of slots of their own: `INLINE + k` is the `k`th
/// of them. It is above any real slot.
const INLINE: u32 = 1 << 30;

struct Builder<'a> {
    g: &'a Graph,
    /// Where each node of the graph is, by its index: 0 if it is the same
    /// always (or not yet met), otherwise a [`Place`] as `mark` writes it.
    places: Vec<u32>,
    /// The level of each slot; a varied input is at level 0.
    level_of: Vec<u32>,
    consts: Vec<f64>,
    const_of: HashMap<u64, u32>,
    /// How many slots the varied (and cut) inputs take, and how many shared
    /// inputs there are.
    vary: usize,
    shared_inputs: usize,
    /// The nodes worked out for every scenario, before they are put in level
    /// order, and their instructions.
    drafts: Vec<Draft>,
    ops: Vec<u32>,
    /// The instructions of the nodes that depend only on shared inputs: per
    /// node, a start in `shared_ops` and a length.
    shared_programs: Vec<u32>,
    shared_ops: Vec<u32>,
    /// The instructions of the nodes worked out inside the totals that read
    /// them, one after another, and where each one's are.
    inline: Vec<u32>,
    inlined: Vec<(u32, u32)>,
    /// Room for `folded` to work in, kept from one node to the next.
    constant_scratch: Vec<bool>,
}

impl Builder<'_> {
    /// Adds a node with these instructions, and says where its value is: in
    /// a slot, if it reads one, or else a shared value. Its constant parts
    /// are worked out now, and an expression that needs a deeper stack than
    /// [`MAX_STACK`] is split into nodes that need less.
    fn node(&mut self, ops: Vec<u32>) -> Result<Place, PlanError> {
        let ops = self.folded(ops)?;
        let ops = self.shallow(ops)?;
        let mut level = None;
        for &op in &ops {
            if op & CODE == LOAD {
                let read = self.level_of[(op >> CODE_BITS) as usize] + 1;
                level = Some(level.unwrap_or(0).max(read));
            }
        }
        match level {
            Some(level) => {
                let start = self.ops.len();
                self.ops.extend_from_slice(&ops);
                let slot = self.vary + self.drafts.len();
                if self.ops.len() > u32::MAX as usize || slot > MAX_ARG {
                    return Err(PlanError::TooLarge);
                }
                self.level_of.push(level);
                self.drafts.push(Draft {
                    level,
                    ops: start..self.ops.len(),
                });
                Ok(Place::Slot(slot as u32))
            }
            None => {
                let start = self.shared_ops.len();
                self.shared_ops.extend_from_slice(&ops);
                let at = self.shared_inputs + self.shared_programs.len() / 2;
                if self.shared_ops.len() > u32::MAX as usize || at > MAX_ARG {
                    return Err(PlanError::TooLarge);
                }
                self.shared_programs.push(start as u32);
                self.shared_programs.push(ops.len() as u32);
                Ok(Place::Shared(at as u32))
            }
        }
    }

    /// The instructions with every part that reads nothing worked out now:
    /// an operation on constants becomes its result, and a condition that
    /// is constant keeps only the branch it takes. The result is what
    /// running them would give, since it is worked out by running them.
    fn folded(&mut self, ops: Vec<u32>) -> Result<Vec<u32>, PlanError> {
        // Most nodes have nothing to fold: find out first, without making
        // anything, whether any operation is on constants only.
        let mut constant = std::mem::take(&mut self.constant_scratch);
        constant.clear();
        let mut any = false;
        for &op in &ops {
            let code = op & CODE;
            let takes = arity(code);
            if takes == 0 {
                constant.push(code == CONST);
                continue;
            }
            let args = constant.len() - takes;
            if constant[args..].iter().all(|&c| c) || (code == SELECT && constant[args]) {
                any = true;
                break;
            }
            constant.truncate(args);
            constant.push(false);
        }
        self.constant_scratch = constant;
        if !any {
            return Ok(ops);
        }
        // For each value on the stack: where its instructions start in
        // `out`, and the value, if it is a constant.
        let mut stack: Vec<(usize, Option<f64>)> = Vec::new();
        let mut out: Vec<u32> = Vec::with_capacity(ops.len());
        for &op in &ops {
            let code = op & CODE;
            let takes = arity(code);
            if takes == 0 {
                let value = (code == CONST).then(|| self.consts[(op >> CODE_BITS) as usize]);
                stack.push((out.len(), value));
                out.push(op);
                continue;
            }
            let args = stack.split_off(stack.len() - takes);
            let start = args[0].0;
            if let (SELECT, Some(condition)) = (code, args[0].1) {
                // Keep the branch taken.
                let (from, value) = match condition != 0.0 {
                    true => (args[1].0, args[1].1),
                    false => (args[2].0, args[2].1),
                };
                let to = match condition != 0.0 {
                    true => args[2].0,
                    false => out.len(),
                };
                let kept = out[from..to].to_vec();
                out.truncate(start);
                out.extend(kept);
                stack.push((start, value));
                continue;
            }
            if args.iter().all(|(_, value)| value.is_some()) {
                let given: Vec<f64> = args.iter().map(|(_, value)| value.unwrap()).collect();
                let program: Vec<u32> = (0..takes as u32)
                    .map(|k| CONST | k << CODE_BITS)
                    .chain([code])
                    .collect();
                let value = eval(&program, &given, &[], &[]);
                out.truncate(start);
                out.push(CONST | self.constant(value)? << CODE_BITS);
                stack.push((start, Some(value)));
                continue;
            }
            out.push(op);
            stack.push((start, None));
        }
        Ok(out)
    }

    /// The instructions, needing a stack no deeper than [`MAX_STACK`]: if
    /// they need more, parts of them are made nodes of their own, and read.
    fn shallow(&mut self, ops: Vec<u32>) -> Result<Vec<u32>, PlanError> {
        if need(&ops) <= MAX_STACK {
            return Ok(ops);
        }
        // The expression as a tree: each instruction's operands, which come
        // before it, and the stack it needs. An operand made a node of its
        // own needs one.
        let mut operands: Vec<Vec<usize>> = Vec::with_capacity(ops.len());
        let mut needs: Vec<usize> = Vec::with_capacity(ops.len());
        let mut cut = vec![false; ops.len()];
        let mut stack: Vec<usize> = Vec::new();
        for (at, &op) in ops.iter().enumerate() {
            let args = stack.split_off(stack.len() - arity(op & CODE));
            let need_of = |args: &[usize], needs: &[usize], cut: &[bool]| {
                args.iter()
                    .enumerate()
                    .map(|(k, &a)| k + if cut[a] { 1 } else { needs[a] })
                    .max()
                    .unwrap_or(1)
            };
            // Make the operand that needs most a node of its own until this
            // needs no more than is allowed. Each operand already does, so
            // this ends: with every operand cut, it needs three at most.
            while need_of(&args, &needs, &cut) > MAX_STACK {
                let deepest = *args
                    .iter()
                    .filter(|&&a| !cut[a])
                    .max_by_key(|&&a| needs[a])
                    .expect("an operand left to cut");
                cut[deepest] = true;
            }
            needs.push(need_of(&args, &needs, &cut));
            operands.push(args);
            stack.push(at);
        }
        // Each cut part, lowest first, then the rest, made into instructions
        // that read the parts below them.
        let mut place_of: HashMap<usize, Place> = HashMap::new();
        let written = |at: usize, place_of: &HashMap<usize, Place>| {
            let mut out = Vec::new();
            let mut visit = vec![(at, false)];
            while let Some((n, done)) = visit.pop() {
                if n != at && cut[n] {
                    out.push(place_of[&n].load());
                } else if done || operands[n].is_empty() {
                    out.push(ops[n]);
                } else {
                    visit.push((n, true));
                    visit.extend(operands[n].iter().rev().map(|&a| (a, false)));
                }
            }
            out
        };
        for (at, _) in cut.iter().enumerate().filter(|(_, is_cut)| **is_cut) {
            let part = written(at, &place_of);
            let place = self.node(part)?;
            place_of.insert(at, place);
        }
        Ok(written(ops.len() - 1, &place_of))
    }

    fn name(&self, id: NodeId) -> String {
        self.g.name(id).to_string()
    }

    /// Where the instructions of a node worked out inside the totals that
    /// read it are in `inline`, if it is one.
    fn inlined(&self, id: NodeId) -> Option<Range<usize>> {
        match self.place(id) {
            Some(Place::Slot(slot)) if slot >= INLINE => {
                let (start, len) = self.inlined[(slot - INLINE) as usize];
                Some(start as usize..(start + len) as usize)
            }
            _ => None,
        }
    }

    fn place(&self, id: NodeId) -> Option<Place> {
        match self.places[id.index()] {
            0 => None,
            mark if mark & 1 == 1 => Some(Place::Slot(mark >> 1)),
            mark => Some(Place::Shared((mark >> 1) - 1)),
        }
    }

    /// Records where a node is. Gives back whether it had a place already.
    fn mark(&mut self, id: NodeId, place: Place) -> bool {
        let before = std::mem::replace(
            &mut self.places[id.index()],
            match place {
                Place::Slot(slot) => slot << 1 | 1,
                Place::Shared(at) => (at + 1) << 1,
            },
        );
        before != 0
    }

    fn constant(&mut self, x: f64) -> Result<u32, PlanError> {
        if let Some(&at) = self.const_of.get(&x.to_bits()) {
            return Ok(at);
        }
        if self.consts.len() >= MAX_ARG {
            return Err(PlanError::TooLarge);
        }
        let at = self.consts.len() as u32;
        self.consts.push(x);
        self.const_of.insert(x.to_bits(), at);
        Ok(at)
    }

    /// The value of a node that is the same always.
    fn fixed(&self, id: NodeId) -> Result<f64, PlanError> {
        if !self.g.is_calculated(id) {
            return Err(PlanError::NotCalculated(self.name(id)));
        }
        match self.g.value(id) {
            Value::F64(x) => Ok(*x),
            Value::Bool(x) => Ok(f64::from(u8::from(*x))),
            Value::I64(x) => exact(*x).ok_or_else(|| PlanError::Unsupported {
                node: self.name(id),
                what: format!("the whole number {x}, too large to hold exactly"),
            }),
            Value::Error(error) => Err(PlanError::HoldsError {
                node: self.name(id),
                error: error.to_string(),
            }),
            _ => Err(PlanError::NotAFloat {
                node: self.name(id),
                ty: self.g.node_type(id),
            }),
        }
    }
}

/// What a node of this type leaves on the stack.
fn kind_of(g: &Graph, id: NodeId) -> Result<Kind, PlanError> {
    match g.node_type(id) {
        Type::F64 => Ok(Kind::Float),
        Type::Bool => Ok(Kind::Bool),
        Type::I64 => Ok(Kind::Int),
        ty => Err(PlanError::NotAFloat {
            node: g.name(id).to_string(),
            ty,
        }),
    }
}

/// Some nodes' instructions before it is known where what they read is: a
/// load's number is into `reads`, and a constant's into `consts`. These are
/// made for many nodes at once, on every core; where each node reads from
/// is settled afterwards, one node after another.
#[derive(Default)]
struct Rough {
    ops: Vec<u32>,
    reads: Vec<NodeId>,
    consts: Vec<f64>,
    /// Each choice's option number, to be checked once it is known where
    /// what it reads is: where its instructions start in `ops`, how many
    /// there are, and how many options there are.
    choices: Vec<(usize, usize, usize)>,
    nodes: Vec<RoughNode>,
}

enum RoughNode {
    /// A total, which has members in place of an expression.
    Total,
    /// These of the rough instructions, and these of the choices.
    Ops(Range<usize>, Range<usize>),
    /// It cannot be done. This matters only if the node turns out to depend
    /// on a varied or shared input.
    Refused(PlanError),
}

/// One node's rough instructions as they are made.
struct Program<'r> {
    g: &'r Graph,
    node: NodeId,
    rough: &'r mut Rough,
}

impl Program<'_> {
    fn op(&mut self, op: u32) {
        self.rough.ops.push(op);
    }

    fn unsupported(&self, what: impl Into<String>) -> PlanError {
        PlanError::Unsupported {
            node: self.g.name(self.node).to_string(),
            what: what.into(),
        }
    }

    fn constant(&mut self, x: f64) {
        let at = self.rough.consts.len() as u32;
        self.rough.consts.push(x);
        self.op(CONST | at << CODE_BITS);
    }

    /// Adds the instructions for `ir`, and says what they leave on the
    /// stack.
    ///
    /// Both branches of a condition are worked out and one is kept. That
    /// gives what the graph gives, which works out only the branch taken,
    /// because nothing here can fail: there are no errors to meet in the
    /// branch not taken.
    fn translate(&mut self, ir: &Ir) -> Result<Kind, PlanError> {
        match ir {
            Ir::Const(Value::F64(x)) => {
                self.constant(*x);
                Ok(Kind::Float)
            }
            Ir::Const(Value::Bool(x)) => {
                self.constant(f64::from(u8::from(*x)));
                Ok(Kind::Bool)
            }
            Ir::Const(Value::I64(x)) => {
                match exact(*x) {
                    Some(x) => {
                        self.constant(x);
                        Ok(Kind::Int)
                    }
                    None => Err(self
                        .unsupported(format!("the whole number {x}, too large to hold exactly"))),
                }
            }
            Ir::Const(other) => Err(self.unsupported(match other.type_of() {
                Some(ty) => format!("work on {ty}"),
                None => "an error".into(),
            })),
            Ir::Ref(r) => {
                let kind = kind_of(self.g, r.node)?;
                let at = self.rough.reads.len() as u32;
                self.rough.reads.push(r.node);
                self.op(LOAD | at << CODE_BITS);
                Ok(kind)
            }
            Ir::Unary(op, a) => {
                let kind = self.translate(a)?;
                match (op, kind) {
                    (UnOp::Neg, Kind::Float) => self.op(NEG),
                    (UnOp::Abs, Kind::Float) => self.op(ABS),
                    (UnOp::Exp, Kind::Float) => self.op(EXP),
                    (UnOp::Ln, Kind::Float) => self.op(LN),
                    (UnOp::Sqrt, Kind::Float) => self.op(SQRT),
                    (UnOp::Not, Kind::Bool) => self.op(NOT),
                    (_, Kind::Int) => return Err(self.unsupported("arithmetic of whole numbers")),
                    (other, _) => return Err(self.unsupported(format!("a {other:?}"))),
                }
                Ok(kind)
            }
            Ir::Binary(op, a, b) => {
                let left = self.translate(a)?;
                let right = self.translate(b)?;
                let (code, gives) = match op {
                    BinOp::Add => (ADD, Kind::Float),
                    BinOp::Sub => (SUB, Kind::Float),
                    BinOp::Mul => (MUL, Kind::Float),
                    BinOp::Div => (DIV, Kind::Float),
                    BinOp::Min => (MIN, left),
                    BinOp::Max => (MAX, left),
                    BinOp::Pow => (POW, Kind::Float),
                    BinOp::Lt => (LT, Kind::Bool),
                    BinOp::Le => (LE, Kind::Bool),
                    BinOp::Gt => (GT, Kind::Bool),
                    BinOp::Ge => (GE, Kind::Bool),
                    BinOp::Eq => (EQ, Kind::Bool),
                    BinOp::Ne => (NE, Kind::Bool),
                    BinOp::And => (AND, Kind::Bool),
                    BinOp::Or => (OR, Kind::Bool),
                    other => return Err(self.unsupported(format!("a {other:?}"))),
                };
                // Arithmetic is on numbers; comparing is of either, since
                // false is below true as 0.0 is below 1.0. `And` and `Or`
                // are of bools, which the graph's own type check has seen to.
                // Whole numbers are compared, and the smaller or larger
                // taken, exactly; adding them and the rest is not done here,
                // since the graph's whole numbers do not round and can
                // overflow.
                let arithmetic = matches!(code, ADD | SUB | MUL | DIV | POW);
                if (arithmetic || matches!(code, MIN | MAX))
                    && (left == Kind::Bool || right == Kind::Bool)
                {
                    return Err(self.unsupported("arithmetic of a bool"));
                }
                if arithmetic && (left == Kind::Int || right == Kind::Int) {
                    return Err(self.unsupported("arithmetic of whole numbers"));
                }
                self.op(code);
                Ok(gives)
            }
            Ir::If(condition, then, otherwise) => {
                self.translate(condition)?;
                let kind = self.translate(then)?;
                self.translate(otherwise)?;
                self.op(SELECT);
                Ok(kind)
            }
            // `(index == 0) ? first : ((index == 1) ? second : (… : NaN))`.
            // The option number must turn out the same in every scenario,
            // and in range (checked once it is known what it reads); then
            // the choice folds to the option it picks, and the NaN is never
            // reached.
            Ir::Choose(index, options) => {
                let mut kind = Kind::Float;
                for (k, option) in options.iter().enumerate() {
                    let start = self.rough.ops.len();
                    if self.translate(index)? != Kind::Int {
                        return Err(self.unsupported("a choice by something not a whole number"));
                    }
                    if k == 0 {
                        let len = self.rough.ops.len() - start;
                        self.rough.choices.push((start, len, options.len()));
                    }
                    self.constant(k as f64);
                    self.op(EQ);
                    kind = self.translate(option)?;
                }
                self.constant(f64::NAN);
                for _ in options {
                    self.op(SELECT);
                }
                Ok(kind)
            }
            Ir::List(_) => Err(self.unsupported("work on a list")),
        }
    }
}

/// The rough instructions of some nodes.
fn rough(g: &Graph, nodes: &[NodeId]) -> Rough {
    let mut rough = Rough::default();
    for &node in nodes {
        let Some(ir) = g.expression(node) else {
            rough.nodes.push(RoughNode::Total);
            continue;
        };
        let start = rough.ops.len();
        let choices = rough.choices.len();
        let mut program = Program {
            g,
            node,
            rough: &mut rough,
        };
        let translated = program.translate(&ir);
        rough.nodes.push(match translated {
            Ok(_) => RoughNode::Ops(start..rough.ops.len(), choices..rough.choices.len()),
            Err(refused) => {
                rough.ops.truncate(start);
                rough.choices.truncate(choices);
                RoughNode::Refused(refused)
            }
        });
    }
    rough
}

/// Part of a total's tree, as it is lowered: nothing (room no member holds,
/// or members that are absent: the operation's identity, which never changes
/// a result), a number the same in every scenario, or instructions, which
/// have been written.
#[derive(Clone, Copy)]
enum Part {
    Nothing,
    Fixed(f64),
    Ops,
}

/// The most instructions a piece of a total is given, roughly: a total is cut
/// into whole subtrees of a number of members that keeps each under this, so
/// that every piece is short enough for a kernel of its own and a GPU works
/// the pieces out side by side.
const PIECE: usize = 512;

/// The most members in one piece of a total.
const MOST_MEMBERS: usize = 256;

/// The longest node, in instructions, that is worked out inside the totals
/// that read it rather than given a slot of its own.
const INLINE_MOST: usize = 24;

/// The instructions for a total (a `sum`, `min`, `max` or `count` over a
/// collection or a group) that does not stay the same always.
///
/// They combine the members in the tree the graph keeps (see
/// `watershed_core`'s aggregate): room for the next power of two, each slot
/// its two halves combined, so the result is the graph's, bit for bit.
/// Room no member holds, and members that are absent in every scenario, hold
/// the identity, so they are left out; members that are the same in every
/// scenario are combined now, in the same order, into one number.
///
/// A large total is cut into whole subtrees of the same number of members,
/// each made a node of its own, and those again, so that the order of
/// combining is the graph's; the total combines the last of them. A member
/// worked out inside its total (see [`INLINE_MOST`]) is worked out in its
/// piece.
fn lowered_total(b: &mut Builder, id: NodeId) -> Result<Vec<u32>, PlanError> {
    let g = b.g;
    let refuse = |what: &str| PlanError::Unsupported {
        node: g.name(id).to_string(),
        what: what.into(),
    };
    let Some((op, members)) = g.aggregate_members(id) else {
        return Err(refuse("a total"));
    };
    let (code, identity) = match op {
        AggregateOp::Sum => (AGG_SUM, -0.0),
        AggregateOp::Min => (AGG_MIN, f64::INFINITY),
        AggregateOp::Max => (AGG_MAX, f64::NEG_INFINITY),
        // A count adds up 1 for each member present: whole numbers, exact
        // in any order. Its identity is 0.
        AggregateOp::Count => (AGG_SUM, 0.0),
    };
    if op != AggregateOp::Count && g.node_type(id) != Type::F64 {
        return Err(refuse("a total of whole numbers or decimals"));
    }
    // How many members a piece takes: a power of two, so that a piece is a
    // whole subtree, as many as keep it under `PIECE` instructions.
    let size = |b: &Builder, node: NodeId| b.inlined(node).map_or(1, |at| at.len());
    let longest = members
        .iter()
        .map(|&(value, present)| match op {
            AggregateOp::Count => size(b, present),
            _ => size(b, value) + size(b, present),
        })
        .max()
        .unwrap_or(1)
        + 3;
    let per_piece = (PIECE / longest).clamp(2, MOST_MEMBERS);
    let per_piece = 1 << per_piece.ilog2();
    let tree = Tree {
        op,
        code,
        identity,
        members: &members,
        capacity: members.len().max(1).next_power_of_two(),
        per_piece,
    };
    let mut ops = Vec::new();
    match tree.fold(b, 0, tree.capacity, &mut ops)? {
        Part::Nothing => ops.push(CONST | b.constant(identity)? << CODE_BITS),
        Part::Fixed(x) => ops.push(CONST | b.constant(x)? << CODE_BITS),
        Part::Ops => {}
    }
    Ok(ops)
}

/// A total being lowered: how it combines, and its members (each a value
/// node and a node saying whether the member is present).
struct Tree<'m> {
    op: AggregateOp,
    code: u32,
    identity: f64,
    members: &'m [(NodeId, NodeId)],
    /// The room in the tree.
    capacity: usize,
    /// How many members a piece of the total takes: a power of two.
    per_piece: usize,
}

impl Tree<'_> {
    /// Two parts combined as the graph's totals combine them.
    fn combine(&self, x: f64, y: f64) -> f64 {
        if x.is_nan() {
            return x;
        }
        if y.is_nan() {
            return y;
        }
        match self.op {
            AggregateOp::Min => x.min(y),
            AggregateOp::Max => x.max(y),
            _ => x + y,
        }
    }

    /// Whether a subtree of this many members is made a piece of its own:
    /// one of `per_piece` members, or of that many pieces, and so on, below
    /// the whole.
    fn is_piece(&self, members: usize) -> bool {
        let (step, at) = (self.per_piece.ilog2(), members.ilog2());
        members < self.capacity && at > 0 && at % step == 0
    }

    /// One member's part: its value, or for a count 1, if it is present.
    fn leaf(
        &self,
        b: &mut Builder,
        (value, present): (NodeId, NodeId),
        out: &mut Vec<u32>,
    ) -> Result<Part, PlanError> {
        let value = |b: &mut Builder, out: &mut Vec<u32>| match self.op {
            AggregateOp::Count => Ok(Part::Fixed(1.0)),
            _ => Part::load(b, value, out),
        };
        match b.place(present) {
            None => match b.fixed(present)? != 0.0 {
                true => value(b, out),
                false => Ok(Part::Nothing),
            },
            // A count's member is its presence, which is 1.0 or 0.0.
            Some(_) if self.op == AggregateOp::Count => Part::load(b, present, out),
            Some(_) => {
                // Present in some scenarios: the value, or the identity.
                Part::load(b, present, out)?;
                if let Part::Fixed(x) = value(b, out)? {
                    out.push(CONST | b.constant(x)? << CODE_BITS);
                }
                out.push(CONST | b.constant(self.identity)? << CODE_BITS);
                out.push(SELECT);
                Ok(Part::Ops)
            }
        }
    }

    /// The members in `lo..hi` of the tree, combined as the graph combines
    /// them: the two halves, each folded the same way. Instructions are
    /// written to `out`.
    fn fold(
        &self,
        b: &mut Builder,
        lo: usize,
        hi: usize,
        out: &mut Vec<u32>,
    ) -> Result<Part, PlanError> {
        if lo >= self.members.len() {
            return Ok(Part::Nothing);
        }
        if hi - lo == 1 {
            return self.leaf(b, self.members[lo], out);
        }
        let start = out.len();
        let mid = (lo + hi) / 2;
        let left = self.fold(b, lo, mid, out)?;
        let right_at = out.len();
        let right = self.fold(b, mid, hi, out)?;
        let part = match (left, right) {
            // The identity changes nothing, so a half that is only room or
            // absent members is left out.
            (Part::Nothing, part) | (part, Part::Nothing) => part,
            (Part::Fixed(x), Part::Fixed(y)) => Part::Fixed(self.combine(x, y)),
            (Part::Ops, Part::Ops) => {
                out.push(self.code);
                Part::Ops
            }
            (Part::Ops, Part::Fixed(y)) => {
                out.push(CONST | b.constant(y)? << CODE_BITS);
                out.push(self.code);
                Part::Ops
            }
            (Part::Fixed(x), Part::Ops) => {
                out.insert(right_at, CONST | b.constant(x)? << CODE_BITS);
                out.push(self.code);
                Part::Ops
            }
        };
        // A piece is a node of its own, unless it only reads one value.
        if let Part::Ops = part {
            if self.is_piece(hi - lo) && out.len() - start > 1 {
                let piece = out.split_off(start);
                let place = b.node(piece)?;
                out.push(place.load());
            }
        }
        Ok(part)
    }
}

impl Part {
    /// A node's value: written as a load from where it is, or as its
    /// instructions if it is worked out inside what reads it; or the number
    /// it always is.
    fn load(b: &mut Builder, node: NodeId, out: &mut Vec<u32>) -> Result<Part, PlanError> {
        if let Some(at) = b.inlined(node) {
            out.extend_from_slice(&b.inline[at]);
            return Ok(Part::Ops);
        }
        Ok(match b.place(node) {
            Some(place) => {
                out.push(place.load());
                Part::Ops
            }
            None => Part::Fixed(b.fixed(node)?),
        })
    }
}

enum Visit {
    Enter(NodeId),
    Leave(NodeId),
}

/// The stack these instructions need.
fn need(ops: &[u32]) -> usize {
    let (mut depth, mut deepest) = (0usize, 0usize);
    for &op in ops {
        depth = depth + 1 - arity(op & CODE);
        deepest = deepest.max(depth);
    }
    deepest
}

/// Runs one node's instructions. `vals` holds this scenario's slots (none,
/// for a node that depends only on shared inputs).
#[inline]
fn eval(ops: &[u32], consts: &[f64], shared: &[f64], vals: &[f64]) -> f64 {
    let truth = |b: bool| f64::from(u8::from(b));
    let mut stack = [0.0f64; MAX_STACK];
    let mut sp = 0;
    for &op in ops {
        let arg = (op >> CODE_BITS) as usize;
        match op & CODE {
            LOAD => {
                stack[sp] = vals[arg];
                sp += 1;
            }
            CONST => {
                stack[sp] = consts[arg];
                sp += 1;
            }
            LOAD_SHARED => {
                stack[sp] = shared[arg];
                sp += 1;
            }
            NEG => stack[sp - 1] = -stack[sp - 1],
            ABS => stack[sp - 1] = stack[sp - 1].abs(),
            EXP => stack[sp - 1] = stack[sp - 1].exp(),
            LN => stack[sp - 1] = stack[sp - 1].ln(),
            SQRT => stack[sp - 1] = stack[sp - 1].sqrt(),
            NOT => stack[sp - 1] = truth(stack[sp - 1] == 0.0),
            SELECT => {
                sp -= 2;
                stack[sp - 1] = match stack[sp - 1] != 0.0 {
                    true => stack[sp],
                    false => stack[sp + 1],
                };
            }
            code => {
                sp -= 1;
                let (a, b) = (stack[sp - 1], stack[sp]);
                stack[sp - 1] = match code {
                    ADD => a + b,
                    SUB => a - b,
                    MUL => a * b,
                    DIV => a / b,
                    POW => a.powf(b),
                    MIN => a.min(b),
                    MAX => a.max(b),
                    AGG_SUM | AGG_MIN | AGG_MAX if a.is_nan() => a,
                    AGG_SUM | AGG_MIN | AGG_MAX if b.is_nan() => b,
                    AGG_SUM => a + b,
                    AGG_MIN => a.min(b),
                    AGG_MAX => a.max(b),
                    // As the graph compares: with a NaN on either side,
                    // only "not equal" is true.
                    LT => truth(a < b),
                    LE => truth(a <= b),
                    GT => truth(a > b),
                    GE => truth(a >= b),
                    EQ => truth(a == b),
                    NE => truth(a != b),
                    AND => truth(a != 0.0 && b != 0.0),
                    OR => truth(a != 0.0 || b != 0.0),
                    _ => unreachable!("no such instruction"),
                };
            }
        }
    }
    stack[0]
}

/// Adds values up a block at a time, then the blocks.
pub(crate) fn total(values: impl Iterator<Item = f64>) -> f64 {
    let (mut all, mut block, mut in_block) = (0.0, 0.0, 0);
    for x in values {
        block += x;
        in_block += 1;
        if in_block == BLOCK {
            all += block;
            (block, in_block) = (0.0, 0);
        }
    }
    if in_block > 0 {
        all += block;
    }
    all
}

impl Scenarios {
    /// Prepares `g` to be run for many sets of values of the inputs `vary`,
    /// reading `outputs` each time.
    pub fn new(g: &Graph, vary: &[NodeId], outputs: &[NodeId]) -> Result<Scenarios, PlanError> {
        Scenarios::with_shared(g, vary, &[], outputs)
    }

    /// As [`new`](Self::new), with inputs `shared` whose values can be
    /// replaced later with [`set_shared`](Self::set_shared) without making
    /// the plan again. They are the same for every scenario of a run: the
    /// data the scenarios are tried on, where `vary` is what is tried.
    /// Until they are replaced they have the values they have in `g` now.
    pub fn with_shared(
        g: &Graph,
        vary: &[NodeId],
        shared: &[NodeId],
        outputs: &[NodeId],
    ) -> Result<Scenarios, PlanError> {
        // EXPERIMENTAL: set WATERSHED_CLUSTERS to make every plan as
        // `with_clusters` does, to try it without changing a program.
        let clusters = std::env::var_os("WATERSHED_CLUSTERS").is_some();
        Scenarios::build(g, vary, &[], shared, outputs, clusters)
    }

    /// EXPERIMENTAL. As [`with_shared`](Self::with_shared), and also puts
    /// the nodes into clusters that a GPU thread works out in one go,
    /// keeping to itself what only the cluster reads. On a big graph run
    /// for many scenarios the GPU is then faster, by half as much again on
    /// the one it was measured on. The plan takes longer to make, and the
    /// first run in a program longer still, while a kernel is compiled for
    /// each shape of cluster. The answers are the same.
    pub fn with_clusters(
        g: &Graph,
        vary: &[NodeId],
        shared: &[NodeId],
        outputs: &[NodeId],
    ) -> Result<Scenarios, PlanError> {
        Scenarios::build(g, vary, &[], shared, outputs, true)
    }

    /// As [`with_shared`](Self::with_shared), with calculated nodes `cut`
    /// that are not worked out but given, like the inputs `vary` and after
    /// them in a scenario's values. Nothing a cut node uses is looked at.
    pub(crate) fn cut_at(
        g: &Graph,
        vary: &[NodeId],
        cut: &[NodeId],
        shared: &[NodeId],
        outputs: &[NodeId],
    ) -> Result<Scenarios, PlanError> {
        Scenarios::build(g, vary, cut, shared, outputs, false)
    }

    fn build(
        g: &Graph,
        vary: &[NodeId],
        cut: &[NodeId],
        shared: &[NodeId],
        outputs: &[NodeId],
        clusters: bool,
    ) -> Result<Scenarios, PlanError> {
        let inputs = vary.len();
        let vary = &[vary, cut].concat()[..];
        if vary.is_empty() {
            return Err(PlanError::NoInputs);
        }
        if outputs.is_empty() {
            return Err(PlanError::NoOutputs);
        }
        if vary.len() > MAX_ARG || shared.len() > MAX_ARG {
            return Err(PlanError::TooLarge);
        }
        let typed = |id: NodeId, whole: bool| {
            if id.index() >= g.len() {
                return Err(PlanError::UnknownNode(id));
            }
            match g.node_type(id) {
                Type::F64 => Ok(()),
                Type::I64 if whole => Ok(()),
                ty => Err(PlanError::NotAFloat {
                    node: g.name(id).to_string(),
                    ty,
                }),
            }
        };
        let mut b = Builder {
            g,
            places: vec![0; g.len()],
            level_of: vec![0; vary.len()],
            consts: Vec::new(),
            const_of: HashMap::new(),
            vary: vary.len(),
            shared_inputs: shared.len(),
            drafts: Vec::new(),
            ops: Vec::new(),
            shared_programs: Vec::new(),
            shared_ops: Vec::new(),
            inline: Vec::new(),
            inlined: Vec::new(),
            constant_scratch: Vec::new(),
        };
        let mut shared_values = Vec::with_capacity(shared.len());
        let places = (vary
            .iter()
            .enumerate()
            .map(|(n, id)| (id, Place::Slot(n as u32))))
        .chain(
            shared
                .iter()
                .enumerate()
                .map(|(n, id)| (id, Place::Shared(n as u32))),
        );
        for (&id, place) in places {
            typed(id, false)?;
            let given = matches!(place, Place::Slot(n) if n as usize >= inputs);
            if !given && !g.is_input(id) {
                return Err(PlanError::NotAnInput(b.name(id)));
            }
            if b.mark(id, place) {
                return Err(PlanError::Repeated(b.name(id)));
            }
            if matches!(place, Place::Shared(_)) {
                shared_values.push(b.fixed(id)?);
            }
        }
        // An output may be a whole number (a count): it comes out as the
        // `f64` of the same value.
        for &id in outputs {
            typed(id, true)?;
        }

        // What a node reads, for finding the order: a count reads only
        // whether its members are present, not their values.
        let reads = |id: NodeId| match g.aggregate_members(id) {
            Some((AggregateOp::Count, members)) => {
                let mut present: Vec<NodeId> = members.iter().map(|&(_, p)| p).collect();
                present.sort();
                present.dedup();
                present
            }
            _ => g.dependencies(id),
        };

        // Every calculated node the outputs depend on, each after the nodes
        // it uses. This follows every branch of a condition, taken or not.
        let mut order: Vec<NodeId> = Vec::new();
        let mut seen = vec![false; g.len()];
        let mut stack: Vec<Visit> = outputs.iter().map(|&id| Visit::Enter(id)).collect();
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Enter(id) => {
                    if std::mem::replace(&mut seen[id.index()], true)
                        || g.is_input(id)
                        || b.place(id).is_some()
                    {
                        continue;
                    }
                    stack.push(Visit::Leave(id));
                    stack.extend(
                        reads(id)
                            .into_iter()
                            .filter(|u| !seen[u.index()])
                            .map(Visit::Enter),
                    );
                }
                Visit::Leave(id) => order.push(id),
            }
        }
        drop(seen);

        // Each node's instructions, without yet knowing where what it reads
        // is: that needs nothing but the graph, so every core does a share.
        const AT_A_TIME: usize = 2048;
        let roughs: Vec<Rough> = order
            .par_chunks(AT_A_TIME)
            .map(|nodes| rough(g, nodes))
            .collect();

        // The nodes read only by totals: short ones are worked out inside
        // each total that reads them, so that a total's members need no
        // slots. An output is read at the end, so it is not one of them.
        const BY_A_TOTAL: u8 = 1;
        const BY_ANOTHER: u8 = 2;
        let mut read_by = vec![0u8; g.len()];
        for &id in outputs {
            read_by[id.index()] |= BY_ANOTHER;
        }
        for (nodes, rough) in order.chunks(AT_A_TIME).zip(&roughs) {
            for (&id, node) in nodes.iter().zip(&rough.nodes) {
                match node {
                    RoughNode::Total => {
                        for read in reads(id) {
                            read_by[read.index()] |= BY_A_TOTAL;
                        }
                    }
                    RoughNode::Ops(range, _) => {
                        for &op in &rough.ops[range.clone()] {
                            if op & CODE == LOAD {
                                let read = rough.reads[(op >> CODE_BITS) as usize];
                                read_by[read.index()] |= BY_ANOTHER;
                            }
                        }
                    }
                    RoughNode::Refused(_) => {
                        for read in g.dependencies(id) {
                            read_by[read.index()] |= BY_ANOTHER;
                        }
                    }
                }
            }
        }

        // Now, one node after another, where each reads from. A node that
        // reads nothing varied or shared is the same always and is left out.
        for (nodes, rough) in order.chunks(AT_A_TIME).zip(&roughs) {
            for (&id, node) in nodes.iter().zip(&rough.nodes) {
                let varied = |b: &Builder| reads(id).iter().any(|&d| b.place(d).is_some());
                let placed = match node {
                    RoughNode::Total => {
                        if !varied(&b) {
                            continue;
                        }
                        lowered_total(&mut b, id)?
                    }
                    RoughNode::Refused(refused) => {
                        // Whether it matters: does it read anything that is
                        // not the same always?
                        if varied(&b) {
                            kind_of(g, id)?;
                            return Err(refused.clone());
                        }
                        continue;
                    }
                    RoughNode::Ops(range, choices) => {
                        let reads = |op: &u32| match op & CODE {
                            LOAD => b.place(rough.reads[(op >> CODE_BITS) as usize]),
                            _ => None,
                        };
                        if !rough.ops[range.clone()]
                            .iter()
                            .any(|op| reads(op).is_some())
                        {
                            continue;
                        }
                        kind_of(g, id)?;
                        let mut placed = Vec::with_capacity(range.len());
                        for &op in &rough.ops[range.clone()] {
                            let at = (op >> CODE_BITS) as usize;
                            placed.push(match op & CODE {
                                LOAD => match b.place(rough.reads[at]) {
                                    Some(place) => place.load(),
                                    None => {
                                        let x = b.fixed(rough.reads[at])?;
                                        CONST | b.constant(x)? << CODE_BITS
                                    }
                                },
                                CONST => CONST | b.constant(rough.consts[at])? << CODE_BITS,
                                _ => op,
                            });
                        }
                        // A choice's option number must be the same in every
                        // scenario, and one of its options.
                        for &(start, len, options) in &rough.choices[choices.clone()] {
                            let index = &placed[start - range.start..][..len];
                            if index
                                .iter()
                                .any(|op| op & CODE != CONST && arity(op & CODE) == 0)
                            {
                                return Err(PlanError::Unsupported {
                                    node: b.name(id),
                                    what: "a choice whose option number varies".into(),
                                });
                            }
                            let k = eval(index, &b.consts, &[], &[]);
                            if !(0.0..options as f64).contains(&k) {
                                return Err(PlanError::HoldsError {
                                    node: b.name(id),
                                    error: format!(
                                        "choice {k} is out of range: there are {options} options, numbered from 0"
                                    ),
                                });
                            }
                        }
                        // Read only by totals, and short: worked out inside
                        // them.
                        if read_by[id.index()] == BY_A_TOTAL {
                            placed = b.folded(placed)?;
                            let k = b.inlined.len() as u32;
                            if placed.len() <= INLINE_MOST
                                && need(&placed) <= MAX_STACK
                                && k < INLINE
                            {
                                let start = b.inline.len() as u32;
                                b.inline.extend_from_slice(&placed);
                                b.inlined.push((start, placed.len() as u32));
                                b.mark(id, Place::Slot(INLINE + k));
                                continue;
                            }
                        }
                        placed
                    }
                };
                let place = b.node(placed)?;
                b.mark(id, place);
            }
        }
        drop(roughs);
        let drafts = std::mem::take(&mut b.drafts);
        let ops = std::mem::take(&mut b.ops);
        let shared_programs = std::mem::take(&mut b.shared_programs);
        let shared_ops = std::mem::take(&mut b.shared_ops);

        // A node's shape is its instructions without what they read. Nodes
        // of one shape in one level are worked out together.
        let mut shapes: Vec<Vec<u32>> = Vec::new();
        let mut shape_of: Vec<u32> = Vec::with_capacity(drafts.len());
        let mut known: HashMap<Vec<u32>, u32> = HashMap::new();
        let mut codes: Vec<u32> = Vec::new();
        for draft in &drafts {
            codes.clear();
            codes.extend(ops[draft.ops.clone()].iter().map(|op| op & CODE));
            let shape = match known.get(&codes) {
                Some(&shape) => shape,
                None => {
                    let shape = shapes.len() as u32;
                    shapes.push(codes.clone());
                    known.insert(codes.clone(), shape);
                    shape
                }
            };
            shape_of.push(shape);
        }

        // Put the nodes in level order, and within a level in order of shape.
        let mut by_level: Vec<u32> = (0..drafts.len() as u32).collect();
        by_level.sort_by_key(|&n| (drafts[n as usize].level, shape_of[n as usize]));
        let mut moved_to = vec![0u32; drafts.len()];
        for (now, &was) in by_level.iter().enumerate() {
            moved_to[was as usize] = now as u32;
        }
        // The node a provisional slot belongs to, in the new order.
        let node_of = |slot: u32| {
            (slot as usize)
                .checked_sub(vary.len())
                .map(|n| moved_to[n] as usize)
        };

        // The last level that reads each node. An output is read at the end.
        let mut last_read: Vec<u32> = by_level
            .iter()
            .map(|&was| drafts[was as usize].level)
            .collect();
        for &was in &by_level {
            let draft = &drafts[was as usize];
            for &op in &ops[draft.ops.clone()] {
                if op & CODE == LOAD {
                    if let Some(read) = node_of(op >> CODE_BITS) {
                        last_read[read] = last_read[read].max(draft.level);
                    }
                }
            }
        }
        for id in outputs {
            if let Some(Place::Slot(slot)) = b.place(*id) {
                if let Some(node) = node_of(slot) {
                    last_read[node] = u32::MAX;
                }
            }
        }

        // Give each node a slot. A slot is free again for the levels after
        // the last one that reads it: no node of a level writes where
        // another node of the same level reads.
        let highest = drafts.iter().map(|d| d.level).max().unwrap_or(0) as usize;
        let mut freed_after: Vec<Vec<u32>> = vec![Vec::new(); highest + 1];
        // The lowest free slot is used first, so that a level's nodes have
        // slots in rising order, mostly next to each other, and the slots in
        // use stay packed together: the CPU reads memory in order far
        // faster than it hops about.
        let mut free: BinaryHeap<Reverse<u32>> = BinaryHeap::new();
        let mut slot_of = vec![0u32; drafts.len()];
        let mut slots = vary.len() as u32;
        let mut released = 0;
        for (now, &was) in by_level.iter().enumerate() {
            let level = drafts[was as usize].level as usize;
            while released < level {
                free.extend(freed_after[released].drain(..).map(Reverse));
                released += 1;
            }
            let slot = match free.pop() {
                Some(Reverse(slot)) => slot,
                None => {
                    slots += 1;
                    slots - 1
                }
            };
            slot_of[now] = slot;
            if last_read[now] != u32::MAX {
                freed_after[last_read[now] as usize].push(slot);
            }
        }
        let final_slot = |slot: u32| match node_of(slot) {
            Some(node) => slot_of[node],
            None => slot,
        };

        // The same nodes as clusters, for the GPU.
        let levels_now: Vec<u32> = by_level
            .iter()
            .map(|&was| drafts[was as usize].level)
            .collect();
        let kept: Vec<usize> = outputs
            .iter()
            .filter_map(|&id| match b.place(id) {
                Some(Place::Slot(slot)) => node_of(slot),
                _ => None,
            })
            .collect();
        let setting = |name: &str, otherwise: usize| {
            std::env::var(name)
                .ok()
                .and_then(|x| x.parse().ok())
                .unwrap_or(otherwise)
        };
        let fused = match clusters {
            true => fuse(
                vary.len(),
                &levels_now,
                &|n| &ops[drafts[by_level[n] as usize].ops.clone()],
                &node_of,
                &kept,
                crate::fuse::Limits {
                    band: setting("WATERSHED_BAND", 32) as u32,
                    most: setting("WATERSHED_CLUSTER", 256),
                    shapes_most: setting("WATERSHED_SHAPES", 96),
                },
            ),
            false => None,
        };

        let mut sorted = Vec::with_capacity(ops.len());
        let mut programs = Vec::with_capacity(3 * drafts.len());
        let mut operands: Vec<u32> = Vec::new();
        let mut operand_at = Vec::with_capacity(drafts.len());
        let mut levels: Vec<Range<u32>> = Vec::new();
        let mut groups: Vec<Group> = Vec::new();
        let mut level_before = None;
        for (now, &was) in by_level.iter().enumerate() {
            let draft = &drafts[was as usize];
            if operands.len() > u32::MAX as usize {
                return Err(PlanError::TooLarge);
            }
            programs.push(sorted.len() as u32);
            programs.push(draft.ops.len() as u32);
            programs.push(slot_of[now]);
            operand_at.push(operands.len() as u32);
            for &op in &ops[draft.ops.clone()] {
                let op = match op & CODE {
                    LOAD => LOAD | final_slot(op >> CODE_BITS) << CODE_BITS,
                    _ => op,
                };
                if matches!(op & CODE, LOAD | CONST | LOAD_SHARED) {
                    operands.push(op >> CODE_BITS);
                }
                sorted.push(op);
            }
            let now = now as u32;
            let same_level = level_before == Some(draft.level);
            level_before = Some(draft.level);
            match levels.last_mut() {
                Some(last) if same_level => last.end = now + 1,
                _ => levels.push(now..now + 1),
            }
            let shape = shape_of[was as usize];
            match groups.last_mut() {
                Some(last) if same_level && last.shape == shape => last.count += 1,
                _ => groups.push(Group {
                    shape,
                    first: now,
                    count: 1,
                }),
            }
        }

        let fused_outputs = outputs
            .iter()
            .map(|&id| match (b.place(id), &fused) {
                (Some(Place::Slot(slot)), Some(fused)) => Ok(Output::Slot(match node_of(slot) {
                    Some(node) => fused.slot_of[node].expect("an output has a slot"),
                    None => slot,
                })),
                (Some(Place::Slot(slot)), None) => Ok(Output::Slot(final_slot(slot))),
                (Some(Place::Shared(at)), _) => Ok(Output::Shared(at)),
                (None, _) => b.fixed(id).map(Output::Fixed),
            })
            .collect::<Result<_, _>>()?;
        let outputs = outputs
            .iter()
            .map(|&id| match b.place(id) {
                Some(Place::Slot(slot)) => Ok(Output::Slot(final_slot(slot))),
                Some(Place::Shared(at)) => Ok(Output::Shared(at)),
                None => b.fixed(id).map(Output::Fixed),
            })
            .collect::<Result<_, _>>()?;

        let mut plan = Scenarios {
            vary: vary.len(),
            slots: slots as usize,
            programs,
            ops: sorted,
            consts: b.consts,
            levels,
            shapes,
            groups,
            operands,
            operand_at,
            fused,
            fused_outputs,
            outputs,
            shared_inputs: shared.len(),
            shared: Vec::new(),
            shared_programs,
            shared_ops,
        };
        plan.set_shared(&shared_values);
        debug_assert_eq!(
            plan.check(),
            Ok(()),
            "a plan the builder made holds together"
        );
        Ok(plan)
    }

    /// Gives the shared inputs new values, in the order they were given to
    /// [`with_shared`](Self::with_shared). Every run from now on uses them.
    ///
    /// # Panics
    /// If `values` is not one value for each shared input.
    pub fn set_shared(&mut self, values: &[f64]) {
        assert_eq!(
            values.len(),
            self.shared_inputs,
            "one value for each shared input"
        );
        self.shared.clear();
        self.shared.extend_from_slice(values);
        // The nodes that depend only on shared inputs, each after the ones
        // it reads.
        for program in self.shared_programs.chunks_exact(2) {
            let (start, len) = (program[0] as usize, program[1] as usize);
            let ops = &self.shared_ops[start..start + len];
            self.shared.push(eval(ops, &self.consts, &self.shared, &[]));
        }
    }

    /// How many inputs each scenario gives a value for.
    pub fn inputs(&self) -> usize {
        self.vary
    }

    /// How many inputs are shared by every scenario of a run.
    pub fn shared_inputs(&self) -> usize {
        self.shared_inputs
    }

    /// How many outputs each scenario produces.
    pub fn outputs(&self) -> usize {
        self.outputs.len()
    }

    /// How many nodes are calculated for each scenario.
    pub fn nodes(&self) -> usize {
        self.programs.len() / 3
    }

    /// How many levels those nodes are in.
    pub fn levels(&self) -> usize {
        self.levels.len()
    }

    /// The memory one scenario takes while it is being worked out: a
    /// number for each varied input and for as many nodes as are needed at
    /// once, which is far fewer than there are when most are read only by
    /// the next level or two.
    pub fn bytes_per_scenario(&self) -> usize {
        self.slots() * size_of::<f64>()
    }

    pub(crate) fn slots(&self) -> usize {
        self.slots
    }

    /// The memory one scenario takes on the GPU when the nodes are worked
    /// out in clusters, which keep to themselves what nothing else reads.
    pub fn bytes_per_scenario_in_clusters(&self) -> Option<usize> {
        self.fused.as_ref().map(|f| f.slots * size_of::<f64>())
    }

    /// How many clusters the nodes are in for the GPU, and how many
    /// different shapes of cluster there are.
    pub fn clusters(&self) -> Option<(usize, usize)> {
        self.fused
            .as_ref()
            .map(|f| (f.operand_at.len(), f.shapes.len()))
    }

    /// How many different shapes of node there are: nodes whose formulas
    /// differ only in what they read have one shape.
    pub fn shapes(&self) -> usize {
        self.shapes.len()
    }

    /// How many scenarios `inputs` holds.
    ///
    /// # Panics
    /// If its length is not a whole number of scenarios.
    pub(crate) fn scenarios_in(&self, inputs: &[f64]) -> usize {
        assert!(
            inputs.len() % self.vary == 0,
            "{} input values is not a whole number of scenarios of {} inputs each",
            inputs.len(),
            self.vary
        );
        inputs.len() / self.vary
    }

    /// Runs every scenario on the CPU, on all cores.
    ///
    /// `inputs` holds one scenario after another, each a value for every
    /// varied input in the order they were given to [`Scenarios::new`]. The
    /// result holds one scenario after another, each a value for every
    /// output. Each is what the graph gives when the inputs are set to that
    /// scenario's values and it is updated.
    ///
    /// # Panics
    /// If the length of `inputs` is not a whole number of scenarios.
    pub fn run_cpu(&self, inputs: &[f64]) -> Vec<f64> {
        let scenarios = self.scenarios_in(inputs);
        let mut results = vec![0.0; scenarios * self.outputs.len()];
        results
            .par_chunks_mut(self.outputs.len())
            .zip(inputs.par_chunks(self.vary))
            .for_each_init(
                || vec![0.0; self.slots()],
                |vals, (result, scenario)| self.run_one(scenario, vals, result),
            );
        results
    }

    /// Each output added up over every scenario, on the CPU.
    ///
    /// The scenarios are added in blocks of 1,024 and then the blocks, the
    /// order `Loaded::totals` adds them in on the
    /// GPU, so the two give the same totals to the last bit.
    ///
    /// # Panics
    /// If the length of `inputs` is not a whole number of scenarios.
    pub fn totals_cpu(&self, inputs: &[f64]) -> Vec<f64> {
        let results = self.run_cpu(inputs);
        let outputs = self.outputs.len();
        (0..outputs)
            .map(|o| total(results.iter().skip(o).step_by(outputs).copied()))
            .collect()
    }

    fn run_one(&self, scenario: &[f64], vals: &mut [f64], result: &mut [f64]) {
        vals[..self.vary].copy_from_slice(scenario);
        for program in self.programs.chunks_exact(3) {
            let (start, len) = (program[0] as usize, program[1] as usize);
            let ops = &self.ops[start..start + len];
            vals[program[2] as usize] = eval(ops, &self.consts, &self.shared, vals);
        }
        for (out, output) in result.iter_mut().zip(&self.outputs) {
            *out = match *output {
                Output::Slot(slot) => vals[slot as usize],
                Output::Shared(at) => self.shared[at as usize],
                Output::Fixed(x) => x,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sum of `members` members, each a varied input plus a number.
    fn a_sum(members: usize) -> (Graph, NodeId, NodeId) {
        let mut g = Graph::new();
        let x = g.input("x", 1.0).unwrap();
        let yes = g.input("yes", true).unwrap().id();
        let members: Vec<(NodeId, NodeId)> = (0..members)
            .map(|m| (g.define(&format!("m{m}"), x + m as f64).unwrap().id(), yes))
            .collect();
        let sum = g
            .aggregate("sum", AggregateOp::Sum, Type::F64, 0, &members)
            .unwrap();
        g.stabilize();
        (g, x.id(), sum)
    }

    #[test]
    fn every_piece_of_a_large_total_is_short_and_most_are_one_shape() {
        let (g, x, sum) = a_sum(100_000);
        let plan = Scenarios::new(&g, &[x], &[sum]).unwrap();
        // Short enough for a kernel of its own (`cuda.rs` writes one for a
        // shape of up to 1,024 instructions), and few shapes.
        assert!(plan.shapes.iter().all(|shape| shape.len() <= PIECE + 8));
        assert!(plan.shapes.len() <= 8, "{} shapes", plan.shapes.len());
        // No member has a slot: each is worked out in its piece.
        assert!(plan.slots() < 100_000 / 16, "{} slots", plan.slots());
    }

    #[test]
    fn constant_parts_are_worked_out_when_the_plan_is_made() {
        let mut b = Builder {
            g: &Graph::new(),
            places: Vec::new(),
            level_of: vec![0],
            consts: Vec::new(),
            const_of: HashMap::new(),
            vary: 1,
            shared_inputs: 0,
            drafts: Vec::new(),
            ops: Vec::new(),
            shared_programs: Vec::new(),
            shared_ops: Vec::new(),
            inline: Vec::new(),
            inlined: Vec::new(),
            constant_scratch: Vec::new(),
        };
        let c = |b: &mut Builder, x: f64| CONST | b.constant(x).unwrap() << CODE_BITS;
        let x = LOAD;
        // (2 * 3) + x, and (1 > 0 ? x : 5) * 4.
        let ops = vec![c(&mut b, 2.0), c(&mut b, 3.0), MUL, x, ADD];
        let folded = b.folded(ops).unwrap();
        assert_eq!(folded, vec![c(&mut b, 6.0), x, ADD]);
        let ops = vec![
            c(&mut b, 1.0),
            c(&mut b, 0.0),
            GT,
            x,
            c(&mut b, 5.0),
            SELECT,
            c(&mut b, 4.0),
            MUL,
        ];
        let folded = b.folded(ops).unwrap();
        assert_eq!(folded, vec![x, c(&mut b, 4.0), MUL]);
        // Not taken, the other branch is kept whole.
        let ops = vec![c(&mut b, 0.0), x, x, c(&mut b, 1.0), ADD, SELECT];
        let folded = b.folded(ops).unwrap();
        assert_eq!(folded, vec![x, c(&mut b, 1.0), ADD]);
    }
}
