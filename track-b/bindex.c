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
typedef __m256i V;
#define S32(x) _mm256_set1_epi32((int)(x))
#define S8(x) _mm256_set1_epi8(x)
#define LU(p) _mm256_loadu_si256((const __m256i *)(p))
#define SU(p, x) _mm256_storeu_si256((__m256i *)(p), x)
#define MSK(x) (u32) _mm256_movemask_ps(_mm256_castsi256_ps(x))
#define MB(x) (u64)(u32) _mm256_movemask_epi8(x)
#define PERM(x, m) _mm256_permutevar8x32_epi32(x, _mm256_load_si256((const __m256i *)PACK[m]))
#define ORV _mm256_or_si256
#define ANDV _mm256_and_si256
#define SHUF _mm256_shuffle_epi8
#define ADD8 _mm256_add_epi8
#define ADD64 _mm256_add_epi64

// docs per phase at most, so a phase-local doc number fits a u16 below the 0xFFFF sentinel
#define PHDOCS 63000
#define SH 11             // term shards: 1 << SH terms each
// entries per worker per shard (address space only), set in main: 2^20 / workers
static uint32_t SCAP;
#define PCAP (1u << 22)   // list entries per shard, address space only
#define QB 64             // queries per claim
// tokens per doc; the spec's longest doc (999 units) has ~10,000
#define MAXTOK 65536
#define RB 2048            // dense and rare hits gathered over documents before they are applied
#define BT 1024             // tokens per batch, at least
#define MT (MAXTOK + 16 + RB + BT)
#define KS (4096 + MAXTOK)      // keys and doc numbers per batch buffer
#ifndef BBITS
#define BBITS 15          // term table: 2^15 buckets of 7 tags
#endif
#define NSLOT (8u << BBITS)
#define DENSE_DIV 32      // a BOOL term in over 1/32 of the sample gets a bitmap row
#define MAXQ 10           // ids printed per query
#define WMAX 64
#define ANCHOR (1u << 31) // table value flag: phrases are checked at this term
#define MISS 0x7FFFFFFFu  // lookup value of a token that is no query term
#define PROBE 0x7FFFFFFEu // a bucket's miss value once a key was probed past it


static int die(const char *m) { fprintf(stderr, "bindex: %s\n", m); exit(1); }
static u32 claim(u32 *c) { return __atomic_fetch_add(c, 1, __ATOMIC_RELAXED); }

enum { AND = 1, OR, NOT };
// phrase query q matches document gid
typedef struct { u32 q, gid; } Hit;
// a query's answer so far
typedef struct { u64 sum; u32 cnt, nid, top[10]; } Tally;
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
    _Alignas(64) u64 *keys, *cw, *sa, *sb;
    // sp: (65536 >> SH) + 1 shard cursors
    u8 *sdf; u32 *ids, *dk, *aps, *dl, *rl, *sbase, *sp[33], nsd, nhit, hcap, cnt[3];
    u16 *la, *lb;
    Hit *hit;
    char *out; size_t outlen;
} Worker;

static Worker wk[WMAX];
// wmem: all workers' memory, WS bytes of address space each: keys, ids, aps, dl, rl, sa, sb, la, lb, sbase, cw, out
static u8 *wmem;
static size_t WS;
#define CK 4               // parts per worker per phase
// workers, parts, phases, parts per phase
static int nw, P, PH, NC;
// each part's ends in its worker's streams and hit list: [0] where it began, [1] where it ended
static u32 *cs[2][WMAX * CK][(65536 >> SH) + 1], ch[2][WMAX * CK], sclaim[256];
// the part's worker
static u8 co[WMAX * CK];
static const u8 *doc, *qtxt;
static size_t dlen, qlen, qs[WMAX + 1], *pstart;
static u32 ndocs, nq, ndense, nsparse, nterms, qc[WMAX], qb[WMAX + 1], *pfirst;
// query q: terms at qterm[8q..], op before term i at qop[8q+i]
static u8 *qkind, *qk, *qop, bflag[NSLOT];
static u8 *sdfs; static u32 *qterm, *sampdf, *newid, *aoff, *aq, *aj, *an;
static size_t sdfsize;
// the phase's bitmap rows, W64 words each
static u64 *bm;
// a list term's list: post[off[s] .. end[s])
static u32 *off, *end, W64, bclaim[256], qclaim[256], pcur[256];
static u16 *post;
static Tally *tally;
static pthread_barrier_t bar;

// n bytes; on 2 MB-aligned huge pages when hp. Regular pages ask for NOHUGEPAGE:
// a huge page would charge its untouched rest to peak memory.
#define MF (MAP_PRIVATE | MAP_ANONYMOUS)
#define HPV ((size_t)1 << 29)
static void *arena(size_t n, int hp) {
    size_t a = hp ? (size_t)2 << 20 : 64;
    u8 *p = mmap(NULL, n + a, PROT_READ | PROT_WRITE, MF, -1, 0);
    p != MAP_FAILED || die("mmap");
    p = (u8 *)(((uintptr_t)p + a - 1) & ~(uintptr_t)(a - 1));
    return madvise(p, n, hp ? MADV_HUGEPAGE : MADV_NOHUGEPAGE), p;
}

static const u8 *map_file(const char *dir, const char *name, size_t *len) {
    char path[4096];
    snprintf(path, sizeof path, "%s/%s", dir, name);
    int fd = open(path, O_RDONLY);
    struct stat st;
    if (fd < 0 || fstat(fd, &st)) die(path);
    // a zero page after the file keeps the 64-byte loads past its end in bounds
    u8 *b = mmap(NULL, (*len = (size_t)st.st_size) + 4096, PROT_READ, MF, -1, 0);
    b != MAP_FAILED && (!*len || mmap(b, *len, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 0) != MAP_FAILED) || die(path);
    return close(fd), b;
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
typedef struct { _Alignas(64) u32 tag[8]; u32 val[8]; } Bucket;
// on one huge page: lookups take no TLB misses
static _Alignas(1 << 21) Bucket tab[1u << BBITS];
#define BMASK ((1u << BBITS) - 1)
#define BOFF ((u64)BMASK << 6)           // a key's bits 6..20: its bucket's byte offset; its high half: the tag
#define BKT(h) ((const Bucket *)((const u8 *)tab + (u32)(h)))
#define TG(s) tab[s >> 3].tag[s & 7] // slot s's tag and value, without a lookup
#define TV(s) tab[s >> 3].val[s & 7]

// LM[L]: first L bytes 0xFF (a table, not a clamp: gcc makes the clamp a branch)
static _Alignas(16) u8 LM[129][16];
static inline u64 hash_at(const u8 *p, u64 L) {
    const u8 *M = LM[0];
    // LM[L] as a byte offset
    L <<= 4;
    __m128i x = _mm_and_si128(_mm_or_si128(ld128(p), _mm_set1_epi8(32)), _mm_load_si128((const __m128i *)(M + L)));
    if (__builtin_expect(L > 256, 0)) x = _mm_xor_si128(x, _mm_aesenc_si128(_mm_and_si128(_mm_or_si128(ld128(p + 16), _mm_set1_epi8(32)), _mm_load_si128((const __m128i *)(M + L - 256))), _mm_set1_epi32(0x3C6EF372)));
    const __m128i K = _mm_set_epi64x(0x9E3779B97F4A7C15ll, 0x243F6A8885A308D3ll);
    return (u64)_mm_cvtsi128_si64(_mm_and_si128(_mm_aesenc_si128(_mm_aesenc_si128(x, K), K), _mm_set_epi64x(0, (long long)(BOFF | ~(u64)0 << 32))));
}

// the tag may sit in a later bucket
static u32 probe(u64 h, u32 b) {
    u32 g = h >> 32;
    for (b = (b + 1) & BMASK;; b = (b + 1) & BMASK)
        for (u32 i = 0; i < 7; i++)
            if (tab[b].tag[i] == g) return tab[b].val[i];
            else if (i == 6 && tab[b].val[7] != PROBE) return MISS;
}

static inline u32 lookup(const u64 *ph) {
    const Bucket *t = BKT(*ph);
    u32 m = MSK(_mm256_cmpeq_epi32(_mm256_load_si256((const V *)t->tag), S32(((const int *)ph)[1])));
    return t->val[_tzcnt_u32(m | 128)];
}
#define LOOK(ph, v) (__builtin_expect((v) == PROBE, 0) ? probe(*(ph), (u32)(*(ph) >> 6) & BMASK) : (v))

// Called by all workers at once while they parse: a free tag slot is claimed by
// compare-and-swap. Relaxed order is enough: no worker looks a term up before
// the barrier after the parse.
static u32 intern(u64 h) {
    for (u32 g = h >> 32, b = h >> 6 & BMASK, i;; b = (b + 1) & BMASK) {
        for (i = 0; i < 7; i++) {
            u32 cur = __atomic_load_n(&tab[b].tag[i], 0);
            if (!cur && __atomic_compare_exchange_n(&tab[b].tag[i], &cur, g, 0, 0, 0)) return tab[b].val[i] = b * 8 + i;
            if (cur == g) return b * 8 + i;
        }
        __atomic_store_n(&tab[b].val[7], PROBE, 0);
    }
}

// ---------------------------------------------------------------- tokenizer
// token bytes of q[0..64): 0-9, a-z, A-Z
static inline u64 tm(const u8 *q) {
    // range tests by a shift to the bottom of the signed bytes: x - '0' - 128 < -118 is a digit, (x | 32) - 'a' - 128 < -102 a letter
    u64 m = 0;
    for (int h = 0; h < 2; h++) {
        V v = LU(q + 32 * h), d = ADD8(v, S8(0x50));
        V a = ADD8(ORV(v, S8(32)), S8(0x1F));
        d = _mm256_cmpgt_epi8(S8(-118), d);
        a = _mm256_cmpgt_epi8(S8(-102), a);
        m |= MB(ORV(d, a)) << 32 * h;
    }
    return m;
}
static inline u64 lfm(const u8 *q) {
    return MB(_mm256_cmpeq_epi8(LU(q), S8('\n')))
         | MB(_mm256_cmpeq_epi8(LU(q + 32), S8('\n'))) << 32;
}

// Tokenizes the text from q to its LF into keys[]; returns the byte after the LF. Each
// round reads the 64 bytes at q, always hashes TK tokens (past the last real one it hashes
// garbage that n never counts) and moves q to the first token it did not take, so q never
// sits inside a token and no per-window loop exit can mispredict. A run reaching the
// window's end waits for the next round; only a token of 64 bytes or more needs a branch.
// Loads may run past the LF and the file end.
#define TK 8
static const u8 *tokenize(const u8 *q, u64 *keys, u32 *pn, u32 *dk, u32 d) {
    u32 n = 0;
    const V DV = S32(d);
    for (;;) {
        // bytes before the LF
        u64 lf = lfm(q), mm = tm(q) & ((lf & -lf) - 1);
        u64 nm = ~mm, cut = 64 - _lzcnt_u64(nm);
        u64 s0 = _bzhi_u64(mm & ~(mm << 1), (u32)cut), s = s0, *kp = keys + n;
        SU(dk + n, DV);
        for (u32 j = 0; j < TK; j++, s &= s - 1) {
            u64 st = _tzcnt_u64(s), L = _tzcnt_u64(nm >> st);
            kp[j] = hash_at(q + st, L);
        }
        n += (u32)_mm_popcnt_u64(s0 ^ s);
        if (lf && !s) { *pn = n; return q + _tzcnt_u64(lf) + 1; }
        // 64 token bytes: the hash reads 32 at most
        if (!cut) {
            dk[n] = d, keys[n++] = hash_at(q, 64);
            while (!~tm(q)) q += 64;
            q += _tzcnt_u64(~tm(q));
            continue;
        }
        u64 nx = _tzcnt_u64(s);
        q += nx < cut ? nx : cut;
    }
}
// A docs.txt line is its id, a TAB and the text; ids run 0 to N-1 in file order (spec).
static u32 id_at(const u8 *p) { u32 v = 0; while (*p - 48u < 10) v = v * 10 + *p++ - 48; return v; }
#define skip_id(q) ({ const u8 *_s = (q); while (*_s ^ 9) _s++; _s + 1; })

// the first offset at or after o that starts a line of b[0..len) whose id is a multiple of m (m 1: any line)
static size_t nxt(size_t o, size_t len, const u8 *b, u32 m) {
    while (o && o < len && (b[o - 1] != '\n' || (m > 1 && id_at(b + o) % m))) {
        const u8 *l = memchr(b + o, '\n', len - o);
        o = l ? (size_t)(l - b) + 1 : len;
    }
    return o;
}

// ---------------------------------------------------------------- queries
static void parse(const u8 *p, const u8 *e, u32 q) {
    for (u32 ph, k; p < e; q++) {
        // past `PHRASE "` or `BOOL `
        qkind[q] = (u8)(ph = *p == 'P'), p += ph ? 8 : 5, k = 0;
        for (;;) {
            u32 s;
            // the spec caps a query at 8 terms
            qterm[8 * q + k++] = s = intern(hash_at(p, _tzcnt_u64(~tm(p))));
            if (!ph) __atomic_store_n(&bflag[s], 1, 0);
            if (*(p += _tzcnt_u64(~tm(p))) != ' ') break;
            ph || (qop[8 * q + k] = p[1] == 'A' ? AND : p[1] == 'O' ? OR : NOT);
            p += ph ? 1 : p[1] == 'O' ? 4 : 5;
        }
        // the same documents as a BOOL term
        if (k == 1) qkind[q] = 0, __atomic_store_n(&bflag[qterm[8 * q]], 1, 0);
        qk[q] = (u8)k, p += ph ? 2 : 1;
    }
}

// ---------------------------------------------------------------- sampling, renumbering
// Document frequency of every term over the first 1/64 of the worker's first
// part; sdf[slot] = tokens counted, stopping at 255.
static void sample(Worker *me, int w) {
    const u8 *q = doc + pstart[w * CK], *end = q + (pstart[w * CK + 1] - pstart[w * CK]) / 12;
    u32 n; u8 *sdf = me->sdf = sdfs + (size_t)w * NSLOT;
    for (me->nsd = 0; q < end; me->nsd++) {
        q = tokenize(skip_id(q), me->keys, &n, me->dk, 0);
        for (u32 i = 0, v; i < n; i++) if ((v = lookup(me->keys + i)), (v = LOOK(me->keys + i, v)) != MISS) sdf[v] += sdf[v] < 255;
    }
}

// the phrase term with the smallest sampled df
static u32 anchor_of(u32 q, u32 *pj) {
    u32 *t = qterm + 8 * q, j = 0;
    for (u32 i = 1; i < qk[q]; i++) j = sampdf[t[i]] < sampdf[t[j]] ? i : j;
    return newid[t[*pj = j]];
}

// A term's class for the renumbering: 0 a BOOL term with a bitmap row (sampled
// in over 1/DENSE_DIV of the documents), 1 another BOOL term (posting list),
// 2 a phrase-only term. New ids run class by class, in slot order.
static inline u32 cls(u32 s, u32 t) { return 2 - bflag[s] * (2 - (sampdf[s] <= t)); }

// ---------------------------------------------------------------- scan
static void match_phrases(Worker *me, u32 na, const u32 *dk, u32 pf) {
    const u32 *ids = me->ids;
    u32 h0 = me->nhit, g0 = 0;
    for (u32 a = 0; a < na; a++) _mm_prefetch((const char *)(aoff + (ids[me->aps[a]] & ~ANCHOR)), _MM_HINT_T0);
    for (u32 a = 0; a < na; a++) {
        long pos = me->aps[a], t = ids[pos] & ~ANCHOR;
        for (u32 e = aoff[t]; e < aoff[t + 1]; e++) {
            u32 j = aj[e] & 255, k = aj[e] >> 8;
            if ((ids[pos + 1 - 2 * (j + 1 == k)] & ~ANCHOR) != an[e]) continue;
            u32 q = aq[e], x = 0, gid = pf + dk[pos] + 1;
            const u32 *qt = qterm + 8 * q, *dt = ids + pos - j;
            while (x < k && (dt[x] & ~ANCHOR) == qt[x]) x++;
            if (x < k) continue;
            if (gid != g0) g0 = gid, h0 = me->nhit;
            for (x = h0; x < me->nhit && me->hit[x].q != q; x++) {}
            // this doc already matched q
            if (x < me->nhit) continue;
            if (me->nhit == me->hcap) me->hit = realloc(me->hit, (me->hcap = 2 * me->hcap + 4096) * sizeof(Hit));
            me->hit[me->nhit++] = (Hit){ q, gid - 1 };
        }
    }
}

// Tokenizes documents *pd.. into K until BT tokens, a row's end (d a multiple of 64) or de. Each document ends in a
// sentinel key: bucket 0, tag 0, which answers MISS, so no phrase matches across it.
static const u8 *batch(const u8 *q, u64 *K, u32 *dk, u32 *pd, u32 de, u32 *pn) {
    u32 n = 0, m, d = *pd;
    do {
        q = tokenize(skip_id(q), K + n, &m, dk + n, d), n += m;
        K[n++] = 0;
    } while (++d < de && d & 63 && n < BT);
    *pd = d, *pn = n;
    return q;
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
static void scan_part(Worker *me, int p) {
    const u32 ND = ndense, NS = nsparse, pf = pfirst[p / NC * NC], c0 = p % NC, d0 = pfirst[p] - pf, de = pfirst[p + 1] - pf;
    u64 *cw = me->cw, *K = me->keys, *K2 = K + KS, *ks;
    u32 *ids = me->ids, *aps = me->aps, *dl = me->dl, *rl = me->rl, **sp = me->sp, *dk = me->dk, *dk2 = dk + KS, *ds, n = 0, nn, d = d0, dn;
    size_t ndh = 0, nr = 0;
    const u8 *q = doc + pstart[p];
    const V NDN = S32(ND), NSN = S32((NS + 0x80000000u)), NDB = S32((ND - 0x80000000u));
    memcpy(cs[0][c0], sp, sizeof me->sp), ch[0][c0] = me->nhit, co[c0] = (u8)(me - wk);
    if (d0 < de) q = batch(q, K, dk, &d, de, &n);
    while (n) {
        // the next batch's first keys follow this one's, for the prefetch ahead
        dn = d, nn = 0;
        if (d < de) q = batch(q, K2, dk2, &d, de, &nn);
        memcpy(K + n, K2, AHEAD * 8);
        const u64 *cur = K;
        size_t na = 0;
        V pos = _mm256_setr_epi32(0, 1, 2, 3, 4, 5, 6, 7);
        for (size_t i = 0; i < n; i++) {
            // AHEAD tokens ahead, into the next batch
            _mm_prefetch((const char *)BKT(cur[i + AHEAD]), _MM_HINT_T0);
            ids[i] = lookup(cur + i);
        }
        // the last block's padding lanes count as misses
        SU(ids + n, S32(MISS));
        // blocks of 8: anchor, dense and rare lanes as masks, dense and rare hits left-packed
        for (size_t i = 0; i < n; i += 8) {
            V v = LU(ids + i);
            if (__builtin_expect(_mm256_movemask_epi8(_mm256_cmpeq_epi32(v, S32(PROBE))), 0)) {
                for (u32 l = 0; l < 8 && i + l < n; l++) ids[i + l] = LOOK(cur + i + l, ids[i + l]);
                v = LU(ids + i);
            }
            V id = ANDV(v, S32(~ANCHOR)), s = _mm256_sub_epi32(id, NDB);
            u32 dm = MSK(_mm256_cmpgt_epi32(NDN, id));
            // s is biased: id - ND + 2^31 < NS + 2^31 as signed
            u32 rm = MSK(_mm256_cmpgt_epi32(NSN, s));
            const V DV = LU(dk + i);
            // a stream entry: term << 16 | doc
            s = ORV(_mm256_slli_epi32(s, 16), DV);
            SU(dl + ndh, PERM(ORV(_mm256_slli_epi32(id, 6), ANDV(DV, S32(63))), dm));
            SU(rl + nr, PERM(s, rm));
            ndh += (u32)_mm_popcnt_u32(dm); nr += (u32)_mm_popcnt_u32(rm);
            // anchor lanes, left-packed like the rest: this loop mispredicted
            u32 am = MSK(v);
            SU(aps + na, PERM(pos, am));
            na += (u32)_mm_popcnt_u32(am);
            pos = _mm256_add_epi32(pos, S32(8));
        }
        // a row word is complete every 64 documents
        int last = dn == de, row = last || !(dn & 63);
        if (nr > RB || last) {
            for (size_t k = 0; k < nr; k++) { u32 e = rl[k]; *sp[e >> (16 + SH)]++ = e; }
            nr = 0;
        }
        if (ndh > RB || row) {
            for (size_t k = 0; k < ndh; k++) cw[dl[k] >> 6] |= 1ull << (dl[k] & 63);
            ndh = 0;
        }
        if (row) {
            for (u32 c = 0; c < ND; c++) bm[(size_t)c * W64 + ((dn - 1) >> 6)] = cw[c];
            memset(cw, 0, ND * 8ull);
        }
        if (na) match_phrases(me, (u32)na, dk, pf);
        ks = K, K = K2, K2 = ks, ds = dk, dk = dk2, dk2 = ds, n = nn;
    }
    memcpy(cs[1][c0], sp, sizeof me->sp), ch[1][c0] = me->nhit;
    // the phase's last stretch: rows past its last document stay empty
    if (c0 == NC - 1)
        for (u32 r = 0; r < ND; r++) memset(bm + (size_t)r * W64 + (de + 63) / 64, 0, (W64 - (de + 63) / 64) * 8);
}

// The lists of shard c: counted, then filled in worker order, which is document
// order. Each list follows a 0xFFFF sentinel, so a repeat of a doc is dropped by
// comparing with the last.
static void build(u32 c, int k) {
    u32 lo = c << SH, hi = (c + 1) << SH, o = 0;
    if (hi > nsparse) hi = nsparse;
    memset(end + lo, 0, (hi - lo) * 4);
    for (int w = 0; w < NC; w++) {
        const u32 *e = cs[0][w][c], *x = cs[1][w][c];
        o += (u32)(x - e);
        for (; e < x; e++) end[*e >> 16]++;
    }
    o = __atomic_fetch_add(&pcur[k], o + hi - lo, __ATOMIC_RELAXED);
    for (u32 s = lo; s < hi; s++) { post[o] = 0xFFFF; off[s] = ++o; o += end[s]; end[s] = off[s]; }
    for (int w = 0; w < NC; w++)
        for (const u32 *e = cs[0][w][c], *x = cs[1][w][c]; e < x; e++) {
            u32 s = *e >> 16, i = end[s];
            post[i] = (u16)*e;
            end[s] = i + (post[i - 1] != (u16)*e);
        }
}

// ---------------------------------------------------------------- BOOL answers
static inline u32 bit(const u64 *b, u32 i) { return b[i >> 6] >> (i & 63) & 1; }

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
    const V PC = _mm256_broadcastsi128_si256(ld128("\0\1\1\2\1\2\2\3\1\2\2\3\2\3\3\4")),
                  PS = _mm256_broadcastsi128_si256(ld128("\0\0\1\1\2\2\3\3\3\3\4\4\5\5\6\6")),
                  PH = ADD8(PS, _mm256_slli_epi16(PC, 2)), M = S8(15), Z = _mm256_setzero_si256(),
                  WT = LU("\0\1\2\3\4\5\6\7\10\11\12\13\14\15\16\17\20\21\22\23\24\25\26\27\30\31\32\33\34\35\36\37");
    V aC = Z, aB = Z, aQ = Z, aW = Z;
    for (u32 t = 0; t < w / 4; t++) {
        V v = LU(b + 4 * t), lo = ANDV(v, M), hi = ANDV(_mm256_srli_epi64(v, 4), M);
        V pc = ADD8(SHUF(PC, lo), SHUF(PC, hi)), s = _mm256_sad_epu8(pc, Z);
        aC = ADD64(aC, s);
        aB = ADD64(aB, _mm256_mul_epu32(s, _mm256_set1_epi64x(t)));
        aQ = ADD64(aQ, _mm256_sad_epu8(ADD8(SHUF(PS, lo), SHUF(PH, hi)), Z));
        aW = _mm256_add_epi32(aW, _mm256_madd_epi16(_mm256_maddubs_epi16(pc, WT), _mm256_set1_epi16(1)));
    }
    u64 c[4], k[4], q[4]; u32 x[8];
    SU(c, aC); SU(k, aB);
    SU(q, aQ); SU(x, aW);
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
    // j0: the next b block while no match is pending, so once one is, the b block
    // where the pending a block first matched; the tail restarts there.
    for (; an * 16 >= bn && i + 8 <= an && j + 8 <= bn; j0 = acc ? j0 : j) {
        __m128i A = ld128(a + i);
        u32 x = a[i + 7], y = b[j + 7];
        acc |= (u32)_mm_cvtsi128_si32(_mm_cmpestrm(ld128(b + j), 8, A, 8, _SIDD_UWORD_OPS | _SIDD_CMP_EQUAL_ANY));
        // as arithmetic: x <= y is a coin flip
        u32 adv = -(u32)(x <= y), m = (acc ^ (keep - 1)) & 0xFF & adv;
        _mm_storeu_si128((__m128i *)(d + n), _mm_shuffle_epi8(A, ld128(tbl[m])));
        n += (u32)_mm_popcnt_u32(m); i += 8 & adv; acc &= ~adv;
        j += (u32)(y <= x) * 8;
    }
    j = j0 < j ? j0 : j;
    if ((an - i) * 8 < bn - j)
        for (; i < an; i++) { u32 l = j + lower(b + j, bn - j, a[i]); d[n] = a[i]; n += (l < bn && b[l] == a[i]) == keep; }
    for (; i < an && j < bn;) { u16 x = a[i], y = b[j]; d[n] = x; n += keep ? x == y : x < y; i += x <= y; j += y <= x; }
    while (!keep && i < an) d[n++] = a[i++];
    return n;
}
static u32 unite(const u16 *a, u32 an, const u16 *b, u32 bn, u16 *d) {
    u32 i = 0, j = 0, n = 0;
    while (i < an && j < bn) { u16 x = a[i], y = b[j]; d[n++] = x < y ? x : y; i += x <= y; j += y <= x; }
    for (; i < an; d[n++] = a[i++]);
    for (; j < bn; d[n++] = b[j++]);
    return n;
}
// sum of the first n ids of l
static u32 suml(const u16 *l, u32 n) {
    u32 s = 0;
    for (u32 k = 0; k < n; k++) s += l[k];
    return s;
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
        sum = suml(rl, rn);
        for (u32 k = 0; k < rn && k < room; k++) t->top[nid++] = first + rl[k];
    }
    t->nid = nid, t->cnt += cnt, t->sum += sum + (u64)cnt * first;
}

// a set bit per id of l in db
static void setb(u64 *db, const u16 *l, u32 n) { for (u32 k = 0; k < n; k++) db[l[k] >> 6] |= 1ull << (l[k] & 63); }

// db = rb | xb, or rb & (xb ^ iv) for AND / AND-NOT of two rows
static void opw(u64 *d, const u64 *a, const u64 *b, u32 o) {
    u64 iv = o == NOT ? ~0ull : 0;
    for (u32 k = 0; k < W64; k++) d[k] = o == OR ? a[k] | b[k] : a[k] & (b[k] ^ iv);
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
        const u16 *rl = t[0] < ND ? me->la : post + off[t[0] - ND];
        u32 rn = t[0] < ND ? 0 : end[t[0] - ND] - off[t[0] - ND];
        const u64 *rb = t[0] < ND ? bm + (size_t)t[0] * W64 : NULL;
        for (u32 i = 1; i < qk[q]; i++) {
            u32 o = qop[8 * q + i], x = t[i], m = 0;
            // an empty result stays empty
            if (!rb && !rn && o != OR) continue;
            u16 *dl = rl == me->la ? me->lb : me->la;
            u64 *db = rb == sa ? sb : sa;
            if (x < ND) {
                const u64 *xb = bm + (size_t)x * W64;
                if (rb) opw(db, rb, xb, o), rb = db;
                else if (o == OR) memcpy(db, xb, (size_t)W64 * 8), setb(db, rl, rn), rb = db;
                else {
                    for (u32 k = 0, keep = o == AND; k < rn; k++) m += (dl[m] = rl[k], bit(xb, rl[k]) == keep);
                    rl = dl, rn = m;
                }
            } else {
                const u16 *xl = post + off[x - ND];
                u32 xn = end[x - ND] - off[x - ND];
                if (!rb && o == OR && i + 1 == qk[q] && tally[q].nid == MAXQ) {
                    // last term of the query and we already hold 10 ids: only the union count and sum are needed
                    u32 n2 = filt(rl, rn, xl, xn, dl, 1), cn = rn + xn - n2;
                    u64 cs = (u64)suml(rl, rn) + suml(xl, xn) - suml(dl, n2);
                    tally[q].cnt += cn, tally[q].sum += cs + (u64)cn * first;
                    goto next_query;
                }
                if (!rb && o == OR && rn + xn > 128) {
                    memset(db, 0, (size_t)W64 * 8), setb(db, rl, rn), setb(db, xl, xn);
                    rb = db;
                    continue;
                }
                if (!rb) rn = o == OR ? unite(rl, rn, xl, xn, dl) : filt(rl, rn, xl, xn, dl, o == AND), rl = dl;
                else if (o == AND) {
                    for (u32 k = 0; k < xn; k++) m += (dl[m] = xl[k], bit(rb, xl[k]));
                    rl = dl, rn = m, rb = NULL;
                } else {
                    memcpy(db, rb, (size_t)W64 * 8), rb = db;
                    if (o == OR) setb(db, xl, xn);
                    else for (u32 k = 0; k < xn; k++) db[xl[k] >> 6] &= ~(1ull << (xl[k] & 63));
                }
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
    u32 lo = qb[c], m = qb[c + 1] - lo;
    char *o = me->out = (char *)(me->cw + NSLOT);
    for (u32 q = lo; q < lo + m; q++) {
        const Tally *t = &tally[q];
        o = put(o, t->cnt), *o++ = ' ', o = put(o, t->sum);
        for (u32 i = 0; i < t->nid; i++) *o++ = ' ', o = put(o, t->top[i]);
        *o++ = '\n';
    }
    me->outlen = (size_t)(o - me->out);
}

// ---------------------------------------------------------------- worker
static pthread_t th[WMAX];
// the loops over this worker's slot range and its parsed queries
#define FOR for (u32 s = lo, n; s < hi; s++)
#define FQ for (u32 q = base; q < base + n; q++)
static void *worker(void *arg) {
    int w = (int)(long)arg;
    for (int c = 2 * w + 1; c <= 2 * w + 2 && c < nw; c++) pthread_create(&th[c], NULL, worker, (void *)(long)c);
    Worker *me = &wk[w];
    // two key buffers, then ids, anchor positions, dense and rare hits
    me->keys = (u64 *)(wmem + w * WS);
    u32 *z = me->ids = (u32 *)(me->keys + 2 * KS);
    z[-1] = MISS, me->dk = z + 4 * MT;
    me->aps = z + MT, me->dl = z + 2 * MT, me->rl = z + 3 * MT;
    // parts start at a line whose id is a multiple of 64
    for (int p = w; p < P; p += nw) {
        size_t o = nxt(dlen * (size_t)p / (size_t)P, dlen, doc, 64);
        pstart[p] = o, pfirst[p] = o < dlen ? id_at(doc + o) : ndocs;
    }
    // 1. queries: count this worker's lines, then parse them into the shared table
    u32 n = 0, base = 0;
    // one thread per huge page
    for (size_t o = w << 21; o < sdfsize; o += nw << 21) sdfs[o] = 0;
    // n ends as the lines of this worker's share, base as the lines before it
    for (size_t i = qs[w]; i < qs[w + 1]; i++) n += qtxt[i] == '\n';
    qc[w] = n, pthread_barrier_wait(&bar);
    for (u32 i = 0; i < w; i++) base += qc[i];
    qb[w] = base, w == nw - 1 && (qb[nw] = nq = base + n);
    parse(qtxt + qs[w], qtxt + qs[w + 1], base), pthread_barrier_wait(&bar);
    // 2. sample, sum the samples by slot range, renumber, rewrite the table
    sample(me, w), pthread_barrier_wait(&bar);
    // Each worker sums the samples over one slot range and counts its terms by
    // class; the counts' prefix sums give each range its first new ids.
    u32 lo = NSLOT * w / nw, hi = NSLOT * (w + 1) / nw, thr = 0, pos;
    u32 next[3] = { 0 }, tot[3] = { 0 };
    for (int i = 0; i < nw; i++) thr += wk[i].nsd;
    for (int i = 0; i < nw; i++) FOR sampdf[s] += wk[i].sdf[s];
    thr /= DENSE_DIV;
    FOR me->cnt[cls(s, thr)] += TG(s) != 0;
    pthread_barrier_wait(&bar);
    for (int i = 0; i < nw; i++)
        for (int c = 0; c < 3; c++) tot[c] += wk[i].cnt[c], next[c] += i < w ? wk[i].cnt[c] : 0;
    next[1] += tot[0], next[2] += tot[0] + tot[1];
    FOR { u32 c = cls(s, thr); newid[s] = next[c], next[c] += TG(s) != 0; }
    if (!w) ndense = tot[0], nsparse = tot[1], nterms = tot[0] + tot[1] + tot[2],
        nsparse <= 65536 || die("over 65,536 rare BOOL terms");
    pthread_barrier_wait(&bar);
    // Phrase queries listed per anchor term: counted, offsets summed, filled. The
    // fill counts each offset back down, so aoff[v] ends as term v's first slot
    // and aoff[v + 1] > aoff[v] says that v anchors a phrase.
    FQ qkind[q] && claim(&aoff[anchor_of(q, &pos)]);
    pthread_barrier_wait(&bar);
    // prefix sums: per range, then each range's base
    u32 al = (nterms + 1) * (u64)w / nw, ah = (nterms + 1) * (u64)(w + 1) / nw, acc = 0;
    for (u32 i = al; i < ah; i++) aoff[i] = acc += aoff[i];
    me->cnt[0] = acc, pthread_barrier_wait(&bar);
    for (int i = acc = 0; i < w; i++) acc += wk[i].cnt[0];
    for (u32 i = al; i < ah; i++) aoff[i] += acc;
    pthread_barrier_wait(&bar);
    FQ if (qkind[q]) {
            u32 e = __atomic_sub_fetch(&aoff[anchor_of(q, &pos)], 1, 0);
            // an still holds a slot number here
            aq[e] = q, aj[e] = pos | qk[q] << 8, an[e] = newid[qterm[8 * q + (pos + 1 < qk[q] ? pos + 1 : pos - 1)]];
        }
    pthread_barrier_wait(&bar);
    FOR { u32 v = newid[s] | (u32)(aoff[newid[s] + 1] > aoff[newid[s]]) << 31; TV(s) = TG(s) ? v : TV(s); }
    for (u32 *t = qterm + 8 * base, *e = t + 8 * n; t < e; t++) *t = newid[*t];
    // 3. index and answer each part
    u32 maxnd = 1, sz, nsp = (nsparse + 2047) >> SH;
    for (u32 k = 0; k < (u32)P; k += NC) maxnd = (sz = pfirst[k + NC] - pfirst[k]) > maxnd ? sz : maxnd;
    maxnd < 65535 || die("a phase holds over 65,534 documents");
    // whole 32-byte blocks for bsum
    u32 W = (maxnd + 255) / 256 * 4;
    u64 *sa = me->sa = (u64 *)(me->dk + 2 * KS);
    me->sb = sa + 1024, me->la = (u16 *)(sa + 2048), me->lb = me->la + 65568;
    // 33 = (65536 >> SH) + 1 shard bases
    me->sbase = (u32 *)(me->lb + 65568), me->cw = (u64 *)(me->sbase + 33 * SCAP);
    if (!w) {
        // the samples' huge pages, already faulted in
        size_t bs = (size_t)ndense * W * 8, n = bs + nq * sizeof(Tally) + 8 * nsparse + 66 * PCAP;
        W64 = W, bm = n > HPV ? arena(n, 1) : (u64 *)sdfs;
        tally = (Tally *)((u8 *)bm + bs), off = (u32 *)(tally + nq);
        end = off + nsparse, post = (u16 *)(end + nsparse);
    }
    pthread_barrier_wait(&bar);
    memset(tally + (size_t)nq * w / nw, 0, ((size_t)nq * (w + 1) / nw - (size_t)nq * w / nw) * sizeof(Tally));
    for (int k = 0; k < PH; k++) {
        me->nhit = 0;
        for (u32 c = 0; c < nsp; c++) me->sp[c] = me->sbase + c * SCAP;
        for (u32 c; (c = claim(&sclaim[k])) < (u32)NC;) scan_part(me, k * NC + c);
        pthread_barrier_wait(&bar);
        int p = (k * nw + w) * CK;
        uintptr_t a = ((uintptr_t)doc + pstart[p] + 4095) & -4096, b = ((uintptr_t)doc + pstart[p + CK]) & -4096;
        // the text's page-table entries, dropped in parallel, not at exit
        a < b && madvise((void *)a, b - a, MADV_DONTNEED);
        for (u32 c; (c = claim(&bclaim[k])) < nsp;) build(c, k);
        pthread_barrier_wait(&bar);
        // phrase matches, each worker's ascending, the workers in doc order
        if (!w)
            for (int v = 0; v < NC; v++)
                for (u32 i = ch[0][v]; i < ch[1][v]; i++) {
                    Hit h = wk[co[v]].hit[i];
                    Tally *t = &tally[h.q];
                    t->cnt++, t->sum += h.gid, t->nid < MAXQ ? t->top[t->nid++] = h.gid : 0;

                }
        for (u32 c; (c = claim(&qclaim[k])) * QB < nq;)
            eval_range(me, c * QB, nq < (c + 1) * QB ? nq : (c + 1) * QB, pfirst[k * NC]);
        pthread_barrier_wait(&bar);
    }
    // 4. format the queries this worker parsed
    return finish(me, w), pthread_barrier_wait(&bar), NULL;
}

int main(int argc, char **argv) {
    const char *in = argc > 1 ? argv[1] : "/data", *out = argc > 2 ? argv[2] : "/out";
    cpu_set_t cs;
    // workers: the hardware threads we may use (12 on the box)
    sched_getaffinity(0, sizeof cs, &cs);
    nw = getenv("SD_WORKERS") ? atoi(getenv("SD_WORKERS")) : CPU_COUNT(&cs);
    nw = nw < 1 ? 1 : nw > WMAX ? WMAX : nw, SCAP = (1u << 20) / nw;
    doc = map_file(in, "docs.txt", &dlen);
    qtxt = map_file(in, "queries.txt", &qlen);
    // query shares: byte ranges cut at line starts
    for (int w = 0; w <= nw; w++) qs[w] = nxt(qlen * w / nw, qlen, qtxt, 1);
    // the shortest query line is `BOOL a` + LF
    size_t maxq = qlen / 7 + 1, t = dlen - 1;
    madvise(tab, sizeof tab, MADV_HUGEPAGE);
    for (u32 b = 0; b <= BMASK; b++) _mm256_store_si256((V *)tab[b].val, S32(MISS));
    for (u32 L = 0; L < 129; L++) memset(LM[L], 0xFF, L < 16 ? L : 16);
    // each worker's sample counts, their sums, the new ids
    sdfs = arena(HPV, 1), sdfsize = dlen / 21;
    sampdf = (u32 *)(sdfs + (size_t)nw * NSLOT), newid = sampdf + NSLOT;
    // one arena block: aoff, qterm, qop, aq, aj, an, qk, qkind
    u32 *g = arena((size_t)NSLOT * 4 + 4 + 54 * maxq, 0);
    aoff = g, qterm = g + NSLOT + 1, qop = (u8 *)(qterm + 8 * maxq);
    aq = (u32 *)(qop + 8 * maxq), aj = aq + maxq, an = aj + maxq;
    qk = (u8 *)(an + maxq), qkind = qk + maxq;
    for (; t > 0 && doc[t - 1] != '\n'; t--);
    ndocs = id_at(doc + t) + 1;
    PH = (int)((ndocs + PHDOCS - 1) / PHDOCS), NC = nw * CK, P = NC * PH;
    pstart = malloc((P + 1) * sizeof *pstart), pfirst = malloc((P + 1) * 4);
    pstart[P] = dlen, pfirst[P] = ndocs;
    for (u32 m = 0; m < 256; m++)
        for (u32 l = 0, k = 0; l < 8; l++)
            if (m >> l & 1) *(u16 *)&tbl[m][2 * k] = 0x202 * l + 0x100, PACK[m][k++] = l;
    pthread_barrier_init(&bar, NULL, (unsigned)nw);
    WS = (KS * 24ull + MT * 16ull + 16384 + 65568 * 4 + (33 * (size_t)SCAP + NSLOT * 2) * 4 + qlen / nw * 24 + 4096) >> 6 << 6;
    wmem = arena(nw * WS, 0);
    worker(NULL);
    char path[4096];
    snprintf(path, sizeof path, "%s/results.txt", out);
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd < 0) die(path);
    for (int w = 0; w < nw; w++) write(fd, wk[w].out, wk[w].outlen);
    exit(0);
}
