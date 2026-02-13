//! A char-wise Double-Array Trie with zero dependencies.
//!
//! This crate provides [`DoubleArray`], a compact trie implementation based on the
//! double-array structure. It supports exact match, common prefix search, predictive
//! search, and probe operations over sequences of [`Label`] elements (`u8` or `char`).
//!
//! # Quick start
//!
//! ```
//! use lexime_trie::DoubleArray;
//!
//! let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc"];
//! let da = DoubleArray::<u8>::build(&keys);
//! assert_eq!(da.exact_match(b"abc"), Some(2));
//! ```

#![warn(missing_docs)]

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
