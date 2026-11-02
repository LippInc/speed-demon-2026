# Track C: performance log

We started in Rust and moved to C on November 1. For about 40 versions the work was cutting branches and instructions, and the numbers came down slowly, from 31.6 ms at v10 to 22.6 ms at v27. Then v51 showed we had the wrong target: 14 % fewer instructions and more cycles. What bound the program was the chain of loads from one record to the next, and branch misses had never been the lever. So on the last day v57 threw out the pass over record ends and read each record once, front to back, with two cursors whose waits overlap. From there instruction count mattered again, and v65 read 14.32 ms on the board. What did not work is listed near the end.

How join.c got to its speed, with the measurement behind each step. Rows named by a run ID are board runs. Local rows from before 2026-11-02 say "box 2"; today's local rows are box 1 (i5-13500, 6 P-cores with HT at a fixed 2.5 GHz, the same machine type as the board).

## Where it ended

The final code is v66. It reads records.csv once per record with no pass 1, keeps each thread's state on three huge pages faulted in by main and one helper, and identifies distinct names by owner threads exchanging 8-byte messages (join.c's header walks the design). The board run run-b73db7cdb927 measured T 14.475 ms, M 8.15 MB, S 396, K 55.68; the reference takes 14.44 s, K 1. That run is the submission named in both this file and README.md. v66 is v65 with its layout and comments cleaned for reading and mmap failures checked: the same hot loops, which also read T 14.32 ms, K 56.60 as v65 (run-6d3cdfadd754). A quiet box reads lower than a busy board: until the final runs the board shares its machine, and T moves with the load. M moves too, we think by charge batches left in the cgroup, a hypothesis ("Other processes on the scoring box").

## Where it stands

The reads of v54, v56 and v60 to v62 are listed under the table by commit: our saved board reads hold no run ID for them.

| Version | Commit | Board run | T | M | S | K |
|---|---|---|---|---|---|---|
| join.c v49 (v43's code, comments fixed) | 6b75bfd | run-14a8fd87f9be | 21.74 ms | 10.3 MB | 417 | 43.04 |
| join.c v49 | 4e793b1 | run-be7fc43b744a | 21.31 ms | 10.51 MB | 417 | 43.27 |
| join.c v49 | 8442d94 | run-5e3b2bc95cec | 21.16 ms | 9.89 MB | 417 | 43.97 |
| join.c v53 (v49's code, waits that sleep) | 55c6a91 | run-739c6cea137d (a busy board) | 27.31 ms | 10.27 MB | 418 | 38.39 |
| v59 | 397ee86 | run-02bcdc588c09 | 16.48 ms | 7.92 MB | 392 | 52.56 |
| v59 (same code) | bfcd0c7 | run-591a67a034d0 | 17.02 ms | 7.86 MB | 392 | 51.79 |
| v58 (a slow read) | fd2350d | run-84ebbf3d52b8 | 21.65 ms | 9.24 MB | 382 | 44.65 |
| v63 | a2b2bdd | run-a5c4d0536268 | 14.754 ms | 7.80 MB | 377 | 56.05 |
| v63 (same code) | a9af0b7 | run-aeb5ee4b7a28 | 14.921 ms | 8.29 MB | 377 | 55.05 |
| v65 | 6cafc7e | run-6d3cdfadd754 | 14.32 ms | 8.07 MB | 373 | 56.60 |
| v65 (same code) | 3fa85cb | run-ec315dedca74 | 14.446 ms | 7.92 MB | 373 | 56.56 |
| v65 (same code, a busy board) | 3d9eece | run-f269418aae73 | 17.674 ms | 7.90 MB | 373 | 51.17 |
| v66 | 5737a0d | run-b73db7cdb927 | 14.475 ms | 8.15 MB | 396 | 55.68 |
| v66 (same machine code, S 382) | 69e21a6 | run-a1188d6eeb3d | 14.75 ms | 8.10 MB | 382 | 55.53 |
| v66 (same machine code, a memory spike) | aaffcdd | run-f0e57703613f | 15.995 ms | 10.02 MB | 382 | 51.10 |

- v54: commit 4ba13ab, T 21.19 ms, M 7.91 MB, S 389, K 46.42.
- v56: commit db1bb02, K 47.82.
- v60: commit 78587c9, T 16.23 ms, M 7.89 MB, S 394, K 52.96.
- v61: commit 73fcc4e, T 16.17 ms, M 10.24 MB (spike, "Other processes on the scoring box"), S 369, K 50.88.
- v62: commit 6ca940a, T 15.53 ms, M 8.01 MB, S 371, K 54.48.

The reference takes 14.44 s, K 1. M is the maximum of the board's 5 runs, which is why the same code reads 7.80 and 8.29 ("Other processes on the scoring box").

## Measuring

- **abn.sh**, the local A/B runner: alternates the binaries over 15 to 31 rounds, deletes the output file before each run and checks its SHA, so a crash or a wrong output leaves no number (a bad run is logged with its exit code). Its wall time includes loop overhead: an empty static binary reads 2.93 ms in the same loop, and the program's own timeline accounts for all but 0.33 ms of the rest. Noise about +-0.1 ms at 15 to 31 rounds; code-layout luck up to 0.2 ms.
- **icount.sh**: user instructions and cycles (perf), stable to 0.1 %. Not while another job runs on the box: it once read 825 M for a 752 M binary. Then we compare with abn only; the one exception is the flag check under Build flags, marked there and only indicative.
- **A trap we hit**: a 13-run check passed a program that crashed in 0.6 % of its runs ("A crash found and fixed"). Each variant now also gets 100 runs.
- **chk.sh**, correctness: 13 runs over 6 seeds at 12 and at 1 CPU, each output's SHA against the reference.
- **Below the noise**: a few kept steps are smaller than the noise above (two prefetches -0.08 ms, v65's -0.06 ms). They cost no lines, and v65 is kept for its memory figure.
- **Board figures**: T is the mean of the middle three of 5 runs (fastest and slowest dropped), M the maximum of the 5.
- **K** = (Tref/T)^.5 (Mref/M)^.2 (Sref/S)^.15; M stops paying at 7.35 MB, so there is no reward below that.
- **S** is code lines counted after clang-format LLVM (merging lines does nothing; wraps caused by trailing comments do count).

## The design and why

- **One read per record, no pass 1.** Earlier versions ran a pass 1 that listed record ends with three AVX2 compares per 32 bytes; it was 13 % of all instructions. Now each record is read once, front to back, from one quote mask of 64 bytes starting at b + 6, past most ids: for 97 % of records the mask reaches past the note. A carry-less multiply turns the mask into "follows an odd number of quotes", which picks the commas after the name and the note however many "" they hold. Only the first record of a piece needs the LF rule (spec 2.2: a record's own LF is followed by a digit, a BOM or the end of the file), and that test runs only at a piece's edges, three times per piece, off the hot path. (A per-record BOM test was another matter: as a cmov it cost 0.68 ms at v58, because the chain from one record to the next binds, and v63 removed it from parse.)
- **Two cursors per piece.** One record's start depends on the last one's end, a wait of about 50 cycles, so each piece is read from two places in turn and the two waits overlap. A run of 64 records is parsed first, staged in Rec; a second loop then does the lookups, totals and messages, each record's region-table entry prefetched (at v31 the prefetch took T from 22.30 to 21.67 ms). One pass with no staging was 2.2 ms slower (the extra cycles were load stalls hitting L3 on the 1 MB region table); the two-cursor shape was 0.8 ms faster than v56. The id's length is kept per cursor because spec 2.2 says ids only grow.
- **Owner threads for names.** The name's hash low byte picks its owner; the reader sends an 8-byte message (the hash, with the country in the low byte) through a lock-free ring, 128 at a time (8 at a time: 1.5 ms slower). The owner keeps a private table: 16 tags per bucket on one cache line (BUCKETS 652; 97 % of messages hit in the first bucket), and one bit per country in the row, set and counted at once by bts and sbb. One table shared by all threads took 13 % longer in C (23.00 against 20.35 ms, v50) and almost twice as long in the Rust version (58.2 against 31.5 ms). The 16-tag bucket was -0.22 ms; the owner from the hash's low byte, -0.17.
- **Three huge pages at a fixed address (3 << 29).** All memory the threads use is one mapping, so nothing else lands on extra huge pages. Only main and one helper thread fault memory in: a thread on another CPU that charges even one page costs the cgroup a 256 KB per-CPU charge batch. The other threads fill the region table meanwhile (all of it before the barrier lies on the first huge page): -0.35 ms together with the spin barrier. The fixed address must lie above the program end plus 1 GB ("A crash found and fixed"); the low address itself was -0.15 ms, and starting the mask at b + 6 another -0.15, two prefetches (b + 512 and b + 576) -0.08.
- **Plain clone threads on 4 KB stacks.** They share main's thread-local block and use nothing of it but errno: no memory for a TLS block or a thread descriptor, and the stacks use at most 640 bytes. Earlier pthread version, same design: no gain from the extra memory.
- **128 KB pieces from a shared counter**, piece i spread across 2 MB stretches, so threads that fault pages at the same time never share a stretch's page-table lock (a kernel profile of v37 had 24 % of kernel cycles spinning on that lock; spreading pieces out cut kernel cycles from ~70 M to ~60 M). 256 KB pieces: +0.8 ms, the rings fill (293 context switches against 40).
- **The end.** Once every total is final, main writes the output while the other 11 threads each drop an eleventh of the mapping; otherwise exit tears down 49,000 page-table entries on one CPU (2 ms).
- **Build flags.** `-mtune-ctrl=^avoid_false_dep_for_bmi` stops gcc adding an xor before BMI instructions such as tzcnt to break a false dependency: 636.2 M against 663.5 M instructions and 347.3 M against 351.4 M cycles, so leaving the xor out costs nothing on this CPU (both on a busy box, where abn could not separate them, 22.29 against 22.24 ms). `-frename-registers` reads flat today (636.2 M against 637.4 M instructions, 347.3 M against 346.8 M cycles) and stays because every final figure was measured with it.

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
- v53, the same speed on a quiet box: every wait spins 128 pauses, then sleeps 10 us ("Other processes on the scoring box").

Today (box 1 unless a run ID is given). The board runs a team's latest push at most once per 5 minutes, so not every version got a read: a version with no run ID below has only its local numbers, or a read we did not save.

- v54 (4ba13ab): all thread memory on three huge pages at a fixed address, faulted in before any thread starts; raw reads in the number chain; gcc 16 (the compiler alone, -0.3 ms on the local loop). Board: T 21.19 ms, M 7.91, S 389, K 46.42.
- v55 (1b57343): one quote mask for name and note; the amount read after its comma, its point dropped by pext; no per-block bound test in pass 1. No A/B of its own: v56's figure covers both.
- v56 (db1bb02): one load for the name's hash and the quote mask, a flat staging index, a branch for the rare unquoted empty note. 865 M instructions, abn 21.3 against v53's 23.5. Board K 47.82.
- v57 (61a43ce), the redesign: each record read once front to back from one quote mask, two places per piece in turn so the waits overlap, no pass over record ends. abn 20.8, and no board read of its own that we kept: the next is v58's run-84ebbf3d52b8. In its own A/B, two cursors with Rec staging and the region prefetch were -0.8 against v56. (A/B figures here and below come from their own rounds of 15 to 21, so they do not add up exactly to the next abn.)
- v58 (fd2350d): abn 18.91. In separate A/Bs: the BOM test a branch, not a cmov, so its load leaves the chain between records, -0.68; pointers as cursors instead of offsets, -0.5; threads fill the region table while main faults memory in, with a spin barrier, -0.35; the low fixed address, -0.15. Board run-84ebbf3d52b8, a slow read: T 21.65 ms, M 9.24, S 382, K 44.65.
- v59 (397ee86): abn 18.43. In separate A/Bs: 16-tag buckets tested for a hit first, -0.22; the owner from the hash's low byte, -0.17; the quote mask from b + 6, so 97 % of notes end inside it, -0.15; two prefetches, -0.08. No hash seed; one signed compare for the tail. Board run-02bcdc588c09: T 16.48 ms, M 7.92, S 392, K 52.56; the same code again as bfcd0c7, run-591a67a034d0: T 17.02 ms, M 7.86, K 51.79.
- v60 (78587c9): the crash fix ("A crash found and fixed", map moved to 3 << 29), memory constants, build flags. Board T 16.23 ms, M 7.89, S 394, K 52.96.
- v61 (73fcc4e): plain clone threads on 4 KB stacks, BUCKETS 652, an atomic_flag lock that uses the sleeping wait, meet() replaces join, no -pthread. abn 17.99; board T 16.17 ms, M 10.24 (the spike, "Other processes on the scoring box"), S 369, K 50.88.
- v62 (6ca940a): the cold test from the mask, a vector min for the region id cap, the id stored first, the tag read straight from the message. 680 M instructions, abn 17.49; board T 15.53 ms, M 8.01, S 371, K 54.48.
- v63 (a2b2bdd), instructions 680.0 M to 614.0 M, abn 17.45 to 16.55, in order: the amount's decimal point skipped by per-lane multipliers instead of pext (666.6 M, 17.41 to 17.16); the next record found by a carry through the run of low bytes, so no BOM test in parse (653.3 M, 16.88); the id's length kept per cursor, because spec 2.2 says ids only grow (643.7 M); bit test and count with bts and sbb (634.2 M, 16.65); the bucket from the message's top two bytes (cycles 340.2 M to 336.1 M); name and note bytes added as one 64-bit field (616.3 M with the rest). Board run-a5c4d0536268: T 14.754 ms, M 7.80, S 377, K 56.05; the same code again as a9af0b7, run-aeb5ee4b7a28: T 14.921 ms, M 8.29, K 55.05.
- v64: the timeline of v63 (timestamps in a copy of the program) showed 11 threads idle for 0.75 ms while main alone faulted memory in: main's first huge page 0.24 ms, the clones 0.11, huge pages 2 and 3 0.55, the page-table pages 0.30; regions done at 0.49; parse ends 13.0, drained 13.09, report 13.39, all done 13.54. One helper thread (thread 11) now faults the third huge page and half the file's page-table pages. abn 16.66 to 16.34 (31 rounds). Memory: another CPU that charges a page costs a 256 KB batch, and the page-table pages use that batch up: median of 15 runs 7.57 to 7.67 MB. Error checks are back on open, clone and fclose.
- v65: start-up balance. The helper is now cloned first, so it starts ahead of main, which still has the other clones to do, and it takes 5/8 of the file's page-table pages: 61 of 98, inside its 64-page charge batch, so little of the batch is left over. abn 16.29 to 16.23 (25 rounds), memory peak 7.50 to 7.40 MB, no lines added. Board run-6d3cdfadd754: T 14.32 ms, M 8.07, S 373, K 56.60; the same code as 3fa85cb, run-ec315dedca74: T 14.446 ms, M 7.92, K 56.56; and as 3d9eece on a busy board, run-f269418aae73: T 17.674 ms, M 7.90, K 51.17.
- v66 (5737a0d): no speed work, for reading. The source now stands in clang-format's own layout (S counts that form anyway), comments above their lines and true to the code, clearer field names, and mmap failures stop the program with an error. The comments-and-names build has the same machine code as v65 (.text SHA compared). The mmap checks: S +6, abn 16.31 against 16.30 (25 rounds), all checks clean. Then, after a judge-style read, every comma-joined statement on its own line and corrected comments, with the machine code still the same (.text SHA compared). S +23 in all (373 to 396), about 0.9 % of K given up on purpose. Board run-b73db7cdb927: T 14.475 ms, M 8.15, S 396, K 55.68; the same machine code as aaffcdd, run-f0e57703613f: T 15.995 ms, M 10.02, K 51.10 (a slow read with a memory spike; abn is flat, so the code does not explain it: "Other processes on the scoring box").

## Other processes on the scoring box

With a busy loop on one of CPUs 0-11 for 1 ms in every 5 (box 2, medians of 2 rounds):

- v24, fixed twelfths, a sender waits when the ring is full: quiet 22.55 ms, with the noise 26.3 to 26.9 ms.
- v26, pieces, unbounded per-owner queue on the hot path: quiet 23.8 ms, with the noise 24.2 ms.
- v27, pieces, overflow queue only when a ring is full: quiet 22.66 ms, with the noise 24.0 ms.
- v39, pieces, a sender that drains while it waits: quiet 20.64 ms, with the noise 24.0 ms.

A busy loop on one CPU for the whole run made v24 25 times slower (546 ms). v43's code: 331 ms with one such loop, 439 with two, against 23.3 quiet. v53's sleeping wait (10 us after 128 pauses) reads 30.0 and 32.9 ms in the same rounds, the same speed on a quiet box.

- Board reads vary with load: while the board was busy, other teams' latest runs read 2 to 4 ms over their best. The final runs have the machine to themselves.
- M spikes on the same code with T unaffected: 7.80 and 8.29 MB for v63, 10.24 for v61. Our hypothesis, not proven: per-CPU 256 KB charge batches left by processes that ran in the cgroup before the program; only main and the helper charge in our design, so nothing consumes them. A quiet box stays at 7.8 to 8.05.
- Stress test of v65, the same hot loops as the final code (box 1, medians of 15): quiet 16.7 ms, one hog on a CPU 22.4, two hogs 24.6.

## A crash found and fixed

Symptom: one stray bad run after about 900 in the A/B loop, a segfault at thread start. (At 0.6 % those 900 runs should have held about 5 crashes; the loop only began logging every bad run after this one, so we cannot say how many it missed.) It passed every 13-run check, the official harness and the board. Cause: the program's memory was mapped MAP_FIXED at 1 << 30, and x86-64 kernels start a static binary's heap at a random page within 1 GB above the program; on 24 of 4000 measured starts (0.6 %) the mapping landed on the heap and zeroed main's thread pointer. Found by making the A/B loop log every bad run's exit code and reading one core (the thread pointer and the fault address pointed there); 800 runs after the fix, none. Fixed in v60 by moving the mapping to 3 << 29, above the heap's range. Since then crash.sh runs each variant 100 times, and any bad run stops the work. 100 runs catch a 0.6 % crash only 45 % of the time, so the final binary got a wider check before the freeze: 6,530 runs with exit code and output hash checked (4,000 quiet on the two official datasets, 1,000 on the E-cores with 12 threads on 8 CPUs, 400 beside busy neighbours, 40 on one or two CPUs, 20 from a cold page cache, 70 on 10 new seeds against the official reference). No failures. Planted bad binaries (wrong output, bad exit code, no output, segfault, hang, one that fails 1 run in 20) all failed the same check.

## What did not work

Local tries with no board run of their own: box 1, except the "Older" ones, which are box 2 and kept short.

- Hand-written report with no stdio: 16.74 vs 16.68 ms, S +8; the report is 0.3 ms of a phase the unmapping sets anyway.
- Free-slot test moved out of line: 2.2 fewer instructions per record, no time, S +11; not worth the lines.
- AES keys as memory operands: +3.2 M instructions; the compiler uses the freed registers worse.
- Two plain bound tests instead of one: +8.4 M instructions; slower.
- Shift counts mod 64: +6.5 M instructions; slower.
- Rows by pointer in drain: no gain; kept the index form.
- Dropping the agg field from Ctx: S -1, +0.07 ms; not worth it.
- Plain pieces in file order, not stretch order: S -2, +0.03 ms; kept the stretch order.
- 18 compiler flag sets (live-range shrinkage, IRA regions, scheduling, alignments and more): cycles 337 to 342 M against 339.6 M; none clear of noise.
- PGO trained on the full data: 16.92 vs 16.72 ms; no gain now (it gave -0.24 at v53).
- RUN 32, 48, 96, 128: 16.82, 16.81, 16.86, 16.95 vs 16.72 at 64; 64 stays.
- Prefetch distance 384, 640, 768; one prefetch: 16.76, 16.82, 16.82, 16.84 vs 16.72 at 512; 512 and two prefetches stay.
- BATCH 64: 24.45 ms; the rings fill.
- RING 3 batches, 630 buckets to fit: 16.43 vs 16.27 ms; the ring was not the limit.
- Senders write straight into the ring, no batch and no memcpy (RING 3 batches, same memory): 16.38 vs 16.27 ms, 2.6 M fewer instructions, 1.1 M more cycles; the copy was only 1 %.
- The same with RING 2 batches, 672 buckets: 21.25 ms; the rings fill.
- Clone order reversed, page-table split unchanged: 16.29 vs 16.29 ms, M unchanged; only with the new split (v65).
- One pass with no Rec staging: +2.2 ms; load stalls hitting L3 on the region table.
- Cursors kept in arrays: +0.7 ms; memory-carried positions.
- Three cursors per piece: no gain; complexity for nothing.
- PIECE 256 KB: +0.8 ms, fewer cycles; rings fill: 293 context switches vs 40.
- -mtune=alderlake/znver4, -march=alderlake: +0.3 to +0.45 ms; kept x86-64-v3.
- MADV_POPULATE_READ per piece: +0.2 ms.
- -fno-tree-slp-vectorize: +0.25 ms.
- Drain in one loop without its prefetch: +0.2 ms, S -4; not taken.
- BUCKETS 590: +0.38 ms.
- Pinning main after thread start: M unchanged.
- Threads parsing while main faults, drains gated (v29): 17.99 vs 18.01 ms, +5 lines; dropped.
- -O2, -fno-gcse, -fsched-pressure, -fira-loop-pressure, -fno-ivopts: 18.26 to 18.35 vs 18.24; no gain over -frename-registers.
- Prefetch in the old pass 1: -0.2 to 0 ms, cycles flat; unclear, dropped.
- Pinning threads: +0.2 ms; worse.
- Merging the scan state into Ctx: +0.3 ms; rejected, though it saves 6 lines.
- Vector per-country totals: no gain.
- Shared already-sent cache: dead: 90 % of messages are new.
- Older: pread instead of mapping (v22): 25.0 vs 24.1 ms, board 25.8; the kernel's copy cost 80 M cycles vs 53 M.
- Older: MADV_POPULATE_READ up front: 28 ms; threads queue on the mapping lock.
- Older: v44 to v48, winning back the per-CPU charge batches: best M 7.9 to 8.2 MB, T 20.8 to 22.2; M saved cost more T than it paid in K.
- Older: counting quotes in pass 1 (v40): +35 M instructions, +0.27 ms.
- Older: region ids read 8 digits at a time: +0.4 ms; each line's start waits on a vector compare.
- Older: v50, shared name ids, one intern table: 23.00 vs 20.35 ms; kernel cycles nearly doubled (91 M against 49 M, page faults on the shared table and the bit rows), although L2 misses fell from 0.86 M to 0.15 M.
- Older: v51, one pass, records read from their start: 22.95 ms, 14 % fewer instructions, more cycles; each record's start waits on the last one's end.
- Older: v52, two to eight interleaved streams: 23.8 to 25.7 ms; did not recover v51. v57 won with the same idea at two cursors. We never ran v52 against v57, so the reason is our reading: v57 added Rec staging and the region prefetch (one pass with no Rec staging cost +2.2 ms in load stalls on the region table).

Branch misses stayed near 4 % of cycles at v18 and lower since: branch-free rewrites were never the lever. Two other things bound the program: the load chain from one record to the next (v51 had 14 % fewer instructions and still took more cycles, which is why the two-cursor design exists), and after that design, instruction count (v63: 680.0 M to 614.0 M instructions, abn 17.45 to 16.55).
