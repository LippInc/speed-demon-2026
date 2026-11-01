# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

Rust, standard library only. K 43.58 / 43.80 (box `sd-bench/A`, commit 44579b2, a-56 forward line walk; lower: fresh; official 44.64 / 43.89): T 0.022-0.023 s, M 20.7 MB, S 464, D 1, against the banked board code a6b3205 (a-55 trim) at 42.54 / 43.04. The board has run a6b3205's code since push f245483 (run-8c41cebcaeef, 80.4 points; re-runs on later pushes 80.2-81.0 points, latest run-6648069094f7). Threads claim 256 KB blocks of the file and parse them into per-hour records; an hour is finished as soon as the registered prefix of the file holds a line two hours later; a line that arrives after its hour was finished makes the run repeat with no early finishing, so the output is exact for any order.

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md):
`docker build -t a track-a && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out a && sha256sum <out>/summary.txt`

| Trick | Where | PERF |
|---|---|---|
| The pipeline: threads claim 256 KiB blocks and parse into per-hour records; an hour finished early once prefix is 2 hours ahead | [`Job::work`](src/main.rs#L411), [`parse`](src/main.rs#L276), [`State::register`](src/main.rs#L364) | a-55 |
| Forward line walk: one 64-byte space mask from byte 28 finds the ends of method and endpoint; status and latency at fixed offsets; latency digits read 8 per load and converted with two SIMD multiply-adds | [`line`](src/main.rs#L68), [`digits8`](src/main.rs#L53) | a-56 |
| Endpoints interned to small ids: first two 32-byte chunks compared as vectors, 128-byte images, new ids sorted in by finishing threads | [`Table::find`](src/main.rs#L213), [`Dict::order`](src/main.rs#L168) | a-10, a-40b, a-55 |
| Counting-sort tail: records placed into scratch by endpoint byte order, percentiles selected, in-order output stream | [`Job::finish`](src/main.rs#L446), [`percentiles`](src/main.rs#L335) | a-20, a-29, a-55 |
| Exactness on late lines: repeat with no early finishing if a line arrives after its hour was claimed | [`run`](src/main.rs#L522), [`main`](src/main.rs#L574-L580) | a-55 |
| Memory: mapping released in 4 MiB steps; one malloc arena; glibc mmap threshold pinned | [`Job::release`](src/main.rs#L512), [`main`](src/main.rs#L545-L550) | a-11, a-12, a-55 |

Every measurement, failed experiments included, with its board run ID: [PERF.md](PERF.md). Receipts: `reports/a/` at the repository root, one `harness-<commit>.txt` per measured submission commit, from the board's runs of that commit; each names the commit it measured, and the commit after a measured push only adds its receipt.

Limit: the line loop is throughput-bound (IPC 2.67 on one worker; hyperthreads fill the core at 12 workers), so overlapping chains by interleaving line streams gave no gain (a-55 r3). Further speed must come from fewer instructions per line (HighVelocity parses latency forward with SWAR without searching '/' or loading backwards) or from finish (~1.1 ms per hour).

## Approach

Rust, standard library only. The binary links the C runtime statically (probe a-26), so no dynamic loader runs before main. The program maps `/data/requests.log` read-only (`src/main.rs` `main`) and claims it in 256 KiB blocks in file order (`Job::align`, `Job::work`). Each thread walks its claimed blocks with private open-addressing tables (`parse`, `Table`), parsing lines into per-hour 64-bit records (global endpoint id, 5xx flag, and latency).

The pipeline finishes hours as soon as the registered prefix of the file holds a line two hours later (LAG 2), so threads do not wait for the whole scan to complete (`Job::work`). If a line arrives for an hour that was already claimed for finishing, the run is marked late, and `main` repeats the entire run with no early finishing (LAG = HOURS), ensuring the output is exact for any line order (`run`, `main`).

The key has two levels: a small open-addressing table interns each endpoint to a local id (its key is its first two 32-byte chunks, zero past its end, with its length in the slot, hashed by CRC32 and compared as vectors with no byte compare call, a-40b; the keys are 128-byte aligned name images published by an atomic counter, a-55 r2d). The lock on adding new names covers only the id; finishing threads sort newly published ids in byte order via `Dict::order` (a-55 r2b). The hour prefix is compared as two u64 words; the timestamp is always 27 bytes, so one 64-byte space mask from byte 28 of the line holds the ends of the method and the endpoint (`line`, a-56); the status and the latency follow at fixed offsets, and the latency's digits are read 8 per load and converted with two SIMD multiply-adds (`digits8`, a-56).

Memory is kept small: one malloc arena (`mallopt(M_ARENA_MAX, 1)`, a-55 r2a) allows freed record buffers to be reused across threads without per-thread heap slack; glibc's mmap threshold is pinned at 128 KiB (`main`, a-11) so output buffers go back to the kernel at once. Once all hours are claimed, threads release the input mapping's page-table entries in shared 4 MiB steps (`Job::release`, a-12, a-55) so process exit has fewer to tear down.

The tail runs in parallel as hours become ready (`Job::finish`): each hour's records are counted per endpoint and 5xx flag, placed into scratch by counting sort in endpoint byte order, their percentiles found (`percentiles`: groups of up to 32 latencies sorted, larger ones selected in ascending rank order), and lines formatted. That hour's records are freed before formatting, and hours are written to `/out/summary.txt` in hour order as they finish (`Job::emit`).

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only. Each output number is written from its low end one digit per step (`push_u64`).

## Dependencies

None.

## With another 24 hours

Cut the line count further: S is 463 lines here (the pipeline's State, Out, Job and synchronization added lines, trimmed to 463 in a-55 trim). A leaner line parse (HighVelocity's forward SWAR latency parse without searching '/' or loading backwards) targets -20-30 % parse instructions per line.
