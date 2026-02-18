use std::marker::PhantomData;

use crate::{CodeMapper, Label, Node, PrefixMatch, ProbeResult, SearchMatch};

/// A borrowed view into a double-array trie, holding references to nodes,
/// siblings, and the code mapper. All search methods are implemented here
/// and shared between `DoubleArray` and `DoubleArrayRef`.
#[derive(Clone, Copy)]
pub(crate) struct TrieView<'a, L: Label> {
    pub(crate) nodes: &'a [Node],
    pub(crate) siblings: &'a [u32],
    pub(crate) code_map: &'a CodeMapper,
    pub(crate) _phantom: PhantomData<L>,
}

impl<'a, L: Label> TrieView<'a, L> {
    /// Traverses the trie from the root following the given key labels.
    /// Returns the node index after consuming all labels, or None if traversal fails.
    #[inline]
    pub(crate) fn traverse(&self, key: &[L]) -> Option<u32> {
        let mut node_idx: u32 = 0; // start at root
        for &label in key {
            let code = self.code_map.get(label);
            if code == 0 {
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
    #[inline]
    pub(crate) fn exact_match(&self, key: &[L]) -> Option<u32> {
        let node_idx = self.traverse(key)?;
        let node = self.nodes[node_idx as usize];

        if !node.has_leaf() {
            return None;
        }

        let terminal_idx = node.base();
        if terminal_idx as usize >= self.nodes.len() {
            return None;
        }
        let terminal = self.nodes[terminal_idx as usize];
        if terminal.check() == node_idx && terminal.is_leaf() {
            Some(terminal.value_id())
        } else {
            None
        }
    }

    /// Common prefix search. Returns an iterator over all prefixes of `query`
    /// that exist as keys in the trie.
    pub(crate) fn common_prefix_search(self, query: &'a [L]) -> CommonPrefixIter<'a, L> {
        CommonPrefixIter {
            view: self,
            query,
            pos: 0,
            node_idx: 0,
            done: false,
        }
    }

    /// Predictive search. Returns an iterator over all keys that start with `prefix`.
    pub(crate) fn predictive_search(self, prefix: &[L]) -> PredictiveIter<'a, L> {
        let start_node = self.traverse(prefix);
        let mut stack = Vec::new();
        if let Some(node) = start_node {
            stack.push((node, prefix.to_vec()));
        }
        PredictiveIter { view: self, stack }
    }

    /// Finds the first child of `node_idx`.
    #[inline]
    fn first_child(&self, node_idx: u32) -> Option<u32> {
        let base = self.nodes[node_idx as usize].base();
        let terminal_idx = base;
        if terminal_idx != node_idx
            && (terminal_idx as usize) < self.nodes.len()
            && self.nodes[terminal_idx as usize].check() == node_idx
        {
            return Some(terminal_idx);
        }
        for code in 1..self.code_map.alphabet_size() {
            let idx = base ^ code;
            if (idx as usize) < self.nodes.len() && self.nodes[idx as usize].check() == node_idx {
                return Some(idx);
            }
        }
        None
    }

    /// Probe a key. Returns whether the key exists and whether it has children.
    #[inline]
    pub(crate) fn probe(&self, key: &[L]) -> ProbeResult {
        let node_idx = match self.traverse(key) {
            Some(idx) => idx,
            None => {
                return ProbeResult {
                    value: None,
                    has_children: false,
                }
            }
        };

        let base = self.nodes[node_idx as usize].base();

        let terminal_idx = base;
        if (terminal_idx as usize) < self.nodes.len() {
            let terminal = self.nodes[terminal_idx as usize];
            if terminal.check() == node_idx && terminal.is_leaf() {
                let has_children = self.siblings[terminal_idx as usize] != 0;
                return ProbeResult {
                    value: Some(terminal.value_id()),
                    has_children,
                };
            }
        }

        let has_children = self.first_child(node_idx).is_some();
        ProbeResult {
            value: None,
            has_children,
        }
    }
}

pub(crate) struct CommonPrefixIter<'a, L: Label> {
    view: TrieView<'a, L>,
    query: &'a [L],
    pos: usize,
    node_idx: u32,
    done: bool,
}

impl<L: Label> CommonPrefixIter<'_, L> {
    #[inline]
    fn check_terminal(&self) -> Option<PrefixMatch> {
        let node = &self.view.nodes[self.node_idx as usize];
        if !node.has_leaf() {
            return None;
        }
        let terminal_idx = node.base();
        if terminal_idx as usize >= self.view.nodes.len() {
            return None;
        }
        let terminal = &self.view.nodes[terminal_idx as usize];
        if terminal.check() == self.node_idx && terminal.is_leaf() {
            Some(PrefixMatch {
                len: self.pos,
                value_id: terminal.value_id(),
            })
        } else {
            None
        }
    }

    #[inline]
    fn try_advance(&mut self) -> bool {
        if self.pos >= self.query.len() {
            return false;
        }
        let label = self.query[self.pos];
        let code = self.view.code_map.get(label);
        if code == 0 {
            return false;
        }
        let base = self.view.nodes[self.node_idx as usize].base();
        let next_idx = base ^ code;
        if next_idx as usize >= self.view.nodes.len()
            || self.view.nodes[next_idx as usize].check() != self.node_idx
        {
            return false;
        }
        self.node_idx = next_idx;
        self.pos += 1;
        true
    }
}

impl<L: Label> Iterator for CommonPrefixIter<'_, L> {
    type Item = PrefixMatch;

    fn next(&mut self) -> Option<PrefixMatch> {
        while !self.done {
            let result = self.check_terminal();
            if !self.try_advance() {
                self.done = true;
            }
            if result.is_some() {
                return result;
            }
        }
        None
    }
}

pub(crate) struct PredictiveIter<'a, L: Label> {
    view: TrieView<'a, L>,
    stack: Vec<(u32, Vec<L>)>,
}

impl<L: Label> Iterator for PredictiveIter<'_, L> {
    type Item = SearchMatch<L>;

    fn next(&mut self) -> Option<SearchMatch<L>> {
        while let Some((node_idx, key)) = self.stack.pop() {
            let node = &self.view.nodes[node_idx as usize];
            let base = node.base();

            let mut children: Vec<(u32, bool)> = Vec::new();

            let terminal_idx = base;
            if (terminal_idx as usize) < self.view.nodes.len()
                && self.view.nodes[terminal_idx as usize].check() == node_idx
            {
                children.push((terminal_idx, true));

                let mut sib = self.view.siblings[terminal_idx as usize];
                while sib != 0 {
                    children.push((sib, false));
                    sib = self.view.siblings[sib as usize];
                }
            } else if let Some(first) = self.view.first_child(node_idx) {
                children.push((first, false));
                let mut sib = self.view.siblings[first as usize];
                while sib != 0 {
                    children.push((sib, false));
                    sib = self.view.siblings[sib as usize];
                }
            }

            let mut result: Option<SearchMatch<L>> = None;

            for &(child_idx, is_terminal) in children.iter().rev() {
                if is_terminal {
                    let child = &self.view.nodes[child_idx as usize];
                    if child.is_leaf() {
                        result = Some(SearchMatch {
                            key: key.clone(),
                            value_id: child.value_id(),
                        });
                    }
                } else {
                    let child_code = base ^ child_idx;
                    let label_u32 = self.view.code_map.reverse(child_code);
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
