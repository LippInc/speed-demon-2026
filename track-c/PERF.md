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
  (step 48).

What did not work, with the measurements (details in the log):

- One shared name table with an atomic OR on its bits (c-40): 58.2 ms against 31.5 ms for the owner design at 12
  workers; cross-core snoop loads 3.03 M against 0.23 M per run, because each new bit pulls a cache line from
  another core.
- A two-stage structural parse (c-58): 37 % faster on one thread (114 against 181 ms) but only 5 % faster at 12 workers,
  because two hyperthreads share one core's execution ports and the method executes more instructions.
- Profile-guided optimisation (c-61): 1.0 to 1.2 ms slower in two A/B rounds.
- One malloc arena (c-65): 0.3 MB less memory, but T 0.031 to 0.033 s and K 30.85 to 29.87.

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
| 43 | c-22 + c-26: the name-table slot is prefetched as soon as the name is parsed; the same program in fewer lines (S 657 to 582); README and PERF.md (c-27) | 63c975d · Track C (run-f9682152b66d) | 0.06 | 16 | 67.6 | KNOWN | 1 run | kept |
| 44 | re-run of step 43 (another push) | c8760d4 · Track C (run-2771b128c250) | 0.06 | 15 | 68.0 | KNOWN | 0.058-0.060 s, 3 runs | re-run |
| 45 | re-run of step 43 (another push) | 19e5d85 · Track C (run-e40aeefdf850) | 0.06 | 15 | 68.2 | KNOWN | 0.058-0.060 s, 3 runs | re-run |
| 46 | c-35, c-37, c-39, c-42: a lean parse over a padded mapping (digits folded by SSE multiply-adds, the BOM test on a cold path), a name table of 4-entry buckets with one tag word, page-table entries dropped in parallel at the end; S 582 to 729 | af85844 · Track C (run-8d78dc657a67) | 0.04 | 14 | 71.2 | OURS | 0.041-0.042 s, 2 runs | kept |
| 47 | re-run of step 46 (another push) | 1be93e4 · Track C (run-a47a8fbd9b44) | 0.04 | 14 | 71.8 | OURS | 0.041-0.042 s, 2 runs | re-run |
| 48 | c-47, c-52, c-57: each name goes to the worker that owns its hash, which keeps it in a private table (no shared table, no atomics on names); the region table built by all workers; S 729 to 812 | 3cfa79a · Track C (run-653e9ee499e8) | 0.03 | 14 | 73.9 | KNOWN | 1 run | kept |

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

Entries are roughly in the order the work was done. A board run has a run ID in the table above; a reading labelled "test-machine" (K, T, M) was taken on our test machine and has none. Hashes such as 1d0fa92 in the table are the pushed commits the board measured.

- floor1, commit 6111c8a: the first correct build, single-threaded (steps 1 to 3 are this build).
- floor12, commit bd5253a: twelve workers over equal byte ranges, record starts found by a two-state quote-parity
  pre-pass, distinct names by per-worker fingerprint lists sorted and merged at the end. Kept as the parallel baseline;
  its fingerprint lists make memory grow with the record count.
- E2, commit 1e98c34 (step 4, 5ecc03f · Track C): one intern table shared by all workers (mutex shards, exact byte
  compare on every hit) and per-country name bitsets written by their owning worker in bounded rounds. Slower than
  floor12 on our test machine but far smaller in memory, so a better score; kept. It attacked the pass 2 name table.
- c-5 arena, commit 60e701c: intern hits compare against a per-shard copy of each distinct name instead of the input
  mapping. Verdict NOISE on the test-machine score; merged into c-6, which needs the arena's stable storage.
- c-6 lock-free, commit ef1b494: intern hits take no lock (atomic slots published with Release, read with Acquire;
  names in append-only arena chunks); the shard lock is taken only to insert. Verdict KEEP on the test-machine score (steps 9 and 10,
  376e3cd · Track C and b80b71e · Track C). It attacked the pass 2 name lookups.
- c-7 mem, commit 537bf48: memory recovered at the same speed: retired slot generations freed at the round barrier,
  8-byte slots naming records in a directory-addressed arena, and one lazily mapped arena with per-shard reservations.
  Verdict KEEP, test-machine K 14.73 (spread not recorded; step 11, 704a88e · Track C). It attacked the
  pass2+names phase's memory.
- c-8 prefetch, commit 36e6eaf: a ring that prefetched each name's home slot and then its arena record some records
  ahead of the exact lookup. Verdict DROP, test-machine K 14.61: T unchanged at 0.127, S 755 -> 819. Faster on the test VM, not
  on our test machine, where the intern state fits in L3.
- Probes P8 (never lifted, wrong output on purpose): nolookup (a stand-in for the intern lookup) and noowner
  (the round's owner step removed). Their test-machine phase lines put the lookups at about a third of pass 2 and the
  owner step at about a fifth.
- c-9 owner, commit 829f91b: each worker files its round pairs into one bucket per owning worker, through a group
  table built once, so an owner reads only the buckets addressed to it. Verdict KEEP, test-machine K 16.54 (T 0.101-0.105 s;
  step 13, 442dbe7 · Track C). It attacked the pass 2 owner step. The variant with
  twice the bucket capacity (538d5b6) scored test-machine K 16.33 and was not shipped.
- c-9.1 fix, commit 7fa72a4 (correctness fix): the shared name arena allocated a block again after a reservation skipped
  several blocks (a very long name), losing the names already in it; the block is now allocated by its own directory
  pointer. Found by the correctness kit's long-name cases.
- c-10 scan, commit 5c161a3: text fields' commas and quotes found with a 32-byte AVX2 compare instead of a byte loop,
  and the CR-or-LF terminator skipped without a branch. Verdict KEEP, test-machine K 17.97 (T 0.086-0.088 s; step 17,
  90c864a · Track C, with c-9.1). It attacked the parse. SWAR parsing of the digit runs was tried and dropped:
  exact, but no gain, as the numbers here are at most eight digits.
- c-11 lookup, commits 01326a0 and e843890: the name hash built from the name's first and last words with one 128-bit
  multiply, and names up to sixteen bytes compared as two words. Verdict KEEP, test-machine K 19.01 (T 0.077-0.077 s; board
  row 19, 1e9607b · Track C). It attacked the pass 2 name lookups.
- c-12.1 slot address, commit 1ee7bd1: a name-table slot holds the record's address instead of an arena offset, so a hit
  skips the block arithmetic. Verdict NOISE, kept on the line: test-machine K 19.17 (T 0.076-0.078 s) and 19.15 against
  19.01. It attacked the pass 2 name lookups.
- e1 record starts, commit cbdf26e: each worker's range starts at the first LF followed by a digit or a BOM, found by a
  short search instead of a full quote-parity pre-pass; every worker checks that it stopped exactly on the next range's
  start, and a mismatch reruns the file on one worker. Verdict KEEP, test-machine K 19.84 (T 0.071-0.073 s; steps 21 and 22, 83d39fc ·
  Track C and e71baea · Track C). It attacked pass 1's record-start scan.
- c-16 static C runtime, commit f133779 (cbdf26e + one Dockerfile line): `+crt-static` links the C runtime into the
  binary, so the process starts without the dynamic loader. Measured on our test machine: M -0.3 MB (15.0 against 15.3 MB), T
  and S unchanged; test-machine K 19.94 official / 19.98 fresh against cbdf26e's 19.89 / 19.84 (spread not recorded). Kept:
  folded into the board's s1 runs (rows 24 to 33).
- s1, s2, s2b folds (1c9ace2, c3c4c74, 8765712): the same program in fewer code lines. Test-machine K 19.97 (spread not
  recorded), 20.39 and 20.73 (T 0.071-0.072 s). Kept: s1 in steps 24 to 33, s2 + s2b in steps 36 and 37.
- c-17, commit 884c6d0: every read in the record loop is bounds-checked, so a range cut inside a quoted field ends
  early instead of aborting; the garbage record past the file's end is dropped, pass 2's exact check fails, and main
  reruns the file on one worker. It attacked the parse's cut-past-end case. Verdict KEEP (steps 34 and 35,
  6ba03de · Track C).
- Probes, never lifted: c-13 regions parsed on all workers (faster, but too many lines; parked); c-15 memory probes
  (smaller round buffers, a higher table load, one malloc arena, fewer workers, names compared against the mapping:
  each traded more speed than it saved in memory); c-18 time-budget probes (page faults cost little; the parse and the
  name lookups are the time; thirteen workers on twelve CPUs stretch the round design by about a third).

- c-19a pass 2 without rounds, commit cdec073: workers claim chunks from a shared counter and hand full buckets to
  each owner's inbox instead of meeting at two barriers per round. Alone it lost on the test-machine score (test-machine K 20.39 against
  s2b's 20.73, T 0.071-0.072 s: faster, but more memory), kept as the base of the next two. It attacked pass 2's round
  barriers. Shared bits set by atomic OR from any worker (no owners) were tried first and dropped: far slower on the
  test VM.
- c-19a2 smaller chunks and buckets, commit a6996c6: the inbox backlog scales with the chunk. Verdict KEEP, test-machine K
  20.86. It attacked pass 2's inbox backlog.
- c-19a3 retired intern generations freed at chunk boundaries by eras, commit a3023b6. Verdict KEEP, test-machine K 21.65
  (T 0.064-0.064 s). It attacked pass 2's retired intern generations. With
  thirteen workers on twelve CPUs this design loses about a fiftieth of its speed, where the round design lost about
  a third (a probe).
- c-20a packed slot word, commit 7cb92a6: a name lookup reaches its slot array from one atomic word (address plus
  size) instead of through the generation's header, one dependent load fewer per lookup. Verdict KEEP, test-machine K 21.95
  (T 0.061-0.062 s). It attacked the pass 2 name lookups.
- c-20b line-aligned arena records, commit 415717a: records that would straddle a cache line moved to the next line.
  Verdict FAILED, reverted: speed unchanged on our test machine, memory up on the fresh dataset.
- c-23p owner-local name sets, commit ff75859: each country's name set kept by its owner, keyed by country and
  name-hash pairs. Expected to help because only the owner would touch a country's set. Test-machine T 0.121 s and
  M 62.5 MB, against 0.061 s and 15.4 MB for the code of the time (K 11.75 official / 11.73 fresh, T 0.120-0.126 s,
  S 665). The build did not follow its spec, so the readings do not count as a fair test of the idea; c-23q below is the
  next try.
  Verdict VOID.
- c-23q owner-local name sets, commit 835b27e: one set entry per name with the country marks packed into it, instead
  of one bitset per country. Test-machine K 20.27 official / 20.33 fresh (T 0.058 s), M 24.1 MB, S 715. Verdict DROP.
- c-23r slot prefetch from the parse, commit 1a6b8bc: the parse's prefetch hook hashed the name and the lookup hashed
  it again. Test-machine K 22.60 official / 22.52 fresh (T 0.058-0.059 s), M 15.0 MB, S 650. Verdict SUPERSEDED by c-22, which
  hands the hash to the lookup instead of computing it twice.
- c-22 slot prefetch in the parse, commit de1a7fb: the parse calls a hook as soon as a record's name is parsed; it
  hashes the name and prefetches its slot, and the lookup runs once the rest of the record is parsed, so the slot's
  cache miss overlaps that work. Test-machine K 22.76 official / 22.96 fresh (T 0.057 s), M 15.2 MB, S 657, against the base
  7a0c756's 22.14 / 22.09 (T 0.061-0.062 s, M 15.1 MB). Verdict KEEP. The structural parse it replaced as this step's
  plan was not built: its unfinished version gave a wrong full-data hash on our test machine.
- c-25 structural parse, commit c74cefd: each 64-byte block classified once into quote, comma and LF masks with the
  quote parity carried as a prefix XOR, fields walked by bit position; the per-field AVX2 search went. Test-machine K 22.23
  official / 22.17 fresh (T 0.059 s), M 15.0 MB, S 712, against the base 6f01659's 22.93 / 22.85 (T 0.056-0.057 s,
  S 657): T 2-3 ms slower, S up. Verdict FAILED, reverted (52c0cf1).
- c-26 S cut, commit a13436c: the same program in fewer code lines (S 657 -> 582, rustfmt defaults): names.rs
  277 -> 241, pass2.rs 123 -> 97, records.rs 123 -> 118, regions.rs 52 -> 49, main.rs 74 -> 70, Dockerfile 8 -> 7.
  The slot array lost its wrapper struct and one-element generation list, the worker body became a closure outside
  the thread scope, a slot starts with min and max at the extremes (no first-amount branch), unreachable asserts and
  an always-true hook result went. Test-machine K 23.70 official / 23.68 fresh (T 0.054-0.055 s), M 15.0 MB, against c-22 tip
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
- c-55 (the code in this commit; no board run yet): cut S and M everywhere except the name path, one step at a time,
  keeping a step only when S or M fell and T stayed inside the spread. s1: two functions that mapped a file merged into
  one, the one-worker rerun written once, a record as a (key, groups) tuple, one helper inlined: test-machine K 30.65
  (official 30.72, fresh 30.65), M 13.7 MB, against 30.55. s2: the region lines scanned by a plain byte loop, the
  long-amount function folded into `amount`, the owner's multiply-shift written at its two send sites. Together:
  S 812 -> 769, test-machine K 30.85 (official 30.93, fresh 30.85; T 0.031 s), M 13.5 MB, output hash 5/5 + 5/5. The
  code of push 3cfa79a read K 30.46 (official 30.46, fresh 30.56), M 13.9 MB on the same machine. Verdict KEEP; it
  has no run ID until it is pushed to the board.
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
- Worker count. Unpinned, 20 workers looked faster than 12 (24.5 against 33.6 ms). The scoring run uses cpus 0-11, six
  physical cores with two hyperthreads each, and under that set 12 workers were best: 31.5 to 32.7 ms, against 32.8
  with 14, 34.9 with 16, 33.8 with 24 and 35.5 with 10 (test machine). Verdict: 12 stays (`main.rs`, `SD_WORKERS`
  default).

## Decisions

- **Input: the files are mapped (mmap), never copied** (KNOWN). The host pre-warms /data, so mapped pages cost the
  memory score nothing; a probe that read records.csv into memory instead used about forty times the memory. Each
  mapping is followed by 96 guard bytes so the parse needs no bounds checks (`main.rs` `map_padded`; step 46).
- **One pass, records cut by the LF + digit/BOM start rule** (OURS). By the grammar (c-join.md:21, 36) every record
  starts with a digit or a BOM after a terminator; the generator never puts an LF followed by a digit inside a quoted
  field (c-join.md:50-51), and a wrong cut is caught by the pairwise end check and rerun on one worker, so the answer
  never depends on that rule. Rejected: a quote-parity pre-pass over every byte (floor12), and a SIMD carry-less
  multiply scan (the scan was a small share of the time when measured).
- **Distinct names: the owner design** (OURS, c-52). Each name goes to the worker that owns its hash, which keeps it in
  a private table with one bit per country. No two workers write a table at the same time, and no atomic operation
  touches a name. It replaced a shared name table with lock-free hits and owner-written bitsets (steps 4 to 45), and
  the owner design read 6.5 % higher in K on our test machine (c-52). Rejected: bitsets set by an atomic OR from any worker (c-40: cache lines
  bounce between cores, 58.2 ms against 31.5 ms), per-worker fingerprint lists sorted at the end (floor12: memory grows
  with the record count), and a prefetch ring for the shared-table lookups (c-8: no gain on our test machine).
- **Names are compared exactly, never by hash alone.** The crc32c hash only picks the owner and the bucket; a name
  counts as already seen only when its whole key matches (two or four words, or the bytes in the mapping for the rare
  name that fits neither form). Counting by hash alone was rejected because it is not exact.
- **12 workers** (OURS). The count is fixed, not derived from the input: `SD_WORKERS` exists only as a test override.
  Under the scoring run's cpus 0-11, 12 workers were the best of 10, 12, 14, 16 and 24 (log, "Worker count"); more
  workers looked faster only when the process was not pinned.
- **Built for x86-64-v3, not for the build machine's CPU** (KNOWN). `-C target-cpu=x86-64-v3` (`Dockerfile`) names the
  instruction set the code uses (AVX2, BMI2, SSE4.2); `target-cpu=native` would tie the binary to whichever CPU builds
  the image, which need not be the one that runs it. The C runtime is linked statically (c-16).

Final result: best board run run-653e9ee499e8 (push 3cfa79a): K 30.04, T 0.0321 s, M 14.2 MB, S 812, 73.9 points; our test machine on this code K 30.85.
