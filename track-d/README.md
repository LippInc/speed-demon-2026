# Track D: Traverse

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/d-traverse.md).

One C file, `traverse.c`, built by gcc 15 for x86-64-v3 and linked statically. No dependencies. Measurements and every failed try: `PERF.md`.

Reproduce from the repository root:

```
python3 check.py d --data data/d --expected a23dbdf0a3e49cc8901310a9ee1dc6a9a569a85297b6da75c2a7002cb2ed277e
```

(full seed 1; the expected hash is `specs/d-traverse.md:203`.)

## How it works

The clock covers the whole process, so the design question is what to build before the queries, not only how to run them. The program goes through these phases on all workers, each worker waiting at a spin barrier between them.

1. **Parse** (`parse_piece`). The file is cut into 48 pieces that workers take from a counter. A 64-byte block gives a separator mask (AVX2), the masks of 4 KB are flattened to positions in one go, and then a line's three numbers are converted together in one vector register (`read_line`). So there is no refill branch per line and no per-number loop. Each edge goes into a cell of a grid (bucket of its tail, piece) with no bounds check: cells have room for the expected fill plus 8 standard deviations, and the edge lines are shuffled by the generator. The file pages a piece has used are dropped straight away, in parallel, so exit has nothing to tear down.
2. **First rows** (`build_bucket`). Each bucket's nodes get four 32-bit slots per node, head and 11-bit weight, so a row is one cache miss with no offset array. The rows are written over the bins of buckets already finished: buckets go in rounds of 12, and the rows start 2 MB before the bins. No new memory, no faults, no frees.
3. **Zones** (`grow_zone`). Each worker grows a breadth-first zone. A node with at most two ways out that is not a query endpoint has no real choice, so an edge into it keeps going along the road to the next node with a choice; roads that end nowhere or return to their start are dropped. Contraction, ordering and gathering are one pass, and the roads of a batch advance one hop per round so their cache misses overlap. Claims are plain byte writes: a lost race lists a node twice, and the copy that heads do not point at is a row no edge leads to, so answers stay exact.
4. **Final rows** (`rename_job`, `rev_count_job`, `rev_place_job`). The forward rows are written with the new ids. The reverse rows come from them slice by slice: an edge into the worker's own slice needs no atomics, the few that cross wait in a list. All of it lives in one contiguous run of huge pages, each faulted once by one thread; the reverse rows overwrite the id map.
5. **Queries** (`search`, `drain`). Bidirectional search, always expanding the side with fewer queued entries, stopping when the two bucket floors reach the best meeting. Buckets are 128 wide, with entries of 21 bits of node and 7 of remainder, and their arrays are recycled, so a thread's queue memory follows the frontier. Labels are 16 bits in 256-entry blocks handed out in order, so a thread's label memory follows its deepest search, and block 0 stays blank for nodes a side has not seen. The relaxation has no data-dependent branch: a pass over a bucket writes what improved to a list, and only those entries are queued and checked against the other side's label, because an edge that improves nothing cannot make a new meeting. An answer that cannot be proved below 65,535 is redone by a plain 64-bit Dijkstra, and a source with no in-edges is its first edge plus a normal search, bounded by the lightest edge's result.
6. **Output and exit.** One `write`, then `_exit`.

Exactness covers weight 0, parallel and duplicate edges, self-loops, isolated and unreachable nodes and 64-bit sums. Weights too big for the 11-bit field are in a small lock-free hash table.

## With another 24 hours

- A tighter build peak: the first rows, the zones' edge lists and the query scratch now peak at about the same height.
- Contracting beyond plain roads. Removing degree-3 nodes would shrink the searches further. Paper arithmetic says the extra build about cancels what the queries gain; it has not been built.
