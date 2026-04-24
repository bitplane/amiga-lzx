//! End-to-end archive round-trip tests.
//!
//! Compress a payload via [`ArchiveWriter`], decompress via
//! [`ArchiveReader`], and assert byte-for-byte equality plus correct
//! metadata propagation.

use std::io::{Cursor, Write};

use amiga_lzx::{ArchiveReader, ArchiveWriter, DateTime, EntryAttrs, EntryBuilder, Error, Level};

fn round_trip(name: &str, payload: &[u8], level: Level) {
    let buf: Vec<u8> = Vec::new();
    let mut ar = ArchiveWriter::new(Cursor::new(buf)).unwrap();
    let mut entry = ar
        .add_entry(
            EntryBuilder::new(name)
                .level(level)
                .datetime(DateTime::try_new(2026, 4, 13, 3, 47, 22).unwrap())
                .attrs(EntryAttrs::READ | EntryAttrs::WRITE),
        )
        .unwrap();
    entry.write_all(payload).unwrap();
    entry.finish().unwrap();
    let writer = ar.finish().unwrap();
    let bytes = writer.into_inner();

    let mut reader = ArchiveReader::new(Cursor::new(&bytes)).unwrap();
    let entry = reader
        .next_entry()
        .unwrap()
        .expect("expected at least one entry");
    assert_eq!(entry.filename, name);
    assert_eq!(entry.data, payload);
    assert_eq!(
        entry.attrs,
        EntryAttrs::READ | EntryAttrs::WRITE,
        "attrs mismatch"
    );
    assert_eq!(
        entry.datetime,
        DateTime::try_new(2026, 4, 13, 3, 47, 22).unwrap(),
        "datetime mismatch"
    );
    assert!(reader.next_entry().unwrap().is_none(), "expected EOF");
}

#[test]
fn empty_file() {
    round_trip("empty.bin", b"", Level::Normal);
}

#[test]
fn one_byte() {
    round_trip("one.bin", b"x", Level::Normal);
}

#[test]
fn lorem_at_each_level() {
    let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                 Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                 Sed do eiusmod tempor incididunt ut labore et dolore magna \
                 aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
                 ullamco laboris nisi ut aliquip ex ea commodo consequat.";
    round_trip("lorem.txt", data, Level::Quick);
    round_trip("lorem.txt", data, Level::Normal);
    round_trip("lorem.txt", data, Level::Max);
}

#[test]
fn long_zeros_64k() {
    let data = vec![0u8; 64 * 1024];
    round_trip("zeros.bin", &data, Level::Normal);
}

#[test]
fn long_zeros_above_64k() {
    // Crosses one full window — exercises the chunk + block boundary logic.
    let data = vec![0u8; 80 * 1024];
    round_trip("zeros80k.bin", &data, Level::Normal);
}

#[test]
fn random_128k() {
    let mut s: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let mut data = vec![0u8; 128 * 1024];
    for b in data.iter_mut() {
        *b = (next() & 0xff) as u8;
    }
    round_trip("random.bin", &data, Level::Normal);
}

#[test]
fn multi_entry_archive() {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut ar = ArchiveWriter::new(Cursor::new(&mut buf)).unwrap();
        for (name, payload) in &[
            ("first.txt", &b"hello world"[..]),
            ("second.bin", &[0u8; 1000][..]),
            (
                "third.txt",
                &b"the quick brown fox jumps over the lazy dog"[..],
            ),
        ] {
            let mut e = ar.add_entry(EntryBuilder::new(*name)).unwrap();
            e.write_all(payload).unwrap();
            e.finish().unwrap();
        }
        ar.finish().unwrap();
    }

    let mut reader = ArchiveReader::new(Cursor::new(&buf)).unwrap();
    let e1 = reader.next_entry().unwrap().unwrap();
    assert_eq!(e1.filename, "first.txt");
    assert_eq!(e1.data, b"hello world");
    let e2 = reader.next_entry().unwrap().unwrap();
    assert_eq!(e2.filename, "second.bin");
    assert_eq!(e2.data, vec![0u8; 1000]);
    let e3 = reader.next_entry().unwrap().unwrap();
    assert_eq!(e3.filename, "third.txt");
    assert_eq!(e3.data, b"the quick brown fox jumps over the lazy dog");
    assert!(reader.next_entry().unwrap().is_none());
}

/// Hand-craft a 2-entry merged group as raw bytes and verify the
/// reader demultiplexes it correctly. Exercises the merged-group code
/// path that the real Sembiance samples hit but isn't reachable through
/// the encoder (which never emits merged groups).
#[test]
fn handcrafted_merged_group_round_trip() {
    use amiga_lzx::block::BlockWriter;
    use amiga_lzx::constants::{
        ENTRY_HEADER_HOST_OS, ENTRY_HEADER_LEN, ENTRY_HEADER_MACHINE, ENTRY_HEADER_PACK_MODE,
        INFO_HEADER_FLAGS, INFO_HEADER_LEN, INFO_HEADER_MAGIC, INFO_HEADER_VERSION, LEVEL_NORMAL,
    };
    use amiga_lzx::crc32::{crc32, Crc32};
    use amiga_lzx::lz77;

    // Two payloads we'll merge into one shared LZX stream.
    let payload_a = b"This is the first file. It contains some text.";
    let payload_b = b"And this is the second file with different content!";
    let mut combined = Vec::new();
    combined.extend_from_slice(payload_a);
    combined.extend_from_slice(payload_b);

    // Encode the combined stream as one LZX payload using our encoder.
    let tokens = lz77::encode(&combined, &LEVEL_NORMAL);
    let mut bw = BlockWriter::new(Vec::new());
    bw.write_block(&tokens).unwrap();
    let (compressed_payload, _) = bw.finish().unwrap();

    // Build a 10-byte info header (mirroring the writer's layout).
    let mut info = [0u8; INFO_HEADER_LEN];
    info[0..4].copy_from_slice(&INFO_HEADER_MAGIC);
    info[6] = INFO_HEADER_VERSION;
    info[7] = INFO_HEADER_FLAGS;
    let sum: u8 = info.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    info[4] = sum;

    // Helper to build one entry header. The first one carries
    // compressed_size = 0 (interior of group), the second one carries
    // the real size (the tail).
    fn entry_header(
        original_size: u32,
        compressed_size: u32,
        data_crc: u32,
        filename: &str,
    ) -> Vec<u8> {
        let mut h = vec![0u8; ENTRY_HEADER_LEN + filename.len()];
        h[0] = 0x07; // attrs (default)
        h[2..6].copy_from_slice(&original_size.to_le_bytes());
        h[6..10].copy_from_slice(&compressed_size.to_le_bytes());
        h[10] = ENTRY_HEADER_MACHINE;
        h[11] = ENTRY_HEADER_PACK_MODE;
        h[12] = 1; // merged_flag
        h[14] = 0; // comment_len
        h[15] = ENTRY_HEADER_HOST_OS;
        h[18..22].copy_from_slice(&[0, 0, 0, 0]); // datetime
        h[22..26].copy_from_slice(&data_crc.to_le_bytes());
        h[30] = filename.len() as u8;
        h[ENTRY_HEADER_LEN..].copy_from_slice(filename.as_bytes());

        // Compute header CRC: bytes 0..31 with 26..29 zeroed, then
        // filename, then comment (empty here).
        let mut crc = Crc32::new();
        crc.update(&h[0..ENTRY_HEADER_LEN]); // bytes 26..29 are still zero
        crc.update(filename.as_bytes());
        let header_crc = crc.finalize();
        h[26..30].copy_from_slice(&header_crc.to_le_bytes());
        h
    }

    let crc_a = crc32(payload_a);
    let crc_b = crc32(payload_b);

    let mut archive = Vec::new();
    archive.extend_from_slice(&info);
    archive.extend(entry_header(
        payload_a.len() as u32,
        0, // not the tail
        crc_a,
        "first.txt",
    ));
    archive.extend(entry_header(
        payload_b.len() as u32,
        compressed_payload.len() as u32, // tail carries shared payload
        crc_b,
        "second.txt",
    ));
    archive.extend_from_slice(&compressed_payload);

    // Now read it back through ArchiveReader and verify both entries.
    let mut reader = amiga_lzx::ArchiveReader::new(Cursor::new(&archive)).unwrap();
    let e1 = reader.next_entry().unwrap().expect("first entry");
    assert_eq!(e1.filename, "first.txt");
    assert_eq!(e1.data, payload_a);
    assert_eq!(e1.data_crc, crc_a);

    let e2 = reader.next_entry().unwrap().expect("second entry");
    assert_eq!(e2.filename, "second.txt");
    assert_eq!(e2.data, payload_b);
    assert_eq!(e2.data_crc, crc_b);

    assert!(reader.next_entry().unwrap().is_none(), "expected EOF");
}

#[test]
fn entry_with_comment() {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut ar = ArchiveWriter::new(Cursor::new(&mut buf)).unwrap();
        let mut e = ar
            .add_entry(EntryBuilder::new("file.txt").comment("a small note"))
            .unwrap();
        e.write_all(b"payload").unwrap();
        e.finish().unwrap();
        ar.finish().unwrap();
    }
    let mut reader = ArchiveReader::new(Cursor::new(&buf)).unwrap();
    let entry = reader.next_entry().unwrap().unwrap();
    assert_eq!(entry.filename, "file.txt");
    assert_eq!(entry.comment, "a small note");
    assert_eq!(entry.data, b"payload");
}

#[test]
fn latin1_filename_and_comment_round_trip() {
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut ar = ArchiveWriter::new(Cursor::new(&mut buf)).unwrap();
        let mut e = ar
            .add_entry(EntryBuilder::new("¡tsa!.txt").comment("olá"))
            .unwrap();
        e.write_all(b"payload").unwrap();
        e.finish().unwrap();
        ar.finish().unwrap();
    }

    let mut reader = ArchiveReader::new(Cursor::new(&buf)).unwrap();
    let entry = reader.next_entry().unwrap().unwrap();
    assert_eq!(entry.filename, "¡tsa!.txt");
    assert_eq!(entry.comment, "olá");
    assert_eq!(entry.data, b"payload");
}

#[test]
fn non_latin1_filename_is_rejected() {
    let mut ar = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    match ar.add_entry(EntryBuilder::new("snowman-☃.txt")) {
        Err(Error::FilenameNotLatin1) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected non-Latin-1 filename to be rejected"),
    };
}

#[test]
fn non_latin1_comment_is_rejected() {
    let mut ar = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
    match ar.add_entry(EntryBuilder::new("file.txt").comment("snowman-☃")) {
        Err(Error::CommentNotLatin1) => {}
        Err(other) => panic!("unexpected error: {other:?}"),
        Ok(_) => panic!("expected non-Latin-1 comment to be rejected"),
    };
}
