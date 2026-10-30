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

## Kept experiments

- Monotone radix heap for both search sides, replacing the binary heaps (commit 82a7e0e, branch exp/d-5-radix). Verdict: kept. Board run 5263115 · Track D (run-f1748b5f56b9), row 9.
- Label-map locality: every query starts in a small L2-sized label table of shared 20-byte labels and moves to a large table only when it outgrows it (commit 3ce5d27, branch exp/d-6-labels). Verdict: kept. Its board row comes later, so no number here.

- Graph build without atomics, written compactly (commit dc9a503, branch exp/d-8-compact): the d-7 build with one placement routine shared by the forward CSR and both reverse slabs, and one thread helper. Verdict: kept; bench K 14.49. Its board row comes later, so no time or memory number here.

- Prefetch of the CSR rows of the next pops in the query loop (commit 40418c5, branch exp/d-10-relax): on each settle, the edge rows of the next eight entries this side will pop start loading before the current node is relaxed. Verdict: kept; bench K 15.84. Its board row comes later, so no time or memory number here.

## Failed experiments

- Push pruning in the query loop (not committed, branch exp/d-10-relax): skip a heap push when the new distance plus the other side's minimum already reaches the best path found. Exact, and it cut pushes on full seed 1 by about a twentieth, but the local query time did not move, so it stayed off the branch.
- Release of a big label table after its query, with glibc's mmap threshold pinned (commit 223e5ac, branch exp/d-9-scratch). Verdict: dropped. Regrowing the big tables cost time and the bench box's memory did not fall; bench K 13.91. No board run, so no number here.
- Graph build without atomics, as first written (commit 8a68049, branch exp/d-7-build): parse into per-thread buckets by tail partition, then per-partition local degree count, prefix sum and scatter, reverse CSR from the forward one in two slabs. Verdict: noise on K. It was faster on the bench box, but it added 111 code lines, which cost about as much; bench K 14.12. Rewritten compactly as d-8. No board run, so no time or memory number here.
- Forward-only early-exit Dijkstra, no reverse CSR (commit 15c71b6, branch exp/d-2b-fwd). Verdict: slower than the bidirectional floor on the bench box, so bidirectional stays (plan D2). No board run yet, so no number here.
- BFS locality relabel of both CSRs before the queries (commit acd7dc4, branch exp/d-3-relabel). It halves the local query phase, but the sequential BFS and the rebuild cost more than that saves on the bench box: slower than the floor there, so it is dropped (plan D3). No board run yet, so no number here.
- Per-thread scratch compaction: one 16-byte label map per search side and a 12-byte packed heap (commit 9306635, branch exp/d-4-scratch). Verdict: dropped. It lowers memory but costs more time than that is worth on K, because each relaxation now probes the other side's map too. No board run yet, so no number here.
