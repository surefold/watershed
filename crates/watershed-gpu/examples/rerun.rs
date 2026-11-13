//! After a batch run, one policy changes: bring every scenario's answer up
//! to date without running the book again.
//!
//! EXPERIMENTAL. This asks one question: the graph knows what a change
//! touches, so can a run over thousands of market paths be brought up to
//! date by working out only that, on every path?
//!
//! The book's totals are a tree of sums, eight to a node. The first run
//! keeps, for every path, the value of every node of that tree from some
//! level up (level 0 is each policy's own answer). When a policy changes,
//! only the kept node above it is worked out again, by a small plan made of
//! just the nodes under it, and the sums above are added again from the kept
//! values, in the order the graph adds them. The result is checked, to the
//! last bit, against making the whole plan again and running all of it.
//!
//!     cargo run --release --features cuda --example rerun -- export [kinds] [paths] [level kept]

use std::path::Path;
use std::time::Instant;

#[path = "shared/savings.rs"]
mod savings;
use savings::*;
use watershed_core::NodeId;
use watershed_core::prelude::*;

/// The tree of sums over the policies, for each measure: `levels[0]` is each
/// policy's own answer, and each level after adds up eight of the one below.
fn tree(book: &mut Book) -> Result<Vec<Vec<Vec<Node<f64>>>>> {
    let mut trees = Vec::with_capacity(MEASURES);
    for measure in 0..MEASURES {
        let mut levels = vec![book.each.iter().map(|e| e[measure]).collect::<Vec<_>>()];
        while levels.last().expect("a level").len() > 1 {
            let below = levels.last().expect("a level");
            let round = levels.len();
            let mut next = Vec::with_capacity(below.len() / 8 + 1);
            for (k, group) in below.chunks(8).enumerate() {
                let mut all = group[0].into_expr();
                for &node in &group[1..] {
                    all = all + node;
                }
                next.push(book.g.define(&format!("total{measure}_{round}_{k}"), all)?);
            }
            levels.push(next);
        }
        trees.push(levels);
    }
    book.g.stabilize();
    Ok(trees)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .ok_or("give the folder the reference wrote to")?;
    let dir = Path::new(&dir);
    let kinds: usize = args.next().map_or(Ok(20_000), |a| a.parse())?;
    let scenarios: usize = args.next().map_or(Ok(10_000), |a| a.parse())?;
    let keep: usize = args.next().map_or(Ok(1), |a| a.parse())?;

    let tables = tables(dir)?;
    let policies = policies(&dir.join("points_book.csv"), kinds)?;
    let kinds = policies.len();
    let best = Assumptions::best_estimate(&tables, 0.0);
    let runner = Runner::new()?;

    let started = Instant::now();
    let mut book = Book::new(&policies, &tables, &best)?;
    let trees = tree(&mut book)?;
    let depth = trees[0].len();
    let keep = keep.min(depth - 1);
    println!(
        "{} kinds of policy, {} market paths; graph made in {}",
        commas(kinds as f64),
        commas(scenarios as f64),
        time(started.elapsed())
    );

    // What the first run keeps: every node of the trees from level `keep`
    // up. `column[measure][level - keep]` is where that level starts in a
    // path's row of results.
    let mut outputs: Vec<NodeId> = Vec::new();
    let mut column = vec![Vec::new(); MEASURES];
    for measure in 0..MEASURES {
        for level in &trees[measure][keep..] {
            column[measure].push(outputs.len());
            outputs.extend(level.iter().map(|n| n.id()));
        }
    }
    let width = outputs.len();
    let roots: Vec<usize> = (0..MEASURES)
        .map(|m| *column[m].last().expect("a root"))
        .collect();

    let started = Instant::now();
    let plan = book.plan(&outputs)?;
    let plan_took = started.elapsed();

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (drift, spread) = (
        (GROWTH - 0.5 * SWING * SWING) / 12.0,
        SWING * (1.0f64 / 12.0).sqrt(),
    );
    let inputs: Vec<f64> = (0..scenarios * MONTHS)
        .map(|_| (drift + spread * rng.normal()).exp() - 1.0)
        .collect();

    let started = Instant::now();
    let mut kept = runner.run(&plan, &inputs)?;
    let first = started.elapsed();
    println!(
        "the whole book: plan of {} nodes made in {}, run on the {} in {}",
        commas(plan.nodes() as f64),
        time(plan_took),
        runner.name(),
        time(first)
    );
    println!(
        "  kept for each path: {} values, the trees of sums from level {keep} up ({:.0} MB in all)",
        commas(width as f64),
        (width * scenarios * 8) as f64 / 1e6
    );
    println!();

    // Amendments to policies with few, middling and many months left.
    let mut by_months: Vec<usize> = (0..kinds).collect();
    by_months.sort_by_key(|&k| policies[k].months_left());
    let chosen = [
        by_months[kinds / 20],
        by_months[kinds / 4],
        by_months[kinds / 2],
        by_months[kinds * 3 / 4],
        by_months[kinds - 1],
    ];
    println!(
        "{:>8} {:>7} {:>9} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "policy", "months", "nodes", "graph", "plan", "run, CPU", "run, GPU", "add again"
    );
    let mut worst = (0.0f64, 0.0f64);
    for (n, &k) in chosen.iter().enumerate() {
        // The graph's own update, for the one path it holds.
        let started = Instant::now();
        book.g
            .set(book.promised[k], policies[k].sum_assured * 1.1)?;
        book.g.stabilize();
        let graph_took = started.elapsed();

        // A plan of just the kept node above the policy, for each measure.
        let at = k >> (3 * keep);
        let started = Instant::now();
        let above: Vec<NodeId> = (0..MEASURES).map(|m| trees[m][keep][at].id()).collect();
        let small = book.plan(&above)?;
        let small_took = started.elapsed();

        let started = Instant::now();
        let again = small.run_cpu(&inputs);
        let cpu_took = started.elapsed();
        #[cfg(feature = "cuda")]
        let gpu_took = {
            let started = Instant::now();
            let on_gpu = runner.gpu.run(&small, &inputs)?;
            let took = started.elapsed();
            assert!(
                on_gpu
                    .iter()
                    .zip(&again)
                    .all(|(a, b)| a.to_bits() == b.to_bits()),
                "the GPU and the CPU disagree"
            );
            time(took)
        };
        #[cfg(not(feature = "cuda"))]
        let gpu_took = String::from("-");

        // Put the new values in, and add the sums above them again.
        let started = Instant::now();
        for (s, row) in kept.chunks_mut(width).enumerate() {
            for m in 0..MEASURES {
                row[column[m][0] + at] = again[s * MEASURES + m];
                let mut child = at;
                for level in keep + 1..depth {
                    let parent = child / 8;
                    let below = column[m][level - 1 - keep];
                    let first = parent * 8;
                    let last = (first + 8).min(trees[m][level - 1].len());
                    let mut all = row[below + first];
                    for x in &row[below + first + 1..below + last] {
                        all += x;
                    }
                    row[column[m][level - keep] + parent] = all;
                    child = parent;
                }
            }
        }
        let add_took = started.elapsed();
        println!(
            "{:>8} {:>7} {:>9} {:>10} {:>10} {:>10} {:>10} {:>10}",
            k,
            policies[k].months_left(),
            commas(small.nodes() as f64),
            time(graph_took),
            time(small_took),
            time(cpu_took),
            gpu_took,
            time(add_took)
        );
        let total = (small_took + cpu_took + add_took).as_secs_f64();
        worst = (worst.0.max(total), worst.1.max(first.as_secs_f64() / total));

        // After the first and the last amendment: the same as making the
        // whole plan again and running all of it?
        if n == 0 || n == chosen.len() - 1 {
            let started = Instant::now();
            let whole = book.plan(&outputs)?;
            let replanned = started.elapsed();
            let started = Instant::now();
            let fresh = runner.run(&whole, &inputs)?;
            let reran = started.elapsed();
            let same = fresh
                .iter()
                .zip(&kept)
                .all(|(a, b)| a.to_bits() == b.to_bits());
            let moved = (0..scenarios)
                .filter(|s| fresh[s * width + roots[NET]] != 0.0)
                .count();
            println!(
                "    checked: the whole plan made again ({}) and run again ({}) gives {} for all {} values on all {} paths",
                time(replanned),
                time(reran),
                if same {
                    "the same to the last bit"
                } else {
                    "SOMETHING ELSE"
                },
                commas(width as f64),
                commas(moved as f64)
            );
            assert!(same, "bringing the run up to date gave something else");
        }
    }
    println!();
    println!(
        "bringing {} paths up to date after one policy changes took at most {} (plan, CPU run and adding again),",
        commas(scenarios as f64),
        time(std::time::Duration::from_secs_f64(worst.0))
    );
    println!(
        "against {} to run the book again with a plan already made.",
        time(first)
    );
    Ok(())
}
