use std::marker::PhantomData;

use crate::{CodeMapper, Label, Node, PrefixMatch, ProbeResult, SearchMatch};

/// A borrowed view into a double-array trie, holding references to nodes,
/// the CSR children representation, and the code mapper. All search methods
/// are implemented here and shared between `DoubleArray` and `DoubleArrayRef`.
#[derive(Clone, Copy)]
pub(crate) struct TrieView<'a, L: Label> {
    pub(crate) nodes: &'a [Node],
    pub(crate) child_offsets: &'a [u32],
    pub(crate) children_list: &'a [u32],
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
        // Root is at nodes[1]; nodes[0] is the invalid sentinel.
        // Deserialization guarantees nodes.len() >= 2.
        let mut node_idx: u32 = 1;
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
            node_idx: 1, // root is at nodes[1]
            done: false,
        }
    }

    /// Predictive search. Returns an iterator over all keys that start with `prefix`.
    pub(crate) fn predictive_search(self, prefix: &[L]) -> PredictiveIter<'a, L> {
        let start_node = self.traverse(prefix);
        let mut stack = Vec::new();
        let key_buf = if start_node.is_some() {
            prefix.to_vec()
        } else {
            Vec::new()
        };
        if let Some(node) = start_node {
            // None label = root entry; key_buf is already set to the prefix.
            stack.push((node, prefix.len() as u32, None));
        }
        PredictiveIter {
            view: self,
            stack,
            key_buf,
        }
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

        let node = self.nodes[node_idx as usize];

        // Fast path: `has_leaf` means the terminal child is at
        // `base(p) XOR 0 == base(p)`. We don't need to touch `children_list`
        // to find it — identical to the v2 code path. `child_offsets` is
        // still needed to decide `has_children` (child count beyond terminal).
        if node.has_leaf() {
            let terminal_idx = node.base() as usize;
            if terminal_idx < self.nodes.len() {
                let terminal = self.nodes[terminal_idx];
                if terminal.check() == node_idx && terminal.is_leaf() {
                    let n = child_range_width(self.child_offsets, node_idx);
                    return ProbeResult {
                        value: Some(terminal.value_id()),
                        has_children: n > 1,
                    };
                }
            }
        }

        // No terminal child — `has_children` is whether the children slice
        // is non-empty.
        let n = child_range_width(self.child_offsets, node_idx);
        ProbeResult {
            value: None,
            has_children: n > 0,
        }
    }
}

/// Width of `child_offsets[p+1] - child_offsets[p]` as `usize`. Uses
/// saturating arithmetic so that non-monotonic offsets (only possible
/// from a corrupted buffer) yield 0 instead of wrapping silently to a
/// huge value and misreporting `has_children`.
#[inline]
fn child_range_width(child_offsets: &[u32], p: u32) -> usize {
    let p = p as usize;
    child_offsets[p + 1].saturating_sub(child_offsets[p]) as usize
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
}

impl<L: Label> Iterator for PredictiveIter<'_, L> {
    type Item = SearchMatch<L>;

    fn next(&mut self) -> Option<SearchMatch<L>> {
        while let Some((node_idx, parent_depth, label)) = self.stack.pop() {
            // Restore key_buf to the parent's depth, then append this node's label.
            self.key_buf.truncate(parent_depth as usize);
            if let Some(l) = label {
                self.key_buf.push(l);
            }
            let depth = self.key_buf.len() as u32;

            let node = &self.view.nodes[node_idx as usize];
            let base = node.base();

            // Children are stored in code-ascending order (terminal first if
            // present). Iterate in reverse so that extensions end up on the
            // stack in descending order and pop in ascending order, matching
            // the documented traversal order.
            let p = node_idx as usize;
            let start = self.view.child_offsets[p] as usize;
            let end = self.view.child_offsets[p + 1] as usize;
            let children = &self.view.children_list[start..end];

            let mut result: Option<SearchMatch<L>> = None;
            for &c in children.iter().rev() {
                let child = &self.view.nodes[c as usize];
                if child.is_leaf() {
                    // Terminal child — yield the current prefix as a match.
                    result = Some(SearchMatch {
                        key: self.key_buf.clone(),
                        value_id: child.value_id(),
                    });
                } else {
                    // Extension child — push onto the DFS stack. `reverse`
                    // silently skips out-of-range / unconvertible codes,
                    // which can only surface from a corrupted buffer.
                    let child_code = base ^ c;
                    if let Some(l) = self.view.code_map.reverse::<L>(child_code) {
                        self.stack.push((c, depth, Some(l)));
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
