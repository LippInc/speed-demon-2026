// Track D (Traverse): exact directed shortest-path lengths for 2,000 queries on a road-like graph.
// See README.md for the design and PERF.md for the measurements behind each constant.
#define _GNU_SOURCE
#include <fcntl.h>
#include <immintrin.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

typedef uint8_t u8;
typedef uint16_t u16;
typedef uint32_t u32;
typedef uint64_t u64;
typedef u64 __attribute__((aligned(1), may_alias)) u64u;
typedef unsigned __int128 u128;

#define LOAD(x) __atomic_load_n(x, __ATOMIC_RELAXED)
#define BUMP(x, by) __atomic_fetch_add(x, by, __ATOMIC_RELAXED)
#define UNLIKELY(c) __builtin_expect(!!(c), 0)
#define INLINE static inline __attribute__((always_inline))
#define PAGE2M (1ul << 21)
#define INF UINT64_MAX

INLINE void fetch(const void *p) { _mm_prefetch(p, _MM_HINT_T0); }


// ---- worker pool: threads wait spinning between phases, so a phase starts in about a microsecond.

static int workers;
static void (*volatile phase_fn)(int);
static u32 phase_no, phase_left;

static void rest(u32 *spins) {
  if (++*spins & 1023)
    _mm_pause();
  else
    sched_yield();
}

static void *worker_main(void *arg) {
  for (u32 seen = 0, spins = 0;; seen++) {
    while (__atomic_load_n(&phase_no, __ATOMIC_ACQUIRE) == seen)
      rest(&spins);
    phase_fn((int)(intptr_t)arg);
    __atomic_fetch_sub(&phase_left, 1, __ATOMIC_RELEASE);
  }
  return 0;
}

static void all_workers(void (*fn)(int)) {
  phase_fn = fn;
  phase_left = workers - 1;
  __atomic_store_n(&phase_no, phase_no + 1, __ATOMIC_RELEASE);
  fn(0);
  u32 spins = 0;
  while (__atomic_load_n(&phase_left, __ATOMIC_ACQUIRE))
    rest(&spins);
}

// ---- memory: big arrays on huge pages, each faulted once by one thread; small sparse ones on 4 KB pages.

static u8 *reserve(size_t bytes, int advice) {
  u8 *p = mmap(0, bytes, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  madvise(p, bytes, advice);
  return p;
}

static u8 *small_top;
static void *small_region(size_t bytes) { return (void *)BUMP(&small_top, (bytes + 4095) & -4096ul); }

static u8 *touch_from;
static u32 touch_next, touch_count;
static void touch_job(int t) {
  (void)t;
  for (u32 i; (i = BUMP(&touch_next, 1)) < touch_count;)
    *(volatile u8 *)(touch_from + i * PAGE2M) = 0;
}
static void touch_pages(u8 *from, size_t bytes) {
  touch_from = from, touch_next = 0;
  touch_count = (bytes + PAGE2M - 1) / PAGE2M;
  all_workers(touch_job);
}

// ---- weights too large for an edge's 11-bit field: one lock-free hash table for every kind of edge index.

enum { RAW_KIND = 1, FWD_KIND, REV_KIND, ZONE_KIND, TABLE_SLOTS = 1 << 15 };
static struct {
  u64 key, value;
} *heavy_table;

INLINE u64 heavy_slot(u64 key) { return (key * 0x9E3779B97F4A7C15ull) >> 49; }

static void heavy_put(u32 kind, u64 index, u64 value) {
  u64 key = (u64)kind << 48 | index;
  for (u64 i = heavy_slot(key);; i = (i + 1) % TABLE_SLOTS) {
    u64 empty = 0;
    if (__atomic_compare_exchange_n(&heavy_table[i].key, &empty, key, 0, __ATOMIC_RELAXED, __ATOMIC_RELAXED)) {
      heavy_table[i].value = value;
      return;
    }
  }
}

static u64 heavy_get(u32 kind, u64 index) {
  u64 key = (u64)kind << 48 | index, i = heavy_slot(key);
  while (heavy_table[i].key != key)
    i = (i + 1) % TABLE_SLOTS;
  return heavy_table[i].value;
}

// ---- input

enum { PIECES = 48, REC = 6, ROUND = 12 };
enum { BUCKET_BITS = 13, BUCKET_NODES = 1 << BUCKET_BITS };
#define ALL_ONES 0xFFFFFFFFu
#define SPILLED 0xE0000000u // a row's fourth slot at or above this indexes the spill area (node ids stay below)
#define WMAX 2047u

typedef struct {
  u32 u, v, w;
} InputEdge;

static const u8 *body, *body_end;
static u32 n, m, buckets;
static size_t cell_bytes;
static u8 *bins;
static u32 *cell_fill;
static InputEdge *rare_edges;
static u32 rare_count, piece_next;

static const u8 *map_file(const char *path, size_t *len) {
  int fd = open(path, O_RDONLY);
  *len = lseek(fd, 0, SEEK_END);
  // Anonymous pages before and after the file: numbers load 8 bytes back and blocks 64 bytes forward.
  u8 *p = mmap(0, *len + 3 * 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  mmap(p + 4096, *len, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0);
  return p + 4096;
}

static u32 read_u32(const u8 **p) {
  u32 x = 0;
  for (; **p >= '0'; ++*p)
    x = x * 10 + **p - '0';
  ++*p;
  return x;
}

static const u8 *piece_start(int p) {
  const u8 *x = body + (body_end - body) * p / PIECES;
  while (x < body_end && x[-1] != '\n')
    x++;
  return x;
}

// One line, three numbers at once: each lane holds the 8 bytes ending at a separator, with the bytes before
// the number masked off, and is converted by the usual digit pairs, quads, eights.
INLINE void read_line(const u8 *a, const u32 *sp, u32 *out) {
  u32 l0 = sp[1] - sp[0] - 1, l1 = sp[2] - sp[1] - 1, l2 = sp[3] - sp[2] - 1;
  __m256i x = _mm256_set_epi64x(0, *(u64u *)(a + sp[3] - 8), *(u64u *)(a + sp[2] - 8), *(u64u *)(a + sp[1] - 8));
  __m256i cut = _mm256_set_epi64x(64, 64 - 8 * l2, 64 - 8 * l1, 64 - 8 * l0);
  x = _mm256_and_si256(x, _mm256_sllv_epi64(_mm256_set1_epi8(0x0F), cut));
  x = _mm256_madd_epi16(_mm256_maddubs_epi16(x, _mm256_set1_epi16(0x010A)), _mm256_set1_epi32(0x00010064));
  x = _mm256_add_epi64(_mm256_mul_epu32(x, _mm256_set1_epi64x(10000)), _mm256_srli_epi64(x, 32));
  out[0] = _mm256_extract_epi32(x, 0), out[1] = _mm256_extract_epi32(x, 2), out[2] = _mm256_extract_epi32(x, 4);
  if (UNLIKELY(l2 > 8)) {
    const u8 *c = a + sp[2] + 1;
    out[2] = read_u32(&c);
  }
}

static void parse_piece(int p, u8 **cursor) {
  const u8 *a = piece_start(p), *z = piece_start(p + 1), *block = a;
  u32 sp[64 * 32 + 32];
  int have = 1;
  sp[0] = ALL_ONES; // the separator before the first line, at offset -1
  for (u32 b = 0; b < buckets; b++)
    cursor[b] = bins + b * cell_bytes * PIECES + p * cell_bytes;
  __m256i zero_char = _mm256_set1_epi8('0');
  while (block < z) {
    for (int j = 0; j < 64 && block < z; j++, block += 64) {
      __m256i x0 = _mm256_loadu_si256((const __m256i *)block), x1 = _mm256_loadu_si256((const __m256i *)(block + 32));
      u32 m0 = _mm256_movemask_epi8(_mm256_cmpgt_epi8(zero_char, x0));
      u64 mask = m0 | (u64)(u32)_mm256_movemask_epi8(_mm256_cmpgt_epi8(zero_char, x1)) << 32;
      u32 base = block - a, c = __builtin_popcountll(mask), *o = sp + have;
      for (u32 i = 0; i < c; i += 12)
        for (int k = 0; k < 12; k++, mask = _blsr_u64(mask))
          o[i + k] = base + (u32)_tzcnt_u64(mask);
      have += c;
    }
    if (block >= z)
      while (have > 1 && sp[have - 1] >= (u32)(z - a))
        have--;
    int i = 0;
    for (; i + 3 < have; i += 3) {
      u32 e[3];
      read_line(a, sp + i, e);
      if (UNLIKELY(e[2] >= 16383)) {
        rare_edges[BUMP(&rare_count, 1)] = (InputEdge){e[0], e[1], e[2]};
        continue;
      }
      u8 **c = &cursor[e[0] >> BUCKET_BITS];
      *(u64u *)*c = (e[0] & (BUCKET_NODES - 1)) | (u64)e[1] << BUCKET_BITS | (u64)e[2] << 34;
      *c += REC;
    }
    memmove(sp, sp + i, (have - i) * sizeof(u32));
    have -= i;
  }
  for (u32 b = 0; b < buckets; b++)
    cell_fill[b * PIECES + p] = (cursor[b] - (bins + b * cell_bytes * PIECES + p * cell_bytes)) / REC;
  uintptr_t lo = ((uintptr_t)a + 4095) & -4096ul, hi = (uintptr_t)z & -4096ul;
  if (lo < hi)
    // real work now, in parallel, and none left for exit
    madvise((void *)lo, hi - lo, MADV_DONTNEED);
}

static void parse_job(int t) {
  (void)t;
  u8 *cursor[buckets];
  for (int p; (p = BUMP(&piece_next, 1)) < PIECES;)
    parse_piece(p, cursor);
}

// ---- first rows: four slots per node, in the memory the bins of earlier buckets just gave up.

static u32 *rows, *spill;
static u32 spill_top = 1, round_next, round_end;
static u8 *state; // one byte per node: a shared line is read once for all three flags, and can be prefetched
enum { LEAF = 1, TAKEN = 2, QUERIED = 4 };

typedef struct {
  u8 fill[BUCKET_NODES];
  u32 next[BUCKET_NODES];
  u32 extra_low[1 << 16], extra_edge[1 << 16];
} RowWork;
static RowWork **row_work;

INLINE void add_input_edge(u32 first, u32 *r, RowWork *k, int *extras, u32 low, u32 v, u32 w) {
  if (UNLIKELY(v == first + low))
    return;
  u32 f = k->fill[low]++, e = v << 11 | (w < WMAX ? w : WMAX);
  if (UNLIKELY(w >= WMAX))
    heavy_put(RAW_KIND, (u64)(first + low) << 8 | f, w);
  if (f < 4)
    r[low * 4 + f] = e;
  else
    k->extra_low[*extras] = low, k->extra_edge[(*extras)++] = e;
}

static void build_bucket(u32 b, RowWork *k) {
  u32 first = b << BUCKET_BITS, size = n - first < BUCKET_NODES ? n - first : BUCKET_NODES, *r = rows + 4ul * first;
  int extras = 0;
  memset(r, 0xFF, size * 16ul);
  memset(k->fill, 0, size);
  for (int p = 0; p < PIECES; p++) {
    const u8 *c = bins + b * cell_bytes * PIECES + p * cell_bytes;
    for (u32 i = 0, fill = cell_fill[b * PIECES + p]; i < fill; i++, c += REC) {
      u64 x = *(u64u *)c;
      add_input_edge(first, r, k, &extras, x & (BUCKET_NODES - 1), x >> BUCKET_BITS & 0x1FFFFF, x >> 34 & 0x3FFF);
    }
  }
  for (u32 i = 0; i < rare_count; i++)
    if (rare_edges[i].u >> BUCKET_BITS == b)
      add_input_edge(first, r, k, &extras, rare_edges[i].u - first, rare_edges[i].v, rare_edges[i].w);
  for (u32 low = 0; low < size; low++) {
    u32 f = k->fill[low], u = first + low;
    // never a branch: about half of all nodes are leaves
    state[u] |= (f <= 2) & !(state[u] & QUERIED);
    if (UNLIKELY(f > 4)) {
      u32 pos = BUMP(&spill_top, f - 3 + 1);
      spill[pos] = f - 3, spill[pos + 1] = r[low * 4 + 3];
      r[low * 4 + 3] = SPILLED + pos;
      k->next[low] = pos + 2;
    }
  }
  for (int i = 0; i < extras; i++)
    spill[k->next[k->extra_low[i]]++] = k->extra_edge[i];
}

static void rows_job(int t) {
  for (u32 b; (b = BUMP(&round_next, 1)) < round_end;)
    build_bucket(b, row_work[t]);
}

// ---- zones: each worker grows a breadth-first zone over the first rows. A step through a node with no real
// choice (out-degree at most 2, not a query endpoint) does not stop: the edge continues along the road to the
// next node with a choice, so contraction, ordering and gathering are one pass.

enum { FRONT_BATCH = 8, WALKS_MAX = 2048 };
#define DEAD ALL_ONES

typedef struct __attribute__((aligned(64))) {
  u32 *order, *edges;
  u8 *degree;
  u32 nodes, edge_count, batch_first;
  u64 *long_edge;
  u32 id_base, edge_base;
} Zone;

typedef struct {
  u32 pos, from, back, at, hops;
  u64 len;
} Road;

static Zone *zones;

// No atomics: a locked operation on these shared lines was 40 % of this phase. Two workers that race for a node
// both list it; heads all get the id written last, the other copy is a row no edge leads to, and is harmless.
INLINE void claim(Zone *z, u32 v) {
  if (!(state[v] & TAKEN)) {
    state[v] |= TAKEN;
    z->order[z->nodes++] = v;
  }
}

INLINE u64 raw_weight(u32 tail, u32 f, u32 e) {
  u32 w = e & WMAX;
  return w < WMAX ? w : heavy_get(RAW_KIND, (u64)tail << 8 | f);
}

INLINE void emit(Zone *z, u32 pos, u32 head, u64 len) {
  z->edges[pos] = head << 11 | (len < WMAX ? (u32)len : WMAX);
  if (UNLIKELY(len >= WMAX))
    z->long_edge[pos - z->batch_first] = len;
}

// One hop along a road. Returns 1 while the road goes on.
INLINE int road_step(Zone *z, Road *r) {
  u32 at = r->at;
  const u32 *row = rows + 4ul * at;
  u32 e0 = row[0], e1 = row[1], ways = 2;
  if (state[at] & LEAF && r->hops++ < 64) {
    u32 ok0 = e0 != ALL_ONES && e0 >> 11 != r->back, ok1 = e1 != ALL_ONES && e1 >> 11 != r->back;
    ways = ok0 + ok1;
    if (ways == 1) {
      u32 f = !ok0, e = row[f];
      r->len += raw_weight(at, f, e);
      r->back = at, r->at = e >> 11;
      if (r->at != r->from)
        return 1;
      ways = 0; // a road back to where it began is never on a shortest path
    }
  }
  if (ways)
    claim(z, at), emit(z, r->pos, at, r->len);
  else
    z->edges[r->pos] = DEAD;
  return 0;
}

static void grow_zone(Zone *z, u32 seed, Road *roads) {
  u32 lo = z->nodes;
  claim(z, seed);
  while (lo < z->nodes) {
    u32 hi = lo + FRONT_BATCH < z->nodes ? lo + FRONT_BATCH : z->nodes, nroads = 0;
    u32 first = z->batch_first = z->edge_count;
    for (u32 h = hi; h < z->nodes && h < hi + 2 * FRONT_BATCH; h++)
      fetch(rows + 4ul * z->order[h]);
    for (u32 h = hi; h < z->nodes && h < hi + FRONT_BATCH; h++)
      for (int f = 0; f < 4; f++)
        fetch(&state[rows[4ul * z->order[h] + f] >> 11]);
    for (u32 h = lo; h < hi; h++) {
      u32 u = z->order[h], before = z->edge_count;
      const u32 *row = rows + 4ul * u, *more = row;
      u32 inline_edges = 4, more_edges = 0;
      if (UNLIKELY(row[3] >= SPILLED && row[3] != ALL_ONES)) {
        more = spill + (row[3] - SPILLED);
        more_edges = *more++;
        inline_edges = 3;
      }
      for (u32 f = 0; f < inline_edges + more_edges; f++) {
        u32 e = f < inline_edges ? row[f] : more[f - inline_edges];
        if (e == ALL_ONES)
          break;
        u32 head = e >> 11, pos = z->edge_count++;
        u64 w = raw_weight(u, f, e);
        if (!(state[head] & LEAF)) {
          claim(z, head);
          emit(z, pos, head, w);
        } else {
          roads[nroads++] = (Road){pos, u, u, head, 0, w};
        }
      }
      z->degree[h] = z->edge_count - before;
    }
    while (nroads) {
      u32 kept = 0;
      for (u32 i = 0; i < nroads; i++) {
        fetch(rows + 4ul * roads[i].at);
        fetch(&state[roads[i].at]);
      }
      for (u32 i = 0; i < nroads; i++)
        if (road_step(z, &roads[i]))
          roads[kept++] = roads[i];
      nroads = kept;
    }
    // Close the batch: drop dead roads, and file long weights under the edges' final places.
    u32 rd = first, wr = first;
    for (u32 h = lo; h < hi; h++) {
      u32 kept = 0;
      for (u32 i = 0, d = z->degree[h]; i < d; i++, rd++) {
        u32 e = z->edges[rd];
        if (e == DEAD)
          continue;
        if (UNLIKELY((e & WMAX) == WMAX))
          heavy_put(ZONE_KIND + (z - zones), wr, z->long_edge[rd - first]);
        z->edges[wr++] = e, kept++;
      }
      z->degree[h] = kept;
    }
    z->edge_count = wr;
    lo = hi;
  }
}

static void zones_job(int t) {
  Zone *z = &zones[t];
  Road *roads = small_region(WALKS_MAX * sizeof(Road));
  z->long_edge = small_region(WALKS_MAX * sizeof(u64));
  z->order = small_region(n * 4ul), z->edges = small_region(m * 4ul), z->degree = small_region(n);
  for (u32 x = (u64)n * t / workers, end = (u64)n * (t + 1) / workers; x < end; x++)
    if (!(state[x] & (LEAF | TAKEN)))
      grow_zone(z, x, roads);
}

// ---- final graph: rows of head << 11 | weight, in the new ids.

static u32 new_n, new_m;
static u32 *new_id, *fwd_off, *fwd_adj, *rev_off, *rev_adj;

static void ids_job(int t) {
  const Zone *z = &zones[t];
  for (u32 i = 0; i < z->nodes; i++)
    new_id[z->order[i]] = z->id_base + i;
}

static void rename_job(int t) {
  const Zone *z = &zones[t];
  u32 pos = 0, at = z->edge_base, id = z->id_base;
  for (u32 i = 0; i < z->nodes; i++, id++) {
    fwd_off[id] = at;
    for (u32 d = z->degree[i]; d--; pos++, at++) {
      fetch(&new_id[z->edges[pos + 24] >> 11]);
      u32 e = z->edges[pos];
      fwd_adj[at] = new_id[e >> 11] << 11 | (e & WMAX);
      if (UNLIKELY((e & WMAX) == WMAX))
        heavy_put(FWD_KIND, at, heavy_get(ZONE_KIND + t, pos));
    }
  }
}

// The reverse rows come from the forward rows, slice by slice of ids: edges into a worker's own slice need no
// atomics, and the few that cross slices wait in a list and are applied with atomics afterwards.
typedef struct __attribute__((aligned(64))) {
  u32 *tail, *edge, count;
} Crossing;
static Crossing *crossings;

static void slice(int t, u32 *lo, u32 *hi) {
  *lo = (u64)new_n * t / workers, *hi = (u64)new_n * (t + 1) / workers;
}

static void rev_count_job(int t) {
  u32 lo, hi;
  slice(t, &lo, &hi);
  Crossing *c = &crossings[t];
  c->tail = small_region(new_m * 4ul), c->edge = small_region(new_m * 4ul);
  for (u32 u = lo; u < hi; u++)
    for (u32 i = fwd_off[u], end = fwd_off[u + 1]; i < end; i++) {
      u32 h = fwd_adj[i] >> 11;
      if (h - lo < hi - lo)
        rev_off[h + 2]++;
      else
        c->tail[c->count] = u, c->edge[c->count++] = i;
    }
}

static void rev_cross_count_job(int t) {
  const Crossing *c = &crossings[t];
  for (u32 i = 0; i < c->count; i++)
    BUMP(&rev_off[(fwd_adj[c->edge[i]] >> 11) + 2], 1);
}

INLINE void rev_put(u32 slot, u32 u, u32 i) {
  u32 w = fwd_adj[i] & WMAX;
  rev_adj[slot] = u << 11 | w;
  if (UNLIKELY(w == WMAX))
    heavy_put(REV_KIND, slot, heavy_get(FWD_KIND, i));
}

static void rev_place_job(int t) {
  u32 lo, hi;
  slice(t, &lo, &hi);
  for (u32 u = lo; u < hi; u++)
    for (u32 i = fwd_off[u], end = fwd_off[u + 1]; i < end; i++) {
      u32 h = fwd_adj[i] >> 11;
      if (h - lo < hi - lo)
        rev_put(rev_off[h + 1]++, u, i);
    }
}

static void rev_cross_place_job(int t) {
  const Crossing *c = &crossings[t];
  for (u32 i = 0; i < c->count; i++)
    rev_put(BUMP(&rev_off[(fwd_adj[c->edge[i]] >> 11) + 1], 1), c->tail[i], c->edge[i]);
}

// ---- queries: bidirectional search with 16-bit labels in blocks handed out in order, so a thread's label
// memory follows its deepest search, not the whole map. A search that cannot prove an answer below 65535 is
// redone by a plain 64-bit Dijkstra. Queue: one bucket per 128 of distance.

enum { WIDTH_BITS = 7, BUCKETS = 512, NODE_BITS = 21 };
enum { BLOCK_BITS = 8, BLOCK = 1 << BLOCK_BITS };
#define NODE_MASK ((1u << NODE_BITS) - 1)
#define FAR 65535u

typedef struct {
  u32 *entry, count, cap;
} Bucket;

typedef struct {
  Bucket bucket[BUCKETS];
  u32 cur, queued;
  u16 *dir, *used, *labels; // block 0 stays blank: a read of a node this side has not seen finds FAR
  u32 nused, top;
} Front;

typedef struct {
  Front front[2];
  u32 *spare[32];
  u8 *pool;
  u64 *improved; // what one pass over a bucket improved, pushed after the pass: most scanned edges push nothing
} Searcher;

static __attribute__((noinline)) void bucket_release(Searcher *s, Bucket *b) {
  int k = __builtin_ctz(b->cap);
  *(u32 **)b->entry = s->spare[k];
  s->spare[k] = b->entry;
  b->cap = 0, b->count = 0;
}

static __attribute__((noinline)) void bucket_grow(Searcher *s, Bucket *b) {
  u32 cap = b->cap ? b->cap * 2 : 16;
  int k = __builtin_ctz(cap);
  u32 *fresh = s->spare[k];
  if (fresh)
    s->spare[k] = *(u32 **)fresh;
  else
    fresh = (u32 *)s->pool, s->pool += (cap + 16) * 4ul;
  if (b->cap) {
    memcpy(fresh, b->entry, b->count * 4ul);
    u32 count = b->count;
    bucket_release(s, b);
    b->count = count;
  }
  b->entry = fresh, b->cap = cap;
}

static __attribute__((noinline)) u32 new_block(Front *f, u32 v) {
  u32 at = f->top++;
  memset(f->labels + ((size_t)at << BLOCK_BITS), 0xFF, BLOCK * 2);
  f->used[f->nused++] = v >> BLOCK_BITS;
  f->dir[v >> BLOCK_BITS] = at;
  return at;
}

INLINE u32 seen(const Front *f, u32 v) {
  return f->labels[(size_t)f->dir[v >> BLOCK_BITS] << BLOCK_BITS | (v & (BLOCK - 1))];
}

INLINE u16 *label(Front *f, u32 v) {
  u32 at = f->dir[v >> BLOCK_BITS];
  if (UNLIKELY(!at))
    at = new_block(f, v);
  return &f->labels[(size_t)at << BLOCK_BITS | (v & (BLOCK - 1))];
}

INLINE void front_push(Searcher *s, Front *f, u32 nd, u32 v) {
  Bucket *q = &f->bucket[nd >> WIDTH_BITS];
  if (UNLIKELY(q->count == q->cap))
    bucket_grow(s, q);
  q->entry[q->count++] = v | (nd & ((1u << WIDTH_BITS) - 1)) << NODE_BITS;
}

INLINE int front_advance(Front *f) {
  if (!f->queued)
    return 0;
  while (!f->bucket[f->cur].count)
    f->cur++;
  return 1;
}

INLINE void drain(Searcher *s, int side, const u32 *off, const u32 *adj, u32 kind, u32 *best) {
  Front *f = &s->front[side], *other = &s->front[1 - side];
  Bucket *cb = &f->bucket[f->cur];
  u32 base = f->cur << WIDTH_BITS, floor_other = other->cur << WIDTH_BITS, low = *best;
  for (u32 k = 0; k < cb->count;) {
    const u32 *p = cb->entry;
    u32 pass_end = cb->count, improved = 0;
    for (; k < pass_end; k++) {
      fetch(&off[p[k + 8] & NODE_MASK]);
      fetch(&adj[off[p[k + 4] & NODE_MASK]]);
      u32 u = p[k] & NODE_MASK, d = base | p[k] >> NODE_BITS;
      if (seen(f, u) != d || d + floor_other >= low)
        continue;
      for (u32 i = off[u], end = off[u + 1]; i < end; i++) {
        u32 e = adj[i], v = e >> 11, w = e & WMAX;
        if (UNLIKELY(w == WMAX)) {
          u64 heavy = heavy_get(kind, i);
          w = heavy < FAR ? heavy : FAR;
        }
        u32 nd = d + w;
        nd = nd < FAR ? nd : FAR;
        u16 *l = label(f, v);
        u32 old = *l, take = nd < old, lower = -take;
        // never a branch: this comparison goes either way about half the time
        *l = (nd & lower) | (old & ~lower);
        s->improved[improved] = (u64)nd << 32 | v;
        improved += take;
      }
    }
    for (u32 i = 0; i < improved; i++) {
      u32 nd = s->improved[i] >> 32, v = (u32)s->improved[i], meet = nd + seen(other, v);
      low = meet < low ? meet : low;
      front_push(s, f, nd, v);
    }
    f->queued += improved;
  }
  *best = low;
  f->queued -= cb->count;
  bucket_release(s, cb);
}

static void search_reset(Searcher *s) {
  for (int side = 0; side < 2; side++) {
    Front *f = &s->front[side];
    for (u32 b = f->cur; b < BUCKETS; b++)
      if (f->bucket[b].cap)
        bucket_release(s, &f->bucket[b]);
    f->cur = f->queued = 0;
    for (u32 i = 0; i < f->nused; i++)
      f->dir[f->used[i]] = 0;
    f->nused = 0, f->top = 1;
  }
}

INLINE u64 fwd_weight(u32 i) {
  u64 w = fwd_adj[i] & WMAX;
  return w == WMAX ? heavy_get(FWD_KIND, i) : w;
}

// The rare query whose answer does not fit: forward only, with a heap of distance << 32 | node.
static u64 wide_search(u32 src, u32 dst) {
  u64 *dist = small_region(new_n * 8ul); // distance + 1; 0 means no label yet
  u128 *heap = small_region(new_m * 16ul);
  u32 size = 1;
  dist[src] = 1, heap[0] = src;
  while (size) {
    u128 top = heap[0], last = heap[--size];
    u32 u = (u32)top, hole = 0;
    u64 d = top >> 32;
    for (u32 c; (c = 2 * hole + 1) < size; hole = c) {
      c += c + 1 < size && heap[c + 1] < heap[c];
      if (heap[c] >= last)
        break;
      heap[hole] = heap[c];
    }
    heap[hole] = last;
    if (u == dst)
      break;
    if (dist[u] != d + 1)
      continue;
    for (u32 i = fwd_off[u]; i < fwd_off[u + 1]; i++) {
      u32 v = fwd_adj[i] >> 11, at = size;
      u64 nd = d + fwd_weight(i);
      if (dist[v] - 1 <= nd) // an unlabelled node wraps to the largest value
        continue;
      dist[v] = nd + 1, size++;
      u128 item = (u128)nd << 32 | v;
      for (u32 up; at && heap[up = (at - 1) / 2] > item; at = up)
        heap[at] = heap[up];
      heap[at] = item;
    }
  }
  u64 answer = dist[dst] - 1; // INF when never reached
  madvise(dist, new_n * 8ul, MADV_DONTNEED);
  madvise(heap, new_m * 16ul, MADV_DONTNEED);
  return answer;
}

// The distance from src to dst if it is below limit, else INF.
static u64 search(Searcher *s, u32 src, u32 dst, u64 limit) {
  if (src == dst)
    return 0;
  if (fwd_off[src] == fwd_off[src + 1] || rev_off[dst] == rev_off[dst + 1])
    return INF;
  if (rev_off[src] == rev_off[src + 1]) {
    // No path returns to src, so its distance is the best first edge plus a search from there. These sources
    // have first edges near 2^32 (spec section 5), which would otherwise send a long way to the 64-bit search.
    u64 shortest = limit, lowest = INF;
    for (u32 i = fwd_off[src]; i < fwd_off[src + 1]; i++)
      lowest = fwd_weight(i) < lowest ? fwd_weight(i) : lowest;
    for (int pass = 0; pass < 2; pass++) // a search only to beat what the lightest edge gave: usually no search
      for (u32 i = fwd_off[src]; i < fwd_off[src + 1]; i++) {
        u64 w = fwd_weight(i);
        if (pass ? w < shortest : w == lowest) {
          u64 rest = search(s, fwd_adj[i] >> 11, dst, shortest - w);
          shortest = rest != INF ? w + rest : shortest;
        }
      }
    return shortest < limit ? shortest : INF;
  }
  u32 best = limit < 2 * FAR ? limit : 2 * FAR;
  *label(&s->front[0], src) = 0, *label(&s->front[1], dst) = 0;
  front_push(s, &s->front[0], 0, src), front_push(s, &s->front[1], 0, dst);
  s->front[0].queued = s->front[1].queued = 1;
  while (front_advance(&s->front[0]) && front_advance(&s->front[1]) &&
         (s->front[0].cur + s->front[1].cur) << WIDTH_BITS < best) {
    if (s->front[0].queued <= s->front[1].queued)
      drain(s, 0, fwd_off, fwd_adj, FWD_KIND, &best);
    else
      drain(s, 1, rev_off, rev_adj, REV_KIND, &best);
  }
  search_reset(s);
  if (best < FAR)
    return best < limit ? best : INF;
  if (limit <= FAR)
    return INF;
  u64 wide = wide_search(src, dst);
  return wide < limit ? wide : INF;
}

static u32 query_count, query_next, *ends;
static u64 *answers;

static void query_job(int t) {
  Searcher *s = small_region(sizeof(Searcher));
  s->pool = small_region(1ul << 26);
  s->improved = small_region(new_m * 8ul);
  for (int side = 0; side < 2; side++) {
    Front *f = &s->front[side];
    f->dir = small_region((new_n / BLOCK + 2) * 2ul);
    f->used = small_region((new_n / BLOCK + 2) * 2ul);
    f->labels = small_region(((size_t)new_n + 2 * BLOCK) * 2);
    memset(f->labels, 0xFF, BLOCK * 2);
    f->top = 1;
  }
  for (u32 k; (k = BUMP(&query_next, 1)) < query_count;)
    answers[k] = search(s, ends[2 * k], ends[2 * k + 1], INF);
}

static void release_zones_job(int t) {
  Zone *z = &zones[t];
  madvise(z->order, n * 4ul, MADV_DONTNEED);
  madvise(z->edges, m * 4ul, MADV_DONTNEED);
}

int main(void) {
  const char *pg = "/data/graph.txt", *pq = "/data/queries.txt", *po = "/out/distances.txt";
  cpu_set_t cpus;
  sched_getaffinity(0, sizeof cpus, &cpus);
  workers = CPU_COUNT(&cpus);
  size_t glen, qlen;
  const u8 *q = map_file(pq, &qlen), *qend = q + qlen, *g = map_file(pg, &glen);
  body_end = g + glen;
  n = read_u32(&g), m = read_u32(&g);
  body = g;
  small_top = reserve(1ul << 38, MADV_NOHUGEPAGE);
  heavy_table = small_region(TABLE_SLOTS * sizeof *heavy_table);
  ends = small_region(qlen * 4 + 64);
  state = small_region(n + 64);
  while (q < qend) {
    u32 s = read_u32(&q), t = read_u32(&q);
    ends[2 * query_count] = s, ends[2 * query_count + 1] = t, query_count++;
    state[s] = state[t] = QUERIED;
  }
  // Bins: a grid of cells (bucket, piece), each with room for the expected fill and 8 standard deviations.
  buckets = ((n - 1) >> BUCKET_BITS) + 1;
  u32 mean = m / PIECES / buckets + 1, root = 1;
  while (root * root < mean)
    root++;
  cell_bytes = (mean + 8 * root + 16) * REC;
  size_t rows_bytes = 16ul * n, bins_bytes = cell_bytes * PIECES * buckets;
  size_t rows_end = (rows_bytes + PAGE2M - 1) & -PAGE2M;
  u8 *arena = reserve(1ul << 28, MADV_HUGEPAGE) + PAGE2M;
  arena = (u8 *)((uintptr_t)arena & -PAGE2M);
  rows = (u32 *)arena, bins = arena + PAGE2M;
  cell_fill = small_region(buckets * PIECES * 4ul);
  rare_edges = small_region(m * sizeof(InputEdge));
  spill = small_region(m * 4ul);
  row_work = small_region(workers * sizeof(RowWork *));
  zones = small_region(workers * sizeof(Zone));
  crossings = small_region(workers * sizeof(Crossing));
  for (int t = 0; t < workers; t++)
    row_work[t] = small_region(sizeof(RowWork));
  for (intptr_t t = 1; t < workers; t++) {
    pthread_t id;
    pthread_create(&id, 0, worker_main, (void *)t);
  }
  touch_pages(arena, PAGE2M + bins_bytes);
  all_workers(parse_job);
  for (u32 b = 0; b < buckets; b += ROUND) {
    round_next = b, round_end = b + ROUND < buckets ? b + ROUND : buckets;
    all_workers(rows_job);
  }
  if (rows_end < PAGE2M + bins_bytes)
    madvise(arena + rows_end, PAGE2M + bins_bytes - rows_end, MADV_DONTNEED);
  all_workers(zones_job);
  for (int t = 0; t < workers; t++) {
    zones[t].id_base = new_n, zones[t].edge_base = new_m;
    new_n += zones[t].nodes, new_m += zones[t].edge_count;
  }
  madvise(spill, m * 4ul, MADV_DONTNEED);
  madvise(state, n, MADV_DONTNEED);
  // Final layout, all in one run of huge pages: forward rows, then the id map; the reverse rows replace the map.
  fwd_off = (u32 *)arena, fwd_adj = fwd_off + new_n + 2;
  new_id = fwd_adj + new_m;
  all_workers(ids_job);
  all_workers(rename_job);
  fwd_off[new_n] = new_m;
  for (u32 i = 0; i < 2 * query_count; i++)
    ends[i] = new_id[ends[i]];
  all_workers(release_zones_job);
  rev_off = new_id, rev_adj = rev_off + new_n + 2;
  memset(rev_off, 0, (new_n + 2ul) * 4);
  size_t all_end = (u8 *)(rev_adj + new_m) - arena;
  touch_pages(arena + rows_end, all_end - rows_end);
  all_workers(rev_count_job);
  all_workers(rev_cross_count_job);
  for (u32 i = 2; i < new_n + 2; i++)
    rev_off[i] += rev_off[i - 1];
  all_workers(rev_place_job);
  all_workers(rev_cross_place_job);
  answers = small_region(query_count * 8ul + 64);
  all_workers(query_job);
  char *w = (char *)small_region(query_count * 21ul + 64) + query_count * 21ul;
  const char *end = w;
  for (u32 i = query_count; i--;) {
    u64 v = answers[i];
    *--w = '\n';
    if (v == INF)
      *--w = '1', *--w = '-';
    else
      do
        *--w = '0' + v % 10;
      while (v /= 10);
  }
  long wrote = write(open(po, O_WRONLY | O_CREAT | O_TRUNC, 0644), w, end - w);
  _exit(wrote != end - w);
}
