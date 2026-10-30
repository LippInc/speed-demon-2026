# Track B: performance log

Every step from the reference to your final version, each with the live-board run ID that measured it. Judges check that the run IDs exist and that each change moved the way you say it did.

| Step | Change | Run ID | T (s) | M (MB) | Points |
|---|---|---|---|---|---|
| 0 | The reference, unchanged | | 56.35 | 2,200 | 0 |
| 1 | push ea8def94cd5e, scored ok | run-852d4ff6be85 | 0.63 | 126 | 54.6 |
| 2 | push b17571d7ffed, scored ok | run-e068b34d67dc | 0.63 | 126 | 54.7 |
| 3 | push f15421bc5e82, scored ok | run-13acc1fdc031 | 0.63 | 126 | 54.7 |
| 4 | push 1d0fa92eb092, scored ok | run-bfe55557d5da | 0.70 | 126 | 53.5 |
| 5 | push dea7a01343cf, scored ok | run-45b8a072e380 | 0.61 | 93 | 56.3 |
| 6 | push 98a661eeaaa6, scored ok | run-5ae62ba09b77 | 0.67 | 93 | 55.1 |

What each push carried:

- Steps 1 to 4 carry experiment b-1, the first Rust build: one pass over the memory-mapped corpus, indexing only the query terms, with phrases from an Aho-Corasick automaton over query-term ids. Postings were varint doc gaps in growing fixed-size blocks, BOOL a left fold over bitmaps. The board re-ran the same code on each of these pushes.
- Steps 5 and 6 carry experiment b-2, posting-store memory: block chains that double from 8 B to 256 B with a 4 B back link, and an 8 B head per term and worker in place of 12 B. The code was otherwise as in b-1.
- Experiment b-3 evaluates BOOL inside the one pass with no posting store: each query is decided per document from the document's query terms, triggered only by the terms of its cover. Its rows ship with a later push.
