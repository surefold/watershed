//! Passing each update's changes to something outside the graph, such as a
//! writer that keeps them in files or a table.

use crate::graph::Graph;
use crate::handle::NodeId;

/// Receives every update's changes, from [`Graph::attach_sink`].
///
/// It is called on the thread running the update, at the end of each
/// [`Graph::stabilize`], so it should hand the work on (to a queue and a
/// background thread, say) rather than do slow work itself. It can read
/// anything in the graph: values, names and types are as of that update.
pub trait ChangeSink: Send + Sync {
    /// `changed` holds the nodes whose value changed in update `epoch`, in
    /// the order they changed. Nodes made since the last call are not in
    /// it unless their value also changed: compare [`Graph::len`] with the
    /// number of nodes seen so far to find them.
    ///
    /// When the sink is attached it is called once with every node, as a
    /// starting point, and the current update's number.
    fn update(&mut self, graph: &Graph, epoch: u64, changed: &[NodeId]);
}
