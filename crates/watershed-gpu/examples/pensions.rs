//! Will a pension scheme have enough money, and what should it do about it?
//!
//! A defined-benefit scheme has promised each member a pension for life.
//! The pensions rise with inflation, but only so far: by no more than a cap
//! each year, and never down. So what the scheme owes depends on the path
//! inflation takes, on how long people live, and on pay for those still
//! working; and whether its assets will cover that depends on what they are
//! invested in. The trustees choose how much to hold in shares and how much
//! the employer pays in. To choose well they need to see each choice against
//! thousands of ways the economy could go, over seventy-five years.
//!
//! The graph is the scheme: every member's pension in every year (it rises
//! by capped inflation, or with pay until retirement), the chance they are
//! alive to draw it, the scheme's payments each year, and its assets year
//! by year after paying pensions and earning returns. A scenario is one
//! path of the economy. The trustees' choices and the actuary's assumptions
//! are shared inputs, so each strategy is one more run of everything.
//!
//!     cargo run --release --features cuda --example pensions
//!
//! then optionally how many groups of members (50,000) and how many paths
//! (5,000).
//!
//! The scheme, the members and the economy are made up. The scheme is
//! closed: nobody earns more service, though pensions of those still
//! working follow their pay.

use std::time::{Duration, Instant};

use rayon::prelude::*;
use watershed_core::prelude::*;
use watershed_core::{IntoExpr, NodeId};
use watershed_gpu::Scenarios;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const YEARS: usize = 75;
const RETIRES_AT: usize = 65;
const LAST_AGE: usize = 105;
/// Pay rises this much faster than prices.
const PAY_OVER_PRICES: f64 = 0.01;

/// What a path sets for each year, in this order.
const INFLATION: usize = 0;
/// What 1 owed a year later is discounted by: the yield on government bonds.
const YIELD: usize = 1;
const SHARES: usize = 2;
const LINKED_BONDS: usize = 3;
const BONDS: usize = 4;
const PER_YEAR: usize = 5;

/// What is worked out for the scheme.
const OWED: usize = 0;
/// The assets when the last pension has been paid.
const AT_THE_END: usize = 1;
/// The lowest the assets ever get: below zero, the money ran out.
const LOWEST: usize = 2;
const OUTPUTS: usize = 3;

#[derive(Clone, Copy)]
enum Kind {
    /// Still working: the pension will be `service` sixtieths of final pay.
    Working {
        pay: f64,
        service: f64,
    },
    /// Left, not yet retired: a pension that rises until it starts.
    Left {
        pension: f64,
    },
    Retired {
        pension: f64,
    },
}

struct Member {
    age: usize,
    kind: Kind,
    /// How many members like this.
    count: f64,
}

/// What the trustees choose and the actuary assumes. The same on every path
/// of a run.
#[derive(Clone)]
struct Choices {
    assets: f64,
    /// The shares of the assets in shares and in inflation-linked bonds;
    /// the rest is in ordinary bonds.
    in_shares: f64,
    in_linked: f64,
    /// What the employer pays in each year for the first ten.
    paid_in: f64,
    /// The most a pension in payment rises in a year, and one not yet
    /// started.
    cap: f64,
    cap_before: f64,
    /// Death rates, as a multiple of the table's.
    deaths: f64,
}

impl Choices {
    fn values(&self) -> Vec<f64> {
        vec![
            self.assets,
            self.in_shares,
            self.in_linked,
            self.paid_in,
            self.cap,
            self.cap_before,
            self.deaths,
        ]
    }
}

/// The chance of dying in a year at an age.
fn dies(age: usize) -> f64 {
    (0.00004 * (0.095 * age as f64).exp()).min(1.0)
}

/// The scheme on one path, written out as ordinary code: the same sums as
/// the graph, to check the graph against.
fn by_hand(members: &[Member], path: &[f64], c: &Choices) -> [f64; OUTPUTS] {
    let mut paid = [0.0f64; YEARS];
    for m in members {
        let mut alive = 1.0;
        let (mut pay, mut pension) = match m.kind {
            Kind::Working { pay, .. } => (pay, 0.0),
            Kind::Left { pension } | Kind::Retired { pension } => (0.0, pension),
        };
        let mut drawing = matches!(m.kind, Kind::Retired { .. });
        for t in 0..YEARS {
            let age = m.age + t + 1;
            if age > LAST_AGE {
                break;
            }
            let inflation = path[PER_YEAR * t + INFLATION];
            alive *= 1.0 - (dies(age - 1) * c.deaths).min(1.0);
            if drawing {
                pension *= 1.0 + inflation.max(0.0).min(c.cap);
            } else if age > RETIRES_AT {
                if let Kind::Working { service, .. } = m.kind {
                    pension = service / 60.0 * pay;
                } else {
                    pension *= 1.0 + inflation.max(0.0).min(c.cap_before);
                }
                drawing = true;
            } else if matches!(m.kind, Kind::Working { .. }) {
                pay *= 1.0 + inflation + PAY_OVER_PRICES;
            } else {
                pension *= 1.0 + inflation.max(0.0).min(c.cap_before);
            }
            if drawing {
                paid[t] += pension * alive * m.count;
            }
        }
    }
    let (mut owed, mut discount) = (0.0, 1.0);
    let (mut assets, mut lowest) = (c.assets, c.assets);
    for t in 0..YEARS {
        let at = |what: usize| path[PER_YEAR * t + what];
        discount /= 1.0 + at(YIELD);
        owed += paid[t] * discount;
        let earned = c.in_shares * at(SHARES)
            + c.in_linked * at(LINKED_BONDS)
            + (1.0 - c.in_shares - c.in_linked) * at(BONDS);
        let paid_in = if t < 10 { c.paid_in } else { 0.0 };
        assets = (assets + paid_in) * (1.0 + earned) - paid[t];
        lowest = lowest.min(assets);
    }
    [owed, assets, lowest]
}

// ---- The scheme as a graph ----

struct Scheme {
    g: Graph,
    economy: Vec<Input<f64>>,
    knobs: Vec<Input<f64>>,
    /// Each member's pension or pay now, which can be changed on the graph.
    amounts: Vec<Input<f64>>,
    outputs: Vec<NodeId>,
    member_years: usize,
}

impl Scheme {
    fn new(members: &[Member], choices: &Choices, usual: &[f64]) -> Result<Scheme> {
        let mut g = Graph::new();
        let names = [
            "assets",
            "in_shares",
            "in_linked",
            "paid_in",
            "cap",
            "cap_before",
            "deaths",
        ];
        let knobs: Vec<Input<f64>> = names
            .iter()
            .zip(choices.values())
            .map(|(name, x)| g.input(name, x))
            .collect::<std::result::Result<_, _>>()?;
        let [
            assets,
            in_shares,
            in_linked,
            paid_in,
            cap,
            cap_before,
            deaths,
        ] = knobs[..]
        else {
            unreachable!()
        };
        let economy: Vec<Input<f64>> = usual
            .iter()
            .enumerate()
            .map(|(k, &x)| g.input(&format!("economy{k}"), x))
            .collect::<std::result::Result<_, _>>()?;
        let at = |t: usize, what: usize| economy[PER_YEAR * t + what];
        // What pensions rise by each year, which every member reads.
        let mut rise = Vec::with_capacity(YEARS);
        let mut rise_before = Vec::with_capacity(YEARS);
        for t in 0..YEARS {
            let inflation = at(t, INFLATION);
            rise.push(g.define(&format!("rise{t}"), 1.0 + inflation.max(0.0).min(cap))?);
            rise_before.push(g.define(
                &format!("rise_before{t}"),
                1.0 + inflation.max(0.0).min(cap_before),
            )?);
        }

        // Each member: a node a year for the pension (or the pay), and one
        // for the chance of being alive, which no path changes.
        let mut paid: Vec<Vec<Expr<f64>>> = vec![Vec::new(); YEARS];
        let mut amounts = Vec::with_capacity(members.len());
        let mut member_years = 0;
        for (k, m) in members.iter().enumerate() {
            let now = match m.kind {
                Kind::Working { pay, .. } => pay,
                Kind::Left { pension } | Kind::Retired { pension } => pension,
            };
            let amount = g.input(&format!("m{k}_now"), now)?;
            amounts.push(amount);
            let mut amount: Expr<f64> = amount.into_expr();
            let mut alive: Expr<f64> = 1.0.into_expr();
            let mut drawing = matches!(m.kind, Kind::Retired { .. });
            let working = matches!(m.kind, Kind::Working { .. });
            for t in 0..YEARS {
                let age = m.age + t + 1;
                if age > LAST_AGE {
                    break;
                }
                member_years += 1;
                alive = g
                    .define(
                        &format!("m{k}_alive{t}"),
                        alive * (1.0 - (dies(age - 1) * deaths).min(1.0)),
                    )?
                    .into_expr();
                let next = if drawing {
                    amount * rise[t]
                } else if age > RETIRES_AT {
                    drawing = true;
                    match m.kind {
                        Kind::Working { service, .. } => service / 60.0 * amount,
                        _ => amount * rise_before[t],
                    }
                } else if working {
                    amount * (1.0 + at(t, INFLATION) + PAY_OVER_PRICES)
                } else {
                    amount * rise_before[t]
                };
                amount = g.define(&format!("m{k}_amount{t}"), next)?.into_expr();
                if drawing {
                    paid[t].push(amount.clone() * alive.clone() * m.count);
                }
            }
        }

        // The scheme, year by year.
        let mut owed: Option<Node<f64>> = None;
        let mut discount: Option<Node<f64>> = None;
        let mut have: Expr<f64> = assets.into_expr();
        let mut lowest: Expr<f64> = assets.into_expr();
        for (t, terms) in paid.into_iter().enumerate() {
            let mut round = 0;
            let mut terms = terms;
            if terms.is_empty() {
                terms.push(0.0.into_expr());
            }
            while terms.len() > 1 || round == 0 {
                round += 1;
                let mut next = Vec::with_capacity(terms.len() / 8 + 1);
                for (k, group) in terms.chunks(8).enumerate() {
                    let mut all = group[0].clone();
                    for term in &group[1..] {
                        all = all + term.clone();
                    }
                    next.push(g.define(&format!("paid{t}_{round}_{k}"), all)?.into_expr());
                }
                terms = next;
            }
            let paid = terms.pop().expect("a total");
            let d = match discount {
                Some(d) => g.define(&format!("discount{t}"), d / (1.0 + at(t, YIELD)))?,
                None => g.define(&format!("discount{t}"), 1.0 / (1.0 + at(t, YIELD)))?,
            };
            discount = Some(d);
            owed = Some(match owed {
                Some(o) => g.define(&format!("owed{t}"), o + paid.clone() * d)?,
                None => g.define(&format!("owed{t}"), paid.clone() * d)?,
            });
            let earned = in_shares * at(t, SHARES)
                + in_linked * at(t, LINKED_BONDS)
                + (1.0 - in_shares - in_linked) * at(t, BONDS);
            let before = if t < 10 { have + paid_in } else { have };
            let after = g.define(&format!("assets{t}"), before * (1.0 + earned) - paid)?;
            lowest = g
                .define(&format!("lowest{t}"), lowest.min(after))?
                .into_expr();
            have = after.into_expr();
        }
        let end = g.define("at_the_end", have)?;
        let lowest = g.define("lowest", lowest)?;
        g.stabilize();
        Ok(Scheme {
            g,
            economy,
            knobs,
            amounts,
            outputs: vec![owed.expect("a year").id(), end.id(), lowest.id()],
            member_years,
        })
    }

    fn plan(&self) -> Result<Scenarios> {
        let vary: Vec<NodeId> = self.economy.iter().map(|e| e.id()).collect();
        let shared: Vec<NodeId> = self.knobs.iter().map(|k| k.id()).collect();
        Ok(Scenarios::with_shared(
            &self.g,
            &vary,
            &shared,
            &self.outputs,
        )?)
    }
}

// ---- The economy and the members ----

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }
    fn normal(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
}

/// A path of the economy. Inflation wanders around 2.5% and takes years to
/// come back; bond yields follow it; shares earn more than bonds and swing
/// much more; bonds lose when yields rise.
fn economy(chance: f64, rng: &mut Rng) -> Vec<f64> {
    let mut path = Vec::with_capacity(PER_YEAR * YEARS);
    let (mut inflation, mut real) = (0.025, 0.012);
    let mut yield_was = inflation + real;
    for _ in 0..YEARS {
        inflation = 0.025 + 0.7 * (inflation - 0.025) + chance * 0.013 * rng.normal();
        real = 0.012 + 0.8 * (real - 0.012) + chance * 0.005 * rng.normal();
        let yields = (inflation + real).max(0.001);
        let shares = yields + 0.035 + chance * 0.17 * rng.normal();
        let linked = inflation + real - 12.0 * (real - 0.012) * 0.2 + chance * 0.03 * rng.normal();
        let bonds = yield_was - 9.0 * (yields - yield_was);
        yield_was = yields;
        path.extend([inflation, yields, shares.max(-0.6), linked, bonds.max(-0.5)]);
    }
    path
}

/// Makes up group of members number `k`.
fn member(k: usize) -> Member {
    let mut rng = Rng((k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    for _ in 0..4 {
        rng.next();
    }
    let count = rng.between(1.0, 12.0).floor();
    match rng.unit() {
        x if x < 0.25 => {
            let age = rng.between(30.0, 65.0) as usize;
            Member {
                age,
                kind: Kind::Working {
                    pay: (38_000.0 * (0.45 * rng.normal()).exp()).round(),
                    service: rng
                        .between(3.0, (age as f64 - 22.0).clamp(4.0, 35.0))
                        .floor(),
                },
                count,
            }
        }
        x if x < 0.6 => Member {
            age: rng.between(35.0, 65.0) as usize,
            kind: Kind::Left {
                pension: (5_500.0 * (0.6 * rng.normal()).exp()).round(),
            },
            count,
        },
        _ => Member {
            age: rng.between(60.0, 96.0) as usize,
            kind: Kind::Retired {
                pension: (11_000.0 * (0.6 * rng.normal()).exp()).round(),
            },
            count,
        },
    }
}

struct Runner {
    #[cfg(feature = "cuda")]
    gpu: watershed_gpu::Gpu,
}

impl Runner {
    fn new() -> Result<Runner> {
        Ok(Runner {
            #[cfg(feature = "cuda")]
            gpu: watershed_gpu::Gpu::new()?,
        })
    }
    fn name(&self) -> &'static str {
        if cfg!(feature = "cuda") { "GPU" } else { "CPU" }
    }
    fn run(&self, plan: &Scenarios, inputs: &[f64]) -> Result<Vec<f64>> {
        #[cfg(feature = "cuda")]
        return Ok(self.gpu.run(plan, inputs)?);
        #[cfg(not(feature = "cuda"))]
        return Ok(plan.run_cpu(inputs));
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let number = |arg: Option<String>, otherwise: usize| match arg {
        Some(n) => n.replace('_', "").parse().expect("a number"),
        None => otherwise,
    };
    let size = number(args.next(), 50_000);
    let scenarios = number(args.next(), 5_000);
    let runner = Runner::new()?;
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());

    let members: Vec<Member> = (0..size).map(member).collect();
    let steady = economy(0.0, &mut Rng(1));
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let paths: Vec<f64> = (0..scenarios)
        .flat_map(|_| economy(1.0, &mut rng))
        .collect();
    // The scheme starts with nine tenths of what it owes on a steady path,
    // 40% of it in shares and 30% in inflation-linked bonds, and the
    // employer paying in a hundredth of that a year for ten years.
    let mut choices = Choices {
        assets: 0.0,
        in_shares: 0.4,
        in_linked: 0.3,
        paid_in: 0.0,
        cap: 0.05,
        cap_before: 0.025,
        deaths: 1.0,
    };
    let owed_steady = by_hand(&members, &steady, &choices)[OWED];
    choices.assets = (0.9 * owed_steady).round();
    choices.paid_in = (0.01 * owed_steady).round();

    // ---- 1. Is it right? ----
    println!("1. The graph against the same sums written out by hand");
    println!();
    let few = &members[..size.min(2_000)];
    let small = Scheme::new(few, &choices, &steady)?;
    let plan = small.plan()?;
    let some = &paths[..scenarios.min(100) * PER_YEAR * YEARS];
    let got = runner.run(&plan, some)?;
    let mut worst = 0.0f64;
    for (path, got) in some.chunks(PER_YEAR * YEARS).zip(got.chunks(OUTPUTS)) {
        let want = by_hand(few, path, &choices);
        for (g, w) in got.iter().zip(want) {
            worst = worst.max((g - w).abs() / want[OWED]);
        }
    }
    println!(
        "  {} groups of members on {} paths: largest difference {worst:.1e} of what the scheme owes",
        commas(few.len() as f64),
        some.len() / (PER_YEAR * YEARS)
    );
    assert!(worst < 1e-9, "the graph and the sums by hand differ");
    println!();

    // ---- 2. The whole scheme ----
    println!("2. The whole scheme");
    println!();
    let started = Instant::now();
    let mut scheme = Scheme::new(&members, &choices, &steady)?;
    let graph_took = started.elapsed();
    let started = Instant::now();
    let mut plan = scheme.plan()?;
    println!(
        "  {} groups of members ({} people), {} years of pension to project",
        commas(size as f64),
        commas(members.iter().map(|m| m.count).sum()),
        commas(scheme.member_years as f64)
    );
    println!(
        "  the graph: {} nodes a path works out, in {} levels (built in {}; plan made in {})",
        commas(plan.nodes() as f64),
        commas(plan.levels() as f64),
        time(graph_took),
        time(started.elapsed())
    );
    let started = Instant::now();
    let results = runner.run(&plan, &paths)?;
    let took = started.elapsed();
    println!(
        "  {}: {} paths of the economy in {}: {} years of pension a second",
        runner.name(),
        commas(scenarios as f64),
        time(took),
        commas(scheme.member_years as f64 * scenarios as f64 / took.as_secs_f64())
    );
    let part = scenarios.min(128);
    let started = Instant::now();
    let on_cpu = plan.run_cpu(&paths[..part * PER_YEAR * YEARS]);
    let cpu_each = started.elapsed() / part as u32;
    println!(
        "  the plan on {cores} CPU cores: {} a path, so about {} for all of them",
        time(cpu_each),
        time(cpu_each * scenarios as u32)
    );
    assert!(
        on_cpu
            .iter()
            .zip(&results)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "the CPU and the {} differ",
        runner.name()
    );
    let started = Instant::now();
    let hand: Vec<[f64; OUTPUTS]> = paths[..part * PER_YEAR * YEARS]
        .par_chunks(PER_YEAR * YEARS)
        .map(|path| by_hand(&members, path, &choices))
        .collect();
    let hand_each = started.elapsed() / part as u32;
    println!(
        "  written out by hand, on {cores} cores: {} a path, so about {} for all of them",
        time(hand_each),
        time(hand_each * scenarios as u32)
    );
    for (hand, plan) in hand.iter().zip(results.chunks(OUTPUTS)) {
        assert!(
            (hand[OWED] - plan[OWED]).abs() <= 1e-9 * hand[OWED],
            "the totals differ"
        );
    }
    println!();

    // ---- 3. What it says ----
    let column =
        |results: &[f64], o: usize| -> Vec<f64> { results.chunks(OUTPUTS).map(|r| r[o]).collect() };
    let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
    let runs_out = |results: &[f64]| {
        results.chunks(OUTPUTS).filter(|r| r[LOWEST] < 0.0).count() as f64
            / (results.len() / OUTPUTS) as f64
    };
    let owed = mean(&column(&results, OWED));
    let mut ends = column(&results, AT_THE_END);
    ends.sort_by(f64::total_cmp);
    println!("3. Where the scheme stands");
    println!();
    println!("  it has {}", commas(choices.assets));
    println!(
        "  on one steady path it owes {} (today's value of every pension)",
        commas(owed_steady)
    );
    println!(
        "  on average over {} paths it owes {}",
        commas(scenarios as f64),
        commas(owed)
    );
    println!(
        "  the money runs out before the last pension is paid on {:.1}% of paths",
        100.0 * runs_out(&results)
    );
    println!(
        "  what is left at the end: 1 path in 10 has less than {}, half have less than {}",
        commas(ends[scenarios / 10]),
        commas(ends[scenarios / 2])
    );
    println!();

    // ---- 4. Strategies ----
    let in_shares = [0.0, 0.2, 0.4, 0.6, 0.8];
    let paid_in = [0.0, 0.01, 0.02, 0.03, 0.04, 0.06];
    println!(
        "4. {} strategies: how often the money runs out, by how much is in shares and how much is paid in",
        in_shares.len() * paid_in.len()
    );
    println!(
        "   (paid in each year for ten years, as a share of what is owed; the rest of the assets are split evenly between the two kinds of bond)"
    );
    println!();
    print!("  {:<18}", "in shares");
    for p in paid_in {
        print!("{:>9}", format!("{:.0}% paid", 100.0 * p));
    }
    println!();
    let started = Instant::now();
    let mut cheapest = Vec::new();
    for shares in in_shares {
        print!("  {:<18}", format!("{:.0}%", 100.0 * shares));
        let mut found = None;
        for p in paid_in {
            let mut c = choices.clone();
            (c.in_shares, c.in_linked) = (shares, (1.0 - shares) / 2.0);
            c.paid_in = p * owed_steady;
            plan.set_shared(&c.values());
            let out = runs_out(&runner.run(&plan, &paths)?);
            print!("{:>8.1}%", 100.0 * out);
            if out < 0.05 && found.is_none() {
                found = Some(p);
            }
        }
        println!();
        cheapest.push((shares, found));
    }
    let grid_took = started.elapsed();
    println!();
    println!(
        "  {} runs of the whole scheme on every path in {}",
        in_shares.len() * paid_in.len(),
        time(grid_took)
    );
    for (shares, found) in cheapest {
        match found {
            Some(p) => println!(
                "  with {:.0}% in shares, paying in {:.0}% a year keeps the chance of running out under 1 in 20",
                100.0 * shares,
                100.0 * p
            ),
            None => println!(
                "  with {:.0}% in shares, none of these keeps the chance of running out under 1 in 20",
                100.0 * shares
            ),
        }
    }
    println!();

    // ---- 5. Other questions ----
    println!("5. Other questions, each one more run");
    println!();
    type Change = (&'static str, fn(&mut Choices));
    let changes: [Change; 3] = [
        ("pensions rise by at most 2.5% a year, not 5%", |c| {
            c.cap = 0.025
        }),
        ("people die at four fifths the rate of the table", |c| {
            c.deaths = 0.8
        }),
        ("pensions rise with inflation, with no cap", |c| c.cap = 1.0),
    ];
    println!(
        "  {:<52} {:>16} {:>9} {:>10}",
        "", "owes on average", "change", "runs out"
    );
    println!(
        "  {:<52} {:>16} {:>9} {:>9.1}%",
        "as it is",
        commas(owed),
        "",
        100.0 * runs_out(&results)
    );
    for (what, change) in changes {
        let mut c = choices.clone();
        change(&mut c);
        plan.set_shared(&c.values());
        let again = runner.run(&plan, &paths)?;
        let owes = mean(&column(&again, OWED));
        println!(
            "  {what:<52} {:>16} {:>+8.1}% {:>9.1}%",
            commas(owes),
            100.0 * (owes / owed - 1.0),
            100.0 * runs_out(&again)
        );
    }
    plan.set_shared(&choices.values());
    println!();

    // ---- 6. One thing changes ----
    println!("6. On the graph itself, when one thing changes");
    println!();
    let nodes = plan.nodes();
    let started = Instant::now();
    scheme.g.set(
        scheme.amounts[0],
        1.05 * {
            match members[0].kind {
                Kind::Working { pay, .. } => pay,
                Kind::Left { pension } | Kind::Retired { pension } => pension,
            }
        },
    )?;
    let one = scheme.g.stabilize();
    println!(
        "  one member's pension is corrected: {} nodes worked out again (of {} a path works out), in {}",
        commas(one.recalculated as f64),
        commas(nodes as f64),
        time(started.elapsed())
    );
    let started = Instant::now();
    scheme.g.set(scheme.economy[INFLATION], 0.06)?;
    let all = scheme.g.stabilize();
    println!(
        "  next year's inflation is forecast at 6%: {} nodes worked out again, in {}",
        commas(all.recalculated as f64),
        time(started.elapsed())
    );
    Ok(())
}

/// An amount with thousands separated: `-1,234,567`.
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
        format!("{:.0} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else if s < 120.0 {
        format!("{s:.1} s")
    } else if s < 7200.0 {
        format!("{:.0} minutes", s / 60.0)
    } else {
        format!("{:.1} hours", s / 3600.0)
    }
}
