# lexime-trie Design Document

> Japanese version: [SPEC.ja.md](SPEC.ja.md)

## Overview

lexime-trie is a general-purpose Double-Array Trie library for [lexime](https://github.com/send/lexime).
It replaces `trie-rs` + `bincode`, providing a unified trie for both dictionary and romaji use cases.

## Motivation

The current `TrieDictionary` serializes `trie-rs::map::Trie<u8, Vec<DictEntry>>` with bincode.

| Aspect | Current | With lexime-trie |
|--------|---------|------------------|
| Dictionary file size | ~49MB (bincode) | To be measured |
| Load time | Hundreds of ms (bincode deserialize) | ~5ms (memcpy) |
| Node representation | trie-rs internals (opaque) | `#[repr(C)]` 8B/node |
| Value storage | `Vec<DictEntry>` inside the trie | External array (referenced by value_id) |
| Label scheme | byte-wise (UTF-8 byte units) | **char-wise** (character units) |
| Dependencies | trie-rs, serde, bincode | None (zero deps) |

Char-wise indexing makes Japanese `common_prefix_search` **1.5-2x faster** than byte-wise
(verified by crawdad benchmarks).

The `RomajiTrie` currently uses a `HashMap<u8, Node>` tree, which can be replaced with
`DoubleArray<u8>` from lexime-trie for unification.

## Prior Art

| Crate | Label | Node Size | predictive_search | Notes |
|-------|-------|-----------|-------------------|-------|
| yada | byte-wise | 8B | No | Rust port of darts-clone |
| crawdad | char-wise | 8B | No | Used by vibrato (2x faster than MeCab) |
| trie-rs | byte-wise | LOUDS | Yes | Currently used by lexime |
| **lexime-trie** | **char-wise** | **8B + CSR children** | **Yes** | crawdad approach + predictive_search |

crawdad benchmarks (ipadic-neologd, 5.5M keys):

| Operation | crawdad (char-wise) | yada (byte-wise) | Difference |
|-----------|---------------------|-------------------|------------|
| exact_match | 9-28 ns | 22-97 ns | 2-3x faster |
| common_prefix_search | 2.0-2.6 us/line | 3.7-5.3 us/line | 1.5-2x faster |
| Build time | 1.93 sec | 34.74 sec | 18x faster |
| Memory | 121 MiB | 153 MiB | 20% smaller |

lexime-trie adopts crawdad's char-wise + CodeMapper approach, adding
**predictive_search** (via CSR children list) and **probe** which crawdad lacks.

## Data Structures

### Node

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// BASE — XOR-based child offset (31 bits) | IS_LEAF (1 bit)
    base: u32,
    /// CHECK — parent node index (31 bits) | HAS_LEAF (1 bit)
    check: u32,
}
```

- **8 bytes/node** — 8 nodes fit in a single cache line (64B)
- Child of node `n` with label `c`: `index = base(n) XOR code_map(c)`, verified by `check(index) == n`
- IS_LEAF: MSB of base. When set, the remaining 31 bits store the value_id
- HAS_LEAF: MSB of check. When set, a terminal child (code 0) exists
- Child lookup is O(1): direct index via `base XOR label`

### Children List (CSR Layout)

```rust
child_offsets: Vec<u32>    // length N + 1
children_list: Vec<u32>    // length E (total edge count)
```

For any parent node at index `p`, its children are:

```
children_list[child_offsets[p] .. child_offsets[p+1]]
```

Children appear in code-ascending order within each parent's slice, so the
terminal child (code 0, if present) is always first.

- **Not stored inside the `Node` struct** — SoA layout keeps the hot paths tight.
- `exact_match` / `common_prefix_search` touch only `nodes` and preserve the
  **8B/node** hot path.
- `probe` reads `nodes` + `child_offsets` (one extra u32 pair per query).
- `predictive_search` reads all three arrays.
- `nodes[0]` is an invalid sentinel; the root lives at `nodes[1]`. This
  removes the ambiguity between "child of root" and "unused slot" that
  `check == 0` used to carry in earlier designs.

| Operation | Arrays Accessed |
|-----------|-----------------|
| `exact_match` | `nodes` only |
| `common_prefix_search` | `nodes` only |
| `probe` | `nodes` + `child_offsets` |
| `predictive_search` | `nodes` + `child_offsets` + `children_list` |

### CodeMapper (Frequency-Ordered Label Remapping)

In a char-wise Double-Array, using raw Unicode code points as labels would result in
a sparse array. **CodeMapper** remaps labels to dense, frequency-ordered codes.

```rust
pub struct CodeMapper {
    /// label (as u32) → remapped code (0 = unmapped)
    table: Vec<u32>,
    /// code → label (as u32). Index 0 is unused (terminal symbol)
    reverse_table: Vec<u32>,
    /// Total number of codes including the terminal symbol
    alphabet_size: u32,
}
```

- At build time, label frequencies across all keys are counted; higher-frequency labels receive smaller codes
- Example: ~80 hiragana + ~80 katakana + ~3000 kanji → effective alphabet size ≈ 4000
- Code 0 is reserved for the terminal symbol
- Same approach as crawdad's Mapped scheme (Kanda et al. 2023)
- `reverse_table` is used for key reconstruction in `predictive_search`
- `DoubleArray<u8>` (romaji trie) also uses frequency-ordered CodeMapper;
  this produces denser arrays than an identity mapping

### Value Storage (Terminal Symbol Approach)

The trie does not store values directly. Values are stored via a **terminal symbol (code = 0)**.

When registering key "きょう" with value_id=42, the internal representation is
`[code('き'), code('ょ'), code('う'), 0]`. The terminal node's BASE field stores the value_id.

```
Regular node:
  base  = XOR offset (31 bits) | IS_LEAF=0
  check = parent node index (31 bits)

Terminal node (IS_LEAF = 1):
  value_id = base & 0x7FFF_FFFF  — 31 bits, max ~2G entries
  check    = parent node index
```

This approach naturally represents **nodes that are both exact matches and prefixes** (ExactAndPrefix).
For example, in a romaji trie where "n" → "ん" and "na" → "な":

```
root --'n'--> N --[0]--> [value_id for "ん"]   (Exact)
                  --'a'--> A --[0]--> [value_id for "な"]
```

Node N has children (terminal, 'a'), so its BASE points to the child array, while the
value_id is stored in the terminal child node. No bit-field conflict occurs.

**Capacity**: value_id is 31 bits, supporting up to ~2G values. Sufficient.

**Size overhead**: Each value-bearing key adds a terminal node (8 bytes).

lexime integration:

| Use Case | Key Type | value_id Points To |
|----------|----------|-------------------|
| Dictionary | `&str` (hiragana reading) | `&[DictEntry]` slice via offset table |
| Romaji | `&[u8]` (ASCII romaji) | Index into kana string table |

## API

### Label Trait

```rust
pub trait Label: Copy + Ord + Into<u32> + TryFrom<u32> {}

impl Label for u8 {}
impl Label for char {}
```

Dictionary trie uses `DoubleArray<char>` + CodeMapper; romaji trie uses `DoubleArray<u8>`.
CodeMapper compresses the effective label space to ~4000, so the size of the raw label
space (e.g. `char`'s 0x11_0000 codepoints) does not affect the array size.

### DoubleArray

```rust
pub struct DoubleArray<L: Label> {
    nodes: Vec<Node>,           // nodes[0] = sentinel, nodes[1] = root
    child_offsets: Vec<u32>,    // CSR offsets, length nodes.len() + 1
    children_list: Vec<u32>,    // flat list of child indices, length E
    code_map: CodeMapper,       // label → internal code mapping
    _phantom: PhantomData<L>,
}
```

### Build

```rust
impl<L: Label> DoubleArray<L> {
    /// Builds from sorted keys.
    /// Each key `keys[i]` is assigned value_id = i.
    ///
    /// # Panics
    /// - If keys are not sorted in ascending order.
    pub fn build(keys: &[impl AsRef<[L]>]) -> Self;
}
```

- Input: sorted key array. `keys[i]` gets value_id `i`.
- Build steps:
  1. Count label frequencies across all keys → build CodeMapper.
  2. Convert keys to remapped code sequences + append terminal symbol.
  3. Greedily place BASE values using a doubly-linked circular free list
     (indices 0 and 1 are reserved for the sentinel and the root).
  4. Record each placed edge `(parent, child)` into a flat edge vector.
  5. After recursion completes, run an O(N + E) count-and-scatter pass
     to flatten edges into `child_offsets` + `children_list`. Within-
     parent order is preserved because edges are enqueued in code-ascending
     order and scatter writes them to consecutive slots.
- Build runs once at dictionary compile time (`dictool compile`).

### Search Operations

```rust
impl<L: Label> DoubleArray<L> {
    /// Exact match search. Returns the value_id if the key exists.
    pub fn exact_match(&self, key: &[L]) -> Option<u32>;

    /// Common prefix search. Returns all prefixes of `query` that exist as keys.
    /// Used for lattice construction (Viterbi).
    pub fn common_prefix_search<'a>(&'a self, query: &'a [L])
        -> impl Iterator<Item = PrefixMatch> + 'a;

    /// Predictive search. Returns all keys starting with `prefix` by iterating
    /// each node's `children_list` slice in a DFS order. Used for
    /// predict / predict_ranked in dictionary.
    pub fn predictive_search<'a>(&'a self, prefix: &'a [L])
        -> impl Iterator<Item = SearchMatch<L>> + 'a;

    /// Probe a key. Returns whether the key exists and whether it has children.
    /// Used for romaji trie lookup (None/Prefix/Exact/ExactAndPrefix).
    ///
    /// O(1) determination:
    /// 1. Traversal fails → None.
    /// 2. Reach node N. If `has_leaf(N)`, the terminal child is at
    ///    `base(N) XOR 0 == base(N)`. `has_children` is read from the
    ///    CSR slice width: `(child_offsets[N+1] - child_offsets[N]) > 1`
    ///    means more than just the terminal.
    /// 3. If `has_leaf(N)` is false, `value = None` and
    ///    `has_children = (child_offsets[N+1] - child_offsets[N]) > 0`.
    pub fn probe(&self, key: &[L]) -> ProbeResult;
}

pub struct PrefixMatch {
    pub len: usize,      // length of the matched prefix
    pub value_id: u32,
}

pub struct SearchMatch<L> {
    pub key: Vec<L>,     // full matched key (built during DFS, allocated per match)
    pub value_id: u32,
}

pub struct ProbeResult {
    pub value: Option<u32>,  // value_id if key exists
    pub has_children: bool,  // whether non-terminal children exist
}
```

### Serialization (LXTR v3)

```rust
impl<L: Label> DoubleArray<L> {
    /// Serializes the internal data to a raw byte representation (v3 format).
    pub fn as_bytes(&self) -> Vec<u8>;

    /// Restores a DoubleArray from raw bytes (copy).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TrieError>;
}
```

**v3 binary format** (24-byte header, 8-byte aligned):

```
Offset                        Size       Content
0                             4          Magic: "LXTR"
4                             1          Version: 0x03
5                             3          Reserved: [0, 0, 0]
8                             4          nodes_count    (u32 LE, = N)
12                            4          children_count (u32 LE, = E)
16                            4          code_map_len   (u32 LE, in bytes)
20                            4          Reserved: [0, 0, 0, 0]
24                            N*8        nodes (base LE u32 + check LE u32)
24+N*8                        (N+1)*4    child_offsets (each: u32 LE)
24+N*8+(N+1)*4                E*4        children_list (each: u32 LE)
24+N*8+(N+1)*4+E*4            M          code_map data
```

- Sizes are in **count** units, not bytes. The byte length of each fixed
  section is derivable from `nodes_count` and `children_count`, which
  eliminates the "length mismatch" class of errors v2 had to validate
  against.
- Four sections: `nodes`, `child_offsets`, `children_list`, `code_map`.
- The 24-byte header keeps `nodes` 8-byte aligned; all subsequent sections
  are at least 4-byte aligned, satisfying `Node` and `u32` requirements.
- Raw `#[repr(C)]` data (little-endian), enabling zero-copy deserialization.
- **Little-endian only** (enforced by `compile_error!`).
- **v2 compatibility was dropped at v0.3**. `DoubleArray::from_bytes` /
  `DoubleArrayRef::from_bytes` return `InvalidVersion` for version != 3.
  Persisted v2 files must be rebuilt from the original keys.

#### Load-time validation

`DoubleArray::from_bytes` / `DoubleArrayRef::from_bytes` run only O(1)
checks:

- Magic, version, buffer size arithmetic.
- `nodes_count >= 2` (sentinel + root).
- Section alignments (zero-copy path only).
- `child_offsets[0] == 0` and `child_offsets[N] == children_count`.

Monotonicity of `child_offsets` is intentionally **not** checked at load
time — it would be O(N) and defeat the zero-copy promise. A malformed
offset cannot cause UB: Rust slice indexing is always bounds-checked,
so corruption surfaces as a graceful panic on query. Callers that need
hard guarantees before any query can run a strict O(N) validator
separately.

### Zero-Copy Deserialization

```rust
pub struct DoubleArrayRef<'a, L: Label> {
    // Sections are stored as (raw pointer, length) pairs rather than
    // `&'a [T]` references. The `PhantomData<&'a [u8]>` carries the
    // borrow lifetime; `view()` materialises real slices on demand.
    // See the type's rustdoc for why raw pointers (Stacked Borrows
    // interaction with `DoubleArrayBacked`'s self-referential layout).
    nodes_ptr: *const Node,
    nodes_len: usize,
    child_offsets_ptr: *const u32,
    child_offsets_len: usize,
    children_list_ptr: *const u32,
    children_list_len: usize,
    code_map: CodeMapper,                // always heap-allocated (small)
    _marker: PhantomData<(&'a [u8], L)>,
}

impl<'a, L: Label> DoubleArrayRef<'a, L> {
    /// Zero-copy deserialization from a byte slice (v3 format only).
    /// The buffer must be aligned to at least 4 bytes (for `Node` and `u32` access).
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, TrieError>;

    /// All search methods are provided by the `TrieSearch` trait:
    /// exact_match, common_prefix_search, predictive_search, probe.

    /// Converts to an owned DoubleArray by copying the borrowed sections to heap.
    pub fn to_owned(&self) -> DoubleArray<L>;
}
```

- `nodes`, `child_offsets`, and `children_list` reference data directly in
  the byte buffer via `unsafe` pointer cast.
- Safety relies on: `Node` being `#[repr(C)]` (8B, align 4, no padding),
  runtime alignment validation, and the LE-only target assumption.
- `code_map` is always deserialized to heap (small, requires reconstruction
  from serialized form).
- `from_bytes` requires the LXTR v3 format (24-byte aligned header).
- `from_bytes` is O(1) after the initial header parse — no loops over
  `nodes`, `child_offsets`, or `children_list`.
- Typical use case: memory-map a file, then pass the buffer to `from_bytes`
  (or to `DoubleArrayBacked::from_backing` if you want the buffer and view
  bundled into one owning value).

### Shared Search Logic (TrieView)

All search methods (`traverse`, `exact_match`, `common_prefix_search`,
`predictive_search`, `probe`) are implemented once in `TrieView<'a, L>`:

```rust
#[derive(Clone, Copy)]
pub(crate) struct TrieView<'a, L: Label> {
    nodes: &'a [Node],
    child_offsets: &'a [u32],
    children_list: &'a [u32],
    code_map: &'a CodeMapper,
    _phantom: PhantomData<L>,
}
```

Both `DoubleArray` and `DoubleArrayRef` delegate to `TrieView`, achieving
zero code duplication. Enumeration operations (`predictive_search`) iterate
the CSR children slice directly, eliminating the O(alphabet_size) scan
that earlier `first_child()` implementations relied on.

### Error Type

```rust
pub enum TrieError {
    /// Binary data has an invalid magic number
    InvalidMagic,
    /// Binary data has an unsupported version
    InvalidVersion,
    /// Binary data is truncated or corrupted
    TruncatedData,
    /// Byte buffer is not properly aligned for zero-copy access
    MisalignedData,
}
```

## Integration with lexime

### Dictionary File Format (LXDX, using LXTR v3 sections)

The lexime dictionary file embeds an LXTR v3 trie followed by lexime-
specific entry tables. The exact LXDX version is tracked separately in
lexime's own repository; the section shapes below reflect the v3 trie
layout:

```
Section                Content
────────────────────   ──────────────────────────
magic                  "LXDX" (4 bytes)
version                dictionary format version
...                    dictionary-specific counts
nodes                  [Node; N]             ← lexime-trie: base+check
child_offsets          [u32; N+1]            ← lexime-trie: CSR offsets
children_list          [u32; E]              ← lexime-trie: CSR children
code_map               CodeMapper            ← lexime-trie: label mapping
offsets                [u32; V+1]            ← lexime: value_id → entry range
entries                [FlatDictEntry; M]    ← lexime: entry data
```

- `FlatDictEntry`: flat representation of `DictEntry` without `String`
  (surface strings stored in a separate string table, referenced by offset)
- **Offset table**: maps value_id to entry ranges when a single reading has multiple DictEntries.
  Entries for value_id `i` are `entries[offsets[i]..offsets[i+1]]`

### Replacing TrieDictionary

| Current API | With lexime-trie |
|-------------|------------------|
| `Trie<u8, Vec<DictEntry>>` | `DoubleArray<char>` + `Vec<DictEntry>` |
| `trie.exact_match(key)` → `Option<&Vec<DictEntry>>` | `da.exact_match(key)` → `Option<u32>` → `entries[range]` |
| `trie.common_prefix_search(query)` → iter | `da.common_prefix_search(query)` → iter |
| `trie.predictive_search(prefix)` → iter | `da.predictive_search(prefix)` → iter |
| `bincode::serialize/deserialize` | `as_bytes()` / `from_bytes()` |

The `Dictionary` trait implementation remains unchanged. Only the internal data structure is replaced.

### Replacing RomajiTrie

| Current | With lexime-trie |
|---------|------------------|
| `HashMap<u8, Node>` tree | `DoubleArray<u8>` |
| `lookup() → TrieLookupResult` | `probe() → ProbeResult` → convert to `TrieLookupResult` |
| Dynamic `insert` | Static build via `DoubleArray::build()` |

```rust
// RomajiTrie::lookup implementation sketch
pub fn lookup(&self, romaji: &str) -> TrieLookupResult {
    let result = self.da.probe(romaji.as_bytes());
    match (result.value, result.has_children) {
        (None, false) => TrieLookupResult::None,
        (None, true) => TrieLookupResult::Prefix,
        (Some(id), false) => TrieLookupResult::Exact(self.kana[id as usize].clone()),
        (Some(id), true) => TrieLookupResult::ExactAndPrefix(self.kana[id as usize].clone()),
    }
}
```

Romaji trie is ASCII-only, so it uses byte-wise (`DoubleArray<u8>`).
CodeMapper uses frequency-ordered remapping (produces denser arrays than identity mapping).

## Crate Structure

```
lexime/
├── lexime-trie/           ← this crate (standalone repository)
│   ├── Cargo.toml         [dependencies] none (dev: criterion)
│   └── src/
│       ├── lib.rs         pub mod + DoubleArray + TrieError
│       ├── label.rs       Label trait + u8/char impl
│       ├── node.rs        Node (base + check, 8B)
│       ├── code_map.rs    CodeMapper (frequency-ordered label remapping)
│       ├── build.rs       DoubleArray::build() + CSR flatten
│       ├── search.rs      search method delegation to TrieView
│       ├── serial.rs      as_bytes, from_bytes, HeaderV3, validate_cheap
│       ├── view.rs        TrieView — shared search logic
│       └── da_ref.rs      DoubleArrayRef — zero-copy deserialization
├── engine/                ← existing crate (depends on lexime-trie)
│   └── Cargo.toml         remove trie-rs, serde, bincode → add lexime-trie
└── Cargo.toml             ← workspace
```

## Constraints & Non-Goals

- **No dynamic insert/delete**. Immutable, build-once trie only
- **No compression (TAIL, MpTrie, etc.)** in the initial implementation. Can be added later

## Implementation Progress

1. **Node + Label + CodeMapper** — basic type definitions and label remapping ✅
2. **build** — build Double-Array from sorted keys (free list + CSR flatten) ✅
3. **exact_match** — simplest search ✅
4. **common_prefix_search** — needed for lattice construction ✅
5. **predictive_search** — needed for prediction (uses `children_list`) ✅
6. **probe** — needed for romaji trie ✅
7. **as_bytes / from_bytes** — serialization (LXTR v3 format) ✅
8. **DoubleArrayRef / from_bytes** — zero-copy mmap deserialization ✅
9. **v3 migration** — drop v2, move root to `nodes[1]`, replace siblings with CSR ✅
10. **lexime integration** — replace TrieDictionary and RomajiTrie internals

## Migration Notes

### v0.2 → v0.3

The v3 format is a **breaking change**. Persisted v2 files are rejected
with `TrieError::InvalidVersion`. Rebuild from the original key set.

Rationale:

- `siblings: Vec<u32>` relied on an implicit invariant that children were
  placed in the same order that `first_child()` rediscovered them at search
  time. Any drift between placement and walk silently dropped keys from
  `predictive_search` (see the #21 regression).
- Replacing `siblings` with `child_offsets` + `children_list` makes the
  ordering a stored property rather than an emergent one, eliminating the
  drift class of bugs.
- Moving the root to `nodes[1]` gives `check == 0` a single unambiguous
  meaning ("unused slot"), removing several multi-condition defensive
  checks from the search paths.
- Removing the `first_child()` alphabet scan cuts `predictive_search` time
  by ~47% on the 50k hiragana benchmark.
