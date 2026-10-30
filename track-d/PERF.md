# Track D: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 72.69 | 893 | 0 |
| 1 | push b17571d7ffed, scored ok | run-dac0416bdd01 | 0.39 | 161 | 57.1 |
| 2 | push f15421bc5e82, scored ok | run-8923670489ca | 0.39 | 164 | 57.0 |
| 3 | push 1d0fa92eb092, scored ok | run-2355f0dae94c | 0.41 | 171 | 56.3 |
| 4 | push dea7a01343cf, scored ok | run-668a1c94f3a7 | 0.39 | 162 | 56.9 |
| 5 | push 98a661eeaaa6, scored ok | run-d47d5f9e32b1 | 0.39 | 165 | 57.0 |
| 6 | push 5ecc03f78566, scored ok | run-31d3f9618fa6 | 0.38 | 165 | 57.2 |

## Failed experiments

- Forward-only early-exit Dijkstra, no reverse CSR (commit 15c71b6, branch exp/d-2b-fwd). Verdict: slower than the bidirectional floor on the bench box, so bidirectional stays (plan D2). No board run yet, so no number here.
- BFS locality relabel of both CSRs before the queries (commit acd7dc4, branch exp/d-3-relabel). It halves the local query phase, but the sequential BFS and the rebuild cost more than that saves on the bench box: slower than the floor there, so it is dropped (plan D3). No board run yet, so no number here.
- Per-thread scratch compaction: one 16-byte label map per search side and a 12-byte packed heap (commit 9306635, branch exp/d-4-scratch). Verdict: dropped. It lowers memory but costs more time than that is worth on K, because each relaxation now probes the other side's map too. No board run yet, so no number here.
