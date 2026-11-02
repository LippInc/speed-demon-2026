# Track B: Index

Reads `queries.txt` (50,000 BOOL and PHRASE queries) and `docs.txt` (254 MB, one document per line) from `/data` and writes `results.txt` to `/out`: for each query, the number of matching documents, the sum of their ids and the ten smallest ids.

## For the judges, in one minute

- **The code:** one file, [bindex.c](bindex.c), C17 with no dependencies. Its top comment gives the design in one paragraph, and each function's comment says why it is shaped the way it is.
- **The result:** 56.35 s → 103.2 ms (about 540×), 2.2 GB → 44 MB. The board's best run, run-24585bac1860, measured this tip (b-109): T 103.2 ms, M 44.0 MB, S 852, D 1, K 36.54. Our copy of the scoring box reads it at 103.2 ms. The board measured the first C version at T 153.9 ms, K 30.12 (run-d36b02f5b879); the Rust version it replaced read T 0.218 s, K 25.19 (run-21676903d119).
- **S** (852 at this tip, 853 in the runs above: one comment moved off a call that clang-format had split) is the board's count of code lines after clang-format with its default settings, which splits many of the file's long lines; comments and blank lines do not count.
- **Where each criterion's evidence is:**
  - Code quality: bindex.c's header and function comments. Errors on open, map and write stop the program with the path or call that failed.
  - Technique: the table below, each trick linked to its line.
  - Optimisation log: [PERF.md](PERF.md), every step with its board run ID, failed experiments included; the C rewrite's own steps are at the end.

| Trick | Where |
|---|---|
| Only the query terms are indexed. All workers parse the queries at once into one shared table; a new term is claimed with a compare-and-swap, no lock | [`intern`](bindex.c#L159), [`parse`](bindex.c#L217) |
| A token of up to 8 bytes is its own key (its lowercased bytes as one 64-bit word), built with two masks picked by its length, so a match needs no byte compare. A bucket holds 5 keys and their values in one cache line, plus a sixth value that answers a miss. All five keys are compared at once and the value is read at the match's index, or at 5 on a miss, so a hit and a miss take the same path; the only branch tests for a bucket a key was probed past, which almost never happens (an earlier hit-or-miss branch missed on about one token in four). The table sits on one 2 MB page | [`hash_at`](bindex.c#L129), [`lookup`](bindex.c#L148) |
| Tokens cut 64 bytes at a time from AVX2 class masks. Each lookup prefetches the bucket of the token 8 places ahead, reaching into the next document's keys at a document's end (a whole document ahead put about 200 lines in flight, and they pushed each other out of L1) | [`tokenize`](bindex.c#L195), [`scan_part`](bindex.c#L310) |
| Lookup and record in one loop per document. A common term (in over 1/32 of the sampled documents) stores a flag byte, a plain store with no read-modify-write chain; at the document's end two movemasks turn the flags into bits, and every 64 documents 64×64 bit transposes turn those into a word of each common term's bitmap row. The rest get sorted 16-bit document lists | [`transpose64`](bindex.c#L290), [`scan_part`](bindex.c#L310) |
| Term ids renumbered by sampled frequency: common terms first, then rare BOOL terms, then phrase-only terms. Every worker counts one slot range by class, and the counts' prefix sums give each range its first ids, so the renumbering runs on all threads and gives the serial order's ids | [`cls`](bindex.c#L267), [`worker`](bindex.c#L559) |
| Phrases checked during the scan at each occurrence of their rarest term, against the document's own token ids | [`match_phrases`](bindex.c#L270) |
| BOOL queries answered per part, strictly left to right, on lists while the result is small and on bitmaps once a common term joins by OR; AND and NOT compare 8-value blocks all to all with SSE4.2, and a short list against a long one binary-searches the long one | [`eval_range`](bindex.c#L464), [`filt`](bindex.c#L411) |
| A bitmap result's count and id sum from nibble tables in AVX2 (per byte a popcount and a sum of bit positions, 256 bits per step); only as many ids as the query still lacks | [`bsum`](bindex.c#L382), [`emit`](bindex.c#L443) |
| Each worker merges and formats the queries it parsed; one write per worker's slice | [`finish`](bindex.c#L528) |

## The approach

Twelve workers, one per hardware thread of the scoring box (the CPU count; `SD_WORKERS` overrides it for our hash tests at 1, 4, 12 and 24 workers). Each owns a contiguous stretch of `docs.txt` (mapped read-only, never copied), cut into 3 parts of at most 32,768 documents, so a document's number inside its part fits in 16 bits.

1. **Queries.** Each worker parses one slice of `queries.txt` into the shared term table. Every BOOL term is flagged; each phrase is a list of term ids.
2. **Sample and renumber.** Each worker counts document frequencies on the first 1/64 of its first part. From the summed counts, terms in over 1/32 of the sampled documents are common (about 370 on seed 1), and every term gets a new id: common, then rare BOOL, then phrase-only. Each phrase is anchored at its term with the lowest sampled frequency.
3. **Scan a part.** Per document: tokenize the next document, then one branch-free loop over this one's tokens: prefetch the bucket 8 tokens ahead, look the token up; a common term stores its flag byte, a rare BOOL term appends its id to a stream, an anchor is noted. The flags become the document's bits, and every 64 documents those become one word of each common term's row by transposes. After the part, the stream is counted and scattered into per-term document lists, repeats dropped. Phrases are checked at the end of each document, at each anchor, against its token ids.
4. **Answer the part.** Every BOOL query is folded left to right over its terms' lists and rows. A query that matched leaves a record: count, sum of part-local numbers, and as many of the smallest numbers as it still needs.
5. **Merge.** Each worker adds up the records of the queries it parsed, part by part in document order, adds the phrase hits and formats its lines. The main thread writes the slices in order.
6. **Leave cheaply.** Each 2 MB page of the sample counts is faulted by one worker only (when several threads fault the same huge page, each zeroes a copy before one wins). Each worker drops its part's text mapping right after the scan, and its arrays after the merge, with `MADV_DONTNEED`, which runs under the address-space read lock, so in parallel; the exit then has no 254 MB of page-table entries to tear down on one thread.

No locks: the phases meet at barriers, and in the scan and answer phases each worker writes only its own memory, 64-byte aligned so no two workers share a cache line.

## Why it is shaped this way

- **Answers per part, no global index.** Holding every posting list at once does not fit in the memory the score rewards (it pays down to 44 MB, the 43,999,232 B cap; a posting store could not get under it, b-2 in PERF.md). Parts of 32,768 documents keep every list 16-bit and in cache, and a part's memory is freed before the merge.
- **Exact keys for short tokens.** Most words are 8 letters or shorter, so the key is the token itself; longer tokens fold their further 8-byte words in with odd multipliers.
- **Rows only for common terms.** A row costs one bit per document of the part whether the term occurs there or not; for the rare terms, about 53,000 of them, a sorted list is smaller and folds faster.
- **Phrases at the rarest term, on token ids.** Checking at the anchor turns 36 M token positions into about a million candidate checks (1.1 M anchor checks against 36.3 M tokens, b-73 in PERF.md), and the ids are already in hand from the lookup.

## Dependencies

None. The C standard library and POSIX threads, linked statically. Built by the [Dockerfile](Dockerfile) with gcc 15, `-O3 -march=x86-64-v3`.

## With another 24 hours

Instructions per token (box, `perf`, after b-105; each stage's instructions divided by the corpus's 36.3 M tokens): tokenize about 36, lookup and record about 33 (62 before b-105), count and scatter about 10, answering about 46. The box is instruction-bound (3.4 instructions per cycle per core), so instructions are what to cut.

- **The answer phase.** AND and NOT now compare 8-value blocks with SSE4.2 (b-103). OR as a min/max merge network gained 1 ms for 34 more lines, under the 0.05 ms a line costs in the score, so it is not in. Each part still answers every BOOL query; answering a worker's three parts at once would cut that fixed cost, but needs the memory the parts save.
- **The tokenizer.** About 36 instructions per token. Writing the token starts and ends with unconditional stores (no loop count that depends on the text) cut branch misses by 3.4 M but added 8 % instructions and lost 1 ms; hashing two tokens per vector register would cut the per-token work instead.
- **The rare-term lists.** Counting and scattering the scan's stream into per-term lists is about a fifth of the scan: running that stage twice adds 10.9 ms to the 103.2 ms run (box, PERF.md b-109). The cost is random writes into 53,000 counters. Splitting the stream by term range first would keep each range's counters in L1.

## Reproduce

From the repository root: `python3 check.py b --data data/b --expected f86973c210228141bcb6f1f1b10188ad2a9b65f71e7cd7cf4590b38de4e80927`

Or by hand, with seed 1's full data in `<data>`: `docker build -t b track-b && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out b && sha256sum <out>/results.txt`
