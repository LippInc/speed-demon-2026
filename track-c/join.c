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
//  2. Each thread pread()s its range in 64 KB blocks into a buffer that stays
//     in its core's L2. On the box that reads the whole file in 5.6 ms where
//     mmap() takes 7.6 ms: faulting in and tearing down 49,000 page-table
//     entries costs more than copying the bytes.
//  3. Per 64-byte block, AVX2 compares give bitmasks of quotes, commas and
//     LFs. A carry-less multiply turns the quote mask into an "inside quotes"
//     mask (every quote flips the state, so an escaped "" flips it twice and
//     changes nothing), and the commas and LFs outside quotes are exactly the
//     four separators and the terminator of each record.
//  4. A quoted field of raw length L holding q quote bytes is L - 1 - q/2
//     bytes long once unescaped (two delimiters, and each "" counts once), so
//     name_bytes and note_bytes never need the text unescaped.
//  5. Distinct names: a name is identified by 56 bits of a hash of its raw
//     bytes (quoting is one-to-one, and spec 2.2 always quotes a non-empty
//     name). Two of the ~100,000 names share those bits with odds of about
//     7e-8 per run.
//  6. Each name has one owner thread, picked by its hash. The reader sends
//     the owner an 8-byte message (the hash, with the country in the low
//     byte), and the owner keeps its names in a private table with one bit
//     per country. A shared table cost 18 ms more: every lookup missed the
//     core's own cache. The owner's table fits in its L2 and needs no atomic
//     instruction. Owners apply round r's messages while everyone parses
//     round r + 1, one barrier per round.
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
  u64 inq, qb;        // all ones if inside quotes at at, quotes before at
  u32 *ix, *qc, n;    // separators pass 1 found that no record has used
} Scan;
typedef struct { // one owner's names
  u64 *slot, mask, n; // 8-slot buckets; slot: message bits 24-63 | id + 1
  u64 *key;           // id -> its message's bits 8-63, to refile on grow
  u64 (*row)[4];      // id -> a bit per country
} Names;

static const u8 *rec;                 // records.csv, mapped, then a guard
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

static u64 hash(const u8 *p, u64 n) {
  u64 h = n * 0x9E3779B97F4A7C15 + 1;
  for (; n > 8; n -= 8, p += 8)
    h = (h ^ load(p)) * 0xBF58476D1CE4E5B9;
  h = (h ^ _bzhi_u64(load(p), n * 8)) * 0x94D049BB133111EB;
  h = (h ^ h >> 31) * 0xBF58476D1CE4E5B9;
  return h ^ h >> 32;
}

// A bit per slot of v (4 of them) whose bits under keep equal t.
static inline u32 eq4(__m256i v, u64 keep, u64 t) {
  return _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(
      _mm256_and_si256(v, _mm256_set1_epi64x(keep)), _mm256_set1_epi64x(t))));
}

// The slot of key k (message bits 8-63), or the free slot where it goes. A
// bucket is a cache line of 8 slots, compared at once. A key's walk starts at
// the bucket its low bits pick and goes on past full ones; slots fill in order
// and never empty, so a key that is there comes before the walk's first free
// slot. Tags are message bits 24-63: two of ~100,000 names pass for one with
// odds of about 1e-6 per run.
static u64 *find(u64 *slot, u64 mask, u64 k) {
  for (u64 b = k & mask;; b = (b + 1) & mask) {
    u64 *s = slot + 8 * b, tag = k >> 16 << 24, keep = ~0ull << 24;
    __m256i lo = _mm256_load_si256((__m256i *)s),
            hi = _mm256_load_si256((__m256i *)s + 1);
    u32 any = eq4(lo, keep, tag) | eq4(hi, keep, tag) << 4 | eq4(lo, ~0ull, 0) |
              eq4(hi, ~0ull, 0) << 4;
    if (any)
      return s + _tzcnt_u32(any);
  }
}

// Doubles the buckets and refiles every name; ids and rows stay.
static void grow(Names *m) {
  u64 *old = m->slot, slots = old ? 16 * (m->mask + 1) : 1 << 14;
  m->slot = aligned_alloc(64, slots * 8);
  m->key = realloc(m->key, slots * 6); // ids reach 3/4 of the slots
  m->row = realloc(m->row, slots * 24);
  if (!m->slot || !m->key || !m->row)
    die("malloc");
  memset(m->slot, 0, slots * 8);
  m->mask = slots / 8 - 1;
  for (u64 id = 0; id < m->n; id++)
    *find(m->slot, m->mask, m->key[id]) = m->key[id] >> 16 << 24 | (id + 1);
  free(old);
}

// Files the name of message msg, seen for the first time, and returns its id.
__attribute__((cold)) static u64 add(Names *m, u64 msg) {
  if (4 * m->n >= 3 * 8 * (m->mask + 1))
    grow(m);
  memset(m->row[m->n], 0, 32); // rows are touched only once used
  m->key[m->n] = msg >> 8;
  *find(m->slot, m->mask, msg >> 8) = msg >> 24 << 24 | (m->n + 1);
  return m->n++;
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

static inline u64 eq(const u8 *p, char c) { // a bit per byte of p[0, 64) == c
  __m256i v = _mm256_set1_epi8(c);
  return (u32)_mm256_movemask_epi8(
             _mm256_cmpeq_epi8(_mm256_loadu_si256((const void *)p), v)) |
         (u64)(u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(
             _mm256_loadu_si256((const void *)(p + 32)), v))
             << 32;
}

// Pass 1 over the blocks of z's part up to offset to: the offset of every
// comma and LF outside quotes (a record's four separators and its end) goes to
// ix, and the count of quote bytes before it to qc. A carry-less multiply
// turns the quote bits into "inside quotes" bits: every quote flips the state,
// so an escaped "" flips it twice and changes nothing.
static void separators(Scan *z, u64 to) {
  // Locals, not z's fields: the stores to ix and qc could alias those.
  u64 at = z->at, inq = z->inq, qb = z->qb, end = z->end, n = z->n;
  u32 *ix = z->ix, *qc = z->qc;
  for (; at < to; at += 64) {
    const u8 *p = z->f + at;
    u64 q = eq(p, '"'), s = eq(p, ',') | eq(p, '\n');
    u64 in = _mm_cvtsi128_si64(_mm_clmulepi64_si128(
                 _mm_set_epi64x(0, q), _mm_set1_epi8(-1), 0)) ^ inq;
    inq = (u64)((int64_t)in >> 63);
    s &= ~in & _bzhi_u64(~0ull, end - at < 64 ? end - at : 64);
    u64 k = n;
    n += _mm_popcnt_u64(s);
    do // eight at a time, without a test per bit: a block holds about five
      for (int i = 0; i < 8; i++, k++, s = _blsr_u64(s)) {
        u64 o = _tzcnt_u64(s);
        ix[k] = at + o, qc[k] = qb + _mm_popcnt_u64(_bzhi_u64(q, o));
      }
    while (s);
    qb += _mm_popcnt_u64(q);
  }
  z->at = at, z->inq = inq, z->qb = qb, z->n = n;
}

// A text field between separators a and b holding q quote bytes: raw length
// b - a - 1, less its two delimiters and one byte per "" (empty unquoted
// fields hold no quotes).
static inline u64 tlen(u64 a, u64 b, u64 q) { return b - a - 1 - q / 2 - (q > 0); }

// Files every message that has reached owner t. A name's slot and its bit row
// mostly miss the cache, so each message is looked up 8 messages before it is
// applied, and its slot fetched 8 before that.
static void drain(int t, Agg *a) {
  Names *n = &names[t]; // its table in locals: the stores below could alias it
  u64 *slot = n->slot, mask = n->mask, ids[8], (*row)[4] = n->row;
  for (int from = 0; from < NT; from++) {
    Ring *r = &ring[from][t];
    u64 *m = r->m, h = r->head, k = atomic_load_explicit(&r->tail, memory_order_acquire);
    if (h == k)
      continue;
    for (u64 j = h - 8; j != k; j++) { // unsigned: j + 8 runs from h
      if (j + 8 >= h + 8) { // apply message j: country m[j] & 255 has the name
        u64 c = m[j % RING] & 255, bit = 1ull << (c & 63);
        u64 *w = &row[ids[j & 7]][c >> 6];
        a[c].names += !(*w & bit);
        *w |= bit;
      }
      if (j + 8 < k) { // look up message j + 8, fetch its row
        u64 msg = m[(j + 8) % RING], e = *find(slot, mask, msg >> 8);
        if (e)
          ids[j & 7] = (e & 0xFFFFFF) - 1;
        else
          ids[j & 7] = add(n, msg), slot = n->slot, mask = n->mask, row = n->row;
        _mm_prefetch((const char *)row[ids[j & 7]], _MM_HINT_T0);
        if (j + 16 < k)
          _mm_prefetch((const char *)&slot[8 * (m[(j + 16) % RING] >> 8 & mask)], _MM_HINT_T0);
      }
    }
    atomic_store_explicit(&r->head, k, memory_order_release);
  }
}

// Sends x's partial line for owner o, waiting (and filing its own messages
// meanwhile, so two full rings never wait on each other) while o's ring is full.
static void post(Ctx *x, int o) {
  Ring *r = &ring[x->t][o];
  u64 t = r->tail;
  while (t + x->fill[o] - x->head[o] > RING &&
         t + x->fill[o] - (x->head[o] = atomic_load_explicit(&r->head, memory_order_acquire)) > RING)
    drain(x->t, x->agg);
  memcpy(r->m + t % RING, x->line[o], 8 * x->fill[o]); // lines never wrap
  atomic_store_explicit(&r->tail, t + x->fill[o], memory_order_release);
  x->fill[o] = 0;
}

// Pass 2: every record whose five separators pass 1 has found. Each record's
// fields sit at known offsets, so records no longer wait on one another.
static void records(Ctx *x, Scan *z) {
  // Locals, not globals or z's fields: the stores below could alias those.
  const u8 *f = z->f, *rt = rtab;
  const u32 *ix = z->ix, *qc = z->qc;
  u64 j = 0, n = z->n, start = z->start, top = maxreg + 1; // top: unmatched
  Agg *ag = x->agg;
  for (; j + 5 <= n; j += 5) {
    if (j + 45 <= n) { // the region table (1 MB) mostly misses: fetch the
      u64 c = ix[j + 42], d = ix[j + 43]; // entry 8 records ahead
      u64 r = fold(load(f + c + 1) ^ 0x3030303030303030, d - c - 1);
      _mm_prefetch((const char *)rt + (r < top ? r : top), _MM_HINT_T0);
    }
    const u8 *p = f + start;
    p += *p == 0xEF ? 3 : 0; // a BOM at a record start is not data
    u64 a = ix[j], b = ix[j + 1], c = ix[j + 2], d = ix[j + 3], e = ix[j + 4];
    u64 id = fold(load(p) ^ 0x3030303030303030, f + a - p);
    u64 rid = fold(load(f + c + 1) ^ 0x3030303030303030, d - c - 1);
    u64 neg = f[d + 1] == '-', il = e - (f[e - 1] == '\r') - d - neg - 4;
    u64 v = load(f + d + 1 + neg) ^ 0x3030303030303030;
    u64 lo = _bzhi_u64(~0ull, 8 * il); // cut out the point's byte
    int64_t cents = fold((v & lo) | (v >> 8 & ~lo), il + 2);
    start = e + 1;
    Agg *g = ag + rt[rid < top ? rid : top];
    cents = (cents ^ -neg) + neg;
    g->n++;
    g->ids += id;
    g->sum += cents;
    g->min = cents < g->min ? cents : g->min;
    g->max = cents > g->max ? cents : g->max;
    g->nb += tlen(a, b, qc[j + 1] - qc[j]);
    g->tb += tlen(b, c, qc[j + 2] - qc[j + 1]);
    u64 h = hash(f + a + 2, b - a > 3 ? b - a - 3 : 0); // "" and empty: one
    u64 o = (h >> 32) * NT >> 32;                        // the name's owner
    x->line[o][x->fill[o]] = (h & ~255ull) | (g - ag);
    if (++x->fill[o] == 8) // a full line: one 64-byte write
      post(x, o);
  }
  z->start = start, z->n = n - j;
  memmove(z->ix, ix + j, z->n * 4); // a record cut by the pass's end
  memmove(z->qc, qc + j, z->n * 4);
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
  z.ix = big(4 * 64 * (CHB + 2)), z.qc = big(4 * 64 * (CHB + 2));
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
    drain(t, x->agg);
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
  // records.csv is mapped over the start of a reservation one page longer, so
  // the bytes after it can hold the guard parse() needs.
  u8 *f = big(rsize + 4096);
  if (rsize && mmap(f, rsize, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_FIXED,
                    rfd, 0) != f)
    die("mmap records.csv");
  for (int i = 0; i < 96; i++)
    f[rsize + i] = "\",\n"[i % 3];
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
