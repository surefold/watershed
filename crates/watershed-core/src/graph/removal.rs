//! Removing a node.
//!
//! A node that nothing uses can be removed: it stops being calculated, its
//! name is free for another node, and reads of it say there is no such
//! node. Its id is not handed out again, so a handle kept from before can
//! never come to mean a different node, and history still answers for the
//! updates before it went.
//!
//! The node's slot stays in the graph's arrays, holding its last value, as
//! an input that nothing reads and nothing can set. That keeps every other
//! part of the graph as it was: the cost is that removing nodes frees their
//! names and their work, not their memory.

use super::{Graph, Hot, Kind};
use crate::aggregate::Aggregate;
use crate::error::CalcError;
use crate::handle::{Handle, NodeId};

impl Graph {
    /// Removes a node that nothing uses.
    ///
    /// From now on the node is not calculated, [`find`](Self::find) does
    /// not find it, its name can be given to a new node, and reading it,
    /// setting it or using it in an expression is an
    /// [`UnknownNode`](CalcError::UnknownNode) error. With history on, it
    /// can still be read as of updates up to the latest one.
    ///
    /// It is refused while another node or a total uses this one (remove
    /// or redefine those first), and for a node a grouping of a collection
    /// is built on.
    ///
    /// Like [`redefine`](Self::redefine), the next update rebuilds every
    /// link in the graph, which takes time in proportion to the graph's
    /// size, so this suits edits rather than a stream of changes. Any number
    /// of removals before one update cost one rebuild.
    pub fn remove<T>(&mut self, node: impl Handle<T>) -> Result<(), CalcError> {
        let at = node.node_ref();
        self.check_ref(at)?;
        self.remove_node(at.node)
    }

    /// [`remove`](Self::remove) for a node given by id, for the untyped API.
    pub fn remove_node(&mut self, id: NodeId) -> Result<(), CalcError> {
        let i = id.index();
        if i >= self.hot.len() || self.is_removed(id) {
            return Err(CalcError::UnknownNode(id));
        }
        // Members waiting to join aggregates are uses too: join them now,
        // so the search below sees them.
        for (aggregate, value, present) in std::mem::take(&mut self.pending_members) {
            self.join_aggregate(aggregate, value, present, self.epoch + 1);
            self.redefined.insert(aggregate);
        }
        if let Some(user) = self.a_user_of(id) {
            return Err(CalcError::Invalid(format!(
                "{:?} cannot be removed: {:?} uses it",
                self.name(id),
                self.name(user)
            )));
        }
        if self.keyed_nodes.contains(&id.0) {
            return Err(CalcError::Invalid(format!(
                "{:?} cannot be removed: a collection groups its rows by it",
                self.name(id)
            )));
        }

        let old = self.hot[i];
        match old.kind {
            Kind::Aggregate => {
                // Let go of its members and its tree.
                let a = &mut self.aggregates[old.code as usize];
                *a = Aggregate::new(a.op, a.value_type);
            }
            Kind::General => self.reads[old.code as usize] = Box::default(),
            Kind::Float | Kind::Input => {}
        }
        // From here it is an input that nothing reads: never queued, never
        // recalculated, and consistent as it stands.
        self.hot[i] = Hot {
            code: 0,
            len: 0,
            height: 0,
            kind: Kind::Input,
        };
        self.pending.remove(&id);
        self.redefined.remove(&id);
        self.redefined_from.remove(&id);
        self.names.forget(id);
        self.removed.insert(id, self.epoch);
        // The nodes it used still list it as a user.
        self.relink_pending = true;
        self.levels = None;
        Ok(())
    }

    /// Whether the node has been removed.
    pub fn is_removed(&self, id: NodeId) -> bool {
        !self.removed.is_empty() && self.removed.contains_key(&id)
    }

    /// A node that uses this one, if any does.
    fn a_user_of(&self, id: NodeId) -> Option<NodeId> {
        self.dependents(id)
            .into_iter()
            .find(|&user| !self.is_removed(user) && self.dependencies(user).contains(&id))
    }
}
