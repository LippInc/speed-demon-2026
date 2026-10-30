# Track A: Ingest

K 28.54 (box `sd-bench/A`, official; fresh 28.71): T 0.044 s, M 27.9 MB, S 583, D 1, sha 5/5+5/5.
Reproduce with `sdrig run --track-dir track-a --track A --data <seed-1 full dir> --expected-sha <hash> --runs 5`.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Rust, standard library only. The program maps `/data/requests.log` read-only (`src/main.rs` `map_file`) and
cuts it into one byte range per worker, each starting at a line start (`line_ranges`). Every thread walks its
range once with private tables, so threads share nothing (`scan`, `Table`). The key has two levels: a small
open-addressing table interns each endpoint to a local id, and each hour keeps an array from endpoint id to
its group, so a line costs one probe in a cache-resident table plus an array index. The hour is parsed only when the 13-byte hour prefix differs from the previous line's;
one 32-byte AVX2 load at byte 32 of the line finds the endpoint's `/` and, for most lines, the LF
(`line_fields`); a 16-byte compare before the LF finds the space before the latency (`latency_space`), and
latency and status are parsed from there.

Memory is kept small: a group is 20 bytes (u32 counts and chunk pointers; its key is its place in the hour's
array), and its latencies go into chains of small chunks inside large per-(thread, hour) blocks (`Arena`), so
there is no per-group allocation and no vector growth slack. After the scan each thread sorts its few distinct
endpoints, and one merge gives every endpoint a global rank (`global_ranks`), so the output order needs only
integer sorts.

The tail runs in parallel, split by hour (`format_hour`): hours are disjoint and contiguous in the output, so
each hour is merged across threads, its percentiles found by selection (`percentiles`) and its lines formatted
by one thread. That hour's latency blocks are dropped as soon as it is formatted, which returns their pages.
The main thread writes the hours' buffers in output order as they finish.

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only.

## Dependencies

None.

## With another 24 hours

Balance the scan threads (some ranges run slower on the box), make the endpoint probe branch-free (SIMD tag
groups), and parse the latency without a loop.
