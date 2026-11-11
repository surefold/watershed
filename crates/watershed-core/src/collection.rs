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
//! row is marked absent and its nodes are reused by the next new row, so a collection's graph grows only to its
//! largest size. Totals are aggregates
//! (see [`Graph::aggregate`]), which update by only the rows that changed and
//! give exactly the result of adding everything up from scratch.
//!
//! Like [`Graph::set`], changes to rows take effect at the next
//! [`Graph::stabilize`].
//!
//! With the graph's history on ([`Graph::keep_history`]), a row can be read
//! as it was at an earlier update: [`Collection::get_at`],
//! [`Collection::contains_at`] and [`Collection::keys_at`]. While history is
//! on, the collection notes which key each reused row's nodes held, so a
//! key is followed from one row's nodes to another's.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::aggregate::AggregateOp;
use crate::error::CalcError;
use crate::expr::IntoExpr;
use crate::graph::{Graph, next_graph_id};
use crate::handle::{GraphId, Node, NodeId, NodeRef};
use crate::ir::{BinOp, Ir, TypeError};
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

/// A change of the key in a slot.
struct KeyChange {
    /// The update from which the slot has its next key.
    from: u64,
    /// The key before that.
    before: String,
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
    /// For each key, the slots it held before the one it holds now (if
    /// any), so it can be found at earlier updates.
    past_slots: HashMap<String, Vec<usize>>,
    /// For each slot whose key has changed, the changes still within
    /// history, oldest first.
    key_changes: HashMap<usize, Vec<KeyChange>>,
    /// The update before which `past_slots` and `key_changes` were last
    /// written. While history is off they hold only changes since then, in
    /// case history starts before the next update.
    recorded_at: u64,
    free: Vec<usize>,
    totals: Vec<(NodeId, Target)>,
    groupings: Vec<Grouping>,
}

/// Totals per group: one aggregate per distinct value of a field.
struct Grouping {
    name: String,
    op: AggregateOp,
    value_type: Type,
    /// The field whose value names a row's group.
    by: usize,
    target: Target,
    /// The height the group totals sit above.
    height: usize,
    /// Each group's total, by the group's key.
    groups: HashMap<String, NodeId>,
    /// For each row and each group it has been in, the `bool` node saying
    /// whether it is in that group now (present, and its key is that group's).
    membership: HashMap<(usize, String), NodeId>,
    /// Each row's group as of the latest upsert (None if absent).
    slot_key: Vec<Option<String>>,
    /// How many rows each group has, as of the latest upserts.
    counts: HashMap<String, usize>,
}

/// A grouped total: one total per distinct value of a field, made by
/// [`Collection::group_sum`] and its siblings. `K` is the field's type and
/// `T` the totals' type.
pub struct Groups<K, T> {
    template: GraphId,
    index: usize,
    _type: PhantomData<fn() -> (K, T)>,
}

impl<K, T> Clone for Groups<K, T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<K, T> Copy for Groups<K, T> {}

/// A group's key as text, the same for a key however it is given.
fn key_text(value: &Value) -> String {
    match value {
        Value::Str(s) => s.to_string(),
        other => other.to_string(),
    }
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
            past_slots: HashMap::new(),
            key_changes: HashMap::new(),
            recorded_at: 0,
            free: Vec::new(),
            totals: Vec::new(),
            groupings: Vec::new(),
        }
    }

    /// Adds a field. Fields come before the first row.
    pub fn field<T: CalcType>(&mut self, name: &str) -> Result<Field<T>, CalcError> {
        Ok(Field {
            at: self.field_of_type(name, T::TYPE)?,
            _type: PhantomData,
        })
    }

    fn field_of_type(&mut self, name: &str, ty: Type) -> Result<NodeRef, CalcError> {
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
        self.fields.push((name.to_string(), ty));
        Ok(self.placeholder(index))
    }

    /// Adds a column, worked out in every row by `formula`, which can use
    /// this collection's fields and columns and any node of the graph.
    pub fn column<T: CalcType>(
        &mut self,
        g: &mut Graph,
        name: &str,
        formula: impl IntoExpr<T>,
    ) -> Result<Column<T>, CalcError> {
        let (at, _) = self.column_of(g, name, formula.into_expr().ir, Some(T::TYPE))?;
        Ok(Column {
            at,
            _type: PhantomData,
        })
    }

    /// Adds a column; if `expected` is given, the formula must have that type.
    fn column_of(
        &mut self,
        g: &mut Graph,
        name: &str,
        formula: Arc<Ir>,
        expected: Option<Type>,
    ) -> Result<(NodeRef, Type), CalcError> {
        self.check_graph(g)?;
        if self.part_named(name) {
            return Err(CalcError::DuplicateName(format!("{}.{name}", self.name)));
        }
        let ty = formula.type_check(&|r| self.type_of(g, r))?;
        if let Some(expected) = expected.filter(|&e| e != ty) {
            return Err(CalcError::TypeMismatch {
                node: name.to_string(),
                expected,
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
        Ok((self.placeholder(COLUMN | index), ty))
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
        let row: Vec<(NodeRef, Value)> = row.iter().map(|s| (s.field, s.value.clone())).collect();
        self.upsert_values(g, key, &row)
    }

    fn upsert_values(
        &mut self,
        g: &mut Graph,
        key: &str,
        row: &[(NodeRef, Value)],
    ) -> Result<(), CalcError> {
        self.check_graph(g)?;
        let mut values: Vec<Option<Value>> = vec![None; self.fields.len()];
        for (field, value) in row {
            let set = Set {
                field: *field,
                value: value.clone(),
            };
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
        // Each grouping's new key for this row, if this upsert sets it.
        let group_keys: Vec<Option<Value>> = self
            .groupings
            .iter()
            .map(|grouping| values[grouping.by].clone())
            .collect();
        if let Some(&slot) = self.keys.get(key) {
            for (k, value) in values.into_iter().enumerate() {
                if let Some(value) = value {
                    g.set_value(self.slots[slot].fields[k], value)?;
                }
            }
            for (i, new) in group_keys.into_iter().enumerate() {
                if let Some(new) = new {
                    self.join_group(g, i, slot, &new)?;
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
        for (i, new) in group_keys.into_iter().enumerate() {
            let new = new.expect("a new row has every field");
            self.join_group(g, i, slot, &new)?;
        }
        Ok(())
    }

    /// Removes a row. Returns whether there was one. Takes effect at the next update.
    pub fn remove(&mut self, g: &mut Graph, key: &str) -> Result<bool, CalcError> {
        self.check_graph(g)?;
        let Some(slot) = self.keys.remove(key) else {
            return Ok(false);
        };
        self.start_recording(g);
        self.past_slots
            .entry(key.to_string())
            .or_default()
            .push(slot);
        self.key_changed(g, slot, key.to_string());
        g.set_value(self.slots[slot].present, Value::Bool(false))?;
        self.free.push(slot);
        for grouping in &mut self.groupings {
            if let Some(old) = grouping.slot_key[slot].take() {
                *grouping.counts.entry(old).or_default() -= 1;
            }
        }
        Ok(true)
    }

    // ---------------------------------------------------------------- groups

    /// Totals per group: the sum of a field or column over the present rows
    /// with each distinct value of the field `by`. Each group's total is a
    /// node, made when the first row with that key appears; when a row
    /// changes group, both groups update by just that row.
    pub fn group_sum<K: CalcType, T: Numeric>(
        &mut self,
        g: &mut Graph,
        name: &str,
        by: Field<K>,
        of: impl Part<T>,
    ) -> Result<Groups<K, T>, CalcError> {
        let index = self.grouping(
            g,
            name,
            AggregateOp::Sum,
            T::TYPE,
            by.at,
            Target::Placeholder(of.placeholder()),
        )?;
        Ok(Groups {
            template: self.template,
            index,
            _type: PhantomData,
        })
    }

    /// Totals per group: the smallest value of a field or column in each group.
    pub fn group_min<K: CalcType, T: Numeric>(
        &mut self,
        g: &mut Graph,
        name: &str,
        by: Field<K>,
        of: impl Part<T>,
    ) -> Result<Groups<K, T>, CalcError> {
        let index = self.grouping(
            g,
            name,
            AggregateOp::Min,
            T::TYPE,
            by.at,
            Target::Placeholder(of.placeholder()),
        )?;
        Ok(Groups {
            template: self.template,
            index,
            _type: PhantomData,
        })
    }

    /// Totals per group: the largest value of a field or column in each group.
    pub fn group_max<K: CalcType, T: Numeric>(
        &mut self,
        g: &mut Graph,
        name: &str,
        by: Field<K>,
        of: impl Part<T>,
    ) -> Result<Groups<K, T>, CalcError> {
        let index = self.grouping(
            g,
            name,
            AggregateOp::Max,
            T::TYPE,
            by.at,
            Target::Placeholder(of.placeholder()),
        )?;
        Ok(Groups {
            template: self.template,
            index,
            _type: PhantomData,
        })
    }

    /// Totals per group: how many present rows each group has.
    pub fn group_count<K: CalcType>(
        &mut self,
        g: &mut Graph,
        name: &str,
        by: Field<K>,
    ) -> Result<Groups<K, i64>, CalcError> {
        let index = self.grouping(
            g,
            name,
            AggregateOp::Count,
            Type::I64,
            by.at,
            Target::Presence,
        )?;
        Ok(Groups {
            template: self.template,
            index,
            _type: PhantomData,
        })
    }

    /// One group's total, or `None` if no row has ever had that key.
    pub fn group<K: CalcType, T>(
        &self,
        g: &Graph,
        groups: Groups<K, T>,
        key: K,
    ) -> Option<Node<T>> {
        if groups.template != self.template {
            return None;
        }
        let id = *self.groupings[groups.index]
            .groups
            .get(&key_text(&key.into_value()))?;
        Some(Node::new(g.node_ref(id)))
    }

    /// The keys of the groups that have rows (including changes since the
    /// last update), sorted.
    pub fn group_keys<K, T>(&self, groups: Groups<K, T>) -> Vec<String> {
        if groups.template != self.template {
            return Vec::new();
        }
        let mut keys: Vec<String> = self.groupings[groups.index]
            .counts
            .iter()
            .filter(|&(_, &n)| n > 0)
            .map(|(k, _)| k.clone())
            .collect();
        keys.sort();
        keys
    }

    /// Adds a grouping; rows that already exist join their groups.
    fn grouping(
        &mut self,
        g: &mut Graph,
        name: &str,
        op: AggregateOp,
        value_type: Type,
        by: NodeRef,
        target: Target,
    ) -> Result<usize, CalcError> {
        self.check_graph(g)?;
        if by.graph != self.template || by.node.0 & COLUMN != 0 {
            return Err(CalcError::Invalid(format!(
                "{}: group by one of its fields",
                self.name
            )));
        }
        if self.groupings.iter().any(|x| x.name == name) {
            return Err(CalcError::DuplicateName(name.to_string()));
        }
        let height = match target {
            Target::Placeholder(r) => {
                if r.graph != self.template {
                    return Err(CalcError::ForeignNode);
                }
                self.height_of(g, r)
            }
            Target::Presence => 0,
        }
        // A row's membership node (present and key == group) sits at height 1.
        .max(1);
        self.groupings.push(Grouping {
            name: name.to_string(),
            op,
            value_type,
            by: by.node.0 as usize,
            target,
            height,
            groups: HashMap::new(),
            membership: HashMap::new(),
            slot_key: vec![None; self.slots.len()],
            counts: HashMap::new(),
        });
        let index = self.groupings.len() - 1;
        let present: Vec<usize> = self.keys.values().copied().collect();
        for slot in present {
            let field = self.slots[slot].fields[self.groupings[index].by];
            let key = g.value(field).clone();
            self.join_group(g, index, slot, &key)?;
        }
        Ok(index)
    }

    /// Puts a row in the group for `key`: counts it there, and makes sure it
    /// has a membership node in that group (and that the group exists).
    fn join_group(
        &mut self,
        g: &mut Graph,
        index: usize,
        slot: usize,
        key: &Value,
    ) -> Result<(), CalcError> {
        let text = key_text(key);
        let grouping = &mut self.groupings[index];
        if grouping.slot_key.len() <= slot {
            grouping.slot_key.resize(slot + 1, None);
        }
        if grouping.slot_key[slot].as_ref() == Some(&text) {
            return Ok(());
        }
        if let Some(old) = grouping.slot_key[slot].replace(text.clone()) {
            *grouping.counts.entry(old).or_default() -= 1;
        }
        *grouping.counts.entry(text.clone()).or_default() += 1;
        if grouping.membership.contains_key(&(slot, text.clone())) {
            return Ok(()); // it has been in this group before, so it already counts there
        }
        // present && key == this group's key
        let (present, field) = (
            self.slots[slot].present,
            self.slots[slot].fields[grouping.by],
        );
        let in_group = Ir::Binary(
            BinOp::And,
            Arc::new(g.reference(present)),
            Arc::new(Ir::Binary(
                BinOp::Eq,
                Arc::new(g.reference(field)),
                Arc::new(Ir::Const(key.clone())),
            )),
        );
        let in_group = g.define_ir(
            &format!("{}[{slot}].{}={text}", self.name, grouping.name),
            in_group,
        )?;
        let target = grouping.target;
        let value = match target {
            Target::Presence => in_group,
            Target::Placeholder(_) => self.member(slot, target),
        };
        let grouping = &mut self.groupings[index];
        grouping.membership.insert((slot, text.clone()), in_group);
        match grouping.groups.get(&text) {
            Some(&total) => g.extend_aggregate(total, value, in_group)?,
            None => {
                let total = g.aggregate(
                    &format!("{}[{text}]", grouping.name),
                    grouping.op,
                    grouping.value_type,
                    grouping.height,
                    &[(value, in_group)],
                )?;
                grouping.groups.insert(text, total);
            }
        }
        Ok(())
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

    // ---------------------------------------------------------------- untyped

    // For front ends whose types are only known at run time, such as Python.
    // Fields and columns are passed around as their placeholder expressions.

    /// Adds a field of type `ty`; returns its placeholder for formulas.
    pub fn add_field(&mut self, name: &str, ty: Type) -> Result<Ir, CalcError> {
        Ok(Ir::Ref(self.field_of_type(name, ty)?))
    }

    /// Adds a column from an untyped formula; returns its placeholder and type.
    pub fn add_column(
        &mut self,
        g: &mut Graph,
        name: &str,
        formula: Ir,
    ) -> Result<(Ir, Type), CalcError> {
        let (at, ty) = self.column_of(g, name, Arc::new(formula), None)?;
        Ok((Ir::Ref(at), ty))
    }

    /// Adds a total of a field or column (given by its placeholder), or a
    /// count of rows when `of` is `None` (only for [`AggregateOp::Count`]).
    pub fn add_total(
        &mut self,
        g: &mut Graph,
        name: &str,
        op: AggregateOp,
        of: Option<&Ir>,
    ) -> Result<NodeId, CalcError> {
        let (target, value_type) = match (op, of) {
            (AggregateOp::Count, None) => (Target::Presence, Type::I64),
            (AggregateOp::Count, Some(_)) => {
                return Err(CalcError::Invalid(
                    "a count counts rows, not a field".into(),
                ));
            }
            (_, Some(Ir::Ref(r))) => {
                let ty = self.part_type(&Ir::Ref(*r)).ok_or_else(|| {
                    CalcError::Invalid(format!("{op} needs a field or column of {}", self.name))
                })?;
                (Target::Placeholder(*r), ty)
            }
            _ => {
                return Err(CalcError::Invalid(format!(
                    "{op} needs a field or column of {}",
                    self.name
                )));
            }
        };
        let node: Node<()> = self.total(g, name, op, value_type, target)?;
        Ok(node.id())
    }

    /// Adds a row, or changes fields of an existing one, by field name.
    pub fn upsert_named(
        &mut self,
        g: &mut Graph,
        key: &str,
        row: &[(&str, Value)],
    ) -> Result<(), CalcError> {
        let mut values = Vec::with_capacity(row.len());
        for (name, value) in row {
            let k = self
                .fields
                .iter()
                .position(|(n, _)| n == name)
                .ok_or_else(|| {
                    CalcError::Invalid(format!("{} has no field {name:?}", self.name))
                })?;
            values.push((self.placeholder(k as u32), value.clone()));
        }
        self.upsert_values(g, key, &values)
    }

    /// A row's field or column (given by its placeholder), as of the last update.
    pub fn get_value(&self, g: &Graph, key: &str, of: &Ir) -> Result<Value, CalcError> {
        self.check_graph(g)?;
        let Ir::Ref(r) = of else {
            return Err(CalcError::Invalid(format!(
                "not a field or column of {}",
                self.name
            )));
        };
        if self.part_type(of).is_none() {
            return Err(CalcError::Invalid(format!(
                "not a field or column of {}",
                self.name
            )));
        }
        let slot = *self
            .keys
            .get(key)
            .ok_or_else(|| CalcError::Invalid(format!("{} has no row {key:?}", self.name)))?;
        Ok(g.value(self.member(slot, Target::Placeholder(*r))).clone())
    }

    // ---------------------------------------------------------------- history

    /// A row's field or column as of the end of update `epoch`, read from the
    /// graph's history (see [`Graph::keep_history`]).
    pub fn get_at<T: CalcType>(
        &self,
        g: &Graph,
        key: &str,
        of: impl Part<T>,
        epoch: u64,
    ) -> Result<T, CalcError> {
        self.check_graph(g)?;
        g.history_covering(epoch)?;
        let slot = self.slot_at(g, key, epoch)?;
        let id = self.member(slot, Target::Placeholder(of.placeholder()));
        g.get_at(Node::<T>::new(g.node_ref(id)), epoch)
    }

    /// [`get_at`](Self::get_at) through the untyped API.
    pub fn get_value_at(
        &self,
        g: &Graph,
        key: &str,
        of: &Ir,
        epoch: u64,
    ) -> Result<Value, CalcError> {
        self.check_graph(g)?;
        g.history_covering(epoch)?;
        let (Ir::Ref(r), Some(_)) = (of, self.part_type(of)) else {
            return Err(CalcError::Invalid(format!(
                "not a field or column of {}",
                self.name
            )));
        };
        let slot = self.slot_at(g, key, epoch)?;
        Ok(
            g.value_at(self.member(slot, Target::Placeholder(*r)), epoch)?
                .clone(),
        )
    }

    /// Whether the collection had a row with this key at the end of update
    /// `epoch`.
    pub fn contains_at(&self, g: &Graph, key: &str, epoch: u64) -> Result<bool, CalcError> {
        self.check_graph(g)?;
        g.history_covering(epoch)?;
        match self.slot_at(g, key, epoch) {
            Ok(_) => Ok(true),
            Err(CalcError::Invalid(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The keys of the rows at the end of update `epoch`, in order.
    pub fn keys_at(&self, g: &Graph, epoch: u64) -> Result<Vec<String>, CalcError> {
        self.check_graph(g)?;
        g.history_covering(epoch)?;
        let mut now: Vec<Option<&str>> = vec![None; self.slots.len()];
        for (key, &slot) in &self.keys {
            now[slot] = Some(key);
        }
        let mut keys = Vec::new();
        for (slot, key) in now.into_iter().enumerate() {
            if self.present_at(g, slot, epoch)? {
                if let Some(key) = self.key_at(slot, epoch, key) {
                    keys.push(key.to_string());
                }
            }
        }
        keys.sort();
        Ok(keys)
    }

    /// Whether the row in `slot` was present at update `epoch` (false if the
    /// slot was made later).
    fn present_at(&self, g: &Graph, slot: usize, epoch: u64) -> Result<bool, CalcError> {
        match g.value_at(self.slots[slot].present, epoch) {
            Ok(v) => Ok(matches!(v, Value::Bool(true))),
            Err(CalcError::NotCreatedYet { .. }) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The key `slot` had at update `epoch`, given the key it has now: the
    /// key before its first change after that update, or the key it has now.
    fn key_at<'a>(&'a self, slot: usize, epoch: u64, now: Option<&'a str>) -> Option<&'a str> {
        match self
            .key_changes
            .get(&slot)
            .and_then(|changes| changes.iter().find(|c| c.from > epoch))
        {
            Some(change) => Some(&change.before),
            None => now,
        }
    }

    /// Records that `slot` is about to stop having the key `before`, from the
    /// next update. (Reusing a free slot needs no record: it is absent until
    /// then, and an absent slot's key never matters.)
    fn key_changed(&mut self, g: &Graph, slot: usize, before: String) {
        self.start_recording(g);
        let latest = g.epoch();
        let oldest = g.history_range().map_or(latest, |(oldest, _)| oldest);
        let changes = self.key_changes.entry(slot).or_default();
        // Changes no update in history can be before are not needed.
        let forgotten = changes.iter().take_while(|c| c.from <= oldest).count();
        changes.drain(..forgotten);
        let from = latest + 1;
        // Several changes before one update: what counts is the key before the first.
        if changes.last().is_none_or(|c| c.from != from) {
            changes.push(KeyChange { from, before });
        }
    }

    /// Before recording a change: while history is off, only changes since
    /// the last update can ever be asked about (if history starts before the
    /// next one), so older ones are dropped.
    fn start_recording(&mut self, g: &Graph) {
        if g.history_range().is_none() && self.recorded_at < g.epoch() {
            self.past_slots.clear();
            self.key_changes.clear();
        }
        self.recorded_at = g.epoch();
    }

    /// The slot that held `key` at update `epoch`: the one it holds now, or
    /// one it held before.
    fn slot_at(&self, g: &Graph, key: &str, epoch: u64) -> Result<usize, CalcError> {
        let now = self.keys.get(key).copied();
        let before = self.past_slots.get(key).map_or(&[][..], Vec::as_slice);
        for slot in now.into_iter().chain(before.iter().rev().copied()) {
            let current = (now == Some(slot)).then_some(key);
            if self.present_at(g, slot, epoch)? && self.key_at(slot, epoch, current) == Some(key) {
                return Ok(slot);
            }
        }
        Err(CalcError::Invalid(format!(
            "{} had no row {key:?} at update {epoch}",
            self.name
        )))
    }

    /// The type of a field or column given by its placeholder; `None` if it
    /// is not one of this collection's.
    pub fn part_type(&self, of: &Ir) -> Option<Type> {
        match of {
            Ir::Ref(r) if r.graph == self.template => {
                let i = r.node.0;
                if i & COLUMN != 0 {
                    self.columns.get((i & !COLUMN) as usize).map(|c| c.ty)
                } else {
                    self.fields.get(i as usize).map(|f| f.1)
                }
            }
            _ => None,
        }
    }

    /// Adds a grouped total by the field `by` (given by its placeholder):
    /// `op` of a field or column per group, or a count per group when `of`
    /// is `None` (only for [`AggregateOp::Count`]). Returns its index.
    pub fn add_grouping(
        &mut self,
        g: &mut Graph,
        name: &str,
        op: AggregateOp,
        by: &Ir,
        of: Option<&Ir>,
    ) -> Result<usize, CalcError> {
        let Ir::Ref(by) = by else {
            return Err(CalcError::Invalid(format!(
                "{}: group by one of its fields",
                self.name
            )));
        };
        let (target, value_type) = match (op, of) {
            (AggregateOp::Count, None) => (Target::Presence, Type::I64),
            (AggregateOp::Count, Some(_)) => {
                return Err(CalcError::Invalid(
                    "a count counts rows, not a field".into(),
                ));
            }
            (_, Some(part @ Ir::Ref(r))) => {
                let ty = self.part_type(part).ok_or_else(|| {
                    CalcError::Invalid(format!("{op} needs a field or column of {}", self.name))
                })?;
                (Target::Placeholder(*r), ty)
            }
            _ => {
                return Err(CalcError::Invalid(format!(
                    "{op} needs a field or column of {}",
                    self.name
                )));
            }
        };
        if !value_type.is_numeric() {
            return Err(CalcError::Invalid(format!(
                "{op} needs numbers, not {value_type}"
            )));
        }
        self.grouping(g, name, op, value_type, *by, target)
    }

    /// The total for one group of the grouping at `index`, if any row has had that key.
    pub fn group_node(&self, index: usize, key: &Value) -> Option<NodeId> {
        self.groupings
            .get(index)?
            .groups
            .get(&key_text(key))
            .copied()
    }

    /// The keys of the groups at `index` that have rows, sorted.
    pub fn group_keys_of(&self, index: usize) -> Vec<String> {
        self.group_keys(Groups::<(), ()> {
            template: self.template,
            index,
            _type: PhantomData,
        })
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
            Ir::List(items) => Ir::List(items.iter().map(re).collect()),
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
