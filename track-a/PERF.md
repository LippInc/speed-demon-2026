# Track A: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | a-1, the floor | f15421b · Track A (run-b0eb3a208c93) | 0.16 | 45 | 59.3 |
| 2 | a-1, re-run (another track's push) | 1d0fa92 · Track A (run-c9b407a05502) | 0.15 | 46 | 59.5 |
| 3 | a-3, parallel tail | dea7a01 · Track A (run-149e73f79cee) | 0.08 | 50 | 66.2 |
| 4 | a-4, memory: 24-byte groups, chunked latencies | 98a661e · Track A (run-dc24f55d6210) | 0.06 | 31 | 69.3 |
| 5 | a-4, re-run (another track's push) | 5ecc03f · Track A (run-4e5caeb3bdf1) | 0.06 | 31 | 69.1 |
| 6 | a-5, one AVX2 load finds the endpoint and LF | dbc91d1 · Track A (run-94c3438d3e16) | 0.05 | 30 | 71.9 |
| 7 | a-6, the latency's space by one 16-byte compare | bdf0564 · Track A (run-ad086f2812c9) | 0.05 | 30 | 71.9 |
| 8 | a-6, re-run (another track's push) | 5263115 · Track A (run-1e8cd19cfb4e) | 0.05 | 30 | 71.9 |
| 9 | a-10, two-level key and global endpoint rank | 68a1f56 · Track A (run-34e8fb84e6ea) | 0.04 | 30 | 72.8 |
| 10 | a-10, re-run (another track's push) | 376e3cd · Track A (run-182a533acad8) | 0.05 | 30 | 72.6 |
| 11 | a-10, re-run (another track's push) | b80b71e · Track A (run-3749ce76278d) | 0.04 | 29 | 73.0 |
| 12 | a-10, re-run (another track's push) | 704a88e · Track A (run-8a546276765a) | 0.05 | 29 | 72.5 |
| 13 | a-10, re-run (another track's push) | cac9dd1 · Track A (run-1d0d9fe3ae1a) | 0.04 | 29 | 73.0 |
| 14 | a-10, re-run (another track's push) | 442dbe7 · Track A (run-9860100dcf4e) | 0.04 | 29 | 72.9 |
| 15 | a-11 + a-12, 3-byte latency slots, page release | 948854a · Track A (run-024d9e23cb50) | 0.04 | 25 | 73.1 |
| 16 | a-11 + a-12, re-run (another track's push) | dc6ca65 · Track A (run-cdd0c0f2c6d5) | 0.04 | 25 | 73.3 |
| 17 | a-13, branch-free parse | 22b6620 · Track A (run-ef28cacbc64d) | 0.04 | 25 | 74.6 |
| 18 | a-13, re-run (another track's push) | 90c864a · Track A (run-500a6311f2e5) | 0.04 | 25 | 74.7 |
| 19 | a-13, re-run (another track's push) | 2e2ee13 · Track A (run-77b664eb8d78) | 0.04 | 25 | 74.5 |
| 20 | a-13, re-run (another track's push) | 1e9607b · Track A (run-326f7e9db4bd) | 0.04 | 25 | 74.6 |
| 21 | a-17, endpoint pool and one-step merge | 48c9f56 · Track A (run-79c3f2b20fb0) | 0.04 | 26 | 75.4 |
| 22 | a-17, re-run (another track's push) | 83d39fc · Track A (run-3ed371d7efad) | 0.03 | 26 | 75.6 |
| 23 | a-17, re-run (another track's push) | e71baea · Track A (run-5564519d32b9) | 0.04 | 26 | 75.3 |
| 24 | a-21 + a-26, percentile select, formatter, static C runtime | c32fd6a · Track A (run-711ada7fc374) | 0.04 | 26 | 75.2 |
| 25 | a-21 + a-26, re-run (another track's push) | 2979028 · Track A (run-11fd5f374201) | 0.03 | 26 | 75.8 |
| 26 | a-21 + a-26, re-run (another track's push) | 0c42543 · Track A (run-9ee51f1d4dbe) | 0.03 | 26 | 75.4 |
| 27 | a-21 + a-26, re-run (another track's push) | b581408 · Track A (run-6cc162793fff) | 0.03 | 26 | 75.7 |
| 28 | a-21 + a-26, re-run (another track's push) | e2b846c · Track A (run-c2f4f3fac5e6) | 0.03 | 26 | 75.6 |
| 29 | a-21 + a-26, re-run (another track's push) | 5d16aa8 · Track A (run-23f2ae0d407d) | 0.03 | 26 | 76.0 |
| 30 | a-21 + a-26, re-run (another track's push) | dc1cab8 · Track A (run-2460ae819b84) | 0.03 | 26 | 76.1 |
| 31 | a-21 + a-26, re-run (another track's push) | 5c78ab8 · Track A (run-ac92f2fd600e) | 0.03 | 26 | 75.4 |

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
  single-run spread of a-17's rows (T 0.035 s on the board against 0.033-0.034 s on the box). Rows 25 to 31 re-ran the same folder on other tracks' pushes: points 75.2 to 76.1, so the board's single-run spread on this code is about 1 point.

Experiments measured on our bench box only, with no board run (their box status is the evidence):

- a-2, failed: a two-pass counting sort into one flat latency array. `K 14.63 | T 0.152s | M 47.4MB | S 405 | sha
  5/5+5/5 OK` against a-1's K 15.46: the second pass cost as much as the one-thread tail it removed.
- a-7, failed, no code kept: batched prefetching of the table slots, groups and chunk writes (batch 4, 8 and 16).
  No variant beat a-6 on our test machine, so nothing was pushed for it.
- a-8, failed, no code kept: a cheaper hash, a 16-byte endpoint prefix in each slot, and a 32-byte vector compare.
  None beat a-6; cachegrind placed the lookup's cost in branch mispredictions of the linear probe.
- a-9: a phase probe (stderr timing lines only), never lifted. It measured where a-6's time goes on the box.
- a-11 and a-12 also have board rows (15 and 16); their box readings were:
- a-11: memory. Latencies in 3-byte slots (larger values as an exact two-slot escape), 16-byte groups, and glibc's
  mmap threshold pinned so freed tail buffers return to the kernel. `K 28.81 | T 0.046s | M 24.1MB | S 610 | sha
  5/5+5/5 OK` against a-10's K 28.54 (M 27.9MB).
- a-12: the exit teardown. Each scan range's page-table entries are dropped after its scan. `K 29.00 | T 0.045s |
  M 24.1MB | S 625 | sha 5/5+5/5 OK`.
- a-13: a branch-free parse. The latency's 1 to 10 digits converted with one 16-byte load and SIMD multiply-adds,
  the 5xx flag read from the status's first digit, and the hour prefix compared as two u64 words. `K 30.17 (fresh;
  official 30.86) | T 0.041s | M 24.1MB | S 641 | sha 5/5+5/5 OK`.
- a-14, failed: four ranges per worker, each thread's own range first and the rest claimed from a shared counter.
  `K 28.24 (official; fresh 29.61) | T 0.043s | M 29.8MB | S 653 | sha 5/5+5/5 OK` against a-13's K 30.17-30.36
  (M 24.1MB): each thread's tables spanned more hours, so memory and the merge grew, and the balance did not win
  the time back.
- a-15, NOISE, not lifted: the per-hour exact latency layout. A thread keeps at most three hours as chunk chains and copies older
  hours back to back into exact arrays; groups live in their hour's arena and are freed with it.
  `K 30.47 (official; fresh 30.66) | T 0.040s | M 22.7MB | S 696 | sha 5/5+5/5 OK`, and 30.45 on a second reading,
  against a-13's 30.17-30.36: inside the spread. It saved 1.4 MB for 55 more lines and ran twice as slow on a
  shuffled file, so a-13 stayed the base.
- a-16, failed, no code kept: one 64-byte block load for both the LF and '/' masks, and a 16-wide tag-group probe
  (SwissTable style) for the endpoint table. On our test machine both raised instructions per line (651 -> 685 and
  671) and neither lowered the scan time.
  The box re-measured a-13's code on that branch's report commit: `K 30.50 (official; fresh 30.92) | M 24.1MB`.
- a-17: the gap between the scan and the tail. Endpoint bytes copied into a small pool per thread, so nothing after
  the scan refaults the released input mapping, and the global endpoint merge takes one step per distinct endpoint
  instead of one per (thread, endpoint). `K 32.08 (official; fresh 32.24) | T 0.035s | M 25.3MB | S 651 | sha
  5/5+5/5 OK` against a-13's K 30.17-30.50 (T 0.039-0.041s).
- a-18, failed (dropped): latency chunks that double from 3 to 24 slots instead of 3 then 15. On our test machine
  it lowered peak memory by 0.8 to 1.0 MB and the latency storage by 1.0 MB; the box read `K 31.40 (official; fresh
  31.58) | T 0.036s | M 26.0MB | S 655 | sha 5/5+5/5 OK`, below a-17's K 31.83-32.44 and above its M 24.9-25.3MB, so
  the test machine's 1 MB cut did not reach the box.
- a-20: the tail's percentile select. Groups of up to 32 latencies are sorted; larger ones are selected in ascending
  rank order (p50 on all values, p95 right of it, p99 right of that), about 1.55n of select work against 2.9n.
  `K 32.36 (official; fresh 32.44) | T 0.035s | M 25.2MB | S 664 | sha 5/5+5/5 OK`, and K 32.21 official / 32.66 fresh
  on a second reading, against a-17's K 31.83-32.44: just above, inside the spread; merged into a-21.
- a-19: the output number formatter, built by a spec-driven builder: two digits per step from a 200-byte table.
  Box K 32.25 official, inside a-17's K 31.83-32.44, and 31.48 fresh, below it; merged into a-21.
- a-21: a-20's select and a-19's formatter together, boxed as one. `K 32.53 (fresh; official 32.58) | T 0.034s |
  M 25.1MB | S 680 | sha 5/5+5/5 OK`, and K 32.09 fresh / 32.92 official (T 0.033s, M 26.5MB) on a second reading,
  against a-17's K 31.83-32.44 (T 0.035s).
- a-22, no gain on the box: the endpoint hit confirmed by four masked 32-byte compares instead of memcmp, and the
  LF found in two 32-byte windows per turn. On our test machine conditional mispredicts fell 2.59 -> 1.64 per line
  and the scan 6-7 %; the box read `K 31.85 (fresh; official 32.69) | T 0.033-0.035s | M 25.1MB | S 712` and
  `K 32.41 (official; fresh 32.51) | T 0.034s | M 25.3MB | S 712`, inside a-21's spread, with 32 more lines.
  a-22 was dropped: it gave no box gain, box lower K 31.85 / 32.41, and S +32.
- a-24, the line budget: the same program in fewer counted lines (S 680 -> 401), with level T and M. The work was a
  layout rewrite after rustfmt, two fixes where the relayout had changed the hash and the probe loop, and where it held
  the probe table and sorted lists through the tail; then dropping the checks spec 2 rules out, and going back to the
  one-digit formatter. `K 35.06 (official; fresh 34.92) | T 0.034s | M 26.4MB | S 401 | sha 5/5+5/5 OK` on the last
  code commit, `K 34.89 (official; fresh 35.00) | T 0.034-0.035s | M 25.1MB | S 401` on the same code again (the docs
  commit), and `K 34.90 (official; fresh 35.39) | T 0.033-0.034s | M 25.5MB | S 413` on the one before, against
  a-21's K 32.09-32.58 (T 0.034s, M 25.1-26.5MB, S 680).
- a-23, inside the spread, not kept: the tail's references placed by dense rank into per-rank linked lists instead of a
  comparison sort, built by a spec-driven builder. On a-21's code the box read K 32.19 (the lower; fresh 32.87, M
  26.9MB), inside a-21's K 32.09-32.58, so it was not merged.
- a-25, failed (dropped): the worker count capped at 6, 8 and 10 threads in place of all available CPUs, built by a
  spec-driven builder. On a-21's code the box read K 30.16 (6 workers; T 0.042s, M 21.2MB), 29.30 (8; T 0.043-0.044s,
  M 22.8MB) and 31.44 (10; T 0.037s, M 23.8MB), against 12 workers' K 32.09-32.53 (T 0.033-0.034s): fewer workers
  saved 2-4 MB and cost 3-10 ms, so all available CPUs stayed.
- a-26: the C runtime linked statically (`+crt-static` in the Dockerfile's RUSTFLAGS), so no dynamic loader runs before
  main, built by a spec-driven builder. On a-21's code the box read K 33.00 official / 32.85 (the lower) and 32.75 (the
  lower) on a second reading, against a-21's K 32.09-32.53.
- a-27: a-26's Dockerfile change on a-24's code, built by a spec-driven builder. `K 35.01 (fresh; official 35.30) |
  T 0.034s | M 26.6MB | S 401 | sha 5/5+5/5 OK` against a-24's code at K 34.89-35.06.
- a-28, probe (never lifted): five timed passes over the input, per thread, at 12 workers. Parse only 7.3-7.6 ms, plus the endpoint lookup 14.4-14.7, plus the records and counts 18.6-19.4, the whole scan 19.4-19.8: parse about 38 %, lookup 36 %, bookkeeping 26 %.
- a-29, kept: per-hour 4-byte records (endpoint id and latency in one u32, a three-word escape for ids of 2047 or more and latencies of 2^21 or more) in place of per-group latency chains; the tail places each hour's latencies by endpoint rank with a counting sort. `K 38.29 (lower: fresh) | official K 38.55 T 0.030s (0.029-0.031) | fresh K 38.29 T 0.031s | M 23.1MB S 367`, against a-27's 35.01-35.41.
- a-30, probe (never lifted): the tail took about 9.5 ms of wall time; select and format were 57-78 thread-ms of it.
- a-31, dropped: the endpoint confirm by length and four 8-byte key words kept per id, and a hash from two independent multiplies. T level (0.030-0.031 s), S +15: K 37.47-38.16 against a-29's 38.29-38.55. The lookup's hash and confirm are not what the scan waits on.
- a-32, inside the spread, not kept: small groups ranked by counting with AVX2 compares in the percentile select. K 38.43 (official) / 38.52 (fresh), T 0.029-0.030 s, S +36: inside a-29's spread.
- a-33, probe (never lifted), on a-29's code: scan and join about 20 ms, global ranks 0.8 ms, the tail about 7.5 ms (CPU-bound and balanced across the 12 threads: placing 19-20, select 37-45, format 25-30 thread-ms), the last writes about 1.5 ms. T about 30 ms.
- a-34, dropped (spec-built): a-34a found each 1 KiB's line ends from 64-byte LF bitmasks and walked the lines from that list, so no line's start waited on the previous line's LF search; a-34b kept each hour's request and 5xx counts side by side. a-34a `K 37.11 (lower: fresh) | official K 37.55 T 0.031s (0.030-0.032) | fresh K 37.11 T 0.031s (0.030-0.036) | M 23.2MB S 413`; a-34b K 36.63 (official) / 37.67 (fresh), S 411; against a-29's 38.29-38.55 at S 367. T level, 46 more lines: the LF search is not what the scan waits on either.
