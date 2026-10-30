# Track B: Index

Track B (index), Rust, standard library only. K 16.46 = T 0.513 s, M 36.5 MB, S 824, D 1 (box sd-bench/B on commit fe9141a5).
Reproduce: `docker build -t b track-b && docker run --rm -v <data>:/data:ro -v <out>:/out b`.

- One pass over the memory-mapped corpus ([`map_file`](src/main.rs#L50)), one doc range per thread ([`scan`](src/main.rs#L596)), looking up only the query terms ([`Dict`](src/main.rs#L72)).
- Phrases come from an Aho-Corasick automaton over query-term ids with dictionary-suffix links ([`Automaton`](src/main.rs#L127)), run in the same pass.
- BOOL queries are decided per document inside the pass, with no posting list stored. Each query is evaluated only on the docs that contain one of its trigger terms ([`cover`](src/main.rs#L360)); the strict left fold is applied to the doc's term set ([`holds`](src/main.rs#L517)), and the smallest ids are shared across workers ([`FirstIds`](src/main.rs#L546)).
- Queries whose cover is estimated to hold at least 1 doc in 64 are folded 64 docs at a time with word operations ([`fold_window`](src/main.rs#L465)); counts come from popcount and id sums from bit-sliced popcounts ([`flush_window`](src/main.rs#L493)).
- Trigger terms and window queries are picked with document frequencies estimated on 1/128 of the corpus ([`estimate_df`](src/main.rs#L290)). The estimate only chooses an evaluation order; answers never depend on it.
- PERF.md: steps 1-4 are b-1 and steps 5-6 are b-2 on the live board; the b-3 and b-4 rows follow their board runs.
- Honest limit: the pass tokenizes byte by byte with a scalar loop, and that is now most of the run.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The program reads `queries.txt` first and interns every term in an open-addressing table that compares full bytes on a hit. Each distinct phrase becomes one pattern of an Aho-Corasick automaton whose alphabet is the query-term ids. A token that is no query term, or a term that appears in no phrase, sends the automaton straight back to its root.

Under the spec's strict left fold, whether a document is in a BOOL query's result depends only on which of the query's terms the document contains, so every query can be decided per document. A document can only be in R if it contains a term of the query's cover. The cover starts as the first term; each OR operand is added to it; an AND operand may replace the cover when its estimated document frequency is lower. Every query is listed, with its code inline, under each term of its cover. Before the pass, document frequencies are estimated on 64 evenly spaced windows covering 1/128 of the corpus, only to make those choices.

`docs.txt` is mapped read-only and cut into one line-aligned range per worker. Each worker tokenizes its documents byte by byte as the spec defines (ASCII lower-casing, runs of `a-z0-9`), hashing while it scans. A token found in the table sets that term's bit in a small per-document presence set and steps the automaton. Each state's dictionary-suffix link chain reports every pattern ending at that token, nested ones included, once per document.

Queries whose cover is estimated to hold at least one document in 64 take a window path instead: each worker keeps, for every term of those queries, one 64-bit word of the current 64-document window. When the window ends, it folds each such query once with AND, OR and AND-NOT on whole words. The match count is the popcount of the result. The id sum is the window base times that count, plus the sum over k of popcount(result AND mask_k) shifted left by k, where mask_k selects the bit positions whose bit k is set. First ids are taken from the word only while the query can still use them. On seed 1, 2,679 queries take this path, and the per-document evaluations drop from 91 M to 10.7 M.

At the end of each document, the worker evaluates every other query listed under the document's terms. Queries of OR operators only are matches as soon as they are triggered. A matching document adds to the worker's count and sum for that query and is offered to a shared set of the ten smallest ids, which a single atomic bound guards. After the pass, the workers' counts and sums are added up. PHRASE answers are the per-document matches the pass collected. `results.txt` is written once at the end.

## Dependencies

None.

## With another 24 hours

Replace the scalar tokenizer with a SIMD separator scan, trim the query-side tables (about 17 MB), and fold the window queries with AVX2 four windows or four queries at a time.
