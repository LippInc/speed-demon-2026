# Track A: Ingest

Reads 3,100,000 HTTP request log lines and, for every hour and endpoint that saw traffic, writes the request count, the nearest-rank p50, p95 and p99 latency, and the number and rate of 5xx responses.

Rust, standard library only. K 39.49 (box `sd-bench/A`, commit fa5a5d4, a-40; fresh, the lower reading; official 40.32): T 0.027-0.028 s, M 23.6 MB, S 392, D 1. The board's best run of this design: run-0c2ed3d6fdec, K 38.98, T 0.0299 s (a-29, the same hot loop before a-35's cold-code line cuts).

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/a-ingest.md):
`docker build -t a track-a && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out a && sha256sum <out>/summary.txt`

| Trick | Where | PERF |
|---|---|---|
| One read-only mapping, one line-aligned byte range per thread, private tables | [`line_ranges`](src/main.rs#L39), [`scan`](src/main.rs#L292) | a-1 |
| The endpoint's `/` and the line end from one 64-byte AVX2 window at byte 32 | [`line_fields`](src/main.rs#L67) | a-5, a-40a |
| Latency digits from one 16-byte load and SIMD multiply-adds | [`parse_latency`](src/main.rs#L104) | a-6, a-13 |
| Endpoints interned to small ids per thread by their first two 32-byte chunks (CRC32 hash, no byte compare call), copied to a pool, one global rank | [`Table`](src/main.rs#L178), [`global_ranks`](src/main.rs#L384) | a-10, a-17, a-40b |
| One 4-byte record per line in its hour's buffer, merged per hour by a counting sort | [`Hour`](src/main.rs#L164) | a-29 |
| Percentiles: up to 32 latencies sorted, larger groups selected | [`percentiles`](src/main.rs#L353) | a-20 |
| Each range's page-table entries dropped after its scan; glibc's mmap threshold pinned | [`release_range`](src/main.rs#L25), [`main`](src/main.rs#L402) | a-11, a-12 |

Every measurement, failed experiments included, with its board run ID: [PERF.md](PERF.md). Receipts: `reports/a/` at the repository root, one `harness-<commit>.txt` per measured submission commit, from the board's runs of that commit; each names the commit it measured, and the commit after a measured push only adds its receipt.

Limit: the scan and the join take about 20 ms of the 30 ms and the tail about 7.5 ms (a-33). Six rewrites of the scan's lookup and line walk read level or slower on the box (a-7, a-8, a-16, a-22, a-31, a-34); a-40 cut the scan's instructions per line instead (231 -> 189 on seed 1), and the scan fell about 3 ms per thread (16.1 -> 13.1 ms, probe 1b9b7de).

## Approach

Rust, standard library only. The binary links the C runtime statically (probe a-26), so no dynamic loader runs before main. The program maps `/data/requests.log` read-only (`src/main.rs` `main`) and
cuts it into one byte range per worker, each starting at a line start (`line_ranges`). Every thread walks its
range once with private tables, so threads share nothing (`scan`, `Table`), then drops that range's page-table
entries (`release_range`) so the process exit has fewer to tear down. The key has two levels: a small
open-addressing table interns each endpoint to a local id (its key is its first two 32-byte chunks, zero past its
end, with its length in the slot, hashed by CRC32 and compared as vectors with no byte compare call, a-40b; the keys
are freed after the scan, a-40d), and each line bumps its hour's request and 5xx
counters at that id and appends one 4-byte record (id and latency) to its hour's buffer, so a line costs one
probe in a cache-resident table, two increments and an append (per-hour records, a-29). The hour is parsed only when the 13-byte hour prefix (two u64 loads) differs from the previous line's;
one 64-byte AVX2 window at byte 32 of the line finds the endpoint's `/` and, for lines of up to 96 bytes, the LF
(`line_fields`, a-40a); one 16-byte load before the LF finds the space before the latency and converts its 1 to 10
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

Cut the line count further: S is 356 lines here (a-35 cut 11 from cold code), and K moves about as S^-0.15, so 300 lines would be about 2.6 % more K. Two rewrites of the endpoint lookup (a-22, a-31) and a line walk from 64-byte LF bitmasks (a-34) read level or lower on the box, so neither the lookup nor the LF search is where the scan waits.
