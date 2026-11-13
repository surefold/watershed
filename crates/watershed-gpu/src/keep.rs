//! Keeps some of a run's values, so that after a change to the graph the
//! run can be brought up to date by working out only what the change
//! touches.
//!
//! EXPERIMENTAL. A plan has the graph's other inputs built into it, so when
//! one of them changes the plan is made again and all of it run again. But
//! the graph says what the change can reach. If the first run kept the
//! values of some nodes for every scenario, then only the kept nodes the
//! change can reach need working out again, and what they read of the kept
//! nodes it cannot reach is already known. A small plan does that: it is cut
//! at the kept nodes that are still right, which it is given as it is given
//! the varied inputs.
//!
//! Which nodes to keep is the caller's choice: more of them means less to
//! work out after a change and more memory. The outputs are always kept.

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;
use watershed_core::{Graph, NodeId};

use crate::plan::{PlanError, Scenarios};

/// A run whose kept values can be brought up to date after a change.
#[derive(Debug)]
pub struct Kept {
    vary: Vec<NodeId>,
    shared: Vec<NodeId>,
    /// The kept nodes: a scenario's kept values are in this order.
    keep: Vec<NodeId>,
    /// Where each output is among the kept nodes.
    outputs: Vec<u32>,
    /// For each input built into the plan, the kept nodes that use it
    /// without going through another kept node.
    users: HashMap<NodeId, Vec<u32>>,
    /// For each kept node, the kept nodes that use it likewise, and the
    /// kept nodes it uses.
    above: Vec<Vec<u32>>,
    below: Vec<Vec<u32>>,
    plan: Scenarios,
    inputs: Vec<f64>,
    values: Vec<f64>,
}

/// What to run to bring a [`Kept`] up to date: `plan` on `inputs`, on the
/// CPU or a GPU, whose results are then given to [`Kept::apply`].
#[derive(Debug)]
pub struct Refresh {
    pub plan: Scenarios,
    pub inputs: Vec<f64>,
    stale: Vec<u32>,
}

impl Refresh {
    /// How many kept nodes are worked out again.
    pub fn stale(&self) -> usize {
        self.stale.len()
    }
}

impl Kept {
    /// As [`Scenarios::with_shared`], keeping the nodes `keep` as well as
    /// the outputs.
    pub fn new(
        g: &Graph,
        vary: &[NodeId],
        shared: &[NodeId],
        outputs: &[NodeId],
        keep: &[NodeId],
    ) -> Result<Kept, PlanError> {
        let mut column: HashMap<NodeId, u32> = HashMap::new();
        let mut all: Vec<NodeId> = Vec::new();
        for &id in keep.iter().chain(outputs) {
            if id.index() >= g.len() {
                return Err(PlanError::UnknownNode(id));
            }
            if !g.is_input(id) && !column.contains_key(&id) {
                column.insert(id, all.len() as u32);
                all.push(id);
            }
        }
        let plan = Scenarios::with_shared(g, vary, shared, &all)?;
        let outputs = outputs
            .iter()
            .map(|id| column.get(id).copied().ok_or(PlanError::NoOutputs))
            .collect::<Result<_, _>>()?;

        // What each kept node uses, stopping at kept nodes and at inputs.
        let given: HashSet<NodeId> = vary.iter().chain(shared).copied().collect();
        let found: Vec<(Vec<u32>, Vec<NodeId>)> = all
            .par_iter()
            .map(|&top| {
                let mut seen: HashSet<NodeId> = HashSet::new();
                let (mut kept, mut inputs) = (Vec::new(), Vec::new());
                let mut stack = g.dependencies(top);
                while let Some(id) = stack.pop() {
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Some(&at) = column.get(&id) {
                        kept.push(at);
                    } else if g.is_input(id) {
                        if !given.contains(&id) {
                            inputs.push(id);
                        }
                    } else {
                        stack.extend(g.dependencies(id));
                    }
                }
                (kept, inputs)
            })
            .collect();
        let mut users: HashMap<NodeId, Vec<u32>> = HashMap::new();
        let mut above = vec![Vec::new(); all.len()];
        let mut below = Vec::with_capacity(all.len());
        for (top, (kept, inputs)) in found.into_iter().enumerate() {
            for input in inputs {
                users.entry(input).or_default().push(top as u32);
            }
            for &at in &kept {
                above[at as usize].push(top as u32);
            }
            below.push(kept);
        }
        Ok(Kept {
            vary: vary.to_vec(),
            shared: shared.to_vec(),
            keep: all,
            outputs,
            users,
            above,
            below,
            plan,
            inputs: Vec::new(),
            values: Vec::new(),
        })
    }

    /// The plan of the whole run. Its results are every kept value for each
    /// scenario, to give to [`hold`](Self::hold).
    pub fn plan(&self) -> &Scenarios {
        &self.plan
    }

    /// How many values are kept for each scenario.
    pub fn width(&self) -> usize {
        self.keep.len()
    }

    /// Takes the scenarios' inputs and the results of running
    /// [`plan`](Self::plan) on them.
    ///
    /// # Panics
    ///
    /// If the two are not for the same number of scenarios.
    pub fn hold(&mut self, inputs: Vec<f64>, results: Vec<f64>) {
        assert_eq!(inputs.len() % self.vary.len(), 0, "a part of a scenario");
        assert_eq!(
            inputs.len() / self.vary.len() * self.keep.len(),
            results.len(),
            "the results are not this plan's for these inputs"
        );
        (self.inputs, self.values) = (inputs, results);
    }

    /// How many scenarios are held.
    pub fn scenarios(&self) -> usize {
        self.inputs.len() / self.vary.len()
    }

    /// The outputs, for each scenario.
    pub fn outputs(&self) -> Vec<f64> {
        self.values
            .chunks(self.keep.len())
            .flat_map(|row| self.outputs.iter().map(|&at| row[at as usize]))
            .collect()
    }

    /// Every kept value, for each scenario, in the order of the plan's
    /// outputs.
    pub fn values(&self) -> &[f64] {
        &self.values
    }

    /// What to run now that the inputs `changed` have other values in `g`
    /// than when the plan was made (or the last refresh). `None` if nothing
    /// kept uses them. Inputs that are varied or shared are not meant here:
    /// they are not built into the plan.
    pub fn refresh(&self, g: &Graph, changed: &[NodeId]) -> Result<Option<Refresh>, PlanError> {
        // The kept nodes the change can reach.
        let mut is_stale = vec![false; self.keep.len()];
        let mut stack: Vec<u32> = changed
            .iter()
            .filter_map(|id| self.users.get(id))
            .flatten()
            .copied()
            .collect();
        let mut stale = Vec::new();
        while let Some(at) = stack.pop() {
            if !std::mem::replace(&mut is_stale[at as usize], true) {
                stale.push(at);
                stack.extend(&self.above[at as usize]);
            }
        }
        if stale.is_empty() {
            return Ok(None);
        }
        stale.sort_unstable();

        // What they read of the kept nodes it cannot reach.
        let mut right: Vec<u32> = stale
            .iter()
            .flat_map(|&at| &self.below[at as usize])
            .copied()
            .filter(|&at| !is_stale[at as usize])
            .collect();
        right.sort_unstable();
        right.dedup();

        let node = |at: &u32| self.keep[*at as usize];
        let plan = Scenarios::cut_at(
            g,
            &self.vary,
            &right.iter().map(node).collect::<Vec<_>>(),
            &self.shared,
            &stale.iter().map(node).collect::<Vec<_>>(),
        )?;
        let (vary, width) = (self.vary.len(), self.keep.len());
        let mut inputs = vec![0.0; self.scenarios() * (vary + right.len())];
        inputs
            .par_chunks_mut(vary + right.len())
            .zip(self.inputs.par_chunks(vary))
            .zip(self.values.par_chunks(width))
            .for_each(|((row, varied), kept)| {
                row[..vary].copy_from_slice(varied);
                for (x, &at) in row[vary..].iter_mut().zip(&right) {
                    *x = kept[at as usize];
                }
            });
        Ok(Some(Refresh {
            plan,
            inputs,
            stale,
        }))
    }

    /// Puts in the results of running a [`Refresh`].
    ///
    /// # Panics
    ///
    /// If `results` are not that refresh's for the scenarios held.
    pub fn apply(&mut self, refresh: &Refresh, results: &[f64]) {
        let stale = &refresh.stale;
        assert_eq!(results.len(), self.scenarios() * stale.len());
        self.values
            .par_chunks_mut(self.keep.len())
            .zip(results.par_chunks(stale.len()))
            .for_each(|(row, new)| {
                for (&at, &x) in stale.iter().zip(new) {
                    row[at as usize] = x;
                }
            });
    }

    /// [`refresh`](Self::refresh), run on the CPU, and
    /// [`apply`](Self::apply). Gives how many kept nodes were worked out
    /// again.
    pub fn refresh_cpu(&mut self, g: &Graph, changed: &[NodeId]) -> Result<usize, PlanError> {
        let Some(refresh) = self.refresh(g, changed)? else {
            return Ok(0);
        };
        let results = refresh.plan.run_cpu(&refresh.inputs);
        self.apply(&refresh, &results);
        Ok(refresh.stale())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use watershed_core::prelude::*;

    /// Accounts in groups, a sum for each group and a sum of those; one
    /// account's own input changes.
    #[test]
    fn a_refresh_gives_what_running_it_all_again_gives() {
        let mut g = Graph::new();
        let rate = g.input("rate", 0.01).unwrap();
        let shock = g.input("shock", 0.0).unwrap();
        let mut floors = Vec::new();
        let mut groups = Vec::new();
        for a in 0..5 {
            let mut each = Vec::new();
            for b in 0..4 {
                let n = a * 4 + b;
                let floor = g.input(&format!("floor{n}"), 90.0 + n as f64).unwrap();
                let mut value = g
                    .define(&format!("v{n}_0"), (1.0 + rate) * (100.0 + n as f64))
                    .unwrap();
                for t in 1..6 {
                    value = g
                        .define(&format!("v{n}_{t}"), value * (1.0 + rate - shock * 0.1))
                        .unwrap();
                }
                each.push(
                    g.define(
                        &format!("worth{n}"),
                        when(value.gt(floor))
                            .then(value)
                            .otherwise(floor * (1.0 + shock)),
                    )
                    .unwrap(),
                );
                floors.push(floor);
            }
            let mut all = each[0].into_expr();
            for &x in &each[1..] {
                all = all + x;
            }
            groups.push(g.define(&format!("group{a}"), all).unwrap());
        }
        let mut all = groups[0].into_expr();
        for &x in &groups[1..] {
            all = all + x;
        }
        let total = g.define("total", all).unwrap();
        g.stabilize();

        let vary = [rate.id(), shock.id()];
        let keep: Vec<NodeId> = groups.iter().map(|n| n.id()).collect();
        let inputs: Vec<f64> = (0..50)
            .flat_map(|s| [0.002 * s as f64 - 0.03, 0.01 * (s % 7) as f64])
            .collect();
        let mut kept = Kept::new(&g, &vary, &[], &[total.id()], &keep).unwrap();
        let first = kept.plan().run_cpu(&inputs);
        kept.hold(inputs.clone(), first);

        for (n, to) in [(6, 150.0), (19, 20.0), (6, 96.0)] {
            g.set(floors[n], to).unwrap();
            g.stabilize();
            // One group and the total.
            assert_eq!(kept.refresh_cpu(&g, &[floors[n].id()]).unwrap(), 2);
            let whole = Kept::new(&g, &vary, &[], &[total.id()], &keep).unwrap();
            let again = whole.plan().run_cpu(&inputs);
            assert!(
                again
                    .iter()
                    .zip(kept.values())
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
        // An input nothing kept uses.
        let aside = g.input("aside", 1.0).unwrap();
        assert_eq!(kept.refresh_cpu(&g, &[aside.id()]).unwrap(), 0);
    }
}
