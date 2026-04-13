//! Archive reader. Streams entries from a `Read` source.
//!
//! Handles three layouts:
//!
//! 1. **Single-entry** (`merged_flag = 0`) — one header + payload, normal.
//! 2. **Stored single-entry** (`merged_flag = 0, pack_mode = 0`) — header
//!    + raw payload bytes equal to `original_size`.
//! 3. **Merged group** (consecutive entries with `merged_flag = 1`,
//!    only the tail carrying `compressed_size > 0`) — one shared LZX
//!    stream that decompresses to the concatenation of all entries'
//!    `original_size` byte counts. Per-entry slices are demultiplexed
//!    from the decoded byte stream and verified with each entry's own
//!    CRC32 from its header.

use std::collections::VecDeque;
use std::io::Read;

use crate::archive::{DateTime, EntryAttrs};
use crate::constants::{ENTRY_HEADER_LEN, INFO_HEADER_LEN};
use crate::crc32::{crc32, Crc32};
use crate::decoder;
use crate::error::{Error, Result};

/// One decoded archive entry.
#[derive(Debug, Clone)]
pub struct Entry {
    pub filename: String,
    pub comment: String,
    pub attrs: EntryAttrs,
    pub datetime: DateTime,
    pub data: Vec<u8>,
    /// CRC32 from the entry header (already verified against `data`).
    pub data_crc: u32,
}

pub struct ArchiveReader<R: Read> {
    inner: R,
    /// Pre-decoded entries waiting to be returned by `next_entry`. Filled
    /// lazily by the merged-group path.
    queue: VecDeque<Entry>,
}

/// Per-entry header metadata, parsed from the 31-byte fixed header plus
/// the variable-length filename and comment bytes that follow it.
#[derive(Debug)]
struct EntryMeta {
    attrs: EntryAttrs,
    original_size: u32,
    compressed_size: u32,
    pack_mode: u8,
    merged_flag: u8,
    datetime: DateTime,
    data_crc: u32,
    filename: Vec<u8>,
    comment: Vec<u8>,
}

impl<R: Read> ArchiveReader<R> {
    /// Read and validate the 10-byte info header. Only the magic bytes
    /// `b"LZX"` are checked, matching `unlzx`.
    pub fn new(mut inner: R) -> Result<Self> {
        let mut hdr = [0u8; INFO_HEADER_LEN];
        inner
            .read_exact(&mut hdr)
            .map_err(|_| Error::InvalidArchive("truncated info header"))?;
        if &hdr[0..3] != b"LZX" {
            return Err(Error::InvalidArchive("not an LZX archive"));
        }
        Ok(ArchiveReader {
            inner,
            queue: VecDeque::new(),
        })
    }

    /// Read the next entry. Returns `Ok(None)` cleanly at end of archive.
    pub fn next_entry(&mut self) -> Result<Option<Entry>> {
        if let Some(buffered) = self.queue.pop_front() {
            return Ok(Some(buffered));
        }

        let first = match self.read_entry_meta()? {
            Some(m) => m,
            None => return Ok(None),
        };

        if first.merged_flag == 0 {
            return self.decode_single_entry(first).map(Some);
        }

        // Merged group: collect entries until we hit one with
        // `compressed_size > 0` (the tail carrying the shared payload).
        let mut group = vec![first];
        while group.last().unwrap().compressed_size == 0 {
            let next = self
                .read_entry_meta()?
                .ok_or(Error::Truncated)?;
            if next.merged_flag == 0 {
                return Err(Error::InvalidArchive(
                    "merged-group entry without merged_flag",
                ));
            }
            group.push(next);
        }

        let tail_compressed = group.last().unwrap().compressed_size as usize;
        let total_uncompressed: u64 =
            group.iter().map(|m| m.original_size as u64).sum();

        let mut payload = vec![0u8; tail_compressed];
        self.inner
            .read_exact(&mut payload)
            .map_err(|_| Error::Truncated)?;

        // Decode the entire merged stream as one continuous LZX payload.
        // Block boundaries inside the stream don't align with file
        // boundaries — the decoder produces one big buffer, we slice it.
        let decoded = decoder::decode(&payload, total_uncompressed as usize)?;

        let mut cursor = 0usize;
        for meta in group {
            let end = cursor + meta.original_size as usize;
            if end > decoded.len() {
                return Err(Error::InvalidArchive(
                    "merged group entry slice runs past decoded length",
                ));
            }
            let slice = decoded[cursor..end].to_vec();
            cursor = end;
            let computed = crc32(&slice);
            if computed != meta.data_crc {
                return Err(Error::CrcMismatch {
                    expected: meta.data_crc,
                    actual: computed,
                });
            }
            self.queue.push_back(Entry {
                filename: latin1_to_string(&meta.filename),
                comment: latin1_to_string(&meta.comment),
                attrs: meta.attrs,
                datetime: meta.datetime,
                data: slice,
                data_crc: meta.data_crc,
            });
        }

        Ok(self.queue.pop_front())
    }

    /// Read one entry's fixed header + filename + comment + verify the
    /// header CRC. Returns `Ok(None)` on a clean EOF before any header
    /// bytes are consumed (the natural end-of-archive case).
    fn read_entry_meta(&mut self) -> Result<Option<EntryMeta>> {
        let mut fixed = [0u8; ENTRY_HEADER_LEN];
        match read_fully_or_eof(&mut self.inner, &mut fixed)? {
            0 => return Ok(None),
            n if n < ENTRY_HEADER_LEN => return Err(Error::Truncated),
            _ => {}
        }

        let attrs = EntryAttrs::from_bits_retain(fixed[0]);
        let original_size = u32::from_le_bytes(fixed[2..6].try_into().unwrap());
        let compressed_size = u32::from_le_bytes(fixed[6..10].try_into().unwrap());
        let pack_mode = fixed[11];
        let merged_flag = fixed[12];
        let comment_len = fixed[14] as usize;
        let date_bytes: [u8; 4] = fixed[18..22].try_into().unwrap();
        let datetime = DateTime::unpack(date_bytes);
        let data_crc = u32::from_le_bytes(fixed[22..26].try_into().unwrap());
        let header_crc = u32::from_le_bytes(fixed[26..30].try_into().unwrap());
        let filename_len = fixed[30] as usize;

        let mut filename = vec![0u8; filename_len];
        self.inner
            .read_exact(&mut filename)
            .map_err(|_| Error::Truncated)?;
        let mut comment = vec![0u8; comment_len];
        self.inner
            .read_exact(&mut comment)
            .map_err(|_| Error::Truncated)?;

        // Verify the header CRC: bytes 0..31 with bytes 26..30 zeroed,
        // followed by the filename bytes, followed by the comment bytes.
        let mut crc = Crc32::new();
        let mut hdr_for_crc = fixed;
        hdr_for_crc[26..30].fill(0);
        crc.update(&hdr_for_crc);
        crc.update(&filename);
        crc.update(&comment);
        let computed = crc.finalize();
        if computed != header_crc {
            return Err(Error::CrcMismatch {
                expected: header_crc,
                actual: computed,
            });
        }

        Ok(Some(EntryMeta {
            attrs,
            original_size,
            compressed_size,
            pack_mode,
            merged_flag,
            datetime,
            data_crc,
            filename,
            comment,
        }))
    }

    /// Decode a non-merged entry. Reads `compressed_size` payload bytes,
    /// dispatches on `pack_mode`, verifies the data CRC, and returns the
    /// resulting `Entry`.
    fn decode_single_entry(&mut self, meta: EntryMeta) -> Result<Entry> {
        let mut payload = vec![0u8; meta.compressed_size as usize];
        self.inner
            .read_exact(&mut payload)
            .map_err(|_| Error::Truncated)?;

        // ALGORITHM.md §11 "Pack mode dispatch":
        //   0 → stored (raw payload, length must equal original_size)
        //   2 → normal LZX-compressed
        //   anything else → unknown
        let data = if meta.original_size == 0 {
            Vec::new()
        } else {
            match meta.pack_mode {
                0 => {
                    if payload.len() as u32 != meta.original_size {
                        return Err(Error::InvalidArchive(
                            "stored entry size does not match payload length",
                        ));
                    }
                    payload
                }
                2 => decoder::decode(&payload, meta.original_size as usize)?,
                _ => return Err(Error::InvalidArchive("unknown pack mode")),
            }
        };

        let computed = crc32(&data);
        if computed != meta.data_crc {
            return Err(Error::CrcMismatch {
                expected: meta.data_crc,
                actual: computed,
            });
        }

        Ok(Entry {
            filename: latin1_to_string(&meta.filename),
            comment: latin1_to_string(&meta.comment),
            attrs: meta.attrs,
            datetime: meta.datetime,
            data,
            data_crc: meta.data_crc,
        })
    }
}

/// Lossless ISO-8859-1 → UTF-8 conversion. Amiga LZX filenames use
/// Latin-1 (e.g. `0xa1` for `¡`); a naive `from_utf8_lossy` would
/// substitute replacement characters and break round-trips on such
/// names. Mapping each byte directly to a `char` in `0..=0xff`
/// produces the corresponding Unicode codepoint, which `String`
/// stores as UTF-8.
fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

fn read_fully_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => return Ok(filled),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Ok(filled)
}
