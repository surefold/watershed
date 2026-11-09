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
constants!(f64, i64, bool, String);

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
arithmetic!(Expr, Node, Input);

// A constant on the left, as in `1.0 + rate`.
macro_rules! constant_on_the_left {
    ($($t:ty),*) => {$(
        constant_on_the_left!(@handle $t, Expr);
        constant_on_the_left!(@handle $t, Node);
        constant_on_the_left!(@handle $t, Input);
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
constant_on_the_left!(f64, i64);

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
methods!(Expr, Node, Input);

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
