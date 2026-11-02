# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

Rust, standard library only. a-57 on the box (12 workers, fresh tmpfs /out per run, 20 interleaved rounds): T median 19.09 ms against a-56's 23.37 ms, memory peak 23.0-23.9 MB (a-56: 20.0-20.6), S 545 (official count). The board's best run of a-56 is run-c0264a2ab8de (K 43.09, 81.7 points). Threads claim 256 KB blocks of the file and parse them into per-hour records; an hour is finished as soon as the registered prefix of the file holds a line two hours later; a line that arrives after its hour was finished makes the run repeat with no early finishing, so the output is exact for any order.

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md):
`docker build -t a track-a && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out a && sha256sum <out>/summary.txt`

| Trick | Where | PERF |
|---|---|---|
| The pipeline: threads claim 256 KiB blocks and parse into per-hour records; an hour finished early once prefix is 2 hours ahead | [`Job::work`](src/main.rs#L464), [`parse`](src/main.rs#L275), [`State::register`](src/main.rs#L416) | a-55 |
| LF prescan: each 4 KiB chunk's line ends found from 64-byte LF masks first (two unconditional stores per window), so no line waits for the one before it | [`parse`](src/main.rs#L295-L309) | a-57 |
| Line read from both ends: the method's space from the front, the latency's space from the back, the status just before it; latency digits read 8 per load and converted with two multiply-adds | [`line`](src/main.rs#L71), [`digits8`](src/main.rs#L56) | a-56, a-57 |
| Endpoints interned to small ids: CRC32 of length, first 8 and last 8 bytes; raw 32-byte compares masked to the length; 128-byte images; new ids sorted in by finishing threads | [`ep_hash`](src/main.rs#L99), [`equal`](src/main.rs#L114), [`Table::find`](src/main.rs#L208), [`Dict::order`](src/main.rs#L167) | a-10, a-40b, a-55, a-57 |
| Finish with no sort and no select: records packed to 32-bit rows (latency << 11 \| id), two counting passes by latency, then dealt out by endpoint in byte order, so every percentile is one read | [`Job::finish`](src/main.rs#L505) | a-57 |
| Branch-free number output: all ten digits made at once, leading zeros shifted out, one 16-byte store | [`put`](src/main.rs#L364), [`decimal8`](src/main.rs#L353) | a-57 |
| Exactness on late lines: repeat with no early finishing if a line arrives after its hour was claimed | [`run`](src/main.rs#L650), [`main`](src/main.rs#L705-L711) | a-55 |
| Memory: file mapped over a zero tail, so reads may pass the end; mapping released in 4 MiB steps; one malloc arena; glibc mmap threshold pinned; finish buffers pooled | [`main`](src/main.rs#L670-L693), [`Job::release`](src/main.rs#L640) | a-11, a-12, a-55, a-57 |

Every measurement, failed experiments included, with its board run ID: [PERF.md](PERF.md). Receipts: `reports/a/` at the repository root, one `harness-<commit>.txt` per measured submission commit, from the board's runs of that commit; each names the commit it measured, and the commit after a measured push only adds its receipt.

Limit: the run is CPU-bound at 12 workers (one worker 114 ms, twelve ~19 ms; hyperthreads fill the cores), so what is left is instructions: ~700 M user instructions per run against HighVelocity's ~525 M on the same box. Moving the mapping's release (a-57 LOST list) does not help: the unmap costs ~2 ms wherever it lands.

## Approach

Rust, standard library only. The binary links the C runtime statically (probe a-26), so no dynamic loader runs before main. The program maps `/data/requests.log` read-only (`src/main.rs` `main`) and claims it in 256 KiB blocks in file order (`Job::align`, `Job::work`). Each thread walks its claimed blocks with private open-addressing tables (`parse`, `Table`), parsing lines into per-hour 64-bit records (global endpoint id, 5xx flag, and latency).

The pipeline finishes hours as soon as the registered prefix of the file holds a line two hours later (LAG 2), so threads do not wait for the whole scan to complete (`Job::work`). If a line arrives for an hour that was already claimed for finishing, the run is marked late, and `main` repeats the entire run with no early finishing (LAG = HOURS), ensuring the output is exact for any line order (`run`, `main`).

Each 4 KiB chunk's LFs are found first, two unconditional stores per 64-byte window (`parse`, a-57), so a line is read from both its ends: the timestamp is always 27 bytes, so one 32-byte space mask from byte 28 ends the method and starts the endpoint, and one 16-byte mask before the LF finds the latency's space, with the 3-digit status just before it (`line`, a-57). The latency's digits are read 8 per load and converted with two multiply-adds (`digits8`, a-56). The hour is checked with one u64 load of "MM-DDTHH" against the previous line's.

The endpoint key has two levels: a small open-addressing table per thread interns each endpoint to a global id. Its hash is CRC32 of the length and the first and last 8 bytes (`ep_hash`, a-57), and a hit is confirmed by raw 32-byte compares masked to the length with `bzhi`, so no masked copy of the line is made (`equal`, a-57). The names are 128-byte aligned images published by an atomic counter (a-55 r2d); the lock on adding a name covers only the id, and finishing threads sort new ids in byte order (`Dict::order`, a-55 r2b).

Memory is kept small: one malloc arena (`mallopt(M_ARENA_MAX, 1)`, a-55 r2a) allows freed record buffers to be reused across threads without per-thread heap slack; glibc's mmap threshold is pinned at 128 KiB (`main`, a-11) so output buffers go back to the kernel at once. Once all hours are claimed, threads release the input mapping's page-table entries in shared 4 MiB steps (`Job::release`, a-12, a-55) so process exit has fewer to tear down.

The tail runs in parallel as hours become ready (`Job::finish`, a-57): each hour's records are counted per endpoint and 5xx flag and packed to 32-bit rows, latency << 11 | id; two stable counting passes (11 and 10 bits) sort the rows by latency, and a third pass deals them out to each endpoint's span in byte order, so each endpoint's latencies arrive sorted and p50, p95 and p99 are one read each. Latencies of 2^21 or more (0.24 % on seed 1) are sorted on their own and dealt after the rows. The record vectors are freed before the passes, the row buffers are kept per thread between hours, and hours are written to `/out/summary.txt` in hour order as they finish (`Job::emit`).

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only. Each output number is written with no branch: all ten digits are made at once in two u64s, the leading zeros shifted out, and one 16-byte store writes them (`put`, `decimal8`).

## Dependencies

None.

## With another 24 hours

Cut instructions: finish's first pass (~16 per record) and the deal could merge into the parse's record write, and 32-bit records would halve the bytes each hour moves. The line loop itself is at ~114 ms on one worker; HighVelocity's ~25 % fewer user instructions show the room.
