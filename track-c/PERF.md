# Track C: performance log

How join.c got to its speed, with the measurement behind each step. Board runs are named by their run ID; everything else was measured on our test machine (box 2) and says so.

## Where it ended

The final code is v53: v43's code (its comments fixed as v49) with waits that sleep after 128 pauses (Steps, v53), so a stray process on the scoring box costs it about 7 ms instead of 300. Best board run of this code: run-5e3b2bc95cec (commit 8442d94, v49's code), T 21.16 ms, M 9.89 MB, S 417, K 43.97; its other runs run-be7fc43b744a 21.31 ms (K 43.27) and run-14a8fd87f9be 21.74 ms (K 43.04). v53's first board run, run-739c6cea137d, read 27.3 ms on a busy board: our A and D runs in the same batch were 7 and 10 % slower than their best. The start: the reference, T 14.44 s, K 1. The steps that cut T most (box 2, fast loop): v19 -2.2 ms (pass 1 without quote counts), v24 -1.55 (messages staged 128 per owner), v21 -1.45 (AES-NI name hash), v20 -1.1 (4-slot SIMD buckets), v31 -0.63 (region table prefetch). Evidence: board runs in the table under "Where it stands", per-step numbers under "Steps", failures under "What did not work (box 2)".

## Where it stands

The reference takes 14.44 s (K 1).

| Run | T | M | S | K |
|---|---|---|---|---|
| Rust version, board run-fff3b356a7d3 (commit 2306aa9) | 28.1 ms | 9.6 MB | 585 | 36.48 |
| join.c v21 on the board, run-fc3513efb0d2 (commit 816a29f) | 27.69 ms | 10.94 MB | 419 | 37.63 |
| join.c v27 on the board, run-ca7e7d104000 (commit ec1791c) | 26.56 ms | 10.65 MB | 443 | 38.31 |
| join.c v39 on the board, run-cc3a05552f65 (commit c04e8b3) | 21.74 ms | 10.39 MB | 425 | 42.83 |
| join.c v43 on the board, run-a355185f438a (commit 09f081f) | 22.48 ms | 10.24 MB | 417 | 42.35 |
| join.c v49 (v43's code, comments fixed) on the board, run-14a8fd87f9be (commit 6b75bfd) | 21.74 ms | 10.3 MB | 417 | 43.04 |
| join.c v49 on the board, run-be7fc43b744a (commit 4e793b1) | 21.31 ms | 10.51 MB | 417 | 43.27 |
| join.c v49 on the board, run-5e3b2bc95cec (commit 8442d94, best run) | 21.16 ms | 9.89 MB | 417 | 43.97 |
| join.c v53 on the board, run-739c6cea137d (commit 55c6a91, a busy board) | 27.31 ms | 10.27 MB | 418 | 38.39 |

The table holds each version's first board run; "Steps" below cites the re-push of 2026-11-01 (v21 24.9 ms, v27 24.6 ms), the same code 2 to 3 ms faster on another run of the shared board. The same code on box 2, 5 local runs of the organisers' harness each (no board run): v27 22.74 ms, M 10.74 MB, K 41.3; v39 20.93 ms, 10.60 MB, K 43.5; v43 20.78 ms, 10.69 MB, K 43.6. Box 2 is a rented i5-13500 like the scoring box, turbo off, running the organisers' harness. The board ran v21 and v27 3.3 to 3.8 ms slower than box 2, v39 0.8 ms slower, v43 1.7 ms and v49 0.5 to 1.0 ms slower (each a single board run against the harness's middle three of five); "Other processes on the scoring box" below has what we know about why. The two v49 rows are the same code, v43's with its comments fixed, under two commits (6b75bfd, 4e793b1); the submitted result is run-be7fc43b744a (README, "The result"). What changed between the commits is not recorded here.

Every board run named in this file, in one place. First runs (the table above): Rust run-fff3b356a7d3 28.1 ms; v21 run-fc3513efb0d2 27.69; v27 run-ca7e7d104000 26.56; v39 run-cc3a05552f65 21.74; v43 run-a355185f438a 22.48; v49 run-14a8fd87f9be 21.74 and run-be7fc43b744a 21.31. Re-push of 2026-11-01 (Steps): v10 run-34ea5e6b408e 33.6; v18 run-66e847fccb66 30.1; v19 run-f963f4408506 27.8; v20 run-b49c7f2b2d8c 26.4; v21 run-9767cb14f1a6 24.9; v24 run-b39ff361cb97 23.3; v27 run-709c5622f461 24.6; v31 run-5d3de098bf35 22.9; v32 run-fc8350371cd8 84.6, the slow shared-board run; v39 run-33798317abda 21.6; v43 as v49 run-be7fc43b744a 21.31. Cited in "What did not work": v22 run-a4105d07d7e5 25.8; v44 run-1cee055fcf78 22.8; v48 run-9937f1928cb0 22.8; v50 run-a727cc41ae99 108.9, a slow run; v51 run-5c0282f45652 24.0.

## Measuring

- **The harness.** The organisers' run_submission.py (5 runs: T is the mean of the middle three, M the peak) and static_axes.py (S) run on box 2 against the full seed-1 data.
- **Fast loop.** A static build timed under `taskset 0-11`, 20 runs per round, median; A/B comparisons alternate the variants over 3 rounds. Validated against the harness: the Rust binary measured 27.4 ms this way against 28.1 ms on the board. The same triangle at the final code: 20.31 ms on the fast loop, 20.78 on the harness, 21.31 on the board (v43, whose code v49 is). Variants that came out the same speed measured 24.05-24.13 against 24.1 ms, and 20.62-20.67 against 20.64, which is the spread a kept delta must clear; v37's 0.3, v38's 0.13 and v42's 0.17 ms do.
- **Correctness.** Every variant is checked against 8 generated seeds (tiny, sample and full sizes) and the two published data sets before it is timed.
- **Where the time goes.** `perf stat` instruction and cycle counts per variant; `perf record` on user and kernel code; variants with parts of the program switched off (the "split" rows below); a per-thread timeline from timestamps written into a copy of the program; and the cgroup's memory statistics sampled during a run, to split M into our pages and the kernel's.

## The design and why

v-numbers name the versions listed under Steps below.

The comment at the top of join.c describes the program. The measurements behind its main choices:

- **Floor.** Reading and scanning all 202 MB with 12 threads and nothing else: 5.6 ms with pread in 64 KB blocks, 7.6 ms mapped. Carry-less-multiply quote masks on top cost nothing (still 7.6 ms): the scan is memory-bound, so the parse can afford SIMD work per byte.
- **Twelve threads taking 256 KB pieces from a shared counter.** On a quiet box a fixed twelfth per thread was as fast (24.05-24.13 against 24.1 ms at v21); the pieces are for a box that is not quiet. Consecutive pieces are 2 MB apart (piece i is the (i / n)th piece of 2 MB stretch i % n): with neighbouring pieces, threads that faulted in the mapping at the same time took the same page-table page's lock, and a kernel profile of v37 had 24 % of kernel cycles spinning on that lock and 13 % in `filemap_map_pages`. Spread out, kernel cycles per run went from ~70 M to ~60 M and T down 0.13 ms (v38).
- **Pass 1 finds record ends only.** Spec 2.2 makes an LF inside a quoted note always followed by a lowercase letter or a quote, and a record's own LF by a digit, a BOM or the end of the file. So pass 1 lists the LFs not followed by `[a-z"]`, three AVX2 compares per 32 bytes with no quote state, and a thread finds its piece's first record without reading anything before it. Against v27's pass 1, which listed every separator outside quotes: 43 instead of 79 instructions per record (v28).
- **Pass 2 reads fields from both ends.** The region and the amount follow the record's last two commas, which its last 32 bytes always hold; the id runs to the first comma; the name is quoted, and the note is what lies between. 12.2 % of names contain a `""`: a cold path for them cost 8 % of cycles, so the name's end is found branch-free instead, as the first non-quote byte after an odd number of quotes (a carry-less multiply turns the quote mask into running parity).
- **Region lookups first, prefetched.** Pass 2's first loop reads the three numbers (in one AVX2 multiply chain), computes each record's region table index and prefetches that byte; the second loop does the rest. In v30 the table load was 17.5 % of all cycles; the prefetch took T from 22.30 to 21.67 ms (v31).
- **Names go to an owner thread.** A name's hash picks its owner; the reader sends an 8-byte message through a ring, 128 messages at a time (8 at a time: 1.5 ms slower; 16: 0.9; 32: 0.6; drain every 8 passes instead of 4: 0.2 ms faster). One table shared by all threads with an atomic OR per bit (measured in the Rust version) took 58.2 ms against 31.5 ms, with 3.03 M cross-core snoop loads against 0.23 M.
- **A sender whose ring is full files its own inbox while it waits.** So two threads that wait on each other both advance, and no batch needs a second home. This replaced v27's per-owner overflow queue (v32: 21.70 to 21.16 ms, together with rings in static memory and the output in code order without a sort; 53 fewer lines).
- **A name is 56 bits of its hash, not its bytes.** So a message is one 8-byte word, the hash with the country in its low byte, and an owner compares four stored names at once with one AVX2 compare (`find`) without reading any name's bytes, which sit in another core's part of the file. The price is a 1-in-10-million chance per run that two names merge (join.c's header).
- **AES-NI name hash.** Three AES rounds over 32 bytes at a time, cut to 64 bits: fewer instructions than the multiply hash it replaced (v21: 1.54 G to 1.17 G per run).
- **Memory.** 4 KB pages only (`MADV_NOHUGEPAGE`): with transparent huge pages on, first touches zeroed 2 MB pages, 5.4 ms. One malloc arena (`M_ARENA_MAX` 1): with one per thread, each thread's first allocation mapped and trimmed its own arena and the threads queued on the process's mapping lock (`osq_lock` 6.6 % of kernel cycles): 0.17 ms (v39). Peak M is our 7.2 MB of pages, 1.0 MB of the kernel's (page tables 0.65) and 1.5 to 2 MB that the cgroup charges in per-CPU batches.
- **The end.** Once every total is final, main writes the output while the other 11 threads each drop an eleventh of the mapping; otherwise exit tears down 49,000 page-table entries on one CPU (2 ms). Overlapping the two: 0.3 ms (v37).

## Steps

Box 2, fast loop, median T, then the live board's T for the same code. On 2026-11-01 we pushed the main steps to the board again, in order, one run each: each step below with a run ID has that board measurement, and the steps without one were measured on box 2 only. Until the freeze the board shares its machine with other teams' runs, so a single run can be slow: v32's 84.6 ms is one such run, and v39, with the same sending scheme, measured 21.6 ms 6 minutes later.

- v10, 31.6 ms (harness), board 33.6 ms (run-34ea5e6b408e): an early C version of the owner design.
- v18, 29.4 ms, board 30.1 ms (run-66e847fccb66): hot-loop state in locals (aliasing stores to struct fields had cost ~10 % of instructions), 8-slot SIMD buckets.
- v19, 27.2 ms, board 27.8 ms (run-f963f4408506): pass 1 without quote counts; pass 2 split into a region loop and the rest; quotes counted from field bytes; branch-free name hash.
- v20, 26.1 ms, board 26.4 ms (run-b49c7f2b2d8c): 4-slot SIMD buckets, 56-bit name identity, the owner stores 16 bits per name instead of the key.
- v20b, 25.55 ms: name hash fast path without a loop: stack spills in the record loop 47 to 2.
- v21, 24.1 ms, board 24.9 ms (run-9767cb14f1a6): AES-NI name hash.
- v24, 22.55 ms, board 23.3 ms (run-b39ff361cb97): messages staged 128 per owner (was 8), rings of 256, owners drain every 8 passes.
- v27, 22.63 ms, board 24.6 ms (run-709c5622f461): 256 KB pieces from a shared counter, and sends that never wait. No faster on a quiet box; kept because it holds up when another process takes a CPU (below).
- v28, 22.7 ms: pass 1 lists record ends only, pass 2 reads fields from both ends. The same 1.14 G instructions as v27: pass 1 lost 36 per record, pass 2 gained 25. Kept because pass 2's new shape is what v30 and v31 cut down.
- v29, 23.2 ms: the three numbers in one AVX2 chain. 1.10 G instructions but slower: GCC spilled the bases of the per-record arrays and rebuilt 64-bit constants inside the loop.
- v30, 22.30 ms: the per-record array inside the thread's context (one base register), the id's comma by a vector compare: 1.04 G instructions, 581 M cycles (v27: 614 M).
- v31, 21.67 ms, board 22.9 ms (run-5d3de098bf35): region table prefetch.
- v32, 21.16 ms, board 84.6 ms (run-fc8350371cd8, the slow run above): senders drain while their ring is full, no overflow queue (below).
- v33, same speed, 409 code lines.
- v37, -0.3 ms: main writes the output while the other threads unmap.
- v38, -0.13 ms: pieces 2 MB apart.
- v39, 20.64 ms, board 21.6 ms (run-33798317abda): one malloc arena.
- v42, 20.45 ms: a new minimum or maximum amount is a branch, not a compare and store per record (a thread sees ~900 amounts per country, so the branch is almost never taken): -0.17 ms. `pthread_barrier_wait` instead of a spinning barrier: same speed, 7 fewer lines.
- v43, 20.31 ms: each record's region table index lives in its record entry, not in an array of its own: one base register fewer in the first loop, which had kept 5 values on the stack (now 2). 1.02 G instructions. Board: its first run, run-a355185f438a, 22.48 ms (v39's first run, run-cc3a05552f65, 21.74 on the shared board); v49, the same code with its comments fixed, 21.74 (run-14a8fd87f9be) and 21.31 (run-be7fc43b744a), and v49's code again at 21.16 (run-5e3b2bc95cec, the best).
- v53, the same speed on a quiet box: every wait spins 128 pauses, then sleeps 10 us. On 2026-11-02 the board ran v43's unchanged code at 326.8 ms (run-2a0d82be6829), 15 times its usual 21 ms, while the teams' runs just before and after ours were normal. One busy loop on a CPU in 0-11 reproduces it (below).

Wall time with parts switched off (v39): pass 1 alone 6.4 ms; then the numbers and region index +3.5, per-country totals +1.6, name hash +1.55, note lengths +1.55, sending names +1.5 (owners that take messages off the ring without filing them: 16.15 ms), owners filing them +4.5. Filing costs 59 instructions and 37 cycles per message. The same at v21: pass 1 alone 7.25 ms, the same as reading and scanning the file with nothing else (7.6 ms); then parsing and per-country sums +7.7, note lengths +0.8, name hash +0.85, sending names +3.0, owners filing them +4.5.

The timeline of v33, from timestamps the threads record: the region table is complete at 0.48 ms, parsing ends at 19.4 to 19.7 ms, then unmapping takes 0.4 ms and the output 0.3 (overlapped since v37), and the exit 0.6. A static binary that does nothing takes 0.16 ms; creating and joining 12 threads 0.41 ms.

## Other processes on the scoring box

With a busy loop on one of CPUs 0-11 for 1 ms in every 5 (box 2, fast loop, medians of 2 rounds):

- v24, fixed twelfths, a sender waits while its owner's ring is full: 22.55 ms quiet, 26.3 to 26.9 ms with the noise. One thread kept off its CPU stalls every thread that sends to it.
- v26, pieces and an unbounded per-owner queue on the hot path: 23.8 quiet, 24.2 with noise.
- v27, pieces, fixed batches, and an overflow queue used only when a ring is full: 22.66 quiet, 24.0 with noise.
- v39, pieces, and a sender that drains while it waits: 20.64 quiet, 24.0 with noise.

A busy loop on one CPU for the whole run made v24 25 times slower (546 ms) and v26 8 times (172 to 192 ms).

v43's code does the same: 331 ms with a busy loop on CPU 3 for the whole run, 439 ms with two loops anywhere in 0-11, against 23.3 ms quiet (our i5-13500 bench machine, static build under `taskset 0-11`, 40 interleaved rounds, medians). The board met it once, run-2a0d82be6829 at 326.8 ms. Every thread waits on every other, so the threads that only spin keep all twelve CPUs busy, and the thread that shares a CPU with the loop runs only in its turns of a few milliseconds while the rest wait for it. v53 sleeps 10 us after every 128 pauses of a wait: the freed CPU takes the stalled thread. Same rounds, quiet / one busy loop / two: spin only (v43) 23.3 / 331 / 439 ms; 128 pauses, then 10 us (v53) 23.3 / 30.0 / 32.9 ms; 256 pauses, then 20 us 23.3 / 31.0 / 34.2 ms; 512 pauses, then 20 us 23.4 / 32.3 / 36.3 ms. Two longer settings, 20 rounds: 1,024 pauses then 50 us, 35.8 and 41.4 ms; 4,096 then 100 us, 58.0 and 70.7 ms. The output matched on seed 1 and a fresh seed, and with the program held to 1, 4 and 12 CPUs.

On a quiet box, v27 and v39 each had 9 slow runs in 800 (more than 1.2 times the median), 4 or 5 of them between 60 and 180 ms: a thread that loses its CPU holds up the threads that send to it, with or without the overflow queue. The harness drops the slowest of 5 runs and repeats a batch whose middle three differ by more than 50 ms.

v27 did not close the board gap: 3.8 ms slower on the board than on box 2, against 3.3 ms for v21, which had no defence against this noise. What matches the gap is another process using the shared L3: a cache-bound loop on the E-cores (CPUs 12-19) made v27 and the Rust version 3.3 to 3.8 ms slower alike, while a memory-bandwidth loop there cost nothing. Until the freeze the board's runs share the scoring machine; the final runs have it to themselves (organisers' FAQ), so v39 keeps the 2 ms it wins on a quiet box. v53 is for a stray process during the final runs: one would cost it 7 ms, not 300.

## What did not work (box 2)

- `-march=alderlake` or `native` instead of x86-64-v3: 0.4 ms slower. Profile-guided optimisation (`-fprofile-use`, at v27): 22.68 against 22.69 ms.
- Reading with pread instead of mapping: 34.8 against 33.3 ms in the full program, although pread wins the bare scan; again at v21 (v22): 25.0 against 24.1 ms, and on the board 25.8 ms (run-a4105d07d7e5) against v21's 24.9. The kernel's copy took 80 M cycles against 53 M for mapping and unmapping.
- Each thread populating its part of the mapping up front (`MADV_POPULATE_READ`): 28 ms, the threads queue on the mapping's lock.
- Main touching all memory before starting the threads, which avoids the cgroup's per-CPU batches (~2 MB of M): 1.6 to 2.8 ms slower, all of it serial. Main allocating the owners' tables (+0.55 ms) or touching the rings and totals (+0.15 ms) before the threads start: slower too.
- Winning back the per-CPU batches after v43 (v44 to v48). The kernel charges a CPU's first 4 KB page fault to the cgroup 64 pages ahead, so twelve threads that each fault a little count up to 3 MB they never use; a 2 MB page is charged exactly. M below is measured the board's way (Docker, the parent cgroup's peak), where v43 has 10.3 to 10.7 MB.
  - v44, main faults in every page the threads write before starting them: M 9.17 MB, T 1.3 ms slower (6.8 MB faulted in on one CPU). Board: 22.8 ms, M 9.74 MB, K 42.11 (run-1cee055fcf78).
  - v45, the tables the threads share (name tables, rings, totals, and the region table compressed to a bit per id plus a country per line) in three 2 MB pages, one faulted in by each of three threads: M about 9.1 MB, T 20.79 against 20.31 ms. The threads cannot fill the region table until its 2 MB page is in, which takes 0.2 to 0.35 ms. v47 got main to the barrier 0.27 ms sooner and changed nothing (20.73 ms): the region table was still the last thing done. On the organisers' harness v47 measured T 20.99 ms, M 9.29 MB, S 495: the 78 extra lines and the slower start cost more than the 1.4 MB saved, K about 0.3 % below v43.
  - v48, the shared tables in 4 KB pages, faulted in 256 KB (one CPU's batch) at a time by whichever thread is free: the lowest M of all (7.9 to 8.2 MB in a bare cgroup, against 9.6 to 9.9 for v43) but T 22.15 ms. Twelve threads faulting in fresh pages at once went no faster than one, about 0.12 ms per MB. Board: 22.8 ms, M 8.83 MB, K 42.15 (run-9937f1928cb0), below v49's 43.27.
  - v46, records.csv read with pread in 32 KB pieces instead of mapped (no page tables to charge): M about 9.0 MB, T 22.2 ms.
- The owners' tables mapped with `mmap` instead of `aligned_alloc`: 0.7 ms slower, 13 % more user cycles. Letting GCC inline the cold path for long names: 29 M more instructions, 0.8 ms slower.
- `_Alignas(64)` on the first field of the per-country totals: it aligns every field of the declaration, making the struct 512 bytes: 1.2 ms slower.
- Name rows zeroed up front instead of when first used: no faster, M +0.4 MB. The threads adding up the per-country totals in parallel: no faster.
- The output formatted by hand instead of with fprintf: 0.15 ms faster for 8 more lines, which is about even in K.
- Counting each record's quotes in pass 1, so that pass 2 gets a note's unescaped length by arithmetic instead of reading the note (v40): 35 M more instructions, 0.27 ms slower.
- Region ids read 8 digits at a time: 0.4 ms slower. Each line's start then waits on a vector compare, where the byte loop ran ahead. Threads that start each other in a tree instead of main starting all eleven: no faster, and more slow runs.
- Pieces of 128 or 64 KB instead of 256 KB: no change at v39 (20.62-20.67 against its 20.64 ms); when a thread runs out of pieces, its core's other thread speeds up. Mapping each piece in one call (`MADV_POPULATE_READ`): +0.05 ms. Prefetching the owners' buckets a loop ahead: no change.
- Region table misses, before the prefetch: a probe without the lookup (wrong output) was 1.6 ms faster, but the same lookup into a 256 KB table recovered only 0.55 ms, so a compact table was not built.
- Rings of 128 messages: 0.2 ms slower than 256. Pass 1 over 4 KB instead of 16 KB: 0.2 ms slower.
- Without 64-byte message lines (one 8-byte store per message): 7 ms slower. Non-temporal stores for the lines: no change.
- Two interleaved input streams per thread: slower. Deferring the per-country updates through a record array: slower.
- Two redesigns on the last day, both slower than v43 (box 2, 3 alternating rounds, all 10 data sets correct), each pushed once to the board for this log:
  - v50, shared name ids: one 1 MB intern table shared by all threads (insert by compare-and-swap; the inserting thread owns the name), 4-byte messages, owners only set a country bit. 23.00 against 20.35 ms in these alternating rounds; v43's own step runs measured 20.31 ms and 1.02 G instructions. User instructions went up (1,081 M against 1,048 M in the same rounds) and kernel cycles nearly doubled (91 M against 49 M, page faults on the shared table and the bit rows), although L2 misses fell from 0.86 M to 0.15 M. Board: 108.9 ms (run-a727cc41ae99), a slow run of the kind described above.
  - v51, one pass: no record-end pass; each record is read from its start, and the note's end found with the same quote-parity multiply as the name's. 22.95 ms with 14 % fewer instructions (905 M) but more cycles (608 M against 557 M): each record's start waits on the previous record's end, and top-down counts show fetch latency at 429 M slots against 251 M. Moving the name hash back into the second loop (v51b) gave 24.8 ms; two to eight interleaved streams per thread (v52), 23.8 to 25.7 ms. Board: 24.0 ms (run-5c0282f45652).
- Branch misses: about 4 % of cycles at v18, so branch-free rewrites were not the lever; the program is bound by instruction count (about 60 % of slots retiring, about 2 instructions per cycle per thread).
