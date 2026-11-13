//! Puts a plan's nodes into clusters that a GPU thread works out in one go.
//!
//! Worked out a node at a time, every node's value is written to the GPU's
//! memory and read back by the nodes that use it, and for a big graph moving
//! that memory is nearly all of the time. A cluster is a set of nodes one
//! thread works out one after another, keeping in registers whatever only
//! the cluster reads. Only what other clusters read, or an output, goes to
//! memory.
//!
//! Clusters are made a band of levels at a time. Within a band, nodes that
//! read each other join one cluster, up to a size; a node that would make
//! its cluster too big starts its own, and the clusters it reads are then
//! *sealed*: nothing more joins them. So a cluster reads only earlier bands
//! and sealed clusters of its own band, and a sealed cluster never grows to
//! read something later, which is what keeps the clusters free of circles.
//!
//! Clusters whose nodes do the same things in the same order, reading and
//! writing in the same places within the cluster, have one *shape*, and one
//! kernel is compiled for it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::plan::{CODE, CODE_BITS, CONST, Group, LOAD, LOAD_SHARED};

/// In a cluster's shape, a load of a node of the same cluster: the number
/// beside it says which, counting the cluster's nodes from 0.
pub(crate) const LOCAL: u32 = 25;

/// A plan as clusters.
#[derive(Debug, Clone, Default)]
#[cfg_attr(not(feature = "cuda"), allow(dead_code))]
pub(crate) struct Fused {
    /// How many slots a scenario needs: the varied inputs, and what clusters
    /// pass to each other.
    pub(crate) slots: usize,
    /// For each shape, its nodes one after another: how many instructions,
    /// whether its value goes to memory (1 or 0), then the instructions.
    pub(crate) shapes: Vec<Vec<u32>>,
    /// The clusters as runs of one shape, in the order they are worked out.
    pub(crate) groups: Vec<Group>,
    /// Per cluster: where its operands start.
    pub(crate) operand_at: Vec<u32>,
    /// Each cluster's operands: what its loads from outside the cluster
    /// read (a slot, a constant or a shared value), in order, then the slots
    /// its values go to.
    pub(crate) operands: Vec<u32>,
    /// The most clusters in one group.
    pub(crate) widest: usize,
    /// For each node (in the plan's order), the slot it is written to, if it
    /// is written at all.
    pub(crate) slot_of: Vec<Option<u32>>,
}

/// How far clustering goes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// How many levels deep a band is.
    pub(crate) band: u32,
    /// The most nodes in a cluster.
    pub(crate) most: usize,
    /// The most shapes of cluster of more than one node.
    pub(crate) shapes_most: usize,
}

struct Cluster {
    members: Vec<u32>,
    sealed: bool,
}

/// Makes clusters of a plan's nodes. `level` and `ops` give each node's
/// level and instructions, in the plan's order; a load's number is a slot of
/// a varied input if below `vary`, and otherwise `node_of` says which node
/// it reads. `kept` are the nodes that are outputs. `limits` say how far to go.
pub(crate) fn fuse<'o>(
    vary: usize,
    level: &[u32],
    ops: &dyn Fn(usize) -> &'o [u32],
    node_of: &dyn Fn(u32) -> Option<usize>,
    kept: &[usize],
    limits: Limits,
) -> Option<Fused> {
    let Limits {
        band,
        most,
        shapes_most,
    } = limits;
    let nodes = level.len();
    let reads = |n: usize| {
        ops(n)
            .iter()
            .filter(|&&op| op & CODE == LOAD)
            .filter_map(|&op| node_of(op >> CODE_BITS))
    };

    // Clusters, a band at a time.
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut cluster_of = vec![0u32; nodes];
    let mut near: Vec<u32> = Vec::new();
    for n in 0..nodes {
        let this_band = level[n] / band;
        near.clear();
        for r in reads(n) {
            if level[r] / band == this_band && !near.contains(&cluster_of[r]) {
                near.push(cluster_of[r]);
            }
        }
        let open = |c: &u32| !clusters[*c as usize].sealed;
        let size: usize = near
            .iter()
            .filter(|c| open(c))
            .map(|&c| clusters[c as usize].members.len())
            .sum();
        if near.iter().any(open) && size < most {
            // Join them all into the biggest.
            let into = *near
                .iter()
                .filter(|c| open(c))
                .max_by_key(|&&c| clusters[c as usize].members.len())
                .expect("an open cluster");
            for &c in &near {
                if c != into && !clusters[c as usize].sealed {
                    let moved = std::mem::take(&mut clusters[c as usize].members);
                    for &m in &moved {
                        cluster_of[m as usize] = into;
                    }
                    clusters[into as usize].members.extend(moved);
                }
            }
            clusters[into as usize].members.push(n as u32);
            cluster_of[n] = into;
        } else {
            for &c in &near {
                clusters[c as usize].sealed = true;
            }
            cluster_of[n] = u32::try_from(clusters.len()).ok()?;
            clusters.push(Cluster {
                members: vec![n as u32],
                sealed: false,
            });
        }
    }

    // Twice at most: the second time with the clusters of the rarer shapes
    // taken apart into nodes on their own, so that no more than `shapes_most`
    // shapes of cluster are left to compile kernels for. A node on its own
    // has one of the plan's few shapes of node.
    let mut again = false;
    let (mut order, stage, last_read, shapes, shape_of) = loop {
        // The clusters that have nodes, each with its nodes in the plan's order
        // (so a node comes after what it reads), in order of their last node: a
        // cluster comes after every cluster it reads.
        let mut order: Vec<u32> = (0..clusters.len() as u32)
            .filter(|&c| !clusters[c as usize].members.is_empty())
            .collect();
        for &c in &order {
            clusters[c as usize].members.sort_unstable();
        }
        order.sort_unstable_by_key(|&c| *clusters[c as usize].members.last().expect("a node"));

        // The stage of each cluster: one more than the highest it reads. And
        // which nodes are read from outside their cluster, and in which stage
        // last.
        let mut stage = vec![0u32; clusters.len()];
        let mut last_read: Vec<Option<u32>> = vec![None; nodes];
        for &c in &order {
            let mut highest = None;
            for &m in &clusters[c as usize].members {
                for r in reads(m as usize) {
                    let from = cluster_of[r];
                    if from != c {
                        highest = highest.max(Some(stage[from as usize]));
                    }
                }
            }
            stage[c as usize] = highest.map_or(0, |s| s + 1);
            for &m in &clusters[c as usize].members {
                for r in reads(m as usize) {
                    if cluster_of[r] != c {
                        last_read[r] = last_read[r].max(Some(stage[c as usize]));
                    }
                }
            }
        }
        for &n in kept {
            last_read[n] = Some(u32::MAX);
        }

        // Each cluster's shape: its nodes' instructions, with a load of a node
        // of the cluster naming it by its place in the cluster.
        let mut shapes: Vec<Vec<u32>> = Vec::new();
        let mut known: HashMap<Vec<u32>, u32> = HashMap::new();
        let mut shape_of = vec![0u32; clusters.len()];
        let mut place: HashMap<u32, u32> = HashMap::new();
        let mut shape: Vec<u32> = Vec::new();
        for &c in &order {
            let members = &clusters[c as usize].members;
            place.clear();
            shape.clear();
            for (k, &m) in members.iter().enumerate() {
                place.insert(m, k as u32);
                let ops = ops(m as usize);
                shape.push(ops.len() as u32);
                shape.push(u32::from(last_read[m as usize].is_some()));
                for &op in ops {
                    let local = match op & CODE {
                        LOAD => node_of(op >> CODE_BITS).and_then(|r| place.get(&(r as u32))),
                        _ => None,
                    };
                    shape.push(match local {
                        Some(&at) => LOCAL | at << CODE_BITS,
                        None => op & CODE,
                    });
                }
            }
            shape_of[c as usize] = match known.get(&shape) {
                Some(&s) => s,
                None => {
                    let s = shapes.len() as u32;
                    shapes.push(shape.clone());
                    known.insert(shape.clone(), s);
                    s
                }
            };
        }

        let several = |c: u32| clusters[c as usize].members.len() > 1;
        let mut covers = vec![0usize; shapes.len()];
        for &c in &order {
            if several(c) {
                covers[shape_of[c as usize] as usize] += clusters[c as usize].members.len();
            }
        }
        let mut most_first: Vec<usize> = (0..shapes.len()).filter(|&s| covers[s] > 0).collect();
        if again || most_first.len() <= shapes_most {
            break (order, stage, last_read, shapes, shape_of);
        }
        again = true;
        most_first.sort_by_key(|&s| Reverse(covers[s]));
        let mut stays = vec![false; shapes.len()];
        for &s in &most_first[..shapes_most] {
            stays[s] = true;
        }
        for &c in &order {
            if clusters[c as usize].members.len() > 1 && !stays[shape_of[c as usize] as usize] {
                let members = std::mem::take(&mut clusters[c as usize].members);
                clusters[c as usize].members = vec![members[0]];
                for &m in &members[1..] {
                    cluster_of[m as usize] = u32::try_from(clusters.len()).ok()?;
                    clusters.push(Cluster {
                        members: vec![m],
                        sealed: true,
                    });
                }
            }
        }
    };

    // Work the clusters out a stage at a time, and within a stage a shape
    // at a time.
    order.sort_by_key(|&c| (stage[c as usize], shape_of[c as usize]));

    // A slot for each node that goes to memory, free again after the last
    // stage that reads it. The lowest free slot is used first.
    let stages = order.last().map_or(0, |&c| stage[c as usize] as usize + 1);
    let mut freed_after: Vec<Vec<u32>> = vec![Vec::new(); stages];
    let mut free: BinaryHeap<Reverse<u32>> = BinaryHeap::new();
    let mut slot_of: Vec<Option<u32>> = vec![None; nodes];
    let mut slots = vary as u32;
    let mut released = 0;
    for &c in &order {
        let at = stage[c as usize] as usize;
        while released < at {
            free.extend(freed_after[released].drain(..).map(Reverse));
            released += 1;
        }
        for &m in &clusters[c as usize].members {
            let Some(last) = last_read[m as usize] else {
                continue;
            };
            let slot = match free.pop() {
                Some(Reverse(slot)) => slot,
                None => {
                    slots = slots.checked_add(1)?;
                    slots - 1
                }
            };
            slot_of[m as usize] = Some(slot);
            if last != u32::MAX {
                freed_after[last as usize].push(slot);
            }
        }
    }

    // Each cluster's operands, and the groups.
    let mut operands: Vec<u32> = Vec::new();
    let mut operand_at = Vec::with_capacity(order.len());
    let mut groups: Vec<Group> = Vec::new();
    let mut before = None;
    for (k, &c) in order.iter().enumerate() {
        operand_at.push(u32::try_from(operands.len()).ok()?);
        let members = &clusters[c as usize].members;
        for &m in members {
            for &op in ops(m as usize) {
                let arg = op >> CODE_BITS;
                match op & CODE {
                    LOAD => match node_of(arg) {
                        // A varied input.
                        None => operands.push(arg),
                        Some(r) if cluster_of[r] != c => {
                            operands.push(slot_of[r].expect("a node read from outside has a slot"));
                        }
                        Some(_) => {}
                    },
                    CONST | LOAD_SHARED => operands.push(arg),
                    _ => {}
                }
            }
        }
        operands.extend(members.iter().filter_map(|&m| slot_of[m as usize]));
        let now = (stage[c as usize], shape_of[c as usize]);
        match groups.last_mut() {
            Some(last) if before == Some(now) => last.count += 1,
            _ => groups.push(Group {
                shape: now.1,
                first: k as u32,
                count: 1,
            }),
        }
        before = Some(now);
    }
    Some(Fused {
        slots: slots as usize,
        widest: groups.iter().map(|g| g.count as usize).max().unwrap_or(1),
        shapes,
        groups,
        operand_at,
        operands,
        slot_of,
    })
}
