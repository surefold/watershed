//! One run of many market paths, cut into lots that separate processes work
//! out, to see whether a batch can be spread over several machines.
//!
//! EXPERIMENTAL. The plan is made once and written to a file. Each worker
//! is another process that knows nothing of the graph: it reads the plan
//! and its lot of paths from files, runs them, and writes its answers to a
//! file. The answers are put back in order and checked, to the last bit,
//! against running every path in one go.
//!
//! The workers here share one machine, and one GPU if there is one, so this
//! shows that the answers are the same and what cutting the run up costs,
//! not that it is faster.
//!
//!     cargo run --release --features cuda --example split -- export [kinds] [paths] [workers] [of them on the CPU]

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[path = "shared/savings.rs"]
mod savings;
use savings::*;
use watershed_core::NodeId;
use watershed_gpu::Scenarios;

fn write(path: &Path, values: &[f64]) -> Result<()> {
    let mut file = BufWriter::new(File::create(path)?);
    for v in values {
        file.write_all(&v.to_le_bytes())?;
    }
    Ok(file.flush()?)
}

/// `count` numbers of the file, from the `first`.
fn read(path: &Path, first: usize, count: usize) -> Result<Vec<f64>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(first as u64 * 8))?;
    let mut bytes = vec![0; count * 8];
    file.read_exact(&mut bytes)?;
    Ok(bytes
        .chunks_exact(8)
        .map(|b| f64::from_le_bytes(b.try_into().expect("eight bytes")))
        .collect())
}

/// A worker: `lot <plan> <inputs> <first path> <paths> <answers> <gpu|cpu> <bytes of the GPU it may use>`.
fn lot(mut args: impl Iterator<Item = String>) -> Result<()> {
    let mut next = || args.next().ok_or("a worker was started with too little");
    let (plan, inputs, first, count, answers, on, allowed) = (
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
        next()?,
    );
    let (first, count, allowed): (usize, usize, usize) =
        (first.parse()?, count.parse()?, allowed.parse()?);

    let started = Instant::now();
    let plan = Scenarios::load(BufReader::new(File::open(plan)?))?;
    let inputs = read(
        Path::new(&inputs),
        first * plan.inputs(),
        count * plan.inputs(),
    )?;
    let loaded = started.elapsed();

    let started = Instant::now();
    let results = match on.as_str() {
        #[cfg(feature = "gpu")]
        "gpu" => {
            watershed_gpu::Gpu::new()?
                .run_within(&plan, &inputs, allowed)?
                .0
        }
        _ => plan.run_cpu(&inputs),
    };
    let ran = started.elapsed();
    let _ = allowed;
    write(Path::new(&answers), &results)?;
    // What the one that started it reads: seconds to load, seconds to run.
    println!("{} {}", loaded.as_secs_f64(), ran.as_secs_f64());
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .ok_or("give the folder the reference wrote to")?;
    if dir == "lot" {
        return lot(args);
    }
    let dir = Path::new(&dir);
    let kinds: usize = args.next().map_or(Ok(20_000), |a| a.parse())?;
    let scenarios: usize = args.next().map_or(Ok(10_000), |a| a.parse())?;
    let workers: usize = args.next().map_or(Ok(4), |a| a.parse())?;
    let on_cpu: usize = args.next().map_or(Ok(0), |a| a.parse())?;
    let (workers, on_cpu) = (workers.clamp(1, scenarios), on_cpu.min(workers));

    let tables = tables(dir)?;
    let policies = policies(&dir.join("points_book.csv"), kinds)?;
    let kinds = policies.len();
    let best = Assumptions::best_estimate(&tables, 0.0);

    let started = Instant::now();
    let mut book = Book::new(&policies, &tables, &best)?;
    let totals: Vec<NodeId> = book.totals()?.iter().map(|t| t.id()).collect();
    let built = started.elapsed();
    let started = Instant::now();
    let plan = book.plan(&totals)?;
    println!(
        "{} kinds of policy, {} market paths; graph made in {}, plan of {} nodes in {}",
        commas(kinds as f64),
        commas(scenarios as f64),
        time(built),
        commas(plan.nodes() as f64),
        time(started.elapsed())
    );

    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let (drift, spread) = (
        (GROWTH - 0.5 * SWING * SWING) / 12.0,
        SWING * (1.0f64 / 12.0).sqrt(),
    );
    let inputs: Vec<f64> = (0..scenarios * MONTHS)
        .map(|_| (drift + spread * rng.normal()).exp() - 1.0)
        .collect();

    // Every path in one go, in this process.
    let runner = Runner::new()?;
    let started = Instant::now();
    let whole = runner.run(&plan, &inputs)?;
    let whole_took = started.elapsed();
    println!("in one go, on the {}: {}", runner.name(), time(whole_took));
    #[cfg(feature = "gpu")]
    let allowed = runner.gpu.free_memory()? / 5 * 4 / (workers - on_cpu).max(1);
    #[cfg(not(feature = "gpu"))]
    let allowed = 0usize;
    // The workers have the GPU to themselves.
    #[cfg(feature = "gpu")]
    drop(runner);

    // What a worker is given: the plan and the paths, as files.
    let folder: PathBuf =
        std::env::temp_dir().join(format!("watershed-split-{}", std::process::id()));
    std::fs::create_dir_all(&folder)?;
    let (plan_at, inputs_at) = (folder.join("plan"), folder.join("paths"));
    let started = Instant::now();
    plan.save(BufWriter::new(File::create(&plan_at)?))?;
    let saved = started.elapsed();
    write(&inputs_at, &inputs)?;
    println!(
        "the plan written out: {:.0} MB in {}; the paths: {:.0} MB",
        std::fs::metadata(&plan_at)?.len() as f64 / 1e6,
        time(saved),
        std::fs::metadata(&inputs_at)?.len() as f64 / 1e6
    );
    println!();
    // Set WATERSHED_KEEP to a folder to have the plan, the paths and the
    // answers of the one run written there for another program to use,
    // and stop.
    if let Some(keep) = std::env::var_os("WATERSHED_KEEP") {
        let keep = PathBuf::from(keep);
        std::fs::create_dir_all(&keep)?;
        std::fs::rename(&plan_at, keep.join("plan"))
            .or_else(|_| std::fs::copy(&plan_at, keep.join("plan")).map(|_| ()))?;
        write(&keep.join("paths"), &inputs)?;
        write(&keep.join("whole"), &whole)?;
        std::fs::remove_dir_all(&folder)?;
        println!(
            "kept in {}: plan, paths ({} numbers a path), whole ({} numbers a path)",
            keep.display(),
            MONTHS,
            totals.len()
        );
        return Ok(());
    }

    let me = std::env::current_exe()?;
    let cores = std::thread::available_parallelism()?.get();
    let each = scenarios.div_ceil(workers);
    let started = Instant::now();
    let mut running = Vec::new();
    for w in 0..workers {
        let first = w * each;
        let count = each.min(scenarios - first);
        let on = if cfg!(feature = "gpu") && w >= on_cpu {
            "gpu"
        } else {
            "cpu"
        };
        let answers = folder.join(format!("answers{w}"));
        let child = Command::new(&me)
            .arg("lot")
            .args([&plan_at, &inputs_at])
            .args([first.to_string(), count.to_string()])
            .arg(&answers)
            .args([on.to_string(), allowed.to_string()])
            .env("RAYON_NUM_THREADS", (cores / workers).max(1).to_string())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        running.push((child, first, count, on, answers));
    }
    println!(
        "{:>7} {:>9} {:>7} {:>5} {:>10} {:>10}",
        "worker", "from path", "paths", "on", "loaded in", "ran in"
    );
    let mut split = Vec::with_capacity(whole.len());
    for (w, (child, first, count, on, answers)) in running.into_iter().enumerate() {
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(format!("worker {w} failed").into());
        }
        let said = String::from_utf8(out.stdout)?;
        let mut said = said.split_whitespace().map(str::parse::<f64>);
        let (loaded, ran) = (
            said.next().ok_or("a worker said nothing")??,
            said.next().ok_or("a worker said too little")??,
        );
        println!(
            "{:>7} {:>9} {:>7} {:>5} {:>10} {:>10}",
            w,
            commas(first as f64),
            commas(count as f64),
            on,
            time(std::time::Duration::from_secs_f64(loaded)),
            time(std::time::Duration::from_secs_f64(ran))
        );
        split.extend(read(&answers, 0, count * totals.len())?);
    }
    let split_took = started.elapsed();
    std::fs::remove_dir_all(&folder)?;

    let same = split.len() == whole.len()
        && split
            .iter()
            .zip(&whole)
            .all(|(a, b)| a.to_bits() == b.to_bits());
    println!();
    println!(
        "{} workers, started to all answers back: {} (in one go: {})",
        workers,
        time(split_took),
        time(whole_took)
    );
    println!(
        "the answers put together are {} as in one go, for {} outputs on {} paths",
        if same {
            "the same to the last bit"
        } else {
            "NOT THE SAME"
        },
        totals.len(),
        commas(scenarios as f64)
    );
    if !same {
        return Err("the split run gave something else".into());
    }
    Ok(())
}
