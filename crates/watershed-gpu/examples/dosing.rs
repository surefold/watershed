//! What dose of an antibiotic gets most patients into the range that works
//! without harming them?
//!
//! Vancomycin is a last-line antibiotic with a narrow margin. Too little
//! over a day and the infection is not cleared; too much and it damages the
//! kidneys, which are also what remove it, so a patient with poor kidneys
//! keeps it for much longer. Hospitals dose it by a protocol: so many
//! milligrams for each kilogram of weight, given more or less often
//! according to kidney function. The aim, from the 2020 consensus guideline
//! (Rybak and others, American Journal of Health-System Pharmacy 77:835,
//! doi 10.1093/ajhp/zxaa036), is an exposure (the area under the curve of
//! concentration against time, over a day) of 400 to 600 mg·h/L.
//!
//! Whether a protocol meets that aim is found by simulation: make up
//! thousands of patients as varied as real ones, dose each by the protocol,
//! follow the drug through each body, and count how many land in the range.
//! This kind of study is called probability of target attainment.
//!
//! The graph is one patient for three days, an hour at a time: kidney
//! function from age, weight, sex and a blood test; how fast this patient
//! clears the drug and how it spreads between blood and tissue; the
//! protocol's doses and when they fall; the amount in each of two
//! compartments hour by hour; and the exposure over the first day and the
//! third. A scenario is one patient. The protocol's numbers are shared
//! inputs, so trying another protocol on every patient is one more run, and
//! the patients stay on the GPU with only the counts coming back.
//!
//! First make the reference, which follows a few hundred patients with an
//! independent solver:
//!
//!     pip install numpy scipy
//!     mkdir out && python examples/shared/dosing_reference.py out
//!
//! then
//!
//!     cargo run --release --features cuda --example dosing -- out
//!
//! and optionally how many patients (5 million).
//!
//! The model is the two-compartment one of Goti and others (Therapeutic Drug
//! Monitoring 2018;40:212-221) for adults not on dialysis, with its numbers
//! as the author of this example recalls them; they have not been checked
//! against the paper. The patients are made up. This is an illustration of
//! the method and not advice on dosing anyone.

use std::path::Path;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use watershed_core::prelude::*;
use watershed_core::{IntoExpr, NodeId, Value, when};
use watershed_gpu::Scenarios;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const HOURS: usize = 72;

// The model. `dosing_reference.py` has the same numbers.
/// Litres an hour cleared by a typical patient with kidney function of 120.
const CLEARANCE: f64 = 4.5;
const KIDNEY_USUAL: f64 = 120.0;
const KIDNEY_POWER: f64 = 0.8;
/// Kidney function above this is counted as this.
const KIDNEY_MOST: f64 = 150.0;
/// Litres of blood and well-supplied tissue, for 70 kg; of the rest; and
/// litres an hour passing between them.
const CENTRAL: f64 = 58.4;
const TISSUE: f64 = 38.4;
const BETWEEN: f64 = 6.5;
/// How much patients differ from typical in clearance and the two volumes,
/// as a share.
const SPREAD: [f64; 3] = [0.398, 0.816, 0.571];

/// What is known about a patient: one scenario's inputs, in this order.
const FACTS: usize = 7;
const AGE: usize = 0;
const WEIGHT: usize = 1;
const FEMALE: usize = 2;
/// Creatinine in the blood, mg/dL: the more, the worse the kidneys.
const CREATININE: usize = 3;
// Then how far this patient is from typical in clearance and each volume.

/// The hours between doses, best kidneys first.
const EVERY: [usize; 4] = [8, 12, 24, 48];
const BANDS: [&str; 4] = [
    "good kidneys (every 8 h)",
    "mild loss (every 12 h)",
    "moderate loss (every 24 h)",
    "severe loss (every 48 h)",
];

/// A dosing protocol. The same for every patient of a run.
#[derive(Clone)]
struct Protocol {
    /// The first dose, in mg for each kg, and the most it can be.
    load: f64,
    load_most: f64,
    /// Later doses, in mg for each kg, for each band of kidney function,
    /// and the most one can be.
    dose: [f64; 4],
    dose_most: f64,
    /// Kidney function at least this is dosed every 8, 12 and 24 hours;
    /// below the last, every 48.
    at_least: [f64; 3],
    /// A dose above this is given over two hours, not one.
    long_over: f64,
    /// The exposure over a day that is aimed for.
    aim: [f64; 2],
}

impl Protocol {
    fn usual() -> Protocol {
        Protocol {
            load: 25.0,
            load_most: 3000.0,
            dose: [15.0; 4],
            dose_most: 2000.0,
            at_least: [90.0, 50.0, 20.0],
            long_over: 1000.0,
            aim: [400.0, 600.0],
        }
    }

    fn values(&self) -> Vec<f64> {
        let mut values = vec![self.load, self.load_most];
        values.extend(self.dose);
        values.push(self.dose_most);
        values.extend(self.at_least);
        values.push(self.long_over);
        values.extend(self.aim);
        values
    }
}

/// One patient as a graph.
struct Patient {
    g: Graph,
    facts: Vec<Input<f64>>,
    protocol: Vec<Input<f64>>,
    /// The exposure over the first day and the third, and the concentration
    /// at the end.
    detail: Vec<NodeId>,
    /// For each band: whether the patient is in it, and in it with the
    /// third day's exposure in the aimed-for range, or above it. Then
    /// whether the first day's exposure reached the range. Each is 1 or 0,
    /// so added up over patients they are counts.
    counts: Vec<NodeId>,
}

impl Patient {
    fn new(protocol: &Protocol) -> Result<Patient> {
        let mut g = Graph::new();
        let names = [
            "age",
            "weight",
            "female",
            "creatinine",
            "eta_cl",
            "eta_v1",
            "eta_v2",
        ];
        // A woman of 60 and 70 kg with a normal blood test, typical in the rest.
        let typical = [60.0, 70.0, 1.0, 0.8, 0.0, 0.0, 0.0];
        let facts: Vec<Input<f64>> = names
            .iter()
            .zip(typical)
            .map(|(name, x)| g.input(name, x))
            .collect::<std::result::Result<_, _>>()?;
        let [age, weight, female, creatinine, eta_cl, eta_v1, eta_v2] = facts[..] else {
            unreachable!()
        };
        let knobs: Vec<Input<f64>> = protocol
            .values()
            .iter()
            .enumerate()
            .map(|(k, &x)| g.input(&format!("protocol{k}"), x))
            .collect::<std::result::Result<_, _>>()?;
        let [
            load,
            load_most,
            dose8,
            dose12,
            dose24,
            dose48,
            dose_most,
            at_least8,
            at_least12,
            at_least24,
            long_over,
            aim_low,
            aim_high,
        ] = knobs[..]
        else {
            unreachable!()
        };

        // Kidney function (Cockcroft and Gault's estimate), and from it and
        // the patient's own differences, how the drug moves.
        let kidney = g.define(
            "kidney",
            ((140.0 - age) * weight / (72.0 * creatinine)
                * when(female.gt(0.5)).then(0.85).otherwise(1.0))
            .min(KIDNEY_MOST),
        )?;
        let clearance = g.define(
            "clearance",
            CLEARANCE * (kidney / KIDNEY_USUAL).pow(KIDNEY_POWER) * eta_cl.exp(),
        )?;
        let central = g.define("central", CENTRAL * (weight / 70.0) * eta_v1.exp())?;
        let tissue = g.define("tissue", TISSUE * eta_v2.exp())?;

        // The two compartments exchange and the central one empties, each in
        // proportion to what it holds. Over an hour with a steady drip that
        // has an exact answer: what is there afterwards is a fixed mix of
        // what was there before and of the drip, and the mix depends only on
        // the patient. So it is worked out once for the patient.
        let out = g.define("rate_out", clearance / central)?;
        let to_tissue = g.define("rate_to_tissue", BETWEEN / central)?;
        let back = g.define("rate_back", BETWEEN / tissue)?;
        let sum = g.define("rate_sum", out + to_tissue + back)?;
        let root = g.define("rate_root", (sum * sum - 4.0 * out * back).sqrt())?;
        let fast = g.define("rate_fast", (sum + root) / 2.0)?;
        let slow = g.define("rate_slow", (sum - root) / 2.0)?;
        let after_fast = g.define("after_fast", (-fast).exp())?;
        let after_slow = g.define("after_slow", (-slow).exp())?;
        let gap = g.define("rate_gap", slow - fast)?;
        let leaving = g.define("rate_leaving", out + to_tissue)?;
        let keep11 = g.define(
            "keep11",
            ((slow - leaving) * after_fast - (fast - leaving) * after_slow) / gap,
        )?;
        let keep12 = g.define("keep12", back * (after_fast - after_slow) / gap)?;
        let keep21 = g.define("keep21", to_tissue * (after_fast - after_slow) / gap)?;
        let keep22 = g.define(
            "keep22",
            ((slow - back) * after_fast - (fast - back) * after_slow) / gap,
        )?;
        let drip1 = g.define("from_drip1", -((keep11 - 1.0) + keep21) / out)?;
        let drip2 = g.define(
            "from_drip2",
            (-to_tissue * (keep11 - 1.0) - leaving * keep21) / (out * back),
        )?;

        // The protocol for this patient: which band, so how often and how
        // much; and a big dose is given over two hours.
        let is8 = g.define("is8", kidney.ge(at_least8))?;
        let is12 = g.define("is12", kidney.ge(at_least12) & !is8)?;
        let is24 = g.define("is24", kidney.ge(at_least24) & !kidney.ge(at_least12))?;
        let is48 = g.define("is48", kidney.lt(at_least24))?;
        let mut band = Vec::new();
        for (k, is) in [is8, is12, is24, is48].into_iter().enumerate() {
            band.push(g.define(&format!("band{k}"), when(is).then(1.0).otherwise(0.0))?);
        }
        let for_each_kg = band[0] * dose8 + band[1] * dose12 + band[2] * dose24 + band[3] * dose48;
        let dose = g.define("dose", (for_each_kg * weight).min(dose_most))?;
        let first = g.define("first_dose", (load * weight).min(load_most))?;
        // What goes in during a dose's first hour, and its second.
        let mut hours = |name: &str, dose: Node<f64>| -> Result<[Node<f64>; 2]> {
            let long = dose.gt(long_over);
            Ok([
                g.define(
                    &format!("{name}_hour1"),
                    when(long.clone()).then(dose / 2.0).otherwise(dose),
                )?,
                g.define(
                    &format!("{name}_hour2"),
                    when(long).then(dose / 2.0).otherwise(0.0),
                )?,
            ])
        };
        let first = hours("first", first)?;
        let later = hours("later", dose)?;

        // Hour by hour.
        let mut blood: Expr<f64> = 0.0.into_expr();
        let mut rest: Expr<f64> = 0.0.into_expr();
        let mut given: Expr<f64> = 0.0.into_expr();
        // What has left the body by the end of each day: all that was given
        // less what is still there.
        let mut gone = Vec::new();
        // Whether a later dose starts in the hour: the bands whose turn it is.
        let starts = |t: usize| -> Option<Expr<f64>> {
            let mut all: Option<Expr<f64>> = None;
            for (k, every) in EVERY.into_iter().enumerate() {
                if t > 0 && t % every == 0 {
                    all = Some(match all {
                        Some(all) => all + band[k],
                        None => band[k].into_expr(),
                    });
                }
            }
            all
        };
        for t in 0..HOURS {
            let mut drip: Option<Expr<f64>> = match t {
                0 => Some(first[0].into_expr()),
                1 => Some(first[1].into_expr()),
                _ => None,
            };
            let parts = [
                starts(t).map(|s| s * later[0]),
                t.checked_sub(1).and_then(starts).map(|s| s * later[1]),
            ];
            for part in parts.into_iter().flatten() {
                drip = Some(match drip {
                    Some(drip) => drip + part,
                    None => part,
                });
            }
            let (next_blood, next_rest) = match drip {
                Some(drip) => {
                    let drip = g.define(&format!("drip{t}"), drip)?;
                    given = g.define(&format!("given{t}"), given + drip)?.into_expr();
                    (
                        keep11 * blood.clone() + keep12 * rest.clone() + drip1 * drip,
                        keep21 * blood + keep22 * rest + drip2 * drip,
                    )
                }
                None => (
                    keep11 * blood.clone() + keep12 * rest.clone(),
                    keep21 * blood + keep22 * rest,
                ),
            };
            let next_blood = g.define(&format!("blood{}", t + 1), next_blood)?;
            let next_rest = g.define(&format!("rest{}", t + 1), next_rest)?;
            (blood, rest) = (next_blood.into_expr(), next_rest.into_expr());
            if (t + 1) % 24 == 0 {
                gone.push(g.define(
                    &format!("gone{}", t + 1),
                    given.clone() - next_blood - next_rest,
                )?);
            }
        }
        // The exposure over a stretch is what left in it, over the clearance.
        let day1 = g.define("exposure_day1", gone[0] / clearance)?;
        let day3 = g.define("exposure_day3", (gone[2] - gone[1]) / clearance)?;
        let at_end = g.define("at_72h", blood / central)?;

        let mut counts = Vec::new();
        let within = g.define("within", day3.ge(aim_low) & day3.le(aim_high))?;
        let above = g.define("above", day3.gt(aim_high))?;
        for (k, &in_band) in band.iter().enumerate() {
            counts.push(in_band.id());
            counts.push(
                g.define(
                    &format!("within{k}"),
                    when(within).then(in_band).otherwise(0.0),
                )?
                .id(),
            );
            counts.push(
                g.define(
                    &format!("above{k}"),
                    when(above).then(in_band).otherwise(0.0),
                )?
                .id(),
            );
        }
        counts.push(
            g.define("early", when(day1.ge(aim_low)).then(1.0).otherwise(0.0))?
                .id(),
        );
        g.stabilize();
        Ok(Patient {
            g,
            facts,
            protocol: knobs,
            detail: vec![day1.id(), day3.id(), at_end.id()],
            counts,
        })
    }

    fn plan(&self, outputs: &[NodeId]) -> Result<Scenarios> {
        let vary: Vec<NodeId> = self.facts.iter().map(|f| f.id()).collect();
        let shared: Vec<NodeId> = self.protocol.iter().map(|p| p.id()).collect();
        Ok(Scenarios::with_shared(&self.g, &vary, &shared, outputs)?)
    }
}

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }
    fn normal(&mut self) -> f64 {
        (-2.0 * self.unit().ln()).sqrt() * (std::f64::consts::TAU * self.unit()).cos()
    }
}

/// Makes up patient number `k`: adults of every age, a quarter of them with
/// kidneys that are not working well.
fn patient(k: usize, f: &mut [f64]) {
    let mut rng = Rng((k as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    for _ in 0..4 {
        rng.next();
    }
    f[AGE] = (18.0 + 73.0 * rng.unit()).floor();
    f[WEIGHT] = (78.0 * (0.22 * rng.normal()).exp())
        .clamp(40.0, 180.0)
        .round();
    f[FEMALE] = f64::from(rng.unit() < 0.45);
    f[CREATININE] = (0.9 * (0.45 * rng.normal()).exp()).clamp(0.4, 6.0);
    for (eta, spread) in f[4..].iter_mut().zip(SPREAD) {
        *eta = rng.normal() * (1.0 + spread * spread).ln().sqrt();
    }
}

/// For each band: how many patients, how many within the aim and how many
/// above it. Then how many reached the aim on the first day.
struct Counts(Vec<f64>);

impl Counts {
    fn all(&self) -> f64 {
        (0..4).map(|k| self.0[3 * k]).sum()
    }
    fn within(&self) -> f64 {
        (0..4).map(|k| self.0[3 * k + 1]).sum()
    }
    fn above(&self) -> f64 {
        (0..4).map(|k| self.0[3 * k + 2]).sum()
    }
    fn show(&self, what: &str) {
        let all = self.all();
        println!(
            "  {what:<34} {:>6.1}% within, {:>5.1}% above, {:>5.1}% below; {:.1}% reach it on the first day",
            100.0 * self.within() / all,
            100.0 * self.above() / all,
            100.0 * (all - self.within() - self.above()) / all,
            100.0 * self.0[12] / all
        );
    }
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .ok_or("give the folder dosing_reference.py wrote to")?;
    let n: usize = match args.next() {
        Some(n) => n.replace('_', "").parse()?,
        None if cfg!(feature = "cuda") => 5_000_000,
        None => 500_000,
    };
    let usual = Protocol::usual();
    let mut one = Patient::new(&usual)?;
    let detail = one.plan(&one.detail)?;
    let mut counting = one.plan(&one.counts)?;
    println!(
        "one patient for three days: {} formulas in {} levels, of {} shapes; {} numbers in the protocol",
        counting.nodes(),
        counting.levels(),
        counting.shapes(),
        counting.shared_inputs()
    );
    println!();

    // ---- 1. Is it right? ----
    println!("1. Against an independent solver (SciPy's LSODA)");
    println!();
    let text = std::fs::read_to_string(Path::new(&dir).join("dosing_reference.csv"))?;
    let rows: Vec<Vec<f64>> = text
        .lines()
        .skip(1)
        .map(|line| {
            line.split(',')
                .map(|x| x.parse().unwrap_or(f64::NAN))
                .collect()
        })
        .collect();
    let inputs: Vec<f64> = rows.iter().flat_map(|r| r[..FACTS].to_vec()).collect();
    let ours = detail.run_cpu(&inputs);
    let mut worst = 0.0f64;
    for (row, ours) in rows.iter().zip(ours.chunks(3)) {
        for (theirs, ours) in row[FACTS..].iter().zip(ours) {
            worst = worst.max((ours - theirs).abs() / theirs.abs().max(1.0));
        }
    }
    println!(
        "  {} patients, three answers each (exposure on day 1 and day 3, concentration at 72 h): largest difference {worst:.1e}",
        rows.len()
    );
    assert!(worst < 1e-7, "the graph and the solver differ");
    println!();

    // ---- 2. A population ----
    println!("2. A population");
    println!();
    let started = Instant::now();
    let mut patients = vec![0.0; n * FACTS];
    patients
        .par_chunks_mut(FACTS)
        .enumerate()
        .for_each(|(k, f)| patient(k, f));
    println!(
        "  {} patients made up in {}; each is {} hours, so {} patient-hours a run",
        commas(n as f64),
        time(started.elapsed()),
        HOURS,
        commas((n * HOURS) as f64)
    );
    let cores = std::thread::available_parallelism().map_or(1, |c| c.get());
    #[cfg(feature = "cuda")]
    let gpu = watershed_gpu::Gpu::new()?;
    #[cfg(feature = "cuda")]
    let mut loaded = {
        let started = Instant::now();
        let loaded = gpu.load(&counting, &patients)?;
        println!("  loaded onto the GPU in {}", time(started.elapsed()));
        loaded
    };
    // Each band's counts under a protocol.
    #[cfg_attr(not(feature = "cuda"), allow(unused_mut))]
    let mut count = |plan: &Scenarios| -> Result<Counts> {
        #[cfg(feature = "cuda")]
        let totals = loaded.totals(plan)?;
        #[cfg(not(feature = "cuda"))]
        let totals = plan.totals_cpu(&patients);
        Ok(Counts(totals))
    };
    let started = Instant::now();
    let now = count(&counting)?;
    let first_took = started.elapsed();
    let started = Instant::now();
    let again = count(&counting)?;
    let took = started.elapsed();
    assert_eq!(now.0, again.0);
    println!(
        "  {}: every patient under the usual protocol in {} the first time, {} after ({} patient-hours a second)",
        if cfg!(feature = "cuda") { "GPU" } else { "CPU" },
        time(first_took),
        time(took),
        commas((n * HOURS) as f64 / took.as_secs_f64())
    );
    if cfg!(feature = "cuda") {
        let some = n.min(1_000_000);
        let started = Instant::now();
        let on_cpu = counting.totals_cpu(&patients[..some * FACTS]);
        let took = started.elapsed();
        println!(
            "  the plan on {cores} CPU cores: {} patients a second, so {} for all of them",
            commas(some as f64 / took.as_secs_f64()),
            time(took.mul_f64(n as f64 / some as f64))
        );
        // The same counts when it is the same patients.
        if some == n {
            assert_eq!(on_cpu, now.0, "the CPU and the GPU count differently");
        }
    }
    // The graph itself, a patient at a time.
    let few = n.min(20_000);
    let started = Instant::now();
    let by_plan = detail.run_cpu(&patients[..few * FACTS]);
    for (f, want) in patients.chunks(FACTS).zip(by_plan.chunks(3)).take(few) {
        for (&fact, &x) in one.facts.iter().zip(f) {
            one.g.set(fact, x)?;
        }
        one.g.stabilize();
        for (&id, &want) in one.detail.iter().zip(want) {
            let Value::F64(here) = one.g.value(id) else {
                unreachable!("an exposure is a number")
            };
            assert_eq!(
                here.to_bits(),
                want.to_bits(),
                "the graph and the plan differ"
            );
        }
    }
    let each = started.elapsed() / few as u32;
    println!(
        "  the graph, a patient at a time: {} each, so {} for all of them",
        time(each),
        time(each * n as u32)
    );
    println!();

    // ---- 3. The usual protocol ----
    println!(
        "3. The usual protocol: {} mg/kg to start, then {} mg/kg, aiming for {} to {} mg·h/L a day on the third day",
        usual.load, usual.dose[0], usual.aim[0], usual.aim[1]
    );
    println!();
    now.show("all patients");
    for (k, band) in BANDS.iter().enumerate() {
        let all = now.0[3 * k];
        println!(
            "    {band:<32} {:>6.1}% within, {:>5.1}% above   ({:.0}% of patients)",
            100.0 * now.0[3 * k + 1] / all,
            100.0 * now.0[3 * k + 2] / all,
            100.0 * all / now.all()
        );
    }
    println!();

    // ---- 4. Other protocols ----
    // Every dose from 5 to 30 mg/kg, on every patient. A band's patients
    // are only affected by that band's dose, so one run for each dose says
    // how every band does at it.
    println!("4. Every dose from 5 to 30 mg/kg, each on every patient");
    println!();
    let doses: Vec<f64> = (10..=60).map(|d| d as f64 / 2.0).collect();
    let started = Instant::now();
    let mut tried = Vec::new();
    for &dose in &doses {
        let mut protocol = usual.clone();
        protocol.dose = [dose; 4];
        counting.set_shared(&protocol.values());
        tried.push(count(&counting)?);
    }
    let took = started.elapsed();
    println!(
        "  {} protocols on {} patients in {} ({} each)",
        doses.len(),
        commas(n as f64),
        time(took),
        time(took / doses.len() as u32)
    );
    // For each band, the dose that puts most of its patients within the aim.
    let mut best = usual.clone();
    println!("  the dose that puts most of each band within the aim:");
    for (k, band) in BANDS.iter().enumerate() {
        let at = (0..doses.len())
            .max_by(|&a, &b| tried[a].0[3 * k + 1].total_cmp(&tried[b].0[3 * k + 1]))
            .expect("some doses");
        best.dose[k] = doses[at];
        let all = tried[at].0[3 * k];
        println!(
            "    {band:<32} {:>5.1} mg/kg: {:>5.1}% within, {:>5.1}% above   (at 15 mg/kg: {:.1}% within, {:.1}% above)",
            doses[at],
            100.0 * tried[at].0[3 * k + 1] / all,
            100.0 * tried[at].0[3 * k + 2] / all,
            100.0 * now.0[3 * k + 1] / all,
            100.0 * now.0[3 * k + 2] / all
        );
    }
    counting.set_shared(&best.values());
    let better = count(&counting)?;
    println!();
    now.show("the usual protocol");
    better.show("a dose for each band");
    // A bigger first dose, for getting there on the first day.
    let mut bigger = best.clone();
    bigger.load = 35.0;
    counting.set_shared(&bigger.values());
    count(&counting)?.show("and 35 mg/kg to start");
    counting.set_shared(&usual.values());
    println!();
    println!(
        "  Even the best dose for a band leaves many patients outside the aim: people differ too much for"
    );
    println!(
        "  weight and kidney function to say what dose each needs, which is why blood levels are measured."
    );
    println!();

    // ---- 5. One thing changes ----
    println!("5. On the graph itself, when one thing changes");
    println!();
    let nodes = counting.nodes();
    let started = Instant::now();
    one.g.set(one.facts[CREATININE], 2.4)?;
    let report = one.g.stabilize();
    println!(
        "  a patient's blood test comes back worse: {} of {} formulas worked out again, in {}",
        report.recalculated,
        nodes,
        time(started.elapsed())
    );
    Ok(())
}

/// An amount with thousands separated: `-1,234,567`.
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
        format!("{:.0} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.0} ms", s * 1e3)
    } else if s < 120.0 {
        format!("{s:.1} s")
    } else {
        format!("{:.0} minutes", s / 60.0)
    }
}
