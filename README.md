# Watershed

[![CI](https://github.com/surefold/watershed/actions/workflows/ci.yml/badge.svg)](https://github.com/surefold/watershed/actions/workflows/ci.yml)
[![Licence: Apache 2.0](https://img.shields.io/badge/licence-Apache%202.0-blue.svg)](LICENSE)

**Describe a calculation once. Choose how it runs afterwards.**

Watershed is a computation engine. You write a model as ordinary typed
formulas, and the same model then updates in microseconds when an input
changes, runs across millions of scenarios on CPU threads or GPUs, and
spreads over several GPUs when the batch grows. None of those needs the
model rewritten.

**[Try it in your browser](https://surefold.dev/watershed/)** ·
[Get started](#get-started) ·
[Guide](https://surefold.dev/guides/watershed/)

| | On one model of 19.9 million calculations |
|---|---|
| **58 µs** | to update after one record changes |
| **10.2 s** | for 10,000 scenarios on one GPU |
| **70 s** | for 2,000,000 scenarios on eight GPUs |

## Try it in your browser

The [playground](https://surefold.dev/watershed/) is the engine itself,
compiled to WebAssembly. Nothing to install.

[![The playground on a model of an online order. The price was changed: four of five formulas were recalculated, the update stopped at free_shipping, whose value came out the same, and shipping was never looked at. Below, the same model run as a plan over a million prices in 126 ms.](docs/playground.png)](https://surefold.dev/watershed/)

- **Change an input** on four example models and see which formulas are
  recalculated, where an update stops, and which are never looked at.
- **Ask any value why.** Click a node for the tree of values behind it.
- **Run a model as scenarios.** The same formulas, compiled to a plan and
  run over up to a million values of one input. In the screenshot, a
  million took 126 ms in a browser on one thread.
- **Time it at scale.** Build a graph of a million nodes and compare one
  change against recalculating everything.
- **Watch it live.** A crypto book priced from a public feed, every
  Wikipedia edit as it happens, and a [data grid](https://surefold.dev/watershed/grid/)
  of 50,000 rows where every cell is a formula.

## What you can build

Each of these is a working example in the repository, on made-up data,
with its checks and timings.

- **A book that changes all day.** Amendments, corrections and late trades
  set on the model, each costing what it touches: 58 µs on a book of a
  million policies.
- **Risk across thousands of market paths.** An insurance book, a loan book
  and a pension scheme valued along 10,000 paths on one GPU, with stress
  tests that replace a few assumptions and run again.
- **A search over a large population.** A thousand price reviews over five
  million policies in 5 seconds, with the population loaded onto the GPU
  once.
- **Results that stay current.** After a batch, one input changes and all
  10,000 results come up to date in 0.2 s, where a rerun takes 11.2 s.
- **A spreadsheet, ten million times.** Formulas read from an `.xlsx`
  workbook, checked cell by cell against the values Excel saved, and run as
  a plan.
- **Live tables in the browser.** A grid whose cells, columns and totals are
  formulas, updated as edits arrive, as a TypeScript package with components
  for [Solid](packages/grid-solid) and [React](packages/grid-react). The
  [cookbook](https://surefold.dev/watershed/grid/cookbook/) shows it in an app.

The [guide](https://surefold.dev/guides/watershed/#applications) has each
with its figures.

## How it works

You describe the calculation. Watershed records what every formula reads,
and everything else follows from that record.

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
neither does setting a calculated node.

An **update** follows the record forwards from a change, lowest level first,
and stops at any value that comes out the same. A **plan** follows it
backwards from the outputs you want and compiles what they need for a batch
of scenarios. The two are held to the same numbers by tests.

[![The engine on a small model of an order, captured from the browser build. One input changes; the formulas it reaches are recalculated, an update stops where a value comes out the same, and the rest are not looked at. Then the same model runs as a plan for three prices.](docs/demo.gif)](https://surefold.dev/guides/watershed/#try)

More: [internals](docs/internals.md), [how plans work](docs/scenario-plans.md),
[testing and correctness](docs/testing.md).

## Execution

The same model, run the way the question needs.

- **Incrementally.** Work in proportion to the change, with dynamic
  dependencies and early cutoff, across cores for large updates.
- **Across scenarios.** One plan for thousands to millions of input sets,
  on every CPU core.
- **On a GPU.** The plan's kernels are generated for it; you write no GPU
  code. A plan and its data can stay on the GPU between runs, with results
  totalled there.
- **Fused.** Groups of formulas that feed one another run in one GPU
  thread, keeping intermediate values out of memory. Experimental.
- **Refreshed.** After a batch, only the part of the plan a change reaches
  is rerun, to the last bit what running it all again gives.
- **Across several GPUs.** A saved plan runs in lots, one worker per GPU,
  with a lot moved if a worker stops. Experimental; run on eight GPUs in one
  machine.

You choose where a plan runs. Choosing automatically from measured costs is
research in progress.

### Status

| | State | Where |
|---|---|---|
| Incremental updates, typed values, collections and grouping | Built | [core guide](crates/watershed-core/README.md) |
| History, explanation, change sinks, formulas as text | Built | [core guide](crates/watershed-core/README.md#history-and-change-tracking) |
| Scenario plans on CPU threads and CUDA; data resident on the GPU | Built | [watershed-gpu](crates/watershed-gpu/README.md), [how plans work](docs/scenario-plans.md) |
| Python, Kotlin, Scala and WebAssembly interfaces to the incremental engine | Built | [Architecture](#architecture) |
| Refreshing a batch after a change | Built | [watershed-gpu](crates/watershed-gpu/README.md#bringing-a-run-up-to-date-after-a-change) |
| Fused kernels | Experimental, off by default | [watershed-gpu](crates/watershed-gpu/README.md#working-nodes-out-in-clusters) |
| Saved plans, and lots across GPUs | Built; lots run on eight GPUs in one machine, not yet on several machines | [watershed-gpu](crates/watershed-gpu/README.md#running-a-batch-somewhere-else), [watershed-jvm](https://github.com/surefold/watershed-jvm) |
| Scenario plans from Kotlin, and in the browser on one thread | Experimental | [watershed-jvm](https://github.com/surefold/watershed-jvm), [grid README](crates/watershed-grid/README.md) |
| Reading a model from an `.xlsx` workbook | Built; 25 functions, each checked against LibreOffice for 24 inputs | [watershed-xlsx](crates/watershed-xlsx/README.md) |
| Choosing CPU or GPU automatically from measured costs | Research | [research note](docs/research/heterogeneous-execution.md) |
| Workers on several machines; a release on crates.io and PyPI | Planned | |

## Performance

Three measurements, one model: lifelib's
[`savings.CashValue_ME_EX2`](https://lifelib.io/libraries/savings/index.html),
20,000 groups of policies projected month by month, 19.9 million
calculations, on synthetic data.

| What | Result | On |
|---|---|---|
| One record changes | 58 µs; 570 calculations rerun | Ryzen 9 5950X, median of five runs |
| 10,000 scenarios | 10.2 s, including transfers and kernel compilation | One RTX 3090, median of five runs |
| 2,000,000 scenarios | 18 min 46 s on one GPU, 2 min 43 s on eight, 70 s on eight with fused kernels | A100 (40 GB) GPUs in one machine; eight plain twice, the others once |

Every result is checked: the model agrees with lifelib to 3.1 × 10⁻¹³, and
a plan on CPU threads is bit-identical to the graph.

The full picture, with hardware, method, the cases where CPU threads are
the better choice, and what did not pay:
[benchmark study](https://surefold.dev/bench/watershed-gpu/),
[eight-GPU study](https://surefold.dev/bench/watershed-cluster/) and
[docs/benchmarks.md](docs/benchmarks.md).

## Get started

Watershed is pre-release. Nothing is published to crates.io, PyPI or Maven
yet, so every route builds from source and needs a
[Rust toolchain](https://rustup.rs) (1.85 or later).

| | Install | Covers |
|---|---|---|
| Rust | A git dependency | The whole engine |
| Python | One `pip install` from the repository | Incremental engine, collections, formulas as text |
| Kotlin, Scala | Clone [watershed-jvm](https://github.com/surefold/watershed-jvm); needs JDK 25 and sbt | Incremental engine; scenario plans in Kotlin, experimental |
| Browser | Nothing: the [playground](https://surefold.dev/watershed/) | The example models, updates and plans on one thread |

**Rust.** Add the crates and run the example above:

```toml
[dependencies]
watershed-core = { git = "https://github.com/surefold/watershed" }
watershed-gpu  = { git = "https://github.com/surefold/watershed" }   # scenario plans; CPU threads by default
```

For a model of realistic size, in a clone of this repository, with no data
files:

```bash
cargo run --release -p watershed-gpu --example loans -- 2000 1000     # 2,000 loans, 1,000 scenarios, on CPU threads
cargo run --release -p watershed-gpu --features cuda --example loans  # 20,000 × 10,000 on an NVIDIA GPU
```

**Python.** Python 3.9 or later.

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

**Kotlin and Scala.** `sbt exampleKotlin/run` and `sbt exampleScala/run` in
[watershed-jvm](https://github.com/surefold/watershed-jvm) run a first
program in each.

**Browser.** The [grid package](packages/grid/README.md) holds rows in a
`Table` and draws them, with components for Solid and React. The
[cookbook](https://surefold.dev/watershed/grid/cookbook/) has whole examples
that run on the page, in Solid and
[React](https://surefold.dev/watershed/grid/react/cookbook/): rows from an API,
loading as you scroll, saving edits back, WebSocket and SSE streams, charts and
a table in a worker. To build the
WebAssembly module and the pages yourself, see the
[grid README](crates/watershed-grid/README.md#building-and-running).

The [guide](https://surefold.dev/guides/watershed/#example) has the same
example in every language, with what each prints.

## Documentation

| | |
|---|---|
| Guide and playground | [surefold.dev/guides/watershed](https://surefold.dev/guides/watershed/), [surefold.dev/watershed](https://surefold.dev/watershed/) |
| The engine, feature by feature | [core guide](crates/watershed-core/README.md), [scenario plans](crates/watershed-gpu/README.md) |
| Examples | [`loans`, `liabilities`, `capital` and the rest](crates/watershed-gpu/README.md#running-the-examples); `cargo run --example tour` |
| API reference | `cargo doc --open -p watershed-core` (or `-p watershed-gpu`); Python [type stubs](crates/watershed-py/watershed.pyi) |
| How it is built | [internals](docs/internals.md), [how plans work](docs/scenario-plans.md), [testing](docs/testing.md) |
| Benchmarks and how to reproduce them | [docs/benchmarks.md](docs/benchmarks.md) |
| Limits | [docs/limitations.md](docs/limitations.md) |
| Contributing | [CONTRIBUTING.md](CONTRIBUTING.md) |

### Architecture

| Component | Location | Role |
|---|---|---|
| Core | [`crates/watershed-core`](crates/watershed-core/README.md) | The graph, incremental update, collections, decimals, history, formulas. No `unsafe`. |
| Scenario plans | [`crates/watershed-gpu`](crates/watershed-gpu/README.md) | Plans on CPU threads; the `cuda` feature adds the NVIDIA GPU executor. CUDA is found at run time, so the workspace builds without it. |
| Parquet sink | [`crates/watershed-parquet`](crates/watershed-parquet) | Writes each update's changes to Parquet files on a background thread. |
| Excel workbooks | [`crates/watershed-xlsx`](crates/watershed-xlsx/README.md) | Translates a workbook's formulas between chosen inputs and outputs into a graph. |
| Python | [`crates/watershed-py`](crates/watershed-py/README.md) | A PyO3 package over the core. |
| WebAssembly | [`crates/watershed-web`](crates/watershed-web), [`web/`](web) | The engine behind the playground: updates, and plans on one thread. |
| Data grid | [`crates/watershed-grid`](crates/watershed-grid/README.md), [`packages/`](packages/grid/README.md), [`web/grid/`](web/grid), [`web/grid-react/`](web/grid-react) | A browser data grid in which every cell is a graph node: the engine as WebAssembly, the TypeScript packages (a `Table`, and components for Solid and React), and the pages built with them. |
| Kotlin and Scala | [surefold/watershed-jvm](https://github.com/surefold/watershed-jvm) | The JVM API, and the experimental cluster that runs a plan's lots across GPUs. |

### Limitations

The ones that decide whether Watershed fits; the rest are in
[docs/limitations.md](docs/limitations.md).

- **Plans cover numeric formulas.** `f64` and `bool` with arithmetic,
  `min`, `max`, `abs`, `exp`, `ln`, `sqrt`, `pow`, `sin`, `cos`, comparisons, logic and
  conditionals; collection and group totals (`sum`, `min`, `max`, `count`);
  and `choose` by an option number that is the same in every scenario.
  Integer arithmetic, decimals and text are incremental only. A plan names
  the node it cannot take, so a model is never silently run differently.
- **Setup is paid once.** Building a graph and compiling a plan each take
  about a microsecond a node. Watershed earns that back on the second run
  and on every change after.
- **GPU agreement has documented allowances** for NaN payloads, signed zero
  and rounding in `exp`, `ln`, `pow`, `sin` and `cos`.
- **CUDA only** on the GPU side.

### Building, releases and licence

```bash
cargo build --workspace
cargo test --workspace                           # no GPU or CUDA needed
cargo test -p watershed-gpu --features cuda      # GPU tests; needs the CUDA toolkit and an NVIDIA GPU
```

There are no tagged releases yet: depend on the repository, and expect APIs
to change. [CONTRIBUTING.md](CONTRIBUTING.md) has the checks CI runs.

Licensed under the Apache License 2.0; see [`LICENSE`](LICENSE). Work
derived from other projects is listed in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
