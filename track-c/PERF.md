# Track C: performance log

How join.c got to its speed, with the measurement behind each step. Board runs are named by their run ID; everything else was measured on our test machine (box 2) and says so.

## Where it stands

The reference takes 14.44 s (K 1).

| Run | T | M | S | K |
|---|---|---|---|---|
| Rust version, board run-fff3b356a7d3 (commit 2306aa9) | 28.1 ms | 9.6 MB | 585 | 36.48 |
| join.c v21, organisers' harness on box 2 (run-f5ed3dcee970, run-e6258add888b, run-b426dc4df42d, run-3d8c94661195, run-37470e5c0dcb) | 24.37 ms | 10.75 MB | 419 | 40.2 |

## Measuring

- **The harness.** The organisers' run_submission.py (5 runs, median T, peak M) and static_axes.py (S) run on box 2 against the full seed-1 data.
- **Fast loop.** A static build timed under `taskset 0-11`, 20 to 40 runs, median; A/B comparisons alternate the variants in rounds. Validated against the harness: the Rust binary measured 27.4 ms this way against 28.1 ms on the board.
- **Correctness.** Every variant is checked against 8 generated seeds (tiny, sample and full sizes) and the two published data sets before it is timed.
- **Where the time goes.** `perf stat` instruction and cycle counts per variant, with parts of the program switched off one at a time (the "split" rows below), plus raw cache-miss and snoop events.

## The design and why

The comment at the top of join.c describes the program. The measurements behind its main choices:

- **Floor.** Reading and scanning all 202 MB with 12 threads and nothing else: 5.6 ms with pread in 64 KB blocks, 7.6 ms mapped. Carry-less-multiply quote masks on top cost nothing (still 7.6 ms): the scan is memory-bound, so the parse can afford SIMD work per byte.
- **Twelve threads, each with its own twelfth of the file.** A record start is found locally (an LF followed by a digit or BOM), so there is no shared work counter and no rounds. Replacing per-round barriers with lock-free rings changed nothing: the threads are balanced.
- **Two passes per 16 KB.** Pass 1 writes the offset of every separator outside quotes; pass 2 reads each record's fields at those offsets, so records do not wait on each other.
- **Region lookups first.** Pass 2 looks up all of a block's regions in a short loop before doing the rest, so the misses on the 1 MB region table overlap.
- **Names go to an owner thread.** A name's hash picks its owner; the reader sends an 8-byte message through a ring, 64 bytes at a time. One table shared by all threads with an atomic OR per bit (measured in the Rust version) took 58.2 ms against 31.5 ms, with 3.03 M cross-core snoop loads against 0.23 M.
- **AES-NI name hash.** Three AES rounds over 32 bytes at a time, cut to 64 bits: fewer instructions than the multiply hash it replaced (see v21).
- **Memory.** 4 KB pages only (`MADV_NOHUGEPAGE`): with transparent huge pages on, first touches zeroed 2 MB pages, 5.4 ms. Each thread drops its share of the mapping at the end, so the exit does not tear down 49,000 page-table entries on one CPU: 2 ms.

## Steps

Box 2, fast loop, median T; none of these were board runs.

- v10, 31.6 ms (harness): an early C version of the owner design.
- v18, 29.4 ms: hot-loop state in locals (aliasing stores to struct fields had cost ~10 % of instructions), 8-slot SIMD buckets.
- v19, 27.2 ms: pass 1 without quote counts; pass 2 split into a region loop and the rest; quotes counted from field bytes; branch-free name hash.
- v20, 26.1 ms: 4-slot SIMD buckets, 56-bit name identity, the owner stores 16 bits per name instead of the key.
- v20b, 25.55 ms: name hash fast path without a loop: stack spills in the record loop 47 to 2.
- v21, 24.1 ms: AES-NI name hash: instructions 1.54 G to 1.17 G per run.

Where v20b's work went (instructions / cycles, summed over threads): pass 1 287 M / 156 M; record parse 468 M / 213 M (141 instructions per record); text lengths 41 M / 22 M; name hash 157 M / 69 M (v21 cut most of this); sending 99 M / 84 M; owner draining 200 M / 131 M. The owner's region table and name tables (~1 MB + ~0.8 MB per core) do not fit a 1.25 MB L2 together: ~1 M L3 hits per run.

## What did not work (box 2)

- `-march=alderlake` or `native` instead of x86-64-v3: 0.4 ms slower.
- Reading with pread instead of mapping: 34.8 against 33.3 ms in the full program, although pread wins the bare scan.
- Without 64-byte message lines (one 8-byte store per message): 7 ms slower. Non-temporal stores for the lines: no change.
- Two interleaved input streams per thread: slower.
- Deferring the per-country updates through a record array: slower.
- SIMD buckets against linear probing in the owner table: no difference at v17 (they won later, at v20, together with the smaller identity).
- Branch misses: about 4 % of cycles at v18, so branch-free rewrites were not the lever; the program is bound by instruction count (67 % of slots retiring, about 2 instructions per cycle per thread).
