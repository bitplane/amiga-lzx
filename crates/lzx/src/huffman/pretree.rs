//! Pretree-compressed delta encoding of code length arrays.
//!
//! A "pretree" is a small Huffman tree over 20 meta-symbols that encodes
//! the deltas between previous and current code lengths. Two sections
//! exist per main tree: literal (indices 0..256) and match (256..768),
//! each with slightly different run-length parameters. See ALGORITHM.md
//! §8 and CONSTANTS.md "Pretree".

use std::io::Read;

use crate::bitio::{BitReader, BitWriter};
use crate::error::{Error, Result};
use crate::huffman::build::{build_lengths, canonical_codes};
use crate::huffman::decode::{decode_symbol, make_decode_table};

pub const PRETREE_NUM_SYMBOLS: usize = 20;
pub const PRETREE_TABLE_BITS: u32 = 6;
pub const PRETREE_LENGTH_BITS: u32 = 4;

/// Which section of the main tree we're delta-coding. Controls the run
/// thresholds and extra-bit widths for sym 17/18/19.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// Literal range (256 symbols). Run threshold ≥4, sym 17 is 4 bits
    /// (4..19), sym 18 is 5 bits (20..51), sym 19 is 1 bit (4..5).
    Literal,
    /// Match range (512 symbols). Run threshold ≥3, sym 17 is 4 bits
    /// (3..18), sym 18 is 6 bits (19..82), sym 19 is 1 bit (3..4).
    Match,
}

impl Section {
    pub const fn run_threshold(self) -> usize {
        match self {
            Section::Literal => 4,
            Section::Match => 3,
        }
    }
    pub const fn sym17_extra_bits(self) -> u32 {
        4
    }
    pub const fn sym17_base(self) -> usize {
        // Section::Literal → 4..19, Section::Match → 3..18.
        match self {
            Section::Literal => 4,
            Section::Match => 3,
        }
    }
    pub const fn sym18_extra_bits(self) -> u32 {
        match self {
            Section::Literal => 5,
            Section::Match => 6,
        }
    }
    pub const fn sym18_base(self) -> usize {
        match self {
            Section::Literal => 20,
            Section::Match => 19,
        }
    }
    pub const fn sym19_extra_bits(self) -> u32 {
        1
    }
    pub const fn sym19_base(self) -> usize {
        match self {
            Section::Literal => 4,
            Section::Match => 3,
        }
    }
    pub const fn sym17_max(self) -> usize {
        // sym17 covers `base + (1 << extra) - 1` (4 bits → +15).
        self.sym17_base() + (1usize << self.sym17_extra_bits()) - 1
    }
    pub const fn sym18_max(self) -> usize {
        self.sym18_base() + (1usize << self.sym18_extra_bits()) - 1
    }
    pub const fn sym19_max(self) -> usize {
        self.sym19_base() + (1usize << self.sym19_extra_bits()) - 1
    }
}

/// Compute the pretree symbol for `(prev - curr) mod 17`.
#[inline]
fn delta_symbol(prev: u8, curr: u8) -> u8 {
    // (prev - curr) mod 17, with both in 0..=16.
    let p = prev as i32;
    let c = curr as i32;
    let d = (p - c).rem_euclid(17);
    d as u8
}

/// Apply a pretree symbol delta to obtain the new length.
#[inline]
fn apply_delta(prev: u8, sym: u8) -> u8 {
    // new = (prev - sym) mod 17
    let p = prev as i32;
    let s = sym as i32;
    ((p - s).rem_euclid(17)) as u8
}

/// First pass: walk `lengths[range]`, building a frequency histogram of
/// pretree symbols (0..=19). The previous length array (`prev`) is what
/// the delta is computed against — it must hold the lengths from the
/// previous block for delta encoding to be valid.
///
/// Returns `(freqs, encoding_plan)` where `encoding_plan` is the sequence
/// of pretree operations the second pass will emit; we materialise it so
/// the encoder doesn't have to re-walk and re-classify on the second pass.
pub fn analyse_section(
    section: Section,
    prev: &[u8],
    curr: &[u8],
) -> ([u32; PRETREE_NUM_SYMBOLS], Vec<PretreeOp>) {
    debug_assert_eq!(prev.len(), curr.len());
    let mut freqs = [0u32; PRETREE_NUM_SYMBOLS];
    let mut ops = Vec::new();

    let n = curr.len();
    let threshold = section.run_threshold();
    let mut i = 0;
    while i < n {
        let cur = curr[i];
        // Detect a run of identical lengths starting at i.
        let mut run_len = 1;
        while i + run_len < n && curr[i + run_len] == cur && run_len < section.sym18_max() {
            run_len += 1;
        }

        if run_len >= threshold {
            if cur == 0 {
                // Zero run: pick sym 17 or sym 18 by length.
                if run_len <= section.sym17_max() {
                    let r = run_len.min(section.sym17_max());
                    freqs[17] += 1;
                    ops.push(PretreeOp::ZeroRunShort {
                        run: r,
                    });
                    i += r;
                    continue;
                } else {
                    let r = run_len.min(section.sym18_max());
                    freqs[18] += 1;
                    ops.push(PretreeOp::ZeroRunLong { run: r });
                    i += r;
                    continue;
                }
            } else {
                // Same-delta run: cap at sym19_max repeats.
                let r = run_len.min(section.sym19_max());
                let delta = delta_symbol(prev[i], cur);
                freqs[19] += 1;
                freqs[delta as usize] += 1;
                ops.push(PretreeOp::SameRun { run: r, delta });
                i += r;
                continue;
            }
        }

        // No run: single delta symbol.
        let delta = delta_symbol(prev[i], cur);
        freqs[delta as usize] += 1;
        ops.push(PretreeOp::Single { delta });
        i += 1;
    }

    (freqs, ops)
}

#[derive(Debug, Clone, Copy)]
pub enum PretreeOp {
    Single { delta: u8 },
    ZeroRunShort { run: usize },
    ZeroRunLong { run: usize },
    SameRun { run: usize, delta: u8 },
}

/// Emit a section: the 20 × 4-bit pretree code lengths, then the encoded
/// operations using the canonical pretree codes.
pub fn emit_section<W: std::io::Write>(
    writer: &mut BitWriter<W>,
    section: Section,
    pretree_lengths: &[u8; PRETREE_NUM_SYMBOLS],
    ops: &[PretreeOp],
) -> Result<()> {
    // Build canonical codes for the pretree itself.
    let (_codes, reversed) = canonical_codes(pretree_lengths);

    // Emit the 20 length headers (4 bits each).
    for &len in pretree_lengths.iter() {
        writer.write_bits(len as u32, PRETREE_LENGTH_BITS)?;
    }

    // Emit operations.
    for op in ops {
        match *op {
            PretreeOp::Single { delta } => {
                writer.write_bits(reversed[delta as usize], pretree_lengths[delta as usize] as u32)?;
            }
            PretreeOp::ZeroRunShort { run } => {
                writer.write_bits(reversed[17], pretree_lengths[17] as u32)?;
                writer.write_bits((run - section.sym17_base()) as u32, section.sym17_extra_bits())?;
            }
            PretreeOp::ZeroRunLong { run } => {
                writer.write_bits(reversed[18], pretree_lengths[18] as u32)?;
                writer.write_bits((run - section.sym18_base()) as u32, section.sym18_extra_bits())?;
            }
            PretreeOp::SameRun { run, delta } => {
                writer.write_bits(reversed[19], pretree_lengths[19] as u32)?;
                writer.write_bits((run - section.sym19_base()) as u32, section.sym19_extra_bits())?;
                writer.write_bits(reversed[delta as usize], pretree_lengths[delta as usize] as u32)?;
            }
        }
    }

    Ok(())
}

/// Decode a section: read 20 pretree lengths, build a decode table, then
/// pull `count` length entries, applying deltas relative to `prev`. The
/// new lengths are written into `out` (which must already carry the
/// previous values from a prior block, or all zeros at stream start).
pub fn decode_section<R: Read>(
    reader: &mut BitReader<R>,
    section: Section,
    out: &mut [u8],
) -> Result<()> {
    // Read the 20 × 4-bit pretree code lengths.
    let mut pretree_lengths = [0u8; PRETREE_NUM_SYMBOLS];
    for slot in pretree_lengths.iter_mut() {
        *slot = reader.read_bits_u8(PRETREE_LENGTH_BITS)?;
    }

    let pretree_table = make_decode_table(PRETREE_NUM_SYMBOLS, PRETREE_TABLE_BITS, &pretree_lengths)?;

    let n = out.len();
    let mut i = 0;
    while i < n {
        let sym = decode_symbol(
            reader,
            &pretree_table,
            &pretree_lengths,
            PRETREE_NUM_SYMBOLS,
            PRETREE_TABLE_BITS,
        )?;
        match sym {
            0..=16 => {
                out[i] = apply_delta(out[i], sym as u8);
                i += 1;
            }
            17 => {
                let extra = reader.read_bits(section.sym17_extra_bits())? as usize;
                let run = section.sym17_base() + extra;
                let end = (i + run).min(n);
                for slot in &mut out[i..end] {
                    *slot = 0;
                }
                i = end;
            }
            18 => {
                let extra = reader.read_bits(section.sym18_extra_bits())? as usize;
                let run = section.sym18_base() + extra;
                let end = (i + run).min(n);
                for slot in &mut out[i..end] {
                    *slot = 0;
                }
                i = end;
            }
            19 => {
                let extra = reader.read_bits(section.sym19_extra_bits())? as usize;
                let run = section.sym19_base() + extra;
                let delta_sym = decode_symbol(
                    reader,
                    &pretree_table,
                    &pretree_lengths,
                    PRETREE_NUM_SYMBOLS,
                    PRETREE_TABLE_BITS,
                )?;
                if delta_sym > 16 {
                    return Err(Error::BadHuffmanTree);
                }
                let new_len = apply_delta(out[i], delta_sym as u8);
                let end = (i + run).min(n);
                for slot in &mut out[i..end] {
                    *slot = new_len;
                }
                i = end;
            }
            _ => return Err(Error::BadHuffmanTree),
        }
    }

    Ok(())
}

/// Convenience: encode an entire section in one call. Builds the pretree
/// from the analysis-derived frequencies, emits the header and ops.
pub fn encode_section<W: std::io::Write>(
    writer: &mut BitWriter<W>,
    section: Section,
    prev: &[u8],
    curr: &[u8],
) -> Result<()> {
    let (freqs, ops) = analyse_section(section, prev, curr);
    let pretree_lengths_vec = build_lengths(&freqs, 15);
    let mut pretree_lengths = [0u8; PRETREE_NUM_SYMBOLS];
    pretree_lengths.copy_from_slice(&pretree_lengths_vec);
    emit_section(writer, section, &pretree_lengths, &ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn round_trip_section(section: Section, prev: &[u8], curr: &[u8]) {
        let mut w = BitWriter::new(Vec::new());
        encode_section(&mut w, section, prev, curr).unwrap();
        let (bytes, _) = w.finish().unwrap();

        let mut decoded = prev.to_vec();
        let mut r = BitReader::new(Cursor::new(bytes));
        decode_section(&mut r, section, &mut decoded).unwrap();
        assert_eq!(decoded, curr);
    }

    #[test]
    fn literal_section_no_runs() {
        let prev = vec![0u8; 256];
        let mut curr = vec![0u8; 256];
        for i in 0..256 {
            curr[i] = ((i % 13) + 1) as u8;
        }
        round_trip_section(Section::Literal, &prev, &curr);
    }

    #[test]
    fn literal_section_with_zero_runs() {
        let prev = vec![5u8; 256];
        let mut curr = vec![0u8; 256];
        for i in 100..120 {
            curr[i] = 4;
        }
        round_trip_section(Section::Literal, &prev, &curr);
    }

    #[test]
    fn match_section_with_long_zero_runs() {
        let prev = vec![3u8; 512];
        let mut curr = vec![0u8; 512];
        for i in 0..10 {
            curr[i] = 7;
        }
        for i in 500..512 {
            curr[i] = 8;
        }
        round_trip_section(Section::Match, &prev, &curr);
    }

    #[test]
    fn match_section_same_delta_run() {
        // A long block where curr is a constant offset from prev should
        // trigger sym19 (same-delta runs).
        let prev = vec![10u8; 512];
        let mut curr = vec![7u8; 512];
        // Sprinkle some variation so it's not entirely a single run.
        curr[100] = 3;
        curr[200] = 0;
        round_trip_section(Section::Match, &prev, &curr);
    }

    #[test]
    fn delta_then_apply_is_identity() {
        for prev in 0..=16u8 {
            for curr in 0..=16u8 {
                let s = delta_symbol(prev, curr);
                assert_eq!(apply_delta(prev, s), curr, "prev={prev} curr={curr}");
            }
        }
    }
}
