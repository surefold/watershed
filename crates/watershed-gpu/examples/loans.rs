//! What will a bank lose on its mortgages, and how sure is it?
//!
//! Accounting rules (IFRS 9, and CECL in the United States) make a bank set
//! money aside today for the loans it expects to go bad: for each loan, the
//! loss it expects over the next year, or over the loan's whole life once
//! the loan looks shakier than when it was made. Regulators then ask what
//! the losses would be in a slump. Both depend on where unemployment, house
//! prices and interest rates go, quarter by quarter, for decades.
//!
//! Projecting every loan along one such path is heavy enough that banks
//! mostly use three to five hand-written economic scenarios and weight
//! them. This example does that, and then does what the rule is really
//! asking for: thousands of paths the economy could take.
//!
//! The graph is the loan book. Every loan's every quarter is four or five
//! nodes: what is still owed (for a loan whose rate moves), the chance of
//! default this quarter, the chance the loan is still there, and the running
//! expected loss. A scenario is one path of the economy: unemployment, the
//! change in house prices and the interest rate in each of 100 quarters.
//! The model's sensitivities are shared inputs, so asking "what if defaults
//! respond more to unemployment than we think?" is one more run.
//!
//!     cargo run --release --features cuda --example loans
//!
//! then optionally how many loans (20,000) and how many paths (10,000).
//!
//! The loans, the model's numbers and the economy are all made up; the
//! shape of the calculation is the usual one. Whether a loan is looking
//! shakier is judged here on each path separately, where a bank would judge
//! it once across its scenarios.

use std::time::{Duration, Instant};

use rayon::prelude::*;
use watershed_core::prelude::*;
use watershed_core::{IntoExpr, NodeId, when};
use watershed_gpu::Scenarios;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The longest a loan has left: 25 years of quarters.
const QUARTERS: usize = 100;
/// Where unemployment (in points) and the interest rate rest.
const USUAL_UNEMPLOYMENT: f64 = 4.5;
const USUAL_RATE: f64 = 0.04;

/// What is worked out for the book.
const PROVISION: usize = 0;
/// The loss expected over every loan's whole life.
const LIFETIME: usize = 1;
/// What is owed on loans looking shakier than when they were made.
const SHAKY: usize = 2;
const OUTPUTS: usize = 3;

// ---- The loans ----

struct Loan {
    owed: f64,
    /// A fixed loan's yearly rate, or what a variable one pays above the
    /// interest rate of the day.
    rate: f64,
    variable: bool,
    /// Quarters left.
    left: usize,
    /// What the house is worth today.
    house: f64,
    /// How likely the borrower is to default in a quarter in usual times,
    /// as log-odds.
    risk: f64,
    /// The chance of default over the loan's life as it looked when the
    /// loan was made.
    looked: f64,
}

/// How strongly the model thinks things respond. The same for every loan
/// and path of a run.
#[derive(Clone)]
struct Model {
    /// Log-odds of default for each point of unemployment above usual.
    unemployment: f64,
    /// For each 1 of loan-to-value above 0.7.
    loan_to_value: f64,
    /// For each 1 by which a variable loan's payment has risen.
    payment_shock: f64,
    /// The share of a house's value got back when it is repossessed.
    recovered: f64,
    /// The least lost on a default, as a share of what is owed.
    least_lost: f64,
    /// Early repayment a quarter, and how much more for each 1 by which a
    /// fixed loan's rate is above what a new loan would cost.
    repaid_early: f64,
    refinancing: f64,
    /// A loan is looking shakier once its chance of default over its life
    /// is this many times what it looked when the loan was made.
    shakier: f64,
}

impl Model {
    fn usual() -> Model {
        Model {
            unemployment: 0.28,
            loan_to_value: 2.2,
            payment_shock: 2.5,
            recovered: 0.75,
            least_lost: 0.05,
            repaid_early: 0.012,
            refinancing: 1.6,
            shakier: 2.0,
        }
    }

    fn values(&self) -> Vec<f64> {
        vec![
            self.unemployment,
            self.loan_to_value,
            self.payment_shock,
            self.recovered,
            self.least_lost,
            self.repaid_early,
            self.refinancing,
            self.shakier,
        ]
    }
}

/// The level payment that clears `owed` in `quarters` at `rate` a quarter.
fn payment(owed: f64, rate: f64, quarters: usize) -> f64 {
    owed * rate / (1.0 - (1.0 + rate).powf(-(quarters as f64)))
}

/// One loan on one path, written out as ordinary code: the same sums as the
/// graph, to check the graph against. Gives what is set aside, the lifetime
/// loss, and what is owed if the loan is looking shakier.
fn by_hand(loan: &Loan, path: &[f64], m: &Model) -> [f64; OUTPUTS] {
    let (mut owed, mut there, mut never, mut loss) = (loan.owed, 1.0, 1.0, 0.0);
    let mut prices = 1.0;
    let mut first_year = 0.0;
    let first_rate = if loan.variable {
        (USUAL_RATE + loan.rate) / 4.0
    } else {
        loan.rate / 4.0
    };
    let usual_payment = payment(loan.owed, first_rate, loan.left);
    for t in 0..loan.left {
        let (unemployment, house_prices, rate) = (path[3 * t], path[3 * t + 1], path[3 * t + 2]);
        prices *= 1.0 + house_prices;
        let (shock, next) = if loan.variable {
            let r = (rate + loan.rate) / 4.0;
            let pay = owed * r / (1.0 - (1.0 + r).powf(-((loan.left - t) as f64)));
            ((pay / usual_payment - 1.0).max(0.0), owed * (1.0 + r) - pay)
        } else {
            (0.0, owed * (1.0 + first_rate) - usual_payment)
        };
        let x = loan.risk
            + m.unemployment * (unemployment - USUAL_UNEMPLOYMENT)
            + m.loan_to_value * (owed / (loan.house * prices) - 0.7)
            + m.payment_shock * shock;
        let defaults = 1.0 / (1.0 + (-x).exp());
        let early = if loan.variable {
            m.repaid_early
        } else {
            (m.repaid_early + m.refinancing * (loan.rate - rate - 0.015).max(0.0) / 4.0).min(0.15)
        };
        let lost = (owed - m.recovered * loan.house * prices).max(m.least_lost * owed);
        loss += there * defaults * lost * (1.0 + first_rate).powf(-(t as f64 + 1.0));
        never *= 1.0 - defaults;
        there = there * (1.0 - defaults) * (1.0 - early);
        owed = next;
        if t == 3.min(loan.left - 1) {
            first_year = loss;
        }
    }
    let shaky = 1.0 - never > m.shakier * loan.looked;
    [
        if shaky { loss } else { first_year },
        loss,
        if shaky { loan.owed } else { 0.0 },
    ]
}

// ---- The book as a graph ----

struct Book {
    g: Graph,
    /// Unemployment, the change in house prices and the interest rate, a
    /// quarter after another: what a path sets.
    economy: Vec<Input<f64>>,
    knobs: Vec<Input<f64>>,
    /// Each loan's house's value, which can be changed on the graph.
    houses: Vec<Input<f64>>,
    each: Vec<[Node<f64>; OUTPUTS]>,
    loan_quarters: usize,
}

impl Book {
    fn new(loans: &[Loan], model: &Model) -> Result<Book> {
        let mut g = Graph::new();
        let names = [
            "unemployment",
            "loan_to_value",
            "payment_shock",
            "recovered",
            "least_lost",
            "repaid_early",
            "refinancing",
            "shakier",
        ];
        let knobs: Vec<Input<f64>> = names
            .iter()
            .zip(model.values())
            .map(|(name, x)| g.input(&format!("model_{name}"), x))
            .collect::<std::result::Result<_, _>>()?;
        let usual = [USUAL_UNEMPLOYMENT, 0.006, USUAL_RATE];
        let economy: Vec<Input<f64>> = (0..3 * QUARTERS)
            .map(|k| g.input(&format!("economy{k}"), usual[k % 3]))
            .collect::<std::result::Result<_, _>>()?;
        // House prices so far, which every loan reads.
        let mut prices: Vec<Node<f64>> = Vec::with_capacity(QUARTERS);
        for t in 0..QUARTERS {
            let change = 1.0 + economy[3 * t + 1];
            prices.push(match prices.last() {
                Some(&so_far) => g.define(&format!("prices{t}"), so_far * change)?,
                None => g.define(&format!("prices{t}"), change)?,
            });
        }
        let mut book = Book {
            g,
            economy,
            knobs,
            houses: Vec::with_capacity(loans.len()),
            each: Vec::with_capacity(loans.len()),
            loan_quarters: 0,
        };
        for (k, loan) in loans.iter().enumerate() {
            book.project(k, loan, &prices)?;
        }
        book.g.stabilize();
        Ok(book)
    }

    /// One loan, a quarter at a time until it is paid off: the same sums as
    /// [`by_hand`].
    fn project(&mut self, k: usize, loan: &Loan, prices: &[Node<f64>]) -> Result<()> {
        let Book {
            g, economy, knobs, ..
        } = self;
        let [
            unemployment,
            loan_to_value,
            payment_shock,
            recovered,
            least_lost,
            repaid_early,
            refinancing,
            shakier,
        ] = knobs[..]
        else {
            unreachable!()
        };
        let house = g.input(&format!("house{k}"), loan.house)?;
        let first_rate = if loan.variable {
            (USUAL_RATE + loan.rate) / 4.0
        } else {
            loan.rate / 4.0
        };
        let usual_payment = payment(loan.owed, first_rate, loan.left);

        let mut owed: Expr<f64> = loan.owed.into_expr();
        // A fixed loan's balance is known in advance.
        let mut fixed_owed = loan.owed;
        let mut there: Expr<f64> = 1.0.into_expr();
        let mut never: Expr<f64> = 1.0.into_expr();
        let mut loss: Option<Node<f64>> = None;
        let mut first_year = None;
        for t in 0..loan.left {
            let (jobless, rate) = (economy[3 * t], economy[3 * t + 2]);
            let worth = house * prices[t];
            let (shock, next): (Expr<f64>, Expr<f64>) = if loan.variable {
                let r = (rate + loan.rate) / 4.0;
                let pay = owed.clone() * r.clone()
                    / (1.0 - (1.0 + r.clone()).pow(-((loan.left - t) as f64)));
                let pay = g.define(&format!("l{k}_pay{t}"), pay)?;
                let next = g.define(
                    &format!("l{k}_owed{}", t + 1),
                    owed.clone() * (1.0 + r) - pay,
                )?;
                ((pay / usual_payment - 1.0).max(0.0), next.into_expr())
            } else {
                fixed_owed = fixed_owed * (1.0 + first_rate) - usual_payment;
                (0.0.into_expr(), fixed_owed.into_expr())
            };
            let x = loan.risk
                + unemployment * (jobless - USUAL_UNEMPLOYMENT)
                + loan_to_value * (owed.clone() / worth.clone() - 0.7)
                + payment_shock * shock;
            let defaults = g.define(&format!("l{k}_defaults{t}"), 1.0 / (1.0 + (-x).exp()))?;
            let early: Expr<f64> = if loan.variable {
                repaid_early.into_expr()
            } else {
                (repaid_early + refinancing * (loan.rate - rate - 0.015).max(0.0) / 4.0).min(0.15)
            };
            let lost = (owed.clone() - recovered * worth).max(least_lost * owed.clone());
            let discount = (1.0 + first_rate).powf(-(t as f64 + 1.0));
            let this = there.clone() * defaults * lost * discount;
            let so_far = match loss {
                Some(so_far) => g.define(&format!("l{k}_loss{t}"), so_far + this)?,
                None => g.define(&format!("l{k}_loss{t}"), this)?,
            };
            loss = Some(so_far);
            never = g
                .define(&format!("l{k}_never{t}"), never * (1.0 - defaults))?
                .into_expr();
            there = g
                .define(
                    &format!("l{k}_there{}", t + 1),
                    there * (1.0 - defaults) * (1.0 - early),
                )?
                .into_expr();
            owed = next;
            if t == 3.min(loan.left - 1) {
                first_year = Some(so_far);
            }
        }
        let (loss, first_year) = (loss.expect("a quarter"), first_year.expect("a quarter"));
        let shaky = g.define(
            &format!("l{k}_shaky"),
            (1.0 - never).gt(shakier * loan.looked),
        )?;
        let provision = g.define(
            &format!("l{k}_provision"),
            when(shaky).then(loss).otherwise(first_year),
        )?;
        let shaky_owed = g.define(
            &format!("l{k}_shaky_owed"),
            when(shaky).then(loan.owed).otherwise(0.0),
        )?;
        self.houses.push(house);
        self.each.push([provision, loss, shaky_owed]);
        self.loan_quarters += loan.left;
        Ok(())
    }

    /// One node for each output, adding up every loan.
    fn totals(&mut self) -> Result<Vec<NodeId>> {
        let mut totals = Vec::new();
        for output in 0..OUTPUTS {
            let mut nodes: Vec<Node<f64>> = self.each.iter().map(|e| e[output]).collect();
            let mut round = 0;
            while nodes.len() > 1 {
                round += 1;
                let mut next = Vec::with_capacity(nodes.len() / 8 + 1);
                for (k, group) in nodes.chunks(8).enumerate() {
                    let mut all = group[0].into_expr();
                    for &node in &group[1..] {
                        all = all + node;
                    }
                    next.push(self.g.define(&format!("total{output}_{round}_{k}"), all)?);
                }
                nodes = next;
            }
            totals.push(nodes[0].id());
        }
        self.g.stabilize();
        Ok(totals)
    }

    fn plan(&self, outputs: &[NodeId]) -> Result<Scenarios> {
        let vary: Vec<NodeId> = self.economy.iter().map(|e| e.id()).collect();
        let shared: Vec<NodeId> = self.knobs.iter().map(|k| k.id()).collect();
        Ok(Scenarios::with_shared(&self.g, &vary, &shared, outputs)?)
    }
}

// ---- The economy ----

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

/// A path of the economy. Unemployment drifts back towards usual, pushed by
/// `slump` (points a quarter, over the first two years) and, where `chance`
/// is 1, by chance: small shocks every quarter and now and then a recession
/// that lasts six. House prices fall when unemployment rises; the interest
/// rate drifts back to usual too, and is cut a little when unemployment
/// rises.
fn economy(slump: f64, chance: f64, rng: &mut Rng) -> Vec<f64> {
    let mut path = Vec::with_capacity(3 * QUARTERS);
    let (mut unemployment, mut rate) = (USUAL_UNEMPLOYMENT, USUAL_RATE);
    let mut recession = 0;
    for t in 0..QUARTERS {
        let mut push = if t < 8 { slump } else { 0.0 };
        if chance > 0.0 {
            if recession == 0 && rng.unit() < 0.02 {
                recession = 6;
            }
            push += 0.4 * rng.normal();
            if recession > 0 {
                recession -= 1;
                push += 0.55;
            }
        }
        let rise = 0.08 * (USUAL_UNEMPLOYMENT - unemployment) + push;
        unemployment = (unemployment + rise).max(2.0);
        let house_prices = 0.006 - 0.02 * rise + chance * 0.012 * rng.normal();
        rate = (rate + 0.08 * (USUAL_RATE - rate) - 0.002 * rise + chance * 0.003 * rng.normal())
            .max(0.001);
        path.extend([unemployment, house_prices, rate]);
    }
    path
}

/// Makes up loan number `k`.
fn loan(k: usize) -> Loan {
    let mut rng = Rng((k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    for _ in 0..4 {
        rng.next();
    }
    let house = (150_000.0 * (0.5 * rng.normal()).exp() + 60_000.0).round();
    let owed = (house * rng.between(0.25, 0.95)).round();
    let variable = rng.unit() < 0.4;
    let mut loan = Loan {
        owed,
        rate: if variable {
            rng.between(0.01, 0.025)
        } else {
            rng.between(0.025, 0.065)
        },
        variable,
        left: 8 + (rng.unit() * (QUARTERS - 8) as f64) as usize,
        house,
        // From 1 in 2,000 to 1 in 70 a quarter.
        risk: -rng.between(4.2, 7.6),
        looked: 1.0,
    };
    // How it looked when it was made: its chance of default over its life
    // in usual times, give or take, so that some loans have got better and
    // some worse.
    let per_quarter = 1.0 / (1.0 + (-loan.risk).exp());
    let never = (1.0 - per_quarter).powf(loan.left as f64);
    loan.looked = (1.0 - never) * rng.between(0.35, 1.6);
    loan
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

/// The same to about twelve figures: powers and exponentials are not
/// rounded alike by the GPU and the CPU.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let number = |arg: Option<String>, otherwise: usize| match arg {
        Some(n) => n.replace('_', "").parse().expect("a number"),
        None => otherwise,
    };
    let size = number(args.next(), 20_000);
    let scenarios = number(args.next(), 10_000);
    let model = Model::usual();
    let runner = Runner::new()?;
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());

    // The economy: the bank's own three scenarios, a regulator's slump, and
    // every path chance could take it down.
    let mut quiet = Rng(1);
    let central = economy(0.0, 0.0, &mut quiet);
    let better = economy(-0.12, 0.0, &mut quiet);
    let worse = economy(0.35, 0.0, &mut quiet);
    let slump = economy(0.75, 0.0, &mut quiet);
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let paths: Vec<f64> = (0..scenarios)
        .flat_map(|_| economy(0.0, 1.0, &mut rng))
        .collect();

    let loans: Vec<Loan> = (0..size).map(loan).collect();

    // ---- 1. Is it right? ----
    println!("1. The graph against the same sums written out by hand");
    println!();
    let few = &loans[..size.min(200)];
    let mut small = Book::new(few, &model)?;
    let each: Vec<NodeId> = small.each.iter().flatten().map(|n| n.id()).collect();
    let plan = small.plan(&each)?;
    let some = &paths[..scenarios.min(50) * 3 * QUARTERS];
    let got = runner.run(&plan, some)?;
    let mut worst = 0.0f64;
    for (path, got) in some
        .chunks(3 * QUARTERS)
        .zip(got.chunks(few.len() * OUTPUTS))
    {
        for (loan, got) in few.iter().zip(got.chunks(OUTPUTS)) {
            let want = by_hand(loan, path, &model);
            for (g, w) in got.iter().zip(want) {
                worst = worst.max((g - w).abs() / loan.owed);
            }
        }
    }
    println!(
        "  {} loans on {} paths, three answers each: largest difference {worst:.1e} of what the loan owes",
        few.len(),
        some.len() / (3 * QUARTERS)
    );
    assert!(worst < 1e-9, "the graph and the sums by hand differ");
    let _ = small.totals()?;
    println!();

    // ---- 2. The whole book ----
    println!("2. The whole book");
    println!();
    let started = Instant::now();
    let mut book = Book::new(&loans, &model)?;
    let totals = book.totals()?;
    let graph_took = started.elapsed();
    let started = Instant::now();
    let mut plan = book.plan(&totals)?;
    let owed: f64 = loans.iter().map(|l| l.owed).sum();
    println!(
        "  {} loans owing {}, {} quarters of loan to project",
        commas(size as f64),
        commas(owed),
        commas(book.loan_quarters as f64)
    );
    println!(
        "  the graph: {} nodes in {} levels (built in {}; plan made in {})",
        commas(plan.nodes() as f64),
        commas(plan.levels() as f64),
        time(graph_took),
        time(started.elapsed())
    );
    let started = Instant::now();
    let results = runner.run(&plan, &paths)?;
    let took = started.elapsed();
    let projected = book.loan_quarters as f64 * scenarios as f64;
    println!(
        "  {}: {} paths of the economy in {}: {} quarters of loan a second",
        runner.name(),
        commas(scenarios as f64),
        time(took),
        commas(projected / took.as_secs_f64())
    );
    let part = scenarios.min(64);
    let started = Instant::now();
    let on_cpu = plan.run_cpu(&paths[..part * 3 * QUARTERS]);
    let cpu_each = started.elapsed() / part as u32;
    println!(
        "  the plan on {cores} CPU cores: {} a path, so about {} for all of them",
        time(cpu_each),
        time(cpu_each * scenarios as u32)
    );
    assert!(
        on_cpu.iter().zip(&results).all(|(a, b)| close(*a, *b)),
        "the CPU and the {} differ",
        runner.name()
    );
    // The sums by hand, on every core: what a program written for this one
    // model would do.
    let part = scenarios.min(32);
    let started = Instant::now();
    let by_hand_totals: Vec<f64> = paths[..part * 3 * QUARTERS]
        .par_chunks(3 * QUARTERS)
        .map(|path| {
            loans
                .iter()
                .map(|l| by_hand(l, path, &model)[PROVISION])
                .sum()
        })
        .collect();
    let hand_each = started.elapsed() / part as u32;
    println!(
        "  written out by hand, on {cores} cores: {} a path, so about {} for all of them",
        time(hand_each),
        time(hand_each * scenarios as u32)
    );
    for (hand, plan) in by_hand_totals.iter().zip(results.chunks(OUTPUTS)) {
        assert!(
            (hand - plan[PROVISION]).abs() <= 1e-9 * owed,
            "the book's total differs"
        );
    }
    println!();

    // ---- 3. What it says ----
    let column =
        |results: &[f64], o: usize| -> Vec<f64> { results.chunks(OUTPUTS).map(|r| r[o]).collect() };
    let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
    let official: Vec<f64> = [&central, &better, &worse, &slump]
        .into_iter()
        .flatten()
        .copied()
        .collect();
    let named = plan.run_cpu(&official);
    let weighted =
        |o: usize| 0.5 * named[o] + 0.25 * named[OUTPUTS + o] + 0.25 * named[2 * OUTPUTS + o];
    let mut provisions = column(&results, PROVISION);
    let average = mean(&provisions);
    provisions.sort_by(f64::total_cmp);
    let at = |share: f64| provisions[((scenarios as f64 * share) as usize).min(scenarios - 1)];
    println!(
        "3. What to set aside (as a share of what is owed, and how much of the book looks shaky)"
    );
    println!();
    let line = |what: &str, provision: f64, shaky: f64| {
        println!(
            "  {what:<46} {:>14} {:>7.3}% {:>9.1}%",
            commas(provision),
            100.0 * provision / owed,
            100.0 * shaky / owed
        );
    };
    println!(
        "  {:<46} {:>14} {:>8} {:>10}",
        "", "set aside", "of owed", "shaky"
    );
    line(
        "the bank's central scenario",
        named[PROVISION],
        named[SHAKY],
    );
    line(
        "its better scenario",
        named[OUTPUTS],
        named[OUTPUTS + SHAKY],
    );
    line(
        "its worse scenario",
        named[2 * OUTPUTS],
        named[2 * OUTPUTS + SHAKY],
    );
    line(
        "the three, weighted a half and two quarters",
        weighted(PROVISION),
        weighted(SHAKY),
    );
    line(
        &format!("the average of {} paths", commas(scenarios as f64)),
        average,
        mean(&column(&results, SHAKY)),
    );
    println!(
        "  (over every loan's whole life the average path loses {}, {:.3}% of what is owed)",
        commas(mean(&column(&results, LIFETIME))),
        100.0 * mean(&column(&results, LIFETIME)) / owed
    );
    println!();
    println!(
        "  across the paths: half need less than {}, 1 in 20 needs more than {}, 1 in 100 more than {}",
        commas(at(0.5)),
        commas(at(0.95)),
        commas(at(0.99))
    );
    let beyond = provisions.partition_point(|&x| x <= named[3 * OUTPUTS]);
    println!(
        "  the regulator's slump needs {}; {:.1}% of the paths are worse than it",
        commas(named[3 * OUTPUTS]),
        100.0 * (scenarios - beyond) as f64 / scenarios as f64
    );
    println!();

    // ---- 4. If the model is wrong ----
    println!("4. If the model is wrong: each is the whole book on every path again");
    println!();
    println!(
        "  {:<58} {:>14} {:>10} {:>8}",
        "", "average", "change", "took"
    );
    type Change = (&'static str, fn(&mut Model));
    let changes: [Change; 5] = [
        ("defaults respond a quarter more to unemployment", |m| {
            m.unemployment *= 1.25
        }),
        ("defaults respond a quarter more to loan-to-value", |m| {
            m.loan_to_value *= 1.25
        }),
        (
            "repossessed houses fetch 65% of their value, not 75%",
            |m| m.recovered = 0.65,
        ),
        ("half as much early repayment", |m| {
            m.repaid_early /= 2.0;
            m.refinancing /= 2.0;
        }),
        ("shaky at 1.5 times how it looked, not 2", |m| {
            m.shakier = 1.5
        }),
    ];
    for (what, change) in changes {
        let mut changed = model.clone();
        change(&mut changed);
        let started = Instant::now();
        plan.set_shared(&changed.values());
        let again = mean(&column(&runner.run(&plan, &paths)?, PROVISION));
        println!(
            "  {what:<58} {:>14} {:>+9.1}% {:>8}",
            commas(again),
            100.0 * (again / average - 1.0),
            time(started.elapsed())
        );
    }
    plan.set_shared(&model.values());
    println!();

    // ---- 5. One thing changes ----
    println!("5. On the graph itself, when one thing changes");
    println!();
    let nodes = plan.nodes();
    let started = Instant::now();
    book.g.set(book.houses[0], loans[0].house * 0.8)?;
    let one = book.g.stabilize();
    println!(
        "  one house is revalued 20% lower: {} of {} nodes worked out again, in {}",
        commas(one.recalculated as f64),
        commas(nodes as f64),
        time(started.elapsed())
    );
    let started = Instant::now();
    book.g.set(book.economy[3 * 80], USUAL_UNEMPLOYMENT + 2.0)?;
    let late = book.g.stabilize();
    println!(
        "  unemployment is forecast 2 points higher in one quarter, twenty years out: {} nodes, in {}",
        commas(late.recalculated as f64),
        time(started.elapsed())
    );
    let started = Instant::now();
    book.g.set(book.economy[0], USUAL_UNEMPLOYMENT + 2.0)?;
    let all = book.g.stabilize();
    println!(
        "  unemployment is 2 points higher next quarter: {} nodes, in {}",
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
