# Track C: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 14.44 | 367 | 0 |
| 1 | push 1d0fa92eb092, scored ok | 1d0fa92 · Track C (run-605dbc1c67cf) | 0.86 | 10 | 42.6 |
| 2 | push dea7a01343cf, scored ok | dea7a01 · Track C (run-c4eb9098eeb1) | 0.93 | 10 | 41.8 |
| 3 | push 98a661eeaaa6, scored ok | 98a661e · Track C (run-a2be17d4d6ad) | 0.79 | 10 | 43.5 |
| 4 | push 5ecc03f78566, scored ok | 5ecc03f · Track C (run-6f175e156c66) | 0.17 | 16 | 56.4 |
| 5 | push dbc91d1aaa1a, scored ok | dbc91d1 · Track C (run-9dc01455a1b6) | 0.16 | 16 | 56.7 |
| 6 | push bdf05643a17c, scored ok | bdf0564 · Track C (run-abaf5b333b4c) | 0.17 | 15 | 56.5 |
| 7 | push 5263115c0f8b, scored ok | 5263115 · Track C (run-ead27acd0328) | 0.17 | 16 | 56.6 |
| 8 | push 68a1f5679a26, scored ok | 68a1f56 · Track C (run-b1890e0c1e2b) | 0.16 | 15 | 56.7 |
| 9 | push 376e3cdfa655, scored ok | 376e3cd · Track C (run-5cfdae8378cd) | 0.13 | 22 | 57.2 |
| 10 | push b80b71eaed3a, scored ok | b80b71e · Track C (run-6b0aeec09c5b) | 0.13 | 22 | 57.0 |
| 11 | push 704a88eb6e9f, scored ok | 704a88e · Track C (run-96e1148bb559) | 0.14 | 16 | 57.5 |
| 12 | push 442dbe7fbe59, scored ok | 442dbe7 · Track C (run-75fe344f3f7e) | 0.10 | 16 | 61.0 |
| 13 | push dc6ca650346c, scored ok | dc6ca65 · Track C (run-f7359dc6db71) | 0.10 | 16 | 60.8 |
| 14 | push 90c864ad6d4e, scored ok | 90c864a · Track C (run-63963655e56f) | 0.09 | 16 | 62.3 |
| 15 | push 2e2ee13a2258, scored ok | 2e2ee13 · Track C (run-4ff8954aecb6) | 0.13 | 16 | 58.1 |
| 16 | push 1e9607b023c2, scored ok | 1e9607b · Track C (run-a11e7d85f631) | 0.08 | 16 | 63.7 |
| 17 | push 48c9f564734d, scored ok | 48c9f56 · Track C (run-e9c771d0a5cd) | 0.08 | 16 | 63.1 |

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
  and the CR-or-LF terminator skipped without a branch. Verdict KEEP, box K 17.97. SWAR parsing of the digit runs was
  tried and dropped: exact, but no gain in two local A/Bs (plus seven and minus three percent of pass 2) for about
  fourteen lines, as the numbers here are at most eight digits.
- c-11 lookup, commits 01326a0 and e843890: the name hash built from the name's first and last words with one 128-bit
  multiply, and names up to sixteen bytes compared as two words. Verdict KEEP, box K 19.01 (board row 16,
  1e9607b · Track C).
