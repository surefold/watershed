//! Every quote a rating workbook can give, worked out ahead of time.
//!
//! A pricing team keeps its rates in a workbook: a customer goes in at the
//! top, the tables of factors are looked up, and a premium comes out at the
//! bottom. A comparison site asks for a price in a few milliseconds, for
//! every excess and kind of cover at once, thousands of times a second. So
//! the workbook is not what answers: its rules are written out again in the
//! quoting system, or every answer it can give is worked out ahead of time
//! and looked up. And when the rates change, the question before they go
//! live is who pays more, and how much.
//!
//! This reads the workbook with `watershed-xlsx` and works out every quote
//! it can give: every age, area, vehicle group, count of years without a
//! claim, mileage, excess and kind of cover. Nothing is rewritten by hand.
//! It checks itself against the values saved in the workbook, against
//! LibreOffice recalculating the workbook for each of 300 customers, and
//! against the graph a quote at a time. Then it puts next quarter's rates
//! in, works every quote out again, and says whose price moves.
//!
//!     cargo run --release --features cuda --example quotes
//!
//! then optionally how many of the fifty vehicle groups to price (all).
//!
//! The workbook is `shared/rating.xlsx`, written by `shared/rating.py`,
//! which also has LibreOffice make the reference. Every rate in it is made
//! up. The customer's cells are numbers: a workbook that takes its cover
//! from a list of names would need the names turned to numbers first.

use std::time::{Duration, Instant};

use watershed_core::Value;
use watershed_gpu::Scenarios;
use watershed_xlsx::{Cell, Model, Val, Workbook};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const WORKBOOK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/shared/rating.xlsx");
/// What LibreOffice made of the workbook for some customers, one at a time.
const REFERENCE: &str = include_str!("shared/rating_reference.csv");

/// The customer's cells: age, area, vehicle group, years without a claim,
/// miles a year, voluntary excess and kind of cover.
const CUSTOMER: [&str; 7] = [
    "Quote!B4",
    "Quote!B5",
    "Quote!B6",
    "Quote!B7",
    "Quote!B8",
    "Quote!B9",
    "Quote!B10",
];
/// The year's premium, a monthly instalment, and whether there is a quote.
const ANSWERS: [&str; 3] = ["Quote!B27", "Quote!B30", "Quote!B28"];
/// Next quarter's rates: the cells that change and what each is times. The
/// base rate and the least premium go up, the youngest drivers pay more,
/// and so do the two dearest areas.
const NEW_RATES: [(&str, f64); 9] = [
    ("Rates!B3", 1.03),
    ("Rates!B5", 1.08),
    ("Rates!B8", 1.08),
    ("Rates!B9", 1.08),
    ("Rates!B10", 1.08),
    ("Rates!B11", 1.08),
    ("Rates!B12", 1.08),
    ("Rates!E16", 1.05),
    ("Rates!E17", 1.05),
];

const MILES: [f64; 5] = [3_000.0, 6_000.0, 9_000.0, 13_000.0, 20_000.0];
const EXCESSES: [f64; 5] = [0.0, 100.0, 250.0, 500.0, 1_000.0];
const COVERS: [&str; 2] = ["comprehensive", "third party, fire and theft"];
/// How many quotes a run takes at once.
const AT_ONCE: usize = 1 << 20;

/// What a customer can ask for: how many of each thing, and so where each
/// quote is in the table.
struct Asked {
    groups: usize,
}

impl Asked {
    /// Ages 17 to 80, ten areas, the vehicle groups, 0 to 9 years without a
    /// claim, five mileages, five excesses and two kinds of cover.
    fn sizes(&self) -> [usize; 7] {
        [
            64,
            10,
            self.groups,
            10,
            MILES.len(),
            EXCESSES.len(),
            COVERS.len(),
        ]
    }

    fn count(&self) -> usize {
        self.sizes().iter().product()
    }

    /// The customer a place in the table is for, as the workbook's cells.
    fn customer(&self, mut at: usize) -> [f64; 7] {
        let mut which = [0; 7];
        for (k, size) in self.sizes().iter().enumerate().rev() {
            which[k] = at % size;
            at /= size;
        }
        [
            (17 + which[0]) as f64,
            (1 + which[1]) as f64,
            (1 + which[2]) as f64,
            which[3] as f64,
            MILES[which[4]],
            EXCESSES[which[5]],
            (1 + which[6]) as f64,
        ]
    }

    /// Where a customer's quote is in the table.
    fn place(&self, customer: &[f64; 7]) -> usize {
        let which = [
            customer[0] as usize - 17,
            customer[1] as usize - 1,
            customer[2] as usize - 1,
            customer[3] as usize,
            MILES
                .iter()
                .position(|&m| m == customer[4])
                .expect("a mileage asked for"),
            EXCESSES
                .iter()
                .position(|&e| e == customer[5])
                .expect("an excess asked for"),
            customer[6] as usize - 1,
        ];
        which
            .iter()
            .zip(self.sizes())
            .fold(0, |at, (&w, size)| at * size + w)
    }
}

/// Every quote: the year's premium and a monthly instalment in pence, with
/// nothing where there is no quote.
struct Table {
    premium: Vec<u32>,
    instalment: Vec<u32>,
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

    /// Every quote the workbook can give.
    fn table(&self, plan: &Scenarios, asked: &Asked) -> Result<Table> {
        let count = asked.count();
        let mut table = Table {
            premium: Vec::with_capacity(count),
            instalment: Vec::with_capacity(count),
        };
        let mut inputs = Vec::with_capacity(AT_ONCE * CUSTOMER.len());
        for from in (0..count).step_by(AT_ONCE) {
            inputs.clear();
            for at in from..(from + AT_ONCE).min(count) {
                inputs.extend(asked.customer(at));
            }
            for quote in self.run(plan, &inputs)?.chunks(ANSWERS.len()) {
                let quoted = quote[2] == 1.0;
                table.premium.push(if quoted { pence(quote[0]) } else { 0 });
                table
                    .instalment
                    .push(if quoted { pence(quote[1]) } else { 0 });
            }
        }
        Ok(table)
    }
}

fn pence(pounds: f64) -> u32 {
    (pounds * 100.0).round() as u32
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

    fn below(&mut self, n: usize) -> usize {
        ((self.unit() * n as f64) as usize).min(n - 1)
    }
}

fn main() -> Result<()> {
    let asked = Asked {
        groups: match std::env::args().nth(1) {
            Some(n) => n.parse::<usize>()?.clamp(1, 50),
            None => 50,
        },
    };
    let runner = Runner::new()?;

    // ---- The workbook ----
    println!("1. The workbook");
    let book = Workbook::open(WORKBOOK)?;
    let cells = |names: &[&str]| -> Result<Vec<Cell>> {
        Ok(names
            .iter()
            .map(|name| book.cell(name))
            .collect::<std::result::Result<_, _>>()?)
    };
    let rates: Vec<&str> = NEW_RATES.iter().map(|&(cell, _)| cell).collect();
    let inputs = cells(&[&CUSTOMER[..], &rates[..]].concat())?;
    let answers = cells(&ANSWERS)?;
    let mut model = Model::build(&book, &inputs, &answers)?;
    let wrong = model.check();
    if let Some(first) = wrong.first() {
        return Err(format!("the graph does not agree with the workbook: {first}").into());
    }
    println!(
        "   rating.xlsx: {} formulas, all between the customer and the price, each as the workbook has it",
        book.formulas()
    );
    println!(
        "   it uses {}",
        model
            .functions()
            .iter()
            .map(|(name, n)| format!("{name} ×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let ids = model.inputs();
    let output_ids = model.outputs();
    // The customer varies from quote to quote; the rates stay as they are.
    let plan = Scenarios::new(model.graph(), &ids[..CUSTOMER.len()], &output_ids)?;
    println!();

    // ---- Against LibreOffice ----
    println!("2. Against LibreOffice, a customer at a time");
    let mut lines = REFERENCE.lines();
    let note = lines.next().ok_or("the reference is empty")?;
    let each_ms: f64 = note
        .rsplit(": ")
        .next()
        .and_then(|took| took.strip_suffix(" ms a quote"))
        .ok_or("the reference does not say how long a quote took")?
        .parse()?;
    let rows: Vec<Vec<f64>> = lines
        .skip(1)
        .map(|line| line.split(',').map(str::parse).collect())
        .collect::<std::result::Result<_, _>>()?;
    let theirs: Vec<f64> = rows
        .iter()
        .flat_map(|row| row[..CUSTOMER.len()].to_vec())
        .collect();
    let ours = runner.run(&plan, &theirs)?;
    for (row, here) in rows.iter().zip(ours.chunks(ANSWERS.len())) {
        for (&there, &here) in row[CUSTOMER.len()..].iter().zip(here) {
            assert!(
                pence(here) == pence(there) && (here - there).abs() < 1e-9,
                "{here} here and {there} in LibreOffice, for the customer {:?}",
                &row[..CUSTOMER.len()]
            );
        }
    }
    println!(
        "   {} customers, three answers each: every one the same to the penny",
        rows.len()
    );
    println!(
        "   LibreOffice took {each_ms} ms a quote, the customer's cells set and the workbook recalculated"
    );
    println!();

    // ---- Every quote ----
    println!("3. Every quote");
    let count = asked.count();
    let started = Instant::now();
    let table = runner.table(&plan, &asked)?;
    let took = started.elapsed();
    let sizes = asked.sizes();
    println!(
        "   {} ages × {} areas × {} vehicle groups × {} counts of years × {} mileages × {} excesses × {} covers",
        sizes[0], sizes[1], sizes[2], sizes[3], sizes[4], sizes[5], sizes[6]
    );
    println!(
        "   {} quotes in {} ({} a second); LibreOffice, at {each_ms} ms a quote, would take {}",
        commas(count as f64),
        time(took),
        commas(count as f64 / took.as_secs_f64()),
        time(Duration::from_secs_f64(each_ms / 1000.0 * count as f64))
    );
    let mut quoted: Vec<u32> = table.premium.iter().copied().filter(|&p| p > 0).collect();
    quoted.sort_unstable();
    println!(
        "   no quote for {} of them; the rest run from {} to {}, half below {}",
        commas((count - quoted.len()) as f64),
        pounds(quoted[0]),
        pounds(quoted[quoted.len() - 1]),
        pounds(quoted[quoted.len() / 2])
    );
    println!(
        "   as a table, the premium and the instalment in pence: {} MB",
        (count * 8) >> 20
    );
    // The graph itself, a quote at a time, as the spreadsheet would do it.
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let few = 2_000;
    let as_given = model.workbook_inputs();
    let started = Instant::now();
    for _ in 0..few {
        let at = rng.below(count);
        let mut inputs = as_given.clone();
        inputs[..CUSTOMER.len()].copy_from_slice(&asked.customer(at));
        model.set(&inputs)?;
        let read = |k: usize| match model.graph().value(output_ids[k]) {
            Value::F64(x) => *x,
            _ => unreachable!("an answer is a number"),
        };
        let quote = match read(2) == 1.0 {
            true => (pence(read(0)), pence(read(1))),
            false => (0, 0),
        };
        assert!(
            quote == (table.premium[at], table.instalment[at]),
            "the graph and the table differ"
        );
    }
    let graph_each = started.elapsed() / few;
    println!(
        "   {} of them at random agree with the graph, a quote at a time ({} each)",
        commas(few as f64),
        time(graph_each)
    );
    println!();

    // ---- Looking a quote up ----
    println!("4. Looking a quote up");
    let lookups = 2_000_000;
    let places: Vec<[f64; 7]> = (0..lookups)
        .map(|_| asked.customer(rng.below(count)))
        .collect();
    let started = Instant::now();
    let mut sum = 0u64;
    for customer in &places {
        sum += u64::from(table.premium[asked.place(customer)]);
    }
    let each = started.elapsed().as_secs_f64() / lookups as f64;
    println!(
        "   {} customers looked up in the table: {:.0} ns each (their premiums come to {})",
        commas(lookups as f64),
        each * 1e9,
        commas(sum as f64 / 100.0)
    );
    // Everything the workbook's own customer could choose.
    let own: [f64; 7] = as_given[..CUSTOMER.len()].try_into().expect("seven cells");
    let mut own = own;
    own[2] = own[2].min(asked.groups as f64);
    println!(
        "   the workbook's customer (aged {}, area {}, vehicle group {}, {} years without a claim, {} miles a year):",
        own[0], own[1], own[2], own[3], own[4]
    );
    println!(
        "     {:<18} {:>32} {:>32}",
        "voluntary excess", COVERS[0], COVERS[1]
    );
    for excess in EXCESSES {
        print!("     {excess:<18}");
        for cover in 1..=COVERS.len() {
            let mut choice = own;
            (choice[5], choice[6]) = (excess, cover as f64);
            let at = asked.place(&choice);
            print!(
                " {:>32}",
                format!(
                    "{} or 10 × {}",
                    pounds(table.premium[at]),
                    pounds(table.instalment[at])
                )
            );
        }
        println!();
    }
    println!();

    // ---- Next quarter's rates ----
    println!("5. Next quarter's rates");
    let rate_ids = &ids[CUSTOMER.len()..];
    for ((&cell, &id), &(name, times)) in inputs[CUSTOMER.len()..]
        .iter()
        .zip(rate_ids)
        .zip(&NEW_RATES)
    {
        let Val::Num(now) = book.value(cell) else {
            return Err(format!("{name} does not hold a number").into());
        };
        model.graph_mut().set_value(id, Value::F64(now * times))?;
    }
    model.graph_mut().stabilize();
    println!(
        "   the base rate up 3%, the least premium up 8%, ages 17 to 22 up 8%, areas 9 and 10 up 5%"
    );
    let started = Instant::now();
    let new_plan = Scenarios::new(model.graph(), &ids[..CUSTOMER.len()], &output_ids)?;
    let next = runner.table(&new_plan, &asked)?;
    println!("   every quote again in {}", time(started.elapsed()));
    println!(
        "     {:<12} {:>12} {:>14} {:>14} {:>16}",
        "age", "quotes", "average rise", "largest rise", "up more than 5%"
    );
    let bands = [(17, 22), (23, 29), (30, 49), (50, 69), (70, 80), (17, 80)];
    let per_age = count / sizes[0];
    for (from, to) in bands {
        let places = (from - 17) * per_age..(to - 16) * per_age;
        let (mut quotes, mut rise, mut largest, mut over) = (0usize, 0.0f64, 0.0f64, 0usize);
        for at in places {
            let (was, is) = (table.premium[at], next.premium[at]);
            if was == 0 {
                continue;
            }
            let up = f64::from(is) / f64::from(was) - 1.0;
            quotes += 1;
            rise += up;
            largest = largest.max(up);
            over += usize::from(up > 0.05);
        }
        println!(
            "     {:<12} {:>12} {:>13.2}% {:>13.2}% {:>15.1}%",
            match (from, to) {
                (17, 80) => "all".to_string(),
                _ => format!("{from} to {to}"),
            },
            commas(quotes as f64),
            100.0 * rise / quotes as f64,
            100.0 * largest,
            100.0 * over as f64 / quotes as f64
        );
    }
    // The new rates are dearer for everyone, and a quote that was refused
    // still is.
    assert!(
        table
            .premium
            .iter()
            .zip(&next.premium)
            .all(|(&was, &is)| if was == 0 { is == 0 } else { is > was }),
        "a price fell, or a refusal changed"
    );
    Ok(())
}

/// Pence as pounds and pence.
fn pounds(pence: u32) -> String {
    format!("£{}.{:02}", commas(f64::from(pence / 100)), pence % 100)
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
