//! The graph: nodes, their values, and incremental recalculation.
//!
//! # How an update works
//!
//! 1. [`Graph::set`] only *records* a new value for an input. Reads keep
//!    returning the values from the last update, so what you read is always
//!    consistent with itself.
//! 2. [`Graph::stabilize`] applies every recorded change at once. Each input
//!    whose value really changed marks the nodes that use it as dirty.
//! 3. Dirty nodes are recalculated in order of *height* (an input has height
//!    0, and every other node is one higher than the highest node it uses).
//!    So a node is only ever recalculated after everything it uses is up to
//!    date, and at most once per update, however many of its inputs changed.
//! 4. **Cutoff:** if a recalculated node's value is identical to its old one,
//!    the nodes above it are not marked, so the update stops spreading there.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::compile::{FloatOp, GeneralProgram, Program, Scratch, eval_float};
use crate::error::CalcError;
use crate::expr::IntoExpr;
use crate::handle::{GraphId, Handle, Input, Node, NodeId, NodeRef};
use crate::ir::{Ir, TypeError};
use crate::value::{CalcType, Type, Value};

static NEXT_GRAPH_ID: AtomicU64 = AtomicU64::new(1);

enum Kind {
    Input,
    /// The expression as written.
    Calculated(Arc<Ir>),
}

/// How to recalculate a node, kept small for the update loop.
enum Code {
    Input,
    /// A pure-float program, stored in `Graph::float_ops[start..start + len]`.
    Float {
        start: u32,
        len: u32,
    },
    General(Box<GeneralProgram>),
}

/// The parts of a node the update loop does not need. What it does need is
/// kept in separate compact arrays in `Graph`, so the loop reads only those.
struct Meta {
    name: String,
    kind: Kind,
    ty: Type,
    /// Nodes this one uses, without repeats.
    deps: Vec<NodeId>,
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
pub struct Graph {
    id: GraphId,
    meta: Vec<Meta>,

    // One entry per node, indexed by node id: what the update loop uses.
    values: Vec<Value>,
    code: Vec<Code>,
    /// Nodes that use this one.
    dependents: Vec<Vec<NodeId>>,
    height: Vec<u32>,
    dirty: Vec<bool>,
    /// The update in which the value last changed (0 = when it was created).
    changed_in: Vec<u64>,
    /// The instructions of every pure-float program, back to back.
    float_ops: Vec<FloatOp>,

    by_name: HashMap<String, NodeId>,
    /// Input changes recorded by `set`, applied by `stabilize`. A later `set`
    /// of the same input replaces an earlier one.
    pending: BTreeMap<NodeId, Value>,
    /// Dirty nodes waiting to be recalculated, one list per height.
    queue: Vec<Vec<NodeId>>,
    epoch: u64,
    last_recalculated: Vec<NodeId>,
    /// Scratch space for evaluating programs.
    scratch: Scratch,
}

impl Default for Graph {
    fn default() -> Self {
        Self::new()
    }
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            id: GraphId(NEXT_GRAPH_ID.fetch_add(1, Ordering::Relaxed)),
            meta: Vec::new(),
            values: Vec::new(),
            code: Vec::new(),
            dependents: Vec::new(),
            height: Vec::new(),
            dirty: Vec::new(),
            changed_in: Vec::new(),
            float_ops: Vec::new(),
            by_name: HashMap::new(),
            pending: BTreeMap::new(),
            queue: Vec::new(),
            epoch: 0,
            last_recalculated: Vec::new(),
            scratch: Scratch::default(),
        }
    }

    // ---------------------------------------------------------------- building

    /// Adds an input with a starting value.
    pub fn input<T: CalcType>(&mut self, name: &str, value: T) -> Result<Input<T>, CalcError> {
        self.check_name(name)?;
        let id = self.push(
            name,
            Kind::Input,
            Code::Input,
            T::TYPE,
            Vec::new(),
            value.into_value(),
        );
        Ok(Input::new(self.node_ref(id)))
    }

    /// Adds an input through the untyped API. Its type is the type of `value`,
    /// which therefore cannot be an error.
    pub fn input_value(&mut self, name: &str, value: Value) -> Result<NodeId, CalcError> {
        self.check_name(name)?;
        let ty = value
            .type_of()
            .ok_or_else(|| TypeError("an input's starting value cannot be an error".into()))?;
        Ok(self.push(name, Kind::Input, Code::Input, ty, Vec::new(), value))
    }

    /// Adds a calculated node. Its value is worked out straight away.
    pub fn define<T: CalcType>(
        &mut self,
        name: &str,
        expr: impl IntoExpr<T>,
    ) -> Result<Node<T>, CalcError> {
        let expr = expr.into_expr();
        let id = self.define_ir(name, (*expr.ir).clone())?;
        let ty = self.meta[id.index()].ty;
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
            self.meta
                .get(r.node.index())
                .map(|m| m.ty)
                .ok_or_else(|| TypeError(format!("there is no node {}", r.node)))
        })?;

        let mut deps: Vec<NodeId> = refs.into_iter().map(|r| r.node).collect();
        deps.sort();
        deps.dedup();
        // Worked out from the values of the last update, like every other node.
        let code = match Program::compile(&ir, &|i| self.meta[i].ty) {
            Program::Float(ops) => {
                let start = u32::try_from(self.float_ops.len()).expect("too many instructions");
                self.float_ops.extend(ops);
                let len = self.float_ops.len() as u32 - start;
                Code::Float { start, len }
            }
            Program::General(program) => Code::General(Box::new(program)),
        };
        let value = evaluate(&code, &self.float_ops, &self.values, &mut self.scratch);
        let id = self.push(
            name,
            Kind::Calculated(Arc::new(ir)),
            code,
            ty,
            deps.clone(),
            value,
        );
        for d in deps {
            self.dependents[d.index()].push(id);
        }
        Ok(id)
    }

    fn push(
        &mut self,
        name: &str,
        kind: Kind,
        code: Code,
        ty: Type,
        deps: Vec<NodeId>,
        value: Value,
    ) -> NodeId {
        // Inputs have height 0; any other node is one above the highest node
        // it uses (1 if it uses none).
        let height = match code {
            Code::Input => 0,
            _ => deps
                .iter()
                .map(|d| self.height[d.index()] as usize + 1)
                .max()
                .unwrap_or(1),
        };
        let id =
            NodeId(u32::try_from(self.meta.len()).expect("a graph holds at most u32::MAX nodes"));
        self.meta.push(Meta {
            name: name.to_string(),
            kind,
            ty,
            deps,
        });
        self.values.push(value);
        self.code.push(code);
        self.dependents.push(Vec::new());
        self.height
            .push(u32::try_from(height).expect("graph too deep"));
        self.dirty.push(false);
        self.changed_in.push(self.epoch);
        self.by_name.insert(name.to_string(), id);
        if self.queue.len() <= height {
            self.queue.resize_with(height + 1, Vec::new);
        }
        id
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
        let meta = self
            .meta
            .get(id.index())
            .ok_or(CalcError::UnknownNode(id))?;
        if !matches!(meta.kind, Kind::Input) {
            return Err(CalcError::NotAnInput(meta.name.clone()));
        }
        if let Some(found) = value.type_of() {
            if found != meta.ty {
                return Err(CalcError::TypeMismatch {
                    node: meta.name.clone(),
                    expected: meta.ty,
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
            let level = std::mem::take(&mut self.queue[height]);
            for id in level {
                let i = id.index();
                self.dirty[i] = false;
                let new_value = evaluate(
                    &self.code[i],
                    &self.float_ops,
                    &self.values,
                    &mut self.scratch,
                );
                report.recalculated += 1;
                self.last_recalculated.push(id);

                if !self.values[i].same_as(&new_value) {
                    self.values[i] = new_value;
                    self.changed_in[i] = epoch;
                    report.changed += 1;
                    self.mark_dependents(id);
                }
                // else: cutoff. The value is the same, so nothing above needs to know.
            }
        }
        report
    }

    fn mark_dependents(&mut self, id: NodeId) {
        for &dependent in &self.dependents[id.index()] {
            let d = dependent.index();
            if !self.dirty[d] {
                self.dirty[d] = true;
                self.queue[self.height[d] as usize].push(dependent);
            }
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
        &self.meta[id.index()].name
    }

    pub fn find(&self, name: &str) -> Option<NodeId> {
        self.by_name.get(name).copied()
    }

    pub fn node_type(&self, id: NodeId) -> Type {
        self.meta[id.index()].ty
    }

    pub fn is_input(&self, id: NodeId) -> bool {
        matches!(self.meta[id.index()].kind, Kind::Input)
    }

    /// The nodes this node uses.
    pub fn dependencies(&self, id: NodeId) -> &[NodeId] {
        &self.meta[id.index()].deps
    }

    /// The nodes that use this node.
    pub fn dependents(&self, id: NodeId) -> &[NodeId] {
        &self.dependents[id.index()]
    }

    pub fn height(&self, id: NodeId) -> usize {
        self.height[id.index()] as usize
    }

    /// The expression a calculated node computes (`None` for inputs).
    pub fn expression(&self, id: NodeId) -> Option<&Ir> {
        match &self.meta[id.index()].kind {
            Kind::Input => None,
            Kind::Calculated(ir) => Some(ir),
        }
    }

    pub fn len(&self) -> usize {
        self.meta.len()
    }

    pub fn is_empty(&self) -> bool {
        self.meta.is_empty()
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
        (0..self.meta.len()).map(|i| NodeId(i as u32))
    }

    // ---------------------------------------------------------------- checking

    /// Recalculates every node from scratch, without touching the graph, and
    /// compares the result with the values the incremental updates produced.
    /// They must always agree. The tests call this after every update.
    pub fn check_consistency(&self) -> Result<(), String> {
        let mut fresh: Vec<Value> = Vec::with_capacity(self.values.len());
        // Ids are in creation order and a node only uses older nodes, so
        // going through them in order always has every dependency ready.
        for (i, meta) in self.meta.iter().enumerate() {
            let value = match &meta.kind {
                Kind::Input => self.values[i].clone(),
                // Deliberately the simple tree evaluator, not the compiled
                // program, so this check is independent of the fast path.
                Kind::Calculated(ir) => ir.eval(&fresh),
            };
            if !value.same_as(&self.values[i]) {
                return Err(format!(
                    "{:?} is {} after incremental updates, but {} when calculated from scratch",
                    meta.name, self.values[i], value
                ));
            }
            if let Some(ty) = value.type_of() {
                if ty != meta.ty {
                    return Err(format!(
                        "{:?} is declared {} but holds {}",
                        meta.name, meta.ty, value
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
        let mut count = 0;
        for i in 0..self.code.len() {
            if !matches!(self.code[i], Code::Input) {
                self.values[i] = evaluate(
                    &self.code[i],
                    &self.float_ops,
                    &self.values,
                    &mut self.scratch,
                );
                count += 1;
            }
        }
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
        if id.index() >= self.meta.len() {
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
        let meta = &self.meta[id.index()];
        let how = match &meta.kind {
            Kind::Input => "(input)".to_string(),
            Kind::Calculated(ir) => format!("← {}", self.render(ir)),
        };
        let changed = if self.epoch > 0 && self.changed_in[id.index()] == self.epoch {
            "  [changed]"
        } else {
            ""
        };
        format!(
            "{} = {}  {how}{changed}",
            meta.name,
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
                let meta = &self.meta[id.index()];
                let how = match &meta.kind {
                    Kind::Input => "input".to_string(),
                    Kind::Calculated(ir) => self.render(ir),
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
                    meta.name.clone(),
                    meta.ty.to_string(),
                    format!("= {}", self.values[id.index()]),
                    how,
                ];
                (self.height[id.index()] as usize, cells, marks)
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
            let meta = &self.meta[id.index()];
            let changed = self.epoch > 0 && self.changed_in[id.index()] == self.epoch;
            let (shape, how) = match &meta.kind {
                Kind::Input => ("box", "input".to_string()),
                Kind::Calculated(ir) => ("ellipse", self.render(ir)),
            };
            let style = match (&meta.kind, recalculated.contains(&id), changed) {
                (Kind::Input, _, true) => ", fillcolor=\"#b2f2bb\"",
                (_, true, true) => ", fillcolor=\"#ffd8a8\"",
                (_, true, false) => ", fillcolor=\"#e9ecef\", style=\"filled,dashed\"",
                _ => "",
            };
            let label = format!(
                "{}: {}\n= {}\n{}",
                meta.name,
                meta.ty,
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
        if self.by_name.contains_key(name) {
            return Err(CalcError::DuplicateName(name.to_string()));
        }
        Ok(())
    }

    fn check_ref(&self, r: NodeRef) -> Result<(), CalcError> {
        if r.graph != self.id {
            return Err(CalcError::ForeignNode);
        }
        if r.node.index() >= self.meta.len() {
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
fn evaluate(code: &Code, float_ops: &[FloatOp], values: &[Value], scratch: &mut Scratch) -> Value {
    match code {
        Code::Float { start, len } => {
            let start = *start as usize;
            eval_float(&float_ops[start..start + *len as usize], values, scratch)
        }
        Code::General(program) => program.eval(values, scratch),
        Code::Input => unreachable!("inputs are never recalculated"),
    }
}
