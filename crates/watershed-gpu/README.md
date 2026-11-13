# watershed-gpu

Runs a watershed graph for many sets of inputs at once: on all CPU cores, or
with the `cuda` feature on an NVIDIA GPU.

A graph holds one set of values. To ask "what if these inputs were
different?" thousands or millions of times (a risk run, a what-if table, a
Monte Carlo simulation), a `Scenarios` plan takes a copy of the part of the
graph the answers depend on and runs every set of inputs through it. The
graph is not touched.

```rust
use watershed_core::prelude::*;
use watershed_gpu::Scenarios;

let mut g = Graph::new();
let qty = g.input("qty", 10.0)?;
let price = g.input("price", 2.5)?;
let total = g.define("total", qty * price)?;

let plan = Scenarios::new(&g, &[price.id()], &[total.id()])?;
// Three scenarios, each one value for `price`.
assert_eq!(plan.run_cpu(&[2.5, 3.0, 4.0]), [25.0, 30.0, 40.0]);

// With `--features cuda`:
// let gpu = watershed_gpu::Gpu::new()?;
// let results = gpu.run(&plan, &inputs)?;
```

Each result is the same, to the last bit, as setting those inputs on the
graph and updating it. On the GPU there are three exceptions: a result that
is not a number may be a different not-a-number; `min` and `max` of `0.0`
and `-0.0` follow x86-64; and `exp`, `ln` and `pow` may differ in the last
digit or two.

## Two kinds of input

*Varied* inputs differ by scenario. *Shared* inputs are the same for every
scenario of a run, and `set_shared` replaces them without making the plan
again:

```rust
let mut plan = Scenarios::with_shared(&g, &vary, &shared, &outputs)?;
let today = gpu.run(&plan, &inputs)?;
plan.set_shared(&stressed);            // a few numbers
let stressed = gpu.run(&plan, &inputs)?;   // the same scenarios again
```

That is what makes "the same run under other assumptions" cheap: a stress
test, a changed tax rule, a shuffled price history.

## What it handles

Numbers (`f64`) and yes-or-no values: arithmetic, `min`, `max`, `abs`, `exp`,
`ln`, `sqrt`, `pow`, comparisons, `&`, `|`, `!`, and conditions. A node that
depends on a varied or shared input and holds a choice, a total, or work on
text, whole numbers or decimals is refused when the plan is made, with its
name.

## Building

The `cuda` feature is off by default, so the workspace builds and tests
without CUDA. With it, the CUDA toolkit's libraries are looked for when a
`Gpu` is opened, not when the program is built, and the kernel is compiled
then.

```bash
cargo test -p watershed-gpu                    # the plan, on the CPU
cargo test -p watershed-gpu --features cuda    # and on the GPU
```

The crate allows `unsafe` in one place, the kernel launch.

## What it has been tried on

Every figure below is from one machine: a Ryzen 9 5950X (16 cores, 32
threads) and one RTX 3090 (24 GB). "The plan on the CPU" is the same plan on all 32 threads.
Where a time is scaled up from a smaller run, it says so.

| Example | What it is | Checked against | GPU | Plan on the CPU | Something else |
|---|---|---|---|---|---|
| `liabilities` | An insurer's savings policies with guarantees: 1,009,152 policies (20,000 kinds) on 10,000 market paths | lifelib's `savings.CashValue_ME_EX2`: 3 million answers, largest difference 3×10⁻¹³ | 10 s | about 7 minutes (scaled) | the reference, Python on one core: about 4.9 hours (scaled) |
| `capital` | The same book valued over 1,000 paths in each of 1,000 possible next years: a million whole-book valuations | the model above | 3 minutes (4,000 kinds) | not measured | the reference: about 98 hours (scaled) |
| `loans` | Expected losses on 20,000 mortgages over 10,000 paths of the economy | the same sums written out by hand: 3×10⁻¹⁷ | 6.6 s | about 2 minutes (scaled) | by hand, 32 threads: 17.7 s (scaled) |
| `pensions` | A pension scheme of 299,562 members over 5,000 paths and 75 years | the same sums by hand: 5×10⁻¹³ | 0.9 s | 15.1 s (scaled) | by hand, 32 threads: 5.0 s (scaled) |
| `spreadsheet` | A public Excel valuation workbook, 10 million trials | Excel's stored values: all 176 cells | 0.5 s | 1.8 s | the graph, a trial at a time: 104 s (scaled) |
| `monte_carlo` | 200,000 positions on 500 shares, 100,000 futures | the graph, bit for bit | 1.6 s | about 29 s (scaled) | the graph: about 25 minutes (scaled) |
| `tax_benefit` | A country's tax and benefit rules over 10 million households | the graph, bit for bit | 0.7 s | 0.5 s | the graph: 28 s (scaled) |
| `insurance` | A car insurer's pricing over 5 million policies | the graph, bit for bit | 0.4 s | 0.15 s | the graph: 10.5 s (scaled) |
| `insurance`, its search | 1,000 price reviews, each priced on all 5 million policies and added up | | 4.9 s, 5 ms a review | | |
| `backtest`, `experts` | Trading rules on 500 shares over 16 years, each against 1,000 shuffled histories | the CPU, bit for bit | 49 ms to 5 s a history | | |

### What that says

- **Small rules over a big population need the data to stay put.** For the
  tax, pricing and spreadsheet examples the graph is a few dozen to a few
  hundred nodes. The GPU calculates ten million households in about 20 ms
  and then spends half a second copying them in and the answers out, so for
  one run the plan on the CPU is as fast. With the population loaded once
  and only totals read back (`Gpu::load`, `Loaded::totals`), a price review
  of five million policies takes 5 ms, and a thousand of them 4.9 s.
- **A big, deep graph run for many scenarios needs the GPU.** Policies
  projected month by month, loans quarter by quarter: millions of nodes in
  hundreds of levels. There the GPU is 20 to 40 times faster than the same
  plan on 32 threads, and it is working: 96 to 98% busy and 335 W through a
  long run. On the insurance book it works out 2 × 10¹⁰ nodes a second,
  which at four or five numbers read and one written for each is about what
  the card's memory can move.
- **A program written by hand for one model is fast.** On the loan and
  pension examples a purpose-written parallel loop is 3 to 8 times faster
  than the plan on the CPU, and the GPU beats that loop by 3 to 6 times.
  The plan's case is that nobody had to write that program, and that the
  same graph still updates in microseconds when one input changes.
- **Making a plan costs.** A 20-million-node graph takes 22 s to build and
  17 s to make a plan from, once. Every run after that, and every
  `set_shared`, skips it.

### What the models found

The books, the economies and the rules in these examples are made up, apart
from the lifelib model, its tables and the workbook. These show what such a
run can reveal, not facts about any insurer, bank or scheme.

- One steady market path prices the insurer's end-of-term guarantee at 0.8
  billion; 10,000 paths price it at 8.0 billion. The guarantee costs
  something only when markets fall.
- The worst year in two hundred costs the insurer 6.1 billion by the run
  inside a run, and 9.8 billion by two prescribed stresses added by formula.
- Three weighted scenarios set aside 8% less for bad loans than the average
  of 10,000 paths.
- Whether a repossessed house fetches 65% or 75% of its value changes the
  loan provision by 76%; a quarter more sensitivity to unemployment, by 6%.
- With nothing paid in, moving the pension scheme from no shares to 40%
  cuts the chance of running out of money from 98% to 34%; 80% does no
  better.
- On real prices from 2010 to 2025, shares that fell behind over a few days
  tended to catch up, far beyond what shuffled histories gave (fewer than 1
  in 100 did as well), by about as much as it costs to trade on it.

## Running the examples

```bash
cargo run --release --features cuda --example monte_carlo
cargo run --release --features cuda --example tax_benefit
cargo run --release --features cuda --example insurance
cargo run --release --features cuda --example loans
cargo run --release --features cuda --example pensions
```

Without `--features cuda` each runs on the CPU, with fewer scenarios unless
told otherwise.

`liabilities` and `capital` need the reference model's tables and answers.
`examples/shared/liabilities_reference.py` runs lifelib and writes them:

```bash
pip install lifelib pandas openpyxl scipy
mkdir export && python crates/watershed-gpu/examples/shared/liabilities_reference.py
cargo run --release --features cuda --example liabilities -- export
cargo run --release --features cuda --example capital -- export
```

`spreadsheet` takes an `.xlsx` workbook, the cells to vary and the cells to
read. The one above is Aswath Damodaran's `fcffsimpleginzu.xlsx`:

```bash
cargo run --release --features cuda --example spreadsheet -- fcffsimpleginzu.xlsx \
    "'Input sheet'!B26=0.02:0.09" "'Input sheet'!B29=0.10:0.18" \
    "'Valuation output'!B33"
```

`backtest` and `experts` take a folder of daily prices, one CSV a share
(`Date,Open,High,Low,Close,Volume`), or `made-up` for prices that wander at
random.

## How it runs

- **Levels.** A node's level is one more than the highest it reads, so a
  whole level is worked out at once.
- **Slots.** A node's value is kept only until the last level that reads it
  has been worked out, and its slot is then used again. A scenario of the
  20-million-node insurance book needs room for about 200,000 numbers, not
  20 million, so 10,000 scenarios fit on the card at once.
- **Shapes.** Nodes whose formulas differ only in what they read have one
  shape. A plan with up to 256 shapes gets a kernel written and compiled
  for each, the first time it is run; the insurance book has a few dozen. A
  plan with more is run by one general kernel that reads each node's
  instructions as it goes. Both give the same answers.
- **Staying on the GPU.** `Gpu::load` puts a plan and its scenarios on the
  card to stay. `Loaded::run` and `Loaded::totals` run them again after
  `set_shared`, or `set_inputs`, copying up only what changed; `totals`
  adds each output up on the card and copies back almost nothing. It adds
  in a fixed order that `Scenarios::totals_cpu` repeats, so the two agree
  to the last bit.

## Not done yet

- A level's nodes are still written to memory one by one and read back by
  the next level. Working a chain of nodes out in one go, keeping what only
  the chain reads off the table, would move less memory, which is what the
  big graphs wait for.
- `Loaded` needs every scenario to fit on the card at once.
- `Loaded::totals` adds up; a count above a threshold, or a percentile,
  still needs every scenario's answer copied back.
- Making a plan reads the graph's nodes one after another to find the order
  to work them out in, and that part uses one core.
- The spreadsheet example has been tried on one workbook. It reads `.xlsx`
  only, and understands the functions listed at the top of the file.
