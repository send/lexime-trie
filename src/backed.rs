use crate::{DoubleArrayRef, Label, PrefixMatch, ProbeResult, SearchMatch, TrieError, TrieSearch};

/// A [`DoubleArrayRef`] bundled together with the byte buffer it borrows from.
///
/// Use this when you want a self-contained, movable trie value that owns
/// its backing storage — without propagating a lifetime parameter through
/// every call site. The typical case is memory-mapping a trie file:
///
/// ```no_run
/// use std::fs::File;
/// # // `memmap2` is illustrative; not a dependency of this crate.
/// # type Mmap = &'static [u8];
/// # fn map(_: &File) -> Result<Mmap, std::io::Error> { unimplemented!() }
/// use lexime_trie::{DoubleArrayBacked, TrieSearch};
///
/// let file = File::open("trie.bin")?;
/// let mmap = map(&file)?;
/// let trie: DoubleArrayBacked<u8, Mmap> = DoubleArrayBacked::new(mmap)?;
/// assert!(trie.exact_match(b"hello").is_some());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Internally this is a self-referential structure: the stored view
/// borrows into the stored backing. The public API never leaks the
/// internal `'static` lifetime, and Rust's drop order (fields drop in
/// declaration order — view first, backing second) keeps the borrow
/// valid for the whole lifetime of `self`.
///
/// # Backing requirements
///
/// `B: AsRef<[u8]>` must return a slice whose *data* address is stable
/// across moves of `B`. This is true for every heap-backed type
/// (`Vec<u8>`, `Box<[u8]>`, `Arc<[u8]>`), shared byte references
/// (`&'static [u8]`), and OS-owned mappings like `memmap2::Mmap`. It is
/// **not** true for inline-stored types such as owned arrays `[u8; N]`,
/// whose data lives alongside `B` itself and would be invalidated by
/// moving `B` into this struct.
pub struct DoubleArrayBacked<L: Label, B: AsRef<[u8]>> {
    // DROP ORDER (load-bearing): `view` borrows into `backing`, so
    // `view` must be dropped before `backing`. Rust drops fields in
    // declaration order, so `view` comes first.
    view: DoubleArrayRef<'static, L>,
    backing: B,
}

impl<L: Label, B: AsRef<[u8]>> DoubleArrayBacked<L, B> {
    /// Parse `backing` as a v3 trie buffer and bundle the two together.
    ///
    /// Returns the same errors as [`DoubleArrayRef::from_bytes_ref`].
    pub fn new(backing: B) -> Result<Self, TrieError> {
        // Borrow the backing to parse it. The resulting slice address
        // is tied to `backing`'s current location; once we move
        // `backing` into `Self` below, that location must remain stable
        // (see the type-level docs on backing requirements).
        let bytes: &[u8] = backing.as_ref();
        // SAFETY: We synthesise a `'static` lifetime to store alongside
        // `backing` in the same struct. The lifetime never leaks to
        // callers — `TrieSearch` methods only expose borrows tied to
        // `&self` — and Rust's declaration-order drop guarantees that
        // `view` drops before `backing`, so the pointer is never
        // dereferenced after `backing` goes away.
        let bytes_static: &'static [u8] =
            unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) };
        let view = DoubleArrayRef::from_bytes_ref(bytes_static)?;
        Ok(Self { view, backing })
    }

    /// Borrow the inner zero-copy view. The returned reference is
    /// lifetime-tied to `&self`, so the internal `'static` lifetime
    /// placeholder never leaks to callers — the compiler shortens it
    /// via the usual subtyping on shared references.
    #[inline]
    pub fn as_view(&self) -> &DoubleArrayRef<'_, L> {
        &self.view
    }

    /// Consume this wrapper and return the backing buffer.
    ///
    /// After calling this the trie view is dropped; the backing can be
    /// re-used for another purpose or dropped on its own.
    #[inline]
    pub fn into_backing(self) -> B {
        self.backing
    }
}

impl<L: Label, B: AsRef<[u8]>> TrieSearch<L> for DoubleArrayBacked<L, B> {
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
        let trie: DoubleArrayBacked<u8, _> = DoubleArrayBacked::new(backing).unwrap();

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
        let trie: DoubleArrayBacked<u8, _> = DoubleArrayBacked::new(backing).unwrap();

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
        let trie = DoubleArrayBacked::<u8, _>::new(backing).unwrap();

        fn take(t: DoubleArrayBacked<u8, AlignedBytes>) -> Option<u32> {
            t.exact_match(b"hello")
        }

        assert_eq!(take(trie), Some(0));
    }

    #[test]
    fn backed_as_view_exposes_inner() {
        let keys: Vec<&[u8]> = vec![b"a", b"b"];
        let da = DoubleArray::<u8>::build(&keys);
        let backing = AlignedBytes::new(&da.as_bytes());
        let trie = DoubleArrayBacked::<u8, _>::new(backing).unwrap();
        let inner: &DoubleArrayRef<'_, u8> = trie.as_view();
        assert_eq!(inner.node_slot_count(), trie.node_slot_count());
    }

    #[test]
    fn backed_into_backing_returns_original() {
        let keys: Vec<&[u8]> = vec![b"x"];
        let da = DoubleArray::<u8>::build(&keys);
        let backing = AlignedBytes::new(&da.as_bytes());
        let expected_len = backing.len();
        let trie = DoubleArrayBacked::<u8, _>::new(backing).unwrap();
        let recovered = trie.into_backing();
        assert_eq!(recovered.len(), expected_len);
    }

    #[test]
    fn backed_invalid_bytes_returns_error() {
        let trie = DoubleArrayBacked::<u8, _>::new(AlignedBytes::new(b"garbage"));
        assert!(trie.is_err());
    }
}
