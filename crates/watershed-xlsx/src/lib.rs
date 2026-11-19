//! Turns the formulas of an Excel workbook into a watershed graph.
//!
//! Someone has built a model in Excel: a valuation, a budget, a project's
//! finances. Some of its inputs are guesses. Given the cells to vary and the
//! cells to read, [`Model::build`] translates the formulas between them into
//! a [`Graph`], with an input for each cell to vary. The graph can then be
//! changed and worked out again like any other, or handed to
//! `watershed-gpu` to run for a million sets of inputs at once.
//!
//! ```no_run
//! use watershed_xlsx::{Model, Workbook};
//!
//! let book = Workbook::open("model.xlsx")?;
//! let growth = book.cell("'Input sheet'!B26")?;
//! let value = book.cell("'Valuation output'!B33")?;
//! let mut model = Model::build(&book, &[growth], &[value])?;
//! // Every translated cell, against what Excel saved for it.
//! assert!(model.check().is_empty());
//! model.set(&[0.05])?;
//! println!("{}", model.value(value).unwrap());
//! # Ok::<(), watershed_xlsx::Error>(())
//! ```
//!
//! # What is translated
//!
//! Only what lies between the inputs and the outputs. A cell that does not
//! depend on a varied input is taken as the value Excel saved for it,
//! whatever its formula; and where a condition does not depend on one
//! (`IF(B17="Yes", ..)`), only the branch it takes is followed. So a workbook
//! can be full of things this does not understand, as long as they are not
//! in the way. Anything in the way that it does not understand is refused,
//! with the cell's name and the reason: nothing is guessed.
//!
//! In the way, it understands numbers, true and false, `+ - * / ^ %`,
//! comparisons, defined names, ranges (`B2:B9`, `B:B`, on any sheet), and
//! the functions in [`FUNCTIONS`]. A further few, [`FIXED_FUNCTIONS`], are
//! understood when what they are given does not depend on a varied input.
//!
//! Cells that read each other in a loop (a circular reference, which Excel
//! works out by iterating) are fine as long as the formulas followed do not
//! go round it; a loop that is followed is refused.
//!
//! # Where it differs from Excel
//!
//! - An Excel error that comes from a varied input (dividing by zero, say)
//!   is infinity or not-a-number here. [`IFERROR`](FUNCTIONS) treats both
//!   as errors.
//! - Text cannot come from a varied input: `IF(x > 0, x, "")` is refused, as
//!   is joining text to a number with `&`.
//! - `0^0` is 1 here when it comes from a varied input, where Excel has
//!   `#NUM!`.
//! - An `INDEX` or `CHOOSE` position that depends on an input and comes to
//!   less than 1 is not-a-number here; Excel takes 0 in `INDEX` to mean the
//!   whole row or column.
//! - Approximate lookups (`VLOOKUP` with `TRUE`, `MATCH` with 1 or -1) need
//!   their keys in order, as Excel asks; a table whose keys are not is
//!   refused, since Excel's answer then depends on how it searches.
//!
//! # How it is checked
//!
//! Excel saves the last value it worked out for every cell, so a workbook is
//! its own check: [`Model::check`] compares each translated cell with it.
//! This crate's tests use workbooks that LibreOffice worked out, in which
//! each row repeats the same formulas for a different input, so every
//! function is compared with a spreadsheet's own answer for dozens of
//! inputs, and both ways through each condition.

mod book;
mod functions;
mod model;
mod parse;

pub use book::{Cell, Val, Workbook};
pub use model::{Mismatch, Model};

#[doc(no_inline)]
pub use watershed_core::Graph;

/// The functions understood whether or not what they are given depends on a
/// varied input.
pub const FUNCTIONS: &[&str] = &[
    "ABS",
    "AND",
    "AVERAGE",
    "CHOOSE",
    "EXP",
    "HLOOKUP",
    "IF",
    "IFERROR",
    "INDEX",
    "INT",
    "LN",
    "LOG",
    "LOG10",
    "MATCH",
    "MAX",
    "MIN",
    "NOT",
    "NPV",
    "OR",
    "PMT",
    "POWER",
    "ROUND",
    "ROUNDDOWN",
    "ROUNDUP",
    "SIGN",
    "SQRT",
    "SUM",
    "SUMPRODUCT",
    "VLOOKUP",
];

/// The functions understood only when what they are given does not depend
/// on a varied input. `COUNT` and `COUNTA` count a varied cell as a number
/// whatever it comes to.
pub const FIXED_FUNCTIONS: &[&str] = &["COUNT", "COUNTA", "MOD"];

/// Why a workbook could not be read or translated.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The file is not a workbook this reads.
    Read(String),
    /// A cell given by name could not be found.
    Name { name: String, problem: String },
    /// A cell on the way from the inputs to the outputs, or one given as an
    /// input or an output, could not be translated.
    Cell { cell: String, problem: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Read(problem) => write!(f, "{problem}"),
            Error::Name { name, problem } => write!(f, "{name}: {problem}"),
            Error::Cell { cell, problem } => write!(f, "{cell}: {problem}"),
        }
    }
}

impl std::error::Error for Error {}
