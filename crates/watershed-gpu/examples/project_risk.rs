//! What will the project cost, when will it finish, and which risk is worth
//! paying to be rid of?
//!
//! An estimator prices a building project in a workbook: the estimate line
//! by line, the programme of activities and what each waits for, and a
//! register of the things that might go wrong. Each rate and duration has a
//! low, a likely and a high figure; each risk a chance, a cost and a delay.
//! The workbook as it stands uses the likely figures and has nothing go
//! wrong, which is the one outcome nobody expects.
//!
//! The usual answer is an add-in that draws every figure at random and
//! recalculates the workbook, a few thousand times, to say what the cost
//! will not exceed four times in five. This reads the workbook with
//! `watershed-xlsx`, takes the lows, likelies, highs and chances from the
//! workbook's own columns, and runs a million trials at once. Nothing is
//! rewritten by hand, and nothing about the project is in this file.
//!
//! Then it asks what a few thousand trials cannot afford to. For each risk
//! on the register, the same trials are run again with that risk never
//! happening, to see what removing it is worth: fifteen runs of everything.
//! The register ranks its risks by chance times cost. A risk that delays
//! an activity everything else waits for is worth more than that, and one
//! that delays an activity with time to spare is worth less.
//!
//!     cargo run --release --features cuda --example project_risk
//!
//! then optionally how many trials (1,000,000).
//!
//! The workbook is `shared/project.xlsx`, written by `shared/project.py`,
//! which also has LibreOffice make the reference. The project and every
//! number in it are made up. The figures are drawn independently, apart
//! from the one for the market that every price moves with; a real study
//! would say which move together.

use std::time::{Duration, Instant};

use watershed_core::Value;
use watershed_gpu::Scenarios;
use watershed_xlsx::{Cell, Model, Val, Workbook};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const WORKBOOK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/shared/project.xlsx");
/// What LibreOffice made of the workbook for some trials, one at a time.
const REFERENCE: &str = include_str!("shared/project_reference.csv");

/// The first row of each table in the workbook.
const FIRST: u32 = 5;
/// The cost, the weeks to finish, and whether the project is over budget
/// and late.
const ANSWERS: [&str; 4] = ["Summary!B13", "Summary!B6", "Summary!B15", "Summary!B16"];
/// How many trials are drawn and run at once.
const AT_ONCE: usize = 50_000;

/// How a cell is drawn for a trial.
enum Draw {
    /// Between a low and a high, most often near the likely.
    Between(f64, f64, f64),
    /// 1 with this chance, else 0.
    Happens(f64),
}

/// A cell a trial sets, with what it is called and how it is drawn.
struct Drawn {
    cell: Cell,
    what: String,
    draw: Draw,
}

/// A risk on the register.
struct Risk {
    what: String,
    /// Where its "happens" is among the cells drawn.
    happens: usize,
    /// Its chance times its likely cost: how the register ranks it.
    by_the_register: f64,
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

    fn draw(&mut self, draw: &Draw) -> f64 {
        match *draw {
            Draw::Happens(chance) => f64::from(u8::from(self.unit() < chance)),
            Draw::Between(low, likely, high) => {
                let (u, span) = (self.unit(), high - low);
                if span <= 0.0 {
                    likely
                } else if u * span < likely - low {
                    low + (u * span * (likely - low)).sqrt()
                } else {
                    high - ((1.0 - u) * span * (high - likely)).sqrt()
                }
            }
        }
    }
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

/// The same to about twelve figures.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6)
}

/// The value a share of the sorted numbers lie below.
fn below(sorted: &[f64], share: f64) -> f64 {
    sorted[((sorted.len() as f64 * share) as usize).min(sorted.len() - 1)]
}

fn main() -> Result<()> {
    let count: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse()?,
        None => 1_000_000,
    };
    let runner = Runner::new()?;

    // ---- The workbook ----
    println!("1. The workbook");
    let book = Workbook::open(WORKBOOK)?;
    let number = |name: &str| -> Result<f64> {
        match book.value(book.cell(name)?) {
            Val::Num(x) => Ok(x),
            _ => Err(format!("{name} does not hold a number").into()),
        }
    };
    let text = |name: &str| match book.cell(name).map(|c| book.value(c)) {
        Ok(Val::Text(t)) => Some(t),
        _ => None,
    };
    // What a trial draws, from the workbook's own columns.
    let mut drawn = vec![Drawn {
        cell: book.cell("Summary!B4")?,
        what: "the market".into(),
        draw: Draw::Between(
            number("Summary!C4")?,
            number("Summary!B4")?,
            number("Summary!D4")?,
        ),
    }];
    let rows = |sheet: &str| -> Vec<(u32, String)> {
        (FIRST..)
            .map_while(|row| text(&format!("{sheet}!A{row}")).map(|what| (row, what)))
            .collect()
    };
    let between = |sheet: &str, columns: [&str; 3], row: u32| -> Result<Draw> {
        let at = |column: &str| number(&format!("{sheet}!{column}{row}"));
        Ok(Draw::Between(
            at(columns[0])?,
            at(columns[1])?,
            at(columns[2])?,
        ))
    };
    let (lines, activities) = (rows("Estimate"), rows("Programme"));
    for (row, what) in &lines {
        drawn.push(Drawn {
            cell: book.cell(&format!("Estimate!G{row}"))?,
            what: format!("the rate for {}", what.to_lowercase()),
            draw: between("Estimate", ["D", "E", "F"], *row)?,
        });
    }
    for (row, what) in &activities {
        drawn.push(Drawn {
            cell: book.cell(&format!("Programme!E{row}"))?,
            what: format!("how long {} takes", what.to_lowercase()),
            draw: between("Programme", ["B", "C", "D"], *row)?,
        });
    }
    let mut risks = Vec::new();
    for (row, what) in rows("Risks") {
        let chance = number(&format!("Risks!B{row}"))?;
        risks.push(Risk {
            what: what.clone(),
            happens: drawn.len(),
            by_the_register: chance * number(&format!("Risks!D{row}"))?,
        });
        drawn.push(Drawn {
            cell: book.cell(&format!("Risks!I{row}"))?,
            what: format!("whether: {}", what.to_lowercase()),
            draw: Draw::Happens(chance),
        });
        drawn.push(Drawn {
            cell: book.cell(&format!("Risks!J{row}"))?,
            what: format!("the cost of: {}", what.to_lowercase()),
            draw: between("Risks", ["C", "D", "E"], row)?,
        });
        drawn.push(Drawn {
            cell: book.cell(&format!("Risks!K{row}"))?,
            what: format!("the delay from: {}", what.to_lowercase()),
            draw: between("Risks", ["F", "G", "H"], row)?,
        });
    }
    let n = drawn.len();
    let inputs: Vec<Cell> = drawn.iter().map(|d| d.cell).collect();
    let answers: Vec<Cell> = ANSWERS
        .iter()
        .map(|name| book.cell(name))
        .collect::<std::result::Result<_, _>>()?;
    let mut model = Model::build(&book, &inputs, &answers)?;
    let wrong = model.check();
    if let Some(first) = wrong.first() {
        return Err(format!("the graph does not agree with the workbook: {first}").into());
    }
    println!(
        "   project.xlsx: {} lines of the estimate, {} activities, {} risks; {} formulas, each as the workbook has it",
        lines.len(),
        activities.len(),
        risks.len(),
        book.formulas()
    );
    let (as_planned, weeks_planned) = (number(ANSWERS[0])?, number(ANSWERS[1])?);
    let (budget, promised) = (number("Summary!B14")?, number("Summary!B9")?);
    println!(
        "   on the likely figures, with nothing going wrong: {} in {} weeks, against a budget of {} and {} weeks promised",
        commas(as_planned),
        weeks_planned,
        commas(budget),
        promised
    );
    println!("   a trial draws {n} of its cells, from the lows, highs and chances beside them");
    let output_ids = model.outputs();
    let plan = Scenarios::new(model.graph(), &model.inputs(), &output_ids)?;
    let m = ANSWERS.len();
    println!();

    // ---- Against LibreOffice ----
    println!("2. Against LibreOffice, a trial at a time");
    let mut reference = REFERENCE.lines();
    let note = reference.next().ok_or("the reference is empty")?;
    let each_ms: f64 = note
        .rsplit(": ")
        .next()
        .and_then(|took| took.strip_suffix(" ms a trial"))
        .ok_or("the reference does not say how long a trial took")?
        .parse()?;
    let named: Vec<&str> = reference.next().ok_or("no heading")?.split(',').collect();
    for (name, d) in named.iter().zip(&drawn) {
        assert_eq!(
            *name,
            book.name(d.cell),
            "the reference's cells are not the workbook's"
        );
    }
    let theirs: Vec<Vec<f64>> = reference
        .map(|line| line.split(',').map(str::parse).collect())
        .collect::<std::result::Result<_, _>>()?;
    let set: Vec<f64> = theirs.iter().flat_map(|row| row[..n].to_vec()).collect();
    let ours = runner.run(&plan, &set)?;
    for (row, here) in theirs.iter().zip(ours.chunks(m)) {
        assert!(
            close(here[0], row[n]) && close(here[1], row[n + 1]),
            "{} in {} weeks here, {} in {} in LibreOffice",
            here[0],
            here[1],
            row[n],
            row[n + 1]
        );
    }
    println!(
        "   {} trials: the cost and the weeks agree for every one",
        theirs.len()
    );
    println!(
        "   LibreOffice took {each_ms} ms a trial, the {n} cells set and the workbook recalculated"
    );
    println!();

    // ---- The trials ----
    println!("3. The trials");
    let trials = |rng: &mut Rng, how_many: usize| -> Vec<f64> {
        let mut out = Vec::with_capacity(how_many * n);
        for _ in 0..how_many {
            out.extend(drawn.iter().map(|d| rng.draw(&d.draw)));
        }
        out
    };
    let seed = 0x2545_F491_4F6C_DD1D;
    let mut rng = Rng(seed);
    let (mut costs, mut weeks) = (Vec::with_capacity(count), Vec::with_capacity(count));
    let (mut over, mut late, mut both) = (0usize, 0usize, 0usize);
    // For what drives the cost: each cell's sums against the cost.
    let mut sums = vec![[0.0f64; 3]; n];
    let (mut cost_sum, mut cost_squares) = (0.0f64, 0.0f64);
    let mut first = (Vec::new(), Vec::new());
    let started = Instant::now();
    let mut running = Duration::ZERO;
    for from in (0..count).step_by(AT_ONCE) {
        let some = trials(&mut rng, AT_ONCE.min(count - from));
        let run = Instant::now();
        let got = runner.run(&plan, &some)?;
        running += run.elapsed();
        for (trial, answer) in some.chunks(n).zip(got.chunks(m)) {
            costs.push(answer[0]);
            weeks.push(answer[1]);
            over += usize::from(answer[2] == 1.0);
            late += usize::from(answer[3] == 1.0);
            both += usize::from(answer[2] == 1.0 && answer[3] == 1.0);
            cost_sum += answer[0];
            cost_squares += answer[0] * answer[0];
            for (sum, &x) in sums.iter_mut().zip(trial) {
                sum[0] += x;
                sum[1] += x * x;
                sum[2] += x * answer[0];
            }
        }
        if from == 0 {
            first = (some, got);
        }
    }
    println!(
        "   {} trials in {}, {} of it running the workbook ({} a second); LibreOffice, at {each_ms} ms a trial, would take {}",
        commas(count as f64),
        time(started.elapsed()),
        time(running),
        commas(count as f64 / running.as_secs_f64()),
        time(Duration::from_secs_f64(each_ms / 1000.0 * count as f64))
    );
    // The graph itself, a trial at a time, as the add-in would do it: it
    // must agree with the plan to the last bit.
    let few = count.min(2_000);
    for (trial, answer) in first.0.chunks(n).zip(first.1.chunks(m)).take(few) {
        model.set(trial)?;
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
        "   the graph, a trial at a time, agrees to the last bit for the first {}",
        commas(few as f64)
    );
    let mut sorted_costs = costs.clone();
    sorted_costs.sort_by(f64::total_cmp);
    let mut sorted_weeks = weeks.clone();
    sorted_weeks.sort_by(f64::total_cmp);
    println!();
    println!("   {:<28} {:>14} {:>10}", "", "cost", "weeks");
    println!(
        "   {:<28} {:>14} {:>10}",
        "as the workbook stands",
        commas(as_planned),
        weeks_planned
    );
    for (what, share) in [
        ("1 in 10 come in under", 0.1),
        ("half come in under", 0.5),
        ("4 in 5 come in under", 0.8),
        ("9 in 10 come in under", 0.9),
        ("99 in 100 come in under", 0.99),
    ] {
        println!(
            "   {what:<28} {:>14} {:>10.1}",
            commas(below(&sorted_costs, share)),
            below(&sorted_weeks, share)
        );
    }
    let share = |k: usize| 100.0 * k as f64 / count as f64;
    println!(
        "   as the workbook stands or better: {:.1}% of trials for cost, {:.1}% for weeks",
        share(sorted_costs.partition_point(|&c| c <= as_planned)),
        share(sorted_weeks.partition_point(|&w| w <= weeks_planned))
    );
    println!(
        "   over budget: {:.1}%; late: {:.1}%; both: {:.1}%",
        share(over),
        share(late),
        share(both)
    );
    let four_in_five = below(&sorted_costs, 0.8);
    println!(
        "   to be covered 4 times in 5, hold {} above the estimate",
        commas(four_in_five - as_planned)
    );
    println!();

    // ---- What drives the cost ----
    println!("4. What the cost moves with");
    let k = count as f64;
    let cost_spread = (cost_squares / k - (cost_sum / k).powi(2)).sqrt();
    let mut moves: Vec<(f64, &str)> = sums
        .iter()
        .zip(&drawn)
        .map(|(sum, d)| {
            let spread = (sum[1] / k - (sum[0] / k).powi(2)).sqrt();
            let together = sum[2] / k - sum[0] / k * cost_sum / k;
            let correlation = if spread > 0.0 {
                together / (spread * cost_spread)
            } else {
                0.0
            };
            (correlation, d.what.as_str())
        })
        .collect();
    moves.sort_by(|a, b| b.0.abs().total_cmp(&a.0.abs()));
    println!(
        "   how closely, from none (0) to wholly (1), and by how much for a usual swing of each"
    );
    for (correlation, what) in moves.iter().take(8) {
        println!(
            "     {correlation:>5.2}  {:>10}  {what}",
            commas(correlation * cost_spread)
        );
    }
    println!();

    // ---- Which risk to be rid of ----
    println!("5. Which risk is worth paying to be rid of");
    // The same trials again, once as they are and once for each risk with
    // that risk never happening.
    let again = count.min(200_000);
    let mut rng = Rng(seed);
    let mut base = Vec::with_capacity(again);
    let mut without: Vec<Vec<(f64, f64)>> =
        risks.iter().map(|_| Vec::with_capacity(again)).collect();
    let started = Instant::now();
    for from in (0..again).step_by(AT_ONCE) {
        let some = trials(&mut rng, AT_ONCE.min(again - from));
        let got = runner.run(&plan, &some)?;
        base.extend(got.chunks(m).map(|a| (a[0], a[1])));
        for (risk, without) in risks.iter().zip(&mut without) {
            let mut spared = some.clone();
            for trial in spared.chunks_mut(n) {
                trial[risk.happens] = 0.0;
            }
            let got = runner.run(&plan, &spared)?;
            without.extend(got.chunks(m).map(|a| (a[0], a[1])));
        }
    }
    println!(
        "   {} trials, {} times over: {} runs of the workbook in {}",
        commas(again as f64),
        risks.len() + 1,
        commas((again * (risks.len() + 1)) as f64),
        time(started.elapsed())
    );
    let fifth = |pairs: &[(f64, f64)], which: usize| {
        let mut all: Vec<f64> = pairs.iter().map(|p| [p.0, p.1][which]).collect();
        all.sort_by(f64::total_cmp);
        below(&all, 0.8)
    };
    let (cost_80, weeks_80) = (fifth(&base, 0), fifth(&base, 1));
    // What being rid of each is worth: the fall in the cost that covers 4
    // trials in 5, and in the weeks.
    let mut worth: Vec<(f64, f64, f64, &Risk)> = risks
        .iter()
        .zip(&without)
        .map(|(risk, without)| {
            // Sparing a risk never costs more or takes longer, in any trial.
            assert!(
                base.iter()
                    .zip(without)
                    .all(|(was, is)| is.0 <= was.0 + 1.0 && is.1 <= was.1 + 1e-9),
                "being rid of a risk made a trial worse"
            );
            let saved = base
                .iter()
                .zip(without)
                .map(|(was, is)| was.0 - is.0)
                .sum::<f64>()
                / again as f64;
            (
                cost_80 - fifth(without, 0),
                weeks_80 - fifth(without, 1),
                saved,
                risk,
            )
        })
        .collect();
    let mut by_register: Vec<&Risk> = risks.iter().collect();
    by_register.sort_by(|a, b| b.by_the_register.total_cmp(&a.by_the_register));
    let rank = |risk: &Risk| {
        1 + by_register
            .iter()
            .position(|r| std::ptr::eq(*r, risk))
            .expect("a risk")
    };
    worth.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!(
        "   {:<44} {:>14} {:>8} {:>12} {:>14} {:>10}",
        "", "off the 4-in-5", "weeks", "on average", "chance × cost", "its rank"
    );
    for (place, (cost, weeks, saved, risk)) in worth.iter().enumerate() {
        println!(
            "   {:>2} {:<41} {:>14} {:>8.1} {:>12} {:>14} {:>10}",
            place + 1,
            risk.what,
            commas(*cost),
            weeks,
            commas(*saved),
            commas(risk.by_the_register),
            rank(risk)
        );
    }
    let moved = worth
        .iter()
        .enumerate()
        .filter(|(place, w)| place.abs_diff(rank(w.3) - 1) >= 3)
        .count();
    println!(
        "   {} of the {} risks are three places or more from where the register's chance times cost puts them",
        moved,
        risks.len()
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
