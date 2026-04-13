//! Pure-Rust LZX (Amiga) compressor and decompressor.
//!
//! Based on reverse-engineered specs in `ALGORITHM.md` and `CONSTANTS.md`
//! at the workspace root, cross-verified against `unlzx.c`.

pub mod bitio;
pub mod block;
pub mod constants;
pub mod crc32;
pub mod decoder;
pub mod error;
pub mod hash;
pub mod huffman;
pub mod lz77;
pub mod matcher;

pub use crate::error::{Error, Result};
