# Santa 2025 on Watershed

An experiment: how would Watershed have done on Kaggle's
[Santa 2025 Christmas Tree Packing Challenge](https://www.kaggle.com/competitions/santa-2025)?
For each n from 1 to 200, pack n copies of a 15-sided tree into the
smallest square; the score is the sum of side² / n, lower is better. The
winning score was 68.78.

It is a standalone crate, outside the main workspace and its CI.

## What is here

| File | What it is |
|---|---|
| `src/geom.rs` | The tree as four convex pieces, and a hand-written overlap test (separating axes). |
| `src/engine.rs` | Two scorers behind one trait: `Native` (hand-written) and `Shed` (the same arithmetic as a Watershed graph). |
| `src/main.rs` | A starting layout (a lattice of upright and inverted trees), simulated annealing that moves one tree at a time, and the `bench` and `solve` commands. |
| `verify.py` | Scores a submission independently with shapely, the way the competition does. |

The Watershed model, for n trees:

- four inputs a tree: `x`, `y`, and the angle as its cosine and sine,
  because formulas have no `sin` or `cos`;
- a node for each vertex coordinate, edge normal and edge offset;
- one node for each pair of trees, holding how far they overlap (0 if
  apart). It is a `when` on the distance between centres, then a `when` for
  each pair of pieces on their circles, then the separating-axis test;
- graph aggregates for the bounding box (`min`/`max` over vertices), the
  total overlap (`sum` over pairs) and a pull towards the centre.

A move sets one tree's inputs and calls `stabilize`. A rejected move sets
them back and calls it again.

## Running it

```bash
cargo run --release -- bench 200 5
```

```bash
cargo run --release -- solve native 12 native.csv
```

```bash
cargo run --release -- solve watershed 12 watershed.csv
```

```bash
pip install shapely && python3 verify.py watershed.csv
```

`bench <n> <seconds>` runs the same search with each scorer from the same
start. `solve <native|watershed> <seconds per puzzle> <out.csv> [threads]`
solves all 200 puzzles and writes a submission file.

## Results so far

Measured once each, 5 seconds a run, on a 4-core cloud container with no
GPU. Moves a second in the same search from the same start:

| Trees | Hand-written | Watershed | Ratio | Watershed graph |
|---|---|---|---|---|
| 10 | 794,000 | 5,300 | 150× | 953 nodes, 82 recalculated a move |
| 50 | 569,000 | 2,600 | 220× | 5,733 nodes, 121 recalculated a move |
| 200 | 471,000 | 1,100 | 420× | 37,908 nodes, 272 recalculated a move, 11 s to build |

Before the per-piece circle checks were added to the model, Watershed was
about 4.5 times slower than this.

The scores of a full solve are not recorded here yet.

## Why Watershed is slower here

Read from the code and the timings; not yet profiled.

1. **Interpreted against compiled.** A pair node is a program of about
   2,500 instructions. Having conditionals, it runs on the general
   interpreter (tagged 16-byte values), not the float-only one.
2. **A rejected move is two updates.** The hand-written scorer stops at the
   first overlap having changed nothing. Most moves are rejected.
3. **Every update visits all of a tree's pairs.** 199 pair nodes are queued
   and tested when only a handful are near.

## Next steps

On a machine with a GPU:

- **Scenario plans.** Not tried. Score a batch of candidate positions for
  one tree as a plan (`watershed-gpu`), on CPU threads and on CUDA. Note
  that a plan evaluates both branches of a conditional, so the distance
  checks save nothing there, and a plan is a snapshot, so it needs the
  other trees as shared inputs or recompiling after each accepted move.
- **Profile one update** to split the time between interpretation, read
  tracking and queueing, before changing the engine.

In the engine, in the order they seem worth doing:

- trial updates with a cheap rollback, so a rejected move costs one update;
- conditionals on the float fast path (`FloatOp` has no compare or jump);
- not sorting and reallocating a conditional node's read set on every
  evaluation;
- dependencies on neighbours only (a spatial join), so a move wakes the
  nearby pairs and not all of them;
- compiling formulas to machine code.
