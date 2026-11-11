//! Does it matter in what order a graph's nodes are made?
//!
//! Nodes are stored in the order they are made, and an update works through
//! the graph level by level, each level in that order. This builds the
//! benchmark's graph (market prices → trades → a tree of subtotals) twice,
//! the same nodes with the same links, made in two orders:
//!
//!   level by level:      every trade, then every first subtotal, and so on
//!   subtree by subtree:  each subtotal straight after the two it adds up,
//!                        so that nodes which change together sit together
//!
//! and times the same updates on both.
//!
//!     cargo run --release --example placement
//!
//! or choose the number of trades, a power of two:
//!
//!     cargo run --release --example placement -- 2097152

use std::time::{Duration, Instant};

use watershed_core::prelude::*;

const MARKET_PRICES: usize = 10_000;
const DEFAULT_TRADES: usize = 1 << 19;

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

/// A trade: two market prices and how much of each.
type Trade = (usize, usize, f64, f64);

fn trade(g: &mut Graph, prices: &[Input<f64>], trades: &[Trade], t: usize) -> Node<f64> {
    let (a, b, qty_a, qty_b) = trades[t];
    g.define(&format!("trade{t}"), prices[a] * qty_a + prices[b] * qty_b)
        .expect("a new name")
}

fn level_by_level(g: &mut Graph, prices: &[Input<f64>], trades: &[Trade]) -> Node<f64> {
    let mut level: Vec<Node<f64>> = (0..trades.len())
        .map(|t| trade(g, prices, trades, t))
        .collect();
    let mut depth = 0;
    while level.len() > 1 {
        depth += 1;
        level = level
            .chunks(2)
            .enumerate()
            .map(|(k, pair)| {
                g.define(&format!("sum{depth}_{k}"), pair[0] + pair[1])
                    .expect("a new name")
            })
            .collect();
    }
    level[0]
}

fn subtree_by_subtree(
    g: &mut Graph,
    prices: &[Input<f64>],
    trades: &[Trade],
    from: usize,
    to: usize,
    depth: u32,
) -> Node<f64> {
    if to - from == 1 {
        return trade(g, prices, trades, from);
    }
    let middle = (from + to) / 2;
    let left = subtree_by_subtree(g, prices, trades, from, middle, depth - 1);
    let right = subtree_by_subtree(g, prices, trades, middle, to, depth - 1);
    g.define(&format!("sum{depth}_{}", from >> depth), left + right)
        .expect("a new name")
}

fn main() -> Result<(), CalcError> {
    let count: usize = match std::env::args().nth(1) {
        Some(n) => n
            .replace('_', "")
            .parse()
            .expect("the argument is a number of trades"),
        None => DEFAULT_TRADES,
    };
    assert!(
        count.is_power_of_two(),
        "the number of trades must be a power of two, so that both orders make the same tree"
    );
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let start: Vec<f64> = (0..MARKET_PRICES).map(|_| 100.0 + rng.unit()).collect();
    let trades: Vec<Trade> = (0..count)
        .map(|_| {
            (
                rng.below(MARKET_PRICES),
                rng.below(MARKET_PRICES),
                rng.unit() * 10.0,
                rng.unit() * -5.0,
            )
        })
        .collect();

    println!(
        "{:<22} {:>14} {:>14} {:>14} {:>14}",
        "median of many runs", "everything", "one price", "1% of prices", "10% of prices"
    );
    let mut totals = Vec::new();
    for order in ["level by level", "subtree by subtree"] {
        let mut g = Graph::new();
        g.reserve(MARKET_PRICES + 2 * count, 4 * count);
        g.set_parallel_threshold(usize::MAX);
        let prices: Vec<Input<f64>> = start
            .iter()
            .enumerate()
            .map(|(i, &p)| g.input(&format!("price{i}"), p))
            .collect::<Result<_, _>>()?;
        let total = if order == "level by level" {
            level_by_level(&mut g, &prices, &trades)
        } else {
            subtree_by_subtree(&mut g, &prices, &trades, 0, count, count.trailing_zeros())
        };

        // The same changes for both orders.
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut change = |g: &mut Graph, how_many: usize| {
            for _ in 0..how_many {
                let p = prices[rng.below(MARKET_PRICES)];
                g.set(p, 100.0 + rng.unit()).unwrap();
            }
            g.stabilize();
        };
        let everything = median(15, || {
            g.recalculate_everything();
        });
        let one = median(2_000, || change(&mut g, 1));
        let some = median(50, || change(&mut g, MARKET_PRICES / 100));
        let many = median(15, || change(&mut g, MARKET_PRICES / 10));
        println!(
            "{order:<22} {:>14} {:>14} {:>14} {:>14}",
            time(everything),
            time(one),
            time(some),
            time(many)
        );
        g.check_consistency()
            .expect("incremental and full recalculation agree");
        totals.push(g.get(total)?);
    }
    assert_eq!(totals[0], totals[1], "both orders make the same graph");
    println!();
    println!("on one core; both graphs checked against a full recalculation, and they agree");
    Ok(())
}

/// Runs `step` `runs` times and returns the median time.
fn median(runs: usize, mut step: impl FnMut()) -> Duration {
    let mut times: Vec<Duration> = (0..runs)
        .map(|_| {
            let started = Instant::now();
            step();
            started.elapsed()
        })
        .collect();
    times.sort();
    times[runs / 2]
}

fn time(d: Duration) -> String {
    if d.as_secs_f64() < 0.001 {
        format!("{:.1} µs", d.as_secs_f64() * 1e6)
    } else {
        format!("{:.1} ms", d.as_secs_f64() * 1e3)
    }
}
