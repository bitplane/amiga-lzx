//! Regression for <https://github.com/.../issues/5>: the year field in
//! the packed date/time is a 4-segment piecewise mapping, not a single
//! `+1970` offset as we used to do. `Test_LZX.lzx` ships inside
//! <http://aminet.net/util/arc/LZX_Y2Kfix.lha> and contains 22 entries
//! named after the year they're dated, which is the ground-truth lookup
//! table for both decoder and encoder.

use std::io::Cursor;

use amiga_lzx::{ArchiveReader, DateTime};

const Y2K_TEST_LZX: &[u8] = include_bytes!("fixtures/y2k_test.lzx");

/// Every entry in `y2k_test.lzx`. The filename is the expected year;
/// day/month/hour/min/sec values come from decoding each entry's raw
/// date bytes with the 4-segment year scheme.
const EXPECTED: &[(&str, u16, u8, u8, u8, u8, u8)] = &[
    //  filename, year, month, day, hour, minute, second
    ("1978", 1978, 1, 1, 0, 1, 2),
    ("1979", 1979, 2, 10, 3, 4, 5),
    ("1980", 1980, 3, 20, 6, 7, 8),
    ("1998", 1998, 4, 30, 9, 10, 11),
    ("1999", 1999, 5, 1, 12, 13, 14),
    ("2000", 2000, 6, 2, 15, 16, 17),
    ("2001", 2001, 7, 3, 18, 19, 20),
    ("2002", 2002, 8, 4, 21, 22, 23),
    ("2003", 2003, 9, 5, 0, 59, 59),
    ("2004", 2004, 10, 6, 1, 58, 58),
    ("2005", 2005, 11, 7, 2, 57, 57),
    ("2006", 2006, 12, 8, 3, 56, 56),
    ("2007", 2007, 1, 9, 4, 55, 55),
    ("2008", 2008, 2, 10, 5, 54, 54),
    ("2010", 2010, 3, 11, 6, 53, 53),
    ("2020", 2020, 4, 12, 7, 52, 52),
    ("2030", 2030, 5, 13, 8, 51, 51),
    ("2033", 2033, 6, 14, 9, 50, 50),
    ("2034", 2034, 7, 15, 10, 49, 49),
    ("2035", 2035, 8, 16, 11, 48, 48),
    ("2040", 2040, 9, 17, 12, 47, 47),
    ("2041", 2041, 10, 18, 13, 46, 46),
];

#[test]
fn y2k_test_archive_decodes_correctly() {
    let mut reader = ArchiveReader::new(Cursor::new(Y2K_TEST_LZX)).unwrap();
    for (name, year, month, day, hour, minute, second) in EXPECTED {
        let entry = reader
            .next_entry()
            .unwrap()
            .unwrap_or_else(|| panic!("missing entry {name}"));
        assert_eq!(entry.filename, *name, "filename mismatch");
        assert_eq!(
            entry.datetime,
            DateTime::try_new(*year, *month, *day, *hour, *minute, *second).unwrap(),
            "datetime mismatch for {name}"
        );
    }
    assert!(reader.next_entry().unwrap().is_none(), "expected EOF");
}

/// The exact sample attached to issue #5 — `date.lzx`, made on
/// 2026-04-19. Confirms the bug described there is gone.
#[test]
fn issue_5_sample_decodes_to_2026_04_19() {
    // The 4 date bytes from date.lzx entry header (offsets 18..22).
    let bytes = [0x99, 0xe4, 0xb8, 0x40];
    let dt = DateTime::unpack(bytes);
    assert_eq!(dt.year, 2026);
    assert_eq!(dt.month, 4);
    assert_eq!(dt.day, 19);
    assert_eq!(dt.hour, 11);
    assert_eq!(dt.minute, 33);
    assert_eq!(dt.second, 0);
}
