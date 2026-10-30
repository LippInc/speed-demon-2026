# Track B: Index

Track B (index), Rust, standard library only. K 15.06 = T 0.646 s, M 35.8 MB, S 694, D 1 (box sd-bench/B on commit eab53317; no live-board run yet).
Reproduce: `docker build -t b track-b && docker run --rm -v <data>:/data:ro -v <out>:/out b`.

- One pass over the memory-mapped corpus ([`map_file`](src/main.rs#L50)), one doc range per thread ([`scan`](src/main.rs#L498)), looking up only the query terms ([`Dict`](src/main.rs#L72)).
- Phrases come from an Aho-Corasick automaton over query-term ids with dictionary-suffix links ([`Automaton`](src/main.rs#L127)), run in the same pass.
- BOOL queries are decided per document inside the pass, with no posting list stored. Each query is evaluated only on the docs that contain one of its trigger terms ([`cover`](src/main.rs#L349)); the strict left fold is applied to the doc's term set ([`holds`](src/main.rs#L419)), and the smallest ids are shared across workers ([`FirstIds`](src/main.rs#L448)).
- Trigger terms are picked with document frequencies estimated on 1/128 of the corpus ([`estimate_df`](src/main.rs#L289)). The estimate only steers speed; answers never depend on it.
- PERF.md experiment: step 1 (row added once the live board has measured it).
- Honest limit: about 85 M (doc, query) matches on seed 1 must each be found and counted per document, so BOOL work is spread over the pass rather than done by bitmap word operations.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The program reads `queries.txt` first and interns every term in an open-addressing table that compares full bytes on a hit. Each distinct phrase becomes one pattern of an Aho-Corasick automaton whose alphabet is the query-term ids. A token that is no query term, or a term that appears in no phrase, sends the automaton straight back to its root.

Under the spec's strict left fold, whether a document is in a BOOL query's result depends only on which of the query's terms the document contains, so every query can be decided per document. A document can only be in R if it contains a term of the query's cover. The cover starts as the first term; each OR operand is added to it; an AND operand may replace the cover when its estimated document frequency is lower. Every query is listed, with its code inline, under each term of its cover. Before the pass, document frequencies are estimated on 64 evenly spaced windows covering 1/128 of the corpus, only to make those choices.

`docs.txt` is mapped read-only and cut into one line-aligned range per worker. Each worker tokenizes its documents byte by byte as the spec defines (ASCII lower-casing, runs of `a-z0-9`), hashing while it scans. A token found in the table sets that term's bit in a small per-document presence set and steps the automaton. Each state's dictionary-suffix link chain reports every pattern ending at that token, nested ones included, once per document.

At the end of each document, the worker evaluates every query listed under the document's terms. Queries of OR operators only are matches as soon as they are triggered. A matching document adds to the worker's count and sum for that query and is offered to a shared set of the ten smallest ids, which a single atomic bound guards. After the pass, the workers' counts and sums are added up. PHRASE answers are the per-document matches the pass collected. `results.txt` is written once at the end.

## Dependencies

None.

## With another 24 hours

Replace the scalar tokenizer with a SIMD separator scan, trim the query-side tables (about 17 MB), and give the queries with the largest results (about 1,000 queries hold 66 M of the 85 M matches on seed 1) a cheaper per-document path.
