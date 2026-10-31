# Track C: Join

The task: read records.csv (3.32 M records, each with a quoted, hostile name field) and regions.csv, join each record
to its region's country, and write one line per country with its count, sums, min/max and number of distinct names.

Best board run: `run-49eb81ea843f`, K 30.49 (T 0.032 s, M 13.9 MB, S 769), 74.2 points. D 0 dependencies
(`.speed-demon.toml`). K is the contest's speed score (higher is better); the other labels are defined in
[PERF.md](PERF.md).

On our test machine (same CPU model and settings as the scoring machine; 5 runs per dataset, the lower K counts) the
code in this commit reads K 31.42 (the lower of official 31.42 and fresh 31.46; T 0.031 s), M 13.9 MB, S 655, output
hash correct in 5/5 + 5/5 runs. The board run above is the code of push 7890539, which read K 30.85, M 13.5 MB, S 769
on the same machine; the difference is the lines cut afterwards (S 769 -> 655, PERF.md c-66).

Reproduce from the repository root: `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`
(full seed 1; the expected hash is c-join.md:212).

References written as c-join.md:<line>, here and in the code comments, point to the track's task spec: [specs/c-join.md](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/c-join.md) in the organisers' repository.

Official harness output for each measured submission commit: [reports/c/](../reports/c/) at the repository root, one file per commit, harness-<commit>.txt (the board's own record of that run).
The numbers in reports/ name the code commit they measured; the commit after it only adds those reports.

What the code does, row by row. Each `src/<file>:<line>` is the line that does it; the last column names the
experiment in [PERF.md](PERF.md) that measured it (IDs such as c-52 are entries in its log).

| What the code does | Where | PERF.md |
|---|---|---|
| Both files are mapped, never copied. Each mapping is followed by 96 guard bytes (`"`, `,`, LF repeated), so every scan stops at the guard and the parse has no bounds checks | `src/main.rs:38`, `src/main.rs:40`, `src/main.rs:43` | Decisions; floor1; c-35 to c-42 |
| records.csv is cut into 256 KiB chunks claimed from a shared counter; a chunk starts at the first LF followed by a digit or BOM; each chunk must stop exactly where the next starts, else the whole file is parsed again on one worker | `src/pass2.rs:21`, `src/pass2.rs:185`, `src/records.rs:251`, `src/pass2.rs:197`, `src/pass2.rs:166` | e1, c-19a2 |
| Commas and quotes are found 32 bytes at a time (AVX2 compare and movemask) | `src/records.rs:72` | c-10 |
| Digit runs (id, region id, amount) are read as one 8-byte load and folded into a number by SSE multiply-adds; a BOM is tested only on the id's slow path | `src/records.rs:119`, `src/records.rs:137`, `src/records.rs:153` | c-35 to c-42 (step 46) |
| CR-or-LF terminator skipped without a branch; amount sign applied without a branch | `src/records.rs:220`, `src/records.rs:184` | c-10 |
| Region id to country: one byte per region id. All workers build it together from 64 KiB pieces of regions.csv before they parse records; a lookup clamps the id with `min` instead of a bounds check | `src/regions.rs:92`, `src/regions.rs:96`, `src/pass2.rs:182`, `src/regions.rs:14` | floor1, c-47 |
| A record's name becomes a 16-byte message (a name of up to 16 ASCII bytes, with its country packed into spare bits), two messages (17 to 32 bytes), or its offset and length in the mapping (anything else) | `src/names.rs:85`, `src/names.rs:99`, `src/names.rs:107` | c-51, c-52 |
| The name is hashed with crc32c (one instruction per 8 bytes) and sent to the worker that owns the hash: owner = high bits of the hash times the worker count | `src/names.rs:64`, `src/pass2.rs:104` | c-52 |
| Messages are written to one 4 KiB bucket per owner through raw cursors; a full bucket is handed to the owner's mailbox | `src/pass2.rs:97`, `src/pass2.rs:129` | c-49 |
| The owner keeps each name once in its own table (64-byte buckets of 8 tagged entries, one AVX2 compare per half bucket, then an exact key compare) with one bit per country | `src/names.rs:151`, `src/names.rs:167`, `src/names.rs:285` | c-50, c-52 |
| The owner prefetches the home bucket of the message 16 message slots ahead | `src/names.rs:250` | c-50, c-63 |
| distinct names of a country = the sum over owners of their new (name, country) bits | `src/names.rs:285`, `src/pass2.rs:225` | c-52 |
| A sender tries to file a lagging owner's mailbox itself only once more than 16 full buckets wait there | `src/pass2.rs:29`, `src/pass2.rs:136` | c-52 |
| Each worker drops its share of the mapping's page-table entries before it finishes; the process exits without freeing | `src/pass2.rs:208`, `src/main.rs:76` | c-37 (step 46) |

## Approach

The design is: workers parse chunks, and each record's name is sent to the one worker that owns its hash. There is no
table that two workers update, and no atomic operation on names.

**Setup.** `main` maps both files (`src/main.rs:57`, `src/main.rs:58`) and starts 12 workers (`src/main.rs:54`; a test-only
environment variable can change the count; the main thread is worker 0, `src/pass2.rs:234`). Before any worker starts, it only reads regions.csv's header and its last
line to size the region table (`src/regions.rs:68`); it parses no region lines.

**Regions.** The table maps a region id to a country index, one byte per id (c-join.md:59: ids ascend, codes are
`[A-Z]{3}`). All workers fill it together: each claims 64 KiB pieces of regions.csv from a counter and stores, for every
line whose LF falls in its piece, that line's country index at its region id (`src/regions.rs:78`,
`src/regions.rs:92`). Country indices are handed out in first-seen order under a lock that is taken only for a country
that has no index yet; the output is sorted by key, so the numbering never shows. A worker then waits until every
piece is done (`src/regions.rs:96`). Group 0 is `_UNMATCHED`: ids with no line, or above the largest id, read as 0.

**Records.** records.csv is cut into 256 KiB chunks, claimed from a shared counter (`src/pass2.rs:185`). A chunk starts
at the first LF followed by a digit or a BOM, the only way a record can start (c-join.md:21, 36). The generator never
puts that pair inside a quoted field (c-join.md:50-51). A worker parses its chunk field by field: commas and quotes are
found with 32-byte AVX2 compares, `""` escapes are counted, a BOM before a record is skipped (c-join.md:36), and money
is kept in integer cents (c-join.md:37). Each record's country comes from the region table, and the record is folded
into that worker's own per-country slot: count, id sum, amount sum, min and max, name bytes, note bytes
(`src/records.rs:222`-`src/records.rs:230`). The parse then hands the name's position and length to the sender.

**Names.** For each record the worker builds a message from the name's raw bytes (between the quotes, escapes kept, which
is one to one with the value, c-join.md:27-28; up to the comma if unquoted) and the record's country (`src/names.rs:81`).
It hashes the key (crc32c; a fixed hasher for the rare names that fit neither message form) and writes the message
into the bucket for the worker that owns that hash (`src/pass2.rs:97`). A bucket holds 256 message slots (4 KiB) and
goes to the owner's mailbox when full; each worker files the buckets waiting in its own mailbox at every
hand-over and after every chunk (`src/pass2.rs:129`, `src/pass2.rs:198`). Filing means the owner looks the name up in its
own table, comparing the full key on every hash match (a name that fits neither message form goes through a plain
HashMap of the bytes it reads from the mapping), and sets the bit for the message's country in that name's 256-bit set.
If the bit was clear, the owner's count for that country goes up by one (`src/names.rs:285`). A country's number of
distinct names is the sum of these counts over all owners: a name always goes to the same owner, so it is counted once
per country. No two workers write the same table at the same time (each table is behind a lock). In a normal run only
the owner writes to it; the one exception is a sender that finds more than 16 full buckets waiting at an owner and
tries to file them itself (`src/pass2.rs:136`).

**Finish.** After its last chunk each worker drops its share of the mapping's page-table entries (`src/pass2.rs:208`),
hands over its partly filled buckets, and keeps filing its mailbox until every worker has handed over everything
(`src/pass2.rs:216`), then merges its slots and its owner's counts into the shared totals. `main` sorts the countries
bytewise (c-join.md:127-129), writes the file once (`src/main.rs:73`) and exits without freeing (`src/main.rs:76`).

**If a cut was wrong.** Each chunk checks that its parse stopped exactly where the next chunk begins
(`src/pass2.rs:197`). If any did not, the owners hold names from wrong records, so everything from that pass is
discarded and the whole file is parsed again on one worker (`src/pass2.rs:166`). The answer therefore never depends on the
rule that picks chunk starts.

## Limits

- Routing is about 35 % of T. On our test machine, with 12 workers pinned to the six P-cores (the scoring machine runs a
  track on cpus 0-11: six physical cores with two hyperthreads each), `perf stat` over 5 runs gave 31.96 ms for the whole
  run; 20.75 ms with the name step removed; 19.78 ms with the name hash also removed (those two builds give wrong
  output and were used only to measure). So parse, start, regions and write take about 19.8 ms, the name hash about
  1.0 ms, and routing (send, mailbox, owner filing) about 11.2 ms. Every record pays for it, and the owner design
  trades the cache-line traffic of a shared table with atomic bit updates (PERF.md c-40) for this message traffic. The
  split was measured on the code before c-66; c-66 rewrote the files for size and kept the hot path (callgrind, one
  worker: 1,179.7 M -> 1,165.4 M instructions).
- It needs a CPU with the x86-64-v3 instruction set (AVX2, BMI2, SSE4.2); it is built with `-C target-cpu=x86-64-v3`
  (`Dockerfile`).
- The region table supports at most 255 countries plus `_UNMATCHED`; more aborts (`src/regions.rs:89`). The spec has 240.
- A regions.csv line without its LF is outside the grammar (c-join.md:58-59); the last line without one stops the
  program with an error rather than a wrong answer (`src/regions.rs:33`).

## Dependencies

None: Rust's standard library only; mmap and madvise are called through short `extern "C"` blocks.

## Dropped ideas

Measured and dropped, with numbers, in [PERF.md](PERF.md): a shared name table with atomic bit updates (c-40), a
two-stage structural parse (c-58), profile-guided optimisation (c-61), a longer prefetch distance (c-63), the
region-table prefetch (c-62), one malloc arena (c-65), per-owner (country, name hash) pair sets (c-23).
