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
//! Main tricks: `ChunkLog` and `fold_chunk` (b-68), `hold` (b-64), `PhrasePlan` (b-8), `TermTable` (b-36a),
//! `estimate_df` (b-11), `Interner` (b-84).

use std::arch::x86_64::*;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, Ordering::{Acquire, Relaxed, Release}};
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

    /// Bit k: byte k is `b`.
    #[inline(always)]
    fn eq_mask(&self, b: u8) -> u32 {
        unsafe { _mm256_movemask_epi8(_mm256_cmpeq_epi8(self.0, _mm256_set1_epi8(b as i8))) as u32 }
    }

    /// Bit k: byte k ends the doc, an LF or the end of `data` (a last line without LF ends there).
    #[inline(always)]
    fn lf(&self, data: &[u8], i: usize) -> u32 {
        let lf = self.eq_mask(b'\n');
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
    // 1 = PROT_READ, 2 = MAP_PRIVATE (the Linux values), declared here so the standard library stays
    // the only dependency.
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

/// The hash of a term zero-padded to 32 bytes: CRC32-C (SSE4.2, part of x86-64-v3) of its two 16-byte halves xor-ed,
/// a word at a time. A fixed amount of work per token, with no loop whose exit depends on the token's length (b-31b).
/// The bytes decide every match, so the hash only spreads the slots. (b-74) Two `crc32` replaced four 64-bit
/// multiplies whose constants were reloaded at every token: the lookup's instructions fell by 0.66 G, its misses
/// and mispredicts unchanged (cachegrind, 1 worker, seed 1).
#[inline(always)]
fn hash32(t: &[u8; MAX_TERM]) -> u32 {
    let w = |k: usize| u64::from_le_bytes(t[8 * k..8 * k + 8].try_into().unwrap());
    unsafe { _mm_crc32_u64(_mm_crc32_u64(0, w(0) ^ w(2)), w(1) ^ w(3)) as u32 }
}

/// The query terms: interned into it while queries.txt is parsed, then probed for every token of the pass. A 4-byte
/// slot holds the hash's top 2 bits and id + 1, so a miss reads one cache line; a hash match is then confirmed
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

    /// The tag only filters; the bytes decide. Token and entry are both zero-padded to 32 bytes, so one 32-byte
    /// compare settles the bytes and the length (it replaced a memcmp call and a length test per hash match).
    /// (b-31c) box T 0.330 → 0.318 s. The probe starts at slot i = `slot(h)`, which the pass already computed for
    /// its prefetch (b-74).
    #[inline]
    fn find(&self, token: &[u8; MAX_TERM], h: u32, mut i: usize) -> Option<u32> {
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

    /// `walk` without the start offsets, for the df sample. (b-59)
    #[inline(always)]
    fn tokenize(&self, data: &[u8], i: usize, mut on_token: impl FnMut(Option<u32>)) -> usize {
        self.walk(data, i, |_, t| on_token(t))
    }

    /// Looks up the token held back by `hold`, if any, and reports it. (b-64)
    #[rustfmt::skip]
    #[inline(always)]
    fn settle(&self, pend: &mut Option<(usize, Option<([u8; MAX_TERM], u32, usize)>)>, f: &mut impl FnMut(usize, Option<u32>)) {
        if let Some((s, tok)) = pend.take() { f(s, tok.and_then(|(t, h, i)| self.find(&t, h, i))); }
    }

    /// Folds and hashes the token at from..to and prefetches its slot, then settles the token before it and holds this
    /// one back: the slot's read overlaps the previous token's probe and hit work. Box T 0.268 -> 0.251 s. (b-64)
    #[rustfmt::skip]
    #[inline(always)]
    fn hold(&self, data: &[u8], from: usize, to: usize, pend: &mut Option<(usize, Option<([u8; MAX_TERM], u32, usize)>)>, f: &mut impl FnMut(usize, Option<u32>)) {
        let tok = (to - from <= MAX_TERM).then(|| {
            let t = Chunk::load(data, from).folded(to - from);
            let h = hash32(&t);
            let i = slot(h, self.shift);
            unsafe { _mm_prefetch(self.slots.as_ptr().add(i) as *const i8, _MM_HINT_T0) };
            (t, h, i)
        });
        self.settle(pend, f);
        *pend = Some((from, tok));
    }

    /// `tokenize` with each token's start offset passed to `f` too: the phrase anchors need it (b-56). The text is
    /// classified 32 bytes at a time (`Chunk`), and tokens are cut where the token-byte mask changes, so no branch is
    /// taken per byte: the byte loop's two branches (separator skip, token end) were 71 M of the run's 161 M
    /// mispredicts (cachegrind, 1 worker, seed 1). (b-31a, b-31b, b-31c)
    /// Starts and ends are two masks, the i-th end closing the i-th start, so no branch decides per edge. (b-55)
    /// Each token is reported one token late, after the next one's slot is prefetched. (b-64)
    #[inline(always)]
    fn walk(&self, data: &[u8], mut i: usize, mut f: impl FnMut(usize, Option<u32>)) -> usize {
        let mut start = NIL as usize; // the token in progress, if any
        let mut pend = None;
        loop {
            let chunk = Chunk::load(data, i);
            let (lf, tok) = (chunk.lf(data, i), chunk.token_mask());
            let tok = tok & lf.wrapping_sub(1) & !lf; // only the bytes before the doc's LF
            let prev = tok << 1 | (start != NIL as usize) as u32; // bit k: byte i + k - 1 is a token byte
            let mut starts = tok & !prev; // bit k: a token starts at byte i + k
            let mut ends = !tok & prev; // bit k: a token ends just before byte i + k
            if start != NIL as usize && ends != 0 {
                // the token carried in from the previous chunk ends at the first end bit
                self.hold(data, start, i + ends.trailing_zeros() as usize, &mut pend, &mut f);
                ends &= ends - 1;
                start = NIL as usize;
            }
            while ends != 0 {
                // every remaining end closes the earliest open start in this chunk
                let (s, e) = (starts.trailing_zeros() as usize, ends.trailing_zeros() as usize);
                self.hold(data, i + s, i + e, &mut pend, &mut f);
                starts &= starts - 1;
                ends &= ends - 1;
            }
            if starts != 0 {
                start = i + starts.trailing_zeros() as usize; // the last token runs into the next chunk
            }
            // The LF is no token byte, so a token that runs up to it has ended in the loop above.
            if lf != 0 {
                self.settle(&mut pend, &mut f);
                return i + lf.trailing_zeros() as usize + 1;
            }
            i += 32;
        }
    }
}

/// The slot where the probe for hash `h` starts, in a table of 2^(32 - shift) slots.
#[inline]
fn slot(h: u32, shift: u32) -> usize {
    (h.wrapping_mul(0x9e37_79b1) >> shift) as usize
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
        Interner { slots, shift, bytes: table.bytes.as_mut_ptr(), cap, next: AtomicU32::new(0) }
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
                if self.slots[i].compare_exchange(0, h & !TERM | id + 1, Release, Relaxed).is_ok() {
                    ids.start += 1;
                    return id;
                }
                continue; // another worker took the slot: read it again
            }
            if (s ^ h) & !TERM == 0 && unsafe { &*self.bytes.add((s & TERM) as usize - 1) }.0 == padded {
                return (s & TERM) - 1;
            }
            i = (i + 1) & (self.slots.len() - 1);
        }
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

/// PHRASE matching by anchor checks (b-8). Each PHRASE line is one pattern and gets one anchor: its term with the
/// lowest estimated df (the df sample only picks the anchor; a term in no doc has df 0, so a phrase with an absent
/// term is never even checked). The pass records only a doc's anchor occurrences, each with its byte offset; at the
/// doc's end each one is a candidate for every pattern anchored there at offset j, confirmed by reading the
/// pattern's other terms off the doc's bytes around it (b-56).
/// It replaces an Aho-Corasick automaton whose edge probes, fail walks and output checks were the pass's largest
/// part (about 0.9-1.1 s of 4.0-4.3 s at 1 worker on seed 1).
struct PhrasePlan {
    terms: Vec<u32>,
    anchor_bits: Vec<u64>, // by term id: the term anchors at least one pattern (tested for every hit)
    anchor_start: Vec<u32>, // by term id, into anchors (CSR)
    anchors: Vec<(u32, u32, u32, u32)>, // (pattern, offset of the anchor in it, its terms' range in `terms`)
}

impl PhrasePlan {
    /// Pattern p's terms are `terms[ends[p - 1]..ends[p]]` (from 0 for p = 0).
    fn new(terms: Vec<u32>, ends: &[u32], n_terms: usize, df: &[u32]) -> Self {
        let mut anchor_bits = vec![0u64; n_terms.div_ceil(64)];
        let (mut anchors, mut lo) = (Vec::new(), 0);
        for (pi, &hi) in ends.iter().enumerate() {
            let p = &terms[lo as usize..hi as usize];
            let j = (0..p.len()).min_by_key(|&j| df[p[j] as usize]).unwrap();
            anchor_bits[p[j] as usize / 64] |= 1 << (p[j] % 64);
            anchors.push((p[j], (pi as u32, j as u32, lo, hi)));
            lo = hi;
        }
        let (anchor_start, anchors) = by_term(n_terms, &anchors);
        PhrasePlan { terms, anchor_bits, anchor_start, anchors }
    }

    /// Calls `f(p)` for each pattern with an anchor occurrence in `hits` (term, byte offset in `doc`) at its anchor
    /// offset and its other terms around it. `doc` is the doc's text without its LF.
    ///
    /// (b-56) Why the bytes decide what the doc's full token sequence did: a token is a maximal run of a-z, A-Z and
    /// 0-9 and every other byte separates (the TAB before `doc`, the LF after it, bytes from 0x80), so the
    /// non-empty pieces of doc[at..] split at separators are the doc's own tokens from the anchor on, and those
    /// of doc[..at] split from the right are the tokens before it in reverse; no phrase spans two docs. A term
    /// equals a token when its bytes equal the token's up to ASCII case and end there (the term table's own
    /// test). The per-token sequence push is gone: 36.3 M pushes on seed 1 for 1.1 M anchor hits.
    #[inline]
    fn check(&self, dt: &TermTable, doc: &[u8], hits: &[(usize, usize, u32)], mut f: impl FnMut(u32)) {
        let sep = |b: &u8| !b.is_ascii_alphanumeric();
        let is_term = |token: Option<&[u8]>, t: u32| {
            token.is_some_and(|tk| {
                let term = &dt.bytes[t as usize].0;
                tk.len() <= MAX_TERM && term[..tk.len()].eq_ignore_ascii_case(tk) && term.get(tk.len()).unwrap_or(&0) == &0
            })
        };
        for &(t, at, left) in hits {
            let (from, to) = (self.anchor_start[t] as usize, self.anchor_start[t + 1] as usize);
            for &(p, j, lo, hi) in &self.anchors[from..to] {
                let (before, after) = self.terms[lo as usize..hi as usize].split_at(j as usize);
                // b-98: the doc's token just before the anchor (its term id, NIL if none) must be the pattern's term
                // before it: one compare skips the pattern before any byte is read (box T 0.216 -> 0.211 s).
                if before.last().is_some_and(|&b| b != left) {
                    continue;
                }
                let mut fwd = doc[at..].split(sep).filter(|t| !t.is_empty());
                let mut bwd = doc[..at].rsplit(sep).filter(|t| !t.is_empty());
                if after.iter().all(|&t| is_term(fwd.next(), t)) && before.iter().rev().all(|&t| is_term(bwd.next(), t)) {
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

/// Returns the estimated df per term and the number of docs sampled.
fn estimate_df(docs: &[u8], dict: &TermTable, workers: usize) -> (Vec<u32>, u32) {
    let (n_terms, window) = (dict.bytes.len(), docs.len() / SAMPLE_SHARE / SAMPLE_WINDOWS);
    // The windows are spread over the workers (window k on worker k % n) and their counts summed: the sample was
    // 15 ms of serial prologue on the box. (b-11)
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
const OP_SHIFT: u32 = 30; // term ids stay below 2^30 (asserted in BoolPlan::new)
const TERM: u32 = (1 << OP_SHIFT) - 1;
const OP_AND: u32 = 0;
const OP_OR: u32 = 1;
const OP_NOT: u32 = 2;

/// Docs per chunk: a doc's place in its chunk takes the low LOCAL_BITS of a log entry, and a chunk's row is
/// CHUNK / 64 words. (b-68)
const LOCAL_BITS: u32 = 10;
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

/// The estimated number of docs holding the terms, summed.
fn df_sum(terms: &[u32], df: &[u32]) -> u64 {
    terms.iter().map(|&t| df[t as usize] as u64).sum()
}

/// Trigger terms of a query, written to `cover` (reused, so a query allocates nothing): every doc in R contains one
/// of them. {first} holds for R = S(first); OR adds its operand; AND narrows R to S(t), so it may replace the cover by
/// {t} when t is estimated rarer; NOT keeps it.
fn cover(q: &Code, df: &[u32], cover: &mut Vec<u32>) {
    cover.clear();
    cover.push(q[0]);
    for &w in q[1..].iter().take_while(|&&w| w != NIL) {
        let t = w & TERM;
        match w >> OP_SHIFT {
            OP_OR => cover.push(t),
            OP_AND if df_sum(&[t], df) < df_sum(cover, df) => *cover = vec![t],
            _ => {}
        }
    }
    cover.sort_unstable();
    cover.dedup();
}

impl BoolPlan {
    fn new(queries: &[Code], n_dense: usize, n_bool: usize, df: &[u32]) -> Self {
        assert!(n_bool < 1 << OP_SHIFT && n_bool - n_dense < 1 << (32 - LOCAL_BITS));
        let (mut every_chunk, mut triggers, mut terms) = (Vec::new(), Vec::new(), Vec::new());
        for (qi, q) in queries.iter().enumerate() {
            cover(q, df, &mut terms);
            let trig = Trigger { query: qi as u32, code: *q };
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
        BoolPlan { n_dense, n_bool, every_chunk, trigger_start, triggers, slot, ids }
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
        r.iter_mut().zip(b).for_each(|(r, b)| *r = *r & (b ^ x | y) | b & o);
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
    rows: Vec<u64>,  // ROW words per dense term, and a dummy row that every sparse hit ORs into instead
    log: Vec<u32>,   // log[..n] is this chunk's; the entry at n is written by every hit and kept by a sparse one
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

    /// Records that doc `id` of this chunk contains BOOL term t: no branch on dense or sparse.
    #[inline(always)]
    fn hit(&mut self, n_dense: usize, t: usize, id: u32) {
        let local = id - self.base;
        self.rows[t.min(n_dense) * ROW + local as usize / 64] |= 1 << (local % 64);
        let key = (t + 1).saturating_sub(n_dense) as u32;
        if self.n == self.log.len() {
            self.grow();
        }
        self.log[self.n] = key << LOCAL_BITS | local;
        self.n += (key != 0) as usize;
    }

    /// Doubles the log, only while the first chunks size it. Out of line, so the hit stays small enough to be inlined
    /// into the token walk: inline, the resize made the hit a call per token. (b-68) The pass's instructions fell by
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
            start = if e >> LOCAL_BITS != prev >> LOCAL_BITS { m } else { start };
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
            let mut words = r.iter().enumerate().fold(0u32, |m, (j, &w)| m | ((w != 0) as u32) << j);
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
            plan.triggers[from as usize..to as usize].iter().for_each(&mut fold);
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

fn scan(data: &[u8], dict: &TermTable, phrases: &PhrasePlan, plan: &BoolPlan) -> Shard {
    let (mut acc, mut matches) = (vec![BoolAcc::default(); plan.slot.len()], Vec::new());
    // Ending a chunk with no hits adds nothing, so the first chunk needs no special case.
    let mut chunk = ChunkLog::new(plan);
    // The doc's anchor occurrences as (term, byte offset, the term before it); per pattern the last doc id + 1 it was
    // reported for.
    let (mut hits, mut pattern_last) = (Vec::new(), vec![0u32; phrases.anchors.len()]);
    let mut i = 0;
    while i < data.len() {
        let (id, text) = parse_id(data, i);
        // Ids are dense and ascending through docs.txt (specs/b-index.md), so a chunk, once left, never returns.
        if id / CHUNK * CHUNK != chunk.base {
            chunk.end(plan, &mut acc);
            chunk.base = id / CHUNK * CHUNK;
        }
        let mut left = NIL; // the previous token's term id in this doc (b-98)
        i = dict.walk(data, text, |start, hit| {
            let before = std::mem::replace(&mut left, hit.unwrap_or(NIL));
            let Some(t) = hit else { return };
            if phrases.anchor_bits[t as usize / 64] >> (t % 64) & 1 != 0 {
                hits.push((t as usize, start - text, before));
            }
            if (t as usize) < plan.n_bool {
                chunk.hit(plan.n_dense, t as usize, id);
            }
        });
        // `i - 1` is the doc's LF, so the check reads this doc's text only; clamped for out-of-spec TAB-less lines.
        let end = (i - 1).min(data.len());
        phrases.check(dict, &data[text.min(end)..end], &hits, |p| {
            if pattern_last[p as usize] != id + 1 {
                pattern_last[p as usize] = id + 1;
                matches.push((p, id));
            }
        });
        hits.clear();
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
    // `<=`: a chunk at the end itself marks the end (`lf`) when the length is a multiple of 32.
    for i in (0..=text.len()).step_by(32) {
        let chunk = Chunk::load(text, i);
        let lf = chunk.lf(text, i);
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
                        let op = (pair[0] == b"OR") as u32 * OP_OR + (pair[0] == b"NOT") as u32 * OP_NOT;
                        *w = op << OP_SHIFT | intern(pair[1]);
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
    let (mut terms, parts) = loop {
        let bytes = Vec::with_capacity(n_slots / 3);
        let mut table = TermTable { slots: vec![0; n_slots], shift: 32 - n_slots.trailing_zeros(), bytes };
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
    let (mut queries, mut bools, mut phrase_terms, mut phrase_ends) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
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

    let docs = map_file(&format!("{data_dir}/docs.txt"));
    let (df, n_sample) = estimate_df(docs, &terms, workers);
    // Renumber the terms: BOOL terms first, so a chunk's rows and log keys cover them only (53,444 of 75,747 terms
    // on seed 1), each part by descending sampled df, so the terms most tokens hit have neighbouring ids and their
    // per-term entries and rows share a few hundred KB of cache lines. Only speed depends on the order.
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
    for t in &mut phrase_terms {
        *t = new_id[*t as usize];
    }
    // The two plans are independent: build them on two threads.
    let (plan, phrases) = std::thread::scope(|s| {
        let phrases = s.spawn(|| PhrasePlan::new(phrase_terms, &phrase_ends, n_terms, &df));
        // Dense: sampled in at least 1/64 of the sampled docs, so a chunk holds about CHUNK / 64 = ROW or more of
        // its docs and a row of ROW words is no larger than its log entries would be. BOOL ids are in descending
        // df, so the dense terms are the first n_dense. (b-68)
    let n_dense = (0..n_bool).take_while(|&t| df[t] > 0 && df[t] as u64 * 64 >= n_sample as u64).count();
    let plan = BoolPlan::new(&bools, n_dense, n_bool, &df);
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
