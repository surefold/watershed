//! What keeping history costs, on the graph shape of the `benchmark` example:
//! 10,000 prices, 500,000 trades and a tree of totals, about a million nodes.
//!
//!     cargo run --release --example history

use std::hint::black_box;
use std::time::{Duration, Instant};

use watershed_core::prelude::*;

const MARKET_PRICES: usize = 10_000;
const TRADES: usize = 500_000;
const KEEP: u64 = 1_000;
const UPDATES_PER_ROUND: usize = 2_000;
const ROUNDS: usize = 7;

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

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn main() -> Result<(), CalcError> {
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut g = Graph::new();
    g.reserve(MARKET_PRICES + 2 * TRADES, 4 * TRADES);
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
    println!("graph: {} nodes", g.len());

    // Rounds alternate, so slow drift in the machine affects both alike.
    let (mut off, mut on) = (Vec::new(), Vec::new());
    for round in 0..2 * ROUNDS {
        let keeping = round % 2 == 1;
        if keeping {
            g.keep_history(KEEP);
        } else {
            g.stop_history();
        }
        for _ in 0..UPDATES_PER_ROUND {
            let p = prices[rng.below(MARKET_PRICES)];
            g.set(p, 100.0 + rng.unit())?;
            let started = Instant::now();
            black_box(g.stabilize());
            let took = started.elapsed();
            if keeping { &mut on } else { &mut off }.push(took);
        }
    }
    println!("one price changes, history off:     {:?}", median(off));
    println!("one price changes, keeping {KEEP}: {:?}", median(on));

    // History is on and full. Each kept update overwrote `total`, so reading
    // it `back` updates ago walks `back` versions.
    let versions = g.history_versions();
    println!(
        "kept: {versions} versions, {} per update, {:.1} MB (32 bytes each)",
        versions as u64 / KEEP,
        versions as f64 * 32.0 / 1e6
    );
    let (oldest, latest) = g.history_range().expect("history is on");
    let id = total.id();
    for back in [0, 1, 10, 100, 1_000] {
        let epoch = latest - back;
        assert!(epoch >= oldest);
        let mut samples = Vec::new();
        for _ in 0..2_000 {
            let started = Instant::now();
            black_box(g.value_at(id, epoch)?);
            samples.push(started.elapsed());
        }
        println!("read total {back} updates back: {:?}", median(samples));
    }
    Ok(())
}
