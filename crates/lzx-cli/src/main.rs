use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lzx::{ArchiveReader, ArchiveWriter, EntryBuilder, Level};

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
    /// Create an archive from one or more files.
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

fn create(archive: &Path, files: &[PathBuf], level: Level) -> io::Result<()> {
    if files.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "no input files"));
    }
    let out = BufWriter::new(File::create(archive)?);
    let mut ar =
        ArchiveWriter::new(out).map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
    for path in files {
        let data = fs::read(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad filename"))?;
        let mut entry = ar
            .add_entry(EntryBuilder::new(name).level(level))
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
        let dst = outdir.join(&entry.filename);
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&dst, &entry.data)?;
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
