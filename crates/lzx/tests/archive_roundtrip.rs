//! End-to-end archive round-trip tests.
//!
//! Compress a payload via [`ArchiveWriter`], decompress via
//! [`ArchiveReader`], and assert byte-for-byte equality plus correct
//! metadata propagation.

use std::io::{Cursor, Write};

use lzx::{ArchiveReader, ArchiveWriter, DateTime, EntryAttrs, EntryBuilder, Level};

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
            ("third.txt", &b"the quick brown fox jumps over the lazy dog"[..]),
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
