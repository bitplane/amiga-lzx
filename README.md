# Amiga LZX in Rust

LLM assisted clean-ish-room implementation of Amiga LZX.

Made by decompiling the compressor with Ghidra, writing spec documents for it
and using unlzx.c to document the decompressor, then hiding the sources and
re-implementing from the spec.

## Install

From crates.io:

```bash
cargo install amiga-lzx-cli
```

This installs a binary called `lzx` (the historic Amiga tool name).
Pre-built binaries for Linux (x86_64 / aarch64), macOS (universal),
and Windows are also attached to each [GitHub release][releases].

[releases]: https://github.com/bitplane/amiga-lzx/releases

## Usage

```bash
# Create an archive from files and/or directories (walked recursively).
lzx c out.lzx file1 file2 mydir/

# List entries in an archive.
lzx l out.lzx

# Extract everything into ./out (or any directory).
lzx x out.lzx ./out

# Verify an archive without writing files.
lzx t out.lzx
```

Compression level: `-1` quick, `-2` normal (default), `-3` maximum —
matching the original Amiga `lzx` tool.

```bash
lzx -3 c best.lzx mydir/
```

File modification times are preserved on both create and extract.

### Library

```toml
[dependencies]
amiga-lzx = "0.1"
```

```rust
use std::io::{Cursor, Write};
use amiga_lzx::{ArchiveWriter, ArchiveReader, EntryBuilder, Level};

let mut buf = Cursor::new(Vec::new());
let mut ar = ArchiveWriter::new(&mut buf)?;
let mut entry = ar.add_entry(EntryBuilder::new("hello.txt").level(Level::Normal))?;
entry.write_all(b"hello, world")?;
entry.finish()?;
ar.finish()?;

let mut reader = ArchiveReader::new(Cursor::new(buf.into_inner()))?;
while let Some(entry) = reader.next_entry()? {
    println!("{} ({} bytes)", entry.filename, entry.data.len());
}
# Ok::<(), amiga_lzx::Error>(())
```

## Links

* [🏠 home](https://bitplane.net/dev/rust/amiga-lzx)
* [🐱 source](https://github.com/bitplane/amiga-lzx)
* [🦀 crates.io](https://crates.io/users/bitplane)
  * [📦 cli](https://crates.io/crates/amiga-lzx-cli)
  * [📦 library](https://crates.io/crates/amiga-lzx)

## License

WTFPL with warranty clause: don't blame me.

