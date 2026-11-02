# Track C: performance log

How join.c got to its speed, with the measurement behind each step. Rows named by a run ID are board runs. Local rows from before 2026-11-02 say "box 2"; today's local rows are box 1 (i5-13500, 6 P-cores with HT at a fixed 2.5 GHz, the same machine type as the board).

## Where it ended

The final code is v65. It reads records.csv once per record with no pass 1, keeps each thread's state on three huge pages faulted in by main and one helper, and identifies distinct names by owner threads exchanging 8-byte messages (join.c's header walks the design). The board run run-6d3cdfadd754 measured T 14.32 ms, M 8.07 MB, S 373, K 56.60; the reference takes 14.44 s, K 1. That run is the submission named in both this file and README.md. A quiet box reads lower than a busy board: until the final runs the board shares its machine, T moves with the load and M with what ran in the cgroup before us (section 6).

## Where it stands

Rows v56 to v62 have no run ID at hand and are identified by their commit.

| Version | Commit | Board run | T | M | S | K |
|---|---|---|---|---|---|---|
| join.c v49 (v43's code, comments fixed) | 6b75bfd | run-14a8fd87f9be | 21.74 ms | 10.3 MB | 417 | 43.04 |
| join.c v49 | 4e793b1 | run-be7fc43b744a | 21.31 ms | 10.51 MB | 417 | 43.27 |
| join.c v49 | 8442d94 | run-5e3b2bc95cec | 21.16 ms | 9.89 MB | 417 | 43.97 |
| join.c v53 (v49's code, waits that sleep) | 55c6a91 | run-739c6cea137d (a busy board) | 27.31 ms | 10.27 MB | 418 | 38.39 |
| v56 | db1bb02 | - | - | - | - | 47.82 |
| v59 | - | - | 16.48 ms | 7.92 MB | 392 | 52.56 |
| v60 | 78587c9 | - | 16.23 ms | 7.89 MB | 394 | 52.96 |
| v61 | 73fcc4e | - | 16.17 ms | 10.24 MB (spike, section 6) | 369 | 50.88 |
| v62 | 6ca940a | - | 15.53 ms | 8.01 MB | 371 | 54.48 |
| v63 | a2b2bdd | run-a5c4d0536268 | 14.754 ms | 7.80 MB | 377 | 56.05 |
| v63 (same code) | a9af0b7 | run-aeb5ee4b7a28 | 14.921 ms | 8.29 MB | 377 | 55.05 |
| v65 | 6cafc7e | run-6d3cdfadd754 | 14.32 ms | 8.07 MB | 373 | 56.60 |
| v65 (same code) | 3fa85cb | run-ec315dedca74 | 14.446 ms | 7.92 MB | 373 | 56.56 |

The reference takes 14.44 s, K 1. M is the maximum of the board's 5 runs, which is why the same code reads 7.80 and 8.29 (section 6).

## Measuring

- **abn.sh**, the local A/B runner: alternates the binaries over 15 to 31 rounds, deletes the output file before each run and checks its SHA, so a crash or a wrong output leaves no number (a bad run is logged with its exit code). Its wall time includes loop overhead: an empty static binary reads 2.93 ms in the same loop, and the program's own timeline accounts for all but 0.33 ms of the rest. Noise about +-0.1 ms at 15 to 31 rounds; code-layout luck up to 0.2 ms.
- **icount.sh**: user instructions and cycles (perf), stable to 0.1 %.
- **Board figures**: T is the mean of the middle three of 5 runs (fastest and slowest dropped), M the maximum of the 5.
- **K** = (Tref/T)^.5 (Mref/M)^.2 (Sref/S)^.15; M stops paying at 7.35 MB, so there is no reward below that.
- **S** is code lines counted after clang-format LLVM (merging lines does nothing; wraps caused by trailing comments do count).

## The design and why

- **One read per record, no pass 1.** Earlier versions ran a pass 1 that listed record ends with three AVX2 compares per 32 bytes; it was 13 % of all instructions. Now each record is read once, front to back, from one quote mask of 64 bytes starting at b + 6, past most ids: for 97 % of records the mask reaches past the note. A carry-less multiply turns the mask into "follows an odd number of quotes", which picks the commas after the name and the note however many "" they hold. Only the first record of a piece needs the LF rule (spec 2.2: a record's own LF is followed by a digit, a BOM or the end of the file), and that test stays a branch: a cmov here cost 0.68 ms, because the chain from one record to the next binds.
- **Two cursors per piece.** One record's start depends on the last one's end, a wait of about 50 cycles, so each piece is read from two places in turn and the two waits overlap. A run of 64 records is parsed first, staged in Rec; a second loop then does the lookups, totals and messages, each record's region-table entry prefetched (at v31 the prefetch took T from 22.30 to 21.67 ms). One pass with no staging was 2.2 ms slower (the extra cycles were load stalls hitting L3 on the 1 MB region table); the two-cursor shape was 0.8 ms faster than v56. The id's length is kept per cursor because spec 2.2 says ids only grow.
- **Owner threads for names.** The name's hash low byte picks its owner; the reader sends an 8-byte message (the hash, with the country in the low byte) through a lock-free ring, 128 at a time (8 at a time: 1.5 ms slower). The owner keeps a private table: 16 tags per bucket on one cache line (BUCKETS 652; 97 % of messages hit in the first bucket), and one bit per country in the row, set and counted at once by bts and sbb. One table shared by all threads took almost twice as long (58.2 against 31.5 ms in the Rust version; 23.00 against 20.35 in C, v50). The 16-tag bucket was -0.22 ms; the owner from the hash's low byte, -0.17.
- **Three huge pages at a fixed address (3 << 29).** All memory the threads use is one mapping, so nothing else lands on extra huge pages. Only main and one helper thread fault memory in: a thread on another CPU that charges even one page costs the cgroup a 256 KB per-CPU charge batch. The other threads fill the region table meanwhile (all of it before the barrier lies on the first huge page): -0.35 ms together with the spin barrier. The fixed address must lie above the program end plus 1 GB (section 7); the low address itself was -0.15 ms, and starting the mask at b + 6 another -0.15, two prefetches (b + 512 and b + 576) -0.08.
- **Plain clone threads on 4 KB stacks.** They share main's thread-local block and use nothing of it but errno: no memory for a TLS block or a thread descriptor, and the stacks use at most 640 bytes. Earlier pthread version, same design: no gain from the extra memory.
- **128 KB pieces from a shared counter**, piece i spread across 2 MB stretches, so threads that fault pages at the same time never share a stretch's page-table lock (a kernel profile of v37 had 24 % of kernel cycles spinning on that lock; spreading pieces out cut kernel cycles from ~70 M to ~60 M). 256 KB pieces: +0.8 ms, the rings fill (293 context switches against 40).
- **The end.** Once every total is final, main writes the output while the other 11 threads each drop an eleventh of the mapping; otherwise exit tears down 49,000 page-table entries on one CPU (2 ms).

## Steps

Board rows carry their run ID; rows without one are local. Rows marked "box 2 only" never had a board run (box 2, fast loop, median).

- v10, 31.6 ms (harness), board 33.6 (run-34ea5e6b408e): an early C version of the owner design.
- v18, 29.4 ms, board 30.1 (run-66e847fccb66): hot-loop state in locals, 8-slot SIMD buckets.
- v19, 27.2 ms, board 27.8 (run-f963f4408506): pass 1 without quote counts; pass 2 split into a region loop and the rest.
- v20, 26.1 ms, board 26.4 (run-b49c7f2b2d8c): 4-slot SIMD buckets, 56-bit name identity.
- v20b, 25.55 ms (box 2 only): name hash fast path without a loop.
- v21, 24.1 ms, board 24.9 (run-9767cb14f1a6): AES-NI name hash (1.54 G to 1.17 G instructions).
- v24, 22.55 ms, board 23.3 (run-b39ff361cb97): messages staged 128 per owner.
- v27, 22.63 ms, board 24.6 (run-709c5622f461): 256 KB pieces from a shared counter, sends that never wait.
- v28, 22.7 ms (box 2 only): pass 1 lists record ends only; pass 2 reads fields from both ends.
- v30, 22.30 ms (box 2 only): per-record array in the thread's context, id's comma by a vector compare.
- v31, 21.67 ms, board 22.9 (run-5d3de098bf35): region table prefetch.
- v32, 21.16 ms, board 84.6 (run-fc8350371cd8, one slow shared-board run): senders drain while their ring is full.
- v37, -0.3 ms (box 2 only): main writes the output while the others unmap.
- v38, -0.13 ms (box 2 only): pieces 2 MB apart.
- v39, 20.64 ms, board 21.6 (run-33798317abda): one malloc arena.
- v42, -0.17 ms (box 2 only): min and max as a branch, not a compare and store per record.
- v43, 20.31 ms, board 22.48 (run-a355185f438a): the region index in the record entry.
- v49 (v43's code, comments fixed): board 21.74, 21.31 and 21.16 ms (the table above).
- v53, the same speed on a quiet box: every wait spins 128 pauses, then sleeps 10 us (section 6).

Today (box 1 unless a run ID is given):

- v54 (4ba13ab): built with gcc 16, -0.3 ms on the local loop. Board: T 21.19 ms, M 7.91, S 389, K 46.42.
- v56 (db1bb02), the redesign: no pass 1, one read per record from a 64-byte quote mask, two cursors per piece, 16-tag buckets, threads fill the region table while main faults the huge pages. 865 M instructions, abn 21.3 against v53's 23.5. In order of what moved T (each an A/B): the BOM test as a branch, not cmov, -0.68; pointers instead of offsets, -0.5; threads fill the region table while main faults, with a spin barrier, -0.35; two cursors with Rec staging and region prefetch, -0.8 against v56; 16-tag buckets, -0.22; owner from the hash's low byte, -0.17; the mask window at b + 6, -0.15; the low fixed address, -0.15; two prefetches, -0.08.
- v57 (61a43ce): abn 20.8.
- v58 (fd2350d): abn 18.91.
- v59: abn 18.43; board T 16.48 ms, M 7.92, S 392, K 52.56.
- v60 (78587c9): the crash fix (section 7, map moved to 3 << 29), memory constants, build flags. Board T 16.23 ms, M 7.89, S 394, K 52.96.
- v61 (73fcc4e): plain clone threads on 4 KB stacks, BUCKETS 652, an atomic_flag lock that uses the sleeping wait, meet() replaces join, no -pthread. abn 17.99; board T 16.17 ms, M 10.24 (the spike, section 6), S 369, K 50.88.
- v62 (6ca940a): the cold test from the mask, a vector min for the amount cap, the id stored first, the tag read straight from the message. 680 M instructions, abn 17.49; board T 15.53 ms, M 8.01, S 371, K 54.48.
- v63 (a2b2bdd), instructions 680.0 M to 614.0 M, abn 17.45 to 16.55, in order: the amount's decimal point skipped by per-lane multipliers instead of pext (666.6 M, 17.41 to 17.16); the next record found by a carry through the run of low bytes, so no BOM test in parse (653.3 M, 16.88); the id's length kept per cursor, because spec 2.2 says ids only grow (643.7 M); bit test and count with bts and sbb (634.2 M, 16.65); the bucket from the message's top two bytes (cycles 340.2 M to 336.1 M); name and note bytes added as one 64-bit field (616.3 M with the rest). Board run-a5c4d0536268: T 14.754 ms, M 7.80, S 377, K 56.05; the same code again as a9af0b7, run-aeb5ee4b7a28: T 14.921 ms, M 8.29, K 55.05.
- v64: the timeline of v63 (timestamps in a copy of the program) showed 11 threads idle for 0.75 ms while main alone faulted memory in: main's first huge page 0.24 ms, the clones 0.11, huge pages 2 and 3 0.55, the page-table pages 0.30; regions done at 0.49; parse ends 13.0, drained 13.09, report 13.39, all done 13.54. One helper thread (thread 11) now faults the third huge page and half the file's page-table pages. abn 16.66 to 16.34 (31 rounds). Memory: another CPU that charges a page costs a 256 KB batch, and the page-table pages use that batch up: median of 15 runs 7.57 to 7.67 MB. Error checks are back on open, clone and close.
- v65: start-up balance. The helper is now cloned first, so it starts ahead of main, which still has the other clones to do, and it takes 5/8 of the file's page-table pages: 61 of 98, inside its 64-page charge batch, so little of the batch is left over. abn 16.29 to 16.23 (25 rounds), memory peak 7.50 to 7.40 MB, no lines added. Board run-6d3cdfadd754: T 14.32 ms, M 8.07, S 373, K 56.60; the same code again as 3fa85cb, run-ec315dedca74: T 14.446 ms, M 7.92, K 56.56.

## Other processes on the scoring box

With a busy loop on one of CPUs 0-11 for 1 ms in every 5 (box 2, medians of 2 rounds):

| Version and scheme | Quiet | With the noise |
|---|---|---|
| v24, fixed twelfths, a sender waits when the ring is full | 22.55 ms | 26.3 to 26.9 ms |
| v26, pieces, unbounded per-owner queue on the hot path | 23.8 ms | 24.2 ms |
| v27, pieces, overflow queue only when a ring is full | 22.66 ms | 24.0 ms |
| v39, pieces, a sender that drains while it waits | 20.64 ms | 24.0 ms |

A busy loop on one CPU for the whole run made v24 25 times slower (546 ms). v43's code: 331 ms with one such loop, 439 with two, against 23.3 quiet. v53's sleeping wait (10 us after 128 pauses) reads 30.0 and 32.9 ms in the same rounds, the same speed on a quiet box.

- Board reads vary with load: while the board was busy, other teams' latest runs read 2 to 4 ms over their best. The final runs have the machine to themselves.
- M spikes on the same code with T unaffected: 7.80 and 8.29 MB for v63, 10.24 for v61. Our hypothesis, not proven: per-CPU 256 KB charge batches left by processes that ran in the cgroup before the program; only main and the helper charge in our design, so nothing consumes them. A quiet box stays at 7.8 to 8.05.
- Stress test of the final code (box 1, medians of 15): quiet 16.7 ms, one hog on a CPU 22.4, two hogs 24.6.

## A crash found and fixed

Symptom: one bad run in about 900 in the A/B loop (0.6 % of runs), a segfault at thread start, only on some starts. It passed every 13-run check, the official harness and the board. Cause: the program's memory was mapped MAP_FIXED at 1 << 30, and x86-64 kernels start a static binary's heap at a random page within 1 GB above the program; on 24 of 4000 measured starts the mapping landed on the heap and zeroed main's thread pointer. Found by making the A/B loop log every bad run's exit code and reading one core (the thread pointer and the fault address pointed there); 800 runs after the fix, none. Fixed in v60 by moving the mapping to 3 << 29, above the heap's range. Since then crash.sh runs each variant 100 times, and any bad run stops the work.

## What did not work

Rows without a note are box 1. "Older" rows are box 2, kept short.

| What | Result | Why dropped |
|---|---|---|
| Hand-written report with no stdio | 16.74 vs 16.68 ms, S +8 | the report is 0.3 ms of a phase the unmapping sets anyway |
| Free-slot test moved out of line | 2.2 fewer instructions per record, no time, S +11 | not worth the lines |
| AES keys as memory operands | +3.2 M instructions | the compiler uses the freed registers worse |
| Two plain bound tests instead of one | +8.4 M instructions | slower |
| Shift counts mod 64 | +6.5 M instructions | slower |
| Rows by pointer in drain | no gain | kept the index form |
| Dropping the agg field from Ctx | S -1, +0.07 ms | not worth it |
| Plain pieces in file order, not stretch order | S -2, +0.03 ms | kept the stretch order |
| 18 compiler flag sets (live-range shrinkage, IRA regions, scheduling, alignments and more) | cycles 337 to 342 M against 339.6 M | none clear of noise |
| PGO trained on the full data | 16.92 vs 16.72 ms | no gain now (it gave -0.24 at v53) |
| RUN 32, 48, 96, 128 | 16.82, 16.81, 16.86, 16.95 vs 16.72 at 64 | 64 stays |
| Prefetch distance 384, 640, 768; one prefetch | 16.76, 16.82, 16.82, 16.84 vs 16.72 at 512 | 512 and two prefetches stay |
| BATCH 64 | 24.45 ms | the rings fill |
| RING 3 batches, 630 buckets to fit | 16.43 vs 16.27 ms | the ring was not the limit |
| Senders write straight into the ring, no batch and no memcpy (RING 3 batches, same memory) | 16.38 vs 16.27 ms, 2.6 M fewer instructions, 1.1 M more cycles | the copy was only 1 % |
| The same with RING 2 batches, 672 buckets | 21.25 ms | the rings fill |
| Clone order reversed, page-table split unchanged | 16.29 vs 16.29 ms, M unchanged | only with the new split (v65) |
| One pass with no Rec staging | +2.2 ms | load stalls hitting L3 on the region table |
| Cursors kept in arrays | +0.7 ms | memory-carried positions |
| Three cursors per piece | no gain | complexity for nothing |
| PIECE 256 KB | +0.8 ms, fewer cycles | rings fill: 293 context switches vs 40 |
| -mtune=alderlake/znver4, -march=alderlake | +0.3 to +0.45 ms | kept x86-64-v3 |
| MADV_POPULATE_READ per piece | +0.2 ms | |
| -fno-tree-slp-vectorize | +0.25 ms | |
| Drain in one loop without its prefetch | +0.2 ms, S -4 | not taken |
| BUCKETS 590 | +0.38 ms | |
| Pinning main after thread start | M unchanged | |
| Threads parsing while main faults, drains gated (v29) | 17.99 vs 18.01 ms, +5 lines | dropped |
| -O2, -fno-gcse, -fsched-pressure, -fira-loop-pressure, -fno-ivopts | 18.26 to 18.35 vs 18.24 | no gain over -frename-registers |
| Prefetch in the old pass 1 | -0.2 to 0 ms, cycles flat | unclear, dropped |
| Pinning threads | +0.2 ms | worse |
| Merging the scan state into Ctx | +0.3 ms | rejected, though it saves 6 lines |
| Vector per-country totals | no gain | |
| Shared already-sent cache | dead: 90 % of messages are new | |
| Older: pread instead of mapping (v22) | 25.0 vs 24.1 ms, board 25.8 | the kernel's copy cost 80 M cycles vs 53 M |
| Older: MADV_POPULATE_READ up front | 28 ms | threads queue on the mapping lock |
| Older: v44 to v48, winning back the per-CPU charge batches | best M 7.9 to 8.2 MB, T 20.8 to 22.2 | M saved cost more T than it paid in K |
| Older: counting quotes in pass 1 (v40) | +35 M instructions, +0.27 ms | |
| Older: region ids read 8 digits at a time | +0.4 ms | each line's start waits on a vector compare |
| Older: v50, shared name ids, one intern table | 23.00 vs 20.35 ms | kernel cycles nearly doubled |
| Older: v51, one pass, records read from their start | 22.95 ms, 14 % fewer instructions, more cycles | each record's start waits on the last one's end |
| Older: v52, two to eight interleaved streams | 23.8 to 25.7 ms | did not recover v51 |

Branch misses stayed near 4 % of cycles at v18 and lower since: branch-free rewrites were never the lever; the program is bound by instruction count.
