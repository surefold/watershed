//! Compiles an expression tree into a flat list of instructions.
//!
//! Evaluating the tree directly ([`Ir::eval`]) means following pointers and
//! checking every operand's type at every step. Because an expression has
//! already been type-checked when it is compiled, the compiler can choose an
//! instruction for the exact types involved ("add two f64s"), and the
//! instructions sit next to each other in memory. The tree evaluator is kept as
//! the simple reference: the tests check that both always give the same answer.

use std::sync::Arc;

use crate::handle::{GraphId, NodeId, NodeRef};
use crate::ir::{BinOp, Ir, UnOp};
use crate::value::{Type, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Op {
    /// Push the value of a node.
    Load(u32),
    /// Push a constant.
    Const(u32),
    AddF,
    SubF,
    MulF,
    DivF,
    MinF,
    MaxF,
    NegF,
    AbsF,
    AddI,
    SubI,
    MulI,
    DivI,
    MinI,
    MaxI,
    NegI,
    AbsI,
    CmpF(Cmp),
    CmpI(Cmp),
    CmpS(Cmp),
    CmpB(Cmp),
    And,
    Or,
    Not,
    /// Pop a condition. True: carry on. False: jump to `otherwise`. An error:
    /// push it and jump to `end`, skipping both branches.
    Branch {
        otherwise: u32,
        end: u32,
    },
    Jump(u32),
}

/// Instructions for the fast path: arithmetic on `f64` only.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FloatOp {
    Load(u32),
    /// An index into the graph's pool of float constants, which keeps each
    /// instruction to 8 bytes.
    Const(u32),
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Neg,
    Abs,
}

/// A compiled expression.
#[derive(Debug, Clone)]
pub(crate) enum Program {
    /// Pure `f64` arithmetic, with no conditionals, stored at
    /// `float_ops[start..start + len]` in the arrays passed to [`compile`].
    /// Arithmetic on floats cannot fail, so the only possible error is one
    /// held by a node it reads, and the evaluator returns the first such
    /// error in reading order, exactly as evaluating the tree would.
    Float { start: u32, len: u32 },
    /// Everything else.
    General(GeneralProgram),
}

#[derive(Debug, Clone)]
pub(crate) struct GeneralProgram {
    ops: Box<[Op]>,
    constants: Box<[Value]>,
}

impl GeneralProgram {
    #[inline]
    pub(crate) fn eval(&self, values: &[Value], scratch: &mut Scratch) -> Value {
        eval_general(&self.ops, &self.constants, values, &mut scratch.values)
    }

    /// Every node the program reads (possibly with repeats).
    pub(crate) fn loads(&self, out: &mut Vec<NodeId>) {
        out.extend(self.ops.iter().filter_map(|op| match op {
            Op::Load(n) => Some(NodeId(*n)),
            _ => None,
        }));
    }

    /// Rebuilds the expression this program was compiled from.
    pub(crate) fn decompile(&self, graph: GraphId) -> Ir {
        decompile_general(&self.ops, &self.constants, graph)
    }
}

/// Scratch space for evaluating programs, reused so evaluating allocates nothing.
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    values: Vec<Value>,
    floats: Vec<f64>,
}

/// Compiles a type-checked expression. `type_of` gives each node's type. A
/// pure-float program is appended to `float_ops` and `float_consts`.
pub(crate) fn compile(
    ir: &Ir,
    type_of: &dyn Fn(usize) -> Type,
    float_ops: &mut Vec<FloatOp>,
    float_consts: &mut Vec<f64>,
) -> Program {
    let (ops_before, consts_before) = (float_ops.len(), float_consts.len());
    if compile_float(ir, type_of, float_ops, float_consts) {
        let start = u32::try_from(ops_before).expect("too many instructions in one graph");
        let len = u32::try_from(float_ops.len() - ops_before).expect("expression too long");
        return Program::Float { start, len };
    }
    // Not pure float arithmetic: undo what was appended and compile it generally.
    float_ops.truncate(ops_before);
    float_consts.truncate(consts_before);
    let mut c = Compiler {
        ops: Vec::new(),
        constants: Vec::new(),
        type_of,
    };
    c.emit(ir);
    Program::General(GeneralProgram {
        ops: c.ops.into(),
        constants: c.constants.into(),
    })
}

/// Every node a float program reads (possibly with repeats).
pub(crate) fn float_loads(ops: &[FloatOp], out: &mut Vec<NodeId>) {
    out.extend(ops.iter().filter_map(|op| match op {
        FloatOp::Load(n) => Some(NodeId(*n)),
        _ => None,
    }));
}

/// Rebuilds the expression a float program was compiled from.
pub(crate) fn decompile_float(ops: &[FloatOp], consts: &[f64], graph: GraphId) -> Ir {
    let mut stack: Vec<Ir> = Vec::new();
    for op in ops {
        let ir = match *op {
            FloatOp::Load(n) => Ir::Ref(NodeRef {
                graph,
                node: NodeId(n),
            }),
            FloatOp::Const(i) => Ir::Const(Value::F64(consts[i as usize])),
            FloatOp::Neg | FloatOp::Abs => {
                let a = stack.pop().expect("well-formed program");
                let op = if matches!(op, FloatOp::Neg) {
                    UnOp::Neg
                } else {
                    UnOp::Abs
                };
                Ir::Unary(op, Arc::new(a))
            }
            _ => {
                let b = stack.pop().expect("well-formed program");
                let a = stack.pop().expect("well-formed program");
                let op = match op {
                    FloatOp::Add => BinOp::Add,
                    FloatOp::Sub => BinOp::Sub,
                    FloatOp::Mul => BinOp::Mul,
                    FloatOp::Div => BinOp::Div,
                    FloatOp::Min => BinOp::Min,
                    _ => BinOp::Max,
                };
                Ir::Binary(op, Arc::new(a), Arc::new(b))
            }
        };
        stack.push(ir);
    }
    stack.pop().expect("well-formed program")
}

fn decompile_general(ops: &[Op], constants: &[Value], graph: GraphId) -> Ir {
    // Jumps are absolute positions in `ops`, so work on ranges of the whole list.
    fn range(ops: &[Op], constants: &[Value], graph: GraphId, mut pc: usize, end: usize) -> Ir {
        let mut stack: Vec<Ir> = Vec::new();
        while pc < end {
            match ops[pc] {
                Op::Branch {
                    otherwise,
                    end: done,
                } => {
                    let condition = stack.pop().expect("well-formed program");
                    // The `then` branch ends with a jump to `done`, just before `otherwise`.
                    let then = range(ops, constants, graph, pc + 1, otherwise as usize - 1);
                    let other = range(ops, constants, graph, otherwise as usize, done as usize);
                    stack.push(Ir::If(Arc::new(condition), Arc::new(then), Arc::new(other)));
                    pc = done as usize;
                    continue;
                }
                Op::Load(n) => stack.push(Ir::Ref(NodeRef {
                    graph,
                    node: NodeId(n),
                })),
                Op::Const(i) => stack.push(Ir::Const(constants[i as usize].clone())),
                Op::Jump(_) => unreachable!("jumps only end a `then` branch"),
                op => {
                    let unary = match op {
                        Op::NegF | Op::NegI => Some(UnOp::Neg),
                        Op::AbsF | Op::AbsI => Some(UnOp::Abs),
                        Op::Not => Some(UnOp::Not),
                        _ => None,
                    };
                    if let Some(u) = unary {
                        let a = stack.pop().expect("well-formed program");
                        stack.push(Ir::Unary(u, Arc::new(a)));
                    } else {
                        let b = stack.pop().expect("well-formed program");
                        let a = stack.pop().expect("well-formed program");
                        let binary = match op {
                            Op::AddF | Op::AddI => BinOp::Add,
                            Op::SubF | Op::SubI => BinOp::Sub,
                            Op::MulF | Op::MulI => BinOp::Mul,
                            Op::DivF | Op::DivI => BinOp::Div,
                            Op::MinF | Op::MinI => BinOp::Min,
                            Op::MaxF | Op::MaxI => BinOp::Max,
                            Op::And => BinOp::And,
                            Op::Or => BinOp::Or,
                            Op::CmpF(c) | Op::CmpI(c) | Op::CmpS(c) | Op::CmpB(c) => match c {
                                Cmp::Lt => BinOp::Lt,
                                Cmp::Le => BinOp::Le,
                                Cmp::Gt => BinOp::Gt,
                                Cmp::Ge => BinOp::Ge,
                                Cmp::Eq => BinOp::Eq,
                                Cmp::Ne => BinOp::Ne,
                            },
                            _ => unreachable!("every other instruction is handled above"),
                        };
                        stack.push(Ir::Binary(binary, Arc::new(a), Arc::new(b)));
                    }
                }
            }
            pc += 1;
        }
        stack.pop().expect("well-formed program")
    }
    range(ops, constants, graph, 0, ops.len())
}

/// Emits `ir` as float instructions, if it is pure `f64` arithmetic.
fn compile_float(
    ir: &Ir,
    type_of: &dyn Fn(usize) -> Type,
    out: &mut Vec<FloatOp>,
    consts: &mut Vec<f64>,
) -> bool {
    match ir {
        Ir::Const(Value::F64(x)) => {
            let i = u32::try_from(consts.len()).expect("too many constants in one graph");
            consts.push(*x);
            out.push(FloatOp::Const(i));
        }
        Ir::Ref(r) if type_of(r.node.index()) == Type::F64 => out.push(FloatOp::Load(r.node.0)),
        Ir::Unary(op @ (UnOp::Neg | UnOp::Abs), a) => {
            if !compile_float(a, type_of, out, consts) {
                return false;
            }
            out.push(if *op == UnOp::Neg {
                FloatOp::Neg
            } else {
                FloatOp::Abs
            });
        }
        Ir::Binary(
            op @ (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Min | BinOp::Max),
            a,
            b,
        ) => {
            if !compile_float(a, type_of, out, consts) || !compile_float(b, type_of, out, consts) {
                return false;
            }
            out.push(match op {
                BinOp::Add => FloatOp::Add,
                BinOp::Sub => FloatOp::Sub,
                BinOp::Mul => FloatOp::Mul,
                BinOp::Div => FloatOp::Div,
                BinOp::Min => FloatOp::Min,
                _ => FloatOp::Max,
            });
        }
        _ => return false,
    }
    true
}

#[inline]
pub(crate) fn eval_float(
    ops: &[FloatOp],
    consts: &[f64],
    values: &[Value],
    scratch: &mut Scratch,
) -> Value {
    let stack = &mut scratch.floats;
    stack.clear();
    for op in ops {
        match *op {
            FloatOp::Load(i) => match &values[i as usize] {
                Value::F64(x) => stack.push(*x),
                // The first error read is the answer: nothing before it
                // failed, and arithmetic after it cannot replace it.
                Value::Error(e) => return Value::Error(e.clone()),
                other => return internal(format!("float program read {other}")),
            },
            FloatOp::Const(i) => stack.push(consts[i as usize]),
            FloatOp::Neg | FloatOp::Abs => {
                let Some(x) = stack.last_mut() else {
                    return internal("stack underflow".into());
                };
                *x = if matches!(op, FloatOp::Neg) {
                    -*x
                } else {
                    x.abs()
                };
            }
            _ => {
                let (Some(y), Some(x)) = (stack.pop(), stack.last_mut()) else {
                    return internal("stack underflow".into());
                };
                *x = match op {
                    FloatOp::Add => *x + y,
                    FloatOp::Sub => *x - y,
                    FloatOp::Mul => *x * y,
                    FloatOp::Div => *x / y,
                    FloatOp::Min => x.min(y),
                    _ => x.max(y),
                };
            }
        }
    }
    debug_assert_eq!(stack.len(), 1, "a program leaves exactly one value");
    stack
        .pop()
        .map_or_else(|| internal("empty stack".into()), Value::F64)
}

fn eval_general(
    ops: &[Op],
    constants: &[Value],
    values: &[Value],
    stack: &mut Vec<Value>,
) -> Value {
    stack.clear();
    let mut pc = 0;
    while pc < ops.len() {
        let op = ops[pc];
        pc += 1;
        match op {
            Op::Load(i) => stack.push(values[i as usize].clone()),
            Op::Const(i) => stack.push(constants[i as usize].clone()),

            Op::AddF => float(stack, |x, y| x + y),
            Op::SubF => float(stack, |x, y| x - y),
            Op::MulF => float(stack, |x, y| x * y),
            Op::DivF => float(stack, |x, y| x / y),
            Op::MinF => float(stack, f64::min),
            Op::MaxF => float(stack, f64::max),
            Op::NegF => unary(stack, |v| match v {
                Value::F64(x) => Value::F64(-x),
                other => other,
            }),
            Op::AbsF => unary(stack, |v| match v {
                Value::F64(x) => Value::F64(x.abs()),
                other => other,
            }),

            Op::AddI => int(stack, |x, y| x.checked_add(y).ok_or(OVERFLOW)),
            Op::SubI => int(stack, |x, y| x.checked_sub(y).ok_or(OVERFLOW)),
            Op::MulI => int(stack, |x, y| x.checked_mul(y).ok_or(OVERFLOW)),
            Op::DivI => int(stack, |x, y| match y {
                0 => Err("division by zero"),
                _ => x.checked_div(y).ok_or(OVERFLOW),
            }),
            Op::MinI => int(stack, |x, y| Ok(x.min(y))),
            Op::MaxI => int(stack, |x, y| Ok(x.max(y))),
            Op::NegI => unary(stack, |v| match v {
                Value::I64(x) => x
                    .checked_neg()
                    .map_or_else(|| Value::error("integer overflow"), Value::I64),
                other => other,
            }),
            Op::AbsI => unary(stack, |v| match v {
                Value::I64(x) => x
                    .checked_abs()
                    .map_or_else(|| Value::error("integer overflow"), Value::I64),
                other => other,
            }),

            Op::CmpF(cmp) | Op::CmpI(cmp) | Op::CmpS(cmp) | Op::CmpB(cmp) => compare(stack, cmp),

            Op::And => logic(stack, |x, y| x && y),
            Op::Or => logic(stack, |x, y| x || y),
            Op::Not => unary(stack, |v| match v {
                Value::Bool(x) => Value::Bool(!x),
                other => other,
            }),

            Op::Branch { otherwise, end } => match stack.pop() {
                Some(Value::Bool(true)) => {}
                Some(Value::Bool(false)) => pc = otherwise as usize,
                Some(err @ Value::Error(_)) => {
                    stack.push(err);
                    pc = end as usize;
                }
                other => return internal(format!("condition evaluated to {other:?}")),
            },
            Op::Jump(to) => pc = to as usize,
        }
    }
    debug_assert_eq!(stack.len(), 1, "a program leaves exactly one value");
    stack
        .pop()
        .unwrap_or_else(|| internal("empty stack".into()))
}

struct Compiler<'a> {
    ops: Vec<Op>,
    constants: Vec<Value>,
    type_of: &'a dyn Fn(usize) -> Type,
}

impl Compiler<'_> {
    /// The type of a type-checked expression.
    fn ty(&self, ir: &Ir) -> Type {
        let type_of = self.type_of;
        ir.type_check(&|r| Ok(type_of(r.node.index())))
            .expect("only type-checked expressions are compiled")
    }

    fn here(&self) -> u32 {
        self.ops.len() as u32
    }

    fn emit(&mut self, ir: &Ir) {
        match ir {
            Ir::Const(v) => {
                self.constants.push(v.clone());
                self.ops.push(Op::Const(self.constants.len() as u32 - 1));
            }
            Ir::Ref(r) => self.ops.push(Op::Load(r.node.0)),
            Ir::Unary(op, a) => {
                let float = self.ty(a) == Type::F64;
                self.emit(a);
                self.ops.push(match (op, float) {
                    (UnOp::Neg, true) => Op::NegF,
                    (UnOp::Neg, false) => Op::NegI,
                    (UnOp::Abs, true) => Op::AbsF,
                    (UnOp::Abs, false) => Op::AbsI,
                    (UnOp::Not, _) => Op::Not,
                });
            }
            Ir::Binary(op, a, b) => {
                let ty = self.ty(a);
                self.emit(a);
                self.emit(b);
                let cmp = |c| match ty {
                    Type::F64 => Op::CmpF(c),
                    Type::I64 => Op::CmpI(c),
                    Type::Str => Op::CmpS(c),
                    Type::Bool => Op::CmpB(c),
                };
                let f = ty == Type::F64;
                self.ops.push(match op {
                    BinOp::Add => {
                        if f {
                            Op::AddF
                        } else {
                            Op::AddI
                        }
                    }
                    BinOp::Sub => {
                        if f {
                            Op::SubF
                        } else {
                            Op::SubI
                        }
                    }
                    BinOp::Mul => {
                        if f {
                            Op::MulF
                        } else {
                            Op::MulI
                        }
                    }
                    BinOp::Div => {
                        if f {
                            Op::DivF
                        } else {
                            Op::DivI
                        }
                    }
                    BinOp::Min => {
                        if f {
                            Op::MinF
                        } else {
                            Op::MinI
                        }
                    }
                    BinOp::Max => {
                        if f {
                            Op::MaxF
                        } else {
                            Op::MaxI
                        }
                    }
                    BinOp::Lt => cmp(Cmp::Lt),
                    BinOp::Le => cmp(Cmp::Le),
                    BinOp::Gt => cmp(Cmp::Gt),
                    BinOp::Ge => cmp(Cmp::Ge),
                    BinOp::Eq => cmp(Cmp::Eq),
                    BinOp::Ne => cmp(Cmp::Ne),
                    BinOp::And => Op::And,
                    BinOp::Or => Op::Or,
                });
            }
            Ir::If(c, t, e) => {
                self.emit(c);
                let branch_at = self.ops.len();
                self.ops.push(Op::Branch {
                    otherwise: 0,
                    end: 0,
                }); // patched below
                self.emit(t);
                let jump_at = self.ops.len();
                self.ops.push(Op::Jump(0)); // patched below
                let otherwise = self.here();
                self.emit(e);
                let end = self.here();
                self.ops[branch_at] = Op::Branch { otherwise, end };
                self.ops[jump_at] = Op::Jump(end);
            }
        }
    }
}

const OVERFLOW: &str = "integer overflow";

fn internal(message: String) -> Value {
    debug_assert!(false, "internal type error: {message}");
    Value::error(format!("internal type error: {message}"))
}

/// Pops two operands; an error in either (the left one first) is the result.
#[inline(always)]
fn pop2(stack: &mut Vec<Value>) -> Result<(Value, Value), Value> {
    let b = stack.pop();
    let a = stack.pop();
    match (a, b) {
        (Some(Value::Error(e)), _) | (_, Some(Value::Error(e))) => Err(Value::Error(e)),
        (Some(a), Some(b)) => Ok((a, b)),
        _ => Err(internal("stack underflow".into())),
    }
}

#[inline(always)]
fn float(stack: &mut Vec<Value>, f: impl Fn(f64, f64) -> f64) {
    let result = match pop2(stack) {
        Ok((Value::F64(x), Value::F64(y))) => Value::F64(f(x, y)),
        Ok((a, b)) => internal(format!("float operation on {a} and {b}")),
        Err(e) => e,
    };
    stack.push(result);
}

#[inline(always)]
fn int(stack: &mut Vec<Value>, f: impl Fn(i64, i64) -> Result<i64, &'static str>) {
    let result = match pop2(stack) {
        Ok((Value::I64(x), Value::I64(y))) => f(x, y).map_or_else(Value::error, Value::I64),
        Ok((a, b)) => internal(format!("integer operation on {a} and {b}")),
        Err(e) => e,
    };
    stack.push(result);
}

#[inline(always)]
fn logic(stack: &mut Vec<Value>, f: impl Fn(bool, bool) -> bool) {
    let result = match pop2(stack) {
        Ok((Value::Bool(x), Value::Bool(y))) => Value::Bool(f(x, y)),
        Ok((a, b)) => internal(format!("logic operation on {a} and {b}")),
        Err(e) => e,
    };
    stack.push(result);
}

#[inline(always)]
fn unary(stack: &mut Vec<Value>, f: impl Fn(Value) -> Value) {
    let result = match stack.pop() {
        Some(Value::Error(e)) => Value::Error(e),
        Some(v) => f(v),
        None => internal("stack underflow".into()),
    };
    stack.push(result);
}

#[inline(always)]
fn compare(stack: &mut Vec<Value>, cmp: Cmp) {
    use std::cmp::Ordering;
    let result = match pop2(stack) {
        Ok((a, b)) => {
            let ordering: Option<Ordering> = match (&a, &b) {
                (Value::F64(x), Value::F64(y)) => x.partial_cmp(y),
                (Value::I64(x), Value::I64(y)) => Some(x.cmp(y)),
                (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
                (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
                _ => {
                    stack.push(internal(format!("comparison of {a} and {b}")));
                    return;
                }
            };
            Value::Bool(match cmp {
                Cmp::Lt => ordering == Some(Ordering::Less),
                Cmp::Le => matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
                Cmp::Gt => ordering == Some(Ordering::Greater),
                Cmp::Ge => matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
                Cmp::Eq => ordering == Some(Ordering::Equal),
                Cmp::Ne => ordering != Some(Ordering::Equal),
            })
        }
        Err(e) => e,
    };
    stack.push(result);
}
