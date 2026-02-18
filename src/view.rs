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
        let nodes = self.nodes;
        let len = nodes.len();
        let mut node_idx: u32 = 0; // start at root (always valid: deserialization rejects empty nodes)
        for &label in key {
            let code = self.code_map.get(label);
            if code == 0 {
                return None;
            }
            // SAFETY: node_idx is a verified index — it was either 0 (root, guaranteed
            // to exist) or set to next_idx after bounds + check validation below.
            let next_idx = unsafe { nodes.get_unchecked(node_idx as usize) }.base() ^ code;
            if next_idx as usize >= len {
                return None;
            }
            // SAFETY: next_idx is within bounds (checked above).
            if unsafe { nodes.get_unchecked(next_idx as usize) }.check() != node_idx {
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
        // SAFETY: traverse guarantees node_idx is a valid index.
        let node = unsafe { *self.nodes.get_unchecked(node_idx as usize) };

        if !node.has_leaf() {
            return None;
        }

        let terminal_idx = node.base();
        if terminal_idx as usize >= self.nodes.len() {
            return None;
        }
        // SAFETY: terminal_idx is within bounds (checked above).
        let terminal = unsafe { *self.nodes.get_unchecked(terminal_idx as usize) };
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
        let key_buf = prefix.to_vec();
        if let Some(node) = start_node {
            // None label = root entry; key_buf is already set to the prefix.
            stack.push((node, prefix.len() as u32, None));
        }
        PredictiveIter {
            view: self,
            stack,
            key_buf,
            children_buf: Vec::new(),
        }
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
        let nodes = self.view.nodes;
        // SAFETY: node_idx is always a valid index (starts at root 0, advanced only
        // after bounds + check validation in try_advance).
        let node = unsafe { nodes.get_unchecked(self.node_idx as usize) };
        if !node.has_leaf() {
            return None;
        }
        let terminal_idx = node.base();
        if terminal_idx as usize >= nodes.len() {
            return None;
        }
        // SAFETY: terminal_idx bounds-checked above.
        let terminal = unsafe { nodes.get_unchecked(terminal_idx as usize) };
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
        let nodes = self.view.nodes;
        // SAFETY: node_idx is a valid index (see check_terminal SAFETY comment).
        let base = unsafe { nodes.get_unchecked(self.node_idx as usize) }.base();
        let next_idx = base ^ code;
        if next_idx as usize >= nodes.len() {
            return false;
        }
        // SAFETY: next_idx bounds-checked above.
        if unsafe { nodes.get_unchecked(next_idx as usize) }.check() != self.node_idx {
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
    /// DFS stack: (node_idx, parent_depth, label_to_append).
    /// `None` label = root entry (prefix node); the key_buf already contains
    /// the prefix so no label needs to be appended.
    stack: Vec<(u32, u32, Option<L>)>,
    /// Shared key buffer. Grows/truncates as DFS proceeds, avoiding per-node
    /// Vec<L> clones. Only cloned when emitting a SearchMatch.
    key_buf: Vec<L>,
    /// Reusable buffer for collecting children within a single `next()` call.
    children_buf: Vec<(u32, bool)>,
}

impl<L: Label> Iterator for PredictiveIter<'_, L> {
    type Item = SearchMatch<L>;

    fn next(&mut self) -> Option<SearchMatch<L>> {
        let node_count = self.view.nodes.len();
        while let Some((node_idx, parent_depth, label)) = self.stack.pop() {
            // Restore key_buf to the parent's depth, then append this node's label.
            self.key_buf.truncate(parent_depth as usize);
            if let Some(l) = label {
                self.key_buf.push(l);
            }
            let depth = self.key_buf.len() as u32;

            let node = &self.view.nodes[node_idx as usize];
            let base = node.base();

            self.children_buf.clear();

            let terminal_idx = base;
            if (terminal_idx as usize) < node_count
                && self.view.nodes[terminal_idx as usize].check() == node_idx
            {
                self.children_buf.push((terminal_idx, true));

                let mut sib = self.view.siblings[terminal_idx as usize];
                // Guard against cycles in malformed data
                let mut steps = 0u32;
                while sib != 0 && (steps as usize) < node_count {
                    self.children_buf.push((sib, false));
                    sib = self.view.siblings[sib as usize];
                    steps += 1;
                }
            } else if let Some(first) = self.view.first_child(node_idx) {
                self.children_buf.push((first, false));
                let mut sib = self.view.siblings[first as usize];
                let mut steps = 0u32;
                while sib != 0 && (steps as usize) < node_count {
                    self.children_buf.push((sib, false));
                    sib = self.view.siblings[sib as usize];
                    steps += 1;
                }
            }

            let mut result: Option<SearchMatch<L>> = None;

            for i in (0..self.children_buf.len()).rev() {
                let (child_idx, is_terminal) = self.children_buf[i];
                if is_terminal {
                    let child = &self.view.nodes[child_idx as usize];
                    if child.is_leaf() {
                        result = Some(SearchMatch {
                            key: self.key_buf.clone(),
                            value_id: child.value_id(),
                        });
                    }
                } else {
                    let child_code = base ^ child_idx;
                    let label_u32 = self.view.code_map.reverse(child_code);
                    if let Ok(l) = L::try_from(label_u32) {
                        self.stack.push((child_idx, depth, Some(l)));
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
