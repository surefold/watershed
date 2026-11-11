//! The typed front end: write calculations with ordinary Rust operators and
//! the compiler checks the types.
//!
//! ```
//! use watershed_core::prelude::*;
//!
//! let mut g = Graph::new();
//! let qty = g.input("qty", 10.0).unwrap();
//! let price = g.input("price", 2.5).unwrap();
//! let total = g.define("total", qty * price * 1.2).unwrap();
//! assert_eq!(g.get(total).unwrap(), 30.0);
//! ```
//!
//! Operators only build an expression (an [`Expr<T>`]); nothing is
//! calculated until the expression is given to [`Graph::define`](crate::Graph::define).

use std::marker::PhantomData;
use std::ops::{Add, BitAnd, BitOr, Div, Mul, Neg, Not, Sub};
use std::sync::Arc;

use crate::collection::{Column, Field};
use crate::decimal::{Decimal, Rounding};
use crate::handle::{Input, Node};
use crate::ir::{BinOp, Ir, UnOp};
use crate::value::{CalcType, Numeric, Ordered};

/// An expression that produces a `T` when evaluated.
pub struct Expr<T> {
    pub(crate) ir: Arc<Ir>,
    _type: PhantomData<fn() -> T>,
}

impl<T> Clone for Expr<T> {
    fn clone(&self) -> Self {
        Expr::from_arc(self.ir.clone())
    }
}

impl<T> std::fmt::Debug for Expr<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Expr({:?})", self.ir)
    }
}

impl<T> Expr<T> {
    fn from_arc(ir: Arc<Ir>) -> Self {
        Expr {
            ir,
            _type: PhantomData,
        }
    }
    pub(crate) fn from_ir(ir: Ir) -> Self {
        Expr::from_arc(Arc::new(ir))
    }
    /// The untyped expression underneath.
    pub fn ir(&self) -> &Ir {
        &self.ir
    }
}

/// Anything that can be used as an expression producing a `T`: another
/// expression, a node or input holding a `T`, or a constant `T`.
pub trait IntoExpr<T> {
    fn into_expr(self) -> Expr<T>;
}

impl<T> IntoExpr<T> for Expr<T> {
    fn into_expr(self) -> Expr<T> {
        self
    }
}
impl<T> IntoExpr<T> for &Expr<T> {
    fn into_expr(self) -> Expr<T> {
        self.clone()
    }
}
impl<T> IntoExpr<T> for Node<T> {
    fn into_expr(self) -> Expr<T> {
        Expr::from_ir(Ir::Ref(self.at))
    }
}
impl<T> IntoExpr<T> for Input<T> {
    fn into_expr(self) -> Expr<T> {
        Expr::from_ir(Ir::Ref(self.at))
    }
}

macro_rules! constants {
    ($($t:ty),*) => {$(
        impl IntoExpr<$t> for $t {
            fn into_expr(self) -> Expr<$t> {
                Expr::from_ir(Ir::Const(self.into_value()))
            }
        }
    )*};
}
constants!(f64, i64, Decimal, bool, String, Vec<String>);

impl IntoExpr<String> for &str {
    fn into_expr(self) -> Expr<String> {
        Expr::from_ir(Ir::Const(self.to_string().into_value()))
    }
}

fn unary<T, U>(op: UnOp, a: Expr<T>) -> Expr<U> {
    Expr::from_ir(Ir::Unary(op, a.ir))
}

fn binary<T, U>(op: BinOp, a: Expr<T>, b: Expr<T>) -> Expr<U> {
    Expr::from_ir(Ir::Binary(op, a.ir, b.ir))
}

// ---- Operators: `+ - * /` and unary `-` on numbers, `& | !` on booleans ----

macro_rules! arithmetic {
    ($($handle:ident),*) => {$(
        arithmetic!(@op $handle, Add, add, BinOp::Add);
        arithmetic!(@op $handle, Sub, sub, BinOp::Sub);
        arithmetic!(@op $handle, Mul, mul, BinOp::Mul);
        arithmetic!(@op $handle, Div, div, BinOp::Div);

        impl<T: Numeric> Neg for $handle<T> {
            type Output = Expr<T>;
            fn neg(self) -> Expr<T> {
                unary(UnOp::Neg, self.into_expr())
            }
        }

        impl<R: IntoExpr<bool>> BitAnd<R> for $handle<bool> {
            type Output = Expr<bool>;
            fn bitand(self, rhs: R) -> Expr<bool> {
                binary(BinOp::And, self.into_expr(), rhs.into_expr())
            }
        }
        impl<R: IntoExpr<bool>> BitOr<R> for $handle<bool> {
            type Output = Expr<bool>;
            fn bitor(self, rhs: R) -> Expr<bool> {
                binary(BinOp::Or, self.into_expr(), rhs.into_expr())
            }
        }
        impl Not for $handle<bool> {
            type Output = Expr<bool>;
            fn not(self) -> Expr<bool> {
                unary(UnOp::Not, self.into_expr())
            }
        }
    )*};
    (@op $handle:ident, $trait:ident, $method:ident, $op:expr) => {
        impl<T: Numeric, R: IntoExpr<T>> $trait<R> for $handle<T> {
            type Output = Expr<T>;
            fn $method(self, rhs: R) -> Expr<T> {
                binary($op, self.into_expr(), rhs.into_expr())
            }
        }
    };
}
arithmetic!(Expr, Node, Input, Field, Column);

// A constant on the left, as in `1.0 + rate`.
macro_rules! constant_on_the_left {
    ($($t:ty),*) => {$(
        constant_on_the_left!(@handle $t, Expr);
        constant_on_the_left!(@handle $t, Node);
        constant_on_the_left!(@handle $t, Input);
        constant_on_the_left!(@handle $t, Field);
        constant_on_the_left!(@handle $t, Column);
    )*};
    (@handle $t:ty, $handle:ident) => {
        constant_on_the_left!(@op $t, $handle, Add, add, BinOp::Add);
        constant_on_the_left!(@op $t, $handle, Sub, sub, BinOp::Sub);
        constant_on_the_left!(@op $t, $handle, Mul, mul, BinOp::Mul);
        constant_on_the_left!(@op $t, $handle, Div, div, BinOp::Div);
    };
    (@op $t:ty, $handle:ident, $trait:ident, $method:ident, $op:expr) => {
        impl $trait<$handle<$t>> for $t {
            type Output = Expr<$t>;
            fn $method(self, rhs: $handle<$t>) -> Expr<$t> {
                binary($op, self.into_expr(), rhs.into_expr())
            }
        }
    };
}
constant_on_the_left!(f64, i64, Decimal);

// ---- Methods: comparisons, min/max/abs ----
//
// Rust's `<` and `==` must return a plain `bool`, so comparisons that build an
// expression are methods instead: `price.gt(100.0)`.

macro_rules! methods {
    ($($handle:ident),*) => {$(
        impl<T: Ordered> $handle<T> {
            pub fn lt(self, rhs: impl IntoExpr<T>) -> Expr<bool> {
                binary(BinOp::Lt, self.into_expr(), rhs.into_expr())
            }
            pub fn le(self, rhs: impl IntoExpr<T>) -> Expr<bool> {
                binary(BinOp::Le, self.into_expr(), rhs.into_expr())
            }
            pub fn gt(self, rhs: impl IntoExpr<T>) -> Expr<bool> {
                binary(BinOp::Gt, self.into_expr(), rhs.into_expr())
            }
            pub fn ge(self, rhs: impl IntoExpr<T>) -> Expr<bool> {
                binary(BinOp::Ge, self.into_expr(), rhs.into_expr())
            }
        }

        impl<T: CalcType> $handle<T> {
            pub fn equals(self, rhs: impl IntoExpr<T>) -> Expr<bool> {
                binary(BinOp::Eq, self.into_expr(), rhs.into_expr())
            }
            pub fn not_equals(self, rhs: impl IntoExpr<T>) -> Expr<bool> {
                binary(BinOp::Ne, self.into_expr(), rhs.into_expr())
            }
        }

        impl $handle<String> {
            /// This text followed by `rhs`. A result over
            /// [`MAX_TEXT_BYTES`](crate::MAX_TEXT_BYTES) is an error.
            pub fn concat(self, rhs: impl IntoExpr<String>) -> Expr<String> {
                binary(BinOp::Concat, self.into_expr(), rhs.into_expr())
            }
        }

        impl $handle<Vec<String>> {
            /// This list followed by `rhs`. A result over
            /// [`MAX_LIST_ITEMS`](crate::MAX_LIST_ITEMS) is an error.
            pub fn concat(self, rhs: impl IntoExpr<Vec<String>>) -> Expr<Vec<String>> {
                binary(BinOp::Concat, self.into_expr(), rhs.into_expr())
            }
            /// The items as one string, with `separator` between them.
            pub fn join(self, separator: impl IntoExpr<String>) -> Expr<String> {
                Expr::from_ir(Ir::Binary(
                    BinOp::Join,
                    self.into_expr().ir,
                    separator.into_expr().ir,
                ))
            }
        }

        impl $handle<Decimal> {
            /// To this many decimal places (0 to 28), a tie going to the
            /// even neighbour.
            pub fn round(self, places: impl IntoExpr<i64>) -> Expr<Decimal> {
                self.round_with(places, Rounding::HalfEven)
            }
            /// To this many decimal places (0 to 28), rounded as said.
            pub fn round_with(
                self,
                places: impl IntoExpr<i64>,
                rounding: Rounding,
            ) -> Expr<Decimal> {
                Expr::from_ir(Ir::Binary(
                    BinOp::Round(rounding),
                    self.into_expr().ir,
                    places.into_expr().ir,
                ))
            }
            /// The nearest `f64`, for where exactness is not needed.
            pub fn to_f64(self) -> Expr<f64> {
                unary(UnOp::ToFloat, self.into_expr())
            }
        }

        impl $handle<i64> {
            /// The same number as a decimal, exactly.
            pub fn to_decimal(self) -> Expr<Decimal> {
                unary(UnOp::ToDecimal, self.into_expr())
            }
        }

        impl<T: Numeric> $handle<T> {
            pub fn min(self, rhs: impl IntoExpr<T>) -> Expr<T> {
                binary(BinOp::Min, self.into_expr(), rhs.into_expr())
            }
            pub fn max(self, rhs: impl IntoExpr<T>) -> Expr<T> {
                binary(BinOp::Max, self.into_expr(), rhs.into_expr())
            }
            pub fn abs(self) -> Expr<T> {
                unary(UnOp::Abs, self.into_expr())
            }
        }
    )*};
}
methods!(Expr, Node, Input, Field, Column);

/// Any value as text: `3` as `"3"`, `2.0` as `"2.0"`, `true` as `"true"`.
///
/// ```
/// use watershed_core::prelude::*;
///
/// let mut g = Graph::new();
/// let major = g.input("major", 2_i64).unwrap();
/// let version = g.define("version", text(major).concat(".1")).unwrap();
/// assert_eq!(g.get(version).unwrap(), "2.1");
/// ```
pub fn text<T>(value: impl IntoExpr<T>) -> Expr<String> {
    unary(UnOp::ToText, value.into_expr())
}

/// A list of these strings.
///
/// ```
/// use watershed_core::prelude::*;
///
/// let mut g = Graph::new();
/// let flag = g.input("flag", "-progressive".to_string()).unwrap();
/// let args = g.define("args", list([flag.into_expr(), "-Werror".into_expr()])).unwrap();
/// let line = g.define("line", args.join(" ")).unwrap();
/// assert_eq!(g.get(line).unwrap(), "-progressive -Werror");
/// ```
pub fn list<O: IntoExpr<String>>(items: impl IntoIterator<Item = O>) -> Expr<Vec<String>> {
    Expr::from_ir(Ir::List(
        items.into_iter().map(|i| i.into_expr().ir).collect(),
    ))
}

// ---- Conditionals: when(cond).then(a).otherwise(b) ----

/// Starts a conditional: `when(cond).then(a).otherwise(b)`.
/// Only the branch that is chosen is evaluated.
pub fn when(condition: impl IntoExpr<bool>) -> When {
    When {
        condition: condition.into_expr(),
    }
}

pub struct When {
    condition: Expr<bool>,
}

impl When {
    pub fn then<T>(self, value: impl IntoExpr<T>) -> Then<T> {
        Then {
            condition: self.condition,
            then: value.into_expr(),
        }
    }
}

pub struct Then<T> {
    condition: Expr<bool>,
    then: Expr<T>,
}

impl<T> Then<T> {
    pub fn otherwise(self, value: impl IntoExpr<T>) -> Expr<T> {
        Expr::from_ir(Ir::If(
            self.condition.ir,
            self.then.ir,
            value.into_expr().ir,
        ))
    }
}

// ---- Choosing by position: choose(index, [a, b, c]) ----

/// The option at position `index`, counting from 0, like a spreadsheet's
/// `CHOOSE`. Only that option is evaluated, and the node is only
/// recalculated when the index or that option changes. An index out of range
/// is an error.
///
/// ```
/// use watershed_core::prelude::*;
///
/// let mut g = Graph::new();
/// let source = g.input("source", 1_i64).unwrap();
/// let feeds = [g.input("primary", 100.0).unwrap(), g.input("backup", 99.5).unwrap()];
/// let price = g.define("price", choose(source, feeds)).unwrap();
/// assert_eq!(g.get(price).unwrap(), 99.5);
/// ```
pub fn choose<T, O: IntoExpr<T>>(
    index: impl IntoExpr<i64>,
    options: impl IntoIterator<Item = O>,
) -> Expr<T> {
    Expr::from_ir(Ir::Choose(
        index.into_expr().ir,
        options.into_iter().map(|o| o.into_expr().ir).collect(),
    ))
}
