//! LZ77 token stream producer.
//!
//! Walks the input, finds matches via [`crate::matcher`], and produces a
//! token stream of literals and matches. Implements lazy matching with
//! the gating rules from ALGORITHM.md §4 — but uses a simplified
//! "take the longer of the two" cost rule rather than the original 68k
//! cost formula. We're targeting valid LZX, not byte-exact parity.

use crate::constants::{LevelParams, MAX_MATCH, MIN_MATCH};
use crate::hash::{hash3, HashChains};
use crate::matcher::{find_longest_match, Match};

/// One LZ77 token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Literal(u8),
    Match { length: u16, distance: u16 },
}

/// Per-level chain walk depth. The original encoder scales walk depth
/// indirectly via lazy-match aggressiveness; we expose it directly.
fn chain_walks(level: &LevelParams) -> usize {
    if level.multi_step {
        1024
    } else if level.lazy_threshold >= 7 {
        128
    } else {
        16
    }
}

/// Produce a token stream from `input` at the given compression level.
pub fn encode(input: &[u8], level: &LevelParams) -> Vec<Token> {
    let mut chains = HashChains::new();
    let mut tokens: Vec<Token> = Vec::with_capacity(input.len() / 2 + 16);
    let walks = chain_walks(level);

    // High-water mark: positions strictly less than this have been inserted
    // into the hash chain. Avoids double-insertion (which would create
    // self-referential chain links and hang the matcher).
    let mut hashed_up_to: usize = 0;

    let mut pos: usize = 0;
    while pos < input.len() {
        // Make sure pos has been inserted.
        ensure_hashed(input, &mut chains, &mut hashed_up_to, pos);

        // Trailing tail too short for any match — emit literals.
        if pos + MIN_MATCH > input.len() {
            tokens.push(Token::Literal(input[pos]));
            pos += 1;
            continue;
        }

        let h_curr = hash3(input[pos], input[pos + 1], input[pos + 2]);
        let curr = find_longest_match(input, pos, &chains, h_curr, walks);

        if !curr.is_useful() {
            tokens.push(Token::Literal(input[pos]));
            pos += 1;
            continue;
        }

        // Lazy-match probe?
        let take_lazy = should_try_lazy(&curr, level)
            && lazy_better(input, &mut chains, &mut hashed_up_to, pos, walks, &curr);

        if take_lazy {
            tokens.push(Token::Literal(input[pos]));
            pos += 1;
            continue;
        }

        // Commit to the current match.
        tokens.push(Token::Match {
            length: curr.length as u16,
            distance: curr.distance as u16,
        });
        let end = pos + curr.length;
        // Make sure every position covered by the match has its hash entry,
        // so later positions can find them.
        ensure_hashed(input, &mut chains, &mut hashed_up_to, end.saturating_sub(1));
        pos = end;
    }

    tokens
}

fn ensure_hashed(
    input: &[u8],
    chains: &mut HashChains,
    hashed_up_to: &mut usize,
    target: usize,
) {
    let limit = (target + 1).min(input.len().saturating_sub(MIN_MATCH - 1));
    while *hashed_up_to < limit {
        let p = *hashed_up_to;
        let h = hash3(input[p], input[p + 1], input[p + 2]);
        chains.insert(h, p as u32);
        *hashed_up_to += 1;
    }
}

#[inline]
fn should_try_lazy(curr: &Match, level: &LevelParams) -> bool {
    // ALGORITHM.md §4 entry conditions (subset; we don't track last_offset
    // yet so we skip the "previous emit was a repeat" gate):
    //
    // - threshold <= curr_len - 3  → already good enough
    let curr_excess = curr.length.saturating_sub(MIN_MATCH) as u16;
    curr_excess < level.lazy_threshold && curr.length < MAX_MATCH
}

fn lazy_better(
    input: &[u8],
    chains: &mut HashChains,
    hashed_up_to: &mut usize,
    pos: usize,
    walks: usize,
    curr: &Match,
) -> bool {
    let next_pos = pos + 1;
    if next_pos + MIN_MATCH > input.len() {
        return false;
    }
    ensure_hashed(input, chains, hashed_up_to, next_pos);
    let h_next = hash3(input[next_pos], input[next_pos + 1], input[next_pos + 2]);
    let next = find_longest_match(input, next_pos, chains, h_next, walks);
    if !next.is_useful() {
        return false;
    }
    if next.length == MIN_MATCH && next.distance > 29_999 {
        return false;
    }
    // Simplified cost rule: take lazy if the next match is strictly longer.
    next.length > curr.length
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{LEVEL_NORMAL, LEVEL_QUICK, LEVEL_MAX};

    fn token_count_matches_input(tokens: &[Token]) -> usize {
        tokens
            .iter()
            .map(|t| match t {
                Token::Literal(_) => 1,
                Token::Match { length, .. } => *length as usize,
            })
            .sum()
    }

    /// Reconstruct the original input from a token stream by simulating the
    /// decoder. Used as a sanity check that the encoder's tokens describe
    /// a self-consistent stream.
    fn reconstruct(tokens: &[Token]) -> Vec<u8> {
        let mut out = Vec::new();
        for tok in tokens {
            match *tok {
                Token::Literal(b) => out.push(b),
                Token::Match { length, distance } => {
                    let start = out.len() - distance as usize;
                    for i in 0..length as usize {
                        let b = out[start + i];
                        out.push(b);
                    }
                }
            }
        }
        out
    }

    #[test]
    fn empty_input_produces_no_tokens() {
        let tokens = encode(b"", &LEVEL_NORMAL);
        assert!(tokens.is_empty());
    }

    #[test]
    fn single_byte_emits_one_literal() {
        let tokens = encode(b"x", &LEVEL_NORMAL);
        assert_eq!(tokens, vec![Token::Literal(b'x')]);
    }

    #[test]
    fn no_repeats_all_literals() {
        let data: Vec<u8> = (0..200u8).collect();
        let tokens = encode(&data, &LEVEL_NORMAL);
        assert_eq!(tokens.len(), data.len());
        assert!(tokens.iter().all(|t| matches!(t, Token::Literal(_))));
        assert_eq!(reconstruct(&tokens), data);
    }

    #[test]
    fn long_zeros_collapse_to_matches() {
        let data = vec![0u8; 4096];
        let tokens = encode(&data, &LEVEL_NORMAL);
        // Should be one literal then a few long matches — far fewer
        // tokens than 4096.
        assert!(tokens.len() < 200);
        assert_eq!(token_count_matches_input(&tokens), data.len());
        assert_eq!(reconstruct(&tokens), data);
    }

    #[test]
    fn periodic_pattern_compresses() {
        let mut data = Vec::new();
        for _ in 0..200 {
            data.extend_from_slice(b"ABCDEFG");
        }
        let tokens = encode(&data, &LEVEL_NORMAL);
        assert!(tokens.len() < data.len() / 2);
        assert_eq!(reconstruct(&tokens), data);
    }

    #[test]
    fn round_trip_lorem_ipsum() {
        let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna \
                     aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
                     ullamco laboris nisi ut aliquip ex ea commodo consequat.";
        for level in [LEVEL_QUICK, LEVEL_NORMAL, LEVEL_MAX] {
            let tokens = encode(data, &level);
            assert_eq!(reconstruct(&tokens), data);
            assert_eq!(token_count_matches_input(&tokens), data.len());
        }
    }

    #[test]
    fn round_trip_random_bytes() {
        // Deterministic xorshift PRNG.
        let mut s: u64 = 0x1234_5678_9abc_def0;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let mut data = vec![0u8; 32 * 1024];
        for b in data.iter_mut() {
            *b = (next() & 0xff) as u8;
        }
        let tokens = encode(&data, &LEVEL_NORMAL);
        assert_eq!(reconstruct(&tokens), data);
    }
}
