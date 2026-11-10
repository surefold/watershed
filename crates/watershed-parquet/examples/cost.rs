//! What writing changes costs, on the graph shape of watershed-core's
//! `benchmark` example (10,000 prices, 500,000 trades and a tree of totals,
//! about a million nodes), and how small the files come out.
//!
//!     cargo run --release -p watershed-parquet --example cost -- <empty folder>

use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::time::{Duration, Instant};

use parquet::file::reader::{FileReader, SerializedFileReader};
use watershed_core::prelude::*;
use watershed_parquet::{LocalStore, Options, parquet_sink};

const MARKET_PRICES: usize = 10_000;
const TRADES: usize = 500_000;
const UPDATES_PER_ROUND: usize = 2_000;
const ROUNDS: usize = 5;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn unit(&mut self) -> f64 {
        (self.next() % 1_000_000) as f64 / 1_000_000.0
    }
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

/// Rows and bytes in a folder's Parquet files, split into the starting
/// points (one update per file, the update the sink was attached at) and
/// the rest.
fn sizes(dir: &Path, starts: &[u64]) -> ((u64, u64), (u64, u64)) {
    let (mut start, mut rest) = ((0, 0), (0, 0));
    for run in fs::read_dir(dir).unwrap() {
        let changes = run.unwrap().path().join("changes");
        for file in fs::read_dir(changes).unwrap() {
            let path = file.unwrap().path();
            let name = path.file_stem().unwrap().to_str().unwrap().to_string();
            let (first, last) = name.split_once('-').unwrap();
            let (first, last): (u64, u64) = (first.parse().unwrap(), last.parse().unwrap());
            let rows = SerializedFileReader::new(fs::File::open(&path).unwrap())
                .unwrap()
                .metadata()
                .file_metadata()
                .num_rows() as u64;
            let bytes = fs::metadata(&path).unwrap().len();
            let into = if first == last && starts.contains(&first) {
                &mut start
            } else {
                &mut rest
            };
            into.0 += rows;
            into.1 += bytes;
        }
    }
    (start, rest)
}

fn main() -> Result<(), CalcError> {
    let dir = std::env::args()
        .nth(1)
        .expect("give an empty folder to write to");
    let dir = Path::new(&dir);
    assert!(
        !dir.exists() || fs::read_dir(dir).unwrap().next().is_none(),
        "the folder must be empty"
    );

    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut g = Graph::new();
    g.reserve(MARKET_PRICES + 2 * TRADES, 4 * TRADES);
    let prices: Vec<Input<f64>> = (0..MARKET_PRICES)
        .map(|i| g.input(&format!("price{i}"), 100.0 + rng.unit()))
        .collect::<Result<_, _>>()?;
    let mut level: Vec<Node<f64>> = Vec::with_capacity(TRADES);
    for t in 0..TRADES {
        let a = prices[rng.below(MARKET_PRICES)];
        let b = prices[rng.below(MARKET_PRICES)];
        let (qty_a, qty_b) = (rng.unit() * 10.0, rng.unit() * -5.0);
        level.push(g.define(&format!("trade{t}"), a * qty_a + b * qty_b)?);
    }
    let mut depth = 0;
    while level.len() > 1 {
        depth += 1;
        let mut next = Vec::with_capacity(level.len() / 2 + 1);
        for (k, pair) in level.chunks(2).enumerate() {
            next.push(match pair {
                [x, y] => g.define(&format!("sum{depth}_{k}"), *x + *y)?,
                [x] => *x,
                _ => unreachable!(),
            });
        }
        level = next;
    }
    println!("graph: {} nodes", g.len());

    // Rounds alternate, so slow drift in the machine affects both alike.
    // Each writing round is a run of its own, starting with every node.
    let (mut off, mut on) = (Vec::new(), Vec::new());
    let mut starts = Vec::new();
    let mut attach_took = Vec::new();
    for round in 0..2 * ROUNDS {
        let writing = round % 2 == 1;
        let writer = if writing {
            let started = Instant::now();
            let (sink, writer) =
                parquet_sink(LocalStore::new(dir), Options::new(format!("round{round}")));
            starts.push(g.epoch());
            g.attach_sink(sink);
            // The starting point goes in a file of its own.
            writer.flush().unwrap();
            attach_took.push(started.elapsed());
            Some(writer)
        } else {
            None
        };
        for _ in 0..UPDATES_PER_ROUND {
            let p = prices[rng.below(MARKET_PRICES)];
            g.set(p, 100.0 + rng.unit())?;
            let started = Instant::now();
            black_box(g.stabilize());
            let took = started.elapsed();
            if writing { &mut on } else { &mut off }.push(took);
        }
        if let Some(writer) = writer {
            g.detach_sink();
            writer.close().unwrap();
        }
    }
    println!("one price changes, not writing: {:?}", median(off));
    println!("one price changes, writing:     {:?}", median(on));
    println!(
        "attaching (every node, written and flushed): {:?}",
        median(attach_took)
    );

    let ((start_rows, start_bytes), (rows, bytes)) = sizes(dir, &starts);
    println!(
        "starting points: {start_rows} rows, {:.1} MB, {:.2} bytes per row",
        start_bytes as f64 / 1e6,
        start_bytes as f64 / start_rows as f64
    );
    println!(
        "updates: {rows} rows ({} per update), {:.1} MB, {:.2} bytes per row",
        rows / (ROUNDS * UPDATES_PER_ROUND) as u64,
        bytes as f64 / 1e6,
        bytes as f64 / rows as f64
    );
    Ok(())
}
