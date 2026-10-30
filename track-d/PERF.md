# Track D: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | push b17571d7ffed, scored ok | b17571d · Track D (run-dac0416bdd01) | 0.39 | 161 | 57.1 |
| 2 | push f15421bc5e82, scored ok | f15421b · Track D (run-8923670489ca) | 0.39 | 164 | 57.0 |
| 3 | push 1d0fa92eb092, scored ok | 1d0fa92 · Track D (run-2355f0dae94c) | 0.41 | 171 | 56.3 |
| 4 | push dea7a01343cf, scored ok | dea7a01 · Track D (run-668a1c94f3a7) | 0.39 | 162 | 56.9 |
| 5 | push 98a661eeaaa6, scored ok | 98a661e · Track D (run-d47d5f9e32b1) | 0.39 | 165 | 57.0 |
| 6 | push 5ecc03f78566, scored ok | 5ecc03f · Track D (run-31d3f9618fa6) | 0.38 | 165 | 57.2 |
| 7 | push dbc91d1aaa1a, scored ok | dbc91d1 · Track D (run-61e856ea6e28) | 0.39 | 161 | 57.1 |
| 8 | push bdf05643a17c, scored ok | bdf0564 · Track D (run-0462d899268d) | 0.39 | 162 | 57.2 |
| 9 | push 5263115c0f8b, scored ok | 5263115 · Track D (run-f1748b5f56b9) | 0.37 | 161 | 57.3 |
| 10 | push 68a1f5679a26, scored ok | 68a1f56 · Track D (run-dc9dc0d16510) | 0.37 | 171 | 57.1 |
| 11 | push 376e3cdfa655, scored ok | 376e3cd · Track D (run-4db71f7b98a5) | 0.36 | 142 | 57.6 |
| 12 | push b80b71eaed3a, scored ok | b80b71e · Track D (run-b7b65339fac3) | 0.37 | 143 | 57.4 |
| 13 | push 704a88eb6e9f, scored ok | 704a88e · Track D (run-062781434d90) | 0.48 | 143 | 54.4 |
| 14 | push cac9dd156516, scored ok | cac9dd1 · Track D (run-6c3368e39bec) | 0.36 | 142 | 57.8 |
| 15 | push 442dbe7fbe59, scored ok | 442dbe7 · Track D (run-a8366d14f9f3) | 0.35 | 143 | 57.9 |
| 16 | push 948854aaefec, scored ok | 948854a · Track D (run-38ba120641f8) | 0.35 | 145 | 57.8 |
| 17 | push dc6ca650346c, scored ok | dc6ca65 · Track D (run-b437d2a75a53) | 0.36 | 146 | 57.5 |
| 18 | push 22b6620692eb, scored ok | 22b6620 · Track D (run-9410227beee2) | 0.36 | 140 | 57.8 |
| 19 | push 90c864ad6d4e, scored ok | 90c864a · Track D (run-50fff4b3912a) | 0.34 | 148 | 58.2 |
| 20 | push 2e2ee13a2258, scored ok | 2e2ee13 · Track D (run-781f369e13b0) | 0.34 | 148 | 58.1 |
| 21 | push 1e9607b023c2, scored ok | 1e9607b · Track D (run-954dfa85b25d) | 0.27 | 147 | 60.4 |
| 22 | push 48c9f564734d, scored ok | 48c9f56 · Track D (run-26fbc9f81cb2) | 0.28 | 148 | 60.1 |
| 23 | push e71baea09250, scored ok | e71baea · Track D (run-e000fdfd8967) | 0.28 | 129 | 60.6 |
| 24 | push c32fd6a3502d, scored ok (lead-AB's A+B push; track D unchanged) | c32fd6a · Track D (run-6a95885c9fae) | 0.28 | 130 | 60.5 |

## Kept experiments

- Monotone radix heap for both search sides, replacing the binary heaps (commit 82a7e0e, branch exp/d-5-radix). Verdict: kept. Board run 5263115 · Track D (run-f1748b5f56b9), row 9.
- Label-map locality: every query starts in a small L2-sized label table of shared 20-byte labels and moves to a large table only when it outgrows it (commit 3ce5d27, branch exp/d-6-labels). Verdict: kept. Its board row comes later, so no number here.

- Graph build without atomics, written compactly (commit dc9a503, branch exp/d-8-compact): the d-7 build with one placement routine shared by the forward CSR and both reverse slabs, and one thread helper. Verdict: kept; bench K 14.49. Board run 90c864a · Track D (run-50fff4b3912a), row 19.

- Prefetch of the CSR rows of the next pops in the query loop (commit 40418c5, branch exp/d-10-relax): on each settle, the edge rows of the next eight entries this side will pop start loading before the current node is relaxed. Verdict: kept; bench K 15.84. Board run 1e9607b · Track D (run-954dfa85b25d), row 21.

- Six-byte CSR entries (commit a17e502, branch exp/d-12-mem): the head as two 16-bit halves and a 16-bit weight, with the few weights that do not fit in a sorted side table. Verdict: kept; bench K 16.14, with lower memory and the same time. Board run e71baea · Track D (run-e000fdfd8967), row 23.

- Static C runtime (commit aaf31e4, branch exp/d-14-static): the binary links libc statically, so the process starts without the dynamic loader. Verdict: kept; bench K 16.10 fresh and 16.54 official, the same time as before.

## Open experiments

- Node relabel for locality (commits c970cca, 097a871, 42cd7dd, fe87585, branch exp/d-13-t): one breadth-first search per worker from a spread of seed nodes claims nodes in parallel, the new ids follow those search orders, the forward rows are copied into the new numbering in old-id order, the old forward rows' memory is reused for the reverse rows, and glibc's mmap threshold is pinned low so freed buffers return their memory. As first committed (c970cca) it was level with its base (bench K 16.06 against 16.10); with the memory fix (097a871, bench K 16.28) and the streaming copy (42cd7dd, bench K 16.35) it is ahead; the line fold (fe87585, bench K 16.33) changed no behaviour. Verdict: held by the lead as the base for the time work; not on the board yet.

## Failed experiments

- Relabel probes for d-13 (branches exp/d-13-probe-relabel, -relabel2, -claim, -claimch, -nomallopt, never merged): a sequential breadth-first relabel with a full rebuild (bench K 11.47), the same twice (bench K 9.94), the first parallel claim version (bench K 13.72), claim with chunked root ids (bench K 15.36, faster but it kept the old rows alive), and d-13 without the pinned mmap threshold (bench K 15.75, same time, more memory). Probe exp/d-13-probe-q2 ran the query phase twice to measure it (bench K 12.01). Full bench lines in REPORT-d-13a.md and REPORT-d-13.md; no board runs, so no time or memory numbers here.
- Prefetch of the other side's next edge rows as well (not committed, branch exp/d-11-prefetch). Verdict: dropped; slower locally at both thread counts.
- Two-stage prefetch: end entries of pops nine to sixteen ahead, edge rows of the nearest eight (commit 7e76e2f, branch exp/d-11-prefetch). Verdict: dropped; no gain on the bench box, bench K 15.82 against 15.84.
- Push pruning in the query loop (not committed, branch exp/d-10-relax): skip a heap push when the new distance plus the other side's minimum already reaches the best path found. Exact, and it cut pushes on full seed 1 by about a twentieth, but the local query time did not move, so it stayed off the branch.
- Release of a big label table after its query, with glibc's mmap threshold pinned (commit 223e5ac, branch exp/d-9-scratch). Verdict: dropped. Regrowing the big tables cost time and the bench box's memory did not fall; bench K 13.91. No board run, so no number here.
- Graph build without atomics, as first written (commit 8a68049, branch exp/d-7-build): parse into per-thread buckets by tail partition, then per-partition local degree count, prefix sum and scatter, reverse CSR from the forward one in two slabs. Verdict: noise on K. It was faster on the bench box, but it added 111 code lines, which cost about as much; bench K 14.12. Rewritten compactly as d-8. No board run, so no time or memory number here.
- Forward-only early-exit Dijkstra, no reverse CSR (commit 15c71b6, branch exp/d-2b-fwd). Verdict: slower than the bidirectional floor on the bench box, so bidirectional stays (plan D2). No board run yet, so no number here.
- BFS locality relabel of both CSRs before the queries (commit acd7dc4, branch exp/d-3-relabel). It halves the local query phase, but the sequential BFS and the rebuild cost more than that saves on the bench box: slower than the floor there, so it is dropped (plan D3). No board run yet, so no number here.
- Per-thread scratch compaction: one 16-byte label map per search side and a 12-byte packed heap (commit 9306635, branch exp/d-4-scratch). Verdict: dropped. It lowers memory but costs more time than that is worth on K, because each relaxation now probes the other side's map too. No board run yet, so no number here.
