# Track C: Join

**K 20.73** on box 2 (sd-bench/C, commit 8765712, the lower of the official and fresh datasets):
T 0.071 s (0.071-0.072) · M 15.2 MB · S 575 code lines · D 0 dependencies (`.speed-demon.toml`) · sha 5/5 + 5/5.

Reproduce from the repository root: `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`
(full seed 1; the expected hash is c-join.md:212).

| Trick | Where (8765712) | PERF.md |
|---|---|---|
| Input mapped read-only, never copied | `src/main.rs:33` | Decisions; floor1 |
| Records cut at LF + digit/BOM, no pre-pass; pairwise end check, one-worker rerun | `src/records.rs:169`, `src/pass2.rs:77`, `src/main.rs:77` | e1 |
| Commas and quotes found 32 bytes at a time (AVX2) | `src/records.rs:41` | c-10 |
| CR-or-LF terminator skipped without a branch | `src/records.rs:123` | c-10 |
| Name hash: first and last word, one 128-bit multiply | `src/names.rs:32` | c-11 |
| Names up to 16 bytes compared as two words | `src/names.rs:97` | c-11 |
| Name-table hits without a lock; 8-byte slot holds the record's address | `src/names.rs:311`, `src/names.rs:84` | c-6, c-7, c-12.1 |
| Table grows before half load, so a lock-free probe always ends | `src/names.rs:332` | c-6 |
| One lazily mapped name arena, 1 KiB per-shard reservations | `src/names.rs:131`, `src/names.rs:166` | c-7, c-9.1 |
| Country bitsets written only by their owner, fed by per-owner buckets | `src/pass2.rs:36`, `src/pass2.rs:69` | E2, c-9 |
| Retired table generations freed between the round barriers | `src/pass2.rs:97` | c-7 |
| Region id → country as a dense u8 table | `src/regions.rs:6` | floor1 |

Honest limit: the workers move in lockstep rounds, so one slow CPU stretches the whole run (13 workers on 12 CPUs:
T 0.071 → 0.098 s on box 2, probe ea471e6); and every record pays a name lookup with two dependent memory loads.

## Approach

The program maps both input files and never copies them. regions.csv becomes a dense table from region id to a
country index, one byte per id (c-join.md:59: ids ascend, codes are `[A-Z]{3}`). records.csv is split into twelve
byte ranges. Each range starts at the first LF followed by a digit or a BOM, the only way a record can start
(c-join.md:21, 36). The generator never puts that pair inside a quoted field (c-join.md:50-51), and each worker checks
that it stopped exactly where the next range begins; if any did not, the whole file is parsed again on one worker.

Twelve workers parse their ranges field by field: commas and quotes are found with 32-byte AVX2 compares, `""` escapes
are counted, a BOM before a record is skipped (c-join.md:36), and money is kept in integer cents (c-join.md:37).
Each worker keeps its own per-country sums, counts, minimum and maximum.

Distinct names per country need an exact set. Each name gets a dense id from one table shared by all workers: 256
shards of open-addressed 8-byte slots; a hit reads the slot and the name's copy in an arena without taking a lock,
and compares the bytes exactly. A new name is inserted under its shard's lock. Each country's set of name ids is one
bitset, written by exactly one worker: workers hand (country, id) pairs to the owner through per-owner buckets, in
rounds separated by barriers. At the end the per-worker sums are merged, the countries sorted bytewise
(c-join.md:127-129), and the file written once.

## Dependencies

None: Rust's standard library only; mmap is called through a short `extern "C"` block.

## With another 24 hours

- Pass 2 without rounds: chunks claimed from a counter and owner inboxes, so a slow CPU delays only its own chunks.
- A structural parse: each 64-byte block classified once into quote, comma and LF bits, fields walked by bit position.
- The name lookups: a prefetch of the slot as soon as the name is hashed, inside the parse.
