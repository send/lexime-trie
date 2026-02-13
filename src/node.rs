const IS_LEAF: u32 = 1 << 31;
const HAS_LEAF: u32 = 1 << 31;
const MASK: u32 = 0x7FFF_FFFF;

/// A node in the double-array trie.
///
/// Each node is exactly 8 bytes (`#[repr(C)]`):
/// - `base`: 31-bit XOR offset | IS_LEAF flag (MSB)
/// - `check`: 31-bit parent index | HAS_LEAF flag (MSB)
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Node {
    base: u32,
    check: u32,
}

impl Node {
    /// Returns the base value (XOR offset), masking out the IS_LEAF flag.
    #[inline]
    pub fn base(&self) -> u32 {
        self.base & MASK
    }

    /// Returns the check value (parent index), masking out the HAS_LEAF flag.
    #[inline]
    pub fn check(&self) -> u32 {
        self.check & MASK
    }

    /// Returns true if this node is a leaf (terminal node storing a value_id).
    #[inline]
    pub fn is_leaf(&self) -> bool {
        self.base & IS_LEAF != 0
    }

    /// Returns true if this node has a terminal child (code 0 child exists).
    #[inline]
    pub fn has_leaf(&self) -> bool {
        self.check & HAS_LEAF != 0
    }

    /// Returns the value_id stored in a leaf node.
    /// Only meaningful when `is_leaf()` is true.
    #[inline]
    pub fn value_id(&self) -> u32 {
        self.base & MASK
    }

    /// Sets the base value (XOR offset), preserving the IS_LEAF flag.
    #[inline]
    pub fn set_base(&mut self, base: u32) {
        debug_assert!(base & IS_LEAF == 0, "base value must fit in 31 bits");
        self.base = (self.base & IS_LEAF) | base;
    }

    /// Sets the check value (parent index), preserving the HAS_LEAF flag.
    #[inline]
    pub fn set_check(&mut self, check: u32) {
        debug_assert!(check & HAS_LEAF == 0, "check value must fit in 31 bits");
        self.check = (self.check & HAS_LEAF) | check;
    }

    /// Marks this node as a leaf and stores the value_id.
    #[inline]
    pub fn set_leaf(&mut self, value_id: u32) {
        debug_assert!(value_id & IS_LEAF == 0, "value_id must fit in 31 bits");
        self.base = IS_LEAF | value_id;
    }

    /// Sets the HAS_LEAF flag indicating a terminal child exists.
    #[inline]
    pub fn set_has_leaf(&mut self) {
        self.check |= HAS_LEAF;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem;

    #[test]
    fn node_size_is_8_bytes() {
        assert_eq!(mem::size_of::<Node>(), 8);
    }

    #[test]
    fn default_node() {
        let n = Node::default();
        assert_eq!(n.base(), 0);
        assert_eq!(n.check(), 0);
        assert!(!n.is_leaf());
        assert!(!n.has_leaf());
    }

    #[test]
    fn base_round_trip() {
        let mut n = Node::default();
        n.set_base(12345);
        assert_eq!(n.base(), 12345);
        assert!(!n.is_leaf());
    }

    #[test]
    fn check_round_trip() {
        let mut n = Node::default();
        n.set_check(67890);
        assert_eq!(n.check(), 67890);
        assert!(!n.has_leaf());
    }

    #[test]
    fn leaf_round_trip() {
        let mut n = Node::default();
        n.set_leaf(42);
        assert!(n.is_leaf());
        assert_eq!(n.value_id(), 42);
    }

    #[test]
    fn has_leaf_flag() {
        let mut n = Node::default();
        n.set_check(100);
        assert!(!n.has_leaf());
        n.set_has_leaf();
        assert!(n.has_leaf());
        assert_eq!(n.check(), 100);
    }

    #[test]
    fn set_base_preserves_leaf_flag() {
        let mut n = Node::default();
        n.set_leaf(10);
        assert!(n.is_leaf());
        // set_base clears IS_LEAF and sets base
        n.set_base(999);
        // After set_base, IS_LEAF is preserved if already set
        assert!(n.is_leaf());
        assert_eq!(n.base(), 999);
    }

    #[test]
    fn set_check_preserves_has_leaf_flag() {
        let mut n = Node::default();
        n.set_has_leaf();
        n.set_check(200);
        assert!(n.has_leaf());
        assert_eq!(n.check(), 200);
    }

    #[test]
    fn max_values() {
        let mut n = Node::default();
        n.set_base(MASK);
        assert_eq!(n.base(), MASK);

        n.set_check(MASK);
        assert_eq!(n.check(), MASK);

        n.set_leaf(MASK);
        assert_eq!(n.value_id(), MASK);
        assert!(n.is_leaf());
    }
}
