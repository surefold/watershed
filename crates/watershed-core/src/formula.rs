//! Formulas as text: `qty * (mark - price)`, `if(side == "Buy", 1, -1)`.
//!
//! This is the front end for people who type a formula rather than write
//! Rust or Python: a grid's column, a rule in a settings file. A formula
//! becomes an expression ([`Ir`]), which the engine type-checks when a node
//! or column is made from it, exactly as it checks any other expression.
//!
//! ```
//! use watershed_core::prelude::*;
//!
//! # fn main() -> Result<(), CalcError> {
//! let mut g = Graph::new();
//! let qty = g.input("qty", 3.0)?;
//! g.input("price", 2.5)?;
//! let total = g.define_formula("total", "qty * price")?;
//! assert_eq!(g.value(total).to_string(), "7.5");
//!
//! g.set(qty, 4.0)?;
//! g.stabilize();
//! assert_eq!(g.value(total).to_string(), "10");
//! # Ok(())
//! # }
//! ```
//!
//! [`Graph::parse_formula`](crate::Graph::parse_formula) reads names as the graph's
//! nodes, and [`Collection::parse_formula`](crate::Collection::parse_formula) as the
//! collection's fields and columns first. [`parse`] takes any way of
//! turning a name into an expression.
//!
//! ```text
//! formula := or
//! or      := and ( "||" and )*
//! and     := compare ( "&&" compare )*
//! compare := sum ( ( "<" | "<=" | ">" | ">=" | "==" | "=" | "!=" ) sum )?
//! sum     := product ( ( "+" | "-" ) product )*
//! product := unary ( ( "*" | "/" ) unary )*
//! unary   := ( "-" | "!" ) unary | primary
//! primary := number | decimal | "text" | 'text' | true | false | name | `any name`
//!          | "(" formula ")" | "[" formula, ... "]"
//!          | if(c, a, b) | min(a, b) | max(a, b) | abs(a)
//!          | text(a) | concat(a, b) | join(list, separator)
//!          | choose(index, a, b, ...)
//!          | round(d, places) | round_half_up(..) | round_down(..) | round_up(..)
//!          | decimal(whole) | float(d)
//! ```
//!
//! A number is an `f64`, unless it ends in `d`: `19.99d` is a
//! [decimal](crate::decimal), exact, and keeps its two places. Decimals and
//! floats do not mix; `round` puts a decimal on a number of places, a tie
//! going to the even neighbour unless another way is named. A name is letters, digits and `_`; any other
//! name, such as `orders[3].qty` or `tax rate`, goes between backticks. A
//! list holds strings. `choose` takes the option at position `index`, an
//! `i64` counting from 0.

use std::sync::Arc;

use crate::decimal::{Decimal, Rounding};
use crate::ir::{BinOp, Ir, UnOp};
use crate::value::{CalcType, Value};

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Decimal(Decimal),
    Text(String),
    Name(String),
    /// A name between backticks: always a node, never a function or `true`.
    Quoted(String),
    Symbol(&'static str),
}

const SYMBOLS: [&str; 19] = [
    "||", "&&", "<=", ">=", "==", "!=", "<", ">", "+", "-", "*", "/", "!", "(", ")", ",", "=", "[",
    "]",
];

fn tokens(src: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit()
            || (c == '.' && chars.get(i + 1).is_some_and(char::is_ascii_digit))
        {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_')
            {
                i += 1;
            }
            // An exponent: 1e6, 2.5e-3.
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let mut j = i + 1;
                if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                    j += 1;
                }
                if j < chars.len() && chars[j].is_ascii_digit() {
                    while j < chars.len() && chars[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                }
            }
            let text: String = chars[start..i].iter().filter(|&&c| c != '_').collect();
            // A `d` straight after the digits makes it a decimal: 19.99d.
            let decimal = chars.get(i) == Some(&'d')
                && !chars
                    .get(i + 1)
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_');
            if decimal {
                i += 1;
                let number = text
                    .parse::<Decimal>()
                    .map_err(|_| format!("{text:?} is not a decimal of up to 28 digits"))?;
                out.push(Token::Decimal(number));
                continue;
            }
            let number = text
                .parse::<f64>()
                .map_err(|_| format!("{text:?} is not a number"))?;
            out.push(Token::Number(number));
        } else if c == '"' || c == '\'' {
            let mut text = String::new();
            i += 1;
            loop {
                match chars.get(i) {
                    None => return Err("a text value is missing its closing quote".into()),
                    Some(&q) if q == c => break,
                    Some(&ch) => text.push(ch),
                }
                i += 1;
            }
            i += 1;
            out.push(Token::Text(text));
        } else if c == '`' {
            let start = i + 1;
            i = start;
            while i < chars.len() && chars[i] != '`' {
                i += 1;
            }
            if i == chars.len() {
                return Err("a name is missing its closing backtick".into());
            }
            out.push(Token::Quoted(chars[start..i].iter().collect()));
            i += 1;
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(Token::Name(chars[start..i].iter().collect()));
        } else {
            let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
            let symbol = SYMBOLS
                .iter()
                .find(|s| rest.starts_with(**s))
                .ok_or_else(|| format!("unexpected {c:?}"))?;
            i += symbol.len();
            out.push(Token::Symbol(symbol));
        }
    }
    Ok(out)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    resolve: &'a dyn Fn(&str) -> Option<Ir>,
    depth: usize,
}

/// Brackets and unary operators can nest this deep; deeper is refused rather
/// than risking the stack.
const MAX_DEPTH: usize = 64;

fn binary(op: BinOp, a: Ir, b: Ir) -> Ir {
    Ir::Binary(op, Arc::new(a), Arc::new(b))
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn symbol(&mut self, options: &[&'static str]) -> Option<&'static str> {
        match self.peek() {
            Some(Token::Symbol(s)) if options.contains(s) => {
                let s = *s;
                self.at += 1;
                Some(s)
            }
            _ => None,
        }
    }

    fn expect(&mut self, symbol: &'static str) -> Result<(), String> {
        match self.symbol(&[symbol]) {
            Some(_) => Ok(()),
            None => Err(match self.peek() {
                Some(found) => format!("expected {symbol:?}, found {}", describe(found)),
                None => format!("expected {symbol:?} at the end"),
            }),
        }
    }

    fn or(&mut self) -> Result<Ir, String> {
        let mut left = self.and()?;
        while self.symbol(&["||"]).is_some() {
            left = binary(BinOp::Or, left, self.and()?);
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Ir, String> {
        let mut left = self.compare()?;
        while self.symbol(&["&&"]).is_some() {
            left = binary(BinOp::And, left, self.compare()?);
        }
        Ok(left)
    }

    fn compare(&mut self) -> Result<Ir, String> {
        let left = self.sum()?;
        let Some(symbol) = self.symbol(&["<=", ">=", "==", "!=", "<", ">", "="]) else {
            return Ok(left);
        };
        let op = match symbol {
            "<" => BinOp::Lt,
            "<=" => BinOp::Le,
            ">" => BinOp::Gt,
            ">=" => BinOp::Ge,
            "!=" => BinOp::Ne,
            _ => BinOp::Eq,
        };
        Ok(binary(op, left, self.sum()?))
    }

    fn sum(&mut self) -> Result<Ir, String> {
        let mut left = self.product()?;
        while let Some(symbol) = self.symbol(&["+", "-"]) {
            let op = if symbol == "+" {
                BinOp::Add
            } else {
                BinOp::Sub
            };
            left = binary(op, left, self.product()?);
        }
        Ok(left)
    }

    fn product(&mut self) -> Result<Ir, String> {
        let mut left = self.unary()?;
        while let Some(symbol) = self.symbol(&["*", "/"]) {
            let op = if symbol == "*" {
                BinOp::Mul
            } else {
                BinOp::Div
            };
            left = binary(op, left, self.unary()?);
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Ir, String> {
        let Some(symbol) = self.symbol(&["-", "!"]) else {
            return self.primary();
        };
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("the formula is nested too deeply".into());
        }
        let inner = self.unary()?;
        self.depth -= 1;
        Ok(match (symbol, inner) {
            // `-3` is the number, not a calculation.
            ("-", Ir::Const(Value::F64(x))) => Ir::Const(Value::F64(-x)),
            ("-", inner) => Ir::Unary(UnOp::Neg, Arc::new(inner)),
            (_, inner) => Ir::Unary(UnOp::Not, Arc::new(inner)),
        })
    }

    /// Formulas separated by commas, up to the closing symbol.
    fn items(&mut self, close: &'static str) -> Result<Vec<Ir>, String> {
        let mut items = Vec::new();
        if self.symbol(&[close]).is_none() {
            loop {
                items.push(self.nested()?);
                if self.symbol(&[","]).is_none() {
                    break;
                }
            }
            self.expect(close)?;
        }
        Ok(items)
    }

    fn arguments(&mut self, name: &str, count: usize) -> Result<Vec<Ir>, String> {
        self.expect("(")?;
        let args = self.items(")")?;
        if args.len() != count {
            return Err(format!(
                "{name} takes {count} value{}, not {}",
                if count == 1 { "" } else { "s" },
                args.len()
            ));
        }
        Ok(args)
    }

    fn nested(&mut self) -> Result<Ir, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("the formula is nested too deeply".into());
        }
        let inner = self.or()?;
        self.depth -= 1;
        Ok(inner)
    }

    fn primary(&mut self) -> Result<Ir, String> {
        let Some(token) = self.peek().cloned() else {
            return Err("the formula ends early".into());
        };
        self.at += 1;
        match token {
            Token::Number(x) => Ok(Ir::Const(Value::F64(x))),
            Token::Decimal(x) => Ok(Ir::Const(x.into_value())),
            Token::Text(s) => Ok(Ir::Const(Value::Str(s.into()))),
            Token::Symbol("(") => {
                let inner = self.nested()?;
                self.expect(")")?;
                Ok(inner)
            }
            Token::Symbol("[") => Ok(Ir::List(
                self.items("]")?.into_iter().map(Arc::new).collect(),
            )),
            Token::Symbol(_) => Err(format!("unexpected {}", describe(&token))),
            Token::Quoted(name) => self.named(&name),
            Token::Name(name) => {
                let call = matches!(self.peek(), Some(Token::Symbol("(")));
                match (name.as_str(), call) {
                    ("true", false) => Ok(Ir::Const(Value::Bool(true))),
                    ("false", false) => Ok(Ir::Const(Value::Bool(false))),
                    ("if", true) => {
                        let mut args = self.arguments("if", 3)?.into_iter().map(Arc::new);
                        let (c, t, e) = (args.next(), args.next(), args.next());
                        Ok(Ir::If(c.unwrap(), t.unwrap(), e.unwrap()))
                    }
                    ("min" | "max", true) => {
                        let mut args = self.arguments(&name, 2)?.into_iter();
                        let op = if name == "min" {
                            BinOp::Min
                        } else {
                            BinOp::Max
                        };
                        Ok(binary(op, args.next().unwrap(), args.next().unwrap()))
                    }
                    ("abs", true) => {
                        let mut args = self.arguments("abs", 1)?.into_iter();
                        Ok(Ir::Unary(UnOp::Abs, Arc::new(args.next().unwrap())))
                    }
                    ("text", true) => {
                        let mut args = self.arguments("text", 1)?.into_iter();
                        Ok(Ir::Unary(UnOp::ToText, Arc::new(args.next().unwrap())))
                    }
                    ("concat" | "join", true) => {
                        let mut args = self.arguments(&name, 2)?.into_iter();
                        let op = if name == "concat" {
                            BinOp::Concat
                        } else {
                            BinOp::Join
                        };
                        Ok(binary(op, args.next().unwrap(), args.next().unwrap()))
                    }
                    ("round" | "round_half_up" | "round_down" | "round_up", true) => {
                        let rounding = match name.as_str() {
                            "round" => Rounding::HalfEven,
                            "round_half_up" => Rounding::HalfUp,
                            "round_down" => Rounding::Down,
                            _ => Rounding::Up,
                        };
                        let mut args = self.arguments(&name, 2)?.into_iter();
                        let (value, places) = (args.next().unwrap(), args.next().unwrap());
                        // A number of places written out (`2`) is a whole number.
                        let places = match places {
                            Ir::Const(Value::F64(x)) if x.fract() == 0.0 && x.abs() < 1e9 => {
                                Ir::Const(Value::I64(x as i64))
                            }
                            other => other,
                        };
                        Ok(binary(BinOp::Round(rounding), value, places))
                    }
                    ("decimal" | "float", true) => {
                        let op = if name == "decimal" {
                            UnOp::ToDecimal
                        } else {
                            UnOp::ToFloat
                        };
                        let mut args = self.arguments(&name, 1)?.into_iter();
                        Ok(Ir::Unary(op, Arc::new(args.next().unwrap())))
                    }
                    ("choose", true) => {
                        self.expect("(")?;
                        let mut args = self.items(")")?.into_iter().map(Arc::new);
                        let index = args.next();
                        let options: Vec<Arc<Ir>> = args.collect();
                        match index {
                            Some(index) if !options.is_empty() => Ok(Ir::Choose(index, options)),
                            _ => Err("choose takes an index and at least one option".into()),
                        }
                    }
                    (_, true) => Err(format!(
                        "there is no function {name:?} (there are if, min, max, abs, text, \
                         concat, join, choose, round, round_half_up, round_down, round_up, \
                         decimal and float)"
                    )),
                    (_, false) => self.named(&name),
                }
            }
        }
    }

    fn named(&self, name: &str) -> Result<Ir, String> {
        (self.resolve)(name).ok_or_else(|| format!("there is no column or value called {name:?}"))
    }
}

fn describe(token: &Token) -> String {
    match token {
        Token::Number(x) => x.to_string(),
        Token::Decimal(x) => format!("{x}d"),
        Token::Text(s) => format!("{s:?}"),
        Token::Name(s) => s.clone(),
        Token::Quoted(s) => format!("`{s}`"),
        Token::Symbol(s) => format!("{s:?}"),
    }
}

/// Parses a formula. `resolve` gives the expression a name stands for (a
/// node, a column's placeholder), or `None` if there is nothing by that
/// name. The result has not been type-checked: making a node or column from
/// it does that.
pub fn parse(src: &str, resolve: &dyn Fn(&str) -> Option<Ir>) -> Result<Ir, String> {
    let tokens = tokens(src)?;
    if tokens.is_empty() {
        return Err("the formula is empty".into());
    }
    let mut parser = Parser {
        tokens,
        at: 0,
        resolve,
        depth: 0,
    };
    let ir = parser.or()?;
    match parser.peek() {
        None => Ok(ir),
        Some(extra) => Err(format!("unexpected {}", describe(extra))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Graph;

    /// Parses a formula over `a = 6`, `b = 4` and `side = "Buy"`, and gives its value as text.
    fn value(src: &str) -> String {
        let mut g = Graph::new();
        let names = [
            ("a", g.input_value("a", Value::F64(6.0)).unwrap()),
            ("b", g.input_value("b", Value::F64(4.0)).unwrap()),
            (
                "side",
                g.input_value("side", Value::Str("Buy".into())).unwrap(),
            ),
        ];
        let ir = parse(src, &|name| {
            names
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, id)| g.reference(*id))
        });
        match ir {
            Err(e) => format!("parse: {e}"),
            Ok(ir) => match g.define_ir("out", ir) {
                Err(e) => format!("define: {e}"),
                Ok(id) => g.value(id).to_string(),
            },
        }
    }

    #[test]
    fn multiplication_binds_tighter_than_addition() {
        assert_eq!(value("a + b * 2"), "14");
        assert_eq!(value("(a + b) * 2"), "20");
        assert_eq!(value("a - b - 1"), "1");
        assert_eq!(value("a / b / 2"), "0.75");
    }

    #[test]
    fn unary_minus_and_not() {
        assert_eq!(value("-a + b"), "-2");
        assert_eq!(value("a * -2"), "-12");
        assert_eq!(value("!(a > b)"), "false");
    }

    #[test]
    fn functions_and_comparisons() {
        assert_eq!(value("if(side == 'Buy', 1, -1) * a"), "6");
        assert_eq!(value("if(side = \"Sell\", 1, -1) * a"), "-6");
        assert_eq!(value("max(a, b) - min(a, b)"), "2");
        assert_eq!(value("abs(b - a)"), "2");
        assert_eq!(value("a > 5 && b > 5 || a == 6"), "true");
        assert_eq!(value("1e3 + 1_000 + .5"), "2000.5");
    }

    #[test]
    fn text_lists_and_choices() {
        assert_eq!(value("concat(side, '!')"), "\"Buy!\"");
        assert_eq!(value("concat(text(a), text(a > b))"), "\"6.0true\"");
        assert_eq!(value("join([side, 'x', text(b)], '-')"), "\"Buy-x-4.0\"");
        assert_eq!(value("join([], ',')"), "\"\"");
        assert_eq!(value("if(side == 'Buy', [side], [])"), "[\"Buy\"]");
        assert_eq!(
            value("choose(a, 1, 2)"),
            "define: type error: a choice's index must be i64, got f64"
        );
        assert_eq!(
            value("choose(a)"),
            "parse: choose takes an index and at least one option"
        );
    }

    #[test]
    fn a_name_between_backticks_can_be_any_name() {
        let mut g = Graph::new();
        g.input("tax rate", 0.2).unwrap();
        g.input("orders[3].qty", 5.0).unwrap();
        g.input("true", 7.0).unwrap();
        g.input("pick", 1_i64).unwrap();
        let total = g
            .define_formula("total", "`orders[3].qty` * (1 + `tax rate`) + `true`")
            .unwrap();
        assert_eq!(g.value(total).to_string(), "13");
        let chosen = g
            .define_formula("chosen", "choose(pick, 'a', 'b')")
            .unwrap();
        assert_eq!(g.value(chosen).to_string(), "\"b\"");
        assert_eq!(
            g.define_formula("bad", "`tax rate")
                .unwrap_err()
                .to_string(),
            "a name is missing its closing backtick"
        );
        assert_eq!(
            g.define_formula("bad", "`nothing` + 1")
                .unwrap_err()
                .to_string(),
            "there is no column or value called \"nothing\""
        );
    }

    #[test]
    fn a_graphs_formula_reads_names_as_its_nodes_and_follows_them() {
        let mut g = Graph::new();
        let qty = g.input("qty", 3.0).unwrap();
        g.input("price", 2.5).unwrap();
        let (_, ty) = g.parse_formula("qty * price > 5").unwrap();
        assert_eq!(ty, crate::Type::Bool);
        let total = g.define_formula("total", "qty * price").unwrap();
        g.set(qty, 4.0).unwrap();
        assert_eq!(g.stabilize().recalculated, 1);
        assert_eq!(g.value(total).to_string(), "10");
        assert!(matches!(
            g.parse_formula("qty && price"),
            Err(crate::CalcError::Type(_))
        ));
        g.check_consistency().unwrap();
    }

    #[test]
    fn a_collections_formula_reads_names_as_its_fields_and_columns_first() {
        use crate::{AggregateOp, Collection, Type};
        let mut g = Graph::new();
        g.input("fx", 2.0).unwrap();
        // A node with a field's name: inside the collection the field wins.
        g.input("qty", 1000.0).unwrap();
        let mut orders = Collection::new(&g, "orders");
        orders.add_field("qty", Type::F64).unwrap();
        orders.add_field("price", Type::F64).unwrap();
        let (line, ty) = orders
            .add_formula_column(&mut g, "line", "qty * price * fx")
            .unwrap();
        assert_eq!(ty, Type::F64);
        orders
            .add_formula_column(&mut g, "big", "line > 10")
            .unwrap();
        let revenue = orders
            .add_total(&mut g, "revenue", AggregateOp::Sum, Some(&line))
            .unwrap();
        for (key, qty) in [("a", 1.0), ("b", 4.0)] {
            orders
                .upsert_named(
                    &mut g,
                    key,
                    &[("qty", Value::F64(qty)), ("price", Value::F64(1.5))],
                )
                .unwrap();
        }
        g.stabilize();
        assert_eq!(g.value(revenue).to_string(), "15");
        let (big, _) = orders.parse_formula(&g, "big").unwrap();
        assert_eq!(
            orders.get_value(&g, "a", &big).unwrap().to_string(),
            "false"
        );
        assert_eq!(orders.get_value(&g, "b", &big).unwrap().to_string(), "true");
        g.check_consistency().unwrap();
    }

    #[test]
    fn decimals_are_written_with_a_d_and_stay_exact() {
        assert_eq!(value("0.1d + 0.2d"), "0.3");
        assert_eq!(value("1.50d * 3d"), "4.50");
        assert_eq!(value("round(2.345d, 2)"), "2.34");
        assert_eq!(value("round_half_up(2.345d, 2)"), "2.35");
        assert_eq!(value("round_down(-2.349d, 2)"), "-2.34");
        assert_eq!(value("round_up(2.341d, 2)"), "2.35");
        assert_eq!(value("float(1.5d) * a"), "9");
        assert_eq!(value("1d / 3d > 0.33d"), "true");
        assert_eq!(value("text(-1.50d)"), "\"-1.50\"");
        assert_eq!(
            value("1.5d + a"),
            "define: type error: Add needs both sides to have the same type, got decimal and f64"
        );
        // A name that starts with d is still a name.
        assert_eq!(value("2 delta"), "parse: unexpected delta");
    }

    #[test]
    fn mistakes_are_explained() {
        assert_eq!(value("a +"), "parse: the formula ends early");
        assert_eq!(value("a b"), "parse: unexpected b");
        assert_eq!(
            value("qty * 2"),
            "parse: there is no column or value called \"qty\""
        );
        assert_eq!(value("min(a)"), "parse: min takes 2 values, not 1");
        assert_eq!(value("(a"), "parse: expected \")\" at the end");
        assert_eq!(
            value("'open"),
            "parse: a text value is missing its closing quote"
        );
        assert!(
            value("a + side").starts_with("define: "),
            "{}",
            value("a + side")
        );
        assert!(value(&"(".repeat(100)).contains("nested too deeply"));
    }
}
