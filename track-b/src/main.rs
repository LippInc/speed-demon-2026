//! Track B: answer the BOOL and PHRASE queries of /data/queries.txt over /data/docs.txt (spec: specs/b-index.md).
//!
//! Queries are read first, so only their terms are looked up. One parallel pass over the memory-mapped corpus
//! tokenizes every document and decides every BOOL query 1,024 documents at a time and every
//! PHRASE pattern (by anchor checks) for it; nothing is indexed.
//! A chunk is 1,024 consecutive documents of one worker. A common BOOL term has a row of 1,024 bits per chunk, and
//! a hit sets the document's bit; any other BOOL term's hit is appended to the chunk's log as the term and the
//! document's place in the chunk. At the chunk's end the log is sorted by term, each query that one of its terms
//! triggers is folded once over its terms' rows (a rare term's row built from its log entries) with branch-free
//! operator masks, the count is a popcount and the id sum comes from bit slices.
//! Main tricks: `ChunkLog` and `fold_chunk` (b-68), `doc_b` and `Table` (b-112), `PhrasePlan` (b-8),
//! `estimate_df` (b-11), `Interner` (b-84).

use std::arch::x86_64::*;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{
    AtomicU32,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::Mutex;

const NIL: u32 = u32::MAX;
const MAX_TERM: usize = 32; // spec 2.3: a query term is 1 to 32 bytes, so a longer token is never a query term
const FIRST_IDS: usize = 10; // spec 4: the ten smallest ids

/// 32 bytes of queries.txt in an AVX2 register (x86-64-v3, the build target), zero past the end of `data`.
struct Chunk(__m256i);

impl Chunk {
    #[inline(always)]
    fn load(data: &[u8], i: usize) -> Chunk {
        if i + 32 <= data.len() {
            return Chunk(unsafe { _mm256_loadu_si256(data.as_ptr().add(i) as *const __m256i) });
        }
        let mut tail = [0u8; 32];
        let rest = &data[i.min(data.len())..];
        tail[..rest.len()].copy_from_slice(rest);
        Chunk(unsafe { _mm256_loadu_si256(tail.as_ptr() as *const __m256i) })
    }

    /// Bit k: byte k is `b`.
    #[inline(always)]
    fn eq_mask(&self, b: u8) -> u32 {
        unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(self.0, _mm256_set1_epi8(b as i8))) as u32 }
    }
}

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
}

/// Maps a file read-only. The corpus is never copied into the heap: its pages stay in the page cache, off our M.
fn map_file(path: &str) -> &'static [u8] {
    let file = File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let len = file.metadata().expect("stat").len() as usize;
    // 1 = PROT_READ, 2 = MAP_PRIVATE (the Linux values), declared here so the standard library stays
    // the only dependency.
    let ptr = unsafe { mmap(std::ptr::null_mut(), len, 1, 2, file.as_raw_fd(), 0) };
    assert!(ptr as isize != -1, "mmap {path} failed");
    // The mapping outlives the descriptor and is never unmapped, so the slice is valid for the whole run.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// Runs `f(k)` for k in 0..n, each on its own scoped thread, and returns the results in k order.
fn parallel<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    std::thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = (0..n).map(|k| s.spawn(move || f(k))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

/// The hash of a term zero-padded to 32 bytes: CRC32-C (SSE4.2, part of x86-64-v3) of its two 16-byte halves xor-ed,
/// a word at a time. The bytes decide every match, so the hash only spreads the slots.
#[inline(always)]
fn hash32(t: &[u8; MAX_TERM]) -> u32 {
    let w = |k: usize| u64::from_le_bytes(t[8 * k..8 * k + 8].try_into().unwrap());
    unsafe { _mm_crc32_u64(_mm_crc32_u64(0, w(0) ^ w(2)), w(1) ^ w(3)) as u32 }
}

/// The query terms, interned into it while queries.txt is parsed (`Interner`); the pass looks its tokens up in
/// `Table` instead (b-112). A 4-byte slot holds the hash's top 2 bits and id + 1; a hash match is then confirmed
/// against the term's bytes, stored inline and zero-padded to 32 B.
struct TermTable {
    // 4 B slots: the hash's top 2 bits | (term id + 1) in the low 30 (TERM: ids stay below 2^30, spec 2.4),
    // 0 = empty. The tag only filters and the bytes decide, so any query file is exact. Half the bytes of 8 B slots
    // for the probe's slot misses (b-36; b-34a measured -5 ms on the box).
    slots: Vec<u32>,
    shift: u32,
    bytes: Vec<TermBytes>, // by term id
}

#[repr(align(32))] // one entry never straddles two cache lines
#[derive(Clone, Copy)]
struct TermBytes([u8; MAX_TERM]);

/// The slot where the probe for hash `h` starts, in a table of 2^(32 - shift) slots.
#[inline]
fn slot(h: u32, shift: u32) -> usize {
    (h.wrapping_mul(0x9e37_79b1) >> shift) as usize
}

/// (b-112) The scan of the lead's probe, module 1: a document's tokens become 64-bit keys first (`doc_b`), then one
/// loop looks the keys up in a table of 64-byte buckets (`Table`), prefetching the bucket 8 keys ahead. The key is the
/// token itself for tokens of 8 bytes or less (bytes OR 0x20, the ASCII lower case of spec 2.2's token bytes, zero
/// past the end) and a CRC of it with bit 63 set and the length in bits 32-39 for longer ones, so equal keys are equal
/// tokens up to a CRC collision between two long tokens (odds about 3e-10 per run, accepted by the design; the bytes
/// are not compared again). The probe's bare read-lookup-record work: 58.6-63.7 ms per worker on box 1 (12 workers,
/// seed 1 full) against about 95 ms for the same work in the chunk engine (tokenize 18 + lookup 53 + hits 24).
const NONE: u32 = u32::MAX;
const FOLD: u64 = 0x2020_2020_2020_2020;
fn mix(k: u64) -> u64 {
    k.wrapping_mul(0x9E37_79B9_7F4A_7C15)
}

/// b-112f: the key of a token of 9 bytes or more is 63 bits of hash, as the design above counts it: the CRC in bits 0-31
/// and the top 31 bits of a multiply chain over the same words in bits 32-62. The CRC alone is 32 bits (its length
/// field adds none), so two same-length tokens collided about once per 4e9 pairs, which a full run reaches (`lailsteel`
/// and `slochaija` have one key); the multiply is not linear over GF(2) like the CRC, so no xor difference of two
/// tokens can cancel in both.
unsafe fn long_key(p: *const u8, n: usize) -> u64 {
    let (mut h, mut g) = (n as u64, n as u64);
    let mut i = 0;
    while i < n {
        let w = (p.add(i) as *const u64).read_unaligned() | FOLD;
        let m = if n - i >= 8 {
            u64::MAX
        } else {
            (1u64 << (8 * (n - i))) - 1
        };
        h = _mm_crc32_u64(h, w & m);
        g = (g ^ (w & m)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        i += 8;
    }
    h | (g >> 33) << 32 | 1 << 63
}
#[inline(always)]
unsafe fn short_key(p: *const u8, n: usize) -> u64 {
    // b-112f: `u64::MAX >> (8 * (8 - n))` shifted by 64 for n = 0 (UB-shaped; the hardware masks the count to 0, so the
    // zeroed entry of an unused id got the key 0x2020..20, not the 0 `term_key` promises); bzhi gives 0 for n = 0.
    let w = (p as *const u64).read_unaligned() | FOLD;
    _bzhi_u64(w, 8 * n.min(8) as u32)
}

/// Four keys and their values in one cache line; v[4] stays NONE, the value of a key the bucket does not hold.
#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct Bucket {
    k: [u64; 4],
    v: [u32; 5],
    _p: [u32; 3],
}
struct Table {
    b: Vec<Bucket>,
    bits: u32,
}
impl Table {
    fn new(bits: u32) -> Table {
        Table {
            b: vec![
                Bucket {
                    k: [0; 4],
                    v: [NONE; 5],
                    _p: [0; 3]
                };
                1 << bits
            ],
            bits,
        }
    }
    #[inline(always)]
    fn at(&self, k: u64) -> usize {
        (mix(k) >> (64 - self.bits)) as usize
    }
    fn insert(&mut self, k: u64, v: u32) -> u32 {
        let (mut i, m) = (self.at(k), self.b.len() - 1);
        loop {
            let b = &mut self.b[i];
            for j in 0..4 {
                if b.k[j] == k {
                    return b.v[j];
                }
                if b.k[j] == 0 {
                    b.k[j] = k;
                    b.v[j] = v;
                    return v;
                }
            }
            i = (i + 1) & m;
        }
    }
    #[inline(never)]
    fn slow(&self, k: u64, mut i: usize) -> u32 {
        let m = self.b.len() - 1;
        loop {
            i = (i + 1) & m;
            let b = &self.b[i];
            for j in 0..4 {
                if b.k[j] == k {
                    return b.v[j];
                }
                if b.k[j] == 0 {
                    return NONE;
                }
            }
        }
    }
    /// The value of key k, NONE if absent: one 32-byte compare of the bucket's four keys; only a full bucket that
    /// does not hold k goes on to the next (`slow`).
    #[inline(always)]
    fn find(&self, k: u64) -> u32 {
        let i = self.at(k);
        let b = unsafe { self.b.get_unchecked(i) };
        let eq = unsafe {
            _mm256_movemask_pd(_mm256_castsi256_pd(_mm256_cmpeq_epi64(
                _mm256_load_si256(b.k.as_ptr() as *const __m256i),
                _mm256_set1_epi64x(k as i64),
            ))) as u32
        };
        let id = unsafe { *b.v.get_unchecked((eq | 16).trailing_zeros() as usize) };
        if (eq == 0) & (b.k[3] != 0) {
            return self.slow(k, i);
        }
        id
    }
    #[inline(always)]
    fn prefetch(&self, k: u64) {
        unsafe { _mm_prefetch(self.b.as_ptr().add(self.at(k)) as *const i8, _MM_HINT_T0) }
    }
    /// Replaces every value v by f(v).
    fn remap(&mut self, f: impl Fn(u32) -> u32) {
        for b in &mut self.b {
            (0..4)
                .filter(|&j| b.k[j] != 0)
                .for_each(|j| b.v[j] = f(b.v[j]));
        }
    }
}

/// For 64 bytes at p: bit k of the first mask says byte k is a token byte (spec 2.2: a-z, A-Z, 0-9; unsigned
/// compares, so no byte from 0x80 matches), bit k of the second that it is the LF.
#[inline(always)]
unsafe fn masks(p: *const u8) -> (u64, u64) {
    let f = |v: __m256i| {
        let a = _mm256_sub_epi8(
            _mm256_or_si256(v, _mm256_set1_epi8(0x20)),
            _mm256_set1_epi8(b'a' as i8),
        );
        let l = _mm256_cmpeq_epi8(_mm256_min_epu8(a, _mm256_set1_epi8(25)), a);
        let d = _mm256_sub_epi8(v, _mm256_set1_epi8(b'0' as i8));
        let g = _mm256_cmpeq_epi8(_mm256_min_epu8(d, _mm256_set1_epi8(9)), d);
        let n = _mm256_cmpeq_epi8(v, _mm256_set1_epi8(b'\n' as i8));
        (
            _mm256_movemask_epi8(_mm256_or_si256(l, g)) as u32 as u64,
            _mm256_movemask_epi8(n) as u32 as u64,
        )
    };
    let (t0, n0) = f(_mm256_loadu_si256(p as *const __m256i));
    let (t1, n1) = f(_mm256_loadu_si256(p.add(32) as *const __m256i));
    (t0 | t1 << 32, n0 | n1 << 32)
}

/// Writes the keys of the tokens of the document text starting at s to keys[0..], in order, and returns the index
/// of its LF and the token count. It reads 64-byte blocks and one block ahead, up to 63 bytes past the LF (`Corpus`
/// keeps that inside the data). A token's length comes from the token mask of its block and the next; a long
/// token's start goes to `long`, and its CRC key is filled in after the walk. `keys` and `long` hold `cap` entries
/// (b-112f); a 64-byte block holds at most 32 tokens, so the walk gives up with None, before a block that might not
/// fit, and `Corpus::doc` runs it again on larger buffers.
#[inline(always)]
unsafe fn doc_b(
    d: &[u8],
    s: usize,
    keys: *mut u64,
    long: *mut u64,
    cap: usize,
) -> Option<(usize, usize)> {
    let p0 = d.as_ptr();
    let (mut n_tok, mut nlong, mut base, mut carry) = (0usize, 0usize, s, 0u64);
    let (mut m, mut nl) = masks(p0.add(base));
    loop {
        if n_tok + 32 > cap {
            return None;
        }
        let last = nl != 0;
        let (m2, nl2) = if last {
            (0, 0)
        } else {
            masks(p0.add(base + 64))
        };
        let lim = if last {
            (1u64 << nl.trailing_zeros()) - 1
        } else {
            u64::MAX
        };
        let mut a = m & !((m << 1) | carry) & lim;
        let m2s = m2 << 1;
        while a != 0 {
            let o = a.trailing_zeros();
            let n = (!((m >> o) | (m2s << (63 - o)))).trailing_zeros();
            let at = base + o as usize;
            *keys.add(n_tok) = _bzhi_u64(
                (p0.add(at) as *const u64).read_unaligned() | FOLD,
                8 * n.min(8),
            );
            *long.add(nlong) = (at as u64) << 32 | (n_tok as u64) << 8 | n as u64;
            nlong += (n > 8) as usize;
            n_tok += 1;
            a &= a - 1;
        }
        if last {
            break;
        }
        carry = m >> 63;
        m = m2;
        nl = nl2;
        base += 64;
    }
    for j in 0..nlong {
        let x = *long.add(j);
        *keys.add(((x >> 8) & 0xff_ffff) as usize) =
            long_key(p0.add((x >> 32) as usize), (x & 0xff) as usize);
    }
    Some((base + nl.trailing_zeros() as usize, n_tok))
}

/// The key of a query term (spec 2.3: 1 to 32 bytes of a-z and 0-9), zero-padded to 32 bytes; 0 for the zeroed
/// entry of an id no term took (`parse`), which is then left out of the table.
fn term_key(t: &[u8; MAX_TERM]) -> u64 {
    let n = t.iter().position(|&b| b == 0).unwrap_or(MAX_TERM);
    unsafe {
        if n <= 8 {
            short_key(t.as_ptr(), n)
        } else {
            long_key(t.as_ptr(), n)
        }
    }
}

/// A document's tokens, bounded by spec 2.4: 1 to 999 word units, a unit being a word, a legal phrase of a few words
/// or a citation of at most 7 tokens (`Fed. R. Civ. P. 12(b)(6)`), with at most one more token (`s`) from its
/// separator; 16 per unit leaves room for the injected cases. The buffers start at this size and `grow_doc` doubles
/// them for a longer document (b-112f).
const MAX_DOC_TOKENS: usize = 999 * 16;

/// docs.txt, and a zero-padded copy of its last lines: the lines whose LF is within 64 bytes of the end of the file
/// (spec 2.1: it ends right after the last LF), which `doc_b` reads from the copy so its block reads stay inside.
struct Corpus<'a> {
    docs: &'a [u8],
    tail: usize, // the start of the first line read from `pad`
    pad: Vec<u8>,
}

impl<'a> Corpus<'a> {
    fn new(docs: &'a [u8]) -> Self {
        let d = &docs[..docs.len().saturating_sub(64)];
        let tail = d.iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
        let mut pad = docs[tail..].to_vec();
        pad.resize(pad.len() + 128, 0);
        Corpus { docs, tail, pad }
    }

    /// Fills `keys` with the keys of the document whose line starts at i; returns its id, the start of the next line
    /// and its token count. `keys` (8 entries more than the doc's keys, for the prefetch ahead) and `long` start at
    /// MAX_DOC_TOKENS and, b-112f, double for a document with more tokens than that: they stay the same length.
    #[inline(always)]
    fn doc(&self, i: usize, keys: &mut Vec<u64>, long: &mut Vec<u64>) -> (u32, usize, usize) {
        let (buf, off) = if i < self.tail {
            (self.docs, 0)
        } else {
            (&self.pad[..], self.tail)
        };
        let (id, text) = parse_id(buf, i - off);
        loop {
            let (kp, lp, cap) = (keys.as_mut_ptr(), long.as_mut_ptr(), keys.len() - 8);
            if let Some((lf, n)) = unsafe { doc_b(buf, text, kp, lp, cap) } {
                return (id, lf + 1 + off, n);
            }
            grow_doc(keys, long);
        }
    }
}

/// b-112f: doubles the buffers of a document with more tokens than they hold (`doc_b` gave up). Out of line: only a
/// document of over MAX_DOC_TOKENS tokens, which spec 2.4 does not allow, ever gets here.
#[cold]
#[inline(never)]
fn grow_doc(keys: &mut Vec<u64>, long: &mut Vec<u64>) {
    keys.resize(keys.len() * 2, 0);
    long.resize(keys.len(), 0);
}

/// A table for `n` keys at a load of 1/4 to 1/2 (at least two buckets).
fn table_bits(n: usize) -> u32 {
    n.max(4).next_power_of_two().trailing_zeros() - 1
}

/// Ids are drawn from the shared counter this many at a time, so the workers rarely touch its cache line.
const ID_BLOCK: u32 = 64;

/// A `TermTable` while every worker interns its share of queries.txt into it at once (b-84; the serial parse was
/// 12.5 ms of the box's 24.5 ms serial prologue, b-81). A new term's bytes go to the entry at an id the worker drew,
/// then a compare-and-swap on the empty slot publishes it (Release): a worker that loads the slot (Acquire) sees
/// the bytes. Slots never move, so `main` sizes the table first and parses again at twice the slots when the drawn
/// ids pass load 1/3 (b-36's load); any query file is exact. Box: parse 12.5 → 3.4 ms, T 0.224 → 0.216 s (b-84).
struct Interner<'a> {
    slots: &'a [AtomicU32],
    shift: u32,
    bytes: *mut TermBytes, // `cap` entries; entry k is written by the one worker that drew k, before a slot names k
    cap: u32,
    next: AtomicU32, // the next undrawn id; above `cap`, the table was too small
}

// The workers share it: an entry is only written by the worker that drew its id, before a slot publishes it.
unsafe impl Sync for Interner<'_> {}

impl<'a> Interner<'a> {
    fn new(table: &'a mut TermTable) -> Self {
        // AtomicU32 has u32's size and alignment; the &mut makes the slots ours for 'a.
        let slots = unsafe { &*(table.slots.as_mut_slice() as *mut [u32] as *const [AtomicU32]) };
        let (shift, cap) = (table.shift, (table.slots.len() / 3) as u32);
        Interner {
            slots,
            shift,
            bytes: table.bytes.as_mut_ptr(),
            cap,
            next: AtomicU32::new(0),
        }
    }

    /// The id of `term` (spec 2.3: 1 to 32 bytes), added if new (any id once the table is too small). `ids` are this
    /// worker's drawn, unused ids; one whose slot another worker took first stays for the next new term.
    fn intern(&self, term: &[u8], ids: &mut std::ops::Range<u32>) -> u32 {
        let mut padded = [0u8; MAX_TERM];
        padded[..term.len()].copy_from_slice(term);
        let h = hash32(&padded);
        let mut i = slot(h, self.shift);
        loop {
            let s = self.slots[i].load(Acquire);
            if s == 0 {
                if ids.start == ids.end {
                    let k = self.next.fetch_add(ID_BLOCK, Relaxed);
                    *ids = k..k + ID_BLOCK;
                }
                let id = ids.start;
                if id >= self.cap {
                    return 0;
                }
                unsafe { self.bytes.add(id as usize).write(TermBytes(padded)) };
                let (p, e) = (h & !TERM | id + 1, Release);
                if self.slots[i].compare_exchange(0, p, e, Relaxed).is_ok() {
                    ids.start += 1;
                    return id;
                }
                continue; // another worker took the slot: read it again
            }
            let b = unsafe { &*self.bytes.add((s & TERM) as usize - 1) }.0;
            if (s ^ h) & !TERM == 0 && b == padded {
                return (s & TERM) - 1;
            }
            i = (i + 1) & (self.slots.len() - 1);
        }
    }
}

/// Lists of items by term id (CSR): list t is `items[start[t]..start[t + 1]]`, in the order `items` gives them.
fn by_term<T: Copy + Default>(n_terms: usize, items: &[(u32, T)]) -> (Vec<u32>, Vec<T>) {
    let mut start = vec![0u32; n_terms + 2];
    items.iter().for_each(|&(t, _)| start[t as usize + 2] += 1);
    (2..start.len()).for_each(|t| start[t] += start[t - 1]);
    // start[t + 1] is now where list t begins; filling list t moves it to where list t ends.
    let mut lists = vec![T::default(); items.len()];
    for &(t, x) in items {
        lists[start[t as usize + 1] as usize] = x;
        start[t as usize + 1] += 1;
    }
    start.pop();
    (start, lists)
}

/// PHRASE matching by anchor checks (b-8). Each PHRASE line is one pattern and gets one anchor: its term with the
/// lowest estimated df (the df sample only picks the anchor; a term in no doc has df 0, so a phrase with an absent
/// term is never even checked). The pass records only a doc's anchor occurrences, each with its token index; at the
/// doc's end each one is a candidate for every pattern anchored there at offset j, confirmed by comparing the
/// pattern's term keys with the doc's token keys around it (b-112; b-56 read the doc's bytes again).
/// It replaces an Aho-Corasick automaton whose edge probes, fail walks and output checks were the pass's largest
/// part (about 0.9-1.1 s of 4.0-4.3 s at 1 worker on seed 1).
struct PhrasePlan {
    keys: Vec<u64>,                     // the patterns' terms' keys, pattern after pattern
    anchor_bits: Vec<u64>, // by term id: the term anchors at least one pattern (tested for every hit)
    anchor_start: Vec<u32>, // by term id, into anchors (CSR)
    anchors: Vec<(u32, u32, u32, u32)>, // (pattern, offset of the anchor in it, its terms' range in `terms`)
}

impl PhrasePlan {
    /// Pattern p's terms are `terms[ends[p - 1]..ends[p]]` (from 0 for p = 0), and `keys` their keys.
    fn new(terms: &[u32], keys: Vec<u64>, ends: &[u32], n_terms: usize, df: &[u32]) -> Self {
        let mut anchor_bits = vec![0u64; n_terms.div_ceil(64)];
        let (mut anchors, mut lo) = (Vec::new(), 0);
        for (pi, &hi) in ends.iter().enumerate() {
            let p = &terms[lo as usize..hi as usize];
            // b-99a: an anchor with a term before it is filtered by that term (b-98), so it may be 8x commoner (T -2 ms).
            let w = if p.len() > 1 { 8 } else { 1 };
            let j = (0..p.len())
                .min_by_key(|&j| df[p[j] as usize] as u64 * if j == 0 { w } else { 1 })
                .unwrap();
            anchor_bits[p[j] as usize / 64] |= 1 << (p[j] % 64);
            anchors.push((p[j], (pi as u32, j as u32, lo, hi)));
            lo = hi;
        }
        let (anchor_start, anchors) = by_term(n_terms, &anchors);
        PhrasePlan {
            keys,
            anchor_bits,
            anchor_start,
            anchors,
        }
    }

    /// Calls `f(p)` for each pattern with an anchor occurrence in `hits` (term, token index) at its anchor offset
    /// and its other terms around it: the doc's keys from the anchor's index - j on equal the pattern's keys.
    /// Equal keys are equal tokens (`doc_b`), so this is the token sequence's own test; no phrase spans two docs.
    #[inline]
    fn check(&self, keys: &[u64], hits: &[(u32, u32)], mut f: impl FnMut(u32)) {
        for &(t, i) in hits {
            let (t, i) = (t as usize, i as usize);
            for &(p, j, lo, hi) in
                &self.anchors[self.anchor_start[t] as usize..self.anchor_start[t + 1] as usize]
            {
                let (j, pattern) = (j as usize, &self.keys[lo as usize..hi as usize]);
                let Some(at) = i.checked_sub(j) else { continue };
                // b-98: the doc's token just before the anchor must be the pattern's term before it: one compare
                // skips the pattern (box T 0.216 -> 0.211 s).
                if j > 0 && keys[i - 1] != pattern[j - 1] {
                    continue;
                }
                if keys.get(at..at + pattern.len()) == Some(pattern) {
                    f(p);
                }
            }
        }
    }
}

/// The start of the line after the first LF at or after `from`: 0 for `from` 0, the end if no LF follows (spec 2.1:
/// every line ends with LF).
/// A `from` that is itself a line start returns the next line; both ends of a cut use this, so the ranges still tile.
fn line_start(data: &[u8], from: usize) -> usize {
    if from == 0 {
        return 0;
    }
    data[from..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(data.len(), |p| from + p + 1)
}

/// Reads the decimal doc id that starts the line at `i` (spec 2.1); returns it and the index just past its TAB.
#[inline]
fn parse_id(data: &[u8], mut i: usize) -> (u32, usize) {
    let mut id = 0u32;
    while data[i] != b'\t' {
        id = id * 10 + (data[i] - b'0') as u32;
        i += 1;
    }
    (id, i + 1)
}

/// Document frequency of the query terms, estimated on about 1/SAMPLE_SHARE of the corpus (about 1 %) read as whole
/// lines from SAMPLE_WINDOWS evenly spaced windows. It only chooses an evaluation order (each query's trigger terms,
/// each phrase's anchor, the term ids' order), so it cannot change an answer: a poor estimate can only slow the pass
/// down. It is not a second pass.
const SAMPLE_WINDOWS: usize = 64;
const SAMPLE_SHARE: usize = 128;

/// Returns the estimated df per term (`table` values are term ids) and the number of docs sampled.
fn estimate_df(corpus: &Corpus, table: &Table, n_terms: usize, workers: usize) -> (Vec<u32>, u32) {
    let docs = corpus.docs;
    let window = docs.len() / SAMPLE_SHARE / SAMPLE_WINDOWS;
    // The windows are spread over the workers (window k on worker k % n) and their counts summed: the sample was
    // 15 ms of serial prologue on the box. (b-11)
    let n = workers.clamp(1, SAMPLE_WINDOWS);
    let parts = parallel(n, |w| {
        let (mut df, mut seen, mut stamp) = (vec![0u32; n_terms], vec![0u32; n_terms], 0u32);
        let mut keys = vec![0u64; MAX_DOC_TOKENS + 8];
        let mut long = vec![0u64; MAX_DOC_TOKENS + 8];
        for k in (w..SAMPLE_WINDOWS).step_by(n) {
            let mut i = line_start(docs, docs.len() * k / SAMPLE_WINDOWS);
            let stop = (i + window).min(docs.len());
            while i < stop {
                stamp += 1;
                let (_, next, n_tok) = corpus.doc(i, &mut keys, &mut long);
                for &key in &keys[..n_tok] {
                    let t = table.find(key);
                    if t != NONE && seen[t as usize] != stamp {
                        seen[t as usize] = stamp;
                        df[t as usize] += 1;
                    }
                }
                i = next;
            }
        }
        (df, stamp)
    });
    let mut df = vec![0u32; n_terms];
    for (part, _) in &parts {
        df.iter_mut().zip(part).for_each(|(d, p)| *d += p);
    }
    (df, parts.iter().map(|p| p.1).sum())
}

/// BOOL queries evaluated inside the pass, CHUNK docs at a time, so no posting list is ever stored (b-2 kept 47.6 MB
/// of blocks and 7.3 MB of heads for 29.2 MB of varints at 12 workers). Under the strict left fold (spec 3) a doc is
/// in R exactly when the same fold over the booleans "the doc contains t" is true, so the fold over words whose bit
/// j says "doc base + j contains t" decides 64 docs at once, a chunk's 16 words all of R in the chunk
/// (`fold_chunk`). A query is only folded for the chunks where one of its trigger terms occurs, a cover such that
/// every doc in R contains one of them (`cover`); every query has one, so the path is exact for every shape, NOT
/// forms and absent terms included.
const MAX_BOOL_TERMS: usize = 6; // spec 2.3: a BOOL query has 1 to 6 terms
/// A query's code: its first term, then op << OP_SHIFT | term per operator, NIL-padded.
type Code = [u32; MAX_BOOL_TERMS];
const OP_SHIFT: u32 = 30; // term ids stay below 2^30 (spec 2.4: 600,000 words)
const TERM: u32 = (1 << OP_SHIFT) - 1;
const OP_AND: u32 = 0;
const OP_OR: u32 = 1;
const OP_NOT: u32 = 2;

/// Docs per chunk: a doc's place in its chunk takes the low LOCAL_BITS of a log entry, and a chunk's row is
/// CHUNK / 64 words. (b-68)
const LOCAL_BITS: u32 = 11;
const CHUNK: u32 = 1 << LOCAL_BITS;
const ROW: usize = CHUNK as usize / 64;

/// One query listed under one of its trigger terms, with its code inline so a chunk's evaluations read the
/// trigger list front to back instead of jumping to each query.
#[derive(Clone, Copy, Default)]
struct Trigger {
    query: u32, // accumulator slot
    code: Code,
}

/// The BOOL terms are the ids 0..n_bool, the dense ones (sampled in at least 1/64 of the docs) first: a dense
/// term's hits set bits in a row of CHUNK bits, every other term's are appended to the chunk's log. Sparse term t
/// has the log key t - n_dense + 1; key 0 marks a dense term's hit, which the log does not keep. (b-68)
struct BoolPlan {
    n_dense: usize,
    n_bool: usize,
    every_chunk: Vec<Trigger>, // queries whose cover holds a dense term: folded in every chunk
    trigger_start: Vec<u32>,   // per sparse key, into triggers (CSR)
    triggers: Vec<Trigger>,
    // Per query its accumulator slot. Slots follow the trigger lists, so the queries one term triggers have
    // neighbouring accumulators and first-id bounds (b-24b, which numbered them by query, read +3 ms on the box).
    slot: Vec<u32>,
    ids: FirstIds, // by slot
}

/// Trigger terms of a query, written to `cover` (reused, so a query allocates nothing): every doc in R contains one
/// of them. {first} holds for R = S(first); OR adds its operand; AND narrows R to S(t), so it may replace the cover by
/// {t} when t is estimated rarer; NOT keeps it.
fn cover(q: &Code, df: &[u32], cover: &mut Vec<u32>) {
    cover.clear();
    cover.push(q[0]);
    let df_of = |c: u32| df[c as usize] as u64;
    for &w in q[1..].iter().take_while(|&&w| w != NIL) {
        let t = w & TERM;
        match w >> OP_SHIFT {
            OP_OR => cover.push(t),
            OP_AND if df_of(t) < cover.iter().map(|&c| df_of(c)).sum() => *cover = vec![t],
            _ => {}
        }
    }
    cover.sort_unstable();
    cover.dedup();
}

impl BoolPlan {
    fn new(queries: &[Code], n_dense: usize, n_bool: usize, df: &[u32]) -> Self {
        let (mut every_chunk, mut triggers, mut terms) = (Vec::new(), Vec::new(), Vec::new());
        for (qi, q) in queries.iter().enumerate() {
            cover(q, df, &mut terms);
            let trig = Trigger {
                query: qi as u32,
                code: *q,
            };
            if terms[0] < n_dense as u32 {
                every_chunk.push(trig); // sorted: a dense term, if any, comes first
            } else {
                triggers.extend(terms.iter().map(|&t| (t - n_dense as u32 + 1, trig)));
            }
        }
        let (trigger_start, mut triggers) = by_term(n_bool - n_dense + 1, &triggers);
        // Every query is in one of the two lists, so every query gets a slot.
        let (mut slot, mut n_slots) = (vec![NIL; queries.len()], 0);
        for trig in every_chunk.iter_mut().chain(&mut triggers) {
            let q = trig.query as usize;
            if slot[q] == NIL {
                (slot[q], n_slots) = (n_slots, n_slots + 1);
            }
            trig.query = slot[q];
        }
        let ids = FirstIds::new(queries.len());
        BoolPlan {
            n_dense,
            n_bool,
            every_chunk,
            trigger_start,
            triggers,
            slot,
            ids,
        }
    }
}

/// spec 3: strict left fold, no precedence; `a OR b AND c` is `(a OR b) AND c` (b-index.md:136-141), over a chunk
/// at once: bit j of `row(t)` says doc base + j contains t. AND, OR and NOT only combine bits that some term set, so
/// docs missing from the range stay 0. Each step is r & (b ^ x | y) | b & o, with the operator's masks from
/// OP_MASKS, so the operator takes no branch (b-32b) and a step is four AVX2 operations per 256 bits.
#[inline]
fn fold_chunk(code: &Code, row: impl Fn(u32) -> [u64; ROW]) -> [u64; ROW] {
    // (x, y, o) per operator in r & (b ^ x | y) | b & o, with 1 = all ones: AND (0, 0, 0) gives r & b,
    // OR (0, 1, 1) gives r | b, NOT (1, 0, 0) gives r & !b.
    const OP_MASKS: [(u64, u64, u64); 3] = [(0, 0, 0), (0, !0, !0), (!0, 0, 0)];
    let mut r = row(code[0]);
    for &w in code[1..].iter().take_while(|&&w| w != NIL) {
        let (b, (x, y, o)) = (row(w & TERM), OP_MASKS[(w >> OP_SHIFT) as usize]);
        r.iter_mut()
            .zip(b)
            .for_each(|(r, b)| *r = *r & (b ^ x | y) | b & o);
    }
    r
}

/// Bit k of a bit position j, for every j at once: the sum of the set positions of w is
/// sum over k of popcount(w & BIT_SLICES[k]) << k, so a word's id sum needs no walk over its matches.
const BIT_SLICES: [u64; 6] = [
    0xaaaa_aaaa_aaaa_aaaa,
    0xcccc_cccc_cccc_cccc,
    0xf0f0_f0f0_f0f0_f0f0,
    0xff00_ff00_ff00_ff00,
    0xffff_0000_ffff_0000,
    0xffff_ffff_0000_0000,
];

/// Adds the matches r (bit j: doc base + j) to slot q's count and sum, and offers them as first ids while they can
/// still be one: the bound is NIL until ten are kept.
#[inline]
fn record(acc: &mut BoolAcc, ids: &FirstIds, q: usize, base: u32, r: u64) {
    let n = r.count_ones();
    let offsets: u32 = (0..6).map(|k| (r & BIT_SLICES[k]).count_ones() << k).sum();
    acc.count += n;
    acc.sum += base as u64 * n as u64 + offsets as u64;
    let mut bits = r;
    while bits != 0 && base + bits.trailing_zeros() < ids.bound[q].load(Relaxed) {
        ids.offer(q, base + bits.trailing_zeros());
        bits &= bits - 1;
    }
}

/// One worker's current chunk of docs base..base + CHUNK, the sorted chunk log: a dense term's hit sets its bit in
/// `rows`, any other BOOL term's hit is appended to `log` as key << LOCAL_BITS | (id - base), a store with no read.
/// It replaces the 64-doc window (b-4, b-35a): a chunk folds each triggered query once, about a fifth of the
/// window's folds. (b-68) box T 0.250 → 0.238 s.
struct ChunkLog {
    base: u32,
    rows: Vec<u64>, // ROW words per dense term, and a dummy row that every sparse hit ORs into instead
    log: Vec<u32>, // log[..n] is this chunk's; the entry at n is written by every hit and kept by a sparse one
    n: usize,
    tmp: Vec<u32>,   // the radix sort's other buffer
    count: Vec<u32>, // the radix sort's digit counts
    half: u32,       // digit bits: two passes cover every key
    // Per key, its entries' (start << 32 | end) in the sorted log; 0 (empty) for a key not in the chunk.
    span: Vec<u64>,
}

impl ChunkLog {
    fn new(plan: &BoolPlan) -> Self {
        let keys = plan.n_bool - plan.n_dense + 1;
        let half = (usize::BITS - keys.leading_zeros()).div_ceil(2);
        ChunkLog {
            base: 0,
            rows: vec![0; (plan.n_dense + 1) * ROW],
            log: vec![0; CHUNK as usize],
            n: 0,
            tmp: Vec::new(),
            count: vec![0; (1 << half) + 1],
            half,
            span: vec![0; keys],
        }
    }

    /// Records a token of doc base + local, by its meta word m (`TOKEN_META`): no branch on dense, sparse, other
    /// or no term. A dense term sets its bit; anything else sets the dummy row's (its term part is n_dense or more),
    /// and only a sparse term's log entry is kept. (b-112, the probe's payload: the store always, the count by flag.)
    #[inline(always)]
    fn hit(&mut self, n_dense: usize, m: u32, local: u32) {
        let t = (m & TERM) as usize;
        self.rows[t.min(n_dense) * ROW + local as usize / 64] |= 1 << (local % 64);
        if self.n == self.log.len() {
            self.grow();
        }
        self.log[self.n] = ((t + 1).wrapping_sub(n_dense) as u32) << LOCAL_BITS | local;
        self.n += (!(m >> SPARSE_BIT) & 1) as usize;
    }

    /// Doubles the log, only while the first chunks size it. Out of line, so the hit stays small enough to be inlined
    /// into the token loop: inline, the resize made the hit a call per token. (b-68) The pass's instructions fell by
    /// 0.85 G (cachegrind, 1 worker, seed 1).
    #[cold]
    #[inline(never)]
    fn grow(&mut self) {
        self.log.resize(self.log.len() * 2, 0);
    }

    /// Sorts the log by key (a 2-pass LSD radix sort, stable, so each key's docs stay ascending), drops repeats of
    /// a (key, doc) pair, and fills `span`. Returns the distinct entries' count.
    fn sort(&mut self) -> usize {
        let (n, half) = (self.n, self.half);
        self.tmp.resize(n, 0);
        let count = &mut self.count;
        let mut pass = |from: &[u32], to: &mut [u32], shift: u32| {
            let digit = |e: u32| (e >> shift) as usize & ((1 << half) - 1);
            count.fill(0);
            from.iter().for_each(|&e| count[digit(e) + 1] += 1);
            (1..count.len()).for_each(|d| count[d] += count[d - 1]);
            for &e in from {
                to[count[digit(e)] as usize] = e;
                count[digit(e)] += 1;
            }
        };
        pass(&self.log[..n], &mut self.tmp, LOCAL_BITS);
        pass(&self.tmp, &mut self.log[..n], LOCAL_BITS + half);
        let log = &mut self.log[..n];
        // Each entry is written at m and kept when it differs from the one before; its key's span is rewritten
        // at every entry, with no branch. prev starts as the complement of the first entry, so the first entry
        // always counts as new: a sentinel such as NIL can equal a real entry (the top key at local doc 1023).
        let (mut m, mut prev, mut start) = (0, !log.first().copied().unwrap_or(0), 0);
        for i in 0..n {
            let e = log[i];
            start = if e >> LOCAL_BITS != prev >> LOCAL_BITS {
                m
            } else {
                start
            };
            log[m] = e;
            m += (e != prev) as usize;
            self.span[(e >> LOCAL_BITS) as usize] = (start as u64) << 32 | m as u64;
            prev = e;
        }
        m
    }

    /// Folds every query of the chunk once: the every-chunk queries, then those a key in the log triggers. Then
    /// clears the chunk.
    fn end(&mut self, plan: &BoolPlan, acc: &mut [BoolAcc]) {
        let m = self.sort();
        let (log, span, rows) = (&self.log[..m], &self.span, &self.rows);
        let n_dense = plan.n_dense;
        // A dense term's row, or a sparse term's built from its entries.
        let row = |t: u32| {
            let t = t as usize;
            if t < n_dense {
                return rows[t * ROW..(t + 1) * ROW].try_into().unwrap();
            }
            let mut r = [0u64; ROW];
            let s = span[t - n_dense + 1];
            for &e in &log[(s >> 32) as usize..s as u32 as usize] {
                let local = e & (CHUNK - 1);
                r[local as usize / 64] |= 1 << (local % 64);
            }
            r
        };
        // A query listed under several keys of the chunk is folded at the first one; `done` is the chunk's stamp.
        let stamp = self.base / CHUNK + 1;
        let mut fold = |trig: &Trigger| {
            let q = trig.query as usize;
            if acc[q].done == stamp {
                return;
            }
            acc[q].done = stamp;
            let r = fold_chunk(&trig.code, row);
            // Only the words with a match are recorded, found through a mask of them, not a branch per word.
            // (b-68) The evaluation's mispredicts fell to 21.6 M (cachegrind, 1 worker, seed 1).
            let mut words = r
                .iter()
                .enumerate()
                .fold(0u32, |m, (j, &w)| m | ((w != 0) as u32) << j);
            while words != 0 {
                let j = words.trailing_zeros();
                record(&mut acc[q], &plan.ids, q, self.base + 64 * j, r[j as usize]);
                words &= words - 1;
            }
        };
        plan.every_chunk.iter().for_each(&mut fold);
        let mut i = 0;
        while i < m {
            let key = (log[i] >> LOCAL_BITS) as usize;
            let (from, to) = (plan.trigger_start[key], plan.trigger_start[key + 1]);
            plan.triggers[from as usize..to as usize]
                .iter()
                .for_each(&mut fold);
            i = span[key] as u32 as usize;
        }
        for k in 0..m {
            self.span[(self.log[k] >> LOCAL_BITS) as usize] = 0;
        }
        self.rows.fill(0);
        self.n = 0;
    }
}

/// Per worker and BOOL query slot: the evaluation stamp and the running count and sum.
#[derive(Clone, Copy, Default)]
struct BoolAcc {
    done: u32, // last chunk index + 1 this query was folded for
    count: u32,
    sum: u64,
}

/// The FIRST_IDS smallest matching ids of every BOOL query, shared by all workers (b-3). Keeping them per worker held
/// 1.42 M (query, doc) entries (12.6 MB at 12 workers): every worker kept ten for each common query. `bound` is the
/// largest id kept once ten are kept (NIL before), so a later, larger id is rejected by one relaxed load and only a
/// candidate takes the lock; a stale bound only sends a few more ids to the lock, where the truncation drops them.
struct FirstIds {
    bound: Vec<AtomicU32>,
    kept: Vec<Mutex<Vec<u32>>>,
}

impl FirstIds {
    fn new(n: usize) -> Self {
        let bound = (0..n).map(|_| AtomicU32::new(NIL)).collect();
        let kept = (0..n).map(|_| Mutex::default()).collect();
        FirstIds { bound, kept }
    }

    #[inline]
    fn offer(&self, q: usize, id: u32) {
        if id < self.bound[q].load(Relaxed) {
            let mut kept = self.kept[q].lock().unwrap();
            let at = kept.partition_point(|&k| k < id);
            kept.insert(at, id);
            kept.truncate(FIRST_IDS);
            if kept.len() == FIRST_IDS {
                self.bound[q].store(kept[FIRST_IDS - 1], Relaxed);
            }
        }
    }
}

/// One worker's pass over its doc range: per-query BOOL count, sum and stamp, and the (pattern, doc) phrase
/// matches in doc order.
type Shard = (Vec<BoolAcc>, Vec<(u32, u32)>);

/// A token's meta word, the value `Table` holds for its key (b-112): the term id in the low 30 bits (TERM),
/// SPARSE_BIT clear for a sparse BOOL term, ANCHOR_BIT clear for a phrase anchor. NONE, an unknown token, reads as
/// term TERM with both bits set: no BOOL term and no anchor.
const SPARSE_BIT: u32 = 30;
const ANCHOR_BIT: u32 = 31;

fn scan(c: &Corpus, from: usize, to: usize, t: &Table, ph: &PhrasePlan, plan: &BoolPlan) -> Shard {
    let (mut acc, mut matches) = (vec![BoolAcc::default(); plan.slot.len()], Vec::new());
    // Ending a chunk with no hits adds nothing, so the first chunk needs no special case.
    let mut chunk = ChunkLog::new(plan);
    // The doc's keys (8 more, so the prefetch 8 keys ahead stays inside), its anchor occurrences as (term, token
    // index); per pattern the last doc id + 1 it was reported for.
    let (mut keys, mut long) = (
        vec![0u64; MAX_DOC_TOKENS + 8],
        vec![0u64; MAX_DOC_TOKENS + 8],
    );
    let mut hits = vec![(0u32, 0u32); MAX_DOC_TOKENS];
    let mut pattern_last = vec![0u32; ph.anchors.len()];
    let mut i = from;
    while i < to {
        let (id, next, n) = c.doc(i, &mut keys, &mut long);
        if hits.len() < n {
            hits.resize(keys.len(), (0, 0)); // b-112f: a document that made `doc` grow its buffers
        }
        // Ids are dense and ascending through docs.txt (specs/b-index.md), so a chunk, once left, never returns.
        if id / CHUNK * CHUNK != chunk.base {
            chunk.end(plan, &mut acc);
            chunk.base = id / CHUNK * CHUNK;
        }
        let (local, mut n_hits) = (id - chunk.base, 0);
        for k in 0..n {
            // k + 8 < n + 8 <= keys.len() and n_hits <= k < n <= hits.len(): inside both buffers.
            let m = unsafe {
                t.prefetch(*keys.get_unchecked(k + 8));
                t.find(*keys.get_unchecked(k))
            };
            chunk.hit(plan.n_dense, m, local);
            unsafe { *hits.get_unchecked_mut(n_hits) = (m & TERM, k as u32) };
            n_hits += (!(m >> ANCHOR_BIT) & 1) as usize;
        }
        ph.check(&keys[..n], &hits[..n_hits], |p| {
            if pattern_last[p as usize] != id + 1 {
                pattern_last[p as usize] = id + 1;
                matches.push((p, id));
            }
        });
        i = next;
    }
    chunk.end(plan, &mut acc);
    (acc, matches)
}

#[derive(Clone, Copy)]
enum Query {
    Bool(usize),
    Phrase(usize),
}

/// The queries of whole lines of queries.txt, in order, numbered within those lines.
#[derive(Default)]
struct Parsed {
    queries: Vec<Query>,
    bools: Vec<Code>,
    phrase_terms: Vec<u32>, // a phrase's terms go to one flat list, so a line allocates nothing
    phrase_ends: Vec<u32>,
}

/// Parses whole lines of queries.txt; every line is its own query. A line dedupe (whole lines as keys of a std
/// HashMap) was 70 M of the parse's 110 M instructions; removing it took the VM's parse from 32 ms to 22.7 ms,
/// for 349 repeated lines of seed 1's 50,000; a repeated line now only costs its own evaluation (b-38, b-38a).
fn parse(text: &[u8], terms: &Interner) -> Parsed {
    let (mut p, mut ids) = (Parsed::default(), 0..0);
    let mut intern = |t: &[u8]| terms.intern(t, &mut ids);
    // One pass cuts the text at every space, double quote and LF, 32 bytes per compare (`Chunk`); the empty
    // pieces are dropped, so a PHRASE line is its keyword and terms (spec 2.3: terms are a-z and 0-9). It replaces
    // a split per line and per word, byte by byte, and a Vec per line: the parse fell from 76 M to 37 M
    // instructions (cachegrind, seed 1) and from 22.4 ms to 16.6 ms on the VM (medians of 15 runs). (b-69)
    let (mut words, mut from) = (Vec::new(), 0);
    for i in (0..text.len()).step_by(32) {
        let chunk = Chunk::load(text, i);
        let lf = chunk.eq_mask(b'\n');
        let mut cuts = lf | chunk.eq_mask(b' ') | chunk.eq_mask(b'"');
        while cuts != 0 {
            let at = i + cuts.trailing_zeros() as usize;
            if at > from {
                words.push(&text[from..at]);
            }
            if lf >> (at - i) & 1 != 0 && !words.is_empty() {
                p.queries.push(if words[0] == b"PHRASE" {
                    p.phrase_terms.extend(words[1..].iter().map(|t| intern(t)));
                    p.phrase_ends.push(p.phrase_terms.len() as u32);
                    Query::Phrase(p.phrase_ends.len() - 1)
                } else {
                    // spec 2.3: "BOOL" term *(op term); op is AND (OP_AND = 0), OR or NOT.
                    let mut code = [NIL; MAX_BOOL_TERMS];
                    code[0] = intern(words[1]);
                    for (w, pair) in code[1..].iter_mut().zip(words[2..].chunks(2)) {
                        let (p0, p1) = (pair[0], pair[1]);
                        let op = (p0 == b"OR") as u32 * OP_OR + (p0 == b"NOT") as u32 * OP_NOT;
                        *w = op << OP_SHIFT | intern(p1);
                    }
                    p.bools.push(code);
                    Query::Bool(p.bools.len() - 1)
                });
                words.clear();
            }
            (from, cuts) = (at + 1, cuts & (cuts - 1));
        }
    }
    // The ids this worker drew and left unused are named by no slot; zeroed, every entry below `next` is written.
    for k in ids.start..ids.end.min(terms.cap) {
        unsafe { terms.bytes.add(k as usize).write(TermBytes([0; MAX_TERM])) };
    }
    p
}

fn main() {
    let mut args = std::env::args().skip(1);
    let data_dir = args.next().unwrap_or_else(|| "/data".into());
    let out_dir = args.next().unwrap_or_else(|| "/out".into());
    // SD_WORKERS is the rule-30 test-only override (the hash runs force 1, 4, 12 and 24 workers); the board never
    // sets it, so the scored run sizes itself from available_parallelism.
    let workers = match std::env::var("SD_WORKERS").map(|v| v.parse::<usize>()) {
        Ok(Ok(n)) if n > 0 => n,
        _ => std::thread::available_parallelism().map_or(1, |n| n.get()),
    };

    // Queries: each worker parses one range of whole lines (`parse`), all into one term table (`Interner`).
    let qtext = map_file(&format!("{data_dir}/queries.txt"));
    let cut = |k: usize| line_start(qtext, qtext.len() * k / workers);
    // The table starts at the size the old doublings reached for one distinct term per 24 bytes of queries.txt
    // (seed 1: 75,747 in 1.76 MB, 2^18 slots), sized from the input as a speed hint only: more terms parse again at
    // twice the slots. The doublings up to there were 4 ms of the VM's 17 ms parse (medians of 21 runs, seed 1). (b-69)
    let mut n_slots = (qtext.len() / 8).next_power_of_two().max(2);
    let (terms, parts) = loop {
        let bytes = Vec::with_capacity(n_slots / 3);
        let mut table = TermTable {
            slots: vec![0; n_slots],
            shift: 32 - n_slots.trailing_zeros(),
            bytes,
        };
        let interner = Interner::new(&mut table);
        let parts = parallel(workers, |k| parse(&qtext[cut(k)..cut(k + 1)], &interner));
        let (n, cap) = (interner.next.into_inner(), interner.cap);
        if n <= cap {
            unsafe { table.bytes.set_len(n as usize) }; // every entry below n is written (`parse`)
            break (table, parts);
        }
        n_slots *= 2;
    };
    // The ranges joined in file order, their BOOL and PHRASE numbers moved past the ranges before them.
    let (mut queries, mut bools, mut phrase_terms, mut phrase_ends) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for p in parts {
        let (b, ph, t) = (bools.len(), phrase_ends.len(), phrase_terms.len() as u32);
        queries.extend(p.queries.iter().map(|&q| match q {
            Query::Bool(i) => Query::Bool(b + i),
            Query::Phrase(i) => Query::Phrase(ph + i),
        }));
        bools.extend(p.bools);
        phrase_terms.extend(p.phrase_terms);
        phrase_ends.extend(p.phrase_ends.iter().map(|&e| e + t));
    }
    let n_terms = terms.bytes.len();

    let corpus = Corpus::new(map_file(&format!("{data_dir}/docs.txt")));
    // The token table: each term's key, its value the term id until the renumbering below.
    let keys: Vec<u64> = terms.bytes.iter().map(|b| term_key(&b.0)).collect();
    let mut table = Table::new(table_bits(n_terms));
    keys.iter()
        .enumerate()
        .filter(|(_, &k)| k != 0)
        .for_each(|(t, &k)| _ = table.insert(k, t as u32));
    let (df, n_sample) = estimate_df(&corpus, &table, n_terms, workers);
    // Renumber the terms: BOOL terms first, so a chunk's rows and log keys cover them only (53,444 of 75,747 terms
    // on seed 1), each part by descending sampled df, so the terms most tokens hit have neighbouring ids and their
    // per-term entries and rows share a few hundred KB of cache lines. Only speed depends on the order.
    let mut in_bool = vec![false; n_terms];
    bools
        .iter()
        .flatten()
        .filter(|&&w| w != NIL)
        .for_each(|&w| in_bool[(w & TERM) as usize] = true);
    let n_bool = in_bool.iter().filter(|&&b| b).count();
    let key: Vec<u64> = (0..n_terms)
        .map(|t| (!in_bool[t] as u64) << 32 | (!df[t]) as u64)
        .collect();
    let mut order: Vec<usize> = (0..n_terms).collect();
    order.sort_by_key(|&t| key[t]);
    let mut new_id = vec![0; n_terms];
    for (new, &old) in order.iter().enumerate() {
        new_id[old] = new as u32;
    }
    let phrase_keys = phrase_terms.iter().map(|&t| keys[t as usize]).collect();
    let mut df = vec![0; n_terms];
    for (t, &d) in key.iter().enumerate() {
        df[new_id[t] as usize] = !d as u32;
    }
    for w in bools.iter_mut().flatten().filter(|w| **w != NIL) {
        *w = *w & !TERM | new_id[(*w & TERM) as usize];
    }
    for t in &mut phrase_terms {
        *t = new_id[*t as usize];
    }
    // The two plans are independent: build them on two threads.
    let (plan, phrases) = std::thread::scope(|s| {
        let phrases =
            s.spawn(|| PhrasePlan::new(&phrase_terms, phrase_keys, &phrase_ends, n_terms, &df));
        // Dense: sampled in at least 1/64 of the sampled docs, so a chunk holds about CHUNK / 64 = ROW or more of
        // its docs and a row of ROW words is no larger than its log entries would be. BOOL ids are in descending
        // df, so the dense terms are the first n_dense. (b-68)
        let n_dense = (0..n_bool)
            .take_while(|&t| df[t] > 0 && df[t] as u64 * 64 >= n_sample as u64)
            .count();
        let plan = BoolPlan::new(&bools, n_dense, n_bool, &df);
        (plan, phrases.join().unwrap())
    });
    // Each term's meta word (`SPARSE_BIT`) in place of its old id.
    table.remap(|old| {
        let t = new_id[old as usize];
        let sparse = (plan.n_dense..plan.n_bool).contains(&(t as usize));
        let anchor = phrases.anchor_bits[t as usize / 64] >> (t % 64) & 1 != 0;
        t | (!sparse as u32) << SPARSE_BIT | (!anchor as u32) << ANCHOR_BIT
    });

    // The pass: one doc range per worker, cut at line starts.
    let docs = corpus.docs;
    let cut = |k: usize| line_start(docs, docs.len() * k / workers);
    let shards = parallel(workers, |k| {
        scan(&corpus, cut(k), cut(k + 1), &table, &phrases, &plan)
    });

    // An answer is count, exact id sum and the smallest ids (spec 4). BOOL answers are by slot.
    let mut bool_results = vec![(0, 0, Vec::new()); bools.len()];
    for (r, kept) in bool_results.iter_mut().zip(&plan.ids.kept) {
        r.2 = std::mem::take(&mut *kept.lock().unwrap());
    }
    for (acc, _) in &shards {
        for (r, a) in bool_results.iter_mut().zip(acc) {
            (r.0, r.1) = (r.0 + a.count as u64, r.1 + a.sum);
        }
    }
    // Shards are in doc order and each lists its matches in doc order, so every pattern's docs arrive ascending.
    let mut phrase_results = vec![(0, 0, Vec::new()); phrase_ends.len()];
    for &(p, d) in shards.iter().flat_map(|s| &s.1) {
        let r = &mut phrase_results[p as usize];
        (r.0, r.1) = (r.0 + 1, r.1 + d as u64);
        if r.2.len() < FIRST_IDS {
            r.2.push(d);
        }
    }

    // Each worker formats one contiguous slice of the queries, and the slices are joined in order: the formatting
    // was 9-10 ms of serial time after the pass on the VM at 4 workers (b-38).
    let format = |k: usize| {
        let mut out = Vec::with_capacity(queries.len() * 32 / workers);
        for &q in &queries[queries.len() * k / workers..queries.len() * (k + 1) / workers] {
            let (count, sum, first) = match q {
                Query::Bool(b) => &bool_results[plan.slot[b] as usize],
                Query::Phrase(p) => &phrase_results[p],
            };
            let first = first.iter().map(|&id| id as u64);
            for n in [*count, *sum].into_iter().chain(first) {
                push_decimal(&mut out, n);
                out.push(b' ');
            }
            *out.last_mut().unwrap() = b'\n';
        }
        out
    };
    let out = parallel(workers, format).concat();
    std::fs::write(format!("{out_dir}/results.txt"), out).expect("write results.txt");
    // results.txt is written and closed: exit now, without dropping the tables (frees and unmaps the run does
    // not need).
    std::process::exit(0);
}

/// Appends n in decimal without leading zeros (spec 4); `write!` through fmt was most of the output's time.
/// The recursion is at most 20 deep: a u64 has at most 20 decimal digits.
fn push_decimal(out: &mut Vec<u8>, n: u64) {
    if n >= 10 {
        push_decimal(out, n / 10);
    }
    out.push(b'0' + (n % 10) as u8);
}
