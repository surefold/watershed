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

On a graph of about a million nodes shaped like a real workload (10,000 market
prices → 500,000 trades → a tree of totals), on one core, median of many runs:

| | time | nodes recalculated |
|---|---|---|
| recalculate everything | 28 ms | 1,000,000 |
| one price changes | 0.21 ms | ~1,300 |
| 1% of prices change together | 14 ms | ~68,000 |

One price changing is over 100× faster than recalculating everything. With 1%
changing it is only about 2×, because changes near the bottom of a tree of
totals reach a large share of the totals above them.

How it is fast:

- **Expressions are compiled** to a flat list of instructions chosen for the
  exact types involved. Pure `f64` arithmetic, the most common case, gets its
  own instructions that work on raw numbers, stored back to back for every
  node in one array.
- **What the update loop needs is kept apart** from what it doesn't (names,
  the original expression), in small arrays indexed by node.
- The simple tree-walking evaluator is kept as the reference: the tests check
  the compiled code against it after every update.

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
```

## Not done yet

- **Speed:** recalculating each level in parallel; better memory locality for
  small updates, which are now limited by waiting on memory.
- **Dynamic dependencies** (a node that changes what it uses), **collections**
  (one node per row, updated per row), **removing nodes**, **pull mode**
  (only calculating what someone is watching).
- **Time travel and full lineage** (values as of an earlier update).
- **Bindings** for Python and the JVM, a formula parser for end users, and the
  cluster.
