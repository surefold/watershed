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
use watershed_gpu::Kept;

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
    // The level of the trees to keep from, or `most=N` to let the engine
    // choose up to N nodes without being told about the trees.
    let keep = args.next().unwrap_or_else(|| "1".into());
    let most: Option<usize> = keep.strip_prefix("most=").map(str::parse).transpose()?;
    let keep: usize = if most.is_some() { 0 } else { keep.parse()? };

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
    // up.
    let vary: Vec<NodeId> = book.returns.iter().map(|r| r.id()).collect();
    let roots: Vec<NodeId> = trees.iter().map(|t| t[depth - 1][0].id()).collect();
    let levels: Vec<NodeId> = trees
        .iter()
        .flat_map(|t| t[keep..].iter().flatten().map(|n| n.id()))
        .collect();
    let levels = match most {
        Some(most) => Kept::nearest(&book.g, &roots, most),
        None => levels,
    };
    let started = Instant::now();
    let mut kept = Kept::new(&book.g, &vary, &book.knobs.ids(), &roots, &levels)?;
    let plan_took = started.elapsed();
    let width = kept.width();

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (drift, spread) = (
        (GROWTH - 0.5 * SWING * SWING) / 12.0,
        SWING * (1.0f64 / 12.0).sqrt(),
    );
    let inputs: Vec<f64> = (0..scenarios * MONTHS)
        .map(|_| (drift + spread * rng.normal()).exp() - 1.0)
        .collect();

    let started = Instant::now();
    let results = runner.run(kept.plan(), &inputs)?;
    let first = started.elapsed();
    println!(
        "the whole book: plan of {} nodes made in {} (with what each kept node uses), run on the {} in {}",
        commas(kept.plan().nodes() as f64),
        time(plan_took),
        runner.name(),
        time(first)
    );
    kept.hold(inputs.clone(), results);
    println!(
        "  kept for each path: {} values, {} ({:.0} MB in all)",
        commas(width as f64),
        match most {
            Some(most) => format!("the nearest the outputs, at most {}", commas(most as f64)),
            None => format!("the trees of sums from level {keep} up"),
        },
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
        "policy", "months", "nodes", "graph", "plan", "run, CPU", "run, GPU", "put in"
    );
    let mut worst = (0.0f64, 0.0f64);
    for (n, &k) in chosen.iter().enumerate() {
        // The graph's own update, for the one path it holds.
        let started = Instant::now();
        book.g
            .set(book.promised[k], policies[k].sum_assured * 1.1)?;
        book.g.stabilize();
        let graph_took = started.elapsed();

        // What to run: a plan of the kept nodes the change reaches, cut
        // at the kept nodes it does not.
        let started = Instant::now();
        let refresh = kept
            .refresh(&book.g, &[book.promised[k].id()])?
            .ok_or("nothing kept uses the policy")?;
        let small_took = started.elapsed();

        let started = Instant::now();
        let again = refresh.plan.run_cpu(&refresh.inputs);
        let cpu_took = started.elapsed();
        #[cfg(feature = "gpu")]
        let gpu_took = {
            let started = Instant::now();
            let on_gpu = runner.gpu.run(&refresh.plan, &refresh.inputs)?;
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
        #[cfg(not(feature = "gpu"))]
        let gpu_took = String::from("-");

        let started = Instant::now();
        kept.apply(&refresh, &again);
        let add_took = started.elapsed();
        println!(
            "{:>8} {:>7} {:>9} {:>10} {:>10} {:>10} {:>10} {:>10}",
            k,
            policies[k].months_left(),
            commas(refresh.plan.nodes() as f64),
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
            let whole = Kept::new(&book.g, &vary, &book.knobs.ids(), &roots, &levels)?;
            let replanned = started.elapsed();
            let started = Instant::now();
            let fresh = runner.run(whole.plan(), &inputs)?;
            let reran = started.elapsed();
            let same = fresh
                .iter()
                .zip(kept.values())
                .all(|(a, b)| a.to_bits() == b.to_bits());
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
                commas(scenarios as f64)
            );
            assert!(same, "bringing the run up to date gave something else");
        }
    }
    println!();
    println!(
        "bringing {} paths up to date after one policy changes took at most {} (plan, CPU run and putting in),",
        commas(scenarios as f64),
        time(std::time::Duration::from_secs_f64(worst.0))
    );
    println!(
        "against {} to run the book again with a plan already made.",
        time(first)
    );
    Ok(())
}
