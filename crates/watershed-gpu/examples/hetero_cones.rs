//! EXPERIMENTAL. Not part of the library; written for the research note
//! `docs/research/heterogeneous-execution.md`. No GPU is used.
//!
//! On the insurance book of the `liabilities` example, for several kinds of
//! change (an assumption, one month's market return, one policy's promise):
//! how much of what the scenario plan calculates depends on it, and how many
//! values it does *not* change would have to be on hand to work out only the
//! part that does. That is what recalculating only the affected part for
//! every scenario would cost and need.
//!
//!     cargo run --release --example hetero_cones -- <export dir> [kinds]

#![allow(dead_code)]

use std::path::Path;
use std::time::Instant;

use watershed_core::NodeId;

#[path = "shared/savings.rs"]
mod savings;
use savings::*;

/// Everything reached from `from` by `next`, as one flag a node.
fn reach(len: usize, from: &[NodeId], next: impl Fn(NodeId) -> Vec<NodeId>) -> Vec<bool> {
    let mut seen = vec![false; len];
    let mut stack: Vec<NodeId> = from.to_vec();
    for id in from {
        seen[id.index()] = true;
    }
    while let Some(id) = stack.pop() {
        for other in next(id) {
            if !std::mem::replace(&mut seen[other.index()], true) {
                stack.push(other);
            }
        }
    }
    seen
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let dir = Path::new(args.get(1).expect("the export directory"));
    let size = args.get(2).map_or(4000, |n| n.parse().expect("a number"));
    let tables = tables(dir)?;
    let best = Assumptions::best_estimate(&tables, 0.0);
    let book_policies = policies(&dir.join("points_book.csv"), size)?;
    let mut book = Book::new(&book_policies, &tables, &best)?;
    let totals = book.totals()?.map(|t| t.id());
    let plan = book.plan(&totals)?;
    let g = &book.g;
    println!(
        "{} kinds; graph {} nodes; plan {} nodes for every scenario, {} levels, {} slots a scenario",
        book_policies.len(),
        g.len(),
        plan.nodes(),
        plan.levels(),
        plan.bytes_per_scenario() / 8
    );

    // What the totals depend on, and of that what a market path reaches:
    // the nodes worked out for every scenario.
    let cone = reach(g.len(), &totals, |id| g.dependencies(id));
    let returns: Vec<NodeId> = book.returns.iter().map(|r| r.id()).collect();
    let varies = reach(g.len(), &returns, |id| g.dependents(id));
    let ids: Vec<NodeId> = g.node_ids().collect();
    assert!(ids.iter().enumerate().all(|(n, id)| id.index() == n));
    let every: Vec<bool> = (0..g.len())
        .map(|n| cone[n] && varies[n] && !g.is_input(ids[n]))
        .collect();
    let all = every.iter().filter(|&&x| x).count();
    println!("nodes in the totals' cone that a market path reaches: {all}");
    println!();
    println!(
        "{:<28} {:>11} {:>8} {:>7} {:>12} {:>10} {:>12}",
        "what changes", "affected", "share", "levels", "kept values", "graph", "recalculated"
    );

    let knobs = &book.knobs;
    let mid = book.promised.len() / 2;
    let changes: Vec<(String, NodeId)> = vec![
        ("promise of one policy".into(), book.promised[mid].id()),
        ("return, last month".into(), returns[MONTHS - 1]),
        ("return, month 120".into(), returns[120]),
        ("return, first month".into(), returns[0]),
        (
            "discount rate, month 239".into(),
            knobs.discount[MONTHS - 1].id(),
        ),
        ("discount rate, month 120".into(), knobs.discount[120].id()),
        ("discount rate, month 0".into(), knobs.discount[0].id()),
        ("expenses".into(), knobs.expenses.id()),
        ("mortality".into(), knobs.mortality.id()),
        ("lapses".into(), knobs.lapses.id()),
        ("fee".into(), knobs.fee.id()),
    ];
    let mut measured = Vec::new();
    for (what, input) in &changes {
        let reached = reach(g.len(), &[*input], |id| g.dependents(id));
        let mut affected = 0usize;
        let mut heights = std::collections::HashSet::new();
        // Values the affected nodes read that are worked out for every
        // scenario and are not themselves affected (a varied input counts:
        // it is a row of the table too).
        let mut kept = vec![false; g.len()];
        for n in 0..g.len() {
            if !(every[n] && reached[n]) {
                continue;
            }
            affected += 1;
            let id = ids[n];
            heights.insert(g.height(id));
            for read in g.dependencies(id) {
                let r = read.index();
                if varies[r] && !reached[r] {
                    kept[r] = true;
                }
            }
        }
        let kept = kept.iter().filter(|&&x| x).count();
        measured.push((what.clone(), *input, affected, heights.len(), kept));
    }
    // Now the graph's own update for each change, one after another.
    for (what, input, affected, levels, kept) in measured {
        let value = match book.g.value(input) {
            watershed_core::Value::F64(x) => *x,
            _ => unreachable!("every one of these is a number"),
        };
        let new = if value == 0.0 { 0.001 } else { value * 1.01 };
        book.g
            .set_value(input, watershed_core::Value::F64(new))
            .expect("an input");
        let started = Instant::now();
        let report = book.g.stabilize();
        let took = started.elapsed();
        println!(
            "{what:<28} {affected:>11} {:>7.3}% {levels:>7} {kept:>12} {:>10} {:>12}",
            100.0 * affected as f64 / all as f64,
            time(took),
            report.recalculated
        );
    }
    Ok(())
}
