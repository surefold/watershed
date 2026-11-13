//! Keeping a valuation up to date as policies change.
//!
//! An insurer's book is not valued once. Through the day policyholders top
//! up, cash in and change their cover, and each time someone wants the
//! book's value as it now is. A model that works on whole columns of
//! policies at once values the book quickly, and then has to value all of
//! it again when one policy changes. A graph works out only what the change
//! touches.
//!
//! This is the same book and the same model both ways: lifelib's
//! `savings.CashValue_ME_EX2` on one market path, and the graph of it that
//! the `liabilities` example checks against lifelib. The times for lifelib
//! are measured by `examples/shared/amendments_reference.py` on the same
//! machine:
//!
//!     python examples/shared/liabilities_reference.py   # once, for the book
//!     python examples/shared/amendments_reference.py export
//!     cargo run --release --example amendments -- export
//!
//! Nothing here uses a GPU.

use std::path::Path;
use std::time::{Duration, Instant};

#[path = "shared/savings.rs"]
mod savings;
use savings::*;

fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .ok_or("give the folder the reference wrote to")?;
    let dir = Path::new(&dir);
    let text = std::fs::read_to_string(dir.join("amendments.csv"))?;
    let theirs: Vec<f64> = text
        .lines()
        .nth(1)
        .ok_or("amendments.csv is empty")?
        .split(',')
        .map(|x| x.parse())
        .collect::<std::result::Result<_, _>>()?;
    let [kinds, whole, again, alone, before, first_changed, after] = theirs[..] else {
        return Err("amendments.csv is not as amendments_reference.py writes it".into());
    };
    let (kinds, first_changed) = (kinds as usize, first_changed as usize);

    let tables = tables(dir)?;
    let policies = policies(&dir.join("points_book.csv"), kinds)?;
    let path = numbers(&dir.join("returns_1.f64"))?;
    let best = Assumptions::best_estimate(&tables, 0.0);

    // The book as a graph, on the reference's market path.
    let started = Instant::now();
    let mut book = Book::new(&policies, &tables, &best)?;
    let total = book.totals()?[NET];
    let built = started.elapsed();
    let started = Instant::now();
    for (&input, &x) in book.returns.iter().zip(&path) {
        book.g.set(input, x)?;
    }
    let all = book.g.stabilize();
    let everything = started.elapsed();
    let ours = book.g.get(total)?;
    println!(
        "{} kinds of policy ({} policies): {} nodes",
        commas(kinds as f64),
        commas(policies.iter().map(|p| p.count).sum()),
        commas(all.recalculated as f64)
    );
    println!(
        "the book's value: {} here, {} by the reference (they differ by {:.1e} of it)",
        commas(ours),
        commas(before),
        (ours - before).abs() / before.abs()
    );
    assert!(
        (ours - before).abs() <= 1e-9 * before.abs(),
        "the two disagree"
    );

    // The amendment the reference made first: the same answer?
    let raise = |book: &mut Book, k: usize, by: f64| -> Result<f64> {
        book.g.set(book.promised[k], policies[k].sum_assured * by)?;
        book.g.stabilize();
        Ok(book.g.get(total)?)
    };
    let amended = raise(&mut book, first_changed, 1.1)?;
    println!(
        "after one policy's guarantee goes up a tenth: {} here, {} by the reference ({:.1e})",
        commas(amended),
        commas(after),
        (amended - after).abs() / after.abs()
    );
    assert!(
        (amended - after).abs() <= 1e-9 * after.abs(),
        "the two disagree"
    );
    raise(&mut book, first_changed, 1.0)?;
    println!();

    // A thousand amendments, each to another policy, the value read after
    // each.
    let amendments = 1_000.min(kinds);
    let started = Instant::now();
    let mut recalculated = 0;
    for a in 0..amendments {
        let k = (a * 7919) % kinds;
        book.g
            .set(book.promised[k], policies[k].sum_assured * 1.1)?;
        recalculated += book.g.stabilize().recalculated;
        std::hint::black_box(book.g.get(total)?);
    }
    let each = started.elapsed() / amendments as u32;
    println!(
        "{amendments} amendments, the book's value after each: {} each, working out {} of the {} nodes again",
        time(each),
        commas(recalculated as f64 / amendments as f64),
        commas(all.recalculated as f64)
    );
    println!();

    let seconds = Duration::from_secs_f64;
    println!("{:<58} {:>12} {:>12}", "", "reference", "the graph");
    println!(
        "{:<58} {:>12} {:>12}",
        "valuing the book the first time",
        time(seconds(whole)),
        time(built + everything)
    );
    println!(
        "{:<58} {:>12} {:>12}",
        "  of which, building the graph",
        "",
        time(built)
    );
    println!(
        "{:<58} {:>12} {:>12}",
        "after a new market path (everything changes)",
        time(seconds(whole)),
        time(everything)
    );
    println!(
        "{:<58} {:>12} {:>12}",
        "after one policy changes",
        time(seconds(again)),
        time(each)
    );
    println!(
        "{:<58} {:>12} {:>12}",
        "  the reference valuing only that policy, by hand",
        time(seconds(alone)),
        ""
    );
    println!();
    println!(
        "after one policy changes the graph is {} times faster than the reference valuing the book again,",
        commas(again / each.as_secs_f64())
    );
    println!(
        "and {} times faster than the reference valuing just that policy.",
        commas(alone / each.as_secs_f64())
    );
    println!(
        "A day of {} amendments: {} by the graph, {} by the reference.",
        commas(100_000.0),
        time(each * 100_000),
        time(seconds(again * 100_000.0))
    );
    Ok(())
}
