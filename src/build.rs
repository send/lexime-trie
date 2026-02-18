use crate::{CodeMapper, DoubleArray, Label, Node};

/// Mutable state used during trie construction.
struct BuildContext {
    nodes: Vec<Node>,
    siblings: Vec<u32>,
    free_list: FreeList,
}

/// Doubly-linked circular free list for managing unused node slots.
struct FreeList {
    /// prev[i] and next[i] form a circular doubly-linked list of free indices.
    prev: Vec<u32>,
    next: Vec<u32>,
}

impl FreeList {
    /// Creates a free list with the given capacity.
    /// All slots except index 0 (root) are free.
    fn new(capacity: usize) -> Self {
        let cap = capacity as u32;
        let mut prev = vec![0u32; capacity];
        let mut next = vec![0u32; capacity];

        // Circular list: 0 is the sentinel (never free).
        // Free nodes: 1, 2, ..., cap-1 form a circular chain through 0.
        // 0.next = 1, 1.next = 2, ..., (cap-1).next = 0
        // 0.prev = cap-1, (cap-1).prev = cap-2, ..., 1.prev = 0
        for i in 0..cap {
            prev[i as usize] = if i == 0 { cap - 1 } else { i - 1 };
            next[i as usize] = if i == cap - 1 { 0 } else { i + 1 };
        }

        Self { prev, next }
    }

    /// Removes index `i` from the free list.
    fn remove(&mut self, i: u32) {
        let p = self.prev[i as usize];
        let n = self.next[i as usize];
        self.next[p as usize] = n;
        self.prev[n as usize] = p;
        // Mark as removed (self-loop)
        self.prev[i as usize] = i;
        self.next[i as usize] = i;
    }

    /// Returns the first free index, or None if the list is empty.
    fn first_free(&self) -> Option<u32> {
        let f = self.next[0];
        if f == 0 {
            None
        } else {
            Some(f)
        }
    }

    /// Checks if index `i` is free (not removed from the list).
    fn is_free(&self, i: u32) -> bool {
        // Index 0 is the sentinel and never free.
        if i == 0 {
            return false;
        }
        // If removed, it has a self-loop
        !(self.prev[i as usize] == i && self.next[i as usize] == i)
    }

    /// Ensures the free list covers at least `new_cap` indices.
    /// Returns the index of the first newly added free slot.
    fn grow(&mut self, new_cap: usize) -> u32 {
        let old_cap = self.prev.len();
        if new_cap <= old_cap {
            return old_cap as u32; // shouldn't happen, but safe
        }

        // The old tail of the free list is prev[0].
        let old_tail = self.prev[0];

        self.prev.resize(new_cap, 0);
        self.next.resize(new_cap, 0);

        // Link old_tail -> old_cap -> old_cap+1 -> ... -> new_cap-1 -> 0
        for i in old_cap..new_cap {
            let i32 = i as u32;
            self.prev[i] = if i == old_cap { old_tail } else { i32 - 1 };
            self.next[i] = if i == new_cap - 1 { 0 } else { i32 + 1 };
        }
        self.next[old_tail as usize] = old_cap as u32;
        self.prev[0] = (new_cap - 1) as u32;

        old_cap as u32
    }
}

impl BuildContext {
    fn new(capacity: usize) -> Self {
        let mut free_list = FreeList::new(capacity);
        free_list.remove(0); // root is at index 0
        Self {
            nodes: vec![Node::default(); capacity],
            siblings: vec![0u32; capacity],
            free_list,
        }
    }

    /// Ensures all arrays cover at least `new_cap` indices.
    fn ensure_capacity(&mut self, new_cap: usize) {
        if new_cap > self.nodes.len() {
            self.nodes.resize(new_cap, Node::default());
            self.siblings.resize(new_cap, 0);
            self.free_list.grow(new_cap);
        }
    }

    /// Recursively places children for keys[begin..end] at the given depth.
    fn build_rec(
        &mut self,
        coded_keys: &[Vec<u32>],
        begin: usize,
        end: usize,
        depth: usize,
        parent: u32,
    ) {
        // Collect distinct child labels and their key ranges
        let mut children: Vec<(u32, usize, usize)> = Vec::new(); // (code, begin, end)
        let mut i = begin;
        while i < end {
            let code = coded_keys[i][depth];
            let child_begin = i;
            i += 1;
            while i < end && coded_keys[i][depth] == code {
                i += 1;
            }
            children.push((code, child_begin, i));
        }

        // Find a base such that base XOR code is free for all children
        let base = self.find_base(&children);
        self.nodes[parent as usize].set_base(base);

        // Place child nodes
        let mut child_indices: Vec<u32> = Vec::with_capacity(children.len());
        for &(code, _, _) in &children {
            let child_idx = base ^ code;
            child_indices.push(child_idx);
            self.free_list.remove(child_idx);
            self.nodes[child_idx as usize].set_check(parent);
        }

        // Build sibling chain
        for w in child_indices.windows(2) {
            self.siblings[w[0] as usize] = w[1];
        }
        // Last child's sibling is 0 (no more siblings)

        // Set leaf/has_leaf flags and recurse into non-terminal children
        for (ci, &(code, child_begin, child_end)) in children.iter().enumerate() {
            let child_idx = child_indices[ci];
            if code == 0 {
                // Terminal symbol — this is a leaf node
                debug_assert_eq!(child_end - child_begin, 1);
                let value_id = child_begin as u32;
                self.nodes[child_idx as usize].set_leaf(value_id);
                self.nodes[parent as usize].set_has_leaf();
            } else {
                // Non-terminal — recurse
                self.build_rec(coded_keys, child_begin, child_end, depth + 1, child_idx);
            }
        }
    }

    /// Finds a base value such that `base XOR code` is a free slot for each child label.
    fn find_base(&mut self, children: &[(u32, usize, usize)]) -> u32 {
        let first_code = children[0].0;

        // Start from the first free slot. We try: base = cursor XOR first_code,
        // then check if base XOR code is free for all children.
        let mut cursor = match self.free_list.first_free() {
            Some(f) => f,
            None => {
                let new_cap = self.nodes.len() * 2;
                self.ensure_capacity(new_cap);
                (self.nodes.len() / 2) as u32 // first slot of newly grown region
            }
        };

        loop {
            let base = cursor ^ first_code;

            // base must not be 0 (reserved for root check semantics)
            if base != 0 {
                // Compute max child index to ensure capacity
                let max_idx = children
                    .iter()
                    .map(|&(code, _, _)| base ^ code)
                    .max()
                    .unwrap();

                // Ensure capacity
                if max_idx as usize >= self.nodes.len() {
                    let new_cap = (max_idx as usize + 1).next_power_of_two();
                    self.ensure_capacity(new_cap);
                }

                let all_free = children
                    .iter()
                    .all(|&(code, _, _)| self.free_list.is_free(base ^ code));

                if all_free {
                    return base;
                }
            }

            // Advance cursor to the next free slot
            let next = self.free_list.next[cursor as usize];
            if next == 0 {
                // Wrapped around to sentinel — all current free slots exhausted, grow
                let new_cap = self.nodes.len() * 2;
                let new_first = self.free_list.grow(new_cap);
                self.nodes.resize(new_cap, Node::default());
                self.siblings.resize(new_cap, 0);
                cursor = new_first;
            } else {
                cursor = next;
            }
        }
    }
}

impl<L: Label> DoubleArray<L> {
    /// Builds a double-array trie from sorted keys.
    ///
    /// Each key `keys[i]` is assigned `value_id = i`.
    ///
    /// # Panics
    /// - If keys are not sorted in ascending order.
    /// - If duplicate keys are found.
    pub fn build(keys: &[impl AsRef<[L]>]) -> Self {
        // Verify sorted and no duplicates
        for w in keys.windows(2) {
            assert!(
                w[0].as_ref() < w[1].as_ref(),
                "keys must be sorted in ascending order with no duplicates"
            );
        }

        if keys.is_empty() {
            let empty: &[Vec<L>] = &[];
            return Self::new(vec![Node::default()], vec![0], CodeMapper::build(empty));
        }

        let code_map = CodeMapper::build(keys);

        // Convert keys to code sequences with terminal symbol (0) appended
        let coded_keys: Vec<Vec<u32>> = keys
            .iter()
            .map(|k| {
                let mut codes: Vec<u32> = k.as_ref().iter().map(|&l| code_map.get(l)).collect();
                codes.push(0); // terminal symbol
                codes
            })
            .collect();

        let initial_cap = 256.max(coded_keys.len() * 4);
        let mut ctx = BuildContext::new(initial_cap);

        ctx.build_rec(&coded_keys, 0, keys.len(), 0, 0);

        // Trim trailing unused nodes
        let last_used = ctx
            .nodes
            .iter()
            .enumerate()
            .rev()
            .find(|(_, n)| *n != &Node::default())
            .map(|(i, _)| i)
            .unwrap_or(0);
        let final_len = last_used + 1;
        ctx.nodes.truncate(final_len);
        ctx.siblings.truncate(final_len);

        Self::new(ctx.nodes, ctx.siblings, code_map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_empty() {
        let keys: Vec<&[u8]> = vec![];
        let da = DoubleArray::<u8>::build(&keys);
        assert!(da.num_nodes() > 0); // at least root
    }

    #[test]
    fn build_single_key() {
        let da = DoubleArray::<u8>::build(&[b"abc"]);
        assert!(da.num_nodes() > 1);
    }

    #[test]
    fn build_shared_prefix() {
        let da = DoubleArray::<u8>::build(&[b"abc", b"abd", b"xyz"]);
        assert!(da.num_nodes() > 1);
    }

    #[test]
    fn build_char_keys() {
        let keys: Vec<Vec<char>> = vec![
            "あい".chars().collect(),
            "あう".chars().collect(),
            "かき".chars().collect(),
        ];
        let da = DoubleArray::<char>::build(&keys);
        assert!(da.num_nodes() > 1);
    }

    #[test]
    #[should_panic(expected = "sorted")]
    fn build_unsorted_panics() {
        DoubleArray::<u8>::build(&[b"bbb", b"aaa"]);
    }

    #[test]
    #[should_panic(expected = "sorted")]
    fn build_duplicates_panics() {
        DoubleArray::<u8>::build(&[b"aaa", b"aaa"]);
    }

    #[test]
    fn check_points_to_parent() {
        let da = DoubleArray::<u8>::build(&[b"ab", b"ac"]);
        for (i, node) in da.nodes.iter().enumerate() {
            if i == 0 || *node == Node::default() {
                continue;
            }
            let parent_idx = node.check() as usize;
            assert!(parent_idx < da.nodes.len());
        }
    }

    #[test]
    fn leaf_and_has_leaf_consistency() {
        let keys: Vec<&[u8]> = vec![b"ab", b"ac", b"b"];
        let da = DoubleArray::<u8>::build(&keys);

        let mut leaf_count = 0;

        for node in &da.nodes {
            if node.is_leaf() {
                leaf_count += 1;
                // A leaf's parent should have has_leaf set
                let parent = &da.nodes[node.check() as usize];
                assert!(parent.has_leaf());
            }
        }

        // We have 3 keys, so 3 terminal nodes (leaves)
        assert_eq!(leaf_count, 3);
    }

    #[test]
    fn sibling_chain_no_cycle() {
        let keys: Vec<&[u8]> = vec![b"a", b"b", b"c"];
        let da = DoubleArray::<u8>::build(&keys);

        for i in 0..da.siblings.len() {
            let mut visited = std::collections::HashSet::new();
            let mut cur = i as u32;
            while cur != 0 {
                assert!(visited.insert(cur), "cycle detected in sibling chain");
                cur = da.siblings[cur as usize];
            }
        }
    }

    #[test]
    fn sibling_chain_links_same_parent() {
        let da = DoubleArray::<u8>::build(&[b"ab", b"ac", b"ad"]);

        // Find node for 'a' from root
        let root_base = da.nodes[0].base();
        let code_a = da.code_map.get(b'a');
        let node_a_idx = root_base ^ code_a;
        assert_eq!(da.nodes[node_a_idx as usize].check(), 0);

        // Count children of node_a via sibling chain
        let a_base = da.nodes[node_a_idx as usize].base();

        // Find any child of node_a to start the chain
        let mut first_child = None;
        for code in 0..da.code_map.alphabet_size() {
            let idx = a_base ^ code;
            if (idx as usize) < da.nodes.len() && da.nodes[idx as usize].check() == node_a_idx {
                first_child = Some(idx);
                break;
            }
        }

        let first = first_child.expect("node_a should have children");
        let mut count = 1;
        let mut cur = da.siblings[first as usize];
        while cur != 0 {
            assert_eq!(
                da.nodes[cur as usize].check(),
                node_a_idx,
                "sibling should have same parent"
            );
            count += 1;
            cur = da.siblings[cur as usize];
        }

        // "ab", "ac", "ad" share prefix "a", so node_a has children:
        // terminal (since none of these keys IS "a"), 'b', 'c', 'd'
        // Actually "a" is not a key, so no terminal child for node_a.
        // Children are: code('b'), code('c'), code('d') = 3 children
        assert_eq!(count, 3);
    }
}
