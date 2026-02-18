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
| **lexime-trie** | **char-wise** | **8B (+4B sibling)** | **Yes** | crawdad approach + predictive_search |

crawdad benchmarks (ipadic-neologd, 5.5M keys):

| Operation | crawdad (char-wise) | yada (byte-wise) | Difference |
|-----------|---------------------|-------------------|------------|
| exact_match | 9-28 ns | 22-97 ns | 2-3x faster |
| common_prefix_search | 2.0-2.6 us/line | 3.7-5.3 us/line | 1.5-2x faster |
| Build time | 1.93 sec | 34.74 sec | 18x faster |
| Memory | 121 MiB | 153 MiB | 20% smaller |

lexime-trie adopts crawdad's char-wise + CodeMapper approach, adding
**predictive_search** (sibling chain) and **probe** which crawdad lacks.

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

### Sibling Array (Parallel SoA Layout)

```rust
siblings: Vec<u32>   // parallel array, same length as nodes
```

- `siblings[i]` — index of the next sibling node sharing the same parent (0 = none)
- **Not included in the Node struct** — Structure of Arrays (SoA) layout
- `common_prefix_search` / `exact_match` access only `nodes` (**8B/node**)
- `predictive_search` / `probe` also access `siblings` (effective 12B/node)

| Operation | Arrays Accessed | Effective Node Size |
|-----------|----------------|---------------------|
| `exact_match` | `nodes` only | **8B** |
| `common_prefix_search` | `nodes` only | **8B** |
| `probe` | `nodes` + `siblings` | 12B |
| `predictive_search` | `nodes` + `siblings` | 12B |

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
- Example: ~80 hiragana + ~80 katakana + ~3000 kanji → effective ALPHABET_SIZE ≈ 4000
- Code 0 is reserved for the terminal symbol
- Same approach as crawdad's Mapped scheme (Kanda et al. 2023)
- `reverse_table` is used for key reconstruction in `predictive_search`
- `DoubleArray<u8>` (romaji trie) also uses frequency-ordered CodeMapper;
  this produces denser arrays and narrower `first_child()` scan ranges than an identity mapping

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
pub trait Label: Copy + Ord + Into<u32> + TryFrom<u32> {
    /// Maximum label value + 1 (used for array allocation)
    const ALPHABET_SIZE: u32;
}

impl Label for u8 {
    const ALPHABET_SIZE: u32 = 256;
}

impl Label for char {
    const ALPHABET_SIZE: u32 = 0x11_0000;
}
```

Dictionary trie uses `DoubleArray<char>` + CodeMapper; romaji trie uses `DoubleArray<u8>`.
CodeMapper compresses the effective label space to ~4000, so `char::ALPHABET_SIZE` does not
affect the array size.

### DoubleArray

```rust
pub struct DoubleArray<L: Label> {
    nodes: Vec<Node>,
    siblings: Vec<u32>,       // parallel array (for predictive_search / probe)
    code_map: CodeMapper,     // label → internal code mapping
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

- Input: sorted key array. `keys[i]` gets value_id `i`
- Build steps:
  1. Count label frequencies across all keys → build CodeMapper
  2. Convert keys to remapped code sequences + append terminal symbol
  3. Greedily place BASE values using a doubly-linked circular free list
  4. Build sibling chains
- Build runs once at dictionary compile time (`dictool compile`)

### Search Operations

```rust
impl<L: Label> DoubleArray<L> {
    /// Exact match search. Returns the value_id if the key exists.
    pub fn exact_match(&self, key: &[L]) -> Option<u32>;

    /// Common prefix search. Returns all prefixes of `query` that exist as keys.
    /// Used for lattice construction (Viterbi).
    pub fn common_prefix_search<'a>(&'a self, query: &'a [L])
        -> impl Iterator<Item = PrefixMatch> + 'a;

    /// Predictive search. Returns all keys starting with `prefix` via sibling chain DFS.
    /// Used for predict / predict_ranked in dictionary.
    pub fn predictive_search<'a>(&'a self, prefix: &'a [L])
        -> impl Iterator<Item = SearchMatch<L>> + 'a;

    /// Probe a key. Returns whether the key exists and whether it has children.
    /// Used for romaji trie lookup (None/Prefix/Exact/ExactAndPrefix).
    ///
    /// O(1) determination via the terminal symbol approach:
    /// 1. Traversal fails → None
    /// 2. Reach node N → check terminal child at base(N) XOR 0
    ///    - Terminal child exists → value = Some(value_id),
    ///      has_children = (siblings[terminal] != 0)
    ///    - No terminal child → value = None, has_children = true
    ///      (since N exists, keys reachable through its children must exist)
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

### Serialization (LXTR v2)

```rust
impl<L: Label> DoubleArray<L> {
    /// Serializes the internal data to a raw byte representation (v2 format).
    pub fn as_bytes(&self) -> Vec<u8>;

    /// Restores a DoubleArray from raw bytes (copy).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TrieError>;
}
```

**v2 binary format** (24-byte header, 8-byte aligned):

```
Offset  Size  Content
0       4     Magic: "LXTR"
4       1     Version: 0x02
5       3     Reserved: [0, 0, 0]
8       4     nodes_len (u32 LE, in bytes)
12      4     siblings_len (u32 LE, in bytes)
16      4     code_map_len (u32 LE, in bytes)
20      4     Reserved: [0, 0, 0, 0]
24      N     nodes data (each node: base LE u32 + check LE u32)
24+N    S     siblings data (each: u32 LE)
24+N+S  C     code_map data
```

- The 24-byte header ensures `nodes` data starts at an 8-byte boundary (exceeds the 4-byte alignment required by `Node`/`u32`)
- Three sections: `nodes`, `siblings`, `code_map`
- Raw `#[repr(C)]` data (serialized as little-endian)
- Copy-load: ~5ms, runs once at app startup
- **Endianness**: serialization uses little-endian (`to_le_bytes` / `from_le_bytes`),
  ensuring cross-platform compatibility

### Zero-Copy Deserialization

```rust
pub struct DoubleArrayRef<'a, L: Label> {
    nodes: &'a [Node],       // borrowed from byte buffer
    siblings: &'a [u32],     // borrowed from byte buffer
    code_map: CodeMapper,    // always heap-allocated (small)
    _phantom: PhantomData<L>,
}

impl<'a, L: Label> DoubleArrayRef<'a, L> {
    /// Zero-copy deserialization from a byte slice (v2 format only).
    /// The buffer must be aligned to at least 4 bytes (for `Node` and `u32` access).
    pub fn from_bytes_ref(bytes: &'a [u8]) -> Result<Self, TrieError>;

    /// All search methods: exact_match, common_prefix_search,
    /// predictive_search, probe — identical API to DoubleArray.

    /// Converts to an owned DoubleArray by copying nodes/siblings to heap.
    pub fn to_owned(&self) -> DoubleArray<L>;
}
```

- `nodes` and `siblings` are borrowed directly from the byte buffer via `unsafe` pointer cast
- Safety relies on: `Node` being `#[repr(C)]` (8B, align 4, no padding), runtime alignment
  validation, and LE-only target assumption (x86_64/aarch64)
- `code_map` is always deserialized to heap (small, requires reconstruction from serialized form)
- `from_bytes_ref` requires the LXTR v2 format (24-byte aligned header)
- Typical use case: memory-map a file, then pass the buffer to `from_bytes_ref`

### Shared Search Logic (TrieView)

All search methods (`traverse`, `exact_match`, `common_prefix_search`, `predictive_search`,
`first_child`, `probe`) are implemented once in `TrieView<'a, L>`:

```rust
#[derive(Clone, Copy)]
pub(crate) struct TrieView<'a, L: Label> {
    nodes: &'a [Node],
    siblings: &'a [u32],
    code_map: &'a CodeMapper,
    _phantom: PhantomData<L>,
}
```

Both `DoubleArray` and `DoubleArrayRef` delegate to `TrieView`, achieving zero code duplication.

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

### Dictionary File Format (LXDX v2)

```
Offset      Size  Content
──────────  ────  ──────────────────────────
0           4     magic: "LXDX"
4           1     version: 2
5           4     nodes_len: u32
9           4     siblings_len: u32
13          4     code_map_len: u32
17          4     offsets_len: u32
21          4     entries_len: u32
25          N     [Node; K]              ← lexime-trie: base+check
25+N        S     [u32; K]               ← lexime-trie: siblings
25+N+S      C     CodeMapper             ← lexime-trie: label mapping table
25+N+S+C    O     [u32; V+1]             ← offset table
25+N+S+C+O  E     [FlatDictEntry; M]     ← lexime: entry data
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
│       ├── build.rs       DoubleArray::build() + sibling chain construction
│       ├── search.rs      search method delegation to TrieView
│       ├── serial.rs      as_bytes, from_bytes
│       ├── view.rs        TrieView — shared search logic (exact_match, common_prefix_search, etc.)
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
2. **build** — build Double-Array from sorted keys (free list + sibling chain) ✅
3. **exact_match** — simplest search ✅
4. **common_prefix_search** — needed for lattice construction ✅
5. **predictive_search** — needed for prediction (uses sibling chain) ✅
6. **probe** — needed for romaji trie ✅
7. **as_bytes / from_bytes** — serialization (LXTR v2 format) ✅
8. **DoubleArrayRef / from_bytes_ref** — zero-copy mmap deserialization ✅
9. **lexime integration** — replace TrieDictionary and RomajiTrie internals
