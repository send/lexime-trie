/// A label type for use as trie keys.
///
/// Labels must be copyable, orderable, and convertible to/from `u32`.
/// `ALPHABET_SIZE` defines the theoretical maximum number of distinct labels.
pub trait Label: Copy + Ord + Into<u32> + TryFrom<u32> {
    const ALPHABET_SIZE: u32;
}

impl Label for u8 {
    const ALPHABET_SIZE: u32 = 256;
}

impl Label for char {
    const ALPHABET_SIZE: u32 = 0x11_0000;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u8_alphabet_size() {
        assert_eq!(u8::ALPHABET_SIZE, 256);
    }

    #[test]
    fn char_alphabet_size() {
        assert_eq!(char::ALPHABET_SIZE, 0x11_0000);
    }

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
