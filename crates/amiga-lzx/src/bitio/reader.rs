//! Bit reader. Inverse of [`BitWriter`].
//!
//! Loads big-endian 16-bit words and consumes their bits least significant
//! first. Tracks actual buffered bits so EOF cannot manufacture data.

use std::io::Read;

use crate::{Error, Result};

pub struct BitReader<R: Read> {
    inner: R,
    /// Bit accumulator. The next bit to consume is in bit 0.
    control: u32,
    /// Number of real bits available in `control`.
    bit_count: u32,
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
            bit_count: 0,
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
            return Ok(());
        }
        let mut buf = [0u8; 2];
        match read_exact_or_eof(&mut self.inner, &mut buf)? {
            2 => {
                self.bytes_read += 2;
                self.control |= (u16::from_be_bytes(buf) as u32) << self.bit_count;
                self.bit_count += 16;
            }
            n => {
                self.bytes_read += n as u64;
                self.eof = true;
                if n == 1 {
                    // The missing low byte contains the next bits to decode.
                    return Err(Error::Truncated);
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
        self.ensure_bits(n)?;
        if self.bit_count < n {
            return Err(Error::Truncated);
        }
        let mask = (1u32 << n) - 1;
        Ok(self.control & mask)
    }

    fn ensure_bits(&mut self, n: u32) -> Result<()> {
        while self.bit_count < n && !self.eof {
            self.refill()?;
        }
        Ok(())
    }

    /// Huffman tables need a full root index even when the last symbol's
    /// code is shorter than the index width. Zero-extend lookahead only;
    /// consuming the selected code still requires real buffered bits.
    pub(crate) fn peek_bits_padded(&mut self, n: u32) -> Result<u32> {
        debug_assert!(n <= 16);
        if n == 0 {
            return Ok(0);
        }
        self.ensure_bits(n)?;
        if self.bit_count == 0 {
            return Err(Error::Truncated);
        }
        Ok(self.control & ((1u32 << n) - 1))
    }

    /// Consume `n` bits, advancing the bit cursor. `n` may be 0.
    pub fn consume_bits(&mut self, n: u32) -> Result<()> {
        debug_assert!(n <= 16);
        if n == 0 {
            return Ok(());
        }
        self.ensure_bits(n)?;
        if self.bit_count < n {
            return Err(Error::Truncated);
        }
        self.control >>= n;
        self.bit_count -= n;
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
    fn eof_does_not_supply_zero_bits() {
        let mut r = BitReader::new(&b""[..]);
        assert!(matches!(r.read_bits(1), Err(Error::Truncated)));
        assert!(matches!(r.consume_bits(1), Err(Error::Truncated)));
        assert_eq!(r.read_bits(0).unwrap(), 0);

        let mut r = BitReader::new(&[0x12, 0x34][..]);
        assert_eq!(r.read_bits(16).unwrap(), 0x1234);
        assert!(matches!(r.read_bits(1), Err(Error::Truncated)));
        assert!(matches!(r.read_bits(1), Err(Error::Truncated)));
        assert_eq!(r.bytes_read(), 2);
    }

    #[test]
    fn odd_byte_cannot_replace_a_complete_word() {
        let mut r = BitReader::new(&[0x12][..]);
        assert!(matches!(r.read_bits(1), Err(Error::Truncated)));
        assert!(matches!(r.read_bits(1), Err(Error::Truncated)));
        assert_eq!(r.bytes_read(), 1);
    }

    #[test]
    fn padded_lookahead_cannot_be_consumed_as_data() {
        let mut r = BitReader::new(&[0x80, 0x00][..]);
        r.consume_bits(15).unwrap();
        assert_eq!(r.peek_bits_padded(12).unwrap(), 1);
        assert!(matches!(r.consume_bits(2), Err(Error::Truncated)));
        assert_eq!(r.read_bits(1).unwrap(), 1);
        assert!(matches!(r.peek_bits_padded(12), Err(Error::Truncated)));
    }

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
