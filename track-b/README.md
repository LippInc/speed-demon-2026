# Track B: Index

Reads `queries.txt` (BOOL and PHRASE queries) and `docs.txt` (one document per line) from `/data` and writes `results.txt` to `/out`: for each query, the number of matching documents, the sum of their ids and the ten smallest ids.

56.35 s → 0.27 s (about 210×), 2.2 GB → 33 MB. No index is built: one parallel pass decides every BOOL query 64 documents at a time from bit-words, and every phrase by checks at its rarest term.

Rust, standard library only. K 23.67 (box `sd-bench/B`, commit f592839, b-59): T 0.267 s, M 32.9 MB, S 645, D 1. The board has not run this code yet. The board's last run, of b-38b's code: run-75493eef7d4f, K 23.04, T 0.284 s.

Terms: K is the organisers' score combining T (run time), M (peak memory), S (lines of code) and D (dependencies) against the reference, higher is better; the box is our bench machine with the scoring machine's CPU; b-NN is one experiment in PERF.md; KNOWN = a textbook technique, OURS = our own design.

Reproduce with seed 1's full data in `<data>`, then compare the hash with the full seed-1 output hash in the [spec](https://github.com/hackathon-raptors/speed-demon-2026/blob/main/specs/b-index.md):
`docker build -t b track-b && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out b && sha256sum <out>/results.txt`

| Trick | Where | PERF |
|---|---|---|
| One pass over the mapped corpus, one document range per thread | [`map_file`](src/main.rs#L80), [`scan`](src/main.rs#L596) | b-1 |
| Only the query terms are looked up, in one compact table | [`TermTable`](src/main.rs#L114) | b-6, b-13, b-36a |
| Phrases checked at the rarest term's byte offsets at the doc's end | [`PhrasePlan`](src/main.rs#L281) | b-8, b-56 |
| BOOL queries folded per 64-document window, no posting store | [`fold_window`](src/main.rs#L481) | b-3, b-4, b-28, b-32b, b-33a |
| A query is folded only in windows where a term of its cover occurs | [`cover`](src/main.rs#L433) | b-3 |
| Counts by popcount, id sums by bit-sliced popcounts, a repeat's fold masked | [`Window`](src/main.rs#L509) | b-4, b-35a |
| Tokens cut 32 bytes at a time, starts and ends as two masks | [`tokenize`](src/main.rs#L199), [`walk`](src/main.rs#L221) | b-31a, b-31b, b-31c, b-55, b-59 |
| Trigger order from document frequencies sampled on 1/128 of the corpus | [`estimate_df`](src/main.rs#L364) | b-9, b-11 |

Every measurement, failed experiments included, with its board run ID: [PERF.md](PERF.md). Receipts: `reports/b/` at the repository root, one `harness-<commit>.txt` per measured submission commit, from the board's runs of that commit; each names the commit it measured, and the commit after a measured push only adds its receipt.

Limit: the BOOL fold at each window's end takes 80 ms of a worker's 231 ms on the box (b-44), and the term-table probe makes 48.4 M L1 misses at one worker (b-48); six measured attempts at them (b-45 to b-48, b-53, b-57) found nothing worth building.

## Approach

The program reads `queries.txt` first. Every line is its own query (a line dedupe was 70 M of the parse's 110 M instructions; removing it took the VM's parse from 32 ms to 22.7 ms), and every term is interned straight into the table the pass later probes. That table uses a fixed-width multiplicative hash of the zero-padded term and open addressing, and doubles whenever it would pass a load factor of 1/3. Each 4-byte slot holds the term's id in its low 30 bits and the hash's top 2 bits, so a miss reads one cache line. A hash match is confirmed against the term's bytes, which are stored inline and zero-padded in a 32-byte entry: one more line, and one 32-byte compare with the zero-padded token. Each PHRASE line becomes one pattern with one anchor, its term with the lowest estimated document frequency. The binary links the C runtime statically (probe b-21), so no dynamic loader runs before main.

Under the spec's strict left fold, whether a document is in a BOOL query's result depends only on which of the query's terms the document contains, so the same fold over 64-bit words, one bit per document, decides 64 documents at once. A document can only be in R if it contains a term of the query's cover. The cover starts as the first term; each OR operand is added to it; an AND operand may replace the cover when its estimated document frequency is lower. Every query is listed, with its code inline, under each term of its cover. Its accumulator slot follows that list order, so the queries one term triggers have neighbouring accumulators. Before the pass, document frequencies are estimated on 64 evenly spaced windows covering 1/128 of the corpus, only to make those choices. The sample's 64 windows are spread over the worker threads. The terms are then renumbered in place in the term table: the BOOL terms first, so a window's words cover only them, and each group by descending estimated frequency, so the terms most tokens hit have neighbouring ids and their table entries and words share cache lines. The BOOL and phrase plans are built on two threads.

`docs.txt` is mapped read-only and cut into one line-aligned range per worker. Each worker tokenizes its documents as the spec defines (ASCII lower-casing, runs of `a-z0-9`), classifying 32 bytes at a time with AVX2 and cutting tokens where the token-byte mask changes, the starts and ends decoded as two masks (b-55), so no branch is taken per byte. Each token's bytes are then loaded once, lower-cased by setting bit 5, zero-padded to 32 bytes, and hashed as four 64-bit words at once, so no loop depends on the token's length. A token found in the table sets the document's bit in that term's word for the current 64-document window; the first bit set in a word puts the term on the window's touched list (every hit writes the entry, and only the first one in the window keeps it, with no branch), with its range of trigger entries read right there. The pass records only a document's anchor occurrences, each with its byte offset. At the end of the document, each anchor occurrence is a candidate for every phrase anchored on that term at offset j, confirmed by reading the doc's bytes around it: the non-empty pieces of the text from the anchor on, split at separators, are the doc's own tokens from the anchor on, and the pieces before it, split from the right, are the tokens before it, so each of the pattern's other terms is checked against the token at its offset from the anchor; a term equals a token when its bytes equal the token's up to ASCII case and end there (the term table's own test). A token is a maximal run of `a-z0-9` and every other byte separates, so this reads the same tokens the old per-token sequence held and decides exactly what it decided, without the 36.3 M pushes per run (b-56). Each phrase counts a document once. A phrase containing a term that occurs nowhere has an estimated frequency of 0 there, so it is anchored on that term and never checked.

When the window ends, the worker walks the touched terms and folds each query listed under them once (a query reached again in the same window, through a second term, has its result masked to zero, with no branch), with AND, OR and AND-NOT on whole words; the operator selects masks from a small table, so the fold takes no branch on it. The fold always takes its first two steps (shorter codes are padded with a step that keeps the result) and loops only over the 4th to 6th terms, so it does not guess a query's length per fold. The match count is the popcount of the result. The id sum is the window base times that count, plus the sum over k of popcount(result AND mask_k) shifted left by k, where mask_k selects the bit positions whose bit k is set. First ids are taken from the word only while the query can still use them, and offered to a shared set of the ten smallest ids, which a single atomic bound guards. Then the touched words are cleared. After the pass, the workers' counts and sums are added up. PHRASE answers are the per-document matches the pass collected. `results.txt` is formatted with a small decimal writer, each worker formatting one contiguous slice of the queries, and written once at the end, and the process exits right after, without freeing its tables.

## Dependencies

None.

## With another 24 hours

- The flush's remaining branches (29.1 M mispredicts at 1 worker on seed 1 after b-35a): the `r == 0` skip and the per-term trigger-loop exits. Cachegrind counts them as missed, but the box did not reward removing them (b-35b, b-35c), so only a form that also cuts instructions or lines is worth trying.
- An 8-byte accumulator that keeps the duplicate-fold stamp inside it (narrow fields that spill exactly on wrap), so the halved footprint does not cost an extra line per fold (b-33b moved the stamp out and lost).
- The pass's table access: a per-document batch with the slot line prefetched took the lookup floor from about 88 to 78-80 ms per worker (b-58), but wired into the pass with more prefetches it read 2 ms slower (b-60); a one-token-ahead slot prefetch inside `walk`, with no buffer, is the form left to try.
