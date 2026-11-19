//! A spreadsheet model, tried a million ways.
//!
//! Someone has built a model in Excel: a valuation, a budget, a project's
//! finances. Some of its inputs are guesses, and they want to know how the
//! answer is spread when the guesses are. One way is to put new numbers in
//! the input cells and recalculate the workbook, thousands of times over.
//!
//! This reads the workbook with `watershed-xlsx`, which turns the formulas
//! between the chosen inputs and the chosen outputs into a watershed graph,
//! and runs the graph for every set of inputs at once. Nothing is rewritten
//! by hand, and the workbook is its own check: Excel stores the last value
//! it calculated for every cell, so each cell of the graph is compared with
//! what Excel got.
//!
//!     cargo run --release --features cuda --example spreadsheet -- model.xlsx \
//!         "'Input sheet'!B26=0.02:0.09" "'Input sheet'!B29=0.10:0.18" \
//!         "'Valuation output'!B33"
//!
//! After the workbook: a cell with `=low:high` is an input to vary, between
//! those and most likely what the workbook has now; a cell alone is an
//! output. Last, optionally, how many trials (a million). What is
//! understood, and what is refused, is in `watershed-xlsx`'s documentation.

use std::time::{Duration, Instant};

use watershed_core::Value;
use watershed_gpu::Scenarios;
use watershed_xlsx::{Cell, Model, Val, Workbook};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The same to about twelve figures.
#[cfg(feature = "gpu")]
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6) || (a.is_nan() && b.is_nan())
}

fn number(v: &Val) -> Option<f64> {
    match v {
        Val::Num(x) => Some(*x),
        _ => None,
    }
}

// ---- Running it ----

/// An input to vary: between `low` and `high`, most likely what the
/// workbook has now.
struct Vary {
    cell: Cell,
    low: f64,
    now: f64,
    high: f64,
}

impl Vary {
    /// A draw from the triangle, given a number between 0 and 1.
    fn draw(&self, u: f64) -> f64 {
        let (span, left) = (self.high - self.low, self.now - self.low);
        if u * span < left {
            self.low + (u * span * left).sqrt()
        } else {
            self.high - ((1.0 - u) * span * (self.high - self.now)).sqrt()
        }
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

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("give a workbook")?;
    let started = Instant::now();
    let book = Workbook::open(&path)?;
    println!(
        "{}: {} sheets, {} formulas (read in {})",
        path.rsplit('/').next().unwrap_or(&path),
        book.sheets().count(),
        book.formulas(),
        time(started.elapsed())
    );

    let (mut vary, mut outputs, mut trials) = (Vec::new(), Vec::new(), 1_000_000usize);
    for arg in args {
        if let Some((cell, between)) = arg.split_once('=') {
            let cell = book.cell(cell)?;
            let (low, high) = between.split_once(':').ok_or("an input is CELL=low:high")?;
            let now = number(&book.value(cell)).ok_or("an input must hold a number")?;
            let (low, high): (f64, f64) = (low.parse()?, high.parse()?);
            if !(low <= now && now <= high && low < high) {
                return Err(format!(
                    "{} is {now} in the workbook, which is not between {low} and {high}",
                    book.name(cell)
                )
                .into());
            }
            vary.push(Vary {
                cell,
                low,
                now,
                high,
            });
        } else if let Ok(n) = arg.replace('_', "").parse() {
            trials = n;
        } else {
            outputs.push(book.cell(&arg)?);
        }
    }
    if vary.is_empty() || outputs.is_empty() {
        return Err("give at least one input (CELL=low:high) and one output (CELL)".into());
    }

    // The formulas between the inputs and the outputs, as a graph.
    let started = Instant::now();
    let cells: Vec<Cell> = vary.iter().map(|v| v.cell).collect();
    let mut model = Model::build(&book, &cells, &outputs)?;
    println!(
        "between the {} inputs and the {} outputs: {} cells became nodes; {} more were followed to a fixed value (built in {})",
        vary.len(),
        outputs.len(),
        model.translated(),
        model.folded(),
        time(started.elapsed())
    );
    println!(
        "  functions met on the way: {}",
        model
            .functions()
            .iter()
            .map(|(name, n)| format!("{name} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Each cell against what Excel calculated.
    let wrong = model.check();
    let checked = model.translated() + model.folded();
    if wrong.is_empty() {
        println!("checked: all {checked} of those cells come to what Excel has for them");
    } else {
        println!("{} of {checked} cells differ from Excel:", wrong.len());
        for line in wrong.iter().take(20) {
            println!("  {line}");
        }
        return Err("the graph does not agree with the workbook".into());
    }
    println!();

    let output_ids = model.outputs();
    let plan = Scenarios::new(model.graph(), &model.inputs(), &output_ids)?;

    // The trials.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut inputs = Vec::with_capacity(trials * vary.len());
    for _ in 0..trials {
        for v in &vary {
            inputs.push(v.draw(rng.unit()));
        }
    }
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    let started = Instant::now();
    let results = plan.run_cpu(&inputs);
    let cpu_took = started.elapsed();
    println!(
        "{} trials on {cores} CPU cores in {} ({} recalculations of the model a second)",
        commas(trials as f64),
        time(cpu_took),
        commas(trials as f64 / cpu_took.as_secs_f64())
    );
    #[cfg(feature = "gpu")]
    {
        let gpu = watershed_gpu::Gpu::new()?;
        gpu.run(&plan, &inputs[..vary.len()])?;
        let started = Instant::now();
        let on_gpu = gpu.run(&plan, &inputs)?;
        let took = started.elapsed();
        println!(
            "the same on the GPU in {} ({} a second)",
            time(took),
            commas(trials as f64 / took.as_secs_f64())
        );
        // Powers and logarithms may differ in the last digits.
        assert!(
            on_gpu
                .iter()
                .zip(&results)
                .all(|(a, b)| close(*a, *b) || (!a.is_finite() && !b.is_finite())),
            "the CPU and the GPU differ"
        );
    }
    // The graph itself, a trial at a time, as a recalculating spreadsheet
    // would do it; it must agree to the last bit.
    let few = trials.min(20_000);
    let started = Instant::now();
    for (trial, result) in inputs
        .chunks(vary.len())
        .zip(results.chunks(outputs.len()))
        .take(few)
    {
        model.set(trial)?;
        for (&id, &x) in output_ids.iter().zip(result) {
            let Value::F64(here) = model.graph().value(id) else {
                unreachable!("an output is a number")
            };
            assert!(
                here.to_bits() == x.to_bits() || (here.is_nan() && x.is_nan()),
                "the graph and the plan differ"
            );
        }
    }
    let each = started.elapsed() / few as u32;
    println!(
        "the graph, a trial at a time: {} each, so {} for all of them",
        time(each),
        time(each * trials as u32)
    );
    println!();

    // What the trials say, for each output.
    for (o, &c) in outputs.iter().enumerate() {
        let mut all: Vec<f64> = results
            .chunks(outputs.len())
            .map(|r| r[o])
            .filter(|x| x.is_finite())
            .collect();
        all.sort_by(f64::total_cmp);
        let now = number(&book.value(c)).unwrap_or(f64::NAN);
        let n = all.len();
        let at = |share: f64| all[((n as f64 * share) as usize).min(n - 1)];
        println!("{}", book.name(c));
        println!("  in the workbook now      {:>14}", figure(now));
        println!(
            "  average over the trials  {:>14}",
            figure(all.iter().sum::<f64>() / n as f64)
        );
        println!(
            "  1 in 20 is below {}, half are below {}, 1 in 20 is above {}",
            figure(at(0.05)),
            figure(at(0.5)),
            figure(at(0.95))
        );
        if n < trials {
            println!(
                "  ({} trials gave an error, as Excel would have)",
                commas((trials - n) as f64)
            );
        }
        histogram(&all);
        println!();
    }

    // Which input matters most: each to its low and its high, the rest as
    // the workbook has them.
    let mut swings = Vec::new();
    let base: Vec<f64> = vary.iter().map(|v| v.now).collect();
    let mut tries = Vec::new();
    for (k, v) in vary.iter().enumerate() {
        for x in [v.low, v.high] {
            let mut one = base.clone();
            one[k] = x;
            tries.extend(one);
        }
    }
    let ends = plan.run_cpu(&tries);
    for (k, v) in vary.iter().enumerate() {
        let (low, high) = (
            ends[2 * k * outputs.len()],
            ends[(2 * k + 1) * outputs.len()],
        );
        swings.push((book.name(v.cell), v.low, v.high, low, high));
    }
    swings.sort_by(|a, b| (b.4 - b.3).abs().total_cmp(&(a.4 - a.3).abs()));
    println!(
        "what moves {} most (each input to its low and its high, the rest as they are)",
        book.name(outputs[0])
    );
    for (name, low, high, at_low, at_high) in &swings {
        println!(
            "  {name:<24} {:>8} to {:<8} gives {:>12} to {:<12}",
            figure(*low),
            figure(*high),
            figure(*at_low),
            figure(*at_high)
        );
    }
    println!();

    // One cell edited, as someone working in the model would.
    model.set(&base)?;
    let first = model.inputs()[0];
    let started = Instant::now();
    model
        .graph_mut()
        .set_value(first, Value::F64(vary[0].high))?;
    let report = model.graph_mut().stabilize();
    println!(
        "one input edited on the graph: {} of {} nodes worked out again, in {}",
        report.recalculated,
        model.translated(),
        time(started.elapsed())
    );
    Ok(())
}

/// How the trials are spread, leaving out the rarest half a percent at each
/// end so the middle can be seen.
fn histogram(sorted: &[f64]) {
    const BARS: usize = 17;
    const WIDTH: usize = 48;
    let n = sorted.len();
    let (low, high) = (sorted[n / 200], sorted[n - 1 - n / 200]);
    if high <= low {
        return;
    }
    let mut counts = [0usize; BARS];
    for &x in sorted {
        if (low..=high).contains(&x) {
            counts[(((x - low) / (high - low) * BARS as f64) as usize).min(BARS - 1)] += 1;
        }
    }
    let most = counts.iter().copied().max().unwrap_or(1).max(1);
    for (bar, &count) in counts.iter().enumerate() {
        let from = low + (high - low) * bar as f64 / BARS as f64;
        println!(
            "  {:>14} {}",
            figure(from),
            "#".repeat((count * WIDTH).div_ceil(most))
        );
    }
}

/// A number to four or five figures, with thousands separated when large.
fn figure(x: f64) -> String {
    if !x.is_finite() {
        return format!("{x}");
    }
    if x.abs() >= 1000.0 {
        return commas(x);
    }
    let places = if x.abs() >= 100.0 {
        1
    } else if x.abs() >= 1.0 {
        2
    } else {
        4
    };
    format!("{x:.places$}")
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
    } else {
        format!("{:.0} minutes", s / 60.0)
    }
}
