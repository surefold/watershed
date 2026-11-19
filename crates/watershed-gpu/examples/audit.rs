//! A workbook tried everywhere its inputs can go.
//!
//! A spreadsheet model is checked by putting in a few cases and looking at
//! the answers. Where it goes wrong is usually somewhere nobody looked: an
//! input near zero, two inputs large together, the row a formula was copied
//! into by hand. And when a workbook is changed, the question is whether
//! anything moved that was not meant to.
//!
//! This reads a workbook with `watershed-xlsx` and runs it for points
//! spread over the whole of its inputs' ranges, and at every corner of
//! them. It reports where an answer is an error or negative, and the least
//! and greatest each answer gets. Then it moves one input at a time through
//! its range, from many starting points, and says for each answer whether
//! it rises, falls or turns round, and where it jumps. Given a second
//! version of the workbook, it lists the cells that were changed and then
//! runs both for the same points, to show which answers moved, how far, and
//! for which inputs.
//!
//! It finds things to look at. Whether a jump or a turn is a mistake is for
//! whoever knows the model to say.
//!
//!     cargo run --release --features cuda --example audit
//!
//! On its own it takes the `credit_loss` example's workbook and a later
//! version of it, which `shared/credit_loss.py` writes with one change that
//! is meant and one slip. Or give a workbook, its inputs and its answers:
//!
//!     cargo run --release --example audit -- model.xlsx \
//!         "Inputs!B4=1000:500000" "Inputs!B6=1:40:1" "Results!B9" \
//!         --against model_v2.xlsx 500000
//!
//! A cell with `=low:high` is an input and its range; a third number is the
//! step it moves in (`1:40:1` is whole numbers). A cell alone is an answer.
//! Last, optionally, how many points (200,000).

use std::time::{Duration, Instant};

use watershed_gpu::Scenarios;
use watershed_xlsx::{Cell, Model, Val, Workbook};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const SHARED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/shared/");
/// What is tried when nothing is given: the loan's seven cells, wider than
/// any loan book, and its four answers.
const ON_ITS_OWN: [&str; 11] = [
    "Loan!B4=20000:1500000",
    "Loan!B5=0:0.12",
    "Loan!B6=1:100:1",
    "Loan!B7=20000:2000000",
    "Loan!B8=0.0005:0.2",
    "Loan!B9=0.0005:0.9",
    "Loan!B10=0:400:1",
    "Loan!B13",
    "Loan!B14",
    "Loan!B15",
    "Loan!B16",
];
/// How many starting points each input is moved from, and through how many
/// values when it does not move in steps.
const LINES: usize = 300;
const ALONG: usize = 48;

/// An input and the range it is tried over.
struct Vary {
    name: String,
    low: f64,
    high: f64,
    /// What it moves in, if not anything at all: 1 for whole numbers.
    step: Option<f64>,
}

impl Vary {
    /// The value a number between 0 and 1 stands for.
    fn at(&self, u: f64) -> f64 {
        let x = self.low + u * (self.high - self.low);
        match self.step {
            Some(step) => (self.low + ((x - self.low) / step).round() * step).min(self.high),
            None => x,
        }
    }

    /// The values it is moved through, from low to high.
    fn along(&self) -> Vec<f64> {
        match self.step {
            Some(step) => {
                let steps = ((self.high - self.low) / step).floor() as usize;
                // Every value, unless there are too many to be worth it.
                let stride = steps.div_ceil(512).max(1);
                (0..=steps)
                    .step_by(stride)
                    .map(|k| self.low + k as f64 * step)
                    .collect()
            }
            None => (0..ALONG)
                .map(|k| self.at(k as f64 / (ALONG - 1) as f64))
                .collect(),
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

/// A workbook as a plan, on the GPU when there is one.
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

/// Reads a workbook, translates what lies between the inputs and the
/// answers, and checks it against the values saved in the workbook.
fn read(path: &str, vary: &[String], answers: &[String]) -> Result<(Workbook, Model, Scenarios)> {
    let book = Workbook::open(path)?;
    let cells = |names: &[String]| -> Result<Vec<Cell>> {
        Ok(names
            .iter()
            .map(|name| book.cell(name))
            .collect::<std::result::Result<_, _>>()?)
    };
    let mut model = Model::build(&book, &cells(vary)?, &cells(answers)?)?;
    let wrong = model.check();
    for line in wrong.iter().take(20) {
        println!("   {line}");
    }
    if !wrong.is_empty() {
        return Err(format!("{path}: the graph does not agree with the workbook").into());
    }
    println!(
        "   {}: {} formulas; the {} between the inputs and the answers come to the values saved for them",
        path.rsplit('/').next().unwrap_or(path),
        book.formulas(),
        model.translated() + model.folded()
    );
    let plan = Scenarios::new(model.graph(), &model.inputs(), &model.outputs())?;
    Ok((book, model, plan))
}

/// Where some of the points are: each input that those points keep to less
/// than three fifths of its range, with the part they keep to.
fn whereabouts(vary: &[Vary], points: &[f64], which: &[usize]) -> String {
    let mut narrow = Vec::new();
    for (k, v) in vary.iter().enumerate() {
        let values = which.iter().map(|&p| points[p * vary.len() + k]);
        let low = values.clone().fold(f64::INFINITY, f64::min);
        let high = values.fold(f64::NEG_INFINITY, f64::max);
        if high - low < 0.6 * (v.high - v.low) {
            narrow.push(match low == high {
                true => format!("{} at {}", v.name, figure(low)),
                false => format!("{} from {} to {}", v.name, figure(low), figure(high)),
            });
        }
    }
    match narrow.is_empty() {
        true => "all over the inputs' ranges".into(),
        false => format!("all with {}", narrow.join(", ")),
    }
}

/// One point's inputs, written out.
fn point(vary: &[Vary], points: &[f64], p: usize) -> String {
    vary.iter()
        .enumerate()
        .map(|(k, v)| format!("{} = {}", v.name, figure(points[p * vary.len() + k])))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What an answer does as one input moves through its range, over every
/// starting point.
#[derive(Default)]
struct Moves {
    still: usize,
    rises: usize,
    falls: usize,
    turns: usize,
    /// How many lines have a jump.
    jumping: usize,
    /// For an input that moves in steps, how many jumps each step has.
    jumps: Vec<usize>,
    /// For any other, the largest jump found: where, and from what to what.
    largest: Option<(f64, f64, f64)>,
    /// The largest step up and the largest step down: the inputs there, and
    /// what the answer went from and to.
    up: Option<(Vec<f64>, f64, f64)>,
    down: Option<(Vec<f64>, f64, f64)>,
}

/// A step in a line that stands out from the steps either side of it, to
/// be closed in on: a jump if the answer still differs across it when the
/// input has all but stopped moving.
struct Suspect {
    /// The line's starting point, the input moved and the answer watched.
    start: Vec<f64>,
    input: usize,
    answer: usize,
    line: usize,
    /// The input either side of the step, and the answer there.
    low: (f64, f64),
    high: (f64, f64),
    /// The largest the answer is on the line, to judge the step against.
    size: f64,
}

fn main() -> Result<()> {
    let (mut path, mut against) = (None, None);
    let (mut inputs, mut answers, mut count) = (Vec::new(), Vec::new(), 200_000usize);
    let mut ranges = Vec::new();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Nothing given, or only how many points: the credit loss workbook.
    let on_its_own = args
        .iter()
        .all(|a| a.replace('_', "").parse::<usize>().is_ok());
    if on_its_own {
        let points = args.pop();
        args = vec![format!("{SHARED}credit_loss.xlsx")];
        args.extend(ON_ITS_OWN.map(String::from));
        args.extend([
            "--against".into(),
            format!("{SHARED}credit_loss_changed.xlsx"),
        ]);
        args.extend(points);
    }
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--against" {
            against = Some(args.next().ok_or("--against takes a workbook")?);
        } else if let Some((cell, range)) = arg.split_once('=') {
            let mut parts = range.split(':').map(str::parse::<f64>);
            let (Some(low), Some(high)) = (parts.next(), parts.next()) else {
                return Err("an input is CELL=low:high or CELL=low:high:step".into());
            };
            let (low, high, step) = (low?, high?, parts.next().transpose()?);
            if !(low < high && step.is_none_or(|s| s > 0.0)) {
                return Err(format!("{arg}: the range must run upwards").into());
            }
            inputs.push(cell.to_string());
            ranges.push((low, high, step));
        } else if let Ok(n) = arg.replace('_', "").parse() {
            count = n;
        } else if path.is_none() {
            path = Some(arg);
        } else {
            answers.push(arg);
        }
    }
    let path = path.ok_or("give a workbook")?;
    if inputs.is_empty() || answers.is_empty() {
        return Err("give at least one input (CELL=low:high) and one answer (CELL)".into());
    }
    let (n, m) = (inputs.len(), answers.len());

    // ---- The workbook ----
    println!("1. The workbook");
    let runner = Runner::new()?;
    let (book, _, plan) = read(&path, &inputs, &answers)?;
    let other = match &against {
        Some(path) => Some(read(path, &inputs, &answers)?),
        None => None,
    };
    let vary: Vec<Vary> = inputs
        .iter()
        .zip(&ranges)
        .map(|(name, &(low, high, step))| {
            Ok(Vary {
                name: book.name(book.cell(name)?),
                low,
                high,
                step,
            })
        })
        .collect::<Result<_>>()?;
    let names: Vec<String> = answers
        .iter()
        .map(|a| Ok(book.name(book.cell(a)?)))
        .collect::<Result<_>>()?;
    println!();

    // ---- Everywhere ----
    println!("2. Everywhere its inputs can go");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut points = Vec::with_capacity((count + (1 << n.min(12))) * n);
    // Every corner first, where things most often break.
    if n <= 12 {
        for corner in 0..1usize << n {
            points.extend(vary.iter().enumerate().map(|(k, v)| match corner >> k & 1 {
                0 => v.low,
                _ => v.high,
            }));
        }
    }
    let corners = points.len() / n;
    for _ in 0..count {
        points.extend(vary.iter().map(|v| v.at(rng.unit())));
    }
    let total = points.len() / n;
    let started = Instant::now();
    let results = runner.run(&plan, &points)?;
    let took = started.elapsed();
    println!(
        "   {} points ({} of them corners) in {} ({} a second)",
        commas(total as f64),
        commas(corners as f64),
        time(took),
        commas(total as f64 / took.as_secs_f64())
    );
    for (o, name) in names.iter().enumerate() {
        let of = |p: usize| results[p * m + o];
        let errors: Vec<usize> = (0..total).filter(|&p| !of(p).is_finite()).collect();
        let negative: Vec<usize> = (0..total).filter(|&p| of(p) < 0.0).collect();
        let fine = (0..total).filter(|&p| of(p).is_finite());
        let least = fine.clone().min_by(|&a, &b| of(a).total_cmp(&of(b)));
        let most = fine.max_by(|&a, &b| of(a).total_cmp(&of(b)));
        println!("   {name}");
        if let (Some(least), Some(most)) = (least, most) {
            println!("     from {} to {}", figure(of(least)), figure(of(most)));
            println!("     greatest with {}", point(&vary, &points, most));
        }
        for (what, found) in [("an error", &errors), ("negative", &negative)] {
            match found.first() {
                None => println!("     never {what}"),
                Some(&first) => {
                    println!(
                        "     {what} at {} points, {}",
                        commas(found.len() as f64),
                        whereabouts(&vary, &points, found)
                    );
                    println!("       for one, {}", point(&vary, &points, first));
                }
            }
        }
    }
    println!();

    // ---- One input at a time ----
    println!("3. One input at a time");
    let starts: Vec<f64> = (0..LINES * n)
        .map(|at| vary[at % n].at(rng.unit()))
        .collect();
    let alongs: Vec<Vec<f64>> = vary.iter().map(Vary::along).collect();
    let mut lines = Vec::new();
    for (k, along) in alongs.iter().enumerate() {
        for start in starts.chunks(n) {
            for &x in along {
                lines.extend(start.iter().enumerate().map(|(j, &s)| match j == k {
                    true => x,
                    false => s,
                }));
            }
        }
    }
    let started = Instant::now();
    let moved = runner.run(&plan, &lines)?;
    println!(
        "   each input through its range from {LINES} starting points, the rest held: {} points in {}",
        commas((lines.len() / n) as f64),
        time(started.elapsed())
    );
    let mut at = 0;
    let mut all: Vec<Vec<Moves>> = Vec::new();
    let mut suspects = Vec::new();
    for (k, along) in alongs.iter().enumerate() {
        let mut moves: Vec<Moves> = (0..m).map(|_| Moves::default()).collect();
        for start in starts.chunks(n) {
            for (o, moves) in moves.iter_mut().enumerate() {
                let line: Vec<f64> = (0..along.len()).map(|j| moved[(at + j) * m + o]).collect();
                let size = line.iter().fold(0.0f64, |a, x| a.max(x.abs()));
                let stood_out = follow(&line, size, vary[k].step.is_some(), moves);
                // The largest step each way, for an answer that mostly goes
                // the other way.
                for (j, step) in line.windows(2).map(|w| w[1] - w[0]).enumerate() {
                    let best = if step > 0.0 {
                        &mut moves.up
                    } else {
                        &mut moves.down
                    };
                    if step != 0.0
                        && best
                            .as_ref()
                            .is_none_or(|(_, a, b)| step.abs() > (b - a).abs())
                    {
                        let mut there = start.to_vec();
                        there[k] = along[j + 1];
                        *best = Some((there, line[j], line[j + 1]));
                    }
                }
                if vary[k].step.is_some() {
                    moves.jumping += usize::from(!stood_out.is_empty());
                    moves.jumps.resize(along.len() - 1, 0);
                    for j in stood_out {
                        moves.jumps[j] += 1;
                    }
                } else {
                    suspects.extend(stood_out.into_iter().map(|j| Suspect {
                        start: start.to_vec(),
                        input: k,
                        answer: o,
                        line: at,
                        low: (along[j], line[j]),
                        high: (along[j + 1], line[j + 1]),
                        size,
                    }));
                }
            }
            at += along.len();
        }
        all.push(moves);
    }
    // Close in on each step that stood out, halving it forty times and
    // keeping the half the answer moves more across.
    for _ in 0..40 {
        let mut middles = Vec::with_capacity(suspects.len() * n);
        for s in &suspects {
            let middle = 0.5 * (s.low.0 + s.high.0);
            middles.extend(
                s.start
                    .iter()
                    .enumerate()
                    .map(|(j, &x)| match j == s.input {
                        true => middle,
                        false => x,
                    }),
            );
        }
        let there = runner.run(&plan, &middles)?;
        for (i, s) in suspects.iter_mut().enumerate() {
            let middle = (0.5 * (s.low.0 + s.high.0), there[i * m + s.answer]);
            match (middle.1 - s.low.1).abs() >= (s.high.1 - middle.1).abs() {
                true => s.high = middle,
                false => s.low = middle,
            }
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for s in &suspects {
        let gap = (s.high.1 - s.low.1).abs();
        if gap > 1e-3 * s.size {
            let moves = &mut all[s.input][s.answer];
            moves.jumping += usize::from(seen.insert((s.input, s.answer, s.line)));
            if moves.largest.is_none_or(|(_, a, b)| gap > (b - a).abs()) {
                moves.largest = Some((s.low.0, s.low.1, s.high.1));
            }
        }
    }
    for ((v, along), moves) in vary.iter().zip(&alongs).zip(&all) {
        println!("   {}", v.name);
        for (name, moves) in names.iter().zip(moves) {
            println!("     {name:<12} {}", told(moves, along));
            // Going against the grain on a few lines is worth a look.
            let (rising, falling) = (moves.rises + moves.turns, moves.falls + moves.turns);
            let rare = |n: usize| (1..=LINES / 20).contains(&n);
            let against = match (
                rare(falling) && rising > falling,
                rare(rising) && falling > rising,
            ) {
                (true, _) => moves.down.as_ref().map(|d| ("falls", d)),
                (_, true) => moves.up.as_ref().map(|u| ("rises", u)),
                _ => None,
            };
            if let Some((what, (there, from, to))) = against {
                println!(
                    "       where it {what}, for one: from {} to {}, reaching {}",
                    figure(*from),
                    figure(*to),
                    point(&vary, there, 0)
                );
            }
        }
    }
    println!();

    // ---- Against another version ----
    let Some((other_book, _, other_plan)) = other else {
        return Ok(());
    };
    println!("4. Against the other version");
    let mut changed = Vec::new();
    let both: std::collections::BTreeSet<(usize, u32, u32)> = book
        .cells()
        .chain(other_book.cells())
        .map(|c| (c.sheet, c.row, c.col))
        .collect();
    for (sheet, row, col) in both {
        let c = Cell { sheet, row, col };
        let show = |b: &Workbook| match (b.formula(c), b.value(c)) {
            (Some(f), _) => Some(format!("={f}")),
            (None, Val::Empty) => None,
            (None, Val::Num(x)) => Some(format!("{x}")),
            (None, v) => Some(format!("{v:?}")),
        };
        // A formula's value moving is what follows, not what was changed.
        let (was, is) = (show(&book), show(&other_book));
        if was != is {
            changed.push((book.name(c), was, is));
        }
    }
    println!("   cells changed: {}", changed.len());
    // On its own, this run is also a test: the two changes the script made
    // are found, and the slip is seen to reach only loans with 57 quarters
    // or more left, and nothing about the next year.
    assert!(!on_its_own || changed.len() == 2, "two cells were changed");
    for (name, was, is) in changed.iter().take(12) {
        let show = |x: &Option<String>| x.clone().unwrap_or_else(|| "nothing".into());
        println!("     {name}");
        println!("       was {}", show(was));
        println!("       is  {}", show(is));
    }
    let started = Instant::now();
    let theirs = runner.run(&other_plan, &points)?;
    println!(
        "   the same {} points through it in {}",
        commas(total as f64),
        time(started.elapsed())
    );
    for (o, name) in names.iter().enumerate() {
        let apart = |p: usize| {
            let (a, b) = (results[p * m + o], theirs[p * m + o]);
            match a.is_finite() && b.is_finite() {
                true => (a - b).abs() / a.abs().max(b.abs()).max(1e-9),
                false => f64::from(u8::from(a.is_finite() != b.is_finite())),
            }
        };
        let differ: Vec<usize> = (0..total).filter(|&p| apart(p) > 1e-12).collect();
        if on_its_own {
            match o {
                2 => assert!(differ.is_empty(), "the next year's loss moved"),
                3 => assert!(
                    !differ.is_empty() && differ.iter().all(|&p| points[p * n + 2] >= 57.0),
                    "the slip reached loans it should not have"
                ),
                _ => assert!(
                    !differ.is_empty(),
                    "the change that was meant changed nothing"
                ),
            }
        }
        let Some(&worst) = differ
            .iter()
            .max_by(|&&a, &&b| apart(a).total_cmp(&apart(b)))
        else {
            println!("   {name}: the same at every point");
            continue;
        };
        println!(
            "   {name}: differs at {} points ({:.1}%), {}",
            commas(differ.len() as f64),
            100.0 * differ.len() as f64 / total as f64,
            whereabouts(&vary, &points, &differ)
        );
        let mut by: Vec<f64> = differ.iter().map(|&p| apart(p)).collect();
        by.sort_by(f64::total_cmp);
        println!(
            "     half of those by less than {}, the most by {}: {}",
            share(by[by.len() / 2]),
            share(by[by.len() - 1]),
            was_and_is(results[worst * m + o], theirs[worst * m + o])
        );
        println!("       with {}", point(&vary, &points, worst));
    }
    Ok(())
}

/// Adds to `moves` whether one line of an answer rises, falls or turns, and
/// gives the steps that stand out from the steps either side of them: by
/// eight times where the input moves in steps, and by three where it does
/// not, since those are then closed in on.
fn follow(line: &[f64], size: f64, in_steps: bool, moves: &mut Moves) -> Vec<usize> {
    let steps: Vec<f64> = line.windows(2).map(|w| w[1] - w[0]).collect();
    let (mut up, mut down) = (false, false);
    for &step in &steps {
        // An error on the way counts as neither.
        up |= step > 1e-12 * size;
        down |= step < -1e-12 * size;
    }
    match (up, down) {
        (false, false) => moves.still += 1,
        (true, false) => moves.rises += 1,
        (false, true) => moves.falls += 1,
        (true, true) => moves.turns += 1,
    }
    let by = if in_steps { 8.0 } else { 3.0 };
    let beside = |j: Option<usize>| {
        j.and_then(|j| steps.get(j))
            .filter(|s| s.is_finite())
            .map_or(0.0, |s| s.abs())
    };
    (0..steps.len())
        .filter(|&j| {
            let around = beside(j.checked_sub(1)).max(beside(Some(j + 1)));
            steps[j].abs() > 1e-3 * size && steps[j].abs() > by * around
        })
        .collect()
}

/// What an answer does as an input moves, in a line.
fn told(moves: &Moves, along: &[f64]) -> String {
    let mut parts = Vec::new();
    for (what, n) in [
        ("does not move", moves.still),
        ("rises", moves.rises),
        ("falls", moves.falls),
        ("turns round", moves.turns),
    ] {
        match n {
            0 => {}
            LINES if what == "does not move" => parts.push("never moves".into()),
            LINES => parts.push(format!("always {what}")),
            n => parts.push(format!("{what} on {n}")),
        }
    }
    let mut out = parts.join(", ");
    if moves.still + moves.rises + moves.falls + moves.turns != LINES.max(moves.still) {
        out.push_str(&format!(" of {LINES}"));
    }
    if moves.jumping == 0 {
        return out;
    }
    out.push_str(&format!("; jumps on {}", moves.jumping));
    if let Some((at, from, to)) = moves.largest {
        out.push_str(&format!(
            ", the largest at {} from {} to {}",
            figure(at),
            figure(from),
            figure(to)
        ));
    } else {
        // The steps it jumps at most often.
        let mut places: Vec<usize> = (0..moves.jumps.len())
            .filter(|&j| moves.jumps[j] > 0)
            .collect();
        places.sort_by_key(|&j| std::cmp::Reverse(moves.jumps[j]));
        let most: Vec<String> = places
            .iter()
            .take(3)
            .map(|&j| format!("{} to {}", figure(along[j]), figure(along[j + 1])))
            .collect();
        out.push_str(&format!(", most often from {}", most.join(", from ")));
    }
    out
}

/// A number to four or five figures, with thousands separated when large.
fn figure(x: f64) -> String {
    if !x.is_finite() {
        return "an error".into();
    }
    if x.abs() >= 1000.0 {
        return commas(x);
    }
    if x == x.trunc() {
        return format!("{x:.0}");
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

/// Two numbers that differ, written so that they can be told apart.
fn was_and_is(was: f64, is: f64) -> String {
    match figure(was) == figure(is) {
        true => format!("{was} was, {is} is"),
        false => format!("{} was, {} is", figure(was), figure(is)),
    }
}

/// A share as a percentage, to two figures however small.
fn share(x: f64) -> String {
    let percent = 100.0 * x;
    match percent >= 0.1 {
        true => format!("{percent:.1}%"),
        false => format!("{percent:.1e}%"),
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
    } else {
        format!("{:.0} minutes", s / 60.0)
    }
}
