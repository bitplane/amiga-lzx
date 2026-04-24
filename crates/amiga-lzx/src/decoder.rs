//! LZX stream decoder, ported from `unlzx.c` (`read_literal_table`,
//! `decrunch`).
//!
//! Reads a sequence of LZX blocks from a [`crate::bitio::BitReader`] and
//! writes decoded bytes into a caller-provided buffer. The decoder owns a
//! 64 KB circular window so match copies can read from previously emitted
//! bytes without bounds checks.

use std::io::Read;

use crate::bitio::BitReader;
use crate::constants::{
    ALIGNED_SYMBOLS, LITERAL_SYMBOLS, MAIN_SYMBOLS, MIN_MATCH, TABLE_ONE, TABLE_THREE, TABLE_TWO,
};
use crate::error::{Error, Result};
use crate::huffman::decode::{decode_symbol, make_decode_table};
use crate::huffman::pretree::{decode_section, Section};

const WINDOW_SIZE: usize = 0x10000; // 64 KB
const WINDOW_MASK: usize = WINDOW_SIZE - 1;
const MAIN_TABLE_BITS: u32 = 12;
const ALIGNED_TABLE_BITS: u32 = 7;

/// Streaming decoder. Holds persistent code-length arrays, the decode
/// tables, the last-offset cache, and the circular window.
pub struct Decoder<R: Read> {
    reader: BitReader<R>,
    /// Main-tree code lengths. Persistent across blocks; zero at stream start.
    literal_len: Vec<u8>,
    /// Aligned-offset code lengths. Only refreshed on type-3 blocks.
    offset_len: Vec<u8>,
    /// Decode tables built from the lengths above. Rebuilt at each block
    /// that carries a new tree.
    literal_table: Vec<u16>,
    offset_table: Vec<u16>,
    /// Block type of the *current* block (1, 2, or 3).
    decrunch_method: u8,
    /// Number of decoded bytes still owed by the current block.
    block_remaining: u32,
    /// Last-offset cache, used by position slot 0. Initialised to 1.
    last_offset: u32,
    /// 64 KB circular window of recently emitted bytes.
    window: Vec<u8>,
    /// Cursor into [`Self::window`].
    window_pos: usize,
    /// Have we read the first block header yet?
    primed: bool,
}

impl<R: Read> Decoder<R> {
    pub fn new(reader: R) -> Self {
        Decoder {
            reader: BitReader::new(reader),
            literal_len: vec![0u8; MAIN_SYMBOLS],
            offset_len: vec![0u8; ALIGNED_SYMBOLS],
            literal_table: Vec::new(),
            offset_table: Vec::new(),
            decrunch_method: 0,
            block_remaining: 0,
            last_offset: 1,
            window: vec![0u8; WINDOW_SIZE],
            window_pos: 0,
            primed: false,
        }
    }

    /// Decode `expected` bytes into the caller-supplied vector. Reads as
    /// many blocks as needed to satisfy the request. Returns `Ok(())` on
    /// success; an error if the stream runs out before producing
    /// `expected` bytes.
    pub fn decode_into(&mut self, out: &mut Vec<u8>, expected: usize) -> Result<()> {
        let target = out.len() + expected;
        while out.len() < target {
            if self.block_remaining == 0 {
                self.read_block_header()?;
                if self.block_remaining == 0 {
                    return Err(Error::Truncated);
                }
            }
            self.decode_some(out, target)?;
        }
        Ok(())
    }

    fn read_block_header(&mut self) -> Result<()> {
        // 3 bits: block type.
        let method = self.reader.read_bits(3)? as u8;
        if method == 0 || method > 3 {
            return Err(Error::InvalidArchive("unknown block type"));
        }
        self.decrunch_method = method;

        // Type 3: 8 × 3 bits aligned-offset code lengths, then build table.
        if method == 3 {
            for slot in self.offset_len.iter_mut() {
                *slot = self.reader.read_bits_u8(3)?;
            }
            self.offset_table =
                make_decode_table(ALIGNED_SYMBOLS, ALIGNED_TABLE_BITS, &self.offset_len)?;
        }

        // 3 × 8 bits source length, MSB byte first.
        let mut len: u32 = 0;
        len |= self.reader.read_bits(8)? << 16;
        len |= self.reader.read_bits(8)? << 8;
        len |= self.reader.read_bits(8)?;
        self.block_remaining = len;

        // Tree section, unless type 1 (reuse previous).
        if method != 1 {
            // Decode literal section (256 entries) then match section (512).
            let (lit, mat) = self.literal_len.split_at_mut(LITERAL_SYMBOLS);
            decode_section(&mut self.reader, Section::Literal, lit)?;
            decode_section(&mut self.reader, Section::Match, mat)?;
            self.literal_table =
                make_decode_table(MAIN_SYMBOLS, MAIN_TABLE_BITS, &self.literal_len)?;
        }

        self.primed = true;
        Ok(())
    }

    fn decode_some(&mut self, out: &mut Vec<u8>, target: usize) -> Result<()> {
        while self.block_remaining > 0 && out.len() < target {
            let symbol = decode_symbol(
                &mut self.reader,
                &self.literal_table,
                &self.literal_len,
                MAIN_SYMBOLS,
                MAIN_TABLE_BITS,
            )?;

            if (symbol as usize) < LITERAL_SYMBOLS {
                let b = symbol as u8;
                out.push(b);
                self.window[self.window_pos] = b;
                self.window_pos = (self.window_pos + 1) & WINDOW_MASK;
                self.block_remaining -= 1;
            } else {
                // Match symbol = 256 + (length_slot << 5) + position_slot.
                let m = symbol - LITERAL_SYMBOLS as u16;
                let position_slot = (m & 0x1f) as usize;
                let length_slot = ((m >> 5) & 0xf) as usize;

                // Decode position.
                let mut distance = TABLE_TWO[position_slot];
                let pbits = TABLE_ONE[position_slot] as u32;

                if pbits >= 3 && self.decrunch_method == 3 {
                    // Aligned offset: top (pbits - 3) bits raw, low 3 bits
                    // through the aligned tree.
                    let top_bits = pbits - 3;
                    let top = self.reader.read_bits(top_bits)?;
                    distance += top << 3;
                    let aligned = decode_symbol(
                        &mut self.reader,
                        &self.offset_table,
                        &self.offset_len,
                        ALIGNED_SYMBOLS,
                        ALIGNED_TABLE_BITS,
                    )?;
                    distance += aligned as u32;
                } else if pbits > 0 {
                    let footer = self.reader.read_bits(pbits)?;
                    distance += footer;
                }

                // Slot 0 with no extra bits → use last_offset cache.
                let dist = if distance == 0 {
                    self.last_offset
                } else {
                    distance
                };
                if dist == 0 {
                    return Err(Error::InvalidArchive("zero match distance"));
                }
                self.last_offset = dist;

                // Decode length.
                let lbits = TABLE_ONE[length_slot] as u32;
                let mut length = TABLE_TWO[length_slot] as usize + MIN_MATCH;
                if lbits > 0 {
                    length += self.reader.read_bits(lbits)? as usize;
                }

                if length as u32 > self.block_remaining {
                    return Err(Error::InvalidArchive("match runs past block end"));
                }
                if out.len() + length > target {
                    return Err(Error::InvalidArchive(
                        "decoded data exceeds expected length",
                    ));
                }

                // Copy `length` bytes from `dist` behind window_pos.
                for _ in 0..length {
                    let src = (self.window_pos + WINDOW_SIZE - dist as usize) & WINDOW_MASK;
                    let b = self.window[src];
                    out.push(b);
                    self.window[self.window_pos] = b;
                    self.window_pos = (self.window_pos + 1) & WINDOW_MASK;
                }
                self.block_remaining -= length as u32;
            }
        }
        let _ = TABLE_THREE; // not used directly here; kept for reference
        Ok(())
    }
}

/// One-shot helper: decode `expected` bytes from a slice. Used in tests
/// and as the engine of the archive-layer reader.
pub fn decode(input: &[u8], expected: usize) -> Result<Vec<u8>> {
    let mut dec = Decoder::new(std::io::Cursor::new(input));
    let mut out = Vec::with_capacity(expected);
    dec.decode_into(&mut out, expected)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BlockWriter;
    use crate::constants::{LEVEL_MAX, LEVEL_NORMAL, LEVEL_QUICK};
    use crate::lz77::encode as lz77_encode;

    fn round_trip(input: &[u8]) {
        for level in [LEVEL_QUICK, LEVEL_NORMAL, LEVEL_MAX] {
            let tokens = lz77_encode(input, &level);
            let mut bw = BlockWriter::new(Vec::new());
            bw.write_block(&tokens).unwrap();
            let (bytes, _) = bw.finish().unwrap();

            let decoded = decode(&bytes, input.len()).unwrap();
            assert_eq!(decoded, input, "level {:?}", level);
        }
    }

    #[test]
    fn empty_round_trip_is_empty() {
        // Empty input → no tokens → no blocks. decode() with expected=0
        // should succeed without consuming any input.
        let result = decode(&[], 0).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn single_literal() {
        round_trip(b"x");
    }

    #[test]
    fn short_text() {
        round_trip(b"hello, world");
    }

    #[test]
    fn lorem_ipsum() {
        let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna \
                     aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
                     ullamco laboris nisi ut aliquip ex ea commodo consequat.";
        round_trip(data);
    }

    #[test]
    fn long_zeros() {
        let data = vec![0u8; 8192];
        round_trip(&data);
    }

    #[test]
    fn periodic_pattern() {
        let mut data = Vec::new();
        for _ in 0..500 {
            data.extend_from_slice(b"ABCDEFG ");
        }
        round_trip(&data);
    }

    #[test]
    fn random_bytes() {
        // Deterministic xorshift PRNG.
        let mut s: u64 = 0xfeed_face_dead_beef;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut data = vec![0u8; 16 * 1024];
        for b in data.iter_mut() {
            *b = (next() & 0xff) as u8;
        }
        round_trip(&data);
    }

    #[test]
    fn all_byte_values_present() {
        let data: Vec<u8> = (0..=255u8).cycle().take(2000).collect();
        round_trip(&data);
    }

    #[test]
    fn match_that_exceeds_expected_size_is_rejected() {
        let input = vec![0u8; 100];
        let tokens = lz77_encode(&input, &LEVEL_NORMAL);
        let mut bw = BlockWriter::new(Vec::new());
        bw.write_block(&tokens).unwrap();
        let (bytes, _) = bw.finish().unwrap();

        let err = decode(&bytes, 10).unwrap_err();
        assert!(matches!(
            err,
            Error::InvalidArchive("decoded data exceeds expected length")
        ));
    }
}
