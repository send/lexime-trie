//! A char-wise Double-Array Trie with zero dependencies.
//!
//! This crate provides [`DoubleArray`], a compact trie implementation based on the
//! double-array structure. It supports exact match, common prefix search, predictive
//! search, and probe operations over sequences of [`Label`] elements (`u8` or `char`).
//!
//! For zero-copy access to memory-mapped files, see [`DoubleArrayRef`].
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
//!
//! # Zero-copy deserialization
//!
//! The input buffer must be aligned to at least 4 bytes. Buffers from
//! `mmap` or page-aligned allocations satisfy this requirement.
//!
//! ```
//! use lexime_trie::{DoubleArray, DoubleArrayRef};
//!
//! let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
//! let da = DoubleArray::<u8>::build(&keys);
//! let raw = da.as_bytes();
//!
//! // Allocate a 4-byte aligned buffer (Vec<u32> guarantees this).
//! // In production, use mmap which returns page-aligned memory.
//! let mut backing = vec![0u32; (raw.len() + 3) / 4];
//! let bytes: &mut [u8] = unsafe {
//!     std::slice::from_raw_parts_mut(backing.as_mut_ptr() as *mut u8, raw.len())
//! };
//! bytes.copy_from_slice(&raw);
//!
//! let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(bytes).unwrap();
//! assert_eq!(da_ref.exact_match(b"abc"), Some(2));
//! ```

#![warn(missing_docs)]

#[cfg(not(target_endian = "little"))]
compile_error!("lexime-trie requires a little-endian platform");

mod build;
mod code_map;
mod da_ref;
mod label;
mod node;
mod search;
mod serial;
mod view;

use std::marker::PhantomData;

pub use da_ref::DoubleArrayRef;
pub use label::Label;
pub use search::{PrefixMatch, ProbeResult, SearchMatch};

use code_map::CodeMapper;
use node::Node;

/// Errors that can occur during trie operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrieError {
    /// The binary data has an invalid magic number.
    InvalidMagic,
    /// The binary data has an unsupported version.
    InvalidVersion,
    /// The binary data is truncated or corrupted.
    TruncatedData,
    /// The byte buffer is not properly aligned for zero-copy access.
    MisalignedData,
}

impl std::fmt::Display for TrieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrieError::InvalidMagic => write!(f, "invalid magic number"),
            TrieError::InvalidVersion => write!(f, "unsupported version"),
            TrieError::TruncatedData => write!(f, "truncated or corrupted data"),
            TrieError::MisalignedData => write!(f, "misaligned data for zero-copy access"),
        }
    }
}

impl std::error::Error for TrieError {}

/// A double-array trie supporting exact match, common prefix search,
/// predictive search, and probe operations.
///
/// ## Internal layout (v3)
///
/// - `nodes[0]` is an invalid sentinel, `nodes[1]` is the root.
/// - For any parent `p`, its children are found at
///   `children_list[child_offsets[p]..child_offsets[p+1]]`. Children appear in
///   code-ascending order (so the terminal child, if any, is first).
#[derive(Clone, Debug)]
pub struct DoubleArray<L: Label> {
    pub(crate) nodes: Vec<Node>,
    /// CSR-style offsets into `children_list`, length `nodes.len() + 1`.
    pub(crate) child_offsets: Vec<u32>,
    /// Flat list of child indices, grouped by parent in node-index order.
    pub(crate) children_list: Vec<u32>,
    pub(crate) code_map: CodeMapper,
    _phantom: PhantomData<L>,
}

impl<L: Label> DoubleArray<L> {
    /// Creates a new DoubleArray with the given components.
    pub(crate) fn new(
        nodes: Vec<Node>,
        child_offsets: Vec<u32>,
        children_list: Vec<u32>,
        code_map: CodeMapper,
    ) -> Self {
        Self {
            nodes,
            child_offsets,
            children_list,
            code_map,
            _phantom: PhantomData,
        }
    }

    /// Returns the number of node slots in the trie's underlying array.
    ///
    /// This is the length of the `nodes` vector after trailing-trim —
    /// not the number of "live" nodes. The sentinel at index 0 and any
    /// unused free slots embedded in the array are counted. For a trie
    /// built from N keys with moderate branching, slot count is typically
    /// a small multiple of N (free slots between occupied ones from the
    /// double-array XOR placement).
    ///
    /// Useful for sanity checks and rough memory estimates. For exact
    /// live-node counts, iterate `self.nodes` and filter by
    /// `!= Node::default()`.
    pub fn node_slot_count(&self) -> usize {
        self.nodes.len()
    }
}
