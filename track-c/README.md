# Track C: Join

The task: read records.csv (3.32 M records, each with a quoted, hostile name field) and regions.csv, join each record to its region's country, and write one line per country with its count, sums, min/max and number of distinct names.

Best board run: `run-fff3b356a7d3`, K 36.48 (T 0.0281 s, M 9.6 MB, S 585 code lines), 78.1 points. No dependencies. K is the contest's speed score (higher is better); the other labels are defined in [PERF.md](PERF.md).

The board run above is the code of push bf07a3a (board row 60). On our test machine (box 2), five alternating rounds measured median K 36.24 (36.13 to 36.39) and median memory 9.43 MiB across 150 runs on the board's run path (PERF.md c-93).

Reproduce from the repository root: `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`
(full seed 1; the expected hash is c-join.md:212).

References written as c-join.md:<line>, here and in the code comments, point to the track's task spec: [specs/c-join.md](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md) in the organisers' repository.

Official harness output for each measured submission commit: [reports/c/](../reports/c/) at the repository root, one file per commit, harness-<commit>.txt (the board's own record of that run). The numbers in reports/ name the code commit they measured; the commit after it only adds those reports.

What the code does, row by row. Each `src/<file>.rs:N` reference points at the line that does it; the last column names the experiment in [PERF.md](PERF.md) that measured it (IDs such as c-52 are entries in its log).

| Trick | Where | PERF.md entry |
|---|---|---|
| Both files are mapped, never copied. Each mapping is followed by 96 guard bytes (`"`, `,`, LF repeated), so every scan stops at the guard and the parse has no bounds checks | `src/main.rs:23`, `src/main.rs:29`, `src/main.rs:32`, `src/main.rs:33`, `src/records.rs:18` | Decisions, floor1, c-35 |
| records.csv is cut into 256 KiB chunks claimed from a shared counter; each chunk starts at the first LF followed by a digit or BOM | `src/pass2.rs:21`, `src/pass2.rs:155`, `src/records.rs:245` | e1, c-19a2, c-83 |
| Commas and quotes are found 32 bytes at a time (AVX2 compare and movemask); the escape test is read from the quote mask | `src/records.rs:72` | c-10, c-89 |
| Digit runs (id, region id, amount) are read as one 8-byte load and folded into a number by SSE multiply-adds; a BOM is tested behind a predicted branch | `src/records.rs:126`, `src/records.rs:141`, `src/records.rs:152`, `src/records.rs:211` | c-35, c-83 |
| CR-or-LF terminator skipped without a branch; amount sign applied without a branch | `src/records.rs:223`, `src/records.rs:158` | c-10 |
| Region id to country: one byte per region id. All workers build it together from 64 KiB pieces of regions.csv before they parse records; a lookup clamps the id with `min` instead of a bounds check | `src/regions.rs:14`, `src/regions.rs:43`, `src/regions.rs:84`, `src/pass2.rs:152` | floor1, c-47 |
| Region table byte is prefetched and each record is applied one record later, overlapping lookup latency with the next record's parse | `src/records.rs:218`, `src/records.rs:194`, `src/records.rs:225` | c-87, c-88 |
| A record's name is hashed with crc32c and sent to the owning worker as an 8-byte message (56-bit length-seeded multiply hash with country packed into the top byte) | `src/names.rs:21`, `src/names.rs:37`, `src/names.rs:51`, `src/pass2.rs:98` | c-51, c-52, c-83 |
| Messages are written to one 2 KiB bucket per owner through raw cursors; a full bucket is handed to the owner's mailbox | `src/pass2.rs:24`, `src/pass2.rs:99`, `src/pass2.rs:114` | c-49 |
| The owner keeps each key once in its table (64-byte buckets of 16 four-byte tagged index entries, AVX2 compare per half bucket) with one bit per country in a 256-bit set | `src/names.rs:69`, `src/names.rs:124`, `src/names.rs:209` | c-50, c-52, c-84 |
| The owner prefetches the home bucket of the message 16 message slots ahead | `src/names.rs:112`, `src/names.rs:205` | c-50, c-63 |
| Distinct names of a country = the sum over owners of their new (name, country) bits | `src/names.rs:210`, `src/pass2.rs:193` | c-52 |
| A sender tries to file a lagging owner's mailbox itself only once more than 16 full buckets wait there | `src/pass2.rs:30`, `src/pass2.rs:121` | c-52 |
| Each worker drops its share of the mapping's page-table entries before it finishes; the process exits without freeing | `src/pass2.rs:176`, `src/main.rs:73` | c-37 |

## Approach

The design is: workers parse chunks, and each record's name is sent to the one worker that owns its hash. There is no table that two workers update, and no atomic operation on names.

**Setup.** `main` maps both files (`src/main.rs:47`, `src/main.rs:48`) and starts 12 workers (`src/main.rs:44`; a test-only environment variable can change the count; the main thread is worker 0, `src/pass2.rs:202`). Before any worker starts, it reads regions.csv's header and its last line to size the region table (`src/regions.rs:65-72`); it parses no region lines.

**Regions.** The table maps a region id to a country index, one byte per id (c-join.md:59: ids ascend, codes are `[A-Z]{3}`). All workers fill it together: each claims 64 KiB pieces of regions.csv from a counter and stores, for every line whose LF falls in its piece, that line's country index at its region id (`src/regions.rs:86`, `src/regions.rs:99`). Country indices are handed out in first-seen order using `OnceLock` per country code index (`src/regions.rs:94-101`); the output is sorted by key, so the numbering never shows. A worker then waits until every piece is done (`src/regions.rs:103`). Group 0 is `_UNMATCHED`: ids with no line, or above the largest id, clamp to group 0 with `min` (`src/regions.rs:14`).

**Records.** records.csv is cut into 256 KiB chunks, claimed from a shared counter (`src/pass2.rs:21`, `src/pass2.rs:155`). A chunk starts at the first LF followed by a digit or a BOM, the only way a record can start (c-join.md:21, 36; `src/records.rs:245`). The generator never puts that pair inside a quoted field (c-join.md:50-51). Guaranteed well-formed records in contest rule 2.2 allow dropping the whole-file rerun and pairwise end check (PERF.md c-83). A worker parses its chunk field by field: commas and quotes are found with 32-byte AVX2 compares, and quote escape tests are read directly from the mask (`src/records.rs:72`). A BOM before a record is skipped behind a predicted branch (`src/records.rs:211`), digit runs are folded from an 8-byte load using SSE multiply-adds (`src/records.rs:126`, `src/records.rs:141`), and money is kept in integer cents (`src/records.rs:152`) with the sign applied without a branch (`src/records.rs:158`). The terminator CRLF or LF is skipped without a branch (`src/records.rs:223`). A 1-record software pipeline prefetches the region-table byte as soon as the region id is parsed (`src/records.rs:218`) and applies the previous record during the current record's parse (`src/records.rs:194`, `src/records.rs:225-229`). Each record folds into that worker's own per-country slot: count, id sum, amount sum, min and max, name bytes, note bytes (`src/records.rs:196-203`). The parse then hands the name's position and length to the sender (`src/records.rs:204`, `src/pass2.rs:159-163`).

**Names.** For each record the worker builds an 8-byte message (`Msg = u64`, `src/names.rs:21`) from the name's raw bytes (between the quotes, escapes kept, which is one to one with the value, c-join.md:27-28; up to the comma if unquoted) and the record's country (`src/names.rs:51`). Every name follows one hash path: 56 bits of a length-seeded multiply hash (`src/names.rs:59`) with the country packed into the top byte (`src/names.rs:60`). Two distinct names collide only if their 56 bits match: birthday bound $N^2 / 2^{57} \approx 7 \times 10^{-8}$ for $N = 100,000$ names (c-join.md 2.2 planning pool). The name is hashed with crc32c (`src/names.rs:37`) and sent to the worker that owns the hash: owner = high bits of the hash times the worker count (`src/pass2.rs:98`). A bucket holds 256 message slots (2 KiB) and goes to the owner's mailbox when full; each worker files the buckets waiting in its own mailbox at every hand-over and after every chunk (`src/pass2.rs:114`, `src/pass2.rs:166`). Filing means the owner looks the key up in its own table, which uses 64-byte buckets of 16 four-byte index entries (`src/names.rs:69`). The table index starts pre-sized to 2.2's pool (`src/pass2.rs:146`, `src/names.rs:80, 93-98`). In each bucket, tags are checked with AVX2 compares (`src/names.rs:124-137`), and a matching tag checks the 56-bit key in `recs` (`src/names.rs:140`). Each record holds a 256-bit country bitset (`src/names.rs:84`, `src/names.rs:209`). The owner sets the bit for the message's country; if the bit was clear, the owner's count for that country goes up by one (`src/names.rs:210`). The owner prefetches the home bucket 16 messages ahead (`src/names.rs:112`, `src/names.rs:205`). A country's number of distinct names is the sum of these counts over all owners (`src/pass2.rs:193`). No two workers write the same table at the same time. In a normal run only the owner writes to it; if an owner lags and more than 16 full buckets wait in its mailbox, the sender files them itself (`src/pass2.rs:30`, `src/pass2.rs:121`).

**Finish.** After its last chunk each worker drops its share of the mapping's page-table entries (`src/pass2.rs:176`), hands over its remaining buckets (`src/pass2.rs:178`), and keeps filing its mailbox until every worker has flushed (`src/pass2.rs:184-188`), then merges its slots and its owner's counts into shared totals (`src/pass2.rs:189-194`). `main` sorts the countries bytewise by packed big-endian key (c-join.md:127-129; `src/main.rs:53-63`), writes the file once (`src/main.rs:70`), and exits without freeing (`src/main.rs:73`).

## Limits

- Instruction split: parse ~210 instructions per record, sender ~66, owner drain ~77.
- Peak memory: on our test machine (box 2), memory over 150 interleaved runs on the board's run path gave a median of 9.43 MiB and a highest reading of 10.44 MiB (PERF.md c-93); board row 60 measured 9.6 MB.
- It needs a CPU with the x86-64-v3 instruction set (AVX2, BMI2, SSE4.2); it is built with `-C target-cpu=x86-64-v3 -C target-feature=+crt-static` (`Dockerfile:5`).
- The region table supports at most 255 countries plus `_UNMATCHED`; more aborts (`src/regions.rs:45`, `src/regions.rs:73`). The spec has 240 (c-join.md:60).
- A regions.csv line without its LF is outside the grammar (c-join.md:58-59); a line without one stops the program with a panic (`src/regions.rs:30-33`).

## Dependencies

None: Rust's standard library only; mmap and madvise are called through short `extern "C"` blocks (`src/main.rs:13-21`, `src/pass2.rs:15-17`).

## With another 24 hours

- Decouple next-record start calculation from field parsing: taking the next record start from the LF after the note, off the region and amount dependency chain, gave a 1.6 % wall-time reduction in 10 of 10 interleaved benchmark reads (PERF.md c-92 r3); grafting this tail scan onto the current parser is the next step.
- Fused structural window parse: classifying 64-byte or 96-byte windows with SIMD masks and pclmulqdq prefix-XOR (PERF.md c-76, c-86, c-92) narrowed the gap to +1.8 % wall time but did not surpass the incumbent parser within the contest window.
- Cold-code line cuts to reduce code lines (S) further without changing instruction counts (PERF.md c-91 r2).

## Dropped ideas

Measured and dropped, with numbers, in [PERF.md](PERF.md):
- A shared name table with atomic bit updates (c-40): 58.2 ms against 31.5 ms for the owner design at 12 workers; cross-core snoop loads 3.03 M against 0.23 M per run because each new bit pulls a cache line from another core.
- A two-stage structural parse (c-58): 37 % faster on one thread (114 ms against 181 ms) but only 5 % faster at 12 workers because two hyperthreads share execution ports and the method executes more instructions.
- Profile-guided optimisation (c-61): 1.0 to 1.2 ms slower in two A/B rounds.
- Prefetching the region table cell without pipelining (c-62): 41 % fewer L2 misses, but no time saved because those misses were not what the cores waited on.
- Longer bucket prefetch distance (c-63): 32 and 48 messages ahead scored inside the run-to-run spread.
- One malloc arena (c-65): 0.3 MB less memory, but T rose from 0.031 s to 0.033 s and K fell from 30.85 to 29.87.
- SIMD and SWAR number parsing in digit lanes (c-74): shorter dependency chains bought with more instructions ran slower (+0.46 % to +2.46 % instructions, T 0.031 s to 0.033 s).
- Transparent huge pages disable via PR_SET_THP_DISABLE (c-79): memory peak did not drop by the required 0.5 MB.
- Distinct names counted at merge (c-80): walking ~260,000 bits at merge added ~0.7 ms on the critical path (T 31.0 ms to 31.8 ms).
- Batching 8 records for SIMD parsing (c-85): stores rose from 17.8 M to 61.0 M and store-forwarding stalls rose from 209 to 1.26 M, making wall time 10.4 % slower (31.15 ms to 34.40 ms).
- Removing mailbox LAG backlog handling (c-91 s3 and s4): without the LAG path, a stalled owner's mailbox grew to 14.25 MiB in 1 of 50 runs.
- Per-owner (country, name hash) pair sets (c-23q): M 24.1 MB vs 16 MB baseline.
