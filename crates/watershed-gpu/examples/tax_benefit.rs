//! Who gains and who loses when a tax or benefit rule changes?
//!
//! The graph is a made-up country's rules for one household, written as a
//! government department would write them down:
//!
//!   - income tax on each adult, with an allowance that is taken away from
//!     high earners, and three bands;
//!   - social insurance on wages and on profit from self-employment;
//!   - a state pension from pension age;
//!   - a payment for each child, withdrawn from high earners;
//!   - income support: an amount for the adults, for children (up to a limit),
//!     for disability, for rent (up to a cap) and for childcare (if every
//!     adult works), reduced as the household earns more, refused above an
//!     amount of savings, and capped in total for those who earn little;
//!   - for pensioners, a top-up to a guaranteed income instead.
//!
//! These are about seventy formulas, no two alike, full of thresholds and
//! conditions. There is no table to lay them out in: a graph is the natural
//! shape for them.
//!
//! A scenario is one household. The rules' numbers (rates, thresholds,
//! amounts) are *shared* inputs: the same for every household of a run, and
//! replaced between runs. So a reform is one call to `set_shared` and one
//! more run over the whole population.
//!
//!     cargo run --release --features cuda --example tax_benefit
//!
//! or choose the number of households:
//!
//!     cargo run --release --features cuda --example tax_benefit -- 1000000
//!
//! The rules look a little like the United Kingdom's but every number is
//! made up, and so are the households. The shape is not.

use std::time::{Duration, Instant};

use rayon::prelude::*;
use watershed_core::NodeId;
use watershed_core::prelude::*;
use watershed_gpu::Scenarios;

/// How many real households the made-up ones stand for, when a cost is given
/// for the whole country.
const COUNTRY: f64 = 28e6;

// ---- The rules' numbers ----

/// Declares the policy once: its numbers ([`Policy`]) and the graph inputs
/// that hold them ([`Params`]), in the same order.
macro_rules! policy {
    ($($(#[$doc:meta])* $name:ident = $value:expr;)*) => {
        /// The numbers in the rules. Money is by the year.
        #[derive(Clone)]
        struct Policy { $($(#[$doc])* $name: f64),* }

        impl Default for Policy {
            fn default() -> Policy { Policy { $($name: $value),* } }
        }

        impl Policy {
            fn values(&self) -> Vec<f64> { vec![$(self.$name),*] }
        }

        /// The graph's inputs for the policy's numbers.
        struct Params { $($name: Input<f64>),* }

        impl Params {
            fn new(g: &mut Graph, policy: &Policy) -> Result<Params, CalcError> {
                Ok(Params { $($name: g.input(stringify!($name), policy.$name)?),* })
            }
            fn ids(&self) -> Vec<NodeId> { vec![$(self.$name.id()),*] }
        }
    };
}

policy! {
    /// Income on which no tax is paid.
    allowance = 12_500.0;
    /// Above this income the allowance is taken away, 1 for every 2.
    allowance_goes_from = 100_000.0;
    /// How much taxed income is in the basic band.
    basic_band = 37_500.0;
    /// Taxed income above this is in the top band.
    top_from = 112_500.0;
    basic_rate = 0.20;
    higher_rate = 0.40;
    top_rate = 0.45;
    /// Interest on savings, and how much of it is not taxed.
    interest = 0.03;
    interest_untaxed = 1_000.0;

    /// Social insurance: on wages between `insurance_from` and
    /// `insurance_upper` at the main rate, above at the upper rate.
    insurance_from = 12_500.0;
    insurance_upper = 50_000.0;
    insurance_rate = 0.08;
    insurance_upper_rate = 0.02;
    /// The rate on profit from self-employment.
    insurance_own_rate = 0.06;

    pension_age = 67.0;
    state_pension = 11_500.0;

    /// For each child, withdrawn evenly as the higher earner's income goes
    /// from `child_withdrawn_from` to `child_withdrawn_over` more.
    child_payment = 1_300.0;
    child_withdrawn_from = 60_000.0;
    child_withdrawn_over = 20_000.0;

    support_single = 4_700.0;
    support_couple = 7_400.0;
    support_child = 3_400.0;
    /// Children after this many add nothing to income support.
    support_child_limit = 2.0;
    support_disabled = 5_000.0;
    rent_cap_single = 6_000.0;
    rent_cap_family = 9_600.0;
    /// The share of childcare costs paid, of costs up to the cap.
    childcare_share = 0.85;
    childcare_cap = 12_000.0;
    /// What a household with children or a disability earns before support
    /// starts to be withdrawn.
    work_allowance = 4_800.0;
    /// Support lost for each 1 earned (after tax) above the allowance.
    taper = 0.55;
    /// Savings below this are ignored; above it each 1 counts as
    /// `savings_count_as` of income; above the limit there is no support.
    savings_ignored = 6_000.0;
    savings_count_as = 0.208;
    savings_limit = 16_000.0;
    /// The most support and child payment together, for a household that
    /// earns less than `cap_earning_less_than` and has no disability.
    cap = 22_000.0;
    cap_earning_less_than = 9_000.0;

    /// What pensioners are topped up to.
    guarantee_single = 11_800.0;
    guarantee_couple = 18_000.0;
}

// ---- A household ----

/// What is known about a household: one scenario's inputs, in this order.
const FACTS: [&str; 11] = [
    "adults",
    "children",
    "age",
    "wages1",
    "wages2",
    "profit",
    "pension",
    "savings",
    "rent",
    "childcare",
    "disabled",
];
const ADULTS: usize = 0;
const CHILDREN: usize = 1;
const AGE: usize = 2;
const WAGES1: usize = 3;
const WAGES2: usize = 4;
const PROFIT: usize = 5;
const PENSION: usize = 6;
const SAVINGS: usize = 7;
const RENT: usize = 8;
const CHILDCARE: usize = 9;
const DISABLED: usize = 10;

/// What is worked out for a household, in this order.
const NET: usize = 0;
const TAX: usize = 1;
const INSURANCE: usize = 2;
const CHILD_PAYMENT: usize = 3;
const SUPPORT: usize = 4;
const OUTPUTS: usize = 5;

/// Income tax on one adult's income.
fn income_tax(
    g: &mut Graph,
    who: &str,
    income: Node<f64>,
    p: &Params,
) -> Result<Node<f64>, CalcError> {
    let allowance = g.define(
        &format!("allowance_{who}"),
        (p.allowance - (income - p.allowance_goes_from).max(0.0) / 2.0).max(0.0),
    )?;
    let taxed = g.define(&format!("taxed_{who}"), (income - allowance).max(0.0))?;
    let basic = g.define(
        &format!("tax_basic_{who}"),
        taxed.min(p.basic_band) * p.basic_rate,
    )?;
    let higher = g.define(
        &format!("tax_higher_{who}"),
        (taxed.min(p.top_from) - p.basic_band).max(0.0) * p.higher_rate,
    )?;
    let top = g.define(
        &format!("tax_top_{who}"),
        (taxed - p.top_from).max(0.0) * p.top_rate,
    )?;
    g.define(&format!("tax_{who}"), basic + higher + top)
}

/// Social insurance on one adult's wages.
fn insurance_on_wages(
    g: &mut Graph,
    who: &str,
    wages: Input<f64>,
    p: &Params,
) -> Result<Node<f64>, CalcError> {
    g.define(
        &format!("insurance_wages_{who}"),
        (wages.min(p.insurance_upper) - p.insurance_from).max(0.0) * p.insurance_rate
            + (wages - p.insurance_upper).max(0.0) * p.insurance_upper_rate,
    )
}

/// The inputs for a household's facts, and the outputs.
type Rules = (Vec<Input<f64>>, Vec<Node<f64>>);

/// The rules for one household. Gives the inputs for the household's facts
/// and the outputs, in the orders above.
fn rules(g: &mut Graph, p: &Params) -> Result<Rules, CalcError> {
    let facts: Vec<Input<f64>> = FACTS
        .iter()
        .map(|name| g.input(name, 0.0))
        .collect::<Result<_, _>>()?;
    let [
        adults,
        children,
        age,
        wages1,
        wages2,
        profit,
        pension,
        savings,
        rent,
        childcare,
        disabled,
    ] = facts[..]
    else {
        unreachable!()
    };

    let couple = g.define("couple", adults.gt(1.5))?;
    let has_children = g.define("has_children", children.gt(0.5))?;
    let is_disabled = g.define("is_disabled", disabled.gt(0.5))?;
    let pensioner = g.define("pensioner", age.ge(p.pension_age))?;

    // What comes in before the state takes or gives anything.
    let state_pension1 = g.define(
        "state_pension1",
        when(pensioner).then(p.state_pension).otherwise(0.0),
    )?;
    let state_pension2 = g.define(
        "state_pension2",
        when(pensioner & couple)
            .then(p.state_pension)
            .otherwise(0.0),
    )?;
    let interest = g.define("interest_earned", savings * p.interest)?;
    let earnings = g.define("earnings", wages1 + wages2 + profit)?;

    // Income tax, an adult at a time. The first adult has everything that is
    // not the second's wages or state pension.
    let income1 = g.define(
        "income1",
        wages1 + profit + pension + state_pension1 + (interest - p.interest_untaxed).max(0.0),
    )?;
    let income2 = g.define("income2", wages2 + state_pension2)?;
    let tax1 = income_tax(g, "1", income1, p)?;
    let tax2 = income_tax(g, "2", income2, p)?;
    let tax = g.define("tax", tax1 + tax2)?;

    // Social insurance, which stops at pension age.
    let on_wages1 = insurance_on_wages(g, "1", wages1, p)?;
    let on_wages2 = insurance_on_wages(g, "2", wages2, p)?;
    let on_profit = g.define(
        "insurance_profit",
        (profit.min(p.insurance_upper) - p.insurance_from).max(0.0) * p.insurance_own_rate,
    )?;
    let insurance = g.define(
        "insurance",
        when(pensioner)
            .then(0.0)
            .otherwise(on_wages1 + on_wages2 + on_profit),
    )?;

    // The payment for children.
    let higher_income = g.define("higher_income", income1.max(income2))?;
    let withdrawn = g.define(
        "child_payment_withdrawn",
        ((higher_income - p.child_withdrawn_from).max(0.0) / p.child_withdrawn_over).min(1.0),
    )?;
    let child_payment = g.define(
        "child_payment_paid",
        children * p.child_payment * (1.0 - withdrawn),
    )?;

    // Income support: what the household is thought to need...
    let for_adults = g.define(
        "support_adults",
        when(couple)
            .then(p.support_couple)
            .otherwise(p.support_single),
    )?;
    let for_children = g.define(
        "support_children",
        children.min(p.support_child_limit) * p.support_child,
    )?;
    let for_disability = g.define(
        "support_disability",
        when(is_disabled).then(p.support_disabled).otherwise(0.0),
    )?;
    let rent_cap = g.define(
        "rent_cap",
        when(couple | has_children)
            .then(p.rent_cap_family)
            .otherwise(p.rent_cap_single),
    )?;
    let for_rent = g.define("support_rent", rent.min(rent_cap))?;
    let everyone_works = g.define(
        "everyone_works",
        (wages1 + profit).gt(0.0) & (!couple | wages2.gt(0.0)),
    )?;
    let for_childcare = g.define(
        "support_childcare",
        when(everyone_works & has_children)
            .then(childcare.min(p.childcare_cap) * p.childcare_share)
            .otherwise(0.0),
    )?;
    let needs = g.define(
        "support_needs",
        for_adults + for_children + for_disability + for_rent + for_childcare,
    )?;

    // ...less what it has.
    let earnings_kept = g.define("earnings_kept", (earnings - tax - insurance).max(0.0))?;
    let allowed = g.define(
        "work_allowance_here",
        when(has_children | is_disabled)
            .then(p.work_allowance)
            .otherwise(0.0),
    )?;
    let from_savings = g.define(
        "income_from_savings",
        (savings - p.savings_ignored).max(0.0) * p.savings_count_as,
    )?;
    let has = g.define(
        "support_has",
        (earnings_kept - allowed).max(0.0) * p.taper + pension + from_savings,
    )?;
    let uncapped = g.define(
        "support_uncapped",
        when(savings.gt(p.savings_limit))
            .then(0.0)
            .otherwise((needs - has).max(0.0)),
    )?;
    let is_capped = g.define(
        "is_capped",
        !is_disabled & earnings.lt(p.cap_earning_less_than),
    )?;
    let working_age_support = g.define(
        "support_working_age",
        when(is_capped)
            .then(uncapped.min((p.cap - child_payment).max(0.0)))
            .otherwise(uncapped),
    )?;

    // Pensioners are topped up to a guaranteed income instead.
    let guarantee = g.define(
        "guarantee",
        when(couple)
            .then(p.guarantee_couple)
            .otherwise(p.guarantee_single),
    )?;
    let top_up = g.define(
        "pension_top_up",
        (guarantee + for_rent
            - state_pension1
            - state_pension2
            - pension
            - from_savings
            - earnings_kept)
            .max(0.0),
    )?;
    let support = g.define(
        "support",
        when(pensioner).then(top_up).otherwise(working_age_support),
    )?;

    let net = g.define(
        "net",
        earnings + pension + interest + state_pension1 + state_pension2 - tax - insurance
            + child_payment
            + support,
    )?;
    Ok((facts, vec![net, tax, insurance, child_payment, support]))
}

// ---- The households ----

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
    /// An amount of money: half are below `middle`, a few are far above.
    fn amount(&mut self, middle: f64, spread: f64) -> f64 {
        (middle * (spread * self.normal()).exp()).round()
    }
}

/// Makes up household number `k`.
fn household(k: usize, h: &mut [f64]) {
    let mut rng = Rng((k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    for _ in 0..4 {
        rng.next();
    }
    h.fill(0.0);
    let old = rng.chance(0.22);
    let couple = rng.chance(0.55);
    h[ADULTS] = if couple { 2.0 } else { 1.0 };
    if rng.chance(0.38) {
        h[RENT] = rng.between(4_000.0, 16_000.0).round();
    }
    if rng.chance(0.5) {
        h[SAVINGS] = rng.between(0.0, 2_000.0).round();
    } else {
        h[SAVINGS] = rng.amount(if old { 40_000.0 } else { 15_000.0 }, 1.3);
    }
    if old {
        h[AGE] = rng.between(67.0, 92.0).floor();
        if rng.chance(0.7) {
            h[PENSION] = rng.amount(9_000.0, 0.9);
        }
        if rng.chance(0.1) {
            h[WAGES1] = rng.amount(12_000.0, 0.6);
        }
        h[DISABLED] = f64::from(rng.chance(0.2));
        return;
    }
    h[AGE] = rng.between(20.0, 67.0).floor();
    h[DISABLED] = f64::from(rng.chance(0.09));
    if rng.chance(0.4) {
        h[CHILDREN] = match rng.unit() {
            x if x < 0.43 => 1.0,
            x if x < 0.80 => 2.0,
            x if x < 0.93 => 3.0,
            x if x < 0.98 => 4.0,
            _ => 5.0,
        };
    }
    // Someone with a disability, or alone with children, works less often.
    let out_of_work = match (h[DISABLED] > 0.0, !couple && h[CHILDREN] > 0.0) {
        (true, _) => 0.5,
        (false, true) => 0.3,
        (false, false) => 0.08,
    };
    if !rng.chance(out_of_work) {
        if rng.chance(0.12) {
            h[PROFIT] = rng.amount(20_000.0, 0.8);
        } else {
            h[WAGES1] = rng.amount(31_000.0, 0.65);
        }
    }
    if couple && rng.chance(0.7) {
        h[WAGES2] = rng.amount(24_000.0, 0.6);
    }
    if h[CHILDREN] > 0.0 && rng.chance(0.35) {
        h[CHILDCARE] = rng.between(2_000.0, 14_000.0).round();
    }
}

/// How many single adults a household's income has to stretch as far as.
fn size(h: &[f64]) -> f64 {
    1.0 + 0.5 * (h[ADULTS] - 1.0) + 0.3 * h[CHILDREN]
}

// ---- Running it ----

/// A change to the rules' numbers.
type Reform = Box<dyn Fn(&mut Policy)>;

struct Runner {
    /// Of the last run, how long the GPU spent calculating: the rest was
    /// copying households to it and answers back.
    calculating: std::cell::Cell<Option<Duration>>,
    #[cfg(feature = "cuda")]
    gpu: watershed_gpu::Gpu,
}

impl Runner {
    fn new() -> Result<Runner, Box<dyn std::error::Error>> {
        Ok(Runner {
            calculating: std::cell::Cell::new(None),
            #[cfg(feature = "cuda")]
            gpu: watershed_gpu::Gpu::new()?,
        })
    }

    fn name(&self) -> &'static str {
        if cfg!(feature = "cuda") { "GPU" } else { "CPU" }
    }

    fn run(
        &self,
        plan: &Scenarios,
        inputs: &[f64],
    ) -> Result<Vec<f64>, Box<dyn std::error::Error>> {
        #[cfg(feature = "cuda")]
        {
            let allowed = self.gpu.free_memory()? / 5 * 4;
            let (results, timing) = self.gpu.run_within(plan, inputs, allowed)?;
            self.calculating.set(Some(timing.compute));
            Ok(results)
        }
        #[cfg(not(feature = "cuda"))]
        Ok(plan.run_cpu(inputs))
    }
}

/// The whole population under one policy.
struct Country<'a> {
    households: &'a [f64],
    /// Households in order of income under today's rules, poorest first,
    /// allowing for their size.
    poorest_first: &'a [u32],
    /// Below this income for its size a household is counted as poor.
    poverty_line: f64,
    today: &'a [f64],
}

impl Country<'_> {
    fn count(&self) -> usize {
        self.households.len() / FACTS.len()
    }

    fn facts(&self, k: usize) -> &[f64] {
        &self.households[k * FACTS.len()..(k + 1) * FACTS.len()]
    }

    /// Children in households counted as poor, as a share of all children.
    fn children_in_poverty(&self, results: &[f64]) -> f64 {
        let (poor, all) = (0..self.count())
            .into_par_iter()
            .map(|k| {
                let h = self.facts(k);
                let poor = results[k * OUTPUTS + NET] / size(h) < self.poverty_line;
                (if poor { h[CHILDREN] } else { 0.0 }, h[CHILDREN])
            })
            .reduce(|| (0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
        poor / all
    }

    /// What a change of rules does, against today's.
    fn report(&self, results: &[f64]) {
        let n = self.count();
        let change = |k: usize| results[k * OUTPUTS + NET] - self.today[k * OUTPUTS + NET];
        let (cost, gain, lose) = (0..n)
            .into_par_iter()
            .map(|k| {
                let c = change(k);
                (c, usize::from(c > 1.0), usize::from(c < -1.0))
            })
            .reduce(|| (0.0, 0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
        let billions = cost / n as f64 * COUNTRY / 1e9;
        println!(
            "  {} the government {:.1} billion a year",
            if cost >= 0.0 { "costs" } else { "saves" },
            billions.abs()
        );
        println!(
            "  {:.1}% of households gain, {:.1}% lose",
            100.0 * gain as f64 / n as f64,
            100.0 * lose as f64 / n as f64
        );
        println!(
            "  children in poverty: {:.1}% → {:.1}%",
            100.0 * self.children_in_poverty(self.today),
            100.0 * self.children_in_poverty(results)
        );
        println!("  the average household's change a year, from the poorest tenth to the richest:");
        let tenths: Vec<f64> = self
            .poorest_first
            .chunks(n.div_ceil(10))
            .map(|tenth| {
                tenth.par_iter().map(|&k| change(k as usize)).sum::<f64>() / tenth.len() as f64
            })
            .collect();
        let most = tenths.iter().fold(1.0f64, |m, x| m.max(x.abs()));
        for (t, x) in tenths.iter().enumerate() {
            let bar = "#".repeat((x.abs() / most * 40.0).round() as usize);
            println!("    {:>2} {:>8} {bar}", t + 1, signed(*x));
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let n: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse().expect("a number of households"),
        None if cfg!(feature = "cuda") => 10_000_000,
        None => 1_000_000,
    };

    // The rules.
    let today = Policy::default();
    let mut g = Graph::new();
    let params = Params::new(&mut g, &today)?;
    let (facts, outputs) = rules(&mut g, &params)?;
    g.stabilize();
    let vary: Vec<NodeId> = facts.iter().map(|f| f.id()).collect();
    let outputs_ids: Vec<NodeId> = outputs.iter().map(|o| o.id()).collect();
    let mut plan = Scenarios::with_shared(&g, &vary, &params.ids(), &outputs_ids)?;
    println!(
        "the rules: {} formulas in {} levels, with {} numbers set by policy and {} facts about a household",
        plan.nodes(),
        plan.levels(),
        plan.shared_inputs(),
        plan.inputs()
    );

    // One household by hand: alone, earning 30,000, nothing else. Tax is a
    // fifth of what is over 12,500 and insurance 8% of it; too much is earned
    // for any support.
    let mut one = vec![0.0; FACTS.len()];
    (one[ADULTS], one[AGE], one[WAGES1]) = (1.0, 40.0, 30_000.0);
    let by_plan = plan.run_cpu(&one);
    assert_eq!(by_plan[TAX], 3_500.0);
    assert_eq!(by_plan[INSURANCE], 1_400.0);
    assert_eq!(by_plan[SUPPORT], 0.0);
    assert_eq!(by_plan[NET], 25_100.0);

    // The households.
    let started = Instant::now();
    let mut households = vec![0.0; n * FACTS.len()];
    households
        .par_chunks_mut(FACTS.len())
        .enumerate()
        .for_each(|(k, h)| household(k, h));
    println!(
        "{} households made up in {}",
        commas(n as f64),
        time(started.elapsed())
    );
    println!();

    // Today's rules.
    let runner = Runner::new()?;
    let started = Instant::now();
    let now = runner.run(&plan, &households)?;
    let took = started.elapsed();
    println!(
        "{}: every household under today's rules in {} ({} households a second)",
        runner.name(),
        time(took),
        commas(n as f64 / took.as_secs_f64())
    );
    if let Some(calculating) = runner.calculating.get() {
        println!(
            "  of that, {} was calculating; the rest was copying households to the GPU and answers back",
            time(calculating)
        );
    }

    // The same on the CPU for some, and on the graph itself for fewer: they
    // must agree to the last bit.
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    if cfg!(feature = "cuda") {
        let some = n.min(1_000_000);
        let started = Instant::now();
        let on_cpu = plan.run_cpu(&households[..some * FACTS.len()]);
        let took = started.elapsed();
        println!(
            "CPU, {cores} cores: {} households a second",
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
    for (h, result) in households
        .chunks(FACTS.len())
        .zip(now.chunks(OUTPUTS))
        .take(few)
    {
        for (&fact, &x) in facts.iter().zip(h) {
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
        "the graph, a household at a time: {} households a second",
        commas(few as f64 / took.as_secs_f64())
    );
    println!("checked: all three give the same answers, to the last bit");
    println!();

    // The country as it is.
    let income =
        |k: usize| now[k * OUTPUTS + NET] / size(&households[k * FACTS.len()..][..FACTS.len()]);
    let mut poorest_first: Vec<u32> = (0..n as u32).collect();
    poorest_first.par_sort_unstable_by(|&a, &b| income(a as usize).total_cmp(&income(b as usize)));
    let poverty_line = 0.6 * income(poorest_first[n / 2] as usize);
    let country = Country {
        households: &households,
        poorest_first: &poorest_first,
        poverty_line,
        today: &now,
    };
    let total =
        |o: usize| now.par_chunks(OUTPUTS).map(|r| r[o]).sum::<f64>() / n as f64 * COUNTRY / 1e9;
    println!(
        "today, for a country of {} million households (billions a year)",
        COUNTRY / 1e6
    );
    println!("  income tax       {:>7.1}", total(TAX));
    println!("  social insurance {:>7.1}", total(INSURANCE));
    println!("  child payment    {:>7.1}", total(CHILD_PAYMENT));
    println!("  income support   {:>7.1}", total(SUPPORT));
    println!(
        "  a household is poor below {} a year for a single adult; {:.1}% of children live in one",
        commas(poverty_line),
        100.0 * country.children_in_poverty(&now)
    );
    println!();

    // Reforms: the same households, other numbers in the rules.
    let reforms: [(&str, Reform); 4] = [
        (
            "count every child for income support, not only the first two",
            Box::new(|p| p.support_child_limit = 100.0),
        ),
        (
            "withdraw income support more slowly: 45 for each 100 earned, not 55",
            Box::new(|p| p.taper = 0.45),
        ),
        (
            "raise the tax allowance to 15,000 and the basic rate to 23%",
            Box::new(|p| (p.allowance, p.basic_rate) = (15_000.0, 0.23)),
        ),
        (
            "drop the savings limit: savings count as income but never bar support",
            Box::new(|p| p.savings_limit = f64::INFINITY),
        ),
    ];
    let mut slower_taper = None;
    for (r, (what, change)) in reforms.iter().enumerate() {
        let mut policy = today.clone();
        change(&mut policy);
        let started = Instant::now();
        plan.set_shared(&policy.values());
        let results = runner.run(&plan, &households)?;
        println!("{what}  ({})", time(started.elapsed()));
        country.report(&results);
        println!();
        if r == 1 {
            slower_taper = Some((policy, results));
        }
    }

    // What does a household keep of a raise? Give the first earner 1,000
    // more and see how much better off the household is.
    const RAISE: f64 = 1_000.0;
    let mut raised = households.clone();
    raised
        .par_chunks_mut(FACTS.len())
        .for_each(|h| h[WAGES1] += RAISE);
    let working: Vec<usize> = (0..n)
        .into_par_iter()
        .filter(|&k| {
            let h = country.facts(k);
            h[WAGES1] > 0.0 && h[AGE] < today.pension_age
        })
        .collect();
    let kept = |before: &[f64], after: &[f64]| -> Vec<f64> {
        working
            .par_iter()
            .map(|&k| (after[k * OUTPUTS + NET] - before[k * OUTPUTS + NET]) / RAISE)
            .collect()
    };
    plan.set_shared(&today.values());
    let started = Instant::now();
    let now_raised = runner.run(&plan, &raised)?;
    let took = started.elapsed();
    let kept_now = kept(&now, &now_raised);
    let (policy, results) = slower_taper.expect("the second reform");
    plan.set_shared(&policy.values());
    let kept_then = kept(&results, &runner.run(&plan, &raised)?);
    plan.set_shared(&today.values());

    println!(
        "of a raise of {}, how much are working households better off?  ({} to give everyone the raise)",
        commas(RAISE),
        time(took)
    );
    println!("  {:<28} {:>8} {:>14}", "", "today", "slower taper");
    let share = |kept: &[f64], low: f64, high: f64| {
        100.0 * kept.par_iter().filter(|&&x| x >= low && x < high).count() as f64
            / kept.len() as f64
    };
    for (label, low, high) in [
        ("worse off than before", f64::NEG_INFINITY, 0.0),
        ("keep less than a quarter", 0.0, 0.25),
        ("keep a quarter to a half", 0.25, 0.5),
        ("keep a half to two thirds", 0.5, 2.0 / 3.0),
        ("keep two thirds or more", 2.0 / 3.0, f64::INFINITY),
    ] {
        println!(
            "  {label:<28} {:>7.1}% {:>13.1}%",
            share(&kept_now, low, high),
            share(&kept_then, low, high)
        );
    }
    // The household that does worst out of a raise today.
    let (worst, lost) = working
        .par_iter()
        .zip(&kept_now)
        .map(|(&k, &x)| (k, x))
        .reduce(|| (0, f64::INFINITY), |a, b| if b.1 < a.1 { b } else { a });
    let h = country.facts(worst);
    println!();
    println!(
        "the worst off after a raise today: {} with {} children, wages of {}{}, savings of {}, rent of {}.",
        if h[ADULTS] > 1.0 {
            "a couple"
        } else {
            "one adult"
        },
        h[CHILDREN],
        commas(h[WAGES1]),
        if h[WAGES2] > 0.0 {
            format!(" and {}", commas(h[WAGES2]))
        } else {
            String::new()
        },
        commas(h[SAVINGS]),
        commas(h[RENT]),
    );
    println!(
        "Earning {} more leaves them {} a year {}: income support goes from {} to {} and the child payment from {} to {}.",
        commas(RAISE),
        commas((lost * RAISE).abs()),
        if lost < 0.0 {
            "worse off"
        } else {
            "better off"
        },
        commas(now[worst * OUTPUTS + SUPPORT]),
        commas(now_raised[worst * OUTPUTS + SUPPORT]),
        commas(now[worst * OUTPUTS + CHILD_PAYMENT]),
        commas(now_raised[worst * OUTPUTS + CHILD_PAYMENT]),
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

/// As [`commas`], with a `+` on a gain.
fn signed(x: f64) -> String {
    let s = commas(x);
    if x > 0.0 && s != "0" {
        format!("+{s}")
    } else {
        s
    }
}

fn time(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else {
        format!("{s:.1} s")
    }
}
