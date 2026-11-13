# Testing and correctness

What the tests check, where they are, and how to judge the engine without
reading its implementation.

## Running the tests

```bash
cargo test --workspace                              # everything that needs no GPU
cargo test -p watershed-gpu --features cuda         # adds the GPU tests (needs CUDA and an NVIDIA GPU)
cd crates/watershed-py && uv run pytest             # the Python bindings
```

CI ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs
formatting, clippy with warnings denied (the workspace, and the core without
its `parallel` feature), the workspace tests on stable Rust and on Rust 1.85
(the minimum supported version), the documentation build, the `tour`,
`benchmark` and `collection` examples, a WebAssembly build of
`watershed-web`, and the Python tests. CI does not run the GPU tests.

## Where the tests are

| Where | What it checks |
|---|---|
| [`crates/watershed-core/tests/behaviour.rs`](../crates/watershed-core/tests/behaviour.rs) | One test per guarantee, named as a sentence: `only_nodes_that_depend_on_a_change_are_recalculated`, `an_update_stops_where_a_value_comes_out_the_same`, `a_node_reached_by_two_paths_is_recalculated_once_and_never_sees_half_an_update`, and so on. The list of names is the specification. |
| [`crates/watershed-core/tests/properties.rs`](../crates/watershed-core/tests/properties.rs) | Property tests (proptest). They build thousands of random graphs with awkward values (NaN, infinity, integer overflow, failed inputs), apply random updates, and check the invariants below after every update. Separate properties cover history, node removal, pull mode, collections and collection history. |
| [`crates/watershed-core/tests/decimal.rs`](../crates/watershed-core/tests/decimal.rs), [`history.rs`](../crates/watershed-core/tests/history.rs), [`sink.rs`](../crates/watershed-core/tests/sink.rs) | Decimal arithmetic and rounding, reading earlier updates, and change sinks. |
| Doc examples in `crates/watershed-core/src/` | The examples in the API documentation run as tests. Two in [`lib.rs`](../crates/watershed-core/src/lib.rs) must fail to compile: a number plus a string, and setting a calculated node. |
| [`crates/watershed-gpu/tests/scenarios.rs`](../crates/watershed-gpu/tests/scenarios.rs) | Scenario plans against the graph, and with `--features cuda` the GPU against the CPU. See below. |
| [`crates/watershed-grid/tests/grid.rs`](../crates/watershed-grid/tests/grid.rs) | The grid's table logic, natively. |
| [`crates/watershed-parquet/tests/roundtrip.rs`](../crates/watershed-parquet/tests/roundtrip.rs) | Changes written to Parquet and read back. |
| [`crates/watershed-py/tests/test_watershed.py`](../crates/watershed-py/tests/test_watershed.py) | The Python API. |

## Invariants the property tests check after every update

1. **Same result as starting from scratch.** Every value reached by
   incremental updates is identical to recalculating the whole graph from
   nothing. This is the definition of correct, and the from-scratch version
   is a simple loop over the reference evaluator.
2. **Nothing is recalculated twice** in one update.
3. **Exactly the right nodes are recalculated.** A node is recalculated if
   and only if something it read changed.
4. **Inputs are never recalculated**, and the update's report adds up.
5. **Order is right.** A node is recalculated only after everything it uses.

The compiled instructions are also checked against the reference evaluator
after every update.

## Scenario plans against the graph

The claim that a plan gives what the graph gives rests on tests, not on a
proof:

- `gives_what_the_graph_gives` builds 40 random graphs of arithmetic,
  conditions and flags with varied, shared and fixed inputs, and requires
  the plan's results to equal the graph's bit for bit. It runs against the
  CPU executor and, with `--features cuda`, against both GPU executors (the
  generated kernels and the general kernel).
- `exp`, `ln` and `pow` on the GPU are tested to a relative 10⁻¹⁴ of the CPU
  result, not to bit equality.
- The larger examples assert bit equality between the graph and the plan on
  a sample of scenarios, and several are checked against independent
  references (lifelib, SciPy, Excel's stored values, hand-written loops).
  The watershed-gpu README lists
  [what each was checked against](../crates/watershed-gpu/README.md#what-it-has-been-tried-on).

## Mutation checks

A test suite shows little unless it fails when the code is wrong. Each of
the following bugs was introduced into the engine by hand, one at a time,
and each made the tests fail. This is a record of manual checks made as the
corresponding features were written. CI does not repeat them.

- No cutoff: always propagate, even when a value is unchanged.
- Fail to propagate when a value does change.
- Recalculate the highest nodes first instead of the lowest.
- Never clear a node's "needs recalculating" flag.
- Treat setting an input to its current value as a change.
- Compute a node's level from its first input instead of its highest.
- Let the type checker accept `&&` on numbers.
- Compare floats with `==` for cutoff (so NaN never equals NaN).
- Evaluate the wrong branch of a condition.
- Fail to recalculate a node given a new expression by `redefine`.
- Skip a redefined node with a condition because what it read under its old
  expression did not change.
- Copy a column's node to a new row still reading the first row's fields.
- When many rows change at once, rebuild only the lower half of a total's
  partial results.
- Never move a row whose calculated group changed during an update.
- Count a row that joins a group during an update from the next update
  instead.
- Leave the links to a removed node in place.
- Remove a node that something still uses.
- Treat a decimal written differently (`1.50` for `1.5`) as no change.
- Round a tie away from zero when half-even was requested.
- In pull mode, skip a woken node because what it read did not change in
  this update.
- In pull mode, watch a node without what it uses.
- Explain an earlier update by the branch of a condition that was not taken
  then.
- Explain a total as of an earlier update with members that joined later.

## Reviewing the engine

The update loop is `Graph::stabilize` in
[`crates/watershed-core/src/graph.rs`](../crates/watershed-core/src/graph.rs).
A reviewer needs that function and the five invariants above.
