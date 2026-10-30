# Track A: Ingest

K 32.08 (box `sd-bench/A`, official, the lower reading; fresh 32.24): T 0.035 s, M 25.3 MB, S 651, D 1,
sha 5/5+5/5.
Reproduce with `sdrig run --track-dir track-a --track A --data <seed-1 full dir> --expected-sha <hash> --runs 5`.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Rust, standard library only. The program maps `/data/requests.log` read-only (`src/main.rs` `map_file`) and
cuts it into one byte range per worker, each starting at a line start (`line_ranges`). Every thread walks its
range once with private tables, so threads share nothing (`scan`, `Table`), then drops that range's page-table
entries (`release_range`) so the process exit has fewer to tear down. The key has two levels: a small
open-addressing table interns each endpoint to a local id, and each hour keeps an array from endpoint id to
its group, so a line costs one probe in a cache-resident table plus an array index. The hour is parsed only when the 13-byte hour prefix (two u64 loads) differs from the previous line's;
one 32-byte AVX2 load at byte 32 of the line finds the endpoint's `/` and, for most lines, the LF
(`line_fields`); one 16-byte load before the LF finds the space before the latency and converts its 1 to 10
digits with SIMD multiply-adds, with no per-digit loop (`parse_latency`); the 5xx flag is the status's first
digit.

Memory is kept small: a group is 16 bytes (u32 counts and chunk pointers; its key is its place in the hour's
array), and its latencies go into chains of small chunks inside large per-(thread, hour) blocks (`Arena`), so
there is no per-group allocation and no vector growth slack. A latency takes 3 bytes; the rare value of
0xFF0000 or more takes two 3-byte slots, so every u32 stays exact. glibc's mmap threshold is pinned so the
tail's freed output buffers go back to the kernel instead of staying charged. Each thread copies its few distinct endpoints into a small pool when
first seen, so nothing after the scan touches the released mapping. After the scan each thread sorts them, and
one merge, one step per distinct endpoint, gives every endpoint a global rank (`global_ranks`), so the output
order needs only integer sorts.

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
