//! Canonical LZX tables and magic numbers.
//!
//! SOURCE: `CONSTANTS.md` and `ALGORITHM.md` at the workspace root. All
//! tables cross-verified against `unlzx.c`.

// --- LZ77 parameters -------------------------------------------------------

pub const MIN_MATCH: usize = 3;
pub const MAX_MATCH: usize = 258;

/// Primary match-finder distance cutoff. Strictly less than 0x10000 to keep
/// a 260-byte safety margin for max-length matches.
pub const MAX_MATCH_DISTANCE: u32 = 0xfefc;

/// Fallback chain walk iteration count (from `dbra` with init 16).
pub const FALLBACK_WALK_ITERATIONS: usize = 17;

/// Repeat-match shortcut: if the last-offset continuation yields a match of
/// at least this length, accept it without walking the hash chain.
pub const REPEAT_MATCH_SHORTCUT: usize = 51;

/// Token buffer capacity before a block flush is forced.
pub const BLOCK_TOKEN_LIMIT: usize = 0x7ff8;

// --- Alphabet --------------------------------------------------------------

/// Main tree symbol count: 256 literals + 16 × 32 match symbols.
pub const MAIN_SYMBOLS: usize = 768;
pub const LITERAL_SYMBOLS: usize = 256;
pub const MATCH_SYMBOLS: usize = 512;
pub const ALIGNED_SYMBOLS: usize = 8;
pub const PRETREE_SYMBOLS: usize = 20;

pub const LENGTH_SLOTS: usize = 16;
pub const POSITION_SLOTS: usize = 32;

pub const MAX_CODE_LENGTH: u8 = 16;
pub const PRETREE_CODE_LENGTH_BITS: u32 = 4;

// --- Slot tables (CONSTANTS.md) -------------------------------------------

/// `table_one[slot]`: number of footer bits for a position (or length) slot.
pub const TABLE_ONE: [u8; 32] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13, 14, 14,
];

/// `table_two[slot]`: base value covered by the slot.
pub const TABLE_TWO: [u32; 32] = [
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 768, 1024, 1536,
    2048, 3072, 4096, 6144, 8192, 12288, 16384, 24576, 32768, 49152,
];

/// `table_three[n] = (1 << n) - 1` — mask for `n` footer bits.
pub const TABLE_THREE: [u32; 16] = [
    0, 1, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047, 4095, 8191, 16383, 32767,
];

/// Pretree delta decoder table: `table_four[old + 17 - symbol]` gives the
/// new code length. Doubled for wrap-free mod-17 arithmetic.
pub const TABLE_FOUR: [u8; 34] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];

/// Runtime distance-slot lookup. Filled by [`build_slot_lookup`].
pub const SLOT_LOOKUP_LEN: usize = 512;

/// Build the 512-entry distance-slot lookup table (ALGORITHM.md §6).
///
/// ```c
/// table[0..=3] = {0,1,2,3};
/// then table[4..512] is filled so each slot range maps to its slot number.
/// ```
///
/// Lookup at runtime:
/// ```c
/// if (pos < 0x200) slot = table[pos];
/// else             slot = table[pos >> 8] + 16;
/// ```
pub fn build_slot_lookup() -> [u8; SLOT_LOOKUP_LEN] {
    let mut t = [0u8; SLOT_LOOKUP_LEN];
    t[0] = 0;
    t[1] = 1;
    t[2] = 2;
    t[3] = 3;
    // For entries 4..512, each slot s (>= 4) covers positions
    // [TABLE_TWO[s], TABLE_TWO[s] + (1 << TABLE_ONE[s])).
    // We fill the lookup for `pos` in 4..512. For `pos >= 512`, the decoder
    // uses `table[pos >> 8] + 16` — entries at indices 2..255 give values
    // 0..15 which translate to slots 16..31 after the +16 bias.
    for slot in 4..POSITION_SLOTS {
        let base = TABLE_TWO[slot] as usize;
        let width = 1usize << TABLE_ONE[slot];
        // Only fill indices that fall inside 0..SLOT_LOOKUP_LEN.
        if base >= SLOT_LOOKUP_LEN {
            break;
        }
        let end = (base + width).min(SLOT_LOOKUP_LEN);
        for i in base..end {
            t[i] = slot as u8;
        }
    }
    // Sanity: slot 17 is the last one that fully fits (384..511); slot 18
    // onwards is handled by the `>> 8` path.
    t
}

/// Compute position slot for a raw distance using the lookup table.
#[inline]
pub fn position_slot(table: &[u8; SLOT_LOOKUP_LEN], raw_position: u32) -> u8 {
    if raw_position < 0x200 {
        table[raw_position as usize]
    } else {
        table[(raw_position >> 8) as usize] + 16
    }
}

/// Compute length slot for a raw match length (>= MIN_MATCH).
#[inline]
pub fn length_slot(table: &[u8; SLOT_LOOKUP_LEN], raw_length: usize) -> u8 {
    // length slot 0 corresponds to length 3; table indexed by length - 3.
    // The top length slot (15) covers up to length 258 (index 255).
    let idx = raw_length - MIN_MATCH;
    // Length range is 0..=255 which always stays in the low-table path.
    debug_assert!(idx < 0x200);
    table[idx]
}

// --- Compression level parameters (ALGORITHM.md §12) ----------------------

#[derive(Debug, Clone, Copy)]
pub struct LevelParams {
    /// Lazy-match threshold in `curr_len - 3` form. If a current match is at
    /// least this long, skip the lazy-match probe.
    pub lazy_threshold: u16,
    /// Multi-step lazy matching (`-3` only).
    pub multi_step: bool,
}

pub const LEVEL_QUICK: LevelParams = LevelParams {
    lazy_threshold: 1,
    multi_step: false,
};

pub const LEVEL_NORMAL: LevelParams = LevelParams {
    lazy_threshold: 7,
    multi_step: false,
};

pub const LEVEL_MAX: LevelParams = LevelParams {
    lazy_threshold: 40,
    multi_step: true,
};

// --- Info header (ALGORITHM.md §11 / CONSTANTS.md) ------------------------

pub const INFO_HEADER_LEN: usize = 10;
pub const INFO_HEADER_MAGIC: [u8; 4] = *b"LZX\0";
pub const INFO_HEADER_VERSION: u8 = 0x0a;
pub const INFO_HEADER_FLAGS: u8 = 0x04;

pub const ENTRY_HEADER_LEN: usize = 31;
pub const ENTRY_HEADER_MACHINE: u8 = 0x0a;
pub const ENTRY_HEADER_HOST_OS: u8 = 0x0a;
pub const ENTRY_HEADER_PACK_MODE: u8 = 0x02;

pub const DATE_EPOCH_YEAR: u16 = 1970;

// --- CRC32 (zlib) ---------------------------------------------------------

pub const CRC32_POLY: u32 = 0xedb88320;
pub const CRC32_INIT: u32 = 0xffffffff;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_three_is_power_mask() {
        for n in 0..16 {
            assert_eq!(TABLE_THREE[n], (1u32 << n) - 1, "n={n}");
        }
    }

    #[test]
    fn table_one_matches_expected() {
        // Each pair has the same footer bit count, progressing 0,0,0,0,1,1,...
        for s in 0..32 {
            let expected = if s < 4 { 0 } else { (s / 2 - 1) as u8 };
            assert_eq!(TABLE_ONE[s], expected, "slot {s}");
        }
    }

    #[test]
    fn table_two_covers_ranges_contiguously() {
        for s in 0..31 {
            let end = TABLE_TWO[s] + (1u32 << TABLE_ONE[s]);
            assert_eq!(end, TABLE_TWO[s + 1], "slot {s}");
        }
        // Final slot 31 covers up to 49152 + (1<<14) = 65536.
        assert_eq!(TABLE_TWO[31] + (1u32 << TABLE_ONE[31]), 65536);
    }

    #[test]
    fn table_four_is_doubled() {
        for i in 0..17 {
            assert_eq!(TABLE_FOUR[i], i as u8);
            assert_eq!(TABLE_FOUR[i + 17], i as u8);
        }
    }

    #[test]
    fn slot_lookup_matches_table_two() {
        let t = build_slot_lookup();
        // Every position in 1..=511 should decode to a slot whose range covers it.
        for pos in 1u32..512 {
            let slot = position_slot(&t, pos) as usize;
            let base = TABLE_TWO[slot];
            let end = base + (1u32 << TABLE_ONE[slot]);
            assert!(
                (base..end).contains(&pos),
                "pos {pos} -> slot {slot} (range {base}..{end})"
            );
        }
        // High path: every position in 512..=65535 should map correctly.
        for pos in (512u32..65536).step_by(37) {
            let slot = position_slot(&t, pos) as usize;
            let base = TABLE_TWO[slot];
            let end = base + (1u32 << TABLE_ONE[slot]);
            assert!(
                (base..end).contains(&pos),
                "pos {pos} -> slot {slot} (range {base}..{end})"
            );
        }
    }

    #[test]
    fn length_slot_bounds() {
        let t = build_slot_lookup();
        assert_eq!(length_slot(&t, 3), 0);
        assert_eq!(length_slot(&t, 4), 1);
        assert_eq!(length_slot(&t, 5), 2);
        assert_eq!(length_slot(&t, 6), 3);
        assert_eq!(length_slot(&t, 7), 4);
        assert_eq!(length_slot(&t, 8), 4);
        assert_eq!(length_slot(&t, 258), 15);
    }
}
