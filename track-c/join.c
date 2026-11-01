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
//     "" (rare: a cold path). The note is what lies between. Region lookups
//     run first, in a loop short enough for the cache misses on the 1 MB
//     region table to overlap.
//  4. Distinct names: a name is identified by 56 bits of a hash of its raw
//     bytes (quoting is one-to-one, and spec 2.2 always quotes a non-empty
//     name). Two of the ~100,000 names share those bits with odds of about
//     1e-7 per run.
//  5. Each name has one owner thread, picked by its hash. The reader sends
//     the owner an 8-byte message (the hash, with the country in the low
//     byte) through a lock-free ring, 128 messages at a time (8 at a time
//     cost 1.5 ms more), and the owner keeps its names in a private table
//     with one bit per country. One table shared by all
//     threads took almost twice as long (PERF.md): every lookup missed the
//     core's own cache. The owner's table needs no atomic instruction.
#define _GNU_SOURCE
#include <fcntl.h>
#include <malloc.h>
#include <immintrin.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stddef.h>
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
// MAXREC: records per pass 1, each 11 bytes or more; HOTREC: the most the
// data puts in one pass 1 (under 300), with room to spare. SLOTS: an owner's
// first name table, which holds NAMES names (an owner gets ~8,400).
enum { NT = 12, CHB = 256, BATCH = 128, RING = 2 * BATCH, PIECE = 1 << 18 };
enum { MAXREC = 64 * CHB / 8 + 2, HOTREC = 512, STACK = 1 << 16 };
enum { SLOTS = 1 << 14, NAMES = SLOTS * 9 / 16, HUGE = 1 << 21 };
enum { CHARGE = 64 << 12 }; // a CPU's memcg charge batch, 64 pages

typedef struct { // one country's running totals, kept per thread
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
  u32 fill[NT];
  int t;
  Agg *agg;
  u32 ix[MAXREC + 2]; // the record ends pass 1 found
  Rec rec[MAXREC];
} Ctx;
typedef struct { // a thread's part of records.csv, parsed in two passes
  const u8 *f;        // its first byte; offsets count from here
  u64 end, at, start; // its length, the next block, the next record
  u32 *ix, n;         // the record ends pass 1 found that pass 2 has not used
} Scan;
typedef struct { // one owner's names
  u64 *slot, mask, n; // 4-slot buckets (mask: their number - 1), see find()
  uint16_t *lo;       // id -> its message's bits 8-23, to refile on grow
  u64 (*row)[4];      // id -> a bit per country
} Names;

static u8 *rec;                       // records.csv, mapped
static u64 rsize, regsize, maxreg;    // file sizes, the largest region id
static const u8 *reg;                 // regions.csv, mapped
// Region id -> country (0: unmatched), for 200,000 ids among 1,000,000. Ids
// ascend, one per line: has[w] has a bit per id 64w..64w+63, first[w] is the
// line of word w's first id and ctry[line] the country.
static u64 *has;
static u32 *first;
static u8 *ctry;
static _Atomic u64 nline[NT]; // the lines of each share of regions.csv, + 1
static _Atomic u8 cidx[26 * 26 * 26]; // country code -> country number
static int ncountry;
static pthread_mutex_t cmu = PTHREAD_MUTEX_INITIALIZER;
static atomic_int finished;
static pthread_barrier_t meet; // all NT threads
static _Atomic u64 piece; // the next piece of records.csv to parse
typedef struct { // the messages from one thread to one owner, RING at most
  _Alignas(64) _Atomic u64 tail; // advanced by the sender
  _Alignas(64) _Atomic u64 head; // advanced by the owner
  _Alignas(64) u64 m[RING];      // message i is m[i % RING]
} Ring;
static Ring (*ring)[NT]; // [sender][owner]
static Agg (*agg)[256];
static Names names[NT];
static Ctx *ctx;    // [NT]
static u8 *shared;  // the tables above
static u64 carved;  // how much of them
// shared is faulted in CHARGE bytes at a time: the stretch to do next, the
// ones done of the region table's rstretch, all nstretch
static _Atomic u64 next;
static atomic_int ready;
static u64 rstretch, nstretch;
extern char __bss_start[], _end[]; // the zeroed statics
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
// Faults in p[0, n) now. The kernel charges 4 KB page faults to the cgroup 64
// pages ahead per CPU, so faults on all 12 CPUs would count up to 3 MB (in M)
// never used. Main faults in what the threads write alone; the tables they
// share are faulted in whole charge batches, see spare().
static void hot(void *p, u64 n) {
  u64 a = (uintptr_t)p & ~4095ull;
  madvise((void *)a, (uintptr_t)p + n - a, MADV_POPULATE_WRITE);
}
// Faults in a byte per 2 MB of p[0, n) and the last, so each 2 MB of a file
// mapping gets its page-table page (a 4 KB charge) here
static void tables(const u8 *p, u64 n) {
  for (u64 i = 0; i < n; i += HUGE)
    (void)*(volatile const u8 *)(p + i);
  (void)*(volatile const u8 *)(p + n - 1);
}
// Faults in stretches of shared until stretch `to` is taken
static void spare(u64 to) {
  for (u64 s; next < to && (s = next++) < nstretch;) {
    hot(shared + s * CHARGE, CHARGE);
    if (s < rstretch)
      ready++;
  }
}
static void *carve(u64 n) { // the next n bytes of shared
  void *p = shared + carved;
  carved += (n + 63) & ~63ull;
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
// its bytes; a quoted one loses its two delimiters and one byte per "".
static inline u64 text(const u8 *p, u64 n) {
  u64 q = 0, m = n;
  for (; m > 64; m -= 64, p += 64)
    q += _mm_popcnt_u64(quotes(p));
  q += _mm_popcnt_u64(_bzhi_u64(quotes(p), m));
  return n - q / 2 - (q > 0);
}

// The slot of the name of message msg, or the free slot where it goes. A slot
// holds message bits 20-63 (the tag) and the name's id + 1. The walk starts
// at the 4-slot bucket that message bits 8-19 pick and goes on past full
// buckets; slots fill in order and never empty, so a name that is there comes
// before the walk's first free slot. Bucket and tag together are 56 bits.
static inline u64 *find(u64 *slot, u64 mask, u64 msg) {
  __m256i tag = _mm256_set1_epi64x(msg >> 20), zero = _mm256_setzero_si256();
  for (u64 b = msg >> 8 & mask;; b = (b + 1) & mask) {
    __m256i v = _mm256_load_si256((const __m256i *)(slot + 4 * b));
    u32 m = _mm256_movemask_pd(_mm256_castsi256_pd(
        _mm256_or_si256(_mm256_cmpeq_epi64(_mm256_srli_epi64(v, 20), tag),
                        _mm256_cmpeq_epi64(v, zero))));
    if (m)
      return slot + 4 * b + _tzcnt_u32(m);
  }
}

// Doubles the table at 9/16 full and refiles every name; ids and rows stay.
// lo[id] keeps the message bits 8-19 that the slot lacks.
static void grow(Names *m) {
  u64 *old = m->slot, n = 4 * (m->mask + 1), size = 2 * n, cap = size * 9 / 16;
  uint16_t *lo = malloc(cap * sizeof *lo);
  u64(*row)[4] = malloc(cap * sizeof *row);
  if (!lo || !row)
    die("malloc");
  memcpy(lo, m->lo, m->n * sizeof *lo), memcpy(row, m->row, m->n * sizeof *row);
  m->slot = big(size * 8), m->lo = lo, m->row = row, m->mask = size / 4 - 1;
  for (u64 i = 0; i < n; i++)
    if (old[i])
      *find(m->slot, m->mask, old[i] >> 20 << 20 | (u64)(m->lo[(old[i] & 0xFFFFF) - 1] & 0xFFF) << 8) = old[i];
}

// Files the name of message msg, seen for the first time; returns its slot.
__attribute__((cold)) static u64 add(Names *m, u64 msg) {
  if (16 * m->n >= 9 * 4 * (m->mask + 1))
    grow(m);
  memset(m->row[m->n], 0, 32); // rows are touched only once used
  m->lo[m->n] = msg >> 8;
  return *find(m->slot, m->mask, msg) = msg >> 20 << 20 | ++m->n;
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

// The lines that start in share i of NT - 2 of regions.csv. Their numbers
// count on from the lines of the shares before, which a thread counts first.
static void regions(int i) {
  u64 p = i ? regsize * i / (NT - 2) : 1, e = regsize * (i + 1) / (NT - 2);
  while (p < e && reg[p - 1] != '\n')
    p++;
  u64 n = 0, j = p - 1, line = 0; // the LFs before each line start in [p, e)
  for (; j + 32 < e; j += 32)
    n += _mm_popcnt_u32(eq32(reg + j, _mm256_set1_epi8('\n')));
  for (; j + 1 < e; j++)
    n += reg[j] == '\n';
  nline[i] = n + 1;
  for (int k = 0; k < i; k++) {
    while (!nline[k])
      _mm_pause();
    line += nline[k] - 1;
  }
  // The word of the line before: if our first id shares it, so does its first
  u64 q = p - 1;
  while (q && reg[q - 1] != '\n')
    q--;
  u64 w = strtoull((const char *)reg + q, 0, 10) >> 6, bits = 0; // header: 0
  while (ready < rstretch) // the table is in
    _mm_pause();
  for (; p < e; p += 5, line++) { // "id,CCC\n": plain ASCII, spec 2.3
    u64 id = 0;
    while (reg[p] != ',')
      id = id * 10 + reg[p++] - '0';
    if (id >> 6 != w) { // words at the shares' ends are shared
      __atomic_fetch_or(&has[w], bits, __ATOMIC_RELAXED);
      w = id >> 6, bits = 0, first[w] = line;
    }
    bits |= 1ull << (id & 63);
    ctry[line] = country(reg + p + 1);
  }
  __atomic_fetch_or(&has[w], bits, __ATOMIC_RELAXED);
}

static u64 record_start(u64 q) { // the first record starting at or after q
  for (q = q ? q : 1; q < rsize; q++)
    if (rec[q - 1] == '\n' && ((u8)(rec[q] - '0') < 10 || rec[q] == 0xEF))
      return q;
  return rsize;
}

// A bit per byte of p[0, 32) that ends a record: an LF that is not followed
// by a lowercase letter or a quote.
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
    do // two at a time, without a test per bit: a block holds about one
      for (int i = 0; i < 2; i++, k++, s = _blsr_u64(s))
        ix[k] = at + _tzcnt_u64(s);
    while (s);
  }
  z->at = at, z->n = n;
}

// Files every message that has reached owner t, 64 at a time: first each
// name's id (its slot and row mostly miss the cache, and these lookups
// overlap), then each country bit. Returns the number of messages.
static u64 drain(int t, Agg *a) {
  Names *nm = &names[t];
  u64 done = 0;
  for (int from = 0; from < NT; from++) {
    Ring *r = &ring[from][t];
    u64 *m = r->m, h = r->head, k = r->tail;
    done += k - h;
    for (u64 e; h < k; h = e) {
      e = k - h < 64 ? k : h + 64;
      u32 id[64];
      u64 *slot = nm->slot, mask = nm->mask, (*row)[4] = nm->row;
      for (u64 j = h; j < e; j++) {
        u64 msg = m[j % RING], s = *find(slot, mask, msg);
        if (!s)
          s = add(nm, msg), slot = nm->slot, mask = nm->mask, row = nm->row;
        id[j - h] = (s & 0xFFFFF) - 1;
        _mm_prefetch((const char *)row[id[j - h]], _MM_HINT_T0);
      }
      for (u64 j = h; j < e; j++) {
        u64 c = m[j % RING] & 255, bit = 1ull << (c & 63);
        u64 *w = &row[id[j - h]][c >> 6];
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
  const u8 *f = z->f, *cs = ctry;
  const u64 *hs = has;
  const u32 *fs = first;
  const u32 *ix = z->ix;
  u64 n = z->n, top = maxreg + 1; // top: unmatched
  Rec *rs = x->rec;
  __m256i comma = _mm256_set1_epi8(',');
  for (u64 r = 0, s = z->start; r < n; s = ix[r++] + 1) {
    // A BOM at a record start is not data. The id: 8 digits at most.
    u64 e = ix[r], at = s + (f[s] == 0xEF ? 3 : 0);
    u64 a = _tzcnt_u32(eq32(f + at, comma));
    // The commas before the region and the amount: the last two of the
    // record, which its last 32 bytes hold
    u32 m = eq32(f + e - 32, comma);
    u64 d = e - 1 - _lzcnt_u32(m);
    u64 c = e - 1 - _lzcnt_u32(_bzhi_u32(m, d + 32 - e));
    // The amount ends the record: its digits, the point dropped, end a load
    u64 cr = f[e - 1] == '\r', neg = f[d + 1] == '-', nd = e - cr - d - 2 - neg;
    u64 v = digits(f + e - cr - 8);
    v = (v & 0xFFFF000000000000) | (v << 8 & 0x0000FFFFFFFFFF00);
    __m256i num = fold3(digits(f + at) << (64 - 8 * a),
                        digits(f + c + 1) << (72 - 8 * (d - c)),
                        v & ~0ull << (64 - 8 * nd));
    u64 rid = _mm256_extract_epi64(num, 1);
    int32_t cents = _mm256_extract_epi64(num, 2);
    rid = rid < top ? rid : top;
    _mm_prefetch((const char *)(hs + (rid >> 6)), _MM_HINT_T0);
    _mm_prefetch((const char *)(fs + (rid >> 6)), _MM_HINT_T0);
    rs[r] = (Rec){_mm256_cvtsi256_si32(num), at + a + 1, c, rid, neg ? -cents : cents};
  }
  Agg *ag = x->agg;
  for (u64 r = 0; r < n; r++) {
    u64 id = rs[r].reg, w = hs[id >> 6], in = w >> (id & 63) & 1;
    u64 cr = cs[fs[id >> 6] + _mm_popcnt_u64(_bzhi_u64(w, id & 63))] & -in;
    Agg *g = ag + cr;
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
    u64 o = (h >> 32) * NT >> 32; // the name's owner
    x->batch[o][x->fill[o]] = (h & ~255ull) | cr;
    if (++x->fill[o] == BATCH)
      post(x, o);
  }
  if (n)
    z->start = ix[n - 1] + 1;
  z->n = 0;
}

static void *worker(void *arg) {
  int t = (int)(intptr_t)arg;
  Ctx *x = &ctx[t];
  // The page-table pages of records.csv: thread NT - 1 charges the first 64,
  // one CPU's charge batch, main the rest
  if (t == NT - 1)
    tables(rec, rsize < 64 * HUGE ? rsize + 1 : 64 * HUGE);
  else if (t)
    spare(rstretch), regions(t - 1);
  spare(nstretch);
  pthread_barrier_wait(&meet); // the region table is complete
  for (int c = 0; c < 256; c++)
    agg[t][c] = (Agg){.min = INT64_MAX, .max = INT64_MIN};
  x->agg = agg[t], x->t = t;
  // Piece i is the (i / n)th of the 2 MB stretch i % n: threads working at
  // once never share a stretch's page-table lock when they fault pages in.
  u64 n = (rsize >> 21) + 1;
  for (u64 i, k = 1; (i = atomic_fetch_add(&piece, 1)) < 8 * n;) {
    u64 c = i % n * 8 * PIECE + i / n * PIECE;
    if (c >= rsize)
      continue;
    u64 lo = record_start(c), hi = record_start(c + PIECE);
    Scan z = {.f = rec + lo, .end = hi - lo, .ix = x->ix};
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
  mallopt(M_ARENA_MAX, 1);
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
  // Ids ascend (spec 2.3): the last line has the largest
  tables(reg, regsize);
  maxreg = strtoull((char *)memrchr(reg, '\n', regsize - 1) + 1, 0, 10);
  // The shared tables, the region table first. A line of regions.csv is 6
  // bytes or more. The last word of has: every id above the range. The last
  // stretch runs past the end.
  u64 words = (maxreg + 1) / 64 + 1;
  u64 size = NT * (SLOTS * 8 + NAMES * 34) + sizeof(Ring[NT][NT]) +
             sizeof(Agg[NT][256]) + words * 12 + regsize / 6 + 4096;
  shared = big(size + CHARGE);
  has = carve(words * 8), first = carve(words * 4), ctry = carve(regsize / 6);
  rstretch = (carved + CHARGE - 1) / CHARGE;
  for (int t = 0; t < NT; t++)
    names[t] = (Names){.slot = carve(SLOTS * 8), .mask = SLOTS / 4 - 1,
                       .lo = carve(NAMES * 2), .row = carve(NAMES * 32)};
  ring = carve(sizeof(Ring[NT][NT])), agg = carve(sizeof(Agg[NT][256]));
  nstretch = (carved + CHARGE - 1) / CHARGE;
  tables(shared, carved); // their page-table pages
  hot(__bss_start, _end - __bss_start);
  ctx = big(NT * sizeof *ctx);
  u8 *stack = big(NT * STACK);
  pthread_t th[NT];
  pthread_attr_t at;
  pthread_attr_init(&at);
  pthread_barrier_init(&meet, 0, NT);
  for (long t = 1; t < NT; t++) {
    hot(stack + t * STACK + STACK * 3 / 4, STACK / 4); // the top, the part used
    pthread_attr_setstack(&at, stack + t * STACK, STACK);
    if (pthread_create(&th[t], &at, worker, (void *)t))
      die("pthread_create");
  }
  for (int t = 0; t < NT; t++)
    hot(&ctx[t], offsetof(Ctx, ix) + 4 * HOTREC), hot(ctx[t].rec, sizeof(Rec) * HOTREC);
  if (rsize >= 64 * HUGE) // and the zeros after the file
    tables(rec + 64 * HUGE, rsize + 1 - 64 * HUGE);
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
