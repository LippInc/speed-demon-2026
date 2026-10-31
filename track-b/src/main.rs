//! Track B: answer the BOOL and PHRASE queries of /data/queries.txt over /data/docs.txt (spec: specs/b-index.md).
//!
//! Queries are read first, so only their terms are looked up. One parallel pass over the memory-mapped corpus
//! tokenizes every document and decides every BOOL query (per document, or 64 documents at a time) and every
//! PHRASE pattern (by anchor checks) for it; nothing is indexed.

use std::arch::x86_64::*;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Mutex;

const NIL: u32 = u32::MAX;
const MAX_TERM: usize = 32; // spec 2.3: a query term is 1 to 32 bytes, so a longer token is never a query term
const FIRST_IDS: usize = 10; // spec 4: the ten smallest ids

/// 32 bytes of the corpus in an AVX2 register (x86-64-v3, the build target), zero past the end of `data`.
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

    /// Bit k: byte k is a token byte, a-z, A-Z or 0-9 (spec 2.2). Signed compares, so bytes from 0x80 never match.
    #[inline(always)]
    fn token_mask(&self) -> u32 {
        unsafe {
            let lower = _mm256_or_si256(self.0, _mm256_set1_epi8(0x20));
            let between = |v, lo: u8, hi: u8| {
                let above = _mm256_cmpgt_epi8(v, _mm256_set1_epi8(lo as i8 - 1));
                _mm256_andnot_si256(_mm256_cmpgt_epi8(v, _mm256_set1_epi8(hi as i8)), above)
            };
            let tok = _mm256_or_si256(between(lower, b'a', b'z'), between(self.0, b'0', b'9'));
            _mm256_movemask_epi8(tok) as u32
        }
    }

    /// Bit k: byte k ends the doc, an LF or the end of `data` (a last line without LF ends there).
    #[inline(always)]
    fn lf(&self, data: &[u8], i: usize) -> u32 {
        let lf = unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(self.0, _mm256_set1_epi8(b'\n' as i8))) as u32 };
        let past_end = data.len().saturating_sub(i);
        if past_end < 32 {
            return lf | 1 << past_end;
        }
        lf
    }

    /// The first n bytes lower-cased (bit 5 set), the rest zero.
    #[inline(always)]
    fn folded(&self, n: usize) -> [u8; 32] {
        let mut out = [0u8; 32];
        unsafe {
            let keep = _mm256_cmpgt_epi8(_mm256_set1_epi8(n as i8), _mm256_setr_epi8(
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
                29, 30, 31,
            ));
            let v = _mm256_and_si256(_mm256_or_si256(self.0, _mm256_set1_epi8(0x20)), keep);
            _mm256_storeu_si256(out.as_mut_ptr() as *mut __m256i, v);
        }
        out
    }
}

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
}

/// Maps a file read-only. The corpus is never copied into the heap: its pages stay in the page cache, off our M.
fn map_file(path: &str) -> &'static [u8] {
    let file = File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let len = file.metadata().expect("stat").len() as usize;
    // PROT_READ, MAP_PRIVATE. An empty file maps one page (mmap refuses length 0) that the empty slice never reads.
    let ptr = unsafe { mmap(std::ptr::null_mut(), len.max(1), 1, 2, file.as_raw_fd(), 0) };
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

/// The hash of a term zero-padded to 32 bytes: its four words multiplied by odd constants and xor-ed, high and low
/// halves folded. A fixed amount of work per token, with no loop whose exit depends on the token's length.
#[inline(always)]
fn hash32(t: &[u8; MAX_TERM]) -> u32 {
    const K: [u64; 4] = [0x9e37_79b9_7f4a_7c15, 0xc2b2_ae3d_27d4_eb4f, 0x1656_67b1_9e37_79f9, 0x27d4_eb2f_1656_67c5];
    let w = |k: usize| u64::from_le_bytes(t[8 * k..8 * k + 8].try_into().unwrap()).wrapping_mul(K[k]);
    let h = w(0) ^ w(1) ^ w(2) ^ w(3);
    (h >> 32) as u32 ^ h as u32
}

/// The query terms: interned into it while queries.txt is parsed, then probed for every token of the pass. A slot
/// holds the term's hash and id + 1 in one u64, so a miss reads one cache line; a hash match is then confirmed
/// against the term's bytes, stored inline and zero-padded to 32 B (one more line). b-4 probed slots sized from
/// every term occurrence (4 MB), a separate hash array, and a slice into queries.txt per term.
struct TermTable {
    // 4 B slots: the hash's top 2 bits | (term id + 1) in the low 30 (TERM: ids stay below 2^30, BoolPlan::new),
    // 0 = empty. The tag only filters and the bytes decide, so any query file is exact. Half the bytes of 8 B slots
    // for the probe's slot misses (b-36; b-34a measured -5 ms on the box).
    slots: Vec<u32>,
    shift: u32,
    bytes: Vec<TermBytes>, // by term id
}

#[repr(align(32))] // one entry never straddles two cache lines
#[derive(Clone, Copy)]
struct TermBytes([u8; MAX_TERM]);

impl TermTable {
    /// The id of `term` (spec 2.3: 1 to 32 bytes), added if new. The slots double whenever they would pass load 1/3,
    /// rehashed from the entries: the 1 MB that 8 B slots took at load 2/3, with fewer collisions for the 2-bit tag
    /// (b-36, cachegrind, 1 worker, seed 1: find's mispredicts 25.2 M at load 2/3, 14.2 M at 1/3; 19.0 M with 8 B).
    fn intern(&mut self, term: &[u8]) -> u32 {
        let mut padded = [0u8; MAX_TERM];
        padded[..term.len()].copy_from_slice(term);
        let h = hash32(&padded);
        if let Some(t) = self.find(&padded, h) {
            return t;
        }
        if 3 * (self.bytes.len() + 1) > self.slots.len() {
            (self.slots, self.shift) = (vec![0; self.slots.len() * 2], self.shift - 1);
            (0..self.bytes.len()).for_each(|t| self.place(hash32(&self.bytes[t].0), t as u32 + 1));
        }
        self.bytes.push(TermBytes(padded));
        self.place(h, self.bytes.len() as u32);
        self.bytes.len() as u32 - 1
    }

    fn place(&mut self, h: u32, id1: u32) {
        let mut i = self.slot(h);
        while self.slots[i] != 0 {
            i = (i + 1) & (self.slots.len() - 1);
        }
        self.slots[i] = h & !TERM | id1;
    }

    /// Renumbers the terms by ascending `key` in place: the slots keep their hash and get the new id, and the
    /// entries move (cheaper than rebuilding the table after the df sample). Returns each term's new id.
    fn renumber(&mut self, key: &[u64]) -> Vec<u32> {
        let mut order: Vec<usize> = (0..key.len()).collect();
        order.sort_by_key(|&t| key[t]);
        let mut new_id = vec![0; key.len()];
        for (new, &old) in order.iter().enumerate() {
            new_id[old] = new as u32;
        }
        for s in self.slots.iter_mut().filter(|s| **s != 0) {
            *s = *s & !TERM | (new_id[(*s & TERM) as usize - 1] + 1);
        }
        self.bytes = order.iter().map(|&t| self.bytes[t]).collect();
        new_id
    }

    #[inline]
    fn slot(&self, h: u32) -> usize {
        (h.wrapping_mul(0x9e37_79b1) >> self.shift) as usize
    }

    /// The tag only filters; the bytes decide. Token and entry are both zero-padded to 32 bytes, so one 32-byte
    /// compare settles the bytes and the length (it replaced a memcmp call and a length test per hash match).
    #[inline]
    fn find(&self, token: &[u8; MAX_TERM], h: u32) -> Option<u32> {
        let mut i = self.slot(h);
        loop {
            let s = self.slots[i];
            if s == 0 {
                return None;
            }
            if (s ^ h) & !TERM == 0 {
                let t = (s & TERM) - 1;
                if self.bytes[t as usize].0 == *token {
                    return Some(t);
                }
            }
            i = (i + 1) & (self.slots.len() - 1);
        }
    }

    /// Tokenizes the doc text from `i` to its LF (spec 2.2), calling `on_token` with the term id of each token that is
    /// a query term and None for any other token; returns the index just past the LF. The text is classified 32 bytes
    /// at a time (`Chunk`), and tokens are cut where the token-byte mask changes, so no branch is taken per byte: the
    /// byte loop's two branches (separator skip, token end) were 71 M of the run's 161 M mispredicts (cachegrind,
    /// 1 worker, seed 1).
    #[inline(always)]
    fn tokenize(&self, data: &[u8], mut i: usize, mut on_token: impl FnMut(Option<u32>)) -> usize {
        let mut start = NIL as usize; // the token in progress, if any
        loop {
            let chunk = Chunk::load(data, i);
            let (lf, tok) = (chunk.lf(data, i), chunk.token_mask());
            let tok = tok & lf.wrapping_sub(1) & !lf; // only the bytes before the doc's LF
            let in_token = (start != NIL as usize) as u32;
            let mut edges = tok ^ (tok << 1 | in_token); // bit k: a token starts or ends at byte i + k
            while edges != 0 {
                let k = i + edges.trailing_zeros() as usize;
                if start == NIL as usize {
                    start = k;
                } else {
                    on_token(self.lookup(data, start, k));
                    start = NIL as usize;
                }
                edges &= edges - 1;
            }
            if lf != 0 {
                let end = i + lf.trailing_zeros() as usize;
                if start != NIL as usize {
                    on_token(self.lookup(data, start, end));
                }
                return end + 1;
            }
            i += 32;
        }
    }

    /// The term id of the token data[from..to], if it is a query term. Its bytes are loaded once, lower-cased by
    /// setting bit 5 (spec 2.2: it maps A-Z to a-z and leaves a-z and 0-9 alone) and zero-padded to 32.
    #[inline(always)]
    fn lookup(&self, data: &[u8], from: usize, to: usize) -> Option<u32> {
        let n = to - from;
        if n > MAX_TERM {
            return None;
        }
        let token = Chunk::load(data, from).folded(n);
        self.find(&token, hash32(&token))
    }
}

/// Lists of items by term id (CSR): list t is `items[start[t]..start[t + 1]]`, in the order `items` gives them.
fn by_term<T: Copy + Default>(n_terms: usize, items: &[(u32, T)]) -> (Vec<u32>, Vec<T>) {
    let mut start = vec![0u32; n_terms + 2];
    for &(t, _) in items {
        start[t as usize + 2] += 1;
    }
    for t in 2..start.len() {
        start[t] += start[t - 1];
    }
    // start[t + 1] is now where list t begins; filling list t moves it to where list t ends.
    let mut lists = vec![T::default(); items.len()];
    for &(t, x) in items {
        lists[start[t as usize + 1] as usize] = x;
        start[t as usize + 1] += 1;
    }
    start.pop();
    (start, lists)
}

/// PHRASE matching by anchor checks. Each PHRASE line is one pattern and gets one anchor: its term with the
/// lowest estimated df (the df sample only picks the anchor; a term in no doc has df 0, so a phrase with an absent
/// term is never even checked). Each doc records its token sequence as term ids (NIL for a token that is no query
/// term, which no phrase can match across); every occurrence of an anchor term at position i is a candidate start
/// i - j for each pattern anchored there at offset j, confirmed by comparing the pattern's ids with the doc's.
/// It replaces an Aho-Corasick automaton whose edge probes, fail walks and output checks were the pass's largest
/// part (about 0.9-1.1 s of 4.0-4.3 s at 1 worker on seed 1).
struct PhrasePlan {
    pattern_start: Vec<u32>, // pattern p is terms[pattern_start[p]..pattern_start[p + 1]]
    terms: Vec<u32>,
    anchor_bits: Vec<u64>, // by term id: the term anchors at least one pattern (tested for every token)
    anchor_start: Vec<u32>, // by term id, into anchors (CSR)
    anchors: Vec<(u32, u32)>, // (pattern, offset of the anchor in it)
}

impl PhrasePlan {
    fn new(patterns: &[Vec<u32>], n_terms: usize, df: &[u32]) -> Self {
        let mut pattern_start = vec![0];
        let mut anchor_bits = vec![0u64; n_terms.div_ceil(64)];
        let mut anchors = Vec::new();
        for (pi, p) in patterns.iter().enumerate() {
            pattern_start.push(pattern_start[pi] + p.len() as u32);
            let j = (0..p.len()).min_by_key(|&j| df[p[j] as usize]).unwrap();
            anchor_bits[p[j] as usize / 64] |= 1 << (p[j] % 64);
            anchors.push((p[j], (pi as u32, j as u32)));
        }
        let (anchor_start, anchors) = by_term(n_terms, &anchors);
        let terms = patterns.concat();
        PhrasePlan { pattern_start, terms, anchor_bits, anchor_start, anchors }
    }

    /// Calls `found(p)` for each pattern that starts at a candidate position of `seq`; `anchored` lists the
    /// positions of anchor terms.
    #[inline]
    fn check(&self, seq: &[u32], anchored: &[u32], mut found: impl FnMut(u32)) {
        for &i in anchored {
            let t = seq[i as usize] as usize;
            let (from, to) = (self.anchor_start[t], self.anchor_start[t + 1]);
            for &(p, j) in &self.anchors[from as usize..to as usize] {
                let from = self.pattern_start[p as usize] as usize;
                let terms = &self.terms[from..self.pattern_start[p as usize + 1] as usize];
                if let Some(start) = (i as usize).checked_sub(j as usize) {
                    if seq.get(start..start + terms.len()) == Some(terms) {
                        found(p);
                    }
                }
            }
        }
    }
}

/// The start of the line after the first LF at or after `from`: 0 for `from` 0, the end if no LF follows (spec 2.1:
/// every line ends with LF).
/// A `from` that is itself a line start returns the next line; both ends of a cut use this, so the ranges still tile.
fn line_start(data: &[u8], from: usize) -> usize {
    match data[from..].iter().position(|&b| b == b'\n') {
        _ if from == 0 => 0,
        Some(p) => from + p + 1,
        None => data.len(),
    }
}

/// Reads the decimal doc id that starts the line at `i` (spec 2.1); returns it and the index just past its TAB.
#[inline]
fn parse_id(data: &[u8], mut i: usize) -> (u32, usize) {
    let mut id = 0u32;
    while i < data.len() && data[i] != b'\t' {
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

fn estimate_df(docs: &[u8], dict: &TermTable, workers: usize) -> Vec<u32> {
    let (n_terms, window) = (dict.bytes.len(), docs.len() / SAMPLE_SHARE / SAMPLE_WINDOWS);
    // The windows are spread over the workers (window k on worker k % n) and their counts summed: the sample was
    // 15 ms of serial prologue on the box.
    let n = workers.clamp(1, SAMPLE_WINDOWS);
    let parts = parallel(n, |w| {
        let (mut df, mut seen, mut stamp) = (vec![0u32; n_terms], vec![0u32; n_terms], 0u32);
        for k in (w..SAMPLE_WINDOWS).step_by(n) {
            let mut i = line_start(docs, docs.len() * k / SAMPLE_WINDOWS);
            let stop = (i + window).min(docs.len());
            while i < stop {
                stamp += 1;
                i = dict.tokenize(docs, parse_id(docs, i).1, |t| {
                    if let Some(t) = t.filter(|&t| seen[t as usize] != stamp) {
                        seen[t as usize] = stamp;
                        df[t as usize] += 1;
                    }
                });
            }
        }
        df
    });
    let mut df = vec![0u32; n_terms];
    for part in &parts {
        df.iter_mut().zip(part).for_each(|(d, p)| *d += p);
    }
    df
}

/// BOOL queries evaluated inside the pass, 64 docs at a time, so no posting list is ever stored (b-2 kept 47.6 MB of
/// blocks and 7.3 MB of heads for 29.2 MB of varints at 12 workers). Under the strict left fold (spec 3) a doc is
/// in R exactly when the same fold over the booleans "the doc contains t" is true, so the fold over words whose bit
/// j says "doc base + j contains t" decides 64 docs at once (`fold_window`). A query is only folded for the windows
/// where one of its trigger terms occurs, a cover such that every doc in R contains one of them (`cover`); every
/// query has one, so the path is exact for every shape, NOT forms and absent terms included.
const MAX_BOOL_TERMS: usize = 6; // spec 2.3: a BOOL query has 1 to 6 terms
/// A query's code: its first term, then op << OP_SHIFT | term per operator, NIL-padded.
type Code = [u32; MAX_BOOL_TERMS];
const OP_SHIFT: u32 = 30; // term ids stay below 2^30 (asserted in BoolPlan::new)
const TERM: u32 = (1 << OP_SHIFT) - 1;
const OP_AND: u32 = 0;
const OP_OR: u32 = 1;
const OP_NOT: u32 = 2;
const OP_KEEP: u32 = 3; // pads a trigger's code to 3 words: r is left as it is

/// One query listed under one of its trigger terms, with its code inline so a window's folds read the
/// trigger list front to back instead of jumping to each query.
#[derive(Clone, Copy, Default)]
struct Trigger {
    query: u32, // accumulator slot
    code: Code,
}

struct BoolPlan {
    trigger_start: Vec<u32>, // per term, into triggers (CSR)
    triggers: Vec<Trigger>,
    // Per query its accumulator slot. Slots follow the trigger lists, so the queries one term triggers have
    // neighbouring accumulators and first-id bounds (b-24b, which numbered them by query, read +3 ms on the box).
    slot: Vec<u32>,
    ids: FirstIds, // by slot
}

/// The estimated number of docs holding the terms, summed.
fn df_sum(terms: &[u32], df: &[u32]) -> u64 {
    terms.iter().map(|&t| df[t as usize] as u64).sum()
}

/// Trigger terms of a query: every doc in R contains one of them. {first} holds for R = S(first); OR adds its
/// operand; AND narrows R to S(t), so it may replace the cover by {t} when t is estimated rarer; NOT keeps it.
fn cover(q: &Code, df: &[u32]) -> Vec<u32> {
    let mut cover = vec![q[0]];
    for &w in q[1..].iter().take_while(|&&w| w != NIL) {
        let t = w & TERM;
        match w >> OP_SHIFT {
            OP_OR => cover.push(t),
            OP_AND if df_sum(&[t], df) < df_sum(&cover, df) => cover = vec![t],
            _ => {}
        }
    }
    cover.sort_unstable();
    cover.dedup();
    cover
}

impl BoolPlan {
    /// The BOOL terms are the ids 0..n_terms.
    fn new(queries: &[Code], n_terms: usize, df: &[u32]) -> Self {
        assert!(n_terms < 1 << OP_SHIFT);
        let mut triggers = Vec::new();
        for (qi, q) in queries.iter().enumerate() {
            let cover = cover(q, df);
            let mut code = *q;
            code[1..3].iter_mut().filter(|w| **w == NIL).for_each(|w| *w = OP_KEEP << OP_SHIFT | q[0]);
            let trig = Trigger { query: qi as u32, code };
            triggers.extend(cover.iter().map(|&t| (t, trig)));
        }
        let (trigger_start, mut triggers) = by_term(n_terms, &triggers);
        // Every query has a trigger, so every query gets a slot.
        let (mut slot, mut n_slots) = (vec![NIL; queries.len()], 0);
        for trig in &mut triggers {
            let q = trig.query as usize;
            if slot[q] == NIL {
                (slot[q], n_slots) = (n_slots, n_slots + 1);
            }
            trig.query = slot[q];
        }
        let ids = FirstIds::new(queries.len());
        BoolPlan { trigger_start, triggers, slot, ids }
    }
}

/// spec 3: strict left fold, no precedence; `a OR b AND c` is `(a OR b) AND c` (b-index.md:136-141), over 64 docs
/// at once: bit j of a term's word says doc base + j contains it. AND, OR and NOT only combine bits that some term
/// set, so docs missing from the range stay 0. Each step is r & (b ^ x | y) | b & o, with the operator's masks from
/// OP_MASKS, so the operator takes no branch: its match was 16.9 M of the flush's mispredicts (cachegrind, 1
/// worker, seed 1).
#[inline]
fn fold_window(code: &[u32; MAX_BOOL_TERMS], words: &[u64]) -> u64 {
    // (x, y, o) for AND, OR, NOT and KEEP
    const OP_MASKS: [(u64, u64, u64); 4] = [(0, 0, 0), (0, !0, !0), (!0, 0, 0), (0, !0, 0)];
    let step = |r: u64, w: u32| {
        let (b, (x, y, o)) = (words[(w & TERM) as usize], OP_MASKS[(w >> OP_SHIFT) as usize]);
        r & (b ^ x | y) | b & o
    };
    // Two fixed steps (OP_KEEP pads codes of 1 or 2 terms), then a tail that only codes of 4 to 6 terms enter, so
    // the loop exit is not a per-fold guess between lengths 1, 2 and 3: the length test was 13.7 M of the flush's
    // mispredicts (cachegrind, 1 worker, seed 1).
    let r = step(step(words[code[0] as usize], code[1]), code[2]);
    code[3..].iter().take_while(|&&w| w != NIL).fold(r, |r, &w| step(r, w))
}

/// Bit k of a bit position j, for every j at once: the sum of the set positions of w is
/// sum over k of popcount(w & BIT_SLICES[k]) << k, so a window's id sum needs no walk over its matches.
const BIT_SLICES: [u64; 6] = [
    0xaaaa_aaaa_aaaa_aaaa,
    0xcccc_cccc_cccc_cccc,
    0xf0f0_f0f0_f0f0_f0f0,
    0xff00_ff00_ff00_ff00,
    0xffff_0000_ffff_0000,
    0xffff_ffff_0000_0000,
];

/// One worker's current window of docs base..base + 64: bit j of words[t] says doc base + j contains term t, and
/// `touched` lists the terms whose word is not zero.
struct Window {
    base: u32,
    words: Vec<u64>, // by term id
    // (term, its trigger range): the range is read at the hit that first sets the term's word, next to the probe
    // that just found the term, instead of as a miss on trigger_start in the flush.
    touched: Vec<(u32, u32, u32)>,
    n_touched: usize, // touched[..n_touched] is this window's; the array has a slot per BOOL term and one more
}

impl Window {
    /// Folds every query that a term of the window triggers, once, and clears the window's words.
    fn flush(&mut self, plan: &BoolPlan, acc: &mut [BoolAcc]) {
        let (base, stamp) = (self.base, self.base / 64 + 1);
        for &(_, from, to) in &self.touched[..self.n_touched] {
            for trig in &plan.triggers[from as usize..to as usize] {
                // A query listed under several touched terms is reached once per term and counts once: every
                // query is stamped and a repeat's result masked to zero, with no branch on a MULTI flag or the stamp
                // (b-35: those were 8.9 M of the flush's mispredicts, cachegrind, 1 worker, seed 1). A query
                // listed under one term is reached once per window, so its stamp never matches early.
                let q = trig.query as usize;
                let repeat = acc[q].done == stamp;
                acc[q].done = stamp;
                let r = fold_window(&trig.code, &self.words) & (repeat as u64).wrapping_sub(1);
                if r == 0 {
                    continue;
                }
                let n = r.count_ones();
                let offsets: u32 = (0..6).map(|k| (r & BIT_SLICES[k]).count_ones() << k).sum();
                acc[q].count += n;
                acc[q].sum += base as u64 * n as u64 + offsets as u64;
                // First ids only while this window can still hold one: the bound is NIL until ten are kept.
                let mut bits = r;
                while bits != 0 && base + bits.trailing_zeros() < plan.ids.bound[q].load(Relaxed) {
                    plan.ids.offer(q, base + bits.trailing_zeros());
                    bits &= bits - 1;
                }
            }
        }
        for &(t, _, _) in &self.touched[..self.n_touched] {
            self.words[t as usize] = 0;
        }
        self.n_touched = 0;
    }
}

/// Per worker and BOOL query slot: the evaluation stamp and the running count and sum.
#[derive(Clone, Copy, Default)]
struct BoolAcc {
    done: u32, // last window index + 1 this query was folded for
    count: u32,
    sum: u64,
}

/// The FIRST_IDS smallest matching ids of every BOOL query, shared by all workers. Keeping them per worker held
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

fn scan(data: &[u8], dict: &TermTable, phrases: &PhrasePlan, plan: &BoolPlan) -> Shard {
    let (mut acc, mut matches) = (vec![BoolAcc::default(); plan.slot.len()], Vec::new());
    // Flushing a window of no words changes nothing, so the first window needs no special case.
    let words = vec![0u64; plan.trigger_start.len() - 1]; // by BOOL term id
    let touched = vec![(0, 0, 0); words.len() + 1];
    let mut window = Window { base: 0, words, touched, n_touched: 0 };
    // The doc's tokens as term ids (NIL for any other token) and the positions in `seq` of anchor terms; per
    // pattern the last doc id + 1 it was reported for.
    let (mut seq, mut anchored) = (Vec::new(), Vec::new());
    let mut pattern_last = vec![0u32; phrases.anchors.len()];
    let mut i = 0;
    while i < data.len() {
        let (id, text) = parse_id(data, i);
        if id / 64 * 64 != window.base {
            window.flush(plan, &mut acc);
            window.base = id / 64 * 64;
        }
        i = dict.tokenize(data, text, |hit| {
            let Some(t) = hit else {
                seq.push(NIL);
                return;
            };
            if phrases.anchor_bits[t as usize / 64] >> (t % 64) & 1 != 0 {
                anchored.push(seq.len() as u32);
            }
            seq.push(t);
            let Some(word) = window.words.get_mut(t as usize) else {
                return; // a phrase-only term
            };
            // The entry is written at n_touched on every hit and kept only by the first one in the window, with no
            // first-touch branch (b-37: 10.9 M mispredicts in cachegrind, 1 worker, seed 1).
            let (from, to) = (plan.trigger_start[t as usize], plan.trigger_start[t as usize + 1]);
            window.touched[window.n_touched] = (t, from, to);
            window.n_touched += (*word == 0) as usize;
            *word |= 1 << (id % 64);
        });
        // A phrase never spans two documents: seq holds this doc only.
        phrases.check(&seq, &anchored, |p| {
            if pattern_last[p as usize] != id + 1 {
                pattern_last[p as usize] = id + 1;
                matches.push((p, id));
            }
        });
        seq.clear();
        anchored.clear();
    }
    window.flush(plan, &mut acc);
    (acc, matches)
}

#[derive(Clone, Copy)]
enum Query {
    Bool(usize),
    Phrase(usize),
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

    // Queries: intern every term; every line is its own query. A line dedupe (whole lines as keys of a std
    // HashMap) was 70 M of the parse's 110 M instructions and most of its 32 ms of serial time on the VM, for 349
    // repeated lines of seed 1's 50,000; a repeated line now only costs its own evaluation (b-38).
    let mut terms = TermTable { slots: vec![0; 2], shift: 31, bytes: Vec::new() };
    let (mut bools, mut patterns): (Vec<Code>, Vec<Vec<u32>>) = (Vec::new(), Vec::new());
    let parse = |line: &[u8]| {
        if let Some(body) = line.strip_prefix(b"PHRASE \"") {
            let phrase = body[..body.len() - 1].split(|&b| b == b' ');
            patterns.push(phrase.map(|t| terms.intern(t)).collect());
            return Query::Phrase(patterns.len() - 1);
        }
        // spec 2.3: "BOOL" term *(op term), single spaces; op is AND (OP_AND = 0), OR or NOT.
        let words: Vec<&[u8]> = line[5..].split(|&b| b == b' ').collect();
        let mut code = [NIL; MAX_BOOL_TERMS];
        code[0] = terms.intern(words[0]);
        for (w, pair) in code[1..].iter_mut().zip(words[1..].chunks(2)) {
            let op = (pair[0] == b"OR") as u32 * OP_OR + (pair[0] == b"NOT") as u32 * OP_NOT;
            *w = op << OP_SHIFT | terms.intern(pair[1]);
        }
        bools.push(code);
        Query::Bool(bools.len() - 1)
    };
    let qtext = map_file(&format!("{data_dir}/queries.txt"));
    let lines = qtext.split(|&b| b == b'\n').filter(|l| !l.is_empty());
    let queries: Vec<Query> = lines.map(parse).collect();
    let n_terms = terms.bytes.len();

    let docs = map_file(&format!("{data_dir}/docs.txt"));
    let df = estimate_df(docs, &terms, workers);
    // Renumber the terms: BOOL terms first, so a window's words cover them only (53,444 of 75,747 terms on seed 1),
    // each part by descending sampled df, so the terms most tokens hit have neighbouring ids and their per-term
    // entries and words share a few hundred KB of cache lines. Only speed depends on the order.
    let mut in_bool = vec![false; n_terms];
    bools.iter().flatten().filter(|&&w| w != NIL).for_each(|&w| in_bool[(w & TERM) as usize] = true);
    let n_bool = in_bool.iter().filter(|&&b| b).count();
    let key: Vec<u64> = (0..n_terms).map(|t| (!in_bool[t] as u64) << 32 | (!df[t]) as u64).collect();
    let new_id = terms.renumber(&key);
    let mut df = vec![0; n_terms];
    for (t, &d) in key.iter().enumerate() {
        df[new_id[t] as usize] = !d as u32;
    }
    for w in bools.iter_mut().flatten().filter(|w| **w != NIL) {
        *w = *w & !TERM | new_id[(*w & TERM) as usize];
    }
    for t in patterns.iter_mut().flatten() {
        *t = new_id[*t as usize];
    }
    // The two plans are independent: build them on two threads.
    let (plan, phrases) = std::thread::scope(|s| {
        let phrases = s.spawn(|| PhrasePlan::new(&patterns, n_terms, &df));
        let plan = BoolPlan::new(&bools, n_bool, &df);
        (plan, phrases.join().unwrap())
    });

    // The pass: one doc range per worker, cut at line starts.
    let cut = |k: usize| line_start(docs, docs.len() * k / workers);
    let shards = parallel(workers, |k| scan(&docs[cut(k)..cut(k + 1)], &terms, &phrases, &plan));

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
    let mut phrase_results = vec![(0, 0, Vec::new()); patterns.len()];
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
fn push_decimal(out: &mut Vec<u8>, n: u64) {
    if n >= 10 {
        push_decimal(out, n / 10);
    }
    out.push(b'0' + (n % 10) as u8);
}
