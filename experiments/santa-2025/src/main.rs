mod engine;
mod geom;
use engine::*;
use geom::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

type Layout = Vec<(f64, f64, f64)>;

/// Side of the bounding square and whether any two trees overlap, checked from scratch.
fn check(layout: &Layout) -> (f64, bool) {
    let le = local_edges();
    let trees: Vec<Tree> = layout
        .iter()
        .map(|&(x, y, th)| Tree::new(x, y, th, &le))
        .collect();
    let mut b = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    let mut bad = false;
    for (i, t) in trees.iter().enumerate() {
        let q = t.bbox();
        b = (b.0.min(q.0), b.1.max(q.1), b.2.min(q.2), b.3.max(q.3));
        bad |= trees[..i].iter().any(|o| overlaps(t, o));
    }
    ((b.1 - b.0).max(b.3 - b.2), bad)
}
fn mid_of(layout: &Layout) -> (f64, f64) {
    let le = local_edges();
    let mut b = (
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
    );
    for &(x, y, th) in layout {
        let q = Tree::new(x, y, th, &le).bbox();
        b = (b.0.min(q.0), b.1.max(q.1), b.2.min(q.2), b.3.max(q.3));
    }
    ((b.0 + b.1) / 2.0, (b.2 + b.3) / 2.0)
}

// ---- starting layout: a lattice of an upright and an inverted tree ----
#[derive(Clone, Copy)]
struct Lattice {
    bx: f64,
    by: f64,
    px: f64,
    rx: f64,
    ry: f64,
}
impl Lattice {
    fn area(&self) -> f64 {
        self.px * self.ry / 2.0
    }
    fn ok(&self, le: &[(f64, f64, f64); NV]) -> bool {
        if self.px < 0.2 || self.ry < 0.2 {
            return false;
        }
        let a = Tree::new(0.0, 0.0, 0.0, le);
        for j in -4i32..=4 {
            for i in -6i32..=6 {
                let (ox, oy) = (i as f64 * self.px + j as f64 * self.rx, j as f64 * self.ry);
                if (i, j) != (0, 0) && overlaps(&a, &Tree::new(ox, oy, 0.0, le)) {
                    return false;
                }
                if overlaps(
                    &a,
                    &Tree::new(ox + self.bx, oy + self.by, std::f64::consts::PI, le),
                ) {
                    return false;
                }
            }
        }
        true
    }
}
fn find_lattice() -> Lattice {
    let le = local_edges();
    let mut best = Lattice {
        bx: 0.71,
        by: 0.6,
        px: 1.42,
        rx: 0.0,
        ry: 1.01,
    };
    assert!(best.ok(&le));
    for restart in 0..6u64 {
        let mut rng = Rng(0x9E3779B97F4A7C15 ^ (restart + 1).wrapping_mul(0xD1B54A32D192ED03));
        let mut cur = Lattice {
            bx: 0.71,
            by: 0.6,
            px: 1.42,
            rx: 0.0,
            ry: 1.01,
        };
        let iters = 60_000;
        for it in 0..iters {
            let frac = it as f64 / iters as f64;
            let step = 0.1 * (0.002f64 / 0.1).powf(frac);
            let temp = 0.01 * (1e-5f64 / 0.01).powf(frac);
            let mut c = cur;
            match rng.below(5) {
                0 => c.bx += step * rng.gauss(),
                1 => c.by += step * rng.gauss(),
                2 => c.px += step * rng.gauss(),
                3 => c.rx += step * rng.gauss(),
                _ => c.ry += step * rng.gauss(),
            }
            let d = c.area() - cur.area();
            if (d <= 0.0 || rng.unit() < (-d / temp).exp()) && c.ok(&le) {
                cur = c;
                if cur.area() < best.area() {
                    best = cur;
                }
            }
        }
    }
    best
}
fn start_layout(n: usize, l: &Lattice) -> Layout {
    let le = local_edges();
    let span = ((n as f64 * l.area()).sqrt() * 1.6 + 3.0).max(4.0);
    let mut all: Vec<((f64, f64, f64), (f64, f64, f64, f64))> = Vec::new();
    let jn = (span / l.ry) as i32 + 2;
    for j in -1..jn {
        let i0 = ((-(j as f64) * l.rx - 1.0) / l.px).floor() as i32 - 1;
        let i1 = ((span + 1.0 - j as f64 * l.rx) / l.px).ceil() as i32 + 1;
        for i in i0..i1 {
            let (ox, oy) = (i as f64 * l.px + j as f64 * l.rx, j as f64 * l.ry);
            for p in [(ox, oy, 0.0), (ox + l.bx, oy + l.by, std::f64::consts::PI)] {
                all.push((p, Tree::new(p.0, p.1, p.2, &le).bbox()));
            }
        }
    }
    let mut best: Option<(f64, Layout)> = None;
    for a in 0..12 {
        for b in 0..12 {
            let (wx, wy) = (a as f64 / 12.0 * l.px, b as f64 / 12.0 * l.ry);
            let mut need: Vec<(f64, (f64, f64, f64))> = all
                .iter()
                .filter(|(_, q)| q.0 >= wx && q.2 >= wy)
                .map(|(p, q)| ((q.1 - wx).max(q.3 - wy), *p))
                .collect();
            if need.len() < n {
                continue;
            }
            need.sort_by(|u, v| u.0.total_cmp(&v.0));
            let layout: Layout = need[..n].iter().map(|u| u.1).collect();
            let side = check(&layout).0;
            if best.as_ref().is_none_or(|b| side < b.0) {
                best = Some((side, layout));
            }
        }
    }
    best.unwrap().1
}

// ---- the search: simulated annealing, one tree moved at a time ----
struct Run {
    best: Layout,
    best_side: f64,
    moves: u64,
    accepted: u64,
}
fn anneal(eng: &mut dyn Engine, start: &Layout, budget: Duration, seed: u64) -> Run {
    let n = start.len();
    let mut rng = Rng(seed);
    let mut cur = start.clone();
    let mut side = check(start).0;
    // The engine's own figures for the start, by a move that changes nothing material.
    let e0 = eng.set(0, cur[0].0, cur[0].1, cur[0].2 + 1e-13);
    assert!(e0.feasible, "the starting layout must be free of overlaps");
    let mut cheb = e0.cheb;
    let mu = 0.5 / n as f64;
    let mut run = Run {
        best: cur.clone(),
        best_side: side,
        moves: 0,
        accepted: 0,
    };
    let t0 = Instant::now();
    let mut frac = 0.0f64;
    loop {
        if run.moves % 32 == 0 {
            frac = t0.elapsed().as_secs_f64() / budget.as_secs_f64();
            if frac >= 1.0 {
                break;
            }
        }
        let step = 0.08 * (0.001f64 / 0.08).powf(frac);
        let temp = 3e-3 * (1e-6f64 / 3e-3).powf(frac);
        let i = rng.below(n);
        let (mut x, mut y, mut th) = cur[i];
        if rng.unit() < 0.65 {
            x += step * rng.gauss();
            y += step * rng.gauss();
        } else {
            th += 4.0 * step * rng.gauss();
            x += 0.3 * step * rng.gauss();
            y += 0.3 * step * rng.gauss();
        }
        run.moves += 1;
        let e = eng.set(i, x, y, th);
        if !e.feasible {
            eng.undo();
            continue;
        }
        let d = (e.side - side) + mu * (e.cheb - cheb);
        if d <= 0.0 || rng.unit() < (-d / temp).exp() {
            cur[i] = (x, y, th);
            side = e.side;
            cheb = e.cheb;
            run.accepted += 1;
            if side < run.best_side - 1e-12 {
                run.best_side = side;
                run.best.clone_from(&cur);
            }
        } else {
            eng.undo();
        }
    }
    run
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args[1].as_str();
    let t = Instant::now();
    let lattice = find_lattice();
    eprintln!(
        "lattice: {:.4} area a tree (b=({:.4},{:.4}) px={:.4} r=({:.4},{:.4})), found in {:.1}s",
        lattice.area(),
        lattice.bx,
        lattice.by,
        lattice.px,
        lattice.rx,
        lattice.ry,
        t.elapsed().as_secs_f64()
    );
    match mode {
        // bench <n> <seconds>: moves a second by each engine from the same start.
        "bench" => {
            let n: usize = args[2].parse().unwrap();
            let secs: f64 = args[3].parse().unwrap();
            let start = start_layout(n, &lattice);
            let mid = mid_of(&start);
            let (side0, bad) = check(&start);
            println!("n={n} start side {side0:.5} overlap={bad}");
            let mut native = Native::new(&start, mid);
            let r = anneal(&mut native, &start, Duration::from_secs_f64(secs), 7);
            println!(
                "hand-written: {:>10.0} moves/s  accepted {:.1}%  side {:.5} -> {:.5}",
                r.moves as f64 / secs,
                100.0 * r.accepted as f64 / r.moves as f64,
                side0,
                r.best_side
            );
            let t = Instant::now();
            let mut shed = Shed::new(&start, mid).unwrap();
            let build = t.elapsed().as_secs_f64();
            let r = anneal(&mut shed, &start, Duration::from_secs_f64(secs), 7);
            println!(
                "watershed:    {:>10.0} moves/s  accepted {:.1}%  side {:.5} -> {:.5}",
                r.moves as f64 / secs,
                100.0 * r.accepted as f64 / r.moves as f64,
                side0,
                r.best_side
            );
            println!(
                "  graph: {} nodes, built in {:.2}s; {:.0} nodes recalculated per update, {:.1} us per update",
                shed.nodes(),
                build,
                shed.recalculated as f64 / shed.updates as f64,
                secs * 1e6 / shed.updates as f64
            );
            let (s2, bad2) = check(&r.best);
            println!("  watershed result rechecked by hand: side {s2:.5} overlap={bad2}");
        }
        // solve <native|watershed> <seconds per puzzle> <out.csv> [threads]
        "solve" => {
            let which = args[2].clone();
            let secs: f64 = args[3].parse().unwrap();
            let threads: usize = args.get(5).map_or(4, |s| s.parse().unwrap());
            let next = AtomicUsize::new(200);
            let results: Mutex<Vec<Option<(Layout, f64, f64, u64, f64)>>> =
                Mutex::new(vec![None; 201]);
            std::thread::scope(|sc| {
                for _ in 0..threads {
                    sc.spawn(|| {
                        loop {
                            let n = next.fetch_sub(1, Ordering::SeqCst);
                            if n == 0 || n > 200 {
                                next.store(0, Ordering::SeqCst);
                                break;
                            }
                            let start = start_layout(n, &lattice);
                            let mid = mid_of(&start);
                            let side0 = check(&start).0;
                            let t = Instant::now();
                            let mut eng: Box<dyn Engine> = if which == "native" { Box::new(Native::new(&start, mid)) } else { Box::new(Shed::new(&start, mid).unwrap()) };
                            let build = t.elapsed().as_secs_f64();
                            let r = anneal(eng.as_mut(), &start, Duration::from_secs_f64(secs), 1000 + n as u64);
                            let (side, bad) = check(&r.best);
                            assert!(!bad, "n={n}: overlap in the result");
                            eprintln!("n={n:3} side {side0:.4} -> {side:.4}  {:.0} moves/s  build {build:.1}s", r.moves as f64 / secs);
                            results.lock().unwrap()[n] = Some((r.best, side0, side, r.moves, build));
                        }
                    });
                }
            });
            let results = results.into_inner().unwrap();
            let mut csv = String::from("id,x,y,deg\n");
            let (mut score0, mut score, mut moves, mut build) = (0.0, 0.0, 0u64, 0.0);
            for n in 1..=200 {
                let (layout, side0, side, m, b) = results[n].as_ref().unwrap();
                score0 += side0 * side0 / n as f64;
                score += side * side / n as f64;
                moves += m;
                build += b;
                for (k, &(x, y, th)) in layout.iter().enumerate() {
                    csv += &format!("{n:03}_{k},s{x:.15},s{y:.15},s{:.15}\n", th.to_degrees());
                }
            }
            std::fs::write(&args[4], csv).unwrap();
            println!(
                "{which}: start {score0:.4} -> final {score:.4}; {moves} moves in total; setup {build:.0}s of cpu; wall {:.0}s",
                t.elapsed().as_secs_f64()
            );
        }
        _ => panic!("bench or solve"),
    }
}
