# Watershed

[![CI](https://github.com/surefold/watershed/actions/workflows/ci.yml/badge.svg)](https://github.com/surefold/watershed/actions/workflows/ci.yml)

Watershed is a computation engine built around a persistent dependency
graph. You define a model once, as a graph of inputs and calculated nodes,
and run it in two ways:

1. **Incremental recomputation.** When inputs change, the graph recalculates
   only the nodes that depend on them, and stops propagating wherever a
   recalculated value is the same as before.
2. **Parallel scenario evaluation.** A plan compiled from the graph
   evaluates many sets of inputs against the same model, across CPU cores or
   on an NVIDIA GPU.

The first answers "what is the value now that this input has changed?" in
time proportional to what the change affects. The second answers "what would
the values be under each of these ten thousand input sets?" using every core
or a GPU. The engine is written in Rust, with Python
bindings, a WebAssembly build and a separately maintained JVM API.

Scenario evaluation covers a subset of what the graph supports: `f64` and
`bool` nodes with arithmetic, comparisons, logic, conditionals and a few
maths functions. See [Limitations](#limitations).

## Why Watershed?

A model that must both react to changes and be run over many scenarios is
often implemented twice: once in a system that tracks dependencies, and
again as an array program for batch runs. The two copies then have to be
kept in agreement.

Watershed keeps one definition. The graph that updates incrementally when
an input changes is the same graph a scenario plan is compiled from, and
each scenario's result is what the graph would give if those inputs were set
on it and it were updated. For the supported operations that agreement is
bit-for-bit on the CPU, with a short list of
[documented exceptions on the GPU](#limitations), and it is checked by
tests.

Compared with a calculation engine that recalculates everything, or
everything downstream of a change:

- Dependencies are tracked per node and are dynamic. A node with a
  conditional depends only on the branch it took.
- Propagation stops at any node whose recalculated value is unchanged.
- Every value can explain itself: `explain` prints the tree of values a node
  was computed from, optionally as of an earlier update.
- The same graph can be compiled to a plan and run for many input sets
  without modifying the graph.

It is not the fastest tool for every job. A whole-model valuation from
nothing is faster in a vectorised array program, and code written by hand
for one model beats the general engine. [Performance](#performance) gives
measured examples of both.

## Quick start

The crates are not published on crates.io. Depend on the repository:

```toml
[dependencies]
watershed-core = { git = "https://github.com/surefold/watershed" }
```

```rust
use watershed_core::prelude::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut g = Graph::new();

    // Inputs.
    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let tax = g.input("tax_rate", 0.2)?;

    // Derived calculations.
    let subtotal = g.define("subtotal", qty * price)?;
    let total = g.define("total", subtotal * (1.0 + tax))?;
    assert_eq!(g.get(total)?, 30.0);

    // Change an input, then stabilise.
    g.set(price, 3.0)?;
    let report = g.stabilize();

    assert_eq!(g.get(total)?, 36.0);
    assert_eq!(report.recalculated, 2); // subtotal and total; qty and tax_rate were not visited
    println!("{}", g.explain(total)); // the value of `total`, as a tree of what it read
    Ok(())
}
```

Expressions are typed: adding a number to a string does not compile, and
neither does setting a calculated node.

To evaluate the same graph for many input sets, add `watershed-gpu` (which
runs on CPU threads unless its `cuda` feature is enabled) and continue:

```rust
use watershed_gpu::Scenarios;

// Vary `price`; read `total`. The plan copies what `total` depends on.
let plan = Scenarios::new(&g, &[price.id()], &[total.id()])?;
let totals = plan.run_cpu(&[2.5, 3.0, 4.0]); // three scenarios, on all cores
assert_eq!(totals, [30.0, 36.0, 48.0]);
assert_eq!(g.get(total)?, 36.0); // the graph is untouched
```

`cargo run --example tour` prints a small graph before and after an update,
with what was recalculated and why.

## How it works

**The graph.** Nodes are inputs or expressions over other nodes. Each node
has a height: one more than the highest node it uses. Values, compiled
expressions and dependency links are stored in flat arrays indexed by node
id.

**An update.** `set` records a change; `stabilize` applies all pending
changes. It processes heights lowest first, so each affected node is
recalculated once, after everything it uses. If a node's new value equals
its old one, its users are not queued (early cutoff). A node with a
conditional records which nodes it read and is skipped if none of them
changed. With the `parallel` feature (on by default), a level with tens of
thousands of affected nodes is recalculated across cores; smaller updates
run on one thread.

**A scenario plan.** `Scenarios` walks back from the requested outputs to
find the part of the graph they depend on, classifies each node as
per-scenario, shared across scenarios or constant, and translates the
per-scenario nodes into a flat program ordered by level. Nodes are assigned
to reusable slots by liveness, so a scenario needs memory for the values
live at one time, not for every node. The plan runs one scenario per task on
CPU threads, or all scenarios level by level on a GPU using kernels
generated for the plan. Plans are compiled once and rerun with new inputs.

These are two separate mechanisms. Parallelism inside `stabilize` speeds up
one large update of one set of values. A scenario plan evaluates many sets
of values and does not use or change the graph's current state.

More detail: [docs/internals.md](docs/internals.md).

## Capabilities

| Capability | Summary | Details |
|---|---|---|
| Incremental updates | Height-ordered propagation with early cutoff and dynamic dependencies for `when`/`choose` | [core guide](crates/watershed-core/README.md#the-update-model) |
| Value types | `f64`, `i64`, exact `Decimal` (28 digits), `bool`, strings, lists of strings; errors are values that propagate | [core guide](crates/watershed-core/README.md#the-update-model) |
| Collections | Keyed tables of rows with typed fields, calculated columns and totals (`sum`, `count`, `min`, `max`) that update by the changed rows | [collections](crates/watershed-core/README.md#collections) |
| Grouping | Per-group totals that follow rows between groups, including groups keyed by a calculated column | [grouping](crates/watershed-core/README.md#grouping) |
| Exact decimals | Decimal nodes with explicit rounding modes; no implicit mixing with floats | [decimals](crates/watershed-core/README.md#exact-decimals) |
| History | Read any node or row as of an earlier update; `explain_at`; change sinks, including Parquet | [history](crates/watershed-core/README.md#history-and-change-tracking) |
| Pull mode | Recalculate only watched nodes and what they use | [pull mode](crates/watershed-core/README.md#pull-mode) |
| Model edits | `redefine` a node's expression; `remove` an unused node | [redefining and removing](crates/watershed-core/README.md#redefining-and-removing-nodes) |
| Text formulas | Parse `qty * price * (1 + tax)` into the same type-checked expressions | [formulas](crates/watershed-core/README.md#formulas-as-text) |
| Inspection | `explain`, `describe`, Graphviz output, consistency checks | [inspecting](crates/watershed-core/README.md#inspecting-a-graph) |
| Scenario evaluation | Many input sets through one plan on CPU threads or CUDA; shared inputs that can be replaced without recompiling; plans kept resident on the GPU | [watershed-gpu](crates/watershed-gpu/README.md) |

## Performance

Two results, one for each execution pattern, both on the same insurance
model: lifelib's [`savings.CashValue_ME_EX2`](https://lifelib.io/libraries/savings/index.html)
(savings policies with guarantees, projected month by month), built as a graph of
19.9 million nodes for 20,000 model points. The model points represent
1,009,152 policies; the computation is over the 20,000. Machine: Ryzen 9
5950X (16 cores, 32 threads), RTX 3090 (24 GB), double precision throughout.
Runs of 13 November 2026.

**Incremental recomputation** (no GPU, one scenario), against lifelib
running the same model on the same machine. All figures measured:

| | lifelib | Watershed graph |
|---|---|---|
| First valuation | 3.6 s | 27.7 s, of which 22.5 s is graph construction |
| Every input changes | 3.6 s | 5.2 s |
| One model point changes | 2.3 s | 56 µs |

After one model point changes, the graph recalculates about 570 of its 19.9
million nodes (mean of 1,000 amendments). The two implementations agree on
the result to a relative 3.7×10⁻¹⁵. Reading this table fairly:

- lifelib is faster for a first valuation and when everything changes.
- lifelib's engine also tracks dependencies. It recomputes whole columns
  because its formulas are vectorised over model points. Valuing only the
  changed model point with lifelib by hand takes 0.29 s.
- A loop written for this one model, revaluing the changed model point and
  adjusting a running total, would be faster than the graph. That was not
  measured, and neither were other incremental engines.

**Scenario evaluation**: the same model over 10,000 market scenarios.

| | Time | Basis |
|---|---|---|
| Watershed plan, GPU | 10.0 s | measured; nine runs, 9.4 to 10.0 s |
| Watershed plan, 32 CPU threads | about 7 minutes | extrapolated from 128 scenarios at 44 ms each |
| Hand-written JAX program, same GPU, model points sorted by remaining term | 6.2 s | measured; five runs |
| Hand-written JAX program, same GPU, unsorted | 13.1 s | measured; five runs |
| lifelib, one CPU core | about 4.9 hours | extrapolated from 200 model points × 1,000 scenarios |

The Watershed times exclude one-off setup: 22.6 s to construct the graph and
18.0 s to compile the plan. The plan agrees with lifelib to a relative
3.1×10⁻¹³ over about 3 million outputs, checked on smaller configurations
than the timed run. A JAX program written for this model with straightforward
tuning is about 1.6 times faster than the plan.

The GPU pays for large, deep graphs run over many scenarios. For a graph of
a few dozen nodes evaluated over millions of scenarios, transfer to and from
the device dominates and the plan on CPU threads is as fast, unless the data
is kept resident on the GPU. On two other models, hand-written parallel CPU
loops were 3 to 8 times faster than the plan on the CPU.

Methodology, the remaining examples, what each was checked against, and how
to reproduce them: [docs/benchmarks.md](docs/benchmarks.md) and the
[watershed-gpu README](crates/watershed-gpu/README.md#what-it-has-been-tried-on).

## Architecture and bindings

| Component | Location | Role |
|---|---|---|
| Watershed Core | [`crates/watershed-core`](crates/watershed-core/README.md) | The graph, incremental update, collections, decimals, history, formulas. No `unsafe`. The `parallel` feature (default) uses rayon to recalculate large levels across cores. |
| Watershed GPU | [`crates/watershed-gpu`](crates/watershed-gpu/README.md) | Scenario plans. Runs on CPU threads by default; the `cuda` feature (off by default) adds the NVIDIA GPU executor. CUDA libraries are located at run time, so the workspace builds without them. |
| Parquet sink | [`crates/watershed-parquet`](crates/watershed-parquet) | Writes each update's changes to Parquet files on a background thread. |
| Python bindings | [`crates/watershed-py`](crates/watershed-py/README.md) | A PyO3 package over the core: graphs, collections, groupings, decimals, formulas, pull mode. Does not expose history, sinks or scenario evaluation. Built from source with uv; not on PyPI. |
| WebAssembly | [`crates/watershed-web`](crates/watershed-web), [`web/`](web) | The core compiled to WebAssembly (single-threaded) behind an interactive page. |
| Data grid | [`crates/watershed-grid`](crates/watershed-grid/README.md), [`web/grid/`](web/grid) | A browser data grid in which every cell is a graph node: formula columns, totals, filtering, sorting, grouping, history. |
| JVM API | [surefold/watershed-jvm](https://github.com/surefold/watershed-jvm) | Watershed for Kotlin and Scala 3, maintained and released in its own repository. Its README documents its API, capabilities and limitations. |

Feature flags that affect execution:

| Crate | Feature | Default | Effect |
|---|---|---|---|
| `watershed-core` | `parallel` | on | Multi-threaded recalculation of large levels within one update |
| `watershed-gpu` | `cuda` | off | GPU execution of scenario plans; without it, plans run on CPU threads |

[surefold/wsb](https://github.com/surefold/wsb), a typed build-file language
for JVM projects, is a separate project that uses the engine.

## Documentation

| Topic | Where |
|---|---|
| Core API, features and examples | [crates/watershed-core/README.md](crates/watershed-core/README.md); `cargo doc --open -p watershed-core` |
| Collections and grouping | [core guide: collections](crates/watershed-core/README.md#collections), [grouping](crates/watershed-core/README.md#grouping) |
| Exact decimals | [core guide: exact decimals](crates/watershed-core/README.md#exact-decimals) |
| History and change tracking | [core guide: history](crates/watershed-core/README.md#history-and-change-tracking); `cargo doc --open -p watershed-parquet` |
| GPU and CPU scenario evaluation | [crates/watershed-gpu/README.md](crates/watershed-gpu/README.md) |
| Python API | [crates/watershed-py/README.md](crates/watershed-py/README.md), [type stubs](crates/watershed-py/watershed.pyi) |
| JVM API (Kotlin, Scala 3) | [surefold/watershed-jvm](https://github.com/surefold/watershed-jvm) |
| WebAssembly and the data grid | [crates/watershed-grid/README.md](crates/watershed-grid/README.md) |
| Benchmarks and reproducibility | [docs/benchmarks.md](docs/benchmarks.md) |
| Implementation architecture | [docs/internals.md](docs/internals.md) |
| Testing and correctness | [docs/testing.md](docs/testing.md) |
| Known limitations | [below](#limitations), and the "Not done yet" section of the [watershed-gpu README](crates/watershed-gpu/README.md#not-done-yet) |

## Limitations

**Scenario evaluation supports a subset of the graph.**

- Supported: `f64` and `bool` nodes using `+ - * /`, `min`, `max`, `abs`,
  `exp`, `ln`, `sqrt`, `pow`, comparisons, and/or/not, and conditionals.
- Refused when the plan is compiled, with the node's name: `i64`, decimals,
  text, lists, `choose`, and aggregate totals (so collections' `sum`,
  `count`, `min`, `max` and groupings).
- A model written with collections, decimals or `choose` must be rewritten
  without them to run as scenarios. The examples build their own trees of
  sums.
- A table cannot be looked up by a varied input.
- CPU plan results are bit-identical to the graph's. GPU results are too,
  except that a NaN may carry a different payload, `min`/`max` of `0.0` and
  `-0.0` follow x86-64, and `exp`, `ln` and `pow` may differ in the last one
  or two digits. This is established by tests, not by proof.
- The GPU executor is CUDA only. Graph construction and most of plan
  compilation are single-threaded, at about 1 µs per node each.

**Core.**

- One graph lives in one process. There is no distributed or clustered
  execution.
- Small and medium updates are limited by memory access; multi-threading
  helps only when a large part of the graph is recalculated.
- `remove` frees a node's name and work but not its memory. A removed
  collection row's nodes are reused by the next new row, but its group
  membership nodes are kept. Groupings over large tables are
  memory-intensive.
- `redefine` and `remove` cause the next update to rebuild the graph's
  reverse links, in time proportional to graph size.
- In pull mode, nodes that a collection grouping is built on are always
  calculated.
- History cannot restore the graph to an earlier update, does not keep a
  redefined node's earlier expressions, and does not cover a collection's
  groups.
- Decimals hold 28 significant digits.

**Bindings and front ends.**

- The Python package is not published and does not expose history, sinks,
  `redefine` or scenario evaluation.
- The grid has no decimal columns, and falls back to summing groups outside
  the graph for large grouped tables.
- The Rust crates are not published on crates.io.

## Building, testing and licence

Requires Rust 1.85 or later.

```bash
cargo build --workspace
cargo test --workspace                           # no GPU or CUDA needed
cargo test -p watershed-gpu --features cuda      # GPU tests; needs the CUDA toolkit and an NVIDIA GPU
cargo run --example tour
cargo run --release --example benchmark

cd crates/watershed-py && uv sync && uv run pytest    # Python bindings
web/build.sh                                          # WebAssembly pages; see the grid README
```

CI runs formatting, clippy, the tests on stable and Rust 1.85, the
documentation build, three examples, a WebAssembly build and the Python
tests. It does not run the GPU tests.

Licensed under the Apache License 2.0; see [`LICENSE`](LICENSE). Work
derived from other projects is listed in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
