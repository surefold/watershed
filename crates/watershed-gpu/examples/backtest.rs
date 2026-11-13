//! Can a simple trading rule beat holding the market? An experiment on real
//! daily prices, set up so that it is hard to fool yourself.
//!
//! The rule, for every share, every day:
//!
//!   - keep two moving averages of the price, one quick and one slow;
//!   - when the quick one is above the slow one the share has been rising:
//!     hold more of it than usual (or, with a negative `gain`, less);
//!   - holding ranges from none of the share to twice the usual amount;
//!   - every change in holding pays a cost.
//!
//! It has four settings: how quick, how slow, the gain, and the cost. With a
//! gain of zero it holds every share equally and never trades, which is "the
//! market" here. The graph is that rule on every share and every day, and
//! each scenario is one choice of settings, so thousands of variants of the
//! rule are tried at once.
//!
//! Trying thousands of variants on one history always finds some that look
//! good, by luck. So:
//!
//!   1. the settings are chosen on the earlier years only, and the chosen
//!      one is then run on the later years, which it has never seen;
//!   2. every trade pays a cost;
//!   3. the comparison is with holding the market, which costs nothing;
//!   4. the same search is run on the same history with its days put in a
//!      random order, where there is nothing to find, many times over. The
//!      share of those on which the best rule looks as good as it does on
//!      the real history is the chance that the real one was luck too.
//!
//! With prices from Stooq (stooq.com/db/h, "U.S." daily, ASCII), unzipped:
//!
//!     cargo run --release --features cuda --example backtest -- /path/to/data
//!
//! then, optionally, how many shares (500), how many shuffled histories (100),
//! and which rule: `trend`, the one above, or `relative`. That one compares
//! each share with the rest: it keeps an average of how much the share has
//! been rising a day and of how much the market has, and holds more of the
//! shares that have been doing better than the market (or, with a negative
//! gain, worse). Its settings are how many days the average looks back over,
//! the gain and the cost. Since for every share ahead of the market another
//! is behind, it holds about the market's amount in all, just spread
//! differently.
//!
//! With no folder, or `made-up` in its place, it makes up prices that
//! wander at random, where no rule can win except by luck.
//!
//! The shares are those with prices for the whole period, so companies that
//! failed along the way are missing. That flatters the market and every rule
//! alike. This is an experiment, not advice.

use std::path::Path;
use std::time::{Duration, Instant};

use watershed_gpu::Scenarios;

#[path = "shared/market.rs"]
mod market;
use market::*;

/// How a rule did over some years.
#[derive(Clone, Copy)]
struct Did {
    /// Profit a year, as a fraction of the money put in.
    a_year: f64,
    /// Profit a year for each unit of how much it swings: the higher, the
    /// steadier the profit. Holding the market is usually between 0.3 and 1.
    steadiness: f64,
}

impl Did {
    fn from(sum: f64, squared: f64, shares: usize, days: usize) -> Did {
        let mean = sum / (shares * days) as f64;
        let variance = squared / (shares * shares * days) as f64 - mean * mean;
        Did {
            a_year: mean * DAYS_A_YEAR,
            steadiness: mean / variance.max(1e-300).sqrt() * DAYS_A_YEAR.sqrt(),
        }
    }
}

struct Search {
    /// For each of the settings: the earlier years, then the later.
    did: Vec<(Did, Did)>,
    run_took: Duration,
}

/// Tries every one of the settings on `history`, with a plan made for a
/// history of its shape.
fn search(
    history: &History,
    rule: Rule,
    settings: &[Settings],
    runner: &Runner,
    plan: &Scenarios,
) -> Result<Search, Box<dyn std::error::Error>> {
    let inputs = inputs_for(rule, settings);
    let started = Instant::now();
    let results = runner.run(plan, &inputs)?;
    let run_took = started.elapsed();

    // The first few again on the CPU: they must be the same to the last bit.
    let few = settings.len().min(3);
    let again = plan.run_cpu(&inputs[..few * 4]);
    assert!(
        again
            .iter()
            .zip(&results)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "the CPU and the {} differ",
        runner.name()
    );

    let shares = history.prices.len();
    let earlier = history.split - WARM_UP - 1;
    let later = history.days() - history.split;
    let did = results
        .chunks(4)
        .map(|r| {
            (
                Did::from(r[0], r[1], shares, earlier),
                Did::from(r[2], r[3], shares, later),
            )
        })
        .collect();
    Ok(Search { did, run_took })
}

/// Of the settings that pay `cost`, the one that did best in the earlier
/// years, and holding the market. Best means steadiest: profit for its swings.
fn best(settings: &[Settings], search: &Search, cost: f64) -> (usize, usize) {
    let paying = || (0..settings.len()).filter(|&k| settings[k].cost == cost);
    let best = paying()
        .max_by(|&a, &b| {
            search.did[a]
                .0
                .steadiness
                .total_cmp(&search.did[b].0.steadiness)
        })
        .expect("some settings");
    let market = paying()
        .find(|&k| settings[k].gain == 0.0)
        .expect("a gain of zero");
    (best, market)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().filter(|dir| dir != "made-up");
    let number = |arg: Option<String>, otherwise: usize| match arg {
        Some(n) => n.parse().expect("the argument is a number"),
        None => otherwise,
    };
    let shares = number(args.next(), 500);
    let shuffles = number(args.next(), 100);
    let rule = match args.next() {
        None => Rule::Trend,
        Some(name) => Rule::named(&name).ok_or(format!("no rule is called {name}"))?,
    };

    let started = Instant::now();
    let history = match &dir {
        Some(dir) => read_history(Path::new(dir), shares)?,
        None => made_up_history(shares),
    };
    println!(
        "prices: {} (read in {})",
        history.what,
        time(started.elapsed())
    );
    let (earlier, later) = (history.split - WARM_UP - 1, history.days() - history.split);
    println!(
        "settings are chosen on {earlier} days ({:.1} years) and tested on the {later} after ({:.1} years)",
        earlier as f64 / DAYS_A_YEAR,
        later as f64 / DAYS_A_YEAR
    );

    let settings = settings_to_try(rule);
    let runner = Runner::new()?;
    let started = Instant::now();
    let setup = Setup::new(&history, rule);
    let mut plan = plan_for(&history, &setup, false)?;
    let plan_took = started.elapsed();
    let real = search(&history, rule, &settings, &runner, &plan)?;
    println!(
        "the rule on every share and day is {} nodes in {} levels; {} settings tried on the {} in {} (plan made in {})",
        plan.nodes(),
        plan.levels(),
        settings.len(),
        runner.name(),
        time(real.run_took),
        time(plan_took)
    );

    // Holding the market must be what the prices say it is.
    let (pick, market) = best(&settings, &real, COST);
    let by_hand: f64 = (WARM_UP + 1..history.split)
        .flat_map(|day| (0..history.prices.len()).map(move |share| (share, day)))
        .map(|(share, day)| history.rise(share, day))
        .sum::<f64>()
        / (history.prices.len() * earlier) as f64
        * DAYS_A_YEAR;
    assert!(
        (by_hand - real.did[market].0.a_year).abs() < 1e-9,
        "holding the market: {by_hand} by hand, {} from the plan",
        real.did[market].0.a_year
    );
    println!();

    let show = |label: &str, k: usize| {
        let (chosen_on, tested_on) = real.did[k];
        println!(
            "  {label:<34} {:>7.1}% {:>8.2}   {:>7.1}% {:>8.2}",
            100.0 * chosen_on.a_year,
            chosen_on.steadiness,
            100.0 * tested_on.a_year,
            tested_on.steadiness
        );
    };
    println!(
        "  {:<34} {:>17}   {:>17}",
        "", "chosen on (earlier)", "tested on (later)"
    );
    println!(
        "  {:<34} {:>8} {:>8}   {:>8} {:>8}",
        "", "a year", "steady", "a year", "steady"
    );
    show("holding the market", market);
    show("best rule, paying to trade", pick);
    let (free_pick, _) = best(&settings, &real, 0.0);
    show("best rule, if trading were free", free_pick);
    println!();
    let describe = |label: &str, k: usize| {
        println!("{label}: {}", market::describe(rule, &settings[k]));
    };
    describe("the best rule paying to trade", pick);
    describe("the best rule if trading were free", free_pick);

    // Did being better on the earlier years say anything about the later?
    let paying: Vec<usize> = (0..settings.len())
        .filter(|&k| settings[k].cost == COST && settings[k].gain != 0.0)
        .collect();
    let beat = |k: usize, later: bool| {
        let (a, b) = (real.did[k], real.did[market]);
        match later {
            false => a.0.steadiness > b.0.steadiness,
            true => a.1.steadiness > b.1.steadiness,
        }
    };
    let beat_earlier: Vec<usize> = paying.iter().copied().filter(|&k| beat(k, false)).collect();
    let beat_both = beat_earlier.iter().filter(|&&k| beat(k, true)).count();
    let beat_later = paying.iter().filter(|&&k| beat(k, true)).count();
    println!(
        "of {} rules paying to trade, {} beat the market on the earlier years; {} of those beat it again on the later years ({} of all {} did)",
        paying.len(),
        beat_earlier.len(),
        beat_both,
        beat_later,
        paying.len()
    );
    println!();

    // The same search where there is nothing to find.
    let edge = |search: &Search, cost: f64| {
        let (pick, market) = best(&settings, search, cost);
        (
            search.did[pick].0.steadiness - search.did[market].0.steadiness,
            search.did[pick].1.steadiness - search.did[market].1.steadiness,
        )
    };
    // Each shuffled history has the same shape, so the plan is kept and
    // only its prices are replaced.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut by_luck = Vec::new();
    let started = Instant::now();
    for _ in 0..shuffles {
        let history = shuffled(&history, &mut rng);
        plan.set_shared(&data_for(&history, &setup));
        let search = search(&history, rule, &settings, &runner, &plan)?;
        by_luck.push([edge(&search, COST), edge(&search, 0.0)]);
    }
    if shuffles > 0 {
        println!(
            "the same search on {shuffles} histories with the days in a random order: {} ({} each)",
            time(started.elapsed()),
            time(started.elapsed() / shuffles as u32)
        );
        println!();
    }
    for (which, cost) in [(0, COST), (1, 0.0)] {
        let paying = ["paying to trade", "if trading were free"][which];
        let (real_earlier, real_later) = edge(&real, cost);
        println!("how much steadier than the market the best rule was, {paying}");
        println!("  {:<46} {:>10} {:>10}", "", "chosen on", "tested on");
        println!(
            "  {:<46} {:>+10.3} {:>+10.3}",
            "the history as it happened", real_earlier, real_later
        );
        if shuffles > 0 {
            // Where the real history stands among the shuffled ones.
            let column = |later: bool| {
                let mut all: Vec<f64> = by_luck
                    .iter()
                    .map(|luck| if later { luck[which].1 } else { luck[which].0 })
                    .collect();
                all.sort_by(f64::total_cmp);
                all
            };
            let (earlier, later) = (column(false), column(true));
            for (label, part) in [("the middle one", 0.5), ("19 in 20 are below", 0.95)] {
                let k = ((shuffles as f64 * part) as usize).min(shuffles - 1);
                println!(
                    "  {:<46} {:>+10.3} {:>+10.3}",
                    format!("days in a random order: {label}"),
                    earlier[k],
                    later[k]
                );
            }
            println!(
                "  {:<46} {:>+10.3} {:>+10.3}",
                "days in a random order: the best",
                earlier[shuffles - 1],
                later[shuffles - 1]
            );
            // The chance of doing at least this well with nothing to find:
            // the share of all the histories, this one among them, that did.
            let chance = |all: &[f64], real: f64| {
                (1 + all.iter().filter(|&&luck| luck >= real).count()) as f64
                    / (shuffles + 1) as f64
            };
            println!(
                "  {:<46} {:>9.1}% {:>9.1}%",
                "chance of this with nothing to find",
                100.0 * chance(&earlier, real_earlier),
                100.0 * chance(&later, real_later)
            );
        }
        println!();
    }
    Ok(())
}
