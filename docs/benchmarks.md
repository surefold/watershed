# Benchmarks and reproducibility

What has been measured, on what, and how to run it again. The scenario
benchmarks and their reference implementations are documented in full in the
[watershed-gpu README](../crates/watershed-gpu/README.md#what-it-has-been-tried-on);
this file covers the core's own benchmarks and how to read all of them.

## Reading the figures

- **One machine.** Ryzen 9 5950X (16 cores, 32 threads), 47 GB, RTX 3090
  (24 GB), Linux under WSL2, CUDA 13.1, rustc 1.99. Nothing here has been
  run on other hardware.
- **Mostly single runs.** Where there are repeats it says so. Between two
  full runs, measured GPU times moved by up to a fifth, and extrapolated
  times by up to a third.
- **Measured or extrapolated.** GPU times are measured in full. Times for
  the same plan on CPU threads, for hand-written CPU loops and for reference
  implementations are in most cases scaled up from a smaller run (a few
  dozen to a few thousand scenarios), and are labelled "scaled" or
  "extrapolated" wherever they appear.
- **Setup is reported separately.** Scenario times exclude graph
  construction and plan compilation, each about 1 µs per node, once per
  model. First GPU runs include kernel compilation.
- **Double precision** everywhere. Fused multiply-add is disabled on the GPU
  so that results match the CPU.
- **Each benchmark checks its own result**: the core examples against a full
  recalculation, the scenario examples against the graph and, where one
  exists, an independent reference.

These are not comparisons against tuned competitors, with one exception (the
JAX program below). No other incremental engine, vectorised CPU kernel or
single-precision variant has been measured.

## Core: incremental updates

```bash
cargo run --release --example benchmark                 # about 1 million nodes
cargo run --release --example benchmark -- 25000000     # 25 million trades, about 50 million nodes
```

The graph is 10,000 market prices feeding trades, which feed a binary tree
of totals. Median of many runs; one run of the example on 13 November 2026.

| about 1,000,000 nodes | 1 thread | 32 threads | nodes recalculated |
|---|---|---|---|
| recalculate everything | 16.9 ms | 11.1 ms | 999,999 |
| one price changes | 0.16 ms | 0.19 ms | 1,338 |
| 1% of prices change together | 8.3 ms | 8.8 ms | 67,829 |
| 10% of prices change together | 16.3 ms | 27.2 ms | 353,165 |

One price changing is about 70 times cheaper than recalculating everything.
When 10% of prices change, a third of the graph is recalculated and the
incremental update costs as much as a full recalculation. Threads do not
help at this size, and for the 10% case they cost time here.

| nodes | peak memory | build | recalculate everything (1 / 32 threads) | one price changes | 1% of prices (1 / 32 threads) |
|---|---|---|---|---|---|
| 1,009,999 | 217 MB | 0.44 s | 16.9 / 11.1 ms | 0.16 ms | 8.3 / 8.8 ms |
| 10,009,999 | 2.1 GB | 5.6 s | 169 / 56 ms | 2.8 ms | 107 / 67 ms |
| 50,009,999 | 10.0 GB | 34 s | 876 / 247 ms | 18.7 ms | 516 / 208 ms |

Memory is 200 to 216 bytes per node. One price changing costs more in the
larger graphs because each of the 10,000 prices then feeds more trades
(1,338, 13,399 and 66,982 nodes recalculated).

### Node order

```bash
cargo run --release --example placement
```

builds the same million-node graph in two creation orders and times the same
updates on one thread:

| | everything | one price | 1% of prices | 10% of prices |
|---|---|---|---|---|
| level by level | 18.3 ms | 0.21 ms | 10.2 ms | 19.1 ms |
| subtree by subtree | 21.1 ms | 0.22 ms | 16.3 ms | 68.4 ms |

Creating nodes level by level makes medium and large updates 1.6 to 3.6
times faster here. A single change costs the same either way.

### Collections

```bash
cargo run --release --example collection -- 1000000
```

A million rows (4,000,005 nodes) built and totalled in 4.1 s. Changing one
row: 9 µs per update and 4 nodes recalculated on average. Removing 1,000
rows in one update: 1.6 ms. Every total is checked against adding the rows
up from scratch.

### History

```bash
cargo run --release --example history
```

On the million-node benchmark graph with one price changing per update,
keeping 1,000 updates of history raised the update from 199 µs to 239 µs and
kept 1,336 values per update at 32 bytes each, 43 KB per update. Reading a
node's value 1,000 updates back took 7 µs; the latest value, 20 ns.

`cargo run --release -p watershed-parquet --example cost -- <empty folder>`
measures the cost and file size of writing changes to Parquet. It was not
re-run for this document.

## Incremental updates against lifelib

The `amendments` example in watershed-gpu (which uses no GPU) values
lifelib's `savings.CashValue_ME_EX2` for 20,000 model points on one market
path, in lifelib and as a graph. All figures measured, on the machine above.

| | lifelib | Watershed graph |
|---|---|---|
| First valuation | 3.6 s | 26.4 s, of which 21.2 s is graph construction |
| Every input changes | 3.6 s | 5.3 s |
| One model point changes | 2.3 s (median of 5) | 58 µs (mean of 1,000, median of five runs, 56 to 94 µs; 570 of 19,887,781 nodes) |
| One model point valued alone, by hand | 0.29 s (median of 5) | |

The values agree to a relative 3.7×10⁻¹⁵. The graph's figures are medians of five runs;
single earlier runs gave 27.7 s, 5.2 s and 56 µs, and 26.9 s, 4.7 s and 54 µs. The
[watershed-gpu README](../crates/watershed-gpu/README.md#against-a-model-that-works-a-column-at-a-time)
discusses what this comparison does and does not show.

## Scenario evaluation

The insurance scenario benchmark (`liabilities`): 20,000 model points
standing for 1,009,152 policies, 19,889,057 nodes in 967 levels, 10,000
market scenarios.

| Stage | Time | Basis |
|---|---|---|
| Graph construction | 22.7 s | measured, once per model |
| Plan compilation | 17.7 s | measured, once per model |
| GPU run, end to end | 10.2 s | measured; median of five runs, 10.1 to 10.3 s |
| of which upload / compute / download | 0.87 s / 9.07 s / 5 ms | measured |
| Same plan, 32 CPU threads | about 7.5 minutes | extrapolated: 45 ms per scenario over 128 scenarios |
| JAX, model points sorted by remaining term, same GPU | 6.2 s | measured; five runs, 6.10 to 6.32 s |
| JAX, every model point stepped for all 241 months, same GPU | 13.1 s | measured; five runs |
| lifelib, one core | about 4.9 hours | extrapolated: 17.6 s for 200 model points × 1,000 scenarios |

One scenario needs 344,124 live values (2.75 MB), so 6,807 scenarios fit on
the 24 GB card at a time and 10,000 are run in two parts.

Agreement with lifelib: 7.9×10⁻¹⁵ to 9.7×10⁻¹⁵ on the model as published
(four configurations, 1.44 million outputs) and 3.1×10⁻¹³ on 200 model
points × 1,000 scenarios with static and dynamic lapses. The 20,000 × 10,000
run itself has no independent reference; lifelib would take hours.

The other scenario examples (nested capital, loan losses, pensions, dosing,
tax and benefit rules, pricing, Monte Carlo, an Excel workbook, backtests),
their sources and their caveats are in the
[watershed-gpu README](../crates/watershed-gpu/README.md#what-it-has-been-tried-on).
Its ["What each example rests on"](../crates/watershed-gpu/README.md#what-each-example-rests-on)
section says which inputs are real and which are made up, and
["Running the examples"](../crates/watershed-gpu/README.md#running-the-examples)
gives the commands, including the reference scripts.

## Figures not re-measured

These appear in the documentation and come from earlier measurements that
were not repeated on 13 November 2026:

- Decimal nodes costing about four times a float node to recalculate
  (about 83 ns against 21 ns).
- Python call overhead: changing one price and updating a 210,000-node
  graph taking about 29 µs from Python.
- A row moving between groups recalculating 4 nodes (1,000 rows in 10
  groups).
- The grid's streaming figures (about 1 ms and 2 ms per frame of 100 price
  changes on 100,000 rows) and its 175 MB per grouping column.
- GPU utilisation of 96 to 98% and about 335 W during a long run, sampled
  once.
