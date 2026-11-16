//! Totals per group, where a row's group is the value of one of its nodes.
//!
//! Each group has an aggregate. A row counts in a group through a `bool`
//! node, `present && key == "that group"`, so a row that leaves a group
//! drops out of its total like any other change. The one thing an ordinary
//! update cannot do is put a row in a group it has never been in: that
//! needs a new membership node, and perhaps a new aggregate.
//!
//! A collection's groupings by the same key (a sum and a count of each
//! group, say) share their membership nodes: whether a row is in a group
//! does not depend on what the grouping adds up. Each grouping has its own
//! totals, and keeps track of which of them each shared node is a member of.
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

/// Marks no group, in a `u32` that otherwise numbers one (see `Pool`).
const NO_GROUP: u32 = u32::MAX;

/// A short list for each row of a table, of groups (by number) and
/// membership nodes. Nearly every row has one entry, so that is held in
/// place, and only the rows with more have a list of their own.
#[derive(Debug, Default)]
struct PerRow {
    first: Vec<(u32, NodeId)>,
    more: HashMap<u32, Vec<(u32, NodeId)>>,
}

impl PerRow {
    fn add_row(&mut self) {
        self.first.push((NO_GROUP, NodeId(0)));
    }

    fn entries(&self, row: usize) -> impl Iterator<Item = (u32, NodeId)> + '_ {
        let first = Some(self.first[row]).filter(|&(group, _)| group != NO_GROUP);
        let more = self.more.get(&(row as u32)).into_iter().flatten().copied();
        first.into_iter().chain(more)
    }

    fn push(&mut self, row: usize, entry: (u32, NodeId)) {
        if self.first[row].0 == NO_GROUP {
            self.first[row] = entry;
        } else {
            self.more.entry(row as u32).or_default().push(entry);
        }
    }

    /// The entry that matches, taken out.
    fn take(
        &mut self,
        row: usize,
        matches: impl Fn((u32, NodeId)) -> bool,
    ) -> Option<(u32, NodeId)> {
        if self.first[row].0 != NO_GROUP && matches(self.first[row]) {
            let taken = self.first[row];
            self.first[row] = match self.more.get_mut(&(row as u32)) {
                Some(more) => {
                    let next = more.pop().expect("a list of more is never empty");
                    if more.is_empty() {
                        self.more.remove(&(row as u32));
                    }
                    next
                }
                None => (NO_GROUP, NodeId(0)),
            };
            return Some(taken);
        }
        let more = self.more.get_mut(&(row as u32))?;
        let at = more.iter().position(|&entry| matches(entry))?;
        let taken = more.swap_remove(at);
        if more.is_empty() {
            self.more.remove(&(row as u32));
        }
        Some(taken)
    }

    /// Every entry, taken out.
    fn take_all(&mut self, row: usize) -> Vec<(u32, NodeId)> {
        let all = self.entries(row).collect();
        self.first[row] = (NO_GROUP, NodeId(0));
        self.more.remove(&(row as u32));
        all
    }
}

/// The membership nodes of the groupings of a collection that group by one
/// field or column: a row's membership node for a group says whether the
/// row's key names that group, whatever the grouping adds up, so every
/// grouping by that key uses the same nodes. Groups are numbered here, in
/// the order their keys are first met, and the groupings use the numbers.
#[derive(Debug)]
pub(crate) struct Pool {
    /// The collection's name and the first grouping's, for naming nodes.
    owner: String,
    name: String,
    /// Each group's key, by number, and the other way.
    keys: Vec<String>,
    numbers: HashMap<String, u32>,
    /// Each row's presence node and key node.
    rows: Vec<(NodeId, NodeId)>,
    /// Each row's groups since it was added (or since its nodes were last
    /// reused), and the `bool` node saying whether it is in that group now.
    current: PerRow,
    /// The membership nodes it had before it was removed: spare, to be
    /// taken when its nodes are reused (see the module's documentation).
    spare: PerRow,
}

impl Pool {
    fn number(&mut self, key: &str) -> u32 {
        if let Some(&n) = self.numbers.get(key) {
            return n;
        }
        let n = u32::try_from(self.keys.len()).expect("fewer than 2^32 groups");
        self.keys.push(key.to_string());
        self.numbers.insert(key.to_string(), n);
        n
    }
}

/// One grouping: its groups, and which group each row is in.
#[derive(Debug)]
pub(crate) struct Keyed {
    /// Where its membership nodes are, shared with the collection's other
    /// groupings by the same key.
    pool: usize,
    name: String,
    op: AggregateOp,
    value_type: Type,
    /// The height the group totals sit above.
    height: usize,
    /// Each row's value node, the one its groups add up; `None` to count.
    values: Vec<Option<NodeId>>,
    /// Each group's total, by the group's number (`None` before a row has
    /// been in it).
    groups: Vec<Option<NodeId>>,
    /// For each row, its membership nodes that are members of one of this
    /// grouping's totals, and which group's. A node taken for another group
    /// is still listed under the old one until this grouping moves it.
    in_groups: PerRow,
    /// Each row's group (`NO_GROUP` if it is absent, or its key is an
    /// error).
    row_key: Vec<u32>,
    /// How many rows each group has, by number.
    counts: Vec<u32>,
}

/// A group's key as text, the same for a key however it is given.
pub(crate) fn key_text(value: &Value) -> String {
    match value {
        Value::Str(s) => s.to_string(),
        other => other.to_string(),
    }
}

/// `present && key == value`: whether a row is in the group `value` names.
fn in_group(g: &Graph, present: NodeId, key: NodeId, value: &Value) -> Ir {
    Ir::Binary(
        BinOp::And,
        Arc::new(g.reference(present)),
        Arc::new(Ir::Binary(
            BinOp::Eq,
            Arc::new(g.reference(key)),
            Arc::new(Ir::Const(value.clone())),
        )),
    )
}

impl Graph {
    /// Adds a grouping with no rows. `height` is the height its totals sit
    /// above: at least that of every value they add up, and of every
    /// membership node (one above the key). With `share`, an earlier
    /// grouping of the same collection by the same key, it uses that
    /// grouping's membership nodes.
    pub(crate) fn keyed_groups(
        &mut self,
        owner: &str,
        name: &str,
        op: AggregateOp,
        value_type: Type,
        height: usize,
        share: Option<usize>,
    ) -> usize {
        let pool = match share {
            Some(other) => self.keyed[other].pool,
            None => {
                self.pools.push(Pool {
                    owner: owner.to_string(),
                    name: name.to_string(),
                    keys: Vec::new(),
                    numbers: HashMap::new(),
                    rows: Vec::new(),
                    current: PerRow::default(),
                    spare: PerRow::default(),
                });
                self.pools.len() - 1
            }
        };
        self.keyed.push(Keyed {
            pool,
            name: name.to_string(),
            op,
            value_type,
            height,
            values: Vec::new(),
            groups: Vec::new(),
            in_groups: PerRow::default(),
            row_key: Vec::new(),
            counts: Vec::new(),
        });
        self.keyed.len() - 1
    }

    /// Adds the next row to a grouping; it is in no group yet. If the key
    /// is calculated, the graph follows it from now on.
    pub(crate) fn keyed_row(&mut self, grouping: usize, row: KeyedRow) {
        let k = &mut self.keyed[grouping];
        let index = u32::try_from(k.values.len()).expect("too many rows");
        k.values.push(row.value);
        k.row_key.push(NO_GROUP);
        k.in_groups.add_row();
        let pool = &mut self.pools[k.pool];
        if pool.rows.len() == index as usize {
            pool.rows.push((row.present, row.key));
            pool.current.add_row();
            pool.spare.add_row();
        }
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
    /// it there, and makes sure its membership node for that group is a
    /// member of the group's total (and that the group exists). A key that
    /// is an error names no group.
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
        let pool = self.keyed[grouping].pool;
        let group = self.pools[pool].number(&key_text(key));
        let k = &mut self.keyed[grouping];
        if k.row_key[row] == group {
            return Ok(());
        }
        if k.counts.len() <= group as usize {
            k.counts.resize(group as usize + 1, 0);
            k.groups.resize(group as usize + 1, None);
        }
        let old = std::mem::replace(&mut k.row_key[row], group);
        if old != NO_GROUP {
            k.counts[old as usize] -= 1;
        }
        k.counts[group as usize] += 1;
        let node = self.pool_member(pool, row, key, group)?;
        let k = &mut self.keyed[grouping];
        match k.in_groups.take(row, |(_, n)| n == node) {
            // It has been in this group before, so it already counts there.
            Some((was, _)) if was == group => {
                k.in_groups.push(row, (group, node));
                return Ok(());
            }
            // The node was taken for this group: it leaves the old one here.
            Some((was, _)) => {
                k.in_groups.push(row, (group, node));
                let total = k.groups[was as usize].expect("a group a row was in has a total");
                self.keyed_vacate(total, node)?;
            }
            None => k.in_groups.push(row, (group, node)),
        }
        self.keyed_join(grouping, row, node, group)
    }

    /// A row's membership node for group number `group` (its key `key`):
    /// the one it has, a spare one for that group, a spare one for another
    /// group taken for this one, or a new one.
    fn pool_member(
        &mut self,
        pool: usize,
        row: usize,
        key: &Value,
        group: u32,
    ) -> Result<NodeId, CalcError> {
        let p = &mut self.pools[pool];
        if let Some((_, node)) = p.current.entries(row).find(|&(g, _)| g == group) {
            return Ok(node);
        }
        if let Some(spare) = p.spare.take(row, |(g, _)| g == group) {
            p.current.push(row, spare);
            return Ok(spare.1);
        }
        let (present, key_node) = p.rows[row];
        let name = format!("{}[{row}].{}={}", p.owner, p.name, p.keys[group as usize]);
        if let Some((_, node)) = p.spare.take(row, |_| true) {
            // Taken for this group: it asks whether the key is this one now,
            // compiled in place of what it was (a comparison with a constant
            // is always a general program, as the node's was). It reads the
            // same nodes as before, so no link changes. The groupings move
            // it between their totals as they follow the row.
            let ir = in_group(self, present, key_node, key);
            let types = &self.types;
            let program = compile(
                &ir,
                &|i| types[i],
                &mut self.float_ops,
                &mut self.float_consts,
            );
            let hot = self.hot[node.index()];
            let (Program::General(program), Kind::General) = (program, hot.kind) else {
                unreachable!("a membership node and its comparison are general programs");
            };
            self.general[hot.code as usize] = program;
            self.reads[hot.len as usize] = Box::default();
            match self.updating {
                true => self.queue[hot.height as usize].push(node),
                false => {
                    self.redefined.insert(node);
                }
            }
            // Its name says which group it is for now.
            let name = self.free_name(name);
            let hash = self.names.hash(&name);
            self.names.rename(node, &name, hash);
            self.pools[pool].current.push(row, (group, node));
            return Ok(node);
        }
        // During an update there is nobody to hand a name clash back to, so
        // a taken name gets a number after it instead.
        let name = match self.updating {
            true => self.free_name(name),
            false => name,
        };
        let node = self.define_ir(&name, in_group(self, present, key_node, key))?;
        self.pools[pool].current.push(row, (group, node));
        Ok(node)
    }

    /// Adds a row's membership node to its group's total, making the group
    /// if it has none yet.
    fn keyed_join(
        &mut self,
        grouping: usize,
        row: usize,
        node: NodeId,
        group: u32,
    ) -> Result<(), CalcError> {
        let k = &self.keyed[grouping];
        let value = k.values[row].unwrap_or(node);
        let (op, value_type, height) = (k.op, k.value_type, k.height);
        match k.groups[group as usize] {
            Some(total) => self.extend_aggregate(total, value, node)?,
            None => {
                let key = &self.pools[k.pool].keys[group as usize];
                let name = format!("{}[{key}]", k.name);
                let name = match self.updating {
                    true => self.free_name(name),
                    false => name,
                };
                let total = self.aggregate(&name, op, value_type, height, &[(value, node)])?;
                self.keyed[grouping].groups[group as usize] = Some(total);
            }
        }
        Ok(())
    }

    /// Empties the member of a group's total whose presence is the
    /// membership node `node`, which has been taken for another group.
    fn keyed_vacate(&mut self, total: NodeId, node: NodeId) -> Result<(), CalcError> {
        // A member still waiting to join is joined first, so that it can be
        // emptied.
        if !self.updating {
            self.join_pending_members(self.epoch + 1);
        }
        let hole = self.hole()?;
        let hot = self.hot[total.index()];
        if self.aggregates[hot.code as usize].vacate(node.0, hole.0) {
            match self.updating {
                true => self.queue[hot.height as usize].push(total),
                false => {
                    self.redefined.insert(total);
                }
            }
        }
        Ok(())
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
        let old = std::mem::replace(&mut k.row_key[row], NO_GROUP);
        if old != NO_GROUP {
            k.counts[old as usize] -= 1;
        }
    }

    /// Takes a removed row out of its group's count. Its membership nodes
    /// stay, and say it is absent; without history they are spare, for
    /// when its nodes are reused.
    pub(crate) fn keyed_remove(&mut self, grouping: usize, row: usize) {
        self.keyed_leave(grouping, row);
        if self.history.is_none() {
            // Every grouping by this key does this; the first moves them.
            let p = &mut self.pools[self.keyed[grouping].pool];
            for entry in p.current.take_all(row) {
                p.spare.push(row, entry);
            }
        }
    }

    /// One group's total, or `None` if no row has ever had that key.
    pub(crate) fn keyed_group(&self, grouping: usize, key: &str) -> Option<NodeId> {
        let k = self.keyed.get(grouping)?;
        let group = *self.pools[k.pool].numbers.get(key)?;
        k.groups.get(group as usize).copied().flatten()
    }

    /// The keys of the groups that have rows, sorted.
    pub(crate) fn keyed_keys(&self, grouping: usize) -> Vec<String> {
        let Some(k) = self.keyed.get(grouping) else {
            return Vec::new();
        };
        let names = &self.pools[k.pool].keys;
        let mut keys: Vec<String> = k
            .counts
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n > 0)
            .map(|(group, _)| names[group].clone())
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
                let pool = self.keyed[grouping as usize].pool;
                let present = self.pools[pool].rows[row as usize].0;
                if !matches!(self.values[present.index()], Value::Bool(true)) {
                    continue;
                }
                self.keyed_follow(grouping as usize, row as usize, &key)
                    .expect("a membership node and its group's total can always be added");
            }
        }
    }
}
