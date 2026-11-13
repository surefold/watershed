//! Prediction with expert advice: instead of choosing one trading rule on
//! the past and hoping, listen to all of them and shift trust, day by day,
//! towards the ones that have been right.
//!
//! The experts are every variant of the six rules in the `backtest` example,
//! several thousand of them, and one that just holds the market. Each
//! morning a learner splits its money among them; each evening it sees how
//! every expert did and changes the split. It never looks ahead, and there
//! are no years set aside: every day is a day it had not seen.
//!
//! The learners:
//!
//!   - split equally, and rebalance every day: trust no one more than another;
//!   - split equally and leave it: an expert's share grows as it wins;
//!   - follow the leader: all of it on whoever is ahead so far;
//!   - exponential weights (Hedge): trust multiplies with each day's result.
//!     How fast is a setting; the first is the one the theory gives before
//!     seeing anything, the others are bolder;
//!   - AdaHedge: exponential weights that set their own speed from how
//!     surprising the experts have been;
//!   - fixed share: exponential weights that each day hand a little trust
//!     back to everyone, so that when the best expert changes, the new one
//!     is not too far behind to be noticed. This is the one for switching.
//!
//! They are compared with the market, with the best single expert (known
//! only afterwards), and with the best expert of each year (likewise).
//!
//!     cargo run --release --features cuda --example experts -- /path/to/data
//!
//! then, optionally, how many shares (500). The folder is as for `backtest`;
//! with none, or `made-up`, the prices wander at random.
//!
//! Experts differ in risk: one that holds twice as much makes twice as much
//! while prices rise, and a learner that scores on profit alone drifts to
//! it. So everything is run a second time with every expert at the market's
//! risk: given more or less money each day so that its recent swings are
//! the size of the market's. Then profit and profit for the risk taken are
//! the same thing. The cost of changing that amount is not charged.
//!
//! Moving money between experts means trading too. A learner paying to
//! trade is charged for all of it as if the two experts held nothing in
//! common, which is the most it could cost.

use std::path::Path;
use std::time::Instant;

#[path = "shared/market.rs"]
mod market;
use market::*;

/// No day's profit is taken to be beyond this, either way: the theory's
/// speed for exponential weights needs a range.
const RANGE: f64 = 0.1;

struct Expert {
    name: String,
    cost: f64,
    market: bool,
}

/// How each expert did each day: `profit[day * experts + expert]`.
struct Pool<'a> {
    experts: Vec<&'a Expert>,
    profit: Vec<f64>,
    days: usize,
}

impl Pool<'_> {
    fn day(&self, day: usize) -> &[f64] {
        let n = self.experts.len();
        &self.profit[day * n..(day + 1) * n]
    }
}

/// How a learner did.
struct Run {
    name: String,
    /// Its profit each day, after paying to move between experts.
    daily: Vec<f64>,
    /// How much of its money it moved from one expert to another, in all.
    moved: f64,
    /// The split it ended on.
    trust: Vec<f64>,
}

/// Runs a learner. Each day `split` is given the day's number and what
/// every expert made on each earlier day through `seen`, and sets the
/// split for the day; it is then shown the day through `learn`.
fn run(name: &str, pool: &Pool, cost: f64, mut learner: impl Learner) -> Run {
    let n = pool.experts.len();
    let mut trust = vec![1.0 / n as f64; n];
    let mut before = trust.clone();
    let (mut daily, mut moved) = (Vec::with_capacity(pool.days), 0.0);
    for day in 0..pool.days {
        learner.split(&mut trust);
        let shifted = trust
            .iter()
            .zip(&before)
            .map(|(a, b)| (a - b).abs())
            .sum::<f64>()
            / 2.0;
        moved += shifted;
        let today = pool.day(day);
        let made: f64 = trust.iter().zip(today).map(|(w, r)| w * r).sum();
        // Moving money between two experts trades at most twice that
        // money: all of what one held sold, all of what the other holds bought.
        daily.push(made - 2.0 * cost * shifted);
        before.copy_from_slice(&trust);
        learner.learn(today, &trust);
    }
    Run {
        name: name.to_string(),
        daily,
        moved,
        trust,
    }
}

trait Learner {
    /// Sets the split for the coming day. `trust` holds the last one.
    fn split(&mut self, trust: &mut [f64]);
    /// Shown what every expert made today, and the split that was used.
    fn learn(&mut self, today: &[f64], trust: &[f64]);
}

/// Trust in proportion to `exp(speed * profit so far)`. A speed of zero is
/// an equal split every day.
struct Exponential {
    speed: f64,
    so_far: Vec<f64>,
}

impl Exponential {
    fn new(n: usize, speed: f64) -> Self {
        Exponential {
            speed,
            so_far: vec![0.0; n],
        }
    }
}

/// `trust[k]` in proportion to `exp(speed * (so_far[k] - the most so far))`.
/// An infinite speed puts it all, equally, on those with the most.
fn exponential_split(trust: &mut [f64], so_far: &[f64], speed: f64) {
    let most = so_far.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    for (w, s) in trust.iter_mut().zip(so_far) {
        *w = match speed.is_finite() {
            true => (speed * (s - most)).exp(),
            false => f64::from(*s == most),
        };
    }
    let total: f64 = trust.iter().sum();
    trust.iter_mut().for_each(|w| *w /= total);
}

impl Learner for Exponential {
    fn split(&mut self, trust: &mut [f64]) {
        exponential_split(trust, &self.so_far, self.speed);
    }
    fn learn(&mut self, today: &[f64], _: &[f64]) {
        for (s, r) in self.so_far.iter_mut().zip(today) {
            *s += r;
        }
    }
}

/// An equal split at the start, then left alone: each expert's share is
/// what its money has grown to.
struct LeftAlone {
    grown: Vec<f64>,
}

impl Learner for LeftAlone {
    fn split(&mut self, trust: &mut [f64]) {
        let total: f64 = self.grown.iter().sum();
        for (w, g) in trust.iter_mut().zip(&self.grown) {
            *w = g / total;
        }
    }
    fn learn(&mut self, today: &[f64], _: &[f64]) {
        for (g, r) in self.grown.iter_mut().zip(today) {
            *g *= 1.0 + r;
        }
    }
}

/// Exponential weights whose speed is `ln(experts)` over how much worse it
/// has done so far than its own trust said it should: fast while the
/// experts agree, slower as they surprise it. (De Rooij, Van Erven,
/// Grünwald and Koolen, "Follow the leader if you can, hedge if you must".)
struct AdaHedge {
    so_far: Vec<f64>,
    surprise: f64,
}

impl AdaHedge {
    fn speed(&self) -> f64 {
        match self.surprise > 0.0 {
            true => (self.so_far.len() as f64).ln() / self.surprise,
            false => f64::INFINITY,
        }
    }
}

impl Learner for AdaHedge {
    fn split(&mut self, trust: &mut [f64]) {
        exponential_split(trust, &self.so_far, self.speed());
    }
    fn learn(&mut self, today: &[f64], trust: &[f64]) {
        let speed = self.speed();
        let made: f64 = trust.iter().zip(today).map(|(w, r)| w * r).sum();
        // How much less it made than its trust, taken as certainty, said
        // it would: with an infinite speed, less than the best expert it
        // trusted. The best is taken out first so that nothing overflows.
        let best = trust
            .iter()
            .zip(today)
            .filter(|(w, _)| **w > 0.0)
            .map(|(_, r)| *r)
            .fold(f64::NEG_INFINITY, f64::max);
        let expected = match speed.is_finite() {
            true => {
                let mix: f64 = trust
                    .iter()
                    .zip(today)
                    .map(|(w, r)| w * (speed * (r - best)).exp())
                    .sum();
                best + mix.ln() / speed
            }
            false => best,
        };
        self.surprise += (expected - made).max(0.0);
        for (s, r) in self.so_far.iter_mut().zip(today) {
            *s += r;
        }
    }
}

/// Exponential weights that each day hand `share` of all trust back to
/// everyone equally. (Herbster and Warmuth, "Tracking the best expert".)
struct FixedShare {
    speed: f64,
    share: f64,
    next: Vec<f64>,
}

impl Learner for FixedShare {
    fn split(&mut self, trust: &mut [f64]) {
        trust.copy_from_slice(&self.next);
    }
    fn learn(&mut self, today: &[f64], trust: &[f64]) {
        let best = today.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        for ((next, w), r) in self.next.iter_mut().zip(trust).zip(today) {
            *next = w * (self.speed * (r - best)).exp();
        }
        let total: f64 = self.next.iter().sum();
        let each = self.share / self.next.len() as f64;
        for next in &mut self.next {
            *next = (1.0 - self.share) * *next / total + each;
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().filter(|dir| dir != "made-up");
    let shares: usize = match args.next() {
        Some(n) => n.parse().expect("the argument is a number"),
        None => 500,
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
    let days = history.days() - WARM_UP - 1;

    // Every expert's profit on every day.
    let runner = Runner::new()?;
    let mut experts: Vec<Expert> = Vec::new();
    // `profit_of[expert][day]`
    let mut profit_of: Vec<f64> = Vec::new();
    for rule in Rule::ALL {
        let settings = settings_to_try(rule);
        let started = Instant::now();
        let plan = plan_for(&history, &Setup::new(&history, rule), true)?;
        let made = started.elapsed();
        let started = Instant::now();
        let results = runner.run(&plan, &inputs_for(rule, &settings))?;
        println!(
            "{} experts' {days} days on the {} in {} (plan made in {})",
            settings.len(),
            runner.name(),
            time(started.elapsed()),
            time(made)
        );
        assert_eq!(results.len(), settings.len() * days);
        profit_of.extend(results.iter().map(|r| r / history.prices.len() as f64));
        experts.extend(settings.iter().map(|s| Expert {
            name: describe(rule, s),
            cost: s.cost,
            market: s.gain == 0.0,
        }));
    }
    println!();

    let pools = [
        (COST, false, "paying to trade"),
        (
            COST,
            true,
            "paying to trade, every expert at the market's risk",
        ),
        (0.0, false, "if trading were free"),
        (
            0.0,
            true,
            "if trading were free, every expert at the market's risk",
        ),
    ];
    for (cost, same_risk, paying) in pools {
        // The experts that pay this cost, and one of the many that hold the
        // market (every rule with a gain of zero does).
        let mut market_seen = false;
        let chosen: Vec<usize> = (0..experts.len())
            .filter(|&k| experts[k].cost == cost)
            .filter(|&k| !experts[k].market || !std::mem::replace(&mut market_seen, true))
            .collect();
        let n = chosen.len();
        let mut profit = vec![0.0; days * n];
        for (at, &k) in chosen.iter().enumerate() {
            for day in 0..days {
                profit[day * n + at] = profit_of[k * days + day];
            }
        }
        if same_risk {
            // An expert that holds twice as much makes twice as much in a
            // rising market, and that is risk, not skill. So each expert is
            // given more or less money, day by day, so that its recent
            // swings are the size of the market's: what it would have made
            // at the market's risk. Recent means an average of the squares
            // of its days so far that forgets half in three months; today's
            // amount is set from the days before today.
            const SETTLE: usize = 63;
            let forget = 0.5f64.powf(1.0 / SETTLE as f64);
            let market = chosen
                .iter()
                .position(|&k| experts[k].market)
                .expect("the market");
            let mut swing = vec![0.0f64; n];
            for day in 0..days {
                let markets = swing[market];
                for (r, swing) in profit[day * n..(day + 1) * n].iter_mut().zip(&mut swing) {
                    let was = *r;
                    if day >= SETTLE && *swing > 0.0 {
                        *r *= (markets / *swing).sqrt().clamp(0.25, 3.0);
                    }
                    *swing = forget * *swing + (1.0 - forget) * was * was;
                }
            }
        }
        let pool = Pool {
            experts: chosen.iter().map(|&k| &experts[k]).collect(),
            profit,
            days,
        };

        // What could only be known afterwards.
        let total = |k: usize, from: usize, to: usize| -> f64 {
            (from..to).map(|day| pool.profit[day * n + k]).sum()
        };
        let most = |from: usize, to: usize| {
            (0..n)
                .max_by(|&a, &b| total(a, from, to).total_cmp(&total(b, from, to)))
                .expect("some experts")
        };
        let market = pool
            .experts
            .iter()
            .position(|e| e.market)
            .expect("the market");
        let best = most(0, days);
        let of = |k: usize| -> Vec<f64> { (0..days).map(|day| pool.profit[day * n + k]).collect() };
        let year = DAYS_A_YEAR as usize;
        let mut each_year = Vec::with_capacity(days);
        for from in (0..days).step_by(year) {
            let to = (from + year).min(days);
            let k = most(from, to);
            each_year.extend((from..to).map(|day| pool.profit[day * n + k]));
        }

        // The speed the theory gives for exponential weights, knowing only
        // how many experts and days there are and the range of a day.
        let theory = (8.0 * (n as f64).ln() / days as f64).sqrt() / (2.0 * RANGE);
        let exponential = |speed: f64| Exponential::new(n, speed);
        let fixed_share = |speed: f64, share: f64| FixedShare {
            speed,
            share,
            next: vec![1.0 / n as f64; n],
        };
        // Fixed share that shares nothing is exponential weights.
        let (shares_nothing, plain) = (
            run("", &pool, cost, fixed_share(theory, 0.0)),
            run("", &pool, cost, exponential(theory)),
        );
        assert!(
            shares_nothing
                .daily
                .iter()
                .zip(&plain.daily)
                .all(|(a, b)| (a - b).abs() < 1e-12),
            "fixed share sharing nothing is not exponential weights"
        );

        let runs = vec![
            run("split equally, every day", &pool, cost, exponential(0.0)),
            run(
                "split equally and leave it",
                &pool,
                cost,
                LeftAlone {
                    grown: vec![1.0; n],
                },
            ),
            run("follow the leader", &pool, cost, exponential(f64::INFINITY)),
            run(
                &format!("exponential weights, speed {theory:.1} (theory)"),
                &pool,
                cost,
                exponential(theory),
            ),
            run(
                &format!("exponential weights, speed {:.0}", 10.0 * theory),
                &pool,
                cost,
                exponential(10.0 * theory),
            ),
            run(
                &format!("exponential weights, speed {:.0}", 100.0 * theory),
                &pool,
                cost,
                exponential(100.0 * theory),
            ),
            run(
                "AdaHedge (sets its own speed)",
                &pool,
                cost,
                AdaHedge {
                    so_far: vec![0.0; n],
                    surprise: 0.0,
                },
            ),
            run(
                &format!("fixed share, speed {:.0}, 1/252 a day", 10.0 * theory),
                &pool,
                cost,
                fixed_share(10.0 * theory, 1.0 / DAYS_A_YEAR),
            ),
            run(
                &format!("fixed share, speed {:.0}, 1/252 a day", 100.0 * theory),
                &pool,
                cost,
                fixed_share(100.0 * theory, 1.0 / DAYS_A_YEAR),
            ),
            run(
                &format!("fixed share, speed {:.0}, 1/21 a day", 100.0 * theory),
                &pool,
                cost,
                fixed_share(100.0 * theory, 1.0 / 21.0),
            ),
        ];

        println!(
            "{n} experts, {paying}, over {days} days ({:.1} years)",
            days as f64 / DAYS_A_YEAR
        );
        println!(
            "  {:<44} {:>7} {:>7} {:>9} {:>8} {:>7}",
            "", "a year", "steady", "1 became", "behind", "moved"
        );
        let best_total: f64 = of(best).iter().sum();
        let show = |name: &str, daily: &[f64], moved: Option<f64>| {
            let mean = daily.iter().sum::<f64>() / daily.len() as f64;
            let spread =
                (daily.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / daily.len() as f64).sqrt();
            println!(
                "  {name:<44} {:>6.1}% {:>7.2} {:>9.2} {:>8.2} {:>7}",
                100.0 * mean * DAYS_A_YEAR,
                mean / spread * DAYS_A_YEAR.sqrt(),
                daily.iter().map(|r| 1.0 + r).product::<f64>(),
                best_total - daily.iter().sum::<f64>(),
                moved.map_or(String::new(), |m| format!(
                    "{:.1}",
                    m / days as f64 * DAYS_A_YEAR
                )),
            );
        };
        show("the market", &of(market), None);
        for run in &runs {
            show(&run.name, &run.daily, Some(run.moved));
        }
        println!("  known only afterwards:");
        show("the best single expert", &of(best), None);
        show("the best expert of each year", &each_year, None);
        println!(
            "  (behind: how much less it made in all than the best single expert, 1.00 being all the money put in. The theory promises exponential weights at its speed no more than {:.1} behind. moved: how many times a year it moved all its money between experts.)",
            2.0 * RANGE * (days as f64 * (n as f64).ln() / 2.0).sqrt()
        );
        println!("  the best single expert: {}", pool.experts[best].name);
        for run in runs
            .iter()
            .filter(|run| run.name.starts_with("AdaHedge") || run.name.ends_with("1/21 a day"))
        {
            let mut most_trusted: Vec<usize> = (0..n).collect();
            most_trusted.sort_by(|&a, &b| run.trust[b].total_cmp(&run.trust[a]));
            println!("  {} ended trusting most:", run.name);
            for &k in most_trusted.iter().take(3) {
                println!(
                    "    {:>5.1}%  {}",
                    100.0 * run.trust[k],
                    pool.experts[k].name
                );
            }
        }
        println!();
    }
    Ok(())
}
