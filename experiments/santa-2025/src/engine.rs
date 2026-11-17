//! Two implementations of the same scorer: hand-written, and on Watershed.
use crate::geom::*;
use watershed_core::prelude::*;
use watershed_core::{AggregateOp, NodeId, Type, Value};

pub struct Eval {
    pub feasible: bool,
    pub side: f64,
    pub cheb: f64,
}

pub trait Engine {
    /// Moves tree i and scores the layout. The layout stays moved.
    fn set(&mut self, i: usize, x: f64, y: f64, th: f64) -> Eval;
    /// Takes back the last `set`.
    fn undo(&mut self);
}

// ---------------- hand-written ----------------
pub struct Native {
    le: [(f64, f64, f64); NV],
    trees: Vec<Tree>,
    boxes: Vec<(f64, f64, f64, f64)>,
    chebs: Vec<f64>,
    cheb: f64,
    mid: (f64, f64),
    last: Option<(usize, Tree, (f64, f64, f64, f64), f64)>,
}
impl Native {
    pub fn new(layout: &[(f64, f64, f64)], mid: (f64, f64)) -> Native {
        let le = local_edges();
        let trees: Vec<Tree> = layout
            .iter()
            .map(|&(x, y, th)| Tree::new(x, y, th, &le))
            .collect();
        let boxes = trees.iter().map(Tree::bbox).collect();
        let chebs: Vec<f64> = trees
            .iter()
            .map(|t| (t.cx - mid.0).abs().max((t.cy - mid.1).abs()))
            .collect();
        let cheb = chebs.iter().sum();
        Native {
            le,
            trees,
            boxes,
            chebs,
            cheb,
            mid,
            last: None,
        }
    }
    fn side(&self) -> f64 {
        let mut b = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for q in &self.boxes {
            b.0 = b.0.min(q.0);
            b.1 = b.1.max(q.1);
            b.2 = b.2.min(q.2);
            b.3 = b.3.max(q.3);
        }
        (b.1 - b.0).max(b.3 - b.2)
    }
}
impl Engine for Native {
    fn set(&mut self, i: usize, x: f64, y: f64, th: f64) -> Eval {
        let t = Tree::new(x, y, th, &self.le);
        for (j, o) in self.trees.iter().enumerate() {
            if j != i && overlaps(&t, o) {
                self.last = None;
                return Eval {
                    feasible: false,
                    side: 0.0,
                    cheb: 0.0,
                };
            }
        }
        self.last = Some((i, self.trees[i], self.boxes[i], self.chebs[i]));
        let ch = (t.cx - self.mid.0).abs().max((t.cy - self.mid.1).abs());
        self.cheb += ch - self.chebs[i];
        self.chebs[i] = ch;
        self.boxes[i] = t.bbox();
        self.trees[i] = t;
        Eval {
            feasible: true,
            side: self.side(),
            cheb: self.cheb,
        }
    }
    fn undo(&mut self) {
        if let Some((i, t, b, ch)) = self.last.take() {
            self.cheb += ch - self.chebs[i];
            self.trees[i] = t;
            self.boxes[i] = b;
            self.chebs[i] = ch;
        }
    }
}

// ---------------- on Watershed ----------------
struct WTree {
    x: Input<f64>,
    y: Input<f64>,
    c: Input<f64>,
    s: Input<f64>,
    vx: Vec<Node<f64>>,
    vy: Vec<Node<f64>>,
    nx: Vec<Node<f64>>,
    ny: Vec<Node<f64>>,
    d: Vec<Node<f64>>,
    cx: Node<f64>,
    cy: Node<f64>,
    /// Centre of each convex piece.
    px: Vec<Node<f64>>,
    py: Vec<Node<f64>>,
}

pub struct Shed {
    pub g: Graph,
    trees: Vec<WTree>,
    cur: Vec<(f64, f64, f64)>,
    side: NodeId,
    viol: NodeId,
    cheb: NodeId,
    last: Option<(usize, (f64, f64, f64))>,
    pub recalculated: u64,
    pub updates: u64,
}

fn f(g: &Graph, id: NodeId) -> f64 {
    match g.value(id) {
        Value::F64(v) => *v,
        other => panic!("not a number: {other:?}"),
    }
}

impl Shed {
    pub fn new(layout: &[(f64, f64, f64)], mid: (f64, f64)) -> Result<Shed, CalcError> {
        let le = local_edges();
        let mut g = Graph::new();
        let yes = g.input("yes", true)?;
        let mut trees = Vec::new();
        let mut chebs = Vec::new();
        for (i, &(x0, y0, th)) in layout.iter().enumerate() {
            let x = g.input(&format!("x{i}"), x0)?;
            let y = g.input(&format!("y{i}"), y0)?;
            // Plans and formulas have no sin or cos, so the angle goes in as its cosine and sine.
            let c = g.input(&format!("c{i}"), th.cos())?;
            let s = g.input(&format!("s{i}"), th.sin())?;
            let mut t = WTree {
                x,
                y,
                c,
                s,
                vx: vec![],
                vy: vec![],
                nx: vec![],
                ny: vec![],
                d: vec![],
                cx: x.node(),
                cy: y.node(),
                px: vec![],
                py: vec![],
            };
            for k in 0..NV {
                t.vx.push(g.define(&format!("vx{i}_{k}"), x + LV[k].0 * c - LV[k].1 * s)?);
                t.vy.push(g.define(&format!("vy{i}_{k}"), y + LV[k].0 * s + LV[k].1 * c)?);
                let nx = g.define(&format!("nx{i}_{k}"), le[k].0 * c - le[k].1 * s)?;
                let ny = g.define(&format!("ny{i}_{k}"), le[k].0 * s + le[k].1 * c)?;
                t.d.push(g.define(&format!("d{i}_{k}"), le[k].2 + nx * x + ny * y)?);
                t.nx.push(nx);
                t.ny.push(ny);
            }
            t.cx = g.define(&format!("cx{i}"), x - CY * s)?;
            t.cy = g.define(&format!("cy{i}"), y + CY * c)?;
            for (k, &(h, _)) in PIECE_CIRCLES.iter().enumerate() {
                t.px.push(g.define(&format!("px{i}_{k}"), x - h * s)?);
                t.py.push(g.define(&format!("py{i}_{k}"), y + h * c)?);
            }
            chebs.push(g.define(
                &format!("cheb{i}"),
                (t.cx - mid.0).abs().max((t.cy - mid.1).abs()),
            )?);
            trees.push(t);
        }
        // One node per pair of trees: how far inside each other they are, 0 if apart.
        let mut pairs = Vec::new();
        for i in 0..trees.len() {
            for j in i + 1..trees.len() {
                let (a, b) = (&trees[i], &trees[j]);
                let mut min_sep: Option<Expr<f64>> = None;
                for (ka, &pa) in PIECES.iter().enumerate() {
                    for (kb, &pb) in PIECES.iter().enumerate() {
                        // Two pieces whose circles are apart need no further test.
                        let (ex, ey) = (a.px[ka] - b.px[kb], a.py[ka] - b.py[kb]);
                        let reach = PIECE_CIRCLES[ka].1 + PIECE_CIRCLES[kb].1 + 1e-6;
                        let close = (ex.clone() * ex + ey.clone() * ey).lt(reach * reach);
                        let sep = when(close)
                            .then(half(a, pa, b, pb).max(half(b, pb, a, pa)))
                            .otherwise(1.0);
                        min_sep = Some(match min_sep {
                            None => sep,
                            Some(m) => m.min(sep),
                        });
                    }
                }
                let (dx, dy) = (a.cx - b.cx, a.cy - b.cy);
                let near = (dx.clone() * dx + dy.clone() * dy).lt(R2);
                let depth = (MARGIN - min_sep.unwrap()).max(0.0);
                pairs.push(g.define(&format!("p{i}_{j}"), when(near).then(depth).otherwise(0.0))?);
            }
        }
        let members = |ns: Vec<NodeId>| -> Vec<(NodeId, NodeId)> {
            ns.into_iter().map(|n| (n, yes.id())).collect()
        };
        let all_x: Vec<NodeId> = trees
            .iter()
            .flat_map(|t| t.vx.iter().map(|n| n.id()))
            .collect();
        let all_y: Vec<NodeId> = trees
            .iter()
            .flat_map(|t| t.vy.iter().map(|n| n.id()))
            .collect();
        g.aggregate(
            "min_x",
            AggregateOp::Min,
            Type::F64,
            0,
            &members(all_x.clone()),
        )?;
        g.aggregate("max_x", AggregateOp::Max, Type::F64, 0, &members(all_x))?;
        g.aggregate(
            "min_y",
            AggregateOp::Min,
            Type::F64,
            0,
            &members(all_y.clone()),
        )?;
        g.aggregate("max_y", AggregateOp::Max, Type::F64, 0, &members(all_y))?;
        let viol = g.aggregate(
            "overlap",
            AggregateOp::Sum,
            Type::F64,
            0,
            &members(pairs.iter().map(|n| n.id()).collect()),
        )?;
        let cheb = g.aggregate(
            "cheb",
            AggregateOp::Sum,
            Type::F64,
            0,
            &members(chebs.iter().map(|n| n.id()).collect()),
        )?;
        let side = g.define_formula("side", "max(max_x - min_x, max_y - min_y)")?;
        g.stabilize();
        Ok(Shed {
            g,
            trees,
            cur: layout.to_vec(),
            side,
            viol,
            cheb,
            last: None,
            recalculated: 0,
            updates: 0,
        })
    }
    pub fn nodes(&self) -> usize {
        self.g.len()
    }
    fn apply(&mut self, i: usize, x: f64, y: f64, th: f64) {
        let t = &self.trees[i];
        self.g.set(t.x, x).unwrap();
        self.g.set(t.y, y).unwrap();
        self.g.set(t.c, th.cos()).unwrap();
        self.g.set(t.s, th.sin()).unwrap();
        let report = self.g.stabilize();
        self.recalculated += report.recalculated as u64;
        self.updates += 1;
        self.cur[i] = (x, y, th);
    }
}
fn half(a: &WTree, pa: (usize, usize), b: &WTree, pb: (usize, usize)) -> Expr<f64> {
    let mut best: Option<Expr<f64>> = None;
    for e in pa.0..pa.1 {
        let mut m: Option<Expr<f64>> = None;
        for v in pb.0..pb.1 {
            let dist = a.nx[e] * b.vx[v] + a.ny[e] * b.vy[v] - a.d[e];
            m = Some(match m {
                None => dist,
                Some(m) => m.min(dist),
            });
        }
        best = Some(match best {
            None => m.unwrap(),
            Some(b) => b.max(m.unwrap()),
        });
    }
    best.unwrap()
}
impl Engine for Shed {
    fn set(&mut self, i: usize, x: f64, y: f64, th: f64) -> Eval {
        self.last = Some((i, self.cur[i]));
        self.apply(i, x, y, th);
        Eval {
            feasible: f(&self.g, self.viol) == 0.0,
            side: f(&self.g, self.side),
            cheb: f(&self.g, self.cheb),
        }
    }
    fn undo(&mut self) {
        if let Some((i, (x, y, th))) = self.last.take() {
            self.apply(i, x, y, th);
        }
    }
}
