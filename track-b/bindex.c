// bindex.c - Speed Demon track B: BOOL and PHRASE queries over docs.txt.
//
// Only the query terms are indexed. Each of the nw workers parses its share of
// queries.txt into one shared term table, then owns a contiguous stretch of the
// corpus cut into parts of at most 32,768 documents. Per part it records which
// documents hold each BOOL term (a bitmap row for the ~180 common terms, a
// sorted u16 list for the rest), answers every BOOL query on the part and keeps
// a record for each query that matched. Phrases are checked during the scan at
// each occurrence of their rarest term. Last, each worker merges every part's
// answers for the queries it parsed and formats their lines. No locks: the
// phases meet at barriers, and each worker writes only its own memory.
#define _GNU_SOURCE
#include <fcntl.h>
#include <immintrin.h>
#include <pthread.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

typedef uint8_t u8; typedef uint16_t u16; typedef uint32_t u32; typedef uint64_t u64;

#define NSUB 3            // parts per worker
#define MAXPART 32768     // docs per part, so a part-local doc number fits a u16
#define MAXTOK 65536      // tokens per doc; the spec's longest doc (999 units) has ~10,000
#define BBITS 15          // term table: 2^15 buckets of 5 keys
#define NSLOT (5u << BBITS)
#define SAMPLE_DIV 64     // each worker samples the first 1/64 of its first part
#define DENSE_DIV 16      // a BOOL term in over 1/16 of the sample gets a bitmap row
#define MAXQ 10           // ids printed per query
#define WMAX 64
#define ANCHOR (1u << 31) // table value flag: phrases are checked at this term
#define NONE 0xFFFFFFFFu

#ifdef TIMING
#include <time.h>
static double t0;
static double ms(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec * 1e3 + t.tv_nsec / 1e6; }
#define MARK(...) (fprintf(stderr, "%8.2f ", ms() - t0), fprintf(stderr, __VA_ARGS__))
#else
#define MARK(...) ((void)0)
#endif

static void die(const char *m) { fprintf(stderr, "bindex: %s\n", m); exit(1); }

enum { AND = 1, OR, NOT };
typedef struct { u32 q, gid; } Hit;            // phrase query q matches document gid
typedef struct {
    _Alignas(64) u64 *keys, *bm, *bt, *dm;     // aligned: no two workers share a cache line
    u8 *heap; size_t hsize;                    // the part arrays, unmapped before the merge
    u32 *ids, *aps, *sdf, *off, *end, *docoff, *rec, nrec, nsd, w64, nhit, hcap, cnt[3];
    u16 *la, *lb, *stream, *post;
    u8 *qdone;
    Hit *hit;
    char *out; size_t outlen;
} Worker;

static Worker wk[WMAX];
static int nw, P, ppw;                         // workers, parts, parts per worker
static const u8 *doc, *qtxt;
static size_t dlen, qlen, qs[WMAX + 1], *pstart;
static u32 ndocs, nq, ndense, nsparse, nterms, qcount[WMAX], qbase[WMAX + 1], *pfirst, *bound;
static u8 *qkind, *qk, *qop, bflag[NSLOT];     // query q: terms at qterm[8q..], op before term i at qop[8q+i]
static u32 *qterm, *sdfs, *sampdf, *newid, *aoff, *aq, *aj;
static size_t sdfsize;
static pthread_barrier_t bar;

static void *arena(size_t n) {
    void *p = mmap(NULL, n + 64, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE, -1, 0);
    if (p == MAP_FAILED) die("mmap");
    madvise(p, n + 64, MADV_NOHUGEPAGE);       // a huge page would charge its untouched rest to peak memory
    return p;
}

static void *huge(size_t n) {                  // n bytes on 2 MB pages
    u8 *p = mmap(NULL, n + (2u << 20), PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (p == MAP_FAILED) die("mmap");
    p = (u8 *)(((uintptr_t)p + (2u << 20) - 1) & ~(uintptr_t)((2u << 20) - 1));
    madvise(p, n, MADV_HUGEPAGE);
    return p;
}

static void *take(u8 **h, size_t n) { void *p = *h; *h += (n + 63) & ~(size_t)63; return p; }

static const u8 *map_file(const char *dir, const char *name, size_t *len) {
    char path[4096];
    snprintf(path, sizeof path, "%s/%s", dir, name);
    int fd = open(path, O_RDONLY);
    struct stat st;
    if (fd < 0 || fstat(fd, &st)) die(path);
    *len = (size_t)st.st_size;
    // a zero page after the file keeps the 64-byte loads past its end in bounds
    u8 *b = mmap(NULL, *len + 4096, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (b == MAP_FAILED || (*len && mmap(b, *len, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0) == MAP_FAILED)) die(path);
    close(fd);
    return b;
}

// ---------------------------------------------------------------- term table
// A token's key is exact up to 8 bytes: the lowercased bytes as a little-endian
// u64 (no token holds a NUL, so different lengths differ). Longer tokens XOR in
// each further 8 bytes times an odd constant, so two tokens share a key only by
// a 64-bit coincidence. A bucket holds 5 keys and their values in one cache
// line. Its slots fill in order, and ovf marks a bucket a key was probed past,
// so a search ends at the first bucket that holds the key or has no ovf: almost
// always the home bucket, so the branch is predictable. A slot's value is its
// own index until the workers renumber the terms.
typedef struct { _Alignas(64) u64 key[5]; u32 val[5], ovf; } Bucket;
static Bucket *tab;                            // on one huge page: lookups take no TLB misses
#define BMASK ((1u << BBITS) - 1)

static inline u32 bucket_of(u64 k) { return (u32)((k * 0x9E3779B97F4A7C15ull) >> (64 - BBITS)); }

static inline u64 ld64(const u8 *p) { u64 x; memcpy(&x, p, 8); return x; }
#define LOWER 0x2020202020202020ull            // on token bytes, | 0x20 lowercases A-Z and keeps 0-9, a-z

// KM[L]: the masks of a token's first and second 8 bytes, for L <= 16: the
// same key as key_at with two ANDs in place of the length arithmetic.
static u64 KM[17][2];

static inline u64 key_at(const u8 *p, u32 L) {
    u32 a = L < 8 ? L : 8, b = L < 16 ? L - a : 8;
    u64 k = _bzhi_u64(ld64(p) | LOWER, 8 * a) ^ _bzhi_u64(ld64(p + 8) | LOWER, 8 * b) * 0x9E3779B97F4A7C15ull;
    if (L > 16) {                              // the generator's longest token has 17 bytes
        u32 c = L < 24 ? L - 16 : 8;
        k ^= _bzhi_u64(ld64(p + 16) | LOWER, 8 * c) * 0xC2B2AE3D27D4EB4Full
           ^ _bzhi_u64(ld64(p + 24) | LOWER, 8 * (L - 16 - c)) * 0x165667B19E3779F9ull;
    }
    return k;
}

static inline u32 lookup(u64 k) {
    const __m256i kk = _mm256_set1_epi64x((long long)k);
    for (u32 b = bucket_of(k);; b = (b + 1) & BMASK) {
        __m256i e = _mm256_cmpeq_epi64(_mm256_load_si256((const __m256i *)tab[b].key), kk);
        u32 m = (u32)_mm256_movemask_pd(_mm256_castsi256_pd(e)) | (u32)(tab[b].key[4] == k) << 4;
        u32 v = tab[b].val[_tzcnt_u32(m) & 7], r = m ? v : NONE;
        if ((m != 0) | (tab[b].ovf == 0)) return r;   // one branch, taken unless the bucket overflowed
    }
}

static u32 intern(u64 k) {                     // called by all workers at once
    for (u32 b = bucket_of(k);; b = (b + 1) & BMASK) {
        for (u32 i = 0; i < 5; i++) {
            u64 cur = __atomic_load_n(&tab[b].key[i], __ATOMIC_RELAXED);
            if (!cur && __atomic_compare_exchange_n(&tab[b].key[i], &cur, k, 0, __ATOMIC_RELAXED, __ATOMIC_RELAXED))
                return tab[b].val[i] = b * 5 + i;
            if (cur == k) return b * 5 + i;
        }
        __atomic_store_n(&tab[b].ovf, 1, __ATOMIC_RELAXED);
    }
}

// ---------------------------------------------------------------- tokenizer
#define LOADU(p) _mm256_loadu_si256((const __m256i *)(p))
static inline u64 tm(const u8 *q) {            // token bytes of q[0..64): 0-9, a-z, A-Z
    u64 m = 0;
    for (int h = 0; h < 2; h++) {
        __m256i v = LOADU(q + 32 * h), d = _mm256_sub_epi8(v, _mm256_set1_epi8('0'));
        __m256i a = _mm256_sub_epi8(_mm256_or_si256(v, _mm256_set1_epi8(32)), _mm256_set1_epi8('a'));
        d = _mm256_cmpeq_epi8(d, _mm256_min_epu8(d, _mm256_set1_epi8(9)));
        a = _mm256_cmpeq_epi8(a, _mm256_min_epu8(a, _mm256_set1_epi8(25)));
        m |= (u64)(u32)_mm256_movemask_epi8(_mm256_or_si256(d, a)) << 32 * h;
    }
    return m;
}
static inline u64 lfm(const u8 *q) {
    return (u64)(u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(LOADU(q), _mm256_set1_epi8('\n')))
         | (u64)(u32)_mm256_movemask_epi8(_mm256_cmpeq_epi8(LOADU(q + 32), _mm256_set1_epi8('\n'))) << 32;
}

// Tokenizes the text from q to its LF into keys[], prefetching each key's
// bucket for the lookups that follow; returns the byte after the LF. 64-byte
// windows: a token starts at a token byte whose predecessor is not one (carry:
// the previous window's last byte); its length comes from the mask, plus the
// next window's leading run when it reaches the window's end. The per-token
// work is scalar: the vector units are shared with the core's other thread.
// Loads may run past the LF and the file end.
static const u8 *tokenize(const u8 *q, u64 *keys, u32 *pn) {
    u32 n = 0;
    u64 m = tm(q), lf = lfm(q), carry = 0;
    for (;; q += 64) {
        u64 m2 = tm(q + 64), lf2 = lfm(q + 64), mm = m & ((lf & -lf) - 1);   // bytes before the LF
        u32 more = (u32)_tzcnt_u64(~m2);
        for (u64 s = mm & ~(mm << 1 | carry); s; s &= s - 1) {
            u32 st = (u32)_tzcnt_u64(s), L = (u32)_tzcnt_u64(~(mm >> st));
            const u8 *t = q + st;
            L = st + L == 64 ? L + more : L;
            u64 k = L <= 16 ? ((ld64(t) | LOWER) & KM[L][0]) ^ ((ld64(t + 8) | LOWER) & KM[L][1]) * 0x9E3779B97F4A7C15ull
                            : key_at(t, L);
            _mm_prefetch((const char *)&tab[bucket_of(k)], _MM_HINT_T0);
            keys[n++] = k;
        }
        if (lf) { *pn = n; return q + _tzcnt_u64(lf) + 1; }
        carry = mm >> 63; m = m2; lf = lf2;
    }
}
static inline const u8 *skip_id(const u8 *q) { while (*q != '\t') q++; return q + 1; }

// ---------------------------------------------------------------- queries
static void parse(const u8 *p, const u8 *e, u32 q) {
    for (; p < e; q++) {
        int ph = *p == 'P';
        qkind[q] = (u8)ph;
        p += ph ? 8 : 5;                       // past `PHRASE "` or `BOOL `
        u32 k = 0;
        for (;;) {
            u32 L = (u32)_tzcnt_u64(~tm(p)), s = intern(key_at(p, L));
            qterm[8 * q + k++] = s;
            if (!ph) __atomic_store_n(&bflag[s], 1, __ATOMIC_RELAXED);
            p += L;
            if (*p != ' ') break;
            if (ph) { p++; continue; }
            qop[8 * q + k] = p[1] == 'A' ? AND : p[1] == 'O' ? OR : NOT;
            p += p[1] == 'O' ? 4 : 5;
        }
        qk[q] = (u8)k;
        p += ph ? 2 : 1;
    }
}

// ---------------------------------------------------------------- sampling, renumbering
// Document frequency of every term over the first 1/64 of the worker's first
// part; sdf[slot] = last doc counted << 16 | count.
static void sample(Worker *me, int w) {
    int p = w * ppw;
    const u8 *q = doc + pstart[p], *end = q + (pstart[p + 1] - pstart[p]) / SAMPLE_DIV;
    u32 d = 0, n, *sdf = me->sdf = sdfs + (size_t)w * NSLOT;
    while (q < end) {
        q = tokenize(skip_id(q), me->keys, &n);
        d++;
        for (u32 i = 0; i < n; i++) {
            u32 v = lookup(me->keys[i]);
            if (v != NONE && sdf[v] >> 16 != d) sdf[v] = d << 16 | ((sdf[v] + 1) & 0xFFFF);
        }
    }
    me->nsd = d;
}

static u32 anchor_of(u32 q, u32 *pj) {         // the phrase term with the smallest sampled df
    const u32 *t = qterm + 8 * q;
    u32 j = 0;
    for (u32 i = 1; i < qk[q]; i++) if (sampdf[t[i]] < sampdf[t[j]]) j = i;
    *pj = j;
    return newid[t[j]];
}

// A term's class for the renumbering: 0 a BOOL term with a bitmap row (sampled
// in over 1/DENSE_DIV of the documents), 1 another BOOL term (posting list),
// 2 a phrase-only term. New ids run class by class, in slot order.
static inline u32 cls(u32 s, u32 thr) { return bflag[s] ? (sampdf[s] > thr ? 0 : 1) : 2; }

// ---------------------------------------------------------------- scan
static void match_phrases(Worker *me, u32 n, u32 na, u32 gid) {
    const u32 *ids = me->ids;
    u32 h0 = me->nhit;
    for (u32 a = 0; a < na; a++) {
        u32 pos = me->aps[a], t = ids[pos];
        for (u32 e = aoff[t]; e < aoff[t + 1]; e++) {
            u32 q = aq[e], j = aj[e], k = qk[q], x = 0;
            if (j > pos || pos - j + k > n) continue;
            const u32 *qt = qterm + 8 * q, *dt = ids + pos - j;
            while (x < k && dt[x] == qt[x]) x++;
            if (x < k) continue;
            for (x = h0; x < me->nhit && me->hit[x].q != q; x++) {}
            if (x < me->nhit) continue;        // this doc already matched q
            if (me->nhit == me->hcap) me->hit = realloc(me->hit, (me->hcap = 2 * me->hcap + 4096) * sizeof(Hit));
            me->hit[me->nhit++] = (Hit){ q, gid };
        }
    }
}

// a[r] bit c <-> a[c] bit r, by swapping ever smaller blocks
static void transpose64(u64 *a) {
    u64 m = 0x00000000FFFFFFFFull;
    for (u32 j = 32; j; j >>= 1, m ^= m << j)
        for (u32 k = 0; k < 64; k = ((k | j) + 1) & ~j) {
            u64 t = (a[k] >> j ^ a[k | j]) & m;
            a[k] ^= t << j;
            a[k | j] ^= t;
        }
}

// Per doc: all lookups first (independent loads, many in flight; the next doc
// is tokenized just before, so its prefetches have a whole doc to land), then per
// token a dense term sets its bit in the doc's own small bitset dm, a rare BOOL
// term appends to the stream and an anchor is noted for the phrase check;
// branch-free, the other cases write to a spare word. dm has four copies, used
// in turn, so a common term's bit-sets do not wait on each other. Every 64 docs
// their bitsets (bt) turn into a word of each dense term's row by 64x64 transposes, and the
// stream is counted and scattered into per-term doc lists, list s =
// post[off[s] .. end[s]), each after a 0xFFFF sentinel so a repeat of the same
// doc is dropped by comparing with the last.
static void scan_part(Worker *me, int p) {
    const u32 ND = ndense, NS = nsparse, DW = ND / 64 + 1, DS = DW + 1, W64 = me->w64, first = pfirst[p], nd = pfirst[p + 1] - first;
    u64 *bt = me->bt, *dm = me->dm, *bm = me->bm, *kc = me->keys, *kn = kc + MAXTOK, *kt;
    u32 *ids = me->ids, *aps = me->aps, *docoff = me->docoff, sl = 0, n = 0, nn = 0;
    u16 *stream = me->stream;
    const u8 *q = doc + pstart[p];
    memset(bm, 0, (size_t)ND * W64 * 8);
    if (nd) q = tokenize(skip_id(q), kc, &n);
    for (u32 d = 0; d < nd; d++) {
        if (d + 1 < nd) q = tokenize(skip_id(q), kn, &nn);   // its prefetches get this doc's time to land
        for (u32 i = 0; i < n; i++) ids[i] = lookup(kc[i]);
        memset(dm, 0, 4 * DS * 8);
        u32 na = 0;
        for (u32 i = 0; i < n; i++) {
            u32 v = ids[i], id = v & ~ANCHOR, s = id - ND;
            ids[i] = id;
            dm[(i & 3) * DS + (id < ND ? id >> 6 : DW)] |= 1ull << (id & 63);
            stream[sl] = (u16)s;
            sl += s < NS;
            aps[na] = i;
            na += (v >> 31) & (v != NONE);
        }
        for (u32 w = 0; w < DW; w++) bt[(d & 63) * DW + w] = dm[w] | dm[DS + w] | dm[2 * DS + w] | dm[3 * DS + w];
        if ((d & 63) == 63 || d + 1 == nd) {
            memset(bt + ((d & 63) + 1) * DW, 0, (63 - (d & 63)) * DW * 8);
            for (u32 w = 0; w < DW; w++) {
                u64 a[64];
                for (u32 r = 0; r < 64; r++) a[r] = bt[r * DW + w];
                transpose64(a);
                for (u32 c = 0; c < 64 && 64 * w + c < ND; c++) bm[(size_t)(64 * w + c) * W64 + (d >> 6)] = a[c];
            }
        }
        docoff[d + 1] = sl;
        if (na) match_phrases(me, n, na, first + d);
        kt = kc; kc = kn; kn = kt; n = nn;
    }
    u32 *off = me->off, *end = me->end;
    u16 *post = me->post;
    memset(end, 0, (size_t)NS * 4);
    for (u32 e = 0; e < sl; e++) end[stream[e]]++;
    for (u32 s = 0, o = 0; s < NS; s++) { post[o] = 0xFFFF; off[s] = ++o; o += end[s]; end[s] = off[s]; }
    for (u32 d = 0; d < nd; d++)
        for (u32 e = docoff[d]; e < docoff[d + 1]; e++) {
            u32 s = stream[e], c = end[s];
            post[c] = (u16)d;
            end[s] = c + (post[c - 1] != d);
        }
}

// ---------------------------------------------------------------- BOOL answers
static inline u32 bit(const u64 *b, u32 i) { return (u32)(b[i >> 6] >> (i & 63) & 1); }

// first i with p[i] >= x (n if none), branch-free halving
static inline u32 lower(const u16 *p, u32 n, u16 x) {
    const u16 *b = p;
    if (!n) return 0;
    for (; n > 1; n -= n / 2) b += b[n / 2] < x ? n / 2 : 0;
    return (u32)(b - p) + (*b < x);
}
// tbl[m]: the shuffle that packs the 16-bit lanes set in m to the front
static u8 tbl[256][16];
static inline __m128i ld128(const void *p) { return _mm_loadu_si128((const __m128i *)p); }

// AND (keep 1) or NOT (keep 0) of sorted lists. 8-value blocks are compared all
// to all (SSE4.2); an a block collects its matches over every b block it meets
// and is written once its last value <= b's last, so no later b can match it.
// The tail restarts at the b block where the unwritten a block began. A short
// list against a long one binary-searches the long one.
static u32 filt(const u16 *a, u32 an, const u16 *b, u32 bn, u16 *d, u32 keep) {
    if (keep && an > bn) { const u16 *t = a; a = b; b = t; u32 tn = an; an = bn; bn = tn; }
    u32 i = 0, j = 0, n = 0, acc = 0, j0 = 0;
    if (an * 16 < bn)
        for (; i < an; i++) { j += lower(b + j, bn - j, a[i]); d[n] = a[i]; n += (j < bn && b[j] == a[i]) == keep; }
    for (; i + 8 <= an && j + 8 <= bn; j0 = acc ? j0 : j) {
        __m128i A = ld128(a + i);
        u32 x = a[i + 7], y = b[j + 7];
        acc |= (u32)_mm_cvtsi128_si32(_mm_cmpestrm(ld128(b + j), 8, A, 8, _SIDD_UWORD_OPS | _SIDD_CMP_EQUAL_ANY));
        if (x <= y) {
            u32 m = (acc ^ (keep - 1)) & 0xFF;
            _mm_storeu_si128((__m128i *)(d + n), _mm_shuffle_epi8(A, ld128(tbl[m])));
            n += (u32)_mm_popcnt_u32(m); i += 8; acc = 0;
        }
        j += y <= x ? 8 : 0;
    }
    for (j = j0 < j ? j0 : j; i < an && j < bn;) { u16 x = a[i], y = b[j]; d[n] = x; n += keep ? x == y : x < y; i += x <= y; j += y <= x; }
    while (!keep && i < an) d[n++] = a[i++];
    return n;
}
static u32 unite(const u16 *a, u32 an, const u16 *b, u32 bn, u16 *d) {
    u32 i = 0, j = 0, n = 0;
    while (i < an && j < bn) { u16 x = a[i], y = b[j]; d[n++] = x < y ? x : y; i += x <= y; j += y <= x; }
    while (i < an) d[n++] = a[i++];
    while (j < bn) d[n++] = b[j++];
    return n;
}
// Record for a query that matched in the part: q | nid << 24, count, the sum of
// the docs' part-local numbers, then the smallest of those numbers as u16s, as
// many as the worker still lacks.
static void emit(Worker *me, u32 q, const u64 *rb, const u16 *rl, u32 rn) {
    static const u64 BM[6] = { 0xAAAAAAAAAAAAAAAAull, 0xCCCCCCCCCCCCCCCCull, 0xF0F0F0F0F0F0F0F0ull,
                               0xFF00FF00FF00FF00ull, 0xFFFF0000FFFF0000ull, 0xFFFFFFFF00000000ull };
    u32 *r = me->rec + me->nrec, room = MAXQ - me->qdone[q], nid = 0, cnt = 0, sum = 0;
    u16 *id = (u16 *)(r + 3);
    if (rb) {
        for (u32 k = 0; k < me->w64; k++) {
            u64 x = rb[k];
            if (!x) continue;
            u32 c = (u32)_mm_popcnt_u64(x);
            cnt += c; sum += c * 64 * k;
            for (int b = 0; b < 6; b++) sum += (u32)_mm_popcnt_u64(x & BM[b]) << b;   // sum of set bit indexes
            for (; x && nid < room; x &= x - 1) id[nid++] = (u16)(64 * k + _tzcnt_u64(x));
        }
    } else {
        cnt = rn;
        for (u32 k = 0; k < rn; k++) sum += rl[k];
        for (; nid < rn && nid < room; nid++) id[nid] = rl[nid];
    }
    if (!cnt) return;
    r[0] = q | nid << 24; r[1] = cnt; r[2] = sum;
    me->nrec += 3 + (nid + 1) / 2;
    me->qdone[q] += (u8)nid;
}

// Strictly left to right. The running result is a sorted list while every term
// so far is a list, and a bitmap once a bitmap term joins by OR; a bitmap term
// filters a list under AND/NOT, and AND with a list turns a bitmap back into a list.
static void eval_part(Worker *me, int p) {
    const u32 ND = ndense, W64 = me->w64, *off = me->off, *end = me->end;
    const u16 *post = me->post;
    u64 *bm = me->bm, *sa = bm + (size_t)ND * W64, *sb = sa + W64;
    u32 *bd = bound + (size_t)p * (nw + 1), c = 0;
    for (u32 q = 0; q < nq; q++) {
        for (; c <= (u32)nw && qbase[c] == q; c++) bd[c] = me->nrec;
        if (qkind[q]) continue;
        const u32 *t = qterm + 8 * q;
        const u64 *rb = NULL;
        const u16 *rl = me->la;
        u32 rn = 0;
        if (t[0] < ND) rb = bm + (size_t)t[0] * W64;
        else rl = post + off[t[0] - ND], rn = end[t[0] - ND] - off[t[0] - ND];
        for (u32 i = 1; i < qk[q]; i++) {
            u32 o = qop[8 * q + i], x = t[i], m = 0;
            if (!rb && !rn && o != OR) continue;  // an empty result stays empty
            u16 *dl = rl == me->la ? me->lb : me->la;
            u64 *db = rb == sa ? sb : sa;
            if (x < ND) {
                const u64 *xb = bm + (size_t)x * W64;
                if (rb && o == OR) for (u32 k = 0; k < W64; k++) db[k] = rb[k] | xb[k];
                else if (rb) { u64 inv = o == NOT ? ~0ull : 0; for (u32 k = 0; k < W64; k++) db[k] = rb[k] & (xb[k] ^ inv); }
                else if (o == OR) {
                    memcpy(db, xb, (size_t)W64 * 8);
                    for (u32 k = 0; k < rn; k++) db[rl[k] >> 6] |= 1ull << (rl[k] & 63);
                } else {
                    for (u32 k = 0, keep = o == AND; k < rn; k++) { dl[m] = rl[k]; m += bit(xb, rl[k]) == keep; }
                    rl = dl; rn = m;
                    continue;
                }
                rb = db;
                continue;
            }
            const u16 *xl = post + off[x - ND];
            u32 xn = end[x - ND] - off[x - ND];
            if (!rb) {
                rn = o == OR ? unite(rl, rn, xl, xn, dl) : filt(rl, rn, xl, xn, dl, o == AND);
                rl = dl;
            } else if (o == AND) {
                for (u32 k = 0; k < xn; k++) { dl[m] = xl[k]; m += bit(rb, xl[k]); }
                rl = dl; rn = m; rb = NULL;
            } else {
                memcpy(db, rb, (size_t)W64 * 8);
                if (o == OR) for (u32 k = 0; k < xn; k++) db[xl[k] >> 6] |= 1ull << (xl[k] & 63);
                else for (u32 k = 0; k < xn; k++) db[xl[k] >> 6] &= ~(1ull << (xl[k] & 63));
                rb = db;
            }
        }
        emit(me, q, rb, rl, rn);
    }
    for (; c <= (u32)nw; c++) bd[c] = me->nrec;
}

// ---------------------------------------------------------------- merge, format
static char *put(char *o, u64 v) {
    char t[20];
    int n = 0;
    do t[n++] = (char)('0' + v % 10); while (v /= 10);
    while (n) *o++ = t[--n];
    return o;
}

// Parts in doc order, so each query's ids arrive ascending.
static void finish(Worker *me, int c) {
    u32 lo = qbase[c], m = qbase[c + 1] - lo, *cnt = arena(m * 4), *top = arena((size_t)m * MAXQ * 4);
    u64 *sum = arena(m * 8);
    u8 *nid = arena(m);
    for (int p = 0; p < P; p++) {
        const u32 *bd = bound + (size_t)p * (nw + 1), *rec = wk[p / ppw].rec;
        for (const u32 *r = rec + bd[c]; r < rec + bd[c + 1]; r += 3 + ((r[0] >> 24) + 1) / 2) {
            u32 q = (r[0] & 0xFFFFFF) - lo;
            cnt[q] += r[1];
            sum[q] += r[2] + (u64)r[1] * pfirst[p];
            for (u32 i = 0; i < r[0] >> 24 && nid[q] < MAXQ; i++) top[q * MAXQ + nid[q]++] = pfirst[p] + ((const u16 *)(r + 3))[i];
        }
    }
    for (int w = 0; w < nw; w++)
        for (u32 i = 0; i < wk[w].nhit; i++) {
            u32 q = wk[w].hit[i].q - lo, g = wk[w].hit[i].gid;
            if (q >= m) continue;
            cnt[q]++; sum[q] += g;
            if (nid[q] < MAXQ) top[q * MAXQ + nid[q]++] = g;
        }
    char *o = me->out = arena((size_t)m * 160);
    for (u32 q = 0; q < m; q++) {
        o = put(o, cnt[q]); *o++ = ' '; o = put(o, sum[q]);
        for (u32 i = 0; i < nid[q]; i++) { *o++ = ' '; o = put(o, top[q * MAXQ + i]); }
        *o++ = '\n';
    }
    me->outlen = (size_t)(o - me->out);
}

// ---------------------------------------------------------------- worker
static void *worker(void *arg) {
    int w = (int)(long)arg;
    Worker *me = &wk[w];
    me->keys = arena((size_t)MAXTOK * 24);     // two key buffers, ids, anchor positions
    me->ids = (u32 *)(me->keys + 2 * MAXTOK); me->aps = me->ids + MAXTOK;
    // 1. queries: count this worker's lines, then parse them into the shared table
    u32 n = 0, base = 0;
    for (size_t i = qs[w]; i < qs[w + 1]; i++) n += qtxt[i] == '\n';
    qcount[w] = n;
    pthread_barrier_wait(&bar);
    for (int i = 0; i < w; i++) base += qcount[i];
    qbase[w] = base;
    if (w == nw - 1) qbase[nw] = nq = base + n;
    parse(qtxt + qs[w], qtxt + qs[w + 1], base);
    pthread_barrier_wait(&bar);
    if (!w) MARK("parsed\n");
    // 2. sample, sum the samples by slot range, renumber, rewrite the table
    sample(me, w);
    pthread_barrier_wait(&bar);
    // Each worker sums the samples over one slot range and counts its terms by
    // class; the counts' prefix sums give each range its first new ids.
    u32 lo = (u32)((u64)NSLOT * w / nw), hi = (u32)((u64)NSLOT * (w + 1) / nw), thr = 0, pos;
    u32 next[3] = { 0, 0, 0 }, tot[3] = { 0, 0, 0 };
    for (int i = 0; i < nw; i++) thr += wk[i].nsd;
    thr /= DENSE_DIV;
    for (u32 s = lo; s < hi; s++) {
        u32 t = 0;
        for (int i = 0; i < nw; i++) t += wk[i].sdf[s] & 0xFFFF;
        sampdf[s] = t;
        if (tab[s / 5].key[s % 5]) me->cnt[cls(s, thr)]++;
    }
    pthread_barrier_wait(&bar);
    for (int i = 0; i < nw; i++)
        for (int c = 0; c < 3; c++) { tot[c] += wk[i].cnt[c]; if (i < w) next[c] += wk[i].cnt[c]; }
    next[1] += tot[0]; next[2] += tot[0] + tot[1];
    for (u32 s = lo; s < hi; s++) if (tab[s / 5].key[s % 5]) newid[s] = next[cls(s, thr)]++;
    if (!w) {
        ndense = tot[0]; nsparse = tot[1]; nterms = tot[0] + tot[1] + tot[2];
        if (nsparse > 65536) die("over 65,536 rare BOOL terms");
        MARK("classified: %u dense, %u rare BOOL terms\n", ndense, nsparse);
    }
    pthread_barrier_wait(&bar);
    // Phrase queries listed per anchor term: counted, offsets summed, filled.
    for (u32 q = base; q < base + n; q++) if (qkind[q]) __atomic_fetch_add(&aoff[anchor_of(q, &pos)], 1, __ATOMIC_RELAXED);
    pthread_barrier_wait(&bar);
    if (!w) for (u32 i = 1; i <= nterms; i++) aoff[i] += aoff[i - 1];
    pthread_barrier_wait(&bar);
    for (u32 q = base; q < base + n; q++)
        if (qkind[q]) { u32 e = __atomic_sub_fetch(&aoff[anchor_of(q, &pos)], 1, __ATOMIC_RELAXED); aq[e] = q; aj[e] = pos; }
    pthread_barrier_wait(&bar);
    for (u32 s = lo; s < hi; s++)
        if (tab[s / 5].key[s % 5]) { u32 v = newid[s]; tab[s / 5].val[s % 5] = v | (aoff[v + 1] > aoff[v] ? ANCHOR : 0); }
    for (u32 q = base; q < base + n; q++) for (u32 j = 0; j < qk[q]; j++) qterm[8 * q + j] = newid[qterm[8 * q + j]];
    // 3. index and answer each part
    u32 maxnd = 1;
    size_t maxb = 0;
    for (int p = w * ppw; p < (w + 1) * ppw; p++) {
        if (pfirst[p + 1] - pfirst[p] > maxnd) maxnd = pfirst[p + 1] - pfirst[p];
        if (pstart[p + 1] - pstart[p] > maxb) maxb = pstart[p + 1] - pstart[p];
    }
    u32 W64 = me->w64 = (maxnd + 63) / 64;
    for (int pass = 0; pass < 2; pass++) {     // pass 0 sizes the arena, pass 1 carves it
        u8 *h = me->heap = pass ? arena(me->hsize) : NULL;
        me->bm = take(&h, ((size_t)ndense + 2) * W64 * 8);
        me->bt = take(&h, 64 * (ndense / 64 + 1) * 8); me->dm = take(&h, (ndense / 64 + 2) * 32);
        me->la = take(&h, maxnd * 2 + 64); me->lb = take(&h, maxnd * 2 + 64);
        me->off = take(&h, nsparse * 4); me->end = take(&h, nsparse * 4); me->docoff = take(&h, maxnd * 4 + 4);
        me->stream = take(&h, maxb); me->post = take(&h, maxb + nsparse * 2);   // a token takes two bytes of text
        me->hsize = (size_t)(h - me->heap);
    }
    me->rec = arena((size_t)ppw * nq * (3 + MAXQ / 2) * 4);
    me->qdone = arena(nq);
    pthread_barrier_wait(&bar);
    if (!w) munmap(sdfs, sdfsize);
    for (int p = w * ppw; p < (w + 1) * ppw; p++) {
        scan_part(me, p);
        if (!w || w == nw - 1) MARK("w%d part %d scanned\n", w, p);
        eval_part(me, p);
        if (!w || w == nw - 1) MARK("w%d part %d answered\n", w, p);
    }
    munmap(me->heap, me->hsize + 64);
    pthread_barrier_wait(&bar);
    // 4. merge and format the queries this worker parsed
    finish(me, w);
    return NULL;
}

static u32 id_at(const u8 *p) { u32 v = 0; while (*p >= '0' && *p <= '9') v = v * 10 + (u32)(*p++ - '0'); return v; }

int main(int argc, char **argv) {
    const char *in = argc > 1 ? argv[1] : "/data", *out = argc > 2 ? argv[2] : "/out";
#ifdef TIMING
    t0 = ms();
#endif
    cpu_set_t cs;
    sched_getaffinity(0, sizeof cs, &cs);      // nproc counts every core; the box gives us 12
    nw = getenv("SD_WORKERS") ? atoi(getenv("SD_WORKERS")) : CPU_COUNT(&cs);
    if (nw < 1 || nw > WMAX) nw = nw < 1 ? 1 : WMAX;
    doc = map_file(in, "docs.txt", &dlen);
    qtxt = map_file(in, "queries.txt", &qlen);
    for (int w = 0; w <= nw; w++) {            // query shares: byte ranges cut at line starts
        size_t o = qlen * (size_t)w / (size_t)nw;
        while (o > 0 && o < qlen && qtxt[o - 1] != '\n') o++;
        qs[w] = o;
    }
    size_t maxq = qlen / 7 + 1;                // the shortest query line is `BOOL a` + LF
    tab = huge(sizeof(Bucket) << BBITS);
    for (u32 L = 0, a; L <= 16; L++)
        a = L < 8 ? L : 8, KM[L][0] = a == 8 ? ~0ull : (1ull << 8 * a) - 1, KM[L][1] = L - a == 8 ? ~0ull : (1ull << 8 * (L - a)) - 1;
    sdfs = huge(sdfsize = (size_t)(nw + 2) * NSLOT * 4);   // each worker's sample counts, their sums, the new ids
    sampdf = sdfs + (size_t)nw * NSLOT; newid = sampdf + NSLOT;
    aoff = arena((size_t)NSLOT * 4 + 4); aq = arena(maxq * 4); aj = arena(maxq * 4);
    qterm = arena(maxq * 32); qop = arena(maxq * 8); qk = arena(maxq); qkind = arena(maxq);
    size_t t = dlen - 1;
    while (t > 0 && doc[t - 1] != '\n') t--;
    ndocs = id_at(doc + t) + 1;
    P = nw * NSUB;
    if ((u32)P < (ndocs + MAXPART - 1) / MAXPART) P = (int)(((ndocs + MAXPART - 1) / MAXPART + nw - 1) / nw * nw);
    ppw = P / nw;
    pstart = malloc((P + 1) * sizeof *pstart); pfirst = malloc((P + 1) * 4); bound = malloc((size_t)P * (nw + 1) * 4);
    for (int p = 0; p <= P; p++) {             // parts: equal byte ranges cut at line starts
        size_t o = dlen * (size_t)p / (size_t)P;
        while (o > 0 && o < dlen && doc[o - 1] != '\n') o++;
        pstart[p] = o;
        pfirst[p] = o < dlen ? id_at(doc + o) : ndocs;
        if (p && pfirst[p] - pfirst[p - 1] > MAXPART) die("a part holds over 32,768 documents");
    }
    for (u32 m = 0; m < 256; m++)
        for (u32 l = 0, k = 0; l < 8; l++) if (m >> l & 1) tbl[m][2 * k] = (u8)(2 * l), tbl[m][2 * k++ + 1] = (u8)(2 * l + 1);
    pthread_barrier_init(&bar, NULL, (unsigned)nw);
    pthread_t th[WMAX];
    for (int w = 1; w < nw; w++) pthread_create(&th[w], NULL, worker, (void *)(long)w);
    worker(NULL);
    for (int w = 1; w < nw; w++) pthread_join(th[w], NULL);
    MARK("merged\n");
    char path[4096];
    snprintf(path, sizeof path, "%s/results.txt", out);
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) die(path);
    for (int w = 0; w < nw; w++)
        for (size_t o = 0; o < wk[w].outlen;) {
            ssize_t r = write(fd, wk[w].out + o, wk[w].outlen - o);
            if (r <= 0) die("write");
            o += (size_t)r;
        }
    exit(0);
}
