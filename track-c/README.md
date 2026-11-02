# Track C: Join

records.csv (202 MB, 3.32 M records with quoted and escaped name and note fields, embedded newlines and mid-file BOMs) joined to regions.csv and aggregated per country: count, id sum, amount sum, min and max, distinct names, name and note bytes.

## For the judges, in one minute

- **The result:** 14.44 s (the reference) to 14.475 ms, about 1,000 times faster. Board run run-b73db7cdb927 (commit 5737a0d): T 14.475 ms, M 8.15 MB, S 396, K 55.68. This run is the submission named in [PERF.md](PERF.md). The same hot loops also read T 14.32 ms, K 56.60 (v65, run-6d3cdfadd754), and 17.67 ms while the board was busy (run-f269418aae73).
- **Reproduce:** from the repository root, `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536` builds the image, runs it and checks the output's SHA.
- **The honest limits:** a name is a hash of about 45 bits, so two of the ~100,000 names merge with odds of about 1.4e-4 per run. The hash keeps a name's first 32 raw bytes, so two names that differ only past those would merge (tested: 32 holds, 33 merges), about 2e-5 per dataset. Longer names are not handled: from reading the code (not run), past 32 raw bytes the mask load starts before its table, so which of the first bytes count is not defined, and a name whose comma falls past the 64-byte mask (about 59 raw bytes) misparses its record with no error. The generator's longest over 40 seeds was 27. A dataset with far more names than the generator's ~100,000 would overrun the name table, which is not checked. join.c has the reasoning.
- **The code:** one file, [join.c](join.c), C with GNU extensions and no dependencies, in clang-format's LLVM style, so the lines you read are the lines S counts. Its top comment gives the design, and the comments say why each part has its shape. A failed open, mmap, clone or fclose stops the program and names what failed.
- **S** is the board's count of code lines after clang-format, plus 6 for the Dockerfile; comments and blank lines do not count.
- **Where each criterion's evidence is:**
  - Code quality: join.c's top comment and the comments above each function.
  - Technique: the table below, each trick linked to its line.
  - Optimisation log: [PERF.md](PERF.md), each step with its board run or local A/B (the few without say so), the failed tries, and a crash in 0.6 % of runs that we found by logging every bad run.

| Trick | Where |
|---|---|
| Each record is read once, with no first pass, from one 64-byte quote mask. A carry-less multiply marks the bytes after an odd number of quotes, so the commas after the name and the note are the first two such bytes that are not quotes, however many `""` the fields hold. The old first pass was 13 % of all instructions | [`parse`](join.c#L346), [`odd_quotes`](join.c#L323) |
| Each piece is read from two places in turn: a record's start depends on the last one's end, about 50 cycles, and two such waits overlap. A run of 64 records is parsed first, each prefetching its region entry, then a second loop does the totals and messages. One pass with no staging was 2.2 ms slower | [`records`](join.c#L428) |
| The next record comes from a carry through the run of low bytes: CR, LF and a BOM's bytes all end the same way, so parsing has no BOM test and no branch on the line ending | [`parse`](join.c#L423) |
| The id's length is kept per cursor, since ids only grow (spec 2.2). One compare of the first quote's place catches a longer id, a short id and an empty name | [`parse`](join.c#L362) |
| Id, region and amount fold together in one vector: a saturating subtract, then three multiplies. Per-lane multipliers step over the amount's decimal point, with no pext | [`fold3`](join.c#L160) |
| Each name has an owner thread, by its hash. Readers send 8-byte messages through lock-free rings, 128 at a time. A sender that finds a ring full files its own messages meanwhile, so two threads can never wait on each other | [`post`](join.c#L311), [`drain`](join.c#L267) |
| 16 tags per bucket on one cache line, compared at once, a hit tested first (97 % of messages) | [`slot_of`](join.c#L205), [`eq16`](join.c#L191) |
| A country bit is set and counted by `bts` and `sbb`: two instructions for nine | [`drain`](join.c#L287) |
| All thread memory on three huge pages at a fixed address, past the range where the kernel may start the heap (at 1 << 30 this crashed 0.6 % of runs: PERF.md, "A crash found and fixed") | [`Mem`](join.c#L111) |
| Only main and one helper fault memory in: any other CPU that charges a page costs a 256 KB batch. The helper starts first and takes 5/8 of the page-table pages, inside its own batch | [`main`](join.c#L568), [`worker`](join.c#L490) |
| Plain clone threads on 4 KB stacks, with no TLS block or thread descriptor to pay for | [`main`](join.c#L581) |
| Pieces spread over the file's 2 MB stretches, so threads that fault pages at once never share a page-table lock (kernel cycles ~70 M to ~60 M at v38; today plain file order is only 0.03 ms slower, so a small gain) | [`worker`](join.c#L497) |
| Each thread drops an eleventh of the mapping while main writes the output, so exit does not tear down 49,000 page-table entries on one CPU | [`worker`](join.c#L521) |
| Waits pause 128 times, then sleep 10 us: beside one busy process, 30 ms where waits that only spin read 331 | [`rest`](join.c#L223) |

## The approach

Twelve threads, one per hardware thread of the scoring box's six P-cores, take 128 KB pieces of the mapped file from a shared counter.

1. Each record is read once, front to back, with no first pass: a 64-byte quote mask starting past most ids, and a carry-less multiply that turns it into "follows an odd number of quotes", find the commas after the name and the note; the region and the amount follow the last comma. Only a piece's first record needs the LF rule (spec 2.2).
2. Each piece is read from two places in turn, so the wait of one record's start on the last one's end overlaps with itself. A run of 64 records is parsed first, each prefetching its region-table entry; a second loop does the lookups, totals and messages.
3. Distinct names are the hard part (2.98 M distinct pairs, about 100,000 names). Each name has an owner thread picked by its hash; readers send 8-byte messages through lock-free rings, 128 at a time, and each owner keeps a private table of 16-tag buckets on a cache line with one bit per country. No atomic instruction touches the hot path; a table shared by all threads took 13 % longer in C (23.00 against 20.35 ms, v50) and almost twice as long in the Rust version.
4. All memory the threads use sits on three huge pages at a fixed address, faulted in by main and one helper thread before parsing starts; the workers are plain clone threads on 4 KB stacks.

## Build

`gcc -O3 -march=x86-64-v3 -mpclmul -maes -mtune-ctrl=^avoid_false_dep_for_bmi -frename-registers -static -o join join.c` with gcc 16 (the Dockerfile builds exactly this). [PERF.md](PERF.md), "The design and why", gives the reason for the two unusual flags, -mtune-ctrl and -frename-registers.

## How it is checked

Every local timing run deletes the output and checks its SHA, so a crash or wrong output leaves no number. Correctness runs over 6 seeds at 12 and at 1 CPU, plus a 100-run crash check on each variant ([PERF.md](PERF.md), "Measuring" and "A crash found and fixed").
