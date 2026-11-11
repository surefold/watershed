//! # watershed-core
//!
//! An incremental calculation engine. You build a graph of inputs and
//! calculated nodes; when inputs change, only the nodes that depend on them
//! are recalculated, and an update stops spreading wherever a recalculated
//! value comes out the same as before.
//!
//! ```
//! use watershed_core::prelude::*;
//!
//! # fn main() -> Result<(), CalcError> {
//! let mut g = Graph::new();
//!
//! let qty = g.input("qty", 10.0)?;
//! let price = g.input("price", 2.5)?;
//! let tax = g.input("tax_rate", 0.2)?;
//!
//! let subtotal = g.define("subtotal", qty * price)?;
//! let total = g.define("total", subtotal * (1.0 + tax))?;
//! assert_eq!(g.get(total)?, 30.0);
//!
//! g.set(price, 3.0)?;
//! let report = g.stabilize();
//!
//! assert_eq!(g.get(total)?, 36.0);
//! assert_eq!(report.recalculated, 2); // subtotal and total; nothing else
//!
//! println!("{}", g.explain(total));
//! # Ok(())
//! # }
//! ```
//!
//! Types are checked by the Rust compiler. Adding a number to a string does
//! not compile:
//!
//! ```compile_fail
//! use watershed_core::prelude::*;
//!
//! let mut g = Graph::new();
//! let qty = g.input("qty", 10.0).unwrap();
//! let region = g.input("region", "EU".to_string()).unwrap();
//! let wrong = g.define("wrong", qty + region); // error: f64 + String
//! ```
//!
//! and neither does setting a calculated node, which only inputs allow:
//!
//! ```compile_fail
//! use watershed_core::prelude::*;
//!
//! let mut g = Graph::new();
//! let qty = g.input("qty", 10.0).unwrap();
//! let double = g.define("double", qty * 2.0).unwrap();
//! g.set(double, 5.0); // error: `double` is a Node, not an Input
//! ```

#![forbid(unsafe_code)]

pub mod aggregate;
pub mod collection;
mod compile;
pub mod error;
pub mod expr;
pub mod graph;
pub mod handle;
mod history;
pub mod ir;
pub mod sink;
pub mod value;

pub use aggregate::AggregateOp;
pub use collection::{Collection, Column, Field, Groups, Part, Set};
pub use error::CalcError;
pub use expr::{Expr, IntoExpr, choose, list, text, when};
pub use graph::{Graph, Report};
pub use handle::{Handle, Input, Node, NodeId};
pub use ir::{BinOp, Ir, MAX_LIST_ITEMS, MAX_TEXT_BYTES, TypeError, UnOp};
pub use sink::ChangeSink;
pub use value::{CalcType, Numeric, Ordered, Text, TextList, Type, Value};

/// Everything you need to build and use a graph: `use watershed_core::prelude::*;`
pub mod prelude {
    pub use crate::{
        CalcError, Collection, Column, Expr, Field, Graph, Input, IntoExpr, Node, Report, choose,
        list, text, when,
    };
}
