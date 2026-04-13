//! Bit-level I/O.
//!
//! LZX is a 16-bit big-endian word stream. Within each word, the bit the
//! writer emits *first* sits at the **least significant** end of the word —
//! the bit reader pulls bits off the low end of an accumulator. See
//! ALGORITHM.md §10.

pub mod reader;
pub mod writer;

pub use reader::BitReader;
pub use writer::BitWriter;
