# Track C: performance log

How join.c got to its speed, with the measurement behind each step. Board runs are named by their run ID; everything else was measured on our test machine (box 2) and says so.

## Where it stands

The reference takes 14.44 s (K 1).

| Run | T | M | S | K |
|---|---|---|---|---|
| Rust version, board run-fff3b356a7d3 (commit 2306aa9) | 28.1 ms | 9.6 MB | 585 | 36.48 |
| join.c v21, organisers' harness on box 2 (run-f5ed3dcee970, run-e6258add888b, run-b426dc4df42d, run-3d8c94661195, run-37470e5c0dcb) | 24.37 ms | 10.75 MB | 419 | 40.2 |
| join.c v21 on the board, run-fc3513efb0d2 (commit 816a29f) | 27.69 ms | 10.94 MB | 419 | 37.63 |
| join.c v27, organisers' harness on box 2 (run-a61e717c2817, run-2668ae2f706a, run-bcf351d7d996, run-7032e46a98b9, run-a6d1b79f12a2) | 22.74 ms | 10.74 MB | 443 | 41.3 |

v21 ran 3.3 ms slower on the board than on box 2; the Rust version had matched within 0.7 ms. The cause is in "Other processes on the scoring box" below; v27 is the fix.

## Measuring

- **The harness.** The organisers' run_submission.py (5 runs, median T, peak M) and static_axes.py (S) run on box 2 against the full seed-1 data.
- **Fast loop.** A static build timed under `taskset 0-11`, 20 to 40 runs, median; A/B comparisons alternate the variants in rounds. Validated against the harness: the Rust binary measured 27.4 ms this way against 28.1 ms on the board.
- **Correctness.** Every variant is checked against 8 generated seeds (tiny, sample and full sizes) and the two published data sets before it is timed.
- **Where the time goes.** `perf stat` instruction and cycle counts per variant, with parts of the program switched off one at a time (the "split" rows below), plus raw cache-miss and snoop events.

## The design and why

The comment at the top of join.c describes the program. The measurements behind its main choices:

- **Floor.** Reading and scanning all 202 MB with 12 threads and nothing else: 5.6 ms with pread in 64 KB blocks, 7.6 ms mapped. Carry-less-multiply quote masks on top cost nothing (still 7.6 ms): the scan is memory-bound, so the parse can afford SIMD work per byte.
- **Twelve threads taking 256 KB pieces from a shared counter.** A record start is found locally (an LF followed by a digit or BOM). On a quiet box a fixed twelfth per thread was as fast (24.05-24.13 against 24.1 ms): a hyperthread that finishes early hands its core to its sibling. The pieces are for a box that is not quiet (below).
- **Two passes per 16 KB.** Pass 1 writes the offset of every separator outside quotes; pass 2 reads each record's fields at those offsets, so records do not wait on each other.
- **Region lookups first.** Pass 2 looks up all of a block's regions in a short loop before doing the rest, so the misses on the 1 MB region table overlap.
- **Names go to an owner thread.** A name's hash picks its owner; the reader sends an 8-byte message through a ring, 128 messages at a time (8 at a time: 1.5 ms slower; 16: 0.9; 32: 0.6; drain every 8 passes instead of 4: 0.2 ms faster). One table shared by all threads with an atomic OR per bit (measured in the Rust version) took 58.2 ms against 31.5 ms, with 3.03 M cross-core snoop loads against 0.23 M.
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
- v24, 22.55 ms: messages staged 128 per owner (was 8), rings of 256, owners drain every 8 passes.
- v27, 22.63 ms: 256 KB pieces from a shared counter, and sends that never wait (below).

Wall time with parts switched off (v21): pass 1 alone 7.25 ms, the same as reading and scanning the file with nothing else (7.6 ms); then parsing and per-country sums +7.7, note lengths +0.8, name hash +0.85, sending names +3.0, owners filing them +4.5.

Where v20b's work went (instructions / cycles, summed over threads): pass 1 287 M / 156 M; record parse 468 M / 213 M (141 instructions per record); text lengths 41 M / 22 M; name hash 157 M / 69 M (v21 cut most of this); sending 99 M / 84 M; owner draining 200 M / 131 M. The owner's region table and name tables (~1 MB + ~0.8 MB per core) do not fit a 1.25 MB L2 together: ~1 M L3 hits per run.

## Other processes on the scoring box

With a busy loop on one of CPUs 0-11 for 1 ms in every 5 (box 2, fast loop, medians of 2 rounds):

- v24, fixed twelfths, a sender waits while its owner's ring is full: 22.55 ms quiet, 26.3 to 26.9 ms with the noise. One thread kept off its CPU stalls every thread that sends to it.
- v26, pieces and an unbounded per-owner queue on the hot path: 23.8 quiet, 24.2 with noise.
- v27, pieces, fixed batches, and an overflow queue used only when a ring is full: 22.63 quiet, 23.8 to 23.9 with noise.

A busy loop on one CPU for the whole run made v24 25 times slower (546 ms) and v26 8 times (172 to 192 ms).

## What did not work (box 2)

- `-march=alderlake` or `native` instead of x86-64-v3: 0.4 ms slower.
- Reading with pread instead of mapping: 34.8 against 33.3 ms in the full program, although pread wins the bare scan; again at v21 (v22): 25.0 against 24.1 ms. The kernel's copy took 80 M cycles against 53 M for mapping and unmapping.
- Each thread populating its part of the mapping up front (`MADV_POPULATE_READ`): 28 ms, the threads queue on the mapping's lock.
- Region table misses: a probe without the lookup (wrong output) was 1.6 ms faster, but the same lookup into a 256 KB table recovered only 0.55 ms, so a compact table was not built.
- Rings of 128 messages: 0.2 ms slower than 256. Pass 1 over 4 KB instead of 16 KB: 0.2 ms slower.
- `-mtune-ctrl=^avoid_false_dep_for_bmi`: 30 M fewer instructions, no change in time.
- Without 64-byte message lines (one 8-byte store per message): 7 ms slower. Non-temporal stores for the lines: no change.
- Two interleaved input streams per thread: slower.
- Deferring the per-country updates through a record array: slower.
- SIMD buckets against linear probing in the owner table: no difference at v17 (they won later, at v20, together with the smaller identity).
- Branch misses: about 4 % of cycles at v18, so branch-free rewrites were not the lever; the program is bound by instruction count (67 % of slots retiring, about 2 instructions per cycle per thread).
