//! What does a change to an insurer's prices do to its book, and which change
//! would be best?
//!
//! The graph is a made-up car insurer's rules for pricing one policy, as a
//! pricing team would write them down:
//!
//!   - a base rate, multiplied by a factor for the driver's age band, for a
//!     short time holding a licence, for the car's group and its value, for
//!     the region, and for the miles driven a year;
//!   - a discount for years without a claim and for a chosen excess, and a
//!     loading for claims and for points on the licence;
//!   - a fee, a least premium, and tax;
//!   - for a customer renewing, a limit on how far the price may move from
//!     what they paid last year.
//!
//! and then what the price means for the insurer: how likely the customer is
//! to take it, given what a rival would charge and how much it has gone up,
//! and so the premium, the claims and the profit to expect from the policy.
//!
//! A scenario is one policy. The numbers in the rules (rates, factors,
//! limits) are *shared* inputs, so a price review is one call to
//! `set_shared` and one more run over the whole book. That is cheap enough
//! to try a thousand reviews and keep the best, which the last part does.
//!
//!     cargo run --release --features cuda --example insurance
//!
//! or choose the number of policies:
//!
//!     cargo run --release --features cuda --example insurance -- 1000000
//!
//! Every number is made up, and so are the drivers. The shape is not.

use std::time::{Duration, Instant};

use rayon::prelude::*;
use watershed_core::prelude::*;
use watershed_core::{IntoExpr, NodeId};
use watershed_gpu::Scenarios;

// ---- The rules' numbers ----

/// Declares the prices once: their numbers ([`Prices`]) and the graph inputs
/// that hold them ([`Params`]), in the same order.
macro_rules! prices {
    ($($(#[$doc:meta])* $name:ident = $value:expr;)*) => {
        /// The numbers in the pricing rules. Money is by the year.
        #[derive(Clone)]
        struct Prices { $($(#[$doc])* $name: f64),* }

        impl Default for Prices {
            fn default() -> Prices { Prices { $($name: $value),* } }
        }

        impl Prices {
            const NAMES: &[&str] = &[$(stringify!($name)),*];
            fn values(&self) -> Vec<f64> { vec![$(self.$name),*] }
        }

        /// The graph's inputs for the prices' numbers.
        struct Params { $($name: Input<f64>),* }

        impl Params {
            fn new(g: &mut Graph, prices: &Prices) -> Result<Params, CalcError> {
                Ok(Params { $($name: g.input(stringify!($name), prices.$name)?),* })
            }
            fn ids(&self) -> Vec<NodeId> { vec![$(self.$name.id()),*] }
        }
    };
}

prices! {
    base_rate = 380.0;

    /// A factor for the driver's age.
    age_under_21 = 2.6;
    age_21_to_24 = 1.8;
    age_25_to_29 = 1.25;
    age_30_to_49 = 1.0;
    age_50_to_69 = 0.9;
    age_70_up = 1.2;

    /// A driver with fewer years' licence than this pays `new_driver_load`
    /// more for each year short.
    new_driver_years = 5.0;
    new_driver_load = 0.08;

    /// More for each insurance group of the car above the first (of 20),
    /// and for each 10,000 of its value above `value_free`.
    group_step = 0.04;
    value_free = 10_000.0;
    value_load = 0.05;

    /// A factor for each of six regions, from the countryside to the middle
    /// of the biggest city.
    region_0 = 1.0;
    region_1 = 0.85;
    region_2 = 1.1;
    region_3 = 1.35;
    region_4 = 0.95;
    region_5 = 1.6;

    /// A factor for the miles driven a year.
    miles_under_5000 = 0.85;
    miles_5000_to_10000 = 1.0;
    miles_10000_to_15000 = 1.15;
    miles_15000_up = 1.35;

    /// Off for each year without a claim, up to the most.
    no_claims_step = 0.07;
    no_claims_most = 0.60;
    /// More for each claim in the last five years, and each point on the
    /// licence.
    claim_load = 0.30;
    points_load = 0.10;
    /// Off for each 100 of excess chosen, up to the most.
    excess_step = 0.03;
    excess_most = 0.20;

    fee = 35.0;
    /// The least a policy costs before tax.
    least = 250.0;
    tax = 0.12;

    /// A renewing customer's price may rise or fall by no more than these
    /// shares of what they paid last year.
    most_rise = 0.15;
    most_fall = 0.30;

    /// What a policy costs to run, apart from claims.
    expense = 55.0;

    /// How likely a customer is to take a price that matches a rival's: one
    /// renewing, and one new.
    keep_renewing = 0.80;
    keep_new = 0.35;
    /// How much less likely for a price twice the rival's, and for a price
    /// twice last year's.
    lost_when_dearer = 1.6;
    lost_when_risen = 0.8;
}

// ---- A policy ----

/// What is known about a policy: one scenario's inputs, in this order.
const FACTS: [&str; 13] = [
    "age",
    "licence",
    "group",
    "value",
    "region",
    "miles",
    "no_claims",
    "claims",
    "points",
    "excess",
    "last",
    "rival",
    "cost",
];
const AGE: usize = 0;
/// Years holding a licence.
const LICENCE: usize = 1;
/// The car's insurance group, 1 to 20.
const GROUP: usize = 2;
const VALUE: usize = 3;
/// 0 to 5.
const REGION: usize = 4;
const MILES: usize = 5;
/// Years without a claim.
const NO_CLAIMS: usize = 6;
/// Claims in the last five years.
const CLAIMS: usize = 7;
const POINTS: usize = 8;
const EXCESS: usize = 9;
/// What the customer paid last year, or 0 for a new customer.
const LAST: usize = 10;
/// What a rival would charge.
const RIVAL: usize = 11;
/// What the policy's claims will cost on average. The insurer does not know
/// this when it sets the price; it is here to say what the price led to.
const COST: usize = 12;

/// What is worked out for a policy, in this order.
const PRICE: usize = 0;
/// How likely the customer is to take the price.
const KEPT: usize = 1;
/// The premium (before tax), claims and profit to expect, allowing for the
/// customer perhaps not taking it.
const PREMIUM: usize = 2;
const CLAIMS_PAID: usize = 3;
const PROFIT: usize = 4;
const OUTPUTS: usize = 5;

/// The value for the band `x` is in: the first whose upper edge it is below,
/// or `last`.
fn band(x: Input<f64>, bands: &[(f64, Input<f64>)], last: Input<f64>) -> Expr<f64> {
    bands
        .iter()
        .rev()
        .fold(last.into_expr(), |otherwise, &(edge, value)| {
            when(x.lt(edge)).then(value).otherwise(otherwise)
        })
}

/// The inputs for a policy's facts, and the outputs.
type Rules = (Vec<Input<f64>>, Vec<Node<f64>>);

/// The rules for one policy.
fn rules(g: &mut Graph, p: &Params) -> Result<Rules, CalcError> {
    let facts: Vec<Input<f64>> = FACTS
        .iter()
        .map(|name| g.input(name, 0.0))
        .collect::<Result<_, _>>()?;
    let [
        age,
        licence,
        group,
        value,
        region,
        miles,
        no_claims,
        claims,
        points,
        excess,
        last,
        rival,
        cost,
    ] = facts[..]
    else {
        unreachable!()
    };

    // The risk: what makes a claim likelier or dearer.
    let for_age = g.define(
        "for_age",
        band(
            age,
            &[
                (21.0, p.age_under_21),
                (25.0, p.age_21_to_24),
                (30.0, p.age_25_to_29),
                (50.0, p.age_30_to_49),
                (70.0, p.age_50_to_69),
            ],
            p.age_70_up,
        ),
    )?;
    let for_new_driver = g.define(
        "for_new_driver",
        1.0 + (p.new_driver_years - licence).max(0.0) * p.new_driver_load,
    )?;
    let for_group = g.define("for_group", 1.0 + (group - 1.0) * p.group_step)?;
    let for_value = g.define(
        "for_value",
        1.0 + (value - p.value_free).max(0.0) / 10_000.0 * p.value_load,
    )?;
    let for_region = g.define(
        "for_region",
        band(
            region,
            &[
                (0.5, p.region_0),
                (1.5, p.region_1),
                (2.5, p.region_2),
                (3.5, p.region_3),
                (4.5, p.region_4),
            ],
            p.region_5,
        ),
    )?;
    let for_miles = g.define(
        "for_miles",
        band(
            miles,
            &[
                (5_000.0, p.miles_under_5000),
                (10_000.0, p.miles_5000_to_10000),
                (15_000.0, p.miles_10000_to_15000),
            ],
            p.miles_15000_up,
        ),
    )?;
    let risk = g.define(
        "risk",
        p.base_rate * for_age * for_new_driver * for_group * for_value * for_region * for_miles,
    )?;

    // The driver's record and choices.
    let no_claims_discount = g.define(
        "no_claims_discount",
        (no_claims * p.no_claims_step).min(p.no_claims_most),
    )?;
    let for_record = g.define(
        "for_record",
        (1.0 + claims * p.claim_load) * (1.0 + points * p.points_load),
    )?;
    let excess_discount = g.define(
        "excess_discount",
        (excess / 100.0 * p.excess_step).min(p.excess_most),
    )?;
    let rated = g.define(
        "rated",
        risk * for_record * (1.0 - no_claims_discount) * (1.0 - excess_discount),
    )?;
    let asked = g.define("asked", (rated + p.fee).max(p.least) * (1.0 + p.tax))?;

    // A renewing customer's price stays near last year's.
    let renewing = g.define("renewing", last.gt(0.0))?;
    let price = g.define(
        "price",
        when(renewing)
            .then(
                asked
                    .max(last * (1.0 - p.most_fall))
                    .min(last * (1.0 + p.most_rise)),
            )
            .otherwise(asked),
    )?;

    // Does the customer take it?
    let dearer = g.define("dearer_than_rival", price / rival - 1.0)?;
    let risen = g.define(
        "risen",
        when(renewing)
            .then((price / last.max(1.0) - 1.0).max(0.0))
            .otherwise(0.0),
    )?;
    let keep_at_rival = g.define(
        "keep_at_rival",
        when(renewing).then(p.keep_renewing).otherwise(p.keep_new),
    )?;
    let kept = g.define(
        "kept",
        (keep_at_rival - dearer * p.lost_when_dearer - risen * p.lost_when_risen)
            .max(0.02)
            .min(0.98),
    )?;

    // What to expect from the policy.
    let before_tax = g.define("before_tax", price / (1.0 + p.tax))?;
    let premium = g.define("premium", kept * before_tax)?;
    let claims_paid = g.define("claims_paid", kept * cost)?;
    let profit = g.define("profit", kept * (before_tax - cost - p.expense))?;
    Ok((facts, vec![price, kept, premium, claims_paid, profit]))
}

// ---- The book ----

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Between 0 and 1, never 0.
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
    fn between(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }
    fn normal(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
    /// A factor near 1: half are below it, a few far above.
    fn about_one(&mut self, spread: f64) -> f64 {
        (spread * self.normal()).exp()
    }
}

/// Makes up policy number `k`. What its claims will cost follows the driver
/// and the car, though not by the insurer's own factors: the insurer's prices
/// are its guess at this.
fn policy(k: usize, f: &mut [f64]) {
    let mut rng = Rng((k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    for _ in 0..4 {
        rng.next();
    }
    let age = (17.0 + 70.0 * rng.unit().powf(1.4)).floor();
    let licence = ((age - 17.0) * rng.between(0.5, 1.0)).floor();
    let group = rng.between(1.0, 21.0).floor().min(20.0);
    let value = (2_000.0 + 1_800.0 * group * rng.about_one(0.5)).round();
    let region = match rng.unit() {
        x if x < 0.25 => 0.0,
        x if x < 0.40 => 1.0,
        x if x < 0.62 => 2.0,
        x if x < 0.80 => 3.0,
        x if x < 0.92 => 4.0,
        _ => 5.0,
    };
    let miles = (7_500.0 * rng.about_one(0.5)).round();
    let claims = match rng.unit() {
        x if x < 0.78 => 0.0,
        x if x < 0.95 => 1.0,
        x if x < 0.99 => 2.0,
        _ => 3.0,
    };
    let no_claims = if claims > 0.0 {
        rng.between(0.0, 3.0).floor()
    } else {
        rng.between(0.0, licence.min(9.0) + 1.0).floor()
    };
    let points = if rng.chance(0.12) {
        rng.between(3.0, 10.0).floor()
    } else {
        0.0
    };
    let excess = [0.0, 100.0, 250.0, 500.0][(rng.next() % 4) as usize];

    let young = 1.0 + 2.4 * (-(age - 17.0) / 7.0).exp();
    let old = 1.0 + 0.5 * ((age - 72.0) / 15.0).max(0.0);
    let place = [0.9, 0.8, 1.05, 1.45, 1.0, 1.75][region as usize];
    let cost = 250.0
        * young
        * old
        * (1.0 + 0.3 * (-licence / 2.0).exp())
        * (1.0 + 0.03 * (group - 1.0))
        * (1.0 + 0.04 * value / 10_000.0)
        * place
        * (miles / 7_500.0).powf(0.45)
        * (1.0 + 0.22 * claims)
        * (1.0 + 0.04 * points)
        * (1.0 - 0.025 * no_claims)
        * (1.0 - 0.0002 * excess)
        * rng.about_one(0.3);
    // A rival's guess at the cost is as rough as anyone's.
    let rival = (cost * rng.about_one(0.15) / 0.68 + 90.0) * 1.12;
    let last = if rng.chance(0.75) {
        (cost * rng.about_one(0.2) / 0.70 + 85.0) * 1.12
    } else {
        0.0
    };

    f[AGE] = age;
    f[LICENCE] = licence;
    f[GROUP] = group;
    f[VALUE] = value;
    f[REGION] = region;
    f[MILES] = miles;
    f[NO_CLAIMS] = no_claims;
    f[CLAIMS] = claims;
    f[POINTS] = points;
    f[EXCESS] = excess;
    f[LAST] = last.round();
    f[RIVAL] = rival.round();
    f[COST] = cost;
}

// ---- Running it ----

struct Runner {
    /// Of the last run, how long the GPU spent calculating: the rest was
    /// copying policies to it and answers back.
    calculating: std::cell::Cell<Option<Duration>>,
    #[cfg(feature = "gpu")]
    gpu: watershed_gpu::Gpu,
}

impl Runner {
    fn new() -> Result<Runner, Box<dyn std::error::Error>> {
        Ok(Runner {
            calculating: std::cell::Cell::new(None),
            #[cfg(feature = "gpu")]
            gpu: watershed_gpu::Gpu::new()?,
        })
    }

    fn name(&self) -> &'static str {
        if cfg!(feature = "gpu") { "GPU" } else { "CPU" }
    }

    fn run(
        &self,
        plan: &Scenarios,
        inputs: &[f64],
    ) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
        #[cfg(feature = "gpu")]
        {
            let allowed = self.gpu.free_memory()? / 5 * 4;
            let (results, timing) = self.gpu.run_within(plan, inputs, allowed)?;
            self.calculating.set(Some(timing.compute));
            Ok(results)
        }
        #[cfg(not(feature = "gpu"))]
        Ok(plan.run_cpu(inputs))
    }
}

/// A book under one set of prices, added up.
#[derive(Clone, Copy)]
struct Book {
    policies: usize,
    /// How many of the policies the insurer expects to keep.
    kept: f64,
    premium: f64,
    claims: f64,
    profit: f64,
}

impl Book {
    fn of(results: &[f64]) -> Book {
        let sums = results
            .par_chunks(OUTPUTS)
            .fold(
                || [0.0; OUTPUTS],
                |mut sums, r| {
                    for (sum, x) in sums.iter_mut().zip(r) {
                        *sum += x;
                    }
                    sums
                },
            )
            .reduce(
                || [0.0; OUTPUTS],
                |mut a, b| {
                    for (sum, x) in a.iter_mut().zip(b) {
                        *sum += x;
                    }
                    a
                },
            );
        Book::of_totals(&sums, results.len() / OUTPUTS)
    }

    /// From each output already added up over `policies` policies.
    fn of_totals(sums: &[f64], policies: usize) -> Book {
        Book {
            policies,
            kept: sums[KEPT],
            premium: sums[PREMIUM],
            claims: sums[CLAIMS_PAID],
            profit: sums[PROFIT],
        }
    }

    fn share_kept(&self) -> f64 {
        self.kept / self.policies as f64
    }

    /// Claims paid for each 1 of premium.
    fn loss_ratio(&self) -> f64 {
        self.claims / self.premium
    }

    fn show(&self, label: &str) {
        println!(
            "  {label:<12} {:>8.1}% {:>14} {:>14} {:>9.1}% {:>14}",
            100.0 * self.share_kept(),
            commas(self.premium),
            commas(self.claims),
            100.0 * self.loss_ratio(),
            commas(self.profit)
        );
    }

    fn heading() {
        println!(
            "  {:<12} {:>9} {:>14} {:>14} {:>10} {:>14}",
            "", "kept", "premium", "claims", "of premium", "profit"
        );
    }
}

const AGES: [(&str, f64); 6] = [
    ("under 21", 21.0),
    ("21 to 24", 25.0),
    ("25 to 29", 30.0),
    ("30 to 49", 50.0),
    ("50 to 69", 70.0),
    ("70 and up", f64::INFINITY),
];

/// For each age band: the average price asked, and the share of customers
/// kept.
fn by_age(policies: &[f64], results: &[f64]) -> Vec<(f64, f64)> {
    let sums = policies
        .par_chunks(FACTS.len())
        .zip(results.par_chunks(OUTPUTS))
        .fold(
            || [(0.0, 0.0, 0.0); 6],
            |mut sums, (f, r)| {
                let b = AGES.iter().position(|a| f[AGE] < a.1).unwrap_or(5);
                sums[b].0 += r[PRICE];
                sums[b].1 += r[KEPT];
                sums[b].2 += 1.0;
                sums
            },
        )
        .reduce(
            || [(0.0, 0.0, 0.0); 6],
            |mut a, b| {
                for (x, y) in a.iter_mut().zip(b) {
                    *x = (x.0 + y.0, x.1 + y.1, x.2 + y.2);
                }
                a
            },
        );
    sums.iter().map(|s| (s.0 / s.2, s.1 / s.2)).collect()
}

/// A change to the prices' numbers.
type Review = Box<dyn Fn(&mut Prices)>;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse().expect("a number of policies"),
        None if cfg!(feature = "gpu") => 5_000_000,
        None => 1_000_000,
    };

    // The rules.
    let today = Prices::default();
    let mut g = Graph::new();
    let params = Params::new(&mut g, &today)?;
    let (facts, outputs) = rules(&mut g, &params)?;
    g.stabilize();
    let vary: Vec<NodeId> = facts.iter().map(|f| f.id()).collect();
    let output_ids: Vec<NodeId> = outputs.iter().map(|o| o.id()).collect();
    let mut plan = Scenarios::with_shared(&g, &vary, &params.ids(), &output_ids)?;
    println!(
        "the rules: {} formulas in {} levels, with {} numbers set by the pricing team and {} facts about a policy",
        plan.nodes(),
        plan.levels(),
        plan.shared_inputs(),
        plan.inputs()
    );

    // One policy by hand: a new customer of 40 with a long licence, the
    // cheapest car, in the first region, driving 7,000 miles, with no record
    // and no discounts. Every factor is 1, so the price is the base rate and
    // the fee, with tax.
    let mut one = vec![0.0; FACTS.len()];
    (one[AGE], one[LICENCE], one[GROUP], one[MILES]) = (40.0, 20.0, 1.0, 7_000.0);
    (one[VALUE], one[RIVAL], one[COST]) = (5_000.0, 500.0, 250.0);
    let by_plan = plan.run_cpu(&one);
    assert_eq!(by_plan[PRICE], (380.0 + 35.0) * (1.0 + 0.12));

    // The book.
    let started = Instant::now();
    let mut policies = vec![0.0; n * FACTS.len()];
    policies
        .par_chunks_mut(FACTS.len())
        .enumerate()
        .for_each(|(k, f)| policy(k, f));
    println!(
        "{} policies made up in {}",
        commas(n as f64),
        time(started.elapsed())
    );
    println!();

    // Today's prices.
    let runner = Runner::new()?;
    let started = Instant::now();
    let now = runner.run(&plan, &policies)?;
    let took = started.elapsed();
    println!(
        "{}: every policy at today's prices in {} ({} policies a second)",
        runner.name(),
        time(took),
        commas(n as f64 / took.as_secs_f64())
    );
    if let Some(calculating) = runner.calculating.get() {
        println!(
            "  of that, {} was calculating; the rest was copying policies to the GPU and answers back",
            time(calculating)
        );
    }

    // The same on the CPU for some, and on the graph itself for fewer: they
    // must agree to the last bit.
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    if cfg!(feature = "gpu") {
        let some = n.min(1_000_000);
        let started = Instant::now();
        let on_cpu = plan.run_cpu(&policies[..some * FACTS.len()]);
        let took = started.elapsed();
        println!(
            "CPU, {cores} cores: {} policies a second",
            commas(some as f64 / took.as_secs_f64())
        );
        assert!(
            on_cpu
                .iter()
                .zip(&now)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "the CPU and the GPU differ"
        );
    }
    let few = n.min(20_000);
    let started = Instant::now();
    for (f, result) in policies
        .chunks(FACTS.len())
        .zip(now.chunks(OUTPUTS))
        .take(few)
    {
        for (&fact, &x) in facts.iter().zip(f) {
            g.set(fact, x)?;
        }
        g.stabilize();
        for (&output, &x) in outputs.iter().zip(result) {
            assert!(
                g.get(output)?.to_bits() == x.to_bits(),
                "the graph and the plan differ"
            );
        }
    }
    let took = started.elapsed();
    println!(
        "the graph, a policy at a time: {} policies a second",
        commas(few as f64 / took.as_secs_f64())
    );
    println!("checked: all three give the same answers, to the last bit");
    println!();

    // The book as it is.
    let book = Book::of(&now);
    let ages_now = by_age(&policies, &now);
    println!("the book at today's prices (money by the year)");
    Book::heading();
    book.show("today");
    println!();

    // Price reviews: the same policies, other numbers in the rules.
    let reviews: [(&str, Review); 4] = [
        ("put the base rate up 5%", Box::new(|p| p.base_rate *= 1.05)),
        (
            "charge the under-25s a fifth less",
            Box::new(|p| {
                p.age_under_21 *= 0.8;
                p.age_21_to_24 *= 0.8;
            }),
        ),
        (
            "a bigger discount for years without a claim: 9% a year, up to 70%",
            Box::new(|p| (p.no_claims_step, p.no_claims_most) = (0.09, 0.70)),
        ),
        (
            "let a renewing customer's price rise by no more than 5%, not 15%",
            Box::new(|p| p.most_rise = 0.05),
        ),
    ];
    for (what, change) in &reviews {
        let mut prices = today.clone();
        change(&mut prices);
        let started = Instant::now();
        plan.set_shared(&prices.values());
        let results = runner.run(&plan, &policies)?;
        println!("{what}  ({})", time(started.elapsed()));
        Book::heading();
        book.show("today");
        Book::of(&results).show("after");
        show_ages(&ages_now, &by_age(&policies, &results));
        println!();
    }

    // Which review would be best? Try many: each moves every factor a little
    // at random. Keep the one that makes most profit without keeping fewer
    // customers, and try again around that one, with smaller moves.
    const ROUNDS: usize = 5;
    const EACH_ROUND: usize = 200;
    let tuned: Vec<usize> = Prices::NAMES
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            [
                "base_rate",
                "group_step",
                "claim_load",
                "points_load",
                "no_claims_step",
            ]
            .contains(name)
                || ["age_", "region_", "miles_"]
                    .iter()
                    .any(|p| name.starts_with(p))
        })
        .map(|(k, _)| k)
        .collect();
    // On the GPU the policies are loaded once and stay there: a review
    // changes a few shared numbers, and only the totals come back, so every
    // review is tried on the whole book. On the CPU, on a part of it.
    let part = match cfg!(feature = "gpu") {
        true => &policies[..],
        false => &policies[..n.min(500_000) * FACTS.len()],
    };
    #[cfg(feature = "gpu")]
    let mut loaded = runner.gpu.load(&plan, part)?;
    // Only the GPU's way changes anything it holds.
    #[cfg_attr(not(feature = "gpu"), allow(unused_mut))]
    let mut book_under = |plan: &Scenarios| -> Result<Book, Box<dyn std::error::Error>> {
        #[cfg(feature = "gpu")]
        let totals = loaded.totals(plan)?;
        #[cfg(not(feature = "gpu"))]
        let totals = plan.totals_cpu(part);
        Ok(Book::of_totals(&totals, part.len() / FACTS.len()))
    };
    plan.set_shared(&today.values());
    let part_now = book_under(&plan)?;
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut best = (today.values(), part_now);
    let started = Instant::now();
    for round in 0..ROUNDS {
        let step = 0.12 / (1 << round) as f64;
        let around = best.0.clone();
        for _ in 0..EACH_ROUND {
            let mut values = around.clone();
            for &k in &tuned {
                values[k] *= 1.0 + step * rng.between(-1.0, 1.0);
            }
            plan.set_shared(&values);
            let tried = book_under(&plan)?;
            if tried.kept >= part_now.kept && tried.profit > best.1.profit {
                best = (values, tried);
            }
        }
    }
    let took = started.elapsed();
    let tries = ROUNDS * EACH_ROUND;
    println!(
        "{tries} reviews tried on {} policies in {} ({} each): the one with most profit that keeps as many customers",
        commas((part.len() / FACTS.len()) as f64),
        time(took),
        time(took / tries as u32)
    );
    #[cfg(feature = "gpu")]
    drop(loaded);
    plan.set_shared(&best.0);
    let results = runner.run(&plan, &policies)?;
    println!("on the whole book:");
    Book::heading();
    book.show("today");
    Book::of(&results).show("the best");
    show_ages(&ages_now, &by_age(&policies, &results));
    println!("  what it changed most:");
    let before = today.values();
    let mut moved: Vec<(usize, f64)> = tuned
        .iter()
        .map(|&k| (k, best.0[k] / before[k] - 1.0))
        .collect();
    moved.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
    for &(k, by) in moved.iter().take(8) {
        println!(
            "    {:<22} {:>8.3} → {:>8.3}  ({:+.0}%)",
            Prices::NAMES[k],
            before[k],
            best.0[k],
            100.0 * by
        );
    }
    Ok(())
}

/// The average price and the share kept in each age band, before and after.
fn show_ages(before: &[(f64, f64)], after: &[(f64, f64)]) {
    println!("  by age:      average price asked            customers kept");
    for ((label, _), (b, a)) in AGES.iter().zip(before.iter().zip(after)) {
        println!(
            "    {label:<10} {:>7} → {:>7} ({:>+5.1}%)     {:>5.1}% → {:>5.1}%",
            commas(b.0),
            commas(a.0),
            100.0 * (a.0 / b.0 - 1.0),
            100.0 * b.1,
            100.0 * a.1
        );
    }
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
    if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else {
        format!("{s:.1} s")
    }
}
