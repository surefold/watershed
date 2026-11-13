# Implementation notes

How the engine is laid out and why updates cost what they do. For what the
features do, see the [watershed-core guide](../crates/watershed-core/README.md);
for scenario plans, see
[How it runs](../crates/watershed-gpu/README.md#how-it-runs) in the
watershed-gpu README.

## The update algorithm

`Graph::stabilize` in
[`crates/watershed-core/src/graph.rs`](../crates/watershed-core/src/graph.rs)
is height-ordered change propagation with early cutoff and dynamic
dependencies:

- `set` records pending input changes. `stabilize` applies the ones that
  differ from the current value and queues their users, one queue per
  height.
- Heights are processed lowest first. A queued node is recalculated; if its
  new value is the same as the old one, its users are not queued.
- A node with a condition or a choice records what it read. It is skipped
  when none of those changed.
- A level with at least 32,768 queued nodes is recalculated on all cores
  when the `parallel` feature is on.

This is the same family of algorithm as Jane Street's Incremental and the
Shake build system (minimal, early cutoff, dynamic dependencies).

## Memory layout

- **Expressions are compiled** when a node is defined, to a flat list of
  instructions chosen for the types involved
  ([`compile.rs`](../crates/watershed-core/src/compile.rs)). Pure `f64`
  arithmetic, the most common case, gets 8-byte instructions that work on
  raw numbers, stored back to back for every node in one array.
- **No per-node heap allocations** for ordinary nodes. Values, a packed
  16-byte record per node, names (one shared string) and the links between
  nodes (linked lists in flat arrays) live in a few large arrays.
  Expressions are not stored; `explain` rebuilds them from the compiled
  code.
- **Every value is 16 bytes.** Decimals, strings and lists sit behind a
  pointer, so a graph of floats does not pay for them.
- **Propagation reads one array.** Each node's users are packed back to
  back, and each link carries its user's level, so queuing the users of a
  changed node is one sweep through memory. Links added since the last pack
  wait in a side list; the graph repacks once that list reaches a quarter
  of the packed links. A user reached through several changed nodes is
  queued several times, and the repeats are removed when its level is
  sorted.
- **Levels are processed in node-id order**, which reads memory roughly
  front to back. Nodes are stored in the order they are created, so
  creation order affects update time; see
  [node order](benchmarks.md#node-order).

Memory comes to roughly 200 bytes per node in the benchmark graph
(see [benchmarks](benchmarks.md#core-incremental-updates)).

## Parallel recalculation

Nodes on one level never use each other, so a large level is recalculated
on all cores at once, and the users of changed nodes are queued from all
cores too. Below the threshold a level stays on one thread, because the
threads would spend longer coordinating than working. Parallelism therefore
speeds up recalculating everything or a large fraction of the graph. Small
and medium updates are limited by memory access and do not gain from it.

This is parallelism within one update of one set of values. Evaluating
many sets of inputs in parallel is a different mechanism, in
[watershed-gpu](../crates/watershed-gpu/README.md).

## The reference evaluator

The tree-walking evaluator in
[`ir.rs`](../crates/watershed-core/src/ir.rs) is kept as the reference. The
property tests check the compiled code against it after every update, and
check that rebuilding an expression from its compiled code gives back
exactly what was compiled.

## Source layout

```
crates/watershed-core/
  src/value.rs         the value types (f64, i64, decimal, bool, string, list, error)
  src/decimal.rs       exact decimal numbers and rounding modes
  src/ir.rs            expressions as data, the type checker and the reference evaluator
  src/compile.rs       compiles expressions to instructions
  src/expr.rs          the typed Rust front end: + - * /, comparisons, when/then/otherwise
  src/formula.rs       formulas written as text, parsed into expressions
  src/handle.rs        typed handles to nodes (Input<T>, Node<T>)
  src/graph.rs         the graph and the incremental update (stabilize)
  src/graph/keyed.rs   totals per group, and rows moving between groups
  src/graph/pull.rs    pull mode
  src/graph/removal.rs removing nodes
  src/aggregate.rs     totals over many nodes, kept as a tree of partial results
  src/collection.rs    keyed tables of rows with columns and totals
  src/history.rs       values overwritten by updates, for reading earlier ones
  src/sink.rs          passing each update's changes outside the graph
  src/error.rs         errors
  tests/               behaviour, decimal, history, sink and property tests
  examples/            tour, benchmark, collection, history and placement
crates/watershed-gpu/
  src/plan.rs          a scenario plan: cone extraction, levels, slots, shapes, the CPU executor
  src/cuda.rs          the CUDA executor (behind the `cuda` feature)
  src/kernel.cu        the general GPU kernel
  tests/scenarios.rs   plan against graph, CPU against GPU
  examples/            the scenario models and their reference scripts (examples/shared)
crates/watershed-parquet/  a ChangeSink that writes changes to Parquet files
crates/watershed-py/       Python bindings (PyO3, maturin, uv)
crates/watershed-web/      the engine as WebAssembly, with preset models
crates/watershed-grid/     the data grid as WebAssembly
web/                       the interactive page (demo.html) and build script
web/grid/                  the grid's page, renderer, stylesheet and chart
```

## Known costs

- Small and medium updates are limited by memory access. What helps is each
  level's nodes sitting together, which building a graph level by level
  gives. A graph built in another order cannot be renumbered into that
  order, because handles hold node ids.
- Building a graph costs about 1 µs per node and runs on one thread.
- `redefine` and `remove` make the next update rebuild every reverse link.
- A removed node keeps its slot in the arrays.
