//! The C functions the JVM calls, and all the unsafe code there is.
//!
//! Each function turns the addresses it is given into references and
//! slices, and calls the safe code in `lib.rs`. What the JVM must get right
//! is the same for all of them:
//!
//! - A graph or collection address came from `ws_graph_new` or
//!   `ws_collection_new` and has not been freed. (The JVM side frees one
//!   only when nothing can reach the object that holds it, and a
//!   collection's object holds its graph's.)
//! - An address given with a length points to that many readable bytes,
//!   and an address to be written to has the room the function says.
//!
//! A function that can fail returns a number that says so (see each one),
//! and the reason is then read with `ws_error`. Text and lists come back
//! through `ws_result`: the function returns their length in bytes, and the
//! JVM copies them out. Both are kept per thread, so a call and the reading
//! of its error or result must be on one thread, with no other call between.
//!
//! A panic never crosses into the JVM: it is caught and reported as an error.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use watershed_core::{CalcType, Value};

use crate::wire::{self, Result};
use crate::{Collection, Graph};

thread_local! {
    static ERROR: RefCell<String> = const { RefCell::new(String::new()) };
    static RESULT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Runs `call`. If it is refused, or panics, the reason is kept for
/// `ws_error` and `otherwise` is returned.
fn attempt<T>(otherwise: T, call: impl FnOnce() -> Result<T>) -> T {
    let reason = match catch_unwind(AssertUnwindSafe(call)) {
        Ok(Ok(value)) => return value,
        Ok(Err(reason)) => reason,
        Err(_) => "the engine panicked; this is a bug in watershed".to_string(),
    };
    ERROR.with(|error| *error.borrow_mut() = reason);
    otherwise
}

/// Keeps bytes for `ws_result` and returns how many there are.
fn result(write: impl FnOnce(&mut Vec<u8>)) -> i64 {
    RESULT.with(|result| {
        let mut result = result.borrow_mut();
        result.clear();
        write(&mut result);
        result.len() as i64
    })
}

fn copy_out(bytes: &[u8], to: *mut u8, room: i64) -> i64 {
    let count = bytes.len().min(usize::try_from(room).unwrap_or(0));
    if count > 0 {
        // SAFETY: the caller gave `room` writable bytes at `to`.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), to, count) };
    }
    bytes.len() as i64
}

/// SAFETY: `address` is a live graph (see the top of this file).
unsafe fn graph<'a>(address: i64) -> &'a Graph {
    unsafe { &*(address as *const Graph) }
}

/// SAFETY: `address` is a live collection.
unsafe fn collection<'a>(address: i64) -> &'a Collection<'static> {
    unsafe { &*(address as *const Collection<'static>) }
}

/// SAFETY: `at` points to `length` readable bytes.
unsafe fn bytes<'a>(at: *const u8, length: i64) -> &'a [u8] {
    match usize::try_from(length) {
        Ok(length) if length > 0 => unsafe { std::slice::from_raw_parts(at, length) },
        _ => &[],
    }
}

/// SAFETY: as `bytes`.
unsafe fn text<'a>(at: *const u8, length: i64) -> Result<&'a str> {
    wire::text(unsafe { bytes(at, length) })
}

const DONE: i32 = 0;
const REFUSED: i32 = 1;
const NO_NODE: i32 = -1;

/// Writes a value for the JVM: up to three numbers in `out`, and for text,
/// a list or a failure, the bytes for `ws_result` (with their length in
/// `out[0]`). Returns which kind it is.
fn give(value: &Value, out: &mut [i64]) -> i32 {
    let kind = match value {
        Value::F64(x) => {
            out[0] = x.to_bits() as i64;
            wire::NUMBER
        }
        Value::I64(x) => {
            out[0] = *x;
            wire::WHOLE
        }
        Value::Decimal(x) => {
            let (low, high, scale) = wire::decimal_parts(x);
            (out[0], out[1], out[2]) = (low, high, i64::from(scale));
            wire::DECIMAL
        }
        Value::Bool(x) => {
            out[0] = i64::from(*x);
            wire::FLAG
        }
        Value::Str(text) => {
            out[0] = result(|bytes| bytes.extend_from_slice(text.as_bytes()));
            wire::TEXT
        }
        Value::List(items) => {
            out[0] = result(|bytes| {
                bytes.extend_from_slice(&(items.len() as i32).to_le_bytes());
                for item in items.iter() {
                    wire::put_text(bytes, item);
                }
            });
            wire::TEXTS
        }
        Value::Error(why) => {
            out[0] = result(|bytes| bytes.extend_from_slice(why.as_bytes()));
            wire::FAILED
        }
    };
    i32::from(kind)
}

// ------------------------------------------------------------ errors, results

/// Copies the last error's reason to `to` (up to `room` bytes) and returns
/// its full length, so a caller with too little room can ask again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_error(to: *mut u8, room: i64) -> i64 {
    ERROR.with(|error| copy_out(error.borrow().as_bytes(), to, room))
}

/// Copies the last text or list to `to`, as `ws_error` does.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_result(to: *mut u8, room: i64) -> i64 {
    RESULT.with(|result| copy_out(&result.borrow(), to, room))
}

// ---------------------------------------------------------------------- graph

/// A new graph. Free it with `ws_graph_free`.
#[unsafe(no_mangle)]
pub extern "C" fn ws_graph_new() -> i64 {
    Box::into_raw(Box::new(Graph::new())) as i64
}

/// Frees a graph. Its address, and its collections', must not be used again.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_graph_free(address: i64) {
    if address != 0 {
        // SAFETY: it came from `ws_graph_new` and is freed once.
        drop(unsafe { Box::from_raw(address as *mut Graph) });
    }
}

/// Adds an input. `kind` says which of the values given is meant: a number
/// (`number`), a whole number (`low`), a decimal (`low`, `high`, `scale`),
/// a flag (`low`), or text (`at`, `length`). Returns its node, or -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_input(
    address: i64,
    name: *const u8,
    name_length: i64,
    kind: i32,
    number: f64,
    low: i64,
    high: i64,
    scale: i32,
    at: *const u8,
    length: i64,
) -> i32 {
    attempt(NO_NODE, || {
        // SAFETY: see the top of this file.
        let (graph, name) = unsafe { (graph(address), text(name, name_length)?) };
        let value = unsafe { value(kind, number, low, high, scale, at, length) }?;
        graph.input(name, value)
    })
}

/// A value given as separate arguments, as `ws_input` describes.
///
/// SAFETY: for text, `at` points to `length` readable bytes.
unsafe fn value(
    kind: i32,
    number: f64,
    low: i64,
    high: i64,
    scale: i32,
    at: *const u8,
    length: i64,
) -> Result<Value> {
    Ok(match u8::try_from(kind).unwrap_or(u8::MAX) {
        wire::NUMBER => Value::F64(number),
        wire::WHOLE => Value::I64(low),
        wire::DECIMAL => wire::decimal(low, high, scale)?.into_value(),
        wire::FLAG => Value::Bool(low != 0),
        wire::TEXT => Value::Str(unsafe { text(at, length) }?.into()),
        _ => return Err(format!("there is no kind of value numbered {kind}")),
    })
}

/// Adds a calculated node from an expression. Returns its node, or -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_define(
    address: i64,
    name: *const u8,
    name_length: i64,
    expression: *const u8,
    length: i64,
) -> i32 {
    attempt(NO_NODE, || {
        // SAFETY: see the top of this file.
        let (graph, name, expression) = unsafe {
            (
                graph(address),
                text(name, name_length)?,
                bytes(expression, length),
            )
        };
        graph.define(name, expression)
    })
}

/// Adds `count` calculated nodes in order, each a name and an expression in
/// `data`, and writes their nodes to `nodes` (room for `count`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_define_many(
    address: i64,
    data: *const u8,
    length: i64,
    nodes: *mut i32,
    count: i64,
) -> i32 {
    attempt(REFUSED, || {
        let count = usize::try_from(count).map_err(|_| "a count passed in is negative")?;
        // SAFETY: see the top of this file; `nodes` has room for `count`.
        let (graph, data, nodes) = unsafe {
            (
                graph(address),
                bytes(data, length),
                std::slice::from_raw_parts_mut(nodes, count),
            )
        };
        graph.define_many(data, nodes).map(|()| DONE)
    })
}

/// Records a new value for an input, given as `ws_input` describes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_set(
    address: i64,
    node: i32,
    kind: i32,
    number: f64,
    low: i64,
    high: i64,
    scale: i32,
    at: *const u8,
    length: i64,
) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file.
        let value = unsafe { value(kind, number, low, high, scale, at, length) }?;
        unsafe { graph(address) }.set(node, value).map(|()| DONE)
    })
}

/// Records a new value for a number input: `ws_set` with nothing to decide.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_set_number(address: i64, node: i32, value: f64) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file.
        unsafe { graph(address) }
            .set(node, Value::F64(value))
            .map(|()| DONE)
    })
}

/// Records new values for `count` number inputs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_set_numbers(
    address: i64,
    nodes: *const i32,
    values: *const f64,
    count: i64,
) -> i32 {
    attempt(REFUSED, || {
        let count = usize::try_from(count).map_err(|_| "a count passed in is negative")?;
        // SAFETY: see the top of this file; both have `count` items.
        let (graph, nodes, values) = unsafe {
            (
                graph(address),
                std::slice::from_raw_parts(nodes, count),
                std::slice::from_raw_parts(values, count),
            )
        };
        graph.set_numbers(nodes, values).map(|()| DONE)
    })
}

/// A number node's value, or NaN if it is anything else (an error, another
/// type, not being calculated, no such node). A node really holding NaN
/// looks the same; `ws_value` tells them apart.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_number(address: i64, node: i32) -> f64 {
    // SAFETY: see the top of this file.
    let graph = unsafe { graph(address) };
    catch_unwind(AssertUnwindSafe(|| graph.number(node)))
        .ok()
        .flatten()
        .unwrap_or(f64::NAN)
}

/// A node's value as of the last update, written to `out` (room for three
/// `i64`s). Returns its kind, or -1 if it cannot be read.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_value(address: i64, node: i32, out: *mut i64) -> i32 {
    attempt(-1, || {
        // SAFETY: see the top of this file; `out` has room for three.
        let (graph, out) = unsafe { (graph(address), std::slice::from_raw_parts_mut(out, 3)) };
        Ok(give(&graph.value(node)?, out))
    })
}

/// Applies every recorded change and recalculates what depends on them.
/// Writes the update's number, the inputs changed, the nodes recalculated
/// and the nodes changed to `report` (room for four `i64`s).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_stabilize(address: i64, report: *mut i64) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file; `report` has room for four.
        let (graph, out) = unsafe { (graph(address), std::slice::from_raw_parts_mut(report, 4)) };
        out.copy_from_slice(&graph.stabilize());
        Ok(DONE)
    })
}

/// Why a node has its value, for `ws_result`. Returns its length, or -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_explain(address: i64, node: i32) -> i64 {
    attempt(-1, || {
        // SAFETY: see the top of this file.
        let text = unsafe { graph(address) }.explain(node)?;
        Ok(result(|bytes| bytes.extend_from_slice(text.as_bytes())))
    })
}

/// A node's name, for `ws_result`. Returns its length, or -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_name(address: i64, node: i32) -> i64 {
    attempt(-1, || {
        // SAFETY: see the top of this file.
        let text = unsafe { graph(address) }.name(node)?;
        Ok(result(|bytes| bytes.extend_from_slice(text.as_bytes())))
    })
}

/// The names of the nodes the last update recalculated, in order, for
/// `ws_result`: how many, then each as text. Returns the length, or -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_last_recalculated(address: i64) -> i64 {
    attempt(-1, || {
        // SAFETY: see the top of this file.
        let names = unsafe { graph(address) }.last_recalculated();
        Ok(result(|bytes| {
            bytes.extend_from_slice(&(names.len() as i32).to_le_bytes());
            for name in &names {
                wire::put_text(bytes, name);
            }
        }))
    })
}

/// How many nodes the graph has had.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_size(address: i64) -> i64 {
    // SAFETY: see the top of this file.
    attempt(-1, || Ok(unsafe { graph(address) }.size()))
}

/// Removes a node that nothing uses.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_remove(address: i64, node: i32) -> i32 {
    // SAFETY: see the top of this file.
    attempt(REFUSED, || {
        unsafe { graph(address) }.remove(node).map(|()| DONE)
    })
}

/// Watches a node (`on` not 0) or stops watching it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_watch(address: i64, node: i32, on: i32) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file.
        unsafe { graph(address) }
            .watch(node, on != 0)
            .map(|()| DONE)
    })
}

/// Pull mode on (`on` not 0) or off.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_calculate_only_watched(address: i64, on: i32) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file.
        unsafe { graph(address) }.calculate_only_watched(on != 0);
        Ok(DONE)
    })
}

/// Checks every node against a calculation from scratch.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_check_consistency(address: i64) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file.
        unsafe { graph(address) }.check_consistency().map(|()| DONE)
    })
}

// ----------------------------------------------------------------- collection

/// A new, empty collection in a graph, or 0. Free it with
/// `ws_collection_free`, before or after its graph, and do not use it after
/// its graph is freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_collection_new(address: i64, name: *const u8, length: i64) -> i64 {
    attempt(0, || {
        // SAFETY: see the top of this file. The graph outlives every use of
        // the collection, which is what `'static` stands for here.
        let (graph, name): (&'static Graph, _) = unsafe { (graph(address), text(name, length)?) };
        Ok(Box::into_raw(Box::new(graph.collection(name))) as i64)
    })
}

/// Frees a collection. Its rows stay in the graph.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_collection_free(address: i64) {
    if address != 0 {
        // SAFETY: it came from `ws_collection_new` and is freed once.
        // Dropping it does not touch its graph.
        drop(unsafe { Box::from_raw(address as *mut Collection<'static>) });
    }
}

/// Adds a field holding the kind of value numbered `kind`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_field(address: i64, name: *const u8, length: i64, kind: i32) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file.
        let (collection, name) = unsafe { (collection(address), text(name, length)?) };
        collection
            .field(name, u8::try_from(kind).unwrap_or(u8::MAX))
            .map(|()| DONE)
    })
}

/// Adds a column worked out in every row by an expression.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_column(
    address: i64,
    name: *const u8,
    name_length: i64,
    expression: *const u8,
    length: i64,
) -> i32 {
    attempt(REFUSED, || {
        // SAFETY: see the top of this file.
        let (collection, name, expression) = unsafe {
            (
                collection(address),
                text(name, name_length)?,
                bytes(expression, length),
            )
        };
        collection.column(name, expression).map(|()| DONE)
    })
}

/// Adds a total (0 sum, 1 smallest, 2 largest, 3 count) of the field or
/// column named, or of rows when `of_length` is negative. Returns its node,
/// or -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_total(
    address: i64,
    name: *const u8,
    name_length: i64,
    op: i32,
    of: *const u8,
    of_length: i64,
) -> i32 {
    attempt(NO_NODE, || {
        // SAFETY: see the top of this file.
        let (collection, name) = unsafe { (collection(address), text(name, name_length)?) };
        let of = match of_length {
            length if length < 0 => None,
            length => Some(unsafe { text(of, length) }?),
        };
        collection.total(name, op, of)
    })
}

/// Adds or changes `rows` rows, written in `data`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_upsert(address: i64, data: *const u8, length: i64, rows: i64) -> i32 {
    attempt(REFUSED, || {
        let rows = usize::try_from(rows).map_err(|_| "a count passed in is negative")?;
        // SAFETY: see the top of this file.
        let (collection, data) = unsafe { (collection(address), bytes(data, length)) };
        collection.upsert(data, rows).map(|()| DONE)
    })
}

/// Removes a row. Returns 1 if there was one, 0 if not, -1 if refused.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_row_remove(address: i64, key: *const u8, length: i64) -> i32 {
    attempt(-1, || {
        // SAFETY: see the top of this file.
        let (collection, key) = unsafe { (collection(address), text(key, length)?) };
        collection.remove(key).map(i32::from)
    })
}

/// A field or column of a row, written to `out` as `ws_value` does.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_row_value(
    address: i64,
    key: *const u8,
    key_length: i64,
    part: *const u8,
    part_length: i64,
    out: *mut i64,
) -> i32 {
    attempt(-1, || {
        // SAFETY: see the top of this file; `out` has room for three.
        let (collection, key, part, out) = unsafe {
            (
                collection(address),
                text(key, key_length)?,
                text(part, part_length)?,
                std::slice::from_raw_parts_mut(out, 3),
            )
        };
        Ok(give(&collection.value(key, part)?, out))
    })
}

/// How many rows there are, counting changes since the last update.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ws_rows(address: i64) -> i64 {
    // SAFETY: see the top of this file.
    attempt(-1, || Ok(unsafe { collection(address) }.size()))
}
