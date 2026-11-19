//! What an index costs and saves: a million rows indexed by a field with a
//! thousand values, looked up against reading every row.
//!
//!     cargo run --release --example index -- 1000000

use std::time::Instant;

use watershed_core::prelude::*;

fn main() -> Result<(), CalcError> {
    let rows: usize = std::env::args().nth(1).map_or(1_000_000, |n| {
        n.replace('_', "").parse().expect("a number of rows")
    });
    let buckets = 1_000_i64;
    let mut g = Graph::new();
    let mut orders = Collection::new(&g, "orders");
    let bucket = orders.field::<i64>("bucket")?;
    let qty = orders.field::<f64>("qty")?;
    for i in 0..rows {
        orders.upsert(
            &mut g,
            &format!("o{i}"),
            &[bucket.is(i as i64 % buckets), qty.is(1.0)],
        )?;
    }
    g.stabilize();
    let nodes = g.len();

    let started = Instant::now();
    let by_bucket = orders.index(&mut g, "bucket_index", bucket)?;
    println!(
        "index {rows} rows: {:.0} ms, {} nodes added",
        started.elapsed().as_secs_f64() * 1e3,
        g.len() - nodes
    );

    let lookups = 1_000;
    let started = Instant::now();
    let mut found = 0;
    for b in 0..lookups {
        found += orders.lookup(&g, by_bucket, b % buckets).len();
    }
    let each = started.elapsed() / lookups as u32;
    println!(
        "lookup: {:.1} µs each, {} rows per lookup",
        each.as_secs_f64() * 1e6,
        found / lookups as usize
    );

    let scans = 10;
    let started = Instant::now();
    let mut scanned = 0;
    for b in 0..scans {
        let keys: Vec<&str> = orders.keys().collect();
        scanned += keys
            .into_iter()
            .filter(|k| orders.get(&g, k, bucket).unwrap() == b % buckets)
            .count();
    }
    let each = started.elapsed() / scans as u32;
    println!(
        "scan every row instead: {:.1} ms each, {} rows per scan",
        each.as_secs_f64() * 1e3,
        scanned / scans as usize
    );

    let updates = 10_000_u32;
    let started = Instant::now();
    for i in 0..updates as usize {
        let key = format!("o{}", (i * 7_919) % rows);
        orders.upsert(&mut g, &key, &[bucket.is((i as i64 * 31) % buckets)])?;
        g.stabilize();
    }
    println!(
        "move one row to another value: {:.1} µs per update",
        (started.elapsed() / updates).as_secs_f64() * 1e6
    );
    g.check_consistency().map_err(CalcError::Invalid)?;
    Ok(())
}
