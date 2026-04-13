//! Archive container format.
//!
//! See ALGORITHM.md §11 and CONSTANTS.md "Archive info header" / "Archive
//! entry header". Layout:
//!
//! ```text
//! [ 10-byte info header ]
//! [ entry header 1 ][ filename 1 ][ comment 1 ][ compressed payload 1 ]
//! [ entry header 2 ][ filename 2 ][ comment 2 ][ compressed payload 2 ]
//! ...
//! ```

pub mod attrs;
pub mod datetime;
pub mod reader;
pub mod writer;

pub use attrs::EntryAttrs;
pub use datetime::DateTime;
pub use reader::{ArchiveReader, Entry};
pub use writer::{ArchiveWriter, EntryBuilder, EntryWriter};
