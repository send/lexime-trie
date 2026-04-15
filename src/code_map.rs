use std::collections::HashMap;

use crate::Label;

/// Threshold for switching from a dense `Vec<u64>` frequency table to a
/// `HashMap<u32, u64>` during `CodeMapper::build`.
///
/// Dense array is strictly faster (O(1) load+store vs. HashMap's hash and
/// probe), but it allocates `8 * (max_label + 1)` bytes up-front. For common
/// Japanese workloads (hiragana/katakana/CJK basic) max_label stays below
/// 0xA000, so the dense array costs at most ~320 KB — a clear win over
/// HashMap overhead.
///
/// For keys that include emoji (U+1F000+) or supplementary planes the
/// dense freq array balloons (U+10FFFF → 8 MB). The HashMap branch only
/// bounds *that* counting structure by the number of distinct labels —
/// the forward lookup `table: Vec<u32>` is still allocated densely up to
/// `max_label + 1` regardless, because `get()` is the hot search path
/// and must stay O(1). So the threshold trades one transient 8 MB
/// allocation for HashMap overhead during build, while the persistent
/// `table` still scales with `max_label`.
///
/// 65_536 is chosen as the cutoff: it covers the BMP entirely (typical
/// Japanese-only dictionaries stay well under it) and caps the dense
/// allocation at 512 KB per table.
const FREQ_DENSE_THRESHOLD: u32 = 65_536;

/// Maps labels to dense, frequency-ordered codes.
///
/// Code 0 is reserved for the terminal symbol.
/// Higher-frequency labels receive smaller codes to improve cache locality.
#[derive(Clone, Debug)]
pub struct CodeMapper {
    /// label (as u32) → remapped code. 0 means unmapped.
    table: Vec<u32>,
    /// code → label (as u32). Index 0 is unused (terminal symbol).
    reverse_table: Vec<u32>,
    /// Number of distinct codes (including terminal symbol at 0).
    alphabet_size: u32,
}

impl CodeMapper {
    /// Builds a CodeMapper from the given keys.
    ///
    /// Counts the frequency of each label across all keys and assigns
    /// dense codes in descending frequency order. Code 0 is reserved
    /// for the terminal symbol.
    pub fn build<L: Label>(keys: &[impl AsRef<[L]>]) -> Self {
        // Find max label value in a single pass to size the frequency array.
        let mut max_label: u32 = 0;
        for key in keys {
            for &label in key.as_ref() {
                let v: u32 = label.into();
                if v > max_label {
                    max_label = v;
                }
            }
        }

        if keys.is_empty() || keys.iter().all(|k| k.as_ref().is_empty()) {
            return Self {
                table: vec![],
                reverse_table: vec![0],
                alphabet_size: 1,
            };
        }

        let table_size = (max_label as usize)
            .checked_add(1)
            .expect("CodeMapper::build: label space too large for this platform");

        // Count label frequencies. For dense alphabets (u8 keys, or `char`
        // keys staying within the BMP) we use a `Vec<u64>` indexed directly
        // by label value — the fastest option. For sparse-but-wide alphabets
        // (char keys reaching into astral planes), we fall back to a
        // HashMap so memory scales with distinct labels instead of with
        // the maximum label value.
        let mut labels: Vec<(u32, u64)> = if max_label < FREQ_DENSE_THRESHOLD {
            let mut freq = vec![0u64; table_size];
            for key in keys {
                for &label in key.as_ref() {
                    freq[<L as Into<u32>>::into(label) as usize] += 1;
                }
            }
            freq.iter()
                .enumerate()
                .filter(|(_, &f)| f > 0)
                .map(|(i, &f)| (i as u32, f))
                .collect()
        } else {
            let mut freq: HashMap<u32, u64> = HashMap::new();
            for key in keys {
                for &label in key.as_ref() {
                    *freq.entry(label.into()).or_insert(0) += 1;
                }
            }
            freq.into_iter().collect()
        };

        // Sort by frequency descending, then by label ascending for stability
        labels.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

        let mut table = vec![0u32; table_size];
        let mut reverse_table = vec![0u32; labels.len() + 1]; // +1 for terminal at index 0

        for (i, &(label, _)) in labels.iter().enumerate() {
            let code = (i as u32) + 1; // code 0 is terminal
            table[label as usize] = code;
            reverse_table[code as usize] = label;
        }

        let alphabet_size = labels.len() as u32 + 1; // including terminal

        Self {
            table,
            reverse_table,
            alphabet_size,
        }
    }

    /// Returns the code for a label. Returns 0 if the label is unmapped.
    #[inline]
    pub fn get<L: Label>(&self, label: L) -> u32 {
        let v: u32 = label.into();
        let idx = v as usize;
        if idx < self.table.len() {
            // SAFETY: bounds verified by the check above.
            unsafe { *self.table.get_unchecked(idx) }
        } else {
            0
        }
    }

    /// Returns the label (as u32) for a code, or `None` if the code is out
    /// of range for this mapper. Code 0 is the terminal symbol.
    ///
    /// Out-of-range codes are returned as `None` rather than a panic so that
    /// corrupted / malformed serialized data surfaces as a graceful signal
    /// at the call site instead of an OOB index panic in release builds.
    #[inline]
    pub fn reverse(&self, code: u32) -> Option<u32> {
        self.reverse_table.get(code as usize).copied()
    }

    /// The number of distinct codes including the terminal symbol.
    #[inline]
    pub fn alphabet_size(&self) -> u32 {
        self.alphabet_size
    }

    /// Returns the serialised size in bytes (without allocating).
    ///
    /// Uses checked arithmetic to match the style of `serial::as_bytes`
    /// and `CodeMapper::from_bytes`. A wrapped value here would feed a
    /// too-small `Vec::with_capacity` in `as_bytes`; the subsequent
    /// `extend_from_slice` calls inside `write_to` would still produce
    /// a correct buffer (they grow as needed), but the capacity hint
    /// would be wrong and reallocation cost would be incurred silently.
    #[inline]
    pub(crate) fn serialized_size(&self) -> usize {
        let table_bytes = self
            .table
            .len()
            .checked_mul(4)
            .expect("CodeMapper::serialized_size: table size overflows usize");
        let reverse_bytes = self
            .reverse_table
            .len()
            .checked_mul(4)
            .expect("CodeMapper::serialized_size: reverse_table size overflows usize");
        12usize
            .checked_add(table_bytes)
            .and_then(|s| s.checked_add(reverse_bytes))
            .expect("CodeMapper::serialized_size: total exceeds usize::MAX")
    }

    /// Writes the serialised CodeMapper directly into `buf`.
    ///
    /// This avoids the intermediate `Vec<u8>` allocation that `as_bytes()` performs.
    pub(crate) fn write_to(&self, buf: &mut Vec<u8>) {
        // Reserve up-front so the per-element `extend_from_slice` calls
        // never reallocate. `serialized_size()` already validates the
        // arithmetic, so subtracting the 12-byte header gives a payload
        // length that is guaranteed to fit. `extend_from_slice` of a
        // 4-byte array compiles to a length check + memcpy + length
        // update — no zero-init pass like `resize(_, 0)` would incur.
        let payload = self.serialized_size() - 12;
        buf.reserve(payload + 12);

        buf.extend_from_slice(&(self.table.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(self.reverse_table.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.alphabet_size.to_le_bytes());

        // `to_le_bytes` makes the byte order explicit rather than relying
        // on in-memory representation, so the code stays correct if the
        // LE compile-time guard is ever relaxed.
        for &v in &self.table {
            buf.extend_from_slice(&v.to_le_bytes());
        }
        for &v in &self.reverse_table {
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }

    /// Serializes the CodeMapper to bytes.
    pub fn as_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.serialized_size());
        self.write_to(&mut buf);
        buf
    }

    /// Deserializes a CodeMapper from bytes. Returns the CodeMapper and the number of bytes consumed.
    pub fn from_bytes(bytes: &[u8]) -> Option<(Self, usize)> {
        if bytes.len() < 12 {
            return None;
        }
        let table_len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let reverse_len = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let alphabet_size = u32::from_le_bytes(bytes[8..12].try_into().unwrap());

        // Use checked arithmetic to prevent overflow on 32-bit platforms
        let table_bytes = table_len.checked_mul(4)?;
        let reverse_bytes = reverse_len.checked_mul(4)?;
        let data_size = table_bytes.checked_add(reverse_bytes)?;
        let total = data_size.checked_add(12)?;
        if bytes.len() < total {
            return None;
        }

        let mut offset = 12;

        // Decode each u32 explicitly via `from_le_bytes`. Slower than a raw
        // memcpy over the full region, but (a) `from_bytes` is a one-time
        // setup cost — not in the query hot path — and (b) the explicit
        // byte order keeps the code correct regardless of host endianness.
        let table: Vec<u32> = bytes[offset..offset + table_bytes]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        offset += table_bytes;

        let reverse_table: Vec<u32> = bytes[offset..offset + reverse_bytes]
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        offset += reverse_bytes;

        Some((
            Self {
                table,
                reverse_table,
                alphabet_size,
            },
            offset,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_keys() {
        let keys: Vec<Vec<u8>> = vec![];
        let cm = CodeMapper::build(&keys);
        assert_eq!(cm.alphabet_size(), 1); // only terminal
    }

    #[test]
    fn frequency_order() {
        // 'a' appears 3 times, 'b' appears 1 time
        let keys: Vec<Vec<u8>> = vec![vec![b'a', b'a', b'a'], vec![b'b']];
        let cm = CodeMapper::build(&keys);

        let code_a = cm.get(b'a');
        let code_b = cm.get(b'b');

        // 'a' is more frequent, so it should get a smaller code
        assert!(code_a < code_b);
        assert_ne!(code_a, 0); // code 0 is reserved
        assert_ne!(code_b, 0);
    }

    #[test]
    fn code_zero_reserved() {
        let keys: Vec<Vec<u8>> = vec![vec![b'x']];
        let cm = CodeMapper::build(&keys);
        // No label should map to code 0
        assert_ne!(cm.get(b'x'), 0);
    }

    #[test]
    fn unmapped_label_returns_zero() {
        let keys: Vec<Vec<u8>> = vec![vec![b'a']];
        let cm = CodeMapper::build(&keys);
        assert_eq!(cm.get(b'z'), 0);
    }

    #[test]
    fn reverse_round_trip() {
        let keys: Vec<Vec<u8>> = vec![vec![b'a', b'b', b'c'], vec![b'd', b'e']];
        let cm = CodeMapper::build(&keys);

        for label in [b'a', b'b', b'c', b'd', b'e'] {
            let code = cm.get(label);
            assert_ne!(code, 0);
            let back = cm.reverse(code).expect("mapped code must reverse");
            assert_eq!(back, label as u32);
        }
    }

    #[test]
    fn reverse_out_of_range_is_none() {
        let keys: Vec<Vec<u8>> = vec![vec![b'a', b'b']];
        let cm = CodeMapper::build(&keys);
        // alphabet_size = 3 (terminal + 'a' + 'b'), so code 3+ is out of range
        assert_eq!(cm.reverse(3), None);
        assert_eq!(cm.reverse(u32::MAX), None);
    }

    #[test]
    fn char_labels() {
        let keys: Vec<Vec<char>> = vec![vec!['あ', 'い'], vec!['う', 'え', 'お'], vec!['あ', 'お']];
        let cm = CodeMapper::build(&keys);

        // 'あ' and 'お' appear 2 times each, others 1 time
        let code_a = cm.get('あ');
        let code_u = cm.get('う');
        assert_ne!(code_a, 0);
        assert_ne!(code_u, 0);

        // Round trip
        assert_eq!(cm.reverse(code_a), Some('あ' as u32));
        assert_eq!(cm.reverse(code_u), Some('う' as u32));
    }

    #[test]
    fn as_bytes_from_bytes_round_trip() {
        let keys: Vec<Vec<u8>> = vec![
            vec![b'h', b'e', b'l', b'l', b'o'],
            vec![b'w', b'o', b'r', b'l', b'd'],
        ];
        let cm = CodeMapper::build(&keys);
        let bytes = cm.as_bytes();
        let (cm2, consumed) = CodeMapper::from_bytes(&bytes).unwrap();

        assert_eq!(consumed, bytes.len());
        assert_eq!(cm.alphabet_size(), cm2.alphabet_size());

        for label in [b'h', b'e', b'l', b'o', b'w', b'r', b'd'] {
            assert_eq!(cm.get(label), cm2.get(label));
        }
    }

    #[test]
    fn from_bytes_too_short() {
        assert!(CodeMapper::from_bytes(&[0; 8]).is_none());
    }

    #[test]
    fn build_with_supplementary_plane_chars() {
        // Exercises the sparse/HashMap path: emoji sit well above
        // FREQ_DENSE_THRESHOLD (65_536), so `max_label` forces the HashMap
        // branch. Behaviour must be identical to the dense path.
        let keys: Vec<Vec<char>> = vec![vec!['あ', '\u{1F600}'], vec!['\u{1F600}', '\u{1F680}']];
        let cm = CodeMapper::build(&keys);
        // 3 distinct labels + terminal.
        assert_eq!(cm.alphabet_size(), 4);
        // '\u{1F600}' appears twice → smaller code than the others.
        let code_smile = cm.get('\u{1F600}');
        let code_rocket = cm.get('\u{1F680}');
        assert_ne!(code_smile, 0);
        assert!(code_smile < code_rocket);
        // Round trip still works.
        assert_eq!(cm.reverse(code_smile), Some('\u{1F600}' as u32));
        assert_eq!(cm.reverse(code_rocket), Some('\u{1F680}' as u32));
    }
}
