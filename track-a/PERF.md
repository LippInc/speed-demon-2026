# Track A: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | push f15421bc5e82, scored ok | f15421b · Track A (run-b0eb3a208c93) | 0.16 | 45 | 59.3 |
| 2 | push 1d0fa92eb092, scored ok | 1d0fa92 · Track A (run-c9b407a05502) | 0.15 | 46 | 59.5 |
| 3 | push dea7a01343cf, scored ok | dea7a01 · Track A (run-149e73f79cee) | 0.08 | 50 | 66.2 |
| 4 | push 98a661eeaaa6, scored ok | 98a661e · Track A (run-dc24f55d6210) | 0.06 | 31 | 69.3 |
| 5 | push 5ecc03f78566, scored ok | 5ecc03f · Track A (run-4e5caeb3bdf1) | 0.06 | 31 | 69.1 |
| 6 | push dbc91d1aaa1a, scored ok | dbc91d1 · Track A (run-94c3438d3e16) | 0.05 | 30 | 71.9 |
| 7 | push bdf05643a17c, scored ok | bdf0564 · Track A (run-ad086f2812c9) | 0.05 | 30 | 71.9 |
| 8 | push 5263115c0f8b, scored ok | 5263115 · Track A (run-1e8cd19cfb4e) | 0.05 | 30 | 71.9 |
| 9 | push 68a1f5679a26, scored ok | 68a1f56 · Track A (run-34e8fb84e6ea) | 0.04 | 30 | 72.8 |
| 10 | push 376e3cdfa655, scored ok | 376e3cd · Track A (run-182a533acad8) | 0.05 | 30 | 72.6 |
| 11 | push b80b71eaed3a, scored ok | b80b71e · Track A (run-3749ce76278d) | 0.04 | 29 | 73.0 |
| 12 | push 704a88eb6e9f, scored ok | 704a88e · Track A (run-8a546276765a) | 0.05 | 29 | 72.5 |
| 13 | push cac9dd156516, scored ok | cac9dd1 · Track A (run-1d0d9fe3ae1a) | 0.04 | 29 | 73.0 |

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
- Rows 9 to 13 carry a-10: a two-level key (each thread interns endpoints to a small id; each hour maps endpoint id
  to its group with an array index) and one global endpoint rank, so the output order needs only integer sorts.

Experiments measured on our bench box only, with no board run (their box status is the evidence):

- a-2, failed: a two-pass counting sort into one flat latency array. `K 14.63 | T 0.152s | M 47.4MB | S 405 | sha
  5/5+5/5 OK` against a-1's K 15.46: the second pass cost as much as the one-thread tail it removed.
- a-7, failed, no code kept: batched prefetching of the table slots, groups and chunk writes (batch 4, 8 and 16).
  No variant beat a-6 on our test machine, so nothing was pushed for it.
- a-8, failed, no code kept: a cheaper hash, a 16-byte endpoint prefix in each slot, and a 32-byte vector compare.
  None beat a-6; cachegrind placed the lookup's cost in branch mispredictions of the linear probe.
- a-9: a phase probe (stderr timing lines only), never lifted. It measured where a-6's time goes on the box.
- a-11: memory. Latencies in 3-byte slots (larger values as an exact two-slot escape), 16-byte groups, and glibc's
  mmap threshold pinned so freed tail buffers return to the kernel. `K 28.81 | T 0.046s | M 24.1MB | S 610 | sha
  5/5+5/5 OK` against a-10's K 28.54 (M 27.9MB).
- a-12: the exit teardown. Each scan range's page-table entries are dropped after its scan. `K 29.00 | T 0.045s |
  M 24.1MB | S 625 | sha 5/5+5/5 OK`.
