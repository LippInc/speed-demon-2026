# Track A: Ingest

K 21.02 (box `sd-bench/A`, fresh; official 21.12): T 0.074 s, M 50.7 MB, S 357, D 1, sha 5/5+5/5. Reproduce with
`sdrig run --track-dir track-a --track A --data <seed-1 full dir> --expected-sha <hash> --runs 5`.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Rust, standard library only. The program maps `/data/requests.log` read-only (`src/main.rs` `map_file`) and
cuts it into one byte range per worker, each starting at a line start (`line_ranges`). Every thread walks its
range once and keeps a private open-addressing table keyed by (hour, endpoint), so threads share nothing
(`scan`, `Table`). The hour is parsed only when the 13-byte hour prefix differs from the previous line's;
the endpoint starts at the first `/` after the method; latency and status are read backwards from the line end.
Each group keeps its latencies in a `Vec<u32>` and a 5xx count, and each thread sorts its groups by key.

The tail runs in parallel too, split by hour (`format_hour`): hours are disjoint and contiguous in the output,
so each hour is merged across threads, its percentiles found by selection (`percentiles`) and its lines
formatted by one thread with no locks. The main thread writes the hours' buffers in output order as they finish.

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only.

## Dependencies

None.

## With another 24 hours

Cut memory: smaller per-thread table entries and latency storage without vector growth slack, then write the
output in place.
