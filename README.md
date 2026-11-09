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

Calculations are typed. `qty + region` (a number plus a string) does not
compile, and neither does setting a calculated node.

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
| `crates/watershed-core/tests/behaviour.rs` | 21 tests, each one promise, named as a sentence: `only_nodes_that_depend_on_a_change_are_recalculated`, `an_update_stops_where_a_value_comes_out_the_same`, `a_node_reached_by_two_paths_is_recalculated_once_and_never_sees_half_an_update`, … Read the names: they are the specification. |
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
| recalculate everything | 18.6 ms | 9–11 ms | 1,000,000 |
| one price changes | 0.16 ms | 0.16 ms | ~1,300 |
| 1% of prices change together | 7.4 ms | 7.7 ms | ~68,000 |
| 10% of prices change together | 24 ms | 25 ms | ~353,000 |

It scales to very large graphs, at about 170–190 bytes per node:

| nodes | memory | build | recalculate everything (4 cores) | one price changes |
|---|---|---|---|---|
| 1,000,000 | 184 MB | 0.8 s | 11 ms | 0.16 ms |
| 10,000,000 | 1.8 GB | 10 s | 93 ms | 2.5 ms |
| 50,000,000 | 8.5 GB | 59 s | 444 ms | 17 ms |

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
- **Levels are processed in node-id order**, which reads memory roughly front
  to back.
- **Large levels run on all cores** (the `parallel` feature, on by default).
  Nodes on one level never use each other, so they are recalculated at once;
  marking the nodes above uses atomic flags, so each is queued exactly once.
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
  src/error.rs    errors
  tests/          behaviour and property tests
  examples/       tour and benchmark
crates/watershed-web/   the engine as WebAssembly, with three example models
web/                    the interactive page and its build script
```

## Not done yet

- **Speed:** small and medium updates are limited by waiting on memory, as
  they jump between nodes scattered across the graph; placing nodes that
  change together near each other would help. Building is about 1 µs per
  node.
- **Dynamic dependencies** (a node that changes what it uses), **collections**
  (one node per row, updated per row), **removing nodes**, **pull mode**
  (only calculating what someone is watching).
- **Time travel and full lineage** (values as of an earlier update).
- **Bindings** for Python and the JVM, a formula parser for end users, and the
  cluster.
