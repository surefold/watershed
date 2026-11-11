//! watershed as a C library for the JVM.
//!
//! The JVM calls the functions in `ffi.rs` directly, through its foreign
//! function API (`java.lang.foreign`, JDK 22 on): plain numbers go in
//! registers, and anything bigger (a name, an expression, a batch of rows)
//! as the address and length of bytes the JVM wrote (see `wire.rs`). There
//! is no generated glue, and a call costs tens of nanoseconds.
//!
//! This file is the safe part: what each call does, given values already
//! read. `ffi.rs` is the unsafe part, and only turns addresses into
//! references and slices before calling in here.
//!
//! On the JVM side, `java/` holds one small class that calls these
//! functions, and `kotlin/` and `scala/` hold the typed layers that
//! programs use:
//!
//! ```kotlin
//! val g = Graph()
//! val qty = g.input("qty", 10.0)                     // Input<Double>
//! val price = g.input("price", 2.5)
//! val total = g.define("total", qty * price * 1.2)   // Node<Double>
//! g[price] = 3.0
//! g.stabilize()
//! val now: Double = g[total]
//! ```

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use watershed_core::{AggregateOp, Collection as CoreCollection, Ir, NodeId, Type, Value};

mod ffi;
mod wire;

use wire::{Reader, Result};

fn refused<T>(e: impl ToString) -> Result<T> {
    Err(e.to_string())
}

/// The engine's graph, and the nodes that have been handed to the JVM.
///
/// The JVM names a node by a number. The engine's own ids cannot be made
/// from a number outside it, so each node handed out is kept here and its
/// place in this list is the number.
pub(crate) struct State {
    graph: watershed_core::Graph,
    handed_out: Vec<NodeId>,
    numbers: HashMap<NodeId, i32>,
}

impl State {
    /// The number the JVM knows this node by.
    fn number(&mut self, id: NodeId) -> i32 {
        if let Some(&number) = self.numbers.get(&id) {
            return number;
        }
        let number = i32::try_from(self.handed_out.len()).expect("too many nodes handed out");
        self.handed_out.push(id);
        self.numbers.insert(id, number);
        number
    }

    /// The node the JVM means by this number.
    fn node(&self, number: i32) -> Result<NodeId> {
        usize::try_from(number)
            .ok()
            .and_then(|at| self.handed_out.get(at))
            .copied()
            .ok_or_else(|| format!("there is no node {number}"))
    }

    /// A node that can be read: not removed, and being calculated.
    fn readable(&self, number: i32) -> Result<NodeId> {
        let id = self.node(number)?;
        if self.graph.is_removed(id) {
            return refused(format!("{:?} has been removed", self.graph.name(id)));
        }
        if !self.graph.is_calculated(id) && !self.graph.is_input(id) {
            return refused(format!(
                "{:?} is not being calculated: nothing that is watched uses it",
                self.graph.name(id)
            ));
        }
        Ok(id)
    }

    fn expression(&self, bytes: &[u8]) -> Result<Ir> {
        wire::expression(
            bytes,
            &|number| Ok(self.graph.reference(self.node(number)?)),
            &|part| {
                refused(format!(
                    "{part:?} is part of a collection's rows, and can only be used in its columns"
                ))
            },
        )
    }
}

/// A graph, as the JVM holds it.
pub(crate) struct Graph {
    inner: Mutex<State>,
}

impl Graph {
    fn new() -> Graph {
        Graph {
            inner: Mutex::new(State {
                graph: watershed_core::Graph::new(),
                handed_out: Vec::new(),
                numbers: HashMap::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while the graph was held would be a bug in the engine;
        // the graph is still the best there is to carry on with.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn input(&self, name: &str, value: Value) -> Result<i32> {
        let mut g = self.lock();
        let id = g.graph.input_value(name, value).or_else(refused)?;
        Ok(g.number(id))
    }

    fn define(&self, name: &str, expression: &[u8]) -> Result<i32> {
        let mut g = self.lock();
        let ir = g.expression(expression)?;
        let id = g.graph.define_ir(name, ir).or_else(refused)?;
        Ok(g.number(id))
    }

    /// Many nodes in one call, in order: each is a name and an expression.
    /// Those before one that is refused stay.
    fn define_many(&self, data: &[u8], out: &mut [i32]) -> Result<()> {
        let mut g = self.lock();
        let mut reader = Reader::new(data);
        for slot in out.iter_mut() {
            let name = reader.text()?;
            let length = reader.count()?;
            let ir = g.expression(reader.bytes(length)?)?;
            let id = g.graph.define_ir(name, ir).or_else(refused)?;
            *slot = g.number(id);
        }
        Ok(())
    }

    fn set(&self, node: i32, value: Value) -> Result<()> {
        let mut g = self.lock();
        let id = g.node(node)?;
        g.graph.set_value(id, value).or_else(refused)
    }

    fn set_numbers(&self, nodes: &[i32], values: &[f64]) -> Result<()> {
        let mut g = self.lock();
        for (&node, &value) in nodes.iter().zip(values) {
            let id = g.node(node)?;
            g.graph.set_value(id, Value::F64(value)).or_else(refused)?;
        }
        Ok(())
    }

    /// A node's value, cloned out from under the lock.
    fn value(&self, node: i32) -> Result<Value> {
        let g = self.lock();
        let id = g.readable(node)?;
        Ok(g.graph.value(id).clone())
    }

    /// A number node's value, or `None` if it is anything but a number.
    fn number(&self, node: i32) -> Option<f64> {
        let g = self.lock();
        match g.graph.value(g.readable(node).ok()?) {
            Value::F64(x) => Some(*x),
            _ => None,
        }
    }

    fn stabilize(&self) -> [i64; 4] {
        let report = self.lock().graph.stabilize();
        [
            report.epoch as i64,
            report.inputs_changed as i64,
            report.recalculated as i64,
            report.changed as i64,
        ]
    }

    fn explain(&self, node: i32) -> Result<String> {
        let g = self.lock();
        Ok(g.graph.explain_node(g.node(node)?))
    }

    fn name(&self, node: i32) -> Result<String> {
        let g = self.lock();
        Ok(g.graph.name(g.node(node)?).to_string())
    }

    fn last_recalculated(&self) -> Vec<String> {
        let g = self.lock();
        g.graph
            .last_recalculated()
            .iter()
            .map(|&id| g.graph.name(id).to_string())
            .collect()
    }

    fn size(&self) -> i64 {
        self.lock().graph.len() as i64
    }

    fn remove(&self, node: i32) -> Result<()> {
        let mut g = self.lock();
        let id = g.node(node)?;
        g.graph.remove_node(id).or_else(refused)
    }

    fn watch(&self, node: i32, on: bool) -> Result<()> {
        let mut g = self.lock();
        let id = g.node(node)?;
        if on {
            g.graph.watch_node(id).or_else(refused)
        } else {
            g.graph.unwatch_node(id);
            Ok(())
        }
    }

    fn calculate_only_watched(&self, on: bool) {
        self.lock().graph.calculate_only_watched(on);
    }

    fn check_consistency(&self) -> Result<()> {
        self.lock().graph.check_consistency()
    }

    fn collection(&self, name: &str) -> Collection<'_> {
        let inner = CoreCollection::new(&self.lock().graph, name);
        Collection {
            graph: self,
            inner: Mutex::new(inner),
        }
    }
}

/// A keyed table of rows, as the JVM holds it. It borrows its graph, which
/// the JVM side keeps alive for as long as the collection is.
pub(crate) struct Collection<'g> {
    graph: &'g Graph,
    inner: Mutex<CoreCollection>,
}

impl Collection<'_> {
    fn lock(&self) -> MutexGuard<'_, CoreCollection> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A field or column, by name.
    fn part(c: &CoreCollection, g: &State, name: &str) -> Result<Ir> {
        // Between backticks, so that any name is a name.
        c.parse_formula(&g.graph, &format!("`{name}`"))
            .map(|(part, _)| part)
            .or_else(refused)
    }

    fn field(&self, name: &str, kind: u8) -> Result<()> {
        let ty = match kind {
            wire::NUMBER => Type::F64,
            wire::WHOLE => Type::I64,
            wire::DECIMAL => Type::Decimal,
            wire::FLAG => Type::Bool,
            wire::TEXT => Type::Str,
            other => return refused(format!("there is no kind of field numbered {other}")),
        };
        self.lock().add_field(name, ty).map(|_| ()).or_else(refused)
    }

    fn column(&self, name: &str, expression: &[u8]) -> Result<()> {
        // The graph first, then the collection, everywhere: one order, so
        // two threads can never each hold what the other is waiting for.
        let mut g = self.graph.lock();
        let mut c = self.lock();
        let ir = wire::expression(
            expression,
            &|number| Ok(g.graph.reference(g.node(number)?)),
            &|part| Self::part(&c, &g, part),
        )?;
        c.add_column(&mut g.graph, name, ir)
            .map(|_| ())
            .or_else(refused)
    }

    /// A total over the rows; `of` is the field or column, or `None` to
    /// count rows. Returns its node.
    fn total(&self, name: &str, op: i32, of: Option<&str>) -> Result<i32> {
        let op = match op {
            0 => AggregateOp::Sum,
            1 => AggregateOp::Min,
            2 => AggregateOp::Max,
            3 => AggregateOp::Count,
            other => return refused(format!("there is no kind of total numbered {other}")),
        };
        let mut g = self.graph.lock();
        let mut c = self.lock();
        let of = of.map(|part| Self::part(&c, &g, part)).transpose()?;
        let id = c
            .add_total(&mut g.graph, name, op, of.as_ref())
            .or_else(refused)?;
        Ok(g.number(id))
    }

    /// Adds or changes many rows in one call. Each is a key, how many
    /// fields are given, and each field's name and value. Rows before one
    /// that is refused stay.
    fn upsert(&self, data: &[u8], rows: usize) -> Result<()> {
        let mut g = self.graph.lock();
        let mut c = self.lock();
        let mut reader = Reader::new(data);
        let mut fields: Vec<(&str, Value)> = Vec::new();
        for _ in 0..rows {
            let key = reader.text()?;
            fields.clear();
            for _ in 0..reader.count()? {
                let name = reader.text()?;
                fields.push((name, reader.value()?));
            }
            c.upsert_named(&mut g.graph, key, &fields)
                .or_else(refused)?;
        }
        Ok(())
    }

    fn remove(&self, key: &str) -> Result<bool> {
        let mut g = self.graph.lock();
        self.lock().remove(&mut g.graph, key).or_else(refused)
    }

    fn value(&self, key: &str, part: &str) -> Result<Value> {
        let g = self.graph.lock();
        let c = self.lock();
        let part = Self::part(&c, &g, part)?;
        c.get_value(&g.graph, key, &part).or_else(refused)
    }

    fn size(&self) -> i64 {
        self.lock().len() as i64
    }
}
