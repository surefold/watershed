//! Runs a watershed graph for many sets of inputs at once.
//!
//! A [`Graph`](watershed_core::Graph) holds one set of values, and answers
//! "what if these inputs were different?" by setting them and updating. To
//! ask that thousands of times (a risk run, a what-if table, a Monte Carlo
//! simulation), [`Scenarios`] takes a copy of the part of the graph that the
//! answer depends on and runs every set of inputs through it, on all CPU
//! cores or, with the `cuda` feature, on an NVIDIA GPU. The graph itself is
//! not touched.
//!
//! ```
//! use watershed_core::prelude::*;
//! use watershed_gpu::Scenarios;
//!
//! let mut g = Graph::new();
//! let qty = g.input("qty", 10.0)?;
//! let price = g.input("price", 2.5)?;
//! let total = g.define("total", qty * price)?;
//!
//! let plan = Scenarios::new(&g, &[price.id()], &[total.id()])?;
//! // Three scenarios, each one value for `price`.
//! assert_eq!(plan.run_cpu(&[2.5, 3.0, 4.0]), [25.0, 30.0, 40.0]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Each result is the same, to the last bit, as setting those inputs on the
//! graph and updating it, with three exceptions on the GPU: a result that is
//! not a number may be a different not-a-number; `min` and `max` of `0.0`
//! and `-0.0` follow x86-64; and `exp`, `ln` and `pow` may differ in the last
//! digit or two, because no two maths libraries round them alike. (`sqrt` is
//! exact everywhere.)
//!
//! # Trying the same things on other data
//!
//! Often there are two sorts of input: what is being tried (a rule's
//! settings, say) and the data it is tried on (prices). Give the data as
//! *shared* inputs with [`Scenarios::with_shared`]: they are the same for
//! every scenario of a run, and [`Scenarios::set_shared`] replaces them
//! without making the plan again. That is what makes it cheap to ask "would
//! this have looked as good on data with nothing in it to find?" hundreds
//! of times.
//!
//! With the `cuda` feature, `Gpu::load` puts a plan and its scenarios on
//! the GPU to stay: `Loaded::run` and `Loaded::totals` then run them
//! again after `set_shared`, copying up only what changed, and `totals`
//! adds each output up on the GPU and copies back almost nothing.
//!
//! # What it handles
//!
//! Numbers (`f64`) and yes-or-no values: arithmetic, `min`, `max`, `abs`,
//! `exp`, `ln`, `sqrt`, `pow`, comparisons, `&`, `|`, `!`, and conditions (`when(..).then(..).otherwise(..)`).
//! Both branches of a condition are worked out and one is kept, which gives
//! the same answer as the graph because none of these can fail.
//!
//! A node that depends on a varied or shared input and holds a choice, a
//! total, or work on text, whole numbers or decimals is refused when the
//! plan is made, with its name. So is one that reads a node holding an
//! error, even in a branch that is not taken.

mod keep;
mod plan;

#[cfg(feature = "cuda")]
mod cuda;

pub use keep::{Kept, Refresh};
pub use plan::{MAX_STACK, PlanError, Scenarios};

#[cfg(feature = "cuda")]
pub use cuda::{Gpu, GpuError, Loaded, Timing};
