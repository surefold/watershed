//! The Santa 2025 tree, as four convex pieces, and a hand-written overlap test.

pub const NV: usize = 15;
/// Local vertices, piece by piece, each piece counter-clockwise.
pub const LV: [(f64, f64); NV] = [
    (-0.125, 0.5),
    (0.125, 0.5),
    (0.0, 0.8),
    (-0.2, 0.25),
    (0.2, 0.25),
    (0.0625, 0.5),
    (-0.0625, 0.5),
    (-0.35, 0.0),
    (0.35, 0.0),
    (0.1, 0.25),
    (-0.1, 0.25),
    (-0.075, -0.2),
    (0.075, -0.2),
    (0.075, 0.0),
    (-0.075, 0.0),
];
pub const PIECES: [(usize, usize); 4] = [(0, 3), (3, 7), (7, 11), (11, 15)];
/// Each piece's circle: height of its centre on the tree's axis, and radius.
pub const PIECE_CIRCLES: [(f64, f64); 4] = [
    (0.65, 0.1953),
    (0.375, 0.2359),
    (0.125, 0.3717),
    (-0.1, 0.1251),
];
/// Centre used for the distance gate, in local coordinates: (0, CY).
pub const CY: f64 = 0.3;
/// Two trees whose centres are further apart than sqrt(R2) cannot touch.
pub const R2: f64 = 1.03;
/// Pieces must be separated by at least this much.
pub const MARGIN: f64 = 1e-8;

/// Local unit outward normal and offset of edge k (from vertex k to the next in its piece).
pub fn local_edges() -> [(f64, f64, f64); NV] {
    let mut out = [(0.0, 0.0, 0.0); NV];
    for &(a, b) in &PIECES {
        for k in a..b {
            let next = if k + 1 == b { a } else { k + 1 };
            let (ex, ey) = (LV[next].0 - LV[k].0, LV[next].1 - LV[k].1);
            let len = (ex * ex + ey * ey).sqrt();
            let (nx, ny) = (ey / len, -ex / len);
            out[k] = (nx, ny, nx * LV[k].0 + ny * LV[k].1);
        }
    }
    out
}

#[derive(Clone, Copy)]
pub struct Tree {
    pub vx: [f64; NV],
    pub vy: [f64; NV],
    pub nx: [f64; NV],
    pub ny: [f64; NV],
    pub d: [f64; NV],
    pub cx: f64,
    pub cy: f64,
}

impl Tree {
    pub fn new(x: f64, y: f64, th: f64, le: &[(f64, f64, f64); NV]) -> Tree {
        let (s, c) = th.sin_cos();
        let mut t = Tree {
            vx: [0.0; NV],
            vy: [0.0; NV],
            nx: [0.0; NV],
            ny: [0.0; NV],
            d: [0.0; NV],
            cx: x - CY * s,
            cy: y + CY * c,
        };
        for k in 0..NV {
            t.vx[k] = x + LV[k].0 * c - LV[k].1 * s;
            t.vy[k] = y + LV[k].0 * s + LV[k].1 * c;
            t.nx[k] = le[k].0 * c - le[k].1 * s;
            t.ny[k] = le[k].0 * s + le[k].1 * c;
            t.d[k] = le[k].2 + t.nx[k] * x + t.ny[k] * y;
        }
        t
    }
    pub fn bbox(&self) -> (f64, f64, f64, f64) {
        let mut b = (
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        );
        for k in 0..NV {
            b.0 = b.0.min(self.vx[k]);
            b.1 = b.1.max(self.vx[k]);
            b.2 = b.2.min(self.vy[k]);
            b.3 = b.3.max(self.vy[k]);
        }
        b
    }
}

#[inline]
fn half_sep(a: &Tree, pa: (usize, usize), b: &Tree, pb: (usize, usize)) -> f64 {
    let mut best = f64::NEG_INFINITY;
    for e in pa.0..pa.1 {
        let mut m = f64::INFINITY;
        for v in pb.0..pb.1 {
            m = m.min(a.nx[e] * b.vx[v] + a.ny[e] * b.vy[v] - a.d[e]);
        }
        best = best.max(m);
    }
    best
}

/// True if the two trees' interiors come closer than MARGIN.
#[inline]
pub fn overlaps(a: &Tree, b: &Tree) -> bool {
    let (dx, dy) = (a.cx - b.cx, a.cy - b.cy);
    if dx * dx + dy * dy >= R2 {
        return false;
    }
    for &pa in &PIECES {
        for &pb in &PIECES {
            let sep = half_sep(a, pa, b, pb).max(half_sep(b, pb, a, pa));
            if sep < MARGIN {
                return true;
            }
        }
    }
    false
}

pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn gauss(&mut self) -> f64 {
        let u = self.unit().max(1e-300);
        let v = self.unit();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}
