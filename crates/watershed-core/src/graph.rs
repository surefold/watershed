//! The graph: nodes, their values, and incremental recalculation.
//!
//! # How an update works
//!
//! 1. [`Graph::set`] only *records* a new value for an input. Reads keep
//!    returning the values from the last update, so what you read is always
//!    consistent with itself.
//! 2. [`Graph::stabilize`] applies every recorded change at once. Each input
//!    whose value really changed queues the nodes that use it.
//! 3. Dirty nodes are recalculated in order of *height* (an input has height
//!    0, and every other node is one higher than the highest node it uses).
//!    So a node is only ever recalculated after everything it uses is up to
//!    date, and at most once per update, however many of its inputs changed.
//! 4. **Cutoff:** if a recalculated node's value is identical to its old one,
//!    the nodes above it are not marked, so the update stops spreading there.

use std::collections::hash_map::RandomState;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::compile::{
    FloatOp, GeneralProgram, Program, Scratch, compile, decompile_float, eval_float, float_loads,
};
use crate::error::CalcError;
use crate::expr::IntoExpr;
use crate::handle::{GraphId, Handle, Input, Node, NodeId, NodeRef};
use crate::ir::{Ir, TypeError};
use crate::value::{CalcType, Type, Value};

static NEXT_GRAPH_ID: AtomicU64 = AtomicU64::new(1);

/// Marks the end of a list in the edge arrays.
const NONE: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Input,
    /// A pure-float program at `float_ops[code..code + len]`.
    Float,
    /// `general[code]`.
    General,
}

/// What the update loop needs about a node, packed into 16 bytes so that
/// visiting a node touches as little memory as possible.
#[derive(Debug, Clone, Copy)]
struct Hot {
    code: u32,
    len: u32,
    height: u32,
    kind: Kind,
}

/// Every node's name, stored back to back in one string, with a compact
/// index from name to node.
#[derive(Default)]
struct Names {
    text: String,
    /// Where each name starts in `text`; one extra entry marks the end.
    starts: Vec<u32>,
    hasher: RandomState,
    /// From a name's hash to its node. Two names can share a hash; the
    /// second and later ones go in `collisions`.
    index: HashMap<u64, NodeId, Prehashed>,
    collisions: HashMap<String, NodeId>,
}

impl Names {
    fn get(&self, id: NodeId) -> &str {
        let i = id.index();
        &self.text[self.starts[i] as usize..self.starts[i + 1] as usize]
    }

    fn find(&self, name: &str) -> Option<NodeId> {
        match self.index.get(&self.hasher.hash_one(name)) {
            Some(&id) if self.get(id) == name => Some(id),
            _ => self.collisions.get(name).copied(),
        }
    }

    /// Adds the next node's name. The caller has checked it is new.
    fn push(&mut self, name: &str, id: NodeId) {
        if self.starts.is_empty() {
            self.starts.push(0);
        }
        self.text.push_str(name);
        self.starts
            .push(u32::try_from(self.text.len()).expect("node names exceed 4 GB in total"));
        let hash = self.hasher.hash_one(name);
        match self.index.entry(hash) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(id);
            }
            std::collections::hash_map::Entry::Occupied(_) => {
                self.collisions.insert(name.to_string(), id);
            }
        }
    }
}

/// The index's keys are already hashes, so hashing them again is wasted work.
#[derive(Default, Clone, Copy)]
struct Prehashed;

impl BuildHasher for Prehashed {
    type Hasher = PrehashedHasher;
    fn build_hasher(&self) -> PrehashedHasher {
        PrehashedHasher(0)
    }
}

struct PrehashedHasher(u64);

impl Hasher for PrehashedHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 << 8) ^ u64::from(b);
        }
    }
    fn write_u64(&mut self, n: u64) {
        self.0 = n;
    }
}

/// What one call to [`Graph::stabilize`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    /// This update's number. The first update is 1.
    pub epoch: u64,
    /// Inputs whose value actually changed (setting an input to the value it
    /// already has does not count).
    pub inputs_changed: usize,
    /// Calculated nodes that were recalculated.
    pub recalculated: usize,
    /// Of those, how many came out with a different value.
    pub changed: usize,
}

/// A graph of inputs and calculated nodes.
///
/// Everything is stored in flat arrays indexed by node id, with no per-node
/// heap allocation for ordinary nodes, so a graph of a million float nodes
/// takes on the order of a hundred bytes per node.
pub struct Graph {
    id: GraphId,

    // One entry per node, indexed by node id.
    values: Vec<Value>,
    hot: Vec<Hot>,
    types: Vec<Type>,
    /// The update in which the value last changed (0 = when it was created).
    changed_in: Vec<u64>,
    names: Names,

    /// Who uses each node, packed back to back so that passing on an update
    /// reads them in one sweep: node n's users are
    /// `packed[packed_start[n]..packed_start[n + 1]]`. This covers the links
    /// that existed at the last repack (see `Graph::repack`).
    packed_start: Vec<u32>,
    packed: Vec<Use>,
    /// Links added since the last repack, as linked lists in one flat array:
    /// `first_user[n]` is node n's first such edge, and each edge holds its
    /// user and the next edge.
    first_user: Vec<u32>,
    last_user: Vec<u32>,
    edges: Vec<Edge>,

    /// The instructions and constants of every pure-float program, back to back.
    float_ops: Vec<FloatOp>,
    float_consts: Vec<f64>,
    /// Programs that are not pure float arithmetic.
    general: Vec<GeneralProgram>,

    /// Input changes recorded by `set`, applied by `stabilize`. A later `set`
    /// of the same input replaces an earlier one.
    pending: BTreeMap<NodeId, Value>,
    /// Dirty nodes waiting to be recalculated, one list per height.
    queue: Vec<Vec<NodeId>>,
    epoch: u64,
    last_recalculated: Vec<NodeId>,
    /// Scratch space for evaluating programs.
    scratch: Scratch,
    /// Levels with at least this many nodes to recalculate are spread over
    /// all CPU cores (with the `parallel` feature).
    parallel_threshold: usize,
    /// Every calculated node, grouped by height, for recalculating everything
    /// in parallel. Built when first needed; cleared when a node is added.
    levels: Option<Vec<Vec<NodeId>>>,
    /// Results of recalculating a level in parallel: the new value for each
    /// node whose value changed. Kept between updates to reuse its memory.
    changes: Vec<Option<Value>>,
    /// Nodes whose value changed on the level being processed in parallel.
    changed_ids: Vec<NodeId>,
}

/// The level size from which recalculating in parallel pays off. Measured on
/// the benchmark graph with 4 cores: below about 30,000 nodes, the time
/// threads spend waiting for each other outweighs the work they share.
const DEFAULT_PARALLEL_THRESHOLD: usize = 32_768;

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            id: GraphId(NEXT_GRAPH_ID.fetch_add(1, Ordering::Relaxed)),
            values: Vec::new(),
            hot: Vec::new(),
            types: Vec::new(),
            changed_in: Vec::new(),
            names: Names::default(),
            packed_start: vec![0],
            packed: Vec::new(),
            first_user: Vec::new(),
            last_user: Vec::new(),
            edges: Vec::new(),
            float_ops: Vec::new(),
            float_consts: Vec::new(),
            general: Vec::new(),
            pending: BTreeMap::new(),
            queue: Vec::new(),
            epoch: 0,
            last_recalculated: Vec::new(),
            scratch: Scratch::default(),
            parallel_threshold: DEFAULT_PARALLEL_THRESHOLD,
            levels: None,
            changes: Vec::new(),
            changed_ids: Vec::new(),
        }
    }

    // ---------------------------------------------------------------- building

    /// Makes room for `nodes` more nodes using about `edges` more links
    /// between them, so a large graph is built without its arrays being
    /// copied as they grow (which briefly needs up to twice the memory).
    pub fn reserve(&mut self, nodes: usize, edges: usize) {
        self.values.reserve(nodes);
        self.hot.reserve(nodes);
        self.types.reserve(nodes);
        self.changed_in.reserve(nodes);
        self.first_user.reserve(nodes);
        self.last_user.reserve(nodes);
        self.names.starts.reserve(nodes + 1);
        self.names.index.reserve(nodes);
        self.edges.reserve(edges);
        self.packed_start.reserve(nodes);
    }

    /// Adds an input with a starting value.
    pub fn input<T: CalcType>(&mut self, name: &str, value: T) -> Result<Input<T>, CalcError> {
        self.check_name(name)?;
        let hot = Hot {
            code: 0,
            len: 0,
            height: 0,
            kind: Kind::Input,
        };
        let id = self.push(name, hot, T::TYPE, &[], value.into_value());
        Ok(Input::new(self.node_ref(id)))
    }

    /// Adds an input through the untyped API. Its type is the type of `value`,
    /// which therefore cannot be an error.
    pub fn input_value(&mut self, name: &str, value: Value) -> Result<NodeId, CalcError> {
        self.check_name(name)?;
        let ty = value
            .type_of()
            .ok_or_else(|| TypeError("an input's starting value cannot be an error".into()))?;
        let hot = Hot {
            code: 0,
            len: 0,
            height: 0,
            kind: Kind::Input,
        };
        Ok(self.push(name, hot, ty, &[], value))
    }

    /// Adds a calculated node. Its value is worked out straight away.
    pub fn define<T: CalcType>(
        &mut self,
        name: &str,
        expr: impl IntoExpr<T>,
    ) -> Result<Node<T>, CalcError> {
        let expr = expr.into_expr();
        let id = self.define_ir(name, (*expr.ir).clone())?;
        let ty = self.types[id.index()];
        if ty != T::TYPE {
            // The typed API cannot build such an expression; this is a safety net.
            return Err(CalcError::TypeMismatch {
                node: name.into(),
                expected: T::TYPE,
                found: Some(ty),
            });
        }
        Ok(Node::new(self.node_ref(id)))
    }

    /// Adds a calculated node from an untyped expression, checking it first.
    /// This is the entry point for front ends other than the typed Rust API.
    pub fn define_ir(&mut self, name: &str, ir: Ir) -> Result<NodeId, CalcError> {
        self.check_name(name)?;

        let mut refs = Vec::new();
        ir.refs(&mut refs);
        for r in &refs {
            self.check_ref(*r)?;
        }
        let ty = ir.type_check(&|r| {
            self.types
                .get(r.node.index())
                .copied()
                .ok_or_else(|| TypeError(format!("there is no node {}", r.node)))
        })?;

        let mut deps: Vec<NodeId> = refs.into_iter().map(|r| r.node).collect();
        deps.sort();
        deps.dedup();
        // Inputs have height 0; any other node is one above the highest node
        // it uses (1 if it uses none).
        let height = deps
            .iter()
            .map(|d| self.hot[d.index()].height + 1)
            .max()
            .unwrap_or(1);
        let types = &self.types;
        let program = compile(
            &ir,
            &|i| types[i],
            &mut self.float_ops,
            &mut self.float_consts,
        );
        let hot = match program {
            Program::Float { start, len } => Hot {
                code: start,
                len,
                height,
                kind: Kind::Float,
            },
            Program::General(program) => {
                self.general.push(program);
                let code = u32::try_from(self.general.len() - 1).expect("too many programs");
                Hot {
                    code,
                    len: 0,
                    height,
                    kind: Kind::General,
                }
            }
        };
        // Worked out from the values of the last update, like every other node.
        let value = self.evaluate(hot);
        Ok(self.push(name, hot, ty, &deps, value))
    }

    fn push(&mut self, name: &str, hot: Hot, ty: Type, deps: &[NodeId], value: Value) -> NodeId {
        let id =
            NodeId(u32::try_from(self.hot.len()).expect("a graph holds at most u32::MAX nodes"));
        self.values.push(value);
        self.hot.push(hot);
        self.types.push(ty);
        self.changed_in.push(self.epoch);
        self.names.push(name, id);
        self.first_user.push(NONE);
        self.last_user.push(NONE);
        for d in deps {
            self.add_user(*d, id);
        }
        self.levels = None;
        let height = hot.height as usize;
        if self.queue.len() <= height {
            self.queue.resize_with(height + 1, Vec::new);
        }
        id
    }

    /// Records that `user` uses `node`, at the end of `node`'s list of users.
    fn add_user(&mut self, node: NodeId, user: NodeId) {
        let edge = u32::try_from(self.edges.len()).expect("a graph holds at most u32::MAX edges");
        self.edges.push(Edge {
            user: user.0,
            height: self.hot[user.index()].height,
            next: NONE,
        });
        let n = node.index();
        match self.last_user[n] {
            NONE => self.first_user[n] = edge,
            last => self.edges[last as usize].next = edge,
        }
        self.last_user[n] = edge;
    }

    #[inline]
    fn evaluate(&mut self, hot: Hot) -> Value {
        evaluate(
            hot,
            &self.float_ops,
            &self.float_consts,
            &self.general,
            &self.values,
            &mut self.scratch,
        )
    }

    // ---------------------------------------------------------------- updating

    /// Records a new value for an input. Nothing is recalculated, and reads
    /// return the old value, until [`stabilize`](Self::stabilize) is called.
    pub fn set<T: CalcType>(&mut self, input: Input<T>, value: T) -> Result<(), CalcError> {
        self.check_ref(input.at)?;
        self.pending.insert(input.at.node, value.into_value());
        Ok(())
    }

    /// Records a new value for an input through the untyped API. The value
    /// must have the input's type, or be an error (for example, to say that a
    /// feed has failed).
    pub fn set_value(&mut self, id: NodeId, value: Value) -> Result<(), CalcError> {
        let hot = self.hot.get(id.index()).ok_or(CalcError::UnknownNode(id))?;
        if hot.kind != Kind::Input {
            return Err(CalcError::NotAnInput(self.name(id).to_string()));
        }
        let ty = self.types[id.index()];
        if let Some(found) = value.type_of() {
            if found != ty {
                return Err(CalcError::TypeMismatch {
                    node: self.name(id).to_string(),
                    expected: ty,
                    found: Some(found),
                });
            }
        }
        self.pending.insert(id, value);
        Ok(())
    }

    /// Applies every recorded input change and recalculates what depends on
    /// them, and nothing else.
    pub fn stabilize(&mut self) -> Report {
        self.epoch += 1;
        let epoch = self.epoch;
        self.last_recalculated.clear();
        if self.edges.len() * 4 > self.packed.len() {
            self.repack();
        }
        let mut report = Report {
            epoch,
            inputs_changed: 0,
            recalculated: 0,
            changed: 0,
        };

        for (id, value) in std::mem::take(&mut self.pending) {
            if !self.values[id.index()].same_as(&value) {
                self.values[id.index()] = value;
                self.changed_in[id.index()] = epoch;
                report.inputs_changed += 1;
                self.mark_dependents(id);
            }
        }

        // Lowest height first. Recalculating a node can only mark nodes that
        // are higher, so they are always still ahead of us in this loop.
        for height in 0..self.queue.len() {
            let mut level = std::mem::take(&mut self.queue[height]);
            // A node is queued once for each changed node it uses, so remove
            // repeats. Sorting first also puts the level in id order: the
            // same order however it was filled, and kinder to memory.
            level.sort_unstable();
            level.dedup();
            if self.recalculate_in_parallel(&level) {
                // Nodes on one level never use each other, so they were all
                // worked out at once. Store the changed values, then pass the
                // update on from all of them at once.
                let mut changes = std::mem::take(&mut self.changes);
                let mut changed_ids = std::mem::take(&mut self.changed_ids);
                changed_ids.clear();
                for (&id, change) in level.iter().zip(changes.drain(..)) {
                    let i = id.index();
                    report.recalculated += 1;
                    self.last_recalculated.push(id);
                    if let Some(new_value) = change {
                        self.values[i] = new_value;
                        self.changed_in[i] = epoch;
                        report.changed += 1;
                        changed_ids.push(id);
                    }
                }
                self.mark_dependents_of_all(&changed_ids);
                self.changes = changes;
                self.changed_ids = changed_ids;
            } else {
                for &id in &level {
                    let i = id.index();
                    let new_value = self.evaluate(self.hot[i]);
                    let change = (!self.values[i].same_as(&new_value)).then_some(new_value);
                    self.apply(id, change, &mut report);
                }
            }
            // Hand the emptied list back, so the next update reuses its memory.
            level.clear();
            self.queue[height] = level;
        }
        report
    }

    /// Records that a node was recalculated and, if its value changed
    /// (`change` holds the new value), stores it and passes the update on.
    #[inline]
    fn apply(&mut self, id: NodeId, change: Option<Value>, report: &mut Report) {
        let i = id.index();
        report.recalculated += 1;
        self.last_recalculated.push(id);
        if let Some(new_value) = change {
            self.values[i] = new_value;
            self.changed_in[i] = report.epoch;
            report.changed += 1;
            self.mark_dependents(id);
        }
        // else: cutoff. The value is the same, so nothing above needs to know.
    }

    /// Recalculates a whole level on all cores, if it is big enough to be
    /// worth it, leaving in `self.changes` the new value of each node whose
    /// value changed. Returns false, doing nothing, for a small level.
    #[cfg(feature = "parallel")]
    fn recalculate_in_parallel(&mut self, level: &[NodeId]) -> bool {
        use rayon::prelude::*;
        if level.len() < self.parallel_threshold {
            return false;
        }
        let Graph {
            hot,
            float_ops,
            float_consts,
            general,
            values,
            changes,
            ..
        } = self;
        let (hot, float_ops, float_consts, general, values) =
            (&*hot, &*float_ops, &*float_consts, &*general, &*values);
        level
            .par_iter()
            .with_min_len(512)
            .map_init(Scratch::default, |scratch, id| {
                let i = id.index();
                let new_value = evaluate(hot[i], float_ops, float_consts, general, values, scratch);
                (!values[i].same_as(&new_value)).then_some(new_value)
            })
            .collect_into_vec(changes);
        true
    }

    #[cfg(not(feature = "parallel"))]
    fn recalculate_in_parallel(&mut self, _level: &[NodeId]) -> bool {
        false
    }

    /// Levels with at least this many nodes to recalculate are spread over
    /// all CPU cores. The default suits most graphs; `usize::MAX` turns it
    /// off, and `1` makes every level parallel (the tests use that). Without
    /// the `parallel` feature, everything runs on one core regardless.
    pub fn set_parallel_threshold(&mut self, nodes: usize) {
        self.parallel_threshold = nodes.max(1);
    }

    /// Queues every node that uses `id`. Each edge already holds its user's
    /// height, so this reads only the edges: no lookups in other arrays. A
    /// user can be queued more than once; the level removes repeats.
    fn mark_dependents(&mut self, id: NodeId) {
        let n = id.index();
        if n + 1 < self.packed_start.len() {
            let range = self.packed_start[n] as usize..self.packed_start[n + 1] as usize;
            for &Use { user, height } in &self.packed[range] {
                self.queue[height as usize].push(NodeId(user));
            }
        }
        let mut edge = self.first_user[n];
        while edge != NONE {
            let Edge { user, height, next } = self.edges[edge as usize];
            self.queue[height as usize].push(NodeId(user));
            edge = next;
        }
    }

    /// Packs every link into `packed`, the users of each node back to back.
    ///
    /// Links added since the last repack sit in linked lists, which are cheap
    /// to add to but slow to follow: each step waits for the memory read
    /// before it. `stabilize` repacks once those links reach a quarter of the
    /// packed ones, so each link is copied a constant number of times on
    /// average, however the graph is built.
    fn repack(&mut self) {
        let nodes = self.hot.len();
        let mut start = Vec::with_capacity(nodes + 1);
        let mut packed = Vec::with_capacity(self.packed.len() + self.edges.len());
        start.push(0);
        for n in 0..nodes {
            if n + 1 < self.packed_start.len() {
                let range = self.packed_start[n] as usize..self.packed_start[n + 1] as usize;
                packed.extend_from_slice(&self.packed[range]);
            }
            let mut edge = self.first_user[n];
            while edge != NONE {
                let Edge { user, height, next } = self.edges[edge as usize];
                packed.push(Use { user, height });
                edge = next;
            }
            start.push(u32::try_from(packed.len()).expect("a graph holds at most u32::MAX links"));
        }
        self.packed = packed;
        self.packed_start = start;
        self.edges.clear();
        self.first_user.fill(NONE);
        self.last_user.fill(NONE);
    }

    /// Marks the users of every node in `changed`, on all cores if there are
    /// enough of them.
    #[cfg(feature = "parallel")]
    fn mark_dependents_of_all(&mut self, changed: &[NodeId]) {
        use rayon::prelude::*;
        if changed.len() < self.parallel_threshold {
            for &id in changed {
                self.mark_dependents(id);
            }
            return;
        }
        let (packed_start, packed, first_user, edges) = (
            &self.packed_start,
            &self.packed,
            &self.first_user,
            &self.edges,
        );
        let marked: Vec<Use> = changed
            .par_iter()
            .with_min_len(512)
            .flat_map_iter(|id| {
                let n = id.index();
                let packed: &[Use] = if n + 1 < packed_start.len() {
                    &packed[packed_start[n] as usize..packed_start[n + 1] as usize]
                } else {
                    &[]
                };
                packed.iter().copied().chain(Users {
                    edge: first_user[n],
                    edges,
                })
            })
            .collect();
        for Use { user, height } in marked {
            self.queue[height as usize].push(NodeId(user));
        }
    }

    #[cfg(not(feature = "parallel"))]
    fn mark_dependents_of_all(&mut self, changed: &[NodeId]) {
        for &id in changed {
            self.mark_dependents(id);
        }
    }

    // ---------------------------------------------------------------- reading

    /// The node's value as of the last update, or why it has none.
    pub fn get<T: CalcType>(&self, node: impl Handle<T>) -> Result<T, CalcError> {
        let at = node.node_ref();
        self.check_ref(at)?;
        let value = &self.values[at.node.index()];
        match value {
            Value::Error(message) => Err(CalcError::Evaluation {
                node: self.name(at.node).to_string(),
                message: message.to_string(),
            }),
            _ => T::from_value(value).ok_or_else(|| CalcError::TypeMismatch {
                node: self.name(at.node).to_string(),
                expected: T::TYPE,
                found: value.type_of(),
            }),
        }
    }

    /// The node's value, untyped.
    pub fn value(&self, id: NodeId) -> &Value {
        &self.values[id.index()]
    }

    pub fn name(&self, id: NodeId) -> &str {
        self.names.get(id)
    }

    pub fn find(&self, name: &str) -> Option<NodeId> {
        self.names.find(name)
    }

    pub fn node_type(&self, id: NodeId) -> Type {
        self.types[id.index()]
    }

    pub fn is_input(&self, id: NodeId) -> bool {
        self.hot[id.index()].kind == Kind::Input
    }

    /// The nodes this node uses, in id order, without repeats.
    pub fn dependencies(&self, id: NodeId) -> Vec<NodeId> {
        let hot = self.hot[id.index()];
        let mut deps = Vec::new();
        match hot.kind {
            Kind::Input => {}
            Kind::Float => float_loads(self.float_program(hot), &mut deps),
            Kind::General => self.general[hot.code as usize].loads(&mut deps),
        }
        deps.sort();
        deps.dedup();
        deps
    }

    /// The nodes that use this node, in the order they were added.
    pub fn dependents(&self, id: NodeId) -> Vec<NodeId> {
        let n = id.index();
        let mut users = Vec::new();
        if n + 1 < self.packed_start.len() {
            let range = self.packed_start[n] as usize..self.packed_start[n + 1] as usize;
            users.extend(self.packed[range].iter().map(|u| NodeId(u.user)));
        }
        let mut edge = self.first_user[n];
        while edge != NONE {
            users.push(NodeId(self.edges[edge as usize].user));
            edge = self.edges[edge as usize].next;
        }
        users
    }

    pub fn height(&self, id: NodeId) -> usize {
        self.hot[id.index()].height as usize
    }

    /// The expression a calculated node computes (`None` for inputs). It is
    /// rebuilt from the compiled program, so it is exactly what runs.
    pub fn expression(&self, id: NodeId) -> Option<Ir> {
        let hot = self.hot[id.index()];
        match hot.kind {
            Kind::Input => None,
            Kind::Float => Some(decompile_float(
                self.float_program(hot),
                &self.float_consts,
                self.id,
            )),
            Kind::General => Some(self.general[hot.code as usize].decompile(self.id)),
        }
    }

    fn float_program(&self, hot: Hot) -> &[FloatOp] {
        &self.float_ops[hot.code as usize..(hot.code + hot.len) as usize]
    }

    pub fn len(&self) -> usize {
        self.hot.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hot.is_empty()
    }

    /// The number of the last update (0 before the first).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The nodes recalculated by the last update, in the order it did them.
    pub fn last_recalculated(&self) -> &[NodeId] {
        &self.last_recalculated
    }

    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        (0..self.hot.len()).map(|i| NodeId(i as u32))
    }

    // ---------------------------------------------------------------- checking

    /// Recalculates every node from scratch, without touching the graph, and
    /// compares the result with the values the incremental updates produced.
    /// They must always agree. The tests call this after every update.
    pub fn check_consistency(&self) -> Result<(), String> {
        let mut fresh: Vec<Value> = Vec::with_capacity(self.values.len());
        // Ids are in creation order and a node only uses older nodes, so
        // going through them in order always has every dependency ready.
        for id in self.node_ids() {
            let i = id.index();
            let value = match self.expression(id) {
                None => self.values[i].clone(),
                // Deliberately the simple tree evaluator, not the compiled
                // program, so this check is independent of the fast path.
                // (The tests separately check that rebuilding an expression
                // from its program gives back exactly what was compiled.)
                Some(ir) => ir.eval(&fresh),
            };
            if !value.same_as(&self.values[i]) {
                return Err(format!(
                    "{:?} is {} after incremental updates, but {} when calculated from scratch",
                    self.name(id),
                    self.values[i],
                    value
                ));
            }
            if let Some(ty) = value.type_of() {
                if ty != self.types[i] {
                    return Err(format!(
                        "{:?} is declared {} but holds {}",
                        self.name(id),
                        self.types[i],
                        value
                    ));
                }
            }
            fresh.push(value);
        }
        Ok(())
    }

    /// Recalculates every node from scratch, in place. This is what you would
    /// do without incremental updates; the benchmark compares against it.
    pub fn recalculate_everything(&mut self) -> usize {
        if !cfg!(feature = "parallel") || self.parallel_threshold == usize::MAX {
            // One core: in id order, which reads memory front to back.
            let mut count = 0;
            for i in 0..self.hot.len() {
                let hot = self.hot[i];
                if hot.kind != Kind::Input {
                    self.values[i] = self.evaluate(hot);
                    count += 1;
                }
            }
            return count;
        }
        // All cores: level by level, since nodes on one level never use each other.
        let levels = self.levels.take().unwrap_or_else(|| {
            let mut levels: Vec<Vec<NodeId>> = vec![Vec::new(); self.queue.len()];
            for id in self.node_ids() {
                let hot = self.hot[id.index()];
                if hot.kind != Kind::Input {
                    levels[hot.height as usize].push(id);
                }
            }
            levels
        });
        let mut count = 0;
        for level in &levels {
            count += level.len();
            if self.recalculate_in_parallel(level) {
                let mut changes = std::mem::take(&mut self.changes);
                for (&id, change) in level.iter().zip(changes.drain(..)) {
                    if let Some(value) = change {
                        self.values[id.index()] = value;
                    }
                }
                self.changes = changes;
            } else {
                for &id in level {
                    self.values[id.index()] = self.evaluate(self.hot[id.index()]);
                }
            }
        }
        self.levels = Some(levels);
        count
    }

    // ---------------------------------------------------------------- explaining

    /// Explains why a node has its value: its formula, and the same for
    /// everything it uses, as a tree. Nodes changed by the last update are
    /// marked.
    ///
    /// ```text
    /// total = 43.2  ← subtotal * (1 + tax_rate)  [changed]
    /// ├── subtotal = 36  ← qty * price  [changed]
    /// │   ├── qty = 12  (input)  [changed]
    /// │   └── price = 3  (input)
    /// └── tax_rate = 0.2  (input)
    /// ```
    pub fn explain<T>(&self, node: impl Handle<T>) -> String {
        let at = node.node_ref();
        if at.graph != self.id {
            return "(not a node of this graph)".into();
        }
        self.explain_node(at.node)
    }

    /// [`explain`](Self::explain) for a node given by id, for the untyped API.
    pub fn explain_node(&self, id: NodeId) -> String {
        if id.index() >= self.hot.len() {
            return "(not a node of this graph)".into();
        }
        let mut out = String::new();
        let mut shown = HashSet::new();
        self.explain_into(id, "", "", &mut shown, &mut out);
        out
    }

    fn explain_into(
        &self,
        id: NodeId,
        first: &str,
        rest: &str,
        shown: &mut HashSet<NodeId>,
        out: &mut String,
    ) {
        let _ = write!(out, "{first}{}", self.summary(id));
        let deps = self.dependencies(id);
        if !deps.is_empty() && !shown.insert(id) {
            out.push_str("  (see above)\n");
            return;
        }
        out.push('\n');
        for (k, &dep) in deps.iter().enumerate() {
            let last = k + 1 == deps.len();
            let (branch, indent) = if last {
                ("└── ", "    ")
            } else {
                ("├── ", "│   ")
            };
            self.explain_into(
                dep,
                &format!("{rest}{branch}"),
                &format!("{rest}{indent}"),
                shown,
                out,
            );
        }
    }

    fn summary(&self, id: NodeId) -> String {
        let how = match self.expression(id) {
            None => "(input)".to_string(),
            Some(ir) => format!("← {}", self.render(&ir)),
        };
        let changed = if self.epoch > 0 && self.changed_in[id.index()] == self.epoch {
            "  [changed]"
        } else {
            ""
        };
        format!(
            "{} = {}  {how}{changed}",
            self.name(id),
            self.values[id.index()]
        )
    }

    fn render(&self, ir: &Ir) -> String {
        ir.render(&|id| self.name(id).to_string())
    }

    /// Prints the whole graph, level by level: inputs first, then each node
    /// after everything it uses. `*` marks nodes recalculated by the last
    /// update and `!` the ones whose value changed.
    ///
    /// ```text
    /// level 0
    ///   #0  qty       f64   = 12   input        !
    /// level 1
    ///   #3  subtotal  f64   = 36   qty * price  * !
    /// ```
    pub fn describe(&self) -> String {
        let recalculated: HashSet<NodeId> = self.last_recalculated.iter().copied().collect();
        let rows: Vec<(usize, [String; 5], String)> = self
            .node_ids()
            .map(|id| {
                let how = match self.expression(id) {
                    None => "input".to_string(),
                    Some(ir) => self.render(&ir),
                };
                let mut marks = String::new();
                if recalculated.contains(&id) {
                    marks.push('*');
                }
                if self.epoch > 0 && self.changed_in[id.index()] == self.epoch {
                    marks.push('!');
                }
                let cells = [
                    id.to_string(),
                    self.name(id).to_string(),
                    self.types[id.index()].to_string(),
                    format!("= {}", self.values[id.index()]),
                    how,
                ];
                (self.height(id), cells, marks)
            })
            .collect();

        let mut widths = [0usize; 5];
        for (_, cells, _) in &rows {
            for (w, cell) in widths.iter_mut().zip(cells) {
                *w = (*w).max(cell.chars().count());
            }
        }

        let mut out = format!(
            "{} nodes, after update {}   (* recalculated in the last update, ! value changed)\n",
            self.len(),
            self.epoch
        );
        let max_height = rows.iter().map(|r| r.0).max().unwrap_or(0);
        for height in 0..=max_height {
            let level: Vec<_> = rows.iter().filter(|r| r.0 == height).collect();
            if level.is_empty() {
                continue;
            }
            let _ = writeln!(out, "level {height}");
            for (_, cells, marks) in level {
                let mut line = String::from(" ");
                for (cell, w) in cells.iter().zip(widths) {
                    let _ = write!(line, " {cell:<w$}");
                }
                let _ = write!(line, "  {marks}");
                out.push_str(line.trim_end());
                out.push('\n');
            }
        }
        out
    }

    /// The graph in Graphviz's DOT language, to draw as a picture
    /// (`dot -Tsvg graph.dot -o graph.svg`, or paste it into an online viewer).
    /// Arrows point from a node to the nodes that use it. Inputs are boxes.
    /// After an update: green inputs changed, orange nodes were recalculated
    /// and changed, and grey dashed nodes were recalculated but came out the
    /// same, so the update stopped there.
    pub fn to_dot(&self) -> String {
        let recalculated: HashSet<NodeId> = self.last_recalculated.iter().copied().collect();
        let mut out = String::from(
            "digraph watershed {\n  rankdir=BT;\n  node [fontname=\"Helvetica\", style=filled, fillcolor=white];\n",
        );
        for id in self.node_ids() {
            let changed = self.epoch > 0 && self.changed_in[id.index()] == self.epoch;
            let (shape, how) = match self.expression(id) {
                None => ("box", "input".to_string()),
                Some(ir) => ("ellipse", self.render(&ir)),
            };
            let style = match (self.is_input(id), recalculated.contains(&id), changed) {
                (true, _, true) => ", fillcolor=\"#b2f2bb\"",
                (_, true, true) => ", fillcolor=\"#ffd8a8\"",
                (_, true, false) => ", fillcolor=\"#e9ecef\", style=\"filled,dashed\"",
                _ => "",
            };
            let label = format!(
                "{}: {}\n= {}\n{}",
                self.name(id),
                self.types[id.index()],
                self.values[id.index()],
                how
            );
            let _ = writeln!(
                out,
                "  n{} [shape={shape}, label=\"{}\"{style}];",
                id.0,
                escape(&label)
            );
        }
        for id in self.node_ids() {
            for dep in self.dependencies(id) {
                let _ = writeln!(out, "  n{} -> n{};", dep.0, id.0);
            }
        }
        out.push_str("}\n");
        out
    }

    // ---------------------------------------------------------------- helpers

    fn node_ref(&self, node: NodeId) -> NodeRef {
        NodeRef {
            graph: self.id,
            node,
        }
    }

    fn check_name(&self, name: &str) -> Result<(), CalcError> {
        if self.names.find(name).is_some() {
            return Err(CalcError::DuplicateName(name.to_string()));
        }
        Ok(())
    }

    fn check_ref(&self, r: NodeRef) -> Result<(), CalcError> {
        if r.graph != self.id {
            return Err(CalcError::ForeignNode);
        }
        if r.node.index() >= self.hot.len() {
            return Err(CalcError::UnknownNode(r.node));
        }
        Ok(())
    }

    /// A reference to one of this graph's nodes, for building untyped
    /// expressions to pass to [`define_ir`](Self::define_ir).
    pub fn reference(&self, id: NodeId) -> Ir {
        Ir::Ref(self.node_ref(id))
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Recalculates one node from the values of the nodes it uses.
#[inline]
fn evaluate(
    hot: Hot,
    float_ops: &[FloatOp],
    float_consts: &[f64],
    general: &[GeneralProgram],
    values: &[Value],
    scratch: &mut Scratch,
) -> Value {
    match hot.kind {
        Kind::Float => {
            let start = hot.code as usize;
            eval_float(
                &float_ops[start..start + hot.len as usize],
                float_consts,
                values,
                scratch,
            )
        }
        Kind::General => general[hot.code as usize].eval(values, scratch),
        Kind::Input => unreachable!("inputs are never recalculated"),
    }
}

/// One "is used by" link: `user` uses the node whose list this edge is on.
/// The user's height is kept here too, so passing on an update can queue the
/// user without looking anything else up.
#[derive(Debug, Clone, Copy)]
struct Edge {
    user: u32,
    height: u32,
    next: u32,
}

/// A packed "is used by" link, with the user's height.
#[derive(Debug, Clone, Copy)]
struct Use {
    user: u32,
    height: u32,
}

/// Walks a node's links added since the last repack.
#[cfg(feature = "parallel")]
struct Users<'a> {
    edge: u32,
    edges: &'a [Edge],
}

#[cfg(feature = "parallel")]
impl Iterator for Users<'_> {
    type Item = Use;
    fn next(&mut self) -> Option<Use> {
        if self.edge == NONE {
            return None;
        }
        let Edge { user, height, next } = self.edges[self.edge as usize];
        self.edge = next;
        Some(Use { user, height })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two names with the same 64-bit hash almost never happen by chance, so
    /// force one: make "b"'s hash already point at "a".
    #[test]
    fn a_name_whose_hash_is_taken_is_still_found() {
        let mut names = Names::default();
        names.push("a", NodeId(0));
        let hash_of_b = names.hasher.hash_one("b");
        names.index.insert(hash_of_b, NodeId(0));
        names.push("b", NodeId(1));
        assert_eq!(names.find("a"), Some(NodeId(0)));
        assert_eq!(names.find("b"), Some(NodeId(1)));
        assert_eq!(names.find("c"), None);
        assert_eq!(names.get(NodeId(1)), "b");
    }
}
