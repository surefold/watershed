//! What if it happened again? A portfolio and its rules, put through every
//! stretch of fifty years of real share prices.
//!
//! "What would a crash do to us" is usually answered with one or two
//! remembered crashes and a guess. The prices of every day since the 1970s
//! are there to be used instead: take the portfolio as it is today, with
//! the rules it is run by, and put it through every sixty days that have
//! ever happened, one stretch starting each day. Thirteen thousand
//! stretches, each of them real.
//!
//! For a portfolio that is simply held, that is arithmetic. This one is
//! run by rules, and rules make it depend on the path: a holding is sold
//! when it falls a set distance below its highest value since the start,
//! and the portfolio may be bought partly with borrowed money, on which
//! interest is paid. What the rules are worth cannot be read off the
//! prices; each has to be played through each stretch, day by day.
//!
//! The graph is the portfolio for those sixty days: every holding's value,
//! its high so far and whether it is still held, each day, and the cash,
//! the debt and what the owner has left. A scenario is one stretch of
//! history: every share's rise on every one of its days. The stop and the
//! borrowing are shared inputs, so each choice of rules is one more run of
//! every stretch.
//!
//! First fetch the prices, which are not part of the repository:
//!
//!     python3 examples/shared/prices.py prices
//!
//! then
//!
//!     cargo run --release --features cuda --example replay -- prices
//!
//! optionally followed by the year to start from (1973) and the days in a
//! stretch (60). Or `made-up` in place of the folder, for prices that
//! wander at random with a crash now and then.
//!
//! The portfolio is an equal amount of every share that has a price from
//! the start year to now. Those are today's members of the S&P 500 that
//! were already listed then: the survivors. Every company that failed on
//! the way is missing, so every figure here is kinder than the truth, and
//! more so the earlier the year. A holding is sold at the day's closing
//! price, with no cost and no gap; real stops do worse.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use watershed_core::prelude::*;
use watershed_core::{NodeId, Value, when};
use watershed_gpu::Scenarios;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What is borrowed costs this much a year.
const INTEREST: f64 = 0.05;
/// How many stretches are run at once.
const AT_ONCE: usize = 2_000;
/// The falls from its high at which a holding is sold (1 is never), and the
/// borrowing as a share of the owner's money, that are tried.
const STOPS: [f64; 4] = [1.0, 0.25, 0.15, 0.10];
const BORROWED: [f64; 3] = [0.0, 0.5, 1.0];
/// Times people remember, to be found among the stretches.
const REMEMBERED: [(&str, u32); 6] = [
    ("the summer of 1974", 19740701),
    ("October 1987", 19871001),
    ("the summer of 2002", 20020603),
    ("the autumn of 2008", 20080915),
    ("February 2020", 20200219),
    ("the spring of 2022", 20220401),
];

/// Every share's price on every day the market was open.
struct History {
    /// Days as one number: year, month, day.
    days: Vec<u32>,
    names: Vec<String>,
    /// `prices[share][day]`.
    prices: Vec<Vec<f64>>,
}

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }

    fn normal(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
}

/// Prices that wander at random, all moved by one market, with a crash now
/// and then: for trying the example with no data.
fn made_up() -> History {
    let (shares, days) = (40, 6_000);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut prices = vec![Vec::with_capacity(days); shares];
    let mut now = vec![50.0f64; shares];
    for day in 0..days {
        let crash = if rng.unit() < 0.004 {
            -0.06 * (1.0 + rng.unit())
        } else {
            0.0
        };
        let market = 0.0003 + 0.008 * rng.normal() + crash;
        for (price, series) in now.iter_mut().zip(&mut prices) {
            if day > 0 {
                *price *= 1.0 + market + 0.012 * rng.normal();
            }
            series.push(*price);
        }
    }
    History {
        // Days numbered from 1, as there are no dates.
        days: (1..=days as u32).collect(),
        names: (1..=shares).map(|k| format!("S{k:02}")).collect(),
        prices,
    }
}

/// Reads a folder of `<TICKER>.csv` files of `Date,Open,High,Low,Close,Volume`
/// and keeps the shares that have a price from the first days of `from`.
fn read(folder: &Path, from: u32) -> Result<History> {
    let mut all: Vec<(String, Vec<(u32, f64)>)> = Vec::new();
    for entry in std::fs::read_dir(folder)? {
        let path = entry?.path();
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        // A name starting with `_` is an index, not a share.
        if path.extension().and_then(|e| e.to_str()) != Some("csv") || name.starts_with('_') {
            continue;
        }
        let mut rows = Vec::new();
        for line in std::fs::read_to_string(&path)?.lines().skip(1) {
            let cells: Vec<&str> = line.split(',').collect();
            let (Some(day), Some(close)) = (cells.first(), cells.get(4)) else {
                continue;
            };
            if let (Ok(day), Ok(close)) =
                (day.replace('-', "").parse::<u32>(), close.parse::<f64>())
            {
                if day >= from * 10_000 && close > 0.0 {
                    rows.push((day, close));
                }
            }
        }
        rows.sort_by_key(|row| row.0);
        all.push((name, rows));
    }
    // A share is kept if it has a price in the first fortnight of the year,
    // and no day's move so wild that it must be a mistake in the data.
    let starts_by = from * 10_000 + 115;
    all.retain(|(_, rows)| {
        rows.first().is_some_and(|row| row.0 <= starts_by)
            && rows
                .windows(2)
                .all(|w| w[1].1 < 3.0 * w[0].1 && 3.0 * w[1].1 > w[0].1)
    });
    all.sort_by(|a, b| a.0.cmp(&b.0));
    if all.len() < 2 {
        return Err(format!(
            "{}: fewer than two shares have prices from the start of {from}",
            folder.display()
        )
        .into());
    }
    // The days the market was open: those most of the shares have a price for.
    let mut seen: HashMap<u32, usize> = HashMap::new();
    for (_, rows) in &all {
        for row in rows {
            *seen.entry(row.0).or_default() += 1;
        }
    }
    let mut days: Vec<u32> = seen
        .into_iter()
        .filter(|&(_, count)| 2 * count >= all.len())
        .map(|(day, _)| day)
        .collect();
    days.sort_unstable();
    // Each market day's price, or the one before where a day is missing.
    let prices = all
        .iter()
        .map(|(_, rows)| {
            let mut at = 0;
            days.iter()
                .map(|&day| {
                    while at + 1 < rows.len() && rows[at + 1].0 <= day {
                        at += 1;
                    }
                    rows[at].1
                })
                .collect()
        })
        .collect();
    Ok(History {
        days,
        names: all.into_iter().map(|(name, _)| name).collect(),
        prices,
    })
}

/// One node adding up `terms`: eight at a time, then those eight at a time.
fn sum(
    g: &mut Graph,
    name: &str,
    terms: Vec<Expr<f64>>,
) -> std::result::Result<Node<f64>, CalcError> {
    let mut nodes = Vec::with_capacity(terms.len() / 8 + 1);
    for (k, group) in terms.chunks(8).enumerate() {
        let mut all = group[0].clone();
        for term in &group[1..] {
            all = all + term.clone();
        }
        nodes.push(g.define(&format!("{name}_0_{k}"), all)?);
    }
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

/// The portfolio and its rules for a stretch of days, as a graph.
struct Portfolio {
    g: Graph,
    /// Every share's rise on every day: day by day, share by share.
    rises: Vec<NodeId>,
    stop: Input<f64>,
    borrowed: Input<f64>,
    /// What the owner has at the end, the least they had on the way, and
    /// how many holdings were sold; all for one unit of the owner's money.
    answers: Vec<NodeId>,
}

fn portfolio(shares: usize, days: usize) -> std::result::Result<Portfolio, CalcError> {
    let mut g = Graph::new();
    let stop = g.input("stop", 1.0)?;
    let borrowed = g.input("borrowed", 0.0)?;
    // An equal amount of each share, bought with the owner's money and
    // what is borrowed.
    let start = g.define("start", (1.0 + borrowed) / shares as f64)?;
    let mut rises = Vec::with_capacity(shares * days);
    let mut value: Vec<Expr<f64>> = vec![start.into_expr(); shares];
    let mut high = value.clone();
    let mut held: Vec<Expr<f64>> = vec![1.0.into_expr(); shares];
    let mut cash: Expr<f64> = 0.0.into_expr();
    let mut least: Expr<f64> = 1.0.into_expr();
    let mut left = least.clone();
    for day in 0..days {
        let (mut kept, mut sold) = (Vec::with_capacity(shares), Vec::with_capacity(shares));
        for share in 0..shares {
            let rise = g.input(&format!("rise_{day}_{share}"), 0.0)?;
            rises.push(rise.id());
            let now = g.define(
                &format!("value_{day}_{share}"),
                value[share].clone() * (1.0 + rise),
            )?;
            let top = g.define(&format!("high_{day}_{share}"), high[share].clone().max(now))?;
            // Sold at today's close once it is the stop's distance below
            // its high, and never bought back.
            let still = g.define(
                &format!("held_{day}_{share}"),
                held[share].clone() * when(now.gt((1.0 - stop) * top)).then(1.0).otherwise(0.0),
            )?;
            kept.push(still * now);
            sold.push((held[share].clone() - still) * now);
            (value[share], high[share], held[share]) =
                (now.into_expr(), top.into_expr(), still.into_expr());
        }
        let in_shares = sum(&mut g, &format!("shares_{day}"), kept)?;
        let proceeds = sum(&mut g, &format!("sold_{day}"), sold)?;
        let in_cash = g.define(&format!("cash_{day}"), cash + proceeds)?;
        cash = in_cash.into_expr();
        // What was borrowed, with interest to this day.
        let owed = borrowed * (1.0 + INTEREST / 252.0).powi(day as i32 + 1);
        let has = g.define(&format!("left_{day}"), in_shares + in_cash - owed)?;
        let low = g.define(&format!("least_{day}"), least.min(has))?;
        (left, least) = (has.into_expr(), low.into_expr());
    }
    let end = g.define("end", left)?;
    let lowest = g.define("lowest", least)?;
    let gone = sum(
        &mut g,
        "gone",
        held.iter().map(|h| 1.0 - h.clone()).collect(),
    )?;
    g.stabilize();
    Ok(Portfolio {
        g,
        rises,
        stop,
        borrowed,
        answers: vec![end.id(), lowest.id(), gone.id()],
    })
}

/// The same sums written out by hand, for one stretch.
fn by_hand(rises: &[f64], shares: usize, stop: f64, borrowed: f64) -> [f64; 3] {
    let start = (1.0 + borrowed) / shares as f64;
    let (mut value, mut high, mut held) =
        (vec![start; shares], vec![start; shares], vec![true; shares]);
    let (mut cash, mut least, mut left) = (0.0, 1.0f64, 1.0);
    for (day, today) in rises.chunks(shares).enumerate() {
        let mut in_shares = 0.0;
        for share in 0..shares {
            value[share] *= 1.0 + today[share];
            high[share] = high[share].max(value[share]);
            let still = held[share] && value[share] > (1.0 - stop) * high[share];
            if still {
                in_shares += value[share];
            } else if held[share] {
                cash += value[share];
            }
            held[share] = still;
        }
        left = in_shares + cash - borrowed * (1.0 + INTEREST / 252.0).powi(day as i32 + 1);
        least = least.min(left);
    }
    [left, least, held.iter().filter(|&&h| !h).count() as f64]
}

/// A plan, on the GPU when there is one.
struct Runner {
    #[cfg(feature = "gpu")]
    gpu: watershed_gpu::Gpu,
}

impl Runner {
    fn new() -> Result<Runner> {
        Ok(Runner {
            #[cfg(feature = "gpu")]
            gpu: watershed_gpu::Gpu::new()?,
        })
    }

    fn run(&self, plan: &Scenarios, inputs: &[f64]) -> Result<Vec<f64>> {
        #[cfg(feature = "gpu")]
        return Ok(self.gpu.run(plan, inputs)?);
        #[cfg(not(feature = "gpu"))]
        Ok(plan.run_cpu(inputs))
    }
}

/// Every share's rise on each day of the stretch starting at `from`.
fn stretch(history: &History, from: usize, days: usize, out: &mut Vec<f64>) {
    for day in from..from + days {
        out.extend(history.prices.iter().map(|p| p[day + 1] / p[day] - 1.0));
    }
}

/// Every stretch of the history under one choice of rules: for each, what
/// the owner has at the end, the least on the way, and the holdings sold.
fn every_stretch(
    runner: &Runner,
    portfolio: &mut Portfolio,
    history: &History,
    days: usize,
    stop: f64,
    borrowed: f64,
) -> Result<Vec<[f64; 3]>> {
    portfolio.g.set(portfolio.stop, stop)?;
    portfolio.g.set(portfolio.borrowed, borrowed)?;
    portfolio.g.stabilize();
    let plan = Scenarios::new(&portfolio.g, &portfolio.rises, &portfolio.answers)?;
    let stretches = history.days.len() - days;
    let mut all = Vec::with_capacity(stretches);
    let mut rises = Vec::with_capacity(AT_ONCE * portfolio.rises.len());
    for from in (0..stretches).step_by(AT_ONCE) {
        rises.clear();
        for start in from..(from + AT_ONCE).min(stretches) {
            stretch(history, start, days, &mut rises);
        }
        let got = runner.run(&plan, &rises)?;
        all.extend(got.chunks(3).map(|a| [a[0], a[1], a[2]]));
    }
    Ok(all)
}

/// A day as people write it.
fn date(day: u32) -> String {
    match day > 10_000_000 {
        true => format!("{}-{:02}-{:02}", day / 10_000, day / 100 % 100, day % 100),
        false => format!("day {day}"),
    }
}

/// A gain or loss as a percentage of the owner's money.
fn change(left: f64) -> String {
    format!("{:+.1}%", 100.0 * (left - 1.0))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let folder = args
        .next()
        .ok_or("give the folder of prices that shared/prices.py wrote, or `made-up`")?;
    let from: u32 = args.next().map_or(Ok(1973), |a| a.parse())?;
    let days: usize = args.next().map_or(Ok(60), |a| a.parse())?;
    let runner = Runner::new()?;

    // ---- The history ----
    println!("1. The history");
    let started = Instant::now();
    let history = match folder.as_str() {
        "made-up" => made_up(),
        _ => read(Path::new(&folder), from)?,
    };
    let shares = history.names.len();
    let stretches = history.days.len() - days;
    println!(
        "   {shares} shares with a price on each of {} days, {} to {} (read in {})",
        commas(history.days.len() as f64),
        date(history.days[0]),
        date(history.days[history.days.len() - 1]),
        time(started.elapsed())
    );
    println!(
        "   {} stretches of {days} days, one starting each day",
        commas(stretches as f64)
    );
    let started = Instant::now();
    let mut portfolio = portfolio(shares, days)?;
    println!(
        "   the portfolio for {days} days: an equal amount of each share, {} nodes (built in {})",
        commas(portfolio.g.len() as f64),
        time(started.elapsed())
    );
    println!();

    // ---- Held and left alone ----
    println!("2. Held and left alone");
    let started = Instant::now();
    let held = every_stretch(&runner, &mut portfolio, &history, days, 1.0, 0.0)?;
    let took = started.elapsed();
    println!(
        "   every stretch in {} ({} a second)",
        time(took),
        commas(stretches as f64 / took.as_secs_f64())
    );
    let mut ends: Vec<f64> = held.iter().map(|a| a[0]).collect();
    ends.sort_by(f64::total_cmp);
    let at = |sorted: &[f64], share: f64| {
        sorted[((sorted.len() as f64 * share) as usize).min(sorted.len() - 1)]
    };
    println!(
        "   after {days} days: half the time better than {}, 1 time in 20 worse than {}, 1 in 100 worse than {}",
        change(at(&ends, 0.5)),
        change(at(&ends, 0.05)),
        change(at(&ends, 0.01))
    );
    println!(
        "   down at the end {:.0}% of the time",
        100.0 * ends.partition_point(|&e| e < 1.0) as f64 / stretches as f64
    );
    // The worst stretches, none overlapping another.
    let mut order: Vec<usize> = (0..stretches).collect();
    order.sort_by(|&a, &b| held[a][0].total_cmp(&held[b][0]));
    let mut worst: Vec<usize> = Vec::new();
    for &start in &order {
        if worst.iter().all(|&w| start.abs_diff(w) >= days) {
            worst.push(start);
            if worst.len() == 5 {
                break;
            }
        }
    }
    println!("   the worst that has happened, stretches that do not overlap:");
    for &start in &worst {
        println!(
            "     from {}: {} at the end, {} at its lowest",
            date(history.days[start]),
            change(held[start][0]),
            change(held[start][1])
        );
    }
    println!();

    // ---- The rules ----
    println!("3. The rules, each through every stretch");
    let started = Instant::now();
    let mut tried = Vec::new();
    for borrowed in BORROWED {
        for stop in STOPS {
            let answers = match (stop, borrowed) {
                (1.0, 0.0) => held.clone(),
                _ => every_stretch(&runner, &mut portfolio, &history, days, stop, borrowed)?,
            };
            tried.push((stop, borrowed, answers));
        }
    }
    let runs = (tried.len() - 1) * stretches;
    println!(
        "   {} more choices of rules: {} stretches played through in {}",
        tried.len() - 1,
        commas(runs as f64),
        time(started.elapsed())
    );
    println!(
        "   {:<12} {:<14} {:>12} {:>14} {:>12} {:>18} {:>14}",
        "borrowed",
        "sold after",
        "half above",
        "1 in 20 below",
        "worst end",
        "lowest on the way",
        "holdings sold"
    );
    for (stop, borrowed, answers) in &tried {
        let mut ends: Vec<f64> = answers.iter().map(|a| a[0]).collect();
        ends.sort_by(f64::total_cmp);
        let lowest = answers.iter().map(|a| a[1]).fold(f64::INFINITY, f64::min);
        let sold = answers.iter().map(|a| a[2]).sum::<f64>() / stretches as f64;
        println!(
            "   {:<12} {:<14} {:>12} {:>14} {:>12} {:>18} {:>14.1}",
            match *borrowed {
                0.0 => "nothing".to_string(),
                b => format!("{:.0}% more", 100.0 * b),
            },
            match *stop {
                1.0 => "never".to_string(),
                s => format!("a {:.0}% fall", 100.0 * s),
            },
            change(at(&ends, 0.5)),
            change(at(&ends, 0.05)),
            change(ends[0]),
            change(lowest),
            sold
        );
    }
    println!();

    // ---- When it mattered ----
    println!("4. When it mattered");
    let bad: Vec<usize> = (0..stretches).filter(|&s| held[s][0] < 0.9).collect();
    println!(
        "   the stretches in which the portfolio, left alone, lost more than a tenth: {} of {} ({:.1}%)",
        commas(bad.len() as f64),
        commas(stretches as f64),
        100.0 * bad.len() as f64 / stretches as f64
    );
    println!("   what selling after a fall did, with nothing borrowed, on average:");
    println!(
        "     {:<14} {:>16} {:>16}",
        "sold after", "in those", "in all the rest"
    );
    for (stop, _, answers) in tried.iter().filter(|t| t.1 == 0.0) {
        let mean = |which: &dyn Fn(usize) -> bool| {
            let these: Vec<f64> = (0..stretches)
                .filter(|&s| which(s))
                .map(|s| answers[s][0])
                .collect();
            these.iter().sum::<f64>() / these.len().max(1) as f64
        };
        println!(
            "     {:<14} {:>16} {:>16}",
            match *stop {
                1.0 => "never".to_string(),
                s => format!("a {:.0}% fall", 100.0 * s),
            },
            change(mean(&|s| held[s][0] < 0.9)),
            change(mean(&|s| held[s][0] >= 0.9))
        );
    }
    println!();

    // ---- If it happened again ----
    println!("5. If it happened again");
    let (stop, borrowed) = (0.15, 0.5);
    let ruled = &tried
        .iter()
        .find(|t| t.0 == stop && t.1 == borrowed)
        .expect("tried")
        .2;
    println!(
        "   the {days} days from each, left alone, and with half as much again borrowed and holdings sold after a 15% fall:"
    );
    let mut found = 0;
    for (what, day) in REMEMBERED {
        let start = history.days.partition_point(|&d| d < day);
        if start == 0 || start >= stretches {
            continue;
        }
        found += 1;
        println!(
            "     {what:<22} {:>8} left alone, {:>8} with the rules ({:.0} holdings sold, {} at its lowest)",
            change(held[start][0]),
            change(ruled[start][0]),
            ruled[start][2],
            change(ruled[start][1])
        );
    }
    if found == 0 {
        println!("     (none of the remembered times is in this history)");
    }
    println!();

    // ---- Checks ----
    println!("6. Checks");
    // The sums by hand for every stretch under one choice of rules, and the
    // graph itself, a stretch at a time, for a few.
    let mut rises = Vec::with_capacity(shares * days);
    let mut furthest = 0.0f64;
    for (start, answer) in ruled.iter().enumerate() {
        rises.clear();
        stretch(&history, start, days, &mut rises);
        let hand = by_hand(&rises, shares, stop, borrowed);
        for (a, b) in answer.iter().zip(hand) {
            furthest = furthest.max((a - b).abs());
        }
    }
    println!("   the same sums by hand for every stretch: the furthest apart is {furthest:.0e}");
    assert!(furthest < 1e-9, "the plan and the sums by hand differ");
    portfolio.g.set(portfolio.stop, stop)?;
    portfolio.g.set(portfolio.borrowed, borrowed)?;
    let few = stretches.min(20);
    for start in (0..stretches).step_by((stretches / few).max(1)).take(few) {
        rises.clear();
        stretch(&history, start, days, &mut rises);
        for (&id, &rise) in portfolio.rises.iter().zip(&rises) {
            portfolio.g.set_value(id, Value::F64(rise))?;
        }
        portfolio.g.stabilize();
        for (&id, &x) in portfolio.answers.iter().zip(&ruled[start]) {
            let Value::F64(here) = portfolio.g.value(id) else {
                unreachable!("an answer is a number")
            };
            assert!(
                here.to_bits() == x.to_bits(),
                "the graph and the plan differ"
            );
        }
    }
    println!("   the graph, a stretch at a time, agrees to the last bit for {few} of them");
    Ok(())
}

fn commas(x: f64) -> String {
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
        format!("{:.0} ms", s * 1e3)
    } else if s < 120.0 {
        format!("{s:.1} s")
    } else {
        format!("{:.0} minutes", s / 60.0)
    }
}
