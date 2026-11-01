# Track C: performance log

Every step from the reference to the submitted code, each with the live-board run ID that measured it; readings from our own test machine are labelled as such.

The task: read records.csv (3.32 M records with a quoted, hostile name field) and regions.csv, join each record to its
region's country, and write one line per country with its count, sums, min/max and number of distinct names. K is the
contest's speed score (higher is better); the Labels list under the table defines every term used here.

## The journey

Times are the live board's T; every step has its run IDs in the table.

- The reference takes 14.44 s (step 0).
- A first correct build, one thread, the input mapped instead of read: 0.86 s (steps 1 to 3).
- Twelve workers, one shared table of names, and each country's set of names written by one owner worker: 0.17 s
  (step 4).
- Hits on the shared table without a lock, owner-bucketed buffers, AVX2 delimiter search, word-wise name hash and
  compare, then no rounds (chunks claimed from a counter, owner inboxes): 0.13 s at step 9 down to 0.06 s at steps 40 to 45.
- A lean parse over a padded mapping (digits folded by SSE multiply-adds, the BOM test off the hot path), a faster name
  table, page-table entries dropped by the workers in parallel at the end: 0.04 s (steps 46 and 47).
- The owner design (D-B): each name goes to the worker that owns its hash, which keeps it in a private table, so there is
  no shared table and no atomic operation on names; the region table is built by all workers: 0.03 s, 73.9 points
  (steps 48 and 49).
- The same program in fewer lines and less memory outside the name path (c-55, S 812 to 769): 0.03 s, 74.2 points
  (step 50).
- Every source file rewritten for size (c-66, S 769 to 655): 0.04 s, 73.6 points (step 51).
- One message format for every name, long names keyed by a 112-bit hash in the short table (c-81, S 655 to 608): 0.03 s, 75.2 points (steps 52 to 57).
- Line cuts, region-table prefetch, unified text search with escape flag, and memory-sized tables (c-90d, S 608 to 571, M 14 to 11 MB): 0.03 s, 77.4 points (step 58).

What did not work, with the measurements (details in the log):

- One shared name table with an atomic OR on its bits (c-40): 58.2 ms against 31.5 ms for the owner design at 12
  workers; cross-core snoop loads 3.03 M against 0.23 M per run, because each new bit pulls a cache line from
  another core.
- A two-stage structural parse (c-58): 37 % faster on one thread (114 against 181 ms) but only 5 % faster at 12 workers,
  because two hyperthreads share one core's execution ports and the method executes more instructions.
- Profile-guided optimisation (c-61): 1.0 to 1.2 ms slower in two A/B rounds.
- Prefetching the region table cell (c-62): 41 % fewer L2 misses, no time saved, because those misses were never what
  the cores waited on.
- One malloc arena (c-65): 0.3 MB less memory, but T 0.031 to 0.033 s and K 30.85 to 29.87.
- Number parsing in digit lanes (c-74): shorter dependency chains bought with more instructions ran slower (+0.5 to +2.5 % instructions, T 0.031 s to 0.033 s).
- Distinct names counted at the end (c-80): walking ~260,000 bits at merge added ~0.7 ms on the critical path (T 31.0 ms to 31.8 ms).
- Batching 8 records for SIMD parsing (c-85): stores rose from 17.8 M to 61.0 M and store-forwarding stalls rose from 209 to 1.26 M, making full wall time 10.4 % slower (31.15 to 34.40 ms).

| Step | Change | Run ID | T (s) | M (MB) | Points | KNOWN/OURS | Spread (board T, same code) | Kept |
|---|---|---|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 | | | |
| 1 | floor1: first correct build, one thread, input mapped | 1d0fa92 · Track C (run-605dbc1c67cf) | 0.86 | 10 | 42.6 | KNOWN | 0.79-0.93 s, 3 runs | kept |
| 2 | re-run of step 1 (another push) | dea7a01 · Track C (run-c4eb9098eeb1) | 0.93 | 10 | 41.8 | KNOWN | 0.79-0.93 s, 3 runs | re-run |
| 3 | re-run of step 1 (another push) | 98a661e · Track C (run-a2be17d4d6ad) | 0.79 | 10 | 43.5 | KNOWN | 0.79-0.93 s, 3 runs | re-run |
| 4 | E2: twelve workers, one shared name table, owner-written name bitsets | 5ecc03f · Track C (run-6f175e156c66) | 0.17 | 16 | 56.4 | OURS | 0.16-0.17 s, 5 runs | kept |
| 5 | re-run of step 4 (another push) | dbc91d1 · Track C (run-9dc01455a1b6) | 0.16 | 16 | 56.7 | OURS | 0.16-0.17 s, 5 runs | re-run |
| 6 | re-run of step 4 (another push) | bdf0564 · Track C (run-abaf5b333b4c) | 0.17 | 15 | 56.5 | OURS | 0.16-0.17 s, 5 runs | re-run |
| 7 | re-run of step 4 (another push) | 5263115 · Track C (run-ead27acd0328) | 0.17 | 16 | 56.6 | OURS | 0.16-0.17 s, 5 runs | re-run |
| 8 | re-run of step 4 (another push) | 68a1f56 · Track C (run-b1890e0c1e2b) | 0.16 | 15 | 56.7 | OURS | 0.16-0.17 s, 5 runs | re-run |
| 9 | c-6: name-table hits without a lock | 376e3cd · Track C (run-5cfdae8378cd) | 0.13 | 22 | 57.2 | KNOWN | 0.13 s, 2 runs | kept |
| 10 | re-run of step 9 (another push) | b80b71e · Track C (run-6b0aeec09c5b) | 0.13 | 22 | 57.0 | KNOWN | 0.13 s, 2 runs | re-run |
| 11 | c-7: memory recovered at the same speed (8-byte slots, one lazily mapped arena) | 704a88e · Track C (run-96e1148bb559) | 0.14 | 16 | 57.5 | OURS | 0.13-0.14 s, 2 runs | kept |
| 12 | re-run of step 11 (another push) | cac9dd1 · Track C (run-b08a7ba7612b) | 0.13 | 16 | 58.3 | OURS | 0.13-0.14 s, 2 runs | re-run |
| 13 | c-9: owner-bucketed round buffers | 442dbe7 · Track C (run-75fe344f3f7e) | 0.10 | 16 | 61.0 | OURS | 0.10-0.11 s, 4 runs | kept |
| 14 | re-run of step 13 (another push) | 948854a · Track C (run-2c36889cbf6d) | 0.11 | 15 | 60.7 | OURS | 0.10-0.11 s, 4 runs | re-run |
| 15 | re-run of step 13 (another push) | dc6ca65 · Track C (run-f7359dc6db71) | 0.10 | 16 | 60.8 | OURS | 0.10-0.11 s, 4 runs | re-run |
| 16 | re-run of step 13 (another push) | 22b6620 · Track C (run-8ea580f8f21b) | 0.11 | 16 | 60.6 | OURS | 0.10-0.11 s, 4 runs | re-run |
| 17 | c-10 + c-9.1: AVX2 delimiter search, branch-free terminator, arena block fix | 90c864a · Track C (run-63963655e56f) | 0.09 | 16 | 62.3 | KNOWN | 0.09-0.13 s, 2 runs | kept |
| 18 | re-run of step 17 (another push) | 2e2ee13 · Track C (run-4ff8954aecb6) | 0.13 | 16 | 58.1 | KNOWN | 0.09-0.13 s, 2 runs | re-run |
| 19 | c-11: word hash and word compare for names | 1e9607b · Track C (run-a11e7d85f631) | 0.08 | 16 | 63.7 | KNOWN | 0.08 s, 2 runs | kept |
| 20 | re-run of step 19 (another push) | 48c9f56 · Track C (run-e9c771d0a5cd) | 0.08 | 16 | 63.1 | KNOWN | 0.08 s, 2 runs | re-run |
| 21 | e1 + c-12.1: record starts by the LF + digit/BOM rule, pairwise end check, one-worker rerun; slots hold record addresses | 83d39fc · Track C (run-a085c737e765) | 0.07 | 16 | 64.6 | OURS | 0.07-0.08 s, 3 runs | kept |
| 22 | re-run of step 21 (another push) | e71baea · Track C (run-60bc42cad400) | 0.07 | 16 | 64.6 | OURS | 0.07-0.08 s, 3 runs | re-run |
| 23 | re-run of step 21 (another push) | c32fd6a · Track C (run-c7f098288806) | 0.08 | 16 | 64.2 | OURS | 0.07-0.08 s, 3 runs | re-run |
| 24 | s1 + c-16: the same program in fewer lines; C runtime linked statically | 2979028 · Track C (run-074941498e3b) | 0.07 | 16 | 64.9 | KNOWN | 0.07-0.10 s, 10 runs | kept |
| 25 | re-run of step 24 (another push) | 0c42543 · Track C (run-eb908b15b18f) | 0.08 | 16 | 64.3 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 26 | re-run of step 24 (another push) | b581408 · Track C (run-e264d4515083) | 0.08 | 16 | 64.5 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 27 | re-run of step 24 (another push) | e2b846c · Track C (run-42013c79b7b8) | 0.10 | 17 | 61.7 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 28 | re-run of step 24 (another push) | 5d16aa8 · Track C (run-ebe010ccb777) | 0.08 | 16 | 63.9 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 29 | re-run of step 24 (another push) | dc1cab8 · Track C (run-17d8b9bf4f29) | 0.07 | 15 | 65.1 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 30 | re-run of step 24 (another push) | 5c78ab8 · Track C (run-c802d0ba14e2) | 0.08 | 16 | 64.6 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 31 | re-run of step 24 (another push) | 59a4bfa · Track C (run-1ea5109eca4d) | 0.08 | 16 | 63.6 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 32 | re-run of step 24 (another push) | 8c4d4fe · Track C (run-5c745581d28a) | 0.07 | 16 | 65.2 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 33 | re-run of step 24 (another push) | 9224ad7 · Track C (run-280a11b1a137) | 0.08 | 16 | 64.5 | KNOWN | 0.07-0.10 s, 10 runs | re-run |
| 34 | c-17: a range cut inside a quoted field ends early instead of aborting; a correctness fix, speed-neutral on our test machine (box 2): K 20.32 against 20.21 before, T 0.071 s both | 6ba03de · Track C (run-0d1a65d51b10) | 0.08 | 16 | 64.8 | OURS | 0.08 s, 2 runs | kept |
| 35 | re-run of step 34 (another push) | 8604c2c · Track C (run-c2311de2ab18) | 0.08 | 16 | 64.8 | OURS | 0.08 s, 2 runs | re-run |
| 36 | s2 + s2b: the same program in fewer lines (8765712); README and PERF.md | 67c71f1 · Track C (run-5b8cc04ed3d8) | 0.07 | 16 | 65.5 | KNOWN | 0.07-0.08 s, 4 runs | kept |
| 37 | re-run of step 36 (another push) | 6b8f071 · Track C (run-36d9a0094bd8) | 0.08 | 16 | 64.8 | KNOWN | 0.07-0.08 s, 4 runs | re-run |
| 38 | re-run of step 36 (another push) | da6f278 · Track C (run-c60b67f889bb) | 0.08 | 16 | 65.3 | KNOWN | 0.07-0.08 s, 4 runs | re-run |
| 39 | re-run of step 36 (another push) | c54220e · Track C (run-cc2a0b3ec2c9) | 0.08 | 16 | 64.9 | KNOWN | 0.07-0.08 s, 4 runs | re-run |
| 40 | c-19a + c-19a3 + c-20a: pass 2 hands out chunks to free workers, owner inboxes, retired name-table generations freed at chunk boundaries, one packed word per shard; README and PERF.md (c-24) | e2c4246 · Track C (run-7971e1f1642f) | 0.06 | 18 | 66.1 | OURS | 0.06-0.07 s, 3 runs | kept |
| 41 | re-run of step 40 (another push) | 5a559a8 · Track C (run-782829ac98a7) | 0.06 | 15 | 66.9 | OURS | 0.06-0.07 s, 3 runs | re-run |
| 42 | re-run of step 40 (another push) | 2fd5fd7 · Track C (run-f2fa240567f9) | 0.07 | 16 | 66.4 | OURS | 0.06-0.07 s, 3 runs | re-run |
| 43 | c-22 + c-26: the name-table slot is prefetched as soon as the name is parsed; the same program in fewer lines (S 657 to 582); README and PERF.md (c-27) | 63c975d · Track C (run-f9682152b66d) | 0.06 | 16 | 67.6 | KNOWN | 1 run | kept |
| 44 | re-run of step 43 (another push) | c8760d4 · Track C (run-2771b128c250) | 0.06 | 15 | 68.0 | KNOWN | 0.058-0.060 s, 3 runs | re-run |
| 45 | re-run of step 43 (another push) | 19e5d85 · Track C (run-e40aeefdf850) | 0.06 | 15 | 68.2 | KNOWN | 0.058-0.060 s, 3 runs | re-run |
| 46 | c-35, c-37, c-39, c-42: a lean parse over a padded mapping (digits folded by SSE multiply-adds, the BOM test on a cold path), a name table of 4-entry buckets with one tag word, page-table entries dropped in parallel at the end; S 582 to 729 | af85844 · Track C (run-8d78dc657a67) | 0.04 | 14 | 71.2 | OURS | 0.041-0.042 s, 2 runs | kept |
| 47 | re-run of step 46 (another push) | 1be93e4 · Track C (run-a47a8fbd9b44) | 0.04 | 14 | 71.8 | OURS | 0.041-0.042 s, 2 runs | re-run |
| 48 | c-47, c-52, c-57: each name goes to the worker that owns its hash, which keeps it in a private table (no shared table, no atomics on names); the region table built by all workers; S 729 to 812 | 3cfa79a · Track C (run-653e9ee499e8) | 0.03 | 14 | 73.9 | KNOWN | 0.032 s, 2 runs | kept |
| 49 | re-run of step 48 (another push) | 178ceb2 · Track C (run-ff90feddd01c) | 0.03 | 14 | 73.9 | KNOWN | 0.032 s, 2 runs | re-run |
| 50 | c-55: the same program in fewer lines and less memory outside the name path (two file-mapping functions merged, the one-worker rerun written once, the region lines scanned by a plain byte loop, the long-amount function folded into `amount`); S 812 to 769 | 7890539 · Track C (run-49eb81ea843f) | 0.03 | 14 | 74.2 | KNOWN | 1 run | kept |
| 51 | c-66: every source file rewritten for size, one file per step; S 769 to 655; our test machine (box 2): K 31.42 against 30.85 for step 50's code, T unchanged (the gain is S) | b769e68 · Track C (run-90039a78bf5b) | 0.04 | 14 | 73.6 | KNOWN | 1 run | kept |
| 52 | c-81: one message form for every name; names up to 16 bytes raw, longer names keyed by 112 bits of a two-lane multiply hash in the short table; medium table and HashMap eliminated; S 655 to 608 | 29fe7fb · Track C (run-dd199ba14e7e) | 0.03 | 14 | 75.2 | OURS | 0.03 s, 6 runs | kept |
| 53 | re-run of step 52 (another push) | 134b2b2 · Track C (run-f6bd9bfdf29e) | 0.03 | 14 | 75.4 | OURS | 0.03 s, 6 runs | re-run |
| 54 | re-run of step 52 (another push) | 60b5493 · Track C (run-ff429b993219) | 0.03 | 14 | 75.5 | OURS | 0.03 s, 6 runs | re-run |
| 55 | re-run of step 52 (another push) | e550f50 · Track C (run-ac81d893dd4b) | 0.03 | 14 | 75.2 | OURS | 0.03 s, 6 runs | re-run |
| 56 | re-run of step 52 (another push) | f245483 · Track C (run-14487cf27e76) | 0.03 | 14 | 75.6 | OURS | 0.03 s, 6 runs | re-run |
| 57 | re-run of step 52 (another push) | 585ce17 · Track C (run-ec016d38390e) | 0.03 | 14 | 74.4 | OURS | 0.03 s, 6 runs | re-run |
| 58 | c-90d: c-83 line cuts, c-87/c-88 region-table prefetch, c-89 r1 s1 escape flag, c-84 s0/s2 and c-84b; S 608 to 571 | b1438ef · Track C (run-9633f0eb49ea) | 0.03 | 11 | 77.4 | OURS | 0.03 s, 2 runs | kept |
| 59 | re-run of step 58 (another push) | 55a0bc1 · Track C (run-e709054008e5) | 0.03 | 12 | 76.0 | OURS | 0.03 s, 2 runs | re-run |

Spread: the range of board T over every run of the same code; each board run is a single run. Readings labelled "our test machine" below are not board runs and have no run ID: K is the lower of the official and fresh readings, 5 runs each, with T's min-max where given.

## Labels

- K: the contest's speed score, computed from T, M, S and D; higher is better.
- T: wall time of one run, in seconds.
- M: peak memory of one run, in MB.
- S: the size of the code in lines, as the contest counts them.
- D: the number of dependencies (0 for this track).
- official: the organisers' dataset (seed 1).
- fresh: a dataset from a seed we never saw, made by the same generator at the same size.
- KNOWN: a known technique.
- OURS: our own idea.
- our test machine: a machine with the same CPU model and settings as the scoring machine; 5 runs per dataset, and the lower K of the two datasets counts.
- IDs such as c-47 or e1 are experiment IDs in the log below; run-... IDs are live-board runs; "step N" is a row of the table above.

## Experiment log

Each experiment's numbers are added here once the live board has measured it and they can cite its run ID.

- floor1, commit 6111c8a: the first correct build, single-threaded (board rows 1 to 3 are this build).
- floor12, commit bd5253a: twelve workers over equal byte ranges, record starts found by a two-state quote-parity
  pre-pass, distinct names by per-worker fingerprint lists sorted and merged at the end. Kept as the parallel baseline;
  its fingerprint lists make memory grow with the record count.
- E2, commit 1e98c34 (board row 4, 5ecc03f · Track C): one intern table shared by all workers (mutex shards, exact byte
  compare on every hit) and per-country name bitsets written by their owning worker in bounded rounds. Slower than
  floor12 on the box but far smaller in memory, so a better score; kept. It attacked the pass 2 name table.
- c-5 arena, commit 60e701c: intern hits compare against a per-shard copy of each distinct name instead of the input
  mapping. Verdict NOISE on the box score; merged into c-6, which needs the arena's stable storage.
- c-6 lock-free, commit ef1b494: intern hits take no lock (atomic slots published with Release, read with Acquire;
  names in append-only arena chunks); the shard lock is taken only to insert. Verdict KEEP on the box score (board rows 9 and 10,
  376e3cd · Track C and b80b71e · Track C). It attacked the pass 2 name lookups.
- c-7 mem, commit 537bf48: memory recovered at the same speed: retired slot generations freed at the round barrier,
  8-byte slots naming records in a directory-addressed arena, and one lazily mapped arena with per-shard reservations.
  Verdict KEEP, box K 14.73 (spread not recorded; board row 11, 704a88e · Track C). It attacked the
  pass2+names phase's memory.
- c-8 prefetch, commit 36e6eaf: a ring that prefetched each name's home slot and then its arena record some records
  ahead of the exact lookup. Verdict DROP, box K 14.61: T unchanged at 0.127, S 755 -> 819. Faster on the test VM, not
  on the box, where the intern state fits in L3.
- Probes P8 (never lifted, wrong output on purpose): nolookup (d3d89b7, a stand-in for the intern lookup) and noowner
  (7632abe, the round's owner step removed). Their box phase lines put the lookups at about a third of pass 2 and the
  owner step at about a fifth.
- c-9 owner, commit 829f91b: each worker files its round pairs into one bucket per owning worker, through a group
  table built once, so an owner reads only the buckets addressed to it. Verdict KEEP, box K 16.54 (T 0.101-0.105 s;
  board row 13, 442dbe7 · Track C). It attacked the pass 2 owner step. The variant with
  twice the bucket capacity (538d5b6) scored box K 16.33 and was not shipped.
- c-9.1 fix, commit 7fa72a4 (correctness fix): the shared name arena allocated a block again after a reservation skipped
  several blocks (a very long name), losing the names already in it; the block is now allocated by its own directory
  pointer. Found by the correctness kit's long-name cases.
- c-10 scan, commit 5c161a3: text fields' commas and quotes found with a 32-byte AVX2 compare instead of a byte loop,
  and the CR-or-LF terminator skipped without a branch. Verdict KEEP, box K 17.97 (T 0.086-0.088 s; board row 17,
  90c864a · Track C, with c-9.1). It attacked the parse. SWAR parsing of the digit runs was tried and dropped:
  exact, but no gain, as the numbers here are at most eight digits.
- c-11 lookup, commits 01326a0 and e843890: the name hash built from the name's first and last words with one 128-bit
  multiply, and names up to sixteen bytes compared as two words. Verdict KEEP, box K 19.01 (T 0.077-0.077 s; board
  row 19, 1e9607b · Track C). It attacked the pass 2 name lookups.
- c-12.1 slot address, commit 1ee7bd1: a name-table slot holds the record's address instead of an arena offset, so a hit
  skips the block arithmetic. Verdict NOISE, kept on the line: box K 19.17 (T 0.076-0.078 s) and 19.15 against
  19.01. It attacked the pass 2 name lookups.
- e1 record starts, commit cbdf26e: each worker's range starts at the first LF followed by a digit or a BOM, found by a
  short search instead of a full quote-parity pre-pass; every worker checks that it stopped exactly on the next range's
  start, and a mismatch reruns the file on one worker. Verdict KEEP, box K 19.84 (T 0.071-0.073 s; board 83d39fc ·
  Track C and e71baea · Track C). It attacked pass 1's record-start scan.
- c-16 static C runtime, commit f133779 (cbdf26e + one Dockerfile line): `+crt-static` links the C runtime into the
  binary, so the process starts without the dynamic loader. Measured on box 2: M -0.3 MB (15.0 against 15.3 MB), T
  and S unchanged; box K 19.94 official / 19.98 fresh against cbdf26e's 19.89 / 19.84 (spread not recorded). Kept:
  folded into the board's s1 runs (rows 24 to 33).
- s1, s2, s2b folds (1c9ace2, c3c4c74, 8765712): the same program in fewer code lines. Box K 19.97 (spread not
  recorded), 20.39 and 20.73 (T 0.071-0.072 s). Kept: s1 in board rows 24 to 33, s2 + s2b in rows 36 and 37.
- c-17, commit 884c6d0: every read in the record loop is bounds-checked, so a range cut inside a quoted field ends
  early instead of aborting; the garbage record past the file's end is dropped, pass 2's exact check fails, and main
  reruns the file on one worker. It attacked the parse's cut-past-end case. Verdict KEEP (board rows 34 and 35,
  6ba03de · Track C).
- Probes, never lifted: c-13 regions parsed on all workers (faster, but too many lines; parked); c-15 memory probes
  (smaller round buffers, a higher table load, one malloc arena, fewer workers, names compared against the mapping:
  each traded more speed than it saved in memory); c-18 time-budget probes (page faults cost little; the parse and the
  name lookups are the time; thirteen workers on twelve CPUs stretch the round design by about a third).

- c-19a pass 2 without rounds, commit cdec073: workers claim chunks from a shared counter and hand full buckets to
  each owner's inbox instead of meeting at two barriers per round. Alone it lost on the box score (box K 20.39 against
  s2b's 20.73, T 0.071-0.072 s: faster, but more memory), kept as the base of the next two. It attacked pass 2's round
  barriers. Shared bits set by atomic OR from any worker (no owners) were tried first and dropped: far slower on the
  test VM.
- c-19a2 smaller chunks and buckets, commit a6996c6: the inbox backlog scales with the chunk. Verdict KEEP, box K
  20.86. It attacked pass 2's inbox backlog.
- c-19a3 retired intern generations freed at chunk boundaries by eras, commit a3023b6. Verdict KEEP, box K 21.65
  (T 0.064-0.064 s). It attacked pass 2's retired intern generations. With
  thirteen workers on twelve CPUs this design loses about a fiftieth of its speed, where the round design lost about
  a third (probe 393ca19).
- c-20a packed slot word, commit 7cb92a6: a name lookup reaches its slot array from one atomic word (address plus
  size) instead of through the generation's header, one dependent load fewer per lookup. Verdict KEEP, box K 21.95
  (T 0.061-0.062 s). It attacked the pass 2 name lookups.
- c-20b line-aligned arena records, commit 415717a: records that would straddle a cache line moved to the next line.
  Verdict FAILED, reverted: speed unchanged on the box, memory up on the fresh dataset.
- c-23p owner-local name sets, commit ff75859: each country's name set kept by its owner, keyed by country and
  name-hash pairs. Built to a wrong spec, so its readings are void: box K 11.75 official / 11.73 fresh
  (T 0.120-0.126 s), M 62.5 MB, S 665. Verdict VOID.
- c-23q owner-local name sets, commit 835b27e: one set entry per name with the country marks packed into it, instead
  of one bitset per country. Box K 20.27 official / 20.33 fresh (T 0.058 s), M 24.1 MB, S 715. Verdict DROP.
- c-23r slot prefetch from the parse, commit 1a6b8bc: the parse's prefetch hook hashed the name and the lookup hashed
  it again. Box K 22.60 official / 22.52 fresh (T 0.058-0.059 s), M 15.0 MB, S 650. Verdict SUPERSEDED by c-22, which
  hands the hash to the lookup instead of computing it twice.
- c-22 slot prefetch in the parse, commit de1a7fb: the parse calls a hook as soon as a record's name is parsed; it
  hashes the name and prefetches its slot, and the lookup runs once the rest of the record is parsed, so the slot's
  cache miss overlaps that work. Box K 22.76 official / 22.96 fresh (T 0.057 s), M 15.2 MB, S 657, against the base
  7a0c756's 22.14 / 22.09 (T 0.061-0.062 s, M 15.1 MB). Verdict KEEP. The structural parse it replaced as this step's
  plan was not built: its unfinished version gave a wrong full-data hash on the box.
- c-25 structural parse, commit c74cefd: each 64-byte block classified once into quote, comma and LF masks with the
  quote parity carried as a prefix XOR, fields walked by bit position; the per-field AVX2 search went. Box K 22.23
  official / 22.17 fresh (T 0.059 s), M 15.0 MB, S 712, against the base 6f01659's 22.93 / 22.85 (T 0.056-0.057 s,
  S 657): T 2-3 ms slower, S up. Verdict FAILED, reverted (52c0cf1).
- c-26 S cut, commit a13436c: the same program in fewer code lines (S 657 -> 582, rustfmt defaults): names.rs
  277 -> 241, pass2.rs 123 -> 97, records.rs 123 -> 118, regions.rs 52 -> 49, main.rs 74 -> 70, Dockerfile 8 -> 7.
  The slot array lost its wrapper struct and one-element generation list, the worker body became a closure outside
  the thread scope, a slot starts with min and max at the extremes (no first-amount branch), unreachable asserts and
  an always-true hook result went. Box K 23.70 official / 23.68 fresh (T 0.054-0.055 s), M 15.0 MB, against c-22 tip
  6f01659's 22.93 / 22.85. Verdict KEEP.

- c-35, c-37, c-39, c-42 (step 46; board runs run-8d78dc657a67 and run-a47a8fbd9b44): a lean parse over a padded
  mapping, a faster name table, and page-table entries dropped by the workers. Expected to help because the earlier
  probes had put the time in the parse and the name lookups. The parse reads every field with unchecked loads over a
  mapping that ends in guard bytes (no bounds checks); digit runs are one 8-byte load folded by SSE multiply-adds; the
  BOM test moved to the id's slow path. Test-machine T fell from 0.055 s to 0.047 s with the parse change alone
  (c-35: K 25.26 fresh / 25.32 official, against 23.68). c-37f (each worker drops its share of the mapping's page-table
  entries after its last chunk, instead of one CPU doing it at exit): test-machine K 25.66, T 0.045-0.046 s. c-37b
  (exit right after the write, skipping the frees): K 25.40 against 25.31, inside the noise; kept because it cannot
  change the output. c-39, a probe with the name path switched off (wrong output on purpose): test-machine T 0.0234 s
  against 0.045 s, so the names cost about 21.6 ms, 48 % of T. Board: T 0.04 s, 71.2 points (step 46) and 71.8
  (step 47). Verdict KEEP. Only the group is cited here: which of the four IDs gave how much of step 46 is not separated.
- c-40 shared bits with an atomic OR, no routing: the per-country name bits kept in one table that every worker sets
  with a load and an atomic fetch_or, so that nothing is sent anywhere. Expected to win by removing the message and
  owner step. Test-machine K 23.53 official / 23.43 fresh (T 0.058 s) against 25.70 (T 0.045 s) for the shared-table
  design of the time: 13 ms slower. Later measured against the owner design at 12 workers, with hardware counters: 58.2 ms
  against 31.5 ms, cross-core snoop loads 3.03 M against 0.23 M per run, IPC 1.04 against 1.45: each new bit pulls a
  cache line from another core. Verdict DROP; this is why the owner design exists.
- c-47 (in step 48): regions.csv was parsed by one thread, about 4.2 ms, before any worker started. Now the workers
  start first and build the region table together from 64 KiB pieces (regions.rs `Build`). An earlier try at the same
  idea, c-35f (regions parsed on all workers), had read flat. Test machine, K over three readings of the new code:
  28.30 to 28.58, against 27.37 to 27.55 before. Steps: s1 (an AVX2 scan of the region lines) K 28.08, T 0.038 s;
  s2 (workers start first and build the table together, first version) K 27.97, T 0.036 s but S 848 and M 14.1 MB,
  reverted; s2b (the same, leaner) K 28.39 (official 28.56 T 0.036 s, fresh 28.39), S 778, M 14.0 MB, kept; s3a (fewer
  lines in the old name routing, since replaced) K 28.58 (official 28.85 T 0.035 s, fresh 28.58 T 0.036 s), S 788,
  M 13.7 MB. Verdict KEEP. The AVX2 scan was later replaced by a plain byte loop
  (c-55 s2): pieces are 64 KiB per worker, too short for it to pay.
- c-49, c-50, c-51 (probes that shaped c-52; test-machine ms are the parallel pass, median of runs): how to send names
  to their owner. c-49: a sender writes into its per-owner buckets through raw cursors instead of Vec pushes: pass
  22.47 ms against 23.54 ms. c-50 and c-51: where to pack the name into a message. The sender packing it while its bytes
  are still in L1 (c-51) gave a pass of 23.58 ms; the owner reading the name from the mapping and packing it gave
  27.3 ms. By differences between probes, the owner's read and pack cost about 5.4 ms and the sender's pack about 3 ms.
  The owner's table of 64-byte buckets compared with AVX2, with a prefetch of its bucket 16 messages ahead, comes
  from c-50. Verdict: all three kept inside c-52.
- c-52 owner design (D-B), in step 48: each name goes, as a message, to the worker that owns its hash, which keeps it
  in a private table with one bit per country; there is no shared table and no atomic operation on names. Expected to
  win because c-40 showed that sharing the bits costs cache-line transfers, and the messages go in 4 KiB batches.
  Test machine: s1 K 30.22; s2 K 30.49 (official 30.49 T 0.031 s, fresh 30.52 T 0.031 s), M 13.6 MB, S 806; s2b K 30.32,
  reverted; s2c (a hand-over files its own mailbox only when a lock-free count says buckets wait) K 30.24, S 806 -> 822
  and M 13.6 -> 13.8 MB, reverted. The shared-table design before it read K 28.58 (one reading); the median of two readings
  for each was 30.38 against 28.535, +6.5 %, beyond both spreads (0.22 and 0.09). Verdict KEEP s2.
- c-57 (in step 48): an empty regions.csv, outside the grammar, maps nothing instead of aborting. A correctness fix;
  test-machine K 30.55 (official 30.55 T 0.031 s, fresh 30.57 T 0.031 s), M 13.6 MB, S 812, output hash 5/5 + 5/5, the
  same as c-52 s2 within the spread. This is the code of push 3cfa79a: board run-653e9ee499e8, K 30.04, T 0.0321 s,
  M 14.2 MB, S 812, 73.9 points (step 48). Verdict KEEP.
- c-55 (in step 50): cut S and M everywhere except the name path, one step at a time,
  keeping a step only when S or M fell and T stayed inside the spread. s1: two functions that mapped a file merged into
  one, the one-worker rerun written once, a record as a (key, groups) tuple, one helper inlined: test-machine K 30.65
  (official 30.72, fresh 30.65), M 13.7 MB, against 30.55. s2: the region lines scanned by a plain byte loop, the
  long-amount function folded into `amount`, the owner's multiply-shift written at its two send sites. Together:
  S 812 -> 769, test-machine K 30.85 (official 30.93, fresh 30.85; T 0.031 s), M 13.5 MB, output hash 5/5 + 5/5. The
  code of push 3cfa79a read K 30.46 (official 30.46, fresh 30.56), M 13.9 MB on the same machine. Verdict KEEP. This is the code of
  push 7890539: board run-49eb81ea843f, K 30.49, T 0.0320 s, M 13.9 MB, S 769, 74.2 points (step 50).
- c-58 two-stage structural parse, four rounds: stage 1 finds every comma and quote of a 64-byte block with SIMD
  masks, stage 2 reads the fields from the masks. Expected to help because it executes fewer instructions per
  field. On one thread it is 37 % faster (114 ms against 181 ms), but at 12 workers only 5 % (pass 16.3 ms against 17.1
  ms): two hyperthreads share one core's execution ports, and the method executes more instructions in total
  (1040 M against 796 M); the fourth round raised them further (1091 M, 17.3 ms). Verdict DROP. (c-25, a similar try on
  the earlier design, also failed: see above.)
- c-61 profile-guided optimisation: a profile-instrumented build, then a build using the profile. Expected to help
  through better code layout and inlining. Test machine, two A/B rounds: 34.84 and 34.48 ms against 33.67 and 33.47
  ms; one K reading 30.17 (official 30.34, fresh 30.17; T 0.032 s), M 13.7 MB, S 777 against 30.85 (T 0.031 s).
  Verdict DROP: slower, outside both spreads.
- c-63 prefetch distance: the owner's bucket prefetch 16 -> 32 and 48 messages ahead. Expected to help if the prefetch
  was late. Test-machine K, official / fresh: 30.76 / 30.81 (32) and 30.85 / 30.83 (48), against 30.93 / 30.85 (16): inside
  the run-to-run spread. Verdict DROP; 16 stays.
- c-65 one malloc arena (`MALLOC_ARENA_MAX=1`): expected to save memory, as threads then share one heap. Test machine:
  M 13.4 -> 13.1 MB, but T 0.031 -> 0.033 s and K 30.85 -> 29.87. Verdict DROP.
- c-66 (step 51): every source file rewritten for size, one file per step, each step
  read on the test machine (lower K): s1 regions.rs 30.55, s2 main.rs 30.75, s3 records.rs 30.97, s4 pass2.rs 30.00
  (official reading; M 16.5 MB), s5 names.rs 31.38, s6 cleanup 31.42 (official 31.42, fresh 31.46, T 0.031 s,
  M 13.9 MB). S 769 -> 655. s4's M 16.5 MB did not come back: 30 runs each in a fresh memory cgroup read d22e493 at a
  median 13.6 MB (12.6-14.2) and c-66 at 13.7 MB (13.0-14.7). The S term alone predicts +2.4 % K and the M change
  -0.6 %; T is unchanged. Verdict KEEP. This is the code of push b769e68: board run-90039a78bf5b, K 29.60, T 0.0353 s,
  M 14.2 MB, S 655, 73.6 points (step 51). Our test machine read the same push at K 31.48 (T 0.031 s), so that board run
  sits about 6 % below it, against about 1 % for push 7890539.

- c-67 table load threshold: raising the owner name table growth threshold from 3/4 to 15/16 (commit cca902d).
  Each owner's short names sat in 2,048 buckets (45 % load); growing past 15/16 would hold them in 1,024 buckets (91 % load),
  halving index memory by about 0.87 MB. On our test machine, s1 measured official K 31.50, M 13.15 MB and fresh K 31.39,
  M 13.25 MB vs baseline c205772 (official K 31.42, fresh K 31.46): memory fell by 0.7 MB but T slowed by about 3 %, leaving
  K flat. Verdict DROP: memory saved was paid back in execution time.
- c-68 instruction-free line cuts: a second round of line cuts across cold paths and file merges (commits ad992e8,
  1eb29b6, 6b75c4d, 17e2b07), restricted to cuts costing zero instructions under callgrind at one worker. On our test
  machine, s1 scored K 31.34 (S 651), s2 K 31.39 (S 645), s3 K 31.44 (S 636), and s4 S-rs 622; s4 increased instructions
  by +0.67 % because inlining loop bodies added an instruction per record. Verdict DROP file merges and dropped mmap assert;
  in-file cuts (~11 lines) kept later. Merging files risked losing code quality marks for a negligible speed score gain (+0.08 track points).
- c-69 timeline probe (commit e70cf64, never lifted): instrumentation probe placing timestamps at worker lifecycle
  boundaries to separate serial from parallel execution. On our test machine (box 1, 4 runs, inner T 31.07 to 31.72 ms,
  M 13.4 MB), workers started in 0.27 to 0.80 ms; parallel parsing took 25.4 to 27.1 ms (last-worker parse spread was 0.9 ms,
  drain spread 0.5 ms); join, merge, and write took 0.82 ms; teardown and exit took 0.98 ms. Serial and idle overhead totaled
  about 3.5 ms, while the parallel phase took about 28 ms. Verdict PROBE (measure-only): confirmed that parallel execution
  dominated wall time.
- c-70 hot-path line cuts: planned third round of line cuts on hot-path files evaluated under a net score gate balancing
  lines against instructions. Not built on our test machine; replaced when leaderboard review showed that short rival
  entries were different architectures rather than our design compacted, directing effort toward whole-mechanism removal.
  Verdict SUPERSEDED by the mechanism-removal strategy and c-83.
- c-71 no-mailbox routing: removing cross-worker mailboxes in two variants: v1 (commit 1c37dbd, owner=sender: each worker
  keeps private per-country name tables) and v2 (commit 27026a0, routing without mailboxes: all workers set bitsets directly).
  On our test machine, v1 private tables held about 94 % of distinct names, increasing memory by +30 to 70 MB (M 45 to 80 MB);
  breaking even at that memory required T of 16 to 20 ms, which was below the ~19.8 ms floor measured with names disabled (v3).
  In v2, direct bitset writes caused heavy cross-core cache line contention. Verdict DROP: structural blocker (private tables
  multiply memory by worker count, while direct writes suffer cross-core cache line bouncing).
- c-72 inline table keys: storing name records directly in table slots to eliminate pointer indirection from bucket to arena
  record (commits cc17388 and 6c761ac). On our test machine, s1 had a bug causing 75x slowdown (T 2.38 / 2.33 s, K 3.54 / 3.62);
  s1b fixed it: official T 33.75 ms / fresh 34.03 ms vs baseline 30.96 / 31.20 ms (+9 % slower), M 14.18 / 14.38 MB, K 28.53.
  Verdict DROP: wider table entries increased cache footprint and L2 misses, running about 9 % slower without saving instructions.
- c-73 documentation and folding: local documentation pass (commits 3c59d97 and 1ffb91c) folding table rows from 52 to 18,
  recording step 51 board run run-90039a78bf5b, and updating test machine vs board comparisons. Documentation refactoring verified
  against board runs with check-perf-runs2.py. Verdict KEPT (documentation; folded into subsequent docs lift).
- c-74 SIMD and SWAR number parsing: parsing amounts and IDs in SIMD digit lanes and SWAR folds (commits 87aca31, caf095d,
  1070e80). On our test machine (box 1 vs base 61db8ce K 31.35 / 31.40): s1 scored K 30.70 / 30.64; s2 K 30.47 / 30.25;
  s3 K 30.39 / 30.24. T rose to 0.033 s (0.032 to 0.035 s) vs 0.031 s baseline. S increased by +41 to +62 lines. Callgrind
  instructions increased by +0.46 % to +2.46 %. Verdict DROP: deciding K fell 2.4 %; shorter dependency chains bought with more
  instructions ran slower on an instruction-bound CPU.
- c-75 structural parse with AVX2 separator masks: classifying 64-byte blocks with AVX2 comma/quote/LF masks and carry-less
  multiplication prefix-XOR for in-quote tracking (commits 48fe0ea and 8e33b36). On our test machine (box 1 vs base 61db8ce
  K 31.35 / 31.40): s1 scored official K 29.91 / fresh K 29.74 (T 32.9 to 33.5 ms, M 13.9 to 14.1 MB); s3 K 29.97 / 29.90.
  With names disabled (v3), parse time was 19.11 ms vs 19.72 ms baseline, but callgrind instructions rose +26 % (1,165 M to
  1,468 M) due to per-separator bit extraction (~13-15 instructions across ~6 separators per record). The full program was +1.7 ms
  slower. Verdict REFINE: deciding K fell 5.3 %, within refinement margin; refined into c-78 and c-76 to eliminate per-separator extraction.
- c-76 fused structural parse: draining AVX2 separator masks with tzcnt and blsr inside the record loop without storing an
  intermediate index (commit 44aff9c). On our test machine (box 2 vs base c205772 31.05 / 31.10 ms): s1 measured official
  T med 32.21 ms / fresh 32.30 ms (+3.7 / +3.9 % T). Names-off parse cycles fell by -9 % (453.6 M vs 498.2 M) despite instructions
  rising +48 % (909.9 M vs 615.5 M, +89 instr/record). In the full program, T was +3.8 % slower. Verdict REFINE: gap was -2.9 to
  -3.0 % K, within the ~11 % refinement margin; refined into c-86 to cut the extra instructions. Round 3 (commit 226f24d)
  ported the family with every refinement that had measured (c-86's splats and top-aligned folds, the pclmulqdq target feature)
  onto the c-84 r1 code: on our test machine (box 2) K 34.06 against 36.20 for its base c67ae58 (-5.9 %), +112 instructions per
  record. Its cut step found nothing left to remove (two variants measured +11 and +3 instructions per record). Verdict DROP after
  three rounds: the cost is instructions on cores whose second hyperthread runs the names work, and no change we could name
  brings it back to the base.
- c-77 research fleet: a 5-lane research sweep (whole architecture, names, parse, framing, web) comparing candidate mechanisms
  against logged measurements. Surveyed published 1BRC and simdcsv techniques, instruction-level trade-offs, and contest rules.
  Identified that contest rule 2.2 guaranteed well-formed records across all holdouts, making whole-file rerun and pairwise end
  check legal to drop (leading to c-83 s1). Also surfaced counting names at merge (c-80), single 16-byte message hashing (c-81),
  and tail exit via _exit (c-82). Verdict: KEPT AS BASIS for c-80, c-81, c-82, and c-83.
- c-78 backward parse: parsing region and amount backward from the record terminator (commits 2993070 and 265403d). Forward
  pass for id and name, one SIMD pass over the note to the first LF outside quotes, then read region and amount backward from
  the terminator using a 32-byte comma mask. On our test machine (box 1 vs base 61db8ce 31.35 / 31.34 ms): s1 T med 33.31 /
  33.34 ms (+6.3 % slower), M 14.31 / 14.05 MB; K est ~30.4 vs 31.40 (-3 %). Callgrind instructions rose +8.4 % (1,263 M vs
  1,165 M). Verdict DROP: backward scanning added instructions and branches compared to forward parsing; superseded by c-76
  and later c-89/c-92.
- c-79 transparent huge pages disable: disabling transparent huge pages via prctl(PR_SET_THP_DISABLE) at program start
  (commit 16394d9). Tested whether disabling THP reduced anonymous memory peak by avoiding 2 MB page rounding. On our test
  machine (box 2 vs base c205772 30.96 / 31.20 ms, M 14.54 / 14.20 MB): s1 T med 31.08 / 31.15 ms; M max 14.18 / 14.38 MB
  (-0.36 / +0.18 MB, inside run-to-run spread). Verdict DROP: memory did not drop by the required 0.5 MB; our memory was already
  compact and not backed by 2 MB huge pages.
- c-80 distinct names counted at merge: counting distinct names per group from owners' tables at the end instead of updating
  a counter on insertion during the run (commits 2b9fad3, f27153f, 11b28b6). On our test machine (box 2 vs base c205772 30.96 /
  31.20 ms): s1 T med 37.62 / 37.64 ms (+21 % slower), M 14.38 / 14.30 MB, S 669 (+21). Aligning Owner to 128 bytes (c-80b)
  measured T 37.46 / 37.42 ms, refuting cache line false sharing. Computing counts before acquiring the totals mutex (c-80c)
  removed serialization: K 30.92 / 30.99, T 31.88 / 31.80 ms, M 14.3 MB, S 676. Verdict DROP: walking ~260,000 bits at the end
  added about 0.7 ms to the critical path tail, whereas the original counter increment was hidden behind memory latency during
  filing; nothing heavy belongs in the end-of-pass merge.
- c-81 single 16-byte message format (step 52; board runs run-dd199ba14e7e to run-ec016d38390e, steps 52 to 57): one message
  format for every name (commit a238a06). Names up to 16 ASCII bytes stay raw bytes for exact comparison; longer names are keyed
  by 112 bits of a two-lane multiply hash in the short table, removing the medium table, exact HashMap, and two-slot messages
  (collision probability ~1e-26 per full file). On our test machine (box 2 vs c205772): T med official 29.15 / fresh 29.30 ms vs
  30.96 / 31.20 ms (-5.8 / -6.1 %); M max 14.04 / 13.95 MB vs 14.54 / 14.20 MB; S 608 (S-rs 601 vs 648); K est 32.98 official /
  32.94 fresh vs 31.42 / 31.46 (+4.7 % K). Board step 52: T 0.03 s, M 14 MB, 75.2 points. Verdict KEEP.
- c-82 tail exit and chunk claim reduction: reducing tail latency via immediate _exit and shrinking chunk claims at the end
  (commits 287fef3 and 1508df2). Step s1 called _exit immediately after writing output to skip libc teardown; s2 shrank chunk
  claims to a quarter of remaining bytes (32 to 256 KiB) so workers finish concurrently. On our test machine (box 2 vs c205772
  30.96 / 31.20 ms): s1 K 31.39 (T 31.09 / 31.04 ms); s2 K 31.23 (T 31.05 / 31.11 ms, S 675) vs baseline K 31.42. Verdict DROP:
  T was flat across both steps, inside spread, and S rose by +20 lines; chunk imbalance at the end was negligible and process::exit
  already skipped most teardown.
- c-83 compact rewrite (in step 58; board run run-9633f0eb49ea): compact rewrite of track C removing unused guards and simplifying
  modules; its own effect was measured by test-machine A/B. Step s1 (commit db9795c) relied on spec 2.2 guarantees, dropping whole-file
  rerun, pairwise end check, and long-number path (S 608 to 575); but moving BOM check to a branchless add introduced loop-carried
  latency (T med 30.10 / 29.97 ms vs 29.15 / 29.30 ms). Step s1b (commit 2f2b420) moved BOM check behind a predicted cold branch,
  restoring T med to 29.16 / 29.25 ms (S 581, K 33.36 / 33.33). Step s2 (commit f0c9019) compacted main and regions, packing
  country codes in byte order (S 541, T 29.21 / 29.24 ms). Step s3 (commit 4b82c69) compacted pass2, destructuring send in place
  (S 518, T 29.13 / 29.22 ms, K 34.02). Step s4 (commit db7d86a) compacted names, sizing owner index to pool capacity with single-line
  CRC/home/Table (S 488, T 29.15 / 29.12 ms, K 34.40). Steps s1b, s2, s3, and s4 reduced S from 608 to 488 while maintaining T at
  29.1 to 29.2 ms, raising K from 32.94 to 34.40. Verdict KEEP s1b, s2, s3, s4.
- c-84 table entry sizing and record pre-allocation (in step 58; board run run-9633f0eb49ea): memory optimizations for region and
  name tables; its own effect was measured by test-machine A/B. Step s0 (commit 5e2b553) corrected table sizing comments (region table
  is ~1 MB, not ~10 MB). Step s1 tested 4-byte table entries (16-bit id + 16-bit group) in 8-entry buckets, but lost T by +0.7 ms (+2.3 %)
  on our test machine because the 64-byte bucket cache line footprint did not change. Step s2 (commit 41180e4) pre-sized name records
  directly to the first index's capacity to avoid doubling reallocation copies. Variant c-84b (commit a921942) retained 8-byte entries
  with records sized to 6 per bucket. On our test machine, s0+s2 reduced M from 12.0 MB to 10.7 MB (-1.3 MB) while keeping T at 0.029 s,
  raising K to 34.78 (official 34.98 / fresh 34.78). When combined in c-90d, M reached 10.74 MB. Verdict DROP s1; KEEP s0, s2, and c-84b.
  Round r1 s1 (commit b832419) reworked s1: 4-byte index entries whose tag width is set from the index size, so ids take only the
  bits they need; measured inside c-93 (below).
- c-85 record batching: batching 8 records to process number parsing and region lookups in parallel SIMD lanes (commit daa3cb2).
  Buffered 8 parsed records and executed parallel SIMD folds and memory lookups across the batch. On our test machine (box 2 vs base
  db9795c): parse instructions rose +9.9 % (581.5 M to 639.0 M), cycles +11.5 % (523.6 M to 584.1 M), and full wall time rose +10.4 %
  (31.15 to 34.40 ms); S increased by +73 lines. A split probe showed stores rose from 17.8 M to 61.0 M (+13 stores/record) and store-forwarding
  stalls rose from 209 to 1.26 M, completely wiping out the arithmetic savings. Batching region lookups cut region table cycles from 58 M
  to 30 M (~8 cycles/record memory-level parallelism). Verdict DROP: buffering and store-forwarding overhead exceeded fold savings; the
  region lookup memory-level parallelism was kept as a finding for c-87.
- c-86 structural parse instruction reduction: reducing instruction overhead in the fused structural parse, tested via independent
  implementations c-86a (Gemini 3.8 Flash, commit 6ed0a27) and GLM (commit d955396). Combined comma and LF into one movemask, folded on
  top-aligned digits without shifts, and reduced macro splats. On our test machine (box 2 vs base c205772 32.36 ms): names-off parse
  cycles fell -10.7 % (444.2 M vs 497.2 M), but full wall time was 33.21 ms (+2.6 % slower). Profile revealed pclmulqdq was being called
  out-of-line because x86-64-v3 lacks the target feature in rustc, causing vector spills, and record hand-off inside the parse closure
  cost ~80 M extra cycles (~24 cycles/record). Verdict REFINE: gap was +2.6 % T, within refinement limits; led to c-87 matched prefetch
  pair and PCLMUL attribute probe.
- c-87 region-table prefetch pair (in step 58; board run run-9633f0eb49ea): prefetching the region-table byte and applying each record
  one record later; its own effect was measured by test-machine A/B. Overlapped region table memory latency with the next record's parse.
  Built as a matched pair: on incumbent f0c9019 (exp/c-87-inc, commit a73769e) and on structural parse 6ed0a27 (exp/c-87-struct, commit
  b9e4482). On our test machine (box 2 interleaved benchmarks): on the incumbent f0c9019, region prefetch cut full wall time by -4.3 % to
  -4.6 % (30.43 ms to 29.08 ms, -1.35 ms; K 33.56 to 34.13). On the structural parse, prefetch also reduced cycles, but structural wall
  time remained slower than the incumbent (31.74 ms vs 29.08 ms). Verdict KEEP on the incumbent branch (became c-88 and part of c-90d);
  the C-STRUCT family was retired as the incumbent with region prefetch was strictly faster.
- c-88 unified record apply with prefetch (in step 58; board run run-9633f0eb49ea): porting c-87 region prefetch onto c-83 s4 (commit db7d86a)
  with the record application written once as a closure (commit 4f0c7d1); its own effect was measured by test-machine A/B. Combined c-83 s4's
  compact names table (S 488) with 1-record-deferred region prefetch, using a single closure for both the main loop and loop tail. On our test
  machine (box 2): K 34.13 (lower: fresh 34.13, official 34.31, T 0.029 s, 0.0286 to 0.0292 s), M 12.0 MB, S 568 (scc lines), output hash
  matched 5/5 + 5/5. Verdict KEEP (formed the code base for c-89 and c-90d).
- c-89 parse quote mask and search unification (in step 58; board run run-9633f0eb49ea): parse optimizations for quotes and search loops;
  its own effect was measured by test-machine A/B. Round 0 (commit b4bf8f1) tested escape from quote mask bit t+1 with byte at i+32 loaded
  beside the window (T 29.21 ms vs 29.13 ms, K 33.95 vs 34.13, noise/loss). Round 1 s1 (commit b847ec8) unified the find() function to return
  both match index and next-byte-escaped flag for both name and note text paths (-7 lines). On our test machine (box 2): r1 s1 cut S by 7 lines
  with zero instruction overhead; combined in c-90d with c-84 s0/s2, measuring K 35.55 (official 35.59 / fresh 35.55), T 0.028 s, M 10.74 MB,
  S 571. Verdict KEEP r1 s1 (carried into c-90d and board step 58); r0 and r1 s2 dropped.
- c-90 composed stack (step 58; board run run-9633f0eb49ea): the composed stack of all validated improvements for the live board submission
  (c-90d, commit a921942, pushed as b1438ef). Combined c-83 line cuts (s1b, s2, s3, s4), c-87/c-88 1-record region prefetch, c-89 r1 s1 unified
  search with escape flag, and c-84 s0/s2/c-84b memory-sized tables (8-byte entries, records pre-sized to 6 per bucket). On our test machine
  (box 2): K 35.55 (official 35.59 / fresh 35.55), T 0.028 s (0.0280 to 0.0284 s), M 10.74 / 10.66 MB, S 571 (reduced from 655). Output hash
  matched 5/5 + 5/5. Board step 58: T 0.03 s, M 11 MB, 77.4 points. Verdict KEEP.
- c-91 line cuts and mailbox privatization: code cleanup and mailbox architecture refinements on top of c-90d (commits 1247e85, 55c73c5,
  de86736, 4d4a790, ff50bf0; none lifted yet). Step s1 compacted arguments, map_padded assert, and money formatting (-13 lines). Step s2 tested madvise(MADV_DONTNEED)
  on mapped input (+1.7 ms slower on our test machine, dropped). Step s3 removed mailbox LAG backlog handling in pass2.rs (-12 lines); output
  stress tests stayed correct without it. Step s4 privatized Owner inside Worker, eliminating Mutex<Owner>. Step s5doc
  documented the measured mailbox mechanism. On our test machine (box 2): K 35.63 (official T 0.028 s, fresh K 36.15, M 11.0 MB, S 559) and
  commit 4d4a790 K 36.10 (M 10.6 MB), both >= banked 35.55 with S reduced by 12 lines (571 to 559), but the same code read 35.63 and 36.10, so the gain sits inside our
  test machine's spread. Memory tail, 10 interleaved batches of 5 runs on the board's run path: banked a921942 never above 11.12 MiB in
  50 runs; 4d4a790 at 10.0-10.8 MiB except one run at 14.25 MiB. Without the LAG path a stalled owner's mailbox grows without bound, and
  the board scores the highest of five runs, so about one board run in ten would lose ~5.7 % K. Verdict DROP s3 and s4; s1 lifted in c-93
  (below, as a147d16). A second line-cut round on cold code only (c-91 r2, base c67ae58) committed nothing within its hour.
- c-92 single-window record parse: single 96-byte window parse per record, retaining the incumbent loop structure and pipeline while replacing
  sequential field scanning with SIMD masks and pclmulqdq prefix-XOR. Round 0 (commit d5cbea2) used a 96-byte window with quote/comma/LF
  masks; on our test machine (box 2), full wall median was 31.25 ms vs baseline a921942 29.09 ms (+7.4 % slower), though parse cycles fell -7.5 %
  (431 M vs 466 M). Round 1 (commit 1fe16c6) inlined the apply closure, reducing wall median to 30.35 ms (+4.0 % vs baseline). Round 2 (commit ab908f3)
  used a 64-byte window at the name with 64-bit masks and tzcnt field extraction; instructions dropped by 229 M vs r1p, but putting the next record
  start back on the dependency chain cost r1p's cycle gains (wall median 29.61 ms, +1.8 % vs baseline 29.09 ms, sd-bench K 34.60 official / 34.74 fresh,
  T 0.028 s, M 10.5 MB, S 680). Round 3 planned decoupled next-record start calculation (t + tzcnt(LF mask) + 1) and specialized fast paths, but was
  paused by quota limits. Verdict REFINE across rounds 0 to 2 (gap narrowed from +7.4 % to +4.0 % to +1.8 %, but did not surpass incumbent c-90d/c-91
  within the contest window; kept as a refined candidate). Round 3 (commits bfe1bba and d312c57) took the next record's start
  from the LF after the note, off the region and amount chain: full wall -1.6 % against the base in 10 of 10 interleaved reads,
  but the window parse's added lines kept it from shipping after one trim round. Verdict DROP for the window parse; its tail scan
  alone is grafted onto the current parser as c-94.
- c-93 lift (commit b832419 = c-90d's a921942 + c-91 s1 + c-84 r1 s1): on our test machine (box 2), 5 alternating rounds against
  a921942, official and fresh seed each, the lower reading per round: K median 36.24 (36.13 to 36.39) against 35.73 (35.42 to 35.80),
  +1.4 %, the two ranges apart; S 571 to 585. Memory over 150 interleaved runs each on the board's run path: highest 10.44 MiB
  against 11.91, median 9.43 against 10.19. Verdict KEEP (lifted; its board row comes with the next push).
- Worker count: evaluated thread scaling under CPU affinity pinning. Unpinned, 20 workers appeared faster than 12 (24.5 vs 33.6 ms). Under scoring CPU
  pinning (cpus 0-11, 6 physical cores with 2 hyperthreads each), 12 workers was fastest on our test machine: 31.5 to 32.7 ms, against 32.8 ms with
  14 workers, 34.9 ms with 16 workers, 33.8 ms with 24 workers, and 35.5 ms with 10 workers. Verdict: 12 workers kept (SD_WORKERS default).

## Decisions

- **Input: the files are mapped (mmap), never copied** (KNOWN). The host pre-warms /data, so mapped pages cost the
  memory score nothing; a probe that read records.csv into memory instead used about forty times the memory. Each
  mapping is followed by 96 guard bytes so the parse needs no bounds checks (`main.rs` `map_padded`; step 46).
- **One pass, records cut by the LF + digit/BOM start rule** (OURS). By the grammar (c-join.md:21, 36) every record
  starts with a digit or a BOM after a terminator; the generator never puts an LF followed by a digit inside a quoted
  field (c-join.md:50-51), and a wrong cut is caught by the pairwise end check and rerun on one worker, so the answer
  never depends on that rule. Spec 2.2 guarantees allowed dropping the whole-file rerun and pairwise checks in the
  compact rewrite (c-83 s1b). Rejected: a quote-parity pre-pass over every byte (floor12), and a SIMD carry-less
  multiply scan (the scan was a small share of the time when measured).
- **Distinct names: the owner design** (OURS, c-52). Each name goes to the worker that owns its hash, which keeps it in
  a private table with one bit per country. No two workers write a table at the same time, and no atomic operation
  touches a name. It replaced a shared name table with lock-free hits and owner-written bitsets (steps 4 to 45), and
  the owner design read 6.5 % higher in K on our test machine (c-52). Rejected: bitsets set by an atomic OR from any worker (c-40: cache lines
  bounce between cores, 58.2 ms against 31.5 ms), per-worker fingerprint lists sorted at the end (floor12: memory grows
  with the record count), and a prefetch ring for the shared-table lookups (c-8: no gain on our test machine).
- **Short names are compared exactly; every other name by a 112-bit hash** (c-81). The crc32c hash only picks the
  owner and the bucket; a key counts as already seen only when its whole 16-byte key matches. A name of up to 16 ASCII
  bytes (89 % of seed 1's records) is its own key, so it is compared byte for byte as c-join.md:37 says. Any other name
  is keyed by 112 bits of a two-lane multiply hash: two distinct long names would count as one only if all 112 bits
  matched, about 1e-26 per full-size file. This is the one place the program relies on a hash for identity; it removed
  the medium table, the exact HashMap and the two-slot messages (S 655 -> 608) and read 4.8 % higher in K (c-81).
  Counting by the 32-bit crc32c alone was rejected: at that width collisions would be expected.
- **Region table prefetch** (OURS, c-87, c-88). The region-table byte is prefetched and each record is applied one record
  later, overlapping memory latency with the next record's parse. On our test machine, this cut full wall time by 4.3 % to
  4.6 % (c-87, c-88) without adding instructions.
- **12 workers** (OURS). The count is fixed, not derived from the input: `SD_WORKERS` exists only as a test override.
  Under the scoring run's cpus 0-11, 12 workers were the best of 10, 12, 14, 16 and 24 (log, "Worker count"); more
  workers looked faster only when the process was not pinned.
- **Built for x86-64-v3, not for the build machine's CPU** (KNOWN). `-C target-cpu=x86-64-v3` (`Dockerfile`) names the
  instruction set the code uses (AVX2, BMI2, SSE4.2); `target-cpu=native` would tie the binary to whichever CPU builds
  the image, which need not be the one that runs it. The C runtime is linked statically (c-16).

Final result: best board run run-9633f0eb49ea (step 58, push b1438ef): K 35.55 on our test machine, board T 0.03 s, M 11 MB, 77.4 points; our test machine on that code (a921942) K 35.55 (official 35.70, fresh 35.55, T 0.028 s, M 10.7 MB, S 571).
