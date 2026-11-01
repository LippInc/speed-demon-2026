# Track C: Join

**K 23.68** on box 2 (sd-bench/C, commit a13436c, the lower of the official and fresh datasets):
T 0.055 s (0.054-0.055) · M 15.0 MB · S 582 code lines · D 0 dependencies (`.speed-demon.toml`) · sha 5/5 + 5/5.

Reproduce from the repository root: `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`
(full seed 1; the expected hash is c-join.md:212).

| Trick | Where (a13436c) | PERF.md |
|---|---|---|
| Input mapped read-only, never copied | `src/main.rs:33` | Decisions; floor1 |
| 256 KiB chunks claimed from a counter, each cut at LF + digit/BOM; end check, one-worker rerun | `src/pass2.rs:77`, `src/records.rs:167`, `src/pass2.rs:94`, `src/main.rs:75` | e1, c-19a2 |
| Commas and quotes found 32 bytes at a time (AVX2) | `src/records.rs:43` | c-10 |
| CR-or-LF terminator skipped without a branch | `src/records.rs:132` | c-10 |
| Name hash: first and last word, one 128-bit multiply | `src/names.rs:32` | c-11 |
| Names up to 16 bytes compared as two words | `src/names.rs:94` | c-11 |
| Lock-free hits: one packed word to the slots, 8-byte slot holds the record's address | `src/names.rs:324`, `src/names.rs:56`, `src/names.rs:81` | c-6, c-12.1, c-20a |
| Table grows before half load, so a lock-free probe always ends | `src/names.rs:345` | c-6 |
| Retired table generations freed by eras at chunk boundaries | `src/names.rs:296`, `src/names.rs:348`, `src/pass2.rs:78` | c-19a3 |
| One lazily mapped name arena, 1 KiB per-shard reservations | `src/names.rs:129`, `src/names.rs:165` | c-7, c-9.1 |
| Country bitsets written only by their owner, fed through owner inboxes | `src/pass2.rs:52`, `src/pass2.rs:89`, `src/pass2.rs:68` | c-9, c-19a |
| Slot prefetch issued from the parse, its hash handed to the lookup | `src/names.rs:308`, `src/records.rs:118` | c-22 |
| Region id → country as a dense u8 table | `src/regions.rs:8` | floor1 |

Official harness output for each measured submission commit: [reports/c/](../reports/c/) at the repository root, one file per commit, harness-<commit>.txt (the board's own record of that run).
The numbers in reports/ name the code commit they measured; the commit after it only adds those reports.

Honest limit: every record pays a name lookup whose two dependent memory loads (slot, then the name's record) are most
of its cost (box probes 781d04f and 54113f4), and the parse finds each field with its own search.

## Approach

The program maps both input files and never copies them. regions.csv becomes a dense table from region id to a
country index, one byte per id (c-join.md:59: ids ascend, codes are `[A-Z]{3}`). records.csv is cut into 256 KiB
chunks, claimed by the workers from a shared counter. Each chunk starts at the first LF followed by a digit or a BOM,
the only way a record can start (c-join.md:21, 36). The generator never puts that pair inside a quoted field
(c-join.md:50-51), and each chunk checks that its parse stopped exactly where the next chunk begins; if any did not,
the whole file is parsed again on one worker.

Twelve workers parse their chunks field by field: commas and quotes are found with 32-byte AVX2 compares, `""` escapes
are counted, a BOM before a record is skipped (c-join.md:36), and money is kept in integer cents (c-join.md:37).
Each worker keeps its own per-country sums, counts, minimum and maximum.

Distinct names per country need an exact set. Each name gets a dense id from one table shared by all workers: 256
shards of open-addressed 8-byte slots; a hit loads one packed word for the shard's slots, reads the slot and the
name's copy in an arena without taking a lock, and compares the bytes exactly. A new name is inserted under its
shard's lock; a grown-out slot array is freed once every worker has passed a chunk boundary since. Each country's set
of name ids is one bitset, written by exactly one worker: the others hand it (country, id) pairs through its inbox,
which it drains after each of its own chunks. Nothing waits for the slowest worker until the end. The per-worker sums
are merged, the countries sorted bytewise (c-join.md:127-129), and the file written once.

## Dependencies

None: Rust's standard library only; mmap is called through a short `extern "C"` block.

## With another 24 hours

- A structural parse: each 64-byte block classified once into quote, comma and LF bits, fields walked by bit position (tried in c-25: box T 2-3 ms slower, reverted).
- Regions parsed beside the records instead of before them.
