//! Bit reader. Inverse of [`BitWriter`].
//!
//! Mirrors `unlzx.c`'s `control` / `shift` pair (ALGORITHM.md §10):
//!
//! ```text
//! shift starts at -16
//! consume(n): value = control & ((1<<n)-1); control >>= n; shift -= n;
//!             if shift < 0 refill
//! refill: shift += 16
//!         control += hi_byte << (8 + shift)
//!         control += lo_byte << shift
//! ```

use std::io::Read;

use crate::{Error, Result};

pub struct BitReader<R: Read> {
    inner: R,
    /// Bit accumulator. The next bit to consume is in bit 0.
    control: u32,
    /// Number of valid bits in `control` minus 16. Starts at -16 so the
    /// first refill loads two bytes into the high half of the buffer.
    shift: i32,
    /// Bytes consumed from the inner reader so far.
    bytes_read: u64,
    /// Sticky end-of-stream marker. After we hit EOF we still allow reads
    /// up to the bits already in the buffer.
    eof: bool,
}

impl<R: Read> BitReader<R> {
    pub fn new(inner: R) -> Self {
        BitReader {
            inner,
            control: 0,
            shift: -16,
            bytes_read: 0,
            eof: false,
        }
    }

    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    /// Pull the next 16-bit big-endian word from the inner reader and merge
    /// it into the accumulator. Sets `eof` if no more bytes are available.
    fn refill(&mut self) -> Result<()> {
        if self.eof {
            // We're already drained; let consumers see whatever's left in
            // the buffer and decide.
            self.shift += 16;
            return Ok(());
        }
        let mut buf = [0u8; 2];
        match read_exact_or_eof(&mut self.inner, &mut buf)? {
            2 => {
                self.bytes_read += 2;
                self.shift += 16;
                let s = self.shift;
                // hi byte goes to bit (8 + s), lo byte to bit s.
                self.control = self
                    .control
                    .wrapping_add((buf[0] as u32) << (8 + s))
                    .wrapping_add((buf[1] as u32) << s);
            }
            n => {
                // Odd byte at end of stream: account for it then mark EOF.
                self.bytes_read += n as u64;
                self.eof = true;
                self.shift += 16;
                if n == 1 {
                    let s = self.shift;
                    self.control = self.control.wrapping_add((buf[0] as u32) << (8 + s));
                }
            }
        }
        Ok(())
    }

    /// Read the low `n` bits of the next code without consuming them. The
    /// requested width must satisfy `0..=16`; `n == 0` is a no-op that
    /// returns 0 (used by the aligned-offset path when `pbits == 3` and
    /// `top_bits = pbits - 3` is zero). Triggers refills as needed.
    pub fn peek_bits(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 16);
        if n == 0 {
            return Ok(0);
        }
        // Make sure at least 16 bits are present (after ≥1 refill the
        // accumulator always holds 16+ valid bits unless we hit EOF).
        while self.shift < 0 {
            self.refill()?;
        }
        let mask = (1u32 << n) - 1;
        Ok(self.control & mask)
    }

    /// Consume `n` bits, advancing the bit cursor. `n` may be 0.
    pub fn consume_bits(&mut self, n: u32) -> Result<()> {
        debug_assert!(n <= 16);
        if n == 0 {
            return Ok(());
        }
        self.control >>= n;
        self.shift -= n as i32;
        if self.shift < 0 {
            self.refill()?;
        }
        Ok(())
    }

    /// Read and consume `n` bits in one call.
    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u32> {
        let v = self.peek_bits(n)?;
        self.consume_bits(n)?;
        Ok(v)
    }

    /// Convenience: read N bits and return as u8. N must be ≤8.
    #[inline]
    pub fn read_bits_u8(&mut self, n: u32) -> Result<u8> {
        debug_assert!(n <= 8);
        Ok(self.read_bits(n)? as u8)
    }
}

/// Read exactly the requested number of bytes, or return how many were
/// actually read on early EOF. Wraps `io::ErrorKind::UnexpectedEof` into a
/// short read instead of an error so callers can detect end-of-stream
/// gracefully.
fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(filled),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitio::BitWriter;

    #[test]
    fn read_back_three_bit_header() {
        let mut w = BitWriter::new(Vec::new());
        w.write_bits(0b011, 3).unwrap();
        let (bytes, _) = w.finish().unwrap();

        let mut r = BitReader::new(std::io::Cursor::new(bytes));
        assert_eq!(r.read_bits(3).unwrap(), 0b011);
    }

    #[test]
    fn round_trip_mixed_widths() {
        let writes: &[(u32, u32)] = &[
            (0b1, 1),
            (0b0101, 4),
            (0xabcd, 16),
            (0x1f, 5),
            (0xfa, 8),
            (0x123, 9),
        ];
        let mut w = BitWriter::new(Vec::new());
        for &(v, n) in writes {
            w.write_bits(v, n).unwrap();
        }
        let (bytes, _) = w.finish().unwrap();

        let mut r = BitReader::new(std::io::Cursor::new(bytes));
        for &(v, n) in writes {
            assert_eq!(r.read_bits(n).unwrap(), v, "width {n}");
        }
    }

    #[test]
    fn long_random_round_trip() {
        // Deterministic xorshift PRNG so this stays a fast unit test.
        let mut state: u64 = 0x00c0_ffee_1234_5678;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut writes = Vec::new();
        for _ in 0..2000 {
            let n = ((next() % 16) + 1) as u32;
            let v = (next() as u32) & ((1u32 << n) - 1);
            writes.push((v, n));
        }
        let mut w = BitWriter::new(Vec::new());
        for &(v, n) in &writes {
            w.write_bits(v, n).unwrap();
        }
        let (bytes, _) = w.finish().unwrap();
        let mut r = BitReader::new(std::io::Cursor::new(bytes));
        for &(v, n) in &writes {
            assert_eq!(r.read_bits(n).unwrap(), v);
        }
    }
}
