//! The parallel pass: workers claim chunks of records.csv from a shared counter, parse each into
//! their own per-group slots, and look each name up in the shared table (names.rs). Each group's
//! name bitset belongs to one worker (group % workers), which alone writes it: a worker files each
//! (group, id) pair into a small bucket per owner, hands a full bucket to that owner's inbox and
//! takes an empty one from a shared pool, and drains its own inbox after every chunk. No rounds and
//! no barriers (8a60e4e's routing: a slow CPU delays only its own chunks and inbox, c-18 probe w13).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::names::{Key, NameSet, Table, View, key};
use crate::{records::Slot, records::parse_chunk, records::record_start, regions::Lookup};

unsafe extern "C" {
    fn madvise(addr: *const u8, len: usize, advice: i32) -> i32;
}

/// Bytes per chunk: a design constant, about 800 chunks for the 202 MB file, so the last chunks to
/// finish are short whoever holds them.
pub const CHUNK: usize = 1 << 18;

/// Pairs per bucket (2 KiB): a full bucket is one hand-off to its owner. 1024 or 2048 measured T
/// 0.045 -> 0.044 s on box 2 for M +0.9 / +1.9 MB (c-41), so the locks cost about 1 ms at 256.
const BUCKET: usize = 256;

/// What the pass returns.
pub struct Pass2 {
    /// Per worker, its per-group slots.
    pub slots: Vec<Vec<Slot>>,
    /// names_distinct per group.
    pub distinct: Vec<u64>,
    /// False when some chunk did not stop exactly on the next chunk's start (e1), i.e. a cut was not
    /// a record start; the caller must then rerun on one worker with one chunk.
    pub exact: bool,
}

/// One worker's side of the name path: its outgoing (group, id) buckets, one per owner.
struct Router<'a> {
    table: View<'a>,
    b: &'a [u8],
    route: &'a [(usize, u32)],
    inbox: &'a [Mutex<Vec<Vec<(u32, u32)>>>],
    pool: &'a Mutex<Vec<Vec<(u32, u32)>>>,
    out: Vec<Vec<(u32, u32)>>,
}

/// An empty bucket, from the pool when one is there. Every bucket has room for BUCKET pairs.
fn take(pool: &Mutex<Vec<Vec<(u32, u32)>>>) -> Vec<(u32, u32)> {
    pool.lock().unwrap().pop().unwrap_or_else(|| Vec::with_capacity(BUCKET))
}

impl Router<'_> {
    /// Looks the record's name up and files its (group, id) pair for the group's owner; a full
    /// bucket goes to the owner's inbox. Inline, at the record's end: batching the lookups (16 or
    /// 32 records resolved in a loop of their own, 92c41ae / 023ce09) measured box T 0.052 / 0.051 s
    /// against 0.050 inline (880ff14).
    #[inline(always)]
    fn send(&mut self, group: usize, key: Key) {
        let id = self.table.id(key, self.b);
        // SAFETY: `route` has an entry per group and names an owner below `out.len()`; a bucket is
        // handed off the moment it holds BUCKET pairs, and has room for BUCKET (`take`), so the
        // write lands in its spare capacity: no bounds, capacity or growth checks per pair.
        unsafe {
            let (owner, local) = *self.route.get_unchecked(group);
            let out = self.out.get_unchecked_mut(owner);
            out.as_mut_ptr().add(out.len()).write((local, id));
            out.set_len(out.len() + 1);
            if out.len() == BUCKET {
                let full = std::mem::replace(out, take(self.pool));
                self.inbox.get_unchecked(owner).lock().unwrap().push(full);
            }
        }
    }
}

/// Runs the pass on `workers` threads over the records of `b[..len]` from `first` to the end (`b`
/// holds the guard after them), cut into `chunk`-byte pieces at record starts. Stops early once
/// `table` overflows (the caller reruns).
pub fn run<L: Lookup + Sync>(
    b: &[u8],
    len: usize,
    first: usize,
    chunk: usize,
    workers: usize,
    lookup: &L,
    groups: usize,
    table: &Table,
) -> Pass2 {
    let file = &b[..len];
    let chunks = (len - first).div_ceil(chunk);
    let start = |k: usize| if k == 0 { first } else { record_start(file, (first + k * chunk).min(len)) };
    // Group -> (owner, index among the owner's groups), so the hot loop needs no `%` or division.
    let route: Vec<(usize, u32)> = (0..groups).map(|g| (g % workers, (g / workers) as u32)).collect();
    let inbox: Vec<Mutex<Vec<Vec<(u32, u32)>>>> = (0..workers).map(|_| Mutex::default()).collect();
    let pool: Mutex<Vec<Vec<(u32, u32)>>> = Mutex::default();
    let next = AtomicUsize::new(0);
    let flushed = AtomicUsize::new(0);
    let exact = AtomicBool::new(true);

    let worker = |w: usize| {
        let mut slots = vec![Slot::new(); groups];
        // A group's set sits at its owner, at index group / workers.
        let mut owned = vec![NameSet::new(table.entries()); groups.div_ceil(workers)];
        // Sets the bits of every bucket in this worker's inbox, and returns the buckets to the pool.
        let drain = |owned: &mut Vec<NameSet>| {
            for mut bucket in std::mem::take(&mut *inbox[w].lock().unwrap()) {
                // SAFETY: `local` indexes this owner's groups (`route`) and every id is below the
                // table's entry count, which sized the sets.
                bucket.iter().for_each(|&(local, id)| unsafe { owned.get_unchecked_mut(local as usize).insert(id) });
                bucket.clear();
                pool.lock().unwrap().push(bucket);
            }
        };
        let out = (0..workers).map(|_| take(&pool)).collect();
        let view = table.view();
        let mut router = Router { table: view, b, route: &route, inbox: &inbox, pool: &pool, out };
        loop {
            let k = next.fetch_add(1, Ordering::Relaxed);
            if k >= chunks || table.overflowed() {
                break;
            }
            let end = start(k + 1);
            // The name is hashed and its home bucket prefetched as soon as it is parsed; the lookup
            // waits for the record's end, so the bucket's cache miss overlaps the rest of the parse.
            let on_name = |at, n| {
                // SAFETY: parse_chunk passes a span inside the padded mapping.
                let key = unsafe { key(b.as_ptr(), at, n) };
                view.prefetch(key);
                key
            };
            let on_record = |g: usize, key| router.send(g, key);
            let stop = parse_chunk(b, len, start(k), end, lookup, &mut slots, on_name, on_record);
            // Pairwise end check (e1): the parse must stop exactly on the next chunk's start.
            exact.fetch_and(stop == end, Ordering::Relaxed);
            drain(&mut owned);
        }
        // The mapping's page-table entries (about 50k for the 202 MB file) were torn down at exit on
        // one CPU; each worker drops its share now (c-37f). A page touched again (a neighbour's last
        // record, a rerun, a long name's compare) faults back in from the page cache. The last
        // partial page is kept: it holds the guard (main.rs), a private copy MADV_DONTNEED would discard.
        let pages = len & !4095;
        let share = pages.div_ceil(workers).next_multiple_of(4096);
        let from = (w * share).min(pages);
        // SAFETY: a page-aligned range inside the mapping; madvise only drops page-table entries.
        unsafe { madvise(b.as_ptr().add(from), share.min(pages - from), 4) }; // 4 = MADV_DONTNEED
        for (owner, bucket) in router.out.into_iter().enumerate() {
            inbox[owner].lock().unwrap().push(bucket);
        }
        // Once every worker has flushed (each flush is under the inbox locks, before its count), no
        // bucket can arrive any more: one last drain empties this inbox for good.
        flushed.fetch_add(1, Ordering::Release);
        while flushed.load(Ordering::Acquire) < workers {
            drain(&mut owned);
            std::thread::yield_now();
        }
        drain(&mut owned);
        (slots, owned)
    };
    let results: Vec<(Vec<Slot>, Vec<NameSet>)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers).map(|w| s.spawn(move || worker(w))).collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let distinct = (0..groups).map(|g| results[g % workers].1[g / workers].len).collect();
    Pass2 { slots: results.into_iter().map(|r| r.0).collect(), distinct, exact: exact.load(Ordering::Relaxed) }
}
