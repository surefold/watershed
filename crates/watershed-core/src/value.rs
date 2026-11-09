//! The values that flow through a graph, and the Rust types that map onto them.

use std::fmt;
use std::sync::Arc;

/// The type of a node. Every node has exactly one, fixed when it is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    F64,
    I64,
    Bool,
    Str,
}

impl Type {
    /// Types that support `+ - * / min max abs` and negation.
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::F64 | Type::I64)
    }

    /// Types that support `< <= > >=`.
    pub fn is_ordered(self) -> bool {
        matches!(self, Type::F64 | Type::I64 | Type::Str)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Type::F64 => "f64",
            Type::I64 => "i64",
            Type::Bool => "bool",
            Type::Str => "string",
        })
    }
}

/// A runtime value.
///
/// `Error` is a value like any other: a node whose calculation fails (an
/// integer division by zero, say) holds an error, and nodes that depend on it
/// hold that error too, until the cause is fixed. Nothing panics.
#[derive(Debug, Clone)]
pub enum Value {
    F64(f64),
    I64(i64),
    Bool(bool),
    Str(Arc<str>),
    Error(Arc<str>),
}

impl Value {
    /// The type of this value, or `None` for an error (which can stand in for any type).
    pub fn type_of(&self) -> Option<Type> {
        match self {
            Value::F64(_) => Some(Type::F64),
            Value::I64(_) => Some(Type::I64),
            Value::Bool(_) => Some(Type::Bool),
            Value::Str(_) => Some(Type::Str),
            Value::Error(_) => None,
        }
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Value::Error(_))
    }

    /// Whether two values are *identical*. This is what decides cutoff: if a
    /// node's new value is identical to its old one, nothing above it is
    /// recalculated.
    ///
    /// Floats compare by their exact bits, so `NaN` is identical to `NaN` and
    /// `0.0` is not identical to `-0.0`. That is deliberately strict: treating
    /// two values as the same when they are not would skip work that was
    /// needed, while the reverse only costs a little extra work.
    pub fn same_as(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::F64(a), Value::F64(b)) => a.to_bits() == b.to_bits(),
            (Value::I64(a), Value::I64(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Error(a), Value::Error(b)) => a == b,
            _ => false,
        }
    }

    pub(crate) fn error(message: impl Into<Arc<str>>) -> Value {
        Value::Error(message.into())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::F64(x) => write!(f, "{x}"),
            Value::I64(x) => write!(f, "{x}"),
            Value::Bool(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::Error(e) => write!(f, "error: {e}"),
        }
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for f64 {}
    impl Sealed for i64 {}
    impl Sealed for bool {}
    impl Sealed for String {}
}

/// A Rust type that a node can hold: `f64`, `i64`, `bool` or `String`.
pub trait CalcType: sealed::Sealed + Sized + 'static {
    const TYPE: Type;
    fn into_value(self) -> Value;
    fn from_value(value: &Value) -> Option<Self>;
}

/// Types that support arithmetic: `f64` and `i64`.
pub trait Numeric: CalcType {}
impl Numeric for f64 {}
impl Numeric for i64 {}

/// Types that support ordering comparisons: `f64`, `i64` and `String`.
pub trait Ordered: CalcType {}
impl Ordered for f64 {}
impl Ordered for i64 {}
impl Ordered for String {}

impl CalcType for f64 {
    const TYPE: Type = Type::F64;
    fn into_value(self) -> Value {
        Value::F64(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::F64(x) => Some(*x),
            _ => None,
        }
    }
}

impl CalcType for i64 {
    const TYPE: Type = Type::I64;
    fn into_value(self) -> Value {
        Value::I64(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::I64(x) => Some(*x),
            _ => None,
        }
    }
}

impl CalcType for bool {
    const TYPE: Type = Type::Bool;
    fn into_value(self) -> Value {
        Value::Bool(self)
    }
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Bool(x) => Some(*x),
            _ => None,
        }
    }
}

impl CalcType for String {
    const TYPE: Type = Type::Str;
    fn into_value(self) -> Value {
        Value::Str(self.into())
    }
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Str(s) => Some(s.to_string()),
            _ => None,
        }
    }
}
