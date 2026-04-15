use crate::{CodeMapper, DoubleArray, Label, Node};

/// Mutable state used during trie construction.
struct BuildContext {
    nodes: Vec<Node>,
    /// Flat list of (parent, child) edges appended as children are placed.
    /// The global order across parents is unspecified (the build walks the
    /// trie with an iterative LIFO work stack), but the key invariant is
    /// per-parent: all edges for a given parent are pushed together in a
    /// single code-ascending sweep. Converted to `child_offsets` +
    /// `children_list` at finalisation via an O(N + E) count-and-scatter
    /// pass — no sort needed, because scatter preserves insertion order
    /// within each parent bucket, which is exactly the code-ascending
    /// ordering the final slice requires.
    edges: Vec<(u32, u32)>,
    free_list: FreeList,
    /// Highest node index ever written to. Maintained incrementally as
    /// children are placed so the final trailing-trim step is O(1) instead
    /// of a reverse linear scan over `nodes`. Starts at 1 because the root
    /// always lives at `nodes[1]` (index 0 is the sentinel).
    max_used_idx: u32,
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
        // nodes[0] is the invalid sentinel and doubles as the free-list head.
        // It must stay in the cyclic list — removing it would cut the chain
        // and force `find_base` to grow capacity on the very first call,
        // orphaning every already-allocated free slot. `is_free(0)` is
        // hardcoded to return false, so `find_base` still rejects any base
        // that would place a child at index 0.
        //
        // nodes[1] is the root and never acts as a child slot, so it is
        // removed up front.
        free_list.remove(1);
        Self {
            nodes: vec![Node::default(); capacity],
            edges: Vec::new(),
            free_list,
            // Root lives at index 1, so it is always considered "used"
            // even before any descendants are placed. Every non-empty
            // `build()` invocation places at least one terminal child
            // (the single-empty-key case still encodes as `[terminal]`),
            // which will bump this further.
            max_used_idx: 1,
        }
    }

    /// Ensures all arrays cover at least `new_cap` indices.
    fn ensure_capacity(&mut self, new_cap: usize) {
        if new_cap > self.nodes.len() {
            self.nodes.resize(new_cap, Node::default());
            self.free_list.grow(new_cap);
        }
    }

    /// Places all descendants for keys[begin..end] starting at `parent` and
    /// `depth`, using an explicit stack of work frames instead of the call
    /// stack. The iterative form avoids Rust stack overflow on pathologically
    /// long keys (stack depth is bounded by the longest coded key length).
    ///
    /// Traversal order across parents differs from the recursive version
    /// (LIFO instead of pre-order DFS), but each parent's edges are still
    /// enqueued in one pass over the code-ascending `children` vector, so
    /// the `children_list` slice per parent remains code-ascending after
    /// the count-and-scatter flatten.
    fn build_trie(
        &mut self,
        coded_keys: &[Vec<u32>],
        begin: usize,
        end: usize,
        depth: usize,
        parent: u32,
    ) {
        // Work frames: (begin, end, depth, parent). Capacity is left as the
        // default heuristic. This is a push-all-children DFS, so the stack
        // holds every not-yet-visited child across the current frontier —
        // including pending sibling subtrees, not only frames along the
        // path from the root. Peak depth is therefore bounded by the
        // active frontier size, which for balanced tries stays modest but
        // can grow with very wide branching.
        let mut stack: Vec<(usize, usize, usize, u32)> = Vec::new();
        stack.push((begin, end, depth, parent));

        while let Some((begin, end, depth, parent)) = stack.pop() {
            // Collect distinct child labels and their key ranges. Keys arrive
            // in byte-sorted order, but codes are frequency-assigned — so byte
            // order and code order can disagree. Children are placed in
            // code-ascending order (code 0 terminal first, then extensions)
            // so that the final `children_list[child_offsets[parent]..]`
            // slice carries the same ordering invariant.
            let mut children: Vec<(u32, usize, usize)> = Vec::new();
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
            children.sort_unstable_by_key(|&(code, _, _)| code);

            // Find a base such that base XOR code is free for all children
            let base = self.find_base(&children);
            self.nodes[parent as usize].set_base(base);

            // Place child nodes. `children` is sorted by code ascending, so
            // pushing edges in this order preserves the code-ascending
            // invariant within each parent's slice once the count-and-scatter
            // flatten runs.
            let mut child_indices: Vec<u32> = Vec::with_capacity(children.len());
            for &(code, _, _) in &children {
                let child_idx = base ^ code;
                child_indices.push(child_idx);
                self.free_list.remove(child_idx);
                self.nodes[child_idx as usize].set_check(parent);
                self.edges.push((parent, child_idx));
                // Track the high-water mark so `build` can trim trailing
                // unused slots in O(1) instead of a reverse linear scan.
                if child_idx > self.max_used_idx {
                    self.max_used_idx = child_idx;
                }
            }

            // Set leaf/has_leaf flags and enqueue non-terminal children.
            for (ci, &(code, child_begin, child_end)) in children.iter().enumerate() {
                let child_idx = child_indices[ci];
                if code == 0 {
                    // Terminal symbol — this is a leaf node
                    debug_assert_eq!(child_end - child_begin, 1);
                    let value_id = child_begin as u32;
                    self.nodes[child_idx as usize].set_leaf(value_id);
                    self.nodes[parent as usize].set_has_leaf();
                } else {
                    // Non-terminal — push onto the work stack. The frame is
                    // self-contained (coded_keys is borrowed outside the loop).
                    stack.push((child_begin, child_end, depth + 1, child_idx));
                }
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

            // base == 0 or base == 1 would place children at sentinel/root
            // slots, but those are not in the free list, so the all_free
            // check below naturally rejects such bases. No explicit guard
            // needed.

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

            // Advance cursor to the next free slot
            let next = self.free_list.next[cursor as usize];
            if next == 0 {
                // Wrapped around to sentinel — all current free slots exhausted, grow.
                let old_cap = self.nodes.len();
                let new_cap = old_cap * 2;
                self.ensure_capacity(new_cap);
                cursor = old_cap as u32;
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
    /// - If `keys.len() > 2^31 - 1` (value_id is stored in 31 bits).
    pub fn build(keys: &[impl AsRef<[L]>]) -> Self {
        // value_id occupies the low 31 bits of Node::base; the MSB is the
        // IS_LEAF flag. Reject key counts that would silently truncate.
        assert!(
            keys.len() <= 0x7FFF_FFFF,
            "keys.len() exceeds the 31-bit value_id limit (max 2_147_483_647)"
        );

        // Verify sorted and no duplicates
        for w in keys.windows(2) {
            assert!(
                w[0].as_ref() < w[1].as_ref(),
                "keys must be sorted in ascending order with no duplicates"
            );
        }

        if keys.is_empty() {
            let empty: &[Vec<L>] = &[];
            // [sentinel, empty root], no edges.
            return Self::new(
                vec![Node::default(), Node::default()],
                vec![0u32, 0, 0], // child_offsets of length N+1 = 3
                Vec::new(),
                CodeMapper::build(empty),
            );
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

        // Root lives at index 1; index 0 is the invalid sentinel.
        ctx.build_trie(&coded_keys, 0, keys.len(), 0, 1);

        // Trim trailing unused nodes. `max_used_idx` is maintained
        // incrementally during child placement, so this is O(1) — no reverse
        // scan over the (often heavily over-allocated) `nodes` vector. Always
        // keep at least [sentinel, root] even if the root happens to have
        // base=0, which would otherwise make it indistinguishable from
        // `Node::default()` by value.
        let last_used = ctx.max_used_idx as usize;
        let final_len = (last_used + 1).max(2);
        ctx.nodes.truncate(final_len);

        // Flatten edges into child_offsets (CSR, len N+1) + children_list (len E)
        // via count-and-scatter — O(N + E), no sort. build_trie enqueues edges
        // in code-ascending order per parent, and scatter preserves that
        // order because it writes consecutive slots as edges are read.
        let edge_count = ctx.edges.len();
        // CSR offsets are stored as `u32`; if `edge_count` doesn't fit, the
        // prefix-sum loop below would silently wrap in release. Reject
        // explicitly instead. In practice this is unreachable given the
        // 31-bit `keys.len()` cap plus the free-list size bounds, but the
        // guard costs nothing and documents the invariant.
        assert!(
            edge_count <= u32::MAX as usize,
            "trie has too many edges to encode CSR offsets as u32 (max {})",
            u32::MAX
        );
        let mut child_offsets: Vec<u32> = vec![0u32; final_len + 1];
        // Count phase: child_offsets[p + 1] temporarily holds the count for p.
        // No overflow possible: each edge contributes +1 and `edge_count`
        // is bounded by `u32::MAX`, so no single bucket exceeds that either.
        for &(p, _) in &ctx.edges {
            child_offsets[p as usize + 1] += 1;
        }
        // Prefix sum → cumulative offsets.
        // `child_offsets[final_len]` will equal `edge_count`, which fits in
        // u32 by the assert above; every intermediate value is ≤ that.
        for i in 1..=final_len {
            child_offsets[i] += child_offsets[i - 1];
        }
        // Scatter phase. `write_pos[p]` starts at child_offsets[p] and is
        // bumped each time an edge for p is placed. `write_pos[p]` is
        // bounded by `child_offsets[p + 1] <= edge_count`, also ≤ u32::MAX.
        let mut write_pos = child_offsets[..final_len].to_vec();
        let mut children_list: Vec<u32> = vec![0u32; edge_count];
        for (p, c) in ctx.edges {
            let slot = write_pos[p as usize] as usize;
            children_list[slot] = c;
            write_pos[p as usize] += 1;
        }

        Self::new(ctx.nodes, child_offsets, children_list, code_map)
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
    fn children_list_has_no_duplicates() {
        // Each node index can appear as a child of at most one parent. Equivalent
        // to the v2 "sibling chain has no cycle" check but stronger: it also
        // verifies that the flattened children_list has no accidental duplicates.
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"ac", b"b", b"bc", b"c"];
        let da = DoubleArray::<u8>::build(&keys);
        let mut seen = std::collections::HashSet::new();
        for &c in &da.children_list {
            assert!(
                seen.insert(c),
                "child index {c} appears more than once in children_list"
            );
        }
    }

    #[test]
    fn free_list_head_survives_build_context_init() {
        // Regression: if BuildContext::new removed the sentinel (index 0)
        // from the free list, `first_free()` would return None right after
        // construction, forcing `find_base` to grow capacity immediately
        // and orphan every slot in the initial region.
        let ctx = BuildContext::new(16);
        assert_eq!(
            ctx.free_list.first_free(),
            Some(2),
            "first free slot after reserving root=1 must be 2, not None"
        );
        // And the sentinel must still be treated as unavailable.
        assert!(
            !ctx.free_list.is_free(0),
            "index 0 must not be reported as free"
        );
    }

    #[test]
    fn sentinel_is_default_after_build() {
        // nodes[0] must always be the default sentinel.
        let cases: Vec<Vec<&[u8]>> = vec![
            vec![],
            vec![b"a"],
            vec![b"a", b"ab", b"abc"],
            vec![b"n", b"na", b"ni", b"nu", b"shi"],
        ];
        for keys in cases {
            let da = DoubleArray::<u8>::build(&keys);
            assert_eq!(
                da.nodes[0],
                Node::default(),
                "nodes[0] must be default sentinel (keys={keys:?})"
            );
            assert!(
                da.nodes.len() >= 2,
                "must have at least [sentinel, root] (keys={keys:?})"
            );
        }
    }

    #[test]
    fn no_node_has_sentinel_as_parent() {
        // No non-sentinel, non-root node should have check() == 0.
        // (Root has check=0 by convention, but it is at index 1, not 0.)
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc", b"n", b"na"];
        let da = DoubleArray::<u8>::build(&keys);
        for (i, node) in da.nodes.iter().enumerate() {
            if i <= 1 || *node == Node::default() {
                continue;
            }
            assert_ne!(
                node.check(),
                0,
                "node {i} has check()=0 but is neither sentinel nor root"
            );
        }
    }

    #[test]
    fn child_offsets_structural() {
        // len == N+1, monotonic non-decreasing, starts at 0, ends at E.
        let cases: Vec<Vec<&[u8]>> = vec![
            vec![],
            vec![b"a"],
            vec![b"a", b"ab", b"abc", b"b", b"bc"],
            vec![b"n", b"na", b"ni", b"nu", b"shi"],
        ];
        for keys in cases {
            let da = DoubleArray::<u8>::build(&keys);
            let n = da.nodes.len();
            assert_eq!(da.child_offsets.len(), n + 1, "len == N+1 (keys={keys:?})");
            assert_eq!(da.child_offsets[0], 0, "starts at 0 (keys={keys:?})");
            assert_eq!(
                da.child_offsets[n] as usize,
                da.children_list.len(),
                "ends at children_list.len() (keys={keys:?})"
            );
            for w in da.child_offsets.windows(2) {
                assert!(w[0] <= w[1], "monotonic (keys={keys:?})");
            }
        }
    }

    #[test]
    fn child_offsets_sentinel_empty() {
        // nodes[0] is the sentinel and has no children in any build output.
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = DoubleArray::<u8>::build(&keys);
        assert_eq!(
            da.child_offsets[0], da.child_offsets[1],
            "sentinel range must be empty"
        );
    }

    #[test]
    fn children_list_check_invariant() {
        // Every entry in children_list[child_offsets[p]..child_offsets[p+1]]
        // must be a node whose check() equals p.
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc", b"n", b"na", b"shi"];
        let da = DoubleArray::<u8>::build(&keys);
        for p in 0..da.nodes.len() {
            let start = da.child_offsets[p] as usize;
            let end = da.child_offsets[p + 1] as usize;
            for &c in &da.children_list[start..end] {
                assert_eq!(
                    da.nodes[c as usize].check() as usize,
                    p,
                    "child {c} of parent {p} must have check()=={p}"
                );
            }
        }
    }

    #[test]
    fn child_ordering_terminal_first_then_code_ascending() {
        // For each parent with children, codes are ascending and — if the
        // parent has_leaf — the first child is the terminal (code 0, is_leaf).
        let keys: Vec<&[u8]> = vec![
            b"a", b"ab", b"abc", b"abd", b"b", b"bc", b"n", b"na", b"ni", b"nu",
        ];
        let da = DoubleArray::<u8>::build(&keys);
        for p in 0..da.nodes.len() {
            let start = da.child_offsets[p] as usize;
            let end = da.child_offsets[p + 1] as usize;
            let children = &da.children_list[start..end];
            if children.is_empty() {
                continue;
            }
            let parent_base = da.nodes[p].base();
            // Compute codes for each child via XOR inverse.
            let codes: Vec<u32> = children.iter().map(|&c| parent_base ^ c).collect();
            // Codes must be strictly ascending.
            for w in codes.windows(2) {
                assert!(
                    w[0] < w[1],
                    "parent {p} children must be code-ascending, got {:?}",
                    codes
                );
            }
            // If has_leaf is set, the first child is the terminal (code 0).
            if da.nodes[p].has_leaf() {
                assert_eq!(
                    codes[0], 0,
                    "parent {p} with has_leaf must have terminal first"
                );
                assert!(
                    da.nodes[children[0] as usize].is_leaf(),
                    "parent {p} first child must be a leaf node"
                );
            } else {
                assert_ne!(
                    codes[0], 0,
                    "parent {p} without has_leaf must not have terminal"
                );
            }
        }
    }

    #[test]
    fn build_handles_very_long_key_without_stack_overflow() {
        // Regression: the recursive `build_rec` would blow the Rust stack on
        // keys long enough that depth exceeded the default thread stack
        // (roughly 8 MiB on macOS/Linux, which is typically a few × 10^4
        // frames). The iterative rewrite uses a heap-allocated work stack,
        // so depth is bounded only by available memory.
        //
        // 200_000 chars is well past the recursive limit but stays small
        // enough to keep the test fast.
        let long_key: Vec<u8> = std::iter::repeat_n(b'a', 200_000).collect();
        let keys: Vec<&[u8]> = vec![&long_key];
        let da = DoubleArray::<u8>::build(&keys);
        assert!(da.num_nodes() > 1);
    }

    #[test]
    fn children_list_links_same_parent() {
        let da = DoubleArray::<u8>::build(&[b"ab", b"ac", b"ad"]);

        // Find node for 'a' from root (root is at nodes[1])
        let root_base = da.nodes[1].base();
        let code_a = da.code_map.get(b'a');
        let node_a_idx = root_base ^ code_a;
        assert_eq!(da.nodes[node_a_idx as usize].check(), 1);

        // Every entry in node_a's children_list slice must have check() == node_a_idx.
        let start = da.child_offsets[node_a_idx as usize] as usize;
        let end = da.child_offsets[node_a_idx as usize + 1] as usize;
        let children = &da.children_list[start..end];
        for &c in children {
            assert_eq!(
                da.nodes[c as usize].check(),
                node_a_idx,
                "child {c} of node_a must have check() == node_a_idx"
            );
        }

        // "ab", "ac", "ad" share prefix "a"; none of them equals "a", so node_a
        // has no terminal child. Extensions are code('b'), code('c'), code('d').
        let count = children.len();
        assert_eq!(count, 3);
    }
}
