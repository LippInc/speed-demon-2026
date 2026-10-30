# Track B: Index

Track B (index), Rust, standard library only. K 21.75 = T 0.318 s, M 33.1 MB, S 631, D 1 (box sd-bench/B on commit ef63a96b).
Reproduce: `docker build -t b track-b && docker run --rm -v <data>:/data:ro -v <out>:/out b`.

- One pass over the memory-mapped corpus ([`map_file`](src/main.rs#L79)), one doc range per thread ([`scan`](src/main.rs#L568)), looking up only the query terms in a compact table ([`TermTable`](src/main.rs#L112)).
- Phrases are matched by anchor checks in the same pass: each phrase is checked only where its rarest term occurs, by comparing term ids ([`PhrasePlan`](src/main.rs#L267)).
- BOOL queries are decided inside the pass, 64 documents at a time, with no posting list stored. A query is folded with word operations ([`fold_window`](src/main.rs#L453)) only in the windows where a term of its cover occurs ([`cover`](src/main.rs#L409)); counts come from popcount and id sums from bit-sliced popcounts ([`Window`](src/main.rs#L482)), and the smallest ids are shared across workers ([`FirstIds`](src/main.rs#L538)).
- Trigger terms are picked with document frequencies estimated on 1/128 of the corpus ([`estimate_df`](src/main.rs#L340)). The estimate only chooses an evaluation order; answers never depend on it.
- PERF.md: board steps 1-26 cover b-1 to b-21; bench-box experiments and VM-only experiments are listed under the table.
- Honest limit: tokenizing and the term-table probe are about half of the pass; the probe still runs one token at a time.

The task, the input files and the exact output format are in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md).

## Approach

The program reads `queries.txt` first. It deduplicates whole lines and interns every term straight into the table the pass later probes. That table uses a fixed-width multiplicative hash of the zero-padded term and open addressing, and doubles whenever it would pass a load factor of 2/3. Each 8-byte slot holds the term's hash and id, so a miss reads one cache line. A hash match is confirmed against the term's bytes, which are stored inline and zero-padded in a 32-byte entry: one more line, and one 32-byte compare with the zero-padded token. Each distinct phrase becomes one pattern with one anchor, its term with the lowest estimated document frequency. The binary links the C runtime statically (probe b-21), so no dynamic loader runs before main.

Under the spec's strict left fold, whether a document is in a BOOL query's result depends only on which of the query's terms the document contains, so the same fold over 64-bit words, one bit per document, decides 64 documents at once. A document can only be in R if it contains a term of the query's cover. The cover starts as the first term; each OR operand is added to it; an AND operand may replace the cover when its estimated document frequency is lower. Every query is listed, with its code inline, under each term of its cover. Its accumulator slot follows that list order, so the queries one term triggers have neighbouring accumulators. Before the pass, document frequencies are estimated on 64 evenly spaced windows covering 1/128 of the corpus, only to make those choices. The sample's 64 windows are spread over the worker threads. The terms are then renumbered in place in the term table: the BOOL terms first, so a window's words cover only them, and each group by descending estimated frequency, so the terms most tokens hit have neighbouring ids and their table entries and words share cache lines. The BOOL and phrase plans are built on two threads.

`docs.txt` is mapped read-only and cut into one line-aligned range per worker. Each worker tokenizes its documents as the spec defines (ASCII lower-casing, runs of `a-z0-9`), classifying 32 bytes at a time with AVX2 and cutting tokens where the token-byte mask changes, so no branch is taken per byte. Each token's bytes are then loaded once, lower-cased by setting bit 5, zero-padded to 32 bytes, and hashed as four 64-bit words at once, so no loop depends on the token's length. A token found in the table sets the document's bit in that term's word for the current 64-document window; the first bit set in a word puts the term on the window's touched list, with its range of trigger entries read right there. Every token, query term or not, appends its term id (or a "no term" mark) to the document's token sequence, and an anchor term also records its position. At the end of the document, each anchor position i is checked against every phrase anchored on that term at offset j: the phrase matches if its term ids equal the sequence starting at i - j. A "no term" mark never equals a term, so no phrase matches across a token that is no query term, and the sequence holds only this document. Each phrase counts a document once. A phrase containing a term that occurs nowhere has an estimated frequency of 0 there, so it is anchored on that term and never checked.

When the window ends, the worker walks the touched terms and folds each query listed under them once, with AND, OR and AND-NOT on whole words. The match count is the popcount of the result. The id sum is the window base times that count, plus the sum over k of popcount(result AND mask_k) shifted left by k, where mask_k selects the bit positions whose bit k is set. First ids are taken from the word only while the query can still use them, and offered to a shared set of the ten smallest ids, which a single atomic bound guards. Then the touched words are cleared. After the pass, the workers' counts and sums are added up. PHRASE answers are the per-document matches the pass collected. `results.txt` is formatted with a small decimal writer and written once at the end, and the process exits right after, without freeing its tables.

## Dependencies

None.

## With another 24 hours

Hash each token a word at a time, order term ids by frequency so the hot entries share cache lines, trim the query-side tables.
