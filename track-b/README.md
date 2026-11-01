# Track B: Index

Reads `queries.txt` (50,000 BOOL and PHRASE queries) and `docs.txt` (254 MB, one document per line) from `/data` and writes `results.txt` to `/out`: for each query, the number of matching documents, the sum of their ids and the ten smallest ids.

## For the judges, in one minute

- **The code:** one file, [bindex.c](bindex.c), C17 with no dependencies. Its top comment gives the design in one paragraph, and each function's comment says why it is shaped the way it is.
- **The result:** 56.35 s → 0.146 s (about 385×), 2.2 GB → 45 MB. On our copy of the scoring box with the organisers' harness (5 runs, seed 1 full): T 145.8-146.8 ms, M 45.3 MB, S 788, D 1, K about 31. The Rust version it replaced read T 0.218 s, K 25.19 on the board (run-21676903d119).
- **Where each criterion's evidence is:**
  - Code quality: bindex.c's header and function comments. Errors on open, map and write stop the program with the path or call that failed.
  - Technique: the table below, each trick linked to its line.
  - Optimisation log: [PERF.md](PERF.md), every step with its board run ID, failed experiments included; the C rewrite's own steps are at the end.

| Trick | Where |
|---|---|
| Only the query terms are indexed. All workers parse the queries at once into one shared table; a new term is claimed with a compare-and-swap, no lock | [`intern`](bindex.c#L130), [`parse`](bindex.c#L184) |
| A token of up to 8 bytes is its own key (its lowercased bytes as one 64-bit word), so a match needs no byte compare; buckets of 4 keys in one cache line, compared by one AVX2 instruction | [`key_at`](bindex.c#L110), [`lookup`](bindex.c#L121) |
| Tokens cut 64 bytes at a time from AVX2 class masks; each token's bucket is prefetched a whole document before its lookup | [`tokenize`](bindex.c#L165), [`scan_part`](bindex.c#L294) |
| Common terms (in over 1/16 of the sampled documents) get a bitmap row, built 64 documents at a time by 64×64 bit transposes; the rest get sorted 16-bit document lists | [`transpose64`](bindex.c#L274), [`scan_part`](bindex.c#L294) |
| Term ids renumbered by sampled frequency: common terms first, then rare BOOL terms, then phrase-only terms | [`classify`](bindex.c#L234) |
| Phrases checked during the scan at each occurrence of their rarest term, against the document's own token ids | [`match_phrases`](bindex.c#L254) |
| BOOL queries answered per part, strictly left to right, on lists while the result is small and on bitmaps once a common term joins by OR; a short list against a long one binary-searches the long one | [`eval_part`](bindex.c#L409), [`isect`](bindex.c#L354) |
| Counts by popcount, id sums by bit-sliced popcounts, and only as many ids as the query still lacks | [`emit`](bindex.c#L381) |
| Each worker merges and formats the queries it parsed; one write per worker's slice | [`finish`](bindex.c#L473) |

## The approach

Twelve workers, one per hardware thread of the scoring box. Each owns a contiguous stretch of `docs.txt` (mapped read-only, never copied), cut into 3 parts of at most 32,768 documents, so a document's number inside its part fits in 16 bits.

1. **Queries.** Each worker parses one slice of `queries.txt` into the shared term table. Every BOOL term is flagged; each phrase is a list of term ids.
2. **Sample and renumber.** Each worker counts document frequencies on the first 1/64 of its first part. From the summed counts, terms in over 1/16 of the sampled documents are common (about 180 on seed 1), and every term gets a new id: common, then rare BOOL, then phrase-only. Each phrase is anchored at its term with the lowest sampled frequency.
3. **Scan a part.** Per document: tokenize, look every token up (independent loads, many in flight), then one branch-free pass. A common term sets its bit in the document's small bitset, a rare BOOL term appends its id to a stream, an anchor is noted. Every 64 documents the bitsets become one word of each common term's row by transposes. After the part, the stream is counted and scattered into per-term document lists, repeats dropped. Phrases are checked at the end of each document, at each anchor, against its token ids.
4. **Answer the part.** Every BOOL query is folded left to right over its terms' lists and rows. A query that matched leaves a record: count, sum of part-local numbers, and as many of the smallest numbers as it still needs.
5. **Merge.** Each worker adds up the records of the queries it parsed, part by part in document order, adds the phrase hits and formats its lines. The main thread writes the slices in order.

No locks: the phases meet at barriers, and in the scan and answer phases each worker writes only its own memory, 64-byte aligned so no two workers share a cache line.

## Why it is shaped this way

- **Answers per part, no global index.** Holding every posting list at once does not fit in the memory the score rewards (it pays down to 44 MB; a posting store could not get under it, b-2 in PERF.md). Parts of 32,768 documents keep every list 16-bit and in cache, and a part's memory is freed before the merge.
- **Exact keys for short tokens.** Most words are 8 letters or shorter, so the key is the token itself; longer tokens fold their further 8-byte words in with odd multipliers.
- **Rows only for common terms.** A row costs one bit per document of the part whether the term occurs there or not; for the rare terms, about 53,000 of them, a sorted list is smaller and folds faster.
- **Phrases at the rarest term, on token ids.** Checking at the anchor turns 36 M token positions into about a million candidate checks, and the ids are already in hand from the lookup.

## Dependencies

None. The C standard library and POSIX threads, linked statically. Built by the [Dockerfile](Dockerfile) with gcc 15, `-O3 -march=x86-64-v3`.

## With another 24 hours

Per token on one thread (box, 2.5 GHz): tokenize about 14 cycles, lookup 16, the branch-free pass 12, count and scatter 5, answering 18.

- **The answer phase.** The list merges are its biggest share (`unite` 9 %, `isect` 8.5 % of cycles). Most folds are over short lists; merging them with SIMD compares instead of one element per step is next.
- **The lookup.** One bucket line per token, prefetched a document ahead. A smaller table for the hot short tokens would keep it in L1.
- **The serial setup.** Renumbering runs on one thread (about 4 ms) while the others wait; it can be split by slot range like the sample sum.

## Reproduce

From the repository root: `python3 check.py b --data data/b --expected f86973c210228141bcb6f1f1b10188ad2a9b65f71e7cd7cf4590b38de4e80927`

Or by hand, with seed 1's full data in `<data>`: `docker build -t b track-b && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out b && sha256sum <out>/results.txt`
