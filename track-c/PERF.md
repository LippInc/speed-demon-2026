# Track C: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

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
| 34 | c-17: a range cut inside a quoted field ends early instead of aborting | 6ba03de · Track C (run-0d1a65d51b10) | 0.08 | 16 | 64.8 | OURS | 0.08 s, 2 runs | kept |
| 35 | re-run of step 34 (another push) | 8604c2c · Track C (run-c2311de2ab18) | 0.08 | 16 | 64.8 | OURS | 0.08 s, 2 runs | re-run |
| 36 | s2 + s2b: the same program in fewer lines (8765712); README and PERF.md | 67c71f1 · Track C (run-5b8cc04ed3d8) | 0.07 | 16 | 65.5 | KNOWN | 0.07-0.08 s, 4 runs | kept |
| 37 | re-run of step 36 (another push) | 6b8f071 · Track C (run-36d9a0094bd8) | 0.08 | 16 | 64.8 | KNOWN | 0.07-0.08 s, 4 runs | re-run |
| 38 | re-run of step 36 (another push) | da6f278 · Track C (run-c60b67f889bb) | 0.08 | 16 | 65.3 | KNOWN | 0.07-0.08 s, 4 runs | re-run |
| 39 | re-run of step 36 (another push) | c54220e · Track C (run-cc2a0b3ec2c9) | 0.08 | 16 | 64.9 | KNOWN | 0.07-0.08 s, 4 runs | re-run |
| 40 | c-19a + c-19a3 + c-20a: pass 2 hands out chunks to free workers, owner inboxes, retired name-table generations freed at chunk boundaries, one packed word per shard; README and PERF.md (c-24) | e2c4246 · Track C (run-7971e1f1642f) | 0.06 | 18 | 66.1 | OURS | 0.06-0.07 s, 3 runs | kept |
| 41 | re-run of step 40 (another push) | 5a559a8 · Track C (run-782829ac98a7) | 0.06 | 15 | 66.9 | OURS | 0.06-0.07 s, 3 runs | re-run |
| 42 | re-run of step 40 (another push) | 2fd5fd7 · Track C (run-f2fa240567f9) | 0.07 | 16 | 66.4 | OURS | 0.06-0.07 s, 3 runs | re-run |

Spread: the range of board T over every run of the same code; each board run is a single run. Box K below is the lower of the official and fresh-seed readings on box 2 (5 runs each), with T's min-max.

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
  from pairs handed to its inbox; no rounds, no barriers (c-19a), and retired table generations are freed by eras. Rejected: per-worker fingerprint lists sorted at the end (floor12: memory grows
  with the record count), a prefetch ring for the lookups (c-8: no gain on the box), bitsets set by atomic OR from any
  worker (cache lines bounce between cores), and counting by hash alone (not exact).

Final result: box K 23.68 on a13436c (official 23.70, fresh 23.68; T 0.055 s, M 15.0 MB, S 582); best board run so far: step 41, run-782829ac98a7, 66.9 points.
