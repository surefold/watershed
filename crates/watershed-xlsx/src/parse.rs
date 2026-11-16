//! Excel's formulas, read into a tree.

use crate::book::{Cell, Workbook};

/// Excel's last row and column, from one.
const ROWS: u32 = 1_048_576;
const COLUMNS: u32 = 16_384;

/// A defined name may stand for another; this many deep is taken to be a
/// loop.
const NAMES_DEEP: usize = 16;

#[derive(Debug, Clone)]
pub(crate) enum Ast {
    Num(f64),
    Text(String),
    Bool(bool),
    /// One of Excel's errors written into a formula, such as `#N/A`.
    Error,
    /// An argument left out, as in `IF(a,,b)`.
    Missing,
    Cell(Cell),
    /// From the first corner to the second, the first above and to the left.
    Range(Cell, Cell),
    Negate(Box<Ast>),
    Percent(Box<Ast>),
    Binary(&'static str, Box<Ast>, Box<Ast>),
    /// A function, its name in capitals.
    Call(String, Vec<Ast>),
}

type Parsed<T> = Result<T, String>;

struct Parser<'a> {
    text: &'a [u8],
    at: usize,
    /// The sheet the formula is on, for a cell named without one.
    sheet: usize,
    book: &'a Workbook,
    /// How many defined names deep this is.
    deep: usize,
}

/// A formula, without its `=`, on the given sheet.
pub(crate) fn parse(book: &Workbook, sheet: usize, formula: &str) -> Parsed<Ast> {
    parse_at(book, sheet, formula, 0)
}

fn parse_at(book: &Workbook, sheet: usize, formula: &str, deep: usize) -> Parsed<Ast> {
    let mut parser = Parser {
        text: formula.trim_start_matches('=').as_bytes(),
        at: 0,
        sheet,
        book,
        deep,
    };
    let ast = parser.comparison()?;
    if parser.peek().is_some() {
        return parser.fail("unexpected text");
    }
    Ok(ast)
}

/// A cell's address alone, `B7` or `$B$7`: its row and column from zero.
pub(crate) fn reference(text: &str) -> Option<(u32, u32)> {
    let text = text.as_bytes();
    let mut at = usize::from(text.first() == Some(&b'$'));
    let mut col = 0u32;
    let letters = at;
    while let Some(b) = text.get(at).filter(|b| b.is_ascii_alphabetic()) {
        col = col * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1;
        at += 1;
        if at - letters > 3 {
            return None;
        }
    }
    if at == letters {
        return None;
    }
    if text.get(at) == Some(&b'$') {
        at += 1;
    }
    let digits = &text[at..];
    if digits.is_empty() || digits.len() > 7 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let row: u32 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    ((1..=ROWS).contains(&row) && col <= COLUMNS).then(|| (row - 1, col - 1))
}

/// A column's letters alone, `C` or `$C`, from zero.
fn column(text: &str) -> Option<u32> {
    let letters = text.strip_prefix('$').unwrap_or(text);
    if letters.is_empty() || letters.len() > 3 || !letters.bytes().all(|b| b.is_ascii_alphabetic())
    {
        return None;
    }
    let col = letters.bytes().fold(0u32, |n, b| {
        n * 26 + u32::from(b.to_ascii_uppercase() - b'A') + 1
    });
    (col <= COLUMNS).then(|| col - 1)
}

const ERRORS: [&str; 8] = [
    "#DIV/0!",
    "#N/A",
    "#NAME?",
    "#NULL!",
    "#NUM!",
    "#REF!",
    "#VALUE!",
    "#GETTING_DATA",
];

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

    fn fail<T>(&self, what: &str) -> Parsed<T> {
        Err(format!(
            "{what}, at character {} of `{}`",
            self.at + 1,
            String::from_utf8_lossy(self.text)
        ))
    }

    fn comparison(&mut self) -> Parsed<Ast> {
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

    fn joined(&mut self) -> Parsed<Ast> {
        let mut left = self.sum()?;
        while self.take("&") {
            left = Ast::Binary("&", Box::new(left), Box::new(self.sum()?));
        }
        Ok(left)
    }

    fn sum(&mut self) -> Parsed<Ast> {
        let mut left = self.product()?;
        loop {
            let Some(op) = ["+", "-"].into_iter().find(|op| self.take(op)) else {
                return Ok(left);
            };
            left = Ast::Binary(op, Box::new(left), Box::new(self.product()?));
        }
    }

    fn product(&mut self) -> Parsed<Ast> {
        let mut left = self.power()?;
        loop {
            let Some(op) = ["*", "/"].into_iter().find(|op| self.take(op)) else {
                return Ok(left);
            };
            left = Ast::Binary(op, Box::new(left), Box::new(self.power()?));
        }
    }

    /// `^` is worked out left to right in Excel: `2^3^2` is 64.
    fn power(&mut self) -> Parsed<Ast> {
        let mut left = self.signed()?;
        while self.take("^") {
            left = Ast::Binary("^", Box::new(left), Box::new(self.signed()?));
        }
        Ok(left)
    }

    /// In Excel a minus sign binds tighter than `^`: `-2^2` is 4.
    fn signed(&mut self) -> Parsed<Ast> {
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

    fn primary(&mut self) -> Parsed<Ast> {
        let Some(c) = self.peek() else {
            return self.fail("the formula ends early");
        };
        match c {
            b'(' => {
                self.at += 1;
                let inner = self.comparison()?;
                if !self.take(")") {
                    return self.fail("expected )");
                }
                Ok(inner)
            }
            b'"' => self.text(),
            b'0'..=b'9' | b'.' => self.number(),
            b'#' => {
                let rest = &self.text[self.at..];
                match ERRORS.iter().find(|e| {
                    rest.len() >= e.len() && rest[..e.len()].eq_ignore_ascii_case(e.as_bytes())
                }) {
                    Some(e) => {
                        self.at += e.len();
                        Ok(Ast::Error)
                    }
                    None => self.fail("not one of Excel's errors"),
                }
            }
            b'{' => self.fail("lists of constants in braces are not understood"),
            b'\'' => {
                let sheet = self.quoted_sheet()?;
                self.reference(sheet)
            }
            _ => self.name(),
        }
    }

    fn text(&mut self) -> Parsed<Ast> {
        let mut text = Vec::new();
        self.at += 1;
        loop {
            match self.text.get(self.at) {
                None => return self.fail("a piece of text does not end"),
                Some(b'"') if self.text.get(self.at + 1) == Some(&b'"') => {
                    text.push(b'"');
                    self.at += 2;
                }
                Some(b'"') => {
                    self.at += 1;
                    return Ok(Ast::Text(String::from_utf8_lossy(&text).into_owned()));
                }
                Some(&b) => {
                    text.push(b);
                    self.at += 1;
                }
            }
        }
    }

    fn number(&mut self) -> Parsed<Ast> {
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
        match text.parse() {
            Ok(x) => Ok(Ast::Num(x)),
            Err(_) => self.fail("not a number"),
        }
    }

    /// A sheet's name in quotes, then `!`. A quote in the name is written
    /// twice.
    fn quoted_sheet(&mut self) -> Parsed<usize> {
        let mut name = Vec::new();
        self.at += 1;
        loop {
            match self.text.get(self.at) {
                None => return self.fail("a sheet's name does not end"),
                Some(b'\'') if self.text.get(self.at + 1) == Some(&b'\'') => {
                    name.push(b'\'');
                    self.at += 2;
                }
                Some(b'\'') => {
                    self.at += 1;
                    break;
                }
                Some(&b) => {
                    name.push(b);
                    self.at += 1;
                }
            }
        }
        if self.text.get(self.at) != Some(&b'!') {
            return self.fail("expected ! after a sheet's name");
        }
        self.at += 1;
        let name = String::from_utf8_lossy(&name);
        match self.book.sheet(&name) {
            Some(sheet) => Ok(sheet),
            None => self.fail(&format!("there is no sheet called {name}")),
        }
    }

    /// The characters a name, a function's or a cell's, is made of.
    fn word(&mut self) -> String {
        let start = self.at;
        while self.text.get(self.at).is_some_and(|&b| {
            b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'$' | b'\\' | b'?') || b >= 0x80
        }) {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.text[start..self.at]).into_owned()
    }

    fn name(&mut self) -> Parsed<Ast> {
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
            return self.reference(sheet);
        }
        let start = self.at;
        let word = self.word();
        if word.is_empty() {
            return self.fail("unexpected character");
        }
        match self.text.get(self.at) {
            Some(b'(') => {
                self.at += 1;
                let name = word.to_uppercase();
                let name = name
                    .strip_prefix("_XLFN.")
                    .or_else(|| name.strip_prefix("_XLWS."))
                    .unwrap_or(&name)
                    .to_string();
                return Ok(Ast::Call(name, self.arguments()?));
            }
            Some(b'!') => {
                self.at += 1;
                return match self.book.sheet(&word) {
                    Some(sheet) => self.reference(sheet),
                    None => self.fail(&format!("there is no sheet called {word}")),
                };
            }
            Some(b'[') => {
                return self
                    .fail("references into a table by its columns' names are not understood");
            }
            _ => {}
        }
        match word.to_uppercase().as_str() {
            "TRUE" => return Ok(Ast::Bool(true)),
            "FALSE" => return Ok(Ast::Bool(false)),
            _ => {}
        }
        if reference(&word).is_some()
            || (column(&word).is_some() && self.text.get(self.at) == Some(&b':'))
        {
            self.at = start;
            return self.reference(self.sheet);
        }
        self.defined(&word)
    }

    fn arguments(&mut self) -> Parsed<Vec<Ast>> {
        let mut args = Vec::new();
        if self.take(")") {
            return Ok(args);
        }
        loop {
            args.push(match self.peek() {
                Some(b',' | b')') => Ast::Missing,
                _ => self.comparison()?,
            });
            if self.take(")") {
                return Ok(args);
            }
            if !self.take(",") {
                return self.fail("expected , or )");
            }
        }
    }

    /// What a defined name stands for.
    fn defined(&mut self, word: &str) -> Parsed<Ast> {
        let wanted = word.to_uppercase();
        let Some((_, means)) = self.book.names.iter().find(|(name, _)| *name == wanted) else {
            return self.fail(&format!("{word} is neither a cell nor a defined name"));
        };
        if self.deep >= NAMES_DEEP {
            return self.fail(&format!("the defined name {word} goes round in a loop"));
        }
        parse_at(self.book, self.sheet, means, self.deep + 1)
            .map_err(|e| format!("in the defined name {word}: {e}"))
    }

    /// A cell or a range on the given sheet: `B2`, `B2:C9`, or whole
    /// columns, `B:C`.
    fn reference(&mut self, sheet: usize) -> Parsed<Ast> {
        let first = self.word();
        if self.text.get(self.at) == Some(&b':') {
            self.at += 1;
            let last = self.word();
            if let (Some(a), Some(b)) = (reference(&first), reference(&last)) {
                return Ok(range(sheet, a, b));
            }
            if let (Some(a), Some(b)) = (column(&first), column(&last)) {
                let rows = self.book.last_row(sheet);
                return Ok(range(sheet, (0, a), (rows, b)));
            }
            return self.fail("expected a range");
        }
        match reference(&first) {
            Some((row, col)) => Ok(Ast::Cell(Cell { sheet, row, col })),
            None => self.fail("expected a cell"),
        }
    }
}

fn range(sheet: usize, a: (u32, u32), b: (u32, u32)) -> Ast {
    Ast::Range(
        Cell {
            sheet,
            row: a.0.min(b.0),
            col: a.1.min(b.1),
        },
        Cell {
            sheet,
            row: a.0.max(b.0),
            col: a.1.max(b.1),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cells_address() {
        assert_eq!(reference("A1"), Some((0, 0)));
        assert_eq!(reference("$B$7"), Some((6, 1)));
        assert_eq!(reference("b$7"), Some((6, 1)));
        assert_eq!(reference("XFD1048576"), Some((1_048_575, 16_383)));
        assert_eq!(reference("XFE1"), None);
        assert_eq!(reference("A1048577"), None);
        assert_eq!(reference("A0"), None);
        assert_eq!(reference("RATE"), None);
        assert_eq!(reference("ABCD1"), None);
        assert_eq!(reference("A1B"), None);
        assert_eq!(column("$C"), Some(2));
        assert_eq!(column("AA"), Some(26));
    }
}
