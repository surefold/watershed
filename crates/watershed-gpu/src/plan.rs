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
use watershed_core::{BinOp, Graph, Ir, NodeId, Type, UnOp, Value};

mod file;
pub use file::LoadError;

// The instruction codes. `kernel.cu` has the same numbers; the two must be
// changed together. An instruction is one `u32`: the code in the low five
// bits, and a slot, shared value or constant number in the rest. A bool is
// held as 1.0 or 0.0.
const CODE_BITS: u32 = 5;
const CODE: u32 = (1 << CODE_BITS) - 1;
const LOAD: u32 = 0;
const CONST: u32 = 1;
const ADD: u32 = 2;
const SUB: u32 = 3;
const MUL: u32 = 4;
const DIV: u32 = 5;
const MIN: u32 = 6;
const MAX: u32 = 7;
const NEG: u32 = 8;
const ABS: u32 = 9;
const LOAD_SHARED: u32 = 10;
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

/// Where a load reads from.
#[cfg(feature = "gpu")]
pub(crate) enum Source {
    Slot,
    Const,
    Shared,
}

/// How many values an instruction takes off the stack.
#[cfg(feature = "gpu")]
pub(crate) fn arity(code: u32) -> usize {
    match code {
        LOAD | CONST | LOAD_SHARED => 0,
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
    /// scenarios cannot yet do: a choice, a total, or work on text, whole
    /// numbers or decimals.
    Unsupported { node: String, what: String },
    /// A node that stays the same in every scenario holds an error.
    HoldsError { node: String, error: String },
    /// Pull mode is on and this node is not being calculated, so its value
    /// may be out of date.
    NotCalculated(String),
    /// One node's expression needs a deeper stack than [`MAX_STACK`].
    TooDeep(String),
    /// More nodes or constants than a plan can number.
    TooLarge,
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
    /// provisional until the levels are sorted.
    Slot(u32),
    /// Worked out once for all scenarios.
    Shared(u32),
}

/// A node worked out for every scenario, before the nodes are put in level
/// order.
struct Draft {
    level: u32,
    ops: Range<usize>,
}

/// What an expression leaves on the stack.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Float,
    Bool,
}

struct Builder<'a> {
    g: &'a Graph,
    /// Where each node of the graph is, by its index: 0 if it is the same
    /// always (or not yet met), otherwise a [`Place`] as `mark` writes it.
    places: Vec<u32>,
    /// The level of each slot; a varied input is at level 0.
    level_of: Vec<u32>,
    consts: Vec<f64>,
    const_of: HashMap<u64, u32>,
}

impl Builder<'_> {
    fn name(&self, id: NodeId) -> String {
        self.g.name(id).to_string()
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
    nodes: Vec<RoughNode>,
}

enum RoughNode {
    /// A total, which has members in place of an expression.
    Total,
    /// These of the rough instructions, needing a stack this deep.
    Ops(Range<usize>, usize),
    /// It cannot be done. This matters only if the node turns out to depend
    /// on a varied or shared input.
    Refused(PlanError),
}

/// One node's rough instructions as they are made.
struct Program<'r> {
    g: &'r Graph,
    node: NodeId,
    rough: &'r mut Rough,
    /// The stack in use, and the most it has been.
    depth: usize,
    deepest: usize,
}

impl Program<'_> {
    /// An instruction that leaves one more on the stack.
    fn push(&mut self, op: u32) {
        self.rough.ops.push(op);
        self.depth += 1;
        self.deepest = self.deepest.max(self.depth);
    }

    /// An instruction that takes `takes` off the stack and leaves one.
    fn apply(&mut self, op: u32, takes: usize) {
        self.rough.ops.push(op);
        self.depth -= takes - 1;
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
        self.push(CONST | at << CODE_BITS);
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
            Ir::Const(other) => Err(self.unsupported(match other.type_of() {
                Some(ty) => format!("work on {ty}"),
                None => "an error".into(),
            })),
            Ir::Ref(r) => {
                let kind = kind_of(self.g, r.node)?;
                let at = self.rough.reads.len() as u32;
                self.rough.reads.push(r.node);
                self.push(LOAD | at << CODE_BITS);
                Ok(kind)
            }
            Ir::Unary(op, a) => {
                let kind = self.translate(a)?;
                match (op, kind) {
                    (UnOp::Neg, Kind::Float) => self.apply(NEG, 1),
                    (UnOp::Abs, Kind::Float) => self.apply(ABS, 1),
                    (UnOp::Exp, Kind::Float) => self.apply(EXP, 1),
                    (UnOp::Ln, Kind::Float) => self.apply(LN, 1),
                    (UnOp::Sqrt, Kind::Float) => self.apply(SQRT, 1),
                    (UnOp::Not, Kind::Bool) => self.apply(NOT, 1),
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
                    BinOp::Min => (MIN, Kind::Float),
                    BinOp::Max => (MAX, Kind::Float),
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
                if gives == Kind::Float && (left == Kind::Bool || right == Kind::Bool) {
                    return Err(self.unsupported("arithmetic of a bool"));
                }
                self.apply(code, 2);
                Ok(gives)
            }
            Ir::If(condition, then, otherwise) => {
                self.translate(condition)?;
                let kind = self.translate(then)?;
                self.translate(otherwise)?;
                self.apply(SELECT, 3);
                Ok(kind)
            }
            Ir::Choose(..) => Err(self.unsupported("a choice")),
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
        let mut program = Program {
            g,
            node,
            rough: &mut rough,
            depth: 0,
            deepest: 0,
        };
        let translated = program.translate(&ir);
        let deepest = program.deepest;
        rough.nodes.push(match translated {
            Ok(_) => RoughNode::Ops(start..rough.ops.len(), deepest),
            Err(refused) => {
                rough.ops.truncate(start);
                RoughNode::Refused(refused)
            }
        });
    }
    rough
}

enum Visit {
    Enter(NodeId),
    Leave(NodeId),
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
        Scenarios::cut_at(g, vary, &[], shared, outputs)
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
        let float = |id: NodeId| {
            if id.index() >= g.len() {
                return Err(PlanError::UnknownNode(id));
            }
            match g.node_type(id) {
                Type::F64 => Ok(()),
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
            float(id)?;
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
        for &id in outputs {
            float(id)?;
        }

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
                        g.dependencies(id)
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

        // Now, one node after another, where each reads from. A node that
        // reads nothing varied or shared is the same always and is left out.
        let mut ops: Vec<u32> = Vec::new();
        let mut drafts: Vec<Draft> = Vec::new();
        let mut shared_ops: Vec<u32> = Vec::new();
        let mut shared_programs: Vec<u32> = Vec::new();
        for (nodes, rough) in order.chunks(AT_A_TIME).zip(&roughs) {
            for (&id, node) in nodes.iter().zip(&rough.nodes) {
                let (range, deepest) = match node {
                    RoughNode::Total => {
                        if g.dependencies(id).iter().any(|&d| b.place(d).is_some()) {
                            return Err(PlanError::Unsupported {
                                node: b.name(id),
                                what: "a total".into(),
                            });
                        }
                        continue;
                    }
                    RoughNode::Refused(refused) => {
                        // Whether it matters: does it read anything that is
                        // not the same always?
                        if g.dependencies(id).iter().any(|&d| b.place(d).is_some()) {
                            kind_of(g, id)?;
                            return Err(refused.clone());
                        }
                        continue;
                    }
                    RoughNode::Ops(range, deepest) => (range.clone(), *deepest),
                };
                let reads = |op: &u32| match op & CODE {
                    LOAD => b.place(rough.reads[(op >> CODE_BITS) as usize]),
                    _ => None,
                };
                let mut places = rough.ops[range.clone()].iter().filter_map(reads);
                let every_scenario = places.clone().any(|p| matches!(p, Place::Slot(_)));
                if !every_scenario && places.next().is_none() {
                    continue;
                }
                kind_of(g, id)?;
                if deepest > MAX_STACK {
                    return Err(PlanError::TooDeep(b.name(id)));
                }
                let into = match every_scenario {
                    true => &mut ops,
                    false => &mut shared_ops,
                };
                let start = into.len();
                let mut level = 0;
                for &op in &rough.ops[range] {
                    let at = (op >> CODE_BITS) as usize;
                    into.push(match op & CODE {
                        LOAD => match b.place(rough.reads[at]) {
                            Some(Place::Slot(slot)) => {
                                level = level.max(b.level_of[slot as usize] + 1);
                                LOAD | slot << CODE_BITS
                            }
                            Some(Place::Shared(at)) => LOAD_SHARED | at << CODE_BITS,
                            None => {
                                let x = b.fixed(rough.reads[at])?;
                                CONST | b.constant(x)? << CODE_BITS
                            }
                        },
                        CONST => CONST | b.constant(rough.consts[at])? << CODE_BITS,
                        _ => op,
                    });
                }
                if into.len() > u32::MAX as usize {
                    return Err(PlanError::TooLarge);
                }
                if every_scenario {
                    let slot = vary.len() + drafts.len();
                    if slot > MAX_ARG {
                        return Err(PlanError::TooLarge);
                    }
                    b.mark(id, Place::Slot(slot as u32));
                    b.level_of.push(level);
                    drafts.push(Draft {
                        level,
                        ops: start..ops.len(),
                    });
                } else {
                    let at = shared.len() + shared_programs.len() / 2;
                    if at > MAX_ARG {
                        return Err(PlanError::TooLarge);
                    }
                    b.mark(id, Place::Shared(at as u32));
                    shared_programs.push(start as u32);
                    shared_programs.push((shared_ops.len() - start) as u32);
                }
            }
        }
        drop(roughs);

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
            outputs,
            shared_inputs: shared.len(),
            shared: Vec::new(),
            shared_programs,
            shared_ops,
        };
        plan.set_shared(&shared_values);
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
