//! How much capital must an insurer hold against its guarantees?
//!
//! The rule (Solvency II's, and others like it) is: enough to survive the
//! worst year in two hundred. To know what the book would be worth after a
//! bad year, the insurer has to value it again as it would stand then, and
//! valuing it once already takes thousands of market paths (see the
//! `liabilities` example). So the honest calculation is a run inside a run:
//!
//!   for each of a thousand ways the next year could go (outer scenarios)
//!     → value the whole book over a thousand market paths from there (inner)
//!
//! A million valuations of the whole book. This is "nested stochastic". A
//! Milliman paper says of it: "This brute force approach is too
//! time-consuming to be used in a real insurance business case" (Boumezoued,
//! Cherchali and Truc, "Efficient computation of Solvency Capital
//! Requirement using multilevel Monte Carlo methods", 2022). The capital is
//! typically worked out by the standard formula instead, which adds up
//! prescribed stresses, or by fitting a curve through far fewer valuations
//! and reading the bad year off the curve.
//!
//! Here it is done outright. An outer scenario is a fall or rise in the
//! funds and a move in interest rates, which are shared inputs of the plan,
//! so each one is a `set_shared` and a run of the inner paths.
//!
//!     cargo run --release --features cuda --example capital -- /path/to/export
//!
//! then optionally how many kinds of policy (4,000), outer scenarios (1,000)
//! and inner paths (1,000). The folder is the one the `liabilities` example
//! uses.
//!
//! The year is taken as a shock to today's book: the funds and the rates
//! move, and the book is valued as it stands today. A year's ageing,
//! premiums and claims are left out, as the prescribed stresses leave them
//! out too.

use std::path::Path;
use std::time::{Duration, Instant};

#[path = "shared/savings.rs"]
mod savings;
use savings::*;

/// The funds over a year, as the world is expected to go and not as prices
/// imply: growth of 5% with swings of 15%.
const YEAR_GROWTH: f64 = 0.05;
const YEAR_SWING: f64 = 0.15;
/// How far interest rates move in a year: 0.7 of a point, typically.
const YEAR_RATES: f64 = 0.007;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .ok_or("give the folder the reference wrote its tables and policies to")?;
    let dir = Path::new(&dir);
    let number = |arg: Option<String>, otherwise: usize| match arg {
        Some(n) => n.replace('_', "").parse().expect("a number"),
        None => otherwise,
    };
    let size = number(args.next(), 4_000);
    let outer = number(args.next(), 1_000);
    let inner = number(args.next(), 1_000);

    let tables = tables(dir)?;
    let runner = Runner::new()?;
    let best = Assumptions::best_estimate(&tables, 0.0);
    let book_policies = policies(&dir.join("points_book.csv"), size)?;
    let started = Instant::now();
    let mut book = Book::new(&book_policies, &tables, &best)?;
    let totals = book.totals()?;
    let mut plan = book.plan(&[totals[NET].id()])?;
    println!(
        "{} kinds of policy ({} policies), {} graph nodes (built and planned in {})",
        commas(book_policies.len() as f64),
        commas(book_policies.iter().map(|p| p.count).sum()),
        commas(plan.nodes() as f64),
        time(started.elapsed())
    );

    // The inner paths: the same draws from every outer scenario, so the
    // difference between two scenarios is the scenarios' and not the draws'.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let draws: Vec<f64> = (0..inner * MONTHS).map(|_| rng.normal()).collect();
    let paths_for = |growth: f64| -> Vec<f64> {
        let (drift, spread) = (
            (growth - 0.5 * SWING * SWING) / 12.0,
            SWING * (1.0f64 / 12.0).sqrt(),
        );
        draws
            .iter()
            .map(|z| (drift + spread * z).exp() - 1.0)
            .collect()
    };
    let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
    // On the GPU the plan is loaded once and stays there: each year changes
    // the paths and a few shared numbers, and nothing else is copied.
    #[cfg(feature = "gpu")]
    let mut loaded = runner.gpu.load(&plan, &paths_for(GROWTH))?;
    // What the book is worth after the funds are multiplied by `funds` and
    // rates move by `rates`: on every inner path, and on the first tenth.
    let mut worth = |funds: f64, rates: f64| -> Result<(f64, f64)> {
        let mut a = Assumptions::best_estimate(&tables, rates);
        a.fund = funds;
        plan.set_shared(&a.values());
        let paths = paths_for(GROWTH + rates);
        #[cfg(feature = "gpu")]
        let results = {
            loaded.set_inputs(&plan, &paths)?;
            loaded.run(&plan)?
        };
        #[cfg(not(feature = "gpu"))]
        let results = runner.run(&plan, &paths)?;
        Ok((mean(&results), mean(&results[..(inner / 10).max(1)])))
    };

    let (today, today_tenth) = worth(1.0, 0.0)?;
    println!("the book is worth {} today", commas(today));
    println!();

    // The outer scenarios.
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let years: Vec<(f64, f64)> = (0..outer)
        .map(|_| {
            let funds =
                (YEAR_GROWTH - 0.5 * YEAR_SWING * YEAR_SWING + YEAR_SWING * rng.normal()).exp();
            (funds, YEAR_RATES * rng.normal())
        })
        .collect();
    let started = Instant::now();
    let mut losses = Vec::with_capacity(outer);
    let mut losses_tenth = Vec::with_capacity(outer);
    for &(funds, rates) in &years {
        let (all, tenth) = worth(funds, rates)?;
        losses.push(today - all);
        losses_tenth.push(today_tenth - tenth);
    }
    let took = started.elapsed();
    let valuations = outer as f64 * inner as f64;
    let projected = valuations * book.policy_months as f64;
    println!(
        "{}: {} years, each valued over {} paths: {} valuations of the whole book in {} ({} months of policy a second)",
        runner.name(),
        commas(outer as f64),
        commas(inner as f64),
        commas(valuations),
        time(took),
        commas(projected / took.as_secs_f64())
    );
    // What the reference would take, from the time it took for its part of
    // this book (see the `liabilities` example).
    let timing = std::fs::read_to_string(dir.join("timings.csv"))?;
    let line: Vec<&str> = timing
        .lines()
        .find(|l| l.starts_with("book_dynamic"))
        .ok_or("no timing for the reference")?
        .split(',')
        .collect();
    let (points, paths, seconds): (f64, f64, f64) =
        (line[1].parse()?, line[2].parse()?, line[3].parse()?);
    let reference = seconds * (book_policies.len() as f64 / points) * (valuations / paths);
    println!(
        "the reference (Python, one core) would take about {} at the speed it ran its part",
        time(Duration::from_secs_f64(reference))
    );
    println!();

    // The worst year in two hundred.
    let in_200 = |losses: &[f64]| {
        let mut sorted = losses.to_vec();
        sorted.sort_by(f64::total_cmp);
        sorted[((sorted.len() as f64 * 0.995) as usize).min(sorted.len() - 1)]
    };
    let capital = in_200(&losses);
    println!("what the book loses over the year, across the {outer} years");
    let mut sorted = losses.clone();
    sorted.sort_by(f64::total_cmp);
    let at = |share: f64| sorted[((outer as f64 * share) as usize).min(outer - 1)];
    println!("  in the middle year          {:>16}", commas(at(0.5)));
    println!("  1 year in 10 is worse than  {:>16}", commas(at(0.9)));
    println!("  1 year in 20 is worse than  {:>16}", commas(at(0.95)));
    println!(
        "  1 year in 200 is worse than {:>16}   <- the capital to hold",
        commas(capital)
    );
    histogram(&sorted);
    println!();

    // The shortcuts.
    println!("against the shortcuts");
    let (funds_down, _) = worth(0.61, 0.0)?;
    let (rates_down, _) = worth(1.0, -0.01)?;
    let (rates_up, _) = worth(1.0, 0.01)?;
    let equity = today - funds_down;
    let rates = (today - rates_down).max(today - rates_up).max(0.0);
    // Added up as the standard formula adds market risks that are half
    // correlated.
    let formula = (equity * equity + rates * rates + equity * rates).sqrt();
    println!(
        "  {:<58} {:>16}",
        "the run inside a run, as above",
        commas(capital)
    );
    println!(
        "  {:<58} {:>16}",
        format!(
            "the same with {} inner paths, not {}",
            (inner / 10).max(1),
            inner
        ),
        commas(in_200(&losses_tenth))
    );
    println!(
        "  {:<58} {:>16}",
        "two prescribed stresses added by formula",
        commas(formula)
    );
    println!(
        "    (funds fall 39%: {}; rates move a point: {})",
        commas(equity),
        commas(rates)
    );
    // Which years were the bad ones.
    let mut worst: Vec<usize> = (0..outer).collect();
    worst.sort_by(|&a, &b| losses[b].total_cmp(&losses[a]));
    println!();
    println!("the five worst years");
    for &k in worst.iter().take(5) {
        println!(
            "  funds {:>+6.1}%, rates {:>+5.2} points: loses {}",
            100.0 * (years[k].0 - 1.0),
            100.0 * years[k].1,
            commas(losses[k])
        );
    }
    Ok(())
}

/// How the losses are spread, leaving out the rarest half a percent at each
/// end so the middle can be seen.
fn histogram(sorted: &[f64]) {
    const BARS: usize = 15;
    const WIDTH: usize = 48;
    let n = sorted.len();
    let (low, high) = (sorted[n / 200], sorted[n - 1 - n / 200]);
    if high <= low {
        return;
    }
    let mut counts = [0usize; BARS];
    for &x in sorted {
        if (low..=high).contains(&x) {
            counts[(((x - low) / (high - low) * BARS as f64) as usize).min(BARS - 1)] += 1;
        }
    }
    let most = counts.iter().copied().max().unwrap_or(1).max(1);
    for (bar, &count) in counts.iter().enumerate() {
        let from = low + (high - low) * bar as f64 / BARS as f64;
        println!(
            "  {:>16} {}",
            commas(from),
            "#".repeat((count * WIDTH).div_ceil(most))
        );
    }
}
