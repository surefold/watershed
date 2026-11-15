//! Balancing a grid for a day. Every party on it, a generator or a supplier,
//! says ahead of time how much energy each of its metering points will put
//! in or take out. Each point is then read twice, by the party that owns it
//! and by the network operator. By the end of the day the two readings have
//! to agree, and each party has to hold enough flexible reserve to make up
//! the difference between what it said and what was metered.
//!
//!     cargo run --release -p watershed-gpu --example grid_balancing -- 1000000 400
//!
//! and with `--features cuda` the weather scenarios at the end run on a GPU.
//! The arguments are how many readings (a metering point in a half hour) and
//! how many parties.
//!
//! It is three questions asked of one model:
//!
//! - **Do the meters agree?** A reading is one row, with what the party's
//!   meter and the operator's meter each say. They arrive in batches from
//!   each. The state a reading is in is a column, and the counts by state
//!   are totals by group, kept as the readings land.
//! - **Is every party covered?** A party's imbalance is what was metered
//!   less what it nominated, added up over its points. What its reserve
//!   cannot make up is uncovered, and the operator has to find it.
//! - **What if the weather turns?** Wind moves what generators put in and
//!   cold moves what suppliers take out. The same model as a plan, over
//!   many days of weather, says how far out the system gets.
//!
//! Volumes are in MWh and there are no prices. Everything is made up: the
//! parties, the readings, the faults and the weather. It follows no real
//! market's rules.

use std::time::Instant;

use watershed_core::prelude::*;
use watershed_core::{Field, NodeId};
use watershed_gpu::Scenarios;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What is uncovered across the grid, how many parties are uncovered, how
/// far the whole system is out, and each party's uncovered volume.
type Cover = (Node<f64>, Node<f64>, Node<f64>, Vec<Node<f64>>);

// One in so many readings is given each kind of fault, by its number.
const NO_OPERATOR: usize = 331; // the party read its meter; the operator's reading never came
const NO_PARTY: usize = 499; // the operator read the point; the party's reading never came
const DISAGREE: usize = 211; // the two meters are well apart
const DRIFT: usize = 97; // the two meters differ a little: within their tolerance
const BATCHES: usize = 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Fault {
    None,
    NoOperator,
    NoParty,
    Disagree,
    Drift,
}

/// What is wrong with reading `i`, if anything: decided by its number, so
/// the counts expected at the end are known.
fn fault(i: usize) -> Fault {
    match i {
        i if i % NO_OPERATOR == 7 => Fault::NoOperator,
        i if i % NO_PARTY == 11 => Fault::NoParty,
        i if i % DISAGREE == 17 => Fault::Disagree,
        i if i % DRIFT == 19 => Fault::Drift,
        _ => Fault::None,
    }
}

fn party(n: usize) -> String {
    format!("party-{n:03}")
}

/// A generator puts energy in; a supplier takes it out. Every third party
/// generates.
fn generates(p: usize) -> bool {
    p % 3 == 0
}

/// Reading `i`: whose point it is, what was nominated for it in MWh (in is
/// positive, out is negative), and what actually flowed.
fn flow(i: usize, parties: usize) -> (usize, f64, f64) {
    let p = (i * 7 + i / parties) % parties;
    let size = 0.2 + ((i * 7919) % 4000) as f64 / 1000.0;
    let nominated = if generates(p) { 2.0 * size } else { -size };
    // Each party is a little out, some more than others and some the other way.
    let out = ((p * 53) % 21) as f64 / 10.0 - 1.0;
    (p, nominated, nominated * (1.0 + 0.012 * out))
}

/// What the operator's meter says for reading `i`, with its fault in it.
fn as_operator_reads(i: usize, parties: usize) -> f64 {
    let flowed = flow(i, parties).2;
    match fault(i) {
        Fault::Disagree => flowed * 0.9,
        Fault::Drift => flowed * 1.003,
        _ => flowed,
    }
}

struct Meters {
    party: Field<String>,
    nominated: Field<f64>,
    by_party: Field<f64>,
    party_read: Field<bool>,
    by_operator: Field<f64>,
    operator_read: Field<bool>,
}

/// One side's reading of a point arrives. A row needs every field when it
/// is first made, so the side not heard from yet is put in as not read.
fn reading(
    g: &mut Graph,
    readings: &mut Collection,
    f: &Meters,
    i: usize,
    parties: usize,
    from_party: bool,
) -> Result<()> {
    let key = format!("R{i:08}");
    let (p, nominated, flowed) = flow(i, parties);
    let operator = as_operator_reads(i, parties);
    let row = match (from_party, readings.contains(&key)) {
        (true, true) => vec![f.by_party.is(flowed), f.party_read.is(true)],
        (false, true) => vec![f.by_operator.is(operator), f.operator_read.is(true)],
        (from_party, false) => vec![
            f.party.is(party(p)),
            f.nominated.is(nominated),
            f.by_party.is(if from_party { flowed } else { 0.0 }),
            f.party_read.is(from_party),
            f.by_operator.is(if from_party { 0.0 } else { operator }),
            f.operator_read.is(!from_party),
        ],
    };
    readings.upsert(g, &key, &row)?;
    Ok(())
}

/// One node adding up many, eight at a time: a total a plan can take.
fn add_up(g: &mut Graph, name: &str, mut nodes: Vec<Node<f64>>) -> Result<Node<f64>> {
    let mut round = 0;
    while nodes.len() > 1 {
        round += 1;
        let mut next = Vec::with_capacity(nodes.len() / 8 + 1);
        for (k, group) in nodes.chunks(8).enumerate() {
            let mut all = group[0].into_expr();
            for &node in &group[1..] {
                all = all + node;
            }
            next.push(g.define(&format!("{name}_{round}_{k}"), all)?);
        }
        nodes = next;
    }
    Ok(nodes[0])
}

fn main() -> Result<()> {
    let number = |arg: Option<String>, otherwise: usize| match arg {
        Some(n) => n.replace('_', "").parse().expect("a number"),
        None => otherwise,
    };
    let mut args = std::env::args().skip(1);
    let count = number(args.next(), 1_000_000);
    let parties = number(args.next(), 400);
    assert!(parties >= 10, "at least ten parties");

    let mut g = Graph::new();
    // Two meters may differ by this share of the reading and still agree.
    let tolerance = g.input("meter_tolerance", 0.005)?;
    // The weather, against the forecast everyone nominated on: wind as a
    // share more or less than forecast, and how many degrees colder it is.
    let wind = g.input("wind", 0.0)?;
    let colder = g.input("colder", 0.0)?;

    let mut readings = Collection::new(&g, "readings");
    let f = Meters {
        party: readings.field::<String>("party")?,
        nominated: readings.field::<f64>("nominated_mwh")?,
        by_party: readings.field::<f64>("by_party_mwh")?,
        party_read: readings.field::<bool>("party_read")?,
        by_operator: readings.field::<f64>("by_operator_mwh")?,
        operator_read: readings.field::<bool>("operator_read")?,
    };
    let apart = readings.column(&mut g, "apart_mwh", f.by_operator - f.by_party)?;
    // The rules, in the order they are asked. A reading whose meters agree
    // exactly never reads the tolerance, so changing it does not touch it.
    let state = readings.column(
        &mut g,
        "state",
        when(!f.party_read)
            .then("no reading from the party".to_string())
            .otherwise(
                when(!f.operator_read)
                    .then("no reading from the operator".to_string())
                    .otherwise(
                        when(apart.equals(0.0))
                            .then("agreed".to_string())
                            .otherwise(
                                when(apart.abs().le(f.by_party.abs() * tolerance))
                                    .then("within tolerance".to_string())
                                    .otherwise("meters disagree".to_string()),
                            ),
                    ),
            ),
    )?;
    // What counts as having flowed: the operator's reading where there is
    // one, and the party's own until then.
    let metered = readings.column(
        &mut g,
        "metered_mwh",
        when(f.operator_read)
            .then(f.by_operator)
            .otherwise(f.by_party),
    )?;
    let out = readings.column(&mut g, "out_mwh", metered - f.nominated)?;
    let by_state = readings.group_count(&mut g, "readings_by_state", state)?;
    let apart_by_state = readings.group_sum(&mut g, "apart_by_state", state, apart)?;
    // Each party's imbalance: what was metered less what it nominated.
    let imbalance = readings.group_sum(&mut g, "imbalance_by_party", f.party, out)?;

    // What each party usually ends a day out by: the day below without its
    // faults, which is what its reserve was sized for.
    let mut usually = vec![0.0f64; parties];
    let mut volume = vec![0.0f64; parties];
    for i in 0..count {
        let (p, nominated, flowed) = flow(i, parties);
        usually[p] += flowed - nominated;
        volume[p] += nominated.abs();
    }

    // ---- the day: each side's readings arrive in batches, in turn
    let started = Instant::now();
    let per_batch = count.div_ceil(BATCHES);
    let mut slowest = 0.0f64;
    let mut cover: Option<Cover> = None;
    for batch in 0..BATCHES {
        for from_party in [true, false] {
            let began = Instant::now();
            let absent = match from_party {
                true => Fault::NoParty,
                false => Fault::NoOperator,
            };
            for i in (batch * per_batch..((batch + 1) * per_batch).min(count))
                .filter(|&i| fault(i) != absent)
            {
                reading(&mut g, &mut readings, &f, i, parties, from_party)?;
            }
            g.stabilize();
            slowest = slowest.max(began.elapsed().as_secs_f64());
        }
        // Once every party has been heard of, say what covering it means.
        // From here on each batch keeps every party's position up to date.
        let known = (0..parties).all(|p| readings.group(&g, imbalance, party(p)).is_some());
        if cover.is_none() && known {
            let mut positions = Vec::with_capacity(parties);
            let mut uncovered = Vec::with_capacity(parties);
            let mut is_uncovered = Vec::with_capacity(parties);
            for (p, (&usual, &traded)) in usually.iter().zip(&volume).enumerate() {
                let name = party(p);
                let metered_out = readings
                    .group(&g, imbalance, name.clone())
                    .expect("a party");
                // Made up: a reserve sized for what the party is usually out
                // by, some more amply than others, and how much the weather
                // moves it. Wind moves generators; cold moves suppliers.
                let spread = ((p * 37) % 100) as f64 / 100.0;
                let reserve = g.input(
                    &format!("{name}_reserve_mwh"),
                    usual.abs() * (0.9 + 0.4 * spread) + traded * 0.001,
                )?;
                let (windy, cold) = match generates(p) {
                    true => (traded * 0.04 * (0.5 + spread), 0.0),
                    false => (0.0, traded * 0.004 * (0.5 + spread)),
                };
                let position = g.define(
                    &format!("{name}_position_mwh"),
                    metered_out + windy * wind - cold * colder,
                )?;
                let lacks = g.define(
                    &format!("{name}_uncovered_mwh"),
                    (position.abs() - reserve).max(0.0),
                )?;
                is_uncovered.push(g.define(
                    &format!("{name}_is_uncovered"),
                    when(lacks.gt(0.0)).then(1.0).otherwise(0.0),
                )?);
                positions.push(position);
                uncovered.push(lacks);
            }
            let total = add_up(&mut g, "uncovered_mwh", uncovered.clone())?;
            let how_many = add_up(&mut g, "parties_uncovered", is_uncovered)?;
            // In is positive and out is negative, so every position added up
            // is how much more went in than came out.
            let system = add_up(&mut g, "system_mwh", positions)?;
            g.stabilize();
            cover = Some((total, how_many, system, uncovered));
        }
    }
    let (total_uncovered, parties_uncovered, system, uncovered) =
        cover.ok_or("some party was never heard of")?;
    println!(
        "{count} readings for {parties} parties, from both meters in {} batches: {:.1} s in all, {:.0} ms for the slowest batch; {} nodes",
        2 * BATCHES,
        started.elapsed().as_secs_f64(),
        slowest * 1e3,
        g.len()
    );

    // ---- the end of the day: both answers are already there to read
    let began = Instant::now();
    let report = |g: &Graph, readings: &Collection| -> Result<Vec<(String, i64, f64)>> {
        readings
            .group_keys(g, by_state)
            .into_iter()
            .map(|key| {
                let in_state = readings.group(g, by_state, key.clone()).expect("a group");
                let gap = readings
                    .group(g, apart_by_state, key.clone())
                    .expect("a group");
                Ok((key, g.get(in_state)?, g.get(gap)?))
            })
            .collect()
    };
    let rows = report(&g, &readings)?;
    let position = |g: &Graph| -> Result<(f64, f64, f64)> {
        Ok((
            g.get(parties_uncovered)?,
            g.get(total_uncovered)?,
            g.get(system)?,
        ))
    };
    let (how_many, lacking, net) = position(&g)?;
    println!(
        "read at the end of the day in {:.0} µs:",
        began.elapsed().as_secs_f64() * 1e6
    );
    for (key, in_state, gap) in &rows {
        println!("  {key:<30} {in_state:>9} readings   operator less party: {gap:>10.1} MWh");
    }
    println!(
        "  {how_many:.0} of {parties} parties are out by more than their reserve covers, {lacking:.0} MWh between them; the system is {} by {:.0} MWh",
        if net >= 0.0 { "long" } else { "short" },
        net.abs()
    );
    // Against what was put in.
    let expected = |wanted: Fault| (0..count).filter(|&i| fault(i) == wanted).count() as i64;
    let found = |rows: &[(String, i64, f64)], key: &str| {
        rows.iter().find(|r| r.0 == key).map_or(0, |r| r.1)
    };
    for (key, kind) in [
        ("no reading from the operator", Fault::NoOperator),
        ("no reading from the party", Fault::NoParty),
        ("meters disagree", Fault::Disagree),
        ("within tolerance", Fault::Drift),
        ("agreed", Fault::None),
    ] {
        assert_eq!(found(&rows, key), expected(kind), "{key}");
    }
    println!("  checked: each count is the number of readings given that fault");

    // Why the party that is furthest out is uncovered.
    let worst = (0..parties)
        .max_by(|&a, &b| {
            let amount = |p: usize| g.get(uncovered[p]).unwrap_or(0.0);
            amount(a).total_cmp(&amount(b))
        })
        .expect("a party");
    println!("why {} is uncovered (the first lines of it):", party(worst));
    for line in g.explain(uncovered[worst]).lines().take(6) {
        println!("{}", line.chars().take(150).collect::<String>());
    }
    println!("…");

    // ---- after the end of the day: the operator reads a meter again, one at a time
    let wrong: Vec<usize> = (0..count)
        .filter(|&i| fault(i) == Fault::Disagree)
        .take(1_000)
        .collect();
    let began = Instant::now();
    let mut recalculated = 0;
    for &i in &wrong {
        let key = format!("R{i:08}");
        readings.upsert(&mut g, &key, &[f.by_operator.is(flow(i, parties).2)])?;
        recalculated += g.stabilize().recalculated;
    }
    println!(
        "{} meters read again, one at a time: {:.1} µs each, {:.1} nodes recalculated each: the reading, its party's imbalance and cover, and the system's",
        wrong.len(),
        began.elapsed().as_secs_f64() * 1e6 / wrong.len() as f64,
        recalculated as f64 / wrong.len() as f64
    );
    assert_eq!(
        found(&report(&g, &readings)?, "meters disagree"),
        found(&rows, "meters disagree") - wrong.len() as i64
    );
    let (how_many, lacking, net) = position(&g)?;
    println!(
        "  now {how_many:.0} parties uncovered, {lacking:.0} MWh between them; the system is {} by {:.0} MWh",
        if net >= 0.0 { "long" } else { "short" },
        net.abs()
    );

    // ---- a rule changes: looser meters, and only readings that differ are asked again
    let began = Instant::now();
    g.set(tolerance, 0.2)?;
    let loosened = g.stabilize();
    println!(
        "meter tolerance loosened from 0.5% to 20%: {:.0} ms, {} of {} nodes recalculated",
        began.elapsed().as_secs_f64() * 1e3,
        loosened.recalculated,
        g.len()
    );
    assert_eq!(found(&report(&g, &readings)?, "meters disagree"), 0);
    g.set(tolerance, 0.005)?;
    g.stabilize();
    g.check_consistency().map_err(CalcError::Invalid)?;
    println!("checked: every total matches adding the rows up from scratch");

    // ---- what if the weather turns: the same model as a plan, over many days
    let scenarios = 200_000;
    let outputs: [NodeId; 3] = [total_uncovered.id(), parties_uncovered.id(), system.id()];
    let began = Instant::now();
    let plan = Scenarios::new(&g, &[wind.id(), colder.id()], &outputs)?;
    let planned = began.elapsed();
    // Wind anywhere from 60% under forecast to 60% over, and up to six
    // degrees colder than forecast.
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut unit = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    let weather: Vec<f64> = (0..scenarios)
        .flat_map(|_| [1.2 * unit() - 0.6, 6.0 * unit() * unit()])
        .collect();
    let began = Instant::now();
    #[cfg(feature = "gpu")]
    let (results, on) = (watershed_gpu::Gpu::new()?.run(&plan, &weather)?, "a GPU");
    #[cfg(not(feature = "gpu"))]
    let (results, on) = (plan.run_cpu(&weather), "CPU threads");
    let ran = began.elapsed();
    let column = |k: usize| -> Vec<f64> { results.iter().skip(k).step_by(3).copied().collect() };
    let mut lacking = column(0);
    lacking.sort_by(f64::total_cmp);
    let mut out_by: Vec<f64> = column(2).iter().map(|x| x.abs()).collect();
    out_by.sort_by(f64::total_cmp);
    let at = |sorted: &[f64], share: f64| {
        sorted[((scenarios as f64 * share) as usize).min(scenarios - 1)]
    };
    println!(
        "{scenarios} days of weather on {on}: plan of {} calculations made in {:.0} ms, run in {:.0} ms",
        plan.nodes(),
        planned.as_secs_f64() * 1e3,
        ran.as_secs_f64() * 1e3
    );
    println!(
        "  uncovered between the parties: {:.0} MWh on a middling day, {:.0} on one day in ten, {:.0} on one in a hundred",
        at(&lacking, 0.5),
        at(&lacking, 0.9),
        at(&lacking, 0.99)
    );
    println!(
        "  the system is out by {:.0} MWh on a middling day, {:.0} on one day in ten, {:.0} on one in a hundred: what the operator has to be able to call on",
        at(&out_by, 0.5),
        at(&out_by, 0.9),
        at(&out_by, 0.99)
    );
    println!(
        "  parties uncovered: {:.0} on average, {:.0} at the worst",
        column(1).iter().sum::<f64>() / scenarios as f64,
        column(1).iter().copied().fold(0.0, f64::max)
    );
    // One scenario is what the graph says with that weather set on it.
    g.set(wind, weather[0])?;
    g.set(colder, weather[1])?;
    g.stabilize();
    for (node, from_plan) in [total_uncovered, parties_uncovered, system]
        .iter()
        .zip(&results)
    {
        assert_eq!(g.get(*node)?.to_bits(), from_plan.to_bits());
    }
    println!(
        "  checked: the first scenario is what the graph gives with that weather set on it, to the last bit"
    );

    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find(|l| l.starts_with("VmHWM:")) {
            println!("peak memory: {}", line.trim_start_matches("VmHWM:").trim());
        }
    }
    Ok(())
}
