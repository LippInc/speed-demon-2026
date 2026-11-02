# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

## For the judges, in one minute

- **The code:** one file, [src/main.rs](src/main.rs), Rust with the standard library only: 439 code lines, 443 with the Dockerfile's 4 (S). Its top comment describes the pipeline, and each function's comment says why it is shaped the way it is.
- **The result:** 32.05 s → 15.4 ms (about 2,100×), 228 MB → 14.2 MB. The best board run of this code, run-e16b382e8281: T 15.41 ms, M 14.20 MB (13.54 MiB), S 443, D 1, K 58.27. Its other board runs read T 15.49 to 16.47 ms. The same work before the readability rewrites (a-71, S 401), best board run run-f4bf9c49660d: K 58.99, T 15.35 ms, M 14.53 MB.
- **The limit:** it relies on the spec's shape. More than 2,047 distinct endpoints abort on an assert (the spec says exactly 1,842), and timestamps outside its 72-hour window are not handled. Lines out of order only cost time: the run starts over and the output stays exact.
- **Box against board:** our copy of the scoring box (same CPU, 12 workers, a fresh tmpfs /out per run, interleaved rounds) reads lower and tighter than the board; its readings are in the table below. We see nothing else on the board's machine, so we read the board's higher and wider T as other work there, without proof. What supports it: beside one busy process the box itself slows down.
- **Where each criterion's evidence is:** code quality in main.rs's comments; technique in the table below, each trick linked to its lines and its PERF step; the optimisation log in [PERF.md](PERF.md), with the board runs in its table and failed experiments with their numbers.
- **Terms:** T is the run time, M the peak memory (MB is 10^6 bytes, MiB 2^20), S the code lines after rustfmt, D 1 plus the declared dependencies (ours is 1: none); K, the speed score, grows as each falls against the reference. The board is the organisers' live leaderboard; the box is our bench machine. KNOWN marks a technique from published work or a public entry, written here as our own code; OURS marks one worked out here.

| Box reading of this code | T | Peak | Hash against the reference |
|---|---|---|---|
| Seed 1, 21 interleaved rounds | 14.17 ms (fastest round 14.07) | mean 13.77 MiB, highest of 40 14.64 | ok on every round |
| Seeds 52719364, 248057911, 7 at full scale, no redo | 14.04, 13.94, 14.31 ms | 12.84 to 14.61 MiB | 9 of 9 each |
| Seed 1's lines shuffled, which forces the redo | 19.3 to 19.8 ms | - | 9 of 9 |
| Seed 1 beside one busy process | 15.99 ms | - | ok on every round |
| Repeat runs: seeds 1 and 7, alone and beside busy processes, the redo, 14 seeds never run before, files ending on a page or block boundary | - | - | 14,620 runs, 0 failed |

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md):
`docker build -t a track-a && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out a && sha256sum <out>/summary.txt`
That reproduces the output and its hash; T and M come from the board's runner.

| Trick | Where | Origin | PERF |
|---|---|---|---|
| The pipeline: threads claim 1 MiB blocks (two per 2 MiB page table, so few threads queue on its lock while faulting the input in) and write each line as a 32-bit row into its hour's region, 256 rows claimed at a time; an hour is finished once the parsed prefix is 2 hours ahead | [`work`](src/main.rs#L479), [`parse`](src/main.rs#L269), [chunk claim](src/main.rs#L299) | KNOWN shape, OURS code | a-55, a-59, a-64 |
| Forward read, no LF search: each line is read from its start and the next line starts where its latency's digits end; two prefetches 1.3 KB ahead per line, since the hardware prefetcher stops at each 4 KiB page | [`parse`](src/main.rs#L269) | KNOWN shape, OURS code | a-62 |
| Line read in one pass: one 64-byte space mask from the method holds the method's and the endpoint's ends, the status follows, and one 8-byte load finds the latency's digits up to the LF, converted with two multiply-adds | [`line`](src/main.rs#L93), [`digits8`](src/main.rs#L80) | KNOWN shape, OURS code | a-56, a-62 |
| One shared endpoint table: a 64-bit AES hash of the whole name, 53 bits of it the identity and the id in the low 11 bits of the slot, so a lookup is one load and one compare; 16,384 slots, filled in the first moments and then only read | [`ep_hash`](src/main.rs#L124), [`id`](src/main.rs#L216), [`insert`](src/main.rs#L204) | KNOWN shape, OURS code | a-63, a-69, a-70 |
| Finish by radix: an hour's rows sorted by latency in two stable counting passes (8 and 12 bits), then dealt to each endpoint's span in byte order, so every span comes out sorted and p50, p95 and p99 are read off by rank; the few long latencies sorted apart and appended | [`finish`](src/main.rs#L321), [`deal`](src/main.rs#L453), [`order`](src/main.rs#L231) | OURS | a-58, a-61, a-69 |
| Late text: an hour is reduced to one 24-byte cell per endpoint and its rows go back; only when the input is parsed does all text go straight into the output's pages through a shared mapping, in parallel, as the cells go back. 10 MB of text never sits beside the rows | [`text`](src/main.rs#L384), [mapping of /out](src/main.rs#L609) | KNOWN, OURS code | a-67 |
| Release at the end: once all is claimed, the threads give the input's and the output's pages back in shared steps, so the exit on one CPU has little left to tear down (16.54 → 14.16 ms) | [`work`](src/main.rs#L557) | OURS | a-71 |
| Number output: all digits made at once, leading zeros shifted out, one 8-byte store below 10^8 (16 above) | [`put`](src/main.rs#L436), [`decimal8`](src/main.rs#L425) | KNOWN shape, OURS code | a-57, a-58 |
| Exactness on late lines: a line for an hour already claimed marks the run late, and the program runs again by exec with no early finishing, so the output is exact for any line order | [`work`](src/main.rs#L519), [`main`](src/main.rs#L627) | OURS | a-55, a-68 |
| Memory: rows in per-hour regions of one MAP_NORESERVE reservation, so only touched pages count; a finished hour's region goes to a pool the next hour takes from, pages already faulted in; transparent huge pages off; long latencies from the top of the region with no lock | [`region`](src/main.rs#L242), [`POOL`](src/main.rs#L185), [long list](src/main.rs#L294) | KNOWN shape, OURS code | a-59, a-60, a-69 |
| Beside a busy process: at most 2 blocks per thread past the oldest unparsed one, and an idle thread sleeps on every 8th look, so a CPU goes idle and takes over a thread another process pushed off its own (22.03 → 15.05 ms beside one busy process, with the holder; this code 15.99) | [`work`](src/main.rs#L514), [the sleep](src/main.rs#L545) | OURS | a-70 |
| Start-up: threads start in a tree (the last is up after 5 spawns, not 11), no Rust runtime set-up, a static non-PIE binary built for the scoring CPU | [`spawn_tree`](src/main.rs#L572), [Dockerfile](Dockerfile) | OURS, the static runtime KNOWN | a-26, a-69, a-70 |

Every measurement, failed experiments included: [PERF.md](PERF.md), with board run IDs where one was saved (its table says where none was). Receipts for four early pushes (da6f278, c54220e, e2c4246, 5a559a8) are in `reports/a/` at the repository root, each from the board's run of that commit; later steps cite their board run IDs only.

## Tried and taken out: the holder process

From a-67 to a-70 the program started a second process that shared its address space and only slept (clone with CLONE_VM). When the run exited the mappings outlived it, so the kernel's teardown of them fell after the process the timer waits for had gone. The official timer's comment describes the rule this relied on: "As PID 1 we also reap anything the submission orphaned; only our own child's exit stops the clock." It reached T 13.87 ms and K 62.50 on the board, and our team's best on the board still comes from that code.

It removed no work, though: the teardown still happened, past the end of the timed interval. We want T to say how fast the program is, so we took it out (a-71). The teardown now runs inside the timed run, shared by the threads. Box, 21 rounds: with the holder 13.29 ms; without it and no other change 16.54 ms; with the threads' shared release 14.16 ms.

## Approach

The threads claim the input in 1 MiB blocks in file order. A thread reads each line forward, looks its endpoint up in one shared table and writes a row into its hour's region. Hours finish while other threads keep parsing, as soon as the parsed prefix of the file is 2 hours past them, so only a few hours' rows are alive at once. Each thread takes the next job in order: an hour that is safe to finish, the next block, or, once the input is parsed, the text of the next hour whose place in the output is known.

Spec 2 promises no bound on how far a line is out of order, and generated files stay within one hour. So the early finishing is checked, not assumed: a late line is detected and the run starts over with no early finishing.

The peak comes at the very end: the output itself is 9.64 MiB on tmpfs, charged to M, and the rest is about 4 MiB (PERF.md, "Floors and budgets").

## Limits

- More than 2,047 distinct endpoints do not fit the 11-bit id, and such a run aborts on an assert (2,047 ran, 2,048 exited 134). Spec 2 says exactly 1,842.
- Timestamps outside the 72-hour window of spec 2 are not handled.
- An endpoint longer than the spec's 125 bytes gives a wrong output (a 200-byte one did).
- What is left: bare reading of the input with 12 threads takes 5.84 ms of the run's 14.2 (about 40 GB/s, the machine's memory). The kernel's share is about 27 % of the cycles, spread over small items, the largest under 2.4 %. All 12 threads are busy in nearly every millisecond.

## Dependencies

None.

## With another 24 hours

The largest item we can still name is the memory cgroup's per-CPU charge stock, 1.3-1.9 MiB of the peak, which counts pages a CPU has reserved and not used. We found no cheap way to shrink it from inside the program.
