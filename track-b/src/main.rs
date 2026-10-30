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
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Posting storage of one worker: fixed-size slabs cut into blocks whose size grows by 4x per class, so a list
/// is a chain of blocks and nothing is ever copied to grow it. Blocks hold LEB128 varints of doc-id gaps.
const SLAB_BITS: u32 = 20;
const SLAB: usize = 1 << SLAB_BITS;
const BLOCK_HEADER: usize = 8; // next block (u32), bytes used (u16), size class (u8), pad
const CLASS_SIZE: [usize; 5] = [16, 64, 256, 1024, 4096];

struct Arena {
    slabs: Vec<Box<[u8]>>,
    used: usize,
}

impl Arena {
    fn block(&self, pos: u32) -> &[u8] {
        &self.slabs[(pos >> SLAB_BITS) as usize][(pos as usize) & (SLAB - 1)..]
    }

    fn block_mut(&mut self, pos: u32) -> &mut [u8] {
        &mut self.slabs[(pos >> SLAB_BITS) as usize][(pos as usize) & (SLAB - 1)..]
    }

    fn new_block(&mut self, class: usize) -> u32 {
        let size = CLASS_SIZE[class];
        if self.slabs.is_empty() || self.used + size > SLAB {
            self.slabs.push(vec![0u8; SLAB].into_boxed_slice());
            self.used = 0;
        }
        let pos = ((self.slabs.len() - 1) << SLAB_BITS | self.used) as u32;
        self.used += size;
        let b = self.block_mut(pos);
        b[0..4].copy_from_slice(&NIL.to_le_bytes());
        b[4..6].copy_from_slice(&0u16.to_le_bytes());
        b[6] = class as u8;
        pos
    }
}

#[derive(Clone, Copy)]
struct List {
    head: u32,
    tail: u32,
    last: u32, // last doc id + 1, 0 = none yet
}

/// One worker's pass over its doc range: postings of every query term and the phrase matches.
struct Shard {
    arena: Arena,
    lists: Vec<List>,
    matches: Vec<(u32, u32)>, // (pattern, doc), docs ascending
    docs_end: u32,            // one past the largest doc id seen
}

impl Shard {
    #[inline]
    fn push(&mut self, t: u32, doc: u32) {
        let l = &mut self.lists[t as usize];
        if l.last == doc + 1 {
            return; // documents are counted, not occurrences
        }
        let mut gap = doc + 1 - l.last;
        l.last = doc + 1;
        let mut buf = [0u8; 5];
        let mut n = 0;
        while gap >= 0x80 {
            buf[n] = gap as u8 | 0x80;
            gap >>= 7;
            n += 1;
        }
        buf[n] = gap as u8;
        n += 1;

        if l.tail == NIL {
            let b = self.arena.new_block(0);
            l.head = b;
            l.tail = b;
        } else {
            let tail = self.arena.block(l.tail);
            let used = u16::from_le_bytes([tail[4], tail[5]]) as usize;
            let class = tail[6] as usize;
            if BLOCK_HEADER + used + n > CLASS_SIZE[class] {
                // A varint never straddles two blocks, so a block decodes on its own.
                let b = self.arena.new_block((class + 1).min(CLASS_SIZE.len() - 1));
                self.arena.block_mut(l.tail)[0..4].copy_from_slice(&b.to_le_bytes());
                l.tail = b;
            }
        }
        let tail = self.arena.block_mut(l.tail);
        let used = u16::from_le_bytes([tail[4], tail[5]]) as usize;
        tail[BLOCK_HEADER + used..BLOCK_HEADER + used + n].copy_from_slice(&buf[..n]);
        tail[4..6].copy_from_slice(&((used + n) as u16).to_le_bytes());
    }

    fn for_each_doc(&self, t: u32, mut f: impl FnMut(u32)) {
        let mut pos = self.lists[t as usize].head;
        let mut last = 0u32;
        while pos != NIL {
            let b = self.arena.block(pos);
            let used = u16::from_le_bytes([b[4], b[5]]) as usize;
            let mut i = BLOCK_HEADER;
            while i < BLOCK_HEADER + used {
                let mut v = 0u32;
                let mut shift = 0;
                loop {
                    let c = b[i];
                    i += 1;
                    v |= ((c & 0x7f) as u32) << shift;
                    if c < 0x80 {
                        break;
                    }
                    shift += 7;
                }
                last += v;
                f(last - 1);
            }
            pos = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        }
    }
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

fn scan(data: &[u8], dict: &Dict, ac: &Automaton, n_patterns: usize) -> Shard {
    let mut shard = Shard {
        arena: Arena { slabs: Vec::new(), used: 0 },
        lists: vec![List { head: NIL, tail: NIL, last: 0 }; dict.terms.len()],
        matches: Vec::new(),
        docs_end: 0,
    };
    let mut pattern_last = vec![0u32; n_patterns]; // last doc id + 1 a pattern was reported for
    let end = data.len();
    let mut i = 0;
    let mut token = [0u8; MAX_TERM];
    while i < end {
        // spec 2.1: a line is the decimal doc id, a TAB, the text and an LF.
        let mut id = 0u32;
        while i < end && data[i] != b'\t' {
            id = id * 10 + (data[i] - b'0') as u32;
            i += 1;
        }
        i += 1;
        shard.docs_end = id + 1;
        let mut state = 0u32; // a phrase never spans two documents
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
            let Some(t) = (if n <= MAX_TERM { dict.find(&token[..n], h) } else { None }) else {
                state = 0;
                continue;
            };
            shard.push(t, id);
            state = ac.step(state, t);
            let mut o = if ac.out[state as usize] != NIL { state } else { ac.dict_link[state as usize] };
            while o != NIL {
                let p = ac.out[o as usize];
                if pattern_last[p as usize] != id + 1 {
                    pattern_last[p as usize] = id + 1;
                    shard.matches.push((p, id));
                }
                o = ac.dict_link[o as usize];
            }
        }
        i += 1;
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

    fn from_bitmap(bits: &[u64]) -> Self {
        let mut a = Answer::default();
        for (w, &word) in bits.iter().enumerate() {
            let mut word = word;
            while word != 0 {
                a.add((w * 64) as u32 + word.trailing_zeros());
                word &= word - 1;
            }
        }
        a
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

struct Index {
    shards: Vec<Shard>,
    bitmaps: Vec<Option<Vec<u64>>>, // N-bit bitmap for each term with df > N/8, where it is no larger than varints
    words: usize,
}

impl Index {
    fn for_each_doc(&self, t: u32, mut f: impl FnMut(u32)) {
        for shard in &self.shards {
            shard.for_each_doc(t, &mut f);
        }
    }

    fn load(&self, t: u32, dst: &mut [u64]) {
        if let Some(bm) = &self.bitmaps[t as usize] {
            dst.copy_from_slice(bm);
        } else {
            dst.fill(0);
            self.for_each_doc(t, |d| dst[d as usize / 64] |= 1 << (d % 64));
        }
    }

    /// spec 3: strict left fold, no precedence; `a OR b AND c` is `(a OR b) AND c` (b-index.md:136-141).
    fn eval(&self, q: &BoolQuery, acc: &mut [u64], tmp: &mut [u64]) -> Answer {
        self.load(q.first, acc);
        for &(op, t) in &q.rest {
            let bm = self.bitmaps[t as usize].as_deref();
            match (op, bm) {
                (Op::Or, Some(b)) => acc.iter_mut().zip(b).for_each(|(a, b)| *a |= b),
                (Op::Not, Some(b)) => acc.iter_mut().zip(b).for_each(|(a, b)| *a &= !b),
                (Op::And, Some(b)) => acc.iter_mut().zip(b).for_each(|(a, b)| *a &= b),
                // OR and NOT with a sparse list touch only its docs; AND needs the list as a bitmap.
                (Op::Or, None) => self.for_each_doc(t, |d| acc[d as usize / 64] |= 1 << (d % 64)),
                (Op::Not, None) => self.for_each_doc(t, |d| acc[d as usize / 64] &= !(1 << (d % 64))),
                (Op::And, None) => {
                    self.load(t, tmp);
                    acc.iter_mut().zip(tmp.iter()).for_each(|(a, b)| *a &= b);
                }
            }
        }
        Answer::from_bitmap(acc)
    }
}

/// Runs `work(i)` for i in 0..n over the worker threads, handing out indices in chunks.
fn parallel_for<R: Send>(n: usize, workers: usize, chunk: usize, init: impl Fn() -> R + Sync, work: impl Fn(&mut R, usize) + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                s.spawn(|| {
                    let mut state = init();
                    loop {
                        let start = next.fetch_add(chunk, Ordering::Relaxed);
                        if start >= n {
                            break;
                        }
                        for i in start..(start + chunk).min(n) {
                            work(&mut state, i);
                        }
                    }
                    state
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
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

    // The pass: one doc range per worker, cut at line starts.
    let docs = map_file(&format!("{data_dir}/docs.txt"));
    let shards: Vec<Shard> = std::thread::scope(|s| {
        let handles: Vec<_> = split_lines(docs, workers)
            .into_iter()
            .map(|part| s.spawn(|| scan(part, &dict, &ac, patterns.len())))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let n_docs = shards.iter().map(|s| s.docs_end).max().unwrap_or(0) as usize;
    lap("pass");

    // Terms with df > N/8 get an N-bit bitmap: their varint list is about as large, and bitmaps fold fastest.
    let mut index = Index { shards, bitmaps: Vec::new(), words: n_docs.div_ceil(64) };
    let n_terms = dict.terms.len();
    let found = parallel_for(n_terms, workers, 256, Vec::new, |found: &mut Vec<(u32, Vec<u64>)>, t| {
        let mut df = 0;
        index.for_each_doc(t as u32, |_| df += 1);
        if df > n_docs / 8 {
            let mut bm = vec![0u64; index.words];
            index.for_each_doc(t as u32, |d| bm[d as usize / 64] |= 1 << (d % 64));
            found.push((t as u32, bm));
        }
    });
    index.bitmaps = vec![None; n_terms];
    for (t, bm) in found.into_iter().flatten() {
        index.bitmaps[t as usize] = Some(bm);
    }
    lap("encode");

    let words = index.words;
    let bool_answers = parallel_for(
        bools.len(),
        workers,
        16,
        || (vec![0u64; words], vec![0u64; words], Vec::new()),
        |(acc, tmp, done): &mut (Vec<u64>, Vec<u64>, Vec<(usize, Answer)>), i| done.push((i, index.eval(&bools[i], acc, tmp))),
    );
    let mut bool_results = vec![Answer::default(); bools.len()];
    for (i, a) in bool_answers.into_iter().flat_map(|(_, _, done)| done) {
        bool_results[i] = a;
    }
    lap("bool");

    // Shards are in doc order and each lists its matches in doc order, so every pattern's docs arrive ascending.
    let mut phrase_results = vec![Answer::default(); patterns.len()];
    for shard in &index.shards {
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
