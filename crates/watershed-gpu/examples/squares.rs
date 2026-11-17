//! Packs unit squares into the smallest square it can find, as Kaggle's
//! "Squares in Squares" competition asks, to see what the engine is worth as
//! the evaluator in a search.
//!
//! The model is a penalty: how far the squares overlap one another and stick
//! out of a container of a given side. A packing is valid when it is zero.
//!
//!   3 inputs a square (centre x, centre y, angle), and the container's half side
//!   → a node a pair of squares: the depth they overlap by, squared
//!   → a node a square: how far it is outside the container, squared
//!   → their sum, the penalty; and the side of the square the packing fills
//!
//! The same graph is then used three ways:
//!
//! - **a move at a time**, on the graph: set one square's inputs, update,
//!   read the penalty. Annealing runs on this.
//! - **whole packings in a batch**, as a plan over all the inputs whose
//!   outputs are the penalty and its derivative by every input
//!   (`Graph::gradient`): the way downhill, and every step length along it
//!   at once. The polish runs on this.
//! - **one square in a batch**, as a plan over one square's inputs with the
//!   others fixed: many places to put it, tried at once.
//!
//!     cargo run --release -p watershed-gpu --example squares -- 10
//!
//! or choose the seconds to search for, and a file for the packing in the
//! competition's format:
//!
//!     cargo run --release -p watershed-gpu --example squares -- 11 60 packing.csv
//!
//! or give it a packing to tighten, such as a published record, keeping its
//! arrangement:
//!
//!     cargo run --release -p watershed-gpu --example squares -- polish record.csv tighter.csv
//!
//! `shared/squares_check.py` scores such a file with the competition's own
//! metric.

use std::f64::consts::FRAC_PI_4;
use std::sync::Arc;
use std::time::{Duration, Instant};

use watershed_core::prelude::*;
use watershed_core::{AggregateOp, BinOp, Ir, NodeId, Type, Value};
use watershed_gpu::Scenarios;

/// Two unit squares whose centres are at least this far apart, squared,
/// cannot touch.
const CLEAR: f64 = 2.0;

/// The places tried at once for a square that is put somewhere else.
const PLACES: usize = 20_000;

/// A penalty at or below this is a packing the polish can finish.
const NEARLY: f64 = 1e-7;

/// The best sides known for a few counts, from Erich Friedman's survey
/// "Packing Unit Squares in Squares".
const KNOWN: [(usize, f64); 6] = [
    (5, 2.707_106_781_186_547_6),
    (10, 3.707_106_781_186_547_6),
    (11, 3.877_083_590_060_299),
    (17, 4.675_553_577_415_86),
    (18, 4.822_875_655_532_295),
    (19, 4.885_618_083_164_127),
];

/// A small deterministic random number generator, so every run is the same.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Between -1 and 1.
    fn spread(&mut self) -> f64 {
        2.0 * self.unit() - 1.0
    }
}

/// A square: centre x, centre y, angle in radians.
type Square = [f64; 3];

/// The model in the graph.
struct Model {
    g: Graph,
    /// Each square's three inputs.
    squares: Vec<[Input<f64>; 3]>,
    half: Input<f64>,
    penalty: NodeId,
    side: NodeId,
    /// The penalty's derivative by each input, in the order of `inputs`.
    /// Empty in a model made without them.
    slopes: Vec<NodeId>,
}

/// Which pairs of squares a model has an overlap node for.
#[derive(Clone, Copy)]
enum Pairs {
    /// Every pair: the squares may go anywhere.
    All,
    /// Pairs whose centres start within this distance: for squares that
    /// will only be moved a little.
    Within(f64),
}

impl Model {
    /// The model with `packing` in it. With `slopes`, it also has the
    /// penalty's derivative by every input, which every move then updates:
    /// a model for moving one square at a time is made without.
    fn new(
        packing: &[Square],
        half: f64,
        pairs: Pairs,
        slopes: bool,
    ) -> Result<Model, Box<dyn std::error::Error>> {
        let n = packing.len();
        let mut g = Graph::new();
        let half = g.input("half", half)?;
        let present = g.input("present", true)?;

        let mut squares = Vec::with_capacity(n);
        let (mut cos, mut sin, mut reach) = (Vec::new(), Vec::new(), Vec::new());
        for (i, &[x, y, angle]) in packing.iter().enumerate() {
            let x = g.input(&format!("x{i}"), x)?;
            let y = g.input(&format!("y{i}"), y)?;
            let angle = g.input(&format!("angle{i}"), angle)?;
            squares.push([x, y, angle]);
            let c = g.define(&format!("cos{i}"), angle.cos())?;
            let s = g.define(&format!("sin{i}"), angle.sin())?;
            // How far the square reaches from its centre, along x and along y.
            reach.push(g.define(&format!("reach{i}"), (c.abs() + s.abs()) * 0.5)?);
            cos.push(c);
            sin.push(s);
        }

        let mut parts = Vec::with_capacity(n + n * (n - 1) / 2);
        let mut edges: [Vec<(NodeId, NodeId)>; 4] = Default::default();
        for i in 0..n {
            let [x, y, _] = squares[i];
            let out_x = (x.abs() + reach[i] - half).max(0.0);
            let out_y = (y.abs() + reach[i] - half).max(0.0);
            let outside = g.define(
                &format!("outside{i}"),
                out_x.clone() * out_x + out_y.clone() * out_y,
            )?;
            parts.push((outside.id(), present.id()));
            let ends = [x + reach[i], x - reach[i], y + reach[i], y - reach[i]];
            for (k, end) in ends.into_iter().enumerate() {
                let end = g.define(&format!("edge{k}_{i}"), end)?;
                edges[k].push((end.id(), present.id()));
            }
        }
        for i in 0..n {
            for j in i + 1..n {
                if let Pairs::Within(reach) = pairs {
                    let (dx, dy) = (packing[j][0] - packing[i][0], packing[j][1] - packing[i][1]);
                    if dx * dx + dy * dy > reach * reach {
                        continue;
                    }
                }
                let (dx, dy) = (squares[j][0] - squares[i][0], squares[j][1] - squares[i][1]);
                let (ci, si, cj, sj) = (cos[i], sin[i], cos[j], sin[j]);
                // Along any side of either square, the two reach this far
                // between them: half a side, and the other square's half
                // extent at the angle between them.
                let together = ((ci * cj + si * sj).abs() + (ci * sj - si * cj).abs() + 1.0) * 0.5;
                // The centres' distance along each of the four sides' directions.
                let apart = (dx.clone() * ci + dy.clone() * si)
                    .abs()
                    .max((dy.clone() * ci - dx.clone() * si).abs())
                    .max((dx.clone() * cj + dy.clone() * sj).abs())
                    .max((dy.clone() * cj - dx.clone() * sj).abs());
                let depth = (together - apart).max(0.0);
                // Plain arithmetic, with no test for squares far apart: the
                // depth is zero for them anyway, and a node with a condition
                // in it costs the graph more than this does.
                let overlap = g.define(&format!("overlap{i}_{j}"), depth.clone() * depth)?;
                parts.push((overlap.id(), present.id()));
            }
        }
        let penalty = g.aggregate("penalty", AggregateOp::Sum, Type::F64, 0, &parts)?;

        let [right, left, top, bottom] = edges;
        let right = g.aggregate("right", AggregateOp::Max, Type::F64, 0, &right)?;
        let left = g.aggregate("left", AggregateOp::Min, Type::F64, 0, &left)?;
        let top = g.aggregate("top", AggregateOp::Max, Type::F64, 0, &top)?;
        let bottom = g.aggregate("bottom", AggregateOp::Min, Type::F64, 0, &bottom)?;
        let span = |g: &Graph, high, low| {
            Arc::new(Ir::Binary(
                BinOp::Sub,
                Arc::new(g.reference(high)),
                Arc::new(g.reference(low)),
            ))
        };
        let side = Ir::Binary(BinOp::Max, span(&g, right, left), span(&g, top, bottom));
        let side = g.define_ir("side", side)?;
        let slopes = if slopes {
            let inputs: Vec<NodeId> = squares.iter().flatten().map(|i| i.id()).collect();
            g.gradient(penalty, &inputs, "slope")?
        } else {
            Vec::new()
        };
        g.stabilize();
        Ok(Model {
            g,
            squares,
            half,
            penalty,
            side,
            slopes,
        })
    }

    fn number(&self, id: NodeId) -> f64 {
        match self.g.value(id) {
            Value::F64(x) => *x,
            other => panic!("{other} is not a number"),
        }
    }

    fn penalty(&self) -> f64 {
        self.number(self.penalty)
    }

    fn place(&mut self, i: usize, square: Square) {
        for (input, value) in self.squares[i].into_iter().zip(square) {
            self.g.set(input, value).expect("it is an input");
        }
    }

    fn place_all(&mut self, packing: &[Square], half: f64) {
        for (i, &square) in packing.iter().enumerate() {
            self.place(i, square);
        }
        self.g.set(self.half, half).expect("it is an input");
        self.g.stabilize();
    }

    /// Every square's inputs, in the order a packing lists its numbers.
    fn inputs(&self) -> Vec<NodeId> {
        self.squares.iter().flatten().map(|i| i.id()).collect()
    }

    /// A plan over whole packings: the penalty and the side of each.
    fn whole(&self) -> Result<Scenarios, Box<dyn std::error::Error>> {
        Ok(Scenarios::with_shared(
            &self.g,
            &self.inputs(),
            &[self.half.id()],
            &[self.penalty, self.side],
        )?)
    }

    /// A plan over whole packings: the penalty of each, and its derivative
    /// by every input.
    fn downhill(&self) -> Result<Scenarios, Box<dyn std::error::Error>> {
        let mut outputs = vec![self.penalty];
        outputs.extend(&self.slopes);
        Ok(Scenarios::with_shared(
            &self.g,
            &self.inputs(),
            &[self.half.id()],
            &outputs,
        )?)
    }

    /// A plan over places for square `i`, the others where they are now.
    fn one(&self, i: usize) -> Result<Scenarios, Box<dyn std::error::Error>> {
        let vary = self.squares[i].map(|input| input.id());
        Ok(Scenarios::new(&self.g, &vary, &[self.penalty])?)
    }
}

// The same sums written out by hand, to check the graph against and to
// time it against.

fn overlap(a: Square, b: Square) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    if dx * dx + dy * dy >= CLEAR {
        return 0.0;
    }
    let depth = (together(a, b) - apart(a, b)).max(0.0);
    depth * depth
}

fn together(a: Square, b: Square) -> f64 {
    let (ci, si, cj, sj) = (a[2].cos(), a[2].sin(), b[2].cos(), b[2].sin());
    ((ci * cj + si * sj).abs() + (ci * sj - si * cj).abs() + 1.0) * 0.5
}

fn apart(a: Square, b: Square) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (ci, si, cj, sj) = (a[2].cos(), a[2].sin(), b[2].cos(), b[2].sin());
    (dx * ci + dy * si)
        .abs()
        .max((dy * ci - dx * si).abs())
        .max((dx * cj + dy * sj).abs())
        .max((dy * cj - dx * sj).abs())
}

fn reach(a: Square) -> f64 {
    (a[2].cos().abs() + a[2].sin().abs()) * 0.5
}

fn outside(a: Square, half: f64) -> f64 {
    let out_x = (a[0].abs() + reach(a) - half).max(0.0);
    let out_y = (a[1].abs() + reach(a) - half).max(0.0);
    out_x * out_x + out_y * out_y
}

fn penalty_by_hand(packing: &[Square], half: f64) -> f64 {
    let mut sum = 0.0;
    for (i, &a) in packing.iter().enumerate() {
        sum += outside(a, half);
        for &b in &packing[i + 1..] {
            sum += overlap(a, b);
        }
    }
    sum
}

/// What square `i` adds to the penalty: all a move of it can change.
fn share_by_hand(packing: &[Square], i: usize, half: f64) -> f64 {
    let a = packing[i];
    let mut sum = outside(a, half);
    for (j, &b) in packing.iter().enumerate() {
        if j != i {
            sum += overlap(a, b);
        }
    }
    sum
}

fn side_by_hand(packing: &[Square]) -> f64 {
    let ends = |k: usize, sign: f64| {
        packing
            .iter()
            .map(|&a| sign * (a[k] + sign * reach(a)))
            .fold(f64::NEG_INFINITY, f64::max)
    };
    (ends(0, 1.0) + ends(0, -1.0)).max(ends(1, 1.0) + ends(1, -1.0))
}

/// Squares on a grid of `ceil(sqrt(n))` a side: the packing anyone can make.
fn grid(n: usize) -> Vec<Square> {
    let across = (n as f64).sqrt().ceil() as usize;
    (0..n)
        .map(|k| {
            let at = |c: usize| c as f64 + 0.5 - across as f64 / 2.0;
            [at(k % across), at(k / across), 0.0]
        })
        .collect()
}

fn scattered(n: usize, half: f64, rng: &mut Rng) -> Vec<Square> {
    let room = (half - 0.5).max(0.0);
    (0..n)
        .map(|_| {
            [
                room * rng.spread(),
                room * rng.spread(),
                FRAC_PI_4 * rng.spread(),
            ]
        })
        .collect()
}

/// Moves one square at a time on the graph, keeping a move that lowers the
/// penalty and, while it is warm, some that raise it: from `warm`, the size
/// of a rise that is likely kept at first, down to nearly none. Returns the
/// moves made.
fn anneal(
    model: &mut Model,
    packing: &mut [Square],
    moves: usize,
    warm: f64,
    rng: &mut Rng,
) -> usize {
    let n = packing.len();
    let cold = 1e-10_f64;
    let mut now = model.penalty();
    for m in 0..moves {
        if now <= NEARLY * 1e-3 {
            return m;
        }
        let heat = warm * (cold / warm).powf(m as f64 / moves as f64);
        let step = (3.0 * heat.sqrt()).clamp(1e-5, 0.3);
        let i = rng.below(n);
        let was = packing[i];
        let mut to = was;
        match rng.below(4) {
            // A turn alone, a slide alone, or both.
            0 => to[2] += 2.0 * step * rng.spread(),
            1 => {
                to[0] += step * rng.spread();
                to[1] += step * rng.spread();
            }
            // A new angle altogether: a square's turns are a quarter apart.
            2 if heat > 1e-5 => to[2] = FRAC_PI_4 * rng.spread(),
            _ => {
                to[0] += step * rng.spread();
                to[1] += step * rng.spread();
                to[2] += step * rng.spread();
            }
        }
        model.place(i, to);
        model.g.stabilize();
        let then = model.penalty();
        if then <= now || rng.unit() < ((now - then) / heat).exp() {
            packing[i] = to;
            now = then;
        } else {
            model.place(i, was);
            model.g.stabilize();
        }
    }
    moves
}

/// Takes the square that adds most to the penalty out and puts it back at
/// the best of many places, all tried at once through a plan made for that
/// square with the others where they are.
fn replace_worst(
    model: &mut Model,
    packing: &mut [Square],
    half: f64,
    rng: &mut Rng,
) -> Result<(), Box<dyn std::error::Error>> {
    let worst = (0..packing.len())
        .max_by(|&a, &b| {
            share_by_hand(packing, a, half).total_cmp(&share_by_hand(packing, b, half))
        })
        .expect("there are squares");
    let plan = model.one(worst)?;
    let room = (half - 0.5).max(0.0);
    let places: Vec<f64> = (0..PLACES)
        .flat_map(|_| {
            [
                room * rng.spread(),
                room * rng.spread(),
                FRAC_PI_4 * rng.spread(),
            ]
        })
        .collect();
    let penalties = plan.run_cpu(&places);
    let best = (0..PLACES)
        .min_by(|&a, &b| penalties[a].total_cmp(&penalties[b]))
        .expect("there are places");
    packing[worst].copy_from_slice(&places[3 * best..3 * best + 3]);
    model.place(worst, packing[worst]);
    model.g.stabilize();
    Ok(())
}

/// Lowers the penalty of a packing as far as it will go, down its
/// gradient, which the plan gives with the penalty: the derivative nodes are
/// among its outputs. Each round runs one batch, of every step length at
/// once along a direction that remembers the last few rounds (L-BFGS).
/// Returns the penalty reached and the batches run.
fn descend(plan: &Scenarios, packing: &mut [Square], rounds: usize) -> (f64, usize) {
    let width = 3 * packing.len();
    let out = width + 1;
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let mut at: Vec<f64> = packing.iter().flatten().copied().collect();
    let first = plan.run_cpu(&at);
    let (mut now, mut slope) = (first[0], first[1..].to_vec());
    // The last few steps and what each did to the slope.
    let mut memory: Vec<(Vec<f64>, Vec<f64>)> = Vec::new();
    let mut batches = 1;
    for _ in 0..rounds {
        if now <= 1e-28 || dot(&slope, &slope) == 0.0 {
            break;
        }
        // The way to go: downhill, bent by what the remembered steps say of
        // the curvature.
        let mut way: Vec<f64> = slope.iter().map(|s| -s).collect();
        let mut weights = Vec::with_capacity(memory.len());
        for (step, change) in memory.iter().rev() {
            let weight = dot(step, &way) / dot(step, change);
            way.iter_mut()
                .zip(change)
                .for_each(|(w, c)| *w -= weight * c);
            weights.push(weight);
        }
        if let Some((step, change)) = memory.last() {
            let scale = dot(step, change) / dot(change, change);
            way.iter_mut().for_each(|w| *w *= scale);
        }
        for ((step, change), weight) in memory.iter().zip(weights.iter().rev()) {
            let back = dot(change, &way) / dot(step, change);
            way.iter_mut()
                .zip(step)
                .for_each(|(w, s)| *w += (weight - back) * s);
        }
        if dot(&way, &slope) >= 0.0 {
            way = slope.iter().map(|s| -s).collect();
            memory.clear();
        }
        // Every step length from too long to far too short, at once.
        let lengths: Vec<f64> = (0..12).map(|k| 2.0 * 0.4_f64.powi(k)).collect();
        let mut tried = Vec::with_capacity(lengths.len() * width);
        for length in &lengths {
            tried.extend(at.iter().zip(&way).map(|(x, w)| x + length * w));
        }
        let reached = plan.run_cpu(&tried);
        batches += 1;
        let best = (0..lengths.len())
            .min_by(|&a, &b| reached[out * a].total_cmp(&reached[out * b]))
            .expect("there are step lengths");
        if reached[out * best] >= now {
            if memory.is_empty() {
                break;
            }
            // The remembered curvature misled: forget it and go straight down.
            memory.clear();
            continue;
        }
        let to = &tried[best * width..(best + 1) * width];
        let new_slope = &reached[out * best + 1..out * (best + 1)];
        let step: Vec<f64> = to.iter().zip(&at).map(|(a, b)| a - b).collect();
        let change: Vec<f64> = new_slope.iter().zip(&slope).map(|(a, b)| a - b).collect();
        if dot(&step, &change) > 1e-300 {
            memory.push((step, change));
            if memory.len() > 12 {
                memory.remove(0);
            }
        }
        now = reached[out * best];
        at.copy_from_slice(to);
        slope.copy_from_slice(new_slope);
    }
    for (square, numbers) in packing.iter_mut().zip(at.chunks(3)) {
        square.copy_from_slice(numbers);
    }
    (now, batches)
}

/// Spreads the squares' centres just far enough that no two overlap, and
/// returns the side of the square they then fill. What is left of the
/// penalty is paid for here, in side.
fn spread(packing: &mut [Square]) -> f64 {
    let mut by = 1.0_f64;
    for (i, &a) in packing.iter().enumerate() {
        for &b in &packing[i + 1..] {
            if overlap(a, b) > 0.0 {
                by = by.max(together(a, b) / apart(a, b));
            }
        }
    }
    // A little more, so that squares that would touch stand clear when the
    // numbers are written out and read back.
    by *= 1.0 + 1e-11;
    for square in packing.iter_mut() {
        square[0] *= by;
        square[1] *= by;
    }
    side_by_hand(packing)
}

/// Reads a packing in the competition's format: a line a square, with
/// columns named `x`, `y` and `angle` among them.
fn read(path: &str) -> Result<Vec<Square>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines();
    let heading: Vec<&str> = lines
        .next()
        .ok_or("the file is empty")?
        .split(',')
        .collect();
    let column = |name: &str| {
        heading
            .iter()
            .position(|h| h.trim() == name)
            .ok_or(format!("{path} has no column {name}"))
    };
    let columns = [column("x")?, column("y")?, column("angle")?];
    let mut packing = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        let cells: Vec<&str> = line.split(',').collect();
        let mut square = [0.0; 3];
        for (number, &at) in square.iter_mut().zip(&columns) {
            *number = cells.get(at).ok_or("a line is short")?.trim().parse()?;
        }
        packing.push(square);
    }
    Ok(packing)
}

fn write(path: &str, packing: &[Square]) -> std::io::Result<()> {
    let n = packing.len();
    let mut csv = String::from("id,n,square_id,x,y,angle\n");
    for (k, [x, y, angle]) in packing.iter().enumerate() {
        csv.push_str(&format!("{k},{n},{k},{x},{y},{angle}\n"));
    }
    std::fs::write(path, csv)
}

/// Moves a packing so that the square it fills is centred on the origin,
/// where the model's container is.
fn centre(packing: &mut [Square]) {
    let end = |k: usize, sign: f64| {
        packing
            .iter()
            .map(|&a| sign * a[k] + reach(a))
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let shift = [
        (end(0, 1.0) - end(0, -1.0)) / 2.0,
        (end(1, 1.0) - end(1, -1.0)) / 2.0,
    ];
    for square in packing.iter_mut() {
        square[0] -= shift[0];
        square[1] -= shift[1];
    }
}

/// Tries to tighten a packing that is already good, such as a published
/// record: squeezes it to a slightly smaller side, goes down the penalty's
/// gradient from there, and keeps the result if what comes out, spread until
/// nothing overlaps, fills a smaller square than before. The arrangement is
/// kept: only pairs of squares that start near each other are modelled.
fn polish(path: &str, out: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut best = read(path)?;
    let n = best.len();
    centre(&mut best);
    let given = side_by_hand(&best);
    // As doubles, squares that touched may overlap by a rounding: the side
    // to beat is the side once they do not.
    let mut side = spread(&mut best);
    let start = side;
    let started = Instant::now();
    let model = Model::new(&best, side / 2.0, Pairs::Within(2.5), true)?;
    let mut plan = model.downhill()?;
    let made = started.elapsed();
    let started = Instant::now();
    let (mut descents, mut batches) = (0, 0);
    for shrink in [1e-3, 1e-5, 1e-7] {
        loop {
            let aim = side * (1.0 - shrink);
            let mut packing: Vec<Square> = best
                .iter()
                .map(|&[x, y, a]| [x * (1.0 - shrink), y * (1.0 - shrink), a])
                .collect();
            plan.set_shared(&[aim / 2.0]);
            let (_, ran) = descend(&plan, &mut packing, 400);
            descents += 1;
            batches += ran;
            centre(&mut packing);
            let reached = spread(&mut packing);
            if reached >= side - 1e-10 {
                break;
            }
            side = reached;
            best = packing;
        }
    }
    assert_eq!(penalty_by_hand(&best, side), 0.0, "the packing overlaps");
    let gain = start - side;
    println!(
        "{n} squares: side {given:.12} given, {start:.12} as doubles with nothing overlapping, {side:.12} polished: {} ({descents} descents, {batches} batches, {}; the model made in {})",
        if gain > 1e-9 {
            format!("TIGHTER by {gain:.3e}")
        } else {
            "no tighter".to_string()
        },
        time(started.elapsed()),
        time(made)
    );
    if let Some(out) = out {
        write(&out, &best)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arg = |nth: usize| std::env::args().nth(nth);
    if arg(1).as_deref() == Some("polish") {
        let path = arg(2).ok_or("polish needs a packing file")?;
        return polish(&path, arg(3));
    }
    let n: usize = arg(1).map_or(10, |n| n.parse().expect("the count is a number"));
    let seconds: f64 = arg(2).map_or(20.0, |s| s.parse().expect("the seconds are a number"));
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);

    let ceiling = (n as f64).sqrt().ceil();
    let started = Instant::now();
    let mut model = Model::new(&grid(n), ceiling / 2.0, Pairs::All, false)?;
    let built = started.elapsed();
    let started = Instant::now();
    let whole = model.whole()?;
    println!(
        "{n} squares: a graph of {} nodes, made in {}; a plan over whole packings of {} nodes, made in {}",
        model.g.len(),
        time(built),
        whole.nodes(),
        time(started.elapsed())
    );

    // The same model with the penalty's derivatives, and a plan that gives
    // them: what the polish goes downhill on.
    let started = Instant::now();
    let with_slopes = Model::new(&grid(n), ceiling / 2.0, Pairs::All, true)?;
    let built = started.elapsed();
    let started = Instant::now();
    let mut downhill = with_slopes.downhill()?;
    println!(
        "with the penalty's derivative by all {} inputs: a graph of {} nodes, made in {}; a plan of {} nodes, made in {}",
        3 * n,
        with_slopes.g.len(),
        time(built),
        downhill.nodes(),
        time(started.elapsed())
    );

    check(&mut model, &whole, &mut rng);
    check_slopes(&whole, &downhill, &mut rng);
    timings(&mut model, &whole, &downhill, &mut rng)?;

    // The search. A side a step below the best is tried by annealing at it.
    // One that works is polished and kept; after one that does not, the
    // step is halved. A failure does not show a side to be out of reach,
    // only that these attempts did not reach it.
    println!();
    println!("searching for {seconds} s, from the grid's side of {ceiling}");
    let search = Instant::now();
    let within = Duration::from_secs_f64(seconds);
    let mut best: (f64, Vec<Square>) = (ceiling, grid(n));
    let (mut moves, mut batches, mut tries, mut replaced) = (0, 0, 0, 0);
    let mut step = (ceiling - (n as f64).sqrt()) / 3.0;
    while search.elapsed() < within && step > 1e-9 {
        let aim = best.0 - step;
        let mut found = false;
        // A small step keeps the arrangement and only tightens it.
        let tightening = step < 0.01;
        for attempt in 0..4 {
            tries += 1;
            // Alternately the best packing squeezed to the new side and
            // shaken, and squares scattered afresh.
            let mut packing = if tightening || attempt % 2 == 0 {
                let squeeze = (aim - 1.0) / (best.0 - 1.0).max(1e-9);
                let shake = if tightening { step } else { 0.05 };
                best.1
                    .iter()
                    .map(|&[x, y, a]| {
                        [
                            x * squeeze + shake * rng.spread(),
                            y * squeeze + shake * rng.spread(),
                            a + shake * rng.spread(),
                        ]
                    })
                    .collect()
            } else {
                scattered(n, aim / 2.0, &mut rng)
            };
            model.place_all(&packing, aim / 2.0);
            if tightening {
                moves += anneal(
                    &mut model,
                    &mut packing,
                    3_000 * n,
                    10.0 * step * step,
                    &mut rng,
                );
            } else {
                moves += anneal(&mut model, &mut packing, 20_000 * n, 0.03, &mut rng);
                // Stuck with squares overlapping: move the worst one, and settle.
                for _ in 0..3 {
                    if model.penalty() <= NEARLY || search.elapsed() >= within {
                        break;
                    }
                    replace_worst(&mut model, &mut packing, aim / 2.0, &mut rng)?;
                    replaced += 1;
                    moves += anneal(&mut model, &mut packing, 5_000 * n, 1e-4, &mut rng);
                }
            }
            if model.penalty() > NEARLY {
                if search.elapsed() >= within {
                    break;
                }
                continue;
            }
            downhill.set_shared(&[aim / 2.0]);
            let (left, ran) = descend(&downhill, &mut packing, 500);
            batches += ran;
            let side = spread(&mut packing);
            if side < best.0 {
                println!(
                    "  {:>7.1} s  side {side:.6}  (aimed at {aim:.6}; penalty {left:.1e} before spreading)",
                    search.elapsed().as_secs_f64()
                );
                best = (side, packing);
                found = true;
                break;
            }
        }
        if !found {
            step /= 2.0;
        }
    }
    println!(
        "{tries} tries: {moves} moves on the graph ({} a move over the search), {replaced} squares put elsewhere by a plan of {PLACES} places, {batches} batches down the gradient",
        time(search.elapsed() / moves.max(1) as u32)
    );

    // The answer, checked by hand and on the graph.
    let (side, packing) = best;
    assert_eq!(penalty_by_hand(&packing, side), 0.0, "the packing overlaps");
    model.place_all(&packing, side);
    assert_eq!(model.penalty(), 0.0, "the graph says the packing overlaps");
    println!();
    println!("side {side:.9} for {n} squares");
    println!(
        "  the grid {ceiling}; the area alone allows {:.6}; the competition's score for it {:.6}",
        (n as f64).sqrt(),
        side / (n as f64).sqrt() - 1.0
    );
    if let Some((_, known)) = KNOWN.iter().find(|(count, _)| *count == n) {
        println!(
            "  the best known {known:.9}: this is {:.2e} above it",
            side - known
        );
    }
    if let Some(path) = arg(3) {
        write(&path, &packing)?;
        println!("  written to {path}");
    }
    Ok(())
}

/// The graph, the plan and the sums by hand give the same penalty and side.
fn check(model: &mut Model, whole: &Scenarios, rng: &mut Rng) {
    let n = model.squares.len();
    let half = 0.45 * (n as f64).sqrt().ceil();
    let mut worst = 0.0_f64;
    for _ in 0..20 {
        let packing = scattered(n, half, rng);
        model.place_all(&packing, half);
        let mut plan = whole.clone();
        plan.set_shared(&[half]);
        let flat: Vec<f64> = packing.iter().flatten().copied().collect();
        let got = plan.run_cpu(&flat);
        let (penalty, side) = (model.penalty(), model.number(model.side));
        assert!(
            got[0].to_bits() == penalty.to_bits() && got[1].to_bits() == side.to_bits(),
            "the plan gives {got:?}, the graph {penalty:?} and {side:?}"
        );
        // By hand the parts are added in another order.
        let by_hand = penalty_by_hand(&packing, half);
        worst = worst.max((penalty - by_hand).abs() / by_hand.max(1e-300));
        assert_eq!(side, side_by_hand(&packing));
    }
    assert!(
        worst < 1e-12,
        "the graph and the sums by hand differ by {worst:e}"
    );
    println!(
        "checked on 20 scattered packings: the plan gives the graph's penalty and side to the last bit, and the sums by hand agree to {worst:.0e}"
    );
}

/// The derivative nodes give what nudging each input gives.
fn check_slopes(whole: &Scenarios, downhill: &Scenarios, rng: &mut Rng) {
    let n = downhill.inputs() / 3;
    let half = 0.45 * (n as f64).sqrt().ceil();
    let (mut whole, mut downhill) = (whole.clone(), downhill.clone());
    whole.set_shared(&[half]);
    downhill.set_shared(&[half]);
    let flat: Vec<f64> = scattered(n, half, rng).into_iter().flatten().collect();
    let slopes = downhill.run_cpu(&flat);
    let nudge = 1e-6;
    let mut worst = 0.0_f64;
    for k in 0..flat.len() {
        let mut both = [flat.clone(), flat.clone()].concat();
        both[k] += nudge;
        both[flat.len() + k] -= nudge;
        let around = whole.run_cpu(&both);
        let nudged = (around[0] - around[2]) / (2.0 * nudge);
        worst = worst.max((slopes[1 + k] - nudged).abs() / nudged.abs().max(1.0));
    }
    assert!(
        worst < 1e-6,
        "a derivative is {worst:e} from what nudging gives"
    );
    println!(
        "checked on a scattered packing: each of the {} derivatives is what nudging its input gives, to {worst:.0e}",
        flat.len()
    );
}

/// What each way of asking costs.
fn timings(
    model: &mut Model,
    whole: &Scenarios,
    downhill: &Scenarios,
    rng: &mut Rng,
) -> Result<(), Box<dyn std::error::Error>> {
    let n = model.squares.len();
    // A packing like one a search is in the middle of: the grid squeezed a
    // little and shaken, so that neighbours overlap and the rest do not.
    let half = 0.48 * (n as f64).sqrt().ceil();
    let mut packing: Vec<Square> = grid(n)
        .iter()
        .map(|&[x, y, _]| [0.96 * x, 0.96 * y, 0.1 * rng.spread()])
        .collect();
    model.place_all(&packing, half);
    let shaken = |square: Square, rng: &mut Rng| {
        [
            square[0] + 0.01 * rng.spread(),
            square[1] + 0.01 * rng.spread(),
            square[2] + 0.01 * rng.spread(),
        ]
    };

    println!();
    println!("one move of one square, the penalty read after it:");
    let base = packing.clone();
    let moves = 20_000;
    let mut recalculated = 0;
    let started = Instant::now();
    for _ in 0..moves {
        let i = rng.below(n);
        packing[i] = shaken(base[i], rng);
        model.place(i, packing[i]);
        recalculated += model.g.stabilize().recalculated;
    }
    let on_graph = started.elapsed() / moves;
    row(
        "the graph",
        on_graph,
        &format!("{} nodes recalculated", recalculated / moves as usize),
    );
    let mut sum = model.penalty();
    let started = Instant::now();
    for _ in 0..moves {
        let i = rng.below(n);
        let before = share_by_hand(&packing, i, half);
        packing[i] = shaken(base[i], rng);
        sum += share_by_hand(&packing, i, half) - before;
    }
    let by_hand = started.elapsed() / moves;
    std::hint::black_box(sum);
    row("by hand, the moved square's share", by_hand, "");
    let started = Instant::now();
    for _ in 0..200 {
        std::hint::black_box(penalty_by_hand(&packing, half));
    }
    row("by hand, everything again", started.elapsed() / 200, "");
    println!(
        "  the graph takes {:.0} times the share by hand",
        on_graph.as_secs_f64() / by_hand.as_secs_f64()
    );

    println!();
    println!("batches through a plan, on {} cores:", cores());
    let mut plan = whole.clone();
    plan.set_shared(&[half]);
    let flat: Vec<f64> = packing.iter().flatten().copied().collect();
    let count = (2_000_000 / (n * n)).clamp(64, 20_000);
    let many: Vec<f64> = (0..count)
        .flat_map(|_| {
            flat.iter()
                .map(|x| x + 0.01 * rng.spread())
                .collect::<Vec<_>>()
        })
        .collect();
    let started = Instant::now();
    std::hint::black_box(plan.run_cpu(&many));
    let each = started.elapsed() / count as u32;
    row("a whole packing", each, &format!("{count} at once"));
    let nudging = each * 6 * n as u32;
    row(
        "a gradient: every input nudged up and down",
        nudging,
        &format!("{} packings", 6 * n),
    );
    let mut plan = downhill.clone();
    plan.set_shared(&[half]);
    let fewer = (count / 8).max(cores());
    let started = Instant::now();
    std::hint::black_box(plan.run_cpu(&many[..fewer * 3 * n]));
    let each = started.elapsed() / fewer as u32;
    row(
        "a gradient: the derivative nodes",
        each,
        &format!(
            "{fewer} at once; {:.0} times quicker",
            nudging.as_secs_f64() / each.as_secs_f64()
        ),
    );
    model.place_all(&packing, half);
    let started = Instant::now();
    let one = model.one(n / 2)?;
    let made = started.elapsed();
    let places = 200_000;
    let tried: Vec<f64> = (0..places)
        .flat_map(|_| {
            [
                half * rng.spread(),
                half * rng.spread(),
                FRAC_PI_4 * rng.spread(),
            ]
        })
        .collect();
    let started = Instant::now();
    let penalties = one.run_cpu(&tried);
    let took = started.elapsed();
    let best = penalties.iter().copied().fold(f64::INFINITY, f64::min);
    row(
        "a place for one square, the rest fixed",
        took / places,
        &format!(
            "{places} at once in {}; the plan of {} nodes made in {}; the best leaves a penalty of {best:.2e}",
            time(took),
            one.nodes(),
            time(made)
        ),
    );
    Ok(())
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

fn row(label: &str, each: Duration, note: &str) {
    println!("  {label:<44} {:>10}   {note}", time(each));
}

fn time(d: Duration) -> String {
    let s = d.as_secs_f64();
    if s < 1e-6 {
        format!("{:.0} ns", s * 1e9)
    } else if s < 0.001 {
        format!("{:.1} µs", s * 1e6)
    } else if s < 1.0 {
        format!("{:.1} ms", s * 1e3)
    } else {
        format!("{s:.2} s")
    }
}
