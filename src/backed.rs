use std::marker::PhantomData;

use crate::view::TrieView;
use crate::{
    CodeMapper, DoubleArrayRef, Label, Node, PrefixMatch, ProbeResult, SearchMatch, TrieError,
    TrieSearch,
};

/// Marker trait for byte buffers whose data pointer is stable across
/// moves of the buffer itself.
///
/// [`DoubleArrayBacked`] stores a raw pointer derived from
/// `backing.as_ref()` and reuses it for the lifetime of the owner, so
/// the pointer must remain valid after `backing` is moved into the
/// outer struct. Implementors guarantee:
///
/// 1. `<Self as AsRef<[u8]>>::as_ref(&self).as_ptr()` stays at the
///    same address across moves of `self`.
/// 2. The pointed-to bytes do not change after construction (no
///    interior mutability that could race with the stored view).
///
/// # Safety
///
/// Implementing this trait for a type that does not uphold (1) or
/// (2) makes [`DoubleArrayBacked::from_backing`] unsound.
///
/// # Pre-blessed types
///
/// Implementations are provided for every common heap-backed
/// byte-buffer type: `Vec<u8>`, `Box<[u8]>`, `Arc<[u8]>`, `Rc<[u8]>`,
/// and shared byte slices (`&[u8]`). These all store their payload
/// in the heap (or static memory) while holding only a fat-pointer
/// header in the stack slot that moves — so their `as_ref()` pointer
/// is inherently stable.
///
/// # Wrapping external types
///
/// To use a non-blessed type (most notably `memmap2::Mmap`), define
/// a newtype in your crate and implement both `AsRef<[u8]>` and this
/// trait:
///
/// ```no_run
/// # type Mmap = &'static [u8]; // illustrative stand-in
/// use lexime_trie::StableBacking;
///
/// struct OwnedMmap(Mmap);
/// impl AsRef<[u8]> for OwnedMmap {
///     fn as_ref(&self) -> &[u8] { self.0.as_ref() }
/// }
/// // SAFETY: `memmap2::Mmap` holds an OS-owned page mapping behind
/// // a stable handle; moving the handle does not relocate the data.
/// unsafe impl StableBacking for OwnedMmap {}
/// ```
///
/// # What breaks (1) or (2)
///
/// Inline-storage types such as owned byte arrays (`[u8; N]`) and
/// the inline mode of `SmallVec` / `tinyvec` / `heapless::Vec` keep
/// the payload *inside* `Self`. Moving `Self` relocates the payload,
/// invalidating any cached pointer. Do not implement this trait for
/// such types.
pub unsafe trait StableBacking: AsRef<[u8]> {}

// Heap-backed: payload lives on the heap; moving the owner copies
// only the fat-pointer header.
unsafe impl StableBacking for Vec<u8> {}
unsafe impl StableBacking for Box<[u8]> {}
unsafe impl StableBacking for std::sync::Arc<[u8]> {}
unsafe impl StableBacking for std::rc::Rc<[u8]> {}

// Reference-based: the data lives wherever the reference points; the
// reference itself is trivially copy-stable.
unsafe impl StableBacking for &[u8] {}

/// A [`DoubleArrayRef`] bundled together with the byte buffer it borrows from.
///
/// Use this when you want a self-contained, movable trie value that owns
/// its backing storage — without propagating a lifetime parameter through
/// every call site. A typical owned-buffer construction:
///
/// ```
/// use lexime_trie::{DoubleArray, DoubleArrayBacked, TrieSearch};
///
/// let keys: Vec<&[u8]> = vec![b"hello", b"world"];
/// let bytes: Vec<u8> = DoubleArray::<u8>::build(&keys).as_bytes();
/// let trie: DoubleArrayBacked<u8, Vec<u8>> =
///     DoubleArrayBacked::from_backing(bytes)?;
/// assert_eq!(trie.exact_match(b"hello"), Some(0));
/// # Ok::<(), lexime_trie::TrieError>(())
/// ```
///
/// To memory-map a trie file, wrap `memmap2::Mmap` in a local newtype
/// with `AsRef<[u8]>` + `unsafe impl StableBacking` — see the
/// [`StableBacking`] docs for the 4-line pattern. `DoubleArrayBacked`
/// does not itself depend on `memmap2`.
///
/// Internally this is a self-referential structure: the stored view
/// borrows into the stored backing. The public API never leaks the
/// internal `'static` lifetime, and Rust's drop order (fields drop in
/// declaration order — view first, backing second) keeps the borrow
/// valid for the whole lifetime of `self`.
///
/// # Backing requirements
///
/// `B: StableBacking` enforces at the type level that `backing.as_ref()`
/// returns a slice whose data address is stable across moves of `B`.
/// Pre-blessed implementations exist for `Vec<u8>`, `Box<[u8]>`,
/// `Arc<[u8]>`, `Rc<[u8]>`, and shared slices `&[u8]`. To wrap an
/// external type such as `memmap2::Mmap`, see the [`StableBacking`]
/// trait docs for the newtype + `unsafe impl` pattern.
pub struct DoubleArrayBacked<L: Label, B: StableBacking> {
    // Raw pointer + length for each of the three sections. We
    // deliberately do NOT store `&'static [T]` here: under stacked
    // borrows, passing a value that contains shared references (even
    // with a synthesised `'static` lifetime) into `drop` would retag
    // those references as protected, and the subsequent reclamation
    // of `backing`'s storage would then fail the Miri check with
    // "would remove [SharedReadOnly for …] which is strongly
    // protected". Raw pointers carry no aliasing protection, so the
    // drop of `backing` is unobstructed.
    //
    // `(thin ptr, len)` pairs instead of `*const [T]` fat pointers
    // sidestep the `dangerous_implicit_autorefs` lint that fires on
    // `&*self.fat_ptr` — explicit `slice::from_raw_parts(ptr, len)`
    // makes the provenance clear.
    nodes_ptr: *const Node,
    nodes_len: usize,
    child_offsets_ptr: *const u32,
    child_offsets_len: usize,
    children_list_ptr: *const u32,
    children_list_len: usize,
    code_map: CodeMapper,
    backing: B,
    _phantom: PhantomData<L>,
}

// SAFETY: The raw pointer fields point into `backing`'s owned storage.
// For `Send`/`Sync` we require `B` itself to be `Send`/`Sync` — if the
// backing can safely cross a thread boundary (or be shared between
// threads), the pointers into it can too. `CodeMapper` is plain owned
// data (`Vec<u32>` + `u32`), which is always `Send + Sync`. `Label`
// implementors (`u8`, `char`) are `Send + Sync`. Raw pointers lose
// these auto-traits by default, so we re-assert them manually.
unsafe impl<L: Label, B: StableBacking + Send> Send for DoubleArrayBacked<L, B> {}
unsafe impl<L: Label, B: StableBacking + Sync> Sync for DoubleArrayBacked<L, B> {}

impl<L: Label, B: StableBacking> DoubleArrayBacked<L, B> {
    /// Parse `backing` as a v3 trie buffer and bundle the two together.
    ///
    /// Returns the same errors as [`DoubleArrayRef::from_bytes`].
    pub fn from_backing(backing: B) -> Result<Self, TrieError> {
        let bytes: &[u8] = backing.as_ref();
        // SAFETY: `B: StableBacking` guarantees that `bytes.as_ptr()`
        // remains valid after `backing` is moved into `Self` below.
        // The synthesised `'static` lifetime exists only for the
        // duration of this function — long enough to parse through
        // `DoubleArrayRef::from_bytes` — and is discarded into raw
        // pointers before we return.
        let bytes_static: &'static [u8] =
            unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
        let view = DoubleArrayRef::<L>::from_bytes(bytes_static)?;
        Ok(Self {
            nodes_ptr: view.nodes.as_ptr(),
            nodes_len: view.nodes.len(),
            child_offsets_ptr: view.child_offsets.as_ptr(),
            child_offsets_len: view.child_offsets.len(),
            children_list_ptr: view.children_list.as_ptr(),
            children_list_len: view.children_list.len(),
            code_map: view.code_map,
            backing,
            _phantom: PhantomData,
        })
    }

    /// Materialise a `TrieView` whose slice lifetimes are tied to
    /// `&self`. The raw-pointer fields are guaranteed valid for the
    /// whole lifetime of `self` (since `backing` is owned here).
    #[inline]
    fn view(&self) -> TrieView<'_, L> {
        // SAFETY: the three raw pointers were produced from
        // `backing.as_ref()` during construction. `backing` is owned
        // by `self`, `B: StableBacking` guarantees the address is
        // stable across moves, and we are borrowing through `&self`
        // so the resulting references cannot outlive `backing`.
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

    /// Consume this wrapper and return the backing buffer.
    ///
    /// After calling this the trie view is discarded; the backing can
    /// be re-used for another purpose or dropped on its own.
    #[inline]
    pub fn into_backing(self) -> B {
        self.backing
    }
}

impl<L: Label, B: StableBacking + Clone> Clone for DoubleArrayBacked<L, B> {
    fn clone(&self) -> Self {
        // A `#[derive(Clone)]` would copy the raw pointers alongside a
        // *fresh* cloned backing at a different address — the clone's
        // pointers would dangle into the original's storage. Re-parse
        // from the cloned backing so the new pointers target the new
        // buffer.
        //
        // `expect` cannot fire in practice: `self.backing` was already
        // parsed successfully during construction, and `B: Clone` is
        // expected to produce an identical byte sequence.
        Self::from_backing(self.backing.clone())
            .expect("cloning a validated backing should reproduce a valid view")
    }
}

impl<L: Label, B: StableBacking> std::fmt::Debug for DoubleArrayBacked<L, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoubleArrayBacked")
            .field("node_slots", &self.nodes_len)
            .field("children", &self.children_list_len)
            .field("backing_len", &self.backing.as_ref().len())
            .finish()
    }
}

impl<L: Label, B: StableBacking> TrieSearch<L> for DoubleArrayBacked<L, B> {
    #[inline]
    fn node_slot_count(&self) -> usize {
        self.nodes_len
    }

    #[inline]
    fn exact_match(&self, key: &[L]) -> Option<u32> {
        self.view().exact_match(key)
    }

    fn common_prefix_search<'a>(
        &'a self,
        query: &'a [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'a {
        self.view().common_prefix_search(query)
    }

    fn predictive_search<'a>(
        &'a self,
        prefix: &'a [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'a {
        self.view().predictive_search(prefix)
    }

    #[inline]
    fn probe(&self, key: &[L]) -> ProbeResult {
        self.view().probe(key)
    }

    #[inline]
    fn validate_strict(&self) -> Result<(), TrieError> {
        let view = self.view();
        crate::serial::validate_strict(view.nodes, view.child_offsets, view.children_list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::AlignedBytes;
    use crate::DoubleArray;

    #[test]
    fn backed_exact_match() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b"];
        let da = DoubleArray::<u8>::build(&keys);
        let backing = AlignedBytes::new(&da.as_bytes());
        let trie: DoubleArrayBacked<u8, _> = DoubleArrayBacked::from_backing(backing).unwrap();

        for (i, k) in keys.iter().enumerate() {
            assert_eq!(trie.exact_match(k), Some(i as u32));
        }
        assert_eq!(trie.exact_match(b"xyz"), None);
    }

    #[test]
    fn backed_predictive_search() {
        let keys: Vec<&[u8]> = vec![b"a", b"ab", b"abc", b"b"];
        let da = DoubleArray::<u8>::build(&keys);
        let backing = AlignedBytes::new(&da.as_bytes());
        let trie: DoubleArrayBacked<u8, _> = DoubleArrayBacked::from_backing(backing).unwrap();

        let count = trie.predictive_search(b"a").count();
        assert_eq!(count, 3);
    }

    #[test]
    fn backed_survives_being_moved() {
        // Construct, then immediately move. If the internal 'static
        // lifetime were genuinely dangling after the move, this would
        // trip Miri.
        let keys: Vec<&[u8]> = vec![b"hello", b"world"];
        let da = DoubleArray::<u8>::build(&keys);
        let backing = AlignedBytes::new(&da.as_bytes());
        let trie = DoubleArrayBacked::<u8, _>::from_backing(backing).unwrap();

        fn take(t: DoubleArrayBacked<u8, AlignedBytes>) -> Option<u32> {
            t.exact_match(b"hello")
        }

        assert_eq!(take(trie), Some(0));
    }

    #[test]
    fn backed_into_backing_returns_original() {
        let keys: Vec<&[u8]> = vec![b"x"];
        let da = DoubleArray::<u8>::build(&keys);
        let backing = AlignedBytes::new(&da.as_bytes());
        let expected_len = backing.len();
        let trie = DoubleArrayBacked::<u8, _>::from_backing(backing).unwrap();
        let recovered = trie.into_backing();
        assert_eq!(recovered.len(), expected_len);
    }

    #[test]
    fn backed_invalid_bytes_returns_error() {
        let trie = DoubleArrayBacked::<u8, _>::from_backing(AlignedBytes::new(b"garbage"));
        assert!(trie.is_err());
    }

    #[test]
    fn backed_auto_traits() {
        // Compile-time assertion that the expected auto-traits are
        // preserved. If a future internal change introduces a `Cell` or
        // other non-`Sync` field, this will fail to compile and force
        // the regression to be addressed deliberately.
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<DoubleArrayBacked<u8, Vec<u8>>>();
        assert_sync::<DoubleArrayBacked<u8, Vec<u8>>>();
        assert_send::<DoubleArrayBacked<u8, std::sync::Arc<[u8]>>>();
        assert_sync::<DoubleArrayBacked<u8, std::sync::Arc<[u8]>>>();
        assert_send::<DoubleArrayRef<'static, u8>>();
        assert_sync::<DoubleArrayRef<'static, u8>>();
    }

    #[test]
    fn backed_survives_thread_handoff() {
        // A `DoubleArrayBacked` is `Send` when its backing is; moving
        // it to a different thread must keep the internal `'static`
        // borrow valid. Miri stress-tests the pointer provenance here.
        let da = DoubleArray::<u8>::build(&[b"hello".as_slice(), b"world"]);
        let backing = AlignedBytes::new(&da.as_bytes());
        let trie = DoubleArrayBacked::<u8, _>::from_backing(backing).unwrap();
        let result = std::thread::spawn(move || trie.exact_match(b"hello"))
            .join()
            .unwrap();
        assert_eq!(result, Some(0));
    }

    #[test]
    fn backed_clone_reparses_from_cloned_backing() {
        // The hand-written `Clone` re-parses the cloned backing rather
        // than copying `view`'s stale pointers. Verify the clone stands
        // on its own after the original is dropped.
        let keys: Vec<&[u8]> = vec![b"abc", b"xyz"];
        let da = DoubleArray::<u8>::build(&keys);
        let original =
            DoubleArrayBacked::<u8, _>::from_backing(AlignedBytes::new(&da.as_bytes())).unwrap();
        let cloned = original.clone();
        drop(original); // The clone's view must borrow into its own backing.
        assert_eq!(cloned.exact_match(b"abc"), Some(0));
        assert_eq!(cloned.exact_match(b"xyz"), Some(1));
    }
}
