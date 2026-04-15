use std::marker::PhantomData;
use std::mem;

use crate::serial::{validate_structural_invariants, HeaderV3};
use crate::view::TrieView;
use crate::{
    CodeMapper, DoubleArray, Label, Node, PrefixMatch, ProbeResult, SearchMatch, TrieError,
};

/// A zero-copy reference to a serialized double-array trie (v3 format).
///
/// Unlike [`DoubleArray`], this type borrows the `nodes`, `child_offsets`,
/// and `children_list` sections directly from an external byte buffer (e.g.
/// an mmap region), avoiding heap allocation for those sections.
///
/// `code_map` is always heap-allocated since it is small and requires
/// deserialization.
pub struct DoubleArrayRef<'a, L: Label> {
    nodes: &'a [Node],
    child_offsets: &'a [u32],
    children_list: &'a [u32],
    code_map: CodeMapper,
    _phantom: PhantomData<L>,
}

impl<'a, L: Label> DoubleArrayRef<'a, L> {
    /// Creates a zero-copy `DoubleArrayRef` from a byte slice (v3 format only).
    ///
    /// The byte slice must:
    /// - Use the LXTR v3 binary format (24-byte header)
    /// - Be aligned to at least 4 bytes (for `Node` and `u32` access)
    ///
    /// # Errors
    ///
    /// Returns [`TrieError::InvalidMagic`] if the magic bytes don't match.
    /// Returns [`TrieError::InvalidVersion`] if the version is not v3.
    /// Returns [`TrieError::MisalignedData`] if the buffer is not properly aligned.
    /// Returns [`TrieError::TruncatedData`] if the buffer is too short or the
    /// CSR structural invariants are violated.
    pub fn from_bytes_ref(bytes: &'a [u8]) -> Result<Self, TrieError> {
        let header = HeaderV3::parse(bytes)?;

        let nodes_ptr = bytes[header.nodes_offset()..].as_ptr();
        let child_offsets_ptr = bytes[header.child_offsets_offset()..].as_ptr();
        let children_list_ptr = bytes[header.children_list_offset()..].as_ptr();

        // Alignment checks: all three sections cast to types with 4-byte
        // alignment (Node is align 4, u32 is align 4). A 4-byte aligned buffer
        // guarantees section alignment because each section offset is itself a
        // multiple of 4.
        if !(nodes_ptr as usize).is_multiple_of(mem::align_of::<Node>()) {
            return Err(TrieError::MisalignedData);
        }
        if !(child_offsets_ptr as usize).is_multiple_of(mem::align_of::<u32>()) {
            return Err(TrieError::MisalignedData);
        }
        if !(children_list_ptr as usize).is_multiple_of(mem::align_of::<u32>()) {
            return Err(TrieError::MisalignedData);
        }

        let child_offsets_count = header.nodes_count + 1;

        // SAFETY:
        // - `Node` is `#[repr(C)]` with two `u32` fields, size 8, align 4, no padding.
        // - `u32` is size 4 align 4 with no invalid bit patterns.
        // - We verified alignment and bounds above.
        // - The lifetime `'a` ties the slices to the input buffer.
        // - We only support little-endian platforms where the in-memory layout
        //   matches the serialized LE format.
        let nodes =
            unsafe { std::slice::from_raw_parts(nodes_ptr as *const Node, header.nodes_count) };
        let child_offsets = unsafe {
            std::slice::from_raw_parts(child_offsets_ptr as *const u32, child_offsets_count)
        };
        let children_list = unsafe {
            std::slice::from_raw_parts(children_list_ptr as *const u32, header.children_count)
        };

        validate_structural_invariants(child_offsets, children_list)?;

        // code_map is always deserialized to heap
        let (code_map, _) = CodeMapper::from_bytes(
            &bytes[header.code_map_offset()..header.code_map_offset() + header.code_map_len],
        )
        .ok_or(TrieError::TruncatedData)?;

        Ok(Self {
            nodes,
            child_offsets,
            children_list,
            code_map,
            _phantom: PhantomData,
        })
    }

    /// Returns a `TrieView` borrowing this ref's data.
    #[inline]
    fn view(&self) -> TrieView<'_, L> {
        TrieView {
            nodes: self.nodes,
            child_offsets: self.child_offsets,
            children_list: self.children_list,
            code_map: &self.code_map,
            _phantom: PhantomData,
        }
    }

    /// Returns the number of nodes in the trie.
    pub fn num_nodes(&self) -> usize {
        self.nodes.len()
    }

    /// Exact match search. Returns the value_id if the key exists.
    #[inline]
    pub fn exact_match(&self, key: &[L]) -> Option<u32> {
        self.view().exact_match(key)
    }

    /// Common prefix search. Returns an iterator over all prefixes of `query`
    /// that exist as keys in the trie.
    pub fn common_prefix_search<'b>(
        &'b self,
        query: &'b [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'b {
        self.view().common_prefix_search(query)
    }

    /// Predictive search. Returns an iterator over all keys that start with `prefix`.
    pub fn predictive_search<'b>(
        &'b self,
        prefix: &'b [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'b {
        self.view().predictive_search(prefix)
    }

    /// Probe a key. Returns whether the key exists and whether it has children.
    #[inline]
    pub fn probe(&self, key: &[L]) -> ProbeResult {
        self.view().probe(key)
    }

    /// Converts this zero-copy reference to an owned [`DoubleArray`].
    pub fn to_owned(&self) -> DoubleArray<L> {
        DoubleArray::new(
            self.nodes.to_vec(),
            self.child_offsets.to_vec(),
            self.children_list.to_vec(),
            self.code_map.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_u8(keys: &[&[u8]]) -> DoubleArray<u8> {
        DoubleArray::build(keys)
    }

    /// 8-byte aligned buffer for `from_bytes_ref` tests.
    ///
    /// `Vec<u8>::as_bytes()` only guarantees 1-byte alignment. Standard allocators
    /// happen to return 8-16 byte aligned memory, but Miri's allocator does not.
    /// This wrapper uses `Vec<u64>` as backing storage to guarantee 8-byte alignment.
    struct AlignedBuffer {
        _backing: Vec<u64>,
        len: usize,
    }

    impl AlignedBuffer {
        fn new(bytes: &[u8]) -> Self {
            let n = bytes.len().div_ceil(8);
            let mut backing = vec![0u64; n];
            // SAFETY: copying bytes into a u64 buffer; u64 has no invalid bit patterns.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    backing.as_mut_ptr() as *mut u8,
                    bytes.len(),
                );
            }
            Self {
                _backing: backing,
                len: bytes.len(),
            }
        }

        fn as_slice(&self) -> &[u8] {
            // SAFETY: _backing is at least `self.len` bytes; u64 alignment (8)
            // satisfies the Node/u32 alignment requirement (4).
            unsafe { std::slice::from_raw_parts(self._backing.as_ptr() as *const u8, self.len) }
        }
    }

    #[test]
    fn exact_match_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc"];
        let da = build_u8(&keys);
        let buf = AlignedBuffer::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(buf.as_slice()).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da_ref.exact_match(key), Some(i as u32));
        }
        assert_eq!(da_ref.exact_match(b"xyz"), None);
    }

    #[test]
    fn common_prefix_search_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b"];
        let da = build_u8(&keys);
        let buf = AlignedBuffer::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(buf.as_slice()).unwrap();

        let results: Vec<PrefixMatch> = da_ref.common_prefix_search(b"abcd").collect();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].len, 1);
        assert_eq!(results[1].len, 2);
        assert_eq!(results[2].len, 3);
    }

    #[test]
    fn predictive_search_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc"];
        let da = build_u8(&keys);
        let buf = AlignedBuffer::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(buf.as_slice()).unwrap();

        let results: Vec<SearchMatch<u8>> = da_ref.predictive_search(b"a").collect();
        let mut value_ids: Vec<u32> = results.iter().map(|r| r.value_id).collect();
        value_ids.sort();
        assert_eq!(value_ids, vec![0, 1, 2]);
    }

    #[test]
    fn probe_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let buf = AlignedBuffer::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(buf.as_slice()).unwrap();

        let r = da_ref.probe(b"a");
        assert_eq!(r.value, Some(0));
        assert!(r.has_children);

        let r = da_ref.probe(b"abc");
        assert_eq!(r.value, Some(2));
        assert!(!r.has_children);

        let r = da_ref.probe(b"xyz");
        assert_eq!(r.value, None);
        assert!(!r.has_children);
    }

    #[test]
    fn char_round_trip_via_ref() {
        let keys: Vec<Vec<char>> = vec![
            "あ".chars().collect(),
            "あい".chars().collect(),
            "あいう".chars().collect(),
            "か".chars().collect(),
        ];
        let da = DoubleArray::<char>::build(&keys);
        let buf = AlignedBuffer::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<char>::from_bytes_ref(buf.as_slice()).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da_ref.exact_match(key), Some(i as u32));
        }
    }

    #[test]
    fn to_owned_works() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let buf = AlignedBuffer::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(buf.as_slice()).unwrap();
        let da_owned = da_ref.to_owned();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da_owned.exact_match(key), Some(i as u32));
        }
    }

    #[test]
    fn misaligned_data_error() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab"];
        let da = build_u8(&keys);
        let bytes = da.as_bytes();

        // Allocate a buffer with extra room, using Vec<u64> for guaranteed
        // 8-byte base alignment. We write into this buffer directly so the
        // offset calculation matches the actual slice being tested.
        let mut backing = vec![0u64; (bytes.len() + 16).div_ceil(8)];
        let buf = unsafe {
            std::slice::from_raw_parts_mut(backing.as_mut_ptr() as *mut u8, backing.len() * 8)
        };
        let base = buf.as_ptr() as usize;

        // We need (base + offset + 24) % 4 != 0.
        // Since 24 % 4 == 0, we need (base + offset) % 4 != 0.
        // At least 3 of offsets 0..4 satisfy this.
        let offset = (0..4)
            .find(|&o| !(base + o + 24).is_multiple_of(4))
            .expect("at least one offset should be misaligned");

        buf[offset..offset + bytes.len()].copy_from_slice(&bytes);
        let misaligned_slice = &buf[offset..offset + bytes.len()];

        assert!(matches!(
            DoubleArrayRef::<u8>::from_bytes_ref(misaligned_slice),
            Err(TrieError::MisalignedData)
        ));
    }

    #[test]
    fn invalid_version_rejected() {
        let keys: Vec<&[u8]> = vec![b"a"];
        let da = build_u8(&keys);
        let mut bytes = da.as_bytes();
        bytes[4] = 99; // bogus version
        assert!(matches!(
            DoubleArrayRef::<u8>::from_bytes_ref(&bytes),
            Err(TrieError::InvalidVersion)
        ));
    }

    #[test]
    fn truncated_data_error() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab"];
        let da = build_u8(&keys);
        let bytes = da.as_bytes();

        // Truncate to less than header
        assert!(matches!(
            DoubleArrayRef::<u8>::from_bytes_ref(&bytes[..10]),
            Err(TrieError::TruncatedData)
        ));

        // Truncate data section
        assert!(matches!(
            DoubleArrayRef::<u8>::from_bytes_ref(&bytes[..24]),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn num_nodes_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let buf = AlignedBuffer::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(buf.as_slice()).unwrap();
        assert_eq!(da_ref.num_nodes(), da.num_nodes());
    }
}
