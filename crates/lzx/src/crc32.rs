//! CRC32 (zlib / pkzip).
//!
//! Polynomial `0xedb88320` (reflected), init `0xffffffff`, final xor
//! `0xffffffff`. Both the archive data CRC and the entry header CRC use
//! this algorithm (ALGORITHM.md §6, CONSTANTS.md).

use crate::constants::{CRC32_INIT, CRC32_POLY};

/// Precomputed 256-entry CRC32 table, generated at compile time.
pub const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                CRC32_POLY ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
};

/// Incremental CRC32 state. Stores the running value **without** the final
/// XOR — call [`Crc32::finalize`] to get the published CRC.
#[derive(Debug, Clone, Copy)]
pub struct Crc32(u32);

impl Crc32 {
    #[inline]
    pub const fn new() -> Self {
        Crc32(CRC32_INIT)
    }

    #[inline]
    pub fn update(&mut self, buf: &[u8]) {
        let mut c = self.0;
        for &b in buf {
            c = CRC32_TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
        }
        self.0 = c;
    }

    #[inline]
    pub const fn finalize(self) -> u32 {
        !self.0
    }

    /// The raw running value (pre-complement), used internally when we need
    /// to continue a CRC across a resume point.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Resume from a previously saved raw value.
    #[inline]
    pub const fn from_raw(raw: u32) -> Self {
        Crc32(raw)
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

/// One-shot CRC32 over a slice.
#[inline]
pub fn crc32(buf: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update(buf);
    c.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn vector_123456789() {
        // Standard zlib/pkzip test vector.
        assert_eq!(crc32(b"123456789"), 0xcbf43926);
    }

    #[test]
    fn quick_brown_fox() {
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414fa339);
    }

    #[test]
    fn incremental_matches_oneshot() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let (a, b) = data.split_at(10);
        let mut c = Crc32::new();
        c.update(a);
        c.update(b);
        assert_eq!(c.finalize(), crc32(data));
    }
}
