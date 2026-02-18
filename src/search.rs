use std::marker::PhantomData;

use crate::view::TrieView;
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
    /// Returns a `TrieView` borrowing this trie's data.
    #[inline]
    fn view(&self) -> TrieView<'_, L> {
        TrieView {
            nodes: &self.nodes,
            siblings: &self.siblings,
            code_map: &self.code_map,
            _phantom: PhantomData,
        }
    }

    /// Exact match search. Returns the value_id if the key exists.
    #[inline]
    pub fn exact_match(&self, key: &[L]) -> Option<u32> {
        self.view().exact_match(key)
    }

    /// Common prefix search. Returns an iterator over all prefixes of `query`
    /// that exist as keys in the trie.
    pub fn common_prefix_search<'a>(
        &'a self,
        query: &'a [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'a {
        self.view().common_prefix_search(query)
    }

    /// Predictive search. Returns an iterator over all keys that start with `prefix`.
    ///
    /// Uses sibling chain DFS to enumerate all keys sharing the given prefix.
    /// Keys are reconstructed using `CodeMapper::reverse`.
    pub fn predictive_search<'a>(
        &'a self,
        prefix: &'a [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'a {
        self.view().predictive_search(prefix)
    }

    /// Probe a key. Returns whether the key exists and whether it has children.
    ///
    /// The 4 possible states:
    /// - `None`: key not in trie, not a prefix of any key
    /// - `Prefix`: key is a prefix of other keys but not a key itself
    /// - `Exact`: key exists but is not a prefix of other keys
    /// - `ExactAndPrefix`: key exists and is also a prefix of other keys
    #[inline]
    pub fn probe(&self, key: &[L]) -> ProbeResult {
        self.view().probe(key)
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
}
