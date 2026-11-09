//! The expression IR: what a calculated node computes, as plain data.
//!
//! An expression never holds a Rust closure. That is what lets it be
//! type-checked, explained, and (later) serialised and run elsewhere. The
//! typed API in [`expr`](crate::expr) builds these trees; other front ends can
//! build them directly and pass them to [`Graph::define_ir`](crate::Graph::define_ir),
//! which checks them before accepting them.

use std::fmt;
use std::sync::Arc;

use crate::handle::{NodeId, NodeRef};
use crate::value::{Type, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
    Abs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Min,
    Max,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub enum Ir {
    Const(Value),
    Ref(NodeRef),
    Unary(UnOp, Arc<Ir>),
    Binary(BinOp, Arc<Ir>, Arc<Ir>),
    /// `If(condition, then, otherwise)`. Only the chosen branch is evaluated.
    If(Arc<Ir>, Arc<Ir>, Arc<Ir>),
    /// `Choose(index, options)`: the option at position `index` (an `i64`,
    /// counting from 0). Only that option is evaluated; an index out of range
    /// is an error.
    Choose(Arc<Ir>, Vec<Arc<Ir>>),
}

/// The error for a `Choose` index outside its options. Shared by both
/// evaluators so they always agree.
pub(crate) fn out_of_range(index: i64, options: usize) -> Value {
    Value::error(format!(
        "choice {index} is out of range: there are {options} options, numbered from 0"
    ))
}

/// An expression whose types do not fit together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeError(pub String);

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Ir {
    /// Works out the type of this expression, or says why it has none.
    /// `type_of_node` gives the type of each node the expression refers to.
    pub fn type_check(
        &self,
        type_of_node: &dyn Fn(NodeRef) -> Result<Type, TypeError>,
    ) -> Result<Type, TypeError> {
        match self {
            Ir::Const(v) => v
                .type_of()
                .ok_or_else(|| TypeError("an error cannot be used as a constant".into())),
            Ir::Ref(r) => type_of_node(*r),
            Ir::Unary(op, a) => {
                let a = a.type_check(type_of_node)?;
                match op {
                    UnOp::Neg | UnOp::Abs if a.is_numeric() => Ok(a),
                    UnOp::Not if a == Type::Bool => Ok(Type::Bool),
                    _ => Err(TypeError(format!("{op:?} cannot be applied to {a}"))),
                }
            }
            Ir::Binary(op, a, b) => {
                let a = a.type_check(type_of_node)?;
                let b = b.type_check(type_of_node)?;
                if a != b {
                    return Err(TypeError(format!(
                        "{op:?} needs both sides to have the same type, got {a} and {b}"
                    )));
                }
                use BinOp::*;
                match op {
                    Add | Sub | Mul | Div | Min | Max if a.is_numeric() => Ok(a),
                    Lt | Le | Gt | Ge if a.is_ordered() => Ok(Type::Bool),
                    Eq | Ne => Ok(Type::Bool),
                    And | Or if a == Type::Bool => Ok(Type::Bool),
                    _ => Err(TypeError(format!("{op:?} cannot be applied to {a}"))),
                }
            }
            Ir::If(c, t, e) => {
                let c = c.type_check(type_of_node)?;
                if c != Type::Bool {
                    return Err(TypeError(format!("a condition must be bool, got {c}")));
                }
                let t = t.type_check(type_of_node)?;
                let e = e.type_check(type_of_node)?;
                if t != e {
                    return Err(TypeError(format!(
                        "both branches must have the same type, got {t} and {e}"
                    )));
                }
                Ok(t)
            }
            Ir::Choose(index, options) => {
                let i = index.type_check(type_of_node)?;
                if i != Type::I64 {
                    return Err(TypeError(format!("a choice's index must be i64, got {i}")));
                }
                let Some((first, rest)) = options.split_first() else {
                    return Err(TypeError("a choice needs at least one option".into()));
                };
                let ty = first.type_check(type_of_node)?;
                for option in rest {
                    let other = option.type_check(type_of_node)?;
                    if other != ty {
                        return Err(TypeError(format!(
                            "every option must have the same type, got {ty} and {other}"
                        )));
                    }
                }
                Ok(ty)
            }
        }
    }

    /// Whether two expressions have exactly the same structure, using
    /// [`Value::same_as`] for constants.
    pub fn same_as(&self, other: &Ir) -> bool {
        match (self, other) {
            (Ir::Const(a), Ir::Const(b)) => a.same_as(b) && a.type_of() == b.type_of(),
            (Ir::Ref(a), Ir::Ref(b)) => a == b,
            (Ir::Unary(o1, a1), Ir::Unary(o2, a2)) => o1 == o2 && a1.same_as(a2),
            (Ir::Binary(o1, a1, b1), Ir::Binary(o2, a2, b2)) => {
                o1 == o2 && a1.same_as(a2) && b1.same_as(b2)
            }
            (Ir::If(c1, t1, e1), Ir::If(c2, t2, e2)) => {
                c1.same_as(c2) && t1.same_as(t2) && e1.same_as(e2)
            }
            (Ir::Choose(i1, o1), Ir::Choose(i2, o2)) => {
                i1.same_as(i2)
                    && o1.len() == o2.len()
                    && o1.iter().zip(o2).all(|(a, b)| a.same_as(b))
            }
            _ => false,
        }
    }

    /// Every node this expression refers to (possibly with repeats).
    pub fn refs(&self, out: &mut Vec<NodeRef>) {
        match self {
            Ir::Const(_) => {}
            Ir::Ref(r) => out.push(*r),
            Ir::Unary(_, a) => a.refs(out),
            Ir::Binary(_, a, b) => {
                a.refs(out);
                b.refs(out);
            }
            Ir::If(c, t, e) => {
                c.refs(out);
                t.refs(out);
                e.refs(out);
            }
            Ir::Choose(index, options) => {
                index.refs(out);
                for option in options {
                    option.refs(out);
                }
            }
        }
    }

    /// Evaluates this expression, reading each referenced node's value from
    /// `values` (indexed by node id). Only call this on type-checked expressions.
    pub(crate) fn eval(&self, values: &[Value]) -> Value {
        match self {
            Ir::Const(v) => v.clone(),
            Ir::Ref(r) => values[r.node.index()].clone(),
            Ir::Unary(op, a) => unary(*op, a.eval(values)),
            Ir::Binary(op, a, b) => binary(*op, a.eval(values), b.eval(values)),
            Ir::If(c, t, e) => match c.eval(values) {
                Value::Bool(true) => t.eval(values),
                Value::Bool(false) => e.eval(values),
                Value::Error(err) => Value::Error(err),
                other => internal(format!("condition evaluated to {other}")),
            },
            Ir::Choose(index, options) => match index.eval(values) {
                Value::I64(i) => match usize::try_from(i).ok().and_then(|i| options.get(i)) {
                    Some(option) => option.eval(values),
                    None => out_of_range(i, options.len()),
                },
                Value::Error(err) => Value::Error(err),
                other => internal(format!("choice index evaluated to {other}")),
            },
        }
    }

    /// Writes the expression in a readable form, naming nodes with `name`.
    pub fn render(&self, name: &dyn Fn(NodeId) -> String) -> String {
        match self {
            Ir::Const(v) => v.to_string(),
            Ir::Ref(r) => name(r.node),
            Ir::Unary(UnOp::Neg, a) => format!("-{}", a.render_operand(name)),
            Ir::Unary(UnOp::Not, a) => format!("!{}", a.render_operand(name)),
            Ir::Unary(UnOp::Abs, a) => format!("abs({})", a.render(name)),
            Ir::Binary(BinOp::Min, a, b) => format!("min({}, {})", a.render(name), b.render(name)),
            Ir::Binary(BinOp::Max, a, b) => format!("max({}, {})", a.render(name), b.render(name)),
            Ir::Binary(op, a, b) => {
                let symbol = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::And => "&&",
                    BinOp::Or => "||",
                    BinOp::Min | BinOp::Max => unreachable!("handled above"),
                };
                format!(
                    "{} {symbol} {}",
                    a.render_operand(name),
                    b.render_operand(name)
                )
            }
            Ir::If(c, t, e) => format!(
                "if({}, {}, {})",
                c.render(name),
                t.render(name),
                e.render(name)
            ),
            Ir::Choose(index, options) => {
                let mut parts = vec![index.render(name)];
                parts.extend(options.iter().map(|o| o.render(name)));
                format!("choose({})", parts.join(", "))
            }
        }
    }

    fn render_operand(&self, name: &dyn Fn(NodeId) -> String) -> String {
        match self {
            Ir::Binary(op, ..) if !matches!(op, BinOp::Min | BinOp::Max) => {
                format!("({})", self.render(name))
            }
            _ => self.render(name),
        }
    }
}

/// A value that should be impossible once an expression has been type-checked.
/// It becomes an error value rather than a crash.
fn internal(message: String) -> Value {
    debug_assert!(false, "internal type error: {message}");
    Value::error(format!("internal type error: {message}"))
}

fn unary(op: UnOp, a: Value) -> Value {
    match (op, a) {
        (_, Value::Error(e)) => Value::Error(e),
        (UnOp::Neg, Value::F64(x)) => Value::F64(-x),
        (UnOp::Neg, Value::I64(x)) => x
            .checked_neg()
            .map_or_else(|| Value::error("integer overflow"), Value::I64),
        (UnOp::Abs, Value::F64(x)) => Value::F64(x.abs()),
        (UnOp::Abs, Value::I64(x)) => x
            .checked_abs()
            .map_or_else(|| Value::error("integer overflow"), Value::I64),
        (UnOp::Not, Value::Bool(x)) => Value::Bool(!x),
        (op, a) => internal(format!("{op:?} on {a}")),
    }
}

fn binary(op: BinOp, a: Value, b: Value) -> Value {
    use BinOp::*;
    match (op, a, b) {
        (_, Value::Error(e), _) | (_, _, Value::Error(e)) => Value::Error(e),

        (Add, Value::F64(x), Value::F64(y)) => Value::F64(x + y),
        (Sub, Value::F64(x), Value::F64(y)) => Value::F64(x - y),
        (Mul, Value::F64(x), Value::F64(y)) => Value::F64(x * y),
        (Div, Value::F64(x), Value::F64(y)) => Value::F64(x / y),
        (Min, Value::F64(x), Value::F64(y)) => Value::F64(x.min(y)),
        (Max, Value::F64(x), Value::F64(y)) => Value::F64(x.max(y)),

        (Add, Value::I64(x), Value::I64(y)) => checked(x.checked_add(y)),
        (Sub, Value::I64(x), Value::I64(y)) => checked(x.checked_sub(y)),
        (Mul, Value::I64(x), Value::I64(y)) => checked(x.checked_mul(y)),
        (Div, Value::I64(_), Value::I64(0)) => Value::error("division by zero"),
        (Div, Value::I64(x), Value::I64(y)) => checked(x.checked_div(y)),
        (Min, Value::I64(x), Value::I64(y)) => Value::I64(x.min(y)),
        (Max, Value::I64(x), Value::I64(y)) => Value::I64(x.max(y)),

        (Lt | Le | Gt | Ge | Eq | Ne, a, b) => compare(op, &a, &b),

        (And, Value::Bool(x), Value::Bool(y)) => Value::Bool(x && y),
        (Or, Value::Bool(x), Value::Bool(y)) => Value::Bool(x || y),

        (op, a, b) => internal(format!("{op:?} on {a} and {b}")),
    }
}

fn checked(result: Option<i64>) -> Value {
    result.map_or_else(|| Value::error("integer overflow"), Value::I64)
}

fn compare(op: BinOp, a: &Value, b: &Value) -> Value {
    use std::cmp::Ordering;
    let ordering: Option<Ordering> = match (a, b) {
        (Value::F64(x), Value::F64(y)) => x.partial_cmp(y), // None when either is NaN
        (Value::I64(x), Value::I64(y)) => Some(x.cmp(y)),
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => return internal(format!("{op:?} on {a} and {b}")),
    };
    let result = match op {
        BinOp::Lt => ordering == Some(Ordering::Less),
        BinOp::Le => matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
        BinOp::Gt => ordering == Some(Ordering::Greater),
        BinOp::Ge => matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
        BinOp::Eq => ordering == Some(Ordering::Equal),
        BinOp::Ne => ordering != Some(Ordering::Equal),
        _ => unreachable!("only comparisons reach here"),
    };
    Value::Bool(result)
}
