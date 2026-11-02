# Track A: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

Terms used below: the board is the organisers' live leaderboard, one run per push; the box is our bench machine with the scoring machine's CPU, where each reading is 5 runs on seed 1's full data (official) and 5 on one fresh seed at full size (fresh), and K is the lower of the two; the VM is a 4-core cloud machine used for quick timings and counts, never for K; lifted means copied from our working repository into this one and pushed, so the board ran it.

| Step | Change | Run ID | T (ms) | M (MB) | Points | KNOWN/OURS | Spread (box T) | Kept/reverted |
|---|---|---|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32047.0 | 228.0 | 0 | - | - | - |
| 1 | a-1, the floor | f15421b · Track A (run-b0eb3a208c93) | 156.8 | 45.4 | 59.3 | KNOWN | - | kept, part replaced (a-4) |
| 2 | a-1, re-run (another track's push) | 1d0fa92 · Track A (run-c9b407a05502) | 153.0 | 45.7 | 59.5 | KNOWN | - | kept, part replaced (a-4) |
| 3 | a-3, parallel tail | dea7a01 · Track A (run-149e73f79cee) | 75.7 | 50.0 | 66.2 | OURS | - | kept, part replaced (a-29) |
| 4 | a-4, memory: 24-byte groups, chunked latencies | 98a661e · Track A (run-dc24f55d6210) | 64.0 | 30.6 | 69.3 | OURS | - | kept, part replaced (a-11, a-29) |
| 5 | a-4, re-run (another track's push) | 5ecc03f · Track A (run-4e5caeb3bdf1) | 64.5 | 30.9 | 69.1 | OURS | - | kept, part replaced (a-11, a-29) |
| 6 | a-5, one AVX2 load finds the endpoint and LF | dbc91d1 · Track A (run-94c3438d3e16) | 49.4 | 30.0 | 71.9 | KNOWN | - | kept |
| 7 | a-6, the latency's space by one 16-byte compare | bdf0564 · Track A (run-ad086f2812c9) | 49.3 | 30.1 | 71.9 | KNOWN | - | kept |
| 8 | a-6, re-run (another track's push) | 5263115 · Track A (run-1e8cd19cfb4e) | 49.6 | 29.8 | 71.9 | KNOWN | - | kept |
| 9 | a-10, two-level key and global endpoint rank | 68a1f56 · Track A (run-34e8fb84e6ea) | 43.5 | 30.1 | 72.8 | OURS | - | kept |
| 10 | a-10, re-run (another track's push) | 376e3cd · Track A (run-182a533acad8) | 45.0 | 29.5 | 72.6 | OURS | - | kept |
| 11 | a-10, re-run (another track's push) | b80b71e · Track A (run-3749ce76278d) | 43.3 | 29.2 | 73.0 | OURS | - | kept |
| 12 | a-10, re-run (another track's push) | 704a88e · Track A (run-8a546276765a) | 45.6 | 29.1 | 72.5 | OURS | - | kept |
| 13 | a-10, re-run (another track's push) | cac9dd1 · Track A (run-1d0d9fe3ae1a) | 43.3 | 29.2 | 73.0 | OURS | - | kept |
| 14 | a-10, re-run (another track's push) | 442dbe7 · Track A (run-9860100dcf4e) | 44.1 | 29.1 | 72.9 | OURS | - | kept |
| 15 | a-11 + a-12, 3-byte latency slots, page release | 948854a · Track A (run-024d9e23cb50) | 44.4 | 25.3 | 73.1 | OURS | - | kept, part replaced (a-29) |
| 16 | a-11 + a-12, re-run (another track's push) | dc6ca65 · Track A (run-cdd0c0f2c6d5) | 44.2 | 25.0 | 73.3 | OURS | - | kept, part replaced (a-29) |
| 17 | a-13, branch-free parse | 22b6620 · Track A (run-ef28cacbc64d) | 38.8 | 24.8 | 74.6 | KNOWN | - | kept |
| 18 | a-13, re-run (another track's push) | 90c864a · Track A (run-500a6311f2e5) | 38.4 | 24.8 | 74.7 | KNOWN | - | kept |
| 19 | a-13, re-run (another track's push) | 2e2ee13 · Track A (run-77b664eb8d78) | 39.3 | 24.9 | 74.5 | KNOWN | - | kept |
| 20 | a-13, re-run (another track's push) | 1e9607b · Track A (run-326f7e9db4bd) | 38.5 | 25.2 | 74.6 | KNOWN | - | kept |
| 21 | a-17, endpoint pool and one-step merge | 48c9f56 · Track A (run-79c3f2b20fb0) | 35.4 | 25.9 | 75.4 | OURS | - | kept |
| 22 | a-17, re-run (another track's push) | 83d39fc · Track A (run-3ed371d7efad) | 34.6 | 26.2 | 75.6 | OURS | - | kept |
| 23 | a-17, re-run (another track's push) | e71baea · Track A (run-5564519d32b9) | 35.4 | 26.3 | 75.3 | OURS | - | kept |
| 24 | a-21 + a-26, percentile select, formatter, static C runtime | c32fd6a · Track A (run-711ada7fc374) | 35.3 | 26.3 | 75.2 | KNOWN | - | kept, part replaced (a-24) |
| 25 | a-21 + a-26, re-run (another track's push) | 2979028 · Track A (run-11fd5f374201) | 33.6 | 26.0 | 75.8 | KNOWN | - | kept, part replaced (a-24) |
| 26 | a-21 + a-26, re-run (another track's push) | 0c42543 · Track A (run-9ee51f1d4dbe) | 34.5 | 26.5 | 75.4 | KNOWN | - | kept, part replaced (a-24) |
| 27 | a-21 + a-26, re-run (another track's push) | b581408 · Track A (run-6cc162793fff) | 34.0 | 25.8 | 75.7 | KNOWN | - | kept, part replaced (a-24) |
| 28 | a-21 + a-26, re-run (another track's push) | e2b846c · Track A (run-c2f4f3fac5e6) | 34.2 | 25.8 | 75.6 | KNOWN | - | kept, part replaced (a-24) |
| 29 | a-21 + a-26, re-run (another track's push) | 5d16aa8 · Track A (run-23f2ae0d407d) | 32.9 | 26.0 | 76.0 | KNOWN | - | kept, part replaced (a-24) |
| 30 | a-21 + a-26, re-run (another track's push) | dc1cab8 · Track A (run-2460ae819b84) | 32.6 | 25.9 | 76.1 | KNOWN | - | kept, part replaced (a-24) |
| 31 | a-21 + a-26, re-run (another track's push) | 5c78ab8 · Track A (run-ac92f2fd600e) | 34.7 | 25.9 | 75.4 | KNOWN | - | kept, part replaced (a-24) |
| 32 | a-24 + a-27 + a-29: the line budget, the static C runtime on it, per-hour 4-byte records and a counting-sort tail | 59a4bfa · Track A (run-c214f7a67975) | 35.0 | 24.1 | 77.7 | KNOWN + OURS | 0.029-0.031 s | kept |
| 33 | a-24 + a-27 + a-29, re-run (another track's push) | 8c4d4fe · Track A (run-0c2ed3d6fdec) | 29.9 | 23.2 | 79.5 | KNOWN + OURS | 0.029-0.031 s | kept |
| 34 | a-24 + a-27 + a-29, re-run (another track's push) | 9224ad7 · Track A (run-9be570fe6b6b) | 30.3 | 23.9 | 79.3 | KNOWN + OURS | 0.029-0.031 s | kept |
| 35 | a-24 + a-27 + a-29, re-run (another track's push) | 6ba03de · Track A (run-86b0c97eda14) | 37.3 | 23.6 | 77.1 | KNOWN + OURS | 0.029-0.031 s | kept |
| 36 | a-24 + a-27 + a-29, re-run (another track's push) | 8604c2c · Track A (run-4cb736114b5b) | 31.3 | 23.6 | 79.0 | KNOWN + OURS | 0.029-0.031 s | kept |
| 37 | a-24 + a-27 + a-29, re-run (another track's push) | 67c71f1 · Track A (run-20c545e1f44a) | 31.2 | 23.3 | 79.1 | KNOWN + OURS | 0.029-0.031 s | kept |

| 38 | a-24 + a-27 + a-29, re-run (root README push) | 6b8f071 · Track A (run-c1f2cb487fc6) | 28.9 | 23.7 | 79.8 | KNOWN + OURS | 0.029-0.031 s | kept |
| 39 | a-35: cold-code line cuts, S 367 → 356 (kit ce481c8) | da6f278 · Track A (run-136eb6e8fd39) | 28.6 | 23.5 | 80.1 | OURS | 0.029-0.032 s | kept |
| 40 | a-35, re-run (another track's push) | c54220e · Track A (run-0b9c36b64e7d) | 35.6 | 24.1 | 77.6 | OURS | 0.029-0.032 s | kept |
| 41 | a-35, re-run (another track's push) | e2c4246 · Track A (run-fee24d575fbd) | 32.2 | 23.4 | 78.8 | OURS | 0.029-0.032 s | kept |
| 42 | a-40: the scan cut to fewer instructions per line (kit ab0db27) | 5a559a8 · Track A (run-cf19ad557a30) | 41.0 | 23.6 | 75.8 | OURS | 0.027-0.028 s | kept |
| 43 | a-40, re-run (receipts push) | 2fd5fd7 · Track A (run-933eb4091b3a) | 28.7 | 23.5 | 79.7 | OURS | 0.027-0.028 s | kept |
| 44 | a-40, re-run (another track's push) | 63c975d · Track A (run-67d08e654e03) | 28.1 | 23.8 | 79.9 | OURS | 0.027-0.028 s | kept |
| 45 | a-45: PERF rows 38-44 and README at the tip (kit 9504c76), a-40's code | c8760d4 · Track A (run-c2e53f031834) | 29.1 | 23.6 | 79.6 | OURS | 0.027-0.028 s | kept |
| 46 | a-45, re-run (another track's push) | 19e5d85 · Track A (run-a43ca3b04d55) | 28.2 | 24.4 | 79.7 | OURS | 0.027-0.028 s | kept |
| 47 | a-45, re-run (another track's push) | af85844 · Track A (run-1356fbfcd982) | 0.03 | 24 | 79.1 | OURS | 0.027-0.028 s | kept |
| 48 | a-45, re-run (another track's push) | 1be93e4 · Track A (run-8e41486361d9) | 0.03 | 24 | 79.5 | OURS | 0.027-0.028 s | kept |
| 49 | a-45, re-run (another track's push) | 3cfa79a · Track A (run-d39e19fdac1c) | 0.03 | 24 | 79.8 | OURS | 0.027-0.028 s | kept |
| 50 | a-47d: the a-47d lift (sd-kit-a 5c47d22) | 178ceb2 · Track A (run-b79ba3882933) | 0.03 | 23 | 79.4 | OURS | 0.026-0.028 s | kept |
| 51 | a-47d, re-run (another track's push) | 7890539 · Track A (run-3a21f91ba00d) | 0.03 | 23 | 79.5 | OURS | 0.026-0.028 s | kept |
| 52 | a-47d, re-run (another track's push) | b769e68 · Track A (run-90473e218dbb) | 0.03 | 23 | 80.5 | OURS | 0.026-0.028 s | kept |
| 53 | a-47d, re-run (another track's push) | 29fe7fb · Track A (run-8291dc062062) | 0.03 | 23 | 79.9 | OURS | 0.026-0.028 s | kept |
| 54 | a-47d, re-run (another track's push) | 134b2b2 · Track A (run-59f3735ada49) | 0.03 | 24 | 79.6 | OURS | 0.026-0.028 s | kept |
| 55 | a-47d, re-run (another track's push) | 60b5493 · Track A (run-a1e8513b77c6) | 0.03 | 23 | 79.3 | OURS | 0.026-0.028 s | kept |
| 56 | a-47d, re-run (another track's push) | e550f50 · Track A (run-ca6d3c048273) | 0.03 | 23 | 77.6 | OURS | 0.026-0.028 s | kept |
| 57 | a-55: the a-55 lift (sd-kit-a a6b3205) | f245483 · Track A (run-8c41cebcaeef) | 0.03 | 21 | 80.4 | OURS | 0.023-0.025 s | kept |
| 58 | a-55, re-run (another track's push) | 585ce17 · Track A (run-2c31c9241d23) | 0.03 | 21 | 81.0 | OURS | 0.023-0.025 s | kept |
| 59 | a-55, re-run (another track's push) | b1438ef · Track A (run-de31ec2f2b8c) | 0.03 | 21 | 80.6 | OURS | 0.023-0.025 s | kept |
| 60 | a-55, re-run (another track's push) | 55a0bc1 · Track A (run-66dd7b069673) | 0.03 | 22 | 80.5 | OURS | 0.023-0.025 s | kept |
| 61 | a-55, re-run (another track's push) | bf07a3a · Track A (run-3bf9d051e3dc) | 0.03 | 20 | 80.2 | OURS | 0.023-0.025 s | kept |
| 62 | a-55, re-run (another track's push) | a188bdf · Track A (run-6648069094f7) | 0.03 | 21 | 80.4 | OURS | 0.023-0.025 s | kept |

## What each push carried

The board re-runs the same code on every push, so several rows can carry one experiment.

- Rows 1 and 2 carry a-1, the floor: Rust with the standard library only, the input mapped with mmap, one byte range
  per thread, a private open-addressing table per thread keyed by (hour, endpoint), and every latency kept in a
  per-group vector. One thread then merged the tables, sorted each group's latencies and formatted the output.
- Row 3 carries a-3: a-1's single pass with a parallel tail. Each thread sorts its own groups, then the hours are
  handed out to the threads, and each hour is merged, its percentiles selected and its lines formatted on one thread,
  written in output order.
- Rows 4 and 5 carry a-4: memory. 24-byte group entries, latencies in chained chunks inside per-(thread, hour) blocks
  instead of per-group vectors, and each hour's blocks freed as soon as that hour is formatted.
- Row 6 carries a-5: the scan. One 32-byte AVX2 load at byte 32 of each line finds the endpoint's `/` and, for most
  lines, the LF, in place of byte-by-byte searches.
- Rows 7 and 8 carry a-6: the space before the latency found with one 16-byte compare before the LF.
- Rows 9 to 14 carry a-10: a two-level key (each thread interns endpoints to a small id; each hour maps endpoint id
  to its group with an array index) and one global endpoint rank, so the output order needs only integer sorts.
- Rows 15 and 16 carry a-11 and a-12 together: latencies in 3-byte slots with an exact two-slot escape, 16-byte
  groups, glibc's mmap threshold pinned, and each scan range's page-table entries dropped after its scan.
- Rows 17 to 20 carry a-13: a branch-free parse. The latency's 1 to 10 digits converted with one 16-byte load
  and SIMD multiply-adds, the 5xx flag read from the status's first digit, and the hour prefix compared as two
  u64 words.
- Rows 21 to 23 carry a-17: the gap between the scan and the tail. Endpoint bytes copied into a small pool per thread,
  so nothing after the scan refaults the released input mapping, and the global endpoint merge takes one step per
  distinct endpoint instead of one per (thread, endpoint).
- Rows 24 to 31 carry a-21 and a-26 together: a-20's percentile select, a-19's two-digit formatter and the C runtime linked
  statically. Box K 32.85 and 32.75 (the lower of each reading); its one board run read K 31.93, inside the board's
  single-run spread of a-17's rows (T 0.035 s on the board against 0.033-0.034 s on the box). Rows 25 to 31 re-ran the same folder on other tracks' pushes: points 75.4 to 76.1, so the board's single-run spread on this code is about 1 point.

- Rows 32 to 37 carry a-29, on a-24's line budget with a-26's static C runtime (a-27): per-hour 4-byte records and a
  counting-sort tail. Row 32's T 0.035 s was a slow single run (K 35.80); row 33 re-ran the same folder at T 0.0299 s
  (K 38.98), against 0.030-0.031 s on the box. Rows 34 to 37 re-ran the same folder on other tracks' pushes.

- Row 38 re-ran a-29's folder on a root README push: T 28.9 ms, the best board run of a-29's code.
- Rows 39 to 41 carry a-35: cold-code line cuts (setup, range splitting, scratch buffers and output formatting in fewer lines), S 367 → 356 on the board, the hot loop unchanged. Row 39 is the track's best board run, K 39.97 at T 28.6 ms; rows 40 and 41 re-ran the same folder at T 35.6 and 32.2 ms, so board single runs of one code ranged 28.6 to 35.6 ms.
- Rows 42 to 44 carry a-40: the scan cut to fewer instructions per line (box T 0.027-0.028 s against a-35's 0.029-0.032 s), S 356 → 392 on the board. Row 42's T 41.0 ms was a slow single run; rows 43 and 44 re-ran the same folder at 28.7 and 28.1 ms.
- Rows 45 and 46 carry a-45: PERF rows 38-44 and README at the tip (kit 9504c76), a-40's code.
- Rows 47 to 49 re-ran a-45's folder (kit 9504c76, a-40's code) on other tracks' pushes: points 79.1 to 79.8 (run-8e41486361d9 on row 48 read T 0.0292 s, M 23.7 MB, K 38.95).
- Row 50 carries the a-47d lift (sd-kit-a 5c47d22): each hour's records reserved at 256 KB and its request and 5xx counters at 2,048 ids up front (a-47's code), S 396 on the board, points 79.4 (run-b79ba3882933, T 0.0296 s, M 23.1 MB, K 38.80).
- Rows 51 to 56 re-ran 5c47d22 on other tracks' pushes: points 77.6 to 80.5 (row 52 run-90473e218dbb points 80.5).

Experiments measured on our bench box only, with no board run (their box status is the evidence):

- a-2 (OURS), failed: a two-pass counting sort into one flat latency array. `K 14.63 | T 0.152s | M 47.4MB | S 405 | sha
  5/5+5/5 OK` against a-1's K 15.46: the second pass cost as much as the one-thread tail it removed.
- a-7 (KNOWN), failed, no code kept: batched prefetching of the table slots, groups and chunk writes (batch 4, 8 and 16).
  No variant beat a-6 on our test machine, so nothing was pushed for it.
- a-8 (KNOWN), failed, no code kept: a cheaper hash, a 16-byte endpoint prefix in each slot, and a 32-byte vector compare.
  None beat a-6; cachegrind placed the lookup's cost in branch mispredictions of the linear probe.
- a-9 (probe): a phase probe (stderr timing lines only), never lifted. It measured where a-6's time goes on the box.
- a-11 (OURS) and a-12 (OURS) also have board rows (15 and 16); their box readings were:
- a-11 (OURS): memory. Latencies in 3-byte slots (larger values as an exact two-slot escape), 16-byte groups, and glibc's
  mmap threshold pinned so freed tail buffers return to the kernel. `K 28.81 | T 0.046s | M 24.1MB | S 610 | sha
  5/5+5/5 OK` against a-10's K 28.54 (M 27.9MB).
- a-12 (OURS): the exit teardown. Each scan range's page-table entries are dropped after its scan. `K 29.00 | T 0.045s |
  M 24.1MB | S 625 | sha 5/5+5/5 OK`.
- a-13 (KNOWN): a branch-free parse. The latency's 1 to 10 digits converted with one 16-byte load and SIMD multiply-adds,
  the 5xx flag read from the status's first digit, and the hour prefix compared as two u64 words. `K 30.17 (fresh;
  official 30.86) | T 0.041s | M 24.1MB | S 641 | sha 5/5+5/5 OK`.
- a-14 (OURS), failed: four ranges per worker, each thread's own range first and the rest claimed from a shared counter.
  `K 28.24 (official; fresh 29.61) | T 0.043s | M 29.8MB | S 653 | sha 5/5+5/5 OK` against a-13's K 30.17-30.36
  (M 24.1MB): each thread's tables spanned more hours, so memory and the merge grew, and the balance did not win
  the time back.
- a-15 (OURS), NOISE, not lifted: the per-hour exact latency layout. A thread keeps at most three hours as chunk chains and copies older
  hours back to back into exact arrays; groups live in their hour's arena and are freed with it.
  `K 30.47 (official; fresh 30.66) | T 0.040s | M 22.7MB | S 696 | sha 5/5+5/5 OK`, and 30.45 on a second reading,
  against a-13's 30.17-30.36: inside the spread. It saved 1.4 MB for 55 more lines and ran twice as slow on a
  shuffled file, so a-13 stayed the base.
- a-16 (KNOWN), failed, no code kept: one 64-byte block load for both the LF and '/' masks, and a 16-wide tag-group probe
  (SwissTable style) for the endpoint table. On our test machine both raised instructions per line (651 -> 685 and
  671) and neither lowered the scan time.
  The box re-measured a-13's code on that branch's report commit: `K 30.50 (official; fresh 30.92) | M 24.1MB`.
- a-17 (KNOWN): the gap between the scan and the tail. Endpoint bytes copied into a small pool per thread, so nothing after
  the scan refaults the released input mapping, and the global endpoint merge takes one step per distinct endpoint
  instead of one per (thread, endpoint). `K 32.08 (official; fresh 32.24) | T 0.035s | M 25.3MB | S 651 | sha
  5/5+5/5 OK` against a-13's K 30.17-30.50 (T 0.039-0.041s).
- a-18 (OURS), failed (dropped): latency chunks that double from 3 to 24 slots instead of 3 then 15. On our test machine
  it lowered peak memory by 0.8 to 1.0 MB and the latency storage by 1.0 MB; the box read `K 31.40 (official; fresh
  31.58) | T 0.036s | M 26.0MB | S 655 | sha 5/5+5/5 OK`, below a-17's K 31.83-32.44 and above its M 24.9-25.3MB, so
  the test machine's 1 MB cut did not reach the box.
- a-20 (KNOWN): the tail's percentile select. Groups of up to 32 latencies are sorted; larger ones are selected in ascending
  rank order (p50 on all values, p95 right of it, p99 right of that), about 1.55n of select work against 2.9n.
  `K 32.36 (official; fresh 32.44) | T 0.035s | M 25.2MB | S 664 | sha 5/5+5/5 OK`, and K 32.21 official / 32.66 fresh
  on a second reading, against a-17's K 31.83-32.44: just above, inside the spread; merged into a-21.
- a-19 (KNOWN): the output number formatter, built by a spec-driven builder: two digits per step from a 200-byte table.
  Box K 32.25 official, inside a-17's K 31.83-32.44, and 31.48 fresh, below it; merged into a-21.
- a-21 (KNOWN): a-20's select and a-19's formatter together, boxed as one. `K 32.53 (fresh; official 32.58) | T 0.034s |
  M 25.1MB | S 680 | sha 5/5+5/5 OK`, and K 32.09 fresh / 32.92 official (T 0.033s, M 26.5MB) on a second reading,
  against a-17's K 31.83-32.44 (T 0.035s).
- a-22 (OURS), no gain on the box: the endpoint hit confirmed by four masked 32-byte compares instead of memcmp, and the
  LF found in two 32-byte windows per turn. On our test machine conditional mispredicts fell 2.59 -> 1.64 per line
  and the scan 6-7 %; the box read `K 31.85 (fresh; official 32.69) | T 0.033-0.035s | M 25.1MB | S 712` and
  `K 32.41 (official; fresh 32.51) | T 0.034s | M 25.3MB | S 712`, inside a-21's spread, with 32 more lines.
  a-22 was dropped: it gave no box gain, box lower K 31.85 / 32.41, and S +32.
- a-24 (OURS), the line budget: the same program in fewer counted lines (S 680 -> 401), with level T and M. The work was a
  layout rewrite after rustfmt, two fixes where the relayout had changed the hash and the probe loop, and where it held
  the probe table and sorted lists through the tail; then dropping the checks spec 2 rules out, and going back to the
  one-digit formatter. `K 35.06 (official; fresh 34.92) | T 0.034s | M 26.4MB | S 401 | sha 5/5+5/5 OK` on the last
  code commit, `K 34.89 (official; fresh 35.00) | T 0.034-0.035s | M 25.1MB | S 401` on the same code again (the docs
  commit), and `K 34.90 (official; fresh 35.39) | T 0.033-0.034s | M 25.5MB | S 413` on the one before, against
  a-21's K 32.09-32.58 (T 0.034s, M 25.1-26.5MB, S 680).
- a-23 (OURS), inside the spread, not kept: the tail's references placed by dense rank into per-rank linked lists instead of a
  comparison sort, built by a spec-driven builder. On a-21's code the box read K 32.19 (the lower; fresh 32.87, M
  26.9MB), inside a-21's K 32.09-32.58, so it was not merged.
- a-25 (OURS), failed (dropped): the worker count capped at 6, 8 and 10 threads in place of all available CPUs, built by a
  spec-driven builder. On a-21's code the box read K 30.16 (6 workers; T 0.042s, M 21.2MB), 29.30 (8; T 0.043-0.044s,
  M 22.8MB) and 31.44 (10; T 0.037s, M 23.8MB), against 12 workers' K 32.09-32.53 (T 0.033-0.034s): fewer workers
  saved 2-4 MB and cost 3-10 ms, so all available CPUs stayed.
- a-26 (KNOWN): the C runtime linked statically (`+crt-static` in the Dockerfile's RUSTFLAGS), so no dynamic loader runs before
  main, built by a spec-driven builder. On a-21's code the box read K 33.00 official / 32.85 (the lower) and 32.75 (the
  lower) on a second reading, against a-21's K 32.09-32.53.
- a-27 (KNOWN): a-26's Dockerfile change on a-24's code, built by a spec-driven builder. `K 35.01 (fresh; official 35.30) |
  T 0.034s | M 26.6MB | S 401 | sha 5/5+5/5 OK` against a-24's code at K 34.89-35.06. A second reading on its docs
  commit d80de57: official K 35.41, fresh K 35.45.
- a-28 (probe), probe (never lifted): five timed passes over the input, per thread, at 12 workers. Parse only 7.3-7.6 ms, plus the endpoint lookup 14.4-14.7, plus the records and counts 18.6-19.4, the whole scan 19.4-19.8: parse about 38 %, lookup 36 %, bookkeeping 26 %.
- a-29 (OURS), kept: per-hour 4-byte records (endpoint id and latency in one u32, a three-word escape for ids of 2047 or more and latencies of 2^21 or more) in place of per-group latency chains; the tail places each hour's latencies by endpoint rank with a counting sort. `K 38.29 (lower: fresh) | official K 38.55 T 0.030s (0.029-0.031) | fresh K 38.29 T 0.031s | M 23.1MB S 367`, against a-27's 35.01-35.41.
- a-30 (probe), probe (never lifted): the tail took about 9.5 ms of wall time; select and format were 57-78 thread-ms of it.
- a-31 (OURS), dropped: the endpoint confirm by length and four 8-byte key words kept per id, and a hash from two independent multiplies. T level (0.030-0.031 s), S +15: K 37.47-38.16 against a-29's 38.29-38.55. The lookup's hash and confirm are not what the scan waits on.
- a-32 (OURS), inside the spread, not kept: small groups ranked by counting with AVX2 compares in the percentile select. K 38.43 (official) / 38.52 (fresh), T 0.029-0.030 s, S +36: inside a-29's spread.
- a-33 (probe), probe (never lifted), on a-29's code: scan and join about 20 ms, global ranks 0.8 ms, the tail about 7.5 ms (CPU-bound and balanced across the 12 threads: placing 19-20, select 37-45, format 25-30 thread-ms), the last writes about 1.5 ms. T about 30 ms.
- a-34 (KNOWN), dropped (spec-built): a-34a found each 1 KiB's line ends from 64-byte LF bitmasks and walked the lines from that list, so no line's start waited on the previous line's LF search; a-34b kept each hour's request and 5xx counts side by side. a-34a `K 37.11 (lower: fresh) | official K 37.55 T 0.031s (0.030-0.032) | fresh K 37.11 T 0.031s (0.030-0.036) | M 23.2MB S 413`; a-34b K 36.63 (official) / 37.67 (fresh), S 411; against a-29's 38.29-38.55 at S 367. T level, 46 more lines: the LF search is not what the scan waits on either.
- a-35 (OURS), inside the spread (spec-built): cold-code line cuts, S 367 -> 350: setup, range splitting, scratch buffers and the output formatting rewritten into fewer lines (release_range's guard folded into the madvise call, line_ranges' zero case folded into the start expression, the tail's scratch and output lets combined), with no hot-loop change. a-35b put the names `tables` and `least` back, which a-35 had shortened only so rustfmt kept the lines unwrapped (S 350 -> 356). `K 38.56 (lower: official) | official K 38.56 T 0.031s | fresh K 39.03 T 0.030s | M 23.4MB S 350`; `K 38.14 (lower: official) | official K 38.14 T 0.031s | fresh K 38.65 T 0.030s | M 23.3MB S 356`, against a-29's 38.29-38.55: S 367 -> 356 with the same speed. Board rows 39 to 41.
- a-40 (probe), probe (never lifted), on a-36's code (ce481c8), exp/a-40-scan-probe 1b9b7de: a scan prototype cut to fewer instructions per line, timed against ce481c8's scan() in the same process (stderr per-thread passes old/new, alternating), with the prototype as the real run (same output). Prototype: one 64-byte window from byte 32 for the '/' and the LF; each endpoint's first two 32-byte chunks kept as its key, the length in its slot, a CRC32 hash, no byte compare call. Cachegrind, 1 worker: scan 230.7 -> 184.1 instructions per line (seed 1), 232.4 -> 185.7 (seed 4242); conditional mispredicts 6.87 M -> 3.43 M on seed 1. Below the step's 60-instruction bar, so no build commit; S 426 against 356, about +1.6 MB of keys. `K 18.68 | T 0.103s` (five passes plus the run).
- a-40 (OURS), kept: the scan cut to fewer instructions per line, built in four commits on exp/a-40-scan-ipl after the probe above read the prototype's scan 16.1 -> 13.1 ms per thread (official seed) and 15.8 -> 13.4 ms (fresh). a-40a, one 64-byte window at byte 32 for the '/' and the LF (scan 230.7 -> 218.2 instructions per line, seed 1): `K 38.60 (lower: official) | official K 38.60 T 0.030s | fresh K 38.65 T 0.030s | M 23.3MB S 371`. a-40b, each endpoint's first two 32-byte chunks as its key, a CRC32 hash, the length in the slot, no byte compare call (218.2 -> 189.1): `K 38.64 (lower: fresh) | official K 38.98 T 0.027s (0.027-0.029) | fresh K 38.64 T 0.028s (0.026-0.030) | M 24.6MB S 424`. a-40c, the same code in fewer counted lines: `K 39.16 (lower: fresh) | official K 39.39 T 0.028s (0.027-0.028) | fresh K 39.16 T 0.028s (0.026-0.029) | M 24.5MB S 392`. a-40d, the keys freed after the scan: `K 39.49 (lower: fresh) | official K 40.32 T 0.027s (0.027-0.028) | fresh K 39.49 T 0.028s (0.027-0.032) | M 23.6MB S 392`, and on a second reading (docs commit aa731cd) `K 39.66 (lower: official) | official K 39.66 T 0.028s | fresh K 39.99 T 0.027s | M 23.0MB S 392`, against ce481c8's 38.74-39.45 (T 0.029-0.030 s, M 23.4 MB, S 356). Board rows 42 to 44.
- a-47 (OURS), kept candidate (spec-built), on a-40d: each hour's records reserved at 1 << 16 entries (256 KB, above glibc's 128 KiB mmap threshold, so each buffer is mapped on its own and only the pages written are charged) and its request and 5xx counters at the 2,048 ids that ID_BITS allows, up front (568a639). `K 40.25 (lower: fresh) | official K 40.91 T 0.027s (0.026-0.028) | fresh K 40.25 T 0.027s (0.027-0.028) | M 22.7MB S 396 L | sha 5/5+5/5 OK`, against a-40d's four readings K 39.49-40.03 fresh / 39.66-40.42 official at M 23.0-23.6 MB.
- a-48 (OURS), failed (spec-built), two branches on a-40d. a-48r, without the per-thread release of the scanned range (91e0d76): `K 39.09 (lower: fresh) | official K 39.64 T 0.028s (0.028-0.030) | fresh K 39.09 T 0.029s (0.028-0.029) | M 23.6MB S 387 L | sha 5/5+5/5 OK`; T +1 ms: the release pays for itself at exit teardown (a-12). a-48p, a per-thread MADV_POPULATE_READ of its range before the scan (5ebcff3): `K 35.08 (lower: fresh) | official K 35.63 T 0.034s (0.034-0.035) | fresh K 35.08 T 0.036s (0.033-0.037) | M 23.1MB S 394 L | sha 5/5+5/5 OK`; T +7 ms: the populate call costs more than the page faults it saves.
- a-47d (OURS), banked board tip (sd-kit-a 5c47d22), a-47's code with PERF rows 45-46: each hour's records reserved at 256 KB and counters at 2,048 ids up front. Second box reading: `K 39.80 (lower: fresh) | official K 40.23 T 0.027s (0.027-0.028) | fresh K 39.80 T 0.028s (0.028-0.029) | M 22.4MB S 396 | sha OK`, and rerun `K 39.88 (fresh; official 41.05, T 0.026-0.028) | M 22.2MB`. Lifted to the board on row 50 (run-b79ba3882933, points 79.4).
- a-48 (OURS), failed (spec-built), two branches on a-40d. a-48r, without the per-thread release of the scanned range (91e0d76): `K 39.09 (lower: fresh) | official K 39.64 T 0.028s (0.028-0.030) | fresh K 39.09 T 0.029s (0.028-0.029) | M 23.6MB S 387 L | sha 5/5+5/5 OK`; T +1 ms: the release pays for itself at exit teardown (a-12). a-48p, a per-thread MADV_POPULATE_READ of its range before the scan (5ebcff3): `K 35.08 (lower: fresh) | official K 35.63 T 0.034s (0.034-0.035) | fresh K 35.08 T 0.036s (0.033-0.037) | M 23.1MB S 394 L | sha 5/5+5/5 OK`; T +7 ms: the populate call costs more than the page faults it saves.
- a-49 (OURS), failed (no board run): batched SIMD parse. Lines parsed in batches (64 lines, db8f945: `K 39.10 (lower: fresh) | official K 39.91 T 0.027-0.028s | S 436`; 8 lines, 9a33286: `K 39.53 (lower: fresh / official) | T 0.027s | S 436`). Scan instructions per line fell 188.1 -> 164.8 (parse 80.7 -> 43.8), but branch mispredicts were not cut. Step (a) edits saved as 303c7ad (a-49s): `K 38.87 (lower: fresh) | official K 39.17 T 0.028s (0.027-0.028) | fresh K 38.87 T 0.028s (0.027-0.030) | M 23.0MB S 438 | sha OK`, rerun K 38.49 (T 0.029 / 0.028). With a-47's reserved hour buffers (a-49m, 124d476): `K 38.98 (lower: fresh; official 39.14 T 0.028s, fresh T 0.028)` and second reading `K 38.52 (official 39.10 T 0.028s, fresh T 0.029s) | M 22.4-22.6MB | S 442`. T level with incumbent, S +46 (-2.0 % K): dropped.
- a-50 (OURS), failed (no board run): block stealing. 64 line-aligned blocks per thread, stealing from the thread with the most left (24d64df): `K 37.84 (lower: fresh) | official K 39.19 T 0.029s (0.028-0.030) | fresh K 37.84 T 0.030s (0.029-0.031) | M 22.5MB S 414 L | sha 5/5+5/5 OK`; per-block release calls (64 madvise per thread) and counter traffic cost more than the tail spread they removed. a-50r (c245be9), releasing once per run of adjacent blocks: `K 38.52 (lower: fresh) | official K 39.37 T 0.028s (0.027-0.029) | fresh K 38.52 T 0.029s (0.029-0.030) | M 22.6MB S 426`. a-50r8 (4a073b1), 8 blocks per thread: `K 39.14 (lower: fresh) | official K 39.77 T 0.027s (0.026-0.030) | fresh K 39.14 T 0.028s (0.028-0.030) | M 22.7MB S 426`, rerun lowers 39.14 / 39.41 (M 22.3MB). T was flat with a-47 (0.027-0.028 s), while S +30 cost 1.3 % K: dropped.
- a-51 (OURS), inside the spread (spec-built, no board run), on a-47: each hour's counters sized past every interned id, so the add skips its bounds check (9f3ab56; d55ca7d changes one comment only and is a second reading of the same code): `K 39.58 (lower: fresh) | official K 40.83 T 0.026s (0.026-0.028) | fresh K 39.58 T 0.028s (0.027-0.033) | M 22.4MB S 402 L | sha 5/5+5/5 OK` and `K 40.26 (lower: fresh) | official K 40.26 T 0.027s (0.025-0.028) | fresh K 40.26 T 0.027s (0.027-0.028) | M 22.5MB S 402 L | sha 5/5+5/5 OK`; no gain over a-47 for S +6 and an unsafe block.
- a-52 (OURS), failed (no board run): radix tail over hour records. Records radix-sorted by latency, percentiles read directly (1a07be7 on 5c47d22): `K 37.73 (lower: fresh) | official K 37.79 T 0.028s (0.026-0.031) | fresh K 37.73 T 0.028s (0.027-0.029) | M 26.3MB S 469 | sha OK`, second reading `K 37.52 (official T 0.028s, fresh 0.028s) | M 26.0MB`. Cachegrind split (1 worker, seed 1): base tail 278.3 M Ir vs a-52 tail 315.7 M Ir (tail +37 M Ir, +13 %; run +40 M Ir, +4.5 %). The two radix scatter passes added 121.5 M Ir to remove 84.1 M Ir of select work: dropped.
- a-53 (probe), phase probe (never lifted): memory probe on 5c47d22 (4cf68f9). A sampler thread read container cgroup memory.current every ~20 us. Peak 22.2-23.3 MB came at 20-28 ms at the tail start, not at the end: anon 17.0-19.7 MB, kernel 1.5 MB (page tables 0.71, slab 0.5, stacks 0.25), output shmem 0-2.4 MB. At scan join anon was 17.0-17.7 MB (records 12.4 MB + ~5 MB scan leftovers: pools 1.4 MB, counts/errors 1.4 MB, sorted lists 0.7 MB, heap slack). Tail start added +2.5-3.3 MB (12 hours in flight). End state: 16.7-17.7 MB (shmem 10.1-10.2 MB + anon 4.1-4.5 MB + kernel 1.3 MB).
- a-54 (OURS), inside spread, not kept (no board run): memory trim on 5c47d22 (214b8a0). Before the tail, one owned copy of each endpoint replaces the 12 padded pools, tables are dropped, and malloc_trim(0) returns heap slack. Two readings: `K 40.00 / 40.04 (lower: fresh; official 40.30 / 40.28) | T 0.028s | M 21.1 / 21.7MB | S 400 | sha OK`, against same-window 5c47d22 rerun `K 39.88 (fresh; official 41.05, T 0.026-0.028) | M 22.2MB`. M fell 0.5-1.1 MB, K inside spread. Mooted by a-55.
- a-55 (OURS), kept (no board run; measured on bench box): the pipeline. Threads claim 256 KiB blocks of /data/requests.log in file order and parse them into per-hour records; an hour is finished (counted, selected, formatted) as soon as the registered prefix of the file holds a line two hours later (LAG 2); finished hours are emitted in hour order. A line that arrives after its hour was claimed marks the run late, and main repeats the run with no early finishing (exact for any order). Mapping released in 4 MiB steps once all hours are claimed. Measured across rounds:
  - r1 (64e425d), first cut: `K 40.13 (lower: fresh) | official K 40.66 T 0.026s (0.025-0.027) | fresh K 40.13 T 0.026s (0.026-0.028) | M 21.1MB | S 514 | sha OK`. T about -1 ms, M -1.1..-1.6 MB vs 5c47d22 (M 22.2-22.7 MB), S +118. Probe 5719d0b showed all 12 threads busy (wait ~0.1 ms), first hour claimed 3.1-3.4 ms, scan end 21.3-21.9 ms, last write 25.0-25.6 ms; misses took 1.4-2.1 ms per thread under the sorted-insert lock.
  - r2a (91264d4), one malloc arena (mallopt M_ARENA_MAX 1): a block's record vectors are freed by the finishing thread; one shared arena lets freed chunks be reused by whichever thread allocates next instead of holding per-thread high-water marks. `K 40.78 / 40.63 (lower: fresh; official 41.02 / 41.28) | M 20.3-20.4MB | S 515`. Kept in family (+1.3-1.6 % K, M -0.7 MB). Effect: PERF.md a-55.
  - r2b (e16515d), id-only lock: the adding mutex covers only the id; finishing threads sort new ids into Dict::order. Removed the 1.4-2.1 ms per-thread serialization on table misses. `K 41.76 / 41.60 (lower: fresh; official 41.60 / 41.76) | T 0.024s | M 20.3-20.6MB | S 528`. Kept in family (+2.4 % K). Effect: PERF.md a-55. Perf split showed Table::find confirm took ~18 % of cycles due to Name stride 144 B with OnceLock state on a second cache line.
  - r2c (a69514f), Name repr(C, align(64)): `K 41.18 / 41.65 (lower: fresh; official 41.38 / 42.11) | T 0.024s | M 21.0-21.2MB | S 529`. Dropped: OnceLock state still sat after the aligned name, so confirm still loaded two cache lines while stride grew to 256 B (M +0.5-0.9 MB).
  - r2d (fdb2fa1), 128-byte name images: names stored as plain 128-byte aligned byte arrays published by atomic counter, length in byte 127, no OnceLock. Confirms of endpoints up to 64 bytes read exactly one cache line. `K 42.22 / 42.23 (lower: fresh; official 42.61 / 42.38) | T 0.023-0.024s | M 20.6MB | S 530`. Kept in family (+1.1-1.5 % K, min 42.22 > max 41.76). Effect: PERF.md a-55.
  - r2e (afc911c), in-place output formatting with fixed-size copies: `K 41.84 / 42.48 (lower: fresh; official 42.03 / 42.69) | T 0.023s | M 20.6-21.0MB | S 541`. Dropped: inside spread at S +11 lines; reverted to r2d (71473d6).
  - r3 (175024c), 4 interleaved line streams per block: `K 41.26 / 41.08 (lower: fresh; official 41.62 / 41.19) | T 0.024s | M 20.4-20.7MB | S 565`. Dropped. Split (perf stat): line loop is already throughput-bound on 1 worker (IPC 2.67), so overlapping chains cannot win; 4 streams added L1 misses (1 worker 5.79 -> 6.53 M, 12 workers 6.63 -> 7.49 M; cycles +1.6-1.8 %).
  - trim (a6b3205, on 739c1eb): cold-code line cuts (hot functions untouched), S 530 -> 463 (-67 lines): `K 42.54 / 43.04 (lower: fresh; official 42.82 / 43.05) | T 0.024s | M 20.2-20.4MB | S 463`. Kept in family, family tip: +5.7-8.1 % K over banked board tip 5c47d22 (39.80 / 40.25).
- a-56 (OURS), kept (no board run; measured on bench box): forward line walk (44579b2), after HighVelocity's public
  track-a line walk. The timestamp is always 27 bytes, so one 64-byte space mask from byte 28 holds the ends of method
  and endpoint; status and latency sit at fixed offsets after them; the latency's digits are read 8 per load and
  converted with two multiply-adds; one load checks the hour; the last 256 bytes take a cold copy path. Two readings:
  `K 43.58 (lower: fresh) | official K 44.64 T 0.022s (0.021-0.023) | fresh K 43.58 T 0.023s (0.022-0.024) | M 20.7MB S 464 L | sha 5/5+5/5 OK`
  and `K 43.80 (lower: fresh) | official K 43.89 T 0.023s (0.022-0.023) | fresh K 43.80 T 0.023s (0.023-0.024) | M 20.7MB S 464 L | sha 5/5+5/5 OK`,
  against a6b3205's `K 42.96 / 43.04 / 42.54 (lower: fresh)`; five-round A/B medians 43.93 vs 43.02 (+2.1 % K).
- a-57 (OURS + KNOWN), kept: the CPU cut, in three steps on a-56, each timed on the box with a fresh tmpfs /out per
  run (as the board writes), 12 workers. Final A/B, 20 interleaved rounds: a-56 median 23.37 ms (min 22.06), a-57
  19.09 ms (min 18.51); HighVelocity's public binary on the same box 16.11 ms. Hash checked at 1, 4, 12 and 24 workers.
  - finish without sort or select (OURS): records packed to 32-bit rows, latency << 11 | id, two stable counting
    passes by latency (11 and 10 bits; the 0.24 % of latencies >= 2^21 sorted aside), then a third pass deals them to
    each endpoint's span in byte order, so p50, p95 and p99 are one read each; branch-free `put` (all ten digits at
    once, leading zeros shifted out); finish and text buffers pooled. Branch misses 2.49 -> 0.95 M, 22.1 -> 20.8 ms.
  - lookup (KNOWN, after HighVelocity): CRC32 of the length and the first and last 8 bytes; the confirm compares raw
    32-byte loads masked to the length with `bzhi`, so no masked copy of the line; records written through a raw
    pointer. User instructions 860 -> 666 M, 20.8 -> 19.0 ms.
  - LF prescan (OURS): each 4 KiB chunk's LFs found first (64-byte windows, two unconditional stores each), so a line
    is read from both ends: the method's space from the front, the latency's space from the back; the file is mapped
    over a zero tail, so the last line needs no copy. One worker 133 -> 114 ms; 12 workers level with the step before
    (the run is throughput-bound there), kept for the single-worker cut.
  - cost: memory peak 20.0-20.6 -> 23.0-23.9 MB (cgroup peak, output 9.64 MB of it): the pooled finish and text
    buffers stay resident at the pools' high-water mark (5-6 finishers and 8 texts at once, per a memory log).
  - LOST, each against the kept step: the mapping's release per block (20.8 ms), none (21.8: exit teardown ~2.5 ms),
    after all hours (same), in 4 MiB steps during the scan (same): the unmap costs ~2 ms wherever it lands. Record
    vectors shrunk to fit (+1 MB, +1.6 ms). Exact-size and capped pools (-3.2 MB, but +1.7-2.3 ms: freeing or
    growing a buffer unmaps while the other threads release the input). At most 2 or 3 hours finishing at once
    (+7 MB: records wait for their hour).
- a-58 (OURS + KNOWN), kept: the finish by select, on a-57 (board: K 44.75, T 19.5 ms, M 24.1 MB, 82.54 points).
  Records counted per endpoint, latencies copied to each endpoint's span in byte order (top bit flipped, so they
  compare as i32), then one quickselect per span for all three ranks (KNOWN: vectorized partition, AVX2 compress by a
  permutation table; OURS: one row serves both stores, a side with no rank is never visited, spans of 16 or fewer
  sorted by a two-register bitonic network). Groups are small on seed 1: median 6 records, 1,762 per hour, 46 % of
  records in groups over 256. `put` writes numbers below 10^8 with one 8-byte store. The select was first drafted by
  GLM-5.3 and fixed by hand (a missing intrinsic, the clear lanes' table offsets, the two stores overlapping with 8
  values left, a wrong 4-lane merge step); its test (every n to 3,000, nine value sets, nearest-rank 50/95/99) failed
  on two planted bugs before it passed. Box, 12 workers, fresh tmpfs /out, 20 interleaved rounds:
  `a-57 med 20.26 ms (min 18.57) | a-58 first cut med 18.51 (min 16.95) | a-58 med 17.92 (min 16.57)`, and an earlier
  round `a-57 19.03 | first cut 18.38`. Memory peak 23.4-24.2 -> 20.4-21.4 MB (the span buffer replaces two row
  buffers). Hash checked at 1, 4, 12 and 24 workers. S 545 -> 659 (+114 lines).

## Decisions

The plan's core choices (KNOWN = the known-best technique's version, OURS = our own design), each held or traded with the measurement that decided it:

- 1, a two-pass counting sort into one flat latency array (OURS): traded back to one pass at a-2, which read K 14.63 against the floor's one pass. The counting sort came back in the tail, per hour, over one 4-byte record per line (a-29).
- 2, a private open-addressing endpoint table per thread with a full byte compare (KNOWN): held, as a two-level key with one global rank (a-10, a-17). Cheaper hashes and confirms read level or slower (a-8, a-22, a-31).
- 3, free the latencies before the output and never hold a full copy of `summary.txt` (OURS): held in aim, but the in-place mapped writer was never built. Each hour's records are freed as soon as they are placed, and its lines go out in output order as the hour finishes; a-55 pipeline finishes hours early behind a 2-hour watermark of the registered prefix (a-3, a-4, a-29, a-55).

Rejected:
a-2 (OURS): a two-pass counting sort into one flat latency array; K 14.63 against a-1's K 15.46: the second pass cost as much as the one-thread tail it removed.
a-7 (KNOWN): batched prefetching of the table slots, groups and chunk writes (batch 4, 8 and 16); no variant beat a-6 on our test machine, so nothing was pushed for it.
a-8 (KNOWN): a cheaper hash, a 16-byte endpoint prefix in each slot, and a 32-byte vector compare; none beat a-6; cachegrind placed the lookup's cost in branch mispredictions of the linear probe.
a-14 (OURS): four ranges per worker, each thread's own range first and the rest claimed from a shared counter; K 28.24 (official; fresh 29.61) against a-13's K 30.17-30.36 (M 24.1MB): each thread's tables spanned more hours, so memory and the merge grew, and the balance did not win the time back.
a-15 (OURS): the per-hour exact latency layout; K 30.47 (official; fresh 30.66), and 30.45 on a second reading, against a-13's 30.17-30.36: inside the spread. It saved 1.4 MB for 55 more lines and ran twice as slow on a shuffled file, so a-13 stayed the base.
a-16 (KNOWN): one 64-byte block load for both the LF and '/' masks, and a 16-wide tag-group probe (SwissTable style) for the endpoint table; both raised instructions per line (651 -> 685 and 671) and neither lowered the scan time.
a-18 (OURS): latency chunks that double from 3 to 24 slots instead of 3 then 15; the box read K 31.40 (official; fresh 31.58), below a-17's K 31.83-32.44 and above its M 24.9-25.3MB: the test machine's 1 MB cut did not reach the box.
a-22 (OURS): the endpoint hit confirmed by four masked 32-byte compares instead of memcmp, and the LF found in two 32-byte windows per turn; the box read K 31.85 / 32.41, inside a-21's spread, with 32 more lines: no box gain.
a-23 (OURS): the tail's references placed by dense rank into per-rank linked lists instead of a comparison sort; on a-21's code the box read K 32.19 (the lower; fresh 32.87, M 26.9MB), inside a-21's K 32.09-32.58, so it was not merged.
a-25 (OURS): the worker count capped at 6, 8 and 10 threads; the box read K 30.16 (6 workers), 29.30 (8) and 31.44 (10), against 12 workers' K 32.09-32.53: fewer workers saved 2-4 MB and cost 3-10 ms, so all available CPUs stayed.
a-31 (OURS): the endpoint confirm by length and four 8-byte key words kept per id, and a hash from two independent multiplies; T level (0.030-0.031 s), S +15: K 37.47-38.16 against a-29's 38.29-38.55. The lookup's hash and confirm are not what the scan waits on.
a-32 (OURS): small groups ranked by counting with AVX2 compares in the percentile select; K 38.43 (official) / 38.52 (fresh), T 0.029-0.030 s, S +36: inside a-29's spread.
a-34 (KNOWN): a-34a found each 1 KiB's line ends from 64-byte LF bitmasks and walked the lines from that list; a-34b kept each hour's request and 5xx counts side by side; K 37.11-37.55 against a-29's 38.29-38.55 at S 367. T level, 46 more lines: the LF search is not what the scan waits on either.
a-48 (OURS): without the per-thread release (K 39.09 fresh / 39.64 official, T +1 ms) and with a per-thread MADV_POPULATE_READ (K 35.08 / 35.63, T +7 ms), against a-40d's K 39.49-40.03 / 39.66-40.42: the release pays at exit teardown, and the populate call costs more than the faults it saves.
a-49 (OURS): batched SIMD parse; scan instructions per line fell 188.1 -> 164.8 (parse 80.7 -> 43.8) but branch mispredicts were not cut and box T did not move.
a-50 (OURS): block stealing; K 37.84 fresh / 39.19 official, T 0.029-0.030 s, S 414, against a-47's 40.25 / 40.91: per-block release calls and counter traffic cost more than the tail spread they removed.
a-51 (OURS): unchecked hour counters; inside the spread for S +6 and an unsafe block.
a-52 (OURS): radix tail over hour records; two scatter passes added 37 M instructions to the tail, more than the selects saved.
a-54 (OURS): endpoint pools replaced by one copy before the tail and malloc_trim(0); inside spread, mooted by a-55.
a-55 r2c (OURS): Name repr(C, align(64)); OnceLock state still sat on a second cache line, M +0.5-0.9 MB.
a-55 r2e (OURS): in-place output formatting with fixed-size copies; inside spread at S +11 lines.
a-55 r3 (OURS): 4 interleaved line streams per block; throughput-bound loop, 4 streams only added L1 misses.

Final result (this tip, commit a6b3205, a-55 trim): box K 42.54 / 43.04 (lower: fresh; official 42.82 / 43.05), T 0.024 s, M 20.2-20.4 MB, S 463, D 1, against the reference's T 32,047 ms and M 228 MB. The banked board tip is 5c47d22 (a-47d, row 50 run-b79ba3882933, T 0.03 s, M 23 MB, points 79.4, S 396; board re-runs up to 80.5 points, run-90473e218dbb); the track's best board run is run-136eb6e8fd39, K 39.97 (a-35's code).
