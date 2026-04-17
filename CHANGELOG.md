# Changelog

All notable changes to this crate are documented in this file.

## 0.5.0 — 2026-04-17

Breaking release that refines `TrieError` so load failures carry
actionable diagnostics and so future variants can be added
non-breakingly.

### Breaking

- **`TrieError` is now `#[non_exhaustive]`.** Callers pattern-matching
  on its variants must include a catch-all arm (`_ => ...`). This
  allows future diagnostic refinements without another breaking
  release.
- **`TrieError::InvalidVersion` now carries a `u8` payload.** The
  value is the actual version byte observed in the buffer, preserved
  so wrappers can forward it rather than hardcoding a placeholder:
  ```rust
  // Before
  TrieError::InvalidVersion
  // After
  TrieError::InvalidVersion(99)
  ```
  This fixes the information loss path in downstream wrappers like
  lexime's `DictError::UnsupportedVersion(_)`.
- **`TrieError::InvalidStructure` is a new variant.** Previously the
  loader conflated two distinct failure modes into `TruncatedData`:
  - actual buffer truncation (header/section/code-map cut off), and
  - structural corruption on a long-enough buffer (`nodes_count < 2`,
    CSR length/endpoint mismatch, non-monotonic `child_offsets`,
    declared counts that overflow `usize`).
  These are now split: `TruncatedData` is emitted only when the
  buffer ends early; `InvalidStructure` is emitted when the shape is
  wrong. Remediation differs — truncation suggests a partial
  download, structural corruption suggests regenerating the trie.

### Migration

Callers that matched on `TrieError` exhaustively should:

1. Add a wildcard arm for `#[non_exhaustive]`.
2. Replace `TrieError::InvalidVersion` with `TrieError::InvalidVersion(_)`
   (or bind the version byte if reporting it).
3. If distinguishing corruption from truncation is useful, add a
   `TrieError::InvalidStructure` arm; otherwise a fallback arm
   (or widening to `TruncatedData | InvalidStructure`) suffices.

The change is purely additive on the data-layout side — no v3 binary
format changes. Existing `.lxtr` buffers load unchanged.

### Internal

- `predictive_search`'s iterator now carries a hard pop cap equal to
  `nodes.len()` so it always terminates, even when a corrupted
  `children_list` + `child_offsets` combination forms a DFS cycle.
  A valid trie pops at most `nodes.len()` times, so the cap never
  constrains well-formed traversals. `validate_strict` still catches
  the underlying corruption up-front when run; this cap exists for
  callers that skip validation.
- `TrieView::probe` reuses the `get_unchecked` + explicit
  bounds-check pattern from the other hot paths.

## 0.4.0 — 2026-04-15

Breaking release that tightens the public surface and adds an owning
wrapper for the zero-copy view.

### Breaking

- **`TrieSearch` trait.** The five query methods previously defined
  inherently on `DoubleArray` and `DoubleArrayRef` —
  `exact_match`, `common_prefix_search`, `predictive_search`,
  `probe`, `node_slot_count` — are now on the `TrieSearch<L>`
  trait. Bring it into scope at call sites:
  ```rust
  use lexime_trie::{DoubleArray, TrieSearch};
  ```
  Generic code can now abstract over "owned" (`DoubleArray<L>`),
  "borrowed" (`DoubleArrayRef<'_, L>`), and "owned-mmap"
  (`DoubleArrayBacked<L, B>`) representations.
- **`num_nodes()` → `node_slot_count()`.** The old name suggested
  "number of live trie nodes", but the value returned is
  `self.nodes.len()` — the slot count including sentinel and any
  remaining free slots. The renamed method has docs explaining
  exactly what it counts.
- **`CodeMapper::reverse` returns `Option<L>`.** Previously it
  returned `Option<u32>` and callers had to follow up with
  `L::try_from`. The generic parameter is now on the method itself
  and the conversion is folded in. (`CodeMapper` is internal in
  0.4 — see below — but this also simplifies the trait
  implementations inside the crate.)
- **`CodeMapper` and `Node` are no longer public.** They were
  re-exported from the crate root but carried internal layout
  details. The 0.4 surface is limited to `DoubleArray`,
  `DoubleArrayRef`, `DoubleArrayBacked`, `TrieSearch`, `Label`,
  `PrefixMatch`, `SearchMatch`, `ProbeResult`, and `TrieError`.
- **`Label::ALPHABET_SIZE` removed.** The constant was unused in
  the crate and in lexime (its only known consumer). External
  implementors of `Label` no longer need to define it.
- **`CodeMapper::alphabet_size()` and `CodeMapper::as_bytes()`
  removed.** Dead outside tests; `CodeMapper` itself is now
  internal, so this is only observable through its deletion from
  the re-export list.
- **`Node::raw_base`, `Node::raw_check`, `Node::from_raw`
  removed.** These existed for external serialisation callers
  that never materialised.
- **`DoubleArrayRef::from_bytes_ref` → `DoubleArrayRef::from_bytes`.**
  The `_ref` suffix was redundant with the type's own name;
  renaming puts `DoubleArray::from_bytes` and
  `DoubleArrayRef::from_bytes` side-by-side.
- **`TrieSearch` is not dyn-compatible.** The trait uses
  return-position `impl Trait` for iterators, so `&dyn
  TrieSearch<L>` does not compile. Use the trait as a generic
  bound (`fn foo<T: TrieSearch<u8>>`) instead.

### Added

- **`DoubleArrayBacked<L, B: StableBacking>`.** A `DoubleArrayRef`
  bundled with the byte buffer it borrows from. Removes the
  self-referential-struct friction that forced consumers like
  lexime to use `mem::transmute<DoubleArrayRef<'_, u8>,
  DoubleArrayRef<'static, u8>>` to keep an mmap and its view in
  the same owning struct. Constructor is `from_backing`.
  Implements `TrieSearch<L>`. Infallibly `Clone` when
  `B: CloneStableBacking` (bitwise-copies the view, refcount-bumps
  the backing); callers with a `Vec<u8>` / `Box<[u8]>` backing use
  the fallible `try_clone()` method instead. Provides
  `into_backing()` to recover the original buffer.
- **`StableBacking` marker trait.** `unsafe trait` that promises
  the implementing type's `AsRef<[u8]>::as_ref` pointer stays
  valid across moves of `Self` and the bytes do not change
  afterwards. `DoubleArrayBacked<L, B>` now bounds `B` by
  `StableBacking` so the invariant that made the previous
  `AsRef<[u8]>` contract sound-on-paper is enforced at the
  type level. Pre-blessed implementations: `Vec<u8>`,
  `Box<[u8]>`, `Arc<[u8]>`, `Rc<[u8]>`, `&[u8]`. To use
  `memmap2::Mmap` or another external type, wrap it in a local
  newtype with `AsRef<[u8]>` + `unsafe impl StableBacking` (the
  trait docs show the 4-line pattern). Inline-storage types
  such as `[u8; N]`, `SmallVec` inline mode, and
  `heapless::Vec` explicitly violate the contract and must not
  implement the trait.
- **`TrieSearch::validate_strict()`.** O(N) structural
  validation: all cheap checks plus monotonicity of
  `child_offsets`. Run this after loading from an untrusted
  source to reject malformed buffers before any query. Not
  invoked by either `from_bytes` constructor.
- **`DoubleArrayRef<'a, L>` now derives `Clone`** and implements
  a compact `Debug`. `DoubleArrayBacked<L, B>` also implements
  both, with `Clone` gated by `CloneStableBacking` (see below).
- **`CloneStableBacking` sub-trait.** `unsafe trait` extending
  `StableBacking + Clone` with the additional guarantee that
  `<B as Clone>::clone(&self).as_ref().as_ptr()` equals
  `self.as_ref().as_ptr()` — i.e. cloning the backing does not
  reallocate, it shares or refcounts the existing storage.
  `DoubleArrayBacked<L, B>` implements `Clone` infallibly and
  without re-parsing when `B: CloneStableBacking`. Pre-blessed
  impls: `Arc<[u8]>`, `Rc<[u8]>`, `&[u8]`. For backings whose
  `Clone` allocates a fresh buffer (`Vec<u8>`, `Box<[u8]>`), use
  `DoubleArrayBacked::try_clone()` — which re-parses the new
  allocation and returns `Err(TrieError::MisalignedData)` if the
  fresh buffer fails to meet the 4-byte alignment requirement.
- **`DoubleArrayBacked::try_clone(&self) -> Result<Self, TrieError>`.**
  Fallible clone available for any `B: StableBacking + Clone`.
  Re-parses from `self.backing.clone()` and is the only clone
  path offered for `Vec<u8>` / `Box<[u8]>` backings; the realistic
  failure mode is a `MisalignedData` when the cloned allocation
  has different alignment than the original (primarily under Miri
  or a custom `GlobalAlloc`).
- **`DoubleArrayBacked::as_view() -> &DoubleArrayRef<'_, L>`.**
  Borrow the inner zero-copy view without consuming the wrapper.
  The returned reference's lifetime is tied to `&self`, so the
  internally-synthesised `'static` lifetime is never observable.

### Internal

- **`DoubleArrayRef` now stores raw pointers instead of `&'a [T]`
  slices.** Public API and type layout size are unchanged. The
  change lets `DoubleArrayBacked` embed a plain
  `DoubleArrayRef<'static, L>` field without tripping Stacked Borrows'
  strong-protection rule on drop.

### Migration (0.3.x → 0.4.0)

1. Add `use lexime_trie::TrieSearch;` at every call site of
   `exact_match`, `common_prefix_search`, `predictive_search`,
   `probe`, `node_slot_count`, or `validate_strict`. The
   compiler's error message suggests the exact import.
2. Rename any `.num_nodes()` to `.node_slot_count()`.
3. Rename `DoubleArrayRef::from_bytes_ref` to
   `DoubleArrayRef::from_bytes`.
4. If you were matching on `lexime_trie::Node` or
   `lexime_trie::CodeMapper`, you can no longer do so; these
   types are internal in 0.4. No replacement is needed for
   typical use — all trie operations are on the owner types.
5. If you were keeping an `mmap` (or `Arc<[u8]>`, `Vec<u8>`,
   etc.) and a `DoubleArrayRef<'static, L>` in the same struct
   via `mem::transmute`, replace that pattern with
   `DoubleArrayBacked::from_backing(backing)`. For `memmap2::Mmap`
   specifically, wrap the mapping in a local newtype that
   implements `AsRef<[u8]>` + `unsafe impl StableBacking`.
   Migration is now mandatory in spirit: while
   `DoubleArrayRef`'s layout size is unchanged, its internal
   field types switched from `&'a [T]` to `(*const T, usize)`
   pairs, so a transmute that happened to satisfy Stacked
   Borrows under 0.3 may behave differently under 0.4 even
   though the byte layout matches.
   Note: if your `open()` function also stores *other*
   `&'static` slices into the same mmap (string pools, index
   tables, etc.), those still need their own bundling solution
   — `DoubleArrayBacked` only covers the trie view. You can
   apply the same self-referential-newtype recipe for them.
6. If you were cloning a `DoubleArrayBacked<L, Vec<u8>>` or
   `DoubleArrayBacked<L, Box<[u8]>>`, replace `.clone()` with
   `.try_clone()?` or `.try_clone().expect(...)`. The infallible
   `Clone` impl is now gated on `B: CloneStableBacking`, which
   `Vec<u8>` / `Box<[u8]>` deliberately do not implement (their
   `Clone` allocates a fresh buffer at a potentially different
   alignment, so re-parsing can fail). To keep the infallible
   `.clone()` ergonomics, wrap the buffer up-front as
   `Arc::<[u8]>::from(buf)` or `Rc::<[u8]>::from(buf)` — both
   are pre-blessed `CloneStableBacking`.

Existing serialised buffers (v3 format, 0.3.0 and 0.3.1) load
unchanged; the binary format itself is unchanged.

## 0.3.1 — 2026-04-15

Non-breaking polish on top of 0.3.0. No public API changes.

### Internal

- `DoubleArray::build` no longer recurses; the trie walk now uses an
  explicit work stack, so very long keys (depth > a few × 10⁴) cannot
  overflow the Rust stack at build time.
- The trailing-trim pass at the end of `build` is now O(1) — the
  build context tracks the highest used node index incrementally
  instead of doing a reverse linear scan over `nodes` at finalisation.
- `CodeMapper::build` falls back to a `HashMap`-based frequency count
  when the dense path's `max_label + 1` would exceed 65 536 entries.
  The persistent forward `table` is still allocated densely (it stays
  on the search hot path), but the transient counting structure no
  longer balloons to ~8 MiB on inputs that include emoji or other
  supplementary-plane code points.

### Safety / robustness

- The four invariants in `serial::as_bytes` (count fields fitting in
  `u32`, `child_offsets.len() == nodes.len() + 1`, code-map size in
  range) are now checked in release builds. They are upheld by the
  builder, but a future bug that violated them would otherwise wrap
  silently via `as u32` and produce a buffer whose zero-copy load
  would build slices over the wrong bounds.
- `CodeMapper::write_to` and `from_bytes` no longer use `unsafe`; the
  serialiser/deserialiser is expressed in terms of `to_le_bytes` /
  `from_le_bytes` and slice operations. `serialized_size` and
  `write_to` size arithmetic now use `checked_*`, matching the style
  of `serial::as_bytes`.

## 0.3.0 — 2026-04-15

### Breaking

- **Binary format v2 is removed.** `DoubleArray::from_bytes` and
  `DoubleArrayRef::from_bytes_ref` now require the LXTR v3 format and
  return `TrieError::InvalidVersion` for older buffers. Persisted v2
  files must be rebuilt from their original key sets.
- `DoubleArray` layout changed: `siblings: Vec<u32>` is replaced with
  `child_offsets: Vec<u32>` (CSR offsets, length N+1) and
  `children_list: Vec<u32>` (flat child indices, length E).
  `DoubleArrayRef` and `TrieView` follow the same layout.
- The root node is now at `nodes[1]`. `nodes[0]` is reserved as an
  invalid sentinel so that `check == 0` has a single unambiguous
  meaning ("unused slot"). Serialized files built with 0.2.x cannot be
  read by 0.3 — see "Binary format" above.
- `CodeMapper::reverse` returns `Option<u32>` instead of `u32`. Out-of-
  range codes previously panicked in release builds (OOB index); they
  now yield `None` so callers can decide. Existing code that reads the
  result with `expect`/`unwrap` will need a trivial adjustment.

### Fixed

- `predictive_search` no longer relies on an implicit invariant between
  child placement order at build time and an `O(alphabet_size)` scan at
  query time. The CSR children list stores the order explicitly, making
  the #21 class of drop-children bugs structurally impossible.
- `CodeMapper::reverse` with an out-of-range code now returns `None`
  instead of panicking in release builds.
- `DoubleArray::build` now explicitly rejects input with more than
  `2^31 - 1` keys, rather than silently truncating `value_id` on cast.
- `validate_cheap` additionally verifies
  `child_offsets.len() == nodes.len() + 1` so corrupt inputs fail at
  load time rather than producing confusing downstream errors.

### Performance (50k hiragana char keys, measured against 0.2.2)

- `predictive_search_2char_prefix`: **−47%** (target win of the v3 work).
- `serial_from_bytes_ref`: roughly unchanged (within noise). `from_bytes_ref`
  remains O(1) after the header parse — no per-node validation.
- `probe_1k`: +7% (slightly slower due to one extra `child_offsets` read
  for `has_children`).
- `build_50k_char`: +36% (count-and-scatter flatten adds overhead to a
  one-time operation).
- `serial_from_bytes`: +46% (the v3 format carries one additional section,
  `children_list`, so there is more data to copy on the owned load path).
- `exact_match` / `common_prefix_search`: unchanged within noise; both
  still touch only `nodes` (8B/node hot path preserved).

### Migration

The v0.3 format is strictly stored data; no runtime API for reading v2
buffers is provided. Rebuild:

```rust
let keys: Vec<&[u8]> = /* your sorted keys */;
let da = lexime_trie::DoubleArray::<u8>::build(&keys);
let bytes_v3 = da.as_bytes();
```

## 0.2.2 — 2026-04-14

- Fix predictive_search sibling-chain traversal that dropped ~30% of
  keys on large tries when byte order and frequency-assigned code order
  disagreed (send/lexime-trie#21).

## 0.2.1 — 2026-02-19

- Initial crates.io release of the char-wise Double-Array Trie with
  zero-copy mmap support.
