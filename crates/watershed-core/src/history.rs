//! Earlier values of a graph's nodes.
//!
//! The current value of every node stays where it always was. When an update
//! overwrites a value, the old one moves here, so what history costs follows
//! what updates change: a node that never changes costs nothing but its slot
//! in `head`, and a value that comes out the same is never recorded.
//!
//! A node's old values form a chain, newest first, through `Version::prev`.
//! Versions are appended in update order, so forgetting the oldest updates is
//! popping from the front.

use std::collections::VecDeque;

use crate::value::Value;

/// Marks the end of a node's chain.
const NONE: u64 = u64::MAX;

/// A value a node used to have.
struct Version {
    value: Value,
    /// The update that gave the node this value.
    since: u64,
    /// The sequence number of the node's next older version.
    prev: u64,
}

pub(crate) struct History {
    versions: VecDeque<Version>,
    /// The sequence number of `versions[0]`. Sequence numbers are never
    /// reused, so links stay valid as the front is forgotten.
    base: u64,
    /// Each node's newest version, by node id. Shorter than the graph until
    /// the later nodes first change.
    head: Vec<u64>,
    /// For each update that recorded versions: the update, and the sequence
    /// number just past them.
    update_ends: VecDeque<(u64, u64)>,
    /// How many nodes the graph had, from each update at which that changed.
    nodes_at: VecDeque<(u64, u32)>,
    /// The earliest update that can still be read.
    oldest: u64,
    /// How many updates before the latest stay readable.
    keep: u64,
}

impl History {
    pub(crate) fn new(epoch: u64, nodes: usize, keep: u64) -> History {
        History {
            versions: VecDeque::new(),
            base: 0,
            head: Vec::new(),
            update_ends: VecDeque::new(),
            nodes_at: VecDeque::from([(epoch, node_count(nodes))]),
            oldest: epoch,
            keep,
        }
    }

    /// Records the value a node is losing, and the update that gave it.
    #[inline]
    pub(crate) fn record(&mut self, node: usize, value: Value, since: u64) {
        if node >= self.head.len() {
            self.head.resize(node + 1, NONE);
        }
        let sequence = self.base + self.versions.len() as u64;
        self.versions.push_back(Version {
            value,
            since,
            prev: self.head[node],
        });
        self.head[node] = sequence;
    }

    /// Closes an update: notes what it recorded and forgets what has expired.
    pub(crate) fn finish_update(&mut self, epoch: u64, nodes: usize) {
        let end = self.base + self.versions.len() as u64;
        let last_end = self.update_ends.back().map_or(self.base, |&(_, e)| e);
        if end != last_end {
            self.update_ends.push_back((epoch, end));
        }
        let nodes = node_count(nodes);
        if self.nodes_at.back().is_none_or(|&(_, n)| n != nodes) {
            self.nodes_at.push_back((epoch, nodes));
        }
        self.forget_expired(epoch);
    }

    pub(crate) fn set_keep(&mut self, epoch: u64, keep: u64) {
        self.keep = keep;
        self.forget_expired(epoch);
    }

    fn forget_expired(&mut self, epoch: u64) {
        self.oldest = self.oldest.max(epoch.saturating_sub(self.keep));
        // A version recorded by update `u` was the value until `u - 1`, so it
        // is needed only while `u - 1` is readable.
        let mut cut = self.base;
        while let Some(&(update, end)) = self.update_ends.front() {
            if update > self.oldest {
                break;
            }
            cut = end;
            self.update_ends.pop_front();
        }
        self.versions.drain(..(cut - self.base) as usize);
        self.base = cut;
        // Keep the last entry at or before `oldest`: it answers for `oldest`.
        while self.nodes_at.len() >= 2 && self.nodes_at[1].0 <= self.oldest {
            self.nodes_at.pop_front();
        }
    }

    pub(crate) fn oldest(&self) -> u64 {
        self.oldest
    }

    pub(crate) fn versions(&self) -> usize {
        self.versions.len()
    }

    /// Whether node `node` existed when update `epoch` finished. The caller
    /// has checked `epoch` is readable.
    pub(crate) fn existed(&self, node: usize, epoch: u64) -> bool {
        let after = self.nodes_at.partition_point(|&(e, _)| e <= epoch);
        after > 0 && node < self.nodes_at[after - 1].1 as usize
    }

    /// The value a node had at update `epoch`. The caller has checked that
    /// the epoch is readable, that the node existed then, and that its value
    /// has changed since, so the chain must reach that far back.
    pub(crate) fn value(&self, node: usize, epoch: u64) -> &Value {
        let mut at = self.head[node];
        loop {
            let version = &self.versions[(at - self.base) as usize];
            if version.since <= epoch {
                return &version.value;
            }
            at = version.prev;
        }
    }
}

fn node_count(nodes: usize) -> u32 {
    u32::try_from(nodes).expect("a graph holds at most u32::MAX nodes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_takes_32_bytes() {
        assert_eq!(std::mem::size_of::<Version>(), 32);
    }
}
