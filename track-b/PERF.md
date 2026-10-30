# Track B: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2,200 | 0 |
| 1 | push ea8def94cd5e, scored ok | ea8def9 · Track B (run-852d4ff6be85) | 0.63 | 126 | 54.6 |
| 2 | push b17571d7ffed, scored ok | b17571d · Track B (run-e068b34d67dc) | 0.63 | 126 | 54.7 |
| 3 | push f15421bc5e82, scored ok | f15421b · Track B (run-13acc1fdc031) | 0.63 | 126 | 54.7 |
| 4 | push 1d0fa92eb092, scored ok | 1d0fa92 · Track B (run-bfe55557d5da) | 0.70 | 126 | 53.5 |
| 5 | push dea7a01343cf, scored ok | dea7a01 · Track B (run-45b8a072e380) | 0.61 | 93 | 56.3 |
| 6 | push 98a661eeaaa6, scored ok | 98a661e · Track B (run-5ae62ba09b77) | 0.67 | 93 | 55.1 |
| 7 | push 5ecc03f78566, scored ok | 5ecc03f · Track B (run-bf51284d401c) | 0.70 | 93 | 54.8 |
| 8 | push dbc91d1aaa1a, scored ok | dbc91d1 · Track B (run-d71935c4c4c4) | 0.66 | 36 | 58.6 |
| 9 | push bdf05643a17c, scored ok | bdf0564 · Track B (run-e45cb35660ab) | 0.53 | 38 | 60.5 |
| 10 | push 5263115c0f8b, scored ok | 5263115 · Track B (run-4ba8c1323c3e) | 0.53 | 37 | 60.5 |
| 11 | push 68a1f5679a26, scored ok | 68a1f56 · Track B (run-9a61ce4ce401) | 0.49 | 36 | 61.1 |
| 12 | push 376e3cdfa655, scored ok | 376e3cd · Track B (run-e6c6411a9710) | 0.49 | 36 | 61.1 |
| 13 | push b80b71eaed3a, scored ok | b80b71e · Track B (run-907df3b3a958) | 0.41 | 34 | 63.2 |
| 14 | push 704a88eb6e9f, scored ok | 704a88e · Track B (run-d882db67789e) | 0.42 | 34 | 63.1 |
| 15 | push cac9dd156516, scored ok | cac9dd1 · Track B (run-05c9f41a7ac2) | 0.41 | 35 | 63.2 |

What each push carried (the board re-runs the same code on every push):

- **Steps 1-4: experiment b-1**, the first Rust build.
  - One pass over the memory-mapped corpus, indexing only the query terms.
  - Phrases from an Aho-Corasick automaton over query-term ids.
  - Postings as varint doc gaps in growing fixed-size blocks; BOOL as a left fold over bitmaps.
- **Steps 5-7: experiment b-2**, posting-store memory: block chains that double from 8 B to 256 B with a 4 B back link, and an 8 B head per term and worker.
- **Step 8: experiment b-3.** BOOL is evaluated inside the one pass with no posting store: each query is decided per document from the document's query terms, triggered only by the terms of its cover.
- **Steps 9-10: experiment b-4.** The large-result BOOL queries are folded 64 documents at a time with word operations, with counts from popcount and id sums from bit-sliced popcounts.
- **Steps 11-12: experiment b-7, with b-6 under it.**
  - b-6: a compact term table, sized from the distinct terms, with inline term bytes.
  - b-7: per-term BOOL roles read once per document.
- **Steps 13-14: experiment b-8.** Phrases are matched by rarest-term anchor checks, and the automaton is removed.
- **Step 15: experiment b-9.** Term ids are renumbered by sampled document frequency.

Experiments measured only on the bench box (sd-bench/B), with no board run:

- **b-0**, the data probes; the box measured the unchanged reference on that branch: `K 1.01 (lower: official) | official K 1.01 T 55.763s | fresh K 1.01 T 55.320s | M 2098.1MB M>=input S ? | sha 5/5+5/5 OK`.
- **b-5**, an AVX2 tokenizer. It was dropped: no gain, and S grew by 50 lines. `K 16.23 (lower: fresh) | official K 16.26 T 0.517s (0.513-0.520) | fresh K 16.23 T 0.519s (0.517-0.542) | M 37.2MB S 874 L | sha 5/5+5/5 OK`.
- **b-10**, a phase probe on b-9 with stderr lines only; it is never lifted. `K 18.32 (lower: official) | official K 18.32 T 0.404s | fresh K 18.36 T 0.402s | M 34.8MB S 902 L | sha 5/5+5/5 OK`.
- **b-11**, a cut of the serial prologue and epilogue. It is not on the board yet. `K 18.97 (lower: official) | official K 18.97 T 0.376s | fresh K 18.98 T 0.376s | M 32.3MB S 906 L | sha 5/5+5/5 OK`.
