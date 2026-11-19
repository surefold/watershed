//! Excel's functions, each worked out at once where what it is given is
//! fixed, and made into an expression where that depends on an input.

use std::cmp::Ordering;

use watershed_core::prelude::*;
use watershed_core::{IntoExpr, when};

use crate::book::{Cell, Val};
use crate::model::{Builder, Built, Out, in_range, num, number, order, truth};
use crate::parse::Ast;
use crate::{FIXED_FUNCTIONS, FUNCTIONS};

/// A number that is either known now or depends on an input; worked out
/// at once while both sides are known.
#[derive(Clone)]
enum N {
    Known(f64),
    Varies(Expr<f64>),
}

impl N {
    fn expr(self) -> Expr<f64> {
        match self {
            N::Known(x) => x.into_expr(),
            N::Varies(e) => e,
        }
    }

    fn with(
        self,
        other: N,
        known: impl Fn(f64, f64) -> f64,
        varies: impl Fn(Expr<f64>, Expr<f64>) -> Expr<f64>,
    ) -> N {
        match (self, other) {
            (N::Known(a), N::Known(b)) => N::Known(known(a, b)),
            (a, b) => N::Varies(varies(a.expr(), b.expr())),
        }
    }

    fn add(self, o: N) -> N {
        self.with(o, |a, b| a + b, |a, b| a + b)
    }
    fn mul(self, o: N) -> N {
        self.with(o, |a, b| a * b, |a, b| a * b)
    }
    fn div(self, o: N) -> N {
        self.with(o, |a, b| a / b, |a, b| a / b)
    }
    fn pow(self, o: N) -> N {
        self.with(o, f64::powf, |a, b| a.pow(b))
    }

    /// `ln(1 + x)`, keeping the figures of a small `x`, which `1 + x`
    /// would lose. For a varied `x` it is worked out as Goldberg showed,
    /// with only a logarithm.
    fn ln_1p(self) -> N {
        match self {
            N::Known(x) => N::Known(x.ln_1p()),
            N::Varies(x) => {
                let u = x.clone() + 1.0;
                let exact = u.clone().ln() * x.clone() / (u.clone() - 1.0);
                N::Varies(
                    when(u.clone().equals(1.0)).then(x).otherwise(
                        when(u.clone().equals(f64::INFINITY))
                            .then(u)
                            .otherwise(exact),
                    ),
                )
            }
        }
    }

    /// `e^y - 1`, keeping the figures of a small `y`, likewise.
    fn exp_m1(self) -> N {
        match self {
            N::Known(y) => N::Known(y.exp_m1()),
            N::Varies(y) => {
                let u = y.clone().exp();
                let exact = (u.clone() - 1.0) * y.clone() / u.clone().ln();
                N::Varies(
                    when(u.clone().equals(1.0)).then(y).otherwise(
                        when(u.clone().equals(0.0)).then(-1.0).otherwise(
                            when(u.clone().equals(f64::INFINITY))
                                .then(u)
                                .otherwise(exact),
                        ),
                    ),
                )
            }
        }
    }

    fn out(self) -> Out {
        match self {
            N::Known(x) => Out::Fixed(num(x)),
            N::Varies(e) => Out::Num(e),
        }
    }
}

/// A number, or `None` where Excel would have an error.
fn to_n(o: &Out) -> Option<N> {
    match o {
        Out::Fixed(v) => number(v).map(N::Known),
        Out::Num(e) => Some(N::Varies(e.clone())),
        Out::Bool(b) => Some(N::Varies(when(b.clone()).then(1.0).otherwise(0.0))),
    }
}

fn all_n(outs: &[Out]) -> Option<Vec<N>> {
    outs.iter().map(to_n).collect()
}

/// How a function takes the cells of a range it is given.
#[derive(Clone, Copy, PartialEq)]
enum Take {
    /// Numbers only, as `SUM` does: text, true and false, and empty cells
    /// are passed over.
    Numbers,
    /// Numbers and true and false, as `AND` does.
    Logicals,
}

/// What kind of value something is, for choosing between two.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Number,
    Logical,
    Text,
}

fn kind(o: &Out) -> Kind {
    match o {
        Out::Num(_) | Out::Fixed(Val::Num(_) | Val::Empty | Val::Error) => Kind::Number,
        Out::Bool(_) | Out::Fixed(Val::Bool(_)) => Kind::Logical,
        Out::Fixed(Val::Text(_)) => Kind::Text,
    }
}

/// The rows (or columns) an index might mean, from zero, each with the
/// condition on an input under which it is the one meant, if it depends on
/// one.
type Places = Vec<(u32, Option<Expr<bool>>)>;

/// Which way an approximate lookup searches.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Exact,
    /// Keys from smallest to largest; the largest not above what is wanted.
    Below,
    /// Keys from largest to smallest; the smallest not below what is wanted.
    Above,
}

/// Round as Excel does: to fifteen figures first, so that 2.675 is 2.675
/// and not the 2.67499.. it is held as, then half away from zero.
fn round(x: f64, digits: f64, how: fn(f64) -> f64) -> f64 {
    let fifteen = |x: f64| -> f64 { format!("{x:.14e}").parse().unwrap_or(x) };
    let d = digits.trunc().clamp(-308.0, 308.0) as i32;
    let m = 10f64.powi(d.abs());
    let scaled = fifteen(if d >= 0 {
        fifteen(x) * m
    } else {
        fifteen(x) / m
    });
    let r = how(scaled);
    if d >= 0 { r / m } else { r * m }
}

/// How near a whole number, or a half, a number that depends on an input
/// has to be to count as it: within four and a half parts in a thousand
/// million million. `0.29 * 100` is a little under 29, and Excel still
/// rounds it down to 29, not 28; `628 - 616.715` is a little under 11.285,
/// and Excel still rounds it to 11.29. That is as far as fifteen figures
/// reach for a number that starts with a 1; a sum that loses figures, as
/// that subtraction does, needs all of it.
const NEARLY: f64 = 4.5e-15;

/// `ROUND`, `ROUNDUP` or `ROUNDDOWN` of a number that depends on an input,
/// to a fixed number of places: away from zero at a half, as Excel rounds.
/// It uses only arithmetic that comes out the same on every machine, and
/// reads the number as few times as it can, since each reading works it
/// out again.
fn rounded(x: Expr<f64>, places: f64, name: &str) -> Expr<f64> {
    let d = places.trunc().clamp(-308.0, 308.0) as i32;
    let m = 10f64.powi(d.abs());
    let size = match d >= 0 {
        true => x.clone().abs() * m,
        false => x.clone().abs() / m,
    };
    let whole = match (name, d) {
        ("ROUNDDOWN", _) => (size * (1.0 + NEARLY)).floor(),
        ("ROUNDUP", _) => -(size * (NEARLY - 1.0)).floor(),
        // To no places a half is a half exactly, as LibreOffice has it. The
        // part after the point, doubled, is 1 or more from a half up.
        (_, 0) => {
            let below = size.clone().floor();
            below.clone() + ((size - below) * 2.0).floor()
        }
        _ => (size * (1.0 + NEARLY) + 0.5).floor(),
    };
    let sign = when(x.lt(0.0)).then(-1.0).otherwise(1.0);
    match d >= 0 {
        true => sign * whole / m,
        false => sign * whole * m,
    }
}

impl Builder<'_> {
    pub(crate) fn call(&mut self, name: &str, args: &[Ast]) -> Built<Out> {
        let wants = |b: &Self, n: std::ops::RangeInclusive<usize>| {
            if n.contains(&args.len()) {
                Ok(())
            } else {
                b.fail(format!("{name} given {} arguments", args.len()))
            }
        };
        match name {
            // As LibreOffice writes them.
            "TRUE" | "FALSE" => {
                wants(self, 0..=0)?;
                Ok(Out::Fixed(Val::Bool(name == "TRUE")))
            }
            "IF" => {
                wants(self, 2..=3)?;
                let otherwise = |b: &mut Self| match args.get(2) {
                    Some(e) => b.eval(e),
                    None => Ok(Out::Fixed(Val::Bool(false))),
                };
                match self.eval(&args[0])? {
                    Out::Fixed(v) => match truth(&v) {
                        // Only the branch taken is followed.
                        Some(true) => self.eval(&args[1]),
                        Some(false) => otherwise(self),
                        None => Ok(Out::Fixed(Val::Error)),
                    },
                    condition => {
                        let condition = self.as_bool(condition)?;
                        let then = self.eval(&args[1])?;
                        let other = otherwise(self)?;
                        self.pick(condition, then, other)
                    }
                }
            }
            "IFERROR" => {
                wants(self, 2..=2)?;
                match self.eval(&args[0])? {
                    Out::Fixed(Val::Error) => self.eval(&args[1]),
                    Out::Num(x) => {
                        let instead = self.eval(&args[1])?;
                        if kind(&instead) != Kind::Number {
                            return self.fail(
                                "IFERROR whose number depends on an input, with something else instead",
                            );
                        }
                        let instead = self.as_num(instead)?;
                        // Anything not finite less itself is not-a-number,
                        // and not 0.
                        let fine = (x.clone() - x.clone()).equals(0.0);
                        Ok(Out::Num(when(fine).then(x).otherwise(instead)))
                    }
                    other => Ok(other),
                }
            }
            "AND" | "OR" => {
                let all = self.gather(args, Take::Logicals)?;
                let and = name == "AND";
                if all.is_empty() || all.iter().any(|o| matches!(o, Out::Fixed(Val::Error))) {
                    return Ok(Out::Fixed(Val::Error));
                }
                let mut known = and;
                let mut varies: Option<Expr<bool>> = None;
                for o in all {
                    match o {
                        Out::Fixed(v) => {
                            let Some(t) = truth(&v) else {
                                return Ok(Out::Fixed(Val::Error));
                            };
                            known = if and { known && t } else { known || t };
                        }
                        other => {
                            let b = self.as_bool(other)?;
                            varies = Some(match varies {
                                None => b,
                                Some(v) if and => v & b,
                                Some(v) => v | b,
                            });
                        }
                    }
                }
                Ok(match varies {
                    // Settled by what is known, whatever the rest are.
                    _ if known != and => Out::Fixed(Val::Bool(known)),
                    None => Out::Fixed(Val::Bool(known)),
                    Some(v) => Out::Bool(v),
                })
            }
            "NOT" => {
                wants(self, 1..=1)?;
                Ok(match self.eval(&args[0])? {
                    Out::Fixed(v) => Out::Fixed(truth(&v).map_or(Val::Error, |t| Val::Bool(!t))),
                    other => Out::Bool(!self.as_bool(other)?),
                })
            }
            "SUM" | "AVERAGE" | "MIN" | "MAX" => {
                let all = self.gather(args, Take::Numbers)?;
                let Some(all) = all_n(&all) else {
                    return Ok(Out::Fixed(Val::Error));
                };
                let count = all.len() as f64;
                let Some(first) = all.first().cloned() else {
                    return Ok(Out::Fixed(if name == "AVERAGE" {
                        Val::Error
                    } else {
                        Val::Num(0.0)
                    }));
                };
                let mut result = first;
                for next in all.into_iter().skip(1) {
                    result = match name {
                        "MIN" => result.with(next, f64::min, |a, b| a.min(b)),
                        "MAX" => result.with(next, f64::max, |a, b| a.max(b)),
                        _ => result.add(next),
                    };
                }
                if name == "AVERAGE" {
                    result = result.div(N::Known(count));
                }
                Ok(result.out())
            }
            "ABS" | "EXP" | "LN" | "SQRT" | "LOG10" | "SIGN" => {
                wants(self, 1..=1)?;
                let x = self.eval(&args[0])?;
                if let Out::Fixed(v) = &x {
                    return Ok(Out::Fixed(number(v).map_or(Val::Error, |x| {
                        num(match name {
                            "ABS" => x.abs(),
                            "EXP" => x.exp(),
                            "LN" => x.ln(),
                            "SQRT" => x.sqrt(),
                            "LOG10" => x.log10(),
                            _ if x == 0.0 => 0.0,
                            _ => x.signum(),
                        })
                    })));
                }
                let x = self.as_num(x)?;
                Ok(Out::Num(match name {
                    "ABS" => x.abs(),
                    "EXP" => x.exp(),
                    "LN" => x.ln(),
                    "SQRT" => x.sqrt(),
                    "LOG10" => x.ln() / std::f64::consts::LN_10,
                    _ => when(x.clone().gt(0.0)).then(1.0).otherwise(
                        when(x.clone().lt(0.0))
                            .then(-1.0)
                            .otherwise(when(x.equals(0.0)).then(0.0).otherwise(f64::NAN)),
                    ),
                }))
            }
            "LOG" => {
                wants(self, 1..=2)?;
                let x = self.eval(&args[0])?;
                let base = match args.get(1) {
                    None | Some(Ast::Missing) => Out::Fixed(Val::Num(10.0)),
                    Some(b) => self.eval(b)?,
                };
                let Some([x, base]) =
                    all_n(&[x, base]).map(|v| <[N; 2]>::try_from(v).ok().expect("two"))
                else {
                    return Ok(Out::Fixed(Val::Error));
                };
                let ln = |n: N| match n {
                    N::Known(x) => N::Known(x.ln()),
                    N::Varies(e) => N::Varies(e.ln()),
                };
                Ok(ln(x).div(ln(base)).out())
            }
            "POWER" => {
                wants(self, 2..=2)?;
                let (a, b) = (self.eval(&args[0])?, self.eval(&args[1])?);
                self.binary("^", a, b)
            }
            "NPV" => {
                if args.len() < 2 {
                    return self.fail("NPV given fewer than 2 arguments");
                }
                let rate = self.eval(&args[0])?;
                let mut all = vec![rate];
                all.extend(self.gather(&args[1..], Take::Numbers)?);
                let Some(all) = all_n(&all) else {
                    return Ok(Out::Fixed(Val::Error));
                };
                let growth = all[0].clone().add(N::Known(1.0));
                let mut total = N::Known(0.0);
                for (i, value) in all.into_iter().enumerate().skip(1) {
                    total = total.add(value.div(growth.clone().pow(N::Known(i as f64))));
                }
                Ok(total.out())
            }
            "PMT" => {
                wants(self, 3..=5)?;
                let mut all = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    all.push(match arg {
                        Ast::Missing if i >= 3 => Out::Fixed(Val::Num(0.0)),
                        arg => self.eval(arg)?,
                    });
                }
                while all.len() < 5 {
                    all.push(Out::Fixed(Val::Num(0.0)));
                }
                let due = match &all[4] {
                    Out::Fixed(v) => match number(v) {
                        Some(t) => t != 0.0,
                        None => return Ok(Out::Fixed(Val::Error)),
                    },
                    _ => return self.fail("PMT whose last argument depends on an input"),
                };
                let Some(n) = all_n(&all[..4]) else {
                    return Ok(Out::Fixed(Val::Error));
                };
                let [rate, periods, now, then] = <[N; 4]>::try_from(n).ok().expect("four");
                // With no interest, what is owed paid off in equal parts.
                let flat = now
                    .clone()
                    .add(then.clone())
                    .div(periods.clone())
                    .mul(N::Known(-1.0));
                // What the rate grows to over the periods, less 1, worked
                // out so a rate near 0 keeps its figures.
                let grown_less_1 = rate.clone().ln_1p().mul(periods).exp_m1();
                let grown = grown_less_1.clone().add(N::Known(1.0));
                let mut below = grown_less_1;
                if due {
                    below = below.mul(rate.clone().add(N::Known(1.0)));
                }
                let general = rate
                    .clone()
                    .mul(now.mul(grown).add(then))
                    .div(below)
                    .mul(N::Known(-1.0));
                Ok(match rate {
                    N::Known(0.0) => flat.out(),
                    N::Known(_) => general.out(),
                    N::Varies(r) => Out::Num(
                        when(r.equals(0.0))
                            .then(flat.expr())
                            .otherwise(general.expr()),
                    ),
                })
            }
            "SUMPRODUCT" => self.sumproduct(args),
            "CHOOSE" => self.choose(args),
            "INDEX" => {
                wants(self, 2..=3)?;
                self.index(args)
            }
            "MATCH" => {
                wants(self, 2..=3)?;
                let (first, last) = self.table(&args[1])?;
                if first.row != last.row && first.col != last.col {
                    return Ok(Out::Fixed(Val::Error));
                }
                let mode = match args.get(2) {
                    None | Some(Ast::Missing) => Mode::Below,
                    Some(ast) => match number(&self.fixed(ast, "MATCH's last argument")?) {
                        Some(t) if t > 0.0 => Mode::Below,
                        Some(t) if t < 0.0 => Mode::Above,
                        Some(_) => Mode::Exact,
                        None => return Ok(Out::Fixed(Val::Error)),
                    },
                };
                let keys = self.keys(in_range(first, last))?;
                let places = (1..=keys.len())
                    .map(|k| Out::Fixed(Val::Num(k as f64)))
                    .collect();
                let wanted = self.eval(&args[0])?;
                self.find(keys, places, wanted, mode)
            }
            "VLOOKUP" | "HLOOKUP" => {
                wants(self, 3..=4)?;
                let (first, last) = self.table(&args[1])?;
                let along = self.fixed(&args[2], "the column or row to read")?;
                let Some(along) = number(&along).map(f64::trunc) else {
                    return Ok(Out::Fixed(Val::Error));
                };
                let across = if name == "VLOOKUP" {
                    last.col - first.col + 1
                } else {
                    last.row - first.row + 1
                };
                if along < 1.0 || along > f64::from(across) {
                    return Ok(Out::Fixed(Val::Error));
                }
                let along = along as u32 - 1;
                let nearest = match args.get(3) {
                    None => true,
                    Some(Ast::Missing) => false,
                    Some(ast) => match truth(&self.fixed(ast, "the last argument")?) {
                        Some(t) => t,
                        None => return Ok(Out::Fixed(Val::Error)),
                    },
                };
                let (key_cells, value_cells): (Vec<Cell>, Vec<Cell>) = if name == "VLOOKUP" {
                    (first.row..=last.row)
                        .map(|row| {
                            let at = |col| Cell {
                                sheet: first.sheet,
                                row,
                                col,
                            };
                            (at(first.col), at(first.col + along))
                        })
                        .unzip()
                } else {
                    (first.col..=last.col)
                        .map(|col| {
                            let at = |row| Cell {
                                sheet: first.sheet,
                                row,
                                col,
                            };
                            (at(first.row), at(first.row + along))
                        })
                        .unzip()
                };
                let keys = self.keys(key_cells.into_iter())?;
                let mut values = Vec::new();
                for c in value_cells {
                    values.push(self.cell(c)?);
                }
                let wanted = self.eval(&args[0])?;
                let mode = if nearest { Mode::Below } else { Mode::Exact };
                self.find(keys, values, wanted, mode)
            }
            "COUNT" | "COUNTA" => {
                let mut count = 0;
                for arg in args {
                    match arg {
                        Ast::Cell(a) => count += self.counted(name, in_range(*a, *a))?,
                        Ast::Range(a, b) => count += self.counted(name, in_range(*a, *b))?,
                        Ast::Missing => count += usize::from(name == "COUNTA"),
                        other => {
                            count += usize::from(match self.eval(other)? {
                                Out::Fixed(Val::Error) => name == "COUNTA",
                                Out::Fixed(v @ Val::Text(_)) => {
                                    name == "COUNTA" || number(&v).is_some()
                                }
                                _ => true,
                            })
                        }
                    }
                }
                Ok(Out::Fixed(Val::Num(count as f64)))
            }
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" | "INT" | "MOD" => {
                let wanted = if name == "INT" { 1..=1 } else { 2..=2 };
                wants(self, wanted)?;
                let first = self.eval(&args[0])?;
                let mut all = Vec::new();
                for (at, arg) in args.iter().enumerate() {
                    let out = if at == 0 {
                        first.clone()
                    } else {
                        self.eval(arg)?
                    };
                    match out {
                        Out::Fixed(v) => all.push(number(&v)),
                        _ if name == "MOD" => {
                            return self.fail(
                                "MOD of something that depends on an input is not understood",
                            );
                        }
                        _ if at > 0 => {
                            return self.fail(format!(
                                "{name} to a number of places that depends on an input is not \
                                 understood"
                            ));
                        }
                        _ => {}
                    }
                }
                if !matches!(first, Out::Fixed(_)) {
                    // Only the places are in `all`: the number depends on an input.
                    let places = all.first().copied().unwrap_or(Some(0.0));
                    let Some(places) = places else {
                        return Ok(Out::Fixed(Val::Error));
                    };
                    let x = self.as_num(first)?;
                    return Ok(Out::Num(match name {
                        "INT" => (x.clone() + x.abs() * NEARLY).floor(),
                        _ => rounded(x, places, name),
                    }));
                }
                let Some(all) = all.into_iter().collect::<Option<Vec<f64>>>() else {
                    return Ok(Out::Fixed(Val::Error));
                };
                Ok(Out::Fixed(num(match name {
                    "ROUND" => round(all[0], all[1], f64::round),
                    "ROUNDUP" => round(all[0], all[1], |x| x.abs().ceil() * x.signum()),
                    "ROUNDDOWN" => round(all[0], all[1], f64::trunc),
                    "INT" => all[0].floor(),
                    _ if all[1] == 0.0 => f64::NAN,
                    _ => all[0] - all[1] * (all[0] / all[1]).floor(),
                })))
            }
            _ => {
                debug_assert!(!FUNCTIONS.contains(&name) && !FIXED_FUNCTIONS.contains(&name));
                self.fail(format!("{name} is not understood"))
            }
        }
    }

    /// One of two values, chosen by a condition that depends on an input.
    fn pick(&self, condition: Expr<bool>, then: Out, other: Out) -> Built<Out> {
        match (kind(&then), kind(&other)) {
            (Kind::Logical, Kind::Logical) => Ok(Out::Bool(
                when(condition)
                    .then(self.as_bool(then)?)
                    .otherwise(self.as_bool(other)?),
            )),
            (Kind::Number, Kind::Number) => Ok(Out::Num(
                when(condition)
                    .then(self.as_num(then)?)
                    .otherwise(self.as_num(other)?),
            )),
            (Kind::Text, _) | (_, Kind::Text) => {
                self.fail("a condition that depends on an input chooses text one way")
            }
            _ => self.fail(
                "a condition that depends on an input chooses a number one way and true or false the other",
            ),
        }
    }

    /// A value that must not depend on an input.
    fn fixed(&mut self, ast: &Ast, what: &str) -> Built<Val> {
        match self.eval(ast)? {
            Out::Fixed(v) => Ok(v),
            _ => self.fail(format!("{what} depends on an input")),
        }
    }

    /// The range a lookup reads.
    fn table(&self, ast: &Ast) -> Built<(Cell, Cell)> {
        match ast {
            Ast::Range(a, b) => Ok((*a, *b)),
            Ast::Cell(c) => Ok((*c, *c)),
            _ => self.fail("a lookup into something that is not a range"),
        }
    }

    /// A lookup's keys, which must not depend on an input.
    fn keys(&mut self, cells: impl Iterator<Item = Cell>) -> Built<Vec<Val>> {
        let mut keys = Vec::new();
        for c in cells {
            match self.cell(c)? {
                Out::Fixed(v) => keys.push(v),
                _ => {
                    let name = self.book.name(c);
                    return self.fail(format!(
                        "a lookup whose keys depend on an input, such as {name}"
                    ));
                }
            }
        }
        Ok(keys)
    }

    /// The value beside the key that matches what is wanted.
    fn find(
        &self,
        mut keys: Vec<Val>,
        mut values: Vec<Out>,
        wanted: Out,
        mode: Mode,
    ) -> Built<Out> {
        if mode != Mode::Exact {
            // A table often runs on past its last row.
            while keys.last() == Some(&Val::Empty) {
                keys.pop();
                values.pop();
            }
            let kinds = |v: &Val| match v {
                Val::Num(_) => 0,
                Val::Text(_) => 1,
                Val::Bool(_) => 2,
                Val::Empty | Val::Error => 3,
            };
            if keys.iter().any(|k| kinds(k) == 3) {
                return self.fail("an approximate lookup into keys with gaps or errors among them");
            }
            if keys.windows(2).any(|w| kinds(&w[0]) != kinds(&w[1])) {
                return self.fail("an approximate lookup into keys of more than one kind");
            }
            let wrong = if mode == Mode::Below {
                Ordering::Greater
            } else {
                Ordering::Less
            };
            if keys.windows(2).any(|w| order(&w[0], &w[1]) == Some(wrong)) {
                return self.fail(
                    "an approximate lookup into keys that are not in order, where Excel's answer depends on how it searches",
                );
            }
        }
        let answer = |o: &Out| match o {
            // A lookup that lands on an empty cell gives 0.
            Out::Fixed(Val::Empty) => Out::Fixed(Val::Num(0.0)),
            other => other.clone(),
        };
        match wanted {
            Out::Fixed(Val::Error | Val::Empty) => Ok(Out::Fixed(Val::Error)),
            Out::Fixed(wanted) => {
                if let Val::Text(t) = &wanted {
                    if mode == Mode::Exact && t.contains(['*', '?', '~']) {
                        return self.fail("a lookup of text with wildcards in it");
                    }
                }
                let same_kind =
                    |k: &Val| std::mem::discriminant(k) == std::mem::discriminant(&wanted);
                let found = match mode {
                    Mode::Exact => keys
                        .iter()
                        .position(|k| same_kind(k) && order(k, &wanted) == Some(Ordering::Equal)),
                    Mode::Below => keys.iter().rposition(|k| {
                        same_kind(k) && order(k, &wanted) != Some(Ordering::Greater)
                    }),
                    Mode::Above => keys
                        .iter()
                        .rposition(|k| same_kind(k) && order(k, &wanted) != Some(Ordering::Less)),
                };
                Ok(found.map_or(Out::Fixed(Val::Error), |k| answer(&values[k])))
            }
            Out::Bool(_) => self.fail("a lookup of true or false that depends on an input"),
            Out::Num(wanted) => {
                let mut result: Expr<f64> = f64::NAN.into_expr();
                let rows: Vec<(f64, &Out)> = keys
                    .iter()
                    .zip(&values)
                    .filter_map(|(k, v)| match k {
                        Val::Num(k) => Some((*k, v)),
                        _ => None,
                    })
                    .collect();
                let value = |o: &Out| -> Built<Expr<f64>> {
                    let o = answer(o);
                    if kind(&o) != Kind::Number {
                        return self.fail(
                            "a lookup that depends on an input, into a table of things other than numbers",
                        );
                    }
                    self.as_num(o)
                };
                if mode == Mode::Exact {
                    // The first that matches is the one taken.
                    for &(key, v) in rows.iter().rev() {
                        result = when(wanted.clone().equals(key))
                            .then(value(v)?)
                            .otherwise(result);
                    }
                } else {
                    for &(key, v) in &rows {
                        let reached = if mode == Mode::Below {
                            wanted.clone().ge(key)
                        } else {
                            wanted.clone().le(key)
                        };
                        result = when(reached).then(value(v)?).otherwise(result);
                    }
                }
                Ok(Out::Num(result))
            }
        }
    }

    /// The values of some arguments, with each range as its cells.
    fn gather(&mut self, args: &[Ast], take: Take) -> Built<Vec<Out>> {
        let mut all = Vec::new();
        for arg in args {
            match arg {
                Ast::Cell(_) | Ast::Range(..) => {
                    let (a, b) = self.table(arg)?;
                    for c in in_range(a, b) {
                        let o = self.cell(c)?;
                        let keep = matches!(
                            (&o, take),
                            (Out::Fixed(Val::Num(_) | Val::Error) | Out::Num(_), _)
                                | (Out::Fixed(Val::Bool(_)) | Out::Bool(_), Take::Logicals)
                        );
                        if keep {
                            all.push(o);
                        }
                    }
                }
                Ast::Missing => all.push(Out::Fixed(match take {
                    Take::Numbers => Val::Num(0.0),
                    Take::Logicals => Val::Bool(false),
                })),
                other => {
                    let o = self.eval(other)?;
                    all.push(match (o, take) {
                        (Out::Fixed(v @ Val::Text(_)), Take::Numbers) => {
                            Out::Fixed(number(&v).map_or(Val::Error, Val::Num))
                        }
                        (Out::Fixed(v @ (Val::Bool(_) | Val::Empty)), Take::Numbers) => {
                            Out::Fixed(Val::Num(number(&v).expect("a number")))
                        }
                        (Out::Fixed(v @ (Val::Text(_) | Val::Empty)), Take::Logicals) => {
                            Out::Fixed(truth(&v).map_or(Val::Error, Val::Bool))
                        }
                        (o, _) => o,
                    });
                }
            }
        }
        Ok(all)
    }

    fn counted(&mut self, name: &str, cells: impl Iterator<Item = Cell>) -> Built<usize> {
        let mut count = 0;
        for c in cells {
            count += usize::from(match self.cell(c)? {
                Out::Fixed(Val::Empty) => false,
                Out::Fixed(Val::Num(_)) | Out::Num(_) => true,
                _ => name == "COUNTA",
            });
        }
        Ok(count)
    }

    fn sumproduct(&mut self, args: &[Ast]) -> Built<Out> {
        if args.is_empty() {
            return self.fail("SUMPRODUCT given nothing");
        }
        let mut arrays: Vec<Vec<Out>> = Vec::new();
        let mut shape = None;
        for arg in args {
            let (a, b) = self.table(arg)?;
            let this = (b.row - a.row, b.col - a.col);
            if *shape.get_or_insert(this) != this {
                return Ok(Out::Fixed(Val::Error));
            }
            let mut cells = Vec::new();
            for c in in_range(a, b) {
                cells.push(self.cell(c)?);
            }
            arrays.push(cells);
        }
        let mut total = N::Known(0.0);
        for k in 0..arrays[0].len() {
            let mut product = N::Known(1.0);
            for array in &arrays {
                product = product.mul(match &array[k] {
                    Out::Fixed(Val::Error) => return Ok(Out::Fixed(Val::Error)),
                    Out::Fixed(Val::Num(x)) => N::Known(*x),
                    // Text, true and false, and empty cells count as 0.
                    Out::Fixed(_) | Out::Bool(_) => N::Known(0.0),
                    Out::Num(e) => N::Varies(e.clone()),
                });
            }
            total = total.add(product);
        }
        Ok(total.out())
    }

    fn choose(&mut self, args: &[Ast]) -> Built<Out> {
        if args.len() < 2 {
            return self.fail("CHOOSE given fewer than 2 arguments");
        }
        let options = &args[1..];
        match self.eval(&args[0])? {
            Out::Fixed(v) => match number(&v).map(f64::trunc) {
                Some(k) if k >= 1.0 && k <= options.len() as f64 => {
                    self.eval(&options[k as usize - 1])
                }
                _ => Ok(Out::Fixed(Val::Error)),
            },
            which => {
                let which = self.as_num(which)?;
                let mut result: Expr<f64> = f64::NAN.into_expr();
                for (k, option) in options.iter().enumerate() {
                    let o = self.eval(option)?;
                    if kind(&o) != Kind::Number {
                        return self.fail(
                            "CHOOSE that depends on an input, among things other than numbers",
                        );
                    }
                    let k = (k + 1) as f64;
                    let this = which.clone().ge(k) & which.clone().lt(k + 1.0);
                    result = when(this).then(self.as_num(o)?).otherwise(result);
                }
                Ok(Out::Num(result))
            }
        }
    }

    fn index(&mut self, args: &[Ast]) -> Built<Out> {
        let (first, last) = self.table(&args[0])?;
        let (rows, cols) = (last.row - first.row + 1, last.col - first.col + 1);
        // One number into a single row or column counts along it.
        let (row_arg, col_arg) = match args.get(2) {
            Some(c) => (Some(&args[1]), Some(c)),
            None if rows == 1 => (None, Some(&args[1])),
            None if cols == 1 => (Some(&args[1]), None),
            None => return self.fail("INDEX of a whole row of a table"),
        };
        // Which of the rows (or columns) might be meant: all of them for an
        // index that depends on an input, with the condition for each.
        let along = |b: &mut Self, arg: Option<&Ast>, n: u32| -> Built<Option<Places>> {
            let Some(arg) = arg else {
                return Ok(Some(vec![(0, None)]));
            };
            match b.eval(arg)? {
                Out::Fixed(v) => match number(&v).map(f64::trunc) {
                    Some(0.0) if n == 1 => Ok(Some(vec![(0, None)])),
                    Some(0.0) => b.fail("INDEX of a whole row or column"),
                    Some(k) if k >= 1.0 && k <= f64::from(n) => {
                        Ok(Some(vec![(k as u32 - 1, None)]))
                    }
                    _ => Ok(None),
                },
                which => {
                    let which = b.as_num(which)?;
                    Ok(Some(
                        (0..n)
                            .map(|k| {
                                let k1 = f64::from(k + 1);
                                (k, Some(which.clone().ge(k1) & which.clone().lt(k1 + 1.0)))
                            })
                            .collect(),
                    ))
                }
            }
        };
        let Some(rows) = along(self, row_arg, rows)? else {
            return Ok(Out::Fixed(Val::Error));
        };
        let Some(cols) = along(self, col_arg, cols)? else {
            return Ok(Out::Fixed(Val::Error));
        };
        if rows.len() == 1 && cols.len() == 1 {
            let c = Cell {
                sheet: first.sheet,
                row: first.row + rows[0].0,
                col: first.col + cols[0].0,
            };
            return Ok(match self.cell(c)? {
                Out::Fixed(Val::Empty) => Out::Fixed(Val::Num(0.0)),
                other => other,
            });
        }
        let mut result: Expr<f64> = f64::NAN.into_expr();
        for (r, row_if) in &rows {
            for (c, col_if) in &cols {
                let cell = Cell {
                    sheet: first.sheet,
                    row: first.row + r,
                    col: first.col + c,
                };
                let o = self.cell(cell)?;
                if kind(&o) != Kind::Number {
                    return self
                        .fail("INDEX that depends on an input, into things other than numbers");
                }
                let value = self.as_num(o)?;
                let this = match (row_if.clone(), col_if.clone()) {
                    (Some(a), Some(b)) => a & b,
                    (Some(a), None) | (None, Some(a)) => a,
                    (None, None) => unreachable!("one of them depends on an input"),
                };
                result = when(this).then(value).otherwise(result);
            }
        }
        Ok(Out::Num(result))
    }
}
