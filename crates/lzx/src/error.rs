use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("filename too long: {0} bytes (max 255)")]
    FilenameTooLong(usize),

    #[error("comment too long: {0} bytes (max 255)")]
    CommentTooLong(usize),

    #[error("date out of range: {0}")]
    DateOutOfRange(&'static str),

    #[error("invalid archive: {0}")]
    InvalidArchive(&'static str),

    #[error("truncated input")]
    Truncated,

    #[error("crc mismatch: expected {expected:#010x}, got {actual:#010x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("malformed huffman tree")]
    BadHuffmanTree,
}

pub type Result<T> = std::result::Result<T, Error>;
