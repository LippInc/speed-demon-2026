//! Track D (Traverse): shortest directed path lengths for every query in /data/queries.txt.
//!
//! Build: two parallel passes over the mmapped graph.txt go straight into a forward and a reverse
//! CSR (pass 1 counts degrees, pass 2 places each edge at an atomic cursor), with no edge buffer
//! and no copy of the input. Queries: cheap answers first (s == t, a target with no in-edges, a
//! source with no out-edges), then a bidirectional Dijkstra per query on a worker thread, with
//! per-thread scratch sized by the search, not by n.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
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

/// One node's labels in the current search: tentative distance from s (side 0) and to t (side 1).
#[derive(Clone, Copy)]
struct Label {
    node: u32,
    generation: u32,
    dist: [u64; 2],
}

/// Per-thread search scratch: an open-addressing map from node to Label, cleared in O(1) per query
/// by bumping the generation, plus one heap per search direction. It grows with the largest search,
/// never to n.
struct Scratch {
    labels: Vec<Label>,
    used: usize,
    generation: u32,
    heaps: [BinaryHeap<Reverse<(u64, u32)>>; 2],
}

impl Scratch {
    fn new() -> Self {
        let empty = Label { node: 0, generation: 0, dist: [INF; 2] };
        Scratch { labels: vec![empty; 1 << 12], used: 0, generation: 0, heaps: [BinaryHeap::new(), BinaryHeap::new()] }
    }

    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            self.labels.iter_mut().for_each(|l| l.generation = 0);
            self.generation = 1;
        }
        self.used = 0;
        self.heaps[0].clear();
        self.heaps[1].clear();
    }

    #[inline(always)]
    fn home(&self, node: u32) -> usize {
        (node.wrapping_mul(0x9E37_79B1) as usize) & (self.labels.len() - 1)
    }

    /// Index of node's label, created unlabelled on first sight. Indices last until the next insert.
    #[inline(always)]
    fn slot(&mut self, node: u32) -> usize {
        let mask = self.labels.len() - 1;
        let mut i = self.home(node);
        loop {
            let cap = self.labels.len();
            let l = &mut self.labels[i];
            if l.generation != self.generation {
                if (self.used + 1) * 2 > cap {
                    self.grow();
                    return self.slot(node);
                }
                *l = Label { node, generation: self.generation, dist: [INF; 2] };
                self.used += 1;
                return i;
            }
            if l.node == node {
                return i;
            }
            i = (i + 1) & mask;
        }
    }

    fn grow(&mut self) {
        let empty = Label { node: 0, generation: 0, dist: [INF; 2] };
        let doubled = vec![empty; self.labels.len() * 2];
        let old = std::mem::replace(&mut self.labels, doubled);
        let mask = self.labels.len() - 1;
        for l in old.into_iter().filter(|l| l.generation == self.generation) {
            let mut i = self.home(l.node);
            while self.labels[i].generation == self.generation {
                i = (i + 1) & mask;
            }
            self.labels[i] = l;
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
        sc.labels[i].dist[side] = 0;
        sc.heaps[side].push(Reverse((0, root)));
    }
    let mut mu = INF;
    let mut settles = 0;
    loop {
        let (Some(&Reverse((top_f, _))), Some(&Reverse((top_b, _)))) = (sc.heaps[0].peek(), sc.heaps[1].peek()) else {
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
        let Reverse((d, u)) = sc.heaps[side].pop().unwrap();
        let iu = sc.slot(u);
        if d > sc.labels[iu].dist[side] {
            continue; // stale: u was relabelled lower after this entry was pushed
        }
        settles += 1;
        for &e in g[side].adj(u as usize) {
            let (v, nd) = (e as u32, d + (e >> 32));
            let iv = sc.slot(v);
            let l = &mut sc.labels[iv];
            if nd < l.dist[side] {
                l.dist[side] = nd;
                let other = l.dist[1 - side];
                if other != INF {
                    mu = mu.min(nd + other);
                }
                sc.heaps[side].push(Reverse((nd, v)));
            }
        }
    }
    (mu, settles)
}

/// The answer to one query and the settles it took; INF means unreachable.
fn answer(fwd: &Csr, rev: &Csr, sc: &mut Scratch, s: u32, t: u32) -> (u64, u64) {
    if s == t {
        return (0, 0);
    }
    if rev.degree(t as usize) == 0 || fwd.degree(s as usize) == 0 {
        return (INF, 0);
    }
    bidirectional([fwd, rev], sc, s, t)
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
    let mut answers = vec![(0u64, 0u64); pairs.len()];
    let results: Vec<Vec<(usize, u64, u64)>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (fwd, rev, pairs, next) = (&fwd, &rev, &pairs, &next);
                s.spawn(move || {
                    let mut sc = Scratch::new();
                    let mut done = Vec::new();
                    loop {
                        let q = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(src, dst)) = pairs.get(q) else { return done };
                        let (d, settles) = answer(fwd, rev, &mut sc, src, dst);
                        done.push((q, d, settles));
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (q, d, settles) in results.into_iter().flatten() {
        answers[q] = (d, settles);
    }
    t.push(("queries", Instant::now()));

    let mut out = String::with_capacity(answers.len() * 12);
    for &(d, _) in &answers {
        if d == INF {
            out.push_str("-1\n");
        } else {
            out.push_str(&d.to_string());
            out.push('\n');
        }
    }
    std::fs::write("/out/distances.txt", out).expect("write /out/distances.txt");
    t.push(("write", Instant::now()));

    let phases: Vec<String> =
        t.windows(2).map(|w| format!("{}={:.1}ms", w[1].0, (w[1].1 - w[0].1).as_secs_f64() * 1e3)).collect();
    let settles: u64 = answers.iter().map(|a| a.1).sum();
    let unreachable: Vec<_> = answers.iter().filter(|a| a.0 == INF).collect();
    eprintln!(
        "phases: {} total={:.1}ms | threads={threads} n={n} m={} queries={} settles={settles} unreachable={} unreachable_settles={}",
        phases.join(" "),
        (t.last().unwrap().1 - t[0].1).as_secs_f64() * 1e3,
        fwd.edges.len(),
        answers.len(),
        unreachable.len(),
        unreachable.iter().map(|a| a.1).sum::<u64>()
    );
}
