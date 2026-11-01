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
//     end with three AVX2 compares per byte, and a thread finds its piece's
//     first record without reading anything before it.
//  3. Pass 2 reads each record's fields from both ends. The region and the
//     amount follow the last two commas, which the 32 bytes before the end
//     always hold (that tail has no quotes). The id runs to the first comma.
//     A quoted name ends at its first quote unless that quote is half of a
//     "" (rare: a cold path). The note is what lies between. A first loop
//     reads the numbers and hashes the name, and prefetches what the second
//     loop looks up: the region's country and the name's table bucket.
//  4. Distinct names: a name is identified by 54 bits of a hash of its raw
//     bytes (quoting is one-to-one, and spec 2.2 always quotes a non-empty
//     name). Two of the ~100,000 names share those bits with odds of about
//     3e-7 per run. One table shared by all threads maps the hash to the
//     name's owner and row. It fills in the first few percent of the file
//     and is only read after that, so each core keeps a copy in its cache.
//     The thread that adds a name owns it.
//  5. The reader sends the owner a 4-byte message (the row and the country)
//     through a lock-free ring, 128 at a time (8 at a time cost 1.5 ms more),
//     and the owner sets the country's bit in the row. Setting the bits in
//     rows shared by all threads took almost twice as long (PERF.md): every
//     write took the line from another core. The owner needs no atomics.
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
// CHB: 64-byte blocks per pass 1; BATCH: messages staged per owner.
// MAXREC: records per pass 1, each 11 bytes or more. TAB: name table slots,
// 77% full with spec 2.2's pool of 100,000 names.
enum { NT = 12, CHB = 256, BATCH = 128, RING = 2 * BATCH, PIECE = 1 << 18 };
enum { MAXREC = 64 * CHB / 8 + 2, TAB = 1 << 17 };

// One country's running totals, kept per thread: records, id sum, amount
// sum, min and max in cents, name and note bytes once unescaped, distinct names
typedef struct {
  long long n, ids, sum, min, max, nb, tb, names;
} Agg;
// What pass 2's first loop reads out of a record: the id, the comma after
// the name, the comma before the region, the region's table index, the name's
// length once unescaped, the amount in cents, the name's hash.
typedef struct {
  u32 id, nend, comma, reg, nl;
  int32_t cents;
  u64 h;
} Rec;
typedef struct { // a thread's outgoing messages, totals and pass 2 records
  _Alignas(64) u32 batch[NT][BATCH]; // each owner's next messages, fill[o] used
  Rec rec[MAXREC];
  u32 fill[NT];
  int t;
  Agg *agg;
} Ctx;
typedef struct { // a thread's part of records.csv, parsed in two passes
  const u8 *f;        // its first byte; offsets count from here
  u64 end, at, start; // its length, the next block, the next record
  u32 *ix, n;         // the record ends pass 1 found that pass 2 has not used
} Scan;
typedef struct { // one owner's names
  _Alignas(64) u64 (*row)[4]; // a bit per country
  u64 n;                      // rows used
} Names;

static u8 *rec;                       // records.csv, mapped
static u64 rsize, regsize, maxreg;    // file sizes, the largest region id
static const u8 *reg;                 // regions.csv, mapped
static u8 *rtab; // region id -> country, 0 = unmatched (the data has 239)
static _Atomic u8 cidx[26 * 26 * 26]; // country code -> country number
static int ncountry;
static pthread_mutex_t cmu = PTHREAD_MUTEX_INITIALIZER;
static atomic_int finished;
static pthread_barrier_t meet; // all NT threads
static _Atomic u64 piece; // the next piece of records.csv to parse
typedef struct { // the messages from one thread to one owner, RING at most
  _Alignas(64) _Atomic u64 tail; // advanced by the sender
  _Alignas(64) _Atomic u64 head; // advanced by the owner
  _Alignas(64) u32 m[RING];      // message i is m[i % RING]
} Ring;
static Ring ring[NT][NT]; // [sender][owner]
static Agg agg[NT][256];
static Names names[NT];
static _Atomic u64 *tab; // the names, see intern()
static const u8 KEEP[64] = {[0 ... 31] = 0xFF}; // KEEP + 32 - n: n bytes kept

static void die(const char *what) {
  perror(what);
  exit(1);
}
static void *big(u64 n) { // zeroed; only the pages touched count as memory
  void *p = mmap(0, n, PROT_READ | PROT_WRITE,
                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  if (p == MAP_FAILED)
    die("mmap");
  // A 2 MB page would be zeroed and counted whole
  madvise(p, n, MADV_NOHUGEPAGE);
  return p;
}
static inline u64 digits(const u8 *p) { // p[0, 8) as digit values
  u64 v;
  memcpy(&v, p, 8);
  return v ^ 0x3030303030303030;
}
static inline __m256i load32(const u8 *p) {
  return _mm256_loadu_si256((const void *)p);
}
static inline u32 eq32(const u8 *p, __m256i c) { // a bit per byte of p == c
  return _mm256_movemask_epi8(_mm256_cmpeq_epi8(load32(p), c));
}

// Three numbers at once, each up to 8 digit values in the top bytes of a
// 64-bit lane (the zeros below read as leading zeros): digit pairs, then
// quads, then octets combine in one multiply each.
static inline __m256i fold3(u64 a, u64 b, u64 c) {
  __m256i x = _mm256_set_epi64x(0, c, b, a);
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

// A name of 31 bytes or more (spec 2.2 sets no bound; the published data
// has none over 29). p is its first byte after the opening quote; returns
// what name() does, the same hash the fast path gives.
__attribute__((cold, noinline)) static u64 slow(const u8 *p, u64 *len,
                                                const u8 **end) {
  u64 n = 0, pairs = 0;
  for (;; n++)
    if (p[n] == '"') {
      if (p[n + 1] != '"')
        break;
      n++, pairs++;
    }
  *len = n - pairs, *end = p + n + 1;
  __m128i h = SEED;
  for (; n > 32; n -= 32, p += 32)
    h = step(h, load32(p));
  return last(h, load32(p), n);
}

// The name field at p, right after the id's comma. Returns the hash of its
// raw bytes (quoting is one-to-one; both empty forms hash as no bytes), the
// length once unescaped in *len and the comma after it in *end.
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
  if (__builtin_expect(j >= 32, 0))
    return slow(p + 1, len, end);
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

// The table slot of the name with hash h. A slot holds h's top 40 bits (the
// tag), the name's row at its owner (bits 4-23) and the owner + 1 (bits 0-3),
// so a used slot is never 0. The walk starts at the 8-slot bucket that h's
// low 14 bits pick and goes on past full buckets; slots fill in order and
// never empty, so a name that is there comes before the walk's first free
// slot. A name not there yet takes that slot, owned by thread t; of two
// threads that add a name at once, one wins the slot and both use it.
__attribute__((noinline)) static u64 intern(u64 h, int t) {
  for (u64 i = 8 * (h & (TAB / 8 - 1));; i = (i + 1) & (TAB - 1)) {
    u64 s = atomic_load_explicit(&tab[i], memory_order_relaxed);
    if (!s) {
      u64 mine = h >> 24 << 24 | names[t].n << 4 | (t + 1);
      if (atomic_compare_exchange_strong(&tab[i], &s, mine)) {
        names[t].n++;
        return mine;
      }
    }
    if (s >> 24 == h >> 24)
      return s;
  }
}

// intern(), when the name is in its first bucket (in ~19 of 20 lookups once
// the table is full): a compare of all eight slots, which may see an old copy
// of the bucket, but a slot found never changes
static inline u64 lookup(u64 h, int t) {
  const u64 *b = (const u64 *)tab + 8 * (h & (TAB / 8 - 1));
  __m256i tag = _mm256_set1_epi64x(h >> 24);
  u32 m = _mm256_movemask_pd(_mm256_castsi256_pd(
              _mm256_cmpeq_epi64(_mm256_srli_epi64(load32((const u8 *)b), 24), tag))) |
          _mm256_movemask_pd(_mm256_castsi256_pd(
              _mm256_cmpeq_epi64(_mm256_srli_epi64(load32((const u8 *)(b + 4)), 24), tag))) << 4;
  return m ? b[_tzcnt_u32(m)] : intern(h, t);
}

static int country(const u8 *p) { // numbers countries in order of first sight
  int k = (p[0] - 'A') * 676 + (p[1] - 'A') * 26 + (p[2] - 'A'), c = cidx[k];
  if (!c) {
    pthread_mutex_lock(&cmu);
    if (!(c = cidx[k]))
      c = cidx[k] = ++ncountry;
    pthread_mutex_unlock(&cmu);
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
    rtab[id] = country(reg + p + 1);
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

// Files every message that has reached owner t, 64 at a time: first a
// prefetch of each row (rows often miss the cache, and these misses
// overlap), then each country bit. Returns the number of messages.
static u64 drain(int t, Agg *a) {
  u64 (*row)[4] = names[t].row, done = 0;
  for (int from = 0; from < NT; from++) {
    Ring *r = &ring[from][t];
    u32 *m = r->m;
    u64 h = r->head, k = r->tail;
    done += k - h;
    for (u64 e; h < k; h = e) {
      e = k - h < 64 ? k : h + 64;
      for (u64 j = h; j < e; j++)
        _mm_prefetch((const char *)row[m[j % RING] >> 8], _MM_HINT_T0);
      for (u64 j = h; j < e; j++) {
        u64 c = m[j % RING] & 255, bit = 1ull << (c & 63);
        u64 *w = &row[m[j % RING] >> 8][c >> 6];
        a[c].names += !(*w & bit);
        *w |= bit;
      }
    }
    atomic_store_explicit(&r->head, k, memory_order_release);
  }
  return done;
}

// Sends x's batch for owner o. While the ring is full the sender files the
// messages sent to it, so two threads that wait on each other both advance.
// Every batch but a thread's last is whole, so none wraps the ring.
static void post(Ctx *x, int o) {
  Ring *r = &ring[x->t][o];
  u64 n = x->fill[o], t = r->tail;
  while (t + n - r->head > RING)
    if (!drain(x->t, x->agg))
      _mm_pause();
  memcpy(r->m + t % RING, x->batch[o], 4 * n);
  atomic_store_explicit(&r->tail, t + n, memory_order_release);
  x->fill[o] = 0;
}

// Pass 2: every record whose end pass 1 has found, in two loops. The first
// reads the three numbers, from both ends of each record, and the name, and
// prefetches the region's country and the name's bucket (both tables miss
// the cache). The second does the totals, the note and the name's message.
static void records(Ctx *x, Scan *z) {
  // Locals, not globals or z's fields: the stores below could alias those.
  const u8 *f = z->f, *rt = rtab;
  const u32 *ix = z->ix;
  u64 n = z->n, top = maxreg + 1; // top: unmatched
  Rec *rs = x->rec;
  __m256i comma = _mm256_set1_epi8(',');
  for (u64 r = 0, s = z->start; r < n; s = ix[r++] + 1) {
    // A BOM at a record start is not data. The id: 8 digits at most.
    u64 e = ix[r], at = s + (f[s] == 0xEF ? 3 : 0);
    u64 a = _tzcnt_u32(eq32(f + at, comma));
    // The commas before the region and the amount: the last two of the
    // record, which its last 32 bytes hold (spec 2.2: a region id has 7 digits
    // at most and an amount 8 characters, so that tail is 19 bytes at most)
    u32 m = eq32(f + e - 32, comma);
    u64 d = e - 1 - _lzcnt_u32(m);
    u64 c = e - 1 - _lzcnt_u32(_bzhi_u32(m, d + 32 - e));
    // The amount ends the record: v is its last 8 bytes, "DDDDD.CC" with the
    // point at byte 5, and the shift moves the digits before it up over it
    u64 cr = f[e - 1] == '\r', neg = f[d + 1] == '-', nd = e - cr - d - 2 - neg;
    u64 v = digits(f + e - cr - 8);
    v = (v & 0xFFFF000000000000) | (v << 8 & 0x0000FFFFFFFFFF00);
    __m256i num = fold3(digits(f + at) << (64 - 8 * a),
                        digits(f + c + 1) << (72 - 8 * (d - c)),
                        v & ~0ull << (64 - 8 * nd));
    u64 rid = _mm256_extract_epi64(num, 1);
    int32_t cents = _mm256_extract_epi64(num, 2);
    rid = rid < top ? rid : top;
    _mm_prefetch((const char *)rt + rid, _MM_HINT_T0);
    const u8 *b;
    u64 nl, h = name(f + at + a + 1, &nl, &b);
    _mm_prefetch((const char *)(tab + 8 * (h & (TAB / 8 - 1))), _MM_HINT_T0);
    rs[r] = (Rec){_mm256_cvtsi256_si32(num), b - f, c, rid, nl, neg ? -cents : cents, h};
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
    g->nb += rs[r].nl;
    g->tb += text(f + rs[r].nend + 1, rs[r].comma - rs[r].nend - 1);
    u64 s = lookup(rs[r].h, x->t), o = (s & 15) - 1;
    x->batch[o][x->fill[o]] = (s >> 4 & 0xFFFFF) << 8 | co;
    if (++x->fill[o] == BATCH)
      post(x, o);
  }
  if (n) // else no record ended in these blocks
    z->start = ix[n - 1] + 1;
  z->n = 0;
}

static void *worker(void *arg) {
  int t = (int)(intptr_t)arg;
  static Ctx ctx[NT];
  Ctx *x = &ctx[t];
  x->agg = agg[t], x->t = t;
  for (int c = 0; c < 256; c++)
    agg[t][c] = (Agg){.min = INT64_MAX, .max = INT64_MIN};
  regions(t);
  pthread_barrier_wait(&meet); // the region table is complete
  names[t].row = big(TAB * sizeof *names[t].row);
  u32 *ix = big(4 * (MAXREC + 2));
  // Piece i is the (i / n)th of the 2 MB stretch i % n: threads working at
  // once never share a stretch's page-table lock when they fault pages in.
  u64 n = (rsize >> 21) + 1;
  for (u64 i, k = 1; (i = atomic_fetch_add(&piece, 1)) < 8 * n;) {
    u64 c = i % n * 8 * PIECE + i / n * PIECE;
    if (c >= rsize)
      continue;
    u64 lo = record_start(c), hi = record_start(c + PIECE);
    Scan z = {.f = rec + lo, .end = hi - lo, .ix = ix};
    for (; z.at < z.end; k++) {
      ends(&z, z.at + 64 * CHB < z.end ? z.at + 64 * CHB : z.end);
      records(x, &z);
      if (k % 8 == 0)
        drain(t, x->agg);
    }
  }
  for (int o = 0; o < NT; o++) // the partial batches
    post(x, o);
  // After every send, so the last drain sees all
  atomic_fetch_add(&finished, 1);
  while (finished < NT)
    if (!drain(t, x->agg))
      _mm_pause();
  drain(t, x->agg);
  // Every total is final: main writes them out meanwhile
  pthread_barrier_wait(&meet);
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
  Agg g = agg[0][c];
  for (int t = 1; t < NT; t++) {
    Agg *a = &agg[t][c];
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
  rec = big(rsize + 4096);
  if (mmap(rec, rsize, PROT_READ, MAP_PRIVATE | MAP_FIXED, rfd, 0) != rec)
    die("mmap records.csv");
  reg = mmap(0, regsize, PROT_READ, MAP_PRIVATE, gfd, 0);
  if (reg == MAP_FAILED)
    die("mmap regions.csv");
  // Ids ascend (spec 2.3): the last line has the largest. regsize - 1 skips
  // the file's final LF.
  maxreg = strtoull((char *)memrchr(reg, '\n', regsize - 1) + 1, 0, 10);
  rtab = big(maxreg + 2); // the last entry: every id above the range
  tab = big(TAB * 8);
  pthread_t th[NT];
  pthread_barrier_init(&meet, 0, NT);
  for (long t = 1; t < NT; t++)
    if (pthread_create(&th[t], 0, worker, (void *)t))
      die("pthread_create");
  worker(0);
  FILE *o = fopen("/out/aggregates.txt", "w");
  if (!o)
    die("/out/aggregates.txt");
  for (int k = 0; k < 26 * 26 * 26; k++) // in code order
    if (cidx[k])
      report(o, (char[]){'A' + k / 676, 'A' + k / 26 % 26, 'A' + k % 26, 0},
             cidx[k]);
  report(o, "_UNMATCHED", 0); // "_" sorts after every code
  if (fclose(o))
    die("/out/aggregates.txt");
  for (int t = 1; t < NT; t++)
    pthread_join(th[t], 0);
  return 0;
}
