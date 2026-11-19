//! A workbook as it was saved: each cell's formula, and the value Excel last
//! worked out for it.

use std::fs::File;
use std::io::{BufReader, Cursor, Read, Seek};
use std::path::Path;

use calamine::{Data, Range, Reader, Xlsx};

use crate::Error;
use crate::parse::{Ast, parse};

/// A cell: which sheet (counted from zero, in the workbook's order), and its
/// row and column, also from zero. `A1` is row 0, column 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Cell {
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
}

/// What a cell holds, as Excel saved it.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Num(f64),
    Text(String),
    Bool(bool),
    Empty,
    /// Any of Excel's errors: `#DIV/0!`, `#N/A`, `#VALUE!` and the rest.
    Error,
}

pub(crate) struct Sheet {
    pub(crate) name: String,
    values: Range<Data>,
    formulas: Range<String>,
}

/// A workbook read from an `.xlsx` or `.xlsm` file.
pub struct Workbook {
    pub(crate) sheets: Vec<Sheet>,
    /// The workbook's defined names, each with what it stands for
    /// (`Sheet1!$B$2`, say), the name in capitals.
    pub(crate) names: Vec<(String, String)>,
}

impl Workbook {
    /// Reads a workbook from a file. Only `.xlsx` and `.xlsm` files are
    /// read: an older `.xls` file is read without some of its formulas, and
    /// a cell whose formula is missing would be taken for a constant.
    pub fn open(path: impl AsRef<Path>) -> Result<Workbook, Error> {
        let path = path.as_ref();
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        if !matches!(extension.as_deref(), Some("xlsx" | "xlsm")) {
            return Err(Error::Read(format!(
                "{}: only .xlsx and .xlsm workbooks are read",
                path.display()
            )));
        }
        let file = File::open(path).map_err(|e| Error::Read(format!("{}: {e}", path.display())))?;
        Workbook::read(BufReader::new(file))
    }

    /// Reads a workbook from the bytes of an `.xlsx` or `.xlsm` file.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Workbook, Error> {
        Workbook::read(Cursor::new(bytes))
    }

    fn read<R: Read + Seek>(reader: R) -> Result<Workbook, Error> {
        let read = |e: calamine::XlsxError| Error::Read(e.to_string());
        let mut book: Xlsx<R> = Xlsx::new(reader).map_err(read)?;
        let names = book
            .defined_names()
            .iter()
            .map(|(name, means)| (name.to_uppercase(), means.clone()))
            .collect();
        let mut sheets = Vec::new();
        for name in book.sheet_names() {
            sheets.push(Sheet {
                values: book.worksheet_range(&name).map_err(read)?,
                formulas: book.worksheet_formula(&name).map_err(read)?,
                name,
            });
        }
        Ok(Workbook { sheets, names })
    }

    /// The sheets' names, in the workbook's order.
    pub fn sheets(&self) -> impl Iterator<Item = &str> {
        self.sheets.iter().map(|s| s.name.as_str())
    }

    /// How many cells hold a formula.
    pub fn formulas(&self) -> usize {
        self.sheets
            .iter()
            .map(|s| s.formulas.used_cells().filter(|c| !c.2.is_empty()).count())
            .sum()
    }

    pub(crate) fn sheet(&self, name: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// The last row anything is in on a sheet, for a whole column (`A:A`).
    pub(crate) fn last_row(&self, sheet: usize) -> u32 {
        let s = &self.sheets[sheet];
        let end = |r: Option<(u32, u32)>| r.map_or(0, |(row, _)| row);
        end(s.values.end()).max(end(s.formulas.end()))
    }

    /// A cell from its name as Excel writes it: `B2`, `Sheet1!B2`,
    /// `'Input sheet'!$B$26`, or a defined name that stands for one cell. A
    /// cell named without a sheet is on the first.
    pub fn cell(&self, name: &str) -> Result<Cell, Error> {
        let not = |problem: String| Error::Name {
            name: name.to_string(),
            problem,
        };
        match parse(self, 0, name).map_err(not)? {
            Ast::Cell(c) => Ok(c),
            _ => Err(not("is not one cell".into())),
        }
    }

    /// Every cell that holds a value or a formula, sheet by sheet, each
    /// sheet in order of row and then column.
    pub fn cells(&self) -> impl Iterator<Item = Cell> + '_ {
        self.sheets.iter().enumerate().flat_map(|(sheet, s)| {
            let (values, formulas) = (s.values.start(), s.formulas.start());
            let held = s
                .values
                .used_cells()
                .filter(|(_, _, v)| !matches!(v, Data::Empty))
                .filter_map(move |(row, col, _)| {
                    values.map(|(top, left)| (top + row as u32, left + col as u32))
                });
            let worked = s
                .formulas
                .used_cells()
                .filter(|(_, _, f)| !f.is_empty())
                .filter_map(move |(row, col, _)| {
                    formulas.map(|(top, left)| (top + row as u32, left + col as u32))
                });
            let all: std::collections::BTreeSet<(u32, u32)> = held.chain(worked).collect();
            all.into_iter()
                .map(move |(row, col)| Cell { sheet, row, col })
        })
    }

    /// The value Excel last worked out for a cell.
    pub fn value(&self, c: Cell) -> Val {
        let Some(sheet) = self.sheets.get(c.sheet) else {
            return Val::Empty;
        };
        match sheet.values.get_value((c.row, c.col)) {
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

    /// A cell's formula, without its `=`, if it has one.
    pub fn formula(&self, c: Cell) -> Option<&str> {
        self.sheets
            .get(c.sheet)?
            .formulas
            .get_value((c.row, c.col))
            .map(|f| f.trim_start_matches('='))
            .filter(|f| !f.is_empty())
    }

    /// A cell's name as Excel writes it, `'Input sheet'!B26`, which
    /// [`Workbook::cell`] reads back.
    pub fn name(&self, c: Cell) -> String {
        let mut col = String::new();
        let mut n = c.col + 1;
        while n > 0 {
            col.insert(0, (b'A' + ((n - 1) % 26) as u8) as char);
            n = (n - 1) / 26;
        }
        let sheet = &self.sheets[c.sheet].name;
        let plain = sheet
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && sheet.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !looks_like_a_cell(sheet);
        if plain {
            format!("{sheet}!{col}{}", c.row + 1)
        } else {
            format!("'{}'!{col}{}", sheet.replace('\'', "''"), c.row + 1)
        }
    }
}

/// Whether a name such as `AB12` could be read as a cell.
fn looks_like_a_cell(name: &str) -> bool {
    crate::parse::reference(name).is_some()
}
