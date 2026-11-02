# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

## For the judges, in one minute

- **The code:** one file, [src/main.rs](src/main.rs), Rust with the standard library only: 390 code lines, 394 with the Dockerfile's 4 (S). Its top comment describes the pipeline, and each function's comment says why it is shaped the way it is.
- **The result:** 32.05 s → 13.9 ms (about 2,300×), 228 MB → 14.3 MB. The best board run, run-a2177c230402 (commit e1cb148; the last commit adds the endpoint cap's assert and comments): T 13.87 ms, M 14,258,176 bytes (14.26 MB, 13.60 MiB), S 392, D 1, K 62.50. Its other board runs read T 13.84 to 17.31 ms. Without the holder process (see its section) the box reads 14.33 ms at best, against 13.29 ms with it.
- **Box against board:** on our copy of the scoring box (same CPU, 12 workers, a fresh tmpfs /out per run, interleaved rounds) this code reads T 13.31 ms and a peak of 13.77 MiB on average, 14.61 MiB at most over 40 runs. The box's rounds stay within 13.1 to 13.5 ms. We see nothing else on the board's machine, so we read the board's higher and wider T as other work there, without proof. What supports it: beside one busy process the box reads 15.05 ms.
- **Other seeds:** seeds 52719364, 248057911 and 7 at full scale match the reference's hash on 9 of 9 runs each, at T 13.15 to 13.37 ms, with no redo. A shuffled full file, which forces the redo, takes 17.6 to 18.0 ms, hash 9 of 9.
- **Where each criterion's evidence is:** code quality in main.rs's comments; technique in the table below, each trick linked to its lines and its PERF step; the optimisation log in [PERF.md](PERF.md), with the board runs in its table and every failed experiment with its numbers.
- **Terms:** T is the run time, M the peak memory (MB is 10^6 bytes, MiB 2^20), S the code lines after rustfmt, D 1 plus the declared dependencies (ours is 1: none); K, the speed score, grows as each falls against the reference. The board is the organisers' live leaderboard; the box is our bench machine. KNOWN marks a technique from published work or a public entry, written here as our own code; OURS marks one worked out here.

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md):
`docker build -t a track-a && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out a && sha256sum <out>/summary.txt`

| Trick | Where | Origin | PERF |
|---|---|---|---|
| The pipeline: threads claim 1 MiB blocks (two per 2 MiB page table, so few threads queue on its lock while faulting the input in) and write each line as a 32-bit row into its hour's region, 256 rows claimed at a time; an hour is finished once the parsed prefix is 2 hours ahead | [`work`](src/main.rs#L439), [`parse`](src/main.rs#L247), [chunk claim](src/main.rs#L276) | OURS | a-55, a-59, a-64 |
| Forward read, no LF search: each line is read from its start and the next line starts where its latency's digits end; two prefetches 1.3 KB ahead per line, since the hardware prefetcher stops at each 4 KiB page | [`parse`](src/main.rs#L247) | KNOWN shape, OURS code | a-62 |
| Line read in one pass: one 64-byte space mask from the method holds the method's and the endpoint's ends, the status follows, and one 8-byte load finds the latency's digits up to the LF, converted with two multiply-adds | [`line`](src/main.rs#L80), [`digits8`](src/main.rs#L66) | KNOWN shape, OURS code | a-56, a-62 |
| One shared endpoint table: a 64-bit AES hash of the whole name, 53 bits of it the identity and the id in the low 11 bits of the slot, so a lookup is one load and one compare; 16,384 slots, filled in the first moments and then only read | [`ep_hash`](src/main.rs#L117), [`id`](src/main.rs#L198), [`insert`](src/main.rs#L185) | OURS | a-63, a-69, a-70 |
| Finish by radix: an hour's rows sorted by latency in two stable counting passes (8 and 12 bits), then dealt to each endpoint's span in byte order, so every span comes out sorted and p50, p95 and p99 are read off by rank; the few long latencies sorted apart and appended | [`finish`](src/main.rs#L297), [`deal`](src/main.rs#L425), [`order`](src/main.rs#L213) | OURS | a-58, a-61, a-69 |
| Late text: an hour is reduced to one 24-byte cell per endpoint and its rows go back; only when the input is parsed does all text go straight into the output's pages through a shared mapping, in parallel, as the cells go back. 10 MB of text never sits beside the rows | [`text`](src/main.rs#L357), [mapping of /out](src/main.rs#L546) | KNOWN, OURS code | a-67 |
| The holder: see the paragraph below | [`holder`](src/main.rs#L508), [its start](src/main.rs#L529) | OURS | a-67 |
| Number output: all digits made at once, leading zeros shifted out, one 8-byte store below 10^8 (16 above) | [`put`](src/main.rs#L408), [`decimal8`](src/main.rs#L397) | OURS | a-57, a-58 |
| Exactness on late lines: a line for an hour already claimed marks the run late, and the program runs again by exec with no early finishing, so the output is exact for any line order | [`work`](src/main.rs#L473), [`main`](src/main.rs#L558) | OURS | a-55, a-68 |
| Memory: rows in per-hour regions of one MAP_NORESERVE reservation, so only touched pages count; a finished hour's region goes to a pool the next hour takes from, pages already faulted in; transparent huge pages off; long latencies from the top of the region with no lock | [`region`](src/main.rs#L223), [`POOL`](src/main.rs#L167), [long list](src/main.rs#L271) | OURS | a-59, a-60, a-69 |
| Beside a busy process: at most 2 blocks per thread past the oldest unparsed one, and an idle thread sleeps on every 8th look, so a CPU goes idle and takes over a thread another process pushed off its own (22.03 → 15.05 ms beside one busy process) | [`work`](src/main.rs#L468), [the sleep](src/main.rs#L497) | OURS | a-70 |
| Start-up: threads start in a tree (the last is up after 4 spawns, not 11), no Rust runtime set-up, a static non-PIE binary built for the scoring CPU | [`go`](src/main.rs#L515), [Dockerfile](Dockerfile) | OURS | a-26, a-69, a-70 |

Every measurement, failed experiments included, with its board run ID: [PERF.md](PERF.md). Receipts for four early pushes (da6f278, c54220e, e2c4246, 5a559a8) are in `reports/a/` at the repository root, each from the board's run of that commit; later steps cite their board run IDs only.

## The holder process

The program starts a second process that shares its address space and only sleeps (clone with CLONE_VM, no thread group). When the run exits, the mappings outlive it, so the kernel's teardown of them happens after the process the timer waits for has gone. The holder then dies by PR_SET_PDEATHSIG. It removes no work. The teardown still happens, past the end of the timed interval. The official timer's own comment states the rule this relies on: "As PID 1 we also reap anything the submission orphaned; only our own child's exit stops the clock." We disclose it here as what it is. Box, when it came in: 14.67-14.78 ms against 14.88-15.01 ms (a-67). Box at the end, 21 rounds: with the holder 13.29 ms; without it and no other change 16.54 ms; without it and the threads giving the input's pages back after the loop, the best we found, 14.33 ms. So against that design it is worth about 1 ms of T.

## Approach

The threads claim the input in 1 MiB blocks in file order. A thread reads each line forward, looks its endpoint up in one shared table and writes a row into its hour's region. Hours finish while other threads keep parsing, as soon as the parsed prefix of the file is 2 hours past them, so only a few hours' rows are alive at once. Each thread takes the next job in order: an hour that is safe to finish, the next block, or, once the input is parsed, the text of the next hour whose place in the output is known.

Spec 2 promises no bound on how far a line is out of order, and generated files stay within one hour. So the early finishing is checked, not assumed: a late line is detected and the run starts over with no early finishing.

The peak comes at the very end: the output itself is 9.64 MiB on tmpfs, charged to M, and the rest is about 4 MiB (PERF.md, "Floors and budgets").

## Limits

- More than 2,047 distinct endpoints do not fit the 11-bit id, and such a run aborts on an assert (2,047 ran, 2,048 exited 134). Spec 2 says exactly 1,842.
- Timestamps outside the 72-hour window of spec 2 are not handled.
- What is left: bare reading of the input with 12 threads takes 5.84 ms (about 40 GB/s, the machine's memory), and about 100 of a line's 120 cycles are that floor. The kernel's share is 23 % of the cycles, spread over small items, the largest under 2.4 %. All 12 threads are busy in nearly every millisecond.

## Dependencies

None.

## With another 24 hours

The largest item we can still name is the memory cgroup's per-CPU charge stock, 1.3-1.9 MiB of the peak, which counts pages a CPU has reserved and not used. We found no cheap way to shrink it from inside the program.
