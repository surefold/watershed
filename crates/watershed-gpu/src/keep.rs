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
//! Run the small plan where the whole one was run. A GPU and a CPU give the
//! same bits except for the last digits of `exp`, `ln` and `pow`, so a
//! model that uses those, refreshed on the other device, is right to about
//! 1e-15 and not to the last bit.
//!
//! Which nodes to keep is the caller's choice: more of them means less to
//! work out after a change and more memory. The outputs are always kept.

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;
use watershed_core::{Graph, NodeId, Type};

use crate::plan::{LoadError, PlanError, Scenarios};

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

/// What to run to bring kept values up to date after a change, apart from
/// the values themselves: a small plan, which kept values it is given, and
/// which it works out again.
///
/// EXPERIMENTAL. A [`Kept`] makes one with [`Kept::patch`]. It can be
/// written to a file and read back, so that whoever holds the values of some
/// of the scenarios, in another process or on another machine, can bring
/// those up to date: [`inputs`](Self::inputs) makes the small plan's inputs
/// from them, the plan is run, and [`apply`](Self::apply) puts its answers
/// in.
#[derive(Debug, Clone)]
pub struct Patch {
    /// The plan to run: its inputs are the varied inputs and then the kept
    /// values it reads, and its outputs are the kept values it replaces.
    pub plan: Scenarios,
    /// How many values are kept for a scenario.
    width: usize,
    /// The kept values the plan reads, in the order it takes them.
    reads: Vec<u32>,
    /// The kept values the plan's outputs replace, in their order.
    writes: Vec<u32>,
}

/// The first bytes of a patch's file. The last is the layout's version.
const PATCH: [u8; 8] = *b"WSPATCH\x01";

impl Patch {
    /// How many kept values a scenario has.
    pub fn width(&self) -> usize {
        self.width
    }

    /// How many of them the plan works out again.
    pub fn stale(&self) -> usize {
        self.writes.len()
    }

    /// How many inputs each scenario varies.
    fn vary(&self) -> usize {
        self.plan.inputs() - self.reads.len()
    }

    /// The small plan's inputs for some scenarios: each one's varied
    /// `inputs`, then the kept `values` the plan reads.
    ///
    /// # Panics
    /// If `inputs` and `values` are not those of the same whole number of
    /// scenarios.
    pub fn inputs(&self, inputs: &[f64], values: &[f64]) -> Vec<f64> {
        let (vary, width, row) = (self.vary(), self.width, self.plan.inputs());
        assert_eq!(inputs.len() % vary, 0, "a part of a scenario");
        assert_eq!(
            inputs.len() / vary * width,
            values.len(),
            "the values are not those kept for these inputs"
        );
        let mut all = vec![0.0; inputs.len() / vary * row];
        all.par_chunks_mut(row)
            .zip(inputs.par_chunks(vary))
            .zip(values.par_chunks(width))
            .for_each(|((row, varied), kept)| {
                row[..vary].copy_from_slice(varied);
                for (x, &at) in row[vary..].iter_mut().zip(&self.reads) {
                    *x = kept[at as usize];
                }
            });
        all
    }

    /// Puts what the small plan gave for some scenarios into their kept
    /// `values`.
    ///
    /// # Panics
    /// If `results` is not the plan's outputs for those scenarios.
    pub fn apply(&self, values: &mut [f64], results: &[f64]) {
        assert_eq!(values.len() % self.width, 0, "a part of a scenario");
        assert_eq!(
            results.len(),
            values.len() / self.width * self.writes.len(),
            "the results are not the plan's for these scenarios"
        );
        values
            .par_chunks_mut(self.width)
            .zip(results.par_chunks(self.writes.len()))
            .for_each(|(row, new)| {
                for (&at, &x) in self.writes.iter().zip(new) {
                    row[at as usize] = x;
                }
            });
    }

    /// Writes the patch out, to be read back by [`Patch::load`].
    pub fn save(&self, mut to: impl std::io::Write) -> std::io::Result<()> {
        to.write_all(&PATCH)?;
        to.write_all(&(self.width as u64).to_le_bytes())?;
        for list in [&self.reads, &self.writes] {
            to.write_all(&(list.len() as u64).to_le_bytes())?;
            for at in list {
                to.write_all(&at.to_le_bytes())?;
            }
        }
        self.plan.save(to)
    }

    /// Reads a patch written by [`Patch::save`]. As with a plan, load only
    /// what you saved.
    pub fn load(mut from: impl std::io::Read) -> Result<Patch, LoadError> {
        let mut magic = [0; 8];
        from.read_exact(&mut magic)
            .map_err(|_| LoadError::NotAPlan)?;
        if magic != PATCH {
            return Err(LoadError::NotAPlan);
        }
        let number = |from: &mut dyn std::io::Read| -> Result<usize, LoadError> {
            let mut bytes = [0; 8];
            from.read_exact(&mut bytes)?;
            usize::try_from(u64::from_le_bytes(bytes)).map_err(|_| LoadError::Damaged)
        };
        let width = number(&mut from)?;
        let mut lists = [Vec::new(), Vec::new()];
        for list in &mut lists {
            let count = number(&mut from)?;
            // No more than there are kept values, so a damaged count
            // cannot ask for more memory than that.
            if count > width {
                return Err(LoadError::Damaged);
            }
            for _ in 0..count {
                let mut bytes = [0; 4];
                from.read_exact(&mut bytes)?;
                let at = u32::from_le_bytes(bytes);
                if at as usize >= width {
                    return Err(LoadError::Damaged);
                }
                list.push(at);
            }
        }
        let [reads, writes] = lists;
        let plan = Scenarios::load(from)?;
        if plan.outputs() != writes.len() || plan.inputs() < reads.len() {
            return Err(LoadError::Damaged);
        }
        Ok(Patch {
            plan,
            width,
            reads,
            writes,
        })
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

    /// Chooses nodes to keep without being told the graph's shape: the
    /// calculated nodes nearest the outputs, a step at a time (what the
    /// outputs use, then what those use, and so on), for as long as no more
    /// than `most` are kept in all. A step is taken whole or not at all.
    ///
    /// This suits a graph that ends by adding many things up, where the
    /// nodes near the outputs are sums over parts of the model: a change to
    /// one part then reaches few of them. It does not look at what any
    /// particular change would reach.
    pub fn nearest(g: &Graph, outputs: &[NodeId], most: usize) -> Vec<NodeId> {
        let mut seen: HashSet<NodeId> = outputs.iter().copied().collect();
        let mut keep: Vec<NodeId> = Vec::new();
        let mut step: Vec<NodeId> = outputs.to_vec();
        loop {
            let mut next: Vec<NodeId> = Vec::new();
            for &id in &step {
                for used in g.dependencies(id) {
                    if !g.is_input(used) && seen.insert(used) {
                        next.push(used);
                    }
                }
            }
            // Only numbers can be kept; anything else is passed through.
            let numbers = next
                .iter()
                .filter(|&&id| g.node_type(id) == Type::F64)
                .count();
            if next.is_empty() || keep.len() + numbers > most {
                return keep;
            }
            keep.extend(next.iter().filter(|&&id| g.node_type(id) == Type::F64));
            step = next;
        }
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

    /// The kept values' places among a scenario's, for each output.
    pub fn output_columns(&self) -> &[u32] {
        &self.outputs
    }

    /// What to run after `changed` inputs of the graph have new values, for
    /// whoever holds the kept values: see [`Patch`]. `None` if no kept
    /// value uses them. Unlike [`refresh`](Self::refresh) this needs no
    /// values to have been given to [`hold`](Self::hold).
    pub fn patch(&self, g: &Graph, changed: &[NodeId]) -> Result<Option<Patch>, PlanError> {
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

        // The kept nodes those read that are still right: the plan is cut
        // there, and given their values.
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
        Ok(Some(Patch {
            plan,
            width: self.keep.len(),
            reads: right,
            writes: stale,
        }))
    }

    /// What to run now that the inputs `changed` have other values in `g`
    /// than when the plan was made (or the last refresh). `None` if nothing
    /// kept uses them. Inputs that are varied or shared are not meant here:
    /// they are not built into the plan.
    pub fn refresh(&self, g: &Graph, changed: &[NodeId]) -> Result<Option<Refresh>, PlanError> {
        let Some(patch) = self.patch(g, changed)? else {
            return Ok(None);
        };
        let inputs = patch.inputs(&self.inputs, &self.values);
        Ok(Some(Refresh {
            plan: patch.plan,
            inputs,
            stale: patch.writes,
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

        // The same scenarios held in two parts by someone else, who has
        // only the values and what is read back from a file.
        let width = kept.width();
        let cut = 20;
        let mut parts = [
            (
                inputs[..cut * 2].to_vec(),
                kept.values()[..cut * width].to_vec(),
            ),
            (
                inputs[cut * 2..].to_vec(),
                kept.values()[cut * width..].to_vec(),
            ),
        ];

        for (n, to) in [(6, 150.0), (19, 20.0), (6, 96.0)] {
            g.set(floors[n], to).unwrap();
            g.stabilize();
            let mut file = Vec::new();
            kept.patch(&g, &[floors[n].id()])
                .unwrap()
                .expect("a group uses the floor")
                .save(&mut file)
                .unwrap();
            let patch = Patch::load(&file[..]).unwrap();
            assert_eq!((patch.width(), patch.stale()), (width, 2));
            for (inputs, values) in &mut parts {
                let results = patch.plan.run_cpu(&patch.inputs(inputs, values));
                patch.apply(values, &results);
            }
            // One group and the total.
            assert_eq!(kept.refresh_cpu(&g, &[floors[n].id()]).unwrap(), 2);
            let apart: Vec<f64> = parts.iter().flat_map(|(_, v)| v.iter().copied()).collect();
            assert!(
                apart
                    .iter()
                    .zip(kept.values())
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
            let whole = Kept::new(&g, &vary, &[], &[total.id()], &keep).unwrap();
            let again = whole.plan().run_cpu(&inputs);
            assert!(
                again
                    .iter()
                    .zip(kept.values())
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            );
        }
        // Chosen without being told: the groups are the step nearest the
        // total, and the step after is too many.
        let mut near = Kept::nearest(&g, &[total.id()], 10);
        near.sort();
        assert_eq!(near, keep);
        assert!(Kept::nearest(&g, &[total.id()], 4).is_empty());
        // With room for more it goes on into the accounts, past the
        // conditions, which are not numbers and are not kept.
        let more = Kept::nearest(&g, &[total.id()], 1_000);
        assert!(more.len() > 25);
        assert!(more.iter().all(|&id| g.node_type(id) == Type::F64));
        let deep = Kept::new(&g, &vary, &[], &[total.id()], &more).unwrap();
        assert_eq!(deep.plan().run_cpu(&inputs).len(), 50 * deep.width());

        // An input nothing kept uses.
        let aside = g.input("aside", 1.0).unwrap();
        assert_eq!(kept.refresh_cpu(&g, &[aside.id()]).unwrap(), 0);
        assert!(kept.patch(&g, &[aside.id()]).unwrap().is_none());
        assert!(matches!(
            Patch::load(&b"WSPATCH\x01 and then nothing"[..]),
            Err(LoadError::Damaged)
        ));
    }
}
