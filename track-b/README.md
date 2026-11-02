# Track B: Index

Reads `queries.txt` (50,000 BOOL and PHRASE queries) and `docs.txt` (254 MB, one document per line) from `/data` and writes `results.txt` to `/out`: for each query, the number of matching documents, the sum of their ids and the ten smallest ids.

## For the judges, in one minute

- **The code:** one file, [bindex.c](bindex.c), C17 with no dependencies, formatted with clang-format's defaults, so the lines you read are the lines S counts. Its top comment gives the design, and the comments on the fast paths say why they are shaped the way they are. Errors on open, map, allocation and write stop the program with the path or call that failed.
- **The result:** 56.35 s → 53.9 ms (about 1,050×), 2.2 GB → 43 MB. This code's board run run-014825fa0945 (commit 5737a0d): T 53.87 ms, M 43.12 MB, S 845, D 1, K 50.63. The board is noisy while its queue is busy: the same code also read T 56.66 ms (run-579e957bcca1). The board's highest K for us, 51.02 (run-9066adc75374), is the previous code, d25bbb9, with the same hot loops at S 805. Our copy of the scoring machine reads both at about 52 ms.
- **Since the previous code:** the same hot loops, with one small speed-up, the comments rewritten for the final design and the setup code unfolded so it reads plainly: S 805 to 845, about 0.7 % of K given back for that on purpose ([PERF.md](PERF.md), b-126 and b-127).
- **Reproduce:** from the repository root, `python3 check.py b --data data/b --expected f86973c210228141bcb6f1f1b10188ad2a9b65f71e7cd7cf4590b38de4e80927`.
- **The honest limit:** a token is matched by a hash, not its text. About 1 dataset in 3,000 would get a wrong answer; none of the 9 we ran does (counted, see [Limits](#limits)). Three system facts carry the speed: huge pages through `madvise`, the data already in the page cache, and 12 hardware threads.
- **S** is the board's count of code lines after clang-format, plus 6 for the Dockerfile; comments and blank lines do not count.
- **Where each criterion's evidence is:**
  - Code quality: bindex.c's header and function comments.
  - Technique: the table below, each trick linked to its line.
  - Optimisation log: [PERF.md](PERF.md), every step, with its board run wherever we hold the ID (a row without one gives its commit), failed experiments included. The last redesign, 103 ms to 52 ms, has its own section near the top, with the measurement errors we made and caught.

| Trick | Where |
|---|---|
| Only the query terms are indexed. All workers parse the queries at once into one shared table; a new term is claimed with a compare-and-swap, no lock | [`intern`](bindex.c#L283), [`parse`](bindex.c#L383) |
| A token's key is three AES rounds over its lowercased bytes, cut to length by a mask table indexed by the length (gcc made the obvious `L < 16 ? L : 16` a branch that mispredicted: 3 ms on the box, 5 to 12 ms in single board runs). The key's low bits are its bucket's byte offset and its high half a 32-bit tag | [`hash_at`](bindex.c#L240) |
| A bucket is one cache line: 7 tags, their values and an eighth value that answers a miss. A lookup compares all the tags at once and reads the value at the match's index, or at 7, so a hit and a miss take the same path. The table sits on one 2 MB page and is addressed directly, without a pointer load | [`lookup`](bindex.c#L269) |
| Tokens cut 64 bytes at a time from AVX2 class masks. Each round always hashes 8 tokens and moves to the first token it did not take, so no loop exit per window can mispredict | [`tokenize`](bindex.c#L325) |
| Whole documents go through in batches, a sentinel key after each document. The sentinel answers as a miss, so no phrase crosses a document's edge and no bound is checked per token. A lookup prefetches the bucket 12 tokens ahead, into the next batch, which is tokenized first | [`batch`](bindex.c#L495), [`scan_part`](bindex.c#L517) |
| One loop only looks up. A second takes the ids 8 at a time: common hits, rare hits and anchors fall out as masks and are left-packed with an AVX2 permute, so no branch depends on a token's class | [`scan_part`](bindex.c#L517) |
| A common term's hit is one OR into that term's word for the current 64 documents, stored to its bitmap row at each row's end. It replaced 64×64 bit transposes | [`scan_part`](bindex.c#L517) |
| A rare term's hit goes to a per-worker stream for its shard of 2,048 terms. Lists are built per shard by claim: counted, placed at a shared cursor, filled in document order, 16-bit ids after a sentinel that drops repeats | [`build`](bindex.c#L613) |
| Term ids renumbered by class: common (over 1/32 of a sample), rare BOOL, phrase-only. Each worker counts one slot range, and the counts' prefix sums give each range its first ids | [`cls`](bindex.c#L445), [`worker`](bindex.c#L897) |
| A phrase is checked during the scan at each occurrence of its rarest term. The anchor's entry holds the neighbouring term's id, so most candidates are rejected without touching the query | [`match_phrases`](bindex.c#L455) |
| One index per phase, answered by claim into a running tally per query: phases run in document order, so there are no per-part records and no merge. Results stay lists while small and become bitmaps once a common term joins by OR | [`eval_range`](bindex.c#L797), [`emit`](bindex.c#L764) |
| AND and NOT compare 8-value blocks with SSE4.2, with no branch on which side advances; a short list against a long one binary-searches it in constant steps. A long OR goes through a bitmap. A query's last OR, once it holds its ten ids, takes count and sum as a + b - (a AND b), with no merge | [`filt`](bindex.c#L696), [`lower`](bindex.c#L643), [`eval_range`](bindex.c#L797) |
| A bitmap's count and id sum from AVX2 nibble tables, 256 bits per step | [`bsum`](bindex.c#L658) |
| Parts of the text are claimed, four per worker per phase, so a worker slowed by a neighbour no longer holds up a phase | [`worker`](bindex.c#L897) |
| Rows, tallies and lists share one huge-page region, each page faulted once by one thread. It removed a page-table lock fight: 4 KB first touches by 12 threads took 20 % of all kernel cycles | [`arena`](bindex.c#L176), [`worker`](bindex.c#L897) |
| Each worker drops its share of the text's page-table entries with `MADV_DONTNEED` during the run, in parallel, so exit does not tear them all down on one thread | [`worker`](bindex.c#L897) |

## The approach

Twelve workers, one per hardware thread of the scoring box (the CPU count; `SD_WORKERS` overrides it for our hash tests at 1, 4 and 24 workers). `docs.txt` is mapped read-only, never copied. Times below are from a box timeline of an earlier build (b-122 and b-123), about 54 ms; the final build is about 2 ms faster.

1. **Queries (setup, 4.3 ms with the steps below).** Each worker parses one slice of `queries.txt` into the shared term table. Every BOOL term is flagged; each phrase is a list of terms.
2. **Sample and renumber.** Each worker counts tokens per term over a small part of the text. From the summed counts, terms whose sampled token count is over 1/32 of the sampled documents are common (about 360), and every term gets a new id: common, then rare BOOL, then phrase-only. Each phrase is listed under its term with the lowest sampled count.
3. **Four phases of about 62,500 documents**, cut by bytes, with a check that stops the program if one reaches 65,535, so a document's number fits 16 bits, each with three steps at a barrier:
   - **Scan (8.7 ms, then 8.05 ms).** Workers claim parts of the phase's text and run the two loops above. Phrases are checked here.
   - **Build (1.1 ms).** Workers claim term shards and turn the streams into sorted lists.
   - **Answer (2.8 ms, then 2.3 ms).** Workers claim blocks of 64 queries and fold each BOOL query left to right over its lists and rows, adding the result to its tally.
4. **Format (0.55 ms).** Each worker formats the lines of the queries it parsed. The main thread writes the slices in order (0.65 ms).
5. **Exit (1.2 ms).** About 1.2 ms goes to freeing the remaining 4 KB pages at exit, and 0.5 ms to process start. Both are inside T.

No locks: the steps meet at barriers, work goes out by atomic counters, and what two workers can write at once is atomic: a new term by compare-and-swap, flags and counters by relaxed stores and fetch-adds.

## Why it is shaped this way

- **One index per phase, one tally per query.** The earlier design answered every query per part and merged the records at the end. Sharing one index across all workers per phase took the box from 103 to 84 ms at a lower peak (39 MB), and dropped the merge.
- **Memory is spent up to the cap, not below it.** K's memory factor stops paying at 50× the reference, about 44 MB on this track, so every M under that scores the same, and we buy time with what is left (M 43 MB).
- **Branches over instructions.** The redesigns gave the biggest wins. Among the smaller ones mispredicts stood out, because the board pays more for them than our box does (the same branch: 5 to 12 ms in single board runs, 3 ms here). So the token path has no branch on a token's class, a hit or miss, or a loop exit per window.
- **Exact bytes are never compared.** Comparing text would mean a second load per token. A 32-bit tag in a 15-bit bucket is enough at this scale; the odds are under "Limits".
- **Rows only for common terms.** A row costs one bit per document whether the term occurs there or not; for the rare terms, about 53,000 of them, a sorted list is smaller and folds faster.

## Limits

- **A token is matched by a hash, not its text.** A word that is not a query term is mistaken for one only if it lands in a query term's bucket with the same 32-bit tag. Two query terms can collide the same way, and a query term whose tag is 0 looks like an empty slot. Seed 1 has 609,889 distinct tokens and 75,747 terms: 1,237,173 token-term pairs share a bucket and about 87,600 term pairs do. Divided by 2^32, plus 75,747 / 2^32 for the zero tag, that is 0.033 % per dataset, about 1 in 3,000, an upper bound since a collision has to change an answer to matter. These odds hold only because the hash now behaves like a uniform one, and it did not always. With two AES rounds, an unseen seed gave `brestjex` and `brostjedo` the same 64 bits and one query in 50,000 came out wrong (b-129). [reports/b/collide.c](../reports/b/collide.c) recounts the pairs with the same key as bindex.c: at 16 tag bits a uniform hash predicts 18.9 pairs and three datasets read 14 to 18; at 32 bits none. The same count with AES removed reads 15 to 4,000,000 times the prediction, so it can tell a bad hash. All 9 datasets we ran match the reference. A fix that would remove these odds, comparing the text, costs a second load per token, about what 0.033 % of 85 points could never pay back, so we kept the hash.
- **The hash reads at most 32 bytes of a token.** Two tokens that agree on their first 32 bytes share a key. The longest token on both seeds is 17 bytes; the spec sets no bound.
- **Three system-level facts the speed leans on:** transparent huge pages available through `madvise`, `/data` already in the page cache, and 12 hardware threads. Without them the program still gives the right answer, only slower (hash tests at 1, 4 and 24 workers).

## Dependencies

None. The C standard library and POSIX threads, linked statically. Built by the [Dockerfile](Dockerfile) with gcc 15, `-O3 -march=x86-64-v3 -maes`.

## With another 24 hours

Share of cycles on the box (perf, 100 runs per reading): hash and tokenizer 23 %, lookup 17 %, applying rare and common hits 9 %, build 8 % (with 31 % of L1 misses).

- **The build's count pass.** Term counts barely change from phase to phase, so the previous phase's counts could place the lists with no count pass. At most 0.5 ms.
- **Running counts in OR through bitmaps.** Keeping count and sum while setting bits would avoid `bsum` on list-only bitmaps: at most 0.5 ms for about 15 lines.
- **gcc 16.** It read 10 ms slower on this code, and we did not find out why. One misjudged inline or vectorization is the first place to look, and it might show a gain on gcc 15 too.
- **Exit.** The last 1.2 ms is freeing 4 KB pages. Parallel `madvise` before exit gained nothing (b-122 in PERF.md), so it needs fewer 4 KB pages, not a different exit.

## Reproduce

From the repository root: `python3 check.py b --data data/b --expected f86973c210228141bcb6f1f1b10188ad2a9b65f71e7cd7cf4590b38de4e80927`

Or by hand, with seed 1's full data in `<data>`: `docker build -t b track-b && docker run --rm --network=none -v <data>:/data:ro -v <out>:/out b && sha256sum <out>/results.txt`
