# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

Rust, standard library only. a-59 on the box (12 workers, fresh tmpfs /out per run, 20 interleaved rounds): T median 18.29 ms against a-58's 18.20 ms in the same rounds (level; a-58 alone: 17.92 ms against a-57's 20.26), memory peak 16.4-17.0 MB against a-58's 20.4-21.4. Board: a-58 K 44.00 (T 20.46 ms, M 20.1 MB, S 659), a-57 K 44.75 (T 19.48 ms, M 24.1 MB, S 545, 82.54 points).

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md):
`docker build -t a track-a && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out a && sha256sum <out>/summary.txt`

| Trick | Where | PERF |
|---|---|---|
| The pipeline: threads claim 256 KiB blocks and write each line as a 32-bit row into its hour's region, 256 rows claimed at a time; an hour is finished once the registered prefix is 2 hours ahead | [`Job::work`](src/main.rs#L657), [`parse`](src/main.rs#L277), [`Rows::claim`](src/main.rs#L422), [`State::register`](src/main.rs#L614) | a-55, a-59 |
| LF prescan: each 4 KiB chunk's line ends found from 64-byte LF masks first (two unconditional stores per window), so no line waits for the one before it | [`parse`](src/main.rs#L294-L305) | a-57 |
| Line read from both ends: the method's space from the front, the latency's space from the back, the status just before it; latency digits read 8 per load and converted with two multiply-adds | [`line`](src/main.rs#L73), [`digits8`](src/main.rs#L58) | a-56, a-57 |
| Endpoints interned to small ids: CRC32 of length, first 8 and last 8 bytes; raw 32-byte compares masked to the length; 128-byte images; new ids sorted in by finishing threads | [`ep_hash`](src/main.rs#L101), [`equal`](src/main.rs#L116), [`Table::find`](src/main.rs#L210), [`Dict::order`](src/main.rs#L169) | a-10, a-40b, a-55, a-57 |
| Finish by select: rows counted per endpoint, latencies copied to each endpoint's span in byte order, then one quickselect per span for all three ranks; AVX2 partition (one permutation row stores both sides), spans of 16 or fewer sorted in two registers by a bitonic network | [`Job::finish`](src/main.rs#L697), [`select_ranks`](src/main.rs#L461), [`partition`](src/main.rs#L509), [`small`](src/main.rs#L578) | a-58, a-59 |
| Number output: all digits made at once, leading zeros shifted out, one 8-byte store below 10^8 (16 above) | [`put`](src/main.rs#L354), [`decimal8`](src/main.rs#L343) | a-57, a-58 |
| Exactness on late lines: repeat with no early finishing, in fresh regions, if a line arrives after its hour was claimed | [`run`](src/main.rs#L819), [`main`](src/main.rs#L866-L871) | a-55 |
| Memory: rows in per-hour regions of one MAP_NORESERVE reservation (only touched pages count); an hour's text written over its rows and the region given back right after its write; transparent huge pages off; file mapped over a zero tail; mapping released in 4 MiB steps | [`Rows`](src/main.rs#L384), [`Job::emit`](src/main.rs#L789), [`main`](src/main.rs#L847-L855), [`Job::release`](src/main.rs#L809) | a-12, a-55, a-59 |

Every measurement, failed experiments included, with its board run ID: [PERF.md](PERF.md). Receipts: `reports/a/` at the repository root, one `harness-<commit>.txt` per measured submission commit, from the board's runs of that commit; each names the commit it measured, and the commit after a measured push only adds its receipt.

Limit: the run is CPU-bound at 12 workers (one worker 114 ms, twelve ~19 ms; hyperthreads fill the cores), so what is left is instructions: ~700 M user instructions per run against HighVelocity's ~525 M on the same box. Moving the mapping's release (a-57 LOST list) does not help: the unmap costs ~2 ms wherever it lands.

## Approach

Rust, standard library only. The binary links the C runtime statically (probe a-26), so no dynamic loader runs before main. The program maps `/data/requests.log` read-only (`src/main.rs` `main`) and claims it in 256 KiB blocks in file order (`Job::align`, `Job::work`). Each thread walks its claimed blocks with private open-addressing tables (`parse`, `Table`), writing each line as a 32-bit row, (global endpoint id + 1) << 21 | 5xx << 20 | latency, into its hour's region (`Rows`, a-59): a thread claims 256 rows of an hour at a time, so a row needs no atomic. A latency of 2^20 - 1 or more (0.24 % on seed 1) goes to the hour's small long area instead, as two slots.

The pipeline finishes hours as soon as the registered prefix of the file holds a line two hours later (LAG 2), so threads do not wait for the whole scan to complete (`Job::work`). If a line arrives for an hour that was already claimed for finishing, the run is marked late, and `main` repeats the entire run with no early finishing (LAG = HOURS), ensuring the output is exact for any line order (`run`, `main`).

Each 4 KiB chunk's LFs are found first, two unconditional stores per 64-byte window (`parse`, a-57), so a line is read from both its ends: the timestamp is always 27 bytes, so one 32-byte space mask from byte 28 ends the method and starts the endpoint, and one 16-byte mask before the LF finds the latency's space, with the 3-digit status just before it (`line`, a-57). The latency's digits are read 8 per load and converted with two multiply-adds (`digits8`, a-56). The hour is checked with one u64 load of "MM-DDTHH" against the previous line's.

The endpoint key has two levels: a small open-addressing table per thread interns each endpoint to a global id. Its hash is CRC32 of the length and the first and last 8 bytes (`ep_hash`, a-57), and a hit is confirmed by raw 32-byte compares masked to the length with `bzhi`, so no masked copy of the line is made (`equal`, a-57). The names are 128-byte aligned images published by an atomic counter (a-55 r2d); the lock on adding a name covers only the id, and finishing threads sort new ids in byte order (`Dict::order`, a-55 r2b).

Memory is kept small (a-59): the hours' regions sit in one anonymous MAP_NORESERVE reservation, so only the pages rows touch cost anything; an hour's text is written over its rows, and the whole region is given back (MADV_DONTNEED) right after its text is written. Transparent huge pages are turned off (PR_SET_THP_DISABLE): with THP on (the box's setting), an hour's first row charged a 2 MiB page and the peak doubled. Nothing large is allocated by malloc. Once all hours are claimed, threads release the input mapping's page-table entries in shared 4 MiB steps (`Job::release`, a-12, a-55) so process exit has fewer to tear down.

The tail runs in parallel as hours become ready (`Job::finish`, a-58, a-59): each hour's rows are counted per endpoint and 5xx flag, and a second pass copies the latencies to each endpoint's span in byte order, in the same region past the rows (top bit flipped, so they compare as i32; free slots go to one trash slot, so the pass has no branch). p50, p95 and p99 come from one quickselect per span for all three ranks (`select_ranks`): the AVX2 partition writes into a scratch buffer, the values below the pivot from the front and the rest from the back, with one permutation row per 8-lane mask serving both stores; ranges of 16 or fewer are sorted in two registers by a bitonic network, and a side with no rank in it is never visited. Groups are small (median 6 rows, 1,762 per hour on seed 1), so most spans go straight to the network. The text goes over the rows; hours are placed in hour order and each is written at its offset with `pwrite`, so finishing threads write at once (`Job::emit`).

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only. Each output number below 10^8 is made at once in one u64, the leading zeros shifted out, and one 8-byte store writes it; larger ones take two u64s and a 16-byte store (`put`, `decimal8`).

## Dependencies

None.

## With another 24 hours

Cut instructions: the finish's count pass could merge into the parse's row write, and the page faults of each hour's fresh region (~7,800 more than a-58's pooled buffers) could go with a small ring of reused regions. The line loop itself is at ~114 ms on one worker; HighVelocity's ~25 % fewer user instructions show the room.
