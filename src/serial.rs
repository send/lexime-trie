use crate::{CodeMapper, DoubleArray, Label, Node, TrieError};

pub(crate) const MAGIC: &[u8; 4] = b"LXTR";
pub(crate) const VERSION: u8 = 3;
/// Header: magic(4) + version(1) + reserved(3) + nodes_count(4) + children_count(4) + code_map_len(4) + reserved(4) = 24
pub(crate) const HEADER_SIZE: usize = 24;

/// Reinterprets a `&[T]` as `&[u8]`.
///
/// # Safety
/// `T` must be `#[repr(C)]` / `#[repr(transparent)]` with no padding.
/// The crate-level `compile_error!` guarantees we are on a little-endian platform,
/// so the in-memory layout matches the serialised LE format.
#[inline]
unsafe fn as_byte_slice<T>(slice: &[T]) -> &[u8] {
    std::slice::from_raw_parts(slice.as_ptr() as *const u8, std::mem::size_of_val(slice))
}

/// Header view parsed from a v3 byte buffer.
///
/// All sizes are derived from `nodes_count` and `children_count`; the format
/// carries those two counts and the variable `code_map_len` only.
pub(crate) struct HeaderV3 {
    pub nodes_count: usize,
    pub children_count: usize,
    pub code_map_len: usize,
    pub nodes_bytes: usize,
    pub child_offsets_bytes: usize,
    pub children_list_bytes: usize,
}

impl HeaderV3 {
    /// Parses and validates the header, checking magic/version/size arithmetic.
    /// Does not touch payload bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, TrieError> {
        if bytes.len() < HEADER_SIZE {
            return Err(TrieError::TruncatedData);
        }
        if &bytes[0..4] != MAGIC {
            return Err(TrieError::InvalidMagic);
        }
        if bytes[4] != VERSION {
            return Err(TrieError::InvalidVersion);
        }

        let nodes_count = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let children_count = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let code_map_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;

        if nodes_count < 2 {
            // nodes[0] = sentinel, nodes[1] = root are both required.
            return Err(TrieError::TruncatedData);
        }

        let nodes_bytes = nodes_count
            .checked_mul(std::mem::size_of::<Node>())
            .ok_or(TrieError::TruncatedData)?;
        let child_offsets_count = nodes_count.checked_add(1).ok_or(TrieError::TruncatedData)?;
        let child_offsets_bytes = child_offsets_count
            .checked_mul(4)
            .ok_or(TrieError::TruncatedData)?;
        let children_list_bytes = children_count
            .checked_mul(4)
            .ok_or(TrieError::TruncatedData)?;

        let total_size = HEADER_SIZE
            .checked_add(nodes_bytes)
            .and_then(|s| s.checked_add(child_offsets_bytes))
            .and_then(|s| s.checked_add(children_list_bytes))
            .and_then(|s| s.checked_add(code_map_len))
            .ok_or(TrieError::TruncatedData)?;
        if bytes.len() < total_size {
            return Err(TrieError::TruncatedData);
        }

        // Note: `total_size` was validated above against `bytes.len()`; we
        // don't store it since callers derive section bounds from the
        // individual size fields.
        let _ = total_size;

        Ok(Self {
            nodes_count,
            children_count,
            code_map_len,
            nodes_bytes,
            child_offsets_bytes,
            children_list_bytes,
        })
    }

    /// Byte offset where each section begins within the buffer.
    #[inline]
    pub fn nodes_offset(&self) -> usize {
        HEADER_SIZE
    }
    #[inline]
    pub fn child_offsets_offset(&self) -> usize {
        HEADER_SIZE + self.nodes_bytes
    }
    #[inline]
    pub fn children_list_offset(&self) -> usize {
        HEADER_SIZE + self.nodes_bytes + self.child_offsets_bytes
    }
    #[inline]
    pub fn code_map_offset(&self) -> usize {
        HEADER_SIZE + self.nodes_bytes + self.child_offsets_bytes + self.children_list_bytes
    }
}

impl<L: Label> DoubleArray<L> {
    /// Serializes the double-array trie to a byte vector.
    ///
    /// Format (v3):
    /// ```text
    /// Offset                        Size       Content
    /// 0                             4          Magic: "LXTR"
    /// 4                             1          Version: 0x03
    /// 5                             3          Reserved: [0, 0, 0]
    /// 8                             4          nodes_count    (u32 LE, = N)
    /// 12                            4          children_count (u32 LE, = E)
    /// 16                            4          code_map_len   (u32 LE, bytes)
    /// 20                            4          Reserved: [0, 0, 0, 0]
    /// 24                            N*8        nodes (base LE u32 + check LE u32)
    /// 24+N*8                        (N+1)*4    child_offsets (each: u32 LE)
    /// 24+N*8+(N+1)*4                E*4        children_list (each: u32 LE)
    /// 24+N*8+(N+1)*4+E*4            M          code_map data
    /// ```
    pub fn as_bytes(&self) -> Vec<u8> {
        // SAFETY: Node is #[repr(C)] (two u32, 8 bytes, no padding).
        //         u32 is 4 bytes with no padding.
        //         LE platform is enforced by the crate-level compile_error.
        let nodes_raw = unsafe { as_byte_slice(&self.nodes) };
        let child_offsets_raw = unsafe { as_byte_slice(&self.child_offsets) };
        let children_list_raw = unsafe { as_byte_slice(&self.children_list) };
        let code_map_size = self.code_map.serialized_size();

        let nodes_count = self.nodes.len();
        let children_count = self.children_list.len();

        debug_assert!(
            nodes_count <= u32::MAX as usize,
            "nodes_count exceeds u32::MAX"
        );
        debug_assert!(
            children_count <= u32::MAX as usize,
            "children_count exceeds u32::MAX"
        );
        debug_assert_eq!(
            self.child_offsets.len(),
            nodes_count + 1,
            "child_offsets length must equal nodes_count + 1"
        );
        debug_assert!(
            code_map_size <= u32::MAX as usize,
            "code_map section exceeds u32::MAX bytes"
        );

        let total = HEADER_SIZE
            .checked_add(nodes_raw.len())
            .and_then(|s| s.checked_add(child_offsets_raw.len()))
            .and_then(|s| s.checked_add(children_list_raw.len()))
            .and_then(|s| s.checked_add(code_map_size))
            .expect("total serialized size exceeds usize::MAX");
        let mut buf = Vec::with_capacity(total);

        // Header (24 bytes)
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&[0, 0, 0]); // reserved
        buf.extend_from_slice(&(nodes_count as u32).to_le_bytes());
        buf.extend_from_slice(&(children_count as u32).to_le_bytes());
        buf.extend_from_slice(&(code_map_size as u32).to_le_bytes());
        buf.extend_from_slice(&[0, 0, 0, 0]); // reserved

        // Data sections — zero intermediate allocations
        buf.extend_from_slice(nodes_raw);
        buf.extend_from_slice(child_offsets_raw);
        buf.extend_from_slice(children_list_raw);
        self.code_map.write_to(&mut buf);

        buf
    }

    /// Deserializes a double-array trie from a byte slice.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TrieError> {
        let header = HeaderV3::parse(bytes)?;

        let nodes = deserialize_nodes(
            &bytes[header.nodes_offset()..header.nodes_offset() + header.nodes_bytes],
        )
        .ok_or(TrieError::TruncatedData)?;

        let child_offsets = deserialize_u32_slice(
            &bytes[header.child_offsets_offset()
                ..header.child_offsets_offset() + header.child_offsets_bytes],
        )
        .ok_or(TrieError::TruncatedData)?;

        let children_list = deserialize_u32_slice(
            &bytes[header.children_list_offset()
                ..header.children_list_offset() + header.children_list_bytes],
        )
        .ok_or(TrieError::TruncatedData)?;

        let (code_map, _consumed) = CodeMapper::from_bytes(
            &bytes[header.code_map_offset()..header.code_map_offset() + header.code_map_len],
        )
        .ok_or(TrieError::TruncatedData)?;

        validate_cheap(&nodes, &child_offsets, &children_list)?;

        Ok(Self::new(nodes, child_offsets, children_list, code_map))
    }
}

/// O(1) sanity checks on `nodes` + `child_offsets` + `children_list` run at
/// load time.
///
/// Verified invariants:
/// - `child_offsets.len() == nodes.len() + 1`
/// - `child_offsets[0] == 0`
/// - `child_offsets[N] == children_list.len()`
///
/// Monotonicity is intentionally NOT checked here: it would be O(N) and
/// defeat zero-copy deserialization for the `from_bytes_ref` path. A
/// non-monotonic or otherwise malformed offset cannot cause UB (Rust slice
/// indexing is always bounds-checked — corruption surfaces as a panic on
/// query, never as memory unsafety). Callers that need hard guarantees can
/// run [`validate_strict`] explicitly.
pub(crate) fn validate_cheap(
    nodes: &[Node],
    child_offsets: &[u32],
    children_list: &[u32],
) -> Result<(), TrieError> {
    if child_offsets.len() != nodes.len() + 1 {
        return Err(TrieError::TruncatedData);
    }
    if child_offsets.first() != Some(&0) {
        return Err(TrieError::TruncatedData);
    }
    if child_offsets.last() != Some(&(children_list.len() as u32)) {
        return Err(TrieError::TruncatedData);
    }
    Ok(())
}

/// Full O(N) validation: cheap checks + monotonicity of `child_offsets`.
///
/// Not invoked by `from_bytes` / `from_bytes_ref`. Currently used only by
/// tests; a public wrapper on `DoubleArray` / `DoubleArrayRef` can be added
/// later if callers need to reject malformed input before any query.
#[cfg(test)]
pub(crate) fn validate_strict(
    nodes: &[Node],
    child_offsets: &[u32],
    children_list: &[u32],
) -> Result<(), TrieError> {
    validate_cheap(nodes, child_offsets, children_list)?;
    for w in child_offsets.windows(2) {
        if w[0] > w[1] {
            return Err(TrieError::TruncatedData);
        }
    }
    Ok(())
}

fn deserialize_nodes(bytes: &[u8]) -> Option<Vec<Node>> {
    if !bytes.len().is_multiple_of(8) {
        return None;
    }
    let count = bytes.len() / std::mem::size_of::<Node>();
    // SAFETY: Node is #[repr(C)], 8 bytes, no padding. LE layout matches serialised format.
    // We use with_capacity + set_len to avoid redundant zero-initialisation.
    let mut nodes = Vec::<Node>::with_capacity(count);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), nodes.as_mut_ptr() as *mut u8, bytes.len());
        nodes.set_len(count);
    }
    Some(nodes)
}

fn deserialize_u32_slice(bytes: &[u8]) -> Option<Vec<u32>> {
    if !bytes.len().is_multiple_of(4) {
        return None;
    }
    let count = bytes.len() / 4;
    // SAFETY: u32 is 4 bytes with no padding. LE layout matches serialised format.
    let mut out = Vec::<u32>::with_capacity(count);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr() as *mut u8, bytes.len());
        out.set_len(count);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DoubleArray;

    #[test]
    fn round_trip_empty() {
        let da = build_empty_u8();
        let bytes = da.as_bytes();
        let da2 = DoubleArray::<u8>::from_bytes(&bytes).unwrap();
        assert_eq!(da.nodes, da2.nodes);
        assert_eq!(da.child_offsets, da2.child_offsets);
        assert_eq!(da.children_list, da2.children_list);
    }

    #[test]
    fn round_trip_u8() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc"];
        let da = DoubleArray::<u8>::build(&keys);
        let bytes = da.as_bytes();
        let da2 = DoubleArray::<u8>::from_bytes(&bytes).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da2.exact_match(key), Some(i as u32));
        }
        assert_eq!(da2.exact_match(b"xyz"), None);
    }

    #[test]
    fn round_trip_char() {
        let keys: Vec<Vec<char>> = vec![
            "あ".chars().collect(),
            "あい".chars().collect(),
            "あいう".chars().collect(),
            "か".chars().collect(),
        ];
        let da = DoubleArray::<char>::build(&keys);
        let bytes = da.as_bytes();
        let da2 = DoubleArray::<char>::from_bytes(&bytes).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da2.exact_match(key), Some(i as u32));
        }
    }

    fn build_empty_u8() -> DoubleArray<u8> {
        let keys: Vec<&[u8]> = vec![];
        DoubleArray::<u8>::build(&keys)
    }

    #[test]
    fn invalid_magic() {
        let mut bytes = build_empty_u8().as_bytes();
        bytes[0] = b'X';
        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&bytes),
            Err(TrieError::InvalidMagic)
        ));
    }

    #[test]
    fn invalid_version() {
        let mut bytes = build_empty_u8().as_bytes();
        bytes[4] = 99;
        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&bytes),
            Err(TrieError::InvalidVersion)
        ));
    }

    #[test]
    fn truncated_data() {
        let bytes = build_empty_u8().as_bytes();
        // Truncate to just the header (no data sections)
        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&bytes[..HEADER_SIZE]),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn truncated_header() {
        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&[0; 4]),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn round_trip_preserves_search_behavior() {
        let keys: Vec<&[u8]> = vec![b"n", b"na", b"ni", b"nu", b"shi"];
        let da = DoubleArray::<u8>::build(&keys);
        let bytes = da.as_bytes();
        let da2 = DoubleArray::<u8>::from_bytes(&bytes).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da2.exact_match(key), Some(i as u32));
        }

        let r = da2.probe(b"n");
        assert_eq!(r.value, Some(0));
        assert!(r.has_children);

        let r = da2.probe(b"shi");
        assert_eq!(r.value, Some(4));
        assert!(!r.has_children);

        let results: Vec<_> = da2.common_prefix_search(b"nab").collect();
        assert_eq!(results.len(), 2); // "n" and "na"

        let results: Vec<_> = da2.predictive_search(b"n").collect();
        assert_eq!(results.len(), 4); // "n", "na", "ni", "nu"
    }

    #[test]
    fn sentinel_preserved_through_round_trip() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = DoubleArray::<u8>::build(&keys);
        let bytes = da.as_bytes();
        let da2 = DoubleArray::<u8>::from_bytes(&bytes).unwrap();
        assert_eq!(da2.nodes[0], crate::Node::default());
        assert!(da2.nodes.len() >= 2);
    }

    #[test]
    fn rejects_less_than_two_nodes() {
        // Craft a v3 header claiming nodes_count = 1. The parser must reject
        // it because [sentinel, root] requires at least 2 nodes.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&1u32.to_le_bytes()); // nodes_count = 1 (invalid)
        bytes.extend_from_slice(&0u32.to_le_bytes()); // children_count
        bytes.extend_from_slice(&0u32.to_le_bytes()); // code_map_len
        bytes.extend_from_slice(&[0, 0, 0, 0]);

        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&bytes),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn v2_buffer_rejected_as_invalid_version() {
        // v2 had VERSION=2; writing that into a v3 buffer must fail with InvalidVersion.
        let da = DoubleArray::<u8>::build(&[b"a"]);
        let mut bytes = da.as_bytes();
        bytes[4] = 2;
        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&bytes),
            Err(TrieError::InvalidVersion)
        ));
    }

    #[test]
    fn count_overflow_rejected() {
        // Craft a header claiming nodes_count = u32::MAX. The resulting
        // section size (N * 8) overflows usize on 32-bit platforms and
        // exceeds the buffer on 64-bit. Either way we must return
        // TruncatedData, not UB.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.push(VERSION);
        bytes.extend_from_slice(&[0, 0, 0]);
        bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // nodes_count
        bytes.extend_from_slice(&0u32.to_le_bytes()); // children_count
        bytes.extend_from_slice(&0u32.to_le_bytes()); // code_map_len
        bytes.extend_from_slice(&[0, 0, 0, 0]);

        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&bytes),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn validate_strict_rejects_non_monotonic_child_offsets() {
        // The load path (from_bytes / from_bytes_ref) deliberately skips the
        // O(N) monotonicity check to keep zero-copy deserialization O(1).
        // `validate_strict` is the opt-in O(N) path callers can run when
        // they need to reject malformed input before any query.
        let nodes = vec![crate::Node::default(); 3];
        let child_offsets: Vec<u32> = vec![0, 5, 2, 0]; // non-monotonic: 5 > 2
        let children_list: Vec<u32> = vec![0u32; 0];
        assert!(matches!(
            validate_strict(&nodes, &child_offsets, &children_list),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn validate_cheap_rejects_wrong_child_offsets_length() {
        // child_offsets.len() must equal nodes.len() + 1. If a corrupt buffer
        // claims N nodes but embeds a different-length child_offsets, the
        // load path must reject at the O(1) check.
        let nodes = vec![crate::Node::default(); 3];
        // Expected: length 4. Provide length 3 instead.
        let child_offsets: Vec<u32> = vec![0, 0, 0];
        let children_list: Vec<u32> = vec![];
        assert!(matches!(
            validate_cheap(&nodes, &child_offsets, &children_list),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn rejects_child_offsets_end_mismatch() {
        // Corrupt the final child_offsets entry so it doesn't match children_list.len().
        let keys: Vec<&[u8]> = vec![b"a", b"ab"];
        let da = DoubleArray::<u8>::build(&keys);
        let mut bytes = da.as_bytes();
        let n = da.nodes.len();
        let child_offsets_start = HEADER_SIZE + n * 8;
        // child_offsets[N] is at child_offsets_start + N*4
        let last_entry_offset = child_offsets_start + n * 4;
        // Set it to 0 — definitely doesn't match children_list length.
        bytes[last_entry_offset..last_entry_offset + 4].copy_from_slice(&0u32.to_le_bytes());

        assert!(matches!(
            DoubleArray::<u8>::from_bytes(&bytes),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn header_alignment() {
        let da = build_empty_u8();
        let bytes = da.as_bytes();

        // Header is 24 bytes — nodes start at offset 24, which is 8-byte aligned
        assert_eq!(bytes[4], VERSION);
        assert_eq!(HEADER_SIZE, 24);
        assert!(HEADER_SIZE.is_multiple_of(8));
    }
}
