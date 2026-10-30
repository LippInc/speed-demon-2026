# Track B: Index

Track B (index), Rust, standard library only. K: pending the first live-board run (box status on exp/b-1-first in REPORT-b-1.md).
Reproduce: `docker build -t b track-b && docker run --rm -v <data>:/data:ro -v <out>:/out b`.

- One pass over the memory-mapped corpus ([`map_file`](src/main.rs#L49)), one doc range per thread ([`scan`](src/main.rs#L378)), indexes only the query terms ([`Dict`](src/main.rs#L71)).
- Phrases come from an Aho-Corasick automaton over query-term ids with dictionary-suffix links ([`Automaton`](src/main.rs#L126)), run in the same pass.
- Postings are varint doc gaps in growing fixed-size blocks ([`push`](src/main.rs#L297)), plus bitmaps for terms with df > N/8 ([main.rs:627](src/main.rs#L627)); BOOL is a strict left fold over bitmaps ([`eval`](src/main.rs#L507)).
- PERF.md experiment: step 1 (row added once the live board has measured it).
- Honest limit: the block arena over-allocates (about 54 MB allocated for 29 MB of varints in one thread), so M is several times the 44 MB cap.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The program reads `queries.txt` first and interns every term in an open-addressing table that compares full bytes on a hit. Each distinct phrase becomes one pattern of an Aho-Corasick automaton whose alphabet is the query-term ids. A token that is no query term, or a term that appears in no phrase, sends the automaton straight back to its root.

`docs.txt` is mapped read-only and cut into one line-aligned range per worker. Each worker tokenizes its documents byte by byte as the spec defines (ASCII lower-casing, runs of `a-z0-9`), hashing while it scans. A token found in the table adds the document to that term's posting list, once per document, and steps the automaton. Each state's dictionary-suffix link chain reports every pattern ending at that token, nested ones included, once per document.

After the pass, every term whose document frequency is above N/8 also gets an N-bit bitmap. BOOL queries, deduplicated by line, are evaluated on all threads as a strict left fold on a bitmap accumulator: OR and NOT with a sparse list touch only that list's documents. PHRASE answers are the per-document matches the pass already collected. `results.txt` is written once at the end.

## Dependencies

None.

## With another 24 hours

Cut the arena's slack (smaller growth steps, or sizing lists exactly), try the u16 shard layout against varints on the box's M, and replace the scalar tokenizer with a SIMD separator scan.
