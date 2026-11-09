//! Typed handles to nodes. A handle is a small copyable value (an id and a
//! type marker); the node itself lives in the [`Graph`](crate::Graph).

use std::fmt;
use std::marker::PhantomData;

/// The position of a node in its graph. Ids are given out in order, and a
/// node can only depend on nodes that already exist, so a lower id is never
/// above a higher one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub(crate) u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Identifies one graph, so a handle from one graph cannot be used in another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GraphId(pub(crate) u64);

/// A reference to a node in a particular graph, as stored in expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub graph: GraphId,
    pub node: NodeId,
}

/// A calculated node holding a `T`.
pub struct Node<T> {
    pub(crate) at: NodeRef,
    _type: PhantomData<fn() -> T>,
}

/// An input node holding a `T`. Only inputs can be [`set`](crate::Graph::set).
/// An `Input<T>` can be used anywhere a `Node<T>` can.
pub struct Input<T> {
    pub(crate) at: NodeRef,
    _type: PhantomData<fn() -> T>,
}

impl<T> Node<T> {
    pub(crate) fn new(at: NodeRef) -> Self {
        Node {
            at,
            _type: PhantomData,
        }
    }
    pub fn id(self) -> NodeId {
        self.at.node
    }
}

impl<T> Input<T> {
    pub(crate) fn new(at: NodeRef) -> Self {
        Input {
            at,
            _type: PhantomData,
        }
    }
    pub fn id(self) -> NodeId {
        self.at.node
    }
    /// The same node, as a plain `Node<T>`.
    pub fn node(self) -> Node<T> {
        Node::new(self.at)
    }
}

// Written by hand because `#[derive]` would wrongly require `T: Clone`.
impl<T> Clone for Node<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Node<T> {}
impl<T> Clone for Input<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Input<T> {}

impl<T> fmt::Debug for Node<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node({})", self.at.node)
    }
}
impl<T> fmt::Debug for Input<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Input({})", self.at.node)
    }
}

/// Anything that names a node holding a `T`: a [`Node<T>`] or an [`Input<T>`].
pub trait Handle<T> {
    fn node_ref(&self) -> NodeRef;
}
impl<T> Handle<T> for Node<T> {
    fn node_ref(&self) -> NodeRef {
        self.at
    }
}
impl<T> Handle<T> for Input<T> {
    fn node_ref(&self) -> NodeRef {
        self.at
    }
}
