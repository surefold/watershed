//! A Monte Carlo risk run: how much could this portfolio lose over the next
//! ten trading days?
//!
//! The graph is the portfolio as a desk would build it:
//!
//!   500 share prices (inputs)
//!   → 200,000 positions, each the profit or loss of a holding of shares,
//!     calls, puts or a swap of one share's rise for another's, bought or sold
//!   → 20 desks, each the sum of its positions
//!   → the firm, the sum of the desks
//!
//! Options are worth `max(price - strike, 0)` when they expire, so the firm's
//! profit is not a straight line in the prices and there is no formula for
//! how it is spread. The way to find out is to try many possible futures:
//! draw the 500 prices ten days from now, many thousands of times, and work
//! the whole portfolio out for each.
//!
//!     cargo run --release --features cuda --example monte_carlo
//!
//! or choose the number of futures to try:
//!
//!     cargo run --release --features cuda --example monte_carlo -- 250000
//!
//! Without `--features cuda` it runs on the CPU's cores, with fewer futures
//! unless told otherwise. The numbers are made up; the shape is not.

use std::time::{Duration, Instant};

use rayon::prelude::*;
use watershed_core::IntoExpr;
use watershed_core::prelude::*;
use watershed_gpu::Scenarios;

const SHARES: usize = 500;
const POSITIONS: usize = 200_000;
const DESKS: usize = 20;
/// Ten trading days, in years.
const HORIZON: f64 = 10.0 / 252.0;

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
    /// Between 0 and 1, never 0.
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }
    /// From the bell curve: mean 0, spread 1.
    fn normal(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
}

/// What is known about a share today.
struct Share {
    price: f64,
    /// How much it moves in a year, as a fraction.
    volatility: f64,
    /// How much of its movement it shares with the whole market.
    with_market: f64,
}

/// The bell curve's area to the left of `x`, to about seven places.
fn area_left_of(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.231_641_9 * x.abs());
    let poly = t
        * (0.319_381_530
            + t * (-0.356_563_782
                + t * (1.781_477_937 + t * (-1.821_255_978 + t * 1.330_274_429))));
    let right = (-x * x / 2.0).exp() / std::f64::consts::TAU.sqrt() * poly;
    if x >= 0.0 { 1.0 - right } else { right }
}

/// What a call pays on average: the right to buy at `strike` something worth
/// `price` now, whose price by then is spread by `spread` (Black and Scholes,
/// with no interest).
fn fair_call(price: f64, strike: f64, spread: f64) -> f64 {
    let d1 = (price / strike).ln() / spread + spread / 2.0;
    price * area_left_of(d1) - strike * area_left_of(d1 - spread)
}

/// One node adding up `nodes`: eight at a time, then those eight at a time.
fn sum(g: &mut Graph, name: &str, mut nodes: Vec<Node<f64>>) -> Result<Node<f64>, CalcError> {
    let mut round = 0;
    while nodes.len() > 1 {
        round += 1;
        let mut next = Vec::with_capacity(nodes.len() / 8 + 1);
        for (k, group) in nodes.chunks(8).enumerate() {
            let mut all = group[0].into_expr();
            for &node in &group[1..] {
                all = all + node;
            }
            next.push(g.define(&format!("{name}_{round}_{k}"), all)?);
        }
        nodes = next;
    }
    Ok(nodes[0])
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let futures: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse().expect("a number of futures"),
        None if cfg!(feature = "gpu") => 100_000,
        None => 10_000,
    };

    // The portfolio.
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut g = Graph::new();
    let shares: Vec<Share> = (0..SHARES)
        .map(|_| Share {
            price: rng.between(20.0, 400.0),
            volatility: rng.between(0.15, 0.60),
            with_market: rng.between(0.3, 0.9),
        })
        .collect();
    let prices: Vec<Input<f64>> = shares
        .iter()
        .enumerate()
        .map(|(i, share)| g.input(&format!("price{i}"), share.price))
        .collect::<Result<_, _>>()?;

    let mut desks: Vec<Vec<Node<f64>>> = vec![Vec::new(); DESKS];
    for p in 0..POSITIONS {
        let i = rng.below(SHARES);
        let (share, price) = (&shares[i], prices[i]);
        // Bought (positive) or sold (negative), a few hundred at a time.
        let quantity = rng.between(-400.0, 500.0).round();
        let strike = (share.price * rng.between(0.85, 1.15)).round();
        // What an option costs today is what it pays on average over the
        // futures drawn below, so on average a position makes nothing.
        let spread = share.volatility * HORIZON.sqrt();
        let call = fair_call(share.price, strike, spread);
        let profit = match rng.below(10) {
            // Shares: what they are worth then, less what they are worth now.
            0..4 => (price - share.price) * quantity,
            // A call pays when the price ends above the strike.
            4..7 => ((price - strike).max(0.0) - call) * quantity,
            // A put pays when it ends below.
            7..9 => {
                let put = call - share.price + strike;
                ((strike - price).max(0.0) - put) * quantity
            }
            // A swap of one share's rise for another's: it pays when this
            // share does better than that one.
            _ => {
                let j = rng.below(SHARES);
                let other = &shares[j];
                let together = share.with_market * other.with_market;
                let apart = (share.volatility.powi(2) + other.volatility.powi(2)
                    - 2.0 * together * share.volatility * other.volatility)
                    .sqrt()
                    * HORIZON.sqrt();
                let cost = fair_call(1.0, 1.0, apart);
                let better =
                    (price * (1.0 / share.price) - prices[j] * (1.0 / other.price)).max(0.0);
                (better - cost) * (quantity * 25.0)
            }
        };
        desks[rng.below(DESKS)].push(g.define(&format!("position{p}"), profit)?);
    }
    let desks: Vec<Node<f64>> = desks
        .into_iter()
        .enumerate()
        .map(|(d, positions)| sum(&mut g, &format!("desk{d}"), positions))
        .collect::<Result<_, _>>()?;
    let firm = sum(&mut g, "firm", desks.clone())?;
    g.stabilize();

    // The firm first, then each desk.
    let vary: Vec<_> = prices.iter().map(|p| p.id()).collect();
    let mut outputs = vec![firm.id()];
    outputs.extend(desks.iter().map(|d| d.id()));
    let plan = Scenarios::new(&g, &vary, &outputs)?;
    println!(
        "portfolio: {POSITIONS} positions on {SHARES} shares, in {DESKS} desks ({} nodes, {} levels)",
        plan.nodes(),
        plan.levels()
    );

    // The futures: each share's price in ten days. A share moves partly with
    // the market and partly on its own, which is what makes a bad day for
    // one tend to be a bad day for many.
    let started = Instant::now();
    let mut inputs = vec![0.0; futures * SHARES];
    inputs
        .par_chunks_mut(SHARES)
        .enumerate()
        .for_each(|(f, future)| {
            let mut rng = Rng((f as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let market = rng.normal();
            for (price, share) in future.iter_mut().zip(&shares) {
                let own = (1.0 - share.with_market * share.with_market).sqrt();
                let moved = share.with_market * market + own * rng.normal();
                let spread = share.volatility * HORIZON.sqrt();
                *price = share.price * (spread * moved - spread * spread / 2.0).exp();
            }
        });
    println!(
        "{futures} possible futures drawn in {}",
        time(started.elapsed())
    );
    println!();

    // Work the portfolio out for every future.
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    #[cfg(feature = "gpu")]
    let results = {
        let gpu = watershed_gpu::Gpu::new()?;
        let allowed = gpu.free_memory()? / 5 * 4;
        let started = Instant::now();
        let (results, timing) = gpu.run_within(&plan, &inputs, allowed)?;
        let took = started.elapsed();
        println!(
            "GPU: {} ({:.0} futures a second; {} at a time)",
            time(took),
            futures as f64 / took.as_secs_f64(),
            timing.scenarios_at_a_time
        );
        // The same on the CPU, for as many as it does in a moment.
        let some = futures.min(2_000);
        let started = Instant::now();
        let on_cpu = plan.run_cpu(&inputs[..some * SHARES]);
        let each = started.elapsed() / some as u32;
        println!(
            "CPU, {cores} cores: {} a future, so about {} for all of them",
            time(each),
            time(each * futures as u32)
        );
        assert!(
            on_cpu
                .iter()
                .zip(&results)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "the CPU and the GPU differ"
        );
        results
    };
    #[cfg(not(feature = "gpu"))]
    let results = {
        let started = Instant::now();
        let results = plan.run_cpu(&inputs);
        let took = started.elapsed();
        println!(
            "CPU, {cores} cores: {} ({:.0} futures a second)",
            time(took),
            futures as f64 / took.as_secs_f64()
        );
        results
    };

    // The graph itself, for a few: it must agree, and this is what each
    // future would cost without a plan.
    let few = futures.min(5);
    let started = Instant::now();
    for (future, result) in inputs
        .chunks(SHARES)
        .zip(results.chunks(outputs.len()))
        .take(few)
    {
        for (&price, &x) in prices.iter().zip(future) {
            g.set(price, x)?;
        }
        g.stabilize();
        assert!(
            g.get(firm)?.to_bits() == result[0].to_bits(),
            "the graph and the plan differ"
        );
    }
    let each = started.elapsed() / few as u32;
    println!(
        "the graph, a future at a time: {} each, so about {} for all of them",
        time(each),
        time(each * futures as u32)
    );
    println!("checked: all of them give the same profit, to the last bit");
    println!();

    // What the futures say.
    let column = |o: usize| -> Vec<f64> {
        let mut sorted: Vec<f64> = results
            .chunks(outputs.len())
            .map(|result| result[o])
            .collect();
        sorted.sort_by(f64::total_cmp);
        sorted
    };
    let profit = column(0);
    let mean = profit.iter().sum::<f64>() / futures as f64;
    println!("the firm's profit over ten days, across {futures} futures");
    println!("  on average            {:>14}", money(mean));
    println!("  best                  {:>14}", money(profit[futures - 1]));
    println!("  worst                 {:>14}", money(profit[0]));
    println!(
        "  loses money in        {:>13.1}%",
        100.0 * profit.partition_point(|&x| x < 0.0) as f64 / futures as f64
    );
    for one_in in [20, 100, 1000] {
        if futures >= 10 * one_in {
            let tail = &profit[..futures / one_in];
            println!(
                "  1 time in {one_in:<5} it loses at least {:>12}, and {:>12} on average when it does",
                money(-tail[tail.len() - 1]),
                money(-tail.iter().sum::<f64>() / tail.len() as f64)
            );
        }
    }
    println!();
    histogram(&profit);
    println!();

    if futures >= 1_000 {
        let mut at_risk: Vec<(usize, f64)> = (0..DESKS)
            .map(|d| (d, -column(1 + d)[futures / 100 - 1]))
            .collect();
        at_risk.sort_by(|a, b| b.1.total_cmp(&a.1));
        println!("the desks with most at risk (the loss reached 1 time in 100)");
        for (d, loss) in at_risk.iter().take(5) {
            println!("  desk{d:<3} {:>14}", money(*loss));
        }
        let alone: f64 = at_risk.iter().map(|(_, loss)| loss).sum();
        println!(
            "  every desk added up: {}. The firm as a whole: {}, less, because the desks' bad days are not all the same day.",
            money(alone),
            money(-profit[futures / 100 - 1])
        );
    }
    Ok(())
}

/// How the futures are spread, from the worst to the best, leaving out the
/// rarest half a percent at each end so the middle can be seen.
fn histogram(sorted: &[f64]) {
    const BARS: usize = 21;
    const WIDTH: usize = 50;
    let n = sorted.len();
    let (low, high) = (sorted[n / 200], sorted[n - 1 - n / 200]);
    if high <= low {
        return;
    }
    let mut counts = [0usize; BARS];
    for &x in sorted {
        if (low..=high).contains(&x) {
            let bar = ((x - low) / (high - low) * BARS as f64) as usize;
            counts[bar.min(BARS - 1)] += 1;
        }
    }
    let most = counts.iter().copied().max().unwrap_or(1).max(1);
    for (bar, &count) in counts.iter().enumerate() {
        let from = low + (high - low) * bar as f64 / BARS as f64;
        println!(
            "  {:>14} {}",
            money(from),
            "#".repeat((count * WIDTH).div_ceil(most))
        );
    }
}

/// An amount with thousands separated: `-1,234,567`.
fn money(x: f64) -> String {
    let digits = format!("{:.0}", x.abs());
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if x < 0.0 && digits != "0" {
        out.insert(0, '-');
    }
    out
}

fn time(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 0.001 {
        format!("{:.1} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.1} ms", s * 1e3)
    } else if s < 120.0 {
        format!("{s:.1} s")
    } else {
        format!("{:.0} minutes", s / 60.0)
    }
}
