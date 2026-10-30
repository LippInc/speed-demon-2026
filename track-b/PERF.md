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
| 16 | push 442dbe7fbe59, scored ok | 442dbe7 · Track B (run-a2acaf6607c2) | 0.41 | 35 | 63.1 |
| 17 | push 948854aaefec, scored ok | 948854a · Track B (run-c17c1fc20c9c) | 0.40 | 36 | 63.4 |
| 18 | push dc6ca650346c, scored ok | dc6ca65 · Track B (run-9543cd485a3e) | 0.38 | 33 | 63.9 |
| 19 | push 22b6620692eb, scored ok | 22b6620 · Track B (run-84d4cd47b2fa) | 0.38 | 35 | 63.8 |
| 20 | push 90c864ad6d4e, scored ok | 90c864a · Track B (run-f639ec518cea) | 0.38 | 34 | 63.8 |
| 21 | push 2e2ee13a2258, scored ok | 2e2ee13 · Track B (run-7a2f5ce62e95) | 0.39 | 34 | 63.8 |
| 22 | push 1e9607b023c2, scored ok | 1e9607b · Track B (run-53a40b147337) | 0.38 | 34 | 64.0 |
| 23 | push 48c9f564734d, scored ok | 48c9f56 · Track B (run-2bdea80de21c) | 0.39 | 34 | 63.8 |
| 24 | push 83d39fc6cbd2, scored ok | 83d39fc · Track B (run-d3665ae71770) | 0.38 | 33 | 64.0 |
| 25 | push e71baea09250, scored ok | e71baea · Track B (run-243ba550bb65) | 0.39 | 33 | 63.9 |
| 26 | push c32fd6a3502d, scored ok | c32fd6a · Track B (run-458067610cc9) | 0.38 | 33 | 64.0 |

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
- **Steps 15-17: experiment b-9.** Term ids are renumbered by sampled document frequency.
- **Steps 18-19: experiment b-11.** The serial prologue and epilogue are cut: lines are deduplicated in FNV tables that grow, the df sample is spread over the workers, the term table is renumbered in place, the two plans are built in parallel, and the output uses a small decimal writer with an exit right after the write.
- **Steps 20-25: experiment b-13**, the S trim: one term table for interning and the probe, plus the removal of a duplicate enum and a debug counter.
- **Step 26: experiment b-21**, the C runtime linked statically (`+crt-static` in the Dockerfile's RUSTFLAGS), so no dynamic loader runs before main, built by a spec-driven builder. On b-13's code the box read K 19.22 and 19.20 (the lower of each reading), above every b-13 reading (19.14-19.18), at T 0.376-0.377 s; its board run read K 19.09.

Experiments measured only on the bench box (sd-bench/B), with no board run:

- **b-0**, the data probes; the box measured the unchanged reference on that branch: `K 1.01 (lower: official) | official K 1.01 T 55.763s | fresh K 1.01 T 55.320s | M 2098.1MB M>=input S ? | sha 5/5+5/5 OK`.
- **b-5**, an AVX2 tokenizer. It was dropped: no gain, and S grew by 50 lines. `K 16.23 (lower: fresh) | official K 16.26 T 0.517s (0.513-0.520) | fresh K 16.23 T 0.519s (0.517-0.542) | M 37.2MB S 874 L | sha 5/5+5/5 OK`.
- **b-10**, a phase probe on b-9 with stderr lines only; it is never lifted. `K 18.32 (lower: official) | official K 18.32 T 0.404s | fresh K 18.36 T 0.402s | M 34.8MB S 902 L | sha 5/5+5/5 OK`.
- **b-12**, buffering a document's tokens and prefetching their table slots before probing. It was dropped: slower on the box than b-11 (0.376 s). `K 18.26 (lower: fresh) | official K 18.28 T 0.401s (0.400-0.404) | fresh K 18.26 T 0.403s (0.400-0.444) | M 33.1MB S 930 L | sha 5/5+5/5 OK`.
- **b-15**, a probe of b-13's pass on the box: stderr lines only, never lifted. Its K includes the cost of its own post-pass stage sampler. `K 17.72 (lower: fresh) | official K 17.75 T 0.414s (0.413-0.416) | fresh K 17.72 T 0.415s (0.414-0.416) | M 33.9MB S 1021 L | sha 5/5+5/5 OK`.
- **b-23**, failed (dropped): the worker count capped at 6, 8 and 10 threads in place of all available CPUs, built by a spec-driven builder. On b-13's code the box read K 16.17 (6 workers), 16.41 (8) and 17.78 (10), at T 0.44-0.53 s against 12 workers' 0.378 s, so all available CPUs stay.
- **b-24**, the S trim at level T and M. S 827 → 595, from removals that buy nothing (the SD_PHASES laps, the query-slot renumbering) and rewrites of the same mechanisms into fewer counted lines (one parse closure, one CSR builder and one scoped-thread helper, BOOL queries parsed straight into code words, code written the way rustfmt keeps it on one line). The hot loops are unchanged. The final code is b-24e (2a2b859). Two later cuts were dropped after box readings of K 19.93 each, and both were reverted. Box reading of the code commit: `K 19.95 (lower: official) | official K 19.95 T 0.385s | fresh K 19.98 T 0.384s | M 27.0MB S 595 L | sha 5/5+5/5 OK`; the docs commit, same code: `K 19.90 (lower: official) | official K 19.90 T 0.387s | fresh K 19.98 T 0.384s | M 27.2MB S 595 L | sha 5/5+5/5 OK`.
- **b-25**, failed (dropped): the window path opened to OR-only queries whose cover holds at least 1 document in 128, 256 or 512 (all queries otherwise use 1 in 64), built by a spec-driven builder. On b-13's code the box read K 19.13 (1 in 128; T 0.378-0.379 s, inside b-13's spread), 18.96 (1 in 256; T 0.384-0.386 s) and 18.65 (1 in 512; T 0.398-0.399 s): the window fold costs more than the per-document evaluation for those queries, so 1 in 64 stays.
- **b-27**: b-21's Dockerfile change on b-24's code, built by a spec-driven builder.

Experiments measured only on the build VM and not kept:

- **b-16**, a parallel query parse over two insert-only sets shared by the workers (lines and terms). The parse got slower on the VM, not faster: the shared, fixed-size term set misses cache on every term, and the parse did not scale from 1 to 4 workers. It also added 91 lines of S, so no code was committed.
- **b-17**, tokenizing 64 bytes at a time with AVX2 class masks, lower-casing in the vector and hashing each token a word at a time. A second variant kept the byte loop and changed only the hash. Both were slower on the VM at 1, 4 and 12 workers, and both ran more instructions per docs byte (cachegrind). The byte loop's FNV multiply costs almost nothing, because it overlaps the scan. No code was committed.
- **b-18** (variant a), 16-byte term-table slots holding each term's first 8 bytes, so a token of at most 8 bytes is settled from the slot alone. It was dropped: slower on the box than b-13 (0.378 s). It saved L1 misses but ran more instructions per token. `K 18.71 (lower: official) | official K 18.71 T 0.393s | fresh K 18.73 T 0.392s | M 33.0MB S 852 L | sha 5/5+5/5 OK`.
- **b-19 probe**, b-15's phase timers on b-13 again, to read the split on the box's own CPU. The box keeps no stderr, so the split was not readable there. The timers alone cost about 37 ms of T (0.415 s against b-13's 0.378 s). `K 17.73 (lower: fresh) | official K 17.74 T 0.415s (0.414-0.419) | fresh K 17.73 T 0.415s (0.414-0.416) | M 33.9MB S 1021 L | sha 5/5+5/5 OK`.
- **b-19** (variant a), per-term document stamps in place of the per-document presence bitset and its clears. It was dropped: slightly slower on the box than b-13 (0.378 s), with M up 3.6 MB. A 300 KB stamp array per worker misses L1 where the 9.5 KB bitset did not. `K 19.10 (lower: official) | official K 19.10 T 0.382s | fresh K 19.12 T 0.381s | M 36.8MB S 820 L | sha 5/5+5/5 OK`.
- **b-20** (variant a), first-id offers only for a worker's first ten matches of a query. Its range is contiguous and ascending, so later matches can never be among the ten smallest. Exact, but it read as noise on the box against b-13 (0.378-0.379 s), with S +5. `K 19.14 (lower: fresh) | official K 19.14 T 0.379s (0.377-0.381) | fresh K 19.14 T 0.379s (0.378-0.380) | M 32.8MB S 832 L | sha 5/5+5/5 OK`.
- **b-20** (variant b), 12-byte BOOL accumulators (count and sum), with the multi-trigger stamps kept apart. It was dropped: slightly slower on the box. `K 19.08 (lower: fresh) | official K 19.10 T 0.381s (0.380-0.381) | fresh K 19.08 T 0.382s (0.381-0.383) | M 33.0MB S 828 L | sha 5/5+5/5 OK`.
- **b-22**, answering AND queries of rare terms by intersecting doc lists instead of per-document triggers. It was not built, because the VM count showed too little to gain: such queries hold 0.9 % of the 10.7 M per-document evaluations at "rarer than 1 in 1,024 docs", and 3 % at 1 in 256. No code was committed.
