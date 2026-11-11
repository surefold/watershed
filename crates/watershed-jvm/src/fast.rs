//! The calls a running program makes over and over, without the bindings'
//! overhead.
//!
//! The generated bindings go through JNA, which costs about 10 µs a call:
//! nothing when building a graph in a few big calls, a great deal when a
//! feed sets a price and updates thousands of times a second. The functions
//! here are plain C functions taking and returning numbers, which the JVM's
//! foreign function API calls in tens of nanoseconds.
//!
//! They never report why something failed, only that it did. The caller
//! then makes the same call through the generated bindings, which gives the
//! same answer or raises the error with its reason. So there is one place
//! that explains errors, and these stay small.
//!
//! This is the only hand-written unsafe code in the repository. Each
//! function takes the address of a graph, as [`RawGraph::address`] gave it.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::RawGraph;

const DONE: i32 = 0;
const ASK_THE_SLOW_WAY: i32 = 1;

/// The graph at an address.
///
/// # Safety
///
/// `address` must have come from [`RawGraph::address`] on a graph that is
/// still alive. The JVM side keeps the graph object reachable for as long
/// as it holds the address, and the graph is only freed when that object is
/// collected.
unsafe fn graph<'a>(address: i64) -> &'a RawGraph {
    // SAFETY: the caller's promise above.
    unsafe { &*(address as *const RawGraph) }
}

/// Runs `call`, turning a refusal or a panic into "ask the slow way". A
/// panic must not cross into the JVM.
fn status(call: impl FnOnce() -> Option<()>) -> i32 {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Some(())) => DONE,
        _ => ASK_THE_SLOW_WAY,
    }
}

/// Records a new value for a number input.
///
/// # Safety
///
/// See [`graph`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn watershed_set_number(address: i64, node: i32, value: f64) -> i32 {
    // SAFETY: passed on from the caller.
    let graph = unsafe { graph(address) };
    status(|| {
        let mut g = graph.lock();
        let id = g.node(node).ok()?;
        g.set_value(id, watershed_core::Value::F64(value)).ok()
    })
}

/// Records new values for `count` number inputs.
///
/// # Safety
///
/// See [`graph`]. `nodes` and `values` must each point to `count` readable
/// items.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn watershed_set_numbers(
    address: i64,
    nodes: *const i32,
    values: *const f64,
    count: i64,
) -> i32 {
    let Ok(count) = usize::try_from(count) else {
        return ASK_THE_SLOW_WAY;
    };
    // SAFETY: passed on from the caller.
    let (graph, nodes, values) = unsafe {
        (
            graph(address),
            std::slice::from_raw_parts(nodes, count),
            std::slice::from_raw_parts(values, count),
        )
    };
    status(|| {
        let mut g = graph.lock();
        for (&node, &value) in nodes.iter().zip(values) {
            let id = g.node(node).ok()?;
            g.set_value(id, watershed_core::Value::F64(value)).ok()?;
        }
        Some(())
    })
}

/// A number node's value as of the last update, or NaN if it is anything
/// else (an error, another type, not being calculated, no such node). A
/// node really holding NaN looks the same, and the slow way sorts it out.
///
/// # Safety
///
/// See [`graph`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn watershed_number(address: i64, node: i32) -> f64 {
    // SAFETY: passed on from the caller.
    let graph = unsafe { graph(address) };
    let read = catch_unwind(AssertUnwindSafe(|| {
        let g = graph.lock();
        let id = g.node(node).ok()?;
        if g.is_removed(id) || (!g.is_calculated(id) && !g.is_input(id)) {
            return None;
        }
        match g.value(id) {
            watershed_core::Value::F64(x) => Some(*x),
            _ => None,
        }
    }));
    read.ok().flatten().unwrap_or(f64::NAN)
}

/// Applies every recorded change and recalculates what depends on them.
/// Writes the update's number, the inputs changed, the nodes recalculated
/// and the nodes changed to `report`.
///
/// # Safety
///
/// See [`graph`]. `report` must point to room for four `i64`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn watershed_stabilize(address: i64, report: *mut i64) -> i32 {
    // SAFETY: passed on from the caller.
    let (graph, out) = unsafe { (graph(address), std::slice::from_raw_parts_mut(report, 4)) };
    status(|| {
        let report = graph.lock().stabilize();
        out[0] = report.epoch as i64;
        out[1] = report.inputs_changed as i64;
        out[2] = report.recalculated as i64;
        out[3] = report.changed as i64;
        Some(())
    })
}
