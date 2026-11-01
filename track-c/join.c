// Speed Demon 2026, Track C (Join): records.csv joined to regions.csv and
// aggregated per country. PERF.md has the measurement behind each choice.
//
// How it works, and why it is fast:
//  1. Twelve threads, one per hardware thread of the scoring box's six
//     P-cores. Thread t owns the records that start in its twelfth of the
//     file. A record starts right after an LF that is followed by a digit or
//     a BOM: inside a quoted note an LF is always followed by a lowercase
//     letter or a quote (spec 2.2), so a thread finds its first record
//     without reading anything before it.
//  2. records.csv is mapped, and each thread parses its part in two passes
//     per 16 KB. Pass 1 is the only one that reads every byte: per 64 bytes,
//     AVX2 compares give bitmasks of quotes, commas and LFs, and a carry-less
//     multiply turns the quote mask into an "inside quotes" mask (every quote
//     flips the state, so an escaped "" flips it twice and changes nothing).
//     The commas and LFs outside quotes are exactly each record's four
//     separators and its terminator; pass 1 writes down their offsets.
//  3. Pass 2 reads each record's fields at those offsets, so records no
//     longer wait on one another. It first looks up every record's region
//     in a loop short enough for the cache misses on the 1 MB region table
//     to overlap, then does the rest.
//  4. Distinct names: a name is identified by 56 bits of a hash of its raw
//     bytes (quoting is one-to-one, and spec 2.2 always quotes a non-empty
//     name). Two of the ~100,000 names share those bits with odds of about
//     1e-7 per run.
//  5. Each name has one owner thread, picked by its hash. The reader sends
//     the owner an 8-byte message (the hash, with the country in the low
//     byte) through a lock-free ring, and the owner keeps its names in a
//     private table with one bit per country. One table shared by all
//     threads took almost twice as long (PERF.md): every lookup missed the
//     core's own cache. The owner's table needs no atomic instruction.
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
typedef uint16_t u16;
typedef uint8_t u8;
enum { NT = 12, CHB = 256, RING = 512 }; // CHB: 64-byte blocks per pass 1

typedef struct { // one country's running totals, kept per thread
  int64_t n, ids, sum, min, max, nb, tb, names;
} Agg;
typedef struct { // a thread's outgoing messages and its totals
  _Alignas(64) u64 line[NT][8]; // each owner's next 64 bytes, fill[o] used
  u64 head[NT]; // each owner's ring head as last read: re-read only when full
  int t;
  u32 fill[NT];
  Agg *agg;
} Ctx;
typedef struct { // a thread's part of records.csv, parsed in two passes
  const u8 *f;        // its first byte; offsets count from here
  u64 end, at, start; // its length, the next block, the next record
  u64 inq;            // all ones if inside quotes at at
  u32 *ix, n;         // separators pass 1 found that no record has used
} Scan;
typedef struct { // one owner's names
  u64 *slot, mask, n; // 4-slot buckets (mask: their number - 1), see find()
  u16 *lo;            // id -> its message's bits 8-23, to refile on grow
  u64 (*row)[4];      // id -> a bit per country
} Names;

static const u8 *rec;                 // records.csv, mapped
static u64 rsize, regsize, maxreg;    // file sizes, the largest region id
static const u8 *reg;                 // regions.csv, mapped
static u8 *rtab;                      // region id -> country, 0 = unmatched
static _Atomic u8 cidx[26 * 26 * 26]; // country code -> country number
static char codes[256][4];            // country number -> code
static int ncountry;
static pthread_mutex_t cmu = PTHREAD_MUTEX_INITIALIZER;
static atomic_int arrived, gen, finished;
typedef struct { // the messages from one thread to one owner, RING at most
  _Alignas(64) _Atomic u64 tail; // advanced by the sender
  _Alignas(64) _Atomic u64 head; // advanced by the owner
  u64 *m;                        // one page; message i is m[i % RING]
} Ring;
static Ring ring[NT][NT]; // [sender][owner]
static Agg agg[NT][256];
static Names names[NT];
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
  madvise(p, n, MADV_NOHUGEPAGE); // a 2 MB page would be zeroed and counted whole
  return p;
}
static inline u64 load(const u8 *p) {
  u64 v;
  memcpy(&v, p, 8);
  return v;
}

static void barrier(void) { // all NT threads meet here
  int g = gen;
  if (atomic_fetch_add(&arrived, 1) == NT - 1) {
    arrived = 0;
    gen = g + 1;
  } else
    while (gen == g)
      _mm_pause();
}

// The first n (1 to 8) digit values of x as a number: they move to the top
// bytes, the zeros below read as leading zeros, and pairs, quads and octets
// combine in three multiplies.
static inline u64 fold(u64 x, u64 n) {
  x <<= 64 - 8 * n;
  x = (x * 10 + (x >> 8)) & 0x00FF00FF00FF00FF;
  x = (x * 100 + (x >> 16)) & 0x0000FFFF0000FFFF;
  return (x * 10000 + (x >> 32)) & 0xFFFFFFFF;
}

static inline u64 bits(__m256i lo, __m256i hi) { // byte masks -> 64 bits
  return (u32)_mm256_movemask_epi8(lo) |
         (u64)(u32)_mm256_movemask_epi8(hi) << 32;
}
static inline u64 quotes(const u8 *p) { // a bit per quote byte of p[0, 64)
  __m256i q = _mm256_set1_epi8('"');
  return bits(_mm256_cmpeq_epi8(_mm256_loadu_si256((const void *)p), q),
              _mm256_cmpeq_epi8(_mm256_loadu_si256((const void *)(p + 32)), q));
}

// Name hashing: 32 bytes v at a time go into the 128-bit state h, the first
// 16 by XOR and the last 16 as the key of one AES round; q counts the quotes.
typedef struct {
  const u8 *p;
  u64 n, q;
  __m128i h;
} Name;
static inline Name step(Name s, __m256i v) {
  s.q += _mm_popcnt_u32(
      _mm256_movemask_epi8(_mm256_cmpeq_epi8(v, _mm256_set1_epi8('"'))));
  s.h = _mm_aesenc_si128(_mm_xor_si128(s.h, _mm256_castsi256_si128(v)),
                         _mm256_extracti128_si256(v, 1));
  return s;
}
// All but the last 32 bytes of a long name (spec 2.2 sets no bound; the
// published data has none over 32 bytes, so this stays out of the loop).
__attribute__((cold, noinline)) static Name head(Name s) {
  for (; s.n > 32; s.n -= 32, s.p += 32)
    s = step(s, _mm256_loadu_si256((const void *)s.p));
  return s;
}
// The hash of the name bytes p[0, n), and in *len their length once
// unescaped: every quote inside a name is half of a "". After two more AES
// rounds every name byte has reached all 128 bits of the state. One round is
// a bijection, so names of up to 16 bytes cannot collide before the state is
// cut to 64 bits.
static inline u64 name(const u8 *p, u64 n, u64 *len) {
  Name s = {p, n, 0, _mm_set_epi64x(0x8EBC6AF09C88C6E3, 0xA0761D6478BD642F)};
  if (n > 32)
    s = head(s);
  s = step(s, _mm256_and_si256(
                  _mm256_loadu_si256((const void *)s.p),
                  _mm256_loadu_si256((const void *)(KEEP + 32 - s.n))));
  *len = n - s.q / 2;
  s.h = _mm_aesenc_si128(s.h, _mm_set_epi64x(0x589965CC75374CC3, 0xE7037ED1A0B428DB));
  s.h = _mm_aesenc_si128(s.h, _mm_set_epi64x(0x1D8E4E27C47D124F, 0x9E3779B97F4A7C15));
  return _mm_cvtsi128_si64(s.h);
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

// Doubles the table and refiles every name; ids and rows stay. lo[id] keeps
// the message bits 8-19 that the slot lacks.
static void grow(Names *m) {
  u64 *old = m->slot, n = old ? 4 * (m->mask + 1) : 0, size = old ? 2 * n : 1 << 14;
  m->slot = aligned_alloc(32, size * 8);
  m->lo = realloc(m->lo, size * 5 / 8 * sizeof *m->lo);
  m->row = realloc(m->row, size * 5 / 8 * sizeof *m->row);
  if (!m->slot || !m->lo || !m->row)
    die("malloc");
  memset(m->slot, 0, size * 8);
  m->mask = size / 4 - 1;
  for (u64 i = 0; i < n; i++)
    if (old[i])
      *find(m->slot, m->mask, old[i] >> 20 << 20 | (u64)(m->lo[(old[i] & 0xFFFFF) - 1] & 0xFFF) << 8) = old[i];
  free(old);
}

// Files the name of message msg, seen for the first time; returns its slot.
__attribute__((cold)) static u64 add(Names *m, u64 msg) {
  if (8 * m->n >= 5 * 4 * (m->mask + 1))
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
      memcpy(codes[c = cidx[k] = ++ncountry], p, 3);
    pthread_mutex_unlock(&cmu);
  }
  return c;
}

static void regions(int t) { // the lines that start in this thread's twelfth
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

static u64 record_start(u64 q) { // the first record starting at or after q
  for (q = q ? q : 1; q < rsize; q++)
    if (rec[q - 1] == '\n' && (rec[q] - '0' < 10u || rec[q] == 0xEF))
      return q;
  return rsize;
}

// Pass 1 over the blocks of z's part up to offset to: the offset of every
// comma and LF outside quotes goes to ix.
static void separators(Scan *z, u64 to) {
  // Locals, not z's fields: the stores to ix could alias those.
  u64 at = z->at, inq = z->inq, end = z->end, n = z->n;
  u32 *ix = z->ix;
  __m256i comma = _mm256_set1_epi8(','), lf = _mm256_set1_epi8('\n');
  for (; at < to; at += 64) {
    const u8 *p = z->f + at;
    __m256i lo = _mm256_loadu_si256((const void *)p),
            hi = _mm256_loadu_si256((const void *)(p + 32));
    u64 s = bits(_mm256_or_si256(_mm256_cmpeq_epi8(lo, comma),
                                 _mm256_cmpeq_epi8(lo, lf)),
                 _mm256_or_si256(_mm256_cmpeq_epi8(hi, comma),
                                 _mm256_cmpeq_epi8(hi, lf)));
    u64 in = _mm_cvtsi128_si64(_mm_clmulepi64_si128(
                 _mm_set_epi64x(0, quotes(p)), _mm_set1_epi8(-1), 0)) ^ inq;
    inq = (u64)((int64_t)in >> 63);
    s &= ~in;
    if (end - at < 64) // the part's last block: the rest is the next part's
      s = _bzhi_u64(s, end - at);
    u64 k = n;
    n += _mm_popcnt_u64(s);
    do // eight at a time, without a test per bit: a block holds about five
      for (int i = 0; i < 8; i++, k++, s = _blsr_u64(s))
        ix[k] = at + _tzcnt_u64(s);
    while (s);
  }
  z->at = at, z->inq = inq, z->n = n;
}

// Files every message that has reached owner t, 64 at a time: first each
// name's id (its slot and row mostly miss the cache, and these lookups
// overlap), then each country bit. Returns the number of messages.
static u64 drain(int t, Agg *a) {
  Names *nm = &names[t];
  u64 done = 0;
  for (int from = 0; from < NT; from++) {
    Ring *r = &ring[from][t];
    u64 *m = r->m, h = r->head, k = atomic_load_explicit(&r->tail, memory_order_acquire);
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

// Sends x's partial line for owner o, waiting (and filing its own messages
// meanwhile, so two full rings never wait on each other) while o's ring is full.
static void post(Ctx *x, int o) {
  Ring *r = &ring[x->t][o];
  u64 t = r->tail;
  while (t + x->fill[o] - x->head[o] > RING &&
         t + x->fill[o] - (x->head[o] = atomic_load_explicit(&r->head, memory_order_acquire)) > RING)
    if (!drain(x->t, x->agg))
      _mm_pause();
  memcpy(r->m + t % RING, x->line[o], 8 * x->fill[o]); // lines never wrap
  atomic_store_explicit(&r->tail, t + x->fill[o], memory_order_release);
  x->fill[o] = 0;
}

// Pass 2: every record whose five separators pass 1 has found.
static void records(Ctx *x, Scan *z) {
  // Locals, not globals or z's fields: the stores below could alias those.
  const u8 *f = z->f, *rt = rtab;
  const u32 *ix = z->ix;
  u64 n = z->n / 5 * 5, start = z->start, top = maxreg + 1; // top: unmatched
  u8 cc[64 * CHB / 5 + 2]; // each record's country
  for (u64 j = 0, r = 0; j < n; j += 5, r++) {
    u64 c = ix[j + 2], d = ix[j + 3];
    u64 rid = fold(load(f + c + 1) ^ 0x3030303030303030, d - c - 1);
    cc[r] = rt[rid < top ? rid : top];
  }
  Agg *ag = x->agg;
  for (u64 j = 0, r = 0; j < n; j += 5, r++) {
    const u8 *p = f + start;
    p += *p == 0xEF ? 3 : 0; // a BOM at a record start is not data
    u64 a = ix[j], b = ix[j + 1], c = ix[j + 2], d = ix[j + 3], e = ix[j + 4];
    u64 id = fold(load(p) ^ 0x3030303030303030, f + a - p);
    u64 neg = f[d + 1] == '-', il = e - (f[e - 1] == '\r') - d - neg - 4;
    u64 v = load(f + d + 1 + neg) ^ 0x3030303030303030;
    u64 lo = _bzhi_u64(~0ull, 8 * il); // cut out the point's byte
    int64_t cents = fold((v & lo) | (v >> 8 & ~lo), il + 2);
    cents = (cents ^ -neg) + neg;
    start = e + 1;
    Agg *g = ag + cc[r];
    g->n++;
    g->ids += id;
    g->sum += cents;
    g->min = cents < g->min ? cents : g->min;
    g->max = cents > g->max ? cents : g->max;
    u64 nl, h = name(f + a + 2, b - a > 3 ? b - a - 3 : 0, &nl); // "" and empty: one
    g->nb += nl;
    g->tb += text(f + b + 1, c - b - 1);
    u64 o = (h >> 32) * NT >> 32; // the name's owner
    x->line[o][x->fill[o]] = (h & ~255ull) | cc[r];
    if (++x->fill[o] == 8) // a full line: one 64-byte write
      post(x, o);
  }
  z->start = start, z->n -= n;
  memmove(z->ix, ix + n, z->n * 4); // a record cut by the pass's end
}

static void *worker(void *arg) {
  int t = (int)(intptr_t)arg;
  static Ctx ctx[NT];
  Ctx *x = &ctx[t];
  x->agg = agg[t], x->t = t;
  regions(t);
  barrier(); // the region table is complete
  grow(&names[t]);
  u64 lo = record_start(rsize * t / NT);
  u64 hi = t + 1 < NT ? record_start(rsize * (t + 1) / NT) : rsize;
  Scan z = {.f = rec + lo, .end = hi - lo};
  z.ix = big(4 * 64 * (CHB + 2));
  for (int k = 1; z.at < z.end; k++) {
    separators(&z, z.at + 64 * CHB < z.end ? z.at + 64 * CHB : z.end);
    records(x, &z);
    if (k % 4 == 0)
      drain(t, x->agg);
  }
  for (int o = 0; o < NT; o++) // the partial lines
    post(x, o);
  atomic_fetch_add(&finished, 1); // after every send, so the last drain sees all
  while (finished < NT)
    if (!drain(t, x->agg))
      _mm_pause();
  drain(t, x->agg);
  // Every record is parsed: drop this thread's twelfth of the mapping, or
  // exit tears down all 49,000 page-table entries on one CPU.
  u64 a = rsize * t / NT & ~4095ull, e = rsize * (t + 1) / NT & ~4095ull;
  madvise((void *)(rec + a), e - a, MADV_DONTNEED);
  return 0;
}

static int bycode(const void *a, const void *b) {
  return strcmp(codes[*(const int *)a], codes[*(const int *)b]);
}
static char *money(char *o, int64_t v) { // cents as -?D+.DD
  u64 a = v < 0 ? -v : v;
  return o + sprintf(o, " %s%llu.%02llu", v < 0 ? "-" : "",
                     (unsigned long long)(a / 100),
                     (unsigned long long)(a % 100));
}

int main(void) {
  int rfd = open("/data/records.csv", O_RDONLY);
  int gfd = open("/data/regions.csv", O_RDONLY);
  if (rfd < 0 || gfd < 0)
    die("open /data");
  rsize = lseek(rfd, 0, SEEK_END);
  regsize = lseek(gfd, 0, SEEK_END);
  // records.csv is mapped over the start of a zeroed reservation one page
  // longer, so the vector loads that run past its end read zeros.
  u8 *f = big(rsize + 4096);
  if (mmap(f, rsize, PROT_READ, MAP_PRIVATE | MAP_FIXED, rfd, 0) != f)
    die("mmap records.csv");
  rec = f;
  reg = mmap(0, regsize, PROT_READ, MAP_PRIVATE, gfd, 0);
  if (reg == MAP_FAILED)
    die("mmap regions.csv");
  u64 p = regsize - 1; // ids ascend (spec 2.3): the last line has the largest
  while (reg[p - 1] != '\n')
    p--;
  maxreg = strtoull((const char *)reg + p, 0, 10);
  rtab = big(maxreg + 2); // the last entry: every id above the range
  u64 *m = big(8 * RING * NT * NT); // a page per ring
  for (int i = 0; i < NT * NT; i++)
    (&ring[0][0])[i].m = m + RING * i;
  for (int t = 0; t < NT; t++)
    for (int c = 0; c < 256; c++)
      agg[t][c] = (Agg){.min = INT64_MAX, .max = INT64_MIN};
  pthread_t th[NT];
  for (long t = 1; t < NT; t++)
    if (pthread_create(&th[t], 0, worker, (void *)t))
      die("pthread_create");
  worker(0);
  for (int t = 1; t < NT; t++)
    pthread_join(th[t], 0);

  Agg tot[256];
  int order[256], n = 0;
  for (int c = 0; c <= ncountry; c++) {
    tot[c] = agg[0][c];
    for (int t = 1; t < NT; t++) {
      Agg *a = &agg[t][c], *g = &tot[c];
      g->n += a->n, g->ids += a->ids, g->sum += a->sum, g->nb += a->nb;
      g->tb += a->tb, g->names += a->names;
      g->min = a->min < g->min ? a->min : g->min;
      g->max = a->max > g->max ? a->max : g->max;
    }
    if (c && tot[c].n)
      order[n++] = c;
  }
  qsort(order, n, sizeof *order, bycode);
  if (tot[0].n) // "_" sorts after every code, so unmatched is last
    order[n++] = 0;
  static char out[1 << 16];
  char *o = out;
  for (int i = 0; i < n; i++) {
    Agg *g = &tot[order[i]];
    o += sprintf(o, "%s %lld %lld", order[i] ? codes[order[i]] : "_UNMATCHED",
                 (long long)g->n, (long long)g->ids);
    o = money(money(money(o, g->sum), g->min), g->max);
    o += sprintf(o, " %lld %lld %lld\n", (long long)g->names, (long long)g->nb,
                 (long long)g->tb);
  }
  int fd = open("/out/aggregates.txt", O_WRONLY | O_CREAT | O_TRUNC, 0644);
  if (fd < 0 || write(fd, out, o - out) != o - out)
    die("write /out/aggregates.txt");
  return 0;
}
