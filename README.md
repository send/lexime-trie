# lexime-trie

A char-wise Double-Array Trie library for [lexime](https://github.com/send/lexime). Zero dependencies.

Replaces `trie-rs` + `bincode` with a compact, cache-friendly trie that supports both dictionary (`DoubleArray<char>`) and romaji (`DoubleArray<u8>`) use cases.

## Features

- **8 bytes/node** (`#[repr(C)]`) — fits 8 nodes per cache line
- **Char-wise indexing** — 1.5-2x faster than byte-wise for Japanese text (based on [crawdad](https://github.com/daac-tools/crawdad) benchmarks)
- **Frequency-ordered CodeMapper** — remaps labels to dense codes for compact arrays
- **Terminal symbol approach** — cleanly handles keys that are both exact matches and prefixes of other keys
- **Sibling chain (SoA)** — enables predictive search via DFS without increasing node size for other operations
- **Fast serialization** — binary format with `LXTR` magic header, ~5ms copy-load

## Search Operations

| Operation | Description | Use Case |
|-----------|-------------|----------|
| `exact_match` | O(m) key lookup | Dictionary lookup |
| `common_prefix_search` | All prefixes of a query | Lattice construction (Viterbi) |
| `predictive_search` | All keys starting with a prefix | Autocomplete / predict |
| `probe` | Key existence + children check (4-state) | Romaji input (None/Prefix/Exact/ExactAndPrefix) |

## Usage

```rust
use lexime_trie::DoubleArray;

// Build from sorted keys (value_id = index)
let da = DoubleArray::<u8>::build(&[b"abc", b"abd", b"xyz"]);

// Exact match
assert_eq!(da.exact_match(b"abc"), Some(0));
assert_eq!(da.exact_match(b"zzz"), None);

// Common prefix search
for m in da.common_prefix_search(b"abcd") {
    println!("prefix len={}, value_id={}", m.len, m.value_id);
}

// Predictive search
for m in da.predictive_search(b"ab") {
    println!("key={:?}, value_id={}", m.key, m.value_id);
}

// Probe (romaji trie scenario)
let result = da.probe(b"ab");
// result.value: Option<u32>, result.has_children: bool

// Serialization
let bytes = da.as_bytes();
let da2 = DoubleArray::<u8>::from_bytes(&bytes).unwrap();
```

### Char keys (dictionary)

```rust
use lexime_trie::DoubleArray;

let keys: Vec<Vec<char>> = vec![
    "あい".chars().collect(),
    "あう".chars().collect(),
    "かき".chars().collect(),
];
let da = DoubleArray::<char>::build(&keys);
assert!(da.exact_match(&"あい".chars().collect::<Vec<_>>()).is_some());
```

## Design

See [SPEC.md](https://github.com/send/lexime-trie/blob/main/SPEC.md) for the full design document.

## License

MIT
