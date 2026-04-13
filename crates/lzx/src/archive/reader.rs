//! Archive reader. Streams entries from a `Read` source.

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
        Ok(ArchiveReader { inner })
    }

    /// Read the next entry. Returns `Ok(None)` cleanly at end of archive.
    pub fn next_entry(&mut self) -> Result<Option<Entry>> {
        // Try to read the 31-byte fixed header. EOF here = end of archive.
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

        if merged_flag != 0 {
            // We can read merged-group entries, but for v1 we don't fully
            // support them — they require carrying decoder state across
            // multiple entries. Surface a clear error.
            return Err(Error::InvalidArchive(
                "merged-group archives not yet supported",
            ));
        }

        let mut filename = vec![0u8; filename_len];
        self.inner
            .read_exact(&mut filename)
            .map_err(|_| Error::Truncated)?;
        let mut comment = vec![0u8; comment_len];
        self.inner
            .read_exact(&mut comment)
            .map_err(|_| Error::Truncated)?;

        // Verify the header CRC.
        let mut crc = Crc32::new();
        let mut hdr_for_crc = fixed;
        hdr_for_crc[26..30].fill(0);
        crc.update(&hdr_for_crc);
        crc.update(&filename);
        crc.update(&comment);
        let computed_header_crc = crc.finalize();
        if computed_header_crc != header_crc {
            return Err(Error::CrcMismatch {
                expected: header_crc,
                actual: computed_header_crc,
            });
        }

        // Read the compressed payload.
        let mut payload = vec![0u8; compressed_size as usize];
        self.inner
            .read_exact(&mut payload)
            .map_err(|_| Error::Truncated)?;

        // Decode according to pack_mode (ALGORITHM.md §11; matches the
        // dispatch in unlzx.c around line 945):
        //   0 → stored (raw payload bytes)
        //   2 → normal LZX-compressed
        //   anything else → unknown, surface a clear error
        let data = if original_size == 0 {
            Vec::new()
        } else {
            match pack_mode {
                0 => {
                    if payload.len() as u32 != original_size {
                        return Err(Error::InvalidArchive(
                            "stored entry size does not match payload length",
                        ));
                    }
                    payload.clone()
                }
                2 => decoder::decode(&payload, original_size as usize)?,
                _ => {
                    return Err(Error::InvalidArchive("unknown pack mode"));
                }
            }
        };
        let computed_data_crc = crc32(&data);
        if computed_data_crc != data_crc {
            return Err(Error::CrcMismatch {
                expected: data_crc,
                actual: computed_data_crc,
            });
        }

        Ok(Some(Entry {
            filename: String::from_utf8_lossy(&filename).into_owned(),
            comment: String::from_utf8_lossy(&comment).into_owned(),
            attrs,
            datetime,
            data,
            data_crc,
        }))
    }
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
