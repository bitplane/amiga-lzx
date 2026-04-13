//! Length-limited canonical Huffman builder.
//!
//! Uses package-merge to compute code lengths bounded by `max_len` (≤ 16
//! for LZX) given symbol frequencies. Then assigns canonical codes in
//! `(length, symbol)` order — the same order the decoder's
//! `make_decode_table` expects (ALGORITHM.md §8a).

/// Produce code lengths for `freqs.len()` symbols, bounded by `max_len`.
///
/// Symbols with zero frequency get length 0 (= unused). The caller is
/// responsible for ensuring `2.pow(max_len)` is at least the number of
/// active (non-zero frequency) symbols, otherwise no valid code exists.
pub fn build_lengths(freqs: &[u32], max_len: u8) -> Vec<u8> {
    let n = freqs.len();
    let mut lengths = vec![0u8; n];

    let active: Vec<(u32, u16)> = freqs
        .iter()
        .enumerate()
        .filter_map(|(i, &f)| if f > 0 { Some((f, i as u16)) } else { None })
        .collect();
    let m = active.len();

    if m == 0 {
        return lengths;
    }
    if m == 1 {
        // Single symbol: assign length 1 to it AND to one "phantom"
        // unused symbol so the resulting tree's Kraft sum equals 1. The
        // decoder requires a complete code; an isolated length-1 entry
        // would be rejected as a malformed tree. The phantom is any other
        // symbol — it will never appear in the encoded stream because
        // the encoder only emits frequencies > 0.
        let only = active[0].1 as usize;
        lengths[only] = 1;
        let phantom = if only == 0 { 1 } else { 0 };
        if phantom < n {
            lengths[phantom] = 1;
        }
        return lengths;
    }

    // Sort by (weight, symbol_index) for deterministic output.
    let mut active = active;
    active.sort_by_key(|&(f, i)| (f, i));

    // Arena of tree nodes. Each "coin" we manipulate references a node in
    // this arena. Leaves come first (one per active symbol).
    let mut arena: Vec<Node> = active.iter().map(|&(_, sym)| Node::Leaf(sym)).collect();

    // The "original" sorted-symbol coin list; this is the per-level seed.
    let original: Vec<Coin> = active
        .iter()
        .enumerate()
        .map(|(idx, &(f, _))| Coin {
            weight: f as u64,
            node: idx as u32,
        })
        .collect();
    let mut coins: Vec<Coin> = original.clone();

    // Iterate from depth `max_len` down to depth 1. At each step package
    // adjacent pairs and seed the next-shallower level with the originals.
    for _ in 0..(max_len - 1) {
        let mut packaged: Vec<Coin> = Vec::with_capacity(coins.len() / 2);
        let mut iter = coins.chunks_exact(2);
        for chunk in &mut iter {
            let a = chunk[0];
            let b = chunk[1];
            let new_node = arena.len() as u32;
            arena.push(Node::Pair(a.node, b.node));
            packaged.push(Coin {
                weight: a.weight + b.weight,
                node: new_node,
            });
        }
        coins = merge_sorted(&packaged, &original);
    }

    // Take the cheapest `2m - 2` coins; each contributes one to the length
    // of every leaf it contains.
    let take = 2 * m - 2;
    for coin in coins.iter().take(take) {
        walk_increment(&arena, coin.node, &mut lengths);
    }

    debug_assert!(
        kraft_sum_valid(&lengths, max_len),
        "package-merge produced invalid lengths {lengths:?}"
    );

    lengths
}

/// Convert code lengths into canonical codes plus their bit-reversed forms.
///
/// `codes[sym]` is the LZX-compatible canonical code (most-significant bit
/// of the prefix sits at bit `length-1`). `reversed[sym]` is the same code
/// with its low `length` bits reversed, matching what the LSB-first
/// [`crate::bitio::BitWriter`] expects to OR into the bit buffer.
///
/// Symbols with length 0 get `(0, 0)`.
pub fn canonical_codes(lengths: &[u8]) -> (Vec<u32>, Vec<u32>) {
    let n = lengths.len();
    let mut codes = vec![0u32; n];
    let mut reversed = vec![0u32; n];

    let mut next_code: u32 = 0;
    for length in 1..=16u8 {
        for sym in 0..n {
            if lengths[sym] == length {
                codes[sym] = next_code;
                reversed[sym] = reverse_bits(next_code, length as u32);
                next_code += 1;
            }
        }
        next_code <<= 1;
    }
    (codes, reversed)
}

/// Reverse the low `n` bits of `value`. `n` ≤ 16.
#[inline]
pub fn reverse_bits(value: u32, n: u32) -> u32 {
    let mut v = value;
    let mut out = 0u32;
    for _ in 0..n {
        out = (out << 1) | (v & 1);
        v >>= 1;
    }
    out
}

#[derive(Debug, Clone, Copy)]
enum Node {
    Leaf(u16),
    Pair(u32, u32),
}

#[derive(Debug, Clone, Copy)]
struct Coin {
    weight: u64,
    node: u32,
}

fn merge_sorted(a: &[Coin], b: &[Coin]) -> Vec<Coin> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let mut i = 0usize;
    let mut j = 0usize;
    while i < a.len() && j < b.len() {
        if a[i].weight <= b[j].weight {
            out.push(a[i]);
            i += 1;
        } else {
            out.push(b[j]);
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

fn walk_increment(arena: &[Node], node: u32, lengths: &mut [u8]) {
    // Iterative walk to avoid any stack concerns; tree depth ≤ max_len ≤ 16
    // so a small stack vec suffices.
    let mut stack: Vec<u32> = Vec::with_capacity(32);
    stack.push(node);
    while let Some(id) = stack.pop() {
        match arena[id as usize] {
            Node::Leaf(sym) => lengths[sym as usize] += 1,
            Node::Pair(a, b) => {
                stack.push(a);
                stack.push(b);
            }
        }
    }
}

fn kraft_sum_valid(lengths: &[u8], max_len: u8) -> bool {
    let mut sum: u64 = 0;
    let scale = 1u64 << max_len;
    for &l in lengths {
        if l == 0 {
            continue;
        }
        if l > max_len {
            return false;
        }
        sum += scale >> l;
    }
    sum <= scale
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_freqs() {
        let l = build_lengths(&[0u32; 8], 16);
        assert!(l.iter().all(|&x| x == 0));
    }

    #[test]
    fn single_symbol_gets_phantom_partner() {
        // A single active symbol gets length 1, plus one phantom partner
        // also at length 1 so the resulting tree is a complete prefix code
        // (Kraft sum == 1). The phantom is never encoded.
        let mut freqs = [0u32; 8];
        freqs[3] = 99;
        let l = build_lengths(&freqs, 16);
        assert_eq!(l[3], 1);
        // Exactly one other symbol should be length 1 (the phantom).
        let extras: Vec<usize> = (0..8).filter(|&i| i != 3 && l[i] == 1).collect();
        assert_eq!(extras.len(), 1);
        // Everything else stays 0.
        let zeros = l.iter().filter(|&&x| x == 0).count();
        assert_eq!(zeros, 6);
    }

    #[test]
    fn two_symbols_get_length_1() {
        let mut freqs = [0u32; 5];
        freqs[1] = 10;
        freqs[4] = 20;
        let l = build_lengths(&freqs, 16);
        assert_eq!(l[1], 1);
        assert_eq!(l[4], 1);
    }

    #[test]
    fn classic_example_is_optimal() {
        // Frequencies for symbols A..E.
        // Standard Huffman gives (5, 9, 12, 13, 16, 45) → lengths
        // (4, 4, 3, 3, 3, 1).
        let freqs = [5u32, 9, 12, 13, 16, 45];
        let l = build_lengths(&freqs, 16);
        // Sum (freq * length) = the cost; check it equals known optimum 224.
        let cost: u32 = freqs
            .iter()
            .zip(l.iter())
            .map(|(&f, &li)| f * li as u32)
            .sum();
        assert_eq!(cost, 224);
        assert!(kraft_sum_valid(&l, 16));
    }

    #[test]
    fn skewed_distribution_kraft_valid() {
        let mut freqs = vec![0u32; 768];
        // Heavy tail to stress the builder.
        for i in 0..768 {
            freqs[i] = (i as u32 + 1) * 3;
        }
        let l = build_lengths(&freqs, 16);
        assert!(kraft_sum_valid(&l, 16));
        for &li in &l {
            assert!(li >= 1 && li <= 16);
        }
    }

    #[test]
    fn limit_constrains_max_length() {
        // Pathological frequencies that would normally produce a deep tree.
        let mut freqs = vec![1u32; 200];
        // Make one symbol massively heavier so others get pushed deep.
        freqs[0] = 1_000_000;
        let l = build_lengths(&freqs, 12);
        for &li in &l {
            assert!(li <= 12, "length {li} exceeds limit");
        }
        assert!(kraft_sum_valid(&l, 12));
    }

    #[test]
    fn canonical_codes_are_prefix_unique() {
        let lengths = vec![3u8, 3, 3, 3, 3, 2, 4, 4];
        let (codes, _rev) = canonical_codes(&lengths);
        // Verify Kraft and prefix-freeness by reconstruction: walk all
        // (code, length) pairs and ensure no two share a prefix.
        let mut seen: Vec<(u32, u8)> = Vec::new();
        for (i, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            for &(other_code, other_len) in &seen {
                let (short, short_len, long, long_len) = if l <= other_len {
                    (codes[i], l, other_code, other_len)
                } else {
                    (other_code, other_len, codes[i], l)
                };
                let shifted = long >> (long_len - short_len);
                assert_ne!(shifted, short, "prefix collision");
            }
            seen.push((codes[i], l));
        }
    }

    #[test]
    fn reverse_bits_known_values() {
        assert_eq!(reverse_bits(0b1, 1), 0b1);
        assert_eq!(reverse_bits(0b10, 2), 0b01);
        assert_eq!(reverse_bits(0b1100, 4), 0b0011);
        assert_eq!(reverse_bits(0b1011_0000, 8), 0b0000_1101);
    }
}
