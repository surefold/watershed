//! A workbook that follows a price feed.
//!
//! A risk desk keeps a workbook of the floor's positions: six hundred of
//! them in sixty shares, each desk's totals against its limits, and the
//! floor's own figures. Prices move all day, one share at a time. A price
//! that moves touches the handful of positions in that share, their desks
//! and the floor: a few dozen cells of three thousand. The rest need not be
//! looked at.
//!
//! This reads the workbook with `watershed-xlsx` and feeds its graph a
//! stream of prices, one change at a time. Each change works out again only
//! what that price reaches. It reports the moments a desk goes over its
//! limit or a position is stopped out, and afterwards says why one of them
//! happened, from the graph's own record of that update. Nothing here uses
//! a plan or a GPU: it is one graph, kept up to date.
//!
//! It checks itself against LibreOffice following the same first three
//! hundred prices, and at the end against a second graph given the final
//! prices all at once, cell for cell and to the last bit.
//!
//!     cargo run --release -p watershed-xlsx --example live
//!
//! then optionally how many prices to feed it (200,000).
//!
//! The workbook is `desk.xlsx`, written by `desk.py`, which also has
//! LibreOffice make the reference. Every name and number in it is made up.

use std::time::{Duration, Instant};

use watershed_core::{NodeId, Value};
use watershed_xlsx::{Cell, Model, Val, Workbook};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const WORKBOOK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/desk.xlsx");
/// What LibreOffice made of the workbook as the first prices came in.
const REFERENCE: &str = include_str!("desk_reference.csv");

const SHARES: u32 = 60;
const DESKS: u32 = 12;
/// The first row of each table in the workbook.
const FIRST: u32 = 5;
/// The floor's figures, `Floor!B4` to `B12`, and what the three counts
/// among them count.
const FLOOR: std::ops::RangeInclusive<u32> = 4..=12;
const COUNTS: [(usize, &str); 3] = [
    (4, "desks over their limit"),
    (5, "desks through their loss for the day"),
    (6, "positions stopped"),
];

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

/// The same to about twelve figures.
fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-6)
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
    let prices: Vec<Cell> = (0..SHARES)
        .map(|k| book.cell(&format!("Market!B{}", FIRST + k)))
        .collect::<std::result::Result<_, _>>()?;
    let floor: Vec<Cell> = FLOOR
        .map(|row| book.cell(&format!("Floor!B{row}")))
        .collect::<std::result::Result<_, _>>()?;
    let label = |c: Cell| match book.value(Cell { col: 0, ..c }) {
        Val::Text(t) => t,
        _ => book.name(c),
    };
    let mut model = Model::build(&book, &prices, &floor)?;
    let wrong = model.check();
    if let Some(first) = wrong.first() {
        return Err(format!("the graph does not agree with the workbook: {first}").into());
    }
    println!(
        "   desk.xlsx: {} formulas; the {} that follow the {} prices are a graph, each as the workbook has it ({})",
        book.formulas(),
        model.translated(),
        SHARES,
        time(started.elapsed())
    );
    let inputs = model.inputs();
    let mut now = model.workbook_inputs();
    let floor_ids: Vec<NodeId> = model.outputs();
    let read = |model: &Model, id: NodeId| match model.graph().value(id) {
        Value::F64(x) => *x,
        Value::Bool(b) => f64::from(u8::from(*b)),
        _ => f64::NAN,
    };
    // Every update is kept, so that any of them can be asked about later.
    model.graph_mut().keep_history(count as u64 + 1_000);
    model.graph_mut().stabilize();
    println!();

    // ---- Against LibreOffice ----
    println!("2. Against LibreOffice, a price at a time");
    let mut lines = REFERENCE.lines();
    let note = lines.next().ok_or("the reference is empty")?;
    let each_ms: f64 = note
        .rsplit(": ")
        .next()
        .and_then(|took| took.strip_suffix(" ms a price"))
        .ok_or("the reference does not say how long a price took")?
        .parse()?;
    let rows: Vec<Vec<f64>> = lines
        .skip(1)
        .map(|line| line.split(',').map(str::parse).collect())
        .collect::<std::result::Result<_, _>>()?;
    for row in &rows {
        let share = row[0] as usize;
        now[share] = row[1];
        model
            .graph_mut()
            .set_value(inputs[share], Value::F64(row[1]))?;
        model.graph_mut().stabilize();
        for (&there, &id) in row[2..].iter().zip(&floor_ids) {
            let here = read(&model, id);
            assert!(
                close(here, there),
                "{here} here and {there} in LibreOffice, after S{:02} went to {}",
                share + 1,
                row[1]
            );
        }
    }
    println!(
        "   {} prices, one after another: the floor's {} figures agree after every one",
        rows.len(),
        floor.len()
    );
    println!(
        "   LibreOffice took {each_ms} ms a price, the cell set and what it reaches recalculated"
    );
    println!();

    // ---- The feed ----
    println!("3. The feed");
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut took = Vec::with_capacity(count);
    let (mut redone, mut changed) = (0usize, 0usize);
    let mut counts: Vec<f64> = COUNTS
        .iter()
        .map(|&(k, _)| read(&model, floor_ids[k]))
        .collect();
    // The moments a count changes: the update, the share, and the count.
    let mut moments: Vec<(u64, usize, f64, usize, f64, f64)> = Vec::new();
    let feed = Instant::now();
    for _ in 0..count {
        let share = (rng.unit() * f64::from(SHARES)) as usize;
        // Mostly a move of up to half a percent; one in fifty, up to four.
        let most = if rng.unit() < 0.02 { 0.04 } else { 0.005 };
        let price = (now[share] * (1.0 + most * (2.0 * rng.unit() - 1.0)) * 100.0).round() / 100.0;
        now[share] = price;
        let started = Instant::now();
        model
            .graph_mut()
            .set_value(inputs[share], Value::F64(price))?;
        let report = model.graph_mut().stabilize();
        took.push(started.elapsed());
        redone += report.recalculated;
        changed += report.changed;
        for (which, &(k, _)) in COUNTS.iter().enumerate() {
            let is = read(&model, floor_ids[k]);
            if is != counts[which] {
                moments.push((report.epoch, share, price, which, counts[which], is));
                counts[which] = is;
            }
        }
    }
    let all = feed.elapsed();
    took.sort();
    let at = |share: f64| took[((count as f64 * share) as usize).min(count - 1)];
    println!(
        "   {} prices in {}: {} a second",
        commas(count as f64),
        time(all),
        commas(count as f64 / all.as_secs_f64())
    );
    println!(
        "   a price takes {} (half take less than {}, 99 in 100 less than {}, the slowest {})",
        time(all / count as u32),
        time(at(0.5)),
        time(at(0.99)),
        time(took[count - 1])
    );
    println!(
        "   it reaches {:.0} of the {} cells on average, and {:.0} of those come out different",
        redone as f64 / count as f64,
        model.translated(),
        changed as f64 / count as f64
    );
    // Every price at once: what recalculating the whole workbook costs.
    let whole = 200;
    let started = Instant::now();
    for round in 0..whole {
        for (k, &input) in inputs.iter().enumerate() {
            // A penny up and back down, so the prices end where they were.
            let nudged = now[k] + if round % 2 == 0 { 0.01 } else { 0.0 };
            model.graph_mut().set_value(input, Value::F64(nudged))?;
        }
        model.graph_mut().stabilize();
    }
    let whole_each = started.elapsed() / whole;
    for (&input, &price) in inputs.iter().zip(&now) {
        model.graph_mut().set_value(input, Value::F64(price))?;
    }
    model.graph_mut().stabilize();
    println!(
        "   every price at once, so every cell again: {} an update",
        time(whole_each)
    );
    println!(
        "   LibreOffice, at {each_ms} ms a price, would take {} over this feed",
        time(Duration::from_secs_f64(each_ms / 1000.0 * count as f64))
    );
    println!();
    println!(
        "   moments a count on the floor changed: {}; the first two of each kind",
        commas(moments.len() as f64)
    );
    for (which, &(_, what)) in COUNTS.iter().enumerate() {
        let of_this = moments.iter().filter(|m| m.3 == which);
        println!("     {what}: {}", commas(of_this.clone().count() as f64));
        for &(update, share, price, _, was, is) in of_this.take(2) {
            println!(
                "       update {update}: S{:02} to {price:.2}, and they went from {was} to {is}",
                share + 1
            );
        }
    }
    println!();

    // ---- Against the whole thing at once ----
    println!("4. Against a second graph given the final prices at once");
    let mut fresh = Model::build(&book, &prices, &floor)?;
    fresh.set(&now)?;
    let mut compared = 0;
    for c in book.cells() {
        if let (Some(here), Some(there)) = (model.value(c), fresh.value(c)) {
            assert!(
                here.to_bits() == there.to_bits(),
                "{}: {here} after the feed, {there} at once",
                book.name(c)
            );
            compared += 1;
        }
    }
    println!(
        "   {} cells, each the same to the last bit",
        commas(compared as f64)
    );
    println!();

    // ---- Why ----
    println!("5. Why");
    // The first time a desk went over its limit, if one did; else the first
    // moment of any kind.
    let Some(&(update, share, price, which, _, _)) =
        moments.iter().find(|m| m.3 == 0).or(moments.first())
    else {
        println!("   nothing happened on the floor to ask about");
        return Ok(());
    };
    println!(
        "   update {update}: S{:02} went to {price:.2} and the {} changed",
        share + 1,
        COUNTS[which].1
    );
    // Everything below is read from the graph's record of that update and
    // the one before it, long after both.
    let at = |name: String, update: u64| -> Option<f64> {
        let id = model.node(book.cell(&name).ok()?)?;
        match model.graph().value_at(id, update) {
            Ok(Value::F64(x)) => Some(*x),
            _ => None,
        }
    };
    let moved = |name: String| -> Option<(f64, f64)> {
        let (was, is) = (at(name.clone(), update - 1)?, at(name, update)?);
        (was != is).then_some((was, is))
    };
    // The desk whose own count changed in that update.
    let column = ["H", "K", "I"][which];
    let Some(desk) = (0..DESKS).find(|d| moved(format!("Desks!{column}{}", FIRST + d)).is_some())
    else {
        return Err("no desk's count changed in that update".into());
    };
    let row = FIRST + desk;
    let flag = book.cell(&format!("Desks!{column}{row}"))?;
    println!("   it was {}", label(flag));
    let id = model
        .node(flag)
        .ok_or("the desk's count is not in the graph")?;
    let why = model.graph().explain_node_at(id, update)?;
    println!("     {}", clip(why.lines().next().unwrap_or(""), 110));
    // What the desk's count rests on, before and after.
    let (rests_on, what) = [
        ("B", "size"),
        ("E", "profit today"),
        ("I", "positions stopped"),
    ][which];
    if let Some((was, is)) = moved(format!("Desks!{rests_on}{row}")) {
        println!(
            "     its {what} went from {} to {}",
            commas(was),
            commas(is)
        );
    }
    // The positions that moved it: the desk's fifty rows of the book.
    let positions = FIRST + desk * 50..FIRST + (desk + 1) * 50;
    for position in positions {
        let Some((was, is)) = moved(format!("Book!F{position}")) else {
            continue;
        };
        let held = match book.value(book.cell(&format!("Book!C{position}"))?) {
            Val::Num(x) => x,
            _ => f64::NAN,
        };
        println!(
            "     row {position} of the book, {} shares of S{:02}: its price went from {was:.2} to {is:.2}",
            commas(held),
            share + 1
        );
    }
    Ok(())
}

/// A line cut to a width, with a mark where it was cut.
fn clip(line: &str, width: usize) -> String {
    match line.chars().count() > width {
        true => format!("{} ...", line.chars().take(width).collect::<String>()),
        false => line.to_string(),
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
    if s < 0.000_001 {
        format!("{:.0} ns", s * 1e9)
    } else if s < 0.001 {
        format!("{:.1} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else if s < 120.0 {
        format!("{s:.1} s")
    } else {
        format!("{:.0} minutes", s / 60.0)
    }
}
