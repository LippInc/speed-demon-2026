# Track A: Ingest

K and its parts: pending the box's `sd-bench/A` status for this commit. Reproduce with
`sdrig run --track-dir track-a --track A --data <seed-1 full dir> --expected-sha <hash> --runs 5`.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Rust, standard library only. The program maps `/data/requests.log` read-only (`src/main.rs` `map_file`) and
cuts it into one byte range per worker, each starting at a line start (`line_ranges`). Every thread walks its
range once and keeps a private open-addressing table keyed by (hour, endpoint), so threads share nothing
(`scan`, `Table`). The hour is parsed only when the 13-byte hour prefix differs from the previous line's;
the endpoint starts at the first `/` after the method; latency and status are read backwards from the line end.
Each group keeps its latencies in a `Vec<u32>` and a 5xx count. At the end the per-thread groups are sorted
into the output order, equal keys are folded together, each group's latencies are sorted and the nearest-rank
percentiles are read off.

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only.

## Dependencies

None.

## With another 24 hours

Parallelise the merge and per-group sorting, which run on one thread after the scan, and replace the
per-group latency vectors with a more compact layout to cut memory.
