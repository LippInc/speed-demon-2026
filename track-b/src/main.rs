//! Track B: answer the BOOL and PHRASE queries of /data/queries.txt over /data/docs.txt (spec: specs/b-index.md).
//!
//! Queries are read first, so only their terms are indexed. One parallel pass over the memory-mapped corpus
//! tokenizes every document, appends the doc to the postings of each query term it contains, and runs an
//! Aho-Corasick automaton over the query-term ids that reports every PHRASE pattern ending at each token.
//! BOOL queries are then folded left to right over bitmaps; PHRASE answers are the automaton's per-doc matches.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

const NIL: u32 = u32::MAX;
const MAX_TERM: usize = 32; // spec 2.3: a query term is 1 to 32 bytes, so a longer token is never a query term
const FIRST_IDS: usize = 10; // spec 4: the ten smallest ids

/// spec 2.2: ASCII-lowercase, and a token is a run of a-z / 0-9; this maps a byte to its lower-case token byte, 0 if separator.
const FOLD: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut c = 0;
    while c < 256 {
        let b = c as u8;
        t[c] = match b {
            b'a'..=b'z' | b'0'..=b'9' => b,
            b'A'..=b'Z' => b + 32,
            _ => 0,
        };
        c += 1;
    }
    t
};

/// Number of worker threads. SD_WORKERS is the rule-30 test-only override (the hash runs force 1, 4, 12 and 24
/// workers); the board never sets it, so the scored run sizes itself from available_parallelism.
fn worker_count() -> usize {
    if let Some(n) = std::env::var("SD_WORKERS").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&n| n > 0) {
        return n;
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, offset: i64) -> *mut u8;
}

/// Maps a file read-only. The corpus is never copied into the heap: its pages stay in the page cache, off our M.
fn map_file(path: &str) -> &'static [u8] {
    const PROT_READ: i32 = 1;
    const MAP_PRIVATE: i32 = 2;
    let file = File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let len = file.metadata().expect("stat").len() as usize;
    if len == 0 {
        return &[];
    }
    let ptr = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0) };
    assert!(ptr as isize != -1, "mmap {path} failed");
    // The mapping outlives the descriptor and is never unmapped, so the slice is valid for the whole run.
    unsafe { std::slice::from_raw_parts(ptr, len) }
}

/// FNV-1a, computed byte by byte while a token is scanned, so the lookup needs no second pass over the token.
#[inline]
fn fnv_step(h: u32, c: u8) -> u32 {
    (h ^ c as u32).wrapping_mul(0x0100_0193)
}
const FNV_OFFSET: u32 = 0x811c_9dc5;

/// Open-addressing table of the query terms; a hit compares the full bytes.
struct Dict<'q> {
    terms: Vec<&'q [u8]>,
    hashes: Vec<u32>,
    slots: Vec<u32>, // term id + 1, 0 = empty
    shift: u32,
}

impl<'q> Dict<'q> {
    fn with_capacity(max_terms: usize) -> Self {
        let size = (2 * max_terms).next_power_of_two().max(2); // load factor at most 1/2
        Dict { terms: Vec::new(), hashes: Vec::new(), slots: vec![0; size], shift: 32 - size.trailing_zeros() }
    }

    #[inline]
    fn slot(&self, h: u32) -> usize {
        (h.wrapping_mul(0x9e37_79b1) >> self.shift) as usize
    }

    #[inline]
    fn find(&self, token: &[u8], h: u32) -> Option<u32> {
        let mask = self.slots.len() - 1;
        let mut i = self.slot(h);
        loop {
            let s = self.slots[i];
            if s == 0 {
                return None;
            }
            let t = (s - 1) as usize;
            if self.hashes[t] == h && self.terms[t] == token {
                return Some(t as u32);
            }
            i = (i + 1) & mask;
        }
    }

    fn intern(&mut self, term: &'q [u8]) -> u32 {
        let h = term.iter().fold(FNV_OFFSET, |h, &c| fnv_step(h, c));
        if let Some(t) = self.find(term, h) {
            return t;
        }
        let mask = self.slots.len() - 1;
        let mut i = self.slot(h);
        while self.slots[i] != 0 {
            i = (i + 1) & mask;
        }
        self.terms.push(term);
        self.hashes.push(h);
        self.slots[i] = self.terms.len() as u32;
        self.terms.len() as u32 - 1
    }
}

/// Aho-Corasick automaton over query-term ids. Each distinct phrase is one pattern; a state's dictionary-suffix
/// link points to the longest proper suffix state that ends a pattern, so a pattern nested inside another
/// (`w w` inside `w w w`, or one ending inside a longer match) is still reported.
struct Automaton {
    root_next: Vec<u32>,  // dense goto from the root, by term id (0 = stay at root)
    in_phrase: Vec<bool>, // a term in no phrase always sends the automaton back to the root
    edge_keys: Vec<u64>,  // open addressing over (state << 32 | term) for the non-root edges
    edge_vals: Vec<u32>,
    edge_shift: u32,
    fail: Vec<u32>,
    out: Vec<u32>,       // pattern ending at this state, or NIL
    dict_link: Vec<u32>, // nearest proper suffix state with an output, or NIL
}

impl Automaton {
    fn new(patterns: &[Vec<u32>], n_terms: usize) -> Self {
        let mut root_next = vec![0u32; n_terms];
        let mut in_phrase = vec![false; n_terms];
        let mut children: Vec<Vec<(u32, u32)>> = vec![Vec::new()];
        let mut out = vec![NIL];
        for (p, pattern) in patterns.iter().enumerate() {
            let mut s = 0u32;
            for &a in pattern {
                in_phrase[a as usize] = true;
                let existing = if s == 0 {
                    Some(root_next[a as usize]).filter(|&n| n != 0)
                } else {
                    children[s as usize].iter().find(|&&(b, _)| b == a).map(|&(_, n)| n)
                };
                s = existing.unwrap_or_else(|| {
                    let n = children.len() as u32;
                    children.push(Vec::new());
                    out.push(NIL);
                    children[s as usize].push((a, n));
                    if s == 0 {
                        root_next[a as usize] = n;
                    }
                    n
                });
            }
            out[s as usize] = p as u32;
        }

        let n_edges: usize = children.iter().skip(1).map(Vec::len).sum();
        let size = (2 * n_edges).next_power_of_two().max(2);
        let mut ac = Automaton {
            root_next,
            in_phrase,
            edge_keys: vec![u64::MAX; size],
            edge_vals: vec![0; size],
            edge_shift: 64 - size.trailing_zeros(),
            fail: vec![0; children.len()],
            out,
            dict_link: vec![NIL; children.len()],
        };
        for (s, edges) in children.iter().enumerate().skip(1) {
            for &(a, n) in edges {
                let key = (s as u64) << 32 | a as u64;
                let mut i = ac.edge_slot(key);
                while ac.edge_keys[i] != u64::MAX {
                    i = (i + 1) & (size - 1);
                }
                ac.edge_keys[i] = key;
                ac.edge_vals[i] = n;
            }
        }

        // Breadth-first, so every shallower state's fail and dictionary links are final before they are used.
        let mut queue: Vec<u32> = children[0].iter().map(|&(_, n)| n).collect();
        let mut head = 0;
        while head < queue.len() {
            let u = queue[head];
            head += 1;
            for &(a, v) in &children[u as usize] {
                let f = if u == 0 { 0 } else { ac.step(ac.fail[u as usize], a) };
                ac.fail[v as usize] = f;
                ac.dict_link[v as usize] = if ac.out[f as usize] != NIL { f } else { ac.dict_link[f as usize] };
                queue.push(v);
            }
        }
        ac
    }

    #[inline]
    fn edge_slot(&self, key: u64) -> usize {
        (key.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> self.edge_shift) as usize
    }

    #[inline]
    fn edge(&self, s: u32, a: u32) -> Option<u32> {
        let key = (s as u64) << 32 | a as u64;
        let mask = self.edge_keys.len() - 1;
        let mut i = self.edge_slot(key);
        loop {
            let k = self.edge_keys[i];
            if k == key {
                return Some(self.edge_vals[i]);
            }
            if k == u64::MAX {
                return None;
            }
            i = (i + 1) & mask;
        }
    }

    #[inline]
    fn step(&self, mut s: u32, a: u32) -> u32 {
        if !self.in_phrase[a as usize] {
            return 0;
        }
        while s != 0 {
            if let Some(n) = self.edge(s, a) {
                return n;
            }
            s = self.fail[s as usize];
        }
        self.root_next[a as usize]
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

/// Tokenizes the doc text from `i` to its LF (spec 2.2), calling `on_token` with the term id of each token that is
/// a query term and None for any other token; returns the index just past the LF.
#[inline(always)]
fn tokenize_doc(data: &[u8], mut i: usize, dict: &Dict, mut on_token: impl FnMut(Option<u32>)) -> usize {
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
        on_token(if n <= MAX_TERM { dict.find(&token[..n], h) } else { None });
    }
    i + 1
}

/// Document frequency of the query terms, estimated on about 1/SAMPLE_SHARE of the corpus (about 1 %) read as whole
/// lines from SAMPLE_WINDOWS evenly spaced windows; also returns how many docs it read. It only chooses an evaluation
/// order (each query's trigger terms, and which queries take the window path; BoolPlan), so it cannot change an
/// answer: a poor estimate can only slow the pass down. It is not a second pass.
const SAMPLE_WINDOWS: usize = 64;
const SAMPLE_SHARE: usize = 128;

fn estimate_df(docs: &[u8], dict: &Dict) -> (Vec<u32>, u32) {
    let n_terms = dict.terms.len();
    let mut df = vec![0u32; n_terms];
    let mut seen = vec![0u32; n_terms];
    let mut stamp = 0u32;
    let window = docs.len() / SAMPLE_SHARE / SAMPLE_WINDOWS;
    for k in 0..SAMPLE_WINDOWS {
        let from = docs.len() * k / SAMPLE_WINDOWS;
        let mut i = match docs[from..].iter().position(|&b| b == b'\n') {
            _ if from == 0 => 0,
            Some(p) => from + p + 1,
            None => continue,
        };
        let stop = (i + window).min(docs.len());
        while i < stop {
            stamp += 1;
            i = tokenize_doc(docs, parse_id(docs, i).1, dict, |t| {
                if let Some(t) = t {
                    if seen[t as usize] != stamp {
                        seen[t as usize] = stamp;
                        df[t as usize] += 1;
                    }
                }
            });
        }
    }
    (df, stamp)
}

/// BOOL queries evaluated per document inside the pass, so no posting list is ever stored (b-2 kept 47.6 MB of
/// blocks and 7.3 MB of heads for 29.2 MB of varints at 12 workers). Under the strict left fold (spec 3) a doc is
/// in R exactly when the same fold over the booleans "the doc contains t" is true, so a query is decided per doc
/// from which of its terms the doc contains. A query is only evaluated for the docs that contain one of its
/// trigger terms, a cover such that every doc in R contains one of them (`cover`); every query has one, so the
/// path is exact for every shape, NOT forms and absent terms included.
const MAX_BOOL_TERMS: usize = 6; // spec 2.3: a BOOL query has 1 to 6 terms
const OP_SHIFT: u32 = 30; // a code word is op << 30 | term id; term ids stay below 2^30 (asserted in BoolPlan::new)
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
#[derive(Clone, Copy)]
struct Trigger {
    query: u32,
    code: [u32; MAX_BOOL_TERMS], // first term, then op << OP_SHIFT | term per operator, NIL-padded
}

struct BoolPlan {
    query_of_slot: Vec<u32>, // accumulator slot -> distinct BOOL query
    trigger_start: Vec<u32>, // per term, into triggers (CSR, sized from the query terms)
    triggers: Vec<Trigger>,
    // The window path: per term its index among the window terms (NIL if none); per window query its code over
    // window-term indices (same layout as Trigger::code) and its accumulator slot.
    window_term: Vec<u32>,
    n_window_terms: usize,
    window_code: Vec<[u32; MAX_BOOL_TERMS]>,
    window_slot: Vec<u32>,
}

/// Trigger terms of a query: every doc in R contains one of them. {first} holds for R = S(first); OR adds its
/// operand; AND narrows R to S(t), so it may replace the cover by {t} when t is estimated rarer; NOT keeps it.
fn cover(q: &BoolQuery, df: &[u32]) -> Vec<u32> {
    let mut cover = vec![q.first];
    for &(op, t) in &q.rest {
        match op {
            Op::Or => cover.push(t),
            Op::And if (df[t as usize] as u64) < cover.iter().map(|&c| df[c as usize] as u64).sum() => cover = vec![t],
            Op::And | Op::Not => {}
        }
    }
    cover.sort_unstable();
    cover.dedup();
    cover
}

impl BoolPlan {
    fn new(queries: &[BoolQuery], n_terms: usize, df: &[u32], sampled_docs: u32) -> Self {
        assert!(n_terms < 1 << OP_SHIFT);
        let covers: Vec<Vec<u32>> = queries.iter().map(|q| cover(q, df)).collect();
        let windowed: Vec<bool> = covers
            .iter()
            .map(|c| sampled_docs > 0 && c.iter().map(|&t| df[t as usize] as u64).sum::<u64>() * WINDOW_SHARE >= sampled_docs as u64)
            .collect();
        let mut trigger_start = vec![0u32; n_terms + 1];
        for (cover, _) in covers.iter().zip(&windowed).filter(|(_, &w)| !w) {
            for &t in cover {
                trigger_start[t as usize + 1] += 1;
            }
        }
        for t in 0..n_terms {
            trigger_start[t + 1] += trigger_start[t];
        }
        let mut fill = trigger_start.clone();
        let mut triggers = vec![Trigger { query: 0, code: [NIL; MAX_BOOL_TERMS] }; trigger_start[n_terms] as usize];
        let mut window_term = vec![NIL; n_terms];
        let mut n_window_terms = 0;
        let mut window_code = Vec::new();
        let mut window_queries = Vec::new();
        for (qi, (q, cover)) in queries.iter().zip(&covers).enumerate() {
            if windowed[qi] {
                let mut index = |t: u32| {
                    if window_term[t as usize] == NIL {
                        window_term[t as usize] = n_window_terms as u32;
                        n_window_terms += 1;
                    }
                    window_term[t as usize]
                };
                window_code.push(compile(q, &mut index));
                window_queries.push(qi);
                continue;
            }
            let code = compile(q, &mut |t| t);
            // A query of OR operators only has all its terms as triggers, so being triggered is a match.
            let always = if q.rest.iter().all(|&(op, _)| matches!(op, Op::Or)) { ALWAYS } else { 0 };
            let query = qi as u32 | always | if cover.len() > 1 { MULTI } else { 0 };
            for &t in cover {
                triggers[fill[t as usize] as usize] = Trigger { query, code };
                fill[t as usize] += 1;
            }
        }
        // Renumber the queries in trigger-list order, so the queries one common term triggers have neighbouring
        // accumulators: a doc's evaluations then update a few cache lines instead of one line per query.
        let mut slot_of = vec![NIL; queries.len()];
        let mut query_of_slot = Vec::with_capacity(queries.len());
        for trig in &mut triggers {
            let q = (trig.query & !FLAGS) as usize;
            if slot_of[q] == NIL {
                slot_of[q] = query_of_slot.len() as u32;
                query_of_slot.push(q as u32);
            }
            trig.query = slot_of[q] | (trig.query & FLAGS);
        }
        let window_slot = window_queries
            .iter()
            .map(|&q| {
                query_of_slot.push(q as u32);
                query_of_slot.len() as u32 - 1
            })
            .collect();
        BoolPlan { query_of_slot, trigger_start, triggers, window_term, n_window_terms, window_code, window_slot }
    }

    #[inline]
    fn triggered_by(&self, t: u32) -> &[Trigger] {
        &self.triggers[self.trigger_start[t as usize] as usize..self.trigger_start[t as usize + 1] as usize]
    }
}

/// A query's code: its first term, then op << OP_SHIFT | term per operator, NIL-padded; `term` maps each term id.
fn compile(q: &BoolQuery, term: &mut impl FnMut(u32) -> u32) -> [u32; MAX_BOOL_TERMS] {
    let mut code = [NIL; MAX_BOOL_TERMS];
    code[0] = term(q.first);
    for (w, &(op, t)) in code[1..].iter_mut().zip(&q.rest) {
        let op = match op {
            Op::And => OP_AND,
            Op::Or => OP_OR,
            Op::Not => OP_NOT,
        };
        *w = op << OP_SHIFT | term(t);
    }
    code
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
fn flush_window(plan: &BoolPlan, words: &mut [u64], base: u32, acc: &mut [BoolAcc], first_ids: &FirstIds) {
    for (code, &slot) in plan.window_code.iter().zip(&plan.window_slot) {
        let r = fold_window(code, words);
        if r == 0 {
            continue;
        }
        let slot = slot as usize;
        let n = r.count_ones();
        let offsets: u32 = BIT_SLICES.iter().enumerate().map(|(k, &m)| (r & m).count_ones() << k).sum();
        acc[slot].count += n;
        acc[slot].sum += base as u64 * n as u64 + offsets as u64;
        // First ids only while this window can still hold one: the bound is NIL until ten are kept.
        let mut bits = r;
        while bits != 0 && base + bits.trailing_zeros() < first_ids.bound[slot].load(Ordering::Relaxed) {
            first_ids.offer(slot, base + bits.trailing_zeros());
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
/// 1.42 M (slot, doc) entries (12.6 MB at 12 workers): every worker kept ten for each common query. `bound` is the
/// largest id kept once ten are kept (NIL before), so a later, larger id is rejected by one relaxed load and only a
/// candidate takes the lock; a stale bound only sends a few more ids to the lock, which checks again.
struct FirstIds {
    bound: Vec<AtomicU32>,
    kept: Vec<Mutex<Vec<u32>>>,
}

impl FirstIds {
    fn new(n: usize) -> Self {
        FirstIds { bound: (0..n).map(|_| AtomicU32::new(NIL)).collect(), kept: (0..n).map(|_| Mutex::new(Vec::new())).collect() }
    }

    #[inline]
    fn offer(&self, slot: usize, id: u32) {
        if id >= self.bound[slot].load(Ordering::Relaxed) {
            return;
        }
        let mut kept = self.kept[slot].lock().unwrap();
        let at = kept.partition_point(|&k| k < id);
        if at < FIRST_IDS {
            kept.insert(at, id);
            kept.truncate(FIRST_IDS);
            if kept.len() == FIRST_IDS {
                self.bound[slot].store(kept[FIRST_IDS - 1], Ordering::Relaxed);
            }
        }
    }
}

/// One worker's pass over its doc range: per-query BOOL count, sum and first ids, and the phrase matches.
struct Shard {
    bool_acc: Vec<BoolAcc>,
    matches: Vec<(u32, u32)>,    // (pattern, doc), docs ascending
    evaluations: u64,            // (doc, query) evaluations, reported by SD_PHASES
}

/// Splits the corpus into `n` ranges that each start at the beginning of a line.
fn split_lines(data: &[u8], n: usize) -> Vec<&[u8]> {
    let mut cuts = vec![0];
    for k in 1..n {
        let from = (data.len() * k / n).max(*cuts.last().unwrap());
        let cut = match data[from..].iter().position(|&b| b == b'\n') {
            Some(p) if from > 0 => from + p + 1,
            _ if from == 0 => 0,
            _ => data.len(),
        };
        cuts.push(cut);
    }
    cuts.push(data.len());
    cuts.windows(2).map(|w| &data[w[0]..w[1]]).collect()
}

fn scan(data: &[u8], dict: &Dict, ac: &Automaton, n_patterns: usize, plan: &BoolPlan, first_ids: &FirstIds) -> Shard {
    let mut shard = Shard {
        bool_acc: vec![BoolAcc::default(); plan.query_of_slot.len()],
        matches: Vec::new(),
        evaluations: 0,
    };
    // The window path's term bits for the current 64-doc window (doc ids id / 64 equal).
    let mut window_words = vec![0u64; plan.n_window_terms];
    let mut window = NIL;
    // The doc's query terms as a bitset (a few KB, so evaluations test it in L1), cleared after each doc from `hits`.
    let mut present = vec![0u64; dict.terms.len().div_ceil(64)];
    let mut hits: Vec<u32> = Vec::new(); // this doc's distinct query terms
    let mut pattern_last = vec![0u32; n_patterns]; // last doc id + 1 a pattern was reported for
    let mut i = 0;
    while i < data.len() {
        let (id, text) = parse_id(data, i);
        let stamp = id + 1;
        if id / 64 != window {
            if window != NIL {
                flush_window(plan, &mut window_words, window * 64, &mut shard.bool_acc, first_ids);
            }
            window = id / 64;
        }
        let mut state = 0u32; // a phrase never spans two documents
        i = tokenize_doc(data, text, dict, |hit| {
            let Some(t) = hit else {
                state = 0;
                return;
            };
            let (w, bit) = (t as usize / 64, 1u64 << (t % 64));
            if present[w] & bit == 0 {
                present[w] |= bit;
                hits.push(t);
                let wt = plan.window_term[t as usize];
                if wt != NIL {
                    window_words[wt as usize] |= 1 << (id % 64);
                }
            }
            state = ac.step(state, t);
            let mut o = if ac.out[state as usize] != NIL { state } else { ac.dict_link[state as usize] };
            while o != NIL {
                let p = ac.out[o as usize];
                if pattern_last[p as usize] != stamp {
                    pattern_last[p as usize] = stamp;
                    shard.matches.push((p, id));
                }
                o = ac.dict_link[o as usize];
            }
        });
        // The doc's term set is complete: evaluate each query one of its terms triggers, MULTI ones once.
        for &t in &hits {
            for trig in plan.triggered_by(t) {
                let q = (trig.query & !FLAGS) as usize;
                if trig.query & MULTI != 0 {
                    if shard.bool_acc[q].done == stamp {
                        continue;
                    }
                    shard.bool_acc[q].done = stamp;
                }
                shard.evaluations += 1;
                if trig.query & ALWAYS != 0 || holds(&trig.code, &present) {
                    let acc = &mut shard.bool_acc[q];
                    acc.count += 1;
                    acc.sum += id as u64;
                    first_ids.offer(q, id);
                }
            }
        }
        for &t in &hits {
            present[t as usize / 64] &= !(1 << (t % 64));
        }
        hits.clear();
    }
    if window != NIL {
        flush_window(plan, &mut window_words, window * 64, &mut shard.bool_acc, first_ids);
    }
    shard
}

/// count, exact id sum and the smallest ids of one answer (spec 4).
#[derive(Clone, Default)]
struct Answer {
    count: u64,
    sum: u64,
    first: Vec<u32>,
}

impl Answer {
    fn add(&mut self, id: u32) {
        self.count += 1;
        self.sum += id as u64;
        if self.first.len() < FIRST_IDS {
            self.first.push(id);
        }
    }
}

#[derive(Clone, Copy)]
enum Op {
    And,
    Or,
    Not,
}

struct BoolQuery {
    first: u32,
    rest: Vec<(Op, u32)>,
}

enum Query {
    Bool(usize),
    Phrase(usize),
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data_dir = args.get(1).map_or("/data", String::as_str);
    let out_dir = args.get(2).map_or("/out", String::as_str);
    // SD_PHASES prints a phase split to stderr for local profiling; the scored run leaves it unset.
    let phases = std::env::var_os("SD_PHASES").is_some();
    let clock = Instant::now();
    let lap = |name: &str| {
        if phases {
            eprintln!("{name:>8} {:8.3} s", clock.elapsed().as_secs_f64());
        }
    };
    let workers = worker_count();

    // Queries: intern every term, dedupe BOOL lines and PHRASE term sequences.
    let qtext = map_file(&format!("{data_dir}/queries.txt"));
    let lines: Vec<&[u8]> = qtext.split(|&b| b == b'\n').filter(|l| !l.is_empty()).collect();
    let mut dict = Dict::with_capacity(qtext.iter().filter(|&&b| b == b' ').count() + lines.len());
    let mut bools: Vec<BoolQuery> = Vec::new();
    let mut bool_ids: HashMap<&[u8], usize> = HashMap::new();
    let mut patterns: Vec<Vec<u32>> = Vec::new();
    let mut pattern_ids: HashMap<Vec<u32>, usize> = HashMap::new();
    let mut queries = Vec::with_capacity(lines.len());
    for &line in &lines {
        if let Some(body) = line.strip_prefix(b"PHRASE \"") {
            let terms: Vec<u32> = body[..body.len() - 1].split(|&b| b == b' ').map(|t| dict.intern(t)).collect();
            let next = patterns.len();
            let p = *pattern_ids.entry(terms.clone()).or_insert(next);
            if p == next {
                patterns.push(terms);
            }
            queries.push(Query::Phrase(p));
        } else {
            let next = bools.len();
            let b = *bool_ids.entry(line).or_insert(next);
            if b == next {
                // spec 2.3: "BOOL" term *(op term), single spaces.
                let mut parts = line[5..].split(|&b| b == b' ');
                let first = dict.intern(parts.next().unwrap());
                let mut rest = Vec::new();
                while let (Some(op), Some(t)) = (parts.next(), parts.next()) {
                    let op = match op {
                        b"AND" => Op::And,
                        b"OR" => Op::Or,
                        _ => Op::Not,
                    };
                    rest.push((op, dict.intern(t)));
                }
                bools.push(BoolQuery { first, rest });
            }
            queries.push(Query::Bool(b));
        }
    }
    let ac = Automaton::new(&patterns, dict.terms.len());
    lap("queries");

    let docs = map_file(&format!("{data_dir}/docs.txt"));
    let (df, sampled_docs) = estimate_df(docs, &dict);
    let plan = BoolPlan::new(&bools, dict.terms.len(), &df, sampled_docs);
    lap("sample");

    // The pass: one doc range per worker, cut at line starts.
    let first_ids = FirstIds::new(plan.query_of_slot.len());
    let shards: Vec<Shard> = std::thread::scope(|s| {
        let handles: Vec<_> = split_lines(docs, workers)
            .into_iter()
            .map(|part| s.spawn(|| scan(part, &dict, &ac, patterns.len(), &plan, &first_ids)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    if phases {
        let evaluations: u64 = shards.iter().map(|s| s.evaluations).sum();
        eprintln!("(doc, query) BOOL evaluations: {evaluations}; window queries: {}", plan.window_code.len());
    }
    lap("pass");

    let mut bool_results = vec![Answer::default(); bools.len()];
    for shard in &shards {
        for (&q, acc) in plan.query_of_slot.iter().zip(&shard.bool_acc) {
            bool_results[q as usize].count += acc.count as u64;
            bool_results[q as usize].sum += acc.sum;
        }
    }
    for (&q, kept) in plan.query_of_slot.iter().zip(first_ids.kept) {
        bool_results[q as usize].first = kept.into_inner().unwrap();
    }
    lap("bool");

    // Shards are in doc order and each lists its matches in doc order, so every pattern's docs arrive ascending.
    let mut phrase_results = vec![Answer::default(); patterns.len()];
    for shard in &shards {
        for &(p, d) in &shard.matches {
            phrase_results[p as usize].add(d);
        }
    }
    lap("phrase");

    let mut out = String::with_capacity(lines.len() * 32);
    for q in &queries {
        let a = match *q {
            Query::Bool(b) => &bool_results[b],
            Query::Phrase(p) => &phrase_results[p],
        };
        write!(out, "{} {}", a.count, a.sum).unwrap();
        for id in &a.first {
            write!(out, " {id}").unwrap();
        }
        out.push('\n');
    }
    std::fs::write(format!("{out_dir}/results.txt"), out).expect("write results.txt");
    lap("write");
}
