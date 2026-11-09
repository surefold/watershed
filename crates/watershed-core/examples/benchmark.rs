//! Compares an incremental update with recalculating everything, on a graph
//! of about a million nodes shaped like a real workload:
//!
//!   10,000 market prices (inputs)
//!   → 500,000 trades, each priced from two random market prices
//!   → a tree of subtotals adding the trades up, pair by pair, to one total
//!
//! Run it with optimisations on:
//!
//!     cargo run --release --example benchmark

use std::time::{Duration, Instant};

use watershed_core::prelude::*;

const MARKET_PRICES: usize = 10_000;
const TRADES: usize = 500_000;

/// A small deterministic random number generator, so every run builds the same graph.
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

fn main() -> Result<(), CalcError> {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut g = Graph::new();

    let started = Instant::now();
    let prices: Vec<Input<f64>> = (0..MARKET_PRICES)
        .map(|i| g.input(&format!("price{i}"), 100.0 + rng.unit()))
        .collect::<Result<_, _>>()?;

    let mut level: Vec<Node<f64>> = Vec::with_capacity(TRADES);
    for t in 0..TRADES {
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
    let build = started.elapsed();

    println!(
        "graph: {} nodes ({} inputs, {} trades, {} levels of totals), built in {}",
        g.len(),
        MARKET_PRICES,
        TRADES,
        depth,
        ms(build)
    );
    println!();

    // Each scenario is timed many times and the median reported, because a
    // single timing on a busy machine can be far off.
    println!(
        "{:<44} {:>12} {:>16}",
        "median of many runs", "time", "recalculated"
    );

    let (full, count) = median(15, || ((), g.recalculate_everything()));
    row("recalculate everything", ms(full), count);

    let (one, count) = median(2_000, || {
        let p = prices[rng.below(MARKET_PRICES)];
        g.set(p, 100.0 + rng.unit()).unwrap();
        ((), g.stabilize().recalculated)
    });
    row("incremental: one price changes", us(one), count);

    let (batch, count) = median(100, || {
        for _ in 0..MARKET_PRICES / 100 {
            let p = prices[rng.below(MARKET_PRICES)];
            g.set(p, 100.0 + rng.unit()).unwrap();
        }
        ((), g.stabilize().recalculated)
    });
    row(
        "incremental: 1% of prices change together",
        ms(batch),
        count,
    );

    println!();
    println!(
        "one price: {:.0}x faster than recalculating everything",
        full.as_secs_f64() / one.as_secs_f64()
    );
    println!(
        "1% of prices: {:.0}x faster",
        full.as_secs_f64() / batch.as_secs_f64()
    );

    // The answer must be the same either way.
    g.check_consistency()
        .expect("incremental and full recalculation agree");
    println!(
        "checked: incremental result matches a full recalculation (total = {:.2})",
        g.get(total)?
    );
    Ok(())
}

/// Runs `step` `runs` times; returns the median time and the median count it reports.
fn median(runs: usize, mut step: impl FnMut() -> ((), usize)) -> (Duration, usize) {
    let mut times = Vec::with_capacity(runs);
    let mut counts = Vec::with_capacity(runs);
    for _ in 0..runs {
        let started = Instant::now();
        let ((), count) = step();
        times.push(started.elapsed());
        counts.push(count);
    }
    times.sort();
    counts.sort();
    (times[runs / 2], counts[runs / 2])
}

fn row(label: &str, time: String, count: usize) {
    println!("{label:<44} {time:>12} {count:>16}");
}

fn ms(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1e3)
}

fn us(d: Duration) -> String {
    format!("{:.1} µs", d.as_secs_f64() * 1e6)
}
