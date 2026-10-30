# Track B: Index

Track B (index), Rust, standard library only. K 13.43 = T 0.600 s (fresh; official 0.580 s), M 88.9 MB, S 699, D 1 (box sd-bench/B on commit 2c8a545c; no live-board run yet).
Reproduce: `docker build -t b track-b && docker run --rm -v <data>:/data:ro -v <out>:/out b`.

- One pass over the memory-mapped corpus ([`map_file`](src/main.rs#L49)), one doc range per thread ([`scan`](src/main.rs#L410)), indexes only the query terms ([`Dict`](src/main.rs#L71)).
- Phrases come from an Aho-Corasick automaton over query-term ids with dictionary-suffix links ([`Automaton`](src/main.rs#L126)), run in the same pass.
- Postings are varint doc gaps in per-list block chains that double from 8 B to 256 B, with a 4 B link and an 8 B head per term and worker ([`push`](src/main.rs#L320), [main.rs:252](src/main.rs#L252)), plus bitmaps for terms with df > N/8 ([main.rs:663](src/main.rs#L663)); BOOL is a strict left fold over bitmaps ([`eval`](src/main.rs#L543)).
- PERF.md experiment: step 1 (row added once the live board has measured it).
- Honest limit: M is still about twice the 44 MB cap: 12 workers keep 728,779 per-worker lists of 29.2 MB of varints in 47.6 MB of blocks, plus 7.3 MB of heads and 17 MB of query-side tables.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The program reads `queries.txt` first and interns every term in an open-addressing table that compares full bytes on a hit. Each distinct phrase becomes one pattern of an Aho-Corasick automaton whose alphabet is the query-term ids. A token that is no query term, or a term that appears in no phrase, sends the automaton straight back to its root.

`docs.txt` is mapped read-only and cut into one line-aligned range per worker. Each worker tokenizes its documents byte by byte as the spec defines (ASCII lower-casing, runs of `a-z0-9`), hashing while it scans. A token found in the table adds the document to that term's posting list, once per document, and steps the automaton. A list is a chain of blocks that double in size from 8 B to 256 B; each size class fills its own slabs, so a block is aligned to its size and needs only a 4 B link back to the previous block. Each state's dictionary-suffix link chain reports every pattern ending at that token, nested ones included, once per document.

After the pass, every term whose document frequency is above N/8 also gets an N-bit bitmap. BOOL queries, deduplicated by line, are evaluated on all threads as a strict left fold on a bitmap accumulator: OR and NOT with a sparse list touch only that list's documents. PHRASE answers are the per-document matches the pass already collected. `results.txt` is written once at the end.

## Dependencies

None.

## With another 24 hours

Evaluate BOOL queries per document inside the pass so no posting list is kept at all, try the u16 shard layout against varints on the box's M, and replace the scalar tokenizer with a SIMD separator scan.
