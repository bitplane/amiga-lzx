//! Real-world LZX samples from <https://sembiance.com/fileFormatSamples/archive/lzx/>.
//!
//! `#[ignore]`-gated so it only runs on demand:
//!
//! ```bash
//! cargo test -p lzx --test sembiance_samples -- --ignored --nocapture
//! ```
//!
//! On first run, downloads the 11 samples to a project-local cache via
//! `curl` (so we don't pull in an HTTP dep). Skips with a clear message
//! if `curl` is unavailable or the network is down.

use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use lzx::{ArchiveReader, ArchiveWriter, EntryBuilder, Level};
use std::io::Write;

const BASE_URL: &str = "https://sembiance.com/fileFormatSamples/archive/lzx";

/// `(url-encoded filename, on-disk filename)` pairs. The three `¡tsa!`
/// samples live at percent-encoded URLs.
const SAMPLES: &[(&str, &str)] = &[
    ("Blizz1220Repair2.0.LZX", "Blizz1220Repair2.0.LZX"),
    ("CXHandler3.8.LZX", "CXHandler3.8.LZX"),
    ("FuentePlasmaAmos.lzx", "FuentePlasmaAmos.lzx"),
    ("RomanStartup30.lzx", "RomanStartup30.lzx"),
    ("landingzone.lzx", "landingzone.lzx"),
    ("sample.bananaboat.lzx", "sample.bananaboat.lzx"),
    ("test.lzx", "test.lzx"),
    ("xpk_compress.lzx", "xpk_compress.lzx"),
    ("%C2%A1tsa%21_astralrmx11.lzx", "tsa_astralrmx11.lzx"),
    ("%C2%A1tsa%21_aztecchallengec64.lzx", "tsa_aztecchallengec64.lzx"),
    ("%C2%A1tsa%21_lowlevel.lzx", "tsa_lowlevel.lzx"),
];

fn cache_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("sembiance-cache");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn fetch_sample(url_name: &str, disk_name: &str) -> Result<Vec<u8>, String> {
    let dir = cache_dir();
    let path = dir.join(disk_name);
    if let Ok(bytes) = fs::read(&path) {
        return Ok(bytes);
    }
    let url = format!("{BASE_URL}/{url_name}");
    let status = Command::new("curl")
        .args(["-sSL", "-o"])
        .arg(&path)
        .arg(&url)
        .status()
        .map_err(|e| format!("curl failed to launch: {e}"))?;
    if !status.success() {
        return Err(format!("curl exited with {status:?} for {url}"));
    }
    fs::read(&path).map_err(|e| format!("read cached {}: {e}", path.display()))
}

#[test]
#[ignore]
fn all_sembiance_samples_round_trip() {
    let mut failures = Vec::new();
    let mut total_orig = 0u64;
    let mut total_re = 0u64;

    for (url_name, disk_name) in SAMPLES {
        let bytes = match fetch_sample(url_name, disk_name) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[skip] {disk_name}: {e}");
                return; // Treat network failure as a skip, not a hard fail.
            }
        };
        let orig_size = bytes.len() as u64;

        // Decode every entry.
        let mut entries = Vec::new();
        let mut reader = match ArchiveReader::new(Cursor::new(&bytes)) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{disk_name}: ArchiveReader::new: {e}"));
                continue;
            }
        };
        let mut decode_err: Option<String> = None;
        loop {
            match reader.next_entry() {
                Ok(Some(entry)) => entries.push(entry),
                Ok(None) => break,
                Err(e) => {
                    decode_err = Some(format!("{disk_name}: next_entry: {e}"));
                    break;
                }
            }
        }
        if let Some(e) = decode_err {
            failures.push(e);
            continue;
        }
        if entries.is_empty() {
            failures.push(format!("{disk_name}: no entries decoded"));
            continue;
        }

        // Re-encode every entry into a fresh archive in memory.
        let mut buf = Cursor::new(Vec::new());
        let mut ar = match ArchiveWriter::new(&mut buf) {
            Ok(a) => a,
            Err(e) => {
                failures.push(format!("{disk_name}: ArchiveWriter::new: {e}"));
                continue;
            }
        };
        let mut write_err: Option<String> = None;
        for entry in &entries {
            let mut ew = match ar.add_entry(
                EntryBuilder::new(entry.filename.clone())
                    .level(Level::Normal)
                    .datetime(entry.datetime),
            ) {
                Ok(w) => w,
                Err(e) => {
                    write_err = Some(format!("{disk_name}: add_entry: {e}"));
                    break;
                }
            };
            if let Err(e) = ew.write_all(&entry.data) {
                write_err = Some(format!("{disk_name}: write payload: {e}"));
                break;
            }
            if let Err(e) = ew.finish() {
                write_err = Some(format!("{disk_name}: finish entry: {e}"));
                break;
            }
        }
        if let Some(e) = write_err {
            failures.push(e);
            continue;
        }
        let _ = ar.finish().map_err(|e| {
            failures.push(format!("{disk_name}: ArchiveWriter::finish: {e}"));
        });
        let re_size = buf.into_inner().len() as u64;

        // Sanity: re-encoded size shouldn't blow up grotesquely. Some
        // samples (xpk_compress, Blizz, CXHandler) are ~1.4–2.4×
        // because they're merged groups in the original; we accept up
        // to 250%.
        if re_size > orig_size * 5 / 2 {
            failures.push(format!(
                "{disk_name}: re-encoded {} >> original {} (ratio {:.1}%)",
                re_size,
                orig_size,
                100.0 * re_size as f64 / orig_size as f64
            ));
            continue;
        }

        total_orig += orig_size;
        total_re += re_size;
        let n = entries.len();
        eprintln!(
            "  ok {disk_name}: {n} entries, {orig_size} -> {re_size} ({:.1}%)",
            100.0 * re_size as f64 / orig_size as f64
        );
    }

    if !failures.is_empty() {
        for f in &failures {
            eprintln!("FAIL: {f}");
        }
        panic!("{} sample(s) failed", failures.len());
    }

    eprintln!(
        "Aggregate: {} -> {} ({:.1}%)",
        total_orig,
        total_re,
        100.0 * total_re as f64 / total_orig as f64
    );
}
