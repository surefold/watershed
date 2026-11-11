//! The example the page opens with: a book of trades whose marks tick.

use wasm_bindgen::prelude::*;
use watershed_core::Value;

use crate::table::{Agg, Kind, Table};
use crate::{Grid, js_error};

const SYMBOLS: [&str; 32] = [
    "ALDR", "BRCH", "CEDR", "DUNE", "ELMS", "FERN", "GLEN", "HAZL", "IRIS", "JUNI", "KELP", "LARK",
    "MOSS", "NETL", "OAKS", "PINE", "QUIL", "REED", "SAGE", "TARN", "UMBR", "VALE", "WREN", "YARO",
    "ZEST", "ASHE", "BECK", "COVE", "DELL", "EDDY", "FORD", "GILL",
];
const DESKS: [&str; 6] = ["Rates", "Credit", "Equities", "FX", "Commodities", "Macro"];
const TRADERS: [&str; 18] = [
    "A. Okafor",
    "B. Lindqvist",
    "C. Marchetti",
    "D. Tanaka",
    "E. Novak",
    "F. Haddad",
    "G. Morgan",
    "H. Petrov",
    "I. Castillo",
    "J. Whitlock",
    "K. Banerjee",
    "L. Fontaine",
    "M. Reyes",
    "N. Sorensen",
    "O. Adeyemi",
    "P. Kowalski",
    "Q. Zhao",
    "R. Delacroix",
];

/// How many updates back a cell's trend goes.
const HISTORY: u64 = 120;

pub(crate) struct Demo {
    seed: u64,
    made: u32,
    mark: usize,
    /// One shared copy of each text, so a hundred thousand rows hold pointers to them.
    symbols: Vec<Value>,
    desks: Vec<Value>,
    traders: Vec<Value>,
    sides: [Value; 2],
}

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn below(seed: &mut u64, n: usize) -> usize {
    (next(seed) % n as u64) as usize
}

fn unit(seed: &mut u64) -> f64 {
    (next(seed) % 1_000_000) as f64 / 1_000_000.0
}

fn cents(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn texts(items: &[&str]) -> Vec<Value> {
    items.iter().map(|&s| Value::Str(s.into())).collect()
}

/// The trade book's columns, with no rows yet.
pub(crate) fn columns(t: &mut Table) -> Result<Demo, String> {
    t.keep_history(HISTORY);
    t.add_field("symbol", Kind::Text, Agg::Count)?;
    t.add_field("desk", Kind::Text, Agg::None)?;
    t.add_field("trader", Kind::Text, Agg::None)?;
    t.add_field("side", Kind::Text, Agg::None)?;
    t.add_field("qty", Kind::Number, Agg::Sum)?;
    t.add_field("price", Kind::Number, Agg::Avg)?;
    let mark = t.add_field("mark", Kind::Number, Agg::Avg)?;
    t.add_value("fee_bps", 2.0)?;
    t.add_formula("notional", "qty * mark", Agg::Sum)?;
    t.add_formula(
        "pnl",
        "if(side == \"Buy\", 1, -1) * (mark - price) * qty",
        Agg::Sum,
    )?;
    t.add_formula("net", "pnl - notional * fee_bps / 10000", Agg::Sum)?;
    Ok(Demo {
        seed: 0x2545_F491_4F6C_DD1D,
        made: 0,
        mark,
        symbols: texts(&SYMBOLS),
        desks: texts(&DESKS),
        traders: texts(&TRADERS),
        sides: [Value::Str("Buy".into()), Value::Str("Sell".into())],
    })
}

/// Adds `count` more trades.
pub(crate) fn fill(t: &mut Table, d: &mut Demo, count: u32) -> Result<(), String> {
    for _ in 0..count {
        d.made += 1;
        let s = &mut d.seed;
        let symbol = below(s, d.symbols.len());
        // Each symbol trades around its own level.
        let level = 20.0 + (symbol as f64 * 37.0) % 480.0;
        let price = cents(level * (0.95 + unit(s) * 0.1));
        let mark = cents(price * (0.99 + unit(s) * 0.02));
        let trader = below(s, d.traders.len());
        let row = vec![
            d.symbols[symbol].clone(),
            // A trader sits on one desk.
            d.desks[trader % d.desks.len()].clone(),
            d.traders[trader].clone(),
            d.sides[below(s, 2)].clone(),
            Value::F64(((1 + below(s, 100)) * 100) as f64),
            Value::F64(price),
            Value::F64(mark),
        ];
        t.upsert(&format!("T-{:06}", d.made), row)?;
    }
    Ok(())
}

/// Moves the mark of `changes` trades picked at random, each by up to 0.3%.
pub(crate) fn tick(t: &mut Table, d: &mut Demo, changes: u32) -> Result<(), String> {
    let rows = t.row_slots();
    if rows == 0 {
        return Ok(());
    }
    for _ in 0..changes {
        let row = below(&mut d.seed, rows);
        let Some(Value::F64(mark)) = t.value_at(row, d.mark).cloned() else {
            continue;
        };
        let moved = cents(mark * (0.997 + unit(&mut d.seed) * 0.006)).max(0.01);
        t.set_at(row, d.mark, Value::F64(moved))?;
    }
    Ok(())
}

#[wasm_bindgen]
impl Grid {
    /// The example trade book's columns, with no rows yet; add them with `demo_fill`.
    pub fn demo() -> Result<Grid, JsError> {
        let mut table = Table::new();
        let demo = columns(&mut table).map_err(js_error)?;
        Ok(Grid {
            table,
            demo: Some(demo),
        })
    }

    /// Adds `count` more example trades.
    pub fn demo_fill(&mut self, count: u32) -> Result<(), JsError> {
        let demo = self
            .demo
            .as_mut()
            .ok_or_else(|| js_error("this grid is not the example".into()))?;
        fill(&mut self.table, demo, count).map_err(js_error)
    }

    /// Moves the mark of `changes` example trades picked at random.
    pub fn demo_tick(&mut self, changes: u32) -> Result<(), JsError> {
        let demo = self
            .demo
            .as_mut()
            .ok_or_else(|| js_error("this grid is not the example".into()))?;
        tick(&mut self.table, demo, changes).map_err(js_error)
    }
}
