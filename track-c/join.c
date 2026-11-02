// Speed Demon 2026, Track C (Join): records.csv joined to regions.csv and
// aggregated per country. PERF.md has the measurement behind each choice.
//
// How it works, and why it is fast:
//  1. Twelve threads, one per hardware thread of the scoring box's six
//     P-cores. They take 256 KB pieces of the file from a shared counter, so
//     a thread that another process slows down takes fewer. A thread owns
//     the records that start in its piece.
//  2. Record ends need no quote tracking. Inside a quoted note an LF is
//     always followed by a lowercase letter or a quote, and a record's
//     terminating LF by a digit, a BOM or the end of the file (spec 2.2).
//     So pass 1, the only pass that reads every byte, lists every record's
//     end with three AVX2 compares per 32 bytes, and a thread finds its piece's
//     first record without reading anything before it.
//  3. Pass 2 reads each record's fields from both ends. The region and the
//     amount follow the last two commas, which the 32 bytes before the end
//     always hold (that tail has no quotes). The id runs to the first comma.
//     A quoted name ends at its first quote unless that quote is half of a
//     "" (rare: a cold path). The note is what lies between. Region lookups
//     run first, in a loop short enough for the cache misses on the 1 MB
//     region table to overlap.
//  4. Distinct names: a name is identified by the 32-bit tag of a hash of
//     its raw bytes (quoting is one-to-one, and spec 2.2 always quotes a
//     non-empty name) plus its owner and bucket choice, about 45 bits. Two
//     of the ~100,000 names merge with odds of about 1.4e-4 per run
//     (n^2 / 2^46).
//  5. Each name has one owner thread, picked by its hash. The reader sends
//     the owner an 8-byte message (the hash, with the country in the low
//     byte) through a lock-free ring, 128 messages at a time (8 at a time
//     cost 1.5 ms more), and the owner keeps its names in a private table
//     with one bit per country. One table shared by all threads took
//     almost twice as long in the Rust version, and in C (v50) 23.00 ms
//     against 20.35 (PERF.md): lookups missed the core's own cache. The
//     owner's table needs no atomic instruction.
#define _GNU_SOURCE
#include <fcntl.h>
#include <immintrin.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

typedef uint64_t u64;
typedef uint32_t u32;
typedef uint8_t u8;
typedef u64 __attribute__((aligned(1), may_alias)) u64u;
// CHB: 64-byte blocks per pass 1; BATCH: messages staged per owner; RING:
// the messages a ring holds (two batches); PIECE: bytes per piece of
// records.csv (256 KB).
enum { NT = 12, CHB = 128, BATCH = 128, RING = 2 * BATCH, PIECE = 1 << 18 };
enum { MAXREC = 256 }; // records per pass 1 (8 KB): 32 bytes or more each on the generator's data (about 61)
enum { BUCKETS = 1248, SLOTS = 8 * (BUCKETS + 8) };

// One country's running totals, kept per thread: records, id sum, amount
// sum, min and max in cents, name and note bytes once unescaped, distinct names
typedef struct {
  long long n, ids, sum, min, max, nb, tb, names;
} Agg;
// What pass 2's first loop reads out of a record: the id, where the name
// starts, the comma before the region, the region's table index, the amount
// in cents.
typedef struct {
  u32 id, name, comma, reg;
  int32_t cents;
} Rec;
typedef struct { // a thread's outgoing messages, totals and pass 2 records
  _Alignas(64) u64 batch[NT][BATCH]; // each owner's next messages, fill[o] used
  Rec rec[MAXREC];
  u32 ix[MAXREC + 2]; // the record ends pass 1 found that pass 2 has not used
  u32 fill[NT];
  int t;
  Agg *agg;
} Ctx;
typedef struct { // a thread's part of records.csv, parsed in two passes
  const u8 *f;        // its first byte; offsets count from here
  u64 end, at, start; // its length, the next block, the next record
  u32 *ix, n;         // the record ends pass 1 found that pass 2 has not used
} Scan;
typedef struct { // one owner's names; slot i of tag and row i belong to one name
  _Alignas(32) u32 tag[SLOTS]; // 0 = free
  u64 row[SLOTS][4];           // a bit per country
} Names;

static u8 *rec;                    // records.csv, mapped
static u64 rsize, regsize;         // file sizes
static const u8 *reg;              // regions.csv, mapped
typedef struct { // the messages from one thread to one owner, RING at most
  _Alignas(64) _Atomic u64 tail; // advanced by the sender
  _Alignas(64) _Atomic u64 head; // advanced by the owner
  _Alignas(64) u64 m[RING];      // message i is m[i % RING]
} Ring;
static const u8 KEEP[64] = {[0 ... 31] = 0xFF}; // KEEP + 32 - n: n bytes kept

// All memory the threads use, on three transparent huge pages that main
// faults in before any thread starts, so no thread ever page-faults on
// anonymous memory. A fixed address: an aligned static object would put the
// small variables around it on two more huge pages.
typedef struct {
  Names names[NT];
  Ring ring[NT][NT];
  _Alignas(64) Agg agg[NT][256];
  Ctx ctx[NT];
  u8 rtab[1000002]; // id -> country, 0 = unmatched; spec 2.3: ids stop at 999,999; the last entry: any id above
  _Atomic u8 cidx[26 * 26 * 26]; // country code -> country number
  int ncountry;
  pthread_mutex_t cmu; // zero bytes are an unlocked mutex
  atomic_int finished;
  pthread_barrier_t meet; // all NT threads
  _Alignas(64) _Atomic u64 piece; // the next piece of records.csv to parse
  _Alignas(4096) u8 stack[NT][1 << 14]; // the worker threads' stacks (the least pthreads takes)
} Mem;
_Static_assert(sizeof(Mem) <= 3 << 21, "three huge pages");
#define mem (*(Mem *)(1ull << 40))

static void die(const char *what) {
  perror(what);
  exit(1);
}
static inline __m256i load32(const u8 *p) {
  return _mm256_loadu_si256((const void *)p);
}
static inline u32 eq32(const u8 *p, __m256i c) { // a bit per byte of p == c
  return _mm256_movemask_epi8(_mm256_cmpeq_epi8(load32(p), c));
}

// Three numbers at once, each up to 8 raw bytes in the top bytes of a
// 64-bit lane: a saturating subtract of '0' turns digits into their values
// and every other byte (the zeros the shifts brought in, the point, the
// sign, the comma) into 0, so the zeros below read as leading zeros. Digit
// pairs, then quads, then octets combine in one multiply each.
static inline __m256i fold3(u64 a, u64 b, u64 c) {
  __m256i x = _mm256_set_epi64x(0, c, b, a);
  x = _mm256_subs_epu8(x, _mm256_set1_epi8('0'));
  x = _mm256_maddubs_epi16(x, _mm256_set1_epi16(0x010A));    // 10a + b
  x = _mm256_madd_epi16(x, _mm256_set1_epi32(0x00010064));   // 100a + b
  return _mm256_add_epi64(_mm256_mul_epu32(x, _mm256_set1_epi64x(10000)),
                          _mm256_srli_epi64(x, 32));         // 10000a + b
}

static inline u64 quotes(const u8 *p) { // a bit per quote byte of p[0, 64)
  __m256i q = _mm256_set1_epi8('"');
  return eq32(p, q) | (u64)eq32(p + 32, q) << 32;
}

// Name hashing: 32 bytes v at a time go into the 128-bit state h, the first
// 16 by XOR and the last 16 as the key of one AES round. After two more
// rounds every name byte has reached all 128 bits of the state. One round is
// a bijection, so names of up to 16 bytes cannot collide before the state is
// cut to 64 bits.
static inline __m128i step(__m128i h, __m256i v) {
  return _mm_aesenc_si128(_mm_xor_si128(h, _mm256_castsi256_si128(v)),
                          _mm256_extracti128_si256(v, 1));
}
#define SEED _mm_set_epi64x(0x8EBC6AF09C88C6E3, 0xA0761D6478BD642F)
#define K1 _mm_set_epi64x(0x589965CC75374CC3, 0xE7037ED1A0B428DB)
#define K2 _mm_set_epi64x(0x1D8E4E27C47D124F, 0x9E3779B97F4A7C15)
// v's first n bytes end the name
static inline u64 last(__m128i h, __m256i v, u64 n) {
  h = step(h, _mm256_and_si256(v, load32(KEEP + 32 - n)));
  return _mm_cvtsi128_si64(_mm_aesenc_si128(_mm_aesenc_si128(h, K1), K2));
}

// The name field at p, right after the id's comma. Returns the hash of its
// raw bytes (quoting is one-to-one; both empty forms hash as no bytes), the
// length once unescaped in *len and the comma after it in *end. A name of
// 31 raw bytes or more is not handled; spec 2.2 sets no bound, but over 40
// generated seeds the longest was 27 (base name at most 24 by the
// generator's forms, plus runs of trailing spaces that get rarer about 4
// times per extra byte), so about 3e-4 per dataset.
static inline u64 name(const u8 *p, u64 *len, const u8 **end) {
  if (*p == ',') { // empty, unquoted
    *len = 0, *end = p;
    return last(SEED, _mm256_setzero_si256(), 0);
  }
  __m256i v = load32(p + 1);
  u64 q = eq32(p + 1, _mm256_set1_epi8('"'));
  // The name ends at the first byte that is not a quote and follows an odd
  // number of them: a "" flips the count twice (12% of names have one).
  u64 odd = _mm_cvtsi128_si64(_mm_clmulepi64_si128(_mm_cvtsi64_si128(q), _mm_set1_epi8(-1), 0));
  u64 j = _tzcnt_u64(odd & ~q), k = j - 1; // the comma, the closing quote
  *len = k - _mm_popcnt_u64(_bzhi_u64(q, k)) / 2, *end = p + 1 + j;
  return last(SEED, v, k);
}

// The length of the text field p[0, n) once unescaped: an unquoted field is
// its bytes; a quoted one holds q quotes, its two delimiters and two per "",
// and loses q / 2 + 1 bytes.
static inline u64 text(const u8 *p, u64 n) {
  u64 q = 0, m = n;
  for (; m > 64; m -= 64, p += 64)
    q += _mm_popcnt_u64(quotes(p));
  q += _mm_popcnt_u64(_bzhi_u64(quotes(p), m));
  return n - q / 2 - (q > 0);
}

// The slot of the name of message msg; a new name takes the first free slot
// of the walk.
static inline u32 slot_of(Names *nm, u64 msg) {
  u32 tag = msg >> 8 | 1; // bits 8-39
  __m256i want = _mm256_set1_epi32(tag), zero = _mm256_setzero_si256();
  for (u64 b = (msg >> 48) * BUCKETS >> 16;; b++) {
    __m256i v = _mm256_load_si256((const __m256i *)(nm->tag + 8 * b));
    u32 m = _mm256_movemask_ps(_mm256_castsi256_ps(_mm256_or_si256(_mm256_cmpeq_epi32(v, want), _mm256_cmpeq_epi32(v, zero))));
    if (m) {
      u32 s = 8 * b + _tzcnt_u32(m);
      nm->tag[s] = tag;
      return s;
    }
  }
}

static int country(const u8 *p) { // numbers countries in order of first sight
  int k = (p[0] - 'A') * 676 + (p[1] - 'A') * 26 + (p[2] - 'A'), c = mem.cidx[k];
  if (!c) {
    pthread_mutex_lock(&mem.cmu);
    if (!(c = mem.cidx[k]))
      c = mem.cidx[k] = ++mem.ncountry;
    pthread_mutex_unlock(&mem.cmu);
  }
  return c;
}

// The lines that start in this thread's twelfth of regions.csv (thread 0's
// first line, the header, starts at 0 and is skipped).
static void regions(int t) {
  u64 p = t ? regsize * t / NT : 1, e = regsize * (t + 1) / NT;
  while (p < e && reg[p - 1] != '\n')
    p++;
  while (p < e) { // "id,CCC\n": plain ASCII, spec 2.3
    u64 id = 0;
    while (reg[p] != ',')
      id = id * 10 + reg[p++] - '0';
    mem.rtab[id] = country(reg + p + 1);
    p += 5;
  }
}

// The first record starting at or after q. Byte 0 starts the BOM and header
// (spec 2.2), never a record, so the search starts at 1.
static u64 record_start(u64 q) {
  for (q = q ? q : 1; q < rsize; q++)
    if (rec[q - 1] == '\n' && ((u8)(rec[q] - '0') < 10 || rec[q] == 0xEF))
      return q;
  return rsize;
}

// A bit per byte of p[0, 32) that ends a record: an LF that is not followed
// by a lowercase letter (the signed compare takes any byte from 'a' to 0x7F)
// or a quote. A BOM's first byte, 0xEF, compares as negative: a record end.
static inline u32 ends32(const u8 *p) {
  __m256i next = load32(p + 1);
  return _mm256_movemask_epi8(_mm256_andnot_si256(
      _mm256_or_si256(_mm256_cmpgt_epi8(next, _mm256_set1_epi8('a' - 1)),
                      _mm256_cmpeq_epi8(next, _mm256_set1_epi8('"'))),
      _mm256_cmpeq_epi8(load32(p), _mm256_set1_epi8('\n'))));
}

// Pass 1 over the blocks of z's part up to offset to: the offset of every
// record's last byte, its LF, goes to ix.
static void ends(Scan *z, u64 to) {
  // Locals, not z's fields: the stores to ix could alias those.
  const u8 *f = z->f;
  u64 at = z->at, end = z->end, n = z->n;
  u32 *ix = z->ix;
  for (; at < to; at += 64) {
    u64 s = ends32(f + at) | (u64)ends32(f + at + 32) << 32;
    if (end - at < 64) // the part's last block: the rest is the next part's
      s = _bzhi_u64(s, end - at);
    u64 k = n;
    n += _mm_popcnt_u64(s);
    // Two at a time, without a test per bit: a block holds about one. An odd
    // count writes one offset past n, which nothing reads (ix has room).
    do
      for (int i = 0; i < 2; i++, k++, s = _blsr_u64(s))
        ix[k] = at + _tzcnt_u64(s);
    while (s);
  }
  z->at = at, z->n = n;
}

// Files every message that has reached owner t, 64 at a time: first each
// name's slot (its row mostly misses the cache, and these lookups overlap),
// then each country bit. Returns the number of messages.
static u64 drain(int t, Agg *a) {
  Names *nm = &mem.names[t];
  u64 done = 0;
  for (int from = 0; from < NT; from++) {
    Ring *r = &mem.ring[from][t];
    u64 *m = r->m, h = r->head, k = r->tail;
    done += k - h;
    for (u64 e; h < k; h = e) {
      e = k - h < 64 ? k : h + 64;
      u32 id[64];
      for (u64 j = h; j < e; j++) {
        id[j - h] = slot_of(nm, m[j % RING]);
        _mm_prefetch((const char *)nm->row[id[j - h]], _MM_HINT_T0);
      }
      for (u64 j = h; j < e; j++) {
        u64 c = m[j % RING] & 255, bit = 1ull << (c & 63);
        u64 *w = &nm->row[id[j - h]][c >> 6];
        a[c].names += !(*w & bit);
        *w |= bit;
      }
    }
    atomic_store_explicit(&r->head, k, memory_order_release);
  }
  return done;
}

// One step of a wait: a pause, and every 128th a 10 us sleep. A thread that
// another process has pushed off its CPU then gets the CPU this one frees;
// waits that only spin ran 15 times slower beside one busy process (PERF.md).
static void rest(int *w) { ++*w % 128 ? _mm_pause() : (void)usleep(10); }

// Sends x's batch for owner o. While the ring is full the sender files the
// messages sent to it, so two threads that wait on each other both advance.
// Every batch but a thread's last is whole, so none wraps the ring.
static void post(Ctx *x, int o) {
  Ring *r = &mem.ring[x->t][o];
  u64 n = x->fill[o], t = r->tail;
  for (int w = 0; t + n - r->head > RING;)
    if (!drain(x->t, x->agg))
      rest(&w);
  memcpy(r->m + t % RING, x->batch[o], 8 * n);
  atomic_store_explicit(&r->tail, t + n, memory_order_release);
  x->fill[o] = 0;
}

// Pass 2: every record whose end pass 1 has found, in two loops. The first
// reads the three numbers, from both ends of each record, and looks up the
// region: it is short enough for the misses on the 1 MB region table to
// overlap. The second does the totals, the name and the note.
static void records(Ctx *x, Scan *z) {
  // Locals, not globals or z's fields: the stores below could alias those.
  const u8 *f = z->f, *rt = mem.rtab;
  const u32 *ix = z->ix;
  u64 n = z->n, top = 1000001; // top: unmatched
  Rec *rs = x->rec;
  __m256i comma = _mm256_set1_epi8(',');
  for (u64 r = 0, s = z->start; r < n; s = ix[r++] + 1) {
    // A BOM at a record start is not data. The id: 8 digits at most.
    u64 e = ix[r], at = s;
    if (__builtin_expect(f[s] == 0xEF, 0)) // 777 BOMs among 3.32 M records
      at += 3;
    u64 a = _tzcnt_u32(eq32(f + at, comma));
    // The commas before the region and the amount: the last two of the
    // record, which its last 32 bytes hold (spec 2.2: a region id has 7 digits
    // at most and an amount 8 characters, so that tail is 19 bytes at most)
    u32 m = eq32(f + e - 32, comma);
    u64 d = e - 1 - _lzcnt_u32(m);
    u64 c = e - 1 - _lzcnt_u32(_bzhi_u32(m, d + 32 - e)); // the comma before d
    // The amount ends the record: v is its last 8 raw bytes, "DDDDD.CC" with
    // the point at byte 5, and the shift moves the digits before it up over
    // it; fold3 turns the bytes into digit values.
    u64 cr = f[e - 1] == '\r', neg = f[d + 1] == '-', nd = e - cr - d - 2 - neg;
    u64 v = *(const u64u *)(f + e - cr - 8);
    v = (v & 0xFFFF000000000000) | (v << 8 & 0x0000FFFFFFFFFF00);
    __m256i num = fold3(*(const u64u *)(f + at) << (64 - 8 * a),
                        *(const u64u *)(f + c + 1) << (72 - 8 * (d - c)),
                        v & ~0ull << (64 - 8 * nd));
    u64 rid = _mm256_extract_epi64(num, 1);
    int32_t cents = _mm256_extract_epi64(num, 2);
    rid = rid < top ? rid : top;
    _mm_prefetch((const char *)rt + rid, _MM_HINT_T0);
    rs[r] = (Rec){_mm256_cvtsi256_si32(num), at + a + 1, c, rid, neg ? -cents : cents};
  }
  Agg *ag = x->agg;
  for (u64 r = 0; r < n; r++) {
    u64 co = rt[rs[r].reg];
    Agg *g = ag + co;
    int64_t cents = rs[r].cents;
    g->n++;
    g->ids += rs[r].id;
    g->sum += cents;
    if (cents < g->min) // rarely: a thread sees ~900 amounts per country
      g->min = cents;
    if (cents > g->max)
      g->max = cents;
    const u8 *b;
    u64 nl, h = name(f + rs[r].name, &nl, &b);
    g->nb += nl;
    g->tb += text(b + 1, f + rs[r].comma - b - 1);
    u64 o = (h >> 40 & 255) * NT >> 8; // the name's owner: h's bits 40-47
    x->batch[o][x->fill[o]] = (h & ~255ull) | co;
    if (++x->fill[o] == BATCH)
      post(x, o);
  }
  if (n) // else no record ended in these blocks
    z->start = ix[n - 1] + 1;
  z->n = 0;
}

static void *worker(void *arg) {
  int t = (int)(intptr_t)arg;
  Ctx *x = &mem.ctx[t];
  x->agg = mem.agg[t], x->t = t;
  for (int c = 0; c < 256; c++)
    mem.agg[t][c] = (Agg){.min = INT64_MAX, .max = INT64_MIN};
  regions(t);
  pthread_barrier_wait(&mem.meet); // the region table is complete
  u32 *ix = x->ix;
  // Piece i is the (i / n)th of the 2 MB stretch i % n: threads working at
  // once never share a stretch's page-table lock when they fault pages in.
  u64 n = (rsize >> 21) + 1;
  for (u64 i, k = 1; (i = atomic_fetch_add(&mem.piece, 1)) < 8 * n;) {
    u64 c = i % n * 8 * PIECE + i / n * PIECE;
    if (c >= rsize)
      continue;
    u64 lo = record_start(c), hi = record_start(c + PIECE);
    Scan z = {.f = rec + lo, .end = hi - lo, .ix = ix};
    for (; z.at < z.end; k++) {
      ends(&z, z.at + 64 * CHB < z.end ? z.at + 64 * CHB : z.end);
      records(x, &z);
      if (k % 8 == 0)
        drain(t, mem.agg[t]);
    }
  }
  for (int o = 0; o < NT; o++) // the partial batches
    post(x, o);
  // After every send, so the last drain sees all
  atomic_fetch_add(&mem.finished, 1);
  for (int w = 0; mem.finished < NT;)
    if (!drain(t, x->agg))
      rest(&w);
  drain(t, x->agg);
  // Every total is final: main writes them out meanwhile
  pthread_barrier_wait(&mem.meet);
  // Every record is parsed: the other threads drop an eleventh of the
  // mapping each, or exit tears down all 49,000 page-table entries on one CPU.
  if (t) {
    u64 a = rsize * (t - 1) / (NT - 1) & ~4095ull, e = rsize * t / (NT - 1);
    madvise(rec + a, (e & ~4095ull) - a, MADV_DONTNEED);
  }
  return 0;
}

static void money(FILE *o, long long v) { // cents as -?D+.DD
  long long a = llabs(v);
  fprintf(o, " %s%lld.%02lld", v < 0 ? "-" : "", a / 100, a % 100);
}

// Country c's line: the threads' totals added up
static void report(FILE *o, const char *code, int c) {
  Agg g = mem.agg[0][c];
  for (int t = 1; t < NT; t++) {
    Agg *a = &mem.agg[t][c];
    g.n += a->n, g.ids += a->ids, g.sum += a->sum, g.nb += a->nb;
    g.tb += a->tb, g.names += a->names;
    g.min = a->min < g.min ? a->min : g.min;
    g.max = a->max > g.max ? a->max : g.max;
  }
  if (!g.n)
    return;
  fprintf(o, "%s %lld %lld", code, g.n, g.ids);
  money(o, g.sum), money(o, g.min), money(o, g.max);
  fprintf(o, " %lld %lld %lld\n", g.names, g.nb, g.tb);
}

int main(void) {
  int rfd = open("/data/records.csv", O_RDONLY);
  int gfd = open("/data/regions.csv", O_RDONLY);
  if (rfd < 0 || gfd < 0)
    die("open /data");
  rsize = lseek(rfd, 0, SEEK_END), regsize = lseek(gfd, 0, SEEK_END);
  // records.csv is mapped over the start of a zeroed reservation one page
  // longer, so the vector loads that run past its end read zeros.
  rec = mmap(0, rsize + 4096, PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  if (rec == MAP_FAILED)
    die("mmap");
  if (mmap(rec, rsize, PROT_READ, MAP_PRIVATE | MAP_FIXED, rfd, 0) != rec)
    die("mmap records.csv");
  reg = mmap(0, regsize, PROT_READ, MAP_PRIVATE, gfd, 0);
  if (reg == MAP_FAILED)
    die("mmap regions.csv");
  if (mmap(&mem, 3 << 21, PROT_READ | PROT_WRITE,
           MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED | MAP_NORESERVE, -1, 0) != &mem)
    die("mmap");
  madvise(&mem, 3 << 21, MADV_HUGEPAGE);
  // Fault the huge pages in here, and one page per 2 MB of each file (its
  // page-table page): a thread on another CPU that charges even one page to
  // the cgroup counts 256 KB against the peak (a per-CPU charge batch).
  for (u64 i = 0; i < 3 << 21; i += 1 << 21)
    ((volatile u8 *)&mem)[i] = 0;
  for (u64 i = 0; i < rsize + (1 << 21); i += 1 << 21)
    (void)((volatile u8 *)rec)[i < rsize ? i : rsize - 1];
  for (u64 i = 0; i < regsize + (1 << 21); i += 1 << 21)
    (void)((volatile u8 *)reg)[i < regsize ? i : regsize - 1];
  pthread_t th[NT];
  pthread_barrier_init(&mem.meet, 0, NT);
  pthread_attr_t at;
  pthread_attr_init(&at);
  for (long t = 1; t < NT; t++) {
    pthread_attr_setstack(&at, mem.stack[t], 1 << 14);
    if (pthread_create(&th[t], &at, worker, (void *)t))
      die("pthread_create");
  }
  worker(0);
  FILE *o = fopen("/out/aggregates.txt", "w");
  if (!o)
    die("/out/aggregates.txt");
  for (int k = 0; k < 26 * 26 * 26; k++) // in code order
    if (mem.cidx[k])
      report(o, (char[]){'A' + k / 676, 'A' + k / 26 % 26, 'A' + k % 26, 0},
             mem.cidx[k]);
  report(o, "_UNMATCHED", 0); // "_" sorts after every code
  if (fclose(o))
    die("/out/aggregates.txt");
  for (int t = 1; t < NT; t++)
    pthread_join(th[t], 0);
  return 0;
}
