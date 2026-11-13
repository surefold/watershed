//! The savings-with-guarantees model of the `liabilities` example, shared
//! with `capital`: lifelib's `savings.CashValue_ME_EX2` as a watershed graph.
//! See `liabilities.rs` for what it is and where its tables come from.

#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use watershed_core::prelude::*;
use watershed_core::{IntoExpr, NodeId};
use watershed_gpu::Scenarios;

/// The longest a policy has left: twenty years.
pub const MONTHS: usize = 240;

/// The fund's yearly growth and how much it swings, on the big run. (The
/// reference's own paths, used for the checks, swing by 3%.)
pub const GROWTH: f64 = 0.02;
pub const SWING: f64 = 0.10;

/// What is worked out for a policy, and added up for the book.
pub const NET: usize = 0;
/// What the promise on death costs: what is paid above the fund.
pub const ON_DEATH: usize = 1;
/// What the promise at the end of the term costs.
pub const AT_END: usize = 2;
pub const FEES: usize = 3;
pub const MEASURES: usize = 4;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

// ---- What the reference gave ----

pub struct Policy {
    /// Pays every month, not once at the start.
    pub level_premium: bool,
    /// Which scale of charges for cashing in early, or 0 for none.
    pub surr_charge_type: usize,
    /// The share of each premium the insurer keeps.
    pub load: f64,
    pub age_at_entry: usize,
    /// In years.
    pub term: usize,
    /// How many policies of this kind.
    pub count: f64,
    /// The amount promised.
    pub sum_assured: f64,
    /// Months since the policy began.
    pub duration: usize,
    pub premium: f64,
    /// The fund for each policy now.
    pub fund: f64,
}

impl Policy {
    pub fn months_left(&self) -> usize {
        12 * self.term - self.duration
    }
}

pub struct Tables {
    /// Chance of dying in a year, by age, then by years since the policy
    /// began (0 to 5).
    pub mortality: Vec<[f64; 6]>,
    /// Charge for cashing in, by year since the policy began (0 to 10), for
    /// each of three scales.
    pub surr_charge: Vec<[f64; 3]>,
    /// Yearly discount rate, by year from now.
    pub disc_rate: Vec<f64>,
}

pub fn rows(path: &Path) -> Result<Vec<Vec<f64>>> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    text.lines()
        .skip(1)
        .map(|line| {
            line.split(',')
                .map(|x| Ok(x.trim().parse::<f64>()?))
                .collect()
        })
        .collect()
}

pub fn numbers(path: &Path) -> Result<Vec<f64>> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(bytes
        .chunks_exact(8)
        .map(|b| f64::from_le_bytes(b.try_into().expect("eight bytes")))
        .collect())
}

pub fn tables(dir: &Path) -> Result<Tables> {
    Ok(Tables {
        mortality: rows(&dir.join("mort_table.csv"))?
            .iter()
            .map(|r| r[1..7].try_into().expect("six columns"))
            .collect(),
        surr_charge: rows(&dir.join("surr_charge_table.csv"))?
            .iter()
            .map(|r| r[1..4].try_into().expect("three columns"))
            .collect(),
        disc_rate: rows(&dir.join("disc_rate_ann.csv"))?
            .iter()
            .map(|r| r[1])
            .collect(),
    })
}

pub fn policies(path: &Path, most: usize) -> Result<Vec<Policy>> {
    Ok(rows(path)?
        .iter()
        .take(most)
        .map(|r| Policy {
            level_premium: r[1] != 0.0,
            surr_charge_type: r[2] as usize,
            load: r[3],
            age_at_entry: r[4] as usize,
            term: r[5] as usize,
            count: r[6],
            sum_assured: r[7],
            duration: r[8] as usize,
            premium: r[9],
            fund: r[10],
        })
        .collect())
}

// ---- The assumptions ----

/// What the insurer assumes: the same on every market path.
#[derive(Clone)]
pub struct Assumptions {
    /// What 1 paid in each month from now is worth today.
    pub discount: Vec<f64>,
    /// The fee taken from the fund each month, as a share of it.
    pub fee: f64,
    /// Deaths, lapses and expenses, as a multiple of the tables'.
    pub mortality: f64,
    pub lapses: f64,
    pub expenses: f64,
    /// 1 if lapses follow how far the fund is above the guarantee, 0 if not.
    pub lapses_follow_fund: f64,
    /// Today's funds, as a multiple of what they are.
    pub fund: f64,
}

impl Assumptions {
    /// The best estimate, with the discount curve moved by `shift` a year.
    pub fn best_estimate(tables: &Tables, shift: f64) -> Assumptions {
        let discount = (0..=MONTHS)
            .map(|t| {
                let monthly = (1.0 + tables.disc_rate[t / 12] + shift).powf(1.0 / 12.0) - 1.0;
                (1.0 + monthly).powi(-(t as i32))
            })
            .collect();
        Assumptions {
            discount,
            fee: 0.01 / 12.0,
            mortality: 1.0,
            lapses: 1.0,
            expenses: 1.0,
            lapses_follow_fund: 1.0,
            fund: 1.0,
        }
    }

    pub fn values(&self) -> Vec<f64> {
        let mut values = self.discount.clone();
        values.extend([
            self.fee,
            self.mortality,
            self.lapses,
            self.expenses,
            self.lapses_follow_fund,
            self.fund,
        ]);
        values
    }
}

/// The graph's inputs for the assumptions.
pub struct Knobs {
    pub discount: Vec<Input<f64>>,
    pub fee: Input<f64>,
    pub mortality: Input<f64>,
    pub lapses: Input<f64>,
    pub expenses: Input<f64>,
    pub lapses_follow_fund: Input<f64>,
    pub fund: Input<f64>,
}

impl Knobs {
    pub fn new(g: &mut Graph, a: &Assumptions) -> Result<Knobs> {
        Ok(Knobs {
            discount: a
                .discount
                .iter()
                .enumerate()
                .map(|(t, &x)| g.input(&format!("discount{t}"), x))
                .collect::<std::result::Result<_, _>>()?,
            fee: g.input("fee", a.fee)?,
            mortality: g.input("mortality", a.mortality)?,
            lapses: g.input("lapses", a.lapses)?,
            expenses: g.input("expenses", a.expenses)?,
            lapses_follow_fund: g.input("lapses_follow_fund", a.lapses_follow_fund)?,
            fund: g.input("fund", a.fund)?,
        })
    }

    /// In the order of [`Assumptions::values`].
    pub fn ids(&self) -> Vec<NodeId> {
        let mut ids: Vec<NodeId> = self.discount.iter().map(|d| d.id()).collect();
        ids.extend([
            self.fee.id(),
            self.mortality.id(),
            self.lapses.id(),
            self.expenses.id(),
            self.lapses_follow_fund.id(),
            self.fund.id(),
        ]);
        ids
    }
}

// ---- The model ----

/// A book of policies as a graph.
pub struct Book {
    pub g: Graph,
    pub knobs: Knobs,
    /// The fund's return in each month: what a market path sets.
    pub returns: Vec<Input<f64>>,
    /// Each policy's promised amount, which can be changed on the graph.
    pub promised: Vec<Input<f64>>,
    /// For each policy, the present value of each measure.
    pub each: Vec<[Node<f64>; MEASURES]>,
    /// Months of policy projected: one for each policy for each month left.
    pub policy_months: usize,
}

impl Book {
    pub fn new(policies: &[Policy], tables: &Tables, assumptions: &Assumptions) -> Result<Book> {
        let mut g = Graph::new();
        let knobs = Knobs::new(&mut g, assumptions)?;
        // With no swings the fund grows by the same each month.
        let steady = (GROWTH / 12.0).exp() - 1.0;
        let returns: Vec<Input<f64>> = (0..MONTHS)
            .map(|t| g.input(&format!("return{t}"), steady))
            .collect::<std::result::Result<_, _>>()?;
        let mut book = Book {
            g,
            knobs,
            returns,
            promised: Vec::with_capacity(policies.len()),
            each: Vec::with_capacity(policies.len()),
            policy_months: 0,
        };
        for (k, policy) in policies.iter().enumerate() {
            book.project(k, policy, tables)?;
        }
        book.g.stabilize();
        Ok(book)
    }

    /// One policy, month by month until its term ends. The names in the
    /// comments are the reference's.
    pub fn project(&mut self, k: usize, p: &Policy, tables: &Tables) -> Result<()> {
        let Book {
            g, knobs, returns, ..
        } = self;
        let months = p.months_left();
        assert!(
            (1..=MONTHS).contains(&months),
            "a policy with {months} months left"
        );
        let promised = g.input(&format!("promised{k}"), p.sum_assured)?;

        // pols_if_at(t, "BEF_MAT") and av_pp_at(t, "BEF_PREM").
        let mut policies: Expr<f64> = (if p.duration > 0 { p.count } else { 0.0 }).into_expr();
        let mut fund = g.define(&format!("p{k}_fund0"), p.fund * knobs.fund)?;
        let mut value: [Option<Node<f64>>; MEASURES] = [None; MEASURES];
        let add = |g: &mut Graph,
                   value: &mut [Option<Node<f64>>; MEASURES],
                   measure: usize,
                   t: usize,
                   flow: Expr<f64>|
         -> Result<()> {
            let name = format!("p{k}_value{measure}_{t}");
            let worth = flow * knobs.discount[t];
            value[measure] = Some(match value[measure] {
                Some(so_far) => g.define(&name, so_far + worth)?,
                None => g.define(&name, worth)?,
            });
            Ok(())
        };

        for (t, &this_return) in returns.iter().enumerate().take(months) {
            let since = p.duration + t;
            let years = since / 12;
            let age = p.age_at_entry + years;
            let dies = tables
                .mortality
                .get(age)
                .map_or(0.0, |row| row[years.min(5)]);
            let dies = 1.0 - (1.0 - dies).powf(1.0 / 12.0);
            let lapses = (0.1 - 0.01 * years as f64).max(0.02);
            let lapses = 1.0 - (1.0 - lapses).powf(1.0 / 12.0);
            let charge = match p.surr_charge_type {
                0 => 0.0,
                scale => tables.surr_charge[years.min(10)][scale - 1],
            };
            let premium = if p.level_premium || since == 0 {
                p.premium
            } else {
                0.0
            };
            let new = if since == 0 { p.count } else { 0.0 };
            let inflation = 1.01f64.powf(t as f64 / 12.0);

            // pols_if_at(t, "BEF_DECR"): no policy ends in these months.
            let before = policies.clone() + new;
            // av_pp_at(t, "BEF_FEE"), "BEF_INV" and "MID_MTH".
            let paid_in = if premium == 0.0 {
                fund
            } else {
                g.define(&format!("p{k}_paid_in{t}"), fund + (1.0 - p.load) * premium)?
            };
            let invested = g.define(&format!("p{k}_invested{t}"), paid_in - knobs.fee * paid_in)?;
            let grew = g.define(&format!("p{k}_grew{t}"), this_return * invested)?;
            let mid = g.define(&format!("p{k}_mid{t}"), invested + 0.5 * grew)?;

            // pols_death and pols_lapse.
            let died = g.define(
                &format!("p{k}_died{t}"),
                before.clone() * (knobs.mortality * dies),
            )?;
            let cash_in = (1.0 - charge) * mid;
            // Exactly `cash_in / promised` when lapses follow the fund, and
            // exactly 1 when they do not.
            let follow =
                knobs.lapses_follow_fund * (cash_in / promised) + (1.0 - knobs.lapses_follow_fund);
            let lapsed = g.define(
                &format!("p{k}_lapsed{t}"),
                (before.clone() - died) * (follow * (knobs.lapses * lapses)).min(1.0),
            )?;
            let left = g.define(&format!("p{k}_left{t}"), before.clone() - lapsed - died)?;
            let next_fund = g.define(&format!("p{k}_fund{}", t + 1), invested + grew)?;

            // The month's cash flows.
            let on_death = promised.max(mid) * died;
            let on_lapse = mid * lapsed - charge * mid * lapsed;
            let premiums = premium * before.clone();
            let expenses =
                (5_000.0 * new + before.clone() * 500.0 / 12.0 * inflation) * knobs.expenses;
            let income = grew * left + 0.5 * grew * (died + lapsed);
            let fund_change = next_fund * left - fund * policies.clone();
            let net = premiums.clone() + income
                - on_death.clone()
                - on_lapse
                - expenses
                - 0.05 * premiums
                - fund_change;
            add(g, &mut value, NET, t, net)?;
            add(g, &mut value, ON_DEATH, t, on_death - mid * died)?;
            add(g, &mut value, FEES, t, knobs.fee * paid_in * before)?;

            policies = left.into_expr();
            fund = next_fund;
        }

        // The term ends: every policy left is paid the fund, or the promised
        // amount if that is more.
        let paid = promised.max(fund) * policies.clone();
        let from_fund = fund * policies;
        add(
            g,
            &mut value,
            AT_END,
            months,
            paid.clone() - from_fund.clone(),
        )?;
        add(g, &mut value, NET, months, from_fund - paid)?;

        self.promised.push(promised);
        self.each
            .push(value.map(|v| v.expect("every measure has a flow")));
        self.policy_months += months + 1;
        Ok(())
    }

    /// One node for each measure, adding up every policy.
    pub fn totals(&mut self) -> Result<[Node<f64>; MEASURES]> {
        let mut totals = Vec::with_capacity(MEASURES);
        for measure in 0..MEASURES {
            let mut nodes: Vec<Node<f64>> = self.each.iter().map(|e| e[measure]).collect();
            let mut round = 0;
            while nodes.len() > 1 {
                round += 1;
                let mut next = Vec::with_capacity(nodes.len() / 8 + 1);
                for (k, group) in nodes.chunks(8).enumerate() {
                    let mut all = group[0].into_expr();
                    for &node in &group[1..] {
                        all = all + node;
                    }
                    next.push(self.g.define(&format!("total{measure}_{round}_{k}"), all)?);
                }
                nodes = next;
            }
            totals.push(nodes[0]);
        }
        self.g.stabilize();
        Ok(totals.try_into().expect("four measures"))
    }

    pub fn plan(&self, outputs: &[NodeId]) -> Result<Scenarios> {
        let vary: Vec<NodeId> = self.returns.iter().map(|r| r.id()).collect();
        Ok(Scenarios::with_shared(
            &self.g,
            &vary,
            &self.knobs.ids(),
            outputs,
        )?)
    }
}

// ---- Running it ----

pub struct Runner {
    #[cfg(feature = "gpu")]
    pub gpu: watershed_gpu::Gpu,
}

impl Runner {
    pub fn new() -> Result<Runner> {
        Ok(Runner {
            #[cfg(feature = "gpu")]
            gpu: watershed_gpu::Gpu::new()?,
        })
    }

    pub fn name(&self) -> &'static str {
        if cfg!(feature = "gpu") { "GPU" } else { "CPU" }
    }

    pub fn run(&self, plan: &Scenarios, inputs: &[f64]) -> Result<Vec<f64>> {
        #[cfg(feature = "gpu")]
        {
            let allowed = self.gpu.free_memory()? / 5 * 4;
            let (results, timing) = self.gpu.run_within(plan, inputs, allowed)?;
            // Set TIMING to see where a run's time went.
            if std::env::var_os("TIMING").is_some() {
                eprintln!("{timing:?}");
            }
            Ok(results)
        }
        #[cfg(not(feature = "gpu"))]
        Ok(plan.run_cpu(inputs))
    }
}

/// A small deterministic random number generator, so every run is the same.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Between 0 and 1, never 0.
    pub fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    /// From the bell curve: mean 0, spread 1.
    pub fn normal(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
}

/// An amount with thousands separated: `-1,234,567`.
pub fn commas(x: f64) -> String {
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
pub fn signed(x: f64) -> String {
    let s = commas(x);
    if x > 0.0 && s != "0" {
        format!("+{s}")
    } else {
        s
    }
}

pub fn time(d: Duration) -> String {
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
