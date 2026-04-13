use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lzx::{ArchiveReader, ArchiveWriter, DateTime, EntryBuilder, Level};

#[derive(Parser, Debug)]
#[command(name = "lzx", version, about = "LZX (Amiga) archiver")]
struct Args {
    /// Quick compression (lazy threshold 1).
    #[arg(short = '1', global = true)]
    quick: bool,
    /// Normal compression (default; lazy threshold 7).
    #[arg(short = '2', global = true)]
    normal: bool,
    /// Maximum compression (lazy threshold 40 + multi-step).
    #[arg(short = '3', global = true)]
    max: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Create an archive from one or more files or directories.
    /// Directories are walked recursively, preserving relative paths.
    #[command(alias = "create")]
    C {
        archive: PathBuf,
        files: Vec<PathBuf>,
    },
    /// Extract all entries from an archive.
    #[command(alias = "extract")]
    X {
        archive: PathBuf,
        #[arg(default_value = ".")]
        outdir: PathBuf,
    },
    /// List the entries in an archive.
    #[command(alias = "list")]
    L { archive: PathBuf },
    /// Verify an archive by decoding it without writing files.
    #[command(alias = "test")]
    T { archive: PathBuf },
}

impl Args {
    fn level(&self) -> Level {
        match (self.quick, self.normal, self.max) {
            (true, _, _) => Level::Quick,
            (_, _, true) => Level::Max,
            _ => Level::Normal,
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    let level = args.level();

    let result = match args.command {
        Command::C { archive, files } => create(&archive, &files, level),
        Command::X { archive, outdir } => extract(&archive, &outdir),
        Command::L { archive } => list(&archive),
        Command::T { archive } => test(&archive),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lzx: {e}");
            ExitCode::FAILURE
        }
    }
}

fn create(archive: &Path, roots: &[PathBuf], level: Level) -> io::Result<()> {
    if roots.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no input files"));
    }
    let inputs = collect_inputs(roots)?;
    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no regular files found",
        ));
    }

    let out = BufWriter::new(File::create(archive)?);
    let mut ar =
        ArchiveWriter::new(out).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;

    for (path, name) in &inputs {
        let data = fs::read(path)?;
        let datetime = entry_datetime(path);
        let mut entry = ar
            .add_entry(
                EntryBuilder::new(name.clone())
                    .level(level)
                    .datetime(datetime),
            )
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        entry.write_all(&data)?;
        entry
            .finish()
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        eprintln!("  + {} ({} bytes)", name, data.len());
    }
    ar.finish()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    Ok(())
}

fn extract(archive: &Path, outdir: &Path) -> io::Result<()> {
    fs::create_dir_all(outdir)?;
    let mut reader = open_reader(archive)?;
    while let Some(entry) = reader
        .next_entry()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
    {
        // Defensive: refuse absolute paths and parent-traversal segments
        // so a malicious archive can't write outside outdir.
        let safe_name = sanitize_archive_path(&entry.filename).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsafe archive path: {}", entry.filename),
            )
        })?;
        let dst = outdir.join(&safe_name);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dst, &entry.data)?;

        // Restore mtime from the entry header. Best-effort — failures
        // here don't abort the whole extraction.
        let mtime = entry.datetime.to_system_time();
        if let Err(e) = File::options()
            .write(true)
            .open(&dst)
            .and_then(|f| f.set_modified(mtime))
        {
            eprintln!("warning: could not set mtime on {}: {e}", dst.display());
        }

        eprintln!("  - {} ({} bytes)", entry.filename, entry.data.len());
    }
    Ok(())
}

fn list(archive: &Path) -> io::Result<()> {
    let mut reader = open_reader(archive)?;
    println!("{:>10}  {:>10}  {:<}", "size", "crc32", "name");
    while let Some(entry) = reader
        .next_entry()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
    {
        println!(
            "{:>10}  {:>08x}  {}",
            entry.data.len(),
            entry.data_crc,
            entry.filename
        );
    }
    Ok(())
}

fn test(archive: &Path) -> io::Result<()> {
    let mut reader = open_reader(archive)?;
    let mut count = 0;
    while let Some(entry) = reader
        .next_entry()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?
    {
        count += 1;
        eprintln!("  ok {}", entry.filename);
    }
    eprintln!("{count} entries verified");
    Ok(())
}

fn open_reader(archive: &Path) -> io::Result<ArchiveReader<BufReader<File>>> {
    let f = BufReader::new(File::open(archive)?);
    ArchiveReader::new(f).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
}

/// Read the mtime from a file's metadata, convert it to a
/// `DateTime`, and warn if it had to be clamped to the LZX year range.
fn entry_datetime(path: &Path) -> DateTime {
    match fs::metadata(path).and_then(|m| m.modified()) {
        Ok(mtime) => {
            let (dt, clamped) = DateTime::from_system_time_clamped(mtime);
            if clamped {
                eprintln!(
                    "warning: mtime out of LZX range for {} — clamped",
                    path.display()
                );
            }
            dt
        }
        Err(_) => DateTime::ZERO,
    }
}

/// Walk the input arguments and produce `(path, archive_filename)` pairs.
///
/// - File argument → one pair using the file's basename.
/// - Directory argument → recursive walk; archive filenames are the
///   relative path inside the directory **prefixed by the directory's
///   basename**, joined with forward slashes (matching `tar` / `zip`).
///
/// Entries within each directory are sorted for deterministic output.
/// Symlinks are skipped to avoid escaping the source tree.
fn collect_inputs(roots: &[PathBuf]) -> io::Result<Vec<(PathBuf, String)>> {
    let mut out = Vec::new();
    for root in roots {
        let meta = fs::symlink_metadata(root)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            eprintln!("warning: skipping symlink {}", root.display());
            continue;
        }
        if ft.is_file() {
            let name = root
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "non-UTF8 filename")
                })?
                .to_owned();
            out.push((root.clone(), name));
        } else if ft.is_dir() {
            // Use the directory's own basename as the prefix.
            let prefix = root
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "non-UTF8 directory")
                })?
                .to_owned();
            walk_dir(root, &prefix, &mut out)?;
        } else {
            eprintln!("warning: skipping non-regular {}", root.display());
        }
    }
    Ok(out)
}

fn walk_dir(dir: &Path, prefix: &str, out: &mut Vec<(PathBuf, String)>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for ent in entries {
        let path = ent.path();
        let meta = fs::symlink_metadata(&path)?;
        let ft = meta.file_type();
        if ft.is_symlink() {
            eprintln!("warning: skipping symlink {}", path.display());
            continue;
        }
        let basename = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_owned(),
            None => {
                eprintln!("warning: skipping non-UTF8 path {}", path.display());
                continue;
            }
        };
        let archive_name = format!("{prefix}/{basename}");
        if ft.is_file() {
            out.push((path, archive_name));
        } else if ft.is_dir() {
            walk_dir(&path, &archive_name, out)?;
        }
    }
    Ok(())
}

/// Reject archive filenames that would escape the extraction directory.
/// Strips a leading slash and rejects any `..` segment.
fn sanitize_archive_path(name: &str) -> Option<PathBuf> {
    let trimmed = name.trim_start_matches('/');
    let mut out = PathBuf::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            return None;
        }
        out.push(segment);
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}
