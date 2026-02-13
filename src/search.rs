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
    ///
    /// Uses sibling chain DFS to enumerate all keys sharing the given prefix.
    /// Keys are reconstructed using `CodeMapper::reverse`.
    pub fn predictive_search<'a>(
        &'a self,
        prefix: &'a [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'a {
        PredictiveIter::new(self, prefix)
    }

    /// Finds the first child of `node_idx` using the sibling chain.
    /// The first child is at `base(node) XOR 0` (terminal), then follows the sibling chain.
    /// Returns the index of the first valid child, or None.
    fn first_child(&self, node_idx: u32) -> Option<u32> {
        let base = self.nodes[node_idx as usize].base();
        // Terminal child is at base XOR 0 = base
        let terminal_idx = base;
        if (terminal_idx as usize) < self.nodes.len()
            && self.nodes[terminal_idx as usize].check() == node_idx
        {
            return Some(terminal_idx);
        }
        // If no terminal child, the first non-terminal child must be found.
        // But the sibling chain starts from the first child placed during build.
        // We need to scan codes to find any child.
        // Actually, for nodes with has_leaf, the terminal is the first child.
        // For nodes without has_leaf, we need another way to find the first child.
        // Since all internal nodes must have at least one child, and the build places
        // children starting from code 0, we can check if terminal exists (above),
        // and if not, we need to search. But the sibling chain only connects siblings
        // once we have a starting child. For nodes without terminal, we know they have
        // children (otherwise they wouldn't exist), so we scan from code 1.
        for code in 1..self.code_map.alphabet_size() {
            let idx = base ^ code;
            if (idx as usize) < self.nodes.len() && self.nodes[idx as usize].check() == node_idx {
                return Some(idx);
            }
        }
        None
    }

    /// Probe a key. Returns whether the key exists and whether it has children.
    ///
    /// The 4 possible states:
    /// - `None`: key not in trie, not a prefix of any key
    /// - `Prefix`: key is a prefix of other keys but not a key itself
    /// - `Exact`: key exists but is not a prefix of other keys
    /// - `ExactAndPrefix`: key exists and is also a prefix of other keys
    pub fn probe(&self, key: &[L]) -> ProbeResult {
        let node_idx = match self.traverse(key) {
            Some(idx) => idx,
            None => {
                return ProbeResult {
                    value: None,
                    has_children: false,
                }
            }
        };

        let node = &self.nodes[node_idx as usize];
        let base = node.base();

        // Check terminal child: base XOR 0 = base
        let terminal_idx = base;
        if (terminal_idx as usize) < self.nodes.len()
            && self.nodes[terminal_idx as usize].check() == node_idx
            && self.nodes[terminal_idx as usize].is_leaf()
        {
            // Terminal child exists — key is in the trie
            let value_id = self.nodes[terminal_idx as usize].value_id();
            // has_children = terminal has siblings (other children of this node)
            let has_children = self.siblings[terminal_idx as usize] != 0;
            ProbeResult {
                value: Some(value_id),
                has_children,
            }
        } else {
            // No terminal child — key is not in the trie, but node exists
            // so it must have non-terminal children (it's a prefix)
            ProbeResult {
                value: None,
                has_children: true,
            }
        }
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

struct PredictiveIter<'a, L: Label> {
    da: &'a DoubleArray<L>,
    // Stack of (node_index, key_so_far) for DFS
    stack: Vec<(u32, Vec<L>)>,
}

impl<'a, L: Label> PredictiveIter<'a, L> {
    fn new(da: &'a DoubleArray<L>, prefix: &[L]) -> Self {
        let mut iter = PredictiveIter {
            da,
            stack: Vec::new(),
        };

        // Traverse prefix to find starting node
        if let Some(start_node) = da.traverse(prefix) {
            let prefix_labels: Vec<L> = prefix.to_vec();
            iter.stack.push((start_node, prefix_labels));
        }

        iter
    }
}

impl<'a, L: Label> Iterator for PredictiveIter<'a, L> {
    type Item = SearchMatch<L>;

    fn next(&mut self) -> Option<SearchMatch<L>> {
        while let Some((node_idx, key)) = self.stack.pop() {
            let node = &self.da.nodes[node_idx as usize];
            let base = node.base();

            // Collect children via the first child + sibling chain
            // We process children in reverse order so the stack pops in forward order
            let mut children: Vec<(u32, bool)> = Vec::new(); // (child_idx, is_terminal)

            // Check terminal child first (code 0): base XOR 0 = base
            let terminal_idx = base;
            if (terminal_idx as usize) < self.da.nodes.len()
                && self.da.nodes[terminal_idx as usize].check() == node_idx
            {
                children.push((terminal_idx, true));

                // Follow sibling chain from terminal
                let mut sib = self.da.siblings[terminal_idx as usize];
                while sib != 0 {
                    children.push((sib, false));
                    sib = self.da.siblings[sib as usize];
                }
            } else {
                // No terminal child — find first non-terminal child
                if let Some(first) = self.da.first_child(node_idx) {
                    children.push((first, false));
                    let mut sib = self.da.siblings[first as usize];
                    while sib != 0 {
                        children.push((sib, false));
                        sib = self.da.siblings[sib as usize];
                    }
                }
            }

            // Push non-terminal children onto stack in reverse order (for DFS ordering)
            // and collect any terminal matches
            let mut result: Option<SearchMatch<L>> = None;

            for &(child_idx, is_terminal) in children.iter().rev() {
                if is_terminal {
                    let child = &self.da.nodes[child_idx as usize];
                    if child.is_leaf() {
                        result = Some(SearchMatch {
                            key: key.clone(),
                            value_id: child.value_id(),
                        });
                    }
                } else {
                    // Reconstruct the label for this child from its code
                    let child_code = base ^ child_idx;
                    let label_u32 = self.da.code_map.reverse(child_code);
                    if let Ok(label) = L::try_from(label_u32) {
                        let mut child_key = key.clone();
                        child_key.push(label);
                        self.stack.push((child_idx, child_key));
                    }
                }
            }

            if let Some(r) = result {
                return Some(r);
            }
        }
        None
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
}
