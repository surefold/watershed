//! A plan written to a file and read back, so that another process or
//! another machine can run it.
//!
//! EXPERIMENTAL. The layout is this build's own: a plan is read back by the
//! version of the crate that wrote it, and the first eight bytes say which.

use std::io::{self, Read, Write};

use super::{Group, Output, Scenarios};

/// The first bytes of a plan's file. The last is the layout's version.
const MAGIC: [u8; 8] = *b"WSPLAN\0\x01";

/// Why a plan could not be read.
#[derive(Debug)]
pub enum LoadError {
    /// The file could not be read to its end.
    Io(io::Error),
    /// The file is not a plan written by this version of the crate.
    NotAPlan,
    /// The file was cut short or changed after it was written.
    Damaged,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "the plan could not be read: {e}"),
            LoadError::NotAPlan => write!(f, "not a plan written by this version of watershed"),
            LoadError::Damaged => {
                write!(f, "the plan was cut short or changed after it was written")
            }
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
    /// EXPERIMENTAL. The shared inputs' values go with it as they are now.
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
        to.count(self.shapes.len())?;
        for shape in &self.shapes {
            to.u32s(shape)?;
        }
        let groups: Vec<u32> = self
            .groups
            .iter()
            .flat_map(|g| [g.shape, g.first, g.count])
            .collect();
        to.u32s(&groups)?;
        to.u32s(&self.operands)?;
        to.u32s(&self.operand_at)?;
        to.count(self.outputs.len())?;
        for output in &self.outputs {
            let (kind, bits) = match *output {
                Output::Slot(slot) => (0u8, u64::from(slot)),
                Output::Shared(at) => (1, u64::from(at)),
                Output::Fixed(value) => (2, value.to_bits()),
            };
            to.put(&[kind])?;
            to.put(&bits.to_le_bytes())?;
        }
        to.f64s(&self.shared)?;
        to.u32s(&self.shared_programs)?;
        to.u32s(&self.shared_ops)?;
        let sum = to.sum;
        to.inner.write_all(&sum.to_le_bytes())?;
        to.inner.flush()
    }

    /// Reads a plan written by [`Scenarios::save`]. It runs as the plan
    /// that was saved does, to the last bit.
    ///
    /// EXPERIMENTAL. The file is checked for being whole and unchanged, not
    /// for being well meant: load only plans you saved.
    pub fn load(from: impl Read) -> Result<Scenarios, LoadError> {
        let mut from = Summed::new(from);
        let mut magic = [0; 8];
        from.inner
            .read_exact(&mut magic)
            .map_err(|_| LoadError::NotAPlan)?;
        if magic != MAGIC {
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
        let shapes = (0..from.a_count()?)
            .map(|_| from.some_u32s())
            .collect::<Result<Vec<_>, _>>()?;
        let groups = from
            .some_u32s()?
            .chunks_exact(3)
            .map(|g| Group {
                shape: g[0],
                first: g[1],
                count: g[2],
            })
            .collect();
        let operands = from.some_u32s()?;
        let operand_at = from.some_u32s()?;
        let outputs = (0..from.a_count()?)
            .map(|_| {
                let [kind] = from.take()?;
                let bits = u64::from_le_bytes(from.take()?);
                let at = u32::try_from(bits).map_err(|_| LoadError::Damaged);
                Ok(match kind {
                    0 => Output::Slot(at?),
                    1 => Output::Shared(at?),
                    2 => Output::Fixed(f64::from_bits(bits)),
                    _ => return Err(LoadError::Damaged),
                })
            })
            .collect::<Result<Vec<_>, LoadError>>()?;
        let shared = from.some_f64s()?;
        let shared_programs = from.some_u32s()?;
        let shared_ops = from.some_u32s()?;
        let sum = from.sum;
        let mut written = [0; 8];
        from.inner.read_exact(&mut written)?;
        if u64::from_le_bytes(written) != sum {
            return Err(LoadError::Damaged);
        }
        Ok(Scenarios {
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
            outputs,
            shared_inputs,
            shared,
            shared_programs,
            shared_ops,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use watershed_core::prelude::*;

    /// Varied and shared inputs, a condition, a constant output and one
    /// that depends only on a shared input.
    fn plan() -> Scenarios {
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
        Scenarios::with_shared(
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
