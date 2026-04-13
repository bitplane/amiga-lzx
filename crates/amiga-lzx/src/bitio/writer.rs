//! Bit writer.
//!
//! Output is a stream of 16-bit big-endian words. Bits accumulate in a
//! 32-bit buffer LSB-first; when 16+ bits are pending, the low 16 are
//! emitted as a big-endian word and shifted out. Mirrors the writer in
//! ALGORITHM.md §10.

use std::io::Write;

use crate::Result;

pub struct BitWriter<W: Write> {
    inner: W,
    /// Pending bits, packed LSB-first.
    buffer: u32,
    /// Number of valid bits currently in `buffer`.
    bit_count: u32,
    /// Bytes written to the inner writer so far.
    bytes_written: u64,
}

impl<W: Write> BitWriter<W> {
    pub fn new(inner: W) -> Self {
        BitWriter {
            inner,
            buffer: 0,
            bit_count: 0,
            bytes_written: 0,
        }
    }

    /// Number of bytes written through to the inner writer (whole 16-bit
    /// words only — pending bits are not counted until flushed).
    pub fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Pending bit count, only useful for invariant checks.
    pub fn pending_bits(&self) -> u32 {
        self.bit_count
    }

    /// Write the low `n` bits of `value`. `n` must be in `1..=24` so the
    /// 32-bit buffer can never overflow (max prior content 15 bits).
    pub fn write_bits(&mut self, value: u32, n: u32) -> Result<()> {
        debug_assert!((1..=24).contains(&n), "write_bits n out of range: {n}");
        debug_assert!(
            n == 32 || value < (1u32 << n),
            "value {value} doesn't fit in {n} bits"
        );
        self.buffer |= value << self.bit_count;
        self.bit_count += n;
        while self.bit_count >= 16 {
            let word = (self.buffer & 0xffff) as u16;
            // Big-endian on disk.
            self.inner.write_all(&word.to_be_bytes())?;
            self.bytes_written += 2;
            self.buffer >>= 16;
            self.bit_count -= 16;
        }
        Ok(())
    }

    /// Pad to the next 16-bit word boundary with zero bits and flush.
    /// Idempotent when already aligned.
    pub fn flush_to_word(&mut self) -> Result<()> {
        if self.bit_count > 0 {
            let pad = 16 - self.bit_count;
            // Emit zero pad bits to complete a word.
            self.write_bits(0, pad)?;
        }
        debug_assert_eq!(self.bit_count, 0);
        Ok(())
    }

    /// Finalise the writer: pad to a word boundary and return the inner
    /// writer along with the total bytes emitted.
    pub fn finish(mut self) -> Result<(W, u64)> {
        self.flush_to_word()?;
        Ok((self.inner, self.bytes_written))
    }

    /// Borrow the inner writer (rarely needed; used by archive layer to
    /// patch headers in place).
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_word_pack_lsb_first_in_be_bytes() {
        // Write 0b1011 as 4 bits, then 0b0010 as 4 bits, then pad.
        // Packing LSB-first: buffer becomes 0010_1011 (= 0x2b) in low byte,
        // padded to 16 bits → high byte zero. On disk BE: 00 2b.
        let mut w = BitWriter::new(Vec::new());
        w.write_bits(0b1011, 4).unwrap();
        w.write_bits(0b0010, 4).unwrap();
        let (out, n) = w.finish().unwrap();
        assert_eq!(n, 2);
        assert_eq!(out, vec![0x00, 0x2b]);
    }

    #[test]
    fn three_bit_block_header_at_start() {
        // ALGORITHM.md §10: a 3-bit block type at the start should land in
        // the bottom 3 bits of the second byte of the first word
        // (the "low" byte of the BE word, which is byte index 1).
        let mut w = BitWriter::new(Vec::new());
        w.write_bits(0b011, 3).unwrap(); // type 3
        let (out, _) = w.finish().unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], 0x00);
        assert_eq!(out[1], 0b0000_0011);
    }

    #[test]
    fn writes_full_word_then_partial() {
        let mut w = BitWriter::new(Vec::new());
        w.write_bits(0xabcd, 16).unwrap();
        // After 16 bits, the buffer flushed: bytes are BE 0xab 0xcd.
        // Note: the value 0xabcd is packed LSB-first, so the low bit of 0xabcd
        // (bit 0) becomes bit 0 of the buffer, which is the LSB of the on-disk
        // low byte (byte index 1). So `buffer & 0xffff = 0xabcd`, BE = ab cd.
        w.write_bits(0xff, 8).unwrap();
        let (out, _) = w.finish().unwrap();
        // First word is 0xabcd in BE: ab cd.
        // Second word: low 8 bits = 0xff, padded high 8 bits = 0x00, BE: 00 ff.
        assert_eq!(out, vec![0xab, 0xcd, 0x00, 0xff]);
    }
}
