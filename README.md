# Watershed

[![CI](https://github.com/surefold/watershed/actions/workflows/ci.yml/badge.svg)](https://github.com/surefold/watershed/actions/workflows/ci.yml)
[![Licence: Apache 2.0](https://img.shields.io/badge/licence-Apache%202.0-blue.svg)](LICENSE)

**Describe a calculation once. Choose how it runs afterwards.**

Watershed is a computation engine. You write a model as typed formulas, and
it records what each one depends on. From that one description it updates
only what a change touches, runs millions of scenarios on CPU threads or
GPUs with no GPU code from you, and brings a finished batch up to date
without running it again.

On one insurance model of 19.9 million calculations, written once:

| | |
|---|---|
| **58 µs** | to update after one record changes; 570 calculations rerun |
| **10.2 s** | for 10,000 scenarios on one GPU |
| **0.2 s** | to bring all 10,000 results up to date after a change, against 11.2 s to rerun |
| **70 s** | for 2,000,000 scenarios on eight GPUs |

**[Try it in your browser](https://surefold.dev/watershed/)** ·
[Guide](https://surefold.dev/guides/watershed/) ·
[Benchmark study](https://surefold.dev/bench/watershed-gpu/) ·
[Eight-GPU study](https://surefold.dev/bench/watershed-cluster/)

[![The engine, compiled to WebAssembly, on a small model of an order. One input changes; the formulas it reaches are recalculated, an update stops where a value comes out the same, and the rest are not looked at. Then the same model runs as a plan for three prices.](docs/demo.gif)](https://surefold.dev/guides/watershed/#try)

That is the engine itself, compiled to WebAssembly, not a drawing of it:
the [guide](https://surefold.dev/guides/watershed/#try) has it live.

## A first example

```rust
use watershed_core::prelude::*;
use watershed_gpu::Scenarios;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut g = Graph::new();
    let qty = g.input("qty", 10.0)?;
    let price = g.input("price", 2.5)?;
    let tax = g.input("tax_rate", 0.2)?;
    let subtotal = g.define("subtotal", qty * price)?;
    let total = g.define("total", subtotal * (1.0 + tax))?;

    // Change one input. Only what depends on it is recalculated.
    g.set(price, 3.0)?;
    let report = g.stabilize();
    assert_eq!(g.get(total)?, 36.0);
    assert_eq!(report.recalculated, 2); // subtotal and total; qty and tax_rate were not visited
    println!("{}", g.explain(total));

    // The same model for three prices at once, on every core. The graph is not touched.
    let plan = Scenarios::new(&g, &[price.id()], &[total.id()])?;
    assert_eq!(plan.run_cpu(&[2.5, 3.0, 4.0]), [30.0, 36.0, 48.0]);
    assert_eq!(g.get(total)?, 36.0);
    Ok(())
}
```

```text
total = 36  ← subtotal * (1 + tax_rate)  [changed]
├── tax_rate = 0.2  (input)
└── subtotal = 30  ← qty * price  [changed]
    ├── qty = 10  (input)
    └── price = 3  (input)  [changed]
```

Expressions are typed: adding a number to a string does not compile, and
neither does setting a calculated node. With the `cuda` feature,
`Gpu::new()?.run(&plan, &inputs)` runs the same plan on a GPU.

## Get started

Everything is pre-release and built from source, so each route needs a
[Rust toolchain](https://rustup.rs) (1.85 or later). Nothing is published to
crates.io or PyPI yet.

**Rust.** Add the crates and run the example above:

```toml
[dependencies]
watershed-core = { git = "https://github.com/surefold/watershed" }
watershed-gpu  = { git = "https://github.com/surefold/watershed" }   # scenario plans; CPU threads by default
```

For both ways of running on a model of realistic size, with no data files:

```bash
cargo run --release -p watershed-gpu --example loans -- 2000 1000     # 2,000 loans, 1,000 scenarios, on CPU threads
cargo run --release -p watershed-gpu --features cuda --example loans  # 20,000 × 10,000 on an NVIDIA GPU
```

**Python.** The incremental engine, collections and formulas as text;
scenario plans are not exposed yet. Python 3.9 or later.

```bash
pip install "git+https://github.com/surefold/watershed#subdirectory=crates/watershed-py"
```

```python
import watershed as ws

g = ws.Graph()
qty = g.input("qty", 10.0)
price = g.input("price", 2.5)
total = g.define("total", qty * price * 1.2)

g.set(price, 3.0)
g.stabilize()
print(total.value)       # 36.0
print(g.explain(total))  # the tree behind it
```

More in the [Python guide](crates/watershed-py/README.md).

**Kotlin and Scala.** In
[surefold/watershed-jvm](https://github.com/surefold/watershed-jvm), with
typed expressions and failures as values. `sbt exampleKotlin/run` and
`sbt exampleScala/run` run a first program in each. Scenario plans are in
Kotlin and experimental.

**Browser.** The [playground](https://surefold.dev/watershed/) needs nothing
installed. To build the WebAssembly module yourself, see the
[grid README](crates/watershed-grid/README.md#building-and-running).

## What it does

The model says what is calculated. How it runs is decided separately, and
the pieces combine.

**Compose.** A model is inputs and typed formulas: `f64`, `i64`, exact
decimals, booleans and strings, conditionals that depend only on the branch
they took, keyed collections with totals and groups, and formulas parsed
from text or read from a workbook. Watershed records what every formula
reads, and that record is all the rest needs.

**Execute.** An update follows the record forwards from a change and stops
at any value that comes out the same. A plan follows it backwards from the
outputs you want and compiles what they need for a batch of scenarios:
levels that each run at once, one kernel for every kind of arithmetic, and
storage reused after a value's last reader. A plan runs on CPU threads or a
CUDA GPU, as you choose, and its data can stay on the GPU between runs.

**Scale.** One plan runs thousands to millions of scenarios, and a stress
test replaces a few shared assumptions with nothing recompiled. After a
batch, a refresh reruns only the part of the plan a change reaches. A saved
plan runs in lots across several GPUs.

These stack: the 70-second run above is a fused plan, saved to a file, run
in lots by eight workers, and checked bit for bit against an unfused plan
in one process.

### Status

| | State | Where |
|---|---|---|
| Incremental updates, typed values, collections and grouping | Built | [core guide](crates/watershed-core/README.md) |
| History, explanation, change sinks, formulas as text | Built | [core guide](crates/watershed-core/README.md#history-and-change-tracking) |
| Scenario plans on CPU threads and CUDA; data resident on the GPU | Built | [watershed-gpu](crates/watershed-gpu/README.md), [how plans work](docs/scenario-plans.md) |
| Python, Kotlin, Scala and WebAssembly interfaces to the incremental engine | Built | [Architecture](#architecture) |
| Refreshing a batch after a change | Experimental | [watershed-gpu](crates/watershed-gpu/README.md#bringing-a-run-up-to-date-after-a-change) |
| Fused kernels | Experimental, off by default | [watershed-gpu](crates/watershed-gpu/README.md#working-nodes-out-in-clusters) |
| Saved plans, and lots across GPUs | Experimental; run on eight GPUs in one machine | [watershed-gpu](crates/watershed-gpu/README.md#running-a-batch-somewhere-else), [watershed-jvm](https://github.com/surefold/watershed-jvm) |
| Scenario plans from Kotlin, and in the browser on one thread | Experimental | [watershed-jvm](https://github.com/surefold/watershed-jvm), [grid README](crates/watershed-grid/README.md) |
| Reading a model from an `.xlsx` workbook | Experimental; one workbook tried | [watershed-gpu](crates/watershed-gpu/README.md#running-the-examples) |
| Choosing CPU or GPU automatically from measured costs | Research | [research note](docs/research/heterogeneous-execution.md) |
| Workers on several machines; a release on crates.io and PyPI | Planned | |

## Measured

All on one synthetic insurance model unless it says otherwise: lifelib's
[`savings.CashValue_ME_EX2`](https://lifelib.io/libraries/savings/index.html),
20,000 groups of policies projected month by month, 19.9 million
calculations. Hardware, runs and method are in
[docs/benchmarks.md](docs/benchmarks.md) and the two studies linked above.

- **Updates cost what they touch.** One record changing recalculated 570
  calculations in 58 µs, on a Ryzen 9 5950X.
- **Large models over many scenarios belong on a GPU.** 10,000 scenarios took
  10.2 s on one RTX 3090, including transfers and kernel compilation, and
  about 7.5 minutes on 32 CPU threads (estimated from a sample).
- **Small models belong on CPU threads, unless the data stays put.** A
  22-calculation pricing model over five million policies takes 5 ms to
  execute on a GPU and 280 ms a run once data is copied each way. With the
  data kept on the GPU, a thousand repricings took 5 seconds.
- **A change after a batch costs a fraction of the batch.** 0.14 to 0.22 s
  against 11.2 s to rerun, with identical results.
- **A batch divides across GPUs, and fusion shortens it again.** Two million
  scenarios took 18 min 46 s on one A100, 2 min 43 s on eight, and 70 s on
  eight with fused kernels.
- **The numbers are checked.** The model agrees with lifelib to 3.1 × 10⁻¹³,
  and a plan on CPU threads is bit-identical to the graph.

## How it works

**The graph.** Nodes are inputs or expressions over other nodes. Each has a
height: one more than the highest node it uses. Values, compiled
expressions and dependency links are stored in flat arrays indexed by node
id.

**An update.** `set` records a change; `stabilize` applies all pending
changes, lowest height first, so each affected node is recalculated once,
after everything it uses. A node whose new value equals its old one does not
queue its users. A level with tens of thousands of affected nodes is
recalculated across cores.

**A plan.** `Scenarios` walks back from the outputs, classifies each node as
per-scenario, shared across scenarios or constant, and translates the
per-scenario nodes into a flat program ordered by level. It knows the last
level that reads each node and reuses its storage afterwards: 344,124 values
of working memory per scenario for the 19.9-million-node model, where one
per node would be 159 MB. That decides how many scenarios fit on a GPU at
once.

More: [internals](docs/internals.md), [how plans work](docs/scenario-plans.md),
[testing and correctness](docs/testing.md).

## Architecture

| Component | Location | Role |
|---|---|---|
| Core | [`crates/watershed-core`](crates/watershed-core/README.md) | The graph, incremental update, collections, decimals, history, formulas. No `unsafe`. |
| Scenario plans | [`crates/watershed-gpu`](crates/watershed-gpu/README.md) | Plans on CPU threads; the `cuda` feature adds the NVIDIA GPU executor. CUDA is found at run time, so the workspace builds without it. |
| Parquet sink | [`crates/watershed-parquet`](crates/watershed-parquet) | Writes each update's changes to Parquet files on a background thread. |
| Python | [`crates/watershed-py`](crates/watershed-py/README.md) | A PyO3 package over the core. |
| WebAssembly | [`crates/watershed-web`](crates/watershed-web), [`web/`](web) | The engine behind the playground: updates, and plans on one thread. |
| Data grid | [`crates/watershed-grid`](crates/watershed-grid/README.md), [`web/grid/`](web/grid) | A browser data grid in which every cell is a graph node. |
| Kotlin and Scala | [surefold/watershed-jvm](https://github.com/surefold/watershed-jvm) | The JVM API, and the experimental cluster that runs a plan's lots across GPUs. |

[surefold/wsb](https://github.com/surefold/wsb), a typed build-file language
for JVM projects, is a separate project that uses the engine.

## Documentation

- **Guide and playground:** [surefold.dev/guides/watershed](https://surefold.dev/guides/watershed/), [surefold.dev/watershed](https://surefold.dev/watershed/).
- **API reference:** `cargo doc --open -p watershed-core` (or `-p watershed-gpu`); Python [type stubs](crates/watershed-py/watershed.pyi).
- **Examples:** [`loans`, `liabilities`, `capital` and the rest](crates/watershed-gpu/README.md#running-the-examples); `cargo run --example tour` prints a small graph before and after an update.
- **Benchmarks and how to reproduce them:** [docs/benchmarks.md](docs/benchmarks.md).
- **Limits:** [docs/limitations.md](docs/limitations.md).
- **Contributing:** [CONTRIBUTING.md](CONTRIBUTING.md).

## Limitations

The ones that decide whether Watershed fits; the full list is in
[docs/limitations.md](docs/limitations.md).

- **Plans cover numeric formulas.** `f64` and `bool` with arithmetic,
  `min`, `max`, `abs`, `exp`, `ln`, `sqrt`, `pow`, comparisons, logic and
  conditionals. Collection totals, `choose`, integers, decimals and text are
  incremental only. A plan names the node it cannot take, so a model is
  never silently run differently.
- **Setup is paid once.** Building a graph and compiling a plan each take
  about a microsecond a node. Watershed earns that back on the second run
  and on every change after.
- **GPU agreement has documented allowances** for NaN payloads, signed zero
  and rounding in `exp`, `ln` and `pow`. CPU plans are bit-identical to the
  graph.
- **CUDA only** on the GPU side.

## Building, releases and licence

```bash
cargo build --workspace
cargo test --workspace                           # no GPU or CUDA needed
cargo test -p watershed-gpu --features cuda      # GPU tests; needs the CUDA toolkit and an NVIDIA GPU
```

There are no tagged releases yet: depend on the repository, and expect APIs
to change. CI runs formatting, clippy, the tests on stable and Rust 1.85,
the documentation build, three examples, a WebAssembly build and the Python
tests. [CONTRIBUTING.md](CONTRIBUTING.md) has the rest.

Licensed under the Apache License 2.0; see [`LICENSE`](LICENSE). Work
derived from other projects is listed in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
