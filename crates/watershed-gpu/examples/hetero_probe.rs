//! EXPERIMENTAL. Not part of the library; written for the research note
//! `docs/research/heterogeneous-execution.md`. It measures what choosing and
//! combining devices is worth, using only the crate's public API.
//!
//! The model is synthetic and shaped like the insurance examples:
//!
//!   `months` varied inputs (a market return a month)
//!   -> a *chain*: one short-rate value stepped `sub` times a month, three
//!      nodes and three levels a step, one node wide
//!   -> a *body*: `kinds` accounts, each three nodes a month, reading the
//!      chain's value for the month and the account's own fee and floor
//!   -> a tree of eight-way sums to one total
//!
//!     cargo run --release --features cuda --example hetero_probe -- <test> [sizes]
//!
//! Tests: launch, crossover, repeat, incremental, mixed, transfer,
//! concurrent, planner. Every strategy's answers are checked against the others to
//! the last bit before a time is printed.

#[cfg(not(feature = "gpu"))]
fn main() {
    println!("this needs --features cuda");
}

#[cfg(feature = "gpu")]
fn main() {
    probe::main()
}

#[cfg(feature = "gpu")]
mod probe {

    use std::collections::HashMap;
    use std::ops::Range;
    use std::time::{Duration, Instant};

    use watershed_core::NodeId;
    use watershed_core::prelude::*;
    use watershed_gpu::{Gpu, Scenarios};

    type E = Expr<f64>;

    fn e(x: impl IntoExpr<f64>) -> E {
        x.into_expr()
    }

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn unit(&mut self) -> f64 {
            (self.next() % 1_000_000) as f64 / 1_000_000.0
        }
    }

    /// The chain: gives the rate at the end of each month.
    fn chain(g: &mut Graph, returns: &[Input<f64>], sub: usize) -> Vec<Node<f64>> {
        let start = g.input("x_start", 0.03).unwrap();
        let mut x = e(start);
        let mut monthly = Vec::new();
        for (t, &r) in returns.iter().enumerate() {
            let mut last = None;
            for d in 0..sub {
                let pull = g
                    .define(&format!("pull{t}_{d}"), (0.03 - x.clone()) * 0.1)
                    .unwrap();
                let step = g
                    .define(
                        &format!("step{t}_{d}"),
                        pull + r * (0.001 + d as f64 * 1e-5),
                    )
                    .unwrap();
                let next = g
                    .define(
                        &format!("x{t}_{d}"),
                        when((x.clone() + step).lt(0.0))
                            .then(0.0)
                            .otherwise(x.clone() + step),
                    )
                    .unwrap();
                x = e(next);
                last = Some(next);
            }
            monthly.push(last.expect("at least one step a month"));
        }
        monthly
    }

    struct Body {
        floors: Vec<Input<f64>>,
        /// Each account's value at the end.
        ends: Vec<Node<f64>>,
    }

    /// The accounts over some months. `rates` are the chain's values for those
    /// months. `starts`, if given, are the accounts' values when they begin
    /// (for a graph that takes up where another left off).
    fn body(
        g: &mut Graph,
        kinds: usize,
        months: Range<usize>,
        rates: &[E],
        starts: Option<&[Input<f64>]>,
    ) -> Body {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut floors = Vec::with_capacity(kinds);
        let mut ends = Vec::with_capacity(kinds);
        for k in 0..kinds {
            // The same numbers for an account whichever months are built.
            let (fee, floor, v0) = (
                0.0005 + rng.unit() * 0.001,
                80.0 + rng.unit() * 40.0,
                100.0 + rng.unit() * 50.0,
            );
            let fee = g.input(&format!("fee{k}"), fee).unwrap();
            let floor = g.input(&format!("floor{k}"), floor).unwrap();
            let mut v = match starts {
                Some(starts) => e(starts[k]),
                None => e(g.input(&format!("open{k}"), v0).unwrap()),
            };
            let mut end = None;
            for (t, rate) in months.clone().zip(rates) {
                let grow = g
                    .define(&format!("grow{k}_{t}"), v * (1.0 + rate.clone()))
                    .unwrap();
                let net = g.define(&format!("net{k}_{t}"), grow - grow * fee).unwrap();
                let next = g
                    .define(
                        &format!("v{k}_{t}"),
                        when(net.lt(floor))
                            .then(floor * 0.5 + net * 0.5)
                            .otherwise(net),
                    )
                    .unwrap();
                v = e(next);
                end = Some(next);
            }
            floors.push(floor);
            ends.push(end.expect("at least one month"));
        }
        Body { floors, ends }
    }

    /// Eight-way sums, level by level, to one node. `tree[0]` is the leaves.
    fn tree(g: &mut Graph, leaves: &[Node<f64>]) -> Vec<Vec<Node<f64>>> {
        let mut levels = vec![leaves.to_vec()];
        let mut depth = 0;
        while levels.last().unwrap().len() > 1 {
            depth += 1;
            let next: Vec<Node<f64>> = levels
                .last()
                .unwrap()
                .chunks(8)
                .enumerate()
                .map(|(n, members)| {
                    let sum = members[1..]
                        .iter()
                        .fold(e(members[0]), |sum, &member| sum + member);
                    g.define(&format!("sum{depth}_{n}"), sum).unwrap()
                })
                .collect();
            levels.push(next);
        }
        levels
    }

    struct Model {
        g: Graph,
        returns: Vec<Input<f64>>,
        monthly: Vec<Node<f64>>,
        body: Body,
        tree: Vec<Vec<Node<f64>>>,
        total: Node<f64>,
    }

    fn model(kinds: usize, months: usize, sub: usize) -> Model {
        let mut g = Graph::new();
        let returns: Vec<Input<f64>> = (0..months)
            .map(|t| g.input(&format!("r{t}"), 0.0).unwrap())
            .collect();
        let monthly = chain(&mut g, &returns, sub);
        let rates: Vec<E> = monthly.iter().map(|&x| e(x)).collect();
        let body = body(&mut g, kinds, 0..months, &rates, None);
        let tree = tree(&mut g, &body.ends);
        let total = tree.last().unwrap()[0];
        g.stabilize();
        Model {
            g,
            returns,
            monthly,
            body,
            tree,
            total,
        }
    }

    /// A graph of the accounts alone for some months: its varied inputs are the
    /// rates for those months and then, unless it starts at month 0, every
    /// account's value at the start. Its outputs are every account's value at the
    /// end or, for the last piece, the total.
    struct Piece {
        plan: Scenarios,
        months: Range<usize>,
        takes_values: bool,
    }

    fn piece(kinds: usize, months: Range<usize>, of: usize) -> Piece {
        let mut g = Graph::new();
        let rates: Vec<Input<f64>> = months
            .clone()
            .map(|t| g.input(&format!("rate{t}"), 0.03).unwrap())
            .collect();
        let takes_values = months.start > 0;
        let starts: Option<Vec<Input<f64>>> = takes_values.then(|| {
            (0..kinds)
                .map(|k| g.input(&format!("start{k}"), 100.0).unwrap())
                .collect()
        });
        let exprs: Vec<E> = rates.iter().map(|&x| e(x)).collect();
        let body = body(&mut g, kinds, months.clone(), &exprs, starts.as_deref());
        let outputs: Vec<NodeId> = if months.end == of {
            let tree = tree(&mut g, &body.ends);
            vec![tree.last().unwrap()[0].id()]
        } else {
            body.ends.iter().map(|n| n.id()).collect()
        };
        g.stabilize();
        let mut vary: Vec<NodeId> = rates.iter().map(|i| i.id()).collect();
        if let Some(starts) = &starts {
            vary.extend(starts.iter().map(|i| i.id()));
        }
        Piece {
            plan: Scenarios::new(&g, &vary, &outputs).unwrap(),
            months,
            takes_values,
        }
    }

    fn ids<T>(nodes: &[Node<T>]) -> Vec<NodeId> {
        nodes.iter().map(|n| n.id()).collect()
    }

    fn inputs_of(inputs: &[Input<f64>]) -> Vec<NodeId> {
        inputs.iter().map(|n| n.id()).collect()
    }

    fn returns_for(scenarios: usize, months: usize) -> Vec<f64> {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        (0..scenarios * months)
            .map(|_| rng.unit() * 0.08 - 0.03)
            .collect()
    }

    #[derive(Clone, Copy)]
    struct Stat {
        median: f64,
        min: f64,
        max: f64,
        n: usize,
    }

    impl std::fmt::Display for Stat {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "{} ({}..{}, n={})",
                time(self.median),
                time(self.min),
                time(self.max),
                self.n
            )
        }
    }

    fn stat(mut samples: Vec<f64>) -> Stat {
        samples.sort_by(f64::total_cmp);
        Stat {
            median: samples[samples.len() / 2],
            min: samples[0],
            max: samples[samples.len() - 1],
            n: samples.len(),
        }
    }

    fn time(s: f64) -> String {
        if s < 1e-3 {
            format!("{:.1} us", s * 1e6)
        } else if s < 1.0 {
            format!("{:.2} ms", s * 1e3)
        } else {
            format!("{s:.3} s")
        }
    }

    /// Runs `f` once to warm up and then `runs` times, fewer when it is slow
    /// (the warm-up then counts). Gives the times and the last result.
    fn bench<T>(runs: usize, mut f: impl FnMut() -> T) -> (Stat, T) {
        let started = Instant::now();
        let mut last = f();
        let first = started.elapsed().as_secs_f64();
        let (runs, mut samples) = match first {
            t if t > 20.0 => (0, vec![first]),
            t if t > 3.0 => (2, vec![first]),
            _ => (runs, Vec::new()),
        };
        for _ in 0..runs {
            let started = Instant::now();
            last = f();
            samples.push(started.elapsed().as_secs_f64());
        }
        (stat(samples), last)
    }

    fn same(what: &str, got: &[f64], want: &[f64]) {
        assert_eq!(got.len(), want.len(), "{what}: lengths differ");
        for (n, (a, b)) in got.iter().zip(want).enumerate() {
            assert!(
                a.to_bits() == b.to_bits(),
                "{what}: value {n} is {a:?}, wanted {b:?}"
            );
        }
    }

    fn secs(d: Duration) -> f64 {
        d.as_secs_f64()
    }

    const RUNS: usize = 7;

    pub fn main() {
        let args: Vec<String> = std::env::args().collect();
        let number = |nth: usize, otherwise: usize| match args.get(nth) {
            Some(n) => n.replace('_', "").parse().expect("a number"),
            None => otherwise,
        };
        let test = args.get(1).map(String::as_str).unwrap_or("help");
        let started = Instant::now();
        let gpu = Gpu::new().expect("a GPU");
        println!("gpu ready in {} (n=1)", time(secs(started.elapsed())));
        match test {
            "launch" => launch(&gpu, number(2, 2000)),
            "crossover" => crossover(&gpu, number(2, 1000), number(3, 48)),
            "repeat" => repeat(&gpu, number(2, 20_000), number(3, 48), number(4, 2000)),
            "incremental" => incremental(&gpu, number(2, 20_000), number(3, 48), number(4, 2000)),
            "mixed" => mixed(&gpu, number(2, 1000), number(3, 240), number(4, 21)),
            "transfer" => transfer(&gpu, number(2, 2000), number(3, 48), number(4, 2000)),
            "concurrent" => concurrent(&gpu, number(2, 16_384), number(3, 48), number(4, 2000)),
            "planner" => planner(&gpu),
            _ => println!(
                "tests: launch crossover repeat incremental mixed transfer concurrent planner"
            ),
        }
    }

    /// What one kernel launch costs: a chain alone is one node wide, so with one
    /// scenario the GPU does almost nothing but launch.
    fn launch(gpu: &Gpu, months: usize) {
        let m = model(1, months, 1);
        let plan =
            Scenarios::new(&m.g, &inputs_of(&m.returns), &[m.monthly[months - 1].id()]).unwrap();
        println!(
            "chain: {} nodes, {} levels, {} shapes",
            plan.nodes(),
            plan.levels(),
            plan.shapes()
        );
        for scenarios in [1, 1000, 100_000] {
            let inputs = returns_for(scenarios, months);
            let (cpu, want) = bench(RUNS, || plan.run_cpu(&inputs));
            let allowed = gpu.free_memory().unwrap() / 5 * 4;
            let mut compute = Vec::new();
            let (all, got) = bench(RUNS, || {
                let (got, timing) = gpu.run_within(&plan, &inputs, allowed).unwrap();
                compute.push(secs(timing.compute));
                got
            });
            same("gpu", &got, &want);
            let compute = stat(compute[1..].to_vec());
            println!(
                "S={scenarios}: cpu {cpu}; gpu end to end {all}; gpu compute {compute} = {:.2} us a level",
                compute.median / plan.levels() as f64 * 1e6
            );
        }
    }

    /// B. The plan on the CPU and on the GPU as the number of scenarios grows.
    fn crossover(gpu: &Gpu, kinds: usize, months: usize) {
        let started = Instant::now();
        let m = model(kinds, months, 1);
        let built = secs(started.elapsed());
        let started = Instant::now();
        let plan = Scenarios::new(&m.g, &inputs_of(&m.returns), &[m.total.id()]).unwrap();
        println!(
            "kinds={kinds} months={months}: graph {} nodes built in {}; plan {} nodes, {} levels, {} shapes, {} bytes a scenario, made in {}",
            m.g.len(),
            time(built),
            plan.nodes(),
            plan.levels(),
            plan.shapes(),
            plan.bytes_per_scenario(),
            time(secs(started.elapsed()))
        );
        let allowed = gpu.free_memory().unwrap() / 5 * 4;
        for scenarios in [1, 4, 16, 64, 256, 1024, 4096, 16_384] {
            if plan.nodes() as f64 * scenarios as f64 > 3e10 {
                break;
            }
            let inputs = returns_for(scenarios, months);
            let (cpu, want) = bench(RUNS, || plan.run_cpu(&inputs));
            let mut parts = Vec::new();
            let (all, got) = bench(RUNS, || {
                let (got, timing) = gpu.run_within(&plan, &inputs, allowed).unwrap();
                parts.push(timing);
                got
            });
            same("gpu", &got, &want);
            let part = |f: fn(&watershed_gpu::Timing) -> Duration| {
                stat(parts[1..].iter().map(|t| secs(f(t))).collect()).median
            };
            let per = |s: f64| s / (plan.nodes() as f64 * scenarios as f64) * 1e9;
            println!(
                "S={scenarios}: cpu {cpu} [{:.3} ns/node-scenario]; gpu {all} [{:.3}]; gpu parts: up {} compute {} down {}; faster: {}",
                per(cpu.median),
                per(all.median),
                time(part(|t| t.upload)),
                time(part(|t| t.compute)),
                time(part(|t| t.download)),
                if cpu.median < all.median {
                    "cpu"
                } else {
                    "gpu"
                }
            );
        }
    }

    /// D. What keeping the plan, the kernels, the buffers and the inputs is worth.
    fn repeat(gpu: &Gpu, kinds: usize, months: usize, scenarios: usize) {
        let started = Instant::now();
        let m = model(kinds, months, 1);
        println!(
            "graph: {} nodes, built in {} (n=1)",
            m.g.len(),
            time(secs(started.elapsed()))
        );
        let vary = inputs_of(&m.returns);
        let (made, plan) = bench(2, || Scenarios::new(&m.g, &vary, &[m.total.id()]).unwrap());
        println!(
            "plan: {} nodes, {} levels; made in {made}",
            plan.nodes(),
            plan.levels()
        );
        let inputs = returns_for(scenarios, months);
        let allowed = gpu.free_memory().unwrap() / 5 * 4;

        let started = Instant::now();
        let (first, timing) = gpu.run_within(&plan, &inputs, allowed).unwrap();
        println!(
            "first gpu run: {} of which compiling {} (n=1)",
            time(secs(started.elapsed())),
            time(secs(timing.compile))
        );
        let mut parts = Vec::new();
        let (again, got) = bench(RUNS, || {
            let (got, timing) = gpu.run_within(&plan, &inputs, allowed).unwrap();
            parts.push(timing);
            got
        });
        same("again", &got, &first);
        let part = |f: fn(&watershed_gpu::Timing) -> Duration| {
            time(stat(parts[1..].iter().map(|t| secs(f(t))).collect()).median)
        };
        println!(
            "gpu run again (plan and kernels kept, buffers not): {again}; up {} compute {} down {}",
            part(|t| t.upload),
            part(|t| t.compute),
            part(|t| t.download)
        );
        let (load, mut loaded) = bench(2, || gpu.load(&plan, &inputs).unwrap());
        println!("load (plan and inputs to the device): {load}");
        let (run, got) = bench(RUNS, || loaded.run(&plan).unwrap());
        same("loaded", &got, &first);
        println!("loaded run (everything kept; outputs copied back): {run}");
        let (totals, _) = bench(RUNS, || loaded.totals(&plan).unwrap());
        println!("loaded totals (reduced on the device): {totals}");
        let (set, _) = bench(RUNS, || loaded.set_inputs(&plan, &inputs).unwrap());
        println!("replace every input on the device: {set}");
        let (cpu, want) = bench(RUNS, || plan.run_cpu(&inputs));
        same("cpu", &first, &want);
        println!("cpu plan: {cpu}");
    }

    /// A. One account's floor changes after the whole book has been run for
    /// every scenario. Ways to get every scenario's new total.
    fn incremental(gpu: &Gpu, kinds: usize, months: usize, scenarios: usize) {
        let mut m = model(kinds, months, 1);
        let vary = inputs_of(&m.returns);
        let inputs = returns_for(scenarios, months);
        let allowed = gpu.free_memory().unwrap() / 5 * 4;
        println!("graph: {} nodes; {scenarios} scenarios", m.g.len());

        // The accounts that will change, and for each the tree nodes whose
        // values have to be kept to add its new value back in: every child of
        // every node above it.
        let chosen: Vec<usize> = (0..5).map(|n| (n * 7919 + 13) % kinds).collect();
        let mut keep: Vec<NodeId> = Vec::new();
        for &k in &chosen {
            let mut at = k;
            for level in 0..m.tree.len() - 1 {
                let first = at / 8 * 8;
                let end = (first + 8).min(m.tree[level].len());
                keep.extend(ids(&m.tree[level][first..end]));
                at /= 8;
            }
        }
        keep.sort_unstable();
        keep.dedup();
        let mut outputs = vec![m.total.id()];
        outputs.extend(&keep);

        // Before any change: the book on the device, and the kept values here.
        let plan = Scenarios::new(&m.g, &vary, &outputs).unwrap();
        let (base, timing) = gpu.run_within(&plan, &inputs, allowed).unwrap();
        println!(
            "plan: {} nodes, {} levels; {} values kept a scenario ({} bytes in all); first run compute {}",
            plan.nodes(),
            plan.levels(),
            keep.len(),
            keep.len() * scenarios * 8,
            time(secs(timing.compute))
        );
        let mut kept: HashMap<NodeId, Vec<f64>> = HashMap::new();
        for (o, &id) in outputs.iter().enumerate() {
            kept.insert(
                id,
                base.iter()
                    .skip(o)
                    .step_by(outputs.len())
                    .copied()
                    .collect(),
            );
        }

        // The same book with every floor a shared input, kept on the device.
        let mut shared_plan =
            Scenarios::with_shared(&m.g, &vary, &inputs_of(&m.body.floors), &[m.total.id()])
                .unwrap();
        let mut shared_now: Vec<f64> = m.body.floors.iter().map(|&f| m.g.get(f).unwrap()).collect();
        let mut loaded = gpu.load(&shared_plan, &inputs).unwrap();
        println!(
            "plan with {} shared floors: {} nodes, {} shapes",
            kinds,
            shared_plan.nodes(),
            shared_plan.shapes()
        );

        let mut t: HashMap<&str, Vec<f64>> = HashMap::new();
        let mut note = |what: &'static str, started: Instant| {
            t.entry(what).or_default().push(secs(started.elapsed()));
        };
        let mut recalculated = Vec::new();
        for round in 0..RUNS + 1 {
            let k = chosen[round % chosen.len()];
            let floor = m.body.floors[k];
            let new = m.g.get(floor).unwrap() * 1.01;

            // The graph's own single scenario.
            let started = Instant::now();
            m.g.set(floor, new).unwrap();
            let report = m.g.stabilize();
            note("graph: set and update (one scenario)", started);
            recalculated.push(report.recalculated);

            // 1. Make the plan again and run all of it.
            let started = Instant::now();
            let plan = Scenarios::new(&m.g, &vary, &[m.total.id()]).unwrap();
            note("1a. make the whole plan again", started);
            let started = Instant::now();
            let truth = gpu.run_within(&plan, &inputs, allowed).unwrap().0;
            note("1b. gpu run of the whole plan", started);
            let started = Instant::now();
            let cpu_truth = plan.run_cpu(&inputs);
            note("1c. cpu run of the whole plan", started);
            same("cpu whole", &cpu_truth, &truth);

            // 2. Floors shared: replace them and run all of it where it is.
            let started = Instant::now();
            shared_now[k] = new;
            shared_plan.set_shared(&shared_now);
            let got = loaded.run(&shared_plan).unwrap();
            note("2. floors shared, loaded: all of it again", started);
            same("shared", &got, &truth);

            // 3. Only what changed: the account's own nodes, for every
            // scenario, and then the sums above it from the kept values.
            let end = m.body.ends[k].id();
            let started = Instant::now();
            let patch = Scenarios::new(&m.g, &vary, &[end]).unwrap();
            note("3a. make a plan of the account alone", started);
            let started = Instant::now();
            let on_cpu = patch.run_cpu(&inputs);
            note("3b. account alone on the cpu", started);
            let started = Instant::now();
            let on_gpu = gpu.run_within(&patch, &inputs, allowed).unwrap().0;
            note("3c. account alone on the gpu (inputs copied up)", started);
            same("patch", &on_gpu, &on_cpu);
            let mut patch_loaded = gpu.load(&patch, &inputs).unwrap();
            patch_loaded.run(&patch).unwrap();
            let started = Instant::now();
            let resident = patch_loaded.run(&patch).unwrap();
            note("3d. account alone on the gpu (inputs resident)", started);
            same("patch resident", &resident, &on_cpu);

            let started = Instant::now();
            kept.insert(end, on_cpu);
            let mut at = k;
            for level in 0..m.tree.len() - 1 {
                let first = at / 8 * 8;
                let last = (first + 8).min(m.tree[level].len());
                let members: Vec<&Vec<f64>> = m.tree[level][first..last]
                    .iter()
                    .map(|n| &kept[&n.id()])
                    .collect();
                let sums: Vec<f64> = (0..scenarios)
                    .map(|s| members[1..].iter().fold(members[0][s], |sum, m| sum + m[s]))
                    .collect();
                at /= 8;
                kept.insert(m.tree[level + 1][at].id(), sums);
            }
            note(
                "3e. the sums above it, from kept values, on the cpu",
                started,
            );
            same("patched totals", &kept[&m.total.id()], &truth);
        }
        println!(
            "nodes the graph recalculated per change: {:?}",
            recalculated
        );
        let mut rows: Vec<_> = t.into_iter().collect();
        rows.sort_by_key(|(what, _)| *what);
        for (what, samples) in rows {
            // The first round is the warm-up.
            println!("{what}: {}", stat(samples[1..].to_vec()));
        }
        println!("checked: 1, 2 and 3 give every scenario the same total, to the last bit");
    }

    /// The rates for some months as one piece wants them, then the accounts'
    /// values if it takes them.
    fn rows_for(
        piece: &Piece,
        rates: &[f64],
        months: usize,
        values: Option<&[f64]>,
        kinds: usize,
        scenarios: usize,
    ) -> Vec<f64> {
        let width = piece.plan.inputs();
        let mut rows = vec![0.0; scenarios * width];
        for (s, row) in rows.chunks_mut(width).enumerate() {
            let n = piece.months.len();
            row[..n].copy_from_slice(
                &rates[s * months + piece.months.start..s * months + piece.months.end],
            );
            if piece.takes_values {
                row[n..].copy_from_slice(&values.expect("values")[s * kinds..(s + 1) * kinds]);
            }
        }
        rows
    }

    /// C. A deep, narrow part (the chain, stepped `sub` times a month) feeding a
    /// wide part (the accounts). The cut between them is one value a month, no
    /// wider than the inputs.
    fn mixed(gpu: &Gpu, kinds: usize, months: usize, sub: usize) {
        let m = model(kinds, months, sub);
        let vary = inputs_of(&m.returns);
        let whole = Scenarios::new(&m.g, &vary, &[m.total.id()]).unwrap();
        let narrow = Scenarios::new(&m.g, &vary, &ids(&m.monthly)).unwrap();
        let wide = piece(kinds, 0..months, months);
        println!(
            "whole: {} nodes, {} levels. chain: {} nodes, {} levels. accounts: {} nodes, {} levels",
            whole.nodes(),
            whole.levels(),
            narrow.nodes(),
            narrow.levels(),
            wide.plan.nodes(),
            wide.plan.levels()
        );
        let allowed = gpu.free_memory().unwrap() / 5 * 4;
        for scenarios in [100, 1000, 10_000] {
            let inputs = returns_for(scenarios, months);
            let (cpu, want) = bench(RUNS, || whole.run_cpu(&inputs));
            let (all_gpu, got) =
                bench(RUNS, || gpu.run_within(&whole, &inputs, allowed).unwrap().0);
            same("gpu", &got, &want);
            let mut parts: [Vec<f64>; 2] = Default::default();
            let (hybrid, got) = bench(RUNS, || {
                let started = Instant::now();
                let rates = narrow.run_cpu(&inputs);
                parts[0].push(secs(started.elapsed()));
                let started = Instant::now();
                let got = gpu.run_within(&wide.plan, &rates, allowed).unwrap().0;
                parts[1].push(secs(started.elapsed()));
                got
            });
            same("chain on cpu, accounts on gpu", &got, &want);
            let (split_gpu, got) = bench(RUNS, || {
                let rates = gpu.run_within(&narrow, &inputs, allowed).unwrap().0;
                gpu.run_within(&wide.plan, &rates, allowed).unwrap().0
            });
            same("both on gpu, as two plans", &got, &want);
            let (wide_cpu, _) = bench(RUNS, || {
                let rates = gpu.run_within(&narrow, &inputs, allowed).unwrap().0;
                wide.plan.run_cpu(&rates)
            });
            let best = cpu.median.min(all_gpu.median);
            println!("S={scenarios}:");
            println!("  whole on cpu: {cpu}");
            println!("  whole on gpu: {all_gpu}");
            println!(
                "  chain on cpu, accounts on gpu: {hybrid} [chain {} + accounts {}] = {:.2}x the better single device",
                time(stat(parts[0][1..].to_vec()).median),
                time(stat(parts[1][1..].to_vec()).median),
                hybrid.median / best
            );
            println!("  chain on gpu, accounts on gpu, two plans: {split_gpu}");
            println!("  chain on gpu, accounts on cpu (the wrong way round): {wide_cpu}");
        }
    }

    /// E. The accounts cut into pieces by time. Every account's value crosses
    /// each cut, so a cut is as wide as the book.
    fn transfer(gpu: &Gpu, kinds: usize, months: usize, scenarios: usize) {
        let rates: Vec<f64> = returns_for(scenarios, months)
            .iter()
            .map(|r| 0.03 + r * 0.1)
            .collect();
        let allowed = gpu.free_memory().unwrap() / 5 * 4;
        let whole = piece(kinds, 0..months, months);
        println!(
            "accounts: {} nodes, {} levels; a cut carries {} values a scenario, {} bytes for {scenarios}",
            whole.plan.nodes(),
            whole.plan.levels(),
            kinds,
            kinds * scenarios * 8
        );
        let (cpu, want) = bench(RUNS, || whole.plan.run_cpu(&rates));
        let (all_gpu, got) = bench(RUNS, || {
            gpu.run_within(&whole.plan, &rates, allowed).unwrap().0
        });
        same("gpu", &got, &want);
        println!("whole on cpu: {cpu}");
        println!("whole on gpu: {all_gpu}");
        for pieces in [2, 4, 8] {
            let cut: Vec<Piece> = (0..pieces)
                .map(|p| {
                    piece(
                        kinds,
                        months * p / pieces..months * (p + 1) / pieces,
                        months,
                    )
                })
                .collect();
            // Which device each piece runs on: all on the GPU (the cost of
            // cutting alone), or turn about.
            for (name, on_gpu) in [
                (
                    "every piece on the gpu, through the host",
                    vec![true; pieces],
                ),
                (
                    "pieces turn about, gpu first",
                    (0..pieces).map(|p| p % 2 == 0).collect::<Vec<_>>(),
                ),
            ] {
                let mut laying_out = Vec::new();
                let (took, got) = bench(RUNS, || {
                    let mut values: Option<Vec<f64>> = None;
                    let mut layout = 0.0;
                    for (piece, &on_gpu) in cut.iter().zip(&on_gpu) {
                        let started = Instant::now();
                        let rows =
                            rows_for(piece, &rates, months, values.as_deref(), kinds, scenarios);
                        layout += secs(started.elapsed());
                        values = Some(match on_gpu {
                            true => gpu.run_within(&piece.plan, &rows, allowed).unwrap().0,
                            false => piece.plan.run_cpu(&rows),
                        });
                    }
                    laying_out.push(layout);
                    values.expect("at least one piece")
                });
                same(name, &got, &want);
                println!(
                    "{pieces} pieces, {name}: {took} = {:.2}x whole on gpu (laying rows out on the host: {})",
                    took.median / all_gpu.median,
                    time(stat(laying_out[1..].to_vec()).median)
                );
            }
        }
    }

    /// F. Two parts of the book that do not depend on each other: both on the
    /// GPU, or one on each device at the same time.
    fn concurrent(gpu: &Gpu, kinds: usize, months: usize, scenarios: usize) {
        let m = model(kinds, months, 1);
        let vary = inputs_of(&m.returns);
        let inputs = returns_for(scenarios, months);
        let allowed = gpu.free_memory().unwrap() / 5 * 4;
        let top = m.tree.len() - 1;
        let whole = Scenarios::new(&m.g, &vary, &[m.total.id()]).unwrap();
        let (cpu, _) = bench(RUNS, || whole.run_cpu(&inputs));
        println!("whole book ({} nodes): cpu {cpu}", whole.nodes());
        // The share given to the CPU: one of the root's parts, then one of the
        // parts below that, and so on.
        for depth in 1..=3.min(top) {
            let level = &m.tree[top - depth];
            let small = level[0].id();
            let rest = ids(&level[1..]);
            let both = [vec![small], rest.clone()].concat();
            let together = Scenarios::new(&m.g, &vary, &both).unwrap();
            let cpu_part = Scenarios::new(&m.g, &vary, &[small]).unwrap();
            let gpu_part = Scenarios::new(&m.g, &vary, &rest).unwrap();
            let (one, want) = bench(RUNS, || {
                gpu.run_within(&together, &inputs, allowed).unwrap().0
            });
            let mut alone: [Vec<f64>; 2] = Default::default();
            let (two, got) = bench(RUNS, || {
                let (a, b) = std::thread::scope(|scope| {
                    let a = scope.spawn(|| {
                        let started = Instant::now();
                        (cpu_part.run_cpu(&inputs), secs(started.elapsed()))
                    });
                    let started = Instant::now();
                    let b = gpu.run_within(&gpu_part, &inputs, allowed).unwrap().0;
                    (a.join().unwrap(), (b, secs(started.elapsed())))
                });
                alone[0].push(a.1);
                alone[1].push(b.1);
                let outputs = rest.len();
                let mut joined = Vec::with_capacity(scenarios * (outputs + 1));
                for s in 0..scenarios {
                    joined.push(a.0[s]);
                    joined.extend_from_slice(&b.0[s * outputs..(s + 1) * outputs]);
                }
                joined
            });
            same("one on each", &got, &want);
            println!(
                "cpu share {} of {} nodes ({:.1}%): all on gpu {one}; one on each at once {two} [cpu part {}, gpu part {}] = {:.3}x",
                cpu_part.nodes(),
                together.nodes(),
                100.0 * cpu_part.nodes() as f64 / together.nodes() as f64,
                time(stat(alone[0][1..].to_vec()).median),
                time(stat(alone[1][1..].to_vec()).median),
                two.median / one.median
            );
        }
    }

    // ---- A basic cost-based choice ----
    //
    // Constants measured on one machine (Ryzen 9 5950X, RTX 3090, WSL2) by the
    // `launch`, `crossover 1000 48` and `transfer` tests above. They are not
    // fitted to the cases `planner` then tries them on, apart from the two
    // that reuse the `crossover 1000 48` graph.

    /// One node for one scenario on one thread, in seconds.
    const CPU_NODE: f64 = 18e-9;
    /// How many threads' worth of work 32 threads do.
    const CPU_SPEEDUP: f64 = 15.0;
    /// Getting the thread pool going, when there is more than one scenario.
    const CPU_WAKE: f64 = 0.3e-3;
    /// A GPU run's fixed cost, and per node of plan copied up.
    const GPU_FIXED: f64 = 0.8e-3;
    const GPU_PLAN_NODE: f64 = 3.7e-9;
    /// One kernel launch. A plan has at least one a level; how many more is not
    /// public, so this is an underestimate for plans with several shapes a level.
    const GPU_LAUNCH: f64 = 7e-6;
    /// One node for one scenario on the GPU.
    const GPU_NODE: f64 = 0.022e-9;
    /// One input or output value copied between host and device.
    const MOVE: f64 = 3.5e-9;

    fn cpu_cost(plan: &Scenarios, scenarios: usize) -> f64 {
        let threads = (scenarios as f64).min(CPU_SPEEDUP);
        let wake = if scenarios > 1 { CPU_WAKE } else { 0.0 };
        wake + plan.nodes() as f64 * scenarios as f64 * CPU_NODE / threads
    }

    fn gpu_cost(plan: &Scenarios, scenarios: usize) -> f64 {
        let (nodes, s) = (plan.nodes() as f64, scenarios as f64);
        GPU_FIXED
            + nodes * GPU_PLAN_NODE
            + plan.levels() as f64 * GPU_LAUNCH
            + nodes * s * GPU_NODE
            + (plan.inputs() + plan.outputs()) as f64 * s * MOVE
    }

    /// Three ways to run the model (all of it on the CPU, all of it on the GPU,
    /// the chain on the CPU and the accounts on the GPU), what the constants
    /// above say each will take, and what each takes.
    fn planner(gpu: &Gpu) {
        let allowed = gpu.free_memory().unwrap() / 5 * 4;
        let cases: [(usize, usize, usize, &[usize]); 5] = [
            (8, 12, 1, &[16, 4096, 65_536]),
            (1000, 48, 1, &[4, 64, 1024]),
            (1000, 240, 21, &[100, 1000, 10_000]),
            (5000, 240, 1, &[100, 1000]),
            (64, 240, 60, &[100, 1000, 10_000]),
        ];
        let (mut right, mut tried, mut worst) = (0, 0, 1.0f64);
        println!(
            "kinds months sub scenarios | predicted cpu gpu hybrid | measured cpu gpu hybrid | chosen, best, time of chosen / time of best"
        );
        for (kinds, months, sub, counts) in cases {
            let m = model(kinds, months, sub);
            let vary = inputs_of(&m.returns);
            let whole = Scenarios::new(&m.g, &vary, &[m.total.id()]).unwrap();
            let narrow = Scenarios::new(&m.g, &vary, &ids(&m.monthly)).unwrap();
            let wide = piece(kinds, 0..months, months);
            for &scenarios in counts {
                let inputs = returns_for(scenarios, months);
                let predicted = [
                    cpu_cost(&whole, scenarios),
                    gpu_cost(&whole, scenarios),
                    cpu_cost(&narrow, scenarios) + gpu_cost(&wide.plan, scenarios),
                ];
                let (cpu, want) = bench(4, || whole.run_cpu(&inputs));
                let (all_gpu, got) =
                    bench(4, || gpu.run_within(&whole, &inputs, allowed).unwrap().0);
                same("gpu", &got, &want);
                let (hybrid, got) = bench(4, || {
                    let rates = narrow.run_cpu(&inputs);
                    gpu.run_within(&wide.plan, &rates, allowed).unwrap().0
                });
                same("hybrid", &got, &want);
                let measured = [cpu.median, all_gpu.median, hybrid.median];
                let least =
                    |xs: &[f64; 3]| (0..3).min_by(|&a, &b| xs[a].total_cmp(&xs[b])).unwrap();
                let (chosen, best) = (least(&predicted), least(&measured));
                let regret = measured[chosen] / measured[best];
                tried += 1;
                right += usize::from(chosen == best);
                worst = worst.max(regret);
                let names = ["cpu", "gpu", "hybrid"];
                println!(
                    "{kinds} {months} {sub} {scenarios} | {} {} {} | {} {} {} (n={}) | {}, {}, {regret:.2}",
                    time(predicted[0]),
                    time(predicted[1]),
                    time(predicted[2]),
                    time(measured[0]),
                    time(measured[1]),
                    time(measured[2]),
                    all_gpu.n,
                    names[chosen],
                    names[best]
                );
            }
        }
        println!(
            "chose the fastest in {right} of {tried}; at worst the choice took {worst:.2}x the fastest"
        );
    }
}
