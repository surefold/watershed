//! Decimal numbers: exact where `f64` is not.
//!
//! `0.1 + 0.2` is `0.3` here, not `0.30000000000000004`. A decimal holds up
//! to 28 significant digits and remembers how many decimal places it was
//! written with, so `1.50` stays `1.50`. It is the type for money and for
//! anything that has to reconcile to the last digit.
//!
//! - Adding, subtracting and multiplying are exact while the result fits
//!   in 28 digits. A result with more decimal places than fit is rounded at
//!   the last one; a result too large to hold is an error value, never a
//!   wrong number.
//! - Dividing rounds at the last digit the type holds (`1 / 3` is
//!   `0.3333333333333333333333333333`). Dividing by zero is an error value.
//! - [`Rounding`] puts a value on the number of decimal places you want:
//!   two for pence, none for whole units.
//!
//! Decimals do not mix with `f64` or `i64` by accident: the type checker
//! refuses `decimal + f64`. `to_decimal` and `to_f64` convert on purpose.
//!
//! The functions here are shared by the tree evaluator and the compiled
//! code, so they always agree.

use std::sync::Arc;

use rust_decimal::RoundingStrategy;
use rust_decimal::prelude::ToPrimitive;

use crate::value::Value;

/// A decimal number of up to 28 significant digits.
pub use rust_decimal::Decimal;

/// How to round when a value falls between two candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rounding {
    /// To the nearest; a tie goes to the even neighbour (`2.5` to `2`,
    /// `3.5` to `4`). Ties then round up as often as down, so totals of
    /// rounded values do not drift. The default.
    HalfEven,
    /// To the nearest; a tie goes away from zero (`2.5` to `3`), as taught
    /// in school.
    HalfUp,
    /// Toward zero: the extra digits are dropped.
    Down,
    /// Away from zero, if any extra digit is not zero.
    Up,
}

const OVERFLOW: &str = "decimal overflow";

pub(crate) fn value(d: Decimal) -> Value {
    Value::Decimal(Arc::new(d))
}

fn checked(result: Option<Decimal>) -> Value {
    result.map_or_else(|| Value::error(OVERFLOW), value)
}

pub(crate) fn add(a: &Decimal, b: &Decimal) -> Value {
    checked(a.checked_add(*b))
}

pub(crate) fn sub(a: &Decimal, b: &Decimal) -> Value {
    checked(a.checked_sub(*b))
}

pub(crate) fn mul(a: &Decimal, b: &Decimal) -> Value {
    checked(a.checked_mul(*b))
}

pub(crate) fn div(a: &Decimal, b: &Decimal) -> Value {
    if b.is_zero() {
        return Value::error("division by zero");
    }
    checked(a.checked_div(*b))
}

/// The smaller; of two that are equal (`1.5` and `1.50`), the first.
pub(crate) fn min(a: &Decimal, b: &Decimal) -> Value {
    value(if b < a { *b } else { *a })
}

/// The larger; of two that are equal, the first.
pub(crate) fn max(a: &Decimal, b: &Decimal) -> Value {
    value(if b > a { *b } else { *a })
}

pub(crate) fn neg(a: &Decimal) -> Value {
    value(-*a)
}

pub(crate) fn abs(a: &Decimal) -> Value {
    value(a.abs())
}

/// `a` to `places` decimal places. A value with fewer places already is
/// left as it is.
pub(crate) fn round(a: &Decimal, places: i64, rounding: Rounding) -> Value {
    let Some(places) = u32::try_from(places).ok().filter(|&p| p <= 28) else {
        return Value::error(format!(
            "a decimal is rounded to between 0 and 28 places, not {places}"
        ));
    };
    let strategy = match rounding {
        Rounding::HalfEven => RoundingStrategy::MidpointNearestEven,
        Rounding::HalfUp => RoundingStrategy::MidpointAwayFromZero,
        Rounding::Down => RoundingStrategy::ToZero,
        Rounding::Up => RoundingStrategy::AwayFromZero,
    };
    value(a.round_dp_with_strategy(places, strategy))
}

pub(crate) fn from_whole(whole: i64) -> Value {
    value(Decimal::from(whole))
}

/// The nearest `f64`.
pub(crate) fn to_float(a: &Decimal) -> Value {
    match a.to_f64() {
        Some(x) => Value::F64(x),
        None => Value::error("this decimal has no f64 near it"),
    }
}

/// Whether two decimals are the same down to how they are written: `1.5`
/// and `1.50` are equal, but not identical, since they show differently.
pub(crate) fn identical(a: &Decimal, b: &Decimal) -> bool {
    a.serialize() == b.serialize()
}
