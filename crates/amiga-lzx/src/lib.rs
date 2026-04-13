//! Pure-Rust compressor and decompressor for the **Amiga LZX** archive
//! format (the format produced by Jonathan Forbes' `LZX` tool on Amiga,
//! as distributed via Aminet). This is **not** Microsoft LZX (the variant
//! used in `.cab`, `.chm`, `.wim` files); the two formats share an
//! algorithmic family but have different wire formats, repeat-offset
//! caches, and container layers. For MS-LZX, see crates such as `lzxd`.
//!
//! The implementation is based on reverse-engineered specs in
//! `ALGORITHM.md` and `CONSTANTS.md` at the workspace root, cross-verified
//! against the canonical `unlzx.c` decompressor from Aminet.

pub mod archive;
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

pub use crate::archive::writer::Level;
pub use crate::archive::{
    ArchiveReader, ArchiveWriter, DateTime, Entry, EntryAttrs, EntryBuilder, EntryWriter,
};

pub use crate::error::{Error, Result};
