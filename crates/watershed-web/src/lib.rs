//! The watershed engine compiled to WebAssembly, for the interactive demo in
//! `web/`. This is the real engine: the same `watershed-core` crate the tests
//! and benchmarks use, wrapped in a small API that JavaScript can call.
//!
//! Results cross to JavaScript as JSON text, written by hand to keep the
//! WebAssembly file small.

use std::fmt::Write as _;

use wasm_bindgen::prelude::*;
use watershed_core::prelude::*;
use watershed_core::{Groups, NodeId, Value};

/// The example models. Each is a file of ordinary watershed code; the page
/// shows the same file as the source of the graph it draws.
const PRESETS: [(&str, &str); 4] = [
    ("crypto", include_str!("presets/crypto.rs")),
    ("order", include_str!("presets/order.rs")),
    ("thermostat", include_str!("presets/thermostat.rs")),
    ("portfolio", include_str!("presets/portfolio.rs")),
];

mod presets {
    #[path = "crypto.rs"]
    pub mod crypto;
    #[path = "order.rs"]
    pub mod order;
    #[path = "portfolio.rs"]
    pub mod portfolio;
    #[path = "thermostat.rs"]
    pub mod thermostat;
}

fn build(name: &str, g: &mut Graph) -> Option<Result<(), CalcError>> {
    Some(match name {
        "crypto" => presets::crypto::build(g),
        "order" => presets::order::build(g),
        "thermostat" => presets::thermostat::build(g),
        "portfolio" => presets::portfolio::build(g),
        _ => return None,
    })
}

fn js_error(e: CalcError) -> JsError {
    JsError::new(&e.to_string())
}

/// A small model the page draws and lets you change.
#[wasm_bindgen]
pub struct Model {
    graph: Graph,
    ids: Vec<NodeId>,
}

#[wasm_bindgen]
impl Model {
    /// Builds one of the example models: "crypto", "order", "thermostat" or "portfolio".
    #[wasm_bindgen(constructor)]
    pub fn new(preset: &str) -> Result<Model, JsError> {
        let mut graph = Graph::new();
        build(preset, &mut graph)
            .ok_or_else(|| JsError::new(&format!("there is no example called {preset:?}")))?
            .map_err(js_error)?;
        let ids = graph.node_ids().collect();
        Ok(Model { graph, ids })
    }

    /// The source code of an example model.
    pub fn source(preset: &str) -> Option<String> {
        PRESETS
            .iter()
            .find(|(name, _)| *name == preset)
            .map(|(_, src)| src.to_string())
    }

    /// Every node: `[{id, name, type, input, height, deps, formula}]`.
    pub fn nodes(&self) -> String {
        let g = &self.graph;
        let mut out = String::from("[");
        for (k, &id) in self.ids.iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            let deps: Vec<String> = g
                .dependencies(id)
                .iter()
                .map(|d| d.index().to_string())
                .collect();
            let formula = g
                .expression(id)
                .map(|ir| ir.render(&|n| g.name(n).to_string()))
                .unwrap_or_default();
            let _ = write!(
                out,
                r#"{{"id":{},"name":{},"type":{},"input":{},"height":{},"deps":[{}],"formula":{}}}"#,
                id.index(),
                json_str(g.name(id)),
                json_str(&g.node_type(id).to_string()),
                g.is_input(id),
                g.height(id),
                deps.join(","),
                json_str(&formula),
            );
        }
        out.push(']');
        out
    }

    /// Every node's current value: `[{kind, value, text}]`, in node order.
    pub fn values(&self) -> String {
        let mut out = String::from("[");
        for (k, &id) in self.ids.iter().enumerate() {
            if k > 0 {
                out.push(',');
            }
            out.push_str(&value_json(self.graph.value(id)));
        }
        out.push(']');
        out
    }

    /// Records a new number for an input. Takes effect at the next `update`.
    pub fn set_number(&mut self, id: u32, value: f64) -> Result<(), JsError> {
        let id = self.id(id)?;
        self.graph
            .set_value(id, Value::F64(value))
            .map_err(js_error)
    }

    /// Records a new true/false for an input. Takes effect at the next `update`.
    pub fn set_bool(&mut self, id: u32, value: bool) -> Result<(), JsError> {
        let id = self.id(id)?;
        self.graph
            .set_value(id, Value::Bool(value))
            .map_err(js_error)
    }

    /// Applies the recorded changes. Returns what happened:
    /// `{epoch, inputsChanged, recalculated:[ids in order], changed:[ids]}`.
    pub fn update(&mut self) -> String {
        let before: Vec<Value> = self
            .ids
            .iter()
            .map(|&id| self.graph.value(id).clone())
            .collect();
        let report = self.graph.stabilize();
        let recalculated: Vec<String> = self
            .graph
            .last_recalculated()
            .iter()
            .map(|id| id.index().to_string())
            .collect();
        let changed: Vec<String> = self
            .ids
            .iter()
            .filter(|id| !self.graph.value(**id).same_as(&before[id.index()]))
            .map(|id| id.index().to_string())
            .collect();
        format!(
            r#"{{"epoch":{},"inputsChanged":{},"recalculated":[{}],"changed":[{}]}}"#,
            report.epoch,
            report.inputs_changed,
            recalculated.join(","),
            changed.join(",")
        )
    }

    /// Why a node has its value, as a tree.
    pub fn explain(&self, id: u32) -> Result<String, JsError> {
        Ok(self.graph.explain_node(self.id(id)?))
    }

    /// The whole graph, level by level.
    pub fn describe(&self) -> String {
        self.graph.describe()
    }

    /// Checks the incremental values against a full recalculation.
    pub fn check(&self) -> Option<String> {
        self.graph.check_consistency().err()
    }
}

impl Model {
    fn id(&self, id: u32) -> Result<NodeId, JsError> {
        self.ids
            .get(id as usize)
            .copied()
            .ok_or_else(|| JsError::new("there is no such node"))
    }
}

/// The million-node benchmark graph: market prices → trades → a tree of totals.
#[wasm_bindgen]
pub struct Benchmark {
    graph: Graph,
    prices: Vec<Input<f64>>,
    seed: u64,
}

#[wasm_bindgen]
impl Benchmark {
    /// Builds a graph with `trades` trades priced from `prices` market prices.
    #[wasm_bindgen(constructor)]
    pub fn new(prices: u32, trades: u32) -> Result<Benchmark, JsError> {
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut g = Graph::new();
        let prices: Vec<Input<f64>> = (0..prices)
            .map(|i| {
                let start = 100.0 + unit(&mut seed);
                g.input(&format!("price{i}"), start)
            })
            .collect::<Result<_, _>>()
            .map_err(js_error)?;
        let mut level: Vec<Node<f64>> = Vec::with_capacity(trades as usize);
        for t in 0..trades {
            let a = prices[below(&mut seed, prices.len())];
            let b = prices[below(&mut seed, prices.len())];
            let (qa, qb) = (unit(&mut seed) * 10.0, unit(&mut seed) * -5.0);
            level.push(
                g.define(&format!("trade{t}"), a * qa + b * qb)
                    .map_err(js_error)?,
            );
        }
        let mut depth = 0;
        while level.len() > 1 {
            depth += 1;
            let mut next = Vec::with_capacity(level.len() / 2 + 1);
            for (k, pair) in level.chunks(2).enumerate() {
                next.push(match pair {
                    [x, y] => g
                        .define(&format!("sum{depth}_{k}"), *x + *y)
                        .map_err(js_error)?,
                    [x] => *x,
                    _ => unreachable!(),
                });
            }
            level = next;
        }
        Ok(Benchmark {
            graph: g,
            prices,
            seed,
        })
    }

    pub fn nodes(&self) -> u32 {
        self.graph.len() as u32
    }

    /// Runs `updates` updates, each changing `per_update` random prices.
    /// Returns the total number of nodes recalculated.
    pub fn change_prices(&mut self, per_update: u32, updates: u32) -> f64 {
        let mut total = 0usize;
        for _ in 0..updates {
            for _ in 0..per_update {
                let p = self.prices[below(&mut self.seed, self.prices.len())];
                let v = 100.0 + unit(&mut self.seed);
                let _ = self.graph.set(p, v);
            }
            total += self.graph.stabilize().recalculated;
        }
        total as f64
    }

    /// Recalculates every node, as you would without incremental updates.
    /// Returns how many were recalculated.
    pub fn recalculate_everything(&mut self) -> f64 {
        self.graph.recalculate_everything() as f64
    }

    /// Checks the incremental values against a full recalculation.
    pub fn check(&self) -> Option<String> {
        self.graph.check_consistency().err()
    }
}

/// A rolling window of recent Wikipedia edits, kept as a collection: each
/// edit is a row, totalled overall and per wiki. A new edit adds a row and
/// pushes out the oldest, so only the totals those two rows touch change.
#[wasm_bindgen]
pub struct Edits {
    graph: Graph,
    edits: Collection,
    wiki: Field<String>,
    bytes: Field<f64>,
    bot: Field<bool>,
    rows: Node<i64>,
    bots: Node<f64>,
    bot_share: Node<f64>,
    added: Node<f64>,
    removed: Node<f64>,
    per_wiki: Groups<String, i64>,
    bytes_per_wiki: Groups<String, f64>,
    order: std::collections::VecDeque<String>,
    window: usize,
}

#[wasm_bindgen]
impl Edits {
    /// Keeps the latest `window` edits.
    #[wasm_bindgen(constructor)]
    pub fn new(window: u32) -> Result<Edits, JsError> {
        let mut g = Graph::new();
        let mut edits = Collection::new(&g, "edits");
        let wiki = edits.field::<String>("wiki").map_err(js_error)?;
        let bytes = edits.field::<f64>("bytes").map_err(js_error)?;
        let bot = edits.field::<bool>("bot").map_err(js_error)?;
        let is_bot = edits
            .column(&mut g, "is_bot", when(bot).then(1.0).otherwise(0.0))
            .map_err(js_error)?;
        let grew = edits
            .column(&mut g, "grew", bytes.max(0.0))
            .map_err(js_error)?;
        let shrank = edits
            .column(&mut g, "shrank", bytes.min(0.0))
            .map_err(js_error)?;
        let rows = edits.count(&mut g, "edits").map_err(js_error)?;
        let bots = edits.sum(&mut g, "bot_edits", is_bot).map_err(js_error)?;
        let added = edits.sum(&mut g, "bytes_added", grew).map_err(js_error)?;
        let removed = edits
            .sum(&mut g, "bytes_removed", shrank)
            .map_err(js_error)?;
        // Counts are whole numbers; the share needs a float, so each row
        // also counts 1.0 into a float total.
        let one = edits.column::<f64>(&mut g, "one", 1.0).map_err(js_error)?;
        let rows_f = edits.sum(&mut g, "edits_f", one).map_err(js_error)?;
        let bot_share = g
            .define(
                "bot_share",
                when(rows_f.gt(0.0)).then(bots / rows_f).otherwise(0.0),
            )
            .map_err(js_error)?;
        let per_wiki = edits
            .group_count(&mut g, "edits_by_wiki", wiki)
            .map_err(js_error)?;
        let bytes_per_wiki = edits
            .group_sum(&mut g, "bytes_by_wiki", wiki, bytes)
            .map_err(js_error)?;
        Ok(Edits {
            graph: g,
            edits,
            wiki,
            bytes,
            bot,
            rows,
            bots,
            bot_share,
            added,
            removed,
            per_wiki,
            bytes_per_wiki,
            order: std::collections::VecDeque::new(),
            window: window.max(1) as usize,
        })
    }

    /// Adds an edit; once the window is full, the oldest one is removed.
    /// Takes effect at the next `update`.
    pub fn add(&mut self, id: &str, wiki: &str, bytes: f64, bot: bool) -> Result<(), JsError> {
        if self.edits.contains(id) {
            return Ok(());
        }
        self.edits
            .upsert(
                &mut self.graph,
                id,
                &[
                    self.wiki.is(wiki.to_string()),
                    self.bytes.is(bytes),
                    self.bot.is(bot),
                ],
            )
            .map_err(js_error)?;
        self.order.push_back(id.to_string());
        while self.order.len() > self.window {
            let oldest = self.order.pop_front().expect("not empty");
            self.edits
                .remove(&mut self.graph, &oldest)
                .map_err(js_error)?;
        }
        Ok(())
    }

    /// Applies the recorded edits: `{epoch, recalculated, nodes}`.
    pub fn update(&mut self) -> String {
        let r = self.graph.stabilize();
        format!(
            r#"{{"epoch":{},"recalculated":{},"nodes":{}}}"#,
            r.epoch,
            r.recalculated,
            self.graph.len()
        )
    }

    /// The totals, and the `top` wikis by edits in the window:
    /// `{rows, bots, botShare, added, removed, wikis, top: [[wiki, edits, bytes]]}`.
    pub fn summary(&self, top: u32) -> String {
        let g = &self.graph;
        let mut wikis: Vec<(String, i64, f64)> = self
            .edits
            .group_keys(self.per_wiki)
            .into_iter()
            .filter_map(|w| {
                let n = self.edits.group(g, self.per_wiki, w.clone())?;
                let b = self.edits.group(g, self.bytes_per_wiki, w.clone())?;
                Some((w, g.get(n).ok()?, g.get(b).ok()?))
            })
            .collect();
        wikis.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let total_wikis = wikis.len();
        let top: Vec<String> = wikis
            .into_iter()
            .take(top as usize)
            .map(|(w, n, b)| format!("[{},{n},{}]", json_str(&w), finite(b)))
            .collect();
        format!(
            r#"{{"rows":{},"bots":{},"botShare":{},"added":{},"removed":{},"wikis":{total_wikis},"top":[{}]}}"#,
            g.get(self.rows).unwrap_or(0),
            finite(g.get(self.bots).unwrap_or(0.0)),
            finite(g.get(self.bot_share).unwrap_or(0.0)),
            finite(g.get(self.added).unwrap_or(0.0)),
            finite(g.get(self.removed).unwrap_or(0.0)),
            top.join(",")
        )
    }

    /// Why one wiki's edit count has its value.
    pub fn explain_wiki(&self, wiki: &str) -> Option<String> {
        let node = self
            .edits
            .group(&self.graph, self.per_wiki, wiki.to_string())?;
        Some(self.graph.explain(node))
    }

    /// Checks the incremental values against a full recalculation.
    pub fn check(&self) -> Option<String> {
        self.graph.check_consistency().err()
    }
}

fn finite(x: f64) -> String {
    if x.is_finite() {
        // `+ 0.0` turns -0 (an empty float sum) into 0.
        (x + 0.0).to_string()
    } else {
        "0".into()
    }
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

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn value_json(v: &Value) -> String {
    let (kind, value) = match v {
        Value::F64(x) if x.is_finite() => ("f64", x.to_string()),
        Value::F64(_) => ("f64", "null".into()),
        Value::I64(x) => ("i64", x.to_string()),
        Value::Bool(b) => ("bool", b.to_string()),
        Value::Str(s) => ("string", json_str(s)),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(|s| json_str(s)).collect();
            ("list", format!("[{}]", parts.join(",")))
        }
        Value::Error(e) => ("error", json_str(e)),
    };
    format!(
        r#"{{"kind":"{kind}","value":{value},"text":{}}}"#,
        json_str(&v.to_string())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_a_rolling_window_and_total_it() {
        let mut e = Edits::new(3).unwrap();
        e.add("1", "en.wikipedia.org", 120.0, false).unwrap();
        e.add("2", "de.wikipedia.org", -40.0, true).unwrap();
        e.add("3", "en.wikipedia.org", 10.0, false).unwrap();
        e.add("3", "en.wikipedia.org", 999.0, false).unwrap(); // a repeat is ignored
        e.update();
        assert_eq!(
            e.summary(5),
            r#"{"rows":3,"bots":1,"botShare":0.3333333333333333,"added":130,"removed":-40,"wikis":2,"top":[["en.wikipedia.org",2,130],["de.wikipedia.org",1,-40]]}"#
        );
        // A fourth edit pushes out the first.
        e.add("4", "fr.wikipedia.org", 5.0, true).unwrap();
        let r = e.update();
        assert!(r.contains(r#""epoch":"#), "{r}");
        assert_eq!(
            e.summary(1),
            r#"{"rows":3,"bots":2,"botShare":0.6666666666666666,"added":15,"removed":-40,"wikis":3,"top":[["de.wikipedia.org",1,-40]]}"#
        );
        assert!(
            e.explain_wiki("en.wikipedia.org")
                .unwrap()
                .contains("edits_by_wiki")
        );
        assert_eq!(e.check(), None);
    }

    #[test]
    fn an_empty_window_has_no_bot_share() {
        let mut e = Edits::new(10).unwrap();
        e.update();
        assert!(e.summary(3).contains(r#""rows":0,"bots":0,"botShare":0"#));
    }
}
