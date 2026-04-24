# LZX 1.21R Compressor Algorithm (Amiga)

Reverse-engineered from `amiga/LZX_68000EC-r` via Ghidra decompilation.
Traced from `compress_file` down through every function in the compression
pipeline. Constants extracted directly from the binary.

**The goal is to implement a byte-compatible LZX compressor from scratch
without re-consulting the original binary or decompiled source.**

---

## 1. Pipeline overview

```
compress_file                       per-file driver (walks linked list of files)
  compress_file_drive               FUN_0000fa58
    compressor_state_alloc          FUN_0000f5d2 (once, 7 big buffers + distance-slot LUT)
    reset_output_for_file           FUN_0000f7b0 (lazily allocs output buf, resets cursor/bits)
    prep_window_for_file            FUN_0000f5b0 (window base, bit buffer, terminator)
    block_encode_wrap               FUN_00010974
      reset_block_state             FUN_00011de8 (per-block globals)
      read_window_chunk             FUN_0000f88e (read up to 64KB into window, update CRC32)
      encode_block_body             FUN_00010dbc — main LZ77 loop
        find_longest_match          FUN_00010a88 / find_match_extended FUN_00010c2c
        flush_block / flush_block_copy  FUN_000112be / FUN_000112fa (when 32760 tokens buffered)
          write_block_header_wrap   FUN_0000fba0
            write_block_header      FUN_0000feac
              choose_block_type     FUN_00010900 (verbatim vs aligned)
              emit_code_length_table FUN_0000fc44 (type 3 only — 8×3-bit aligned tree lengths)
              clear_huffman_workspace FUN_000108e4 (zero 0x600 bytes of main tree workspace)
              save_and_write_tree   FUN_00010074 (delta pretree, TWO independent sections)
                build_huffman_from_freqs  FUN_0000f99a
              dispatch_block_encoder FUN_0001004a
                encode_reuse_tree_block  FUN_000108e0 (type 1, 2-byte stub)
                encode_verbatim_block    FUN_00011462 (type 2)
                encode_aligned_block     FUN_0001161c (type 3)
    finalize_output_for_file        FUN_0000f858
```

High-level flow: **read up to 64 KB into a window → run LZ77 against the
64 KB → build Huffman trees from observed frequencies → emit block header
+ compressed stream → repeat**.

---

## 2. Window and hash

### Window — **larger than 64 KB**, with a base-relocation trick

Verified from `compressor_state_alloc` (line 13015) and `encode_block_body`:

Allocation size: `DAT_00100f04 + DAT_00100f0c + 0x502`. Defaults:
`0x10000 + 0x10000 + 0x502 = 0x20502` = **131 842 bytes**.

Layout (by offset within the allocation):
```
+0x00000 .. +0x0ffff   history area   (DAT_00100f0c, default 0x10000)
+0x10000 .. +0x1ffff   current chunk  (DAT_00100f04, default 0x10000)
+0x20000 .. +0x20501   tail padding   (0x502 bytes, safety margin for match finder read-ahead)
```

Both `DAT_00100f04` and `DAT_00100f0c` can be overridden via command-line
options. `DAT_00100f04` is clamped to `[0x400, 0x80000]` (1 KB..512 KB)
and `DAT_00100ef8` (output buffer) to `[0x2200, 0x40000]`.

**Base-relocation trick**: `_DAT_001018dc` (the "window base pointer") is
adjusted down by `DAT_00100f04` every time a new chunk is read
(line 15104). This keeps `window_base + absolute_position` valid no
matter where in the file we are. Positions throughout the compressor are
**absolute 32-bit file positions**, not window-local offsets — that's why
the hash table stores uint32 positions, and why `find_longest_match`
operates on `pos & 0xffff` only for indexing the chain tables.

### LZ77 distances
- Max distance (spec): **65 535** bytes
- Chain walk distance cutoff: `pos - 0xfefc` ≈ 65 276 (avoids the last 260
  byte reach to prevent memcmp overrun with max-length 258-byte matches)

### Hash function (3-byte rolling, shift-5)

Verified from assembly at 0x10de6..0x10dfa:

```c
uint16_t h = 0;                 // d4 cleared
h = (h << 5) ^ window[i];       // lsl.w #5 / eor.b (byte)
h = (h << 5) ^ window[i+1];
h = (h << 5) ^ window[i+2];
h &= HASH_MASK;                 // and.w $1232(a4), d4
```

All arithmetic is **16-bit word-truncated**. The `eor.b` instructions
XOR only the low byte of `d4`, but since the byte being XORed has zero
upper bits, this is mathematically equivalent to a word XOR.

For position advancement (rolling update), the main loop doesn't
re-read all three bytes — it shifts in one new byte and XORs, like:
```c
h = (h << 5) ^ window[i+2];   // appends one new byte to the hash
h &= HASH_MASK;
```
(See the update at 0x10e88..0x10e8e inside the lazy-match loop.)

Hash mask stored at `DAT_00101232`. Its size determines the hash table
dimension: `hash_table_size = HASH_MASK + 1`.

### Hash chain — **THREE tables**, not two

Verified from `encode_block_body` + `compressor_state_alloc`:

- **`hash_table[hash]`** (`_DAT_001018d8`, 4 bytes per entry × `hash_size`)
  — stores the **full 32-bit absolute position** of the most recent
  occurrence with this hash. NOT a window-local position. `hash_size` is
  0x8000 by default (32K entries, 128 KB total) or 0x4000 for lower
  quality (16K entries, 64 KB). Mask is `hash_size - 1`, stored at
  `DAT_00101232` (separate from the size stored at `DAT_0010122e`).
- **`chain_a[pos & 0xffff]`** (`_DAT_001018b8 → _DAT_001018c4`, 0x20000
  bytes = 2 bytes × 65536) — primarily stores **distances** to previous
  occurrences, or `0xffff` as a sentinel.
- **`chain_b[pos & 0xffff]`** (`_DAT_001018bc → _DAT_001018c8`, 0x20000
  bytes = 2 bytes × 65536) — a parallel chain used by `find_longest_match`
  for chain-compaction and to flag entries as "skipped" (`0xffff`) or
  "end" (`0`).

Insertion at absolute position `pos` (from `encode_block_body` at lines
14905..14911):
```c
prev_pos = hash_table[hash];       // 32-bit absolute position of prev occurrence
hash_table[hash] = pos;             // store 32-bit absolute position
distance = pos - prev_pos;
if (distance > 0xffff) distance = -1;
chain_a[pos & 0xffff] = (short)distance;
```

The chain_b table is populated at match-advance time (lines 15015..15034)
with a **two-table encoding**: if the chain-advance distance fits in
16 bits, `chain_a` stores the distance and `chain_b` stores `0xffff`;
otherwise, `chain_a` stores `0xffff` and `chain_b` stores `0`. The match
finder uses this split to distinguish "no link" from "skip-over-here".

**Why 32-bit absolute positions in the hash table?** So you don't have to
renormalize the hash table when the window slides forward. A 32-bit
position can outrun the window by any amount; old hash entries are simply
ignored when `pos - prev_pos > 0xffff`.

---

## 3. Match finder (`find_longest_match`)

### Priority 1: repeat match
Try continuing the **last emitted match** by comparing at
`pos - last_offset`, where `last_offset` (`DAT_001018ce`) is the distance
of the most recently emitted match (initialised to 1 at block start).
Compare up to 258 bytes.

**If this repeat match matches ≥ 51 bytes, accept it immediately** —
don't walk the hash chain. The return distance (`DAT_001018cc`) is set to
0 to signal "use the last_offset cache".

### Priority 2: hash-chain walk
The match finder has **two distinct walk paths**:

1. **Primary walk** (the main loop near line 14548 in the labelled source):
   walks backwards through the chain, **distance-limited** by the window
   (`while ((int)(in_D0 - 0xfefc) < (int)in_D1)`, i.e. until the candidate
   falls out of the 65 276-byte reach). There is no iteration counter on
   this loop; it also compacts the chain by rewriting intermediate links
   as it walks. The first candidate whose first 2 bytes match delegates to
   `find_match_extended`.
2. **Secondary / fallback walk** (`LAB_00010b7e` in `find_longest_match`,
   plus the `sVar4 = 0x10` loop inside `find_match_extended`): a **17-
   iteration** walk (`moveq #$10, dn` + `dbra`). Entered when the primary
   walk's chain-compaction logic hits a terminal link mid-walk, or when
   `find_match_extended` reaches its own fallback section.

So the 17-iteration cap is a property of the fallback walk, not the main
walk. A from-scratch reimplementation that uses a single distance-limited
walk + a separate 17-iteration tail is a faithful match.

Quick reject at each candidate: compare first 2 bytes against current
position. On pass: inner loop compares up to 255 more bytes forward (the
initial 3-byte hash prefix is implicit).

Track best match length; return length and store the found distance in
`DAT_001018cc` (0 if the repeat match was used).

### Constants

| Parameter              | Value |
|------------------------|-------|
| Min match length       | 3     |
| Max match length       | 258   |
| Max match distance     | 65 276 bytes (primary walk cutoff) |
| Fallback walk cap      | 17 iterations (`dbra` with init 16) |
| Repeat-match shortcut  | ≥ 51 bytes (`sVar7 < 0xcf` where init = 0x101) |

---

## 4. Main LZ77 loop (`encode_block_body`)

At each position:
1. Update rolling hash, insert position into hash chain
2. Call `find_longest_match`
3. If match length < 3 → emit literal, advance by 1
4. Otherwise, attempt **lazy matching** unless gated out:
   - Skip lazy if `curr_len - 3 >= threshold` (level parameter: 1 / 7 / 40)
   - Skip lazy if `last_offset == 0` (previous emit was a repeat)
   - Skip lazy if `next_len < 3` (no real next match)
   - Skip lazy if `next_len == 3 && next_dist > 29999` (next is weak and far)

   Otherwise insert position+1 into hash chain, call `find_longest_match`
   again, and apply the cost formula:

   ```c
   cost_delta = slot(curr_dist) - slot(next_dist);   // +ve: curr expensive
   diff       = curr_len - next_len;                  // +ve: curr longer

   if (diff > 1)       keep_current;                  // curr much longer
   if (diff == 1) {
       if (cost_delta > 17) take_lazy;                // cost saving justifies
       else                 keep_current;
   }
   if (diff == 0) {
       if (cost_delta > 5)  take_lazy;                // equal length, small saving
       else                 keep_current;
   }
   if (diff == -1) {
       if (cost_delta > -3) take_lazy;                // next 1 longer unless curr much cheaper
       else                 keep_current;
   }
   if (diff < -1)      take_lazy;                     // next is much longer
   ```

5. After emitting a match of length N, all N positions covered by the
   match must have hash/chain entries so future searches can find them:
   - Position `match_start` was already inserted when the match finder was called
   - Position `match_start + 1` is either (a) inserted during lazy-match
     check, or (b) marked as chain-terminal (`chain_table[pos+1] = -1`)
     in the no-lazy path at 0x10e9a..0x10ea6
   - Positions `match_start + 2` through `match_start + N - 1` are
     inserted by the post-match loop at 0x110fa..0x1114a (which runs
     `match_length - 2` iterations via `dbra d0` where d0 = match_len - 3)
6. Taking lazy = emit literal for current position, re-run step 1 at pos+1.
   At level `-3` (multi-step flag set), this can iterate multiple times.

### Token buffer

Tokens accumulate in memory, not directly in the bit stream:
- Literals go into a byte buffer
- Matches are recorded as `(distance, length)` pairs in separate buffers
- A **bitmap** marks literals vs. matches:
  ```c
  bitmap[pos >> 3] |= 1 << (pos & 7);   // set = match token, clear = literal
  ```
- When the token count reaches **0x7ff8 (32760)**, `FUN_000112fa` is called
  to flush a block

At block-emit time, the encoder walks the buffers in parallel, counting
symbol frequencies, building Huffman trees, and then emitting everything
through the trees.

---

## 5. Symbol alphabet (main tree)

The **main tree has 768 symbols**:
- **0..255** — literal bytes
- **256..767** — match symbols, encoded as:
  ```
  symbol = 256 + (length_slot << 5) + position_slot
  ```
  where there are **16 length slots × 32 position slots = 512 match symbols**.

Decoder view (from `unlzx.c`):
```c
symbol -= 256;
position_slot = symbol & 31;        // low 5 bits
length_slot   = (symbol >> 5) & 15; // high 4 bits
```

Position slots are 0..31 (see `table_two[32]` / `table_one[32]` in
CONSTANTS.md). **Slot 0 is reserved: it means "reuse `last_offset`"** —
this is the repeat-match cache. Slots 1..31 encode actual distances.

Length slots are 0..15, using the same `table_one`/`table_two` tables but
offset by +3 (minimum match length). Length slot 0 = length 3, slot 15 =
lengths 195..258 (table_two[15]=192, 6 footer bits → 64 values, + 3 base).

Emission order for a match token (**verified by disassembling
encode_verbatim_block at 0x11500..0x11536**):

1. Emit Huffman code for the match symbol (256 + slot combination)
2. **Emit position footer bits first** — `table_one[position_slot]` bits
   of `(raw_position & table_three[table_one[position_slot]])`
3. **Then emit length footer bits** — `table_one[length_slot]` bits
   of `((raw_length - 3) & table_three[table_one[length_slot]])`
4. For type 3 (aligned), position footer handling changes: if
   `table_one[position_slot] >= 3`, emit the top `(footer_bits - 3)`
   bits raw then emit the low 3 bits through the aligned-offset tree.

The trick with `value & mask` instead of `value - table_two[slot]` works
because every slot base in `table_two` is an exact multiple of the slot
width — so `raw & mask` equals `raw - base` for all values within the
slot. Verified: slot 4 base 4 mask 1, slot 6 base 8 mask 3, slot 10 base
32 mask 15, etc.

---

## 6. Tables (extracted from binary)

All tables below were extracted from the actual binary — see `CONSTANTS.md`
for raw dumps.

### Position slot footer bit counts (32 entries)
```
{0,0,0,0, 1,1, 2,2, 3,3, 4,4, 5,5, 6,6, 7,7, 8,8, 9,9, 10,10, 11,11, 12,12, 13,13, 14,14}
```
Classic LZX position-slot progression covering 2^14 positions.

### Position slot footer bit masks (32 entries, 16-bit each)
`mask[i] = (1 << footer_bits[i]) - 1`:
```
{0,0,0,0, 1,1, 3,3, 7,7, 15,15, 31,31, 63,63, 127,127, 255,255, 511,511,
 1023,1023, 2047,2047, 4095,4095, 8191,8191, 16383,16383}
```

### Distance slot lookup (`DAT_0010192c`, built at runtime)
512-byte table, built by `init_distance_slot_table` (`FUN_0000fbcc`).
Entries 0..3 are explicitly written, then the loop fills 4..511:

| indices   | value | footer bits (from table_one) |
|-----------|-------|------------------------------|
| 0         | 0     | 0 (distance 0 = "use last_offset"; never a raw distance) |
| 1         | 1     | 0 (exact distance 1) |
| 2         | 2     | 0 (exact distance 2) |
| 3         | 3     | 0 (exact distance 3) |
| 4..5      | 4     | 1 (slot 4) |
| 6..7      | 5     | 1 |
| 8..11     | 6     | 2 |
| 12..15    | 7     | 2 |
| 16..23    | 8     | 3 |
| 24..31    | 9     | 3 |
| 32..47    | 10    | 4 |
| 48..63    | 11    | 4 |
| 64..95    | 12    | 5 |
| 96..127   | 13    | 5 |
| 128..191  | 14    | 6 |
| 192..255  | 15    | 6 |
| 256..383  | 16    | 7 |
| 384..511  | 17    | 7 |

Position-slot lookup:
```c
if (pos < 0x200) slot = table[pos];
else             slot = table[pos >> 8] + 16;
```

For `pos < 512`, a direct read gives the slot in 0..17. For `pos >= 512`,
shifting `pos >> 8` gives an index in 2..255, which reads out 0, 1, 2, 3,
4, 4, 5, 5, 6, 6, 6, 6, ..., 15. Adding 16 gives the final slot in 16..31.

For `pos >> 8 == 2` (i.e. pos in [512, 768)), `table[2] = 2`, `+16 = 18`.
So slot 18 covers positions 512..767. Similarly slot 19 covers 768..1023,
slot 20 covers 1024..1535, etc. The progression matches `table_two[]` /
`table_one[]` from unlzx exactly.

### Length slot lookup
The **same** `distance_slot_lookup` table is used for length slot
computation, indexed by `raw_length - 3`. 16 length slots are active
(values 0..15 from the table).

The encoder code in `encode_verbatim_block` computes:
```c
length_slot   = table[(raw_length - 3)];      // 0..15
position_slot = table[raw_position];          // 0..31 (with high-pos split)
symbol = 256 + length_slot * 32 + position_slot;
```

This works because the table's values for small indices happen to match the
length slot progression: length 3 → slot 0, length 4 → slot 1, ..., length 6
→ slot 3, length 7-8 → slot 4, etc., which is exactly what `table_two[]`
encodes for lengths.

### Pretree delta table (`DAT_000108c0`, with base 0x108cd for indexing)
30 bytes, allows indexing with negative offsets:
```
base-13: 4   base-12: 5   base-11: 6   base-10: 7   base-9:  8
base-8:  9   base-7:  10  base-6:  11  base-5:  12  base-4:  13
base-3:  14  base-2:  15  base-1:  16  base+0:  0   base+1:  1
base+2:  2   base+3:  3   base+4:  4   base+5:  5   base+6:  6
base+7:  7   base+8:  8   base+9:  9   base+10: 10  base+11: 11
base+12: 12  base+13: 13  base+14: 14  base+15: 15  base+16: 16
```

Encodes `pretree_symbol = (prev_len - curr_len) mod 17`, covering deltas
in the range **-13..+16**. Matches MS-LZX pretree delta encoding exactly.

### CRC32 table (zlib/zip standard)
At data hunk offset 0x000, 1024 bytes. Polynomial **0xEDB88320** (reflected).
Standard init = `0xFFFFFFFF`, final = `~crc`.
```c
uint32_t crc32(const uint8_t *buf, size_t len) {
    uint32_t c = 0xFFFFFFFF;
    while (len--) c = crc32_table[(c ^ *buf++) & 0xff] ^ (c >> 8);
    return ~c;
}
```

### CRC16 table (Modbus/ARC standard)
At data hunk offset 0x400, 512 bytes (256 × 16-bit). Polynomial **0xA001**
(reflected). Present in the binary but **not used** by any code path we
traced — the archive's header CRC is a CRC32 (using the same table as the
data CRC). The CRC16 table appears to be dead code or an artifact from
an earlier format version.

---

## 7. Block format

```
 3 bits   block type
[if type 3]
 8 × 3 bits    aligned-offset tree code lengths
24 bits   block length in source bytes (3 × 8-bit writes)
[if type != 1] main Huffman tree, pretree-compressed — see section 8
...       block body: sequence of Huffman-coded literal/match tokens
```

### Block header byte/bit layout (from unlzx.c `read_literal_table`)

Reads in order, consuming from `control` low bits:
1. **3 bits** — `decrunch_method` (block type 1/2/3).
2. **If type 3**: **8 × 3 bits** — `offset_len[0..7]` (aligned-offset
   tree code lengths). Rebuild `offset_table` via `make_decode_table(8, 7, ...)`.
3. **3 × 8 bits** — block output length in source bytes, read as three
   8-bit chunks **most-significant byte first**:
   ```c
   decrunch_length  = (control & 255) << 16; consume 8;
   decrunch_length |= (control & 255) <<  8; consume 8;
   decrunch_length |= (control & 255);       consume 8;
   ```
   **This is the number of decoded output bytes** the block produces —
   NOT a token count, NOT a byte count of the compressed body. The
   decoder's outer loop uses it to know when the block is done.
4. **If type != 1**: the main Huffman tree, emitted in two sections
   (see §8). On type 1 the tree is reused verbatim from the previous
   block (`literal_len[]` carries over).

### Block types (verified against unlzx.c)
- **Type 0** — not used at the block level (pack_mode=0 is store at the file level)
- **Type 1** — verbatim block **reusing the previous tree** (no tree header)
- **Type 2** — verbatim block with new tree
- **Type 3** — aligned offset block with new tree

The LZX 1.21R compressor has `encode_reuse_tree_block` as a 2-byte no-op
corresponding to type 1. Since `write_block_header` skips emitting a new
tree when `block_type == 1`, and the body encoder is a no-op, emitting a
type-1 block would produce literally zero output beyond the 3+24-bit
block header — this is exactly the MS-LZX "reuse previous tree" semantics.

`choose_block_type` only returns 2 or 3, so **type 1 is dead code in the
encoder**. It could be used in a decoder-compatible way by a future
encoder that wants to avoid re-emitting identical trees.

### Block type selection (`choose_block_type` / FUN_00010900)
```c
uint16_t freq[8];
clear_aligned_offset_freq(freq);
count_aligned_offset_frequencies(freq);   // bsr $11d00

uint16_t max_freq   = 0;
uint32_t total_freq = 0;
for (int i = 0; i < 8; i++) {
    if (freq[i] > max_freq) max_freq = freq[i];
    total_freq += freq[i];
}

uint32_t quintile = total_freq / 5;

if (max_freq > quintile && match_count >= 100)
    block_type = 3;    // aligned offset
else
    block_type = 2;    // verbatim
```

Type 3 is chosen when **(a)** some single aligned-offset value (low 3 bits
of a match distance) occurs in more than **20%** of the block's
**aligned-eligible matches** — i.e. matches whose raw distance is ≥ 16
(position slot ≥ 8, where `table_one[slot] ≥ 3`), verified in
`count_aligned_offset_freqs` by the `if (uVar1 < 0x10)` filter — and
**(b)** the block has at least **100 matches total** (`DAT_001018b0 > 99`,
counted unconditionally by `flush_block_copy`, not just aligned-eligible ones).

The quintile comparison uses the sum of the 8 histogram bins (i.e. total
of aligned-eligible matches), not the total block match count — so condition
(a) is strictly "some bin > total_aligned_eligible / 5".

---

## 8. Huffman tree serialization (`save_and_write_tree` + pretree)

This is the **pretree-compressed delta** encoding, identical in structure
to MS-LZX. **Verified against the decompiled `save_and_write_tree` body
at lines ~13981..14343.**

Tree workspace struct (`param_1`):
```
+0x04  data passed to build_huffman_from_freqs (workspace)
+0x08  pointer to current_lengths[]  (768 bytes for main tree)
+0x0c  pointer to prev_lengths[]     (saved from previous block)
```

### Top-level procedure
1. **Save** previous tree state: `memcpy(prev, current, 0x300)` (768 bytes
   — covers literals + match symbols in one copy)
2. **Build** a new main tree from accumulated symbol frequencies
   (`build_huffman_from_freqs`). This overwrites `current_lengths[]`.
3. **Emit the tree in TWO INDEPENDENT sections**, one for literals and one
   for match symbols. Each section has its own pretree built from its own
   frequency counts, its own pretree Huffman codes, its own 20×4-bit
   pretree length header, and its own emission pass. The two sections use
   **different run/zero-run parameters** (see below).
4. At the end: `memcpy(prev, current, 0x300)` — restore so the "previous"
   now matches what the decoder will also have.

### Pretree alphabet (same for both sections)
| Symbol | Meaning |
|--------|---------|
| 0..16  | Length delta (mod 17 of `prev - curr`), via `(&DAT_000108cd)[prev-curr]` table lookup |
| 17     | Zero-run (short): extra bits, **count starts at 4 (literal) / 3 (match)** |
| 18     | Zero-run (long):  **5 extra bits (literal) / 6 extra bits (match)** |
| 19     | Same-delta run: 1 extra bit, then one more pretree code giving the delta symbol |

### Literal section (indices 0..255), `fix=1` in unlzx.c
- Clear 24-entry pretree freq counts (`asStack_60`)
- Temporarily set `current[0x100] = 99` as a sentinel to bound run detection
- **Pass A** (freq count): walk `current_lengths[0..255]`, detect runs
  (`sVar6 < 4` threshold: a "run" needs 4 repeats)
  - `curr_len == 0`, run ≥ 4: cap at 51 → sym 17 if run < 20, sym 18 otherwise
  - `curr_len != 0`, run ≥ 4: cap at 5 → sym 19 (and also count the delta symbol)
  - otherwise: increment count for the delta symbol
- Build pretree Huffman codes from the freqs (`build_huffman_from_freqs`)
- Drain pending bits to a multiple-of-16 boundary before emission
- Emit pretree **20 × 4-bit code lengths**
- **Pass B** (emission): re-walk and emit pretree codes
  - sym 17: then 4 extra bits = `run - 4` (covers runs 4..19)
  - sym 18: then 5 extra bits = `run - 20` (covers runs 20..51)
  - sym 19: then 1 extra bit = `run - 4` (covers 4..5), then pretree code for the delta
- Restore the saved byte at `current[0x100]`

### Match section (indices 256..767), `fix=0` in unlzx.c
- Clear 24-entry pretree freq counts
- **Pass A**: walk `current_lengths[256..767]`, different thresholds
  - Run threshold: `sVar6 < 3` (a "run" needs only 3 repeats here)
  - `curr_len == 0`, run ≥ 3: cap at 82 → sym 17 if run < 19, sym 18 otherwise
  - `curr_len != 0`, run ≥ 3: cap at 4 → sym 19
- Build pretree Huffman codes (fresh, independent from the literal pretree)
- Emit pretree **20 × 4-bit code lengths**
- **Pass B**: emit
  - sym 17: then 4 extra bits = `run - 3` (covers 3..18)
  - sym 18: then **6 extra bits** = `run - 19` (covers 19..82)  ← wider than literal section
  - sym 19: then 1 extra bit = `run - 3` (covers 3..4), then pretree code for the delta

### Why the two sections differ
The match symbol range is twice as wide (512 symbols vs 256 literals) so
long runs of zeros in the match section can be much longer than in the
literal section — the 6-bit zero-run field accommodates this. Both the
compressor and unlzx.c share this split via the `fix` variable (which is
literally the +1 increment applied in the literal section).

### Pretree symbol run parameters — differ between literal/match sections
See the two-section table above. Quick reference:

| Section | Run threshold | sym 17 bits (range) | sym 18 bits (range) | sym 19 bits (range) |
|---------|---------------|----------------------|----------------------|----------------------|
| Literal (0..255)   | ≥4 | 4 bits (4..19)  | 5 bits (20..51) | 1 bit (4..5) |
| Match (256..767)   | ≥3 | 4 bits (3..18)  | 6 bits (19..82) | 1 bit (3..4) |

### Max code length
Main tree codes are limited to **16 bits**. Length 0 = unused symbol.
The pretree itself uses 4-bit lengths so its own codes are capped at 15
bits; pretree symbol `0` means "literal length delta 0" not "unused".

### Persistence across blocks (from unlzx.c `extract_normal` + `read_literal_table`)

`literal_len[768]` and `offset_len[8]` are the **only** tree state. They
are zeroed **once** at the start of a compressed stream (i.e. once per
file entry, or once per merged-group stream — see §11), and thereafter
persist across blocks. There is no separate `prev_lengths[]` buffer in
the decoder: each new type-2/3 block's pretree-delta pass mutates
`literal_len[]` in place, using its current values as the "previous"
lengths. The compressor's `save_and_write_tree` conceptually does the
same — it `memcpy`s current→prev only so the frequency-count pass can
inspect the old lengths while writing the new ones.

A type-1 block reuses `literal_len[]` exactly as-is and emits no tree
header. `offset_len[]` is only ever rewritten by a type-3 block; a
type-2 block following a type-3 leaves `offset_len[]` stale-but-unused.

`last_offset` (the repeat-match cache) is initialised to **1** at the
start of the stream and persists across blocks. Not reset on block
boundaries.

---

## 8a. Canonical Huffman code assignment and decode tables

From unlzx.c's `make_decode_table(number_symbols, table_size, length[], table[])`.

### Canonical code assignment
Codes are assigned in ascending (bit_length, symbol_index) order, starting
from the all-zeros code for the shortest length. For each used length
`L = 1..16`, walk symbols `0..number_symbols-1` and assign the current
`pos` as the code, then increment `pos` by `1 << (table_size - L)` (for
codes that fit in the root lookup) or `1 << (16 - L)` in the 16.16
fixed-point tail. At the end, `pos` must exactly equal `table_mask`
(`1 << table_size`) or the tree is ill-formed and decoding aborts.

Both encoder and decoder must agree on this ordering; since it's
"sort by (length, symbol)" with no tie-breaker, it is unambiguous.

### Fast lookup table layout

`make_decode_table` builds a **two-level** table:

- **Root level**: a `1 << table_size`-entry array indexed by the next
  `table_size` bits of the bitstream, read in **reversed order** (the
  bit the decoder would consume first sits at the **high** end of the
  root index, because canonical codes are MSB-first while the bit reader
  hands them out LSB-first — so each code's position is bit-reversed
  before use).
- **Secondary tree**: for codes longer than `table_size` bits, the root
  entry stores a node index ≥ `number_symbols`. The decoder walks a
  binary tree stored in the upper half of `table[]`, one bit at a time,
  until it hits a leaf (`< number_symbols`).

Root sizes used by LZX:
| Table                 | Symbols | `table_size` | Array size |
|-----------------------|---------|--------------|------------|
| Main (literal/match)  | 768     | 12           | 4096 root + secondary space → `literal_table[5120]` |
| Aligned offset        | 8       | 7            | `offset_table[128]` |
| Pretree               | 20      | 6            | `huffman20_table[96]` |

Root-index bit-reversal is done once, at table-build time, by the inner
loop that computes `leaf` from `reverse = pos`. The decoder then just
indexes with `table[control & mask]` — no per-symbol reversal.

### Decode step (main tree example)
```c
symbol = literal_table[control & 4095];
if (symbol >= 768) {
    /* long code: walk the secondary tree */
    consume 12 bits;
    do {
        symbol = literal_table[(control & 1) + (symbol << 1)];
        consume 1 bit;
    } while (symbol >= 768);
} else {
    consume literal_len[symbol] bits;  /* exact length */
}
```
The offset and pretree decoders use the same pattern with their own
sizes (`>= 8`, 7 root bits; `>= 20`, 6 root bits).

### Encoder side
Since the on-disk bit order is "canonical code, MSB first, but emitted
into a LSB-first bit buffer," the encoder must **bit-reverse each
Huffman code** before ORing it into `bit_buffer`. A from-scratch
implementation can either reverse at build time (store reversed codes
alongside lengths) or at emit time.

Code-length limiting to 16 bits: if the naive Huffman build produces a
tree deeper than 16, apply package-merge or the classic Kraft-inequality
fixup. The compressor's `build_huffman_from_freqs` (not fully
disassembled) presumably does this; any textbook length-limiting build
that produces canonical codes in (length, symbol) order will be
bit-compatible.

---

## 9. Symbol emission (`encode_verbatim_block` / `encode_aligned_block`)

Both functions walk the token buffer + bitmap in parallel:

### Literal token
```c
emit_code(main_tree, literal);
```

### Match token (order verified from encode_verbatim_block disasm)

```c
length_slot   = distance_slot_lookup[raw_length - 3];
if (raw_position < 0x200)
    position_slot = distance_slot_lookup[raw_position];
else
    position_slot = distance_slot_lookup[raw_position >> 8] + 16;

symbol = 256 + (length_slot << 5) + position_slot;

// 1. Emit Huffman code for the symbol
emit_code(main_tree, symbol);

// 2. Emit POSITION footer first
if (table_one[position_slot]) {
    if (block_type == 3 && table_one[position_slot] >= 3) {
        // Aligned offset: top bits raw, low 3 bits via aligned tree
        int top_bits = table_one[position_slot] - 3;
        emit_bits((raw_position - table_two[position_slot]) >> 3, top_bits);
        emit_code(aligned_tree, raw_position & 7);
    } else {
        emit_bits(raw_position & table_three[table_one[position_slot]],
                  table_one[position_slot]);
    }
}

// 3. THEN emit LENGTH footer
if (table_one[length_slot]) {
    emit_bits((raw_length - 3) & table_three[table_one[length_slot]],
              table_one[length_slot]);
}
```

The aligned-tree handling is the distinguishing feature of type-3 blocks.

---

## 10. Bit output

### State
- `bit_buffer` (`DAT_00101b2c`, 32-bit) — accumulates pending bits
- `bit_count`  (`DAT_00101b30`, signed byte) — number of pending bits
- `out_cursor` (`_DAT_00101904`, `uint16_t *`) — output cursor (word-aligned)
- `out_end`    (`_DAT_00101908`) — output buffer end

### Initialization sequence — gotcha resolved
`reset_output_for_file` sets `bit_count = 16`, which would cause the
first write to immediately flush a zero word. **This is dead code**: the
next function called in the per-file sequence is `prep_window_for_file`,
which calls `reset_bit_state` (`FUN_0000f884`) — that zeros both
`bit_buffer` and `bit_count`. By the time `write_block_header` is
reached, `bit_count = 0`. So the first real write goes cleanly into
bits 0..2 of a fresh 16-bit word. No leading zero word is emitted.

A from-scratch reimplementation should just initialize `bit_count = 0`
and skip the `= 16` misdirection entirely.

### Write N bits
```c
bit_buffer |= value << bit_count;
bit_count += N;
while (bit_count >= 16) {
    bit_count -= 16;
    *out_cursor++ = (uint16_t)bit_buffer;   // big-endian via 68k store
    bit_buffer >>= 16;
    if (out_cursor >= out_end) flush_output_buffer();
}
```

### Final flush (end of block)
If any bits remain after the last token, zero-pad to the next **word**
boundary and emit the remaining word.

**Output is 16-bit-word-granular, big-endian** (native 68k order). When
reading an LZX archive back on a little-endian host, each pair of bytes
must be byte-swapped before processing.

### Decoder bit reader (from unlzx.c)

The inverse of the writer. Two globals:
- `control` (uint32) — accumulator
- `shift`   (int32, signed) — number of **valid** bits currently in `control` minus 16; starts at `-16`

Refill (when `shift < 0` after a consume):
```c
shift += 16;
control += (*source++) << (8 + shift);
control += (*source++) << shift;
```
Reads two bytes. The first byte becomes the **high** byte of the freshly
loaded 16-bit word (bits `(8+shift)..(15+shift)`), the second byte the low
byte. On-disk the 16-bit word is thus **big-endian**, but within the
word the bit the writer emitted first ends up at the **low** end of
`control`.

Consume N bits: `value = control & ((1<<N)-1); control >>= N; shift -= N;`
then refill if `shift < 0`.

Single-bit consume uses the faster path `if (!shift--) { refill with
shift += 16 and <<24 / <<16 }; control >>= 1` — same semantics.

Consequence for the writer: bits are ORed into `bit_buffer` starting at
bit 0, and a full 16-bit word is flushed as a big-endian half-word. The
writer's first-emitted bit is the LSB of the on-disk low byte of each
16-bit pair (= LSB of byte `2n+1` for word `n`). Section 10 already
describes this writer; the refill formula above is its exact inverse.

---

## 11. Archive format

### Archive info header (10 bytes)

Fully resolved by reading the archive-creation code around decompiled
line 2675, plus `info_header_checksum` (`FUN_0000461c`):

| Offset | Value | Notes |
|--------|-------|-------|
| 0      | `'L'` (0x4c) | magic byte 0 |
| 1      | `'Z'` (0x5a) | magic byte 1 |
| 2      | `'X'` (0x58) | magic byte 2 |
| 3      | 0x00         | magic byte 3 / padding |
| 4      | checksum     | 8-bit byte-sum over all 10 bytes, computed with this byte = 0 |
| 5      | 0x00         | |
| 6      | 0x0a (10)    | version? |
| 7      | 0x04 (4)     | flags/revision? |
| 8      | 0x00         | |
| 9      | 0x00         | |

Writer procedure:
```c
memset(buf, 0, 10);
buf[0..3] = "LZX\0";
buf[6]    = 0x0a;
buf[7]    = 0x04;
// checksum pass: byte sum of all 10 bytes
buf[4] = 0;
for (int i = 0; i < 10; i++) buf[4] += buf[i];
// write 10 bytes
```

`unlzx.c` only validates bytes 0..2 (`"LZX"`), so the full layout only
matters for generating archives that *look* identical byte-for-byte to
the original compressor's output.

### Per-entry header (31 bytes fixed)
Most multi-byte values are **little-endian** (even though the host is
big-endian) — **except** the packed date/time at bytes 0x12..0x15,
which is big-endian. See "Packed date/time byte order" below.

Fully mapped from `write_file_header` (`FUN_0000185a`), `mem_alloc_tiny`
(`FUN_0000252a`), `entry_attr_default`/`entry_attr_byte`, and cross-verified
against `unlzx.c`:

| Offset    | Size | Field | Source |
|-----------|------|-------|--------|
| 0x00      | 1    | attributes (PSHAEDWR) | `entry_attr_byte` — `0x0f` (`----rwed`) for a freshly-created Amiga file; see `bits.lzx` in issue #3 for the full bit layout. The Ghidra symbol `entry_attr_default = 0x07` exists in the binary but doesn't match what the compressor actually writes for default files. |
| 0x01      | 1    | 0 (unused) | zeroed by `mem_alloc_tiny`, never overwritten |
| 0x02..05  | 4    | **original size** (LE) | from file entry at offset 0x70 |
| 0x06..09  | 4    | **compressed size** (LE) | from compressor, or 0 if not last-in-group |
| 0x0a      | 1    | hardcoded `0x0a` | "machine type" / host version? Always 10 |
| 0x0b      | 1    | **pack mode** | `DAT_001013f1 \| 2` → the original Amiga compressor always writes `0x02`, **but real archives in the wild also use `0x00` for stored entries** — see "Pack mode dispatch" below |
| 0x0c      | 1    | merged-group flag | `1` if multi-file group, else `0` |
| 0x0d      | 1    | 0 (unused) | zeroed, never overwritten |
| 0x0e      | 1    | **comment length** | set by `mem_alloc_tiny` |
| 0x0f      | 1    | hardcoded `0x0a` | "extract version" / host OS? Always 10 |
| 0x10..11  | 2    | 0 (unused) | zeroed, never overwritten |
| 0x12..15  | 4    | **packed date/time** | 4 bytes of packed year-1970 / month / day / hour / minute / second, via `FUN_000070d8` bit-packing |
| 0x16..19  | 4    | **data CRC32** (LE) | from file entry word 2; computed during compression |
| 0x1a..1d  | 4    | **header CRC32** (LE) | computed last with these 4 bytes zeroed |
| 0x1e      | 1    | **filename length** | set by `mem_alloc_tiny` |
| 0x1f..    | var  | filename + comment | copied by `mem_alloc_tiny` |

Total entry header on disk: `31 + filename_len + comment_len` bytes
(from `entry_header_size` / `FUN_00002628`).

### Pack mode dispatch (decoder side)

The original LZX 1.21R compressor always writes `pack_mode = 0x02`, but
the **format itself permits stored entries** and `unlzx.c` dispatches on
byte `0x0b` (around line 945):

```c
switch(pack_mode) {
    case 0:  extract_store(in_file);  break;  /* raw payload, no LZX */
    case 2:  extract_normal(in_file); break;  /* LZX-compressed */
    default: extract_unknown(in_file); break;
}
```

A conformant decoder **must** handle at least the two known values:

| `pack_mode` | Meaning | Payload layout |
|-------------|---------|----------------|
| `0x00`      | **stored** | `compressed_size` bytes of raw payload, equal to `original_size` |
| `0x02`      | **normal** | LZX-compressed stream, decompresses to `original_size` bytes |
| other       | unknown | Treat as a hard error or skip via `compressed_size` |

Real-world Aminet samples that use `pack_mode = 0`:
- Small `file_id.diz` and similar manifests stored alongside compressed
  data in the same archive
- Tiny entries (≤ 17 bytes in some samples) where compression would
  cost more than it saves
- Mixed-mode archives where some files are pre-compressed (e.g. ZIP/JPEG)
  and the LZX writer chose to store them rather than re-compress

If `pack_size == 0` the entry has no payload at all (this happens for
non-last members of a merged group — see "Multi-file groups" below).
Skip straight to the next entry header.

After processing each entry, `unlzx` defensively `fseek`s `pack_size`
bytes from the entry's payload start so a partial extractor read can't
desynchronise the stream:

```c
if(fseek(in_file, pack_size, SEEK_CUR)) { ... }
```

A from-scratch reader should do the same: track the position where the
payload begins, and after attempting to decode it, advance to
`payload_start + compressed_size` regardless of how many bytes the
extractor actually consumed.

Header CRC computation:
1. `header_crc = 0xFFFFFFFF`
2. CRC32 over bytes 0x00..0x1e (31 bytes, with bytes 0x1a..0x1d set to 0)
3. CRC32 over the filename bytes
4. CRC32 over the comment bytes
5. Invert (`~`) and store at bytes 0x1a..0x1d (LE)

### Packed date/time at bytes 0x12..0x15

Format (from `pack_date`, inverse `unpack_date`). **Verified by bit-level
analysis**: the input-struct field order is `{year-1970, day, month, hour,
minute, second}` — note **day comes before month**, unlike MS-DOS:

```c
void pack_date(const unsigned char in[6], uint8_t out[4]) {
    // in[0]=year-1970, in[1]=day, in[2]=month,
    // in[3]=hour,       in[4]=minute, in[5]=second
    out[0] = (in[2] >> 1) | (in[1] << 3);
    out[1] = (in[3] >> 4) | (in[0] << 1) | (in[2] << 7);
    out[2] = (in[4] >> 2) | (in[3] << 4);
    out[3] =  in[5]       | (in[4] << 6);
}
```

Bit field widths: **year=6, day=5, month=4, hour=5, minute=6, second=6**
(total 32 bits).

Byte layout (MSB..LSB within each byte):
```
byte 0: [ day(5 bits)    | month(high 3 bits)     ]
byte 1: [ month(bit 0)   | year-1970(6 bits)      | hour(bit 4)    ]
byte 2: [ hour(low 4)    | minute(high 4 bits)    ]
byte 3: [ minute(low 2)  | second(6 bits)         ]
```

Packed bit stream (high-to-low across the 4 bytes): `day | month | year |
hour | minute | second`.

This is **NOT** MS-DOS / FAT format — the field ordering differs (MS-DOS
packs hour/min/sec + year/month/day, and uses a 1980 epoch). This is
LZX's own scheme with a 1970 epoch (year adjustment is `-0x46 = -70`,
verified at decompiled line 6578).

**Previous doc had `day ↔ month` swapped**; corrected after bit-level
re-derivation from `pack_date` + `unpack_date`.

### Notes on the "unused" bytes

The zeroed bytes 0x01, 0x0d, 0x10, 0x11 are slots the format reserves but
this compressor never uses. Other LZX implementations (or later versions)
may put something there. For byte-exact output parity just keep them zero.

The hardcoded `0x0a` at bytes 0x0a and 0x0f are most likely **version
fields** or **host OS identifier**. They're written unconditionally and
never read by `unlzx`. Value 10 doesn't match LHA-family conventions
(which would use '-' = 0x2d for Amiga) so they're LZX-specific identifiers.

### Decoder window (from unlzx.c)

The decoder uses a **65 536-byte circular output buffer** with 258-byte
overrun margins on both sides. Total allocation: `258 + 65536 + 258`.

```
decrunch_buffer [0 .. 257]            overrun head (match source zone)
                [258 .. 258+65535]    the actual 64 KB window
                [258+65536 .. +257]   overrun tail (match destination zone)
```

The decoder maintains a `destination` pointer that advances through
the tail of this buffer. When `destination >= decrunch_buffer + 258 + 65536`,
it copies the last `destination - (decrunch_buffer + 65536)` bytes back
to the head region and resets `destination` — this gives the next
match copies a valid "previous 64 KB" to read from without bounds
checks.

Match copy with circular wrap (distance up to 65 535):
```c
string = (decrunch_buffer + last_offset < destination)
       ? destination - last_offset
       : destination + 65536 - last_offset;
```
The `+ 65536` branch handles the case where the copy source would fall
before the buffer start, by wrapping around to the logically equivalent
position 64 KB ahead. This relies on the window being exactly 64 KB.

Note this is smaller than the **compressor's** ~128 KB allocation
(§2): the compressor's extra 64 KB is match-finder history only; the
format is still a 64 KB-window LZ. The maximum valid distance (65 535)
is strictly less than the window size (65 536), so a match never
references a byte older than the window edge.

### Multi-file groups
When compressing multiple small files, LZX can **concatenate them** into a
single compressed stream (called "merging" in the source). In that case:
- Each file in the group has its own header with `merged_flag = 1` and
  `compressed_size = 0`
- The **last** file in the group carries the real `compressed_size` in its
  header (as computed by the total bytes of the compressed stream for the group)
- The merged stream covers all the files concatenated back-to-back

This is an optimisation for many-small-files cases common on Amiga (think
icon sets, text files).

Decoder handling (from `extract_archive` in unlzx.c): the reader walks
entry headers appending each filename to a linked list. When it hits
an entry whose `pack_size > 0`, that entry is the **last in the group**
and its `pack_size` is the total compressed size of the merged stream.
The decoder then runs a single `extract_normal` over the stream,
iterating the linked list of output files — each file gets its own
output handle and a **per-file CRC reset (`sum = 0`)** but shares the
same `last_offset`, `literal_len[]`, `offset_len[]`, bit-reader state,
and decrunch buffer. Block boundaries inside the merged stream do
**not** align with file boundaries; a single 32 760-token block can
straddle multiple output files.

For a from-scratch encoder, the simple correct behaviour is "never
merge" (always emit `merged_flag=0` and one stream per file). To match
the original compressor byte-for-byte you need to reproduce its
grouping heuristic, which isn't covered in this document.

### Packed date/time byte order — BIG-ENDIAN, not LE

From `view_archive`:
```c
temp = (archive_header[18] << 24)
     + (archive_header[19] << 16)
     + (archive_header[20] << 8)
     +  archive_header[21];
```

So the 4 date bytes at offsets 0x12..0x15 are **big-endian** — the
opposite of every other multi-byte field in the entry header. This is
consistent with the `pack_date` bit-layout in §11 above (`out[0]` holds
the highest-order bits: day, month-high).

Bit field unpacking (after BE-combining the 4 bytes into a 32-bit word):
```
day    = (temp >> 27) & 31    // bits 27..31
month  = (temp >> 23) & 15    // bits 23..26
year   = (temp >> 17) & 63    // bits 17..22   (+ 1970)
hour   = (temp >> 12) & 31    // bits 12..16
minute = (temp >>  6) & 63    // bits  6..11
second =  temp        & 63    // bits  0..5
```

---

## 12. Design summary

| Choice                  | Value          |
|-------------------------|----------------|
| Hash                    | 3-byte shift-5 |
| Chain walk depth        | 17 (hardcoded, via `dbra` with initial 16) |
| Lazy matching           | 1-step (`-1`/`-2`), iterated (`-3`) |
| Lazy match threshold    | 1 / 7 / 40 (`-1` / `-2` / `-3`)  |
| Repeat-match shortcut   | ≥ 51 bytes     |
| Window                  | 64 KB          |
| Max match length        | 258            |
| Min match length        | 3              |
| Block size              | 32 760 tokens (fixed, no adaptive splitting) |
| Main tree               | **768 symbols** (256 literals + 16×32 match) |
| Length slots            | **16**         |
| Position slots          | 32             |
| Aligned tree            | 8 symbols, 3-bit codes |
| Pretree                 | 20 symbols, 4-bit lengths, MS-LZX format |
| Code length max         | 16             |
| Output word size        | 16 bits, big-endian |
| CRC                     | CRC32 (zlib) for data and headers |
| Byte order in archive   | Little-endian (despite big-endian host) |
| Default level           | `-2` (normal)  |

### Key globals (data hunk base = 0x00100000)

| Address       | Purpose                                    |
|---------------|--------------------------------------------|
| 0x00101b2c    | bit output buffer (32-bit)                 |
| 0x00101b30    | bit output pending count (signed byte)     |
| 0x00101904    | output word cursor                         |
| 0x00101908    | output buffer end                          |
| 0x001018b4    | current block type (2 or 3)                |
| 0x00101914    | current block length in source bytes       |
| 0x0010185c    | main Huffman tree workspace (0x600 bytes for 768 symbols × 2) |
| 0x0010186c    | aligned-offset frequency counts (8 shorts) |
| 0x0010192c    | distance-slot length LUT (512 bytes, built at runtime) |
| 0x001018b8    | chain_a pointer (distances table, 128 KB)  |
| 0x001018bc    | chain_b pointer (sentinel/skip table, 128 KB) |
| 0x001018c4    | chain_a backing store (actual allocation)   |
| 0x001018c8    | chain_b backing store (actual allocation)   |
| 0x001018d8    | hash_table (absolute 32-bit positions, hash_size×4 bytes) |
| 0x001018cc    | match finder return distance (0 = "use last_offset cache") |
| 0x001018ce    | last_offset cache — committed distance of the most recent emitted match. Initialised to 1 at block start. |
| 0x001018dc    | window base pointer                        |
| 0x001018d8    | hash_table base                            |
| 0x00101232    | hash mask                                  |
| 0x00101234    | lazy-match threshold (match_length form, threshold+3) |
| 0x00101236    | lazy-match threshold (match_length-3 form) |
| 0x00101238    | multi-step lazy flag (set at level -3)     |
| 0x00101242    | CRC working value                          |
| 0x001018fc    | block start position (for length calc)     |
| 0x001018e4    | saved curr_len - 3 during lazy check       |
| 0x001018f6    | literal/match bitmap base pointer          |

### Resolved in the second pass through the source:

1. **Info header layout** — RESOLVED:
   ```
   [0..3] = "LZX\0"            — magic
   [4]    = 8-bit sum checksum (all 10 bytes with [4]=0, then sum written back)
   [5]    = 0
   [6]    = 0x0a               — format version (10)
   [7]    = 0x04               — flags/revision (4)
   [8..9] = 0
   ```
   See `FUN_0000461c` for the checksum and the archive-creation code around
   line 2675 for the layout. The compressor zeroes a 10-byte buffer, writes
   the 4-byte magic and the 2 version/flags bytes, then calls the checksum
   function.

2. **CRC complement** — RESOLVED: both `unlzx.c` (which computes
   `temp = ~sum; ...; sum = ~temp`) and the compressor (which returns
   `~_DAT_00101242` from its CRC function) store the **inverted** form.
   The archive stores standard zlib-style CRC32 with init `0xFFFFFFFF` and
   final XOR `0xFFFFFFFF`.

3. **Block split heuristic** — RESOLVED: **purely size-based**. There is
   no adaptive splitting. `FUN_000112fa` is called exactly when the token
   buffer reaches 32760 entries (`0x7ff7 < count + 1` test in the main
   loop). No other conditions trigger a block split.

4. **Compression levels** — NEW finding: `lzx -1`, `-2`, `-3` set three
   parameters (and `-0` is "store" mode):
   | CLI flag | `DAT_00101236` (threshold) | `DAT_00101238` (multi-step flag) | `DAT_00101234` (= threshold + 3) |
   |----------|-----|-----|-----|
   | `-1` quick  | 1  | 0 | 4  |
   | `-2` normal | 7  | 0 | 10 |
   | `-3` max    | 40 | 1 | 43 |

   The threshold controls **how long a current match must be before lazy
   matching is skipped**. The check is
   `if (threshold <= curr_len - 3 || last_dist == 0) break;` — when the
   current match is already long enough, don't bother looking for a
   better one.

   The multi-step flag (set only at `-3`) enables **iterated** lazy
   matching (look ahead multiple positions, not just one).

5. **Lazy-match cost formula** — FULLY RESOLVED by direct disassembly of
   encode_block_body at addresses 0x10f32..0x10f56. Clean form:

   ```c
   // diff = curr_len - next_len (signed)
   // cost_delta = slot(curr_dist) - slot(next_dist) (signed byte)
   //   positive  => current is more expensive than next (lazy saves bits)
   //   negative  => current is cheaper than next

   int diff = curr_len - next_len;

   if (diff > 0) {
       // Current is strictly longer
       if (cost_delta <= 17)   keep_current;
       else if (diff > 1)       keep_current;   // curr > next + 1
       else                     take_lazy;      // curr = next + 1, but cost saving > 17 justifies it
   }
   else if (diff == 0) {
       // Equal lengths
       if (cost_delta <= 5)     keep_current;
       else                     take_lazy;
   }
   else {
       // Next is strictly longer
       if (diff < -1)           take_lazy;      // next > curr + 1, always go lazy
       else {                                    // diff == -1: next is 1 longer
           // d5 += 3; ble keep
           if (cost_delta <= -3) keep_current;   // curr is cheaper by 3+, don't go lazy
           else                  take_lazy;
       }
   }
   ```

   Raw 68k assembly (from 0x10f32 onwards):
   ```
   move.w $18e4(a4), d1   ; d1 = saved curr_len - 3
   sub.w  d0, d1           ; d1 -= (next_len - 3) → d1 = curr - next
   bgt    .curr_longer     ; d1 > 0: curr longer
   beq    .equal           ; d1 == 0: equal
   addq.w #1, d1           ; d1 < 0: d1 += 1
   bne    .take_lazy       ; if (curr - next) != -1, diff < -1 → take lazy
   addq.b #3, d5           ; d5 += 3 (d5 is cost_delta)
   ble    .keep_current    ; if d5 + 3 <= 0 (i.e. d5 <= -3) → keep
   bra    .take_lazy
   .curr_longer:
   cmpi.b #$11, d5         ; compare d5 with 17
   ble    .keep_current    ; d5 <= 17 → keep
   subq.w #1, d1           ; d1 -= 1
   bgt    .keep_current    ; d1 > 1 (orig) → diff > 1 → keep
   bra    .take_lazy       ; diff == 1 → take lazy
   .equal:
   subq.b #5, d5
   ble    .keep_current    ; d5 <= 5 → keep
   ; fall through → take_lazy
   ```

### Entry conditions for lazy matching

Before even attempting the lazy search, these conditions gate it:
- `threshold <= curr_len - 3` → already a good enough match, don't lazy
  (where threshold is 1/7/40 for level `-1`/`-2`/`-3`)
- `last_offset == 0` (previous emit was a repeat) → don't lazy
- `next_len < 3` → no real next match, keep current
- `next_len == 3 && next_dist > 29999` → next match is minimum and far away, keep current

### Position slot lookup entries 0..3 — RESOLVED

Re-reading `init_distance_slot_table` more carefully shows that entries 0..3
are explicitly initialised **before** the loop that fills 4..511:

```c
DAT_0010192c = 0;   // table[0] = 0
DAT_0010192d = 1;   // table[1] = 1
DAT_0010192e = 2;   // table[2] = 2
DAT_0010192f = 3;   // table[3] = 3
```

So the full lookup table is:
- table[0] = 0 → slot 0 (never used for real matches: distance 0 is invalid;
  slot 0 decodes as "use last_offset cache")
- table[1] = 1 → slot 1 (covers distance 1 exactly)
- table[2] = 2 → slot 2 (covers distance 2 exactly)
- table[3] = 3 → slot 3 (covers distance 3 exactly)
- table[4..5] = 4, etc. (as before)

Distances 1, 2, 3 **are** emitted directly via slots 1, 2, 3, each of which
has 0 footer bits. My earlier concern about the BSS zero init was based on
a misreading — these entries are written by the init function.

### Strategy for validation

- **Test with the reference `unlzx`**: compress a file with your Rust impl,
  decompress with the stock `unlzx` — if it round-trips the data correctly,
  the format is valid.
- **For byte-exact output parity** (if you want to match the original
  compressor's output exactly, not just "a correct LZX archive"):
  - Run `lzx` under `vamos` on a suite of known inputs (empty, 1 byte,
    all-zeros, repeating pattern, random, short text, long text)
  - Byte-compare the outputs against your Rust implementation
  - Any divergence is either a lazy-match cost formula bug, a tie-breaker
    issue, or a different frequency→code-length assignment in the Huffman
    builder. The *format* will still be valid even if the exact bytes differ.
- **A correct-but-not-byte-identical compressor is easier** and probably
  good enough for most use cases. Byte parity is mostly about being able
  to regression-test against the original.
