//! Collections: keyed tables of rows, with calculated columns and totals
//! that update by only the rows that change.
//!
//! ```
//! use watershed_core::prelude::*;
//!
//! # fn main() -> Result<(), CalcError> {
//! let mut g = Graph::new();
//! let fx = g.input("eur_usd", 1.25)?;
//!
//! let mut orders = Collection::new(&g, "orders");
//! let qty = orders.field::<f64>("qty")?;
//! let price = orders.field::<f64>("price")?;
//! let line = orders.column(&mut g, "line_total", qty * price * fx)?;
//! let revenue = orders.sum(&mut g, "revenue", line)?;
//! let count = orders.count(&mut g, "orders")?;
//!
//! orders.upsert(&mut g, "o-1", &[qty.is(2.0), price.is(10.0)])?;
//! orders.upsert(&mut g, "o-2", &[qty.is(1.0), price.is(5.0)])?;
//! g.stabilize();
//! assert_eq!(g.get(revenue)?, 31.25);
//! assert_eq!(g.get(count)?, 2);
//!
//! // Changing one row recalculates that row and the totals, nothing else.
//! orders.upsert(&mut g, "o-2", &[qty.is(3.0)])?;
//! g.stabilize();
//! assert_eq!(g.get(revenue)?, 43.75);
//!
//! orders.remove(&mut g, "o-1")?;
//! g.stabilize();
//! assert_eq!(g.get(count)?, 1);
//! # Ok(())
//! # }
//! ```
//!
//! Each row is a set of ordinary nodes: an input per field, a node per
//! column, and a `bool` input saying whether the row is present. A removed
//! row is marked absent and its nodes are reused by the next new row, so a
//! collection's graph grows only to its largest size. Totals are aggregates
//! (see [`Graph::aggregate`]), which update by only the rows that changed and
//! give exactly the result of adding everything up from scratch.
//!
//! Like [`Graph::set`], changes to rows take effect at the next
//! [`Graph::stabilize`].

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::aggregate::AggregateOp;
use crate::error::CalcError;
use crate::expr::IntoExpr;
use crate::graph::{Graph, next_graph_id};
use crate::handle::{GraphId, Node, NodeId, NodeRef};
use crate::ir::{Ir, TypeError};
use crate::value::{CalcType, Numeric, Type, Value};

/// Marks a placeholder that stands for a column rather than a field.
const COLUMN: u32 = 1 << 31;

/// A field of a collection's rows: an input in each row. Use it in column
/// formulas like any other value, and set it with [`Field::is`].
pub struct Field<T> {
    pub(crate) at: NodeRef,
    _type: PhantomData<fn() -> T>,
}

/// A calculated column of a collection: a node in each row, worked out by
/// the same formula from that row's fields and other columns.
pub struct Column<T> {
    pub(crate) at: NodeRef,
    _type: PhantomData<fn() -> T>,
}

impl<T> Clone for Field<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Field<T> {}
impl<T> Clone for Column<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Column<T> {}

impl<T> std::fmt::Debug for Field<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Field({})", self.at.node.0)
    }
}
impl<T> std::fmt::Debug for Column<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Column({})", self.at.node.0 & !COLUMN)
    }
}

/// A field's new value in a row, made with [`Field::is`].
#[derive(Debug, Clone)]
pub struct Set {
    field: NodeRef,
    value: Value,
}

impl<T: CalcType> Field<T> {
    /// This field's value for [`Collection::upsert`].
    pub fn is(self, value: T) -> Set {
        Set {
            field: self.at,
            value: value.into_value(),
        }
    }
}

impl<T> IntoExpr<T> for Field<T> {
    fn into_expr(self) -> crate::expr::Expr<T> {
        crate::expr::Expr::from_ir(Ir::Ref(self.at))
    }
}
impl<T> IntoExpr<T> for Column<T> {
    fn into_expr(self) -> crate::expr::Expr<T> {
        crate::expr::Expr::from_ir(Ir::Ref(self.at))
    }
}

/// A field or column, for totals and for reading a row.
pub trait Part<T> {
    #[doc(hidden)]
    fn placeholder(&self) -> NodeRef;
}
impl<T> Part<T> for Field<T> {
    fn placeholder(&self) -> NodeRef {
        self.at
    }
}
impl<T> Part<T> for Column<T> {
    fn placeholder(&self) -> NodeRef {
        self.at
    }
}

struct ColumnDef {
    name: String,
    ty: Type,
    /// The formula, with placeholders for this row's fields and columns.
    formula: Arc<Ir>,
    /// The height of the column's nodes, the same in every row.
    height: usize,
}

/// One row's nodes.
struct Slot {
    present: NodeId,
    fields: Vec<NodeId>,
    columns: Vec<NodeId>,
}

/// What a total adds up: a field, a column, or (for a count) presence.
#[derive(Clone, Copy)]
enum Target {
    Placeholder(NodeRef),
    Presence,
}

/// A keyed table of rows. See the [module documentation](self).
pub struct Collection {
    graph: GraphId,
    template: GraphId,
    name: String,
    fields: Vec<(String, Type)>,
    columns: Vec<ColumnDef>,
    slots: Vec<Slot>,
    keys: HashMap<String, usize>,
    free: Vec<usize>,
    totals: Vec<(NodeId, Target)>,
}

impl Collection {
    /// A new, empty collection in graph `g`. Its nodes are named
    /// `name[slot].field`.
    pub fn new(g: &Graph, name: &str) -> Collection {
        Collection {
            graph: g.id(),
            template: next_graph_id(),
            name: name.to_string(),
            fields: Vec::new(),
            columns: Vec::new(),
            slots: Vec::new(),
            keys: HashMap::new(),
            free: Vec::new(),
            totals: Vec::new(),
        }
    }

    /// Adds a field. Fields come before the first row.
    pub fn field<T: CalcType>(&mut self, name: &str) -> Result<Field<T>, CalcError> {
        if !self.slots.is_empty() {
            return Err(CalcError::Invalid(format!(
                "{}: add fields before the first row",
                self.name
            )));
        }
        if self.part_named(name) {
            return Err(CalcError::DuplicateName(format!("{}.{name}", self.name)));
        }
        let index = u32::try_from(self.fields.len()).expect("too many fields");
        self.fields.push((name.to_string(), T::TYPE));
        Ok(Field {
            at: self.placeholder(index),
            _type: PhantomData,
        })
    }

    /// Adds a column, worked out in every row by `formula`, which can use
    /// this collection's fields and columns and any node of the graph.
    pub fn column<T: CalcType>(
        &mut self,
        g: &mut Graph,
        name: &str,
        formula: impl IntoExpr<T>,
    ) -> Result<Column<T>, CalcError> {
        self.check_graph(g)?;
        if self.part_named(name) {
            return Err(CalcError::DuplicateName(format!("{}.{name}", self.name)));
        }
        let formula = formula.into_expr().ir;
        let ty = formula.type_check(&|r| self.type_of(g, r))?;
        if ty != T::TYPE {
            return Err(CalcError::TypeMismatch {
                node: name.to_string(),
                expected: T::TYPE,
                found: Some(ty),
            });
        }
        let mut refs = Vec::new();
        formula.refs(&mut refs);
        let height = 1 + refs
            .iter()
            .map(|&r| self.height_of(g, r))
            .max()
            .unwrap_or(0);
        let index = u32::try_from(self.columns.len()).expect("too many columns");
        self.columns.push(ColumnDef {
            name: name.to_string(),
            ty,
            formula,
            height,
        });
        // Rows that already exist (including removed ones, which can be reused) get it too.
        for s in 0..self.slots.len() {
            let node = self.define_column(g, s, index as usize)?;
            self.slots[s].columns.push(node);
        }
        Ok(Column {
            at: self.placeholder(COLUMN | index),
            _type: PhantomData,
        })
    }

    /// A total: the sum of a field or column over the present rows.
    pub fn sum<T: Numeric>(
        &mut self,
        g: &mut Graph,
        name: &str,
        of: impl Part<T>,
    ) -> Result<Node<T>, CalcError> {
        self.total(
            g,
            name,
            AggregateOp::Sum,
            T::TYPE,
            Target::Placeholder(of.placeholder()),
        )
    }

    /// A total: the smallest value of a field or column over the present
    /// rows (+∞, or `i64::MAX`, if there are none).
    pub fn min<T: Numeric>(
        &mut self,
        g: &mut Graph,
        name: &str,
        of: impl Part<T>,
    ) -> Result<Node<T>, CalcError> {
        self.total(
            g,
            name,
            AggregateOp::Min,
            T::TYPE,
            Target::Placeholder(of.placeholder()),
        )
    }

    /// A total: the largest value of a field or column over the present
    /// rows (-∞, or `i64::MIN`, if there are none).
    pub fn max<T: Numeric>(
        &mut self,
        g: &mut Graph,
        name: &str,
        of: impl Part<T>,
    ) -> Result<Node<T>, CalcError> {
        self.total(
            g,
            name,
            AggregateOp::Max,
            T::TYPE,
            Target::Placeholder(of.placeholder()),
        )
    }

    /// A total: how many rows are present.
    pub fn count(&mut self, g: &mut Graph, name: &str) -> Result<Node<i64>, CalcError> {
        self.total(g, name, AggregateOp::Count, Type::I64, Target::Presence)
    }

    fn total<T>(
        &mut self,
        g: &mut Graph,
        name: &str,
        op: AggregateOp,
        value_type: Type,
        target: Target,
    ) -> Result<Node<T>, CalcError> {
        self.check_graph(g)?;
        let height = match target {
            Target::Placeholder(r) => {
                if r.graph != self.template {
                    return Err(CalcError::ForeignNode);
                }
                self.height_of(g, r)
            }
            Target::Presence => 0,
        };
        let members: Vec<(NodeId, NodeId)> = (0..self.slots.len())
            .map(|s| (self.member(s, target), self.slots[s].present))
            .collect();
        let id = g.aggregate(name, op, value_type, height, &members)?;
        self.totals.push((id, target));
        Ok(Node::new(g.node_ref(id)))
    }

    /// Adds a row, or changes the given fields of an existing one. A new row
    /// needs every field. Takes effect at the next update.
    pub fn upsert(&mut self, g: &mut Graph, key: &str, row: &[Set]) -> Result<(), CalcError> {
        self.check_graph(g)?;
        let mut values: Vec<Option<Value>> = vec![None; self.fields.len()];
        for set in row {
            if set.field.graph != self.template || set.field.node.0 & COLUMN != 0 {
                return Err(CalcError::Invalid(format!(
                    "{}: that is not one of its fields",
                    self.name
                )));
            }
            let (name, ty) = &self.fields[set.field.node.0 as usize];
            if set.value.type_of() != Some(*ty) {
                return Err(CalcError::TypeMismatch {
                    node: format!("{}.{name}", self.name),
                    expected: *ty,
                    found: set.value.type_of(),
                });
            }
            values[set.field.node.0 as usize] = Some(set.value.clone());
        }
        if let Some(&slot) = self.keys.get(key) {
            for (k, value) in values.into_iter().enumerate() {
                if let Some(value) = value {
                    g.set_value(self.slots[slot].fields[k], value)?;
                }
            }
            return Ok(());
        }
        if let Some(k) = values.iter().position(Option::is_none) {
            return Err(CalcError::Invalid(format!(
                "{}: a new row needs every field, and {:?} is missing",
                self.name, self.fields[k].0
            )));
        }
        let values: Vec<Value> = values.into_iter().map(Option::unwrap).collect();
        let slot = match self.free.pop() {
            Some(slot) => {
                for (k, value) in values.into_iter().enumerate() {
                    g.set_value(self.slots[slot].fields[k], value)?;
                }
                slot
            }
            None => self.new_slot(g, values)?,
        };
        g.set_value(self.slots[slot].present, Value::Bool(true))?;
        self.keys.insert(key.to_string(), slot);
        Ok(())
    }

    /// Removes a row. Returns whether there was one. Takes effect at the next update.
    pub fn remove(&mut self, g: &mut Graph, key: &str) -> Result<bool, CalcError> {
        self.check_graph(g)?;
        let Some(slot) = self.keys.remove(key) else {
            return Ok(false);
        };
        g.set_value(self.slots[slot].present, Value::Bool(false))?;
        self.free.push(slot);
        Ok(true)
    }

    /// A field or column of a row, as of the last update.
    pub fn get<T: CalcType>(&self, g: &Graph, key: &str, of: impl Part<T>) -> Result<T, CalcError> {
        self.check_graph(g)?;
        let slot = *self
            .keys
            .get(key)
            .ok_or_else(|| CalcError::Invalid(format!("{} has no row {key:?}", self.name)))?;
        let id = self.member(slot, Target::Placeholder(of.placeholder()));
        g.get(Node::<T>::new(g.node_ref(id)))
    }

    /// The node for a row's field or column, to use like any other node.
    pub fn node<T>(&self, g: &Graph, key: &str, of: impl Part<T>) -> Option<Node<T>> {
        let slot = *self.keys.get(key)?;
        Some(Node::new(g.node_ref(
            self.member(slot, Target::Placeholder(of.placeholder())),
        )))
    }

    /// How many rows there are (including ones added since the last update).
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains_key(key)
    }

    /// The rows' keys, in no particular order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.keys.keys().map(String::as_str)
    }

    // ---------------------------------------------------------------- inside

    fn placeholder(&self, index: u32) -> NodeRef {
        NodeRef {
            graph: self.template,
            node: NodeId(index),
        }
    }

    fn part_named(&self, name: &str) -> bool {
        self.fields.iter().any(|(n, _)| n == name) || self.columns.iter().any(|c| c.name == name)
    }

    fn check_graph(&self, g: &Graph) -> Result<(), CalcError> {
        if g.id() != self.graph {
            return Err(CalcError::ForeignNode);
        }
        Ok(())
    }

    fn type_of(&self, g: &Graph, r: NodeRef) -> Result<Type, TypeError> {
        if r.graph == self.template {
            let i = r.node.0;
            let ty = if i & COLUMN != 0 {
                self.columns.get((i & !COLUMN) as usize).map(|c| c.ty)
            } else {
                self.fields.get(i as usize).map(|f| f.1)
            };
            return ty
                .ok_or_else(|| TypeError(format!("{} has no such field or column", self.name)));
        }
        if r.graph == g.id() && r.node.index() < g.len() {
            return Ok(g.node_type(r.node));
        }
        Err(TypeError(
            "a node from a different graph or collection was used".into(),
        ))
    }

    /// The height a placeholder or graph node has (a field is an input: 0).
    fn height_of(&self, g: &Graph, r: NodeRef) -> usize {
        if r.graph == self.template {
            let i = r.node.0;
            if i & COLUMN != 0 {
                self.columns[(i & !COLUMN) as usize].height
            } else {
                0
            }
        } else {
            g.height(r.node)
        }
    }

    /// The node in row `slot` for a field, column or presence.
    fn member(&self, slot: usize, target: Target) -> NodeId {
        let s = &self.slots[slot];
        match target {
            Target::Presence => s.present,
            Target::Placeholder(r) => {
                let i = r.node.0;
                if i & COLUMN != 0 {
                    s.columns[(i & !COLUMN) as usize]
                } else {
                    s.fields[i as usize]
                }
            }
        }
    }

    /// The column's formula with this row's nodes in place of placeholders.
    fn instantiate(&self, g: &Graph, slot: usize, ir: &Ir) -> Ir {
        let re = |ir: &Arc<Ir>| Arc::new(self.instantiate(g, slot, ir));
        match ir {
            Ir::Ref(r) if r.graph == self.template => {
                g.reference(self.member(slot, Target::Placeholder(*r)))
            }
            Ir::Const(_) | Ir::Ref(_) => ir.clone(),
            Ir::Unary(op, a) => Ir::Unary(*op, re(a)),
            Ir::Binary(op, a, b) => Ir::Binary(*op, re(a), re(b)),
            Ir::If(c, t, e) => Ir::If(re(c), re(t), re(e)),
            Ir::Choose(i, options) => Ir::Choose(re(i), options.iter().map(re).collect()),
        }
    }

    fn define_column(
        &self,
        g: &mut Graph,
        slot: usize,
        column: usize,
    ) -> Result<NodeId, CalcError> {
        let def = &self.columns[column];
        let ir = self.instantiate(g, slot, &def.formula);
        g.define_ir(&format!("{}[{slot}].{}", self.name, def.name), ir)
    }

    /// Makes a new row's nodes, absent until the update; it joins every total.
    fn new_slot(&mut self, g: &mut Graph, values: Vec<Value>) -> Result<usize, CalcError> {
        let s = self.slots.len();
        let present = g.input_value(&format!("{}[{s}].present", self.name), Value::Bool(false))?;
        let mut fields = Vec::with_capacity(values.len());
        for ((name, _), value) in self.fields.iter().zip(values) {
            fields.push(g.input_value(&format!("{}[{s}].{name}", self.name), value)?);
        }
        self.slots.push(Slot {
            present,
            fields,
            columns: Vec::new(),
        });
        for c in 0..self.columns.len() {
            let node = self.define_column(g, s, c)?;
            self.slots[s].columns.push(node);
        }
        for &(total, target) in &self.totals {
            g.extend_aggregate(total, self.member(s, target), present)?;
        }
        Ok(s)
    }
}
