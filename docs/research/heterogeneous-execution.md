# Cost-based heterogeneous execution for Watershed: a research note

13 November 2026. Code examined: `surefold/watershed`, `main` at `8045417`. Branch for this note: `research/heterogeneous-execution`. Nothing in the library was changed; two experimental examples were added (`crates/watershed-gpu/examples/hetero_probe.rs`, `hetero_cones.rs`).

Machine: Ryzen 9 5950X (16 cores, 32 threads), 47 GB, one RTX 3090 24 GB, WSL2, CUDA 13.1, rustc 1.99. Everything is double precision.

Labels: **measured** (run here for this note; median, range and sample count given), **earlier** (measured on this machine on the same day, in `measurements.md` of the earlier investigation; not re-run), **source** (read in the code or a primary document), **reported** (from a literature search; "opened" means the primary text was read, "snippet" means only a search result was seen), **estimated** (arithmetic on measured constants), **judgement**.

## 0. Corrections to the brief

- The brief names the repository `matthewjones372/surefold` and asks that the design respect a Kotlin/JVM architecture. The code is a Rust workspace at `surefold/watershed` (crates `watershed-core`, `-gpu`, `-grid`, `-parquet`, `-py`, `-web`). JVM bindings are in a separate private repository that I did not have. The sketches in section 8 are therefore Rust, in the style of the existing crates. The intent of the instruction (small, composable, type-safe, few dependencies) is kept.
- The published benchmark page was audited in the earlier investigation; I did not re-audit it.
- An unpublished branch (`fusion`) has an in-progress experiment that groups several nodes into one kernel. I read it as evidence of what partitioning looks like in this code. It is unfinished, and nothing below depends on it.

## 1. Executive assessment

**Can Watershed use CPUs and GPUs together efficiently?** It already does in the one arrangement that pays: nodes that are the same in every scenario are worked out once on the CPU, and nodes that differ by scenario go to one device. Splitting the *per-scenario* work of one batch run between the two devices does not pay on this hardware. The GPU's compiled kernels do about 50 times the work per second of the plan's CPU interpreter on 32 threads (measured, section 4), so the most the CPU can add by working at the same time is a few percent, and every cut between devices costs a host round trip.

**Does the current graph representation support it?** The graph does: a region can be taken out by naming its outputs, because a plan is made from "what these outputs depend on". The plan and the GPU code do not: a plan's inputs must be graph inputs, so a region cannot *start* at interior nodes; two plans cannot share device memory; working memory is reused level by level, so intermediate values are gone when a run ends; and a loaded plan can only be run in full.

**Are there realistic workloads where hybrid beats both?** I found one structural case and one operational case.

- Structural: a deep, narrow per-scenario part (thousands of levels, a few nodes wide) feeding a wide part. The GPU pays about 7 µs for each kernel launch whatever the width, so the narrow part is cheaper on the CPU at moderate scenario counts. Measured gains are in section 6. This is an artefact of one launch per (level, shape); fusing levels or CUDA Graphs would remove most of it without any planner.
- Operational: after a batch run, something small changes (one policy, one late month). Recalculating only what depends on it, for every scenario, and re-adding the totals from retained partial sums, was 62 to 530 times faster than running the batch again in the prototype (section 6, test A), with identical bits. The device this small part runs on mattered by a factor of about 3, in the CPU's favour. Knowing that the part is small is what produced the gain.

**Could incremental dependency tracking give an advantage over existing heterogeneous schedulers?** Yes, and it is the only part of the hypothesis I would call both distinctive and supported. StarPU, Legion, Dask and the GPU database planners decide *where* to run a task they have already decided to run. None takes "which tasks need not run at all, given what changed and what is still valid on which device" from a dependency tracker. Watershed has that tracker. But on the real insurance book the benefit is narrow: realistic assumption stresses reach 10% to 100% of the plan and would need 5 to 35 times the device memory per scenario to keep the values they read (measured, section 5.3).

**What is established in prior research?** Cost models with a transfer term calibrated from history (StarPU); tracking which device holds a valid copy (StarPU, Legion, Kokkos DualView); the finding that placing each operator separately loses to placing whole regions where the data already is (CoGaDB, HERO); a size threshold for choosing CPU or GPU for an incremental update (GPU-accelerated OpenTimer); grouping a levelled scalar graph into kernels over batched stimuli (RTLflow). All of these work on tasks of microseconds to milliseconds. None places single scalar operations.

**What is distinctive?** Not the planner. The distinctive asset is that one model has both an exact dirty set (from the incremental engine) and a compiled batch form, held to bit equality by tests. The useful step is to let the batch form re-run only the dirty part. That is an incremental-batch feature first and a device-placement feature a distant second.

## 2. Existing implementation

Paths are relative to the repository root.

### 2.1 The ten questions

1. **Nodes and dependencies.** `crates/watershed-core/src/graph.rs`, `struct Graph`: flat arrays indexed by node id. A 16-byte `Hot` record per node (program location, length, height, kind). Forward edges are in each node's compiled expression (`Graph::dependencies`, `Graph::expression`). Reverse edges ("users") are packed back to back in `packed`/`packed_start`, with recent additions in linked lists (`edges`) until `Graph::repack`. Each reverse edge carries the user's height.
2. **Affected nodes.** `Graph::stabilize`: `set` records pending input values; `stabilize` stores those that differ, calls `mark_dependents`, and processes one queue per height, lowest first. `recalculate` stops propagation when the new value equals the old (`Value::same_as`), and skips a conditional node when nothing it read last time changed (`reads_unchanged`). Large levels go to all cores (`recalculate_in_parallel`, threshold 32,768 nodes). The result is exact: `Report::recalculated`, `Graph::last_recalculated`, and per node `changed_in` (private) give the dirty set after the fact. A `ChangeSink` (`sink.rs`) is called with the changed nodes at the end of each update.
3. **GPU plans.** `crates/watershed-gpu/src/plan.rs`, `Scenarios::with_shared`: depth-first cone from the outputs; classification of each node as per-scenario, shared-only or constant; translation to postfix instructions (`Program::translate`, in parallel by `rough`); levels; slots by liveness; shapes; `(level, shape)` groups. `cuda.rs`, `source_for`: one generated kernel per shape, compiled by NVRTC, launched once per group (`Gpu::work_out`, `launch_shape`). Plans with more than 256 shapes use one interpreter kernel per level (`kernel.cu`, `launch_level`).
4. **Isolating a region.** Downward-closed regions only. `Scenarios::new(g, vary, outputs)` extracts exactly what `outputs` depend on, so "the sub-graph feeding these nodes" is one call (test `only_what_the_outputs_need_and_the_inputs_reach_is_calculated`, `tests/scenarios.rs`). A region cannot begin at interior nodes: `vary` and `shared` must be inputs (`PlanError::NotAnInput`). To run "the part downstream of a cut" the prototype had to build a second graph whose inputs stand for the cut values.
5. **Intermediate results.** In the graph: one `Value` per node, always current after `stabilize`, optionally with history (`Graph::keep_history`, `value_at`). In a plan run: a table of `slots × scenarios` doubles; a node's slot is reused after the last level that reads it (`plan.rs`, "Give each node a slot"). The insurance plan keeps 344,124 slots for 19.9 million nodes (earlier). So after a run only outputs and whatever happened to be live last survive. Intermediates are not addressable afterwards.
6. **GPU buffers.** `cuda.rs`, `Gpu::device`: per run, the plan tables (`operands`, `nodes`), `consts`, `shared` and the working table `vals` are allocated and uploaded. `Gpu::run_within` does this on every call and frees on return. `Gpu::load` returns a `Loaded` that keeps them; `Loaded::run`/`totals` reuse them; `Loaded::set_inputs` replaces every input. `Loaded::work_out` allocates and uploads a fresh `shared` buffer on every run. Uploads use pageable memory and a host-side transposition from scenario-major to input-major (`Gpu::give`); downloads the reverse (`Gpu::take`).
7. **Caching.** Compiled kernels are cached per `Gpu` by a hash of the shape list (`Gpu::shapes`). Plans are not cached by the library: the caller holds the `Scenarios`. There is no graph structure version; `Graph::epoch` counts updates only. A plan is a snapshot ("later changes to the graph do not reach it", `Scenarios` doc comment) and nothing detects a stale one.
8. **What crosses the boundary.** Up: plan tables once per `device`; `vary × scenarios` inputs per run; shared values per run. Down: `outputs × scenarios` (`take`), or one number per 1,024 scenarios per output (`Loaded::totals`). Nothing else.
9. **Topology and performance.** Launches = number of `(level, shape)` groups, about 7 µs each (measured, section 4). Width × scenarios sets the work per launch. Long-lived values raise slots per scenario and lower how many scenarios fit. Shape count above 256 switches to the slower interpreter kernel.
10. **Equivalence.** `tests/scenarios.rs`, `gives_what_the_graph_gives`: 40 random graphs; the CPU plan, the general GPU kernel and the compiled kernels must equal the graph bit for bit. Fused multiply-add is disabled (`cuda.rs`, `options`). Stated exceptions on the GPU: NaN payloads, `min`/`max` of ±0, and last-digit differences in `exp`, `ln`, `pow` (`lib.rs` crate docs; test `maths_functions_are_within_a_few_last_digits_of_the_cpu`). Totals use the same 1,024-scenario blocking on both devices (`plan.rs`, `BLOCK`, `total`). Every strategy in the prototype was checked for bit equality against the others before a time was printed; all passed (the synthetic model uses no `exp`/`ln`/`pow`).

### 2.2 Where the device is chosen

By the caller, per call, for the whole plan:

- incremental, one scenario, CPU: `Graph::stabilize`;
- batch on CPU: `Scenarios::run_cpu`, `totals_cpu` (`plan.rs`);
- batch on GPU: `Gpu::run`, `run_within`, `Gpu::load` then `Loaded::run`/`totals` (`cuda.rs`).

The examples wrap this in a `Runner` that picks the GPU when the feature is compiled in (`examples/shared/savings.rs`, `Runner::run`). There is one automatic fallback: `run_within` uses `run_cpu` when the plan has no per-scenario nodes.

### 2.3 Does the code assume one backend for the whole graph?

For the per-scenario part, yes: one `Scenarios` goes to one executor. There are two existing exceptions, both fixed by classification and neither by cost:

- **Shared-only nodes run on the CPU** (`Scenarios::set_shared`), once per call, with the same interpreter; their results are uploaded as the `shared` buffer. This is hybrid execution with a cut chosen by a rule: "does not depend on a varied input".
- **Constant nodes are never run by a plan**: their values are read from the graph when the plan is made (`Builder::fixed`). The incremental engine is, in effect, the executor for that region, and the plan is stale if it later changes.

`set_shared` recomputes every shared node, not only those downstream of the shared inputs that changed.

### 2.4 Which of the brief's five responsibilities exist

| Responsibility | Exists? | Where |
|---|---|---|
| Computational graph | Yes | `watershed-core`, `Graph` |
| Incremental engine | Yes | `Graph::stabilize` |
| Execution backend | Yes, three | graph evaluator; `Scenarios::run_cpu`; `cuda.rs` |
| Execution plan (an IR) | Yes | `Scenarios` (levels, slots, shapes, groups) |
| Cost model | No | the only cost-like constants are `DEFAULT_PARALLEL_THRESHOLD` (graph.rs) and `MOST_SHAPES` (cuda.rs) |
| Execution planner | No | the caller |

## 3. Prior art

A literature search was run for this note. "Opened" and "snippet" follow the labels above; the list of URLs is in section 12. Compiler-based planning and run-time scheduling are separated in the last column.

| System | Representation | Who assigns devices | Transfer cost in the decision | Cost model | Moves between devices mid-run | Keeps results on device; validity | Reuses work after inputs change | Multi-machine | Kind |
|---|---|---|---|---|---|---|---|---|---|
| StarPU (handbook and ICPADS'10 paper opened) | Codelets with per-device implementations over registered data handles | Run-time scheduler (`dmda`, `dmdas`) | Yes: minimises execution + transfer estimate; bandwidth and latency measured per pair of memory nodes | Measured: per-footprint history and fitted regressions | Per task | Yes; tracks where valid copies are and prefers them | No | Yes (MPI layer) | Run-time |
| PaRSEC (snippets only) | Parameterised task graph | Run-time | Not verified | Not verified | Per task | Device data retained (snippet) | No | Yes | Run-time |
| Legion (mapper docs opened) | Tasks with privileges on logical regions | A mapper, user-written or default | No built-in model; mapper sees valid instances | Heuristic; profiling available to mappers | Per task | Yes; physical instances persist, validity tracked by the runtime | No | Yes | Run-time, policy by user |
| Kokkos (DualView doc opened) | Parallel patterns over views | The programmer | No | None | No | `DualView` holds both copies with modified flags the user sets; `sync` copies if stale | No | No (by itself) | Compile-time, by hand |
| SYCL (snippets) | Buffers and accessors form an implicit task graph | The programmer, per queue | No | None | No | Runtime manages buffer copies | No | No | Compile-time, by hand |
| OpenMP target (snippets) | Pragmas | `device` clause; `if` clause can fall back to host | No | None found | No | `target data` mappings persist | No | No | Compile-time, by hand |
| JAX / XLA (FAQ snippet) | Traced array program | Follows where the data was committed; mixing devices is an error | No | None | No | Arrays stay on their device | No | Separate mechanisms | Compiler, by hand |
| TensorFlow (whitepaper, `placer.h`, GPU guide opened) | Dataflow graph | Placer: constraints and colocation groups; GPU preferred when a kernel exists | The 2016 whitepaper describes a greedy placement with a cost model including communication; the open-source `placer.h` describes only constraints. My reading: the cost-based placer did not ship | Described as static or measured in the whitepaper | No | Yes | No | Yes | Compiler |
| MLIR gpu dialect (doc partly opened) | IR with explicit `gpu.launch` regions | The programmer or a front end | No | None; no partitioner found | No | n/a | No | No | Compiler |
| CUDA Graphs (two NVIDIA blogs opened) | A captured DAG of kernel launches and copies | n/a (one device) | n/a | n/a | n/a | n/a | Parameters of a node can be updated without re-capture | No | Launch batching |
| Dask / Dask-CUDA (scheduler docs opened) | Task graph of Python calls | Run-time scheduler; GPU eligibility by user-declared resources | Yes: dependency sizes and measured bandwidth, with worker occupancy | Measured, adaptive | Per task | Results stay on workers; spill to host | No | Yes | Run-time |
| ONNX Runtime (opened), TVM BYOC (snippet) | Operator graph | Greedy partition by operator support, largest sub-graphs first | No; documentation warns that inserted copies can hurt | None | Only at sub-graph boundaries | n/a | No | No | Compiler |
| HyPE / CoGaDB; Breß, Funke, Teubner, SIGMOD 2016 (opened) | Query operator DAG | Run-time, data-driven: an operator runs on the co-processor only if its input is already there | Central | Learned per operator; they report compile-time cost-based placement as fragile, with slowdowns up to 24× from cache thrashing | At chunk level | Device cache of columns | No | No | Run-time |
| HERO; Karnagel, Habich, Lehner, PVLDB 2017 (opened) | Query operator DAG | Run-time, per "execution island" | "Dominant"; per-operator placement "introduc[es] many additional transfers" | Measured | Per island | Copies kept, stale copies deleted on update | No | No | Run-time |
| GPU-accelerated OpenTimer; Guo et al., TCAD 2023 (opened) | Levelled timing graph | A size threshold: GPU when propagation candidates are many, else CPU | Implicit in the threshold | Empirical; break-even about 67,000 candidates | No | Graph arrays on device | **Yes: incremental timing** | No | Run-time rule |
| RTLflow; Lin et al., ICPP 2022 (opened) | Levelled RTL graph, batched stimuli, generated CUDA | GPU only | n/a | n/a | No | n/a | No | No | Compiler |

Task granularity matters more than any column above. StarPU's handbook puts scheduling overhead at "a couple of microseconds" per task. The Task Bench study (abstract opened) puts the smallest task that the best systems handle well at about 100 µs. CoGaDB reports several microseconds per cost estimate. A Watershed node costs 0.02 to 2 ns per scenario and about 100 ns on the incremental path. Any decision must therefore be per region and made once per plan, never per node or per run of a node.

DAG scheduling algorithms (HEFT, CPOP, PEFT; snippets only) rank tasks by estimated finish time including communication. StarPU's first schedulers were named after HEFT (ICPADS'10, opened). They assume task costs per device are known and tasks are coarse; they are a source of vocabulary here, not an algorithm to adopt.

### Closest systems

1. **StarPU `dmda`**: the reference design for "execution estimate + transfer estimate + where the valid copy is". Three orders of magnitude coarser than a Watershed node.
2. **CoGaDB / HERO**: the same decision shape (operator DAG, device cache, learned costs) and the most relevant negative results: place regions, not operators; let data location drive placement; expect compile-time estimates to be wrong.
3. **GPU-accelerated OpenTimer**: the only system found that pairs incremental updates on a levelled DAG with a CPU/GPU choice. It uses a size threshold, with no cost model and no tracking of partially valid device state.
4. **RTLflow**: almost exactly Watershed's batch path. It reduces launch overhead by partitioning into larger kernels and using CUDA Graphs, not by moving work to the CPU.

### Not found

Self-adjusting computation, incremental view maintenance or differential dataflow with GPU execution; any cost model whose inputs are a dirty-set size from a dependency tracker and partial validity of device-resident results. The searches are listed in section 12. An absence in one bounded search is not proof.

## 4. Cost model

### 4.1 Constants on this machine

All **measured** for this note with `hetero_probe` unless marked. Format: median (min..max, n). Raw output is in `docs/research/heterogeneous-execution-data/`. The synthetic model's nodes are simple (two or three operations each); the insurance book's are heavier, so its per-node figures are about twice these (earlier).

| Component | Value | From |
|---|---|---|
| Incremental update, per affected node | 0.18 µs on the insurance book (129 µs for 711 nodes; 587 ms for 3.29 million) | `hetero_cones`, n=1 each |
| Plan interpreter, one thread | 18.0 ns per node (51.96 ms (51.36..52.27, n=7) for 2,883,002 nodes, S=1) | `crossover-large` |
| Plan interpreter, 32 threads | 1.18 to 1.21 ns per node-scenario for S ≥ 64: a speed-up of 15 | `crossover-medium`, `-large` |
| Thread-pool overhead for a small job | 324 nodes: 6.2 µs at S=1, 293 µs at S=4, 1.91 ms at S=64 (n=7 each) | `crossover-small` |
| GPU compiled kernels | 0.021 ns per node-scenario (247.95 ms compute for 2,883,002 × 4,096; 49.42 ms for 144,287 × 16,384) | `crossover-large`, `-medium` |
| Kernel launch | 6.7 to 7.4 µs per level on a chain one node wide (three scenario counts, n=7 each) | `launch` |
| GPU fixed cost per `run_within` | about 1.1 ms for a 324-node plan at S=1 (1.08 ms (1.03..1.34, n=7)) | `crossover-small` |
| Plan tables to the device | 11 ms for 2.88 million nodes, about 3.7 ns a node, every `run_within` | `crossover-large`, upload at S=1 |
| Inputs host to device | 1.6 GB in about 680 ms: 2.4 GB/s, 3.4 ns a value, including the host-side transposition | `launch`, S=100,000 |
| A value crossing a cut, down and up | about 23 ns (94 ms a cut for 4 million values each way) | `transfer`, section 6 E |
| Plan construction | 0.39 µs a node (1.113 s (1.097..1.113, n=2) for 2.88 million); 0.9 µs on the insurance book (earlier) | `repeat` |
| Plan of a 144-node cone inside a 2.9-million-node graph | 1.01 ms (0.97..1.06, n=7): it allocates two arrays the size of the graph | `incremental` 3a |
| Kernel compilation | 11.5 ms for 13 shapes (n=1) | `repeat` |
| Opening the GPU (`Gpu::new`) | 212 to 246 ms (one sample in each of 11 processes) | all |

The GPU does about 55 times the CPU plan's work per second here (1.2 ns against 0.021 ns), and about 48 times on the insurance book (earlier: 44 ms a path on the CPU, 9.07 s for 10,000 paths on the GPU).

The brief notes that a consumer card's double-precision rate is far below its single-precision rate. I did not measure single precision, so I cannot say how much of the 0.021 ns is arithmetic and how much is memory traffic. Nothing below depends on it.

Shared-GPU caveat, measured by accident: other benchmark jobs ran on this GPU from about 14:02 to 14:36. Runs of the same test during that time gave 0.065 ns per node-scenario where the quiet runs give 0.021. All figures in this note are from runs after those jobs ended (a 1 Hz sampler saw no other compute process; utilisation before each run was 5% or less). The contended runs are not reported, but the threefold swing is itself a finding: a static GPU constant is wrong on a shared device. Breß et al. (SIGMOD 2016) report the same fragility.

### 4.2 The model

For a plan with `N` per-scenario nodes in `L` levels, `G` launch groups (`L ≤ G`), `I` inputs and `O` outputs per scenario, run for `S` scenarios:

- CPU plan: `wake·[S > 1] + N·S·18 ns ÷ min(S, 15)`
- GPU plan: `0.8 ms + N·3.7 ns + G·7 µs + N·S·0.021 ns + (I + O)·S·3.4 ns`
- GPU, loaded: `G·7 µs + N·S·0.021 ns`, plus `O·S·3.4 ns` if outputs come back in full
- Incremental (one scenario): `A·0.18 µs` for `A` affected nodes
- Once per structure: graph `≈ 0.7 to 1.1 µs·nodes`, plan `≈ 0.4 to 0.9 µs·N`. Once per shape set: compile, about 10 ms and upwards.
- A cut between two plans on different devices, or two plans on the same device through the host: `23 ns · width · S` plus the second plan's fixed costs.

Which terms are measurable ahead of time, estimable, or need profiling:

| Term | How to get it |
|---|---|
| `N`, `L`, `I`, `O`, slots | Exact, from the plan (`Scenarios::nodes`, `levels`, `inputs`, `outputs`, `bytes_per_scenario`) |
| `G` | Exact in the plan (`groups`) but not public. The prototype used `L`, which underestimates |
| `A` | Exact after the fact (`Report::recalculated`). Ahead of time, a reachability walk over `Graph::dependents` gives an upper bound (no cutoff) |
| Per-node CPU and GPU constants | Calibrate once per machine; they vary about 2× with node complexity, and the GPU constant about 3× with contention |
| Transfer and launch constants | Calibrate once per machine |
| Whether values are still valid on a device | Needs state the library does not keep today (section 5.3) |
| Cache effects, branch behaviour | Folded into the per-node constants. The plan interpreter dispatches on an opcode per instruction; the GPU evaluates both branches and selects, so irregular control flow costs the GPU nothing extra and the "branch-heavy favours the CPU" intuition does not apply to this subset |

Overhead of using the model: a dozen multiplications per candidate. It is negligible. The cost is in calibration and in being wrong.

### 4.3 How well it predicts

`hetero_probe planner` applies the formulas, with the constants hard-coded from the `launch`, `crossover 1000 48` and `transfer` runs, to 14 cases (five model shapes, two or three scenario counts each) and three candidates: all CPU, all GPU, narrow part on CPU with wide part on GPU. **Measured**, n=4 per timing:

- It picked the fastest candidate in 11 of 14 cases.
- The three misses cost 1.62×, 1.58× and 1.11× the fastest. All three are runs of under 5 ms where the GPU's real fixed costs are two to three times the model's (the download path and more than one group a level).
- Where measured times exceed 10 ms, most predictions were within 20%. Three GPU or hybrid figures were under-predicted by a factor of 1.3 to 1.5 (for example 111 ms predicted, 164 ms measured).

So a useful model is cheap to build. It does not need to be good: the candidates usually differ by a factor of 3 to 50, and they are close only where little time is at stake.

## 5. Partitioning and incremental recomputation

### 5.1 Can the graph be divided into regions?

| Way to divide | Supported today? | Worth it? |
|---|---|---|
| By what a set of outputs depends on (a downward-closed cone) | Yes, `Scenarios::new` | Yes: this is how independent sub-books and affected accounts were isolated in the prototype |
| By level (a cut across the whole plan at a level boundary) | No. A plan cannot start at interior nodes | No. The cut is as wide as everything live at that level. Section 6 E measures the cost |
| By classification (constant / shared-only / per-scenario) | Yes, built in | Yes, already done |
| By shape within a level (kernel groups) | Yes, built in (`groups`) | Already done |
| Several levels into one kernel (fusion) | In progress on the unpublished `fusion` branch (bands of levels, clusters sealed to stay acyclic) | Likely: it attacks memory traffic and launch count on the GPU itself |
| Independent regions in parallel on two devices | Possible with two cone plans | No. Section 6 F |

A general graph partitioner is not needed. The useful cuts are the ones the structure gives for free: classification, output cones, and zero-width cuts between independent cones joined by a reduction.

### 5.2 When hybrid can beat both single devices

Moving a region `R` from the GPU to the CPU saves its launches and its GPU compute, and costs its CPU compute plus any extra transfer:

`gain = G_R·7 µs + N_R·S·0.021 ns − N_R·S·1.2 ns − extra transfer`

With `w = N_R ÷ G_R` the region's average width, the gain is positive only if `w·S < about 5,900` and the cut adds no transfer. In words: a region is cheaper on the CPU only if each of its kernel launches would have run fewer than about six thousand threads. The measurements agree: a chain one node wide wins on the CPU at 100 and 1,000 scenarios and loses at 10,000 (section 6 C).

Hybrid therefore wins when all of these hold: a per-scenario region is narrow and deep; it accounts for a real share of the launches; the scenario count is in the low thousands or less; and the cut out of it is no wider than what would have been uploaded anyway.

Hybrid almost certainly loses when:

- the cut is wide (anything that slices a population at a point in time): 23 ns per value crossing against 0.021 ns per node on the GPU;
- the region is wide (more than a few thousand node-scenarios per launch);
- the scenario count is large: the launch saving is fixed and the CPU penalty grows with `S`;
- the aim is to use both devices at once on independent regions: the CPU adds about 2% of the GPU's throughput (section 6 F);
- regions alternate between devices: each alternation is another round trip (section 6 E).

The insurance book has a narrow per-scenario prefix only one node wide and one step a month. Running an equivalent shape as a hybrid gave 0.86× to 1.01× the time of the GPU alone (section 6 C, second table): nothing.

Two cheaper remedies remove the structural case altogether. CUDA Graphs submit many launches with one host call; NVIDIA's blog reports 9.6 µs per kernel with a synchronisation after each, 3.8 µs with overlapped launches and 3.4 µs in a graph (V100, CUDA 10.1; opened). Fusing levels into fewer kernels reduces the launch count itself. Either should be tried before a planner.

### 5.3 Incremental recomputation

This is where the graph knows something a generic scheduler does not.

**One scenario (the graph's own state).** Candidates when inputs change:

| Strategy | Cost | Status |
|---|---|---|
| Incremental on the CPU | `A·0.18 µs` | Exists (`Graph::stabilize`) |
| Whole plan on the GPU for one scenario | 13.2 ms (12.8..13.7, n=7) for 2.88 million nodes, of which 2.15 ms is compute | Measured, but the results cannot be written back to the graph: slots are reused, and nothing copies plan values into `Graph::values` |
| Whole graph on the CPU | 0.2 to 0.25 µs a node (earlier): about 0.6 s for this graph (estimated) | Exists (`recalculate_everything`) |

By these constants the GPU would overtake the incremental engine when more than about 12,000 nodes are affected (plan resident) to 75,000 (plan uploaded each time) (estimated). GPU-accelerated OpenTimer reports a break-even of about 67,000 for incremental timing (opened). So a size threshold on `A` is established practice, and the numbers here are of the same order. It would need the plan to keep every node's value and a path to copy them back. Not built.

**Many scenarios (a batch already run).** Today any change means running the whole plan again. If the changed input was baked in as a constant, the plan must also be made again, and nothing detects that it is stale. The alternative: recalculate, for every scenario, only the nodes that depend on what changed, reading unaffected values that were kept.

The prototype did this for a change to one account (section 6 A): 62 to 530 times faster than a whole re-run, with identical bits. The affected part was faster on the CPU than on the GPU.

What the real insurance book would allow (**measured** structure, `hetero_cones`, 20,000 kinds, 19,889,057 per-scenario nodes, 344,124 slots a scenario; "kept values" are distinct unaffected per-scenario values that affected nodes read, each a row of `S` doubles that must be on hand):

| What changes | Affected nodes | Share | Kept values | Kept ÷ slots | Graph update (n=1) |
|---|---|---|---|---|---|
| One policy's promise | 716 | 0.004% | 591 | 0.002 | 129 µs |
| Return, last month | 4,911 | 0.025% | 13,311 | 0.04 | 1 ms |
| Discount rate, month 239 | 3,316 | 0.017% | 16,015 | 0.05 | 0.8 ms |
| Discount rate, month 120 | 956,755 | 4.8% | 2,110,040 | 6.1 | 273 ms |
| Expenses | 1,919,438 | 9.7% | 11,359,474 | 33 | 1.8 s |
| Return, month 120 | 3,293,183 | 16.6% | 93,994 | 0.27 | 587 ms |
| Discount rate, month 0 | 5,698,314 | 28.7% | 12,291,305 | 36 | 2.7 s |
| Mortality | 11,390,910 | 57.3% | 6,621,568 | 19 | 4.7 s |
| Lapses | 11,370,910 | 57.2% | 6,641,568 | 19 | 4.6 s |
| Fee; return, first month | 19,889,057 | 100% | 240 | 0.001 | 5.0 s |

Reading it:

- Single-policy amendments and late-month changes touch thousandths of a percent and need almost nothing kept. A dirty-region re-run would cost milliseconds against 9.4 s (estimated from the prototype's ratio; not run on this book).
- The stresses the `liabilities` example actually runs (mortality, lapses, expenses, rates, fee) touch 10% to 100%. The most a dirty-region re-run could save is between nothing and 10×, and for expenses, mortality, lapses and discount rates it would need 19 to 36 times the working memory per scenario, which cuts scenarios per pass from 6,807 to a few hundred. For these, running everything again is the right plan.
- Liveness-based slot reuse and incremental reuse pull in opposite directions. The plan fits 6,807 scenarios in 24 GB because it forgets intermediates. Incremental batch reuse needs them remembered.

**Do incremental updates give more than a conventional heterogeneous scheduler?** Yes in kind: a scheduler places work, while the dirty set removes work. The choice of device for the dirty part is then a small follow-on decision that the section 4 model handles. For narrow changes it will usually say CPU.

### 5.4 Correctness

Holding CPU incremental state and GPU batch state at once is feasible, and three things can go silently wrong today:

1. **A plan is a snapshot of constants.** Nodes that depend on no varied or shared input are baked in when the plan is made (`Builder::fixed`). If the graph later changes one, every run of the old plan is wrong and nothing says so. The prototype's strategy 1 exists because of this.
2. **A `Loaded` does not know its plan.** `Loaded::work_out` takes the plan by reference on each call and checks only that slot count × scenarios matches. Another plan of the same size would be accepted.
3. **No structure version.** `Graph::epoch` counts updates. `define` and `redefine` do not change it until the next `stabilize`, and a redefinition that leaves the value unchanged is invisible to a `ChangeSink`.

What a correct design needs, in order of necessity:

- A validity stamp on each plan: the graph's identity, a structure counter, and the update number at which constants were read. Before a run, compare against the graph; refuse or rebuild if a baked-in node has changed since. The data exists (`changed_in` per node) but is private. A `ChangeSink` can approximate it from outside today.
- Dirty tracking per plan, not per device buffer: the set of plan inputs (varied, shared, baked) changed since the last run. From it, the affected plan nodes follow by a forward walk. This is cheaper and safer than tracking individual device rows.
- Explicit staleness of retained values: any retained row downstream of a changed input is invalid until recomputed. The rule from HERO applies: delete or mark stale copies on update; never read one optimistically.
- Snapshot semantics: a plan is immutable, so concurrent scenario runs already see one consistent state. Keep that. An incremental batch re-run should produce a new consistent set of retained values or none (apply all-or-nothing).
- Determinism: results must not depend on which strategy ran. The prototype's patched totals equal the full re-run to the last bit because the retained partial sums are re-added in the plan's own order. A "subtract old, add new" delta would be faster to write and would not be bit-identical. With `exp`, `ln` or `pow` in a region, moving it between CPU and GPU changes last digits (crate docs), so a planner that picks the device by size makes results depend on size. Either pin such regions to one device or state the tolerance.

## 6. Prototype findings

`crates/watershed-gpu/examples/hetero_probe.rs` (experimental, public API only). The model: `months` varied inputs; a chain one node wide stepped `sub` times a month (three nodes and three levels a step); `kinds` accounts of three nodes a month reading the chain; a tree of eight-way sums. Every strategy was checked against the others to the last bit before any time was printed. Times are end to end for the call, median (min..max, n), after one discarded warm-up. No other GPU compute process was seen during these runs.

### B. Large parallel calculation: when is the GPU worthwhile?

| Plan nodes | CPU faster up to | GPU faster from | At the largest S tried |
|---|---|---|---|
| 324 | S = 4,096: 2.29 ms (1.56..3.38, n=7) vs 4.56 ms (2.40..4.98, n=7) | S = 16,384: 6.48 ms (6.40..7.66) vs 3.09 ms (2.80..4.18) | 2.1× for the GPU |
| 144,287 | S = 4: 3.01 ms (2.85..3.17) vs 4.34 ms (4.18..5.03) | S = 16: 5.39 ms (5.16..5.62) vs 4.93 ms (4.71..5.28) | S = 16,384: 2.790 s (2.788..2.795) vs 55.2 ms (55.0..56.5): 51× |
| 2,883,002 | never | S = 1: 51.96 ms (51.36..52.27) vs 13.18 ms (12.76..13.74) | S = 4,096: 14.24 s (14.16..14.26, n=3) vs 264.8 ms (262.6..267.8): 54× |

The crossover is at roughly 2 to 5 million node-scenarios. Below it both take single-digit milliseconds, so little is at stake. A one-line rule on `N × S` captures it.

### D. Repeated execution: what does keeping things save?

2,883,002 nodes, 2,000 scenarios.

| Step | Time |
|---|---|
| Build the graph | 2.0 s (n=1) |
| Make the plan | 1.113 s (1.097..1.113, n=2) |
| First GPU run, including 11.5 ms of compilation | 180.5 ms (n=1) |
| GPU run again, plan and kernels kept, buffers re-made | 137.1 ms (135.6..139.4, n=7): upload 12.9, compute 121.8, download 2.1 |
| `Loaded::run`, everything kept, outputs copied back | 123.4 ms (123.0..126.1, n=7) |
| `Loaded::totals`, reduced on the device | 122.1 ms (121.8..123.4, n=7) |
| Replace every input on the device | 2.1 ms (2.0..2.2, n=7) |
| CPU plan | 6.975 s (6.963..6.992, n=3) |

Keeping the plan is worth 8 runs; keeping the graph and plan, 23 runs. Keeping device buffers saves about 10% at this size. It saves nearly everything for small graphs over many scenarios: the earlier investigation measured 627 ms end to end for 19 ms of compute on the tax example.

### A. Small incremental change after a batch run

2,943,052-node graph, 2,000 scenarios. One account's floor changes (48 graph nodes recalculate). Wanted: all 2,000 new totals.

| Strategy | Time |
|---|---|
| The graph's own update, one scenario | 12.2 µs (11.8..14.0, n=7) |
| 1. Make the whole plan again (the floor is baked in) | 1.183 s (1.155..1.202, n=7) |
|    then run it all on the GPU | 156.7 ms (156.0..157.6, n=7) |
|    or run it all on the CPU | 6.920 s (6.866..7.061, n=7) |
| 2. Every floor a shared input, plan loaded: `set_shared` and run it all | 319.5 ms (304.0..359.9, n=7) |
| 3a. Make a plan of the changed account alone (144 nodes) | 1.01 ms (0.97..1.06, n=7) |
| 3b. Run it for 2,000 scenarios on the CPU | 1.47 ms (1.42..2.62, n=7) |
| 3c. or on the GPU, inputs copied up | 6.42 ms (5.54..7.78, n=7) |
| 3d. or on the GPU, inputs already there | 3.67 ms (3.13..5.97, n=7) |
| 3e. Re-add the five sums above it from kept values, on the CPU | 57 µs (52..105, n=7) |

Strategy 3 with the CPU totals 2.5 ms: 62 times faster than the fastest whole re-run that needs no new plan (156.7 ms), about 125 times faster than strategy 2, and about 530 times faster than strategy 1 in full (1.34 s). All three give the same 2,000 totals to the last bit.

- The gain comes from the dependency information. The device choice inside strategy 3 is worth a factor of 2.5 to 4, in the CPU's favour: 144 nodes × 2,000 scenarios is 288,000 node-scenarios, below the crossover in B, and the GPU spends its time on about 145 launches.
- Strategy 3 needed values kept from the first run: here 157 rows of 2,000 doubles (2.5 MB), chosen for five accounts picked in advance. Keeping enough for *any* account means keeping every account's final value and the whole sum tree.
- Strategy 2 is twice as slow as a plain re-run (319.5 against 156.7 ms). Making 20,000 inputs shared turns constants into loads from a 20,000-entry table and re-uploads it. Declaring things shared "in case they change" is not free.

### C. Mixed: a deep narrow part feeding a wide part

Constructed best case: the chain takes 21 steps a month (15,119 levels, one node wide); 1,000 accounts over 240 months (720,144 nodes, 724 levels). The cut is 240 values a scenario, the same size as the inputs.

| S | All CPU | All GPU | Chain on CPU, accounts on GPU | Hybrid ÷ best single |
|---|---|---|---|---|
| 100 | 106.8 ms (97.9..108.9, n=7) | 113.4 ms (109.8..132.5, n=7) | 16.3 ms (15.5..17.5, n=7) | 0.15 |
| 1,000 | 889.3 ms (880.2..913.7, n=7) | 128.3 ms (118.2..224.8, n=7) | 44.2 ms (43.2..45.7, n=7) | 0.34 |
| 10,000 | 8.826 s (8.756..8.941, n=3) | 274.5 ms (272.7..278.0, n=7) | 331.2 ms (330.3..334.8, n=7) | 1.21 |

Hybrid beats both by 6.6× and 2.9× at 100 and 1,000 scenarios and loses at 10,000, as the rule in 5.2 says. The whole of the gain is avoided launches: about 15,000 × 7 µs.

Insurance-like shape: one chain step a month, 5,000 accounts (3.6 million nodes, 727 levels).

| S | All GPU | Chain on CPU, accounts on GPU | Hybrid ÷ GPU |
|---|---|---|---|
| 100 | 32.6 ms (30.2..52.5, n=7) | 28.0 ms (26.2..33.2, n=7) | 0.86 |
| 1,000 | 97.9 ms (96.7..99.5, n=7) | 96.7 ms (95.8..97.4, n=7) | 0.99 |
| 10,000 | 762.1 ms (760.6..763.4, n=7) | 773.5 ms (771.5..774.5, n=7) | 1.01 |

No material gain. The CPU ran all of it in 479 ms, 4.36 s and 43.3 s (n=7, 3, 1).

### E. Transfer-heavy: cuts as wide as the book

2,000 accounts over 48 months (288,287 nodes), 2,000 scenarios, cut by time into pieces. Each cut carries 2,000 values a scenario (32 MB each way).

| Strategy | Time | ÷ whole on GPU |
|---|---|---|
| Whole on GPU | 19.2 ms (17.5..21.0, n=7) | 1 |
| Whole on CPU | 682.2 ms (680.3..683.9, n=7) | 36 |
| 2 pieces, both on GPU, through the host | 113.2 ms (107.7..121.7, n=7) | 5.9 |
| 2 pieces, GPU then CPU | 425.7 ms (417.7..503.4, n=7) | 22 |
| 4 pieces, all on GPU | 327.6 ms (315.5..336.8, n=7) | 17 |
| 4 pieces, alternating | 639.0 ms (625.1..846.6, n=7) | 33 |
| 8 pieces, all on GPU | 668.9 ms (608.7..901.2, n=7) | 35 |
| 8 pieces, alternating | 885.4 ms (879.2..969.9, n=7) | 46 |

A cut costs about 94 ms here, five times the whole computation. The planner of 4.2 rejects every one of these plans: `23 ns × 2,000 × 2,000 = 92 ms` per cut against 19 ms for the whole. Part of the 23 ns is the current code: `Gpu::take` issues one device-to-host copy per output row (2,000 of them), `give` and `take` transpose on the host, and every piece allocates and uploads its own buffers. A cut that stayed on the device would cost none of this, which is an argument for one plan on one device, not for a smarter cut.

### F. Both devices at once

2,361,780 nodes, 2,000 scenarios, two independent parts of the book.

| CPU's share of the nodes | All on GPU | One part on each, at once | Ratio |
|---|---|---|---|
| 25% | 114.5 ms (112.5..115.1, n=7) | 1.470 s (1.467..1.527, n=7) | 12.8 |
| 3.1% | 114.9 ms (113.0..154.3, n=7) | 274.6 ms (271.6..277.7, n=7) | 2.39 |
| 0.4% | 119.7 ms (118.5..134.4, n=7) | 132.2 ms (131.6..132.8, n=7) | 1.10 |

No share helped. Two reasons. The bound: the CPU does the whole book in 5.64 s, so in the GPU's 115 ms it can do 2% of it; the most concurrency could save is about 2 ms. And contention: the GPU path's host-side work (`give`, `take`) uses the same global thread pool the CPU plan is saturating, so the GPU part waits (its own time rose from about 120 ms to 132 ms even at a 0.4% share).

### What the prototype could not do, and why

- Start a plan at interior nodes. The "downstream of the cut" pieces are separate graphs with inputs standing for the cut values.
- Pass values between two plans on the device. Every cut goes through the host.
- Keep intermediate values. Retained values had to be declared as extra outputs before the first run.
- Count launches exactly (`groups` is private).
- Test an operation the GPU path refuses (a `choose`, a total, integers). The plan rejects the whole cone, so there is no single-device baseline to compare a hybrid with. For such models hybrid is not an optimisation; it would be the only way to use the GPU at all. That case is about coverage, and extending what plans accept is the direct fix.
- Represent real models. The synthetic model is regular. Only the cone analysis (5.3) uses the real insurance graph.

## 7. Implications for clustering

Nothing was implemented. Three architectures, not two, are worth separating.

**Scenario distribution** (same plan everywhere, scenarios split). The design already suits it:

- A plan is immutable plain data (`Vec<u32>`, `Vec<f64>`), with nothing device-specific in it. Shipping it needs a byte format, not a framework. Size is about 18 bytes a node (estimated from the operand and node tables): about 360 MB for the insurance book, once per plan.
- Workers compile kernels from the plan's shapes and cache them by shape hash (`Gpu::shapes` does this per process).
- Per job, a worker receives `I × S` doubles and returns `O × S`, or block sums. For the insurance book: 19 MB of inputs for 10,000 paths.
- Reductions can stay bit-identical if workers take scenario ranges that are multiples of 1,024 (`BLOCK`) and return per-block sums that the coordinator adds in scenario order, which is what `total` does locally.
- Scenarios are pure, so a failed chunk is retried with no coordination.
- Heterogeneous workers: `Gpu::fit` already sizes a pass by free memory. A CPU-only worker runs `run_cpu` at about 1/50 the rate. Mixing CPU and GPU workers, or GPU models, breaks bit-identity for models using `exp`, `ln` or `pow`.
- The cost model extends by adding a network term per target (bytes ÷ bandwidth + latency), as Dask does.

**Population distribution** (independent sub-books on different machines, joined by a small reduction). This is graph partitioning along zero-width cuts. Cone extraction supports it today: test F built separate plans for separate sub-trees of the total. Each worker needs only its sub-plan. The join is a handful of numbers per scenario. This is the only form of "different regions on different machines" that the measurements support.

**Distributed graph execution in general** (arbitrary regions on different machines). Test E measured a cut through the population at 23 ns per value per scenario on one machine over PCIe, five times the whole computation for one cut. A network is slower. Do not build this.

The planner sketched below does not need to know about machines. A remote target would be another row of costs.

## 8. Recommended architecture

Do not introduce a planning layer, a scheduler, or backend traits. Three small additions, in order. Sketches are Rust in the style of `watershed-gpu`; none exists.

**1. An estimate, not a planner.** A plain function on the plan and a struct of calibrated constants. The caller, or a three-line helper, picks the smaller number.

```rust
/// Costs measured once on this machine. Plain numbers, so they can be
/// saved, compared and overridden.
pub struct Costs {
    pub cpu_node: Duration,      // one node, one scenario, one thread
    pub cpu_threads: f64,        // effective speed-up on all cores
    pub gpu_fixed: Duration,     // per run
    pub gpu_launch: Duration,    // per (level, shape) group
    pub gpu_node: Duration,      // one node, one scenario
    pub move_value: Duration,    // one f64 between host and device
}

pub struct Estimate {
    pub cpu: Duration,
    pub gpu: Duration,           // buffers made and filled for this run
    pub gpu_loaded: Duration,    // plan and inputs already on the device
}

impl Scenarios {
    pub fn estimate(&self, scenarios: usize, costs: &Costs) -> Estimate;
}
```

**2. A validity stamp.** Correctness before speed. It needs a structure counter and a "changed since update n" query from `watershed-core`.

```rust
impl Scenarios {
    /// Whether the graph still has the structure, and the baked-in values,
    /// this plan was made from.
    pub fn is_current(&self, g: &Graph) -> bool;
}
```

`Loaded` should own or fingerprint its plan so that it cannot be run with another.

**3. Re-running only what changed, for narrow changes.**

```rust
/// What a change to some of a plan's inputs reaches.
pub struct Affected {
    pub nodes: usize,
    pub levels: usize,
    /// Unaffected per-scenario values the affected nodes read: rows that
    /// must have been kept.
    pub kept: usize,
}

impl Scenarios {
    pub fn affected_by(&self, changed: &[NodeId]) -> Affected;
}
```

The executor for it is the open design question. The smallest version: when a plan is made, the caller names the reduction nodes to retain (the sum tree); a re-run takes the changed inputs, makes the cone plan of the affected leaves, runs it on whichever device `estimate` prefers, and re-adds the retained sums in plan order. That is what the prototype did by hand. It requires plans that can start at interior nodes, or the workaround of retained outputs.

What I would not add: a `Backend` trait (there are three executors and one caller); a region or partition type; a task queue; per-node device tags.

Of the brief's five boxes, the graph, the incremental engine and the backends exist and should stay as they are. The cost model is the `Costs` struct. The "planner" is a comparison of two or three numbers and should remain one until evidence says otherwise.

## 9. Experimental roadmap

Smallest first. Each has a result that would stop the line of work.

1. **Selection rule on the real examples** (hours). Compute `estimate` for every existing example and compare with the measured CPU and GPU times already on file. Stop if the simple `N × S` threshold gets the same answers; then ship the threshold and skip the struct.
2. **Staleness guard** (days). Detect a plan whose baked-in constants have changed. No performance question; it removes a silent-wrong-answer path that any incremental batch feature would widen.
3. **Cheap fixes that move the constants** (days, each measured alone): one download for all output rows; reuse device buffers across `run_within` calls; stop re-uploading `shared` when it has not changed; avoid the thread pool for small transpositions; pinned host memory. These change where the CPU/GPU crossover sits and should precede calibration.
4. **Launch overhead** (days). CUDA Graph capture of `work_out`, or level banding as on the `fusion` branch, on the deep-chain case of test C. Expectation: the hybrid's advantage there disappears. If it does not, the structural case for hybrid is stronger than I have concluded.
5. **Dirty-region re-run on the real book** (one to two weeks). One policy amended, 10,000 paths, 20,000 kinds, bit-identical totals. Estimated 10 to 30 ms against 9.4 s. Measure the memory to retain: every policy's four measures for 10,000 paths is 6.4 GB (estimated); retaining only the first level of sums and recomputing eight policies instead of one is 0.8 GB (estimated). Stop if retention costs more scenarios per pass than users will give up, or if no user has the workload (amend, then re-run thousands of paths).
6. **One-scenario wide updates on the GPU** (only if 5 succeeds). When a change reaches more than about 50,000 nodes, run the plan for one scenario and copy values back. Needs every node's value kept.

Not on the roadmap: a general partitioner; concurrent CPU and GPU execution of one batch; distributed graph execution.

Benchmarks A to F in section 6 are the designs asked for in the brief, run on a synthetic model. What they still lack: the same tests on the insurance, loans and pensions graphs (needs item 5's plan support); cold-start figures in a fresh process for each strategy (only `Gpu::new`, compile and first run were separated); power; a second GPU model; native Linux in place of WSL2, where launch overhead may differ.

## 10. Risks and limitations

Of the idea:

- **Slower.** Any cut wider than the inputs; any alternation; any attempt to share one batch between devices. Measured at 5.9× to 46× slower (E) and 1.1× to 12.8× slower (F).
- **Less predictable.** A device chosen by size makes latency step at the threshold, and with `exp`/`ln`/`pow` makes the last digits depend on the device. On a shared GPU the constant moved threefold during this work.
- **Complexity for nothing.** For the insurance-shaped model hybrid gave 0.86× to 1.01×. A planner there adds states to test and no speed.
- **Memory.** Retaining values for incremental batch re-runs competes with scenarios per pass: up to 36× the working memory for realistic stresses on the real book.
- **Silent staleness.** More retained state on more devices means more ways to read an old value. The library has no guard today.
- **Fixing the wrong layer.** The structural hybrid win is launch overhead, and the transfer penalty is partly the current copy path. Both are better fixed in `cuda.rs` than planned around.

Of this note:

- The timing model is synthetic and regular. Only the cone analysis uses the real book.
- One machine, one GPU, WSL2. Launch and transfer constants may differ on native Linux.
- n=7 for most timings, n=3 or 1 where a run takes seconds, n=4 in the planner test. Cone-analysis update times are single samples.
- The planner's constants were calibrated on the same model family it was tested on. Its 11 of 14 is an upper bound on what to expect elsewhere.
- Single precision was not measured.
- The estimates for the real book in 5.3 and roadmap item 5 are arithmetic, not runs.
- The prior-art search was bounded. Several entries rest on search snippets (marked), and "not found" is not "does not exist".
- Other benchmark jobs shared the machine for the first half hour. Their runs and my discarded early runs overlapped between about 14:02 and 14:12, so their figures for that period may also be affected.

## 11. Recommendation

**Option 3, narrowed: focus on data residency and incremental batch updates, for narrow changes only. Do the trivial part of option 2 first. Do not pursue option 1.**

Evidence against a heterogeneous planner (option 1):

- The GPU is about 50 times faster than the CPU plan per node-scenario (measured). Sharing a batch between them can gain at most about 2% and in practice lost 10% or more (F).
- Wide cuts cost 23 ns per value against 0.021 ns per node; one cut cost five times the whole run (E).
- The one structural win (deep narrow regions, up to 6.6× in a constructed case) is launch overhead. It vanished on the insurance-like shape (0.86× to 1.01×) and has cheaper remedies.
- The closest prior systems that tried per-operator cost-based placement report that it is fragile and that region-level, data-location-driven placement is what works. Their tasks are a thousand times coarser than a Watershed node.

Evidence for improving selection a little (option 2):

- A dozen-line cost formula picked the fastest of three strategies in 11 of 14 cases, and its misses were confined to runs of a few milliseconds. The CPU/GPU crossover sits near 2 to 5 million node-scenarios. This is worth an afternoon and no architecture.

Evidence for incremental batch updates (option 3):

- Re-running only the affected part for all scenarios was 62 to 530 times faster than re-running the batch, bit-identical (A). Nothing in the prior art found does this from a dependency tracker.
- On the real book, single-policy and late-period changes affect under 0.03% of nodes and need almost nothing retained.

Evidence that keeps option 3 narrow:

- The stresses the examples actually run touch 10% to 100% of the book and would need up to 36 times the memory. Full re-runs remain right for them.
- Whether anyone amends one policy and then wants ten thousand paths again is a product question this note cannot answer. Roadmap item 5 should not start without a user who does.

If that user does not exist, the answer becomes option 4 for everything beyond the selection rule and the staleness guard.

## 12. Sources

### Code and data

- `crates/watershed-core/src/graph.rs` (`Graph`, `stabilize`, `recalculate`, `mark_dependents`, `recalculate_in_parallel`, `repack`), `sink.rs` (`ChangeSink`)
- `crates/watershed-gpu/src/plan.rs` (`Scenarios::with_shared`, `set_shared`, `run_cpu`, `eval`, `total`), `cuda.rs` (`Gpu::run_within`, `load`, `device`, `give`, `take`, `work_out`, `shapes`, `source_for`, `Loaded`), `kernel.cu`, `lib.rs`
- `crates/watershed-gpu/tests/scenarios.rs`
- `crates/watershed-gpu/examples/shared/savings.rs`, `liabilities.rs`
- Experimental, added for this note: `crates/watershed-gpu/examples/hetero_probe.rs`, `hetero_cones.rs`; raw output in `docs/research/heterogeneous-execution-data/`
- Earlier same-day measurements and survey: `measurements.md` and `watershed-investigation.md` (outside the repository, in the owner's research folder)
- Read but not relied on: `crates/watershed-gpu/src/fuse.rs` on the unpublished `fusion` branch

### External, opened

- StarPU handbook, scheduling and performance models: https://files.inria.fr/starpu/doc/html/Scheduling.html , https://files.inria.fr/starpu/doc/html/OfflinePerformanceTools.html
- Augonnet, Thibault, Namyst, "Data-Aware Task Scheduling on Multi-Accelerator based Platforms", ICPADS 2010: https://www.par.univie.ac.at/project/peppher/publications/Published/icpads10.pdf
- Legion mapper documentation: https://legion.stanford.edu/mapper , https://legion.stanford.edu/tutorial/custom_mappers.html
- Kokkos DualView: https://kokkos.org/kokkos-core-wiki/API/containers/DualView.html
- NVIDIA, "Getting Started with CUDA Graphs": https://developer.nvidia.com/blog/cuda-graphs/ ; "Constructing CUDA Graphs with Dynamic Parameters": https://developer.nvidia.com/blog/constructing-cuda-graphs-with-dynamic-parameters
- Dask distributed, scheduling policies and resources: https://distributed.dask.org/en/stable/scheduling-policies.html , https://distributed.dask.org/en/stable/resources.html
- Abadi et al., TensorFlow whitepaper: https://arxiv.org/pdf/1603.04467 ; `placer.h`: https://raw.githubusercontent.com/tensorflow/tensorflow/master/tensorflow/core/common_runtime/placer.h ; GPU guide: https://www.tensorflow.org/guide/gpu
- MLIR gpu dialect (partly): https://mlir.llvm.org/docs/Dialects/GPU/
- ONNX Runtime execution providers and design: https://onnxruntime.ai/docs/execution-providers/ , https://onnxruntime.ai/docs/reference/high-level-design.html
- Breß, Funke, Teubner, "Robust Query Processing in Co-Processor-accelerated Databases", SIGMOD 2016: https://readingxtra.github.io/docs/db_GPU/BrebSIGMOD2016.pdf
- Karnagel, Habich, Lehner, "Adaptive Work Placement for Query Processing on Heterogeneous Computing Resources", PVLDB 10(7), 2017: https://www.vldb.org/pvldb/vol10/p733-karnagel.pdf
- Guo, Huang, Lin et al., GPU-accelerated static timing analysis, TCAD 2023: https://guozz.cn/publication/gputimertcad-23/gputimertcad-23.pdf
- Lin et al., "From RTL to CUDA" (RTLflow), ICPP 2022: https://tsung-wei-huang.github.io/papers/icpp22-rtlflow.pdf
- Slaughter et al., Task Bench (abstract): https://arxiv.org/abs/1908.05790

### External, search snippets only (not opened)

- PaRSEC GPU support: https://hpcg.icl.utk.edu/files/publications/2020/icl-utk-1453-2020.pdf
- SYCL 2020 specification: https://registry.khronos.org/SYCL/specs/sycl-2020/html/sycl-2020.html
- OpenMP 5.0 target constructs: https://www.openmp.org/spec-html/5.0/openmpsu57.html
- JAX FAQ on device placement: https://docs.jax.dev/en/latest/faq.html (page opened; the placement section was not in what was returned)
- CUDA Graph API reference: https://docs.nvidia.com/cuda/archive/11.0/cuda-runtime-api/group__CUDART__GRAPH.html
- Dask-CUDA spilling: https://docs.rapids.ai/api/dask-cuda/legacy/spilling/
- TVM "Bring Your Own Codegen": https://tvm.apache.org/docs/dev/how_to/relay_bring_your_own_codegen.html
- Topcuoglu, Hariri, Wu, HEFT and CPOP, IEEE TPDS 2002; Arabnejad and Barbosa, PEFT, TPDS 2014 (DOI 10.1109/TPDS.2013.57)
- Luk, Hong, Kim, Qilin, MICRO 2009: https://www.doi.org/10.1145/1669112.1669121
- HyPE / CoGaDB: https://wwwiti.cs.uni-magdeburg.de/iti_db/research/gpu/cogadb/ ; HOP: https://link.springer.com/doi/10.1007/s13222-014-0167-9
- Funke et al., SIGMOD 2018: https://dbis.cs.tu-dortmund.de/publikationen/2018/pipelined-query-processing-in-coprocessor-environments/
- Lutz et al., SIGMOD 2020: https://users.cs.utah.edu/~pandey/courses/cs6530/fall24/papers/GPU/3318464.3389705.pdf
- GEM (DAC 2025): https://research.nvidia.com/publication/2025-06_gem-gpu-accelerated-emulator-inspired-rtl-simulation
- GPU Datalog: https://arxiv.org/pdf/2311.02206

### Could not open

- StarPU journal paper (bot check): https://inria.hal.science/inria-00550877/document
- Legion mapping page (404): https://legion.stanford.edu/mapping/
- CUDA programming guide, graphs chapter (404): https://docs.nvidia.com/cuda/cuda-c-programming-guide/04-special-topics/cuda-graphs.html

### Searches that found nothing for "incremental computation with GPU or device residency"

Self-adjusting computation, change propagation, incremental view maintenance, differential dataflow, incremental Datalog, dynamic-graph processing, and RTL activity skipping, each combined with "GPU". Dynamic-graph GPU systems recompute affected vertices wholly on the GPU; activity-skipping RTL simulators are CPU-only.
