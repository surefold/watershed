//! Does work pay? One household's ready reckoner, for a whole country.
//!
//! An adviser works out what a family has to live on in a workbook: pay at
//! the top, tax and social insurance taken off, a payment for the children,
//! income support that shrinks as the family earns. To show a family what a
//! pay rise is worth, the adviser types in the new pay and reads the new
//! answer, and does it again for the next rise. Drawn as a line, that is
//! the family's budget: mostly it climbs, slowly where support is being
//! withdrawn, and here and there it drops, where a rise in pay ends
//! something worth more than the rise.
//!
//! A policy researcher wants that line for every household in the country,
//! to say how many keep less than a third of a rise, and how many would be
//! worse off for earning more. That is the workbook forty-one times for
//! each household.
//!
//! This reads the workbook with `watershed-xlsx` and does exactly that.
//! Nothing is rewritten by hand. It checks itself against the values saved
//! in the workbook, against LibreOffice recalculating the workbook for each
//! of 300 households, and against the graph a household at a time. Then it
//! changes two of the rules, as a reform would, and draws every line again.
//!
//!     cargo run --release --features cuda --example households
//!
//! then optionally how many households (500,000).
//!
//! The workbook is `shared/household.xlsx`, written by
//! `shared/household.py`, which also has LibreOffice make the reference.
//! The rules look a little like the United Kingdom's, but every number is
//! made up and much is left out, and so are the households: nothing here
//! says anything about a real country. The `tax_benefit` example is rules
//! of the same kind written straight onto a graph.

use std::time::{Duration, Instant};

use watershed_core::Value;
use watershed_gpu::Scenarios;
use watershed_xlsx::{Model, Val, Workbook};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const WORKBOOK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/shared/household.xlsx"
);
/// What LibreOffice made of the workbook for some households, one at a time.
const REFERENCE: &str = include_str!("shared/household_reference.csv");

/// The household's cells: adults, children, each adult's pay a year, rent
/// and childcare a month, and savings.
const HOUSEHOLD: [&str; 7] = [
    "Household!B4",
    "Household!B5",
    "Household!B6",
    "Household!B7",
    "Household!B8",
    "Household!B9",
    "Household!B10",
];
/// Where the first adult's pay is among those.
const PAY: usize = 2;
/// What the household has to live on, the tax and social insurance it
/// pays, and the support it gets, each for a year.
const ANSWERS: [&str; 3] = ["Household!B13", "Household!B14", "Household!B15"];
/// The reform: what is taken off income support for each pound earned goes
/// from 55p to 50p, and school meals no longer stop at a level of earnings.
const REFORM: [(&str, f64); 2] = [("Rules!B31", 0.50), ("Rules!B39", 1e9)];
/// A household's pay is raised in steps of this, this many times.
const RISE: f64 = 500.0;
const RISES: usize = 40;
/// How many real households the made-up ones stand for, when a sum is given
/// for the whole country.
const COUNTRY: f64 = 28e6;
/// How many households' lines are drawn at once.
const AT_ONCE: usize = 20_000;

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

    fn run(&self, plan: &Scenarios, households: &[[f64; 7]]) -> Result<Vec<f64>> {
        #[cfg(feature = "gpu")]
        return Ok(self.gpu.run(plan, households.as_flattened())?);
        #[cfg(not(feature = "gpu"))]
        Ok(plan.run_cpu(households.as_flattened()))
    }
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
}

/// One household of a made-up country, as the workbook's seven numbers.
fn made_up_household(rng: &mut Rng) -> [f64; 7] {
    let adults = if rng.unit() < 0.6 { 2.0 } else { 1.0 };
    let children = [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 2.0, 2.0, 3.0, 4.0][(10.0 * rng.unit()) as usize];
    let mut pay = |out_of_work: f64| match rng.unit() < out_of_work {
        true => 0.0,
        false => (90.0 * (2.4 * rng.unit().powf(1.3)).exp()).round() * 100.0,
    };
    let first = pay(0.15);
    let second = if adults == 2.0 { pay(0.35) } else { 0.0 };
    let rent = match rng.unit() < 0.45 {
        true => 0.0,
        false => ((400.0 + 1100.0 * rng.unit()) / 10.0).round() * 10.0,
    };
    let childcare = match children > 0.0 && rng.unit() < 0.5 {
        true => (120.0 * rng.unit()).round() * 10.0,
        false => 0.0,
    };
    let savings = (600.0 * rng.unit().powi(6)).round() * 100.0;
    [adults, children, first, second, rent, childcare, savings]
}

/// The same to about twelve figures.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6)
}

/// What the lines of every household say, taken together.
#[derive(Default)]
struct Lines {
    /// How many households keep this share of the next thousand of pay:
    /// less than nothing, under a fifth, under two fifths, under three
    /// fifths, and more.
    kept: [usize; 5],
    /// Households for whom some rise on the way leaves them worse off.
    worse_off: usize,
    /// The largest such drop: the household, its pay before, and what it
    /// had to live on before and after.
    worst: Option<([f64; 7], f64, f64, f64)>,
}

/// Raises each household's first adult's pay step by step and reads what it
/// has to live on at each.
fn lines(runner: &Runner, plan: &Scenarios, households: &[[f64; 7]]) -> Result<Lines> {
    let mut all = Lines::default();
    let points = RISES + 1;
    for some in households.chunks(AT_ONCE) {
        let mut tried = Vec::with_capacity(some.len() * points);
        for household in some {
            for k in 0..points {
                let mut risen = *household;
                risen[PAY] += k as f64 * RISE;
                tried.push(risen);
            }
        }
        let got = runner.run(plan, &tried)?;
        for (household, line) in some.iter().zip(got.chunks(points * ANSWERS.len())) {
            let at = |k: usize| line[k * ANSWERS.len()];
            let kept = (at(2) - at(0)) / (2.0 * RISE);
            all.kept[match kept {
                k if k < 0.0 => 0,
                k if k < 0.2 => 1,
                k if k < 0.4 => 2,
                k if k < 0.6 => 3,
                _ => 4,
            }] += 1;
            // A drop of more than a pound, to leave rounding out of it.
            let drop = (0..RISES)
                .map(|k| (at(k) - at(k + 1), k))
                .max_by(|a, b| a.0.total_cmp(&b.0))
                .expect("some rises");
            if drop.0 > 1.0 {
                all.worse_off += 1;
                if all.worst.is_none_or(|(_, _, was, is)| drop.0 > was - is) {
                    let pay = household[PAY] + drop.1 as f64 * RISE;
                    all.worst = Some((*household, pay, at(drop.1), at(drop.1 + 1)));
                }
            }
        }
    }
    Ok(all)
}

fn told(lines: &Lines, count: usize) {
    let share = |n: usize| 100.0 * n as f64 / count as f64;
    println!("   of the next 1,000 of pay, households that keep");
    for (what, n) in [
        "less than nothing",
        "under 200",
        "200 to 400",
        "400 to 600",
        "600 or more",
    ]
    .iter()
    .zip(lines.kept)
    {
        println!(
            "     {what:<18} {:>10}  {:>5.1}%",
            commas(n as f64),
            share(n)
        );
    }
    println!(
        "   worse off after some rise of {RISE} on the way to {} more: {} ({:.1}%)",
        commas(RISE * RISES as f64),
        commas(lines.worse_off as f64),
        share(lines.worse_off)
    );
    if let Some((household, pay, was, is)) = lines.worst {
        println!(
            "     the worst: {} adults, {} children, the other adult paid {}, rent {} and childcare {} a month",
            household[0],
            household[1],
            commas(household[3]),
            commas(household[4]),
            commas(household[5])
        );
        println!(
            "       at pay of {} they have {} to live on; at {}, {}",
            commas(pay),
            commas(was),
            commas(pay + RISE),
            commas(is)
        );
    }
}

fn main() -> Result<()> {
    let count: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse()?,
        None => 500_000,
    };
    let runner = Runner::new()?;

    // ---- The workbook ----
    println!("1. The workbook");
    let book = Workbook::open(WORKBOOK)?;
    let cells = |names: &[&str]| -> Result<Vec<_>> {
        Ok(names
            .iter()
            .map(|name| book.cell(name))
            .collect::<std::result::Result<_, _>>()?)
    };
    let rules: Vec<&str> = REFORM.iter().map(|&(cell, _)| cell).collect();
    let inputs = cells(&[&HOUSEHOLD[..], &rules[..]].concat())?;
    let answers = cells(&ANSWERS)?;
    let mut model = Model::build(&book, &inputs, &answers)?;
    let wrong = model.check();
    if let Some(first) = wrong.first() {
        return Err(format!("the graph does not agree with the workbook: {first}").into());
    }
    println!(
        "   household.xlsx: {} formulas, each as the workbook has it; it uses {}",
        book.formulas(),
        model
            .functions()
            .iter()
            .map(|(name, n)| format!("{name} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let ids = model.inputs();
    let output_ids = model.outputs();
    // The household varies from run to run; the rules stay as they are.
    let plan = Scenarios::new(model.graph(), &ids[..HOUSEHOLD.len()], &output_ids)?;
    println!();

    // ---- Against LibreOffice ----
    println!("2. Against LibreOffice, a household at a time");
    let mut reference = REFERENCE.lines();
    let note = reference.next().ok_or("the reference is empty")?;
    let each_ms: f64 = note
        .rsplit(": ")
        .next()
        .and_then(|took| took.strip_suffix(" ms a household"))
        .ok_or("the reference does not say how long a household took")?
        .parse()?;
    let rows: Vec<Vec<f64>> = reference
        .skip(1)
        .map(|line| line.split(',').map(str::parse).collect())
        .collect::<std::result::Result<_, _>>()?;
    let theirs: Vec<[f64; 7]> = rows
        .iter()
        .map(|row| row[..HOUSEHOLD.len()].try_into().expect("seven numbers"))
        .collect();
    let ours = runner.run(&plan, &theirs)?;
    for (row, here) in rows.iter().zip(ours.chunks(ANSWERS.len())) {
        for (&there, &here) in row[HOUSEHOLD.len()..].iter().zip(here) {
            assert!(
                close(here, there),
                "{here} here and {there} in LibreOffice, for the household {:?}",
                &row[..HOUSEHOLD.len()]
            );
        }
    }
    println!(
        "   {} households, three answers each: every one agrees",
        rows.len()
    );
    println!(
        "   LibreOffice took {each_ms} ms a household, its cells set and the workbook recalculated"
    );
    println!();

    // ---- The country ----
    println!("3. The country");
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let households: Vec<[f64; 7]> = (0..count).map(|_| made_up_household(&mut rng)).collect();
    let started = Instant::now();
    let now = runner.run(&plan, &households)?;
    let took = started.elapsed();
    println!(
        "   {} households in {} ({} a second)",
        commas(count as f64),
        time(took),
        commas(count as f64 / took.as_secs_f64())
    );
    // The graph itself, a household at a time, as the spreadsheet would do
    // it: it must agree with the plan to the last bit.
    let as_given = model.workbook_inputs();
    let few = count.min(2_000);
    for (household, answer) in households.iter().zip(now.chunks(ANSWERS.len())).take(few) {
        let mut set = as_given.clone();
        set[..HOUSEHOLD.len()].copy_from_slice(household);
        model.set(&set)?;
        for (&id, &x) in output_ids.iter().zip(answer) {
            let Value::F64(here) = model.graph().value(id) else {
                unreachable!("an answer is a number")
            };
            assert!(
                here.to_bits() == x.to_bits(),
                "the graph and the plan differ"
            );
        }
    }
    println!(
        "   the graph, a household at a time, agrees to the last bit for the first {}",
        commas(few as f64)
    );
    let sum = |answers: &[f64], which: usize| -> f64 {
        answers.chunks(ANSWERS.len()).map(|a| a[which]).sum::<f64>() / count as f64
    };
    println!(
        "   a household has {} to live on, pays {} in tax and social insurance and gets {} in support, on average",
        commas(sum(&now, 0)),
        commas(sum(&now, 1)),
        commas(sum(&now, 2))
    );
    println!();

    // ---- Does work pay ----
    println!("4. Does work pay");
    let started = Instant::now();
    let before = lines(&runner, &plan, &households)?;
    let took = started.elapsed();
    let runs = count * (RISES + 1);
    println!(
        "   each household's first adult paid {RISE} more, {RISES} times over: {} workbooks' worth in {}",
        commas(runs as f64),
        time(took)
    );
    println!(
        "   LibreOffice, at {each_ms} ms a household, would take {}",
        time(Duration::from_secs_f64(each_ms / 1000.0 * runs as f64))
    );
    told(&before, count);
    assert!(before.worse_off > 0 && before.kept[4] > 0);
    println!();

    // ---- A reform ----
    println!("5. A reform");
    println!(
        "   income support withdrawn at 50p for each pound earned, not 55p; school meals for all who get it"
    );
    for ((&cell, &id), &(name, to)) in inputs[HOUSEHOLD.len()..]
        .iter()
        .zip(&ids[HOUSEHOLD.len()..])
        .zip(&REFORM)
    {
        if !matches!(book.value(cell), Val::Num(_)) {
            return Err(format!("{name} does not hold a number").into());
        }
        model.graph_mut().set_value(id, Value::F64(to))?;
    }
    model.graph_mut().stabilize();
    let reformed = Scenarios::new(model.graph(), &ids[..HOUSEHOLD.len()], &output_ids)?;
    let then = runner.run(&reformed, &households)?;
    let (mut gain, mut gainers, mut losers) = (0.0, 0usize, 0usize);
    for (was, is) in now.chunks(ANSWERS.len()).zip(then.chunks(ANSWERS.len())) {
        let more = is[0] - was[0];
        gain += more;
        gainers += usize::from(more > 0.005);
        losers += usize::from(more < -0.005);
    }
    println!(
        "   {} households gain ({:.1}%), {} a year each on average; {} lose",
        commas(gainers as f64),
        100.0 * gainers as f64 / count as f64,
        commas(gain / gainers.max(1) as f64),
        losers
    );
    println!(
        "   it costs {} a household, or {:.1} billion a year for a country of {} million households",
        commas(gain / count as f64),
        gain / count as f64 * COUNTRY / 1e9,
        COUNTRY / 1e6
    );
    assert!(losers == 0 && gainers > 0);
    let after = lines(&runner, &reformed, &households)?;
    told(&after, count);
    println!(
        "   households worse off after some rise: {} before, {} after",
        commas(before.worse_off as f64),
        commas(after.worse_off as f64)
    );
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
    } else if s < 7200.0 {
        format!("{:.0} minutes", s / 60.0)
    } else {
        format!("{:.1} hours", s / 3600.0)
    }
}
