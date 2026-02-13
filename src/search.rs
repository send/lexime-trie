use crate::{DoubleArray, Label};

/// Result of a common prefix search match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrefixMatch {
    /// Length of the matched prefix (in labels).
    pub len: usize,
    /// The value_id associated with the matched key.
    pub value_id: u32,
}

/// Result of a predictive search match.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch<L> {
    /// The full matched key.
    pub key: Vec<L>,
    /// The value_id associated with the matched key.
    pub value_id: u32,
}

/// Result of probing a key in the trie.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeResult {
    /// The value_id if the key exists as a complete entry.
    pub value: Option<u32>,
    /// Whether the key is a prefix of other entries (excluding terminal children).
    pub has_children: bool,
}

impl<L: Label> DoubleArray<L> {
    /// Exact match search. Returns the value_id if the key exists.
    pub fn exact_match(&self, _key: &[L]) -> Option<u32> {
        todo!("exact_match will be implemented in feat/search-basic")
    }

    /// Common prefix search. Returns an iterator over all prefixes of `query`
    /// that exist as keys in the trie.
    pub fn common_prefix_search<'a>(
        &'a self,
        _query: &'a [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'a {
        std::iter::empty()
    }

    /// Predictive search. Returns an iterator over all keys that start with `prefix`.
    pub fn predictive_search<'a>(
        &'a self,
        _prefix: &'a [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'a {
        std::iter::empty()
    }

    /// Probe a key. Returns whether the key exists and whether it has children.
    pub fn probe(&self, _key: &[L]) -> ProbeResult {
        todo!("probe will be implemented in feat/probe")
    }
}
