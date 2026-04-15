//! Shared test helpers.
//!
//! Kept behind `#[cfg(test)]` at the crate root and consumed by the
//! per-module `mod tests` blocks, so this never contributes to the
//! released library.

/// 8-byte aligned byte buffer for `from_bytes` tests.
///
/// `Vec<u8>` only guarantees 1-byte alignment. Standard allocators
/// happen to return 8- or 16-byte aligned memory, but Miri's allocator
/// does not. Backing storage as `Vec<u64>` guarantees 8-byte alignment,
/// which comfortably satisfies the 4-byte alignment the zero-copy
/// loader requires.
///
/// Implements both `as_slice(&self) -> &[u8]` (for call sites that want
/// an explicit method) and `AsRef<[u8]>` (for APIs that take
/// `impl AsRef<[u8]>`, such as `DoubleArrayBacked::from_backing`).
#[derive(Clone)]
pub(crate) struct AlignedBytes {
    buf: Vec<u64>,
    len: usize,
}

impl AlignedBytes {
    pub(crate) fn new(bytes: &[u8]) -> Self {
        let n = bytes.len().div_ceil(8);
        let mut buf = vec![0u64; n];
        // SAFETY: copying `bytes.len()` bytes into `buf`, which holds at
        // least that many (`n * 8 >= bytes.len()`); `u64` has no invalid
        // bit patterns.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf.as_mut_ptr() as *mut u8, bytes.len());
        }
        Self {
            buf,
            len: bytes.len(),
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        // SAFETY: `self.len <= self.buf.len() * 8`; `u64` alignment (8)
        // satisfies all downstream alignment requirements (max align 4).
        unsafe { std::slice::from_raw_parts(self.buf.as_ptr() as *const u8, self.len) }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }
}

impl AsRef<[u8]> for AlignedBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

// SAFETY: `AlignedBytes` stores its payload in a `Vec<u64>`, whose heap
// buffer address is stable across moves of the struct. The slice returned
// by `as_ref` points into that heap allocation.
unsafe impl crate::StableBacking for AlignedBytes {}
