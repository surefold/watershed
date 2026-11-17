# Limits

What Watershed does not do, or does differently in each way of running. The
[README](../README.md#status) says what is built, experimental and planned.

## What both ways of running can take

A model runs under both patterns if every node between the scenario inputs
and outputs is in the first row. Nodes elsewhere in the graph can be
anything: only the part a plan is compiled from matters, and any part of
the graph that no scenario input reaches is folded to constants, whatever
it contains.

| | Constructs | In a model meant for both |
|---|---|---|
| Both patterns | `f64` and `bool` nodes: `+ - * /`, `min`, `max`, `abs`, `exp`, `ln`, `sqrt`, `pow`, `sin`, `cos`, comparisons, and/or/not, conditionals (`when`), however deeply nested; `sum`, `min`, `max` and `count` totals over a collection or a grouping; a count compared, or the smaller or larger of two taken; `choose` whose option number is the same in every scenario | Use directly. A lookup by a value that varies per scenario is nested conditionals, as the spreadsheet example writes `VLOOKUP` |
| Incremental only; could be lowered to a plan, not implemented | `i64` arithmetic (adding to a count, say); `choose` whose option number varies per scenario | Integers: `f64`. A varying choice: nested conditionals |
| Incremental only | Exact decimals, text and lists of text, errors as values | No equivalent. `f64` in place of a decimal gives up exactness |

A plan refuses an unsupported node when it is compiled, naming the node, so
a model is never silently run differently. Nothing needs rewriting for the
incremental engine; what changes is how totals and lookups are written in
the part of a model that scenarios run through.

History, change sinks, pull mode and `redefine` are features of the graph,
not of a model's formulas, and do not apply to a plan: a plan is a snapshot
of the graph as it was when compiled.

## Scenario plans

- CPU plan results are bit-identical to the graph's. GPU results are too,
  except that a NaN may carry a different payload, `min`/`max` of `0.0` and
  `-0.0` follow x86-64, and `exp`, `ln`, `pow`, `sin` and `cos` may differ in
  the last one or two digits. This is established by tests, not by proof.
- A lookup written as nested conditionals evaluates every row, in pieces
  of about fifteen rows a node, rather than searching the table.
- A conditional evaluates both branches in a plan and selects one, which
  matches the graph only because nothing in the supported subset can fail.
- Loops are unrolled when the graph is built, so step counts are fixed.
- The GPU executor is CUDA only. Graph construction and most of plan
  compilation are single-threaded, at about 1 µs per node each.

## The graph

- One graph lives in one process. What can be spread out is a plan: saved
  to a file, its scenarios run in lots by separate workers. The engine has
  the saving and loading ([watershed-gpu](../crates/watershed-gpu/README.md#running-a-batch-somewhere-else));
  the workers and their coordinator are in
  [watershed-jvm](https://github.com/surefold/watershed-jvm), experimental,
  and have run on eight GPUs in one machine.
- Small and medium updates are limited by memory access; multi-threading
  helps only when a large part of the graph is recalculated.
- `remove` frees a node's name, its work and what it held, but not its
  slot (some tens of bytes), since ids are never reissued. A removed
  collection row's nodes, its group membership nodes among them, are reused
  by the next new row (with history on, membership nodes are kept). A
  grouping in the graph costs a membership node a row (shared by the
  groupings by one key) and a member of a total a row and totalled column:
  74 MB for 50,000 rows with six totals.
- `redefine` and `remove` cause the next update to rebuild the graph's
  reverse links, in time proportional to graph size.
- In pull mode, nodes that a collection grouping is built on are always
  calculated.
- History cannot restore the graph to an earlier update, does not keep a
  redefined node's earlier expressions, and does not cover a collection's
  groups.
- Decimals hold 28 significant digits.

## Bindings and front ends

- The Python package is not published and does not expose history, sinks,
  `redefine` or scenario evaluation.
- In a browser a plan runs on one thread, and only the example models are
  exposed.
- The grid falls back to summing groups outside the graph for large grouped
  tables. Its decimal columns hold 28 significant digits.
- The Rust crates are not published on crates.io.
