use std::marker::PhantomData;

use crate::view::TrieView;
use crate::{DoubleArray, Label, TrieError};

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

/// Search and introspection operations shared by [`DoubleArray`] (owned),
/// [`DoubleArrayRef`](crate::DoubleArrayRef) (zero-copy borrowed), and
/// [`DoubleArrayBacked`](crate::DoubleArrayBacked) (owned-buffer wrapper).
///
/// **Not dyn-compatible.** `common_prefix_search` and `predictive_search`
/// return `impl Iterator<...>` (RPIT), which is not permitted through a
/// trait object. Use the trait as a bound on generic functions
/// (`fn foo<T: TrieSearch<u8>>(t: &T)`) rather than `&dyn TrieSearch<u8>`.
///
/// Users should bring this trait into scope to call `exact_match`,
/// `common_prefix_search`, `predictive_search`, or `probe` on any of
/// the three trie representations:
///
/// ```
/// use lexime_trie::{DoubleArray, TrieSearch};
///
/// let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
/// let da = DoubleArray::<u8>::build(&keys);
/// assert_eq!(da.exact_match(b"abc"), Some(2));
/// ```
///
/// A generic function can then work over either representation:
///
/// ```
/// use lexime_trie::{DoubleArray, TrieSearch};
///
/// fn count_hits<T: TrieSearch<u8>>(trie: &T, needles: &[&[u8]]) -> usize {
///     needles.iter().filter(|k| trie.exact_match(k).is_some()).count()
/// }
///
/// let da = DoubleArray::<u8>::build(&[b"abc".as_slice(), b"abd"]);
/// assert_eq!(count_hits(&da, &[b"abc", b"xyz"]), 1);
/// ```
pub trait TrieSearch<L: Label> {
    /// Returns the number of node slots in the trie's underlying array.
    ///
    /// This is the length of the `nodes` vector after trailing-trim —
    /// not the count of "live" nodes. The sentinel at index 0 and any
    /// unused free slots embedded in the array are counted. For a trie
    /// built from N keys with moderate branching, the slot count is
    /// typically a small multiple of N (free slots between occupied
    /// ones from the double-array XOR placement). Useful for sanity
    /// checks and rough memory estimates.
    fn node_slot_count(&self) -> usize;

    /// Exact match search. Returns the `value_id` assigned at build time
    /// (i.e. the position in the original key slice) if the key exists,
    /// `None` otherwise.
    fn exact_match(&self, key: &[L]) -> Option<u32>;

    /// Common prefix search: returns every prefix of `query` that exists
    /// as a key in the trie, in prefix-length ascending order.
    fn common_prefix_search<'a>(&'a self, query: &'a [L])
        -> impl Iterator<Item = PrefixMatch> + 'a;

    /// Predictive search: returns every key starting with `prefix`.
    /// Keys are reconstructed from the trie structure via the CSR
    /// children list; order is code-ascending per node (terminal first).
    fn predictive_search<'a>(
        &'a self,
        prefix: &'a [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'a;

    /// Probe a key for its existence and whether it is a prefix of other
    /// stored keys. The four possible states of [`ProbeResult`]:
    /// - `value=None, has_children=false`: key not in trie, not a prefix
    /// - `value=None, has_children=true`: prefix-only
    /// - `value=Some, has_children=false`: exact match, no extension
    /// - `value=Some, has_children=true`: exact match and prefix
    fn probe(&self, key: &[L]) -> ProbeResult;

    /// Full O(N) structural validation. Runs the cheap checks already
    /// performed by the `from_bytes` constructors (section lengths and
    /// end offset) plus monotonicity of `child_offsets`, which the
    /// zero-copy load path skips. Call this after loading a trie
    /// from an untrusted source to reject malformed inputs before
    /// issuing any queries; corrupted offsets cannot cause UB (Rust
    /// slice indexing is bounds-checked) but can produce wrong
    /// results at query time.
    ///
    /// ```
    /// use lexime_trie::{DoubleArray, DoubleArrayRef, TrieSearch};
    ///
    /// # let da = DoubleArray::<u8>::build(&[b"hello".as_slice()]);
    /// # let bytes = da.as_bytes();
    /// // Align the buffer; `mmap`'d regions already satisfy this.
    /// let mut aligned = vec![0u32; bytes.len().div_ceil(4)];
    /// let slice: &mut [u8] = unsafe {
    ///     std::slice::from_raw_parts_mut(aligned.as_mut_ptr() as *mut u8, bytes.len())
    /// };
    /// slice.copy_from_slice(&bytes);
    /// let trie = DoubleArrayRef::<u8>::from_bytes(slice)?;
    /// trie.validate_strict()?;
    /// # Ok::<(), lexime_trie::TrieError>(())
    /// ```
    fn validate_strict(&self) -> Result<(), TrieError>;
}

impl<L: Label> DoubleArray<L> {
    /// Returns a `TrieView` borrowing this trie's data.
    #[inline]
    pub(crate) fn view(&self) -> TrieView<'_, L> {
        TrieView {
            nodes: &self.nodes,
            child_offsets: &self.child_offsets,
            children_list: &self.children_list,
            code_map: &self.code_map,
            _phantom: PhantomData,
        }
    }
}

impl<L: Label> TrieSearch<L> for DoubleArray<L> {
    #[inline]
    fn node_slot_count(&self) -> usize {
        self.nodes.len()
    }

    #[inline]
    fn exact_match(&self, key: &[L]) -> Option<u32> {
        self.view().exact_match(key)
    }

    fn common_prefix_search<'a>(
        &'a self,
        query: &'a [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'a {
        self.view().common_prefix_search(query)
    }

    fn predictive_search<'a>(
        &'a self,
        prefix: &'a [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'a {
        self.view().predictive_search(prefix)
    }

    #[inline]
    fn probe(&self, key: &[L]) -> ProbeResult {
        self.view().probe(key)
    }

    fn validate_strict(&self) -> Result<(), TrieError> {
        crate::serial::validate_strict(&self.nodes, &self.child_offsets, &self.children_list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DoubleArray;

    fn build_u8(keys: &[&[u8]]) -> DoubleArray<u8> {
        DoubleArray::build(keys)
    }

    fn build_char(keys: &[&str]) -> DoubleArray<char> {
        let mut char_keys: Vec<Vec<char>> = keys.iter().map(|s| s.chars().collect()).collect();
        char_keys.sort();
        DoubleArray::build(&char_keys)
    }

    // === exact_match tests ===

    #[test]
    fn exact_match_found() {
        let da = build_u8(&[b"abc", b"abd", b"xyz"]);
        assert_eq!(da.exact_match(b"abc"), Some(0));
        assert_eq!(da.exact_match(b"abd"), Some(1));
        assert_eq!(da.exact_match(b"xyz"), Some(2));
    }

    #[test]
    fn exact_match_not_found() {
        let da = build_u8(&[b"abc", b"abd"]);
        assert_eq!(da.exact_match(b"ab"), None);
        assert_eq!(da.exact_match(b"abcd"), None);
        assert_eq!(da.exact_match(b"zzz"), None);
        assert_eq!(da.exact_match(b""), None);
    }

    #[test]
    fn exact_match_prefix_only() {
        // "ab" is a prefix of "abc" but not a key itself
        let da = build_u8(&[b"abc"]);
        assert_eq!(da.exact_match(b"ab"), None);
        assert_eq!(da.exact_match(b"a"), None);
        assert_eq!(da.exact_match(b"abc"), Some(0));
    }

    #[test]
    fn exact_match_empty_trie() {
        let da = build_u8(&[]);
        assert_eq!(da.exact_match(b"abc"), None);
    }

    #[test]
    fn exact_match_char_keys() {
        let da = build_char(&["あい", "あう", "かき"]);
        assert!(da
            .exact_match(&"あい".chars().collect::<Vec<_>>())
            .is_some());
        assert!(da
            .exact_match(&"あう".chars().collect::<Vec<_>>())
            .is_some());
        assert!(da
            .exact_match(&"かき".chars().collect::<Vec<_>>())
            .is_some());
        assert_eq!(da.exact_match(&"あ".chars().collect::<Vec<_>>()), None);
        assert_eq!(da.exact_match(&"か".chars().collect::<Vec<_>>()), None);
    }

    #[test]
    fn exact_match_all_keys_round_trip() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc", b"bcd"];
        let da = build_u8(&keys);
        for (i, key) in keys.iter().enumerate() {
            assert_eq!(
                da.exact_match(key),
                Some(i as u32),
                "key {:?} should have value_id {}",
                std::str::from_utf8(key).unwrap(),
                i
            );
        }
    }

    // === common_prefix_search tests ===

    #[test]
    fn common_prefix_search_basic() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b"];
        let da = build_u8(&keys);

        let results: Vec<PrefixMatch> = da.common_prefix_search(b"abcd").collect();
        assert_eq!(results.len(), 3);
        assert_eq!(
            results[0],
            PrefixMatch {
                len: 1,
                value_id: 0
            }
        ); // "a"
        assert_eq!(
            results[1],
            PrefixMatch {
                len: 2,
                value_id: 1
            }
        ); // "ab"
        assert_eq!(
            results[2],
            PrefixMatch {
                len: 3,
                value_id: 2
            }
        ); // "abc"
    }

    #[test]
    fn common_prefix_search_no_match() {
        let da = build_u8(&[b"abc"]);
        let results: Vec<PrefixMatch> = da.common_prefix_search(b"xyz").collect();
        assert!(results.is_empty());
    }

    #[test]
    fn common_prefix_search_empty_query() {
        let da = build_u8(&[b"abc"]);
        let results: Vec<PrefixMatch> = da.common_prefix_search(b"").collect();
        assert!(results.is_empty());
    }

    #[test]
    fn common_prefix_search_exact_only() {
        let da = build_u8(&[b"abc"]);
        let results: Vec<PrefixMatch> = da.common_prefix_search(b"abc").collect();
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0],
            PrefixMatch {
                len: 3,
                value_id: 0
            }
        );
    }

    #[test]
    fn common_prefix_search_char_keys() {
        let keys: Vec<Vec<char>> = vec![
            "あ".chars().collect(),
            "あい".chars().collect(),
            "あいう".chars().collect(),
        ];
        let da = DoubleArray::<char>::build(&keys);
        let query: Vec<char> = "あいうえお".chars().collect();
        let results: Vec<PrefixMatch> = da.common_prefix_search(&query).collect();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].len, 1); // "あ"
        assert_eq!(results[1].len, 2); // "あい"
        assert_eq!(results[2].len, 3); // "あいう"
    }

    #[test]
    fn common_prefix_search_empty_trie() {
        let da = build_u8(&[]);
        let results: Vec<PrefixMatch> = da.common_prefix_search(b"abc").collect();
        assert!(results.is_empty());
    }

    // === predictive_search tests ===

    #[test]
    fn predictive_search_basic() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc"];
        let da = build_u8(&keys);

        let results: Vec<SearchMatch<u8>> = da.predictive_search(b"a").collect();
        // Should find "a", "ab", "abc"
        let mut value_ids: Vec<u32> = results.iter().map(|r| r.value_id).collect();
        value_ids.sort();
        assert_eq!(value_ids, vec![0, 1, 2]); // "a"=0, "ab"=1, "abc"=2
    }

    #[test]
    fn predictive_search_empty_prefix() {
        let keys: Vec<&[u8]> = vec![b"a", b"b", b"c"];
        let da = build_u8(&keys);

        let results: Vec<SearchMatch<u8>> = da.predictive_search(b"").collect();
        // Empty prefix = all keys
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn predictive_search_no_match() {
        let da = build_u8(&[b"abc", b"abd"]);
        let results: Vec<SearchMatch<u8>> = da.predictive_search(b"xyz").collect();
        assert!(results.is_empty());
    }

    #[test]
    fn predictive_search_exact_only() {
        let da = build_u8(&[b"abc"]);
        let results: Vec<SearchMatch<u8>> = da.predictive_search(b"abc").collect();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, b"abc");
        assert_eq!(results[0].value_id, 0);
    }

    #[test]
    fn predictive_search_key_reconstruction() {
        let keys: Vec<&[u8]> = vec![b"ab", b"abc", b"abd"];
        let da = build_u8(&keys);

        let mut results: Vec<SearchMatch<u8>> = da.predictive_search(b"ab").collect();
        results.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].key, b"ab");
        assert_eq!(results[1].key, b"abc");
        assert_eq!(results[2].key, b"abd");
    }

    #[test]
    // Builds a 35k-key trie, which takes >15 min under Miri. The smaller
    // `predictive_search_root_many_children` test covers the same bug, so
    // there's no value in paying for this one under Miri.
    #[cfg_attr(miri, ignore)]
    fn predictive_search_emits_every_key_at_scale() {
        // Regression: on large tries, predictive_search(b"") silently dropped
        // ~30% of keys even though exact_match still worked. Reproduces with
        // a systematic key set that stresses the sibling-chain walk.
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for a in b'a'..=b'z' {
            for b in b'a'..=b'z' {
                for c in b'a'..=b'z' {
                    keys.push(vec![a, b, c]);
                    keys.push(vec![a, b, c, b'x']);
                }
            }
        }
        keys.sort();
        keys.dedup();
        let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let da = DoubleArray::<u8>::build(&key_refs);

        // Every key must be reachable via exact_match (sanity check).
        for k in &keys {
            assert!(da.exact_match(k).is_some(), "exact_match lost {k:?}");
        }

        let mut emitted: Vec<Vec<u8>> = da.predictive_search(b"").map(|m| m.key).collect();
        emitted.sort();
        assert_eq!(emitted, keys, "iter must emit every key");
    }

    #[test]
    fn predictive_search_small_terminal_and_children() {
        // Minimal case: each node has BOTH a terminal (value at this node) AND
        // extension children. E.g. "ab", "ab" + "abc".
        let keys: Vec<&[u8]> = vec![b"ab", b"abc"];
        let da = DoubleArray::<u8>::build(&keys);
        let mut v: Vec<Vec<u8>> = da.predictive_search(b"").map(|m| m.key).collect();
        v.sort();
        assert_eq!(v, vec![b"ab".to_vec(), b"abc".to_vec()]);
    }

    #[test]
    fn predictive_search_root_many_children() {
        // Regression: with 26 root children where the highest-frequency label
        // is not also the byte-smallest, the sibling chain previously skipped
        // ~90% of subtrees because children were placed in byte order while
        // first_child iterates by code order.
        let mut keys: Vec<Vec<u8>> = Vec::new();
        for a in b'a'..=b'z' {
            keys.push(vec![a]);
            keys.push(vec![a, b'x']);
        }
        keys.sort();
        let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let da = DoubleArray::<u8>::build(&refs);
        let count = da.predictive_search(b"").count();
        assert_eq!(count, keys.len());
    }

    #[test]
    fn predictive_search_terminal_plus_many_children() {
        // Node "a" has a value AND 10 extension children.
        let mut keys: Vec<Vec<u8>> = vec![b"a".to_vec()];
        for c in b'0'..=b'9' {
            keys.push(vec![b'a', c]);
        }
        keys.sort();
        let refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
        let da = DoubleArray::<u8>::build(&refs);
        let count = da.predictive_search(b"").count();
        assert_eq!(count, keys.len());
    }

    #[test]
    fn predictive_search_char_keys() {
        let da = build_char(&["あ", "あい", "あいう", "か"]);
        let prefix: Vec<char> = "あ".chars().collect();
        let results: Vec<SearchMatch<char>> = da.predictive_search(&prefix).collect();
        // Should find "あ", "あい", "あいう"
        assert_eq!(results.len(), 3);
        let mut keys: Vec<String> = results.iter().map(|r| r.key.iter().collect()).collect();
        keys.sort();
        assert_eq!(keys, vec!["あ", "あい", "あいう"]);
    }

    // === probe tests ===

    #[test]
    fn probe_none() {
        let da = build_u8(&[b"abc"]);
        let result = da.probe(b"xyz");
        assert_eq!(
            result,
            ProbeResult {
                value: None,
                has_children: false,
            }
        );
    }

    #[test]
    fn probe_prefix() {
        let da = build_u8(&[b"abc"]);
        let result = da.probe(b"ab");
        assert_eq!(
            result,
            ProbeResult {
                value: None,
                has_children: true,
            }
        );
    }

    #[test]
    fn probe_exact() {
        let da = build_u8(&[b"abc"]);
        let result = da.probe(b"abc");
        assert_eq!(
            result,
            ProbeResult {
                value: Some(0),
                has_children: false,
            }
        );
    }

    #[test]
    fn probe_exact_and_prefix() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let result = da.probe(b"a");
        assert_eq!(
            result,
            ProbeResult {
                value: Some(0),
                has_children: true,
            }
        );
    }

    #[test]
    fn probe_romaji_scenario() {
        // Simulates romaji trie: "n"→ん, "na"→な, "ni"→に, "nu"→ぬ, "shi"→し
        let keys: Vec<&[u8]> = vec![b"n", b"na", b"ni", b"nu", b"shi"];
        let da = build_u8(&keys);

        // "n" is both exact and prefix (of "na", "ni", "nu")
        let r = da.probe(b"n");
        assert_eq!(r.value, Some(0));
        assert!(r.has_children);

        // "s" is prefix only (of "shi")
        let r = da.probe(b"s");
        assert_eq!(r.value, None);
        assert!(r.has_children);

        // "sh" is prefix only
        let r = da.probe(b"sh");
        assert_eq!(r.value, None);
        assert!(r.has_children);

        // "shi" is exact, no further children
        let r = da.probe(b"shi");
        assert_eq!(r.value, Some(4));
        assert!(!r.has_children);

        // "na" is exact, no further children
        let r = da.probe(b"na");
        assert_eq!(r.value, Some(1));
        assert!(!r.has_children);

        // "x" doesn't exist
        let r = da.probe(b"x");
        assert_eq!(r.value, None);
        assert!(!r.has_children);
    }

    #[test]
    fn probe_empty_trie() {
        let da = build_u8(&[]);
        let result = da.probe(b"abc");
        assert_eq!(
            result,
            ProbeResult {
                value: None,
                has_children: false,
            }
        );
    }

    #[test]
    fn probe_empty_key_on_empty_trie() {
        let da = build_u8(&[]);
        let result = da.probe(b"");
        assert_eq!(
            result,
            ProbeResult {
                value: None,
                has_children: false,
            }
        );
    }

    #[test]
    fn validate_strict_on_built_trie_passes() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b"];
        let da = build_u8(&keys);
        assert!(da.validate_strict().is_ok());
    }
}
