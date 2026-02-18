use crate::Label;

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

        // Direct frequency counting — avoids HashMap overhead.
        let mut freq = vec![0u64; table_size];
        for key in keys {
            for &label in key.as_ref() {
                freq[<L as Into<u32>>::into(label) as usize] += 1;
            }
        }

        // Collect (label, freq) pairs for non-zero entries
        let mut labels: Vec<(u32, u64)> = freq
            .iter()
            .enumerate()
            .filter(|(_, &f)| f > 0)
            .map(|(i, &f)| (i as u32, f))
            .collect();

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

    /// Returns the label (as u32) for a code. Code 0 is the terminal symbol.
    #[inline]
    pub fn reverse(&self, code: u32) -> u32 {
        debug_assert!(
            (code as usize) < self.reverse_table.len(),
            "code {code} out of bounds (alphabet_size {})",
            self.alphabet_size
        );
        self.reverse_table[code as usize]
    }

    /// The number of distinct codes including the terminal symbol.
    #[inline]
    pub fn alphabet_size(&self) -> u32 {
        self.alphabet_size
    }

    /// Returns the serialised size in bytes (without allocating).
    #[inline]
    pub(crate) fn serialized_size(&self) -> usize {
        12 + (self.table.len() + self.reverse_table.len()) * 4
    }

    /// Writes the serialised CodeMapper directly into `buf`.
    ///
    /// This avoids the intermediate `Vec<u8>` allocation that `as_bytes()` performs.
    pub(crate) fn write_to(&self, buf: &mut Vec<u8>) {
        buf.extend_from_slice(&(self.table.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(self.reverse_table.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.alphabet_size.to_le_bytes());
        // SAFETY: u32 has no padding; LE platform is enforced by the crate-level compile_error.
        unsafe {
            buf.extend_from_slice(std::slice::from_raw_parts(
                self.table.as_ptr() as *const u8,
                self.table.len() * 4,
            ));
            buf.extend_from_slice(std::slice::from_raw_parts(
                self.reverse_table.as_ptr() as *const u8,
                self.reverse_table.len() * 4,
            ));
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

        // SAFETY: u32 has no padding; LE layout matches serialised format.
        // with_capacity + set_len avoids redundant zero-initialisation.
        let table = unsafe {
            let mut v = Vec::<u32>::with_capacity(table_len);
            std::ptr::copy_nonoverlapping(
                bytes[offset..].as_ptr(),
                v.as_mut_ptr() as *mut u8,
                table_bytes,
            );
            v.set_len(table_len);
            v
        };
        offset += table_bytes;

        let reverse_table = unsafe {
            let mut v = Vec::<u32>::with_capacity(reverse_len);
            std::ptr::copy_nonoverlapping(
                bytes[offset..].as_ptr(),
                v.as_mut_ptr() as *mut u8,
                reverse_bytes,
            );
            v.set_len(reverse_len);
            v
        };
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
            let back = cm.reverse(code);
            assert_eq!(back, label as u32);
        }
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
        assert_eq!(cm.reverse(code_a), 'あ' as u32);
        assert_eq!(cm.reverse(code_u), 'う' as u32);
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
}
