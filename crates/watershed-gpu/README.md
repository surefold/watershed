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

Every figure below is from one machine, mostly from a single run; a second
run of everything moved the measured GPU times by up to a fifth and the
scaled CPU and by-hand times by up to a third. It is a Ryzen 9 5950X (16 cores, 32
threads) and one RTX 3090 (24 GB). "The plan on the CPU" is the same plan on all 32 threads.
Where a time is scaled up from a smaller run, it says so.

| Example | What it is | Checked against | GPU | Plan on the CPU | Something else |
|---|---|---|---|---|---|
| `liabilities` | An insurer's savings policies with guarantees: 1,009,152 policies (20,000 kinds) on 10,000 market paths | lifelib's `savings.CashValue_ME_EX2`: 3 million answers, largest difference 3×10⁻¹³ | 10 s | about 7 minutes (scaled) | the reference, Python on one core: about 4.9 hours (scaled) |
| `capital` | The same book valued over 1,000 paths in each of 1,000 possible next years: a million whole-book valuations | the model above | 3 minutes (4,000 kinds) | not measured | the reference: about 98 hours (scaled) |
| `loans` | Expected losses on 20,000 mortgages over 10,000 paths of the economy | the same sums written out by hand: 3×10⁻¹⁷ | 6.6 s | about 2 minutes (scaled) | by hand, 32 threads: 17.7 s (scaled) |
| `pensions` | A pension scheme of 299,562 members over 5,000 paths and 75 years | the same sums by hand: 5×10⁻¹³ | 0.9 s | 15.1 s (scaled) | by hand, 32 threads: 5.0 s (scaled) |
| `spreadsheet` | A public Excel valuation workbook, 10 million trials | Excel's stored values: all 176 cells | 0.5 s | 1.8 s | the graph, a trial at a time: 104 s (scaled) |
| `dosing` | An antibiotic's dosing protocol tried on 5 million made-up patients, each followed for 72 hours | SciPy's LSODA solver, 300 patients: 1.7×10⁻¹⁰ | 60 ms a protocol, 51 protocols in 3.0 s | 1.8 s a protocol | the graph: 70 s (scaled) |
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

### Against a model that works a column at a time

The usual way to make such a model fast is to hold every policy as a row
and work on whole columns at once, as lifelib does with NumPy and as the
GPU versions of it do with JAX or PyTorch. That is quick to value a book
and has to value all of it again when one policy changes. The `amendments`
example puts the two side by side, with no GPU: the same model
(`savings.CashValue_ME_EX2`), the same 1,009,152 policies in 20,000 kinds,
one market path, both timed on this machine. They agree on the book's
value, before and after an amendment, to 4×10⁻¹⁵.

| | lifelib | The graph |
|---|---|---|
| Valuing the book the first time | 3.6 s | 26.9 s, of which 22.2 s is building the graph |
| After a new market path, when everything changes | 3.6 s | 4.7 s |
| After one policy changes | 2.3 s | 54 µs |
| The same, valuing only the changed policy by hand and adjusting the total | 0.29 s | |

After one policy changes the graph works out about 570 of its 19.9 million
nodes again: 42,600 times faster than lifelib valuing the book again, and
5,400 times faster than valuing just that policy with it. A day of 100,000
amendments is 5.4 s against about 64 hours. Valuing the whole book from
nothing, lifelib is seven times faster, and a version of it written for
speed would be faster still.

This is one kind of problem, chosen because it is the one a graph is for:
a large model of which a little changes at a time. It says nothing about
valuing a book from nothing, where the graph loses, and it is a comparison
with one tool. Nor is the graph the fastest thing that could do it. Code
written for this model, valuing the one changed policy in a loop and
adjusting a running total, would be quicker: on the loan example such a
loop takes about 3 µs for a loan on a path, against 54 µs here. And other
engines are built on the same idea (Jane Street's Incremental, Salsa,
differential dataflow, a spreadsheet's own recalculation) and should be in
the same range; none of them was measured.

### Against the same model written by hand for a GPU

`examples/shared/savings_jax.py` is the insurance model written by hand as
an array program in JAX: every kind of policy a row, every path a column,
stepped a month at a time, in double precision. It takes the same inputs and
gives the same outputs as the plan, and agrees with lifelib to the same
3.1×10⁻¹³. On the same card, for 20,000 kinds of policy on 10,000 paths,
each measured in full:

| | Time |
|---|---|
| JAX, every policy stepped for all 241 months | 13.1 s (five runs, 13.11 to 13.12) |
| The plan | 10.0 s first run; eight more at 9.4 to 9.9 |
| JAX, policies sorted so each lot stops at its longest policy | 6.2 s (five runs, 6.10 to 6.32) |

So a program written for this one model, with the obvious tuning, is about
1.6 times faster than the plan, and without it is slower. The plan does not
need the tuning because the graph only has nodes for the months each policy
runs. The array program keeps what it is working on in registers within a
month, where the plan writes every node to memory and reads it back; that
is most of the difference. Nothing here says how either would do against a
program tuned further, in single precision, or on another model.

The Actuarial Open Source benchmarks run lifelib's term-life model for 100
million policies in 3.4 s on an A100 with JAX. That has not been run here:
it needs a table looked up by a varied input, and the plan cannot do that
yet.

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
- At the same dose for every kilogram, patients with good kidneys overshoot
  the aimed-for exposure to the antibiotic far more often than those with
  poor ones; a dose for each band of kidney function puts 41% within the
  aim against 31%, and no dose by weight and kidney function does much
  better, which is why blood levels are measured.
- On real prices from 2010 to 2025, shares that fell behind over a few days
  tended to catch up, far beyond what shuffled histories gave (fewer than 1
  in 100 did as well), by about as much as it costs to trade on it.

## What each example rests on

What is taken from somewhere, and what is made up.

- **`liabilities`, `capital`, `amendments`.** The model, its mortality table
  and its charges are lifelib's
  [`savings.CashValue_ME_EX2`](https://lifelib.io/libraries/savings/index.html),
  and the graph is checked against it. The policies are made up, as are the
  swings in the funds and how far markets and rates move in a year. The
  worst year in two hundred is Solvency II's measure
  ([Directive 2009/138/EC](https://eur-lex.europa.eu/eli/dir/2009/138/oj),
  Article 101). That the run inside a run is not done in practice is from a
  Milliman paper: "This brute force approach is too time-consuming to be
  used in a real insurance business case"
  ([Boumezoued, Cherchali and Truc, 2022](https://ae.milliman.com/-/media/milliman/pdfs/2022-articles/11-8-22_efficient_computation-of-scr-using-mlmc.ashx)).
  The 39% fall in funds is the size of the standard formula's shock for
  shares; the one-point move in rates and the way the two are added up are
  simplifications, not that formula.
- **`loans`.** Banks set money aside for expected losses under IFRS 9,
  weighting several economic scenarios; the European Banking Authority's
  [2023 monitoring report](https://www.eba.europa.eu/sites/default/files/2023-11/25b12d35-9c28-4335-a589-166c77198920/Final%20Report%20on%20IFRS9%20implementation%20by%20EU%20institutions.pdf)
  says "the most common approach is to envisage three scenarios". The
  loans, how defaults respond to unemployment and house prices, and the
  economy are made up and follow no published model.
- **`pensions`.** Capped increases are how United Kingdom schemes work:
  pensions earned since 1997 must rise with prices up to 5% a year, and
  2.5% for service since 2005
  ([limited price indexation](https://en.wikipedia.org/wiki/Limited_price_indexation);
  [Pensions Act 2004, section 278](https://www.legislation.gov.uk/ukpga/2004/35/section/278/notes)).
  The scheme, its members, the death rates and the economy are made up.
- **`dosing`.** The aim of 400 to 600 mg·h/L is from the 2020 consensus
  guideline ([Rybak and others](https://doi.org/10.1093/ajhp/zxaa036)). The
  model of how the drug moves is from
  [Goti and others (2018)](https://doi.org/10.1097/FTD.0000000000000490),
  with its numbers from memory and unchecked. Kidney function is estimated
  as [Cockcroft and Gault (1976)](https://doi.org/10.1159/000180580) did.
  The protocol is made up to resemble common practice, and the patients
  are made up. None of it was reviewed by anyone with medical training.
- **`spreadsheet`.** `fcffsimpleginzu.xlsx` is from Aswath Damodaran's
  [page of valuation spreadsheets](https://pages.stern.nyu.edu/~adamodar/New_Home_Page/spreadsh.htm),
  as downloaded. The ranges the inputs were varied over are made up.
- **`backtest`, `experts`.** Daily prices from 2010 to 2025 for shares with
  a price throughout, from the FINSABER data set on Hugging Face
  (`finsaber-team/FINSABER-V2-Data`), which does not say where its prices
  come from. Shares that stopped trading are missing, which flatters the
  market and every rule alike.
- **`tax_benefit`, `insurance`, `monte_carlo`.** Made up throughout.

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

`amendments` needs lifelib's times for the same book:

```bash
python crates/watershed-gpu/examples/shared/amendments_reference.py export
cargo run --release --example amendments -- export
```

The JAX version of the insurance model, for comparison on the same card:

```bash
pip install "jax[cuda12]" numpy
python crates/watershed-gpu/examples/shared/savings_jax.py export 20000 10000 2500 sorted
```

`dosing` needs a few hundred patients worked by an independent solver:

```bash
pip install numpy scipy
mkdir out && python crates/watershed-gpu/examples/shared/dosing_reference.py out
cargo run --release --features cuda --example dosing -- out
```

Its model's numbers are as recalled from the paper it names and have not
been checked against it; it shows the method and is not advice on dosing.

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
  20-million-node insurance book needs room for 344,124 numbers (2.75 MB),
  not 20 million, so about 6,800 scenarios fit on a 24 GB card at once and
  10,000 are run in two parts.
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
- A table cannot be looked up by a varied input, so a model where each row
  reads a rate for its own age has to be built with the rates fixed in.
- `Loaded` needs every scenario to fit on the card at once.
- `Loaded::totals` adds up; a count above a threshold, or a percentile,
  still needs every scenario's answer copied back.
- Making a plan reads the graph's nodes one after another to find the order
  to work them out in, and that part uses one core.
- The spreadsheet example has been tried on one workbook. It reads `.xlsx`
  only, and understands the functions listed at the top of the file.
