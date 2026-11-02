// Exact directed shortest-path lengths for 2,000 source-target queries on a
// road-like graph of 1.7 M nodes and 5 M edges, as fast and as small as
// possible. The clock covers the whole process, so the build matters as much as
// the queries.
//
// Phases, in order. All but the output run on all workers, between spin
// barriers:
// 1. Parse: read the edge file in 48 pieces, pack edges into a grid of cells.
// 2. First rows: four edge slots per node, written over the finished cells.
// 3. Zones: breadth-first zones that contract roads while they order nodes.
// 4. Final rows: forward and reverse rows in the new, zone-ordered ids.
// 5. Queries: bidirectional search on 16-bit labels, 64-bit redo on overflow.
// 6. Output: all answers in one write, then _exit.
//
// Three ideas the rest of the file leans on:
// - Road contraction during the breadth-first pass: a node with at most two
//   ways out and no query on it is walked through when only one way is not the
//   way the walk came from. With two such ways it stays a real node.
// - 4-byte edges with a small table for large weights: head and an 11-bit
//   weight fit 32 bits, and the rare larger weight waits in a lock-free hash
//   table.
// - 16-bit labels with a 64-bit redo: a bidirectional search that can prove the
//   answer below 65,535 needs no 64-bit distances anywhere.
#define _GNU_SOURCE
#include <fcntl.h>
#include <immintrin.h>
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

#define BUMP(x, by) __atomic_fetch_add(x, by, __ATOMIC_RELAXED)
#define UNLIKELY(c) __builtin_expect(!!(c), 0)
#define INLINE static inline __attribute__((always_inline))
#define PAGE2M (1ul << 21)
#define INF UINT64_MAX

// Prefetch into all cache levels.
INLINE void fetch(const void *p) { _mm_prefetch(p, _MM_HINT_T0); }

// ---- worker pool: threads wait spinning between phases, so a phase starts in
// about a microsecond.

static int workers;
static void (*volatile phase_fn)(int);
static u32 phase_no, phase_left;

// Spin with a pause instruction most of the time; a yield only every 1024
// rounds, so the common short wait costs no syscall and a long one still yields
// the core.
static void rest(u32 *spins) {
  if (++*spins & 1023)
    _mm_pause();
  else
    sched_yield();
}

// Worker loop: wait for a new phase number, run it, count down the finishers.
// Each worker remembers the last phase it saw, so the barrier is one counter.
static int worker_main(void *arg) {
  for (u32 seen = 0, spins = 0;; seen++) {
    while (__atomic_load_n(&phase_no, __ATOMIC_ACQUIRE) == seen)
      rest(&spins);
    phase_fn((int)(intptr_t)arg);
    __atomic_fetch_sub(&phase_left, 1, __ATOMIC_RELEASE);
  }
}

// Publish the phase, join in as worker 0, then wait for the others to count
// down.
static void all_workers(void (*fn)(int)) {
  phase_fn = fn;
  phase_left = workers - 1;
  __atomic_store_n(&phase_no, phase_no + 1, __ATOMIC_RELEASE);
  fn(0);
  u32 spins = 0;
  while (__atomic_load_n(&phase_left, __ATOMIC_ACQUIRE))
    rest(&spins);
}

// ---- memory: big arrays on huge pages, each page faulted by one thread; small
// sparse ones on 4 KB pages.

// Map memory that costs nothing until touched; the advice picks huge or small
// pages per caller.
static u8 *reserve(size_t bytes, int advice) {
  u8 *p = mmap(0, bytes, PROT_READ | PROT_WRITE,
               MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
  madvise(p, bytes, advice);
  return p;
}

// Bump allocator over 4 KB pages for the many scratch arrays: no bookkeeping,
// and only the pages a worker touches cost memory. Where it matters, pages go
// back with madvise.
static u8 *small_top;
static void *small_region(size_t bytes) {
  return (void *)BUMP(&small_top, (bytes + 4095) & -4096ul);
}

static u8 *touch_from;
static u32 touch_next, touch_count;
// One store per huge page, claimed from a counter, so faulting a large array is
// parallel work and each page is touched once by one thread.
static void touch_job(int t) {
  for (u32 i; (i = BUMP(&touch_next, 1)) < touch_count;)
    *(volatile u8 *)(touch_from + i * PAGE2M) = 0;
}
// Fault one address range in, a huge page at a time, on all workers.
static void touch_pages(u8 *from, size_t bytes) {
  touch_from = from, touch_next = 0;
  touch_count = (bytes + PAGE2M - 1) / PAGE2M;
  all_workers(touch_job);
}

// ---- weights too large for an edge's 11-bit field: one lock-free hash table
// for every kind of edge index.

enum { RAW_KIND = 1, FWD_KIND, REV_KIND, ZONE_KIND, TABLE_SLOTS = 1 << 15 };
// Key 0 marks an empty slot, which is why the kinds start at 1.
static struct {
  u64 key, value;
} *heavy_table;

// Fibonacci hashing on the kind-and-index key; the shift keeps the top 15 bits,
// which is all 32,768 slots need.
INLINE u64 heavy_slot(u64 key) { return (key * 0x9E3779B97F4A7C15ull) >> 49; }

// Lock-free insert with linear probing. The table runs about half full at this
// scale (see PERF.md), so probes stay short. The kind bits in the key let every
// edge layout share one table. There is no full check: more keys than slots
// would spin here forever, which the README lists as a limit.
static void heavy_put(u32 kind, u64 index, u64 value) {
  u64 key = (u64)kind << 48 | index;
  for (u64 i = heavy_slot(key);; i = (i + 1) % TABLE_SLOTS) {
    u64 empty = 0;
    if (__atomic_compare_exchange_n(&heavy_table[i].key, &empty, key, 0,
                                    __ATOMIC_RELAXED, __ATOMIC_RELAXED)) {
      heavy_table[i].value = value;
      return;
    }
  }
}

// Read side; reached only for an edge whose weight field holds the WMAX
// sentinel, so the common edge never pays a table lookup. The probe ends
// because every key is put in an earlier phase than the one that reads it: raw
// weights in the rows, zone edges in the zones, forward rows in the rename, and
// reverse rows before the queries.
static u64 heavy_get(u32 kind, u64 index) {
  u64 key = (u64)kind << 48 | index, i = heavy_slot(key);
  while (heavy_table[i].key != key)
    i = (i + 1) % TABLE_SLOTS;
  return heavy_table[i].value;
}

// ---- input

// 48 pieces keep every worker busy to the end; REC is the 6-byte packed record
// a parse piece appends to a cell.
enum { PIECES = 48, REC = 6 };
// Buckets of 8,192 nodes: the first rows are built one bucket at a time, into
// memory the parse cells of earlier buckets have just released.
enum { BUCKET_BITS = 13, BUCKET_NODES = 1 << BUCKET_BITS };
#define ALL_ONES 0xFFFFFFFFu
// A row's fourth slot at or above this indexes the spill area (node ids stay
// below).
#define SPILLED 0xE0000000u
// The largest weight an 11-bit field holds; an edge at exactly this value means
// the real weight is in the heavy table.
#define WMAX 2047u

// Holds only the rare edges whose weight does not fit a parse record's 14-bit
// field; they join their buckets in the rows phase.
typedef struct {
  u32 u, v, w;
} InputEdge;

static const u8 *body, *body_end;
static u32 n, m, buckets;
static size_t cell_bytes;
static u8 *bins;
static u32 *cell_fill;
static InputEdge *rare_edges;
static u32 rare_count, piece_next, query_count, *ends;
// One byte per node: a shared line is read once for all three flags, and can
// be prefetched.
static u8 *state;
enum { LEAF = 1, TAKEN = 2, QUERIED = 4 };

// Map the file read-only with anonymous padding pages before and after it: the
// vector loads below read up to 64 bytes past a block and up to 8 bytes before
// a number, and the padding keeps those reads inside mapped memory.
static const u8 *map_file(const char *path, size_t *len) {
  int fd = open(path, O_RDONLY);
  // A missing input ends the run with its own exit code rather than a fault.
  if (fd < 0)
    _exit(3);
  *len = lseek(fd, 0, SEEK_END);
  u8 *p =
      mmap(0, *len + 3 * 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  mmap(p + 4096, *len, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0);
  return p + 4096;
}

// Small decimal reader: digits, then the one separator after them. Used for the
// header and for a third number too long for the vector path.
static u32 read_u32(const u8 **p) {
  u32 x = 0;
  for (; **p >= '0'; ++*p)
    x = x * 10 + **p - '0';
  ++*p;
  return x;
}

// A piece begins at the first line boundary after an even 1/48 split of the
// file, so no line is ever shared between two pieces and none of this needs a
// lock.
static const u8 *piece_start(int p) {
  const u8 *x = body + (body_end - body) * p / PIECES;
  while (x < body_end && x[-1] != '\n')
    x++;
  return x;
}

// One line, three numbers at once: each lane holds the 8 bytes ending at a
// separator, with the bytes before the number masked off, and is converted by
// the usual digit pairs, quads, eights.
INLINE void read_line(const u8 *a, const u32 *sp, u32 *out) {
  u32 l0 = sp[1] - sp[0] - 1, l1 = sp[2] - sp[1] - 1, l2 = sp[3] - sp[2] - 1;
  __m256i x =
      _mm256_set_epi64x(0, *(u64u *)(a + sp[3] - 8), *(u64u *)(a + sp[2] - 8),
                        *(u64u *)(a + sp[1] - 8));
  __m256i cut = _mm256_set_epi64x(64, 64 - 8 * l2, 64 - 8 * l1, 64 - 8 * l0);
  x = _mm256_and_si256(x, _mm256_sllv_epi64(_mm256_set1_epi8(0x0F), cut));
  x = _mm256_madd_epi16(_mm256_maddubs_epi16(x, _mm256_set1_epi16(0x010A)),
                        _mm256_set1_epi32(0x00010064));
  x = _mm256_add_epi64(_mm256_mul_epu32(x, _mm256_set1_epi64x(10000)),
                       _mm256_srli_epi64(x, 32));
  out[0] = _mm256_extract_epi32(x, 0), out[1] = _mm256_extract_epi32(x, 2),
  out[2] = _mm256_extract_epi32(x, 4);
  if (UNLIKELY(l2 > 8)) {
    const u8 *c = a + sp[2] + 1;
    out[2] = read_u32(&c);
  }
}

// Parse one piece: collect separator positions for a run of 64-byte blocks,
// convert whole lines with read_line, and append each edge to its bucket cell
// with no bounds check. A cell's room covers the expected fill plus eight
// standard deviations (the arithmetic is in main), which holds because the
// generator shuffles the edge lines.
static void parse_piece(int p, u8 **cursor) {
  const u8 *a = piece_start(p), *z = piece_start(p + 1), *block = a;
  u32 sp[64 * 32 + 32];
  int have = 1;
  sp[0] = ALL_ONES; // the separator before the first line, at offset -1
  u32 piece_bytes = z - a;
  for (u32 b = 0; b < buckets; b++)
    cursor[b] = bins + b * cell_bytes * PIECES + p * cell_bytes;
  // One cell per (bucket, piece) pair, so pieces of the same bucket never share
  // a cursor and the whole phase runs without locks.
  __m256i zero_char = _mm256_set1_epi8('0');
  while (block < z) {
    for (int j = 0; j < 64 && block < z; j++, block += 64) {
      __m256i x0 = _mm256_loadu_si256((const __m256i *)block),
              x1 = _mm256_loadu_si256((const __m256i *)(block + 32));
      u32 m0 = _mm256_movemask_epi8(_mm256_cmpgt_epi8(zero_char, x0));
      u64 mask =
          m0 | (u64)(u32)_mm256_movemask_epi8(_mm256_cmpgt_epi8(zero_char, x1))
                   << 32;
      // The separator masks of up to 64 blocks are flattened to positions in
      // one go. Twelve positions are always written per block, most blocks have
      // fewer, so the loop count does not mispredict; writes past the count are
      // overwritten by the next block.
      u32 base = block - a, c = __builtin_popcountll(mask), *o = sp + have;
      for (u32 i = 0; i < c; i += 12)
        for (int k = 0; k < 12; k++, mask = _blsr_u64(mask))
          o[i + k] = base + (u32)_tzcnt_u64(mask);
      have += c;
    }
    int i = 0;
    // Each line takes three separators. The piece_bytes guard stops at the
    // first line that starts past this piece, since blocks overrun its end.
    for (; i + 3 < have && sp[i] + 1 < piece_bytes; i += 3) {
      u32 e[3];
      read_line(a, sp + i, e);
      // Weights of 16,383 and above are kept out of the record's 14-bit field,
      // so the edge is parked here and joined to its bucket in the rows phase.
      if (UNLIKELY(e[2] >= 16383)) {
        rare_edges[BUMP(&rare_count, 1)] = (InputEdge){e[0], e[1], e[2]};
        continue;
      }
      u8 **c = &cursor[e[0] >> BUCKET_BITS];
      *(u64u *)*c = (e[0] & (BUCKET_NODES - 1)) | (u64)e[1] << BUCKET_BITS |
                    (u64)e[2] << 34;
      *c += REC;
    }
    // Positions not yet consumed move to the front, since a line can straddle
    // the boundary of a 64-byte block run.
    memmove(sp, sp + i, (have - i) * sizeof(u32));
    have -= i;
  }
  // Record how many records each cell holds, so the rows phase reads back
  // exactly what was written, then give the piece's file pages back: the pages
  // rebuild on demand if anything ever reads them again, and exit has nothing
  // left to tear down.
  for (u32 b = 0; b < buckets; b++)
    cell_fill[b * PIECES + p] =
        (cursor[b] - (bins + b * cell_bytes * PIECES + p * cell_bytes)) / REC;
  uintptr_t lo = ((uintptr_t)a + 4095) & -4096ul, hi = (uintptr_t)z & -4096ul;
  if (lo < hi)
    // real work now, in parallel, and none left for exit
    madvise((void *)lo, hi - lo, MADV_DONTNEED);
}

// Each worker's share of the query endpoints; marking them during the parse
// spreads the first touches of the state pages over all workers.
INLINE u32 ends_cut(int t) { return 2 * query_count * t / workers; }

// One worker's parse: its share of the endpoint marks, then pieces until none
// are left.
static void parse_job(int t) {
  // Endpoints must carry the QUERIED flag before the rows phase decides which
  // nodes are leaves, or a query node could be contracted away into a road.
  for (u32 i = ends_cut(t); i < ends_cut(t + 1); i++)
    state[ends[i]] = QUERIED;
  u8 *cursor[buckets];
  // Pieces are taken from a shared counter, so a slow piece delays no one.
  for (int p; (p = BUMP(&piece_next, 1)) < PIECES;)
    parse_piece(p, cursor);
}

// ---- first rows: four slots per node, in the memory the bins of earlier
// buckets just gave up.

// The first rows: four slots per node, plus a spill area for the nodes whose
// edges do not fit in four.
static u32 *rows, *spill;
static u32 spill_top = 1, round_next, round_end;

// Per-worker scratch while one bucket is being built: the edge count of every
// node, and flat staging lists for the fifth and later edges before they are
// spilled.
typedef struct {
  // A byte per node: a node with out-degree 256 or more would lose edges. The
  // README lists this as a limit; road-like graphs stay far below it.
  u8 fill[BUCKET_NODES];
  u32 next[BUCKET_NODES];
  // No bounds check: room for 65,536 edges a bucket; ten seeds peak at 321.
  u32 extra_low[1 << 16], extra_edge[1 << 16];
} RowWork;
static RowWork *row_work;

// The first four edges of a node go straight into its row; the rest are staged
// and spilled afterwards. A weight at or above WMAX stores the sentinel and the
// true value in the heavy table, keyed by tail and slot so the zones can find
// it.
INLINE void add_input_edge(u32 first, u32 *r, RowWork *k, int *extras, u32 low,
                           u32 v, u32 w) {
  u32 f = k->fill[low]++, e = v << 11 | (w < WMAX ? w : WMAX);
  if (UNLIKELY(w >= WMAX))
    heavy_put(RAW_KIND, (u64)(first + low) << 8 | f, w);
  if (f < 4)
    r[low * 4 + f] = e;
  else
    k->extra_low[*extras] = low, k->extra_edge[(*extras)++] = e;
}

// Build the first rows of one bucket from its parse cells and the rare-edge
// list. Empty slots start as ALL_ONES so a row with fewer than four edges needs
// no count byte: an ALL_ONES slot simply reads as no edge.
static void build_bucket(u32 b, RowWork *k) {
  u32 first = b << BUCKET_BITS,
      size = n - first < BUCKET_NODES ? n - first : BUCKET_NODES,
      *r = rows + 4ul * first;
  int extras = 0;
  memset(r, 0xFF, size * 16ul);
  memset(k->fill, 0, size);
  for (int p = 0; p < PIECES; p++) {
    const u8 *c = bins + b * cell_bytes * PIECES + p * cell_bytes;
    for (u32 i = 0, fill = cell_fill[b * PIECES + p]; i < fill; i++, c += REC) {
      u64 x = *(u64u *)c;
      add_input_edge(first, r, k, &extras, x & (BUCKET_NODES - 1),
                     x >> BUCKET_BITS & 0x1FFFFF, x >> 34 & 0x3FFF);
    }
  }
  for (u32 i = 0; i < rare_count; i++)
    if (rare_edges[i].u >> BUCKET_BITS == b)
      add_input_edge(first, r, k, &extras, rare_edges[i].u - first,
                     rare_edges[i].v, rare_edges[i].w);
  for (u32 low = 0; low < size; low++) {
    u32 f = k->fill[low], u = first + low;
    // never a branch: about half of all nodes are leaves
    state[u] |= (f <= 2) & !(state[u] & QUERIED);
    // A node with more than four edges: the fourth slot becomes a spill
    // pointer, the spill area starts with a count, and the row itself stays 16
    // bytes.
    if (UNLIKELY(f > 4)) {
      u32 pos = BUMP(&spill_top, f - 3 + 1);
      spill[pos] = f - 3, spill[pos + 1] = r[low * 4 + 3];
      r[low * 4 + 3] = SPILLED + pos;
      k->next[low] = pos + 2;
    }
  }
  // next[] holds each node's next free spill slot, set in the loop above.
  for (int i = 0; i < extras; i++)
    spill[k->next[k->extra_low[i]]++] = k->extra_edge[i];
}

// Buckets are claimed one at a time from a counter; a bucket is small enough
// that the balance stays good.
static void rows_job(int t) {
  for (u32 b; (b = BUMP(&round_next, 1)) < round_end;)
    build_bucket(b, &row_work[t]);
}

// ---- zones: each worker grows a breadth-first zone over the first rows. A
// step through a node with no real choice (out-degree at most 2, not a query
// endpoint) does not stop: the edge continues along the road to the next node
// with a choice, so contraction, ordering and gathering are one pass.

enum { FRONT_BATCH = 16 };
#define DEAD ALL_ONES

// One zone per worker: the nodes in BFS order, the edges they emit, and the
// bases that place this zone's ids and edges inside the global arrays. The
// struct is 64-byte aligned because two zones side by side on one cache line
// cost the phase a quarter to false sharing (31 -> 23.5 ms, see PERF.md).
typedef struct __attribute__((aligned(64))) {
  u32 *order, *edges;
  u8 *degree;
  u32 nodes, edge_count, batch_first;
  u64 *long_edge;
  u32 id_base, edge_base, rev_base, rev_total;
  u32 *cross_tail, *cross_edge, cross_count;
} Zone;

// A walk in progress along a road: the edge being filed, the node it began at,
// the node it last came from, the node it is at, hops so far, and the length
// gathered so far.
typedef struct {
  u32 pos, from, back, at, hops;
  u64 len;
} Road;

static Zone *zones;
static u32 seed_next;

// No atomics: locked operations on these lines took this phase 17.1 -> 23.5 ms.
// Two workers that race for a node both list it; heads all get the id written
// last, the other copy is a row no edge leads to, and is harmless.
INLINE void claim(Zone *z, u32 v) {
  if (!(state[v] & TAKEN)) {
    state[v] |= TAKEN;
    z->order[z->nodes++] = v;
  }
}

// The true weight of an edge slot; only the WMAX sentinel pays a table lookup.
INLINE u64 raw_weight(u32 tail, u32 f, u32 e) {
  u32 w = e & WMAX;
  return w < WMAX ? w : heavy_get(RAW_KIND, (u64)tail << 8 | f);
}

// File an edge with head and length packed into 32 bits. A length at or above
// WMAX stores the sentinel and the true 64-bit length in long_edge, indexed by
// the edge's place in the batch.
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
  // A leaf (at most two ways out, not a query endpoint) is walked through, as
  // long as the walk stays under 64 hops. The cap bounds a road that wanders or
  // loops without ever offering a choice; when it trips, the node simply stays
  // in the graph as a real node, so answers remain exact.
  if (state[at] & LEAF && r->hops++ < 64) {
    u32 ok0 = e0 != ALL_ONES && e0 >> 11 != r->back,
        ok1 = e1 != ALL_ONES && e1 >> 11 != r->back;
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
  // A node with a real choice, or a walk that ran out of hops: the node is
  // claimed and the road's edge ends here. A road that died is marked DEAD and
  // dropped when the batch closes.
  if (ways)
    claim(z, at), emit(z, r->pos, at, r->len);
  else
    z->edges[r->pos] = DEAD;
  return 0;
}

// Grow one zone: scan a batch of the frontier, then advance every road the
// batch started, one hop per round, so the cache misses of all the walks
// overlap, and finally close the batch.
static void grow_zone(Zone *z, u32 seed, Road *roads) {
  u32 lo = z->nodes;
  claim(z, seed);
  while (lo < z->nodes) {
    u32 hi = lo + FRONT_BATCH < z->nodes ? lo + FRONT_BATCH : z->nodes,
        nroads = 0;
    u32 first = z->batch_first = z->edge_count;
    // Prefetch, before the scan: the rows of the next nodes, twice as far ahead
    // as the batch, then the state bytes of the next batch's edge heads.
    for (u32 h = hi; h < z->nodes && h < hi + 2 * FRONT_BATCH; h++)
      fetch(rows + 4ul * z->order[h]);
    for (u32 h = hi; h < z->nodes && h < hi + FRONT_BATCH; h++)
      for (int f = 0; f < 4; f++)
        fetch(&state[rows[4ul * z->order[h] + f] >> 11]);
    for (u32 h = lo; h < hi; h++) {
      u32 u = z->order[h], before = z->edge_count;
      const u32 *row = rows + 4ul * u, *more = row;
      u32 inline_edges = 4, more_edges = 0;
      // A row whose fourth slot spilled reads the rest of its edges from the
      // spill area: a count first, then the extra edges after it.
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
        // An edge into a plain node is filed at once; an edge into a leaf
        // starts a road instead, to be resolved in the rounds below.
        if (!(state[head] & LEAF)) {
          claim(z, head);
          emit(z, pos, head, w);
        } else {
          roads[nroads++] = (Road){pos, u, u, head, 0, w};
        }
      }
      z->degree[h] = z->edge_count - before;
    }
    // Every road advances one hop per round and a finished road is dropped, so
    // the cache misses of all the walks overlap.
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
    // Close the batch: drop dead roads, and file long weights under the edges'
    // final places.
    u32 rd = first, wr = first;
    for (u32 h = lo; h < hi; h++) {
      u32 kept = 0;
      for (u32 i = 0, d = z->degree[h]; i < d; i++, rd++) {
        u32 e = z->edges[rd];
        if (e == DEAD)
          continue;
        // The squeeze moves edges down, so a long edge's heavy entry is filed
        // under the edge's final place in the zone's rows.
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

// One zone per worker. Seeds are claimed in 4,096-node chunks from a counter,
// so a stalled worker holds little of the map. The scratch arrays are sized to
// the whole graph because only touched pages cost memory.
static void zones_job(int t) {
  Zone *z = &zones[t];
  Road *roads = small_region(m * sizeof(Road));
  z->long_edge = small_region(m * sizeof(u64));
  z->order = small_region(n * 4ul), z->edges = small_region(m * 4ul),
  z->degree = small_region(n);
  for (u32 c; (c = BUMP(&seed_next, 4096)) < n;)
    for (u32 x = c, end = c + 4096 < n ? c + 4096 : n; x < end; x++)
      if (!(state[x] & (LEAF | TAKEN)))
        grow_zone(z, x, roads);
}

// ---- final graph: rows of head << 11 | weight, in the new ids.

// The contracted graph: nodes numbered in zone order, then forward rows
// (offsets plus packed edges) and reverse rows in the same layout.
static u32 new_n, new_m;
static u32 *new_id, *fwd_off, *fwd_adj, *rev_off, *rev_adj;

// New ids are the BFS order within each zone, offset by the zone's base; the
// prefetch warms the id map 16 writes ahead of the store.
static void ids_job(int t) {
  const Zone *z = &zones[t];
  for (u32 i = 0; i < z->nodes; i++) {
    fetch(&new_id[z->order[i + 16]]);
    new_id[z->order[i]] = z->id_base + i;
  }
}

// Write the forward rows in the new ids: heads renamed, heavy weights re-keyed
// from their zone-edge place to their forward-row place, and each node's row
// end recorded in fwd_off. Renaming in zone order keeps each worker's writes
// inside its own slice of ids.
static void rename_job(int t) {
  for (u32 i = ends_cut(t); i < ends_cut(t + 1); i++)
    ends[i] = new_id[ends[i]];
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

// The reverse rows come from the forward rows, slice by slice of ids: edges
// into a worker's own slice need no atomics, and the few that cross slices wait
// in a list and are applied with atomics afterwards. Count, per worker, the
// in-edges of its own id slice. rev_off holds the counts at id+2 during this
// phase and becomes the offsets in place later.
static void rev_count_job(int t) {
  u32 lo = zones[t].id_base, hi = lo + zones[t].nodes;
  Zone *c = &zones[t];
  c->cross_tail = small_region(new_m * 4ul);
  c->cross_edge = small_region(new_m * 4ul);
  memset(rev_off + lo + 2, 0, (hi - lo) * 4ul);
  for (u32 u = lo; u < hi; u++)
    for (u32 i = fwd_off[u], end = fwd_off[u + 1]; i < end; i++) {
      u32 h = fwd_adj[i] >> 11;
      if (h - lo < hi - lo)
        rev_off[h + 2]++;
      else
        c->cross_tail[c->cross_count] = u, c->cross_edge[c->cross_count++] = i;
    }
}

// The few crossing edges count towards the head's slice with one atomic each;
// a plain count would race the slice's owner.
static void rev_cross_count_job(int t) {
  const Zone *c = &zones[t];
  for (u32 i = 0; i < c->cross_count; i++)
    BUMP(&rev_off[(fwd_adj[c->cross_edge[i]] >> 11) + 2], 1);
}

// Each worker's own total, so the global prefix sum can be stitched from the
// zone bases in one pass.
static void rev_total_job(int t) {
  Zone *z = &zones[t];
  u32 total = 0;
  for (u32 i = z->id_base + 2, end = i + z->nodes; i < end; i++)
    total += rev_off[i];
  z->rev_total = total;
}

// Running sum over this worker's slice, starting from the global base its
// predecessors reached.
static void rev_sum_job(int t) {
  const Zone *z = &zones[t];
  u32 sum = z->rev_base;
  for (u32 i = z->id_base + 2, end = i + z->nodes; i < end; i++)
    rev_off[i] = sum += rev_off[i];
}

// One reverse edge: the tail's id with the weight, and the heavy entry re-keyed
// from the forward place to the reverse slot.
INLINE void rev_put(u32 slot, u32 u, u32 i) {
  u32 w = fwd_adj[i] & WMAX;
  rev_adj[slot] = u << 11 | w;
  if (UNLIKELY(w == WMAX))
    heavy_put(REV_KIND, slot, heavy_get(FWD_KIND, i));
}

// Place the edges that stay inside the worker's own slice; the entry at id+1 is
// a fill cursor while placing and an offset once the phase is done.
static void rev_place_job(int t) {
  u32 lo = zones[t].id_base, hi = lo + zones[t].nodes;
  for (u32 u = lo; u < hi; u++)
    for (u32 i = fwd_off[u], end = fwd_off[u + 1]; i < end; i++) {
      u32 h = fwd_adj[i] >> 11;
      if (h - lo < hi - lo)
        rev_put(rev_off[h + 1]++, u, i);
    }
}

// The crossing edges take their slot with an atomic fetch-add on the head's
// cursor, since that cursor is shared with the head's owner.
static void rev_cross_place_job(int t) {
  const Zone *c = &zones[t];
  for (u32 i = 0; i < c->cross_count; i++) {
    u32 edge = c->cross_edge[i];
    rev_put(BUMP(&rev_off[(fwd_adj[edge] >> 11) + 1], 1), c->cross_tail[i],
            edge);
  }
}

// ---- queries: bidirectional search with 16-bit labels in blocks handed out in
// order, so a thread's label memory follows its deepest search, not the whole
// map. A search that cannot prove an answer below 65535 is redone by a plain
// 64-bit Dijkstra. Queue: one bucket per 128 of distance.

// A queue entry packs 21 bits of node and 7 of distance remainder, so a pop
// recovers its exact distance from the entry and the bucket number. The
// contracted graph fits 21-bit ids.
enum { WIDTH_BITS = 7, BUCKETS = 512, NODE_BITS = 21 };
enum { BLOCK_BITS = 8, BLOCK = 1 << BLOCK_BITS };
#define NODE_MASK ((1u << NODE_BITS) - 1)
// A label of FAR means no distance below FAR was proved; the answer then needs
// the 64-bit search.
#define FAR 65535u

// One distance bucket: a recycled entry array plus its fill and capacity.
typedef struct {
  u32 *entry, count, cap;
} Bucket;

// One direction of a search: the bucket queue, the current bucket, how many
// entries are queued, and the 16-bit labels in 256-entry blocks.
typedef struct {
  Bucket bucket[BUCKETS];
  u32 cur, queued;
  // Block 0 stays blank: a read of a node this side has not seen finds FAR.
  u16 *dir, *used, *labels;
  u32 nused, top;
} Front;

// Both directions plus what they share: a pool of bucket memory, a spare list
// of freed arrays by size class so a grown bucket is reused rather than re-cut,
// and the improvement list of the pass in progress.
typedef struct {
  Front front[2];
  u32 *spare[32];
  u8 *pool;
  // What one pass over a bucket improved, pushed after the pass: most scanned
  // edges push nothing.
  u64 *improved;
} Searcher;

// Park a bucket's array on the spare list of its size class; the array's first
// word is the list link, so no extra memory is spent.
static __attribute__((noinline)) void bucket_release(Searcher *s, Bucket *b) {
  int k = __builtin_ctz(b->cap);
  *(u32 **)b->entry = s->spare[k];
  s->spare[k] = b->entry;
  b->cap = b->count = 0;
}

// Double a bucket's capacity, preferring a parked array of exactly that size
// and cutting fresh memory from the pool only when the size class is empty.
static __attribute__((noinline)) void bucket_grow(Searcher *s, Bucket *b) {
  u32 cap = b->cap ? b->cap * 2 : 16;
  int k = __builtin_ctz(cap);
  u32 *fresh = s->spare[k];
  if (fresh)
    s->spare[k] = *(u32 **)fresh;
  else
    // The 16 extra entries are the slack for the read-ahead in drain.
    fresh = (u32 *)s->pool, s->pool += (cap + 16) * 4ul;
  u32 count = b->count;
  if (count) {
    memcpy(fresh, b->entry, count * 4ul);
    bucket_release(s, b);
  }
  b->entry = fresh, b->cap = cap, b->count = count;
}

// Hand a fresh block of 256 labels, all FAR, to a node block this side has not
// seen; the block number is recorded so the reset clears only what was used.
static __attribute__((noinline)) u32 new_block(Front *f, u32 v) {
  u32 at = f->top++;
  memset(f->labels + ((size_t)at << BLOCK_BITS), 0xFF, BLOCK * 2);
  f->used[f->nused++] = v >> BLOCK_BITS;
  f->dir[v >> BLOCK_BITS] = at;
  return at;
}

// A node's label on this side, or FAR if its block was never handed out: the
// directory then gives block 0, which stays blank, so no check is needed.
INLINE u32 seen(const Front *f, u32 v) {
  return f->labels[(size_t)f->dir[v >> BLOCK_BITS] << BLOCK_BITS |
                   (v & (BLOCK - 1))];
}

// The address of a node's label, taking a block on first touch. Blocks are
// handed out in visit order, so the label memory follows the search's depth.
INLINE u16 *label(Front *f, u32 v) {
  u32 at = f->dir[v >> BLOCK_BITS];
  if (UNLIKELY(!at))
    at = new_block(f, v);
  return &f->labels[(size_t)at << BLOCK_BITS | (v & (BLOCK - 1))];
}

// Queue a node at its distance; the remainder bits ride along in the entry.
INLINE void front_push(Searcher *s, Front *f, u32 nd, u32 v) {
  Bucket *q = &f->bucket[nd >> WIDTH_BITS];
  if (UNLIKELY(q->count == q->cap))
    bucket_grow(s, q);
  q->entry[q->count++] = v | (nd & ((1u << WIDTH_BITS) - 1)) << NODE_BITS;
}

// Move to the next non-empty bucket, walking up from the current one. Returns 0
// when the side is exhausted.
INLINE int front_advance(Front *f) {
  if (!f->queued)
    return 0;
  while (!f->bucket[f->cur].count)
    f->cur++;
  return 1;
}

// Settle the current bucket of one side. The pass itself reads only the bucket
// and the labels and writes every improvement to a list; the pushes and the
// meeting checks run after the pass, on that list alone.
INLINE void drain(Searcher *s, int side, const u32 *off, const u32 *adj,
                  u32 kind, u32 *best) {
  Front *f = &s->front[side], *other = &s->front[1 - side];
  Bucket *cb = &f->bucket[f->cur];
  // base rebuilds an entry's exact distance from the bucket number; floor_other
  // is the least distance the other side could still produce, so an entry whose
  // distance already reaches the best meeting is dropped before its row is
  // read.
  u32 base = f->cur << WIDTH_BITS, floor_other = other->cur << WIDTH_BITS,
      low = *best;
  for (u32 k = 0; k < cb->count;) {
    const u32 *p = cb->entry;
    u32 pass_end = cb->count, improved = 0;
    for (; k < pass_end; k++) {
      // Prefetch the offsets 8 entries ahead and the row 4 ahead. Near the end
      // of the list these read past count: bucket_grow leaves 16 spare entries,
      // a stale value is masked to the node range, and that part of the arena
      // is mapped, so the worst case is a useless prefetch.
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
    // After the pass: push only what improved, and check only those against the
    // other side's label. An edge that improves nothing cannot make a new
    // meeting, because the later of the two label writes already saw the pair.
    for (u32 i = 0; i < improved; i++) {
      u32 nd = s->improved[i] >> 32, v = (u32)s->improved[i],
          meet = nd + seen(other, v);
      low = meet < low ? meet : low;
      front_push(s, f, nd, v);
    }
    f->queued += improved;
  }
  // The bucket's array goes back to the spare list and the queued count drops
  // by what this bucket held.
  *best = low;
  f->queued -= cb->count;
  bucket_release(s, cb);
}

// Undo one search: release every bucket that was used, clear the dir entries of
// the blocks that were handed out (block 0 goes back to meaning "unseen"), and
// restart block numbering at 1.
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

// The true weight of a forward edge, heavy table only for the sentinel.
INLINE u64 fwd_weight(u32 i) {
  u64 w = fwd_adj[i] & WMAX;
  return w == WMAX ? heavy_get(FWD_KIND, i) : w;
}

// The rare query whose answer does not fit: forward only, with a heap of
// distance << 32 | node.
static u64 wide_search(u32 src, u32 dst) {
  u64 *dist = small_region(new_n * 8ul); // distance + 1; 0 means no label yet
  // 128-bit heap items, distance above bit 32 and node below, so key and node
  // compare as one unit and the pop order is by distance.
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
    // A node can sit in the heap several times; only the entry that matches its
    // best distance settles.
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
  // These two arrays are the biggest per-query memory. Each wide query takes a
  // new range, so these pages are never used again and go back here.
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
    // No path returns to src, so its distance is the best first edge plus a
    // search from there. These sources have first edges near 2^32 (spec section
    // 2), which would otherwise send a long way to the 64-bit search.
    u64 shortest = limit, lowest = INF;
    for (u32 i = fwd_off[src]; i < fwd_off[src + 1]; i++)
      lowest = fwd_weight(i) < lowest ? fwd_weight(i) : lowest;
    // Pass 0 searches the lightest edge. Pass 1 tries every edge lighter than
    // the answer so far, the lightest again included, bounded by that answer.
    for (int pass = 0; pass < 2; pass++)
      for (u32 i = fwd_off[src]; i < fwd_off[src + 1]; i++) {
        u64 w = fwd_weight(i);
        if (pass ? w < shortest : w == lowest) {
          u64 rest = search(s, fwd_adj[i] >> 11, dst, shortest - w);
          shortest = rest != INF ? w + rest : shortest;
        }
      }
    return shortest < limit ? shortest : INF;
  }
  // best is capped at 2 * FAR: above that the two bucket floors cannot meet
  // inside 16-bit labels anyway, and the 64-bit search takes over.
  u32 best = limit < 2 * FAR ? limit : 2 * FAR;
  *label(&s->front[0], src) = 0, *label(&s->front[1], dst) = 0;
  front_push(s, &s->front[0], 0, src), front_push(s, &s->front[1], 0, dst);
  s->front[0].queued = s->front[1].queued = 1;
  // Expand the side with fewer queued entries, so a lopsided query does not
  // leave one side running alone, and stop once the two floors add up to the
  // best meeting found.
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

static u32 query_next;
static u64 *answers;

// Each worker builds one searcher, sized once, then takes queries from a
// counter. Label block 0 of each side stays blank and all FAR, so a read of a
// node that side has not seen returns FAR with no separate check.
static void query_job(int t) {
  Searcher *s = small_region(sizeof(Searcher));
  // 64 MB of address space for bucket arrays, with no bounds check: recycling
  // keeps a thread far below this. The README lists it as a limit.
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

// The zone arrays are dead once the rows are renamed; their pages are dropped
// before the reverse rows and query scratch arrive, keeping the peak flat.
static void release_zones_job(int t) {
  Zone *z = &zones[t];
  madvise(z->order, n * 4ul, MADV_DONTNEED);
  madvise(z->edges, m * 4ul, MADV_DONTNEED);
}

// The phases in order: each all_workers call is one phase, with the main thread
// as worker 0 and the serial work between phases kept as small as possible.
int main(void) {
  const char *pg = "/data/graph.txt", *pq = "/data/queries.txt",
             *po = "/out/distances.txt";
  // Worker count follows the affinity mask the harness set; every worker is
  // used for every phase.
  cpu_set_t cpus;
  sched_getaffinity(0, sizeof cpus, &cpus);
  workers = CPU_COUNT(&cpus);
  size_t glen, qlen;
  const u8 *q = map_file(pq, &qlen), *qend = q + qlen, *g = map_file(pg, &glen);
  body_end = g + glen;
  n = read_u32(&g), m = read_u32(&g);
  body = g;
  // A 256 GB reservation for the bump allocator: MAP_NORESERVE means it commits
  // nothing, so the size only has to be larger than anything the run touches.
  small_top = reserve(1ul << 38, MADV_NOHUGEPAGE);
  heavy_table = small_region(TABLE_SLOTS * sizeof *heavy_table);
  ends = small_region(qlen * 4 + 64);
  state = small_region(n + 64);
  // The queries are read first so the parse phase can mark their endpoints.
  while (q < qend) {
    u32 s = read_u32(&q), t = read_u32(&q);
    ends[2 * query_count] = s, ends[2 * query_count + 1] = t, query_count++;
  }
  // Bins: a grid of cells (bucket, piece), each with room for the expected fill
  // and 8 standard deviations.
  buckets = ((n - 1) >> BUCKET_BITS) + 1;
  u32 mean = m / PIECES / buckets + 1, root = 1;
  while (root * root < mean)
    root++;
  cell_bytes = (mean + 8 * root + 16) * REC;
  size_t rows_bytes = 16ul * n, bins_bytes = cell_bytes * PIECES * buckets;
  size_t rows_end = (rows_bytes + PAGE2M - 1) & -PAGE2M;
  // One huge-page arena: the rows start 2 MB before the bins, so a round of
  // buckets can write rows over bins that have already been read.
  u8 *arena = reserve(1ul << 28, MADV_HUGEPAGE) + PAGE2M;
  arena = (u8 *)((uintptr_t)arena & -PAGE2M);
  rows = (u32 *)arena, bins = arena + PAGE2M;
  cell_fill = small_region(buckets * PIECES * 4ul);
  rare_edges = small_region(m * sizeof(InputEdge));
  spill = small_region(m * 4ul);
  row_work = small_region(workers * sizeof(RowWork));
  zones = small_region(workers * sizeof(Zone));
  // Plain clone threads on 64 KB stacks: they need no thread library setup, and
  // _exit ends them all.
  int same_process =
      CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD;
  for (intptr_t t = 1; t < workers; t++)
    clone(worker_main, (u8 *)small_region(1 << 16) + (1 << 16), same_process,
          (void *)t);
  touch_pages(arena, PAGE2M + bins_bytes);
  all_workers(parse_job);
  for (u32 b = 0; b < buckets; b = round_end) {
    // Each round reaches as far as the rows can go without touching a bin not
    // yet read. That advances only while a bucket's bins are larger than its
    // rows (128 KB); at full scale they are about 198 KB.
    round_next = b,
    round_end = (PAGE2M + b * cell_bytes * PIECES) / (BUCKET_NODES * 16ul);
    round_end = round_end < buckets ? round_end : buckets;
    all_workers(rows_job);
  }
  // The bins and the row scratch are dead: their pages are dropped before the
  // zones allocate, so the peak does not stack the two phases.
  madvise(arena + rows_end, PAGE2M + bins_bytes - rows_end, MADV_DONTNEED);
  madvise(row_work, workers * sizeof(RowWork), MADV_DONTNEED);
  all_workers(zones_job);
  // Zone bases are assigned in worker order, so ids and edges land contiguously
  // and no later phase needs a scatter to place them.
  for (int t = 0; t < workers; t++) {
    zones[t].id_base = new_n, zones[t].edge_base = new_m;
    new_n += zones[t].nodes, new_m += zones[t].edge_count;
  }
  // The spill area and the state bytes are dead too; their pages are dropped.
  madvise(spill, m * 4ul, MADV_DONTNEED);
  madvise(state, n, MADV_DONTNEED);
  // Final layout, all in one run of huge pages: forward rows, then the id map;
  // the reverse rows replace the map.
  fwd_off = (u32 *)arena, fwd_adj = fwd_off + new_n + 2;
  new_id = fwd_adj + new_m;
  all_workers(ids_job);
  all_workers(rename_job);
  fwd_off[new_n] = new_m;
  all_workers(release_zones_job);
  rev_off = new_id, rev_adj = rev_off + new_n + 2;
  size_t all_end = (u8 *)(rev_adj + new_m) - arena;
  touch_pages(arena + rows_end, all_end - rows_end);
  rev_off[0] = rev_off[1] = 0;
  all_workers(rev_count_job);
  all_workers(rev_cross_count_job);
  all_workers(rev_total_job);
  for (int t = 1; t < workers; t++)
    zones[t].rev_base = zones[t - 1].rev_base + zones[t - 1].rev_total;
  all_workers(rev_sum_job);
  all_workers(rev_place_job);
  all_workers(rev_cross_place_job);
  answers = small_region(query_count * 8ul + 64);
  all_workers(query_job);
  // Digits are written back to front into one buffer, so the answers need no
  // second pass and the file is one write followed by _exit: nothing is flushed
  // or torn down at exit, because exit is timed too.
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
