//! The values that flow through a graph, and the Rust types that map onto them.

use std::fmt;
use std::sync::Arc;

use crate::decimal::Decimal;

/// The type of a node. Every node has exactly one, fixed when it is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    F64,
    I64,
    /// An exact decimal number (see the [`decimal`](crate::decimal) module).
    Decimal,
    Bool,
    Str,
    /// A list of strings.
    StrList,
}

impl Type {
    /// Types that support `+ - * / min max abs` and negation.
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::F64 | Type::I64 | Type::Decimal)
    }

    /// Types that support `< <= > >=`.
    pub fn is_ordered(self) -> bool {
        matches!(self, Type::F64 | Type::I64 | Type::Decimal | Type::Str)
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Type::F64 => "f64",
            Type::I64 => "i64",
            Type::Decimal => "decimal",
            Type::Bool => "bool",
            Type::Str => "string",
            Type::StrList => "list",
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
    /// Behind a pointer, like text, so that every `Value` stays 16 bytes
    /// and graphs of `f64` cost nothing for decimals existing.
    Decimal(Arc<Decimal>),
    Bool(bool),
    Str(Text),
    List(TextList),
    Error(Text),
}

/// Shared, immutable text, as held by a [`Value`].
///
/// It is a single pointer, which keeps every `Value` to 16 bytes. Cloning it
/// only bumps a reference count. Make one with `"text".into()`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Text(Arc<String>);

impl From<&str> for Text {
    fn from(s: &str) -> Self {
        Text(Arc::new(s.to_string()))
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Text(Arc::new(s))
    }
}

impl std::ops::Deref for Text {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self)
    }
}

/// A shared, immutable list of strings, as held by a [`Value`].
///
/// Like [`Text`] it is a single pointer, so cloning one only bumps a reference
/// count.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TextList(Arc<Vec<Text>>);

impl From<Vec<Text>> for TextList {
    fn from(items: Vec<Text>) -> Self {
        TextList(Arc::new(items))
    }
}

impl std::ops::Deref for TextList {
    type Target = [Text];
    fn deref(&self) -> &[Text] {
        &self.0
    }
}

impl fmt::Debug for TextList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

impl Value {
    /// The type of this value, or `None` for an error (which can stand in for any type).
    pub fn type_of(&self) -> Option<Type> {
        match self {
            Value::F64(_) => Some(Type::F64),
            Value::I64(_) => Some(Type::I64),
            Value::Decimal(_) => Some(Type::Decimal),
            Value::Bool(_) => Some(Type::Bool),
            Value::Str(_) => Some(Type::Str),
            Value::List(_) => Some(Type::StrList),
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
    /// Floats compare by their exact bits, except that every NaN is identical
    /// to every other NaN. So `NaN` is identical to `NaN`, and `0.0` is not
    /// identical to `-0.0` (dividing by them gives different answers).
    ///
    /// NaNs are lumped together because the bits inside a NaN are not
    /// reliable: hardware, the compiler and WebAssembly may each produce a
    /// different NaN for the same calculation, and nothing in the engine can
    /// tell two NaNs apart except by those bits. Otherwise this is
    /// deliberately strict: treating two values as the same when they are not
    /// would skip work that was needed, while the reverse only costs a little
    /// extra work.
    #[inline]
    pub fn same_as(&self, other: &Value) -> bool {
        // Floats first and on their own: every recalculated node asks this,
        // most nodes are floats, and the rest need not be part of that loop.
        if let (Value::F64(a), Value::F64(b)) = (self, other) {
            return a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan());
        }
        self.same_as_unless_floats(other)
    }

    #[inline(never)]
    fn same_as_unless_floats(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::I64(a), Value::I64(b)) => a == b,
            // `1.5` and `1.50` are equal but show differently, so they are
            // not identical: something above may be showing the value.
            (Value::Decimal(a), Value::Decimal(b)) => crate::decimal::identical(a, b),
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Error(a), Value::Error(b)) => a == b,
            _ => false,
        }
    }

    /// An error value with this message.
    pub fn error(message: impl Into<Text>) -> Value {
        Value::Error(message.into())
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::F64(x) => write!(f, "{x}"),
            Value::I64(x) => write!(f, "{x}"),
            Value::Decimal(x) => write!(f, "{x}"),
            Value::Bool(x) => write!(f, "{x}"),
            Value::Str(s) => write!(f, "{:?}", &**s),
            Value::List(items) => write!(f, "{items:?}"),
            Value::Error(e) => write!(f, "error: {e}"),
        }
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for f64 {}
    impl Sealed for i64 {}
    impl Sealed for crate::decimal::Decimal {}
    impl Sealed for bool {}
    impl Sealed for String {}
    impl Sealed for Vec<String> {}
}

/// A Rust type that a node can hold: `f64`, `i64`, [`Decimal`], `bool`,
/// `String` or `Vec<String>`.
pub trait CalcType: sealed::Sealed + Sized + 'static {
    const TYPE: Type;
    fn into_value(self) -> Value;
    fn from_value(value: &Value) -> Option<Self>;
}

/// Types that support arithmetic: `f64`, `i64` and [`Decimal`].
pub trait Numeric: CalcType {}
impl Numeric for f64 {}
impl Numeric for i64 {}
impl Numeric for Decimal {}

/// Types that support ordering comparisons: `f64`, `i64`, [`Decimal`] and
/// `String`.
pub trait Ordered: CalcType {}
impl Ordered for f64 {}
impl Ordered for i64 {}
impl Ordered for Decimal {}
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

impl CalcType for Decimal {
    const TYPE: Type = Type::Decimal;
    fn into_value(self) -> Value {
        Value::Decimal(Arc::new(self))
    }
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Decimal(x) => Some(**x),
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

impl CalcType for Vec<String> {
    const TYPE: Type = Type::StrList;
    fn into_value(self) -> Value {
        let items: Vec<Text> = self.into_iter().map(Text::from).collect();
        Value::List(items.into())
    }
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::List(items) => Some(items.iter().map(|s| s.to_string()).collect()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_takes_16_bytes() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
    }
}
