//! Totals per group, where a row's group is the value of one of its nodes.
//!
//! Each group has an aggregate. A row counts in a group through a `bool`
//! node, `present && key == "that group"`, so a row that leaves a group
//! drops out of its total like any other change. The one thing an ordinary
//! update cannot do is put a row in a group it has never been in: that
//! needs a new membership node, and perhaps a new aggregate.
//!
//! A row's membership nodes stay while the row does: within one row's life
//! its key can come back to a group it was in, and a scenario plan, which
//! may vary the key, needs a membership node for every group the row can be
//! in. When the row is removed and its nodes are reused for another row,
//! though, its membership nodes are spare: the new row is put in its group
//! by taking one of them, rather than making a node, and the group it named
//! loses a member (the member is emptied, and the next row to join that
//! group takes its place). So a collection whose rows come and go needs
//! membership nodes for the rows it holds, not for every row and group there
//! has ever been. With history on, nothing is taken: an earlier update's
//! totals must still be explained by the members they had.
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
use crate::compile::{Program, compile};
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
    /// For each row, each group it has been in since it was added (or since
    /// its nodes were last reused), and the `bool` node saying whether it is
    /// in that group now.
    membership: Vec<Vec<(String, NodeId)>>,
    /// For each row, the membership nodes it had before it was removed:
    /// spare, to be taken when its nodes are reused (see the module's
    /// documentation).
    spare: Vec<Vec<(String, NodeId)>>,
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
            membership: Vec::new(),
            spare: Vec::new(),
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
        k.membership.push(Vec::new());
        k.spare.push(Vec::new());
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
        if k.membership[row].iter().any(|(group, _)| *group == text) {
            return Ok(()); // it has been in this group before, so it already counts there
        }
        // A spare membership node for this very group: it counts there
        // already.
        if let Some(at) = k.spare[row].iter().position(|(group, _)| *group == text) {
            let spare = k.spare[row].swap_remove(at);
            k.membership[row].push(spare);
            return Ok(());
        }
        if let Some((old, in_group)) = k.spare[row].pop() {
            return self.keyed_take_spare(grouping, row, &old, in_group, key, text);
        }
        let KeyedRow {
            present,
            key: key_node,
            ..
        } = k.rows[row];
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
        self.keyed[grouping].membership[row].push((text.clone(), in_group));
        self.keyed_join(grouping, row, in_group, text, total)
    }

    /// Adds a row's membership node to its group's total, making the group
    /// (named `total`) if it has none yet.
    fn keyed_join(
        &mut self,
        grouping: usize,
        row: usize,
        in_group: NodeId,
        text: String,
        total: String,
    ) -> Result<(), CalcError> {
        let k = &self.keyed[grouping];
        let value = k.rows[row].value.unwrap_or(in_group);
        let (op, value_type, height) = (k.op, k.value_type, k.height);
        match k.groups.get(&text) {
            Some(&group) => self.extend_aggregate(group, value, in_group)?,
            None => {
                let group = self.aggregate(&total, op, value_type, height, &[(value, in_group)])?;
                self.keyed[grouping].groups.insert(text, group);
            }
        }
        Ok(())
    }

    /// Puts a row whose nodes are being reused in the group `text` (its key
    /// `key`) by taking a membership node it had before it was removed, for
    /// the group `old`: the member it was in `old`'s total is emptied, the
    /// node now asks whether the row's key is `key`, and it joins that
    /// group's total. It reads the same nodes as before, so no link changes.
    fn keyed_take_spare(
        &mut self,
        grouping: usize,
        row: usize,
        old: &str,
        in_group: NodeId,
        key: &Value,
        text: String,
    ) -> Result<(), CalcError> {
        // A member still waiting to join is joined first, so that it can be
        // emptied.
        if !self.updating {
            self.join_pending_members(self.epoch + 1);
        }
        let hole = self.hole()?;
        let group = self.keyed[grouping].groups[old];
        let hot = self.hot[group.index()];
        if self.aggregates[hot.code as usize].vacate(in_group.0, hole.0) {
            match self.updating {
                true => self.queue[hot.height as usize].push(group),
                false => {
                    self.redefined.insert(group);
                }
            }
        }

        // present && key == the new group's key, compiled in place of what
        // it was: a comparison with a constant is always a general program,
        // as the node's was.
        let KeyedRow {
            present,
            key: key_node,
            ..
        } = self.keyed[grouping].rows[row];
        let ir = Ir::Binary(
            BinOp::And,
            Arc::new(self.reference(present)),
            Arc::new(Ir::Binary(
                BinOp::Eq,
                Arc::new(self.reference(key_node)),
                Arc::new(Ir::Const(key.clone())),
            )),
        );
        let types = &self.types;
        let program = compile(
            &ir,
            &|i| types[i],
            &mut self.float_ops,
            &mut self.float_consts,
        );
        let node = self.hot[in_group.index()];
        let (Program::General(program), Kind::General) = (program, node.kind) else {
            unreachable!("a membership node and its comparison are general programs");
        };
        self.general[node.code as usize] = program;
        self.reads[node.len as usize] = Box::default();
        match self.updating {
            true => self.queue[node.height as usize].push(in_group),
            false => {
                self.redefined.insert(in_group);
            }
        }

        // Its name says which group it is for now.
        let k = &self.keyed[grouping];
        let member = format!("{}[{row}].{}={text}", k.owner, k.name);
        let member = self.free_name(member);
        let hash = self.names.hash(&member);
        self.names.rename(in_group, &member, hash);

        let total = format!("{}[{text}]", self.keyed[grouping].name);
        let total = match self.updating {
            true => self.free_name(total),
            false => total,
        };
        self.keyed[grouping].membership[row].push((text.clone(), in_group));
        self.keyed_join(grouping, row, in_group, text, total)
    }

    /// The `bool` input that is always false, which stands in for a total's
    /// emptied members.
    fn hole(&mut self) -> Result<NodeId, CalcError> {
        if let Some(hole) = self.hole {
            return Ok(hole);
        }
        let name = self.free_name("(no member)".to_string());
        let hole = self.input_value(&name, Value::Bool(false))?;
        self.hole = Some(hole);
        Ok(hole)
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

    /// Takes a row out of its group's count (when its key is an error).
    /// Its membership nodes stay, and say it is in no group.
    pub(crate) fn keyed_leave(&mut self, grouping: usize, row: usize) {
        let k = &mut self.keyed[grouping];
        if let Some(old) = k.row_key[row].take() {
            *k.counts.entry(old).or_default() -= 1;
        }
    }

    /// Takes a removed row out of its group's count. Its membership nodes
    /// stay, and say it is absent; without history they are spare, for
    /// when its nodes are reused.
    pub(crate) fn keyed_remove(&mut self, grouping: usize, row: usize) {
        self.keyed_leave(grouping, row);
        if self.history.is_none() {
            let k = &mut self.keyed[grouping];
            let had = std::mem::take(&mut k.membership[row]);
            k.spare[row].extend(had);
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
