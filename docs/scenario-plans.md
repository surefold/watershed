# Scenario plans

How `watershed-gpu` turns a graph into something that can be evaluated for
many input sets, and why that needs far less working memory than one value
per node. The code is in
[`crates/watershed-gpu/src/plan.rs`](../crates/watershed-gpu/src/plan.rs)
(compilation and the CPU executor) and
[`cuda.rs`](../crates/watershed-gpu/src/cuda.rs) (the GPU executor).

## Compilation

`Scenarios::with_shared(&graph, &vary, &shared, &outputs)` compiles a plan
in six steps.

1. **Sub-graph extraction.** A walk back from the outputs over the graph's
   dependency edges finds every node they can depend on. Nothing else is
   compiled.
2. **Classification.** A node that depends on a varied input is evaluated
   once per scenario. A node that depends only on shared inputs is evaluated
   once per `set_shared`, on the CPU. Every other node is folded into a
   constant at its current value.
3. **Translation.** Each per-scenario node's expression becomes a short
   postfix program over slots, constants and shared values. A conditional
   evaluates both branches and selects; that matches the graph's lazy
   evaluation because nothing in the supported subset can fail. This step
   runs on all cores.
4. **Levels.** A node's level is one more than the highest level it reads.
   Nodes in a level do not read each other, so a level can be evaluated in
   any order, or all at once.
5. **Slot allocation by liveness.** Described below.
6. **Shape grouping.** Nodes with the same operation sequence and different
   operands share a shape. Within a level, nodes are ordered by shape, so
   each (level, shape) group is a contiguous range that one kernel launch
   evaluates.

The plan is a snapshot. Later changes to the graph do not reach it, and a
change to the graph's structure needs a new plan. Shared inputs are the
exception: `set_shared` replaces their values without recompiling.

## Memory reuse

A naive executor keeps one value per node per scenario. For the insurance
benchmark's 19,889,057 nodes that is 159 MB per scenario, so a 24 GB card
would hold about 120 scenarios at a time.

Most of those values are dead long before the run ends. A node in a
month-by-month projection is typically read by nodes one to a few levels
later and never again. The plan uses the dependency information it already
has to exploit that:

1. For every node, find the **last level that reads it**. Outputs are
   treated as read at the end.
2. Walk the levels in order. Before evaluating level *L*, release the slots
   of all nodes whose last reader was in a level below *L*.
3. Give each node of level *L* a free slot, lowest-numbered first, or a new
   slot if none is free.

A released slot is only handed to nodes of a later level than its last
reader, so no node of a level overwrites a value another node of the same
level still has to read. That is what makes it safe to evaluate a whole
level in parallel.

Taking the lowest free slot first keeps the live slots packed together and
gives a level's nodes mostly consecutive slots. The CPU executor is
noticeably faster for it, because it reads memory in order.

```
level:      1        2        3        4
          a  b     c  d       e        f
reads:    in in    a  b     c  d       e

slot 0:   a ─────► freed after 2 ─► e ─► freed after 4
slot 1:   b ─────► freed after 2          f  (output: kept)
slot 2:            c ─────► freed after 3
slot 3:            d ─────► freed after 3
```

Six nodes need four slots here, and a long chain needs only a handful
however long it is: a 1,000-node chain where each node reads the one before
it compiles to at most four slots, its input included (this is a test,
`a_chain_needs_room_for_a_few_nodes_at_once_not_all_of_them`).

### What it achieves, and what it does not

For the insurance plan the result is **344,124 live slots for 19,889,057
nodes**: 2.75 MB of working memory per scenario where one value per node
would take 159 MB. That is the figure
`Scenarios::bytes_per_scenario` reports.

- It raises the **scenario batch size**. On a 24 GB card with 80% of free
  memory allowed, 6,807 scenarios fit at once, so 10,000 run in two parts
  where they would otherwise run in more than eighty.
- It does **not** shrink the plan itself. The device also holds two `u32`
  per node (where its operands start, and its slot: 159 MB for this plan),
  one `u32` for every value a node reads, and the constants. Those tables
  are the same size with or without slot reuse.
- It does **not** mean a run uses little device memory. The executor uses
  the memory it saves to run more scenarios at once: the working table is
  slots × scenarios × 8 bytes, which for 6,807 scenarios is about 18.7 GB.
- How much it saves depends on the graph. A value read by a much later
  level stays live for all the levels in between. A graph with long-range
  reads, or one that is wide and shallow, saves less.
- Inputs that vary per scenario keep their slots for the whole run, and so
  do outputs.

## Execution

**On CPU threads** each scenario is one task: an interpreter runs the
nodes' programs in level order against that scenario's slots.

**On a GPU** the working table has one row per slot and one column per
scenario. For plans with at most 256 shapes a CUDA kernel is generated for
each shape and compiled with NVRTC on first use; a launch evaluates one
(level, shape) group for every scenario, each thread computing one node for
one scenario. Plans with more shapes use a single interpreter kernel. Fused
multiply-add is disabled so that results match the CPU.

`Gpu::run` copies the inputs up and the outputs back on every call.
`Gpu::load` keeps the plan, the inputs and the working table on the device;
`Loaded::run` and `Loaded::totals` then rerun after `set_shared` or
`set_inputs` without copying what has not changed, and `totals` reduces
each output on the device in a fixed block order that
`Scenarios::totals_cpu` reproduces exactly. `load` requires every scenario
to fit at once.

Measured stage costs for the insurance plan, and the cases where the CPU is
faster, are in [benchmarks.md](benchmarks.md) and the
[watershed-gpu README](../crates/watershed-gpu/README.md#what-it-has-been-tried-on).
