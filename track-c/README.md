# Track C: Join

records.csv (202 MB, 3.32 M records with quoted and escaped name and note fields, embedded newlines and mid-file BOMs) joined to regions.csv and aggregated per country: count, id sum, amount sum, min and max, distinct names, name and note bytes.

## The result

Board run run-6d3cdfadd754: T 14.32 ms, M 8.07 MB, S 373, K 56.60. The reference takes 14.44 s (K 1). The same run is the submission named in [PERF.md](PERF.md).

## The approach

Twelve threads, one per hardware thread of the scoring box's six P-cores, take 128 KB pieces of the mapped file from a shared counter.

1. Each record is read once, front to back, with no first pass: a 64-byte quote mask starting past most ids, and a carry-less multiply that turns it into "follows an odd number of quotes", find the commas after the name and the note; the region and the amount follow the last comma. Only a piece's first record needs the LF rule (spec 2.2).
2. Each piece is read from two places in turn, so the wait of one record's start on the last one's end overlaps with itself. A run of 64 records is parsed first, each prefetching its region-table entry; a second loop does the lookups, totals and messages.
3. Distinct names are the hard part (2.98 M distinct pairs, about 100,000 names). Each name has an owner thread picked by its hash; readers send 8-byte messages through lock-free rings, 128 at a time, and each owner keeps a private table of 16-tag buckets on a cache line with one bit per country. No atomic instruction touches the hot path; a table shared by all threads took almost twice as long.
4. All memory the threads use sits on three huge pages at a fixed address, faulted in by main and one helper thread before parsing starts; the workers are plain clone threads on 4 KB stacks.

**The accuracy trade, plainly:** a name is identified by a hash tag plus its owner and bucket, about 45 bits, so two names can merge with odds of about 1.4e-4 per run; and a name of over 30 raw bytes is not handled, about 3e-4 per dataset. join.c's header has the reasoning.

## Build

`gcc -O3 -march=x86-64-v3 -mpclmul -maes -mtune-ctrl=^avoid_false_dep_for_bmi -frename-registers -static -o join join.c` with gcc 16 (the Dockerfile builds exactly this).

## How it is checked

Every local timing run deletes the output and checks its SHA, so a crash or wrong output leaves no number. Correctness runs over 6 seeds at 12 and at 1 CPU, plus a 100-run crash check on each variant ([PERF.md](PERF.md), "Measuring" and "A crash found and fixed").

## Reproduce

From the repository root: `python3 check.py c --data data/c --expected d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`
