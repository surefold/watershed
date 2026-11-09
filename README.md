# watershed

[![CI](https://github.com/matthewjones372/watershed/actions/workflows/ci.yml/badge.svg)](https://github.com/matthewjones372/watershed/actions/workflows/ci.yml)

An incremental calculation engine, in Rust. You build a graph of inputs and
calculated nodes. When inputs change, only the nodes that depend on them are
recalculated, and an update stops spreading wherever a recalculated value comes
out the same as before.

This is the core only: one process, one thread. Distribution, parallelism,
Python and JVM bindings, and GPUs come later and build on it.

```rust
use watershed_core::prelude::*;

let mut g = Graph::new();
let qty = g.input("qty", 10.0)?;
let price = g.input("price", 2.5)?;
let tax = g.input("tax_rate", 0.2)?;

let subtotal = g.define("subtotal", qty * price)?;
let total = g.define("total", subtotal * (1.0 + tax))?;

g.set(price, 3.0)?;
g.stabilize();               // recalculates subtotal and total, nothing else
println!("{}", g.get(total)?);  // 36
println!("{}", g.explain(total));
```

A node with a condition or a choice only waits on what it actually read.
With `when(use_live).then(live).otherwise(end_of_day)`, while `use_live` is
true a change to `end_of_day` recalculates nothing; switch it, and from then
on only `end_of_day` matters. `choose(index, [a, b, c])` picks an option by
position, like a spreadsheet's `CHOOSE`, and waits only on the one it picked.
`g.reads(node)` says what a node read last time, and `explain` follows only
those.

### Collections

A collection is a keyed table of rows, with typed fields, calculated columns
and totals:

```rust
let mut orders = Collection::new(&g, "orders");
let qty = orders.field::<f64>("qty")?;
let price = orders.field::<f64>("price")?;
let line = orders.column(&mut g, "line_total", qty * price * fx)?;
let revenue = orders.sum(&mut g, "revenue", line)?;   // also count, min, max

orders.upsert(&mut g, "o-1042", &[qty.is(5.0), price.is(9.99)])?;
orders.remove(&mut g, "o-7")?;
g.stabilize();
```

Each row is a set of ordinary nodes, so changing one row recalculates that
row's columns and the totals it feeds, and nothing else: on a million rows,
16 µs and 4 nodes per changed row. Totals keep a balanced tree of partial
results, so they update by only the changed rows' paths to the root, and give
exactly, bit for bit, what adding every row up from scratch gives (a running
total that subtracted and added would drift). Removed rows are marked absent
and their nodes reused by the next new row.

```bash
cargo run --release --example collection -- 1000000
```

Totals per group follow rows as they move between groups:

```rust
let by_region = orders.group_sum(&mut g, "revenue_by_region", region, line)?;
let eu = orders.group(&g, by_region, "EU".to_string());   // a node, like any other
orders.group_keys(by_region);                              // groups that have rows
```

Each group's total is a node, made when the first row with that key arrives.
When a row changes group, its old and new groups update by just that row:
on 1,000 rows in 10 groups, a move recalculates 4 nodes.

Calculations are typed. `qty + region` (a number plus a string) does not
compile, and neither does setting a calculated node.

## From Python

`crates/watershed-py` is a Python package with the same engine, built with
PyO3 and managed with [uv](https://docs.astral.sh/uv/):

```python
import watershed as ws

g = ws.Graph()
qty = g.input("qty", 10.0)
price = g.input("price", 2.5)
total = g.define("total", qty * price * 1.2)
g.set(price, 3.0)
g.stabilize()
print(total.value, g.explain(total))
```

```bash
cd crates/watershed-py
uv sync && uv run pytest
```

Calling from Python adds little: on a 210,000-node graph, changing one price
and updating takes about 29 µs from Python.

## Build files

`crates/watershed-build` is a typed build file language for JVM projects
(Kotlin first), with `wsb`, a tool to check, show, explain and watch build
files. Every value in the file is a node, so saving an edit works out only
what it changes, and any value can say where it came from:

```kotlin
val ci = env("CI") == "true"

module core {
    jvmTarget = if (ci) 21 else 17
}

module app {
    dependsOn(core)
    jvmTarget = core.jvmTarget
    dependency("com.fasterxml.jackson.core:jackson-databind:2.17.1")
}
```

```bash
cargo run -p watershed-build --bin wsb -- -f crates/watershed-build/examples/build.ws explain app.jvmTarget
```

A misspelt setting is an error with a suggestion, not a key that is silently
ignored. See [its README](crates/watershed-build/README.md) for the language,
and `editors/watershed-build` for highlighting in IntelliJ and VS Code.

A node's formula can be changed after it is made: `g.redefine(node, expr)`.
Like `set`, it takes effect at the next update, which recalculates the node
and whatever its new value reaches.

## Try it in your browser

`web/` is an interactive page running the real engine, compiled to
WebAssembly: move inputs and watch only the dependent nodes recalculate, ask
any node why it has its value, and time a graph of up to a million nodes on
your own machine.

```bash
cargo install wasm-bindgen-cli --version 0.2.130   # once
rustup target add wasm32-unknown-unknown            # once
web/build.sh
python3 -m http.server -d web/dist 8000             # then open http://localhost:8000
```

## Seeing what it does

```bash
cargo run --example tour
```

builds a small graph, changes an input, and prints:

- `describe()`: every node, level by level, marking what the update
  recalculated (`*`) and what changed (`!`)
- `explain(node)`: why a node has its value, as a tree of what it uses
- `to_dot()`: a picture of the graph, drawn with Graphviz

![The tour graph after price changes](docs/tour.png)

Green: the input that changed. Orange: recalculated, and the value changed.
Grey dashed: recalculated, but the value came out the same, so the update
stopped there and `shipping` was never touched.

## How to check this is correct (without reading Rust)

You don't need to read the engine to trust it. You need to know that the tests
check the right things, and that they would notice if the engine were wrong.

### 1. Run the tests

```bash
cargo test
```

Every line should say `ok`. There are four kinds of test:

| Where | What it checks |
|---|---|
| `crates/watershed-core/tests/behaviour.rs` | 44 tests, each one promise, named as a sentence: `only_nodes_that_depend_on_a_change_are_recalculated`, `an_update_stops_where_a_value_comes_out_the_same`, `a_node_reached_by_two_paths_is_recalculated_once_and_never_sees_half_an_update`, … Read the names: they are the specification. |
| `crates/watershed-core/tests/properties.rs` | Builds thousands of random graphs (with awkward values: NaN, infinity, integer overflow, failed inputs), applies random updates, and after every update checks the rules below. |
| `src/lib.rs` (doc examples) | The example above runs, and two examples that must *not* compile (number + string, setting a calculated node) are checked to fail. |

### 2. The rules the random tests check after every update

1. **Same answer as starting from scratch.** Every value reached by incremental
   updates is identical to recalculating the whole graph from nothing. This is
   the definition of correct, and the from-scratch version is a simple loop
   that is easy to trust.
2. **Nothing recalculated twice** in one update.
3. **Exactly the right nodes recalculated.** A node is recalculated if and only
   if something it uses changed. Not one more (wasted work), not one fewer
   (a stale value).
4. **Inputs are never recalculated**, and the update's report adds up.
5. **Order is right.** A node is only recalculated after everything it uses.

### 3. Proof the tests would catch mistakes

A test suite that always passes proves nothing unless it fails when the code
is wrong. These deliberate bugs were each put into the engine, one at a time,
and the tests failed every time:

| Deliberate bug | Caught |
|---|---|
| No cutoff: always pass an update upward, even when a value is unchanged | ✅ |
| Forget to pass an update upward when a value does change | ✅ |
| Recalculate the highest nodes first instead of the lowest | ✅ |
| Never clear a node's "needs recalculating" flag | ✅ |
| Treat setting an input to its current value as a change | ✅ |
| Work out a node's level from its first input instead of its highest | ✅ |
| Let the type checker accept `&&` on numbers | ✅ |
| Compare floats with `==` for cutoff (so NaN never equals NaN) | ✅ |
| Evaluate the wrong branch of an `if` | ✅ |
| Forget to recalculate a node given a new formula by `redefine` | ✅ |
| Skip a redefined node with an `if` because what it read under its old formula did not change | ✅ |

### 4. Ask for a second opinion

To have the code reviewed independently, ask a reviewer (a person, or a
separate AI session) to check it *without* telling them it is correct. Point
them at `src/graph.rs` (the update loop is `stabilize`) and the five rules
above.

## Speed

```bash
cargo run --release --example benchmark
```

On a graph shaped like a real workload (10,000 market prices → trades → a
tree of totals), on a 4-core machine, median of many runs:

| about 1,000,000 nodes | 1 core | 4 cores | nodes recalculated |
|---|---|---|---|
| recalculate everything | 18 ms | 9.6 ms | 1,000,000 |
| one price changes | 0.13 ms | 0.13 ms | ~1,300 |
| 1% of prices change together | 5.3 ms | 5.5 ms | ~68,000 |
| 10% of prices change together | 16 ms | 15 ms | ~353,000 |

It scales to very large graphs, at about 170–210 bytes per node:

| nodes | peak memory | build | recalculate everything (4 cores) | one price changes |
|---|---|---|---|---|
| 1,000,000 | 213 MB | 0.7 s | 9.6 ms | 0.13 ms |
| 10,000,000 | 1.8 GB | 10 s | 93 ms | 2.5 ms |
| 50,000,000 | 9.9 GB | 57 s | 462 ms | 14 ms |

(One price changing costs more in bigger graphs because each of the 10,000
prices then feeds more trades.)

```bash
cargo run --release --example benchmark -- 25000000   # 25 million trades, ~50 million nodes
```

How it is fast and small:

- **Expressions are compiled** to a flat list of instructions chosen for the
  exact types involved. Pure `f64` arithmetic, the most common case, gets
  8-byte instructions that work on raw numbers, stored back to back for
  every node in one array.
- **No per-node heap allocations** for ordinary nodes: values, a packed
  16-byte record per node, names (one shared string) and the links between
  nodes (linked lists in flat arrays) all live in a few large arrays.
  Expressions are not stored; `explain` rebuilds them from the compiled code.
- **Passing an update on reads one array.** Each node's users are packed back
  to back, and each link carries its user's level, so queuing the users of a
  changed node is one sweep through memory. (Links added since the last pack
  wait in a small side list; the graph repacks once that list reaches a
  quarter of the packed links.) A user reached through several changed nodes
  is queued several times, and the repeats are removed when its level is
  sorted.
- **Levels are processed in node-id order**, which reads memory roughly front
  to back.
- **Large levels run on all cores** (the `parallel` feature, on by default).
  Nodes on one level never use each other, so they are recalculated at once,
  and their users are queued from all cores too.
  Below about 30,000 nodes a level stays on one core: the threads would spend
  longer waiting for each other than working. So parallelism speeds up
  recalculating everything, but small and medium updates, which mostly wait
  on memory, do not gain from it.
- The simple tree-walking evaluator is kept as the reference: the tests check
  the compiled code against it after every update, and check that rebuilding
  an expression from its compiled code gives back exactly what was compiled.

## Layout

```
crates/watershed-core/
  src/value.rs    the value types (f64, i64, bool, string, error)
  src/ir.rs       expressions as data, the type checker and the reference evaluator
  src/compile.rs  compiles expressions to fast instructions
  src/expr.rs     the typed Rust front end: + - * /, comparisons, when/then/otherwise
  src/handle.rs   typed handles to nodes (Input<T>, Node<T>)
  src/graph.rs    the graph and the incremental update (stabilize)
  src/aggregate.rs  totals over many nodes, kept as a tree of partial results
  src/collection.rs keyed tables of rows with columns and totals
  src/error.rs    errors
  tests/          behaviour and property tests
  examples/       tour and benchmark
crates/watershed-build/ a typed build file language on the engine, and the `wsb` tool
crates/watershed-py/    the engine as a Python package (PyO3, maturin, uv)
crates/watershed-web/   the engine as WebAssembly, with three example models
web/                    the interactive page and its build script
editors/watershed-build/  syntax highlighting for build files (IntelliJ, VS Code)
```

## Not done yet

- **Speed:** small and medium updates are limited by waiting on memory, as
  they jump between nodes scattered across the graph; placing nodes that
  change together near each other would help. Building is about 1 µs per
  node.
- **Collections:** grouping by a calculated column (groups are by a field
  today), and building rows faster (about 2.4 µs per node).
- **Removing nodes** in general (collections reuse rows instead, and build
  files redefine a removed value as a constant), and **pull mode** (only
  calculating what someone is watching).
- **Time travel and full lineage** (values as of an earlier update).
- **Bindings** for the JVM, a formula parser for end users, publishing the
  Python package, and the cluster.
