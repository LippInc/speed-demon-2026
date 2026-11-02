// Speed Demon 2026, Track C (Join): records.csv joined to regions.csv and
// aggregated per country. PERF.md has the measurement behind each choice.
//
// How it works, and why it is fast:
//  1. Twelve threads, one per hardware thread of the scoring box's six
//     P-cores. They take 128 KB pieces of the file from a shared counter, so
//     a thread that another process slows down takes fewer. A thread owns
//     the records that start in its piece.
//  2. Each record is read once, front to back, from one quote mask of 64 bytes
//     starting past most ids. A carry-less multiply turns the mask into
//     "follows an odd number of quotes", which picks out the
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
//     against 20.35 (PERF.md): page faults on the shared table nearly
//     doubled kernel cycles. The owner's table needs no atomic instruction.
//  6. All thread memory lies on three huge pages at a fixed address (see Mem),
//     faulted in by main and one helper before parsing starts. At the end
//     each thread drops its share of the file's mapping, so exit has next to
//     nothing left to tear down.
#define _GNU_SOURCE
#include <fcntl.h>
#include <immintrin.h>
#include <sched.h>
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
typedef uint16_t __attribute__((may_alias)) u16a;
typedef __m256i V;
typedef u64 __attribute__((aligned(1), may_alias)) u64u;
// BATCH: messages staged per owner; RING: the messages a ring holds (two
// batches); PIECE: bytes per piece of records.csv; RUN: records per parse loop.
enum { NT = 12, BATCH = 128, RING = 2 * BATCH, PIECE = 1 << 17, RUN = 64 };
// BUCKETS: as many as three huge pages hold, 10,496 slots for each owner's
// ~8,400 names; 590 buckets cost 0.38 ms more (PERF.md). The 4 extra let a walk
// run past the last bucket.
enum { BUCKETS = 652, SLOTS = 16 * (BUCKETS + 4) };

// One country's running totals, kept per thread: records, id sum, amount
// sum, min and max in cents, name and note bytes once unescaped (one field: the
// file is 202 MB, so the low half can't carry), distinct names.
// The five additive fields come first and are summed as one array in report.
typedef struct {
  long long n, ids, sum, text_bytes, names, min, max;
} Agg;
// What the parse loop reads out of a record: the name's hash, the id, the
// region's table index, name and note bytes once unescaped, the amount in
// cents.
typedef struct {
  u64 h;
  u32 id, reg, name_bytes, note_bytes, cents;
} Rec;
typedef struct { // a thread's outgoing messages, totals and parsed records
  // Owner o's next messages: from o * BATCH up to fill[o]
  _Alignas(64) u64 batch[NT * BATCH];
  Rec rec[RUN];
  u32 fill[NT], t; // t: this thread's number
  Agg *agg;
} Ctx;
// One owner's names; slot i of tag and row i belong to one name.
typedef struct {
  _Alignas(64) u32 tag[SLOTS]; // 0 = free; 16 per bucket, a cache line
  u64 row[SLOTS][4];           // a bit per country number
} Names;

static u64 rsize, regsize;  // file sizes
static const u8 *rec, *reg; // records.csv and regions.csv, mapped
typedef struct { // the messages from one thread to one owner, RING at most
  _Alignas(64) _Atomic u64 tail; // advanced by the sender
  _Alignas(64) _Atomic u64 head; // advanced by the owner
  _Alignas(64) u64 m[RING];      // message i is m[i % RING]
} Ring;
// Not const: the compiler rebuilds a constant it can see with three
// instructions per use, and the hot loop has no register free to keep one (2 %
// of all instructions).
#define ALL(x) {x, x, x, x}
V QUOTE = ALL(0x2222222222222222), DASH = ALL(0x2D2D2D2D2D2D2D2D),
  ZERO = ALL(0x3030303030303030);
// Caps fold3's lane 1, the region id: an id above the table reads its last
// entry, unmatched
V CAP = {-1, 1000001, -1, -1};
static const u8 KEEP[64] = {[0 ... 31] = 0xFF}; // KEEP + 32 - n keeps n <= 32

// All memory the threads use, on three transparent huge pages that main and
// one helper fault in before parsing starts, so no other thread ever
// page-faults on anonymous memory. A fixed address: an aligned static object
// would put the small variables around it on two more huge pages. It lies past
// the first GB above the program, where the kernel starts the heap at random,
// and with it main's thread pointer: at 1 << 30 this map erased those in 0.6 %
// of runs (PERF.md).
typedef struct {
  Ring ring[NT][NT];
  // A country number fits a byte: spec 2.3 has 240 countries, and 0 is
  // unmatched
  _Alignas(64) Agg agg[NT][256];
  Ctx ctx[NT];
  // Region id -> country number. Spec 2.3: ids stop at 999,999; the last entry
  // stands for any id above.
  u8 rtab[1000002];
  _Atomic u8 cidx[26 * 26 * 26]; // country code -> country number
  int ncountry;
  atomic_flag cmu;                // zero bytes: clear
  atomic_int finished, ready;     // ready: arrivals, see meet
  _Alignas(64) _Atomic u64 piece; // the next piece of records.csv to parse
  // The worker threads' stacks; they use at most 640 bytes
  _Alignas(64) u8 stack[NT][1 << 12];
  Names names[NT]; // the rest of Mem fits the first huge page
} Mem;
_Static_assert(sizeof(Mem) <= 3 << 21, "three huge pages");
#define mem (*(Mem *)(3ull << 29))

static void die(const char *what) {
  perror(what);
  exit(1);
}
static void *map(void *at, u64 n, int prot, int flags, int fd) {
  void *p = mmap(at, n, prot, flags, fd, 0);
  if (p == MAP_FAILED)
    die("mmap");
  return p;
}
static inline V load32(const u8 *p) { return _mm256_loadu_si256((const V *)p); }
static inline u32 eq32(V a, V b) { // a bit per byte of a == b
  return _mm256_movemask_epi8(_mm256_cmpeq_epi8(a, b));
}

// Three numbers at once, each up to 8 raw bytes in the top bytes of a
// 64-bit lane: a saturating subtract of '0' turns digits into their values
// and every other byte (the zeros the shifts brought in, the sign, the comma)
// into 0, so the zeros below read as leading zeros. Digit pairs, then quads,
// then octets combine in one multiply each. Lane c is an amount, "DDDDD.CC":
// its multipliers leave the point out, and its top four digits count 1000.
static inline V fold3(u64 a, u64 b, u64 c) {
  V x = _mm256_set_epi64x(0, c, b, a);
  x = _mm256_subs_epu8(x, ZERO);
  long long d = 0x010A010A010A010A;
  x = _mm256_maddubs_epi16(
      x, _mm256_set_epi64x(0, 0x010A0001010A010A, d, d));  // 10a + b
  x = _mm256_madd_epi16(x, _mm256_set1_epi32(0x00010064)); // 100a + b
  V y = _mm256_mul_epu32(x, _mm256_set_epi64x(0, 1000, 10000, 10000));
  return _mm256_add_epi64(y, _mm256_srli_epi64(x, 32)); // 10000a + b
}

// A bit per quote byte of 64 bytes: the first 32 in v, then those at p
static inline u64 quotes(V v, const u8 *p) {
  return eq32(v, QUOTE) | (u64)eq32(load32(p), QUOTE) << 32;
}

// Name hashing: of the 32 bytes v, the first 16 are the 128-bit state and the
// last 16 the key of one AES round. After two more rounds every name byte has
// reached all 128 bits of the state. One round is a bijection, so names of up
// to 16 bytes cannot collide before the state is cut to 64 bits.
#define K1 _mm_set_epi64x(0x589965CC75374CC3, 0xE7037ED1A0B428DB)
#define K2 _mm_set_epi64x(0x1D8E4E27C47D124F, 0x9E3779B97F4A7C15)
// v's first n bytes are the name
static inline u64 name_hash(V v, u64 n) {
  v = _mm256_and_si256(v, load32(KEEP + 32 - n));
  __m128i h = _mm_aesenc_si128(_mm256_castsi256_si128(v),
                               _mm256_extracti128_si256(v, 1));
  return _mm_cvtsi128_si64(_mm_aesenc_si128(_mm_aesenc_si128(h, K1), K2));
}

// A bit per tag of the 16 at p that equals w's
static inline u32 eq16(const u32 *p, V w) {
  V d = _mm256_cmpeq_epi32(_mm256_load_si256((const V *)p), w);
  V e = _mm256_cmpeq_epi32(_mm256_load_si256((const V *)(p + 8)), w);
  return _mm256_movemask_ps(_mm256_castsi256_ps(d)) |
         _mm256_movemask_ps(_mm256_castsi256_ps(e)) << 8;
}

// The slot of the name of message msg; a new name takes the first free slot
// of the walk. A hit is what nearly all messages find (97%), in the first
// bucket; only the rest pay for the search for a free slot. The walk ends at
// the first bucket with a free slot, and that bound is not checked: an owner
// has 10,496 slots for about 8,400 names (8,604 at most when measured), so full
// buckets come in short runs, and the 4 extra catch one at the table's end. A
// dataset with far more names than the generator's ~100,000 would overrun.
static inline u32 slot_of(Names *nm, const u64 *msg) {
  // The tag, bits 8 to 39 of the message. A name whose tag is 0 (2^-32) would
  // share its row with the next name to take that slot.
  V tag = _mm256_broadcastd_epi32(_mm_loadu_si32((const u8 *)msg + 1));
  u32 m;
  for (u64 b = ((const u16a *)msg)[3] * BUCKETS >> 16;; b++) {
    if (__builtin_expect((m = eq16(nm->tag + 16 * b, tag)) != 0, 1))
      return 16 * b + _tzcnt_u32(m);
    if ((m = eq16(nm->tag + 16 * b, _mm256_setzero_si256()))) {
      nm->tag[16 * b + _tzcnt_u32(m)] = *msg >> 8;
      return 16 * b + _tzcnt_u32(m);
    }
  }
}

// One step of a wait: a pause, and every 128th a 10 us sleep. A thread that
// another process has pushed off its CPU then gets the CPU this one frees;
// waits that only spin ran 14 times slower beside one busy process (PERF.md).
static void rest(int *w) { ++*w % 128 ? _mm_pause() : (void)usleep(10); }

static int country(const u8 *p) { // numbers countries in order of first sight
  int k = (p[0] - 'A') * 676 + (p[1] - 'A') * 26 + (p[2] - 'A'), c;
  if (!(c = mem.cidx[k])) {
    for (int w = 0; atomic_flag_test_and_set(&mem.cmu);)
      rest(&w);
    if (!(c = mem.cidx[k]))
      c = mem.cidx[k] = ++mem.ncountry;
    atomic_flag_clear(&mem.cmu);
  }
  return c;
}

// The lines that start in this thread's tenth of regions.csv (thread 1's first
// line, the header, starts at 0 and is skipped). Main (thread 0) and the helper
// (thread 11) have none: they are faulting memory in.
static void regions(int t) {
  u64 p = t > 1 ? regsize * (t - 1) / (NT - 2) : 1, e = regsize * t / (NT - 2);
  while (p < e && reg[p - 1] != '\n')
    p++;
  while (p < e) { // "id,CCC\n": plain ASCII, spec 2.3
    u64 id = 0;
    while (reg[p] != ',')
      id = id * 10 + reg[p++] - '0';
    mem.rtab[id] = country(reg + p + 1);
    // Past the comma, the 3-letter code and the LF (spec 2.3)
    p += 5;
  }
}

// The first record starting at or after q, past its BOM if it has one. Byte 0
// starts the BOM and header (spec 2.2), never a record, so the search starts
// at 1.
static u64 record_start(u64 q) {
  for (q = q ? q : 1; q < rsize; q++)
    if (rec[q - 1] == '\n' && ((u8)(rec[q] - '0') < 10 || rec[q] == 0xEF))
      return q + (rec[q] == 0xEF) * 3;
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
      // Never wraps: batches are whole, and 64 divides them
      const u64 *ms = m + h % RING;
      for (u64 j = 0; j < e - h; j++) {
        id[j] = slot_of(nm, ms + j);
        _mm_prefetch((const char *)nm->row[id[j]], _MM_HINT_T0);
      }
      for (u64 j = 0; j < e - h; j++) {
        u64 c = ms[j] & 255, *w = &nm->row[id[j]][c >> 6], x = *w;
        // Sets the country's bit; a bit that was clear counts (9 instructions
        // in C)
        __asm__("btsq %2, %0; sbbq $-1, %1"
                : "+r"(x), "+m"(a[c].names)
                : "r"(c)
                : "cc");
        *w = x;
      }
    }
    atomic_store_explicit(&r->head, k, memory_order_release);
  }
  return done;
}

// Waits until n arrivals in all have reached the meeting points. The count only
// grows: all 12 threads arrive at NT and again at 2 * NT, then the 11 others
// (after they unmap) and main at 3 * NT.
static void meet(int n) {
  atomic_fetch_add(&mem.ready, 1);
  for (int w = 0; mem.ready < n;)
    rest(&w);
}

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

// Prefix XOR of q: a bit is set when q has an odd count at or below it
static inline u64 odd_quotes(u64 q) {
  return _mm_cvtsi128_si64(
      _mm_clmulepi64_si128(_mm_cvtsi64_si128(q), _mm_set1_epi8(-1), 0));
}

// The comma after the field at b, a name or a note of any length, quoted or
// empty and unquoted: its offset in the low half, the field's quotes in the
// high half.
static u64 note_end(const u8 *b) {
  u64 p = 0, n = 0;
  for (u64 i = 0;; i += 64) {
    u64 q = quotes(load32(b + i), b + i + 32), odd = odd_quotes(q) ^ p,
        o = ~(odd | q);
    if (o)
      return i + _tzcnt_u64(o) |
             (n + _mm_popcnt_u64(_bzhi_u64(q, _tzcnt_u64(o)))) << 32;
    n += _mm_popcnt_u64(q);
    p = -(odd >> 63);
  }
}

// Reads the record at b into *rc; returns the next record's start.
static inline __attribute__((always_inline)) const u8 *
parse(const u8 *b, const u8 *rt, Rec *rc, u64 *open) {
  _mm_prefetch((const char *)b + 512, _MM_HINT_T0);
  _mm_prefetch((const char *)b + 576, _MM_HINT_T0);
  // j, d: the commas after the name and the note. pq, qn: the quotes of the
  // name and the note. o: quote-mask bits that are commas outside quotes.
  u64 j, d, qn, o = 0, pq = 0;
  // The quote mask q starts at w, past most ids, so that for 97% of records it
  // reaches the end of the note. j, d and t count from w; t is the first quote.
  const u8 *w = b + 6;
  u64 q = quotes(load32(w), w + 32), t = _tzcnt_u64(q);
  // The name opens where it did in this cursor's last record: ids only grow
  // (spec 2.2), so their length changes 6 times in a file and needs no search.
  // If the mask's first quote is elsewhere, the id got longer, or is too short
  // for the mask (0.6%), or the name is empty and unquoted (0.5%). That last
  // one's first quote is the note's, a byte past where the name's would be, and
  // ids never shrink, so it can't match *open either.
  if (__builtin_expect(t != *open, 0)) {
    // a: the id's comma. A name's opening quote follows it: a - 5 from w.
    u64 a = _tzcnt_u64(eq32(load32(b), _mm256_set1_epi8(',')));
    t = *open = a - 5;
    u64 ne = note_end(b + a + 1), n = (u32)ne;
    j = a + 1 - 6 + n;
    pq = ne >> 32;
    rc->name_bytes = n - pq / 2 - (n != 0);
    rc->h = name_hash(load32(b + a + 2), n ? n - 2 : 0);
  } else {
    // Past the opening quote, the commas after the name and after the note
    // are the first two bytes that follow an odd number of quotes and are
    // not one. A "" flips the count twice.
    o = _andn_u64(q, odd_quotes(_blsr_u64(q)));
    j = _tzcnt_u64(o);
    o = _blsr_u64(o);
    pq = _mm_popcnt_u64(_bzhi_u64(q, j));
    u64 n = j - t - 2;
    rc->name_bytes = n + 1 - pq / 2;
    // The hash keeps a name's first 32 raw bytes, so two names that differ only
    // past those merge (tested: 32 holds, 33 merges). Spec 2.2 sets no bound,
    // but over 40 generated seeds the longest was 27 (base name at most 24 by
    // the generator's forms, plus runs of trailing spaces that get rarer
    // about 4 times per extra byte), so about 2e-5 per dataset.
    rc->h = name_hash(load32(w + t + 1), n);
  }
  if (__builtin_expect(o != 0, 1)) {
    d = _tzcnt_u64(o);
    qn = _mm_popcnt_u64(_bzhi_u64(q, d)) - pq;
  } else { // the note runs past the mask, or the name was read above
    u64 ne = note_end(w + j + 1);
    d = j + 1 + (u32)ne;
    qn = ne >> 32;
  }
  // A quoted note holds qn quotes, its two delimiters and two per "", and
  // loses qn / 2 + 1 bytes; an unquoted one is empty (1.5%).
  u64 len = d - j - 1;
  rc->note_bytes = len - qn / 2 - (len != 0);
  // The id in the top of the lane; it has t + 5 digits
  u64 idb = *(const u64u *)b << (24 - 8 * t & 63);
  // The region and the amount. The commas, CRs and LFs are the bytes below '-';
  // digits, '-' and '.' are not. A BOM's first byte is too, as a signed byte.
  const u8 *r = w + d + 1;
  u64 m = (u32)_mm256_movemask_epi8(_mm256_cmpgt_epi8(DASH, load32(r)));
  u64 c = _tzcnt_u64(m); // the comma after the region
  m = _blsr_u64(m);
  // The end of the amount: a CR, an LF, or the zeros past the file
  u64 e = _tzcnt_u64(m);
  // The amount, up in the top of the lane
  u64 v = *(const u64u *)(r + c + 1) << (64 - 8 * (e - c - 1));
  V f = _mm256_min_epu32(fold3(idb, *(const u64u *)r << (64 - 8 * c), v), CAP);
  rc->id = _mm256_cvtsi256_si32(f);
  u64 rid = _mm256_extract_epi64(f, 1);
  int32_t cents = _mm256_extract_epi64(f, 2);
  _mm_prefetch((const char *)rt + rid, _MM_HINT_T0);
  rc->reg = rid;
  rc->cents = r[c + 1] == '-' ? -cents : cents;
  // The next record: past the whole run of low bytes, an LF after a CR and a
  // BOM's three bytes (777 among 3.32 M records) too. m's lowest bit is the
  // amount's end. Adding it to m carries through the run of set bits there and
  // clears them, so the sum's lowest set bit is the first byte past the run.
  return r + _tzcnt_u64(m + _blsi_u64(m));
}

// The records from a up to m and from m up to e, a run of up to RUN at a time:
// parsed in turn from the two places, then totalled and sent on.
static void records(Ctx *x, const u8 *a, const u8 *m, const u8 *e) {
  // Locals, not globals: the stores below could alias those.
  const u8 *rt = mem.rtab, *b = m;
  Agg *ag = x->agg;
  Rec *rs = x->rec;
  u64 n, oa = -1, ob = -1;
  while (a < m || b < e) {
    for (n = 0; n < RUN && ((a - m) & (b - e)) < 0; n += 2) { // a < m, b < e
      a = parse(a, rt, rs + n, &oa);
      b = parse(b, rt, rs + n + 1, &ob);
    }
    if (a >= m) { // the first half is done: the second's cursor goes on alone
      a = b;
      m = e;
      oa = ob;
      b = e;
    }
    for (; n < RUN && a < m; n++)
      a = parse(a, rt, rs + n, &oa);
    for (u64 r = 0; r < n; r++) {
      u64 co = rt[rs[r].reg], h = rs[r].h;
      Agg *g = ag + co;
      int64_t cents = (int32_t)rs[r].cents;
      g->n++;
      g->ids += rs[r].id;
      g->sum += cents;
      if (cents < g->min) // rarely: a thread sees ~900 amounts per country
        g->min = cents;
      if (cents > g->max)
        g->max = cents;
      // Name bytes in the low half, note bytes in the high half
      g->text_bytes += *(const u64u *)&rs[r].name_bytes;
      // The name's owner: the hash's low byte, which the message does not carry
      u64 o = (h & 255) * NT >> 8;
      u32 w = x->fill[o];
      x->batch[w++] = (h & ~255ull) | co;
      x->fill[o] = w;
      if (w % BATCH == 0)
        post(x, o);
    }
  }
}

// Reads one byte per 2 MB up to the last byte, to fault the page-table pages
// in.
static void touch(const u8 *p, u64 n) {
  for (u64 i = 0; i < n + (1 << 21) - 1; i += 1 << 21)
    (void)((volatile const u8 *)p)[i < n ? i : n - 1];
}

// One thread's whole run, main's too as thread 0: its start-up part, the first
// meeting, pieces until none are left, the last messages, the second meeting,
// the unmapping.
static int worker(void *arg) {
  int t = (int)(intptr_t)arg;
  Ctx *x = &mem.ctx[t];
  x->agg = mem.agg[t];
  x->t = t;
  for (int o = 0; o < NT; o++)
    x->fill[o] = o * BATCH;
  for (int c = 0; c < 256; c++)
    mem.agg[t][c] = (Agg){.min = INT64_MAX, .max = INT64_MIN};
  // The helper; main does the same with the other huge page and the rest
  if (t == NT - 1) {
    ((volatile u8 *)&mem)[2 << 21] = 0;
    touch(rec + rsize * 3 / 8, rsize - rsize * 3 / 8);
  } else
    regions(t);
  meet(NT); // the region table is complete
  // Piece i is the (i / n)th of the 2 MB stretch i % n: threads working at
  // once never share a stretch's page-table lock when they fault pages in. A
  // stretch holds 16 pieces.
  u64 n = (rsize >> 21) + 1;
  for (u64 i; (i = atomic_fetch_add(&mem.piece, 1)) < 16 * n;) {
    u64 c = i % n * (1 << 21) + i / n * PIECE;
    records(x, rec + record_start(c), rec + record_start(c + PIECE / 2),
            rec + record_start(c + PIECE));
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
  meet(2 * NT);
  // Every record is parsed: the other threads drop an eleventh of the
  // mapping each, or exit tears down all 49,000 page-table entries on one CPU.
  if (t) {
    u64 a = rsize * (t - 1) / (NT - 1) & ~4095ull, e = rsize * t / (NT - 1);
    madvise((u8 *)rec + a, (e & ~4095ull) - a, MADV_DONTNEED);
    atomic_fetch_add(&mem.ready, 1);
  }
  return 0;
}

static void money(FILE *o, long long v) { // cents as -?D+.DD
  fprintf(o, " %s%lld.%02lld", "-" + (v >= 0), llabs(v) / 100, llabs(v) % 100);
}

// Country c's line: the threads' totals added up
static void report(FILE *o, const char *code, int c) {
  Agg g = mem.agg[0][c];
  for (int t = 1; t < NT; t++) {
    // The five additive fields
    const long long *a = (long long *)&mem.agg[t][c];
    for (int i = 0; i < 5; i++)
      ((long long *)&g)[i] += a[i];
    g.min = a[5] < g.min ? a[5] : g.min;
    g.max = a[6] > g.max ? a[6] : g.max;
  }
  if (!g.n)
    return;
  fprintf(o, "%s %lld %lld", code, g.n, g.ids);
  money(o, g.sum);
  money(o, g.min);
  money(o, g.max);
  fprintf(o, " %lld %lld %lld\n", g.names, g.text_bytes & UINT32_MAX,
          g.text_bytes >> 32);
}

int main(void) {
  int rfd = open("/data/records.csv", O_RDONLY);
  int gfd = open("/data/regions.csv", O_RDONLY);
  if (rfd < 0 || gfd < 0)
    die("/data");
  // A failed lseek gives a size that map below refuses
  rsize = lseek(rfd, 0, SEEK_END);
  regsize = lseek(gfd, 0, SEEK_END);
  // records.csv is mapped over the start of a zeroed reservation one page
  // longer, so the vector loads that run past its end read zeros.
  int anon = MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE;
  rec = map(0, rsize + 4096, PROT_READ, anon, -1);
  map((u8 *)rec, rsize, PROT_READ, MAP_PRIVATE | MAP_FIXED, rfd);
  reg = map(0, regsize, PROT_READ, MAP_PRIVATE, gfd);
  map(&mem, 3 << 21, PROT_READ | PROT_WRITE, anon | MAP_FIXED, -1);
  madvise(&mem, 3 << 21, MADV_HUGEPAGE);
  // Only main and one helper fault memory in, here and at the top of worker: a
  // thread on another CPU that charges even one page to the cgroup counts 256
  // KB against the peak (a per-CPU charge batch). Reading one byte per 2 MB of
  // each file faults its page-table pages in, and those use up the helper's
  // batch.
  *(volatile u8 *)&mem = 0;
  touch(reg, regsize);
  // Plain threads: they share main's thread-local block and use nothing of it
  // but errno, and take no memory for a TLS block or a thread descriptor.
  int fl = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
  // The helper first: it starts ahead of main, so it takes 5/8 of the file. A
  // stack grows down, so thread t's starts at the end of stack[t].
  for (long t = NT - 1; t > 0; t--)
    if (clone(worker, mem.stack[t + 1], fl, (void *)t) < 0)
      die("clone");
  // The other threads fill the region table meanwhile: all they touch before
  // the barrier lies on the first huge page.
  ((volatile u8 *)&mem)[1 << 21] = 0;
  touch(rec, rsize * 3 / 8);
  worker(0);
  FILE *o = fopen("/out/aggregates.txt", "w");
  if (!o)
    die("/out/aggregates.txt");
  for (int k = 0, c; k < 26 * 26 * 26; k++) // in code order
    if ((c = mem.cidx[k]))
      report(o, (char[]){'A' + k / 676, 'A' + k / 26 % 26, 'A' + k % 26, 0}, c);
  report(o, "_UNMATCHED", 0); // "_" sorts after every code
  if (fclose(o))
    die("/out/aggregates.txt");
  meet(3 * NT); // every other thread is done with its part of the mapping
  return 0;
}
