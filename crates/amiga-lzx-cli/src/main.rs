use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;

use amiga_lzx::{ArchiveReader, ArchiveWriter, DateTime, EntryBuilder, Level};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "lzx", version, about = "Amiga LZX archiver")]
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
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no input files",
        ));
    }
    let inputs = collect_inputs(roots)?;
    if inputs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no regular files found",
        ));
    }
    if archive_matches_input(archive, &inputs)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "archive output path is also an input file",
        ));
    }

    let out = BufWriter::new(File::create(archive)?);
    let mut ar = ArchiveWriter::new(out).map_err(|e| io::Error::other(e.to_string()))?;

    for (path, name) in &inputs {
        let data = fs::read(path)?;
        let datetime = entry_datetime(path);
        let mut entry = ar
            .add_entry(
                EntryBuilder::new(name.clone())
                    .level(level)
                    .datetime(datetime),
            )
            .map_err(|e| io::Error::other(e.to_string()))?;
        entry.write_all(&data)?;
        entry
            .finish()
            .map_err(|e| io::Error::other(e.to_string()))?;
        eprintln!("  + {} ({} bytes)", name, data.len());
    }
    ar.finish().map_err(|e| io::Error::other(e.to_string()))?;
    Ok(())
}

fn archive_matches_input(archive: &Path, inputs: &[(PathBuf, String)]) -> io::Result<bool> {
    let archive_path = canonical_output_path(archive)?;
    for (path, _) in inputs {
        if fs::canonicalize(path)? == archive_path {
            return Ok(true);
        }
    }
    Ok(false)
}

fn canonical_output_path(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path);
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = match parent {
        Some(p) => fs::canonicalize(p)?,
        None => std::env::current_dir()?,
    };
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "archive output path has no filename",
        )
    })?;
    Ok(parent.join(file_name))
}

fn extract(archive: &Path, outdir: &Path) -> io::Result<()> {
    fs::create_dir_all(outdir)?;
    let mut reader = open_reader(archive)?;
    while let Some(entry) = reader
        .next_entry()
        .map_err(|e| io::Error::other(e.to_string()))?
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
        .map_err(|e| io::Error::other(e.to_string()))?
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
        .map_err(|e| io::Error::other(e.to_string()))?
    {
        count += 1;
        eprintln!("  ok {}", entry.filename);
    }
    eprintln!("{count} entries verified");
    Ok(())
}

fn open_reader(archive: &Path) -> io::Result<ArchiveReader<BufReader<File>>> {
    let f = BufReader::new(File::open(archive)?);
    ArchiveReader::new(f).map_err(|e| io::Error::other(e.to_string()))
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
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF8 filename"))?
                .to_owned();
            out.push((root.clone(), name));
        } else if ft.is_dir() {
            // Use the directory's own basename as the prefix.
            let prefix = root
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "non-UTF8 directory"))?
                .to_owned();
            walk_dir(root, &prefix, &mut out)?;
        } else {
            eprintln!("warning: skipping non-regular {}", root.display());
        }
    }
    Ok(out)
}

fn walk_dir(dir: &Path, prefix: &str, out: &mut Vec<(PathBuf, String)>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
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
/// Rejects absolute paths, parent traversal, Windows separators, drive
/// prefixes, and alternate-data-stream syntax.
fn sanitize_archive_path(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains('\\')
        || name.contains(':')
    {
        return None;
    }

    let path = Path::new(name);
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => out.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizer_accepts_relative_archive_paths() {
        assert_eq!(
            sanitize_archive_path("dir/sub/file.txt").unwrap(),
            PathBuf::from("dir").join("sub").join("file.txt")
        );
        assert_eq!(
            sanitize_archive_path("./file.txt").unwrap(),
            PathBuf::from("file.txt")
        );
    }

    #[test]
    fn sanitizer_rejects_paths_that_can_escape_or_target_windows_roots() {
        for name in [
            "",
            "/abs.txt",
            "../evil.txt",
            "dir/../../evil.txt",
            r"..\evil.txt",
            r"C:\tmp\evil.txt",
            "C:evil.txt",
            "file.txt:stream",
        ] {
            assert!(sanitize_archive_path(name).is_none(), "{name}");
        }
    }

    #[test]
    fn archive_output_matching_input_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("same.lzx");
        fs::write(&input, b"original").unwrap();
        let inputs = vec![(input.clone(), "same.lzx".to_owned())];

        assert!(archive_matches_input(&input, &inputs).unwrap());
    }

    #[test]
    fn create_refuses_to_overwrite_its_own_input() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("same.lzx");
        fs::write(&input, b"original").unwrap();

        let err = create(&input, std::slice::from_ref(&input), Level::Normal).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(fs::read(&input).unwrap(), b"original");
    }

    #[test]
    fn archive_output_in_same_dir_with_different_name_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.txt");
        fs::write(&input, b"original").unwrap();
        let archive = dir.path().join("out.lzx");
        let inputs = vec![(input, "input.txt".to_owned())];

        assert!(!archive_matches_input(&archive, &inputs).unwrap());
    }
}
