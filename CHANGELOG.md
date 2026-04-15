# Changelog

All notable changes to this crate are documented in this file.

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

### Fixed

- `predictive_search` no longer relies on an implicit invariant between
  child placement order at build time and an `O(alphabet_size)` scan at
  query time. The CSR children list stores the order explicitly, making
  the #21 class of drop-children bugs structurally impossible.

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
