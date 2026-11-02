# Track C: Join

records.csv (202 MB, 3.32 M records with quoted and escaped name and note fields, embedded newlines and mid-file BOMs) joined to regions.csv and aggregated per country: count, id sum, amount sum, min and max, distinct names, name and note bytes.

## For the judges, in one minute

- **The scores' terms:** T is the run time, M the peak memory, S the code lines (clang-format defaults), D is 1 plus the number of declared dependencies (ours is 1: none); K, the speed score, grows as each falls against the reference.
- **The code:** one file, [join.c](join.c), 417 code lines of C17. Its top comment walks through the design in five steps, and every function's comment says why it is shaped the way it is.
- **The result:** board run-be7fc43b744a measured T 21.31 ms, M 10.5 MB, S 417, K 43.27 (the reference takes 14.44 s); the same code's other board run, run-14a8fd87f9be, measured 21.74 ms. On our test machine with the organisers' harness it measures T 20.78 ms.
- **Where each criterion's evidence is:**
  - Code quality: join.c's header (the design) and the comment on each function (the reason for its shape). Errors on open, map, thread start and output close stop the program with the system's message.
  - Technique: [PERF.md, "The design and why"](PERF.md#the-design-and-why): each choice with the measurement that decided it.
  - Optimisation log: [PERF.md, "Steps"](PERF.md#steps): every step with its test-machine time, the main ones also with the live-board run that measured them, and "What did not work", including two redesigns tried on the last day that lost.

## The approach

Twelve threads, one per hardware thread of the scoring box's six P-cores, take 256 KB pieces of the mapped file from a shared counter. A thread finds its piece's first record without reading anything before it, because spec 2.2 makes a record's own LF the only LF followed by a digit, a BOM or the end of the file.

Pass 1 lists record ends with three AVX2 compares per byte and no quote state. Pass 2 reads each record's fields from both ends: the id up to the first comma, the region and amount after the last two commas, and the quoted name, whose end a carry-less multiply finds as the first byte after an odd number of quotes. Region lookups are prefetched in a first loop so their cache misses overlap.

Distinct names per country are the hard part: 2.98 M of the 3.32 M (name, country) pairs are distinct. Each name has an owner thread, picked by a hash of its bytes. Readers send owners 8-byte messages through lock-free rings, 128 at a time. Each owner keeps a private table of its names with one bit per country, so no atomic instruction touches the hot path. One table shared by all threads with atomic bit sets took almost twice as long (measured in the Rust version).

## Dependencies

None. The C standard library and POSIX threads, linked statically. Built by the [Dockerfile](Dockerfile) with gcc 15, `-O3 -march=x86-64-v3 -mpclmul -maes`.

## With another 24 hours

- **One pass instead of two (v51, measured on the last day).** Reading each record from its start, finding the note's end with the same quote-parity multiply as the name, removes pass 1. It ran 14 % fewer instructions (905 M against 1,048 M) but 9 % more cycles, because each record's start now waits for the previous record's end and instruction fetch stalled more (top-down: fetch latency 429 M against 251 M slots). Four parsing streams per thread did not recover it. Finding that stall is the most promising lead we have.
- **The region table.** Its lookups cost about 1.6 ms. A 256 KB compressed table recovered only 0.55 ms; a layout that keeps the hot regions together might do better.
- **The board gap.** The board measures us 0.8 to 1.7 ms slower than our copy of the scoring box. The earlier, larger gap matched another process sharing the L3 cache (PERF.md, "Other processes on the scoring box"). The final runs have the machine to themselves.

## Reproduce

From the repository root: `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`
