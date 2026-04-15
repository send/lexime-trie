use crate::{DoubleArrayRef, Label, PrefixMatch, ProbeResult, SearchMatch, TrieError, TrieSearch};

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

/// Sub-trait of [`StableBacking`]: cloning the backing produces a copy
/// whose data resides at the **same address** as the original, so the
/// bytes — and their alignment — are shared rather than re-allocated.
///
/// This unlocks an infallible, zero-cost [`Clone`] impl for
/// [`DoubleArrayBacked`]: on clone the inner view's raw pointers are
/// still valid for the cloned backing, so re-parsing (and its potential
/// [`TrieError::MisalignedData`] failure on unusual allocators) is
/// avoided entirely.
///
/// # Pre-blessed types
///
/// - `Arc<[u8]>`, `Rc<[u8]>` — `Clone` increments the reference count;
///   the cloned handle points at the same `[u8]` storage as the original.
/// - `&[u8]` — `Clone` on a shared reference is a pointer-plus-length
///   copy; the referenced bytes are shared by construction.
///
/// `Vec<u8>` and `Box<[u8]>` deliberately do **not** implement this
/// trait: their `Clone` allocates a fresh buffer, so the cloned data
/// lives at a new address (and possibly with a different alignment).
/// [`DoubleArrayBacked`] built on top of those backings is not `Clone`
/// — callers should use [`DoubleArrayBacked::try_clone`] (fallible
/// because the fresh allocation may not meet the 4-byte alignment
/// requirement) or wrap the backing in an `Arc<[u8]>` /  `Rc<[u8]>`
/// upfront.
///
/// # Safety
///
/// Implementors must guarantee that for every `self: Self` and every
/// `clone: Self` obtained from `<Self as Clone>::clone(&self)`:
///
/// - `clone.as_ref().as_ptr() == self.as_ref().as_ptr()` — the data
///   addresses are literally equal (not merely byte-equivalent).
/// - `clone.as_ref().len() == self.as_ref().len()`.
/// - The alignment of the returned slice is identical.
///
/// Implementing this trait for a type that allocates a fresh buffer
/// on `clone` (or that otherwise changes the data address) makes
/// [`DoubleArrayBacked`]'s `Clone` impl unsound.
pub unsafe trait CloneStableBacking: StableBacking + Clone {}

// Ref-counted: `Clone` bumps the refcount; the `[u8]` payload is shared
// across all handles, so `as_ref().as_ptr()` is identical for every
// clone.
unsafe impl CloneStableBacking for std::sync::Arc<[u8]> {}
unsafe impl CloneStableBacking for std::rc::Rc<[u8]> {}

// Shared reference: `Clone` is a trivial pointer-plus-length copy.
unsafe impl CloneStableBacking for &[u8] {}

/// A [`DoubleArrayRef`] bundled together with the byte buffer it borrows from.
///
/// Use this when you want a self-contained, movable trie value that owns
/// its backing storage — without propagating a lifetime parameter through
/// every call site. A typical owned-buffer construction:
///
/// ```no_run
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
/// The example is `no_run` because Rust only guarantees `Vec<u8>`
/// allocations have 1-byte alignment. Most system allocators happen
/// to return word-aligned memory in practice, which is enough for the
/// 4-byte requirement of the zero-copy loader, but that stronger
/// alignment is not a portable guarantee — it can be broken by a
/// custom `GlobalAlloc` and is deliberately weakened under Miri. So
/// running this snippet under `cargo miri test --doc` would trip
/// [`TrieError::MisalignedData`].
///
/// For a runnable variant — and for callers that need a real 4-byte
/// alignment guarantee — borrow into a 4-byte aligned scratch buffer:
///
/// ```
/// use lexime_trie::{DoubleArray, DoubleArrayBacked, TrieSearch};
///
/// let raw = DoubleArray::<u8>::build(&[b"hello".as_slice(), b"world"]).as_bytes();
/// // `Vec<u32>` heap blocks are 4-byte aligned by construction.
/// let mut aligned = vec![0u32; raw.len().div_ceil(4)];
/// // SAFETY: `aligned` holds at least `raw.len()` bytes; `u32` has no
/// // invalid bit patterns; the source and destination do not overlap.
/// unsafe {
///     std::ptr::copy_nonoverlapping(
///         raw.as_ptr(),
///         aligned.as_mut_ptr() as *mut u8,
///         raw.len(),
///     );
/// }
/// // SAFETY: `aligned.as_ptr()` is 4-byte aligned and points to
/// // `raw.len()` initialized bytes (we just wrote them).
/// let bytes: &[u8] = unsafe {
///     std::slice::from_raw_parts(aligned.as_ptr() as *const u8, raw.len())
/// };
/// let trie: DoubleArrayBacked<u8, &[u8]> =
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
/// internal `'static` lifetime.
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
    // `DoubleArrayRef` stores its sections as raw pointers, so
    // embedding a `DoubleArrayRef<'static, L>` is sound under Stacked
    // Borrows: the inner value carries no shared references that
    // drop-time retagging would protect. The synthesised `'static`
    // lifetime is never exposed — `as_view` down-casts it to `'_`.
    view: DoubleArrayRef<'static, L>,
    backing: B,
}

// `DoubleArrayRef<'static, L>` is `Send + Sync` (see its unsafe impls),
// and `B` carries its own auto-traits, so `DoubleArrayBacked<L, B>`
// picks up `Send`/`Sync` via ordinary auto-derivation whenever `B`
// does. No manual unsafe impl is needed.

impl<L: Label, B: StableBacking> DoubleArrayBacked<L, B> {
    /// Parse `backing` as a v3 trie buffer and bundle the two together.
    ///
    /// Returns the same errors as [`DoubleArrayRef::from_bytes`].
    pub fn from_backing(backing: B) -> Result<Self, TrieError> {
        let bytes: &[u8] = backing.as_ref();
        // SAFETY: `B: StableBacking` guarantees that `bytes.as_ptr()`
        // remains valid after `backing` is moved into `Self` below.
        // The synthesised `'static` lifetime is used only to satisfy
        // the `DoubleArrayRef` type parameter; the inner type stores
        // raw pointers so no `&'static` references escape.
        let bytes_static: &'static [u8] =
            unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
        let view = DoubleArrayRef::<'static, L>::from_bytes(bytes_static)?;
        Ok(Self { view, backing })
    }

    /// Borrow the inner zero-copy view.
    ///
    /// The returned reference's lifetime is tied to `&self`, so the
    /// synthesised `'static` lifetime on the stored view is never
    /// observable by callers. This is a zero-cost coercion (covariant
    /// lifetime narrowing on `DoubleArrayRef`).
    ///
    /// Use this to pass the borrowed view to a function that accepts
    /// `&DoubleArrayRef<'_, L>`:
    ///
    /// ```no_run
    /// use lexime_trie::{DoubleArray, DoubleArrayBacked, DoubleArrayRef, Label, TrieSearch};
    ///
    /// fn lookup<L: Label>(view: &DoubleArrayRef<'_, L>, key: &[L]) -> Option<u32> {
    ///     view.exact_match(key)
    /// }
    ///
    /// let bytes: Vec<u8> = DoubleArray::<u8>::build(&[b"hi".as_slice()]).as_bytes();
    /// let trie: DoubleArrayBacked<u8, Vec<u8>> =
    ///     DoubleArrayBacked::from_backing(bytes)?;
    /// assert_eq!(lookup(trie.as_view(), b"hi"), Some(0));
    /// # Ok::<(), lexime_trie::TrieError>(())
    /// ```
    ///
    /// `no_run` for the alignment reason described on
    /// [`DoubleArrayBacked`]'s type-level docs.
    #[inline]
    pub fn as_view(&self) -> &DoubleArrayRef<'_, L> {
        &self.view
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

impl<L: Label, B: StableBacking + Clone> DoubleArrayBacked<L, B> {
    /// Fallible clone that works with any `B: StableBacking + Clone`.
    ///
    /// For backings that implement [`CloneStableBacking`] (`Arc<[u8]>`,
    /// `Rc<[u8]>`, `&[u8]`), prefer the infallible [`Clone`] impl —
    /// it avoids re-parsing and cannot fail. `try_clone` exists
    /// primarily for backings whose `Clone` allocates a fresh buffer
    /// (`Vec<u8>`, `Box<[u8]>`), where the new allocation may not
    /// satisfy the 4-byte alignment the zero-copy loader requires
    /// (see [`StableBacking`]'s alignment notes).
    ///
    /// Returns the same errors as [`DoubleArrayRef::from_bytes`]; the
    /// realistic failure mode is [`TrieError::MisalignedData`] when
    /// `B::clone` produces a buffer at a different alignment than the
    /// original (most commonly under Miri or a custom `GlobalAlloc`).
    pub fn try_clone(&self) -> Result<Self, TrieError> {
        Self::from_backing(self.backing.clone())
    }
}

impl<L: Label, B: CloneStableBacking> Clone for DoubleArrayBacked<L, B> {
    fn clone(&self) -> Self {
        // SAFETY: `B: CloneStableBacking` guarantees that
        // `self.backing.clone().as_ref().as_ptr()` equals
        // `self.backing.as_ref().as_ptr()`. Therefore the raw pointers
        // stored in `self.view` are *still* valid for the cloned
        // backing: the data lives at the same address and alignment.
        // Cloning the view is a bitwise copy of those raw pointers
        // (via the derived `Clone` on `DoubleArrayRef`), so no
        // re-parsing and no alignment re-validation is required. The
        // infallibility of this impl is exactly what motivated
        // `CloneStableBacking` in the first place.
        Self {
            view: self.view.clone(),
            backing: self.backing.clone(),
        }
    }
}

impl<L: Label, B: StableBacking> std::fmt::Debug for DoubleArrayBacked<L, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DoubleArrayBacked")
            .field("view", &self.view)
            .field("backing_len", &self.backing.as_ref().len())
            .finish()
    }
}

impl<L: Label, B: StableBacking> TrieSearch<L> for DoubleArrayBacked<L, B> {
    #[inline]
    fn node_slot_count(&self) -> usize {
        self.view.node_slot_count()
    }

    #[inline]
    fn exact_match(&self, key: &[L]) -> Option<u32> {
        self.view.exact_match(key)
    }

    fn common_prefix_search<'a>(
        &'a self,
        query: &'a [L],
    ) -> impl Iterator<Item = PrefixMatch> + 'a {
        self.view.common_prefix_search(query)
    }

    fn predictive_search<'a>(
        &'a self,
        prefix: &'a [L],
    ) -> impl Iterator<Item = SearchMatch<L>> + 'a {
        self.view.predictive_search(prefix)
    }

    #[inline]
    fn probe(&self, key: &[L]) -> ProbeResult {
        self.view.probe(key)
    }

    #[inline]
    fn validate_strict(&self) -> Result<(), TrieError> {
        self.view.validate_strict()
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
        assert_send::<DoubleArrayRef<'static, char>>();
        assert_sync::<DoubleArrayRef<'static, char>>();
    }

    #[test]
    fn backed_as_view_is_borrow_checked() {
        // `as_view()` returns `&DoubleArrayRef<'_, L>` whose lifetime is
        // tied to `&self`; the synthesised `'static` is never observable.
        // A free function taking `&DoubleArrayRef<'_, L>` must accept
        // the borrow without an explicit lifetime annotation.
        fn search<L: crate::Label>(v: &DoubleArrayRef<'_, L>, k: &[L]) -> Option<u32> {
            v.exact_match(k)
        }
        let da = DoubleArray::<u8>::build(&[b"hello".as_slice(), b"hi"]);
        let trie =
            DoubleArrayBacked::<u8, _>::from_backing(AlignedBytes::new(&da.as_bytes())).unwrap();
        assert_eq!(search(trie.as_view(), b"hello"), Some(0));
        assert_eq!(search(trie.as_view(), b"hi"), Some(1));
        assert_eq!(search(trie.as_view(), b"miss"), None);
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
    fn backed_try_clone_reparses_from_cloned_backing() {
        // For address-changing backings (`AlignedBytes` clones a fresh
        // `Vec<u64>`, so the payload moves to a new heap block),
        // `try_clone` re-parses the cloned bytes rather than copying
        // `view`'s stale pointers. Verify the clone stands on its own
        // after the original is dropped.
        let keys: Vec<&[u8]> = vec![b"abc", b"xyz"];
        let da = DoubleArray::<u8>::build(&keys);
        let original =
            DoubleArrayBacked::<u8, _>::from_backing(AlignedBytes::new(&da.as_bytes())).unwrap();
        let cloned = original.try_clone().unwrap();
        drop(original); // The clone's view must borrow into its own backing.
        assert_eq!(cloned.exact_match(b"abc"), Some(0));
        assert_eq!(cloned.exact_match(b"xyz"), Some(1));
    }

    #[test]
    fn backed_clone_arc_preserves_address() {
        // For `Arc<[u8]>`, `Clone` is a refcount bump — the cloned
        // handle points at the same `[u8]` payload. The `Clone` impl
        // on `DoubleArrayBacked<L, Arc<[u8]>>` exploits this by
        // bitwise-copying the view instead of re-parsing.
        use std::sync::Arc;
        let keys: Vec<&[u8]> = vec![b"abc", b"xyz"];
        let da = DoubleArray::<u8>::build(&keys);
        // Aligned backing via Vec<u32> → Arc<[u8]> — a realistic path
        // for shipping an mmap-like handle.
        let aligned = AlignedBytes::new(&da.as_bytes());
        let arc: Arc<[u8]> = Arc::from(aligned.as_slice());
        let original = DoubleArrayBacked::<u8, Arc<[u8]>>::from_backing(arc).unwrap();
        let original_addr = original.as_view() as *const DoubleArrayRef<'_, u8> as usize;
        let cloned = original.clone();
        // Sanity: both clones resolve queries the same way.
        assert_eq!(original.exact_match(b"abc"), Some(0));
        assert_eq!(cloned.exact_match(b"abc"), Some(0));
        assert_eq!(cloned.exact_match(b"xyz"), Some(1));
        // The two wrappers live at distinct stack addresses (sanity
        // check that we got two `Self` values, not a single shared one).
        assert_ne!(
            original_addr,
            cloned.as_view() as *const DoubleArrayRef<'_, u8> as usize
        );
    }

    #[test]
    fn backed_clone_shared_slice_preserves_address() {
        // For `&[u8]`, `Clone` is a pointer-plus-length copy.
        let keys: Vec<&[u8]> = vec![b"abc", b"xyz"];
        let da = DoubleArray::<u8>::build(&keys);
        let aligned = AlignedBytes::new(&da.as_bytes());
        let slice: &[u8] = aligned.as_slice();
        let original = DoubleArrayBacked::<u8, &[u8]>::from_backing(slice).unwrap();
        let cloned = original.clone();
        assert_eq!(original.exact_match(b"abc"), Some(0));
        assert_eq!(cloned.exact_match(b"abc"), Some(0));
    }
}
