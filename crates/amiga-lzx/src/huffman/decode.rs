//! Two-level Huffman decode table.
//!
//! Direct port of `unlzx.c`'s `make_decode_table(number_symbols,
//! table_size, length[], table[])`. The root level is a `1 <<
//! table_size`-entry array indexed by the next `table_size` bits of the
//! bitstream; entries that refer to a single short code store the symbol
//! directly. Codes longer than `table_size` bits index into a secondary
//! binary tree stored in the high half of the same `table[]` array.
//!
//! Refer to ALGORITHM.md §8a for the encoder/decoder agreement.

use crate::error::{Error, Result};

/// Build a two-level decode table.
///
/// - `number_symbols`: alphabet size (e.g. 768 for the main tree).
/// - `table_size`: root index width in bits (12 for main, 7 for aligned,
///   6 for pretree).
/// - `lengths`: per-symbol code length, length 0 = unused.
pub fn make_decode_table(
    number_symbols: usize,
    table_size: u32,
    lengths: &[u8],
) -> Result<Vec<u16>> {
    debug_assert_eq!(lengths.len(), number_symbols);
    debug_assert!((1..=12).contains(&table_size));
    // Secondary node IDs start at `table_mask >> 1` and must stay above
    // the symbol-ID range so the decoder can disambiguate symbols from
    // pointer-to-secondary-node entries with `sym >= number_symbols`.
    debug_assert!(
        (1u32 << table_size) >> 1 >= number_symbols as u32,
        "table_mask/2 ({}) must be >= number_symbols ({})",
        (1u32 << table_size) >> 1,
        number_symbols
    );

    let table_mask: u32 = 1 << table_size;
    // Secondary tree pairs grow upward from `table_mask`. unlzx uses
    // hand-sized tables (5120 / 128 / 96); allocate 4 * number_symbols
    // worth of secondary slots which is always sufficient.
    let total = (table_mask as usize) + 4 * number_symbols;
    let mut table = vec![0u16; total];

    let mut pos: u32 = 0;
    let mut bit_mask: u32 = table_mask >> 1;

    // --- Pass 1: codes that fit in the root table ---
    for bit_num in 1..=table_size {
        for (symbol, &len) in lengths.iter().enumerate() {
            if len as u32 != bit_num {
                continue;
            }
            // Reverse the full table_size-wide pos to get the starting leaf.
            let mut reverse = pos;
            let mut leaf: u32 = 0;
            for _ in 0..table_size {
                leaf = (leaf << 1) | (reverse & 1);
                reverse >>= 1;
            }
            // Advance the canonical-position counter; abort if we'd overrun.
            pos = pos.checked_add(bit_mask).ok_or(Error::BadHuffmanTree)?;
            if pos > table_mask {
                return Err(Error::BadHuffmanTree);
            }
            // Fill `bit_mask` slots, stepping by 1 << bit_num between fills.
            let next_symbol_step = 1u32 << bit_num;
            for _ in 0..bit_mask {
                if (leaf as usize) >= table.len() {
                    return Err(Error::BadHuffmanTree);
                }
                table[leaf as usize] = symbol as u16;
                leaf += next_symbol_step;
            }
        }
        bit_mask >>= 1;
    }

    // If the root table is fully populated, we're done.
    if pos == table_mask {
        return Ok(table);
    }

    // --- Pass 2: codes longer than table_size bits ---
    // Zero out the unused root entries (so the long-code walker can detect
    // "uninitialised" via `table[leaf] == 0`).
    for symbol in pos..table_mask {
        let mut reverse = symbol;
        let mut leaf: u32 = 0;
        for _ in 0..table_size {
            leaf = (leaf << 1) | (reverse & 1);
            reverse >>= 1;
        }
        table[leaf as usize] = 0;
    }

    // Switch to a 16.16-style position counter: pos is shifted up so the
    // canonical "MSB" sits at bit 15.
    let mut next_secondary: u32 = table_mask >> 1; // unlzx uses table_mask>>1, doubled when written
    let mut pos = pos << 16;
    let table_mask_shifted = table_mask << 16;
    let mut bit_mask: u32 = 32768;

    for bit_num in (table_size + 1)..=16 {
        for (symbol, &len) in lengths.iter().enumerate() {
            if len as u32 != bit_num {
                continue;
            }
            // Reverse the high (table_size) bits of pos to find the root index.
            let mut reverse = pos >> 16;
            let mut leaf: u32 = 0;
            for _ in 0..table_size {
                leaf = (leaf << 1) | (reverse & 1);
                reverse >>= 1;
            }
            for fill in 0..(bit_num - table_size) {
                if table[leaf as usize] == 0 {
                    // Allocate a new secondary node pair at index next_secondary.
                    let pair = next_secondary << 1;
                    if (pair as usize + 1) >= table.len() {
                        return Err(Error::BadHuffmanTree);
                    }
                    table[pair as usize] = 0;
                    table[(pair + 1) as usize] = 0;
                    table[leaf as usize] = next_secondary as u16;
                    next_secondary += 1;
                }
                // Walk one level deeper. Use bit (15 - fill) of `pos` to
                // decide left/right.
                leaf = (table[leaf as usize] as u32) << 1;
                leaf += (pos >> (15 - fill)) & 1;
            }
            table[leaf as usize] = symbol as u16;
            pos = pos.checked_add(bit_mask).ok_or(Error::BadHuffmanTree)?;
            if pos > table_mask_shifted {
                return Err(Error::BadHuffmanTree);
            }
        }
        bit_mask >>= 1;
    }

    if pos != table_mask_shifted {
        return Err(Error::BadHuffmanTree);
    }

    Ok(table)
}

/// Decode a single symbol via the two-level table. Mirrors the unlzx.c
/// pattern `symbol = table[control & ((1<<table_size)-1)]; if (symbol >=
/// number_symbols) walk secondary tree`.
pub fn decode_symbol<R: std::io::Read>(
    reader: &mut crate::bitio::BitReader<R>,
    table: &[u16],
    lengths: &[u8],
    number_symbols: usize,
    table_size: u32,
) -> Result<u16> {
    let root_bits = reader.peek_bits(table_size)?;
    let mut sym = table[root_bits as usize];
    if (sym as usize) < number_symbols {
        let len = lengths[sym as usize] as u32;
        reader.consume_bits(len)?;
        return Ok(sym);
    }

    // Long code: consume the root width then walk the secondary tree.
    reader.consume_bits(table_size)?;
    while (sym as usize) >= number_symbols {
        let bit = reader.read_bits(1)?;
        sym = table[((sym as u32) << 1 | bit) as usize];
    }
    Ok(sym)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitio::BitWriter;
    use crate::huffman::build::{build_lengths, canonical_codes};
    use std::io::Cursor;

    fn round_trip(freqs: &[u32], symbols: &[u16], number_symbols: usize, table_size: u32) {
        let lengths = build_lengths(freqs, 16);
        let (_codes, reversed) = canonical_codes(&lengths);

        let mut w = BitWriter::new(Vec::new());
        for &sym in symbols {
            w.write_bits(reversed[sym as usize], lengths[sym as usize] as u32)
                .unwrap();
        }
        let (bytes, _) = w.finish().unwrap();

        let table = make_decode_table(number_symbols, table_size, &lengths).unwrap();
        let mut r = crate::bitio::BitReader::new(Cursor::new(bytes));
        let mut decoded = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            let sym = decode_symbol(&mut r, &table, &lengths, number_symbols, table_size).unwrap();
            decoded.push(sym);
        }
        assert_eq!(decoded, symbols);
    }

    #[test]
    fn small_alphabet_round_trip() {
        let freqs = [5u32, 9, 12, 13, 16, 45, 0, 0];
        round_trip(&freqs, &[0, 1, 2, 3, 4, 5, 5, 5, 4, 0], 8, 6);
    }

    #[test]
    fn alphabet_with_long_codes() {
        // Table dimensions chosen so `table_mask >> 1 >= number_symbols`
        // (i.e. secondary node IDs cannot collide with symbol IDs). All
        // real LZX trees — main 768/12, aligned 8/7, pretree 20/6 — satisfy
        // this invariant.
        let mut freqs = vec![1u32; 64];
        freqs[0] = 100_000;
        freqs[1] = 50_000;
        let mut symbols = Vec::new();
        for i in 0..64u16 {
            symbols.push(i);
        }
        round_trip(&freqs, &symbols, 64, 7);
    }

    #[test]
    fn full_main_tree_round_trip() {
        let mut freqs = vec![0u32; 768];
        for (i, slot) in freqs.iter_mut().enumerate().take(256) {
            *slot = 5 + (i as u32 % 17);
        }
        for (i, slot) in freqs.iter_mut().enumerate().skip(256) {
            *slot = if i % 7 == 0 { 12 } else { 1 };
        }
        let symbols: Vec<u16> = (0..768u16).chain(0..256u16).collect();
        round_trip(&freqs, &symbols, 768, 12);
    }
}
