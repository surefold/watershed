use std::fmt;

use crate::handle::NodeId;
use crate::ir::TypeError;
use crate::value::Type;

/// Everything that can go wrong when building or reading a graph.
#[derive(Debug, Clone, PartialEq)]
pub enum CalcError {
    /// Two nodes were given the same name.
    DuplicateName(String),
    /// A handle or expression from a different graph was used.
    ForeignNode,
    /// No node has this id (only possible through the untyped API).
    UnknownNode(NodeId),
    /// Only inputs can be set.
    NotAnInput(String),
    /// A value of the wrong type was given to or read from a node.
    TypeMismatch {
        node: String,
        expected: Type,
        found: Option<Type>,
    },
    /// An expression's types do not fit together.
    Type(TypeError),
    /// A request that does not fit the graph's structure, such as adding to
    /// an aggregate a member that sits above it.
    Invalid(String),
    /// The node's current value is an error, for example a division by zero
    /// in it or in something it depends on.
    Evaluation { node: String, message: String },
    /// An earlier value was asked for, but history is not being kept.
    HistoryOff,
    /// An update was asked for that history does not cover: forgotten,
    /// before history started, or not happened yet.
    EpochUnavailable {
        epoch: u64,
        oldest: u64,
        latest: u64,
    },
    /// The node was made after the update that was asked for.
    NotCreatedYet { node: String, epoch: u64 },
}

impl fmt::Display for CalcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CalcError::DuplicateName(name) => write!(f, "a node named {name:?} already exists"),
            CalcError::ForeignNode => write!(f, "a node from a different graph was used"),
            CalcError::UnknownNode(id) => write!(f, "there is no node {id}"),
            CalcError::NotAnInput(name) => write!(
                f,
                "{name:?} is calculated, not an input, so it cannot be set"
            ),
            CalcError::TypeMismatch {
                node,
                expected,
                found,
            } => match found {
                Some(found) => write!(f, "{node:?} holds {expected}, not {found}"),
                None => write!(f, "{node:?} holds {expected}, not an error"),
            },
            CalcError::Type(e) => write!(f, "type error: {e}"),
            CalcError::Invalid(message) => f.write_str(message),
            CalcError::HistoryOff => f.write_str("history is not being kept"),
            CalcError::EpochUnavailable {
                epoch,
                oldest,
                latest,
            } => write!(
                f,
                "update {epoch} cannot be read: history covers updates {oldest} to {latest}"
            ),
            CalcError::NotCreatedYet { node, epoch } => {
                write!(f, "{node:?} did not exist at update {epoch}")
            }
            CalcError::Evaluation { node, message } => {
                write!(f, "{node:?} could not be calculated: {message}")
            }
        }
    }
}

impl std::error::Error for CalcError {}

impl From<TypeError> for CalcError {
    fn from(e: TypeError) -> Self {
        CalcError::Type(e)
    }
}
