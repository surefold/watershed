//! How values, expressions and rows cross from the JVM: as bytes.
//!
//! The JVM writes them into one buffer and passes its address and length;
//! this reads them back, checking every length against what is left. Whole
//! numbers are little-endian. Text is its length (`i32`) and then UTF-8.

use std::sync::Arc;

use watershed_core::{BinOp, CalcType, Decimal, Ir, Rounding, UnOp, Value};

pub(crate) type Result<T> = std::result::Result<T, String>;

/// Reads a buffer from the front.
pub(crate) struct Reader<'a> {
    rest: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Reader { rest: bytes }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.rest.is_empty()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        if count > self.rest.len() {
            return Err("the bytes passed in end early".into());
        }
        let (taken, rest) = self.rest.split_at(count);
        self.rest = rest;
        Ok(taken)
    }

    pub(crate) fn bytes(&mut self, count: usize) -> Result<&'a [u8]> {
        self.take(count)
    }

    pub(crate) fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn int(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().expect("four")))
    }

    fn long(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().expect("eight")))
    }

    /// A count that is not negative.
    pub(crate) fn count(&mut self) -> Result<usize> {
        usize::try_from(self.int()?).map_err(|_| "a count passed in is negative".to_string())
    }

    pub(crate) fn text(&mut self) -> Result<&'a str> {
        let length = self.count()?;
        text(self.take(length)?)
    }

    /// A value: a byte saying which kind, then the value.
    pub(crate) fn value(&mut self) -> Result<Value> {
        Ok(match self.byte()? {
            NUMBER => Value::F64(f64::from_bits(self.long()? as u64)),
            WHOLE => Value::I64(self.long()?),
            DECIMAL => {
                let (low, high, scale) = (self.long()?, self.long()?, self.int()?);
                decimal(low, high, scale)?.into_value()
            }
            FLAG => Value::Bool(self.byte()? != 0),
            TEXT => Value::Str(self.text()?.into()),
            other => return Err(format!("there is no kind of value numbered {other}")),
        })
    }
}

pub(crate) fn text(bytes: &[u8]) -> Result<&str> {
    std::str::from_utf8(bytes).map_err(|_| "text passed in is not UTF-8".to_string())
}

// The kinds of value, as the JVM numbers them.
pub(crate) const NUMBER: u8 = 0;
pub(crate) const WHOLE: u8 = 1;
pub(crate) const DECIMAL: u8 = 2;
pub(crate) const FLAG: u8 = 3;
pub(crate) const TEXT: u8 = 4;
pub(crate) const TEXTS: u8 = 5;
pub(crate) const FAILED: u8 = 6;

/// A decimal from its digits as one whole number (in two halves) and how
/// many of them come after the point.
pub(crate) fn decimal(low: i64, high: i64, scale: i32) -> Result<Decimal> {
    let digits = (i128::from(high) << 64) | i128::from(low as u64);
    u32::try_from(scale)
        .ok()
        .and_then(|scale| Decimal::try_from_i128_with_scale(digits, scale).ok())
        .ok_or_else(|| {
            "that decimal has more digits than the engine holds: it keeps 28 significant digits"
                .to_string()
        })
}

/// A decimal as its digits in two halves and its scale.
pub(crate) fn decimal_parts(decimal: &Decimal) -> (i64, i64, i32) {
    let digits = decimal.mantissa();
    (digits as i64, (digits >> 64) as i64, decimal.scale() as i32)
}

/// An expression: a list of steps in the order a calculator with a stack
/// would take them. `qty * price + 1` is `qty`, `price`, multiply, `1`, add.
/// `node` and `part` say what a node number and a part's name stand for.
pub(crate) fn expression(
    bytes: &[u8],
    node: &dyn Fn(i32) -> Result<Ir>,
    part: &dyn Fn(&str) -> Result<Ir>,
) -> Result<Ir> {
    fn take(stack: &mut Vec<Ir>) -> Result<Arc<Ir>> {
        stack
            .pop()
            .map(Arc::new)
            .ok_or_else(|| "the expression is missing a value".to_string())
    }
    fn take_many(stack: &mut Vec<Ir>, count: usize) -> Result<Vec<Arc<Ir>>> {
        let mut items = (0..count)
            .map(|_| take(stack))
            .collect::<Result<Vec<_>>>()?;
        items.reverse();
        Ok(items)
    }
    let mut reader = Reader::new(bytes);
    let mut stack: Vec<Ir> = Vec::new();
    while !reader.is_done() {
        let next = match reader.byte()? {
            0 => node(reader.int()?)?,
            1 => part(reader.text()?)?,
            2 => Ir::Const(reader.value()?),
            3 => {
                let op = match reader.byte()? {
                    0 => UnOp::Neg,
                    1 => UnOp::Not,
                    2 => UnOp::Abs,
                    3 => UnOp::ToText,
                    4 => UnOp::ToDecimal,
                    5 => UnOp::ToFloat,
                    other => {
                        return Err(format!(
                            "there is no operation on one value numbered {other}"
                        ));
                    }
                };
                Ir::Unary(op, take(&mut stack)?)
            }
            4 => {
                let op = match reader.byte()? {
                    0 => BinOp::Add,
                    1 => BinOp::Sub,
                    2 => BinOp::Mul,
                    3 => BinOp::Div,
                    4 => BinOp::Min,
                    5 => BinOp::Max,
                    6 => BinOp::Lt,
                    7 => BinOp::Le,
                    8 => BinOp::Gt,
                    9 => BinOp::Ge,
                    10 => BinOp::Eq,
                    11 => BinOp::Ne,
                    12 => BinOp::And,
                    13 => BinOp::Or,
                    14 => BinOp::Concat,
                    15 => BinOp::Join,
                    16 => BinOp::Round(Rounding::HalfEven),
                    17 => BinOp::Round(Rounding::HalfUp),
                    18 => BinOp::Round(Rounding::Down),
                    19 => BinOp::Round(Rounding::Up),
                    other => {
                        return Err(format!(
                            "there is no operation on two values numbered {other}"
                        ));
                    }
                };
                let right = take(&mut stack)?;
                let left = take(&mut stack)?;
                Ir::Binary(op, left, right)
            }
            5 => {
                let otherwise = take(&mut stack)?;
                let then = take(&mut stack)?;
                let condition = take(&mut stack)?;
                Ir::If(condition, then, otherwise)
            }
            6 => {
                let options = reader.count()?;
                let options = take_many(&mut stack, options)?;
                Ir::Choose(take(&mut stack)?, options)
            }
            7 => {
                let items = reader.count()?;
                Ir::List(take_many(&mut stack, items)?)
            }
            other => {
                return Err(format!(
                    "there is no step of an expression numbered {other}"
                ));
            }
        };
        stack.push(next);
    }
    match (stack.pop(), stack.is_empty()) {
        (Some(ir), true) => Ok(ir),
        _ => Err("the expression does not come to one value".into()),
    }
}

/// Writes text to send back: its length, then its bytes.
pub(crate) fn put_text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as i32).to_le_bytes());
    out.extend_from_slice(text.as_bytes());
}
