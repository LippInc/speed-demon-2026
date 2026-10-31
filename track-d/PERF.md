# Track D: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points | KNOWN/OURS | Spread (board T, same code) | Kept |
|---|---|---|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 | | | |
| 1 | d-2 floor: parallel two-pass parse into forward and reverse CSR, bidirectional early-exit Dijkstra | b17571d · Track D (run-dac0416bdd01) | 0.39 | 161 | 57.1 | KNOWN | 0.38-0.41 s, 8 runs | kept |
| 2 | re-run of step 1 (another push) | f15421b · Track D (run-8923670489ca) | 0.39 | 164 | 57.0 | KNOWN | 0.38-0.41 s, 8 runs | re-run |
| 3 | re-run of step 1 (another push) | 1d0fa92 · Track D (run-2355f0dae94c) | 0.41 | 171 | 56.3 | KNOWN | 0.38-0.41 s, 8 runs | re-run |
| 4 | re-run of step 1 (another push) | dea7a01 · Track D (run-668a1c94f3a7) | 0.39 | 162 | 56.9 | KNOWN | 0.38-0.41 s, 8 runs | re-run |
| 5 | re-run of step 1 (another push) | 98a661e · Track D (run-d47d5f9e32b1) | 0.39 | 165 | 57.0 | KNOWN | 0.38-0.41 s, 8 runs | re-run |
| 6 | re-run of step 1 (another push) | 5ecc03f · Track D (run-31d3f9618fa6) | 0.38 | 165 | 57.2 | KNOWN | 0.38-0.41 s, 8 runs | re-run |
| 7 | re-run of step 1 (another push) | dbc91d1 · Track D (run-61e856ea6e28) | 0.39 | 161 | 57.1 | KNOWN | 0.38-0.41 s, 8 runs | re-run |
| 8 | re-run of step 1 (another push) | bdf0564 · Track D (run-0462d899268d) | 0.39 | 162 | 57.2 | KNOWN | 0.38-0.41 s, 8 runs | re-run |
| 9 | d-5: monotone radix heap for both search sides | 5263115 · Track D (run-f1748b5f56b9) | 0.37 | 161 | 57.3 | KNOWN | 0.37 s, 2 runs | kept |
| 10 | re-run of step 9 (another push) | 68a1f56 · Track D (run-dc9dc0d16510) | 0.37 | 171 | 57.1 | KNOWN | 0.37 s, 2 runs | re-run |
| 11 | d-6: label-map locality (small L2-sized label table first, large table on overflow) | 376e3cd · Track D (run-4db71f7b98a5) | 0.36 | 142 | 57.6 | OURS | 0.35-0.48 s, 8 runs | kept |
| 12 | re-run of step 11 (another push) | b80b71e · Track D (run-b7b65339fac3) | 0.37 | 143 | 57.4 | OURS | 0.35-0.48 s, 8 runs | re-run |
| 13 | re-run of step 11 (another push) | 704a88e · Track D (run-062781434d90) | 0.48 | 143 | 54.4 | OURS | 0.35-0.48 s, 8 runs | re-run |
| 14 | re-run of step 11 (another push) | cac9dd1 · Track D (run-6c3368e39bec) | 0.36 | 142 | 57.8 | OURS | 0.35-0.48 s, 8 runs | re-run |
| 15 | re-run of step 11 (another push) | 442dbe7 · Track D (run-a8366d14f9f3) | 0.35 | 143 | 57.9 | OURS | 0.35-0.48 s, 8 runs | re-run |
| 16 | re-run of step 11 (another push) | 948854a · Track D (run-38ba120641f8) | 0.35 | 145 | 57.8 | OURS | 0.35-0.48 s, 8 runs | re-run |
| 17 | re-run of step 11 (another push) | dc6ca65 · Track D (run-b437d2a75a53) | 0.36 | 146 | 57.5 | OURS | 0.35-0.48 s, 8 runs | re-run |
| 18 | re-run of step 11 (another push) | 22b6620 · Track D (run-9410227beee2) | 0.36 | 140 | 57.8 | OURS | 0.35-0.48 s, 8 runs | re-run |
| 19 | d-8: graph build without atomics, written compactly | 90c864a · Track D (run-50fff4b3912a) | 0.34 | 148 | 58.2 | KNOWN | 0.34 s, 2 runs | kept |
| 20 | re-run of step 19 (another push) | 2e2ee13 · Track D (run-781f369e13b0) | 0.34 | 148 | 58.1 | KNOWN | 0.34 s, 2 runs | re-run |
| 21 | d-10a: prefetch of the CSR rows of the next pops | 1e9607b · Track D (run-954dfa85b25d) | 0.27 | 147 | 60.4 | KNOWN | 0.27-0.29 s, 3 runs | kept |
| 22 | re-run of step 21 (another push) | 48c9f56 · Track D (run-26fbc9f81cb2) | 0.28 | 148 | 60.1 | KNOWN | 0.27-0.29 s, 3 runs | re-run |
| 23 | re-run of step 21 (another push) | 83d39fc · Track D (run-a2a22bd830e2) | 0.29 | 148 | 59.8 | KNOWN | 0.27-0.29 s, 3 runs | re-run |
| 24 | d-12b: six-byte CSR entries, heavy weights in a side table | e71baea · Track D (run-e000fdfd8967) | 0.28 | 129 | 60.6 | OURS | 0.27-0.28 s, 5 runs | kept |
| 25 | re-run of step 24 (another push) | c32fd6a · Track D (run-6a95885c9fae) | 0.28 | 130 | 60.5 | OURS | 0.27-0.28 s, 5 runs | re-run |
| 26 | re-run of step 24 (another push) | 2979028 · Track D (run-013bc7f4b2fc) | 0.27 | 128 | 60.8 | OURS | 0.27-0.28 s, 5 runs | re-run |
| 27 | re-run of step 24 (another push) | 0c42543 · Track D (run-b2bbc363cc23) | 0.28 | 128 | 60.5 | OURS | 0.27-0.28 s, 5 runs | re-run |
| 28 | re-run of step 24 (another push) | b581408 · Track D (run-a31d57cf18c6) | 0.28 | 129 | 60.4 | OURS | 0.27-0.28 s, 5 runs | re-run |
| 29 | d-13b + d-14: parallel claim-BFS node relabel for locality; C runtime linked statically | e2b846c · Track D (run-86b2705d01d9) | 0.27 | 130 | 60.8 | OURS | 0.27 s, 2 runs | kept |
| 30 | re-run of step 29 (another push) | 5d16aa8 · Track D (run-57d46d47a619) | 0.27 | 127 | 60.8 | OURS | 0.27 s, 2 runs | re-run |
| 31 | d-15a + d-15b: queries in relabelled order; per-thread direct-mapped label table | dc1cab8 · Track D (run-e5d1af9ec25d) | 0.23 | 124 | 62.5 | OURS | 0.23-0.25 s, 9 runs | kept |
| 32 | re-run of step 31 (another push) | 5c78ab8 · Track D (run-3258a1a4491e) | 0.23 | 124 | 62.5 | OURS | 0.23-0.25 s, 9 runs | re-run |
| 33 | re-run of step 31 (another push) | 59a4bfa · Track D (run-4762f5a8c8fd) | 0.23 | 124 | 62.3 | OURS | 0.23-0.25 s, 9 runs | re-run |
| 34 | re-run of step 31 (another push) | 8c4d4fe · Track D (run-8eca4a7dd47e) | 0.23 | 125 | 62.3 | OURS | 0.23-0.25 s, 9 runs | re-run |
| 35 | re-run of step 31 (another push) | 9224ad7 · Track D (run-6502ce4bb615) | 0.25 | 124 | 61.4 | OURS | 0.23-0.25 s, 9 runs | re-run |
| 36 | re-run of step 31 (another push) | 6ba03de · Track D (run-598f85742905) | 0.24 | 124 | 62.2 | OURS | 0.23-0.25 s, 9 runs | re-run |
| 37 | re-run of step 31 (another push) | 8604c2c · Track D (run-daaceef96899) | 0.23 | 124 | 62.4 | OURS | 0.23-0.25 s, 9 runs | re-run |
| 38 | re-run of step 31 (another push) | 67c71f1 · Track D (run-1118c4a8fb5c) | 0.23 | 122 | 62.5 | OURS | 0.23-0.25 s, 9 runs | re-run |
| 39 | re-run of step 31 (another push) | 6b8f071 · Track D (run-ed8a1be4940c) | 0.24 | 125 | 62.1 | OURS | 0.23-0.25 s, 9 runs | re-run |
Spread: the range of board T over every run of the same code; each board run is a single run. Box K below is the lower of the official and fresh-seed readings on box 2 (5 runs each).

## Decisions

The four choices the solution rests on, each marked KNOWN (published, standard practice) or OURS (this team's):

- **Parallel two-pass mmap parse straight into forward and reverse CSR** (KNOWN). Each worker parses its byte range once into per-partition buckets, then one placement routine counts degrees, sums prefixes and scatters per partition, with no atomic adds (src/main.rs:299, src/main.rs:170). The build fell from 104.6 to 85 ms on the box (REPORT-d-7.md).
- **Bidirectional early-exit Dijkstra with the degree checks first** (KNOWN). Cheap answers before any search (src/main.rs:665), the sides take turns on the smaller heap, and the stop test is the two heap tops against the best path so far (src/main.rs:623). The forward-only build stays on its branch (exp/d-2b-fwd) as the oracle; it measured slower than the bidirectional floor on the bench box (Failed experiments).
- **Per-thread scratch with no n-sized array** (OURS). A direct-mapped table on the relabelled ids with hashed overflow (src/main.rs:514, src/main.rs:559): a node's label sits in the slot given by the low bits of its id, so neighbours' labels share cache lines. Box K 17.73 fresh against 16.46 for its base (REPORT-d-15.md).
- **The claim-BFS relabel** (OURS). One breadth-first search per worker from spread seeds claims the nodes, the new ids follow the search orders, the forward rows are copied in old-id order, the old CSR's buffers are reused for the reverse build, and glibc's mmap threshold is pinned low (src/main.rs:229). Box K 16.33 fresh against 16.10 for the base before it (REPORT-d-13b.md).

Rejected options, each with its measured reason:

- Forward-only early-exit Dijkstra, no reverse CSR (commit 15c71b6, branch exp/d-2b-fwd): slower than the bidirectional floor on the bench box (bench K 8.76/8.81 fresh/official against 13.94/14.07). It stays on its branch as the oracle.
- Sequential BFS relabel of both CSRs (commit acd7dc4, branch exp/d-3-relabel): it halves the local query phase, but the sequential BFS and the rebuild cost more than that saves on the bench box (bench K 11.28/11.31 against the floor's 13.94/14.07).
- Landmark (ALT) lower bounds (probe exp/d-15-probe-alt, d7f7783, never lifted): two landmarks' forward and backward full searches cost more wall time on the box than the settles they save could win back (K 12.90 with the searches, T 0.352 / 0.358 s against 0.225 / 0.230 s without, about 0.127 s; about +55 MB).
- Per-thread scratch compaction (commit 9306635, branch exp/d-4-scratch): one 16-byte label map per search side and a 12-byte packed heap; it lowers memory but costs more time than that is worth on K, because each relaxation probes the other side's map too (bench K 13.20/13.45 against 13.94/14.07).
- Release of a big label table after its query (commit 223e5ac, branch exp/d-9-scratch): regrowing the big tables cost time and the box's memory did not fall (bench K 13.91 fresh).
- Prefetch of the other side's next edge rows, and two-stage prefetch (not committed / commit 7e76e2f, branch exp/d-11-prefetch): slower locally at both thread counts, and no gain on the bench box (bench K 15.82 against 15.84).


## Kept experiments

Each entry: the phase it attacked, then its box 2 readings (K official / fresh, T medians, M; the box status printed no min-max for these runs, so each T range reads "spread not recorded"), and the table row that measured the same code on the board.

- d-2 floor (commit 24c6aee): mmap parse into a forward and a reverse CSR, bidirectional early-exit Dijkstra with cheap answers first, per-thread label tables, binary heaps. Phase: the whole run (the floor every later step attacks). Box 2: official K 14.07 / fresh K 13.94, T 0.372 / 0.392 s, M 160.1 MB (spread not recorded). Board run: row 1.
- Monotone radix heap for both search sides, replacing the binary heaps (d-5) (commit 82a7e0e, branch exp/d-5-radix). Phase: queries. Box 2: official K 14.16 / fresh K 14.00, T 0.349 / 0.368 s, M 163.3 MB (spread not recorded). Verdict: kept; T 0.372 -> 0.349 s official against the floor's readings. Board run: row 9.
- Label-map locality: every query starts in a small L2-sized label table of shared 20-byte labels and moves to a large table only when it outgrows it (d-6) (commit 3ce5d27, branch exp/d-6-labels). Phase: queries and memory. Box 2: official K 14.56 / fresh K 14.18, T 0.341 / 0.359 s, M 139.2 MB (spread not recorded). Verdict: kept. Board run: row 11.
- Graph build without atomics, written compactly (d-8) (commit dc9a503, branch exp/d-8-compact): the d-7 build with one placement routine shared by the forward CSR and both reverse slabs, and one thread helper. Phase: build. Box 2: official K 14.92 / fresh K 14.49, T 0.320 / 0.339 s, M 141.5 MB (spread not recorded). Verdict: kept. Board run: row 19.
- Prefetch of the CSR rows of the next pops in the query loop (d-10a) (commit 40418c5, branch exp/d-10-relax): on each settle, the edge rows of the next eight entries this side will pop start loading before the current node is relaxed. Phase: queries. Box 2: official K 16.25 / fresh K 15.84, T 0.268 / 0.283 s, M 141.4 MB (spread not recorded). Verdict: kept. Board run: row 21.
- Six-byte CSR entries (d-12b) (commit a17e502, branch exp/d-12-mem): the head as two 16-bit halves and a 16-bit weight, with the few weights that do not fit in a sorted side table. Phase: memory. Box 2: official K 16.48 / fresh K 16.14, T 0.271 / 0.285 s, M 125.3 MB (spread not recorded); M 141.4 -> 125.3 against d-10a's readings at the same T. Verdict: kept. Board run: row 24.
- Static C runtime (d-14) (commit aaf31e4, branch exp/d-14-static): the binary links libc statically, so the process starts without the dynamic loader (one rustflags line). Phase: memory. Box 2, against d-12b's commit a17e502: official K 16.54 / fresh K 16.10, T 0.271 / 0.286 s, M 124.0 MB (spread not recorded); M -1.3 MB, T unchanged, K within noise. Verdict: kept. Board run: row 29, together with d-13.
- Node relabel for locality (d-13) (commits c970cca, 097a871, 42cd7dd, fe87585, branch exp/d-13-t): one breadth-first search per worker from a spread of seed nodes claims nodes in parallel, the new ids follow those search orders, the forward rows are copied into the new numbering in old-id order, the old forward rows' memory is reused for the reverse rows, and glibc's mmap threshold is pinned low so freed buffers return their memory. Phase: queries (REPORT-d-13a.md, REPORT-d-13b.md). Box 2, fe87585: official K 16.62 / fresh K 16.33, T 0.257 / 0.267 s, M 127.0 MB (spread not recorded). As first committed (c970cca) it was level with its base (bench K 16.06 against 16.10); with the memory fix (097a871, bench K 16.28) and the streaming copy (42cd7dd, bench K 16.35) it is ahead; the line fold (fe87585, bench K 16.33) changed no behaviour. Verdict: kept. Board run: row 29.
- Queries run in order of their relabelled source and target (d-15a) (commit 3c97338, branch exp/d-15-q): after the relabel, queries taken at the same time search neighbouring regions and share cache. Phase: queries (REPORT-d-15.md). Box 2, against d-13b.3: official K 16.73 / fresh K 16.46, T 0.254 / 0.262 s, M 126.7 MB (spread not recorded). K +0.08 to +0.13 over its base's runs (REPORT-d-15.md). Verdict: kept. Board run: row 31.
- Direct-mapped label table on the relabelled ids (d-15b) (commit b8d90ea, branch exp/d-15-q): each worker keeps a node's label in the slot given by the low bits of its id, so neighbours' labels share cache lines; a slot taken by another node sends the label to the hashed tables. Phase: queries and memory (REPORT-d-15.md). Box 2, against d-15a: official K 17.89 / fresh K 17.73, T 0.225 / 0.230 s, M 118.9 MB (spread not recorded). K +7.7 % over its base's fresh run (REPORT-d-15.md), lower T and lower M. Verdict: kept. Board run: row 31.


## Failed experiments

- Early drop of the relabel's claim flags (commit b4be9db, branch exp/d-15-q, reverted): memory on the bench box stayed within the spread of the same code without it (bench K 17.72 against 17.69 to 17.73).
- Landmark (ALT) lower bounds, probe only (branch exp/d-15-probe-alt, never lifted): two landmarks' forward and backward full searches, run in parallel, cost more wall time on the bench box than the settles they save could win back (bench K 12.90 with the searches and no use of them). Not built.
- Relabel probes for d-13 (branches exp/d-13-probe-relabel, -relabel2, -claim, -claimch, -nomallopt, never merged): a sequential breadth-first relabel with a full rebuild (bench K 11.47), the same twice (bench K 9.94), the first parallel claim version (bench K 13.72), claim with chunked root ids (bench K 15.36, faster but it kept the old rows alive), and d-13 without the pinned mmap threshold (bench K 15.75, same time, more memory). Probe exp/d-13-probe-q2 ran the query phase twice to measure it (bench K 12.01). Full bench lines in REPORT-d-13a.md and REPORT-d-13.md; no board runs, so no time or memory numbers here.
- Prefetch of the other side's next edge rows as well (not committed, branch exp/d-11-prefetch). Verdict: dropped; slower locally at both thread counts.
- Two-stage prefetch: end entries of pops nine to sixteen ahead, edge rows of the nearest eight (commit 7e76e2f, branch exp/d-11-prefetch). Verdict: dropped; no gain on the bench box, bench K 15.82 against 15.84.
- Push pruning in the query loop (not committed, branch exp/d-10-relax): skip a heap push when the new distance plus the other side's minimum already reaches the best path found. Exact, and it cut pushes on full seed 1 by about a twentieth, but the local query time did not move, so it stayed off the branch.
- Release of a big label table after its query, with glibc's mmap threshold pinned (commit 223e5ac, branch exp/d-9-scratch). Verdict: dropped. Regrowing the big tables cost time and the bench box's memory did not fall; bench K 13.91. No board run, so no number here.
- Graph build without atomics, as first written (commit 8a68049, branch exp/d-7-build): parse into per-thread buckets by tail partition, then per-partition local degree count, prefix sum and scatter, reverse CSR from the forward one in two slabs. Verdict: noise on K. It was faster on the bench box, but it added 111 code lines, which cost about as much; bench K 14.12. Rewritten compactly as d-8. No board run, so no time or memory number here.
- Forward-only early-exit Dijkstra, no reverse CSR (commit 15c71b6, branch exp/d-2b-fwd). Verdict: slower than the bidirectional floor on the bench box, so bidirectional stays (plan D2). No board run yet, so no number here.
- BFS locality relabel of both CSRs before the queries (commit acd7dc4, branch exp/d-3-relabel). It halves the local query phase, but the sequential BFS and the rebuild cost more than that saves on the bench box: slower than the floor there, so it is dropped (plan D3). No board run yet, so no number here.
- Per-thread scratch compaction: one 16-byte label map per search side and a 12-byte packed heap (commit 9306635, branch exp/d-4-scratch). Verdict: dropped. It lowers memory but costs more time than that is worth on K, because each relaxation now probes the other side's map too. No board run yet, so no number here.

Final result: box K 17.73 on 7068578 (official 17.91, fresh 17.73); best board run so far: step 31, run-e5d1af9ec25d, 62.5 points.
