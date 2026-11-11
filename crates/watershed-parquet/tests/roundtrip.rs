//! Writes random runs to files and reads them back. The graph's own history
//! (`Graph::value_at`, checked against full copies in watershed-core's
//! property tests) is the reference: the files must agree with it for every
//! node at every update.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use watershed_core::prelude::*;
use watershed_core::{CalcError as E, Value};
use watershed_parquet::read::Run;
use watershed_parquet::{LocalStore, Options, Put, Stats, Store, parquet_sink};

/// A fresh folder for one test.
fn folder(name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "watershed-parquet-{name}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

/// Runs a random graph for `updates` updates with a sink attached after
/// `attach_after`, then checks the files against the graph's history.
/// Returns the writer's stats.
fn run(seed: u64, dir: &Path, attach_after: u64, updates: u64, rows_per_file: usize) -> Stats {
    let mut rng = Rng(seed);
    let mut g = Graph::new();
    let floats: Vec<Input<f64>> = (0..4)
        .map(|k| g.input(&format!("x{k}"), 1.0).unwrap())
        .collect();
    let ints: Vec<Input<i64>> = (0..3)
        .map(|k| g.input(&format!("n{k}"), 1_i64).unwrap())
        .collect();
    let word = g.input("word", "a".to_string()).unwrap();
    let tags = g.input("tags", Vec::<String>::new()).unwrap();
    g.define("words", watershed_core::list([word, word]))
        .unwrap();
    let flag = g.input("flag", true).unwrap();
    let product = g.define("product", floats[0] * floats[1]).unwrap();
    g.define("sum", floats[2] + floats[3] + product).unwrap();
    g.define("pick", when(flag).then(floats[0]).otherwise(floats[3]))
        .unwrap();
    g.define("ratio", ints[0] / ints[1]).unwrap(); // an error when n1 is 0
    g.define("big", ints[2].gt(1_i64)).unwrap();
    let mut orders = Collection::new(&g, "orders");
    let qty = orders.field::<f64>("qty").unwrap();
    let line = orders.column(&mut g, "line", qty * floats[0]).unwrap();
    orders.sum(&mut g, "revenue", line).unwrap();
    let mut late = 0;

    let float_values = [0.0, -0.0, 1.0, 2.5, f64::NAN, f64::INFINITY, 1e308];
    let mut writer = None;
    for update in 0..updates {
        if update == attach_after {
            g.keep_history(u64::MAX);
            let mut options = Options::new("test");
            options.rows_per_file = rows_per_file;
            options.queue_updates = 4;
            let (sink, w) = parquet_sink(LocalStore::new(dir), options);
            writer = Some(w);
            g.attach_sink(sink);
        }
        for _ in 0..1 + rng.below(3) {
            match rng.below(6) {
                0 => g.set(rng.pick(&floats), rng.pick(&float_values)).unwrap(),
                1 => g.set(rng.pick(&ints), rng.pick(&[0_i64, 1, 2, 3])).unwrap(),
                2 => g.set(word, rng.pick(&["a", "b", ""]).to_string()).unwrap(),
                3 => g.set(flag, rng.below(2) == 0).unwrap(),
                4 => {
                    let items: &[&str] =
                        rng.pick(&[&[][..], &["a"][..], &["a", "b"][..], &["", "c"][..]]);
                    g.set(tags, items.iter().map(|t| t.to_string()).collect())
                        .unwrap()
                }
                _ => {
                    let key = format!("k{}", rng.below(6));
                    if rng.below(3) == 0 {
                        orders.remove(&mut g, &key).unwrap();
                    } else {
                        orders
                            .upsert(&mut g, &key, &[qty.is(rng.pick(&float_values))])
                            .unwrap();
                    }
                }
            }
        }
        // Sometimes a node appears between updates.
        if rng.below(10) == 0 {
            g.define(&format!("late{late}"), rng.pick(&floats) * 3.0)
                .unwrap();
            late += 1;
        }
        g.stabilize();
    }
    g.detach_sink();
    let stats = writer.expect("attached").close().unwrap();

    let files = Run::load(dir, "test").unwrap();
    let (oldest, latest) = g.history_range().unwrap();
    for epoch in 0..=latest {
        for id in g.node_ids() {
            let node = id.index() as u32;
            let found = files.value_at(node, epoch);
            let expected = if epoch < oldest {
                None
            } else {
                match g.value_at(id, epoch) {
                    Ok(v) => Some(v),
                    Err(E::NotCreatedYet { .. }) => None,
                    Err(e) => panic!("{e}"),
                }
            };
            let agree = match (found, expected) {
                (Some(a), Some(b)) => a.same_as(b),
                (None, None) => true,
                _ => false,
            };
            assert!(
                agree,
                "seed {seed}: {} at update {epoch}: files say {found:?}, graph says {expected:?}",
                g.name(id)
            );
        }
    }
    for id in g.node_ids() {
        let (name, _, _) = files.node(id.index() as u32).expect("every node described");
        assert_eq!(name, g.name(id));
    }
    stats
}

#[test]
fn files_agree_with_the_graph_at_every_update() {
    for seed in 0..40 {
        let dir = folder("agree");
        let stats = run(seed, &dir, seed % 5, 120, 1 + (seed as usize % 7) * 20);
        assert!(stats.files_written > 1, "seed {seed}: {stats:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn writing_the_same_updates_again_keeps_the_first_files_and_adds_nothing() {
    let dir = folder("again");
    let first = run(7, &dir, 3, 80, 25);
    let rows = Run::load(&dir, "test").unwrap().rows;

    let second = run(7, &dir, 3, 80, 25);
    assert_eq!(second.files_written, 0);
    assert_eq!(second.files_already_there, first.files_written);
    assert_eq!(Run::load(&dir, "test").unwrap().rows, rows);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_flush_stores_everything_so_far_and_says_how_far() -> Result<(), CalcError> {
    let dir = folder("flush");
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let (sink, writer) = parquet_sink(LocalStore::new(&dir), Options::new("r"));
    assert_eq!(writer.flushed_through(), None);
    g.attach_sink(sink);
    for i in 1..=3 {
        g.set(a, f64::from(i) * 10.0)?;
        g.stabilize();
    }
    writer.flush().unwrap();
    assert_eq!(writer.flushed_through(), Some(3));
    let files = Run::load(&dir, "r").unwrap();
    assert!(
        files
            .value_at(0, 2)
            .is_some_and(|v| v.same_as(&Value::F64(20.0)))
    );
    g.detach_sink();
    writer.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A store that always fails.
struct Broken;
impl Store for Broken {
    fn put_if_absent(&self, _: &str, _: &[u8]) -> io::Result<Put> {
        Err(io::Error::other("disk full"))
    }
}

#[test]
fn a_failing_store_is_reported_and_the_graph_carries_on() -> Result<(), CalcError> {
    let mut g = Graph::new();
    let a = g.input("a", 1.0)?;
    let doubled = g.define("doubled", a * 2.0)?;
    let (sink, writer) = parquet_sink(Broken, Options::new("r"));
    g.attach_sink(sink);
    assert!(writer.flush().is_err());
    for i in 0..5 {
        g.set(a, f64::from(i))?;
        g.stabilize();
    }
    assert_eq!(g.get(doubled)?, 8.0);
    assert!(writer.close().is_err());
    Ok(())
}
