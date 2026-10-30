//! Track B: answer the BOOL and PHRASE queries of /data/queries.txt over /data/docs.txt (spec: specs/b-index.md).
//!
//! Queries are read first, so only their terms are looked up. One parallel pass over the memory-mapped corpus
//! tokenizes every document and decides every BOOL query (per document, or 64 documents at a time) and every
//! PHRASE pattern (by anchor checks) for it; nothing is indexed.

use std::collections::HashMap;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Mutex;

const NIL: u32 = u32::MAX;
const MAX_TERM: usize = 32; // spec 2.3: a query term is 1 to 32 bytes, so a longer token is never a query term
const FIRST_IDS: usize = 10; // spec 4: the ten smallest ids

/// spec 2.2: ASCII-lowercase, and a token is a run of a-z / 0-9; this maps a byte to its lower-case token byte, 0 if separator.
const FOLD: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut c = 0;
    while c < 256 {
        let b = (c as u8).to_ascii_lowercase();
        t[c] = b * b.is_ascii_alphanumeric() as u8;
        c += 1;
    }
    t
};

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

/// FNV-1a, computed byte by byte while a token is scanned, so the lookup needs no second pass over the token.
#[inline]
fn fnv_step(h: u32, c: u8) -> u32 {
    (h ^ c as u32).wrapping_mul(0x0100_0193)
}
const FNV_OFFSET: u32 = 0x811c_9dc5;

/// The query terms: interned into it while queries.txt is parsed, then probed for every token of the pass. A slot
/// holds the term's hash and id + 1 in one u64, so a miss reads one cache line; a hash match is then confirmed
/// against the term's bytes, stored inline and zero-padded to 32 B (one more line). b-4 probed slots sized from
/// every term occurrence (4 MB), a separate hash array, and a slice into queries.txt per term.
struct TermTable {
    slots: Vec<u64>, // hash << 32 | (term id + 1), 0 = empty
    shift: u32,
    bytes: Vec<TermBytes>, // by term id
}

#[repr(align(32))] // one entry never straddles two cache lines
#[derive(Clone, Copy)]
struct TermBytes([u8; MAX_TERM]);

impl TermTable {
    /// The id of `term` (spec 2.3: 1 to 32 bytes), added if new. The slots double whenever they would pass load 2/3,
    /// rehashed from the hash each slot keeps: 1 MB of slots on seed 1. Measured against load 1/2 (2 MB) and 1/4
    /// (4 MB): the smallest table was fastest, since misses cost more than probe steps.
    fn intern(&mut self, term: &[u8]) -> u32 {
        let h = term.iter().fold(FNV_OFFSET, |h, &c| fnv_step(h, c));
        if let Some(t) = self.find(term, h) {
            return t;
        }
        if 3 * (self.bytes.len() + 1) > 2 * self.slots.len() {
            let size = self.slots.len() * 2;
            let old = std::mem::replace(&mut self.slots, vec![0; size]);
            self.shift -= 1;
            old.into_iter().for_each(|s| self.place(s)); // an empty slot placed again stays empty
        }
        let mut entry = TermBytes([0; MAX_TERM]);
        entry.0[..term.len()].copy_from_slice(term);
        self.bytes.push(entry);
        self.place((h as u64) << 32 | self.bytes.len() as u64);
        self.bytes.len() as u32 - 1
    }

    fn place(&mut self, s: u64) {
        let mut i = self.slot((s >> 32) as u32);
        while self.slots[i] != 0 {
            i = (i + 1) & (self.slots.len() - 1);
        }
        self.slots[i] = s;
    }

    /// Renumbers the terms by descending `df` in place: the slots keep their hash and get the new id, and the
    /// entries move (cheaper than rebuilding the table after the df sample). Returns each term's new id.
    fn renumber(&mut self, df: &[u32]) -> Vec<u32> {
        let mut order: Vec<usize> = (0..df.len()).collect();
        order.sort_by_key(|&t| std::cmp::Reverse(df[t]));
        let mut new_id = vec![0; df.len()];
        for (new, &old) in order.iter().enumerate() {
            new_id[old] = new as u32;
        }
        for s in self.slots.iter_mut().filter(|s| **s != 0) {
            *s = (*s >> 32) << 32 | (new_id[(*s as u32 - 1) as usize] as u64 + 1);
        }
        self.bytes = order.iter().map(|&t| self.bytes[t]).collect();
        new_id
    }

    #[inline]
    fn slot(&self, h: u32) -> usize {
        (h.wrapping_mul(0x9e37_79b1) >> self.shift) as usize
    }

    /// The hash only filters; the bytes decide. Terms hold no zero byte, so `e[n] == 0` means the term is n long.
    #[inline]
    fn find(&self, token: &[u8], h: u32) -> Option<u32> {
        let mut i = self.slot(h);
        loop {
            let s = self.slots[i];
            if s == 0 {
                return None;
            }
            if (s >> 32) as u32 == h {
                let t = (s as u32 - 1) as usize;
                let (e, n) = (&self.bytes[t].0, token.len());
                if e[..n] == *token && (n == MAX_TERM || e[n] == 0) {
                    return Some(t as u32);
                }
            }
            i = (i + 1) & (self.slots.len() - 1);
        }
    }

    /// Tokenizes the doc text from `i` to its LF (spec 2.2), calling `on_token` with the term id of each token that is
    /// a query term and None for any other token; returns the index just past the LF.
    #[inline(always)]
    fn tokenize(&self, data: &[u8], mut i: usize, mut on_token: impl FnMut(Option<u32>)) -> usize {
        let end = data.len();
        let mut token = [0u8; MAX_TERM];
        while i < end && data[i] != b'\n' {
            let mut c = FOLD[data[i] as usize];
            if c == 0 {
                i += 1;
                continue;
            }
            let mut h = FNV_OFFSET;
            let mut n = 0;
            while c != 0 {
                if n < MAX_TERM {
                    token[n] = c;
                }
                n += 1;
                h = fnv_step(h, c);
                i += 1;
                c = if i < end { FOLD[data[i] as usize] } else { 0 };
            }
            on_token(if n <= MAX_TERM { self.find(&token[..n], h) } else { None });
        }
        i + 1
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

/// PHRASE matching by anchor checks. Each distinct phrase is one pattern and gets one anchor: its term with the
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

/// The start of the first line that starts at or after `from` (spec 2.1: every line ends with LF).
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
/// lines from SAMPLE_WINDOWS evenly spaced windows; also returns how many docs it read. It only chooses an evaluation
/// order (each query's trigger terms, and which queries take the window path; BoolPlan), so it cannot change an
/// answer: a poor estimate can only slow the pass down. It is not a second pass.
const SAMPLE_WINDOWS: usize = 64;
const SAMPLE_SHARE: usize = 128;

fn estimate_df(docs: &[u8], dict: &TermTable, workers: usize) -> (Vec<u32>, u32) {
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
        (df, stamp)
    });
    let mut df = vec![0u32; n_terms];
    for (part, _) in &parts {
        df.iter_mut().zip(part).for_each(|(d, p)| *d += p);
    }
    (df, parts.iter().map(|p| p.1).sum())
}

/// BOOL queries evaluated per document inside the pass, so no posting list is ever stored (b-2 kept 47.6 MB of
/// blocks and 7.3 MB of heads for 29.2 MB of varints at 12 workers). Under the strict left fold (spec 3) a doc is
/// in R exactly when the same fold over the booleans "the doc contains t" is true, so a query is decided per doc
/// from which of its terms the doc contains. A query is only evaluated for the docs that contain one of its
/// trigger terms, a cover such that every doc in R contains one of them (`cover`); every query has one, so the
/// path is exact for every shape, NOT forms and absent terms included.
const MAX_BOOL_TERMS: usize = 6; // spec 2.3: a BOOL query has 1 to 6 terms
/// A query's code: its first term, then op << OP_SHIFT | term per operator, NIL-padded.
type Code = [u32; MAX_BOOL_TERMS];
const OP_SHIFT: u32 = 30; // term ids stay below 2^30 (asserted in BoolPlan::new)
const TERM: u32 = (1 << OP_SHIFT) - 1;
const OP_AND: u32 = 0;
const OP_OR: u32 = 1;
const OP_NOT: u32 = 2;
const MULTI: u32 = 1 << 31; // Trigger::query flag: the query has several triggers, so one doc can reach it twice
const ALWAYS: u32 = 1 << 30; // Trigger::query flag: single term or OR only; 44 M of the 85 M seed-1 matches need no fold
const FLAGS: u32 = MULTI | ALWAYS;
/// A query whose cover is estimated to hold at least one doc in WINDOW_SHARE takes the window path (`fold_window`).
/// Per doc, it would cost an evaluation, a count and a sum for each doc of its cover. Per window, it costs one
/// word-wide fold of its terms' window bits. 1,042 seed-1 queries with |R| > N/16 held 66 M of the 85 M matches.
const WINDOW_SHARE: u64 = 64;

/// One query listed under one of its trigger terms, with its code inline so a doc's evaluations read the
/// trigger list front to back instead of jumping to each query.
#[derive(Clone, Copy, Default)]
struct Trigger {
    query: u32,
    code: Code,
}

struct BoolPlan {
    // Per term, read once per doc and term: its index among the window path's terms (NIL if none) and the range
    // in `triggers` of the queries it triggers, so a term that triggers no query skips the per-doc trigger loop.
    // Measured: the per-doc BOOL layer of the pass fell from 0.57-1.33 s to 0.47-0.59 s (1 worker, seed 1).
    roles: Vec<(u32, u32, u32)>,
    triggers: Vec<Trigger>,
    // The window path: per window query its code over window-term indices, and its query.
    n_window_terms: usize,
    window_code: Vec<Code>,
    window_query: Vec<u32>,
    ids: FirstIds,
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
    fn new(queries: &[Code], n_terms: usize, df: &[u32], sampled_docs: u32) -> Self {
        assert!(n_terms < 1 << OP_SHIFT);
        let (mut triggers, mut window_code, mut window_query) = (Vec::new(), Vec::new(), Vec::new());
        let (mut window_term, mut n_window_terms) = (vec![NIL; n_terms], 0);
        for (qi, q) in queries.iter().enumerate() {
            let cover = cover(q, df);
            if sampled_docs > 0 && df_sum(&cover, df) * WINDOW_SHARE >= sampled_docs as u64 {
                let mut code = *q;
                for w in code.iter_mut().filter(|w| **w != NIL) {
                    let t = (*w & TERM) as usize;
                    if window_term[t] == NIL {
                        (window_term[t], n_window_terms) = (n_window_terms as u32, n_window_terms + 1);
                    }
                    *w = *w & !TERM | window_term[t];
                }
                window_code.push(code);
                window_query.push(qi as u32);
                continue;
            }
            // A query of OR operators only has all its terms as triggers, so being triggered is a match.
            let or_only = q[1..].iter().all(|&w| w == NIL || w >> OP_SHIFT == OP_OR);
            let query = qi as u32 | or_only as u32 * ALWAYS | (cover.len() > 1) as u32 * MULTI;
            let trig = Trigger { query, code: *q };
            triggers.extend(cover.iter().map(|&t| (t, trig)));
        }
        let (start, triggers) = by_term(n_terms, &triggers);
        let roles = (0..n_terms).map(|t| (window_term[t], start[t], start[t + 1])).collect();
        let ids = FirstIds::new(queries.len());
        BoolPlan { roles, triggers, n_window_terms, window_code, window_query, ids }
    }
}

/// The same strict left fold as `holds`, over 64 docs at once: bit j of a window term's word says doc base + j
/// contains it. AND, OR and NOT only combine bits that some term set, so docs missing from the range stay 0.
#[inline]
fn fold_window(code: &[u32; MAX_BOOL_TERMS], words: &[u64]) -> u64 {
    let mut r = words[code[0] as usize];
    for &w in &code[1..] {
        if w == NIL {
            break;
        }
        let b = words[(w & ((1 << OP_SHIFT) - 1)) as usize];
        r = match w >> OP_SHIFT {
            OP_AND => r & b,
            OP_OR => r | b,
            _ => r & !b,
        };
    }
    r
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

/// Folds every window query over the window of docs base..base + 64 and clears the window bits.
fn flush_window(plan: &BoolPlan, words: &mut [u64], base: u32, acc: &mut [BoolAcc]) {
    for (code, &q) in plan.window_code.iter().zip(&plan.window_query) {
        let r = fold_window(code, words);
        if r == 0 {
            continue;
        }
        let q = q as usize;
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
    words.fill(0);
}

/// spec 3: strict left fold, no precedence; `a OR b AND c` is `(a OR b) AND c` (b-index.md:136-141).
/// (A branch-free fold over all MAX_BOOL_TERMS words measured slower here: most queries have 2 or 3 terms.)
#[inline]
fn holds(code: &[u32; MAX_BOOL_TERMS], present: &[u64]) -> bool {
    let has = |t: u32| present[t as usize / 64] >> (t % 64) & 1 != 0;
    let mut r = has(code[0]);
    for &w in &code[1..] {
        if w == NIL {
            break;
        }
        let t = w & ((1 << OP_SHIFT) - 1);
        r = match w >> OP_SHIFT {
            OP_AND => r && has(t),
            OP_OR => r || has(t),
            _ => r && !has(t),
        };
    }
    r
}

/// Per worker and BOOL query: the evaluation stamp (read only for MULTI queries) and the running count and sum.
#[derive(Clone, Copy, Default)]
struct BoolAcc {
    done: u32, // last doc id + 1 this query was evaluated for
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
    let (mut acc, mut matches) = (vec![BoolAcc::default(); plan.ids.bound.len()], Vec::new());
    // The window path's term bits for the current 64-doc window (doc ids id / 64 equal). Flushing a window of no
    // bits changes nothing, so the first window needs no special case.
    let (mut words, mut window) = (vec![0u64; plan.n_window_terms], 0);
    // The doc's query terms as a bitset (a few KB, so evaluations test it in L1), cleared after each doc from `hits`.
    let mut present = vec![0u64; dict.bytes.len().div_ceil(64)];
    // This doc's distinct query terms, their non-empty trigger ranges, its tokens as term ids (NIL for any other
    // token) and the positions in `seq` of anchor terms; per pattern the last doc id + 1 it was reported for.
    let (mut hits, mut triggered, mut seq, mut anchored) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut pattern_last = vec![0u32; phrases.anchors.len()];
    let mut i = 0;
    while i < data.len() {
        let (id, text) = parse_id(data, i);
        let stamp = id + 1;
        if id / 64 != window {
            flush_window(plan, &mut words, window * 64, &mut acc);
            window = id / 64;
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
            let (w, bit) = (t as usize / 64, 1u64 << (t % 64));
            if present[w] & bit == 0 {
                present[w] |= bit;
                hits.push(t);
                let (window_term, from, to) = plan.roles[t as usize];
                if to > from {
                    triggered.push((from, to));
                }
                if window_term != NIL {
                    words[window_term as usize] |= 1 << (id % 64);
                }
            }
        });
        // A phrase never spans two documents: seq holds this doc only.
        phrases.check(&seq, &anchored, |p| {
            if pattern_last[p as usize] != stamp {
                pattern_last[p as usize] = stamp;
                matches.push((p, id));
            }
        });
        seq.clear();
        anchored.clear();
        // The doc's term set is complete: evaluate each query one of its terms triggers, MULTI ones once.
        for &(from, to) in &triggered {
            for trig in &plan.triggers[from as usize..to as usize] {
                let q = (trig.query & !FLAGS) as usize;
                if trig.query & MULTI != 0 {
                    if acc[q].done == stamp {
                        continue;
                    }
                    acc[q].done = stamp;
                }
                if trig.query & ALWAYS != 0 || holds(&trig.code, &present) {
                    let acc = &mut acc[q];
                    acc.count += 1;
                    acc.sum += id as u64;
                    plan.ids.offer(q, id);
                }
            }
        }
        for &t in &hits {
            present[t as usize / 64] &= !(1 << (t % 64));
        }
        hits.clear();
        triggered.clear();
    }
    flush_window(plan, &mut words, window * 64, &mut acc);
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

    // Queries: intern every term, dedupe BOOL lines and PHRASE term sequences. Under spec 2.3's grammar (single
    // spaces, lower-case terms) two PHRASE lines are equal exactly when their term sequences are, so whole lines are
    // the dedupe key for both kinds (b-9 also hashed a cloned Vec<u32> per phrase).
    let mut terms = TermTable { slots: vec![0; 2], shift: 31, bytes: Vec::new() };
    let mut distinct: HashMap<&[u8], Query> = HashMap::new();
    let (mut bools, mut patterns): (Vec<Code>, Vec<Vec<u32>>) = (Vec::new(), Vec::new());
    let mut parse = |line: &[u8]| {
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
    let queries: Vec<Query> = lines.map(|l| *distinct.entry(l).or_insert_with(|| parse(l))).collect();
    drop(distinct);
    let n_terms = terms.bytes.len();

    let docs = map_file(&format!("{data_dir}/docs.txt"));
    let (mut df, sampled_docs) = estimate_df(docs, &terms, workers);
    // Renumber the terms by descending sampled df, so the terms most tokens hit have neighbouring ids and their
    // per-term entries, roles and bits share a few hundred KB of cache lines. Only speed depends on the order.
    let new_id = terms.renumber(&df);
    df.sort_unstable_by(|a, b| b.cmp(a));
    for w in bools.iter_mut().flatten().filter(|w| **w != NIL) {
        *w = *w & !TERM | new_id[(*w & TERM) as usize];
    }
    for t in patterns.iter_mut().flatten() {
        *t = new_id[*t as usize];
    }
    // The two plans are independent: build them on two threads.
    let (plan, phrases) = std::thread::scope(|s| {
        let phrases = s.spawn(|| PhrasePlan::new(&patterns, n_terms, &df));
        let plan = BoolPlan::new(&bools, n_terms, &df, sampled_docs);
        (plan, phrases.join().unwrap())
    });

    // The pass: one doc range per worker, cut at line starts.
    let cut = |k: usize| line_start(docs, docs.len() * k / workers);
    let shards = parallel(workers, |k| scan(&docs[cut(k)..cut(k + 1)], &terms, &phrases, &plan));

    // An answer is count, exact id sum and the smallest ids (spec 4).
    let mut bool_results = vec![(0, 0, Vec::new()); bools.len()];
    for (r, kept) in bool_results.iter_mut().zip(plan.ids.kept) {
        r.2 = kept.into_inner().unwrap();
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

    let mut out = Vec::with_capacity(queries.len() * 32);
    for q in queries {
        let (count, sum, first) = match q {
            Query::Bool(b) => &bool_results[b],
            Query::Phrase(p) => &phrase_results[p],
        };
        let first = first.iter().map(|&id| id as u64);
        for n in [*count, *sum].into_iter().chain(first) {
            push_decimal(&mut out, n);
            out.push(b' ');
        }
        *out.last_mut().unwrap() = b'\n';
    }
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
