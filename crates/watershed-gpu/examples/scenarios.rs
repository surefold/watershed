//! Runs the benchmark graph of `watershed-core` for many sets of market
//! prices, three ways: on the graph itself (set the prices, update, read the
//! total), as a scenario plan on the CPU, and as a scenario plan on the GPU.
//!
//!   10,000 market prices (inputs, all varied)
//!   → 500,000 trades, each priced from two random market prices
//!   → a tree of subtotals adding the trades up, pair by pair, to one total
//!
//!     cargo run --release --features cuda --example scenarios
//!
//! or choose the number of trades and of scenarios:
//!
//!     cargo run --release --features cuda --example scenarios -- 500000 2000
//!
//! Without `--features cuda` the GPU is left out.

use std::time::{Duration, Instant};

use watershed_core::prelude::*;
use watershed_gpu::Scenarios;

const MARKET_PRICES: usize = 10_000;

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn unit(&mut self) -> f64 {
        (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let number = |nth: usize, otherwise: usize| match std::env::args().nth(nth) {
        Some(n) => n
            .replace('_', "")
            .parse()
            .expect("the argument is a number"),
        None => otherwise,
    };
    let (trades, scenarios) = (number(1, 500_000), number(2, 1_000));

    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut g = Graph::new();
    g.reserve(MARKET_PRICES + 2 * trades, 4 * trades);
    let prices: Vec<Input<f64>> = (0..MARKET_PRICES)
        .map(|i| g.input(&format!("price{i}"), 100.0 + rng.unit()))
        .collect::<Result<_, _>>()?;
    let mut level: Vec<Node<f64>> = Vec::with_capacity(trades);
    for t in 0..trades {
        let a = prices[rng.below(MARKET_PRICES)];
        let b = prices[rng.below(MARKET_PRICES)];
        let (qty_a, qty_b) = (rng.unit() * 10.0, rng.unit() * -5.0);
        level.push(g.define(&format!("trade{t}"), a * qty_a + b * qty_b)?);
    }
    let mut depth = 0;
    while level.len() > 1 {
        depth += 1;
        let mut next = Vec::with_capacity(level.len() / 2 + 1);
        for (k, pair) in level.chunks(2).enumerate() {
            next.push(match pair {
                [x, y] => g.define(&format!("sum{depth}_{k}"), *x + *y)?,
                [x] => *x,
                _ => unreachable!(),
            });
        }
        level = next;
    }
    let total = level[0];
    g.stabilize();

    let started = Instant::now();
    let vary: Vec<_> = prices.iter().map(|p| p.id()).collect();
    let plan = Scenarios::new(&g, &vary, &[total.id()])?;
    println!(
        "graph: {} nodes. plan: {} nodes in {} levels, {:.1} MB a scenario, made in {}",
        g.len(),
        plan.nodes(),
        plan.levels(),
        plan.bytes_per_scenario() as f64 / 1e6,
        time(started.elapsed())
    );
    println!("{scenarios} scenarios, each a new value for all {MARKET_PRICES} prices");
    println!();

    let inputs: Vec<f64> = (0..scenarios * MARKET_PRICES)
        .map(|_| 100.0 + rng.unit())
        .collect();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());

    // The plan on every core. Everything else is checked against this.
    let started = Instant::now();
    let results = plan.run_cpu(&inputs);
    let cpu = started.elapsed();

    // The graph itself, for a few scenarios: it takes the same time for each.
    let few = scenarios.min(10);
    let started = Instant::now();
    for (s, scenario) in inputs.chunks(MARKET_PRICES).take(few).enumerate() {
        for (&price, &x) in prices.iter().zip(scenario) {
            g.set(price, x)?;
        }
        g.stabilize();
        assert_same("the graph", s, g.get(total)?, results[s]);
    }
    let graph = started.elapsed() / few as u32;

    // The plan on one core, for a few scenarios.
    let one_core = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
    let started = Instant::now();
    let some = one_core.install(|| plan.run_cpu(&inputs[..few * MARKET_PRICES]));
    let single = started.elapsed() / few as u32;
    for (s, (&a, &b)) in some.iter().zip(&results).enumerate() {
        assert_same("the plan on one core", s, a, b);
    }

    println!(
        "{:<46} {:>14} {:>16}",
        "", "a scenario", "scenarios a second"
    );
    row(
        &format!("the graph: set, update, read ({cores} cores)"),
        graph,
    );
    row("the plan, 1 core", single);
    row(&format!("the plan, {cores} cores"), cpu / scenarios as u32);

    #[cfg(feature = "gpu")]
    {
        use watershed_gpu::Gpu;
        let started = Instant::now();
        let gpu = Gpu::new()?;
        let ready = started.elapsed();
        let allowed = gpu.free_memory()? / 5 * 4;
        // Twice: the first run pays for things the GPU does once.
        let mut run = None;
        for _ in 0..2 {
            let started = Instant::now();
            let (got, timing) = gpu.run_within(&plan, &inputs, allowed)?;
            run = Some((got, timing, started.elapsed()));
        }
        let (got, timing, took) = run.expect("it ran");
        for (s, (&a, &b)) in got.iter().zip(&results).enumerate() {
            assert_same("the GPU", s, a, b);
        }
        row("the plan, GPU", took / scenarios as u32);
        println!();
        println!(
            "GPU: ready in {}, then {} for all {scenarios}: {} copying up, {} calculating, {} copying back, {} scenarios at a time",
            time(ready),
            time(took),
            time(timing.upload),
            time(timing.compute),
            time(timing.download),
            timing.scenarios_at_a_time
        );
        println!(
            "GPU against the plan on {cores} cores: {:.1}x",
            cpu.as_secs_f64() / took.as_secs_f64()
        );
    }

    println!();
    println!("checked: every way gives the same totals, to the last bit");
    Ok(())
}

fn assert_same(who: &str, scenario: usize, got: f64, want: f64) {
    assert!(
        got.to_bits() == want.to_bits(),
        "scenario {scenario}: {who} gives {got:?}, the plan on every core {want:?}"
    );
}

fn row(label: &str, each: Duration) {
    println!(
        "{label:<46} {:>14} {:>16.1}",
        time(each),
        1.0 / each.as_secs_f64()
    );
}

fn time(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 0.001 {
        format!("{:.1} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.1} ms", s * 1e3)
    } else {
        format!("{s:.2} s")
    }
}
