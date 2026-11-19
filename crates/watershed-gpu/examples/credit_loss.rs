//! One loan's spreadsheet, run for the whole loan book.
//!
//! A credit risk team works out what to set aside for a mortgage (its
//! expected credit loss, under IFRS 9) in a workbook: the loan on one sheet,
//! three paths the economy might take on another, and the loan followed
//! down each path quarter by quarter. The workbook answers for one loan.
//! The bank has a million of them, and has to answer every quarter. The
//! usual ways are a macro that puts each loan into the sheet in turn, or
//! writing the model out again in another language and keeping the two in
//! step.
//!
//! This reads the workbook with `watershed-xlsx` and runs its formulas for
//! every loan at once. Nothing is rewritten by hand. It checks itself three
//! ways: every cell against the value saved in the workbook, 300 loans
//! against LibreOffice recalculating the workbook for each, and the plan
//! against the graph bit for bit.
//!
//! Then it asks what a spreadsheet cannot be asked in an afternoon: what
//! the whole book's provision would be if the slump were worse, or
//! likelier. Every loan is worked out again for each answer.
//!
//!     cargo run --release --features cuda --example credit_loss
//!
//! then optionally how many loans (200,000).
//!
//! The workbook is `shared/credit_loss.xlsx`, written by
//! `shared/credit_loss.py`, which also has LibreOffice make the reference.
//! The loans, the model's numbers and the economy are made up; the shape of
//! the calculation is the usual one. The `loans` example is the same kind
//! of model written straight onto a graph, on thousands of paths.

use std::time::{Duration, Instant};

use watershed_core::Value;
use watershed_gpu::Scenarios;
use watershed_xlsx::{Model, Workbook};

#[path = "shared/loan_book.rs"]
mod loan_book;
use loan_book::*;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What LibreOffice made of the workbook for some loans, a loan at a time.
const REFERENCE: &str = include_str!("shared/credit_loss_reference.csv");

/// How likely the slump is, and how bad.
const SLUMP: [&str; 2] = ["Model!B6", "Model!B8"];
/// The stage, the provision, and the loss expected in a year and over the
/// loan's life.
const ANSWERS: [&str; 4] = ["Loan!B13", "Loan!B14", "Loan!B15", "Loan!B16"];
const INPUTS: usize = LOAN.len() + SLUMP.len();

/// The same to about twelve figures.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6)
}

/// The book's provision: for each stage, how many loans, what they owe and
/// what is set aside for them.
#[derive(Default, Clone, Copy)]
struct Stage {
    loans: usize,
    owed: f64,
    provision: f64,
}

fn by_stage(loans: &[[f64; 7]], answers: &[f64]) -> [Stage; 3] {
    let mut stages = [Stage::default(); 3];
    for (loan, answer) in loans.iter().zip(answers.chunks(ANSWERS.len())) {
        let stage = &mut stages[(answer[0] as usize).clamp(1, 3) - 1];
        stage.loans += 1;
        stage.owed += loan[0];
        stage.provision += answer[1];
    }
    stages
}

/// Every loan with the slump as likely and as bad as given.
fn with_slump(loans: &[[f64; 7]], likely: f64, bad: f64) -> Vec<f64> {
    loans
        .iter()
        .flat_map(|loan| loan.iter().copied().chain([likely, bad]))
        .collect()
}

fn main() -> Result<()> {
    let count: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse()?,
        None => 200_000,
    };

    // ---- The workbook ----
    println!("1. The workbook");
    let started = Instant::now();
    let book = Workbook::open(WORKBOOK)?;
    let cells = |names: &[&str]| -> Result<Vec<_>> {
        Ok(names
            .iter()
            .map(|name| book.cell(name))
            .collect::<std::result::Result<_, _>>()?)
    };
    let inputs = cells(&[&LOAN[..], &SLUMP[..]].concat())?;
    let answers = cells(&ANSWERS)?;
    let mut model = Model::build(&book, &inputs, &answers)?;
    println!(
        "   credit_loss.xlsx: {} formulas, of which {} lie between the loan and its provision",
        book.formulas(),
        model.translated()
    );
    println!(
        "   read and translated in {}; it uses {}",
        time(started.elapsed()),
        model
            .functions()
            .iter()
            .map(|(name, n)| format!("{name} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let wrong = model.check();
    for line in wrong.iter().take(20) {
        println!("   {line}");
    }
    if !wrong.is_empty() {
        return Err("the graph does not agree with the workbook".into());
    }
    println!(
        "   every one of those cells comes to the value saved in the workbook for it ({})",
        model.translated() + model.folded()
    );
    let as_given: Vec<f64> = inputs
        .iter()
        .map(|&c| match book.value(c) {
            watershed_xlsx::Val::Num(x) => Ok(x),
            _ => Err(format!("{} does not hold a number", book.name(c))),
        })
        .collect::<std::result::Result<_, _>>()?;
    let (likely, bad) = (as_given[LOAN.len()], as_given[LOAN.len() + 1]);
    let output_ids = model.outputs();
    let plan = Scenarios::new(model.graph(), &model.inputs(), &output_ids)?;
    println!();

    // ---- Against LibreOffice ----
    println!("2. Against LibreOffice, a loan at a time");
    let mut lines = REFERENCE.lines();
    let note = lines.next().ok_or("the reference is empty")?;
    let each_ms: f64 = note
        .rsplit(": ")
        .next()
        .and_then(|took| took.strip_suffix(" ms a loan"))
        .ok_or("the reference does not say how long a loan took")?
        .parse()?;
    let rows: Vec<Vec<f64>> = lines
        .skip(1)
        .map(|line| line.split(',').map(str::parse).collect())
        .collect::<std::result::Result<_, _>>()?;
    let theirs: Vec<[f64; 7]> = rows
        .iter()
        .map(|row| row[..LOAN.len()].try_into().expect("seven numbers"))
        .collect();
    let ours = plan.run_cpu(&with_slump(&theirs, likely, bad));
    let mut worst = 0.0f64;
    for (row, here) in rows.iter().zip(ours.chunks(ANSWERS.len())) {
        for (&there, &here) in row[LOAN.len()..].iter().zip(here) {
            assert!(
                close(here, there),
                "{here} here and {there} in LibreOffice, for the loan {:?}",
                &row[..LOAN.len()]
            );
            worst = worst.max((here - there).abs() / there.abs().max(1.0));
        }
    }
    println!(
        "   {} loans, four answers each: the furthest from LibreOffice's is {worst:.0e} of it",
        rows.len()
    );
    println!(
        "   LibreOffice took {each_ms} ms a loan, the loan's cells set and the workbook recalculated"
    );
    println!();

    // ---- The book ----
    println!("3. The book");
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let loans: Vec<[f64; 7]> = (0..count).map(|_| made_up_loan(&mut rng)).collect();
    let book_inputs = with_slump(&loans, likely, bad);
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    let started = Instant::now();
    let results = plan.run_cpu(&book_inputs);
    let cpu_took = started.elapsed();
    println!(
        "   {} loans on {cores} CPU cores in {} ({} loans a second)",
        commas(count as f64),
        time(cpu_took),
        commas(count as f64 / cpu_took.as_secs_f64())
    );
    #[cfg(feature = "gpu")]
    let gpu = watershed_gpu::Gpu::new()?;
    #[cfg(feature = "gpu")]
    {
        gpu.run(&plan, &book_inputs[..INPUTS])?;
        let started = Instant::now();
        let on_gpu = gpu.run(&plan, &book_inputs)?;
        println!(
            "   the same on the GPU in {} ({} loans a second)",
            time(started.elapsed()),
            commas(count as f64 / started.elapsed().as_secs_f64())
        );
        // `exp` and powers may differ in the last digits. A loan sitting on
        // the edge of a stage could then fall the other side of it.
        let differ = on_gpu
            .chunks(ANSWERS.len())
            .zip(results.chunks(ANSWERS.len()))
            .filter(|(a, b)| a.iter().zip(*b).any(|(a, b)| !close(*a, *b)))
            .count();
        println!("   loans the GPU and the CPU answer differently for: {differ}");
        assert!(differ * 100_000 <= count, "the CPU and the GPU differ");
    }
    println!(
        "   LibreOffice, at {each_ms} ms a loan, would take {}",
        time(Duration::from_secs_f64(each_ms / 1000.0 * count as f64))
    );
    // The graph itself, a loan at a time, as the spreadsheet would do it:
    // it must agree with the plan to the last bit.
    let few = count.min(2_000);
    let started = Instant::now();
    for (loan, result) in book_inputs
        .chunks(INPUTS)
        .zip(results.chunks(ANSWERS.len()))
        .take(few)
    {
        model.set(loan)?;
        for (&id, &x) in output_ids.iter().zip(result) {
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
        "   the graph, a loan at a time, agrees to the last bit for the first {}: {} a loan",
        commas(few as f64),
        time(started.elapsed() / few as u32)
    );
    println!();
    let stages = by_stage(&loans, &results);
    println!(
        "   {:<24} {:>10} {:>18} {:>16} {:>8}",
        "", "loans", "owed", "set aside", "cover"
    );
    let names = ["1, sound", "2, looking shakier", "3, in default"];
    for (name, stage) in names.iter().zip(&stages) {
        println!(
            "   stage {name:<18} {:>10} {:>18} {:>16} {:>7.2}%",
            commas(stage.loans as f64),
            commas(stage.owed),
            commas(stage.provision),
            100.0 * stage.provision / stage.owed.max(1.0)
        );
    }
    let owed: f64 = stages.iter().map(|s| s.owed).sum();
    let provision: f64 = stages.iter().map(|s| s.provision).sum();
    println!(
        "   {:<24} {:>10} {:>18} {:>16} {:>7.2}%",
        "all",
        commas(count as f64),
        commas(owed),
        commas(provision),
        100.0 * provision / owed
    );
    println!();

    // ---- A worse slump, or a likelier one ----
    println!("4. If the slump were worse, or likelier");
    // Each answer is every loan again. On CPU threads a sample of the book
    // stands for it; a GPU takes the whole book.
    #[cfg(feature = "gpu")]
    let sample = &loans[..];
    #[cfg(not(feature = "gpu"))]
    let sample = &loans[..count.min(10_000)];
    // Scaled so that the sample, with the slump as given, is the book.
    let sample_as_given: f64 = results
        .chunks(ANSWERS.len())
        .take(sample.len())
        .map(|answer| answer[1])
        .sum();
    let scale = provision / sample_as_given;
    let how_bad = [0.5, 1.0, 1.5, 2.0];
    let how_likely = [0.1, 0.2, 0.3, 0.4, 0.5];
    let mut asked = Vec::with_capacity(how_bad.len() * how_likely.len() * sample.len() * INPUTS);
    for &likely in &how_likely {
        for &bad in &how_bad {
            asked.extend(with_slump(sample, likely, bad));
        }
    }
    let runs = asked.len() / INPUTS;
    let started = Instant::now();
    #[cfg(feature = "gpu")]
    let told = gpu.run(&plan, &asked)?;
    #[cfg(not(feature = "gpu"))]
    let told = plan.run_cpu(&asked);
    println!(
        "   {} loans, {} slumps: {} workbooks' worth in {}{}",
        commas(sample.len() as f64),
        how_bad.len() * how_likely.len(),
        commas(runs as f64),
        time(started.elapsed()),
        match sample.len() == count {
            true => String::new(),
            false => format!(", scaled up to the book (×{scale:.1})"),
        }
    );
    println!("   provision for the book, in millions, and the share of what is owed in stage 2");
    print!("   {:<22}", "slump as bad as given ×");
    for bad in how_bad {
        print!("{bad:>16}");
    }
    println!();
    let per = sample.len() * ANSWERS.len();
    for (l, likely) in how_likely.iter().enumerate() {
        print!("   {:<22}", format!("{:.0}% likely", likely * 100.0));
        for b in 0..how_bad.len() {
            let at = (l * how_bad.len() + b) * per;
            let stages = by_stage(sample, &told[at..at + per]);
            let set_aside: f64 = stages.iter().map(|s| s.provision).sum();
            let all: f64 = stages.iter().map(|s| s.owed).sum();
            print!(
                "{:>16}",
                format!(
                    "{:.1} ({:.0}%)",
                    set_aside * scale / 1e6,
                    100.0 * stages[1].owed / all
                )
            );
        }
        println!();
    }
    println!();

    // ---- One loan changes ----
    println!("5. One loan falls behind");
    model.set(&as_given)?;
    let before = model.value(answers[1]).ok_or("no provision")?;
    let days = model.inputs()[6];
    let started = Instant::now();
    model.graph_mut().set_value(days, Value::F64(45.0))?;
    let report = model.graph_mut().stabilize();
    let took = started.elapsed();
    let after = model.value(answers[1]).ok_or("no provision")?;
    println!(
        "   the workbook's own loan, 45 days past due: its provision goes from {} to {}",
        commas(before),
        commas(after)
    );
    println!(
        "   {} of {} cells worked out again, in {}",
        report.recalculated,
        model.translated(),
        time(took)
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
