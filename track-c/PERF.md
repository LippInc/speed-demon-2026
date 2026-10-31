# Track C: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | floor1: first correct build, one thread, input mapped | 1d0fa92 · Track C (run-605dbc1c67cf) | 0.86 | 10 | 42.6 |
| 2 | re-run of step 1 (another track's push) | dea7a01 · Track C (run-c4eb9098eeb1) | 0.93 | 10 | 41.8 |
| 3 | re-run of step 1 (another track's push) | 98a661e · Track C (run-a2be17d4d6ad) | 0.79 | 10 | 43.5 |
| 4 | E2: twelve workers, one shared name table, owner-written name bitsets | 5ecc03f · Track C (run-6f175e156c66) | 0.17 | 16 | 56.4 |
| 5 | re-run of step 4 (another track's push) | dbc91d1 · Track C (run-9dc01455a1b6) | 0.16 | 16 | 56.7 |
| 6 | re-run of step 4 (another track's push) | bdf0564 · Track C (run-abaf5b333b4c) | 0.17 | 15 | 56.5 |
| 7 | re-run of step 4 (another track's push) | 5263115 · Track C (run-ead27acd0328) | 0.17 | 16 | 56.6 |
| 8 | re-run of step 4 (another track's push) | 68a1f56 · Track C (run-b1890e0c1e2b) | 0.16 | 15 | 56.7 |
| 9 | c-6: name-table hits without a lock | 376e3cd · Track C (run-5cfdae8378cd) | 0.13 | 22 | 57.2 |
| 10 | re-run of step 9 (another track's push) | b80b71e · Track C (run-6b0aeec09c5b) | 0.13 | 22 | 57.0 |
| 11 | c-7: memory recovered at the same speed (8-byte slots, one lazily mapped arena) | 704a88e · Track C (run-96e1148bb559) | 0.14 | 16 | 57.5 |
| 12 | c-9: owner-bucketed round buffers | 442dbe7 · Track C (run-75fe344f3f7e) | 0.10 | 16 | 61.0 |
| 13 | re-run of step 12 (another track's push) | dc6ca65 · Track C (run-f7359dc6db71) | 0.10 | 16 | 60.8 |
| 14 | c-10 + c-9.1: AVX2 delimiter search, branch-free terminator, arena block fix | 90c864a · Track C (run-63963655e56f) | 0.09 | 16 | 62.3 |
| 15 | re-run of step 14 (another track's push) | 2e2ee13 · Track C (run-4ff8954aecb6) | 0.13 | 16 | 58.1 |
| 16 | c-11: word hash and word compare for names | 1e9607b · Track C (run-a11e7d85f631) | 0.08 | 16 | 63.7 |
| 17 | re-run of step 16 (another track's push) | 48c9f56 · Track C (run-e9c771d0a5cd) | 0.08 | 16 | 63.1 |

## Experiment log

Each experiment's numbers are added here once the live board has measured it and they can cite its run ID.

- floor1, commit 6111c8a: the first correct build, single-threaded (board rows 1 to 3 are this build).
- floor12, commit bd5253a: twelve workers over equal byte ranges, record starts found by a two-state quote-parity
  pre-pass, distinct names by per-worker fingerprint lists sorted and merged at the end. Kept as the parallel baseline;
  its fingerprint lists make memory grow with the record count.
- E2, commit 1e98c34 (board row 4, 5ecc03f · Track C): one intern table shared by all workers (mutex shards, exact byte
  compare on every hit) and per-country name bitsets written by their owning worker in bounded rounds. Slower than
  floor12 on the box but far smaller in memory, so a better score; kept.
- c-5 arena, commit 60e701c: intern hits compare against a per-shard copy of each distinct name instead of the input
  mapping. Verdict NOISE on the box score; merged into c-6, which needs the arena's stable storage.
- c-6 lock-free, commit ef1b494: intern hits take no lock (atomic slots published with Release, read with Acquire;
  names in append-only arena chunks); the shard lock is taken only to insert. Verdict KEEP on the box score (board rows 9 and 10,
  376e3cd · Track C and b80b71e · Track C).
- c-7 mem, commit 537bf48: memory recovered at the same speed: retired slot generations freed at the round barrier,
  8-byte slots naming records in a directory-addressed arena, and one lazily mapped arena with per-shard reservations.
  Verdict KEEP, box K 14.73 (board row 11, 704a88e · Track C).
- c-8 prefetch, commit 36e6eaf: a ring that prefetched each name's home slot and then its arena record some records
  ahead of the exact lookup. Verdict DROP, box K 14.61: T unchanged at 0.127, S 755 -> 819. Faster on the test VM, not
  on the box, where the intern state fits in L3.
- Probes P8 (never lifted, wrong output on purpose): nolookup (d3d89b7, a stand-in for the intern lookup) and noowner
  (7632abe, the round's owner step removed). Their box phase lines put the lookups at about a third of pass 2 and the
  owner step at about a fifth.
- c-9 owner, commit 829f91b: each worker files its round pairs into one bucket per owning worker, through a group
  table built once, so an owner reads only the buckets addressed to it. Verdict KEEP, box K 16.54 (board row 12,
  442dbe7 · Track C). The variant with
  twice the bucket capacity (538d5b6) scored box K 16.33 and was not shipped.
- c-9.1 fix, commit 7fa72a4 (correctness fix): the shared name arena allocated a block again after a reservation skipped
  several blocks (a very long name), losing the names already in it; the block is now allocated by its own directory
  pointer. Found by the correctness kit's long-name cases.
- c-10 scan, commit 5c161a3: text fields' commas and quotes found with a 32-byte AVX2 compare instead of a byte loop,
  and the CR-or-LF terminator skipped without a branch. Verdict KEEP, box K 17.97 (board row 14, 90c864a · Track C,
  with c-9.1). SWAR parsing of the digit runs was tried and dropped: exact, but no gain, as the numbers here are at
  most eight digits.
- c-11 lookup, commits 01326a0 and e843890: the name hash built from the name's first and last words with one 128-bit
  multiply, and names up to sixteen bytes compared as two words. Verdict KEEP, box K 19.01 (board row 16,
  1e9607b · Track C).
- c-12.1 slot address, commit 1ee7bd1: a name-table slot holds the record's address instead of an arena offset, so a hit
  skips the block arithmetic. Verdict NOISE, kept on the line (box K 19.17 and 19.15 against 19.01).
- e1 record starts, commit cbdf26e: each worker's range starts at the first LF followed by a digit or a BOM, found by a
  short search instead of a full quote-parity pre-pass; every worker checks that it stopped exactly on the next range's
  start, and a mismatch reruns the file on one worker. Verdict KEEP, box K 19.84 (board 83d39fc · Track C and
  e71baea · Track C).
- s1, s2, s2b folds (f133779, f5fefd1, 8765712): the same program in fewer code lines; s1 also links the C runtime
  statically. Box K 19.94, 20.39 and 20.73.
- Probes, never lifted: c-13 regions parsed on all workers (faster, but too many lines; parked); c-15 memory probes
  (smaller round buffers, a higher table load, one malloc arena, fewer workers, names compared against the mapping:
  each traded more speed than it saved in memory); c-18 time-budget probes (page faults cost little; the parse and the
  name lookups are the time; thirteen workers on twelve CPUs stretch the round design by about a third).

## Decisions

- **Input: the files are mapped (mmap), never copied** (KNOWN). The host pre-warms /data, so mapped pages cost the
  memory score nothing; a probe that read records.csv into memory instead used about forty times the memory (P2).
- **One pass, records cut by the LF + digit/BOM start rule** (OURS). By the grammar (c-join.md:21, 36) every record
  starts with a digit or a BOM after a terminator; the generator never puts an LF followed by a digit inside a quoted
  field (c-join.md:50-51), and a wrong cut is caught by the pairwise end check and rerun on one worker, so the answer
  never depends on that rule. Rejected: a quote-parity pre-pass over every byte (floor12), and a SIMD carry-less
  multiply scan (the scan was a small share of the time when measured).
- **Distinct names: one shared intern table and owner-written per-country bitsets** (OURS). Every worker interns into
  one lock-free-on-hit table (exact byte compare on every hit); each country's bitset is written by one worker only,
  from pairs handed to it in rounds. Rejected: per-worker fingerprint lists sorted at the end (floor12: memory grows
  with the record count), a prefetch ring for the lookups (c-8: no gain on the box), bitsets set by atomic OR from any
  worker (cache lines bounce between cores), and counting by hash alone (not exact).

Final result: box K 20.73 on 8765712 (sd-bench/C status); its board run is pending.
