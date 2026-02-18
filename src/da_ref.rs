#[cfg(not(target_endian = "little"))]
compile_error!("DoubleArrayRef zero-copy deserialization requires a little-endian platform");

use std::marker::PhantomData;
use std::mem;

use crate::view::TrieView;
use crate::{
    CodeMapper, DoubleArray, Label, Node, PrefixMatch, ProbeResult, SearchMatch, TrieError,
};

/// A zero-copy reference to a serialized double-array trie (v2 format).
///
/// Unlike [`DoubleArray`], this type borrows the `nodes` and `siblings` data
/// directly from an external byte buffer (e.g. an mmap region), avoiding
/// heap allocation for those sections.
///
/// `code_map` is always heap-allocated since it is small and requires
/// deserialization.
pub struct DoubleArrayRef<'a, L: Label> {
    nodes: &'a [Node],
    siblings: &'a [u32],
    code_map: CodeMapper,
    _phantom: PhantomData<L>,
}

impl<'a, L: Label> DoubleArrayRef<'a, L> {
    /// Creates a zero-copy `DoubleArrayRef` from a byte slice (v2 format only).
    ///
    /// The byte slice must:
    /// - Use the LXTR v2 binary format (24-byte header)
    /// - Be aligned to at least 8 bytes (for `Node` access)
    ///
    /// # Errors
    ///
    /// Returns [`TrieError::InvalidMagic`] if the magic bytes don't match.
    /// Returns [`TrieError::InvalidVersion`] if the version is not v2.
    /// Returns [`TrieError::MisalignedData`] if the buffer is not properly aligned.
    /// Returns [`TrieError::TruncatedData`] if the buffer is too short.
    pub fn from_bytes_ref(bytes: &'a [u8]) -> Result<Self, TrieError> {
        const HEADER_SIZE: usize = crate::serial::HEADER_SIZE;

        if bytes.len() < HEADER_SIZE {
            return Err(TrieError::TruncatedData);
        }

        if &bytes[0..4] != b"LXTR" {
            return Err(TrieError::InvalidMagic);
        }

        if bytes[4] != 2 {
            return Err(TrieError::InvalidVersion);
        }

        let nodes_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let siblings_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let code_map_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;

        let expected_size = HEADER_SIZE + nodes_len + siblings_len + code_map_len;
        if bytes.len() < expected_size {
            return Err(TrieError::TruncatedData);
        }

        // Validate nodes section
        if !nodes_len.is_multiple_of(8) {
            return Err(TrieError::TruncatedData);
        }

        // Validate siblings section
        if !siblings_len.is_multiple_of(4) {
            return Err(TrieError::TruncatedData);
        }

        let nodes_ptr = bytes[HEADER_SIZE..].as_ptr();

        // Check alignment for Node (align 4) — header is 24 bytes so if buffer
        // base is 8-aligned, nodes_ptr is also 8-aligned. But verify at runtime.
        if !(nodes_ptr as usize).is_multiple_of(mem::align_of::<Node>()) {
            return Err(TrieError::MisalignedData);
        }

        let siblings_ptr = bytes[HEADER_SIZE + nodes_len..].as_ptr();
        if !(siblings_ptr as usize).is_multiple_of(mem::align_of::<u32>()) {
            return Err(TrieError::MisalignedData);
        }

        let node_count = nodes_len / mem::size_of::<Node>();
        let sibling_count = siblings_len / mem::size_of::<u32>();

        // SAFETY:
        // - `Node` is `#[repr(C)]` with two `u32` fields, size 8, align 4, no padding
        // - We verified alignment and bounds above
        // - The data is valid for any bit pattern (u32 fields)
        // - The lifetime `'a` ties the slice to the input buffer
        // - We only support little-endian platforms (x86_64, aarch64) where the
        //   in-memory layout matches the serialized LE format
        let nodes = unsafe { std::slice::from_raw_parts(nodes_ptr as *const Node, node_count) };

        let siblings =
            unsafe { std::slice::from_raw_parts(siblings_ptr as *const u32, sibling_count) };

        // code_map is always deserialized to heap
        let code_map_offset = HEADER_SIZE + nodes_len + siblings_len;
        let (code_map, _) =
            CodeMapper::from_bytes(&bytes[code_map_offset..code_map_offset + code_map_len])
                .ok_or(TrieError::TruncatedData)?;

        Ok(Self {
            nodes,
            siblings,
            code_map,
            _phantom: PhantomData,
        })
    }

    /// Returns a `TrieView` borrowing this ref's data.
    #[inline]
    fn view(&self) -> TrieView<'_, L> {
        TrieView {
            nodes: self.nodes,
            siblings: self.siblings,
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
            self.siblings.to_vec(),
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

    #[test]
    fn exact_match_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc"];
        let da = build_u8(&keys);
        let bytes = da.as_bytes();
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(&bytes).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da_ref.exact_match(key), Some(i as u32));
        }
        assert_eq!(da_ref.exact_match(b"xyz"), None);
    }

    #[test]
    fn common_prefix_search_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b"];
        let da = build_u8(&keys);
        let bytes = da.as_bytes();
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(&bytes).unwrap();

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
        let bytes = da.as_bytes();
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(&bytes).unwrap();

        let results: Vec<SearchMatch<u8>> = da_ref.predictive_search(b"a").collect();
        let mut value_ids: Vec<u32> = results.iter().map(|r| r.value_id).collect();
        value_ids.sort();
        assert_eq!(value_ids, vec![0, 1, 2]);
    }

    #[test]
    fn probe_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let bytes = da.as_bytes();
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(&bytes).unwrap();

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
        let bytes = da.as_bytes();
        let da_ref = DoubleArrayRef::<char>::from_bytes_ref(&bytes).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da_ref.exact_match(key), Some(i as u32));
        }
    }

    #[test]
    fn to_owned_works() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let bytes = da.as_bytes();
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(&bytes).unwrap();
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

        // Allocate a buffer with extra room, then find an offset that makes
        // the nodes section (at +24 from slice start) misaligned to 4 bytes.
        let buf = vec![0u8; bytes.len() + 16];
        let base = buf.as_ptr() as usize;

        // We need (base + offset + 24) % 4 != 0, i.e. (base + offset) % 4 != 0.
        // Try offsets 0..4 until we find one that is NOT 4-aligned.
        let offset = (0..4).find(|&o| (base + o + 24) % 4 != 0);

        if let Some(offset) = offset {
            let mut buf = vec![0u8; bytes.len() + 16];
            buf[offset..offset + bytes.len()].copy_from_slice(&bytes);
            let misaligned_slice = &buf[offset..offset + bytes.len()];

            assert!(matches!(
                DoubleArrayRef::<u8>::from_bytes_ref(misaligned_slice),
                Err(TrieError::MisalignedData)
            ));
        }
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
        let bytes = da.as_bytes();
        let da_ref = DoubleArrayRef::<u8>::from_bytes_ref(&bytes).unwrap();
        assert_eq!(da_ref.num_nodes(), da.num_nodes());
    }
}
