# Track C: Join

records.csv (3.32 M records, quoted and escaped name and note fields) joined to regions.csv and aggregated per country: count, id sum, amount sum, min and max, distinct names, name and note bytes.

One file, [join.c](join.c): C17, gcc 15, x86-64-v3 with PCLMUL and AES-NI, linked statically, no dependencies. The comment at its top walks through the design; each function's comment says why it is shaped the way it is.

Measured with the organisers' harness (run_submission.py, 5 runs, full seed 1) on our test machine: T 20.93 ms (median of run-79fdadd1aeb9, run-fcfa3f190b88, run-e74d4b5ca8fd, run-4e384d44fd78, run-46f27f0b1569), M 10.60 MB, S 425 code lines. PERF.md has the steps and what did not work. The Rust version this replaces is at commit 2306aa9 (board K 36.48).

Reproduce from the repository root: `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`
