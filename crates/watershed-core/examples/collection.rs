//! How a collection's totals scale: a table of rows with a calculated column
//! and four totals, changing one row at a time.
//!
//!     cargo run --release --example collection -- 1000000

use std::time::Instant;

use watershed_core::prelude::*;

fn main() -> Result<(), CalcError> {
    let rows: usize = std::env::args().nth(1).map_or(1_000_000, |n| {
        n.replace('_', "").parse().expect("a number of rows")
    });
    let mut g = Graph::new();
    let fx = g.input("fx", 1.25)?;
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty")?;
    let price = orders.field::<f64>("price")?;
    let line = orders.column(&mut g, "line", qty * price * fx)?;
    let revenue = orders.sum(&mut g, "revenue", line)?;
    let count = orders.count(&mut g, "count")?;
    let largest = orders.max(&mut g, "largest", line)?;
    let smallest = orders.min(&mut g, "smallest", line)?;

    let started = Instant::now();
    for i in 0..rows {
        orders.upsert(
            &mut g,
            &format!("o{i}"),
            &[qty.is((i % 100) as f64), price.is(1.5)],
        )?;
    }
    g.stabilize();
    println!(
        "{rows} rows ({} nodes) built and totalled in {:.0} ms",
        g.len(),
        started.elapsed().as_secs_f64() * 1e3
    );

    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let updates = 10_000;
    let started = Instant::now();
    let mut recalculated = 0;
    for _ in 0..updates {
        let key = format!("o{}", next() % rows as u64);
        orders.upsert(&mut g, &key, &[qty.is((next() % 100) as f64)])?;
        recalculated += g.stabilize().recalculated;
    }
    let each = started.elapsed() / updates;
    println!(
        "change one row: {:.1} µs per update, {:.1} nodes recalculated on average (the row's line and up to four totals)",
        each.as_secs_f64() * 1e6,
        recalculated as f64 / updates as f64
    );

    let started = Instant::now();
    for i in 0..1_000 {
        orders.remove(&mut g, &format!("o{i}"))?;
    }
    g.stabilize();
    println!(
        "remove 1,000 rows in one update: {:.1} ms",
        started.elapsed().as_secs_f64() * 1e3
    );

    println!(
        "revenue {:.2}, {} rows, line from {} to {}",
        g.get(revenue)?,
        g.get(count)?,
        g.get(smallest)?,
        g.get(largest)?
    );
    g.check_consistency().map_err(CalcError::Invalid)?;
    println!("checked: every total matches adding the rows up from scratch");
    Ok(())
}
