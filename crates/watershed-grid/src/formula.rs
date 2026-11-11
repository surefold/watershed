//! Column formulas as text: `qty * (mark - price)`, `if(side == "Buy", 1, -1)`.
//!
//! A formula becomes a watershed expression ([`Ir`]); the engine type-checks
//! it when the column is added. Names are the grid's columns and its named
//! values. Numbers are always `f64`.
//!
//! ```text
//! formula := or
//! or      := and ( "||" and )*
//! and     := compare ( "&&" compare )*
//! compare := sum ( ( "<" | "<=" | ">" | ">=" | "==" | "!=" ) sum )?
//! sum     := product ( ( "+" | "-" ) product )*
//! product := unary ( ( "*" | "/" ) unary )*
//! unary   := ( "-" | "!" ) unary | primary
//! primary := number | "text" | true | false | name | "(" formula ")"
//!          | if(c, a, b) | min(a, b) | max(a, b) | abs(a)
//! ```

use std::sync::Arc;

use watershed_core::{BinOp, Ir, UnOp, Value};

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Text(String),
    Name(String),
    Symbol(&'static str),
}

const SYMBOLS: [&str; 17] = [
    "||", "&&", "<=", ">=", "==", "!=", "<", ">", "+", "-", "*", "/", "!", "(", ")", ",", "=",
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

    fn arguments(&mut self, name: &str, count: usize) -> Result<Vec<Ir>, String> {
        self.expect("(")?;
        let mut args = Vec::new();
        if self.symbol(&[")"]).is_none() {
            loop {
                args.push(self.nested()?);
                if self.symbol(&[","]).is_none() {
                    break;
                }
            }
            self.expect(")")?;
        }
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
            Token::Text(s) => Ok(Ir::Const(Value::Str(s.into()))),
            Token::Symbol("(") => {
                let inner = self.nested()?;
                self.expect(")")?;
                Ok(inner)
            }
            Token::Symbol(_) => Err(format!("unexpected {}", describe(&token))),
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
                    (_, true) => Err(format!(
                        "there is no function {name:?} (there are if, min, max and abs)"
                    )),
                    (_, false) => (self.resolve)(&name)
                        .ok_or_else(|| format!("there is no column or value called {name:?}")),
                }
            }
        }
    }
}

fn describe(token: &Token) -> String {
    match token {
        Token::Number(x) => x.to_string(),
        Token::Text(s) => format!("{s:?}"),
        Token::Name(s) => s.clone(),
        Token::Symbol(s) => format!("{s:?}"),
    }
}

/// Parses a formula. `resolve` gives the expression for a column or named value.
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
    use watershed_core::Graph;

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
