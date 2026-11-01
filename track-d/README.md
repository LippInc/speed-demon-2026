# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

Current board numbers (repo commit a188bdf, board row 63, run-e22137237ce5): T 0.19 s, M 111 MB, 64.8 points; board K 19.80 at S 681 code lines; no dependencies (stdlib only).

Reproduce from the repository root:

```
python3 check.py d --data data/d --expected a23dbdf0a3e49cc8901310a9ee1dc6a9a569a85297b6da75c2a7002cb2ed277e
```

(full seed 1; the expected hash is `specs/d-traverse.md:203`.)

| Trick | Where | PERF.md entry |
|---|---|---|
| Four-byte packed CSR edges with side table escapes | `src/main.rs:82` | d-27: Four-byte packed edges |
| Parallel two-pass forward CSR placement without atomics | `src/main.rs:170` | d-8: Graph build without atomics, written compactly |
| Parallel claim-BFS relabel with chunked roots | `src/main.rs:230` | d-13b: Node relabel for locality |
| Parallel relabel new_id and row-end assignment | `src/main.rs:265` | d-26: Parallel relabel loop |
| Zero-allocation reverse CSR reusing forward buffers | `src/main.rs:337` | d-13b: Buffer reuse |
| Reverse CSR build via atomic in-degree count and placement | `src/main.rs:346` | d-28: Reverse CSR by in-degree count and atomic placement |
| Monotone bucket queue (2048 x 32) with bitmask scan | `src/main.rs:399` | d-27: Monotone bucket queue |
| Small L2-sized label table, large table on overflow | `src/main.rs:495` | d-6: Label-map locality |
| Two-tier scratch table with 64K direct-mapped slots | `src/main.rs:583` | d-15b: Direct-mapped label table on relabelled ids |
| Prefetch of next 8 pops' CSR rows during bucket drain | `src/main.rs:665` | d-10a: Prefetch of CSR rows of next pops |
| Bidirectional Dijkstra with 256-settle forward probe | `src/main.rs:706` | d-27: Forward probe and bidirectional search |
| Work-balanced search expanding side with fewer queued entries | `src/main.rs:737` | d-27 r1: Work-balanced search |
| Cheap answers first before any heap search | `src/main.rs:746` | d-2 floor: Reference to step 1 |
| Pinned glibc mmap threshold (256 KiB) via mallopt | `src/main.rs:759` | d-13b: Pinned mmap threshold |
| Queries sorted by relabelled (s, t) for cache reuse | `src/main.rs:784` | d-15a: Queries run in order of relabelled ids |
| Static C runtime via extern block (mmap, mallopt) | `src/main.rs:15` | d-14: Static C runtime |

Official harness output for each measured submission commit: reports/d/ at the repository root, one file per commit, harness-<commit>.txt (the board's own record of that run).

Honest limit: memory. Both CSRs together are about 61 MB (2 x (20 MB edges + 7 MB row ends) + 7 MB id map). Two phases peak at about the same height: the forward build (12-byte parse records, 5,000,000 x 12 B = 60 MB, beside the forward CSR) and the queries (per-thread scratch), about 95-99 MB each on the laptop. Freeing the query scratch early (d-28 r1, not shipped) brought the queries to 91 MB and box M did not move, because the forward build then set the peak.

## Approach

One Rust file (`src/main.rs`). Program order: pin the allocator threshold, map input files, parse into forward CSR, relabel nodes for locality, build reverse CSR in reused buffers, answer queries across worker threads, write output.

**Parse and build.** The input file is mapped read-only with private pages (`src/main.rs:24`). A decimal reader walks the mapped byte slice directly without allocating strings (`src/main.rs:39`). Workers parse disjoint byte ranges into per-partition buckets (`src/main.rs:314`). The forward CSR is placed per partition with a local degree count, prefix sum, and scatter, with no locked adds (`src/main.rs:170`). Once forward rows are relabelled into new node IDs, the reverse CSR is built directly into the reused memory of the old forward CSR (`src/main.rs:337`). The reverse build runs in two atomic passes: pass 1 counts in-degrees atomically into `rev_end` (`src/main.rs:346`), a serial prefix sum turns degrees into row starts (`src/main.rs:355`), and pass 2 scatters packed reverse edges at atomic cursors (`src/main.rs:364`). This eliminates intermediate edge slabs, cutting peak reverse build memory by ~30 MB and dropping local reverse build time from 24-45 ms to 12-16 ms.

**CSR layout.** Edges are packed into 4 bytes (`src/main.rs:79`). The head node ID occupies the top `hb` bits (`hb = 32 - leading_zeros(n - 1)`), and the weight occupies the remaining `32 - hb` bits (`src/main.rs:82`). For 1,740,512 nodes, `hb` is 21 bits, leaving 11 bits for weights up to 2046. Weights of 2047 or higher escape to a sorted side table (`heavy`). At 4 bytes per edge, 5,000,000 edges take 20 MB per direction, down from 80 MB at 8 bytes and 60 MB at 6 bytes.

**Relabel for locality.** The input graph randomly permutes node IDs, so adjacent nodes sit in scattered DRAM pages. Workers run parallel breadth-first searches from spread seed nodes (`src/main.rs:230`). Nodes are claimed via atomic boolean flags. Blocked workers claim root batches of 4096 nodes from a shared counter (`src/main.rs:212`). Once orders are collected, each worker assigns new IDs and computes row ends for its own block in parallel (`src/main.rs:265`), eliminating a 23 ms serial walk. Forward rows are copied into new ID order (`src/main.rs:276`), and old forward buffers are returned to the reverse build. Nearby nodes in the graph receive nearby IDs, so neighbour lookups during queries share cache lines.

**Queries.** Cheap checks resolve degenerate queries before touching search scratch: identical source and target (`s == t`), target with in-degree 0, or source with out-degree 0 return immediately (`src/main.rs:746`). Searches begin with a forward-only probe capped at 256 settles (`src/main.rs:712`). If target `t` is reached and its distance is final (`dt <= lower_f`), the probe returns immediately without initializing the backward queue (`src/main.rs:716`). For longer paths, the backward search is initialized from `t`. The query loop alternates expansion by selecting the side with fewer queued entries (`src/main.rs:737`), balancing search work rather than radii and preventing asymmetric stalls. Search stops when `lower_f + lower_b >= best` (`src/main.rs:732`). During relaxation, edge rows of the next 8 pop candidates are prefetched (`src/main.rs:665`, `src/main.rs:672`). Queries are sorted by relabelled `(s, t)` before execution (`src/main.rs:784`), allowing concurrently running workers to share active CSR rows in CPU cache.

**Bucket queues.** Both search sides use a monotone bucket queue (`src/main.rs:399`) consisting of a circular ring of 2048 buckets of width 32 (`WB = 32`), spanning a distance window of 65,536. Next non-empty buckets are located in O(1) time using bitwise trailing-zero counts over 64-bit occupancy bitmasks (`src/main.rs:454`). Entries exceeding the current ring window spill to an overflow binary heap (`src/main.rs:442`) and refill into the ring as `base_bucket` advances (`src/main.rs:446`). This avoids logarithmic heap comparison overhead.

**Scratch tables.** Search state uses a two-tier per-thread structure (`src/main.rs:587`). Each thread owns a 64K direct-mapped table (`DIRECT_SLOTS = 65536`, `src/main.rs:583`). Relabelled node IDs map to slot `node & 65535`. A slot is claimed if vacant or already holding that node. Conflicting nodes overflow to an open-addressing table sized to L2 cache (`SMALL_SLOTS = 16384`, 320 KB, `src/main.rs:495`), which doubles into a large table only if load exceeds 7/10 (`src/main.rs:645`). Tables are reset between queries using touched lists (`src/main.rs:561`), avoiding O(n) reinitializations.

**Process.** Thread count defaults to `available_parallelism` or the `SD_WORKERS` override (`src/main.rs:764`). glibc's dynamic mmap threshold is pinned to 256 KiB via `mallopt(-3, 1 << 18)` (`src/main.rs:759`), ensuring deallocated build buffers return to the operating system rather than fragmenting the malloc arena. The binary links the C runtime statically (`src/main.rs:15`). Output strings are formatted and written in one pass (`src/main.rs:803`). A single stderr line prints phase timings and settle metrics (`src/main.rs:812`).

## Dependencies

None. The Rust standard library only. The two C calls (mmap, mallopt) go through a short `extern "C"` block (`src/main.rs:15`), and the binary links the C runtime statically.

## With another 24 hours

- SWAR or SIMD integer parsing: the parse phase is about 26 ms on the bench box (25.8-26.1 ms); how much SIMD decimal parsing saves is not measured.
- Cut the forward-phase memory peak with 8-byte place records (tail offset inside its bucket plus head, weight packed as in an edge) instead of 12-byte ones; placing straight from the parse was tried and was slower (d-28 s4 in PERF.md).
- In-place BFS relabelling: update heads and degrees directly without allocating intermediate row copies.
- Then re-measure the parked query-scratch frees (d-28 r1) on box M over 15 or more runs: with the forward peak gone they can show.
