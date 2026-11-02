// collide.c DATADIR: counts how close track B comes to a wrong answer. bindex.c matches a token by bucket and 32-bit tag, never its text.
// For each width w, the pairs of (distinct document token, query term) sharing a bucket and w tag bits, against a uniform hash.
// Build: gcc -O2 -march=x86-64-v3 -maes -o collide collide.c   The known-bad: add -DBAD (AES removed); it must read far above the prediction.
#include <immintrin.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
typedef unsigned char u8; typedef unsigned u32; typedef unsigned long long u64;
static _Alignas(16) u8 LM[129][16];
static inline __m128i ld(const u8 *p) { return _mm_loadu_si128((const __m128i *)p); }
static u64 key(const u8 *p, u64 L) {
  const u8 *M = LM[0]; if (L > 64) L = 64; L <<= 4;
  __m128i x = _mm_and_si128(_mm_or_si128(ld(p), _mm_set1_epi8(32)), _mm_load_si128((const __m128i *)(M + L)));
  if (L > 256) x = _mm_xor_si128(x, _mm_aesenc_si128(_mm_and_si128(_mm_or_si128(ld(p + 16), _mm_set1_epi8(32)), _mm_load_si128((const __m128i *)(M + L - 256))), _mm_set1_epi32(0x3C6EF372)));
  const __m128i K = _mm_set_epi64x(0x9E3779B97F4A7C15ll, 0x243F6A8885A308D3ll);
#ifdef BAD
  return (u64)_mm_cvtsi128_si64(x);
#else
  return (u64)_mm_cvtsi128_si64(_mm_aesenc_si128(_mm_aesenc_si128(x, K), K));
#endif
}
static u8 *rd(const char *d, const char *n, size_t *l) {
  char f[512]; snprintf(f, 512, "%s/%s", d, n); FILE *h = fopen(f, "rb"); if (!h) { perror(f); exit(1); }
  fseek(h, 0, SEEK_END); *l = ftell(h); rewind(h); u8 *b = calloc(*l + 128, 1); if (fread(b, 1, *l, h) != *l) exit(1); fclose(h); return b;
}
static inline int tb(u8 c) { return (c >= '0' && c <= '9') || ((c | 32) >= 'a' && (c | 32) <= 'z'); }
typedef struct { const u8 *p; u32 l; u32 isq; } E;
#define SZ (1u << 24)
static E *set;
static u32 lc(u8 c) { return c >= 'A' && c <= 'Z' ? c + 32 : c; }
static E *put(const u8 *p, u32 l, u32 *nd) {
  u64 h = 14695981039346656037ull; for (u32 i = 0; i < l; i++) h = (h ^ lc(p[i])) * 1099511628211ull;
  for (u32 i = (u32)(h ^ h >> 29) & (SZ - 1);; i = (i + 1) & (SZ - 1)) {
    E *e = set + i;
    if (!e->p) { e->p = p; e->l = l; ++*nd; return e; }
    if (e->l == l) { u32 j = 0; while (j < l && lc(e->p[j]) == lc(p[j])) j++; if (j == l) return e; }
  }
}
typedef struct { u32 tag, next; } T;
int main(int c, char **v) {
  for (u32 L = 0; L < 129; L++) memset(LM[L], 0xFF, L < 16 ? L : 16);
  set = calloc(SZ, sizeof(E)); size_t dl, ql; u8 *D = rd(v[1], "docs.txt", &dl), *Q = rd(v[1], "queries.txt", &ql);
  u32 nd = 0, nq = 0; u8 *p = Q, *qe = Q + ql;
  while (p < qe) { u32 ph = *p == 'P'; p += ph ? 8 : 5;
    for (;;) { u32 L = 0; while (tb(p[L])) L++; E *e = put(p, L, &nd); if (!e->isq) { e->isq = 1; nq++; } p += L; if (*p != ' ') break; p += ph ? 1 : p[1] == 'O' ? 4 : 5; }
    while (p < qe && *p != '\n') p++; p++; }
  for (p = D; p < D + dl;) { while (*p != '\t' && p < D + dl) p++; p++; while (p < D + dl && *p != '\n') { while (p < D + dl && !tb(*p) && *p != '\n') p++; u32 L = 0; while (tb(p[L])) L++; if (L) put(p, L, &nd); p += L; } p++; }
  static u32 head[1 << 15]; T *t = malloc((nq + 1) * sizeof(T)); u32 n = 1;
  for (u32 i = 0; i < SZ; i++) if (set[i].p && set[i].isq) { u64 k = key(set[i].p, set[i].l); u32 b = k >> 6 & 0x7FFF; t[n].tag = k >> 32; t[n].next = head[b]; head[b] = n++; }
  static const int W[] = {8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32}; enum { NW = 11 };
  double pairs = 0, tpairs = 0, lo[NW] = {0}, hi[NW] = {0}, tl[NW] = {0};
  for (u32 i = 0; i < SZ; i++) if (set[i].p) { u64 k = key(set[i].p, set[i].l); u32 b = k >> 6 & 0x7FFF, g = k >> 32;
    for (u32 j = head[b]; j; j = t[j].next) { u32 x = t[j].tag ^ g;
      if (set[i].isq) { tpairs += .5; if (x == 0) tpairs -= 0; for (int w = 0; w < NW; w++) if (!(W[w] == 32 ? x : x & ((1u << W[w]) - 1))) tl[w] += .5; }
      else { pairs++; for (int w = 0; w < NW; w++) { if (!(W[w] == 32 ? x : x & ((1u << W[w]) - 1))) lo[w]++; if (!(W[w] == 32 ? x : x >> (32 - W[w]))) hi[w]++; } } } }
  printf("%s distinct %u query terms %u nonquery-term pairs in a bucket %.0f (per token %.3f)\n", v[1], nd, nq, pairs, pairs / (nd - nq));
  printf("odds per dataset at 32 tag bits: token-term %.3e  term-term %.3e\n", pairs / 4294967296.0, (tpairs - nq / 2.0) / 4294967296.0);
  for (int w = 0; w < NW; w++) { double e = pairs / (double)(1ull << W[w]); printf("w=%2d expected %12.2f low %10.0f (x%.2f) high %10.0f (x%.2f)\n", W[w], e, lo[w], lo[w] / e, hi[w], hi[w] / e); }
  return 0;
}
