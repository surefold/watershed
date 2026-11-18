//! A plan written to a file and read back, so that another process or
//! another machine can run it.
//!
//! The layout is this build's own: a plan is read back by the version of the
//! crate that wrote it, and the first eight bytes say which. What is read is
//! checked for holding together before it is run (see `check`).

use std::io::{self, Read, Write};

use super::{Group, Output, Scenarios};
use crate::fuse::Fused;

/// The first bytes of a plan's file. The last is the layout's version: 3
/// since plans could hold totals, whose instructions an earlier crate does
/// not know, so it refuses the file rather than misread it.
const MAGIC: [u8; 8] = *b"WSPLAN\0\x03";
/// The same layout, from before totals: read, and never written.
const BEFORE_TOTALS: [u8; 8] = *b"WSPLAN\0\x02";
/// The layout before clusters were written: read, and never written.
const BEFORE_CLUSTERS: [u8; 8] = *b"WSPLAN\0\x01";
/// A node of a clustered plan that is not written to a slot.
const NO_SLOT: u32 = u32::MAX;

/// Why a plan could not be read.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be read to its end.
    Io(io::Error),
    /// The file is not a plan written by this version of the crate.
    NotAPlan,
    /// The file was cut short or changed after it was written.
    Damaged,
    /// The file is whole, but what it holds does not make a plan: it was not
    /// written by `save`. It is refused rather than run.
    NotWhole(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "the plan could not be read: {e}"),
            LoadError::NotAPlan => write!(f, "not a plan written by this version of watershed"),
            LoadError::Damaged => {
                write!(f, "the plan was cut short or changed after it was written")
            }
            LoadError::NotWhole(why) => write!(f, "the file does not hold a plan: {why}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<io::Error> for LoadError {
    fn from(e: io::Error) -> LoadError {
        match e.kind() {
            io::ErrorKind::UnexpectedEof => LoadError::Damaged,
            _ => LoadError::Io(e),
        }
    }
}

/// Bytes on their way out or in, with a running sum of them (FNV-1a) that is
/// written last and checked on the way in.
struct Summed<T> {
    inner: T,
    sum: u64,
}

impl<T> Summed<T> {
    fn new(inner: T) -> Summed<T> {
        Summed {
            inner,
            sum: 0xcbf2_9ce4_8422_2325,
        }
    }
    fn add(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.sum = (self.sum ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl<W: Write> Summed<W> {
    fn put(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.add(bytes);
        self.inner.write_all(bytes)
    }
    fn count(&mut self, n: usize) -> io::Result<()> {
        self.put(&(n as u64).to_le_bytes())
    }
    fn u32s(&mut self, values: &[u32]) -> io::Result<()> {
        self.count(values.len())?;
        let mut bytes = Vec::with_capacity(CHUNK * 4);
        for chunk in values.chunks(CHUNK) {
            bytes.clear();
            chunk
                .iter()
                .for_each(|v| bytes.extend_from_slice(&v.to_le_bytes()));
            self.put(&bytes)?;
        }
        Ok(())
    }
    fn shapes(&mut self, shapes: &[Vec<u32>]) -> io::Result<()> {
        self.count(shapes.len())?;
        shapes.iter().try_for_each(|shape| self.u32s(shape))
    }
    fn groups(&mut self, groups: &[Group]) -> io::Result<()> {
        let groups: Vec<u32> = groups
            .iter()
            .flat_map(|g| [g.shape, g.first, g.count])
            .collect();
        self.u32s(&groups)
    }
    fn outputs(&mut self, outputs: &[Output]) -> io::Result<()> {
        self.count(outputs.len())?;
        for output in outputs {
            let (kind, bits) = match *output {
                Output::Slot(slot) => (0u8, u64::from(slot)),
                Output::Shared(at) => (1, u64::from(at)),
                Output::Fixed(value) => (2, value.to_bits()),
            };
            self.put(&[kind])?;
            self.put(&bits.to_le_bytes())?;
        }
        Ok(())
    }
    fn f64s(&mut self, values: &[f64]) -> io::Result<()> {
        self.count(values.len())?;
        let mut bytes = Vec::with_capacity(CHUNK * 8);
        for chunk in values.chunks(CHUNK) {
            bytes.clear();
            chunk
                .iter()
                .for_each(|v| bytes.extend_from_slice(&v.to_le_bytes()));
            self.put(&bytes)?;
        }
        Ok(())
    }
}

/// How many numbers are turned to bytes at a time.
const CHUNK: usize = 1 << 16;

impl<R: Read> Summed<R> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], LoadError> {
        let mut bytes = [0; N];
        self.inner.read_exact(&mut bytes)?;
        self.add(&bytes);
        Ok(bytes)
    }
    fn a_count(&mut self) -> Result<usize, LoadError> {
        usize::try_from(u64::from_le_bytes(self.take()?)).map_err(|_| LoadError::Damaged)
    }
    /// `n` numbers of `width` bytes each, as bytes. Nothing is set aside
    /// before the bytes have arrived, so a damaged count cannot ask for
    /// more memory than the file is long.
    fn bytes(&mut self, n: usize, width: usize) -> Result<Vec<u8>, LoadError> {
        let wanted = n.checked_mul(width).ok_or(LoadError::Damaged)?;
        let mut bytes = Vec::new();
        (&mut self.inner)
            .take(wanted as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() != wanted {
            return Err(LoadError::Damaged);
        }
        self.add(&bytes);
        Ok(bytes)
    }
    fn some_u32s(&mut self) -> Result<Vec<u32>, LoadError> {
        let n = self.a_count()?;
        let bytes = self.bytes(n, 4)?;
        Ok(bytes
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    }
    fn some_shapes(&mut self) -> Result<Vec<Vec<u32>>, LoadError> {
        (0..self.a_count()?).map(|_| self.some_u32s()).collect()
    }
    fn some_groups(&mut self) -> Result<Vec<Group>, LoadError> {
        Ok(self
            .some_u32s()?
            .chunks_exact(3)
            .map(|g| Group {
                shape: g[0],
                first: g[1],
                count: g[2],
            })
            .collect())
    }
    fn some_outputs(&mut self) -> Result<Vec<Output>, LoadError> {
        (0..self.a_count()?)
            .map(|_| {
                let [kind] = self.take()?;
                let bits = u64::from_le_bytes(self.take()?);
                let at = u32::try_from(bits).map_err(|_| LoadError::Damaged);
                Ok(match kind {
                    0 => Output::Slot(at?),
                    1 => Output::Shared(at?),
                    2 => Output::Fixed(f64::from_bits(bits)),
                    _ => return Err(LoadError::Damaged),
                })
            })
            .collect()
    }
    fn some_f64s(&mut self) -> Result<Vec<f64>, LoadError> {
        let n = self.a_count()?;
        let bytes = self.bytes(n, 8)?;
        Ok(bytes
            .chunks_exact(8)
            .map(|b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
            .collect())
    }
}

impl Scenarios {
    /// Writes the plan out, to be read back by [`Scenarios::load`].
    ///
    /// The shared inputs' values go with it as they are now.
    /// A plan made with [`Scenarios::with_clusters`] is written with its
    /// clusters, and read back runs with them.
    /// A plan of 20 million nodes is about 2 GB; give this a
    /// `BufWriter`.
    pub fn save(&self, to: impl Write) -> io::Result<()> {
        let mut to = Summed::new(to);
        to.inner.write_all(&MAGIC)?;
        to.count(self.vary)?;
        to.count(self.slots)?;
        to.count(self.shared_inputs)?;
        to.u32s(&self.programs)?;
        to.u32s(&self.ops)?;
        to.f64s(&self.consts)?;
        let levels: Vec<u32> = self.levels.iter().flat_map(|l| [l.start, l.end]).collect();
        to.u32s(&levels)?;
        to.shapes(&self.shapes)?;
        to.groups(&self.groups)?;
        to.u32s(&self.operands)?;
        to.u32s(&self.operand_at)?;
        to.outputs(&self.outputs)?;
        to.f64s(&self.shared)?;
        to.u32s(&self.shared_programs)?;
        to.u32s(&self.shared_ops)?;
        match &self.fused {
            None => to.put(&[0])?,
            Some(fused) => {
                to.put(&[1])?;
                to.count(fused.slots)?;
                to.shapes(&fused.shapes)?;
                to.groups(&fused.groups)?;
                to.u32s(&fused.operand_at)?;
                to.u32s(&fused.operands)?;
                to.count(fused.widest)?;
                let slot_of: Vec<u32> = fused
                    .slot_of
                    .iter()
                    .map(|slot| slot.unwrap_or(NO_SLOT))
                    .collect();
                to.u32s(&slot_of)?;
                to.outputs(&self.fused_outputs)?;
            }
        }
        let sum = to.sum;
        to.inner.write_all(&sum.to_le_bytes())?;
        to.inner.flush()
    }

    /// Reads a plan written by [`Scenarios::save`]. It runs as the plan
    /// that was saved does, to the last bit.
    ///
    /// The file is checked for being whole and unchanged, and
    /// what it holds for making a plan: every number that points somewhere
    /// points inside what it points into, and every program is well formed,
    /// so a file not written by `save` is refused rather than run.
    pub fn load(from: impl Read) -> Result<Scenarios, LoadError> {
        let mut from = Summed::new(from);
        let mut magic = [0; 8];
        from.inner
            .read_exact(&mut magic)
            .map_err(|_| LoadError::NotAPlan)?;
        if magic != MAGIC && magic != BEFORE_TOTALS && magic != BEFORE_CLUSTERS {
            return Err(LoadError::NotAPlan);
        }
        let vary = from.a_count()?;
        let slots = from.a_count()?;
        let shared_inputs = from.a_count()?;
        let programs = from.some_u32s()?;
        let ops = from.some_u32s()?;
        let consts = from.some_f64s()?;
        let levels = from
            .some_u32s()?
            .chunks_exact(2)
            .map(|l| l[0]..l[1])
            .collect();
        let shapes = from.some_shapes()?;
        let groups = from.some_groups()?;
        let operands = from.some_u32s()?;
        let operand_at = from.some_u32s()?;
        let outputs = from.some_outputs()?;
        let shared = from.some_f64s()?;
        let shared_programs = from.some_u32s()?;
        let shared_ops = from.some_u32s()?;
        let clustered = match magic != BEFORE_CLUSTERS {
            true => from.take::<1>()?[0],
            false => 0,
        };
        let (fused, fused_outputs) = match clustered {
            0 => (None, Vec::new()),
            1 => {
                let fused = Fused {
                    slots: from.a_count()?,
                    shapes: from.some_shapes()?,
                    groups: from.some_groups()?,
                    operand_at: from.some_u32s()?,
                    operands: from.some_u32s()?,
                    widest: from.a_count()?,
                    slot_of: from
                        .some_u32s()?
                        .into_iter()
                        .map(|slot| (slot != NO_SLOT).then_some(slot))
                        .collect(),
                };
                (Some(fused), from.some_outputs()?)
            }
            _ => return Err(LoadError::Damaged),
        };
        let sum = from.sum;
        let mut written = [0; 8];
        from.inner.read_exact(&mut written)?;
        if u64::from_le_bytes(written) != sum {
            return Err(LoadError::Damaged);
        }
        let plan = Scenarios {
            vary,
            slots,
            programs,
            ops,
            consts,
            levels,
            shapes,
            groups,
            operands,
            operand_at,
            fused,
            fused_outputs,
            outputs,
            shared_inputs,
            shared,
            shared_programs,
            shared_ops,
        };
        plan.check().map_err(LoadError::NotWhole)?;
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PlanError;
    use watershed_core::NodeId;
    use watershed_core::prelude::*;

    /// Varied and shared inputs, a condition, a constant output and one
    /// that depends only on a shared input.
    fn plan() -> Scenarios {
        made_by(Scenarios::with_shared)
    }

    type Make = fn(&Graph, &[NodeId], &[NodeId], &[NodeId]) -> Result<Scenarios, PlanError>;

    fn made_by(make: Make) -> Scenarios {
        let mut g = Graph::new();
        let rate = g.input("rate", 0.01).unwrap();
        let shock = g.input("shock", 0.0).unwrap();
        let fee = g.input("fee", 0.002).unwrap();
        let mut worth = Vec::new();
        for n in 0..12 {
            let mut value = g
                .define(&format!("v{n}_0"), (1.0 + rate) * (100.0 + n as f64))
                .unwrap();
            for t in 1..5 {
                value = g
                    .define(&format!("v{n}_{t}"), value * (1.0 + rate - fee) - shock)
                    .unwrap();
            }
            let floor = 95.0 + n as f64;
            worth.push(
                g.define(
                    &format!("worth{n}"),
                    when(value.gt(floor)).then(value).otherwise(floor - shock),
                )
                .unwrap(),
            );
        }
        let mut all = worth[0].into_expr();
        for &x in &worth[1..] {
            all = all + x;
        }
        let total = g.define("total", all).unwrap();
        let yearly = g.define("yearly", fee * 12.0).unwrap();
        let fixed = g.define("fixed", (2.0f64).into_expr() * 3.5).unwrap();
        g.stabilize();
        make(
            &g,
            &[rate.id(), shock.id()],
            &[fee.id()],
            &[total.id(), yearly.id(), fixed.id(), worth[3].id()],
        )
        .unwrap()
    }

    fn inputs() -> Vec<f64> {
        (0..200)
            .flat_map(|s| [0.003 * f64::from(s) - 0.2, 0.7 * f64::from(s % 9)])
            .collect()
    }

    #[test]
    fn a_plan_read_back_gives_what_the_plan_gives() {
        let mut plan = plan();
        plan.set_shared(&[0.004]);
        let mut file = Vec::new();
        plan.save(&mut file).unwrap();
        let mut back = Scenarios::load(&file[..]).unwrap();
        let (was, now) = (plan.run_cpu(&inputs()), back.run_cpu(&inputs()));
        assert!(
            was.iter()
                .zip(&now)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
        assert_eq!(was.len(), now.len());

        // Written out again it is the same file.
        let mut again = Vec::new();
        back.save(&mut again).unwrap();
        assert!(again == file);

        // And it is still a whole plan: its shared inputs can be set.
        plan.set_shared(&[0.01]);
        back.set_shared(&[0.01]);
        let (was, now) = (plan.run_cpu(&inputs()), back.run_cpu(&inputs()));
        assert!(
            was.iter()
                .zip(&now)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }

    #[test]
    fn a_clustered_plan_is_read_back_with_its_clusters() {
        let plan = made_by(Scenarios::with_clusters);
        let mut file = Vec::new();
        plan.save(&mut file).unwrap();
        let back = Scenarios::load(&file[..]).unwrap();
        let (was, now) = (plan.fused.as_ref().unwrap(), back.fused.as_ref().unwrap());
        assert_eq!(was.slots, now.slots);
        assert_eq!(was.widest, now.widest);
        assert_eq!(was.slot_of, now.slot_of);
        let mut again = Vec::new();
        back.save(&mut again).unwrap();
        assert!(again == file);
        // It has more in it than the plain plan's file.
        let mut plain = Vec::new();
        self::plan().save(&mut plain).unwrap();
        assert!(file.len() > plain.len());
    }

    #[test]
    fn a_file_written_before_clusters_were_is_still_read() {
        let plan = plan();
        let mut file = Vec::new();
        plan.save(&mut file).unwrap();
        // The older layout: its own first bytes, no mark for clusters, and
        // the sum of what is left.
        let body = &file[8..file.len() - 9];
        let mut summed = Summed::new(());
        summed.add(body);
        let old = [&BEFORE_CLUSTERS[..], body, &summed.sum.to_le_bytes()].concat();
        let back = Scenarios::load(&old[..]).unwrap();
        assert!(back.fused.is_none());
        let (was, now) = (plan.run_cpu(&inputs()), back.run_cpu(&inputs()));
        assert!(
            was.iter()
                .zip(&now)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }

    #[test]
    fn a_file_written_before_totals_were_is_still_read() {
        let plan = plan();
        let mut file = Vec::new();
        plan.save(&mut file).unwrap();
        // The same layout under its older first bytes; the sum is of what
        // follows them, so it stands.
        let old = [&BEFORE_TOTALS[..], &file[8..]].concat();
        let back = Scenarios::load(&old[..]).unwrap();
        let (was, now) = (plan.run_cpu(&inputs()), back.run_cpu(&inputs()));
        assert!(
            was.iter()
                .zip(&now)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }

    #[test]
    fn a_damaged_file_is_refused() {
        let mut file = Vec::new();
        plan().save(&mut file).unwrap();
        assert!(matches!(
            Scenarios::load(&b"not a plan at all"[..]),
            Err(LoadError::NotAPlan)
        ));
        assert!(matches!(
            Scenarios::load(&file[..file.len() / 2]),
            Err(LoadError::Damaged)
        ));
        let mut changed = file.clone();
        let middle = changed.len() / 2;
        changed[middle] ^= 1;
        assert!(matches!(
            Scenarios::load(&changed[..]),
            Err(LoadError::Damaged)
        ));
    }
}

#[cfg(test)]
mod not_whole {
    use super::*;
    use crate::plan::{CODE, CODE_BITS, LOAD, Output, SELECT};
    use watershed_core::prelude::*;

    /// A plan with every part: levels, groups, shared values, a cluster
    /// layout if `clusters`.
    fn plan(clusters: bool) -> Scenarios {
        let mut g = Graph::new();
        let rate = g.input("rate", 0.01).unwrap();
        let shock = g.input("shock", 0.0).unwrap();
        let fee = g.input("fee", 0.002).unwrap();
        let yearly = g.define("yearly", fee * 12.0).unwrap();
        let mut sum = (rate * 1.0).into_expr();
        for n in 0..8 {
            let mut v = g
                .define(&format!("v{n}"), rate * (n as f64) + yearly)
                .unwrap();
            for t in 0..3 {
                v = g
                    .define(
                        &format!("v{n}_{t}"),
                        when(v.gt(shock)).then(v - fee).otherwise(v + 1.0),
                    )
                    .unwrap();
            }
            sum = sum + v;
        }
        let total = g.define("total", sum).unwrap();
        g.stabilize();
        let make = match clusters {
            true => Scenarios::with_clusters,
            false => Scenarios::with_shared,
        };
        make(
            &g,
            &[rate.id(), shock.id()],
            &[fee.id()],
            &[total.id(), yearly.id()],
        )
        .unwrap()
    }

    /// The plan written out and read back after `change`, with the sum made
    /// right again, so that only the check can tell.
    fn read_after(
        clusters: bool,
        change: impl FnOnce(&mut Scenarios),
    ) -> Result<Scenarios, LoadError> {
        let mut plan = plan(clusters);
        change(&mut plan);
        let mut file = Vec::new();
        plan.save(&mut file).unwrap();
        Scenarios::load(&file[..])
    }

    fn refused(what: &str, clusters: bool, change: impl FnOnce(&mut Scenarios)) {
        match read_after(clusters, change) {
            Err(LoadError::NotWhole(_)) => {}
            other => panic!("{what}: {other:?}"),
        }
    }

    #[test]
    fn a_whole_plan_is_read_back() {
        assert!(read_after(false, |_| {}).is_ok());
        assert!(read_after(true, |_| {}).is_ok());
    }

    #[test]
    fn a_file_whose_numbers_point_outside_is_refused() {
        let first_load = |p: &Scenarios| p.ops.iter().position(|op| op & CODE == LOAD).unwrap();
        refused("a load past the slots", false, |p| {
            let at = first_load(p);
            p.ops[at] = LOAD | (p.slots as u32 + 5) << CODE_BITS;
        });
        refused("a constant past the constants", false, |p| {
            p.consts.clear();
        });
        refused("a node writing a varied input", false, |p| {
            p.programs[2] = 0
        });
        refused("a program past the instructions", false, |p| {
            p.programs[1] += 1_000
        });
        refused("an unknown instruction", false, |p| {
            let last = p.ops.len() - 1;
            p.ops[last] = 31;
        });
        refused("a stack that runs dry", false, |p| {
            let start = p.programs[0] as usize;
            p.ops[start] = SELECT;
        });
        refused("operands that are not what is read", false, |p| {
            p.operands[0] = p.operands[0].wrapping_add(1);
        });
        refused("levels that leave a gap", false, |p| p.levels[0].end -= 1);
        refused("a group of no shape", false, |p| p.groups[0].shape = 999);
        refused("an output past the slots", false, |p| {
            p.outputs[0] = Output::Slot(p.slots as u32);
        });
        refused("a shared program reading a slot", false, |p| {
            p.shared_ops[0] = LOAD;
        });
        refused("a cluster reading past its slots", true, |p| {
            let fused = p.fused.as_mut().unwrap();
            let last = fused.operands.len() - 1;
            fused.operands[last] = fused.slots as u32;
        });
        refused("a cluster's node reading a later one", true, |p| {
            let fused = p.fused.as_mut().unwrap();
            let shape = fused.shapes.iter_mut().find(|s| s.len() > 6).unwrap();
            shape[2] = crate::fuse::LOCAL | 99 << CODE_BITS;
        });
    }

    #[test]
    fn a_file_changed_anywhere_is_refused_or_runs() {
        // Random words of the file changed, and the sum made right again: the
        // plan read back is refused, or runs without failing.
        for clusters in [false, true] {
            let mut file = Vec::new();
            plan(clusters).save(&mut file).unwrap();
            let body = file.len() - 8;
            let mut rng = 0x2545_F491_4F6C_DD1Du64;
            let mut next = || {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                rng
            };
            let mut ran = 0;
            for _ in 0..3000 {
                let mut changed = file.clone();
                for _ in 0..1 + next() % 3 {
                    let at = 8 + (next() as usize % ((body - 8) / 4)) * 4;
                    let word = match next() % 3 {
                        0 => (next() % 64) as u32,
                        1 => u32::MAX - (next() % 4) as u32,
                        _ => next() as u32,
                    };
                    changed[at..at + 4].copy_from_slice(&word.to_le_bytes());
                }
                let mut sum = Summed::new(());
                sum.add(&changed[8..body]);
                changed[body..].copy_from_slice(&sum.sum.to_le_bytes());
                if let Ok(plan) = Scenarios::load(&changed[..]) {
                    // A plan read back can still ask a lot of every scenario
                    // (as a real one with as many inputs would); here, one.
                    let scenario = vec![0.25; plan.inputs()];
                    plan.run_cpu(&scenario);
                    ran += 1;
                }
            }
            assert!(ran < 3000, "every change was read back");
        }
    }
}
