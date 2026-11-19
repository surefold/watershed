//! One member's pension statement, worked out for the whole scheme.
//!
//! Every year a pension scheme tells each member what their pot might buy
//! when they retire. The sums live in a workbook: the member at the top,
//! the pot followed year by year to retirement at a low, a middle and a
//! high rate of growth, and at the bottom an income in today's money and
//! whether it is the income the member should be aiming for. The workbook
//! answers for one member. The scheme has hundreds of thousands, and the
//! statements are a mail merge that runs overnight.
//!
//! This reads the workbook with `watershed-xlsx` and works every member's
//! statement out at once. Nothing is rewritten by hand. It checks itself
//! against the values saved in the workbook, against LibreOffice
//! recalculating the workbook for each of 300 members, and against the
//! graph a member at a time.
//!
//! Then it does what a statement cannot. A statement says "you are not on
//! track"; a member wants to know what would put them on track. A
//! spreadsheet answers that with Goal Seek, for the one member on the
//! screen. Here every member who is short gets their own answer: the least
//! they could pay in that would do it, found by trying rates between the
//! one they pay now and thirty percent, each try a run of the workbook.
//!
//!     cargo run --release --features cuda --example illustrations
//!
//! then optionally how many members (300,000).
//!
//! The workbook is `shared/illustration.xlsx`, written by
//! `shared/illustration.py`, which also has LibreOffice make the reference.
//! The members and every assumption are made up; the shape of the
//! calculation is the usual one. The `pensions` example is a different
//! kind of scheme, one that has promised its members a pension, written
//! straight onto a graph.

use std::time::{Duration, Instant};

use watershed_core::Value;
use watershed_gpu::Scenarios;
use watershed_xlsx::{Model, Workbook};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const WORKBOOK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/examples/shared/illustration.xlsx"
);
/// What LibreOffice made of the workbook for some members, one at a time.
const REFERENCE: &str = include_str!("shared/illustration_reference.csv");

/// The member's cells: age, pay, the pot today, what the member and the
/// employer pay in as shares of pay, and the age the member retires at.
const MEMBER: [&str; 6] = [
    "Member!B4",
    "Member!B5",
    "Member!B6",
    "Member!B7",
    "Member!B8",
    "Member!B9",
];
/// Where a member's own rate is among those.
const PAYS: usize = 3;
/// The pot at retirement and the income a year on the middle figures, the
/// income on the low and the high, whether the member is on track, and how
/// far short a year.
const ANSWERS: [&str; 6] = [
    "Member!C13",
    "Member!C17",
    "Member!B17",
    "Member!D17",
    "Member!B21",
    "Member!B22",
];
const INCOME: usize = 1;
const ON_TRACK: usize = 4;
/// Rates are tried in steps of half a point of pay, up to thirty points.
const STEP: f64 = 0.005;
const MOST: f64 = 0.30;

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

    fn run(&self, plan: &Scenarios, members: &[[f64; 6]]) -> Result<Vec<f64>> {
        #[cfg(feature = "gpu")]
        return Ok(self.gpu.run(plan, members.as_flattened())?);
        #[cfg(not(feature = "gpu"))]
        Ok(plan.run_cpu(members.as_flattened()))
    }

    /// Whether each member is on track.
    fn on_track(&self, plan: &Scenarios, members: &[[f64; 6]]) -> Result<Vec<bool>> {
        Ok(self
            .run(plan, members)?
            .chunks(ANSWERS.len())
            .map(|answer| answer[ON_TRACK] == 1.0)
            .collect())
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

    fn pick(&mut self, among: &[f64]) -> f64 {
        among[((self.unit() * among.len() as f64) as usize).min(among.len() - 1)]
    }
}

/// One member of a made-up scheme, as the six numbers the workbook asks for.
fn made_up_member(rng: &mut Rng) -> [f64; 6] {
    let age = (22 + (43.0 * rng.unit()) as u32) as f64;
    let pay = (170.0 * (1.9 * rng.unit().powf(1.5)).exp()).round() * 100.0;
    // A pot is some years of contributions, more for the older.
    let pot = (pay * 0.08 * (age - 21.0) * (0.2 + 1.6 * rng.unit()) / 100.0).round() * 100.0;
    let pays = rng.pick(&[0.03, 0.04, 0.05, 0.05, 0.05, 0.06, 0.08, 0.10]);
    let employer = rng.pick(&[0.03, 0.03, 0.04, 0.05, 0.06, 0.08]);
    let retires = rng
        .pick(&[60.0, 63.0, 65.0, 66.0, 67.0, 67.0, 67.0, 68.0])
        .max(age + 1.0);
    [age, pay, pot, pays, employer, retires]
}

/// The same to about twelve figures.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6)
}

fn main() -> Result<()> {
    let count: usize = match std::env::args().nth(1) {
        Some(n) => n.replace('_', "").parse()?,
        None => 300_000,
    };
    let runner = Runner::new()?;

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
    let inputs = cells(&MEMBER)?;
    let answers = cells(&ANSWERS)?;
    let mut model = Model::build(&book, &inputs, &answers)?;
    let wrong = model.check();
    if let Some(first) = wrong.first() {
        return Err(format!("the graph does not agree with the workbook: {first}").into());
    }
    println!(
        "   illustration.xlsx: {} formulas, {} of them between the member and the statement, each as the workbook has it",
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
    let output_ids = model.outputs();
    let plan = Scenarios::new(model.graph(), &model.inputs(), &output_ids)?;
    let own: [f64; 6] = model.workbook_inputs()[..].try_into().expect("six cells");
    println!();

    // ---- Against LibreOffice ----
    println!("2. Against LibreOffice, a member at a time");
    let mut lines = REFERENCE.lines();
    let note = lines.next().ok_or("the reference is empty")?;
    let each_ms: f64 = note
        .rsplit(": ")
        .next()
        .and_then(|took| took.strip_suffix(" ms a member"))
        .ok_or("the reference does not say how long a member took")?
        .parse()?;
    let rows: Vec<Vec<f64>> = lines
        .skip(1)
        .map(|line| line.split(',').map(str::parse).collect())
        .collect::<std::result::Result<_, _>>()?;
    let theirs: Vec<[f64; 6]> = rows
        .iter()
        .map(|row| row[..MEMBER.len()].try_into().expect("six numbers"))
        .collect();
    let ours = runner.run(&plan, &theirs)?;
    for (row, here) in rows.iter().zip(ours.chunks(ANSWERS.len())) {
        for (&there, &here) in row[MEMBER.len()..].iter().zip(here) {
            assert!(
                close(here, there),
                "{here} here and {there} in LibreOffice, for the member {:?}",
                &row[..MEMBER.len()]
            );
        }
    }
    println!(
        "   {} members, six answers each: every one agrees",
        rows.len()
    );
    println!(
        "   LibreOffice took {each_ms} ms a member, the member's cells set and the workbook recalculated"
    );
    println!();

    // ---- The scheme ----
    println!("3. The scheme");
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let members: Vec<[f64; 6]> = (0..count).map(|_| made_up_member(&mut rng)).collect();
    let started = Instant::now();
    let statements = runner.run(&plan, &members)?;
    let took = started.elapsed();
    println!(
        "   {} statements in {} ({} a second); LibreOffice, at {each_ms} ms a member, would take {}",
        commas(count as f64),
        time(took),
        commas(count as f64 / took.as_secs_f64()),
        time(Duration::from_secs_f64(each_ms / 1000.0 * count as f64))
    );
    // The graph itself, a member at a time, as the spreadsheet would do it:
    // it must agree with the plan to the last bit.
    let few = count.min(2_000);
    let started = Instant::now();
    for (member, statement) in members
        .iter()
        .zip(statements.chunks(ANSWERS.len()))
        .take(few)
    {
        model.set(member)?;
        for (&id, &x) in output_ids.iter().zip(statement) {
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
        "   the graph, a member at a time, agrees to the last bit for the first {}: {} a member",
        commas(few as f64),
        time(started.elapsed() / few as u32)
    );
    println!();
    println!(
        "   {:<10} {:>10} {:>26} {:>10}",
        "age", "members", "income a year, half below", "on track"
    );
    let on_track: Vec<bool> = statements
        .chunks(ANSWERS.len())
        .map(|s| s[ON_TRACK] == 1.0)
        .collect();
    for (from, to) in [(22, 29), (30, 39), (40, 49), (50, 59), (60, 64), (22, 64)] {
        let these: Vec<usize> = (0..count)
            .filter(|&m| (from..=to).contains(&(members[m][0] as u32)))
            .collect();
        if these.is_empty() {
            continue;
        }
        let mut incomes: Vec<f64> = these
            .iter()
            .map(|&m| statements[m * ANSWERS.len() + INCOME])
            .collect();
        incomes.sort_by(f64::total_cmp);
        println!(
            "   {:<10} {:>10} {:>26} {:>9.1}%",
            match (from, to) {
                (22, 64) => "all".to_string(),
                _ => format!("{from} to {to}"),
            },
            commas(these.len() as f64),
            commas(incomes[incomes.len() / 2]),
            100.0 * these.iter().filter(|&&m| on_track[m]).count() as f64 / these.len() as f64
        );
    }
    let statement = runner.run(&plan, &[own])?;
    println!(
        "   the workbook's own member (aged {}, paid {}, a pot of {}): {} a year, or {} to {}; short by {}",
        own[0],
        commas(own[1]),
        commas(own[2]),
        commas(statement[1]),
        commas(statement[2]),
        commas(statement[3]),
        commas(statement[5])
    );
    println!();

    // ---- If they all did one thing ----
    println!("4. If every member did one thing");
    let share = |members: &[[f64; 6]]| -> Result<f64> {
        let on = runner.on_track(&plan, members)?;
        Ok(100.0 * on.iter().filter(|&&on| on).count() as f64 / on.len() as f64)
    };
    let mut changed = members.clone();
    for member in &mut changed {
        member[PAYS] += 0.01;
    }
    let a_point_more = share(&changed)?;
    let mut changed = members.clone();
    for member in &mut changed {
        member[5] += 1.0;
    }
    let a_year_longer = share(&changed)?;
    let now = 100.0 * on_track.iter().filter(|&&on| on).count() as f64 / count as f64;
    println!("   on track now: {now:.1}%");
    println!("   if each paid in one more point of pay: {a_point_more:.1}%");
    println!("   if each worked a year longer: {a_year_longer:.1}%");
    assert!(a_point_more >= now && a_year_longer >= now);
    println!();

    // ---- What it would take, member by member ----
    println!("5. What it would take, member by member");
    let short: Vec<usize> = (0..count).filter(|&m| !on_track[m]).collect();
    // The steps of half a point between what each pays now and the most.
    // `low` is a count of steps known to fall short, `high` one known to do.
    let steps = |m: usize| ((MOST - members[m][PAYS]) / STEP).round() as u32;
    let at = |m: usize, step: u32| {
        let mut member = members[m];
        member[PAYS] += f64::from(step) * STEP;
        member
    };
    let started = Instant::now();
    let most: Vec<[f64; 6]> = short.iter().map(|&m| at(m, steps(m))).collect();
    let can = runner.on_track(&plan, &most)?;
    let mut runs = most.len();
    // Those thirty points would put on track: halve the gap until it is a step.
    let mut seeking: Vec<(usize, u32, u32)> = short
        .iter()
        .zip(&can)
        .filter(|&(_, &can)| can)
        .map(|(&m, _)| (m, 0, steps(m)))
        .collect();
    let never = short.len() - seeking.len();
    let mut rounds = 0;
    while seeking.iter().any(|&(_, low, high)| high - low > 1) {
        let tries: Vec<[f64; 6]> = seeking
            .iter()
            .map(|&(m, low, high)| at(m, (low + high) / 2))
            .collect();
        let enough = runner.on_track(&plan, &tries)?;
        runs += tries.len();
        rounds += 1;
        for ((_, low, high), enough) in seeking.iter_mut().zip(enough) {
            if *high - *low > 1 {
                let middle = (*low + *high) / 2;
                match enough {
                    true => *high = middle,
                    false => *low = middle,
                }
            }
        }
    }
    println!(
        "   {} members are short; {} tries of the workbook in {} ({} rounds of halving)",
        commas(short.len() as f64),
        commas(runs as f64),
        time(started.elapsed()),
        rounds
    );
    // Each answer is the least that does: it does, and half a point less does not.
    let check = seeking.len().min(5_000);
    let does: Vec<[f64; 6]> = seeking[..check]
        .iter()
        .map(|&(m, _, high)| at(m, high))
        .collect();
    let does_not: Vec<[f64; 6]> = seeking[..check]
        .iter()
        .map(|&(m, _, high)| at(m, high - 1))
        .collect();
    assert!(runner.on_track(&plan, &does)?.iter().all(|&on| on));
    assert!(runner.on_track(&plan, &does_not)?.iter().all(|&on| !on));
    let mut more: Vec<f64> = seeking
        .iter()
        .map(|&(_, _, high)| f64::from(high) * STEP * 100.0)
        .collect();
    more.sort_by(f64::total_cmp);
    let within = |points: f64| more.iter().filter(|&&m| m <= points).count();
    println!("   the least more each would have to pay in, as points of pay:");
    for points in [1.0, 2.0, 5.0, 10.0] {
        println!(
            "     {:>4} or less: {:>9} ({:.1}% of those short)",
            points,
            commas(within(points) as f64),
            100.0 * within(points) as f64 / short.len() as f64
        );
    }
    println!(
        "     more than 10: {:>9}; half of those who can get there need {} or less",
        commas((more.len() - within(10.0)) as f64),
        more[more.len() / 2]
    );
    println!(
        "     thirty points of pay would not do it for {} ({:.1}%)",
        commas(never as f64),
        100.0 * never as f64 / short.len() as f64
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
