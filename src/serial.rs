use crate::{CodeMapper, DoubleArray, Label, Node, TrieError};

pub(crate) const MAGIC: &[u8; 4] = b"LXTR";
pub(crate) const VERSION: u8 = 2;
/// Header: magic(4) + version(1) + reserved(3) + nodes_len(4) + siblings_len(4) + code_map_len(4) + reserved(4) = 24
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

impl<L: Label> DoubleArray<L> {
    /// Serializes the double-array trie to a byte vector.
    ///
    /// Format (v2):
    /// ```text
    /// Offset  Size  Content
    /// 0       4     Magic: "LXTR"
    /// 4       1     Version: 0x02
    /// 5       3     Reserved: [0, 0, 0]
    /// 8       4     nodes_len (u32 LE, in bytes)
    /// 12      4     siblings_len (u32 LE, in bytes)
    /// 16      4     code_map_len (u32 LE, in bytes)
    /// 20      4     Reserved: [0, 0, 0, 0]
    /// 24      N     nodes data (each node: base LE u32 + check LE u32)
    /// 24+N    S     siblings data (each: u32 LE)
    /// 24+N+S  C     code_map data
    /// ```
    pub fn as_bytes(&self) -> Vec<u8> {
        // SAFETY: Node is #[repr(C)] (two u32, 8 bytes, no padding).
        //         u32 is 4 bytes with no padding.
        //         LE platform is enforced by the crate-level compile_error.
        let nodes_raw = unsafe { as_byte_slice(&self.nodes) };
        let siblings_raw = unsafe { as_byte_slice(&self.siblings) };
        let code_map_size = self.code_map.serialized_size();

        debug_assert!(
            nodes_raw.len() <= u32::MAX as usize,
            "nodes section exceeds u32::MAX bytes"
        );
        debug_assert!(
            siblings_raw.len() <= u32::MAX as usize,
            "siblings section exceeds u32::MAX bytes"
        );
        debug_assert!(
            code_map_size <= u32::MAX as usize,
            "code_map section exceeds u32::MAX bytes"
        );

        let total = HEADER_SIZE + nodes_raw.len() + siblings_raw.len() + code_map_size;
        let mut buf = Vec::with_capacity(total);

        // Header (24 bytes)
        buf.extend_from_slice(MAGIC);
        buf.push(VERSION);
        buf.extend_from_slice(&[0, 0, 0]); // reserved
        buf.extend_from_slice(&(nodes_raw.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(siblings_raw.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(code_map_size as u32).to_le_bytes());
        buf.extend_from_slice(&[0, 0, 0, 0]); // reserved

        // Data sections — zero intermediate allocations
        buf.extend_from_slice(nodes_raw);
        buf.extend_from_slice(siblings_raw);
        self.code_map.write_to(&mut buf);

        buf
    }

    /// Deserializes a double-array trie from a byte slice.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TrieError> {
        if bytes.len() < HEADER_SIZE {
            return Err(TrieError::TruncatedData);
        }

        if &bytes[0..4] != MAGIC {
            return Err(TrieError::InvalidMagic);
        }

        if bytes[4] != VERSION {
            return Err(TrieError::InvalidVersion);
        }

        let nodes_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let siblings_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let code_map_len = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;

        let expected_size = HEADER_SIZE
            .checked_add(nodes_len)
            .and_then(|s| s.checked_add(siblings_len))
            .and_then(|s| s.checked_add(code_map_len))
            .ok_or(TrieError::TruncatedData)?;
        if bytes.len() < expected_size {
            return Err(TrieError::TruncatedData);
        }

        let mut offset = HEADER_SIZE;

        let nodes = deserialize_nodes(&bytes[offset..offset + nodes_len])
            .ok_or(TrieError::TruncatedData)?;
        offset += nodes_len;

        let siblings = deserialize_u32_slice(&bytes[offset..offset + siblings_len])
            .ok_or(TrieError::TruncatedData)?;
        offset += siblings_len;

        let (code_map, _consumed) = CodeMapper::from_bytes(&bytes[offset..offset + code_map_len])
            .ok_or(TrieError::TruncatedData)?;

        // Search logic assumes a root node at index 0
        if nodes.is_empty() {
            return Err(TrieError::TruncatedData);
        }

        // nodes and siblings must be parallel arrays of equal length
        if siblings.len() != nodes.len() {
            return Err(TrieError::TruncatedData);
        }

        Ok(Self::new(nodes, siblings, code_map))
    }
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
        assert_eq!(da.siblings, da2.siblings);
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
    fn header_alignment() {
        let da = build_empty_u8();
        let bytes = da.as_bytes();

        // Header is 24 bytes — nodes start at offset 24, which is 8-byte aligned
        assert_eq!(bytes[4], VERSION);
        assert_eq!(HEADER_SIZE, 24);
        assert!(HEADER_SIZE.is_multiple_of(8));
    }
}
