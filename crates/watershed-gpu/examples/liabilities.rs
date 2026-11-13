//! What are an insurer's savings policies with guarantees going to cost it?
//!
//! A policyholder pays in, the money is invested in a fund, and the insurer
//! promises a least amount on death and at the end of the term whatever the
//! fund has done. If markets fall the insurer makes up the difference. How
//! much that will cost depends on the path the markets take, on who dies and
//! who cashes in early, and on fees, so there is no formula: the insurer
//! projects every policy month by month to its end, along thousands of
//! possible market paths.
//!
//! The model here is `savings.CashValue_ME_EX2` from lifelib
//! (<https://lifelib.io>, MIT licence), an open-source reference model, built
//! as a watershed graph: every policy's every month is a few nodes (its fund
//! before and after fees and growth, deaths, lapses, policies left, and the
//! running present value of each cash flow). A scenario is one market path:
//! the fund's return in each of 240 months. The assumptions (the discount
//! curve, fees, and how far to shift mortality, lapses, expenses and the
//! fund) are shared inputs, so a regulator's stress is one `set_shared` and
//! one more run.
//!
//! First run the reference, which writes its tables, some policies, its
//! market paths and its answers to a folder called `export`:
//!
//!     pip install lifelib pandas openpyxl scipy
//!     mkdir export && python examples/shared/liabilities_reference.py
//!
//! Then:
//!
//!     cargo run --release --features cuda --example liabilities -- /path/to/export
//!
//! and optionally how many kinds of policy (20,000) and how many market paths
//! (10,000). It checks itself against the reference's answers before doing
//! anything bigger, then values the book, stresses it, and shows what the
//! graph does when one thing changes.
//!
//! One thing differs from the model as published: where lapses follow how
//! far the fund is above the guarantee, the published model scales the
//! yearly lapse rate and takes its twelfth root; here the monthly rate is
//! scaled, because there are no powers on the GPU path yet. The check for
//! that part is against the reference changed in the same way.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use watershed_core::NodeId;

#[path = "shared/savings.rs"]
mod savings;
use savings::*;

/// The bell curve's area to the left of `x`, to about seven places.
fn area_left_of(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.231_641_9 * x.abs());
    let poly = t
        * (0.319_381_530
            + t * (-0.356_563_782
                + t * (1.781_477_937 + t * (-1.821_255_978 + t * 1.330_274_429))));
    let right = (-x * x / 2.0).exp() / std::f64::consts::TAU.sqrt() * poly;
    if x >= 0.0 { 1.0 - right } else { right }
}

/// Checks a book against the reference's answers for it: `policies` on the
/// reference's own market paths, under `assumptions`. Gives the largest
/// difference, as a share of the policy's largest answer.
fn check(
    dir: &Path,
    answers: &str,
    policies: &[Policy],
    tables: &Tables,
    assumptions: &Assumptions,
    paths: &[f64],
    runner: &Runner,
) -> Result<(f64, Vec<f64>)> {
    let book = Book::new(policies, tables, assumptions)?;
    let outputs: Vec<NodeId> = book.each.iter().flatten().map(|n| n.id()).collect();
    let plan = book.plan(&outputs)?;
    let scenarios = paths.len() / 242;
    // The reference draws 242 months for each path; a policy uses 240.
    let inputs: Vec<f64> = paths
        .chunks(242)
        .flat_map(|path| &path[..MONTHS])
        .copied()
        .collect();
    let ours = runner.run(&plan, &inputs)?;
    let theirs = numbers(&dir.join(answers))?;
    assert_eq!(theirs.len(), ours.len(), "{answers} is for another book");
    // A difference is measured against the size of the policy's answers: an
    // answer can be a few pence left from flows of millions cancelling, and
    // the last digits of those are not the model's.
    let mut worst = 0.0f64;
    for p in 0..policies.len() {
        let theirs = &theirs[p * scenarios * MEASURES..(p + 1) * scenarios * MEASURES];
        let size = theirs.iter().fold(1.0f64, |size, x| size.max(x.abs()));
        for s in 0..scenarios {
            for m in 0..MEASURES {
                let ours = ours[(s * policies.len() + p) * MEASURES + m];
                worst = worst.max((ours - theirs[s * MEASURES + m]).abs() / size);
            }
        }
    }
    Ok((worst, ours))
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .ok_or("give the folder the reference wrote its tables and answers to")?;
    let dir = Path::new(&dir);
    let number = |arg: Option<String>, otherwise: usize| match arg {
        Some(n) => n.replace('_', "").parse().expect("a number"),
        None => otherwise,
    };
    let size = number(args.next(), 20_000);
    let scenarios = number(args.next(), 10_000);
    let tables = tables(dir)?;
    let runner = Runner::new()?;
    let best = Assumptions::best_estimate(&tables, 0.0);
    let mut report = String::from("{\n");

    // ---- 1. Is it right? ----
    println!("1. Against the reference (lifelib's savings.CashValue_ME_EX2)");
    println!();
    let published = policies(&dir.join("points_published.csv"), usize::MAX)?;
    let paths = numbers(&dir.join("returns_10000.f64"))?;
    println!(
        "  the model as published: 9 policies on its 10,000 market paths, four answers for each"
    );
    let mut checks = Vec::new();
    let mut plain = Vec::new();
    for (sim, what, fee, mortality, lapses) in [
        (1, "no fees, deaths or lapses", 0.0, 0.0, 0.0),
        (2, "with fees", 0.01 / 12.0, 0.0, 0.0),
        (3, "with fees and deaths", 0.01 / 12.0, 1.0, 0.0),
        (4, "with fees, deaths and lapses", 0.01 / 12.0, 1.0, 1.0),
    ] {
        let assumptions = Assumptions {
            fee,
            mortality,
            lapses,
            lapses_follow_fund: 0.0,
            ..best.clone()
        };
        let (worst, ours) = check(
            dir,
            &format!("published_{sim}.f64"),
            &published,
            &tables,
            &assumptions,
            &paths,
            &runner,
        )?;
        println!("    simulation {sim}, {what:<30} largest difference {worst:.1e}");
        assert!(worst < 1e-9, "simulation {sim} differs from the reference");
        checks.push((
            format!("Published model, simulation {sim}: {what}"),
            9 * 10_000 * 4,
            worst,
        ));
        if sim == 1 {
            plain = ours;
        }
    }
    // With nothing but the market, the promise at the end of the term is an
    // option that has a formula (Black and Scholes): the average over the
    // paths should be close to it.
    println!("  the promise at the end, with nothing but the market, against the formula for it:");
    let mut against_formula = Vec::new();
    for (p, policy) in published.iter().enumerate().step_by(2) {
        let average = (0..10_000)
            .map(|s| plain[(s * published.len() + p) * MEASURES + AT_END])
            .sum::<f64>()
            / 10_000.0;
        let (rate, swing, years) = (0.02, 0.03, policy.term as f64);
        let (fund, strike) = (
            policy.premium * policy.count,
            policy.sum_assured * policy.count,
        );
        let d1 =
            ((fund / strike).ln() + (rate + 0.5 * swing * swing) * years) / (swing * years.sqrt());
        let d2 = d1 - swing * years.sqrt();
        let formula = strike * (-rate * years).exp() * area_left_of(-d2) - fund * area_left_of(-d1);
        println!(
            "    paying in {:>7} for a promise of {}: {:>10} by the formula, {:>10} over 10,000 paths",
            commas(policy.premium),
            commas(policy.sum_assured),
            commas(formula),
            commas(average)
        );
        against_formula.push((policy.premium, formula, average));
    }

    let sample = policies(&dir.join("points_book.csv"), 200)?;
    let paths = numbers(&dir.join("returns_1000.f64"))?;
    println!("  every kind of policy: 200 policies on 1,000 market paths");
    for (answers, what, follow) in [
        ("book_static.f64", "lapses by the table", 0.0),
        ("book_dynamic.f64", "lapses that follow the fund", 1.0),
    ] {
        let assumptions = Assumptions {
            lapses_follow_fund: follow,
            ..best.clone()
        };
        let (worst, _) = check(
            dir,
            answers,
            &sample,
            &tables,
            &assumptions,
            &paths,
            &runner,
        )?;
        println!("    {what:<41} largest difference {worst:.1e}");
        assert!(worst < 1e-9, "{answers} differs from the reference");
        checks.push((
            format!("Mixed book of 200 policies: {what}"),
            200 * 1_000 * 4,
            worst,
        ));
    }
    println!();

    // ---- 2. At scale ----
    println!("2. A whole book");
    println!();
    let book_policies = policies(&dir.join("points_book.csv"), size)?;
    let started = Instant::now();
    let mut book = Book::new(&book_policies, &tables, &best)?;
    let totals = book.totals()?;
    let graph_took = started.elapsed();
    let started = Instant::now();
    let mut plan = book.plan(&totals.map(|t| t.id()))?;
    let plan_took = started.elapsed();
    println!(
        "  {} kinds of policy ({} policies), {} months of policy to project",
        commas(book_policies.len() as f64),
        commas(book_policies.iter().map(|p| p.count).sum()),
        commas(book.policy_months as f64)
    );
    println!(
        "  the graph: {} nodes in {} levels, of {} shapes, {} bytes a path while it runs (built in {}; plan made in {})",
        commas(plan.nodes() as f64),
        commas(plan.levels() as f64),
        plan.shapes(),
        commas(plan.bytes_per_scenario() as f64),
        time(graph_took),
        time(plan_took)
    );

    // The market paths: the same draws serve every growth rate.
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let draws: Vec<f64> = (0..scenarios * MONTHS).map(|_| rng.normal()).collect();
    let paths_for = |growth: f64| -> Vec<f64> {
        let (drift, spread) = (
            (growth - 0.5 * SWING * SWING) / 12.0,
            SWING * (1.0f64 / 12.0).sqrt(),
        );
        draws
            .iter()
            .map(|z| (drift + spread * z).exp() - 1.0)
            .collect()
    };
    let inputs = paths_for(GROWTH);

    let started = Instant::now();
    let results = runner.run(&plan, &inputs)?;
    let took = started.elapsed();
    let projected = book.policy_months as f64 * scenarios as f64;
    println!(
        "  {}: {} market paths in {}: {} months of policy a second",
        runner.name(),
        commas(scenarios as f64),
        time(took),
        commas(projected / took.as_secs_f64())
    );
    // The same on the CPU for a few paths: the same to the last bit.
    let few = scenarios.min(128);
    let started = Instant::now();
    let on_cpu = plan.run_cpu(&inputs[..few * MONTHS]);
    let cpu_each = started.elapsed() / few as u32;
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    println!(
        "  CPU, {cores} cores: {} a path, so about {} for all of them",
        time(cpu_each),
        time(cpu_each * scenarios as u32)
    );
    assert!(
        on_cpu
            .iter()
            .zip(&results)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "the CPU and the {} differ",
        runner.name()
    );
    // How long the reference took for its part of this book says how long
    // it would take for all of it: it projects every policy on every path
    // for every month up to the longest term.
    let timing = std::fs::read_to_string(dir.join("timings.csv"))?;
    let line: Vec<&str> = timing
        .lines()
        .find(|l| l.starts_with("book_dynamic"))
        .ok_or("no timing for the reference")?
        .split(',')
        .collect();
    let (points, paths, seconds): (f64, f64, f64) =
        (line[1].parse()?, line[2].parse()?, line[3].parse()?);
    let reference = seconds * (book_policies.len() as f64 / points) * (scenarios as f64 / paths);
    println!(
        "  the reference (Python, one core) took {seconds:.0} s for {points} kinds of policy on {paths} paths, so about {} for this",
        time(Duration::from_secs_f64(reference))
    );
    println!();

    // ---- 3. What it says ----
    let column = |results: &[f64], m: usize| -> Vec<f64> {
        results.chunks(MEASURES).map(|r| r[m]).collect()
    };
    let mean = |xs: &[f64]| xs.iter().sum::<f64>() / xs.len() as f64;
    let steady = plan.run_cpu(&vec![(GROWTH / 12.0f64).exp() - 1.0; MONTHS]);
    let mut net = column(&results, NET);
    let (on_death, at_end, fees) = (
        mean(&column(&results, ON_DEATH)),
        mean(&column(&results, AT_END)),
        mean(&column(&results, FEES)),
    );
    let value = mean(&net);
    net.sort_by(f64::total_cmp);
    println!("3. What the book is worth to the insurer (present value, money in, less money out)");
    println!();
    println!(
        "  {:<44} {:>16} {:>16}",
        "",
        "one steady path",
        format!("{} paths", commas(scenarios as f64))
    );
    println!(
        "  {:<44} {:>16} {:>16}",
        "what the book is worth",
        commas(steady[NET]),
        commas(value)
    );
    println!(
        "  {:<44} {:>16} {:>16}",
        "cost of the promise on death",
        commas(steady[ON_DEATH]),
        commas(on_death)
    );
    println!(
        "  {:<44} {:>16} {:>16}",
        "cost of the promise at the end of the term",
        commas(steady[AT_END]),
        commas(at_end)
    );
    println!(
        "  {:<44} {:>16} {:>16}",
        "fees taken",
        commas(steady[FEES]),
        commas(fees)
    );
    println!(
        "  a single steady path misses {} of cost that only shows up when markets swing",
        commas(steady[NET] - value)
    );
    let at = |share: f64| net[((net.len() as f64 * share) as usize).min(net.len() - 1)];
    println!(
        "  across the paths: 1 in 200 is worse than {}, the middle one is {}, 1 in 200 is better than {}",
        commas(at(0.005)),
        commas(at(0.5)),
        commas(at(0.995))
    );
    println!();

    // ---- 4. Stresses ----
    println!("4. Stresses: each is the whole book on every path again");
    println!();
    println!(
        "  {:<44} {:>16} {:>16} {:>9}",
        "", "worth", "change", "took"
    );
    let mut stresses = Vec::new();
    type Change = Box<dyn Fn(&Tables) -> (Assumptions, f64)>;
    let with = |change: fn(&mut Assumptions)| -> Change {
        Box::new(move |tables| {
            let mut a = Assumptions::best_estimate(tables, 0.0);
            change(&mut a);
            (a, GROWTH)
        })
    };
    let rates = |shift: f64| -> Change {
        Box::new(move |tables| (Assumptions::best_estimate(tables, shift), GROWTH + shift))
    };
    let list: Vec<(&str, Change)> = vec![
        ("funds fall 39% today", with(|a| a.fund = 0.61)),
        ("15% more deaths", with(|a| a.mortality = 1.15)),
        ("half as many more lapses", with(|a| a.lapses = 1.5)),
        ("half as many lapses", with(|a| a.lapses = 0.5)),
        ("expenses 10% higher", with(|a| a.expenses = 1.1)),
        ("interest rates 1 point lower", rates(-0.01)),
        ("interest rates 1 point higher", rates(0.01)),
        ("fees halved", with(|a| a.fee /= 2.0)),
    ];
    for (what, change) in &list {
        let (assumptions, growth) = change(&tables);
        let started = Instant::now();
        plan.set_shared(&assumptions.values());
        let stressed = if growth == GROWTH {
            runner.run(&plan, &inputs)?
        } else {
            runner.run(&plan, &paths_for(growth))?
        };
        let worth = mean(&column(&stressed, NET));
        println!(
            "  {what:<44} {:>16} {:>16} {:>9}",
            commas(worth),
            signed(worth - value),
            time(started.elapsed())
        );
        stresses.push((*what, worth, worth - value));
    }
    plan.set_shared(&best.values());
    println!();

    // ---- 5. One thing changes ----
    println!("5. On the graph itself, when one thing changes");
    println!();
    let nodes = plan.nodes();
    let k = book_policies
        .iter()
        .position(|p| p.months_left() > 120)
        .unwrap_or(0);
    let started = Instant::now();
    book.g
        .set(book.promised[k], book_policies[k].sum_assured * 1.1)?;
    let one = book.g.stabilize();
    let one_took = started.elapsed();
    println!(
        "  one policyholder is promised 10% more: {} of {} nodes worked out again, in {}",
        commas(one.recalculated as f64),
        commas(nodes as f64),
        time(one_took)
    );
    book.g.set(book.promised[k], book_policies[k].sum_assured)?;
    book.g.stabilize();
    let started = Instant::now();
    book.g.set(book.returns[MONTHS - 12], -0.2)?;
    let late = book.g.stabilize();
    let late_took = started.elapsed();
    println!(
        "  the fund falls 20% in a month nineteen years from now: {} nodes worked out again, in {}",
        commas(late.recalculated as f64),
        time(late_took)
    );
    let started = Instant::now();
    book.g.set(book.knobs.fee, 0.005 / 12.0)?;
    let all = book.g.stabilize();
    let all_took = started.elapsed();
    println!(
        "  the fee is halved for everyone: {} nodes worked out again, in {}",
        commas(all.recalculated as f64),
        time(all_took)
    );

    // For the page that shows all this.
    let list_of = |items: Vec<String>| format!("[{}]", items.join(", "));
    writeln!(
        report,
        "  \"checks\": {},",
        list_of(
            checks
                .iter()
                .map(|(what, answers, worst)| format!(
                    "{{\"what\": \"{what}\", \"answers\": {answers}, \"worst\": {worst:e}}}"
                ))
                .collect()
        )
    )?;
    writeln!(
        report,
        "  \"formula\": {},",
        list_of(
            against_formula
                .iter()
                .map(|(premium, formula, average)| format!(
                    "{{\"premium\": {premium}, \"formula\": {formula}, \"paths\": {average}}}"
                ))
                .collect()
        )
    )?;
    writeln!(
        report,
        "  \"book\": {{\"kinds\": {}, \"policies\": {}, \"policy_months\": {}, \"nodes\": {}, \"levels\": {}, \"scenarios\": {scenarios}, \"graph_seconds\": {}, \"plan_seconds\": {}}},",
        book_policies.len(),
        book_policies.iter().map(|p| p.count).sum::<f64>(),
        book.policy_months,
        nodes,
        plan.levels(),
        graph_took.as_secs_f64(),
        plan_took.as_secs_f64()
    )?;
    writeln!(
        report,
        "  \"speed\": {{\"runner\": \"{}\", \"seconds\": {}, \"cpu_cores\": {cores}, \"cpu_seconds\": {}, \"reference_seconds\": {reference}}},",
        runner.name(),
        took.as_secs_f64(),
        (cpu_each * scenarios as u32).as_secs_f64()
    )?;
    writeln!(
        report,
        "  \"worth\": {{\"steady\": {:?}, \"paths\": [{value}, {on_death}, {at_end}, {fees}]}},",
        steady
    )?;
    // The spread of what the book is worth, as two hundred evenly spaced
    // points from the worst path to the best.
    let spread: Vec<String> = (0..=200)
        .map(|i| format!("{:.0}", net[(i * (net.len() - 1)) / 200]))
        .collect();
    writeln!(report, "  \"spread\": {},", list_of(spread))?;
    writeln!(
        report,
        "  \"stresses\": {},",
        list_of(
            stresses
                .iter()
                .map(|(what, worth, change)| format!(
                    "{{\"what\": \"{what}\", \"worth\": {worth}, \"change\": {change}}}"
                ))
                .collect()
        )
    )?;
    writeln!(
        report,
        "  \"graph\": {{\"one\": [{}, {}], \"late\": [{}, {}], \"all\": [{}, {}]}}",
        one.recalculated,
        one_took.as_secs_f64(),
        late.recalculated,
        late_took.as_secs_f64(),
        all.recalculated,
        all_took.as_secs_f64()
    )?;
    report.push_str("}\n");
    std::fs::write(dir.join("liabilities.json"), report)?;
    Ok(())
}
