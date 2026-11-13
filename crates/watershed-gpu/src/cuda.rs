//! Runs a [`Scenarios`] plan on an NVIDIA GPU.
//!
//! The GPU holds one table of `f64`: a row per slot and a column per
//! scenario. The varied inputs are copied up, each level is worked out for
//! every scenario at once, and only the outputs' rows are copied back. When
//! the table for every scenario would not fit in the GPU's memory, the
//! scenarios are run a part at a time.
//!
//! Nodes whose formulas differ only in what they read have one *shape*. A
//! plan with few shapes gets a kernel written and compiled for each, which
//! works out all the nodes of that shape in a level; this is several times
//! faster than a kernel that reads each node's instructions as it goes. A
//! plan with many shapes (a spreadsheet, where most cells are their own)
//! uses that general kernel.

use std::collections::HashMap;
use std::fmt::{self, Write as _};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use cudarc::driver::{
    CudaContext, CudaFunction, CudaSlice, CudaStream, DriverError, LaunchConfig, PushKernelArg,
};
use cudarc::nvrtc::{CompileError, CompileOptions, compile_ptx_with_opts};

use crate::fuse::{Fused, LOCAL};
use crate::plan::{
    BLOCK, CODE, CODE_BITS, Output, Scenarios, Source, arity, in_c, source_of, total,
};

const THREADS_PER_BLOCK: u32 = 256;

/// A plan with more shapes than this is run by the general kernel: writing
/// and compiling a kernel for each would take longer than it saves.
const MOST_SHAPES: usize = 256;

/// Why a GPU run failed.
#[derive(Debug)]
pub enum GpuError {
    /// The CUDA driver refused something.
    Driver(DriverError),
    /// The kernel did not compile.
    Compile(CompileError),
    /// Not even one scenario fits in the memory allowed, or, for
    /// [`Gpu::load`], not all of them.
    TooLittleMemory { needed: usize, allowed: usize },
}

impl fmt::Display for GpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GpuError::Driver(e) => write!(f, "CUDA driver: {e}"),
            GpuError::Compile(e) => write!(f, "compiling the kernel: {e}"),
            GpuError::TooLittleMemory { needed, allowed } => write!(
                f,
                "this needs {needed} bytes on the GPU and only {allowed} are allowed"
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
    /// Writing and compiling kernels for the plan's shapes, the first time
    /// a plan with those shapes is run.
    pub compile: Duration,
    /// Copying the plan and the inputs to the GPU.
    pub upload: Duration,
    /// Working out every level.
    pub compute: Duration,
    /// Copying the outputs back.
    pub download: Duration,
    /// How many scenarios were run at a time.
    pub scenarios_at_a_time: usize,
}

/// An NVIDIA GPU, with the general kernels compiled and ready.
pub struct Gpu {
    ctx: Arc<CudaContext>,
    stream: Arc<CudaStream>,
    level: CudaFunction,
    total: CudaFunction,
    /// Whether to compile a kernel for each shape of a plan.
    compiles: bool,
    /// Whether to work nodes out in clusters, where a plan has them.
    fuses: bool,
    /// The kernels compiled so far, by what they were compiled from.
    compiled: Mutex<HashMap<u64, Arc<Vec<CudaFunction>>>>,
}

/// How a plan's nodes are worked out on the GPU.
enum Program {
    /// By the general kernel, from each node's instructions.
    Read {
        ops: CudaSlice<u32>,
        programs: CudaSlice<u32>,
    },
    /// By a kernel for each shape.
    Compiled {
        operands: CudaSlice<u32>,
        /// Per node: where its operands start, and its slot. Or, when the
        /// nodes are in clusters, per cluster: where its operands start.
        nodes: CudaSlice<u32>,
        shapes: Arc<Vec<CudaFunction>>,
        /// Whether these are clusters.
        fused: bool,
    },
}

/// A plan on the GPU, with room for some scenarios.
struct Device {
    program: Program,
    consts: CudaSlice<f64>,
    shared: CudaSlice<f64>,
    vals: CudaSlice<f64>,
    /// How many scenarios `vals` has room for, and how many slots each has.
    room: usize,
    slots: usize,
    /// Whether the nodes are worked out in clusters, which decides where
    /// the outputs are.
    fused: bool,
}

impl Device {
    fn outputs<'p>(&self, plan: &'p Scenarios) -> &'p [Output] {
        match self.fused {
            true => &plan.fused_outputs,
            false => &plan.outputs,
        }
    }
}

/// A plan and its scenarios held on the GPU, to run again and again without
/// copying either up each time. See [`Gpu::load`].
pub struct Loaded<'g> {
    gpu: &'g Gpu,
    device: Device,
    scenarios: usize,
    /// One number for each block of scenarios, for totals.
    blocks: CudaSlice<f64>,
}

/// The fused-multiply-add switch is off: with it on, `a * b + c` is rounded
/// once where the CPU rounds twice, and the answers differ in the last bit.
fn options() -> CompileOptions {
    CompileOptions {
        fmad: Some(false),
        ..Default::default()
    }
}

/// The source of a kernel for each shape. A shape's kernel works out, for
/// every scenario, every node of one group: each thread reads what its node
/// reads, does the shape's arithmetic in the order the instructions give,
/// and writes one number.
fn source_for(shapes: &[Vec<u32>]) -> String {
    let mut source = String::new();
    for (k, shape) in shapes.iter().enumerate() {
        writeln!(
            source,
            "extern \"C\" __global__ void shape{k}(const unsigned* operands, const unsigned* nodes, \
             const double* consts, const double* shared, double* vals, unsigned first, \
             unsigned count, unsigned scenarios) {{\n\
             unsigned long long thread = (unsigned long long)blockIdx.x * blockDim.x + threadIdx.x;\n\
             unsigned long long n = thread / scenarios;\n\
             if (n >= count) return;\n\
             unsigned long long s = thread - n * scenarios;\n\
             const unsigned* node = nodes + 2 * (first + n);\n\
             const unsigned* o = operands + node[0];"
        )
        .expect("writing to a string");
        // The instructions are for a stack; here the stack holds the names
        // of the values, each worked out once into its own variable.
        let mut stack: Vec<String> = Vec::new();
        let pop = |stack: &mut Vec<String>| stack.pop().expect("a well-formed shape");
        let (mut made, mut read) = (0, 0);
        for &code in shape {
            let value = match arity(code) {
                0 => {
                    let from = match source_of(code) {
                        Source::Slot => {
                            format!("vals[(unsigned long long)o[{read}] * scenarios + s]")
                        }
                        Source::Const => format!("consts[o[{read}]]"),
                        Source::Shared => format!("shared[o[{read}]]"),
                    };
                    read += 1;
                    from
                }
                1 => {
                    let a = pop(&mut stack);
                    in_c(code, &[&a])
                }
                2 => {
                    let b = pop(&mut stack);
                    let a = pop(&mut stack);
                    in_c(code, &[&a, &b])
                }
                _ => {
                    let otherwise = pop(&mut stack);
                    let then = pop(&mut stack);
                    let condition = pop(&mut stack);
                    in_c(code, &[&condition, &then, &otherwise])
                }
            };
            writeln!(source, "double t{made} = {value};").expect("writing to a string");
            stack.push(format!("t{made}"));
            made += 1;
        }
        let result = pop(&mut stack);
        writeln!(
            source,
            "vals[(unsigned long long)node[1] * scenarios + s] = {result};\n}}"
        )
        .expect("writing to a string");
    }
    source
}

/// The source of a kernel for each shape of cluster. A thread works out
/// every node of one cluster for one scenario, one after another, and writes
/// to memory only the values something outside the cluster reads.
fn source_for_clusters(shapes: &[Vec<u32>]) -> String {
    let mut source = String::new();
    for (k, shape) in shapes.iter().enumerate() {
        writeln!(
            source,
            "extern \"C\" __global__ void shape{k}(const unsigned* operands, const unsigned* starts, \
             const double* consts, const double* shared, double* vals, unsigned first, \
             unsigned count, unsigned scenarios) {{\n\
             unsigned long long thread = (unsigned long long)blockIdx.x * blockDim.x + threadIdx.x;\n\
             unsigned long long c = thread / scenarios;\n\
             if (c >= count) return;\n\
             unsigned long long s = thread - c * scenarios;\n\
             const unsigned* o = operands + starts[first + c];"
        )
        .expect("writing to a string");
        let pop = |stack: &mut Vec<String>| stack.pop().expect("a well-formed shape");
        let (mut made, mut read, mut at, mut node) = (0, 0, 0, 0);
        let mut written = Vec::new();
        while at < shape.len() {
            let (ops, goes_to_memory) = (shape[at] as usize, shape[at + 1] == 1);
            let mut stack: Vec<String> = Vec::new();
            for &op in &shape[at + 2..at + 2 + ops] {
                let code = op & CODE;
                if code == LOCAL {
                    stack.push(format!("n{}", op >> CODE_BITS));
                    continue;
                }
                let value = match arity(code) {
                    0 => {
                        let from = match source_of(code) {
                            Source::Slot => {
                                format!("vals[(unsigned long long)o[{read}] * scenarios + s]")
                            }
                            Source::Const => format!("consts[o[{read}]]"),
                            Source::Shared => format!("shared[o[{read}]]"),
                        };
                        read += 1;
                        from
                    }
                    1 => {
                        let a = pop(&mut stack);
                        in_c(code, &[&a])
                    }
                    2 => {
                        let b = pop(&mut stack);
                        let a = pop(&mut stack);
                        in_c(code, &[&a, &b])
                    }
                    _ => {
                        let otherwise = pop(&mut stack);
                        let then = pop(&mut stack);
                        let condition = pop(&mut stack);
                        in_c(code, &[&condition, &then, &otherwise])
                    }
                };
                writeln!(source, "double t{made} = {value};").expect("writing to a string");
                stack.push(format!("t{made}"));
                made += 1;
            }
            let result = pop(&mut stack);
            writeln!(source, "double n{node} = {result};").expect("writing to a string");
            if goes_to_memory {
                written.push(node);
            }
            node += 1;
            at += 2 + ops;
        }
        for (k, node) in written.iter().enumerate() {
            writeln!(
                source,
                "vals[(unsigned long long)o[{}] * scenarios + s] = n{node};",
                read + k
            )
            .expect("writing to a string");
        }
        source.push_str("}\n");
    }
    source
}

impl Gpu {
    /// Opens the first GPU and compiles the general kernels for it.
    ///
    /// # Panics
    /// If the CUDA libraries are not installed: they are looked for when
    /// this is called, not when the program is built.
    pub fn new() -> Result<Gpu, GpuError> {
        let ctx = CudaContext::new(0)?;
        let ptx = compile_ptx_with_opts(include_str!("kernel.cu"), options())?;
        let module = ctx.load_module(ptx)?;
        let level = module.load_function("level")?;
        let total = module.load_function("total")?;
        let stream = ctx.default_stream();
        Ok(Gpu {
            ctx,
            stream,
            level,
            total,
            compiles: true,
            fuses: true,
            compiled: Mutex::new(HashMap::new()),
        })
    }

    /// The same GPU, never compiling kernels for a plan's shapes: every plan
    /// is run by the general kernel. The answers are the same; this is for
    /// comparing the two.
    pub fn without_compiling(mut self) -> Gpu {
        self.compiles = false;
        self
    }

    /// The same GPU, working out a node at a time and never a cluster of
    /// them in one go. The answers are the same; this is for comparing the
    /// two.
    pub fn without_fusing(mut self) -> Gpu {
        self.fuses = false;
        self
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
        let at_a_time = self.fit(plan, scenarios, allowed);
        if at_a_time == 0 {
            return Err(GpuError::TooLittleMemory {
                needed: self.fixed(plan) + self.slots(plan) * size_of::<f64>(),
                allowed,
            });
        }
        timing.scenarios_at_a_time = at_a_time;
        let mut device = self.device(plan, at_a_time, &mut timing)?;

        let (vary, outputs) = (plan.vary, plan.outputs.len());
        let mut results = vec![0.0; scenarios * outputs];
        for first in (0..scenarios).step_by(at_a_time) {
            let count = at_a_time.min(scenarios - first);
            let started = Instant::now();
            self.give(
                &mut device,
                plan,
                &inputs[first * vary..(first + count) * vary],
            )?;
            timing.upload += started.elapsed();

            let started = Instant::now();
            self.work_out(&mut device, plan, count)?;
            self.stream.synchronize()?;
            timing.compute += started.elapsed();

            let started = Instant::now();
            self.take(
                &device,
                plan,
                count,
                &mut results[first * outputs..(first + count) * outputs],
            )?;
            timing.download += started.elapsed();
        }
        Ok((results, timing))
    }

    /// Puts a plan and its scenarios on the GPU to stay, for running again
    /// and again: after [`Scenarios::set_shared`], or with other inputs of
    /// the same number. Nothing is copied up again that has not changed, and
    /// [`Loaded::totals`] copies almost nothing back.
    ///
    /// Every scenario has to fit at once in four fifths of the GPU's free
    /// memory.
    ///
    /// # Panics
    /// If the length of `inputs` is not a whole number of scenarios, or
    /// there are none.
    pub fn load<'g>(&'g self, plan: &Scenarios, inputs: &[f64]) -> Result<Loaded<'g>, GpuError> {
        let scenarios = plan.scenarios_in(inputs);
        assert!(scenarios > 0, "no scenarios to load");
        let allowed = self.free_memory()? / 5 * 4;
        if self.fit(plan, scenarios, allowed) < scenarios {
            return Err(GpuError::TooLittleMemory {
                needed: self.fixed(plan) + scenarios * self.slots(plan) * size_of::<f64>(),
                allowed,
            });
        }
        let mut device = self.device(plan, scenarios, &mut Timing::default())?;
        self.give(&mut device, plan, inputs)?;
        let blocks = self.stream.alloc_zeros::<f64>(scenarios.div_ceil(BLOCK))?;
        Ok(Loaded {
            gpu: self,
            device,
            scenarios,
            blocks,
        })
    }

    /// Whether a kernel is compiled for each of a plan's shapes.
    fn compiles(&self, plan: &Scenarios) -> bool {
        self.compiles && plan.shapes.len() <= MOST_SHAPES
    }

    /// The plan's clusters, if its nodes are to be worked out in them: it
    /// has them, and few enough shapes of them to compile a kernel for each.
    fn clusters<'p>(&self, plan: &'p Scenarios) -> Option<&'p Fused> {
        plan.fused
            .as_ref()
            .filter(|f| self.compiles && self.fuses && f.shapes.len() <= MOST_SHAPES)
    }

    /// How many slots a scenario takes on this GPU.
    fn slots(&self, plan: &Scenarios) -> usize {
        self.clusters(plan).map_or(plan.slots(), |f| f.slots)
    }

    /// The memory a plan takes on the GPU whatever the number of scenarios.
    fn fixed(&self, plan: &Scenarios) -> usize {
        let program = match (self.clusters(plan), self.compiles(plan)) {
            (Some(f), _) => size_of_val(&f.operands[..]) + size_of_val(&f.operand_at[..]),
            (None, true) => size_of_val(&plan.operands[..]) + 2 * size_of_val(&plan.operand_at[..]),
            (None, false) => size_of_val(&plan.ops[..]) + size_of_val(&plan.programs[..]),
        };
        program + size_of_val(&plan.consts[..]) + size_of_val(&plan.shared[..])
    }

    /// How many of `scenarios` fit at once: in memory, and in one launch,
    /// which can have at most 2^31 - 1 blocks of threads.
    fn fit(&self, plan: &Scenarios, scenarios: usize, allowed: usize) -> usize {
        let widest = match self.clusters(plan) {
            Some(f) => f.widest,
            None => plan.levels.iter().map(|l| l.len()).max().unwrap_or(1),
        };
        let most_threads = (i32::MAX as usize) * THREADS_PER_BLOCK as usize;
        (allowed.saturating_sub(self.fixed(plan)) / (self.slots(plan) * size_of::<f64>()))
            .min(most_threads / widest)
            .min(u32::MAX as usize)
            .min(scenarios)
    }

    /// The kernels for a plan's shapes, compiled now if no plan with these
    /// shapes has been run before.
    fn shapes(
        &self,
        shapes: &[Vec<u32>],
        clusters: bool,
    ) -> Result<Arc<Vec<CudaFunction>>, GpuError> {
        let mut hasher = DefaultHasher::new();
        (clusters, shapes).hash(&mut hasher);
        let key = hasher.finish();
        if let Some(known) = self.compiled.lock().expect("no panic while held").get(&key) {
            return Ok(known.clone());
        }
        let source = match clusters {
            true => source_for_clusters(shapes),
            false => source_for(shapes),
        };
        let ptx = compile_ptx_with_opts(source, options())?;
        let module = self.ctx.load_module(ptx)?;
        let shapes = (0..shapes.len())
            .map(|k| module.load_function(&format!("shape{k}")))
            .collect::<Result<Vec<_>, _>>()?;
        let shapes = Arc::new(shapes);
        self.compiled
            .lock()
            .expect("no panic while held")
            .insert(key, shapes.clone());
        Ok(shapes)
    }

    /// Copies a plan to the GPU, with room for `room` scenarios.
    fn device(
        &self,
        plan: &Scenarios,
        room: usize,
        timing: &mut Timing,
    ) -> Result<Device, GpuError> {
        let program = if let Some(f) = self.clusters(plan) {
            let started = Instant::now();
            let shapes = self.shapes(&f.shapes, true)?;
            timing.compile += started.elapsed();
            let started = Instant::now();
            // An empty allocation is refused, and a plan may read nothing.
            let operands = match f.operands.is_empty() {
                true => self.stream.clone_htod(&[0u32])?,
                false => self.stream.clone_htod(&f.operands)?,
            };
            let program = Program::Compiled {
                operands,
                nodes: self.stream.clone_htod(&f.operand_at)?,
                shapes,
                fused: true,
            };
            timing.upload += started.elapsed();
            program
        } else if self.compiles(plan) {
            let started = Instant::now();
            let shapes = self.shapes(&plan.shapes, false)?;
            timing.compile += started.elapsed();
            let started = Instant::now();
            let nodes: Vec<u32> = plan
                .operand_at
                .iter()
                .zip(plan.programs.chunks_exact(3))
                .flat_map(|(&at, program)| [at, program[2]])
                .collect();
            // An empty allocation is refused, and a plan may read nothing.
            let operands = match plan.operands.is_empty() {
                true => self.stream.clone_htod(&[0u32])?,
                false => self.stream.clone_htod(&plan.operands)?,
            };
            let program = Program::Compiled {
                operands,
                nodes: self.stream.clone_htod(&nodes)?,
                shapes,
                fused: false,
            };
            timing.upload += started.elapsed();
            program
        } else {
            let started = Instant::now();
            let program = Program::Read {
                ops: self.stream.clone_htod(&plan.ops)?,
                programs: self.stream.clone_htod(&plan.programs)?,
            };
            timing.upload += started.elapsed();
            program
        };
        let started = Instant::now();
        let slots = self.slots(plan);
        let fused = self.clusters(plan).is_some();
        let device = Device {
            program,
            consts: self.some(&plan.consts)?,
            shared: self.some(&plan.shared)?,
            vals: self.stream.alloc_zeros::<f64>(slots * room)?,
            room,
            slots,
            fused,
        };
        self.stream.synchronize()?;
        timing.upload += started.elapsed();
        Ok(device)
    }

    /// Values on the GPU. An empty allocation is refused, and a plan may
    /// have no constants or no shared values.
    fn some(&self, values: &[f64]) -> Result<CudaSlice<f64>, GpuError> {
        Ok(match values.is_empty() {
            true => self.stream.clone_htod(&[0.0f64])?,
            false => self.stream.clone_htod(values)?,
        })
    }

    /// Copies some scenarios' inputs to the GPU.
    fn give(&self, device: &mut Device, plan: &Scenarios, rows: &[f64]) -> Result<(), GpuError> {
        let vary = plan.vary;
        let count = rows.len() / vary;
        assert!(
            count <= device.room,
            "more scenarios than there is room for"
        );
        // The inputs arrive a scenario at a time; the GPU wants them an
        // input at a time.
        let mut columns = vec![0.0; rows.len()];
        columns
            .par_chunks_mut(count)
            .enumerate()
            .for_each(|(v, column)| {
                for (s, x) in column.iter_mut().enumerate() {
                    *x = rows[s * vary + v];
                }
            });
        self.stream
            .memcpy_htod(&columns, &mut device.vals.slice_mut(..vary * count))?;
        self.stream.synchronize()?;
        Ok(())
    }

    /// Works out every node for the first `count` scenarios on the device.
    fn work_out(
        &self,
        device: &mut Device,
        plan: &Scenarios,
        count: usize,
    ) -> Result<(), GpuError> {
        let count = count as u32;
        match &device.program {
            Program::Read { ops, programs } => {
                for level in &plan.levels {
                    self.launch_level(
                        ops,
                        programs,
                        &device.consts,
                        &device.shared,
                        &mut device.vals,
                        level.start,
                        level.len() as u32,
                        count,
                    )?;
                }
            }
            Program::Compiled {
                operands,
                nodes,
                shapes,
                fused,
            } => {
                let groups = match (fused, &plan.fused) {
                    (true, Some(f)) => &f.groups,
                    _ => &plan.groups,
                };
                for group in groups {
                    self.launch_shape(
                        &shapes[group.shape as usize],
                        operands,
                        nodes,
                        &device.consts,
                        &device.shared,
                        &mut device.vals,
                        group.first,
                        group.count,
                        count,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Copies the outputs of the first `count` scenarios back, a scenario
    /// after another.
    fn take(
        &self,
        device: &Device,
        plan: &Scenarios,
        count: usize,
        results: &mut [f64],
    ) -> Result<(), GpuError> {
        let outputs = plan.outputs.len();
        // The outputs come back an output at a time, and are wanted a
        // scenario at a time.
        let mut back = vec![0.0; outputs * count];
        for (output, column) in device.outputs(plan).iter().zip(back.chunks_mut(count)) {
            match *output {
                Output::Slot(slot) => {
                    let row = slot as usize * count;
                    self.stream
                        .memcpy_dtoh(&device.vals.slice(row..row + count), column)?;
                }
                Output::Shared(at) => column.fill(plan.shared[at as usize]),
                Output::Fixed(x) => column.fill(x),
            }
        }
        self.stream.synchronize()?;
        results
            .par_chunks_mut(outputs)
            .enumerate()
            .for_each(|(s, row)| {
                for (o, x) in row.iter_mut().enumerate() {
                    *x = back[o * count + s];
                }
            });
        Ok(())
    }

    fn config(threads: u64) -> LaunchConfig {
        LaunchConfig {
            grid_dim: (threads.div_ceil(THREADS_PER_BLOCK as u64) as u32, 1, 1),
            block_dim: (THREADS_PER_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        }
    }

    /// Works out one level for `scenarios` scenarios with the general
    /// kernel.
    #[allow(unsafe_code, clippy::too_many_arguments)]
    fn launch_level(
        &self,
        ops: &CudaSlice<u32>,
        programs: &CudaSlice<u32>,
        consts: &CudaSlice<f64>,
        shared: &CudaSlice<f64>,
        vals: &mut CudaSlice<f64>,
        first: u32,
        nodes: u32,
        scenarios: u32,
    ) -> Result<(), GpuError> {
        let mut launch = self.stream.launch_builder(&self.level);
        launch
            .arg(ops)
            .arg(programs)
            .arg(consts)
            .arg(shared)
            .arg(vals)
            .arg(&first)
            .arg(&nodes)
            .arg(&scenarios);
        // SAFETY: the arguments are those `level` in kernel.cu declares, in
        // order. The kernel stays inside the arrays because the plan was made
        // by `Scenarios::with_shared`: every node of `first..first + nodes`
        // has an entry in `programs` pointing inside `ops` and naming a slot
        // below `plan.slots()`; every load names a slot below `plan.slots()`,
        // every constant an entry of `consts` and every shared value an entry
        // of `shared`; no expression needs a deeper stack than the kernel's;
        // and `vals` has `plan.slots()` rows of at least `scenarios` columns,
        // laid out with `scenarios` columns to a row.
        unsafe { launch.launch(Self::config(nodes as u64 * scenarios as u64)) }?;
        Ok(())
    }

    /// Works out one group of nodes of one shape for `scenarios` scenarios
    /// with that shape's kernel.
    #[allow(unsafe_code, clippy::too_many_arguments)]
    fn launch_shape(
        &self,
        shape: &CudaFunction,
        operands: &CudaSlice<u32>,
        nodes: &CudaSlice<u32>,
        consts: &CudaSlice<f64>,
        shared: &CudaSlice<f64>,
        vals: &mut CudaSlice<f64>,
        first: u32,
        count: u32,
        scenarios: u32,
    ) -> Result<(), GpuError> {
        let mut launch = self.stream.launch_builder(shape);
        launch
            .arg(operands)
            .arg(nodes)
            .arg(consts)
            .arg(shared)
            .arg(vals)
            .arg(&first)
            .arg(&count)
            .arg(&scenarios);
        // SAFETY: the arguments are those `source_for` declares for every
        // shape's kernel, in order, and `shape` was compiled from the shape
        // every node of this group has. The kernel stays inside the arrays
        // because the plan was made by `Scenarios::with_shared`: every node of
        // `first..first + count` has an entry in `nodes` giving where its
        // operands start and a slot below `plan.slots()`; it has as many
        // operands as the shape has loads, each naming a slot below
        // `plan.slots()`, an entry of `consts` or an entry of `shared`
        // according to the load; and `vals` has `plan.slots()` rows of at
        // least `scenarios` columns, laid out with `scenarios` columns to a
        // row.
        unsafe { launch.launch(Self::config(count as u64 * scenarios as u64)) }?;
        Ok(())
    }

    /// Adds up one slot's row, a block of scenarios to each number of `out`.
    #[allow(unsafe_code)]
    fn launch_total(
        &self,
        vals: &CudaSlice<f64>,
        out: &mut CudaSlice<f64>,
        slot: u32,
        scenarios: u32,
    ) -> Result<(), GpuError> {
        let block = BLOCK as u32;
        let blocks = (scenarios as u64).div_ceil(BLOCK as u64);
        let mut launch = self.stream.launch_builder(&self.total);
        launch
            .arg(vals)
            .arg(out)
            .arg(&slot)
            .arg(&scenarios)
            .arg(&block);
        // SAFETY: the arguments are those `total` in kernel.cu declares, in
        // order. `slot` is below `plan.slots()` and `vals` has that many rows
        // of `scenarios` columns, so the row read is inside it; `out` has a
        // number for each block of `scenarios`, and a thread past the last
        // block writes nothing.
        unsafe { launch.launch(Self::config(blocks)) }?;
        Ok(())
    }
}

impl Loaded<'_> {
    /// How many scenarios are loaded.
    pub fn scenarios(&self) -> usize {
        self.scenarios
    }

    /// Replaces every scenario's inputs.
    ///
    /// # Panics
    /// If `inputs` is not for the plan this was loaded with and the same
    /// number of scenarios.
    pub fn set_inputs(&mut self, plan: &Scenarios, inputs: &[f64]) -> Result<(), GpuError> {
        assert_eq!(
            plan.scenarios_in(inputs),
            self.scenarios,
            "the same number of scenarios as were loaded"
        );
        self.gpu.give(&mut self.device, plan, inputs)
    }

    /// Works every scenario out again, with the plan's shared values as
    /// they are now.
    fn work_out(&mut self, plan: &Scenarios) -> Result<(), GpuError> {
        assert_eq!(
            self.device.slots * self.scenarios,
            self.device.vals.len(),
            "the plan this was loaded with"
        );
        self.device.shared = self.gpu.some(&plan.shared)?;
        self.gpu.work_out(&mut self.device, plan, self.scenarios)
    }

    /// Runs every scenario and gives every output of each, as
    /// [`Gpu::run`] does. `plan` must be the plan this was loaded with; its
    /// shared values are used as they are now.
    pub fn run(&mut self, plan: &Scenarios) -> Result<Vec<f64>, GpuError> {
        self.work_out(plan)?;
        let mut results = vec![0.0; self.scenarios * plan.outputs.len()];
        self.gpu
            .take(&self.device, plan, self.scenarios, &mut results)?;
        Ok(results)
    }

    /// Runs every scenario and gives each output added up over all of them,
    /// as [`Scenarios::totals_cpu`] does and to the last bit. The adding up
    /// is done on the GPU, so almost nothing is copied back.
    pub fn totals(&mut self, plan: &Scenarios) -> Result<Vec<f64>, GpuError> {
        self.work_out(plan)?;
        let mut blocks = vec![0.0; self.scenarios.div_ceil(BLOCK)];
        let mut totals = Vec::with_capacity(plan.outputs.len());
        for output in self.device.outputs(plan) {
            totals.push(match *output {
                Output::Slot(slot) => {
                    self.gpu.launch_total(
                        &self.device.vals,
                        &mut self.blocks,
                        slot,
                        self.scenarios as u32,
                    )?;
                    self.gpu.stream.memcpy_dtoh(&self.blocks, &mut blocks)?;
                    self.gpu.stream.synchronize()?;
                    blocks.iter().fold(0.0, |all, block| all + block)
                }
                Output::Shared(at) => total(std::iter::repeat_n(
                    plan.shared[at as usize],
                    self.scenarios,
                )),
                Output::Fixed(x) => total(std::iter::repeat_n(x, self.scenarios)),
            });
        }
        Ok(totals)
    }
}
