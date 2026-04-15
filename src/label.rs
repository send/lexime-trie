/// A label type for use as trie keys.
///
/// Labels must be copyable, orderable, and convertible to/from `u32`.
/// The forward conversion (`Into<u32>`) is used to compute codes during
/// build; the reverse (`TryFrom<u32>`) decodes labels when iterating
/// search results.
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
