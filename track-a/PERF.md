# Track A: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 32.05 | 228 | 0 |
| 1 | push f15421bc5e82, scored ok | run-b0eb3a208c93 | 0.16 | 45 | 59.3 |
| 2 | push 1d0fa92eb092, scored ok | run-c9b407a05502 | 0.15 | 46 | 59.5 |
| 3 | push dea7a01343cf, scored ok | run-149e73f79cee | 0.08 | 50 | 66.2 |

## What each push carried

- Steps 1 and 2 carry a-1, the floor (run-b0eb3a208c93, and run-c9b407a05502 when the board re-ran it on another
  track's push): Rust with the standard library only, the input mapped with mmap, one byte range per thread, a private
  open-addressing table per thread keyed by (hour, endpoint), and every latency kept in a per-group vector. One thread
  then merged the tables, sorted each group's latencies and formatted the output.
- a-2 failed and was never pushed to the board: a two-pass counting sort (pass 1 counts per group, pass 2 writes
  each latency into one flat array, percentiles by selection). Its box status was
  `K 14.63 | T 0.152s | M 47.4MB | S 405 | sha 5/5+5/5 OK` against a-1's K 15.46: the second pass cost as much as the
  one-thread tail it removed, and memory and code size went up.
- Step 3 carries a-3 (run-149e73f79cee): a-1's single pass with a parallel tail. Each thread sorts its own groups,
  then the hours are handed out to the threads, and each hour is merged, its percentiles selected and its lines
  formatted on one thread, written in output order.
- a-4 (no board run yet; it ships with a later push): memory. 24-byte group entries, latencies in chained chunks
  inside per-(thread, hour) blocks instead of per-group vectors, and each hour's blocks freed as soon as that hour
  is formatted.
