//! What moves a workbook's answer, and by how much, for every input at once.
//!
//! Whoever signs off a provision is asked what it rests on. How much does
//! it move if defaults respond more to unemployment than assumed? If
//! selling a repossessed house costs more? If the slump's third year is
//! worse than the economists drew it? A spreadsheet answers each by
//! changing the number and recalculating: one recalculation of the whole
//! book for each question, and the questions run to hundreds once every
//! quarter of every path of the economy is one.
//!
//! This reads the `credit_loss` example's workbook with `watershed-xlsx`
//! and asks the graph for the derivative of a loan's provision by each of
//! its assumptions and by every quarter of the economy: 688 numbers. They
//! are worked out backwards from the provision, all together, so the lot
//! costs a few times the provision alone, not 688 times. The derivatives
//! are nodes of the graph like any others, so a plan runs them for the
//! whole book with the provision itself.
//!
//! It checks every one of them for the workbook's own loan against nudging
//! that input up and down. Then it adds them up over a made-up book, and
//! says which assumptions and which quarters the book's provision rests on.
//!
//! A derivative is the slope where each loan stands. It says nothing of a
//! loan that a change would carry across a threshold into another stage:
//! that is a jump, not a slope, and the last part shows the difference.
//! For the same reason the loan's own cells are left out: the workbook
//! rounds interest and payments to the penny, and a rounded number's slope
//! is nought.
//!
//!     cargo run --release --features cuda --example sensitivities
//!
//! then optionally how many loans (50,000). The workbook is written by
//! `shared/credit_loss.py`; everything in it is made up.

use std::time::{Duration, Instant};

use watershed_core::NodeId;
use watershed_gpu::Scenarios;
use watershed_xlsx::{Cell, Model, Val, Workbook};

#[path = "shared/loan_book.rs"]
mod loan_book;
use loan_book::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The bank's choices on the `Model` sheet: the rows of column B, and the
/// discounts on a forced sale in column E.
const CHOICES: [u32; 11] = [5, 6, 8, 10, 11, 12, 13, 15, 16, 18, 19];
const DISCOUNTS: std::ops::RangeInclusive<u32> = 5..=10;
/// The economy's typed columns, and its rows: a quarter each.
const ECONOMY: [(&str, &str); 6] = [
    ("B", "expected, unemployment"),
    ("C", "expected, house prices"),
    ("D", "better, unemployment"),
    ("E", "better, house prices"),
    ("F", "the slump's rise in unemployment"),
    ("G", "the slump's fall in house prices"),
];
const QUARTERS: std::ops::RangeInclusive<u32> = 5..=116;
/// How many loans a run takes at once: each gives back hundreds of numbers.
const AT_ONCE: usize = 2_000;

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

    /// The sum over the loans of each thing the plan works out.
    fn summed(&self, plan: &Scenarios, loans: &[[f64; 7]], each: usize) -> Result<Vec<f64>> {
        let mut sums = vec![0.0; each];
        for some in loans.chunks(AT_ONCE) {
            let got = self.run(plan, some.as_flattened())?;
            for one in got.chunks(each) {
                for (sum, x) in sums.iter_mut().zip(one) {
                    *sum += x;
                }
            }
        }
        Ok(sums)
    }
}

/// An input the provision is differentiated by.
struct By {
    cell: Cell,
    /// What it is, in words.
    what: String,
    /// What the workbook has for it.
    value: f64,
}

fn main() -> Result<()> {
    let count: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse()?,
        None => 50_000,
    };
    let runner = Runner::new()?;

    // ---- The workbook ----
    println!("1. The workbook");
    let book = Workbook::open(WORKBOOK)?;
    let number = |c: Cell| match book.value(c) {
        Val::Num(x) => Ok(x),
        _ => Err(format!("{} does not hold a number", book.name(c))),
    };
    let text = |name: &str| -> Result<String> {
        Ok(match book.value(book.cell(name)?) {
            Val::Text(t) => t,
            _ => String::new(),
        })
    };
    let mut by = Vec::new();
    for row in CHOICES {
        let cell = book.cell(&format!("Model!B{row}"))?;
        let what = match row {
            5 | 6 => format!(
                "How likely the {} path is",
                text(&format!("Model!A{row}"))?.to_lowercase()
            ),
            _ => text(&format!("Model!A{row}"))?,
        };
        by.push(By {
            cell,
            what,
            value: number(cell)?,
        });
    }
    for row in DISCOUNTS {
        let cell = book.cell(&format!("Model!E{row}"))?;
        let from = number(book.cell(&format!("Model!D{row}"))?)?;
        let what = format!("Discount on a forced sale, loan to value from {from}");
        by.push(By {
            cell,
            what,
            value: number(cell)?,
        });
    }
    let choices = by.len();
    for (column, what) in ECONOMY {
        for row in QUARTERS {
            let cell = book.cell(&format!("Economy!{column}{row}"))?;
            let what = format!("{what}, quarter {}", row - 4);
            by.push(By {
                cell,
                what,
                value: number(cell)?,
            });
        }
    }
    let loan: Vec<Cell> = LOAN
        .iter()
        .map(|name| book.cell(name))
        .collect::<std::result::Result<_, _>>()?;
    let all: Vec<Cell> = loan
        .iter()
        .copied()
        .chain(by.iter().map(|b| b.cell))
        .collect();
    let provision_cell = book.cell("Loan!B14")?;
    let mut model = Model::build(&book, &all, &[provision_cell])?;
    let wrong = model.check();
    if let Some(first) = wrong.first() {
        return Err(format!("the graph does not agree with the workbook: {first}").into());
    }
    let ids = model.inputs();
    let provision = model.outputs()[0];
    let before = model.graph().len();
    let started = Instant::now();
    let slopes: Vec<NodeId> = model
        .graph_mut()
        .gradient(provision, &ids[loan.len()..], "slope")?;
    model.graph_mut().stabilize();
    println!(
        "   credit_loss.xlsx: {} cells between its inputs and the provision, all as the workbook has them",
        model.translated()
    );
    println!(
        "   the provision's derivative by {} of them ({} choices, {} quarters of the economy): {} more nodes, added in {}",
        by.len(),
        choices,
        by.len() - choices,
        model.graph().len() - before,
        time(started.elapsed())
    );
    println!();

    // ---- One loan, against nudging ----
    println!("2. The workbook's own loan, against nudging each input");
    let as_given = model.workbook_inputs();
    let mut outputs = vec![provision];
    outputs.extend(&slopes);
    // The loan's seven cells vary from loan to loan; the rest stay as the
    // workbook has them.
    let with_slopes = Scenarios::new(model.graph(), &ids[..loan.len()], &outputs)?;
    let alone = Scenarios::new(model.graph(), &ids[..loan.len()], &[provision])?;
    let everything = Scenarios::new(model.graph(), &ids, &[provision])?;
    let own = runner.run(&with_slopes, &as_given[..loan.len()])?;
    let mut nudged = Vec::with_capacity(2 * by.len() * as_given.len());
    let nudge = |x: f64| 1e-6 * x.abs().max(0.01);
    for k in 0..by.len() {
        for sign in [-1.0, 1.0] {
            let mut inputs = as_given.clone();
            inputs[loan.len() + k] += sign * nudge(by[k].value);
            nudged.extend(inputs);
        }
    }
    let moved = runner.run(&everything, &nudged)?;
    let mut apart = Vec::new();
    let mut worst = 0.0f64;
    for (k, b) in by.iter().enumerate() {
        let measured = (moved[2 * k + 1] - moved[2 * k]) / (2.0 * nudge(b.value));
        let slope = own[1 + k];
        // To five figures of the slope, or of what a nudge can measure.
        let room = 1e-5 * slope.abs() + 1e-13 * own[0] / nudge(b.value);
        let off = (measured - slope).abs();
        if off > room {
            apart.push(format!("{}: {slope} here, {measured} by nudging", b.what));
        } else if slope != 0.0 {
            worst = worst.max(off / slope.abs());
        }
    }
    println!(
        "   provision {:.2}; {} derivatives, each against the provision with that input a millionth up and down",
        own[0],
        by.len()
    );
    println!(
        "   {} agree, the furthest by {worst:.0e} of itself; {} do not",
        by.len() - apart.len(),
        apart.len()
    );
    for line in apart.iter().take(10) {
        println!("     {line}");
    }
    assert!(apart.is_empty(), "a derivative is not what nudging gives");
    let nonzero = (0..by.len()).filter(|&k| own[1 + k] != 0.0).count();
    println!("   {nonzero} of them are not nought for this loan");
    println!("   what a hundredth more of each would do, the largest first:");
    let effect = |k: usize, slopes: &[f64]| slopes[k] * by[k].value * 0.01;
    let mut order: Vec<usize> = (0..by.len()).collect();
    order.sort_by(|&a, &b| {
        effect(b, &own[1..])
            .abs()
            .total_cmp(&effect(a, &own[1..]).abs())
    });
    for &k in order.iter().take(6) {
        println!(
            "     {:>+9.3}  {} (now {})",
            effect(k, &own[1..]),
            by[k].what,
            by[k].value
        );
    }
    println!();

    // ---- The book ----
    println!("3. The book");
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let loans: Vec<[f64; 7]> = (0..count).map(|_| made_up_loan(&mut rng)).collect();
    let started = Instant::now();
    let plain = runner.summed(&alone, &loans, 1)?;
    let plain_took = started.elapsed();
    let started = Instant::now();
    let sums = runner.summed(&with_slopes, &loans, outputs.len())?;
    let took = started.elapsed();
    assert!((sums[0] - plain[0]).abs() <= 1e-9 * plain[0].abs());
    println!(
        "   {} loans: the provision alone in {}, with its {} derivatives in {} ({:.1} times as long)",
        commas(count as f64),
        time(plain_took),
        by.len(),
        time(took),
        took.as_secs_f64() / plain_took.as_secs_f64()
    );
    println!(
        "   nudging each instead is two runs of the book apiece: {} runs, about {}",
        commas(2.0 * by.len() as f64),
        time(plain_took * 2 * by.len() as u32)
    );
    let total = sums[0];
    let book_slopes = &sums[1..];
    println!("   provision for the book: {}", commas(total));
    println!();
    println!("   the bank's choices: what a hundredth more of each does to the provision");
    for (k, b) in by.iter().enumerate().take(choices) {
        let moves = effect(k, book_slopes);
        println!(
            "     {:>12} {:>+7.2}%  {} (now {})",
            signed(moves),
            100.0 * moves / total,
            b.what,
            b.value
        );
    }
    println!();
    println!("   the economy: what a year of it does to the provision, by the year it falls in");
    println!(
        "     unemployment a point higher all that year, or house prices lower by a hundredth of today's"
    );
    print!("     {:<34}", "");
    let years = [
        "year 1",
        "year 2",
        "year 3",
        "year 4",
        "year 5",
        "years 6-10",
        "after",
    ];
    for year in years {
        print!("{year:>12}");
    }
    println!();
    let per = QUARTERS.clone().count();
    for (c, (_, what)) in ECONOMY.iter().enumerate() {
        // House prices are asked about going down.
        let unit = if c % 2 == 0 { 1.0 } else { -0.01 };
        let column = &book_slopes[choices + c * per..choices + (c + 1) * per];
        print!("     {what:<34}");
        for (y, _) in years.iter().enumerate() {
            let quarters = match y {
                0..=4 => 4 * y..4 * y + 4,
                5 => 20..40,
                _ => 40..per,
            };
            print!(
                "{:>12}",
                signed(unit * column[quarters].iter().sum::<f64>())
            );
        }
        println!();
    }
    println!();

    // ---- What a slope does not see ----
    println!("4. What a slope does not see");
    let threshold = CHOICES.iter().position(|&row| row == 18).expect("row 18");
    let step = 0.05 * by[threshold].value;
    let mut sample = Vec::new();
    let few = &loans[..count.min(10_000)];
    for sign in [0.0, 1.0] {
        for loan in few {
            let mut inputs = as_given.clone();
            inputs[..loan.len()].copy_from_slice(loan);
            inputs[loan.len() + threshold] += sign * step;
            sample.extend(inputs);
        }
    }
    let got = runner.run(&everything, &sample)?;
    let (was, is): (f64, f64) = (got[..few.len()].iter().sum(), got[few.len()..].iter().sum());
    println!("   {} (now {})", by[threshold].what, by[threshold].value);
    println!(
        "   its derivative for the book is {}: no loan's provision slopes with it",
        signed(book_slopes[threshold])
    );
    println!(
        "   yet at {} the first {} loans set aside {} where they set aside {}: {:+.1}%",
        by[threshold].value + step,
        commas(few.len() as f64),
        commas(is),
        commas(was),
        100.0 * (is - was) / was
    );
    println!(
        "   it moves loans from one stage to another, which is a jump: the `audit` example is for those"
    );
    assert!(book_slopes[threshold] == 0.0 && is < was);
    Ok(())
}

/// A sum of money with its sign and thousands separated.
fn signed(x: f64) -> String {
    match x >= 0.0 {
        true => format!("+{}", commas(x)),
        false => commas(x),
    }
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
