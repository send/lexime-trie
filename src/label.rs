/// A label type for use as trie keys.
///
/// Labels must be copyable, orderable, and convertible to/from `u32`.
/// The forward conversion (`Into<u32>`) is used to compute codes during
/// build; the reverse (`TryFrom<u32>`) decodes labels when reconstructing
/// keys (for example, during `predictive_search`).
///
/// # Required conversion contract
///
/// For every label value `l` that can appear in a key, converting to
/// `u32` and back must succeed and preserve the original:
///
/// ```text
/// L::try_from(l.into()) == Ok(l)
/// ```
///
/// The pre-blessed impls (`u8`, `char`) satisfy this automatically —
/// `u8::try_from(u8_value as u32)` is always `Ok`, and `char` only
/// round-trips through its documented Unicode scalar range.
///
/// If an external `Label` implementation violates the contract for a
/// label that was stored in the trie, `predictive_search` silently
/// skips affected children rather than panicking or reporting an
/// error — the same treatment given to codes decoded from corrupted
/// buffers. The built trie remains valid for all other queries
/// (`exact_match`, `common_prefix_search`, `probe`, `node_slot_count`)
/// because those paths do not reconstruct labels from codes.
pub trait Label: Copy + Ord + Into<u32> + TryFrom<u32> {}

impl Label for u8 {}

impl Label for char {}

#[cfg(test)]
mod tests {
    #[test]
    fn u8_round_trip() {
        for v in [0u8, 1, 127, 255] {
            let code: u32 = v.into();
            let back = u8::try_from(code).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn char_round_trip() {
        for c in ['a', 'z', 'あ', '漢', '\u{10FFFF}'] {
            let code: u32 = c.into();
            let back = char::try_from(code).unwrap();
            assert_eq!(c, back);
        }
    }
}
