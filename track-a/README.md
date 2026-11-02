# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

Rust, standard library only. a-61 on the box (12 workers, fresh tmpfs /out per run, 12 interleaved rounds): T median 18.07 ms against a-60's 17.12, memory peak 16.2-16.9 MB against a-60's 17.2-17.8, S 698 -> 566: expected K +1.6 %. Board: a-59 K 47.91 (T 18.59 ms, M 16.5 MB, S 673, 84.02 points), a-58 K 44.00 (T 20.46 ms, M 20.1 MB, S 659).

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md):
`docker build -t a track-a && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out a && sha256sum <out>/summary.txt`

| Trick | Where | PERF |
|---|---|---|
| The pipeline: threads claim 256 KiB blocks and write each line as a 32-bit row into its hour's region, 256 rows claimed at a time; an hour is finished once the registered prefix is 2 hours ahead | [`Job::work`](src/main.rs#L543), [`parse`](src/main.rs#L278), [`Rows::claim`](src/main.rs#L451), [`State::register`](src/main.rs#L500) | a-55, a-59 |
| LF prescan: each 4 KiB chunk's line ends found from 64-byte LF masks first (two unconditional stores per window), so no line waits for the one before it | [`parse`](src/main.rs#L295-L306) | a-57 |
| Line read from both ends: the method's space from the front, the latency's space from the back, the status just before it; latency digits read 8 per load and converted with two multiply-adds | [`line`](src/main.rs#L73), [`digits8`](src/main.rs#L58) | a-56, a-57 |
| Endpoints interned to small ids: CRC32 of length, first 8 and last 8 bytes; raw 32-byte compares masked to the length; 128-byte images; new ids sorted in by finishing threads | [`ep_hash`](src/main.rs#L101), [`equal`](src/main.rs#L116), [`Table::find`](src/main.rs#L210), [`Dict::order`](src/main.rs#L169) | a-10, a-40b, a-55, a-57 |
| Finish by radix: an hour's rows sorted by latency in two stable 10-bit counting passes, then dealt to each endpoint's span in byte order, so every span comes out sorted and p50, p95 and p99 are read off by rank; the few long latencies sorted apart and appended | [`Job::finish`](src/main.rs#L580), [`deal`](src/main.rs#L473) | a-58, a-61 |
| Number output: all digits made at once, leading zeros shifted out, one 8-byte store below 10^8 (16 above) | [`put`](src/main.rs#L355), [`decimal8`](src/main.rs#L344) | a-57, a-58 |
| Exactness on late lines: repeat with no early finishing, in fresh regions, if a line arrives after its hour was claimed | [`run`](src/main.rs#L706), [`main`](src/main.rs#L753-L758) | a-55 |
| Memory: rows in per-hour regions of one MAP_NORESERVE reservation (only touched pages count); an hour's text written over its rows; after its write the region goes to a one-region pool that the next hour takes, pages already faulted in, or else back to the kernel; transparent huge pages off; file mapped over a zero tail; mapping released in 4 MiB steps | [`Rows`](src/main.rs#L391), [`Rows::take`](src/main.rs#L438), [`Job::emit`](src/main.rs#L671), [`main`](src/main.rs#L734-L742), [`Job::release`](src/main.rs#L696) | a-12, a-55, a-59, a-60 |

Every measurement, failed experiments included, with its board run ID: [PERF.md](PERF.md). Receipts: `reports/a/` at the repository root, one `harness-<commit>.txt` per measured submission commit, from the board's runs of that commit; each names the commit it measured, and the commit after a measured push only adds its receipt.

Limit: the run is CPU-bound at 12 workers (one worker 114 ms, twelve ~19 ms; hyperthreads fill the cores), so what is left is instructions: ~700 M user instructions per run against HighVelocity's ~525 M on the same box. Moving the mapping's release (a-57 LOST list) does not help: the unmap costs ~2 ms wherever it lands.

## Approach

Rust, standard library only. The binary links the C runtime statically (probe a-26), so no dynamic loader runs before main. The program maps `/data/requests.log` read-only (`src/main.rs` `main`) and claims it in 256 KiB blocks in file order (`Job::align`, `Job::work`). Each thread walks its claimed blocks with private open-addressing tables (`parse`, `Table`), writing each line as a 32-bit row, (global endpoint id + 1) << 21 | 5xx << 20 | latency, into its hour's region (`Rows`, a-59): a thread claims 256 rows of an hour at a time, so a row needs no atomic. A latency of 2^20 - 1 or more (0.24 % on seed 1) goes to the hour's small long area instead, as two slots.

The pipeline finishes hours as soon as the registered prefix of the file holds a line two hours later (LAG 2), so threads do not wait for the whole scan to complete (`Job::work`). If a line arrives for an hour that was already claimed for finishing, the run is marked late, and `main` repeats the entire run with no early finishing (LAG = HOURS), ensuring the output is exact for any line order (`run`, `main`).

Each 4 KiB chunk's LFs are found first, two unconditional stores per 64-byte window (`parse`, a-57), so a line is read from both its ends: the timestamp is always 27 bytes, so one 32-byte space mask from byte 28 ends the method and starts the endpoint, and one 16-byte mask before the LF finds the latency's space, with the 3-digit status just before it (`line`, a-57). The latency's digits are read 8 per load and converted with two multiply-adds (`digits8`, a-56). The hour is checked with one u64 load of "MM-DDTHH" against the previous line's.

The endpoint key has two levels: a small open-addressing table per thread interns each endpoint to a global id. Its hash is CRC32 of the length and the first and last 8 bytes (`ep_hash`, a-57), and a hit is confirmed by raw 32-byte compares masked to the length with `bzhi`, so no masked copy of the line is made (`equal`, a-57). The names are 128-byte aligned images published by an atomic counter (a-55 r2d); the lock on adding a name covers only the id, and finishing threads sort new ids in byte order (`Dict::order`, a-55 r2b).

Memory is kept small (a-59, a-60): the hours' regions sit in one anonymous MAP_NORESERVE reservation, so only the pages rows touch cost anything; an hour's text is written over its rows, and once it is written the region goes to a pool of one (`Rows::take`), from which the next hour takes it with its pages already faulted in; when the pool is full a region is given back (MADV_DONTNEED). A fresh region per hour (a-59) cost ~7,000 page faults a run; an unbounded pool grew to 11 regions and +3.8 MB of peak, one costs ~+0.6 MB. A pooled region holds an older hour's bytes, so each claimed chunk of 256 rows is zeroed first (a row no line takes must read 0). Transparent huge pages are turned off (PR_SET_THP_DISABLE): with THP on (the box's setting), an hour's first row charged a 2 MiB page and the peak doubled. Nothing large is allocated by malloc. Once all hours are claimed, threads release the input mapping's page-table entries in shared 4 MiB steps (`Job::release`, a-12, a-55) so process exit has fewer to tear down.

The tail runs in parallel as hours become ready (`Job::finish`, a-58, a-61): one pass counts each hour's rows per endpoint and 5xx flag and builds two 1,024-entry histograms of the latency's low and high 10 bits; two stable counting passes (`deal`) then sort the rows by latency, using the region past the rows as the other buffer, and a last pass deals them to each endpoint's span in byte order (free slots go to one trash slot, so the pass has no branch). A stable deal keeps each span sorted, so p50, p95 and p99 are read off by rank. The few latencies of 2^20 - 1 or more sit in the long area as (row, latency) pairs, which read as u64s order by latency: one sort, then they are appended to their spans. Until a-60 a quickselect per span (AVX2 partition, bitonic base case) did the ranks: as fast in instructions (671 M against 676 M a run) and 0.9 ms faster, but 132 more lines and a scratch buffer per thread (+0.9 MB of peak). The text goes over the rows; hours are placed in hour order and each is written at its offset with `pwrite`, so finishing threads write at once (`Job::emit`).

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only. Each output number below 10^8 is made at once in one u64, the leading zeros shifted out, and one 8-byte store writes it; larger ones take two u64s and a 16-byte store (`put`, `decimal8`).

## Dependencies

None.

## With another 24 hours

Cut instructions: the finish's count pass could merge into the parse's row write. The line loop itself is at ~114 ms on one worker; HighVelocity's ~25 % fewer user instructions show the room.
