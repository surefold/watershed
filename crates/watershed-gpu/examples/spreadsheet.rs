//! A spreadsheet model, tried a million ways.
//!
//! Someone has built a model in Excel: a valuation, a budget, a project's
//! finances. Some of its inputs are guesses, and they want to know how the
//! answer is spread when the guesses are. One way is to put new numbers in
//! the input cells and recalculate the workbook, thousands of times over.
//!
//! This reads the workbook, turns the formulas between the chosen inputs and
//! the chosen outputs into a watershed graph, and runs the graph for every
//! set of inputs at once. Nothing is rewritten by hand, and the workbook is
//! its own check: Excel stores the last value it calculated for every cell,
//! so each cell of the graph is compared with what Excel got.
//!
//!     cargo run --release --features cuda --example spreadsheet -- model.xlsx \
//!         "'Input sheet'!B26=0.02:0.09" "'Input sheet'!B29=0.10:0.18" \
//!         "'Valuation output'!B33"
//!
//! After the workbook: a cell with `=low:high` is an input to vary, between
//! those and most likely what the workbook has now; a cell alone is an
//! output. Last, optionally, how many trials (a million).
//!
//! Only what lies between the inputs and the outputs is translated. A cell
//! that does not depend on a varied input is taken as the value Excel has
//! for it, whatever its formula; and where a condition does not depend on
//! one (`IF(B17="Yes", ..)`), only the branch it takes is followed. So a
//! workbook can be full of things this does not understand, as long as they
//! are not in the way.
//!
//! The workbook must be an `.xlsx` or `.xlsm` file.
//!
//! What it understands in the way: numbers, `+ - * / ^ %`, comparisons,
//! `IF`, `AND`, `OR`, `NOT`, `SUM`, `AVERAGE`, `MIN`, `MAX`, `ABS`, `EXP`,
//! `LN`, `SQRT`, and `VLOOKUP` into a table of constants. Anything else
//! there is refused, with the cell's name. An Excel error (dividing by
//! zero, say) is infinity or not-a-number here.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use calamine::{Data, Range, Reader, open_workbook_auto};
use watershed_core::prelude::*;
use watershed_core::{IntoExpr, NodeId, Value, when};
use watershed_gpu::Scenarios;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// ---- The workbook ----

/// A cell: which sheet, and its row and column from zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Cell {
    sheet: usize,
    row: u32,
    col: u32,
}

/// What a cell holds.
#[derive(Debug, Clone, PartialEq)]
enum Val {
    Num(f64),
    Text(String),
    Bool(bool),
    Empty,
    Error,
}

struct Sheet {
    name: String,
    values: Range<Data>,
    formulas: Range<String>,
}

struct Workbook {
    sheets: Vec<Sheet>,
}

impl Workbook {
    fn open(path: &str) -> Result<Workbook> {
        // An older `.xls` file is read without some of its formulas, and a
        // cell whose formula is missing would be taken for a constant.
        if !path.ends_with(".xlsx") && !path.ends_with(".xlsm") {
            return Err("only .xlsx and .xlsm workbooks are read".into());
        }
        let mut book = open_workbook_auto(path)?;
        let mut sheets = Vec::new();
        for name in book.sheet_names() {
            sheets.push(Sheet {
                values: book.worksheet_range(&name)?,
                formulas: book.worksheet_formula(&name)?,
                name,
            });
        }
        Ok(Workbook { sheets })
    }

    fn sheet(&self, name: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// The value Excel last calculated for the cell.
    fn value(&self, c: Cell) -> Val {
        match self.sheets[c.sheet].values.get_value((c.row, c.col)) {
            None | Some(Data::Empty) => Val::Empty,
            Some(Data::Int(i)) => Val::Num(*i as f64),
            Some(Data::Float(x)) => Val::Num(*x),
            Some(Data::String(s)) => Val::Text(s.clone()),
            Some(Data::Bool(b)) => Val::Bool(*b),
            Some(Data::DateTime(d)) => Val::Num(d.as_f64()),
            Some(Data::Error(_)) => Val::Error,
            Some(Data::DateTimeIso(s) | Data::DurationIso(s)) => Val::Text(s.clone()),
        }
    }

    fn formula(&self, c: Cell) -> Option<&str> {
        self.sheets[c.sheet]
            .formulas
            .get_value((c.row, c.col))
            .map(String::as_str)
            .filter(|f| !f.is_empty())
    }

    fn name(&self, c: Cell) -> String {
        let mut col = String::new();
        let mut n = c.col + 1;
        while n > 0 {
            col.insert(0, (b'A' + ((n - 1) % 26) as u8) as char);
            n = (n - 1) / 26;
        }
        format!("{}!{col}{}", self.sheets[c.sheet].name, c.row + 1)
    }
}

// ---- Formulas ----

#[derive(Debug, Clone)]
enum Ast {
    Num(f64),
    Text(String),
    Bool(bool),
    /// An argument left out, as in `IF(a,,b)`.
    Missing,
    Cell(Cell),
    Range(Cell, Cell),
    Negate(Box<Ast>),
    Percent(Box<Ast>),
    Binary(&'static str, Box<Ast>, Box<Ast>),
    Call(String, Vec<Ast>),
}

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
    /// The sheet the formula is on, for a cell named without one.
    sheet: usize,
    book: &'a Workbook,
}

impl Parser<'_> {
    fn space(&mut self) {
        while self.text.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.space();
        self.text.get(self.at).copied()
    }

    fn take(&mut self, symbol: &str) -> bool {
        self.space();
        let found = self.text[self.at..].starts_with(symbol.as_bytes());
        if found {
            self.at += symbol.len();
        }
        found
    }

    fn fail<T>(&self, what: &str) -> std::result::Result<T, String> {
        Err(format!(
            "{what} at {} in `{}`",
            self.at,
            String::from_utf8_lossy(self.text)
        ))
    }

    fn comparison(&mut self) -> std::result::Result<Ast, String> {
        let mut left = self.joined()?;
        loop {
            // The two-character ones first.
            let Some(op) = ["<=", ">=", "<>", "<", ">", "="]
                .into_iter()
                .find(|op| self.take(op))
            else {
                return Ok(left);
            };
            left = Ast::Binary(op, Box::new(left), Box::new(self.joined()?));
        }
    }

    fn joined(&mut self) -> std::result::Result<Ast, String> {
        let mut left = self.sum()?;
        while self.take("&") {
            left = Ast::Binary("&", Box::new(left), Box::new(self.sum()?));
        }
        Ok(left)
    }

    fn sum(&mut self) -> std::result::Result<Ast, String> {
        let mut left = self.product()?;
        loop {
            let Some(op) = ["+", "-"].into_iter().find(|op| self.take(op)) else {
                return Ok(left);
            };
            left = Ast::Binary(op, Box::new(left), Box::new(self.product()?));
        }
    }

    fn product(&mut self) -> std::result::Result<Ast, String> {
        let mut left = self.power()?;
        loop {
            let Some(op) = ["*", "/"].into_iter().find(|op| self.take(op)) else {
                return Ok(left);
            };
            left = Ast::Binary(op, Box::new(left), Box::new(self.power()?));
        }
    }

    fn power(&mut self) -> std::result::Result<Ast, String> {
        let mut left = self.signed()?;
        while self.take("^") {
            left = Ast::Binary("^", Box::new(left), Box::new(self.signed()?));
        }
        Ok(left)
    }

    /// In Excel a minus sign binds tighter than `^`: `-2^2` is 4.
    fn signed(&mut self) -> std::result::Result<Ast, String> {
        if self.take("-") {
            return Ok(Ast::Negate(Box::new(self.signed()?)));
        }
        if self.take("+") {
            return self.signed();
        }
        let mut inner = self.primary()?;
        while self.take("%") {
            inner = Ast::Percent(Box::new(inner));
        }
        Ok(inner)
    }

    fn primary(&mut self) -> std::result::Result<Ast, String> {
        let Some(c) = self.peek() else {
            return self.fail("the formula ends early");
        };
        if c == b'(' {
            self.at += 1;
            let inner = self.comparison()?;
            if !self.take(")") {
                return self.fail("expected )");
            }
            return Ok(inner);
        }
        if c == b'"' {
            let mut text = String::new();
            self.at += 1;
            loop {
                match self.text.get(self.at) {
                    None => return self.fail("a string does not end"),
                    Some(b'"') if self.text.get(self.at + 1) == Some(&b'"') => {
                        text.push('"');
                        self.at += 2;
                    }
                    Some(b'"') => {
                        self.at += 1;
                        return Ok(Ast::Text(text));
                    }
                    Some(&b) => {
                        text.push(b as char);
                        self.at += 1;
                    }
                }
            }
        }
        if c.is_ascii_digit() || c == b'.' {
            let start = self.at;
            while self.text.get(self.at).is_some_and(|b| {
                b.is_ascii_digit()
                    || *b == b'.'
                    || matches!(b, b'e' | b'E')
                    || (matches!(b, b'+' | b'-') && matches!(self.text[self.at - 1], b'e' | b'E'))
            }) {
                self.at += 1;
            }
            let text = std::str::from_utf8(&self.text[start..self.at]).expect("digits");
            return match text.parse() {
                Ok(x) => Ok(Ast::Num(x)),
                Err(_) => self.fail("not a number"),
            };
        }
        // A sheet's name in quotes, then `!`.
        if c == b'\'' {
            let start = self.at + 1;
            let Some(len) = self.text[start..].iter().position(|&b| b == b'\'') else {
                return self.fail("a sheet's name does not end");
            };
            let name = String::from_utf8_lossy(&self.text[start..start + len]).into_owned();
            self.at = start + len + 1;
            if !self.take("!") {
                return self.fail("expected ! after a sheet's name");
            }
            let Some(sheet) = self.book.sheet(&name) else {
                return self.fail("no such sheet");
            };
            return self.cells(sheet);
        }
        // A sheet's name written without quotes though it has spaces in it,
        // as older workbooks have them.
        let rest = &self.text[self.at..];
        let unquoted = (0..self.book.sheets.len())
            .filter(|&k| {
                let name = self.book.sheets[k].name.as_bytes();
                rest.len() > name.len()
                    && rest[..name.len()].eq_ignore_ascii_case(name)
                    && rest[name.len()] == b'!'
            })
            .max_by_key(|&k| self.book.sheets[k].name.len());
        if let Some(sheet) = unquoted {
            self.at += self.book.sheets[sheet].name.len() + 1;
            return self.cells(sheet);
        }
        // A name: a function, TRUE or FALSE, a sheet, or a cell.
        let start = self.at;
        while self
            .text
            .get(self.at)
            .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'$'))
        {
            self.at += 1;
        }
        if self.at == start {
            return self.fail("unexpected character");
        }
        let name = String::from_utf8_lossy(&self.text[start..self.at]).to_uppercase();
        if self.text.get(self.at) == Some(&b'(') {
            self.at += 1;
            let mut args = Vec::new();
            if !self.take(")") {
                loop {
                    args.push(match self.peek() {
                        Some(b',' | b')') => Ast::Missing,
                        _ => self.comparison()?,
                    });
                    if self.take(")") {
                        break;
                    }
                    if !self.take(",") {
                        return self.fail("expected , or )");
                    }
                }
            }
            return Ok(Ast::Call(name, args));
        }
        if self.text.get(self.at) == Some(&b'!') {
            self.at += 1;
            let Some(sheet) = self.book.sheet(&name) else {
                return self.fail("no such sheet");
            };
            return self.cells(sheet);
        }
        match name.as_str() {
            "TRUE" => Ok(Ast::Bool(true)),
            "FALSE" => Ok(Ast::Bool(false)),
            _ => {
                self.at = start;
                self.cells(self.sheet)
            }
        }
    }

    /// A cell, or two with a colon between: a range.
    fn cells(&mut self, sheet: usize) -> std::result::Result<Ast, String> {
        let first = self.cell(sheet)?;
        if self.text.get(self.at) == Some(&b':') {
            self.at += 1;
            let last = self.cell(sheet)?;
            return Ok(Ast::Range(first, last));
        }
        Ok(Ast::Cell(first))
    }

    fn cell(&mut self, sheet: usize) -> std::result::Result<Cell, String> {
        let mut col = 0u32;
        let mut letters = 0;
        if self.text.get(self.at) == Some(&b'$') {
            self.at += 1;
        }
        while let Some(b) = self.text.get(self.at).filter(|b| b.is_ascii_alphabetic()) {
            col = col * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1;
            letters += 1;
            self.at += 1;
        }
        if self.text.get(self.at) == Some(&b'$') {
            self.at += 1;
        }
        let digits = self.at;
        while self.text.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        let row = std::str::from_utf8(&self.text[digits..self.at])
            .expect("digits")
            .parse::<u32>();
        match row {
            Ok(row) if (1..=3).contains(&letters) && row > 0 => Ok(Cell {
                sheet,
                row: row - 1,
                col: col - 1,
            }),
            _ => self.fail("expected a cell"),
        }
    }
}

fn parse(book: &Workbook, sheet: usize, formula: &str) -> std::result::Result<Ast, String> {
    let mut parser = Parser {
        text: formula.trim_start_matches('=').as_bytes(),
        at: 0,
        sheet,
        book,
    };
    let ast = parser.comparison()?;
    if parser.peek().is_some() {
        return parser.fail("unexpected text");
    }
    Ok(ast)
}

/// A cell given on the command line.
fn cell_named(book: &Workbook, text: &str) -> Result<Cell> {
    match parse(book, 0, text)? {
        Ast::Cell(c) => Ok(c),
        _ => Err(format!("{text} is not a cell").into()),
    }
}

// ---- From formulas to a graph ----

/// What a formula, or part of one, comes to.
#[derive(Clone)]
enum Out {
    /// The same whatever the varied inputs are.
    Fixed(Val),
    Num(Expr<f64>),
    Bool(Expr<bool>),
}

fn number(v: &Val) -> Option<f64> {
    match v {
        Val::Num(x) => Some(*x),
        Val::Bool(b) => Some(f64::from(u8::from(*b))),
        Val::Empty => Some(0.0),
        Val::Text(t) => t.trim().parse().ok(),
        Val::Error => None,
    }
}

/// How Excel orders two values: numbers, then text (ignoring case), then
/// true and false. An empty cell is 0 beside a number and "" beside text.
fn order(a: &Val, b: &Val) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    let rank = |v: &Val| match v {
        Val::Num(_) => 0,
        Val::Text(_) => 1,
        Val::Bool(_) => 2,
        Val::Empty | Val::Error => 3,
    };
    match (a, b) {
        (Val::Error, _) | (_, Val::Error) => None,
        (Val::Empty, Val::Empty) => Some(Ordering::Equal),
        (Val::Empty, Val::Num(_)) => order(&Val::Num(0.0), b),
        (Val::Num(_), Val::Empty) => order(a, &Val::Num(0.0)),
        (Val::Empty, Val::Text(_)) => order(&Val::Text(String::new()), b),
        (Val::Text(_), Val::Empty) => order(a, &Val::Text(String::new())),
        (Val::Empty, Val::Bool(_)) => order(&Val::Bool(false), b),
        (Val::Bool(_), Val::Empty) => order(a, &Val::Bool(false)),
        (Val::Num(x), Val::Num(y)) => x.partial_cmp(y),
        (Val::Text(x), Val::Text(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
        (Val::Bool(x), Val::Bool(y)) => Some(x.cmp(y)),
        _ => Some(rank(a).cmp(&rank(b))),
    }
}

struct Model<'a> {
    book: &'a Workbook,
    g: Graph,
    inputs: HashMap<Cell, Input<f64>>,
    /// What each cell reached so far comes to.
    cells: HashMap<Cell, Out>,
    /// Whether each cell reached so far depends on a varied input.
    depends: HashMap<Cell, bool>,
    /// The cells that became nodes, to check against Excel.
    nodes: Vec<(Cell, NodeId)>,
    /// Cells that came to a fixed value by following their formula.
    folded: usize,
    /// Of those, the ones that came to something other than Excel has.
    folded_wrong: Vec<String>,
    functions: HashMap<String, usize>,
}

impl<'a> Model<'a> {
    fn new(book: &'a Workbook, vary: &[Cell]) -> Result<Model<'a>> {
        let mut g = Graph::new();
        let mut inputs = HashMap::new();
        for &c in vary {
            let now = number(&book.value(c))
                .ok_or_else(|| format!("{} does not hold a number", book.name(c)))?;
            inputs.insert(c, g.input(&book.name(c), now)?);
        }
        Ok(Model {
            book,
            g,
            inputs,
            cells: HashMap::new(),
            depends: HashMap::new(),
            nodes: Vec::new(),
            folded: 0,
            folded_wrong: Vec::new(),
            functions: HashMap::new(),
        })
    }

    /// Whether a cell's formula mentions, through any number of cells, a
    /// varied input. This follows every branch, so it may say yes where the
    /// branch that mentions one is never taken; that is found out later.
    fn depends(&mut self, c: Cell) -> std::result::Result<bool, String> {
        if self.inputs.contains_key(&c) {
            return Ok(true);
        }
        if let Some(&known) = self.depends.get(&c) {
            return Ok(known);
        }
        // Until it is known, no: a cell that reads itself does not depend
        // on an input by doing so.
        self.depends.insert(c, false);
        let depends = match self.book.formula(c) {
            None => false,
            Some(formula) => {
                let ast = parse(self.book, c.sheet, formula)
                    .map_err(|e| format!("{}: {e}", self.book.name(c)))?;
                self.mentions(&ast)?
            }
        };
        self.depends.insert(c, depends);
        Ok(depends)
    }

    fn mentions(&mut self, ast: &Ast) -> std::result::Result<bool, String> {
        Ok(match ast {
            Ast::Num(_) | Ast::Text(_) | Ast::Bool(_) | Ast::Missing => false,
            Ast::Cell(c) => self.depends(*c)?,
            Ast::Range(a, b) => {
                let mut any = false;
                for c in in_range(*a, *b) {
                    any |= self.depends(c)?;
                }
                any
            }
            Ast::Negate(a) | Ast::Percent(a) => self.mentions(a)?,
            Ast::Binary(_, a, b) => self.mentions(a)? | self.mentions(b)?,
            Ast::Call(_, args) => {
                let mut any = false;
                for arg in args {
                    any |= self.mentions(arg)?;
                }
                any
            }
        })
    }

    /// What a cell comes to, making nodes for it and whatever it reads.
    fn cell(&mut self, c: Cell) -> std::result::Result<Out, String> {
        if let Some(input) = self.inputs.get(&c) {
            return Ok(Out::Num(input.into_expr()));
        }
        if let Some(known) = self.cells.get(&c) {
            return Ok(known.clone());
        }
        let out = if !self.depends(c)? {
            Out::Fixed(self.book.value(c))
        } else {
            let formula = self
                .book
                .formula(c)
                .expect("a cell that depends has a formula");
            let ast = parse(self.book, c.sheet, formula)
                .map_err(|e| format!("{}: {e}", self.book.name(c)))?;
            let name = self.book.name(c);
            let define = |e: CalcError| format!("{name}: {e}");
            match self.eval(&ast).map_err(|e| format!("{name}: {e}"))? {
                Out::Fixed(v) => {
                    // The formula was followed; it should have come to what
                    // Excel has.
                    self.folded += 1;
                    let excel = self.book.value(c);
                    let same = match (&v, &excel) {
                        (Val::Num(a), Val::Num(b)) => close(*a, *b),
                        (a, b) => order(a, b) == Some(std::cmp::Ordering::Equal),
                    };
                    if !same {
                        self.folded_wrong
                            .push(format!("{name}: {v:?} here, {excel:?} in Excel"));
                    }
                    Out::Fixed(v)
                }
                Out::Num(e) => {
                    let node = self.g.define(&name, e).map_err(define)?;
                    self.nodes.push((c, node.id()));
                    Out::Num(node.into_expr())
                }
                Out::Bool(e) => {
                    let node = self.g.define(&name, e).map_err(define)?;
                    self.nodes.push((c, node.id()));
                    Out::Bool(node.into_expr())
                }
            }
        };
        self.cells.insert(c, out.clone());
        Ok(out)
    }

    fn eval(&mut self, ast: &Ast) -> std::result::Result<Out, String> {
        match ast {
            Ast::Num(x) => Ok(Out::Fixed(Val::Num(*x))),
            Ast::Text(t) => Ok(Out::Fixed(Val::Text(t.clone()))),
            Ast::Bool(b) => Ok(Out::Fixed(Val::Bool(*b))),
            Ast::Missing => Ok(Out::Fixed(Val::Empty)),
            Ast::Cell(c) => self.cell(*c),
            Ast::Range(..) => Err("a range where one value is wanted".into()),
            Ast::Negate(a) => Ok(match self.eval(a)? {
                Out::Fixed(v) => Out::Fixed(number(&v).map_or(Val::Error, |x| Val::Num(-x))),
                other => Out::Num(-as_num(other)?),
            }),
            Ast::Percent(a) => Ok(match self.eval(a)? {
                Out::Fixed(v) => Out::Fixed(number(&v).map_or(Val::Error, |x| Val::Num(x / 100.0))),
                other => Out::Num(as_num(other)? / 100.0),
            }),
            Ast::Binary(op, a, b) => {
                let (a, b) = (self.eval(a)?, self.eval(b)?);
                binary(op, a, b)
            }
            Ast::Call(name, args) => {
                *self.functions.entry(name.clone()).or_default() += 1;
                self.call(name, args)
            }
        }
    }

    fn call(&mut self, name: &str, args: &[Ast]) -> std::result::Result<Out, String> {
        let wants = |n: std::ops::RangeInclusive<usize>| match n.contains(&args.len()) {
            true => Ok(()),
            false => Err(format!("{name} with {} arguments", args.len())),
        };
        match name {
            "IF" => {
                wants(2..=3)?;
                let otherwise = |model: &mut Model| match args.get(2) {
                    Some(e) => model.eval(e),
                    None => Ok(Out::Fixed(Val::Bool(false))),
                };
                match self.eval(&args[0])? {
                    // Only the branch taken is followed.
                    Out::Fixed(Val::Error) => Ok(Out::Fixed(Val::Error)),
                    Out::Fixed(v) => match truth(&v) {
                        Some(true) => self.eval(&args[1]),
                        Some(false) => otherwise(self),
                        None => Err(format!("a condition that is {v:?}")),
                    },
                    condition => {
                        let condition = as_bool(condition)?;
                        let (then, other) = (self.eval(&args[1])?, otherwise(self)?);
                        Ok(match (&then, &other) {
                            (Out::Bool(_), _) | (_, Out::Bool(_))
                                if !matches!(then, Out::Num(_))
                                    && !matches!(other, Out::Num(_))
                                    && is_bool(&then)
                                    && is_bool(&other) =>
                            {
                                Out::Bool(
                                    when(condition)
                                        .then(as_bool(then)?)
                                        .otherwise(as_bool(other)?),
                                )
                            }
                            _ => Out::Num(
                                when(condition)
                                    .then(as_num(then)?)
                                    .otherwise(as_num(other)?),
                            ),
                        })
                    }
                }
            }
            "AND" | "OR" => {
                let all = self.values(args)?;
                let and = name == "AND";
                if all.iter().all(|o| matches!(o, Out::Fixed(_))) {
                    let mut result = and;
                    for o in &all {
                        let Out::Fixed(v) = o else { unreachable!() };
                        let Some(t) = truth(v) else {
                            return Ok(Out::Fixed(Val::Error));
                        };
                        result = if and { result && t } else { result || t };
                    }
                    return Ok(Out::Fixed(Val::Bool(result)));
                }
                let mut all = all.into_iter().map(as_bool);
                let mut result = all.next().expect("an argument")?;
                for next in all {
                    result = if and { result & next? } else { result | next? };
                }
                Ok(Out::Bool(result))
            }
            "NOT" => {
                wants(1..=1)?;
                Ok(match self.eval(&args[0])? {
                    Out::Fixed(v) => Out::Fixed(truth(&v).map_or(Val::Error, |t| Val::Bool(!t))),
                    other => Out::Bool(!as_bool(other)?),
                })
            }
            "SUM" | "AVERAGE" | "MIN" | "MAX" => {
                let all = self.values(args)?;
                if all.is_empty() {
                    return Ok(Out::Fixed(Val::Num(0.0)));
                }
                let count = all.len() as f64;
                if let Some(fixed) = all
                    .iter()
                    .map(|o| match o {
                        Out::Fixed(v) => number(v),
                        _ => None,
                    })
                    .collect::<Option<Vec<f64>>>()
                {
                    let first = fixed[0];
                    return Ok(Out::Fixed(Val::Num(match name {
                        "SUM" => fixed.iter().sum(),
                        "AVERAGE" => fixed.iter().sum::<f64>() / count,
                        "MIN" => fixed.iter().copied().fold(first, f64::min),
                        _ => fixed.iter().copied().fold(first, f64::max),
                    })));
                }
                let mut all = all.into_iter().map(as_num);
                let mut result = all.next().expect("an argument")?;
                for next in all {
                    let next = next?;
                    result = match name {
                        "MIN" => result.min(next),
                        "MAX" => result.max(next),
                        _ => result + next,
                    };
                }
                Ok(Out::Num(if name == "AVERAGE" {
                    result / count
                } else {
                    result
                }))
            }
            "ABS" | "EXP" | "LN" | "SQRT" => {
                wants(1..=1)?;
                Ok(match self.eval(&args[0])? {
                    Out::Fixed(v) => Out::Fixed(number(&v).map_or(Val::Error, |x| {
                        Val::Num(match name {
                            "ABS" => x.abs(),
                            "EXP" => x.exp(),
                            "LN" => x.ln(),
                            _ => x.sqrt(),
                        })
                    })),
                    other => {
                        let x = as_num(other)?;
                        Out::Num(match name {
                            "ABS" => x.abs(),
                            "EXP" => x.exp(),
                            "LN" => x.ln(),
                            _ => x.sqrt(),
                        })
                    }
                })
            }
            "VLOOKUP" => {
                wants(3..=4)?;
                self.lookup(args)
            }
            _ => Err(format!("{name} is not understood")),
        }
    }

    /// The values of some arguments, with each range as its cells. As in
    /// Excel, text and empty cells in a range are passed over.
    fn values(&mut self, args: &[Ast]) -> std::result::Result<Vec<Out>, String> {
        let mut all = Vec::new();
        for arg in args {
            match arg {
                Ast::Range(a, b) => {
                    for c in in_range(*a, *b) {
                        match self.cell(c)? {
                            Out::Fixed(Val::Text(_) | Val::Empty | Val::Bool(_)) => {}
                            other => all.push(other),
                        }
                    }
                }
                other => all.push(self.eval(other)?),
            }
        }
        Ok(all)
    }

    /// `VLOOKUP(value, table, column, [nearest])` into a table that does
    /// not depend on a varied input.
    fn lookup(&mut self, args: &[Ast]) -> std::result::Result<Out, String> {
        let Ast::Range(first, last) = &args[1] else {
            return Err("VLOOKUP into something that is not a range".into());
        };
        let fixed = |model: &mut Model, ast: &Ast, what: &str| match model.eval(ast)? {
            Out::Fixed(v) => Ok(v),
            _ => Err(format!("VLOOKUP whose {what} depends on a varied input")),
        };
        let column = number(&fixed(self, &args[2], "column")?).ok_or("VLOOKUP column")? as u32;
        let nearest = match args.get(3) {
            None => true,
            Some(ast) => truth(&fixed(self, ast, "last argument")?).unwrap_or(true),
        };
        let mut rows = Vec::new();
        for row in first.row..=last.row {
            let at = |col| Cell {
                sheet: first.sheet,
                row,
                col,
            };
            let (key, value) = (
                self.cell(at(first.col))?,
                self.cell(at(first.col + column - 1))?,
            );
            let (Out::Fixed(key), Out::Fixed(value)) = (key, value) else {
                return Err("VLOOKUP into a table that depends on a varied input".into());
            };
            rows.push((key, value));
        }
        use std::cmp::Ordering;
        match self.eval(&args[0])? {
            Out::Fixed(wanted) => Ok(Out::Fixed(if nearest {
                // The last row whose key is not above what is wanted.
                rows.iter()
                    .take_while(|(key, _)| {
                        matches!(order(key, &wanted), Some(Ordering::Less | Ordering::Equal))
                    })
                    .last()
                    .map_or(Val::Error, |(_, value)| value.clone())
            } else {
                rows.iter()
                    .find(|(key, _)| order(key, &wanted) == Some(Ordering::Equal))
                    .map_or(Val::Error, |(_, value)| value.clone())
            })),
            wanted => {
                let wanted = as_num(wanted)?;
                let rows: Vec<(f64, f64)> = rows
                    .iter()
                    .filter_map(|(key, value)| match (key, value) {
                        (Val::Num(k), v) => Some((*k, number(v)?)),
                        _ => None,
                    })
                    .collect();
                if rows.is_empty() {
                    return Err("VLOOKUP of a number into a table with no numbers".into());
                }
                // Where Excel has "not found", this has not-a-number.
                let mut result: Expr<f64> = f64::NAN.into_expr();
                if nearest {
                    for &(key, value) in &rows {
                        result = when(wanted.clone().ge(key)).then(value).otherwise(result);
                    }
                } else {
                    for &(key, value) in rows.iter().rev() {
                        result = when(wanted.clone().equals(key))
                            .then(value)
                            .otherwise(result);
                    }
                }
                Ok(Out::Num(result))
            }
        }
    }
}

fn in_range(a: Cell, b: Cell) -> impl Iterator<Item = Cell> {
    (a.row..=b.row).flat_map(move |row| {
        (a.col..=b.col).map(move |col| Cell {
            sheet: a.sheet,
            row,
            col,
        })
    })
}

fn truth(v: &Val) -> Option<bool> {
    match v {
        Val::Bool(b) => Some(*b),
        Val::Num(x) => Some(*x != 0.0),
        Val::Empty => Some(false),
        Val::Text(_) | Val::Error => None,
    }
}

fn is_bool(o: &Out) -> bool {
    matches!(o, Out::Bool(_) | Out::Fixed(Val::Bool(_)))
}

fn as_num(o: Out) -> std::result::Result<Expr<f64>, String> {
    match o {
        Out::Num(e) => Ok(e),
        Out::Bool(b) => Ok(when(b).then(1.0).otherwise(0.0)),
        Out::Fixed(v) => number(&v)
            .map(IntoExpr::into_expr)
            .ok_or_else(|| format!("{v:?} where a number is wanted")),
    }
}

fn as_bool(o: Out) -> std::result::Result<Expr<bool>, String> {
    match o {
        Out::Bool(b) => Ok(b),
        Out::Num(e) => Ok(e.not_equals(0.0)),
        Out::Fixed(v) => truth(&v)
            .map(IntoExpr::into_expr)
            .ok_or_else(|| format!("{v:?} where true or false is wanted")),
    }
}

fn binary(op: &str, a: Out, b: Out) -> std::result::Result<Out, String> {
    use std::cmp::Ordering::*;
    let compares = matches!(op, "=" | "<>" | "<" | ">" | "<=" | ">=");
    if let (Out::Fixed(a), Out::Fixed(b)) = (&a, &b) {
        if compares {
            let Some(o) = order(a, b) else {
                return Ok(Out::Fixed(Val::Error));
            };
            return Ok(Out::Fixed(Val::Bool(match op {
                "=" => o == Equal,
                "<>" => o != Equal,
                "<" => o == Less,
                ">" => o == Greater,
                "<=" => o != Greater,
                _ => o != Less,
            })));
        }
        if op == "&" {
            let text = |v: &Val| match v {
                Val::Text(t) => t.clone(),
                Val::Num(x) => x.to_string(),
                Val::Bool(b) => b.to_string().to_uppercase(),
                Val::Empty | Val::Error => String::new(),
            };
            return Ok(Out::Fixed(Val::Text(text(a) + &text(b))));
        }
        let (Some(x), Some(y)) = (number(a), number(b)) else {
            return Ok(Out::Fixed(Val::Error));
        };
        return Ok(Out::Fixed(Val::Num(match op {
            "+" => x + y,
            "-" => x - y,
            "*" => x * y,
            "/" => x / y,
            _ => x.powf(y),
        })));
    }
    if op == "&" {
        return Err("text joined to something that depends on a varied input".into());
    }
    let (a, b) = (as_num(a)?, as_num(b)?);
    Ok(match op {
        "+" => Out::Num(a + b),
        "-" => Out::Num(a - b),
        "*" => Out::Num(a * b),
        "/" => Out::Num(a / b),
        "^" => Out::Num(a.pow(b)),
        "=" => Out::Bool(a.equals(b)),
        "<>" => Out::Bool(a.not_equals(b)),
        "<" => Out::Bool(a.lt(b)),
        ">" => Out::Bool(a.gt(b)),
        "<=" => Out::Bool(a.le(b)),
        _ => Out::Bool(a.ge(b)),
    })
}

/// The same to about twelve figures: Excel stores seventeen and works
/// things out in an order of its own.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6) || (a.is_nan() && b.is_nan())
}

// ---- Running it ----

/// An input to vary: between `low` and `high`, most likely what the
/// workbook has now.
struct Vary {
    cell: Cell,
    low: f64,
    now: f64,
    high: f64,
}

impl Vary {
    /// A draw from the triangle, given a number between 0 and 1.
    fn draw(&self, u: f64) -> f64 {
        let (span, left) = (self.high - self.low, self.now - self.low);
        if u * span < left {
            self.low + (u * span * left).sqrt()
        } else {
            self.high - ((1.0 - u) * span * (self.high - self.now)).sqrt()
        }
    }
}

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("give a workbook")?;
    let started = Instant::now();
    let book = Workbook::open(&path)?;
    let formulas: usize = book
        .sheets
        .iter()
        .map(|s| s.formulas.used_cells().filter(|c| !c.2.is_empty()).count())
        .sum();
    println!(
        "{}: {} sheets, {} formulas (read in {})",
        path.rsplit('/').next().unwrap_or(&path),
        book.sheets.len(),
        formulas,
        time(started.elapsed())
    );

    let (mut vary, mut outputs, mut trials) = (Vec::new(), Vec::new(), 1_000_000usize);
    for arg in args {
        if let Some((cell, between)) = arg.split_once('=') {
            let cell = cell_named(&book, cell)?;
            let (low, high) = between.split_once(':').ok_or("an input is CELL=low:high")?;
            let now = number(&book.value(cell)).ok_or("an input must hold a number")?;
            let (low, high): (f64, f64) = (low.parse()?, high.parse()?);
            if !(low <= now && now <= high && low < high) {
                return Err(format!(
                    "{} is {now} in the workbook, which is not between {low} and {high}",
                    book.name(cell)
                )
                .into());
            }
            vary.push(Vary {
                cell,
                low,
                now,
                high,
            });
        } else if let Ok(n) = arg.replace('_', "").parse() {
            trials = n;
        } else {
            outputs.push(cell_named(&book, &arg)?);
        }
    }
    if vary.is_empty() || outputs.is_empty() {
        return Err("give at least one input (CELL=low:high) and one output (CELL)".into());
    }

    // The formulas between the inputs and the outputs, as a graph.
    let started = Instant::now();
    let cells: Vec<Cell> = vary.iter().map(|v| v.cell).collect();
    let mut model = Model::new(&book, &cells)?;
    let mut output_ids = Vec::new();
    for &c in &outputs {
        match model.cell(c)? {
            Out::Num(_) => {
                let (_, id) = model
                    .nodes
                    .iter()
                    .find(|(cell, _)| *cell == c)
                    .ok_or_else(|| format!("{} is an input, not an output", book.name(c)))?;
                output_ids.push(*id);
            }
            Out::Bool(_) => return Err(format!("{} is true or false", book.name(c)).into()),
            Out::Fixed(_) => {
                return Err(format!("{} does not depend on any input given", book.name(c)).into());
            }
        }
    }
    model.g.stabilize();
    let mut used: Vec<(&String, &usize)> = model.functions.iter().collect();
    used.sort();
    println!(
        "between the {} inputs and the {} outputs: {} cells became nodes; {} more were followed to a fixed value (built in {})",
        vary.len(),
        outputs.len(),
        model.nodes.len(),
        model.folded,
        time(started.elapsed())
    );
    println!(
        "  functions met on the way: {}",
        used.iter()
            .map(|(name, n)| format!("{name} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Each cell against what Excel calculated.
    let mut wrong = model.folded_wrong.clone();
    for &(c, id) in &model.nodes {
        let same = match (model.g.value(id), book.value(c)) {
            (Value::F64(here), Val::Num(excel)) => close(*here, excel),
            (Value::Bool(here), Val::Bool(excel)) => *here == excel,
            (Value::F64(here), Val::Empty) => *here == 0.0,
            // An error in Excel is not a finite number here.
            (Value::F64(here), Val::Error) => !here.is_finite(),
            _ => false,
        };
        if !same {
            wrong.push(format!(
                "{}: {:?} here, {:?} in Excel",
                book.name(c),
                model.g.value(id),
                book.value(c)
            ));
        }
    }
    let checked = model.nodes.len() + model.folded;
    if wrong.is_empty() {
        println!("checked: all {checked} of those cells come to what Excel has for them");
    } else {
        println!("{} of {checked} cells differ from Excel:", wrong.len());
        for line in wrong.iter().take(20) {
            println!("  {line}");
        }
        return Err("the graph does not agree with the workbook".into());
    }
    println!();

    let vary_ids: Vec<NodeId> = vary.iter().map(|v| model.inputs[&v.cell].id()).collect();
    let plan = Scenarios::new(&model.g, &vary_ids, &output_ids)?;

    // The trials.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut inputs = Vec::with_capacity(trials * vary.len());
    for _ in 0..trials {
        for v in &vary {
            inputs.push(v.draw(rng.unit()));
        }
    }
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    let started = Instant::now();
    let results = plan.run_cpu(&inputs);
    let cpu_took = started.elapsed();
    println!(
        "{} trials on {cores} CPU cores in {} ({} recalculations of the model a second)",
        commas(trials as f64),
        time(cpu_took),
        commas(trials as f64 / cpu_took.as_secs_f64())
    );
    #[cfg(feature = "cuda")]
    {
        let gpu = watershed_gpu::Gpu::new()?;
        gpu.run(&plan, &inputs[..vary.len()])?;
        let started = Instant::now();
        let on_gpu = gpu.run(&plan, &inputs)?;
        let took = started.elapsed();
        println!(
            "the same on the GPU in {} ({} a second)",
            time(took),
            commas(trials as f64 / took.as_secs_f64())
        );
        // Powers and logarithms may differ in the last digits.
        assert!(
            on_gpu
                .iter()
                .zip(&results)
                .all(|(a, b)| close(*a, *b) || (!a.is_finite() && !b.is_finite())),
            "the CPU and the GPU differ"
        );
    }
    // The graph itself, a trial at a time, as a recalculating spreadsheet
    // would do it; it must agree to the last bit.
    let few = trials.min(20_000);
    let started = Instant::now();
    for (trial, result) in inputs
        .chunks(vary.len())
        .zip(results.chunks(outputs.len()))
        .take(few)
    {
        for (v, &x) in vary.iter().zip(trial) {
            model.g.set(model.inputs[&v.cell], x)?;
        }
        model.g.stabilize();
        for (&id, &x) in output_ids.iter().zip(result) {
            let Value::F64(here) = model.g.value(id) else {
                unreachable!("an output is a number")
            };
            assert!(
                here.to_bits() == x.to_bits() || (here.is_nan() && x.is_nan()),
                "the graph and the plan differ"
            );
        }
    }
    let each = started.elapsed() / few as u32;
    println!(
        "the graph, a trial at a time: {} each, so {} for all of them",
        time(each),
        time(each * trials as u32)
    );
    println!();

    // What the trials say, for each output.
    for (o, &c) in outputs.iter().enumerate() {
        let mut all: Vec<f64> = results
            .chunks(outputs.len())
            .map(|r| r[o])
            .filter(|x| x.is_finite())
            .collect();
        all.sort_by(f64::total_cmp);
        let now = number(&book.value(c)).unwrap_or(f64::NAN);
        let n = all.len();
        let at = |share: f64| all[((n as f64 * share) as usize).min(n - 1)];
        println!("{}", book.name(c));
        println!("  in the workbook now      {:>14}", figure(now));
        println!(
            "  average over the trials  {:>14}",
            figure(all.iter().sum::<f64>() / n as f64)
        );
        println!(
            "  1 in 20 is below {}, half are below {}, 1 in 20 is above {}",
            figure(at(0.05)),
            figure(at(0.5)),
            figure(at(0.95))
        );
        if n < trials {
            println!(
                "  ({} trials gave an error, as Excel would have)",
                commas((trials - n) as f64)
            );
        }
        histogram(&all);
        println!();
    }

    // Which input matters most: each to its low and its high, the rest as
    // the workbook has them.
    let mut swings = Vec::new();
    let base: Vec<f64> = vary.iter().map(|v| v.now).collect();
    let mut tries = Vec::new();
    for (k, v) in vary.iter().enumerate() {
        for x in [v.low, v.high] {
            let mut one = base.clone();
            one[k] = x;
            tries.extend(one);
        }
    }
    let ends = plan.run_cpu(&tries);
    for (k, v) in vary.iter().enumerate() {
        let (low, high) = (
            ends[2 * k * outputs.len()],
            ends[(2 * k + 1) * outputs.len()],
        );
        swings.push((book.name(v.cell), v.low, v.high, low, high));
    }
    swings.sort_by(|a, b| (b.4 - b.3).abs().total_cmp(&(a.4 - a.3).abs()));
    println!(
        "what moves {} most (each input to its low and its high, the rest as they are)",
        book.name(outputs[0])
    );
    for (name, low, high, at_low, at_high) in &swings {
        println!(
            "  {name:<24} {:>8} to {:<8} gives {:>12} to {:<12}",
            figure(*low),
            figure(*high),
            figure(*at_low),
            figure(*at_high)
        );
    }
    println!();

    // One cell edited, as someone working in the model would.
    for (v, &x) in vary.iter().zip(&base) {
        model.g.set(model.inputs[&v.cell], x)?;
    }
    model.g.stabilize();
    let started = Instant::now();
    model.g.set(model.inputs[&vary[0].cell], vary[0].high)?;
    let report = model.g.stabilize();
    println!(
        "one input edited on the graph: {} of {} nodes worked out again, in {}",
        report.recalculated,
        model.nodes.len(),
        time(started.elapsed())
    );
    Ok(())
}

/// How the trials are spread, leaving out the rarest half a percent at each
/// end so the middle can be seen.
fn histogram(sorted: &[f64]) {
    const BARS: usize = 17;
    const WIDTH: usize = 48;
    let n = sorted.len();
    let (low, high) = (sorted[n / 200], sorted[n - 1 - n / 200]);
    if high <= low {
        return;
    }
    let mut counts = [0usize; BARS];
    for &x in sorted {
        if (low..=high).contains(&x) {
            counts[(((x - low) / (high - low) * BARS as f64) as usize).min(BARS - 1)] += 1;
        }
    }
    let most = counts.iter().copied().max().unwrap_or(1).max(1);
    for (bar, &count) in counts.iter().enumerate() {
        let from = low + (high - low) * bar as f64 / BARS as f64;
        println!(
            "  {:>14} {}",
            figure(from),
            "#".repeat((count * WIDTH).div_ceil(most))
        );
    }
}

/// A number to four or five figures, with thousands separated when large.
fn figure(x: f64) -> String {
    if !x.is_finite() {
        return format!("{x}");
    }
    if x.abs() >= 1000.0 {
        return commas(x);
    }
    let places = if x.abs() >= 100.0 {
        1
    } else if x.abs() >= 1.0 {
        2
    } else {
        4
    };
    format!("{x:.places$}")
}

fn commas(x: f64) -> String {
    let digits = format!("{:.0}", x.abs());
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if x < 0.0 && digits != "0" {
        out.insert(0, '-');
    }
    out
}

fn time(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 0.001 {
        format!("{:.1} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else if s < 120.0 {
        format!("{s:.1} s")
    } else {
        format!("{:.0} minutes", s / 60.0)
    }
}
