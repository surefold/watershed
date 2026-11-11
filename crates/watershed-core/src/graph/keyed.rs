//! Totals per group, where a row's group is the value of one of its nodes.
//!
//! Each group has an aggregate. A row counts in a group through a `bool`
//! node, `present && key == "that group"`, so a row that leaves a group
//! drops out of its total like any other change. The one thing an ordinary
//! update cannot do is put a row in a group it has never been in: that
//! needs a new membership node, and perhaps a new aggregate.
//!
//! When the key is an input, whoever sets it knows the new group and calls
//! [`Graph::keyed_follow`] at the same time. When the key is calculated, it
//! is only known during the update, so the graph watches those key nodes:
//! as soon as the level a changed key is on has been recalculated, the row
//! joins its new group. The membership node and the group's total sit above
//! the key, so they have not been reached yet, and the same update counts
//! the row in its new group.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Graph, Kind};
use crate::aggregate::AggregateOp;
use crate::error::CalcError;
use crate::handle::NodeId;
use crate::ir::{BinOp, Ir};
use crate::value::{Type, Value};

/// One row's nodes, as a grouping needs them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KeyedRow {
    pub(crate) present: NodeId,
    /// The node whose value names the row's group.
    pub(crate) key: NodeId,
    /// The node the group adds up; `None` to count rows.
    pub(crate) value: Option<NodeId>,
}

/// One grouping: its groups, and which group each row is in.
#[derive(Debug)]
pub(crate) struct Keyed {
    /// The collection's name, for naming membership nodes.
    owner: String,
    name: String,
    op: AggregateOp,
    value_type: Type,
    /// The height the group totals sit above.
    height: usize,
    rows: Vec<KeyedRow>,
    /// Each group's total, by the group's key.
    groups: HashMap<String, NodeId>,
    /// For each row and each group it has been in, the `bool` node saying
    /// whether it is in that group now.
    membership: HashMap<(u32, String), NodeId>,
    /// Each row's group (`None` if it is absent, or its key is an error).
    row_key: Vec<Option<String>>,
    /// How many rows each group has.
    counts: HashMap<String, usize>,
}

/// A group's key as text, the same for a key however it is given.
pub(crate) fn key_text(value: &Value) -> String {
    match value {
        Value::Str(s) => s.to_string(),
        other => other.to_string(),
    }
}

impl Graph {
    /// Adds a grouping with no rows. `height` is the height its totals sit
    /// above: at least that of every value they add up, and of every
    /// membership node (one above the key).
    pub(crate) fn keyed_groups(
        &mut self,
        owner: &str,
        name: &str,
        op: AggregateOp,
        value_type: Type,
        height: usize,
    ) -> usize {
        self.keyed.push(Keyed {
            owner: owner.to_string(),
            name: name.to_string(),
            op,
            value_type,
            height,
            rows: Vec::new(),
            groups: HashMap::new(),
            membership: HashMap::new(),
            row_key: Vec::new(),
            counts: HashMap::new(),
        });
        self.keyed.len() - 1
    }

    /// Adds the next row to a grouping; it is in no group yet. If the key
    /// is calculated, the graph follows it from now on.
    pub(crate) fn keyed_row(&mut self, grouping: usize, row: KeyedRow) {
        let k = &mut self.keyed[grouping];
        let index = u32::try_from(k.rows.len()).expect("too many rows");
        k.rows.push(row);
        k.row_key.push(None);
        let nodes = [row.present, row.key].into_iter().chain(row.value);
        for node in nodes {
            self.keyed_nodes.insert(node.0);
            // A row can change group during an update, so what a grouping
            // is built on is always calculated.
            if self.pull {
                self.need(node);
            }
        }
        if self.hot[row.key.index()].kind != Kind::Input {
            self.watched
                .entry(row.key.0)
                .or_default()
                .push((grouping as u32, index));
        }
    }

    /// Puts a row in the group its key names, given the key's value: counts
    /// it there, and makes sure it has a membership node in that group (and
    /// that the group exists). A key that is an error names no group.
    pub(crate) fn keyed_follow(
        &mut self,
        grouping: usize,
        row: usize,
        key: &Value,
    ) -> Result<(), CalcError> {
        if key.type_of().is_none() {
            self.keyed_leave(grouping, row);
            return Ok(());
        }
        let text = key_text(key);
        let k = &mut self.keyed[grouping];
        if k.row_key[row].as_ref() == Some(&text) {
            return Ok(());
        }
        if let Some(old) = k.row_key[row].replace(text.clone()) {
            *k.counts.entry(old).or_default() -= 1;
        }
        *k.counts.entry(text.clone()).or_default() += 1;
        let pair = (row as u32, text);
        if k.membership.contains_key(&pair) {
            return Ok(()); // it has been in this group before, so it already counts there
        }
        let text = pair.1.clone();
        let KeyedRow {
            present,
            key: key_node,
            value,
        } = k.rows[row];
        let (op, value_type, height) = (k.op, k.value_type, k.height);
        let member = format!("{}[{row}].{}={text}", k.owner, k.name);
        let total = format!("{}[{text}]", k.name);
        // During an update there is nobody to hand a name clash back to, so
        // a taken name gets a number after it instead.
        let (member, total) = if self.updating {
            (self.free_name(member), self.free_name(total))
        } else {
            (member, total)
        };
        // present && key == this group's key
        let in_group = Ir::Binary(
            BinOp::And,
            Arc::new(self.reference(present)),
            Arc::new(Ir::Binary(
                BinOp::Eq,
                Arc::new(self.reference(key_node)),
                Arc::new(Ir::Const(key.clone())),
            )),
        );
        let in_group = self.define_ir(&member, in_group)?;
        let value = value.unwrap_or(in_group);
        self.keyed[grouping].membership.insert(pair, in_group);
        match self.keyed[grouping].groups.get(&text) {
            Some(&group) => self.extend_aggregate(group, value, in_group)?,
            None => {
                let group = self.aggregate(&total, op, value_type, height, &[(value, in_group)])?;
                self.keyed[grouping].groups.insert(text, group);
            }
        }
        Ok(())
    }

    /// `name`, or `name#2`, `name#3`... if nodes already have those names.
    fn free_name(&self, name: String) -> String {
        if self.find(&name).is_none() {
            return name;
        }
        (2..)
            .map(|n| format!("{name}#{n}"))
            .find(|candidate| self.find(candidate).is_none())
            .expect("some number is free")
    }

    /// Takes a row out of its group's count (when the row is removed). Its
    /// membership nodes stay, and say it is absent.
    pub(crate) fn keyed_leave(&mut self, grouping: usize, row: usize) {
        let k = &mut self.keyed[grouping];
        if let Some(old) = k.row_key[row].take() {
            *k.counts.entry(old).or_default() -= 1;
        }
    }

    /// One group's total, or `None` if no row has ever had that key.
    pub(crate) fn keyed_group(&self, grouping: usize, key: &str) -> Option<NodeId> {
        self.keyed.get(grouping)?.groups.get(key).copied()
    }

    /// The keys of the groups that have rows, sorted.
    pub(crate) fn keyed_keys(&self, grouping: usize) -> Vec<String> {
        let Some(k) = self.keyed.get(grouping) else {
            return Vec::new();
        };
        let mut keys: Vec<String> = k
            .counts
            .iter()
            .filter(|&(_, &n)| n > 0)
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort();
        keys
    }

    /// Moves every row whose calculated key changed on the level just
    /// recalculated into its new group. Called during an update.
    pub(super) fn follow_changed_keys(&mut self) {
        for node in std::mem::take(&mut self.rekey) {
            let key = self.values[node as usize].clone();
            let rows = self.watched.get(&node).cloned().unwrap_or_default();
            for (grouping, row) in rows {
                // A row that has been removed keeps its nodes, which can
                // still change; it is in no group until it is reused.
                let present = self.keyed[grouping as usize].rows[row as usize].present;
                if !matches!(self.values[present.index()], Value::Bool(true)) {
                    continue;
                }
                self.keyed_follow(grouping as usize, row as usize, &key)
                    .expect("a membership node and its group's total can always be added");
            }
        }
    }
}
