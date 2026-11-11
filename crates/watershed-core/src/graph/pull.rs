//! Pull mode: only calculating what someone is watching.
//!
//! Normally an update recalculates every node that depends on what
//! changed. In pull mode it recalculates only the nodes that are *needed*:
//! the watched nodes and everything they use. A node that is not needed and
//! would have been recalculated is *parked* instead: it keeps its old value,
//! and nothing above it hears of the change.
//!
//! Every node that is not parked still holds exactly what its expression
//! gives for the values its dependencies hold now. So when a parked node
//! becomes needed, recalculating it at the next update, and passing that on
//! as usual, brings everything above it up to date.

use super::Graph;
use crate::error::CalcError;
use crate::handle::{Handle, NodeId};

/// Not needed, and up to date with what its dependencies hold.
pub(super) const IDLE: u8 = 0;
/// A watched node, or one a watched node uses: calculated as usual.
pub(super) const NEEDED: u8 = 1;
/// Not needed, and something it uses has changed since it was calculated.
pub(super) const PARKED: u8 = 2;

impl Graph {
    /// Watches a node: in pull mode, it and everything it uses are kept up
    /// to date. Watching is remembered while pull mode is off.
    pub fn watch<T>(&mut self, node: impl Handle<T>) -> Result<(), CalcError> {
        let at = node.node_ref();
        self.check_ref(at)?;
        self.watch_node(at.node)
    }

    /// [`watch`](Self::watch) for a node given by id.
    pub fn watch_node(&mut self, id: NodeId) -> Result<(), CalcError> {
        if id.index() >= self.hot.len() || self.is_removed(id) {
            return Err(CalcError::UnknownNode(id));
        }
        if self.watching.insert(id) && self.pull {
            self.need(id);
        }
        Ok(())
    }

    /// Stops watching a node. What only it needed is no longer calculated.
    pub fn unwatch<T>(&mut self, node: impl Handle<T>) -> Result<(), CalcError> {
        let at = node.node_ref();
        self.check_ref(at)?;
        self.unwatch_node(at.node);
        Ok(())
    }

    /// [`unwatch`](Self::unwatch) for a node given by id.
    pub fn unwatch_node(&mut self, id: NodeId) {
        if self.watching.remove(&id) && self.pull {
            self.work_out_what_is_needed();
        }
    }

    /// Turns pull mode on or off. While it is on, an update calculates only
    /// the watched nodes ([`watch`](Self::watch)) and what they use, and
    /// [`get`](Self::get) refuses to read any other calculated node, since
    /// its value may be out of date. Inputs can always be set and read.
    ///
    /// Turning it off, or watching more, takes effect at the next update,
    /// which brings up to date whatever had been left.
    ///
    /// The nodes a collection's groupings are built on are always
    /// calculated, so that rows can change group during an update.
    pub fn calculate_only_watched(&mut self, on: bool) {
        if on == self.pull {
            return;
        }
        self.pull = on;
        if on {
            self.needed = vec![IDLE; self.hot.len()];
            self.work_out_what_is_needed();
        } else {
            for (i, state) in self.needed.iter().enumerate() {
                if *state == PARKED {
                    self.wake.push(NodeId(i as u32));
                }
            }
            self.needed = Vec::new();
        }
    }

    /// Whether pull mode is on.
    pub fn calculates_only_watched(&self) -> bool {
        self.pull
    }

    /// Whether an update keeps this node up to date: always, unless pull
    /// mode is on and nothing watched uses it.
    pub fn is_calculated(&self, id: NodeId) -> bool {
        !self.pull || self.needed.get(id.index()) == Some(&NEEDED)
    }

    /// Whether the node is waiting to be recalculated when it is next needed.
    pub(super) fn is_parked(&self, id: NodeId) -> bool {
        self.pull && self.needed.get(id.index()) == Some(&PARKED)
    }

    /// Marks a node and everything it uses as needed. A parked one is
    /// recalculated at the next update.
    pub(super) fn need(&mut self, id: NodeId) {
        let mut stack = vec![id];
        while let Some(node) = stack.pop() {
            let state = &mut self.needed[node.index()];
            if *state == NEEDED {
                continue;
            }
            if *state == PARKED {
                self.wake.push(node);
            }
            *state = NEEDED;
            stack.extend(self.dependencies(node));
        }
    }

    /// Works out afresh which nodes are needed, after something stopped
    /// being watched or used (which can only be done from the top down).
    pub(super) fn work_out_what_is_needed(&mut self) {
        for state in &mut self.needed {
            if *state == NEEDED {
                *state = IDLE;
            }
        }
        let mut watched: Vec<NodeId> = self.watching.iter().copied().collect();
        watched.extend(self.keyed_nodes.iter().map(|&n| NodeId(n)));
        watched.retain(|&id| !self.is_removed(id));
        for id in watched {
            self.need(id);
        }
    }

    /// Takes the nodes that are not needed out of a level about to be
    /// recalculated, and parks them.
    pub(super) fn park_what_is_not_needed(&mut self, level: &mut Vec<NodeId>) {
        let needed = &mut self.needed;
        level.retain(|id| match &mut needed[id.index()] {
            state if *state == NEEDED => true,
            state => {
                *state = PARKED;
                false
            }
        });
    }
}
