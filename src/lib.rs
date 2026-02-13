mod build;
mod code_map;
mod label;
mod node;
mod search;
mod serial;

use std::marker::PhantomData;

pub use code_map::CodeMapper;
pub use label::Label;
pub use node::Node;
pub use search::{PrefixMatch, ProbeResult, SearchMatch};

/// Errors that can occur during trie operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrieError {
    /// The binary data has an invalid magic number.
    InvalidMagic,
    /// The binary data has an unsupported version.
    InvalidVersion,
    /// The binary data is truncated or corrupted.
    TruncatedData,
}

impl std::fmt::Display for TrieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrieError::InvalidMagic => write!(f, "invalid magic number"),
            TrieError::InvalidVersion => write!(f, "unsupported version"),
            TrieError::TruncatedData => write!(f, "truncated or corrupted data"),
        }
    }
}

impl std::error::Error for TrieError {}

/// A double-array trie supporting exact match, common prefix search,
/// predictive search, and probe operations.
#[derive(Clone, Debug)]
pub struct DoubleArray<L: Label> {
    pub(crate) nodes: Vec<Node>,
    pub(crate) siblings: Vec<u32>,
    pub(crate) code_map: CodeMapper,
    _phantom: PhantomData<L>,
}

impl<L: Label> DoubleArray<L> {
    /// Creates a new DoubleArray with the given components.
    pub(crate) fn new(nodes: Vec<Node>, siblings: Vec<u32>, code_map: CodeMapper) -> Self {
        Self {
            nodes,
            siblings,
            code_map,
            _phantom: PhantomData,
        }
    }

    /// Returns the number of nodes in the trie.
    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }
}
