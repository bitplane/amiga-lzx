//! Pure-Rust LZX (Amiga) compressor and decompressor.
//!
//! Based on reverse-engineered specs in `ALGORITHM.md` and `CONSTANTS.md`
//! at the workspace root, cross-verified against `unlzx.c`.

pub mod constants;
pub mod crc32;
pub mod error;

pub use crate::error::{Error, Result};
