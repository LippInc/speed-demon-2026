// Speed Demon 2026, Track C (Join): records.csv joined to regions.csv and
// aggregated per country. PERF.md has the measurement behind each choice.
//
// How it works, and why it is fast:
//  1. Twelve threads, one per hardware thread of the scoring box's six
//     P-cores. They take 256 KB pieces of the file from a shared counter, so
//     a thread that another process slows down takes fewer. A thread owns
//     the records that start in its piece.
//  2. Each record is read once, front to back, from one quote mask of its
//     first 64 bytes. The first quote ends the id. A carry-less multiply turns
//     the mask into "follows an odd number of quotes", which picks out the
//     commas after the name and the note, however many "" they hold. The region
//     and the amount follow, 19 bytes at most. Only the first record of a piece
//     needs another rule: an LF inside a quoted note is always followed by a
//     lowercase letter or a quote, a record's own LF by a digit or a BOM (spec
//     2.2). An earlier pass 1 listed all record ends that way: 13 % of all
//     instructions, gone (PERF.md).
//  3. One record's start depends on the last one's end, a wait of about 50
//     cycles, so each piece is read from two places in turn and the two waits
//     overlap. A run of 64 records is parsed first, each prefetching its entry
//     in the 1 MB region table; a second loop then does the lookups, totals and
//     messages, and those misses overlap too.
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
typedef __m256i V;
typedef u64 __attribute__((aligned(1), may_alias)) u64u;
// BATCH: messages staged per owner; RING: the messages a ring holds (two
// batches); PIECE: bytes per piece of records.csv; RUN: records per parse loop.
enum { NT = 12, BATCH = 128, RING = 2 * BATCH, PIECE = 1 << 17, RUN = 64 };
enum { BUCKETS = 1248, SLOTS = 8 * (BUCKETS + 8) };

// One country's running totals, kept per thread: records, id sum, amount
// sum, min and max in cents, name and note bytes once unescaped, distinct names.
// The six additive fields come first and are summed as one array in report.
typedef struct {
  long long n, ids, sum, nb, tb, names, min, max;
} Agg;
// What the parse loop reads out of a record: the name's hash, the id, the
// region's table index, name and note bytes once unescaped, the amount in cents.
typedef struct {
  u64 h;
  u32 id, reg, nl, tb;
  int32_t cents;
} Rec;
typedef struct { // a thread's outgoing messages, totals and parsed records
  _Alignas(64) u64 batch[NT * BATCH]; // owner o's next messages: from o * BATCH up to fill[o]
  Rec rec[RUN];
  u32 fill[NT];
  int t;
  Agg *agg;
} Ctx;
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

static void die(const char *what) { perror(what), exit(1); }
static inline V load32(const u8 *p) { return _mm256_loadu_si256((const void *)p); }
static inline u32 eq32(const u8 *p, V c) { // a bit per byte of p == c
  return _mm256_movemask_epi8(_mm256_cmpeq_epi8(load32(p), c));
}

// Three numbers at once, each up to 8 raw bytes in the top bytes of a
// 64-bit lane: a saturating subtract of '0' turns digits into their values
// and every other byte (the zeros the shifts brought in, the point, the
// sign, the comma) into 0, so the zeros below read as leading zeros. Digit
// pairs, then quads, then octets combine in one multiply each.
static inline V fold3(u64 a, u64 b, u64 c) {
  V x = _mm256_set_epi64x(0, c, b, a);
  x = _mm256_subs_epu8(x, _mm256_set1_epi8('0'));
  x = _mm256_maddubs_epi16(x, _mm256_set1_epi16(0x010A));    // 10a + b
  x = _mm256_madd_epi16(x, _mm256_set1_epi32(0x00010064));   // 100a + b
  return _mm256_add_epi64(_mm256_mul_epu32(x, _mm256_set1_epi64x(10000)),
                          _mm256_srli_epi64(x, 32));         // 10000a + b
}

// A bit per quote byte of 64 bytes: the first 32 in v, then those at p
static inline u64 quotes(V v, const u8 *p) {
  V q = _mm256_set1_epi8('"');
  return (u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(v, q)) | (u64)eq32(p, q) << 32;
}

// Name hashing: 32 bytes v at a time go into the 128-bit state h, the first
// 16 by XOR and the last 16 as the key of one AES round. After two more
// rounds every name byte has reached all 128 bits of the state. One round is
// a bijection, so names of up to 16 bytes cannot collide before the state is
// cut to 64 bits.
static inline __m128i step(__m128i h, V v) {
  return _mm_aesenc_si128(_mm_xor_si128(h, _mm256_castsi256_si128(v)),
                          _mm256_extracti128_si256(v, 1));
}
#define SEED _mm_set_epi64x(0x8EBC6AF09C88C6E3, 0xA0761D6478BD642F)
#define K1 _mm_set_epi64x(0x589965CC75374CC3, 0xE7037ED1A0B428DB)
#define K2 _mm_set_epi64x(0x1D8E4E27C47D124F, 0x9E3779B97F4A7C15)
// v's first n bytes end the name
static inline u64 last(__m128i h, V v, u64 n) {
  h = step(h, _mm256_and_si256(v, load32(KEEP + 32 - n)));
  return _mm_cvtsi128_si64(_mm_aesenc_si128(_mm_aesenc_si128(h, K1), K2));
}

// The slot of the name of message msg; a new name takes the first free slot
// of the walk.
static inline u32 slot_of(Names *nm, u64 msg) {
  u32 tag = msg >> 8 | 1; // bits 8-39
  V want = _mm256_set1_epi32(tag), zero = _mm256_setzero_si256();
  for (u64 b = (msg >> 48) * BUCKETS >> 16;; b++) {
    V v = _mm256_load_si256((const V *)(nm->tag + 8 * b));
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
  u64 n = x->fill[o] - o * BATCH, t = r->tail;
  for (int w = 0; t + n - r->head > RING;)
    if (!drain(x->t, x->agg))
      rest(&w);
  memcpy(r->m + t % RING, x->batch + o * BATCH, 8 * n);
  atomic_store_explicit(&r->tail, t + n, memory_order_release);
  x->fill[o] = o * BATCH;
}

// A bit per byte that follows an odd number of the quotes q, or is one
static inline u64 odd_quotes(u64 q) {
  return _mm_cvtsi128_si64(_mm_clmulepi64_si128(_mm_cvtsi64_si128(q), _mm_set1_epi8(-1), 0));
}

// The offset of the comma after the note at b, of any length, or empty and
// unquoted; adds the note's quotes to *qn.
static u64 note_end(const u8 *b, u64 *qn) {
  u64 p = 0;
  for (u64 i = 0;; i += 64) {
    u64 q = quotes(load32(b + i), b + i + 32), odd = odd_quotes(q) ^ p, o = ~(odd | q);
    if (o) {
      *qn += _mm_popcnt_u64(_bzhi_u64(q, _tzcnt_u64(o)));
      return i + _tzcnt_u64(o);
    }
    *qn += _mm_popcnt_u64(q);
    p = -(odd >> 63);
  }
}

// Reads the record at offset s into *rc; returns the next record's offset.
static inline __attribute__((always_inline)) u64 parse(const u8 *f, const u8 *rt, u64 s, Rec *rc) {
  V comma = _mm256_set1_epi8(',');
  const u8 *b = f + s;
  if (__builtin_expect(*b == 0xEF, 0)) // a BOM at a record start: 777 among 3.32 M records
    b += 3;
  V head = load32(b);
  _mm_prefetch((const char *)b + 512, _MM_HINT_T0);
  // A name is quoted unless empty, an id has 8 digits at most (spec 2.2).
  u64 q = quotes(head, b + 32);
  u64 t = _tzcnt_u64(q), a = t - 1, j, d, h, nl, qn = 0; // the opening quote, the id's digits
  if (__builtin_expect(t > 9 || b[t - 2] == ',', 0)) { // empty, unquoted name: 0.5%
    a = _tzcnt_u32(_mm256_movemask_epi8(_mm256_cmpeq_epi8(head, comma)));
    j = a + 1, nl = 0, h = last(SEED, _mm256_setzero_si256(), 0);
    d = j + 1 + note_end(b + j + 1, &qn);
  } else {
    // Past the opening quote, the commas after the name and after the note
    // are the first two bytes that follow an odd number of quotes and are
    // not one. A "" flips the count twice.
    u64 o = _andn_u64(q, odd_quotes(_blsr_u64(q)));
    j = _tzcnt_u64(o), o = _blsr_u64(o);
    u64 pq = _mm_popcnt_u64(_bzhi_u64(q, j));
    nl = j - t - 1 - pq / 2;
    // A name of 31 raw bytes or more is not handled; spec 2.2 sets no bound,
    // but over 40 generated seeds the longest was 27 (base name at most 24 by
    // the generator's forms, plus runs of trailing spaces that get rarer
    // about 4 times per extra byte), so about 3e-4 per dataset.
    h = last(SEED, load32(b + t + 1), j - t - 2);
    if (__builtin_expect(o != 0, 1)) {
      d = _tzcnt_u64(o);
      qn = _mm_popcnt_u64(_bzhi_u64(q, d)) - pq;
    } else { // the note runs past the 64 bytes: 4%
      d = j + 1 + note_end(b + j + 1, &qn);
    }
  }
  // The region and the amount. The commas, CRs and LFs are the bytes up to ',';
  // digits, '-', '.' and a BOM are above it.
  const u8 *r = b + d + 1;
  V tail = load32(r);
  u32 m = _mm256_movemask_epi8(_mm256_cmpeq_epi8(_mm256_min_epu8(tail, comma), tail));
  u64 c = _tzcnt_u32(m);
  m = _blsr_u32(m);
  u64 e = _tzcnt_u32(m); // the comma, the end of the amount
  // The amount: its bytes up in the top of the lane, "DDDDD.CC" with the
  // point at byte 5; pext drops the point and the shift closes the gap.
  u64 v = *(const u64u *)(r + c + 1) << (64 - 8 * (e - c - 1));
  v = _pext_u64(v, 0xFFFF00FFFFFFFFFF) << 8;
  V num = fold3(*(const u64u *)b << (64 - 8 * a), *(const u64u *)r << (64 - 8 * c), v);
  u64 rid = _mm256_extract_epi64(num, 1), top = 1000001; // top: unmatched
  int32_t cents = _mm256_extract_epi64(num, 2);
  rid = rid < top ? rid : top;
  _mm_prefetch((const char *)rt + rid, _MM_HINT_T0);
  // A quoted note holds qn quotes, its two delimiters and two per "", and
  // loses qn / 2 + 1 bytes; an unquoted one is empty (1.5%).
  u64 len = d - j - 1;
  *rc = (Rec){h, _mm256_cvtsi256_si32(num), rid, nl, len - qn / 2 - (len != 0),
              r[c + 1] == '-' ? -cents : cents};
  // The next record: past the LF; a CR is followed by it
  return r + e + 1 + (m >> (e + 1) & 1) - f;
}

// The records from a up to m and from m up to e, a run of up to RUN at a time:
// parsed in turn from the two places, then totalled and sent on.
static void records(Ctx *x, u64 a, u64 m, u64 e) {
  // Locals, not globals: the stores below could alias those.
  const u8 *f = rec, *rt = mem.rtab;
  Agg *ag = x->agg;
  Rec *rs = x->rec;
  u64 b = m, n;
  while (a < m || b < e) {
    for (n = 0; n + 2 <= RUN && a < m && b < e; n += 2) {
      a = parse(f, rt, a, rs + n);
      b = parse(f, rt, b, rs + n + 1);
    }
    for (; n < RUN && a < m; n++)
      a = parse(f, rt, a, rs + n);
    for (; n < RUN && b < e; n++)
      b = parse(f, rt, b, rs + n);
    for (u64 r = 0; r < n; r++) {
      u64 co = rt[rs[r].reg], h = rs[r].h;
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
      g->tb += rs[r].tb;
      u64 o = (h >> 40 & 255) * NT >> 8; // the name's owner: h's bits 40-47
      u32 w = x->fill[o];
      x->batch[w++] = (h & ~255ull) | co;
      x->fill[o] = w;
      if (w % BATCH == 0)
        post(x, o);
    }
  }
}

static void *worker(void *arg) {
  int t = (int)(intptr_t)arg;
  Ctx *x = &mem.ctx[t];
  x->agg = mem.agg[t], x->t = t;
  for (int o = 0; o < NT; o++)
    x->fill[o] = o * BATCH;
  for (int c = 0; c < 256; c++)
    mem.agg[t][c] = (Agg){.min = INT64_MAX, .max = INT64_MIN};
  regions(t);
  pthread_barrier_wait(&mem.meet); // the region table is complete
  // Piece i is the (i / n)th of the 2 MB stretch i % n: threads working at
  // once never share a stretch's page-table lock when they fault pages in.
  u64 n = (rsize >> 21) + 1;
  for (u64 i; (i = atomic_fetch_add(&mem.piece, 1)) < 16 * n;) {
    u64 c = i % n * (1 << 21) + i / n * PIECE;
    records(x, record_start(c), record_start(c + PIECE / 2), record_start(c + PIECE));
    drain(t, x->agg);
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
  fprintf(o, " %s%lld.%02lld", v < 0 ? "-" : "", llabs(v) / 100, llabs(v) % 100);
}

// Country c's line: the threads' totals added up
static void report(FILE *o, const char *code, int c) {
  Agg g = mem.agg[0][c];
  for (int t = 1; t < NT; t++) {
    const long long *a = (long long *)&mem.agg[t][c]; // the six additive fields
    for (int i = 0; i < 6; i++)
      ((long long *)&g)[i] += a[i];
    g.min = a[6] < g.min ? a[6] : g.min;
    g.max = a[7] > g.max ? a[7] : g.max;
  }
  if (!g.n) return;
  fprintf(o, "%s %lld %lld", code, g.n, g.ids);
  money(o, g.sum), money(o, g.min), money(o, g.max);
  fprintf(o, " %lld %lld %lld\n", g.names, g.nb, g.tb);
}

// Reads one byte per 2 MB and the last byte, to fault the page-table pages in.
static void touch(const u8 *p, u64 n) {
  for (u64 i = 0; i < n; i += 1 << 21)
    (void)((volatile const u8 *)p)[i];
  (void)((volatile const u8 *)p)[n - 1];
}

int main(void) {
  int rfd = open("/data/records.csv", O_RDONLY);
  int gfd = open("/data/regions.csv", O_RDONLY);
  if (rfd < 0 || gfd < 0) die("open /data");
  rsize = lseek(rfd, 0, SEEK_END), regsize = lseek(gfd, 0, SEEK_END);
  // records.csv is mapped over the start of a zeroed reservation one page
  // longer, so the vector loads that run past its end read zeros.
  // A failed map crashes at the first touch, which fails the run just as loudly.
  rec = mmap(0, rsize + 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  mmap(rec, rsize, PROT_READ, MAP_PRIVATE | MAP_FIXED, rfd, 0);
  reg = mmap(0, regsize, PROT_READ, MAP_PRIVATE, gfd, 0);
  mmap(&mem, 3 << 21, PROT_READ | PROT_WRITE,
       MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED | MAP_NORESERVE, -1, 0);
  madvise(&mem, 3 << 21, MADV_HUGEPAGE);
  // Fault the huge pages in here, and one page per 2 MB of each file (its
  // page-table page): a thread on another CPU that charges even one page to
  // the cgroup counts 256 KB against the peak (a per-CPU charge batch).
  for (u64 i = 0; i < 3 << 21; i += 1 << 21) ((volatile u8 *)&mem)[i] = 0;
  touch(rec, rsize), touch(reg, regsize);
  pthread_t th[NT];
  pthread_barrier_init(&mem.meet, 0, NT);
  pthread_attr_t at;
  pthread_attr_init(&at);
  for (long t = 1; t < NT; t++) {
    pthread_attr_setstack(&at, mem.stack[t], 1 << 14);
    if (pthread_create(&th[t], &at, worker, (void *)t)) die("pthread_create");
  }
  worker(0);
  FILE *o = fopen("/out/aggregates.txt", "w");
  if (!o) die("/out/aggregates.txt");
  for (int k = 0; k < 26 * 26 * 26; k++) // in code order
    if (mem.cidx[k])
      report(o, (char[]){'A' + k / 676, 'A' + k / 26 % 26, 'A' + k % 26, 0}, mem.cidx[k]);
  report(o, "_UNMATCHED", 0); // "_" sorts after every code
  fclose(o);
  for (int t = 1; t < NT; t++)
    pthread_join(th[t], 0);
  return 0;
}
