//! Pass 2: workers claim chunks of records.csv from a shared counter, parse each chunk's records into
//! their own per-group slots and intern each name in the shared table. Each group's name bitset
//! belongs to one worker (group % workers), which alone writes it: a worker files each (group, id)
//! pair into a small bucket per owner, hands a full bucket to that owner's inbox and takes an empty one
//! from a shared pool, and drains its own inbox after every chunk. No rounds and no barriers: a slow
//! CPU delays only the chunks it holds and its own inbox. The rounds made every worker wait for the
//! slowest at 27 barriers, so one doubly loaded CPU stretched the whole run by 38 % (c-18 probe w13,
//! box 0.071 -> 0.098 s), the size of the board's swings. Shared bits set by atomic OR instead of
//! owners measured 219 -> 494 ms on this VM (every OR fetches a line another core last wrote).
//! Measured (this VM, 12 workers, full seed 1, interleaved medians of 9): 232 -> 180 ms wall; M about
//! +2.5 MiB (sdrig), as retired intern generations now live to the end (no quiescent point).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::{names::Intern, names::NameSet, records::parse_range, records::record_start, records::Slot, regions::Lookup};

/// Bytes per chunk: a design constant, about 800 chunks for the 202 MB file, so the last chunks to
/// finish are short whoever holds them. An owner drains its inbox once per chunk, so the pairs
/// waiting for it scale with the chunk: 1 MiB chunks and 512-pair buckets measured box M 17.8 MB
/// (c-19a); 256 KiB and 256 pairs: sdrig M 16.2 -> 15.8 MiB on this VM, box in the commit log.
pub const CHUNK: usize = 1 << 18;

/// Pairs per bucket (2 KiB): a full bucket is one hand-off to its owner.
const BUCKET: usize = 256;

/// What pass 2 returns.
pub struct Pass2 {
    /// Per worker, its per-group slots.
    pub slots: Vec<Vec<Slot>>,
    /// names_distinct per group.
    pub distinct: Vec<u64>,
    /// False when some chunk did not stop exactly on the next chunk's start (e1), i.e. a cut was not
    /// a record start; the caller must then rerun on one worker with one chunk.
    pub exact: bool,
}

/// Runs pass 2 on `workers` threads over the records from `first` to the end, cut into `chunk`-byte
/// pieces at record starts.
pub fn run<L: Lookup + Sync>(
    b: &[u8],
    first: usize,
    chunk: usize,
    workers: usize,
    lookup: &L,
    groups: usize,
    intern: &Intern,
) -> Pass2 {
    let chunks = (b.len() - first).div_ceil(chunk);
    let start = |k: usize| if k == 0 { first } else { record_start(b, (first + k * chunk).min(b.len())) };
    // Group -> (owner, index among the owner's groups), so the hot loop needs no `%` or division.
    let route: Vec<(usize, u32)> = (0..groups).map(|g| (g % workers, (g / workers) as u32)).collect();
    let inbox: Vec<Mutex<Vec<Vec<(u32, u32)>>>> = (0..workers).map(|_| Mutex::default()).collect();
    let pool: Mutex<Vec<Vec<(u32, u32)>>> = Mutex::default();
    let take = || pool.lock().unwrap().pop().unwrap_or_default();
    let next = AtomicUsize::new(0);
    let flushed = AtomicUsize::new(0);
    let exact = AtomicBool::new(true);
    // Per worker, the intern era it last reached between chunks (`Intern::quiesce`).
    let seen: Vec<AtomicU64> = (0..workers).map(|_| AtomicU64::new(0)).collect();

    // One worker's whole run; the closure holds only shared references, so every thread copies it.
    let worker = |w: usize| {
        let mut slots = vec![Slot::new(); groups];
        // A group's set sits at its owner, at index group / workers.
        let mut owned = vec![NameSet::default(); groups.div_ceil(workers)];
        // Sets the bits of every bucket in this worker's inbox, and returns the buckets to the pool.
        let drain = |owned: &mut Vec<NameSet>| {
            for mut bucket in std::mem::take(&mut *inbox[w].lock().unwrap()) {
                bucket.iter().for_each(|&(local, id)| owned[local as usize].insert(id));
                bucket.clear();
                pool.lock().unwrap().push(bucket);
            }
        };
        let mut out: Vec<Vec<(u32, u32)>> = (0..workers).map(|_| take()).collect();
        loop {
            let k = next.fetch_add(1, Ordering::Relaxed);
            intern.quiesce(&seen, w, k >= chunks);
            if k >= chunks {
                break;
            }
            let (mut i, end) = (start(k), start(k + 1));
            let hashed = |at, len| intern.prefetch(b, at, len);
            let on_name = |g: usize, at, len, h| {
                let (owner, local) = route[g];
                out[owner].push((local, intern.id(b, at, len, h)));
                if out[owner].len() == BUCKET {
                    let full = std::mem::replace(&mut out[owner], take());
                    inbox[owner].lock().unwrap().push(full);
                }
            };
            parse_range(b, &mut i, end, lookup, &mut slots, hashed, on_name);
            // Pairwise end check (e1): the parse must stop exactly on the next chunk's start.
            exact.fetch_and(i == end, Ordering::Relaxed);
            drain(&mut owned);
        }
        for (owner, bucket) in out.into_iter().enumerate() {
            inbox[owner].lock().unwrap().push(bucket);
        }
        // Once every worker has flushed (each flush is under the inbox locks, before its
        // count), no bucket can arrive any more: one last drain empties this inbox for good.
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

    let distinct = (0..groups).map(|g| results[g % workers].1[g / workers].len()).collect();
    Pass2 { slots: results.into_iter().map(|r| r.0).collect(), distinct, exact: exact.load(Ordering::Relaxed) }
}
