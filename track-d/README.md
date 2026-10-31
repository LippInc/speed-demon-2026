# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

K 17.73 on box 2 (i5-13500, 5 official runs on full seed 1 + 5 runs on one fixed fresh seed; K is the lower), commit 7068578: fresh K 17.73 at T 0.229 s, official K 17.91 at T 0.224 s, M 119.4 MB, S 610 code lines, D: no dependencies (box 2 readings in PERF.md).

Reproduce from the repository root:

```
python3 check.py d --data data/d --expected a23dbdf0a3e49cc8901310a9ee1dc6a9a569a85297b6da75c2a7002cb2ed277e
```

(full seed 1; the expected hash is `specs/d-traverse.md:203`.)

| Trick | Where | PERF.md |
|---|---|---|
| Parallel two-pass parse into a forward and a reverse CSR, no atomics | `src/main.rs:170` | d-8: Graph build without atomics, written compactly |
| Six-byte CSR entries, heavy weights in a side table | `src/main.rs:92` | d-12b: Six-byte CSR entries |
| Claim-BFS relabel for locality | `src/main.rs:229` | d-13: Node relabel for locality |
| Bidirectional early-exit Dijkstra, cheap answers first | `src/main.rs:606` | d-2 floor (row 1) |
| Per-thread direct-mapped label table with hashed overflow | `src/main.rs:559` | d-15b: Direct-mapped label table on the relabelled ids |
| Queries run in relabelled order | `src/main.rs:698` | d-15a: Queries run in order of their relabelled source and target |
| Static C runtime; the only C is this extern block | `src/main.rs:16` | d-14: Static C runtime |
| Prefetch of the next pops' CSR rows | `src/main.rs:638` | d-10a: Prefetch of the CSR rows of the next pops in the query loop |
| Monotone radix heap for both search sides | `src/main.rs:355` | d-5: Monotone radix heap |
| Small L2-sized label table, large one on overflow | `src/main.rs:518` | d-6: Label-map locality |

Official harness output for each measured submission commit: [reports/d/](../reports/d/) at the repository root, one file per commit, harness-<commit>.txt (the board's own record of that run).
The numbers in reports/ name the code commit they measured; the commit after it only adds those reports.

Honest limit: the query phase is down to about 0.09 s of box T (REPORT-d-15.md), so the relabel (about 0.05 s) and the parse and build (about 0.08 s) hold a large share of what is left (REPORT-d-13b.md); M 119.4 MB sits close to its floor from every side (fwd 108 MB, rev 116 MB, queries 118 MB, REPORT-d-12.md).

## Approach

One Rust file. Program order: map the input, parse it into both CSRs, relabel the nodes, answer the queries, write the distances.

**Parse and build.** The input file is mapped read-only and never copied (`src/main.rs:27`); a decimal reader walks the mapped bytes (`src/main.rs:50`). Each worker parses its byte range once and files every edge into a bucket for the tail's partition (`src/main.rs:299`). One placement routine then serves the forward CSR and both reverse slabs: per partition a local degree count, a prefix sum, and a scatter into that partition's own slots, with no atomic adds (`src/main.rs:170`). Why: the atomic build paid two locked adds per edge into scattered slots; without them the build fell from 104.6 ms to 85 ms on the box (REPORT-d-7.md).

**CSR layout.** An entry is six bytes: the head as two 16-bit halves and a 16-bit weight; weights of 65,535 or more live in a side table sorted by edge index (`src/main.rs:92`). On full seed 1, 50 of the 5,000,000 weights need the side table. Why: the two CSRs were most of the query phase's memory; six bytes cut them from 80 MB to 60 MB and moved box M from 141.4 to 125.3 MB with no time cost (PERF.md, d-12b).

**Relabel for locality.** Workers run claim-based breadth-first searches from spread roots, and the new node ids follow those search orders, so nodes near in the graph get near ids (`src/main.rs:229`). The forward rows are then copied row by row in old-id order (`src/main.rs:262`), and the old forward buffers are reused for the reverse build (`src/main.rs:315`). Why: the input's ids are a random permutation, so one settle's neighbours sat in scattered CSR rows; on the box, the base before the locality work read T 0.271/0.285 s, and the code with the relabel, the query order and the direct-mapped table reads 0.225/0.230 s (PERF.md, d-12b and d-15b).

**Queries.** Cheap answers first: s == t, or a target with no in-edges or a source with no out-edges, before any search (`src/main.rs:665`). Otherwise a bidirectional early-exit Dijkstra (`src/main.rs:606`): the sides take turns on the smaller heap, mu is updated on every relaxation that reaches a node the other side has labelled, and the search stops once the two heap tops sum to mu or more (`src/main.rs:623`). Labels live in per-thread tables with no n-sized array (`src/main.rs:518`): a node's label sits in the slot given by the low bits of its relabelled id, so neighbours' labels share cache lines (`src/main.rs:560`); a slot taken by another node sends the label to hashed tables (`src/main.rs:572`), which double only for the searches that need more. Clearing goes through a touched list, so it costs the labels used, not the table size (`src/main.rs:492`). While settling a node, the edge rows of the next eight pops start loading (`src/main.rs:638`). Queries run sorted by their relabelled (s, t), so workers searching at the same time share CSR rows in cache (`src/main.rs:698`).

**Heaps.** Both search sides use one monotone radix heap (`src/main.rs:355`): a push is a bucket append, the top is the exact minimum, and an entry moves to lower buckets at most 64 times, instead of paying log-size compares per pop.

**Process.** Worker count comes from the available parallelism (`src/main.rs:41`). glibc's mmap threshold is pinned at 256 KiB at start, so freed build buffers leave the malloc arena (`src/main.rs:678`). The binary links the C runtime statically; the only C is the two-function extern block for mmap and mallopt (`src/main.rs:16`). Answers are written in input order (`src/main.rs:715`), and one stderr line reports the phase times (`src/main.rs:732`).

## Dependencies

None. The Rust standard library only. The two C calls (mmap, mallopt) go through a short `extern "C"` block (`src/main.rs:16`), and the binary links the C runtime statically (PERF.md, d-14).

## With another 24 hours

- SWAR parsing in the parse threads: the box's parse is about 27 ms and would drop to about 15 ms (REPORT-d-13a.md).
- Relabel without the copy: keep the old forward rows, rewrite only the heads and the row ends in BFS order in one streaming pass (REPORT-d-13.md).
- Build the reverse CSR from the relabelled records inside that streaming pass, skipping the separate reverse build (REPORT-d-13b.md).
- A two-level heap (exact small buckets above the radix buckets) to cut the radix heap's refill work, still exact (REPORT-d-5.md).
