// bindex.c - Speed Demon track B: BOOL and PHRASE queries over docs.txt.
//
// Only the query terms are indexed. Each of the nw workers parses its share of
// queries.txt into one shared term table, then owns a contiguous stretch of the
// corpus cut into parts of at most 32,768 documents. Per part it records which
// documents hold each BOOL term (a bitmap row for each term in over 1/32 of the
// sampled documents, about 370 on seed 1; a sorted u16 list for the rest),
// answers every BOOL query on the part and keeps a record for each query that
// matched. Phrases are checked during the scan at each occurrence of their
// rarest term. Last, each worker merges every part's
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

static inline __m128i ld128(const void *p) { return _mm_loadu_si128((const __m128i *)p); }

#define PHDOCS 63000      // docs per phase at most, so a phase-local doc number fits a u16 below the 0xFFFF sentinel
#define SH 10             // term shards: 1 << SH terms each
static uint32_t SCAP;     // entries per worker per shard (address space only), set in main: 2^20 / workers
#define PCAP (1u << 22)   // list entries per shard, address space only
#define QB 64             // queries per claim
#define MAXTOK 65536      // tokens per doc; the spec's longest doc (999 units) has ~10,000
#ifndef BBITS
#define BBITS 15          // term table: 2^15 buckets of 7 tags
#endif
#define NSLOT (8u << BBITS)
#define SAMPLE_DIV 48     // each worker samples the first 1/64 of its first part
#define DENSE_DIV 32      // a BOOL term in over 1/32 of the sample gets a bitmap row
#define MAXQ 10           // ids printed per query
#define WMAX 64
#define ANCHOR (1u << 31) // table value flag: phrases are checked at this term
#define MISS 0x7FFFFFFFu  // lookup value of a token that is no query term
#define PROBE 0x7FFFFFFEu // a bucket's miss value once a key was probed past it


static void die(const char *m) { fprintf(stderr, "bindex: %s\n", m); exit(1); }

enum { AND = 1, OR, NOT };
typedef struct { u32 q, gid; } Hit;            // phrase query q matches document gid
typedef struct { u64 sum; u32 cnt, nid, top[10]; } Tally;   // a query's answer so far
// A worker's memory. One document's scan: keys (its token keys, then the next
// document's), ids (their term ids), aps (positions of its anchor tokens), db (a
// byte per bitmap term: seen in this document). One part: bt (64 documents' db
// bits, transposed into bm, the bitmap rows), stream (rare term numbers in token
// order, document d's from docoff[d]), post (each rare term's documents, from
// off to end). Answers: la and lb (list buffers), rec and nrec (a record per
// matched query), qdone (ids recorded per query), hit, nhit and hcap (phrase
// matches). Setup: sdf and nsd (sampled document counts per term slot, documents
// sampled), cnt (terms per class: bitmap row, list, phrase only). w64: 64-bit
// words per bitmap row.
typedef struct {
    _Alignas(64) u64 *keys, *bt, *sa, *sb;
    u8 *sdf; u32 *ids, *aps, *dl, *rl, *sbase, *sp[65], nsd, nhit, hcap, cnt[3];
    u16 *la, *lb;
    u8 *db;
    Hit *hit;
    char *out; size_t outlen;
} Worker;

static Worker wk[WMAX];
static int nw, P, PH;                          // workers, parts (one per worker per phase), phases
static const u8 *doc, *qtxt;
static size_t dlen, qlen, qs[WMAX + 1], *pstart;
static u32 ndocs, nq, ndense, nsparse, nterms, qcount[WMAX], qbase[WMAX + 1], *pfirst;
static u8 *qkind, *qk, *qop, bflag[NSLOT];     // query q: terms at qterm[8q..], op before term i at qop[8q+i]
static u8 *sdfs; static u32 *qterm, *sampdf, *newid, *aoff, *aq, *aj, *an;
static size_t sdfsize;
static u64 *bm;                                // the phase's bitmap rows, W64 words each
static u32 *off, *end, W64, bclaim[256], qclaim[256];   // a list term's list: post[off[s] .. end[s])
static u16 *post;
static Tally *tally;
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
// A token's key is an AES hash of its lowercased bytes: two AES rounds over up
// to 32 bytes, never 0 (tag 0 marks an empty slot). Two tokens share a key
// only by a 64-bit coincidence. A bucket holds 7 tags and their values in one
// cache line, plus an eighth value that answers a miss: MISS, or PROBE once a
// term was placed past the bucket. A lookup compares all eight tags at once
// (tag[7] stays 0, so it never matches) and reads val[index of the match, or
// 7], so a hit and a miss take the same path. A slot's value is its own index
// until the workers renumber the terms.
typedef struct { _Alignas(64) u32 tag[8], val[8]; } Bucket;
static Bucket *tab;                            // on one huge page: lookups take no TLB misses
#define BMASK ((1u << BBITS) - 1)

static _Alignas(16) u8 LM[129][16];       // LM[L]: the first L bytes 0xFF; filled in main
static inline u64 hash_at(const u8 *p, u32 L) {
    __m128i x = _mm_and_si128(_mm_or_si128(ld128(p), _mm_set1_epi8(32)), _mm_load_si128((const __m128i *)LM[L]));
    if (L > 16) x = _mm_xor_si128(x, _mm_slli_epi64(_mm_and_si128(_mm_or_si128(ld128(p + 16), _mm_set1_epi8(32)), _mm_load_si128((const __m128i *)LM[L - 16])), 3));
    unsigned long long hi, lo = _mulx_u64((u64)_mm_cvtsi128_si64(x) ^ 0x9E3779B97F4A7C15ull, (u64)_mm_extract_epi64(x, 1) ^ 0xD6E8FEB86659FD93ull, &hi);
    return (hi ^ lo) | 1;   // never 0: tag 0 is an empty slot
}

static u32 probe(u64 h, u32 b) {               // the tag may sit in a later bucket
    for (;;) {
        b = (b + 1) & BMASK;
        for (u32 i = 0; i < 7; i++) if (tab[b].tag[i] == (u32)h) return tab[b].val[i];
        if (tab[b].val[7] != PROBE) return MISS;
    }
}

static inline u32 lookup(u64 h) {
    const Bucket *t = &tab[h >> (64 - BBITS)];
    u32 m = (u32)_mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpeq_epi32(_mm256_load_si256((const __m256i *)t->tag), _mm256_set1_epi32((int)h))));
    u32 v = t->val[_tzcnt_u32(m | 128)];
    return v == PROBE ? probe(h, (u32)(h >> (64 - BBITS))) : v;
}

// Called by all workers at once while they parse: a free tag slot is claimed by
// compare-and-swap. Relaxed order is enough: no worker looks a term up before
// the barrier after the parse.
static u32 intern(u64 h) {
    for (u32 b = (u32)(h >> (64 - BBITS));; b = (b + 1) & BMASK) {
        for (u32 i = 0; i < 7; i++) {
            u32 cur = __atomic_load_n(&tab[b].tag[i], __ATOMIC_RELAXED);
            if (!cur && __atomic_compare_exchange_n(&tab[b].tag[i], &cur, (u32)h, 0, __ATOMIC_RELAXED, __ATOMIC_RELAXED))
                return tab[b].val[i] = b * 8 + i;
            if (cur == (u32)h) return b * 8 + i;
        }
        __atomic_store_n(&tab[b].val[7], PROBE, __ATOMIC_RELAXED);
    }
}

// ---------------------------------------------------------------- tokenizer
#define LOADU(p) _mm256_loadu_si256((const __m256i *)(p))
static inline u64 tm(const u8 *q) {            // token bytes of q[0..64): 0-9, a-z, A-Z
    u64 m = 0;   // unsigned range tests: x - '0' <= 9 is a digit, (x | 32) - 'a' <= 25 a letter
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

// Tokenizes the text from q to its LF into keys[]; returns the byte after the LF. 64-byte
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
            keys[n++] = hash_at(t, L);
        }
        if (lf) { *pn = n; return q + _tzcnt_u64(lf) + 1; }
        carry = mm >> 63; m = m2; lf = lf2;
    }
}
// A docs.txt line is its id, a TAB and the text; ids run 0 to N-1 in file order (spec).
static u32 id_at(const u8 *p);
static inline const u8 *skip_id(const u8 *q) { while (*q != '\t') q++; return q + 1; }

// ---------------------------------------------------------------- queries
static void parse(const u8 *p, const u8 *e, u32 q) {
    for (; p < e; q++) {
        int ph = *p == 'P';
        qkind[q] = (u8)ph;
        p += ph ? 8 : 5;                       // past `PHRASE "` or `BOOL `
        u32 k = 0;
        for (;;) {
            u32 L = (u32)_tzcnt_u64(~tm(p)), s = intern(hash_at(p, L));
            qterm[8 * q + k++] = s;            // the spec caps a query at 8 terms
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
    int p = w;
    const u8 *q = doc + pstart[p], *end = q + (pstart[p + 1] - pstart[p]) / SAMPLE_DIV;
    u32 d = 0, n; u8 *sdf = me->sdf = sdfs + (size_t)w * NSLOT;
    while (q < end) {
        q = tokenize(skip_id(q), me->keys, &n);
        d++;
        for (u32 i = 0; i < n; i++) {
            u32 v = lookup(me->keys[i]);
            if (v != MISS) sdf[v] += sdf[v] < 255;
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
            u32 j = aj[e] & 255, k = aj[e] >> 8, side = j + 1 < k ? pos + 1 : pos - 1;
            if (j > pos || pos - j + k > n || ids[side] != an[e]) continue;
            u32 q = aq[e], x = 0;
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

// Per token: only the lookup; the dense flags, the rare stream entries and the
// anchor positions fall out of the ids a block of 8 at a time, AVX2 left-packed.
// Each lookup's
// bucket is asked for 8 tokens ahead, into the next doc (tokenized first): a
// whole doc ahead, ~200 lines in flight pushed each other out of L1 (b-109:
// L1 misses 65.5 -> 47.5 M, box 105.0 -> 103.2 ms). The flags of 64
// docs (bt, one bit per term) turn into a word of each dense term's row by
// 64x64 transposes, and the stream is counted and scattered into per-term doc
// lists, list s = post[off[s] .. end[s]), each after a 0xFFFF sentinel so a
// repeat of the same doc is dropped by comparing with the last.
// PACK[m]: the permute indexes that pack the 32-bit lanes set in m to the front
static _Alignas(32) u32 PACK[256][8];

#ifndef AHEAD
#define AHEAD 12
#endif
#define RING 512
static void scan_part(Worker *me, int p) {
    const u32 ND = ndense, NS = nsparse, DW = ND / 64 + 1, pf = pfirst[p / nw * nw], d0 = pfirst[p] - pf, de = pfirst[p + 1] - pf;
    u64 *bt = me->bt, *K = me->keys;
    u32 *ids = me->ids, *aps = me->aps, *dl = me->dl, *rl = me->rl, **sp = me->sp, n = 0, nn = 0, a = 0;
    u8 *db = me->db;
    const u8 *q = doc + pstart[p];
    const __m256i NDN = _mm256_set1_epi32((int)ND), NSN = _mm256_set1_epi32((int)NS);
    for (u32 c = 0; c < ((NS + (1u << SH) - 1) >> SH); c++) sp[c] = me->sbase + c * SCAP;
    if (d0 < de) q = tokenize(skip_id(q), K, &n);
    for (u32 d = d0; d < de; d++) {
        // the current doc's hashes sit at K + a, the next doc's right after them,
        // so the prefetch ahead needs no choice of buffer
        if (a > RING) { memmove(K, K + a, (size_t)n * 8); a = 0; }   // keep the touched memory small
        const u64 *cur = K + a;
        if (d + 1 < de) q = tokenize(skip_id(q), K + a + n, &nn);
        u32 na = 0, ndh = 0, nr = 0;
        for (u32 i = 0; i < n; i++) {
            _mm_prefetch((const char *)&tab[cur[i + AHEAD] >> (64 - BBITS)], _MM_HINT_T0);   // AHEAD tokens ahead, into the next doc
            ids[i] = lookup(cur[i]);
        }
        _mm256_storeu_si256((__m256i *)(ids + n), _mm256_set1_epi32((int)MISS));   // the last block's padding lanes count as misses
        // blocks of 8: anchor, dense and rare lanes as masks, dense and rare hits left-packed
        for (u32 i = 0; i < n; i += 8) {
            __m256i v = _mm256_loadu_si256((__m256i *)(ids + i));
            __m256i id = _mm256_and_si256(v, _mm256_set1_epi32((int)~ANCHOR)), s = _mm256_sub_epi32(id, NDN);
            _mm256_storeu_si256((__m256i *)(ids + i), id);
            u32 dm = (u32)_mm256_movemask_ps(_mm256_castsi256_ps(_mm256_cmpgt_epi32(NDN, id)));
            u32 rm = (u32)_mm256_movemask_ps(_mm256_castsi256_ps(_mm256_andnot_si256(_mm256_srai_epi32(s, 31), _mm256_cmpgt_epi32(NSN, s))));
            _mm256_storeu_si256((__m256i *)(dl + ndh), _mm256_permutevar8x32_epi32(id, _mm256_load_si256((const __m256i *)PACK[dm])));
            _mm256_storeu_si256((__m256i *)(rl + nr), _mm256_permutevar8x32_epi32(s, _mm256_load_si256((const __m256i *)PACK[rm])));
            ndh += (u32)_mm_popcnt_u32(dm); nr += (u32)_mm_popcnt_u32(rm);
            for (u32 m = (u32)_mm256_movemask_ps(_mm256_castsi256_ps(v)); m; m &= m - 1) aps[na++] = i + _tzcnt_u32(m);
        }
        for (u32 k = 0; k < ndh; k++) db[dl[k]] = 0x80;
        for (u32 k = 0; k < nr; k++) { u32 s = rl[k]; *sp[s >> SH]++ = s << 16 | d; }
        for (u32 w = 0; w < DW; w++) {
            __m256i *f = (__m256i *)(db + 64 * w);
            bt[(d & 63) * DW + w] = (u32)_mm256_movemask_epi8(f[0]) | (u64)(u32)_mm256_movemask_epi8(f[1]) << 32;
            f[0] = f[1] = _mm256_setzero_si256();
        }
        if ((d & 63) == 63 || d + 1 == de) {
            memset(bt + ((d & 63) + 1) * DW, 0, (63 - (d & 63)) * DW * 8);
            for (u32 w = 0; w < DW; w++) {
                u64 a[64];
                for (u32 r = 0; r < 64; r++) a[r] = bt[r * DW + w];
                transpose64(a);
                for (u32 c = 0; c < 64 && 64 * w + c < ND; c++) bm[(size_t)(64 * w + c) * W64 + (d >> 6)] = a[c];
            }
        }
        if (na) match_phrases(me, n, na, pf + d);
        a += n; n = nn;
    }
    if (p % nw == nw - 1)                      // the phase's last stretch: rows past its last document stay empty
        for (u32 r = 0; r < ND; r++) memset(bm + (size_t)r * W64 + (de + 63) / 64, 0, (W64 - (de + 63) / 64) * 8);
}

// The lists of shard c: counted, then filled in worker order, which is document
// order. Each list follows a 0xFFFF sentinel, so a repeat of a doc is dropped by
// comparing with the last.
static void build(u32 c) {
    u32 lo = c << SH, hi = (c + 1) << SH, o = c * PCAP;
    if (hi > nsparse) hi = nsparse;
    memset(end + lo, 0, (hi - lo) * 4);
    for (int w = 0; w < nw; w++)
        for (const u32 *e = wk[w].sbase + c * SCAP, *x = wk[w].sp[c]; e < x; e++) end[*e >> 16]++;
    for (u32 s = lo; s < hi; s++) { post[o] = 0xFFFF; off[s] = ++o; o += end[s]; end[s] = off[s]; }
    for (int w = 0; w < nw; w++)
        for (const u32 *e = wk[w].sbase + c * SCAP, *x = wk[w].sp[c]; e < x; e++) {
            u32 s = *e >> 16, i = end[s];
            post[i] = (u16)*e;
            end[s] = i + (post[i - 1] != (u16)*e);
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

// cnt and sum of the set bits' indexes of b[0..w), w a multiple of 4. Bit index =
// 256 block + 8 byte + bit in byte: per byte a popcount and a sum of positions
// from nibble tables, the bytes weighted by their index in the 32-byte block.
static void bsum(const u64 *b, u32 w, u32 *cnt, u32 *sum) {
    // PC: set bits per nibble. PS: the sum of a low nibble's set-bit positions.
    // PH: the same for a high nibble, PS + 4 * PC (each bit sits 4 places higher).
    // WT: each byte's index in the 32-byte block, 0-31.
    const __m256i PC = _mm256_broadcastsi128_si256(ld128("\0\1\1\2\1\2\2\3\1\2\2\3\2\3\3\4")),
                  PS = _mm256_broadcastsi128_si256(ld128("\0\0\1\1\2\2\3\3\3\3\4\4\5\5\6\6")),
                  PH = _mm256_add_epi8(PS, _mm256_slli_epi16(PC, 2)), M = _mm256_set1_epi8(15), Z = _mm256_setzero_si256(),
                  WT = _mm256_loadu_si256((const __m256i *)"\0\1\2\3\4\5\6\7\10\11\12\13\14\15\16\17\20\21\22\23\24\25\26\27\30\31\32\33\34\35\36\37");
    __m256i aC = Z, aB = Z, aQ = Z, aW = Z;
    for (u32 t = 0; t < w / 4; t++) {
        __m256i v = _mm256_loadu_si256((const __m256i *)(b + 4 * t)), lo = _mm256_and_si256(v, M), hi = _mm256_and_si256(_mm256_srli_epi64(v, 4), M);
        __m256i pc = _mm256_add_epi8(_mm256_shuffle_epi8(PC, lo), _mm256_shuffle_epi8(PC, hi)), s = _mm256_sad_epu8(pc, Z);
        aC = _mm256_add_epi64(aC, s);
        aB = _mm256_add_epi64(aB, _mm256_mul_epu32(s, _mm256_set1_epi64x(t)));
        aQ = _mm256_add_epi64(aQ, _mm256_sad_epu8(_mm256_add_epi8(_mm256_shuffle_epi8(PS, lo), _mm256_shuffle_epi8(PH, hi)), Z));
        aW = _mm256_add_epi32(aW, _mm256_madd_epi16(_mm256_maddubs_epi16(pc, WT), _mm256_set1_epi16(1)));
    }
    u64 c[4], k[4], q[4]; u32 x[8];
    _mm256_storeu_si256((__m256i *)c, aC); _mm256_storeu_si256((__m256i *)k, aB);
    _mm256_storeu_si256((__m256i *)q, aQ); _mm256_storeu_si256((__m256i *)x, aW);
    *cnt = (u32)(c[0] + c[1] + c[2] + c[3]);
    *sum = (u32)(256 * (k[0] + k[1] + k[2] + k[3]) + q[0] + q[1] + q[2] + q[3]) + 8 * (x[0] + x[1] + x[2] + x[3] + x[4] + x[5] + x[6] + x[7]);
}

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
    // j0: the next b block while no match is pending, so once one is, the b block
    // where the pending a block first matched; the tail restarts there.
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
// Adds a phase's result to the query's tally. Phases run in doc order, so ids arrive ascending.
static void emit(u32 q, u32 first, const u64 *rb, const u16 *rl, u32 rn) {
    Tally *t = &tally[q];
    u32 room = MAXQ - t->nid, nid = t->nid, cnt = rn, sum = 0;
    if (rb) {
        bsum(rb, W64, &cnt, &sum);
        for (u32 k = 0; k < W64 && nid < MAXQ; k++)
            for (u64 x = rb[k]; x && nid < MAXQ; x &= x - 1) t->top[nid++] = first + 64 * k + (u32)_tzcnt_u64(x);
    } else {
        for (u32 k = 0; k < rn; k++) sum += rl[k];
        for (u32 k = 0; k < rn && k < room; k++) t->top[nid++] = first + rl[k];
    }
    t->nid = nid; t->cnt += cnt; t->sum += sum + (u64)cnt * first;
}

// Strictly left to right. The running result is a sorted list while every term
// so far is a list, and a bitmap once a bitmap term joins by OR; a bitmap term
// filters a list under AND/NOT, and AND with a list turns a bitmap back into a list.
static void eval_range(Worker *me, u32 lo, u32 hi, u32 first) {
    const u32 ND = ndense;
    u64 *sa = me->sa, *sb = me->sb;
    for (u32 q = lo; q < hi; q++) {
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
            if (!rb && o == OR && i + 1 == qk[q] && tally[q].nid == MAXQ) {
                // last term of the query and we already hold 10 ids: only the union count and sum are needed
                u32 cn = filt(rl, rn, xl, xn, dl, 1), cs = 0;
                for (u32 k = 0; k < rn; k++) cs += rl[k];
                for (u32 k = 0; k < cn; k++) cs -= dl[k];
                for (u32 k = 0; k < xn; k++) cs += xl[k];
                Tally *t = &tally[q];
                cn = rn + xn - cn;
                t->cnt += cn; t->sum += cs + (u64)cn * first;
                goto next_query;
            }
            if (!rb && o == OR && rn + xn > 128) {
                memset(db, 0, (size_t)W64 * 8);
                for (u32 k = 0; k < rn; k++) db[rl[k] >> 6] |= 1ull << (rl[k] & 63);
                for (u32 k = 0; k < xn; k++) db[xl[k] >> 6] |= 1ull << (xl[k] & 63);
                rb = db;
                continue;
            }
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
        emit(q, first, rb, rl, rn);
next_query: ;
    }
}

// ---------------------------------------------------------------- merge, format
static char *put(char *o, u64 v) {
    char t[20];
    int n = 0;
    do t[n++] = (char)('0' + v % 10); while (v /= 10);
    while (n) *o++ = t[--n];
    return o;
}

static void finish(Worker *me, int c) {
    u32 lo = qbase[c], m = qbase[c + 1] - lo;
    char *o = me->out = arena((size_t)m * 160);
    for (u32 q = lo; q < lo + m; q++) {
        const Tally *t = &tally[q];
        o = put(o, t->cnt); *o++ = ' '; o = put(o, t->sum);
        for (u32 i = 0; i < t->nid; i++) { *o++ = ' '; o = put(o, t->top[i]); }
        *o++ = '\n';
    }
    me->outlen = (size_t)(o - me->out);
}

// ---------------------------------------------------------------- worker
static void *worker(void *arg) {
    int w = (int)(long)arg;
    Worker *me = &wk[w];
    me->keys = arena((size_t)8192 * 8 + (size_t)MAXTOK * 16 + ((size_t)MAXTOK + 8) * 16);   // two key buffers, ids, anchor positions, dense and rare hits
    me->ids = (u32 *)(me->keys + 8192 + 2 * MAXTOK); me->aps = me->ids + MAXTOK + 8; me->dl = me->aps + MAXTOK + 8; me->rl = me->dl + MAXTOK + 8;
    for (int k = 0; k < PH; k++) {             // this worker's stretch of each phase: starts at a line whose id is a multiple of 64
        int p = k * nw + w;
        size_t o = dlen * (size_t)p / (size_t)P;
        while (o && o < dlen && (doc[o - 1] != '\n' || (id_at(doc + o) & 63))) o++;
        pstart[p] = o;
        pfirst[p] = o < dlen ? id_at(doc + o) : ndocs;
    }
    // 1. queries: count this worker's lines, then parse them into the shared table
    u32 n = 0, base = 0;
    for (size_t o = (size_t)w << 21; o < sdfsize; o += (size_t)nw << 21) ((volatile u8 *)sdfs)[o] = 0;   // one thread per huge page
    for (size_t i = qs[w]; i < qs[w + 1]; i++) n += qtxt[i] == '\n';
    qcount[w] = n;
    pthread_barrier_wait(&bar);
    for (int i = 0; i < w; i++) base += qcount[i];
    qbase[w] = base;
    if (w == nw - 1) qbase[nw] = nq = base + n;
    parse(qtxt + qs[w], qtxt + qs[w + 1], base);
    pthread_barrier_wait(&bar);
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
        for (int i = 0; i < nw; i++) t += wk[i].sdf[s];
        sampdf[s] = t;
        if (tab[s >> 3].tag[s & 7]) me->cnt[cls(s, thr)]++;
    }
    pthread_barrier_wait(&bar);
    for (int i = 0; i < nw; i++)
        for (int c = 0; c < 3; c++) { tot[c] += wk[i].cnt[c]; if (i < w) next[c] += wk[i].cnt[c]; }
    next[1] += tot[0]; next[2] += tot[0] + tot[1];
    for (u32 s = lo; s < hi; s++) if (tab[s >> 3].tag[s & 7]) newid[s] = next[cls(s, thr)]++;
    if (!w) {
        ndense = tot[0]; nsparse = tot[1]; nterms = tot[0] + tot[1] + tot[2];
        if (nsparse > 65536) die("over 65,536 rare BOOL terms");
    }
    pthread_barrier_wait(&bar);
    // Phrase queries listed per anchor term: counted, offsets summed, filled. The
    // fill counts each offset back down, so aoff[v] ends as term v's first slot
    // and aoff[v + 1] > aoff[v] says that v anchors a phrase.
    for (u32 q = base; q < base + n; q++) if (qkind[q]) __atomic_fetch_add(&aoff[anchor_of(q, &pos)], 1, __ATOMIC_RELAXED);
    pthread_barrier_wait(&bar);
    if (!w) for (u32 i = 1; i <= nterms; i++) aoff[i] += aoff[i - 1];
    pthread_barrier_wait(&bar);
    for (u32 q = base; q < base + n; q++)
        if (qkind[q]) {
            u32 e = __atomic_sub_fetch(&aoff[anchor_of(q, &pos)], 1, __ATOMIC_RELAXED);
            aq[e] = q; aj[e] = pos | qk[q] << 8;
            const u32 *t = qterm + 8 * q;          // still slot numbers here
            an[e] = newid[t[pos + 1 < qk[q] ? pos + 1 : pos - 1]];
        }
    pthread_barrier_wait(&bar);
    for (u32 s = lo; s < hi; s++)
        if (tab[s >> 3].tag[s & 7]) { u32 v = newid[s]; tab[s >> 3].val[s & 7] = v | (aoff[v + 1] > aoff[v] ? ANCHOR : 0); }
    for (u32 q = base; q < base + n; q++) for (u32 j = 0; j < qk[q]; j++) qterm[8 * q + j] = newid[qterm[8 * q + j]];
    // 3. index and answer each part
    u32 maxnd = 1;
    for (int k = 0; k < PH; k++) if (pfirst[(k + 1) * nw] - pfirst[k * nw] > maxnd) maxnd = pfirst[(k + 1) * nw] - pfirst[k * nw];
    if (maxnd >= 65535) die("a phase holds over 65,534 documents");
    u32 W = (maxnd + 255) / 256 * 4;           // whole 32-byte blocks for bsum
    u8 *h = arena(((size_t)2 * W * 8 + 64 * (ndense / 64 + 1) * 8 + (ndense / 64 + 2) * 64 + 2 * ((size_t)maxnd * 2 + 64)) + 1024);
    me->sa = take(&h, (size_t)W * 8); me->sb = take(&h, (size_t)W * 8);
    me->bt = take(&h, 64 * (ndense / 64 + 1) * 8); me->db = take(&h, (ndense / 64 + 2) * 64);
    me->la = take(&h, maxnd * 2 + 64); me->lb = take(&h, maxnd * 2 + 64);
    me->sbase = arena((size_t)65 * SCAP * 4);
    if (!w) {
        W64 = W;
        bm = arena((size_t)ndense * W * 8);
        off = arena((size_t)nsparse * 4); end = arena((size_t)nsparse * 4);
        post = arena((size_t)65 * PCAP * 2);
        tally = arena((size_t)nq * sizeof(Tally));
    }
    pthread_barrier_wait(&bar);
    if (!w) munmap(sdfs, sdfsize);
    for (int k = 0; k < PH; k++) {
        int p = k * nw + w;
        me->nhit = 0;
        scan_part(me, p);
        uintptr_t a = ((uintptr_t)doc + pstart[p] + 4095) & ~(uintptr_t)4095, b = ((uintptr_t)doc + pstart[p + 1]) & ~(uintptr_t)4095;
        if (a < b) madvise((void *)a, b - a, MADV_DONTNEED);   // the text's page-table entries, dropped in parallel, not at exit
        pthread_barrier_wait(&bar);
        for (u32 c; (c = __atomic_fetch_add(&bclaim[k], 1, __ATOMIC_RELAXED)) < ((nsparse + (1u << SH) - 1) >> SH);) build(c);
        pthread_barrier_wait(&bar);
        if (!w)                                // phrase matches, each worker's ascending, the workers in doc order
            for (int v = 0; v < nw; v++)
                for (u32 i = 0; i < wk[v].nhit; i++) {
                    Tally *t = &tally[wk[v].hit[i].q];
                    u32 g = wk[v].hit[i].gid;
                    t->cnt++; t->sum += g;
                    if (t->nid < MAXQ) t->top[t->nid++] = g;
                }
        for (u32 c; (c = __atomic_fetch_add(&qclaim[k], 1, __ATOMIC_RELAXED)) * QB < nq;)
            eval_range(me, c * QB, (c + 1) * QB < nq ? (c + 1) * QB : nq, pfirst[k * nw]);
        pthread_barrier_wait(&bar);
    }
    // 4. format the queries this worker parsed
    finish(me, w);
    pthread_barrier_wait(&bar);
    return NULL;
}

static u32 id_at(const u8 *p) { u32 v = 0; while (*p >= '0' && *p <= '9') v = v * 10 + (u32)(*p++ - '0'); return v; }

int main(int argc, char **argv) {
    const char *in = argc > 1 ? argv[1] : "/data", *out = argc > 2 ? argv[2] : "/out";
    cpu_set_t cs;
    // workers: the hardware threads we may use (12 on the box)
    sched_getaffinity(0, sizeof cs, &cs);
    nw = getenv("SD_WORKERS") ? atoi(getenv("SD_WORKERS")) : CPU_COUNT(&cs);
    if (nw < 1 || nw > WMAX) nw = nw < 1 ? 1 : WMAX;
    SCAP = (1u << 20) / nw;
    doc = map_file(in, "docs.txt", &dlen);
    qtxt = map_file(in, "queries.txt", &qlen);
    for (int w = 0; w <= nw; w++) {            // query shares: byte ranges cut at line starts
        size_t o = qlen * (size_t)w / (size_t)nw;
        while (o > 0 && o < qlen && qtxt[o - 1] != '\n') o++;
        qs[w] = o;
    }
    size_t maxq = qlen / 7 + 1;                // the shortest query line is `BOOL a` + LF
    tab = huge(sizeof(Bucket) << BBITS);
    for (u32 b = 0; b <= BMASK; b++) tab[b].val[7] = MISS;
    for (u32 L = 0; L < 129; L++) memset(LM[L], 0xFF, L < 16 ? L : 16);
    sdfs = huge(sdfsize = (size_t)(nw + 8) * NSLOT);   // each worker's sample counts, their sums, the new ids
    sampdf = (u32 *)(sdfs + (size_t)nw * NSLOT); newid = sampdf + NSLOT;
    aoff = arena((size_t)NSLOT * 4 + 4); aq = arena(maxq * 4); aj = arena(maxq * 4); an = arena(maxq * 4);
    qterm = arena(maxq * 32); qop = arena(maxq * 8); qk = arena(maxq); qkind = arena(maxq);
    size_t t = dlen - 1;
    while (t > 0 && doc[t - 1] != '\n') t--;
    ndocs = id_at(doc + t) + 1;
    PH = (int)((ndocs + PHDOCS - 1) / PHDOCS);
    P = nw * PH;
    pstart = malloc((P + 1) * sizeof *pstart); pfirst = malloc((P + 1) * 4);
    pstart[P] = dlen; pfirst[P] = ndocs;
    for (u32 m = 0; m < 256; m++)
        for (u32 l = 0, k = 0; l < 8; l++) if (m >> l & 1) tbl[m][2 * k] = (u8)(2 * l), tbl[m][2 * k++ + 1] = (u8)(2 * l + 1);
    for (u32 m = 0; m < 256; m++) for (u32 l = 0, k = 0; l < 8; l++) if (m >> l & 1) PACK[m][k++] = l;
    pthread_barrier_init(&bar, NULL, (unsigned)nw);
    pthread_t th[WMAX];
    for (int w = 1; w < nw; w++) pthread_create(&th[w], NULL, worker, (void *)(long)w);
    worker(NULL);
    for (int w = 1; w < nw; w++) pthread_join(th[w], NULL);
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
