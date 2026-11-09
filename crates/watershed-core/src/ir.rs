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
use crate::value::{Text, TextList, Type, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
    Abs,
    /// Any value as text: `3` as `"3"`, `2.0` as `"2.0"`, `true` as `"true"`.
    /// Text stays as it is.
    ToText,
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
    /// Two strings joined, or two lists, one after the other. A result over
    /// [`MAX_TEXT_BYTES`] or [`MAX_LIST_ITEMS`] is an error.
    Concat,
    /// `Join(list, separator)`: a list's items as one string. A result over
    /// [`MAX_TEXT_BYTES`] is an error.
    Join,
}

/// The longest text a join can produce. Joining a value to itself doubles its
/// length, so a chain of joins could otherwise ask for more memory than exists.
pub const MAX_TEXT_BYTES: usize = 1 << 20;

/// The most items a joined list can hold, for the same reason.
pub const MAX_LIST_ITEMS: usize = 1 << 16;

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
    /// A list of these strings, in this order. May be empty.
    List(Vec<Arc<Ir>>),
}

/// The error for a `Choose` index outside its options. Shared by both
/// evaluators so they always agree.
pub(crate) fn out_of_range(index: i64, options: usize) -> Value {
    Value::error(format!(
        "choice {index} is out of range: there are {options} options, numbered from 0"
    ))
}

/// Two strings joined, or an error if the result would be too long. Shared by
/// both evaluators so they always agree.
pub(crate) fn concat(a: &str, b: &str) -> Value {
    if a.len() + b.len() > MAX_TEXT_BYTES {
        return Value::error(format!(
            "text too long: joining these would make more than {MAX_TEXT_BYTES} bytes"
        ));
    }
    let mut joined = String::with_capacity(a.len() + b.len());
    joined.push_str(a);
    joined.push_str(b);
    Value::Str(joined.into())
}

/// Two lists, one after the other, or an error if the result would be too long.
pub(crate) fn concat_lists(a: &[Text], b: &[Text]) -> Value {
    if a.len() + b.len() > MAX_LIST_ITEMS {
        return Value::error(format!(
            "list too long: joining these would make more than {MAX_LIST_ITEMS} items"
        ));
    }
    let mut joined = Vec::with_capacity(a.len() + b.len());
    joined.extend_from_slice(a);
    joined.extend_from_slice(b);
    Value::List(TextList::from(joined))
}

/// A list's items as one string, or an error if that would be too long.
pub(crate) fn join(items: &[Text], separator: &str) -> Value {
    let separators = separator
        .len()
        .saturating_mul(items.len().saturating_sub(1));
    let total = items
        .iter()
        .fold(separators, |sum, s| sum.saturating_add(s.len()));
    if total > MAX_TEXT_BYTES {
        return Value::error(format!(
            "text too long: joining these would make more than {MAX_TEXT_BYTES} bytes"
        ));
    }
    let mut joined = String::with_capacity(total);
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            joined.push_str(separator);
        }
        joined.push_str(item);
    }
    Value::Str(joined.into())
}

/// A list of these evaluated items. The first error among them is the result.
pub(crate) fn make_list(values: Vec<Value>) -> Value {
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        match value {
            Value::Str(s) => items.push(s),
            err @ Value::Error(_) => return err,
            other => return internal(format!("list item evaluated to {other}")),
        }
    }
    Value::List(TextList::from(items))
}

/// A value as text. Text and errors pass through unchanged. Shared by both
/// evaluators so they always agree.
pub(crate) fn to_text(value: Value) -> Value {
    let text = match value {
        Value::F64(x) if x == f64::INFINITY => "Infinity".to_string(),
        Value::F64(x) if x == f64::NEG_INFINITY => "-Infinity".to_string(),
        // The debug form keeps the `.0` of a whole number, as Kotlin does.
        Value::F64(x) => format!("{x:?}"),
        Value::I64(x) => x.to_string(),
        Value::Bool(x) => x.to_string(),
        // As Kotlin shows a list: `[a, b]`.
        Value::List(items) => match join(&items, ", ") {
            Value::Str(inner) => format!("[{inner}]"),
            err => return err,
        },
        other @ (Value::Str(_) | Value::Error(_)) => return other,
    };
    Value::Str(text.into())
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
                    UnOp::ToText => Ok(Type::Str),
                    _ => Err(TypeError(format!("{op:?} cannot be applied to {a}"))),
                }
            }
            Ir::Binary(BinOp::Join, a, b) => {
                let a = a.type_check(type_of_node)?;
                let b = b.type_check(type_of_node)?;
                if a != Type::StrList || b != Type::Str {
                    return Err(TypeError(format!(
                        "Join needs a list and a string, got {a} and {b}"
                    )));
                }
                Ok(Type::Str)
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
                    Concat if matches!(a, Type::Str | Type::StrList) => Ok(a),
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
            Ir::List(items) => {
                for item in items {
                    let ty = item.type_check(type_of_node)?;
                    if ty != Type::Str {
                        return Err(TypeError(format!(
                            "every item of a list must be a string, got {ty}"
                        )));
                    }
                }
                Ok(Type::StrList)
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
            (Ir::List(a), Ir::List(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same_as(y))
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
            Ir::List(items) => {
                for item in items {
                    item.refs(out);
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
            Ir::List(items) => make_list(items.iter().map(|i| i.eval(values)).collect()),
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
            Ir::Unary(UnOp::ToText, a) => format!("text({})", a.render(name)),
            Ir::Binary(BinOp::Min, a, b) => format!("min({}, {})", a.render(name), b.render(name)),
            Ir::Binary(BinOp::Max, a, b) => format!("max({}, {})", a.render(name), b.render(name)),
            Ir::Binary(BinOp::Join, a, b) => {
                format!("join({}, {})", a.render(name), b.render(name))
            }
            // Joining is the same however it is bracketed, so a chain reads
            // left to right with no brackets.
            Ir::Binary(BinOp::Concat, a, b) => {
                let left = match &**a {
                    Ir::Binary(BinOp::Concat, ..) => a.render(name),
                    _ => a.render_operand(name),
                };
                format!("{left} + {}", b.render_operand(name))
            }
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
                    BinOp::Min | BinOp::Max | BinOp::Concat | BinOp::Join => {
                        unreachable!("handled above")
                    }
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
            Ir::List(items) => {
                let parts: Vec<String> = items.iter().map(|i| i.render(name)).collect();
                format!("listOf({})", parts.join(", "))
            }
        }
    }

    fn render_operand(&self, name: &dyn Fn(NodeId) -> String) -> String {
        match self {
            Ir::Binary(op, ..) if !matches!(op, BinOp::Min | BinOp::Max | BinOp::Join) => {
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
        (UnOp::ToText, a) => to_text(a),
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

        (Concat, Value::Str(x), Value::Str(y)) => concat(&x, &y),
        (Concat, Value::List(x), Value::List(y)) => concat_lists(&x, &y),
        (Join, Value::List(x), Value::Str(y)) => join(&x, &y),

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
        (Value::List(x), Value::List(y)) => Some(x.cmp(y)),
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
