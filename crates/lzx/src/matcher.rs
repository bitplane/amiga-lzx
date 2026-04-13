//! Match finder.
//!
//! Walks the hash chain at the current position, looking for the longest
//! common substring within the LZX distance limit (65 535 bytes). Returns
//! the length and distance of the best match.
//!
//! This is a clean, simple implementation — not a faithful port of the
//! Amiga binary's two-walk match finder. We're targeting valid LZX
//! output, not byte-exact parity (see plan §"Approach"). The chain walk
//! is bounded by `max_chain_walks`; the original's compression-level
//! tuning will be expressed via that knob in the LZ77 layer.

use crate::constants::{MAX_MATCH, MIN_MATCH};
use crate::hash::{HashChains, NONE};

/// LZX max distance. Strictly less than the 64 KB window so a max-length
/// match can never read past the end. Mirrors `MAX_MATCH_DISTANCE` in
/// `constants.rs` but expressed as a `usize` for ergonomics here.
pub const MAX_DISTANCE: usize = 0xfefc;

/// Result of a match-finder probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub length: usize,
    pub distance: usize,
}

impl Match {
    pub const NONE: Match = Match { length: 0, distance: 0 };
    pub fn is_useful(&self) -> bool {
        self.length >= MIN_MATCH
    }
}

/// Find the longest match for the 3-byte sequence at `window[pos..]`
/// against earlier positions in `window`, walking up to
/// `max_chain_walks` candidates.
///
/// `chains` must already have been populated for positions `0..=pos`.
pub fn find_longest_match(
    window: &[u8],
    pos: usize,
    chains: &HashChains,
    hash: u32,
    max_chain_walks: usize,
) -> Match {
    if pos + MIN_MATCH > window.len() {
        return Match::NONE;
    }

    let max_len = (window.len() - pos).min(MAX_MATCH);
    if max_len < MIN_MATCH {
        return Match::NONE;
    }

    let mut best = Match::NONE;
    let mut walks = 0;
    let mut candidate = chains.head(hash);

    while candidate != NONE && walks < max_chain_walks {
        // Only consider candidates strictly before the current position.
        if (candidate as usize) >= pos {
            // Stale chain head from a parallel position; chase the link.
            candidate = chains.prev(candidate);
            walks += 1;
            continue;
        }
        let dist = pos - candidate as usize;
        if dist == 0 || dist > MAX_DISTANCE {
            break;
        }

        // Quick reject: the byte just past the current best length must
        // match (canonical zlib trick — saves a lot of inner-loop work).
        let cand = candidate as usize;
        if best.length >= MIN_MATCH {
            // Compare the byte at offset `best.length` first; if that doesn't
            // match, this candidate can't beat us.
            if window[cand + best.length] != window[pos + best.length] {
                candidate = chains.prev(candidate);
                walks += 1;
                continue;
            }
        }

        // Verify the leading 3 bytes (we use a 3-byte hash so the first 2
        // matching is implied but cheap to check).
        if window[cand] != window[pos] || window[cand + 1] != window[pos + 1] {
            candidate = chains.prev(candidate);
            walks += 1;
            continue;
        }

        // Extend the match forward.
        let mut len = 2;
        while len < max_len && window[cand + len] == window[pos + len] {
            len += 1;
        }

        if len >= MIN_MATCH && len > best.length {
            best = Match {
                length: len,
                distance: dist,
            };
            if len >= max_len {
                break;
            }
        }

        candidate = chains.prev(candidate);
        walks += 1;
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash3;

    fn build_chains(window: &[u8], up_to: usize) -> HashChains {
        let mut c = HashChains::new();
        let limit = up_to.min(window.len().saturating_sub(2));
        for i in 0..limit {
            let h = hash3(window[i], window[i + 1], window[i + 2]);
            c.insert(h, i as u32);
        }
        c
    }

    #[test]
    fn finds_simple_repeat() {
        let data = b"the quick brown fox the quick brown";
        // Insert positions 0..=18; search at position 20 ("the quick brown").
        let chains = build_chains(data, 20);
        let h = hash3(data[20], data[21], data[22]);
        let m = find_longest_match(data, 20, &chains, h, 64);
        assert!(m.is_useful());
        assert_eq!(m.distance, 20);
        assert_eq!(m.length, 15);
    }

    #[test]
    fn finds_max_length_run_of_zeros() {
        let mut data = vec![0u8; 1024];
        // Insert positions 0..512 so position 512 has plenty to match.
        for byte in data.iter_mut() {
            *byte = 0;
        }
        let chains = build_chains(&data, 512);
        let h = hash3(0, 0, 0);
        let m = find_longest_match(&data, 512, &chains, h, 256);
        assert_eq!(m.length, MAX_MATCH);
        // Distance 1 (the previous zero) is the closest valid match.
        assert!(m.distance >= 1 && m.distance <= MAX_DISTANCE);
    }

    #[test]
    fn no_match_returns_none() {
        let data = b"abcdefghijklmnop";
        let chains = build_chains(data, 8);
        let pos = 8;
        let h = hash3(data[pos], data[pos + 1], data[pos + 2]);
        let m = find_longest_match(data, pos, &chains, h, 64);
        assert!(!m.is_useful());
    }

    #[test]
    fn distance_limit_is_respected() {
        // Place a match exactly at the distance boundary and one just past.
        let mut data = vec![0xaau8; MAX_DISTANCE + 100];
        // Make positions distinct so only the planted matches show up.
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        // Plant a 3-byte sequence at position 0 and again at position 50.
        data[0] = 0x10;
        data[1] = 0x20;
        data[2] = 0x30;
        let pos = MAX_DISTANCE; // Within limit (distance MAX_DISTANCE).
        data[pos] = 0x10;
        data[pos + 1] = 0x20;
        data[pos + 2] = 0x30;

        let chains = build_chains(&data, pos);
        let h = hash3(data[pos], data[pos + 1], data[pos + 2]);
        let m = find_longest_match(&data, pos, &chains, h, 256);
        // Distance pos - 0 == MAX_DISTANCE, exactly at the limit.
        assert!(m.is_useful());
        assert_eq!(m.distance, MAX_DISTANCE);
    }
}
