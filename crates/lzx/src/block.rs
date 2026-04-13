//! Block writer.
//!
//! Takes an LZ77 token stream and emits an LZX block to a [`BitWriter`].
//! For v1 we always emit type-2 (verbatim with new tree) blocks; type 3
//! (aligned offset) is a deferred optimisation. See ALGORITHM.md §7..§9.
//!
//! The encoder maintains `main_lengths` across blocks because the
//! pretree encoder computes deltas against the previous block's lengths.
//! At stream start, main_lengths is all zeros.

use std::io::Write;

use crate::bitio::BitWriter;
use crate::constants::{
    build_slot_lookup, length_slot, position_slot, LITERAL_SYMBOLS, MAIN_SYMBOLS, MATCH_SYMBOLS,
    SLOT_LOOKUP_LEN, TABLE_ONE, TABLE_THREE,
};
use crate::error::Result;
use crate::huffman::build::{build_lengths, canonical_codes};
use crate::huffman::pretree::{encode_section, Section};
use crate::lz77::Token;

/// LZX block type the writer emits. Currently only Verbatim is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// Type 2 — verbatim block with a fresh main tree.
    Verbatim = 2,
}

pub struct BlockWriter<W: Write> {
    pub(crate) bit_writer: BitWriter<W>,
    /// Persistent main-tree code lengths. Updated after every block; the
    /// next block's pretree encoder uses these as the "previous" baseline.
    main_lengths: Vec<u8>,
    /// Cached distance-slot lookup.
    slot_lookup: [u8; SLOT_LOOKUP_LEN],
}

impl<W: Write> BlockWriter<W> {
    pub fn new(writer: W) -> Self {
        BlockWriter {
            bit_writer: BitWriter::new(writer),
            main_lengths: vec![0u8; MAIN_SYMBOLS],
            slot_lookup: build_slot_lookup(),
        }
    }

    /// Number of bytes written to the inner writer so far (excluding any
    /// pending bits in the bit buffer).
    pub fn bytes_written(&self) -> u64 {
        self.bit_writer.bytes_written()
    }

    /// Emit a single block from the given tokens. The tokens are
    /// interpreted left-to-right as literals and matches.
    pub fn write_block(&mut self, tokens: &[Token]) -> Result<()> {
        if tokens.is_empty() {
            return Ok(());
        }

        // 1. Compute symbol frequencies.
        let freqs = self.collect_freqs(tokens);
        // Block source length (decoded bytes).
        let source_len: usize = tokens
            .iter()
            .map(|t| match t {
                Token::Literal(_) => 1usize,
                Token::Match { length, .. } => *length as usize,
            })
            .sum();
        debug_assert!(source_len < (1 << 24), "block source length overflow 24 bits");

        // 2. Build the new main-tree code lengths from the freqs. Length
        // limit 16. Save the previous lengths first so the pretree
        // delta-encoder has the right baseline.
        let prev_lengths = self.main_lengths.clone();
        let new_lengths = build_lengths(&freqs, 16);
        debug_assert_eq!(new_lengths.len(), MAIN_SYMBOLS);
        self.main_lengths = new_lengths.clone();

        // 3. Emit block header: 3 bits block type, then 24 bits source length
        //    (most-significant byte first).
        self.bit_writer.write_bits(BlockType::Verbatim as u32, 3)?;
        let len = source_len as u32;
        self.bit_writer.write_bits((len >> 16) & 0xff, 8)?;
        self.bit_writer.write_bits((len >> 8) & 0xff, 8)?;
        self.bit_writer.write_bits(len & 0xff, 8)?;

        // 4. Emit the main tree: literal section (0..256), then match
        //    section (256..768), each pretree-compressed against the
        //    corresponding slice of prev_lengths.
        encode_section(
            &mut self.bit_writer,
            Section::Literal,
            &prev_lengths[0..LITERAL_SYMBOLS],
            &new_lengths[0..LITERAL_SYMBOLS],
        )?;
        encode_section(
            &mut self.bit_writer,
            Section::Match,
            &prev_lengths[LITERAL_SYMBOLS..],
            &new_lengths[LITERAL_SYMBOLS..],
        )?;

        // 5. Build canonical codes from the new lengths.
        let (_codes, reversed) = canonical_codes(&new_lengths);

        // 6. Emit the body — Huffman codes for each token, plus footer bits
        //    for matches.
        for tok in tokens {
            match *tok {
                Token::Literal(b) => {
                    let sym = b as usize;
                    self.bit_writer
                        .write_bits(reversed[sym], new_lengths[sym] as u32)?;
                }
                Token::Match { length, distance } => {
                    let raw_len = length as usize;
                    let raw_pos = distance as u32;
                    let pslot = position_slot(&self.slot_lookup, raw_pos) as usize;
                    let lslot = length_slot(&self.slot_lookup, raw_len) as usize;
                    let sym = LITERAL_SYMBOLS + (lslot << 5) + pslot;
                    self.bit_writer
                        .write_bits(reversed[sym], new_lengths[sym] as u32)?;

                    // Position footer first, then length footer.
                    let pbits = TABLE_ONE[pslot] as u32;
                    if pbits > 0 {
                        let mask = TABLE_THREE[pbits as usize];
                        self.bit_writer.write_bits(raw_pos & mask, pbits)?;
                    }
                    let lbits = TABLE_ONE[lslot] as u32;
                    if lbits > 0 {
                        let mask = TABLE_THREE[lbits as usize];
                        let v = ((raw_len - crate::constants::MIN_MATCH) as u32) & mask;
                        self.bit_writer.write_bits(v, lbits)?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Pad the bit stream to the next 16-bit word boundary and return the
    /// inner writer plus total bytes written.
    pub fn finish(self) -> Result<(W, u64)> {
        self.bit_writer.finish()
    }

    fn collect_freqs(&self, tokens: &[Token]) -> Vec<u32> {
        let mut freqs = vec![0u32; MAIN_SYMBOLS];
        for tok in tokens {
            match *tok {
                Token::Literal(b) => freqs[b as usize] += 1,
                Token::Match { length, distance } => {
                    let raw_len = length as usize;
                    let raw_pos = distance as u32;
                    let pslot = position_slot(&self.slot_lookup, raw_pos) as usize;
                    let lslot = length_slot(&self.slot_lookup, raw_len) as usize;
                    let sym = LITERAL_SYMBOLS + (lslot << 5) + pslot;
                    freqs[sym] += 1;
                }
            }
        }
        let _ = MATCH_SYMBOLS; // silence unused-import warning when refactored
        freqs
    }
}
