# LZX Constants

All constants have now been **cross-verified against the canonical `unlzx.c`
decompressor source** (Aminet: misc/unix/unlzx.c.gz). Where `unlzx.c` has a
cleaner or more definitive form of a constant, that form is preferred.

---

## Position/length slot tables (from `unlzx.c`)

These are the **authoritative canonical tables**. The compressor's
extracted data exactly matches.

### `table_one[32]` — footer bit counts per slot
```c
static const unsigned char table_one[32] = {
    0,0,0,0, 1,1, 2,2, 3,3, 4,4, 5,5, 6,6,
    7,7, 8,8, 9,9, 10,10, 11,11, 12,12, 13,13, 14,14
};
```

### `table_two[32]` — base value per slot (slot → minimum value covered)
```c
static const unsigned int table_two[32] = {
    0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192,
    256, 384, 512, 768, 1024, 1536, 2048, 3072, 4096, 6144,
    8192, 12288, 16384, 24576, 32768, 49152
};
```

Position range covered by slot `s`: `[table_two[s], table_two[s] + (1 << table_one[s]))`

So:
- slot 0 → position 0 (**repeat-last-offset signal**)
- slot 1 → position 1
- slot 2 → position 2
- slot 3 → position 3
- slot 4 → positions 4-5 (1 footer bit)
- slot 5 → positions 6-7
- slot 6 → positions 8-11 (2 footer bits)
- slot 7 → positions 12-15
- slot 8 → positions 16-23 (3 bits)
- slot 9 → positions 24-31
- slot 10 → positions 32-47 (4 bits)
- slot 11 → positions 48-63
- slot 12 → positions 64-95 (5 bits)
- slot 13 → positions 96-127
- slot 14 → positions 128-191 (6 bits)
- slot 15 → positions 192-255
- slot 16 → positions 256-383 (7 bits)
- slot 17 → positions 384-511
- slot 18 → positions 512-767 (8 bits)
- slot 19 → positions 768-1023
- slot 20 → positions 1024-1535 (9 bits)
- slot 21 → positions 1536-2047
- slot 22 → positions 2048-3071 (10 bits)
- slot 23 → positions 3072-4095
- slot 24 → positions 4096-6143 (11 bits)
- slot 25 → positions 6144-8191
- slot 26 → positions 8192-12287 (12 bits)
- slot 27 → positions 12288-16383
- slot 28 → positions 16384-24575 (13 bits)
- slot 29 → positions 24576-32767
- slot 30 → positions 32768-49151 (14 bits)
- slot 31 → positions 49152-65535

**Slot 0 is reserved for the repeat-match cache** — it means "use the
previous match's distance" (`last_offset`).

### `table_three[16]` — mask per footer bit count
```c
static const unsigned int table_three[16] = {
    0, 1, 3, 7, 15, 31, 63, 127, 255, 511, 1023, 2047,
    4095, 8191, 16383, 32767
};
```
`table_three[n] = (1 << n) - 1`, used to extract footer bits: `footer = control & table_three[table_one[slot]]`.

### Compressor's per-slot encoder tables (embedded in code)

Unlike the decoder, the compressor does **not** store `table_one` and
`table_three` indexed by bit count. Instead, it stores pre-indexed-by-slot
tables embedded in the code section (two independent copies — one for
verbatim, one for aligned blocks, due to 68k addressing):

- `table_one_per_slot[32]` at `0x00011556` (verbatim) / `0x00011730` (aligned)
  — 32 bytes, value is the footer bit count for that slot (values 0..14
  matching `table_one` above).
- `table_three_per_slot[32]` at `0x00011576` (verbatim) / `0x00011750` (aligned)
  — 32 × 2 bytes, value is the full footer mask `(1 << table_one[slot]) - 1`.
  Used with `mask & raw_position` instead of `mask & (raw_position - table_two[slot])`
  — equivalent because every slot base in `table_two` is an exact multiple
  of the slot width.
- `table_topbits_mask_per_slot[32]` at `0x00011744` (aligned only) — 32 × 2
  bytes, mask sized for `(footer_bits - 3)`. Used only in aligned blocks,
  with `mask & (raw_position >> 3)` to emit the top bits of a distance
  that has ≥ 3 footer bits.

### Aligned-offset Huffman tree storage
- `_DAT_0010188c` — 8 × 2 bytes = 16 bytes. Aligned tree Huffman CODES.
- `_DAT_0010189c` — 8 bytes. Aligned tree code LENGTHS.
- `_DAT_00100bc8` — source buffer passed to `emit_code_length_table(8, ...)`
  when emitting the 8 × 3-bit aligned tree header in the block preamble.

### `table_four[34]` — pretree delta symbol decoder
```c
static const unsigned char table_four[34] = {
    0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,
    0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16
};
```
Doubled copy of 0..16 for mod-17 arithmetic without needing to handle wrap.

Decoder formula: `new_length = table_four[old_length + 17 - symbol]`
Encoder formula: `symbol = (old_length - new_length + 17) mod 17`

---

## Length slot encoding (same tables, different range)

The tables above are used **for both position and length encoding** — the
match symbol packs both slot numbers into a single Huffman symbol:

```c
// Match symbol (0..511 within the match range after subtracting 256)
symbol = (length_slot << 5) | position_slot;

// Decoder:
position_slot = symbol & 31;            // low 5 bits (0..31)
length_slot   = (symbol >> 5) & 15;     // high 4 bits (0..15)
```

- **32 position slots** (as above)
- **16 length slots** (0..15). Slot 15 covers lengths 195..258

For length slot 0, minimum length = `table_two[0] + 3 = 3` (the "+ 3" comes
from the minimum match length). So:
- length_slot 0 → length 3  (base 3, 0 footer bits)
- length_slot 1 → length 4
- length_slot 2 → length 5
- length_slot 3 → length 6
- length_slot 4 → lengths 7-8 (base 7)
- length_slot 5 → lengths 9-10
- length_slot 6 → lengths 11-14
- length_slot 7 → lengths 15-18
- length_slot 8 → lengths 19-26
- length_slot 9 → lengths 27-34
- length_slot 10 → lengths 35-50
- length_slot 11 → lengths 51-66
- length_slot 12 → lengths 67-98
- length_slot 13 → lengths 99-130
- length_slot 14 → lengths 131-194
- length_slot 15 → lengths 195-258

This confirms **length slot 0 is length 3** (the minimum match length).

---

## Main tree

**768 symbols total**:
- 0..255: literal bytes
- 256..767: match symbols = `256 + (length_slot << 5) + position_slot`

Max code length is 16 bits (would be indexed by `literal_table[control & 4095]`
in the decoder, so codes ≤ 12 bits get direct lookup and codes > 12 bits get
a secondary walk).

---

## Aligned offset tree

**8 symbols**, each 3-bit lookup table. Used in block type 3 (aligned) to
Huffman-code the low 3 bits of any match position whose slot has ≥ 3 footer
bits. Code lengths are emitted as 8 × 3 bits in the block header.

From `unlzx.c`:
```c
unsigned char offset_len[8];
unsigned short offset_table[128];
```

The 3 footer bits of position slot 6+ (`table_one[6]=2`… wait, slot 6 has
2 footer bits, too few) — actually slot 8 has 3 footer bits (the first slot
with ≥ 3). For aligned blocks, the encoder emits `(slot_footer_bits - 3)` raw
bits, then the last 3 bits through the aligned-offset Huffman tree.

---

## Pretree

**20 symbols**, **each code length emitted as 4 bits** in the tree header.

```c
unsigned char huffman20_len[20];
unsigned short huffman20_table[96];
```

Pretree symbol meanings:
| Symbol | Meaning |
|--------|---------|
| 0..16  | Length delta (mod 17 of prev-curr) — literal code length |
| 17     | Run of zeros (short): 4 extra bits → 4..19 (or 3..18?) zero entries |
| 18     | Run of zeros (long):  5/6 extra bits → 19..50 or 20..51 (depending on `fix`) |
| 19     | Same-delta run: 1 extra bit (3..4 or 4..5 reps), then another pretree symbol for the delta |

### `fix` subtlety (from unlzx.c)

The decoder uses a `fix` variable that starts at 1 for the first tree pass
(literal range 0..255) and decrements to 0 for the second pass (match range
256..767). This affects the run counts:

```c
case 17:        /* SHORT zero run */
    temp = 4;  count = 3;          // fix=1 → count=4..19 (=3+(0..15)+1)
    break;                          // fix=0 → count=3..18 (=3+(0..15)+0)
case 18:        /* LONG zero run */
    temp = 6 - fix;                 // fix=1 → 5 extra bits, fix=0 → 6
    count = 19;                     // fix=1 → count=20..51 (=19+(0..31)+1)
    break;                          // fix=0 → count=19..82 (=19+(0..63)+0)
case 19:        /* run of same delta */
    count = (control & 1) + 3 + fix;  // fix=1 → 4..5, fix=0 → 3..4
    ...
```

So **the second tree pass has wider run counts** because the match symbol
range is twice as large (512 vs 256 literals).

---

## CRC tables

### CRC32 (zlib/zip)
Polynomial **0xEDB88320** (reflected). Init = `0xFFFFFFFF`, final XOR `~`.
Table at data hunk offset 0x000, 256 × 4 bytes.

From `unlzx.c` (`crc_calc` function):
```c
void crc_calc(unsigned char *memory, unsigned int length)
{
    /* uses sum as the running CRC, without the final complement */
    if (length) {
        do {
            sum = crc_table[((unsigned char) sum) ^ *memory++] ^ (sum >> 8);
        } while (--length);
    }
}
```

Note: `unlzx` **doesn't do a final XOR** — it stores and compares the raw
running sum. The compressor binary does `return ~_DAT_00101242` in its CRC
function, which **does** complement the output.

**Resolved**: both unlzx and the compressor store and compare the
**inverted** form. `unlzx` does `temp = ~sum; ... compute ... ; sum = ~temp`
(so `sum` is always in inverted form externally). The compressor returns
`~running_value`. The archive stores the standard zlib-style inverted CRC32.

### CRC16 (Modbus/ARC)
Polynomial **0xA001** (reflected). Table at data hunk offset 0x400,
256 × 2 bytes. **Not used by any code path we traced** — the archive's
header CRC is CRC32 (same table and algorithm as the data CRC), not CRC16.
The CRC16 table appears to be dead code or an artefact from an earlier
version of the format.

---

## Archive info header (10 bytes)

Fully resolved (both the layout from the encoder and the checksum formula
from `info_header_checksum` / FUN_0000461c):

```
info_header[0]   = 'L'   (0x4c)
info_header[1]   = 'Z'   (0x5a)
info_header[2]   = 'X'   (0x58)
info_header[3]   = 0x00
info_header[4]   = checksum  (8-bit sum of all 10 bytes, computed with [4]=0)
info_header[5]   = 0x00
info_header[6]   = 0x0a
info_header[7]   = 0x04
info_header[8]   = 0x00
info_header[9]   = 0x00
```

`unlzx.c` only validates bytes 0..2 ("LZX"), so any archive that starts
with those three bytes will be accepted. For byte-exact output parity,
write the full layout above.

---

## Archive entry header (31 bytes) — fully resolved

Most multi-byte values **little-endian** — **except** the packed
date/time at bytes 18..21 which is **big-endian** (see ALGORITHM.md
§11 "Packed date/time byte order"). Cross-verified against `unlzx.c`
and the compressor's `write_file_header` / `mem_alloc_tiny`.

| Offset | Size | Value | Notes |
|--------|------|-------|-------|
| 0      | 1    | attribute byte | HSPARWED bits or default 0x07 |
| 1      | 1    | 0x00          | zeroed, never written |
| 2..5   | 4    | original size (LE) | from file entry word 0x1c (= byte 0x70) |
| 6..9   | 4    | compressed size (LE) | from compressor; 0 if not last of multi-file group |
| 10     | 1    | 0x0a           | hardcoded constant — machine type/version? |
| 11     | 1    | 0x02           | pack mode (`DAT_001013f1 \| 2`, DAT is always 0) |
| 12     | 1    | merged-flag    | 1 if multi-file group, 0 otherwise |
| 13     | 1    | 0x00           | zeroed, never written |
| 14     | 1    | comment length | from `mem_alloc_tiny` |
| 15     | 1    | 0x0a           | hardcoded constant — host OS/version? |
| 16, 17 | 2    | 0x00 0x00      | zeroed, never written |
| 18..21 | 4    | packed date/time | 4 bytes packed year/month/day/hour/minute/second (1970 epoch, via FUN_000070d8) |
| 22..25 | 4    | data CRC32 (LE)| from file entry word 2; set during compression |
| 26..29 | 4    | header CRC32 (LE) | computed last, with bytes 26..29 set to 0 |
| 30     | 1    | filename length | from `mem_alloc_tiny` |

Following the 31-byte fixed header:
- Filename (length given at byte 30, no null terminator on-disk)
- Comment (length given at byte 14, no null terminator on-disk)

Header CRC computation:
```c
sum = 0xFFFFFFFF;
crc32_update(header, 31);          // with bytes 26..29 zeroed
crc32_update(filename, fname_len);
crc32_update(comment,  cmt_len);
stored_crc = ~sum;
write LE at header[26..29]
```

### Bytes 10 and 15 — the two 0x0a constants

Both set to decimal 10, unconditionally. Plausible interpretations (can't
confirm without more format docs):
- **Byte 10** = "machine type" or "pack method version"
- **Byte 15** = "host OS" or "extract method version"

`unlzx` doesn't read either byte. For byte-exact output parity, just write
`0x0a` in both places.

---

## Block type (decrunch_method) — CORRECTED

From `unlzx.c`:
- `decrunch_method == 0` — store (uncompressed, handled at pack_mode level)
- `decrunch_method == 1` — **verbatim block REUSING previous tree** (doesn't re-read tree lengths)
- `decrunch_method == 2` — verbatim block with NEW tree
- `decrunch_method == 3` — aligned offset block with NEW tree

The Amiga compressor binary has `encode_stored_block` as a 2-byte no-op,
corresponding to type 1 ("reuse previous tree"). Since reusing the tree
means the header writer skips `huffman_build_tree` (see the `if
(DAT_001018b4 != 1)` check in `write_block_header`), the body encoder has
nothing to do for type 1 — the encoder continues appending to the same
stream with the same trees.

**But the LZX 1.21R compressor may not actually emit type 1 blocks** —
it always builds a fresh tree for each block. The type-1 handler is
present in the dispatch table but empty, suggesting it's either dead
code or used only in a special reinitialization path we haven't exercised.

---

## Keyfile data (`DAT_00100800`)

Starts at data hunk offset 0x800, continues to the end (0xe50). **1616
bytes** of pseudo-random data — this is the generic unlock keyfile that
Jonathan Forbes released as freeware in 1997 to let the unregistered LZX
1.21 behave as the registered version. Not relevant to compression.

---

## Resolved in follow-up pass

1. **Info header bytes 3..9** — RESOLVED: the writer zeroes all 10 bytes,
   writes `"LZX\0"` at [0..3], writes `0x0a` at [6] and `0x04` at [7],
   then runs an 8-bit byte-sum checksum over the 10 bytes (with [4]=0)
   and stores the result at [4]. Bytes [5], [8], [9] are always zero.
2. **Entry header** — RESOLVED to the unlzx layout (see ALGORITHM.md §11).
3. **Final CRC complement** — RESOLVED: standard zlib form. Both the
   compressor (`~_DAT_00101242` on return) and `unlzx` (which inverts
   at start and end of each chunk) store the inverted CRC32.
4. **CRC16 usage** — still appears unused. The table is present in the
   data hunk but no code path in the compress/extract/test/view commands
   invokes it. Safe to ignore for a compressor-only reimplementation.
5. **Positions 1..3** — RESOLVED: `init_distance_slot_table` explicitly
   writes `table[0]=0, table[1]=1, table[2]=2, table[3]=3` **before** the
   main loop that fills indices 4..511. I missed this earlier because I
   was reading the loop output. So distances 1, 2, 3 are encoded via
   slots 1, 2, 3 (each 0 footer bits, each covers one exact distance).
   Slot 0 remains the "use last_offset cache" signal (distance 0 is never
   a valid raw match distance).

## Still outstanding

Nothing structural and nothing that blocks a from-scratch implementation.
The only loose ends are semantic labels for two constant 0x0a bytes in the
entry header (bytes 10 and 15) and the 4-byte FileInfoBlock slice at bytes
18..21. All are preserved byte-exact by the compressor regardless of what
they mean; a fresh implementation just writes them as 0x0a and the
FileInfoBlock-derived value respectively.
