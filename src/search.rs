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
    /// Traverses the trie from the root following the given key labels.
    /// Returns the node index after consuming all labels, or None if traversal fails.
    fn traverse(&self, key: &[L]) -> Option<u32> {
        let mut node_idx: u32 = 0; // start at root
        for &label in key {
            let code = self.code_map.get(label);
            if code == 0 {
                // Unmapped label — cannot exist in trie
                return None;
            }
            let next_idx = self.nodes[node_idx as usize].base() ^ code;
            if next_idx as usize >= self.nodes.len()
                || self.nodes[next_idx as usize].check() != node_idx
            {
                return None;
            }
            node_idx = next_idx;
        }
        Some(node_idx)
    }

    /// Exact match search. Returns the value_id if the key exists.
    pub fn exact_match(&self, key: &[L]) -> Option<u32> {
        let node_idx = self.traverse(key)?;

        // Fast path: check has_leaf flag before probing terminal child
        if !self.nodes[node_idx as usize].has_leaf() {
            return None;
        }

        // Check terminal child: base XOR terminal_code where terminal_code = 0
        let terminal_idx = self.nodes[node_idx as usize].base();
        if terminal_idx as usize >= self.nodes.len() {
            return None;
        }
        let terminal = &self.nodes[terminal_idx as usize];
        if terminal.check() == node_idx && terminal.is_leaf() {
            Some(terminal.value_id())
        } else {
            None
        }
    }

    /// Common prefix search. Returns an iterator over all prefixes of `query`
    /// that exist as keys in the trie.
    pub fn common_prefix_search<'a>(
        &'a self,
        query: &'a [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'a {
        CommonPrefixIter {
            da: self,
            query,
            pos: 0,
            node_idx: 0,
            done: false,
        }
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

struct CommonPrefixIter<'a, L: Label> {
    da: &'a DoubleArray<L>,
    query: &'a [L],
    pos: usize,
    node_idx: u32,
    done: bool,
}

impl<'a, L: Label> Iterator for CommonPrefixIter<'a, L> {
    type Item = PrefixMatch;

    fn next(&mut self) -> Option<PrefixMatch> {
        if self.done {
            return None;
        }

        loop {
            // At each position, check if the current node has a terminal child
            if self.da.nodes[self.node_idx as usize].has_leaf() {
                let base = self.da.nodes[self.node_idx as usize].base();
                // terminal_code = 0, so base XOR 0 = base
                let terminal_idx = base;
                if (terminal_idx as usize) < self.da.nodes.len() {
                    let terminal = &self.da.nodes[terminal_idx as usize];
                    if terminal.check() == self.node_idx && terminal.is_leaf() {
                        let result = PrefixMatch {
                            len: self.pos,
                            value_id: terminal.value_id(),
                        };

                        // Try to advance to next position
                        if self.pos < self.query.len() {
                            let label = self.query[self.pos];
                            let code = self.da.code_map.get(label);
                            if code != 0 {
                                let next_idx = base ^ code;
                                if (next_idx as usize) < self.da.nodes.len()
                                    && self.da.nodes[next_idx as usize].check() == self.node_idx
                                {
                                    self.node_idx = next_idx;
                                    self.pos += 1;
                                    return Some(result);
                                }
                            }
                            // Cannot advance further
                            self.done = true;
                        } else {
                            self.done = true;
                        }
                        return Some(result);
                    }
                }
            }

            // No terminal at current position; try to advance
            if self.pos >= self.query.len() {
                self.done = true;
                return None;
            }

            let label = self.query[self.pos];
            let code = self.da.code_map.get(label);
            if code == 0 {
                self.done = true;
                return None;
            }

            let base = self.da.nodes[self.node_idx as usize].base();
            let next_idx = base ^ code;
            if next_idx as usize >= self.da.nodes.len()
                || self.da.nodes[next_idx as usize].check() != self.node_idx
            {
                self.done = true;
                return None;
            }

            self.node_idx = next_idx;
            self.pos += 1;
        }
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
}
