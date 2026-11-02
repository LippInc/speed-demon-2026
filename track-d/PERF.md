# Track D: performance log

Track D got the least attention. The Rust program came down from 0.39 s to 0.17 s over the first days, and then we left it to work on the other tracks. About two and a half hours before the freeze we looked again: 168 ms and 107 MB, with the top of the board at half of both, so no single phase could explain the gap. We rewrote it from nothing in C instead of tuning it (d-40), and the first version read 73 ms and 60 MB on the board. The Rust steps stay below because they show how far tuning got us.

Every scored commit from the reference to our final version, with every live-board run that measured it. The sub-steps inside d-40 to d-42 are box medians from interleaved rounds. Runs of unchanged code are in the appendix at the end, numbered the same.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | d-2 floor: parallel two-pass CSR build, bidirectional early-exit Dijkstra (commit 24c6aee) | run-dac0416bdd01 | 0.39 | 161 | 57.1 |
| 9 | d-5: monotone radix heap for both search sides (commits 82a7e0e, a01a503) | run-f1748b5f56b9 | 0.37 | 161 | 57.3 |
| 11 | d-6: label-map locality, small L2 table and large table on overflow (commits 3ce5d27, 0319d5b) | run-4db71f7b98a5 | 0.36 | 142 | 57.6 |
| 19 | d-8: compact graph build without atomics, partitioned prefix sums (commits dc9a503, 92a99a2) | run-50fff4b3912a | 0.34 | 148 | 58.2 |
| 21 | d-10a: prefetch CSR rows of next 8 pops in query loop (commits 40418c5, e3ba1f6) | run-954dfa85b25d | 0.27 | 147 | 60.4 |
| 24 | d-12b: six-byte CSR entries, heavy weights in side table (commits a17e502, 4884aaf) | run-e000fdfd8967 | 0.28 | 129 | 60.6 |
| 29 | d-13b + d-14: parallel claim-BFS relabel, rev buffer reuse, static C runtime (commits aaf31e4, c970cca, 097a871, 42cd7dd, fe87585, 179f365) | run-86b2705d01d9 | 0.27 | 130 | 60.8 |
| 31 | d-15a + d-15b: queries in relabelled order, direct-mapped label table (commits 3c97338, b8d90ea, 0b2c4e6) | run-e5d1af9ec25d | 0.23 | 124 | 62.5 |
| 42 | d-18: documentation and comments update for 7068578 (commits 7068578, 28877f1, 30cd199) | run-ea1446bd3f7d | 0.25 | 124 | 61.7 |
| 56 | d-23 + d-26: code compaction (main.rs 603 -> 546 lines), parallel relabel new_id loop (commits b9c02e6, f273579, c2b5d4b) | run-aab79cef7747 | 0.21 | 124 | 63.4 |
| 61 | d-27: bucket queue, 256-settle forward probe, drop build buffers, 4-byte packed edges (commits 8ea91ed, e43fdf4, 8d7ddd4, a1f9f32, f91ea98) | run-90db6f3cc3ba | 0.19 | 110 | 64.6 |
| 63 | d-27 r1 + d-28: work-balanced search, early probe return, reverse CSR atomic placement (commits 864d392, af13865, cc8abd2, 7e2c03d) | run-e22137237ce5 | 0.19 | 111 | 64.8 |
| 64 | d-29 and later Rust lifts (tip 172a905); scored by a track C push | run-d24d95933e61 | 0.17 | 107 | 66.1 |
| 66 | d-40: rewrite in C, all phases redesigned (commits 3973446, 54d3dd5, scored at tip 3a2e0c6) | run-d88f87a079dc | 0.073 | 60 | 76.9 |
| 67 | d-41: fewer loads per edge, S 884 -> 781 (commit 3d9eece) | run-7d9634c6a75e | 0.064 | 60 | 78.6 |
| 68 | d-42: serial parts and barriers, S 787, then comments and a missing-file exit code (commits 42f0ca9, bf553b5, 9f80648, bac12be, scored at tip aaffcdd) | run-944839bcefa6 | 0.070 | 59 | 77.8 |

## How to read the table

Every push to the repository re-evaluates all four competition tracks on the official leaderboard. Many of those runs re-measure unchanged Track D binaries, triggered by changes to tracks A, B, C or root configuration; they are in the appendix at the end, and they document real host variance on unchanged code. Across 11 consecutive runs of the row 31 code (rows 31 to 41), wall time spans from 0.23 s to 0.25 s. Across 14 runs of the row 42 code (rows 42 to 55), it spans from 0.23 s to 0.31 s, with transient host-load spikes reaching 0.31 s (row 47) and 0.48 s (row 13).

Terms. The *board* is the organisers' live run; the *box* is our own machine running the official harness. We had two, *box 1* and *box 2*; a *laptop* number is only compared with another laptop number. *Official* and *fresh* are seed 1 and another seed's dataset, and *deciding K* is the lowest K over the datasets tried. *Points* are the board's speed points, which rise with the log of K: +15 % K is about +3 points. A *GO line* is the K gain a probe had to show before we built the full version.

## From the reference to step 1

The reference entry was an unoptimized single-threaded Python script (`d_traverse.py`) reading graph and query files into high-level objects: 72.69 s wall time and 893 MB RSS. Step 1 (commit 24c6aee, board row 1, run-dac0416bdd01: 0.39 s, 161 MB, 57.1 points) achieved an immediate 186x speedup by replacing interpreted script execution with compiled Rust (edition 2024, x86-64-v3, LTO enabled, zero external dependencies).

The 186x reduction came from six structural changes:
1. Zero-copy input parsing: `mmap` maps `/data/graph.txt` and `/data/queries.txt` directly from OS page cache without heap copies, while custom decimal parsing (`read_num`) reads numbers in place without string allocations.
2. Parallel CSR construction: every available core (`available_parallelism`) parses its own byte range to construct forward and reverse Compressed Sparse Row (CSR) arrays without intermediate edge buffers.
3. Bidirectional early-exit Dijkstra: searching forward from source `s` and backward from target `t` simultaneously until frontier distances meet (`lower_f + lower_b >= best`). Two search spheres of radius R/2 explore half the area of a single sphere of radius R. On the bench box, bidirectional search was 2.41x faster than forward-only Dijkstra (0.372 s vs 0.895 s official).
4. Structural fast paths: queries with identical endpoints (`s == t`), target in-degree 0, or source out-degree 0 are answered in O(1) time before initializing search state.
5. Scoped thread query parallelization: independent queries are distributed across all available CPU threads using `std::thread::scope`.
6. Dynamic per-thread scratch: label maps and heaps scale with query settlement depth rather than allocating full `n`-sized arrays, dropping resident memory from 893 MB to 161 MB.

## Decisions (Rust program, replaced by d-40)

- **Bidirectional search held over forward-only Dijkstra**: forward-only search was 2.41x slower on the bench box (0.895 s vs 0.372 s official, K 8.76 vs 13.94), proving that bidirectional expansion is essential on road networks.
- **Topological claim-BFS relabelling held**: node IDs are randomly permuted in the input; parallel BFS relabelling gives neighbours nearby ids, so their rows and labels share cache lines; locally the query phase fell 35-45 % (box T 0.271 / 0.285 s -> 0.257 / 0.267 s, d-13b).
- **Two-tier per-thread scratch held over n-sized global arrays**: 64K direct-mapped slots backed by 16K L2-sized open-addressing tables avoid O(n) reinitializations and keep label lookups within L2 cache budgets.
- **Monotone bucket queues traded for radix and binary heaps**: a 2048-bucket ring of width 32 with 64-bit occupancy bitmasks replaced logarithmic heap operations with O(1) bitwise scans.
- **Atomic reverse CSR placement traded for slab records**: building reverse CSRs via atomic in-degree counting and atomic placement into reused forward buffers eliminated ~30 MB of slab allocations, cutting reverse build time from 24-45 ms to 12-16 ms.
- **Four-byte packed edges chosen over 8-byte and 6-byte representations**: packing head node IDs into `hb` bits and weights into `32 - hb` bits with side-table overflow fits 5,000,000 edges into ~20 MB of DRAM, maximizing memory bandwidth.
- **Landmark (ALT) heuristics discarded**: computing forward and backward landmark potentials cost ~0.127 s of preprocessing and +55 MB memory, exceeding the query search time saved.
- **Intermediate memory deallocation discarded**: freeing search tables or queue buckets between queries caused malloc churn without lowering RSS, which is anchored during graph construction.

## Kept experiments (Rust program)

- **d-2 floor (commit 24c6aee, branch exp/d-2-floor)**: mmap parse into forward and reverse CSR, bidirectional early-exit Dijkstra with cheap checks first, per-thread label tables, binary heaps. Box 2: official K 14.07, fresh K 13.94, T 0.372 / 0.392 s, M 160.1 MB, S 382. Board run: row 1 (run-dac0416bdd01, 0.39 s, 161 MB, 57.1 points).
- **d-5: Monotone radix heap (commit 82a7e0e, tip a01a503, branch exp/d-5-radix)**: replaced std binary heaps with monotone radix heaps for both search directions. Box 2: official K 14.16, fresh K 14.00, T 0.349 / 0.368 s, M 163.3 MB, S 443. Query phase reduced by 21% at 12 workers. Board run: row 9 (run-f1748b5f56b9, 0.37 s, 161 MB, 57.3 points).
- **d-6: Label-map locality (commit 3ce5d27, tip 0319d5b, branch exp/d-6-labels)**: small L2-sized label table (2^14 slots, 320 KB) per thread with overflow to large table and touched-list clearing. Box 2: official K 14.56, fresh K 14.18, T 0.341 / 0.359 s, M 139.2 MB, S 490. M dropped 15%, T dropped 2-3%. Board run: row 11 (run-4db71f7b98a5, 0.36 s, 142 MB, 57.6 points).
- **d-8: Graph build without atomics, written compactly (commit dc9a503, tip 92a99a2, branch exp/d-8-compact)**: compact rewrite of d-7 partitioned build with local prefix sums and scatters. Box 2: official K 14.92, fresh K 14.49, T 0.320 / 0.339 s, M 141.5 MB, S 503. Build time fell from 104.6 ms to 85 ms (parse 27.0 ms, fwd 15.9 ms, rev 37.4 ms). Board run: row 19 (run-50fff4b3912a, 0.34 s, 148 MB, 58.2 points).
- **d-10a: Prefetch of CSR rows of next pops (commit 40418c5, tip e3ba1f6, branch exp/d-10-relax)**: on each settle, prefetch edge rows of the next 8 entries to be popped on this search side. Box 2: official K 16.25, fresh K 15.84, T 0.268 / 0.283 s, M 141.4 MB, S 512. Query phase reduced from 236.9 ms to 183.0 ms official. Board run: row 21 (run-954dfa85b25d, 0.27 s, 147 MB, 60.4 points).
- **d-12b: Six-byte CSR entries (commit a17e502, tip 4884aaf, branch exp/d-12-mem)**: 6-byte edge struct `[u16; 3]` with 32-bit head split into two u16 halves and 16-bit weight; weights >= 65,535 escape to sorted side table. Box 2: official K 16.48, fresh K 16.14, T 0.271 / 0.285 s, M 125.3 MB, S 529. CSR memory reduced from 80 MB to 60 MB, M down 12% with T unchanged. Board run: row 24 (run-e000fdfd8967, 0.28 s, 129 MB, 60.6 points).
- **d-14: Static C runtime (commit aaf31e4, branch exp/d-14-static)**: linked libc statically via `+crt-static` in `.cargo/config.toml`, avoiding dynamic loader startup. Box 2: official K 16.54, fresh K 16.10, T 0.271 / 0.286 s, M 124.0 MB, S 529. M -1.3 MB, T unchanged. Board run: row 29 (lifted with d-13b).
- **d-13b: Parallel claim-BFS node relabel and buffer reuse (commits c970cca, 097a871, 42cd7dd, fe87585, tip 179f365, branch exp/d-13-t)**: parallel BFS from spread seeds with chunked root claims (`ROOT_CHUNK = 4096`), forward rows copied in old-id order, old forward CSR buffers reused for reverse CSR, and `mallopt(-3, 262144)` pins mmap threshold at 256 KiB. Box 2: official K 16.62, fresh K 16.33, T 0.257 / 0.267 s, M 127.0 MB, S 590. Board run: row 29 (run-86b2705d01d9, 0.27 s, 130 MB, 60.8 points).
- **d-15a: Queries run in relabelled order (commit 3c97338, branch exp/d-15-q)**: sort queries by relabelled `(s, t)` before execution so threads run searches in overlapping graph regions and share cache lines. Box 2: official K 16.73, fresh K 16.46, T 0.254 / 0.262 s, M 126.7 MB, S 592. Board run: row 31 (lifted with d-15b).
- **d-15b: Direct-mapped label table on relabelled ids (commit b8d90ea, tip 0b2c4e6, branch exp/d-15-q)**: 2^16 direct slots (1.3 MB) indexed by `node & (DIRECT_SLOTS - 1)` with open-addressing overflow table. Box 2: official K 17.89, fresh K 17.73, T 0.225 / 0.230 s, M 118.9 MB, S 610. K +7.7%, T -12%, M -8 MB. Board run: row 31 (run-e5d1af9ec25d, 0.23 s, 124 MB, 62.5 points).
- **d-23: Code compaction (commits b9c02e6, 5ec1513, ca0dfa7, 84d9650, f03de6c, f273579, branch exp/d-23)**: compacted lines across startup/IO, heap setup, placement, and query loops, main.rs 603 -> 546 lines (scored S 610 -> 553). Board run: row 56 (lifted with d-26).
- **d-26: Parallel relabel loop (commit c2b5d4b, branch exp/d-26)**: parallelized the relabel's `new_id` and row-end assignment across worker order blocks. Eliminated a ~23 ms serial bottleneck on the box. Board run: row 56 (run-aab79cef7747, 0.21 s, 124 MB, 63.4 points).
- **d-27: Bucket queue, forward probe, buffer drop, 4-byte edges (commits 8ea91ed, e43fdf4, 8d7ddd4, a1f9f32, f91ea98, branch exp/d-27-argon)**: monotone bucket queue (2048 x 32 with 64-bit occupancy bitmasks), 256-settle forward probe before activating backward search, early drop of build buffers, and 4-byte packed edges with heavy weight side table. Graph edges reduced to ~20 MB per direction, memory fell to 110 MB, and T reached 0.19 s. Box: median deciding K 19.319 against the banked c2b5d4b 18.807 (+2.7 %). Board run: row 61 (run-90db6f3cc3ba, 0.19 s, 110 MB, 64.6 points).
- **d-27 r1: Work-balanced search and early probe return (commits 864d392, af13865, cc8abd2, branch exp/d-27r1-flash)**: search expands the side with fewer queued entries, so one side no longer runs alone on a lopsided query (the case that prompted it: one fresh-seed query took 52 ms with 234,179 backward against 279 forward settles); and forward probe exits immediately once target distance is final (`dt <= lower_f`). Box, interleaved rounds: cc8abd2 = d-27 r1 s1-s3 at deciding K 20.518 against f91ea98 19.248 (+6.6 %), S 689 -> 659. Locally s1 cut all-query settles from 7,648,607 to 7,439,518 and the query phase from 100.8 to 96.0 ms at 12 workers. Board run: row 63 (lifted with d-28 s3).
- **d-28 s3: Reverse CSR by in-degree count and atomic placement (commit 6f079e0 on exp/d-28-s3, cherry-picked onto d-27 r1 as 7e2c03d, branch exp/d-29-lift)**: reverse CSR built from the forward CSR by a parallel atomic in-degree count, a prefix sum and atomic slot placement into the reused forward buffers. Avoids the ~30 MB slab records; locally the reverse build fell from 24-45 ms to 12-16 ms and the rev-phase memory from 90 to 58 MB. Box, interleaved rounds on four datasets (deciding K = lowest dataset): alone on f91ea98 +3.5 % (18.634 -> 19.282); on top of d-27 r1 +0.5 %; the whole lift 7e2c03d 20.619 against f91ea98 19.248 = +7.1 %, ranges disjoint. Board run: row 63 (run-e22137237ce5, 0.19 s, 111 MB, 64.8 points).

## Failed experiments (Rust program)

- **Forward-only early-exit Dijkstra, no reverse CSR (commit 15c71b6, branch exp/d-2b-fwd)**: box K 8.76 fresh / 8.81 official, T 0.895 s official / 0.950 s fresh vs floor 0.372 s / 0.392 s. Slower than bidirectional floor by 2.41x; kept on branch as oracle.
- **Sequential BFS locality relabel of both CSRs before queries (commit acd7dc4, branch exp/d-3-relabel)**: halved local query phase (959-1001 ms -> 448-469 ms), but sequential BFS (339-361 ms) and rebuild (175-199 ms) cost more than saved net; box K 11.28 fresh / 11.31 official, T 0.493 s (+33% T), M 212.2 MB (+33% M).
- **Per-thread scratch compaction: one 16-byte label map per search side and 12-byte packed heap (commit 9306635, branch exp/d-4-scratch)**: lowered memory (160 -> 138.7 MB) but cost +10% time (0.372 -> 0.410 s) because each relaxation probes the other side's map too; box K 13.20 fresh / 13.45 official vs 13.94 / 14.07.
- **Graph build without atomics, as first written (commit 8a68049, branch exp/d-7-build)**: parse into per-thread buckets by tail partition, local degree count, prefix sum, scatter, reverse CSR from forward in 2 slabs. T -6% (0.323 s) but S +23% (490 -> 601 lines), net noise on K (14.12 vs 14.15-14.18). Rewritten compactly as d-8 (S 503, K 14.49).
- **Release of big label table after its query, with glibc mmap threshold pinned (commit 223e5ac, branch exp/d-9-scratch)**: box K 13.91 fresh / 14.36 official; M did not fall (143.5 vs 141.5 MB), queries +23 ms from refaulting released memory. Peak resident memory is set during reverse build (~134 MB), not query scratch.
- **Push pruning in query loop (not committed, branch exp/d-10-relax)**: skip heap push when new distance plus other side's minimum reaches best path found. Cut pushes on full seed 1 by 5.7% (8.65M -> 8.15M), but settles +0.3% and local query time did not move.
- **Prefetch of other side's next edge rows (not committed, branch exp/d-11-prefetch)**: slower locally by +4-5% at both thread counts.
- **Two-stage prefetch: end[] of pops 9-16 ahead, edge rows of nearest 8 (commit 7e76e2f, branch exp/d-11-prefetch)**: box T unchanged (0.268 s), S +7 lines, box K 15.82 fresh / 16.23 official vs 15.84 base. Local -6.6% at 12 workers did not reach the box because end[] was already in L3.
- **Relabel probes for d-13 (branches exp/d-13-probe-relabel, -relabel2, -claim, -claimch, -nomallopt, never merged)**: sequential BFS with full rebuild (bench K 11.47), same twice (bench K 9.94), first parallel claim (bench K 13.72), claim with chunked roots (bench K 15.36 fresh / 15.65 official, kept old forward rows alive), probe without pinned mmap threshold 765efdb (M 148.0 MB vs 131.6 MB, mallopt worth 16.4 MB). Probe exp/d-13-probe-q2 ran query phase twice to measure share of T (K 12.01).
- **Landmark (ALT) lower bounds, probe only (commit d7f7783, branch exp/d-15-probe-alt, never lifted)**: two landmarks with forward and backward full searches in parallel cost more wall time on box than settles saved (bench K 12.90, T 0.352 / 0.358 s vs 0.225 / 0.230 s without, ~0.127 s cost and +55 MB memory).
- **Early drop of relabel claim flags (commit b4be9db, branch exp/d-15-q, reverted in f9dab2e)**: box M stayed within spread of same code without it (bench K 17.72 fresh / 17.97 official, M 119.1 vs 118.9-119.5 MB, S +1 line). Dropped: no gain beyond the spread, and one more line.
- **Fused forward and reverse rows into single records (d-16a, dropped before building)**: both search sides reach only 4,921 of 8,065,456 labels (0.06%), so storing forward and reverse edges together wastes cache bandwidth on unvisited directions.
- **48-bit label representations (d-16c, dropped before building)**: the spec bounds answers by (n - 1)(2^32 - 1), about 2^53 at this graph's size, so 48-bit distances could overflow on a deep graph.
- **Single-slab reverse build (d-16, dropped before building)**: building reverse CSR in a single slab saves a re-read pass but increases peak memory by +30 MB (-4.6% K).
- **Symmetric rows probe (commit 7be51bd, branch exp/d-20-sym-glm)**: one row per node holding (neighbour, w_out, w_in) for both search directions, built from the forward and reverse CSRs. Box: query phase median 110.6 ms of 10 runs against the base's 108.85 (+1.75 ms), 5,137,536 entries; inside its GO line (base + 5 ms), so d-21 built the layout in full straight from the parse.
- **Symmetric graph built from parse without forward or reverse CSR (commits 084610b, 66fbfe5, 6d1f4b2, 20f3189, dde2377, branch exp/d-21-sym-glm)**: the design aimed at M ~120 -> ~90 MB. Laptop against 7068578: peak RSS -27 MB and queries level (111 vs 115 ms), but placing took 91.5 ms against 12.6 + 23.9 ms for the two CSR builds, because the per-bucket unmaps that return memory cost 25-55 ms. Box K 16.69 against the base's 17.73, M 104.6 against 119.2 MB (-15) but S 822 against 610 (+212); its best case was -1 to +0.5 % K, so refining could not reach the GO line. Dropped.
- **Scratch shrink and early new_id free (commits fc14dfe, 9ada4e4, e8becd0, branch exp/d-22-mem-glm)**: freeing `new_id` once the queries are mapped (fc14dfe) and shrinking a scratch table after a query that outgrew it (e8becd0); 9ada4e4 only printed per-thread scratch bytes to stderr (38.8-39.1 MB at 12 workers). Box M -0.5 to -1 MB against a GO line of -6 MB, K 17.71 / 17.63 / 17.46 against the base's 17.73: the query phase did not set the peak, so d-24 split the phases next. Dropped.
- **Phase instrumentation probe (commit e3ad6a0, branch exp/d-24, never lifted)**: diagnostic probe printing per-phase VmHWM and timing metrics to stderr.
- **Relabel breakdown probe (commit 2cb7b40, branch exp/d-25, never lifted)**: diagnostic probe isolating relabel phases, revealing a ~23 ms serial walk over worker orders and motivating the d-26 parallelization.
- **Forward CSR atomic placement without buckets (commit 2cdba8f, branch exp/d-28-mem-glm)**: placing forward edges at atomic cursors straight from the parse cut the forward phase's memory (94 -> 25 MB locally) but made it 40-60 ms slower; box T 0.175 -> 0.195 s, K 18.84 -> 18.33, M 114.9 -> 114.1 MB (the queries set the peak then, so the forward cut did not show). Dropped.
- **Post-query frees of grown tables and queue buckets (commits b6b647f, dcc2519, branch exp/d-28r1-glm)**: box M did not move (105.2 -> 108.3 / 105.5 MB) while S rose 16 lines. Parked, not dropped: the run's peak had moved to the forward build (12-byte parse records, 60 MB), so a query-time cut cannot show until that peak is cut.
- **malloc_trim before the queries and an early free of grown label tables (commits 486917f, ce640c0, d-28 s1 and s2)**: noise on the box (s2 +1.9 %, ranges overlapping, M unmoved), so they stayed out of the lift.

## d-40: rewrite in C, all phases redesigned

The Rust program above spent 168 ms and 107 MB, and the top of the board stood at half of both, so no single phase could explain the gap. The program was rewritten from nothing in C (`traverse.c`) rather than tuned. Tag KNOWN means a technique already used elsewhere (published work or other entries), written here in our own code. OURS means we have not seen it elsewhere; it is not a claim that nobody has done it.

- Road contraction: a node with at most two ways out and no query on it is walked through when only one way is not the way the walk came from (KNOWN): 1.74 M nodes to 1.20 M, 5 M edges to 3.91 M.
- One BFS pass does contraction, ordering and gathering, with a batch of road walks stepping together (KNOWN).
- Four slots per node in the first rows, so a row is one cache miss (KNOWN).
- 16-bit labels with a 64-bit redo, blocks handed out in order (KNOWN).
- Reverse rows by id slices, with a list for the edges that cross (KNOWN): rev 4.3 ms.
- Unlocked claims, a duplicate is harmless because heads take the id written last (KNOWN): zones 23.5 -> 17.1 ms.
- Separator masks flattened in 4 KB runs, three numbers per line in one vector register (KNOWN parts, OURS as combined): parse 7.3 ms against 26 ms in Rust.
- First rows written in place over the finished bins (OURS): the rows take no new memory and no faults.
- Dropping dead roads at the end of each batch (OURS): 128,000 fewer edges (3 %) than keeping them as self-loops.
- Source with no in-edges = lightest first edge + normal search, second edge only if it can still win, the lightest edge repeating in that pass under its own answer (OURS): fresh seed queries 39.5 -> 29.6 ms.
- Huge pages for all final rows, one thread per page, one contiguous layout, recycled queue arrays (KNOWN): query phase anonymous memory 53.5 -> 49.8 MB.

Reading the compiled code paid twice. gcc turned `take ? nd : old` into a branch that goes either way about half the time; written as a mask, queries went 34.3 -> 27.4 ms. And false sharing cost a quarter of the zones phase, from `Zone` structs side by side on one cache line (zones 31 -> 23.5 ms).

Failed tries in this rewrite:

- A label pair per node (both sides in one 4-byte slot, one cache line for the meeting check): about 35 ms of queries against 34.3 with one array per side, and twice the label footprint. Dropped.
- Out-degree kept in the high bits of the order entries instead of a byte array: M -1.1 MB, zones 17.1 -> 20 ms in interleaved rounds. Dropped.
- A plain 64-bit forward search for sources with no in-edges: about +5 ms on the fresh seed. Replaced.
- First version of the search from a no-in source, without trying the lightest edge first: the far edge flooded the 65,535 ball, fresh queries 47.6 ms. Fixed by the order above.
- 6, 8 and 10 query workers against 12: queries 50, 44 and 38 ms against 35 at that point. 12 stays.

Official harness on box 1 for this code: D T 0.0706 s, M 60.49 MB, S 884, K 34.94 against the board's calibration; D-fresh T 0.0728 s, M 60.3 MB, K 34.43.

## d-41: fewer loads per edge, fewer lines

The numbers are box medians on D from interleaved rounds of two builds, each run's output hash checked. The profile said a third of the search went on the queue: the push ran for every scanned edge, written without a branch, though only 7.1 M of 17.5 M scanned edges improve a label.

- Three node flags merged into one byte array, prefetched with the row: zones 17.15 -> 14.91 ms, rows 5.45 -> 5.06 ms.
- A popped node whose distance plus the other side's floor already reaches the best meeting is skipped (KNOWN): queries 27.6 -> 27.0 ms.
- A pass over a bucket only writes what improved to a list, one store and one counter per edge, and the pushes run after the pass. The occupancy bitmap went too, since finding the next bucket from the counts costs next to nothing: queries 27.06 -> 24.24 ms.
- The other side's label is read only for an improved label: an edge that improves nothing cannot make a new meeting, because the later of the two label writes already saw the pair. Queries 24.21 -> 21.16 ms, and 17.5 M pairs of loads become 7.1 M.
- Lines, S 884 -> 781: no query sort (queries +0.45 ms, sort -0.32 ms, so level, and 10 lines less), zones as the rename's work units instead of spans, a 128-bit heap key in the 64-bit search, its memory released after each use instead of a list of touched nodes. Phase sum +0.2 ms.

Failed tries:

- The state array on a huge page: phase sum 67.36 -> 67.63 ms. Dropped.
- Buckets 64 wide (1,024 of them): 67.48 -> 67.57 ms, fresh seed 69.71 -> 70.06 ms. Stays 128.
- Skipping the push as well as the pop against the other side's floor: no better than the pop alone (27.28 against 27.02 ms, same baseline). The label write it saves is cheap.
- Dropping entries beaten within the same pass before queueing them: queries -1.5 ms against -3.05 ms without the check. Dropped.
- Local copies of the label pointers: within noise. Dropped.
- Expanding the side with the smaller current bucket instead of fewer queued entries: queries 21.19 -> 21.74 ms. Dropped.
- One reserved stretch per bucket, no growing and no free list: queries -0.44 ms and about 30 lines less, but the query phase's memory rose 6.4 MB and became the run's peak, with a page-table page per touched bucket on top. Recycled arrays stay.

Official harness on box 1 for this code: D T 0.0627 s, M 60.31 MB, S 781, K 37.78; D-fresh T 0.0643 s, M 60.56 MB, K 37.27. Board run: run-7d9634c6a75e, T 64.3 ms, M 60.42 MB, K 37.30. Correct on D, D-fresh, 1, 4, 12 and 24 workers, the worked example, and generator seeds 11-15 at two scales.

## d-42: the serial parts and the barriers

The same interleaved rounds. Everything here is small; the point was what the main thread still did alone while eleven workers spun.

- Workers started with plain `clone` on 64 KB stacks, and query endpoints marked by the workers at the start of the parse, since those writes are the first touches of the state pages: start-up 3.02 -> 2.34 ms.
- A round of first rows reaches as far as the rows can go without touching a bin not yet read, instead of a fixed 12 buckets. Rounds grow from 16 buckets, so fewer barriers: rows 5.05 -> 4.55 ms.
- Reverse rows: the slices are now the zones, the id map is cleared by the worker that counts into that slice, and the running sum goes by slices. Phase sum -0.48 ms.
- Zone seeds claimed in chunks from a counter (zones -0.1 ms, and a stalled worker holds less), 16 nodes per front batch (zones 14.93 -> 14.72 ms), prefetch on id writes (ids 1.25 -> 1.08 ms).
- Row scratch released before the zones: anonymous peak 54.0 -> 53.4 MB.

Failed tries:

- Prefetching ahead in the bins while building rows, and ahead of the write cursors in the parse: no change. The hardware prefetcher already follows these.
- Front batches of 4 (zones +0.5 ms) and 32 (zones +0.2 ms).
- Prefetching rows at push time, and pop prefetch distances of 12/6 and 6/3 against 8/4: no change. A second prefetch for the end of a row: queries +0.3 ms.
- Label blocks of 128 and 512 entries against 256: queries +0.25 and +0.35 ms.
- `MADV_POPULATE_READ` per parse piece: parse -0.09 ms, within noise. Dropped.
- Compiler flags against `-O3`: `-O2` zones +0.4 ms; `-mtune=alderlake` and `-funroll-loops` no better; aligned loops and functions within noise.

Unexplained: the parse phase sits near 7.1 or near 7.95 ms for a whole set of rounds, and rebuilding the same source can move it between the two. We did not find the cause, so parse results under 0.8 ms mean nothing unless both builds sit at the same level.

The big-weight table was 54 % full on D and 53 % on D-fresh (17,788 and 17,292 of 32,768 slots), and it is reached by linear probing, so other seeds have room. Only 0.7 % of edges cross a slice in the reverse build.

Official harness on box 1 for this code: D T 0.0606 s, M 59.69 MB, S 787, K 38.48; D-fresh T 0.0629 s, M 59.92 MB, K 37.73. 600 repeat runs, 300 on each dataset, with exit code and output hash checked: no failure.

Board runs of this code: run-944839bcefa6, T 69.7 ms, M 58.87 MB, K 35.97; run-b41d8d31d286, T 67.8 ms, M 58.80 MB, K 36.49; run-e59a7ba41a96, T 62.9 ms, M 59.32 MB, K 37.82; run-39b97637f319, T 61.7 ms, M 59.96 MB, K 38.09, the best; run-d57b9024136b, T 66.0 ms, M 58.93 MB, K 36.96. In the first two runs our other tracks read 5 to 12 % and 3 to 58 % above their own best, so a busy queue moves a single run by more than this step's gain (box K 37.78 -> 38.48).

## Appendix: runs of unchanged code

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 2 | D unchanged (code of row 1); re-scored by a track A push | run-8923670489ca | 0.39 | 164 | 57.0 |
| 3 | D unchanged (code of row 1); re-scored by a track C push | run-2355f0dae94c | 0.41 | 171 | 56.3 |
| 4 | D unchanged (code of row 1); re-scored by a track B push | run-668a1c94f3a7 | 0.39 | 162 | 56.9 |
| 5 | D unchanged (code of row 1); re-scored by a track A push | run-d47d5f9e32b1 | 0.39 | 165 | 57.0 |
| 6 | D unchanged (code of row 1); re-scored by a track C push | run-31d3f9618fa6 | 0.38 | 165 | 57.2 |
| 7 | D unchanged (code of row 1); re-scored by a track B push | run-61e856ea6e28 | 0.39 | 161 | 57.1 |
| 8 | D unchanged (code of row 1); re-scored by a track A push | run-0462d899268d | 0.39 | 162 | 57.2 |
| 10 | D unchanged (code of row 9); re-scored by a track B push | run-dc9dc0d16510 | 0.37 | 171 | 57.1 |
| 12 | D unchanged (code of row 11); re-scored by a track B push | run-b7b65339fac3 | 0.37 | 143 | 57.4 |
| 13 | D unchanged (code of row 11); re-scored by a track C push | run-062781434d90 | 0.48 | 143 | 54.4 |
| 14 | D unchanged (code of row 11); re-scored by a track B push | run-6c3368e39bec | 0.36 | 142 | 57.8 |
| 15 | D unchanged (code of row 11); re-scored by a track C push | run-a8366d14f9f3 | 0.35 | 143 | 57.9 |
| 16 | D unchanged (code of row 11); re-scored by a track A push | run-38ba120641f8 | 0.35 | 145 | 57.8 |
| 17 | D unchanged (code of row 11); re-scored by a track B push | run-b437d2a75a53 | 0.36 | 146 | 57.5 |
| 18 | D unchanged (code of row 11); re-scored by a track A push | run-9410227beee2 | 0.36 | 140 | 57.8 |
| 20 | D unchanged (code of row 19); re-scored by a track B push | run-781f369e13b0 | 0.34 | 148 | 58.1 |
| 22 | D unchanged (code of row 21); re-scored by a track A push | run-26fbc9f81cb2 | 0.28 | 148 | 60.1 |
| 23 | D unchanged (code of row 21); re-scored by a track C push | run-a2a22bd830e2 | 0.29 | 148 | 59.8 |
| 25 | D unchanged (code of row 24); re-scored by a track B push | run-6a95885c9fae | 0.28 | 130 | 60.5 |
| 26 | D unchanged (code of row 24); re-scored by a track C push | run-013bc7f4b2fc | 0.27 | 128 | 60.8 |
| 27 | D unchanged (code of row 24); re-scored by a track B push | run-b2bbc363cc23 | 0.28 | 128 | 60.5 |
| 28 | D unchanged (code of row 24); re-scored by a track B push | run-a31d57cf18c6 | 0.28 | 129 | 60.4 |
| 30 | D unchanged (code of row 29); re-scored by a track B push | run-57d46d47a619 | 0.27 | 127 | 60.8 |
| 32 | D unchanged (code of row 31); re-scored by a track B push | run-3258a1a4491e | 0.23 | 124 | 62.5 |
| 33 | D unchanged (code of row 31); re-scored by a track B push | run-4762f5a8c8fd | 0.23 | 124 | 62.3 |
| 34 | D unchanged (code of row 31); re-scored by a track B push | run-8eca4a7dd47e | 0.23 | 125 | 62.3 |
| 35 | D unchanged (code of row 31); re-scored by a track B push | run-6502ce4bb615 | 0.25 | 124 | 61.4 |
| 36 | D unchanged (code of row 31); re-scored by a track C push | run-598f85742905 | 0.24 | 124 | 62.2 |
| 37 | D unchanged (code of row 31); re-scored by a track B push | run-daaceef96899 | 0.23 | 124 | 62.4 |
| 38 | D unchanged (code of row 31); re-scored by a track C push | run-1118c4a8fb5c | 0.23 | 122 | 62.5 |
| 39 | D unchanged (code of row 31); re-scored by a root push | run-ed8a1be4940c | 0.24 | 125 | 62.1 |
| 40 | D unchanged (code of row 31); re-scored by a track A push | run-082ccb61b42d | 0.24 | 124 | 62.1 |
| 41 | D unchanged (code of row 31); re-scored by a track B push | run-ad2b155dd208 | 0.23 | 124 | 62.4 |
| 43 | D unchanged (code of row 42); re-scored by a track A push | run-b3b84e18dda4 | 0.23 | 124 | 62.4 |
| 44 | D unchanged (code of row 42); re-scored by a track A and B push | run-21f946a9257d | 0.24 | 123 | 61.9 |
| 45 | D unchanged (code of row 42); re-scored by a track C push | run-aabc8a176249 | 0.24 | 125 | 62.0 |
| 46 | D unchanged (code of row 42); re-scored by a track A push | run-d1c457d64f78 | 0.23 | 123 | 62.3 |
| 47 | D unchanged (code of row 42); re-scored by a track B push | run-6295078bd500 | 0.31 | 124 | 59.3 |
| 48 | D unchanged (code of row 42); re-scored by a track C push | run-72eeda205836 | 0.24 | 124 | 61.9 |
| 49 | D unchanged (code of row 42); re-scored by a track B push | run-31b296b2dd71 | 0.24 | 125 | 62.0 |
| 50 | D unchanged (code of row 42); re-scored by a track C push | run-0736e1e8641a | 0.23 | 125 | 62.4 |
| 51 | D unchanged (code of row 42); re-scored by a track A push | run-44a0563fb8c2 | 0.28 | 125 | 60.4 |
| 52 | D unchanged (code of row 42); re-scored by a track C push | run-a7b90ede5634 | 0.25 | 125 | 61.6 |
| 53 | D unchanged (code of row 42); re-scored by a track C push | run-26461d95009f | 0.24 | 124 | 61.9 |
| 54 | D unchanged (code of row 42); re-scored by a track C push | run-57d6f4157458 | 0.23 | 125 | 62.2 |
| 55 | D unchanged (code of row 42); re-scored by a track B push | run-58c468358ded | 0.24 | 125 | 61.9 |
| 57 | D unchanged (code of row 56); re-scored by a track B push | run-9f92eaf8c792 | 0.23 | 125 | 62.6 |
| 58 | D unchanged (code of row 56); re-scored by a track A push | run-bc4d6915b073 | 0.21 | 125 | 63.5 |
| 59 | D unchanged (code of row 56); re-scored by a track B push | run-1496ba765bb1 | 0.22 | 124 | 63.0 |
| 60 | D unchanged (code of row 56); re-scored by a track C push | run-55df5fa5f0ef | 0.22 | 124 | 63.2 |
| 62 | D unchanged (code of row 61); re-scored by a track C push | run-8469ea648aca | 0.20 | 108 | 64.4 |
| 65 | D unchanged (code of row 64 plus comments and text, 59cfb26); re-scored by a track A push | run-69e6fabffb63 | 0.18 | 110 | 65.3 |
| 69 | D unchanged (code of row 68); re-scored by another track's push | run-b41d8d31d286 | 0.068 | 59 | 78.1 |
| 70 | D unchanged (code of row 68, commit 1b98d6c); re-scored by another track's push | run-e59a7ba41a96 | 0.063 | 59 | 78.9 |
| 71 | D unchanged (code of row 68, commit 5737a0d); re-scored by a track B push | run-39b97637f319 | 0.062 | 60 | 79.0 |
| 72 | D unchanged (code of row 68, commit 7f9ea27); re-scored by a track A push | run-d57b9024136b | 0.066 | 59 | 78.4 |
