# Track A: Ingest

K 24.07 (box `sd-bench/A`, official; fresh 24.15): T 0.065 s, M 29.6 MB, S 473, D 1, sha 5/5+5/5.
Reproduce with `sdrig run --track-dir track-a --track A --data <seed-1 full dir> --expected-sha <hash> --runs 5`.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Rust, standard library only. The program maps `/data/requests.log` read-only (`src/main.rs` `map_file`) and
cuts it into one byte range per worker, each starting at a line start (`line_ranges`). Every thread walks its
range once and keeps a private open-addressing table keyed by (hour, endpoint), so threads share nothing
(`scan`, `Table`). The hour is parsed only when the 13-byte hour prefix differs from the previous line's;
the endpoint starts at the first `/` after the method; latency and status are read backwards from the line end.

Memory is kept small: a group is 24 bytes (endpoint as an offset into the mapping, u32 counts), and its
latencies go into chains of small chunks inside large per-(thread, hour) blocks (`Arena`), so there is no
per-group allocation and no vector growth slack. Each thread sorts its groups by key.

The tail runs in parallel, split by hour (`format_hour`): hours are disjoint and contiguous in the output, so
each hour is merged across threads, its percentiles found by selection (`percentiles`) and its lines formatted
by one thread. That hour's latency blocks are dropped as soon as it is formatted, which returns their pages.
The main thread writes the hours' buffers in output order as they finish.

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only.

## Dependencies

None.

## With another 24 hours

Speed up the per-line scan (a word-at-a-time search for the line end and the endpoint, one table probe per
line), then write the output in place.
