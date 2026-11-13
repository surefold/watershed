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

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;

use rayon::prelude::*;
use watershed_core::{BinOp, Graph, Ir, NodeId, Type, UnOp, Value};

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

/// A graph prepared to run for many sets of inputs.
///
/// It is a copy: later changes to the graph do not reach it. Values of nodes
/// that depend on neither a varied nor a shared input are taken as they are
/// when the plan is made, so call [`Graph::stabilize`] first.
#[derive(Debug, Clone)]
pub struct Scenarios {
    /// How many inputs are varied. They take slots `0..vary`; calculated node
    /// `n` takes slot `vary + n`.
    pub(crate) vary: usize,
    /// Per calculated node: where its instructions start in `ops`, and how many.
    pub(crate) programs: Vec<u32>,
    pub(crate) ops: Vec<u32>,
    pub(crate) consts: Vec<f64>,
    /// The calculated nodes of each level, lowest first.
    pub(crate) levels: Vec<Range<u32>>,
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
    place_of: HashMap<NodeId, Place>,
    /// The level of each slot; a varied input is at level 0.
    level_of: Vec<u32>,
    consts: Vec<f64>,
    const_of: HashMap<u64, u32>,
}

/// One node's instructions as they are made.
struct Program<'o> {
    node: NodeId,
    ops: &'o mut Vec<u32>,
    /// The stack in use, and the most it has been.
    depth: usize,
    deepest: usize,
    /// One more than the highest level read.
    level: u32,
}

impl Program<'_> {
    /// An instruction that leaves one more on the stack.
    fn push(&mut self, op: u32) {
        self.ops.push(op);
        self.depth += 1;
        self.deepest = self.deepest.max(self.depth);
    }
    /// An instruction that takes `takes` off the stack and leaves one.
    fn apply(&mut self, op: u32, takes: usize) {
        self.ops.push(op);
        self.depth -= takes - 1;
    }
}

impl Builder<'_> {
    fn name(&self, id: NodeId) -> String {
        self.g.name(id).to_string()
    }

    fn unsupported(&self, node: NodeId, what: impl Into<String>) -> PlanError {
        PlanError::Unsupported {
            node: self.name(node),
            what: what.into(),
        }
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
    fn fixed(&self, id: NodeId) -> Result<(f64, Kind), PlanError> {
        if !self.g.is_calculated(id) {
            return Err(PlanError::NotCalculated(self.name(id)));
        }
        match self.g.value(id) {
            Value::F64(x) => Ok((*x, Kind::Float)),
            Value::Bool(x) => Ok((f64::from(u8::from(*x)), Kind::Bool)),
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

    /// The kind of a node that is not the same always.
    fn kind_of(&self, id: NodeId) -> Result<Kind, PlanError> {
        match self.g.node_type(id) {
            Type::F64 => Ok(Kind::Float),
            Type::Bool => Ok(Kind::Bool),
            ty => Err(PlanError::NotAFloat {
                node: self.name(id),
                ty,
            }),
        }
    }

    /// Adds the instructions for `ir` to the program, and says what they
    /// leave on the stack.
    ///
    /// Both branches of a condition are worked out and one is kept. That
    /// gives what the graph gives, which works out only the branch taken,
    /// because nothing here can fail: there are no errors to meet in the
    /// branch not taken.
    fn translate(&mut self, p: &mut Program, ir: &Ir) -> Result<Kind, PlanError> {
        let node = p.node;
        let floats = |b: &Self, kind: Kind, what: &str| match kind {
            Kind::Float => Ok(()),
            Kind::Bool => Err(b.unsupported(node, format!("{what} of a bool"))),
        };
        match ir {
            Ir::Const(Value::F64(x)) => {
                let at = self.constant(*x)?;
                p.push(CONST | at << CODE_BITS);
                Ok(Kind::Float)
            }
            Ir::Const(Value::Bool(x)) => {
                let at = self.constant(f64::from(u8::from(*x)))?;
                p.push(CONST | at << CODE_BITS);
                Ok(Kind::Bool)
            }
            Ir::Const(other) => Err(self.unsupported(
                p.node,
                match other.type_of() {
                    Some(ty) => format!("work on {ty}"),
                    None => "an error".into(),
                },
            )),
            Ir::Ref(r) => match self.place_of.get(&r.node).copied() {
                Some(Place::Slot(slot)) => {
                    p.level = p.level.max(self.level_of[slot as usize] + 1);
                    p.push(LOAD | slot << CODE_BITS);
                    self.kind_of(r.node)
                }
                Some(Place::Shared(at)) => {
                    p.push(LOAD_SHARED | at << CODE_BITS);
                    self.kind_of(r.node)
                }
                None => {
                    let (x, kind) = self.fixed(r.node)?;
                    let at = self.constant(x)?;
                    p.push(CONST | at << CODE_BITS);
                    Ok(kind)
                }
            },
            Ir::Unary(op, a) => {
                let kind = self.translate(p, a)?;
                match (op, kind) {
                    (UnOp::Neg, Kind::Float) => p.apply(NEG, 1),
                    (UnOp::Abs, Kind::Float) => p.apply(ABS, 1),
                    (UnOp::Exp, Kind::Float) => p.apply(EXP, 1),
                    (UnOp::Ln, Kind::Float) => p.apply(LN, 1),
                    (UnOp::Sqrt, Kind::Float) => p.apply(SQRT, 1),
                    (UnOp::Not, Kind::Bool) => p.apply(NOT, 1),
                    (other, _) => return Err(self.unsupported(p.node, format!("a {other:?}"))),
                }
                Ok(kind)
            }
            Ir::Binary(op, a, b) => {
                let left = self.translate(p, a)?;
                let right = self.translate(p, b)?;
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
                    other => return Err(self.unsupported(p.node, format!("a {other:?}"))),
                };
                // Arithmetic is on numbers; comparing is of either, since
                // false is below true as 0.0 is below 1.0. `And` and `Or`
                // are of bools, which the graph's own type check has seen to.
                if gives == Kind::Float {
                    floats(self, left, "arithmetic")?;
                    floats(self, right, "arithmetic")?;
                }
                p.apply(code, 2);
                Ok(gives)
            }
            Ir::If(condition, then, otherwise) => {
                self.translate(p, condition)?;
                let kind = self.translate(p, then)?;
                self.translate(p, otherwise)?;
                p.apply(SELECT, 3);
                Ok(kind)
            }
            Ir::Choose(..) => Err(self.unsupported(p.node, "a choice")),
            Ir::List(_) => Err(self.unsupported(p.node, "work on a list")),
        }
    }
}

enum Visit {
    Enter(NodeId),
    Leave(NodeId, Option<Ir>),
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
            place_of: HashMap::new(),
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
            if !g.is_input(id) {
                return Err(PlanError::NotAnInput(b.name(id)));
            }
            if b.place_of.insert(id, place).is_some() {
                return Err(PlanError::Repeated(b.name(id)));
            }
            if matches!(place, Place::Shared(_)) {
                shared_values.push(b.fixed(id)?.0);
            }
        }
        for &id in outputs {
            float(id)?;
        }

        // Every calculated node the outputs depend on, each after the nodes
        // it uses. This follows every branch of a condition, taken or not.
        let mut order: Vec<(NodeId, Option<Ir>)> = Vec::new();
        let mut seen: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<Visit> = outputs.iter().map(|&id| Visit::Enter(id)).collect();
        let mut refs = Vec::new();
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Enter(id) => {
                    if !seen.insert(id) || g.is_input(id) {
                        continue;
                    }
                    let ir = g.expression(id);
                    let uses: Vec<NodeId> = match &ir {
                        Some(ir) => {
                            refs.clear();
                            ir.refs(&mut refs);
                            refs.iter().map(|r| r.node).collect()
                        }
                        // A total, which has members in place of an expression.
                        None => g.dependencies(id),
                    };
                    stack.push(Visit::Leave(id, ir));
                    stack.extend(
                        uses.into_iter()
                            .filter(|u| !seen.contains(u))
                            .map(Visit::Enter),
                    );
                }
                Visit::Leave(id, ir) => order.push((id, ir)),
            }
        }

        // Turn each node that is not the same always into instructions.
        let mut ops: Vec<u32> = Vec::new();
        let mut drafts: Vec<Draft> = Vec::new();
        let mut shared_ops: Vec<u32> = Vec::new();
        let mut shared_programs: Vec<u32> = Vec::new();
        for (id, ir) in order {
            let Some(ir) = ir else {
                if g.dependencies(id)
                    .iter()
                    .any(|d| b.place_of.contains_key(d))
                {
                    return Err(b.unsupported(id, "a total"));
                }
                continue;
            };
            refs.clear();
            ir.refs(&mut refs);
            let mut reads = refs.iter().filter_map(|r| b.place_of.get(&r.node));
            let every_scenario = reads.clone().any(|p| matches!(p, Place::Slot(_)));
            if !every_scenario && reads.next().is_none() {
                continue;
            }
            b.kind_of(id)?;
            let into = match every_scenario {
                true => &mut ops,
                false => &mut shared_ops,
            };
            let start = into.len();
            let mut program = Program {
                node: id,
                ops: into,
                depth: 0,
                deepest: 0,
                level: 0,
            };
            b.translate(&mut program, &ir)?;
            let (deepest, level) = (program.deepest, program.level);
            if deepest > MAX_STACK {
                return Err(PlanError::TooDeep(b.name(id)));
            }
            if into.len() > u32::MAX as usize {
                return Err(PlanError::TooLarge);
            }
            if every_scenario {
                let slot = vary.len() + drafts.len();
                if slot > MAX_ARG {
                    return Err(PlanError::TooLarge);
                }
                b.place_of.insert(id, Place::Slot(slot as u32));
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
                b.place_of.insert(id, Place::Shared(at as u32));
                shared_programs.push(start as u32);
                shared_programs.push((shared_ops.len() - start) as u32);
            }
        }

        // Put the nodes in level order, and point every load at the slot its
        // node ends up in.
        let mut by_level: Vec<u32> = (0..drafts.len() as u32).collect();
        by_level.sort_by_key(|&n| drafts[n as usize].level);
        let mut moved_to = vec![0u32; drafts.len()];
        for (now, &was) in by_level.iter().enumerate() {
            moved_to[was as usize] = now as u32;
        }
        let final_slot = |slot: u32| match (slot as usize).checked_sub(vary.len()) {
            Some(n) => vary.len() as u32 + moved_to[n],
            None => slot,
        };
        let mut sorted = Vec::with_capacity(ops.len());
        let mut programs = Vec::with_capacity(2 * drafts.len());
        let mut levels: Vec<Range<u32>> = Vec::new();
        for (now, &was) in by_level.iter().enumerate() {
            let draft = &drafts[was as usize];
            programs.push(sorted.len() as u32);
            programs.push(draft.ops.len() as u32);
            for &op in &ops[draft.ops.clone()] {
                sorted.push(match op & CODE {
                    LOAD => LOAD | final_slot(op >> CODE_BITS) << CODE_BITS,
                    _ => op,
                });
            }
            let now = now as u32;
            match levels.last_mut() {
                Some(last)
                    if drafts[by_level[last.start as usize] as usize].level == draft.level =>
                {
                    last.end = now + 1;
                }
                _ => levels.push(now..now + 1),
            }
        }

        let outputs = outputs
            .iter()
            .map(|id| match b.place_of.get(id) {
                Some(&Place::Slot(slot)) => Ok(Output::Slot(final_slot(slot))),
                Some(&Place::Shared(at)) => Ok(Output::Shared(at)),
                None => b.fixed(*id).map(|(x, _)| Output::Fixed(x)),
            })
            .collect::<Result<_, _>>()?;

        let mut plan = Scenarios {
            vary: vary.len(),
            programs,
            ops: sorted,
            consts: b.consts,
            levels,
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
        self.programs.len() / 2
    }

    /// How many levels those nodes are in.
    pub fn levels(&self) -> usize {
        self.levels.len()
    }

    /// The memory one scenario takes while it is being worked out.
    pub fn bytes_per_scenario(&self) -> usize {
        self.slots() * size_of::<f64>()
    }

    pub(crate) fn slots(&self) -> usize {
        self.vary + self.nodes()
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

    fn run_one(&self, scenario: &[f64], vals: &mut [f64], result: &mut [f64]) {
        vals[..self.vary].copy_from_slice(scenario);
        for (node, program) in self.programs.chunks_exact(2).enumerate() {
            let (start, len) = (program[0] as usize, program[1] as usize);
            let ops = &self.ops[start..start + len];
            vals[self.vary + node] = eval(ops, &self.consts, &self.shared, vals);
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
