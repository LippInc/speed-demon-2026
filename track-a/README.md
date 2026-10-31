# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

K 38.29 (box `sd-bench/A`, fresh, the lower reading; official 38.55): T 0.030 s, M 23.1 MB, S 367, D 1,
sha 5/5+5/5 (a-29, commit d00aac9). Latest board run: run-ac92f2fd600e, K 32.28, for the previous tip (a-21 + a-26);
this tip's own board run lands with the next push.
Reproduce with `sdrig run --track-dir track-a --track A --data <seed-1 full dir> --expected-sha <hash> --runs 5`.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md).

## Approach

Rust, standard library only. The binary links the C runtime statically (probe a-26), so no dynamic loader runs before main. The program maps `/data/requests.log` read-only (`src/main.rs` `main`) and
cuts it into one byte range per worker, each starting at a line start (`line_ranges`). Every thread walks its
range once with private tables, so threads share nothing (`scan`, `Table`), then drops that range's page-table
entries (`release_range`) so the process exit has fewer to tear down. The key has two levels: a small
open-addressing table interns each endpoint to a local id, and each line bumps its hour's request and 5xx
counters at that id and appends one 4-byte record (id and latency) to its hour's buffer, so a line costs one
probe in a cache-resident table, two increments and an append (per-hour records, a-29). The hour is parsed only when the 13-byte hour prefix (two u64 loads) differs from the previous line's;
one 32-byte AVX2 load at byte 32 of the line finds the endpoint's `/` and, for most lines, the LF
(`line_fields`); one 16-byte load before the LF finds the space before the latency and converts its 1 to 10
digits with SIMD multiply-adds, with no per-digit loop (`parse_latency`); the 5xx flag is the status's first
digit.

Memory is kept small: a line takes one 4-byte record, the endpoint id in its low 11 bits and the latency above;
the rare line whose latency is 2^21 or more (or whose id is 2,047 or more, which spec 2's 1,842 endpoints never
reach) takes three words, so every u32 stays exact. Each (thread, hour) keeps its records in one buffer, with no
per-group allocation. glibc's mmap threshold is pinned so the
tail's freed output buffers go back to the kernel instead of staying charged. Each thread copies its few distinct endpoints into a small pool when
first seen, so nothing after the scan touches the released mapping. After the scan each thread sorts them, and
one merge, one step per distinct endpoint, gives every endpoint a global rank (`global_ranks`), so the output
order needs only integer sorts.

The tail runs in parallel, split by hour (the `format_hour` closure in `main`): hours are disjoint and contiguous in the output, so
each hour is merged across threads by a counting sort on global endpoint rank (counts summed per rank, prefix
sums, one pass placing each latency), its percentiles found (`percentiles`: groups of up to 32 latencies are
sorted, larger ones are selected in ascending rank order, each rank inside the part right of the previous one)
and its lines formatted by one thread. That hour's records are dropped as soon as they are placed, and a buffer above glibc's pinned 128 KiB mmap threshold goes back to the kernel at once.
The main thread writes the hours' buffers in output order as they finish.

The worker count is `available_parallelism`; `SD_WORKERS` overrides it for the hash tests only. Each
output number is written from its low end one digit per step.

## Dependencies

None.

## With another 24 hours

Cut the line count: S is 367 lines here, and K moves about as S^-0.15, so 300 lines would be about 3 % more K. Two rewrites of the endpoint lookup (a-22, a-31) and a line walk from 64-byte LF bitmasks (a-34) read level or lower on the box, so neither the lookup nor the LF search is where the scan waits.
