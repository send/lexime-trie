use std::marker::PhantomData;
use std::mem;

use crate::serial::{validate_cheap, HeaderV3};
use crate::view::TrieView;
use crate::{
    CodeMapper, DoubleArray, Label, Node, PrefixMatch, ProbeResult, SearchMatch, TrieError,
    TrieSearch,
};

/// A zero-copy reference to a serialized double-array trie (v3 format).
///
/// Unlike [`DoubleArray`], this type borrows the `nodes`, `child_offsets`,
/// and `children_list` sections directly from an external byte buffer (e.g.
/// an mmap region), avoiding heap allocation for those sections.
///
/// `code_map` is always heap-allocated since it is small and requires
/// deserialization.
///
/// # Internal representation
///
/// Internally this stores **raw pointers + lengths** rather than real
/// `&'a [T]` slices because [`DoubleArrayBacked`](crate::DoubleArrayBacked)
/// holds a `DoubleArrayRef<'static, L>` co-located with the byte buffer it
/// borrows from: if the inner type contained real shared references
/// Stacked Borrows would treat them as strongly-protected during
/// `drop`, conflicting with reclamation of the backing storage.
#[derive(Clone)]
pub struct DoubleArrayRef<'a, L: Label> {
    nodes_ptr: *const Node,
    nodes_len: usize,
    child_offsets_ptr: *const u32,
    child_offsets_len: usize,
    children_list_ptr: *const u32,
    children_list_len: usize,
    pub(crate) code_map: CodeMapper,
    _marker: PhantomData<(&'a [u8], L)>,
}

// SAFETY: The raw pointer fields point into a `[u8]` borrow that is
// tracked by the `PhantomData<&'a [u8]>` marker. Shared `&[u8]` is
// `Send + Sync`; reconstructing `&[Node]` / `&[u32]` over the same
// memory inside `view()` is equivalent to holding those shared slices,
// which are likewise `Send + Sync`. `CodeMapper` (owned `Vec<u32>` +
// `u32`) is `Send + Sync`. Raw pointers lose these auto-traits by
// default, so we re-assert them — but `L` also appears in the struct
// via `PhantomData<(&'a [u8], L)>`, so we must propagate `L`'s own
// auto-traits to avoid bypassing a downstream `!Send`/`!Sync` `Label`
// type's contract (e.g. a `Label` containing `PhantomData<Rc<()>>`).
// The crate's pre-blessed `Label` impls (`u8`, `char`) are
// `Send + Sync`, so this is a no-op for typical use.
unsafe impl<'a, L: Label + Send> Send for DoubleArrayRef<'a, L> {}
unsafe impl<'a, L: Label + Sync> Sync for DoubleArrayRef<'a, L> {}

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
    /// Returns [`TrieError::InvalidVersion`] if the version is not v3 (the
    /// observed version byte is attached to the variant).
    /// Returns [`TrieError::MisalignedData`] if the buffer is not properly aligned.
    /// Returns [`TrieError::TruncatedData`] if the buffer ends before the
    /// declared header, sections, or code-map block are fully present.
    /// Returns [`TrieError::InvalidStructure`] if the buffer is long enough
    /// but violates a v3 structural invariant.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, TrieError> {
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

        // Validate through transient slices; store as (ptr, len) to avoid
        // long-lived `&[T]` references inside `Self` (see struct-level doc).
        //
        // SAFETY:
        // - `Node` is `#[repr(C)]` with two `u32` fields, size 8, align 4, no padding.
        // - `u32` is size 4 align 4 with no invalid bit patterns.
        // - Pointer alignment (4 bytes for both `Node` and `u32`) was
        //   verified by the three `is_multiple_of` checks immediately above.
        // - Section bounds — that each `{nodes, child_offsets, children_list}`
        //   slice fits inside the input buffer — were verified by
        //   `HeaderV3::parse` when it computed the section offsets.
        // - The lifetime `'a` of the transient slices is tied to the input buffer.
        // - We only support little-endian platforms where the in-memory layout
        //   matches the serialized LE format.
        let nodes: &'a [Node] =
            unsafe { std::slice::from_raw_parts(nodes_ptr as *const Node, header.nodes_count) };
        let child_offsets: &'a [u32] = unsafe {
            std::slice::from_raw_parts(child_offsets_ptr as *const u32, child_offsets_count)
        };
        let children_list: &'a [u32] = unsafe {
            std::slice::from_raw_parts(children_list_ptr as *const u32, header.children_count)
        };

        validate_cheap(nodes, child_offsets, children_list)?;

        // code_map is always deserialized to heap
        let (code_map, _) = CodeMapper::from_bytes(
            &bytes[header.code_map_offset()..header.code_map_offset() + header.code_map_len],
        )
        .ok_or(TrieError::TruncatedData)?;

        Ok(Self {
            nodes_ptr: nodes.as_ptr(),
            nodes_len: nodes.len(),
            child_offsets_ptr: child_offsets.as_ptr(),
            child_offsets_len: child_offsets.len(),
            children_list_ptr: children_list.as_ptr(),
            children_list_len: children_list.len(),
            code_map,
            _marker: PhantomData,
        })
    }

    /// Materialise a `TrieView` whose slice lifetimes are tied to `&self`.
    #[inline]
    pub(crate) fn view(&self) -> TrieView<'_, L> {
        // SAFETY: the three raw pointers were produced during construction
        // from validated slices in `from_bytes`. Two cases to consider for
        // how long those pointers remain valid, both of which bound
        // validity to at least the returned `'_` lifetime:
        //
        // - Normal borrow case (`DoubleArrayRef::from_bytes(bytes: &'a [u8])`):
        //   the slices borrow into `bytes`, and the `PhantomData<&'a [u8]>`
        //   marker prevents this `DoubleArrayRef` from outliving that borrow.
        // - Self-referential case (`DoubleArrayBacked`, which stores a
        //   `DoubleArrayRef<'static, L>` beside the owned backing):
        //   the `'static` marker is synthesised and provides no real
        //   protection, but soundness follows from ownership — the
        //   backing is owned by the outer struct and outlives every
        //   `&self` borrow of the wrapper (and therefore this view).
        //
        // In both cases the reconstructed slices are valid for `'_`.
        // Alignment, bounds, and layout were all checked in `from_bytes`.
        unsafe {
            TrieView {
                nodes: std::slice::from_raw_parts(self.nodes_ptr, self.nodes_len),
                child_offsets: std::slice::from_raw_parts(
                    self.child_offsets_ptr,
                    self.child_offsets_len,
                ),
                children_list: std::slice::from_raw_parts(
                    self.children_list_ptr,
                    self.children_list_len,
                ),
                code_map: &self.code_map,
                _phantom: PhantomData,
            }
        }
    }

    /// Converts this zero-copy reference to an owned [`DoubleArray`].
    pub fn to_owned(&self) -> DoubleArray<L> {
        let view = self.view();
        DoubleArray::new(
            view.nodes.to_vec(),
            view.child_offsets.to_vec(),
            view.children_list.to_vec(),
            self.code_map.clone(),
        )
    }
}

impl<'a, L: Label> std::fmt::Debug for DoubleArrayRef<'a, L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Avoid dumping the whole nodes array; show shape instead.
        f.debug_struct("DoubleArrayRef")
            .field("node_slots", &self.nodes_len)
            .field("children", &self.children_list_len)
            .finish()
    }
}

impl<'a, L: Label> TrieSearch<L> for DoubleArrayRef<'a, L> {
    #[inline]
    fn node_slot_count(&self) -> usize {
        self.nodes_len
    }

    #[inline]
    fn exact_match(&self, key: &[L]) -> Option<u32> {
        self.view().exact_match(key)
    }

    fn common_prefix_search<'b>(
        &'b self,
        query: &'b [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'b {
        self.view().common_prefix_search(query)
    }

    fn predictive_search<'b>(
        &'b self,
        prefix: &'b [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'b {
        self.view().predictive_search(prefix)
    }

    #[inline]
    fn probe(&self, key: &[L]) -> ProbeResult {
        self.view().probe(key)
    }

    fn validate_strict(&self) -> Result<(), TrieError> {
        let view = self.view();
        crate::serial::validate_strict(view.nodes, view.child_offsets, view.children_list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::AlignedBytes;

    fn build_u8(keys: &[&[u8]]) -> DoubleArray<u8> {
        DoubleArray::build(keys)
    }

    #[test]
    fn exact_match_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b", b"bc"];
        let da = build_u8(&keys);
        let buf = AlignedBytes::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes(buf.as_slice()).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da_ref.exact_match(key), Some(i as u32));
        }
        assert_eq!(da_ref.exact_match(b"xyz"), None);
    }

    #[test]
    fn common_prefix_search_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b"];
        let da = build_u8(&keys);
        let buf = AlignedBytes::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes(buf.as_slice()).unwrap();

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
        let buf = AlignedBytes::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes(buf.as_slice()).unwrap();

        let results: Vec<SearchMatch<u8>> = da_ref.predictive_search(b"a").collect();
        let mut value_ids: Vec<u32> = results.iter().map(|r| r.value_id).collect();
        value_ids.sort();
        assert_eq!(value_ids, vec![0, 1, 2]);
    }

    #[test]
    fn probe_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let buf = AlignedBytes::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes(buf.as_slice()).unwrap();

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
        let buf = AlignedBytes::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<char>::from_bytes(buf.as_slice()).unwrap();

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(da_ref.exact_match(key), Some(i as u32));
        }
    }

    #[test]
    fn to_owned_works() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let buf = AlignedBytes::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes(buf.as_slice()).unwrap();
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
            DoubleArrayRef::<u8>::from_bytes(misaligned_slice),
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
            DoubleArrayRef::<u8>::from_bytes(&bytes),
            Err(TrieError::InvalidVersion(99))
        ));
    }

    #[test]
    fn truncated_data_error() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab"];
        let da = build_u8(&keys);
        let bytes = da.as_bytes();

        // Truncate to less than header
        assert!(matches!(
            DoubleArrayRef::<u8>::from_bytes(&bytes[..10]),
            Err(TrieError::TruncatedData)
        ));

        // Truncate data section
        assert!(matches!(
            DoubleArrayRef::<u8>::from_bytes(&bytes[..24]),
            Err(TrieError::TruncatedData)
        ));
    }

    #[test]
    fn node_slot_count_via_ref() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc"];
        let da = build_u8(&keys);
        let buf = AlignedBytes::new(&da.as_bytes());
        let da_ref = DoubleArrayRef::<u8>::from_bytes(buf.as_slice()).unwrap();
        assert_eq!(da_ref.node_slot_count(), da.node_slot_count());
    }

    #[test]
    fn clone_aliases_same_buffer() {
        // The derived `Clone` shallow-copies the raw pointers; both
        // copies alias into the original byte buffer. Dropping one
        // copy must not invalidate the other.
        let keys: Vec<&[u8]> = vec![b"a", b"ab"];
        let da = build_u8(&keys);
        let buf = AlignedBytes::new(&da.as_bytes());
        let r1 = DoubleArrayRef::<u8>::from_bytes(buf.as_slice()).unwrap();
        let r2 = r1.clone();
        drop(r1);
        assert_eq!(r2.exact_match(b"a"), Some(0));
        assert_eq!(r2.exact_match(b"ab"), Some(1));
    }
}
