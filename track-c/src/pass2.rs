//! The parallel pass: workers claim chunks of records.csv from a shared counter, parse each into
//! their own per-group slots, and send each record's name to its owner (names.rs). A worker writes
//! its messages into one bucket per owner through raw cursors, hands a full bucket to the owner's
//! inbox, and files the buckets in its own inbox at each of its hand-overs and after every chunk.
//! No rounds and no barriers (the routing of c-18: a slow CPU delays only its own chunks and
//! inbox, probe w13). At its end each worker adds its slots and its owner's counts to the totals.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::*};
use std::sync::Mutex;

use crate::names::{Msg, Name, Owner};
use crate::records::{Slot, parse_chunk, record_start};
use crate::regions::Build;

unsafe extern "C" {
    fn madvise(addr: *const u8, len: usize, advice: i32) -> i32;
}

/// Bytes per chunk: a design constant, about 800 chunks for the 202 MB file, so the last chunks to
/// finish are short whoever holds them.
const CHUNK: usize = 1 << 18;

/// Slots per bucket (4 KiB): a full bucket is one hand-off to its owner.
const BUCKET: usize = 256;

/// Full buckets an owner's mailbox may hold before a sender files them itself. A stalled owner (a
/// descheduled or late thread) let 11 senders run ahead: mailboxes of 300-380 buckets and 2,300-3,900
/// fresh 4 KiB buckets per run (12-15 MB, laptop counts, c-52 s2). Never reached while owners keep up.
const LAG: usize = 16;

/// Per group the merged slots, and names_distinct.
pub type Totals = (Vec<Slot>, Vec<u64>);

/// An owner's mailbox: the full buckets sent to it, and the empty ones filed, which senders take back.
#[derive(Default)]
struct Mailbox {
    full: Vec<Vec<Msg>>,
    empty: Vec<Vec<Msg>>,
}

/// One worker's sending side: its outgoing buckets, one per owner, written through raw cursors (no
/// Vec length or capacity work per message, c-49 b2: 22.47 ms of pass against 23.54 with Vec pushes).
struct Worker<'a> {
    w: usize,
    b: &'a [u8],
    boxes: &'a [Mutex<Mailbox>],
    /// Each owner's names; its own worker files them, a sender only to clear a backlog (LAG).
    owners: &'a [Mutex<Owner>],
    out: Vec<Vec<Msg>>,
    cur: Vec<*mut Msg>,
    /// Where each bucket is handed over: one slot short of BUCKET, so a two-slot name always fits.
    lim: Vec<*mut Msg>,
}

impl<'a> Worker<'a> {
    fn new(w: usize, b: &'a [u8], boxes: &'a [Mutex<Mailbox>], owners: &'a [Mutex<Owner>]) -> Self {
        let n = boxes.len();
        let none = std::ptr::null_mut();
        let (out, cur, lim) = (vec![Vec::new(); n], vec![none; n], vec![none; n]);
        let mut me = Worker { w, b, boxes, owners, out, cur, lim };
        (0..n).for_each(|o| me.refill(o, None));
        me
    }

    /// Makes `bucket` (or a new one) owner `o`'s outgoing bucket, with its cursor and limit.
    fn refill(&mut self, o: usize, bucket: Option<Vec<Msg>>) {
        self.out[o] = bucket.unwrap_or_else(|| Vec::with_capacity(BUCKET));
        self.cur[o] = self.out[o].as_mut_ptr();
        // SAFETY: each bucket has room for BUCKET slots.
        self.lim[o] = unsafe { self.cur[o].add(BUCKET - 1) };
    }

    /// Files every bucket in owner `o`'s mailbox and returns them emptied, unless another worker is
    /// filing for `o` right now (`wait`: then waits for it). Never inlined: it runs once per bucket or
    /// chunk, and profiles then show the owner's work apart.
    #[inline(never)]
    fn drain(&self, o: usize, wait: bool) {
        let mut own = match self.owners[o].try_lock() {
            Ok(own) => own,
            Err(_) if wait => self.owners[o].lock().unwrap(),
            Err(_) => return,
        };
        let mut buckets = std::mem::take(&mut self.boxes[o].lock().unwrap().full);
        if buckets.is_empty() {
            return;
        }
        for bucket in &mut buckets {
            own.file(bucket, self.b);
            bucket.clear();
        }
        self.boxes[o].lock().unwrap().empty.append(&mut buckets);
    }

    /// Writes a record's name into its owner's bucket. The owner is the name hash's high bits times
    /// the worker count: multiply-shift over the hash's 32 bits.
    #[inline(always)]
    fn send(&mut self, name: Name) {
        let workers = self.out.len();
        // SAFETY: owner < workers; its cursor is below its limit (hand_over runs once it gets there),
        // so both slots land inside the bucket's capacity.
        unsafe {
            let (o, c) = match name {
                Name::One(h, m) => {
                    let o = (h as u64 * workers as u64 >> 32) as usize;
                    let c = *self.cur.get_unchecked(o);
                    c.write(m);
                    (o, c.add(1))
                }
                Name::Two(h, m, tail) => {
                    let o = (h as u64 * workers as u64 >> 32) as usize;
                    let c = *self.cur.get_unchecked(o);
                    c.write(m);
                    c.add(1).write(tail);
                    (o, c.add(2))
                }
            };
            *self.cur.get_unchecked_mut(o) = c;
            if c >= *self.lim.get_unchecked(o) {
                self.hand_over(o);
            }
        }
    }

    /// Hands owner `o`'s bucket to its mailbox and takes an empty one back, then files this worker's
    /// own mailbox (and `o`'s past LAG). The rare path, kept out of the parse loop (c-49). Filing at
    /// each hand-over, not only after each chunk, keeps the buckets few and their slots still cached.
    #[cold]
    #[inline(never)]
    fn hand_over(&mut self, o: usize) {
        let full = self.seal(o);
        let (fresh, backlog) = {
            let mut mailbox = self.boxes[o].lock().unwrap();
            mailbox.full.push(full);
            (mailbox.empty.pop(), mailbox.full.len())
        };
        if backlog > LAG {
            self.drain(o, false);
        }
        self.refill(o, fresh);
        self.drain(self.w, false);
    }

    /// Owner `o`'s bucket with its length set to the slots written, leaving an empty Vec behind.
    fn seal(&mut self, o: usize) -> Vec<Msg> {
        let mut v = std::mem::take(&mut self.out[o]);
        // SAFETY: every slot from the bucket's start up to cur[o] was written, inside its capacity.
        unsafe { v.set_len(self.cur[o].offset_from(v.as_ptr()) as usize) };
        v
    }
}

/// Runs the pass over the records of `b` (followed by the guard, see `map_padded`) on `workers`
/// threads and returns the totals: per group the merged slots and names_distinct. Every worker
/// first helps build the region table (`Build::ready`; at once on a rerun).
///
/// e1: a chunk that does not stop exactly on the next chunk's start means a cut was not a record
/// start, so the parse is untrustworthy and the owners hold names from wrong records. Then the
/// whole file runs again as one chunk on one worker, where the start is always right. That rerun's
/// own end check is not made: its one chunk starts at the first record (always a record start) and
/// ends at the file's end, and one worker parses it to there, so it stops exactly where it should
/// (a last record without a terminator ends at the file's end, records.rs `i.min(len)`). Only a
/// record running past the end of the file (outside the grammar) breaks that, and then no answer is
/// right anyway.
pub fn run(b: &[u8], workers: usize, regions: &Build) -> Totals {
    let (totals, exact) = pass(b, CHUNK, workers, regions);
    exact.then_some(totals).unwrap_or_else(|| pass(b, usize::MAX, 1, regions).0)
}

/// One pass with `chunk`-byte pieces cut at record starts (the first at the first record after the
/// header), and whether every piece stopped exactly on the next one's start.
fn pass(b: &[u8], chunk: usize, workers: usize, regions: &Build) -> (Totals, bool) {
    let chunks = b.len().div_ceil(chunk);
    let start = |k: usize| record_start(b, (k * chunk).min(b.len()));
    let boxes: Vec<Mutex<Mailbox>> = (0..workers).map(|_| Mutex::default()).collect();
    let owners: Vec<Mutex<Owner>> = (0..workers).map(|_| Mutex::new(Owner::new())).collect();
    let totals = Mutex::new((vec![Slot::new(); 256], vec![0; 256]));
    let next = AtomicUsize::new(0);
    let flushed = AtomicUsize::new(0);
    let exact = AtomicBool::new(true);

    let worker = |w: usize| {
        let table = regions.ready();
        let mut slots = vec![Slot::new(); 256];
        let mut me = Worker::new(w, b, &boxes, &owners);
        while let k = next.fetch_add(1, Relaxed)
            && k < chunks
        {
            let end = start(k + 1);
            let on_record = |g: usize, at: usize, n: usize| {
                // SAFETY: parse_chunk passes a name span inside the padded mapping, after the quote
                // or comma that opens it; GUARD covers 32 bytes past its start.
                me.send(unsafe { Name::new(b.as_ptr(), at, n, g) })
            };
            // SAFETY: `b` comes from `map_padded`, which follows it with GUARD bytes; `slots` has 256.
            let stop = unsafe { parse_chunk(b, start(k), end, table, &mut slots, on_record) };
            // Pairwise end check (e1): the parse must stop exactly on the next chunk's start.
            exact.fetch_and(stop == end, Relaxed);
            me.drain(w, false);
        }
        // The mapping's page-table entries (about 50k for the 202 MB file) were torn down at exit on
        // one CPU; each worker drops its share now (c-37f). A page touched again (a neighbour's last
        // record, a rerun, an exact name's compare) faults back in from the page cache. The last
        // partial page is kept: it holds the guard (main.rs), a private copy MADV_DONTNEED would discard.
        let pages = b.len() & !4095;
        let share = pages.div_ceil(workers).next_multiple_of(4096);
        let from = (w * share).min(pages);
        // SAFETY: a page-aligned range inside the mapping; madvise only drops page-table entries.
        unsafe { madvise(b.as_ptr().add(from), share.min(pages - from), 4) }; // 4 = MADV_DONTNEED
        for (o, mailbox) in boxes.iter().enumerate() {
            mailbox.lock().unwrap().full.push(me.seal(o));
        }
        // Once every worker has flushed (each flush is under the mailbox locks, before its count), no
        // bucket can arrive any more: one last drain, waiting out a sender filing here, empties this
        // mailbox for good.
        flushed.fetch_add(1, Release);
        while flushed.load(Acquire) < workers {
            me.drain(w, false);
            std::thread::yield_now();
        }
        me.drain(w, true);
        let (total_slots, distinct) = &mut *totals.lock().unwrap();
        let counts = owners[w].lock().unwrap().counts;
        for g in 0..256 {
            total_slots[g].merge(&slots[g]);
            distinct[g] += counts[g];
        }
    };
    // The workers start before the region table exists and build it first (c-47 s2); this thread is
    // worker 0, so it spawns one thread fewer and joins in once the others are spawned.
    std::thread::scope(|s| {
        for w in 1..workers {
            s.spawn(move || worker(w));
        }
        worker(0);
    });
    (totals.into_inner().unwrap(), exact.into_inner())
}
