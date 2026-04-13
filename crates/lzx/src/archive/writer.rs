//! Archive writer. Streaming, requires `Write + Seek` so entry headers
//! can be patched in place after the compressed payload has been written.

use std::io::{Seek, SeekFrom, Write};

use crate::archive::{DateTime, EntryAttrs};
use crate::block::BlockWriter;
use crate::constants::{
    BLOCK_TOKEN_LIMIT, ENTRY_HEADER_HOST_OS, ENTRY_HEADER_LEN, ENTRY_HEADER_MACHINE,
    ENTRY_HEADER_PACK_MODE, INFO_HEADER_FLAGS, INFO_HEADER_LEN, INFO_HEADER_MAGIC,
    INFO_HEADER_VERSION, LEVEL_NORMAL, LEVEL_QUICK, LEVEL_MAX, LevelParams,
};
use crate::crc32::{crc32, Crc32};
use crate::error::{Error, Result};
use crate::lz77;

/// Build the 10-byte info header. The 8-bit checksum is the byte sum of
/// the header itself with the checksum field initially zero.
pub(crate) fn make_info_header() -> [u8; INFO_HEADER_LEN] {
    let mut h = [0u8; INFO_HEADER_LEN];
    h[0..4].copy_from_slice(&INFO_HEADER_MAGIC);
    h[6] = INFO_HEADER_VERSION;
    h[7] = INFO_HEADER_FLAGS;
    let mut sum: u8 = 0;
    for &b in h.iter() {
        sum = sum.wrapping_add(b);
    }
    h[4] = sum;
    h
}

/// Top-level archive writer.
pub struct ArchiveWriter<W: Write + Seek> {
    inner: W,
}

impl<W: Write + Seek> ArchiveWriter<W> {
    /// Construct a writer and immediately emit the 10-byte info header.
    pub fn new(mut inner: W) -> Result<Self> {
        let header = make_info_header();
        inner.write_all(&header)?;
        Ok(ArchiveWriter { inner })
    }

    /// Begin a new entry. Returns a sink that you write the uncompressed
    /// bytes into; call [`EntryWriter::finish`] when done.
    pub fn add_entry(&mut self, builder: EntryBuilder) -> Result<EntryWriter<'_, W>> {
        let fname_len = builder.filename.len();
        let cmt_len = builder.comment.len();
        if fname_len > 255 {
            return Err(Error::FilenameTooLong(fname_len));
        }
        if cmt_len > 255 {
            return Err(Error::CommentTooLong(cmt_len));
        }

        // Reserve placeholder space for the entry header so the payload
        // can be streamed straight after. We patch the header on finish.
        let header_pos = self.inner.stream_position()?;
        let total_header = ENTRY_HEADER_LEN + fname_len + cmt_len;
        let zeros = vec![0u8; total_header];
        self.inner.write_all(&zeros)?;

        Ok(EntryWriter {
            archive: self,
            builder,
            header_pos,
            uncompressed: Vec::new(),
            finished: false,
        })
    }

    /// Consume the writer and return the inner writer.
    pub fn finish(self) -> Result<W> {
        Ok(self.inner)
    }

    fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

/// Builder describing the metadata for one entry.
#[derive(Debug, Clone)]
pub struct EntryBuilder {
    pub filename: String,
    pub comment: String,
    pub attrs: EntryAttrs,
    pub datetime: DateTime,
    pub level: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    Quick,
    #[default]
    Normal,
    Max,
}

impl Level {
    pub(crate) fn params(self) -> LevelParams {
        match self {
            Level::Quick => LEVEL_QUICK,
            Level::Normal => LEVEL_NORMAL,
            Level::Max => LEVEL_MAX,
        }
    }
}

impl EntryBuilder {
    pub fn new(filename: impl Into<String>) -> Self {
        EntryBuilder {
            filename: filename.into(),
            comment: String::new(),
            attrs: EntryAttrs::default(),
            datetime: DateTime::ZERO,
            level: Level::default(),
        }
    }

    pub fn comment(mut self, c: impl Into<String>) -> Self {
        self.comment = c.into();
        self
    }

    pub fn attrs(mut self, a: EntryAttrs) -> Self {
        self.attrs = a;
        self
    }

    pub fn datetime(mut self, d: DateTime) -> Self {
        self.datetime = d;
        self
    }

    pub fn level(mut self, l: Level) -> Self {
        self.level = l;
        self
    }
}

/// Sink for writing uncompressed bytes to one entry. Buffers internally
/// because the compressor needs the full input up-front.
pub struct EntryWriter<'a, W: Write + Seek> {
    archive: &'a mut ArchiveWriter<W>,
    builder: EntryBuilder,
    header_pos: u64,
    uncompressed: Vec<u8>,
    finished: bool,
}

impl<'a, W: Write + Seek> Write for EntryWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.uncompressed.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a, W: Write + Seek> EntryWriter<'a, W> {
    /// Encode the buffered bytes, write the payload, and patch the entry
    /// header in place.
    pub fn finish(mut self) -> Result<()> {
        self.finished = true;

        let original_size = self.uncompressed.len() as u32;
        let data_crc = crc32(&self.uncompressed);

        // Encode payload: LZ77 → tokens → blocks of up to BLOCK_TOKEN_LIMIT.
        let level_params = self.builder.level.params();
        let tokens = lz77::encode(&self.uncompressed, &level_params);
        let mut bw = BlockWriter::new(Vec::new());
        if !tokens.is_empty() {
            for chunk in tokens.chunks(BLOCK_TOKEN_LIMIT) {
                bw.write_block(chunk)?;
            }
        }
        let (payload, _) = bw.finish()?;
        let compressed_size = payload.len() as u32;

        // Write payload after the placeholder header.
        self.archive.inner_mut().write_all(&payload)?;
        let end_pos = self.archive.inner_mut().stream_position()?;

        // Build the final fixed header.
        let fname_len = self.builder.filename.len();
        let cmt_len = self.builder.comment.len();
        let total = ENTRY_HEADER_LEN + fname_len + cmt_len;
        let mut header = vec![0u8; total];
        header[0] = self.builder.attrs.bits();
        header[2..6].copy_from_slice(&original_size.to_le_bytes());
        header[6..10].copy_from_slice(&compressed_size.to_le_bytes());
        header[10] = ENTRY_HEADER_MACHINE;
        header[11] = ENTRY_HEADER_PACK_MODE;
        header[12] = 0; // merged flag — never set in v1
        header[14] = cmt_len as u8;
        header[15] = ENTRY_HEADER_HOST_OS;
        header[18..22].copy_from_slice(&self.builder.datetime.pack());
        header[22..26].copy_from_slice(&data_crc.to_le_bytes());
        // header[26..30] = header CRC, computed with these bytes zeroed.
        header[30] = fname_len as u8;
        header[ENTRY_HEADER_LEN..ENTRY_HEADER_LEN + fname_len]
            .copy_from_slice(self.builder.filename.as_bytes());
        let cmt_off = ENTRY_HEADER_LEN + fname_len;
        header[cmt_off..cmt_off + cmt_len].copy_from_slice(self.builder.comment.as_bytes());

        // Compute and write the header CRC.
        let mut crc = Crc32::new();
        crc.update(&header[0..ENTRY_HEADER_LEN]); // bytes 26..29 still zero
        crc.update(&header[ENTRY_HEADER_LEN..ENTRY_HEADER_LEN + fname_len]);
        crc.update(&header[ENTRY_HEADER_LEN + fname_len..]);
        let header_crc = crc.finalize();
        header[26..30].copy_from_slice(&header_crc.to_le_bytes());

        // Seek back, patch, restore position.
        let inner = self.archive.inner_mut();
        inner.seek(SeekFrom::Start(self.header_pos))?;
        inner.write_all(&header)?;
        inner.seek(SeekFrom::Start(end_pos))?;
        Ok(())
    }
}

impl<'a, W: Write + Seek> Drop for EntryWriter<'a, W> {
    fn drop(&mut self) {
        debug_assert!(
            self.finished,
            "EntryWriter dropped without calling finish() — header will be unpatched"
        );
    }
}
