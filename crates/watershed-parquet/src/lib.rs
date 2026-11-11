//! Writes a graph's changes to Parquet files, so its history outlives the
//! process and can be queried with SQL.
//!
//! ```no_run
//! use watershed_core::prelude::*;
//! use watershed_parquet::{LocalStore, Options, parquet_sink};
//!
//! let mut g = Graph::new();
//! let price = g.input("price", 1.0).unwrap();
//! let (sink, writer) = parquet_sink(LocalStore::new("history"), Options::new("run-1"));
//! g.attach_sink(sink);
//! g.set(price, 2.0).unwrap();
//! g.stabilize();
//! g.detach_sink();
//! writer.close().unwrap();
//! ```
//!
//! # What is written
//!
//! Under `run=<run id>/`, two kinds of file, each covering a range of updates
//! (`<first>-<last>.parquet`, zero-padded so they sort):
//!
//! - `changes/`: one row per value that changed: `run_id`, `epoch`,
//!   `node_id`, `kind` (`f64`, `i64`, `bool`, `str`, `list` or `error`), and
//!   the value in the column for its kind (`f64`, `i64`, `bool`, `list` for a
//!   list of strings, or `text` for both strings and error messages). The first update holds every node's
//!   value when the sink was attached. Rows are sorted by node, then update.
//! - `nodes/`: one row per node, written with the update it first appears
//!   in: `run_id`, `node_id`, `name`, `type`, `created_epoch`.
//!
//! A node's value at update `e` is its row with the highest `epoch <= e`.
//!
//! # How it behaves
//!
//! The graph's thread only copies the changed values and queues them; a
//! background thread builds and writes the files. When the queue is full,
//! updates wait for the writer, so no change is ever dropped.
//!
//! Files are written with [`Store::put_if_absent`], so writing the same
//! range twice (a retry, or a process replaying the same updates) keeps
//! the first copy. Readers should still treat `(run_id, epoch, node_id)`
//! as unique, since a replay that stops at different places writes
//! different ranges.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::thread::JoinHandle;

use arrow_array::builder::{
    BooleanBuilder, Float64Builder, Int64Builder, ListBuilder, StringBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use watershed_core::{ChangeSink, Graph, NodeId, Type, Value};

pub mod read;

// ---------------------------------------------------------------- storage

/// What [`Store::put_if_absent`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Put {
    Written,
    /// An object with that key was already there, and was kept.
    AlreadyThere,
}

/// Somewhere to keep files by key.
///
/// On S3, `put_if_absent` is a `PutObject` with `If-None-Match: *`: a
/// `412 Precondition Failed` means [`Put::AlreadyThere`], and a `409
/// Conflict` should be retried. That store is not written yet.
pub trait Store: Send + 'static {
    fn put_if_absent(&self, key: &str, bytes: &[u8]) -> io::Result<Put>;
}

/// Files in a folder on this machine.
pub struct LocalStore {
    root: PathBuf,
}

impl LocalStore {
    pub fn new(root: impl Into<PathBuf>) -> LocalStore {
        LocalStore { root: root.into() }
    }
}

impl Store for LocalStore {
    /// Writes to a temporary file, then links it into place, which fails if
    /// the name is taken: the file appears whole or not at all.
    fn put_if_absent(&self, key: &str, bytes: &[u8]) -> io::Result<Put> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = self.root.join(key);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        {
            use std::io::Write;
            let mut file = fs::File::create(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        let linked = fs::hard_link(&temporary, &path);
        fs::remove_file(&temporary)?;
        match linked {
            Ok(()) => Ok(Put::Written),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(Put::AlreadyThere),
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------- options

pub struct Options {
    /// Names this run of the graph. Node ids and update numbers only mean
    /// something within one run, so every row carries it.
    pub run_id: String,
    /// A file is written once this many changes are waiting (at the end of
    /// an update, so an update is never split between files).
    pub rows_per_file: usize,
    /// How many updates can wait for the writer before updates wait too.
    pub queue_updates: usize,
}

impl Options {
    pub fn new(run_id: impl Into<String>) -> Options {
        Options {
            run_id: run_id.into(),
            rows_per_file: 1_000_000,
            queue_updates: 1_024,
        }
    }
}

/// What a writer did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub files_written: usize,
    /// Files whose key already existed, left as they were.
    pub files_already_there: usize,
    pub rows: usize,
    pub bytes: usize,
}

// ---------------------------------------------------------------- the sink

struct NewNode {
    id: u32,
    name: String,
    ty: Type,
}

enum Message {
    Update {
        epoch: u64,
        values: Vec<(u32, Value)>,
        nodes: Vec<NewNode>,
    },
    Flush(Sender<io::Result<()>>),
    Stop,
}

/// Makes a sink to attach to a graph, and the writer that controls it.
pub fn parquet_sink(store: impl Store, options: Options) -> (Box<dyn ChangeSink>, Writer) {
    let (tx, rx) = sync_channel(options.queue_updates.max(1));
    let flushed = Arc::new(AtomicU64::new(0));
    let state = WriterThread {
        store,
        run_id: options.run_id,
        rows_per_file: options.rows_per_file.max(1),
        flushed: flushed.clone(),
        pending: Pending::default(),
        stats: Stats::default(),
    };
    let thread = std::thread::Builder::new()
        .name("watershed-parquet".into())
        .spawn(move || state.run(rx))
        .expect("could not start the parquet writer thread");
    let sink = GraphSide {
        tx: tx.clone(),
        known_nodes: 0,
    };
    let writer = Writer {
        tx,
        thread: Some(thread),
        flushed,
    };
    (Box::new(sink), writer)
}

/// The part that runs on the graph's thread: copies each update's changes
/// and queues them.
struct GraphSide {
    tx: SyncSender<Message>,
    known_nodes: usize,
}

impl ChangeSink for GraphSide {
    fn update(&mut self, g: &Graph, epoch: u64, changed: &[NodeId]) {
        let known = self.known_nodes;
        let new: Vec<NodeId> = g.node_ids().skip(known).collect();
        let mut values = Vec::with_capacity(changed.len() + new.len());
        // A new node is written with its value, whether or not it changed.
        for &id in changed.iter().filter(|id| id.index() < known).chain(&new) {
            values.push((id.index() as u32, g.value(id).clone()));
        }
        let nodes = new
            .iter()
            .map(|&id| NewNode {
                id: id.index() as u32,
                name: g.name(id).to_string(),
                ty: g.node_type(id),
            })
            .collect();
        self.known_nodes = g.len();
        // If the writer has stopped, its error comes back from `Writer`.
        let _ = self.tx.send(Message::Update {
            epoch,
            values,
            nodes,
        });
    }
}

/// Controls the background writer.
pub struct Writer {
    tx: SyncSender<Message>,
    thread: Option<JoinHandle<io::Result<Stats>>>,
    flushed: Arc<AtomicU64>,
}

impl Writer {
    /// Writes everything queued so far, and waits until it is stored.
    pub fn flush(&self) -> io::Result<()> {
        let (reply, answer) = channel();
        self.tx.send(Message::Flush(reply)).map_err(|_| stopped())?;
        answer.recv().map_err(|_| stopped())?
    }

    /// The last update whose changes are all stored, if any. History kept
    /// in memory up to here can be forgotten safely.
    pub fn flushed_through(&self) -> Option<u64> {
        self.flushed.load(Ordering::Acquire).checked_sub(1)
    }

    /// Writes what is queued and stops. Detach the sink from the graph
    /// first: updates after this are not written.
    pub fn close(mut self) -> io::Result<Stats> {
        let _ = self.tx.send(Message::Stop);
        self.thread
            .take()
            .expect("closed once")
            .join()
            .map_err(|_| io::Error::other("the parquet writer panicked"))?
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.tx.send(Message::Stop);
            let _ = thread.join();
        }
    }
}

fn stopped() -> io::Error {
    io::Error::other("the parquet writer has stopped; `close` says why")
}

// ---------------------------------------------------------------- the writer thread

#[derive(Default)]
struct Pending {
    first_epoch: Option<u64>,
    last_epoch: u64,
    changes: Vec<(u64, u32, Value)>,
    nodes: Vec<(NewNode, u64)>,
}

struct WriterThread<S> {
    store: S,
    run_id: String,
    rows_per_file: usize,
    flushed: Arc<AtomicU64>,
    pending: Pending,
    stats: Stats,
}

impl<S: Store> WriterThread<S> {
    fn run(mut self, rx: Receiver<Message>) -> io::Result<Stats> {
        while let Ok(message) = rx.recv() {
            match message {
                Message::Update {
                    epoch,
                    values,
                    nodes,
                } => {
                    let p = &mut self.pending;
                    p.first_epoch.get_or_insert(epoch);
                    p.last_epoch = epoch;
                    p.changes
                        .extend(values.into_iter().map(|(id, v)| (epoch, id, v)));
                    p.nodes.extend(nodes.into_iter().map(|n| (n, epoch)));
                    if p.changes.len() >= self.rows_per_file {
                        self.write()?;
                    }
                }
                Message::Flush(reply) => {
                    let result = self.write();
                    let failed = result
                        .as_ref()
                        .err()
                        .map(|e| io::Error::new(e.kind(), e.to_string()));
                    let _ = reply.send(result);
                    if let Some(e) = failed {
                        return Err(e);
                    }
                }
                Message::Stop => break,
            }
        }
        self.write()?;
        Ok(self.stats)
    }

    /// Writes what is pending: nodes first, so every change's node is
    /// described by the time the change can be read.
    fn write(&mut self) -> io::Result<()> {
        let Some(first) = self.pending.first_epoch else {
            return Ok(());
        };
        let pending = std::mem::take(&mut self.pending);
        let range = format!("{first:020}-{:020}.parquet", pending.last_epoch);
        if !pending.nodes.is_empty() {
            let bytes = encode(nodes_batch(&self.run_id, &pending.nodes)?)?;
            self.put(&format!("run={}/nodes/{range}", self.run_id), &bytes)?;
        }
        if !pending.changes.is_empty() {
            let rows = pending.changes.len();
            let mut changes = pending.changes;
            // Sorted by node, then update: a node's history is read in one place.
            changes.sort_by_key(|&(epoch, id, _)| (id, epoch));
            let bytes = encode(changes_batch(&self.run_id, &changes)?)?;
            self.put(&format!("run={}/changes/{range}", self.run_id), &bytes)?;
            self.stats.rows += rows;
        }
        self.flushed
            .store(pending.last_epoch + 1, Ordering::Release);
        Ok(())
    }

    fn put(&mut self, key: &str, bytes: &[u8]) -> io::Result<()> {
        match self.store.put_if_absent(key, bytes)? {
            Put::Written => {
                self.stats.files_written += 1;
                self.stats.bytes += bytes.len();
            }
            Put::AlreadyThere => self.stats.files_already_there += 1,
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- encoding

pub(crate) fn changes_schema() -> Schema {
    Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false),
        Field::new("epoch", DataType::Int64, false),
        Field::new("node_id", DataType::Int64, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("f64", DataType::Float64, true),
        Field::new("i64", DataType::Int64, true),
        Field::new("bool", DataType::Boolean, true),
        Field::new("text", DataType::Utf8, true),
        Field::new("list", list_type(), true),
    ])
}

/// A list of strings, as the `list` column holds it.
pub(crate) fn list_type() -> DataType {
    DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)))
}

pub(crate) fn nodes_schema() -> Schema {
    Schema::new(vec![
        Field::new("run_id", DataType::Utf8, false),
        Field::new("node_id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, false),
        Field::new("created_epoch", DataType::Int64, false),
    ])
}

fn type_name(ty: Type) -> &'static str {
    match ty {
        Type::F64 => "f64",
        Type::I64 => "i64",
        Type::Decimal => "decimal",
        Type::Bool => "bool",
        Type::Str => "str",
        Type::StrList => "list",
    }
}

fn changes_batch(run_id: &str, rows: &[(u64, u32, Value)]) -> io::Result<RecordBatch> {
    let n = rows.len();
    let mut run = StringBuilder::with_capacity(n, n * run_id.len());
    let mut epoch = Int64Builder::with_capacity(n);
    let mut node = Int64Builder::with_capacity(n);
    let mut kind = StringBuilder::with_capacity(n, n * 4);
    let mut f = Float64Builder::with_capacity(n);
    let mut i = Int64Builder::with_capacity(n);
    let mut b = BooleanBuilder::with_capacity(n);
    let mut text = StringBuilder::new();
    let mut list = ListBuilder::new(StringBuilder::new());
    for (e, id, value) in rows {
        run.append_value(run_id);
        epoch.append_value(to_i64(*e)?);
        node.append_value(i64::from(*id));
        // A decimal is written as its digits, in the text column: exact,
        // and with the decimal places it had.
        let digits = match value {
            Value::Decimal(x) => x.to_string(),
            _ => String::new(),
        };
        let (k, fv, iv, bv, tv, lv) = match value {
            Value::F64(x) => ("f64", Some(*x), None, None, None, None),
            Value::I64(x) => ("i64", None, Some(*x), None, None, None),
            Value::Decimal(_) => ("decimal", None, None, None, Some(digits.as_str()), None),
            Value::Bool(x) => ("bool", None, None, Some(*x), None, None),
            Value::Str(s) => ("str", None, None, None, Some(&**s), None),
            Value::List(items) => ("list", None, None, None, None, Some(items)),
            Value::Error(s) => ("error", None, None, None, Some(&**s), None),
        };
        kind.append_value(k);
        f.append_option(fv);
        i.append_option(iv);
        b.append_option(bv);
        text.append_option(tv);
        match lv {
            Some(items) => {
                for item in items.iter() {
                    list.values().append_value(&**item);
                }
                list.append(true);
            }
            None => list.append(false),
        }
    }
    let columns: Vec<ArrayRef> = vec![
        Arc::new(run.finish()),
        Arc::new(epoch.finish()),
        Arc::new(node.finish()),
        Arc::new(kind.finish()),
        Arc::new(f.finish()),
        Arc::new(i.finish()),
        Arc::new(b.finish()),
        Arc::new(text.finish()),
        Arc::new(list.finish()),
    ];
    RecordBatch::try_new(Arc::new(changes_schema()), columns).map_err(io::Error::other)
}

fn nodes_batch(run_id: &str, rows: &[(NewNode, u64)]) -> io::Result<RecordBatch> {
    let mut run = StringBuilder::new();
    let mut id = Int64Builder::new();
    let mut name = StringBuilder::new();
    let mut ty = StringBuilder::new();
    let mut created = Int64Builder::new();
    for (node, epoch) in rows {
        run.append_value(run_id);
        id.append_value(i64::from(node.id));
        name.append_value(&node.name);
        ty.append_value(type_name(node.ty));
        created.append_value(to_i64(*epoch)?);
    }
    let columns: Vec<ArrayRef> = vec![
        Arc::new(run.finish()),
        Arc::new(id.finish()),
        Arc::new(name.finish()),
        Arc::new(ty.finish()),
        Arc::new(created.finish()),
    ];
    RecordBatch::try_new(Arc::new(nodes_schema()), columns).map_err(io::Error::other)
}

/// Update numbers are written as signed integers, which more SQL engines
/// read than unsigned ones.
fn to_i64(epoch: u64) -> io::Result<i64> {
    i64::try_from(epoch).map_err(|_| io::Error::other("update number too large to write"))
}

fn encode(batch: RecordBatch) -> io::Result<Vec<u8>> {
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .build();
    let mut bytes = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut bytes, batch.schema(), Some(properties))
        .map_err(io::Error::other)?;
    writer.write(&batch).map_err(io::Error::other)?;
    writer.close().map_err(io::Error::other)?;
    Ok(bytes)
}
