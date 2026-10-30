# Track B: Index

Track B (index), Rust, standard library only. K 19.95 = T 0.385 s, M 27.0 MB, S 595, D 1 (box sd-bench/B on commit 2a2b859c).
Reproduce: `docker build -t b track-b && docker run --rm -v <data>:/data:ro -v <out>:/out b`.

- One pass over the memory-mapped corpus ([`map_file`](src/main.rs#L34)), one doc range per thread ([`scan`](src/main.rs#L512)), looking up only the query terms in a compact table ([`TermTable`](src/main.rs#L64)).
- Phrases are matched by anchor checks in the same pass: each phrase is checked only where its rarest term occurs, by comparing term ids ([`PhrasePlan`](src/main.rs#L200)).
- BOOL queries are decided per document inside the pass, with no posting list stored. Each query is evaluated only on the docs that contain one of its trigger terms ([`cover`](src/main.rs#L350)); the strict left fold is applied to the doc's term set ([`holds`](src/main.rs#L453)), and the smallest ids are shared across workers ([`FirstIds`](src/main.rs#L482)).
- Queries whose cover is estimated to hold at least 1 doc in 64 are folded 64 docs at a time with word operations ([`fold_window`](src/main.rs#L401)); counts come from popcount and id sums from bit-sliced popcounts ([`flush_window`](src/main.rs#L429)).
- Trigger terms and window queries are picked with document frequencies estimated on 1/128 of the corpus ([`estimate_df`](src/main.rs#L271)). The estimate only chooses an evaluation order; answers never depend on it.
- PERF.md: board steps 1-19 cover b-1 to b-11; box-only experiments are listed under the table.
- Honest limit: tokenizing and the term-table probe are now about half of the pass, and both still run one byte or one token at a time.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The program reads `queries.txt` first. It deduplicates whole lines and interns every term straight into the table the pass later probes. That table uses FNV hashes and open addressing, and doubles whenever it would pass a load factor of 2/3. Each 8-byte slot holds the term's hash and id, so a miss reads one cache line. A hash match is confirmed against the term's bytes, which are stored inline and zero-padded in a 32-byte entry: one more line. Each distinct phrase becomes one pattern with one anchor, its term with the lowest estimated document frequency. The binary links the C runtime statically (probe b-21), so no dynamic loader runs before main.

Under the spec's strict left fold, whether a document is in a BOOL query's result depends only on which of the query's terms the document contains, so every query can be decided per document. A document can only be in R if it contains a term of the query's cover. The cover starts as the first term; each OR operand is added to it; an AND operand may replace the cover when its estimated document frequency is lower. Every query is listed, with its code inline, under each term of its cover. Before the pass, document frequencies are estimated on 64 evenly spaced windows covering 1/128 of the corpus, only to make those choices. The sample's 64 windows are spread over the worker threads. The terms are then renumbered by descending estimated frequency, in place in the term table, so the terms most tokens hit have neighbouring ids and their table entries and per-term records share cache lines. The BOOL and phrase plans are built on two threads.

`docs.txt` is mapped read-only and cut into one line-aligned range per worker. Each worker tokenizes its documents byte by byte as the spec defines (ASCII lower-casing, runs of `a-z0-9`), hashing while it scans. A token found in the table sets that term's bit in a small per-document presence set. Every token, query term or not, appends its term id (or a "no term" mark) to the document's token sequence, and an anchor term also records its position. On the term's first appearance in a document, its BOOL roles are read from a small per-term record: its range of trigger entries and its window-term index. A term that triggers no query then adds nothing to the end-of-document evaluation. At the end of the document, each anchor position i is checked against every phrase anchored on that term at offset j: the phrase matches if its term ids equal the sequence starting at i - j. A "no term" mark never equals a term, so no phrase matches across a token that is no query term, and the sequence holds only this document. Each phrase counts a document once. A phrase containing a term that occurs nowhere has an estimated frequency of 0 there, so it is anchored on that term and never checked.

Queries whose cover is estimated to hold at least one document in 64 take a window path instead: each worker keeps, for every term of those queries, one 64-bit word of the current 64-document window. When the window ends, it folds each such query once with AND, OR and AND-NOT on whole words. The match count is the popcount of the result. The id sum is the window base times that count, plus the sum over k of popcount(result AND mask_k) shifted left by k, where mask_k selects the bit positions whose bit k is set. First ids are taken from the word only while the query can still use them. On seed 1, 2,679 queries take this path, and the per-document evaluations drop from 91 M to 10.7 M.

At the end of each document, the worker evaluates every other query listed under the document's terms. Queries of OR operators only are matches as soon as they are triggered. A matching document adds to the worker's count and sum for that query and is offered to a shared set of the ten smallest ids, which a single atomic bound guards. After the pass, the workers' counts and sums are added up. PHRASE answers are the per-document matches the pass collected. `results.txt` is formatted with a small decimal writer and written once at the end, and the process exits right after, without freeing its tables.

## Dependencies

None.

## With another 24 hours

Hash each token a word at a time, order term ids by frequency so the hot entries share cache lines, trim the query-side tables.
