//! 3-byte rolling hash and zlib-style hash chains.
//!
//! The hash function matches the LZX 1.21R assembly (ALGORITHM.md §2):
//! a 16-bit shift-5 XOR over three bytes. We use a single sliding-window
//! chain (head + prev) instead of the original encoder's dual chain_a /
//! chain_b layout; the original layout exists only to encode "no link"
//! versus "skipped" without a sentinel collision, and a clean Rust impl
//! gets that for free with `Option<u32>` semantics.

pub const HASH_BITS: u32 = 15;
pub const HASH_SIZE: usize = 1 << HASH_BITS;
pub const HASH_MASK: u32 = (HASH_SIZE - 1) as u32;

/// Sentinel meaning "no entry".
pub const NONE: u32 = u32::MAX;

#[inline]
pub fn hash3(b0: u8, b1: u8, b2: u8) -> u32 {
    let mut h: u16 = 0;
    h = (h << 5) ^ (b0 as u16);
    h = (h << 5) ^ (b1 as u16);
    h = (h << 5) ^ (b2 as u16);
    (h as u32) & HASH_MASK
}

/// Hash chain table. Indexed by absolute byte position; chain links use
/// `pos & 0xffff` so collisions older than 65 KB silently fall off the
/// end (which is fine because the LZX max distance is 65 535).
pub struct HashChains {
    /// `head[hash]` = most recent absolute position with this 3-byte
    /// hash, or `NONE`.
    head: Vec<u32>,
    /// `prev[pos & 0xffff]` = previous absolute position sharing the same
    /// hash, or `NONE`.
    prev: Vec<u32>,
}

impl HashChains {
    pub fn new() -> Self {
        HashChains {
            head: vec![NONE; HASH_SIZE],
            prev: vec![NONE; 0x10000],
        }
    }

    #[inline]
    pub fn insert(&mut self, hash: u32, pos: u32) {
        let h = (hash & HASH_MASK) as usize;
        let prev_pos = self.head[h];
        self.head[h] = pos;
        self.prev[(pos & 0xffff) as usize] = prev_pos;
    }

    /// Most recent position with this hash, or `NONE`.
    #[inline]
    pub fn head(&self, hash: u32) -> u32 {
        self.head[(hash & HASH_MASK) as usize]
    }

    /// Walk one link back from `pos`.
    #[inline]
    pub fn prev(&self, pos: u32) -> u32 {
        self.prev[(pos & 0xffff) as usize]
    }
}

impl Default for HashChains {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash3(b'a', b'b', b'c'), hash3(b'a', b'b', b'c'));
        assert_ne!(hash3(b'a', b'b', b'c'), hash3(b'a', b'b', b'd'));
    }

    #[test]
    fn chain_round_trips_recent_inserts() {
        let mut chains = HashChains::new();
        let h = hash3(1, 2, 3);
        chains.insert(h, 100);
        chains.insert(h, 200);
        chains.insert(h, 300);

        assert_eq!(chains.head(h), 300);
        assert_eq!(chains.prev(300), 200);
        assert_eq!(chains.prev(200), 100);
        assert_eq!(chains.prev(100), NONE);
    }
}
