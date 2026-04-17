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
//! use lexime_trie::{DoubleArray, TrieSearch};
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
//! use lexime_trie::{DoubleArray, DoubleArrayRef, TrieSearch};
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
//! let da_ref = DoubleArrayRef::<u8>::from_bytes(bytes).unwrap();
//! assert_eq!(da_ref.exact_match(b"abc"), Some(2));
//! ```

#![warn(missing_docs)]

#[cfg(not(target_endian = "little"))]
compile_error!("lexime-trie requires a little-endian platform");

mod backed;
mod build;
mod code_map;
mod da_ref;
mod label;
mod node;
mod search;
mod serial;
mod view;

#[cfg(test)]
mod test_support;

use std::marker::PhantomData;

pub use backed::{CloneStableBacking, DoubleArrayBacked, StableBacking};
pub use da_ref::DoubleArrayRef;
pub use label::Label;
pub use search::{PrefixMatch, ProbeResult, SearchMatch, TrieSearch};

use code_map::CodeMapper;
use node::Node;

/// Errors that can occur when loading a serialized trie.
///
/// Marked `#[non_exhaustive]` so future failure modes can be
/// introduced without a breaking change — callers matching on
/// specific variants should include a catch-all arm.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrieError {
    /// The first four bytes do not match the `LXTR` magic identifier.
    InvalidMagic,
    /// The buffer declares a version this build does not support. The
    /// payload is the actual version byte observed, preserved so the
    /// caller can report it in diagnostics.
    InvalidVersion(u8),
    /// The buffer ends before the header, a declared section, or the
    /// code-map block is fully present. Indicates the bytes are
    /// incomplete; the remedy is to re-obtain a complete buffer.
    TruncatedData,
    /// The buffer pointer does not meet the 4-byte alignment required
    /// for zero-copy access via [`DoubleArrayRef`] / [`DoubleArrayBacked`].
    MisalignedData,
    /// The buffer is long enough but violates a v3 structural invariant
    /// (declared counts, the CSR length/endpoint relationship, or — under
    /// [`TrieSearch::validate_strict`] — child-offset monotonicity).
    /// Indicates the bytes are corrupt rather than truncated; the remedy
    /// is to regenerate the trie rather than re-fetch the buffer.
    InvalidStructure,
}

impl std::fmt::Display for TrieError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrieError::InvalidMagic => write!(f, "invalid magic number"),
            TrieError::InvalidVersion(v) => write!(f, "unsupported version: {v}"),
            TrieError::TruncatedData => write!(f, "truncated data"),
            TrieError::MisalignedData => write!(f, "misaligned data for zero-copy access"),
            TrieError::InvalidStructure => write!(f, "invalid trie structure"),
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
}
