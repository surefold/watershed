//! Runs a [`Scenarios`] plan on an NVIDIA GPU.
//!
//! The GPU holds one table of `f64`: a row per slot and a column per
//! scenario. The varied inputs are copied up, one kernel launch works out
//! each level for every scenario at once, and only the outputs' rows are
//! copied back. When the table for every scenario would not fit in the GPU's
//! memory, the scenarios are run a part at a time.

use std::fmt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, DriverError, LaunchConfig, PushKernelArg,
};
use rayon::prelude::*;

use cudarc::nvrtc::{CompileError, CompileOptions, compile_ptx_with_opts};

use crate::plan::{Output, Scenarios};

const THREADS_PER_BLOCK: u32 = 256;

/// Why a GPU run failed.
#[derive(Debug)]
pub enum GpuError {
    /// The CUDA driver refused something.
    Driver(DriverError),
    /// The kernel did not compile.
    Compile(CompileError),
    /// Not even one scenario fits in the memory allowed.
    TooLittleMemory { one_scenario: usize, allowed: usize },
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::Driver(e) => write!(f, "CUDA driver: {e}"),
            GpuError::Compile(e) => write!(f, "compiling the kernel: {e}"),
            GpuError::TooLittleMemory {
                one_scenario,
                allowed,
            } => write!(
                f,
                "one scenario needs {one_scenario} bytes on the GPU and only {allowed} are allowed"
            ),
        }
    }
}

impl std::error::Error for GpuError {}

impl From<DriverError> for GpuError {
    fn from(e: DriverError) -> Self {
        GpuError::Driver(e)
    }
}

impl From<CompileError> for GpuError {
    fn from(e: CompileError) -> Self {
        GpuError::Compile(e)
    }
}

/// Where the time of a run went.
#[derive(Debug, Clone, Copy, Default)]
pub struct Timing {
    /// Copying the plan and the inputs to the GPU.
    pub upload: Duration,
    /// Working out every level.
    pub compute: Duration,
    /// Copying the outputs back.
    pub download: Duration,
    /// How many scenarios were run at a time.
    pub scenarios_at_a_time: usize,
}

/// An NVIDIA GPU, with the kernel compiled and ready.
pub struct Gpu {
    ctx: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    level: CudaFunction,
}

impl Gpu {
    /// Opens the first GPU and compiles the kernel for it.
    ///
    /// # Panics
    /// If the CUDA libraries are not installed: they are looked for when
    /// this is called, not when the program is built.
    pub fn new() -> Result<Gpu, GpuError> {
        let ctx = CudaContext::new(0)?;
        // Fused multiply-add off: with it on, `a * b + c` is rounded once
        // where the CPU rounds twice, and the answers differ in the last bit.
        let options = CompileOptions {
            fmad: Some(false),
            ..Default::default()
        };
        let ptx = compile_ptx_with_opts(include_str!("kernel.cu"), options)?;
        let module = ctx.load_module(ptx)?;
        let level = module.load_function("level")?;
        let stream = ctx.default_stream();
        Ok(Gpu { ctx, stream, level })
    }

    /// The GPU's memory that is free now, in bytes.
    pub fn free_memory(&self) -> Result<usize, GpuError> {
        Ok(self.ctx.mem_get_info()?.0)
    }

    /// Runs every scenario, using up to four fifths of the GPU's free
    /// memory. See [`Scenarios::run_cpu`] for the layout of `inputs` and of
    /// the result, which is the same.
    ///
    /// # Panics
    /// If the length of `inputs` is not a whole number of scenarios.
    pub fn run(&self, plan: &Scenarios, inputs: &[f64]) -> Result<Vec<f64>, GpuError> {
        let allowed = self.free_memory()? / 5 * 4;
        Ok(self.run_within(plan, inputs, allowed)?.0)
    }

    /// As [`run`](Self::run), using at most `allowed` bytes of the GPU's
    /// memory, and saying where the time went.
    pub fn run_within(
        &self,
        plan: &Scenarios,
        inputs: &[f64],
        allowed: usize,
    ) -> Result<(Vec<f64>, Timing), GpuError> {
        let scenarios = plan.scenarios_in(inputs);
        let mut timing = Timing::default();
        if scenarios == 0 || plan.nodes() == 0 {
            // Nothing to calculate: every output is an input or a constant.
            return Ok((plan.run_cpu(inputs), timing));
        }
        let (vary, outputs) = (plan.vary, plan.outputs.len());

        // How many scenarios fit at once: in memory, and in one launch,
        // which can have at most 2^31 - 1 blocks of threads.
        let fixed = size_of_val(&plan.ops[..])
            + size_of_val(&plan.programs[..])
            + size_of_val(&plan.consts[..])
            + size_of_val(&plan.shared[..]);
        let widest = plan.levels.iter().map(|l| l.len()).max().unwrap_or(1);
        let most_threads = (i32::MAX as usize) * THREADS_PER_BLOCK as usize;
        let at_a_time = (allowed.saturating_sub(fixed) / plan.bytes_per_scenario())
            .min(most_threads / widest)
            .min(scenarios);
        if at_a_time == 0 {
            return Err(GpuError::TooLittleMemory {
                one_scenario: fixed + plan.bytes_per_scenario(),
                allowed,
            });
        }
        timing.scenarios_at_a_time = at_a_time;

        let started = Instant::now();
        let ops = self.stream.clone_htod(&plan.ops)?;
        let programs = self.stream.clone_htod(&plan.programs)?;
        // An empty allocation is refused, and a plan may have no constants.
        let or_one = |values: &[f64]| match values.is_empty() {
            true => self.stream.clone_htod(&[0.0f64]),
            false => self.stream.clone_htod(values),
        };
        let consts = or_one(&plan.consts)?;
        let shared = or_one(&plan.shared)?;
        let mut vals = self.stream.alloc_zeros::<f64>(plan.slots() * at_a_time)?;
        self.stream.synchronize()?;
        timing.upload += started.elapsed();

        let mut results = vec![0.0; scenarios * outputs];
        let mut columns = vec![0.0; vary * at_a_time];
        let mut back = vec![0.0; outputs * at_a_time];
        for first in (0..scenarios).step_by(at_a_time) {
            let count = at_a_time.min(scenarios - first);

            // The inputs arrive a scenario at a time; the GPU wants them an
            // input at a time.
            let started = Instant::now();
            let rows = &inputs[first * vary..(first + count) * vary];
            columns[..vary * count]
                .par_chunks_mut(count)
                .enumerate()
                .for_each(|(v, column)| {
                    for (s, x) in column.iter_mut().enumerate() {
                        *x = rows[s * vary + v];
                    }
                });
            self.stream.memcpy_htod(
                &columns[..vary * count],
                &mut vals.slice_mut(..vary * count),
            )?;
            self.stream.synchronize()?;
            timing.upload += started.elapsed();

            let started = Instant::now();
            for level in &plan.levels {
                self.launch(
                    &ops,
                    &programs,
                    &consts,
                    &shared,
                    &mut vals,
                    level.start,
                    level.len() as u32,
                    vary as u32,
                    count as u32,
                )?;
            }
            self.stream.synchronize()?;
            timing.compute += started.elapsed();

            let started = Instant::now();
            // The outputs come back an output at a time, and are wanted a
            // scenario at a time.
            for (output, column) in plan.outputs.iter().zip(back.chunks_mut(count)) {
                match *output {
                    Output::Slot(slot) => {
                        let row = slot as usize * count;
                        self.stream
                            .memcpy_dtoh(&vals.slice(row..row + count), column)?;
                    }
                    Output::Shared(at) => column.fill(plan.shared[at as usize]),
                    Output::Fixed(x) => column.fill(x),
                }
            }
            self.stream.synchronize()?;
            let back = &back[..outputs * count];
            results[first * outputs..(first + count) * outputs]
                .par_chunks_mut(outputs)
                .enumerate()
                .for_each(|(s, row)| {
                    for (o, x) in row.iter_mut().enumerate() {
                        *x = back[o * count + s];
                    }
                });
            timing.download += started.elapsed();
        }
        Ok((results, timing))
    }

    /// Works out one level for `scenarios` scenarios.
    #[allow(unsafe_code, clippy::too_many_arguments)]
    fn launch(
        &self,
        ops: &CudaSlice<u32>,
        programs: &CudaSlice<u32>,
        consts: &CudaSlice<f64>,
        shared: &CudaSlice<f64>,
        vals: &mut CudaSlice<f64>,
        first: u32,
        nodes: u32,
        vary: u32,
        scenarios: u32,
    ) -> Result<(), GpuError> {
        let threads = nodes as u64 * scenarios as u64;
        let config = LaunchConfig {
            grid_dim: (threads.div_ceil(THREADS_PER_BLOCK as u64) as u32, 1, 1),
            block_dim: (THREADS_PER_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let mut launch = self.stream.launch_builder(&self.level);
        launch
            .arg(ops)
            .arg(programs)
            .arg(consts)
            .arg(shared)
            .arg(vals)
            .arg(&first)
            .arg(&nodes)
            .arg(&vary)
            .arg(&scenarios);
        // SAFETY: the arguments are those `level` in kernel.cu declares, in
        // order. The kernel stays inside the arrays because the plan was made
        // by `Scenarios::new`: every node of `first..first + nodes` has an
        // entry in `programs` pointing inside `ops`; every load names a slot
        // below `plan.slots()`, every constant an entry of `consts` and every
        // shared value an entry of `shared`; no
        // expression needs a deeper stack than the kernel's; and `vals` has
        // `plan.slots()` rows of at least `scenarios` columns.
        unsafe { launch.launch(config) }?;
        Ok(())
    }
}
