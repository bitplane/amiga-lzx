//! Huffman coding for LZX.
//!
//! - [`build`] computes length-limited code lengths from frequencies and
//!   produces canonical codes (sorted by `(length, symbol)`) along with
//!   their bit-reversed forms ready for [`crate::bitio::BitWriter`].
//! - [`decode`] builds the two-level lookup table used by the decoder,
//!   ported from `unlzx.c`'s `make_decode_table`.
//! - [`pretree`] implements the pretree-compressed delta encoding of code
//!   length arrays for the literal and match sections of the main tree
//!   (ALGORITHM.md §8).

pub mod build;
pub mod decode;
pub mod pretree;
