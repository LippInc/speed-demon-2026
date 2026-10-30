//! Track D (Traverse): shortest directed path lengths for every query in /data/queries.txt.
//!
//! Build: two parallel passes over the mmapped graph.txt go straight into a forward and a reverse
//! CSR (pass 1 counts degrees, pass 2 places each edge at an atomic cursor), with no edge buffer
//! and no copy of the input. Queries: cheap answers first (s == t, a target with no in-edges, a
//! source with no out-edges), then a bidirectional Dijkstra per query on a worker thread, with
//! per-thread scratch sized by the search, not by n.

use std::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};
use std::ffi::c_void;
use std::fs::File;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// A shared output array that threads fill at disjoint indices (each thread owns whole partitions).
struct Slots<T>(*mut T);
unsafe impl<T> Sync for Slots<T> {}

impl<T> Slots<T> {
    /// SAFETY: the caller is the only writer of index i.
    unsafe fn put(&self, i: usize, value: T) {
        unsafe { *self.0.add(i) = value }
    }
}

/// One CSR entry, 6 bytes: the head as two u16 halves, then the weight, or HEAVY for a weight of
/// HEAVY or more (u32 weights, spec section 2), which Csr::heavy holds by edge index.
/// Why: the two CSRs were 80 MB of u64 entries, most of the query phase's memory, while nearly all
/// weights fit 16 bits (full seed 1: 50 of 5,000,000 do not). 6 bytes cut them to 60 MB.
type Edge = [u16; 3];
const HEAVY: u16 = u16::MAX;

/// Compressed adjacency: the edges of node u are edges[start(u)..end[u]].
struct Csr {
    end: Vec<u32>,
    edges: Vec<Edge>,
    heavy: Vec<(u32, u32)>,
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
    /// (head, weight) of each edge of u.
    #[inline(always)]
    fn row(&self, u: usize) -> impl Iterator<Item = (u32, u64)> + '_ {
        (self.start(u)..self.end[u] as usize).map(move |i| {
            let (e, heavy) = (self.edges[i], || self.heavy[self.heavy.partition_point(|h| h.0 < i as u32)].1);
            let w = Some(e[2]).filter(|&w| w < HEAVY).map_or_else(heavy, u32::from);
            (e[0] as u32 | (e[1] as u32) << 16, w as u64)
        })
    }
}

/// One edge on its way into a CSR: (row, head, weight). 12 bytes.
type Record = (u32, u32, u32);

/// Node-range partitions per worker (a design constant): enough that threads taking partitions from
/// a counter finish together, few enough that each thread's per-partition buckets stay large.
const PARTS_PER_WORKER: usize = 4;

/// Runs job(0..threads) on scoped threads and returns their results in order.
fn on_threads<T: Send>(threads: usize, job: impl Fn(usize) -> T + Sync) -> Vec<T> {
    std::thread::scope(|s| {
        let job = &job;
        let running: Vec<_> = (0..threads).map(|k| s.spawn(move || job(k))).collect();
        running.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

/// Phase names and the time each ended, for the stderr phase line.
type Phases = Vec<(&'static str, Instant)>;

/// The reverse CSR is built in this many slabs of consecutive partitions (a design constant), so
/// only half the edges sit in records next to both CSRs: build peak below the query phase's.
/// Re-filing the parse records by head instead (no second read of fwd) measured 176 MB of anon
/// peak locally at 12 workers against 134 MB this way, so the slabs stay.
const REV_SLABS: usize = 2;

/// Writes the rows of partitions first.. of g from the records filed for them (buckets[thread][k]
/// for partition first + k; partition p = nodes with id >> shift == p), each partition on one
/// thread: a local degree count, a prefix sum from the partition's first edge slot, then a scatter
/// into its own slices of g.end and g.edges. No atomics; each thread first-touches its own pages.
/// Partitions before `first` must already be placed.
fn place(buckets: &[Vec<Vec<Record>>], first: usize, shift: u32, g: &mut Csr, threads: usize) {
    let n = g.end.len();
    let mut first_slot = vec![g.start(first << shift) as u32];
    for p in 0..buckets[0].len() {
        first_slot.push(first_slot[p] + buckets.iter().map(|b| b[p].len() as u32).sum::<u32>());
    }
    let (edges, end) = (Slots(g.edges.as_mut_ptr()), Slots(g.end.as_mut_ptr()));
    let next = AtomicUsize::new(0);
    let heavy = on_threads(threads, |_| {
        let (mut cursor, mut heavy) = (Vec::new(), Vec::new());
        loop {
            let p = next.fetch_add(1, Ordering::Relaxed);
            if p + 1 >= first_slot.len() {
                return heavy;
            }
            let lo = (first + p) << shift;
            cursor.clear();
            cursor.resize(n.min(lo + (1 << shift)) - lo, 0u32);
            let records = || buckets.iter().flat_map(|b| &b[p]);
            records().for_each(|r| cursor[r.0 as usize - lo] += 1);
            let mut at = first_slot[p];
            for (k, c) in cursor.iter_mut().enumerate() {
                at += std::mem::replace(c, at);
                // SAFETY: node lo + k is in partition p, which only this thread writes.
                unsafe { end.put(lo + k, at) };
            }
            for r in records() {
                let c = &mut cursor[r.0 as usize - lo];
                // SAFETY: c walks this row's own slots inside partition p's slice of edges.
                let e = [r.1 as u16, (r.1 >> 16) as u16, r.2.min(HEAVY as u32) as u16];
                unsafe { edges.put(*c as usize, e) };
                heavy.extend((r.2 >= HEAVY as u32).then_some((*c, r.2)));
                *c += 1;
            }
        }
    });
    g.heavy.extend(heavy.into_iter().flatten());
    g.heavy.sort_unstable();
}

/// Builds both CSRs without atomics. Parse: each thread parses its byte range once and files every
/// edge as (tail, head, w) into its own bucket for the tail's partition; place() builds fwd. Rev, in
/// REV_SLABS rounds: threads read forward partitions and file (head, tail, w) by head partition for
/// this slab's heads, then place() builds those reverse partitions.
/// Why: the atomic build paid two locked adds per line in each pass into random slots, and each
/// locked add drains the store buffer behind the previous line's store misses. Measured on the box
/// (d-7, 8a68049): build 104.6 -> 85 ms; locally (full seed 1, interleaved with the atomic two-pass
/// build) 590-640 ms -> 185-200 ms at 12 workers.
/// Partitions are the smallest power-of-two node range that splits this input's n nodes into at
/// most PARTS_PER_WORKER * threads ranges.
fn build_graph(graph: &[u8], body: usize, n: usize, threads: usize, t: &mut Phases) -> (Csr, Csr) {
    let shift = (0..).find(|&s| (n.max(1) - 1) >> s < PARTS_PER_WORKER * threads).unwrap();
    let parts = ((n.max(1) - 1) >> shift) + 1;
    let cuts = cut_at_lf(graph, body, threads);
    let by_tail = on_threads(threads, |k| {
        let (mut mine, mut i) = (vec![Vec::new(); parts], cuts[k]);
        while i < cuts[k + 1] {
            let u = read_num(graph, &mut i) as u32;
            let v = read_num(graph, &mut i) as u32;
            // w is at most 4,294,967,295 (spec section 2), so all three fit u32.
            mine[(u >> shift) as usize].push((u, v, read_num(graph, &mut i) as u32));
        }
        mine
    });
    t.push(("parse", Instant::now()));
    let m = by_tail.iter().flatten().map(Vec::len).sum();
    assert!(m <= u32::MAX as usize, "CSR offsets are u32");
    let mut fwd = Csr { end: vec![0; n], edges: vec![[0; 3]; m], heavy: Vec::new() };
    place(&by_tail, 0, shift, &mut fwd, threads);
    drop(by_tail);
    t.push(("fwd", Instant::now()));
    let mut rev = Csr { end: vec![0; n], edges: vec![[0; 3]; m], heavy: Vec::new() };
    for slab in 0..REV_SLABS {
        let (first, last) = (parts * slab / REV_SLABS, parts * (slab + 1) / REV_SLABS);
        let next = AtomicUsize::new(0);
        let by_head = on_threads(threads, |_| {
            let mut mine = vec![Vec::new(); last - first];
            while let Some(p) = Some(next.fetch_add(1, Ordering::Relaxed)).filter(|&p| p < parts) {
                for u in p << shift..n.min((p + 1) << shift) {
                    for (v, w) in fwd.row(u) {
                        let q = (v >> shift) as usize;
                        if (first..last).contains(&q) {
                            mine[q - first].push((v, u as u32, w as u32));
                        }
                    }
                }
            }
            mine
        });
        place(&by_head, first, shift, &mut rev, threads);
    }
    t.push(("rev", Instant::now()));
    (fwd, rev)
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

/// Pops ahead whose CSR rows are prefetched (a design constant). The CSRs (94 MB) are four times the
/// box's L3, so a settle's edge row is a DRAM miss; the next pops of this side sit at the end of
/// radix bucket 0, so their rows can load while u is relaxed. Measured locally (full seed 1,
/// interleaved with d-8, query phase medians): 875 -> 564 ms at 12 workers, 795 -> 490 ms at 4.
/// Distances 4 / 8 / 16 gave 569 / 564 / 565 ms at 12; prefetching only end[next] gave 725-739 ms,
/// and prefetching each neighbour's label slot as well bought under 2 %, so neither is here.
const PREFETCH_AHEAD: usize = 8;

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
        // The next pops of this side are the last entries of bucket 0 (it pops from its end).
        for e in sc.heaps[side].buckets[0].iter().rev().take(PREFETCH_AHEAD) {
            let row = g[side].edges.as_ptr().wrapping_add(g[side].start(e.node as usize));
            unsafe { _mm_prefetch(row as *const i8, _MM_HINT_T0) }; // a hint: never faults
        }
        for (v, w) in g[side].row(u as usize) {
            let nd = d + w;
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
    let results = on_threads(threads, |_| {
        let (mut sc, mut done) = (Scratch::new(), Vec::new());
        loop {
            let q = next.fetch_add(1, Ordering::Relaxed);
            let Some(&(src, dst)) = pairs.get(q) else { return done };
            done.push((q, answer(&fwd, &rev, &mut sc, src, dst)));
        }
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
