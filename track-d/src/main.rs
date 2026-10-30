//! Track D (Traverse): shortest directed path lengths for every query in /data/queries.txt.
//!
//! Build: two parallel passes over the mmapped graph.txt go straight into a forward and a reverse
//! CSR (pass 1 counts degrees, pass 2 places each edge at an atomic cursor), with no edge buffer
//! and no copy of the input. Queries: cheap answers first (s == t, a target with no in-edges, a
//! source with no out-edges), then a bidirectional Dijkstra per query on a worker thread, with
//! per-thread scratch sized by the search, not by n.

use std::ffi::c_void;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Instant;

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut c_void;
}
const PROT_READ: i32 = 1;
const MAP_PRIVATE: i32 = 2;

/// "Unreachable": no distance reaches it, since every answer is below 2^63 (spec section 3).
const INF: u64 = u64::MAX;

/// Maps a whole file read-only. The page cache backs it, so the bytes are never copied.
fn map_file(path: &str) -> &'static [u8] {
    let file = File::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let len = file.metadata().expect("stat").len() as usize;
    if len == 0 {
        return &[];
    }
    let ptr = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ, MAP_PRIVATE, file.as_raw_fd(), 0) };
    assert!(ptr as isize != -1, "mmap {path} failed");
    // The mapping outlives the fd and is never unmapped: it lives until the process exits.
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len) }
}

/// Worker count: available_parallelism. SD_WORKERS is rule 30's test-only override (the hash runs
/// force 1, 4, 12 and 24 workers); the board never sets it.
fn workers() -> usize {
    match std::env::var("SD_WORKERS") {
        Ok(s) => s.trim().parse::<usize>().ok().filter(|&n| n > 0).expect("SD_WORKERS must be a positive integer"),
        Err(_) => std::thread::available_parallelism().map_or(1, |n| n.get()),
    }
}

/// Reads one unsigned decimal ending in a space or LF (both sort below '0') and steps past the separator.
#[inline(always)]
fn read_num(b: &[u8], i: &mut usize) -> u64 {
    let mut x = 0u64;
    loop {
        let c = b[*i];
        *i += 1;
        if c < b'0' {
            return x;
        }
        x = x * 10 + (c - b'0') as u64;
    }
}

/// Splits b[body..] into `parts` byte ranges whose starts each sit just after an LF.
fn cut_at_lf(b: &[u8], body: usize, parts: usize) -> Vec<usize> {
    let len = b.len() - body;
    let mut cuts = vec![body];
    for k in 1..parts {
        let mut p = (body + len * k / parts).max(*cuts.last().unwrap());
        while p < b.len() && b[p - 1] != b'\n' {
            p += 1;
        }
        cuts.push(p);
    }
    cuts.push(b.len());
    cuts
}

/// Runs `f` on each byte range on its own thread.
fn each_range(cuts: &[usize], f: impl Fn(usize, usize) + Sync) {
    std::thread::scope(|s| {
        for r in cuts.windows(2) {
            let f = &f;
            s.spawn(move || f(r[0], r[1]));
        }
    });
}

/// A shared output array that threads fill at disjoint indices (each index comes from one fetch_add).
struct Slots(*mut u64);
unsafe impl Sync for Slots {}

/// Compressed adjacency: the edges of node u are edges[start(u)..end[u]], each packed as
/// (weight << 32) | head.
struct Csr {
    end: Vec<u32>,
    edges: Vec<u64>,
}

impl Csr {
    #[inline(always)]
    fn start(&self, u: usize) -> usize {
        if u == 0 { 0 } else { self.end[u - 1] as usize }
    }
    #[inline(always)]
    fn degree(&self, u: usize) -> u32 {
        self.end[u] - self.start(u) as u32
    }
    #[inline(always)]
    fn adj(&self, u: usize) -> &[u64] {
        &self.edges[self.start(u)..self.end[u] as usize]
    }
}

/// Turns per-node degree counts into each node's first edge slot, in place; returns the edge total.
fn degrees_to_cursors(deg: &[AtomicU32]) -> usize {
    let mut sum = 0usize;
    for d in deg {
        let k = d.load(Ordering::Relaxed) as usize;
        d.store(sum as u32, Ordering::Relaxed);
        sum += k;
    }
    assert!(sum <= u32::MAX as usize, "edge count {sum} does not fit the u32 CSR offsets");
    sum
}

/// Pass 1 counts degrees, pass 2 writes every edge into both CSRs at its node's atomic cursor.
/// After pass 2 each cursor has advanced to its node's end, which is exactly what Csr::end holds.
fn build_graph(graph: &[u8], body: usize, n: usize, threads: usize, t: &mut Vec<(&'static str, Instant)>) -> (Csr, Csr) {
    let cuts = cut_at_lf(graph, body, threads);
    let out_cur: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let in_cur: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    each_range(&cuts, |start, end| {
        let mut i = start;
        while i < end {
            let u = read_num(graph, &mut i) as usize;
            let v = read_num(graph, &mut i) as usize;
            while graph[i] != b'\n' {
                i += 1;
            }
            i += 1;
            out_cur[u].fetch_add(1, Ordering::Relaxed);
            in_cur[v].fetch_add(1, Ordering::Relaxed);
        }
    });
    t.push(("pass1", Instant::now()));

    let m = degrees_to_cursors(&out_cur);
    degrees_to_cursors(&in_cur);
    let mut fwd = vec![0u64; m];
    let mut rev = vec![0u64; m];
    let (fwd_out, rev_out) = (Slots(fwd.as_mut_ptr()), Slots(rev.as_mut_ptr()));
    each_range(&cuts, |start, end| {
        let (fwd_out, rev_out) = (&fwd_out, &rev_out);
        let mut i = start;
        while i < end {
            let u = read_num(graph, &mut i);
            let v = read_num(graph, &mut i);
            let w = read_num(graph, &mut i); // at most 4,294,967,295 (spec section 2): fits the high half
            let f = out_cur[u as usize].fetch_add(1, Ordering::Relaxed) as usize;
            let r = in_cur[v as usize].fetch_add(1, Ordering::Relaxed) as usize;
            // SAFETY: f and r come from fetch_add on cursors that stay inside u's and v's slot ranges,
            // so no two writes share an index and every index is below m.
            unsafe {
                *fwd_out.0.add(f) = w << 32 | v;
                *rev_out.0.add(r) = w << 32 | u;
            }
        }
    });
    t.push(("pass2", Instant::now()));
    let ends = |c: Vec<AtomicU32>| c.into_iter().map(AtomicU32::into_inner).collect();
    (Csr { end: ends(out_cur), edges: fwd }, Csr { end: ends(in_cur), edges: rev })
}

/// A heap entry: 12 bytes packed instead of the 16 a (u64, u32) tuple takes.
#[derive(Clone, Copy)]
#[repr(C, packed)]
struct Entry {
    key: u64,
    node: u32,
}

/// Monotone radix heap (Ahuja, Mehlhorn, Orlin, Tarjan) over u64 keys. Entry k sits in bucket
/// 64 - lzcnt(k ^ last), where last is the most recent minimum taken: bucket 0 holds keys equal to
/// last, bucket b keys whose highest bit differing from last is b - 1, so 65 buckets cover the key
/// width. Valid because Dijkstra only pushes keys >= the key it just popped. An entry moves only to
/// lower buckets, at most 64 times, instead of paying log(len) compares per pop like a binary heap.
/// Measured (local, full seed 1, interleaved with the floor's BinaryHeap): queries phase 1.09-1.10 s
/// -> 0.85-0.87 s at 12 workers, 1.00-1.08 s -> 0.87-0.95 s at 4.
struct RadixHeap {
    buckets: [Vec<Entry>; 65],
    last: u64,
    len: usize,
}

impl RadixHeap {
    fn new() -> Self {
        RadixHeap { buckets: std::array::from_fn(|_| Vec::new()), last: 0, len: 0 }
    }

    fn clear(&mut self) {
        self.buckets.iter_mut().for_each(Vec::clear);
        self.last = 0;
        self.len = 0;
    }

    fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    fn bucket(&self, key: u64) -> usize {
        64 - (key ^ self.last).leading_zeros() as usize
    }

    #[inline(always)]
    fn push(&mut self, key: u64, node: u32) {
        debug_assert!(key >= self.last, "radix heap keys must not go below the last minimum");
        let b = self.bucket(key);
        self.buckets[b].push(Entry { key, node });
        self.len += 1;
    }

    /// The exact minimum key, the same value BinaryHeap::peek gives. When bucket 0 is empty this
    /// advances last to the minimum of the lowest non-empty bucket and spreads that bucket into lower
    /// ones; the set of entries does not change. Safe for later pushes: a side only pushes after it
    /// pops its own minimum, and pushes keys >= that minimum.
    fn top(&mut self) -> Option<u64> {
        if self.len == 0 {
            return None;
        }
        if self.buckets[0].is_empty() {
            let b = self.buckets.iter().position(|v| !v.is_empty()).unwrap();
            let moving = std::mem::take(&mut self.buckets[b]);
            self.last = moving.iter().map(|e| e.key).min().unwrap();
            for &e in &moving {
                let to = self.bucket(e.key);
                self.buckets[to].push(e);
            }
            // Hand the emptied vector's capacity back to bucket b for reuse.
            self.buckets[b] = moving;
            self.buckets[b].clear();
        }
        Some(self.last)
    }

    fn pop(&mut self) -> Option<(u64, u32)> {
        self.top()?;
        let e = self.buckets[0].pop().unwrap();
        self.len -= 1;
        Some((e.key, e.node))
    }
}

/// Marks a free slot. Never a node id: ids are below n, and n is itself a u32 (spec section 2).
const EMPTY: u32 = u32::MAX;

/// Slots in each thread's small label table (a design constant, from the cache, not the input):
/// 2^14 slots x 20 bytes = 320 KB, so the tables of the two hyperthreads sharing a P-core
/// fit its 1.25 MB L2 with room for heap and CSR lines.
const SMALL_SLOTS: usize = 1 << 14;

/// One node's labels in the current search: tentative distance from s (side 0) and to t (side 1).
/// 20 bytes packed, no generation: a table is cleared through its touched list instead.
#[derive(Clone, Copy)]
#[repr(C, packed)]
struct Label {
    node: u32,
    dist: [u64; 2],
}

const FREE: Label = Label { node: EMPTY, dist: [INF; 2] };

/// An open-addressing map from node to Label, at most 7/10 full, that remembers which slots this
/// query filled so clearing costs the labels used, not the table size.
struct Table {
    slots: Vec<Label>,
    touched: Vec<u32>,
}

impl Table {
    fn with_slots(len: usize) -> Self {
        Table { slots: vec![FREE; len], touched: Vec::new() }
    }

    #[inline(always)]
    fn home(&self, node: u32) -> usize {
        (node.wrapping_mul(0x9E37_79B1) as usize) & (self.slots.len() - 1)
    }

    /// Index of node's label, created unlabelled on first sight; None if that would pass 7/10 load.
    #[inline(always)]
    fn find_or_insert(&mut self, node: u32) -> Option<usize> {
        let mask = self.slots.len() - 1;
        let mut i = self.home(node);
        loop {
            let here = self.slots[i].node;
            if here == node {
                return Some(i);
            }
            if here == EMPTY {
                if (self.touched.len() + 1) * 10 > self.slots.len() * 7 {
                    return None;
                }
                self.slots[i] = Label { node, dist: [INF; 2] };
                self.touched.push(i as u32);
                return Some(i);
            }
            i = (i + 1) & mask;
        }
    }

    /// Moves every label of `from` (listed by its touched slots) into this table, which has room.
    fn take_labels(&mut self, from: &[Label], touched: &[u32]) {
        let mask = self.slots.len() - 1;
        for &k in touched {
            let l = from[k as usize];
            let mut i = self.home(l.node);
            while self.slots[i].node != EMPTY {
                i = (i + 1) & mask;
            }
            self.slots[i] = l;
            self.touched.push(i as u32);
        }
    }

    fn clear(&mut self) {
        for &i in &self.touched {
            self.slots[i as usize].node = EMPTY;
        }
        self.touched.clear();
    }
}

/// Per-thread search scratch. Every query starts in the small, L2-sized label table; most searches
/// stay in it (local full seed 1: touched labels p50 199, p90 12,905, p99 52,100, max 78,485). A
/// query that outgrows it moves its labels to the large table, which doubles as needed and keeps
/// its size for later queries; it grows with the largest search, never to n. One heap per direction.
/// Measured locally (4-core VM, full seed 1, interleaved with the single growing table): 10.6 % of
/// queries outgrow the small table; queries phase 837-998 ms vs 865-1045 ms at 4 workers, 889-986 ms
/// vs 803-861 ms at 12 (three threads share each core's L2 here); M 108.9 vs about 121 MiB.
struct Scratch {
    small: Table,
    large: Table,
    in_large: bool,
    heaps: [RadixHeap; 2],
}

impl Scratch {
    fn new() -> Self {
        Scratch {
            small: Table::with_slots(SMALL_SLOTS),
            large: Table::with_slots(SMALL_SLOTS * 2),
            in_large: false,
            heaps: [RadixHeap::new(), RadixHeap::new()],
        }
    }

    fn reset(&mut self) {
        self.small.clear();
        self.large.clear();
        self.in_large = false;
        self.heaps[0].clear();
        self.heaps[1].clear();
    }

    #[inline(always)]
    fn label(&mut self, i: usize) -> &mut Label {
        if self.in_large { &mut self.large.slots[i] } else { &mut self.small.slots[i] }
    }

    /// Index of node's label in the current table. Indices last until the next insert.
    #[inline(always)]
    fn slot(&mut self, node: u32) -> usize {
        loop {
            let table = if self.in_large { &mut self.large } else { &mut self.small };
            if let Some(i) = table.find_or_insert(node) {
                return i;
            }
            self.make_room();
        }
    }

    /// The small table is full: move to the large one. The large table is full: double it.
    fn make_room(&mut self) {
        if !self.in_large {
            self.large.take_labels(&self.small.slots, &self.small.touched);
            self.small.clear();
            self.in_large = true;
        } else {
            let doubled = Table::with_slots(self.large.slots.len() * 2);
            let old = std::mem::replace(&mut self.large, doubled);
            self.large.take_labels(&old.slots, &old.touched);
        }
    }
}

/// Bidirectional Dijkstra: side 0 searches forward from s, side 1 backward from t on the reverse CSR.
/// mu is the best s-t path seen so far, updated on every relaxation that labels a node the other side
/// has labelled (zero-weight edges make "the frontiers met" too early a stop). Stop once the two heap
/// tops sum to mu or more, or either side runs dry. Returns the distance and the number of settles.
fn bidirectional(g: [&Csr; 2], sc: &mut Scratch, s: u32, t: u32) -> (u64, u64) {
    sc.reset();
    for (side, root) in [(0, s), (1, t)] {
        let i = sc.slot(root);
        let mut dist = sc.label(i).dist;
        dist[side] = 0;
        sc.label(i).dist = dist;
        sc.heaps[side].push(0, root);
    }
    let mut mu = INF;
    let mut settles = 0;
    loop {
        // Each top is the side's exact minimum key (RadixHeap::top), so the stop test is the floor's.
        let (Some(top_f), Some(top_b)) = (sc.heaps[0].top(), sc.heaps[1].top()) else {
            break;
        };
        // Both tops are below 2^63, so the sum cannot overflow.
        if top_f + top_b >= mu {
            break;
        }
        // Expand the side with fewer queued entries. Choosing by the lower top key instead lets a
        // backward search sweep the whole main body while a forward search from an island waits
        // behind its near-2^32 spur weights (local A/B: twice the settles, 3x the query time).
        let side = if sc.heaps[0].len() <= sc.heaps[1].len() { 0 } else { 1 };
        let (d, u) = sc.heaps[side].pop().unwrap();
        let iu = sc.slot(u);
        if d > { sc.label(iu).dist }[side] {
            continue; // stale: u was relabelled lower after this entry was pushed
        }
        settles += 1;
        for &e in g[side].adj(u as usize) {
            let (v, nd) = (e as u32, d + (e >> 32));
            let iv = sc.slot(v);
            let l = sc.label(iv);
            let mut dist = l.dist;
            if nd < dist[side] {
                dist[side] = nd;
                l.dist = dist;
                let other = dist[1 - side];
                if other != INF {
                    mu = mu.min(nd + other);
                }
                sc.heaps[side].push(nd, v);
            }
        }
    }
    (mu, settles)
}

/// The answer to one query (INF means unreachable), the settles it took, and whether its labels
/// outgrew the small table.
fn answer(fwd: &Csr, rev: &Csr, sc: &mut Scratch, s: u32, t: u32) -> (u64, u64, bool) {
    if s == t {
        return (0, 0, false);
    }
    if rev.degree(t as usize) == 0 || fwd.degree(s as usize) == 0 {
        return (INF, 0, false);
    }
    let (d, settles) = bidirectional([fwd, rev], sc, s, t);
    (d, settles, sc.in_large)
}

fn main() {
    let mut t = vec![("start", Instant::now())];
    let graph = map_file("/data/graph.txt");
    let queries = map_file("/data/queries.txt");
    let threads = workers();

    let mut i = 0;
    let n = read_num(graph, &mut i) as usize; // header "n m" (spec section 2); m is recounted in pass 1
    read_num(graph, &mut i);
    let (fwd, rev) = build_graph(graph, i, n, threads, &mut t);

    let mut pairs = Vec::new();
    let mut i = 0;
    while i < queries.len() {
        pairs.push((read_num(queries, &mut i) as u32, read_num(queries, &mut i) as u32));
    }

    let next = AtomicUsize::new(0);
    let mut answers = vec![(0u64, 0u64, false); pairs.len()];
    let results: Vec<Vec<(usize, (u64, u64, bool))>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (fwd, rev, pairs, next) = (&fwd, &rev, &pairs, &next);
                s.spawn(move || {
                    let mut sc = Scratch::new();
                    let mut done = Vec::new();
                    loop {
                        let q = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(src, dst)) = pairs.get(q) else { return done };
                        done.push((q, answer(fwd, rev, &mut sc, src, dst)));
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (q, a) in results.into_iter().flatten() {
        answers[q] = a;
    }
    t.push(("queries", Instant::now()));

    let mut out = String::with_capacity(answers.len() * 12);
    for &(d, _, _) in &answers {
        if d == INF {
            out.push_str("-1\n");
        } else {
            out.push_str(&d.to_string());
            out.push('\n');
        }
    }
    std::fs::write("/out/distances.txt", out).expect("write /out/distances.txt");
    t.push(("write", Instant::now()));

    // One stderr line of phase times (ms), worker count and search effort. sd-bench keeps each run's
    // stderr tail, so this line is the box's phase split on the scoring CPU.
    let phases: Vec<String> =
        t.windows(2).map(|w| format!("{}={:.1}ms", w[1].0, (w[1].1 - w[0].1).as_secs_f64() * 1e3)).collect();
    let settles: u64 = answers.iter().map(|a| a.1).sum();
    let unreachable: Vec<_> = answers.iter().filter(|a| a.0 == INF).collect();
    eprintln!(
        "phases: {} total={:.1}ms | threads={threads} n={n} m={} queries={} settles={settles} unreachable={} unreachable_settles={} outgrew_small={:.1}%",
        phases.join(" "),
        (t.last().unwrap().1 - t[0].1).as_secs_f64() * 1e3,
        fwd.edges.len(),
        answers.len(),
        unreachable.len(),
        unreachable.iter().map(|a| a.1).sum::<u64>(),
        answers.iter().filter(|a| a.2).count() as f64 * 100.0 / answers.len().max(1) as f64
    );
}
